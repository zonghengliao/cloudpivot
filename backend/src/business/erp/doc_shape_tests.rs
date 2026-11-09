//! 出参三段式口径的**值一致性**用例（E2E01-FIX-4 验收 ②）。
//!
//! 本卡只做加法：新字段 `header` / `items` / `meta` 与旧字段并存。验收要求
//! 「旧字段值 = 新字段值（不是两套逻辑）」，所以这里不测「字段存在」，而是把整份
//! 响应序列化成 JSON，逐字段比较 `header == 旧单头字段`、`items == 旧明细字段`。
//!
//! 三个被统一的接口：
//! - `POST /business/erp/purchase-requisition/{id}/to-po` → [`ToPoRes`]
//! - `POST /business/erp/purchase-order/{id}/submit-approval` → [`PurchaseOrderDetailRes`]
//!   （同一个 `detail_of` 也喂 GET 详情 / 建单 / 改单 / 状态流转）
//! - `POST /business/erp/price-inquiry` → [`PriceInquiryDetailRes`]（同样喂详情 / 报价 / 定标）

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;
use serde_json::json;

use crate::business::erp::doc_shape::DocMeta;
use crate::business::erp::dto::PurchaseOrderRes;
use crate::business::erp::model::{PurchaseOrderItemRow, PurchaseOrderRow};
use crate::business::erp::price_inquiry_dto::{PriceInquiryDetailRes, PriceInquiryItemRes, PriceInquiryRowRes};
use crate::business::erp::purchase_dto::{PurchaseOrderDetailRes, PurchaseOrderLineRes};
use crate::business::erp::purchase_requisition_dto::{CreatedPurchaseOrderRes, MergedLineRes, ToPoRes};

fn dec(text: &str) -> Decimal {
    text.parse().expect("测试用定点数")
}

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 9).expect("固定日期")
}

// ==================== ① 采购申请 → 采购订单 ====================

fn created_po() -> CreatedPurchaseOrderRes {
    CreatedPurchaseOrderRes {
        po_id: Uuid::now_v7().to_string(),
        po_no: "PO-20261109-000001".to_string(),
        supplier_id: Uuid::now_v7().to_string(),
        supplier_name: "测试供应商".to_string(),
        line_count: 1,
        total_with_tax: dec("113.00"),
    }
}

fn merged_line(po_item_id: &str) -> MergedLineRes {
    MergedLineRes {
        line_no: 1,
        material_id: Uuid::now_v7().to_string(),
        material_code: "M-001".to_string(),
        material_name: "测试物料".to_string(),
        qty: dec("2"),
        est_price: dec("50"),
        tax_rate: dec("13"),
        demand_date: Some(date()),
        source_req_nos: vec!["PRQ-20261109-000001".to_string()],
        source_item_ids: vec![Uuid::now_v7().to_string()],
        po_item_id: Some(po_item_id.to_string()),
    }
}

fn to_po_res() -> ToPoRes {
    let header = created_po();
    let items = vec![merged_line("11111111-1111-1111-1111-111111111111")];
    let meta = DocMeta::empty()
        .with_extra("merged_count", json!(2))
        .with_extra("requisition_nos", json!(["PRQ-1", "PRQ-2"]));
    ToPoRes::new(header, items, meta, vec!["PRQ-1".into(), "PRQ-2".into()], 2)
}

#[test]
fn to_po_header_and_items_mirror_legacy_fields() {
    let value = serde_json::to_value(to_po_res()).expect("序列化");
    assert_eq!(value["header"], value["purchase_order"], "新单头必须与旧字段同值");
    assert_eq!(value["items"], value["lines"], "新明细必须与旧字段同值");
    // 旧字段本身没被删（破坏性变更不在本卡范围）
    assert!(value["purchase_order"].is_object());
    assert!(value["lines"].is_array());
}

#[test]
fn to_po_meta_carries_merge_statistics() {
    let value = serde_json::to_value(to_po_res()).expect("序列化");
    assert_eq!(value["meta"]["merged_count"], json!(2));
    assert_eq!(value["meta"]["requisition_nos"].as_array().expect("数组").len(), 2);
    // meta 与旧字段内容一致（同一份数据，不是另算一遍）
    assert_eq!(value["meta"]["merged_count"], value["merged_count"]);
}

#[test]
fn to_po_keeps_legacy_top_level_statistics() {
    let value = serde_json::to_value(to_po_res()).expect("序列化");
    assert_eq!(value["merged_count"], json!(2));
    assert_eq!(value["requisition_nos"], json!(["PRQ-1", "PRQ-2"]));
}

// ==================== ② 采购订单（提交审批 / 详情共用） ====================

fn po_row() -> PurchaseOrderRow {
    PurchaseOrderRow {
        id: Uuid::now_v7(),
        order_no: "PO-20261109-000002".to_string(),
        supplier_id: Uuid::now_v7(),
        order_date: date(),
        expect_date: None,
        warehouse_id: Uuid::now_v7(),
        status: "SUBMITTED".to_string(),
        currency_code: "CNY".to_string(),
        total_qty: dec("2"),
        total_amount: dec("100"),
        total_tax: dec("13"),
        total_with_tax: dec("113"),
        credit_days: 0,
        remark: None,
        price_mode: "EXCL".to_string(),
        submitted_at: None,
        submitted_by: None,
        approved_at: None,
        approved_by: None,
        closed_at: None,
        closed_by: None,
        approval_instance_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        create_by: None,
        update_by: None,
        deleted_at: None,
        purchase_type: "SPOT".to_string(),
        contract_id: None,
        contract_no_snapshot: None,
        supplier_name_snapshot: None,
        warehouse_name_snapshot: None,
        project_id: None,
        over_credit_limit: false,
        credit_exposure: None,
        credit_limit_snapshot: None,
    }
}

fn po_line() -> PurchaseOrderLineRes {
    PurchaseOrderLineRes::from(&PurchaseOrderItemRow {
        id: Uuid::now_v7(),
        order_id: Uuid::now_v7(),
        line_no: 1,
        material_id: Uuid::now_v7(),
        unit_id: Uuid::now_v7(),
        qty: dec("2"),
        price: dec("50"),
        price_incl: dec("56.5"),
        tax_rate: dec("13"),
        amount: dec("100"),
        tax_amount: dec("13"),
        with_tax_amount: dec("113"),
        received_qty: Decimal::ZERO,
        remark: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
}

fn po_detail() -> PurchaseOrderDetailRes {
    let meta = DocMeta::empty().with_instance(
        Some("22222222-2222-2222-2222-222222222222".to_string()),
        Some("FLW-20261109-000001".to_string()),
        Some("PENDING".to_string()),
    );
    PurchaseOrderDetailRes::new(PurchaseOrderRes::from(&po_row()), vec![po_line()], meta)
}

#[test]
fn purchase_order_detail_header_mirrors_legacy_order() {
    let value = serde_json::to_value(po_detail()).expect("序列化");
    assert_eq!(value["header"], value["order"], "新单头必须与旧字段 order 同值");
    assert!(value["order"].is_object(), "旧字段 order 不能删");
    // items 本来就是新口径的名字，这里钉住它没被顺带改名
    assert_eq!(value["items"].as_array().expect("明细数组").len(), 1);
}

#[test]
fn purchase_order_detail_meta_exposes_approval_instance() {
    let value = serde_json::to_value(po_detail()).expect("序列化");
    assert_eq!(
        value["meta"]["instance_id"],
        json!("22222222-2222-2222-2222-222222222222")
    );
    assert_eq!(value["meta"]["instance_no"], json!("FLW-20261109-000001"));
    assert_eq!(value["meta"]["flow_status"], json!("PENDING"));
}

#[test]
fn purchase_order_detail_without_instance_has_empty_meta() {
    let res = PurchaseOrderDetailRes::new(PurchaseOrderRes::from(&po_row()), Vec::new(), DocMeta::empty());
    let value = serde_json::to_value(&res).expect("序列化");
    assert_eq!(
        value["meta"],
        json!({}),
        "没走审批的单据 meta 是空对象，前端不用判 undefined"
    );
    assert!(value["header"].is_object());
}

// ==================== ③ 询价单（POST /price-inquiry 与详情共用） ====================

fn inquiry_head() -> PriceInquiryRowRes {
    PriceInquiryRowRes {
        id: Uuid::now_v7().to_string(),
        inquiry_no: "PIQ-20261109-000001".to_string(),
        inquiry_date: date(),
        buyer_id: None,
        buyer_name: None,
        warehouse_id: Uuid::now_v7().to_string(),
        warehouse_name: "主仓".to_string(),
        status: "DRAFT".to_string(),
        status_label: "草稿".to_string(),
        item_count: 1,
        supplier_count: 0,
        award_mode: None,
        award_mode_label: String::new(),
        award_supplier_id: None,
        award_supplier_name: None,
        remark: None,
        awarded_at: None,
        po_refs: json!([]),
        created_at: "2026-11-09T00:00:00Z".to_string(),
    }
}

fn inquiry_item() -> PriceInquiryItemRes {
    PriceInquiryItemRes {
        id: Uuid::now_v7().to_string(),
        line_no: 1,
        material_id: Uuid::now_v7().to_string(),
        material_code: "M-001".to_string(),
        material_name: "测试物料".to_string(),
        unit_id: Uuid::now_v7().to_string(),
        unit_name: "个".to_string(),
        qty: dec("3"),
        demand_date: Some(date()),
        remark: None,
    }
}

fn inquiry_detail() -> PriceInquiryDetailRes {
    PriceInquiryDetailRes::new(
        inquiry_head(),
        vec![inquiry_item()],
        Vec::new(),
        Vec::new(),
        false,
        Some("还没定标".to_string()),
    )
}

#[test]
fn price_inquiry_header_mirrors_legacy_head() {
    let value = serde_json::to_value(inquiry_detail()).expect("序列化");
    assert_eq!(value["header"], value["head"], "新单头必须与旧字段 head 同值");
    assert!(value["head"].is_object(), "旧字段 head 不能删");
    assert_eq!(value["items"].as_array().expect("明细数组").len(), 1);
}

#[test]
fn price_inquiry_meta_summarises_quotes_without_duplicating_them() {
    let value = serde_json::to_value(inquiry_detail()).expect("序列化");
    assert_eq!(value["meta"]["quote_count"], json!(0));
    assert_eq!(value["meta"]["comparison_count"], json!(0));
    assert_eq!(value["meta"]["can_to_po"], json!(false));
    // 大体量数组只在顶层下发一份（meta 只给计数）
    assert!(value["quotes"].is_array());
    assert!(value["meta"].get("quotes").is_none());
    assert_eq!(value["to_po_blocked_reason"], json!("还没定标"));
}
