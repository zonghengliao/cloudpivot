//! 固定资产接口层（FIN-2 / ARCH-6 第五章）：分类 / 卡片 / 变动 / 处置。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/asset-category/page` | `asset-category:list` | 分类分页 |
//! | GET | `/business/erp/asset-category/list` | `asset-category:list` | 启用分类（建卡下拉） |
//! | POST | `/business/erp/asset-category` | `asset-category:create` | 新建分类 |
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

use crate::business::erp::asset_bridge;
use crate::business::erp::asset_compute;
use crate::business::erp::asset_dto::{
    self, AssetDetailRes, AssetFilter, AssetRes, CategoryFilter, CategoryReq, CategoryRes,
    ChangeRes, CreateAssetReq, CustodianReq, DepDetailRes, DisposeReq, IdRes, TransferReq,
    UpdateAssetReq, conflict, invalid, normalize_page, parse_uuid_text, status_label,
};
use crate::business::erp::asset_model::{self, AssetInsert, ChangeInsert};
use crate::business::erp::voucher_multi;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "资产卡片不存在或已删除",
    )
}

fn category_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "资产分类不存在或已删除",
    )
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
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

/// 启用分类（建卡下拉，不分页）。
#[nik::permission(
    path = "/business/erp/asset-category/list",
    method = "GET",
    permission = "business:erp:asset-category:list"
)]
#[nik::get("/business/erp/asset-category/list")]
pub async fn asset_category_list(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
) -> ResResult<Vec<CategoryRes>> {
    let rows = asset_model::category_active()
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?;
    Ok(Res::ok(rows.iter().map(CategoryRes::from).collect()))
}

/// 新建分类。
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
    let spec = asset_dto::validate_category(&req)?;
    let operator = operator_of(&claims);
    let (asset_account, accum_account, expense_account) = resolve_category_accounts(&spec).await?;
    let id = Uuid::now_v7();
    asset_model::category_insert(
        db(),
        id,
        &spec,
        asset_account,
        accum_account,
        expense_account,
        operator,
    )
    .await
    .map_err(|error| match error {
        DbError::UniqueViolation { .. } => conflict(format!(
            "分类编码 {} 已存在：编码是分类的唯一标识，请换一个",
            spec.category_code
        )),
        other => db_err(other, "写入资产分类"),
    })?;
    tracing::info!(%id, code = %spec.category_code, "资产分类已新建");
    let row = asset_model::category_find(id)
        .await
        .map_err(|error| db_err(error, "查询资产分类"))?
        .ok_or_else(category_not_found)?;
    Ok(Res::ok(CategoryRes::from(&row)))
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
    if spec.category_code != existing.category_code {
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
async fn resolve_category_accounts(
    spec: &asset_dto::CategorySpec,
) -> Result<(Uuid, Uuid, Uuid), ApiError> {
    let asset_account = pick_account(spec.asset_account_id, "1601", "原值科目").await?;
    let accum_account = pick_account(spec.accum_account_id, "1602", "累计折旧科目").await?;
    let expense_account = pick_account(spec.expense_account_id, "5602.01", "折旧费用科目").await?;
    Ok((asset_account, accum_account, expense_account))
}

/// 显式科目优先，缺省按编码取（都要末级 + 启用）。
async fn pick_account(
    explicit: Option<Uuid>,
    code: &'static str,
    what: &'static str,
) -> Result<Uuid, ApiError> {
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
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
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
    let (residual, base, monthly) = asset_dto::derive_amounts(
        spec.original_value,
        category.salvage_rate,
        category.useful_life_months,
    );
    let start_date = asset_compute::start_depreciation_date(spec.purchase_date);

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启建卡事务"))?;
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
        let voucher_id =
            post_acquisition_voucher(&tx, &id, &asset_no, &spec, &category, now, operator).await?;
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
    tx.commit()
        .await
        .map_err(|error| db_err(error, "提交建卡事务"))?;
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
            voucher_multi::DebitGroup::new(tax, spec.tax_amount, None)
                .with_summary(format!("{asset_no} 可抵扣进项税")),
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
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启转移事务"))?;
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
        asset_model::asset_set_dept(
            &tx,
            id,
            &row.status,
            spec.to_dept_id,
            &to_dept_name,
            operator,
        )
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
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交转移事务"))?;
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
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启变更事务"))?;
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
        asset_model::asset_set_custodian(
            &tx,
            id,
            &row.status,
            spec.to_user_id,
            &to_user_name,
            operator,
        )
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
                row.custodian_name
                    .clone()
                    .unwrap_or_else(|| "（空）".to_string()),
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
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交变更事务"))?;
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 提交处置审批（`asset_disposal`）：通过后由 `asset_bridge` 生成凭证并改状态。
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
        let tx = db()
            .begin()
            .await
            .map_err(|error| db_err(error, "开启处置审批事务"))?;
        match engine::start(&tx, &start).await {
            Ok(out) => {
                tx.commit()
                    .await
                    .map_err(|error| db_err(error, "提交处置审批事务"))?;
                out
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(db_err(error, "发起处置审批单"));
            }
        }
    };
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(rejected.status, rejected.code, rejected.msg));
    }
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
fn disposal_form_data(
    row: &asset_model::AssetRow,
    spec: &asset_dto::DisposeSpec,
) -> serde_json::Value {
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
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启作废处置事务"))?;
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
        // ④ 变动留痕（ENABLE：恢复在役；词表里没有 UNDISPOSE，理由写进 reason）
        asset_model::insert_change(
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
        Ok::<usize, ApiError>(reds.len())
    }
    .await;
    match outcome {
        Ok(count) => {
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交作废处置事务"))?;
            tracing::info!(%id, red_count = count, "资产处置已作废（凭证红冲 + 资产恢复在役）");
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
        Some(row) => row
            .get_typed("dept_name")
            .map_err(|error| db_err(error, "读取部门名")),
    }
}

/// 用户姓名（不存在 → 400）。
// SCOPE-EXEMPT: 按主键取单个账号的显示名（使用人姓名快照），不涉及行级可见性，
//   非列表 / 导出查询（口径同上方 dept_name_of）。
async fn user_name_of(user_id: Uuid) -> Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(nickname, ''), username) AS name FROM sys_user \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询用户"))?;
    match rows.first() {
        None => Err(invalid(format!("使用人 {user_id} 不存在或已删除"))),
        Some(row) => row
            .get_typed("name")
            .map_err(|error| db_err(error, "读取使用人姓名")),
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
