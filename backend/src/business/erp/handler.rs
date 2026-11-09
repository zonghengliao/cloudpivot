//! ERP 主数据接口层（批次 D · W1c）。
//!
//! 依据：`backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql`，
//! 契约与取数来自 `backend/src/business/erp/{model,dto}.rs`（W1b）。**表结构以迁移为准**：
//! 取数一律复用 [`crate::business::erp::model`] 的查询（分页已走 `scope::paginate_unscoped`）。
//!
//! # 接口清单（前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`）
//!
//! ## ① 可写主数据（5 张，完整 CRUD）
//!
//! | 实体 | 路径 | 权限码 |
//! |------|------|--------|
//! | 仓库 | `/business/erp/warehouse/page` · `/{id}` · `POST` · `PUT` · `DELETE` | `business:erp:warehouse:{list,detail,create,update,delete}` |
//! | 库位 | `/business/erp/location/...` | `business:erp:location:*` |
//! | 物料 | `/business/erp/material/...` | `business:erp:material:*` |
//! | 计量单位 | `/business/erp/unit/...` | `business:erp:unit:{list,create,update}` |
//! | 供应商 | `/business/erp/supplier/...` | `business:erp:supplier:{list,create,update}` |
//!
//! ## ② 特殊查询（6 条）
//!
//! | 方法 | 路径 | 说明 |
//! |------|------|------|
//! | GET | `/business/erp/warehouse/tree?root_id=` | 仓库树（物化路径 + `text_pattern_ops`，组装 1–3 层） |
//! | GET | `/business/erp/location/list?warehouse_id=` | 库位下拉（按仓库，可选含停用） |
//! | GET | `/business/erp/material/list?keyword=` | 物料搜索（编码 / 名称模糊，最多 100 条） |
//! | GET | `/business/erp/supplier/next-code` | 供应商编码预览（**只读不占号**，CODE-05「生成编码」按钮用） |
//! | GET | `/business/erp/material/next-code` | 物料编码预览（**只读不占号**，CODE-06「生成编码」按钮用，`MAT-0001`） |
//! | GET | `/business/erp/warehouse/next-code` | 仓库编码预览（**只读不占号**，CODE-06「生成编码」按钮用，`WH-01`） |
//! | GET | `/business/erp/unit/next-code` | **显式 400**：计量单位不自动取号（E2E08-FIX-2 ④，避免被 `/unit/{id}` 吃掉后误判成路由丢失） |
//!
//! ## ③ 只读台账（8 张，列表 + 详情）
//!
//! `/business/erp/{account,period,seq,purchase-order,purchase-order-item,inventory,inventory-location,inventory-txn}/{page,{id}}`
//! —— 这 8 张是**事务 / 账务**表：库存与成本的唯一真相来自库存流水（V0137 §4 头注释），
//! 直接 `POST/PUT` 会让账实不符，写路径留给后续批次的单据编排。
//!
//! # 与卡面的三处差异（以库为准，逐条实测过 `information_schema`）
//!
//! 1. **卡面点名的 11 张表里 6 张在 V0137 里不存在**：
//!    `biz_warehouse_location`（真实表名是 **`biz_location`**）、`biz_material_category`、
//!    `biz_customer`、`biz_storage_area`、`biz_warehouse_type`（是 `biz_warehouse.warehouse_type` 列）、
//!    `biz_material_attribute`、`biz_material_barcode`。W1a 按架构文档落地的是 13 张表，
//!    本卡就按**真实存在的 13 张**给接口（5 张可写 + 8 张只读），不伪造不存在的表。
//! 2. **没有 `sys_permission` 表**：权限码的唯一来源是 `sys_menu.auth_code`
//!    （`system/menu/model.rs` 文件头明写），V0141 只种 `sys_menu` + `sys_role_menu`。
//! 3. **卡面给 unit / supplier 各 3 个码**：按 3 码制落地 —— `:list`（含详情）、`:create`、
//!    `:update`（含删除）。其余 3 张主力表是标准 5 码制。合计仍是卡面的 24 个码。
//!
//! # 数据权限（门禁闸①/闸②）
//!
//! ```text
//! SCOPE-EXEMPT: ERP 13 张表是全局主数据 / 仓储结构 / 账务台账，V0137 无 dept_id 与归属人列，
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:* 权限码控制，
//! 强注范围只会静默少数据。列表分页全部走 model 的 scope::paginate_unscoped（闸①口径），
//! 本文件不含任何手写 COUNT/LIMIT 分页。
//! ```
//!
//! # 写操作的六条不变量（每条都在写路径上硬校验，不只靠 DB 约束）
//!
//! 1. 编码类字段（unit/warehouse/material/supplier + 仓库内唯一的库位编码）重复 → `409 + 66001`
//!    （DB 的部分唯一索引只是兜底，重复键也会被翻译成同一个码）；
//! 2. 弱引用（上级仓库 / 基准单位 / 所属仓库 / 计量单位 / 默认仓库 / 应付科目）不存在 → `400 + 66002`；
//! 3. 仓库层级 1–3 层，第 4 层 → `400 + 66004`；`level` 与上级层级不一致 → `400 + 10001`；
//! 4. 库位只能挂 `is_leaf = 1` 的仓库 → `400 + 66005`；
//! 5. 每仓最多 1 个默认库位（部分唯一索引 `uk_biz_location_default`）→ 置默认时先清同仓其它默认位；
//! 6. 删除一律**软删**（`deleted_at` 非空），有下级仓库 / 有物料在用的单位 → `409`。

use axum::Json;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::business::erp::code_policy;
use crate::business::erp::dto::{
    AccountFilter, AccountRes, CreateLocationReq, CreateMaterialReq, CreateSupplierReq, CreateUnitReq,
    CreateWarehouseReq, InventoryFilter, InventoryLocationFilter, InventoryLocationRes, InventoryRes,
    InventoryTxnFilter, InventoryTxnRes, LocationFilter, LocationRes, MaterialFilter, MaterialRes, PeriodFilter,
    PeriodRes, PurchaseOrderFilter, PurchaseOrderItemFilter, PurchaseOrderItemRes, PurchaseOrderRes, SeqFilter, SeqRes,
    SupplierFilter, SupplierRes, UnitFilter, UnitRes, UpdateLocationReq, UpdateMaterialReq, UpdateSupplierReq,
    UpdateUnitReq, UpdateWarehouseReq, WarehouseFilter, WarehouseRes,
};
use crate::business::erp::inventory_audit as inv_audit;
use crate::business::erp::inventory_audit::AuditReport;
use crate::business::erp::model::{self, InventoryLocationRow, InventoryRow, InventoryTxnRow, WarehouseRow};
use crate::business::erp::seq;
use crate::business::erp::tax_rate as tax_rate_mod;
use crate::business::erp::unit_guard;
use crate::shared::error_code;
use crate::utils;

// ==================== 口径常量 ====================

/// 仓库层级上限（V0137 §1.2 头注释：总仓 / 分仓 / 库房三级）。
pub const MAX_WAREHOUSE_LEVEL: i16 = 3;

/// 物料搜索下拉的返回上限（`GET /business/erp/material/list`）。
pub const MATERIAL_LIST_LIMIT: u64 = 100;

// ==================== 权限码（与 V0141 迁移逐字一致） ====================

/// 卡面点名的权限码（5 张主力表 5 码制 + 单位/供应商 3 码制）。
///
/// BUG-SYS-05（2026-10-31）：原来的「预留的物料分类 3 码」(`business:erp:category:list/detail/create`)
/// 已从清单里移除 —— `biz_material_category` 表未建、`/business/erp/category` 路由不存在，
/// 这两个码在代码里**没有任何校验点**，只会在管理面勾给角色却不生效。等物料分类功能落地时
/// 再连带补 `#[nik::permission]` 注解、迁移和菜单（同一批加，避免再次出现死码）。
pub const CARD_PERMISSIONS: [(&str, &str); 21] = [
    ("business:erp:warehouse:list", "仓库列表"),
    ("business:erp:warehouse:detail", "仓库详情"),
    ("business:erp:warehouse:create", "新建仓库"),
    ("business:erp:warehouse:update", "编辑仓库"),
    ("business:erp:warehouse:delete", "删除仓库"),
    ("business:erp:location:list", "库位列表"),
    ("business:erp:location:detail", "库位详情"),
    ("business:erp:location:create", "新建库位"),
    ("business:erp:location:update", "编辑库位"),
    ("business:erp:location:delete", "删除库位"),
    ("business:erp:material:list", "物料列表"),
    ("business:erp:material:detail", "物料详情"),
    ("business:erp:material:create", "新建物料"),
    ("business:erp:material:update", "编辑物料"),
    ("business:erp:material:delete", "删除物料"),
    ("business:erp:unit:list", "计量单位列表 / 详情"),
    ("business:erp:unit:create", "新建计量单位"),
    ("business:erp:unit:update", "编辑 / 删除计量单位"),
    ("business:erp:supplier:list", "供应商列表 / 详情"),
    ("business:erp:supplier:create", "新建供应商"),
    ("business:erp:supplier:update", "编辑 / 删除供应商"),
];

/// 13 张真实表的只读码（卡面 24 码之外的补充：8 张事务 / 账务表的列表与详情）。
pub const READONLY_PERMISSIONS: [(&str, &str); 8] = [
    ("business:erp:account:list", "会计科目"),
    ("business:erp:period:list", "会计期间"),
    ("business:erp:seq:list", "单号序列"),
    ("business:erp:purchase-order:list", "采购订单"),
    ("business:erp:purchase-order-item:list", "采购明细"),
    ("business:erp:inventory:list", "即时库存"),
    ("business:erp:inventory-location:list", "库位库存"),
    ("business:erp:inventory-txn:list", "库存流水"),
];

/// 全部 32 个权限码（24 卡面码 + 8 只读码），单测钉住「无重复 / 前缀正确」。
pub fn all_permissions() -> Vec<&'static str> {
    CARD_PERMISSIONS
        .iter()
        .chain(READONLY_PERMISSIONS.iter())
        .map(|(code, _)| *code)
        .collect()
}

// ==================== 共用辅助 ====================

/// DTO 的 id 是 `String`（`Uuid` 没开 serde），落库前显式解析；非法 → `400 + 66006`。
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

/// 可空 UUID 参数：SQL 侧写 `NULLIF(?::text,'')::uuid`（`Value::Null` 绑 `::uuid` 驱动编不出来）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 可空数值参数：SQL 侧写 `NULLIF(?::text,'')::numeric`（理由同上，且钉住 Decimal 的定点标度）。
fn decimal_text(value: Option<Decimal>) -> Value {
    Value::Text(value.map(|num| num.to_string()).unwrap_or_default())
}

/// 操作人（`create_by` / `update_by`）。
fn operator_of(claims: &TokenClaims) -> Value {
    uuid_text(utils::current_user_uuid(claims))
}

/// 分页出参（`total` 由 model 的 `i64` 收口成 `u64`，`page/page_size` 用统一归一化口径）。
fn page_body<T: Serialize>(list: Vec<T>, total: i64, page: u64, page_size: u64) -> Res<PageData<Vec<T>>> {
    let (page, page_size) = model::normalize_page(page, page_size);
    Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size)
}

/// 写路径的数据库错误收口。
///
/// - `DbError::UniqueViolation`（PG `23505`）→ `409 + 66001`：编码重复；
/// - `DbError::CheckViolation`（PG `23514`）→ `400 + 10001`：DDL 里的 CHECK 兜住了
///   dto 没校验到的组合（把约束名回给前端比 500 有用得多）；
/// - 其余仍是 500（内部错误，细节只进日志）。
///
/// 判据用**变体**而不是文案：实测 `DbError` 的 `Display` 是
/// `unique constraint violated constraint=uk_biz_xxx`，不含 `duplicate key` 字样，
/// 第一版按文案匹配 → 唯一冲突被判成 500（真库单测当场抓到）。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "编码已存在（唯一约束{}）：同一范围内不允许重复编码，已软删的编码可以复用",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（枚举 / 范围 / 组合关系）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

/// 通用「按主键取一行」：`soft` 为真时过滤 `deleted_at`。
async fn find_row(table: &str, columns: &str, id: Uuid, soft: bool) -> std::result::Result<Option<Row>, ApiError> {
    let mut sql = format!("SELECT {columns} FROM {table} WHERE id = ?::uuid");
    if soft {
        sql.push_str(" AND deleted_at IS NULL");
    }
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&id)])
        .await
        .map_err(|error| utils::db_error(error, "按主键查询 ERP 行"))?;
    Ok(rows.into_iter().next())
}

/// 不存在统一 404（详情 / 编辑 / 删除三处共用一条文案口径）。
fn not_found(what: &str) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, format!("{what}不存在或已删除"))
}

/// 弱引用缺失统一 400（`66002`）。
fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 入参不合法统一 400（`10001`）。
fn param_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 自动取号失败（`biz_seq` 没有可用规则 / 连号全被占）→ `500 + 10004`。
///
/// 对外只说「找管理员」，真实原因（带 `seq_type` 与建议）进 `internal_detail`，
/// 与 [`utils::db_error`] 同一口径：不把库里的细节丢给前端。
fn seq_error(error: DbError, ctx: &str) -> ApiError {
    ApiError {
        status: 500,
        code: error_code::INTERNAL_ERROR,
        msg: format!("{ctx}：自动取号失败，请联系管理员检查取号规则"),
        internal_detail: Some(format!("{ctx}: {error}")),
        data: None,
    }
}

/// 布尔入参 → `SMALLINT`（库里的 `is_base` / `is_default` / `allow_negative` 都是 0/1）。
fn flag_to_i16(value: bool) -> i16 {
    i16::from(value)
}

/// 仓库层级推导：顶级 = 1，下级 = 上级 + 1；超过 [`MAX_WAREHOUSE_LEVEL`] → `400 + 66004`。
fn next_level(parent: Option<&WarehouseRow>) -> std::result::Result<i16, ApiError> {
    match parent {
        None => Ok(1),
        Some(row) => {
            if row.level >= MAX_WAREHOUSE_LEVEL {
                return Err(ApiError::new(
                    400,
                    error_code::ERP_WAREHOUSE_LEVEL_LIMIT,
                    format!(
                        "仓库最多 {MAX_WAREHOUSE_LEVEL} 层：上级已是第 {} 层，不能再挂下级",
                        row.level
                    ),
                ));
            }
            Ok(row.level + 1)
        }
    }
}

/// 物化路径推导：顶级 `/id/`，子节点 `父路径 + id + "/"`（与 V0137 的写法逐字一致）。
fn child_path(parent: Option<&WarehouseRow>, id: Uuid) -> String {
    match parent {
        Some(row) => format!("{}{id}/", row.warehouse_path),
        None => format!("/{id}/"),
    }
}

/// 仓库树节点（出参）：`WarehouseRes` 平铺 + `children` 递归。
#[derive(Debug, Clone, Serialize)]
pub struct WarehouseTreeNode {
    /// 仓库本体（字段与详情接口完全一致）。
    #[serde(flatten)]
    pub node: WarehouseRes,
    /// 下级仓库（已按 `sort_order` / 路径排序）。
    pub children: Vec<WarehouseTreeNode>,
}

/// 把按路径排好序的行组装成 1–3 层树。
///
/// **子树查询的根节点 `parent_id` 指向集合外**（`root_id` 的上级不会一起返回），
/// 这种行必须提升为根：`utils::build_tree` 会把「父不在入参集合里」的节点塞进
/// `children_map`，而那个父永远不会来认领 → 节点**静默消失**（批次 B 踩过的老坑，
/// 本卡真库单测 `warehouse_tree_returns_all_levels` 当场抓到子树返回空）。
fn assemble_tree(rows: &[WarehouseRow]) -> Vec<WarehouseTreeNode> {
    #[derive(Clone)]
    struct Seed {
        node: WarehouseTreeNode,
        id: String,
        parent_id: Option<String>,
        sort_order: i32,
    }
    let visible: std::collections::HashSet<Uuid> = rows.iter().map(|row| row.id).collect();
    let seeds = rows
        .iter()
        .map(|row| Seed {
            node: WarehouseTreeNode {
                node: WarehouseRes::from(row),
                children: Vec::new(),
            },
            id: row.id.to_string(),
            // 父不可见（子树根）→ 当作根，别让它挂在永远不来认领的父下
            parent_id: row
                .parent_id
                .filter(|parent| visible.contains(parent))
                .map(|parent| parent.to_string()),
            sort_order: row.sort_order,
        })
        .collect::<Vec<_>>();
    utils::build_tree(
        seeds,
        |seed| seed.id.clone(),
        |seed| seed.parent_id.clone(),
        |seed| seed.sort_order,
        |seed, children| {
            seed.node.children = children.into_iter().map(|child| child.node).collect();
        },
    )
    .into_iter()
    .map(|seed| seed.node)
    .collect()
}

// ==================== ① 仓库 biz_warehouse ====================

/// 仓库分页（编码 / 名称关键词 + 类型 / 层级 / 上级 / 启用位）。
///
/// # 异常情况
///
/// - 400 + `66006`：`parent_id` 不是合法 UUID；
/// - 403：无 `business:erp:warehouse:list`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/warehouse/page",
    method = "GET",
    permission = "business:erp:warehouse:list"
)]
#[nik::get("/business/erp/warehouse/page")]
pub async fn warehouse_page(Query(filter): Query<WarehouseFilter>) -> ResResult<PageData<Vec<WarehouseRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::warehouse_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询仓库"))?;
    let list = rows.iter().map(WarehouseRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 仓库树（整棵 / 指定子树）。
///
/// 走 `warehouse_path ~>=~ ? AND ~<~ ?`，命中 `idx_biz_warehouse_path_pattern`
/// （`text_pattern_ops`）；参数化 `LIKE` 在 PG 里拿不到前缀字面量，会退化成全表扫。
#[nik::permission(
    path = "/business/erp/warehouse/tree",
    method = "GET",
    permission = "business:erp:warehouse:list"
)]
#[nik::get("/business/erp/warehouse/tree")]
pub async fn warehouse_tree(Query(query): Query<WarehouseTreeQuery>) -> ResResult<Vec<WarehouseTreeNode>> {
    let root_id = match query.root_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_uuid(raw, "root_id")?),
        None => None,
    };
    let rows = model::warehouse_tree(root_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库树"))?;
    Ok(Res::ok(assemble_tree(&rows)))
}

/// 仓库详情。
#[nik::permission(
    path = "/business/erp/warehouse/{id}",
    method = "GET",
    permission = "business:erp:warehouse:detail"
)]
#[nik::get("/business/erp/warehouse/{id}")]
pub async fn warehouse_detail(Path(id): Path<String>) -> ResResult<WarehouseRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::warehouse_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库详情"))?
        .ok_or_else(|| not_found("仓库"))?;
    Ok(Res::ok(WarehouseRes::from(&row)))
}

/// 新建仓库（层级 / 物化路径由上级推导，`level` 必须与推导值一致）。
///
/// # 异常情况
///
/// - 400 + `10001`：`level` 与上级层级不一致 / 字段不合规 / `warehouse_code` 格式不合法；
/// - 400 + `66002`：上级仓库不存在；
/// - 400 + `66004`：超过 3 层；
/// - 409 + `66001`：`warehouse_code` 重复（文案点名具体编码）。
#[nik::permission(
    path = "/business/erp/warehouse",
    method = "POST",
    permission = "business:erp:warehouse:create"
)]
#[nik::post("/business/erp/warehouse")]
pub async fn warehouse_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateWarehouseReq>,
) -> ResResult<WarehouseRes> {
    let res = create_warehouse(utils::current_user_uuid(&claims), req).await?;
    Ok(Res::ok(res))
}

/// 仓库编码预览（卡 CODE-06 · 交互方式 B：前端「生成编码」按钮先看号，
/// 保存时把这个号当**手工编码**提交）。
///
/// 与供应商 / 客户（CODE-05）同一口径：`seq::peek_master_code` 只 `SELECT biz_seq`，
/// **不推进 `current_no`** —— 点了按钮又取消不浪费号；并发下两人看到同一个号时，
/// 先保存的人赢，后保存的走手工编码撞唯一索引的老路（`409 + 66001`，文案点名编码）。
///
/// 注意：保存时**不要**再调 `next_master_code*`（那会真占号）；不填编码直接保存时由
/// [`create_warehouse`] 自己取号。
#[nik::permission(
    path = "/business/erp/warehouse/next-code",
    method = "GET",
    permission = "business:erp:warehouse:create"
)]
#[nik::get("/business/erp/warehouse/next-code")]
pub async fn warehouse_next_code(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    let code = preview_warehouse_code().await?;
    Ok(Res::ok(json!({ "code": code })))
}

/// 仓库编码预览的**业务主体**（单测直接调本函数，不走认证）。
///
/// 号段口径：`biz_seq` 的 WH 规则 —— 前缀 `WH`、分隔符 `-`、**纯流水 2 位**
/// （`WH-01`，由 `pad_width = 2` 决定）、不计日期、永不重置；连号被占则顺延到下一个空号。
/// 第 100 个起只补零不截断（`WH-100`），见 [`seq::format_master_code`]。
///
/// 错误口径与真实取号一致：规则缺失 / 连号全被占 ⇒ `500 + 10004`，细节只进日志。
pub async fn preview_warehouse_code() -> std::result::Result<String, ApiError> {
    seq::peek_master_code(db(), "WH")
        .await
        .map_err(|error| seq_error(error, "预览仓库编码"))
}

/// 新建仓库的**业务主体**（CODE-03：编码留空则由后端取号 `WH-01` 形态）。
///
/// axum handler [`warehouse_create`] 只做参数解包，单测直接调本函数
/// （同一条代码路径，只跳过认证）。
///
/// # 编码口径（用户已拍板，不许改）
///
/// | 入参 `warehouse_code` | 行为 |
/// |---|---|
/// | `null` / `""` / 纯空白 | 自动取号：`WH-01` 形态（`biz_seq` 的 WH 号段，**纯流水 2 位**、不计日期、永不重置）；撞号自动顺延 |
/// | 有值 | **原样保留**（企业自有编码体系要能对接，`CK-01` 也行），只校验长度 ≤ 40 + 字符集 |
///
/// 位数由 `biz_seq.pad_width` 决定（WH = 2）：`WH-01` … `WH-99`，第 100 个起自然变成
/// `WH-100`（[`seq::format_master_code`] 只补零不截断，避免 2 位号段被腰斩成重号）。
///
/// # 取号与落库同一事务
///
/// 取号是 `UPDATE biz_seq … RETURNING`（行锁），插入失败则整体回滚、号一起退回，
/// 不留空号；并发建两个仓库也不会拿到同一个号。
pub async fn create_warehouse(
    operator: Option<Uuid>,
    req: CreateWarehouseReq,
) -> std::result::Result<WarehouseRes, ApiError> {
    req.validate()?;
    // ① 手工编码：有值就保留（先去重，给出「编码 XXX 已存在」的人话错误）
    // CODE-08：开关关闭时填了编码直接 400（读在 begin 之前，被拒不占号）
    code_policy::guard_manual(code_policy::allow_manual_code().await, req.warehouse_code.as_deref())?;
    let manual = seq::resolve_manual_code(req.warehouse_code.as_deref(), seq::MAX_MANUAL_CODE_LEN)
        .map_err(|reason| param_invalid(format!("warehouse_code 不合法：{reason}")))?;
    if let Some(code) = manual.as_deref() {
        let taken = seq::master_code_taken(db(), "WH", code)
            .await
            .map_err(|error| utils::db_error(error, "校验仓库编码唯一性"))?;
        if taken {
            return Err(warehouse_code_conflict(code));
        }
    }
    let id = Uuid::now_v7();
    let parent = match req.parent_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let parent_id = parse_uuid(raw, "parent_id")?;
            Some(
                model::warehouse_find_by_id(parent_id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询上级仓库"))?
                    .ok_or_else(|| reference_missing("上级仓库不存在或已删除"))?,
            )
        }
        None => None,
    };
    let level = next_level(parent.as_ref())?;
    if level != req.level {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "level 与上级层级不一致：{} ⇒ 应为 {level}（level 由上级推导，不接受手填）",
                match parent.as_ref() {
                    Some(row) => format!("上级 level={}", row.level),
                    None => "无上级（顶级仓库）".to_string(),
                }
            ),
        ));
    }
    let path = child_path(parent.as_ref(), id);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建仓库事务"))?;
    // ② 编码 + INSERT 同一事务：没填就取号，插入失败则号一起回滚
    let warehouse_code = match manual {
        Some(code) => code,
        None => seq::next_master_code_free(&tx, "WH")
            .await
            .map_err(|error| seq_error(error, "新建仓库"))?,
    };
    tx.execute_with_params(
        "INSERT INTO biz_warehouse \
         (id, warehouse_code, warehouse_name, warehouse_type, manager, allow_negative, address, \
          status, sort_order, remark, created_at, updated_at, create_by, update_by, \
          parent_id, level, warehouse_path, is_leaf) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?, ?, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::uuid, ?, ?, 1)",
        vec![
            Value::from(&id),
            Value::from(warehouse_code.clone()),
            Value::from(req.warehouse_name.trim().to_string()),
            Value::from(req.warehouse_type.clone()),
            Value::from(req.manager.clone().map(|text| text.trim().to_string())),
            Value::I16(flag_to_i16(req.allow_negative)),
            Value::from(req.address.clone()),
            Value::I16(req.status),
            Value::I32(req.sort_order),
            Value::from(req.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
            uuid_text(parent.as_ref().map(|row| row.id)),
            Value::I16(level),
            Value::from(path),
        ],
    )
    .await
    .map_err(|error| warehouse_write_error(error, &warehouse_code))?;
    if let Some(row) = parent.as_ref() {
        tx.execute_with_params(
            "UPDATE biz_warehouse SET is_leaf = 0, updated_at = now() WHERE id = ?::uuid",
            vec![Value::from(&row.id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "更新上级仓库叶子标记"))?;
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建仓库事务"))?;
    let row = model::warehouse_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建仓库"))?
        .ok_or_else(|| not_found("仓库"))?;
    Ok(WarehouseRes::from(&row))
}

/// 主数据编码重复的**唯一文案出口**（`409 + 66001`；E2E08-FIX-2 ③ 统一口径）。
///
/// `kind` 是实体名（`供应商` / `客户` / `物料` / `仓库` / `资产分类` / `费用类型` / `计量单位`），
/// 拼出来就是「`{kind}编码 `X` 已存在（已软删的编码可以复用）`」——六类共用一句话术，
/// 差别只在 `kind`。**点名具体编码**是刻意的：用户要知道撞了哪一条，而不是只看到裸约束名。
///
/// 已软删的编码可以复用：与六张主数据表的「未删除内唯一」索引口径一致。
pub(crate) fn code_conflict(kind: &str, code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_CODE_CONFLICT,
        format!("{kind}编码 `{code}` 已存在（已软删的编码可以复用）"),
    )
}

/// 仓库编码重复（409 + `66001`）：走六类共用的 [`code_conflict`]。
fn warehouse_code_conflict(code: &str) -> ApiError {
    code_conflict("仓库", code)
}

/// 仓库写路径的 DB 错误收口：唯一索引冲突翻成**带编码**的 409，其余走 [`write_error`]。
fn warehouse_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => warehouse_code_conflict(code),
        _ => write_error(err, "新建仓库"),
    }
}

/// 编辑仓库（**不允许换上级**：换上级等于整棵子树重写路径，应由专门的移动接口做）。
#[nik::permission(
    path = "/business/erp/warehouse/{id}",
    method = "PUT",
    permission = "business:erp:warehouse:update"
)]
#[nik::put("/business/erp/warehouse/{id}")]
pub async fn warehouse_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWarehouseReq>,
) -> ResResult<WarehouseRes> {
    req.validate()?;
    let id = parse_uuid(&id, "id")?;
    let current = model::warehouse_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑仓库"))?
        .ok_or_else(|| not_found("仓库"))?;
    let name = req
        .warehouse_name
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| current.warehouse_name.clone());
    let warehouse_type = req.warehouse_type.unwrap_or_else(|| current.warehouse_type.clone());
    let manager = match req.manager {
        None => current.manager.clone(),
        Some(value) => value,
    };
    let allow_negative = req.allow_negative.map(flag_to_i16).unwrap_or(current.allow_negative);
    let address = match req.address {
        None => current.address.clone(),
        Some(value) => value,
    };
    let status = req.status.unwrap_or(current.status);
    let sort_order = req.sort_order.unwrap_or(current.sort_order);
    let remark = match req.remark {
        None => current.remark.clone(),
        Some(value) => value,
    };
    db().execute_with_params(
        "UPDATE biz_warehouse SET warehouse_name = ?, warehouse_type = ?, manager = ?, \
             allow_negative = ?, address = ?, status = ?, sort_order = ?, remark = ?, \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
             WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(name),
            Value::from(warehouse_type),
            Value::from(manager),
            Value::I16(allow_negative),
            Value::from(address),
            Value::I16(status),
            Value::I32(sort_order),
            Value::from(remark),
            operator_of(&claims),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "编辑仓库"))?;
    let row = model::warehouse_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读仓库"))?
        .ok_or_else(|| not_found("仓库"))?;
    Ok(Res::ok(WarehouseRes::from(&row)))
}

/// 删除仓库（软删）。有下级 / 有库位 → 409，不做级联（删父不删子会留下孤儿路径）。
#[nik::permission(
    path = "/business/erp/warehouse/{id}",
    method = "DELETE",
    permission = "business:erp:warehouse:delete"
)]
#[nik::delete("/business/erp/warehouse/{id}")]
pub async fn warehouse_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    let current = model::warehouse_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待删除仓库"))?
        .ok_or_else(|| not_found("仓库"))?;
    let children: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_warehouse WHERE parent_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计仓库下级"))?;
    if children > 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_WAREHOUSE_HAS_CHILDREN,
            format!("仓库还有 {children} 个下级，请先删除下级（不做级联删除）"),
        ));
    }
    let locations: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_location WHERE warehouse_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计仓库库位"))?;
    if locations > 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_WAREHOUSE_HAS_CHILDREN,
            format!("仓库还有 {locations} 个库位，请先删除库位"),
        ));
    }
    // 有物料把该仓库设为默认仓 → 409。
    // 不加这道闸的后果（E2E-08 实测）：删完仓库 200，物料的 `default_warehouse_id` 仍指向这行，
    // 入库单默认带出的仓库是一行已软删的记录 —— 页面上看是空白，逻辑上是个悬空指针，
    // 只能靠工程师改库恢复（违反「不许留死单」）。与计量单位「还有 N 个物料在用」同口径。
    let referenced: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material WHERE default_warehouse_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计引用该仓库的物料"))?;
    if referenced > 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("还有 {referenced} 个物料把该仓库设为默认仓，请先调整这些物料的默认仓，或改为停用该仓库"),
        ));
    }
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除仓库事务"))?;
    tx.execute_with_params(
        "UPDATE biz_warehouse SET deleted_at = now(), updated_at = now(), \
         update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![operator_of(&claims), Value::from(&id)],
    )
    .await
    .map_err(|error| utils::db_error(error, "软删仓库"))?;
    if let Some(parent_id) = current.parent_id {
        // 删掉最后一个下级后，上级要变回叶子，否则「叶子仓库才能挂库位」会误判
        tx.execute_with_params(
            "UPDATE biz_warehouse SET is_leaf = 1, updated_at = now() \
             WHERE id = ?::uuid AND deleted_at IS NULL \
               AND NOT EXISTS (SELECT 1 FROM biz_warehouse child \
                                WHERE child.parent_id = ?::uuid AND child.deleted_at IS NULL)",
            vec![Value::from(&parent_id), Value::from(&parent_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "复位上级仓库叶子标记"))?;
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交删除仓库事务"))?;
    Ok(Res::ok(()))
}

// ==================== ② 库位 biz_location ====================

/// 库位分页（关键词 / 仓库 / 类型 / 启用位）。
#[nik::permission(
    path = "/business/erp/location/page",
    method = "GET",
    permission = "business:erp:location:list"
)]
#[nik::get("/business/erp/location/page")]
pub async fn location_page(Query(filter): Query<LocationFilter>) -> ResResult<PageData<Vec<LocationRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::location_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询库位"))?;
    let list = rows.iter().map(LocationRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 库位下拉（按仓库）。`include_disabled=true` 时连停用库位一起返回（编辑回填用）。
#[nik::permission(
    path = "/business/erp/location/list",
    method = "GET",
    permission = "business:erp:location:list"
)]
#[nik::get("/business/erp/location/list")]
pub async fn location_list(Query(query): Query<LocationListQuery>) -> ResResult<Vec<LocationRes>> {
    let warehouse_id = parse_uuid(&query.warehouse_id, "warehouse_id")?;
    let rows = model::location_by_warehouse(warehouse_id, query.include_disabled.unwrap_or(false))
        .await
        .map_err(|error| utils::db_error(error, "按仓库查询库位"))?;
    Ok(Res::ok(rows.iter().map(LocationRes::from).collect()))
}

/// 库位详情。
#[nik::permission(
    path = "/business/erp/location/{id}",
    method = "GET",
    permission = "business:erp:location:detail"
)]
#[nik::get("/business/erp/location/{id}")]
pub async fn location_detail(Path(id): Path<String>) -> ResResult<LocationRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::location_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询库位详情"))?
        .ok_or_else(|| not_found("库位"))?;
    Ok(Res::ok(LocationRes::from(&row)))
}

/// 新建库位（只能挂在叶子仓库下；置默认时先清同仓其它默认位）。
#[nik::permission(
    path = "/business/erp/location",
    method = "POST",
    permission = "business:erp:location:create"
)]
#[nik::post("/business/erp/location")]
pub async fn location_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateLocationReq>,
) -> ResResult<LocationRes> {
    req.validate()?;
    let warehouse_id = parse_uuid(&req.warehouse_id, "warehouse_id")?;
    let warehouse = model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询所属仓库"))?
        .ok_or_else(|| reference_missing("所属仓库不存在或已删除"))?;
    if warehouse.is_leaf == 0 {
        return Err(ApiError::new(
            400,
            error_code::ERP_WAREHOUSE_NOT_LEAF,
            format!(
                "库位只能挂在叶子仓库下：仓库「{}」还有下级仓库",
                warehouse.warehouse_name
            ),
        ));
    }
    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建库位事务"))?;
    if req.is_default {
        tx.execute_with_params(
            "UPDATE biz_location SET is_default = 0, updated_at = now() \
             WHERE warehouse_id = ?::uuid AND is_default = 1 AND deleted_at IS NULL",
            vec![Value::from(&warehouse_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "清理同仓默认库位"))?;
    }
    tx.execute_with_params(
        "INSERT INTO biz_location \
         (id, location_code, location_name, warehouse_id, location_type, is_default, capacity, \
          status, sort_order, remark, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?::uuid, ?, ?, NULLIF(?::text,'')::numeric, ?, ?, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(req.location_code.trim().to_string()),
            Value::from(req.location_name.clone().map(|text| text.trim().to_string())),
            Value::from(&warehouse_id),
            Value::from(req.location_type.clone()),
            Value::I16(flag_to_i16(req.is_default)),
            decimal_text(req.capacity),
            Value::I16(req.status),
            Value::I32(req.sort_order),
            Value::from(req.remark.clone()),
            operator_of(&claims),
            operator_of(&claims),
        ],
    )
    .await
    .map_err(|error| write_error(error, "新建库位"))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建库位事务"))?;
    let row = model::location_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建库位"))?
        .ok_or_else(|| not_found("库位"))?;
    Ok(Res::ok(LocationRes::from(&row)))
}

/// 编辑库位（不允许换仓库：换仓库等于换身份，应重建）。
#[nik::permission(
    path = "/business/erp/location/{id}",
    method = "PUT",
    permission = "business:erp:location:update"
)]
#[nik::put("/business/erp/location/{id}")]
pub async fn location_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateLocationReq>,
) -> ResResult<LocationRes> {
    req.validate()?;
    let id = parse_uuid(&id, "id")?;
    let current = model::location_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑库位"))?
        .ok_or_else(|| not_found("库位"))?;
    let location_name = match req.location_name {
        None => current.location_name.clone(),
        Some(value) => value.map(|text| text.trim().to_string()),
    };
    let location_type = req.location_type.unwrap_or_else(|| current.location_type.clone());
    let is_default = req.is_default.map(flag_to_i16).unwrap_or(current.is_default);
    let capacity = match req.capacity {
        None => current.capacity,
        Some(value) => value,
    };
    let status = req.status.unwrap_or(current.status);
    let sort_order = req.sort_order.unwrap_or(current.sort_order);
    let remark = match req.remark {
        None => current.remark.clone(),
        Some(value) => value,
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启编辑库位事务"))?;
    if is_default == 1 && current.is_default == 0 {
        tx.execute_with_params(
            "UPDATE biz_location SET is_default = 0, updated_at = now() \
             WHERE warehouse_id = ?::uuid AND is_default = 1 AND deleted_at IS NULL AND id <> ?::uuid",
            vec![Value::from(&current.warehouse_id), Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "清理同仓默认库位"))?;
    }
    tx.execute_with_params(
        "UPDATE biz_location SET location_name = ?, location_type = ?, is_default = ?, \
         capacity = NULLIF(?::text,'')::numeric, status = ?, sort_order = ?, remark = ?, \
         updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(location_name),
            Value::from(location_type),
            Value::I16(is_default),
            decimal_text(capacity),
            Value::I16(status),
            Value::I32(sort_order),
            Value::from(remark),
            operator_of(&claims),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "编辑库位"))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交编辑库位事务"))?;
    let row = model::location_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读库位"))?
        .ok_or_else(|| not_found("库位"))?;
    Ok(Res::ok(LocationRes::from(&row)))
}

/// 删除库位（软删）。
#[nik::permission(
    path = "/business/erp/location/{id}",
    method = "DELETE",
    permission = "business:erp:location:delete"
)]
#[nik::delete("/business/erp/location/{id}")]
pub async fn location_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    let affected = db()
        .execute_with_params(
            "UPDATE biz_location SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![operator_of(&claims), Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "软删库位"))?;
    if affected == 0 {
        return Err(not_found("库位"));
    }
    Ok(Res::ok(()))
}

// ==================== ③ 物料 biz_material ====================

/// 物料分页（关键词 / 类型 / 单位 / 默认仓库 / 启用位）。
#[nik::permission(
    path = "/business/erp/material/page",
    method = "GET",
    permission = "business:erp:material:list"
)]
#[nik::get("/business/erp/material/page")]
pub async fn material_page(Query(filter): Query<MaterialFilter>) -> ResResult<PageData<Vec<MaterialRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::material_page_named(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询物料"))?;
    let list = rows.iter().map(MaterialRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 物料搜索（`keyword` 匹配编码 / 名称，最多 [`MATERIAL_LIST_LIMIT`] 条，按编码升序）。
#[nik::permission(
    path = "/business/erp/material/list",
    method = "GET",
    permission = "business:erp:material:list"
)]
#[nik::get("/business/erp/material/list")]
pub async fn material_list(Query(query): Query<MaterialListQuery>) -> ResResult<Vec<MaterialRes>> {
    let filter = MaterialFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        material_type: None,
        unit_id: None,
        default_warehouse_id: None,
        status: query.status,
        serial_required: None,
        page: 1,
        page_size: MATERIAL_LIST_LIMIT,
    };
    let (rows, _) = model::material_page_named(&filter)
        .await
        .map_err(|error| utils::db_error(error, "搜索物料"))?;
    Ok(Res::ok(rows.iter().map(MaterialRes::from).collect()))
}

/// 物料详情。
#[nik::permission(
    path = "/business/erp/material/{id}",
    method = "GET",
    permission = "business:erp:material:detail"
)]
#[nik::get("/business/erp/material/{id}")]
pub async fn material_detail(Path(id): Path<String>) -> ResResult<MaterialRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::material_find_by_id_named(id)
        .await
        .map_err(|error| utils::db_error(error, "查询物料详情"))?
        .ok_or_else(|| not_found("物料"))?;
    Ok(Res::ok(MaterialRes::from(&row)))
}

/// 新建物料（计量单位必须存在；默认仓库 / 存货科目存在性同样校验）。
///
/// # 编码（CODE-03）
///
/// `material_code` 留空（`null` / `""` / 纯空白）⇒ 自动取号 `MAT-0001` 形态；
/// 有值 ⇒ 原样保留。口径详见 [`create_material`]。
#[nik::permission(
    path = "/business/erp/material",
    method = "POST",
    permission = "business:erp:material:create"
)]
#[nik::post("/business/erp/material")]
pub async fn material_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateMaterialReq>,
) -> ResResult<MaterialRes> {
    let res = create_material(utils::current_user_uuid(&claims), req).await?;
    Ok(Res::ok(res))
}

/// 物料编码预览（卡 CODE-06 · 交互方式 B：前端「生成编码」按钮先看号，
/// 保存时把这个号当**手工编码**提交）。
///
/// 与供应商 / 客户（CODE-05）、仓库同卡同一口径：**只读、不占号**
/// （`seq::peek_master_code` 不动 `biz_seq.current_no`）；并发撞号由保存时的
/// `409 + 66001` 兜底，前端提示重新生成即可。
#[nik::permission(
    path = "/business/erp/material/next-code",
    method = "GET",
    permission = "business:erp:material:create"
)]
#[nik::get("/business/erp/material/next-code")]
pub async fn material_next_code(Extension(AuthClaims(_claims)): Extension<AuthClaims>) -> ResResult<serde_json::Value> {
    let code = preview_material_code().await?;
    Ok(Res::ok(json!({ "code": code })))
}

/// 物料编码预览的**业务主体**（单测直接调本函数，不走认证）。
///
/// 号段口径：`biz_seq` 的 MAT 规则 —— 前缀 `MAT`、分隔符 `-`、**纯流水 4 位**
/// （`MAT-0001`）、不计日期、永不重置；连号被占（如历史遗留 `MAT-0001..0003`）则顺延到下一个空号。
///
/// 错误口径与真实取号一致：规则缺失 / 连号全被占 ⇒ `500 + 10004`，细节只进日志。
pub async fn preview_material_code() -> std::result::Result<String, ApiError> {
    seq::peek_master_code(db(), "MAT")
        .await
        .map_err(|error| seq_error(error, "预览物料编码"))
}

/// 新建物料的**业务主体**（CODE-03：编码留空则由后端取号 `MAT-0001` 形态）。
///
/// axum handler [`material_create`] 只做参数解包，单测直接调本函数
/// （同一条代码路径，只跳过认证）。
///
/// # 编码口径（用户已拍板，不许改）
///
/// | 入参 `material_code` | 行为 |
/// |---|---|
/// | `null` / `""` / 纯空白 | 自动取号：`MAT-0001` 形态（`biz_seq` 的 MAT 号段，纯流水 4 位、不计日期、永不重置）；撞号自动顺延 |
/// | 有值 | **原样保留**（物料常按分类自编号，`YB-001-02` 也必须能用），只校验长度 ≤ 40 + 字符集 |
///
/// # 取号与落库同一事务
///
/// 取号是 `UPDATE biz_seq … RETURNING`（行锁），插入失败则整体回滚、号一起退回，
/// 不留空号；并发建两个物料也不会拿到同一个号。
///
/// # 错误口径
///
/// - 手工编码已存在（预检命中 / 并发撞唯一索引）→ `409 + 66001`「物料编码 `XXX` 已存在」；
/// - 编码格式不合法 → `400 + 10001`；
/// - 取号规则缺失 / 连号全被占 → `500 + 10004`。
pub async fn create_material(
    operator: Option<Uuid>,
    req: CreateMaterialReq,
) -> std::result::Result<MaterialRes, ApiError> {
    req.validate()?;
    // ① 手工编码：有值就保留（先去重，给出「编码 XXX 已存在」的人话错误）
    // CODE-08：开关关闭时填了编码直接 400（读在 begin 之前，被拒不占号）
    code_policy::guard_manual(code_policy::allow_manual_code().await, req.material_code.as_deref())?;
    let manual = seq::resolve_manual_code(req.material_code.as_deref(), seq::MAX_MANUAL_CODE_LEN)
        .map_err(|reason| param_invalid(format!("material_code 不合法：{reason}")))?;
    if let Some(code) = manual.as_deref() {
        let taken = seq::master_code_taken(db(), "MAT", code)
            .await
            .map_err(|error| utils::db_error(error, "校验物料编码唯一性"))?;
        if taken {
            return Err(material_code_conflict(code));
        }
    }
    let unit_id = parse_uuid(&req.unit_id, "unit_id")?;
    let unit = model::unit_find_by_id(unit_id)
        .await
        .map_err(|error| utils::db_error(error, "查询计量单位"))?
        .ok_or_else(|| reference_missing("计量单位不存在或已删除"))?;
    // ★ DC-09 补口（缺口 2）：物料只能挂**基本单位**。
    //   挂在折算单位（箱/打）上的物料，它的任何单据都会被建单期的单位止血闸 400 拒 ——
    //   用户只会看到「这个物料的单建不了」，根因却在主数据。所以在这里就堵住。
    unit_guard::ensure_material_unit_is_base(unit.is_base, &unit.unit_name)?;
    let default_warehouse_id = match req
        .default_warehouse_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let warehouse_id = parse_uuid(raw, "default_warehouse_id")?;
            model::warehouse_find_by_id(warehouse_id)
                .await
                .map_err(|error| utils::db_error(error, "查询默认仓库"))?
                .ok_or_else(|| reference_missing("默认仓库不存在或已删除"))?;
            Some(warehouse_id)
        }
        None => None,
    };
    let inventory_account_id = match req
        .inventory_account_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let account_id = parse_uuid(raw, "inventory_account_id")?;
            model::account_find_by_id(account_id)
                .await
                .map_err(|error| utils::db_error(error, "查询存货科目"))?
                .ok_or_else(|| reference_missing("存货科目不存在或已删除"))?;
            Some(account_id)
        }
        None => None,
    };
    // FIX-6：物料税率统一按**百分数**落库（V0327）。老前端传小数（0.13）按过渡期规则 ×100
    //         并打 WARN；实际采用的标度写 `tax_rate_mode` 供审计（过渡期后删列）。
    let material_tax = match req.tax_rate {
        Some(rate) => Some(tax_rate_mod::normalize_with_mode(
            rate,
            tax_rate_mod::TaxRateMode::Legacy,
            "物料 tax_rate",
        )?),
        None => None,
    };
    let id = Uuid::now_v7();
    // ② 编码 + INSERT 同一事务：没填就取号，插入失败则号一起回滚
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建物料事务"))?;
    let material_code = match manual {
        Some(code) => code,
        None => seq::next_master_code_free(&tx, "MAT")
            .await
            .map_err(|error| seq_error(error, "新建物料"))?,
    };
    tx.execute_with_params(
        "INSERT INTO biz_material \
             (id, material_code, material_name, spec, unit_id, material_type, \
              default_warehouse_id, safety_stock, costing_method, inventory_account_id, tax_rate, \
              tax_rate_mode, standard_price, status, serial_required, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, ?::uuid, ?, NULLIF(?::text,'')::uuid, \
                     COALESCE(NULLIF(?::text,''),'0')::numeric, ?, NULLIF(?::text,'')::uuid, \
                     COALESCE(NULLIF(?::text,''),'0')::numeric, ?, NULLIF(?::text,'')::numeric, ?, ?, ?, now(), now(), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(material_code.clone()),
            Value::from(req.material_name.trim().to_string()),
            Value::from(req.spec.clone()),
            Value::from(&unit_id),
            Value::from(req.material_type.clone()),
            uuid_text(default_warehouse_id),
            decimal_text(req.safety_stock),
            Value::from(req.costing_method.clone()),
            uuid_text(inventory_account_id),
            Value::Text(
                material_tax
                    .map(|value| value.percent.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            ),
            Value::Text(
                material_tax
                    .map(|value| value.mode_text().to_string())
                    .unwrap_or_else(|| tax_rate_mod::MODE_PERCENT.to_string()),
            ),
            decimal_text(req.standard_price),
            Value::I16(req.status),
            Value::from(req.serial_required.unwrap_or(false)),
            Value::from(req.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
    .map_err(|error| material_write_error(error, &material_code))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建物料事务"))?;
    let row = model::material_find_by_id_named(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建物料"))?
        .ok_or_else(|| not_found("物料"))?;
    Ok(MaterialRes::from(&row))
}

/// 物料编码重复（409 + `66001`）：走六类共用的 [`code_conflict`]。
fn material_code_conflict(code: &str) -> ApiError {
    code_conflict("物料", code)
}

/// 物料写路径的 DB 错误收口：唯一索引冲突翻成**带编码**的 409，其余走 [`write_error`]。
fn material_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => material_code_conflict(code),
        _ => write_error(err, "新建物料"),
    }
}

/// 编辑物料。
#[nik::permission(
    path = "/business/erp/material/{id}",
    method = "PUT",
    permission = "business:erp:material:update"
)]
#[nik::put("/business/erp/material/{id}")]
pub async fn material_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateMaterialReq>,
) -> ResResult<MaterialRes> {
    req.validate()?;
    let id = parse_uuid(&id, "id")?;
    let current = model::material_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑物料"))?
        .ok_or_else(|| not_found("物料"))?;
    let material_name = req
        .material_name
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| current.material_name.clone());
    let spec = match req.spec {
        None => current.spec.clone(),
        Some(value) => value,
    };
    let unit_id = match req.unit_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => parse_uuid(raw, "unit_id")?,
        None => current.unit_id,
    };
    // ★ DC-09 补口（缺口 2）：按**解析后的有效单位**判（这次没改单位也要判）。
    //   存量已挂在折算单位上的物料因此不能再保存 —— 生产实测 0 行，命中先出清单给业务，
    //   不自动改数据；本版本单据不支持多单位换算，折算单位只能用于显示。
    let unit = model::unit_find_by_id(unit_id)
        .await
        .map_err(|error| utils::db_error(error, "查询计量单位"))?
        .ok_or_else(|| reference_missing("计量单位不存在或已删除"))?;
    unit_guard::ensure_material_unit_is_base(unit.is_base, &unit.unit_name)?;
    let material_type = req.material_type.unwrap_or_else(|| current.material_type.clone());
    let default_warehouse_id = match req.default_warehouse_id {
        None => current.default_warehouse_id,
        Some(None) => None,
        Some(Some(raw)) => {
            let warehouse_id = parse_uuid(&raw, "default_warehouse_id")?;
            model::warehouse_find_by_id(warehouse_id)
                .await
                .map_err(|error| utils::db_error(error, "查询默认仓库"))?
                .ok_or_else(|| reference_missing("默认仓库不存在或已删除"))?;
            Some(warehouse_id)
        }
    };
    let safety_stock = req.safety_stock.unwrap_or(current.safety_stock);
    let costing_method = req.costing_method.unwrap_or_else(|| current.costing_method.clone());
    let inventory_account_id = match req.inventory_account_id {
        None => current.inventory_account_id,
        Some(None) => None,
        Some(Some(raw)) => {
            let account_id = parse_uuid(&raw, "inventory_account_id")?;
            model::account_find_by_id(account_id)
                .await
                .map_err(|error| utils::db_error(error, "查询存货科目"))?
                .ok_or_else(|| reference_missing("存货科目不存在或已删除"))?;
            Some(account_id)
        }
    };
    // FIX-6：只有**请求显式给了**税率才归一（老前端传 0.13 → 13% + WARN）。
    //         缺席就整列不写：库里已是百分数口径（V0327 迁过），再归一一次会把 0.5% 放大成 50%。
    let (tax_clause, tax_params) = match req.tax_rate {
        Some(rate) => {
            let normalized =
                tax_rate_mod::normalize_with_mode(rate, tax_rate_mod::TaxRateMode::Legacy, "物料 tax_rate")?;
            (
                // 参数是 Text（`normalized.percent.to_string()`）：**必须先 `::text` 再 `::numeric`**。
                // 直接 `?::numeric` 会让库把占位符推断成 numeric，canal 序列化 Text 失败 →
                // 500「error serializing parameter」（E2E-04 实测：编辑物料带税率整页 500）。
                " tax_rate = NULLIF(?::text,'')::numeric, tax_rate_mode = ?, ".to_string(),
                vec![
                    Value::Text(normalized.percent.to_string()),
                    Value::Text(normalized.mode_text().to_string()),
                ],
            )
        }
        None => (String::new(), Vec::new()),
    };
    // 标准售价三态（与 spec / remark 同口径）：缺席 = 不改、`null` = 清空、有值 = 覆盖。
    // 数值校验（> 0）已在 `req.validate()` 拦过，这里只负责拼 SQL 片段。
    let (standard_clause, standard_param) = match req.standard_price {
        Some(Some(price)) => (
            // 参数是 Text，用 `NULLIF(?::text,'')::numeric` 两步转：直接 `?::numeric`
            // 会让库把占位符推断成 numeric，canal 序列化 Text 失败 → 500
            // 「error serializing parameter」（与同函数里 tax_rate 的坑同源，E2E-04 实测过）。
            " standard_price = NULLIF(?::text,'')::numeric, ".to_string(),
            vec![Value::Text(price.to_string())],
        ),
        Some(None) => (" standard_price = NULL, ".to_string(), Vec::new()),
        None => (String::new(), Vec::new()),
    };
    let status = req.status.unwrap_or(current.status);
    // 一物一码开关：缺席 = 不改（不是「关掉」），与其余可选字段同口径。
    let (serial_clause, serial_param) = match req.serial_required {
        Some(flag) => (" serial_required = ?, ".to_string(), Some(Value::from(flag))),
        None => (String::new(), None),
    };
    let remark = match req.remark {
        None => current.remark.clone(),
        Some(value) => value,
    };
    let mut params = vec![
        Value::from(material_name),
        Value::from(spec),
        Value::from(&unit_id),
        Value::from(material_type),
        uuid_text(default_warehouse_id),
        Value::Text(safety_stock.to_string()),
        Value::from(costing_method),
        uuid_text(inventory_account_id),
    ];
    params.extend(standard_param);
    params.extend(tax_params);
    params.push(Value::I16(status));
    params.push(Value::from(remark));
    // 一物一码片段插在 status/remark 之后、updated_at 之前（占位符顺序必须与 vec 一致）
    if let Some(value) = serial_param {
        params.push(value);
    }
    params.push(operator_of(&claims));
    params.push(Value::from(&id));
    db().execute_with_params(
        &format!(
            "UPDATE biz_material SET material_name = ?, spec = ?, unit_id = ?::uuid, \
             material_type = ?, default_warehouse_id = NULLIF(?::text,'')::uuid, \
             safety_stock = NULLIF(?::text,'')::numeric, costing_method = ?, \
             inventory_account_id = NULLIF(?::text,'')::uuid,{standard_clause}{tax_clause} \
             status = ?, remark = ?,{serial_clause} \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
             WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        params,
    )
    .await
    .map_err(|error| write_error(error, "编辑物料"))?;
    let row = model::material_find_by_id_named(id)
        .await
        .map_err(|error| utils::db_error(error, "回读物料"))?
        .ok_or_else(|| not_found("物料"))?;
    Ok(Res::ok(MaterialRes::from(&row)))
}

/// 删除物料（软删）。
#[nik::permission(
    path = "/business/erp/material/{id}",
    method = "DELETE",
    permission = "business:erp:material:delete"
)]
#[nik::delete("/business/erp/material/{id}")]
pub async fn material_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    let affected = db()
        .execute_with_params(
            "UPDATE biz_material SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![operator_of(&claims), Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "软删物料"))?;
    if affected == 0 {
        return Err(not_found("物料"));
    }
    Ok(Res::ok(()))
}

// ==================== ④ 计量单位 biz_unit ====================

/// 计量单位分页。
#[nik::permission(
    path = "/business/erp/unit/page",
    method = "GET",
    permission = "business:erp:unit:list"
)]
#[nik::get("/business/erp/unit/page")]
pub async fn unit_page(Query(filter): Query<UnitFilter>) -> ResResult<PageData<Vec<UnitRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::unit_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询计量单位"))?;
    let list = rows.iter().map(UnitRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 计量单位详情（3 码制下由 `:list` 覆盖）。
#[nik::permission(
    path = "/business/erp/unit/{id}",
    method = "GET",
    permission = "business:erp:unit:list"
)]
#[nik::get("/business/erp/unit/{id}")]
pub async fn unit_detail(Path(id): Path<String>) -> ResResult<UnitRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::unit_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询计量单位详情"))?
        .ok_or_else(|| not_found("计量单位"))?;
    Ok(Res::ok(UnitRes::from(&row)))
}

/// 计量单位「取号」端点 —— **显式返回 400**（E2E08-FIX-2 ④，方案 A）。
///
/// # 为什么要有这条路由（而不是什么都不做）
///
/// 六类主数据（供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型）都有 `next-code`，
/// 只有计量单位**不自动取号**（`PCS` / `KG` / `M` 是行业标准码，替用户编 `U-01` 没人看得懂，
/// 见 [`seq`] 文件头）。
/// 没有这条路由时，`GET /business/erp/unit/next-code` 会被 `/unit/{id}` 吃掉，
/// 返回 `400 + 66006「id 不是 UUID」`——对接方按「路由丢失」排查，排查半天才发现是「本就不提供」。
///
/// 所以这里注册一条**显式**路由，把结论写在响应里：`400 + 10001 + 人话`。
/// 选静态路由不会和 `/unit/{id}` 冲突：axum（matchit）静态段优先于参数段。
#[nik::permission(
    path = "/business/erp/unit/next-code",
    method = "GET",
    permission = "business:erp:unit:create"
)]
#[nik::get("/business/erp/unit/next-code")]
pub async fn unit_next_code(Extension(AuthClaims(_claims)): Extension<AuthClaims>) -> ResResult<serde_json::Value> {
    Err(unit_code_not_auto())
}

/// 「计量单位不提供自动取号」的统一错误（`400 + 10001`）。
///
/// 与 [`crate::business::erp::dto::UNIT_CODE_REQUIRED`] 是**一对**：一个说「建单必须自己填编码」，
/// 一个说「别指望这里能取号」，两条文案合起来才是完整的单位编码口径。
pub fn unit_code_not_auto() -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        "计量单位不提供自动取号：新建时请手工填写 unit_code（`PCS` / `KG` / `M` 这类行业标准码，\
         系统不会替你编）。有 next-code 端点的是供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型 六类"
            .to_string(),
    )
}

/// 计量单位编码是否已被占用（`未删除内唯一`，与 `uk_biz_unit_code` 同口径）。
async fn unit_code_taken(code: &str) -> std::result::Result<bool, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT 1 AS hit FROM biz_unit WHERE unit_code = ? AND deleted_at IS NULL",
            vec![Value::from(code)],
        )
        .await
        .map_err(|error| utils::db_error(error, "校验计量单位编码唯一性"))?;
    Ok(!rows.is_empty())
}

/// 计量单位编码重复（409 + `66001`）：走六类共用的 [`code_conflict`]。
fn unit_code_conflict(code: &str) -> ApiError {
    code_conflict("计量单位", code)
}

/// 计量单位写路径的 DB 错误收口：唯一索引冲突翻成**带编码**的 409，其余走 [`write_error`]。
///
/// 预检（[`unit_code_taken`]）与 INSERT 之间的并发窗口由这一层兜底：并发建同一个编码时
/// 后到的那个拿到 DB 的 `23505`，仍然翻译成同一句「计量单位编码 `X` 已存在」，
/// 而不是裸约束名。
fn unit_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => unit_code_conflict(code),
        _ => write_error(err, "新建计量单位"),
    }
}

/// 新建计量单位。
#[nik::permission(
    path = "/business/erp/unit",
    method = "POST",
    permission = "business:erp:unit:create"
)]
#[nik::post("/business/erp/unit")]
pub async fn unit_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateUnitReq>,
) -> ResResult<UnitRes> {
    req.validate()?;
    // ② 编码必填但不自动取号：缺失 / 纯空白 ⇒ 400 + 10001 + 人话（不是框架反序列化的 422）。
    //    与其余五类「留空 = 自动取号」是**相反**的语义，所以文案必须说清「单位不自动取号」。
    let unit_code = req.unit_code_required()?.to_string();
    // ③ 建单前预检（与供应商 / 物料 / 仓库 / 分类 / 费用类型同一口径）：
    //    撞码直接 409 + 点名编码，不等 DB 唯一索引把裸约束名抛给用户。
    if unit_code_taken(&unit_code).await? {
        return Err(unit_code_conflict(&unit_code));
    }
    let base_unit_id = match req.base_unit_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let base_id = parse_uuid(raw, "base_unit_id")?;
            model::unit_find_by_id(base_id)
                .await
                .map_err(|error| utils::db_error(error, "查询折算基准单位"))?
                .ok_or_else(|| reference_missing("折算基准单位不存在或已删除"))?;
            Some(base_id)
        }
        None => None,
    };
    let rate = req.rate.unwrap_or(Decimal::ONE);
    let precision = req.precision.unwrap_or(0);
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_unit \
             (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order, remark, \
              is_system, precision, created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::numeric, ?, ?, ?, \
                     false, ?, now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(unit_code.clone()),
            Value::from(req.unit_name.trim().to_string()),
            Value::I16(flag_to_i16(req.is_base)),
            uuid_text(base_unit_id),
            Value::Text(rate.to_string()),
            Value::I16(req.status),
            Value::I32(req.sort_order),
            Value::from(req.remark.clone()),
            // precision（0–6）：客户自建单位缺省 0 位小数。
            // is_system 恒为 false，写成 SQL 里的常量（接口不给这个开关，预置只由迁移种下）。
            Value::I16(precision),
            operator_of(&claims),
            operator_of(&claims),
        ],
    )
    .await
    .map_err(|error| unit_write_error(error, &unit_code))?;
    let row = model::unit_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建计量单位"))?
        .ok_or_else(|| not_found("计量单位"))?;
    Ok(Res::ok(UnitRes::from(&row)))
}

/// 编辑计量单位（清空 `base_unit_id` 即变成基础单位，`rate` 自动归 1）。
#[nik::permission(
    path = "/business/erp/unit/{id}",
    method = "PUT",
    permission = "business:erp:unit:update"
)]
#[nik::put("/business/erp/unit/{id}")]
pub async fn unit_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateUnitReq>,
) -> ResResult<UnitRes> {
    req.validate()?;
    let id = parse_uuid(&id, "id")?;
    let current = model::unit_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑计量单位"))?
        .ok_or_else(|| not_found("计量单位"))?;
    let unit_name = req
        .unit_name
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| current.unit_name.clone());
    let (is_base, base_unit_id) = match req.base_unit_id {
        None => (current.is_base, current.base_unit_id),
        Some(None) => (1, None),
        Some(Some(raw)) => {
            let base_id = parse_uuid(&raw, "base_unit_id")?;
            if base_id == id {
                return Err(reference_missing("折算基准单位不能是自己"));
            }
            model::unit_find_by_id(base_id)
                .await
                .map_err(|error| utils::db_error(error, "查询折算基准单位"))?
                .ok_or_else(|| reference_missing("折算基准单位不存在或已删除"))?;
            (0, Some(base_id))
        }
    };
    let rate = match req.rate {
        Some(rate) => rate,
        None => {
            if is_base == 1 {
                Decimal::ONE
            } else {
                current.rate
            }
        }
    };
    if is_base == 1 && rate != Decimal::ONE {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "基础单位的 rate 必须为 1（折算率是恒等的）",
        ));
    }
    if is_base == 0 && rate <= Decimal::ZERO {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "折算单位的 rate 必须大于 0",
        ));
    }
    let status = req.status.unwrap_or(current.status);
    let sort_order = req.sort_order.unwrap_or(current.sort_order);
    let remark = match req.remark {
        None => current.remark.clone(),
        Some(value) => value,
    };
    let precision = req.precision.unwrap_or(current.precision);
    db().execute_with_params(
        "UPDATE biz_unit SET unit_name = ?, is_base = ?, base_unit_id = NULLIF(?::text,'')::uuid, \
             rate = NULLIF(?::text,'')::numeric, status = ?, sort_order = ?, remark = ?, precision = ?, \
             updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(unit_name),
            Value::I16(is_base),
            uuid_text(base_unit_id),
            Value::Text(rate.to_string()),
            Value::I16(status),
            Value::I32(sort_order),
            Value::from(remark),
            Value::I16(precision),
            operator_of(&claims),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "编辑计量单位"))?;
    let row = model::unit_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读计量单位"))?
        .ok_or_else(|| not_found("计量单位"))?;
    Ok(Res::ok(UnitRes::from(&row)))
}

/// 删除计量单位（软删）。被物料引用 → 409（否则物料的 `unit_id` 会指向已删单位）。
///
/// 系统预置单位（`is_system = true`，ERP-S1 V0187 种下的标准值）→ 409：
/// 预置值可以停用（`status = 0`），不能删除 —— 否则客户不小心删掉「千克」，
/// 以后想用只能从头再建，且历史单据的单位名称会失去统一口径。
#[nik::permission(
    path = "/business/erp/unit/{id}",
    method = "DELETE",
    permission = "business:erp:unit:update"
)]
#[nik::delete("/business/erp/unit/{id}")]
pub async fn unit_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    let target = model::unit_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询计量单位"))?
        .ok_or_else(|| not_found("计量单位"))?;
    if target.is_system {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "「{}」是系统预置单位，不能删除；不用了就把它停用（status = 0）",
                target.unit_name
            ),
        ));
    }
    let used: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material WHERE unit_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计引用该单位的物料"))?;
    if used > 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("还有 {used} 个物料在用该单位，不能删除"),
        ));
    }
    let affected = db()
        .execute_with_params(
            "UPDATE biz_unit SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![operator_of(&claims), Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "软删计量单位"))?;
    if affected == 0 {
        return Err(not_found("计量单位"));
    }
    Ok(Res::ok(()))
}

// ==================== ⑤ 供应商 biz_supplier ====================

/// 供应商分页。
#[nik::permission(
    path = "/business/erp/supplier/page",
    method = "GET",
    permission = "business:erp:supplier:list"
)]
#[nik::get("/business/erp/supplier/page")]
pub async fn supplier_page(Query(filter): Query<SupplierFilter>) -> ResResult<PageData<Vec<SupplierRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::supplier_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询供应商"))?;
    let list = rows.iter().map(SupplierRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 供应商编码预览（卡 CODE-05 · 交互方式 B：前端「生成编码」按钮先看号，
/// 保存时把这个号当**手工编码**提交）。
///
/// # 为什么不占号（用户拍板的方案 B）
///
/// `peek_master_code` 只读：`biz_seq.current_no` 不动，所以点了按钮又取消不会浪费号。
/// 代价是并发下两个人可能看到同一个号 —— 真到保存那一刻号被别人抢走，就走
/// 手工编码撞唯一索引的老路（`409 + 66001`，文案点名编码），前端提示重新生成即可。
///
/// 换句话说：**预览 = 看格式，取号仍发生在保存时**（不点按钮直接保存则由后端取号）。
#[nik::permission(
    path = "/business/erp/supplier/next-code",
    method = "GET",
    permission = "business:erp:supplier:create"
)]
#[nik::get("/business/erp/supplier/next-code")]
pub async fn supplier_next_code(Extension(AuthClaims(_claims)): Extension<AuthClaims>) -> ResResult<serde_json::Value> {
    let code = preview_supplier_code().await?;
    Ok(Res::ok(json!({ "code": code })))
}

/// 供应商编码预览的**业务主体**（单测直接调本函数，不走认证）。
///
/// 号段口径：`biz_seq` 的 SUP 规则 —— 前缀 `SUP`、分隔符 `-`、纯流水 4 位、不计日期、
/// 永不重置；连号被占（如历史数据留着 `SUP-0001`）则顺延到下一个空号。
///
/// 错误口径与取号一致：规则缺失 / 连号全被占 ⇒ `500 + 10004`，细节只进日志。
pub async fn preview_supplier_code() -> std::result::Result<String, ApiError> {
    seq::peek_master_code(db(), "SUP")
        .await
        .map_err(|error| seq_error(error, "预览供应商编码"))
}

/// 供应商详情（3 码制下由 `:list` 覆盖）。
#[nik::permission(
    path = "/business/erp/supplier/{id}",
    method = "GET",
    permission = "business:erp:supplier:list"
)]
#[nik::get("/business/erp/supplier/{id}")]
pub async fn supplier_detail(Path(id): Path<String>) -> ResResult<SupplierRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::supplier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商详情"))?
        .ok_or_else(|| not_found("供应商"))?;
    Ok(Res::ok(SupplierRes::from(&row)))
}

/// 新建供应商（`409 + 66001` 表示编码重复）。
#[nik::permission(
    path = "/business/erp/supplier",
    method = "POST",
    permission = "business:erp:supplier:create"
)]
#[nik::post("/business/erp/supplier")]
pub async fn supplier_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateSupplierReq>,
) -> ResResult<SupplierRes> {
    let res = create_supplier(utils::current_user_uuid(&claims), req).await?;
    Ok(Res::ok(res))
}

/// 新建供应商的**业务主体**（CODE-02：编码留空则由后端取号）。
///
/// axum handler [`supplier_create`] 只做参数解包，单测直接调本函数（同一条代码路径，只跳过认证）。
///
/// # 编码口径（用户已拍板，不许改）
///
/// | 入参 `supplier_code` | 行为 |
/// |---|---|
/// | `null` / `""` / 纯空白 | 自动取号：`SUP-0001` 形态（`biz_seq` 的 SUP 号段，纯流水 4 位、不计日期、永不重置）；撞号自动顺延 |
/// | 有值 | **原样保留**（企业自有编码体系要能对接，`GYS-001` 也行），只校验长度 ≤ 40 + 字符集 |
///
/// 手工编码**不强制** `SUP-xxx` 形态 —— 只挡真正没法用的输入（空白 / 控制字符 / 中文标点）。
///
/// # 取号与落库同一事务
///
/// 取号是 `UPDATE biz_seq … RETURNING`（行锁），插入失败则整体回滚、号一起退回，
/// 不留空号；并发建两个也不会拿到同一个号。
///
/// # 错误口径
///
/// - 手工编码已存在（预检命中 / 并发撞唯一索引）→ `409 + 66001`「供应商编码 `XXX` 已存在」；
/// - 编码格式不合法 → `400 + 10001`；
/// - 取号规则缺失 / 连号全被占 → `500 + 10004`（细节只进日志，提示找管理员）。
pub async fn create_supplier(
    operator: Option<Uuid>,
    req: CreateSupplierReq,
) -> std::result::Result<SupplierRes, ApiError> {
    // CODE-08：编码策略读一次（进程内快照，命中是纯内存读），交给实现体；
    // 实现体不自己读配置，用例才能在不改库、不动全局快照的前提下分别钉死两条分支。
    let allow_manual = code_policy::allow_manual_code().await;
    create_supplier_with_policy(operator, req, allow_manual).await
}

/// 建供应商的实现体：**编码策略由调用方传入**（`true` = 允许手工编码）。
///
/// # 参数
///
/// - `operator`：操作人。
/// - `req`：建单请求（编码可留空 ⇒ 自动取号）。
/// - `allow_manual`：是否允许手工指定编码（生产路径由 [`code_policy::allow_manual_code`] 给）。
///
/// 为什么单独拆出来：`allow_manual` 是**进程级**系统参数的投影，用例如果靠改
/// `sys_config` + 重载快照来验证两条分支，就会和同进程里其它用例抢同一个全局状态
/// （实测：并发跑时别的用例建单突然收到 400 + 66054）。把策略作为入参传进来，
/// 用例就能在零副作用的前提下覆盖「允许 / 禁止」两条路。
pub(crate) async fn create_supplier_with_policy(
    operator: Option<Uuid>,
    req: CreateSupplierReq,
    allow_manual: bool,
) -> std::result::Result<SupplierRes, ApiError> {
    req.validate()?;
    // ① 手工编码：有值就保留（先去重，给出「编码 XXX 已存在」的人话错误）
    // CODE-08：开关关闭时填了编码直接 400（闸在 begin 之前 —— 被拒不占号、不留空号）
    code_policy::guard_manual(allow_manual, req.supplier_code.as_deref())?;
    let manual = seq::resolve_manual_code(req.supplier_code.as_deref(), seq::MAX_MANUAL_CODE_LEN)
        .map_err(|reason| param_invalid(format!("supplier_code 不合法：{reason}")))?;
    if let Some(code) = manual.as_deref() {
        let taken = seq::master_code_taken(db(), "SUP", code)
            .await
            .map_err(|error| utils::db_error(error, "校验供应商编码唯一性"))?;
        if taken {
            return Err(supplier_code_conflict(code));
        }
    }
    let payable_account_id = match req
        .payable_account_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let account_id = parse_uuid(raw, "payable_account_id")?;
            model::account_find_by_id(account_id)
                .await
                .map_err(|error| utils::db_error(error, "查询应付科目"))?
                .ok_or_else(|| reference_missing("应付科目不存在或已删除"))?;
            Some(account_id)
        }
        None => None,
    };
    let id = Uuid::now_v7();

    // ② 编码 + INSERT 同一事务：没填就取号，插入失败则号一起回滚
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建供应商事务"))?;
    let supplier_code = match manual {
        Some(code) => code,
        None => seq::next_master_code_free(&tx, "SUP")
            .await
            .map_err(|error| seq_error(error, "新建供应商"))?,
    };
    tx.execute_with_params(
        "INSERT INTO biz_supplier \
         (id, supplier_code, supplier_name, short_name, contact_person, contact_phone, email, \
          tax_no, address, credit_days, credit_limit, payable_account_id, invoice_title, \
          invoice_bank_name, invoice_bank_account, status, remark, created_at, updated_at, \
          create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::uuid, \
                 ?, ?, ?, ?, ?, now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(supplier_code.clone()),
            Value::from(req.supplier_name.trim().to_string()),
            Value::from(req.short_name.clone()),
            Value::from(req.contact_person.clone()),
            Value::from(req.contact_phone.clone()),
            Value::from(req.email.clone()),
            Value::from(req.tax_no.clone()),
            Value::from(req.address.clone()),
            Value::I32(req.credit_days),
            Value::Text(req.credit_limit.unwrap_or(Decimal::ZERO).to_string()),
            uuid_text(payable_account_id),
            Value::from(req.invoice_title.clone()),
            Value::from(req.invoice_bank_name.clone()),
            Value::from(req.invoice_bank_account.clone()),
            Value::I16(req.status),
            Value::from(req.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
    .map_err(|error| supplier_write_error(error, &supplier_code))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建供应商事务"))?;
    let row = model::supplier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建供应商"))?
        .ok_or_else(|| not_found("供应商"))?;
    Ok(SupplierRes::from(&row))
}

/// 供应商编码重复（409 + `66001`）：走六类共用的 [`code_conflict`]。
fn supplier_code_conflict(code: &str) -> ApiError {
    code_conflict("供应商", code)
}

/// 供应商写路径的 DB 错误收口：唯一索引冲突翻成**带编码**的 409，其余走 [`write_error`]。
fn supplier_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => supplier_code_conflict(code),
        _ => write_error(err, "新建供应商"),
    }
}

/// 编辑供应商。
#[nik::permission(
    path = "/business/erp/supplier/{id}",
    method = "PUT",
    permission = "business:erp:supplier:update"
)]
#[nik::put("/business/erp/supplier/{id}")]
pub async fn supplier_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSupplierReq>,
) -> ResResult<SupplierRes> {
    req.validate()?;
    let id = parse_uuid(&id, "id")?;
    let current = model::supplier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑供应商"))?
        .ok_or_else(|| not_found("供应商"))?;
    let supplier_name = req
        .supplier_name
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| current.supplier_name.clone());
    let text_or = |incoming: Option<Option<String>>, fallback: Option<String>| match incoming {
        None => fallback,
        Some(value) => value,
    };
    let short_name = text_or(req.short_name, current.short_name.clone());
    let contact_person = text_or(req.contact_person, current.contact_person.clone());
    let contact_phone = text_or(req.contact_phone, current.contact_phone.clone());
    let email = text_or(req.email, current.email.clone());
    let tax_no = text_or(req.tax_no, current.tax_no.clone());
    let address = text_or(req.address, current.address.clone());
    let credit_days = req.credit_days.unwrap_or(current.credit_days);
    let credit_limit = req.credit_limit.unwrap_or(current.credit_limit);
    let payable_account_id = match req.payable_account_id {
        None => current.payable_account_id,
        Some(None) => None,
        Some(Some(raw)) => {
            let account_id = parse_uuid(&raw, "payable_account_id")?;
            model::account_find_by_id(account_id)
                .await
                .map_err(|error| utils::db_error(error, "查询应付科目"))?
                .ok_or_else(|| reference_missing("应付科目不存在或已删除"))?;
            Some(account_id)
        }
    };
    let invoice_title = text_or(req.invoice_title, current.invoice_title.clone());
    let invoice_bank_name = text_or(req.invoice_bank_name, current.invoice_bank_name.clone());
    let invoice_bank_account = text_or(req.invoice_bank_account, current.invoice_bank_account.clone());
    let status = req.status.unwrap_or(current.status);
    let remark = text_or(req.remark, current.remark.clone());
    db().execute_with_params(
        "UPDATE biz_supplier SET supplier_name = ?, short_name = ?, contact_person = ?, \
             contact_phone = ?, email = ?, tax_no = ?, address = ?, credit_days = ?, \
             credit_limit = NULLIF(?::text,'')::numeric, payable_account_id = NULLIF(?::text,'')::uuid, \
             invoice_title = ?, invoice_bank_name = ?, invoice_bank_account = ?, status = ?, \
             remark = ?, updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
             WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(supplier_name),
            Value::from(short_name),
            Value::from(contact_person),
            Value::from(contact_phone),
            Value::from(email),
            Value::from(tax_no),
            Value::from(address),
            Value::I32(credit_days),
            Value::Text(credit_limit.to_string()),
            uuid_text(payable_account_id),
            Value::from(invoice_title),
            Value::from(invoice_bank_name),
            Value::from(invoice_bank_account),
            Value::I16(status),
            Value::from(remark),
            operator_of(&claims),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "编辑供应商"))?;
    let row = model::supplier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读供应商"))?
        .ok_or_else(|| not_found("供应商"))?;
    Ok(Res::ok(SupplierRes::from(&row)))
}

/// 删除供应商（软删）。
#[nik::permission(
    path = "/business/erp/supplier/{id}",
    method = "DELETE",
    permission = "business:erp:supplier:update"
)]
#[nik::delete("/business/erp/supplier/{id}")]
pub async fn supplier_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    let affected = db()
        .execute_with_params(
            "UPDATE biz_supplier SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![operator_of(&claims), Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "软删供应商"))?;
    if affected == 0 {
        return Err(not_found("供应商"));
    }
    Ok(Res::ok(()))
}

// ==================== ⑥ 只读台账（8 张：列表 + 详情） ====================

/// 会计科目分页。
#[nik::permission(
    path = "/business/erp/account/page",
    method = "GET",
    permission = "business:erp:account:list"
)]
#[nik::get("/business/erp/account/page")]
pub async fn account_page(Query(filter): Query<AccountFilter>) -> ResResult<PageData<Vec<AccountRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::account_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询会计科目"))?;
    let list = rows.iter().map(AccountRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 会计科目详情。
#[nik::permission(
    path = "/business/erp/account/{id}",
    method = "GET",
    permission = "business:erp:account:list"
)]
#[nik::get("/business/erp/account/{id}")]
pub async fn account_detail(Path(id): Path<String>) -> ResResult<AccountRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::account_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询会计科目详情"))?
        .ok_or_else(|| not_found("会计科目"))?;
    Ok(Res::ok(AccountRes::from(&row)))
}

/// 会计期间分页。
#[nik::permission(
    path = "/business/erp/period/page",
    method = "GET",
    permission = "business:erp:period:list"
)]
#[nik::get("/business/erp/period/page")]
pub async fn period_page(Query(filter): Query<PeriodFilter>) -> ResResult<PageData<Vec<PeriodRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::period_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询会计期间"))?;
    let list = rows.iter().map(PeriodRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 会计期间详情（本表无 `deleted_at`：期间不可删除）。
#[nik::permission(
    path = "/business/erp/period/{id}",
    method = "GET",
    permission = "business:erp:period:list"
)]
#[nik::get("/business/erp/period/{id}")]
pub async fn period_detail(Path(id): Path<String>) -> ResResult<PeriodRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::period_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间详情"))?
        .ok_or_else(|| not_found("会计期间"))?;
    Ok(Res::ok(PeriodRes::from(&row)))
}

/// 单号序列分页。
#[nik::permission(
    path = "/business/erp/seq/page",
    method = "GET",
    permission = "business:erp:seq:list"
)]
#[nik::get("/business/erp/seq/page")]
pub async fn seq_page(Query(filter): Query<SeqFilter>) -> ResResult<PageData<Vec<SeqRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::seq_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询单号序列"))?;
    let list = rows.iter().map(SeqRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 单号序列详情。
#[nik::permission(
    path = "/business/erp/seq/{id}",
    method = "GET",
    permission = "business:erp:seq:list"
)]
#[nik::get("/business/erp/seq/{id}")]
pub async fn seq_detail(Path(id): Path<String>) -> ResResult<SeqRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::seq_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询单号序列详情"))?
        .ok_or_else(|| not_found("单号序列"))?;
    Ok(Res::ok(SeqRes::from(&row)))
}

/// 采购订单分页。
#[nik::permission(
    path = "/business/erp/purchase-order/page",
    method = "GET",
    permission = "business:erp:purchase-order:list"
)]
#[nik::get("/business/erp/purchase-order/page")]
pub async fn purchase_order_page(
    Query(filter): Query<PurchaseOrderFilter>,
) -> ResResult<PageData<Vec<PurchaseOrderRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::purchase_order_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询采购订单"))?;
    let mut list: Vec<PurchaseOrderRes> = rows.iter().map(PurchaseOrderRes::from).collect();
    // P-FLOW-3A：列表要显示「审批中（FLW-xxx）」，实例单号批量回填一次
    crate::business::erp::purchase_handler::fill_approval_no(&mut list).await;
    // A3：供应商名称快照优先（存量行没快照 → 主档现值兜底），**不 JOIN 合同表**
    crate::business::erp::purchase_handler::fill_supplier_names(&mut list).await;
    // P2-8：所属项目名称按 id 现查（不是快照 —— 项目改名后列表跟着变）
    crate::business::erp::purchase_handler::fill_project_names(&mut list).await;
    // P2-9：仓库名**列表读快照**（老单快照为空 → 批量现查兜底，不写库）
    crate::business::erp::purchase_handler::fill_warehouse_names(&mut list).await;
    Ok(page_body(list, total, page, page_size))
}

/// 采购订单详情。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}",
    method = "GET",
    permission = "business:erp:purchase-order:list"
)]
#[nik::get("/business/erp/purchase-order/{id}")]
pub async fn purchase_order_detail(Path(id): Path<String>) -> ResResult<PurchaseOrderRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::purchase_order_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单详情"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let mut res = PurchaseOrderRes::from(&row);
    crate::business::erp::purchase_handler::fill_approval_no(std::slice::from_mut(&mut res)).await;
    crate::business::erp::purchase_handler::fill_supplier_names(std::slice::from_mut(&mut res)).await;
    // P2-8：详情同样回填项目名（关联查现值，口径见 fill_project_names）
    crate::business::erp::purchase_handler::fill_project_names(std::slice::from_mut(&mut res)).await;
    // P2-9：详情要**现值**（仓库改名后点进来看到现在的名字），快照另行下发
    crate::business::erp::purchase_handler::fill_warehouse_name_current(std::slice::from_mut(&mut res)).await;
    // FIX-2 ①：预算尚未生效的提示（草稿 / 审批中 / 未到生效日），只提示不阻断
    res.budget_warnings = crate::business::erp::purchase_handler::budget_warnings_of(&row).await;
    Ok(Res::ok(res))
}

/// 采购明细分页。
#[nik::permission(
    path = "/business/erp/purchase-order-item/page",
    method = "GET",
    permission = "business:erp:purchase-order-item:list"
)]
#[nik::get("/business/erp/purchase-order-item/page")]
pub async fn purchase_order_item_page(
    Query(filter): Query<PurchaseOrderItemFilter>,
) -> ResResult<PageData<Vec<PurchaseOrderItemRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::purchase_order_item_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询采购明细"))?;
    let list = rows.iter().map(PurchaseOrderItemRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 采购明细详情。
#[nik::permission(
    path = "/business/erp/purchase-order-item/{id}",
    method = "GET",
    permission = "business:erp:purchase-order-item:list"
)]
#[nik::get("/business/erp/purchase-order-item/{id}")]
pub async fn purchase_order_item_detail(Path(id): Path<String>) -> ResResult<PurchaseOrderItemRes> {
    let id = parse_uuid(&id, "id")?;
    let row = model::purchase_order_item_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购明细详情"))?
        .ok_or_else(|| not_found("采购明细"))?;
    Ok(Res::ok(PurchaseOrderItemRes::from(&row)))
}

/// 即时库存分页。
#[nik::permission(
    path = "/business/erp/inventory/page",
    method = "GET",
    permission = "business:erp:inventory:list"
)]
#[nik::get("/business/erp/inventory/page")]
pub async fn inventory_page(Query(filter): Query<InventoryFilter>) -> ResResult<PageData<Vec<InventoryRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::inventory_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询即时库存"))?;
    let list = rows.iter().map(InventoryRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 即时库存详情（主键查询；业务入口是 `material_id + warehouse_id` 的唯一行）。
#[nik::permission(
    path = "/business/erp/inventory/{id}",
    method = "GET",
    permission = "business:erp:inventory:list"
)]
#[nik::get("/business/erp/inventory/{id}")]
pub async fn inventory_detail(Path(id): Path<String>) -> ResResult<InventoryRes> {
    let id = parse_uuid(&id, "id")?;
    let row = find_row("biz_inventory", model::INVENTORY_COLUMNS, id, false)
        .await?
        .ok_or_else(|| not_found("库存行"))?;
    Ok(Res::ok(InventoryRes::from(
        &InventoryRow::from_row(&row).map_err(|error| utils::db_error(error, "解析库存行"))?,
    )))
}

/// 库位库存分页。
#[nik::permission(
    path = "/business/erp/inventory-location/page",
    method = "GET",
    permission = "business:erp:inventory-location:list"
)]
#[nik::get("/business/erp/inventory-location/page")]
pub async fn inventory_location_page(
    Query(filter): Query<InventoryLocationFilter>,
) -> ResResult<PageData<Vec<InventoryLocationRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::inventory_location_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询库位库存"))?;
    let list = rows.iter().map(InventoryLocationRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 库位库存详情。
#[nik::permission(
    path = "/business/erp/inventory-location/{id}",
    method = "GET",
    permission = "business:erp:inventory-location:list"
)]
#[nik::get("/business/erp/inventory-location/{id}")]
pub async fn inventory_location_detail(Path(id): Path<String>) -> ResResult<InventoryLocationRes> {
    let id = parse_uuid(&id, "id")?;
    let row = find_row("biz_inventory_location", model::INVENTORY_LOCATION_COLUMNS, id, false)
        .await?
        .ok_or_else(|| not_found("库位库存行"))?;
    Ok(Res::ok(InventoryLocationRes::from(
        &InventoryLocationRow::from_row(&row).map_err(|error| utils::db_error(error, "解析库位库存行"))?,
    )))
}

/// 库存流水分页。
#[nik::permission(
    path = "/business/erp/inventory-txn/page",
    method = "GET",
    permission = "business:erp:inventory-txn:list"
)]
#[nik::get("/business/erp/inventory-txn/page")]
pub async fn inventory_txn_page(Query(filter): Query<InventoryTxnFilter>) -> ResResult<PageData<Vec<InventoryTxnRes>>> {
    let page = filter.page;
    let page_size = filter.page_size;
    let (rows, total) = model::inventory_txn_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询库存流水"))?;
    let mut list: Vec<InventoryTxnRes> = rows.iter().map(InventoryTxnRes::from).collect();
    // AUD-02：操作人姓名，整页一次（流水量最大的一张表，逐行查必炸）
    crate::business::shared::user_name::fill_person_names(&mut list)
        .await
        .unwrap_or_default();
    Ok(page_body(list, total, page, page_size))
}

/// 库存流水详情。
#[nik::permission(
    path = "/business/erp/inventory-txn/{id}",
    method = "GET",
    permission = "business:erp:inventory-txn:list"
)]
#[nik::get("/business/erp/inventory-txn/{id}")]
pub async fn inventory_txn_detail(Path(id): Path<String>) -> ResResult<InventoryTxnRes> {
    let id = parse_uuid(&id, "id")?;
    let row = find_row("biz_inventory_txn", model::INVENTORY_TXN_COLUMNS, id, false)
        .await?
        .ok_or_else(|| not_found("库存流水"))?;
    let mut res = InventoryTxnRes::from(
        &InventoryTxnRow::from_row(&row).map_err(|error| utils::db_error(error, "解析库存流水"))?,
    );
    // AUD-02：操作人姓名（单条同一入口）
    crate::business::shared::user_name::fill_person_names(std::slice::from_mut(&mut res))
        .await
        .unwrap_or_default();
    Ok(Res::ok(res))
}

// ==================== 查询参数（只在本文件用的三个） ====================

/// 仓库树查询：`root_id` 为空 = 整棵树。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WarehouseTreeQuery {
    /// 子树根（为空表示从顶级开始）。
    pub root_id: Option<String>,
}

/// 库位下拉查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LocationListQuery {
    /// 所属仓库（必填）。
    pub warehouse_id: String,
    /// 是否包含停用库位。
    pub include_disabled: Option<bool>,
}

/// 物料搜索查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MaterialListQuery {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 启用位。
    pub status: Option<i16>,
}

// ==================== 库存巡检（不变式③，W2c-2） ====================

/// 巡检查询串（`?dry_run=true|false`）。
///
/// 用 `String` 而不是 `bool` 接收：`?dry_run=1` / `yes` / 空串这些前端常见写法
/// 交给 [`inv_audit::parse_dry_run`] 统一收口，框架的 `bool` 反序列化只认 `true/false`
/// （`?dry_run=1` 会直接 422，且报错文案与业务无关）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InventoryAuditQuery {
    /// 是否只报不修（**缺省 true**，红线③：防误修）。
    #[serde(default)]
    pub dry_run: Option<String>,
}

/// 库存巡检：`SUM(biz_inventory.qty)`（该物料×仓库**所有桶之和**）是否等于在库序列号件数（不变式③）。
///
/// `dry_run=true`（默认）只报不修；`false` 按序列号重算并修正
/// （单桶按差额调整、零桶建档、**多桶一律不修**并标 `MULTI_BUCKET`）。
/// 扫描与修正在**同一事务**里（否则「查到 3 个」与「修了 4 个」可能对不上）；
/// 扫描是一次 `WITH … SELECT`、修正是两条集合语句（`UPDATE` / `INSERT`），都不是 N+1。
#[nik::permission(
    path = "/business/erp/inventory/audit",
    method = "GET",
    permission = "business:erp:inventory:audit"
)]
#[nik::get("/business/erp/inventory/audit")]
pub async fn inventory_audit(Query(query): Query<InventoryAuditQuery>) -> ResResult<AuditReport> {
    let dry_run = inv_audit::parse_dry_run(query.dry_run.as_deref())
        .map_err(|msg| ApiError::new(400, error_code::ERP_FILTER_INVALID, msg))?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启库存巡检事务"))?;
    match inv_audit::audit(&tx, dry_run).await {
        Ok(report) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交库存巡检事务"))?;
            Ok(Res::ok(report))
        }
        Err(error) => {
            // 回滚失败也只是把原始错误报出去（回滚幂等，重复调用无害）
            let _ = tx.rollback().await;
            Err(utils::db_error(error, "库存巡检"))
        }
    }
}

// ==================== 单元测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;

    /// 取响应体：`Res<T>` 的 `data` 是 `Option<T>`，测试里统一在这里收口。
    fn data<T: Serialize>(res: Res<T>) -> T {
        res.data.expect("响应体应带 data")
    }

    // ---------- 纯逻辑用例（无需数据库） ----------

    /// ① 权限码清单：21 卡面码 + 8 只读码 = 29，无重复，前缀统一。
    ///
    /// BUG-SYS-05：物料分类 3 码（表未建、无路由、无注解）已移除，见 `CARD_PERMISSIONS` 文档。
    #[test]
    fn permissions_are_complete_and_unique() {
        let codes = all_permissions();
        assert_eq!(codes.len(), 29, "21 卡面码 + 8 只读码");
        assert_eq!(CARD_PERMISSIONS.len(), 21, "卡面码去掉预留的物料分类 3 码");
        assert_eq!(READONLY_PERMISSIONS.len(), 8, "8 张只读表各 1 个码");
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "权限码不得重复");
        assert!(
            codes.iter().all(|code| code.starts_with("business:erp:")),
            "所有码必须以 business:erp: 开头"
        );
        // 3 码制的两个实体：不允许出现 detail / delete 码（卡面只给 3 个）
        for entity in ["unit", "supplier"] {
            let prefix = format!("business:erp:{entity}:");
            let count = codes.iter().filter(|code| code.starts_with(&prefix)).count();
            assert_eq!(count, 3, "{entity} 是 3 码制");
        }
        // 5 码制的三个主力表
        for entity in ["warehouse", "location", "material"] {
            let prefix = format!("business:erp:{entity}:");
            let count = codes.iter().filter(|code| code.starts_with(&prefix)).count();
            assert_eq!(count, 5, "{entity} 是 5 码制");
        }
    }

    /// ② 仓库层级推导：顶级 1、下级 +1、第 4 层被拒（`66004`）。
    #[test]
    fn warehouse_level_is_derived_from_parent() {
        assert_eq!(next_level(None).expect("顶级"), 1);
        let mut row = sample_warehouse("/a/", 1, None);
        assert_eq!(next_level(Some(&row)).expect("第二层"), 2);
        row.level = 3;
        let error = next_level(Some(&row)).expect_err("第 4 层必须被拒");
        assert_eq!(error.code, error_code::ERP_WAREHOUSE_LEVEL_LIMIT);
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("最多 3 层"), "文案：{}", error.msg);
    }

    /// ③ 物化路径：顶级 `/id/`，子节点 `父路径 + id + "/"`（与 V0137 写法逐字一致）。
    #[test]
    fn warehouse_path_is_materialized() {
        let id = Uuid::parse_str("018f4b2c-1111-7222-8333-444455556666").unwrap();
        let root = child_path(None, id);
        assert_eq!(root, "/018f4b2c-1111-7222-8333-444455556666/");
        let parent = sample_warehouse(&root, 1, None);
        let child = child_path(Some(&parent), id);
        assert_eq!(
            child,
            "/018f4b2c-1111-7222-8333-444455556666/018f4b2c-1111-7222-8333-444455556666/"
        );
        // 上界（model 的 path_prefix_upper）必须覆盖全部后代
        let upper = model::path_prefix_upper(&parent.warehouse_path);
        assert!(child.as_str() >= parent.warehouse_path.as_str());
        assert!(child.as_str() < upper.as_str(), "子树必须落在 [path, upper) 区间内");
    }

    /// ④ 三层树组装：查全 3 层、子节点归位、顶级只有 1 个。
    #[test]
    fn warehouse_tree_assembles_three_levels() {
        let root_id = Uuid::now_v7();
        let sub_id = Uuid::now_v7();
        let leaf_id = Uuid::now_v7();
        let mut root = sample_warehouse(&format!("/{root_id}/"), 1, None);
        root.id = root_id;
        root.warehouse_code = "W1C-T-ROOT".to_string();
        let mut sub = sample_warehouse(&format!("/{root_id}/{sub_id}/"), 2, Some(root_id));
        sub.id = sub_id;
        sub.warehouse_code = "W1C-T-SUB".to_string();
        let mut leaf = sample_warehouse(&format!("/{root_id}/{sub_id}/{leaf_id}/"), 3, Some(sub_id));
        leaf.id = leaf_id;
        leaf.warehouse_code = "W1C-T-LEAF".to_string();
        let tree = assemble_tree(&[root, sub, leaf]);
        assert_eq!(tree.len(), 1, "只有一个顶级");
        assert_eq!(tree[0].children.len(), 1, "第 2 层 1 个");
        assert_eq!(tree[0].children[0].children.len(), 1, "第 3 层 1 个");
        assert!(tree[0].children[0].children[0].children.is_empty(), "第 3 层是叶子");
    }

    /// ④b 子树查询：根节点的父不在返回集合里 → 必须提升为根，不许静默丢节点。
    #[test]
    fn subtree_root_with_invisible_parent_is_promoted() {
        let outer_parent = Uuid::now_v7();
        let root_id = Uuid::now_v7();
        let child_id = Uuid::now_v7();
        // 根：parent_id 指向集合外的 outer_parent（root_id 查询的典型形态）
        let mut root = sample_warehouse(&format!("/{outer_parent}/{root_id}/"), 2, Some(outer_parent));
        root.id = root_id;
        let mut child = sample_warehouse(&format!("/{outer_parent}/{root_id}/{child_id}/"), 3, Some(root_id));
        child.id = child_id;
        let tree = assemble_tree(&[root, child]);
        assert_eq!(tree.len(), 1, "父不在集合内的节点必须是根，不能消失");
        assert_eq!(tree[0].node.id, root_id.to_string());
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].node.id, child_id.to_string());
    }

    /// ⑤ 分页口径：页码 0 → 1、page_size 0 → 默认、超上限被截到 200。
    #[test]
    fn page_args_are_normalized() {
        let (page, size) = model::normalize_page(0, 0);
        assert_eq!((page, size), (1, model::DEFAULT_PAGE_SIZE));
        let (page, size) = model::normalize_page(3, 10_000);
        assert_eq!((page, size), (3, model::MAX_PAGE_SIZE));
        assert_eq!(model::MAX_PAGE_SIZE, 200);
    }

    /// ⑥ 布尔入参 → SMALLINT（0/1），与库里的 CHECK 对齐。
    #[test]
    fn flags_map_to_smallint() {
        assert_eq!(flag_to_i16(true), 1);
        assert_eq!(flag_to_i16(false), 0);
    }

    /// ⑦ 非法 UUID → `400 + 66006`（不静默当「没传」，避免筛条件消失）。
    #[test]
    fn invalid_uuid_is_rejected() {
        let error = parse_uuid("not-a-uuid", "warehouse_id").expect_err("非法 UUID 必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("warehouse_id"), "文案要带字段名");
        let error = parse_uuid("   ", "id").expect_err("空串必须 400");
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(parse_uuid("018f4b2c-1111-7222-8333-444455556666", "id").is_ok());
    }

    /// ⑧ 可空参数走 `NULLIF(?::text,'')`：None → 空串（SQL 侧转 NULL），Some → 规范小写 UUID 文本。
    #[test]
    fn nullable_params_use_text_literal() {
        assert_eq!(uuid_text(None), Value::Text(String::new()));
        let id = Uuid::parse_str("018F4B2C-1111-7222-8333-444455556666").unwrap();
        assert_eq!(uuid_text(Some(id)), Value::Text(id.to_string()));
        assert_eq!(decimal_text(None), Value::Text(String::new()));
        assert_eq!(
            decimal_text(Some(Decimal::new(105000, 4))),
            Value::Text("10.5000".to_string()),
            "Decimal 转文本要保留标度"
        );
    }

    /// ⑨ 唯一冲突（DB 的 duplicate key）→ `409 + 66001`；其它错误仍是 500。
    #[test]
    fn duplicate_key_maps_to_conflict_code() {
        // 唯一冲突：按 DbError 变体判断（Display 是 "unique constraint violated constraint=…"，
        // 不含 "duplicate key" 字样，按文案猜会把唯一冲突判成 500）
        let error = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_unit_code".to_string()),
                source: None,
            },
            "测试",
        );
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_CODE_CONFLICT);
        assert!(error.msg.contains("uk_biz_unit_code"), "文案要带约束名：{}", error.msg);
        // CHECK 兜底 → 400（不是 500）
        let error = write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_warehouse_type".to_string()),
                source: None,
            },
            "测试",
        );
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        // 其它错误仍是 500
        let error = write_error(DbError::InvalidValue("别的错".to_string()), "测试");
        assert_eq!(error.status, 500);
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    /// ⑩ 软删口径：5 张主数据表的删除都是 `deleted_at` 赋值，且全文没有物理删除。
    ///
    /// 判据串在这里拼出来，避免「断言文本自己命中自己」（第一版与第二版都这么假红过）。
    #[test]
    fn deletes_are_soft() {
        let source = include_str!("handler.rs");
        let physical = ["DELETE", "FROM", "biz_"].join(" ");
        assert!(!source.contains(&physical), "ERP 写路径不得出现物理删除");
        let soft = ["SET", "deleted_at", "=", "now()"].join(" ");
        let count = source.matches(&soft).count();
        assert!(count >= 5, "5 张可写主数据表都要有软删语句，实测只有 {count} 处");
        // 三张 5 码制表的 DELETE 端点必须真的写过 deleted_at（防止有人改成物理删）
        for table in ["biz_warehouse", "biz_location", "biz_material"] {
            let marker = format!("UPDATE {table} {soft}");
            assert!(source.contains(&marker), "{table} 的删除必须走软删（找不到：{marker}）");
        }
    }

    /// ⑪ 特殊查询的路由路径互不冲突，且都在 `/business/erp/` 下。
    ///
    /// E2E08-FIX-2 ④：单位的 `next-code` 是**显式注册**的静态路由（返回 400「不提供取号」），
    /// 必须与 `/unit/{id}` 并存 —— axum（matchit）静态段优先于参数段，所以静态路由能命中，
    /// 不会被当成一个「id 不是 UUID」的请求。这条用例钉住「路径本身不重复」。
    #[test]
    fn special_routes_are_distinct() {
        let routes = [
            "/business/erp/warehouse/tree",
            "/business/erp/location/list",
            "/business/erp/material/list",
            "/business/erp/supplier/next-code",
            "/business/erp/material/next-code",
            "/business/erp/warehouse/next-code",
            "/business/erp/unit/next-code",
        ];
        let unique: std::collections::BTreeSet<_> = routes.iter().collect();
        assert_eq!(unique.len(), routes.len(), "特殊查询路径不得重复");
        assert!(
            routes.iter().all(|route| route.starts_with("/business/erp/")),
            "路径前缀统一"
        );
        // `/unit/next-code` 与 `/unit/{id}` 是两条不同的路由：前者是字面量，后者带参数占位
        assert!(
            !routes.iter().any(|route| route.contains("{id}")),
            "静态路由列表里不许出现参数占位（否则就不是「静态优先」了）"
        );
        // 下拉上限必须落在分页归一化的合法区间内；用运行时比较，别写成常量断言
        // （`clippy::assertions_on_constants` 在 `-D warnings` 下会直接红）
        let capped = model::normalize_page(1, MATERIAL_LIST_LIMIT).1;
        assert_eq!(capped, MATERIAL_LIST_LIMIT.min(model::MAX_PAGE_SIZE));
    }

    // ---------- 真库用例（`ERP_DB_URL`，与 W1b 同一套夹具约定） ----------

    use tokio::sync::Mutex;

    /// 真库用例必须串行（共用全局 `db()` 与同前缀编码）。
    static DB_LOCK: Mutex<()> = Mutex::const_new(());

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`，用例直接跳过。
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

    /// 拿锁 + 连库；`None` = 没配库，用例直接返回。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        Some(guard)
    }

    /// 假 claims（真库用例只用到 `sub` / `permissions` 两个字段）。
    fn claims() -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "018f4b2c-1111-7222-8333-444455556666".to_string(),
            username: Some("w1c-test".to_string()),
            roles: vec!["SUPER_ADMIN".to_string()],
            permissions: vec!["*".to_string()],
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    /// 每次运行都不同的编码（一次性库也怕重跑撞唯一索引）。
    fn code(prefix: &str) -> String {
        format!("W1C-{prefix}-{}", &Uuid::now_v7().simple().to_string()[..10])
    }

    /// 物理删掉本用例造的行（真库用例外不留痕；业务接口走的是软删）。
    async fn hard_delete(table: &str, id: Uuid) {
        let sql = format!("DELETE FROM {table} WHERE id = ?::uuid");
        let _ = db().execute_with_params(&sql, vec![Value::from(&id)]).await;
    }

    /// 造一个可挂库位的叶子仓库。
    async fn make_leaf_warehouse(tag: &str) -> Uuid {
        let req = CreateWarehouseReq {
            warehouse_code: Some(code(tag)),
            warehouse_name: format!("W1C 测试仓 {tag}"),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 0,
            remark: None,
            parent_id: None,
            level: 1,
        };
        let res = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect("新建仓库"),
        );
        Uuid::parse_str(&res.id).expect("仓库 id")
    }

    /// ⑫ 单位：建 → 详情 → 分页能查到。
    #[tokio::test]
    async fn unit_create_then_detail_and_page() {
        let Some(_guard) = setup().await else { return };
        let unit_code = code("U");
        let req = CreateUnitReq {
            unit_code: Some(unit_code.clone()),
            unit_name: "W1C 个".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: None,
            status: 1,
            sort_order: 0,
            remark: Some("w1c".to_string()),
            precision: None,
        };
        let created = data(
            unit_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect("新建单位"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.unit_code, unit_code);
        assert!(created.is_base);
        assert_eq!(created.rate, Decimal::ONE);

        let detail = data(unit_detail(Path(id.to_string())).await.expect("详情"));
        assert_eq!(detail.id, id.to_string());

        let filter = UnitFilter {
            keyword: Some(unit_code.clone()),
            status: None,
            page: 1,
            page_size: 10,
        };
        let page = data(unit_page(Query(filter)).await.expect("分页"));
        assert_eq!(page.total, 1, "关键词应精确命中 1 条");
        assert_eq!(page.list[0].unit_code, unit_code);

        hard_delete("biz_unit", id).await;
    }

    /// ⑬ 单位：重复编码 → `409 + 66001`，**文案点名编码**（E2E08-FIX-2 ③：
    /// 原来是裸 DB 口径「编码已存在（唯一约束 uk_biz_unit_code）」，用户不知道撞了哪条）。
    #[tokio::test]
    async fn unit_duplicate_code_conflicts() {
        let Some(_guard) = setup().await else { return };
        let unit_code = code("UDUP");
        let build = || CreateUnitReq {
            unit_code: Some(unit_code.clone()),
            unit_name: "W1C 重复".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: None,
            status: 1,
            sort_order: 0,
            remark: None,
            precision: None,
        };
        let created = data(
            unit_create(Extension(AuthClaims(claims())), Json(build()))
                .await
                .expect("首次新建"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        let error = unit_create(Extension(AuthClaims(claims())), Json(build()))
            .await
            .expect_err("重复编码必须失败");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_CODE_CONFLICT);
        assert_eq!(
            error.msg,
            format!("计量单位编码 `{unit_code}` 已存在（已软删的编码可以复用）"),
            "文案必须与其余五类逐字同口径，且点名具体编码"
        );
        // 预检（SELECT）先拦：不把裸约束名 `uk_biz_unit_code` 抛给用户
        assert!(
            !error.msg.contains("uk_biz_unit_code"),
            "不许出现裸 DB 口径：{}",
            error.msg
        );

        // 预检放过但 DB 唯一索引兜住（并发窗口）：本用例直接打 `unit_write_error` 验翻译
        let concurrent = unit_write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_unit_code".to_string()),
                source: None,
            },
            &unit_code,
        );
        assert_eq!(concurrent.status, 409);
        assert_eq!(concurrent.code, error_code::ERP_CODE_CONFLICT);
        assert!(
            concurrent.msg.contains(&unit_code),
            "并发兜底也要点名编码：{}",
            concurrent.msg
        );

        hard_delete("biz_unit", id).await;
    }

    /// ⑬-b 单位：**没传 unit_code** ⇒ `400 + 10001 + 人话`，不是框架反序列化的 422
    /// （E2E08-FIX-2 ②；其余五类此时走「自动取号」，单位不取号所以必须报错）。
    ///
    /// 不需要真库：`validate` 在碰 DB 之前就返回。
    #[tokio::test]
    async fn unit_create_without_code_is_400_with_human_message() {
        for missing in [None, Some(String::new()), Some("   ".to_string())] {
            let req = CreateUnitReq {
                unit_code: missing.clone(),
                unit_name: "E2E08F2 无编码单位".to_string(),
                is_base: true,
                base_unit_id: None,
                rate: None,
                status: 1,
                sort_order: 0,
                remark: None,
                precision: None,
            };
            let error = unit_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect_err("缺编码必须被拒");
            assert_eq!(
                error.status, 400,
                "必须是业务 400（422 会让前端按错误码分支拿不到人话）"
            );
            assert_eq!(error.code, error_code::PARAM_INVALID);
            assert_eq!(
                error.msg, "计量单位编码必填（单位不自动取号）",
                "文案要人话且点明「不自动取号」（入参 {missing:?}）"
            );
        }

        // 反序列化层：JSON 里根本没有 unit_code 也不能 422
        let body = r#"{"unit_name":"E2E08F2 直接反序列化"}"#;
        let req: CreateUnitReq = serde_json::from_str(body).expect("缺 unit_code 必须能反序列化");
        assert!(req.unit_code.is_none(), "缺席 → None（不是 422）");
        assert_eq!(
            req.unit_code_required().expect_err("缺席必须报错").msg,
            "计量单位编码必填（单位不自动取号）"
        );
    }

    /// ⑬-c 单位：`GET /unit/next-code` 返回**明确的** 400（E2E08-FIX-2 ④）。
    #[tokio::test]
    async fn unit_next_code_is_explicit_400() {
        let error = unit_next_code(Extension(AuthClaims(claims())))
            .await
            .expect_err("单位不提供取号，必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(
            error.msg.contains("计量单位不提供自动取号"),
            "文案要点明结论：{}",
            error.msg
        );
        assert!(
            error.msg.contains("unit_code"),
            "文案要给出下一步（手工填 unit_code）：{}",
            error.msg
        );
        // 关键：不再是「被 /unit/{id} 吃掉」那句（66006 = id 不是 UUID）
        assert_ne!(error.code, error_code::ERP_FILTER_INVALID, "不许退化成路由错");
    }

    /// ⑭ 单位：编辑名称 + 折算率；转成基础单位后 rate 归 1。
    #[tokio::test]
    async fn unit_update_applies_patch_semantics() {
        let Some(_guard) = setup().await else { return };
        let base_req = CreateUnitReq {
            unit_code: Some(code("UBASE")),
            unit_name: "W1C 基础".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: None,
            status: 1,
            sort_order: 0,
            remark: None,
            precision: None,
        };
        let base = data(
            unit_create(Extension(AuthClaims(claims())), Json(base_req))
                .await
                .expect("基础单位"),
        );
        let base_id = Uuid::parse_str(&base.id).unwrap();

        let box_req = CreateUnitReq {
            unit_code: Some(code("UBOX")),
            unit_name: "W1C 箱".to_string(),
            is_base: false,
            base_unit_id: Some(base_id.to_string()),
            rate: Some(Decimal::new(12, 0)),
            status: 1,
            sort_order: 1,
            remark: None,
            precision: None,
        };
        let created = data(
            unit_create(Extension(AuthClaims(claims())), Json(box_req))
                .await
                .expect("折算单位"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.base_unit_id.as_deref(), Some(base_id.to_string().as_str()));

        let update = UpdateUnitReq {
            unit_name: Some("W1C 箱（改名）".to_string()),
            base_unit_id: Some(None),
            rate: None,
            status: Some(0),
            sort_order: None,
            remark: None,
            precision: None,
        };
        let updated = data(
            unit_update(Extension(AuthClaims(claims())), Path(id.to_string()), Json(update))
                .await
                .expect("编辑单位"),
        );
        assert_eq!(updated.unit_name, "W1C 箱（改名）");
        assert!(updated.is_base, "清空 base_unit_id 后应变成基础单位");
        assert_eq!(updated.rate, Decimal::ONE, "基础单位 rate 归 1");
        assert_eq!(updated.status, 0);

        hard_delete("biz_unit", id).await;
        hard_delete("biz_unit", base_id).await;
    }

    /// ⑮ 单位：软删后详情 404、分页查不到。
    #[tokio::test]
    async fn unit_soft_delete_hides_row() {
        let Some(_guard) = setup().await else { return };
        let unit_code = code("UDEL");
        let req = CreateUnitReq {
            unit_code: Some(unit_code.clone()),
            unit_name: "W1C 待删".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: None,
            status: 1,
            sort_order: 0,
            remark: None,
            precision: None,
        };
        let created = data(
            unit_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect("新建单位"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        unit_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
            .await
            .expect("删除单位");

        let error = unit_detail(Path(id.to_string())).await.expect_err("删后详情必须 404");
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);

        let filter = UnitFilter {
            keyword: Some(unit_code),
            status: None,
            page: 1,
            page_size: 10,
        };
        let page = data(unit_page(Query(filter)).await.expect("分页"));
        assert_eq!(page.total, 0, "软删行不得出现在列表");

        // 软删行仍在表里（deleted_at 非空）
        let remains: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_unit WHERE id = ?::uuid AND deleted_at IS NOT NULL",
                vec![Value::from(&id)],
            )
            .await
            .expect("查软删行");
        assert_eq!(remains, 1, "软删是 UPDATE 不是 DELETE");

        hard_delete("biz_unit", id).await;
    }

    /// ⑮b ERP-S1（V0187）：系统预置单位不能删除，只能停用。
    ///
    /// 判据：`is_system = true` → DELETE 接口 409 + 行还在；
    /// 把 status 改成 0（停用）→ 200 且回读 status = 0（预置值还能停用，不是全锁死）。
    #[tokio::test]
    async fn system_unit_cannot_be_deleted_but_can_be_disabled() {
        let Some(_guard) = setup().await else { return };
        // 造一个「系统预置」单位：接口不允许建 is_system=true，只能直接写库（模拟迁移种子）
        let insert = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code("USYS")),
                    unit_name: "W1C 预置".to_string(),
                    is_base: true,
                    base_unit_id: None,
                    rate: None,
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: Some(3),
                }),
            )
            .await
            .expect("新建单位"),
        );
        let id = Uuid::parse_str(&insert.id).unwrap();
        assert!(!insert.is_system, "客户自建单位必须是 is_system = false");
        assert_eq!(insert.precision, 3, "precision 要落库并回读");

        db().execute_with_params(
            "UPDATE biz_unit SET is_system = true WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("标记为系统预置");

        let error = unit_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
            .await
            .expect_err("系统预置单位不允许删除");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
        assert!(
            error.msg.contains("系统预置") && error.msg.contains("停用"),
            "文案要说清「不能删、只能停用」：{}",
            error.msg
        );
        let still_there = data(unit_detail(Path(id.to_string())).await.expect("行还在"));
        assert!(still_there.is_system);

        // 停用是允许的收口
        let disabled = data(
            unit_update(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(UpdateUnitReq {
                    status: Some(0),
                    ..Default::default()
                }),
            )
            .await
            .expect("停用系统预置单位"),
        );
        assert_eq!(disabled.status, 0);
        assert!(disabled.is_system, "停用不改变「系统预置」身份");

        hard_delete("biz_unit", id).await;
    }

    /// ⑮c ERP-S1（V0187）：`precision` 走接口落库、可改，越界 400（不静默截断）。
    #[tokio::test]
    async fn unit_precision_round_trip_and_rejects_overflow() {
        let Some(_guard) = setup().await else { return };
        let req = CreateUnitReq {
            unit_code: Some(code("UPREC")),
            unit_name: "W1C 精度".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: None,
            status: 1,
            sort_order: 0,
            remark: None,
            precision: Some(0),
        };
        let created = data(
            unit_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect("新建单位"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.precision, 0);

        let updated = data(
            unit_update(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(UpdateUnitReq {
                    precision: Some(3),
                    ..Default::default()
                }),
            )
            .await
            .expect("改精度"),
        );
        assert_eq!(updated.precision, 3);
        let row = model::unit_find_by_id(id).await.unwrap().unwrap();
        assert_eq!(row.precision, 3, "precision 必须真的写进 biz_unit 列");

        // 越界在 DTO 校验就被挡下（400），不会走到 SQL 让 CHECK 兜底
        let error = unit_create(
            Extension(AuthClaims(claims())),
            Json(CreateUnitReq {
                unit_code: Some(code("UBADP")),
                unit_name: "W1C 越界".to_string(),
                is_base: true,
                base_unit_id: None,
                rate: None,
                status: 1,
                sort_order: 0,
                remark: None,
                precision: Some(7),
            }),
        )
        .await
        .expect_err("precision = 7 必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        hard_delete("biz_unit", id).await;
    }

    /// ⑯ 仓库：建顶级 → 路径 `/id/`、level 1、is_leaf 1。
    #[tokio::test]
    async fn warehouse_create_root_builds_path() {
        let Some(_guard) = setup().await else { return };
        let id = make_leaf_warehouse("WROOT").await;
        let row = model::warehouse_find_by_id(id)
            .await
            .expect("查仓库")
            .expect("仓库存在");
        assert_eq!(row.level, 1);
        assert_eq!(row.warehouse_path, format!("/{id}/"));
        assert_eq!(row.is_leaf, 1);
        assert!(row.parent_id.is_none());
        hard_delete("biz_warehouse", id).await;
    }

    /// ⑰ 仓库：建 3 层子树 → 路径前缀 / 上级 is_leaf 变 0 / 第 4 层 400+66004。
    #[tokio::test]
    async fn warehouse_three_levels_and_limit() {
        let Some(_guard) = setup().await else { return };
        let root = make_leaf_warehouse("W3L1").await;
        let sub_req = CreateWarehouseReq {
            warehouse_code: Some(code("W3L2")),
            warehouse_name: "W1C 分仓".to_string(),
            warehouse_type: "RAW".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 1,
            remark: None,
            parent_id: Some(root.to_string()),
            level: 2,
        };
        let sub = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(sub_req))
                .await
                .expect("建第 2 层"),
        );
        let sub_id = Uuid::parse_str(&sub.id).unwrap();
        assert_eq!(sub.level, 2);
        assert_eq!(sub.warehouse_path, format!("/{root}/{sub_id}/"));
        let root_row = model::warehouse_find_by_id(root).await.unwrap().unwrap();
        assert_eq!(root_row.is_leaf, 0, "有了下级就不再是叶子");

        let leaf_req = CreateWarehouseReq {
            warehouse_code: Some(code("W3L3")),
            warehouse_name: "W1C 库房".to_string(),
            warehouse_type: "FINISHED".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 2,
            remark: None,
            parent_id: Some(sub_id.to_string()),
            level: 3,
        };
        let leaf = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(leaf_req))
                .await
                .expect("建第 3 层"),
        );
        let leaf_id = Uuid::parse_str(&leaf.id).unwrap();
        assert_eq!(leaf.level, 3);

        // 第 4 层：level 给 3（dto 只允许 1..=3 的关系），层级守卫先拦
        let too_deep = CreateWarehouseReq {
            warehouse_code: Some(code("W3L4")),
            warehouse_name: "W1C 第4层".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 3,
            remark: None,
            parent_id: Some(leaf_id.to_string()),
            level: 3,
        };
        let error = warehouse_create(Extension(AuthClaims(claims())), Json(too_deep))
            .await
            .expect_err("第 4 层必须被拒");
        assert_eq!(error.code, error_code::ERP_WAREHOUSE_LEVEL_LIMIT);
        assert_eq!(error.status, 400);

        // level 与上级不一致 → 400 + 10001（上级是第 2 层 ⇒ 推导应为 3，这里故意填 2）
        let wrong_level = CreateWarehouseReq {
            warehouse_code: Some(code("W3LX")),
            warehouse_name: "W1C 层级不符".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 4,
            remark: None,
            parent_id: Some(sub_id.to_string()),
            level: 2,
        };
        let error = warehouse_create(Extension(AuthClaims(claims())), Json(wrong_level))
            .await
            .expect_err("level 不一致必须被拒");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("应为 3"), "文案：{}", error.msg);

        hard_delete("biz_warehouse", leaf_id).await;
        hard_delete("biz_warehouse", sub_id).await;
        hard_delete("biz_warehouse", root).await;
    }

    /// ⑱ 仓库树：整棵树查全 3 层；按 `root_id` 只回子树。
    #[tokio::test]
    async fn warehouse_tree_returns_all_levels() {
        let Some(_guard) = setup().await else { return };
        let root = make_leaf_warehouse("WT1").await;
        let sub_req = CreateWarehouseReq {
            warehouse_code: Some(code("WT2")),
            warehouse_name: "W1C 树分仓".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 1,
            remark: None,
            parent_id: Some(root.to_string()),
            level: 2,
        };
        let sub = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(sub_req))
                .await
                .expect("建第 2 层"),
        );
        let sub_id = Uuid::parse_str(&sub.id).unwrap();
        let leaf_req = CreateWarehouseReq {
            warehouse_code: Some(code("WT3")),
            warehouse_name: "W1C 树库房".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 2,
            remark: None,
            parent_id: Some(sub_id.to_string()),
            level: 3,
        };
        let leaf = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(leaf_req))
                .await
                .expect("建第 3 层"),
        );
        let leaf_id = Uuid::parse_str(&leaf.id).unwrap();

        let whole = data(
            warehouse_tree(Query(WarehouseTreeQuery::default()))
                .await
                .expect("整棵树"),
        );
        let root_node = whole
            .iter()
            .find(|node| node.node.id == root.to_string())
            .expect("整棵树里应有根节点");
        assert_eq!(root_node.children.len(), 1);
        assert_eq!(root_node.children[0].node.id, sub_id.to_string());
        assert_eq!(root_node.children[0].children.len(), 1);
        assert_eq!(root_node.children[0].children[0].node.id, leaf_id.to_string());

        let subtree = data(
            warehouse_tree(Query(WarehouseTreeQuery {
                root_id: Some(sub_id.to_string()),
            }))
            .await
            .expect("子树"),
        );
        assert_eq!(subtree.len(), 1, "子树根只有 1 个");
        assert_eq!(subtree[0].node.id, sub_id.to_string());
        assert_eq!(subtree[0].children.len(), 1);

        hard_delete("biz_warehouse", leaf_id).await;
        hard_delete("biz_warehouse", sub_id).await;
        hard_delete("biz_warehouse", root).await;
    }

    /// ⑲ 仓库删除：有下级 → `409 + 66003`；删完下级后上级 is_leaf 复位。
    #[tokio::test]
    async fn warehouse_delete_guards_children() {
        let Some(_guard) = setup().await else { return };
        let root = make_leaf_warehouse("WDEL").await;
        let sub_req = CreateWarehouseReq {
            warehouse_code: Some(code("WDEL2")),
            warehouse_name: "W1C 待删子仓".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 1,
            remark: None,
            parent_id: Some(root.to_string()),
            level: 2,
        };
        let sub = data(
            warehouse_create(Extension(AuthClaims(claims())), Json(sub_req))
                .await
                .expect("建子仓"),
        );
        let sub_id = Uuid::parse_str(&sub.id).unwrap();

        let error = warehouse_delete(Extension(AuthClaims(claims())), Path(root.to_string()))
            .await
            .expect_err("有下级不能删");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_WAREHOUSE_HAS_CHILDREN);

        warehouse_delete(Extension(AuthClaims(claims())), Path(sub_id.to_string()))
            .await
            .expect("删子仓");
        let root_row = model::warehouse_find_by_id(root).await.unwrap().unwrap();
        assert_eq!(root_row.is_leaf, 1, "删掉最后一个下级后应复位为叶子");

        hard_delete("biz_warehouse", sub_id).await;
        hard_delete("biz_warehouse", root).await;
    }

    /// ⑳ 库位：叶子仓库可建；非叶子 → `400 + 66005`；`warehouse_id` 过滤生效。
    #[tokio::test]
    async fn location_create_and_list_by_warehouse() {
        let Some(_guard) = setup().await else { return };
        let leaf = make_leaf_warehouse("WLOC").await;
        let location_code = code("L");
        let req = CreateLocationReq {
            location_code: location_code.clone(),
            warehouse_id: leaf.to_string(),
            location_name: Some("W1C A-01".to_string()),
            location_type: "STORAGE".to_string(),
            is_default: true,
            capacity: Some(Decimal::new(1000000, 4)),
            status: 1,
            sort_order: 1,
            remark: None,
        };
        let created = data(
            location_create(Extension(AuthClaims(claims())), Json(req))
                .await
                .expect("建库位"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.location_code, location_code);
        assert!(created.is_default);

        let list = data(
            location_list(Query(LocationListQuery {
                warehouse_id: leaf.to_string(),
                include_disabled: None,
            }))
            .await
            .expect("库位下拉"),
        );
        assert!(list.iter().any(|item| item.id == id.to_string()));

        // 非叶子仓库（刚建了子仓的那条链）不能挂库位
        let non_leaf = make_leaf_warehouse("WPARENT").await;
        let sub = data(
            warehouse_create(
                Extension(AuthClaims(claims())),
                Json(CreateWarehouseReq {
                    warehouse_code: Some(code("WPARENT2")),
                    warehouse_name: "W1C 非叶子".to_string(),
                    warehouse_type: "NORMAL".to_string(),
                    manager: None,
                    allow_negative: false,
                    address: None,
                    status: 1,
                    sort_order: 1,
                    remark: None,
                    parent_id: Some(non_leaf.to_string()),
                    level: 2,
                }),
            )
            .await
            .expect("建子仓"),
        );
        let sub_id = Uuid::parse_str(&sub.id).unwrap();
        let error = location_create(
            Extension(AuthClaims(claims())),
            Json(CreateLocationReq {
                location_code: code("LBAD"),
                warehouse_id: non_leaf.to_string(),
                location_name: None,
                location_type: "STORAGE".to_string(),
                is_default: false,
                capacity: None,
                status: 1,
                sort_order: 0,
                remark: None,
            }),
        )
        .await
        .expect_err("非叶子仓库不能挂库位");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_WAREHOUSE_NOT_LEAF);

        hard_delete("biz_location", id).await;
        hard_delete("biz_warehouse", sub_id).await;
        hard_delete("biz_warehouse", non_leaf).await;
        hard_delete("biz_warehouse", leaf).await;
    }

    /// ㉑ 库位：同仓第二个默认位会把第一个清掉（部分唯一索引不会炸）。
    #[tokio::test]
    async fn location_default_is_exclusive() {
        let Some(_guard) = setup().await else { return };
        let leaf = make_leaf_warehouse("WDEF").await;
        let first = data(
            location_create(
                Extension(AuthClaims(claims())),
                Json(CreateLocationReq {
                    location_code: code("LD1"),
                    warehouse_id: leaf.to_string(),
                    location_name: None,
                    location_type: "STORAGE".to_string(),
                    is_default: true,
                    capacity: None,
                    status: 1,
                    sort_order: 1,
                    remark: None,
                }),
            )
            .await
            .expect("第一个默认位"),
        );
        let first_id = Uuid::parse_str(&first.id).unwrap();
        let second = data(
            location_create(
                Extension(AuthClaims(claims())),
                Json(CreateLocationReq {
                    location_code: code("LD2"),
                    warehouse_id: leaf.to_string(),
                    location_name: None,
                    location_type: "STORAGE".to_string(),
                    is_default: true,
                    capacity: None,
                    status: 1,
                    sort_order: 2,
                    remark: None,
                }),
            )
            .await
            .expect("第二个默认位"),
        );
        let second_id = Uuid::parse_str(&second.id).unwrap();

        let first_row = model::location_find_by_id(first_id).await.unwrap().unwrap();
        assert_eq!(first_row.is_default, 0, "旧默认位必须被清掉");
        let second_row = model::location_find_by_id(second_id).await.unwrap().unwrap();
        assert_eq!(second_row.is_default, 1);

        hard_delete("biz_location", first_id).await;
        hard_delete("biz_location", second_id).await;
        hard_delete("biz_warehouse", leaf).await;
    }

    /// ㉒ 物料：建（引用不存在的单位 → 400+66002）→ 建成功 → 关键词搜索 → 软删。
    #[tokio::test]
    async fn material_reference_search_and_soft_delete() {
        let Some(_guard) = setup().await else { return };
        let unit = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code("MU")),
                    unit_name: "W1C 物料单位".to_string(),
                    is_base: true,
                    base_unit_id: None,
                    rate: None,
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: None,
                }),
            )
            .await
            .expect("建单位"),
        );
        let unit_id = Uuid::parse_str(&unit.id).unwrap();

        let error = material_create(
            Extension(AuthClaims(claims())),
            Json(CreateMaterialReq {
                material_code: Some(code("MBAD")),
                material_name: "W1C 坏物料".to_string(),
                spec: None,
                unit_id: Uuid::now_v7().to_string(),
                material_type: "RAW".to_string(),
                default_warehouse_id: None,
                safety_stock: None,
                costing_method: "MOVING_AVG".to_string(),
                inventory_account_id: None,
                tax_rate: None,
                standard_price: None,
                status: 1,
                serial_required: None,
                remark: None,
            }),
        )
        .await
        .expect_err("单位不存在必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_REFERENCE_MISSING);

        let material_code = code("M-SEARCH");
        let created = data(
            material_create(
                Extension(AuthClaims(claims())),
                Json(CreateMaterialReq {
                    material_code: Some(material_code.clone()),
                    material_name: "W1C 六角螺栓".to_string(),
                    spec: Some("M8x20".to_string()),
                    unit_id: unit_id.to_string(),
                    material_type: "RAW".to_string(),
                    default_warehouse_id: None,
                    safety_stock: Some(Decimal::new(105000, 4)),
                    costing_method: "MOVING_AVG".to_string(),
                    inventory_account_id: None,
                    tax_rate: Some(Decimal::new(1300, 4)),
                    standard_price: None,
                    status: 1,
                    serial_required: None,
                    remark: None,
                }),
            )
            .await
            .expect("建物料"),
        );
        let material_id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.safety_stock, Decimal::new(105000, 4));

        // 关键词搜索：编码前缀能命中
        let found = data(
            material_list(Query(MaterialListQuery {
                keyword: Some("W1C-M-SEARCH".to_string()),
                status: None,
            }))
            .await
            .expect("搜索物料"),
        );
        assert!(
            found.iter().any(|item| item.id == material_id.to_string()),
            "关键词搜索必须命中刚建的物料"
        );

        // 编辑：改名 + 清空 spec
        let updated = data(
            material_update(
                Extension(AuthClaims(claims())),
                Path(material_id.to_string()),
                Json(UpdateMaterialReq {
                    material_name: Some("W1C 六角螺栓（改名）".to_string()),
                    spec: Some(None),
                    unit_id: None,
                    material_type: None,
                    default_warehouse_id: None,
                    safety_stock: None,
                    costing_method: None,
                    inventory_account_id: None,
                    tax_rate: None,
                    standard_price: None,
                    status: None,
                    serial_required: None,
                    remark: None,
                }),
            )
            .await
            .expect("编辑物料"),
        );
        assert_eq!(updated.material_name, "W1C 六角螺栓（改名）");
        assert!(updated.spec.is_none());

        // 软删 → 详情 404
        material_delete(Extension(AuthClaims(claims())), Path(material_id.to_string()))
            .await
            .expect("删物料");
        let error = material_detail(Path(material_id.to_string()))
            .await
            .expect_err("删后详情 404");
        assert_eq!(error.status, 404);

        hard_delete("biz_material", material_id).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// ★ 本卡补口（DC-09 缺口 2）：物料主数据的计量单位必须是**基本单位**。
    ///
    /// 折算单位（箱/打）挂在物料上 = 这个物料的**任何单据**都会被建单期的单位止血闸 400 拒，
    /// 用户只会看到「这个物料的单建不了」，根因却藏在主数据里。建 / 改两条路都要拦。
    ///
    /// 顺带钉住存量语义：已挂在折算单位上的物料**不改单位也不能保存**（要先把单位改回基本单位）。
    /// 这是有意为之（生产实测 0 行，命中先出清单给业务），不是 bug。
    #[tokio::test]
    async fn material_unit_must_be_base() {
        let Some(_guard) = setup().await else { return };
        // 基本单位「个」+ 折算单位「箱」（1 箱 = 12 个）
        let base = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code("UBASE")),
                    unit_name: "W1C 个".to_string(),
                    is_base: true,
                    base_unit_id: None,
                    rate: None,
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: None,
                }),
            )
            .await
            .expect("建基本单位"),
        );
        let base_id = Uuid::parse_str(&base.id).unwrap();
        let boxed = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code("UBOX")),
                    unit_name: "W1C 箱".to_string(),
                    is_base: false,
                    base_unit_id: Some(base_id.to_string()),
                    rate: Some(Decimal::from(12)),
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: None,
                }),
            )
            .await
            .expect("建折算单位"),
        );
        let box_id = Uuid::parse_str(&boxed.id).unwrap();

        // ---------- ① Create：挂在折算单位上 → 400 + 66030 + 文案，一行都不落库 ----------
        let rejected_code = code("MBADU");
        let error = material_create(
            Extension(AuthClaims(claims())),
            Json(CreateMaterialReq {
                material_code: Some(rejected_code.clone()),
                material_name: "W1C 箱装物料".to_string(),
                spec: None,
                unit_id: box_id.to_string(),
                material_type: "RAW".to_string(),
                default_warehouse_id: None,
                safety_stock: None,
                costing_method: "MOVING_AVG".to_string(),
                inventory_account_id: None,
                tax_rate: None,
                standard_price: None,
                status: 1,
                serial_required: None,
                remark: None,
            }),
        )
        .await
        .expect_err("折算单位不许建物料");
        assert_eq!(error.status, 400, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
        assert!(
            error.msg.contains(
                "物料的计量单位必须是基本单位：折算单位（箱/打）只能用于显示，\
                 本版本单据不支持多单位换算（当前「W1C 箱」）"
            ),
            "{}",
            error.msg
        );
        let landed: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_material WHERE material_code = ?",
                vec![Value::from(rejected_code.clone())],
            )
            .await
            .unwrap();
        assert_eq!(landed, 0, "被拒的物料不许落库");

        // ---------- ② 正常基本单位 → 200 ----------
        let created = data(
            material_create(
                Extension(AuthClaims(claims())),
                Json(CreateMaterialReq {
                    material_code: Some(code("MOK")),
                    material_name: "W1C 六角螺栓".to_string(),
                    spec: Some("M8x20".to_string()),
                    unit_id: base_id.to_string(),
                    material_type: "RAW".to_string(),
                    default_warehouse_id: None,
                    safety_stock: None,
                    costing_method: "MOVING_AVG".to_string(),
                    inventory_account_id: None,
                    tax_rate: None,
                    standard_price: None,
                    status: 1,
                    serial_required: None,
                    remark: None,
                }),
            )
            .await
            .expect("基本单位能建物料"),
        );
        let material_id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.unit_id, base_id.to_string());

        // ---------- ③ Update：把单位改成折算单位 → 400，库里单位没被改 ----------
        let error = material_update(
            Extension(AuthClaims(claims())),
            Path(material_id.to_string()),
            Json(UpdateMaterialReq {
                unit_id: Some(box_id.to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect_err("折算单位不许改成物料单位");
        assert_eq!(error.status, 400, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
        assert!(
            error.msg.contains("本版本单据不支持多单位换算（当前「W1C 箱」）"),
            "{}",
            error.msg
        );
        let row = model::material_find_by_id(material_id)
            .await
            .unwrap()
            .expect("物料必须还在");
        assert_eq!(row.unit_id, base_id, "被拒的编辑不许改单位");

        // 不改单位的正常编辑 → 200
        let updated = data(
            material_update(
                Extension(AuthClaims(claims())),
                Path(material_id.to_string()),
                Json(UpdateMaterialReq {
                    material_name: Some("W1C 六角螺栓（改名）".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .expect("基本单位的物料能保存"),
        );
        assert_eq!(updated.material_name, "W1C 六角螺栓（改名）");

        // ---------- ④ 存量已挂折算单位的物料：不改单位也不能保存，改回基本单位就能存 ----------
        let legacy_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
             VALUES (?::uuid, ?, 'W1C 存量箱装物料', ?::uuid, 1, false)",
            vec![
                Value::from(&legacy_id),
                Value::from(code("MLEGACY")),
                Value::from(&box_id),
            ],
        )
        .await
        .expect("插存量折算单位物料（模拟升级前的物料）");
        let error = material_update(
            Extension(AuthClaims(claims())),
            Path(legacy_id.to_string()),
            Json(UpdateMaterialReq {
                remark: Some(Some("只改备注".to_string())),
                ..Default::default()
            }),
        )
        .await
        .expect_err("存量折算单位物料不能保存");
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
        let fixed = data(
            material_update(
                Extension(AuthClaims(claims())),
                Path(legacy_id.to_string()),
                Json(UpdateMaterialReq {
                    unit_id: Some(base_id.to_string()),
                    remark: Some(Some("先把单位改回基本单位".to_string())),
                    ..Default::default()
                }),
            )
            .await
            .expect("改回基本单位就能保存"),
        );
        assert_eq!(fixed.unit_id, base_id.to_string());

        hard_delete("biz_material", material_id).await;
        hard_delete("biz_material", legacy_id).await;
        hard_delete("biz_unit", box_id).await;
        hard_delete("biz_unit", base_id).await;
    }

    /// ㉒-B 标准售价写接口 + 取价链第 ④ 级（E2E-FIX-4 ① / ② 的真库闭环）。
    ///
    /// 走的是**真实 handler**（`material_create` / `material_update`）与真实取价
    /// （`price_handler::resolve_price`），不是改库造数：
    ///
    /// ① 建物料带 `standard_price = 11.5` → 落库并可回读；
    /// ② `GET /price/resolve`（无客户、无阶梯）→ 命中 `STANDARD`，且回口径标注：
    ///    `tax_inclusive = false`、`price_excl = price`、`tax_rate = 物料税率`；
    /// ③ 编辑成 `20` → 第 ④ 级给新价（证明改接口真的生效）；
    /// ④ 编辑清空（`standard_price: null`）→ 第 ④ 级不再命中 → `NONE`。
    #[tokio::test]
    async fn material_standard_price_feeds_price_resolve() {
        use crate::business::erp::price_dto::PriceResolveQuery;

        let Some(_guard) = setup().await else { return };
        let unit = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code("SPU")),
                    unit_name: "E2E4 标准价单位".to_string(),
                    is_base: true,
                    base_unit_id: None,
                    rate: None,
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: None,
                }),
            )
            .await
            .expect("建单位"),
        );
        let unit_id = Uuid::parse_str(&unit.id).unwrap();

        // ---------- ① 建：带标准售价 + 13% 税率 ----------
        let created = data(
            material_create(
                Extension(AuthClaims(claims())),
                Json(CreateMaterialReq {
                    material_code: Some(code("MSTD")),
                    material_name: "E2E4 标准价物料".to_string(),
                    spec: None,
                    unit_id: unit_id.to_string(),
                    material_type: "RAW".to_string(),
                    default_warehouse_id: None,
                    safety_stock: None,
                    costing_method: "MOVING_AVG".to_string(),
                    inventory_account_id: None,
                    tax_rate: Some(Decimal::from(13)),
                    standard_price: Some(Decimal::new(115000, 4)),
                    status: 1,
                    serial_required: None,
                    remark: None,
                }),
            )
            .await
            .expect("建带标准售价的物料"),
        );
        let material_id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(
            created.standard_price,
            Some(Decimal::new(115000, 4)),
            "建接口必须能写标准售价（原实现无此字段，取价第 ④ 级只能改库）"
        );
        let row = model::material_find_by_id(material_id)
            .await
            .unwrap()
            .expect("物料必须落库");
        assert_eq!(row.standard_price, Some(Decimal::new(115000, 4)), "落库值");

        // ---------- ② 取价：无客户 / 无阶梯 → 第 ④ 级 ----------
        let resolve = |qty: &str| {
            crate::business::erp::price_handler::resolve_price(PriceResolveQuery {
                customer_id: None,
                material_id: Some(material_id.to_string()),
                qty: Some(qty.to_string()),
                date: None,
            })
        };
        let hit = resolve("1").await.expect("取价");
        assert_eq!(hit.source, "STANDARD", "只维护了标准售价 → 命中第 ④ 级");
        assert_eq!(hit.price, Some(Decimal::new(115000, 4)));
        assert_eq!(
            hit.source_id,
            Some(material_id.to_string()),
            "STANDARD 的 source_id = 物料 id"
        );
        assert!(!hit.tax_inclusive, "标准售价本身是不含税价");
        assert_eq!(
            hit.price_excl,
            Some(Decimal::new(115000, 4)),
            "不含税价不再折算，price_excl = price"
        );
        assert_eq!(hit.tax_rate, Some(Decimal::from(13)), "折算税率 = 物料税率");

        // ---------- ③ 改：20 → 第 ④ 级给新价 ----------
        let updated = data(
            material_update(
                Extension(AuthClaims(claims())),
                Path(material_id.to_string()),
                Json(UpdateMaterialReq {
                    standard_price: Some(Some(Decimal::from(20))),
                    ..Default::default()
                }),
            )
            .await
            .expect("改标准售价"),
        );
        assert_eq!(updated.standard_price, Some(Decimal::from(20)), "改接口生效");
        let hit = resolve("1").await.expect("取价");
        assert_eq!(hit.source, "STANDARD");
        assert_eq!(hit.price, Some(Decimal::from(20)), "改完立刻按新价取");

        // ---------- ④ 清空（null）→ 第 ④ 级不再命中 ----------
        let cleared = data(
            material_update(
                Extension(AuthClaims(claims())),
                Path(material_id.to_string()),
                Json(UpdateMaterialReq {
                    standard_price: Some(None),
                    ..Default::default()
                }),
            )
            .await
            .expect("清空标准售价"),
        );
        assert_eq!(cleared.standard_price, None, "`null` = 清空");
        let miss = resolve("1").await.expect("取价");
        assert_eq!(miss.source, "NONE", "标准售价清空后第 ④ 级不再命中");
        assert_eq!(miss.price, None);
        assert_eq!(miss.price_excl, None, "没取到价就没有不含税价");
        assert_eq!(miss.tax_rate, None, "没取到价就不给折算税率");

        // ---------- ⑤ 0 元被校验挡住（不落库、不留假价）----------
        let error = material_update(
            Extension(AuthClaims(claims())),
            Path(material_id.to_string()),
            Json(UpdateMaterialReq {
                standard_price: Some(Some(Decimal::ZERO)),
                ..Default::default()
            }),
        )
        .await
        .expect_err("0 元标准售价必须被拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("standard_price"), "{}", error.msg);
        let row = model::material_find_by_id(material_id).await.unwrap().unwrap();
        assert_eq!(row.standard_price, None, "被拒的编辑不许改库");

        hard_delete("biz_material", material_id).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// ㉓ 供应商：建 → 详情 → 编辑 → 软删。
    #[tokio::test]
    async fn supplier_crud_cycle() {
        let Some(_guard) = setup().await else { return };
        let supplier_code = code("SUP");
        let created = data(
            supplier_create(
                Extension(AuthClaims(claims())),
                Json(CreateSupplierReq {
                    supplier_code: Some(supplier_code.clone()),
                    supplier_name: "W1C 供应商".to_string(),
                    short_name: Some("W1C 供应".to_string()),
                    contact_person: Some("张三".to_string()),
                    contact_phone: Some("13800000000".to_string()),
                    email: None,
                    tax_no: None,
                    address: None,
                    credit_days: 30,
                    credit_limit: Some(Decimal::new(123456789, 4)),
                    payable_account_id: None,
                    invoice_title: None,
                    invoice_bank_name: None,
                    invoice_bank_account: None,
                    status: 1,
                    remark: None,
                }),
            )
            .await
            .expect("建供应商"),
        );
        let id = Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.credit_days, 30);

        let detail = data(supplier_detail(Path(id.to_string())).await.expect("详情"));
        assert_eq!(detail.supplier_code, supplier_code);

        let updated = data(
            supplier_update(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(UpdateSupplierReq {
                    supplier_name: Some("W1C 供应商（改名）".to_string()),
                    short_name: Some(None),
                    contact_person: None,
                    contact_phone: None,
                    email: None,
                    tax_no: None,
                    address: None,
                    credit_days: Some(60),
                    credit_limit: None,
                    payable_account_id: None,
                    invoice_title: None,
                    invoice_bank_name: None,
                    invoice_bank_account: None,
                    status: Some(0),
                    remark: None,
                }),
            )
            .await
            .expect("编辑供应商"),
        );
        assert_eq!(updated.supplier_name, "W1C 供应商（改名）");
        assert!(updated.short_name.is_none(), "显式 null 应清空简称");
        assert_eq!(updated.credit_days, 60);
        assert_eq!(updated.status, 0);

        supplier_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
            .await
            .expect("删供应商");
        assert!(
            model::supplier_find_by_id(id).await.unwrap().is_none(),
            "软删后按主键查不到"
        );
        hard_delete("biz_supplier", id).await;
    }

    // ==================== CODE-02：供应商编码自动取号 ====================
    //
    // 这几条用例会真的**消耗 SUP 号段**（自动取号就是被验证的行为本身），只按 id
    // 物理删掉自己建的行，不回退 `biz_seq.current_no`：主数据是档案，建成功就该永久占号。

    /// `SUP-0001` 形态（前缀 + `-` + ≥4 位纯流水）。
    fn is_auto_code(code: &str, prefix: &str) -> bool {
        let Some(rest) = code.strip_prefix(prefix) else {
            return false;
        };
        let Some(digits) = rest.strip_prefix('-') else {
            return false;
        };
        digits.len() >= 4 && digits.chars().all(|ch| ch.is_ascii_digit())
    }

    /// 流水号（取最后一段数字）。
    fn seq_no(code: &str) -> i64 {
        code.rsplit('-')
            .next()
            .and_then(|digits| digits.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("编码里没有流水段：{code}"))
    }

    /// 造一个供应商请求：`supplier_code` 传 `None` = 留空自动取号。
    fn supplier_req(supplier_code: Option<&str>, supplier_name: &str) -> CreateSupplierReq {
        CreateSupplierReq {
            supplier_code: supplier_code.map(str::to_string),
            supplier_name: supplier_name.to_string(),
            short_name: None,
            contact_person: None,
            contact_phone: None,
            email: None,
            tax_no: None,
            address: None,
            credit_days: 30,
            credit_limit: Some(Decimal::ZERO),
            payable_account_id: None,
            invoice_title: None,
            invoice_bank_name: None,
            invoice_bank_account: None,
            status: 1,
            remark: None,
        }
    }

    /// ㉕ CODE-02 ①：`null` / `""` / 纯空白三种「没填」都自动取号，且真落库。
    #[tokio::test]
    async fn supplier_auto_code_generated_when_blank() {
        let Some(_guard) = setup().await else { return };

        let by_none = create_supplier(None, supplier_req(None, "自动编码供应商（未传）"))
            .await
            .expect("supplier_code 缺席应自动取号");
        assert!(
            is_auto_code(&by_none.supplier_code, "SUP"),
            "自动编码必须是 SUP-0001 形态，实际 {}",
            by_none.supplier_code
        );
        let id = Uuid::parse_str(&by_none.id).expect("id");
        let row = model::supplier_find_by_id(id).await.expect("回读").expect("行存在");
        assert_eq!(row.supplier_code, by_none.supplier_code, "自动编码必须真落库");

        let by_empty = create_supplier(None, supplier_req(Some(""), "自动编码供应商（空串）"))
            .await
            .expect("空串应自动取号");
        assert!(
            is_auto_code(&by_empty.supplier_code, "SUP"),
            "{}",
            by_empty.supplier_code
        );
        assert_ne!(by_empty.supplier_code, by_none.supplier_code, "两次自动取号不得同号");

        let by_blank = create_supplier(None, supplier_req(Some("   "), "自动编码供应商（空白）"))
            .await
            .expect("纯空白应自动取号");
        assert!(
            is_auto_code(&by_blank.supplier_code, "SUP"),
            "{}",
            by_blank.supplier_code
        );
        assert_ne!(by_blank.supplier_code, by_empty.supplier_code, "三次自动取号两两不同");

        for created in [by_none, by_empty, by_blank] {
            hard_delete("biz_supplier", Uuid::parse_str(&created.id).expect("id")).await;
        }
    }

    /// ㉖ CODE-02 ②：手工编码原样保留（不强制 `SUP-xxx` 形态），只 trim。
    #[tokio::test]
    async fn supplier_manual_code_is_preserved() {
        let Some(_guard) = setup().await else { return };
        let manual = "W1C-GYS-001";
        let created = create_supplier(None, supplier_req(Some(&format!("  {manual}  ")), "手工编码供应商"))
            .await
            .expect("手工编码应保留");
        assert_eq!(created.supplier_code, manual, "手工编码只 trim，不改成 SUP-xxx");

        hard_delete("biz_supplier", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// ㉗ CODE-02 ③：连续两次自动取号 → 流水 +1。
    #[tokio::test]
    async fn supplier_auto_code_increments_consecutively() {
        let Some(_guard) = setup().await else { return };
        let first = create_supplier(None, supplier_req(None, "连续取号甲"))
            .await
            .expect("建甲");
        let second = create_supplier(None, supplier_req(None, "连续取号乙"))
            .await
            .expect("建乙");
        assert_eq!(
            seq_no(&second.supplier_code),
            seq_no(&first.supplier_code) + 1,
            "连续自动取号必须 +1（{} → {}）",
            first.supplier_code,
            second.supplier_code
        );

        hard_delete("biz_supplier", Uuid::parse_str(&first.id).expect("id")).await;
        hard_delete("biz_supplier", Uuid::parse_str(&second.id).expect("id")).await;
    }

    /// ㉘ CODE-02 ④：手工编码重复 → `409 + 66001`，文案点名具体编码（不靠 DB 唯一索引兜底）。
    #[tokio::test]
    async fn supplier_duplicate_code_rejected_with_message() {
        let Some(_guard) = setup().await else { return };
        let manual = code("SUPDUP");
        let created = create_supplier(None, supplier_req(Some(&manual), "重复编码一号"))
            .await
            .expect("首次创建应成功");
        let error = create_supplier(None, supplier_req(Some(&manual), "重复编码二号"))
            .await
            .expect_err("重复手工编码必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(error.msg.contains(&manual), "错误文案要点名具体编码：{}", error.msg);

        hard_delete("biz_supplier", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// ㉔-CODE05 供应商编码预览：**只读不占号**（方案 B），保存时才真取号。
    #[tokio::test]
    async fn supplier_preview_code_does_not_consume_sequence() {
        let Some(_guard) = setup().await else { return };

        let first = preview_supplier_code().await.expect("预览应成功");
        assert!(is_auto_code(&first, "SUP"), "预览必须是 SUP-0001 形态，实际 {first}");
        let second = preview_supplier_code().await.expect("再预览应成功");
        assert_eq!(
            first, second,
            "预览不得推进 biz_seq（点了「生成编码」又取消不能浪费号）"
        );

        // 保存时才真取号：留空编码建单拿到的正是预览的那个号
        let created = create_supplier(None, supplier_req(None, "预览后建单供应商"))
            .await
            .expect("留空编码应自动取号");
        assert_eq!(
            created.supplier_code, first,
            "自动取号必须用掉预览的那个号（预览不占号，取号在保存时）"
        );

        let third = preview_supplier_code().await.expect("预览应成功");
        assert_ne!(third, first, "号已被保存用掉，再预览必须给新号");

        hard_delete("biz_supplier", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// ㉕-CODE05 供应商：预览号当**手工编码**提交 ⇒ 原样保留；撞号按 409 + 66001 兜。
    #[tokio::test]
    async fn supplier_previewed_code_is_used_verbatim_on_create() {
        let Some(_guard) = setup().await else { return };

        let previewed = preview_supplier_code().await.expect("预览应成功");
        let created = create_supplier(None, supplier_req(Some(&previewed), "用预览号建单供应商"))
            .await
            .expect("预览号当手工编码应创建成功");
        assert_eq!(
            created.supplier_code, previewed,
            "手工提交的编码必须原样保留（不能被再取号覆盖）"
        );

        let after = preview_supplier_code().await.expect("预览应成功");
        assert_ne!(after, previewed, "预览必须跳过已占用的号");

        let conflict = create_supplier(None, supplier_req(Some(&previewed), "预览号重复供应商"))
            .await
            .expect_err("已占用的编码必须被拒");
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(conflict.msg.contains(&previewed), "文案要点名编码：{}", conflict.msg);

        hard_delete("biz_supplier", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// ㉔ 仓库分页：`page_size` 截断与 `keyword` 组合。
    #[tokio::test]
    async fn warehouse_page_combines_keyword_and_page_size() {
        let Some(_guard) = setup().await else { return };
        let marker = code("WPAGE");
        let mut ids = Vec::new();
        for index in 0..3 {
            let created = data(
                warehouse_create(
                    Extension(AuthClaims(claims())),
                    Json(CreateWarehouseReq {
                        warehouse_code: Some(format!("{marker}-{index}")),
                        warehouse_name: format!("W1C 分页仓 {index}"),
                        warehouse_type: "NORMAL".to_string(),
                        manager: None,
                        allow_negative: false,
                        address: None,
                        status: 1,
                        sort_order: index,
                        remark: None,
                        parent_id: None,
                        level: 1,
                    }),
                )
                .await
                .expect("建分页仓"),
            );
            ids.push(Uuid::parse_str(&created.id).unwrap());
        }
        let filter = WarehouseFilter {
            keyword: Some(marker.clone()),
            warehouse_type: None,
            level: None,
            parent_id: None,
            status: None,
            page: 1,
            page_size: 2,
        };
        let page = data(warehouse_page(Query(filter)).await.expect("分页"));
        assert_eq!(page.total, 3, "关键词命中 3 条");
        assert_eq!(page.list.len(), 2, "page_size=2 只回 2 条");
        assert_eq!(page.page, 1);
        assert_eq!(page.page_size, 2);

        for id in ids {
            warehouse_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
                .await
                .expect("清场");
        }
    }

    /// ㉕ 只读台账：8 张表的列表接口都能正常分页（空库也不报错）。
    #[tokio::test]
    async fn readonly_ledgers_are_pageable() {
        let Some(_guard) = setup().await else { return };
        account_page(Query(AccountFilter::default())).await.expect("科目");
        period_page(Query(PeriodFilter::default())).await.expect("期间");
        seq_page(Query(SeqFilter::default())).await.expect("序列");
        purchase_order_page(Query(PurchaseOrderFilter::default()))
            .await
            .expect("采购单");
        purchase_order_item_page(Query(PurchaseOrderItemFilter::default()))
            .await
            .expect("采购明细");
        inventory_page(Query(InventoryFilter::default())).await.expect("库存");
        inventory_location_page(Query(InventoryLocationFilter::default()))
            .await
            .expect("库位库存");
        inventory_txn_page(Query(InventoryTxnFilter::default()))
            .await
            .expect("库存流水");
    }

    /// ㉖ 只读台账详情：不存在的 id → 404（不是 500）。
    #[tokio::test]
    async fn readonly_detail_returns_404() {
        let Some(_guard) = setup().await else { return };
        let missing = Uuid::now_v7().to_string();
        for outcome in [
            inventory_detail(Path(missing.clone())).await.map(|_| ()),
            inventory_location_detail(Path(missing.clone())).await.map(|_| ()),
            inventory_txn_detail(Path(missing.clone())).await.map(|_| ()),
            account_detail(Path(missing.clone())).await.map(|_| ()),
            period_detail(Path(missing.clone())).await.map(|_| ()),
            seq_detail(Path(missing.clone())).await.map(|_| ()),
            purchase_order_detail(Path(missing.clone())).await.map(|_| ()),
            purchase_order_item_detail(Path(missing.clone())).await.map(|_| ()),
        ] {
            let error = outcome.expect_err("不存在的 id 必须 404");
            assert_eq!(error.status, 404);
            assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);
        }
    }

    /// 夹具：一个最小可用的仓库行（纯逻辑用例用，不落库）。
    fn sample_warehouse(path: &str, level: i16, parent_id: Option<Uuid>) -> WarehouseRow {
        WarehouseRow {
            id: Uuid::now_v7(),
            warehouse_code: "W1C-SAMPLE".to_string(),
            warehouse_name: "W1C 样例仓".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: 0,
            address: None,
            status: 1,
            sort_order: 0,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
            parent_id,
            level,
            warehouse_path: path.to_string(),
            is_leaf: 1,
        }
    }

    // ==================== CODE-03：物料 / 仓库编码自动取号 ====================
    //
    // 同 CODE-02：这几条用例会真的**消耗 MAT / WH 号段**（自动取号就是被验证的行为本身），
    // 只按 id 物理删掉自己建的行，不回退 `biz_seq.current_no`：主数据是档案，建成功就该永久占号。

    /// 仓库编码形态：`WH-` + ≥2 位纯流水（WH 的 `biz_seq.pad_width = 2`，`WH-01`）。
    ///
    /// 与物料的 4 位形态分开写：位数是**每类主数据自己的规则**，混用会掩盖取号规则的漂移。
    fn is_auto_wh_code(code: &str) -> bool {
        let Some(digits) = code.strip_prefix("WH-") else {
            return false;
        };
        digits.len() >= 2 && digits.chars().all(|ch| ch.is_ascii_digit())
    }

    /// 造一个基本计量单位（物料必须挂基本单位），返回 id。
    async fn base_unit(tag: &str) -> Uuid {
        let unit = data(
            unit_create(
                Extension(AuthClaims(claims())),
                Json(CreateUnitReq {
                    unit_code: Some(code(&format!("U{tag}"))),
                    unit_name: format!("W1C 单位 {tag}"),
                    is_base: true,
                    base_unit_id: None,
                    rate: None,
                    status: 1,
                    sort_order: 0,
                    remark: None,
                    precision: None,
                }),
            )
            .await
            .expect("建基本单位"),
        );
        Uuid::parse_str(&unit.id).expect("单位 id")
    }

    /// 造一个物料请求：`material_code` 传 `None` = 留空自动取号。
    fn material_req(material_code: Option<&str>, unit_id: &Uuid, name: &str) -> CreateMaterialReq {
        CreateMaterialReq {
            material_code: material_code.map(str::to_string),
            material_name: name.to_string(),
            spec: None,
            unit_id: unit_id.to_string(),
            material_type: "RAW".to_string(),
            default_warehouse_id: None,
            safety_stock: None,
            costing_method: "MOVING_AVG".to_string(),
            inventory_account_id: None,
            tax_rate: None,
            standard_price: None,
            status: 1,
            serial_required: None,
            remark: None,
        }
    }

    /// 造一个仓库请求：`warehouse_code` 传 `None` = 留空自动取号。
    fn warehouse_req(warehouse_code: Option<&str>, name: &str) -> CreateWarehouseReq {
        CreateWarehouseReq {
            warehouse_code: warehouse_code.map(str::to_string),
            warehouse_name: name.to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 0,
            remark: None,
            parent_id: None,
            level: 1,
        }
    }

    /// ㉙ CODE-03 ①：物料 `null` / `""` / 纯空白三种「没填」都自动取号，且真落库。
    #[tokio::test]
    async fn material_auto_code_generated_when_blank() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MAT").await;

        let by_none = create_material(None, material_req(None, &unit_id, "自动编码物料（未传）"))
            .await
            .expect("material_code 缺席应自动取号");
        assert!(
            is_auto_code(&by_none.material_code, "MAT"),
            "自动编码必须是 MAT-0001 形态，实际 {}",
            by_none.material_code
        );
        let id = Uuid::parse_str(&by_none.id).expect("id");
        let row = model::material_find_by_id(id).await.expect("回读").expect("行存在");
        assert_eq!(row.material_code, by_none.material_code, "自动编码必须真落库");

        let by_empty = create_material(None, material_req(Some(""), &unit_id, "自动编码物料（空串）"))
            .await
            .expect("空串应自动取号");
        assert!(
            is_auto_code(&by_empty.material_code, "MAT"),
            "{}",
            by_empty.material_code
        );
        assert_ne!(by_empty.material_code, by_none.material_code, "两次自动取号不得同号");

        let by_blank = create_material(None, material_req(Some("   "), &unit_id, "自动编码物料（空白）"))
            .await
            .expect("纯空白应自动取号");
        assert!(
            is_auto_code(&by_blank.material_code, "MAT"),
            "{}",
            by_blank.material_code
        );
        assert_ne!(by_blank.material_code, by_empty.material_code, "三次自动取号两两不同");

        for created in [by_none, by_empty, by_blank] {
            hard_delete("biz_material", Uuid::parse_str(&created.id).expect("id")).await;
        }
        hard_delete("biz_unit", unit_id).await;
    }

    /// ㉚ CODE-03 ②：物料手工编码原样保留（按分类自编号 `YB-001-02` 也要能用），只 trim。
    #[tokio::test]
    async fn material_manual_code_is_preserved() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MATM").await;
        let manual = "W1C-YB-001-02";
        let created = create_material(
            None,
            material_req(Some(&format!("  {manual}  ")), &unit_id, "手工编码物料"),
        )
        .await
        .expect("手工编码应保留");
        assert_eq!(created.material_code, manual, "手工编码只 trim，不改成 MAT-xxx");

        hard_delete("biz_material", Uuid::parse_str(&created.id).expect("id")).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// ㉛ CODE-03 ③：物料连续两次自动取号 → 流水 +1。
    #[tokio::test]
    async fn material_auto_code_increments_consecutively() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MATC").await;
        let first = create_material(None, material_req(None, &unit_id, "连续取号物料甲"))
            .await
            .expect("建甲");
        let second = create_material(None, material_req(None, &unit_id, "连续取号物料乙"))
            .await
            .expect("建乙");
        assert_eq!(
            seq_no(&second.material_code),
            seq_no(&first.material_code) + 1,
            "连续自动取号必须 +1（{} → {}）",
            first.material_code,
            second.material_code
        );

        hard_delete("biz_material", Uuid::parse_str(&first.id).expect("id")).await;
        hard_delete("biz_material", Uuid::parse_str(&second.id).expect("id")).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// ㉜ CODE-03 ④：物料手工编码重复 → `409 + 66001`，文案点名具体编码（不靠唯一索引兜底）。
    #[tokio::test]
    async fn material_duplicate_code_rejected_with_message() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MATD").await;
        let manual = code("MATDUP");
        let created = create_material(None, material_req(Some(&manual), &unit_id, "重复编码物料一号"))
            .await
            .expect("首次创建应成功");
        let error = create_material(None, material_req(Some(&manual), &unit_id, "重复编码物料二号"))
            .await
            .expect_err("重复手工编码必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(error.msg.contains(&manual), "错误文案要点名具体编码：{}", error.msg);

        hard_delete("biz_material", Uuid::parse_str(&created.id).expect("id")).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// ㉝ CODE-03 ⑤：仓库 `null` / `""` / 纯空白三种「没填」都自动取号（**2 位** `WH-01`），且真落库。
    #[tokio::test]
    async fn warehouse_auto_code_generated_when_blank() {
        let Some(_guard) = setup().await else { return };

        let by_none = create_warehouse(None, warehouse_req(None, "自动编码仓库（未传）"))
            .await
            .expect("warehouse_code 缺席应自动取号");
        assert!(
            is_auto_wh_code(&by_none.warehouse_code),
            "自动编码必须是 WH-01 形态（2 位流水），实际 {}",
            by_none.warehouse_code
        );
        let id = Uuid::parse_str(&by_none.id).expect("id");
        let row = model::warehouse_find_by_id(id).await.expect("回读").expect("行存在");
        assert_eq!(row.warehouse_code, by_none.warehouse_code, "自动编码必须真落库");
        assert_eq!(row.level, 1, "自动编码不影响层级推导");
        assert_eq!(row.warehouse_path, format!("/{id}/"));

        let by_empty = create_warehouse(None, warehouse_req(Some(""), "自动编码仓库（空串）"))
            .await
            .expect("空串应自动取号");
        assert!(is_auto_wh_code(&by_empty.warehouse_code), "{}", by_empty.warehouse_code);
        assert_ne!(by_empty.warehouse_code, by_none.warehouse_code, "两次自动取号不得同号");

        let by_blank = create_warehouse(None, warehouse_req(Some("   "), "自动编码仓库（空白）"))
            .await
            .expect("纯空白应自动取号");
        assert!(is_auto_wh_code(&by_blank.warehouse_code), "{}", by_blank.warehouse_code);
        assert_ne!(by_blank.warehouse_code, by_empty.warehouse_code, "三次自动取号两两不同");

        for created in [by_none, by_empty, by_blank] {
            hard_delete("biz_warehouse", Uuid::parse_str(&created.id).expect("id")).await;
        }
    }

    /// ㉞ CODE-03 ⑥：仓库手工编码原样保留（`CK-01` 这类企业自有编码），只 trim。
    #[tokio::test]
    async fn warehouse_manual_code_is_preserved() {
        let Some(_guard) = setup().await else { return };
        let manual = "W1C-CK-01";
        let created = create_warehouse(None, warehouse_req(Some(&format!("  {manual}  ")), "手工编码仓库"))
            .await
            .expect("手工编码应保留");
        assert_eq!(created.warehouse_code, manual, "手工编码只 trim，不改成 WH-xx");

        hard_delete("biz_warehouse", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// ㉟ CODE-03 ⑦：仓库连续两次自动取号 → 流水 +1（2 位号段同口径）。
    #[tokio::test]
    async fn warehouse_auto_code_increments_consecutively() {
        let Some(_guard) = setup().await else { return };
        let first = create_warehouse(None, warehouse_req(None, "连续取号仓库甲"))
            .await
            .expect("建甲");
        let second = create_warehouse(None, warehouse_req(None, "连续取号仓库乙"))
            .await
            .expect("建乙");
        assert_eq!(
            seq_no(&second.warehouse_code),
            seq_no(&first.warehouse_code) + 1,
            "连续自动取号必须 +1（{} → {}）",
            first.warehouse_code,
            second.warehouse_code
        );

        hard_delete("biz_warehouse", Uuid::parse_str(&first.id).expect("id")).await;
        hard_delete("biz_warehouse", Uuid::parse_str(&second.id).expect("id")).await;
    }

    /// ㊱ CODE-03 ⑧：仓库手工编码重复 → `409 + 66001`，文案点名具体编码。
    #[tokio::test]
    async fn warehouse_duplicate_code_rejected_with_message() {
        let Some(_guard) = setup().await else { return };
        let manual = code("WHDUP");
        let created = create_warehouse(None, warehouse_req(Some(&manual), "重复编码仓库一号"))
            .await
            .expect("首次创建应成功");
        let error = create_warehouse(None, warehouse_req(Some(&manual), "重复编码仓库二号"))
            .await
            .expect_err("重复手工编码必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(error.msg.contains(&manual), "错误文案要点名具体编码：{}", error.msg);

        hard_delete("biz_warehouse", Uuid::parse_str(&created.id).expect("id")).await;
    }

    // ==================== CODE-06：物料 / 仓库编码预览（交互方式 B） ====================
    //
    // 前端「生成编码」按钮 → `GET /business/erp/{material,warehouse}/next-code`。
    // 这几条钉死两件事：**预览不占号**（方案 B）、**预览号当手工编码提交时原样保留**。
    // 号段仍会因保存而推进（自动取号本来就是被验证的行为），故只按 id 物理删行。

    /// CODE-06 ①：物料预览**只读不占号** —— 连点两次拿到同一个号；真正取号发生在保存时。
    #[tokio::test]
    async fn material_preview_code_does_not_consume_sequence() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MPV").await;

        let first = preview_material_code().await.expect("物料编码预览应成功");
        assert!(
            is_auto_code(&first, "MAT"),
            "预览必须是纯流水 4 位的 MAT-0001 形态，实际 {first}"
        );
        let second = preview_material_code().await.expect("再预览应成功");
        assert_eq!(
            first, second,
            "预览不得推进 biz_seq（点了「生成编码」又取消不能浪费号）"
        );

        // 保存时才真取号：留空编码建单拿到的正是刚才预览的号
        let created = create_material(None, material_req(None, &unit_id, "预览后建单物料"))
            .await
            .expect("留空编码应自动取号");
        assert_eq!(
            created.material_code, first,
            "自动取号必须用掉预览的那个号（说明预览没占号、取号在保存时）"
        );

        let third = preview_material_code().await.expect("预览应成功");
        assert_ne!(
            third, first,
            "号已被保存用掉，再预览必须给出新的号（{first} → {third}）"
        );

        hard_delete("biz_material", Uuid::parse_str(&created.id).expect("id")).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// CODE-06 ②：物料预览号当**手工编码**提交 ⇒ 原样保留；撞号按 `409 + 66001` 兜。
    ///
    /// 钉住「点生成 → 填入编码框 → 保存」时后端不能偷偷把它当自动号再取一次。
    #[tokio::test]
    async fn material_previewed_code_is_used_verbatim_on_create() {
        let Some(_guard) = setup().await else { return };
        let unit_id = base_unit("MPV2").await;

        let previewed = preview_material_code().await.expect("物料编码预览应成功");
        let created = create_material(None, material_req(Some(&previewed), &unit_id, "用预览号建单物料"))
            .await
            .expect("预览号当手工编码应创建成功");
        assert_eq!(
            created.material_code, previewed,
            "手工提交的编码必须原样保留（不能被再取号覆盖）"
        );

        // 该号已被占用 ⇒ 下一个预览必须跳过它
        let after = preview_material_code().await.expect("预览应成功");
        assert_ne!(after, previewed, "预览必须跳过已占用的号");

        // 同一号再建一次 ⇒ 409 + 66001（并发抢号的兜底口径）
        let conflict = create_material(None, material_req(Some(&previewed), &unit_id, "预览号重复物料"))
            .await
            .expect_err("已占用的编码必须被拒");
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(conflict.msg.contains(&previewed), "文案要点名编码：{}", conflict.msg);

        hard_delete("biz_material", Uuid::parse_str(&created.id).expect("id")).await;
        hard_delete("biz_unit", unit_id).await;
    }

    /// CODE-06 ③：仓库预览**只读不占号** —— 连点两次同一个号，**2 位**流水形态（`WH-01`）。
    #[tokio::test]
    async fn warehouse_preview_code_does_not_consume_sequence() {
        let Some(_guard) = setup().await else { return };

        let first = preview_warehouse_code().await.expect("仓库编码预览应成功");
        assert!(
            is_auto_wh_code(&first),
            "预览必须是 WH-01 形态（2 位流水），实际 {first}"
        );
        let second = preview_warehouse_code().await.expect("再预览应成功");
        assert_eq!(
            first, second,
            "预览不得推进 biz_seq（点了「生成编码」又取消不能浪费号）"
        );

        let created = create_warehouse(None, warehouse_req(None, "预览后建单仓库"))
            .await
            .expect("留空编码应自动取号");
        assert_eq!(
            created.warehouse_code, first,
            "自动取号必须用掉预览的那个号（预览不占号，取号在保存时）"
        );

        let third = preview_warehouse_code().await.expect("预览应成功");
        assert_ne!(
            third, first,
            "号已被保存用掉，再预览必须给出新的号（{first} → {third}）"
        );

        hard_delete("biz_warehouse", Uuid::parse_str(&created.id).expect("id")).await;
    }

    /// CODE-06 ④：仓库预览号当**手工编码**提交 ⇒ 原样保留；撞号按 `409 + 66001` 兜。
    #[tokio::test]
    async fn warehouse_previewed_code_is_used_verbatim_on_create() {
        let Some(_guard) = setup().await else { return };

        let previewed = preview_warehouse_code().await.expect("仓库编码预览应成功");
        let created = create_warehouse(None, warehouse_req(Some(&previewed), "用预览号建单仓库"))
            .await
            .expect("预览号当手工编码应创建成功");
        assert_eq!(
            created.warehouse_code, previewed,
            "手工提交的编码必须原样保留（不能被再取号覆盖）"
        );

        let after = preview_warehouse_code().await.expect("预览应成功");
        assert_ne!(after, previewed, "预览必须跳过已占用的号");

        let conflict = create_warehouse(None, warehouse_req(Some(&previewed), "预览号重复仓库"))
            .await
            .expect_err("已占用的编码必须被拒");
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(conflict.msg.contains(&previewed), "文案要点名编码：{}", conflict.msg);

        hard_delete("biz_warehouse", Uuid::parse_str(&created.id).expect("id")).await;
    }
}
