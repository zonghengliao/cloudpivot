//! 采购订单接口层（批次 D · P2-A2）：建 / 改 / 状态流转 / 删（列表与详情沿用 W1c）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-order/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/purchase-order` | `business:erp:purchase-order:create` | 建单（`DRAFT` + `PO-yyyyMMdd-000001`，金额服务端算） |
//! | PUT | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:update` | 改单（**仅 `DRAFT`**，明细致删旧插新） |
//! | PATCH | `/business/erp/purchase-order/{id}/status` | `business:erp:purchase-order:status` | 状态流转（4 条合法边，其余 409） |
//! | DELETE | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:delete` | 软删（**仅 `DRAFT`**） |
//! | GET | `/business/erp/purchase-order/page` / `{id}` | `business:erp:purchase-order:list` | **沿用 W1c 的只读接口**（P2-A2 只补 `price_mode` 返回） |
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库」
//! 收进 [`create_purchase_order`] / [`update_purchase_order`] / [`change_purchase_order_status`] /
//! [`delete_purchase_order`] 之后，单测走**同一条代码路径**（只跳过认证）。
//!
//! # 金额只由服务端算（硬红线 2）
//!
//! 请求契约里没有金额字段（见 [`crate::business::erp::purchase_dto`]），
//! 单头 `total_*` 与行 `amount / tax_amount / with_tax_amount` 全部由
//! [`crate::business::erp::purchase_model::line_amounts`] 现算并逐行舍入后相加。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 采购订单是 ERP 单据表（V0031 建表，无 dept_id / 归属人列），
//! 可见性由 business:erp:purchase-order:* 权限码控制；分页沿用 model::purchase_order_page
//! （内部走 scope::paginate_unscoped，闸①口径）。
//! ```
//!
//! # 不收货（硬红线 7）
//!
//! 本卡**不写** `biz_stock_in` / `biz_inventory` / `biz_inventory_txn`，
//! 已收数量（`received_qty`）是派生值（P2-3：只算已过账收货单，建单时恒 0），
//! `PARTIAL_RECEIVED` / `RECEIVED` 两个状态也不由本卡写入（P2-B1）。

use axum::Json;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate;
use crate::business::erp::budget_guard;
use crate::business::erp::contract_model;
use crate::business::erp::dto::PurchaseOrderRes;
use crate::business::erp::form_snapshot;
use crate::business::erp::model::PurchaseOrderRow;
use crate::business::erp::purchase_bridge;
use crate::business::erp::purchase_credit;
use crate::business::erp::purchase_dto::{
    CreatePurchaseOrderReq, PURCHASE_TYPE_CONTRACT, PendingItemRes, PendingOrderRes,
    PendingReceiveRes, PendingStockInRes, PreparedOrder, PurchaseOrderDetailRes,
    PurchaseOrderLineRes, PurchaseOrderStatusReq, ensure_submittable_lines,
};
use crate::business::erp::purchase_model::{
    self, NewItem, NewPurchaseOrder, STATUS_CANCELLED, STATUS_DRAFT, STATUS_SUBMITTED, Totals,
    can_transition, status_label, transition_rejected_message,
};
use crate::business::erp::stockin_model;
use crate::business::erp::unit_guard;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 共用辅助 ====================

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 操作人（`create_by` / `update_by`）。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 不存在统一 404。
fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 状态冲突统一 409。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 单据名词（409 文案里用）。
const PO_LABEL: &str = "采购订单";

/// 提交审批撞 `uk_biz_flow_instance_biz_active` → `409 + 本单号`（不是「单号重复、去查 biz_seq」）。
///
/// 判据与文案收在 [`flow_model::is_biz_active_duplicate`] / [`flow_model::approval_in_progress`]，
/// 与调拨单等兄弟模块同一份实现。只在**拿得到单号的入口**（发起采购审批）用它；
/// 其余出口走 [`write_error`] 的兜底分支（文案退化成「该采购订单」）。
fn submit_conflict(err: DbError, ctx: &str, order_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(
            context = ctx,
            order_no,
            error = %err,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(PO_LABEL, Some(order_no));
    }
    write_error(err, ctx)
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    // ⚠️ 顺序要紧：审批实例的唯一索引冲突**先**认出来。
    // 落到下面的 `UniqueViolation` 分支会被说成「采购订单号重复（唯一约束
    // uk_biz_flow_instance_biz_active）…联系运维排查 biz_seq」—— 用户按提示去查取号器，
    // 方向完全错（真正原因是这张单已经有一个在跑的审批单）。
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(context = ctx, error = %err, "撞审批实例唯一索引：已有在跑实例，翻 409");
        return flow_model::approval_in_progress(PO_LABEL, None);
    }
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "采购订单号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（三金额必须精确恒等 total_with_tax = total_amount + total_tax；\
                 数量 > 0；税率 0–100）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::ForeignKeyViolation { constraint, .. } => ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "引用的数据不存在（外键 {}）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        other => utils::db_error(other, ctx),
    }
}

// ==================== A3：合同解析 / 额度现算 ====================

/// 解析后的合同（**采购单供应商的唯一取值点**，设计 §6.2-①）。
#[derive(Debug, Clone)]
struct OrderContract {
    /// 合同主键（落 `contract_id`，弱引用）。
    id: Uuid,
    /// 合同号（落 `contract_no_snapshot`）。
    contract_no: String,
    /// 乙方 = 供应商（落 `supplier_id`）。
    party_b_id: Uuid,
    /// 乙方名称（落 `supplier_name_snapshot`；主档现值优先，主档软删则回落到签约快照）。
    supplier_name: String,
}

/// 单头解析结果：供应商 / 合同 / 名称快照三件套。
struct ResolvedHead {
    /// 实际落库的供应商（`CONTRACT` = 合同乙方；`SPOT` = 用户选的）。
    supplier_id: Uuid,
    /// 合同（`SPOT` 为 `None`）。
    contract: Option<OrderContract>,
    /// 供应商名称快照。
    supplier_name_snapshot: Option<String>,
}

/// 校验并解析合同（V-22 五步：存在 / 未软删 → 采购类 → 状态可用 → 下单日在有效期内）。
///
/// 状态口径跟 `contract_model::USABLE_FOR_ORDER`（**`SIGNED` / `ACTIVE`**）走，
/// 不是卡面写的「只认 ACTIVE」：
/// - ARCH-2 §5.1 V-22 与设计 §6.2/§7.3 都是「已签署（SIGNED）/ 生效（ACTIVE）」；
/// - A2 的详情接口已按同一常量下发 `usable_for_order`，两处若不一致，
///   前端会看到「可用 = true」却下不了单。
async fn resolve_contract(
    raw: &str,
    order_date: NaiveDate,
) -> std::result::Result<OrderContract, ApiError> {
    let id = parse_uuid(raw, "contract_id")?;
    let contract = contract_model::contract_find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询合同"))?
        .ok_or_else(|| reference_missing(format!("contract_id 合同不存在或已删除（{id}）")))?;
    if contract.contract_type != "PURCHASE" {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 的类型是 {}，不能用于采购下单（只接受 PURCHASE）",
                contract.contract_no, contract.contract_type
            ),
        ));
    }
    if !contract_model::USABLE_FOR_ORDER.contains(&contract.status.as_str()) {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 当前是「{}」（{}），只有已签署（SIGNED）/ 生效（ACTIVE）的合同可以下单",
                contract.contract_no,
                contract_model::status_label(&contract.status),
                contract.status
            ),
        ));
    }
    if let Some(start) = contract.start_date
        && order_date < start
    {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "下单日期 {order_date} 早于合同 {} 的生效日 {start}",
                contract.contract_no
            ),
        ));
    }
    if let Some(end) = contract.end_date
        && order_date > end
    {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "下单日期 {order_date} 已超出合同 {} 的到期日 {end}（请续签或改用新合同）",
                contract.contract_no
            ),
        ));
    }
    Ok(OrderContract {
        id: contract.id,
        contract_no: contract.contract_no,
        party_b_id: contract.party_b_id,
        supplier_name: contract
            .party_b_current_name
            .unwrap_or(contract.party_b_name),
    })
}

/// 把 DTO 的四种组合（类型 × 合同 × 供应商）解析成可落库的单头。
///
/// - `CONTRACT`：合同必须是有效采购合同；供应商**从合同乙方派生**，客户端传了就必须相等
///   （相等视为幂等，不等 → `400 + 66025`，不静默覆盖）；
/// - `SPOT`：供应商必须存在且未软删（名称顺手作为快照），合同必须为空（DTO 已拦）。
async fn resolve_head(prepared: &PreparedOrder) -> std::result::Result<ResolvedHead, ApiError> {
    if prepared.purchase_type == PURCHASE_TYPE_CONTRACT {
        let raw = prepared.contract_id_raw.as_deref().ok_or_else(|| {
            ApiError::new(
                400,
                error_code::ERP_PO_CONTRACT_REQUIRED,
                "purchase_type = CONTRACT 必须给 contract_id",
            )
        })?;
        let contract = resolve_contract(raw, prepared.order_date).await?;
        if let Some(given_raw) = prepared.supplier_id_raw.as_deref() {
            let given = parse_uuid(given_raw, "supplier_id")?;
            if given != contract.party_b_id {
                return Err(ApiError::new(
                    400,
                    error_code::ERP_PO_SUPPLIER_MISMATCH,
                    format!(
                        "采购单的供应商从合同乙方带出：合同 {} 的乙方是 {}，本单填的是 {}。\
                         请留空自动带出，或改选属于该乙方的合同",
                        contract.contract_no, contract.party_b_id, given
                    ),
                ));
            }
        }
        Ok(ResolvedHead {
            supplier_id: contract.party_b_id,
            supplier_name_snapshot: Some(contract.supplier_name.clone()),
            contract: Some(contract),
        })
    } else {
        let raw = prepared.supplier_id_raw.as_deref().ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "supplier_id 必填（SPOT 零星采购必须手选供应商）",
            )
        })?;
        let supplier_id = parse_uuid(raw, "supplier_id")?;
        let name = purchase_model::supplier_ref(supplier_id)
            .await
            .map_err(|error| utils::db_error(error, "查询供应商"))?
            .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
        Ok(ResolvedHead {
            supplier_id,
            contract: None,
            supplier_name_snapshot: Some(name),
        })
    }
}

/// 合同额度守卫（V-23）：**现算**「其它未取消采购单 + 本单」是否超出合同金额。
///
/// 为什么必须在事务里先 `FOR UPDATE` 锁合同：两个并发下单各自都看到「还剩额度」，
/// 加起来就超了（ARCH-2 V-23 的并发坑）。锁顺序 = 合同行 → 采购单写入，
/// 与 A2 的改合同流程同序，不会互相死锁。
///
/// 不做「已下单额」缓存列（V-21/R1：缓存必漂移），每次 `SUM(total_with_tax)` 现算。
async fn guard_contract_amount(
    ex: &impl SqlExecutor,
    contract: &OrderContract,
    this_total: Decimal,
    exclude: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let locked = contract_model::contract_lock_full(ex, contract.id)
        .await
        .map_err(|error| utils::db_error(error, "锁定合同"))?;
    let Some(locked) = locked else {
        return Err(reference_missing(format!(
            "合同 {} 在本次下单期间已被删除",
            contract.contract_no
        )));
    };
    // 锁内再校一次状态：从读到写之间合同可能被终止
    if !contract_model::USABLE_FOR_ORDER.contains(&locked.status.as_str()) {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 当前是「{}」（{}），只有已签署（SIGNED）/ 生效（ACTIVE）的合同可以下单",
                locked.contract_no,
                contract_model::status_label(&locked.status),
                locked.status
            ),
        ));
    }
    let used = purchase_model::committed_amount(ex, locked.id, exclude)
        .await
        .map_err(|error| utils::db_error(error, "统计合同已下单额"))?;
    let after = used + this_total;
    if after > locked.amount {
        return Err(ApiError::new(
            409,
            error_code::ERP_CONTRACT_AMOUNT_EXCEEDED,
            format!(
                "合同 {} 额度不足：合同金额 {}，已下单 {}（不含作废单），本单 {}，\
                 下单后累计 {} 将超出合同额 {}",
                locked.contract_no,
                locked.amount,
                used,
                this_total,
                after,
                locked.amount - used
            ),
        ));
    }
    Ok(())
}

/// 列表 / 详情的供应商名称兜底：**快照优先**，存量行（快照 `NULL`）用主档现值补。
///
/// 为什么不 JOIN：列表必须走快照列（R3-a / AP-6：合同换乙方后历史单显示「当时是谁」）；
/// 存量行没有快照是历史事实（V0198 不回填），显示上补一个现值即可，且不进筛选条件。
pub async fn fill_supplier_names(rows: &mut [PurchaseOrderRes]) {
    let missing: Vec<Uuid> = rows
        .iter()
        .filter(|row| row.supplier_name.is_none())
        .filter_map(|row| Uuid::parse_str(&row.supplier_id).ok())
        .collect();
    if missing.is_empty() {
        return;
    }
    let map = purchase_model::supplier_names_of(&missing)
        .await
        .unwrap_or_default();
    for row in rows.iter_mut() {
        if row.supplier_name.is_some() {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&row.supplier_id)
            && let Some(name) = map.get(&id)
        {
            row.supplier_name = Some(name.clone());
        }
    }
}

/// 列表 / 详情的**项目名称**回填（P2-8）：按 `project_id` 现查，不是快照。
///
/// 为什么项目用关联查、供应商用快照（口径 P1）：
/// - 供应商：合同可以换乙方、主数据会改名，历史单必须显示「当时是谁」（审计事实）；
/// - 项目：成本归集看的是「同一个项目」，项目改名后旧单必须跟着显示新名，
///   否则「按项目汇总」会因为改名裂成两个项目（与 P2-7 的 S2 同一条理由）。
///
/// 查不到名字（项目已软删 / 主档不存在）时 `project_name` 保持 `None`，
/// 前端显示「（已删除项目）」，**不回落 uuid**。
pub async fn fill_project_names(rows: &mut [PurchaseOrderRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter(|row| row.project_id.is_some() && row.project_name.is_none())
        .filter_map(|row| row.project_id.as_deref())
        .filter_map(|raw| Uuid::parse_str(raw).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let map = purchase_model::project_names_of(&ids)
        .await
        .unwrap_or_default();
    for row in rows.iter_mut() {
        if row.project_name.is_some() {
            continue;
        }
        if let Some(id) = row.project_id.as_deref()
            && let Ok(parsed) = Uuid::parse_str(id)
            && let Some(name) = map.get(&parsed)
        {
            row.project_name = Some(name.clone());
        }
    }
}

/// 列表口径：**快照优先**，老单（快照为 `NULL`）回落关联查（P2-9 / DC-23）。
///
/// # 为什么列表要「读快照」而不是「读现值」
///
/// 验收口径（卡面）：仓库改名后，**老采购单列表仍显示旧名**（那批货当时进的就是
/// 那个仓），点进详情才看到「现在的名字」。若列表也读现值，用户会以为单据被改过。
///
/// # 存量单的过渡期方案（本卡不做的回填）
///
/// `warehouse_name_snapshot` 是 V0238 才加的列，P2-9 之前的老单全是 `NULL`。
/// 这里对「`warehouse_name` 为空」的行按 `warehouse_id` **批量**现查一次补上
/// （`purchase_model::warehouse_names_of`），于是老单在列表里也有名字可显示，
/// 且**不写库**（不回填 = 不伪造历史：那些单下单时仓库叫什么，谁也不知道）。
/// 新单有快照 ⇒ 不进这条查询（0 次额外往返）。
///
/// 仓库已被软删的老单：查不到名字 ⇒ 保持 `None`，前端显示「（仓库已删除）」，
/// **不回落 uuid**（与 `project_name` / `supplier_name` 同口径）。
pub async fn fill_warehouse_names(rows: &mut [PurchaseOrderRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter(|row| row.warehouse_name.is_none())
        .filter_map(|row| Uuid::parse_str(&row.warehouse_id).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let map = purchase_model::warehouse_names_of(&ids)
        .await
        .unwrap_or_default();
    for row in rows.iter_mut() {
        if row.warehouse_name.is_some() {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&row.warehouse_id)
            && let Some(name) = map.get(&id)
        {
            row.warehouse_name = Some(name.clone());
        }
    }
}

/// 详情口径：**现名优先**（关联查），查不到才回落到快照（P2-9 / DC-23）。
///
/// # 与列表相反，为什么
///
/// 详情页回答的是「这张单的货要进/已进哪个仓，**现在**是哪个仓」—— 用户要点进去
/// 才能知道仓库是否改名 / 是否停用，所以详情必须显示现值；快照仍然下发
/// （`warehouse_name_snapshot`），前端可以并列显示「现名（原：旧名）」，
/// 与 P2-7 供应商名的 S2 口径一致。
///
/// # 查不到现值（仓库已软删）
///
/// 回落 `warehouse_name_snapshot`（**不清空**）：详情比列表更不能一片空白 ——
/// 「当时叫什么」是这张单唯一的仓库线索。
pub async fn fill_warehouse_name_current(rows: &mut [PurchaseOrderRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.warehouse_id).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let map = purchase_model::warehouse_names_of(&ids)
        .await
        .unwrap_or_default();
    for row in rows.iter_mut() {
        let live = Uuid::parse_str(&row.warehouse_id)
            .ok()
            .and_then(|id| map.get(&id).cloned());
        row.warehouse_name = live.or_else(|| row.warehouse_name_snapshot.clone());
    }
}

// ==================== 详情组装 ====================
/// 详情：单头（含三金额 + 口径）+ 明细（行号升序）。
pub async fn detail_of(id: Uuid) -> std::result::Result<PurchaseOrderDetailRes, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let items = purchase_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
    let mut order = PurchaseOrderRes::from(&row);
    fill_approval_no(std::slice::from_mut(&mut order)).await;
    fill_supplier_names(std::slice::from_mut(&mut order)).await;
    fill_project_names(std::slice::from_mut(&mut order)).await;
    // P2-9：详情口径 = 仓库现名（查不到回落快照）
    fill_warehouse_name_current(std::slice::from_mut(&mut order)).await;
    Ok(PurchaseOrderDetailRes {
        order,
        items: items.iter().map(PurchaseOrderLineRes::from).collect(),
    })
}

/// 回填审批单号（`approval_instance_no`）：一次查完整页的实例号，逐行按 id 对上。
///
/// 为什么要它：列表 / 详情要显示「审批中（FLW-xxx）」（O5 的可执行报文口径），
/// 而 `PurchaseOrderRes::from(&Row)` 只拿得到实例 **id**。批量查一次而不是逐行查 ——
/// 一页 20 行逐行查就是 20 次往返。
pub async fn fill_approval_no(rows: &mut [PurchaseOrderRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| row.approval_instance_id.as_deref())
        .filter_map(|raw| Uuid::parse_str(raw).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let found = db()
        .find_with_params(
            "SELECT id, instance_no FROM biz_flow_instance \
              WHERE id = ANY(?::text::uuid[])",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await
        .unwrap_or_default();
    let map: std::collections::HashMap<String, String> = found
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.get_typed("id").ok()?;
            let no: String = row.get_typed("instance_no").ok()?;
            Some((id.to_string(), no))
        })
        .collect();
    for row in rows.iter_mut() {
        if let Some(id) = row.approval_instance_id.as_deref()
            && let Some(no) = map.get(id)
        {
            row.approval_instance_no = Some(no.clone());
        }
    }
}

// ==================== ① 建单 ====================

/// 建单核心逻辑（handler 与单测共用）。
///
/// 顺序：DTO 校验 + 算金额（纯函数）→ 供应商 / 仓库存在 → 逐行物料存在（带出 `unit_id`）→
/// 一个事务里「取号 + 单头 + 明细」。
pub async fn create_purchase_order(
    req: &CreatePurchaseOrderReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let prepared = req.prepare()?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;

    // 收货仓库：存在性 + 名称快照一次拿到（P2-9 / DC-23：多查一列，不多一次往返）
    let warehouse_name = purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // 供应商 / 合同 / 名称快照一次解析（CONTRACT 的供应商从这里派生，**不接受手填覆盖**）
    // 所属项目（P2-8）：在事务外先确认存在性，错文案能点名 project_id
    let project_id = resolve_project(&prepared).await?;
    let head = resolve_head(&prepared).await?;

    let items = prepare_items(&prepared).await?;
    let totals = Totals::of(&items);
    let new = NewPurchaseOrder {
        supplier_id: head.supplier_id,
        warehouse_id,
        purchase_type: &prepared.purchase_type,
        contract_id: head.contract.as_ref().map(|contract| contract.id),
        contract_no_snapshot: head
            .contract
            .as_ref()
            .map(|contract| contract.contract_no.as_str()),
        supplier_name_snapshot: head.supplier_name_snapshot.as_deref(),
        warehouse_name_snapshot: Some(warehouse_name.as_str()),
        project_id,
        order_date: prepared.order_date,
        expect_date: prepared.expect_date,
        price_mode: &prepared.price_mode,
        remark: prepared.remark.as_deref(),
        credit_days: prepared.credit_days,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    // 合同额度守卫在**事务内**：先锁合同行再算累计额（V-23 的并发坑）
    if let Some(contract) = head.contract.as_ref()
        && let Err(error) = guard_contract_amount(&tx, contract, totals.with_tax, None).await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    match purchase_model::create(&tx, &new, &items, &totals, op, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建采购订单"))
        }
    }
}

/// 所属项目（P2-8）：`None` = 不归集项目；给了就必须「存在且未软删」。
///
/// 走 select 而不是外键：`biz_project` 是软删表，外键拦不住已软删（见
/// `purchase_model::project_ref` 的注释）。**不校验项目状态**：归档项目上补录当期采购
/// 是合法业务，拒绝只会让用户把成本挂到别的项目上。
async fn resolve_project(
    prepared: &crate::business::erp::purchase_dto::PreparedOrder,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let Some(raw) = prepared.project_id_raw.as_deref() else {
        return Ok(None);
    };
    let project_id = parse_uuid(raw, "project_id")?;
    purchase_model::project_ref(project_id)
        .await
        .map_err(|error| utils::db_error(error, "查询所属项目"))?
        .ok_or_else(|| reference_missing("project_id 项目不存在或已删除"))?;
    Ok(Some(project_id))
}

/// 明细：物料存在性 + 基础单位（`unit_id` 列 `NOT NULL`，由物料带出，不让客户端传）。
async fn prepare_items(
    prepared: &crate::business::erp::purchase_dto::PreparedOrder,
) -> std::result::Result<Vec<NewItem>, ApiError> {
    let mut items = Vec::with_capacity(prepared.lines.len());
    for (index, line) in prepared.lines.iter().enumerate() {
        let material_id = parse_uuid(
            &line.material_id_raw,
            &format!("items[{index}].material_id"),
        )?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        // ★ DC-09 单位止血（方案 B）：明细的 `unit_id` 由物料带出，物料挂折算单位（如「箱」）
        //   时本单会把「5 箱」记成「5 个」——库存三本账只用 `qty`，`rate` 无人消费。
        unit_guard::ensure_base_unit(material_id, index, line.qty).await?;
        items.push(NewItem {
            material_id,
            unit_id: material.unit_id,
            qty: line.qty,
            price: line.price_excl,
            price_incl: line.price_incl,
            tax_rate: line.tax_rate,
            amounts: line.amounts,
            remark: line.remark.clone(),
        });
    }
    Ok(items)
}

/// 新建采购订单（`status = DRAFT`，单号 `PO-yyyyMMdd-000001`）。
#[nik::permission(
    path = "/business/erp/purchase-order",
    method = "POST",
    permission = "business:erp:purchase-order:create"
)]
#[nik::post("/business/erp/purchase-order")]
pub async fn purchase_order_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreatePurchaseOrderReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = create_purchase_order(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑥ 待收明细（D4：一键生成收货单的数据源） ====================

/// 「查采购单待收明细」的核心逻辑（handler 与单测共用）。
///
/// 口径（卡面 F2 + 硬红线 4 / 5）：
///
/// - `received_qty` **只算已过账（`POSTED`）的收货单**，草稿 / 作废 / 软删一律不算
///   （聚合表达式见 [`crate::business::erp::purchase_model::POSTED_RECEIVED_SQL`]）；
/// - 按**采购明细行**（`si.source_item_id`）匹配，不按 `material_id`；
/// - `pending_qty = ordered_qty - received_qty`（负数按 0）；
/// - `can_receive` 同时要求「状态可收」与「还有待收行」：`RECEIVED` / `CLOSED` /
///   `CANCELLED` / `DRAFT` 都给 `false` + 明确原因，前端不用自己拼文案。
pub async fn pending_items_of(id: Uuid) -> std::result::Result<PendingReceiveRes, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let lines = purchase_model::pending_receive_lines(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单待收明细"))?;
    let briefs = purchase_model::stock_ins_of_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单已生成的收货单"))?;

    // 名称查不到（供应商 / 仓库被软删）一律给空串：前端是把它回填进下拉框的，
    // 给 null 会让「带出默认值」这一步静默失败。
    let supplier_name = purchase_model::supplier_ref(row.supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .unwrap_or_default();
    let warehouse_name = purchase_model::warehouse_ref(row.warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .unwrap_or_default();

    let pending_line_count = lines.iter().filter(|line| line.has_pending()).count();
    let receivable = stockin_model::purchase_receivable(&row.status);
    let can_receive = receivable && pending_line_count > 0;
    let blocked_reason = if can_receive {
        None
    } else if pending_line_count > 0 {
        // 状态不可收（DRAFT / RECEIVED / CLOSED / CANCELLED）且还有待收行：原因在状态上
        Some(format!(
            "采购单 {} 当前是「{}」（{}），不能收货：只有已提交（SUBMITTED）/ 已审批（APPROVED）/\n部分收货（PARTIAL_RECEIVED）的采购单可以收",
            row.order_no,
            status_label(&row.status),
            row.status
        ))
    } else {
        // 一行都不剩：原因在数量上（文案要带「已收满」+ 当前状态，两个问题都答）
        Some(format!(
            "采购单 {} 已收满：{} 行全部收完，没有可收的待收数量（当前状态「{}」/ {}）",
            row.order_no,
            lines.len(),
            status_label(&row.status),
            row.status
        ))
    };

    Ok(PendingReceiveRes {
        order: PendingOrderRes {
            id: row.id.to_string(),
            order_no: row.order_no.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            supplier_id: row.supplier_id.to_string(),
            supplier_name,
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_name,
            total_qty: row.total_qty,
            order_date: row.order_date,
            remark: row.remark.clone(),
        },
        items: lines.iter().map(PendingItemRes::from_line).collect(),
        stock_ins: briefs.iter().map(PendingStockInRes::from_brief).collect(),
        pending_line_count,
        can_receive,
        blocked_reason,
    })
}

/// 查采购单待收明细（收货页「一键带出」的数据源）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/pending-items",
    method = "GET",
    permission = "business:erp:purchase-order:list"
)]
#[nik::get("/business/erp/purchase-order/{id}/pending-items")]
pub async fn purchase_order_pending_items(Path(id): Path<String>) -> ResResult<PendingReceiveRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(pending_items_of(id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
pub async fn update_purchase_order(
    id: Uuid,
    req: &CreatePurchaseOrderReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    // ⚠️ 这里**不能**先跑 `req.prepare()`：省缺 `purchase_type` 的 CONTRACT 单会被当成
    // SPOT，从而被「SPOT 不能带 contract_id」误拒。必须是「先取当前类型 → 归一化 → 再校验」。
    let current = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    if current.status != STATUS_DRAFT {
        // P10 铁律 ④：入账态（收过货）→ 统一的「已入账」文案
        state_machine::ensure_writable(
            state_machine::ENTITY_PURCHASE_ORDER,
            &current.status,
            &current.order_no,
        )?;
        return Err(conflict(format!(
            "采购订单当前是「{}」（{}），只有草稿（DRAFT）能编辑：\
             已提交的单据请先取消，或由收货流程处理",
            status_label(&current.status),
            current.status
        )));
    }

    // ---------- A3：采购类型不可改（改类型会把合同关联洗掉） ----------
    // 请求是整单覆盖，`purchase_type` 省缺视为「沿用当前类型」；
    // 显式给了且与库里不一致 → 400。真要换类型必须先取消合同关联（本批次不提供）。
    if req
        .purchase_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_uppercase)
        .is_some_and(|requested| requested != current.purchase_type)
    {
        return Err(ApiError::new(
            400,
            error_code::ERP_PO_TYPE_LOCKED,
            format!(
                "采购单 {} 的采购类型是「{}」，建单后不允许改成「{}」：\
                 改类型会洗掉合同关联（合同采购 → 零星采购），历史就查不到这张单挂过哪张合同了。\
                 确需变更请作废本单后重开",
                current.order_no,
                current.purchase_type,
                req.purchase_type.as_deref().unwrap_or_default().trim()
            ),
        ));
    }
    // 省缺 `purchase_type` → 用当前类型（否则 CONTRACT 单改备注会被默认成 SPOT）
    let mut req = req.clone();
    if req
        .purchase_type
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        req.purchase_type = Some(current.purchase_type.clone());
    }
    let prepared = req.prepare()?;

    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;
    let warehouse_name = purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    // 所属项目（P2-8）：在事务外先确认存在性，错文案能点名 project_id
    let project_id = resolve_project(&prepared).await?;
    let head = resolve_head(&prepared).await?;

    let items = prepare_items(&prepared).await?;
    let totals = Totals::of(&items);
    let new = NewPurchaseOrder {
        supplier_id: head.supplier_id,
        warehouse_id,
        purchase_type: &prepared.purchase_type,
        contract_id: head.contract.as_ref().map(|contract| contract.id),
        contract_no_snapshot: head
            .contract
            .as_ref()
            .map(|contract| contract.contract_no.as_str()),
        supplier_name_snapshot: head.supplier_name_snapshot.as_deref(),
        warehouse_name_snapshot: Some(warehouse_name.as_str()),
        project_id,
        order_date: prepared.order_date,
        expect_date: prepared.expect_date,
        price_mode: &prepared.price_mode,
        remark: prepared.remark.as_deref(),
        credit_days: prepared.credit_days,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    // 合同额度守卫：排除本单（`exclude = Some(id)`），否则自己会被算两遍
    if let Some(contract) = head.contract.as_ref()
        && let Err(error) = guard_contract_amount(&tx, contract, totals.with_tax, Some(id)).await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    match purchase_model::update(&tx, id, &new, &items, &totals, operator, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            // 读状态与写状态之间被别人推走（并发）：按冲突处理，不静默成功
            Err(conflict(
                "采购订单状态在本次编辑期间已被改动（只有草稿能改），请刷新后重试",
            ))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交改单事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "编辑采购订单"))
        }
    }
}

/// 编辑采购订单（仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}",
    method = "PUT",
    permission = "business:erp:purchase-order:update"
)]
#[nik::put("/business/erp/purchase-order/{id}")]
pub async fn purchase_order_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreatePurchaseOrderReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    update_purchase_order(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 状态流转 ====================

/// 状态流转核心逻辑（handler 与单测共用）。
///
/// 顺序：读单 → 目标态白名单 → 状态机（非法边 409）→ 提交前的明细数量守卫 →
/// CAS 写状态 → 回读。
pub async fn change_purchase_order_status(
    id: Uuid,
    req: &PurchaseOrderStatusReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let target = req.target_status()?;
    let reason = req.reason_text();
    if let Some(text) = reason
        && text.chars().count() > crate::business::erp::purchase_dto::MAX_REASON_LEN
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "reason 超过 {} 字（当前 {} 字）",
                crate::business::erp::purchase_dto::MAX_REASON_LEN,
                text.chars().count()
            ),
        ));
    }

    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    if !can_transition(&row.status, target) {
        return Err(conflict(transition_rejected_message(&row.status, target)));
    }

    // ★ R-P1-5：收过货的单据**不许从这个入口直接作废**。
    //
    // 边表为了联动作废（`erp::cancel_cascade`）补了 `PARTIAL_RECEIVED/RECEIVED → CANCELLED`，
    // 但那条边的前提是「下游收货单已经全部作废、三本账已回基线」。从 `PATCH /status`
    // 直接推过去就绕过了影响面确认，留下的是一堆孤儿收货单 / 凭证 / 应付单 ——
    // 正是这张卡要消灭的场景。所以这里显式拦下，把用户导向联动作废入口。
    if target == STATUS_CANCELLED
        && matches!(
            row.status.as_str(),
            purchase_model::STATUS_PARTIAL_RECEIVED | purchase_model::STATUS_RECEIVED
        )
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "采购订单 {} 已经收过货（{}），不能直接作废：请点「作废」先看影响面\
                 （GET /business/erp/purchase-order/{}/cancel-impact），再一键按顺序处理\
                 （POST …/cancel-cascade）—— 下游收货单 / 凭证 / 应付单会被一起回滚",
                row.order_no,
                purchase_model::status_label(&row.status),
                row.id
            ),
        ));
    }

    // SUBMITTED → CANCELLED 必须给出原因（卡面 §三）；DRAFT → CANCELLED 原因可选
    if target == STATUS_CANCELLED && row.status == STATUS_SUBMITTED && reason.is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "已提交的采购订单作废必须给出原因（reason）：提交后可能已有收货安排，\
             原因要能追溯到为什么不用了",
        ));
    }

    // 提交前的明细守卫（PRD §3.1.6：「提交/过账时拦截 0 数量」）
    if target == STATUS_SUBMITTED {
        let lines = purchase_model::items_of(id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
        ensure_submittable_lines(&lines)?;
    }

    let now = Utc::now();
    // LNK-02：联动 outbox 与事务同生共死 —— 事务内的动作随 `tx` 回滚，
    // 通知这类「提交后动作」只在 `commit()` 成功之后 flush。
    let mut linkage_outbox = purchase_model::LinkageOutbox::default();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    // P10 铁律 ③：先过唯一入口（边表校验 + reason 校验 + 写 biz_status_log），再 CAS
    // P-FLOW-3A 反向兜底：审批中作废 → **同一事务先终止在跑的审批单**（P11：不留孤儿待办，
    // 否则 `uk_biz_flow_instance_biz_active` 会让这张单以后再也发不起审批）
    if row.status == STATUS_SUBMITTED {
        purchase_bridge::cancel_approval_of(&tx, id, &row.order_no, "采购单已作废，审批自动终止")
            .await
            .map_err(|error| write_error(error, "作废：终止在跑的审批"))?;
    }
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_PURCHASE_ORDER,
        id,
        &row.order_no,
        &purchase_model::LEGAL_EDGES,
        &row.status,
        target,
        op.clone(),
        reason,
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    match purchase_model::apply_status(
        &tx,
        id,
        &row.status,
        target,
        reason,
        op.id,
        now,
        &mut linkage_outbox,
    )
    .await
    {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "采购订单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.status
            )))
        }
        Ok(_) => {
            // ★ FIN-1（ARCH-6 §3.1.3）：预算占用的生命周期跟着单据状态走 —— 与状态流转**同一事务**。
            //   · 作废 / 取消 → 释放（`OCCUPIED`/`CONSUMED` → `RELEASED`，**不删行**，留痕）
            //   · 关闭（订单终结）→ 已执行（`OCCUPIED` → `CONSUMED`）
            if target == STATUS_CANCELLED {
                if let Err(error) = budget_guard::release(
                    &tx,
                    budget_guard::SOURCE_PURCHASE,
                    id,
                    &format!("采购单作废：{}", reason.unwrap_or("未填写原因")),
                )
                .await
                {
                    let _ = tx.rollback().await;
                    return Err(write_error(error, "释放预算占用"));
                }
            } else if target == purchase_model::STATUS_CLOSED
                && let Err(error) =
                    budget_guard::consume(&tx, budget_guard::SOURCE_PURCHASE, id, None).await
            {
                let _ = tx.rollback().await;
                return Err(write_error(error, "预算占用转已执行"));
            }
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            // LNK-02：提交之后才发通知 / 建任务（Phase::AfterCommit 动作）。
            linkage_outbox.flush().await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "采购订单状态流转"))
        }
    }
}

/// 状态流转（`DRAFT→SUBMITTED` / `DRAFT→CANCELLED` / `SUBMITTED→CLOSED` / `SUBMITTED→CANCELLED`）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/status",
    method = "PATCH",
    permission = "business:erp:purchase-order:status"
)]
#[nik::patch("/business/erp/purchase-order/{id}/status")]
pub async fn purchase_order_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PurchaseOrderStatusReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    // P-FLOW-3A：`SUBMITTED` 这个目标态走**审批入口**（有流程就先送审），
    // 其余（CLOSED / CANCELLED）仍是普通状态流转。
    if req.target_status()? == STATUS_SUBMITTED {
        let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
        submit_approval(id, &claims, &gate).await?;
    } else {
        change_purchase_order_status(id, &req, &claims).await?;
    }
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③b 提交审批（P-FLOW-3A） ====================

/// 提交采购订单审批的**唯一入口**（P-FLOW-3A + ARCH-3 DC-01）：配了已发布的
/// `po_approve` 流程就先送审（`DRAFT → SUBMITTED` + 审批单）；
/// **没配流程 → 409 + `APPROVAL_FLOW_NOT_CONFIGURED`（默认拒绝）**，
/// 只有持有 `business:erp:skip-approval` 并写明理由（≥10 字）的账号才能显式跳过，
/// 理由会作为本次状态流转的 `reason` 落进 `biz_status_log`。
///
/// 为什么入口收在这里：`PATCH /status`（老前端）与 `POST /submit-approval`（本卡新按钮）
/// **必须同一条路** —— 否则审批可以被换一个 URL 绕过。
///
/// # P2-2：供应商信用额度在这里生效（策略 B）
///
/// 提交前**现算**「该供应商累计未结采购额（含本单）」（不缓存，见
/// [`purchase_credit`] 的口径说明），结果冻结进单据三列 + 审批单 `form_data`：
///
/// * 没超 → 照常提交（顺带记下当时的累计，便于以后对账）；
/// * 超了 → **必须走审批**：有 `po_approve` 就走（标记已落单据，审批流可按
///   `form.over_credit_limit` 配条件分支）；没有已发布流程时**直接 409 +
///   `ERP_CREDIT_LIMIT_EXCEEDED`** —— `skip-approval` 这条后门对超限单**不开放**，
///   否则「信用额度」等于没生效。
pub async fn submit_approval(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    // 已提交（审批中）→ 409 且带审批单号（O5：报文要能直接照着去找）
    if row.status == STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "采购单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.order_no, instance_no
        )));
    }
    // P2-2：信用额度现算（**不缓存**）。审计事实落单据，当前值每次重算。
    let credit = purchase_credit::verdict_of(db(), &row)
        .await
        .map_err(|error| utils::db_error(error, "统计供应商累计未结采购额"))?;
    if credit.over {
        tracing::warn!(
            order_id = %row.id,
            order_no = %row.order_no,
            supplier_id = %row.supplier_id,
            limit = %credit.limit,
            exposure = %credit.exposure,
            "采购单提交：超出供应商信用额度（标记落单据 + 强制审批）"
        );
    }
    // ★★ FIN-1（ARCH-6 §3.5.2）：预算守卫 —— 与信用额度**并列**、互不影响。
    //    只读判定（不改库存 / 不写 usage）：科目靠 `biz_budget_subject_map` 解析，
    //    部门取提交人部门快照，期间取单据日期。usage 在 `submit_po_tx` 的事务里写。
    let operator_dept = match op.id {
        Some(uid) => crate::business::erp::voucher::user_dept_of(db(), uid)
            .await
            .unwrap_or(None),
        None => None,
    };
    let budget = budget_guard::check_for_purchase(db(), &row, operator_dept)
        .await
        .map_err(|error| utils::db_error(error, "预算校验（采购提交）"))?;
    if budget.over() {
        tracing::warn!(
            order_id = %row.id,
            order_no = %row.order_no,
            mode = %budget.mode.as_str(),
            reasons = %budget.reason,
            "采购单提交：超出部门预算（策略 B：标记 + 强制审批）"
        );
    }
    // BLOCK 模式下直接 409（ARCH-6 D3：409 只在显式 BLOCK）
    if budget.blocks() {
        return Err(ApiError::new(
            409,
            error_code::ERP_BUDGET_EXCEEDED,
            format!(
                "采购单 {} 超出部门预算，且该槽的控制策略是 BLOCK（强管控），不能提交。{}\n\
                 两条出路：① 在「预算管理」里对「部门 × 科目 × 期间」做一次预算调整（走审批，新增版本）；\
                 ② 让管理员把该槽（或全局 sys_config `erp.budget.control_mode`）改成 WARN —— \
                 那样超预算单会带着「超预算」标记进审批，由审批人复核。",
                row.order_no, budget.reason
            ),
        ));
    }
    match purchase_bridge::published_po_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(purchase_bridge::FLOW_CODE, gate)?;
            submit_po_approval(&row, &op, &flow, &credit, &budget).await
        }
        None => {
            // ★ P2-2 ①：超信用额度的单**不许**走「显式跳过审批」这条后门。
            //   额度是客户认真填的内控参数，超限又没有流程承接时，唯一正确的动作是把人
            //   指到「配流程」或「调额度」，而不是让一个持 skip-approval 的账号点一下放行。
            if credit.over {
                let supplier_name = purchase_model::supplier_ref(row.supplier_id)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| row.supplier_id.to_string());
                return Err(ApiError::new(
                    409,
                    error_code::ERP_CREDIT_LIMIT_EXCEEDED,
                    format!(
                        "采购单 {} 超出供应商「{}」的信用额度，必须走审批，但系统里没有已发布的 \
                         po_approve 流程，所以不能提交。{}\n\
                         两条出路：① 请管理员在「审批流程」里发布采购订单审批流程（po_approve），\
                         超限单会带着「超信用额度」标记进审批；② 或者把该供应商的信用额度调到 \
                         {} 及以上（供应商主数据「信用额度」，0 = 不限）。",
                        row.order_no,
                        supplier_name,
                        credit.describe(),
                        credit.exposure
                    ),
                ));
            }
            // ARCH-3 DC-01：**不再静默降级**。默认 409（点名 po_approve）；
            // 只有「持 business:erp:skip-approval + 理由 ≥10 字」才放行，
            // 理由随状态流转写进 `biz_status_log`（审计据此追溯为什么这次没有复核）。
            let skip_reason = approval_gate::authorize_missing_flow(
                purchase_bridge::BIZ_TYPE,
                purchase_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                order_id = %row.id,
                order_no = %row.order_no,
                %skip_reason,
                "采购单提交：没有已发布的 po_approve 流程，按「显式跳过审批」放行（理由已写 biz_status_log）"
            );
            let req = PurchaseOrderStatusReq {
                status: Some(STATUS_SUBMITTED.to_string()),
                reason: Some(skip_reason),
                ..Default::default()
            };
            change_purchase_order_status(id, &req, op.clone()).await?;
            // 跳过审批路径的信用评估结果**事后回写**（本路径自己没有事务可以挂）：
            // `change_purchase_order_status` 已经提交了状态流转，这里补一条不带事务的
            // UPDATE。为什么可以这样：这三列是**审计快照**，不参与任何状态机判定 ——
            // 写失败只影响「这张单当时的评估结果能不能看到」，不能反过来把提交撤销。
            if let Err(error) = purchase_credit::write_verdict(db(), id, &credit).await {
                tracing::warn!(
                    order_id = %id,
                    order_no = %row.order_no,
                    %error,
                    "采购单已提交（跳过审批），但信用评估快照回写失败（只影响展示，不影响单据）"
                );
            }
            // ★ FIN-1：跳过审批路径的预算占用**事后回写**（本路径没有事务可挂）。
            //    与信用快照同口径：写失败只影响「这张单占没占预算」的可见性，
            //    不能反过来把已经提交的单据撤销；失败留 WARN 并由巡检对账兜底。
            match budget_guard::occupy_for_purchase(db(), &row, &budget, op.id).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        order_id = %id,
                        order_no = %row.order_no,
                        %error,
                        "采购单已提交（跳过审批），但预算占用写入失败（额度未占，巡检会对账出来）"
                    );
                }
            }
            Ok(())
        }
    }
}

/// **发起采购订单审批**：`DRAFT → SUBMITTED`，同一事务里状态机过闸 + 发起审批 + 回写实例。
async fn submit_po_approval(
    row: &PurchaseOrderRow,
    operator: &state_machine::Operator,
    flow: &purchase_bridge::PoFlow,
    credit: &purchase_credit::CreditVerdict,
    budget: &budget_guard::PurchaseBudget,
) -> std::result::Result<(), ApiError> {
    // ① 只有草稿能提交审批
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "采购订单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
            row.order_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ② 明细 / 金额闸（与直接提交同一套守卫：0 数量、0 金额不许送审）
    let lines = purchase_model::items_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
    if lines.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("采购订单 {} 没有任何明细，不能提交审批", row.order_no),
        ));
    }
    ensure_submittable_lines(&lines)?;
    if row.total_with_tax <= Decimal::ZERO {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "采购订单 {} 的含税金额为 {}，不能提交审批（金额必须 > 0）",
                row.order_no, row.total_with_tax
            ),
        ));
    }
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 幂等：同一单已有在跑的审批 → 409
    if flow_model::biz_instance_running(purchase_bridge::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "采购单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.order_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 采购订单审批", row.order_no)
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
        biz_no: Some(row.order_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: purchase_form_data(row, credit, budget).await,
    };

    let now = Utc::now();
    // LNK-02：提交（DRAFT → SUBMITTED）也要触发联动 —— 事务内动作随 `tx` 回滚，
    // 通知 / 建任务这类提交后动作在 commit 之后 flush。
    let mut linkage_outbox = purchase_model::LinkageOutbox::default();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启采购审批事务"))?;
    let outcome = submit_po_tx(
        &tx,
        row,
        operator,
        &start,
        credit,
        budget,
        now,
        &mut linkage_outbox,
    )
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
        .map_err(|error| utils::db_error(error, "提交采购审批事务"))?;
    // LNK-02：提交之后才发采购单状态联动的通知（与下面审批引擎的锁外副作用同一口径）
    linkage_outbox.flush().await;
    // 锁外副作用（审批通知 / 客户自配的通知节点）必须在提交之后发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        order_id = %row.id,
        order_no = %row.order_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "采购订单已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了，
    // 提交后立刻执行「审批通过」的回调，否则单据会停在 SUBMITTED 且没有待办能推进它。
    if auto_finished {
        purchase_bridge::on_po_settled(Some(instance), "APPROVED").await;
    }
    Ok(())
}

/// [`submit_po_approval`] 的事务体：返回引擎动作结果（调用方提交后负责发锁外副作用）。
#[allow(clippy::too_many_arguments)]
async fn submit_po_tx(
    tx: &Transaction,
    row: &PurchaseOrderRow,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    credit: &purchase_credit::CreditVerdict,
    budget: &budget_guard::PurchaseBudget,
    now: DateTime<Utc>,
    linkage_outbox: &mut purchase_model::LinkageOutbox,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 状态机唯一入口（边表 + 写 biz_status_log）
    state_machine::transition(
        tx,
        state_machine::ENTITY_PURCHASE_ORDER,
        row.id,
        &row.order_no,
        &purchase_model::LEGAL_EDGES,
        STATUS_DRAFT,
        STATUS_SUBMITTED,
        operator.clone(),
        Some("提交审批"),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起采购审批单", &row.order_no))?;
    if let Some(rejected) = out.rejected.take() {
        // 引擎的码与文案原样交给前端；只在「自审策略」这一类上加一句「怎么解决」。
        // `FLOW_NO_ASSIGNEE`（含 t_72e95761 的「审批人全被内控排除」）的报文自己
        // 已经写明两条出路（换审批岗位 / 换人提交），不再叠一句泛泛的提示。
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
        } else {
            ""
        };
        // FIX-1：用 `appended` 而不是 `ApiError::new(..)` 重建 —— 后者会把 65018 的
        // 结构化载荷（前端「指派审批人」引导框）连同 `internal_detail` 一起丢掉。
        return Err(rejected.appended(hint));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 提交痕迹 + 实例 id
    let affected = purchase_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_DRAFT,
        STATUS_SUBMITTED,
        None,
        operator.id,
        Some(instance_id),
        now,
        linkage_outbox,
    )
    .await
    .map_err(|error| write_error(error, "采购单进入已提交"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "采购订单 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.order_no
        )));
    }
    // ④ P2-2：信用评估结果与状态流转**同一事务**落库（审计快照必须和「已提交」同生共死）：
    //    状态提交成功而快照没写进去，就会出现「这张单到底评没评过」说不清的单据。
    purchase_credit::write_verdict(tx, row.id, credit)
        .await
        .map_err(|error| write_error(error, "写入信用额度评估快照"))?;
    // ★ FIN-1：预算占用与状态流转**同一事务**（ARCH-6 §3.5.1）：
    //    单据没提交成功却留下占用 = 额度泄漏；提交成功却没有占用 = 预算形同虚设。
    budget_guard::occupy_for_purchase(tx, row, budget, operator.id)
        .await
        .map_err(|error| write_error(error, "写入预算占用"))?;
    Ok(out)
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_instance_no_of(
    instance_id: Option<Uuid>,
) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("-".to_string());
    };
    let no = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .map(|instance| instance.instance_no)
        .unwrap_or_else(|| "-".to_string());
    Ok(no)
}

/// 采购审批的**表单变量**。金额必须写成 JSON **数字**（不是字符串）：
/// 条件边比较走 `as_f64()`，`"5000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边。
///
/// ⚠️ 返回类型写全 `serde_json::Value`：`use nik::prelude::*` 里的 `Value` 是数据库绑定值。
///
/// # P1-FIX：冻结显示名 + 明细（`supplier_name` / `warehouse_name` / `items`）
///
/// 审批岗位角色没有 `business:erp:*` 读权限（四个角色实测 0 条），前端「ID→名称」必然降级成
/// `(ID: …)`。所以把**人看的中文名**与**明细**一起冻进 `form_data`：审批人零 ERP 权限也能看懂，
/// 明细也不会因为单据后续变动而和审批时看到的不一致。键名契约见
/// [`crate::business::erp::form_snapshot`]。查询失败**不阻断提交**（降级为不写那几个键）：
/// 显示名缺了顶多退化回 `(ID: …)`，不该让审批发不出去。
///
/// # P2-2：信用额度三键（给审批流的条件分支用）
///
/// `over_credit_limit` 是 **JSON 布尔**（`eq` 比较走 `json_eq`，布尔按值比，别写成字符串 `"true"`）；
/// `credit_exposure` / `credit_limit` 是 **JSON 数字**（要能配 `gt` / `gte` 之类的阈值边）。
/// 流程管理员可以据此加分支，例如：
///
/// ```json
/// { "field": "form.over_credit_limit", "cmp": "eq", "value": true }
/// ```
///
/// → 指向「财务经理 / 总经理」节点，实现「超信用额度加一道复核」。不配也能用：
/// 标记照样落在单据上（`biz_purchase_order.over_credit_limit`），审批人看得到。
async fn purchase_form_data(
    row: &PurchaseOrderRow,
    credit: &purchase_credit::CreditVerdict,
    budget: &budget_guard::PurchaseBudget,
) -> serde_json::Value {
    let amount = row.total_with_tax.round_dp(4).to_string();
    let number = json_number(&amount);
    let mut form = serde_json::json!({
        "order_id": row.id.to_string(),
        "order_no": row.order_no,
        "amount": number,
        "total_with_tax": row.total_with_tax.to_string(),
        "total_amount": row.total_amount.to_string(),
        "total_qty": row.total_qty.to_string(),
        "supplier_id": row.supplier_id.to_string(),
        "warehouse_id": row.warehouse_id.to_string(),
        "price_mode": row.price_mode,
        // 内控留痕：审批人 ≠ 制单人（`purchase_bridge::guard_self_approval` 兜底）
        "create_by": row.create_by.map(|value| value.to_string()),
        // P2-2：供应商信用额度（现算结果的快照；布尔 + 两个数字，见函数注释）
        "over_credit_limit": credit.over,
        "credit_exposure": json_number(&credit.exposure.round_dp(4).to_string()),
        "credit_limit": json_number(&credit.limit.round_dp(4).to_string()),
        // FIN-1（ARCH-6 §3.5.2 纪律 3）：超预算**只加一个标记**，不改 po_approve 流程；
        // 客户想要「超预算加一道财务」，自己在流程设计器里加条件节点即可。
        "over_budget": budget.over(),
        "budget_mode": budget.mode.as_str(),
        "budget_over_amount": json_number(&budget
            .verdict
            .as_ref()
            .map(|verdict| verdict.over_amount().round_dp(4).to_string())
            .unwrap_or_else(|| "0".to_string())),
    });
    // 显示名 / 明细：查不到就**不写这个键**（前端按 `*_id` 兜底），绝不写空串冒充「查到了」。
    let (supplier, warehouse, items) = (
        form_snapshot::supplier_name(db(), row.supplier_id).await,
        form_snapshot::warehouse_name(db(), row.warehouse_id).await,
        form_snapshot::purchase_items(db(), row.id).await,
    );
    if let Some(object) = form.as_object_mut() {
        match supplier {
            Ok(Some(name)) => {
                object.insert("supplier_name".to_string(), serde_json::Value::String(name));
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：供应商名查询失败，降级")
            }
        }
        match warehouse {
            Ok(Some(name)) => {
                object.insert(
                    "warehouse_name".to_string(),
                    serde_json::Value::String(name),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：仓库名查询失败，降级")
            }
        }
        match items {
            Ok(items) => {
                object.insert("items".to_string(), items);
            }
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：明细查询失败，降级")
            }
        }
    }
    form
}

/// `Decimal` 文本 → JSON **数字**。
///
/// 条件求值走 `as_f64()`：写成字符串（`"5000.0000"`）会 `TypeMismatch` → 引擎按「条件不成立」
/// **静默走兜底边**（金额分级会悄悄错档）。解析失败写 `null`（JSON 合法值）而不是退化成字符串。
///
/// 抽出来是因为 `amount` / `credit_exposure` / `credit_limit` 三处都要「数字不是字符串」——
/// 各写一遍就把这条踩坑记录埋成三份，改一处漏两处。
fn json_number(decimal_text: &str) -> serde_json::Value {
    decimal_text
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

/// 提交采购订单审批（`DRAFT → SUBMITTED`；没配流程时**默认 409**，见 [`submit_approval`]）。
///
/// 请求体可省；要显式跳过审批时传 `{"skip_approval": true, "skip_reason": "<≥10 字>"}`
/// （需要权限码 `business:erp:skip-approval`）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/submit-approval",
    method = "POST",
    permission = "business:erp:purchase-order:submit-approval"
)]
#[nik::post("/business/erp/purchase-order/{id}/submit-approval")]
pub async fn purchase_order_submit_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<approval_gate::SkipApprovalReq>>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let skip = body.map(|Json(item)| item).unwrap_or_default();
    let gate = approval_gate::ApprovalGate::new(&skip, &claims);
    submit_approval(id, &claims, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

/// 删除核心逻辑：**DRAFT 可删**；**审批中（`SUBMITTED` 且确有在跑审批）也可删**
/// —— 后者会连同审批单一起终止（P11：不许留孤儿待办）。
///
/// 其余状态（已审批 / 部分收货 / 已收货 / 已关闭 / 已取消）是审批与收货的事实，只能作废。
pub async fn delete_purchase_order(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let approving = row.status == STATUS_SUBMITTED
        && flow_model::biz_instance_running(purchase_bridge::BIZ_TYPE, id)
            .await
            .map_err(|error| utils::db_error(error, "查询在跑审批单"))?;
    if row.status != STATUS_DRAFT && !approving {
        return Err(conflict(format!(
            "采购订单当前是「{}」（{}），只有草稿（DRAFT）或审批中（SUBMITTED + 在跑审批）能删：\
             已提交 / 已审批 / 已收货 / 已关闭 / 已取消的单据是审批与收货的事实，只能作废（状态流转）",
            status_label(&row.status),
            row.status
        )));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除事务"))?;
    let outcome = async {
        // 先撤在跑的审批（幂等：没有就什么都不做），再软删
        purchase_bridge::cancel_approval_of(&tx, id, &row.order_no, "采购单已删除，审批自动终止")
            .await
            .map_err(|error| write_error(error, "删除：终止在跑的审批"))?;
        let affected = purchase_model::soft_delete(&tx, id, operator, now)
            .await
            .map_err(|error| utils::db_error(error, "软删采购订单"))?;
        if affected == 0 {
            return Err(not_found("采购订单"));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交删除事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 删除采购订单（软删；仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}",
    method = "DELETE",
    permission = "business:erp:purchase-order:delete"
)]
#[nik::delete("/business/erp/purchase-order/{id}")]
pub async fn purchase_order_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_purchase_order(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== 单测 ====================
//
// 纯逻辑（金额口径 / 状态机 / 校验）在 `purchase_dto` 与 `purchase_model` 的单测里；
// 这里只放**真库用例**（建单落库 / 改单 / 状态机 CAS / 软删）。
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0145 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_p2a2'
// export ERP_DB_URL='postgres://cloudpivot:<pw>@127.0.0.1:5432/cloudpivot_p2a2'
// cargo test --lib business::erp::purchase_handler
// ```
//
// 夹具固定 UUID（`d2a2…` 段）+ `P2A2-` 前缀，每次 `setup()` 先按供应商清掉上一轮的单
// （`biz_purchase_order_item` 是 `ON DELETE CASCADE`，明细跟着走），所以重复跑不积累。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::purchase_dto::PurchaseOrderItemReq;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    /// ARCH-3 DC-01：系统路径的审批前置上下文 —— 不请求跳过、也没有权限声明。
    ///
    /// 用它调 [`submit_approval`] 等于「一个普通内部调用」：没配 `po_approve` 流程时
    /// **一律 409**（这正是新的默认行为，不再是旧的静默降级）。
    fn none_gate() -> approval_gate::ApprovalGate<'static> {
        approval_gate::ApprovalGate::none()
    }

    // ---------- ARCH-3 DC-01：审批流未配置 → 409（不再是静默降级） ----------

    /// **DC-01 ①**：没配 `po_approve` 流程、也没请求跳过 → **409 + 66033**。
    ///
    /// 改造前这里会把草稿直接推成 `SUBMITTED`（无审批单，只留一条 `warn`）；
    /// 现在必须 409，报文点名 `po_approve` 并给出两条出路（发布流程 / 授予 skip-approval），
    /// 且单据**一动不动**（仍 `DRAFT`、没有审批实例）。
    #[tokio::test]
    async fn dc01_submit_without_published_flow_is_409() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "5", "100", "13")]), None)
            .await
            .expect("建单");
        // 临时下线种子流程（模拟「客户还没配审批」）
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();
        let outcome = submit_approval(id, Some(uid(U_MAKER)), &none_gate()).await;
        // 先还原再断言：断言 panic 也不会把流程留在 DISABLED 状态里
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();
        assert!(
            purchase_bridge::published_po_flow()
                .await
                .unwrap()
                .is_some(),
            "夹具自检：流程必须已还原成 PUBLISHED"
        );

        let error = outcome.expect_err("没配流程必须 409，不能静默直接提交");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(
            error.code,
            error_code::APPROVAL_FLOW_NOT_CONFIGURED,
            "必须用 DC-01 的新码 66033：{}",
            error.msg
        );
        assert!(
            error.msg.contains("po_approve"),
            "报文必须点名缺哪个 flow_code：{}",
            error.msg
        );
        assert!(
            error.msg.contains("business:erp:skip-approval"),
            "报文要给出「显式跳过审批」这条出路：{}",
            error.msg
        );
        // 单据一动不动
        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_DRAFT, "被拒后单据必须仍是草稿");
        assert!(row.approval_instance_id.is_none(), "被拒后不许有审批实例");
    }

    // ---------- 审批实例唯一索引冲突：409 + 自家单号（不是「单号重复、去查 biz_seq」） ----------

    /// 撞 `uk_biz_flow_instance_biz_active` → **409 + 本单号 + 「已有审批中实例」**。
    ///
    /// 两条分支都钉住：驱动给了约束名（`UniqueViolation`）与退化形态（`DbError::InvalidValue`，
    /// 文案里只剩 PG 原文）—— 后者正是真库里掉到「联系运维排查 biz_seq」的那条路。
    #[test]
    fn biz_active_conflict_maps_to_409_with_order_no() {
        let variant = || DbError::UniqueViolation {
            column: None,
            constraint: Some(flow_model::BIZ_ACTIVE_INDEX.to_string()),
            source: None,
        };
        let by_text = || {
            DbError::InvalidValue(format!(
                "db error: ERROR: duplicate key value violates unique constraint \"{}\"",
                flow_model::BIZ_ACTIVE_INDEX
            ))
        };
        // 别的唯一索引不能被误判成审批冲突
        let other = DbError::UniqueViolation {
            column: None,
            constraint: Some("uk_biz_purchase_order_no".to_string()),
            source: None,
        };
        assert!(!flow_model::is_biz_active_duplicate(&other));

        for err in [variant(), by_text()] {
            let api = submit_conflict(err, "发起采购审批单", "PO-20261102-000123");
            assert_eq!(api.status, 409, "{}", api.msg);
            assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
            assert!(api.msg.contains("PO-20261102-000123"), "{}", api.msg);
            assert!(api.msg.contains("已有审批中实例"), "{}", api.msg);
            assert!(
                !api.msg.contains("biz_seq"),
                "不能把用户指到取号器：{}",
                api.msg
            );
            assert!(!api.msg.contains("单号重复"), "{}", api.msg);
        }

        // 拿不到单号的通用出口（write_error）也必须 409 + 可执行文案
        let generic = write_error(variant(), "采购订单状态流转");
        assert_eq!(generic.status, 409);
        assert_eq!(generic.code, error_code::RESOURCE_CONFLICT);
        assert!(generic.msg.contains("该采购订单"), "{}", generic.msg);
        assert!(!generic.msg.contains("biz_seq"), "{}", generic.msg);

        // 真的号段冲突不能被新分支吞掉：仍然 409 + 取号器提示
        let genuine = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_purchase_order_no".to_string()),
                source: None,
            },
            "创建采购订单",
        );
        assert_eq!(genuine.status, 409);
        assert!(genuine.msg.contains("biz_seq"), "{}", genuine.msg);
    }

    const SUPPLIER: &str = "d2a20001-0000-4000-8000-000000000001";
    const WAREHOUSE: &str = "d2a20001-0000-4000-8000-000000000002";
    const MATERIAL_A: &str = "d2a20001-0000-4000-8000-000000000011";
    const MATERIAL_B: &str = "d2a20001-0000-4000-8000-000000000012";
    /// DC-09 夹具：挂在折算单位上的物料 —— 本版本**不能**在单据里使用。
    const MATERIAL_BOX: &str = "d2a20001-0000-4000-8000-000000000013";

    /// A3 合同夹具①：ACTIVE 采购合同，乙方 = 上面的 `SUPPLIER`，额度 10 万。
    const CONTRACT_ACTIVE: &str = "d2a20001-0000-4000-8000-0000000000c1";
    /// A3 合同夹具②：DRAFT（草稿合同不能下单，内控）。
    const CONTRACT_DRAFT: &str = "d2a20001-0000-4000-8000-0000000000c2";
    /// A3 合同夹具③：ACTIVE 但额度只有 1 元（额度守卫用例）。
    const CONTRACT_TINY: &str = "d2a20001-0000-4000-8000-0000000000c3";
    /// 夹具合同号（断言快照用；唯一索引 `uk_biz_contract_no` 要求活跃行不重号）。
    const CONTRACT_ACTIVE_NO: &str = "P2A3-HT-ACTIVE";
    const CONTRACT_TINY_NO: &str = "P2A3-HT-TINY";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('d2a20001-0000-4000-8000-000000000003', 'P2A2-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT (id) DO NOTHING",
        // DC-09 夹具：折算单位「箱」（is_base = 0，1 箱 = 12 个）。单据里用它会 400。
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('d2a20001-0000-4000-8000-000000000004', 'P2A2-BOX', '箱', 0,
                 'd2a20001-0000-4000-8000-000000000003', 12.000000, 1, 2)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('d2a20001-0000-4000-8000-000000000001', 'P2A2-SUP', 'P2A2 测试供应商', 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('d2a20001-0000-4000-8000-000000000002', 'P2A2-WH', 'P2A2 测试仓',
                 '/d2a20001-0000-4000-8000-000000000002/', 1, 1, 1)
         ON CONFLICT (id) DO UPDATE SET warehouse_name = EXCLUDED.warehouse_name, deleted_at = NULL",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status)
         VALUES ('d2a20001-0000-4000-8000-000000000011', 'P2A2-M1', '螺丝 M6x20',
                 'd2a20001-0000-4000-8000-000000000003', 1),
                ('d2a20001-0000-4000-8000-000000000012', 'P2A2-M2', '垫片 8mm',
                 'd2a20001-0000-4000-8000-000000000003', 1)
         ON CONFLICT (id) DO NOTHING",
        // DC-09 夹具：物料挂在折算单位「箱」上 —— 建单必须被拒（手工插入，模拟主数据里
        // 已经配成折算单位的存量物料；主数据接口本卡不动）。
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status)
         VALUES ('d2a20001-0000-4000-8000-000000000013', 'P2A2-M3-BOX', '整箱螺丝（折算单位）',
                 'd2a20001-0000-4000-8000-000000000004', 1)
         ON CONFLICT (id) DO NOTHING",
        // A3：合同（`biz_contract` 无外键指向供应商；status 与日期必须满足
        //     ck_biz_contract_signed_dates —— SIGNED/ACTIVE/COMPLETED 三个日期都要有）
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                   party_b_id, party_b_name, amount, currency_code,
                                   sign_date, start_date, end_date, status)
         VALUES ('d2a20001-0000-4000-8000-0000000000c1', 'P2A3-HT-ACTIVE', 'P2A3 采购合同（生效）',
                 'PURCHASE', 'SUPPLIER', 'd2a20001-0000-4000-8000-000000000001',
                 'P2A2 测试供应商', 100000.0000, 'CNY',
                 DATE '2026-01-01', DATE '2026-01-01', DATE '2027-12-31', 'ACTIVE')
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                   party_b_id, party_b_name, amount, currency_code, status)
         VALUES ('d2a20001-0000-4000-8000-0000000000c2', 'P2A3-HT-DRAFT', 'P2A3 采购合同（草稿）',
                 'PURCHASE', 'SUPPLIER', 'd2a20001-0000-4000-8000-000000000001',
                 'P2A2 测试供应商', 5000.0000, 'CNY', 'DRAFT')
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                   party_b_id, party_b_name, amount, currency_code,
                                   sign_date, start_date, end_date, status)
         VALUES ('d2a20001-0000-4000-8000-0000000000c3', 'P2A3-HT-TINY', 'P2A3 采购合同（1 元额度）',
                 'PURCHASE', 'SUPPLIER', 'd2a20001-0000-4000-8000-000000000001',
                 'P2A2 测试供应商', 1.0000, 'CNY',
                 DATE '2026-01-01', DATE '2026-01-01', DATE '2027-12-31', 'ACTIVE')
         ON CONFLICT (id) DO NOTHING",
    ];

    /// 真库用例必须串行（共用全局 `db()` 与同一供应商夹具）。
    /// 本模块（含 `setup_approval`）串行跑的锁 = **共享种子角色组锁**。
    ///
    /// 本模块会写 `ROLE_PM` / `ROLE_GM` 上的 `sys_user_role` 绑定（`setup_approval` 还按角色 id
    /// 摘别人的绑定），`contract_approval_tests` 写的是同一批行 —— 两把不相识的锁时
    /// 一方会把另一方的夹具账号摘掉，待办展开不到人（实测：同一库两个进程并行时本模块一轮 5 条红）。
    use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
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
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接（W1b 实测）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 本夹具的 `PO_APPROVE` 实例定位口径（三处复用，必须逐字一致）。
    ///
    /// 三条任一成立即算本夹具的：
    /// 1. `biz_id` 指向本夹具供应商的采购单；
    /// 2. `biz_id` 为空，或指向**已经不存在的采购单** —— 上一轮 `setup()` 删单时留下的孤儿，
    ///    正是「每跑一轮全量就多 7 条 `PO_APPROVE | PENDING`」的真身（实例 `biz_id` 是弱引用，
    ///    删单据带不走它）；
    /// 3. 都没有也算（`biz_id IS NULL`），清掉没有归属的残行。
    ///
    /// 不按 `biz_no LIKE 'PO-%'` 扫：号段前缀是生产口径（技能 §6.40 的教训），
    /// 而「孤儿 + 本夹具供应商的单据」这个外键链已经足够精确。
    fn po_instance_where() -> String {
        format!(
            "biz_type = 'PO_APPROVE' AND ( \
                 biz_id IS NULL \
              OR biz_id NOT IN (SELECT id FROM biz_purchase_order) \
              OR biz_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid))"
        )
    }

    /// 清掉本夹具的审批痕迹（**删单据之前**跑）：实例 / 待办 / 抄送 / 状态日志。
    ///
    /// # 顺序为什么必须是这样
    ///
    /// 1. `biz_purchase_order.approval_instance_id` 是弱引用 → 先置 NULL（否则单据指着不存在的实例，
    ///    `instance_status_of` 查不到就按「不拦」处理，门闸静默失效）；
    /// 2. `biz_flow_instance.prev_instance_id` 是**自引用且没有 ON DELETE**（V0122）→ 先解开，
    ///    漏了这一步整批 DELETE 会被外键拦下并抛 `query failed: db error`（PG 原文里没有约束名）；
    /// 3. `biz_flow_task` / `biz_flow_cc` 虽然 ON DELETE CASCADE，仍显式先删（把顺序写死）；
    /// 4. `biz_flow_instance` 本体（**含孤儿**，见 [`po_instance_where`]）；
    /// 5. `biz_status_log` **没有外键**，必须在删单据行之前按 `entity_id` 清，否则每轮多攒几十行
    ///    永远对不上的孤儿。
    async fn purge_approval_traces() {
        let inst_where = po_instance_where();
        let statements = [
            format!(
                "UPDATE biz_purchase_order SET approval_instance_id = NULL \
                  WHERE supplier_id = '{SUPPLIER}'::uuid"
            ),
            format!(
                "UPDATE biz_flow_instance SET prev_instance_id = NULL \
                  WHERE prev_instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!(
                "DELETE FROM biz_flow_task \
                  WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!(
                "DELETE FROM biz_flow_cc \
                  WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!("DELETE FROM biz_flow_instance WHERE {inst_where}"),
            // 卡 t_8ec0a1af：这里过去还有半句
            // `OR entity_id NOT IN (SELECT id FROM biz_purchase_order)`
            // —— 那是「**全库**孤儿扫」：本模块每跑一次 `setup()` 就全表写一遍，
            // 并行时会删掉别的模块「单据行已删、自己的日志还没删」的毫秒级中间态
            // （症状：兄弟模块偶发少了状态日志行，且责任不可追）。
            // 夹具只许动**自己造的行**：按供应商 id 集合删即可；
            // 历史孤儿由各模块自己的 `zz_` 收口（`fixture_hygiene::settle_orphans`）。
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN \
                 (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
        ];
        for sql in statements {
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("清理采购审批痕迹失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 清掉本夹具的**全部**痕迹 + 单据（每个用例开头与模块级收尾共用）。
    ///
    /// 顺序不可交换：先解弱引用 / 自引用外键、删实例与状态日志，最后才删单据本体
    /// （详见 [`Self::purge_approval_traces`]）。
    async fn purge_fixture_docs() {
        purge_approval_traces().await;
        purge_projects().await;
        let sql = format!("DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid");
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理上一轮采购订单失败：{err}");
        }
        // A3：合同夹具也一起清（必须先删采购单再删合同 —— 合同是**弱引用**，
        // 库里没有外键拦着，顺序靠这里写死，免得留下「PO 指着已删合同」的残行）
        let sql = format!(
            "DELETE FROM biz_contract WHERE id IN ('{CONTRACT_ACTIVE}'::uuid, \
             '{CONTRACT_DRAFT}'::uuid, '{CONTRACT_TINY}'::uuid)"
        );
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理上一轮合同夹具失败：{err}");
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。`None` = 没配 `ERP_DB_URL`。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        // 先清审批痕迹（实例是弱引用，删单据带不走 → 不先清就会积累孤儿 PENDING），
        // 再清上一轮的订单（明细随主单 CASCADE）。
        purge_fixture_docs().await;
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        // P2-8：项目夹具（采购单 → 项目是弱引用，夹具只在本模块内可见）
        seed_projects().await;
        Some(guard)
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 组一个建单请求：`(material, qty, price, tax_rate)`。
    ///
    /// 口径由 `mode` 决定：`EXCL` 只给 `price_excl`，`INCL` 只给 `price_incl`
    /// （与前端两种录入形态一致）。
    fn req(mode: &str, lines: &[(&str, &str, &str, &str)]) -> CreatePurchaseOrderReq {
        CreatePurchaseOrderReq {
            supplier_id: Some(SUPPLIER.to_string()),
            warehouse_id: Some(WAREHOUSE.to_string()),
            price_mode: Some(mode.to_string()),
            items: lines
                .iter()
                .map(|(material, qty, price, rate)| PurchaseOrderItemReq {
                    material_id: Some((*material).to_string()),
                    qty: Some(dec(qty)),
                    price_excl: (mode == "EXCL").then(|| dec(price)),
                    price_incl: (mode == "INCL").then(|| dec(price)),
                    tax_rate: Some(dec(rate)),
                    remark: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn status_req(status: &str, reason: Option<&str>) -> PurchaseOrderStatusReq {
        PurchaseOrderStatusReq {
            status: Some(status.to_string()),
            reason: reason.map(str::to_string),
            ..Default::default()
        }
    }

    /// ① 口径 A 算例落库：100 × 2、13% → 200.00 / 26.00 / 226.00（PRD §3.1.4）。
    #[tokio::test]
    async fn excl_example_persisted_with_server_side_amounts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req("EXCL", &[(MATERIAL_A, "100", "2", "13")]);
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_DRAFT);
        assert_eq!(detail.order.price_mode, "EXCL");
        assert!(
            detail.order.order_no.starts_with("PO-"),
            "{}",
            detail.order.order_no
        );
        assert_eq!(detail.order.total_amount, dec("200.00"));
        assert_eq!(detail.order.total_tax, dec("26.00"));
        assert_eq!(detail.order.total_with_tax, dec("226.00"));
        assert_eq!(detail.items.len(), 1);
        let line = &detail.items[0];
        assert_eq!(line.amount, dec("200.00"));
        assert_eq!(line.tax_amount, dec("26.00"));
        assert_eq!(line.with_tax_amount, dec("226.00"));
        // 含税单价是派生快照：2 × 1.13 = 2.26
        assert_eq!(line.price_incl, dec("2.260000"));
        assert_eq!(line.received_qty, dec("0"));
    }

    /// ② 口径 B 算例落库：100 × 2、13% → 176.99 / 23.01 / 200.00（减法倒挤）。
    #[tokio::test]
    async fn incl_example_persisted_with_subtracted_tax() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req("INCL", &[(MATERIAL_A, "100", "2", "13")]);
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.price_mode, "INCL");
        assert_eq!(detail.order.total_with_tax, dec("200.00"));
        assert_eq!(detail.order.total_amount, dec("176.99"));
        assert_eq!(detail.order.total_tax, dec("23.01"));
        let line = &detail.items[0];
        assert_eq!(line.amount, dec("176.99"));
        assert_eq!(line.tax_amount, dec("23.01"));
        // 减法倒挤的恒等式必须精确成立（否则 ck_biz_po_item_tax 会拒写）
        assert_eq!(line.amount + line.tax_amount, line.with_tax_amount);
    }

    /// ③ 尾差：3 行各 1 × 0.005 → 逐行舍入 0.01 × 3 = 0.03（先加总再舍入会得 0.02）。
    #[tokio::test]
    async fn totals_are_rounded_per_line_not_on_sum() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req(
            "EXCL",
            &[
                (MATERIAL_A, "1", "0.005", "0"),
                (MATERIAL_A, "1", "0.005", "0"),
                (MATERIAL_A, "1", "0.005", "0"),
            ],
        );
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 3);
        for line in &detail.items {
            assert_eq!(line.amount, dec("0.01"), "逐行必须舍入到 2 位");
        }
        assert_eq!(detail.order.total_amount, dec("0.03"));
        assert_ne!(
            detail.order.total_amount,
            dec("0.02"),
            "先加总再舍入会得 0.02 —— 那是 PRD §3.1.5 明令禁止的口径"
        );
    }

    /// ④ 改单：DRAFT 能改，明细「删旧插新」，汇总跟着重算。
    #[tokio::test]
    async fn update_draft_replaces_items_and_totals() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let changed = req(
            "EXCL",
            &[(MATERIAL_A, "10", "2", "13"), (MATERIAL_B, "5", "3", "6")],
        );
        update_purchase_order(id, &changed, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 2, "旧行必须被替换掉");
        assert_eq!(detail.items[0].line_no, 1);
        assert_eq!(detail.items[1].line_no, 2);
        // 20.00 + 26.00? → 10×2=20, 税 2.60；5×3=15, 税 0.90
        assert_eq!(detail.order.total_amount, dec("35.00"));
        assert_eq!(detail.order.total_tax, dec("3.50"));
        assert_eq!(detail.order.total_with_tax, dec("38.50"));
        assert_eq!(detail.order.total_qty, dec("15"));
    }

    /// DC-09（P0 单位止血 · 采购单侧）：物料挂折算单位「箱」（1 箱 = 12 个）→ 400 + 折算建议，
    /// **一行都不落库**；改单走同一条路径（`prepare_items` 建/改共用）。
    ///
    /// 判据不只是「报错」：还要证明它没留下任何明细行 —— 否则文案对了、脏单却进了库，
    /// 「rate 配了不生效」的账照样会被结出来。
    #[tokio::test]
    async fn non_base_unit_line_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 5 箱 = 60 个
        let error = create_purchase_order(&req("EXCL", &[(MATERIAL_BOX, "5", "2", "13")]), None)
            .await
            .unwrap_err();
        assert_eq!(error.status, 400, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
        assert!(
            error.msg.contains(
                "单位「箱」不是基本单位：本版本不支持多单位换算，\
                 请按基本单位录入（1 箱 = 12 个 → 录 60）"
            ),
            "{}",
            error.msg
        );
        // 文案要点出「哪一行、哪个物料」：一单多行时用户得知道改哪一行
        assert!(error.msg.contains("items[0]"), "{}", error.msg);
        assert!(error.msg.contains("P2A2-M3-BOX"), "{}", error.msg);

        let rows = db()
            .find_with_params(
                "SELECT COUNT(*) AS cnt FROM biz_purchase_order_item WHERE material_id = ?::uuid",
                vec![Value::from(&uid(MATERIAL_BOX))],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.first().map(|row| row.get_typed::<i64>("cnt").unwrap()),
            Some(0),
            "被拒的单据不许留下明细行"
        );

        // 基本单位 + 折算单位混填：整单拒，不许「先把前一行落进去」
        let mixed = create_purchase_order(
            &req(
                "EXCL",
                &[(MATERIAL_A, "1", "2", "13"), (MATERIAL_BOX, "1", "2", "13")],
            ),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(mixed.code, error_code::ERP_UNIT_NOT_BASE, "{}", mixed.msg);
        assert!(mixed.msg.contains("items[1]"), "{}", mixed.msg);
        assert!(mixed.msg.contains("→ 录 12）"), "{}", mixed.msg);

        // 草稿单改成折算单位 → 同样 400，且原明细原样保留（改单是整单覆盖）
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "1", "2", "13")]), None)
            .await
            .unwrap();
        let edit = update_purchase_order(id, &req("EXCL", &[(MATERIAL_BOX, "1", "2", "13")]), None)
            .await
            .unwrap_err();
        assert_eq!(edit.status, 400, "{}", edit.msg);
        assert_eq!(edit.code, error_code::ERP_UNIT_NOT_BASE, "{}", edit.msg);
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 1, "改单被拒后原明细必须原样保留");
        assert_eq!(detail.items[0].material_id, MATERIAL_A);
    }

    /// ⑤ 改单：SUBMITTED 不能改 → 409（卡面 ⑬）。
    #[tokio::test]
    async fn update_submitted_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();
        let err = update_purchase_order(id, &req("EXCL", &[(MATERIAL_A, "1", "2", "13")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    }

    /// ⑥ 状态机：DRAFT → SUBMITTED ✅，写 `submitted_at` / `submitted_by`（卡面 ⑭）。
    #[tokio::test]
    async fn submit_moves_draft_to_submitted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let operator = Uuid::now_v7();
        let id = create_purchase_order(
            &req("EXCL", &[(MATERIAL_A, "100", "2", "13")]),
            Some(operator),
        )
        .await
        .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), Some(operator))
            .await
            .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_SUBMITTED);
        assert!(detail.order.submitted_at.is_some(), "提交必须写时间戳");
        assert_eq!(
            detail.order.submitted_by.as_deref(),
            Some(operator.to_string().as_str())
        );
    }

    /// ⑦ 状态机：DRAFT → CLOSED 跳级 → 409（卡面 ⑮）。
    #[tokio::test]
    async fn draft_cannot_jump_to_closed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let err = change_purchase_order_status(id, &status_req("CLOSED", None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("SUBMITTED"),
            "文案要点名可选去向：{}",
            err.msg
        );
    }

    /// ⑧ 作废：SUBMITTED → CANCELLED 必须给 reason（400），给了才成功并追加进 remark。
    #[tokio::test]
    async fn cancel_submitted_requires_reason_then_appends_remark() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();

        let err = change_purchase_order_status(id, &status_req(STATUS_CANCELLED, None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("reason"), "{}", err.msg);

        change_purchase_order_status(
            id,
            &status_req(STATUS_CANCELLED, Some("供应商断供，改走另一家")),
            None,
        )
        .await
        .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_CANCELLED);
        assert!(
            detail.order.closed_at.is_some(),
            "作废落 closed_at（表里没有 cancelled_at）"
        );
        let remark = detail.order.remark.clone().unwrap_or_default();
        assert!(remark.contains("取消原因：供应商断供"), "{remark}");
    }

    /// ⑨ 删除：DRAFT 能软删（卡面 ⑯），删完详情 404。
    #[tokio::test]
    async fn delete_draft_soft_deletes() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        delete_purchase_order(id, None).await.unwrap();
        let err = detail_of(id).await.unwrap_err();
        assert_eq!(err.status, 404, "{}", err.msg);
        // 软删：行还在库里，只是 deleted_at 非空
        let left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NOT NULL",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(left, 1);
    }

    /// ⑩ 删除：SUBMITTED 不能删 → 409（卡面 ⑰）。
    #[tokio::test]
    async fn delete_submitted_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();
        let err = delete_purchase_order(id, None).await.unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    }

    /// ⑪ 多税率单：各行独立算，税额相加正确（13% 与 6% 混排）。
    #[tokio::test]
    async fn multi_tax_rate_lines_are_independent() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req(
            "EXCL",
            &[
                (MATERIAL_A, "10", "2.5", "13"),
                (MATERIAL_B, "3", "10", "6"),
            ],
        );
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        // 行 1：25.00 + 3.25 = 28.25；行 2：30.00 + 1.80 = 31.80
        assert_eq!(detail.items[0].amount, dec("25.00"));
        assert_eq!(detail.items[0].tax_amount, dec("3.25"));
        assert_eq!(detail.items[1].amount, dec("30.00"));
        assert_eq!(detail.items[1].tax_amount, dec("1.80"));
        assert_eq!(detail.order.total_amount, dec("55.00"));
        assert_eq!(detail.order.total_tax, dec("5.05"));
        assert_eq!(detail.order.total_with_tax, dec("60.05"));
    }

    /// ⑫ 引用校验：物料不存在 → 400 + 66002（不落半张单）。
    #[tokio::test]
    async fn unknown_material_is_reference_error() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let missing = Uuid::now_v7().to_string();
        let request = req("EXCL", &[(missing.as_str(), "1", "1", "13")]);
        let err = create_purchase_order(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
    }

    // ==================== P-FLOW-3A：采购订单审批（8 条） ====================
    //
    // 这一批跑的是**真种子**（V0182 的 `po_approve`：7 节点 8 边，三个审批节点全是 ROLE），
    // 所以它同时验证「种子能发起、能派给角色、能按金额分流」——不是另造一张测试图。

    /// 角色夹具（V0182 种子里的固定 UUID）。
    const ROLE_DEPT_LEADER: &str = "20000000-0000-4000-8000-0000000f0001";
    /// 采购经理。
    const ROLE_PM: &str = "20000000-0000-4000-8000-0000000f0002";
    /// 总经理。
    const ROLE_GM: &str = "20000000-0000-4000-8000-0000000f0003";
    /// 制单人（无审批角色）。
    const U_MAKER: &str = "d2a2000a-0000-4000-8000-000000000001";
    /// 部门主管（DEPT_LEADER 成员）。
    const U_DEPT: &str = "d2a2000a-0000-4000-8000-000000000002";
    /// 采购经理（PURCHASE_MANAGER 成员）。
    const U_PM: &str = "d2a2000a-0000-4000-8000-000000000003";
    /// 总经理（GENERAL_MANAGER 成员）。
    const U_GM: &str = "d2a2000a-0000-4000-8000-000000000004";
    /// 超管夹具（t_72e95761：本库没有 bootstrap admin 时用它顶替「admin」）。
    const U_SUPER: &str = "d2a2000a-0000-4000-8000-000000000005";

    fn uid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 UUID 非法")
    }

    async fn sql(text: &str) {
        if let Err(err) = db().execute_with_params(text, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {text}");
        }
    }

    /// 审批夹具：四个账号 + 角色绑定 + 清掉本卡造过的审批实例。
    ///
    /// 四个种子角色是**共享资源**：别的用例 / 运维给他们绑过的人会让「这条待办派给谁」
    /// 不可判定，所以这里**只留本卡夹具的人**（按 id 白名单删绑定），并在模块末尾说明。
    async fn setup_approval() {
        sql(&format!(
            "DELETE FROM sys_user_role WHERE role_id IN \
                ('{ROLE_DEPT_LEADER}'::uuid, '{ROLE_PM}'::uuid, '{ROLE_GM}'::uuid) \
                AND user_id NOT IN ('{U_DEPT}'::uuid, '{U_PM}'::uuid, '{U_GM}'::uuid)"
        ))
        .await;
        for (id, name) in [
            (U_MAKER, "p3a 制单人"),
            (U_DEPT, "p3a 部门主管"),
            (U_PM, "p3a 采购经理"),
            (U_GM, "p3a 总经理"),
        ] {
            sql(&format!(
                "INSERT INTO sys_user (id, username, password, real_name, status) \
                 VALUES ('{id}'::uuid, 'p3a_{id_tail}', 'x', '{name}', 1) \
                 ON CONFLICT (id) DO NOTHING",
                id_tail = &id[id.len() - 4..]
            ))
            .await;
        }
        for (user, role) in [(U_DEPT, ROLE_DEPT_LEADER), (U_PM, ROLE_PM), (U_GM, ROLE_GM)] {
            sql(&format!(
                "INSERT INTO sys_user_role (user_id, role_id) VALUES ('{user}'::uuid, '{role}'::uuid) \
                 ON CONFLICT DO NOTHING"
            ))
            .await;
        }
        // 本卡夹具的单：先清审批痕迹（**含上一轮删单留下的孤儿实例**，见
        // `po_instance_where` 的第 2 条），再清订单本体 —— 顺序反了就会每轮攒一批孤儿 PENDING。
        purge_fixture_docs().await;
    }

    /// 取「admin」：本库有 bootstrap admin 就用它，否则建一个挂 `SUPER_ADMIN` 角色的夹具。
    ///
    /// t_72e95761 的复现条件是「提交人就是平台超管（兜底审批人的唯一人选）」，
    /// 所以用例必须真的用超管身份提交，不能拿普通制单人糊弄过去。
    async fn admin_like_user() -> Uuid {
        let existing = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL LIMIT 1",
                vec![],
            )
            .await
            .expect("查 bootstrap admin");
        if let Some(id) = existing {
            return id;
        }
        sql(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('{U_SUPER}'::uuid, 'p3a_admin', 'x', 'p3a 超管', 1) \
             ON CONFLICT (id) DO NOTHING"
        ))
        .await;
        sql(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             SELECT '{U_SUPER}'::uuid, id FROM sys_role \
              WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        uid(U_SUPER)
    }

    /// 造一张含税金额 = `amount` 的采购单（单行、税率 0，所以含税 = 不含税）。
    async fn order_with_amount(amount: &str) -> Uuid {
        let request = req("EXCL", &[(MATERIAL_A, "1", amount, "0")]);
        create_purchase_order(&request, Some(uid(U_MAKER)))
            .await
            .unwrap_or_else(|err| panic!("造单失败：{} {}", err.code, err.msg))
    }

    /// 该单在跑的实例 id（没有 → panic）。
    async fn running_instance(order_id: Uuid) -> Uuid {
        db().scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE' \
              AND biz_id = ?::uuid AND status = 'PENDING' LIMIT 1",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap()
        .expect("应当有在跑的审批实例")
    }

    /// 该实例的待办（PENDING），按 assignee 升序。
    async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' \
                  ORDER BY assignee_id, id",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("读待办失败");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("id").unwrap(),
                    row.get_typed("assignee_id").unwrap(),
                )
            })
            .collect()
    }

    /// 某人在本实例上的待办（**不限状态**）：`(task_id, status, comment)`。
    ///
    /// P0-FIX 之后「制单人 / 发起人」不再拿 `PENDING`（生成时就被排除成 `SKIPPED`），
    /// 所以内控用例必须按「人」取行，而不是按 `status = 'PENDING'` 取。
    async fn task_of(instance_id: Uuid, assignee: Uuid) -> Option<(Uuid, String, String)> {
        let rows = db()
            .find_with_params(
                "SELECT id, status, COALESCE(comment,'') AS comment FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND assignee_id = ?::uuid ORDER BY created_at LIMIT 1",
                vec![Value::from(&instance_id), Value::from(&assignee)],
            )
            .await
            .expect("读待办失败");
        rows.first().map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("status").unwrap(),
                row.get_typed("comment").unwrap(),
            )
        })
    }

    /// 以某人的身份同意一条待办，并按 `flow::handler` 的口径推进业务回调。
    async fn approve_as(task_id: Uuid, operator: &str) -> crate::business::flow::engine::ActionOut {
        let op = uid(operator);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::approve(&tx, task_id, op, Some("同意")).await
            })
            .await
            .unwrap_or_else(|err| panic!("同意失败：{err}"));
        purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
        out
    }

    /// 以某人的身份驳回（驳回并终止）。
    async fn reject_as(
        task_id: Uuid,
        operator: &str,
        comment: &str,
    ) -> crate::business::flow::engine::ActionOut {
        let op = uid(operator);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::reject(
                    &tx,
                    task_id,
                    op,
                    comment,
                    crate::business::flow::engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .unwrap_or_else(|err| panic!("驳回失败：{err}"));
        purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
        out
    }

    /// ① 提交审批 → 生成实例 + 状态 SUBMITTED + 回写 approval_instance_id + 写状态日志。
    #[tokio::test]
    async fn pflow3a_po_submit_creates_instance_and_submits() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap_or_else(|err| panic!("提交审批失败：{} {}", err.code, err.msg));

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_SUBMITTED);
        let instance = row.approval_instance_id.expect("必须回写审批实例 id");
        let status: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "PENDING", "实例必须还在跑（等待审批）");
        let biz_no: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT biz_no FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(biz_no, row.order_no, "实例的 biz_no 必须是订单号");
        // 状态日志：建单 1 条 + 提交 1 条
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log WHERE entity_id = ?::uuid \
                  AND entity_type = 'PURCHASE_ORDER'",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(logs, 2, "建单 + 提交审批各一条状态日志");
    }

    /// ② 8000 元（< 1 万）→ 派给「部门主管」。
    #[tokio::test]
    async fn pflow3a_po_8000_goes_to_dept_leader() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        assert_eq!(
            tasks.len(),
            1,
            "第一档只有一个审批人，实际待办 {}",
            tasks.len()
        );
        assert_eq!(
            tasks[0].1,
            uid(U_DEPT),
            "8000 元必须派给 DEPT_LEADER（部门主管）"
        );
    }

    /// ②b t_72e95761：「admin（超管）提交 8000 元采购单」必须停在 SUBMITTED + 1 条待办。
    ///
    /// 卡面复现路径就是 `POST /purchase-order` → `PATCH /status {SUBMITTED}`，操作人 admin
    /// （SUPER_ADMIN）。以前没人批就到 APPROVED：种子 `po_approve` 的 `n_dept` 是
    /// `empty_assignee=FALLBACK_ADMIN`，部门主管角色 0 人时兜底到**发起人本人**，
    /// 内控把他排除后节点 0 人 → 引擎按「空审批人」自动通过。
    ///
    /// 现在必须满足：单据停在 SUBMITTED、实例 PENDING、**有且仅有 1 条 PENDING 待办**、
    /// 待办落在部门主管头上、admin 自己一条待办都没有（内控不许自审）、批完才 APPROVED。
    #[tokio::test]
    async fn t72957_admin_submit_8000_waits_for_a_real_approver() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let admin = admin_like_user().await;

        // 与卡面一致：admin 自己造单、自己提交（DRAFT → SUBMITTED 走 submit_approval）
        let request = req("EXCL", &[(MATERIAL_A, "1", "8000", "0")]);
        let id = create_purchase_order(&request, Some(admin))
            .await
            .unwrap_or_else(|err| panic!("admin 造单失败：{} {}", err.code, err.msg));
        submit_approval(id, Some(admin), &none_gate())
            .await
            .unwrap_or_else(|err| panic!("admin 提交失败：{} {}", err.code, err.msg));

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(
            row.status, STATUS_SUBMITTED,
            "提交后必须停在 SUBMITTED —— 没有任何人点过「通过」，不许是 APPROVED"
        );
        assert!(
            row.approved_at.is_none(),
            "没人点过通过，approval 时间必须为空"
        );
        let instance = row
            .approval_instance_id
            .expect("SUBMITTED 必须回写审批实例 id");
        let instance_status: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance_status, "PENDING", "实例必须还在等审批");

        let tasks = pending_tasks(instance).await;
        assert_eq!(
            tasks.len(),
            1,
            "必须恰好 1 条待办（流程真的在等人批），实际 {tasks:?}"
        );
        assert_eq!(tasks[0].1, uid(U_DEPT), "8000 元档的待办要落在部门主管头上");
        assert!(
            task_of(instance, admin).await.is_none(),
            "admin 是制单人 + 发起人，内控不许他拿到一条自己的待办"
        );

        // 部门主管批完才 APPROVED（证明这条链路还能走完，不是把单卡死）
        approve_as(tasks[0].0, U_DEPT).await;
        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, "APPROVED", "部门主管点通过之后才允许 APPROVED");
        assert_eq!(row.approved_by, Some(uid(U_DEPT)));
    }

    /// ③ 50000 元（1–10 万）→ 派给「采购经理」。
    #[tokio::test]
    async fn pflow3a_po_50000_goes_to_purchase_manager() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("50000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].1,
            uid(U_PM),
            "50000 元必须派给 PURCHASE_MANAGER（采购经理）"
        );
    }

    /// ④ 500000 元（≥ 10 万）→ 采购经理 + 总经理**会签**，只批一人不通过。
    #[tokio::test]
    async fn pflow3a_po_500000_needs_both_countersigners() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("500000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let instance = running_instance(id).await;
        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 2, "会签节点必须同时生成 2 条待办");
        let assignees: Vec<Uuid> = tasks.iter().map(|(_, who)| *who).collect();
        assert!(assignees.contains(&uid(U_PM)), "{}", U_PM);
        assert!(assignees.contains(&uid(U_GM)), "{}", U_GM);

        // 只批采购经理那一票 → 单据仍是 SUBMITTED（会签没走完）
        let pm_task = tasks.iter().find(|(_, who)| *who == uid(U_PM)).unwrap().0;
        let out = approve_as(pm_task, U_PM).await;
        assert_eq!(out.instance_status, "PENDING", "会签没走完不能结束");
        assert_eq!(
            purchase_model::find_order(id)
                .await
                .unwrap()
                .unwrap()
                .status,
            STATUS_SUBMITTED,
            "只批一票不能变 APPROVED"
        );

        // 总经理那一票再批 → 实例结束，单据 APPROVED
        let gm_task = tasks.iter().find(|(_, who)| *who == uid(U_GM)).unwrap().0;
        let out = approve_as(gm_task, U_GM).await;
        assert_eq!(out.instance_status, "APPROVED");
        assert_eq!(
            purchase_model::find_order(id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "APPROVED",
            "两人都批才 APPROVED"
        );
    }

    /// ⑤ 审批通过 → 单据 APPROVED（此后可收货）。
    #[tokio::test]
    async fn pflow3a_po_approve_marks_order_approved() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        approve_as(tasks[0].0, U_DEPT).await;

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, "APPROVED");
        assert!(row.approved_at.is_some(), "审批通过必须写 approved_at");
        assert_eq!(row.approved_by, Some(uid(U_DEPT)), "审批人要落库");
        assert!(
            row.approval_instance_id.is_some(),
            "审批通过后保留实例 id 作审计（详情显示「审批通过 FLW-xxx」）"
        );
        // 采购买单进入可收货态
        assert!(stockin_model::purchase_receivable(&row.status));
    }

    /// ⑥ 审批驳回 → 回 DRAFT + 审批意见写进 remark。
    #[tokio::test]
    async fn pflow3a_po_reject_back_to_draft_with_comment() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        reject_as(tasks[0].0, U_DEPT, "预算不够，下季度再买").await;

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(
            row.status, "DRAFT",
            "驳回必须打回草稿（不是关闭、不是取消）"
        );
        let remark = row.remark.unwrap_or_default();
        assert!(
            remark.contains("预算不够"),
            "审批意见必须写进 remark，实际：{remark}"
        );
        assert!(
            row.approval_instance_id.is_none(),
            "打回草稿必须清空审批实例（否则下次提交的判断被污染）"
        );
        // 打回后还能重新提交（边表 DRAFT → SUBMITTED 仍合法）
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect("打回后必须能重新提交审批");
    }

    /// ⑦ 制单人自批 → 403 + 65019（内控：制单与审批分离）。
    ///
    /// P0-FIX 之后语义是「**生成待办时就排除**」：制单人不再是 `PENDING`，而是留一条
    /// `SKIPPED` 行（带内控原因）—— 他既看不到待办，也批不了；而他人照常能批。
    /// 403 兜底仍在（转办 / 加签 / 老数据绕过时会走到它）。
    #[tokio::test]
    async fn pflow3a_po_maker_cannot_approve_own_order() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        // 让制单人**同时**是部门主管：内控要拦的就是这种「自己提自己批」
        sql(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             VALUES ('{U_MAKER}'::uuid, '{ROLE_DEPT_LEADER}'::uuid) ON CONFLICT DO NOTHING"
        ))
        .await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let instance = running_instance(id).await;
        let (mine, status, comment) = task_of(instance, uid(U_MAKER))
            .await
            .expect("制单人在节点上必须有留痕行（不删待办，只改状态）");
        assert_eq!(
            status, "SKIPPED",
            "P0-FIX：制单人的票在生成时就被内控排除，不进 PENDING"
        );
        assert!(comment.contains("内控"), "留痕要写原因：{comment}");
        assert!(
            pending_tasks(instance)
                .await
                .iter()
                .all(|(_, who)| *who != uid(U_MAKER)),
            "制单人不该出现在「待批」清单里（他点不动）"
        );

        let err = purchase_bridge::guard_self_approval(mine, uid(U_MAKER))
            .await
            .expect_err("自批必须被拦");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(
            err.msg.contains("不能审批自己提交的采购订单"),
            "{}",
            err.msg
        );
        // 换成别的部门主管就能批（闸只拦本人）
        purchase_bridge::guard_self_approval(mine, uid(U_DEPT))
            .await
            .expect("他人可以正常审批");
        sql(&format!(
            "DELETE FROM sys_user_role WHERE user_id = '{U_MAKER}'::uuid \
              AND role_id = '{ROLE_DEPT_LEADER}'::uuid"
        ))
        .await;
    }

    /// ⑧ 重复提交审批 → 409（报文带审批单号，O5）。
    #[tokio::test]
    async fn pflow3a_po_duplicate_submit_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let instance = running_instance(id).await;
        let instance_no: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();

        let err = submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect_err("重复提交必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains(&instance_no),
            "报文要点名审批单号：{}",
            err.msg
        );
        // 不能因此多出一张审批单
        let count: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE' AND biz_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 1, "同一单只能有一张审批实例");
    }

    /// ⑨ **P1-FIX 验收标准 1 + 3**：提交审批后 `form_data` 里冻结了显示名 / 明细，
    /// 且「我的待办」列表（`TaskRes`）直接带出同一份 `form_data`（前端不必再逐单查实例详情）。
    ///
    /// 这条用**真库 + 真审批链**跑：`create_purchase_order` → `submit_approval` → 读
    /// `biz_flow_instance.form_data` / `model::page_my_tasks`。卡面第 1 条的验收 SQL
    /// 就是这里的第一步。
    #[tokio::test]
    async fn p1fix_po_form_data_freezes_display_names_items_and_task_carries_it() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        // 8000 元 → 部门主管单节点（U_DEPT），便于查「我的待办」。
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap();
        let instance = running_instance(id).await;

        // 验收标准 1 的 SQL：直接读库里冻结的那一份。
        let text: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT form_data::text FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .expect("实例必须存在");
        let form: serde_json::Value =
            serde_json::from_str(&text).expect("form_data 必须是合法 JSON");

        // 显示名：审批岗位角色没有 business:erp:* 权限，这两个键是「看不见 ID 也能看懂」的全部依靠。
        assert_eq!(
            form["supplier_name"], "P2A2 测试供应商",
            "供应商中文名必须冻结进 form_data：{form}"
        );
        assert_eq!(
            form["warehouse_name"], "P2A2 测试仓",
            "收货仓库中文名必须冻结进 form_data：{form}"
        );
        // 明细：八个键齐全（前端按存在的键出列，少一个就是少一列）。
        let items = form["items"].as_array().expect("items 必须是数组");
        assert_eq!(items.len(), 1, "本单只有一行明细：{form}");
        for key in form_snapshot::ITEM_KEYS {
            assert!(
                items[0].get(key).is_some(),
                "明细行缺键 `{key}`（前端会少一列）：{form}"
            );
        }
        assert_eq!(items[0]["material_code"], "P2A2-M1");
        assert_eq!(items[0]["material_name"], "螺丝 M6x20");
        assert_eq!(items[0]["unit_name"], "个");
        assert_eq!(items[0]["qty"], "1.0000");
        assert_eq!(items[0]["amount"], "8000.0000");
        // 条件边要用的 `amount` 仍然是 **JSON 数字**（写成字符串会让条件边静默走兜底边）。
        assert_eq!(form["amount"], 8000.0);
        assert!(form["amount"].is_number(), "amount 必须是数字：{form}");

        // 验收标准 3：待办列表行自带同一份 form_data（`TaskRes.form_data`）。
        let todo = crate::business::flow::model::page_my_tasks(
            uid(U_DEPT),
            crate::business::flow::model::MY_TODO_STATUSES,
            &crate::business::flow::dto::TASK_KINDS,
            crate::business::flow::model::MyTaskOrder::Arrived,
            1,
            20,
            true,
        )
        .await
        .expect("查我的待办");
        let mine = todo
            .list
            .iter()
            .find(|row| row.task.instance_id == instance)
            .expect("部门主管必须有一条待办");
        assert_eq!(
            mine.form_data["supplier_name"], "P2A2 测试供应商",
            "待办列表必须直接带出 form_data（否则前端又得逐单补详情）"
        );
        assert_eq!(
            mine.form_data["total_with_tax"], "8000.0000",
            "卡片摘要要用的价税合计也在里面"
        );
        assert_eq!(mine.form_data["items"][0]["material_name"], "螺丝 M6x20");
    }

    /// ⑩ 显示名查不到（供应商被硬删 / id 写错）→ **不写这个键**，其余字段照旧。
    ///
    /// 契约：`None` 是「没查到」，不能写空串（前端会显示「供应商：」后面空空的假象），
    /// 更不能因此让审批发不出去。
    #[tokio::test]
    async fn p1fix_po_form_data_omits_unknown_display_names() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "1", "100", "0")]), None)
            .await
            .unwrap();
        let mut row = purchase_model::find_order(id).await.unwrap().unwrap();
        // 指向一个不存在的供应商 / 仓库（模拟主数据被删）。
        row.supplier_id = Uuid::nil();
        row.warehouse_id = Uuid::nil();
        // 信用评估结果按「额度不限」传：本用例只钉显示名的降级口径
        let credit =
            purchase_credit::CreditVerdict::unassessable(Decimal::ZERO, row.total_with_tax);
        // FIN-1：预算判定用「未解析到科目」的空结果（本用例只钉显示名降级口径）
        let budget = budget_guard::PurchaseBudget {
            mode: budget_guard::ControlMode::Warn,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: String::new(),
        };
        let form = purchase_form_data(&row, &credit, &budget).await;
        assert!(
            form.get("supplier_name").is_none(),
            "查不到供应商名就不能写这个键：{form}"
        );
        assert!(
            form.get("warehouse_name").is_none(),
            "查不到仓库名就不能写这个键：{form}"
        );
        // ids / 金额 / 明细照旧（降级只影响「显示名」）。
        assert_eq!(form["supplier_id"], Uuid::nil().to_string());
        assert_eq!(form["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(form["order_no"], row.order_no.as_str());
    }

    // ---------- ⑬ 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留**「PO_APPROVE 实例 / 待办 / 状态日志 / 夹具订单」四类残留。
    ///
    /// 为什么必须有这一条：`setup()` / `setup_approval()` 只在**每个用例开头**跑，
    /// 模块最后一个用例的产物没人收；同一库连跑第二遍时，遗留的
    /// `PO_APPROVE | PENDING` 会顶住 `uk_biz_flow_instance_biz_active`（下一次提交永久 409），
    /// `biz_status_log` 则每轮多攒几十行永远对不上的孤儿。libtest 按名字排序，`zz_` 必然最后。
    ///
    /// 顺带钉住「孤儿也算本夹具的」这条口径：本用例造的单被清场删掉后，实例
    /// （`biz_id` 是弱引用）必须**跟着一起**被 `purge_fixture_docs` 收走 ——
    /// 这正是「每跑一轮多 7 条 PENDING」的结构性缺口。
    #[tokio::test]
    async fn zz_teardown_purges_po_approval_residue() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect("提交审批应成功");
        let instance_id = running_instance(id).await;
        assert!(
            !pending_tasks(instance_id).await.is_empty(),
            "清场前必须有待办，否则这条用例证明不了什么"
        );

        purge_fixture_docs().await;

        // ① 单据没了
        assert!(
            purchase_model::find_order(id)
                .await
                .expect("查单据")
                .is_none(),
            "清场后采购订单必须不存在"
        );
        // ② 实例没了（**孤儿也认**：单据已删，实例仍必须被收走）
        for (label, sql) in [
            (
                "残留实例",
                "SELECT count(*) FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
            ),
            (
                "残留待办",
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid".to_string(),
            ),
        ] {
            let n: i64 = db()
                .scalar_with_params::<Option<i64>>(&sql, vec![Value::from(&instance_id)])
                .await
                .expect("查残留")
                .unwrap_or(0);
            assert_eq!(n, 0, "清场后{label}必须为 0");
        }
        // ③ 状态日志没了（无外键 → 必须在删单据之前清）
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'PURCHASE_ORDER' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert_eq!(logs, 0, "清场后状态日志必须为 0");
        // ④ 全库不剩「在跑的采购审批实例」—— 顶住唯一索引的就是它
        let running: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_instance \
                  WHERE biz_type = 'PO_APPROVE' AND status = 'PENDING' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("查在跑实例")
            .unwrap_or(0);
        assert_eq!(
            running, 0,
            "模块收尾后不许剩 PENDING 的采购审批实例（同库连跑第二遍的前提）"
        );
    }
    // ==================== A3：采购单绑定合同（CONTRACT / SPOT 双类型） ====================
    //
    // 口径锚点：设计 §6.2-① 的四条写入链路 + ARCH-2 §5.1 V-22（合同可用状态）/V-23（额度现算）。
    // 每个真库用例开头 `setup()`：清上一轮 → 建夹具（供应商 / 仓 / 物料 / 三张合同）。

    use crate::business::erp::dto::{PurchaseOrderFilter, PurchaseOrderRes};
    use nik::prelude::chrono::NaiveDate;
    use nik::uuid::Uuid;

    /// 组一张合同采购单（`CONTRACT` 类型）：金额 = 10 × 2 × 1.13 = 22.60。
    fn contract_req(contract_id: &str, supplier: Option<&str>) -> CreatePurchaseOrderReq {
        CreatePurchaseOrderReq {
            supplier_id: supplier.map(str::to_string),
            warehouse_id: Some(WAREHOUSE.to_string()),
            purchase_type: Some("CONTRACT".to_string()),
            contract_id: Some(contract_id.to_string()),
            price_mode: Some("EXCL".to_string()),
            items: vec![PurchaseOrderItemReq {
                material_id: Some(MATERIAL_A.to_string()),
                qty: Some(dec("10")),
                price_excl: Some(dec("2")),
                tax_rate: Some(dec("13")),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// ⑧ CONTRACT + 有效合同：成功，供应商**从合同乙方派生**，两个快照都写对。
    #[tokio::test]
    async fn contract_order_derives_supplier_and_writes_snapshots() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 故意不传 supplier_id：CONTRACT 允许省略（唯一取值点是合同乙方）
        let id = create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.purchase_type, "CONTRACT");
        assert_eq!(detail.order.supplier_id, SUPPLIER, "供应商必须 = 合同乙方");
        assert_eq!(detail.order.contract_id.as_deref(), Some(CONTRACT_ACTIVE));
        assert_eq!(
            detail.order.contract_no.as_deref(),
            Some(CONTRACT_ACTIVE_NO)
        );
        assert_eq!(
            detail.order.contract_no_snapshot.as_deref(),
            Some(CONTRACT_ACTIVE_NO),
            "落库的是合同号**快照**，不是运行时 JOIN 出来的"
        );
        assert_eq!(
            detail.order.supplier_name_snapshot.as_deref(),
            Some("P2A2 测试供应商")
        );
        assert_eq!(
            detail.order.supplier_name.as_deref(),
            Some("P2A2 测试供应商")
        );
    }

    /// ⑨ CONTRACT 但 contract_id 为空 → 400（业务码 66023，不是 500）。
    #[test]
    fn contract_order_without_contract_id_is_rejected() {
        let mut request = contract_req(CONTRACT_ACTIVE, None);
        request.contract_id = None;
        let err = request.prepare().unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_PO_CONTRACT_REQUIRED);
        assert!(err.msg.contains("contract_id"), "{}", err.msg);
    }

    /// ⑩ CONTRACT 但合同不存在（幽灵 UUID）→ 400 + 66002（弱引用缺失，不是 500）。
    #[tokio::test]
    async fn contract_order_with_unknown_contract_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let ghost = "d2a20001-0000-4000-8000-0000000000ff";
        let err = create_purchase_order(&contract_req(ghost, None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
        assert!(err.msg.contains("合同不存在"), "{}", err.msg);
    }

    /// ⑪ CONTRACT 挂 DRAFT 合同 → 400（内控：草稿合同不能下单）。
    #[tokio::test]
    async fn contract_order_on_draft_contract_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = create_purchase_order(&contract_req(CONTRACT_DRAFT, None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_CONTRACT_NOT_USABLE);
        assert!(
            err.msg.contains("草稿"),
            "文案要说清是状态问题：{}",
            err.msg
        );
    }

    /// ⑫ CONTRACT 但传入的 supplier_id ≠ 合同乙方 → 400 + 66025（不静默按合同方覆盖）。
    #[tokio::test]
    async fn contract_order_with_mismatched_supplier_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let other = "d2a20001-0000-4000-8000-0000000000ee";
        let err = create_purchase_order(&contract_req(CONTRACT_ACTIVE, Some(other)), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_PO_SUPPLIER_MISMATCH);
        assert!(err.msg.contains("从合同乙方带出"), "{}", err.msg);
    }

    /// ⑬ CONTRACT 且 supplier_id 与合同乙方**一致** → 幂等通过（前端回显整单时不会被误拒）。
    #[tokio::test]
    async fn contract_order_accepts_matching_supplier() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&contract_req(CONTRACT_ACTIVE, Some(SUPPLIER)), None)
            .await
            .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.supplier_id, SUPPLIER);
    }

    /// ⑭ SPOT 却传了 contract_id → 400 + 66024（不一致必须报错，不能静默丢弃字段）。
    #[test]
    fn spot_order_with_contract_id_is_rejected() {
        let mut request = req("EXCL", &[(MATERIAL_A, "1", "2", "13")]);
        request.purchase_type = Some("SPOT".to_string());
        request.contract_id = Some(CONTRACT_ACTIVE.to_string());
        let err = request.prepare().unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_PO_CONTRACT_UNEXPECTED);
        assert!(err.msg.contains("SPOT"), "{}", err.msg);
    }

    /// ⑮ SPOT 正常建单（不传 purchase_type = 默认 SPOT）→ 快照为 NULL，供应商名称照写。
    #[tokio::test]
    async fn spot_order_keeps_contract_snapshot_null() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "5", "2", "13")]), None)
            .await
            .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.order.purchase_type, "SPOT",
            "缺省必须是 SPOT（与列默认值一致）"
        );
        assert!(detail.order.contract_id.is_none());
        assert_eq!(
            detail.order.contract_no_snapshot, None,
            "SPOT 的合同号快照必须为 NULL"
        );
        assert_eq!(
            detail.order.supplier_name_snapshot.as_deref(),
            Some("P2A2 测试供应商")
        );
    }

    /// ⑯ 改单想改 purchase_type → 400 + 66022（CONTRACT → SPOT 会洗掉合同关联）。
    #[tokio::test]
    async fn update_cannot_change_purchase_type() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let mut change = contract_req(CONTRACT_ACTIVE, None);
        change.purchase_type = Some("SPOT".to_string());
        change.contract_id = None;
        change.supplier_id = Some(SUPPLIER.to_string());
        let err = update_purchase_order(id, &change, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_PO_TYPE_LOCKED);
        assert!(err.msg.contains("不允许改"), "{}", err.msg);
        // 库里必须还是 CONTRACT（没有被偷偷改掉）
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.purchase_type, "CONTRACT");
    }

    /// ⑰ 改单**省缺** purchase_type → 沿用当前类型（CONTRACT 单不会被默认成 SPOT）。
    #[tokio::test]
    async fn update_without_type_keeps_contract_type() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let mut change = contract_req(CONTRACT_ACTIVE, None);
        change.purchase_type = None;
        change.remark = Some("改备注".to_string());
        update_purchase_order(id, &change, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.purchase_type, "CONTRACT");
        assert_eq!(detail.order.contract_id.as_deref(), Some(CONTRACT_ACTIVE));
        assert_eq!(detail.order.remark.as_deref(), Some("改备注"));
    }

    /// ⑱ 列表页下发合同快照列，且按 contract_id 筛选命中（走 idx_biz_po_contract）。
    #[tokio::test]
    async fn page_exposes_contract_snapshot_columns() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let filter = PurchaseOrderFilter {
            contract_id: Some(CONTRACT_ACTIVE.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let (rows, total) = crate::business::erp::model::purchase_order_page(&filter)
            .await
            .unwrap();
        assert_eq!(total, 1, "按合同筛选必须只命中这一张");
        let mut list: Vec<PurchaseOrderRes> = rows.iter().map(PurchaseOrderRes::from).collect();
        fill_supplier_names(&mut list).await;
        fill_project_names(&mut list).await;
        assert_eq!(list[0].purchase_type, "CONTRACT");
        assert_eq!(list[0].contract_no.as_deref(), Some(CONTRACT_ACTIVE_NO));
        assert_eq!(
            list[0].supplier_name.as_deref(),
            Some("P2A2 测试供应商"),
            "列表用快照列显示供应商名，不 JOIN 合同表"
        );
    }

    /// ⑲ 存量 SPOT 单（A3 之前建的，两个快照列都是 NULL）查询不受影响：
    ///    `purchase_type` 落到默认值 `SPOT`，供应商名称由 handler 用主档现值兜底。
    #[tokio::test]
    async fn legacy_spot_order_still_readable() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 模拟 V0198 之前的历史行：完全不写 purchase_type / 两个快照列
        let legacy = Uuid::now_v7();
        let sql = format!(
            "INSERT INTO biz_purchase_order \
                (id, order_no, supplier_id, order_date, warehouse_id, status, price_mode, \
                 total_qty, total_amount, total_tax, total_with_tax) \
             VALUES ('{legacy}'::uuid, 'A3-LEGACY-1', '{SUPPLIER}'::uuid, current_date, \
                 '{WAREHOUSE}'::uuid, 'DRAFT', 'EXCL', 1, 2, 0.26, 2.26)"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .expect("写存量行");
        let detail = detail_of(legacy).await.unwrap();
        assert_eq!(
            detail.order.purchase_type, "SPOT",
            "存量单必须落默认值 SPOT"
        );
        assert!(detail.order.contract_id.is_none());
        assert_eq!(
            detail.order.supplier_name_snapshot, None,
            "存量行没有名称快照（不回填）"
        );
        assert_eq!(
            detail.order.supplier_name.as_deref(),
            Some("P2A2 测试供应商"),
            "显示层用主档现值兜底，不能显示空白"
        );
    }

    /// ⑳ 合同额度守卫（V-23 现算）：合同只有 1 元，本单 22.60 → 409 + 66019。
    #[tokio::test]
    async fn contract_amount_limit_is_enforced() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = create_purchase_order(&contract_req(CONTRACT_TINY, None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_CONTRACT_AMOUNT_EXCEEDED);
        assert!(err.msg.contains(CONTRACT_TINY_NO), "{}", err.msg);
        // 没有落库（事务回滚）
        let filter = PurchaseOrderFilter {
            contract_id: Some(CONTRACT_TINY.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let (_, total) = crate::business::erp::model::purchase_order_page(&filter)
            .await
            .unwrap();
        assert_eq!(total, 0, "超限的单必须整体回滚，不留半张");
    }

    /// ㉑ 额度是**累计现算**：第一张成功后，`committed_amount` 必须等于这张的含税金额。
    #[tokio::test]
    async fn committed_amount_sums_previous_orders() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let used =
            purchase_model::committed_amount(db(), Uuid::parse_str(CONTRACT_ACTIVE).unwrap(), None)
                .await
                .unwrap();
        assert_eq!(
            used,
            dec("22.60"),
            "已下单额必须现算（SUM 未取消单），不是缓存列"
        );
        // 第二张同样能建（累计 45.20 远小于 10 万）
        let second = create_purchase_order(&contract_req(CONTRACT_ACTIVE, None), None)
            .await
            .unwrap();
        let detail = detail_of(second).await.unwrap();
        assert_eq!(detail.order.contract_id.as_deref(), Some(CONTRACT_ACTIVE));
    }

    /// ㉒ 下单日期超出合同有效期 → 400 + 66014（V-22 第四步）。
    #[tokio::test]
    async fn contract_order_outside_validity_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let mut request = contract_req(CONTRACT_ACTIVE, None);
        request.order_date = Some(NaiveDate::from_ymd_opt(2028, 1, 5).unwrap());
        let err = create_purchase_order(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_CONTRACT_NOT_USABLE);
        assert!(err.msg.contains("到期日"), "{}", err.msg);
    }

    /// ㉓ 非法 purchase_type → 400 + 66026（不是静默当 SPOT）。
    #[test]
    fn invalid_purchase_type_is_rejected() {
        let mut request = req("EXCL", &[(MATERIAL_A, "1", "2", "13")]);
        request.purchase_type = Some(" contrCAT ".to_string());
        let err = request.prepare().unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_PO_TYPE_INVALID);
        assert!(
            err.msg.contains("CONTRACT"),
            "文案要给出可选项：{}",
            err.msg
        );
    }

    /// ㉔ 类型大小写 / 空格归一化：`"  contract  "` 等价于 `CONTRACT`。
    #[test]
    fn purchase_type_is_normalized() {
        let mut request = contract_req(CONTRACT_ACTIVE, None);
        request.purchase_type = Some("  contract  ".to_string());
        let prepared = request.prepare().unwrap();
        assert_eq!(prepared.purchase_type, "CONTRACT");
    }

    // ==================== P2-2 ①：供应商信用额度（ARCH-3 DC-12） ====================
    //
    // 口径锚点：`purchase_credit` 的文件头（现算 / 0 = 不限 / 作废不计入 / 策略 B）。
    // 每个用例开头 `setup()` + `setup_approval()`，并**显式重设**供应商额度 ——
    // 供应商是共享夹具，额度必须在用例里写死，不能依赖上一个用例留下的值。

    /// 把夹具供应商的信用额度设成 `limit`（`0` = 不限）。
    async fn set_credit_limit(limit: &str) {
        sql(&format!(
            "UPDATE biz_supplier SET credit_limit = {limit}::numeric WHERE id = '{SUPPLIER}'::uuid"
        ))
        .await;
    }

    /// 读单据的信用评估三元组（`(over, exposure, limit)`，`exposure`/`limit` 转字符串便于断言）。
    async fn credit_of(order_id: Uuid) -> (bool, Option<String>, Option<String>) {
        let row = purchase_model::find_order(order_id)
            .await
            .expect("查单据")
            .expect("单据应存在");
        (
            row.over_credit_limit,
            row.credit_exposure
                .map(|value| value.normalize().to_string()),
            row.credit_limit_snapshot
                .map(|value| value.normalize().to_string()),
        )
    }

    /// **P2-2-①a**：供应商额度 1 万，下单 2 万 → 提交后单据带 `over_credit_limit = true`。
    ///
    /// 同时钉住三条：
    /// ① 草稿阶段**未评估**（`credit_exposure IS NULL`，不是「评估过没超」）；
    /// ② 提交时点快照三元组正确（累计 20000 / 额度 10000）；
    /// ③ 审批单 `form_data` 里 `over_credit_limit` 是 **JSON 布尔 `true`**、
    ///    `credit_exposure`/`credit_limit` 是 **JSON 数字** —— 流程管理员据此配条件边，
    ///    类型写错（字符串 "true" / "20000.0000"）会让条件 TypeMismatch 静默走兜底边。
    #[tokio::test]
    async fn p2_2_credit_over_limit_marks_order_and_goes_to_approval() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("10000").await;
        let id = order_with_amount("20000").await;

        // ① 草稿 = 未评估（`false` + NULL，而不是「查过没超」）
        assert_eq!(
            credit_of(id).await,
            (false, None, None),
            "草稿阶段不该有评估快照（NULL = 没查过）"
        );

        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "超限单应当能提交（策略 B：允许 + 强制审批）：{} {}",
                    err.code, err.msg
                )
            });

        // ② 快照三元组（`normalize()` 去掉 PG 补的尾零）
        assert_eq!(
            credit_of(id).await,
            (true, Some("20000".to_string()), Some("10000".to_string())),
            "提交时点必须冻结「累计 2 万 / 额度 1 万 / 超限」"
        );

        // ③ 审批单 form_data 的三个键（类型也要对）
        let instance_id = running_instance(id).await;
        let form = form_data_of(instance_id).await;
        assert_eq!(
            form["over_credit_limit"],
            serde_json::Value::Bool(true),
            "必须是 JSON 布尔 true（不是字符串）：{form}"
        );
        assert_eq!(
            form["credit_exposure"].as_f64(),
            Some(20000.0),
            "累计必须是 JSON 数字：{form}"
        );
        assert_eq!(
            form["credit_limit"].as_f64(),
            Some(10000.0),
            "额度必须是 JSON 数字：{form}"
        );

        // ④ 真机同款结论在引擎里也成立：这样一条边真的会命中（不是纸面约定）
        use crate::business::flow::graph::{CompareOp, Condition, EvalContext, eval};
        let condition = Condition::Cmp {
            field: "form.over_credit_limit".to_string(),
            cmp: CompareOp::Eq,
            value: serde_json::Value::Bool(true),
        };
        let context = EvalContext {
            form: form
                .as_object()
                .expect("form_data 必须是对象")
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            eval(&condition, &context),
            Ok(true),
            "「超信用额度」条件分支必须命中"
        );
    }

    /// **P2-2-①b**：额度 10 万，下单 8 千 → **不**标记超限，但累计照记。
    #[tokio::test]
    async fn p2_2_credit_within_limit_is_not_marked() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("100000").await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect("额度内的单必须能提交");

        let (over, exposure, limit) = credit_of(id).await;
        assert!(!over, "额度内不该标超限");
        assert_eq!(exposure.as_deref(), Some("8000"), "累计要记下来（审计用）");
        assert_eq!(limit.as_deref(), Some("100000"));

        let form = form_data_of(running_instance(id).await).await;
        assert_eq!(form["over_credit_limit"], serde_json::Value::Bool(false));
    }

    /// **P2-2-①c**：额度为空（库里 = `0`）→ **不校验**，永不标记超限。
    ///
    /// `biz_supplier.credit_limit` 是 `NOT NULL DEFAULT 0`，V0137 的列定义写明「0 = 不限」。
    /// 所以「留空」在本仓就是 `0`，没有 NULL 这个状态 —— 这里断言的是「0 不拦」。
    #[tokio::test]
    async fn p2_2_credit_zero_limit_is_unlimited() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("0").await;
        let id = order_with_amount("50000").await;
        submit_approval(id, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect("额度为 0（不限）时任何金额都必须能提交");

        assert_eq!(
            credit_of(id).await,
            (false, Some("50000".to_string()), Some("0".to_string())),
            "不限额度也要记累计（前端显示「累计 X（额度不限）」）"
        );
    }

    /// **P2-2-①d**：**作废的采购单不计入累计**（卡面红线）。
    ///
    /// 额度 3 万：先下一张 2 万的草稿单并作废，再下一张 2 万的单 → 累计只算后一张（2 万），
    /// **不超限**。若把作废单也算进去（4 万）就会误标超限，白白把用户卡进审批。
    #[tokio::test]
    async fn p2_2_credit_cancelled_order_not_counted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("30000").await;

        let cancelled = order_with_amount("20000").await;
        change_purchase_order_status(
            cancelled,
            &status_req("CANCELLED", None),
            Some(uid(U_MAKER)),
        )
        .await
        .expect("草稿单作废应当成功");

        let live = order_with_amount("20000").await;
        submit_approval(live, Some(uid(U_MAKER)), &none_gate())
            .await
            .expect("额度 3 万、在跑 2 万，第二张 2 万不该被拦");

        assert_eq!(
            credit_of(live).await,
            (false, Some("20000".to_string()), Some("30000".to_string())),
            "作废单（2 万）不许计入累计"
        );
    }

    /// **P2-2-①e**：超限 + **没有已发布流程** → 409 + 66036，且 `skip-approval` **不放行**。
    ///
    /// 这是策略 B 的牙齿：超限单只能走审批。流程没配时不能让持 `business:erp:skip-approval`
    /// 的账号点一下「跳过」就把超限单硬放过去 —— 那样信用额度等于没生效。
    /// 对照组：同一场景下**额度内的单**用同一个 skip gate 是能跳过的（见下一个用例），
    /// 说明这里拒的是「超限」而不是「跳过这个动作本身」。
    #[tokio::test]
    async fn p2_2_credit_over_limit_cannot_skip_approval() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("1000").await;
        let id = order_with_amount("5000").await;
        // 临时下线种子流程（模拟「客户还没配审批」）
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();
        let skip = approval_gate::SkipApprovalReq {
            skip_approval: true,
            skip_reason: Some("月底关账，供应商已现场验收".to_string()),
        };
        let claims = skip_claims();
        let gate = approval_gate::ApprovalGate::new(&skip, &claims);
        let outcome = submit_approval(id, Some(uid(U_MAKER)), &gate).await;
        // 先还原流程再断言（断言 panic 也不能把流程留在 DISABLED）
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();

        let error = outcome.expect_err("超限单不许靠 skip-approval 绕开审批");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(
            error.code,
            error_code::ERP_CREDIT_LIMIT_EXCEEDED,
            "必须是「超信用额度」码（不是泛泛的审批未配置）：{}",
            error.msg
        );
        assert!(error.msg.contains("po_approve"), "{}", error.msg);
        assert!(
            error.msg.contains("信用额度"),
            "报文要点名是额度问题：{}",
            error.msg
        );
        // 单据一动不动
        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_DRAFT, "被拒后必须仍是草稿");
        assert!(row.approval_instance_id.is_none());
        assert_eq!(
            credit_of(id).await,
            (false, None, None),
            "被拒后不该留下「评估过」的快照（否则详情会显示假结论）"
        );
    }

    /// **P2-2-①f**（对照组）：额度内的单，同一 skip gate 在没流程时**可以**跳过 ——
    /// 证明上一个用例拒的是「超限」而不是「跳过」这个动作本身，也顺带覆盖
    /// 「跳过后仍把信用评估快照补上」这条分支（事后回写）。
    #[tokio::test]
    async fn p2_2_credit_within_limit_can_skip_approval_and_still_records_snapshot() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        set_credit_limit("100000").await;
        let id = order_with_amount("8000").await;
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();
        let skip = approval_gate::SkipApprovalReq {
            skip_approval: true,
            skip_reason: Some("月底关账，供应商已现场验收".to_string()),
        };
        let claims = skip_claims();
        let gate = approval_gate::ApprovalGate::new(&skip, &claims);
        let outcome = submit_approval(id, Some(uid(U_MAKER)), &gate).await;
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'po_approve'",
            vec![],
        )
        .await
        .unwrap();

        outcome
            .unwrap_or_else(|err| panic!("额度内的单应当能显式跳过审批：{} {}", err.code, err.msg));
        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_SUBMITTED);
        assert_eq!(
            credit_of(id).await,
            (false, Some("8000".to_string()), Some("100000".to_string())),
            "跳过审批也要补上评估快照（否则这张单永远显示「未评估」）"
        );
    }

    /// 读审批单的 `form_data`（引擎冻结的那份）。
    async fn form_data_of(instance_id: Uuid) -> serde_json::Value {
        let text: Option<String> = db()
            .scalar_with_params::<Option<String>>(
                "SELECT form_data::text FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("读审批单 form_data")
            .filter(|text| !text.trim().is_empty());
        serde_json::from_str(text.as_deref().unwrap_or("{}")).expect("form_data 必须是合法 JSON")
    }

    /// ARCH-3 DC-01：一个**持有** `business:erp:skip-approval` 的 claims（显式跳过审批要用）。
    fn skip_claims() -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: U_MAKER.to_string(),
            username: Some("p22_skip_case".to_string()),
            roles: Vec::new(),
            permissions: vec![approval_gate::PERM_SKIP_APPROVAL.to_string()],
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    // ==================== P2-8：所属项目（project_id，V0237） ====================

    /// 四个项目夹具（`a280…` 段；全仓 grep 过无冲突）。
    ///
    /// 采购单 → 项目是**弱引用**（无外键），所以这些夹具的建/删只影响本模块：
    /// `setup()` 先 `purge_projects()` 再 `seed_projects()`，用例之间互不污染。
    const PROJECT_MAIN: &str = "a2800000-0000-4000-8000-000000000001";
    const PROJECT_OTHER: &str = "a2800000-0000-4000-8000-000000000002";
    const PROJECT_ARCHIVED: &str = "a2800000-0000-4000-8000-000000000003";
    const PROJECT_DELETED: &str = "a2800000-0000-4000-8000-000000000004";
    /// 主项目名（断言「下发的是**现值**」用；软删/改名用例依赖它不是快照）
    const PROJECT_MAIN_NAME: &str = "P2-8 试点项目";

    /// 建 / 重置四个项目夹具。
    ///
    /// 为什么是 `ON CONFLICT (id) DO UPDATE` 而不是 `DO NOTHING`：上一个用例可能把
    /// `PROJECT_MAIN` 软删过（「项目已删只丢名字」那条就这么干），`DO NOTHING` 会把
    /// 已删状态留给下一条用例 → 立刻假红（技能 §1c 的坑）。DO UPDATE 顺手把
    /// `deleted_at` 复位成 NULL。
    async fn seed_projects() {
        let rows = [
            (
                PROJECT_MAIN,
                "P28-MAIN",
                PROJECT_MAIN_NAME,
                "ACTIVE",
                "NULL",
            ),
            (
                PROJECT_OTHER,
                "P28-OTHER",
                "P2-8 备用项目",
                "ACTIVE",
                "NULL",
            ),
            (
                PROJECT_ARCHIVED,
                "P28-ARCH",
                "P2-8 归档项目",
                "ARCHIVED",
                "NULL",
            ),
            (
                PROJECT_DELETED,
                "P28-DEL",
                "P2-8 已删项目",
                "ACTIVE",
                "now()",
            ),
        ];
        for (id, no, name, status, deleted_at) in rows {
            let sql = format!(
                "INSERT INTO biz_project (id, project_no, project_name, status, deleted_at) \
                 VALUES ('{id}'::uuid, '{no}', '{name}', '{status}', {deleted_at}) \
                 ON CONFLICT (id) DO UPDATE SET project_no = EXCLUDED.project_no, \
                     project_name = EXCLUDED.project_name, status = EXCLUDED.status, \
                     deleted_at = EXCLUDED.deleted_at"
            );
            db().execute_with_params(&sql, vec![])
                .await
                .unwrap_or_else(|err| panic!("项目夹具写入失败：{err}\nSQL: {sql}"));
        }
    }

    /// 删本模块的项目夹具（**只删这四行**，不碰库里别人的项目）。
    async fn purge_projects() {
        let sql = format!(
            "DELETE FROM biz_project WHERE id IN ('{PROJECT_MAIN}'::uuid, \
             '{PROJECT_OTHER}'::uuid, '{PROJECT_ARCHIVED}'::uuid, '{PROJECT_DELETED}'::uuid)"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("清理项目夹具失败：{err}"));
    }

    /// 直接读库里的 `project_id::text`：验证「真的落库」，不只看 DTO 回显。
    async fn stored_project_of(id: Uuid) -> Option<String> {
        db().scalar_with_params::<Option<String>>(
            "SELECT project_id::text FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读 project_id")
        .filter(|text| !text.trim().is_empty())
    }

    /// 建单请求 + 所属项目（其余字段取 `req()` 的标准形状）。
    fn req_with_project(project: Option<&str>) -> CreatePurchaseOrderReq {
        let mut request = req("EXCL", &[(MATERIAL_A, "100", "2", "13")]);
        request.project_id = project.map(str::to_string);
        request
    }

    /// ① 建单带项目 → 落库 + 详情同时下发 `project_id` 与 `project_name`。
    #[tokio::test]
    async fn p28_create_with_project_persists_and_reports_name() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req_with_project(Some(PROJECT_MAIN)), None)
            .await
            .expect("建单（带项目）");
        assert_eq!(
            stored_project_of(id).await.as_deref(),
            Some(PROJECT_MAIN),
            "project_id 必须真的写进 biz_purchase_order"
        );
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.project_id.as_deref(), Some(PROJECT_MAIN));
        assert_eq!(
            detail.order.project_name.as_deref(),
            Some(PROJECT_MAIN_NAME),
            "详情要下发项目名（关联查现值）"
        );
    }

    /// ② 建单不带项目 → 落 NULL，详情两个字段都是 `None`（老单/不归集项目的单不受影响）。
    #[tokio::test]
    async fn p28_create_without_project_stays_null() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req_with_project(None), None)
            .await
            .expect("建单（不带项目）");
        assert_eq!(stored_project_of(id).await, None, "不选项目必须落 NULL");
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.project_id, None);
        assert_eq!(detail.order.project_name, None);
    }

    /// ③ 项目不存在 → `400 + 66002`，且**一张单都不落**（校验在建单事务之前）。
    #[tokio::test]
    async fn p28_create_unknown_project_is_400() {
        use crate::business::erp::dto::PurchaseOrderFilter;

        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let ghost = Uuid::now_v7().to_string();
        let err = create_purchase_order(&req_with_project(Some(&ghost)), None)
            .await
            .expect_err("项目不存在必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING, "{}", err.msg);
        assert!(
            err.msg.contains("project_id"),
            "文案必须点名 project_id：{}",
            err.msg
        );
        let filter = PurchaseOrderFilter {
            supplier_id: Some(SUPPLIER.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let (_, total) = crate::business::erp::model::purchase_order_page(&filter)
            .await
            .unwrap();
        assert_eq!(total, 0, "被拒的单不能留下半张");
    }

    /// ④ 项目已软删 → 同上 `400 + 66002`（外键拦不住软删，所以必须走 select 校验）。
    #[tokio::test]
    async fn p28_create_soft_deleted_project_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = create_purchase_order(&req_with_project(Some(PROJECT_DELETED)), None)
            .await
            .expect_err("已软删项目必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING, "{}", err.msg);
        assert!(err.msg.contains("已删除"), "{}", err.msg);
    }

    /// ⑤ 非法 UUID → `400 + 66006`（绝不静默当作「没传」）。
    #[tokio::test]
    async fn p28_create_invalid_project_uuid_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = create_purchase_order(&req_with_project(Some("not-a-uuid")), None)
            .await
            .expect_err("非法 UUID 必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID, "{}", err.msg);
        assert!(err.msg.contains("project_id"), "{}", err.msg);
    }

    /// ⑥ 归档项目**允许**挂（跨期补录是合法业务；状态拦截只在前端下拉）。
    #[tokio::test]
    async fn p28_archived_project_is_allowed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req_with_project(Some(PROJECT_ARCHIVED)), None)
            .await
            .expect("归档项目上补录采购不该被拒");
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.project_id.as_deref(), Some(PROJECT_ARCHIVED));
        assert_eq!(detail.order.project_name.as_deref(), Some("P2-8 归档项目"));
    }

    /// ⑦ 改单：整单覆盖口径 —— 先换项目，再不传项目（= 清空），库里都跟着变。
    #[tokio::test]
    async fn p28_update_reassigns_and_clears_project() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req_with_project(Some(PROJECT_MAIN)), None)
            .await
            .unwrap();
        update_purchase_order(id, &req_with_project(Some(PROJECT_OTHER)), None)
            .await
            .expect("改单换项目");
        assert_eq!(stored_project_of(id).await.as_deref(), Some(PROJECT_OTHER));
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.project_name.as_deref(), Some("P2-8 备用项目"));

        // 整单覆盖：不传 project_id = 清空归属（与 contract_id / remark 同口径）
        update_purchase_order(id, &req_with_project(None), None)
            .await
            .expect("改单清空项目");
        assert_eq!(
            stored_project_of(id).await,
            None,
            "不传 project_id 必须清空"
        );
        assert_eq!(detail_of(id).await.unwrap().order.project_name, None);
    }

    /// ⑧ 列表：按 `project_id` 筛得中该项目的单，且 `project_name` 批量回填。
    #[tokio::test]
    async fn p28_list_filters_by_project_and_fills_name() {
        use crate::business::erp::dto::{PurchaseOrderFilter, PurchaseOrderRes};

        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let with_main = create_purchase_order(&req_with_project(Some(PROJECT_MAIN)), None)
            .await
            .unwrap();
        let _with_other = create_purchase_order(&req_with_project(Some(PROJECT_OTHER)), None)
            .await
            .unwrap();
        let _without = create_purchase_order(&req_with_project(None), None)
            .await
            .unwrap();

        let filter = PurchaseOrderFilter {
            project_id: Some(PROJECT_MAIN.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let (rows, total) = crate::business::erp::model::purchase_order_page(&filter)
            .await
            .unwrap();
        assert_eq!(total, 1, "按项目筛选必须只命中挂在该项目上的那 1 张");
        let mut list: Vec<PurchaseOrderRes> = rows.iter().map(PurchaseOrderRes::from).collect();
        fill_project_names(&mut list).await;
        assert_eq!(list[0].id, with_main.to_string());
        assert_eq!(list[0].project_id.as_deref(), Some(PROJECT_MAIN));
        assert_eq!(
            list[0].project_name.as_deref(),
            Some(PROJECT_MAIN_NAME),
            "列表要显示项目名（不是 uuid）"
        );

        // 不筛项目 → 三张都在（未归集的单不会被筛没）
        let all = PurchaseOrderFilter {
            supplier_id: Some(SUPPLIER.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let (_, total_all) = crate::business::erp::model::purchase_order_page(&all)
            .await
            .unwrap();
        assert_eq!(total_all, 3);
    }

    /// ⑨ 项目被软删后：`project_id` 仍在，但**不再给名字**（前端显示「已删除项目」，不回落 uuid）。
    #[tokio::test]
    async fn p28_soft_deleted_project_keeps_id_but_loses_name() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req_with_project(Some(PROJECT_MAIN)), None)
            .await
            .unwrap();
        db().execute_with_params(
            &format!("UPDATE biz_project SET deleted_at = now() WHERE id = '{PROJECT_MAIN}'::uuid"),
            vec![],
        )
        .await
        .expect("软删项目");
        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.order.project_id.as_deref(),
            Some(PROJECT_MAIN),
            "单据上的归属不因主档软删而消失"
        );
        assert_eq!(
            detail.order.project_name, None,
            "查不到名字就留空，不许把 uuid 当名字下发"
        );
    }

    /// ⑩ 列表筛参校验：合法 UUID 放行、非法 UUID `400 + 66006`（与 contract_id 同口径）。
    #[tokio::test]
    async fn p28_filter_rejects_invalid_project_uuid() {
        use crate::business::erp::dto::PurchaseOrderFilter;

        let ok = PurchaseOrderFilter {
            project_id: Some(PROJECT_MAIN.to_string()),
            ..Default::default()
        };
        assert!(ok.validate().is_ok(), "合法 UUID 应通过");
        let bad = PurchaseOrderFilter {
            project_id: Some("xyz".to_string()),
            ..Default::default()
        };
        let err = bad.validate().expect_err("非法 UUID 必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        assert!(err.msg.contains("project_id"), "{}", err.msg);
    }

    // ==================== P2-9 / DC-23：采购单仓库名快照 ====================

    /// 夹具仓库的原始名（`FIXTURES` 里那份；改名前后的断言都拿它当基准）。
    const WAREHOUSE_NAME: &str = "P2A2 测试仓";

    /// 把夹具仓库恢复成「原名 + 未软删」（每个 P2-9 用例开头调一次：
    /// 上一条用例若 panic 在半路，重命名 / 软删会留在库里，下一条立刻假红）。
    async fn reset_warehouse() {
        db().execute_with_params(
            &format!(
                "UPDATE biz_warehouse SET warehouse_name = '{WAREHOUSE_NAME}', deleted_at = NULL \
                  WHERE id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .expect("复位仓库夹具");
    }

    /// 改仓库名（P2-9 的核心场景：主档改名后历史单据该显示什么）。
    async fn rename_warehouse(name: &str) {
        db().execute_with_params(
            &format!(
                "UPDATE biz_warehouse SET warehouse_name = '{name}' WHERE id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .expect("重命名仓库夹具");
    }

    /// 软删仓库（验证「详情查不到现值 → 回落快照」）。
    async fn soft_delete_warehouse() {
        db().execute_with_params(
            &format!("UPDATE biz_warehouse SET deleted_at = now() WHERE id = '{WAREHOUSE}'::uuid"),
            vec![],
        )
        .await
        .expect("软删仓库夹具");
    }

    /// 直接读库里的 `warehouse_name_snapshot`（验证「真的落库」，不只看 DTO 回显）。
    async fn stored_warehouse_snapshot(id: Uuid) -> Option<String> {
        db().scalar_with_params::<Option<String>>(
            "SELECT warehouse_name_snapshot FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读 warehouse_name_snapshot")
        .filter(|text| !text.trim().is_empty())
    }

    /// 清掉快照列，模拟「P2-9 之前的老单」（验证过渡期的 fallback，不回填）。
    async fn null_warehouse_snapshot(id: Uuid) {
        db().execute_with_params(
            "UPDATE biz_purchase_order SET warehouse_name_snapshot = NULL WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("清空快照列");
    }

    /// 列表口径：与 `handler::purchase_order_page` 同一串回填（快照优先 + 老单兜底）。
    async fn list_rows() -> Vec<PurchaseOrderRes> {
        use crate::business::erp::dto::PurchaseOrderFilter;

        let filter = PurchaseOrderFilter {
            supplier_id: Some(SUPPLIER.to_string()),
            page: 1,
            page_size: 50,
            ..Default::default()
        };
        let (rows, _) = crate::business::erp::model::purchase_order_page(&filter)
            .await
            .expect("列表查询");
        let mut list: Vec<PurchaseOrderRes> = rows.iter().map(PurchaseOrderRes::from).collect();
        fill_warehouse_names(&mut list).await;
        list
    }

    /// ① 建单写快照：`warehouse_name_snapshot` = 当时的仓库名（真落库），详情同值。
    #[tokio::test]
    async fn p9_create_writes_warehouse_name_snapshot() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        reset_warehouse().await;
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "10", "5", "13")]), None)
            .await
            .expect("建单");
        assert_eq!(
            stored_warehouse_snapshot(id).await.as_deref(),
            Some(WAREHOUSE_NAME),
            "warehouse_name_snapshot 必须真的写进 biz_purchase_order"
        );
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.warehouse_name.as_deref(), Some(WAREHOUSE_NAME));
        assert_eq!(
            detail.order.warehouse_name_snapshot.as_deref(),
            Some(WAREHOUSE_NAME),
            "快照列原样下发（与 supplier_name_snapshot 同口径）"
        );
    }

    /// ② 改单刷新快照：仓库改名后改单（同一仓库）→ 快照跟着改成新名。
    #[tokio::test]
    async fn p9_update_refreshes_snapshot() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        reset_warehouse().await;
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "10", "5", "13")]), None)
            .await
            .expect("建单");
        rename_warehouse("P2A2 改名前仓").await;
        update_purchase_order(id, &req("EXCL", &[(MATERIAL_A, "12", "5", "13")]), None)
            .await
            .expect("改单");
        assert_eq!(
            stored_warehouse_snapshot(id).await.as_deref(),
            Some("P2A2 改名前仓"),
            "改单是整单覆盖，快照要刷新成当前仓库名"
        );
        reset_warehouse().await;
    }

    /// ③ 卡面验收：仓库改名后 **列表显示旧名（快照）**、**详情显示现名（关联）**。
    #[tokio::test]
    async fn p9_list_keeps_snapshot_but_detail_shows_live_name() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        reset_warehouse().await;
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "10", "5", "13")]), None)
            .await
            .expect("建单");
        rename_warehouse("P2A2 改名后仓").await;

        let list = list_rows().await;
        let row = list
            .iter()
            .find(|row| row.id == id.to_string())
            .expect("列表里必须有这张单");
        assert_eq!(
            row.warehouse_name.as_deref(),
            Some(WAREHOUSE_NAME),
            "列表读快照：改名后仍显示下单当时的仓库名"
        );

        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.order.warehouse_name.as_deref(),
            Some("P2A2 改名后仓"),
            "详情读现值：点进单据看到现在的仓库名"
        );
        assert_eq!(
            detail.order.warehouse_name_snapshot.as_deref(),
            Some(WAREHOUSE_NAME),
            "快照仍原样下发，前端可以并列显示「现值（原：旧名）」"
        );
        reset_warehouse().await;
    }

    /// ④ 过渡期方案：老单（快照为 NULL）列表 fallback 到关联查，且**不写库**。
    #[tokio::test]
    async fn p9_legacy_null_snapshot_falls_back_without_backfill() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        reset_warehouse().await;
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "10", "5", "13")]), None)
            .await
            .expect("建单");
        null_warehouse_snapshot(id).await;
        rename_warehouse("P2A2 老单改名仓").await;

        let list = list_rows().await;
        let row = list
            .iter()
            .find(|row| row.id == id.to_string())
            .expect("列表里必须有这张单");
        assert_eq!(
            row.warehouse_name.as_deref(),
            Some("P2A2 老单改名仓"),
            "老单没快照 → 批量现查兜底（否则列表一片空白）"
        );

        // 不回填：库里那两列必须还是 NULL / 原值（`fill_warehouse_names` 只读不写）
        assert_eq!(
            stored_warehouse_snapshot(id).await,
            None,
            "显示兜底不许偷偷回填快照（R1：不伪造历史）"
        );
        reset_warehouse().await;
    }

    /// ⑤ 仓库被软删：详情查不到现值 → 回落快照（不许一片空白），列表仍是快照。
    #[tokio::test]
    async fn p9_deleted_warehouse_detail_falls_back_to_snapshot() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        reset_warehouse().await;
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "10", "5", "13")]), None)
            .await
            .expect("建单");
        soft_delete_warehouse().await;

        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.order.warehouse_name.as_deref(),
            Some(WAREHOUSE_NAME),
            "现值查不到时回落快照：详情不能因为仓库被删就变空白"
        );
        let list = list_rows().await;
        let row = list
            .iter()
            .find(|row| row.id == id.to_string())
            .expect("列表里必须有这张单");
        assert_eq!(row.warehouse_name.as_deref(), Some(WAREHOUSE_NAME));
        reset_warehouse().await;
    }
}
