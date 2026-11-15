//! 员工自助三接口（`/attendance/my` · `/leave/my` · `/salary/my`）的真库用例（HR-41）。
//!
//! 单测（`self_service_dto` / `self_service_model` / `self_service_handler` 里的十几条）
//! 证明的是**契约与 SQL 形状**；只有真库能回答这四件事：
//!
//! 1. **只返回自己的数据**（本卡验收项①）：两个账号各有员工档、考勤、请假、工资条，
//!    各自只看到自己那一份 —— 三条接口逐条断言「别人的行不在我这儿」；
//! 2. **没绑员工档的账号回空、不是 403**（本卡验收项②）：200 + `total = 0`，
//!    与「我确实没数据」在响应上不可区分；
//! 3. **在途薪资单默认不可见**：`CALCULATED` 单在默认口径下 0 行、`include_pending=true`
//!    才 1 行；改成 `POSTED` 后默认口径也能看到且 `paid = true`；
//! 4. **明细项是批量 IN 取回并按 `detail_id` 分组**：工资条上的项数 > 0，且项都属于本人那行。
//!
//! 不设库地址时全部 SKIP（**不假绿**）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr41_drill \
//!   cargo test --lib business::hr::self_service_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! * 编码前缀 `HR41`（员工工号 `HR41A-<纳秒>` / `HR41B-<纳秒>`，账号 `hr41_<纳秒>_a/b/c`）；
//! * 薪资期间固定 **`2026-10`**（避开 HR-34 的 `2026-11` 与 HR-36 的 `2026-12`：
//!   那些卡按期间清场，同一期间会让夹具互相踩）；
//! * 引擎取数是全量读（员工 / 标准 / 考勤），所以本文件必须 `--test-threads=1` 且自建自清；
//! * 薪资项用 `V0396` 的 17 个种子项（不自建项：种子 `GROSS` 公式逐项枚举了 8 个收入编码，
//!   多出来的自定义收入项会被引擎的 `check_summary` 正确拦下 —— 详见
//!   `salary_sheet_db_tests` 的夹具文档）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use nik::prelude::rust_decimal::Decimal;

    use crate::business::hr::fixture_hygiene as hygiene;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_sheet_dto::SalarySheetCalcReq;
    use crate::business::hr::salary_sheet_handler;
    use crate::business::hr::self_service_dto::{MyAttendanceQuery, MyLeaveQuery, MySalaryQuery};
    use crate::business::hr::self_service_handler as handler;
    use crate::business::hr::self_service_model as model;
    use crate::business::shared::state_machine::Operator;
    use tokio::sync::OnceCell;

    /// 夹具编码前缀（清场按它精确删）。
    const PREFIX: &str = "HR41";
    /// 薪资期间（`ck_biz_sal_sheet_code` 只认 `yyyy-MM`）。
    const PERIOD: &str = "2026-10";

    fn org() -> Uuid {
        SENTINEL_ORG_ID
    }

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址或连不上，用例直接跳过。
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
                        let database = url.rsplit('/').next().unwrap_or("?");
                        println!("HR41 真库用例已连接：database={database}");
                        set_db(connection).is_ok()
                    }
                    Err(err) => {
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    fn money(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    /// 一份自助夹具：3 个账号（A / B 各绑一份员工档，C 不绑）+ 考勤 + 请假 + 薪资单。
    struct Fixture {
        suffix: String,
        emp_a: Uuid,
        emp_b: Uuid,
        user_a: Uuid,
        user_b: Uuid,
        user_c: Uuid,
        period_id: Uuid,
        operator: Operator,
        /// 引擎输入集的互斥锁 guard（用例结束才释放；字段只为持有生命周期）。
        _engine: tokio::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
        fn emp_a_no(&self) -> String {
            format!("HR41A-{}", self.suffix)
        }

        fn emp_b_no(&self) -> String {
            format!("HR41B-{}", self.suffix)
        }

        /// 建单入参：两人都只靠「标准 + 考勤」，不填人工项（`INPUT` 项缺失 = 0）。
        fn calc_req(&self) -> SalarySheetCalcReq {
            SalarySheetCalcReq {
                period_code: Some(PERIOD.to_string()),
                attendance_period_id: Some(self.period_id.to_string()),
                dry_run: Some(false),
                remark: Some("HR41 真库用例".to_string()),
                manual: None,
            }
        }
    }

    async fn dept_id() -> Uuid {
        let row = db()
            .query_first_sql(
                "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY dept_name LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查部门")
            .expect("库里至少要有一个部门（bootstrap 会建「总部」）");
        row.get_typed("id").expect("部门 id")
    }

    /// 清场 = 统一清空薪资引擎的输入集 + 本模块自己的请假单 / 账号残留；
    /// 返回引擎互斥锁的 guard（本用例期间独占输入集）。
    async fn sweep() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = hygiene::exclusive().await;

        let statements: [(String, Vec<Value>); 9] = [
            // 状态日志没有外键（比业务行活得久），按「即将删掉的单」清，否则每轮留孤儿
            (
                "DELETE FROM biz_status_log WHERE entity_type = 'SALARY_SHEET' AND entity_id IN \
                    (SELECT id FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid)"
                    .to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid".to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_leave_request WHERE employee_no LIKE ?".to_string(),
                vec![Value::Text(format!("{PREFIX}%"))],
            ),
            (
                "DELETE FROM biz_attendance_record WHERE period_id IN \
                    (SELECT id FROM biz_attendance_period WHERE period_code = ? AND org_id = ?::uuid)"
                    .to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE period_code = ? AND org_id = ?::uuid".to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                format!(
                    "DELETE FROM biz_employee_salary WHERE employee_id IN \
                        (SELECT id FROM biz_employee WHERE emp_no LIKE '{PREFIX}%')"
                ),
                vec![],
            ),
            (
                format!("DELETE FROM biz_salary_item WHERE item_code LIKE '{PREFIX}%'"),
                vec![],
            ),
            (
                format!("DELETE FROM biz_employee WHERE emp_no LIKE '{PREFIX}%'"),
                vec![],
            ),
            (
                "DELETE FROM sys_user WHERE username LIKE 'hr41\\_%' ESCAPE '\\'".to_string(),
                vec![],
            ),
        ];
        for (sql, params) in statements {
            db().exec_sql(sql, params).await.expect("清场");
        }
        guard
    }

    async fn insert_user(id: Uuid, username: &str, real_name: &str) {
        db().exec_sql(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::uuid, ?, 'x', ?, 1)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(username.to_string()),
                Value::Text(real_name.to_string()),
            ],
        )
        .await
        .expect("插账号");
    }

    async fn insert_employee(id: Uuid, emp_no: &str, name: &str, dept: Uuid, user_id: Option<Uuid>) {
        db().exec_sql(
            "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                      employment_type, id_card_type, user_id) \
             VALUES (?::uuid, ?::uuid, ?, ?, 1, ?::uuid, ?::text::date, 'ACTIVE', 'FULL_TIME', 'ID_CARD', \
                     NULLIF(?::text,'')::uuid)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::from(&org()),
                Value::Text(emp_no.to_string()),
                Value::Text(name.to_string()),
                Value::from(&dept),
                Value::Text("2020-01-01".to_string()),
                Value::Text(user_id.map(|id| id.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .expect("插员工");
    }

    async fn setup() -> Option<Fixture> {
        if !ensure_db().await {
            return None;
        }
        let engine = sweep().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let user_a = Uuid::now_v7();
        let user_b = Uuid::now_v7();
        let user_c = Uuid::now_v7();
        let period_id = Uuid::now_v7();

        insert_user(user_a, &format!("hr41_{suffix}_a"), "自助甲").await;
        insert_user(user_b, &format!("hr41_{suffix}_b"), "自助乙").await;
        insert_user(user_c, &format!("hr41_{suffix}_c"), "未绑档").await;

        insert_employee(emp_a, &format!("HR41A-{suffix}"), "自助甲", dept, Some(user_a)).await;
        insert_employee(emp_b, &format!("HR41B-{suffix}"), "自助乙", dept, Some(user_b)).await;

        // 薪资标准：两人金额不同，便于「拿错行」时立刻看出来
        for (employee_id, amount) in [(emp_a, "8000"), (emp_b, "6000")] {
            db().exec_sql(
                "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, 'BASE', ?::text::numeric, ?::text::date)"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::from(&employee_id),
                    Value::Text(amount.to_string()),
                    Value::Text("2020-01-01".to_string()),
                ],
            )
            .await
            .expect("插薪资标准");
        }

        // 考勤期间（DRAFT：试算 / 建单允许，锁定是过账的要求）
        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')"
                .to_string(),
            vec![
                Value::from(&period_id),
                Value::from(&org()),
                Value::Text(PERIOD.to_string()),
                Value::Text("2026-10-01".to_string()),
                Value::Text("2026-10-31".to_string()),
            ],
        )
        .await
        .expect("插考勤期间");

        // 考勤记录：甲加班 10h、乙加班 2h（数字不同 → 断言能区分是谁那一行）
        for (employee_id, no, name, ot) in [
            (emp_a, format!("HR41A-{suffix}"), "自助甲", "10"),
            (emp_b, format!("HR41B-{suffix}"), "自助乙", "2"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_attendance_record (id, org_id, period_id, employee_id, employee_no, employee_name, \
                                                    dept_id, should_days, actual_days, leave_unpaid_days, \
                                                    ot_hours_workday, source) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, ?, ?::uuid, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, 'MANUAL')"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::from(&period_id),
                    Value::from(&employee_id),
                    Value::Text(no),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("22".to_string()),
                    Value::Text("20".to_string()),
                    Value::Text("2".to_string()),
                    Value::Text(ot.to_string()),
                ],
            )
            .await
            .expect("插考勤记录");
        }

        // 请假：两人各一张（乙那张 APPROVED，用于状态筛选断言）
        for (employee_id, no, name, leave_no, status, days) in [
            (emp_a, format!("HR41A-{suffix}"), "自助甲", "1", "SUBMITTED", "1.00"),
            (emp_b, format!("HR41B-{suffix}"), "自助乙", "2", "APPROVED", "0.50"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_leave_request (id, org_id, leave_no, employee_id, employee_no, employee_name, \
                                                dept_id, leave_type, start_time, end_time, duration_days, paid, \
                                                status, reason) \
                 VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?, ?, ?::uuid, 'PERSONAL', \
                         ?::text::timestamptz, ?::text::timestamptz, ?::text::numeric, 0, ?, '家中有事')"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::Text(format!("LV-{PREFIX}-{suffix}-{leave_no}")),
                    Value::from(&employee_id),
                    Value::Text(no),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("2026-10-08T09:00:00+08:00".to_string()),
                    Value::Text(if days == "0.50" {
                        "2026-10-08T12:00:00+08:00".to_string()
                    } else {
                        "2026-10-09T09:00:00+08:00".to_string()
                    }),
                    Value::Text(days.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("插请假单");
        }

        let operator_id = db()
            .query_first_sql(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查操作人")
            .map(|row| row.get_typed::<Uuid>("id").expect("用户 id"));

        Some(Fixture {
            suffix,
            emp_a,
            emp_b,
            user_a,
            user_b,
            user_c,
            period_id,
            operator: Operator::named(operator_id, "hr41-test"),
            _engine: engine,
        })
    }

    async fn cleanup(fixture: &Fixture) {
        // 叶子先删：明细项 / 明细随单头级联
        for (sql, params) in [
            // 状态日志没有外键：不先收走就会在库里留孤儿（每轮 2 行）
            (
                "DELETE FROM biz_status_log WHERE entity_type = 'SALARY_SHEET' AND entity_id IN \
                    (SELECT id FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid)",
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid",
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_leave_request WHERE employee_no IN (?, ?)",
                vec![Value::Text(fixture.emp_a_no()), Value::Text(fixture.emp_b_no())],
            ),
            (
                "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid",
                vec![Value::from(&fixture.period_id)],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid",
                vec![Value::from(&fixture.period_id)],
            ),
            (
                "DELETE FROM biz_employee_salary WHERE employee_id IN (?::uuid, ?::uuid)",
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM sys_user WHERE id IN (?::uuid, ?::uuid, ?::uuid)",
                vec![
                    Value::from(&fixture.user_a),
                    Value::from(&fixture.user_b),
                    Value::from(&fixture.user_c),
                ],
            ),
        ] {
            let _ = db().exec_sql(sql.to_string(), params).await;
        }
    }

    /// 建单（引擎落库），返回单头 id。
    async fn create_sheet(fixture: &Fixture) -> Uuid {
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req())
            .await
            .expect("建单必须成功");
        let sheet = created.sheet.expect("建单必须回单头");
        Uuid::parse_str(&sheet.id).expect("单头 id")
    }

    async fn set_sheet_status(sheet_id: Uuid, status: &str) {
        db().exec_sql(
            "UPDATE biz_salary_sheet SET status = ? WHERE id = ?::uuid".to_string(),
            vec![Value::Text(status.to_string()), Value::from(&sheet_id)],
        )
        .await
        .expect("改单状态");
    }

    // ---- ① 考勤：只看自己 ----

    #[tokio::test]
    async fn attendance_my_returns_only_the_callers_rows() {
        let Some(fixture) = setup().await else { return };

        let (rows_a, total_a, page, page_size) = handler::attendance_of(fixture.user_a, &MyAttendanceQuery::default())
            .await
            .expect("甲的我的考勤");
        assert_eq!(page, 1);
        assert_eq!(page_size, crate::business::hr::self_service_dto::MY_PAGE_DEFAULT);
        assert_eq!(total_a, 1, "甲只有一条考勤记录");
        assert_eq!(rows_a.len(), 1);
        assert_eq!(rows_a[0].period_code, PERIOD);
        assert_eq!(rows_a[0].numbers.ot_hours_workday, money("10"), "甲那行是加班 10h");
        assert_eq!(rows_a[0].ot_hours_total, money("10"));
        assert!(!rows_a[0].period_status_label.is_empty());
        assert!(rows_a[0].source_label.contains("手工") || !rows_a[0].source_label.is_empty());

        let (rows_b, total_b, _, _) = handler::attendance_of(fixture.user_b, &MyAttendanceQuery::default())
            .await
            .expect("乙的我的考勤");
        assert_eq!(total_b, 1);
        assert_eq!(rows_b[0].numbers.ot_hours_workday, money("2"), "乙那行是加班 2h");
        assert_ne!(rows_a[0].id, rows_b[0].id, "两人拿到的必须是各自的记录");

        // 期间筛选：命中 → 1 条；不存在期间 → 0 条（不是报错）
        let (hit, hit_total, _, _) = handler::attendance_of(
            fixture.user_a,
            &MyAttendanceQuery {
                period_code: Some(PERIOD.to_string()),
                page: None,
                page_size: None,
            },
        )
        .await
        .expect("按期筛选");
        assert_eq!(hit_total, 1);
        assert_eq!(hit[0].id, rows_a[0].id);

        let (miss, miss_total, _, _) = handler::attendance_of(
            fixture.user_a,
            &MyAttendanceQuery {
                period_code: Some("2025-01".to_string()),
                page: None,
                page_size: None,
            },
        )
        .await
        .expect("空期间是空列表，不是错误");
        assert_eq!(miss_total, 0);
        assert!(miss.is_empty());

        cleanup(&fixture).await;
    }

    // ---- ② 请假：只看自己 + 状态筛选 ----

    #[tokio::test]
    async fn leave_my_returns_only_the_callers_requests() {
        let Some(fixture) = setup().await else { return };

        let (rows_a, total_a, _, _) = handler::leaves_of(fixture.user_a, &MyLeaveQuery::default())
            .await
            .expect("甲的我的请假");
        assert_eq!(total_a, 1);
        assert_eq!(rows_a[0].status, "SUBMITTED", "甲那张是审批中");
        assert!(!rows_a[0].paid, "夹具里是无薪事假");
        assert!(rows_a[0].leave_no.contains(PREFIX));
        assert!(!rows_a[0].start_time.is_empty());

        let (rows_b, _, _, _) = handler::leaves_of(fixture.user_b, &MyLeaveQuery::default())
            .await
            .expect("乙的我的请假");
        assert_eq!(rows_b[0].status, "APPROVED");
        assert_eq!(rows_b[0].duration_days, money("0.50"));
        assert_ne!(rows_a[0].id, rows_b[0].id, "两边必须是各自的单");

        // 状态筛选：甲按 APPROVED 筛 → 0 条（他只有 SUBMITTED）；乙按 APPROVED 筛 → 1 条
        let query = |status: &str| MyLeaveQuery {
            status: Some(status.to_string()),
            page: None,
            page_size: None,
        };
        let (none, none_total, _, _) = handler::leaves_of(fixture.user_a, &query("APPROVED"))
            .await
            .expect("按状态筛");
        assert_eq!(none_total, 0, "甲没有已批准的假");
        assert!(none.is_empty());

        let (approved, approved_total, _, _) = handler::leaves_of(fixture.user_b, &query("approved"))
            .await
            .expect("小写状态也认");
        assert_eq!(approved_total, 1);
        assert_eq!(approved[0].id, rows_b[0].id);

        // 非法状态 → 400 且点名收到值
        let error = handler::leaves_of(fixture.user_a, &query("PAID"))
            .await
            .expect_err("PAID 不是请假状态");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("PAID"), "{}", error.msg);

        cleanup(&fixture).await;
    }

    // ---- ③ 薪资：只看自己 + 在途单默认不可见（本卡验收项） ----

    #[tokio::test]
    async fn salary_my_hides_pending_sheets_and_returns_only_mine() {
        let Some(fixture) = setup().await else { return };
        let sheet_id = create_sheet(&fixture).await;

        // 建单后是 CALCULATED（在途）：默认口径一行都不该看到
        let (hidden, hidden_total, _, _) = handler::salary_of(fixture.user_a, &MySalaryQuery::default())
            .await
            .expect("甲的我的薪资（默认）");
        assert_eq!(hidden_total, 0, "在途单不许出现在员工自助页：{hidden:?}");
        assert!(hidden.is_empty());

        // include_pending=true 才看得到（HR 自查用），且只有自己那一行
        let pending_query = MySalaryQuery {
            period_code: None,
            include_pending: Some(true),
            page: None,
            page_size: None,
        };
        let (pending_a, pending_total_a, _, _) = handler::salary_of(fixture.user_a, &pending_query)
            .await
            .expect("甲的我的薪资（含在途）");
        assert_eq!(pending_total_a, 1);
        assert_eq!(pending_a[0].sheet_id, sheet_id.to_string());
        assert_eq!(pending_a[0].employee_no, fixture.emp_a_no());
        assert_eq!(pending_a[0].sheet_status, "CALCULATED");
        assert!(!pending_a[0].paid, "已计算 ≠ 已发放");
        assert_eq!(
            pending_a[0].gross_amount,
            pending_a[0].earning_amount - pending_a[0].pre_deduct_amount,
            "工资条内部恒等式（明细行 CHECK 的读回值）"
        );
        assert_eq!(
            pending_a[0].gross_amount,
            pending_a[0].net_amount + pending_a[0].withhold_amount
        );
        assert!(pending_a[0].earning_amount >= money("8000"), "甲的 BASE 8000 没算进去");
        assert!(!pending_a[0].items.is_empty(), "明细项要能展开");
        assert!(
            pending_a[0].items.iter().any(|item| item.item_code == "BASE"),
            "明细项里必须有 BASE：{:?}",
            pending_a[0]
                .items
                .iter()
                .map(|item| &item.item_code)
                .collect::<Vec<_>>()
        );

        let (pending_b, pending_total_b, _, _) = handler::salary_of(fixture.user_b, &pending_query)
            .await
            .expect("乙的我的薪资（含在途）");
        assert_eq!(pending_total_b, 1);
        assert_eq!(pending_b[0].employee_no, fixture.emp_b_no());
        assert_ne!(pending_a[0].detail_id, pending_b[0].detail_id, "两人是各自的明细行");
        assert!(
            pending_a[0].items.iter().all(|item| !item.item_name.is_empty()),
            "明细项必须有名称（公式解释层的快照列）：{:?}",
            pending_a[0].items
        );
        assert!(
            pending_a[0].items.len() >= pending_b[0].items.len(),
            "甲的明细项（BASE 8000 + 加班 10h）不该少于乙：{} vs {}",
            pending_a[0].items.len(),
            pending_b[0].items.len()
        );

        // 改成 POSTED（已定稿）：默认口径就能看到，且 paid = true
        set_sheet_status(sheet_id, "POSTED").await;
        let (posted, posted_total, _, _) = handler::salary_of(fixture.user_a, &MySalaryQuery::default())
            .await
            .expect("甲看已过账的工资条");
        assert_eq!(posted_total, 1, "已过账必须在默认口径里可见");
        assert!(posted[0].paid, "POSTED 应标记为已发放");
        assert_eq!(posted[0].sheet_status_label, "已过账");

        // 期间筛选：命中 1 条、不存在期间 0 条
        let (hit, hit_total, _, _) = handler::salary_of(
            fixture.user_a,
            &MySalaryQuery {
                period_code: Some(PERIOD.to_string()),
                include_pending: None,
                page: None,
                page_size: None,
            },
        )
        .await
        .expect("按期筛工资条");
        assert_eq!(hit_total, 1);
        assert_eq!(hit[0].detail_id, posted[0].detail_id);
        let (miss, miss_total, _, _) = handler::salary_of(
            fixture.user_a,
            &MySalaryQuery {
                period_code: Some("2025-01".to_string()),
                include_pending: None,
                page: None,
                page_size: None,
            },
        )
        .await
        .expect("空期间");
        assert_eq!(miss_total, 0);
        assert!(miss.is_empty());

        cleanup(&fixture).await;
    }

    // ---- ④ 没绑员工档的账号：200 + 空（不是 403） ----

    #[tokio::test]
    async fn unbound_account_gets_empty_pages_not_forbidden() {
        let Some(fixture) = setup().await else { return };
        create_sheet(&fixture).await;

        // 账号没有员工档 → 三条接口都是空页
        assert!(
            model::my_employee_id(fixture.user_c)
                .await
                .expect("解析员工档")
                .is_none()
        );

        let default_page = crate::business::hr::self_service_dto::MY_PAGE_DEFAULT;
        let (att_rows, att_total, att_page, att_size) =
            handler::attendance_of(fixture.user_c, &MyAttendanceQuery::default())
                .await
                .expect("attendance：未绑员工档不许报错（应回空页）");
        assert!(att_rows.is_empty(), "attendance：不许有行");
        assert_eq!(att_total, 0, "attendance：total 必须是 0");
        assert_eq!((att_page, att_size), (1, default_page));

        let (leave_rows, leave_total, _, _) = handler::leaves_of(fixture.user_c, &MyLeaveQuery::default())
            .await
            .expect("leave：未绑员工档不许报错（应回空页）");
        assert!(leave_rows.is_empty(), "leave：不许有行");
        assert_eq!(leave_total, 0, "leave：total 必须是 0");

        let (salary_rows, salary_total, _, _) = handler::salary_of(
            fixture.user_c,
            &MySalaryQuery {
                period_code: None,
                include_pending: Some(true),
                page: None,
                page_size: None,
            },
        )
        .await
        .expect("salary：未绑员工档不许报错（应回空页）");
        assert!(salary_rows.is_empty(), "salary：不许有行");
        assert_eq!(salary_total, 0, "salary：total 必须是 0");

        // 已绑账号反过来必须看得到数据（证明上面不是「全都空」的假绿）
        let (rows_a, total_a, _, _) = handler::attendance_of(fixture.user_a, &MyAttendanceQuery::default())
            .await
            .expect("甲的考勤");
        assert_eq!((rows_a.len(), total_a), (1, 1));

        // 员工档被软删 → 视为没绑（自助页回空，不是报错）
        db().exec_sql(
            "UPDATE biz_employee SET deleted_at = CURRENT_TIMESTAMP WHERE id = ?::uuid".to_string(),
            vec![Value::from(&fixture.emp_a)],
        )
        .await
        .expect("软删员工档");
        assert!(
            model::my_employee_id(fixture.user_a)
                .await
                .expect("解析员工档")
                .is_none(),
            "软删的员工档不许再解析出本人"
        );
        let (gone, gone_total, _, _) = handler::attendance_of(fixture.user_a, &MyAttendanceQuery::default())
            .await
            .expect("软删后仍是空页");
        assert_eq!(gone_total, 0);
        assert!(gone.is_empty());

        cleanup(&fixture).await;
    }

    // ---- ⑤ 分页：page_size=1 翻页不重不漏 ----

    #[tokio::test]
    async fn payslip_paging_walks_every_period_once() {
        let Some(fixture) = setup().await else { return };
        let first = create_sheet(&fixture).await;
        set_sheet_status(first, "POSTED").await;

        // 第二张单：换期间（同期间唯一键不允许两张），只为验证翻页
        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, '2026-09', '2026-09-01'::date, '2026-09-30'::date, 'DRAFT')"
                .to_string(),
            vec![Value::from(&Uuid::now_v7()), Value::from(&org())],
        )
        .await
        .expect("插 2026-09 期间");
        let second_req = SalarySheetCalcReq {
            period_code: Some("2026-09".to_string()),
            attendance_period_id: None,
            dry_run: Some(false),
            remark: Some("HR41 翻页夹具".to_string()),
            manual: None,
        };
        salary_sheet_handler::persist(fixture.operator.clone(), &second_req)
            .await
            .expect("建第二张单");
        let second_id: Uuid = db()
            .query_first_sql(
                "SELECT id FROM biz_salary_sheet WHERE period_code = '2026-09' AND org_id = ?::uuid".to_string(),
                vec![Value::from(&org())],
            )
            .await
            .expect("查第二张单")
            .expect("第二张单必须存在")
            .get_typed("id")
            .expect("单头 id");
        set_sheet_status(second_id, "POSTED").await;

        let mut seen: Vec<String> = Vec::new();
        for page in 1..=2 {
            let (rows, total, _, _) = handler::salary_of(
                fixture.user_a,
                &MySalaryQuery {
                    period_code: None,
                    include_pending: None,
                    page: Some(page),
                    page_size: Some(1),
                },
            )
            .await
            .expect("翻页");
            assert!(total >= 2, "两个期间各一张单，total={total}");
            assert_eq!(rows.len(), 1, "page_size=1 每页一行");
            seen.extend(rows.iter().map(|row| row.period_code.clone()));
        }
        assert_eq!(seen.len(), 2, "两页共两行（不重不漏）：{seen:?}");
        assert_ne!(seen[0], seen[1], "两页不许是同一张单：{seen:?}");

        // 清场：第二张单 + 它的期间（cleanup 只按固定期间删）
        // 状态日志先收（没有外键，不随单据级联）
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_status_log WHERE entity_type = 'SALARY_SHEET' AND entity_id IN \
                    (SELECT id FROM biz_salary_sheet WHERE period_code = '2026-09' AND org_id = ?::uuid)"
                    .to_string(),
                vec![Value::from(&org())],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_sheet WHERE period_code = '2026-09' AND org_id = ?::uuid".to_string(),
                vec![Value::from(&org())],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_period WHERE period_code = '2026-09' AND org_id = ?::uuid".to_string(),
                vec![Value::from(&org())],
            )
            .await;
        cleanup(&fixture).await;
    }
}
