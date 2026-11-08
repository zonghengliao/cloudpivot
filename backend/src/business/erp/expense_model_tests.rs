//! `expense_model` / `expense_bridge` 的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时这些用例 **SKIP**（与本仓其它 ERP 用例同口径）。
//! 交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin3 \
//!   cargo test --lib business::erp::expense -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面「单测 ≥12 条」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `header_and_items_are_consistent` | 明细合计 = 单头（L1） |
//! | 2 | `header_tax_sum_constraint` | 单头价税恒等（库 CHECK） |
//! | 3 | `item_tax_sum_constraint` | 明细价税恒等（库 CHECK） |
//! | 4 | `item_positive_amount_constraint` | 明细金额必须 > 0（库 CHECK） |
//! | 5 | `update_draft_only_from_draft` | 改草稿的 CAS（审批中改不动） |
//! | 6 | `apply_partial_payment` | 部分付款 → PARTIAL |
//! | 7 | `apply_full_payment` | 付清 → PAID |
//! | 8 | `overpay_is_rejected_by_database` | 超额付款撞 CHECK / 0 行 |
//! | 9 | `type_usage_count` | 被引用的类型删不掉（409 的判据） |
//! | 10 | `check_totals_detects_mismatch` | 跨表恒等式（66042 的判据） |
//! | 11 | `approval_voucher_is_balanced_with_tax_line` | 入账凭证：借费用+借税 / 贷 2241.01 |
//! | 12 | `payment_voucher_two_lines` | 付款凭证：借 2241.01 / 贷 1002 |
//! | 13 | `reverse_vouchers_after_cancel` | 作废红冲（红字凭证 + 蓝字 reversed_by） |
//! | 14 | `budget_check_and_occupy` | 预算校验 / 占用 / 释放 |
//! | 15 | `pay_status_three_values_from_db` | pay_status 三值完备（L6） |
//! | 16 | `resubmit_after_release_reuses_same_usage_row` | 撤回 / 驳回后**重提复用同一行**（本卡缺陷回归：唯一键把 `RELEASED` 也算唯一） |

use super::*;
use crate::business::erp::expense_model;
use crate::business::erp::voucher;
use nik::prelude::rust_decimal::Decimal;

mod db {
    use super::*;
    use tokio::sync::OnceCell;

    /// 金额字面量（不引 `rust_decimal_macros`）。
    macro_rules! dec {
        ($v:literal) => {{
            use std::str::FromStr as _;
            Decimal::from_str(stringify!($v)).expect("金额字面量非法")
        }};
    }

    /// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
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
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 取一个部门 id：**库里有就用，没有就造夹具部门**（TST-02b A 方案）。
    async fn any_dept() -> Uuid {
        crate::shared::test_helpers::ensure_any_dept().await
    }

    /// 取一个用户 id（报销人）：库里有就用，没有就造夹具账号。
    async fn any_user() -> Uuid {
        crate::shared::test_helpers::ensure_any_user().await
    }

    /// 取一条费用类型（V0242 迁移种子；`TRAVEL` 优先）。查不到 = 库版本不对。
    async fn travel_type() -> crate::business::erp::expense_model::TypeRow {
        if let Ok(Some(kind)) = expense_model::type_find_by_code(db(), "TRAVEL").await {
            return kind;
        }
        expense_model::type_active()
            .await
            .expect("查费用类型")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("库里没有启用中的费用类型（V0242 迁移种子未应用？）"))
    }

    /// 清场：按 id 集合删本用例造的行（**绝不按前缀扫全库**）。
    async fn cleanup(report_ids: &[Uuid]) {
        if report_ids.is_empty() {
            return;
        }
        let list = report_ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(",");
        for sql in [
            format!("DELETE FROM biz_expense_payment WHERE report_id IN ({list})"),
            format!("DELETE FROM biz_expense_item WHERE report_id IN ({list})"),
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                   (SELECT id FROM biz_voucher WHERE source_id IN ({list}))"
            ),
            format!("DELETE FROM biz_voucher WHERE source_id IN ({list})"),
            format!("DELETE FROM biz_budget_usage WHERE source_id IN ({list})"),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'EXPENSE_REPORT' \
                   AND entity_id IN ({list})"
            ),
            format!("DELETE FROM biz_expense_report WHERE id IN ({list})"),
        ] {
            let _ = db().exec_sql(sql, vec![]).await;
        }
    }

    fn uniq() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}{}", now % 1_000_000_000_000, std::process::id())
    }

    /// 造一张单的写库载荷（一条明细，税率 6%，可抵扣）。
    fn insert_of(
        report_no: String,
        dept_id: Uuid,
        reporter_id: Uuid,
        kind: &crate::business::erp::expense_model::TypeRow,
        with_tax: Decimal,
    ) -> crate::business::erp::expense_model::ReportInsert {
        use crate::business::erp::expense_compute;
        let split = expense_compute::split(with_tax, dec!(0.06), kind.tax_deductible);
        crate::business::erp::expense_model::ReportInsert {
            id: Uuid::now_v7(),
            report_no,
            reporter_id,
            reporter_name: "用例报销人".to_string(),
            dept_id,
            dept_name: "用例部门".to_string(),
            title: "真库用例：差旅报销".to_string(),
            expense_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
            period_code: "2026-11".to_string(),
            total_amount: split.amount,
            total_tax: split.tax,
            total_with_tax: split.with_tax,
            item_count: 1,
            remark: Some("expense_model_tests".to_string()),
            items: vec![crate::business::erp::expense_model::ItemInsert {
                line_no: 1,
                expense_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
                expense_type_id: kind.id,
                type_code: kind.type_code.clone(),
                type_name: kind.type_name.clone(),
                account_id: kind.account_id,
                account_code: kind.account_code.clone(),
                account_name: kind.account_name.clone(),
                amount: split.amount,
                tax_rate: dec!(0.06),
                tax_amount: split.tax,
                with_tax_amount: split.with_tax,
                invoice_no: Some("INV-TEST-001".to_string()),
                invoice_date: None,
                invoice_type: Some("VAT_SPECIAL".to_string()),
                summary: "高铁票".to_string(),
                remark: None,
            }],
        }
    }

    /// 建一张单（单头 + 明细），返回 id。
    async fn create_report(insert: &crate::business::erp::expense_model::ReportInsert) -> Uuid {
        expense_model::report_insert(db(), insert, None)
            .await
            .expect("插报销单");
        expense_model::items_insert_many(db(), insert.id, &insert.items)
            .await
            .expect("插明细");
        insert.id
    }

    // ==================== 1) 明细合计 = 单头 ====================

    #[tokio::test]
    async fn header_and_items_are_consistent() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        let id = create_report(&insert).await;
        let (total, tax, count) = expense_model::items_sum(db(), id).await.expect("聚合明细");
        assert_eq!(count, 1);
        assert_eq!(total, dec!(1060));
        assert_eq!(tax, dec!(60), "1060 含税 6% → 税额 60（1000 × 6%）");
        let row = expense_model::report_find(id).await.unwrap().expect("单据");
        assert_eq!(row.total_with_tax, total, "单头价税合计 = Σ 明细");
        assert_eq!(row.total_tax, tax, "单头税额 = Σ 明细");
        assert_eq!(row.item_count, count);
        assert_eq!(row.status, "DRAFT");
        assert_eq!(row.pay_status, "UNPAID");
        assert_eq!(row.unpaid_amount, row.total_with_tax);
        // 跨表恒等式（bridge 的复核）
        let items = expense_model::items_of(db(), id).await.expect("明细");
        crate::business::erp::expense_bridge::check_totals(&row, &items).expect("恒等式");
        cleanup(&[id]).await;
    }

    // ==================== 2) 单头价税恒等（库 CHECK） ====================

    #[tokio::test]
    async fn header_tax_sum_constraint() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let mut insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        insert.total_tax = dec!(9999); // 破坏 total_with_tax = total_amount + total_tax
        let err = expense_model::report_insert(db(), &insert, None).await;
        assert!(err.is_err(), "单头三道恒等式必须由库 CHECK 挡住");
        cleanup(&[insert.id]).await;
    }

    // ==================== 3) 明细价税恒等（库 CHECK） ====================

    #[tokio::test]
    async fn item_tax_sum_constraint() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let mut insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        insert.items[0].tax_amount = dec!(1);
        let id = insert.id;
        expense_model::report_insert(db(), &insert, None)
            .await
            .expect("单头合法");
        let err = expense_model::items_insert_many(db(), id, &insert.items).await;
        assert!(
            err.is_err(),
            "明细 with_tax = amount + tax 必须由库 CHECK 挡住"
        );
        cleanup(&[id]).await;
    }

    // ==================== 4) 明细金额必须 > 0 ====================

    #[tokio::test]
    async fn item_positive_amount_constraint() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let mut insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(100));
        insert.items[0].amount = Decimal::ZERO;
        insert.items[0].tax_amount = Decimal::ZERO;
        insert.items[0].with_tax_amount = Decimal::ZERO;
        let id = insert.id;
        expense_model::report_insert(db(), &insert, None)
            .await
            .expect("单头合法");
        let err = expense_model::items_insert_many(db(), id, &insert.items).await;
        assert!(err.is_err(), "零金额明细必须被 ck_biz_exp_item_pos 挡住");
        cleanup(&[id]).await;
    }

    // ==================== 5) 审批中改不动 ====================

    #[tokio::test]
    async fn update_draft_only_from_draft() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        let id = create_report(&insert).await;
        // DRAFT：能改
        let ok = expense_model::report_update_draft(db(), id, &insert, None)
            .await
            .expect("改草稿");
        assert_eq!(ok, 1);
        // 人为置成 SUBMITTED（CAS 的判据：不是草稿就 0 行）
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'SUBMITTED', approval_instance_id = gen_random_uuid() \
              WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("置审批中");
        let blocked = expense_model::report_update_draft(db(), id, &insert, None)
            .await
            .expect("改单查询");
        assert_eq!(blocked, 0, "审批中的单不能再改明细");
        cleanup(&[id]).await;
    }

    // ==================== 6/7/8) 付款回写 ====================

    #[tokio::test]
    async fn apply_partial_payment() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(6600));
        let id = create_report(&insert).await;
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("置已审批");
        let date = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let affected = expense_model::report_apply_payment(
            db(),
            id,
            dec!(1000),
            date,
            "PARTIAL",
            "APPROVED",
            None,
        )
        .await
        .expect("部分付款");
        assert_eq!(affected, 1);
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.status, "APPROVED", "部分付款不改状态");
        assert_eq!(row.pay_status, "PARTIAL");
        assert_eq!(row.paid_amount, dec!(1000));
        assert_eq!(row.unpaid_amount, dec!(5600));
        assert_eq!(row.last_pay_date, Some(date));
        cleanup(&[id]).await;
    }

    #[tokio::test]
    async fn apply_full_payment() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(500));
        let id = create_report(&insert).await;
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("置已审批");
        let date = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let affected =
            expense_model::report_apply_payment(db(), id, dec!(500), date, "PAID", "PAID", None)
                .await
                .expect("付清");
        assert_eq!(affected, 1);
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.status, "PAID");
        assert_eq!(row.pay_status, "PAID");
        assert_eq!(row.unpaid_amount, Decimal::ZERO);
        cleanup(&[id]).await;
    }

    #[tokio::test]
    async fn overpay_is_rejected_by_database() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(100));
        let id = create_report(&insert).await;
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("置已审批");
        let date = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let affected =
            expense_model::report_apply_payment(db(), id, dec!(1000), date, "PAID", "PAID", None)
                .await
                .expect("超额付款语句本身不报错");
        assert_eq!(
            affected, 0,
            "超额付款必须 0 行（WHERE paid + x <= total 挡住）"
        );
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.paid_amount, Decimal::ZERO);
        cleanup(&[id]).await;
    }

    // ==================== 9) 类型引用计数 ====================

    #[tokio::test]
    async fn type_usage_count() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let before = expense_model::type_used_count(db(), kind.id)
            .await
            .expect("计数");
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(100));
        let id = create_report(&insert).await;
        let after = expense_model::type_used_count(db(), kind.id)
            .await
            .expect("计数");
        assert!(
            after > before,
            "新建单据后引用计数必须 +1（当前 {before} → {after}）"
        );
        cleanup(&[id]).await;
    }

    // ==================== 10) 跨表恒等式 ====================

    #[tokio::test]
    async fn check_totals_detects_mismatch() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        let id = create_report(&insert).await;
        let mut row = expense_model::report_find(id).await.unwrap().unwrap();
        let items = expense_model::items_of(db(), id).await.expect("明细");
        // 单头被人改成别的数（模拟脏数据）：必须 422 + 66042
        row.total_with_tax = dec!(9999);
        let err =
            crate::business::erp::expense_bridge::check_totals(&row, &items).expect_err("必须报错");
        assert_eq!(err.status, 422);
        assert_eq!(
            err.code,
            crate::shared::error_code::ERP_EXPENSE_TOTAL_MISMATCH
        );
        cleanup(&[id]).await;
    }

    // ==================== 11) 入账凭证 ====================

    #[tokio::test]
    async fn approval_voucher_is_balanced_with_tax_line() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let mut insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(3180));
        // 加第二条：办公费 240（13%），验证「多费用科目 → 多借方行」
        let office = expense_model::type_find_by_code(db(), "OFFICE")
            .await
            .ok()
            .flatten();
        if let Some(office) = office {
            let split = crate::business::erp::expense_compute::split(
                dec!(240),
                dec!(0.13),
                office.tax_deductible,
            );
            insert
                .items
                .push(crate::business::erp::expense_model::ItemInsert {
                    line_no: 2,
                    expense_date: insert.expense_date,
                    expense_type_id: office.id,
                    type_code: office.type_code.clone(),
                    type_name: office.type_name.clone(),
                    account_id: office.account_id,
                    account_code: office.account_code.clone(),
                    account_name: office.account_name.clone(),
                    amount: split.amount,
                    tax_rate: dec!(0.13),
                    tax_amount: split.tax,
                    with_tax_amount: split.with_tax,
                    invoice_no: Some("INV-TEST-002".to_string()),
                    invoice_date: None,
                    invoice_type: Some("VAT_SPECIAL".to_string()),
                    summary: "打印耗材".to_string(),
                    remark: None,
                });
            insert.total_amount += split.amount;
            insert.total_tax += split.tax;
            insert.total_with_tax += split.with_tax;
            insert.item_count += 1;
        }
        let id = create_report(&insert).await;
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        let items = expense_model::items_of(db(), id).await.expect("明细");
        let tx = db().begin().await.expect("事务");
        let voucher_id = crate::business::erp::expense_bridge::post_report_voucher(
            &tx,
            &row,
            &items,
            nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            nik::prelude::chrono::Utc::now(),
        )
        .await
        .expect("生成入账凭证");
        tx.commit().await.expect("提交");
        let entries = voucher::entries_of(db(), voucher_id).await.expect("分录");
        let debit: Decimal = entries.iter().map(|entry| entry.debit).sum();
        let credit: Decimal = entries.iter().map(|entry| entry.credit).sum();
        assert_eq!(debit, credit, "借贷必须平衡");
        assert_eq!(debit, row.total_with_tax, "借方合计 = 报销单价税合计");
        let tax_entry = entries.iter().find(|entry| entry.account.code == "2221.01");
        assert!(tax_entry.is_some(), "可抵扣明细必须生成进项税行");
        assert_eq!(
            tax_entry.unwrap().debit,
            row.total_tax,
            "税行金额 = Σ 明细税额"
        );
        let credit_entry = entries.iter().find(|entry| entry.credit > Decimal::ZERO);
        assert_eq!(
            credit_entry.map(|entry| entry.account.code.as_str()),
            Some("2241.01"),
            "贷方必须是 2241.01 员工垫付"
        );
        cleanup(&[id]).await;
    }

    // ==================== 12) 付款凭证 ====================

    #[tokio::test]
    async fn payment_voucher_two_lines() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        let id = create_report(&insert).await;
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        let tx = db().begin().await.expect("事务");
        let voucher_id = crate::business::erp::expense_bridge::post_payment_voucher(
            &tx,
            &row,
            "PAY-EXP-TEST-000001",
            dec!(500),
            nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            "1002",
            nik::prelude::chrono::Utc::now(),
        )
        .await
        .expect("生成付款凭证");
        tx.commit().await.expect("提交");
        let entries = voucher::entries_of(db(), voucher_id).await.expect("分录");
        assert_eq!(entries.len(), 2, "付款凭证只有两行（借 2241.01 / 贷 1002）");
        let debit = entries
            .iter()
            .find(|entry| entry.debit > Decimal::ZERO)
            .expect("借方");
        let credit = entries
            .iter()
            .find(|entry| entry.credit > Decimal::ZERO)
            .expect("贷方");
        assert_eq!(debit.account.code, "2241.01");
        assert_eq!(credit.account.code, "1002");
        assert_eq!(debit.debit, dec!(500));
        assert_eq!(credit.credit, dec!(500));
        cleanup(&[id]).await;
    }

    // ==================== 13) 作废红冲 ====================

    #[tokio::test]
    async fn reverse_vouchers_after_cancel() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1060));
        let id = create_report(&insert).await;
        let row = expense_model::report_find(id).await.unwrap().unwrap();
        let items = expense_model::items_of(db(), id).await.expect("明细");
        let tx = db().begin().await.expect("事务");
        let voucher_id = crate::business::erp::expense_bridge::post_report_voucher(
            &tx,
            &row,
            &items,
            nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            nik::prelude::chrono::Utc::now(),
        )
        .await
        .expect("生成入账凭证");
        // 红冲必须发生在同一事务里（作废的三件事同事务）
        let reds = crate::business::erp::expense_bridge::reverse_report_vouchers(
            &tx,
            id,
            "报销单填错部门，财务要求整单作废重提",
            None,
        )
        .await
        .expect("红冲");
        tx.commit().await.expect("提交");
        assert_eq!(reds.len(), 1, "一张蓝字凭证对应一张红字凭证");
        let blue = voucher::find_voucher(db(), voucher_id)
            .await
            .expect("查蓝字")
            .expect("蓝字存在");
        assert!(blue.reversed_by.is_some(), "蓝字凭证必须被标记已红冲");
        let red = voucher::find_voucher(db(), reds[0])
            .await
            .expect("查红字")
            .expect("红字存在");
        assert_eq!(red.is_red, 1, "红字凭证 is_red = 1");
        // 再冲一次：已经红冲过的不重复冲
        let again = crate::business::erp::expense_bridge::reverse_report_vouchers(
            db(),
            id,
            "重复作废",
            None,
        )
        .await
        .expect("重复红冲");
        assert!(again.is_empty(), "已红冲的蓝字凭证不能重复红冲");
        cleanup(&[id]).await;
    }

    // ==================== 14) 预算校验 / 占用 / 释放 ====================

    #[tokio::test]
    async fn budget_check_and_occupy() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1200));
        let id = create_report(&insert).await;
        let items = expense_model::items_of(db(), id).await.expect("明细");
        let slots = vec![crate::business::erp::budget_guard::ExpenseSlotAmount {
            account_id: items[0].account_id,
            amount: items[0].with_tax_amount,
            amount_net: items[0].amount,
        }];
        let budget =
            crate::business::erp::budget_guard::check_for_expense(db(), dept, "2026-11", &slots)
                .await
                .expect("预算校验");
        // 三种策略都可能（取决于库里配置）：只断言「结构完整」与「不误报 BLOCK」
        assert_eq!(budget.dept_id, dept);
        if !budget.checks.is_empty() {
            assert_eq!(budget.checks[0].amount, dec!(1200));
            if !budget.over() {
                let tx = db().begin().await.expect("事务");
                let written = crate::business::erp::budget_guard::occupy_for_expense(
                    &tx,
                    &budget,
                    id,
                    "EXP-T-BUDGET",
                    None,
                )
                .await
                .expect("写占用");
                tx.commit().await.expect("提交");
                if written > 0 {
                    let usages =
                        crate::business::erp::budget_guard::usages_of(db(), "EXPENSE_REPORT", id)
                            .await
                            .expect("查占用");
                    assert_eq!(usages.len(), written);
                    assert_eq!(usages[0].status, "OCCUPIED");
                    let released = crate::business::erp::budget_guard::release(
                        db(),
                        "EXPENSE_REPORT",
                        id,
                        "REJECTED",
                    )
                    .await
                    .expect("释放");
                    assert_eq!(released, u64::try_from(written).unwrap());
                    let after =
                        crate::business::erp::budget_guard::usages_of(db(), "EXPENSE_REPORT", id)
                            .await
                            .expect("查占用");
                    assert_eq!(after[0].status, "RELEASED");
                    assert_eq!(after[0].release_reason.as_deref(), Some("REJECTED"));
                }
            }
        }
        cleanup(&[id]).await;
    }

    // ==================== 15) pay_status 三值 ====================

    #[tokio::test]
    async fn pay_status_three_values_from_db() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(600));
        let id = create_report(&insert).await;
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'APPROVED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("置已审批");
        let date = nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        // UNPAID → PARTIAL → PAID
        let mut row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.pay_status, "UNPAID");
        expense_model::report_apply_payment(db(), id, dec!(200), date, "PARTIAL", "APPROVED", None)
            .await
            .expect("第一次付款");
        row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.pay_status, "PARTIAL");
        expense_model::report_apply_payment(db(), id, dec!(400), date, "PAID", "PAID", None)
            .await
            .expect("第二次付款");
        row = expense_model::report_find(id).await.unwrap().unwrap();
        assert_eq!(row.pay_status, "PAID");
        assert_eq!(row.status, "PAID");
        assert_eq!(row.paid_amount, row.total_with_tax);
        assert_eq!(row.unpaid_amount, Decimal::ZERO);
        cleanup(&[id]).await;
    }

    // ==================== 16) 撤回后重新提交：复用同一行（缺陷回归） ====================

    /// 卡面缺陷回归：`提交 → 撤回 → 再提交` **不能再撞唯一键**。
    ///
    /// 释放（`release`）只把 usage 置 `RELEASED`、不删行（留痕，`deleted_at` 仍为 NULL），
    /// 而 `uk_biz_budget_usage_source` 的谓词只有 `deleted_at IS NULL` —— 第二次提交若走
    /// 纯 INSERT 必然 23505 → 整个提交事务 ROLLBACK（真机上的「重提 500，单据卡 DRAFT」）。
    ///
    /// 修法：`occupy` 先条件 UPDATE 把同键的 `RELEASED` 行拉回 `OCCUPIED`，没命中才 INSERT。
    /// 本用例钉四件事：
    /// 1. 连续 3 轮「释放 → 再占用」**复用同一个 id**，库里永远只有 1 行；
    /// 2. 复用时金额 / 期间 / 超支快照被重置成**这一次**的值；
    /// 3. `released_at / release_reason` 清空（库 CHECK `ck_biz_budget_usage_done` 要求
    ///    `RELEASED` ⟺ `released_at` 非空，所以状态改回来必须一起清）；
    /// 4. 并发两个「再提交」也只会得到 1 行（一个复用成功、另一个走 23505 幂等路径）。
    #[tokio::test]
    async fn resubmit_after_release_reuses_same_usage_row() {
        if !ensure_db().await {
            return;
        }
        let dept = any_dept().await;
        let user = any_user().await;
        let kind = travel_type().await;
        use crate::business::erp::budget_guard;
        let insert = insert_of(format!("EXP-T{}", uniq()), dept, user, &kind, dec!(1200));
        let id = create_report(&insert).await;
        let items = expense_model::items_of(db(), id).await.expect("明细");
        let slot = budget_guard::Slot::new(dept, items[0].account_id, &insert.period_code)
            .expect("槽（期间码合法）");
        let verdict = budget_guard::BudgetVerdict::evaluate(
            budget_guard::ControlMode::Warn,
            Some(Uuid::now_v7()),
            dec!(100000),
            Decimal::ZERO,
            dec!(100000),
            Decimal::ZERO,
            dec!(1200),
        );
        assert!(!verdict.over, "额度充足时不该标超支");
        let snapshot = budget_guard::SlotSnapshot {
            budget_id: None,
            year_limit: dec!(100000),
            year_used: Decimal::ZERO,
            month_limit: dec!(100000),
            month_used: Decimal::ZERO,
        };
        let occupy = |amount: Decimal| {
            budget_guard::occupy(
                db(),
                &slot,
                amount,
                amount,
                budget_guard::SOURCE_EXPENSE,
                id,
                "EXP-T-RESUBMIT",
                &verdict,
                &snapshot,
                None,
            )
        };
        // 第一次提交：库里没有同键行 → 必然插入新行。
        let first_id = occupy(dec!(1200))
            .await
            .expect("首次占用")
            .expect("首次必须插新行");
        // 3 轮「撤回 → 再提交」：id 必须不变、行数必须不涨。
        for round in 1..=3 {
            let released =
                budget_guard::release(db(), budget_guard::SOURCE_EXPENSE, id, "WITHDRAWN")
                    .await
                    .expect("释放");
            assert_eq!(released, 1, "第 {round} 轮释放 1 行");
            let again = occupy(dec!(1200))
                .await
                .expect("再次占用不能再报错（旧实现这里 23505 → 提交事务回滚）");
            assert_eq!(
                again,
                Some(first_id),
                "第 {round} 轮必须复用同一行，不能新插一条"
            );
            let usages = budget_guard::usages_of(db(), budget_guard::SOURCE_EXPENSE, id)
                .await
                .expect("查占用");
            assert_eq!(usages.len(), 1, "第 {round} 轮后 usage 仍只有一份");
            assert_eq!(usages[0].status, "OCCUPIED", "第 {round} 轮后回到占用中");
            assert_eq!(usages[0].amount, dec!(1200));
            assert!(
                usages[0].released_at.is_none() && usages[0].release_reason.is_none(),
                "第 {round} 轮复用后释放痕迹必须清空（ck_biz_budget_usage_done）"
            );
        }
        // 复用时金额被重置成「这一次」的值（草稿里改过金额再提交）。
        budget_guard::release(db(), budget_guard::SOURCE_EXPENSE, id, "WITHDRAWN")
            .await
            .expect("改金额前释放");
        let changed = occupy(dec!(800))
            .await
            .expect("改金额后再占用")
            .expect("复用同一行");
        assert_eq!(changed, first_id, "金额变了也还是同一行");
        let usages = budget_guard::usages_of(db(), budget_guard::SOURCE_EXPENSE, id)
            .await
            .expect("查占用");
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].amount, dec!(800), "金额必须重置成这次提交的值");
        assert_eq!(usages[0].period_code, insert.period_code);
        // 并发两个「再提交」：一个复用、一个走 23505 幂等路径，结果仍是 1 行。
        budget_guard::release(db(), budget_guard::SOURCE_EXPENSE, id, "WITHDRAWN")
            .await
            .expect("并发前释放");
        let (left, right) = tokio::join!(occupy(dec!(800)), occupy(dec!(800)));
        let left = left.expect("并发占用 A");
        let right = right.expect("并发占用 B");
        assert_eq!(
            [left, right].iter().filter(|value| value.is_some()).count(),
            1,
            "并发两个提交只能有一个把行拉回来（另一个当重复提交幂等成功）"
        );
        let usages = budget_guard::usages_of(db(), budget_guard::SOURCE_EXPENSE, id)
            .await
            .expect("查占用");
        assert_eq!(usages.len(), 1, "并发后也不能产生第二行");
        assert_eq!(usages[0].status, "OCCUPIED");
        cleanup(&[id]).await;
    }

    // ==================== 17) 分页 SQL 契约（SCOPE-PAGE 回归；不连库，永远跑） ====================

    /// 与 `report_page` **同一条**组装路径：基础 SQL（带 `{FILTER}`）+ 业务过滤片段。
    fn report_page_sql(
        q: &crate::business::erp::expense_dto::ReportFilter,
    ) -> (String, Vec<Value>) {
        let mut params: Vec<Value> = Vec::new();
        let filters = crate::business::erp::expense_model::report_page_filters(q, &mut params)
            .expect("组装报销分页过滤片段");
        let mut sql = crate::business::erp::expense_model::report_page_base_sql();
        sql.push_str(&filters);
        (sql, params)
    }

    /// `{FILTER}` 必须**恰好一次**：0 次就是 SCOPE-PAGE 卡的现场（片段只能追加到末尾 → 非超管带筛选
    /// 500），2 次 `scope::paginate` 会直接拒（无法判断插哪个）。
    #[test]
    fn page_base_sql_has_exactly_one_filter_marker() {
        let sql = crate::business::erp::expense_model::report_page_base_sql();
        assert_eq!(
            sql.matches(scope::FILTER_MARKER).count(),
            1,
            "报销分页的 base_sql 必须恰好一个 {{FILTER}}：{sql}"
        );
        assert!(
            sql.contains("e.deleted_at IS NULL"),
            "恒真条件不能丢：{sql}"
        );
        assert!(!sql.contains('?'), "基础 SQL 不得自带业务参数：{sql}");
    }

    /// 片段参数恒前置（架构 §2.6）：`{FILTER}` 必须在**所有业务 `?` 之前**，且 `?` 个数
    /// 等于参数个数 —— 顺序错位比报错更危险，所以逐组合锁死。
    #[test]
    fn business_params_always_follow_the_filter_marker() {
        use crate::business::erp::expense_dto::ReportFilter;
        use nik::prelude::chrono::NaiveDate;
        let day = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let dept = Uuid::now_v7().to_string();
        let reporter = Uuid::now_v7().to_string();
        let cases: Vec<(&str, ReportFilter)> = vec![
            ("无筛选", ReportFilter::default()),
            (
                "keyword",
                ReportFilter {
                    keyword: Some("差旅".into()),
                    ..Default::default()
                },
            ),
            (
                "status",
                ReportFilter {
                    status: Some("DRAFT".into()),
                    ..Default::default()
                },
            ),
            (
                "pay_status",
                ReportFilter {
                    pay_status: Some("UNPAID".into()),
                    ..Default::default()
                },
            ),
            (
                "reporter_id",
                ReportFilter {
                    reporter_id: Some(reporter.clone()),
                    ..Default::default()
                },
            ),
            (
                "dept_id",
                ReportFilter {
                    dept_id: Some(dept.clone()),
                    ..Default::default()
                },
            ),
            (
                "period_code",
                ReportFilter {
                    period_code: Some("2026-11".into()),
                    ..Default::default()
                },
            ),
            (
                "date_from",
                ReportFilter {
                    date_from: Some(day),
                    ..Default::default()
                },
            ),
            (
                "date_to",
                ReportFilter {
                    date_to: Some(day),
                    ..Default::default()
                },
            ),
            (
                "全部同时",
                ReportFilter {
                    keyword: Some("差旅".into()),
                    status: Some("DRAFT".into()),
                    pay_status: Some("UNPAID".into()),
                    reporter_id: Some(reporter),
                    dept_id: Some(dept),
                    period_code: Some("2026-11".into()),
                    date_from: Some(day),
                    date_to: Some(day),
                    page: Some(1),
                    page_size: Some(20),
                },
            ),
        ];
        for (name, filter) in cases {
            let (sql, params) = report_page_sql(&filter);
            let marker = sql
                .find(scope::FILTER_MARKER)
                .unwrap_or_else(|| panic!("【{name}】base_sql 缺 {{FILTER}}：{sql}"));
            assert_eq!(
                sql.matches('?').count(),
                params.len(),
                "【{name}】`?` 个数必须等于参数个数（游标/类型别名也算占位）：{sql}"
            );
            if let Some(first) = sql.find('?') {
                assert!(
                    marker < first,
                    "【{name}】{{FILTER}} 必须在第一个业务 `?` 之前：{sql}"
                );
            }
            // 真机上 500 的那道守卫：直接跑一遍，任何组合都不许被拒。
            crate::business::shared::scope::ensure_injection_point(&sql, params.len())
                .unwrap_or_else(|err| panic!("【{name}】注入位置契约校验失败：{err}"));
        }
    }

    /// 反证：SCOPE-PAGE 卡的旧写法（业务条件拼在恒真条件后、不留标记）必须被守卫拒绝 ——
    /// 这条锁住「为什么非超管带筛选会 500」的机制，防止以后又被「顺手拼 WHERE」退化。
    #[test]
    fn old_shape_without_marker_is_rejected_by_guard() {
        let old = "SELECT 1 FROM biz_expense_report e WHERE e.deleted_at IS NULL AND e.status = ?";
        let error = crate::business::shared::scope::ensure_injection_point(old, 1)
            .expect_err("带业务参数却没有 {FILTER} 必须报错");
        let text = format!("{error}");
        assert!(
            text.contains("{FILTER}"),
            "错误信息要点名标记，便于定位：{text}"
        );
    }

    // ==================== 18) 带筛选 + 数据权限裁剪（SCOPE-PAGE 真库回归） ====================

    /// 取 n 个部门 id：库里有多少用多少，不够的用夹具补齐（TST-02b A 方案）。
    ///
    /// 跨部门数据权限用例过去在「库少于 2 个部门」时 SKIP 假绿；现在空库也会补出
    /// `TST-DEPT` / `TST-DEPT-2`，断言照跑。补齐用的是固定 UUID + 幂等插入，
    /// 不会按前缀删别人的行。
    async fn some_depts(n: usize) -> Vec<Uuid> {
        let rows = db()
            .query_sql(
                format!(
                    "SELECT id FROM sys_dept WHERE deleted_at IS NULL \
                      ORDER BY parent_id NULLS FIRST, sort_order, id LIMIT {n}"
                ),
                vec![],
            )
            .await
            .unwrap_or_else(|err| panic!("查部门失败：{err}"));
        let mut ids: Vec<Uuid> = rows
            .iter()
            .filter_map(|row| row.get_typed::<Uuid>("id").ok())
            .collect();
        let mut seq = 0usize;
        while ids.len() < n {
            seq += 1;
            let code = if seq == 1 {
                crate::shared::test_helpers::FIXTURE_DEPT_CODE.to_string()
            } else {
                format!("{}-{seq}", crate::shared::test_helpers::FIXTURE_DEPT_CODE)
            };
            let name = if seq == 1 {
                crate::shared::test_helpers::FIXTURE_DEPT_NAME.to_string()
            } else {
                format!("{}{seq}", crate::shared::test_helpers::FIXTURE_DEPT_NAME)
            };
            let id = crate::shared::test_helpers::ensure_dept(&code, &name).await;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// **SCOPE-PAGE 回归（真库）**：有数据权限裁剪的 principal 带 keyword / status / pay_status
    /// 的分页必须成功（旧代码在 `scope::paginate` 的注入守卫直接 500），且行数按范围收敛；
    /// 同一组筛选的超管路径结果不受影响（两张都看得见）。
    #[tokio::test]
    async fn page_with_filters_is_scoped_and_never_fails() {
        use crate::business::erp::expense_dto::ReportFilter;
        use crate::business::shared::principal::ScopedPrincipal;
        use crate::business::shared::scope::ScopeKind;
        if !ensure_db().await {
            return;
        }
        let user = any_user().await;
        let kind = travel_type().await;
        let depts = some_depts(2).await;
        let (mine, other) = (depts[0], depts[1]);
        let keyword = format!("SCOPE-REGRESS-{}", uniq());
        let mut mine_insert =
            insert_of(format!("EXP-S1-{}", uniq()), mine, user, &kind, dec!(1060));
        mine_insert.title = format!("{keyword} 本部门");
        let mut other_insert =
            insert_of(format!("EXP-S2-{}", uniq()), other, user, &kind, dec!(1060));
        other_insert.title = format!("{keyword} 他部门");
        let mine_id = create_report(&mine_insert).await;
        let other_id = create_report(&other_insert).await;

        // 「本部门」范围的非超管：片段是 ` AND e.dept_id = ?`（1 个参数）→ 旧代码在这里 500。
        let restricted =
            ScopedPrincipal::for_test(user, Some(mine), false, ScopeKind::Dept, vec![mine], false);
        let super_admin =
            ScopedPrincipal::for_test(user, Some(mine), true, ScopeKind::All, Vec::new(), false);

        let cases: Vec<(&str, ReportFilter)> = vec![
            (
                "keyword",
                ReportFilter {
                    keyword: Some(keyword.clone()),
                    ..Default::default()
                },
            ),
            (
                "keyword+status",
                ReportFilter {
                    keyword: Some(keyword.clone()),
                    status: Some("DRAFT".into()),
                    ..Default::default()
                },
            ),
            (
                "keyword+pay_status",
                ReportFilter {
                    keyword: Some(keyword.clone()),
                    pay_status: Some("UNPAID".into()),
                    ..Default::default()
                },
            ),
            (
                "keyword+status+pay_status+period",
                ReportFilter {
                    keyword: Some(keyword.clone()),
                    status: Some("DRAFT".into()),
                    pay_status: Some("UNPAID".into()),
                    period_code: Some("2026-11".into()),
                    ..Default::default()
                },
            ),
        ];
        for (name, filter) in &cases {
            let (rows, total) = expense_model::report_page(filter, 1, 50, &restricted)
                .await
                .unwrap_or_else(|err| {
                    panic!("【{name}】非超管带筛选的分页必须成功（旧代码 500）：{err}")
                });
            assert_eq!(
                total, 1,
                "【{name}】范围裁剪后只该看见本部门那张，total={total}"
            );
            assert_eq!(rows.len(), 1, "【{name}】");
            assert!(
                rows.iter().all(|row| row.dept_id == mine),
                "【{name}】返回行必须都在数据范围内"
            );
            assert!(
                rows.iter().any(|row| row.id == mine_id),
                "【{name}】本部门那张必须在结果里"
            );
            // 修复不能改坏超管路径：超管不做裁剪，两张都看得见。
            let (super_rows, super_total) = expense_model::report_page(filter, 1, 50, &super_admin)
                .await
                .unwrap_or_else(|err| panic!("【{name}】超管分页失败：{err}"));
            assert_eq!(
                super_total, 2,
                "【{name}】超管看见两张，total={super_total}"
            );
            assert!(
                super_rows.iter().any(|row| row.id == other_id),
                "【{name}】超管要能看见他部门那张"
            );
        }
        // 无筛选（空条件，不触发注入校验）也必须按范围裁剪。
        let (all_rows, _) =
            expense_model::report_page(&ReportFilter::default(), 1, 200, &restricted)
                .await
                .expect("无筛选分页必须成功");
        assert!(
            all_rows.iter().all(|row| row.dept_id == mine),
            "无筛选也必须按数据范围裁剪"
        );

        cleanup(&[mine_id, other_id]).await;
    }
}
