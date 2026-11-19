//! 差异凭证的真库用例（`ERP_DB_URL` 开关）—— M5-16。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/<演练库> \
//!   cargo test -p cloudpivot --lib cost_variance_voucher_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`posts_one_balanced_voucher_for_a_period`] | **卡面「生成凭证」正面证据**：一张凭证、`source_type='COST_VARIANCE'`、`status='POSTED'`、分录科目与金额逐行 = 分摊结果（不利→借 5401 / 贷 1404，有利反向）、Σ借 = Σ贷、`voucher_ids_of` 能反查到 |
//! | 2 | [`second_post_is_rejected_without_writing`] | 幂等闸：同期间第二次 ⇒ 409 + `66112`，凭证条数不变（不重复记账） |
//! | 3 | [`empty_draft_posts_nothing`] | 没有非零差异不发空凭证（`Ok(None)`，库里零张） |
//! | 4 | [`closed_period_is_rejected_without_writing`] | 期间已结账 ⇒ 被 `post_voucher` 的期间闸拦下，且库里不留半张凭证 |
//!
//! # 夹具纪律
//!
//! 期间年份从 `2700` 起（与 B3-5 的 2600+ 错开），期间码 = `<年>-11`；清理只按本前缀的
//! `source_no` / `biz_accounting_period.period_code` 删，绝不动别人的数据。
//! 科目 1404 / 5401 来自 V0183 的标准科目表（本用例**不种**科目：种了就等于测自己造的表）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_qty_variance_dto::VARIANCE_LABOR_EFF;
    use crate::business::erp::cost::cost_standard_compare_dto::{ELEMENT_LABOR, ELEMENT_MATERIAL};
    use crate::business::erp::cost::cost_variance_alloc_dto::{
        PeriodVarianceAllocOutcome, VARIANCE_TYPES_ALL, VarianceAllocLine, VarianceAllocOutcome,
    };
    use crate::business::erp::cost::cost_variance_dto::VARIANCE_MAT_PRICE;
    use crate::business::erp::cost::cost_variance_voucher::{self as cv, VarianceVoucherContext};
    use cloudpivot_kernel::shared::error_code;
    use nik::prelude::chrono::{NaiveDate, Utc};
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;
    use tokio::sync::OnceCell;

    /// 连库（进程内只初始化一次）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
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
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    /// 临时会计期间（用例自建，跑完删）。
    struct Fixture {
        year: i32,
        period_code: String,
    }

    impl Fixture {
        async fn build() -> Self {
            let year = crate::test_seed::year_seq(2700, 300);
            let period_code = format!("{year}-11");
            sweep(&period_code).await;
            // 会计期间走统一夹具（P2-62：测试代码不许手写主数据 INSERT）
            crate::test_seed::ensure_accounting_period(year, 11).await;
            Self { year, period_code }
        }

        fn voucher_date(&self) -> NaiveDate {
            NaiveDate::from_ymd_opt(self.year, 11, 30).expect("凭证日期")
        }

        fn context(&self) -> VarianceVoucherContext<'static> {
            VarianceVoucherContext {
                period_code: Box::leak(self.period_code.clone().into_boxed_str()),
                voucher_date: self.voucher_date(),
                operator: None,
                now: Utc::now(),
            }
        }

        async fn cleanup(&self) {
            sweep(&self.period_code).await;
        }
    }

    /// 清掉该期间码的差异凭证 + 会计期间（前缀限定，绝不动别人的数据）。
    async fn sweep(period_code: &str) {
        db().exec_sql(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                (SELECT id FROM biz_voucher WHERE source_type = 'COST_VARIANCE' AND source_no = ?)"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("清差异凭证分录");
        db().exec_sql(
            "DELETE FROM biz_voucher WHERE source_type = 'COST_VARIANCE' AND source_no = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("清差异凭证");
        db().exec_sql(
            "DELETE FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("清会计期间");
    }

    fn alloc_line(object_no: &str, amount: &str) -> VarianceAllocLine {
        VarianceAllocLine {
            cost_object_id: Uuid::now_v7(),
            cost_object_no: object_no.to_string(),
            cost_object_status: "OPEN".to_string(),
            basis_value: dec("100"),
            share_rate: Some(dec("0.5")),
            alloc_amount: dec(amount),
            rounding_adj: Decimal::ZERO,
            participates: !dec(amount).is_zero(),
        }
    }

    fn pool(
        period: &str,
        variance_type: &str,
        element: &str,
        material: Option<&str>,
        lines: Vec<VarianceAllocLine>,
    ) -> VarianceAllocOutcome {
        let allocated: Decimal = lines.iter().map(|line| line.alloc_amount).sum();
        VarianceAllocOutcome {
            period_code: period.to_string(),
            variance_type: variance_type.to_string(),
            element: element.to_string(),
            material_id: material.map(|_| Uuid::now_v7()),
            material_code: material.map(|code| code.to_string()),
            rule_id: None,
            rule_code: "DEFAULT".to_string(),
            rule_name: "默认口径".to_string(),
            basis: "ACTUAL_QTY".to_string(),
            basis_source: "DEFAULT",
            total_amount: allocated,
            attributed_amount: Decimal::ZERO,
            allocatable_amount: allocated,
            basis_total: dec("200"),
            lines,
            rounding_total: Decimal::ZERO,
        }
    }

    /// 两个池：材料价差 +1200（两个对象）、人工效率差 −300（一个对象，另一个 0 元行）。
    fn sample(period: &str) -> PeriodVarianceAllocOutcome {
        PeriodVarianceAllocOutcome {
            period_code: period.to_string(),
            outcomes: vec![
                pool(
                    period,
                    VARIANCE_MAT_PRICE,
                    ELEMENT_MATERIAL,
                    Some("MAT-A"),
                    vec![alloc_line("CB-DV-1", "700"), alloc_line("CB-DV-2", "500")],
                ),
                pool(
                    period,
                    VARIANCE_LABOR_EFF,
                    ELEMENT_LABOR,
                    None,
                    vec![alloc_line("CB-DV-1", "-300"), alloc_line("CB-DV-2", "0")],
                ),
            ],
        }
    }

    /// 凭证单头（断言用）。
    struct Header {
        id: Uuid,
        status: String,
        source_type: String,
        source_no: Option<String>,
        total_debit: Decimal,
        total_credit: Decimal,
    }

    async fn header(period_code: &str) -> Option<Header> {
        let rows = db()
            .query_sql(
                "SELECT id, status, source_type, source_no, total_debit, total_credit \
                   FROM biz_voucher WHERE source_type = 'COST_VARIANCE' AND source_no = ? \
                    AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::Text(period_code.to_string())],
            )
            .await
            .expect("查凭证单头");
        rows.iter()
            .map(|row| {
                Ok(Header {
                    id: row.get_typed("id")?,
                    status: row.get_typed("status")?,
                    source_type: row.get_typed("source_type")?,
                    source_no: row.get_typed("source_no")?,
                    total_debit: row.get_typed("total_debit")?,
                    total_credit: row.get_typed("total_credit")?,
                })
            })
            .collect::<DbResult<Vec<Header>>>()
            .expect("解析凭证单头")
            .into_iter()
            .next()
    }

    /// 分录行（科目, 借, 贷）。
    async fn entries(voucher_id: Uuid) -> Vec<(String, Decimal, Decimal)> {
        let rows = db()
            .query_sql(
                "SELECT account_code, debit_amount, credit_amount FROM biz_voucher_entry \
                  WHERE voucher_id = ?::uuid ORDER BY line_no ASC"
                    .to_string(),
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查分录");
        rows.iter()
            .map(|row| {
                Ok((
                    row.get_typed::<String>("account_code")?,
                    row.get_typed::<Decimal>("debit_amount")?,
                    row.get_typed::<Decimal>("credit_amount")?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()
            .expect("解析分录")
    }

    /// ① 卡面「生成凭证」：一张、平衡、科目与金额 = 分摊结果。
    #[tokio::test]
    async fn posts_one_balanced_voucher_for_a_period() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let draft = cv::build_draft(&sample(&fixture.period_code)).expect("草案");
        assert_eq!(draft.line_count(), 3, "0 元行不进凭证");
        assert_eq!(draft.net_amount, dec("900"));
        let posted = cv::post_period_voucher(db(), &fixture.context(), &draft)
            .await
            .expect("落库")
            .expect("非空草案应出凭证");
        assert_eq!(posted.entry_count, 5, "3 条差异腿 + 2 条损益腿");
        assert_eq!(posted.total, dec("1500"));

        let head = header(&fixture.period_code).await.expect("凭证单头");
        assert_eq!(head.id, posted.voucher_id);
        assert_eq!(head.status, "POSTED");
        assert_eq!(head.source_type, "COST_VARIANCE");
        assert_eq!(head.source_no.as_deref(), Some(fixture.period_code.as_str()));
        assert_eq!(head.total_debit, dec("1500"));
        assert_eq!(head.total_credit, dec("1500"));

        let lines = entries(posted.voucher_id).await;
        assert_eq!(lines.len(), 5);
        let debit: Decimal = lines.iter().map(|line| line.1).sum();
        let credit: Decimal = lines.iter().map(|line| line.2).sum();
        assert_eq!(debit, credit, "借贷必须相等");
        assert_eq!(debit, dec("1500"));
        // 差异科目 1404：贷 1200（不利）+ 借 300（有利）
        let variance: Vec<&(String, Decimal, Decimal)> = lines.iter().filter(|line| line.0 == "1404").collect();
        assert_eq!(variance.len(), 3, "逐成本对象成行");
        assert_eq!(
            variance.iter().map(|line| line.1).sum::<Decimal>(),
            dec("300"),
            "有利差异在借方"
        );
        assert_eq!(
            variance.iter().map(|line| line.2).sum::<Decimal>(),
            dec("1200"),
            "不利差异在贷方"
        );
        // 对方科目 5401：借 1200 / 贷 300
        let counterpart: Vec<&(String, Decimal, Decimal)> = lines.iter().filter(|line| line.0 == "5401").collect();
        assert_eq!(counterpart.len(), 2);
        assert_eq!(counterpart.iter().map(|line| line.1).sum::<Decimal>(), dec("1200"));
        assert_eq!(counterpart.iter().map(|line| line.2).sum::<Decimal>(), dec("300"));

        // 反查：该会计期间的差异凭证只有这一张
        let period_id: Uuid = db()
            .query_first_sql(
                "SELECT id FROM biz_accounting_period WHERE period_code = ?".to_string(),
                vec![Value::Text(fixture.period_code.clone())],
            )
            .await
            .expect("查期间 id")
            .expect("期间行")
            .get_typed("id")
            .expect("期间 id");
        let ids = cv::voucher_ids_of(db(), period_id).await.expect("反查");
        assert_eq!(ids, vec![posted.voucher_id]);

        fixture.cleanup().await;
        println!("[M5-16 ①] 真库差异凭证：1 张 / 5 行 / Σ借=Σ贷=1500 ✓");
    }

    /// ② 幂等：同期间第二次 ⇒ 409 + 66112，且不新增凭证。
    #[tokio::test]
    async fn second_post_is_rejected_without_writing() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let draft = cv::build_draft(&sample(&fixture.period_code)).expect("草案");
        let first = cv::post_period_voucher(db(), &fixture.context(), &draft)
            .await
            .expect("首次落库")
            .expect("凭证");
        let error = cv::post_period_voucher(db(), &fixture.context(), &draft)
            .await
            .expect_err("第二次必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_VARIANCE_VOUCHER_DUPLICATE);
        let count: i64 = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_voucher \
                  WHERE source_type = 'COST_VARIANCE' AND source_no = ?"
                    .to_string(),
                vec![Value::Text(fixture.period_code.clone())],
            )
            .await
            .expect("查条数")
            .expect("count 行")
            .get_typed("n")
            .expect("count");
        assert_eq!(count, 1, "被拒的第二次不能落凭证");
        assert_eq!(header(&fixture.period_code).await.expect("单头").id, first.voucher_id);
        fixture.cleanup().await;
        println!("[M5-16 ②] 真库幂等闸：409 + 66112，凭证仍是 1 张 ✓");
    }

    /// ③ 空草案（没有非零差异）⇒ 不发空凭证。
    #[tokio::test]
    async fn empty_draft_posts_nothing() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let empty = PeriodVarianceAllocOutcome {
            period_code: fixture.period_code.clone(),
            outcomes: Vec::new(),
        };
        let draft = cv::build_draft(&empty).expect("草案");
        let posted = cv::post_period_voucher(db(), &fixture.context(), &draft)
            .await
            .expect("空草案不算失败");
        assert!(posted.is_none(), "没有差异就不出凭证");
        assert!(header(&fixture.period_code).await.is_none());
        fixture.cleanup().await;
        println!("[M5-16 ③] 真库空草案：0 张凭证（不造空单）✓");
    }

    /// ④ 期间已结账 ⇒ 被期间闸拦下，且不留半张凭证。
    #[tokio::test]
    async fn closed_period_is_rejected_without_writing() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        db().exec_sql(
            "UPDATE biz_accounting_period SET status = 'CLOSED' WHERE period_code = ?".to_string(),
            vec![Value::Text(fixture.period_code.clone())],
        )
        .await
        .expect("关期");
        let draft = cv::build_draft(&sample(&fixture.period_code)).expect("草案");
        let error = cv::post_period_voucher(db(), &fixture.context(), &draft)
            .await
            .expect_err("已结账期间必须拒绝");
        assert!(!format!("{error:?}").is_empty());
        assert!(header(&fixture.period_code).await.is_none(), "被拒的凭证不能留半张");
        db().exec_sql(
            "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = ?".to_string(),
            vec![Value::Text(fixture.period_code.clone())],
        )
        .await
        .expect("回开期间（清理前置）");
        fixture.cleanup().await;
        println!("[M5-16 ④] 真库期间闸：已结账期间拒绝落差异凭证，零残留 ✓");
    }

    /// 科目表前置：1404 / 5401 必须存在且是末级（V0183 / V0377 种下的）。
    #[tokio::test]
    async fn variance_accounts_exist_and_are_leaf() {
        if !ensure_db().await {
            return;
        }
        for code in ["1404", "5401"] {
            let row = db()
                .query_first_sql(
                    "SELECT account_code, is_leaf, status FROM biz_account \
                      WHERE account_code = ? AND deleted_at IS NULL"
                        .to_string(),
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("查科目")
                .unwrap_or_else(|| panic!("科目 {code} 不存在（V0183 标准科目表应已种）"));
            let is_leaf: i16 = row.get_typed("is_leaf").expect("is_leaf");
            let status: i16 = row.get_typed("status").expect("status");
            assert_eq!(is_leaf, 1, "差异凭证只能记末级科目（{code}）");
            assert_eq!(status, 1, "科目 {code} 必须启用");
        }
        assert_eq!(VARIANCE_TYPES_ALL.len(), 6);
        println!("[M5-16 ⑤] 科目前置：1404 / 5401 存在且为启用的末级科目 ✓");
    }
}
