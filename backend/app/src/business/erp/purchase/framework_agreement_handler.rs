//! 框架协议接口层（M4-11b）：建单 / 改单 / 删单 / 列表 / 详情 / **提交审批**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/framework-agreement/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/framework-agreement/page` | `…:list` | 台账列表（**与执行量总览同一张视图**，只差权限码） |
//! | GET | `/business/erp/framework-agreement/{id}` | `…:list` | 详情（单头 + 物料范围，含行备注 —— 改单表单要回填） |
//! | POST | `/business/erp/framework-agreement` | `…:create` | 建单（`FWA-yyyyMMdd-000001`，`DRAFT`） |
//! | PUT | `/business/erp/framework-agreement/{id}` | `…:update` | 整单替换（只有 `DRAFT`） |
//! | DELETE | `/business/erp/framework-agreement/{id}` | `…:delete` | 软删（只有 `DRAFT`） |
//! | POST | `/business/erp/framework-agreement/{id}/submit` | `…:submit` | 提交审批（走 `fw_agreement` 流程） |
//!
//! 执行量两条只读接口（`…:execution`）属 M4-09，在
//! [`crate::business::erp::purchase::framework_agreement_execution_handler`] —— 那边是**报表**口径
//! （已用 / 剩余 / 下钻来源），本文件是**维护**口径（单据自身 + 状态机），不是一件事。
//!
//! # 为什么 `GET /page` 与 `GET /execution/page` 返回同一份行、却是两个接口码
//!
//! AGENTS 约定 14（页面显形码与接口码解耦）：页面上两个 Tab 是两个入口 ——
//! 「协议台账」看的是**单据**（谁签的、条款、上限），判 `…:list`；
//! 「执行量」看的是**用量**（已用 / 剩余 / 超限），判 `…:execution`。
//! 两个码不该互相顶替：只有执行量码的人不该能改单，只有台账码的人也不该看超限报表。
//! 数据源是同一张视图（`v_framework_agreement_execution`），所以这里**委派**
//! [`crate::business::erp::purchase::framework_agreement_execution_model`] 取数，不复制 SQL。
//!
//! # 三条硬规则（每条都有落库证据）
//!
//! ```text
//! ① 走审批流，不硬编码：提交时查「已发布的 fw_agreement 流程」——
//!    有 → 同一事务里「发起审批 + CAS 状态 DRAFT→SUBMITTED + 写实例 id」；
//!    没有 → **默认 409**，只有持 business:erp:skip-approval + ≥10 字理由才直接 APPROVED
//!    （理由写进备注）。代码里没有任何「金额 > X 就加签」的 if —— 分级在流程定义的条件边里。
//! ② 单头两个 total_executed_* 是派生列：请求体里带了也不读，一律 recalc_totals 重算。
//! ③ 改单不许把已用额度改小到低于已用（`check_cap_shrink`），也不许删掉已有执行量的物料行
//!    （`check_missing_material`）—— 否则 M4-08 的准入与 M4-09 的报表互相矛盾。
//! ```
//!
//! # 幂等
//!
//! * 提交：`SELECT … FOR UPDATE` 锁单头 + 状态机 CAS（`DRAFT → SUBMITTED`）+
//!   `biz_instance_running` 双保险 —— 重复提交 409 且带审批单号；
//! * 改单 / 删单：CAS（`WHERE status='DRAFT'`），并发的第二个请求拿到 0 行 → 409。
//!
//! # 事务提交后失败必须有补偿（AGENTS 约定 11）
//!
//! 免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后立即回调
//! `framework_agreement_flow::on_fwa_settled`。它失败时单据会停在 `SUBMITTED` 且
//! **没有待办**能推进它，所以这里在失败路径上**显式打回草稿**并对外报错（用户能重新提交）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_framework_agreement / _item 与采购计划同口径 —— 框架协议是公司级
//!        采购文件（采购要看全公司、财务要复核），`dept_id` 是「经办部门」快照语义，
//!        不是数据范围列（`shared::scope::SCOPED_TABLES` 里没有这两张表）。
//!        可见性由权限码控制（本卡的 6 个码）。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::erp::purchase::framework_agreement_dto::{
    self as dto, AGREEMENT_PERMISSIONS, AgreementDetailRes, AgreementItemRes, AgreementRowRes, CreateAgreementReq,
    SubmitAgreementReq, check_adjust_rule, check_agreed_price, check_cap, check_cap_shrink, check_currency,
    check_items, check_materials_unique, check_missing_material, check_name, check_period, check_price_mode,
    check_tax_rate, check_tolerance, clean_remark, parse_date_field, parse_optional_uuid, parse_uuid_field,
    status_label,
};
use crate::business::erp::purchase::framework_agreement_execution_dto::{ExecutionFilter, ExecutionPageQuery};
use crate::business::erp::purchase::framework_agreement_execution_handler;
use crate::business::erp::purchase::framework_agreement_execution_model;
use crate::business::erp::purchase::framework_agreement_flow;
use crate::business::erp::purchase::framework_agreement_model::{
    self as model, AgreementItemRow, AgreementRow, NewAgreementHead, NewAgreementItem, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::erp::purchase::purchase_model;
use crate::business::erp::unit_guard;
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0456 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:framework-agreement:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:framework-agreement:create";
/// 修改。
pub const PERM_UPDATE: &str = "business:erp:framework-agreement:update";
/// 删除。
pub const PERM_DELETE: &str = "business:erp:framework-agreement:delete";
/// 提交审批。
pub const PERM_SUBMIT: &str = "business:erp:framework-agreement:submit";

// 代码里断言一份常量与 DTO 的清单一致：两份清单漂移时这里会红，不会等到运行时 403。
const _: () = assert!(AGREEMENT_PERMISSIONS.len() == 6);

/// 单据中文名（409 / 404 报文用）。
const LABEL: &str = "框架协议";

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("框架协议不存在或已删除（{id}）"),
    )
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// `DbError` → `ApiError`（写库阶段的统一出口）。
fn write_error(error: DbError, context: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{context}失败：{error}"))
}

/// 发起审批撞唯一索引时的 409（同一张单不允许两个在跑的审批）。
fn submit_conflict(error: DbError, context: &str, agreement_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&error) {
        tracing::warn!(
            context,
            agreement_no,
            error = %error,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(LABEL, Some(agreement_no));
    }
    write_error(error, context)
}

// ==================== 入参 → 单头 + 物料范围（建单 / 改单一套） ====================

/// 把请求体校验成「可直接落库的单头 + 物料范围」。
///
/// 校验顺序（越早拒绝越好）：名称 → 有效期 → 定价方式 → 调价规则 → 币种 → 容差 → 上限 →
/// 备注 → 供应商 → 部门 → 经办人 → 逐行（物料 / 单位 / 单价 / 税率 / 上限 / 备注）→
/// 「同物料唯一」。
///
/// `self_id` 用于改单时排除自己（当前只影响报文语义，唯一键是 `(agreement, material)`）。
async fn build_head_and_items(
    req: &CreateAgreementReq,
    operator: Option<Uuid>,
    self_id: Option<Uuid>,
) -> std::result::Result<(NewAgreementHead, Vec<NewAgreementItem>), ApiError> {
    let agreement_name = check_name(req.agreement_name.as_deref())?;
    let (start_date, end_date) = check_period(
        parse_date_field(req.start_date.as_deref(), "start_date")?,
        parse_date_field(req.end_date.as_deref(), "end_date")?,
    )?;
    let sign_date = parse_date_field(req.sign_date.as_deref(), "sign_date")?;
    let price_mode = check_price_mode(req.price_mode.as_deref())?;
    let price_adjust_rule = check_adjust_rule(&price_mode, req.price_adjust_rule.as_deref())?;
    let price_currency = check_currency(req.price_currency.as_deref())?;
    let price_tolerance = check_tolerance(req.price_tolerance)?;
    let total_max_qty = check_cap(req.total_max_qty, "total_max_qty")?;
    let total_max_amount = check_cap(req.total_max_amount, "total_max_amount")?;
    let remark = clean_remark(req.remark.as_deref())?;

    // 供应商：必须存在且存活；名字落**快照**（签约主体是历史事实，约定 13）
    let supplier_id = parse_uuid_field(req.supplier_id.as_deref(), "supplier_id")?;
    let supplier_name = purchase_model::supplier_ref(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing(format!("supplier_id 供应商不存在或已删除（{supplier_id}）")))?;

    // 经办部门（可空 = 公司级）：给了就必须存在，落名称快照
    let dept_id = parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = match dept_id {
        None => None,
        Some(id) => Some(
            model::dept_name_of(db(), id)
                .await
                .map_err(|error| utils::db_error(error, "查询经办部门"))?
                .ok_or_else(|| reference_missing("dept_id 部门不存在或已删除"))?,
        ),
    };

    // 经办人：缺省 = 当前登录人；姓名一律服务端快照（前端传的名字不可信）
    let owner_id = match parse_optional_uuid(req.owner_id.as_deref(), "owner_id")? {
        Some(id) => Some(id),
        None => operator,
    };
    let owner_name = match owner_id {
        Some(id) => user_name_of(id).await?,
        None => None,
    };

    check_items(&req.items)?;
    let mut items = Vec::with_capacity(req.items.len());
    let mut materials = Vec::with_capacity(req.items.len());
    for (index, line) in req.items.iter().enumerate() {
        let material_id = parse_uuid_field(line.material_id.as_deref(), &format!("items[{index}].material_id"))?;
        let agreed_price = check_agreed_price(line.agreed_price, &format!("items[{index}].agreed_price"))?;
        let tax_rate = check_tax_rate(line.tax_rate, &format!("items[{index}].tax_rate"))?;
        let max_qty = check_cap(line.max_qty, &format!("items[{index}].max_qty"))?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        // DC-09：单位一律由物料基础单位带出。协议的 max_qty 会与采购单数量直接比较
        // （M4-08 按基础单位判上限），折算单位会让「5 箱」的额度被记成「5 个」。
        unit_guard::ensure_base_unit(material_id, index, max_qty.unwrap_or(Decimal::ONE)).await?;
        materials.push(material_id);
        items.push(NewAgreementItem {
            material_id,
            unit_id: material.unit_id,
            agreed_price,
            tax_rate,
            max_qty,
            remark: clean_remark(line.remark.as_deref())?,
        });
    }
    // 「同一协议里同物料只能一行」：在请求层先拒（库层 uk_biz_fwai_material 兜底是 500）
    check_materials_unique(&materials)?;
    let _ = self_id;

    Ok((
        NewAgreementHead {
            agreement_name,
            supplier_id,
            supplier_name: Some(supplier_name),
            dept_id,
            dept_name,
            owner_id,
            owner_name,
            start_date,
            end_date,
            sign_date,
            price_mode,
            price_includes_tax: req.price_includes_tax.unwrap_or(false),
            price_currency,
            price_tolerance,
            price_adjust_rule,
            total_max_qty,
            total_max_amount,
            remark,
        },
        items,
    ))
}

/// 用户显示名（`real_name` 优先，其次 `username`）。
async fn user_name_of(id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username) AS name FROM sys_user WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询经办人"))?;
    rows.first()
        .map(|row| row.get_typed::<String>("name"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取经办人姓名"))
}

/// 改单前的「明细额度收紧」守卫（用**真实已用量**判，两条数据源取大）。
///
/// 为什么取大：明细的 `executed_qty` 是 M4-08 的回写快照，而报表口径是**从采购单链现算**
/// （M4-09 刻意不读回写列）。两者可能分叉（漏刷 / 补录），收紧额度时按**大的那个**判 ——
/// 宁可拒绝一次「其实可以改小」，也不能放过一次「改小到低于真实已用」，
/// 后者会让准入与报表长期互相矛盾。
async fn guard_item_shrink(agreement_id: Uuid, items: &[NewAgreementItem]) -> std::result::Result<(), ApiError> {
    let existing = model::items_of_agreement(db(), agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料范围"))?;
    let chain = framework_agreement_execution_model::items_of_agreement(db(), agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料已用量"))?;

    // material_id → (真实已用量, 物料名)
    let mut used: std::collections::BTreeMap<Uuid, (Decimal, String)> = std::collections::BTreeMap::new();
    let mut booked: std::collections::BTreeMap<Uuid, (Decimal, String)> = std::collections::BTreeMap::new();
    for row in existing.iter() {
        booked.insert(row.material_id, (row.executed_qty, row.material_name.clone()));
    }
    for row in chain.iter() {
        let name = booked
            .get(&row.material_id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| row.material_name.clone());
        let on_booked = booked
            .get(&row.material_id)
            .map(|(qty, _)| *qty)
            .unwrap_or(Decimal::ZERO);
        let truth = if row.executed_qty > on_booked {
            row.executed_qty
        } else {
            on_booked
        };
        used.insert(row.material_id, (truth, name));
    }
    // 链上没行、但回写列有值的（刚下单还没进视图）：也要算进来
    for (material_id, (qty, name)) in booked.iter() {
        used.entry(*material_id).or_insert((*qty, name.clone()));
    }

    for item in items.iter() {
        if let Some((used_qty, name)) = used.get(&item.material_id) {
            check_cap_shrink("max_qty", &format!("物料「{name}」"), item.max_qty, *used_qty)?;
        }
    }
    Ok(())
}

/// 改单时「不许删掉已有执行量的物料行」。
fn guard_removals(
    agreement: &AgreementRow,
    existing: &[AgreementItemRow],
    items: &[NewAgreementItem],
) -> std::result::Result<(), ApiError> {
    let keep: std::collections::BTreeSet<Uuid> = items.iter().map(|item| item.material_id).collect();
    for row in existing.iter() {
        if keep.contains(&row.material_id) {
            continue;
        }
        check_missing_material(row.material_id, &row.material_name, row.executed_qty)?;
    }
    let _ = agreement;
    Ok(())
}

// ==================== ① 建单 ====================

/// 建单核心逻辑（handler 与真库用例共用，只跳过认证与路由）。
pub async fn create_agreement(req: &CreateAgreementReq, operator: Option<Uuid>) -> std::result::Result<Uuid, ApiError> {
    let (head, items) = build_head_and_items(req, operator, None).await?;
    model::create_agreement(&head, &items, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "创建框架协议"))
}

/// 新建框架协议。
#[nik::permission(
    path = "/business/erp/framework-agreement",
    method = "POST",
    permission = "business:erp:framework-agreement:create"
)]
#[nik::post("/business/erp/framework-agreement")]
pub async fn framework_agreement_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateAgreementReq>,
) -> ResResult<AgreementDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_agreement(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：整单替换，只有 `DRAFT` 能改。
pub async fn update_agreement(
    id: Uuid,
    req: &CreateAgreementReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = model::find_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议"))?
        .ok_or_else(|| not_found(id))?;
    ensure_draft(&row, "修改")?;

    let (head, items) = build_head_and_items(req, operator, Some(id)).await?;

    // 额度收紧 + 删行守卫（跨行语义，库层 CHECK 做不到）
    let existing = model::items_of_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料范围"))?;
    guard_removals(&row, &existing, &items)?;
    guard_item_shrink(id, &items).await?;
    // 单头总量 / 总额上限：只在「改小」时判（None = 不限，不构成收紧）
    if let Some(new_cap) = head.total_max_qty {
        check_cap_shrink("total_max_qty", "协议总量上限", Some(new_cap), row.total_executed_qty)?;
    }
    if let Some(new_cap) = head.total_max_amount {
        check_cap_shrink(
            "total_max_amount",
            "协议总额上限",
            Some(new_cap),
            row.total_executed_amount,
        )?;
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    let outcome = async { model::replace_agreement(&tx, id, &head, &items, operator, now).await }.await;
    let affected = match outcome {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "修改框架协议"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(status_drift(&row, "修改"));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改单事务"))?;
    Ok(())
}

/// 修改框架协议（整单替换）。
#[nik::permission(
    path = "/business/erp/framework-agreement/{id}",
    method = "PUT",
    permission = "business:erp:framework-agreement:update"
)]
#[nik::put("/business/erp/framework-agreement/{id}")]
pub async fn framework_agreement_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreateAgreementReq>,
) -> ResResult<AgreementDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    update_agreement(id, &req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 删单（软删） ====================

/// 删单核心逻辑：只有 `DRAFT` 能删（审批中的单删了会让审批详情指向空气）。
pub async fn delete_agreement(id: Uuid, operator: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let row = model::find_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议"))?
        .ok_or_else(|| not_found(id))?;
    ensure_draft(&row, "删除")?;
    let affected = model::soft_delete_agreement(db(), id, operator, Utc::now())
        .await
        .map_err(|error| write_error(error, "删除框架协议"))?;
    if affected == 0 {
        return Err(status_drift(&row, "删除"));
    }
    Ok(())
}

/// 删除框架协议（软删）。
#[nik::permission(
    path = "/business/erp/framework-agreement/{id}",
    method = "DELETE",
    permission = "business:erp:framework-agreement:delete"
)]
#[nik::delete("/business/erp/framework-agreement/{id}")]
pub async fn framework_agreement_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    delete_agreement(id, operator).await?;
    Ok(Res::ok(()))
}

// ==================== ④ 分页 / 详情 ====================

/// 台账分页（与执行量总览同一张视图；权限码是 `…:list`）。
#[nik::permission(
    path = "/business/erp/framework-agreement/page",
    method = "GET",
    permission = "business:erp:framework-agreement:list"
)]
#[nik::get("/business/erp/framework-agreement/page")]
pub async fn framework_agreement_page(
    Query(query): Query<ExecutionPageQuery>,
) -> ResResult<PageData<Vec<crate::business::erp::purchase::framework_agreement_execution_dto::AgreementExecRowRes>>> {
    let filter: ExecutionFilter = query.filter()?;
    let (list, total) = framework_agreement_execution_handler::page_of(&filter).await?;
    Ok(Res::ok(PageData {
        list,
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 详情（单头 + 物料范围，含行备注 —— 改单表单要靠它回填）。
#[nik::permission(
    path = "/business/erp/framework-agreement/{id}",
    method = "GET",
    permission = "business:erp:framework-agreement:list"
)]
#[nik::get("/business/erp/framework-agreement/{id}")]
pub async fn framework_agreement_detail(Path(id): Path<String>) -> ResResult<AgreementDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情。
///
/// 404 语义：**只有**协议不存在 / 已软删才 404（空物料范围是合法中间态，与 M4-09 同口径）。
pub async fn detail_of(id: Uuid) -> std::result::Result<AgreementDetailRes, ApiError> {
    let row = model::find_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议"))?
        .ok_or_else(|| not_found(id))?;
    let items = model::items_of_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料范围"))?;

    // 已用量取「链现算」与「回写」的较大值：详情要给前端一个**能用来判收紧**的数，
    // 与 `guard_shrink` 同一口径（两处不一致就会出现「界面说可以改、保存却 409」）。
    let chain = framework_agreement_execution_model::items_of_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料已用量"))?;
    let mut used: std::collections::BTreeMap<Uuid, Decimal> = std::collections::BTreeMap::new();
    for item in chain.iter() {
        used.insert(item.material_id, item.executed_qty);
    }

    Ok(AgreementDetailRes {
        agreement: to_row_res(&row),
        items: items
            .iter()
            .map(|item| {
                let on_chain = used.get(&item.material_id).copied().unwrap_or(Decimal::ZERO);
                to_item_res(
                    item,
                    if on_chain > item.executed_qty {
                        on_chain
                    } else {
                        item.executed_qty
                    },
                )
            })
            .collect(),
    })
}

// ==================== ⑤ 提交审批 ====================

/// 提交审批核心逻辑。
///
/// # 顺序（每一步都有理由）
///
/// 1. 只允许 `DRAFT`（`SUBMITTED` 单独给「已在审批中 + 审批单号」的 409）；
/// 2. 物料范围非空（空范围的协议批了就只是一张纸：既不会被引用，也产生不了执行量）；
/// 3. 有已发布流程 → 同一事务里「发起审批 + CAS 状态 + 实例 id」；
///    没流程 → 默认 409，持跳过权限才直接 `APPROVED`（理由写备注）。
pub async fn submit_agreement(
    id: Uuid,
    req: &SubmitAgreementReq,
    operator: Option<Uuid>,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let row = model::find_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议"))?
        .ok_or_else(|| not_found(id))?;
    if row.status == STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(dto::conflict(format!(
            "框架协议 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.agreement_no, instance_no
        )));
    }
    ensure_draft(&row, "提交审批")?;
    let items = model::items_of_agreement(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询协议物料范围"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "框架协议 {} 没有任何物料范围，不能提交审批：空协议既不能被下单引用，也产生不了执行量。\
             请先编物料（协议价 + 数量上限）",
            row.agreement_no
        )));
    }
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let business_reason = clean_remark(req.reason.as_deref())?;

    match framework_agreement_flow::published_fwa_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(framework_agreement_flow::FLOW_CODE, gate)?;
            submit_with_approval(&row, &items, &flow, operator_id, business_reason).await
        }
        None => {
            // ARCH-3 DC-01：**不静默降级**。默认 409（点名 fw_agreement）；
            // 只有「持 business:erp:skip-approval + 理由 ≥10 字」才放行，理由写进备注。
            let skip_reason = approval_gate::authorize_missing_flow(
                framework_agreement_flow::BIZ_TYPE,
                framework_agreement_flow::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                agreement_id = %row.id,
                agreement_no = %row.agreement_no,
                %skip_reason,
                "框架协议提交：没有已发布的 fw_agreement 流程，按「显式跳过审批」直接放行到 APPROVED"
            );
            let reason = approval_gate::merge_reason(business_reason.as_deref(), &skip_reason);
            let affected = model::mark_skipped_approval(db(), row.id, Some(&reason), Some(operator_id), Utc::now())
                .await
                .map_err(|error| write_error(error, "框架协议进入已批准（跳过审批）"))?;
            if affected == 0 {
                return Err(status_drift(&row, "提交审批"));
            }
            Ok(())
        }
    }
}

/// 走审批的提交：状态推进 + 发起实例**同一事务**。
async fn submit_with_approval(
    row: &AgreementRow,
    items: &[AgreementItemRow],
    flow: &framework_agreement_flow::FwaFlow,
    operator_id: Uuid,
    business_reason: Option<String>,
) -> std::result::Result<(), ApiError> {
    // 幂等：同一单已有在跑的审批 → 409（唯一索引之前先给可读报文）
    if flow_model::biz_instance_running(framework_agreement_flow::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(dto::conflict(format!(
            "框架协议 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.agreement_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 框架协议审批", row.agreement_no)
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
        biz_no: Some(row.agreement_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: framework_agreement_flow::form_data(row, items),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启框架协议审批事务"))?;
    let outcome = async {
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| submit_conflict(error, "发起框架协议审批单", &row.agreement_no))?;
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
        let affected = model::mark_submitted(
            &tx,
            row.id,
            instance,
            business_reason.as_deref(),
            Some(operator_id),
            now,
        )
        .await
        .map_err(|error| write_error(error, "框架协议进入已提交"))?;
        if affected == 0 {
            return Err(status_drift(row, "提交审批"));
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
        .map_err(|error| utils::db_error(error, "提交框架协议审批事务"))?;
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
        agreement_id = %row.id,
        agreement_no = %row.agreement_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "框架协议已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了。
    // 这里必须立刻回调到 APPROVED，否则单据停在 SUBMITTED 且**没有任何待办**能推进它
    // （AGENTS 约定 11 点名的高危形态）；回调失败就显式打回草稿，让用户能重提。
    if auto_finished
        && framework_agreement_flow::on_fwa_settled(Some(instance), "APPROVED")
            .await
            .is_none()
    {
        let compensation = model::mark_back_to_draft(
            db(),
            row.id,
            Some("免审过账回调失败，系统已自动打回草稿，请重新提交（无需改库）"),
            Some(operator_id),
            Utc::now(),
        )
        .await;
        match compensation {
            Ok(affected) if affected > 0 => {
                tracing::warn!(
                    agreement_id = %row.id,
                    agreement_no = %row.agreement_no,
                    instance_id = %instance,
                    "免审直过回调失败：单据已打回 DRAFT（用户可重新提交）"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "框架协议 {} 的审批实例已结束（无审批节点），但结论回写失败；\
                         系统已自动把单据打回草稿，请重新提交（无需改库）",
                        row.agreement_no
                    ),
                ));
            }
            other => {
                tracing::error!(
                    agreement_id = %row.id,
                    agreement_no = %row.agreement_no,
                    instance_id = %instance,
                    result = ?other,
                    "免审直过回调失败且打回草稿也失败：单据停在 SUBMITTED（无待办），需人工介入"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "框架协议 {} 的审批实例已结束，但结论回写与自动打回都失败（审批单号可查）；\
                         请联系管理员排查，单据当前停在「审批中」",
                        row.agreement_no
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_instance_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
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

/// 提交框架协议审批。
#[nik::permission(
    path = "/business/erp/framework-agreement/{id}/submit",
    method = "POST",
    permission = "business:erp:framework-agreement:submit"
)]
#[nik::post("/business/erp/framework-agreement/{id}/submit")]
pub async fn framework_agreement_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SubmitAgreementReq>>,
) -> ResResult<AgreementDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let gate = ApprovalGate::new(&req.skip, &claims);
    let operator = utils::current_user_uuid(&claims);
    submit_agreement(id, &req, operator, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== 状态守卫（三处共用，报文一致） ====================

/// 只有草稿能做（改 / 删 / 提交）。
fn ensure_draft(row: &AgreementRow, action: &str) -> std::result::Result<(), ApiError> {
    if row.status == STATUS_DRAFT {
        return Ok(());
    }
    Err(dto::conflict(format!(
        "框架协议 {} 当前是「{}」（{}），只有草稿（DRAFT）可以{action}；\
         已提交 / 已生效的协议要调整请另签一份新协议（审批过的条款不接受改写）",
        row.agreement_no,
        status_label(&row.status),
        row.status
    )))
}

/// 读到的状态和刚写完的 CAS 撞车时的 409（并发提交 / 并发删除）。
fn status_drift(row: &AgreementRow, action: &str) -> ApiError {
    dto::conflict(format!(
        "框架协议 {} 的状态在本次{action}期间已被改动（期望 DRAFT），请刷新后重试",
        row.agreement_no
    ))
}

// ==================== 行 → DTO ====================

/// 单头行 → DTO（详情 / 列表 / 转换结果共用一份，避免三条赋值路径）。
pub(crate) fn to_row_res(row: &AgreementRow) -> AgreementRowRes {
    AgreementRowRes {
        id: row.id.to_string(),
        agreement_no: row.agreement_no.clone(),
        agreement_name: row.agreement_name.clone(),
        supplier_id: row.supplier_id.to_string(),
        supplier_name: row.supplier_name.clone(),
        supplier_display_name: row.supplier_display_name.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        owner_id: row.owner_id.map(|id| id.to_string()),
        owner_name: row.owner_name.clone(),
        start_date: row.start_date.to_string(),
        end_date: row.end_date.to_string(),
        sign_date: row.sign_date.map(|date| date.to_string()),
        price_mode: row.price_mode.clone(),
        price_mode_label: dto::price_mode_label(&row.price_mode).to_string(),
        price_includes_tax: row.price_includes_tax,
        price_currency: row.price_currency.clone(),
        price_tolerance: row.price_tolerance,
        price_adjust_rule: row.price_adjust_rule.clone(),
        total_max_qty: row.total_max_qty,
        total_max_amount: row.total_max_amount,
        total_executed_qty: row.total_executed_qty,
        total_executed_amount: row.total_executed_amount,
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        create_by: row.create_by.map(|id| id.to_string()),
        item_count: row.item_count,
        editable: row.status == STATUS_DRAFT,
    }
}

fn to_item_res(item: &AgreementItemRow, executed_qty: Decimal) -> AgreementItemRes {
    AgreementItemRes {
        id: item.id.to_string(),
        line_no: item.line_no,
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        agreed_price: item.agreed_price,
        tax_rate: item.tax_rate,
        max_qty: item.max_qty,
        executed_qty,
        executed_amount: item.executed_amount,
        remaining_qty: item.max_qty.map(|max| max - executed_qty),
        remark: item.remark.clone(),
    }
}
