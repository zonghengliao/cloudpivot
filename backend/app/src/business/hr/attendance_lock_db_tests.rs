//! 考勤「锁定后写保护」的真库用例（HR-23）。
//!
//! 单测（[`crate::business::hr::attendance_lock::tests`]）证明的是**判定与拼装**：
//! 状态域互补、`409 + 68010` 报文、契约表里锁定路由挂着 `attendance:lock`、
//! 迁移文件里触发器只挂 `INSERT OR UPDATE`。证明不了四件事，只有真库能答：
//!
//! 1. **验收项「锁定后改数据被拒」**：`LOCKED` 之后 `INSERT` / `UPDATE`（含软删 `deleted_at`）
//!    被 `V0392` 的触发器逐条拒绝，且**库里的值逐列未变**（拒绝不是「改了再报错」）；
//! 2. 拒绝确实能被 [`attendance_lock::is_lock_violation`] 认出来，并翻成 `409 + 68010`
//!    而不是 500（认不出来 = 前端看到「服务器内部错误」，用户不知道去解锁）；
//! 3. **验收项「解锁要权限 + 审计」**：解锁必须给原因（缺原因 400），
//!    锁定与解锁两次操作在 `biz_status_log` 里都有操作人与原因；
//! 4. 逃生门是**事务级**的：`SET LOCAL … 'off'` 在事务内放行、提交后自动关回去
//!    （会话级 `SET` 会在连接池里留到下一个请求，等于永久关掉守卫）。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr23_card \
//!   cargo test --lib business::hr::attendance_lock_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! - 期间码由时间戳派生（年份 2400+），重跑不撞 `uk_biz_att_period`，也不搅演示数据；
//! - 清场按 id 删，顺序 记录 → 状态日志 → 期间 → 员工 → 用户 → 部门
//!   （记录/期间的 `employee_id` 是 `RESTRICT`，FK 顺序不能换）；
//! - 记录用裸 SQL 插：本卡验的是**库层**守卫，绕开 handler 才看得见触发器；
//!   期间本身走 HR-19 的真接口（`create_period` / `change_status`），
//!   免得自己拼状态机绕过 `biz_status_log`。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::attendance_lock::{self, BYPASS_SQL, GUARD_MARKER};
    use crate::business::hr::attendance_period_dto::{
        AttendancePeriodRes, PeriodCreateReq, PeriodStatusReq, STATUS_DRAFT, STATUS_LOCKED, STATUS_SUBMITTED,
    };
    use crate::business::hr::attendance_period_handler;
    use crate::business::hr::employee_handler;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::{Duration, NaiveDate};
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("HR_DB_URL").or_else(|_| std::env::var("ERP_DB_URL")) else {
                    println!("SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    fn uniq() -> u64 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        now.wrapping_add(seq.wrapping_mul(7_919))
    }

    /// 期间码：年份 2400~2899（远离真实数据与演示库），月份 01~12。
    fn period_code() -> String {
        let n = uniq();
        let year = 2400 + n % 500;
        let month = 1 + (n / 500) % 12;
        format!("{year:04}-{month:02}")
    }

    fn month_range(period_code: &str) -> (NaiveDate, NaiveDate) {
        let year: i32 = period_code[..4].parse().expect("年份");
        let month: u32 = period_code[5..].parse().expect("月份");
        let start = NaiveDate::from_ymd_opt(year, month, 1).expect("月初");
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .expect("次月初");
        (start, next - Duration::days(1))
    }

    /// 一条考勤记录在库里的可核对快照（用于「值逐列未变」）。
    #[derive(Debug, PartialEq)]
    struct RecordSnapshot {
        actual_days: String,
        should_days: String,
        deleted: bool,
        remark: Option<String>,
    }

    /// 夹具：一个部门 + 一名员工 + 期间码。
    struct Fixture {
        dept: Uuid,
        emp: Uuid,
        emp_no: String,
        emp_name: String,
        user_name: String,
        code: String,
        operator_id: Uuid,
    }

    async fn fresh_employee(tag: &str) -> (Uuid, Uuid, String, String, String) {
        let suffix = uniq();
        let dept_code = format!("HR23-{tag}-{suffix}");
        let dept = test_helpers::ensure_dept(&dept_code, &format!("HR23 部门 {tag}")).await;
        let username = format!("hr23.{tag}.{suffix}");
        let name = format!("HR23 员工 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR23-{tag}-{suffix}")),
                name: Some(name.clone()),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                user_id: Some(user.to_string()),
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建员工");
        let emp = Uuid::parse_str(&created.id).expect("员工 id");
        let emp_no = get_emp_no(emp).await;
        (dept, emp, emp_no, name, username)
    }

    async fn get_emp_no(employee: Uuid) -> String {
        let row = db()
            .query_first_sql(
                "SELECT emp_no FROM biz_employee WHERE id = ?::uuid".to_string(),
                vec![Value::from(&employee)],
            )
            .await
            .expect("读工号")
            .expect("员工必有行");
        row.get_typed("emp_no").expect("emp_no 列")
    }

    async fn fixture(tag: &str) -> Fixture {
        let (dept, emp, emp_no, emp_name, user_name) = fresh_employee(tag).await;
        Fixture {
            dept,
            emp,
            emp_no,
            emp_name,
            user_name,
            code: period_code(),
            operator_id: Uuid::now_v7(),
        }
    }

    impl Fixture {
        fn operator(&self) -> Operator {
            Operator::named(Some(self.operator_id), "HR23 夹具操作人")
        }

        fn create_req(&self) -> PeriodCreateReq {
            let (start, end) = month_range(&self.code);
            PeriodCreateReq {
                period_code: Some(self.code.clone()),
                period_start: Some(start.to_string()),
                period_end: Some(end.to_string()),
                remark: Some("HR23 夹具".to_string()),
            }
        }

        fn status_req(to_status: &str, reason: Option<&str>) -> PeriodStatusReq {
            PeriodStatusReq {
                to_status: Some(to_status.to_string()),
                reason: reason.map(str::to_string),
            }
        }

        /// 建期间（真接口，会写建单日志）+ 把它推到目标态（`DRAFT → SUBMITTED → LOCKED`）。
        async fn period_in(&self, target: &str) -> AttendancePeriodRes {
            let created = attendance_period_handler::create_period(self.operator(), &self.create_req())
                .await
                .expect("建考勤期间");
            let id = Uuid::parse_str(&created.id).expect("期间 id");
            if target == STATUS_SUBMITTED || target == STATUS_LOCKED {
                attendance_period_handler::change_status(
                    self.operator(),
                    id,
                    &Self::status_req(STATUS_SUBMITTED, None),
                )
                .await
                .expect("提交期间");
            }
            if target == STATUS_LOCKED {
                attendance_period_handler::change_status(self.operator(), id, &Self::status_req(STATUS_LOCKED, None))
                    .await
                    .expect("锁定期间");
            }
            created
        }

        /// 直接插一条记录（绕开 handler —— 本卡要看的正是**库层**守卫）。
        async fn insert_record(&self, period_id: Uuid, actual: &str) -> DbResult<Uuid> {
            self.insert_record_for(period_id, actual, self.emp, &self.emp_no, &self.emp_name, self.dept)
                .await
        }

        /// 给**指定员工**插一条记录（解锁用例要第二个人：`uk_biz_att_rec` 是
        /// `(period_id, employee_id)`，同一个人一期只能有一行）。
        async fn insert_record_for(
            &self,
            period_id: Uuid,
            actual: &str,
            emp: Uuid,
            emp_no: &str,
            emp_name: &str,
            dept: Uuid,
        ) -> DbResult<Uuid> {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_attendance_record \
                     (id, org_id, period_id, employee_id, employee_no, employee_name, dept_id, \
                      should_days, actual_days, source) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text, ?::text, ?::uuid, \
                         ?::text::numeric, ?::text::numeric, 'IMPORT')"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(&period_id),
                    Value::from(&emp),
                    Value::Text(emp_no.to_string()),
                    Value::Text(emp_name.to_string()),
                    Value::from(&dept),
                    Value::Text("21".to_string()),
                    Value::Text(actual.to_string()),
                ],
            )
            .await
            .map(|_| id)
        }

        async fn update_actual(&self, record_id: Uuid, actual: &str) -> DbResult<u64> {
            db().exec_sql(
                "UPDATE biz_attendance_record SET actual_days = ?::text::numeric, updated_at = now() \
                  WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::Text(actual.to_string()), Value::from(&record_id)],
            )
            .await
        }

        async fn soft_delete(&self, record_id: Uuid) -> DbResult<u64> {
            db().exec_sql(
                "UPDATE biz_attendance_record SET deleted_at = now() WHERE id = ?::uuid".to_string(),
                vec![Value::from(&record_id)],
            )
            .await
        }

        async fn snapshot(&self, record_id: Uuid) -> RecordSnapshot {
            let row = db()
                .query_first_sql(
                    "SELECT actual_days::text AS actual_days, should_days::text AS should_days, \
                            (deleted_at IS NOT NULL) AS deleted, remark \
                       FROM biz_attendance_record WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&record_id)],
                )
                .await
                .expect("读记录快照")
                .expect("记录必在");
            RecordSnapshot {
                actual_days: row.get_typed("actual_days").expect("actual_days"),
                should_days: row.get_typed("should_days").expect("should_days"),
                deleted: row.get_typed("deleted").expect("deleted"),
                remark: row.get_typed("remark").expect("remark"),
            }
        }

        /// 期间内的记录条数（软删的算不算由 `only_alive` 决定）。
        async fn record_count(&self, period_id: Uuid, only_alive: bool) -> i64 {
            let sql = if only_alive {
                "SELECT count(*)::bigint AS n FROM biz_attendance_record \
                  WHERE period_id = ?::uuid AND deleted_at IS NULL"
            } else {
                "SELECT count(*)::bigint AS n FROM biz_attendance_record WHERE period_id = ?::uuid"
            };
            let row = db()
                .query_first_sql(sql.to_string(), vec![Value::from(&period_id)])
                .await
                .expect("记录条数")
                .expect("count 必有行");
            row.get_typed("n").expect("n 列")
        }

        /// 期间的状态日志（按时间正序）：`(from, to, reason, operator_id)`。
        async fn logs(&self, period_id: Uuid) -> Vec<(Option<String>, String, Option<String>, Option<Uuid>)> {
            let rows = db()
                .query_sql(
                    "SELECT from_status, to_status, reason, operator_id FROM biz_status_log \
                      WHERE entity_type = 'ATTENDANCE_PERIOD' AND entity_id = ?::uuid \
                      ORDER BY created_at, id"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读状态日志");
            rows.into_iter()
                .map(|row| {
                    (
                        row.get_typed("from_status").expect("from_status"),
                        row.get_typed("to_status").expect("to_status"),
                        row.get_typed("reason").expect("reason"),
                        row.get_typed("operator_id").expect("operator_id"),
                    )
                })
                .collect()
        }

        /// 清场：记录 → 状态日志 → 期间 → 员工 → 用户 → 部门。
        ///
        /// 记录用硬 `DELETE`：`V0392` 的守卫**刻意不拦 DELETE**（演练清场 / `ON DELETE CASCADE`
        /// 要能收回数据，见迁移文件头 ③），所以锁定中的期间也清得掉。
        async fn clean(&self, period_ids: &[Uuid]) {
            for period_id in period_ids {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid".to_string(),
                        vec![Value::from(period_id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_status_log WHERE entity_type = 'ATTENDANCE_PERIOD' \
                          AND entity_id = ?::uuid"
                            .to_string(),
                        vec![Value::from(period_id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                        vec![Value::from(period_id)],
                    )
                    .await;
            }
            self.clean_employee(self.dept, self.emp, &self.user_name).await;
        }

        /// 清掉一个员工及其用户与部门（按 FK 倒序：员工 → 用户 → 部门）。
        ///
        /// 单独拆出来是为了「解锁用例额外建的第二个人」也能按 id 清干净（夹具纪律：不留残留）。
        async fn clean_employee(&self, dept: Uuid, emp: Uuid, user_name: &str) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&emp)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM sys_user WHERE username = ?::text".to_string(),
                    vec![Value::Text(user_name.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM sys_dept WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&dept)],
                )
                .await;
        }
    }

    // ==================== ① 锁定后「插 / 改 / 软删」全被库层拒 ====================

    /// 验收项「锁定后改数据被拒」的**库层**证据：绕开 handler 直接写也写不进去。
    #[tokio::test]
    async fn locked_period_rejects_insert_update_and_soft_delete() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("lock").await;
        let period = fx.period_in(STATUS_SUBMITTED).await;
        let period_id = Uuid::parse_str(&period.id).expect("期间 id");

        // SUBMITTED 期间可写（守卫不许过宽：客户第一轮导入几乎总有错行要补）
        let rec = fx.insert_record(period_id, "20").await.expect("SUBMITTED 期间可插");
        fx.update_actual(rec, "19").await.expect("SUBMITTED 期间可改");
        assert_eq!(fx.record_count(period_id, true).await, 1, "SUBMITTED 期间记录数");

        // 锁定
        let locked = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &Fixture::status_req(STATUS_LOCKED, None),
        )
        .await
        .expect("锁定期间");
        assert_eq!(locked.status, STATUS_LOCKED);
        let before = fx.snapshot(rec).await;

        // ② INSERT 被拒
        let err = fx.insert_record(period_id, "21").await.expect_err("锁定后不许插记录");
        assert!(
            attendance_lock::is_lock_violation(&err),
            "拒绝必须能认出来（守卫报文带 `{GUARD_MARKER}`）：{err}"
        );
        assert!(
            attendance_lock::error_chain_text(&err).contains(&fx.code),
            "报文要点名期间码：{err}"
        );
        let mapped = attendance_lock::map_write_error(err, "插入考勤记录");
        assert_eq!(mapped.status, 409);
        assert_eq!(mapped.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(mapped.msg.contains("解锁"), "报文要给出下一步：{}", mapped.msg);

        // ③ UPDATE 被拒
        let err = fx.update_actual(rec, "3").await.expect_err("锁定后不许改记录");
        assert!(attendance_lock::is_lock_violation(&err), "改也被守卫拒：{err}");

        // ④ 软删（= UPDATE deleted_at）被拒
        let err = fx.soft_delete(rec).await.expect_err("锁定后不许软删记录");
        assert!(attendance_lock::is_lock_violation(&err), "软删也被守卫拒：{err}");

        // ⑤ 值逐列未变、记录数没变（拒绝不是「改了再报错」）
        assert_eq!(fx.snapshot(rec).await, before, "锁定期间记录的值必须逐列未变");
        assert_eq!(fx.record_count(period_id, false).await, 1, "记录条数不许变");

        println!(
            "[HR-23] ① 锁定后 INSERT/UPDATE/软删 全被库层守卫拒（CP001→409/68010），期间={} 值逐列未变",
            fx.code
        );
        fx.clean(&[period_id]).await;
    }

    // ==================== ② 解锁要原因 + 审计 ====================

    /// 验收项「解锁要权限 + 审计」：缺原因 400；给原因才放行；
    /// `biz_status_log` 里锁定与解锁两行都带操作人，解锁行带原因。
    #[tokio::test]
    async fn unlock_requires_reason_writes_audit_and_restores_writes() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("unlock").await;
        let period = fx.period_in(STATUS_LOCKED).await;
        let period_id = Uuid::parse_str(&period.id).expect("期间 id");
        let rec = {
            // 锁定前先落一行，解锁后要能改它
            let unlocked = attendance_period_handler::change_status(
                fx.operator(),
                period_id,
                &Fixture::status_req(STATUS_DRAFT, Some("HR23 夹具：先解锁补一行记录")),
            )
            .await
            .expect("解锁回草稿");
            assert_eq!(unlocked.status, STATUS_DRAFT);
            let rec = fx.insert_record(period_id, "20").await.expect("解锁后可插");
            attendance_period_handler::change_status(
                fx.operator(),
                period_id,
                &Fixture::status_req(STATUS_SUBMITTED, None),
            )
            .await
            .expect("再提交");
            attendance_period_handler::change_status(
                fx.operator(),
                period_id,
                &Fixture::status_req(STATUS_LOCKED, None),
            )
            .await
            .expect("再锁定");
            rec
        };

        // ① 缺原因 → 400（审计要求「解锁必须说清为什么」，空原因等于没审计）
        let error = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &Fixture::status_req(STATUS_DRAFT, None),
        )
        .await
        .expect_err("缺原因的解锁必须被拒");
        assert_eq!(error.status, 400, "缺原因应为 400 参数错：{}", error.msg);

        // ② 原因太短也拒（≥5 字，与员工状态机 reason 边同口径）
        let error = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &Fixture::status_req(STATUS_DRAFT, Some("漏人")),
        )
        .await
        .expect_err("原因过短必须被拒");
        assert_eq!(error.status, 400, "原因过短应为 400：{}", error.msg);

        // ③ 给足原因 → 解锁成功
        let reason = "HR23 夹具：月末补录漏掉的两人";
        let unlocked = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &Fixture::status_req(STATUS_DRAFT, Some(reason)),
        )
        .await
        .expect("给原因的解锁必须成功");
        assert_eq!(unlocked.status, STATUS_DRAFT);
        // `locked` 的语义是「**现在**是不是锁定态」（HR-19 的定义），解锁后必须是 false；
        // 而 `locked_at / locked_by` 是「最近一次锁定」的审计信息，解锁**不清空**（轨迹另有 biz_status_log）。
        assert!(!unlocked.locked, "解锁后 locked 必须翻成 false：{}", unlocked.status);
        assert!(
            unlocked.locked_at.is_some() && unlocked.locked_by.is_some(),
            "解锁不许清空 locked_at/locked_by（审计轨迹）：{:?}/{:?}",
            unlocked.locked_at,
            unlocked.locked_by
        );

        // ④ 解锁后写恢复（第二个人，避开 `uk_biz_att_rec`：(period_id, employee_id) 一人一期一行）
        fx.update_actual(rec, "21").await.expect("解锁后改记录");
        let (dept2, emp2, emp_no2, name2, user2) = fresh_employee("unlock2").await;
        fx.insert_record_for(period_id, "20", emp2, &emp_no2, &name2, dept2)
            .await
            .expect("解锁后插记录");
        assert_eq!(fx.record_count(period_id, true).await, 2, "解锁后可写");

        // ⑤ 审计：建单(1) + 首轮 提交/锁定(2) + 解锁/再提交/再锁定(3) + 末次解锁(1) = 7 行；每行都有操作人
        let logs = fx.logs(period_id).await;
        assert_eq!(logs.len(), 7, "状态日志行数：{logs:?}");
        assert!(
            logs.iter().all(|(_, _, _, op)| *op == Some(fx.operator_id)),
            "每行都要有操作人"
        );
        let lock_rows: Vec<_> = logs.iter().filter(|(_, to, _, _)| to == STATUS_LOCKED).collect();
        assert_eq!(lock_rows.len(), 2, "锁定两次 → 两条锁定日志");
        let unlock_rows: Vec<_> = logs
            .iter()
            .filter(|(from, to, _, _)| from.as_deref() == Some(STATUS_LOCKED) && to == STATUS_DRAFT)
            .collect();
        assert_eq!(unlock_rows.len(), 2, "解锁两次 → 两条解锁日志");
        assert_eq!(unlock_rows[1].2.as_deref(), Some(reason), "解锁原因必须落库");
        assert_eq!(
            logs.last().map(|(from, to, _, _)| (from.clone(), to.clone())),
            Some((Some(STATUS_LOCKED.to_string()), STATUS_DRAFT.to_string())),
            "最后一条应为解锁"
        );

        println!(
            "[HR-23] ② 缺原因/原因过短 → 400；给原因解锁成功；状态日志 {} 行，解锁原因已落库 `{}`",
            logs.len(),
            reason
        );
        fx.clean(&[period_id]).await;
        fx.clean_employee(dept2, emp2, &user2).await;
    }

    // ==================== ③ handler 层守卫（lock_for_write）====================

    /// 后续卡（导入 upsert / 请假 bridge）要照抄的前置动作：锁期间行 → 判状态。
    #[tokio::test]
    async fn lock_for_write_helper_blocks_locked_and_passes_draft() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("helper").await;
        let period = fx.period_in(STATUS_DRAFT).await;
        let period_id = Uuid::parse_str(&period.id).expect("期间 id");

        // 草稿期：放行，并把期间码带出来（调用方拼报文/校验日期都要用）
        let tx = db().begin().await.expect("开事务");
        let row = attendance_lock::lock_for_write(&tx, period_id)
            .await
            .expect("草稿期应放行");
        assert_eq!(row.status, STATUS_DRAFT);
        assert_eq!(row.period_code, fx.code);
        tx.rollback().await.expect("回滚");

        // 挪到锁定态后：拒绝（409 + 68010），且锁在事务回滚后释放
        attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &Fixture::status_req(STATUS_SUBMITTED, None),
        )
        .await
        .expect("提交");
        attendance_period_handler::change_status(fx.operator(), period_id, &Fixture::status_req(STATUS_LOCKED, None))
            .await
            .expect("锁定");

        let tx = db().begin().await.expect("开事务");
        let error = attendance_lock::lock_for_write(&tx, period_id)
            .await
            .expect_err("锁定态必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(error.msg.contains(&fx.code), "报文点名期间：{}", error.msg);
        tx.rollback().await.expect("回滚");

        // 回滚后同一期间还能被别人拿到锁（不残留）
        let tx = db().begin().await.expect("开事务");
        let again = attendance_lock::lock_for_write(&tx, period_id).await;
        assert!(again.is_err(), "期间仍锁定 → 仍拒绝");
        tx.rollback().await.expect("回滚");

        // 期间不存在 → 404（不是 409/68010：别把「没这期」说成「锁了」）
        let tx = db().begin().await.expect("开事务");
        let error = attendance_lock::lock_for_write(&tx, Uuid::now_v7())
            .await
            .expect_err("不存在的期间必须 404");
        assert_eq!(error.status, 404);
        tx.rollback().await.expect("回滚");

        println!("[HR-23] ③ lock_for_write：草稿放行 / 锁定 409+68010 / 不存在 404，事务回滚后无残留锁");
        fx.clean(&[period_id]).await;
    }

    // ==================== ④ 逃生门是事务级的 ====================

    /// `SET LOCAL … 'off'` 只在本次事务内放行，提交后守卫自动恢复。
    ///
    /// 为什么不许会话级 `SET`：连接池会把会话级设置带到下一个请求，
    /// 等于把守卫永久关掉（且没人知道关过）。
    #[tokio::test]
    async fn bypass_gate_is_transaction_scoped() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("bypass").await;
        let period = fx.period_in(STATUS_LOCKED).await;
        let period_id = Uuid::parse_str(&period.id).expect("期间 id");

        // 不给逃生门 → 拒
        fx.insert_record(period_id, "20").await.expect_err("没开逃生门必须拒");

        // 事务内开逃生门 → 放行，且提交后真的落库
        let tx = db().begin().await.expect("开事务");
        tx.exec_sql(BYPASS_SQL.to_string(), vec![])
            .await
            .expect("设置事务级逃生门");
        tx.exec_sql(
            "INSERT INTO biz_attendance_record \
                 (id, org_id, period_id, employee_id, employee_no, employee_name, dept_id, \
                  should_days, actual_days, source) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text, ?::text, ?::uuid, \
                     ?::text::numeric, ?::text::numeric, 'MANUAL')"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&period_id),
                Value::from(&fx.emp),
                Value::Text(fx.emp_no.clone()),
                Value::Text(fx.emp_name.clone()),
                Value::from(&fx.dept),
                Value::Text("21".to_string()),
                Value::Text("20".to_string()),
            ],
        )
        .await
        .expect("逃生门内可插");
        tx.commit().await.expect("提交");
        assert_eq!(fx.record_count(period_id, true).await, 1, "逃生门内写入已落库");

        // 提交后守卫自动恢复
        let err = fx.insert_record(period_id, "21").await.expect_err("提交后守卫必须恢复");
        assert!(attendance_lock::is_lock_violation(&err), "恢复后仍是守卫拒：{err}");

        println!(
            "[HR-23] ④ 逃生门 SET LOCAL 事务内放行、提交后自动恢复（拒：{}）",
            GUARD_MARKER
        );
        fx.clean(&[period_id]).await;
    }
}
