// ============================================================================
// rebate_settle_db_tests.rs —— 销售返利结算（M3-16）的真库用例
//
// # 覆盖什么（每条都对应卡面 / 迁移 / DTO 注释里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `by_qty_settlement_end_to_end` | 按量：净销量（订单 100 − 退货 20）× 0.5 = 40；单号 `RB-`、状态 DRAFT、快照带档位、客户名 JOIN 回来 |
// | `by_amount_settlement_uses_percent` | 按额：净销售额 1000 × 2% = 20（含税口径） |
// | `tiered_settlement_picks_hit_tier` | 阶梯：150 件落第 2 档、按该档 2 元/件全额乘 = 300（不累进） |
// | `draft_orders_and_cancelled_returns_are_excluded` | 基数白名单：DRAFT 订单不算、未过账退货不减、期间外单据不算 |
// | `duplicate_settlement_is_66092` | 同客户同期间第二张 → 409 + 66092，且库里只有 1 行 |
// | `policy_not_covering_period_is_66091` | 期间无政策 / 政策只有半期 → 409 + 66091（不静默算） |
// | `recalc_refreshes_base_and_amount` | 新增订单后重算：金额更新、政策快照刷新；`SETTLED` 后重算 → 409 |
// | `cancel_allows_reopen_and_blocks_settled` | 作废 → CANCELLED（行留着）；同期间可重开；`SETTLED` 不能作废 |
// | `run_settles_all_covering_customers_and_is_idempotent` | 跑批：2 客户各建一张；再跑一遍全 EXISTS（幂等） |
// | `policy_delete_blocked_when_referenced` | 政策被未作废结算单引用 → 删政策 409；作废后可删 |
// | `concurrent_settle_only_one_wins` | 并发：两个请求同时结算同客户同期间，只有一个成功 |
// | `zz_teardown_purges_fixtures` | 清场：`M316-` 客户的结算单 / 政策 0 条 |
//
// # 为什么夹具直调 `handler::*_of`
//
// 写入路径就是本卡的四个动作（试算 / 计算落库 / 重算 / 作废），没有旁路可绕；
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试不许手写主数据 INSERT），
// 只有**单据**（订单 / 退货）是测试自己插的 —— 那是被结算的输入，不是主数据。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池；本文件复用 `aging_db_tests::DB_LOCK`（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m316_rebate_settle" cargo test --lib \
//   business::erp::sales::rebate_settle -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::sales::rebate_dto::{
    RULE_BY_AMOUNT, RULE_BY_QTY, RULE_TIERED, RebatePolicyWriteReq, RebateRuleReq, STATUS_DISABLED,
};
use crate::business::erp::sales::rebate_handler as policy_handler;
use crate::business::erp::sales::rebate_settle_dto::{self as dto, STATUS_CANCELLED, STATUS_DRAFT};
use crate::business::erp::sales::rebate_settle_handler as handler;
use crate::business::erp::sales::rebate_settle_model as model;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具客户编码前缀（清场判据）。
const CUS_PREFIX: &str = "M316-";
/// 夹具仓库编码。
const WH_CODE: &str = "M316-WH";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
}

/// 夹具客户（主数据走 `test_seed`）。
async fn customer(code: &str) -> Uuid {
    test_seed::ensure_customer(code, &format!("返利结算夹具 {code}")).await
}

/// 夹具销售订单 + 一行明细（金额 = 含税 = 本位币；税率 0 ⇒ 价税恒等成立）。
///
/// 插完立刻断言 `base_total_with_tax`：漏了就落回原币，多币种用例会静默算错。
async fn sell(customer_id: Uuid, date: &str, status: &str, taxed: &str, qty: &str) -> Uuid {
    let so_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_sales_order \
                (id, so_no, customer_id, order_date, status, total_amount, total_tax, \
                 total_with_tax, base_total_amount, base_total_tax, base_total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::date, ?, ?::text::numeric, 0, ?::text::numeric, \
                 ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(&so_id),
            Value::from(format!("M316-SO-{}", &so_id.to_string()[24..])),
            Value::from(&customer_id),
            Value::Text(date.to_string()),
            Value::Text(status.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    let material = test_seed::ensure_material("M316-MAT", "返利夹具物料").await;
    exec(
        "INSERT INTO biz_sales_order_item \
                (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
         VALUES (?::uuid, 1, ?::uuid, ?::text::numeric, 1, ?::text::numeric, 0, 0, 0)",
        vec![
            Value::from(&so_id),
            Value::from(&material),
            Value::Text(qty.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    so_id
}

/// 夹具销售退货 + 一行明细。
async fn give_back(customer_id: Uuid, date: &str, status: &str, taxed: &str, qty: &str) -> Uuid {
    let return_id = Uuid::now_v7();
    let warehouse = test_seed::ensure_warehouse(WH_CODE, "返利夹具仓").await;
    exec(
        "INSERT INTO biz_sales_return \
                (id, return_no, customer_id, return_date, return_type, warehouse_id, status, \
                 total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::date, 'OTHER', ?::uuid, ?, \
                 ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(&return_id),
            Value::from(format!("M316-SR-{}", &return_id.to_string()[24..])),
            Value::from(&customer_id),
            Value::Text(date.to_string()),
            Value::from(&warehouse),
            Value::Text(status.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    let material = test_seed::ensure_material("M316-MAT", "返利夹具物料").await;
    exec(
        "INSERT INTO biz_sales_return_item \
                (return_id, material_id, qty, price, amount, tax_rate, tax_amount) \
         VALUES (?::uuid, ?::uuid, ?::text::numeric, 1, ?::text::numeric, 0, 0)",
        vec![
            Value::from(&return_id),
            Value::from(&material),
            Value::Text(qty.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    return_id
}

/// 政策入参（阶梯用 `tiers` 给档位；按量/按额只给一档）。
///
/// `unit` 只对 `TIERED` 有意义（阶梯必须显式给计价单位 —— M3-15 的闸），
/// 按量/按额传 `None` 由服务端按类型补默认单位。
fn policy_req(
    customer_id: Uuid,
    rule_type: &str,
    unit: Option<&str>,
    settle_period: &str,
    from: &str,
    to: Option<&str>,
    tiers: &[(&str, Option<&str>, &str)],
) -> RebatePolicyWriteReq {
    RebatePolicyWriteReq {
        policy_name: format!("M316 {rule_type} 返利"),
        customer_id: customer_id.to_string(),
        rule_type: rule_type.to_string(),
        rate_unit: unit.map(str::to_string),
        settle_period: settle_period.to_string(),
        effective_from: from.to_string(),
        effective_to: to.map(str::to_string),
        status: None,
        remark: Some("M3-16 夹具".to_string()),
        rules: tiers
            .iter()
            .map(|(from, to, rate)| RebateRuleReq {
                tier_from: Some(dec(from)),
                tier_to: to.map(dec),
                rate: Some(dec(rate)),
                remark: None,
            })
            .collect(),
    }
}

/// 清场：物理删本模块造的结算单 / 政策 / 单据（主数据留给 `test_seed` 复用）。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_sales_rebate_settlement WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_sales_rebate_policy WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    // 单据：明细随单头 CASCADE（V0264 / V0303 的硬外键）
    exec(
        &format!(
            "DELETE FROM biz_sales_return WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_sales_order WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("UPDATE biz_customer SET status = 'ACTIVE' WHERE code LIKE '{CUS_PREFIX}%'"),
        vec![],
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 该客户在库里的结算单条数（含已作废：断言「行留着」）。
async fn settle_count(customer_id: Uuid) -> i64 {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_sales_rebate_settlement WHERE customer_id = ?::uuid".to_string(),
            vec![Value::from(&customer_id)],
        )
        .await
        .expect("计数");
    rows.first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("取计数")
        .unwrap_or(0)
}

fn req(customer_id: Uuid, settle_period: &str, period_code: &str) -> dto::RebateSettleReq {
    dto::RebateSettleReq {
        customer_id: customer_id.to_string(),
        settle_period: settle_period.to_string(),
        period_code: period_code.to_string(),
        remark: None,
    }
}

// ==================== 用例 ====================

/// 按量返利：净销量（订单 100 − 退货 20）× 0.5 = 40，单号 / 状态 / 快照 / 客户名全对。
#[tokio::test]
async fn by_qty_settlement_end_to_end() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C01").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建按量政策");
    sell(cus, "2026-10-05", "APPROVED", "600", "60").await;
    sell(cus, "2026-10-20", "OUTBOUND", "400", "40").await;
    give_back(cus, "2026-10-25", "POSTED", "200", "20").await;
    // 期间外 / 未确认的单据不参与
    sell(cus, "2026-09-30", "APPROVED", "999", "99").await;
    sell(cus, "2026-11-01", "APPROVED", "999", "99").await;

    let preview = handler::preview_of(req(cus, "MONTH", "2026-10")).await.expect("试算");
    assert_eq!(preview.period_from, day("2026-10-01"));
    assert_eq!(preview.period_to, day("2026-11-01"), "左闭右开");
    assert_eq!(preview.base.order_count, 2);
    assert_eq!(preview.base.order_qty, dec("100"));
    assert_eq!(preview.base.return_qty, dec("20"));
    assert_eq!(preview.base.net_qty, dec("80"));
    assert_eq!(preview.base.net_amount, dec("800.0000"));
    assert_eq!(preview.tier_seq, Some(1));
    assert_eq!(preview.rate, dec("0.5"));
    assert_eq!(preview.rebate_amount, dec("40.00"), "80 件 × 0.5 元/件");
    assert!(preview.existing.is_none(), "还没算过");
    assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);

    let saved = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("计算落库");
    assert!(saved.settlement_no.starts_with("RB-"), "{}", saved.settlement_no);
    assert!(model::parse_settlement_no(&saved.settlement_no).is_some());
    assert_eq!(saved.status, STATUS_DRAFT);
    assert_eq!(saved.rebate_amount, dec("40.00"));
    assert_eq!(saved.customer_name.as_deref(), Some("返利结算夹具 M316-C01"));
    assert_eq!(saved.policy_tiers.len(), 1, "快照带全部档位");
    assert_eq!(saved.policy_tiers[0].rate, dec("0.5"));
    assert_eq!(saved.settle_period_label, "月度");
    assert_eq!(saved.period_last_day, day("2026-10-31"));

    // 回读：详情与落库结果一致（快照从 JSONB 解析回来）
    let detail = handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(detail.settlement_no, saved.settlement_no);
    assert_eq!(detail.base.net_qty, dec("80"));
    assert_eq!(detail.policy_no, saved.policy_no);

    println!(
        "[M3-16] 按量结算：单据 2 张/100 件、退货 20 件 → 净 80 × 0.5 = 40.00，单号 {} ✓",
        saved.settlement_no
    );
}

/// 按额返利：净销售额（含税、本位币）1000 × 2% = 20。
#[tokio::test]
async fn by_amount_settlement_uses_percent() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C02").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_AMOUNT,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "2")],
        ),
    )
    .await
    .expect("建按额政策");
    sell(cus, "2026-10-08", "APPROVED", "1000", "10").await;

    let saved = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("按额结算");
    assert_eq!(saved.rate_unit, "PERCENT");
    assert_eq!(saved.rate_unit_label, "销售额百分比");
    assert_eq!(saved.base.net_amount, dec("1000.0000"));
    assert_eq!(saved.rebate_amount, dec("20.00"), "1000 × 2% = 20");

    // 只按额不按量：销量基数不进公式（10 件不影响金额）
    assert_eq!(saved.base.net_qty, dec("10"));
    println!("[M3-16] 按额结算：净销售额 1000 × 2% = 20.00（销量不参与）✓");
}

/// 阶梯按量：150 件落第 2 档、按 2 元/件全额乘 = 300（不累进）。
#[tokio::test]
async fn tiered_settlement_picks_hit_tier() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C03").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_TIERED,
            Some("PER_UNIT"),
            "QUARTER",
            "2026-01-01",
            None,
            &[("0", Some("100"), "1"), ("100", None, "2")],
        ),
    )
    .await
    .expect("建阶梯政策");
    sell(cus, "2026-11-10", "OUTBOUND", "9000", "150").await;

    let saved = handler::settle_of(None, req(cus, "QUARTER", "2026-Q4"))
        .await
        .expect("季度阶梯结算");
    assert_eq!(saved.settle_period, "QUARTER");
    assert_eq!(saved.period_from, day("2026-10-01"));
    assert_eq!(saved.period_to, day("2027-01-01"), "跨年季度");
    assert_eq!(saved.tier_seq, Some(2), "150 件落第 2 档（100 是第 2 档下限）");
    assert_eq!(saved.tier_from, dec("100.0000"));
    assert_eq!(saved.tier_to, None, "末档无上限");
    assert_eq!(saved.rate, dec("2"));
    assert_eq!(
        saved.rebate_amount,
        dec("300.00"),
        "不累进：150 × 2 = 300（不是 100×1+50×2）"
    );
    assert_eq!(saved.policy_tiers.len(), 2, "快照带两档");
    println!("[M3-16] 阶梯（季度）：150 件落第 2 档 × 2 元/件 = 300.00（不累进）✓");
}

/// 基数白名单：DRAFT 订单不算、未过账退货不减、期间外不算。
#[tokio::test]
async fn draft_orders_and_cancelled_returns_are_excluded() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C04").await;
    policy_handler::create_policy_of(
        None,
        policy_req(cus, RULE_BY_QTY, None, "MONTH", "2026-01-01", None, &[("0", None, "1")]),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-03", "APPROVED", "500", "50").await;
    sell(cus, "2026-10-04", "DRAFT", "7000", "700").await; // 草稿不算销售
    sell(cus, "2026-10-05", "CANCELLED", "7000", "700").await; // 作废不算
    give_back(cus, "2026-10-10", "PENDING_POST", "800", "80").await; // 未过账不减
    give_back(cus, "2026-10-11", "CANCELLED", "800", "80").await; // 作废退货不减

    let preview = handler::preview_of(req(cus, "MONTH", "2026-10")).await.expect("试算");
    assert_eq!(preview.base.order_count, 1, "只有 APPROVED 那张算");
    assert_eq!(preview.base.order_qty, dec("50"));
    assert_eq!(preview.base.return_count, 0, "未过账/作废退货都不减");
    assert_eq!(preview.base.net_qty, dec("50"));
    assert_eq!(preview.rebate_amount, dec("50.00"));
    println!("[M3-16] 基数白名单：DRAFT/CANCELLED 订单、PENDING_POST 退货全不计 ✓");
}

/// 同客户同期间第二张 → 409 + 66092，库里只有 1 行（唯一索引兜底）。
#[tokio::test]
async fn duplicate_settlement_is_66092() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C05").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-09", "APPROVED", "300", "30").await;

    let first = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("第一张");
    let second = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect_err("第二张必须被拒");
    assert_eq!(second.status, 409);
    assert_eq!(second.code, error_code::ERP_REBATE_SETTLEMENT_DUPLICATE);
    assert!(
        second.msg.contains(&first.settlement_no),
        "报文要点名已有那张：{}",
        second.msg
    );
    assert_eq!(settle_count(cus).await, 1, "库里只能有 1 行");

    // 试算会回显「已经算过」，帮前端避免撞 409
    let preview = handler::preview_of(req(cus, "MONTH", "2026-10")).await.expect("试算");
    assert_eq!(preview.existing.map(|row| row.settlement_no), Some(first.settlement_no));
    println!("[M3-16] 期间重复：第二张 409 + 66092、库里 1 行、试算回显已有单 ✓");
}

/// 期间无政策 / 政策只有半期 → 409 + 66091（不静默算）。
#[tokio::test]
async fn policy_not_covering_period_is_66091() {
    let Some(_guard) = setup().await else { return };
    // ① 完全没有政策
    let bare = customer("M316-C06").await;
    sell(bare, "2026-10-02", "APPROVED", "100", "10").await;
    let error = handler::settle_of(None, req(bare, "MONTH", "2026-10"))
        .await
        .expect_err("没有政策不能结算");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_SETTLEMENT_NO_POLICY);
    assert!(error.msg.contains("没有任何启用的月度返利政策"), "{}", error.msg);

    // ② 政策从 10-15 才生效：期间首日没有政策 → 报文点名「最近一份政策从哪天起」
    let half = customer("M316-C07").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            half,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-10-15",
            None,
            &[("0", None, "1")],
        ),
    )
    .await
    .expect("建半期政策");
    sell(half, "2026-10-20", "APPROVED", "100", "10").await;
    let error = handler::settle_of(None, req(half, "MONTH", "2026-10"))
        .await
        .expect_err("首日没有政策不能结算");
    assert_eq!(error.code, error_code::ERP_REBATE_SETTLEMENT_NO_POLICY);
    assert!(error.msg.contains("尚未覆盖期间首日"), "{}", error.msg);
    assert!(error.msg.contains("2026-10-15"), "报文要点名政策生效日：{}", error.msg);

    // ③ 政策 10-01 ~ 10-20 生效：首日有政策但没覆盖到期间末日 → 报文点名缺口日期
    let partial = customer("M316-C14").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            partial,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-10-01",
            Some("2026-10-20"),
            &[("0", None, "1")],
        ),
    )
    .await
    .expect("建短有效期政策");
    sell(partial, "2026-10-10", "APPROVED", "100", "10").await;
    let error = handler::settle_of(None, req(partial, "MONTH", "2026-10"))
        .await
        .expect_err("半期政策不能覆盖整月");
    assert_eq!(error.code, error_code::ERP_REBATE_SETTLEMENT_NO_POLICY);
    assert!(error.msg.contains("没有覆盖整个结算期间"), "{}", error.msg);
    assert!(error.msg.contains("2026-10-31"), "报文要点名缺口日期：{}", error.msg);

    // ③ 换到政策真正覆盖的期间（2026-11）就能算
    sell(half, "2026-11-10", "APPROVED", "200", "20").await;
    let ok = handler::settle_of(None, req(half, "MONTH", "2026-11"))
        .await
        .expect("11 月整期覆盖");
    assert_eq!(ok.rebate_amount, dec("20.00"), "20 件 × 1 元/件");

    // ④ 试算对「没有政策」不报错，而是 policy=null + 提示（试算的作用之一就是看有没有政策）
    let preview = handler::preview_of(req(bare, "MONTH", "2026-10"))
        .await
        .expect("试算不因缺政策报错");
    assert!(preview.policy.is_none());
    assert_eq!(preview.rebate_amount, Decimal::ZERO);
    assert!(
        preview
            .warnings
            .iter()
            .any(|text| text.contains("没有任何启用的月度返利政策"))
    );
    println!("[M3-16] 缺政策 / 半期政策 → 结算 409 + 66091（点名缺口日期）；试算降级成提示 ✓");
}

/// 重算：新增订单后金额刷新；`SETTLED` 之后不能重算。
#[tokio::test]
async fn recalc_refreshes_base_and_amount() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C08").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-06", "APPROVED", "600", "60").await;

    let first = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("首算");
    assert_eq!(first.rebate_amount, dec("30.00"), "60 × 0.5");

    // 又发了一批货：重算必须跟上
    sell(cus, "2026-10-28", "OUTBOUND", "400", "40").await;
    let refreshed = handler::recalc_of(None, &first.id, Some("补发货后重算"))
        .await
        .expect("重算");
    assert_eq!(refreshed.id, first.id, "重算是同一张单");
    assert_eq!(refreshed.settlement_no, first.settlement_no, "单号不变");
    assert_eq!(refreshed.base.net_qty, dec("100"));
    assert_eq!(refreshed.rebate_amount, dec("50.00"), "100 × 0.5");
    assert_eq!(refreshed.remark.as_deref(), Some("补发货后重算"));

    // 政策改档位后重算：快照必须跟着变（金额按新口径、说明也要按新口径）
    let policy_id = refreshed.policy_id.clone();
    policy_handler::update_policy_of(
        None,
        &policy_id,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.8")],
        ),
    )
    .await
    .expect("改档位");
    let repriced = handler::recalc_of(None, &first.id, None).await.expect("重算");
    assert_eq!(repriced.rate, dec("0.8"), "快照换成新比率");
    assert_eq!(repriced.rebate_amount, dec("80.00"), "100 × 0.8");

    // 转应付（M3-17 的动作，这里直接改库模拟）后不能重算
    exec(
        "UPDATE biz_sales_rebate_settlement SET status = 'SETTLED' WHERE id = ?::uuid",
        vec![Value::from(Uuid::parse_str(&first.id).expect("结算单 id 是 UUID"))],
    )
    .await;
    let error = handler::recalc_of(None, &first.id, None)
        .await
        .expect_err("SETTLED 不能重算");
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("已转应付"), "{}", error.msg);
    println!("[M3-16] 重算：基数/金额/政策快照一起刷新；SETTLED 后重算 409 ✓");
}

/// 作废：`DRAFT → CANCELLED`（行留着）、同期间可重开；`SETTLED` 不能作废。
#[tokio::test]
async fn cancel_allows_reopen_and_blocks_settled() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C09").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-07", "APPROVED", "200", "20").await;

    let first = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("首算");
    handler::cancel_of(None, &first.id, Some("算错了，重来"))
        .await
        .expect("作废");
    let after = handler::detail_settlement_of(&first.id).await.expect("作废后仍可查");
    assert_eq!(after.status, STATUS_CANCELLED, "行留着、状态改掉");
    assert_eq!(after.status_label, "已作废");
    assert_eq!(after.remark.as_deref(), Some("算错了，重来"));
    assert_eq!(settle_count(cus).await, 1, "作废不删行");

    // 同期间重开一张（唯一索引排除 CANCELLED）
    let second = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("作废后可以重开");
    assert_ne!(second.id, first.id);
    assert_eq!(second.status, STATUS_DRAFT);
    assert_eq!(settle_count(cus).await, 2, "两张并存（一张作废一张有效）");

    // 已作废的再作废 → 409（不是 DRAFT）
    let again = handler::cancel_of(None, &first.id, None)
        .await
        .expect_err("已作废不能再作废");
    assert_eq!(again.status, 409);

    // 转应付后不能作废
    exec(
        "UPDATE biz_sales_rebate_settlement SET status = 'SETTLED' WHERE id = ?::uuid",
        vec![Value::from(Uuid::parse_str(&second.id).expect("结算单 id 是 UUID"))],
    )
    .await;
    let blocked = handler::cancel_of(None, &second.id, Some("想撤销"))
        .await
        .expect_err("SETTLED 不能作废");
    assert_eq!(blocked.status, 409);
    assert!(blocked.msg.contains("红冲"), "{}", blocked.msg);
    println!("[M3-16] 作废：DRAFT→CANCELLED（行留着）、同期间可重开、SETTLED 不可作废 ✓");
}

/// 跑批：覆盖该期间的客户各建一张；再跑一遍全 EXISTS（幂等）。
#[tokio::test]
async fn run_settles_all_covering_customers_and_is_idempotent() {
    let Some(_guard) = setup().await else { return };
    let a = customer("M316-C10").await;
    let b = customer("M316-C11").await;
    policy_handler::create_policy_of(
        None,
        policy_req(a, RULE_BY_QTY, None, "MONTH", "2026-01-01", None, &[("0", None, "1")]),
    )
    .await
    .expect("建政策 A");
    policy_handler::create_policy_of(
        None,
        policy_req(
            b,
            RULE_BY_AMOUNT,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "5")],
        ),
    )
    .await
    .expect("建政策 B");
    sell(a, "2026-10-12", "APPROVED", "1000", "100").await;
    sell(b, "2026-10-13", "APPROVED", "1000", "10").await;

    let run = handler::run_of(
        None,
        dto::RebateRunReq {
            settle_period: "MONTH".to_string(),
            period_code: "2026-10".to_string(),
            customer_ids: vec![a.to_string(), b.to_string()],
            remark: Some("10 月返利跑批".to_string()),
        },
    )
    .await
    .expect("跑批");
    assert_eq!(run.total, 2, "两个客户各有覆盖整期的政策");
    assert_eq!(run.created, 2);
    assert_eq!(run.skipped, 0);
    assert_eq!(run.failed, 0);
    assert!(run.items.iter().all(|item| item.outcome == "CREATED"));
    let sum: Decimal = run.items.iter().filter_map(|item| item.rebate_amount).sum();
    assert_eq!(sum, dec("150.00"), "A 100×1 + B 1000×5% = 100 + 50");

    // 再跑一遍：全 EXISTS（幂等，不重复算）
    let again = handler::run_of(
        None,
        dto::RebateRunReq {
            settle_period: "MONTH".to_string(),
            period_code: "2026-10".to_string(),
            customer_ids: vec![a.to_string(), b.to_string()],
            remark: None,
        },
    )
    .await
    .expect("二次跑批");
    assert_eq!(again.created, 0);
    assert_eq!(again.skipped, 2, "已有结算单的客户跳过");
    assert!(again.items.iter().all(|item| item.outcome == "EXISTS"));
    assert_eq!(settle_count(a).await + settle_count(b).await, 2, "总数没变");
    println!("[M3-16] 跑批：2 客户 CREATED（合计 150.00），二次跑批全 EXISTS（幂等）✓");
}

/// 政策被未作废结算单引用 → 删政策 409；作废后可删。
#[tokio::test]
async fn policy_delete_blocked_when_referenced() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C12").await;
    let policy = policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-14", "APPROVED", "200", "20").await;
    let settle = handler::settle_of(None, req(cus, "MONTH", "2026-10"))
        .await
        .expect("结算");

    let blocked = policy_handler::delete_policy_of(None, &policy.id)
        .await
        .expect_err("被引用不能删");
    assert_eq!(blocked.status, 409);
    assert!(blocked.msg.contains("停用"), "报文要给出正确出路：{}", blocked.msg);

    // 停用是允许的（正确退役方式）
    let mut disable = policy_req(
        cus,
        RULE_BY_QTY,
        None,
        "MONTH",
        "2026-01-01",
        None,
        &[("0", None, "0.5")],
    );
    disable.status = Some(STATUS_DISABLED);
    policy_handler::update_policy_of(None, &policy.id, disable)
        .await
        .expect("停用可用");

    // 作废结算单之后政策可以删
    handler::cancel_of(None, &settle.id, Some("不再返利"))
        .await
        .expect("作废结算");
    policy_handler::delete_policy_of(None, &policy.id)
        .await
        .expect("引用都作废后可以删");
    println!("[M3-16] 政策引用闸：被结算引用 409（提示改停用）；作废结算后可删 ✓");
}

/// 并发：两个请求同时结算同客户同期间，只有一个成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_settle_only_one_wins() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M316-C13").await;
    policy_handler::create_policy_of(
        None,
        policy_req(
            cus,
            RULE_BY_QTY,
            None,
            "MONTH",
            "2026-01-01",
            None,
            &[("0", None, "0.5")],
        ),
    )
    .await
    .expect("建政策");
    sell(cus, "2026-10-15", "APPROVED", "200", "20").await;

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let barrier = barrier.clone();
        let req = req(cus, "MONTH", "2026-10");
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            handler::settle_of(None, req).await
        }));
    }
    let mut ok = 0;
    let mut conflicts = 0;
    for task in tasks {
        match task.await.expect("任务不 panic") {
            Ok(_) => ok += 1,
            Err(error) => {
                assert_eq!(
                    error.code,
                    error_code::ERP_REBATE_SETTLEMENT_DUPLICATE,
                    "并发输家必须是 66092（不是 500）：{} / {}",
                    error.code,
                    error.msg
                );
                conflicts += 1;
            }
        }
    }
    assert_eq!(ok, 1, "只能有一个成功");
    assert_eq!(conflicts, 1, "另一个必须是 409 + 66092");
    assert_eq!(settle_count(cus).await, 1, "库里只有一张");
    println!("[M3-16] 并发结算同客户同期间：1 成功 + 1 个 409/66092，库里 1 行 ✓");
}

/// 清场：`M316-` 客户的结算单 / 政策 0 条（单据也清掉，避免污染别的用例）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let rows = db()
        .query_sql(
            format!(
                "SELECT (SELECT count(*) FROM biz_sales_rebate_settlement s JOIN biz_customer c ON c.id = s.customer_id \
                          WHERE c.code LIKE '{CUS_PREFIX}%') AS n_settle, \
                        (SELECT count(*) FROM biz_sales_rebate_policy p JOIN biz_customer c ON c.id = p.customer_id \
                          WHERE c.code LIKE '{CUS_PREFIX}%') AS n_policy"
            ),
            vec![],
        )
        .await
        .expect("清场自检");
    let row = rows.first().expect("有行");
    let n_settle: i64 = row.get_typed("n_settle").expect("settle 计数");
    let n_policy: i64 = row.get_typed("n_policy").expect("policy 计数");
    assert_eq!(n_settle, 0, "M316 结算单应清零");
    assert_eq!(n_policy, 0, "M316 政策应清零");
    println!("[M3-16] 清场：M316 夹具结算单 / 政策均为 0 ✓");
}
