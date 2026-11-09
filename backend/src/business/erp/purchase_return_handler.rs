//! 采购退货接口层（P2-03）：建单 / 分页 / 详情 / 可退明细 / 提交审批 / 确认退货 / 作废。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-return/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/purchase-return` | `…:create` | 建单（从入库单带出可退明细） |
//! | GET  | `/business/erp/purchase-return/page` | `…:list` | 分页 |
//! | GET  | `/business/erp/purchase-return/{id}` | `…:list` | 详情（单头 + 明细） |
//! | GET  | `/business/erp/purchase-return/source/{stock_in_id}` | `…:create` | 可退明细（选入库单带明细） |
//! | POST | `/business/erp/purchase-return/{id}/submit` | `…:submit` | 提交审批（`prt_approve`） |
//! | POST | `/business/erp/purchase-return/{id}/confirm` | `…:confirm` | 确认退货（出库 + 冲应付 + 凭证） |
//! | POST | `/business/erp/purchase-return/{id}/cancel` | `…:confirm` | 作废（草稿 / 已批准；执行权同确认） |
//!
//! # 三条硬规则（每条都有落库证据与单测）
//!
//! ```text
//! ① 防超退：可退量 = 入库量 − 已退量（未作废退货单累计，排除本单自己）
//! ② 价格锁死：退货单价一律取原入库明细快照，请求里给单价只用于校验
//! ③ 确认退货在同一事务里：出库单 + 三本账 + 应付冲减 + 供应商应退款 + 冲销凭证
//!    + 状态 CAS + 状态日志 + 联动（事务内 run_excluding / 提交后 run_deferred）
//! ```
//!
//! # 联动（卡面 §⑤）
//!
//! 触发点 `PURCHASE_RETURN/CONFIRMED` 已接入通用引擎；**排除动作**：
//!
//! | 动作码 | 退货侧的真实落账点 | 通用引擎再跑一次的后果 |
//! |--------|--------------------|------------------------|
//! | `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` | 确认事务 ③ 三本账 | 同一行扣两次库存、两条流水 |
//! | `UPDATE_COST` | 确认事务 ③ 明细成本快照 | 成本被二次覆盖 |
//! | `GENERATE_VOUCHER` | 确认事务 ⑤ 冲销凭证 | **凭证生凭证**（卡面明确要求排除） |
//! | `UPDATE_PAYABLE_BALANCE` | 确认事务 ④ 应付冲减 | 应付余额扣两次（`unpaid` 直接负数） |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_return / _item / biz_supplier_receivable 是 ERP 单据表，
//!        可见性由 business:erp:purchase-return:* 权限码控制；分页走
//!        model::page_returns（内部 scope::paginate_unscoped，闸①口径）。
//! ```

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::attachment::model::DocKind as AttachDocKind;
use crate::business::attachment::rule::{self as attach_rule, RequiredAt as AttachAt};
use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::erp::purchase_return_bridge;
use crate::business::erp::purchase_return_dto::{
    self as dto, CancelReturnReq, ConfirmReturnReq, CreateReturnReq, ReturnDetailRes, ReturnPageQuery,
    ReturnableItemRes, ReturnableSourceRes, STATUS_APPROVED, STATUS_CONFIRMED, STATUS_DRAFT, STATUS_SUBMITTED,
    SubmitReturnReq,
};
use crate::business::erp::purchase_return_model::{self as model, NewItem, NewReturn};
use crate::business::erp::purchase_return_posting;
use crate::business::flow::engine;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec as linkage_spec;
use crate::business::flow::model as flow_model;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// 采购退货状态通知的 `biz_type`（与审批流发的待办区分开：一个是被联动发的状态通知，
/// 一个是流程引擎的待办）。
pub const NOTICE_BIZ_TYPE: &str = "PURCHASE_RETURN";

// ==================== 权限码（与 V0332 的 sys_menu.auth_code 逐字一致） ====================
/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:purchase-return:list";
/// 新建退货单（含「可退明细」选择）。
pub const PERM_CREATE: &str = "business:erp:purchase-return:create";
/// 提交审批。
pub const PERM_SUBMIT: &str = "business:erp:purchase-return:submit";
/// 确认退货 / 作废（都动账务执行权）。
pub const PERM_CONFIRM: &str = "business:erp:purchase-return:confirm";

const _: () = assert!(dto::RETURN_PERMISSIONS.len() == 4);

/// 单据中文名（409 / 404 报文用）。
const LABEL: &str = "采购退货单";

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
        format!("采购退货单不存在或已删除（{id}）"),
    )
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

fn write_error(error: DbError, context: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{context}失败：{error}"))
}

fn submit_conflict(error: DbError, context: &str, return_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&error) {
        tracing::warn!(
            context,
            return_no,
            error = %error,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(LABEL, Some(return_no));
    }
    write_error(error, context)
}

// ==================== ① 建退货单 ====================

/// 来源入库单快照（建单 / 可退明细共用）。
#[derive(Debug, Clone)]
struct SourceStockIn {
    id: Uuid,
    in_no: String,
    in_type: String,
    source_order_id: Option<Uuid>,
    supplier_id: Option<Uuid>,
    warehouse_id: Uuid,
    in_date: chrono::NaiveDate,
    status: String,
    /// 该入库单过账时生成的应付单（退货冲减的对象）。
    payable_id: Option<Uuid>,
}

/// 读来源入库单（不存在 / 已删除 → 400 引用缺失）。
async fn load_stock_in(stock_in_id: Uuid) -> std::result::Result<SourceStockIn, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, \
                    payable_id \
               FROM biz_stock_in WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询入库单"))?;
    let Some(row) = rows.first() else {
        return Err(reference_missing(format!(
            "stock_in_id 入库单不存在或已删除（{stock_in_id}）"
        )));
    };
    Ok(SourceStockIn {
        id: row
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读入库单 id"))?,
        in_no: row
            .get_typed("in_no")
            .map_err(|error| utils::db_error(error, "读入库单号"))?,
        in_type: row
            .get_typed("in_type")
            .map_err(|error| utils::db_error(error, "读入库类型"))?,
        source_order_id: row
            .get_typed("source_order_id")
            .map_err(|error| utils::db_error(error, "读来源采购单"))?,
        supplier_id: row
            .get_typed("supplier_id")
            .map_err(|error| utils::db_error(error, "读供应商"))?,
        warehouse_id: row
            .get_typed("warehouse_id")
            .map_err(|error| utils::db_error(error, "读仓库"))?,
        in_date: row
            .get_typed("in_date")
            .map_err(|error| utils::db_error(error, "读入库日期"))?,
        status: row
            .get_typed("status")
            .map_err(|error| utils::db_error(error, "读入库状态"))?,
        payable_id: row
            .get_typed("payable_id")
            .map_err(|error| utils::db_error(error, "读入库应付单"))?,
    })
}

/// 建单核心逻辑（handler 与单测共用，只跳过认证）。
pub async fn create_return(req: &CreateReturnReq, operator: Option<Uuid>) -> std::result::Result<Uuid, ApiError> {
    let stock_in_id = dto::parse_uuid_field(Some(&req.stock_in_id), "stock_in_id")?;
    let stock_in = load_stock_in(stock_in_id).await?;
    // 只有「已过账的采购入库」能退：草稿没收货、作废的两类都没有可退数量
    if stock_in.status != "POSTED" {
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "入库单 {} 当前是「{}」（{}），只有已过账（POSTED）的入库单可以退货",
                stock_in.in_no, stock_in.status, stock_in.status
            ),
        ));
    }
    if stock_in.in_type != "PURCHASE" {
        return Err(bad_request(format!(
            "入库单 {} 的类型是 {}，只有采购入库（PURCHASE）可以走采购退货",
            stock_in.in_no, stock_in.in_type
        )));
    }
    let Some(supplier_id) = stock_in.supplier_id else {
        return Err(bad_request(format!(
            "入库单 {} 没有供应商，无法生成应付冲减与凭证",
            stock_in.in_no
        )));
    };
    let reason = dto::check_reason(req.reason.as_deref())?;
    let remark = dto::clean_remark(req.remark.as_deref())?;
    let return_date = req.return_date.unwrap_or_else(dto::today_cst);
    if req.items.is_empty() {
        return Err(bad_request("items 不能为空：至少退 1 行"));
    }
    if req.items.len() > dto::MAX_LINES {
        return Err(bad_request(format!(
            "items 最多 {} 行（收到 {}）",
            dto::MAX_LINES,
            req.items.len()
        )));
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    let outcome = async {
        // 锁读来源明细（防并发两张退货单把同一行退超）
        let sources = model::lock_source_items(&tx, stock_in.id)
            .await
            .map_err(|error| write_error(error, "锁读入库明细"))?;
        let source_by_id: HashMap<Uuid, model::SourceItem> = sources.into_iter().map(|item| (item.id, item)).collect();

        let mut seen: HashMap<Uuid, ()> = HashMap::new();
        let mut new_items: Vec<NewItem> = Vec::with_capacity(req.items.len());
        let mut amount_lines: Vec<(Decimal, Decimal, Decimal)> = Vec::with_capacity(req.items.len());
        let mut total_qty = Decimal::ZERO;
        for (index, line) in req.items.iter().enumerate() {
            let stock_in_item_id = dto::parse_uuid_field(
                Some(&line.stock_in_item_id),
                &format!("items[{index}].stock_in_item_id"),
            )?;
            if seen.insert(stock_in_item_id, ()).is_some() {
                return Err(bad_request(format!(
                    "items[{index}] 的 stock_in_item_id 重复：同一入库明细只能出现一次\
                     （要分开退请合并成一行数量）"
                )));
            }
            let Some(source) = source_by_id.get(&stock_in_item_id) else {
                return Err(reference_missing(format!(
                    "items[{index}].stock_in_item_id 不属于入库单 {}（{stock_in_item_id}）",
                    stock_in.in_no
                )));
            };
            let qty = dto::check_qty(line.qty, &format!("items[{index}].qty"))?;
            // ② 价格锁死（卡面 §④ b）
            dto::check_price_locked(
                line.unit_price,
                source.price,
                source.price_incl,
                &format!("第 {} 行", source.line_no),
            )?;
            // ① 防超退（卡面 §④ a）
            let already = model::returned_qty_of(&tx, stock_in_item_id, None)
                .await
                .map_err(|error| write_error(error, "统计已退数量"))?;
            dto::check_returnable(
                qty,
                source.qty,
                already,
                &format!("入库单 {} 第 {} 行", stock_in.in_no, source.line_no),
            )?;
            let (amount, tax, with_tax) = dto::line_amounts(qty, source.price, source.price_incl, source.tax_rate);
            amount_lines.push((amount, tax, with_tax));
            total_qty += qty;
            new_items.push(NewItem {
                line_no: i32::try_from(index + 1).unwrap_or(0),
                stock_in_item_id,
                purchase_order_item_id: source.source_item_id,
                material_id: source.material_id,
                unit_id: source.unit_id,
                qty,
                price: source.price,
                price_incl: source.price_incl,
                tax_rate: source.tax_rate,
                amount,
                tax_amount: tax,
                with_tax_amount: with_tax,
                location_id: source.location_id,
                remark: dto::clean_remark(line.remark.as_deref())?,
            });
        }
        let (total_amount, total_tax, total_with_tax) = dto::totals_of(&amount_lines);
        if total_with_tax <= Decimal::ZERO {
            return Err(bad_request(format!(
                "退货金额合计为 0（价税合计 {total_with_tax}）：零金额退货不构成财务冲销。\
                 赠品 / 样品退回请走「其他出库」，不要用采购退货（否则应付与凭证无法配平）"
            )));
        }
        let head = NewReturn {
            stock_in_id: stock_in.id,
            purchase_order_id: stock_in.source_order_id,
            payable_id: stock_in.payable_id,
            supplier_id,
            warehouse_id: stock_in.warehouse_id,
            return_date,
            total_qty: total_qty.round_dp(4),
            total_amount,
            total_tax,
            total_with_tax,
            reason,
            remark,
        };
        let (id, return_no) = model::insert_return(&tx, &head, &new_items, operator, now)
            .await
            .map_err(|error| write_error(error, "创建采购退货单"))?;
        // 建单留痕（`NULL → DRAFT`）
        state_machine::log_created(
            &tx,
            state_machine::ENTITY_PURCHASE_RETURN,
            id,
            &return_no,
            STATUS_DRAFT,
            state_machine::Operator::of(operator),
            Some("采购退货建单"),
        )
        .await?;
        Ok::<Uuid, ApiError>(id)
    }
    .await;
    match outcome {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 新建采购退货单。
#[nik::permission(
    path = "/business/erp/purchase-return",
    method = "POST",
    permission = "business:erp:purchase-return:create"
)]
#[nik::post("/business/erp/purchase-return")]
pub async fn purchase_return_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateReturnReq>,
) -> ResResult<ReturnDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_return(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 分页 / 详情 / 可退明细 ====================

/// 分页查询。
#[nik::permission(
    path = "/business/erp/purchase-return/page",
    method = "GET",
    permission = "business:erp:purchase-return:list"
)]
#[nik::get("/business/erp/purchase-return/page")]
pub async fn purchase_return_page(Query(query): Query<ReturnPageQuery>) -> ResResult<PageData<Vec<dto::ReturnRowRes>>> {
    let filter = query.filter()?;
    let (rows, total) = model::page_returns(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?;
    Ok(Res::ok(PageData {
        list: rows.iter().map(model::to_row_res).collect(),
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 详情。
#[nik::permission(
    path = "/business/erp/purchase-return/{id}",
    method = "GET",
    permission = "business:erp:purchase-return:list"
)]
#[nik::get("/business/erp/purchase-return/{id}")]
pub async fn purchase_return_detail(Path(id): Path<String>) -> ResResult<ReturnDetailRes> {
    let id = dto::parse_uuid_field(Some(&id), "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情（明细的 `returned_qty` = 含本单的累计已退）。
pub async fn detail_of(id: Uuid) -> std::result::Result<ReturnDetailRes, ApiError> {
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?
        .ok_or_else(|| not_found(id))?;
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询退货明细"))?;
    let returned_map = model::returned_qty_map(db(), row.stock_in_id, None)
        .await
        .map_err(|error| utils::db_error(error, "统计已退数量"))?;
    Ok(ReturnDetailRes {
        purchase_return: model::to_row_res(&row),
        items: items
            .iter()
            .map(|item| {
                let returned = returned_map
                    .get(&item.stock_in_item_id)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                model::to_item_res(item, returned)
            })
            .collect(),
    })
}

/// 可退明细（前端「选入库单带明细」用）。
#[nik::permission(
    path = "/business/erp/purchase-return/source/{id}",
    method = "GET",
    permission = "business:erp:purchase-return:create"
)]
#[nik::get("/business/erp/purchase-return/source/{id}")]
pub async fn purchase_return_source(Path(id): Path<String>) -> ResResult<ReturnableSourceRes> {
    let stock_in_id = dto::parse_uuid_field(Some(&id), "stock_in_id")?;
    let stock_in = load_stock_in(stock_in_id).await?;
    if stock_in.status != "POSTED" {
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "入库单 {} 当前是「{}」，只有已过账（POSTED）的入库单可以退货",
                stock_in.in_no, stock_in.status
            ),
        ));
    }
    let returned_map = model::returned_qty_map(db(), stock_in_id, None)
        .await
        .map_err(|error| utils::db_error(error, "统计已退数量"))?;
    let rows = db()
        .find_with_params(
            "SELECT si.id, si.line_no, si.material_id, \
                    COALESCE(m.material_code, '') AS material_code, \
                    COALESCE(m.material_name, '') AS material_name, \
                    si.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                    si.qty, si.price, si.price_incl, si.tax_rate, si.location_id \
               FROM biz_stock_in_item si \
               LEFT JOIN biz_material m ON m.id = si.material_id \
               LEFT JOIN biz_unit u ON u.id = si.unit_id \
              WHERE si.stock_in_id = ?::uuid \
              ORDER BY si.line_no",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询入库明细"))?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let item_id: Uuid = row
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读入库明细 id"))?;
        let received: Decimal = row
            .get_typed("qty")
            .map_err(|error| utils::db_error(error, "读入库数量"))?;
        let returned = returned_map.get(&item_id).copied().unwrap_or(Decimal::ZERO);
        items.push(ReturnableItemRes {
            stock_in_item_id: item_id.to_string(),
            line_no: row
                .get_typed("line_no")
                .map_err(|error| utils::db_error(error, "读行号"))?,
            material_id: row
                .get_typed::<Uuid>("material_id")
                .map_err(|error| utils::db_error(error, "读物料"))?
                .to_string(),
            material_code: row.get_typed("material_code").ok(),
            material_name: row.get_typed("material_name").ok(),
            unit_id: row
                .get_typed::<Uuid>("unit_id")
                .map_err(|error| utils::db_error(error, "读单位"))?
                .to_string(),
            unit_name: row.get_typed("unit_name").ok(),
            received_qty: received,
            returned_qty: returned,
            returnable_qty: dto::returnable_qty(received, returned),
            price: row
                .get_typed("price")
                .map_err(|error| utils::db_error(error, "读单价"))?,
            price_incl: row
                .get_typed("price_incl")
                .map_err(|error| utils::db_error(error, "读含税单价"))?,
            tax_rate: row
                .get_typed("tax_rate")
                .map_err(|error| utils::db_error(error, "读税率"))?,
            location_id: row
                .get_typed::<Option<Uuid>>("location_id")
                .ok()
                .flatten()
                .map(|v| v.to_string()),
        });
    }
    let (supplier_name, purchase_order_no) = source_names(&stock_in).await?;
    Ok(Res::ok(ReturnableSourceRes {
        stock_in_id: stock_in.id.to_string(),
        stock_in_no: stock_in.in_no.clone(),
        in_date: stock_in.in_date,
        supplier_id: stock_in.supplier_id.map(|v| v.to_string()),
        supplier_name,
        warehouse_id: stock_in.warehouse_id.to_string(),
        purchase_order_id: stock_in.source_order_id.map(|v| v.to_string()),
        purchase_order_no,
        items,
    }))
}

/// 来源入库单的供应商名 / 采购单号（可退明细页展示用）。
async fn source_names(stock_in: &SourceStockIn) -> std::result::Result<(Option<String>, Option<String>), ApiError> {
    let supplier_name = match stock_in.supplier_id {
        None => None,
        Some(supplier_id) => db()
            .find_with_params(
                "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&supplier_id)],
            )
            .await
            .map_err(|error| utils::db_error(error, "查询供应商"))?
            .first()
            .and_then(|row| row.get_typed::<String>("supplier_name").ok()),
    };
    let purchase_order_no = match stock_in.source_order_id {
        None => None,
        Some(order_id) => db()
            .find_with_params(
                "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&order_id)],
            )
            .await
            .map_err(|error| utils::db_error(error, "查询采购订单"))?
            .first()
            .and_then(|row| row.get_typed::<String>("order_no").ok()),
    };
    Ok((supplier_name, order_no_or_empty(purchase_order_no)))
}

fn order_no_or_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

// ==================== ③ 提交审批 ====================

/// 提交审批核心逻辑。
pub async fn submit_return(
    id: Uuid,
    req: &SubmitReturnReq,
    operator: Option<Uuid>,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?
        .ok_or_else(|| not_found(id))?;
    if row.status == STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "采购退货 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.return_no, instance_no
            ),
        ));
    }
    if row.status != STATUS_DRAFT {
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "采购退货 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
                row.return_no,
                dto::status_label(&row.status),
                row.status
            ),
        ));
    }
    // ATT-06：采购退货提交审批前必须已有退货单 / 签收凭证（规则表 biz_attachment_rule）
    attach_rule::ensure_attachments(AttachDocKind::PurchaseReturn, id, AttachAt::Submit).await?;
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询退货明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "采购退货 {} 没有任何明细，不能提交审批",
            row.return_no
        )));
    }
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // 提交前重跑一次防超退（草稿期间别人可能已经退了同一行）
    for item in &items {
        let returned = model::returned_qty_of(db(), item.stock_in_item_id, Some(row.id))
            .await
            .map_err(|error| utils::db_error(error, "统计已退数量"))?;
        dto::check_returnable(
            item.qty,
            item.received_qty,
            returned,
            &format!("第 {} 行", item.line_no),
        )?;
    }
    let business_reason = dto::clean_remark(req.reason.as_deref())?;

    match purchase_return_bridge::published_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货审批流程"))?
    {
        Some(flow) => {
            approval_gate::ensure_not_skipped(purchase_return_bridge::FLOW_CODE, gate)?;
            submit_with_approval(&row, &items, &flow, operator_id, business_reason).await
        }
        None => {
            let skip_reason = approval_gate::authorize_missing_flow(
                purchase_return_bridge::BIZ_TYPE,
                purchase_return_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                return_id = %row.id,
                return_no = %row.return_no,
                %skip_reason,
                "采购退货提交：没有已发布的 prt_approve 流程，按「显式跳过审批」直接放行到 APPROVED"
            );
            let note = approval_gate::merge_reason(business_reason.as_deref(), &skip_reason);
            let affected = model::mark_approved(db(), row.id, Some(&note), Some(operator_id), Utc::now())
                .await
                .map_err(|error| write_error(error, "采购退货进入已批准（跳过审批）"))?;
            if affected == 0 {
                return Err(conflict(
                    error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                    format!(
                        "采购退货 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
                        row.return_no
                    ),
                ));
            }
            Ok(())
        }
    }
}

/// 走审批的提交：状态推进 + 发起实例**同一事务**。
async fn submit_with_approval(
    row: &model::ReturnRow,
    items: &[model::ItemRow],
    flow: &purchase_return_bridge::ReturnFlow,
    operator_id: Uuid,
    business_reason: Option<String>,
) -> std::result::Result<(), ApiError> {
    if flow_model::biz_instance_running(purchase_return_bridge::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "采购退货 {} 已有在跑的审批单（{}）：请先撤回再提交",
                row.return_no, instance_no
            ),
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 采购退货审批", row.return_no)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.return_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: form_data(row, items),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启提交审批事务"))?;
    let outcome = async {
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| submit_conflict(error, "发起采购退货审批单", &row.return_no))?;
        if let Some(rejected) = out.rejected.take() {
            let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
                "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
            } else {
                ""
            };
            return Err(rejected.appended(hint));
        }
        let instance = out
            .instance_id
            .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
        let affected = model::mark_submitted(&tx, row.id, instance, Some(operator_id), now)
            .await
            .map_err(|error| write_error(error, "采购退货进入已提交"))?;
        if affected == 0 {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!(
                    "采购退货 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
                    row.return_no
                ),
            ));
        }
        if let Some(reason) = business_reason.as_deref() {
            tracing::info!(
                return_no = %row.return_no,
                reason,
                "采购退货提交审批（带说明）"
            );
        }
        Ok::<engine::ActionOut, ApiError>(out)
    }
    .await;
    let mut out = match outcome {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let instance = out.instance_id.unwrap_or_default();
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交采购退货审批事务"))?;
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        return_id = %row.id,
        return_no = %row.return_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "采购退货已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有审批节点（全被跳过）→ 实例在同一事务里走完，提交后立刻跑回调
    if auto_finished {
        purchase_return_bridge::on_settled(Some(instance), "APPROVED").await;
    }
    Ok(())
}

/// 审批表单变量（条件边的作用域；金额一律写 JSON **数字**）。
fn form_data(row: &model::ReturnRow, items: &[model::ItemRow]) -> serde_json::Value {
    let lines: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "line_no": item.line_no,
                "material_id": item.material_id.to_string(),
                "material_code": item.material_code,
                "material_name": item.material_name,
                "unit_name": item.unit_name,
                "qty": item.qty.to_string(),
                "price": item.price.to_string(),
                "price_incl": item.price_incl.to_string(),
                "tax_rate": item.tax_rate.to_string(),
                "amount": item.amount.to_string(),
                "with_tax_amount": item.with_tax_amount.to_string(),
            })
        })
        .collect();
    serde_json::json!({
        "return_no": row.return_no,
        "return_date": row.return_date.to_string(),
        "stock_in_no": row.stock_in_no,
        "supplier_name": row.supplier_name.clone().unwrap_or_default(),
        "warehouse_name": row.warehouse_name.clone().unwrap_or_default(),
        "reason": row.reason,
        // 条件边读这三个键（**数字**，不是字符串）
        "total_qty": json_number(&row.total_qty.round_dp(4).to_string()),
        "total_amount": json_number(&row.total_amount.round_dp(4).to_string()),
        "total_with_tax": json_number(&row.total_with_tax.round_dp(4).to_string()),
        "items": lines,
    })
}

/// 十进制字符串 → JSON **数字**。
fn json_number(decimal_text: &str) -> serde_json::Value {
    decimal_text
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

/// 在跑实例的单号（409 报文用）。
async fn pending_instance_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("未知审批单".to_string());
    };
    let rows = db()
        .find_with_params(
            "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询审批实例"))?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("instance_no").ok())
        .unwrap_or_else(|| instance_id.to_string()))
}

/// 提交审批（HTTP 入口）。
#[nik::permission(
    path = "/business/erp/purchase-return/{id}/submit",
    method = "POST",
    permission = "business:erp:purchase-return:submit"
)]
#[nik::post("/business/erp/purchase-return/{id}/submit")]
pub async fn purchase_return_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SubmitReturnReq>>,
) -> ResResult<ReturnDetailRes> {
    let id = dto::parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let gate = ApprovalGate::new(&req.skip, &claims);
    let operator = utils::current_user_uuid(&claims);
    submit_return(id, &req, operator, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 确认退货 ====================

/// 确认退货核心逻辑（**一个事务**）。
pub async fn confirm_return(
    id: Uuid,
    req: &ConfirmReturnReq,
    operator: Option<Uuid>,
) -> std::result::Result<ReturnDetailRes, ApiError> {
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?
        .ok_or_else(|| not_found(id))?;
    match row.status.as_str() {
        STATUS_APPROVED => {}
        STATUS_CONFIRMED => {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!(
                    "采购退货 {} 已确认（出库单 {} / 凭证 {}）：不能再确认第二次",
                    row.return_no,
                    row.stock_out_no.clone().unwrap_or_default(),
                    row.voucher_no.clone().unwrap_or_default()
                ),
            ));
        }
        other => {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!(
                    "采购退货 {} 当前是「{}」（{}），只有已批准（APPROVED）的单据可以确认退货",
                    row.return_no,
                    dto::status_label(other),
                    other
                ),
            ));
        }
    }
    // 审批前置：有实例就必须是 APPROVED（跳过审批的单据没有实例）
    if let Some(instance_id) = row.approval_instance_id
        && let Some(instance) = flow_model::find_instance(instance_id)
            .await
            .map_err(|error| utils::db_error(error, "查询审批实例"))?
        && instance.status != "APPROVED"
    {
        return Err(conflict(
            error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
            format!(
                "采购退货 {} 的审批单 {} 状态是 {}，未通过审批不能确认退货",
                row.return_no, instance.instance_no, instance.status
            ),
        ));
    }
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询退货明细"))?;
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "确认退货需要登录用户（令牌里没有用户 id）",
        ));
    };
    tracing::debug!(%operator_id, return_no = %row.return_no, "确认采购退货：操作人已确认");
    let note = dto::clean_remark(req.reason.as_deref())?;
    let now = Utc::now();
    let op = state_machine::Operator::of(operator);

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启确认退货事务"))?;
    let outcome = async {
        // 锁单头（防并发两次确认）
        let Some(current) = model::lock_return(&tx, id)
            .await
            .map_err(|error| write_error(error, "锁定退货单"))?
        else {
            return Err(not_found(id));
        };
        if current != STATUS_APPROVED {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!("采购退货 {} 的状态已变为 {current}，请刷新后重试", row.return_no),
            ));
        }
        // 出库 + 三本账 + 应付 + 应退款 + 凭证
        let posted = purchase_return_posting::confirm_tx(&tx, &row, &items, operator, now).await?;
        // 状态机留痕（APPROVED → CONFIRMED）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_PURCHASE_RETURN,
            id,
            &row.return_no,
            &state_machine::PURCHASE_RETURN_EDGES,
            STATUS_APPROVED,
            dto::STATUS_CONFIRMED,
            op.clone(),
            note.as_deref(),
            Some(("STOCK_OUT", &posted.stock_out_no)),
        )
        .await?;
        let affected = model::mark_confirmed(&tx, id, purchase_return_posting::links_of(&posted), operator, now)
            .await
            .map_err(|error| write_error(error, "采购退货确认落库"))?;
        if affected == 0 {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!("采购退货 {} 的状态在确认期间已被改动，请刷新后重试", row.return_no),
            ));
        }
        // 联动（事务内关键动作）；凭证 / 库存 / 应付余额由上面专用路径落账，必须排除
        let ctx = linkage_ctx(&row, operator);
        let run = linkage::engine::run_excluding(&tx, &ctx, linkage_spec::PURCHASE_RETURN_EXCLUDED_ACTIONS).await?;
        tracing::info!(
            return_no = %row.return_no,
            stock_out_no = %posted.stock_out_no,
            voucher_no = %posted.voucher_no,
            cost_total = %posted.cost_total,
            payable_after = ?posted.payable_amount_after,
            supplier_receivable = ?posted.supplier_receivable_no,
            linkage_runs = run.runs.len(),
            linkage_deferred = run.deferred.len(),
            "采购退货确认：账务与状态已同事务落库"
        );
        Ok::<linkage::LinkageRun, ApiError>(run)
    }
    .await;
    match outcome {
        Ok(run) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交确认退货事务"))?;
            // 提交后动作（通知 / 建任务）：失败只记日志，永不回滚业务
            if !run.deferred.is_empty() {
                let ctx = linkage_ctx(&row, operator);
                for item in linkage::engine::run_deferred(&ctx, run.deferred).await {
                    tracing::info!(
                        return_no = %row.return_no,
                        rule_id = %item.rule_id,
                        action_code = %item.action_code,
                        outcome = item.outcome.label(),
                        reason = item.outcome.detail(),
                        "采购退货确认：提交后的联动动作执行结果"
                    );
                }
            }
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 联动上下文（`PURCHASE_RETURN/CONFIRMED`）。
fn linkage_ctx<'a>(row: &'a model::ReturnRow, operator: Option<Uuid>) -> linkage::LinkageContext<'a> {
    // 默认收件人 = 制单人（「我经办的单子退货确认了」）：退货单没有负责人字段，
    // 制单人是唯一稳定的人；没有制单人（外部导入 / 历史行）退操作人；
    // 都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
    let recipient = row.create_by.or(operator);
    let label = linkage::spec::status_label_of(state_machine::ENTITY_PURCHASE_RETURN, dto::STATUS_CONFIRMED)
        .unwrap_or_else(|| dto::status_label(dto::STATUS_CONFIRMED));
    let mut draft = notice::NoticeDraft::text(
        NoticeType::Workflow,
        NoticeLevel::Info,
        format!("采购退货{label}"),
        format!(
            "采购退货「{}」已{}：供应商 {}，退货数量 {}，价税合计 {}，仓库 {}；\
             库存回冲、应付冲减与冲销凭证已同事务落账",
            row.return_no,
            label,
            row.supplier_name.as_deref().unwrap_or("-"),
            row.total_qty,
            row.total_with_tax,
            row.warehouse_name.as_deref().unwrap_or("-"),
        ),
        notice::NoticeTarget::User(recipient.into_iter().collect()),
    );
    // 退货单没有独立详情路由（列表页用抽屉展示），只能深链到列表；单号在正文里，够定位。
    draft.link_url = Some("/business/erp/purchase-return".to_string());
    draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
    draft.biz_id = Some(row.id.to_string());
    draft.sender_name = Some("采购退货".to_string());
    // 知情类通知（账已落定，漏看不造成损失）→ 尊重 notify.inbox 偏好。
    draft.force = false;
    // 幂等键 = 单据 + 目标态：CONFIRMED 是**终态**（确认后不可再确认），一张单一条通知。
    // `sys_notice.idempotency_key` 是 VARCHAR(128)（V0043），本键 66 字符（单测钉住）。
    draft.idempotency_key = Some(format!("LNK:PURCHASE_RETURN:{}:{}", dto::STATUS_CONFIRMED, row.id));
    linkage::LinkageContext::new(
        state_machine::ENTITY_PURCHASE_RETURN,
        dto::STATUS_CONFIRMED,
        row.id,
        &row.return_no,
        operator,
    )
    .with_people(operator, row.create_by, None)
    .with_notice(draft)
}

/// 确认退货（HTTP 入口）。
#[nik::permission(
    path = "/business/erp/purchase-return/{id}/confirm",
    method = "POST",
    permission = "business:erp:purchase-return:confirm"
)]
#[nik::post("/business/erp/purchase-return/{id}/confirm")]
pub async fn purchase_return_confirm(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<ConfirmReturnReq>>,
) -> ResResult<ReturnDetailRes> {
    let id = dto::parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(confirm_return(id, &req, operator).await?))
}

// ==================== ⑤ 作废 ====================

/// 作废核心逻辑。
///
/// * `DRAFT`：直接作废（不要求原因，从未占过账务）；
/// * `APPROVED`：作废**必须给原因**（财务已按它准备冲账）；
/// * `SUBMITTED`：**不允许**（审批中请先撤回，撤回后回草稿再作废 —— 否则审批单会
///   挂在一张已作废的单据上）；
/// * `CONFIRMED`：终态（账已动，撤销必须走红冲链，不是一条状态边）。
pub async fn cancel_return(
    id: Uuid,
    req: &CancelReturnReq,
    operator: Option<Uuid>,
) -> std::result::Result<ReturnDetailRes, ApiError> {
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?
        .ok_or_else(|| not_found(id))?;
    let reason = req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty());
    match row.status.as_str() {
        STATUS_DRAFT | STATUS_APPROVED => {}
        STATUS_SUBMITTED => {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!(
                    "采购退货 {} 正在审批中：请先到审批中心撤回（撤回后回到草稿），再作废",
                    row.return_no
                ),
            ));
        }
        other => {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!(
                    "采购退货 {} 当前是「{}」（{}），不能作废{}",
                    row.return_no,
                    dto::status_label(other),
                    other,
                    if other == dto::STATUS_CONFIRMED {
                        "（账已动：撤销要走红冲链，不是作废）"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
    if state_machine::Entity::PurchaseReturn.needs_reason(&row.status, dto::STATUS_CANCELLED) && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ 已作废必须给出原因（reason）：财务已按这张单据准备冲账，\
             对账时要能回答「为什么不退了」",
            dto::status_label(&row.status),
            row.status
        )));
    }
    let now = Utc::now();
    let op = state_machine::Operator::of(operator);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_PURCHASE_RETURN,
            id,
            &row.return_no,
            &state_machine::PURCHASE_RETURN_EDGES,
            &row.status,
            dto::STATUS_CANCELLED,
            op.clone(),
            reason,
            None,
        )
        .await?;
        let affected = model::mark_cancelled(&tx, id, &row.status, reason.unwrap_or("草稿作废"), operator, now)
            .await
            .map_err(|error| write_error(error, "采购退货作废"))?;
        if affected == 0 {
            return Err(conflict(
                error_code::ERP_PURCHASE_RETURN_STATUS_GUARD,
                format!("采购退货 {} 的状态在作废期间已被改动，请刷新后重试", row.return_no),
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废事务"))?;
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 作废（HTTP 入口）。
#[nik::permission(
    path = "/business/erp/purchase-return/{id}/cancel",
    method = "POST",
    permission = "business:erp:purchase-return:confirm"
)]
#[nik::post("/business/erp/purchase-return/{id}/cancel")]
pub async fn purchase_return_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<CancelReturnReq>>,
) -> ResResult<ReturnDetailRes> {
    let id = dto::parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(cancel_return(id, &req, operator).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;
    use std::str::FromStr;

    /// 测试用定点数（仓库不引 `rust_decimal_macros`，用 `Decimal::from_str`）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试字面量必须是合法小数")
    }

    /// 一张「已批准、待确认」的退货单（只为联动上下文测试造形，不落库）。
    fn sample_row(create_by: Option<Uuid>) -> model::ReturnRow {
        model::ReturnRow {
            id: Uuid::now_v7(),
            return_no: "PRT-20990115-000001".to_string(),
            stock_in_id: Uuid::now_v7(),
            stock_in_no: "SI-20990115-000001".to_string(),
            purchase_order_id: None,
            purchase_order_no: None,
            supplier_id: Uuid::now_v7(),
            supplier_name: Some("示范供应商".to_string()),
            warehouse_id: Uuid::now_v7(),
            warehouse_name: Some("主仓".to_string()),
            return_date: dto::today_cst(),
            status: STATUS_APPROVED.to_string(),
            total_qty: dec("2"),
            total_amount: dec("100.00"),
            total_tax: dec("13.00"),
            total_with_tax: dec("113.00"),
            reason: "质量不合格".to_string(),
            approval_instance_id: None,
            stock_out_id: None,
            stock_out_no: None,
            payable_id: None,
            payable_no: None,
            supplier_receivable_id: None,
            supplier_receivable_no: None,
            voucher_id: None,
            voucher_no: None,
            remark: None,
            create_by,
            created_at: Utc::now(),
        }
    }

    /// `PURCHASE_RETURN/CONFIRMED` 的上下文必须带**可用的**通知草稿。
    ///
    /// 为什么钉住这条：`actions::prepare_notice` 在 `ctx.notice` 为空时直接
    /// `ActionError::Skipped`（不报错、不发送）。少了它，V0351 预置的
    /// 「退货确认 → 通知制单人」规则会**每次都跳过**，界面上什么都不会发生 ——
    /// 这正是 V0323 记录过的「看起来配好了，实际不工作」。
    #[test]
    fn confirm_ctx_carries_notice_draft() {
        let creator = Uuid::now_v7();
        let row = sample_row(Some(creator));
        let operator = Uuid::now_v7();
        let ctx = linkage_ctx(&row, Some(operator));

        assert_eq!(ctx.entity, state_machine::ENTITY_PURCHASE_RETURN);
        assert_eq!(ctx.status, dto::STATUS_CONFIRMED);
        assert_eq!(ctx.doc_id, row.id, "触发单据主键必须是退货单 id");
        assert_eq!(ctx.doc_no, row.return_no);
        assert_eq!(ctx.operator, Some(operator));
        assert_eq!(ctx.creator, Some(creator), "creator 必须是制单人");

        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert_eq!(draft.biz_id.as_deref(), Some(row.id.to_string().as_str()));
        assert_eq!(draft.link_url.as_deref(), Some("/business/erp/purchase-return"));
        assert!(
            draft.title.contains("已确认"),
            "标题必须用人读的中文状态名：{}",
            draft.title
        );
        assert!(
            draft.content.contains(&row.return_no) && draft.content.contains("示范供应商"),
            "正文必须带单号与供应商：{}",
            draft.content
        );
        assert!(
            !draft.force,
            "退货确认是知情类通知（账已落定），默认尊重 notify.inbox 偏好"
        );
        match &draft.target {
            notice::NoticeTarget::User(users) => {
                assert_eq!(users, &vec![creator], "默认收件人 = 制单人");
            }
            other => panic!("默认收件人必须是 User(制单人)，实际 {other:?}"),
        }
        let key = draft.idempotency_key.as_deref().expect("必须带幂等键（重放不重复发）");
        assert!(
            key.contains(dto::STATUS_CONFIRMED) && key.contains(&row.id.to_string()),
            "幂等键必须由「目标态 + 单据 id」组成：{key}"
        );
        assert!(
            key.len() <= 128,
            "幂等键超长（{}/128）：sys_notice.idempotency_key 是 VARCHAR(128)（V0043）",
            key.len()
        );
    }

    /// 没有制单人（外部导入 / 历史行）时退操作人：不能因为缺人就把通知丢掉。
    #[test]
    fn confirm_ctx_falls_back_to_operator_as_recipient() {
        let row = sample_row(None);
        let operator = Uuid::now_v7();
        let draft = linkage_ctx(&row, Some(operator)).notice.expect("必须带通知草稿");
        match &draft.target {
            notice::NoticeTarget::User(users) => assert_eq!(users, &vec![operator]),
            other => panic!("收件人必须是 User(操作人)，实际 {other:?}"),
        }
    }
}
