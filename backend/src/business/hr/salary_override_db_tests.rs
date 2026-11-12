//! 薪资明细**手工覆盖**的真库用例（HR-37）。
//!
//! 单测（`salary_override_dto` / `salary_sheet_model` / `salary_sheet_handler`）证明的是
//! 「契约、聚合纯函数、SQL 形状」；只有真库能回答这四件事（= 本卡的三条验收项）：
//!
//! 1. **改单项后汇总自动重算**：改 `PERF` 一项之后，明细行 11 个金额列与单头 7 个金额列
//!    都必须跟着变，且「单头 = `SUM(明细)` = `SUM(明细项按类型)`」三口径仍然一分不差
//!    （`summary_of` 的 `consistent = true`）；库层六条 `CHECK` 一条都不能被撞。
//! 2. **越权改别人薪资被拒**：部门范围 / 仅本人范围之外的行 → 403，且**库里分文未动**、
//!    状态日志一行都不许多。
//! 3. **状态日志完整**：`entity_type / entity_id / entity_no / from / to / action /
//!    operator_name / reason / biz_ref_type / biz_ref_no` 十个字段逐字段可查，
//!    `reason` 里带「谁、哪个项、旧值→新值、差额、原因」。
//!
//! 另外两条守卫也在这里验（它们只在真库上才成立）：
//!
//! * `SUBMITTED` 之后的单子覆盖一律 409（设计 §4.8.3 的四态冻结）；
//! * 覆盖后算成负数（应发 < 0）→ 409 且整笔回滚（不留半笔改动的明细项）。
//!
//! 不设库地址时全部 SKIP（**不假绿**）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr37_drill \
//!   cargo test --lib business::hr::salary_override_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! * 员工工号前缀 `HR37A-<纳秒>` / `HR37B-<纳秒>`；
//! * 薪资期间固定 **`2027-01`**（HR-34 用 `2026-11`、HR-36 用 `2026-12`：各卡的 sweep 按
//!   期间删单，同期间会互相清掉夹具）；
//! * **不建薪资项**：直接用 `V0396` 的 17 个种子项（自建 `EARNING` 项会被 `GROSS` 的
//!   逐项公式拦下，见 `salary_sheet_db_tests` 顶部注释）；
//! * 引擎取数是**全量**读，所以本文件必须 `--test-threads=1`，且每个用例自建自清。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use nik::prelude::rust_decimal::Decimal;

    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_compute::STATUS_CALCULATED;
    use crate::business::hr::salary_override_dto::{ACTION_AMOUNT_OVERRIDE, SalaryOverrideItemReq, SalaryOverrideReq};
    use crate::business::hr::salary_sheet_dto::{STATUS_SUBMITTED, SalaryManualReq, SalarySheetCalcReq};
    use crate::business::hr::salary_sheet_handler;
    use crate::business::hr::salary_sheet_model::{self as model, TABLE_DETAIL};
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::{ENTITY_SALARY_SHEET, Operator, trail_of_no};
    use crate::shared::error_code;
    use tokio::sync::OnceCell;

    /// 夹具工号前缀（清场按它精确删）。
    const PREFIX: &str = "HR37";
    /// 薪资期间：与邻居卡错开（HR-34 `2026-11` / HR-36 `2026-12`）。
    const PERIOD: &str = "2027-01";

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
                        // 连上就打一行：`--nocapture` 时能证明这批用例**真的**跑了（不假绿）。
                        let database = url.rsplit('/').next().unwrap_or("?");
                        println!("HR37 真库用例已连接：database={database}");
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

    /// 唯一后缀（纳秒 + 进程内序号）。
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

    /// 一个夹具：2 名员工（同一部门）+ 标准 + 考勤期间与记录 + 一张已算出的薪资单。
    struct Fixture {
        suffix: String,
        emp_a: Uuid,
        emp_b: Uuid,
        /// 张三的明细 id（本卡的主角）。
        detail_a: Uuid,
        /// 李四的明细 id（越权用例的靶子）。
        detail_b: Uuid,
        sheet_id: Uuid,
        sheet_no: String,
        dept: Uuid,
        operator: Operator,
    }

    impl Fixture {
        fn emp_a_no(&self) -> String {
            format!("HR37A-{}", self.suffix)
        }

        /// 建单入参：张三有标准 + 考勤，李四两样都没有（金额全 0 也是合法行）。
        fn calc_req(&self) -> SalarySheetCalcReq {
            let entries = [
                ("PERF", "1200"),
                ("SI_PERSONAL", "840"),
                ("HF_PERSONAL", "960"),
                ("IIT", "0"),
                ("SI_COMPANY", "1680"),
                ("HF_COMPANY", "960"),
            ];
            SalarySheetCalcReq {
                period_code: Some(PERIOD.to_string()),
                attendance_period_id: None,
                dry_run: Some(false),
                remark: Some("HR37 真库用例".to_string()),
                manual: Some(
                    entries
                        .iter()
                        .map(|(code, value)| SalaryManualReq {
                            employee_id: Some(self.emp_a.to_string()),
                            item_code: Some((*code).to_string()),
                            amount: Some(money(value)),
                        })
                        .collect(),
                ),
            }
        }

        /// 覆盖请求（本卡的主角请求：改 PERF）。
        fn override_req(&self, item_code: &str, amount: &str, reason: &str) -> SalaryOverrideReq {
            SalaryOverrideReq {
                reason: Some(reason.to_string()),
                remark: Some("HR37 覆盖用例".to_string()),
                items: Some(vec![SalaryOverrideItemReq {
                    item_code: Some(item_code.to_string()),
                    amount: Some(money(amount)),
                }]),
            }
        }

        /// 部门范围主体：只能看本部门（本夹具两人都在同一个部门）。
        fn dept_principal(&self) -> ScopedPrincipal {
            let me = Uuid::now_v7();
            ScopedPrincipal::for_test(me, Some(self.dept), false, ScopeKind::Dept, vec![self.dept], false)
        }

        /// 仅本人主体：`user_id` 与明细的 `user_id`（NULL）不匹配 → 一律 403。
        fn self_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), None, false, ScopeKind::SelfOnly, Vec::new(), true)
        }

        /// 别的部门的主体 → 403。
        fn other_dept_principal(&self) -> ScopedPrincipal {
            let elsewhere = Uuid::now_v7();
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(elsewhere),
                false,
                ScopeKind::Dept,
                vec![elsewhere],
                false,
            )
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

    /// 清掉上一轮（可能是 panic 中断的）留下的 `HR37` 夹具。
    async fn sweep() {
        let statements: [String; 5] = [
            format!(
                "DELETE FROM biz_salary_sheet WHERE period_code = '{PERIOD}' AND org_id = '{}'",
                org()
            ),
            format!(
                "DELETE FROM biz_attendance_record WHERE period_id IN \
                   (SELECT id FROM biz_attendance_period WHERE period_code = '{PERIOD}' AND org_id = '{}')",
                org()
            ),
            format!(
                "DELETE FROM biz_attendance_period WHERE period_code = '{PERIOD}' AND org_id = '{}'",
                org()
            ),
            format!(
                "DELETE FROM biz_employee_salary WHERE employee_id IN \
                   (SELECT id FROM biz_employee WHERE emp_no LIKE '{PREFIX}%')"
            ),
            format!("DELETE FROM biz_employee WHERE emp_no LIKE '{PREFIX}%'"),
        ];
        for sql in statements {
            db().exec_sql(sql, vec![]).await.expect("清场");
        }
    }

    async fn setup() -> Option<Fixture> {
        if !ensure_db().await {
            return None;
        }
        sweep().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let period_id = Uuid::now_v7();

        for (id, no, name) in [
            (emp_a, format!("{PREFIX}A-{suffix}"), "张三"),
            (emp_b, format!("{PREFIX}B-{suffix}"), "李四"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                          employment_type, id_card_type) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 1, ?::uuid, ?::text::date, 'ACTIVE', 'FULL_TIME', 'ID_CARD')"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&org()),
                    Value::Text(no),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("2020-01-01".to_string()),
                ],
            )
            .await
            .expect("插员工");
        }

        // 标准：张三 BASE 8000（李四不配标准 → 全 0）
        db().exec_sql(
            "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'BASE', ?::text::numeric, ?::text::date)"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&emp_a),
                Value::Text("8000".to_string()),
                Value::Text("2020-01-01".to_string()),
            ],
        )
        .await
        .expect("插薪资标准");

        // 考勤期间（DRAFT：薪资试算允许 DRAFT 期间）
        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')"
                .to_string(),
            vec![
                Value::from(&period_id),
                Value::from(&org()),
                Value::Text(PERIOD.to_string()),
                Value::Text("2027-01-01".to_string()),
                Value::Text("2027-01-31".to_string()),
            ],
        )
        .await
        .expect("插考勤期间");

        // 考勤记录：只给张三（22 应出勤 / 20 实出勤 / 2 天无薪假 / 10 小时工作日加班）
        db().exec_sql(
            "INSERT INTO biz_attendance_record (id, org_id, period_id, employee_id, employee_no, employee_name, \
                                                dept_id, should_days, actual_days, leave_unpaid_days, \
                                                ot_hours_workday, source) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, '张三', ?::uuid, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, 'MANUAL')"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&period_id),
                Value::from(&emp_a),
                Value::Text(format!("{PREFIX}A-{suffix}")),
                Value::from(&dept),
                Value::Text("22".to_string()),
                Value::Text("20".to_string()),
                Value::Text("2".to_string()),
                Value::Text("10".to_string()),
            ],
        )
        .await
        .expect("插考勤记录");

        // 操作人用库里真实存在的账号（状态日志要写姓名快照；空库首启时回退成 None）
        let operator_id = db()
            .query_first_sql(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查操作人")
            .map(|row| row.get_typed::<Uuid>("id").expect("用户 id"));

        let mut fixture = Fixture {
            suffix,
            emp_a,
            emp_b,
            detail_a: Uuid::nil(),
            detail_b: Uuid::nil(),
            sheet_id: Uuid::nil(),
            sheet_no: String::new(),
            dept,
            operator: Operator::named(operator_id, "hr37-test"),
        };

        // 建单（走真实引擎：HR-36 的薄壳）
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req())
            .await
            .expect("建单必须成功");
        let sheet = created.sheet.clone().expect("建单必须回单头");
        assert_eq!(sheet.status, STATUS_CALCULATED);
        fixture.sheet_id = Uuid::parse_str(&sheet.id).expect("单头 id");
        fixture.sheet_no = sheet.sheet_no.clone();

        // 明细 id：本卡的主角是张三那一行
        for row in db()
            .query_sql(
                "SELECT id, employee_no FROM biz_salary_detail WHERE sheet_id = ?::uuid".to_string(),
                vec![Value::from(&fixture.sheet_id)],
            )
            .await
            .expect("查明细")
        {
            let id: Uuid = row.get_typed("id").expect("明细 id");
            let no: String = row.get_typed("employee_no").expect("工号");
            if no.starts_with(&format!("{PREFIX}A-")) {
                fixture.detail_a = id;
            } else {
                fixture.detail_b = id;
            }
        }
        assert_ne!(fixture.detail_a, Uuid::nil(), "没找到张三的明细行");
        assert_ne!(fixture.detail_b, Uuid::nil(), "没找到李四的明细行");

        Some(fixture)
    }

    async fn cleanup(fixture: &Fixture) {
        // 从叶子删起：明细项 / 明细随单头级联 → 先删单头；再考勤、标准、员工
        for (sql, params) in [
            (
                "DELETE FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&fixture.sheet_id)],
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
                "DELETE FROM biz_employee_salary WHERE employee_id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            // 状态日志没有外键（比业务行活得久），按单号清
            (
                "DELETE FROM biz_status_log WHERE entity_no = ?".to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
        ] {
            let _ = db().exec_sql(sql, params).await;
        }
    }

    // ---- 断言素材 ----

    /// 明细行 11 个金额列（直接读库，绕过接口）。
    struct DetailDb {
        earning: Decimal,
        pre_deduct: Decimal,
        gross: Decimal,
        social: Decimal,
        fund: Decimal,
        iit: Decimal,
        other: Decimal,
        withhold: Decimal,
        net: Decimal,
        si_company: Decimal,
        hf_company: Decimal,
    }

    async fn detail_db(detail_id: Uuid) -> DetailDb {
        let row = db()
            .query_first_sql(
                "SELECT earning_amount, pre_deduct_amount, gross_amount, social_personal, fund_personal, \
                        iit_amount, other_withhold, withhold_amount, net_amount, si_company, hf_company \
                   FROM biz_salary_detail WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&detail_id)],
            )
            .await
            .expect("读明细")
            .expect("明细必须存在");
        DetailDb {
            earning: row.get_typed("earning_amount").expect("earning"),
            pre_deduct: row.get_typed("pre_deduct_amount").expect("pre_deduct"),
            gross: row.get_typed("gross_amount").expect("gross"),
            social: row.get_typed("social_personal").expect("social"),
            fund: row.get_typed("fund_personal").expect("fund"),
            iit: row.get_typed("iit_amount").expect("iit"),
            other: row.get_typed("other_withhold").expect("other"),
            withhold: row.get_typed("withhold_amount").expect("withhold"),
            net: row.get_typed("net_amount").expect("net"),
            si_company: row.get_typed("si_company").expect("si_company"),
            hf_company: row.get_typed("hf_company").expect("hf_company"),
        }
    }

    /// 明细项的金额与说明（按编码）。
    async fn item_of(detail_id: Uuid, item_code: &str) -> (Decimal, String, Option<String>) {
        let row = db()
            .query_first_sql(
                "SELECT amount, COALESCE(calc_note, '') AS calc_note, expr_snapshot \
                   FROM biz_salary_detail_item WHERE detail_id = ?::uuid AND item_code = ?"
                    .to_string(),
                vec![Value::from(&detail_id), Value::Text(item_code.to_string())],
            )
            .await
            .expect("读明细项")
            .expect("明细项必须存在");
        (
            row.get_typed("amount").expect("金额"),
            row.get_typed("calc_note").expect("说明"),
            row.get_typed("expr_snapshot").expect("公式快照"),
        )
    }

    /// 状态日志（本单、本动作）的行数。
    async fn override_log_count(sheet_no: &str) -> i64 {
        db().query_first_sql(
            "SELECT count(*) AS n FROM biz_status_log WHERE entity_no = ? AND action = ?".to_string(),
            vec![
                Value::Text(sheet_no.to_string()),
                Value::Text(ACTION_AMOUNT_OVERRIDE.to_string()),
            ],
        )
        .await
        .expect("数日志")
        .map(|row| row.get_typed::<i64>("n").expect("count"))
        .unwrap_or(0)
    }

    // ==================== ① 改单项 → 重算明细 + 单头 ====================

    /// 本卡第一条验收：改 `PERF` 一项 1200 → 1500，明细 11 列与单头 7 列必须自动重算，
    /// 且三口径（单头 / `SUM(明细)` / `SUM(明细项按类型)`）一分不差。
    #[tokio::test]
    async fn override_recomputes_detail_and_sheet_totals() {
        let Some(fixture) = setup().await else { return };
        let before_detail = detail_db(fixture.detail_a).await;
        let before_sheet = model::find_sheet(fixture.sheet_id)
            .await
            .expect("读单头")
            .expect("单头存在");
        let (perf_before, _, perf_expr) = item_of(fixture.detail_a, "PERF").await;
        assert_eq!(perf_before, money("1200"));

        let res = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("PERF", "1500", "客户口头确认 2027-01 绩效补 300"),
        )
        .await
        .expect("覆盖必须成功");

        // ---- 回执 ----
        assert!(res.changed);
        assert_eq!(res.sheet_no, fixture.sheet_no);
        assert_eq!(res.sheet_status, STATUS_CALCULATED, "覆盖不改状态");
        assert_eq!(res.changes.len(), 1, "只改了一项");
        assert_eq!(res.changes[0].item_code, "PERF");
        assert_eq!(res.changes[0].old_amount, money("1200"));
        assert_eq!(res.changes[0].new_amount, money("1500"));
        assert_eq!(res.changes[0].delta, money("300"));
        assert!(res.log_id.is_some(), "改了钱必须留下状态日志 id");
        assert_eq!(res.detail.employee_no, fixture.emp_a_no());

        // ---- 明细项：金额与说明改了，公式快照没动 ----
        let (perf_after, note, expr_after) = item_of(fixture.detail_a, "PERF").await;
        assert_eq!(perf_after, money("1500"));
        assert!(note.contains("人工覆盖 1200.00→1500.00（+300.00）"), "说明：{note}");
        assert!(note.contains("客户口头确认"), "说明要带原因：{note}");
        assert_eq!(expr_after, perf_expr, "expr_snapshot 是历史证据，不许改");

        // ---- 明细 11 列：收入 +300，应发 +300，实发 +300，其余不变 ----
        let after_detail = detail_db(fixture.detail_a).await;
        assert_eq!(after_detail.earning, before_detail.earning + money("300"));
        assert_eq!(after_detail.gross, before_detail.gross + money("300"));
        assert_eq!(after_detail.net, before_detail.net + money("300"));
        assert_eq!(after_detail.pre_deduct, before_detail.pre_deduct);
        assert_eq!(after_detail.withhold, before_detail.withhold);
        assert_eq!(after_detail.si_company, before_detail.si_company);
        assert_eq!(after_detail.hf_company, before_detail.hf_company);
        // 恒等式（库层 CHECK 也这么要求）
        assert_eq!(after_detail.gross, after_detail.earning - after_detail.pre_deduct);
        assert_eq!(after_detail.gross, after_detail.net + after_detail.withhold);
        assert_eq!(
            after_detail.withhold,
            after_detail.social + after_detail.fund + after_detail.iit + after_detail.other
        );

        // ---- 单头 7 列：跟着明细走 ----
        let after_sheet = model::find_sheet(fixture.sheet_id)
            .await
            .expect("读单头")
            .expect("单头存在");
        assert_eq!(
            after_sheet.gross_total,
            before_sheet.gross_total + money("300"),
            "单头应发必须重算"
        );
        assert_eq!(after_sheet.net_total, before_sheet.net_total + money("300"));
        assert_eq!(after_sheet.earning_total, before_sheet.earning_total + money("300"));
        assert_eq!(after_sheet.withhold_total, before_sheet.withhold_total);
        assert_eq!(after_sheet.employee_count, before_sheet.employee_count);

        // ---- 三口径对拍（A6 / A7） ----
        let summary = salary_sheet_handler::summary_of(fixture.sheet_id)
            .await
            .expect("读汇总");
        assert!(summary.consistent, "覆盖后三口径必须仍一致：{:?}", summary.mismatches);
        assert_eq!(summary.sheet.totals, summary.detail_totals);
        assert_eq!(summary.sheet.totals.employee_count, 2);

        // ---- 状态日志：一行、字段齐全 ----
        let trail = trail_of_no(db(), &fixture.sheet_no).await.expect("读轨迹");
        let logs: Vec<_> = trail
            .iter()
            .filter(|row| row.action.as_deref() == Some(ACTION_AMOUNT_OVERRIDE))
            .collect();
        assert_eq!(logs.len(), 1, "一次覆盖只写一行日志");
        let log = logs[0];
        assert_eq!(log.entity_type, ENTITY_SALARY_SHEET);
        assert_eq!(log.entity_id, fixture.sheet_id);
        assert_eq!(log.from_status.as_deref(), Some(STATUS_CALCULATED));
        assert_eq!(log.to_status, STATUS_CALCULATED, "覆盖不改状态");
        assert!(
            log.operator_name.as_deref().unwrap_or("").contains("hr37-test"),
            "操作人姓名快照"
        );
        assert_eq!(log.biz_ref_type.as_deref(), Some(TABLE_DETAIL));
        assert_eq!(log.biz_ref_no.as_deref(), Some(fixture.emp_a_no().as_str()));
        let reason = log.reason.as_deref().unwrap_or("");
        for needle in [
            "张三",
            &fixture.emp_a_no(),
            "PERF 1200.00→1500.00",
            "+300.00",
            "客户口头确认 2027-01 绩效补 300",
        ] {
            assert!(reason.contains(needle), "日志原因缺「{needle}」：{reason}");
        }

        // 实测数字（`--nocapture` 时作为卡内证据）
        println!(
            "HR37 覆盖实测 单号={} 员工={} 应发 {}→{} 代扣 {} 实发 {}→{} 人数 {} 日志id={} 日志={}",
            fixture.sheet_no,
            fixture.emp_a_no(),
            before_sheet.gross_total,
            after_sheet.gross_total,
            after_sheet.withhold_total,
            before_sheet.net_total,
            after_sheet.net_total,
            after_sheet.employee_count,
            log.id,
            reason
        );

        cleanup(&fixture).await;
    }

    // ==================== ② 越权 ====================

    /// 本卡第二条验收：范围之外的行改不动 —— 403，且**库里分文未动、日志一行不多**。
    #[tokio::test]
    async fn override_rejects_out_of_scope_and_keeps_data_intact() {
        let Some(fixture) = setup().await else { return };
        let before = detail_db(fixture.detail_a).await;

        for (label, principal) in [
            ("仅本人（明细 user_id 为空）", fixture.self_principal()),
            ("别的部门", fixture.other_dept_principal()),
        ] {
            let error = salary_sheet_handler::override_detail(
                fixture.operator.clone(),
                &principal,
                fixture.sheet_id,
                fixture.detail_a,
                &fixture.override_req("PERF", "1500", "越权尝试：改别人的绩效"),
            )
            .await
            .expect_err(&format!("{label} 必须被拒"));
            assert_eq!(
                (error.status, error.code),
                (403, error_code::PERMISSION_DENIED),
                "{label} → 403；报文：{}",
                error.msg
            );
            assert!(error.msg.contains(&fixture.sheet_no), "报文点名单号：{}", error.msg);
            assert!(error.msg.contains("张三"), "报文点名员工：{}", error.msg);
            assert!(error.msg.contains("数据范围"), "报文说明原因：{}", error.msg);
        }

        // 库里分文未动
        let after = detail_db(fixture.detail_a).await;
        assert_eq!(after.earning, before.earning);
        assert_eq!(after.gross, before.gross);
        assert_eq!(after.net, before.net);
        let (perf, note, _) = item_of(fixture.detail_a, "PERF").await;
        assert_eq!(perf, money("1200"), "越权请求不许改金额");
        assert!(!note.contains("人工覆盖"), "越权请求不许写说明：{note}");
        assert_eq!(override_log_count(&fixture.sheet_no).await, 0, "越权请求不许留日志");

        cleanup(&fixture).await;
    }

    /// 越权闸放行的对照：同部门范围的主体可以改（否则上面的 403 可能只是「一律拒绝」）。
    #[tokio::test]
    async fn override_allows_principal_whose_scope_covers_the_row() {
        let Some(fixture) = setup().await else { return };
        let res = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("PERF", "1300", "同部门 HR 正常调整绩效"),
        )
        .await
        .expect("同部门范围必须放行");
        assert!(res.changed);
        cleanup(&fixture).await;
    }

    // ==================== ③ 只读态 / 非法值 / 幂等 ====================

    /// 已提交的单子覆盖一律 409，且金额不变（设计 §4.8.3 四态冻结）。
    #[tokio::test]
    async fn override_is_blocked_after_submit() {
        let Some(fixture) = setup().await else { return };
        // 直接把状态推到 SUBMITTED（HR-38 的提交卡还没落地，这里只验只读闸）
        db().exec_sql(
            "UPDATE biz_salary_sheet SET status = ? WHERE id = ?::uuid".to_string(),
            vec![
                Value::Text(STATUS_SUBMITTED.to_string()),
                Value::from(&fixture.sheet_id),
            ],
        )
        .await
        .expect("推状态");
        let before = detail_db(fixture.detail_a).await;

        let error = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("PERF", "1500", "已提交还想改"),
        )
        .await
        .expect_err("已提交必须拒");
        assert_eq!(error.status, 409, "报文：{}", error.msg);
        assert!(error.msg.contains(&fixture.sheet_no), "报文点名单号：{}", error.msg);

        let after = detail_db(fixture.detail_a).await;
        assert_eq!(after.gross, before.gross, "被拒的请求不许改动");
        assert_eq!(override_log_count(&fixture.sheet_no).await, 0);

        // 复位（cleanup 用不到状态，但别把 SUBMITTED 留在库里）
        db().exec_sql(
            "UPDATE biz_salary_sheet SET status = ? WHERE id = ?::uuid".to_string(),
            vec![
                Value::Text(STATUS_CALCULATED.to_string()),
                Value::from(&fixture.sheet_id),
            ],
        )
        .await
        .expect("复位状态");
        cleanup(&fixture).await;
    }

    /// 覆盖后算成负工资 → 409，且**整笔回滚**（明细项金额不留半笔改动）。
    #[tokio::test]
    async fn override_that_makes_gross_negative_rolls_back() {
        let Some(fixture) = setup().await else { return };
        let before = detail_db(fixture.detail_a).await;

        // PERF -20000 → 收入变负 → 应发为负（张三 BASE 8000）
        let error = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("PERF", "-20000", "测试负工资护栏"),
        )
        .await
        .expect_err("负应发必须拒");
        assert_eq!(
            (error.status, error.code),
            (409, error_code::HR_SALARY_RULE_BLOCKED),
            "报文：{}",
            error.msg
        );
        assert!(
            error.msg.contains("负数") || error.msg.contains("非负"),
            "报文：{}",
            error.msg
        );

        let after = detail_db(fixture.detail_a).await;
        assert_eq!(after.earning, before.earning, "整笔回滚，收入不许变");
        assert_eq!(after.gross, before.gross);
        let (perf, note, _) = item_of(fixture.detail_a, "PERF").await;
        assert_eq!(perf, money("1200"), "明细项金额必须回滚");
        assert!(!note.contains("人工覆盖"), "明细项说明必须回滚：{note}");
        assert_eq!(override_log_count(&fixture.sheet_no).await, 0);

        cleanup(&fixture).await;
    }

    /// 金额与库里一致 → `changed = false`、不写日志、不回 4xx（网络重试幂等）。
    #[tokio::test]
    async fn override_with_same_amount_is_idempotent() {
        let Some(fixture) = setup().await else { return };
        let before = detail_db(fixture.detail_a).await;
        let res = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("PERF", "1200", "重试同一个请求"),
        )
        .await
        .expect("金额没变也算成功");
        assert!(!res.changed);
        assert!(res.changes.is_empty());
        assert!(res.log_id.is_none(), "没有改动就不该写日志");
        assert_eq!(override_log_count(&fixture.sheet_no).await, 0);
        let after = detail_db(fixture.detail_a).await;
        assert_eq!(after.gross, before.gross);
        cleanup(&fixture).await;
    }

    /// 不存在的薪资项 → 400 并列出可覆盖项（别让用户猜哪个编码能改）。
    #[tokio::test]
    async fn override_with_unknown_item_code_is_400() {
        let Some(fixture) = setup().await else { return };
        let error = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("BONUS_X", "100", "改一个不存在的项"),
        )
        .await
        .expect_err("不存在的项必须拒");
        assert_eq!(error.status, 400, "报文：{}", error.msg);
        assert!(error.msg.contains("BONUS_X"), "报文点名编码：{}", error.msg);
        assert!(error.msg.contains("PERF"), "报文列可覆盖项：{}", error.msg);
        assert_eq!(override_log_count(&fixture.sheet_no).await, 0);
        cleanup(&fixture).await;
    }

    /// 别的单的明细 id 挂在这张单上 → 404（`detail_id` 必须属于 `sheet_id`）。
    #[tokio::test]
    async fn override_with_mismatched_detail_id_is_404() {
        let Some(fixture) = setup().await else { return };
        let error = salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            Uuid::now_v7(),
            &fixture.override_req("PERF", "1500", "编一个明细 id"),
        )
        .await
        .expect_err("明细不存在必须 404");
        assert_eq!(error.status, 404, "报文：{}", error.msg);
        assert!(error.msg.contains(&fixture.sheet_no), "报文点名单号：{}", error.msg);
        cleanup(&fixture).await;
    }

    /// 单头 / 明细的 7+11 列在覆盖后仍与 `SUM(明细)` 对齐（跨表口径，库层 CHECK 管不到）。
    #[tokio::test]
    async fn sheet_totals_equal_sum_of_details_after_override() {
        let Some(fixture) = setup().await else { return };
        salary_sheet_handler::override_detail(
            fixture.operator.clone(),
            &fixture.dept_principal(),
            fixture.sheet_id,
            fixture.detail_a,
            &fixture.override_req("IIT", "250", "客户补算个税"),
        )
        .await
        .expect("覆盖必须成功");

        let row = db()
            .query_first_sql(
                "SELECT s.gross_total, s.withhold_total, s.net_total, \
                        COALESCE(SUM(d.gross_amount), 0) AS detail_gross, \
                        COALESCE(SUM(d.withhold_amount), 0) AS detail_withhold, \
                        COALESCE(SUM(d.net_amount), 0) AS detail_net \
                   FROM biz_salary_sheet s \
                   LEFT JOIN biz_salary_detail d ON d.sheet_id = s.id \
                  WHERE s.id = ?::uuid GROUP BY s.gross_total, s.withhold_total, s.net_total"
                    .to_string(),
                vec![Value::from(&fixture.sheet_id)],
            )
            .await
            .expect("对拍查询")
            .expect("单头存在");
        let sheet_gross: Decimal = row.get_typed("gross_total").expect("gross");
        let sheet_withhold: Decimal = row.get_typed("withhold_total").expect("withhold");
        let sheet_net: Decimal = row.get_typed("net_total").expect("net");
        let detail_gross: Decimal = row.get_typed("detail_gross").expect("d gross");
        let detail_withhold: Decimal = row.get_typed("detail_withhold").expect("d withhold");
        let detail_net: Decimal = row.get_typed("detail_net").expect("d net");
        assert_eq!(sheet_gross, detail_gross, "单头应发 ≠ SUM(明细)");
        assert_eq!(sheet_withhold, detail_withhold, "单头代扣 ≠ SUM(明细)");
        assert_eq!(sheet_net, detail_net, "单头实发 ≠ SUM(明细)");

        cleanup(&fixture).await;
    }
}
