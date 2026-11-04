//! 领域 2：收货单全链路（16 个用例，S01–S16）。
//!
//! # 为什么这些用例不能写在 `stockin_handler` 的 `#[cfg(test)] mod` 里
//!
//! 模块内的用例只管「收货单自己」：建单 / 过账 / 作废各写一遍，断言各自那一段。
//! 本文件问的是**另一组问题**：一张收货单的状态变更，会同时牵动 **6 张异物**
//! （库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号）。任何一张没跟上，账实就不符，
//! 而单模块用例看不见「没跟上的那一张」。
//!
//! ```text
//! DRAFT ──过账──▶ POSTED ──作废──▶ CANCELLED
//!   │                │                │
//!   │                ├─ biz_inventory          +qty  ──▶ 退回基线
//!   │                ├─ biz_inventory_location +qty  ──▶ 退回基线
//!   │                ├─ biz_inventory_txn      IN     ──▶ 追加 OUT/PURCHASE_IN_CANCEL
//!   │                ├─ biz_voucher            蓝字   ──▶ 红字 + reversed_by 回写
//!   │                ├─ biz_payable            OPEN   ──▶ CANCELLED
//!   │                └─ biz_material_serial    IN_STOCK ─▶ 软删
//! ```
//!
//! 每条正向（该发生的发生了）都配一条反向（**不该动的没动**）；S12 是总闸：
//! 作废后四账必须逐项回到过账前的基线，而不是「差不多」。
//!
//! 夹具与连库见 [`crate::linkage::common`]；所有断言按本用例的物料 / 仓库 / 单据 id
//! 过滤，不做全局 `COUNT(*)`（共享测试库会漂数）。

use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::erp::stockin_dto::{
    CreateStockInReq, StockInItemReq, StockInLineSerials, StockInStatusReq,
};
use cloudpivot::business::erp::stockin_handler::{change_stock_in_status, create_stock_in};

use crate::linkage::common::{self, Ctx};

// ==================== 夹具动作 ====================

/// 一行收货明细，**自动带库位库存行**。
///
/// 坑（实测踩过）：明细的 `location_id` 指的是 `biz_inventory_location.id`（库位库存行），
/// 不是库位字典 `biz_location.id`；传字典 id 会 400 + `66002 库位库存行不存在`。
/// 所以每行先建一行数量 0 的库位库存行（[`Ctx::loc_row`]），再把它挂到明细上。
async fn line(ctx: &Ctx, material: Uuid, qty: &str, price: &str, rate: &str) -> StockInItemReq {
    let loc = ctx.loc_row(material).await;
    StockInItemReq {
        purchase_order_item_id: None,
        material_id: Some(material.to_string()),
        qty: Some(common::dec(qty)),
        price_excl: Some(common::dec(price)),
        tax_rate: Some(common::dec(rate)),
        location_id: Some(loc.to_string()),
        location_dict_id: None,
        serial_ids: Vec::new(),
        remark: None,
    }
}

/// 一行收货明细（**不带库位**）：用于验证「只有成本账、没有库位账」的分支。
fn line_no_loc(material: Uuid, qty: &str, price: &str, rate: &str) -> StockInItemReq {
    StockInItemReq {
        purchase_order_item_id: None,
        material_id: Some(material.to_string()),
        qty: Some(common::dec(qty)),
        price_excl: Some(common::dec(price)),
        tax_rate: Some(common::dec(rate)),
        location_id: None,
        location_dict_id: None,
        serial_ids: Vec::new(),
        remark: None,
    }
}

/// 建单请求（供应商 / 仓库取本用例夹具）。
fn req(ctx: &Ctx, items: Vec<StockInItemReq>) -> CreateStockInReq {
    CreateStockInReq {
        source_order_id: None,
        purchase_order_id: None,
        // A4（V0199）起：PURCHASE 且无来源采购单 = 无源单据，必须显式声明（remark 已给）
        is_sourceless: Some(true),
        supplier_id: Some(ctx.supplier.to_string()),
        warehouse_id: Some(ctx.wh_from.to_string()),
        in_date: None,
        invoice_received: None,
        remark: Some(format!("{} 联动矩阵", ctx.tag)),
        items,
        // 只读派生字段（P10 起 DTO 显式收下这三个，用于识别「客户端试图改金额」）：
        // 建单时一律不给，交给服务端由明细算。
        total_qty: None,
        total_amount: None,
        total_tax: None,
    }
}

fn post_req() -> StockInStatusReq {
    StockInStatusReq {
        status: Some("POSTED".to_string()),
        reason: None,
        serials: Vec::new(),
        // ARCH-3 DC-01：不请求跳过审批（默认行为不变）
        approval: Default::default(),
    }
}

fn cancel_req(reason: Option<&str>) -> StockInStatusReq {
    StockInStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: reason.map(str::to_string),
        serials: Vec::new(),
        // ARCH-3 DC-01：不请求跳过审批（默认行为不变）
        approval: Default::default(),
    }
}

/// 建单（`DRAFT`）。
async fn draft(ctx: &Ctx, items: Vec<StockInItemReq>) -> Uuid {
    create_stock_in(&req(ctx, items), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("{}：建收货单失败 {} {}", ctx.tag, error.code, error.msg))
}

/// 建单 + 过账（`POSTED`），返回收货单 id。
async fn posted(ctx: &Ctx, items: Vec<StockInItemReq>) -> Uuid {
    let id = draft(ctx, items).await;
    change_stock_in_status(id, &post_req(), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("{}：过账失败 {} {}", ctx.tag, error.code, error.msg));
    id
}

/// 单据状态。
async fn status_of(id: Uuid) -> String {
    common::scalar_text(
        "SELECT status AS v FROM biz_stock_in WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap_or_else(|| panic!("收货单 {id} 不存在"))
}

/// 价税合计（库里口径）。
async fn with_tax_of(id: Uuid) -> rust_decimal::Decimal {
    common::scalar_dec(
        "SELECT total_with_tax::numeric(18,4) AS v FROM biz_stock_in WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
}

/// 本单产生的凭证 id（`source_id` 是弱引用，按单据 id 精确命中）。
async fn voucher_ids_of(stock_in_id: Uuid) -> Vec<Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_voucher \
              WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY created_at, voucher_no",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查凭证失败：{error}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("id").expect("凭证 id"))
        .collect()
}

/// 本单产生的应付单 `(id, status, amount, paid_amount)`。
async fn payable_of(
    stock_in_id: Uuid,
) -> Option<(Uuid, String, rust_decimal::Decimal, rust_decimal::Decimal)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status, amount, paid_amount FROM biz_payable \
              WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查应付单失败：{error}"));
    rows.first().map(|row| {
        (
            row.get_typed::<Uuid>("id").expect("应付 id"),
            row.get_typed::<String>("status").expect("应付状态"),
            row.get_typed::<rust_decimal::Decimal>("amount")
                .expect("应付金额"),
            row.get_typed::<rust_decimal::Decimal>("paid_amount")
                .expect("已付金额"),
        )
    })
}

/// 本单写下的流水条数（`doc_id` 锁定到本单，避免串到别的用例）。
async fn txn_count_of(stock_in_id: Uuid) -> i64 {
    common::scalar_i64(
        "SELECT count(*) AS v FROM biz_inventory_txn \
          WHERE doc_id = ?::uuid AND doc_type = 'STOCK_IN'",
        vec![Value::from(&stock_in_id)],
    )
    .await
}

/// 本单的反向流水条数。
async fn cancel_txn_count_of(stock_in_id: Uuid) -> i64 {
    common::scalar_i64(
        "SELECT count(*) AS v FROM biz_inventory_txn \
          WHERE doc_id = ?::uuid AND txn_type = 'OUT' AND biz_type = 'PURCHASE_IN_CANCEL'",
        vec![Value::from(&stock_in_id)],
    )
    .await
}

/// 未软删的序列号条数。
async fn serial_count_of(stock_in_id: Uuid) -> i64 {
    common::scalar_i64(
        "SELECT count(*) AS v FROM biz_material_serial \
          WHERE source_type = 'PURCHASE' AND source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&stock_in_id)],
    )
    .await
}

// ==================== S01–S05：过账的正向级联 ====================

/// S01 建单 → DRAFT → 过账 POSTED：库存 +qty、库位账 +qty、流水 1 条、凭证 1 张、应付 1 张。
#[tokio::test]
async fn s01_post_writes_all_six_ledgers() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S01").await;
    let l = line(&ctx, ctx.mat, "10", "2", "13").await;
    let id = draft(&ctx, vec![l]).await;
    assert_eq!(status_of(id).await, "DRAFT", "S01：新建必须是 DRAFT");

    let before = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    assert_eq!(
        before,
        (common::dec("0"), common::dec("0"), 0, 0),
        "S01 前置：过账前四账必须全零，实际 {before:?}"
    );

    change_stock_in_status(id, &post_req(), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S01：过账失败 {} {}", error.code, error.msg));
    assert_eq!(
        status_of(id).await,
        "POSTED",
        "S01：过账后状态必须是 POSTED"
    );

    let (inv, loc, txn, vou) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(inv, common::dec("10"), "S01：库存数量");
    common::assert_dec(loc, common::dec("10"), "S01：库位账数量");
    assert_eq!(txn, 1, "S01：过账必须写且只写 1 条入库流水");
    assert_eq!(vou, 1, "S01：过账必须生成 1 张凭证");
    assert_eq!(
        txn_count_of(id).await,
        1,
        "S01：流水的 doc_id 必须挂在本单上（否则「按单据反查流水」全断）"
    );

    let payable = payable_of(id)
        .await
        .unwrap_or_else(|| panic!("S01：过账必须生成应付单（采购入库的法定后果）"));
    assert_eq!(payable.1, "OPEN", "S01：新应付必须是 OPEN");
    common::assert_dec(payable.3, common::dec("0"), "S01：新应付已付金额");
    ctx.cleanup().await;
}

/// S02 过账后：凭证借贷平衡、分录科目正确（借 1405 / 借 2221.01 / 贷 2202）。
#[tokio::test]
async fn s02_voucher_is_balanced_with_expected_accounts() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S02").await;
    // 10 × 2 = 20 不含税；13% → 2.60 税；价税合计 22.60
    let l = line(&ctx, ctx.mat, "10", "2", "13").await;
    let id = posted(&ctx, vec![l]).await;
    let vouchers = voucher_ids_of(id).await;
    assert_eq!(vouchers.len(), 1, "S02：一张收货单只该生成 1 张凭证");
    let voucher_id = vouchers[0];

    let rows = db()
        .find_with_params(
            "SELECT a.account_code, e.debit_amount::numeric(18,4) AS debit, \
                    e.credit_amount::numeric(18,4) AS credit \
               FROM biz_voucher_entry e JOIN biz_account a ON a.id = e.account_id \
              WHERE e.voucher_id = ?::uuid ORDER BY a.account_code",
            vec![Value::from(&voucher_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("S02：查分录失败：{error}"));

    let mut debit_sum = common::dec("0");
    let mut credit_sum = common::dec("0");
    let mut by_code: Vec<(String, rust_decimal::Decimal, rust_decimal::Decimal)> = Vec::new();
    for row in &rows {
        let code: String = row.get_typed("account_code").expect("科目编码");
        let debit: rust_decimal::Decimal = row.get_typed("debit").expect("借方");
        let credit: rust_decimal::Decimal = row.get_typed("credit").expect("贷方");
        debit_sum += debit;
        credit_sum += credit;
        by_code.push((code, debit, credit));
    }
    common::assert_dec(debit_sum, credit_sum, "S02：Σ借 = Σ贷（借贷必须平衡）");

    let find = |code: &str| {
        by_code
            .iter()
            .find(|(c, _, _)| c == code)
            .map(|(_, d, c)| (*d, *c))
    };
    let (d1405, c1405) =
        find("1405").unwrap_or_else(|| panic!("S02：缺少 1405 库存商品分录：{by_code:?}"));
    common::assert_dec(
        d1405,
        common::dec("20"),
        "S02：借 1405 金额（10 × 2 不含税）",
    );
    common::assert_dec(c1405, common::dec("0"), "S02：1405 不许有贷方");
    let (d_tax, c_tax) =
        find("2221.01").unwrap_or_else(|| panic!("S02：缺少 2221.01 进项税分录：{by_code:?}"));
    common::assert_dec(d_tax, common::dec("2.6"), "S02：借 2221.01 进项税额");
    common::assert_dec(c_tax, common::dec("0"), "S02：2221.01 不许有贷方");
    let (d2202, c2202) =
        find("2202").unwrap_or_else(|| panic!("S02：缺少 2202 应付账款分录：{by_code:?}"));
    common::assert_dec(d2202, common::dec("0"), "S02：2202 不许有借方");
    common::assert_dec(c2202, common::dec("22.6"), "S02：贷 2202 价税合计");
    ctx.cleanup().await;
}

/// S03 过账后：应付金额 = 收货单价税合计。
#[tokio::test]
async fn s03_payable_amount_equals_stock_in_total() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S03").await;
    let l = line(&ctx, ctx.mat, "7", "3.5", "13").await;
    let id = posted(&ctx, vec![l]).await;
    let with_tax = with_tax_of(id).await;
    let (_, _, amount, _) = payable_of(id).await.expect("S03：必须生成应付单");
    common::assert_dec(amount, with_tax, "S03：应付金额 = 收货单价税合计");
    assert!(
        with_tax > common::dec("0"),
        "S03：价税合计必须为正，实际 {with_tax}"
    );
    ctx.cleanup().await;
}

/// S04 过账后：一物一码物料 → 生成序列号 N 个（N = 数量）。
#[tokio::test]
async fn s04_post_creates_serials_for_serial_material() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S04").await;
    let codes = vec![ctx.code("SN-1"), ctx.code("SN-2"), ctx.code("SN-3")];
    let mut item = line(&ctx, ctx.mat_sn, "3", "40", "0").await;
    item.serial_ids = codes.clone();
    let id = draft(&ctx, vec![item]).await;

    let mut post = post_req();
    post.serials = vec![StockInLineSerials {
        line_no: 1,
        serial_ids: codes.clone(),
    }];
    change_stock_in_status(id, &post, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S04：过账失败 {} {}", error.code, error.msg));

    assert_eq!(serial_count_of(id).await, 3, "S04：3 件必须生成 3 个序列号");
    let stored = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_material_serial \
          WHERE source_type = 'PURCHASE' AND source_id = ?::uuid AND status = 'IN_STOCK' \
            AND warehouse_id = ?::uuid",
        vec![Value::from(&id), Value::from(&ctx.wh_from)],
    )
    .await;
    assert_eq!(
        stored, 3,
        "S04：新生成的序列号必须是 IN_STOCK，且落在本用例仓库里"
    );
    // 反向：不变量 —— 序列号件数必须等于库存数量（一物一码的账实一致）
    let (inv, _, _, _) = common::four_ledger_snapshot(ctx.mat_sn, ctx.wh_from).await;
    common::assert_dec(
        inv,
        rust_decimal::Decimal::from(stored),
        "S04：一物一码物料的库存件数必须等于 IN_STOCK 序列号条数",
    );
    ctx.cleanup().await;
}

/// S05 过账后：不变量 B1（库位账合计 = 成本账数量）通过。
#[tokio::test]
async fn s05_invariant_b1_location_equals_cost() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S05").await;
    // 两行：同一物料、同一库位（库位账必须累加而不是覆盖）
    let first = line(&ctx, ctx.mat, "6", "2", "0").await;
    let second = line(&ctx, ctx.mat, "4", "2", "0").await;
    let _id = posted(&ctx, vec![first, second]).await;
    let (inv, loc, _, _) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(inv, common::dec("10"), "S05：成本账数量 = 6 + 4");
    common::assert_dec(loc, inv, "S05：B1 不变量 —— 库位账合计必须等于成本账数量");

    // 反向：不带库位的行进「无库位账行」（`location_id IS NULL` 的库位账行），
    // 不污染带库位的那一行 —— B1 在两种情形下都必须成立。
    let _plain = posted(&ctx, vec![line_no_loc(ctx.mat, "5", "2", "0")]).await;
    let (inv2, loc2, _, _) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(inv2, common::dec("15"), "S05：成本账 = 10 + 5");
    common::assert_dec(loc2, inv2, "S05：B1 在不带库位的行上同样成立（15 = 15）");
    let no_loc_row = common::scalar_dec(
        "SELECT COALESCE(sum(qty),0)::numeric(18,4) AS v FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&ctx.mat), Value::from(&ctx.wh_from)],
    )
    .await;
    common::assert_dec(
        no_loc_row,
        common::dec("5"),
        "S05：无库位行只该进「无库位」账行（不该混进带库位的那一行）",
    );
    ctx.cleanup().await;
}

// ==================== S06–S11：作废的逐项级联 ====================

/// S06 作废（带 reason）→ 库存回滚到 0。
#[tokio::test]
async fn s06_cancel_rolls_back_inventory() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S06").await;
    let l = line(&ctx, ctx.mat, "10", "2", "13").await;
    let id = posted(&ctx, vec![l]).await;
    change_stock_in_status(
        id,
        &cancel_req(Some("S06 作废：供应商发错货")),
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("S06：作废失败 {} {}", error.code, error.msg));
    assert_eq!(status_of(id).await, "CANCELLED");
    let (inv, _, _, _) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(inv, common::dec("0"), "S06：作废后库存必须回到 0");
    ctx.cleanup().await;
}

/// S07 作废 → 库位账回滚到 0（B1 在作废后依然成立）。
#[tokio::test]
async fn s07_cancel_rolls_back_location_ledger() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S07").await;
    let l = line(&ctx, ctx.mat, "8", "2", "0").await;
    let id = posted(&ctx, vec![l]).await;
    change_stock_in_status(id, &cancel_req(Some("S07 作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S07：作废失败 {} {}", error.code, error.msg));
    let (inv, loc, _, _) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(loc, common::dec("0"), "S07：作废后库位账必须回到 0");
    common::assert_dec(loc, inv, "S07：作废后 B1 仍必须成立（0 = 0）");
    ctx.cleanup().await;
}

/// S08 作废 → 写反向流水 `OUT / PURCHASE_IN_CANCEL`。
#[tokio::test]
async fn s08_cancel_writes_reverse_txn() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S08").await;
    let l = line(&ctx, ctx.mat, "5", "2", "0").await;
    let id = posted(&ctx, vec![l]).await;
    let txn_before = txn_count_of(id).await;
    assert_eq!(txn_before, 1, "S08 前置：过账先写 1 条入库流水");
    change_stock_in_status(id, &cancel_req(Some("S08 作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S08：作废失败 {} {}", error.code, error.msg));

    assert_eq!(
        cancel_txn_count_of(id).await,
        1,
        "S08：作废必须写且只写 1 条 OUT/PURCHASE_IN_CANCEL 反向流水"
    );
    assert_eq!(
        txn_count_of(id).await,
        txn_before + 1,
        "S08：反向流水是**追加**而不是改原流水（审计链要能回放）"
    );
    let qty = common::scalar_dec(
        "SELECT qty::numeric(18,4) AS v FROM biz_inventory_txn \
          WHERE doc_id = ?::uuid AND biz_type = 'PURCHASE_IN_CANCEL'",
        vec![Value::from(&id)],
    )
    .await;
    common::assert_dec(qty, common::dec("5"), "S08：反向流水数量必须等于原入库数量");
    ctx.cleanup().await;
}

/// S09 作废 → 凭证红冲（红字凭证生成、原凭证 `reversed_by` 回写且仍 POSTED）。
#[tokio::test]
async fn s09_cancel_reverses_voucher() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S09").await;
    let l = line(&ctx, ctx.mat, "10", "2", "13").await;
    let id = posted(&ctx, vec![l]).await;
    let vouchers = voucher_ids_of(id).await;
    assert_eq!(vouchers.len(), 1);
    let blue = vouchers[0];

    change_stock_in_status(id, &cancel_req(Some("S09 作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S09：作废失败 {} {}", error.code, error.msg));

    let after = voucher_ids_of(id).await;
    assert_eq!(
        after.len(),
        2,
        "S09：作废后本单名下必须有 2 张凭证（蓝字 + 红字），实际 {} 张",
        after.len()
    );
    let red = after
        .iter()
        .find(|v| **v != blue)
        .copied()
        .expect("S09：必须有红字凭证");

    let reversed_by = common::scalar_text(
        "SELECT reversed_by::text AS v FROM biz_voucher WHERE id = ?::uuid",
        vec![Value::from(&blue)],
    )
    .await;
    assert_eq!(
        reversed_by.as_deref(),
        Some(red.to_string().as_str()),
        "S09：原凭证必须把 reversed_by 回写到红字凭证"
    );
    let blue_status = common::scalar_text(
        "SELECT status AS v FROM biz_voucher WHERE id = ?::uuid",
        vec![Value::from(&blue)],
    )
    .await;
    assert_eq!(
        blue_status.as_deref(),
        Some("POSTED"),
        "S09：原凭证状态仍是 POSTED（红冲用反向凭证表达，不原地改状态）"
    );
    let is_red = common::scalar_i64(
        "SELECT is_red::int AS v FROM biz_voucher WHERE id = ?::uuid",
        vec![Value::from(&red)],
    )
    .await;
    assert_eq!(is_red, 1, "S09：红字凭证必须 is_red = 1");

    // 红字凭证必须自平衡，且借贷合计与蓝字相等（红冲把借贷两边都镜像）
    let blue_debit = common::scalar_dec(
        "SELECT COALESCE(sum(debit_amount),0)::numeric(18,4) AS v FROM biz_voucher_entry \
          WHERE voucher_id = ?::uuid",
        vec![Value::from(&blue)],
    )
    .await;
    let red_debit = common::scalar_dec(
        "SELECT COALESCE(sum(debit_amount),0)::numeric(18,4) AS v FROM biz_voucher_entry \
          WHERE voucher_id = ?::uuid",
        vec![Value::from(&red)],
    )
    .await;
    let red_credit = common::scalar_dec(
        "SELECT COALESCE(sum(credit_amount),0)::numeric(18,4) AS v FROM biz_voucher_entry \
          WHERE voucher_id = ?::uuid",
        vec![Value::from(&red)],
    )
    .await;
    common::assert_dec(
        red_debit,
        blue_debit,
        "S09：红字凭证借方合计 = 蓝字借方合计（镜像后仍平衡）",
    );
    common::assert_dec(red_credit, blue_debit, "S09：红字凭证也必须借贷平衡");
    ctx.cleanup().await;
}

/// S10 作废 → 应付变 CANCELLED。
#[tokio::test]
async fn s10_cancel_cancels_payable() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S10").await;
    let l = line(&ctx, ctx.mat, "10", "2", "13").await;
    let id = posted(&ctx, vec![l]).await;
    assert_eq!(
        payable_of(id).await.map(|p| p.1).as_deref(),
        Some("OPEN"),
        "S10 前置：过账后应付必须 OPEN"
    );
    change_stock_in_status(id, &cancel_req(Some("S10 作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S10：作废失败 {} {}", error.code, error.msg));
    let (_, status, amount, paid) = payable_of(id).await.expect("S10：应付单不该消失");
    assert_eq!(
        status, "CANCELLED",
        "S10：作废收货单必须同时作废应付单（否则财务会重复付款）"
    );
    common::assert_dec(paid, common::dec("0"), "S10：作废的应付已付金额仍是 0");
    assert!(
        amount > common::dec("0"),
        "S10：作废不减金额（审计保留原额）"
    );
    ctx.cleanup().await;
}

/// S11 作废 → 序列号软删（行保留、`deleted_at` 置位）。
#[tokio::test]
async fn s11_cancel_voids_serials() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S11").await;
    let codes = vec![ctx.code("SN-A"), ctx.code("SN-B")];
    let mut item = line(&ctx, ctx.mat_sn, "2", "40", "0").await;
    item.serial_ids = codes.clone();
    let id = draft(&ctx, vec![item]).await;
    let mut post = post_req();
    post.serials = vec![StockInLineSerials {
        line_no: 1,
        serial_ids: codes.clone(),
    }];
    change_stock_in_status(id, &post, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S11：过账失败 {} {}", error.code, error.msg));
    assert_eq!(serial_count_of(id).await, 2);

    change_stock_in_status(id, &cancel_req(Some("S11 作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S11：作废失败 {} {}", error.code, error.msg));

    assert_eq!(
        serial_count_of(id).await,
        0,
        "S11：作废必须软删（deleted_at 置位）本单生成的全部序列号"
    );
    let kept = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_material_serial \
          WHERE source_type = 'PURCHASE' AND source_id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await;
    assert_eq!(
        kept, 2,
        "S11：软删不是物理删 —— 行要保留下来当审计痕迹（实际 {kept} 行）"
    );
    ctx.cleanup().await;
}

// ==================== S12：总闸（四账回基线） ====================

/// S12 ★ 作废后四账回到基线（库存 / 库位账 / 流水 / 凭证）—— 本文件最重要的断言。
///
/// 与 S06–S11 的区别：那几条各看一项，这条要求**一次作废让全部项同时归位**，
/// 任何一项漏掉都会在这里暴露（P9 之前正是「库存回去了、库位账没回」这种半吊子状态）。
#[tokio::test]
async fn s12_cancel_returns_all_four_ledgers_to_baseline() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S12").await;

    // 基线：建单前的四账（本用例物料 + 本用例仓库，全新，必须全零）
    let baseline = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    assert_eq!(
        baseline,
        (common::dec("0"), common::dec("0"), 0, 0),
        "S12 前置：基线必须全零，实际 {baseline:?}"
    );

    let l = line(&ctx, ctx.mat, "12", "2.5", "13").await;
    let id = posted(&ctx, vec![l]).await;
    let posted_snapshot = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(posted_snapshot.0, common::dec("12"), "S12 中途：库存 12");
    common::assert_dec(posted_snapshot.1, common::dec("12"), "S12 中途：库位账 12");
    assert_eq!(posted_snapshot.2, 1, "S12 中途：1 条入库流水");
    assert_eq!(posted_snapshot.3, 1, "S12 中途：1 张凭证");

    change_stock_in_status(
        id,
        &cancel_req(Some("S12 作废：整链回基线")),
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("S12：作废失败 {} {}", error.code, error.msg));

    let after = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(after.0, baseline.0, "S12：库存必须回到基线");
    common::assert_dec(after.1, baseline.1, "S12：库位账必须回到基线");
    assert_eq!(
        after.2,
        baseline.2 + 2,
        "S12：流水是追加账 —— 过账写 1 条 IN、作废再追加 1 条 OUT，作废后应为「基线 + 2」，\
         实际 {}（基线 {}）",
        after.2,
        baseline.2
    );
    assert_eq!(
        after.3,
        baseline.3 + 2,
        "S12：凭证是开新单 —— 过账 1 张蓝字、作废 1 张红字，作废后应为「基线 + 2」，\
         实际 {}（基线 {}）",
        after.3,
        baseline.3
    );

    // 金额也要归零（数量对、金额不对同样是坏账）
    let amount = common::scalar_dec(
        "SELECT COALESCE(sum(amount),0)::numeric(18,4) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&ctx.mat), Value::from(&ctx.wh_from)],
    )
    .await;
    common::assert_dec(amount, common::dec("0"), "S12：作废后库存金额也必须回到 0");
    ctx.cleanup().await;
}

// ==================== S13–S16：反向保护 ====================

/// S13 作废不给 reason → 400（已过账单据的红冲必须可追溯）。
#[tokio::test]
async fn s13_cancel_without_reason_is_bad_request() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S13").await;
    let l = line(&ctx, ctx.mat, "3", "2", "0").await;
    let id = posted(&ctx, vec![l]).await;
    let error = change_stock_in_status(id, &cancel_req(None), Some(ctx.worker))
        .await
        .expect_err("S13：已过账的收货单不给 reason 必须被拒");
    assert_eq!(
        error.status, 400,
        "S13：缺 reason 是参数问题 → 400，实际 {error:?}"
    );
    assert_eq!(status_of(id).await, "POSTED", "S13：被拒后状态不许变");
    // 空白 reason 与不给等价
    let blank = change_stock_in_status(id, &cancel_req(Some("   ")), Some(ctx.worker))
        .await
        .expect_err("S13：空白 reason 也必须被拒");
    assert_eq!(
        blank.status, 400,
        "S13：空白 reason 同样 400，实际 {blank:?}"
    );
    ctx.cleanup().await;
}

/// S14 货已出库后作废 → 409（文案点名「货已出库 / 库存不足」）。
#[tokio::test]
async fn s14_cancel_after_shipping_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S14").await;
    let l = line(&ctx, ctx.mat, "10", "2", "0").await;
    let id = posted(&ctx, vec![l]).await;

    // 模拟「货已领用出库」：库存被别的单据扣走 6 件（这里直接改库存台账，
    // 不动收货单 —— 正是生产里「收货后发生了领料」的账面后果）
    db().execute_with_params(
        "UPDATE biz_inventory SET qty = 4, amount = 8 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&ctx.mat), Value::from(&ctx.wh_from)],
    )
    .await
    .expect("S14：造「已出库」场景失败");

    let error = change_stock_in_status(id, &cancel_req(Some("S14 作废")), Some(ctx.worker))
        .await
        .expect_err("S14：货已出库还允许作废就是把账做坏");
    assert_eq!(error.status, 409, "S14：库存不足 → 409，实际 {error:?}");
    assert!(
        error.msg.contains("货已部分出库") || error.msg.contains("库存不足"),
        "S14：文案必须说清「货已出库/库存不足」，实际：{}",
        error.msg
    );
    assert_eq!(
        status_of(id).await,
        "POSTED",
        "S14：被拒后单据必须仍是 POSTED"
    );
    ctx.cleanup().await;
}

/// S15 重复作废 → 409（幂等保护，绝不二次退库存）。
#[tokio::test]
async fn s15_double_cancel_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S15").await;
    let l = line(&ctx, ctx.mat, "10", "2", "0").await;
    let id = posted(&ctx, vec![l]).await;
    change_stock_in_status(id, &cancel_req(Some("S15 首次作废")), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S15：首次作废失败 {} {}", error.code, error.msg));
    let after_first = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;

    let error = change_stock_in_status(id, &cancel_req(Some("S15 二次作废")), Some(ctx.worker))
        .await
        .expect_err("S15：重复作废必须 409");
    assert_eq!(error.status, 409, "S15：重复作废 → 409，实际 {error:?}");
    assert!(
        error.msg.contains("重复作废") || error.msg.contains("已作废"),
        "S15：文案要说明「已经是已作废」，实际：{}",
        error.msg
    );
    assert_eq!(
        common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await,
        after_first,
        "S15：被拒的二次作废不许再动任何一本账（否则库存被退两次变成负数）"
    );
    ctx.cleanup().await;
}

/// S16 作废失败 → 事务整体回滚（库存 / 凭证 / 流水 / 应付全都不变）。
///
/// 造失败的方式同 S14（货已出库），但这里断言的是**四账 + 序列号 + 应付**的完整快照不变，
/// 而不是只看状态码：`cancel_in_tx` 里前面几步（退序列号、反向冲减）可能已经写了库，
/// 只有整笔回滚才能保证「半成品」不外泄。
#[tokio::test]
async fn s16_failed_cancel_rolls_back_everything() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("S16").await;
    let codes = vec![ctx.code("SN-X"), ctx.code("SN-Y")];
    let mut item = line(&ctx, ctx.mat_sn, "2", "40", "0").await;
    item.serial_ids = codes.clone();
    let id = draft(&ctx, vec![item]).await;
    let mut post = post_req();
    post.serials = vec![StockInLineSerials {
        line_no: 1,
        serial_ids: codes.clone(),
    }];
    change_stock_in_status(id, &post, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("S16：过账失败 {} {}", error.code, error.msg));

    // 出库 1 件：库存 2 → 1，序列号里有一件变成 OUTBOUND（一物一码最硬的「已出库」证据）
    db().execute_with_params(
        "UPDATE biz_inventory SET qty = 1, amount = 40 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&ctx.mat_sn), Value::from(&ctx.wh_from)],
    )
    .await
    .expect("S16：造已出库库存失败");
    db().execute_with_params(
        "UPDATE biz_material_serial SET status = 'OUTBOUND' \
          WHERE id = (SELECT id FROM biz_material_serial \
                       WHERE source_id = ?::uuid ORDER BY serial_no LIMIT 1)",
        vec![Value::from(&id)],
    )
    .await
    .expect("S16：造已出库序列号失败");

    let before = common::four_ledger_snapshot(ctx.mat_sn, ctx.wh_from).await;
    let serial_before = serial_count_of(id).await;
    let payable_before = payable_of(id).await.map(|p| p.1);

    let error = change_stock_in_status(id, &cancel_req(Some("S16 作废")), Some(ctx.worker))
        .await
        .expect_err("S16：有件已出库时必须拒绝作废");
    assert_eq!(error.status, 409, "S16：已出库 → 409，实际 {error:?}");

    assert_eq!(
        common::four_ledger_snapshot(ctx.mat_sn, ctx.wh_from).await,
        before,
        "S16：作废失败必须整笔回滚 —— 四账快照必须逐项不变"
    );
    assert_eq!(
        serial_count_of(id).await,
        serial_before,
        "S16：作废失败不许把序列号软删（回滚要连序列号一起滚）"
    );
    assert_eq!(
        payable_of(id).await.map(|p| p.1),
        payable_before,
        "S16：作废失败不许提前把应付作废"
    );
    assert_eq!(
        status_of(id).await,
        "POSTED",
        "S16：单据状态必须仍是 POSTED"
    );
    ctx.cleanup().await;
}
