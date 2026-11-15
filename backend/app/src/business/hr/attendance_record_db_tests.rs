//! 考勤记录 / 台账（`biz_attendance_record`）的真库用例（HR-20）。
//!
//! 单测（DTO 的合并 / 范围 / 警告，model 的占位符 / 守卫 SQL / diff 白名单）证明的是
//! **校验与拼装**；证明不了这四件事，只有真库能答：
//!
//! 1. **验收项「台账按人一行」**：两人两行、`employee_no` 不重复、分页 `total` 正确，
//!    且数据权限（部门范围）真的把别部门的人挡在外面；
//! 2. **验收项「单行修正写入审计」**：`PUT` 改一列 → `sys_change_log` 恰好一条、
//!    `changed_fields` 只含改动列、`b` / `a` 成对且是两位小数；同值再改一次
//!    **不写库也不留痕**（`Skipped`）；
//! 3. **验收项「锁定期间不可改」**：`DRAFT→SUBMITTED→LOCKED` 之后修正被 409（`68010`）
//!    挡住，且**库里 11 个数值列逐列未变**（不是"报错了但写进去了"）；
//! 4. **汇总随修正重算**：改 `actual_days` 之后期间头的 `actual_days_total` 跟上，
//!    `employee_count` / `should_days_total` 不变 —— 「合计 = 各行之和」继续成立。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr20_card \
//!   cargo test --lib business::hr::attendance_record_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 / HR-15 / HR-16 / HR-19 的教训）
//!
//! - 期间码带**时间戳派生的年份**（2400+），重跑不撞 `uk_biz_att_period`；
//! - 清场**按 id 删**（绝不按前缀扫全库）：记录 → 审计日志 → 状态日志 → 期间 → 员工 →
//!   用户 → 部门（FK 顺序不能换：记录的 `employee_id` 是 `RESTRICT`）；
//! - 员工走真实 `employee_handler::create_employee`（记录挂在员工外键上，自己拼 INSERT
//!   会漏掉 `uk_biz_employee_*` 的口径）；
//! - `sys_change_log` 是**月分区表**：先 `ensure_partitions` 再写，否则「当前月分区不存在」
//!   会让审计写入直接 500（那是真库层面的环境问题，不该被当成本卡的业务失败）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::changelog;
    use crate::business::hr::attendance_period_dto::{
        AttendancePeriodRes, PeriodCreateReq, PeriodStatusReq, STATUS_DRAFT, STATUS_LOCKED, STATUS_SUBMITTED,
    };
    use crate::business::hr::attendance_period_handler;
    use crate::business::hr::attendance_record_dto::{RecordFilter, RecordNumbers, RecordPatchReq};
    use crate::business::hr::attendance_record_handler;
    use crate::business::hr::attendance_record_model as model;
    use crate::business::hr::employee_handler;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::{Duration, NaiveDate};
    use nik::prelude::rust_decimal::Decimal;
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
                    Ok(connection) => {
                        if set_db(connection).is_err() {
                            return false;
                        }
                        // 审计日志是月分区表：先补齐当前月及后两个月的分区
                        match changelog::partition::ensure_partitions(db(), nik::prelude::chrono::Utc::now()).await {
                            Ok((created, _)) => {
                                if !created.is_empty() {
                                    println!("[夹具] sys_change_log 补建分区：{}", created.join(", "));
                                }
                                true
                            }
                            Err(err) => {
                                println!("SKIP: sys_change_log 分区不可用（{err}）");
                                false
                            }
                        }
                    }
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

    /// 期间码：年份取 2400~2899（远离真实数据与演示库），月份取 01~12。
    fn period_code() -> String {
        let n = uniq();
        let year = 2400 + n % 500;
        let month = 1 + (n / 500) % 12;
        format!("{year:04}-{month:02}")
    }

    /// 与期间码同年的一个自然月区间（起 = 1 日，止 = 月末）。
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

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法小数")
    }

    /// 夹具：两个部门（本部门 + 别部门）+ 本部门两名员工 + 清理清单。
    struct Fixture {
        dept: Uuid,
        dept_code: String,
        dept_name: String,
        other_dept: Uuid,
        other_dept_code: String,
        emp1: Uuid,
        emp1_no: String,
        emp1_name: String,
        user1: Uuid,
        user1_name: String,
        emp2: Uuid,
        emp2_no: String,
        emp2_name: String,
        user2: Uuid,
        user2_name: String,
        code: String,
    }

    async fn fresh_dept(tag: &str) -> (Uuid, String, String) {
        let code = format!("HR20-{tag}-{}", uniq());
        let name = format!("HR20 部门 {tag}");
        let id = test_helpers::ensure_dept(&code, &name).await;
        (id, code, name)
    }

    async fn fresh_employee(tag: &str, dept: Uuid) -> (Uuid, String, String, Uuid, String) {
        let suffix = uniq();
        let username = format!("hr20.{tag}.{suffix}");
        let name = format!("HR20 员工 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR20-{tag}-{suffix}")),
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
        let emp_id = Uuid::parse_str(&created.id).expect("员工 id");
        let emp_no = get_emp_no(emp_id).await;
        (emp_id, emp_no, name, user, username)
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
        let (dept, dept_code, dept_name) = fresh_dept(tag).await;
        let (other_dept, other_dept_code, _) = fresh_dept(&format!("{tag}-other")).await;
        let (emp1, emp1_no, emp1_name, user1, user1_name) = fresh_employee("a", dept).await;
        let (emp2, emp2_no, emp2_name, user2, user2_name) = fresh_employee("b", dept).await;
        Fixture {
            dept,
            dept_code,
            dept_name,
            other_dept,
            other_dept_code,
            emp1,
            emp1_no,
            emp1_name,
            user1,
            user1_name,
            emp2,
            emp2_no,
            emp2_name,
            user2,
            user2_name,
            code: period_code(),
        }
    }

    /// 一行的 11 个数值列（`should=22 / actual=20 / unpaid=2 / ot_wd=10`）。
    fn numbers_a() -> RecordNumbers {
        RecordNumbers {
            should_days: dec("22"),
            actual_days: dec("20"),
            leave_paid_days: dec("0"),
            leave_unpaid_days: dec("2"),
            sick_days: dec("0"),
            absent_days: dec("0"),
            ot_hours_workday: dec("10"),
            ot_hours_weekend: dec("4"),
            ot_hours_holiday: dec("0"),
            late_count: 1,
            early_leave_count: 0,
        }
    }

    /// 第二个人：`should=22 / actual=22 / ot=0`。
    fn numbers_b() -> RecordNumbers {
        RecordNumbers {
            should_days: dec("22"),
            actual_days: dec("22"),
            leave_paid_days: dec("0"),
            leave_unpaid_days: dec("0"),
            sick_days: dec("0"),
            absent_days: dec("0"),
            ot_hours_workday: dec("0"),
            ot_hours_weekend: dec("0"),
            ot_hours_holiday: dec("0"),
            late_count: 0,
            early_leave_count: 0,
        }
    }

    /// 超管范围（`ALL`，看全部部门）。
    fn super_principal() -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), None, true, ScopeKind::All, vec![], false)
    }

    /// 部门范围（只看 `dept`）。
    fn dept_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), false, ScopeKind::Dept, vec![dept], false)
    }

    impl Fixture {
        /// 改考勤的 HR 操作人：用**真实 `sys_user`**（`user1`）而不是随机 UUID ——
        /// 出参的 `updated_by_name` 是实时 join `sys_user` 出来的，随机 id join 不到
        /// 只会得到空串（那样等于没验证 join）。
        fn operator(&self) -> Operator {
            Operator::named(Some(self.user1), "HR20 夹具")
        }

        /// 第二个操作人（期间状态流转用：证明 `biz_status_log` / 审计里的操作人可区分）。
        fn operator2(&self) -> Operator {
            Operator::named(Some(self.user2), "HR20 夹具(2)")
        }

        /// 每个用例一份独立的审计上下文（`request_id` 互不相同便于排查）。
        fn request_ctx(&self) -> changelog::RequestCtx {
            changelog::RequestCtx {
                request_id: format!("hr20-{}", uniq()),
                client_ip: None,
                operator_id: Some(Uuid::now_v7()),
                operator_name: Some("HR20 夹具".to_string()),
            }
        }

        fn period_create_req(&self) -> PeriodCreateReq {
            let (start, end) = month_range(&self.code);
            PeriodCreateReq {
                period_code: Some(self.code.clone()),
                period_start: Some(start.to_string()),
                period_end: Some(end.to_string()),
                remark: Some("HR20 夹具".to_string()),
            }
        }

        async fn create_period(&self) -> Uuid {
            let res = attendance_period_handler::create_period(self.operator(), &self.period_create_req())
                .await
                .expect("建考勤期间");
            Uuid::parse_str(&res.id).expect("期间 id")
        }

        /// 建房两行记录（走模型层的 `insert`：撞唯一键的翻译也顺带被覆盖）。
        async fn seed_records(&self, period_id: Uuid) -> (Uuid, Uuid) {
            let a = self
                .insert_record(period_id, self.emp1, &self.emp1_no, &self.emp1_name, &numbers_a())
                .await;
            let b = self
                .insert_record(period_id, self.emp2, &self.emp2_no, &self.emp2_name, &numbers_b())
                .await;
            (a, b)
        }

        async fn insert_record(
            &self,
            period_id: Uuid,
            employee: Uuid,
            emp_no: &str,
            name: &str,
            numbers: &RecordNumbers,
        ) -> Uuid {
            let id = Uuid::now_v7();
            model::insert(
                db(),
                id,
                &model::NewRecord {
                    period_id,
                    employee_id: employee,
                    employee_no: emp_no,
                    employee_name: name,
                    dept_id: Some(self.dept),
                    numbers,
                    source: "IMPORT",
                    remark: Some("HR20 夹具导入"),
                    operator: Some(Uuid::now_v7()),
                },
            )
            .await
            .expect("插考勤记录");
            id
        }

        /// 走状态机把期间推到目标态（`DRAFT→SUBMITTED→LOCKED`）。
        ///
        /// 用 `operator2`（第二个 HR）：锁定/解锁是**另一个账号**做的，
        /// 这样 `biz_status_log` 与「谁锁的这一期」在轨迹上可区分。
        async fn drive_status(&self, period_id: Uuid, to_status: &str) -> AttendancePeriodRes {
            if to_status == STATUS_SUBMITTED || to_status == STATUS_LOCKED {
                attendance_period_handler::change_status(
                    self.operator2(),
                    period_id,
                    &PeriodStatusReq {
                        to_status: Some(STATUS_SUBMITTED.to_string()),
                        reason: None,
                    },
                )
                .await
                .expect("提交期间");
            }
            if to_status == STATUS_LOCKED {
                return attendance_period_handler::change_status(
                    self.operator2(),
                    period_id,
                    &PeriodStatusReq {
                        to_status: Some(STATUS_LOCKED.to_string()),
                        reason: None,
                    },
                )
                .await
                .expect("锁定期间");
            }
            attendance_period_handler::change_status(
                self.operator2(),
                period_id,
                &PeriodStatusReq {
                    to_status: Some(STATUS_DRAFT.to_string()),
                    reason: None,
                },
            )
            .await
            .expect("解锁期间")
        }

        /// 库里的 11 个数值列（直接读，不经过 DTO —— 验证的是**库**里的值）。
        async fn stored_numbers(&self, record_id: Uuid) -> RecordNumbers {
            let row = db()
                .query_first_sql(
                    "SELECT should_days, actual_days, leave_paid_days, leave_unpaid_days, sick_days, \
                            absent_days, ot_hours_workday, ot_hours_weekend, ot_hours_holiday, \
                            late_count, early_leave_count \
                       FROM biz_attendance_record WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&record_id)],
                )
                .await
                .expect("读库里的数值列")
                .expect("记录必有行");
            RecordNumbers {
                should_days: row.get_typed("should_days").expect("should_days"),
                actual_days: row.get_typed("actual_days").expect("actual_days"),
                leave_paid_days: row.get_typed("leave_paid_days").expect("leave_paid_days"),
                leave_unpaid_days: row.get_typed("leave_unpaid_days").expect("leave_unpaid_days"),
                sick_days: row.get_typed("sick_days").expect("sick_days"),
                absent_days: row.get_typed("absent_days").expect("absent_days"),
                ot_hours_workday: row.get_typed("ot_hours_workday").expect("ot_hours_workday"),
                ot_hours_weekend: row.get_typed("ot_hours_weekend").expect("ot_hours_weekend"),
                ot_hours_holiday: row.get_typed("ot_hours_holiday").expect("ot_hours_holiday"),
                late_count: row.get_typed("late_count").expect("late_count"),
                early_leave_count: row.get_typed("early_leave_count").expect("early_leave_count"),
            }
        }

        /// 期间头的汇总四列（库里的值）。
        async fn stored_totals(&self, period_id: Uuid) -> (i32, Decimal, Decimal, Decimal) {
            let row = db()
                .query_first_sql(
                    "SELECT employee_count, should_days_total, actual_days_total, ot_hours_total \
                       FROM biz_attendance_period WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读期间汇总")
                .expect("期间必有行");
            (
                row.get_typed("employee_count").expect("employee_count"),
                row.get_typed("should_days_total").expect("should_days_total"),
                row.get_typed("actual_days_total").expect("actual_days_total"),
                row.get_typed("ot_hours_total").expect("ot_hours_total"),
            )
        }

        /// 某条记录的审计日志条数（`sys_change_log`）。
        async fn audit_count(&self, record_id: Uuid) -> i64 {
            changelog::model::count_for_object(db(), model::TABLE, &record_id.to_string())
                .await
                .expect("审计条数")
        }

        /// 某条记录**最近一条**审计日志的 `(changed_fields, diff::text)`。
        ///
        /// `changed_fields` 是 `text[]`：驱动只给 `Vec<u8>` 之类实现了 `TypeMapper`，
        /// 所以用 `array_to_string` 在库侧拼成 `a,b` 再读 `String`（不在测试里手写
        /// PG 数组字面量解析 —— 那会把「驱动能读什么」的问题掩盖成解析 bug）。
        async fn last_audit(&self, record_id: Uuid) -> (Vec<String>, String) {
            let row = db()
                .query_first_sql(
                    "SELECT array_to_string(changed_fields, ',') AS changed_fields, diff::text AS diff \
                       FROM sys_change_log \
                      WHERE resource = ?::text AND resource_id = ?::text \
                      ORDER BY created_at DESC, id DESC LIMIT 1"
                        .to_string(),
                    vec![Value::from(model::TABLE), Value::from(record_id.to_string())],
                )
                .await
                .expect("读审计日志")
                .expect("必有审计行");
            let joined: String = row.get_typed("changed_fields").expect("changed_fields");
            let fields = joined
                .split(',')
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect();
            (fields, row.get_typed("diff").expect("diff"))
        }

        /// 清场：记录 → 审计日志 → 状态日志 → 期间 → 员工 → 用户 → 部门（FK 顺序不能换）。
        async fn clean(&self, period_id: Uuid, record_ids: &[Uuid]) {
            for record_id in record_ids {
                let _ = changelog::model::purge_for_object(db(), model::TABLE, &record_id.to_string()).await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid".to_string(),
                    vec![Value::from(&period_id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_status_log WHERE entity_type = 'ATTENDANCE_PERIOD' \
                      AND entity_id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&period_id)],
                )
                .await;
            for employee in [self.emp1, self.emp2] {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&employee)],
                    )
                    .await;
            }
            test_helpers::purge_user(&self.user1_name).await;
            test_helpers::purge_user(&self.user2_name).await;
            test_helpers::purge_dept(&self.dept_code).await;
            test_helpers::purge_dept(&self.other_dept_code).await;
        }
    }

    fn filter_of(period_id: Uuid) -> RecordFilter {
        RecordFilter {
            period_id,
            dept_id: None,
            keyword: None,
        }
    }

    // ==================== ① 台账一人一行 + 数据权限 ====================

    /// 台账按人一行：两人两行、工号不重复、按工号升序、`total` 正确；
    /// 部门范围只看得见本部门的人；`dept_id` / `keyword` 过滤真的生效。
    #[tokio::test]
    async fn ledger_is_one_row_per_person_with_scope() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("ledger").await;
        let period_id = fx.create_period().await;
        let (a, b) = fx.seed_records(period_id).await;

        // 超管视角：两人两行
        let filter = filter_of(period_id);
        let (rows, total) = attendance_record_handler::page_records(&super_principal(), &filter, 1, 50)
            .await
            .expect("台账分页");
        assert_eq!(total, 2, "一期两人 = 两行（一人一行）");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].employee_no, fx.emp1_no, "按工号升序");
        assert_eq!(rows[1].employee_no, fx.emp2_no);
        assert_eq!(rows[0].id, a.to_string());
        assert_eq!(rows[1].id, b.to_string());
        // 快照姓名 / 实时部门名 / 加班合计 / 期间态都要在
        assert_eq!(rows[0].employee_name, fx.emp1_name);
        assert_eq!(
            rows[0].dept_name.as_deref(),
            Some(fx.dept_name.as_str()),
            "部门名实时 join"
        );
        assert_eq!(rows[0].ot_hours_total, dec("14"), "10 + 4 + 0");
        assert_eq!(rows[0].period_code, fx.code);
        assert!(rows[0].editable, "草稿期间的台账可改");

        // 同一人不会因为「有多条记录」出两行：uk_biz_att_rec 挡住重复插入
        let dup = model::insert(
            db(),
            Uuid::now_v7(),
            &model::NewRecord {
                period_id,
                employee_id: fx.emp1,
                employee_no: &fx.emp1_no,
                employee_name: &fx.emp1_name,
                dept_id: Some(fx.dept),
                numbers: &numbers_a(),
                source: "IMPORT",
                remark: None,
                operator: Some(Uuid::now_v7()),
            },
        )
        .await;
        let error = model::write_error(
            dup.expect_err("同一人同一期插第二行必须撞 uk_biz_att_rec"),
            "hr_attendance_record_duplicate",
            &fx.emp1_no,
            &fx.code,
        );
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
        assert!(error.msg.contains(&fx.emp1_no), "报文要点名工号：{}", error.msg);

        // 数据权限：别部门的人看不到本部门的台账
        let (other_rows, other_total) =
            attendance_record_handler::page_records(&dept_principal(fx.other_dept), &filter, 1, 50)
                .await
                .expect("别部门台账");
        assert_eq!(other_total, 0, "部门范围不该看见别的部门的人");
        assert!(other_rows.is_empty());
        // 本部门范围看得见
        let (mine, mine_total) = attendance_record_handler::page_records(&dept_principal(fx.dept), &filter, 1, 50)
            .await
            .expect("本部门台账");
        assert_eq!(mine_total, 2);
        assert_eq!(mine.len(), 2);

        // dept_id 过滤：指向别部门 → 0 行（真过滤，不是被忽略）
        let by_other_dept = RecordFilter {
            period_id,
            dept_id: Some(fx.other_dept),
            keyword: None,
        };
        let (rows, total) = attendance_record_handler::page_records(&super_principal(), &by_other_dept, 1, 50)
            .await
            .expect("按别部门筛");
        assert_eq!(total, 0);
        assert!(rows.is_empty());

        // keyword：工号 / 姓名都能搜到
        let by_no = RecordFilter {
            period_id,
            dept_id: None,
            keyword: Some(fx.emp1_no.clone()),
        };
        let (rows, total) = attendance_record_handler::page_records(&super_principal(), &by_no, 1, 50)
            .await
            .expect("按工号搜");
        assert_eq!(total, 1);
        assert_eq!(rows[0].employee_no, fx.emp1_no);

        // 期间不存在 → 台账空（不是 500）
        let (_, empty_total) =
            attendance_record_handler::page_records(&super_principal(), &filter_of(Uuid::now_v7()), 1, 50)
                .await
                .expect("不存在的期间");
        assert_eq!(empty_total, 0);

        fx.clean(period_id, &[a, b]).await;
    }

    // ==================== ② 单行修正 + 审计 ====================

    /// 单行修正：改一个字段 → 回执 changed=true、汇总跟着重算、审计恰好一条且只记改动列；
    /// 同值再提交一次 → changed=false、不写库、不留痕（`Skipped`）。
    #[tokio::test]
    async fn patch_writes_audit_and_recalcs_totals() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("patch").await;
        let period_id = fx.create_period().await;
        let (a, b) = fx.seed_records(period_id).await;
        // 期间汇总先重算一次（模拟「导入后重算」，否则期间头四个数是建期时的 0）
        attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect("重算期间汇总");
        let (count0, should0, actual0, ot0) = fx.stored_totals(period_id).await;
        assert_eq!(count0, 2);
        assert_eq!(should0, dec("44"));
        assert_eq!(actual0, dec("42"));
        assert_eq!(ot0, dec("14"));

        // 改 20 → 19.5（补半天事假以外的情况：只改实际出勤，不动无薪假）
        let req = RecordPatchReq {
            actual_days: Some(dec("19.5")),
            ..RecordPatchReq::default()
        };
        let res = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &req)
            .await
            .expect("单行修正");
        assert!(res.changed, "确实改了");
        assert_eq!(res.record.numbers.actual_days, dec("19.5"));
        assert_eq!(res.record.id, a.to_string());
        let operator_id = fx.user1.to_string();
        assert_eq!(
            res.record.updated_by.as_deref(),
            Some(operator_id.as_str()),
            "回执带修改人 id（= 本次操作人）"
        );
        assert_eq!(
            res.record.updated_by_name.as_deref(),
            Some(fx.emp1_name.as_str()),
            "修改人姓名是实时 join sys_user 出来的（不是 UUID）"
        );
        // 出现「应出勤 22 ≠ 18.5+…」的差额：只报告不拦截（设计 §4.9 W1）
        assert_eq!(res.warnings.len(), 1, "应出勤对不上要出警告：{:?}", res.warnings);
        assert!(res.warnings[0].contains("只提示不拦截"));

        // 汇总随修正重算：actual 42 → 41.5，其余三列不变
        assert_eq!(res.period_totals.employee_count, 2);
        assert_eq!(res.period_totals.should_days_total, dec("44"));
        assert_eq!(res.period_totals.actual_days_total, dec("41.5"));
        assert_eq!(res.period_totals.ot_hours_total, dec("14"));
        let (count1, should1, actual1, ot1) = fx.stored_totals(period_id).await;
        assert_eq!((count1, should1, actual1, ot1), (2, dec("44"), dec("41.5"), dec("14")));

        // 库里的值确实是 19.50
        let stored = fx.stored_numbers(a).await;
        assert_eq!(stored.actual_days, dec("19.5"));

        // 审计：恰好一条，只记 actual_days，b/a 成对且两位小数
        assert_eq!(fx.audit_count(a).await, 1, "一次修正 = 一条审计");
        let (changed_fields, diff_text) = fx.last_audit(a).await;
        assert_eq!(changed_fields, vec!["actual_days".to_string()], "只记改动列");
        let diff: serde_json::Value = serde_json::from_str(&diff_text).expect("diff 是 JSON");
        assert_eq!(diff["mode"], serde_json::json!("WHITELIST"));
        assert_eq!(
            diff["fields"]["actual_days"]["b"],
            serde_json::json!("20.00"),
            "before 侧两位小数：{diff_text}"
        );
        assert_eq!(
            diff["fields"]["actual_days"]["a"],
            serde_json::json!("19.50"),
            "after 侧两位小数：{diff_text}"
        );
        assert_eq!(
            diff["fields"].as_object().expect("fields 是对象").len(),
            1,
            "只记变化字段（未变的 10 列不该进 diff）：{diff_text}"
        );
        assert_eq!(fx.audit_count(b).await, 0, "没改的那一行不该有日志");

        // 同值再提交一次（单元格失焦保存的常态）：200 + changed=false，不写库不留痕
        let same = RecordPatchReq {
            actual_days: Some(dec("19.50")),
            ..RecordPatchReq::default()
        };
        let again = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &same)
            .await
            .expect("同值修正不该报错");
        assert!(!again.changed);
        assert_eq!(again.record.numbers.actual_days, dec("19.5"));
        assert_eq!(fx.audit_count(a).await, 1, "值没变不该多写一条日志");
        let (_, _, actual2, _) = fx.stored_totals(period_id).await;
        assert_eq!(actual2, dec("41.5"), "值没变，汇总也不该动");

        // 一次改多列：审计一条、changed_fields 两列（升序）
        let multi = RecordPatchReq {
            should_days: Some(dec("21")),
            remark: Some(Some("应出勤按 21 天口径".to_string())),
            ..RecordPatchReq::default()
        };
        attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &multi)
            .await
            .expect("多列修正");
        assert_eq!(fx.audit_count(a).await, 2);
        let (changed_fields, diff_text) = fx.last_audit(a).await;
        assert_eq!(
            changed_fields,
            vec!["remark".to_string(), "should_days".to_string()],
            "两列一起改要一起记"
        );
        assert!(diff_text.contains("应出勤按 21 天口径"), "{diff_text}");

        // 备份恢复原状不必要（清场会删）—— 直接清理
        fx.clean(period_id, &[a, b]).await;
    }

    // ==================== ③ 锁定期间不可改 ====================

    /// 锁定之后：单行修正 409 + `68010`，**库里逐列未变**，台账行标 `editable=false`；
    /// 解锁后又能改（设计 §4.2.3 的唯一出口）。
    #[tokio::test]
    async fn locked_period_blocks_record_updates() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("locked").await;
        let period_id = fx.create_period().await;
        let (a, b) = fx.seed_records(period_id).await;
        let before = fx.stored_numbers(a).await;

        // DRAFT → SUBMITTED → LOCKED
        let locked = fx.drive_status(period_id, STATUS_LOCKED).await;
        assert_eq!(locked.status, STATUS_LOCKED);

        // 台账还能看，但标成不可编辑
        let (rows, total) = attendance_record_handler::page_records(&super_principal(), &filter_of(period_id), 1, 50)
            .await
            .expect("锁定期间台账仍可读");
        assert_eq!(total, 2);
        assert!(
            rows.iter().all(|row| !row.editable),
            "锁定期间的行必须标 editable=false"
        );
        assert_eq!(rows[0].period_status, STATUS_LOCKED);

        // 改 → 409 + 68010
        let req = RecordPatchReq {
            actual_days: Some(dec("1")),
            ..RecordPatchReq::default()
        };
        let error = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &req)
            .await
            .expect_err("锁定期间修正必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(error.msg.contains(&fx.code), "报文要点名期间：{}", error.msg);
        assert!(error.msg.contains("解锁"), "报文要给下一步：{}", error.msg);

        // 库里的 11 个数值列逐列未变（不是「报错了但写进去了」）
        assert_eq!(fx.stored_numbers(a).await, before, "锁定期间的记录必须逐列不变");
        assert_eq!(fx.audit_count(a).await, 0, "被拒的修改不该留审计");

        // 解锁（要原因）后又能改
        let unlocked = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &PeriodStatusReq {
                to_status: Some(STATUS_DRAFT.to_string()),
                reason: Some("HR20 用例：核对补录后解锁重算".to_string()),
            },
        )
        .await
        .expect("解锁");
        assert_eq!(unlocked.status, STATUS_DRAFT);
        let after_unlock = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &req)
            .await
            .expect("解锁后可改");
        assert!(after_unlock.changed);
        assert_eq!(after_unlock.record.numbers.actual_days, dec("1"));

        fx.clean(period_id, &[a, b]).await;
    }

    // ==================== ④ 入参与 404 ====================

    /// 入参：空 body / 越界 → 400；不存在的记录 → 404；403 之外不该有 500。
    #[tokio::test]
    async fn invalid_payloads_are_rejected_before_writing() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("invalid").await;
        let period_id = fx.create_period().await;
        let (a, b) = fx.seed_records(period_id).await;
        let before = fx.stored_numbers(a).await;

        // 一个字段都没给
        let empty = RecordPatchReq::default();
        let error = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &empty)
            .await
            .expect_err("空入参必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        // 天数超 31
        let too_many = RecordPatchReq {
            should_days: Some(dec("40")),
            ..RecordPatchReq::default()
        };
        let error = attendance_record_handler::patch_record(fx.operator(), &fx.request_ctx(), a, &too_many)
            .await
            .expect_err("越界必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("should_days"), "{}", error.msg);

        // 不存在的记录 → 404
        let error = attendance_record_handler::patch_record(
            fx.operator(),
            &fx.request_ctx(),
            Uuid::now_v7(),
            &RecordPatchReq {
                late_count: Some(1),
                ..RecordPatchReq::default()
            },
        )
        .await
        .expect_err("不存在的记录必须 404");
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);

        // 被拒的请求一个字节都没写进库
        assert_eq!(fx.stored_numbers(a).await, before);
        assert_eq!(fx.audit_count(a).await, 0);

        fx.clean(period_id, &[a, b]).await;
    }

    /// 台账 `period_id` 缺失 / 非 uuid 是 400（形状错），不是空列表。
    #[test]
    fn filter_shape_is_enforced() {
        use crate::business::hr::attendance_record_dto::{RecordPageQuery, filter_of as dto_filter_of};
        let error = dto_filter_of(&RecordPageQuery::default()).expect_err("缺期间必须 400");
        assert_eq!(error.status, 400);
        let bad = RecordPageQuery {
            period_id: Some("2400-01".to_string()),
            ..RecordPageQuery::default()
        };
        assert!(dto_filter_of(&bad).is_err());
    }
}
