//! ERP 批量操作（D7）：勾选 → 批量过账 / 作废 / 删除 / 提交审批 / 导出。
//!
//! # 统一形态
//!
//! ```text
//! POST /business/erp/<资源>/batch-status   { "ids": [...], "status": "POSTED", "reason": null }
//! POST /business/erp/<资源>/batch-delete   { "ids": [...] }
//! POST /business/erp/<资源>/batch-export   { "ids": [...] }   // 空 ids = 当前筛选
//!
//! 响应：{ succeeded:[{id,no}], failed:[{id,no,code,error}], total, success_count, fail_count }
//! ```
//!
//! # 三条硬约束（卡面 §六）
//!
//! 1. **逐条独立事务**：本模块**不开事务**，逐条调用已有的单条业务函数
//!    （`change_stock_in_status` / `submit_approval` / `change_transfer_status` /
//!    `apply_status`），那些函数各自 `begin/commit/rollback`。所以 10 条里第 5 条失败时，
//!    前 4 条**已经落库**，不会被第 5 条拖回去。
//! 2. **失败逐条报原因**：`error` 是单条接口的原文（含单号、当前状态、下一步怎么改），
//!    不是「部分失败」。
//! 3. **幂等**：重复点同一个批量按钮，已处理的行由单条函数判 409（如「已过账不能重复过账，
//!    库存只加一次」），第二次是「0 成功 / N 失败」，**不会**把库存加两遍。
//!
//! # 为什么状态校验先做一遍
//!
//! `status = "XXX"` 这种整体非法（不在白名单）会让 200 条各报一次同样的 400，
//! 用户看到的是一屏一样的错误。所以入口先用同一个 DTO 的 `target_status()` 校验一次，
//! 非法直接 400；**逐条**才判「这一条能不能这么转」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只按主键批量点名 biz_* 单据表（V0137+ 无 dept_id / 归属人列），
//! 可见性由权限码控制（与单条接口同一套码），不做行级范围裁剪。
//! ```

use crate::utils;
use axum::{Extension, Json};
use nik::prelude::*;

use crate::business::erp::approval_gate;
use crate::business::erp::purchase_dto::PurchaseOrderStatusReq;
use crate::business::erp::purchase_handler::{change_purchase_order_status, submit_approval};
use crate::business::erp::purchase_model::STATUS_SUBMITTED;
use crate::business::erp::serial_dto::SerialStatusReq;
use crate::business::erp::serial_handler::apply_status;
use crate::business::erp::stockin_dto::StockInStatusReq;
use crate::business::erp::stockin_handler::{change_stock_in_status, delete_stock_in};
use crate::business::erp::transfer_dto::TransferStatusReq;
use crate::business::erp::transfer_handler::change_transfer_status;
use crate::business::impexp::batch_export::{self, BatchExportTask};
use crate::business::impexp::definition::ExportFilter;
use crate::business::shared::batch::{self, BatchResult};

// ==================== 权限码（与单条接口逐字一致） ====================

/// 收货单状态流转。
pub const PERM_STOCK_IN_STATUS: &str = "business:erp:stock-in:status";
/// 收货单删除。
pub const PERM_STOCK_IN_DELETE: &str = "business:erp:stock-in:delete";
/// 收货单列表（导出按列表权限）。
pub const PERM_STOCK_IN_LIST: &str = "business:erp:stock-in:list";
/// 采购订单状态流转。
pub const PERM_PURCHASE_STATUS: &str = "business:erp:purchase-order:status";
/// 采购订单提交审批。
pub const PERM_PURCHASE_SUBMIT: &str = "business:erp:purchase-order:submit-approval";
/// 调拨单状态流转。
pub const PERM_TRANSFER_STATUS: &str = "business:erp:transfer:status";
/// 序列号状态流转。
pub const PERM_SERIAL_STATUS: &str = "business:erp:serial:status";
/// 应付单列表。
pub const PERM_PAYABLE_LIST: &str = "business:erp:payable:list";
/// 凭证列表。
pub const PERM_VOUCHER_LIST: &str = "business:erp:voucher:list";

// ==================== 请求体 ====================

/// 批量状态流转请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchStatusReq {
    /// 勾选的主键。
    pub ids: Vec<String>,
    /// 目标状态（白名单由各资源自己的 DTO 判）。
    pub status: Option<String>,
    /// 原因（`→CANCELLED` 等必填场景由单条函数判）。
    pub reason: Option<String>,
}

/// 批量导出请求。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchExportReq {
    /// 勾选的主键；空 = 按 `keyword` / `status` 导「当前筛选结果」。
    pub ids: Vec<String>,
    /// 关键词（与列表页同一个过滤口径）。
    pub keyword: Option<String>,
    /// 状态过滤（1 启用 / 0 停用这类整型码；ERP 单据传 `None`）。
    pub status: Option<i16>,
}

// ==================== 业务实现（单测直接调这一层） ====================

/// 收货入库单批量状态流转（过账 / 作废）。
///
/// # 异常情况
///
/// - `403`：缺 `business:erp:stock-in:status`；
/// - `400`：`ids` 为空 / 超上限 / 非法 UUID / `status` 不在白名单；
/// - 单条失败不影响其它条（进 `failed`，带原文原因）。
pub async fn batch_stock_in_status(
    claims: &TokenClaims,
    req: &BatchStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_STOCK_IN_STATUS)?;
    let ids = batch::parse_ids(&req.ids)?;
    let inner = StockInStatusReq {
        status: req.status.clone(),
        reason: req.reason.clone(),
        serials: Vec::new(),
        ..Default::default()
    };
    // 目标态整体校验：非法值一次性 400，不要让 200 条各报一遍
    inner.target_status()?;
    let labels = batch::labels_of("biz_stock_in", "in_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询收货单号"))?;
    let claims = claims.clone();
    Ok(batch::run(&ids, &labels, |id| {
        let inner = inner.clone();
        let claims = claims.clone();
        async move { change_stock_in_status(id, &inner, &claims).await }
    })
    .await)
}

/// 收货入库单批量删除（仅草稿 / 过账审批中，逐条判）。
pub async fn batch_stock_in_delete(
    claims: &TokenClaims,
    req: &BatchStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_STOCK_IN_DELETE)?;
    let ids = batch::parse_ids(&req.ids)?;
    let labels = batch::labels_of("biz_stock_in", "in_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询收货单号"))?;
    let operator = utils::current_user_uuid(claims);
    Ok(batch::run(&ids, &labels, |id| async move {
        delete_stock_in(id, operator).await
    })
    .await)
}

/// 采购订单批量状态流转（取消 / 关闭；`status=SUBMITTED` 走审批入口）。
pub async fn batch_purchase_status(
    claims: &TokenClaims,
    req: &BatchStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    let inner = PurchaseOrderStatusReq {
        status: req.status.clone(),
        reason: req.reason.clone(),
        ..Default::default()
    };
    // 目标态决定要哪个权限码：「提交审批」与「作废/关闭」是两个按钮、两个码
    let target = inner.target_status()?;
    if target == STATUS_SUBMITTED {
        batch::ensure_permission(claims, PERM_PURCHASE_SUBMIT)?;
    } else {
        batch::ensure_permission(claims, PERM_PURCHASE_STATUS)?;
    }
    let ids = batch::parse_ids(&req.ids)?;
    let labels = batch::labels_of("biz_purchase_order", "order_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单号"))?;
    let claims = claims.clone();
    // ARCH-3 DC-01：批量入口**不支持**跳过审批（`skip_approval` 不在批量请求体里）——
    // 批量勾选「提交审批」时逐条按默认拒绝走，没配流程的每条都回 409 + 66033，
    // 由批量报告逐条说明。要跳过请对单张单据用单条接口显式传 skip_approval。
    let skip = approval_gate::SkipApprovalReq::default();
    Ok(batch::run(&ids, &labels, |id| {
        let inner = inner.clone();
        let claims = claims.clone();
        let skip = skip.clone();
        async move {
            if inner.target_status()? == STATUS_SUBMITTED {
                let gate = approval_gate::ApprovalGate::new(&skip, &claims);
                submit_approval(id, &claims, &gate).await
            } else {
                change_purchase_order_status(id, &inner, &claims).await
            }
        }
    })
    .await)
}

/// 调拨单批量状态流转。
pub async fn batch_transfer_status(
    claims: &TokenClaims,
    req: &BatchStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_TRANSFER_STATUS)?;
    let ids = batch::parse_ids(&req.ids)?;
    let inner = TransferStatusReq {
        status: req.status.clone().unwrap_or_default(),
        reason: req.reason.clone(),
    };
    // 目标态整体校验（非法值 400）
    inner.target_status()?;
    let operator = utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            400,
            crate::shared::error_code::PARAM_INVALID,
            "无法从令牌解析当前用户：批量状态流转需要记录操作人",
        )
    })?;
    let labels = batch::labels_of("biz_stock_transfer", "transfer_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单号"))?;
    Ok(batch::run(&ids, &labels, |id| {
        let inner = inner.clone();
        async move { change_transfer_status(id, &inner, operator).await }
    })
    .await)
}

/// 序列号批量状态流转（出库 / 报废）。
pub async fn batch_serial_status(
    claims: &TokenClaims,
    req: &BatchStatusReq,
) -> std::result::Result<BatchResult, ApiError> {
    batch::ensure_permission(claims, PERM_SERIAL_STATUS)?;
    let ids = batch::parse_ids(&req.ids)?;
    let inner = SerialStatusReq {
        status: req.status.clone().unwrap_or_default(),
        reason: req.reason.clone(),
    };
    inner.target_status()?;
    let operator = utils::current_user_uuid(claims);
    let labels = batch::labels_of("biz_material_serial", "serial_no", &ids)
        .await
        .map_err(|error| utils::db_error(error, "查询序列号"))?;
    Ok(batch::run(&ids, &labels, |id| {
        let inner = inner.clone();
        async move { apply_status(id, &inner, operator).await.map(|_| ()) }
    })
    .await)
}

/// 批量导出（收货 / 应付 / 凭证共用）。
///
/// # 异常情况
///
/// - `403`：缺列表权限；
/// - `400`：`ids` 为空且没给筛选条件（那等于「导出全表」，不做）；
/// - `422`：命中 0 行。
pub async fn batch_export_doc(
    claims: &TokenClaims,
    permission: &str,
    biz_type: &str,
    req: &BatchExportReq,
) -> std::result::Result<BatchExportTask, ApiError> {
    batch::ensure_permission(claims, permission)?;
    let ids = if req.ids.is_empty() {
        Vec::new()
    } else {
        batch::parse_ids(&req.ids)?
    };
    let keyword = utils::trimmed(req.keyword.as_deref()).map(str::to_string);
    if ids.is_empty() && keyword.is_none() && req.status.is_none() {
        return Err(ApiError::bad_request(
            "ids 为空时必须带上当前筛选条件（keyword / status）：\
             否则「全选」会变成「全表」，导出前请先在列表页筛一下或直接勾选",
        ));
    }
    let filter = ExportFilter {
        keyword,
        status: req.status,
        ids: Vec::new(),
    };
    batch_export::create_by_ids(
        biz_type,
        &ids,
        filter,
        utils::current_user_uuid(claims),
        None,
    )
    .await
}

// ==================== HTTP 入口 ====================

/// 收货单批量过账 / 作废。
#[nik::permission(
    path = "/business/erp/stock-in/batch-status",
    method = "POST",
    permission = "business:erp:stock-in:status"
)]
#[nik::post("/business/erp/stock-in/batch-status")]
pub async fn stock_in_batch_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_stock_in_status(&claims, &req).await?))
}

/// 收货单批量删除。
#[nik::permission(
    path = "/business/erp/stock-in/batch-delete",
    method = "POST",
    permission = "business:erp:stock-in:delete"
)]
#[nik::post("/business/erp/stock-in/batch-delete")]
pub async fn stock_in_batch_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_stock_in_delete(&claims, &req).await?))
}

/// 收货单批量导出（异步任务）。
///
/// 权限是**两道**：路由码 `business:impexp:export:create`（建导出任务 = 写操作，
/// 用 `:list` 会被 `system::authz_tests::o13` 判为「有权查看即可改数据」），
/// 函数内再判 `business:erp:stock-in:list`（看不见的数据不许导出去）。
#[nik::permission(
    path = "/business/erp/stock-in/batch-export",
    method = "POST",
    permission = "business:impexp:export:create"
)]
#[nik::post("/business/erp/stock-in/batch-export")]
pub async fn stock_in_batch_export(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchExportReq>,
) -> ResResult<BatchExportTask> {
    Ok(Res::ok(
        batch_export_doc(&claims, PERM_STOCK_IN_LIST, "erp.stock_in", &req).await?,
    ))
}

/// 采购订单批量状态流转（提交审批 / 取消 / 关闭）。
#[nik::permission(
    path = "/business/erp/purchase-order/batch-status",
    method = "POST",
    permission = "business:erp:purchase-order:status"
)]
#[nik::post("/business/erp/purchase-order/batch-status")]
pub async fn purchase_order_batch_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_purchase_status(&claims, &req).await?))
}

/// 调拨单批量状态流转。
#[nik::permission(
    path = "/business/erp/transfer/batch-status",
    method = "POST",
    permission = "business:erp:transfer:status"
)]
#[nik::post("/business/erp/transfer/batch-status")]
pub async fn transfer_batch_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_transfer_status(&claims, &req).await?))
}

/// 序列号批量状态流转。
#[nik::permission(
    path = "/business/erp/serial/batch-status",
    method = "POST",
    permission = "business:erp:serial:status"
)]
#[nik::post("/business/erp/serial/batch-status")]
pub async fn serial_batch_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchStatusReq>,
) -> ResResult<BatchResult> {
    Ok(Res::ok(batch_serial_status(&claims, &req).await?))
}

/// 应付单批量导出。
#[nik::permission(
    path = "/business/erp/payable/batch-export",
    method = "POST",
    permission = "business:impexp:export:create"
)]
#[nik::post("/business/erp/payable/batch-export")]
pub async fn payable_batch_export(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchExportReq>,
) -> ResResult<BatchExportTask> {
    Ok(Res::ok(
        batch_export_doc(&claims, PERM_PAYABLE_LIST, "erp.payable", &req).await?,
    ))
}

/// 凭证批量导出。
#[nik::permission(
    path = "/business/erp/voucher/batch-export",
    method = "POST",
    permission = "business:impexp:export:create"
)]
#[nik::post("/business/erp/voucher/batch-export")]
pub async fn voucher_batch_export(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchExportReq>,
) -> ResResult<BatchExportTask> {
    Ok(Res::ok(
        batch_export_doc(&claims, PERM_VOUCHER_LIST, "erp.voucher", &req).await?,
    ))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::fixture_hygiene;
    use crate::business::erp::stockin_model::{
        self, NewStockIn, NewStockInLine, STATUS_CANCELLED, Totals,
    };
    use nik::prelude::chrono::{NaiveDate, Utc};
    use nik::prelude::rust_decimal::Decimal;
    use nik::uuid::Uuid;
    use std::str::FromStr;

    const UNIT: &str = "b2b70001-0000-4000-8000-000000000001";
    const SUPPLIER: &str = "b2b70001-0000-4000-8000-000000000002";
    const WAREHOUSE: &str = "b2b70001-0000-4000-8000-000000000003";
    const MATERIAL: &str = "b2b70001-0000-4000-8000-000000000011";
    const OPERATOR: &str = "b2b70009-0000-4000-8000-000000000001";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2b70001-0000-4000-8000-000000000001', 'D7-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('b2b70001-0000-4000-8000-000000000002', 'D7-SUP', 'D7 批量测试供应商', 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('b2b70001-0000-4000-8000-000000000003', 'D7-WH', 'D7 批量仓',
                 '/b2b70001-0000-4000-8000-000000000003/', 1, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
         VALUES ('b2b70001-0000-4000-8000-000000000011', 'D7-M1', 'D7 螺栓',
                 'b2b70001-0000-4000-8000-000000000001', 1, false)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                   is_default, capacity, status, sort_order)
         VALUES ('b2b70001-0000-4000-8000-000000000021', 'D7-L01', 'D7 A 区',
                 'b2b70001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO sys_user (id, username, password, real_name)
         VALUES ('b2b70009-0000-4000-8000-000000000001', 'd7_batch', 'x', 'D7 批量操作员')
         ON CONFLICT DO NOTHING",
    ];

    /// 真库用例必须串行（共用全局 `db()` 与 `biz_seq(IN)`）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局数据库；`false` = 没配 `ERP_DB_URL`（用例直接跳过）。
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
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库 + 建夹具；`None` = 没配库。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        for sql in FIXTURES {
            if let Err(error) = db().exec_sql((*sql).to_string(), vec![]).await {
                panic!("夹具失败（{sql}）：{error}");
            }
        }
        Some(guard)
    }

    /// 物理清掉本模块造的行（真库用例不留痕）。
    ///
    /// 顺序：序列号 → 流水 → 库存 → 库位账 → 凭证/应付 → 状态日志 → 单据。
    async fn cleanup() {
        let cleanups = [
            format!(
                "DELETE FROM biz_material_serial WHERE source_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory_txn WHERE doc_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory_location WHERE material_id = '{MATERIAL}'::uuid \
                 AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            format!(
                "DELETE FROM biz_inventory WHERE material_id = '{MATERIAL}'::uuid \
                 AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            // 状态日志（无外键）：必须在删凭证 / 应付单之前按 id 清（`fixture_hygiene`）
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                  (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN \
                 (SELECT id FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN \
                  (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                 (SELECT id FROM biz_voucher WHERE source_id IN \
                  (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_voucher WHERE source_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!("DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid"),
        ];
        for sql in cleanups {
            if let Err(error) = db().exec_sql(sql.clone(), vec![]).await {
                println!("清理失败（{sql}）：{error}");
            }
        }
    }

    /// 全权限 claims（批量接口的第二道闸靠它放行）。
    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: OPERATOR.to_string(),
            username: Some("d7-batch".to_string()),
            roles: vec!["TEST".to_string()],
            permissions: permissions.iter().map(|item| (*item).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    fn full_claims() -> TokenClaims {
        claims_with(&[
            PERM_STOCK_IN_STATUS,
            PERM_STOCK_IN_DELETE,
            PERM_STOCK_IN_LIST,
        ])
    }

    fn dec(value: i64) -> Decimal {
        Decimal::from(value)
    }

    /// 造一张 1 行明细的草稿收货单（走生产同款 `stockin_model::create`）。
    async fn make_draft() -> Uuid {
        let line = NewStockInLine {
            material_id: Uuid::parse_str(MATERIAL).expect("material"),
            unit_id: Uuid::parse_str(UNIT).expect("unit"),
            source_item_id: None,
            qty: dec(2),
            price: dec(10),
            price_incl: Decimal::from_str("11.3").expect("price_incl"),
            tax_rate: dec(13),
            amount: dec(20),
            tax_amount: Decimal::from_str("2.6").expect("tax"),
            with_tax_amount: Decimal::from_str("22.6").expect("with_tax"),
            location_id: None,
            location_dict_id: None,
            serial_ids: Vec::new(),
            remark: None,
        };
        let totals = Totals::of(std::slice::from_ref(&line));
        let new = NewStockIn {
            supplier_id: Uuid::parse_str(SUPPLIER).expect("supplier"),
            warehouse_id: Uuid::parse_str(WAREHOUSE).expect("warehouse"),
            source_order_id: None,
            // A4：无源单必须显式打标记（库约束 ck_biz_si_sourceless_pair）
            is_sourceless: true,
            in_date: NaiveDate::from_ymd_opt(2026, 3, 15).expect("date"),
            invoice_received: true,
            remark: Some("D7 批量测试"),
        };
        let operator = crate::business::shared::state_machine::Operator::named(
            Some(Uuid::parse_str(OPERATOR).expect("operator")),
            "d7-batch",
        );
        let tx = db().begin().await.expect("begin");
        let id =
            match stockin_model::create(&tx, &new, &[line], &totals, operator, Utc::now()).await {
                Ok(id) => id,
                Err(error) => {
                    let _ = tx.rollback().await;
                    panic!("造草稿单失败：{error}");
                }
            };
        tx.commit().await.expect("commit");
        id
    }

    /// 直接把单据状态改成 `status`（造「已是已过账 / 已作废」这类前置状态，不动账）。
    async fn force_status(id: Uuid, status: &str) {
        db().exec_sql(
            "UPDATE biz_stock_in SET status = ? WHERE id = ?::uuid".to_string(),
            vec![Value::from(status), Value::from(id)],
        )
        .await
        .expect("force status");
    }

    /// 读单据状态。
    async fn status_of(id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT status::text FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await
        .expect("status")
    }

    // ---------- ① 5 张全成功 ----------

    #[tokio::test]
    async fn batch_post_five_drafts_all_succeed() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ids: Vec<Uuid> = {
            let mut out = Vec::new();
            for _ in 0..5 {
                out.push(make_draft().await);
            }
            out
        };
        let req = BatchStatusReq {
            ids: ids.iter().map(Uuid::to_string).collect(),
            status: Some("POSTED".to_string()),
            reason: None,
        };
        let outcome = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("批量过账应 200（单条失败进 failed）");
        assert_eq!(outcome.total, 5);
        assert_eq!(
            outcome.success_count, 5,
            "5 张草稿应全部过账：{:?}",
            outcome.failed
        );
        assert_eq!(outcome.fail_count, 0);
        assert!(outcome.all_ok());
        assert_eq!(outcome.succeeded.len(), 5);
        for id in &ids {
            assert_eq!(status_of(*id).await, "POSTED", "每张都应落成已过账");
        }
        // 每一条都点名了单号（IN- 前缀）
        for item in &outcome.succeeded {
            assert!(
                item.no.as_deref().is_some_and(|no| no.starts_with("IN-")),
                "成功行必须带单号：{:?}",
                item
            );
        }
        cleanup().await;
    }

    // ---------- ② 3 成功 2 失败（逐条报告，成功的已提交） ----------

    #[tokio::test]
    async fn batch_post_reports_each_failure_and_keeps_successes() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ok_ids: Vec<Uuid> = {
            let mut out = Vec::new();
            for _ in 0..3 {
                out.push(make_draft().await);
            }
            out
        };
        let bad_ids: Vec<Uuid> = {
            let mut out = Vec::new();
            for _ in 0..2 {
                let id = make_draft().await;
                // 已作废：再过账必须被拒（状态机不认 CANCELLED → POSTED）
                force_status(id, STATUS_CANCELLED).await;
                out.push(id);
            }
            out
        };
        let all: Vec<Uuid> = ok_ids.iter().chain(bad_ids.iter()).copied().collect();
        let req = BatchStatusReq {
            ids: all.iter().map(Uuid::to_string).collect(),
            status: Some("POSTED".to_string()),
            reason: None,
        };
        let outcome = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("批量接口本身不报错");
        assert_eq!(outcome.total, 5);
        assert_eq!(outcome.success_count, 3);
        assert_eq!(outcome.fail_count, 2);
        assert_eq!(outcome.summary(), "共 5 条：成功 3 条，失败 2 条");
        // 成功的已提交：状态真的变了（不是「整批回滚」）
        for id in &ok_ids {
            assert_eq!(status_of(*id).await, "POSTED");
        }
        // 失败的原样未动，且逐条带原因 + 单号
        for id in &bad_ids {
            assert_eq!(status_of(*id).await, STATUS_CANCELLED);
        }
        for item in &outcome.failed {
            assert_eq!(
                item.code,
                crate::shared::error_code::RESOURCE_CONFLICT,
                "状态不对是冲突（10003）：{item:?}"
            );
            assert!(
                item.error.contains("已作废") || item.error.contains("没有可流转"),
                "失败原因必须是原文：{}",
                item.error
            );
            assert!(
                item.no.as_deref().is_some(),
                "失败行也要点名单号：{:?}",
                item
            );
        }
        cleanup().await;
    }

    // ---------- ③ 空 ids → 400 ----------

    #[tokio::test]
    async fn batch_empty_ids_is_bad_request() {
        // 这一条不需要库：权限 + ids 校验都在解析阶段
        let error = batch_stock_in_status(
            &full_claims(),
            &BatchStatusReq {
                ids: Vec::new(),
                status: Some("POSTED".to_string()),
                reason: None,
            },
        )
        .await
        .expect_err("空 ids 必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("勾选一条"), "报文：{}", error.msg);
    }

    // ---------- ④ ids 里有不存在的 id → 该条失败，其他继续 ----------

    #[tokio::test]
    async fn batch_missing_id_fails_alone() {
        let Some(_guard) = setup().await else {
            return;
        };
        let real = make_draft().await;
        let ghost = Uuid::now_v7();
        let req = BatchStatusReq {
            ids: vec![real.to_string(), ghost.to_string()],
            status: Some("CANCELLED".to_string()),
            reason: Some("批量作废：D7 单测".to_string()),
        };
        let outcome = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("批量接口本身不报错");
        assert_eq!(outcome.total, 2);
        assert_eq!(outcome.success_count, 1, "真实单据应作废成功");
        assert_eq!(outcome.fail_count, 1);
        assert_eq!(status_of(real).await, STATUS_CANCELLED);
        let failed = &outcome.failed[0];
        assert_eq!(failed.id, ghost.to_string());
        assert_eq!(
            failed.code,
            crate::shared::error_code::RESOURCE_NOT_FOUND,
            "不存在是 404 + 10002：{failed:?}"
        );
        assert!(failed.no.is_none(), "不存在的单据没有单号");
        assert!(failed.error.contains("不存在"), "报文：{}", failed.error);
        cleanup().await;
    }

    // ---------- ⑤ 无权限 → 403 ----------

    #[tokio::test]
    async fn batch_without_permission_is_forbidden() {
        let error = batch_stock_in_status(
            &claims_with(&["business:erp:stock-in:list"]),
            &BatchStatusReq {
                ids: vec![Uuid::now_v7().to_string()],
                status: Some("POSTED".to_string()),
                reason: None,
            },
        )
        .await
        .expect_err("只有 list 码不能批量过账");
        assert_eq!(error.status, 403);
        assert!(
            error.msg.contains(PERM_STOCK_IN_STATUS),
            "403 要点名缺哪个码：{}",
            error.msg
        );
        // 超管（`*` 通配）必须放行到「ids 校验」这一层，而不是被 403 挡住
        let super_admin = claims_with(&["*"]);
        let after = batch_stock_in_status(
            &super_admin,
            &BatchStatusReq {
                ids: Vec::new(),
                status: Some("POSTED".to_string()),
                reason: None,
            },
        )
        .await
        .expect_err("空 ids 仍然 400");
        assert_eq!(after.status, 400, "超管应过权限闸、停在参数校验");
    }

    // ---------- ⑥ 幂等：已处理的再来一遍是「0 成功 / N 失败」 ----------

    #[tokio::test]
    async fn batch_post_is_idempotent_on_second_run() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ids: Vec<Uuid> = {
            let mut out = Vec::new();
            for _ in 0..2 {
                out.push(make_draft().await);
            }
            out
        };
        let req = BatchStatusReq {
            ids: ids.iter().map(Uuid::to_string).collect(),
            status: Some("POSTED".to_string()),
            reason: None,
        };
        let first = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("首次");
        assert_eq!(first.success_count, 2);
        let second = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("第二次");
        assert_eq!(second.success_count, 0, "重复点不能把库存加两遍");
        assert_eq!(second.fail_count, 2);
        for item in &second.failed {
            assert_eq!(item.code, crate::shared::error_code::RESOURCE_CONFLICT);
            assert!(
                item.error.contains("已过账"),
                "幂等失败原因要可读：{}",
                item.error
            );
        }
        // 单据仍是已过账（一次生效）
        for id in &ids {
            assert_eq!(status_of(*id).await, "POSTED");
        }
        cleanup().await;
    }

    // ---------- ⑦ 批量删除：只删草稿，逐条判 ----------

    #[tokio::test]
    async fn batch_delete_skips_posted_docs() {
        let Some(_guard) = setup().await else {
            return;
        };
        let draft = make_draft().await;
        let posted = make_draft().await;
        force_status(posted, "POSTED").await;
        let req = BatchStatusReq {
            ids: vec![draft.to_string(), posted.to_string()],
            status: None,
            reason: None,
        };
        let outcome = batch_stock_in_delete(&full_claims(), &req)
            .await
            .expect("批量删除接口本身不报错");
        assert_eq!(outcome.success_count, 1);
        assert_eq!(outcome.fail_count, 1);
        let soft_deleted: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_stock_in WHERE id = ?::uuid AND deleted_at IS NOT NULL",
                vec![Value::from(draft)],
            )
            .await
            .expect("查软删");
        assert_eq!(soft_deleted, 1, "草稿应被软删");
        assert_eq!(status_of(posted).await, "POSTED", "已过账的不许删");
        assert!(
            outcome.failed[0].error.contains("只有草稿"),
            "报文：{}",
            outcome.failed[0].error
        );
        cleanup().await;
    }

    // ---------- ⑧ 批量导出的参数闸（不建任务） ----------

    #[tokio::test]
    async fn batch_export_requires_ids_or_filter() {
        let error = batch_export_doc(
            &full_claims(),
            PERM_STOCK_IN_LIST,
            "erp.stock_in",
            &BatchExportReq::default(),
        )
        .await
        .expect_err("既没勾选也没筛选，不许导出全表");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("筛选条件"), "报文：{}", error.msg);
    }

    // ---------- ⑨ 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 批量过账会生成**应付单 + 凭证**，它们各自写一条 `biz_status_log`；本模块的清场
    /// 以前只删 `STOCK_IN` 的日志，凭证 / 应付的日志就变成孤儿，每跑一遍全量多攒
    /// 10 + 10 行（本卡实测）。这条收尾用例把「删单据前先按 id 删日志」钉死：
    /// 自造一张会过账的单（真实 `batch_stock_in_status`）→ 走真实的 `cleanup()`
    /// → 断言三类日志按 id 全为 0 → 兜底扫孤儿断言为 0。
    #[tokio::test]
    async fn zz_teardown_purges_finance_status_logs() {
        let Some(_guard) = setup().await else {
            return;
        };
        let id = make_draft().await;
        let req = BatchStatusReq {
            ids: vec![id.to_string()],
            status: Some("POSTED".to_string()),
            reason: None,
        };
        let outcome = batch_stock_in_status(&full_claims(), &req)
            .await
            .expect("批量过账应成功");
        assert_eq!(outcome.success_count, 1, "{:?}", outcome.failed);

        // 过账产生的应付单 / 凭证 id（清场后单据就查不到了，必须先取出来）
        let payable_id: Option<Uuid> = db()
            .query_first_sql(
                "SELECT id FROM biz_payable \
                  WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid LIMIT 1"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查应付单")
            .map(|row| row.get_typed("id").expect("读应付 id"));
        let voucher_id: Option<Uuid> = db()
            .query_first_sql(
                "SELECT id FROM biz_voucher WHERE source_id = ?::uuid LIMIT 1".to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查凭证")
            .map(|row| row.get_typed("id").expect("读凭证 id"));
        let payable_id = payable_id.expect("过账必须生成应付单");
        let voucher_id = voucher_id.expect("过账必须生成凭证");

        let count_logs = |entity_type: &'static str, target: Uuid| async move {
            db().scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = ? AND entity_id = ?::uuid",
                vec![Value::from(entity_type), Value::from(&target)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0)
        };
        for (label, entity_type, target) in [
            ("PAYABLE", "PAYABLE", payable_id),
            ("VOUCHER", "VOUCHER", voucher_id),
        ] {
            assert!(
                count_logs(entity_type, target).await > 0,
                "清场前必须有 {label} 状态日志，否则这条用例证明不了什么"
            );
        }

        // 走模块真实的清场
        cleanup().await;

        for (label, entity_type, target) in [
            ("PAYABLE", "PAYABLE", payable_id),
            ("VOUCHER", "VOUCHER", voucher_id),
        ] {
            assert_eq!(
                count_logs(entity_type, target).await,
                0,
                "清场后 {label} 状态日志必须为 0（日志无外键，必须按 id 先删）"
            );
        }

        // 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        fixture_hygiene::purge_orphans(fixture_hygiene::FINANCE).await;
        assert_eq!(
            fixture_hygiene::orphans(fixture_hygiene::FINANCE).await,
            0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志"
        );
    }
}
