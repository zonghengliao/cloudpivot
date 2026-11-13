//! 薪资**过账 + 审批回调**的真库用例（HR-39）。
//!
//! 单测（`salary_bridge` 的 `mod tests`）证明的是净额法汇总的数学；只有真库能回答这四件事
//! （= 本卡的三条验收项 + 一条红线）：
//!
//! 1. **两张凭证借贷平衡**：`biz_voucher.total_debit = total_credit`（库层有 CHECK，
//!    但这里还要证明「我生成的每一行加起来正好是它」），且两张凭证的 `source_type`
//!    分别是 `PAYROLL_ACCRUAL` / `PAYROLL_PAYMENT`、`source_no` 都指向这张薪资单。
//! 2. **净额法**：计提凭证借 `5602.10` 的合计 = `SUM(明细.gross_amount)` =
//!    单头 `gross_total`（= Σ EARNING − Σ PRE_DEDUCT），**且没有任何负数行**
//!    （单边 CHECK 要求每行 > 0，写负数会直接 23514）。
//! 3. **发放凭证的倒挤**：借 `2211.01` = 应发合计；贷 `1002` = 实发合计；
//!    贷 `2241.03/04` = 社保 / 公积金个人合计；个税为 0 → **一行都不落**。
//! 4. **不留中间态**（`AGENTS.md` 约定 11）：状态不对 / 考勤未锁定 / 重复过账一律拒绝，
//!    且**一张凭证都不留**（不是「凭证生成了、单据没动」）。
//!
//! 另外验审批回调的三条路径（通过 / 驳回 / 中间态）与「**通过不出凭证**」（设计 §4.3）。
//!
//! 不设库地址时全部 SKIP（**不假绿**）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr39_drill \
//!   cargo test --lib business::hr::salary_bridge_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! * 员工工号前缀 `HR39A-<纳秒>` / `HR39B-<纳秒>`，薪资单 `remark = 'HR39 真库用例'`；
//! * 薪资期间固定 **`2028-0X`**（HR-34 `2026-11` / HR-36 `2026-12` / HR-37 `2027-01`：
//!   各卡的 sweep 按期间删单，同期间会互相清掉夹具）；
//! * 不建薪资项：直接用 `V0396` 的 17 个种子项（种子项的借贷科目正是设计 §4.6 的口径）；
//! * 过账要求考勤期间 `LOCKED`：夹具先按 `DRAFT` 建单（试算允许），过账前再 `UPDATE` 成
//!   `LOCKED` —— 每条用例按自己需要选，不共用状态；
//! * `--test-threads=1`：引擎取数是全量的，且各用例写同一批种子科目。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use nik::prelude::rust_decimal::Decimal;

    use crate::business::hr::fixture_hygiene as hygiene;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_bridge::{self, SOURCE_ACCRUAL, SOURCE_PAYMENT};
    use crate::business::hr::salary_sheet_dto::{
        STATUS_APPROVED, STATUS_CALCULATED, STATUS_POSTED, STATUS_REVERSED, STATUS_SUBMITTED, SalaryManualReq,
        SalarySheetCalcReq,
    };
    use crate::business::hr::salary_sheet_handler;
    use crate::business::hr::salary_sheet_model as model;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use tokio::sync::OnceCell;

    /// 夹具员工工号前缀（清场按它精确删）。
    const PREFIX: &str = "HR39";
    /// 薪资单备注（清场按它精确删 —— 比按期间删更窄，不会误伤邻居卡）。
    const REMARK: &str = "HR39 真库用例";

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
                        println!("HR39 真库用例已连接：database={database}");
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

    async fn first_user() -> Option<Uuid> {
        db().query_first_sql(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
            vec![],
        )
        .await
        .expect("查操作人")
        .map(|row| row.get_typed::<Uuid>("id").expect("用户 id"))
    }

    /// 清场 = 统一清空薪资引擎的输入集（员工 / 考勤 / 标准 / 工资单 / 非种子薪资项）
    /// + 本模块自己的凭证残留；返回引擎互斥锁的 guard（本用例期间独占输入集）。
    async fn sweep() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = hygiene::exclusive().await;

        // 先清凭证（按 source_no 指向的单号）：biz_voucher 没有指向薪资单的外键
        let sheet_nos: Vec<String> = db()
            .query_sql(
                format!(
                    "SELECT sheet_no FROM biz_salary_sheet WHERE remark = '{REMARK}' AND org_id = '{}'",
                    org()
                ),
                vec![],
            )
            .await
            .expect("查旧夹具单号")
            .into_iter()
            .filter_map(|row| row.get_typed::<String>("sheet_no").ok())
            .collect();
        for sheet_no in &sheet_nos {
            for sql in [
                format!(
                    "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                       (SELECT id FROM biz_voucher WHERE source_no = '{sheet_no}')"
                ),
                format!("DELETE FROM biz_voucher WHERE source_no = '{sheet_no}'"),
                format!("DELETE FROM biz_status_log WHERE entity_no = '{sheet_no}'"),
            ] {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }
        let statements: [String; 5] = [
            format!(
                "DELETE FROM biz_salary_sheet WHERE remark = '{REMARK}' AND org_id = '{}'",
                org()
            ),
            format!("DELETE FROM biz_attendance_record WHERE employee_no LIKE '{PREFIX}%'"),
            format!(
                "DELETE FROM biz_attendance_period WHERE period_code LIKE '2028-%' AND org_id = '{}'",
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
        guard
    }

    /// 一个夹具：2 名员工（同一部门）+ 张三的标准 / 考勤 + 一张已算出的薪资单。
    #[allow(dead_code)] // `period` 只在个别用例的报文断言里用到
    struct Fixture {
        period: String,
        emp_a: Uuid,
        emp_b: Uuid,
        attendance_period: Uuid,
        sheet_id: Uuid,
        sheet_no: String,
        dept: Uuid,
        operator: Operator,
        /// 引擎输入集的互斥锁 guard（用例结束才释放；字段只为持有生命周期）。
        _engine: tokio::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
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
                period_code: Some(self.period.clone()),
                attendance_period_id: None,
                dry_run: Some(false),
                remark: Some(REMARK.to_string()),
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

        /// 把单据推到 `APPROVED`（模拟「审批已通过」，过账闸要求这个状态）。
        async fn approve(&self) {
            db().exec_sql(
                "UPDATE biz_salary_sheet SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
                vec![Value::from(&self.sheet_id)],
            )
            .await
            .expect("推到 APPROVED");
        }

        /// 把考勤期间推到 `LOCKED`（设计 §4.8.3：过账要求考勤已定稿）。
        async fn lock_attendance(&self) {
            db().exec_sql(
                "UPDATE biz_attendance_period SET status = 'LOCKED' WHERE id = ?::uuid".to_string(),
                vec![Value::from(&self.attendance_period)],
            )
            .await
            .expect("锁定考勤期间");
        }
    }

    /// 造一个夹具（`period` 由调用方给，按用例错开）。
    ///
    /// 张三：BASE 8000 + PERF 1200 收入，2 天无薪假 → ATT_DEDUCT = ROUND(8000/21.75*2, 2) = 735.63；
    /// 代扣 社保个人 840 / 公积金个人 960；单位社保 1680 / 单位公积金 960。
    /// 李四：不配标准也没有考勤 → 全 0（合法行，不产生任何凭证行）。
    async fn setup(period: &str) -> Option<Fixture> {
        if !ensure_db().await {
            return None;
        }
        let engine = sweep().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let period_id = Uuid::now_v7();
        let (start, end) = (format!("{period}-01"), format!("{period}-28"));

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

        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')"
                .to_string(),
            vec![
                Value::from(&period_id),
                Value::from(&org()),
                Value::Text(period.to_string()),
                Value::Text(start),
                Value::Text(end),
            ],
        )
        .await
        .expect("插考勤期间");

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
                Value::Text("0".to_string()),
            ],
        )
        .await
        .expect("插考勤记录");

        let operator = Operator::named(first_user().await, "hr39-test");
        let mut fixture = Fixture {
            period: period.to_string(),
            emp_a,
            emp_b,
            attendance_period: period_id,
            sheet_id: Uuid::nil(),
            sheet_no: String::new(),
            dept,
            operator,
            _engine: engine,
        };
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req())
            .await
            .expect("建单必须成功");
        let sheet = created.sheet.clone().expect("建单必须回单头");
        assert_eq!(sheet.status, STATUS_CALCULATED);
        fixture.sheet_id = Uuid::parse_str(&sheet.id).expect("单头 id");
        fixture.sheet_no = sheet.sheet_no.clone();
        Some(fixture)
    }

    async fn cleanup(fixture: &Fixture) {
        for (sql, params) in [
            // 凭证先删（分录 → 凭证）
            (
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                   (SELECT id FROM biz_voucher WHERE source_no = ?)"
                    .to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_voucher WHERE source_no = ?".to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
            // 审批实例（提交用例会建）：先任务再实例
            (
                "DELETE FROM biz_flow_task WHERE instance_id IN \
                   (SELECT id FROM biz_flow_instance WHERE biz_no = ?)"
                    .to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_flow_instance WHERE biz_no = ?".to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&fixture.sheet_id)],
            ),
            (
                "DELETE FROM biz_attendance_record WHERE employee_no LIKE ?".to_string(),
                vec![Value::Text(format!("{PREFIX}%"))],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                vec![Value::from(&fixture.attendance_period)],
            ),
            (
                "DELETE FROM biz_employee_salary WHERE employee_id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM biz_status_log WHERE entity_no = ?".to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            ),
        ] {
            let _ = db().exec_sql(sql, params).await;
        }
    }

    // ---- 断言素材 ----

    /// 一张凭证的分录（科目编码, 借, 贷, 部门）。
    struct Entry {
        code: String,
        debit: Decimal,
        credit: Decimal,
        dept: Option<Uuid>,
    }

    async fn entries_of(voucher_id: Uuid) -> Vec<Entry> {
        db().query_sql(
            "SELECT a.account_code, e.debit_amount, e.credit_amount, e.aux_dept_id \
               FROM biz_voucher_entry e JOIN biz_account a ON a.id = e.account_id \
              WHERE e.voucher_id = ?::uuid ORDER BY e.line_no"
                .to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await
        .expect("查分录")
        .into_iter()
        .map(|row| Entry {
            code: row.get_typed("account_code").expect("科目编码"),
            debit: row.get_typed("debit_amount").expect("借方"),
            credit: row.get_typed("credit_amount").expect("贷方"),
            dept: row.get_typed("aux_dept_id").expect("部门"),
        })
        .collect()
    }

    /// 某科目的借方合计 / 贷方合计。
    fn sum_of(entries: &[Entry], code: &str, side: &str) -> Decimal {
        entries
            .iter()
            .filter(|entry| entry.code == code)
            .map(|entry| if side == "debit" { entry.debit } else { entry.credit })
            .sum()
    }

    /// 凭证头：`(source_type, total_debit, total_credit, voucher_no, period)`.
    async fn voucher_head(id: Uuid) -> (String, Decimal, Decimal, String, String) {
        let row = db()
            .query_first_sql(
                "SELECT source_type, total_debit, total_credit, voucher_no, period \
                   FROM biz_voucher WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查凭证头")
            .expect("凭证必须存在");
        (
            row.get_typed("source_type").expect("来源类型"),
            row.get_typed("total_debit").expect("借方合计"),
            row.get_typed("total_credit").expect("贷方合计"),
            row.get_typed("voucher_no").expect("凭证号"),
            row.get_typed("period").expect("期间"),
        )
    }

    async fn sheet_vouchers(sheet_no: &str) -> Vec<(String, Uuid)> {
        db().query_sql(
            "SELECT source_type, id FROM biz_voucher WHERE source_no = ? ORDER BY created_at".to_string(),
            vec![Value::Text(sheet_no.to_string())],
        )
        .await
        .expect("查薪资单的凭证")
        .into_iter()
        .map(|row| {
            (
                row.get_typed("source_type").expect("来源"),
                row.get_typed("id").expect("凭证 id"),
            )
        })
        .collect()
    }

    // ==================== ① 两张凭证：平衡 + 净额法 + 倒挤 ====================

    /// 过账生成两张凭证；逐项对账（本卡三条验收项的主用例）。
    #[tokio::test]
    async fn post_creates_two_balanced_vouchers_with_net_method() {
        let Some(fixture) = setup("2028-01").await else {
            return;
        };
        fixture.lock_attendance().await;
        fixture.approve().await;

        let sheet_before = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let card = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("过账必须成功");

        // ---- ① 单据：POSTED + 两张凭证 id/no ----
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_POSTED);
        let accrual_id = sheet.accrual_voucher_id.expect("计提凭证 id");
        let payment_id = sheet.payment_voucher_id.expect("发放凭证 id");
        assert_ne!(accrual_id, payment_id);
        assert!(sheet.accrual_voucher_no.is_some(), "计提凭证号必须回写");
        assert!(sheet.payment_voucher_no.is_some(), "发放凭证号必须回写");
        assert!(sheet.posted_at.is_some(), "posted_at 必须写");
        assert_eq!(card.status, STATUS_POSTED);
        assert_eq!(card.accrual_voucher_no, sheet.accrual_voucher_no);

        // ---- ② 两张凭证头：来源类型 / 借贷平衡 / 期间 ----
        let (accrual_source, accrual_debit, accrual_credit, _, accrual_period) = voucher_head(accrual_id).await;
        assert_eq!(accrual_source, SOURCE_ACCRUAL);
        assert_eq!(accrual_debit, accrual_credit, "计提凭证必须借贷平衡");
        assert_eq!(accrual_period, sheet_before.period_code, "计提凭证期间 = 薪资期间");

        let (payment_source, payment_debit, payment_credit, _, _) = voucher_head(payment_id).await;
        assert_eq!(payment_source, SOURCE_PAYMENT);
        assert_eq!(payment_debit, payment_credit, "发放凭证必须借贷平衡");

        // ---- ③ 计提凭证：净额法（借 5602.10 = 应发合计 = 收入 − 应发前扣减）----
        let accrual = entries_of(accrual_id).await;
        assert_eq!(
            sum_of(&accrual, "5602.10", "debit"),
            sheet_before.gross_total,
            "计提凭证借 5602.10 必须等于应发合计（净额法：考勤扣款直接抵减）"
        );
        assert_eq!(
            sum_of(&accrual, "2211.01", "credit"),
            sheet_before.gross_total,
            "贷 2211.01 = 应发合计"
        );
        assert_eq!(sum_of(&accrual, "5602.11", "debit"), sheet_before.si_company_total);
        assert_eq!(sum_of(&accrual, "5602.12", "debit"), sheet_before.hf_company_total);
        assert_eq!(sum_of(&accrual, "2211.02", "credit"), sheet_before.si_company_total);
        assert_eq!(sum_of(&accrual, "2211.03", "credit"), sheet_before.hf_company_total);
        // 每行金额 > 0（单边 CHECK 的硬要求 —— 净额法就是为了它）
        for entry in &accrual {
            assert!(
                entry.debit > Decimal::ZERO || entry.credit > Decimal::ZERO,
                "分录不许有 0 / 负数行：{}/{}",
                entry.code,
                entry.debit - entry.credit
            );
            assert!(
                entry.debit.is_zero() || entry.credit.is_zero(),
                "一行只能一个方向：{}",
                entry.code
            );
        }
        // 部门辅助核算：5602.1x / 2211.0x 必须带部门（HR-02 种的 DEPT 维度）
        for entry in &accrual {
            if entry.code.starts_with("5602.1") || entry.code.starts_with("2211.0") {
                assert_eq!(entry.dept, Some(fixture.dept), "科目 {} 必须带部门", entry.code);
            }
        }
        // 代扣项**不进**计提凭证
        for code in ["2241.03", "2241.04", "2221.03"] {
            assert!(
                accrual.iter().all(|entry| entry.code != code),
                "代扣科目 {code} 不该出现在计提凭证里"
            );
        }

        // ---- ④ 发放凭证：借 2211.01 = 应发；贷 1002 = 实发；代扣按项落 ----
        let payment = entries_of(payment_id).await;
        assert_eq!(
            sum_of(&payment, "2211.01", "debit"),
            sheet_before.gross_total,
            "借 2211.01 = 应发合计"
        );
        assert_eq!(
            sum_of(&payment, "1002", "credit"),
            sheet_before.net_total,
            "贷 1002 = 实发合计（倒挤）"
        );
        assert_eq!(sum_of(&payment, "2241.03", "credit"), money("840"));
        assert_eq!(sum_of(&payment, "2241.04", "credit"), money("960"));
        // 个税 0 → 一行都不落
        assert!(
            payment.iter().all(|entry| entry.code != "2221.03"),
            "个税为 0 时不许落行（单边 CHECK）"
        );
        // 代扣科目没有 DEPT 维度 → 不带部门
        for entry in &payment {
            if entry.code.starts_with("2241.0") {
                assert_eq!(entry.dept, None, "科目 {} 没有 DEPT 维度，不许带部门", entry.code);
            }
        }

        // ---- ⑤ 干净收尾：库里只有这 2 张凭证 ----
        assert_eq!(sheet_vouchers(&fixture.sheet_no).await.len(), 2);
        cleanup(&fixture).await;
    }

    // ==================== ② 状态闸 / 幂等 / 考勤闸 ====================

    /// 重复过账 → 409 且**不新增凭证**（凭证数仍是 2）。
    #[tokio::test]
    async fn post_is_rejected_twice_and_leaves_no_extra_vouchers() {
        let Some(fixture) = setup("2028-02").await else {
            return;
        };
        fixture.lock_attendance().await;
        fixture.approve().await;
        salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("第一次过账必须成功");
        let error = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect_err("重复过账必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains("已经过账"), "报文要说明已经过账：{}", error.msg);
        assert_eq!(sheet_vouchers(&fixture.sheet_no).await.len(), 2, "重复过账不许新增凭证");
        cleanup(&fixture).await;
    }

    /// 未审批通过（CALCULATED / SUBMITTED）→ 409 且**一张凭证都不留**。
    #[tokio::test]
    async fn post_requires_approved_status_and_leaves_nothing() {
        let Some(fixture) = setup("2028-03").await else {
            return;
        };
        fixture.lock_attendance().await;
        // 不推到 APPROVED：直接过账
        let error = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect_err("未审批通过不许过账");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("APPROVED"), "报文要点名目标状态：{}", error.msg);
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_CALCULATED, "失败的过账不许改状态");
        assert!(sheet.accrual_voucher_id.is_none());
        assert!(
            sheet_vouchers(&fixture.sheet_no).await.is_empty(),
            "失败的过账不许留半张凭证"
        );
        cleanup(&fixture).await;
    }

    /// 考勤期间未锁定 → 409 点名期间，且不留凭证（设计 §4.8.3）。
    #[tokio::test]
    async fn post_requires_locked_attendance_period() {
        let Some(fixture) = setup("2028-04").await else {
            return;
        };
        fixture.approve().await;
        // 考勤期间保持 DRAFT
        let error = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect_err("考勤未锁定不许过账");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("还没锁定"), "报文要点名考勤未锁定：{}", error.msg);
        assert!(error.msg.contains(&fixture.period), "报文要点名期间：{}", error.msg);
        assert!(sheet_vouchers(&fixture.sheet_no).await.is_empty());
        // 锁定之后就能过账（同一张单，不需要改库救命）
        fixture.lock_attendance().await;
        let card = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("锁定考勤后必须能过账");
        assert_eq!(card.status, STATUS_POSTED);
        cleanup(&fixture).await;
    }

    // ==================== ③ 审批回调 ====================

    /// 审批通过 → `SUBMITTED → APPROVED`，且**不出凭证**（设计 §4.3）；重复回调幂等。
    #[tokio::test]
    async fn approval_callback_approves_without_vouchers_and_is_idempotent() {
        let Some(fixture) = setup("2028-05").await else {
            return;
        };
        if fixture.operator.id.is_none() {
            println!("SKIP: 库里没有 sys_user，无法提交审批");
            cleanup(&fixture).await;
            return;
        }
        fixture.lock_attendance().await;
        let submitted = salary_sheet_handler::submit_sheet(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("提交审批必须成功");
        assert_eq!(submitted.status, STATUS_SUBMITTED);
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let instance_id = sheet.approval_instance_id.expect("提交必须写实例 id");
        assert_eq!(sheet.status, STATUS_SUBMITTED);

        // 中间态（PENDING）不许动单据
        assert!(
            salary_bridge::on_salary_settled(Some(instance_id), "PENDING")
                .await
                .is_none()
        );
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_SUBMITTED, "PENDING 不许推进单据");

        // 通过
        let settled = salary_bridge::on_salary_settled(Some(instance_id), "APPROVED").await;
        assert_eq!(settled, Some(fixture.sheet_id));
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_APPROVED);
        assert!(
            sheet_vouchers(&fixture.sheet_no).await.is_empty(),
            "审批通过**不出凭证**（设计 §4.3）：凭证要在过账时才生成"
        );

        // 重复回调 → 幂等跳过（不报错、不改状态）
        assert!(
            salary_bridge::on_salary_settled(Some(instance_id), "APPROVED")
                .await
                .is_none()
        );
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_APPROVED);

        // 通过之后可以过账（闭环）
        let card = salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("审批通过后必须能过账");
        assert_eq!(card.status, STATUS_POSTED);
        cleanup(&fixture).await;
    }

    /// 驳回 → `SUBMITTED → CALCULATED`（退回重算，可改可重提）。
    #[tokio::test]
    async fn rejection_callback_returns_sheet_to_calculated() {
        let Some(fixture) = setup("2028-06").await else {
            return;
        };
        if fixture.operator.id.is_none() {
            println!("SKIP: 库里没有 sys_user，无法提交审批");
            cleanup(&fixture).await;
            return;
        }
        fixture.lock_attendance().await;
        salary_sheet_handler::submit_sheet(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("提交审批必须成功");
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let instance_id = sheet.approval_instance_id.expect("实例 id");

        let settled = salary_bridge::on_salary_settled(Some(instance_id), "REJECTED").await;
        assert_eq!(settled, Some(fixture.sheet_id));
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(
            sheet.status, STATUS_CALCULATED,
            "驳回必须退回 CALCULATED（不是关闭、不是草稿）"
        );
        assert!(sheet_vouchers(&fixture.sheet_no).await.is_empty());
        cleanup(&fixture).await;
    }

    /// 陈旧回调：单据已改挂新实例时，旧实例的结论不予采纳。
    #[tokio::test]
    async fn stale_instance_callback_is_ignored() {
        let Some(fixture) = setup("2028-07").await else {
            return;
        };
        if fixture.operator.id.is_none() {
            println!("SKIP: 库里没有 sys_user，无法提交审批");
            cleanup(&fixture).await;
            return;
        }
        salary_sheet_handler::submit_sheet(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("提交审批必须成功");
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let old_instance = sheet.approval_instance_id.expect("实例 id");
        // 模拟「撤回后重提」：单上改挂另一个实例
        let new_instance = Uuid::now_v7();
        db().exec_sql(
            "UPDATE biz_salary_sheet SET approval_instance_id = ?::uuid WHERE id = ?::uuid".to_string(),
            vec![Value::from(&new_instance), Value::from(&fixture.sheet_id)],
        )
        .await
        .expect("改挂实例");
        // 旧实例（old_instance）的回调迟到 → 不采纳
        assert!(
            salary_bridge::on_salary_settled(Some(old_instance), "APPROVED")
                .await
                .is_none(),
            "旧实例的结论不许推进单据"
        );
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_SUBMITTED, "陈旧回调不许动状态");
        assert_eq!(sheet.approval_instance_id, Some(new_instance));
        cleanup(&fixture).await;
    }

    // ==================== ⑤ 红冲（HR-40）：两张红字凭证 + 状态机 + 原因必填 ====================

    /// 把夹具推到 `POSTED`（红冲的前置状态）。
    async fn post_for_reverse(fixture: &Fixture) {
        fixture.lock_attendance().await;
        fixture.approve().await;
        salary_bridge::post_salary(fixture.operator.clone(), fixture.sheet_id)
            .await
            .expect("过账必须成功（红冲的前置）");
    }

    /// 一张凭证的 `(is_red, reverse_of, reversed_by, voucher_no, source_type, source_no, remark, 借合计, 贷合计)`。
    struct VoucherMeta {
        is_red: i16,
        reverse_of: Option<Uuid>,
        reversed_by: Option<Uuid>,
        voucher_no: String,
        source_type: String,
        source_no: Option<String>,
        remark: Option<String>,
        debit: Decimal,
        credit: Decimal,
    }

    async fn voucher_meta(id: Uuid) -> VoucherMeta {
        let row = db()
            .query_first_sql(
                "SELECT is_red, reverse_of, reversed_by, voucher_no, source_type, source_no, remark, \
                        total_debit, total_credit \
                   FROM biz_voucher WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查凭证元信息")
            .expect("凭证必须存在");
        VoucherMeta {
            is_red: row.get_typed("is_red").expect("is_red"),
            reverse_of: row.get_typed("reverse_of").expect("reverse_of"),
            reversed_by: row.get_typed("reversed_by").expect("reversed_by"),
            voucher_no: row.get_typed("voucher_no").expect("凭证号"),
            source_type: row.get_typed("source_type").expect("来源类型"),
            source_no: row.get_typed("source_no").expect("来源单号"),
            remark: row.get_typed("remark").expect("备注"),
            debit: row.get_typed("total_debit").expect("借合计"),
            credit: row.get_typed("total_credit").expect("贷合计"),
        }
    }

    /// 红字凭证的 id（按 `reverse_of` 反查）。
    async fn red_of(original: Uuid) -> Option<Uuid> {
        db().query_first_sql(
            "SELECT id FROM biz_voucher WHERE reverse_of = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&original)],
        )
        .await
        .expect("查红字凭证")
        .map(|row| row.get_typed("id").expect("红字 id"))
    }

    /// 验收①②：红冲**两张都冲**（计提 + 发放各一张红字），借贷互换、金额恒正、`PZR` 号段、
    /// 原凭证 `reversed_by` 回写、单据 `REVERSED` + `reversal_voucher_id` 记第一张 + 原因落库。
    #[tokio::test]
    async fn reverse_creates_two_red_vouchers_for_accrual_and_payment() {
        let Some(fixture) = setup("2028-08").await else {
            return;
        };
        post_for_reverse(&fixture).await;
        let posted = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let accrual_id = posted.accrual_voucher_id.expect("过账后必须有计提凭证");
        let payment_id = posted.payment_voucher_id.expect("过账后必须有发放凭证");
        let reason = "11 月绩效口径报错，财务确认整单重发";

        let card = salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, reason)
            .await
            .expect("红冲必须成功");
        assert_eq!(card.status, STATUS_REVERSED, "红冲后单据是 REVERSED");
        assert_eq!(card.status_label, "已红冲");

        // ---- 单据侧：状态 + 指针 + 原因 + 时间戳 ----
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_REVERSED);
        assert_eq!(sheet.reverse_reason.as_deref(), Some(reason), "原因必须落库");
        assert!(sheet.reversed_at.is_some(), "红冲时间必须写");
        let first_red = sheet.reversal_voucher_id.expect("reversal_voucher_id 必须写");

        // ---- 两张红字凭证：都冲了，且各自指向自己的原凭证 ----
        let accrual_red = red_of(accrual_id).await.expect("计提凭证的红字凭证必须存在");
        let payment_red = red_of(payment_id).await.expect("发放凭证的红字凭证必须存在");
        assert_eq!(first_red, accrual_red, "reversal_voucher_id 记第一张（计提的红字）");
        assert_ne!(accrual_red, payment_red, "两张原凭证各冲一张，不能只冲一张");

        for (label, original_id, red_id, expect_source) in [
            ("计提", accrual_id, accrual_red, SOURCE_ACCRUAL),
            ("发放", payment_id, payment_red, SOURCE_PAYMENT),
        ] {
            let original = voucher_meta(original_id).await;
            let red = voucher_meta(red_id).await;
            assert_eq!(original.is_red, 0, "{label}原凭证是蓝字");
            assert_eq!(red.is_red, 1, "{label}红字凭证 is_red = 1");
            assert_eq!(red.reverse_of, Some(original_id), "{label}红字 reverse_of 指向原凭证");
            assert_eq!(
                original.reversed_by,
                Some(red_id),
                "{label}原凭证必须回写 reversed_by（不是把原凭证删掉）"
            );
            assert_eq!(
                red.voucher_no.chars().take(4).collect::<String>(),
                "PZR-",
                "{label}红字凭证走独立号段 PZR-（实际：{}）",
                red.voucher_no
            );
            assert_eq!(
                red.source_type, expect_source,
                "{label}红字继承来源类型（巡检 A8 要查）"
            );
            assert_eq!(
                red.source_no.as_deref(),
                Some(fixture.sheet_no.as_str()),
                "{label}红字凭证仍能按薪资单号查到"
            );
            assert_eq!(red.remark.as_deref(), Some(reason), "{label}红字备注写的是红冲原因");
            // 金额等额（借贷合计不变），借贷平衡
            assert_eq!(red.debit, original.debit, "{label}红字借方合计 = 原凭证");
            assert_eq!(red.credit, original.credit, "{label}红字贷方合计 = 原凭证");
            assert_eq!(red.debit, red.credit, "{label}红字凭证必须平衡");

            // ---- 分录侧：逐行借贷互换、金额恒正 ----
            let originals = entries_of(original_id).await;
            let reds = entries_of(red_id).await;
            assert_eq!(reds.len(), originals.len(), "{label}红字行数 = 原凭证行数");
            for entry in &reds {
                assert!(
                    entry.debit > Decimal::ZERO || entry.credit > Decimal::ZERO,
                    "{label}红字每行必须恒正（单边 CHECK 要求 > 0）：{}/{}",
                    entry.code,
                    entry.debit
                );
            }
            for entry in &originals {
                let swapped_debit = sum_of(&reds, &entry.code, "credit");
                let swapped_credit = sum_of(&reds, &entry.code, "debit");
                let original_debit = sum_of(&originals, &entry.code, "debit");
                let original_credit = sum_of(&originals, &entry.code, "credit");
                assert_eq!(swapped_debit, original_debit, "{label} {} 的借→贷互换", entry.code);
                assert_eq!(swapped_credit, original_credit, "{label} {} 的贷→借互换", entry.code);
            }
        }

        // ---- 一张单 4 张凭证（2 蓝 + 2 红），没有多出来的 ----
        let all = sheet_vouchers(&fixture.sheet_no).await;
        assert_eq!(all.len(), 4, "计提/发放 + 各自的红字 = 4 张，实际：{all:?}");

        // ---- 状态日志：POSTED → REVERSED + 原因（审计轨迹） ----
        let log = db()
            .query_first_sql(
                "SELECT from_status, to_status, reason FROM biz_status_log \
                  WHERE entity_no = ? AND from_status = 'POSTED' AND to_status = 'REVERSED'"
                    .to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            )
            .await
            .expect("查状态日志")
            .expect("必须有一条 POSTED → REVERSED 的状态日志");
        let log_reason: Option<String> = log.get_typed("reason").expect("日志原因");
        assert!(
            log_reason.as_deref().unwrap_or_default().contains(reason),
            "状态日志必须带原因：{log_reason:?}"
        );

        cleanup(&fixture).await;
    }

    /// 验收②（反向）：只有 `POSTED` 能红冲；红冲过的**不能再冲第二次**，且不留多余凭证。
    #[tokio::test]
    async fn reverse_requires_posted_and_is_rejected_twice() {
        let Some(fixture) = setup("2028-09").await else {
            return;
        };
        // ① CALCULATED 阶段红冲 → 409，且不留红字凭证
        let error = salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, "还没过账不能红冲")
            .await
            .expect_err("未过账不许红冲");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains("POSTED"), "报文要点名目标状态：{}", error.msg);
        assert!(sheet_vouchers(&fixture.sheet_no).await.is_empty(), "不许留凭证");

        // ② 过账后红冲成功
        post_for_reverse(&fixture).await;
        salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, "口径报错整单重发")
            .await
            .expect("过账后红冲必须成功");
        assert_eq!(sheet_vouchers(&fixture.sheet_no).await.len(), 4);

        // ③ 再冲一次 → 409（终态），且凭证数不变（不会多出两张红字）
        let error = salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, "再冲一次试试")
            .await
            .expect_err("已红冲的单不能再红冲");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("REVERSED") || error.msg.contains("已经红冲"),
            "报文要说明已红冲：{}",
            error.msg
        );
        assert_eq!(sheet_vouchers(&fixture.sheet_no).await.len(), 4, "重复红冲不许新增凭证");
        cleanup(&fixture).await;
    }

    /// 验收②：**原因必填** —— 空白原因 400，且账上一分不动（单据仍 `POSTED`、无红字凭证）。
    #[tokio::test]
    async fn reverse_without_reason_is_rejected_and_leaves_ledger_untouched() {
        let Some(fixture) = setup("2028-10").await else {
            return;
        };
        post_for_reverse(&fixture).await;
        for blank in ["", "   ", "\t\n"] {
            let error = salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, blank)
                .await
                .expect_err("空白原因必须拒绝");
            assert_eq!(error.status, 400, "缺原因要 400");
            assert_eq!(error.code, error_code::PARAM_INVALID);
            assert!(error.msg.contains("reason 必填"), "报文：{}", error.msg);
        }
        let sheet = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(sheet.status, STATUS_POSTED, "失败的 红冲不许改状态");
        assert!(sheet.reversal_voucher_id.is_none(), "失败的 红冲不许写指针");
        assert!(sheet.reverse_reason.is_none());
        let reds = db()
            .query_sql(
                "SELECT id FROM biz_voucher WHERE source_no = ? AND is_red = 1".to_string(),
                vec![Value::Text(fixture.sheet_no.clone())],
            )
            .await
            .expect("查红字凭证");
        assert!(reds.is_empty(), "失败的 红冲一张红字凭证都不许留");
        cleanup(&fixture).await;
    }

    /// 验收③：**红冲后可重算** —— 同期间可以重新建单（红冲单让出期间位），
    /// 新单金额口径与红冲前一致，红冲单本体（红字凭证 + 原因）原样留档。
    #[tokio::test]
    async fn reversed_sheet_frees_the_period_for_a_new_sheet() {
        let Some(fixture) = setup("2028-11").await else {
            return;
        };
        post_for_reverse(&fixture).await;
        let posted = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        let gross_before = posted.gross_total;
        salary_bridge::reverse_salary(fixture.operator.clone(), fixture.sheet_id, "整单重发：重算一版")
            .await
            .expect("红冲必须成功");

        // 唯一键/前置检查都必须放行（否则这条用例会拿到「同期间已存在」的 409）
        let leftover = crate::business::hr::salary_compute::existing_sheet_no(db(), &fixture.period)
            .await
            .expect("查期间占位");
        assert!(leftover.is_none(), "红冲单不该再占期间位（实际：{leftover:?}）");

        // 同期间重新建单：必须成功，且是一张**新单**（旧单不动）
        let again = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req())
            .await
            .expect("红冲后同期间必须能重新建单（否则客户只能找研发改库）");
        let new_sheet = again.sheet.clone().expect("建单必须回单头");
        let new_id = Uuid::parse_str(&new_sheet.id).expect("新单 id");
        assert_ne!(new_id, fixture.sheet_id, "必须是新单，不是原单复活");
        assert_ne!(new_sheet.sheet_no, fixture.sheet_no, "新单要新单号");
        assert_eq!(new_sheet.status, STATUS_CALCULATED);
        let new_head = model::find_sheet(new_id).await.expect("读新单").expect("新单在");
        assert_eq!(new_head.gross_total, gross_before, "同一批考勤 + 标准 → 应发口径一致");

        // 旧单仍是 REVERSED，红字凭证与原因都还在（审计要比稿）
        let old = model::find_sheet(fixture.sheet_id).await.expect("读单").expect("单在");
        assert_eq!(old.status, STATUS_REVERSED);
        assert_eq!(old.reverse_reason.as_deref(), Some("整单重发：重算一版"));
        assert_eq!(sheet_vouchers(&fixture.sheet_no).await.len(), 4);

        // 新单也能正常走完全程（重算后的这张才是要发的）
        db().exec_sql(
            "UPDATE biz_salary_sheet SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&new_id)],
        )
        .await
        .expect("新单推到 APPROVED");
        salary_bridge::post_salary(fixture.operator.clone(), new_id)
            .await
            .expect("新单过账必须成功（红冲重算的闭环）");

        // 清场：新单的凭证 / 日志 / 单头（旧单由 cleanup 收拾）
        for (sql, params) in [
            (
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                   (SELECT id FROM biz_voucher WHERE source_no = ?)"
                    .to_string(),
                vec![Value::Text(new_sheet.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_voucher WHERE source_no = ?".to_string(),
                vec![Value::Text(new_sheet.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_status_log WHERE entity_no = ?".to_string(),
                vec![Value::Text(new_sheet.sheet_no.clone())],
            ),
            (
                "DELETE FROM biz_salary_detail_item WHERE detail_id IN \
                   (SELECT id FROM biz_salary_detail WHERE sheet_id = ?::uuid)"
                    .to_string(),
                vec![Value::from(&new_id)],
            ),
            (
                "DELETE FROM biz_salary_detail WHERE sheet_id = ?::uuid".to_string(),
                vec![Value::from(&new_id)],
            ),
            (
                "DELETE FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&new_id)],
            ),
        ] {
            let _ = db().exec_sql(sql, params).await;
        }
        cleanup(&fixture).await;
    }
}
