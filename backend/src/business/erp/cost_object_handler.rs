//! 成本对象（生产批）接口层（成本核算 · ARCH-9 §5.2 #1–#6）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-object/...`。
//!
//! | # | 方法 | 路径 | 权限码 |
//! |---|---|---|---|
//! | 1 | GET | `/business/erp/cost-object` | `business:erp:cost-object:list` |
//! | 2 | GET | `/business/erp/cost-object/{id}` | `business:erp:cost-object:list` |
//! | 3 | POST | `/business/erp/cost-object` | `business:erp:cost-object:create` |
//! | 4 | PUT | `/business/erp/cost-object/{id}` | `business:erp:cost-object:update` |
//! | 5 | POST | `/business/erp/cost-object/{id}/close` | `business:erp:cost-object:close` |
//! | 7 | PUT | `/business/erp/cost-object/{id}/output` | `business:erp:cost-object:update` |
//!
//! 说明：编号沿用设计 §5.2（#5 是 `DELETE`，B1-8 卡面未实现软删接口）；#7 是本卡
//! （B2-14）新增的「本期产量与在制」录入，设计 §5.2 的清单里没有它，但它落地的是
//! §4.5.1 数据源矩阵「产量/进度：车间主任每月手填」那一行 —— 不补这个入口，
//! 表上三列永远是 0，calc 第 6 段必然撞 `66067`。
//!
//! # 关批：5 条前置 + 补偿
//!
//! §4.1 的 5 条前置由 [`close_object`] 统一执行：
//!
//! ```text
//! 取事实（model::close_facts，4 条查库 + wip/force_wip）
//!    → 判定（cost_object_dto::evaluate_close，纯函数）
//!    → 有拦截 ⇒ 409 + 66055，报文里逐条列出来（一次让用户把活干完）
//!    → 无拦截 ⇒ 一个事务里：状态机 transition（写 biz_status_log）+ 落 CLOSED
//! ```
//!
//! **为什么关批必须在事务里且状态机先走**：`biz_status_log` 与状态本身必须同生共死；
//! 先写日志再改状态、或先改状态再写日志，任一步失败都会留下「状态变了没轨迹」或
//! 「轨迹说变了但状态没变」的二义记录。
//!
//! # 本期产量与在制（#7）：为什么独立接口、为什么复用 `update` 权限码
//!
//! §4.5.1 的数据源矩阵里「产量 / 进度」是**车间主任每月手填**的三个数
//! （`completed_qty` 累计完工 / `wip_qty` 期末在制 / `completion_pct` 完工进度）。
//! 它们不进主数据表单（`PUT /{id}`）而单独成接口，因为：
//!
//! * **语义不同**：主数据错了改主数据；产量是「本期实际干出来多少」，跨月要反复改；
//! * **守卫不同**：主数据只要 `OPEN`；产量还要求「该对象参与过的期间都没结算」
//!   （结算后这三个数已经变成入库金额与结转凭证，再改就是改写已入账成本）；
//! * **审计不同**：设计 §8.3 R-4 要求留「填写人 + 时间」，写进 `sys_change_log`
//!   （本卡在 `changelog::diff::REGISTRY` 登记了 `biz_cost_object` 的三列）。
//!
//! 权限码**复用** `business:erp:cost-object:update`：卡面允许「`update` 或新开
//! `output`」，而新开一个码要连带种 `sys_permission` / `sys_menu`（迁移），
//! 本卡是「无迁移」卡（三列 V0368 已有）。同一个「改这张表的人」语义上也确实是
//! 同一个权限 —— 真要拆，等菜单权限重排时一起做。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `commission_rule_handler` / `customer_handler` 同款：axum handler 要 `AuthClaims`
//! 才能跑，单测调不到；把「校验 + 落库」收进 `create_object` / `update_object` /
//! `close_object` / `page_objects` 之后，单测走**同一条代码路径**（只跳过认证），
//! 操作人用 `Option<Uuid>` 传。
//!
//! # 取号
//!
//! `object_no = CB-yyyyMMdd-NNNNNN`（设计 §3.1 ② 列注释）。走
//! `voucher::next_doc_no(SeqSpec{seq_type:"COST_OBJECT", prefix:"CB"})` —— 与凭证号
//! 同一套 `biz_seq` 实现（并发不重号，按北京时间日切）。`biz_seq` 行由该函数
//! 幂等补齐，**不需要**为号段单写迁移。

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action as ChangeAction, ChangeTarget, DiffInput};
use crate::business::erp::cost_object_dto::{
    self, CollectSummary, CostObjectCloseReq, CostObjectCreateReq, CostObjectDetailRes, CostObjectFilter,
    CostObjectListRes, CostObjectOutputReq, CostObjectOutputRes, CostObjectPageQuery, CostObjectUpdateReq, OutputSpec,
};
use crate::business::erp::cost_object_model::{self as model, CostObjectRow};
use crate::business::erp::voucher;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与设计 §5.3 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:cost-object:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:cost-object:create";
/// 编辑。
pub const PERM_UPDATE: &str = "business:erp:cost-object:update";
/// 软删（本卡不实现接口，权限码先登记以便菜单种子与前端按钮共用）。
pub const PERM_DELETE: &str = "business:erp:cost-object:delete";
/// 关批。
pub const PERM_CLOSE: &str = "business:erp:cost-object:close";

/// 成本对象全部权限码（设计 §5.3 的 5 个）。
pub const COST_OBJECT_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE, PERM_CLOSE];

/// 取号规则（`CB-yyyyMMdd-000001`）。
pub const OBJECT_SEQ: voucher::SeqSpec<'static> = voucher::SeqSpec {
    seq_type: "COST_OBJECT",
    prefix: "CB",
    seq_name: "成本对象号",
    remark: "成本对象号：CB-yyyyMMdd-000001，按天（CST）滚动",
};

// ==================== 错误出口 ====================

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("成本对象不存在或已删除（{id}）"),
    )
}

// ==================== 引用校验（建 / 改单的查库部分） ====================

/// 建 / 改前把「要查库才知道对不对」的引用逐个落实，并回填车间名快照。
///
/// 任何一项不合格都 400 指名，不做「先落库再报错」：
/// 成本对象是结算的根，挂一个不存在的仓库会在「完工入库」那一步才炸 —— 那时
/// 成本已经算完了，用户只能删单重来。
async fn ensure_refs(spec: &mut cost_object_dto::CostObjectSpec) -> std::result::Result<(), ApiError> {
    if !model::finished_product_exists(spec.product_id)
        .await
        .map_err(|error| utils::db_error(error, "校验成本对象产出品"))?
    {
        return Err(cost_object_dto::bad_request(format!(
            "product_id（{}）指向的物料不存在、已删除，或不是产出品\
             （只允许 material_type IN ('FINISHED','SEMI')）",
            spec.product_id
        )));
    }
    if model::warehouse_of(spec.target_warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "校验成本对象目标仓"))?
        .is_none()
    {
        return Err(cost_object_dto::bad_request(format!(
            "target_warehouse_id（{}）指向的仓库不存在或已删除",
            spec.target_warehouse_id
        )));
    }
    if let Some(location_id) = spec.target_location_id
        && model::location_of(location_id, spec.target_warehouse_id)
            .await
            .map_err(|error| utils::db_error(error, "校验成本对象目标库位"))?
            .is_none()
    {
        return Err(cost_object_dto::bad_request(format!(
            "target_location_id（{location_id}）指向的库位不存在、已删除，或不属于所选仓库"
        )));
    }
    let dept_name = model::dept_name_of(spec.workshop_dept_id)
        .await
        .map_err(|error| utils::db_error(error, "校验成本对象生产车间"))?
        .ok_or_else(|| {
            cost_object_dto::bad_request(format!(
                "workshop_dept_id（{}）指向的部门不存在或已删除",
                spec.workshop_dept_id
            ))
        })?;
    // 车间名快照（审计口径：对象建单时点是谁的车间，之后部门改名不改历史）
    spec.workshop_dept_name = dept_name;
    if let Some(customer_id) = spec.customer_id
        && !model::customer_exists(customer_id)
            .await
            .map_err(|error| utils::db_error(error, "校验成本对象客户"))?
    {
        return Err(cost_object_dto::bad_request(format!(
            "customer_id（{customer_id}）指向的客户不存在或已删除"
        )));
    }
    Ok(())
}

// ==================== 出参组装 ====================

/// 列表行。
fn to_list_res(row: &CostObjectRow) -> CostObjectListRes {
    CostObjectListRes {
        id: row.id.to_string(),
        object_no: row.object_no.clone(),
        object_type: row.object_type.clone(),
        product_id: row.product_id.to_string(),
        product_name: row.product_name.clone(),
        workshop_dept_id: row.workshop_dept_id.to_string(),
        workshop_dept_name: row.dept_display(),
        plan_qty: row.plan_qty.to_string(),
        completed_qty: row.completed_qty.to_string(),
        wip_qty: row.wip_qty.to_string(),
        status: row.status.clone(),
        lot_no: row.lot_no.clone(),
        source_doc_no: row.source_doc_no.clone(),
        cost_total: row.cost_total.to_string(),
    }
}

/// 详情（含归集汇总 / 计算单 / 可去状态）。
///
/// `edges` 必须是**有效边**（`state_machine::effective_edges`）：管理端禁用某条边之后，
/// `next_statuses` 里就不再出现它，前端按钮随之消失（不是「按钮还在、点下去 409」）。
fn to_detail_res(
    row: &CostObjectRow,
    summary: CollectSummary,
    period_items: Vec<cost_object_dto::CostPeriodItemRes>,
    edges: Vec<(String, String)>,
    output_fill: Option<(Option<String>, DateTime<Utc>)>,
) -> CostObjectDetailRes {
    let next_statuses = edges
        .iter()
        .filter(|(from, _)| *from == row.status)
        .map(|(_, to)| to.clone())
        .collect();
    let (output_filled_by, output_filled_at) = match output_fill {
        Some((by, at)) => (by, Some(at.to_rfc3339())),
        None => (None, None),
    };
    CostObjectDetailRes {
        id: row.id.to_string(),
        object_no: row.object_no.clone(),
        object_type: row.object_type.clone(),
        product_id: row.product_id.to_string(),
        product_name: row.product_name.clone(),
        product_code: row.product_code.clone(),
        lot_no: row.lot_no.clone(),
        target_warehouse_id: row.target_warehouse_id.to_string(),
        target_warehouse_name: row.warehouse_name.clone(),
        target_location_id: row.target_location_id.map(|id| id.to_string()),
        target_location_name: row.location_name.clone(),
        plan_qty: row.plan_qty.to_string(),
        completed_qty: row.completed_qty.to_string(),
        wip_qty: row.wip_qty.to_string(),
        completion_pct: row.completion_pct.to_string(),
        material_input_mode: row.material_input_mode.clone(),
        source_doc_no: row.source_doc_no.clone(),
        customer_id: row.customer_id.map(|id| id.to_string()),
        customer_name: row.customer_name.clone(),
        workshop_dept_id: row.workshop_dept_id.to_string(),
        workshop_dept_name: row.dept_display(),
        start_date: row.start_date.to_string(),
        due_date: row.due_date.map(|date| date.to_string()),
        status: row.status.clone(),
        closed_at: row.closed_at.map(|at| at.to_rfc3339()),
        closed_by: row.closed_by.map(|id| id.to_string()),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        collect_summary: summary,
        period_items,
        next_statuses,
        legal_edges: edges,
        output_filled_by,
        output_filled_at,
    }
}

/// 按 id 组装详情（建 / 改 / 关批后的回单都走它）。
async fn detail_of(id: Uuid) -> std::result::Result<CostObjectDetailRes, ApiError> {
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象"))?
        .ok_or_else(|| not_found(id))?;
    let (mat, labor, oh) = model::collect_summary(id)
        .await
        .map_err(|error| utils::db_error(error, "汇总成本对象归集"))?;
    let items = model::period_items(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象计算单"))?;
    let edges = model::effective_edges().await;
    // 产量/在制的「最后填写人 + 时间」（§8.3 R-4）：读审计表，没有就是 null
    let fill = model::latest_output_fill(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象产量填写记录"))?;
    Ok(to_detail_res(
        &row,
        CollectSummary::of(mat, labor, oh),
        items,
        edges,
        fill,
    ))
}

/// 成本对象详情（HTTP 入口与真库用例共用的业务入口）。
pub async fn detail_object(id_raw: &str) -> std::result::Result<CostObjectDetailRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 新建成本对象。
pub async fn create_object(
    operator: Option<Uuid>,
    req: CostObjectCreateReq,
) -> std::result::Result<CostObjectDetailRes, ApiError> {
    let mut spec = cost_object_dto::spec_from_create(&req)?;
    ensure_refs(&mut spec).await?;
    let id = Uuid::now_v7();
    let now = Utc::now();
    let object_no = voucher::next_doc_no(db(), &OBJECT_SEQ, now)
        .await
        .map_err(|error| utils::db_error(error, "生成成本对象号"))?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建成本对象事务"))?;
    let result = async {
        model::insert(id, &object_no, &spec, operator)
            .await
            .map_err(|error| utils::db_error(error, "新建成本对象"))?;
        // 填了批号 ⇒ 自动建批次主数据（§6.2.1「填了自动建 biz_lot，source=PRODUCTION」）
        if let Some(lot_no) = spec.lot_no.as_deref() {
            model::ensure_production_lot(lot_no, spec.product_id, spec.source_doc_no.as_deref(), operator)
                .await
                .map_err(|error| utils::db_error(error, "建成本对象的产出批次"))?;
        }
        // 建单留痕（from_status = NULL）：之后任何一次状态变更都能对上「谁建的」
        state_machine::log_created(
            &tx,
            state_machine::ENTITY_COST_OBJECT,
            id,
            &object_no,
            cost_object_dto::STATUS_OPEN,
            operator,
            Some("新建成本对象（生产批）"),
        )
        .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    result?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建成本对象"))?;
    detail_of(id).await
}

/// 编辑成本对象（部分更新；`OPEN` 才可写，`product_id` 有归集后只读）。
pub async fn update_object(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostObjectUpdateReq,
) -> std::result::Result<CostObjectDetailRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的成本对象"))?
        .ok_or_else(|| not_found(id))?;
    if row.status != cost_object_dto::STATUS_OPEN {
        return Err(cost_object_dto::conflict(format!(
            "成本对象 {} 当前是「{}」：只有「在产」的对象能改（已关批要复工请新建批，\
             设计要求 §4.1 明确 CLOSED 不可回 OPEN）",
            row.object_no, row.status
        )));
    }
    let mut spec = cost_object_dto::spec_from_update(&row.to_spec(), &req)?;
    // `product_id` 有归集后只读：改了产出品，已归集的材料成本就落到了另一个产品头上
    if spec.product_id != row.product_id {
        let collected = model::collect_row_count(id)
            .await
            .map_err(|error| utils::db_error(error, "统计成本对象的归集行"))?;
        if collected > 0 {
            return Err(cost_object_dto::conflict(format!(
                "成本对象 {} 已有 {collected} 行归集，产出品不能再改（改了等于把已归集的成本挪到另一个产品）：\
                 请作废重建",
                row.object_no
            )));
        }
    }
    ensure_refs(&mut spec).await?;
    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| utils::db_error(error, "编辑成本对象"))?;
    if affected == 0 {
        return Err(cost_object_dto::conflict(format!(
            "成本对象 {} 的状态在本次编辑期间被改动（可能已被关批），请刷新后重试",
            row.object_no
        )));
    }
    // 改单时补建批号（原本没填，现在填了）
    if let Some(lot_no) = spec.lot_no.as_deref()
        && row.lot_no.as_deref() != Some(lot_no)
    {
        model::ensure_production_lot(lot_no, spec.product_id, spec.source_doc_no.as_deref(), operator)
            .await
            .map_err(|error| utils::db_error(error, "补建成本对象的产出批次"))?;
    }
    detail_of(id).await
}

/// 关批（5 条前置 §4.1；带拦截一律 409 + `66055`）。
pub async fn close_object(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostObjectCloseReq,
) -> std::result::Result<CostObjectDetailRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待关批的成本对象"))?
        .ok_or_else(|| not_found(id))?;
    if row.status != cost_object_dto::STATUS_OPEN {
        return Err(cost_object_dto::conflict(format!(
            "成本对象 {} 当前是「{}」：只有「在产」的对象能关批",
            row.object_no, row.status
        )));
    }
    // ---- 5 条前置：取事实 → 纯函数判定 ----
    let force_wip = req.force_wip.unwrap_or(false);
    let facts = model::close_facts(id, row.wip_qty, force_wip)
        .await
        .map_err(|error| utils::db_error(error, "采集关批前置事实"))?;
    let blockers = cost_object_dto::evaluate_close(&facts);
    if !blockers.is_empty() {
        let detail = blockers
            .iter()
            .map(|item| format!("[{}] {}", item.code, item.message))
            .collect::<Vec<_>>()
            .join("；");
        let mut error = ApiError::new(
            409,
            cost_object_dto::BLOCKED_CODE,
            format!("成本对象 {} 不能关批：{detail}", row.object_no),
        );
        // 结构化明细只进日志 / 单测断言，前端按 `msg` 原样展示即可
        error.internal_detail = Some(serde_json::to_string(&blockers).unwrap_or_else(|_| "[]".to_string()));
        return Err(error);
    }
    let remark = cost_object_dto::clean_optional(req.remark.clone());
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启关批事务"))?;
    let result = async {
        // ① 状态机：OPEN → CLOSED（边表核对 + 写 biz_status_log，reason = 关批备注）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_COST_OBJECT,
            id,
            &row.object_no,
            &model::LEGAL_EDGES,
            cost_object_dto::STATUS_OPEN,
            cost_object_dto::STATUS_CLOSED,
            operator,
            remark.as_deref().or(Some("关批")),
            None,
        )
        .await?;
        // ② 状态与关批痕迹（同事务：CAS `WHERE status='OPEN'`；库层 ck_..._closed 也要求 closed_at）
        let affected = model::apply_close(id, operator, remark.as_deref(), now)
            .await
            .map_err(|error| utils::db_error(error, "写成本对象关批状态"))?;
        if affected == 0 {
            return Err(cost_object_dto::conflict(format!(
                "成本对象 {} 的状态在本次关批期间被改动，请刷新后重试",
                row.object_no
            )));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    result?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交关批事务"))?;
    detail_of(id).await
}

/// 产量录入的回单组装（读**库里的值**，不回显请求里的数字）。
fn to_output_res(
    row: &CostObjectRow,
    warnings: Vec<String>,
    changed: bool,
    fill: Option<(Option<String>, DateTime<Utc>)>,
) -> CostObjectOutputRes {
    let (filled_by, filled_at) = match fill {
        Some((by, at)) => (by, Some(at.to_rfc3339())),
        None => (None, None),
    };
    CostObjectOutputRes {
        id: row.id.to_string(),
        object_no: row.object_no.clone(),
        plan_qty: row.plan_qty.to_string(),
        completed_qty: row.completed_qty.to_string(),
        wip_qty: row.wip_qty.to_string(),
        completion_pct: row.completion_pct.to_string(),
        changed,
        warnings,
        filled_by,
        filled_at,
    }
}

/// 填写「本期产量与在制」（§4.5.1 产量/进度行 · §8.3 R-4）。
///
/// # 校验顺序（每一条都对应一个用户动作）
///
/// ```text
/// 404  对象不存在 / 已软删
/// 409  对象不是 OPEN（CLOSED / CANCELLED 只读；要复工请新建批）
/// 400  三个数的值域（负数 / 进度越界 / 小数位超列 / 在制 > 0 但进度 0）
/// 409  该对象参与过的期间已结算（+ 66070，报文点名期间）→ 先红冲再改
/// ```
///
/// # 为什么「没变化」不写库也不写审计
///
/// 用户点两次保存、或面板自动带出原值再保存，三个数完全相同时：
/// 写 `updated_at` 会让「最后修改时间」骗人，写一条空 diff 会让审计表变成操作流水
/// （`changelog` 自己的口径也是 `Skipped`）。这里直接回 `changed=false` +
/// 上次填写人与时间，前端提示「没有改动」。
///
/// # 事务与补偿（AGENTS 约定 11）
///
/// 提交前没有「可能失败的外部动作」：一条 UPDATE + 一条审计 INSERT 同事务，
/// 任一失败整笔回滚（对象仍是旧数字，用户能立刻重试）。提交后只做**只读回查**，
/// 所以不存在「提交后失败留死单」的形态。
pub async fn set_output(
    ctx: &changelog::ChangeCtx<'_>,
    id_raw: &str,
    req: CostObjectOutputReq,
) -> std::result::Result<CostObjectOutputRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待填产量的成本对象"))?
        .ok_or_else(|| not_found(id))?;
    if row.status != cost_object_dto::STATUS_OPEN {
        return Err(cost_object_dto::conflict(format!(
            "成本对象 {} 当前是「{}」：只有「在产」的对象能填本期产量与在制\
             （已关批的批成本已经结转，要复工请新建批）",
            row.object_no, row.status
        )));
    }
    let current = OutputSpec::new(row.completed_qty, row.wip_qty, row.completion_pct);
    let spec = cost_object_dto::spec_from_output(&current, &req)?;

    // ---- 期间已结算 ⇒ 只读（卡面口径：SETTLED / CLOSED 且该对象有计算单）----
    let locked = model::settled_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象的已结算期间"))?;
    if !locked.is_empty() {
        let list = locked
            .iter()
            .map(|(period, status)| format!("{period}({status})"))
            .collect::<Vec<_>>()
            .join("、");
        return Err(ApiError::new(
            409,
            cost_object_dto::PERIOD_LOCKED_CODE,
            format!(
                "成本对象 {} 的本期产量与在制已锁定：以下期间的成本批次已结算（{list}），\
                 这三个数已随结算变成完工入库金额与结转凭证，再改就是改写已入账成本。\
                 如需更正，请先红冲该期间的成本批次，再回来改数字",
                row.object_no
            ),
        ));
    }

    // ---- 归集总额只用于「只提示不拦」的两条提醒 ----
    let (mat, labor, oh) = model::collect_summary(id)
        .await
        .map_err(|error| utils::db_error(error, "汇总成本对象归集（产量提醒）"))?;
    let warnings = cost_object_dto::output_warnings(&spec, row.plan_qty, mat + labor + oh);

    // ---- 三个数都没变：不写库、不写审计（不留空 diff 的假记录）----
    if spec == current {
        let fill = model::latest_output_fill(id)
            .await
            .map_err(|error| utils::db_error(error, "查询成本对象产量填写记录"))?;
        return Ok(to_output_res(&row, warnings, false, fill));
    }

    let before = current.to_snapshot();
    let after = spec.to_snapshot();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启产量录入事务"))?;
    let result = async {
        // ① CAS 写三列（`WHERE status='OPEN'`：与并发关批抢跑时 affected = 0）
        let affected = model::apply_output(id, &spec, ctx.operator_id)
            .await
            .map_err(|error| utils::db_error(error, "写入本期产量与在制"))?;
        if affected == 0 {
            return Err(cost_object_dto::conflict(format!(
                "成本对象 {} 的状态在本次保存期间被改动（可能刚被关批），请刷新后重试",
                row.object_no
            )));
        }
        // ② 审计（§8.3 R-4「填写人 + 时间」）：与业务写同事务，写不进去就整笔回滚
        changelog::record_in(
            &tx,
            ctx,
            ChangeTarget::of(
                model::COST_OBJECT_TABLE,
                id.to_string(),
                ChangeAction::Update,
                cost_object_dto::OUTPUT_ACTION_LABEL,
            ),
            DiffInput::update(&before, &after),
        )
        .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    result?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交产量录入事务"))?;

    // ---- 回单：读库（不信内存）+ 审计里的填写人/时间 ----
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回查成本对象"))?
        .ok_or_else(|| not_found(id))?;
    let fill = model::latest_output_fill(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象产量填写记录"))?;
    Ok(to_output_res(&row, warnings, true, fill))
}

/// 列表分页（数据权限：按车间裁剪）。
pub async fn page_objects(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    query: CostObjectPageQuery,
) -> std::result::Result<PageData<Vec<CostObjectListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询成本对象"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &CostObjectPageQuery) -> std::result::Result<CostObjectFilter, ApiError> {
    let status = cost_object_dto::clean_optional(query.status.clone());
    if let Some(status) = status.as_deref() {
        let upper = status.to_ascii_uppercase();
        if !cost_object_dto::STATUSES.contains(&upper.as_str()) {
            return Err(cost_object_dto::bad_request(format!(
                "status 只认 {:?}（收到「{status}」）",
                cost_object_dto::STATUSES
            )));
        }
    }
    let (page, page_size) =
        crate::business::erp::model::normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostObjectFilter {
        keyword: cost_object_dto::clean_optional(query.keyword.clone()),
        status: status.map(|text| text.to_ascii_uppercase()),
        product_id: cost_object_dto::parse_opt_uuid(query.product_id.as_deref(), "product_id")?,
        workshop_dept_id: cost_object_dto::parse_opt_uuid(query.workshop_dept_id.as_deref(), "workshop_dept_id")?,
        start_from: cost_object_dto::parse_opt_date(query.start_from.as_deref(), "start_from")?,
        start_to: cost_object_dto::parse_opt_date(query.start_to.as_deref(), "start_to")?,
        page,
        page_size,
    })
}

// ==================== HTTP 入口 ====================

/// 成本对象分页。
#[nik::permission(
    path = "/business/erp/cost-object",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-object")]
pub async fn cost_object_page_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<CostObjectPageQuery>,
) -> ResResult<PageData<Vec<CostObjectListRes>>> {
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(page_objects(&principal, query).await?))
}

/// 成本对象详情。
#[nik::permission(
    path = "/business/erp/cost-object/{id}",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-object/{id}")]
pub async fn cost_object_detail_api(Path(id): Path<String>) -> ResResult<CostObjectDetailRes> {
    Ok(Res::ok(detail_object(&id).await?))
}

/// 新建成本对象。
#[nik::permission(
    path = "/business/erp/cost-object",
    method = "POST",
    permission = "business:erp:cost-object:create"
)]
#[nik::post("/business/erp/cost-object")]
pub async fn cost_object_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostObjectCreateReq>,
) -> ResResult<CostObjectDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_object(operator, req).await?))
}

/// 编辑成本对象。
#[nik::permission(
    path = "/business/erp/cost-object/{id}",
    method = "PUT",
    permission = "business:erp:cost-object:update"
)]
#[nik::put("/business/erp/cost-object/{id}")]
pub async fn cost_object_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostObjectUpdateReq>,
) -> ResResult<CostObjectDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_object(operator, &id, req).await?))
}

/// 关批。
#[nik::permission(
    path = "/business/erp/cost-object/{id}/close",
    method = "POST",
    permission = "business:erp:cost-object:close"
)]
#[nik::post("/business/erp/cost-object/{id}/close")]
pub async fn cost_object_close_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostObjectCloseReq>,
) -> ResResult<CostObjectDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(close_object(operator, &id, req).await?))
}

/// 填写本期产量与在制（B2-14 · §4.5.1 产量/进度行）。
///
/// 权限码**复用** `business:erp:cost-object:update`（见模块头注释：新开码要连带种
/// 权限/菜单，本卡无迁移）。
///
/// 失败口径：对象不存在 404；非 `OPEN` 或该对象参与过的期间已结算 409
/// （后者业务码 `66070`，报文点名期间）；数值非法 400（`10001`）；
/// 变更日志写不进去 ⇒ 整笔回滚并 500（接入缺陷，不静默丢审计）。
#[nik::permission(
    path = "/business/erp/cost-object/{id}/output",
    method = "PUT",
    permission = "business:erp:cost-object:update"
)]
#[nik::put("/business/erp/cost-object/{id}/output")]
pub async fn cost_object_output_api(
    headers: HeaderMap,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostObjectOutputReq>,
) -> ResResult<CostObjectOutputRes> {
    // 审计上下文（谁 / 哪个请求 / 哪个 IP）：`request_id` 能 join 到 operation_logs
    let audit = changelog::RequestCtx::from_request(&claims, &headers, None);
    let ctx = audit.ctx();
    Ok(Res::ok(set_output(&ctx, &id, req).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    COST_OBJECT_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost_object_dto::{CloseFacts, STATUS_CLOSED, STATUS_OPEN};

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-object:create");
        assert_eq!(PERM_UPDATE, "business:erp:cost-object:update");
        assert_eq!(PERM_DELETE, "business:erp:cost-object:delete");
        assert_eq!(PERM_CLOSE, "business:erp:cost-object:close");
        assert_eq!(all_permissions().len(), 5, "设计 §5.3 给了 5 个码");
        for code in COST_OBJECT_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "cost-object", "{code}");
        }
        // 与成本核算批次的码是两个不同资源（别把 settle 的码复用过来）
        assert!(COST_OBJECT_PERMISSIONS.iter().all(|code| code.contains("cost-object")));
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/cost-object",
            "/business/erp/cost-object/{id}",
            "/business/erp/cost-object/{id}/close",
        ] {
            assert!(path.starts_with("/business/erp/cost-object"));
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    #[test]
    fn object_no_spec_is_cb_daily() {
        assert_eq!(OBJECT_SEQ.seq_type, "COST_OBJECT");
        assert_eq!(OBJECT_SEQ.prefix, "CB");
        // 号段名与设计 §3.1 ② 的列注释一致
        assert!(OBJECT_SEQ.remark.contains("CB-yyyyMMdd-000001"));
    }

    // ---------- 列表筛选归一化 ----------

    #[test]
    fn filter_normalizes_status_and_paging() {
        let query = CostObjectPageQuery {
            status: Some(" open ".to_string()),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.status.as_deref(), Some(STATUS_OPEN), "状态大小写归一");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "page_size=0 回落到默认 20");

        let query = CostObjectPageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_unknown_status() {
        let query = CostObjectPageQuery {
            status: Some("FINISHED".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("未知状态");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("status"), "{}", error.msg);
    }

    #[test]
    fn filter_rejects_bad_date_and_uuid() {
        let query = CostObjectPageQuery {
            start_from: Some("2026-13-01".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect_err("非法日期").status, 400);

        let query = CostObjectPageQuery {
            product_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("非法 UUID");
        assert!(error.msg.contains("product_id"), "{}", error.msg);
    }

    // ---------- 详情出参：next_statuses 跟有效边走 ----------

    fn sample_row(status: &str) -> CostObjectRow {
        let now = Utc::now();
        CostObjectRow {
            id: Uuid::now_v7(),
            object_no: "CB-20261110-000001".to_string(),
            object_type: cost_object_dto::OBJECT_TYPE_BATCH.to_string(),
            product_id: Uuid::now_v7(),
            lot_no: Some("LOT-A".to_string()),
            target_warehouse_id: Uuid::now_v7(),
            target_location_id: None,
            plan_qty: "100".parse().expect("qty"),
            completed_qty: "80".parse().expect("qty"),
            wip_qty: "20".parse().expect("qty"),
            completion_pct: "50".parse().expect("pct"),
            material_input_mode: cost_object_dto::MODE_ONE_TIME.to_string(),
            source_doc_no: Some("SO-0001".to_string()),
            customer_id: None,
            workshop_dept_id: Uuid::now_v7(),
            workshop_dept_name: "一车间".to_string(),
            start_date: "2026-11-01".parse().expect("date"),
            due_date: None,
            status: status.to_string(),
            closed_at: None,
            closed_by: None,
            remark: None,
            created_at: now,
            updated_at: now,
            product_name: Some("钢轴".to_string()),
            product_code: Some("MAT-0001".to_string()),
            dept_name: Some("一车间".to_string()),
            warehouse_name: Some("成品仓".to_string()),
            location_name: None,
            customer_name: None,
            cost_total: "125.5000".parse().expect("amount"),
        }
    }

    #[test]
    fn detail_offers_both_edges_when_open() {
        let edges = vec![
            (STATUS_OPEN.to_string(), STATUS_CLOSED.to_string()),
            (STATUS_OPEN.to_string(), cost_object_dto::STATUS_CANCELLED.to_string()),
        ];
        let res = to_detail_res(
            &sample_row(STATUS_OPEN),
            CollectSummary::of("0".parse().unwrap(), "0".parse().unwrap(), "0".parse().unwrap()),
            Vec::new(),
            edges,
            None,
        );
        assert_eq!(
            res.next_statuses,
            vec![STATUS_CLOSED.to_string(), "CANCELLED".to_string()]
        );
        assert_eq!(res.legal_edges.len(), 2);
        // 出参字段（前端 C 卡逐字对齐）
        let json = serde_json::to_value(&res).expect("序列化");
        for field in [
            "id",
            "object_no",
            "product_id",
            "product_name",
            "workshop_dept_id",
            "workshop_dept_name",
            "plan_qty",
            "completed_qty",
            "wip_qty",
            "status",
            "collect_summary",
            "period_items",
            "next_statuses",
            "legal_edges",
        ] {
            assert!(json.get(field).is_some(), "详情缺字段 {field}");
        }
        assert_eq!(json["plan_qty"], serde_json::json!("100"), "数量是字符串");
    }

    #[test]
    fn closed_object_offers_no_edge() {
        let edges = vec![
            (STATUS_OPEN.to_string(), STATUS_CLOSED.to_string()),
            (STATUS_OPEN.to_string(), cost_object_dto::STATUS_CANCELLED.to_string()),
        ];
        let res = to_detail_res(
            &sample_row(STATUS_CLOSED),
            CollectSummary::of("0".parse().unwrap(), "0".parse().unwrap(), "0".parse().unwrap()),
            Vec::new(),
            edges,
            None,
        );
        assert!(res.next_statuses.is_empty(), "CLOSED 是终态，没有可去的状态");
        assert_eq!(res.legal_edges.len(), 2, "边表本身仍然如实返回");
    }

    #[test]
    fn list_row_serializes_frontend_fields() {
        let res = to_list_res(&sample_row(STATUS_OPEN));
        let json = serde_json::to_value(&res).expect("序列化");
        for field in [
            "id",
            "object_no",
            "object_type",
            "product_id",
            "product_name",
            "workshop_dept_id",
            "workshop_dept_name",
            "plan_qty",
            "completed_qty",
            "wip_qty",
            "status",
            "lot_no",
            "source_doc_no",
            "cost_total",
        ] {
            assert!(json.get(field).is_some(), "列表行缺字段 {field}（前端会 undefined）");
        }
        assert_eq!(json["cost_total"], serde_json::json!("125.5000"));
    }

    // ---------- 关批拦截的错误形状 ----------

    #[test]
    fn blocked_close_carries_66055_and_structured_detail() {
        let facts = CloseFacts {
            uncollected_stock_out: 2,
            ..Default::default()
        };
        let blockers = cost_object_dto::evaluate_close(&facts);
        assert_eq!(blockers.len(), 1);
        // handler 组装报文的口径（这里复刻一份，钉住「码 + 分类码」两件都在）
        let detail = blockers
            .iter()
            .map(|item| format!("[{}] {}", item.code, item.message))
            .collect::<Vec<_>>()
            .join("；");
        assert!(detail.contains(cost_object_dto::BLOCK_UNCOLLECTED_STOCK_OUT));
        assert!(detail.contains("归集"));
        assert_eq!(cost_object_dto::BLOCKED_CODE, error_code::ERP_COST_OBJECT_CLOSE_BLOCKED);
        let json = serde_json::to_value(&blockers).expect("序列化");
        assert_eq!(json[0]["code"], serde_json::json!("UNCOLLECTED_STOCK_OUT"));
    }

    // ---------- 本期产量与在制（B2-14，路由 #7） ----------

    /// 路由与权限码：`PUT /cost-object/{id}/output` 挂在 `update` 权限下。
    ///
    /// 权限码表仍是 5 个（新开 `output` 码要连带种权限/菜单 = 迁移，本卡无迁移）。
    #[test]
    fn output_route_reuses_update_permission() {
        let path = "/business/erp/cost-object/{id}/output";
        assert!(!path.starts_with("/api"), "约定 8：宏内路径不带 /api 前缀");
        assert!(path.ends_with("/output"));
        assert_eq!(all_permissions().len(), 5, "没有新开权限码");
        assert!(!COST_OBJECT_PERMISSIONS.contains(&"business:erp:cost-object:output"));
        // 与卡面「复用 update」一致：权限码字面量
        assert_eq!(PERM_UPDATE, "business:erp:cost-object:update");
    }

    /// 回单字段（前端面板逐字对齐：三个数 + 提示 + 填写人/时间）。
    #[test]
    fn output_res_serializes_panel_fields() {
        let now = Utc::now();
        let res = to_output_res(
            &sample_row(STATUS_OPEN),
            vec!["超产提示".to_string()],
            true,
            Some((Some("张三".to_string()), now)),
        );
        let json = serde_json::to_value(&res).expect("序列化");
        for field in [
            "id",
            "object_no",
            "plan_qty",
            "completed_qty",
            "wip_qty",
            "completion_pct",
            "changed",
            "warnings",
            "filled_by",
            "filled_at",
        ] {
            assert!(json.get(field).is_some(), "回单缺字段 {field}（前端会 undefined）");
        }
        assert_eq!(json["plan_qty"], serde_json::json!("100"), "数量是字符串");
        assert_eq!(json["changed"], serde_json::json!(true));
        assert_eq!(json["filled_by"], serde_json::json!("张三"));
        assert!(json["filled_at"].as_str().unwrap_or("").contains('T'), "时间要 RFC3339");
        assert_eq!(json["warnings"][0], serde_json::json!("超产提示"));
    }

    /// 详情带「最后填写人 + 时间」（§8.3 R-4 的界面落点）；没填过就是 null。
    #[test]
    fn detail_carries_output_fill_snapshot() {
        let edges = vec![(STATUS_OPEN.to_string(), STATUS_CLOSED.to_string())];
        let zero = CollectSummary::of("0".parse().unwrap(), "0".parse().unwrap(), "0".parse().unwrap());
        let filled_at = Utc::now();
        let res = to_detail_res(
            &sample_row(STATUS_OPEN),
            zero.clone(),
            Vec::new(),
            edges.clone(),
            Some((Some("李四".to_string()), filled_at)),
        );
        assert_eq!(res.output_filled_by.as_deref(), Some("李四"));
        assert_eq!(res.output_filled_at.as_deref(), Some(filled_at.to_rfc3339().as_str()));

        let empty = to_detail_res(&sample_row(STATUS_OPEN), zero, Vec::new(), edges, None);
        assert!(empty.output_filled_by.is_none(), "没填过就是 null（不造「未知」）");
        assert!(empty.output_filled_at.is_none());
    }
}
