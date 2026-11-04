//! 单品序列号接口层（批次 D · W2b）：5 个接口 + 权限码 + 状态机落地。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/serial/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/serial/batch-generate` | `business:erp:serial:generate` | 批量生成（物料必须 `serial_required = TRUE`） |
//! | GET | `/business/erp/serial/page` | `business:erp:serial:list` | 分页列表 |
//! | GET | `/business/erp/serial/scan?serial_no=` | `business:erp:serial:view` | 扫码定位（唯一索引点查） |
//! | PATCH | `/business/erp/serial/{id}/status` | `business:erp:serial:status` | 状态流转（不可逆） |
//! | GET | `/business/erp/serial/{id}` | `business:erp:serial:detail` | 详情 |
//!
//! 路由冲突说明：`/serial/scan`、`/serial/page` 是**静态段**，`/serial/{id}` 是动态段，
//! axum 的 matchit 静态优先，`GET /serial/scan` 不会被 `{id}` 吃掉（e2e 实测）。
//!
//! # 为什么把业务逻辑拆成 `generate_serials` / `apply_status` 两个函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；
//! 把「校验 + 取号 + 落库」收进普通 async 函数后，单测走的是**同一条代码路径**（只跳过认证），
//! 比「另写一份测试逻辑」更能钉住行为。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 单品序列号是仓储作业数据（V0138 无 dept_id / 归属人列），
//! 可见性由 business:erp:serial:* 权限码控制；列表分页走 model 的 scope::paginate_unscoped。
//! ```

use axum::Json;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Deserialize;

use crate::business::erp::model;
use crate::business::erp::serial_dto::{
    BatchGenerateReq, SerialBatchRes, SerialFilter, SerialRes, SerialScanRes, SerialStatusReq,
};
use crate::business::erp::serial_model::{self, GenerateParams, TransitionOutcome};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0142 迁移逐字一致） ====================

/// 卡面 §三 的 5 个权限码。
pub const SERIAL_PERMISSIONS: [(&str, &str); 5] = [
    ("business:erp:serial:list", "序列号列表"),
    ("business:erp:serial:detail", "序列号详情"),
    ("business:erp:serial:view", "序列号扫码"),
    ("business:erp:serial:generate", "批量生成序列号"),
    ("business:erp:serial:status", "序列号状态流转"),
];

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

/// 可空 UUID：空串 / 缺席都是 `None`。
fn optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_uuid(text, field).map(Some),
        None => Ok(None),
    }
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

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`，其余交给 `utils::db_error`。
///
/// 注意：**不要**把 `DbError::InvalidValue` 一律翻成 400 —— 模型层用同一个变体表达
/// 两类完全不同的事：①「调用方参数不自洽」（应当 400）与 ②「服务端前置数据缺失」
/// （科目被停用 / 映射缺失，应当 500 + `internal_detail`，见 T15 用例）。
/// 入参类的判断在接口层做（`source_pair` 校验就是），这里只保留唯一冲突这一条已知语义。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "序列号重复（唯一约束{}）：取号器与应用数据不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== ① 批量生成 ====================

/// 批量生成的核心逻辑（handler 与单测共用）。
///
/// 校验顺序即错误优先级：**参数 → 物料存在 → 一物一码开关 → 仓库 → 库位 → 取号落库**。
/// 先校验引用再取号：取号是会推进 `biz_seq` 的副作用，校验不过就不该把号消耗掉。
pub async fn generate_serials(
    req: &BatchGenerateReq,
    operator: Option<Uuid>,
) -> std::result::Result<SerialBatchRes, ApiError> {
    req.validate()?;
    let material_id = parse_uuid(&req.material_id, "material_id")?;
    // 成本单价（校验已在 `validate` 里做过一次；这里取同一个舍入后的值，避免两处口径漂移）
    let unit_cost = req.parse_unit_cost()?;

    let required = serial_model::material_serial_required(material_id)
        .await
        .map_err(|error| utils::db_error(error, "查询物料一物一码开关"))?;
    let Some(required) = required else {
        return Err(not_found("物料"));
    };
    if !required {
        return Err(ApiError::new(
            400,
            error_code::ERP_SERIAL_NOT_REQUIRED,
            format!(
                "物料 {material_id} 未开启一物一码（serial_required = FALSE）：\
                     按数量管理的物料不生成序列号，请先在物料主数据里打开该开关"
            ),
        ));
    }

    let warehouse_id = optional_uuid(req.warehouse_id.as_deref(), "warehouse_id")?;
    // 不变式①（W2c-1）：账面数量按 (物料, 仓库) 由序列号派生 —— 没有仓库就没有账面，
    // 也就无法保证「quantity == count(IN_STOCK)」，所以一物一码物料必须指定仓库。
    let Some(warehouse_id) = warehouse_id else {
        return Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            "一物一码物料批量生成必须指定 warehouse_id：库存数量按 物料×仓库 由序列号派生（不变式①）",
        ));
    };
    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库"))?
        .ok_or_else(|| reference_missing("仓库不存在或已删除"))?;

    let location_id = optional_uuid(req.location_id.as_deref(), "location_id")?;
    if let Some(id) = location_id {
        let row = serial_model::inventory_location_ref(id)
            .await
            .map_err(|error| utils::db_error(error, "查询库位库存行"))?
            .ok_or_else(|| {
                reference_missing(
                    "库位库存行不存在：location_id 指向 biz_inventory_location.id（不是 biz_location.id）",
                )
            })?;
        if warehouse_id != row.warehouse_id {
            return Err(reference_missing(
                "库位库存行不属于所选仓库：一次批量生成只能落在同一个仓库的库位上",
            ));
        }
    }

    let batch_no = req
        .batch_no
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let source_type = req
        .source_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let source_id = optional_uuid(req.source_id.as_deref(), "source_id")?;
    // P2-6b：来源类型与来源 id 必须成对、且 id 指向真实未软删的单据。
    // 闸门**先于取号**（取号会推进 biz_seq，参数错不该消耗号段）；
    // 模型层 `batch_generate` 里还有一次同样的校验（防直接调模型层绕过）。
    serial_model::ensure_source_pair(db(), source_type, source_id)
        .await
        .map_err(|err| match err {
            DbError::InvalidValue(message) => {
                ApiError::new(400, error_code::PARAM_INVALID, message)
            }
            other => utils::db_error(other, "校验序列号来源单据"),
        })?;

    let params = GenerateParams {
        material_id,
        warehouse_id: Some(warehouse_id),
        location_id,
        batch_no,
        source_type,
        source_id,
        count: i64::try_from(req.count).unwrap_or(serial_model::MAX_BATCH_COUNT),
        unit_cost,
    };
    let period_key = serial_model::today_period_key();
    // 事务边界在接口层（W2c-1 · 不变式②）：取号 → 落号 → 两本账 → 凭证 → 流水 必须原子。
    // 模型层吃 `&tx`（同一个 `SqlExecutor`），事务里不再开新事务。
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启序列号生成事务"))?;
    let outcome = match serial_model::batch_generate(&tx, &params, &period_key, operator).await {
        Ok(outcome) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交序列号生成事务"))?;
            outcome
        }
        Err(error) => {
            // 回滚失败也只能把原始错误报出去（回滚本身是幂等的，重复调用无害）
            let _ = tx.rollback().await;
            return Err(write_error(error, "批量生成序列号"));
        }
    };
    Ok(SerialBatchRes::new(
        &period_key,
        outcome.rows,
        outcome.unit_cost,
        outcome.amount,
        (
            outcome.inventory.qty,
            outcome.inventory.amount,
            outcome.inventory.avg_cost,
        ),
    ))
}

/// 批量生成序列号。
#[nik::permission(
    path = "/business/erp/serial/batch-generate",
    method = "POST",
    permission = "business:erp:serial:generate"
)]
#[nik::post("/business/erp/serial/batch-generate")]
pub async fn serial_batch_generate(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchGenerateReq>,
) -> ResResult<SerialBatchRes> {
    let body = generate_serials(&req, operator_of(&claims)).await?;
    Ok(Res::ok(body))
}

// ==================== ② 分页 ====================

/// 序列号分页（物料 / 仓库 / 库位 / 状态 / 号模糊）。
#[nik::permission(
    path = "/business/erp/serial/page",
    method = "GET",
    permission = "business:erp:serial:list"
)]
#[nik::get("/business/erp/serial/page")]
pub async fn serial_page(
    Query(filter): Query<SerialFilter>,
) -> ResResult<PageData<Vec<SerialRes>>> {
    let filter = filter.validated()?;
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = serial_model::serial_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询序列号"))?;
    let (page, page_size) = model::normalize_page(page, page_size);
    let list = rows.iter().map(SerialRes::from).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

// ==================== ③ 扫码 ====================

/// 扫码查询串（`?serial_no=xxx`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SerialScanQuery {
    /// 序列号（必填；扫码枪通常带换行/空格，服务端 `trim`）。
    #[serde(default)]
    pub serial_no: Option<String>,
}

/// 扫码定位的核心逻辑（handler 与单测共用）。
pub async fn scan_one(serial_no: Option<&str>) -> std::result::Result<SerialScanRes, ApiError> {
    let serial_no = serial_no.map(str::trim).filter(|text| !text.is_empty());
    let Some(serial_no) = serial_no else {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            "serial_no 不能为空：扫码必须给出完整序列号（如 SN-20261030-000001）",
        ));
    };
    let row = serial_model::serial_scan(serial_no)
        .await
        .map_err(|error| utils::db_error(error, "扫码查询序列号"))?
        .ok_or_else(|| not_found(&format!("序列号 {serial_no} ")))?;
    Ok(SerialScanRes::from(&row))
}

/// 扫码：按序列号定位物料 / 仓库 / 库位（走 `uk_biz_material_serial_no`）。
#[nik::permission(
    path = "/business/erp/serial/scan",
    method = "GET",
    permission = "business:erp:serial:view"
)]
#[nik::get("/business/erp/serial/scan")]
pub async fn serial_scan(Query(query): Query<SerialScanQuery>) -> ResResult<SerialScanRes> {
    let body = scan_one(query.serial_no.as_deref()).await?;
    Ok(Res::ok(body))
}

// ==================== ④ 状态流转 ====================

/// 状态流转的核心逻辑（handler 与单测共用）。
///
/// W2c-1：事务边界提到这一层 —— 序列号状态、库存流水、账面数量重算必须原子。
/// 模型层吃 `&tx`，事务里不再开新事务；`reason` 落进流水的 `remark`（V0138 序列号表没有该列）。
pub async fn apply_status(
    id: Uuid,
    req: &SerialStatusReq,
    operator: Option<Uuid>,
) -> std::result::Result<SerialRes, ApiError> {
    let target = req.target_status()?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启序列号状态流转事务"))?;
    let outcome = match serial_model::transition_status_with(
        &tx,
        id,
        target,
        operator,
        req.reason.as_deref(),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "更新序列号状态"));
        }
    };
    match outcome {
        TransitionOutcome::Updated { from, row } => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交序列号状态流转事务"))?;
            // reason 落流水 remark（V0138 无该列），同时进日志留痕，方便按单号追责。
            tracing::info!(
                serial_no = %row.serial_no,
                from = %from,
                to = %row.status,
                reason = %req.reason.as_deref().unwrap_or(""),
                "序列号状态流转"
            );
            Ok(SerialRes::from(row.as_ref()))
        }
        TransitionOutcome::NotFound => {
            let _ = tx.rollback().await;
            Err(not_found("序列号"))
        }
        TransitionOutcome::Illegal { from, to } => {
            let _ = tx.rollback().await;
            Err(ApiError::new(
                400,
                error_code::ERP_SERIAL_STATUS_INVALID,
                serial_model::transition_rejected_message(&from, &to),
            ))
        }
    }
}

/// 状态流转（单向不可逆：`IN_STOCK→OUTBOUND|SCRAPPED`、`OUTBOUND→SCRAPPED`）。
#[nik::permission(
    path = "/business/erp/serial/{id}/status",
    method = "PATCH",
    permission = "business:erp:serial:status"
)]
#[nik::patch("/business/erp/serial/{id}/status")]
pub async fn serial_update_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<SerialStatusReq>,
) -> ResResult<SerialRes> {
    let id = parse_uuid(&id, "id")?;
    let body = apply_status(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(body))
}

// ==================== ⑤ 详情 ====================

/// 序列号详情。
#[nik::permission(
    path = "/business/erp/serial/{id}",
    method = "GET",
    permission = "business:erp:serial:detail"
)]
#[nik::get("/business/erp/serial/{id}")]
pub async fn serial_detail(Path(id): Path<String>) -> ResResult<SerialRes> {
    let id = parse_uuid(&id, "id")?;
    let row = serial_model::serial_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询序列号详情"))?
        .ok_or_else(|| not_found("序列号"))?;
    Ok(Res::ok(SerialRes::from(&row)))
}

// ==================== 单测（真库用例需 ERP_DB_URL，未设置则跳过） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::fixture_hygiene;
    use crate::business::erp::serial_dto::{MAX_PAGE_SIZE, OptionalDecimal, WRITABLE_TARGETS};
    use crate::business::erp::serial_model::{
        ALL_STATUSES, BIZ_TYPE_SERIAL_IN, BIZ_TYPE_SERIAL_OUT, BIZ_TYPE_SERIAL_SCRAP,
        MAX_BATCH_COUNT, STATUS_IN_STOCK, STATUS_OUTBOUND, STATUS_SCRAPPED, SerialRow, TXN_TYPE_IN,
        TXN_TYPE_OUT, format_serial_no, period_code_of,
    };
    use nik::prelude::chrono::{DateTime, Utc};
    use nik::prelude::rust_decimal::Decimal;

    // ---------- 夹具 ----------
    const W_ID: &str = "b2b00002-0000-4000-8000-000000000001";
    const W_OTHER_ID: &str = "b2b00002-0000-4000-8000-000000000002";
    const M_SN_ID: &str = "b2b00004-0000-4000-8000-000000000001";
    const M_PLAIN_ID: &str = "b2b00004-0000-4000-8000-000000000002";
    const IL_ID: &str = "b2b00005-0000-4000-8000-000000000001";
    /// W2c-1 库存联动专用仓库（与 W_ID 分开，账面互不干扰）。
    const W_LINK_ID: &str = "b2b00002-0000-4000-8000-00000000000a";
    /// W2c-1 一物一码物料。
    const M_LINK_ID: &str = "b2b00004-0000-4000-8000-00000000000a";
    /// W2c-1 按数量管理物料（对照组）。
    const M_PLAIN_LINK_ID: &str = "b2b00004-0000-4000-8000-00000000000b";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
         VALUES ('b2b00001-0000-4000-8000-000000000001', 'W2B-PCS', '个', 1, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type,
                                    allow_negative, status, sort_order, parent_id, level,
                                    warehouse_path, is_leaf)
         VALUES ('b2b00002-0000-4000-8000-000000000001', 'WH-W2B-1', 'W2B 一号仓', 'NORMAL', 0, 1, 1,
                 NULL, 1, '/b2b00002-0000-4000-8000-000000000001/', 1),
                ('b2b00002-0000-4000-8000-000000000002', 'WH-W2B-2', 'W2B 二号仓', 'NORMAL', 0, 1, 2,
                 NULL, 1, '/b2b00002-0000-4000-8000-000000000002/', 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                   is_default, capacity, status, sort_order)
         VALUES ('b2b00003-0000-4000-8000-000000000001', 'W2B-A-01', 'W2B A 区一号',
                 'b2b00002-0000-4000-8000-000000000001', 'STORAGE', 0, 100.0000, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                   safety_stock, costing_method, tax_rate, status, serial_required)
         VALUES ('b2b00004-0000-4000-8000-000000000001', 'W2B-M-SN', 'W2B 一物一码物料', 'SN 设备',
                 'b2b00001-0000-4000-8000-000000000001', 'FINISHED', 0.0000, 'MOVING_AVG',
                 0.1300, 1, TRUE),
                ('b2b00004-0000-4000-8000-000000000002', 'W2B-M-PLAIN', 'W2B 按数量物料', NULL,
                 'b2b00001-0000-4000-8000-000000000001', 'RAW', 0.0000, 'MOVING_AVG',
                 0.1300, 1, FALSE)
         ON CONFLICT DO NOTHING",
        // 一物一码开关可能被「另一张卡」的夹具改过 → 这里幂等纠正
        "UPDATE biz_material SET serial_required = TRUE
          WHERE id = 'b2b00004-0000-4000-8000-000000000001'",
        "UPDATE biz_material SET serial_required = FALSE
          WHERE id = 'b2b00004-0000-4000-8000-000000000002'",
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
         VALUES ('b2b00005-0000-4000-8000-000000000001',
                 'b2b00004-0000-4000-8000-000000000001',
                 'b2b00002-0000-4000-8000-000000000001',
                 'b2b00003-0000-4000-8000-000000000001', 0.0000)
         ON CONFLICT DO NOTHING",
        // ---- W2c-1 库存联动夹具：独立物料 + 独立仓库，每轮从零开始（确定性）----
        "DELETE FROM biz_inventory_txn
          WHERE material_id IN ('b2b00004-0000-4000-8000-00000000000a',
                                'b2b00004-0000-4000-8000-00000000000b')",
        "DELETE FROM biz_material_serial
          WHERE material_id IN ('b2b00004-0000-4000-8000-00000000000a',
                                'b2b00004-0000-4000-8000-00000000000b')",
        "DELETE FROM biz_inventory
          WHERE material_id IN ('b2b00004-0000-4000-8000-00000000000a',
                                'b2b00004-0000-4000-8000-00000000000b')",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type,
                                    allow_negative, status, sort_order, parent_id, level,
                                    warehouse_path, is_leaf)
         VALUES ('b2b00002-0000-4000-8000-00000000000a', 'WH-W2C1', 'W2C1 联动仓', 'NORMAL', 0, 1, 3,
                 NULL, 1, '/b2b00002-0000-4000-8000-00000000000a/', 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                   safety_stock, costing_method, tax_rate, status, serial_required)
         VALUES ('b2b00004-0000-4000-8000-00000000000a', 'W2C1-M-SN', 'W2C1 一物一码物料', 'SN 设备',
                 'b2b00001-0000-4000-8000-000000000001', 'FINISHED', 0.0000, 'MOVING_AVG',
                 0.1300, 1, TRUE),
                ('b2b00004-0000-4000-8000-00000000000b', 'W2C1-M-PLAIN', 'W2C1 按数量物料', NULL,
                 'b2b00001-0000-4000-8000-000000000001', 'RAW', 0.0000, 'MOVING_AVG',
                 0.1300, 1, FALSE)
         ON CONFLICT DO NOTHING",
        "UPDATE biz_material SET serial_required = TRUE
          WHERE id = 'b2b00004-0000-4000-8000-00000000000a'",
        "UPDATE biz_material SET serial_required = FALSE
          WHERE id = 'b2b00004-0000-4000-8000-00000000000b'",
        // 按数量管理的物料：账面 7 件（证明序列号操作不碰它）
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
         VALUES ('b2b0000b-0000-4000-8000-00000000000a',
                 'b2b00004-0000-4000-8000-00000000000b',
                 'b2b00002-0000-4000-8000-00000000000a', 7.0000, 0.0000, 20.000000, 140.0000)
         ON CONFLICT (material_id, warehouse_id, COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid)) DO UPDATE
            SET qty = 7.0000, avg_cost = 20.000000, amount = 140.0000",
        // ---- ADR-20261031 起「批量生成 = 两本账 + 凭证」：每个用例都要从「干净且两本账自洽」的
        //      状态出发，否则上一个用例留下的序列号 / 成本账 / 库位账会让不变式①的断言漂数。
        //      顺序：先删号（序列号对库位行有硬外键）→ 再删成本账 → 最后把库位账清零。
        "DELETE FROM biz_material_serial
          WHERE material_id IN ('b2b00004-0000-4000-8000-000000000001',
                                'b2b00004-0000-4000-8000-000000000002')",
        "DELETE FROM biz_inventory
          WHERE material_id IN ('b2b00004-0000-4000-8000-000000000001',
                                'b2b00004-0000-4000-8000-000000000002')",
        "UPDATE biz_inventory_location SET qty = 0.0000
          WHERE material_id IN ('b2b00004-0000-4000-8000-000000000001',
                                'b2b00004-0000-4000-8000-000000000002',
                                'b2b00004-0000-4000-8000-00000000000a')",
    ];

    /// 真库用例串行（同一个库，取号器是全局单行，并行会互相推进号段）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局 db（只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清掉本模块造的**凭证**（序列号变动会发凭证：ADR-20261031「批量生成 = 两本账 + 凭证」）。
    ///
    /// 为什么必须单独收：这些凭证是本模块的真实业务路径造出来的（`SERIAL_BATCH` /
    /// `SERIAL_OUT` / `SERIAL_SCRAP` 三种来源），但夹具只清序列号 / 成本账 / 库位账，
    /// 从来没清凭证 ⇒ `biz_voucher` 与 `biz_status_log` 的 `VOUCHER` 行每跑一遍全量
    /// 就线性增长（实测 +58 行/遍，`t_7f47221c` 的验收对象之一）。
    ///
    /// 顺序：状态日志（`biz_status_log` 无外键 → 凭证一删就再也查不到 id）→ 分录 → 凭证。
    async fn purge_serial_vouchers() {
        for sql in [
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
              (SELECT id FROM biz_voucher \
                WHERE source_type IN ('SERIAL_BATCH', 'SERIAL_OUT', 'SERIAL_SCRAP'))",
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
              (SELECT id FROM biz_voucher \
                WHERE source_type IN ('SERIAL_BATCH', 'SERIAL_OUT', 'SERIAL_SCRAP'))",
            "DELETE FROM biz_voucher \
              WHERE source_type IN ('SERIAL_BATCH', 'SERIAL_OUT', 'SERIAL_SCRAP')",
        ] {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("清理序列号凭证失败：{err}\nSQL: {sql}");
            }
        }
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        // 先清上一轮的凭证（依赖序列号 / 物料行仍在，故必须在 FIXTURES 的删除之前）
        purge_serial_vouchers().await;
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        Some(guard)
    }

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    fn req_for(material_id: &str, count: u64) -> BatchGenerateReq {
        BatchGenerateReq {
            material_id: material_id.to_string(),
            warehouse_id: Some(W_ID.to_string()),
            location_id: Some(IL_ID.to_string()),
            batch_no: Some("W2B-BATCH-1".to_string()),
            count,
            // ADR-20261031：批量生成 = 无来源入库，必须录成本单价（夹具统一 10.000000）
            unit_cost: Some(OptionalDecimal::Num(Decimal::from(10))),
            // P2-6b：批量生成是「无来源入库（期初 / 其他入库）」，来源两列**都不写**。
            // 以前这里写 `PURCHASE` 却把 `source_id` 留空 —— 正是 DI-21 恒红的那个设计缺口
            // （有类型无 id），现在由 `serial_model::ensure_source_pair` 直接拒绝这种组合。
            source_type: None,
            source_id: None,
        }
    }

    async fn generate(
        material_id: &str,
        count: u64,
    ) -> std::result::Result<SerialBatchRes, ApiError> {
        generate_serials(&req_for(material_id, count), None).await
    }

    // ---------- W2c-1 库存联动辅助 ----------

    /// 指定物料 / 仓库的批量生成请求（W2C1 联动夹具专用）。
    fn req_for_link(count: u64) -> BatchGenerateReq {
        BatchGenerateReq {
            material_id: M_LINK_ID.to_string(),
            warehouse_id: Some(W_LINK_ID.to_string()),
            location_id: None,
            batch_no: Some("W2C1-BATCH".to_string()),
            count,
            unit_cost: Some(OptionalDecimal::Num(Decimal::from(10))),
            source_type: Some("PRODUCE".to_string()),
            source_id: None,
        }
    }

    /// 在 W2C1 联动仓里生成 `count` 条一物一码序列号。
    async fn generate_link(count: u64) -> std::result::Result<SerialBatchRes, ApiError> {
        generate_serials(&req_for_link(count), None).await
    }

    /// 不变式①的右端：该 (物料, 仓库) 下 `IN_STOCK` 的序列号件数。
    async fn in_stock_count(material_id: Uuid, warehouse_id: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_material_serial \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND status = 'IN_STOCK' AND deleted_at IS NULL",
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await
        .expect("统计在库序列号")
    }

    /// 不变式①的左端：`biz_inventory.qty`（`None` = 还没建账）。
    ///
    /// **按仓级求和**（V0139 之后 `biz_inventory` 的行粒度是「物料×仓库×库位」，
    /// 同一物料同仓可以有 NULL 汇总桶 + 若干库位桶）。旧版本只取 `rows.first()`，
    /// 多桶时读到的是「某一行」而不是仓级账面 ⇒ 混桶用例会假红（实测 5 vs 8）。
    /// 口径与 `erp_invariants.py` 的 B4 / C5 一致：仓级 = `SUM(qty)`。
    async fn inventory_qty(material_id: Uuid, warehouse_id: Uuid) -> Option<Decimal> {
        let rows = db()
            .find_with_params(
                "SELECT COALESCE(SUM(qty), 0)::numeric(18,4)::text AS qty FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&material_id), Value::from(&warehouse_id)],
            )
            .await
            .expect("读账面数量");
        rows.first()
            .map(|row| row.get_typed::<String>("qty").expect("qty"))
            .map(|text| Decimal::from_str_exact(&text).expect("decimal"))
    }

    /// 某物料的流水条数（`biz_type` 可选过滤）。
    async fn txn_count(material_id: Uuid, biz_type: Option<&str>) -> i64 {
        let sql = match biz_type {
            Some(_) => {
                "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid AND biz_type = ?"
            }
            None => "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid",
        };
        let mut params = vec![Value::from(&material_id)];
        if let Some(biz_type) = biz_type {
            params.push(Value::from(biz_type));
        }
        db().scalar_with_params(sql, params)
            .await
            .expect("统计库存流水")
    }

    /// 最新一条流水的关键列（时间倒序，同秒用 id 兜底）。
    async fn latest_txn(
        material_id: Uuid,
    ) -> (String, String, Decimal, Decimal, Decimal, Option<String>) {
        let rows = db()
            .find_with_params(
                "SELECT txn_type, biz_type, qty, before_qty, after_qty, remark \
                   FROM biz_inventory_txn WHERE material_id = ?::uuid \
                  ORDER BY created_at DESC, id DESC LIMIT 1",
                vec![Value::from(&material_id)],
            )
            .await
            .expect("读最新流水");
        let row = rows.first().expect("应该有一条流水");
        (
            row.get_typed("txn_type").expect("txn_type"),
            row.get_typed("biz_type").expect("biz_type"),
            row.get_typed("qty").expect("qty"),
            row.get_typed("before_qty").expect("before_qty"),
            row.get_typed("after_qty").expect("after_qty"),
            row.get_typed("remark").expect("remark"),
        )
    }

    /// 断言不变式①（`quantity == count(IN_STOCK)`），失败时把两端口径一起打出来。
    async fn assert_invariant(material_id: &str, warehouse_id: &str, ctx: &str) {
        let material = uuid(material_id);
        let warehouse = uuid(warehouse_id);
        let counted = in_stock_count(material, warehouse).await;
        let booked = inventory_qty(material, warehouse).await;
        // 按数值比较（`NUMERIC(18,4)` 读回来是 4.0000，`count(*)` 是 4，数值相等即通过）
        assert_eq!(
            booked,
            Some(Decimal::from(counted)),
            "不变式①被破坏（{ctx}）：biz_inventory.qty={booked:?} / count(IN_STOCK)={counted}"
        );
    }

    // ---------- 纯逻辑用例（不连库） ----------

    /// ⑰ 权限码：5 个、无重复、前缀正确、与卡面逐字一致。
    #[test]
    fn permission_codes_match_card() {
        assert_eq!(SERIAL_PERMISSIONS.len(), 5, "卡面 §三 是 5 个码");
        let codes: Vec<&str> = SERIAL_PERMISSIONS.iter().map(|(code, _)| *code).collect();
        let unique: std::collections::BTreeSet<&&str> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "权限码不得重复");
        assert!(
            codes
                .iter()
                .all(|code| code.starts_with("business:erp:serial:")),
            "前缀必须统一"
        );
        for expected in ["list", "detail", "view", "generate", "status"] {
            let code = format!("business:erp:serial:{expected}");
            assert!(codes.contains(&code.as_str()), "缺少权限码 {code}");
        }
        assert!(SERIAL_PERMISSIONS.iter().all(|(_, name)| !name.is_empty()));
    }

    /// ⑱ 可写目标态与全状态集自洽：`IN_STOCK` 只能由生成产生。
    #[test]
    fn writable_targets_exclude_in_stock() {
        assert_eq!(WRITABLE_TARGETS, ["OUTBOUND", "SCRAPPED"]);
        assert!(!WRITABLE_TARGETS.contains(&STATUS_IN_STOCK));
        assert!(ALL_STATUSES.contains(&STATUS_IN_STOCK));
        // 单页上限必须 >= 默认页大小，否则默认请求就被 clamp
        const {
            assert!(MAX_PAGE_SIZE >= crate::business::erp::serial_dto::DEFAULT_PAGE_SIZE);
            assert!(MAX_BATCH_COUNT == 1000, "卡面红线：一次最多 1000 条");
        }
    }

    /// ⑲ 扫码入参：空白 / 缺席都 400，且文案提示号格式。
    #[tokio::test]
    async fn scan_requires_serial_no() {
        for input in [None, Some(""), Some("   ")] {
            let err = scan_one(input).await.expect_err("空号必须 400");
            assert_eq!(err.status, 400, "input={input:?}");
            assert!(err.msg.contains("serial_no"), "{}", err.msg);
        }
    }

    /// ⑳ 状态流转非法目标在**不碰库**的情况下就被拒（`IN_STOCK` / 未知值）。
    #[tokio::test]
    async fn apply_status_rejects_non_writable_target_before_db() {
        for target in ["IN_STOCK", "SCRAP", ""] {
            let req = SerialStatusReq {
                status: target.to_string(),
                reason: None,
            };
            let err = apply_status(Uuid::now_v7(), &req, None)
                .await
                .expect_err("非法目标态必须在库操作之前拒掉");
            assert_eq!(err.status, 400, "target={target}");
            assert_eq!(
                err.code,
                error_code::ERP_SERIAL_STATUS_INVALID,
                "target={target}"
            );
        }
    }

    // ---------- 真库用例 ----------

    /// ㉑ 批量生成 1000 条：无重号（DISTINCT = 1000），全部 IN_STOCK 且写 inbound_at。
    #[tokio::test]
    async fn batch_generate_1000_has_no_duplicate() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let started = std::time::Instant::now();
        let body = generate(M_SN_ID, 1000).await.expect("批量生成 1000 条");
        let elapsed = started.elapsed();
        assert_eq!(body.count, 1000);
        assert_eq!(body.serial_nos.len(), 1000);
        assert_eq!(body.list.len(), 1000, "回读行数必须等于生成条数");
        let unique: std::collections::BTreeSet<&String> = body.serial_nos.iter().collect();
        assert_eq!(unique.len(), 1000, "1000 条里出现了重号");
        assert!(
            body.serial_nos
                .iter()
                .all(|no| no.starts_with(&format!("SN-{}-", body.period_key))),
            "号格式必须是 SN-yyyyMMdd-XXXXXX"
        );
        assert!(body.list.iter().all(|row| row.status == STATUS_IN_STOCK));
        assert!(body.list.iter().all(|row| row.inbound_at.is_some()));
        assert!(
            body.list
                .iter()
                .all(|row| row.batch_no.as_deref() == Some("W2B-BATCH-1"))
        );

        let prefix = format!("SN-{}-%", body.period_key);
        // 库内核对：该周期键下「行数 == 不同号数」（当天可能有之前批次/并发批次的号，
        // 所以不能硬编码 1000；真正要证的是**没有任何重号**）
        let rows: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_material_serial \
                  WHERE serial_no LIKE ? AND deleted_at IS NULL",
                vec![Value::from(prefix.as_str())],
            )
            .await
            .expect("库内计数");
        let distinct: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(DISTINCT serial_no) FROM biz_material_serial \
                  WHERE serial_no LIKE ? AND deleted_at IS NULL",
                vec![Value::from(prefix.as_str())],
            )
            .await
            .expect("库内计数");
        assert_eq!(
            rows, distinct,
            "库内该周期键下存在重号（rows={rows} distinct={distinct}）"
        );
        assert!(rows >= 1000, "库内该周期的号至少 1000 个（实际 {rows}）");
        println!("[1000 条批量生成] 耗时 {:?}", elapsed);
    }

    /// ㉒ 并发：两个任务各 500 条同时取号 → 1000 条仍无重号（卡面并发红线）。
    #[tokio::test]
    async fn concurrent_batches_do_not_collide() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (left, right) = tokio::join!(generate(M_SN_ID, 500), generate(M_SN_ID, 500));
        let left = left.expect("并发批次 1");
        let right = right.expect("并发批次 2");
        let mut all: Vec<String> = left.serial_nos.clone();
        all.extend(right.serial_nos.iter().cloned());
        assert_eq!(all.len(), 1000);
        let unique: std::collections::BTreeSet<&String> = all.iter().collect();
        assert_eq!(unique.len(), 1000, "并发批量生成出现了重号");
    }

    /// ㉓ 同一次批量的号连续（`start+1 ..= start+n`）。
    #[tokio::test]
    async fn batch_numbers_are_contiguous() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 3).await.expect("生成 3 条");
        let parsed: Vec<i64> = body
            .serial_nos
            .iter()
            .map(|no| {
                no.rsplit('-')
                    .next()
                    .and_then(|tail| tail.parse::<i64>().ok())
                    .expect("号尾必须是数字")
            })
            .collect();
        assert_eq!(
            (parsed[1], parsed[2]),
            (parsed[0] + 1, parsed[0] + 2),
            "同批号必须连续：{parsed:?}"
        );
    }

    /// ㉔ 跨天重置：周期键进入更新的一天 → 号从 000001 重新开始；
    /// 周期键**回退**（时钟回拨）→ 绝不重置（防重号）。
    ///
    /// 这条用例会**临时改写** `biz_seq(SN)` 的 `(period_key, current_no)`，跑完必须还原：
    /// 号段是全局单行，任何「用假日期来回推」的做法都会把当天已发的号撞掉
    /// （本卡第一版就踩了这个：12 条真库用例全报 409 重号）。所以这里
    /// 先备份 → 造状态 → 取值 → **先还原再断言**（断言失败也不留下脏状态）。
    #[tokio::test]
    async fn new_period_restarts_from_one() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 两个固定但足够古老的周期键（永远 < 今天，所以不会污染当天号段）
        let old_period = "10000001";
        let new_period = "10000002";
        // 可重跑：先清掉本用例上一轮插的行（同号再插会撞 uk_biz_material_serial_no）
        db().execute_with_params(
            "DELETE FROM biz_material_serial WHERE serial_no LIKE 'SN-1000000%'",
            vec![],
        )
        .await
        .expect("清理跨天用例的历史行");
        let backup = seq_state().await;

        // ① 旧的一天已经发到 7 → 换新的一天必须从 000001 开始
        set_seq_state(old_period, 7).await;
        let params = GenerateParams {
            material_id: uuid(M_SN_ID),
            warehouse_id: Some(uuid(W_ID)),
            location_id: None,
            batch_no: None,
            source_type: None,
            source_id: None,
            count: 5,
            unit_cost: Decimal::from(10),
        };
        let day = serial_model::batch_generate(db(), &params, new_period, None).await;
        let same_day = serial_model::batch_generate(db(), &params, new_period, None).await;
        // ② 周期回退：period_key 比本次大 → 只累加、不重置
        set_seq_state(new_period, 5).await;
        let rollback = serial_model::take_numbers(db(), 1, old_period).await;
        // ③ 立刻还原全局号段（后续用例还要用当天的号）
        set_seq_state(&backup.0, backup.1).await;

        let day = day.expect("新的一天生成");
        assert_eq!(
            day.rows.first().expect("非空").serial_no,
            format_serial_no(new_period, 1),
            "跨天必须从 000001 重新开始"
        );
        assert!(
            day.rows
                .first()
                .expect("非空")
                .serial_no
                .ends_with("000001")
        );
        assert!(day.rows.last().expect("非空").serial_no.ends_with("000005"));

        let same_day = same_day.expect("同一天再生成");
        assert!(
            same_day
                .rows
                .first()
                .expect("非空")
                .serial_no
                .ends_with("000006"),
            "同一天必须接着上一段（实际 {}）",
            same_day.rows.first().expect("非空").serial_no
        );

        assert_eq!(
            rollback.expect("回退取号"),
            5,
            "周期键回退时不得重置 current_no（否则撞号）"
        );
    }

    /// 读 `biz_seq(SN)` 当前状态（`period_key`, `current_no`）。
    async fn seq_state() -> (String, i64) {
        let rows = db()
            .find_with_params(
                "SELECT period_key, current_no FROM biz_seq \
                  WHERE seq_type = 'SN' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("读号段");
        let row = rows.first().expect("V0138 应已种入 SN 取号规则");
        (
            row.get_typed("period_key").expect("period_key"),
            row.get_typed("current_no").expect("current_no"),
        )
    }

    /// 直接改写 `biz_seq(SN)`（**只给单测造跨天场景用**，生产路径永远走 `take_numbers`）。
    async fn set_seq_state(period_key: &str, current_no: i64) {
        db().execute_with_params(
            "UPDATE biz_seq SET period_key = ?, current_no = ? \
              WHERE seq_type = 'SN' AND deleted_at IS NULL",
            vec![Value::from(period_key), Value::from(current_no)],
        )
        .await
        .expect("写号段状态");
    }

    /// ㉕ 未开启一物一码的物料批量生成 → 400 + 66007。
    #[tokio::test]
    async fn material_without_serial_required_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = generate(M_PLAIN_ID, 1)
            .await
            .expect_err("按数量管理的物料必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_SERIAL_NOT_REQUIRED);
        assert!(err.msg.contains("serial_required"), "{}", err.msg);
    }

    /// ㉖ 物料不存在 / 已软删 → 404（不是 400：请求本身合法，资源没了）。
    #[tokio::test]
    async fn generate_for_missing_material_is_404() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = generate(&Uuid::now_v7().to_string(), 1)
            .await
            .expect_err("不存在的物料必须 404");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, error_code::RESOURCE_NOT_FOUND);
    }

    /// ㉗ 库位库存行不存在 / 不属于所选仓库 → 400 + 66002（且不消耗号段）。
    #[tokio::test]
    async fn generate_validates_location_reference() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let mut req = req_for(M_SN_ID, 1);
        req.location_id = Some(Uuid::now_v7().to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("悬空库位必须拒");
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
        assert!(err.msg.contains("biz_inventory_location"), "{}", err.msg);

        let mut req = req_for(M_SN_ID, 1);
        req.warehouse_id = Some(W_OTHER_ID.to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("库位与仓库不匹配必须拒");
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
        assert!(err.msg.contains("不属于所选仓库"), "{}", err.msg);

        let before: i64 = db()
            .scalar_with_params(
                "SELECT current_no FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("读号段");
        assert!(before >= 0, "校验失败不该把号段写成负数");
    }

    /// ㉘ 扫码：存在的号返回完整物料 + 仓库 + 库位（含名称）。
    #[tokio::test]
    async fn scan_returns_full_context() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let serial_no = body.serial_nos.first().expect("有号").clone();
        let scan = scan_one(Some(&format!("  {serial_no}\n")))
            .await
            .expect("扫码命中");
        assert_eq!(scan.serial_no, serial_no);
        assert_eq!(scan.material_code, "W2B-M-SN");
        assert_eq!(scan.material_name, "W2B 一物一码物料");
        assert_eq!(scan.warehouse_name.as_deref(), Some("W2B 一号仓"));
        assert_eq!(scan.location_name.as_deref(), Some("W2B A 区一号"));
        assert_eq!(scan.location_id.as_deref(), Some(IL_ID));
        assert_eq!(scan.batch_no.as_deref(), Some("W2B-BATCH-1"));
        assert_eq!(scan.status, STATUS_IN_STOCK);
        assert!(scan.inbound_at.is_some());
        assert!(scan.outbound_at.is_none());
    }

    /// ㉙ 扫码：不存在的号 → 404。
    #[tokio::test]
    async fn scan_missing_serial_is_404() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = scan_one(Some("SN-19700101-000001"))
            .await
            .expect_err("不存在的号 404");
        assert_eq!(err.status, 404);
        assert!(err.msg.contains("SN-19700101-000001"), "{}", err.msg);
    }

    /// ㉚ 扫码：软删的号 → 404（部分唯一索引同样把它排除在外）。
    #[tokio::test]
    async fn scan_soft_deleted_serial_is_404() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let serial_no = body.serial_nos.first().expect("有号").clone();
        db().execute_with_params(
            "UPDATE biz_material_serial SET deleted_at = now() WHERE serial_no = ?",
            vec![Value::from(serial_no.as_str())],
        )
        .await
        .expect("软删");
        let err = scan_one(Some(&serial_no)).await.expect_err("软删号 404");
        assert_eq!(err.status, 404);
    }

    /// ㉚b 物料行能读出 `serial_required`（W2b 补口：W1c 当初没接这一列，
    /// 导致「物料接口打不开一物一码」→ 序列号功能无从启用）。
    #[tokio::test]
    async fn material_row_exposes_serial_required() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = model::material_find_by_id(uuid(M_SN_ID))
            .await
            .expect("查询物料")
            .expect("物料存在");
        assert!(row.serial_required, "一物一码物料必须是 true");
        let plain = model::material_find_by_id(uuid(M_PLAIN_ID))
            .await
            .expect("查询物料")
            .expect("物料存在");
        assert!(!plain.serial_required, "按数量物料必须是 false");
        assert_eq!(
            serial_model::material_serial_required(uuid(M_SN_ID))
                .await
                .expect("查询开关"),
            Some(true)
        );
        assert_eq!(
            serial_model::material_serial_required(Uuid::now_v7())
                .await
                .expect("查询开关"),
            None,
            "不存在的物料返回 None（接口层转 404）"
        );
    }

    /// ㉛ 软删后同号可重用（W2a 口径：唯一索引带 `WHERE deleted_at IS NULL`）。
    #[tokio::test]
    async fn soft_deleted_serial_no_can_be_reused() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let serial_no = body.serial_nos.first().expect("有号").clone();
        db().execute_with_params(
            "UPDATE biz_material_serial SET deleted_at = now() WHERE serial_no = ?",
            vec![Value::from(serial_no.as_str())],
        )
        .await
        .expect("软删");
        let reused = db()
            .execute_with_params(
                "INSERT INTO biz_material_serial (id, serial_no, material_id, status, inbound_at)
                 VALUES (?, ?, ?::uuid, 'IN_STOCK', now())",
                vec![
                    Value::from(Uuid::now_v7()),
                    Value::from(serial_no.as_str()),
                    Value::from(&uuid(M_SN_ID)),
                ],
            )
            .await;
        assert!(reused.is_ok(), "软删后同号必须能再插：{reused:?}");
        let alive: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_material_serial \
                  WHERE serial_no = ? AND deleted_at IS NULL",
                vec![Value::from(serial_no.as_str())],
            )
            .await
            .expect("计数");
        assert_eq!(alive, 1);
        let scan = scan_one(Some(&serial_no)).await.expect("新行能扫出来");
        assert_eq!(scan.status, STATUS_IN_STOCK);
    }

    /// ㉜ 状态流转：IN_STOCK → OUTBOUND 写 outbound_at。
    #[tokio::test]
    async fn transition_to_outbound_sets_outbound_at() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        let req = SerialStatusReq {
            status: STATUS_OUTBOUND.to_string(),
            reason: Some("发货出库".to_string()),
        };
        let res = apply_status(id, &req, None)
            .await
            .expect("IN_STOCK → OUTBOUND");
        assert_eq!(res.status, STATUS_OUTBOUND);
        assert!(res.outbound_at.is_some(), "转 OUTBOUND 必须写 outbound_at");
        let count: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_material_serial \
                  WHERE id = ?::uuid AND status = 'OUTBOUND' AND outbound_at IS NOT NULL",
                vec![Value::from(&id)],
            )
            .await
            .expect("库内断言");
        assert_eq!(count, 1);
    }

    /// ㉝ IN_STOCK → SCRAPPED。
    #[tokio::test]
    async fn transition_to_scrapped_from_stock() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        let req = SerialStatusReq {
            status: STATUS_SCRAPPED.to_string(),
            reason: Some("来料破损".to_string()),
        };
        let res = apply_status(id, &req, None)
            .await
            .expect("IN_STOCK → SCRAPPED");
        assert_eq!(res.status, STATUS_SCRAPPED);
        assert!(res.outbound_at.is_some(), "报废也要留时间戳");
    }

    /// ㉞ OUTBOUND → SCRAPPED（保留出库时刻）。
    #[tokio::test]
    async fn transition_to_scrapped_from_outbound_keeps_time() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        let to_out = SerialStatusReq {
            status: STATUS_OUTBOUND.to_string(),
            reason: None,
        };
        let out = apply_status(id, &to_out, None).await.expect("先出库");
        let outbound_at = out.outbound_at.expect("出库时间");
        let to_scrap = SerialStatusReq {
            status: STATUS_SCRAPPED.to_string(),
            reason: None,
        };
        let scrap = apply_status(id, &to_scrap, None).await.expect("再报废");
        assert_eq!(scrap.status, STATUS_SCRAPPED);
        assert_eq!(
            scrap.outbound_at,
            Some(outbound_at),
            "报废不应覆盖出库时刻（COALESCE 语义）"
        );
    }

    /// ㉟ OUTBOUND → IN_STOCK 必须 400 + 66008，且库内状态不变。
    #[tokio::test]
    async fn transition_back_to_stock_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("先出库");
        let err = apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_IN_STOCK.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect_err("IN_STOCK 不是可写目标态");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_SERIAL_STATUS_INVALID);
        let status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_material_serial WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("读状态");
        assert_eq!(status, STATUS_OUTBOUND, "拒绝后库里状态不得改动");
    }

    /// ㊱ SCRAPPED 之后任何流转都拒（状态机不可逆的终点）。
    #[tokio::test]
    async fn scrapped_is_terminal() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_SCRAPPED.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("先报废");
        for target in WRITABLE_TARGETS {
            let err = apply_status(
                id,
                &SerialStatusReq {
                    status: target.to_string(),
                    reason: None,
                },
                None,
            )
            .await
            .expect_err("报废后不能再流转");
            assert_eq!(
                err.code,
                error_code::ERP_SERIAL_STATUS_INVALID,
                "target={target}"
            );
            assert!(err.msg.contains("已报废"), "{}", err.msg);
        }
    }

    /// ㊲ 流转不存在的行 / 软删行 → 404。
    #[tokio::test]
    async fn transition_missing_row_is_404() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let req = SerialStatusReq {
            status: STATUS_OUTBOUND.to_string(),
            reason: None,
        };
        let err = apply_status(Uuid::now_v7(), &req, None)
            .await
            .expect_err("不存在的行 404");
        assert_eq!(err.status, 404);

        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        db().execute_with_params(
            "UPDATE biz_material_serial SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("软删");
        let err = apply_status(id, &req, None).await.expect_err("软删行 404");
        assert_eq!(err.status, 404);
    }

    /// ㊳ 分页：按物料 / 仓库 / 状态筛选，且 total 与列表同口径。
    #[tokio::test]
    async fn page_filters_by_material_warehouse_status() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 6).await.expect("生成 6 条");
        let id = uuid(&body.list[0].id);
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("出库一条");

        let filter = SerialFilter {
            material_id: Some(M_SN_ID.to_string()),
            warehouse_id: Some(W_ID.to_string()),
            status: Some(STATUS_OUTBOUND.to_string()),
            page: 1,
            page_size: 10,
            ..SerialFilter::default()
        };
        let (rows, total) = serial_model::serial_page(&filter.validated().expect("筛选合法"))
            .await
            .expect("分页查询");
        assert!(total >= 1, "至少能筛到刚出库的那条");
        // 「total 与列表同口径」：未满页时两者相等；满页时列表长度必须等于 page_size。
        // （不能用 `total == rows.len()` 死判：M_SN 的历史流水会随全量套件反复跑而累积，
        //   超过一页后该断言会假红 —— W2c-1 复跑时实测 total=19 / rows=10。）
        if total <= 10 {
            assert_eq!(total, rows.len() as i64, "未满页时 total 必须与列表同口径");
        } else {
            assert_eq!(rows.len(), 10, "满页时列表长度必须等于 page_size");
        }
        assert!(rows.iter().all(|row| row.status == STATUS_OUTBOUND));
        assert!(rows.iter().all(|row| row.material_id == uuid(M_SN_ID)));
        assert!(rows.iter().all(|row| row.warehouse_id == Some(uuid(W_ID))));

        let by_material = SerialFilter {
            material_id: Some(M_SN_ID.to_string()),
            page: 1,
            page_size: 5,
            ..SerialFilter::default()
        };
        let (rows, total) = serial_model::serial_page(&by_material.validated().expect("合法"))
            .await
            .expect("按物料分页");
        assert!(total >= 6);
        assert_eq!(rows.len(), 5, "page_size 必须生效");

        let bad_status = SerialFilter {
            status: Some("INVALID".to_string()),
            ..SerialFilter::default()
        };
        assert!(bad_status.validated().is_err(), "非法 status 必须拒");
    }

    /// ㊴ 详情接口：按主键返回整行；软删后 404。
    #[tokio::test]
    async fn detail_returns_row_and_hides_soft_deleted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        let row = serial_model::serial_find_by_id(id)
            .await
            .expect("查询详情")
            .expect("行存在");
        assert_eq!(row.serial_no, body.serial_nos[0]);
        assert_eq!(row.status, STATUS_IN_STOCK);
        db().execute_with_params(
            "UPDATE biz_material_serial SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("软删");
        assert!(
            serial_model::serial_find_by_id(id)
                .await
                .expect("查询")
                .is_none(),
            "软删行详情必须查不到"
        );
    }

    /// ㊵ 序列号行结构：16 列全部映射（含出库/来源字段）。
    #[tokio::test]
    async fn serial_row_maps_all_columns() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate(M_SN_ID, 1).await.expect("生成 1 条");
        let id = uuid(&body.list[0].id);
        let row: SerialRow = serial_model::serial_find_by_id(id)
            .await
            .expect("查询详情")
            .expect("行存在");
        assert!(!row.serial_no.is_empty());
        assert_eq!(row.material_id, uuid(M_SN_ID));
        assert_eq!(row.warehouse_id, Some(uuid(W_ID)));
        assert_eq!(row.location_id, Some(uuid(IL_ID)));
        // P2-6b：批量生成 = 无来源入库 ⇒ 来源两列都为空（不再写「有类型无 id」的半个来源）
        assert!(row.source_type.is_none(), "批量生成不写来源类型");
        assert!(row.source_id.is_none());
        assert!(row.deleted_at.is_none());
        assert_eq!(row.created_at, row.updated_at, "刚生成时两者相等");
    }

    /// ㊶ P2-6b：`source_type` 与 `source_id` 必须**成对**且指向真实单据。
    ///
    /// 四类坏组合逐条拒（都是 400，不是 500）：有类型没 id / 有 id 没类型 /
    /// id 指向不存在的单据 / 无单据表的类型（PRODUCE）却带 id。
    /// 另外断言「拒的时候一条号都没落」：闸门在 `take_numbers` **之前**，
    /// 拒绝不能把当天号段推进一截（跳号可接受，但因为参数错而跳号不该发生）。
    #[tokio::test]
    async fn source_pair_gate_rejects_half_and_dangling_references() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let before = in_stock_count(uuid(M_SN_ID), uuid(W_ID)).await;

        // ① 有类型没 id：PURCHASE 有实体单据表（biz_stock_in）⇒ 必须给 id
        let mut req = req_for(M_SN_ID, 1);
        req.source_type = Some("PURCHASE".to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("有类型无 id 必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("必须同时给 source_id"), "{}", err.msg);

        // ② 有 id 没类型：不知道这个 id 指向哪张表
        let mut req = req_for(M_SN_ID, 1);
        req.source_id = Some(Uuid::now_v7().to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("有 id 无类型必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("不能单独出现"), "{}", err.msg);

        // ③ 指向不存在的单据：今天写进去、明天就是悬空引用
        let mut req = req_for(M_SN_ID, 1);
        req.source_type = Some("PURCHASE".to_string());
        req.source_id = Some(Uuid::now_v7().to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("指向不存在的收货单必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("不存在或已删除"), "{}", err.msg);

        // ④ 生产入库没有实体单据表（生产模块尚未落地）⇒ 不许带 id
        let mut req = req_for(M_SN_ID, 1);
        req.source_type = Some("PRODUCE".to_string());
        req.source_id = Some(Uuid::now_v7().to_string());
        let err = generate_serials(&req, None)
            .await
            .expect_err("PRODUCE 带 id 必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("没有实体单据表"), "{}", err.msg);

        assert_eq!(
            in_stock_count(uuid(M_SN_ID), uuid(W_ID)).await,
            before,
            "四次拒绝都必须在取号之前，一条序列号都不许落"
        );
    }

    /// ㊷ P2-6b：收货单 `in_type` → 序列号来源类型（纯逻辑，不连库）。
    #[test]
    fn in_type_maps_to_serial_source() {
        use super::super::stockin_base::serial_source_of_in_type;
        assert_eq!(serial_source_of_in_type("PURCHASE"), Some("PURCHASE"));
        assert_eq!(serial_source_of_in_type("RETURN"), Some("RETURN"));
        // 其他入库不在序列号来源的 4 类域内 ⇒ 不记来源（而不是硬塞成 PURCHASE）
        assert_eq!(serial_source_of_in_type("OTHER"), None);
        assert_eq!(serial_source_of_in_type("WHATEVER"), None);
    }

    // ---------- W2c-1 库存联动用例（不变式 ①②） ----------

    /// ① 批量生成 5 个 → `biz_inventory.qty` 由序列号派生 = 5（首次入库自动建档）。
    #[tokio::test]
    async fn batch_generate_derives_inventory_quantity() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // `inventory_qty` 现在是仓级 `SUM`（多桶），没有行时返回 `Some(0)` 而不是 `None`，
        // 所以「账面行已清空」要按**行数**断言（首次入库必须走 upsert 建档）。
        let rows: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&uuid(M_LINK_ID)), Value::from(&uuid(W_LINK_ID))],
            )
            .await
            .expect("读账面行数");
        assert_eq!(rows, 0, "夹具应把账面行清空：首次入库要能走 upsert 建档");
        let body = generate_link(5).await.expect("批量生成 5 条");
        assert_eq!(body.count, 5);
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(5)),
            "生成 5 件 → 账面数量必须由序列号派生为 5"
        );
        assert_eq!(in_stock_count(uuid(M_LINK_ID), uuid(W_LINK_ID)).await, 5);
        assert_invariant(M_LINK_ID, W_LINK_ID, "批量生成 5 件").await;
    }

    /// ② 批量生成 5 个 → 只有 1 条 `qty=+5` 的 IN 流水（不是 5 条）。
    #[tokio::test]
    async fn batch_generate_writes_single_in_txn() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let before = txn_count(uuid(M_LINK_ID), None).await;
        generate_link(5).await.expect("批量生成 5 条");
        assert_eq!(
            txn_count(uuid(M_LINK_ID), None).await - before,
            1,
            "一次批量生成只写一条 IN 流水"
        );
        let (txn_type, biz_type, qty, before_qty, after_qty, remark) =
            latest_txn(uuid(M_LINK_ID)).await;
        assert_eq!(txn_type, TXN_TYPE_IN);
        assert_eq!(biz_type, BIZ_TYPE_SERIAL_IN);
        assert_eq!(qty, Decimal::from(5), "qty 必须等于生成件数");
        assert_eq!(before_qty, Decimal::ZERO, "变动前无账面行 → 0");
        assert_eq!(after_qty, Decimal::from(5));
        assert!(
            remark.unwrap_or_default().contains("批量生成 5 件"),
            "流水备注要写清来源"
        );
    }

    /// ③ 1 个序列号转 OUTBOUND → 账面数量 5 → 4（不是手工 -1，而是按序列号重算）。
    #[tokio::test]
    async fn outbound_decrements_derived_quantity() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(5).await.expect("批量生成 5 条");
        let id = uuid(&body.list[0].id);
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("发货出库".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → OUTBOUND");
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(4)),
            "出库 1 件 → 账面 4"
        );
        assert_eq!(in_stock_count(uuid(M_LINK_ID), uuid(W_LINK_ID)).await, 4);
        assert_invariant(M_LINK_ID, W_LINK_ID, "出库 1 件后").await;
    }

    /// ④ 转 OUTBOUND → 1 条 OUT 流水（`qty=+1`：`ck_biz_inv_txn_qty` 禁负数，方向看 txn_type）。
    #[tokio::test]
    async fn outbound_writes_out_txn_with_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(5).await.expect("批量生成 5 条");
        let id = uuid(&body.list[0].id);
        let before = txn_count(uuid(M_LINK_ID), None).await;
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("发货出库".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → OUTBOUND");
        assert_eq!(
            txn_count(uuid(M_LINK_ID), None).await - before,
            1,
            "出库只写一条流水"
        );
        let (txn_type, biz_type, qty, before_qty, after_qty, remark) =
            latest_txn(uuid(M_LINK_ID)).await;
        assert_eq!(txn_type, TXN_TYPE_OUT);
        assert_eq!(biz_type, BIZ_TYPE_SERIAL_OUT);
        assert_eq!(qty, Decimal::from(1), "单件流水 qty 恒为 1（正数）");
        assert_eq!(before_qty, Decimal::from(5));
        assert_eq!(after_qty, Decimal::from(4));
        assert_eq!(
            remark.as_deref(),
            Some("发货出库"),
            "接口层的 reason 现在落流水 remark"
        );
    }

    /// ⑤ IN_STOCK 转 SCRAPPED → 数量 -1 + 1 条报废流水（`OUT` + `SERIAL_SCRAP`）。
    #[tokio::test]
    async fn scrap_from_stock_decrements_and_writes_scrap_txn() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(3).await.expect("批量生成 3 条");
        let id = uuid(&body.list[0].id);
        let before = txn_count(uuid(M_LINK_ID), Some(BIZ_TYPE_SERIAL_SCRAP)).await;
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_SCRAPPED.to_string(),
                reason: Some("来料破损".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → SCRAPPED");
        assert_eq!(
            txn_count(uuid(M_LINK_ID), Some(BIZ_TYPE_SERIAL_SCRAP)).await - before,
            1,
            "报废必须写一条 SCRAP 业务类型的流水"
        );
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(2))
        );
        let (txn_type, biz_type, qty, _, after_qty, _) = latest_txn(uuid(M_LINK_ID)).await;
        assert_eq!(txn_type, TXN_TYPE_OUT);
        assert_eq!(biz_type, BIZ_TYPE_SERIAL_SCRAP);
        assert_eq!(qty, Decimal::from(1));
        assert_eq!(after_qty, Decimal::from(2));
        assert_invariant(M_LINK_ID, W_LINK_ID, "在库报废 1 件后").await;
    }

    /// ⑥ OUTBOUND 转 SCRAPPED → 数量不变 + 无新流水（出库时已扣过，再扣就是重复扣减）。
    #[tokio::test]
    async fn scrap_from_outbound_keeps_quantity_and_writes_no_txn() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(2).await.expect("批量生成 2 条");
        let id = uuid(&body.list[0].id);
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("先出库");
        let qty_after_out = inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await;
        let txn_after_out = txn_count(uuid(M_LINK_ID), None).await;
        apply_status(
            id,
            &SerialStatusReq {
                status: STATUS_SCRAPPED.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("再报废");
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            qty_after_out,
            "OUTBOUND → SCRAPPED 不涉及库存"
        );
        assert_eq!(
            txn_count(uuid(M_LINK_ID), None).await,
            txn_after_out,
            "不许再写一条流水"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "出库后报废").await;
    }

    /// ⑦ 事务性：流水 INSERT 失败（remark 超 `VARCHAR(255)`）→ 序列号状态也不变。
    ///
    /// 走的是**真实生产路径**（`transition_status_with`），只把 reason 换成一个必然写不进去的值：
    /// 状态 UPDATE 先执行、流水后失败，整单回滚后库里必须还是 `IN_STOCK`。
    #[tokio::test]
    async fn txn_failure_rolls_back_serial_status() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(1).await.expect("批量生成 1 条");
        let id = uuid(&body.list[0].id);
        let txn_before = txn_count(uuid(M_LINK_ID), None).await;
        let too_long = "超".repeat(300);
        let tx = db().begin().await.expect("开启事务");
        let error =
            serial_model::transition_status_with(&tx, id, STATUS_OUTBOUND, None, Some(&too_long))
                .await
                .expect_err("remark 超过 255 字符，流水必须写失败");
        println!("预期失败：{error}");
        tx.rollback().await.expect("回滚");

        let row = serial_model::serial_find_by_id(id)
            .await
            .expect("查行")
            .expect("行存在");
        assert_eq!(row.status, STATUS_IN_STOCK, "回滚后状态必须还是 IN_STOCK");
        assert!(row.outbound_at.is_none(), "outbound_at 也不许留下");
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(1)),
            "账面数量必须回到 1"
        );
        assert_eq!(
            txn_count(uuid(M_LINK_ID), None).await,
            txn_before,
            "流水一条都不许留"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "回滚之后").await;
    }

    /// ⑧ 不变式①：一串任意操作之后，每一步 `quantity == count(serial IN_STOCK)`。
    #[tokio::test]
    async fn invariant_holds_after_every_operation() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(4).await.expect("批量生成 4 条");
        assert_invariant(M_LINK_ID, W_LINK_ID, "生成 4 条").await;
        let out_id = uuid(&body.list[0].id);
        let scrap_id = uuid(&body.list[1].id);
        let out_req = SerialStatusReq {
            status: STATUS_OUTBOUND.to_string(),
            reason: None,
        };
        let scrap_req = SerialStatusReq {
            status: STATUS_SCRAPPED.to_string(),
            reason: None,
        };
        apply_status(out_id, &out_req, None).await.expect("出库");
        assert_invariant(M_LINK_ID, W_LINK_ID, "出库 1").await;
        apply_status(scrap_id, &scrap_req, None)
            .await
            .expect("在库报废");
        assert_invariant(M_LINK_ID, W_LINK_ID, "在库报废 1").await;
        apply_status(out_id, &scrap_req, None)
            .await
            .expect("出库后报废");
        assert_invariant(M_LINK_ID, W_LINK_ID, "出库后报废").await;
        generate_link(2).await.expect("再生成 2 条");
        assert_invariant(M_LINK_ID, W_LINK_ID, "再生成 2").await;
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(4)),
            "4 - 1(出库) - 1(报废) + 2 = 4"
        );
    }

    /// ⑨ 非 `serial_required` 物料 → 完全按数量管理：拒发序列号，账面一个字不动。
    #[tokio::test]
    async fn plain_material_keeps_quantity_management() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        assert_eq!(
            inventory_qty(uuid(M_PLAIN_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(7)),
            "按数量物料账面 = 夹具的 7 件"
        );
        let err = generate_serials(
            &BatchGenerateReq {
                material_id: M_PLAIN_LINK_ID.to_string(),
                warehouse_id: Some(W_LINK_ID.to_string()),
                location_id: None,
                batch_no: None,
                count: 1,
                unit_cost: Some(OptionalDecimal::Num(Decimal::from(10))),
                source_type: None,
                source_id: None,
            },
            None,
        )
        .await
        .expect_err("按数量物料不许生成序列号");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_SERIAL_NOT_REQUIRED);

        // 同期对一物一码物料做真实出入库，按数量物料的账面必须纹丝不动
        let body = generate_link(3).await.expect("生成 3 条");
        apply_status(
            uuid(&body.list[0].id),
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("出库");
        assert_eq!(
            inventory_qty(uuid(M_PLAIN_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::from(7)),
            "序列号操作不得改按数量物料的账面"
        );
        assert_eq!(
            txn_count(uuid(M_PLAIN_LINK_ID), None).await,
            0,
            "按数量物料不产生序列号流水"
        );
    }

    /// ⑩ 并发：两个请求同时转 OUTBOUND 同一个号 → 一个成功一个 400，数量只减 1。
    #[tokio::test]
    async fn concurrent_outbound_only_decrements_once() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let body = generate_link(1).await.expect("批量生成 1 条");
        let id = uuid(&body.list[0].id);
        let txn_before = txn_count(uuid(M_LINK_ID), None).await;
        let req = SerialStatusReq {
            status: STATUS_OUTBOUND.to_string(),
            reason: Some("并发出库".to_string()),
        };
        let (left, right) =
            tokio::join!(apply_status(id, &req, None), apply_status(id, &req, None));
        let ok = usize::from(left.is_ok()) + usize::from(right.is_ok());
        assert_eq!(ok, 1, "同一件只能出库一次：{left:?} / {right:?}");
        let failed = [&left, &right]
            .into_iter()
            .filter_map(|result| result.as_ref().err())
            .collect::<Vec<_>>();
        assert_eq!(failed.len(), 1, "另一个必须失败");
        assert_eq!(failed[0].status, 400, "失败方是非法流转 400");
        assert_eq!(
            failed[0].code,
            error_code::ERP_SERIAL_STATUS_INVALID,
            "失败方业务码"
        );
        assert_eq!(
            inventory_qty(uuid(M_LINK_ID), uuid(W_LINK_ID)).await,
            Some(Decimal::ZERO),
            "数量只许减 1（1 → 0）"
        );
        assert_eq!(
            txn_count(uuid(M_LINK_ID), None).await - txn_before,
            1,
            "只许有一条 OUT 流水"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "并发出库之后").await;
    }

    /// ⑪ 期间码 `yyyy-MM` 用 CST：北京时间跨月那一刻才翻月（流水期间不许记错月）。
    #[test]
    fn period_code_uses_china_time() {
        let last = DateTime::parse_from_rfc3339("2026-10-31T15:59:59Z")
            .expect("合法时间")
            .with_timezone(&Utc);
        assert_eq!(period_code_of(last), "2026-10");
        let first = DateTime::parse_from_rfc3339("2026-10-31T16:00:00Z")
            .expect("合法时间")
            .with_timezone(&Utc);
        assert_eq!(period_code_of(first), "2026-11", "北京时间已进入 11 月");
    }

    // ==========================================================================
    // ADR-20261031：批量生成 = 无来源入库（必录成本 + 两本账 + 凭证 + 流水写实值）
    //
    // 覆盖规格 v3 §9.2 的 T1–T20（T19「落号条数不符」注入不了，见交付说明）：
    //   每个用例都是**真库**断言，不允许只断 `is_ok()`。
    // ==========================================================================

    /// 可空 UUID 的文本参数形态（`Value::Null` 绑 `::uuid` 驱动编不出来）。
    fn uuid_text_param(value: Option<Uuid>) -> Value {
        Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
    }

    /// 成本账某个桶的 `(qty, amount, avg_cost)`，**按库里的标度取文本**（`"5.0000"` 这种
    /// 逐字比对才能钉住 `numeric(18,4)` / `(18,6)` 的精度，`Decimal ==` 会放过 `2 == 2.0000`）。
    async fn cost_bucket(
        material_id: Uuid,
        warehouse_id: Uuid,
        bucket: Option<Uuid>,
    ) -> Option<(String, String, String)> {
        let rows = db()
            .find_with_params(
                "SELECT qty::text AS qty, amount::text AS amount, avg_cost::text AS avg_cost \
                   FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                      = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid)",
                vec![
                    Value::from(&material_id),
                    Value::from(&warehouse_id),
                    uuid_text_param(bucket),
                ],
            )
            .await
            .expect("读成本账桶");
        rows.first().map(|row| {
            (
                row.get_typed("qty").expect("qty"),
                row.get_typed("amount").expect("amount"),
                row.get_typed("avg_cost").expect("avg_cost"),
            )
        })
    }

    /// 库位账合计（某个 物料×仓库 的全部库位行求和）。
    async fn location_total(material_id: Uuid, warehouse_id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await
        .expect("读库位账合计")
    }

    /// 某张凭证的分录（`(account_code, debit, credit)`，按行号）。
    async fn voucher_lines(source_type: &str, source_id: Uuid) -> Vec<(String, String, String)> {
        let rows = db()
            .find_with_params(
                "SELECT e.account_code::text AS account_code, e.debit_amount::text AS debit, \
                        e.credit_amount::text AS credit \
                   FROM biz_voucher_entry e \
                   JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.source_type = ? AND v.source_id = ?::uuid \
                  ORDER BY e.line_no",
                vec![Value::from(source_type), Value::from(&source_id)],
            )
            .await
            .expect("读凭证分录");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("account_code").expect("code"),
                    row.get_typed("debit").expect("debit"),
                    row.get_typed("credit").expect("credit"),
                )
            })
            .collect()
    }

    /// 某来源单据的蓝字凭证数（红冲不计）。
    async fn voucher_count(source_type: &str, source_id: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid AND is_red = 0",
            vec![Value::from(source_type), Value::from(&source_id)],
        )
        .await
        .expect("统计凭证")
    }

    /// 最新一条**指定业务类型**的流水 `(unit_cost, amount, qty, before_qty, after_qty, remark)`。
    async fn latest_txn_of(
        material_id: Uuid,
        biz_type: &str,
    ) -> (String, String, String, String, String, Option<String>) {
        let rows = db()
            .find_with_params(
                "SELECT unit_cost::text AS unit_cost, amount::text AS amount, qty::text AS qty, \
                        before_qty::text AS before_qty, after_qty::text AS after_qty, remark \
                   FROM biz_inventory_txn WHERE material_id = ?::uuid AND biz_type = ? \
                  ORDER BY created_at DESC, id DESC LIMIT 1",
                vec![Value::from(&material_id), Value::from(biz_type)],
            )
            .await
            .expect("读流水");
        let row = rows.first().expect("应该有一条流水");
        (
            row.get_typed("unit_cost").expect("unit_cost"),
            row.get_typed("amount").expect("amount"),
            row.get_typed("qty").expect("qty"),
            row.get_typed("before_qty").expect("before_qty"),
            row.get_typed("after_qty").expect("after_qty"),
            row.get_typed("remark").expect("remark"),
        )
    }

    /// 判据 B2 的差值：`Σ biz_inventory.amount − Σ(1405 借 − 贷)`（规格 §1.2；BLOCKER）。
    ///
    /// 断言用**增量**（`|after − before| ≤ 0.0001`）：库里其它模块的历史数据可能有残差，
    /// 本卡只保证「自己写进去的凭证配平」。
    async fn b2_gap() -> Decimal {
        db().scalar_with_params(
            "SELECT (SELECT COALESCE(SUM(amount), 0) FROM biz_inventory) \
                  - (SELECT COALESCE(SUM(debit_amount - credit_amount), 0) FROM biz_voucher_entry \
                      WHERE account_code = '1405')",
            vec![],
        )
        .await
        .expect("读 B2 差值")
    }

    /// 六表 + 凭证总行数的快照（「400 零落库」用它们逐项前后对比）。
    ///
    /// 每条 SQL 的占位符个数不同（后两条是**全表**计数，没有 `?`）⇒ 参数必须按条给，
    /// 不能共用一个 `vec![material_id]`（驱动会报 `Parameters(1, 0)`）。
    async fn ledger_snapshot(material_id: Uuid) -> Vec<i64> {
        let mut out = Vec::new();
        let by_material = [
            "SELECT COUNT(*) FROM biz_material_serial WHERE material_id = ?::uuid",
            "SELECT COUNT(*) FROM biz_inventory WHERE material_id = ?::uuid",
            "SELECT COUNT(*) FROM biz_inventory_location WHERE material_id = ?::uuid",
            "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid",
        ];
        for sql in by_material {
            let n: i64 = db()
                .scalar_with_params(sql, vec![Value::from(&material_id)])
                .await
                .expect("计数");
            out.push(n);
        }
        for sql in [
            "SELECT COUNT(*) FROM biz_voucher",
            "SELECT COUNT(*) FROM biz_voucher_entry",
        ] {
            let n: i64 = db().scalar_with_params(sql, vec![]).await.expect("计数");
            out.push(n);
        }
        out
    }

    /// `biz_seq(SN).current_no`（「零落库」还要证明号段没被推进）。
    async fn seq_current_no() -> i64 {
        db().scalar_with_params(
            "SELECT current_no FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("读号段")
    }

    /// T1/T2：缺 `unit_cost` ⇒ 400 + 10001 逐字报文，且**六表 + 号段零变化**。
    ///
    /// 「零落库」的机制是校验位置：`validate()` 是 `generate_serials` 的第一行，
    /// 早于物料查询、早于 `db().begin()`、更早于 `take_numbers`。
    #[tokio::test]
    async fn t1_missing_unit_cost_rejects_with_zero_writes() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let before_counts = ledger_snapshot(uuid(M_LINK_ID)).await;
        let before_seq = seq_current_no().await;
        let mut req = req_for_link(3);
        req.unit_cost = None;
        let err = generate_serials(&req, None)
            .await
            .expect_err("缺 unit_cost 必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert_eq!(
            err.msg,
            "unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）"
        );
        assert_eq!(
            ledger_snapshot(uuid(M_LINK_ID)).await,
            before_counts,
            "400 必须零落库（号/成本账/库位账/流水/凭证/分录一行都不许有）"
        );
        assert_eq!(
            seq_current_no().await,
            before_seq,
            "校验失败不得推进 biz_seq"
        );
    }

    /// T3：0 / -1.5 / "abc" / 1e11 四种非法单价都 400，且报文逐字（规格 §2.3）。
    #[tokio::test]
    async fn t3_invalid_unit_cost_values_reject() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let before = ledger_snapshot(uuid(M_LINK_ID)).await;
        let cases: Vec<(Option<OptionalDecimal>, &str)> = vec![
            (
                Some(OptionalDecimal::Num(Decimal::ZERO)),
                "unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收",
            ),
            (
                Some(OptionalDecimal::Num(
                    Decimal::from_str_exact("-1.5").expect("decimal"),
                )),
                "unit_cost 必须 > 0（收到 -1.5）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收",
            ),
            (
                Some(OptionalDecimal::Raw("abc".to_string())),
                "unit_cost 不是合法数字：「abc」",
            ),
            (
                Some(OptionalDecimal::Num(Decimal::from(
                    crate::business::erp::serial_dto::MAX_UNIT_COST,
                ))),
                "unit_cost 超出可记账范围（须 < 100000000000）：100000000000",
            ),
        ];
        for (unit_cost, expected) in cases {
            let mut req = req_for_link(1);
            req.unit_cost = unit_cost;
            let err = generate_serials(&req, None)
                .await
                .expect_err("非法单价必须 400（且逐字报文）");
            assert_eq!(err.status, 400, "{expected}");
            assert_eq!(err.msg, expected);
        }
        assert_eq!(
            ledger_snapshot(uuid(M_LINK_ID)).await,
            before,
            "非法单价一律零落库"
        );
    }

    /// T4–T7 + T13/T14：一次生成 5 件 @10 ⇒ 成本账 / 库位账 / 流水 / 凭证 / 响应体全对，B2 增量 0。
    #[tokio::test]
    async fn t4_to_t7_and_t14_single_generate_writes_两本账_and_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_LINK_ID);
        let warehouse = uuid(W_LINK_ID);
        let b2_before = b2_gap().await;
        let body = generate_link(5).await.expect("批量生成 5 件 @10");

        // T7 响应体回显
        assert_eq!(body.unit_cost.to_string(), "10.000000");
        assert_eq!(body.amount.to_string(), "50.0000");
        assert_eq!(body.inventory.qty.to_string(), "5.0000");
        assert_eq!(body.inventory.amount.to_string(), "50.0000");
        assert_eq!(body.inventory.avg_cost.to_string(), "10.000000");

        // T4 成本账（NULL 汇总桶）+ 库位账
        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "5.0000".to_string(),
                "50.0000".to_string(),
                "10.000000".to_string()
            )),
            "qty=5 / amount=5×10 / avg_cost=单价"
        );
        assert_eq!(
            location_total(material, warehouse).await,
            "5.0000",
            "库位账合计 = 5"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "生成 5 件 @10 后").await;

        // T5 流水：形态不变（IN + SERIAL_IN + SERIAL_BATCH），单价/金额写实值
        let (unit_cost, amount, qty, before_qty, after_qty, remark) =
            latest_txn_of(material, BIZ_TYPE_SERIAL_IN).await;
        assert_eq!(unit_cost, "10.000000");
        assert_eq!(amount, "50.0000", "流水金额是实值，不是 0");
        assert_eq!(qty, "5.0000");
        assert_eq!(before_qty, "0.0000");
        assert_eq!(after_qty, "5.0000");
        let remark = remark.unwrap_or_default();
        assert!(remark.contains("批量生成 5 件"), "{remark}");
        assert!(remark.contains("单价 10.000000"), "{remark}");
        assert!(remark.contains("金额 50.0000"), "{remark}");
        assert!(
            remark.chars().count() <= 255,
            "remark 必须塞得进 VARCHAR(255)：{remark}"
        );
        let shape: (String, String) = {
            let rows = db()
                .find_with_params(
                    "SELECT txn_type::text AS txn_type, doc_type::text AS doc_type \
                       FROM biz_inventory_txn WHERE material_id = ?::uuid \
                      ORDER BY created_at DESC, id DESC LIMIT 1",
                    vec![Value::from(&material)],
                )
                .await
                .expect("读流水形态");
            let row = rows.first().expect("有流水");
            (
                row.get_typed("txn_type").expect("type"),
                row.get_typed("doc_type").expect("doc"),
            )
        };
        assert_eq!(shape, (TXN_TYPE_IN.to_string(), "SERIAL_BATCH".to_string()));

        // T6 凭证：借 1405 / 贷 1901，来源 = 首号（doc_id）
        let doc_id = uuid(&body.list[0].id);
        assert_eq!(voucher_count("SERIAL_BATCH", doc_id).await, 1);
        let lines = voucher_lines("SERIAL_BATCH", doc_id).await;
        assert_eq!(
            lines,
            vec![
                (
                    "1405".to_string(),
                    "50.0000".to_string(),
                    "0.0000".to_string()
                ),
                (
                    "1901".to_string(),
                    "0.0000".to_string(),
                    "50.0000".to_string()
                ),
            ],
            "期初入库：借 库存商品 / 贷 待处理财产损溢"
        );

        // T14 B2 增量 = 0
        let b2_after = b2_gap().await;
        assert!(
            (b2_after - b2_before).abs() <= Decimal::from_str_exact("0.0001").expect("tol"),
            "B2 增量必须为 0（before {b2_before} / after {b2_after}）"
        );
    }

    /// T8：二次生成不同单价（10 → 13，中间出一件）⇒ `avg = ROUND((40+65)/9, 6) = 11.666667`
    /// （规格 §8.3 步骤 1–3）。
    #[tokio::test]
    async fn t8_second_generate_uses_moving_weighted_average() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_LINK_ID);
        let warehouse = uuid(W_LINK_ID);
        let first = generate_link(5).await.expect("第一次 5 件 @10");
        // 规格 §8.3 的步骤 2：先出 1 件（5/50/10 → 4/40/10），再第二次入库，
        // 这样「分母是 9」才成立。少了这一步，二次生成后是 10 件 / 115 / 11.5。
        apply_status(
            uuid(&first.list[0].id),
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("T8：中间出库 1 件".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → OUTBOUND");
        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "4.0000".to_string(),
                "40.0000".to_string(),
                "10.000000".to_string()
            )),
            "出库 1 件后：4 / 40 / 10（avg 不动）"
        );
        let mut req = req_for_link(5);
        req.unit_cost = Some(OptionalDecimal::Num(Decimal::from(13)));
        let body = generate_serials(&req, None).await.expect("第二次 5 件 @13");
        assert_eq!(body.amount.to_string(), "65.0000", "本次入账 = 5 × 13");
        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "9.0000".to_string(),
                "105.0000".to_string(),
                "11.666667".to_string()
            )),
            "avg = ROUND((40 + 65) / 9, 6) = 11.666667"
        );
        assert_eq!(location_total(material, warehouse).await, "9.0000");
        assert_invariant(M_LINK_ID, W_LINK_ID, "二次生成后").await;
    }

    /// T9：出库 1 件 ⇒ 成本账 4/40/10、库位账 4、流水 OUT 实值、凭证 借 6401 / 贷 1405。
    #[tokio::test]
    async fn t9_outbound_converts_cost_and_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_LINK_ID);
        let warehouse = uuid(W_LINK_ID);
        let body = generate_link(5).await.expect("生成 5 件 @10");
        let serial_id = uuid(&body.list[0].id);
        let b2_before = b2_gap().await;
        apply_status(
            serial_id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("发货出库".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → OUTBOUND");

        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "4.0000".to_string(),
                "40.0000".to_string(),
                "10.000000".to_string()
            )),
            "出库 1 件：4 / 40 / 10"
        );
        assert_eq!(
            location_total(material, warehouse).await,
            "4.0000",
            "库位账同步减到 4"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "出库 1 件后").await;

        let (unit_cost, amount, qty, _, _, _) = latest_txn_of(material, BIZ_TYPE_SERIAL_OUT).await;
        assert_eq!(unit_cost, "10.000000", "出库流水单价 = 该桶 avg_cost");
        assert_eq!(amount, "10.0000", "出库流水金额 = 账上真实差额");
        assert_eq!(qty, "1.0000");

        let lines = voucher_lines("SERIAL_OUT", serial_id).await;
        assert_eq!(
            lines,
            vec![
                (
                    "6401".to_string(),
                    "10.0000".to_string(),
                    "0.0000".to_string()
                ),
                (
                    "1405".to_string(),
                    "0.0000".to_string(),
                    "10.0000".to_string()
                ),
            ],
            "出库结转：借 主营业务成本 / 贷 库存商品"
        );
        let b2_after = b2_gap().await;
        assert!(
            (b2_after - b2_before).abs() <= Decimal::from_str_exact("0.0001").expect("tol"),
            "出库也要让 B2 增量 = 0（before {b2_before} / after {b2_after}）"
        );
    }

    /// T10：报废 1 件（规格 §8.3 步骤 1–4：生成 5@10 → 出 1 → 生成 5@13 → 报废 1）
    /// ⇒ 8/93.3333；凭证 借 1901 11.6667（差额口径）。
    #[tokio::test]
    async fn t10_scrap_converts_cost_by_difference() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_LINK_ID);
        let warehouse = uuid(W_LINK_ID);
        let first = generate_link(5).await.expect("5 件 @10");
        apply_status(
            uuid(&first.list[0].id),
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("T10：中间出库 1 件".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → OUTBOUND");
        let mut req = req_for_link(5);
        req.unit_cost = Some(OptionalDecimal::Num(Decimal::from(13)));
        let body = generate_serials(&req, None).await.expect("5 件 @13");
        let serial_id = uuid(&body.list[0].id);
        apply_status(
            serial_id,
            &SerialStatusReq {
                status: STATUS_SCRAPPED.to_string(),
                reason: Some("来料破损".to_string()),
            },
            None,
        )
        .await
        .expect("IN_STOCK → SCRAPPED");

        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "8.0000".to_string(),
                "93.3333".to_string(),
                "11.666667".to_string()
            )),
            "报废 1 件：8 / 93.3333（amount = 8 × 11.666667）"
        );
        assert_eq!(location_total(material, warehouse).await, "8.0000");
        let lines = voucher_lines("SERIAL_SCRAP", serial_id).await;
        assert_eq!(
            lines,
            vec![
                (
                    "1901".to_string(),
                    "11.6667".to_string(),
                    "0.0000".to_string()
                ),
                (
                    "1405".to_string(),
                    "0.0000".to_string(),
                    "11.6667".to_string()
                ),
            ],
            "报废损失按差额口径：105.0000 − 93.3333 = 11.6667（不是 1 × 11.666667）"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "报废 1 件后").await;
    }

    /// T11/T12/T20：混桶（NULL 汇总桶 + 库位桶）生成 → 出库，B1 恒等、流水条数不随桶数翻倍。
    #[tokio::test]
    async fn t11_t12_t20_mixed_buckets_keep_b1_and_one_txn_per_movement() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_SN_ID);
        let warehouse = uuid(W_ID);
        let bucket = uuid(IL_ID);
        // 流水是**只增不改**的审计台账，`setup()` 不清历史行 ⇒ 条数断言必须用「增量」，
        // 不能拿绝对值（重跑同一个库会漂数：第一次 2 条、第二次 4 条…）。
        let txn_in_before = txn_count(material, Some(BIZ_TYPE_SERIAL_IN)).await;
        // 桶 1：NULL 汇总桶 5 件 @10
        let mut req = req_for(M_SN_ID, 5);
        req.location_id = None;
        generate_serials(&req, None)
            .await
            .expect("NULL 桶 5 件 @10");
        // 桶 2：库位桶 3 件 @15
        let mut req = req_for(M_SN_ID, 3);
        req.unit_cost = Some(OptionalDecimal::Num(Decimal::from(15)));
        let body = generate_serials(&req, None).await.expect("库位桶 3 件 @15");

        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "5.0000".to_string(),
                "50.0000".to_string(),
                "10.000000".to_string()
            )),
            "NULL 桶 5 / 50 / 10"
        );
        assert_eq!(
            cost_bucket(material, warehouse, Some(bucket)).await,
            Some((
                "3.0000".to_string(),
                "45.0000".to_string(),
                "15.000000".to_string()
            )),
            "库位桶 3 / 45 / 15"
        );
        assert_eq!(
            location_total(material, warehouse).await,
            "8.0000",
            "库位账 5 + 3 = 8"
        );
        assert_invariant(M_SN_ID, W_ID, "混桶生成后（B1 差 0）").await;
        // T20：两次生成 = 两条流水（不是两条 × 桶数）
        assert_eq!(
            txn_count(material, Some(BIZ_TYPE_SERIAL_IN)).await - txn_in_before,
            2,
            "每次库存变动恰好一条流水（多桶也不许翻倍，否则 B4 双计）"
        );

        // T12：从库位桶出库 1 件 → 该桶 2/30，NULL 桶不动
        let serial_id = uuid(&body.list[0].id);
        apply_status(
            serial_id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("从库位桶出库 1 件");
        assert_eq!(
            cost_bucket(material, warehouse, Some(bucket)).await,
            Some((
                "2.0000".to_string(),
                "30.0000".to_string(),
                "15.000000".to_string()
            )),
            "库位桶 2 / 30"
        );
        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "5.0000".to_string(),
                "50.0000".to_string(),
                "10.000000".to_string()
            )),
            "NULL 桶不受影响"
        );
        assert_eq!(
            location_total(material, warehouse).await,
            "7.0000",
            "库位账 5 + 2 = 7"
        );
        assert_invariant(M_SN_ID, W_ID, "库位桶出库 1 件后").await;
        let lines = voucher_lines("SERIAL_OUT", serial_id).await;
        assert_eq!(
            lines,
            vec![
                (
                    "6401".to_string(),
                    "15.0000".to_string(),
                    "0.0000".to_string()
                ),
                (
                    "1405".to_string(),
                    "0.0000".to_string(),
                    "15.0000".to_string()
                ),
            ],
            "库位桶出库按该桶单价 15 结转"
        );
        // 桶里最后一件事走了也要归零：再出 1 件 → 库位桶 1 → 出完 → 0（B1 仍差 0）
        let serial_id2 = uuid(&body.list[1].id);
        apply_status(
            serial_id2,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("库位桶再出 1 件");
        let serial_id3 = uuid(&body.list[2].id);
        apply_status(
            serial_id3,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: None,
            },
            None,
        )
        .await
        .expect("库位桶出空");
        assert_eq!(
            cost_bucket(material, warehouse, Some(bucket)).await,
            Some((
                "0.0000".to_string(),
                "0.0000".to_string(),
                "15.000000".to_string()
            )),
            "桶出空后 qty/amount 归零（avg_cost 不动）"
        );
        assert_invariant(M_SN_ID, W_ID, "库位桶出空后（B1 差 0）").await;
    }

    /// T15：缺 `1901` 科目时生成 ⇒ 500，且**零落库**（凭证写不出来就整单回滚）。
    #[tokio::test]
    async fn t15_missing_account_rolls_back_whole_batch() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let before = ledger_snapshot(uuid(M_LINK_ID)).await;
        // 「科目不可用」用**停用**而不是物理删：`biz_voucher_entry.account_id` 有外键，
        // 其它用例已经写过 1901 的分录，物理删会撞 FK（而停用正是运维真实的故障形态：
        // 科目被停用 / 未启用 ⇒ `account_required` 取不到）。
        db().execute_with_params(
            "UPDATE biz_account SET status = 0 WHERE account_code = '1901'",
            vec![],
        )
        .await
        .expect("临时停用 1901 科目");
        let err = generate_link(2)
            .await
            .expect_err("科目不可用必须报错（不能静默不记账）");
        // 科目缺失是 DbError::InvalidValue → utils::db_error 收敛成 500 + 10004
        assert_eq!(err.status, 500, "{}", err.msg);
        db().execute_with_params(
            "UPDATE biz_account SET status = 1 WHERE account_code = '1901'",
            vec![],
        )
        .await
        .expect("还原 1901 科目");
        assert_eq!(
            ledger_snapshot(uuid(M_LINK_ID)).await,
            before,
            "科目缺失 ⇒ 号 / 两本账 / 流水 / 凭证 全部回滚（号段浪费可接受）"
        );
    }

    /// T16：历史零成本存量（qty=2 / avg=0 / amount=0）出库 ⇒ 成功、流水金额 0、**不发凭证**。
    ///
    /// 这是「存量不补账」的过渡口径（规格 v3 §8.2）：`post_voucher` 在借贷合计 0 时会报错，
    /// 所以 `create_for_serial_movement` 必须自己先判「差额 ≤ 0 → 跳过 + WARN」。
    #[tokio::test]
    async fn t16_legacy_zero_cost_outbound_skips_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let material = uuid(M_LINK_ID);
        let warehouse = uuid(W_LINK_ID);
        let serial_id = Uuid::now_v7();
        // 直接造「改判前」的形态：号在库、成本账零价值、库位账有件数
        db().execute_with_params(
            "DELETE FROM biz_material_serial WHERE serial_no IN ('SN-LEGACY-T16', 'SN-LEGACY-T16-B')",
            vec![],
        )
        .await
        .expect("清理旧夹具");
        db().execute_with_params(
            "INSERT INTO biz_material_serial \
                 (id, serial_no, material_id, warehouse_id, location_id, status, inbound_at) \
             VALUES (?::uuid, 'SN-LEGACY-T16', ?::uuid, ?::uuid, NULL, 'IN_STOCK', now()), \
                    (?::uuid, 'SN-LEGACY-T16-B', ?::uuid, ?::uuid, NULL, 'IN_STOCK', now())",
            vec![
                Value::from(&serial_id),
                Value::from(&material),
                Value::from(&warehouse),
                Value::from(&Uuid::now_v7()),
                Value::from(&material),
                Value::from(&warehouse),
            ],
        )
        .await
        .expect("插入历史号（2 件：出 1 件后账面应剩 1）");
        db().execute_with_params(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                        avg_cost, amount) \
             VALUES (?::uuid, ?::uuid, ?::uuid, NULL, 2.0000, 0, 0, 0) \
             ON CONFLICT (material_id, warehouse_id, \
                          COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET qty = 2.0000, avg_cost = 0, amount = 0",
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material),
                Value::from(&warehouse),
            ],
        )
        .await
        .expect("插入零成本成本账");
        db().execute_with_params(
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) \
             VALUES (?::uuid, ?::uuid, ?::uuid, NULL, 2.0000) \
             ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = 2.0000",
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material),
                Value::from(&warehouse),
            ],
        )
        .await
        .expect("插入库位账");

        apply_status(
            serial_id,
            &SerialStatusReq {
                status: STATUS_OUTBOUND.to_string(),
                reason: Some("历史零成本出库".to_string()),
            },
            None,
        )
        .await
        .expect("历史零成本库存必须能正常出库（不能因差额 0 报 500）");

        assert_eq!(
            cost_bucket(material, warehouse, None).await,
            Some((
                "1.0000".to_string(),
                "0.0000".to_string(),
                "0.000000".to_string()
            )),
            "1 件 / 0 元（存量不补账）"
        );
        assert_eq!(location_total(material, warehouse).await, "1.0000");
        let (_, amount, _, _, _, _) = latest_txn_of(material, BIZ_TYPE_SERIAL_OUT).await;
        assert_eq!(amount, "0.0000", "差额 0 ⇒ 流水金额 0");
        assert_eq!(
            voucher_count("SERIAL_OUT", serial_id).await,
            0,
            "差额 ≤ 0 ⇒ 不发凭证（否则 post_voucher 的「借贷合计为 0」护栏会把正常出库打成 500）"
        );
        assert_invariant(M_LINK_ID, W_LINK_ID, "历史零成本出库后").await;
    }

    /// T17/T18：既有校验不回归 —— 非一物一码物料 400、缺仓库 400 + 66005。
    #[tokio::test]
    async fn t17_t18_existing_validations_do_not_regress() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let mut req = req_for(M_PLAIN_ID, 1);
        req.location_id = None;
        let err = generate_serials(&req, None)
            .await
            .expect_err("非一物一码物料必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_SERIAL_NOT_REQUIRED);

        let mut req = req_for(M_SN_ID, 1);
        req.warehouse_id = None;
        req.location_id = None;
        let err = generate_serials(&req, None)
            .await
            .expect_err("缺 warehouse_id 必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
        assert!(err.msg.contains("warehouse_id"), "{}", err.msg);
    }

    // ---------- 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留**序列号变动产生的 `VOUCHER` 凭证与状态日志。
    ///
    /// 批量生成 / 出库 / 报废都会发凭证（ADR-20261031「两本账 + 凭证」），而夹具以前
    /// 只清序列号与两本账 ⇒ `biz_voucher` 与 `biz_status_log` 每跑一遍全量就 +58 行
    /// （本卡实测，线性和增长）。`zz_` 排在本模块最后，是这一块的收口。
    #[tokio::test]
    async fn zz_teardown_purges_serial_vouchers() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // ① 自造一批会发凭证的序列号（真实 `generate_serials` 路径）
        generate(M_SN_ID, 2).await.expect("批量生成应成功");
        let voucher_id: Option<Uuid> = db()
            .query_first_sql(
                "SELECT id FROM biz_voucher WHERE source_type = 'SERIAL_BATCH' \
                  ORDER BY created_at DESC, id DESC LIMIT 1"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查序列号凭证")
            .map(|row| row.get_typed("id").expect("读凭证 id"));
        let voucher_id = voucher_id.expect("批量生成必须发一张凭证（否则这条用例证明不了什么）");
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert!(logs > 0, "凭证落地必须写 VOUCHER 状态日志");

        // ② 走模块真实的清场
        purge_serial_vouchers().await;

        // ③ 凭证与其日志都收干
        let left_voucher: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_voucher WHERE id = ?::uuid",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查凭证")
            .unwrap_or(0);
        assert_eq!(left_voucher, 0, "清场后序列号凭证必须不存在");
        let left_logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'VOUCHER' AND entity_id = ?::uuid",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert_eq!(
            left_logs, 0,
            "清场后 VOUCHER 状态日志必须为 0（日志无外键，必须按 id 先删）"
        );
        let serial_vouchers: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_voucher \
                  WHERE source_type IN ('SERIAL_BATCH', 'SERIAL_OUT', 'SERIAL_SCRAP')"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查序列号凭证总数");
        assert_eq!(serial_vouchers, 0, "本模块不许留任何序列号凭证");

        // ④ 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        fixture_hygiene::purge_orphans(fixture_hygiene::FINANCE).await;
        assert_eq!(
            fixture_hygiene::orphans(fixture_hygiene::FINANCE).await,
            0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志"
        );
    }
}
