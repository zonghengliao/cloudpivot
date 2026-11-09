//! 询比价接口层（P2-01）：建询价单 / 分页 / 详情 / 录报价 / 定标 / 转采购订单。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/price-inquiry/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/price-inquiry` | `business:erp:price-inquiry:create` | 建单（`PIQ-yyyyMMdd-000001`，`DRAFT`） |
//! | GET | `/business/erp/price-inquiry/page` | `business:erp:price-inquiry:list` | 分页 |
//! | GET | `/business/erp/price-inquiry/{id}` | `business:erp:price-inquiry:list` | 详情（明细 + 报价 + **比价表**） |
//! | POST | `/business/erp/price-inquiry/{id}/quote` | `business:erp:price-inquiry:quote` | 录入一家供应商的报价（重复录入 = 改价） |
//! | POST | `/business/erp/price-inquiry/{id}/award` | `business:erp:price-inquiry:award` | 定标（至少 2 家 / 单源例外） |
//! | POST | `/business/erp/price-inquiry/{id}/to-po` | `business:erp:price-inquiry:to-po` | 按中标供应商分组生成采购订单 |
//!
//! # 与卡面路径的差异
//!
//! 卡面写 `POST /erp/price-inquiry`，真实前缀是 `/api/business/erp/price-inquiry`
//! （`[router].prefix = /api` + 业务容器 `business`，与采购订单一致）。
//! 前端 API 层必须写全 `/api/...`，否则命中 SPA fallback 返回 HTML。
//!
//! # 三条业务规则（每条都有落库证据）
//!
//! ```text
//! ① 至少 2 家报价才能定标：按**明细行**数报价家数，不足 2 家时：
//!    没给 single_source_reason（≥10 字）→ 409 code=66047，给了一句都不写库；
//!    给了 → award_mode='SINGLE_SOURCE' + award_reason 留痕（例外可审计，不是静默放过）。
//! ② 定标价 → 采购单价的传递：to-po 时 `biz_purchase_order_item.price_excl` 直接取
//!    中标报价的 `price_excl`（订单金额仍由采购模块服务端现算，本卡不写金额列）。
//! ③ 一单多单：award 按明细行表达中标供应商，to-po 按供应商分组 ⇒ 一张询价单
//!    拆成 N 张采购订单（同一供应商的多行合成一张，不拆碎）。
//! ```
//!
//! # 幂等
//!
//! * 报价：`(item_id, supplier_id)` 唯一 + upsert ⇒ 重复提交不产生第二行；
//! * 定标：状态必须是 `DRAFT` / `QUOTING`，重复定标 409；
//! * 转单：`SELECT … FOR UPDATE` 锁单头 + 状态必须 `AWARDED` ⇒ 并发/重复转单只有一次成功，
//!   第二次拿到的是 `CONVERTED` 的 409（不会生成第二套采购订单）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_price_inquiry / _item / _quote 是 ERP 单据表（V0321 无 dept_id、
//!         无归属人列），可见性由 business:erp:price-inquiry:* 权限码控制；
//!         分页走 model::page_inquiries（内部 scope::paginate_unscoped，闸①口径）。
//! ```

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::price_inquiry_dto::{
    AWARD_MODE_MULTI, AWARD_MODE_SINGLE_SOURCE, AwardPair, AwardReq, CompareQuoteRes, CompareRowRes,
    CreatePriceInquiryReq, CreatedPurchaseOrderRes, MAX_LINES, PRICE_INQUIRY_PERMISSIONS, PriceInquiryDetailRes,
    PriceInquiryItemRes, PriceInquiryPageQuery, PriceInquiryRowRes, PriceQuoteRes, QuoteReq, STATUS_AWARDED,
    STATUS_CONVERTED, STATUS_DRAFT, STATUS_QUOTING, ToPoReq, ToPoRes, award_mode_label, check_award_reason,
    check_delivery_days, check_price, check_qty, check_tax, clean_remark, group_by_supplier, parse_uuid_field,
    price_incl_of, reason_is_long_enough, resolve_award, single_source_items, status_label, today_cst,
    unmatched_awards,
};
use crate::business::erp::price_inquiry_model::{self as model, ItemRow, NewInquiry, NewItem, NewQuote, QuoteRow};
use crate::business::erp::purchase_dto::{CreatePurchaseOrderReq, PurchaseOrderItemReq};
use crate::business::erp::purchase_handler;
use crate::business::erp::purchase_model::{self, PRICE_MODE_EXCL};
use crate::business::erp::unit_guard;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0321 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:price-inquiry:list";
/// 新建询价单。
pub const PERM_CREATE: &str = "business:erp:price-inquiry:create";
/// 录入报价。
pub const PERM_QUOTE: &str = "business:erp:price-inquiry:quote";
/// 定标。
pub const PERM_AWARD: &str = "business:erp:price-inquiry:award";
/// 转采购订单。
pub const PERM_TO_PO: &str = "business:erp:price-inquiry:to-po";

// 代码里断言一份常量与 DTO 的清单一致：`perm_menu_gate` 只认字面量，
// 但两份清单漂移时这里会红，不会等到运行时 403。
const _: () = assert!(PRICE_INQUIRY_PERMISSIONS.len() == 5);

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(code: i32, message: impl Into<String>) -> ApiError {
    ApiError::new(409, code, message.into())
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("询价单不存在或已删除（{id}）"),
    )
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

// ==================== ① 建询价单 ====================

/// 建单核心逻辑（handler 与单测共用，只跳过认证）。
pub async fn create_inquiry(
    req: &CreatePriceInquiryReq,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    let warehouse_id = parse_uuid_field(req.warehouse_id.as_deref(), "warehouse_id")?;
    let warehouse_name = purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    let _ = warehouse_name; // 名称由详情接口 join 派生，这里只需存在性

    if req.items.is_empty() {
        return Err(bad_request("items 不能为空：至少填 1 个物料"));
    }
    if req.items.len() > MAX_LINES {
        return Err(bad_request(format!(
            "items 最多 {MAX_LINES} 行（收到 {}）",
            req.items.len()
        )));
    }

    // 采购员：缺省 = 当前登录人；姓名一律服务端快照（前端传的名字不可信）
    let buyer_id = match req.buyer_id.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(raw) => Some(parse_uuid_field(Some(raw), "buyer_id")?),
        None => operator,
    };
    let buyer_name = match buyer_id {
        Some(id) => user_name_of(id).await?,
        None => None,
    };

    let mut items = Vec::with_capacity(req.items.len());
    for (index, line) in req.items.iter().enumerate() {
        let material_id = parse_uuid_field(line.material_id.as_deref(), &format!("items[{index}].material_id"))?;
        let qty = check_qty(line.qty, &format!("items[{index}].qty"))?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        // DC-09：单位一律由物料基础单位带出（折算单位会让「5 箱」记成「5 个」）
        unit_guard::ensure_base_unit(material_id, index, qty).await?;
        items.push(NewItem {
            material_id,
            unit_id: material.unit_id,
            qty,
            demand_date: line.demand_date,
            remark: clean_remark(line.remark.as_deref())?,
        });
    }

    let head = NewInquiry {
        inquiry_date: req.inquiry_date.unwrap_or_else(today_cst),
        buyer_id,
        buyer_name,
        warehouse_id,
        remark: clean_remark(req.remark.as_deref())?,
    };
    model::create_inquiry(&head, &items, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "创建询价单"))
}

/// 用户显示名（`real_name` 优先，其次 `username`）。
async fn user_name_of(id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username) AS name FROM sys_user \
              WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询采购员"))?;
    match rows.first() {
        None => Err(reference_missing(format!("buyer_id 用户不存在（{id}）"))),
        Some(row) => Ok(row.get_typed("name")?),
    }
}

/// 新建询价单。
#[nik::permission(
    path = "/business/erp/price-inquiry",
    method = "POST",
    permission = "business:erp:price-inquiry:create"
)]
#[nik::post("/business/erp/price-inquiry")]
pub async fn price_inquiry_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreatePriceInquiryReq>,
) -> ResResult<PriceInquiryDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_inquiry(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 分页 ====================

/// 分页核心逻辑。
pub async fn page_inquiries(
    query: PriceInquiryPageQuery,
) -> std::result::Result<PageData<Vec<PriceInquiryRowRes>>, ApiError> {
    let mut filter = query.filter()?;
    let (page, page_size) = normalize_page(filter.page, filter.page_size);
    filter.page = page;
    filter.page_size = page_size;
    let (rows, total) = model::page_inquiries(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询询价单"))?;
    Ok(PageData {
        list: rows.iter().map(to_row_res).collect(),
        total,
        page,
        page_size,
    })
}

/// 询价单分页。
#[nik::permission(
    path = "/business/erp/price-inquiry/page",
    method = "GET",
    permission = "business:erp:price-inquiry:list"
)]
#[nik::get("/business/erp/price-inquiry/page")]
pub async fn price_inquiry_page(
    Query(query): Query<PriceInquiryPageQuery>,
) -> ResResult<PageData<Vec<PriceInquiryRowRes>>> {
    Ok(Res::ok(page_inquiries(query).await?))
}

// ==================== ③ 详情（含比价表） ====================

/// 详情核心逻辑：单头 + 明细 + 报价 + **横向比价表**。
pub async fn detail_of(id: Uuid) -> std::result::Result<PriceInquiryDetailRes, ApiError> {
    let head = model::find_inquiry(id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价单"))?
        .ok_or_else(|| not_found(id))?;
    let items = model::list_items(id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价明细"))?;
    let quotes = model::list_quotes(id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价"))?;

    let comparison = build_comparison(&items, &quotes);
    let (can_to_po, blocked) = to_po_gate(&head.status, &items, &quotes);
    // 出参三段式（E2E01-FIX-4）：header/items/meta 与旧字段 head 由 `new` 一次成型
    Ok(PriceInquiryDetailRes::new(
        to_row_res(&head),
        items.iter().map(to_item_res).collect(),
        quotes.iter().map(to_quote_res).collect(),
        comparison,
        can_to_po,
        blocked,
    ))
}

/// 转单前置判定（详情页按钮可用性 + to-po 的同一份口径）。
fn to_po_gate(status: &str, items: &[ItemRow], quotes: &[QuoteRow]) -> (bool, Option<String>) {
    if status == STATUS_CONVERTED {
        return (false, Some("该询价单已转采购订单，不能重复转单".to_string()));
    }
    if status != STATUS_AWARDED {
        return (
            false,
            Some(format!(
                "当前状态是「{}」（{status}），只有已定标（AWARDED）的询价单可以转采购订单",
                status_label(status)
            )),
        );
    }
    let awarded: std::collections::BTreeSet<Uuid> = quotes
        .iter()
        .filter(|quote| quote.is_awarded)
        .map(|quote| quote.item_id)
        .collect();
    let missing: Vec<i32> = items
        .iter()
        .filter(|item| !awarded.contains(&item.id))
        .map(|item| item.line_no)
        .collect();
    if !missing.is_empty() {
        return (
            false,
            Some(format!(
                "以下明细没有中标报价，无法转单：第 {} 行",
                missing.iter().map(i32::to_string).collect::<Vec<_>>().join("、")
            )),
        );
    }
    (true, None)
}

/// 比价表：每条明细一行，横向列出所有报价（最低价打标）。
fn build_comparison(items: &[ItemRow], quotes: &[QuoteRow]) -> Vec<CompareRowRes> {
    let mut by_item: BTreeMap<Uuid, Vec<&QuoteRow>> = BTreeMap::new();
    for quote in quotes {
        by_item.entry(quote.item_id).or_default().push(quote);
    }
    items
        .iter()
        .map(|item| {
            let rows = by_item.get(&item.id).cloned().unwrap_or_default();
            let lowest = rows.iter().map(|quote| quote.price_excl).min().unwrap_or(Decimal::ZERO);
            CompareRowRes {
                item_id: item.id.to_string(),
                line_no: item.line_no,
                material_id: item.material_id.to_string(),
                material_code: item.material_code.clone(),
                material_name: item.material_name.clone(),
                unit_name: item.unit_name.clone(),
                qty: item.qty,
                demand_date: item.demand_date,
                bidder_count: rows.len(),
                has_competition: rows.len() >= crate::business::erp::price_inquiry_dto::MIN_BIDDERS,
                quotes: rows
                    .iter()
                    .map(|quote| CompareQuoteRes {
                        quote_id: quote.id.to_string(),
                        supplier_id: quote.supplier_id.to_string(),
                        supplier_name: quote.supplier_name.clone(),
                        price_excl: quote.price_excl,
                        price_incl: quote.price_incl,
                        tax_rate: quote.tax_rate,
                        delivery_days: quote.delivery_days,
                        delivery_date: quote.delivery_date,
                        remark: quote.remark.clone(),
                        is_lowest: quote.price_excl == lowest,
                        is_awarded: quote.is_awarded,
                    })
                    .collect(),
            }
        })
        .collect()
}

/// 询价单详情。
#[nik::permission(
    path = "/business/erp/price-inquiry/{id}",
    method = "GET",
    permission = "business:erp:price-inquiry:list"
)]
#[nik::get("/business/erp/price-inquiry/{id}")]
pub async fn price_inquiry_detail(Path(id): Path<String>) -> ResResult<PriceInquiryDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 录入报价 ====================

/// 录报价核心逻辑。
///
/// * 状态必须是 `DRAFT` / `QUOTING`（已定标/已转单的单子再改价会让中标价与采购单分叉）；
/// * 每条明细必须属于本询价单（否则报价会挂到别的单上）；
/// * 同一供应商重复提交 = 改价（`(item_id, supplier_id)` 唯一）。
pub async fn record_quote(
    inquiry_id: Uuid,
    req: &QuoteReq,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Vec<QuoteRow>, ApiError> {
    let head = model::find_inquiry(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价单"))?
        .ok_or_else(|| not_found(inquiry_id))?;
    if head.status != STATUS_DRAFT && head.status != STATUS_QUOTING {
        return Err(conflict(
            error_code::RESOURCE_CONFLICT,
            format!(
                "询价单 {} 当前是「{}」（{}），不能再录报价",
                head.inquiry_no,
                status_label(&head.status),
                head.status
            ),
        ));
    }
    let supplier_id = parse_uuid_field(req.supplier_id.as_deref(), "supplier_id")?;
    if purchase_model::supplier_ref(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .is_none()
    {
        return Err(reference_missing("supplier_id 供应商不存在或已删除"));
    }
    if req.items.is_empty() {
        return Err(bad_request("items 不能为空：至少给 1 条明细报价"));
    }
    if req.items.len() > MAX_LINES {
        return Err(bad_request(format!(
            "items 最多 {MAX_LINES} 行（收到 {}）",
            req.items.len()
        )));
    }

    let items = model::list_items(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价明细"))?;
    let item_ids: std::collections::BTreeSet<Uuid> = items.iter().map(|item| item.id).collect();

    let mut quotes = Vec::with_capacity(req.items.len());
    let mut seen: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for (index, line) in req.items.iter().enumerate() {
        let item_id = parse_uuid_field(line.item_id.as_deref(), &format!("items[{index}].item_id"))?;
        if !item_ids.contains(&item_id) {
            return Err(bad_request(format!(
                "items[{index}].item_id（{item_id}）不属于询价单 {}",
                head.inquiry_no
            )));
        }
        if !seen.insert(item_id) {
            return Err(bad_request(format!(
                "items[{index}].item_id（{item_id}）重复：同一供应商对同一条明细只能报一个价"
            )));
        }
        let price_excl = check_price(line.price_excl, &format!("items[{index}].price_excl"))?;
        let tax_rate = check_tax(line.tax_rate)?;
        quotes.push(NewQuote {
            item_id,
            price_excl,
            price_incl: price_incl_of(price_excl, tax_rate),
            tax_rate,
            delivery_days: check_delivery_days(line.delivery_days)?,
            delivery_date: line.delivery_date,
            remark: clean_remark(line.remark.as_deref())?,
        });
    }

    model::upsert_quotes(supplier_id, &quotes, operator, now)
        .await
        .map_err(|error| utils::db_error(error, "录入报价"))?;
    // 首次录入把 DRAFT 推进到 QUOTING（失败不影响报价已写入的事实，但要留日志）
    if let Err(error) = model::mark_quoting(inquiry_id, operator, now).await {
        tracing::warn!(%error, inquiry_id = %inquiry_id, "询价单状态推进 QUOTING 失败（报价已写入）");
    }
    model::list_quotes(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "回读报价"))
}

/// 录入报价。
#[nik::permission(
    path = "/business/erp/price-inquiry/{id}/quote",
    method = "POST",
    permission = "business:erp:price-inquiry:quote"
)]
#[nik::post("/business/erp/price-inquiry/{id}/quote")]
pub async fn price_inquiry_quote(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<QuoteReq>,
) -> ResResult<PriceInquiryDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    record_quote(id, &req, operator, Utc::now()).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑤ 定标 ====================

/// 定标核心逻辑。
///
/// 判定顺序（每一步都有 409 的具体文案，前端不用自己拼）：
/// 1. 状态必须是 `DRAFT` / `QUOTING`；
/// 2. 中标组合必须覆盖全部明细，且每条都「该供应商真的报过价」；
/// 3. 每条明细不足 2 家报价 → 必须给 ≥10 字的单源例外理由，否则 409 `66047`。
pub async fn award_inquiry(
    inquiry_id: Uuid,
    req: &AwardReq,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let head = model::find_inquiry(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价单"))?
        .ok_or_else(|| not_found(inquiry_id))?;
    if head.status != STATUS_DRAFT && head.status != STATUS_QUOTING {
        return Err(conflict(
            error_code::ERP_PRICE_INQUIRY_AWARD_GUARD,
            format!(
                "询价单 {} 当前是「{}」（{}），不能重复定标",
                head.inquiry_no,
                status_label(&head.status),
                head.status
            ),
        ));
    }
    let items = model::list_items(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价明细"))?;
    let item_ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let pairs = resolve_award(req, &item_ids)?;

    let quotes = model::list_quotes(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价"))?;
    let quoted: std::collections::BTreeSet<(Uuid, Uuid)> =
        quotes.iter().map(|quote| (quote.item_id, quote.supplier_id)).collect();
    let unmatched = unmatched_awards(&quoted, &pairs);
    if !unmatched.is_empty() {
        let line_of: BTreeMap<Uuid, i32> = items.iter().map(|item| (item.id, item.line_no)).collect();
        let detail = unmatched
            .iter()
            .map(|pair| {
                format!(
                    "第 {} 行（{}）",
                    line_of.get(&pair.item_id).copied().unwrap_or(0),
                    pair.supplier_id
                )
            })
            .collect::<Vec<_>>()
            .join("、");
        return Err(conflict(
            error_code::ERP_PRICE_INQUIRY_AWARD_GUARD,
            format!("以下中标明细没有对应报价，请先让该供应商报价：{detail}"),
        ));
    }

    let per_item = model::quoted_suppliers_per_item(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "统计报价家数"))?;
    let gaps = single_source_items(&per_item, &pairs);
    let reason = check_award_reason(req.single_source_reason.as_deref());
    let mode = if gaps.is_empty() {
        AWARD_MODE_MULTI
    } else {
        if !reason_is_long_enough(reason.as_deref()) {
            let line_of: BTreeMap<Uuid, i32> = items.iter().map(|item| (item.id, item.line_no)).collect();
            let lines = gaps
                .iter()
                .map(|id| line_of.get(id).copied().unwrap_or(0).to_string())
                .collect::<Vec<_>>()
                .join("、");
            return Err(conflict(
                error_code::ERP_PRICE_INQUIRY_AWARD_GUARD,
                format!(
                    "至少 2 家报价才能定标：第 {lines} 行只有 1 家（或没有）报价。\
                     单源采购请填 single_source_reason（≥10 字）走例外留痕"
                ),
            ));
        }
        AWARD_MODE_SINGLE_SOURCE
    };

    // 单一供应商定标才写 award_supplier_id（多供应商定标以明细 is_awarded 为准）
    let distinct: std::collections::BTreeSet<Uuid> = pairs.iter().map(|pair| pair.supplier_id).collect();
    let primary_supplier = if distinct.len() == 1 {
        distinct.iter().next().copied()
    } else {
        None
    };

    let rows: Vec<(Uuid, Uuid)> = pairs.iter().map(|pair| (pair.item_id, pair.supplier_id)).collect();
    let affected = model::award(
        inquiry_id,
        &rows,
        mode,
        reason.as_deref(),
        primary_supplier,
        operator,
        now,
    )
    .await
    .map_err(|error| utils::db_error(error, "定标"))?;
    if affected == 0 {
        // 报价行一条都没置位 = 数据在判定后被并发改过；宁可报错也不留「已定标但没有中标行」
        return Err(conflict(
            error_code::ERP_PRICE_INQUIRY_AWARD_GUARD,
            "定标时没有匹配到任何报价行（可能被并发改价/删除），请刷新后重试",
        ));
    }
    Ok(())
}

/// 定标。
#[nik::permission(
    path = "/business/erp/price-inquiry/{id}/award",
    method = "POST",
    permission = "business:erp:price-inquiry:award"
)]
#[nik::post("/business/erp/price-inquiry/{id}/award")]
pub async fn price_inquiry_award(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AwardReq>,
) -> ResResult<PriceInquiryDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    award_inquiry(id, &req, operator, Utc::now()).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑥ 转采购订单 ====================

/// 转单核心逻辑（按中标供应商分组 → 一组一张采购订单）。
///
/// 单价口径：采购单 `price_mode = EXCL`，每行 `price_excl` 取中标报价的
/// `price_excl`，`tax_rate` 同步带过去 —— 订单金额由采购模块服务端现算。
pub async fn to_purchase_orders(
    inquiry_id: Uuid,
    req: &ToPoReq,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<ToPoRes, ApiError> {
    // ① 锁单头：并发/重复转单只能成功一次
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启转单事务"))?;
    let locked = async { model::lock_status(&tx, inquiry_id).await }.await;
    let status = match locked {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found(inquiry_id));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定询价单"));
        }
    };
    if status != STATUS_AWARDED {
        let _ = tx.rollback().await;
        return Err(conflict(
            error_code::RESOURCE_CONFLICT,
            if status == STATUS_CONVERTED {
                "该询价单已转采购订单，不能重复转单".to_string()
            } else {
                format!(
                    "询价单当前是「{}」（{status}），必须先定标（AWARDED）才能转采购订单",
                    status_label(&status)
                )
            },
        ));
    }

    let head = model::find_inquiry(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价单"))?
        .ok_or_else(|| not_found(inquiry_id))?;
    let items = model::list_items(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询询价明细"))?;
    let quotes = model::list_quotes(inquiry_id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价"))?;

    let (can_to_po, blocked) = to_po_gate(&status, &items, &quotes);
    if !can_to_po {
        let _ = tx.rollback().await;
        return Err(conflict(
            error_code::RESOURCE_CONFLICT,
            blocked.unwrap_or_else(|| "当前询价单不满足转单条件".to_string()),
        ));
    }

    // ② 中标组合：明细 → (供应商, 报价)
    let mut awarded: BTreeMap<Uuid, &QuoteRow> = BTreeMap::new();
    for quote in &quotes {
        if quote.is_awarded {
            awarded.insert(quote.item_id, quote);
        }
    }
    let pairs: Vec<AwardPair> = awarded
        .iter()
        .map(|(item_id, quote)| AwardPair {
            item_id: *item_id,
            supplier_id: quote.supplier_id,
        })
        .collect();
    let groups = group_by_supplier(&pairs);

    let order_date = req.order_date.unwrap_or_else(today_cst);
    let warehouse_id = match req.warehouse_id.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => parse_uuid_field(Some(raw), "warehouse_id")?,
        None => head.warehouse_id,
    };
    let warehouse_name = purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    let _ = warehouse_name;
    if req.warehouse_id.is_some() {
        // 覆盖仓库也回写到询价单：转单后询价单与采购单的收货仓必须一致（否则对不上账）
        model::override_warehouse(&tx, inquiry_id, warehouse_id, operator, now)
            .await
            .map_err(|error| utils::db_error(error, "回写收货仓库"))?;
    }

    let item_by_id: BTreeMap<Uuid, &ItemRow> = items.iter().map(|item| (item.id, item)).collect();
    let mut created: Vec<CreatedPurchaseOrderRes> = Vec::with_capacity(groups.len());
    let mut po_refs: Vec<serde_json::Value> = Vec::with_capacity(groups.len());

    for (supplier_id, item_ids) in &groups {
        let mut po_items: Vec<PurchaseOrderItemReq> = Vec::with_capacity(item_ids.len());
        let mut expect_date = None;
        for item_id in item_ids {
            let quote = awarded
                .get(item_id)
                .copied()
                .ok_or_else(|| bad_request(format!("明细 {item_id} 缺少中标报价")))?;
            let item = item_by_id
                .get(item_id)
                .copied()
                .ok_or_else(|| bad_request(format!("明细 {item_id} 不存在")))?;
            if expect_date.is_none() {
                expect_date = item.demand_date;
            }
            po_items.push(PurchaseOrderItemReq {
                material_id: Some(item.material_id.to_string()),
                qty: Some(item.qty),
                price_excl: Some(quote.price_excl),
                price_incl: None,
                tax_rate: Some(quote.tax_rate),
                remark: Some(format!("询价单 {} 第 {} 行（中标报价）", head.inquiry_no, item.line_no)),
            });
        }
        let remark = match req.remark.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            Some(text) => format!("{text}（由询价单 {} 转单）", head.inquiry_no),
            None => format!("由询价单 {} 定标转单（供应商分组）", head.inquiry_no),
        };
        let po_req = CreatePurchaseOrderReq {
            supplier_id: Some(supplier_id.to_string()),
            warehouse_id: Some(warehouse_id.to_string()),
            purchase_type: None, // 缺省 SPOT（零星采购），与询价单的定位一致
            contract_id: None,
            project_id: None,
            order_date: Some(order_date),
            expect_date,
            price_mode: Some(PRICE_MODE_EXCL.to_string()),
            credit_days: None,
            remark: Some(remark),
            items: po_items,
        };
        let po_id = purchase_handler::create_purchase_order(&po_req, operator).await?;
        let detail = purchase_handler::detail_of(po_id).await?;
        let supplier_name = model::supplier_name_of(db(), *supplier_id)
            .await
            .map_err(|error| utils::db_error(error, "查询供应商名"))?;
        created.push(CreatedPurchaseOrderRes {
            po_id: po_id.to_string(),
            po_no: detail.order.order_no.clone(),
            supplier_id: supplier_id.to_string(),
            supplier_name,
            line_count: item_ids.len(),
            total_with_tax: detail.order.total_with_tax,
        });
        po_refs.push(serde_json::json!({
            "po_id": po_id.to_string(),
            "po_no": detail.order.order_no,
            "supplier_id": supplier_id.to_string(),
            "line_count": item_ids.len(),
        }));
    }

    let refs = serde_json::Value::Array(po_refs);
    let changed = model::mark_converted(&tx, inquiry_id, &refs, operator, now)
        .await
        .map_err(|error| utils::db_error(error, "回写转单结果"))?;
    if changed == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            error_code::RESOURCE_CONFLICT,
            "转单状态回写失败（询价单已被并发修改），请刷新后重试",
        ));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交转单事务"))?;

    Ok(ToPoRes {
        inquiry_id: inquiry_id.to_string(),
        inquiry_no: head.inquiry_no,
        status: STATUS_CONVERTED.to_string(),
        purchase_orders: created,
    })
}

/// 转采购订单。
#[nik::permission(
    path = "/business/erp/price-inquiry/{id}/to-po",
    method = "POST",
    permission = "business:erp:price-inquiry:to-po"
)]
#[nik::post("/business/erp/price-inquiry/{id}/to-po")]
pub async fn price_inquiry_to_po(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ToPoReq>,
) -> ResResult<ToPoRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(to_purchase_orders(id, &req, operator, Utc::now()).await?))
}

// ==================== 出参转换 ====================

fn to_row_res(row: &model::InquiryRow) -> PriceInquiryRowRes {
    PriceInquiryRowRes {
        id: row.id.to_string(),
        inquiry_no: row.inquiry_no.clone(),
        inquiry_date: row.inquiry_date,
        buyer_id: row.buyer_id.map(|id| id.to_string()),
        buyer_name: row.buyer_name.clone(),
        warehouse_id: row.warehouse_id.to_string(),
        warehouse_name: row.warehouse_name.clone(),
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        item_count: row.item_count,
        supplier_count: row.supplier_count,
        award_mode: row.award_mode.clone(),
        award_mode_label: award_mode_label(row.award_mode.as_deref()).to_string(),
        award_supplier_id: row.award_supplier_id.map(|id| id.to_string()),
        award_supplier_name: row.award_supplier_name.clone().filter(|name| !name.is_empty()),
        remark: row.remark.clone(),
        awarded_at: row.awarded_at.map(|value| value.to_rfc3339()),
        po_refs: row.po_refs.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

fn to_item_res(item: &ItemRow) -> PriceInquiryItemRes {
    PriceInquiryItemRes {
        id: item.id.to_string(),
        line_no: item.line_no,
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        qty: item.qty,
        demand_date: item.demand_date,
        remark: item.remark.clone(),
    }
}

fn to_quote_res(quote: &QuoteRow) -> PriceQuoteRes {
    PriceQuoteRes {
        id: quote.id.to_string(),
        item_id: quote.item_id.to_string(),
        supplier_id: quote.supplier_id.to_string(),
        supplier_name: quote.supplier_name.clone(),
        price_excl: quote.price_excl,
        price_incl: quote.price_incl,
        tax_rate: quote.tax_rate,
        delivery_days: quote.delivery_days,
        delivery_date: quote.delivery_date,
        remark: quote.remark.clone(),
        is_awarded: quote.is_awarded,
    }
}
