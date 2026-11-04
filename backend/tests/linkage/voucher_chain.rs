//! 领域 5：凭证 ↔ 分录（8 个用例，V01–V08）。
//!
//! # 红线：凭证只增不改
//!
//! 一张凭证一旦 `POSTED` 就是账务事实，**更正只能靠红冲开新单**（不原地改状态、不物理删）。
//! 于是「红冲」这条链路天然跨三张表：`biz_voucher`（原单指针 + 红字单）+ `biz_voucher_entry`
//! （镜像分录）+ 调它的人（库存 / 应付 / 手工）。单模块用例能证明「红冲函数返回了新 id」，
//! 但证明不了：
//!
//! ```text
//! 红字凭证的分录是不是原单的**镜像**（借↔贷对调，金额不丢一分）
//! 原单的 reversed_by 有没有回写、status 有没有被偷偷改成 CANCELLED
//! 重复红冲 / 红冲红字凭证 会不会把同一笔账冲两次
//! ```
//!
//! # 与卡面 V07 的一处口径说明
//!
//! 卡面写「全库扫描 Σ借 = Σ贷」。这里**不扫全库**：`cloudpivot_test` 是并发共享库
//! （P9/P10/P11 同时在里面造数据），全库扫描会被别的卡片跑到一半的中间态污染，变成偶发红
//! （P9 已踩过「全局计数漂数」）。改为「本用例可归属的凭证全集」逐张校验 —— 覆盖同样的
//! 不变量，但不依赖别人跑没跑完。

use nik::prelude::*;
use nik::uuid::Uuid;
use rust_decimal::Decimal;

use cloudpivot::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
use cloudpivot::business::erp::stockin_handler::{change_stock_in_status, create_stock_in};
use cloudpivot::business::erp::voucher_write::{
    CreateVoucherReq, ManualEntryReq, create_manual_voucher, reverse_voucher,
};

use crate::linkage::common::{self, Ctx};

// ==================== 夹具动作 ====================

/// 按科目编码取 `biz_account.id`。
async fn account_id(code: &str) -> Uuid {
    let raw = common::scalar_text(
        "SELECT id::text AS v FROM biz_account WHERE account_code = ? AND deleted_at IS NULL",
        vec![Value::Text(code.to_string())],
    )
    .await
    .unwrap_or_else(|| panic!("科目 {code} 不存在（V0149 种子应当种好）"));
    Uuid::parse_str(&raw).expect("科目 id")
}

/// 建一张已过账的收货单，返回它生成的那张蓝字凭证 id。
async fn blue_voucher(ctx: &Ctx) -> Uuid {
    let loc = ctx.loc_row(ctx.mat).await;
    let req = CreateStockInReq {
        source_order_id: None,
        purchase_order_id: None,
        // A4（V0199）起：PURCHASE 且无来源采购单 = 无源单据，必须显式声明（remark 已给）
        is_sourceless: Some(true),
        supplier_id: Some(ctx.supplier.to_string()),
        warehouse_id: Some(ctx.wh_from.to_string()),
        in_date: None,
        invoice_received: None,
        remark: Some(format!("{} 凭证链路", ctx.tag)),
        items: vec![StockInItemReq {
            purchase_order_item_id: None,
            material_id: Some(ctx.mat.to_string()),
            qty: Some(common::dec("10")),
            price_excl: Some(common::dec("2")),
            tax_rate: Some(common::dec("13")),
            location_id: Some(loc.to_string()),
            location_dict_id: None,
            serial_ids: Vec::new(),
            remark: None,
        }],
        total_qty: None,
        total_amount: None,
        total_tax: None,
    };
    let id = create_stock_in(&req, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("{}：建收货单失败 {} {}", ctx.tag, error.code, error.msg));
    change_stock_in_status(
        id,
        &StockInStatusReq {
            status: Some("POSTED".to_string()),
            reason: None,
            serials: Vec::new(),
            // ARCH-3 DC-01：不请求跳过审批（默认行为不变）
            approval: Default::default(),
        },
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("{}：过账失败 {} {}", ctx.tag, error.code, error.msg));

    let vouchers = voucher_ids_of_source(id).await;
    assert_eq!(vouchers.len(), 1, "{}：过账应当生成 1 张蓝字凭证", ctx.tag);
    vouchers[0]
}

/// 某张收货单产生的凭证（按 `source_id` 精确命中）。
async fn voucher_ids_of_source(stock_in_id: Uuid) -> Vec<Uuid> {
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

/// 凭证单头快照：`(status, is_red, reversed_by, total_debit, total_credit)`。
async fn voucher_head(voucher_id: Uuid) -> (String, i16, Option<Uuid>, Decimal, Decimal) {
    let rows = db()
        .find_with_params(
            "SELECT status, is_red::int AS is_red, reversed_by, \
                    total_debit::numeric(18,4) AS total_debit, \
                    total_credit::numeric(18,4) AS total_credit \
               FROM biz_voucher WHERE id = ?::uuid",
            vec![Value::from(&voucher_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查凭证失败：{error}"));
    let row = rows
        .first()
        .unwrap_or_else(|| panic!("凭证 {voucher_id} 不存在"));
    (
        row.get_typed("status").expect("状态"),
        row.get_typed("is_red").expect("is_red"),
        row.get_typed("reversed_by").ok().flatten(),
        row.get_typed("total_debit").expect("借方合计"),
        row.get_typed("total_credit").expect("贷方合计"),
    )
}

/// 一张凭证的分录：`(account_code, debit, credit)`，按 `line_no` 排序。
async fn entries_of(voucher_id: Uuid) -> Vec<(String, Decimal, Decimal)> {
    let rows = db()
        .find_with_params(
            "SELECT account_code, debit_amount::numeric(18,4) AS debit, \
                    credit_amount::numeric(18,4) AS credit \
               FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY line_no",
            vec![Value::from(&voucher_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查分录失败：{error}"));
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<String>("account_code").expect("科目"),
                row.get_typed::<Decimal>("debit").expect("借"),
                row.get_typed::<Decimal>("credit").expect("贷"),
            )
        })
        .collect()
}

/// 手工凭证请求（`remark` 带本用例前缀，便于精确清理）。
fn manual_req(ctx: &Ctx, debit: &str, credit: &str) -> CreateVoucherReq {
    CreateVoucherReq {
        voucher_date: None,
        summary: Some(ctx.code("手工凭证")),
        entries: Some(vec![
            ManualEntryReq {
                account_id: Some(String::new()), // 由调用方填（见 [`manual_req_with`]）
                debit: Some(common::dec(debit)),
                credit: None,
                summary: None,
            },
            ManualEntryReq {
                account_id: Some(String::new()),
                debit: None,
                credit: Some(common::dec(credit)),
                summary: None,
            },
        ]),
        remark: Some(format!("{}-MANUAL", ctx.prefix)),
    }
}

/// 手工凭证请求（填好两个科目）：借 `debit_code` / 贷 `credit_code`。
async fn manual_req_with(
    ctx: &Ctx,
    debit_code: &str,
    debit: &str,
    credit_code: &str,
    credit: &str,
) -> CreateVoucherReq {
    let mut req = manual_req(ctx, debit, credit);
    let debit_account = account_id(debit_code).await.to_string();
    let credit_account = account_id(credit_code).await.to_string();
    if let Some(entries) = req.entries.as_mut() {
        entries[0].account_id = Some(debit_account);
        entries[1].account_id = Some(credit_account);
    }
    req
}

/// 清掉本用例造的手工凭证（按 remark 前缀精确定位，绝不按全局 `MANUAL` 删）。
async fn cleanup_manual(ctx: &Ctx) {
    let remark = format!("{}-MANUAL", ctx.prefix);
    if let Err(error) = db()
        .execute_with_params(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                 (SELECT id FROM biz_voucher WHERE remark = ?)",
            vec![Value::Text(remark.clone())],
        )
        .await
    {
        println!("WARN: 清理手工凭证分录失败（{}）：{error}", ctx.tag);
    }
    if let Err(error) = db()
        .execute_with_params(
            "DELETE FROM biz_voucher WHERE remark = ?",
            vec![Value::Text(remark)],
        )
        .await
    {
        println!("WARN: 清理手工凭证失败（{}）：{error}", ctx.tag);
    }
}

// ==================== V01–V02：红冲的正向级联 ====================

/// V01 红冲 → 生成红字凭证，金额取反（借↔贷对调，一分不差）。
#[tokio::test]
async fn v01_reverse_creates_mirrored_red_voucher() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V01").await;
    let blue = blue_voucher(&ctx).await;
    let (_, blue_is_red, _, blue_debit, blue_credit) = voucher_head(blue).await;
    assert_eq!(blue_is_red, 0, "V01 前置：过账生成的是蓝字凭证");
    common::assert_dec(blue_debit, blue_credit, "V01 前置：蓝字凭证必须借贷平衡");

    let red = reverse_voucher(blue, Some("V01 红冲：金额录错"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V01：红冲失败 {} {}", error.code, error.msg))
        .voucher
        .id
        .parse::<Uuid>()
        .expect("红字凭证 id");

    assert_ne!(red, blue, "V01：红冲必须开新单，不是改原单");
    let (_, red_is_red, _, red_debit, red_credit) = voucher_head(red).await;
    assert_eq!(red_is_red, 1, "V01：红字凭证必须 is_red = 1");
    common::assert_dec(red_debit, red_credit, "V01：红字凭证自身必须借贷平衡");

    // 「金额取反」的正确判据：每条分录的借↔贷完全对调
    let blue_entries = entries_of(blue).await;
    let red_entries = entries_of(red).await;
    assert_eq!(
        red_entries.len(),
        blue_entries.len(),
        "V01：红字凭证的分录条数必须与原单一致"
    );
    let mut mirrored = blue_entries.clone();
    mirrored.sort_by(|a, b| a.0.cmp(&b.0));
    let mut actual = red_entries.clone();
    actual.sort_by(|a, b| a.0.cmp(&b.0));
    for (expect, got) in mirrored.iter().zip(actual.iter()) {
        assert_eq!(
            expect.0, got.0,
            "V01：科目集合必须一致（{mirrored:?} vs {actual:?}）"
        );
        common::assert_dec(
            got.1,
            expect.2,
            &format!("V01：科目 {} 的借方 = 原单贷方", got.0),
        );
        common::assert_dec(
            got.2,
            expect.1,
            &format!("V01：科目 {} 的贷方 = 原单借方", got.0),
        );
    }
    ctx.cleanup().await;
}

/// V02 红冲 → 原凭证 `reversed_by` 回写，且 **status 仍是 POSTED**（不原地改成 CANCELLED）。
#[tokio::test]
async fn v02_reverse_backfills_pointer_and_keeps_posted() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V02").await;
    let blue = blue_voucher(&ctx).await;
    let (status_before, _, pointer_before, _, _) = voucher_head(blue).await;
    assert_eq!(status_before, "POSTED");
    assert!(pointer_before.is_none(), "V02 前置：新凭证不该有红冲指针");

    let red = reverse_voucher(blue, Some("V02 红冲"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V02：红冲失败 {} {}", error.code, error.msg))
        .voucher
        .id
        .parse::<Uuid>()
        .expect("红字凭证 id");

    let (status_after, _, pointer_after, _, _) = voucher_head(blue).await;
    assert_eq!(
        pointer_after,
        Some(red),
        "V02：原凭证必须把 reversed_by 指到红字凭证"
    );
    assert_eq!(
        status_after, "POSTED",
        "V02：红冲不改原单状态（账务事实只能靠反向凭证抵消，不能把历史改成「已作废」）"
    );
    // 反向：红字凭证自己不许有 reversed_by（它是这笔红冲的终点）
    let (_, _, red_pointer, _, _) = voucher_head(red).await;
    assert!(
        red_pointer.is_none(),
        "V02：红字凭证不该再指向谁（否则形成环）"
    );
    ctx.cleanup().await;
}

// ==================== V03–V04：红冲的反向保护 ====================

/// V03 重复红冲 → 409（同一笔账绝不能被冲两次）。
#[tokio::test]
async fn v03_double_reverse_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V03").await;
    let blue = blue_voucher(&ctx).await;
    let _red = reverse_voucher(blue, Some("V03 第一次红冲"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V03：首次红冲失败 {} {}", error.code, error.msg));

    let error = reverse_voucher(blue, Some("V03 第二次红冲"), Some(ctx.worker))
        .await
        .expect_err("V03：同一张凭证不许红冲两次");
    assert_eq!(error.status, 409, "V03：重复红冲 → 409，实际 {error:?}");

    // 副作用检查：只有 2 张凭证（蓝 + 1 张红），没有第二张红字
    let (_, _, pointer, _, _) = voucher_head(blue).await;
    let red = pointer.expect("reversed_by");
    let count = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_voucher WHERE reverse_of = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&blue)],
    )
    .await;
    assert_eq!(count, 1, "V03：被拒的重复红冲不许留下第二张红字凭证");
    assert_eq!(voucher_head(red).await.0, "POSTED");
    ctx.cleanup().await;
}

/// V04 红冲「红字凭证」→ 409（要更正请补蓝字凭证，不允许红冲套红冲）。
#[tokio::test]
async fn v04_reverse_red_voucher_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V04").await;
    let blue = blue_voucher(&ctx).await;
    let red = reverse_voucher(blue, Some("V04 先正常红冲"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V04：红冲蓝字失败 {} {}", error.code, error.msg))
        .voucher
        .id
        .parse::<Uuid>()
        .expect("红字凭证 id");

    let error = reverse_voucher(red, Some("V04 再红冲红字"), Some(ctx.worker))
        .await
        .expect_err("V04：红字凭证不许再红冲");
    assert_eq!(error.status, 409, "V04：红冲红字 → 409，实际 {error:?}");
    assert!(
        error.msg.contains("红字"),
        "V04：文案要点名「红字凭证不能再红冲」，实际：{}",
        error.msg
    );
    let (status, is_red, pointer, _, _) = voucher_head(red).await;
    assert_eq!(status, "POSTED");
    assert_eq!(is_red, 1);
    assert!(pointer.is_none(), "V04：被拒后红字凭证不该被写 reversed_by");
    ctx.cleanup().await;
}

// ==================== V05–V06：手工凭证的借贷校验 ====================

/// V05 手工凭证借贷不平 → 400，文案点名差额，且**不落库**。
#[tokio::test]
async fn v05_unbalanced_manual_voucher_is_bad_request() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V05").await;
    // 借 100 / 贷 60 → 差 40
    let req = manual_req_with(&ctx, "1405", "100", "2202", "60").await;

    let error = create_manual_voucher(&req, Some(ctx.worker))
        .await
        .expect_err("V05：借贷不平必须被拒");
    assert_eq!(error.status, 400, "V05：借贷不平 → 400，实际 {error:?}");
    assert!(
        error.msg.contains("不平衡") && error.msg.contains("40"),
        "V05：文案必须点名差额（差 40），实际：{}",
        error.msg
    );

    let leaked = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_voucher WHERE remark = ? AND deleted_at IS NULL",
        vec![Value::Text(format!("{}-MANUAL", ctx.prefix))],
    )
    .await;
    assert_eq!(leaked, 0, "V05：被拒的手工凭证不许落库（连草稿都不该留）");
    cleanup_manual(&ctx).await;
    ctx.cleanup().await;
}

/// V06 手工凭证借贷平衡 → 成功，`entry_count` / 合计与分录一致。
#[tokio::test]
async fn v06_balanced_manual_voucher_is_posted() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V06").await;
    let req = manual_req_with(&ctx, "1405", "100", "2202", "100").await;

    let detail = create_manual_voucher(&req, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V06：平衡的手工凭证必须成功 {} {}", error.code, error.msg));
    assert_eq!(detail.entries.len(), 2, "V06：分录条数必须是 2");
    common::assert_dec(
        detail.entry_total_debit,
        common::dec("100"),
        "V06：借方合计 100",
    );
    common::assert_dec(
        detail.entry_total_credit,
        common::dec("100"),
        "V06：贷方合计 100",
    );
    assert!(detail.balanced, "V06：详情自检必须 balanced");

    let id = detail.voucher.id.parse::<Uuid>().expect("凭证 id");
    let (status, is_red, _, total_debit, total_credit) = voucher_head(id).await;
    assert_eq!(status, "POSTED", "V06：手工凭证落库即 POSTED");
    assert_eq!(is_red, 0, "V06：手工凭证是蓝字");
    common::assert_dec(total_debit, common::dec("100"), "V06：单头借方合计");
    common::assert_dec(total_credit, common::dec("100"), "V06：单头贷方合计");
    assert_eq!(entries_of(id).await.len(), 2, "V06：库里也必须是 2 条分录");
    cleanup_manual(&ctx).await;
    ctx.cleanup().await;
}

// ==================== V07–V08：全量不变量 ====================

/// V07 每张凭证 Σ借 = Σ贷（本用例可归属的凭证全集逐张校验）。
///
/// 覆盖三类凭证：收货自动凭证（蓝）、它的红字凭证、手工凭证 —— 正好是这条链路的全部来源。
#[tokio::test]
async fn v07_every_voucher_is_balanced() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V07").await;
    let blue = blue_voucher(&ctx).await;
    let red = reverse_voucher(blue, Some("V07 红冲保持平衡"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V07：红冲失败 {} {}", error.code, error.msg))
        .voucher
        .id
        .parse::<Uuid>()
        .expect("红字凭证 id");
    let manual = create_manual_voucher(
        &manual_req_with(&ctx, "1405", "33.33", "2202", "33.33").await,
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("V07：建手工凭证失败 {} {}", error.code, error.msg))
    .voucher
    .id
    .parse::<Uuid>()
    .expect("手工凭证 id");

    // 归属集合：本用例的蓝 / 红 / 手工三张，绝不做全局 COUNT（共享库会漂数）
    let vouchers = vec![blue, red, manual];
    for voucher_id in &vouchers {
        let entries = entries_of(*voucher_id).await;
        assert!(!entries.is_empty(), "V07：凭证 {voucher_id} 不能没有分录");
        let mut debit = common::dec("0");
        let mut credit = common::dec("0");
        for (_, d, c) in &entries {
            debit += *d;
            credit += *c;
        }
        common::assert_dec(
            debit,
            credit,
            &format!("V07：凭证 {voucher_id} 的分录 Σ借 必须 = Σ贷"),
        );
    }
    cleanup_manual(&ctx).await;
    ctx.cleanup().await;
}

/// V08 每条分录有且仅有一边非零（借 XOR 贷）。
#[tokio::test]
async fn v08_each_entry_has_exactly_one_non_zero_side() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("V08").await;
    let blue = blue_voucher(&ctx).await;
    let red = reverse_voucher(blue, Some("V08 红冲"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("V08：红冲失败 {} {}", error.code, error.msg))
        .voucher
        .id
        .parse::<Uuid>()
        .expect("红字凭证 id");
    let manual = create_manual_voucher(
        &manual_req_with(&ctx, "1002", "12.5", "2202", "12.5").await,
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("V08：建手工凭证失败 {} {}", error.code, error.msg))
    .voucher
    .id
    .parse::<Uuid>()
    .expect("手工凭证 id");

    for voucher_id in [blue, red, manual] {
        for (code, debit, credit) in entries_of(voucher_id).await {
            let only_debit = debit > common::dec("0") && credit.is_zero();
            let only_credit = credit > common::dec("0") && debit.is_zero();
            assert!(
                only_debit || only_credit,
                "V08：凭证 {voucher_id} 的科目 {code} 分录必须是单边（实际 借 {debit} / 贷 {credit}）"
            );
        }
    }
    cleanup_manual(&ctx).await;
    ctx.cleanup().await;
}
