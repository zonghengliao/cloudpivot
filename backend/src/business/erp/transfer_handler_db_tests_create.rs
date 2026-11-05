// ============================================================================
// transfer_handler_db_tests_create.rs —— 真库：建单 / 改单 / 复制 / 校验（含同仓拒绝、序列号归属）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::db_tests_fixtures::*;
use super::*;
use crate::business::erp::transfer_dto::TRANSFER_PERMISSIONS;
use crate::business::erp::transfer_model::STATUS_DRAFT;
use crate::business::erp::transfer_model::{self, NewItem, NewTransfer, STATUS_PENDING};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use std::str::FromStr;

/// DC-09（P0 单位止血 · 调拨单侧）：物料挂折算单位「箱」（1 箱 = 12 个）→ 400 + 折算建议，
/// **单头与明细都不落库**（调拨是「发货 / 收货」两次动账的源头，漏一行就是两本账各错一次）。
#[tokio::test]
async fn non_base_unit_item_is_rejected() {
    let _db = match prepare().await {
        Some(guard) => guard,
        None => return,
    };
    let error = create_transfer(&body(vec![item(M_BOX, "1")]), Some(uuid(U_APPLICANT)), None)
        .await
        .unwrap_err();
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
    assert!(
        error.msg.contains(
            "单位「箱」不是基本单位：本版本不支持多单位换算，\
             请按基本单位录入（1 箱 = 12 个 → 录 12）"
        ),
        "{}",
        error.msg
    );
    assert!(error.msg.contains("items[0]"), "{}", error.msg);
    assert!(error.msg.contains("W3B-M-BOX"), "{}", error.msg);

    // 单头都不许留下：`prepare_items` 在事务外跑，任何一行不过就整张单不落库
    let heads = query(
        "SELECT COUNT(*) AS cnt FROM biz_stock_transfer WHERE from_warehouse_id = ?::uuid",
        vec![Value::from(&uuid(WH_FROM))],
    )
    .await;
    assert_eq!(
        heads
            .first()
            .map(|row| row.get_typed::<i64>("cnt").unwrap()),
        Some(0),
        "被拒的调拨单不许留下单头"
    );

    // 对照组：基本单位物料照常建单（同一夹具、同一请求形状）
    create_transfer(
        &body(vec![item(M_PLAIN, "1")]),
        Some(uuid(U_APPLICANT)),
        None,
    )
    .await
    .unwrap();
}

// ---------- ① 建单：DRAFT + 单号格式 ----------
#[tokio::test]
async fn create_returns_draft_with_card_no_format() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_transfer(
        &body(vec![item(M_PLAIN, "5")]),
        Some(uuid(U_APPLICANT)),
        Some("w3b_applicant"),
    )
    .await
    .expect("建单必须成功");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_DRAFT);
    let parts: Vec<&str> = row.transfer_no.split('-').collect();
    assert_eq!(parts.len(), 3, "单号段数：{}", row.transfer_no);
    assert_eq!(parts[0], "TK");
    assert_eq!(parts[1].len(), 8, "日期段 8 位：{}", row.transfer_no);
    assert!(parts[1].chars().all(|ch| ch.is_ascii_digit()));
    assert_eq!(parts[2].len(), 6, "流水段 6 位：{}", row.transfer_no);
    assert_eq!(row.total_qty.to_string(), "5.0000");
    assert!(row.submitted_at.is_none(), "DRAFT 不能有 submitted_at");
    assert_eq!(row.applicant_id, Some(uuid(U_APPLICANT)));

    // P10 铁律 ③：建单留痕（`NULL → DRAFT`）—— 五张单据里调拨单最后一个补上，
    // 少了这一行，详情页「状态历史」的第一格永远是空的。
    let trail = state_machine::trail_of_no(db(), &row.transfer_no)
        .await
        .expect("查调拨单状态轨迹");
    let seen: Vec<String> = trail
        .iter()
        .map(|log| {
            format!(
                "{}→{}",
                log.from_status.as_deref().unwrap_or("NULL"),
                log.to_status
            )
        })
        .collect();
    assert_eq!(seen, vec!["NULL→DRAFT".to_string()], "轨迹：{seen:?}");
    assert_eq!(trail[0].entity_type, state_machine::ENTITY_TRANSFER);
    assert_eq!(trail[0].operator_id, Some(uuid(U_APPLICANT)));
}

// ---------- ② 建单：from = to 被 DB CHECK 拒 ----------
#[tokio::test]
async fn db_check_rejects_same_warehouse() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 绕过 DTO 校验，直接打模型层：证明 DB 的 ck_transfer_diff_wh 真的在
    let new = NewTransfer {
        from_warehouse_id: uuid(WH_FROM),
        to_warehouse_id: uuid(WH_FROM),
        transfer_date: None,
        reason: None,
        remark: None,
        applicant_id: Some(uuid(U_APPLICANT)),
        applicant_name: None,
    };
    let items = vec![NewItem {
        material_id: uuid(M_PLAIN),
        from_location_id: None,
        to_location_id: None,
        qty: Decimal::from_str("1").expect("qty"),
        serial_ids: Vec::new(),
    }];
    let err = transfer_model::create(
        db(),
        &new,
        &items,
        Decimal::from_str("1").expect("qty"),
        Some(uuid(U_APPLICANT)),
        Utc::now(),
    )
    .await
    .expect_err("同仓调拨必须被库约束拒绝");
    assert!(
        matches!(&err, DbError::CheckViolation { .. }),
        "应当是 CheckViolation（ck_transfer_diff_wh），实际：{err}"
    );
}

// ---------- ③ 建单：qty <= 0 / 明细为空（DTO 层 400） ----------
#[test]
fn create_validation_rejects_bad_qty_and_empty_items() {
    let zero = body(vec![item(M_PLAIN, "0")]);
    let err = zero.validate().expect_err("qty = 0 必须 400");
    assert_eq!(err.status, 400);
    let empty = body(Vec::new());
    let err = empty.validate().expect_err("空明细必须 400");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("items 不能为空"), "{}", err.msg);
}

// ---------- ④ 建单：序列号引用不合法（不是该物料 / 不在库）→ 400 ----------
#[tokio::test]
async fn create_rejects_foreign_or_outbound_serial() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 序列号属于 M_SN，却挂在 M_PLAIN 的行上 → 必须 400 而不是落库
    let mut line = item(M_PLAIN, "1");
    line.serial_ids = vec![SERIAL_1.to_string()];
    let err = create_transfer(&body(vec![line]), Some(uuid(U_APPLICANT)), None)
        .await
        .expect_err("序列号不属于该物料必须 400");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("在库"), "{}", err.msg);
}

// ---------- ⑤ 改单：DRAFT 能改（明细整体重建） ----------
#[tokio::test]
async fn update_rebuilds_items_for_draft() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
        .await
        .expect("建单");
    let mut second = item(M_SN, "2");
    second.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    update_transfer(id, &body(vec![item(M_PLAIN, "3"), second]), Some(applicant))
        .await
        .expect("DRAFT 改单必须成功");
    let detail = detail_of(id).await.expect("详情");
    assert_eq!(detail.items.len(), 2, "明细必须整体重建");
    assert_eq!(detail.items[1].line_no, 2);
    assert_eq!(detail.items[1].serial_ids.len(), 2);
    assert_eq!(detail.transfer.total_qty.to_string(), "5.0000");
}

// ---------- ⑥ 改单：PENDING 不能改 → 409 ----------
#[tokio::test]
async fn update_pending_conflicts() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (id, _) = transfer_in_status(STATUS_PENDING).await;
    let err = update_transfer(id, &body(vec![item(M_PLAIN, "9")]), Some(uuid(U_APPLICANT)))
        .await
        .expect_err("PENDING 改单必须 409");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    // 明细没被改动
    let detail = detail_of(id).await.expect("详情");
    assert_eq!(detail.transfer.total_qty.to_string(), "5.0000");
}

// ---------- ⑳ 权限码：6 条与 V0144 逐字一致（真 403 见 verify 脚本） ----------
#[test]
fn permission_codes_are_six_and_stable() {
    assert_eq!(TRANSFER_PERMISSIONS.len(), 6);
    for (code, _) in TRANSFER_PERMISSIONS {
        assert!(code.starts_with("business:erp:transfer:"), "{code}");
    }
}

/// ④ 复制一张调拨单：单头（两仓 / 原因）+ 明细一致，状态 DRAFT、新单号、序列号不复制。
#[tokio::test]
async fn d9_transfer_copy_keeps_header_and_items_but_resets_state() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 原单先走到「已提交审批」再复制（复制的价值就是「这张单再来一遍」）
    let mut request = body(vec![item(M_PLAIN, "7"), item(M_SN, "1")]);
    request.remark = Some("D9 复制源单备注".to_string());
    let origin_id = create_transfer(&request, Some(uuid(U_APPLICANT)), Some("申请人"))
        .await
        .expect("建单应成功");
    change_transfer_status(origin_id, &status_req("PENDING", None), uuid(U_APPLICANT))
        .await
        .expect("提交审批应成功");
    let origin = refresh(origin_id).await;
    assert_eq!(origin.status, STATUS_PENDING);

    let new_id = copy_transfer(origin_id, &copy_claims(&[COPY_PERMISSION]))
        .await
        .expect("复制应成功");
    let copied = refresh(new_id).await;
    assert_ne!(copied.id, origin.id);
    assert_ne!(copied.transfer_no, origin.transfer_no, "单号必须是新的");
    assert!(
        copied.transfer_no.starts_with("TK-"),
        "{}",
        copied.transfer_no
    );
    assert_eq!(copied.status, STATUS_DRAFT, "复制出来的必须是草稿");
    assert_eq!(copied.from_warehouse_id, origin.from_warehouse_id);
    assert_eq!(copied.to_warehouse_id, origin.to_warehouse_id);
    assert_eq!(copied.reason, origin.reason, "原因要带出");
    assert_eq!(copied.remark, origin.remark, "备注要带出");
    assert_eq!(copied.total_qty, origin.total_qty, "数量合计要一致");
    // 状态带出的时间戳 / 审批关联全清（`ck_biz_transfer_state` 要求 DRAFT 全 NULL）
    assert!(copied.approval_instance_id.is_none());
    assert!(copied.submitted_at.is_none());
    assert!(copied.approved_at.is_none());
    assert!(copied.shipped_at.is_none());
    assert!(copied.received_at.is_none());
    assert!(copied.cancelled_at.is_none());
    assert!(copied.cancel_reason.is_none());
    assert!(copied.approver_id.is_none());
    assert!(copied.transit_warehouse_id.is_none());

    // 明细逐行一致（物料 / 数量 / 库位）
    let origin_items = transfer_model::items_of(origin_id).await.unwrap();
    let copied_items = transfer_model::items_of(new_id).await.unwrap();
    assert_eq!(copied_items.len(), origin_items.len());
    for (new_line, old_line) in copied_items.iter().zip(origin_items.iter()) {
        assert_eq!(new_line.material_id, old_line.material_id);
        assert_eq!(new_line.qty, old_line.qty);
        assert_eq!(new_line.from_location_id, old_line.from_location_id);
        assert_eq!(new_line.to_location_id, old_line.to_location_id);
        assert_eq!(new_line.shipped_qty, Decimal::ZERO, "已发数量必须归零");
        assert_eq!(new_line.received_qty, Decimal::ZERO, "已收数量必须归零");
    }
    // 序列号不复制（原单没带号码，这条保证「复制也不凭空造」）
    assert!(
        transfer_model::serials_of(new_id).await.unwrap().is_empty(),
        "复制不复制序列号"
    );
    // 原单不变：还是 PENDING，审批实例还在
    let origin_after = refresh(origin_id).await;
    assert_eq!(origin_after.status, STATUS_PENDING);
    assert_eq!(origin_after.transfer_no, origin.transfer_no);
    assert_eq!(
        origin_after.approval_instance_id,
        origin.approval_instance_id
    );
    assert_eq!(
        origin_after.updated_at, origin.updated_at,
        "原单 updated_at 不许变"
    );
}

/// ④b 复制不带建单权限 → 403；权限码必须与建单同一个。
#[tokio::test]
async fn d9_transfer_copy_requires_create_permission() {
    let Some(_guard) = prepare().await else {
        return;
    };
    assert_eq!(COPY_PERMISSION, "business:erp:transfer:create");
    let origin_id = create_transfer(
        &body(vec![item(M_PLAIN, "1")]),
        Some(uuid(U_APPLICANT)),
        None,
    )
    .await
    .expect("建单应成功");
    let err = copy_transfer(origin_id, &copy_claims(&["business:erp:transfer:list"]))
        .await
        .expect_err("没有建单权限必须 403");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert!(
        err.msg.contains("business:erp:transfer:create"),
        "403 文案要点名权限码：{}",
        err.msg
    );
}
