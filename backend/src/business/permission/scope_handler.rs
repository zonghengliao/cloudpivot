//! 数据权限配置与自检接口（批次 B · B4）。
//!
//! # 接口清单（前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/permission/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/permission/scope/role/{role_id}` | `business:permission:scope:read` | 查角色范围配置（含自定义部门） |
//! | PUT | `/permission/scope/role/{role_id}` | `business:permission:scope:write` | 设范围（1..5）+ 自定义部门集合 |
//! | GET | `/permission/scope/dept-tree` | `business:permission:scope:read` | 自定义部门树（**按操作者范围裁剪**） |
//! | GET | `/permission/scope/self-check` | `business:permission:scope:self-check` | 当前用户的解析结果 |
//! | GET | `/permission/scope/uncovered-tables` | `business:permission:scope:self-check` | 已登记但未接入的表 |
//!
//! # 三条设计约束（都是「不这样做就出事」的约束，不是风格问题）
//!
//! 1. **部门树必须按操作者范围裁剪**：部门编码 / 名称本身是组织信息。返回全量树
//!    等于告诉一个只能看本部门的人「公司有几个部门、都叫什么」（架构 §2.5 `sys_dept` 行）。
//!    裁剪逻辑在 [`scope_model::find_visible_depts`] + [`scope_model::crop_to_visible`]，
//!    本文件不允许出现「查全部部门再过滤」的写法。
//! 2. **不可授出超过自身可见范围**：`PUT` 先过
//!    [`crate::business::shared::guard::ensure_scope_grant_allowed`]（集合包含关系，
//!    B2 的单一权威），再过 [`scope_model::ensure_kind_grantable`]（范围**形态**的上限：
//!    「仅本人」的管理员不能把角色设成「全部」——集合校验对空集合恒通过，这条是补漏）。
//! 3. **`data_scope=5` 必须带非空集合**：配错不能静默降级（B2 的 S10 在解析侧降级为
//!    「仅本人」是运行期兜底；配置侧必须当场拒绝，见 `scope_model::validate_custom_scope`）。
//!
//! # 缓存失效
//!
//! 范围配置改动后，必须删掉**该角色所有持有者**的数据范围缓存（架构 §2.8 失效矩阵）。
//! 反查持有者用 `sys_user_role` 的 `SELECT DISTINCT user_id`，逐个 `DEL`——不用
//! `delete_pattern`（键里带用户 ID，模式匹配会误删别人的键）。

use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;

use crate::business::permission::dto::{
    DeptTreeRes, RoleScopeRes, SelfCheckRes, SetRoleScopeReq, SetRoleScopeRes, UncoveredTablesRes,
};
use crate::business::permission::scope_model;
use crate::business::shared::guard;
use crate::business::shared::principal::{self, cache_stats, current_epoch};
use crate::business::shared::scope::{SCOPED_TABLES, ScopeKind, filter_for};
use crate::shared::constants;
use crate::shared::error_code;
use crate::utils;

// ==================== 查角色范围配置 ====================

/// 查某个角色的数据范围配置。
///
/// # 参数
///
/// - `role_id`：角色 ID（路径参数）。
///
/// # 返回值
///
/// [`RoleScopeRes`]：范围下标 + 稳定编码 + 中文标签 + 自定义部门明细 + 持有者数量。
///
/// # 异常情况
///
/// - 400 + `10001`：`role_id` 不是合法 UUID；
/// - 404 + `10002`：角色不存在或已软删；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/permission/scope/role/{role_id}",
    method = "GET",
    permission = "business:permission:scope:read"
)]
#[alun::get("/business/permission/scope/role/{role_id}")]
pub async fn role_scope_detail(Path(role_id): Path<String>) -> ResResult<RoleScopeRes> {
    let role_id = utils::require_uuid(&role_id, "role_id")?;
    let role = require_role(&role_id).await?;
    let custom = scope_model::find_custom_depts(&role_id)
        .await
        .map_err(|error| utils::db_error(error, "查询角色的自定义部门"))?;
    let holders = scope_model::count_role_holders(&role_id)
        .await
        .map_err(|error| utils::db_error(error, "统计角色持有者"))?;
    Ok(Res::ok(scope_model::role_scope_res(
        &role, &custom, holders,
    )))
}

// ==================== 设角色范围配置 ====================

/// 设置某个角色的数据范围（含自定义部门集合）。
///
/// # 参数
///
/// - `role_id`：角色 ID（路径参数）；
/// - `claims`：当前用户（范围上限校验与审计都用**令牌里的**用户，不信任请求体）；
/// - `req`：`{ "data_scope": 1..5, "custom_dept_ids": null | [] | ["…"] }`。
///
/// # 返回值
///
/// [`SetRoleScopeRes`]：生效后的配置 + 本次被清缓存的用户数。
///
/// # 异常情况
///
/// - 400 + `10001`：ID / UUID 格式非法；
/// - 400 + `61001`：`data_scope` 不在 1..5；`data_scope=5` 但集合为空；
///   `data_scope≠5` 却提交了非空集合；
/// - 403 + `61003`：试图修改**自己持有**角色的数据范围（防自我扩权）；
/// - 403 + `61002`：新范围超出操作者自身可见范围（消息里点名越界部门名称 + ID）；
/// - 404 + `10002`：角色不存在；404 + `61004`：集合里有不存在 / 已软删的部门；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/permission/scope/role/{role_id}",
    method = "PUT",
    permission = "business:permission:scope:write"
)]
#[alun::put("/business/permission/scope/role/{role_id}")]
pub async fn role_scope_update(
    Path(role_id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SetRoleScopeReq>,
) -> ResResult<SetRoleScopeRes> {
    let role_id = utils::require_uuid(&role_id, "role_id")?;
    let operator_id = current_user_id(&claims)?;
    let role = require_role(&role_id).await?;

    // ① 取值与集合校验（纯逻辑，61001）
    let kind = ScopeKind::from_i16(req.data_scope)?;
    let requested: Option<Vec<Uuid>> = match &req.custom_dept_ids {
        None => None,
        Some(list) => {
            let mut ids = Vec::with_capacity(list.len());
            for raw in list {
                ids.push(utils::require_uuid(raw.trim(), "custom_dept_ids")?);
            }
            Some(ids)
        }
    };
    let custom = scope_model::validate_custom_scope(kind, requested.as_deref())?;

    // ② 部门存在性（404 + 61004）：不存在 / 已软删的部门不许写进授权表
    if let Some(ids) = custom.as_deref().filter(|ids| !ids.is_empty()) {
        let existing = scope_model::find_depts(ids)
            .await
            .map_err(|error| utils::db_error(error, "校验自定义部门"))?;
        let existing_ids: Vec<Uuid> = existing.iter().map(|row| row.id).collect();
        let missing = scope_model::missing_dept_ids(ids, &existing_ids);
        if !missing.is_empty() {
            return Err(ApiError::new(
                404,
                error_code::DATA_SCOPE_DEPT_NOT_FOUND,
                format!(
                    "以下部门不存在或已删除，不能加入自定义范围：{}",
                    missing
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            ));
        }
    }

    // ③ 操作者自身范围（后续三条校验的唯一依据）
    let operator = principal::resolve(&operator_id)
        .await
        .map_err(|error| utils::db_error(error, "解析操作者的数据范围"))?;

    // ④ 不能改自己持有角色的范围（403 + 61003；超管不受限，架构 §2.7）
    if !operator.is_super()
        && scope_model::user_holds_role(&operator_id, &role_id)
            .await
            .map_err(|error| utils::db_error(error, "查询操作者是否持有该角色"))?
    {
        return Err(ApiError::new(
            403,
            error_code::DATA_SCOPE_SELF_ROLE_LOCKED,
            format!(
                "不能修改自己持有角色「{}」的数据范围（避免自我扩权）",
                role.role_name
            ),
        ));
    }

    // ⑤ 范围形态上限（403 + 61002）
    scope_model::ensure_kind_grantable(&operator, kind)?;

    // ⑥ 部门集合上限（403 + 61002；B2 的单一权威，本处只把消息补成「名称（ID）」）
    if let Some(ids) = custom.as_deref()
        && let Err(error) = guard::ensure_scope_grant_allowed(&operator, ids)
    {
        let exceeded = scope_model::exceeded_dept_ids(&operator, ids);
        let named = scope_model::find_depts(&exceeded).await.unwrap_or_default();
        return Err(ApiError::new(
            error.status,
            error.code,
            format!(
                "新数据范围超出你自身的可见部门范围，越界部门：{}",
                scope_model::describe_depts(&exceeded, &named)
            ),
        ));
    }

    // ⑦ 落库（同一事务）+ 缓存失效
    let affected = scope_model::save_role_scope(&role_id, kind, custom.as_deref(), &operator_id)
        .await
        .map_err(|error| utils::db_error(error, "保存数据范围"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "角色不存在或已被删除",
        ));
    }
    let holders = scope_model::find_role_holder_ids(&role_id)
        .await
        .map_err(|error| utils::db_error(error, "反查角色持有者"))?;
    guard::invalidate_user_scopes(&holders).await;

    let custom_rows = scope_model::find_custom_depts(&role_id)
        .await
        .map_err(|error| utils::db_error(error, "回读自定义部门"))?;
    tracing::info!(
        role_id = %role_id,
        role_code = %role.role_code,
        data_scope = kind.as_i16(),
        custom_depts = custom_rows.len(),
        invalidated_users = holders.len(),
        operator = %operator_id,
        "数据范围配置已更新（含持有者缓存失效）"
    );
    Ok(Res::ok(SetRoleScopeRes {
        role_id: role_id.to_string(),
        data_scope: kind.as_i16(),
        data_scope_code: kind.code().to_string(),
        data_scope_label: kind.label().to_string(),
        custom_dept_ids: custom_rows.iter().map(|row| row.id.to_string()).collect(),
        custom_depts: scope_model::to_briefs(&custom_rows),
        invalidated_users: i64::try_from(holders.len()).unwrap_or(i64::MAX),
    }))
}

// ==================== 自定义部门树（按操作者范围裁剪） ====================

/// 供前端选择自定义部门用的部门树。
///
/// # 参数
///
/// - `claims`：当前用户（裁剪依据）。
///
/// # 返回值
///
/// [`DeptTreeRes`]：**只含操作者可见部门**的树 + 节点数（验收时用节点数对比两个账号）。
///
/// # 异常情况
///
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/permission/scope/dept-tree",
    method = "GET",
    permission = "business:permission:scope:read"
)]
#[alun::get("/business/permission/scope/dept-tree")]
pub async fn dept_tree(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<DeptTreeRes> {
    let user_id = current_user_id(&claims)?;
    let operator = principal::resolve(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "解析操作者的数据范围"))?;

    let rows = scope_model::find_visible_depts(&operator)
        .await
        .map_err(|error| utils::db_error(error, "查询可见部门"))?;
    let visible: std::collections::BTreeSet<Uuid> = rows.iter().map(|row| row.id).collect();
    let cropped = scope_model::crop_to_visible(rows, &visible);
    let tree = scope_model::build_tree(cropped);
    let total = scope_model::count_nodes(&tree);
    let root_count = tree.len();
    let scope_limited = !(operator.is_super() || operator.kind() == ScopeKind::All);
    tracing::debug!(
        %user_id,
        scope = operator.kind().code(),
        total,
        root_count,
        scope_limited,
        "返回按操作者范围裁剪后的部门树"
    );
    Ok(Res::ok(DeptTreeRes {
        total,
        root_count,
        scope_limited,
        visible_dept_ids: visible.iter().map(ToString::to_string).collect(),
        tree,
    }))
}

// ==================== 自检：当前用户的解析结果 ====================

/// 当前用户的数据权限自检。
///
/// 回答三个问题：**我是什么范围**、**我能看到几个部门**、**这是哪个角色给的**；
/// 外加各登记表最终注入的片段原文（不含参数值）。
///
/// # 参数
///
/// - `claims`：当前用户。
///
/// # 返回值
///
/// [`SelfCheckRes`]。
///
/// # 异常情况
///
/// - 401 + `20005`：令牌里没有合法用户 ID（会话与库不一致）；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/permission/scope/self-check",
    method = "GET",
    permission = "business:permission:scope:self-check"
)]
#[alun::get("/business/permission/scope/self-check")]
pub async fn self_check(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<SelfCheckRes> {
    let user_id = current_user_id(&claims)?;
    let operator = principal::resolve(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "解析当前用户的数据范围"))?;
    let brief = scope_model::find_user_brief(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询用户信息"))?
        .ok_or_else(|| {
            ApiError::new(
                401,
                error_code::AUTH_TOKEN_INVALID,
                "当前令牌对应的用户不存在，请重新登录",
            )
        })?;
    let roles = scope_model::find_user_roles(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询用户角色"))?;

    // 可见部门明细：与 dept-tree 同源（同一个裁剪入口），保证「树里看到的」与
    // 「自检说能看到的」永远一致
    let visible_rows = scope_model::find_visible_depts(&operator)
        .await
        .map_err(|error| utils::db_error(error, "查询可见部门"))?;
    let dept_ids: Vec<Uuid> = visible_rows.iter().map(|row| row.id).collect();

    let tables = SCOPED_TABLES
        .iter()
        .map(|target| {
            let filter = filter_for(&operator, *target);
            crate::business::permission::dto::TableFilter {
                table: target.table.to_string(),
                alias: target.alias.to_string(),
                where_sql: filter.sql().to_string(),
                binds: filter.bind_count(),
                is_noop: filter.is_noop(),
                degraded: filter.is_degraded(),
            }
        })
        .collect();

    let stats = cache_stats();
    Ok(Res::ok(SelfCheckRes {
        user_id: user_id.to_string(),
        username: brief.username,
        real_name: brief.real_name,
        is_super_admin: operator.is_super(),
        own_dept_id: brief.dept_id.map(|id| id.to_string()),
        own_dept_name: brief.dept_name,
        scope: scope_model::scope_summary(operator.kind()),
        dept_count: dept_ids.len(),
        dept_ids: dept_ids.iter().map(ToString::to_string).collect(),
        depts: scope_model::to_briefs(&visible_rows),
        self_only: operator.self_only(),
        roles: roles
            .iter()
            .map(scope_model::RoleScope::to_source_role)
            .collect(),
        tables,
        cache: crate::business::permission::dto::CacheInfo {
            hits: stats.hits,
            misses: stats.misses,
            db_loads: stats.db_loads,
            cache_unavailable: stats.cache_unavailable,
            epoch: current_epoch().await,
            ttl_secs: constants::CACHE_PRINCIPAL_TTL_SECS,
        },
    }))
}

// ==================== 自检：未接入的登记表 ====================

/// 列出**已登记但未接入**数据权限的表。
///
/// # 为什么需要这个接口
///
/// 架构 §2.4 的闸② 是构建期 grep 门禁（查 `FROM <表>` 的文件里有没有 `scope::`），
/// 运维 / 实施在现场没有构建环境。这里给出运行时镜像：`SCOPED_TABLES`（登记表）×
/// `scope_model::WIRED_TABLES`（已接入声明）的差集。**`wired` 只能由接入方在
/// 代码里声明**——它不是"根据 SQL 猜出来的"，避免自检本身成为新的误报源。
///
/// # 返回值
///
/// [`UncoveredTablesRes`]。
///
/// # 异常情况
///
/// 无（纯内存计算）。
#[alun::permission(
    path = "/business/permission/scope/uncovered-tables",
    method = "GET",
    permission = "business:permission:scope:self-check"
)]
#[alun::get("/business/permission/scope/uncovered-tables")]
pub async fn uncovered_tables() -> ResResult<UncoveredTablesRes> {
    let tables = scope_model::table_coverage();
    let wired = tables.iter().filter(|item| item.wired).count();
    let uncovered: Vec<String> = tables
        .iter()
        .filter(|item| !item.wired)
        .map(|item| item.table.clone())
        .collect();
    Ok(Res::ok(UncoveredTablesRes {
        registered: tables.len(),
        wired,
        uncovered: uncovered.len(),
        uncovered_tables: uncovered,
        tables,
    }))
}

// ==================== 公共辅助 ====================

/// 取令牌里的用户 ID；缺失 / 非法 → `401 + 20005`。
fn current_user_id(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少有效的用户标识，请重新登录",
        )
    })
}

/// 读角色；不存在 / 已软删 → `404 + 10002`。
async fn require_role(role_id: &Uuid) -> std::result::Result<scope_model::RoleScope, ApiError> {
    scope_model::find_role(role_id)
        .await
        .map_err(|error| utils::db_error(error, "查询角色"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("角色不存在或已删除：{role_id}"),
            )
        })
}
