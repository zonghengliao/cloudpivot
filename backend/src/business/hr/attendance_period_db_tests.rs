//! 考勤期间（`biz_attendance_period`）的真库用例（HR-19）。
//!
//! 单测（`attendance_period_dto` 的期间码 / 区间 / 状态形状、`attendance_period_model` 的
//! 占位符与守卫 SQL）证明的是**校验与拼装**；证明不了五件事，只有真库能答：
//!
//! 1. `INSERT` / `UPDATE` / 状态 CAS / 重算 SQL 的占位符与 `?::text::date`、`?::numeric`
//!    写法真能跑（尤其 `NULLIF(?::text,'')::timestamptz` 这条时间写法）；
//! 2. **验收项「状态机正确」**：`DRAFT→SUBMITTED→LOCKED→DRAFT` 三条边真能走，
//!    非法边（`DRAFT→LOCKED`、`LOCKED→SUBMITTED`）真被拒，`biz_status_log` 每次都在；
//! 3. **验收项「汇总重算幂等」**：两次重算四个快照列完全一致、`changed=false`；
//! 4. `uk_biz_att_period` 撞码真翻 `409 + 68009`（报文点名期间码）；
//! 5. 锁定后「改 / 删 / 重算」被库层与 handler 双层挡住（`409 + 68010`）。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr19_card \
//!   cargo test --lib business::hr::attendance_period_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 / HR-15 / HR-16 的教训）
//!
//! - 期间码带**时间戳派生的年份**（2400+），重跑不撞 `uk_biz_att_period`，
//!   也不会和真实演示数据（2026 年）搅在一起；
//! - 清场**按 id 删**（绝不按前缀扫全库）：记录 → 期间 → 状态日志 → 员工 → 用户 → 部门
//!   （FK 顺序不能换：记录/期间的 `employee_id` 是 `RESTRICT`）；
//! - 员工走真实 `employee_handler::create_employee`（考勤记录挂在员工外键上，
//!   自己拼 INSERT 会漏掉 `uk_biz_employee_*` 的口径）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::attendance_period_dto::{
        AttendancePeriodRes, PeriodCreateReq, PeriodPageQuery, PeriodStatusReq, PeriodUpdateReq,
    };
    use crate::business::hr::attendance_period_handler;
    use crate::business::hr::employee_handler;
    use crate::business::hr::model::SENTINEL_ORG_ID;
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

    /// 期间码：年份取 2400~2899（远离真实数据与演示库），月份取 01~12。
    ///
    /// 直接写死 `2026-11` 会撞上一次跑剩下的行（`uk_biz_att_period` 是全局唯一），
    /// 用例之间也会互相踩 —— 所以期间码必须由时间戳派生。
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

    fn money(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法金额")
    }

    /// 夹具：一个部门 + 两名员工 + 清理清单。
    struct Fixture {
        dept: Uuid,
        dept_code: String,
        emp1: Uuid,
        emp1_no: String,
        emp1_name: String,
        user1_name: String,
        emp2: Uuid,
        emp2_no: String,
        emp2_name: String,
        user2_name: String,
        code: String,
    }

    async fn fresh_dept(tag: &str) -> (Uuid, String) {
        let code = format!("HR19-{tag}-{}", uniq());
        let id = test_helpers::ensure_dept(&code, &format!("HR19 部门 {tag}")).await;
        (id, code)
    }

    async fn fresh_employee(tag: &str, dept: Uuid) -> (Uuid, String, String, String) {
        let suffix = uniq();
        let username = format!("hr19.{tag}.{suffix}");
        let name = format!("HR19 员工 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR19-{tag}-{suffix}")),
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
        (emp_id, emp_no, name, username)
    }

    /// 直接读工号（`create_employee` 回执只给 id + emp_no，这里统一成一条路）。
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
        let (dept, dept_code) = fresh_dept(tag).await;
        let (emp1, emp1_no, emp1_name, user1_name) = fresh_employee("a", dept).await;
        let (emp2, emp2_no, emp2_name, user2_name) = fresh_employee("b", dept).await;
        Fixture {
            dept,
            dept_code,
            emp1,
            emp1_no,
            emp1_name,
            user1_name,
            emp2,
            emp2_no,
            emp2_name,
            user2_name,
            code: period_code(),
        }
    }

    impl Fixture {
        fn create_req(&self) -> PeriodCreateReq {
            let (start, end) = month_range(&self.code);
            PeriodCreateReq {
                period_code: Some(self.code.clone()),
                period_start: Some(start.to_string()),
                period_end: Some(end.to_string()),
                remark: Some("HR19 夹具".to_string()),
            }
        }

        fn operator(&self) -> Operator {
            Operator::named(Some(Uuid::now_v7()), "HR19 夹具")
        }

        /// 插一条考勤记录（绕开 HR-20/21 的接口，本卡只需要「记录存在」这件事）。
        #[allow(clippy::too_many_arguments)]
        async fn insert_record(
            &self,
            period_id: Uuid,
            employee: Uuid,
            emp_no: &str,
            name: &str,
            should: &str,
            actual: &str,
            ot_workday: &str,
            ot_weekend: &str,
            ot_holiday: &str,
        ) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_attendance_record \
                     (id, org_id, period_id, employee_id, employee_no, employee_name, dept_id, \
                      should_days, actual_days, ot_hours_workday, ot_hours_weekend, ot_hours_holiday, \
                      source) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text, ?::text, ?::uuid, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, 'MANUAL')"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(&period_id),
                    Value::from(&employee),
                    Value::Text(emp_no.to_string()),
                    Value::Text(name.to_string()),
                    // uuid 参数必须是 `Value::from(&Uuid)`：驱动的 `PgParam::Text` 只接受
                    // text 家族类型，uuid 列传文本会在序列化阶段报
                    // 「error serializing parameter N」（数值列同理，见下面的 `?::text::numeric`）
                    Value::from(&self.dept),
                    Value::Text(should.to_string()),
                    Value::Text(actual.to_string()),
                    Value::Text(ot_workday.to_string()),
                    Value::Text(ot_weekend.to_string()),
                    Value::Text(ot_holiday.to_string()),
                ],
            )
            .await
            .unwrap_or_else(|err| panic!("插考勤记录失败：{err}"));
            id
        }

        /// 状态日志行数（`ATTENDANCE_PERIOD` 实体）。
        async fn log_count(&self, period_id: Uuid) -> i64 {
            let row = db()
                .query_first_sql(
                    "SELECT count(*)::bigint AS n FROM biz_status_log \
                      WHERE entity_type = 'ATTENDANCE_PERIOD' AND entity_id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("状态日志行数")
                .expect("count 必有行");
            row.get_typed("n").expect("n 列")
        }

        /// 最后一条状态日志的 `(from, to, reason)`。
        async fn last_log(&self, period_id: Uuid) -> (Option<String>, String, Option<String>) {
            let row = db()
                .query_first_sql(
                    "SELECT from_status, to_status, reason FROM biz_status_log \
                      WHERE entity_type = 'ATTENDANCE_PERIOD' AND entity_id = ?::uuid \
                      ORDER BY created_at DESC, id DESC LIMIT 1"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读最后一条状态日志")
                .expect("必有行");
            (
                row.get_typed("from_status").expect("from_status"),
                row.get_typed("to_status").expect("to_status"),
                row.get_typed("reason").expect("reason"),
            )
        }

        /// 库里该期间的行数（软删后仍在，用来验证「软删不是硬删」）。
        async fn period_rows(&self, period_id: Uuid) -> i64 {
            let row = db()
                .query_first_sql(
                    "SELECT count(*)::bigint AS n FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("期间行数")
                .expect("count 必有行");
            row.get_typed("n").expect("n 列")
        }

        /// 清场：记录 → 期间 → 状态日志 → 员工 → 用户 → 部门（FK 顺序不能换）。
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
        }
    }

    fn id_of(res: &AttendancePeriodRes) -> Uuid {
        Uuid::parse_str(&res.id).expect("回执 id")
    }

    fn status_req(to_status: &str, reason: Option<&str>) -> PeriodStatusReq {
        PeriodStatusReq {
            to_status: Some(to_status.to_string()),
            reason: reason.map(str::to_string),
        }
    }

    // ==================== ① 建 / 改 / 列表 ====================

    /// 建期间 → 列表能查到 → 改日期与备注 → 原样重放被 409 挡住。
    #[tokio::test]
    async fn create_update_and_list_roundtrip() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("crud").await;
        let created = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("建期间");
        let period_id = id_of(&created);
        assert_eq!(created.status, "DRAFT");
        assert_eq!(created.status_label, "草稿");
        assert!(!created.locked);
        assert_eq!(created.employee_count, 0, "新建期间一定没有记录");
        assert_eq!(created.should_days_total, Decimal::ZERO);
        assert_eq!(fx.log_count(period_id).await, 1, "建期要写一条 biz_status_log");
        let (from, to, _) = fx.last_log(period_id).await;
        assert_eq!(from, None, "建单日志的 from_status 为空");
        assert_eq!(to, "DRAFT");

        // 列表（年份 + 状态 + 关键字三个条件都命中自己）
        let year: i32 = fx.code[..4].parse().expect("年份");
        let (list, total) = attendance_period_handler::page_periods(
            &crate::business::hr::attendance_period_dto::filter_of(&PeriodPageQuery {
                keyword: Some(fx.code.clone()),
                status: Some("DRAFT".to_string()),
                year: Some(year),
                page: None,
                page_size: None,
            })
            .expect("合法查询串"),
            1,
            20,
        )
        .await
        .expect("列表");
        assert_eq!(total, 1, "按期间码过滤只应命中自己");
        assert_eq!(list[0].id, created.id);
        assert_eq!(list[0].period_code, fx.code);

        // 改：换成年内另一段区间 + 改备注
        let (start, _) = month_range(&fx.code);
        let moved_start = start + Duration::days(25);
        let updated = attendance_period_handler::update_period(
            fx.operator(),
            period_id,
            &PeriodUpdateReq {
                period_start: Some(moved_start.to_string()),
                period_end: Some((moved_start + Duration::days(29)).to_string()),
                remark: Some("  改成 26-25 计薪周期  ".to_string()),
            },
        )
        .await
        .expect("改期间");
        assert_eq!(updated.period_start, moved_start);
        assert_eq!(updated.period_end, moved_start + Duration::days(29));
        assert_eq!(
            updated.remark.as_deref(),
            Some("改成 26-25 计薪周期"),
            "备注 trim 后落库"
        );

        // 原样重放：409（不是静默成功 —— 「什么都没改」要让用户知道）
        let replay = attendance_period_handler::update_period(
            fx.operator(),
            period_id,
            &PeriodUpdateReq {
                period_start: Some(moved_start.to_string()),
                period_end: Some((moved_start + Duration::days(29)).to_string()),
                remark: Some("改成 26-25 计薪周期".to_string()),
            },
        )
        .await
        .expect_err("无改动要拦");
        assert_eq!(replay.status, 409);
        assert!(replay.msg.contains("没有任何改动"), "报文：{}", replay.msg);

        println!("[HR-19] ① 建/改/列表 code={} id={period_id}", fx.code);
        fx.clean(&[period_id]).await;
    }

    // ==================== ② 撞码 ====================

    /// 同一期间码建第二次 → `409 + 68009`，报文点名期间码（软删后可重建的那条口径另测）。
    #[tokio::test]
    async fn duplicate_period_code_is_conflict() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("dup").await;
        let first = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("首次建期");
        let period_id = id_of(&first);

        let second = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect_err("撞码要拦");
        assert_eq!(second.status, 409);
        assert_eq!(second.code, error_code::HR_ATTENDANCE_PERIOD_DUPLICATE);
        assert!(second.msg.contains(&fx.code), "报文要点名期间码：{}", second.msg);
        assert!(
            second.msg.contains("一个月只能有一批"),
            "报文要给下一步：{}",
            second.msg
        );

        println!("[HR-19] ② 撞码 → {} {} {}", second.status, second.code, second.msg);
        fx.clean(&[period_id]).await;
    }

    // ==================== ③ 状态机 ====================

    /// **验收项「状态机正确」**：三条合法边走通、非法边被拒、解锁必须给原因、
    /// 每条边都在 `biz_status_log` 里。
    #[tokio::test]
    async fn status_machine_walks_three_edges_and_logs_each() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("flow").await;
        let created = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("建期间");
        let period_id = id_of(&created);

        // 非法边 ①：DRAFT → LOCKED（边表里没有这一跳）
        let skip_submit =
            attendance_period_handler::change_status(fx.operator(), period_id, &status_req("LOCKED", None))
                .await
                .expect_err("DRAFT 不能直接锁定");
        assert_eq!(skip_submit.status, 409, "非法边是 409（不是 400）: {}", skip_submit.msg);
        assert_eq!(fx.log_count(period_id).await, 1, "被拒的流转不写日志");

        // 合法边 ①：DRAFT → SUBMITTED
        let submitted =
            attendance_period_handler::change_status(fx.operator(), period_id, &status_req("SUBMITTED", None))
                .await
                .expect("提交");
        assert_eq!(submitted.status, "SUBMITTED");
        assert!(submitted.submitted_at.is_some(), "提交要落 submitted_at");
        assert!(submitted.submitted_by.is_some(), "提交要落 submitted_by");

        // 合法边 ②：SUBMITTED → LOCKED
        let locked = attendance_period_handler::change_status(fx.operator(), period_id, &status_req("LOCKED", None))
            .await
            .expect("锁定");
        assert_eq!(locked.status, "LOCKED");
        assert!(locked.locked, "回执的 locked 标记要跟上");
        assert!(locked.locked_at.is_some(), "锁定要落 locked_at");

        // 非法边 ②：LOCKED → SUBMITTED（没有这条回位边）
        let backwards =
            attendance_period_handler::change_status(fx.operator(), period_id, &status_req("SUBMITTED", None))
                .await
                .expect_err("锁定不能退回提交态");
        assert_eq!(backwards.status, 409);

        // 解锁不给原因 → 400（设计 §4.2.3「解锁(给原因)」）
        let no_reason = attendance_period_handler::change_status(fx.operator(), period_id, &status_req("DRAFT", None))
            .await
            .expect_err("解锁必须给原因");
        assert_eq!(no_reason.status, 400);
        assert!(no_reason.msg.contains("原因"), "报文：{}", no_reason.msg);

        // 合法边 ③：LOCKED → DRAFT（带原因）
        let unlocked = attendance_period_handler::change_status(
            fx.operator(),
            period_id,
            &status_req("DRAFT", Some("薪资口径调整，需重导考勤")),
        )
        .await
        .expect("解锁");
        assert_eq!(unlocked.status, "DRAFT");
        assert!(!unlocked.locked);
        assert!(
            unlocked.locked_at.is_some(),
            "解锁保留最近一次锁定时间（审计），`status` 才是唯一真相"
        );
        let (from, to, reason) = fx.last_log(period_id).await;
        assert_eq!(from.as_deref(), Some("LOCKED"));
        assert_eq!(to, "DRAFT");
        assert_eq!(reason.as_deref(), Some("薪资口径调整，需重导考勤"), "原因要落状态日志");

        // 建单 + 三次流转 = 4 条日志
        assert_eq!(fx.log_count(period_id).await, 4, "每次成功流转都要有轨迹");

        println!(
            "[HR-19] ③ 状态机 DRAFT→SUBMITTED→LOCKED→DRAFT 日志={} 解锁原因={:?}",
            fx.log_count(period_id).await,
            reason
        );
        fx.clean(&[period_id]).await;
    }

    // ==================== ④ 锁定守卫 ====================

    /// 锁定后「改 / 删 / 重算」都要被挡（`409 + 68010`），且期间数据一字不动。
    #[tokio::test]
    async fn locked_period_blocks_edit_delete_and_recalc() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("lock").await;
        let created = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("建期间");
        let period_id = id_of(&created);
        attendance_period_handler::change_status(fx.operator(), period_id, &status_req("SUBMITTED", None))
            .await
            .expect("提交");
        attendance_period_handler::change_status(fx.operator(), period_id, &status_req("LOCKED", None))
            .await
            .expect("锁定");

        let edit = attendance_period_handler::update_period(
            fx.operator(),
            period_id,
            &PeriodUpdateReq {
                remark: Some("锁定后还想改".to_string()),
                ..PeriodUpdateReq::default()
            },
        )
        .await
        .expect_err("锁定后不能改");
        assert_eq!(edit.status, 409);
        assert_eq!(edit.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(edit.msg.contains("解锁"), "报文要点名下一步：{}", edit.msg);

        let recalc = attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect_err("锁定后不能重算");
        assert_eq!(recalc.status, 409);
        assert_eq!(recalc.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);

        let delete = attendance_period_handler::delete_period(fx.operator(), period_id)
            .await
            .expect_err("锁定后不能删");
        assert_eq!(delete.status, 409);
        assert_eq!(delete.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);

        // 库里状态与备注都没被上面的三次尝试改动，且期间仍在
        let row = crate::business::hr::attendance_period_model::find_in(db(), period_id)
            .await
            .expect("读期间")
            .expect("期间还在");
        assert_eq!(row.status, "LOCKED");
        assert_eq!(row.remark.as_deref(), Some("HR19 夹具"), "备注没被改");

        println!(
            "[HR-19] ④ 锁定守卫 改={} 重算={} 删={}",
            edit.code, recalc.code, delete.code
        );
        fx.clean(&[period_id]).await;
    }

    // ==================== ⑤ 汇总重算幂等 ====================

    /// **验收项「汇总重算幂等」**：按记录全量覆盖 → 第二次重算 `changed=false` 且数字不变 →
    /// 记录变了之后重算 `changed=true`。
    #[tokio::test]
    async fn recalc_is_idempotent_and_matches_records() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("calc").await;
        let created = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("建期间");
        let period_id = id_of(&created);

        let first = fx
            .insert_record(
                period_id,
                fx.emp1,
                &fx.emp1_no,
                &fx.emp1_name,
                "22",
                "20.5",
                "4",
                "2",
                "1",
            )
            .await;
        fx.insert_record(
            period_id,
            fx.emp2,
            &fx.emp2_no,
            &fx.emp2_name,
            "22",
            "22",
            "0",
            "0",
            "0",
        )
        .await;

        let recalc = attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect("首次重算");
        assert_eq!(recalc.scanned, 2, "两条记录都进汇总");
        assert!(recalc.changed, "首次重算（从 0 到真值）必须标记改变");
        assert_eq!(recalc.after.employee_count, 2);
        assert_eq!(recalc.after.should_days_total, money("44"));
        assert_eq!(recalc.after.actual_days_total, money("42.5"));
        assert_eq!(
            recalc.after.ot_hours_total,
            money("7"),
            "加班合计 = 平时 4 + 休息日 2 + 节假日 1"
        );
        assert_eq!(recalc.period.employee_count, 2, "回执的期间全量要跟上");

        // 幂等：同一份记录再算一次 → 数字一模一样、changed=false
        let again = attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect("第二次重算");
        assert!(!again.changed, "第二次重算不许改任何数字");
        assert!(again.before.same_as(&again.after), "重算前后必须逐列相等");
        assert_eq!(again.before, recalc.after, "第二次的 before 就是第一次的 after");
        assert_eq!(
            fx.period_rows(period_id).await,
            1,
            "重算不改期间行数（全量覆盖不是追加）"
        );

        // 记录变了 → 重算必须跟上（不是「算过一次就冻住」）
        db().exec_sql(
            "UPDATE biz_attendance_record SET actual_days = ?::text::numeric WHERE id = ?::uuid".to_string(),
            vec![Value::Text("21".to_string()), Value::from(&first)],
        )
        .await
        .expect("改记录");
        let after_change = attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect("第三次重算");
        assert!(after_change.changed, "记录改了之后重算要能看出来");
        assert_eq!(after_change.after.actual_days_total, money("43"));

        println!(
            "[HR-19] ⑤ 重算 首次={:?} 二次改变={} 三次{:?}",
            recalc.after, again.changed, after_change.after
        );
        fx.clean(&[period_id]).await;
    }

    // ==================== ⑥ 删除守卫 ====================

    /// 删期间的三个守卫：有记录不能删 → 清掉记录后可删（软删）→ 同期间码可重建。
    #[tokio::test]
    async fn delete_requires_draft_and_no_records() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("del").await;
        let created = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("建期间");
        let period_id = id_of(&created);
        let record = fx
            .insert_record(
                period_id,
                fx.emp1,
                &fx.emp1_no,
                &fx.emp1_name,
                "22",
                "22",
                "0",
                "0",
                "0",
            )
            .await;
        attendance_period_handler::recalc_period(fx.operator(), period_id)
            .await
            .expect("重算");

        let blocked = attendance_period_handler::delete_period(fx.operator(), period_id)
            .await
            .expect_err("有记录不能删");
        assert_eq!(blocked.status, 409);
        assert_eq!(blocked.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(blocked.msg.contains("考勤记录"), "报文要点名原因：{}", blocked.msg);

        // 删掉记录（本卡不实现记录层接口，直接清掉夹具那一行）
        db().exec_sql(
            "DELETE FROM biz_attendance_record WHERE id = ?::uuid".to_string(),
            vec![Value::from(&record)],
        )
        .await
        .expect("删记录");

        let deleted = attendance_period_handler::delete_period(fx.operator(), period_id)
            .await
            .expect("空草稿可删");
        assert_eq!(deleted.id, created.id, "回执是删除前的快照");
        assert_eq!(fx.period_rows(period_id).await, 1, "软删：行还在，只是带 deleted_at");
        let soft = crate::business::hr::attendance_period_model::find_in(db(), period_id)
            .await
            .expect("读期间");
        assert!(soft.is_none(), "软删后按 id 查不到");

        // 同期间码可以重建（`uk_biz_att_period` 带 `WHERE deleted_at IS NULL`）
        let rebuilt = attendance_period_handler::create_period(fx.operator(), &fx.create_req())
            .await
            .expect("软删后同期间码可重建");
        assert_ne!(rebuilt.id, created.id, "重建是新行");
        assert_eq!(rebuilt.status, "DRAFT");

        println!(
            "[HR-19] ⑥ 删除守卫 → 有记录 {} / 软删后重建 {}",
            blocked.code, rebuilt.id
        );
        fx.clean(&[period_id, id_of(&rebuilt)]).await;
    }
}
