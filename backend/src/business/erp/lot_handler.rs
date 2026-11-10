//! 批次主数据接口层（成本核算 · ARCH-9 §5.2 #7–#9 + 「按物料取批号」内部接口）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/lot/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 7 | GET | `/business/erp/lot` | `business:erp:lot:list` | 分页列表 |
//! | — | GET | `/business/erp/lot/options` | `business:erp:lot:list` | **按物料取批号**（下拉） |
//! | 8 | POST | `/business/erp/lot` | `business:erp:lot:create` | 新建 |
//! | 9 | PUT | `/business/erp/lot/{id}` | `business:erp:lot:update` | 编辑（部分更新） |
//!
//! # 「按物料取批号」为什么不多开一个权限码
//!
//! 设计 §5.3 只给了 `lot:list | create | update` 三个码，且这张菜单（V0378 的
//! `business:erp:lot:list`）就是批次管理的入口。下拉是**读**操作，复用 `lot:list`；
//! 若为它单开码，菜单种子（已应用的迁移）里没有承载行，`perm_menu_gate` 会红。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_object_handler` / `customer_handler` 同款：axum handler 要 `AuthClaims`
//! 才能跑，单测调不到；把「校验 + 落库」收进 `create_lot` / `update_lot` /
//! `page_lots` / `lot_options` 之后，真库用例走**同一条代码路径**（只跳过认证），
//! 操作人用 `Option<Uuid>` 传。
//!
//! # 唯一键冲突怎么给友好文案
//!
//! 两层：① 写之前 `model::lot_no_taken` 先查一次（命中 ⇒ 409 + `66056`）；
//! ② 并发穿透时库的 `uk_biz_lot_material_no` 会抛 `DbError::UniqueViolation`，
//! 由 `lot_dto::map_write_error` 按**变体**翻译成同一条码同一条文案。
//! 用户永远看到「批号『X』在物料『Y』下已存在」，看不到 PG 的约束名。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::lot_dto::{
    self, LotCreateReq, LotFilter, LotListRes, LotOptionQuery, LotOptionRes, LotPageQuery, LotUpdateReq,
};
use crate::business::erp::lot_model::{self as model, LotRow};
use crate::business::erp::model_base::normalize_page;
use crate::utils;

// ==================== 权限码（与设计 §5.3 逐字一致） ====================

/// 列表 / 下拉。
pub const PERM_LIST: &str = "business:erp:lot:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:lot:create";
/// 编辑。
pub const PERM_UPDATE: &str = "business:erp:lot:update";

/// 批次主数据全部权限码（设计 §5.3 的 3 个）。
pub const LOT_PERMISSIONS: [&str; 3] = [PERM_LIST, PERM_CREATE, PERM_UPDATE];

// ==================== 引用校验（建 / 改的查库部分） ====================

/// 物料必存在且未软删；返回人话标识（错误文案 / 日志用）。
async fn ensure_material(material_id: Uuid) -> std::result::Result<String, ApiError> {
    match model::material_of(material_id)
        .await
        .map_err(|error| utils::db_error(error, "校验批次物料"))?
    {
        Some((code, name)) => Ok(model::label_of(material_id, code.as_deref(), name.as_deref())),
        None => Err(lot_dto::bad_request(format!(
            "material_id（{material_id}）指向的物料不存在或已删除"
        ))),
    }
}

/// 供应商存在性（填了才校验：采购批才有供应商，生产批没有）。
async fn ensure_supplier(supplier_id: Option<Uuid>) -> std::result::Result<(), ApiError> {
    if let Some(supplier_id) = supplier_id
        && model::supplier_of(supplier_id)
            .await
            .map_err(|error| utils::db_error(error, "校验批次供应商"))?
            .is_none()
    {
        return Err(lot_dto::bad_request(format!(
            "supplier_id（{supplier_id}）指向的供应商不存在或已删除"
        )));
    }
    Ok(())
}

/// 批号唯一性（同物料下不重复；编辑时排除自己）。
async fn ensure_lot_no_free(
    material_id: Uuid,
    lot_no: &str,
    exclude_id: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let taken = model::lot_no_taken(material_id, lot_no, exclude_id)
        .await
        .map_err(|error| utils::db_error(error, "校验批次批号唯一性"))?;
    if !taken {
        return Ok(());
    }
    let label = ensure_material(material_id).await?;
    Err(ApiError::new(
        409,
        lot_dto::DUPLICATE_CODE,
        lot_dto::duplicate_message(lot_no, &label),
    ))
}

// ==================== 出参组装 ====================

/// 列表行。
fn to_list_res(row: &LotRow) -> LotListRes {
    LotListRes {
        id: row.id.to_string(),
        lot_no: row.lot_no.clone(),
        material_id: row.material_id.to_string(),
        material_name: row.material_name.clone(),
        material_code: row.material_code.clone(),
        source_type: row.source_type.clone(),
        source_doc_type: row.source_doc_type.clone(),
        source_doc_id: row.source_doc_id.map(|id| id.to_string()),
        source_doc_no: row.source_doc_no.clone(),
        supplier_id: row.supplier_id.map(|id| id.to_string()),
        supplier_name: row.supplier_name.clone(),
        prod_date: row.prod_date.map(|date| date.to_string()),
        expire_date: row.expire_date.map(|date| date.to_string()),
        qc_status: row.qc_status.clone(),
        status: row.status,
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 下拉选项。
fn to_option_res(row: &LotRow) -> LotOptionRes {
    LotOptionRes {
        id: row.id.to_string(),
        lot_no: row.lot_no.clone(),
        material_id: row.material_id.to_string(),
        material_name: row.material_name.clone(),
        source_type: row.source_type.clone(),
        prod_date: row.prod_date.map(|date| date.to_string()),
        expire_date: row.expire_date.map(|date| date.to_string()),
        qc_status: row.qc_status.clone(),
    }
}

/// 按 id 组装列表行（建 / 改后的回单都走它）。
async fn row_of(id: Uuid) -> std::result::Result<LotListRes, ApiError> {
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次"))?
        .ok_or_else(|| lot_dto::not_found(id))?;
    Ok(to_list_res(&row))
}

// ==================== 业务逻辑（单测 / 真库用例直接调这些） ====================

/// 新建批次（§5.2 #8）。
pub async fn create_lot(operator: Option<Uuid>, req: LotCreateReq) -> std::result::Result<LotListRes, ApiError> {
    let spec = lot_dto::spec_from_create(&req)?;
    let label = ensure_material(spec.material_id).await?;
    ensure_supplier(spec.supplier_id).await?;
    // 先查一次：常规重复在这里就是 409 + 人话（并发穿透由下面的 map_write_error 兜）
    if model::lot_no_taken(spec.material_id, &spec.lot_no, None)
        .await
        .map_err(|error| utils::db_error(error, "校验批次批号唯一性"))?
    {
        return Err(ApiError::new(
            409,
            lot_dto::DUPLICATE_CODE,
            lot_dto::duplicate_message(&spec.lot_no, &label),
        ));
    }
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| lot_dto::map_write_error(error, &spec.lot_no, &label, "新建批次"))?;
    row_of(id).await
}

/// 编辑批次（§5.2 #9；`material_id` 不可改 —— 业务键不可变）。
pub async fn update_lot(
    operator: Option<Uuid>,
    id_raw: &str,
    req: LotUpdateReq,
) -> std::result::Result<LotListRes, ApiError> {
    let id = lot_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的批次"))?
        .ok_or_else(|| lot_dto::not_found(id))?;
    let spec = lot_dto::spec_from_update(&row.to_spec(), &req)?;
    let label = ensure_material(spec.material_id).await?;
    ensure_supplier(spec.supplier_id).await?;
    // 改批号：唯一性要排除自己（改成自己原批号不算冲突）
    if spec.lot_no != row.lot_no {
        ensure_lot_no_free(spec.material_id, &spec.lot_no, Some(id)).await?;
    }
    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| lot_dto::map_write_error(error, &spec.lot_no, &label, "编辑批次"))?;
    if affected == 0 {
        // 查过存在、更新却是 0 行 ⇒ 期间被别人软删了
        return Err(lot_dto::not_found(id));
    }
    row_of(id).await
}

/// 列表分页（批次不参与数据权限 ⇒ 只要权限码）。
pub async fn page_lots(query: LotPageQuery) -> std::result::Result<PageData<Vec<LotListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询批次"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &LotPageQuery) -> std::result::Result<LotFilter, ApiError> {
    let source_type = lot_dto::clean_optional(query.source_type.clone())
        .map(|text| text.to_ascii_uppercase())
        .map(|text| {
            if lot_dto::SOURCE_TYPES.contains(&text.as_str()) {
                Ok(text)
            } else {
                Err(lot_dto::bad_request(format!(
                    "source_type 只认 {:?}（收到「{text}」）",
                    lot_dto::SOURCE_TYPES
                )))
            }
        })
        .transpose()?;
    let qc_status = lot_dto::clean_optional(query.qc_status.clone())
        .map(|text| text.to_ascii_uppercase())
        .map(|text| {
            if lot_dto::QC_STATUSES.contains(&text.as_str()) {
                Ok(text)
            } else {
                Err(lot_dto::bad_request(format!(
                    "qc_status 只认 {:?}（收到「{text}」）",
                    lot_dto::QC_STATUSES
                )))
            }
        })
        .transpose()?;
    if let Some(status) = query.status
        && status != lot_dto::STATUS_ENABLED
        && status != lot_dto::STATUS_DISABLED
    {
        return Err(lot_dto::bad_request(format!(
            "status 只认 0（停用）/ 1（启用）（收到 {status}）"
        )));
    }
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(LotFilter {
        keyword: lot_dto::clean_optional(query.keyword.clone()),
        material_id: lot_dto::parse_opt_uuid(query.material_id.as_deref(), "material_id")?,
        source_type,
        qc_status,
        status: query.status,
        page,
        page_size,
    })
}

/// 「按物料取批号」（下拉）：物料必填，只返回启用批次。
pub async fn lot_options(query: LotOptionQuery) -> std::result::Result<Vec<LotOptionRes>, ApiError> {
    let material_id = lot_dto::require_uuid(query.material_id.as_deref(), "material_id")?;
    // 物料不存在时给 400 指名，而不是「空下拉」（空列表会让用户以为「这个料没有批」）
    ensure_material(material_id).await?;
    let keyword = lot_dto::clean_optional(query.keyword.clone());
    let rows = model::options_for_material(material_id, &keyword)
        .await
        .map_err(|error| utils::db_error(error, "按物料查询批次"))?;
    Ok(rows.iter().map(to_option_res).collect())
}

// ==================== HTTP 入口 ====================

/// 批次分页。
#[nik::permission(path = "/business/erp/lot", method = "GET", permission = "business:erp:lot:list")]
#[nik::get("/business/erp/lot")]
pub async fn lot_page_api(Query(query): Query<LotPageQuery>) -> ResResult<PageData<Vec<LotListRes>>> {
    Ok(Res::ok(page_lots(query).await?))
}

/// 按物料取批号（下拉）。
#[nik::permission(
    path = "/business/erp/lot/options",
    method = "GET",
    permission = "business:erp:lot:list"
)]
#[nik::get("/business/erp/lot/options")]
pub async fn lot_options_api(Query(query): Query<LotOptionQuery>) -> ResResult<Vec<LotOptionRes>> {
    Ok(Res::ok(lot_options(query).await?))
}

/// 新建批次。
#[nik::permission(path = "/business/erp/lot", method = "POST", permission = "business:erp:lot:create")]
#[nik::post("/business/erp/lot")]
pub async fn lot_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<LotCreateReq>,
) -> ResResult<LotListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_lot(operator, req).await?))
}

/// 编辑批次。
#[nik::permission(
    path = "/business/erp/lot/{id}",
    method = "PUT",
    permission = "business:erp:lot:update"
)]
#[nik::put("/business/erp/lot/{id}")]
pub async fn lot_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<LotUpdateReq>,
) -> ResResult<LotListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_lot(operator, &id, req).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    LOT_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:lot:list");
        assert_eq!(PERM_CREATE, "business:erp:lot:create");
        assert_eq!(PERM_UPDATE, "business:erp:lot:update");
        assert_eq!(all_permissions().len(), 3, "设计 §5.3 只给了 3 个码");
        for code in LOT_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "lot", "{code}");
        }
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/lot",
            "/business/erp/lot/options",
            "/business/erp/lot/{id}",
        ] {
            assert!(path.starts_with("/business/erp/lot"), "{path}");
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    // ---------- 列表筛选归一化 ----------

    #[test]
    fn filter_normalizes_enums_and_paging() {
        let query = LotPageQuery {
            source_type: Some(" production ".to_string()),
            qc_status: Some("passed".to_string()),
            status: Some(1),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.source_type.as_deref(), Some("PRODUCTION"));
        assert_eq!(filter.qc_status.as_deref(), Some("PASSED"));
        assert_eq!(filter.status, Some(1));
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "page_size=0 回落到默认 20");

        let query = LotPageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_unknown_enums() {
        let query = LotPageQuery {
            source_type: Some("MES".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("未知来源");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("source_type"), "{}", error.msg);

        let query = LotPageQuery {
            qc_status: Some("OK".to_string()),
            ..Default::default()
        };
        assert!(filter_of(&query).expect_err("未知质检").msg.contains("qc_status"));

        let query = LotPageQuery {
            status: Some(9),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("未知状态");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("status"), "{}", error.msg);
    }

    #[test]
    fn filter_rejects_bad_uuid() {
        let query = LotPageQuery {
            material_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("非法 UUID");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("material_id"), "{}", error.msg);
    }

    // ---------- 出参映射 ----------

    fn sample_row() -> LotRow {
        let now = Utc::now();
        LotRow {
            id: Uuid::now_v7(),
            lot_no: "LOT-A1".to_string(),
            material_id: Uuid::now_v7(),
            source_type: "PURCHASE".to_string(),
            source_doc_type: Some("STOCK_IN".to_string()),
            source_doc_id: Some(Uuid::now_v7()),
            source_doc_no: Some("SI-20261110-000001".to_string()),
            supplier_id: Some(Uuid::now_v7()),
            prod_date: Some("2026-11-01".parse().expect("date")),
            expire_date: Some("2027-11-01".parse().expect("date")),
            qc_status: "PASSED".to_string(),
            status: 1,
            remark: Some("首件".to_string()),
            created_at: now,
            updated_at: now,
            material_name: Some("钢轴".to_string()),
            material_code: Some("MAT-0001".to_string()),
            supplier_name: Some("宁波钢厂".to_string()),
        }
    }

    #[test]
    fn list_res_carries_names_for_every_id() {
        let row = sample_row();
        let res = to_list_res(&row);
        assert_eq!(res.material_id, row.material_id.to_string());
        assert_eq!(res.material_name.as_deref(), Some("钢轴"));
        assert_eq!(res.material_code.as_deref(), Some("MAT-0001"));
        assert_eq!(res.supplier_id, row.supplier_id.map(|id| id.to_string()));
        assert_eq!(res.supplier_name.as_deref(), Some("宁波钢厂"));
        assert_eq!(res.prod_date.as_deref(), Some("2026-11-01"));
        assert_eq!(res.status, 1);
        // 日期是字符串出参，不是 JS Date（约定：跨端统一字符串）
        assert!(res.created_at.contains('T'), "{}", res.created_at);
    }

    #[test]
    fn option_res_is_minimal() {
        let row = sample_row();
        let res = to_option_res(&row);
        assert_eq!(res.lot_no, "LOT-A1");
        assert_eq!(res.material_name.as_deref(), Some("钢轴"));
        assert_eq!(res.qc_status, "PASSED");
    }
}
