//! 固定资产接口层（FIN-2 / ARCH-6 第五章）：分类 / 卡片 / 变动 / 处置。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/asset-category/page` | `asset-category:list` | 分类分页 |
//! | GET | `/business/erp/asset-category/list` | `asset-category:list` | 启用分类（建卡下拉） |
//! | POST | `/business/erp/asset-category` | `asset-category:create` | 新建分类（**编码留空自动取号 `AC-01`**） |
//! | GET | `/business/erp/asset-category/next-code` | `asset-category:create` | 预览下一个可用编码（只读，不消耗号） |
//! | PUT | `/business/erp/asset-category/{id}` | `asset-category:update` | 改分类 |
//! | DELETE | `/business/erp/asset-category/{id}` | `asset-category:delete` | 删分类（有卡片则 409） |
//! | GET | `/business/erp/asset/page` | `asset:list` | 卡片分页 |
//! | GET | `/business/erp/asset/{id}` | `asset:detail` | 详情（变动时间线 + 折旧明细） |
//! | POST | `/business/erp/asset` | `asset:create` | 建卡（**同时生成购入凭证**） |
//! | PUT | `/business/erp/asset/{id}` | `asset:update` | 改卡片（金额 / 分类不可改） |
//! | DELETE | `/business/erp/asset/{id}` | `asset:delete` | 软删（已计提 / 已处置 409） |
//! | POST | `/business/erp/asset/{id}/transfer` | `asset:transfer` | 部门转移（无凭证，留痕） |
//! | POST | `/business/erp/asset/{id}/custodian` | `asset:update` | 使用人变更（无凭证，留痕） |
//! | POST | `/business/erp/asset/{id}/dispose` | `asset:dispose` | 提交处置审批（`asset_disposal`） |
//! | POST | `/business/erp/asset/{id}/dispose/cancel` | `asset:dispose` | 作废处置（红冲 + 恢复在役） |
//! | POST | `/business/erp/asset/{id}/status` | `asset:update` | 停用 / 启用（`IN_USE ⇄ IDLE`；无凭证，折旧照提） |
//!
//! # 三条硬约束（写在这里免得后人「顺手优化」）
//!
//! ① **状态只能走状态机**：`state_machine::transition` 是唯一的状态写入口
//!    （它自己写 `biz_status_log`）；本文件没有 `match (from, to)` 的硬判。
//! ② **缓存列不给建卡 / 转移 / 处置碰**：`accumulated / net_value / months_done /
//!    last_period` 的唯一写入者是折旧过账与红冲（ARCH-6 §5.5.4）。
//! ③ **金额与分类不可改**（`asset_update` 只放行名称 / 规格 / 地点 / 备注）：
//!    它们决定已计提折旧的口径，改了会让「累计 = Σ明细」永久失真。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
// 标准库 `Result`（prelude 的 `Result<T>` 只有 1 个泛型参数，装不下 `ApiError`）。
use nik::uuid::Uuid;
use serde_json::json;
use std::result::Result;

use crate::business::attachment::model::DocKind as AttachDocKind;
use crate::business::attachment::rule::{self as attach_rule, RequiredAt as AttachAt};
use crate::business::erp::asset_bridge;
use crate::business::erp::asset_compute;
use crate::business::erp::asset_dto::{
    self, AssetDetailRes, AssetFilter, AssetRes, CategoryFilter, CategoryReq, CategoryRes, ChangeRes, CreateAssetReq,
    CustodianReq, DepDetailRes, DisposeReq, IdRes, StatusChangeReq, TransferReq, UpdateAssetReq, conflict, invalid,
    normalize_page, parse_uuid_text, status_label,
};
use crate::business::erp::asset_linkage;
use crate::business::erp::asset_model::{self, AssetInsert, ChangeInsert};
use crate::business::erp::code_policy;
use crate::business::erp::seq;
use crate::business::erp::voucher_multi;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "资产卡片不存在或已删除")
}

fn category_not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "资产分类不存在或已删除")
}

/// 岗位用 db_error（500 + 原始错误进 internal_detail）。
fn db_err(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

// ==================== ① 资产分类 ====================

/// 分类分页。
#[nik::permission(
    path = "/business/erp/asset-category/page",
    method = "GET",
    permission = "business:erp:asset-category:list"
)]
#[nik::get("/business/erp/asset-category/page")]
pub async fn asset_category_page(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
    Query(filter): Query<CategoryFilter>,
) -> ResResult<PageData<Vec<CategoryRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(asset_dto::DEFAULT_PAGE_SIZE),
    );
    let (rows, total) = asset_model::category_page(&filter, page, page_size)
        .await
        .map_err(|error| db_err(error, "分页查询资产分类"))?;
    let list: Vec<CategoryRes> = rows.iter().map(CategoryRes::from).collect();
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 启用分类（建卡下拉，不分页）。
#[nik::permission(
    path = "/business/erp/asset-category/list",
    method = "GET",
    permission = "business:erp:asset-category:list"
)]
#[nik::get("/business/erp/asset-category/list")]
pub async fn asset_category_list(Extension(AuthClaims(_claims)): Extension<AuthClaims>) -> ResResult<Vec<CategoryRes>> {
    let rows = asset_model::category_active()
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?;
    Ok(Res::ok(rows.iter().map(CategoryRes::from).collect()))
}

/// 新建分类（编码留空则由后端取号 `AC-01` 形态）。
#[nik::permission(
    path = "/business/erp/asset-category",
    method = "POST",
    permission = "business:erp:asset-category:create"
)]
#[nik::post("/business/erp/asset-category")]
pub async fn asset_category_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CategoryReq>,
) -> ResResult<CategoryRes> {
    let res = create_asset_category(operator_of(&claims), req).await?;
    Ok(Res::ok(res))
}

/// 分类编码预览（交互方式 B：前端「生成编码」按钮先看号，保存时把该号当手工编码提交）。
///
/// **只读、不消耗号**：`biz_seq.current_no` 不动，所以点了按钮又取消不会留空号；
/// 真到保存那一刻号被别人抢走 ⇒ 按手工编码撞唯一索引处理（409 + 66001），再点一次即可。
#[nik::permission(
    path = "/business/erp/asset-category/next-code",
    method = "GET",
    permission = "business:erp:asset-category:create"
)]
#[nik::get("/business/erp/asset-category/next-code")]
pub async fn asset_category_next_code(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    let code = seq::peek_master_code(db(), "AC")
        .await
        .map_err(|error| code_seq_error(error, "预览资产分类编码"))?;
    Ok(Res::ok(json!({ "code": code })))
}

/// 新建分类的**业务主体**（卡 CODE-04：编码留空则由后端取号 `AC-01` 形态）。
///
/// axum handler [`asset_category_create`] 只做参数解包，单测直接调本函数
/// （同一条代码路径，只跳过认证）。
///
/// # 编码口径（用户已拍板，不许改）
///
/// | 入参 `category_code` | 行为 |
/// |---|---|
/// | `null` / `""` / 纯空白 | 自动取号：`AC-01` 形态（`biz_seq` 的 AC 号段，**纯流水 2 位**、不计日期、永不重置）；撞号自动顺延 |
/// | 有值 | **原样保留**（大小写不改，与供应商 / 客户 / 物料 / 仓库 / 费用类型同一口径；`CAT-BUILDING` / `AC-01` 都能对接），只校验长度 ≤ 40 + 字符集 |
///
/// 位数由 `biz_seq.pad_width` 决定（AC = 2）：`AC-01` … `AC-99`，第 100 个起自然变成
/// `AC-100`（[`seq::format_master_code`] 只补零不截断，避免 2 位号段被腰斩成重号）。
///
/// # 取号与落库同一事务
///
/// 取号是 `UPDATE biz_seq … RETURNING`（行锁），插入失败则整体回滚、号一起退回，
/// 不留空号；并发建两个分类也不会拿到同一个号。
///
/// # 存量数据
///
/// **不动**任何已有分类（`ELEC` / `FURNITURE` / `VEHICLE` … 这些手工编码照旧），
/// 本函数只影响「此后新建且没填编码」的档案。
pub async fn create_asset_category(operator: Option<Uuid>, req: CategoryReq) -> Result<CategoryRes, ApiError> {
    let mut spec = asset_dto::validate_category(&req)?;
    // ① 手工编码：有值就保留（先查重，给出「编码 XXX 已存在」的人话错误）
    // CODE-08：开关关闭时填了编码直接 400（读在 begin 之前，被拒不占号）
    code_policy::guard_manual(
        code_policy::allow_manual_code().await,
        Some(spec.category_code.as_str()),
    )?;
    if !spec.category_code.is_empty() {
        let taken = seq::master_code_taken(db(), "AC", &spec.category_code)
            .await
            .map_err(|error| db_err(error, "校验分类编码唯一性"))?;
        if taken {
            return Err(category_code_conflict(&spec.category_code));
        }
    }
    let (asset_account, accum_account, expense_account) = resolve_category_accounts(&spec).await?;
    let id = Uuid::now_v7();
    // ② 编码 + INSERT 同一事务：没填就取号，插入失败则号一起回滚
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启新建资产分类事务"))?;
    if spec.category_code.is_empty() {
        spec.category_code = seq::next_master_code_free(&tx, "AC")
            .await
            .map_err(|error| code_seq_error(error, "新建资产分类"))?;
    }
    asset_model::category_insert(&tx, id, &spec, asset_account, accum_account, expense_account, operator)
        .await
        .map_err(|error| category_write_error(error, &spec.category_code))?;
    tx.commit()
        .await
        .map_err(|error| db_err(error, "提交新建资产分类事务"))?;
    tracing::info!(%id, code = %spec.category_code, "资产分类已新建");
    let row = asset_model::category_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(category_not_found)?;
    Ok(CategoryRes::from(&row))
}

/// 分类编码重复（409 + `66001`）：走六类共用的 [`code_conflict`]（E2E08-FIX-2 ③ 统一文案）。
fn category_code_conflict(code: &str) -> ApiError {
    super::handler::code_conflict("分类", code)
}

/// 分类写路径的 DB 错误收口：唯一索引冲突翻成**带编码**的 409，其余走 [`db_err`]。
fn category_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => category_code_conflict(code),
        _ => db_err(err, "写入资产分类"),
    }
}

/// 取号失败（规则缺失 / 连号全被占）：500 + `10004`，`internal_detail` 留原始错误。
fn code_seq_error(error: DbError, ctx: &str) -> ApiError {
    ApiError {
        status: 500,
        code: error_code::INTERNAL_ERROR,
        msg: format!("{ctx}：自动取号失败，请联系管理员检查取号规则"),
        internal_detail: Some(format!("{ctx}: {error}")),
        data: None,
    }
}

/// 修改分类。
#[nik::permission(
    path = "/business/erp/asset-category/{id}",
    method = "PUT",
    permission = "business:erp:asset-category:update"
)]
#[nik::put("/business/erp/asset-category/{id}")]
pub async fn asset_category_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CategoryReq>,
) -> ResResult<CategoryRes> {
    let id = parse_uuid_text(&id, "id")?;
    let existing = asset_model::category_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(category_not_found)?;
    let mut spec = asset_dto::validate_category(&req)?;
    // 编码留空 = 不改编码（编辑表单里编码常是只读/预填，提交空值不应报错）；
    // 填了就必须与原来一致 —— 编码是历史卡片的快照口径，改了会让老卡片对不上。
    if !spec.category_code.is_empty() && spec.category_code != existing.category_code {
        return Err(invalid(format!(
            "分类编码不可修改（当前 {}）：编码是历史卡片的快照口径，改了会让老卡片的 \
             category_code 与新分类对不上",
            existing.category_code
        )));
    }
    spec.category_code = existing.category_code.clone();
    let (asset_account, accum_account, expense_account) = resolve_category_accounts(&spec).await?;
    let affected = asset_model::category_update(
        db(),
        id,
        &spec,
        asset_account,
        accum_account,
        expense_account,
        operator_of(&claims),
    )
    .await
    .map_err(|error| db_err(error, "更新资产分类"))?;
    if affected == 0 {
        return Err(category_not_found());
    }
    // 分类参数改了**不影响已建卡**（快照口径，ARCH-6 §5.3.2）；已经在用的卡要按新参数提，
    // 只能新建卡片。这里明确告诉调用方，避免「改了分类以为老卡片跟着变」。
    let in_use = asset_model::category_asset_count(db(), id)
        .await
        .map_err(|error| db_err(error, "统计分类下的卡片"))?;
    if in_use > 0 {
        tracing::info!(
            %id,
            in_use,
            "资产分类已更新：{} 张存量卡片的折旧参数是快照，不随分类变化",
            in_use
        );
    }
    let row = asset_model::category_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(category_not_found)?;
    Ok(Res::ok(CategoryRes::from(&row)))
}

/// 删除分类（软删）。
#[nik::permission(
    path = "/business/erp/asset-category/{id}",
    method = "DELETE",
    permission = "business:erp:asset-category:delete"
)]
#[nik::delete("/business/erp/asset-category/{id}")]
pub async fn asset_category_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    let existing = asset_model::category_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(category_not_found)?;
    let in_use = asset_model::category_asset_count(db(), id)
        .await
        .map_err(|error| db_err(error, "统计分类下的卡片"))?;
    if in_use > 0 {
        return Err(conflict(format!(
            "分类 {} 下还有 {} 张资产卡片（含已处置）：先处理这些卡片，或把分类改为停用",
            existing.category_code, in_use
        )));
    }
    asset_model::category_soft_delete(db(), id, operator_of(&claims))
        .await
        .map_err(|error| db_err(error, "删除资产分类"))?;
    Ok(Res::ok(json!({"id": id.to_string(), "deleted": true})))
}

/// 分类的三个科目：未指定时用默认编码（1601 / 1602 / 5602.01），并校验末级 + 启用。
async fn resolve_category_accounts(spec: &asset_dto::CategorySpec) -> Result<(Uuid, Uuid, Uuid), ApiError> {
    let asset_account = pick_account(spec.asset_account_id, "1601", "原值科目").await?;
    let accum_account = pick_account(spec.accum_account_id, "1602", "累计折旧科目").await?;
    let expense_account = pick_account(spec.expense_account_id, "5602.01", "折旧费用科目").await?;
    Ok((asset_account, accum_account, expense_account))
}

/// 显式科目优先，缺省按编码取（都要末级 + 启用）。
async fn pick_account(explicit: Option<Uuid>, code: &'static str, what: &'static str) -> Result<Uuid, ApiError> {
    let account = match explicit {
        Some(id) => voucher_multi::ensure_leaf(db(), id)
            .await
            .map_err(move |error| asset_bridge::voucher_error(error, what))?,
        None => voucher_multi::leaf_required(db(), code)
            .await
            .map_err(move |error| asset_bridge::voucher_error(error, what))?,
    };
    Ok(account.id)
}

// ==================== ② 资产卡片 ====================

/// 卡片分页。
#[nik::permission(
    path = "/business/erp/asset/page",
    method = "GET",
    permission = "business:erp:asset:list"
)]
#[nik::get("/business/erp/asset/page")]
pub async fn asset_page(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
    Query(filter): Query<AssetFilter>,
) -> ResResult<PageData<Vec<AssetRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(asset_dto::DEFAULT_PAGE_SIZE),
    );
    let (rows, total) = asset_model::asset_page(&filter, page, page_size)
        .await
        .map_err(|error| db_err(error, "分页查询资产卡片"))?;
    let list: Vec<AssetRes> = rows.iter().map(AssetRes::from).collect();
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 卡片详情（变动时间线 + 折旧明细 + 在跑处置审批）。
#[nik::permission(
    path = "/business/erp/asset/{id}",
    method = "GET",
    permission = "business:erp:asset:detail"
)]
#[nik::get("/business/erp/asset/{id}")]
pub async fn asset_detail(Path(id): Path<String>) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 详情组装（handler 与真机用例共用）。
pub async fn detail_of(id: Uuid) -> Result<AssetDetailRes, ApiError> {
    let row = asset_model::asset_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产卡片"))?
        .ok_or_else(not_found)?;
    let changes = asset_model::changes_of(id)
        .await
        .map_err(|error| db_err(error, "查询资产变动"))?;
    let depreciations = asset_model::details_of_asset(id)
        .await
        .map_err(|error| db_err(error, "查询折旧明细"))?;
    let pending = flow_model::biz_instance_running(asset_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| db_err(error, "查询在跑处置审批"))?;
    let months_left = (row.useful_life_months - row.depreciation_months_done).max(0);
    Ok(AssetDetailRes {
        months_left,
        amount_left: row.depreciable_base - row.accumulated_depreciation,
        next_statuses: state_machine::Entity::FixedAsset
            .next_statuses(&row.status)
            .into_iter()
            .map(str::to_string)
            .collect(),
        pending_disposal: pending.then(|| id.to_string()),
        asset: AssetRes::from(&row),
        changes: changes.iter().map(ChangeRes::from).collect(),
        depreciations: depreciations.iter().map(DepDetailRes::from).collect(),
    })
}

/// 建卡（**同一事务里生成购入凭证**）。
#[nik::permission(
    path = "/business/erp/asset",
    method = "POST",
    permission = "business:erp:asset:create"
)]
#[nik::post("/business/erp/asset")]
pub async fn asset_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateAssetReq>,
) -> ResResult<IdRes> {
    let spec = asset_dto::validate_create(&req)?;
    let operator = operator_of(&claims);
    let now = Utc::now();
    let category = asset_model::category_find(spec.category_id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(|| {
            invalid(format!(
                "资产分类 {} 不存在或已删除：先建分类（折旧年限 / 残值率 / 科目都从分类带出）",
                spec.category_id
            ))
        })?;
    if category.status != 1 {
        return Err(invalid(format!(
            "资产分类 {} 已停用：不能再建卡",
            category.category_name
        )));
    }
    let dept_name = dept_name_of(spec.use_dept_id).await?;
    let custodian_name = match spec.custodian_id {
        Some(user_id) => Some(user_name_of(user_id).await?),
        None => None,
    };
    let supplier_name = match spec.supplier_id {
        Some(supplier_id) => Some(supplier_name_of(supplier_id).await?),
        None => None,
    };
    let (residual, base, monthly) =
        asset_dto::derive_amounts(spec.original_value, category.salvage_rate, category.useful_life_months);
    let start_date = asset_compute::start_depreciation_date(spec.purchase_date);

    let tx = db().begin().await.map_err(|error| db_err(error, "开启建卡事务"))?;
    let outcome = async {
        let asset_no = asset_model::next_asset_no(&tx, now)
            .await
            .map_err(|error| db_err(error, "取资产编号"))?;
        let id = Uuid::now_v7();
        let input = AssetInsert {
            asset_no: asset_no.clone(),
            category_code: category.category_code.clone(),
            use_dept_name: dept_name.clone(),
            custodian_name: custodian_name.clone(),
            supplier_name: supplier_name.clone(),
            residual_value: residual,
            depreciable_base: base,
            monthly_depreciation: monthly,
            useful_life_months: category.useful_life_months,
            salvage_rate: category.salvage_rate,
            start_depreciation_date: start_date,
            operator,
            spec: spec.clone(),
        };
        asset_model::asset_insert(&tx, id, &input)
            .await
            .map_err(|error| match error {
                DbError::UniqueViolation { .. } => conflict(format!(
                    "来源单 {} 已经建过资产卡片了（一张来源单只能转固一次）；\
                     或用同一编号重复提交，请刷新后重试",
                    spec.source_no.clone().unwrap_or_default()
                )),
                other => db_err(other, "写入资产卡片"),
            })?;
        // 购入凭证（ARCH-6 §5.3.3 的两种口径）
        let voucher_id = post_acquisition_voucher(&tx, &id, &asset_no, &spec, &category, now, operator).await?;
        asset_model::asset_set_voucher(&tx, id, voucher_id)
            .await
            .map_err(|error| db_err(error, "回写购入凭证"))?;
        // 状态日志（NULL → IN_USE）
        state_machine::log_created(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            id,
            &asset_no,
            asset_dto::STATUS_IN_USE,
            operator,
            Some("建卡并生成购入凭证"),
        )
        .await?;
        Ok::<(Uuid, String, Uuid), ApiError>((id, asset_no, voucher_id))
    }
    .await;
    let (id, asset_no, voucher_id) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit().await.map_err(|error| db_err(error, "提交建卡事务"))?;
    tracing::info!(%id, asset_no = %asset_no, %voucher_id, "固定资产卡片已建立（含购入凭证）");
    Ok(Res::ok(IdRes {
        id: id.to_string(),
        no: asset_no,
        note: Some(format!("购入凭证 {voucher_id}")),
    }))
}

/// 生成购入凭证（DIRECT_BUY 赊购 / 现付，或 FROM_STOCK 由存货转入）。
async fn post_acquisition_voucher(
    ex: &impl SqlExecutor,
    asset_id: &Uuid,
    asset_no: &str,
    spec: &asset_dto::CreateAssetSpec,
    category: &asset_model::CategoryRow,
    now: nik::prelude::chrono::DateTime<Utc>,
    operator: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let rule_code = match spec.buy_mode.as_str() {
        asset_dto::BUY_FROM_STOCK => "FIXED_ASSET_ACQ_STOCK",
        _ if spec.pay_mode == asset_dto::PAY_CASH => "FIXED_ASSET_ACQ_CASH",
        _ => "FIXED_ASSET_ACQ_DEFAULT",
    };
    let rule = voucher_multi::rule_accounts(ex, rule_code)
        .await
        .map_err(|error| asset_bridge::voucher_error(error, "解析购入凭证规则"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::APPROVAL_FLOW_NOT_CONFIGURED,
                format!(
                    "固定资产购入凭证规则 {rule_code} 没有配置（biz_voucher_rule）：\
                     请先跑迁移 V0232 或手工启用这条规则"
                ),
            )
        })?;
    // 借方原值科目取分类配置（默认 1601）；分类缺失科目时退回规则里的借方
    let asset_account = voucher_multi::ensure_leaf(ex, category.asset_account_id)
        .await
        .map_err(|error| asset_bridge::voucher_error(error, "解析资产原值科目"))?;
    let mut debits = vec![
        voucher_multi::DebitGroup::new(asset_account, spec.original_value, None)
            .with_summary(format!("{asset_no} 固定资产购入")),
    ];
    let mut credit_amount = spec.original_value;
    if spec.tax_amount > Decimal::ZERO {
        let tax = rule.tax.clone().ok_or_else(|| {
            ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "本次购入有可抵扣进项税，但凭证规则里没有税务科目（2221.01）：\
                 请检查 biz_voucher_rule 的 tax_account_id",
            )
        })?;
        debits.push(
            voucher_multi::DebitGroup::new(tax, spec.tax_amount, None).with_summary(format!("{asset_no} 可抵扣进项税")),
        );
        credit_amount += spec.tax_amount;
    }
    let summary = format!(
        "{asset_no} 固定资产购入（{}）价税合计 {}",
        if spec.buy_mode == asset_dto::BUY_FROM_STOCK {
            "由收货单转入"
        } else if spec.pay_mode == asset_dto::PAY_CASH {
            "现付"
        } else {
            "赊购"
        },
        credit_amount
    );
    voucher_multi::post_multi_debit(
        ex,
        &voucher_multi::MultiDebit {
            source_type: "FIXED_ASSET_ACQ",
            source_id: Some(*asset_id),
            source_no: asset_no,
            voucher_date: spec.purchase_date,
            summary,
            debits,
            credit_account: rule.credit,
            credit_amount,
            credit_summary: None,
            remark: None,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| asset_bridge::voucher_error(error, "生成购入凭证"))
}

/// 改卡片（只能改名称 / 规格 / 地点 / 备注）。
#[nik::permission(
    path = "/business/erp/asset/{id}",
    method = "PUT",
    permission = "business:erp:asset:update"
)]
#[nik::put("/business/erp/asset/{id}")]
pub async fn asset_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAssetReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let row = asset_model::asset_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产卡片"))?
        .ok_or_else(not_found)?;
    if row.status == asset_dto::STATUS_DISPOSED {
        return Err(conflict(format!(
            "资产 {} 已处置（{}）：已处置的卡片只读，要改请先作废处置",
            row.asset_no,
            status_label(&row.status)
        )));
    }
    let fields = asset_dto::validate_update(&req)?;
    let affected = asset_model::asset_update(
        db(),
        id,
        fields.asset_name.as_deref(),
        fields.spec.as_deref(),
        fields.location.as_deref(),
        fields.remark.as_deref(),
        operator_of(&claims),
    )
    .await
    .map_err(|error| db_err(error, "更新资产卡片"))?;
    if affected == 0 {
        return Err(not_found());
    }
    Ok(Res::ok(detail_of(id).await?))
}

/// 软删卡片（只有「从未计提、未处置、没有在跑审批」的卡能删）。
#[nik::permission(
    path = "/business/erp/asset/{id}",
    method = "DELETE",
    permission = "business:erp:asset:delete"
)]
#[nik::delete("/business/erp/asset/{id}")]
pub async fn asset_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    let row = asset_model::asset_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产卡片"))?
        .ok_or_else(not_found)?;
    if row.status == asset_dto::STATUS_DISPOSED {
        return Err(conflict(format!(
            "资产 {} 已处置：处置过的卡片不能删（它是处置凭证的来源单据，删了账就对不上）",
            row.asset_no
        )));
    }
    if row.depreciation_months_done > 0 || row.accumulated_depreciation > Decimal::ZERO {
        return Err(conflict(format!(
            "资产 {} 已计提 {} 个月折旧（累计 {}）：不能删。\
             要清理请做处置（出售 / 报废 / 盘亏），让账跟着凭证一起走",
            row.asset_no, row.depreciation_months_done, row.accumulated_depreciation
        )));
    }
    // **建卡即生成购入凭证**（`FIXED_ASSET_ACQ`，借 1601）：删卡片不会动那张凭证，
    // 1601 余额与卡片台账从此永久差这笔原值，而且界面上没有任何入口能把它找回来。
    // 所以「没计提到」不等于「能删」—— 有凭证就只能走处置，让账跟着凭证一起走。
    if let Some(acq) = crate::business::erp::voucher::find_voucher_by_source(db(), "FIXED_ASSET_ACQ", id)
        .await
        .map_err(|error| db_err(error, "查询购入凭证"))?
    {
        return Err(conflict(format!(
            "资产 {} 已生成购入凭证 {}（借 1601 原值）：不能删。\
             删了 1601 固定资产余额会永久多出这笔钱，且没有界面能冲回；\
             建错请做处置（报废 / 盘亏）冲销",
            row.asset_no, acq.voucher_no
        )));
    }
    if flow_model::biz_instance_running(asset_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| db_err(error, "查询在跑处置审批"))?
    {
        return Err(conflict(format!(
            "资产 {} 有在跑的处置审批：先撤回审批再删除",
            row.asset_no
        )));
    }
    asset_model::asset_soft_delete(db(), id, operator_of(&claims))
        .await
        .map_err(|error| db_err(error, "删除资产卡片"))?;
    Ok(Res::ok(json!({"id": id.to_string(), "deleted": true})))
}

/// 部门转移（无凭证；主表当前值 + 变动留痕 + 业务日志同事务）。
#[nik::permission(
    path = "/business/erp/asset/{id}/transfer",
    method = "POST",
    permission = "business:erp:asset:transfer"
)]
#[nik::post("/business/erp/asset/{id}/transfer")]
pub async fn asset_transfer(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TransferReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let today = voucher_today();
    let spec = asset_dto::validate_transfer(&req, today)?;
    let operator_name = operator_name_of(&claims);
    let tx = db().begin().await.map_err(|error| db_err(error, "开启转移事务"))?;
    let outcome = async {
        let row = asset_model::asset_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定资产卡片"))?
            .ok_or_else(not_found)?;
        if !asset_dto::disposable(&row.status) {
            return Err(conflict(format!(
                "资产 {} 当前是「{}」：只有在役 / 闲置的资产能转移",
                row.asset_no,
                status_label(&row.status)
            )));
        }
        if spec.to_dept_id == row.use_dept_id {
            return Err(invalid(format!(
                "目标部门与当前部门相同（{}）：原地转移不是变动",
                row.use_dept_name
            )));
        }
        let to_dept_name = dept_name_of(spec.to_dept_id).await?;
        asset_model::asset_set_dept(&tx, id, &row.status, spec.to_dept_id, &to_dept_name, operator)
            .await
            .map_err(|error| db_err(error, "更新资产使用部门"))?;
        asset_model::insert_change(
            &tx,
            id,
            &ChangeInsert {
                change_type: asset_dto::CHANGE_TRANSFER,
                change_date: spec.change_date,
                from_dept_id: Some(row.use_dept_id),
                from_dept_name: Some(row.use_dept_name.as_str()),
                to_dept_id: Some(spec.to_dept_id),
                to_dept_name: Some(to_dept_name.as_str()),
                from_user_id: row.custodian_id,
                from_user_name: row.custodian_name.as_deref(),
                to_user_id: row.custodian_id,
                to_user_name: row.custodian_name.as_deref(),
                from_status: Some(&row.status),
                to_status: Some(&row.status),
                reason: &spec.reason,
                voucher_id: None,
                approval_instance_id: None,
                operator_id: operator,
                operator_name: Some(operator_name.as_str()),
                remark: None,
            },
        )
        .await
        .map_err(|error| db_err(error, "写转移留痕"))?;
        // 状态没变（仍是 IN_USE / IDLE）：写「业务事件」日志而不是状态流转
        state_machine::log_event(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            id,
            &row.asset_no,
            &row.status,
            operator,
            Some(&format!(
                "部门转移 {} → {}：{}",
                row.use_dept_name, to_dept_name, spec.reason
            )),
            None,
        )
        .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await.map_err(|error| db_err(error, "提交转移事务"))?;
            tracing::info!(%id, "资产部门转移完成（下月起折旧费用归新部门）");
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 使用人变更（无凭证；留痕同转移）。
#[nik::permission(
    path = "/business/erp/asset/{id}/custodian",
    method = "POST",
    permission = "business:erp:asset:update"
)]
#[nik::post("/business/erp/asset/{id}/custodian")]
pub async fn asset_custodian(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CustodianReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let today = voucher_today();
    let spec = asset_dto::validate_custodian(&req, today)?;
    let tx = db().begin().await.map_err(|error| db_err(error, "开启变更事务"))?;
    let outcome = async {
        let row = asset_model::asset_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定资产卡片"))?
            .ok_or_else(not_found)?;
        if !asset_dto::disposable(&row.status) {
            return Err(conflict(format!(
                "资产 {} 当前是「{}」：只有在役 / 闲置的资产能变更使用人",
                row.asset_no,
                status_label(&row.status)
            )));
        }
        if row.custodian_id == Some(spec.to_user_id) {
            return Err(invalid("新使用人与当前使用人相同：原地变更不是变动"));
        }
        let to_user_name = user_name_of(spec.to_user_id).await?;
        asset_model::asset_set_custodian(&tx, id, &row.status, spec.to_user_id, &to_user_name, operator)
            .await
            .map_err(|error| db_err(error, "更新资产使用人"))?;
        asset_model::insert_change(
            &tx,
            id,
            &ChangeInsert {
                change_type: asset_dto::CHANGE_CUSTODIAN,
                change_date: spec.change_date,
                from_dept_id: Some(row.use_dept_id),
                from_dept_name: Some(row.use_dept_name.as_str()),
                to_dept_id: Some(row.use_dept_id),
                to_dept_name: Some(row.use_dept_name.as_str()),
                from_user_id: row.custodian_id,
                from_user_name: row.custodian_name.as_deref(),
                to_user_id: Some(spec.to_user_id),
                to_user_name: Some(to_user_name.as_str()),
                from_status: Some(&row.status),
                to_status: Some(&row.status),
                reason: &spec.reason,
                voucher_id: None,
                approval_instance_id: None,
                operator_id: operator,
                operator_name: None,
                remark: None,
            },
        )
        .await
        .map_err(|error| db_err(error, "写使用人变更留痕"))?;
        state_machine::log_event(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            id,
            &row.asset_no,
            &row.status,
            operator,
            Some(&format!(
                "使用人变更 {} → {}：{}",
                row.custodian_name.clone().unwrap_or_else(|| "（空）".to_string()),
                to_user_name,
                spec.reason
            )),
            None,
        )
        .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await.map_err(|error| db_err(error, "提交变更事务"))?;
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 提交处置审批（`asset_disposal`）：通过后由 `asset_bridge` 生成凭证并改状态。
///
/// **前置闸（FIX-3 方案 ①）**：处置当月的折旧必须先计提并过账，否则 409
/// `ERP_ASSET_DEPRECIATION_PENDING`。口径与理由见 [`asset_compute::disposal_dep_gate`]
/// 的文件注释（为什么把顺序钉成「先计提当月 → 再处置」，而不是让扫描纳入 `DISPOSED`）。
#[nik::permission(
    path = "/business/erp/asset/{id}/dispose",
    method = "POST",
    permission = "business:erp:asset:dispose"
)]
#[nik::post("/business/erp/asset/{id}/dispose")]
pub async fn asset_dispose(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<DisposeReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let Some(operator) = operator_of(&claims) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交处置需要登录用户（令牌里没有用户 id）",
        ));
    };
    let today = voucher_today();
    let spec = asset_dto::validate_dispose(&req, today)?;
    let row = asset_model::asset_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产卡片"))?
        .ok_or_else(not_found)?;
    if !asset_dto::disposable(&row.status) {
        return Err(ApiError::new(
            409,
            error_code::ERP_ASSET_DISPOSAL_STATE,
            format!(
                "资产 {} 当前是「{}」：只有在役 / 闲置的资产能处置",
                row.asset_no,
                status_label(&row.status)
            ),
        ));
    }
    // ── 铁律②「当月减少当月照提」的前置闸（FIX-3 方案 ①，口径定稿见
    //    `asset_compute::disposal_dep_gate` 的文件注释：为什么不选「扫描纳入 DISPOSED
    //    + 处置凭证延后」那条路，那里写清了）。
    //
    // 判据：**处置当月的折旧必须先计提并过账**。不满足 → 409 66048，报文指路。
    // 闸门放在「提交处置」这一步（审批发起之前 fail fast）：放到审批落定后的执行步，
    // 审批已通过、半路 409 只会把处置单卡在中间态，用户还得先作废审批。
    //
    // 两种放行的情况（当月本来不提折旧）：当月购入当月处置（起提日 = 次月 1 日）、
    // 已提足（提足即停，扫描 SQL 也不再命中它）—— 都由闸门函数自己判，这里只拼报文。
    let disposal_period = asset_compute::period_of(spec.disposal_date);
    let dep_status = asset_model::depreciation_status_of(id, &disposal_period)
        .await
        .map_err(|error| db_err(error, "查询处置当月的折旧明细"))?;
    match asset_compute::disposal_dep_gate(
        &disposal_period,
        row.start_depreciation_date,
        row.depreciable_base,
        row.accumulated_depreciation,
        dep_status.as_deref(),
    ) {
        asset_compute::DisposalDepGate::Missing(miss) => {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_DEPRECIATION_PENDING,
                format!(
                    "资产 {} 不能处置：{}。请先在「折旧计提」里计提并过账 {} 的折旧\
                     （手动计提时 period_code 填 {}），再回来提交处置 —— \
                     当月减少的资产当月照提，先处置会让这一个月的折旧永远提不到\
                     （处置凭证已按处置时点的累计折旧落账）。\
                     若该期间的批次已过账但不含本资产、或已被红冲，需先红冲该批次再重新计提。",
                    row.asset_no,
                    miss.label(),
                    disposal_period,
                    disposal_period
                ),
            ));
        }
        asset_compute::DisposalDepGate::NotRequired(skip) => {
            tracing::info!(
                asset_id = %id,
                asset_no = %row.asset_no,
                period = %disposal_period,
                why = skip.label(),
                "处置前置闸：当月无需计提折旧，放行"
            );
        }
        asset_compute::DisposalDepGate::Posted => {
            tracing::info!(
                asset_id = %id,
                asset_no = %row.asset_no,
                period = %disposal_period,
                "处置前置闸：处置当月折旧已过账"
            );
        }
    }
    let flow = asset_bridge::published_disposal_flow()
        .await
        .map_err(|error| db_err(error, "查询处置审批流程"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::APPROVAL_FLOW_NOT_CONFIGURED,
                format!(
                    "资产 {} 的处置必须走审批，但系统里没有已发布的 asset_disposal 流程，\
                     所以不能提交。请管理员在「审批流程」里发布资产处置流程（asset_disposal）：\
                     第一站总经理，不分金额档。",
                    row.asset_no
                ),
            )
        })?;
    if flow_model::biz_instance_running(asset_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| db_err(error, "查询在跑处置审批"))?
    {
        return Err(ApiError::new(
            409,
            error_code::ERP_ASSET_DISPOSAL_STATE,
            format!(
                "资产 {} 已有在跑的处置审批：请等审批结束或先撤回（同一张卡同时只能有一个处置单）",
                row.asset_no
            ),
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator)
        .await
        .map_err(|error| db_err(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.asset_no.clone()),
        title: format!("{} {} 处置审批", row.asset_no, spec.kind.label())
            .chars()
            .take(200)
            .collect::<String>(),
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: disposal_form_data(&row, &spec),
    };
    let mut out = {
        // 发起审批单要在事务里（`engine::start` 只吃 `&Transaction`）：
        // 实例 + 节点 + 待办要么一起落库，要么一起不落。
        let tx = db().begin().await.map_err(|error| db_err(error, "开启处置审批事务"))?;
        // ⚠️ 引擎的业务拒绝（65018 没有可用审批人 / 65019 自审被禁）必须在**事务内**
        //    当硬错误处理：`engine::start` 拒绝时已经写了一张 PENDING 实例却没有待办，
        //    先 commit 再判 `out.rejected` 会留下「半截审批单」——该实例占着
        //    `uk_biz_flow_instance_biz_active`，资产就永远卡在「已有在跑的处置审批」，
        //    既提不了也撤不掉（与卡 t_0127714a 的报销单同一个根因）。
        let out = match engine::start_in_tx(&tx, &start).await {
            Ok(out) => out,
            Err(engine::StartFailure::Rejected(rejected)) => {
                let _ = tx.rollback().await;
                // FIX-1：原样透传（不要用 `ApiError::new(..)` 重建，那会丢 65018 的结构化载荷）
                return Err(rejected.appended(
                    "（资产处置保持未提交、没有生成审批单：请让能审批的人提交，\
                     或把一位非制单人加入该节点的审批岗位后再提交）",
                ));
            }
            Err(engine::StartFailure::Db(error)) => {
                let _ = tx.rollback().await;
                return Err(db_err(error, "发起处置审批单"));
            }
        };
        tx.commit().await.map_err(|error| db_err(error, "提交处置审批事务"))?;
        out
    };
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    // 单站流程（只有总经理一个审批节点时）可能当场走完：立刻执行处置
    if asset_bridge::should_execute(&out.instance_status) {
        asset_bridge::on_disposal_settled(out.instance_id, &out.instance_status).await;
    }
    tracing::info!(
        %id,
        asset_no = %row.asset_no,
        kind = spec.kind.as_str(),
        instance = %instance_id,
        "资产处置已提交审批"
    );
    Ok(Res::ok(detail_of(id).await?))
}

/// 处置表单（`form_data` 契约：字段名与 `asset_bridge::disposal_spec_of` 逐字对应）。
fn disposal_form_data(row: &asset_model::AssetRow, spec: &asset_dto::DisposeSpec) -> serde_json::Value {
    json!({
        "asset_no": row.asset_no,
        "asset_name": row.asset_name,
        "disposal_type": spec.kind.as_str(),
        "disposal_date": spec.disposal_date.to_string(),
        "reason": spec.reason,
        "sale_price": spec.sale_price.to_string(),
        "output_tax": spec.output_tax.to_string(),
        "original_value": row.original_value.to_string(),
        "accumulated_depreciation": row.accumulated_depreciation.to_string(),
        "net_value": row.net_value.to_string(),
        "depreciation_months_done": row.depreciation_months_done,
        "useful_life_months": row.useful_life_months,
        "use_dept_name": row.use_dept_name,
    })
}

/// 作废处置（红冲处置凭证 + 资产恢复在役）。
#[nik::permission(
    path = "/business/erp/asset/{id}/dispose/cancel",
    method = "POST",
    permission = "business:erp:asset:dispose"
)]
#[nik::post("/business/erp/asset/{id}/dispose/cancel")]
pub async fn asset_dispose_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CancelDisposalReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("作废处置必须填原因（至少 2 个字）"))?
        .to_string();
    let tx = db().begin().await.map_err(|error| db_err(error, "开启作废处置事务"))?;
    let outcome = async {
        let row = asset_model::asset_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定资产卡片"))?
            .ok_or_else(not_found)?;
        if row.status != asset_dto::STATUS_DISPOSED {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_DISPOSAL_STATE,
                format!(
                    "资产 {} 当前是「{}」：只有「已处置」的卡片能作废处置",
                    row.asset_no,
                    status_label(&row.status)
                ),
            ));
        }
        // ① 红冲处置凭证（正向必配反向）
        let reds = asset_bridge::reverse_disposal_vouchers(&tx, id, &reason, operator).await?;
        if reds.is_empty() {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_DISPOSAL_STATE,
                format!(
                    "资产 {} 找不到可红冲的处置凭证（可能已经作废过）：请刷新后再试",
                    row.asset_no
                ),
            ));
        }
        // ② 状态机：DISPOSED → IN_USE（原因必填）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            row.id,
            &row.asset_no,
            &asset_bridge::asset_bridge_edges(),
            asset_dto::STATUS_DISPOSED,
            asset_dto::STATUS_IN_USE,
            operator,
            Some(&format!("作废处置：{reason}")),
            None,
        )
        .await?;
        // ③ 清处置痕迹 + 状态回写（一条 UPDATE：ck_biz_fa_disposed 要求两者同时到位）
        let affected = asset_model::asset_restore_from_disposal(&tx, id, operator)
            .await
            .map_err(|error| db_err(error, "恢复资产在役"))?;
        if affected == 0 {
            return Err(conflict("资产状态在作废处置期间被改动，请刷新后重试"));
        }
        // ④ 变动留痕（ENABLE：恢复在役；词表里没有 UNDISPOSE，理由写进 reason）。
        //    返回的变动行 id = **本次事件标识**：进通知幂等键（可逆边第二次事件要能发）。
        let event_ref = asset_model::insert_change(
            &tx,
            id,
            &ChangeInsert {
                change_type: asset_dto::CHANGE_ENABLE,
                change_date: voucher_today(),
                from_dept_id: Some(row.use_dept_id),
                from_dept_name: Some(row.use_dept_name.as_str()),
                to_dept_id: Some(row.use_dept_id),
                to_dept_name: Some(row.use_dept_name.as_str()),
                from_user_id: row.custodian_id,
                from_user_name: row.custodian_name.as_deref(),
                to_user_id: row.custodian_id,
                to_user_name: row.custodian_name.as_deref(),
                from_status: Some(asset_dto::STATUS_DISPOSED),
                to_status: Some(asset_dto::STATUS_IN_USE),
                reason: &reason,
                voucher_id: reds.last().copied(),
                approval_instance_id: None,
                operator_id: operator,
                operator_name: None,
                remark: Some(&format!("作废处置：红冲 {} 张处置凭证", reds.len())),
            },
        )
        .await
        .map_err(|error| db_err(error, "写作废处置留痕"))?;
        // ⑤ 跨单据联动（事务内动作；处置凭证已在 ① 步红冲 ⇒ 引擎排除
        //    GENERATE_VOUCHER / REVERSE_VOUCHER，见 asset_linkage 文件头）。
        //    目标态 = IN_USE：`FIXED_ASSET/IN_USE` 上的规则由「作废处置」与「启用」两处触发
        //    （建卡是建行、不接）；`event_ref` = ④ 的变动行 id ⇒ 幂等键按事件区分。
        let change =
            asset_linkage::run_in_tx(&tx, id, &row.asset_no, asset_dto::STATUS_IN_USE, operator, event_ref).await?;
        Ok::<(usize, asset_linkage::AssetStatusChange), ApiError>((reds.len(), change))
    }
    .await;
    match outcome {
        Ok((count, change)) => {
            tx.commit().await.map_err(|error| db_err(error, "提交作废处置事务"))?;
            // 铁律：通知 / 建任务只能在事务提交**之后**发（提交失败 = 作废没做成）
            asset_linkage::run_deferred_after_commit(change).await;
            tracing::info!(%id, red_count = count, "资产处置已作废（凭证红冲 + 资产恢复在役）");
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 停用 / 启用（IN_USE ⇄ IDLE） ====================

/// 停用 / 启用（`IN_USE ⇄ IDLE`）：**只改状态**，不落凭证、不走审批、折旧照提。
///
/// 事务顺序（与处置 / 作废处置同序，别调换）：
/// ② 锁卡（`FOR UPDATE`，并发两次只有一个进来）→ ③ 同态 / 可停用判定（409 66046）→
/// ④ 状态机（唯一状态写入口，写 `biz_status_log`）→ ⑤ CAS 回写（0 行 = 有别的写入路径，
/// 必须报错不能静默）→ ⑥ 变动留痕（部门 / 使用人四组全 `None`）→ ⑦ 事务内跑联动 →
/// ⑧ commit → ⑨ 提交后跑通知 → ⑩ 返回详情。
///
/// **不做期间结账校验**：不落凭证 ⇒ 与 `biz_accounting_period` 无关（别照抄处置那段）。
#[nik::permission(
    path = "/business/erp/asset/{id}/status",
    method = "POST",
    permission = "business:erp:asset:update"
)]
#[nik::post("/business/erp/asset/{id}/status")]
pub async fn asset_status_change(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatusChangeReq>,
) -> ResResult<AssetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let spec = asset_dto::validate_status_change(&req, voucher_today())?;
    let operator_name = operator_name_of(&claims);
    // ATT-06：启用（IDLE → IN_USE）即资产的「入账 / 生效」动作 —— 发票或验收单必传。
    // 建卡（asset_create）本身就把状态置为 `IN_USE` 并生成购入凭证，所以「新建可无」仍然成立。
    if spec.target_status == asset_dto::STATUS_IN_USE {
        attach_rule::ensure_attachments(AttachDocKind::FixedAsset, id, AttachAt::Activate).await?;
    }
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启停用 / 启用事务"))?;
    let outcome = async {
        // ② 锁卡：并发两次停用只有一个能进来（否则两条状态日志 + 两条通知）
        let row = asset_model::asset_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定资产卡片"))?
            .ok_or_else(not_found)?;
        // ③-a 同态：重复点击不是状态变更（也因此不会重复通知）
        if row.status == spec.target_status {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_STATUS_ILLEGAL,
                format!(
                    "资产 {} 当前已经是「{}」：重复{}不是状态变更（不会重复通知）",
                    row.asset_no,
                    status_label(&row.status),
                    if spec.target_status == asset_dto::STATUS_IDLE {
                        "停用"
                    } else {
                        "启用"
                    }
                ),
            ));
        }
        // ③-b 只有「在役 / 闲置」能停用 / 启用（已处置 / 已作废先作废处置）
        if !asset_dto::disposable(&row.status) {
            return Err(ApiError::new(
                409,
                error_code::ERP_ASSET_STATUS_ILLEGAL,
                format!(
                    "资产 {} 当前是「{}」：只有「在役 / 闲置」的卡片能停用 / 启用；\
                     已处置的请先作废处置",
                    row.asset_no,
                    status_label(&row.status)
                ),
            ));
        }
        // ④ 状态机（边校验 + `biz_status_log`）：与处置 / 作废处置同序 —— 先判边，再落地
        state_machine::transition(
            &tx,
            state_machine::ENTITY_FIXED_ASSET,
            row.id,
            &row.asset_no,
            &asset_bridge::asset_bridge_edges(),
            &row.status,
            spec.target_status,
            operator,
            Some(spec.reason.as_str()),
            None,
        )
        .await?;
        // ⑤ CAS 回写（WHERE status = row.status）：0 行 = 锁后仍被改，必须报错不能静默
        let affected = asset_model::apply_asset_status(&tx, id, &row.status, spec.target_status, operator)
            .await
            .map_err(|error| db_err(error, "回写资产状态"))?;
        if affected == 0 {
            return Err(conflict("资产状态在停用 / 启用期间被改动，请刷新后重试"));
        }
        // ⑥ 变动留痕：变动的是**状态**，部门 / 使用人四组全 None（时间线据 from/to_status 渲染）。
        //    变动类型只可能是两个词表值：停用 `CHANGE_IDLE` / 启用 `CHANGE_ENABLE`
        //    （映射只写在 `asset_dto::change_type_of` 一处，这里再钉一道防漂移断言）。
        let change_type = asset_dto::change_type_of(spec.target_status);
        debug_assert!(
            matches!(change_type, asset_dto::CHANGE_IDLE | asset_dto::CHANGE_ENABLE),
            "停用 / 启用的变动类型只可能是 IDLE / ENABLE"
        );
        let event_ref = asset_model::insert_change(
            &tx,
            id,
            &ChangeInsert {
                change_type,
                change_date: spec.change_date,
                from_dept_id: None,
                from_dept_name: None,
                to_dept_id: None,
                to_dept_name: None,
                from_user_id: None,
                from_user_name: None,
                to_user_id: None,
                to_user_name: None,
                from_status: Some(&row.status),
                to_status: Some(spec.target_status),
                reason: &spec.reason,
                voucher_id: None,
                approval_instance_id: None,
                operator_id: operator,
                operator_name: Some(operator_name.as_str()),
                remark: Some("停用 / 启用：不落凭证，折旧口径不变"),
            },
        )
        .await
        .map_err(|error| db_err(error, "写停用 / 启用留痕"))?;
        // ⑦ 跨单据联动（事务内；`event_ref` = ⑥ 的变动行 id ⇒ 可逆边第二次照样发通知）
        let change = asset_linkage::run_in_tx(&tx, id, &row.asset_no, spec.target_status, operator, event_ref).await?;
        Ok::<asset_linkage::AssetStatusChange, ApiError>(change)
    }
    .await;
    match outcome {
        Ok(change) => {
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交停用 / 启用事务"))?;
            // 铁律：通知 / 建任务只能在事务提交**之后**发（提交失败 = 状态没改）
            asset_linkage::run_deferred_after_commit(change).await;
            tracing::info!(
                %id,
                status = spec.target_status,
                "资产状态变更完成（停用 / 启用：不落凭证、折旧照提）"
            );
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 辅助查询 ====================

/// 部门名（不存在 / 已删 → 400，不允许编出「无主资产」）。
// SCOPE-EXEMPT: 按主键取单个部门的 dept_name（资产卡 / 处置单上的部门名快照），
//   只影响这一张单据的展示字段，不涉及行级可见性，也不是列表 / 导出查询；
//   注入数据范围反而会让「别的部门的资产」查不到自己挂的部门名。
pub async fn dept_name_of(dept_id: Uuid) -> Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&dept_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询部门"))?;
    match rows.first() {
        None => Err(invalid(format!(
            "部门 {dept_id} 不存在或已删除：资产必须挂在真实部门上（折旧费用要按部门核算）"
        ))),
        Some(row) => row.get_typed("dept_name").map_err(|error| db_err(error, "读取部门名")),
    }
}

/// 用户姓名（不存在 → 400）。
// SCOPE-EXEMPT: 按主键取单个账号的显示名（使用人姓名快照），不涉及行级可见性，
//   非列表 / 导出查询（口径同上方 dept_name_of）。
async fn user_name_of(user_id: Uuid) -> Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username) AS name FROM sys_user \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询用户"))?;
    match rows.first() {
        None => Err(invalid(format!("使用人 {user_id} 不存在或已删除"))),
        Some(row) => row.get_typed("name").map_err(|error| db_err(error, "读取使用人姓名")),
    }
}

/// 供应商名称（快照，仅溯源）。
async fn supplier_name_of(supplier_id: Uuid) -> Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&supplier_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询供应商"))?;
    match rows.first() {
        None => Err(invalid(format!("供应商 {supplier_id} 不存在或已删除"))),
        Some(row) => row
            .get_typed("supplier_name")
            .map_err(|error| db_err(error, "读取供应商名称")),
    }
}

/// 操作人名（日志可读性）。
fn operator_name_of(claims: &TokenClaims) -> String {
    claims.username.clone().unwrap_or_default()
}

/// 业务日（CST）—— 与凭证模块同一个口径。
fn voucher_today() -> nik::prelude::chrono::NaiveDate {
    asset_bridge::today_cst(Utc::now())
}

/// 作废处置请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CancelDisposalReq {
    /// 原因（必填）。
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    //! 卡 CODE-04：**资产分类编码**真库用例（`ERP_DB_URL` 开关，未设则 SKIP）。
    //!
    //! ```bash
    //! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<已迁移的库>" \
    //!   cargo test --lib business::erp::asset_handler -- --test-threads=1
    //! ```
    //!
    //! | # | 用例 | 覆盖 |
    //! |---|---|---|
    //! | 1 | `category_auto_code_generated_when_blank` | `null` / `""` / 纯空白 三种「没填」都取号 `AC-NN`，且真落库 |
    //! | 2 | `category_manual_code_is_kept` | 手工编码原样保留（`CAT-BUILDING` 这种企业自有形态），不强行改成 `AC-NN` |
    //! | 3 | `category_auto_code_increments` | 连续两次留空 ⇒ 号递增（= `biz_seq` 推进），不重号 |
    //! | 4 | `category_duplicate_code_conflicts` | 重复编码 ⇒ **409 + 66001** 且文案点名编码 |
    //!
    //! 这四条会**真的消耗 AC 号段**（自动取号就是被验证的行为本身）。只按 id 物理删
    //! 自己建的行，**不回退** `biz_seq.current_no`：主数据是档案，建成功就该永久占号
    //! （与 CODE-02/03 同口径）。存量 5 条分类（ELEC / FURNITURE / VEHICLE …）不动。

    use super::*;
    use crate::business::erp::period_test_support as support;

    /// 自动编码形态：`AC-` + ≥2 位纯流水（AC 的 `biz_seq.pad_width = 2` ⇒ `AC-01`）。
    fn is_auto_ac_code(code: &str) -> bool {
        let Some(digits) = code.strip_prefix("AC-") else {
            return false;
        };
        digits.len() >= 2 && digits.chars().all(|ch| ch.is_ascii_digit())
    }

    /// 分类请求：`category_code` 传 `None` / `Some("")` = 留空（自动取号）。
    /// 科目全部缺省 ⇒ 按编码取 1601 / 1602 / 5602.01（迁移种好的末级科目）。
    fn category_req(category_code: Option<&str>, name: &str) -> CategoryReq {
        CategoryReq {
            category_code: category_code.map(str::to_string),
            category_name: Some(name.to_string()),
            useful_life_months: Some(36),
            salvage_rate: None,
            asset_account_id: None,
            accum_account_id: None,
            expense_account_id: None,
            status: None,
            sort_order: None,
            remark: None,
        }
    }

    /// 读回一行分类（验「真落库」，不只信返回值）。
    async fn category_row(id: &str) -> asset_model::CategoryRow {
        let id = Uuid::parse_str(id).expect("分类 id");
        asset_model::category_find(id)
            .await
            .expect("回读分类")
            .expect("分类行必须存在")
    }

    /// 物理删掉本用例造的行（业务接口走软删；用例不留痕）。
    async fn drop_category(id: &str) {
        let id = Uuid::parse_str(id).expect("分类 id");
        db().execute_with_params(
            "DELETE FROM biz_asset_category WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("清分类夹具");
    }

    /// 当前 AC 号段的 `current_no`（判断「有没有真的推进」用）。
    async fn ac_current_no() -> i64 {
        db().query_first_sql(
            "SELECT current_no FROM biz_seq WHERE seq_type = 'AC' AND deleted_at IS NULL".to_string(),
            vec![],
        )
        .await
        .expect("查 AC 号段")
        .expect("AC 号段必须由 V0337 种入")
        .get_typed::<i64>("current_no")
        .expect("current_no")
    }

    /// ① 三种「没填」都自动取号，且真落库（存量分类不动）。
    #[tokio::test]
    async fn category_auto_code_generated_when_blank() {
        let Some(_guard) = support::setup().await else {
            return;
        };
        let before = ac_current_no().await;
        let seeded = asset_model::category_active().await.expect("存量分类");

        let by_none = create_asset_category(None, category_req(None, "CODE04 自动分类（未传）"))
            .await
            .expect("category_code 缺席应自动取号");
        assert!(
            is_auto_ac_code(&by_none.category_code),
            "自动编码必须是 AC-01 形态，实际 {}",
            by_none.category_code
        );
        let row = category_row(&by_none.id).await;
        assert_eq!(row.category_code, by_none.category_code, "自动编码必须真落库");

        let by_empty = create_asset_category(None, category_req(Some(""), "CODE04 自动分类（空串）"))
            .await
            .expect("空串应自动取号");
        assert!(is_auto_ac_code(&by_empty.category_code), "{}", by_empty.category_code);

        let by_spaces = create_asset_category(None, category_req(Some("   "), "CODE04 自动分类（空白）"))
            .await
            .expect("纯空白应自动取号");
        assert!(is_auto_ac_code(&by_spaces.category_code), "{}", by_spaces.category_code);

        // 三个号互不相同，且号段正好推进 3
        let mut codes = vec![
            by_none.category_code.clone(),
            by_empty.category_code.clone(),
            by_spaces.category_code.clone(),
        ];
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), 3, "三次取号必须是 3 个不同号：{codes:?}");
        assert_eq!(ac_current_no().await, before + 3, "取号数应与号段推进一致");

        // 存量分类一条不动
        let after = asset_model::category_active().await.expect("存量分类");
        assert_eq!(after.len(), seeded.len() + 3, "只应有新建的 3 条，存量分类不许被改");

        for created in [&by_none, &by_empty, &by_spaces] {
            drop_category(&created.id).await;
        }
    }

    /// ② 手工编码原样保留（企业自有编码体系要能对接），不走取号。
    #[tokio::test]
    async fn category_manual_code_is_kept() {
        let Some(_guard) = support::setup().await else {
            return;
        };
        let before = ac_current_no().await;
        let manual = format!("cat-building-{}", &Uuid::now_v7().simple().to_string()[..6]);
        let created = create_asset_category(None, category_req(Some(&manual), "CODE04 手工分类"))
            .await
            .expect("手工编码应通过");
        assert_eq!(
            created.category_code, manual,
            "手工编码大小写原样保留（E2E08-FIX-2 ①：六类统一不改大小写）"
        );
        assert!(!is_auto_ac_code(&created.category_code), "手工编码不该被改写成交号形态");
        let row = category_row(&created.id).await;
        assert_eq!(row.category_code, manual, "手工编码真落库（小写也照存）");
        assert_eq!(ac_current_no().await, before, "手工编码不消耗号段（取号器不该被调用）");

        // 预览接口口径：peek 不消耗号，拿到的号可以直接当手工编码保存
        let peeked = seq::peek_master_code(db(), "AC").await.expect("预览 AC 编码");
        assert!(is_auto_ac_code(&peeked), "预览必须给 AC-NN 形态：{peeked}");
        assert_eq!(ac_current_no().await, before, "预览不能推进号段");
        let from_peek = create_asset_category(None, category_req(Some(&peeked), "CODE04 预览号分类"))
            .await
            .expect("预览号应当能直接保存");
        assert_eq!(from_peek.category_code, peeked);

        drop_category(&created.id).await;
        drop_category(&from_peek.id).await;
    }

    /// ③ 连续两次留空 ⇒ 号递增（与 `biz_seq` 推进一致）。
    #[tokio::test]
    async fn category_auto_code_increments() {
        let Some(_guard) = support::setup().await else {
            return;
        };
        let first = create_asset_category(None, category_req(None, "CODE04 连号分类 A"))
            .await
            .expect("第一次取号");
        let second = create_asset_category(None, category_req(None, "CODE04 连号分类 B"))
            .await
            .expect("第二次取号");
        assert_ne!(first.category_code, second.category_code, "两次自动编码不能相同");
        let n1 = first
            .category_code
            .strip_prefix("AC-")
            .and_then(|d| d.parse::<i64>().ok())
            .expect("AC-NN 形态");
        let n2 = second
            .category_code
            .strip_prefix("AC-")
            .and_then(|d| d.parse::<i64>().ok())
            .expect("AC-NN 形态");
        assert_eq!(
            n2,
            n1 + 1,
            "第二次必须正好是下一个号（{} → {}）",
            first.category_code,
            second.category_code
        );

        drop_category(&first.id).await;
        drop_category(&second.id).await;
    }

    /// ④ 重复编码 ⇒ 409 + 66001，文案点名编码（预检先拦，不等 DB 唯一索引）。
    #[tokio::test]
    async fn category_duplicate_code_conflicts() {
        let Some(_guard) = support::setup().await else {
            return;
        };
        let manual = format!("CAT-DUP-{}", &Uuid::now_v7().simple().to_string()[..6]);
        let created = create_asset_category(None, category_req(Some(&manual), "CODE04 重复分类"))
            .await
            .expect("第一次应当成功");

        let err = create_asset_category(None, category_req(Some(&manual), "CODE04 重复分类 2"))
            .await
            .expect_err("重复编码必须被拒");
        assert_eq!(err.status, 409, "重复编码是冲突，不是参数错");
        assert_eq!(err.code, error_code::ERP_CODE_CONFLICT);
        assert!(err.msg.contains(&manual), "文案必须点名具体编码：{}", err.msg);

        drop_category(&created.id).await;
    }
}
