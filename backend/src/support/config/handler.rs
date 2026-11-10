//! 系统参数处理器：参数配置的增删改查与按键取值。
//!
//! 处理器承担**参数校验、业务编排与响应组装**，并直接调用框架快捷方法
//! （`find_or_none` / `insert` / `update` / `delete` 等）；需要编写 SQL 的查询
//! （分页、按键读取、唯一性统计）与加密 / 解密领域行为集中在
//! [`crate::support::config::model`]。
//!
//! # 加密存储说明
//!
//! `is_encrypted = true` 时，写入前用 [`nik_utils::Crypto`]（AES-256-GCM）加密
//! `config_value`，读取 / 列表 / 详情 / 按键取值时统一解密后下发；密钥由环境变量
//! [`crate::shared::constants::ENV_CONFIG_SECRET_KEY`] 注入，未配置时加密写入返回
//! 500，绝不静默明文落库。加解密实现集中在 [`crate::support::config::model`]。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;

use std::collections::HashMap;
use std::net::SocketAddr;

use crate::shared::constants::{self, EnableStatus};
use crate::shared::error_code;
use crate::support::config::dto::{ConfigPageQuery, ConfigRes, ConfigSaveReq};
use crate::support::config::model::{self, ConfigFilter, SysConfigModel};
use crate::support::log;
use crate::utils;

// ==================== 接口 ====================

/// 公共参数查询（**免登录**）。
///
/// 仅返回 `config_key` 以 `public.` 开头、且处于启用状态的参数，供登录页 / 门户等
/// 未认证场景读取系统标题、Logo、版权等公开信息；返回体为 `{ 去掉前缀的键: 值 }`
/// 映射（如 `public.system.title` → `system.title`）。敏感与加密参数不会出现在这里。
///
/// # 返回值
///
/// 公共参数键值映射（无公共参数时为空对象）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::no_auth("/support/config/public")]
#[nik::get("/support/config/public")]
pub async fn public() -> ResResult<HashMap<String, String>> {
    // 公共参数是高频匿名访问点：进程内快照命中时零 IO，且免去一次缓存往返
    if let Some(snapshot) = model::runtime_snapshot() {
        return Ok(Res::ok(model::pick_public(&snapshot)));
    }
    // 快照未装载（bootstrap 之前的早期调用）时回退缓存
    if let Some(cached) = model::load_cached_public().await {
        return Ok(Res::ok(cached));
    }

    let configs = model::find_public_configs()
        .await
        .map_err(|e| utils::db_error(e, "查询公共参数"))?;
    let mut map = HashMap::new();
    for config in configs {
        let Some(raw) = config.config_value else {
            continue;
        };
        // 公共参数理论上不加密，这里统一兼容加密情形
        let value = if config.is_encrypted {
            model::decrypt_config_value(&raw)?
        } else {
            raw
        };
        if let Some(key) = config.config_key.strip_prefix(constants::PUBLIC_CONFIG_KEY_PREFIX) {
            map.insert(key.to_string(), value);
        }
    }
    model::cache_public(&map).await;
    Ok(Res::ok(map))
}

/// 分页查询系统参数。
///
/// # 参数
///
/// - `query`：分页与过滤条件（配置键模糊、类型 / 分类精确、状态）。
///
/// # 返回值
///
/// 成功返回系统参数分页数据。
///
/// # 异常情况
///
/// 状态值非法返回 400，数据库异常返回 500。
#[nik::permission(path = "/support/config/page", method = "GET", permission = "support:config:list")]
#[nik::get("/support/config/page")]
pub async fn page(Query(query): Query<ConfigPageQuery>) -> ResResult<PageData<Vec<ConfigRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let status = query.status.map(EnableStatus::from_i16).transpose()?;
    let filter = ConfigFilter {
        config_key: query.config_key,
        config_type: query
            .config_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        category: query
            .category
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        status,
    };

    let result = model::page_configs(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "分页查询系统参数"))?;
    let list = result
        .list
        .iter()
        .map(ConfigRes::from_model)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 系统参数详情。
///
/// # 参数
///
/// - `id`：系统参数 ID。
///
/// # 返回值
///
/// 成功返回系统参数详情。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：系统参数不存在。
#[nik::permission(path = "/support/config/{id}", method = "GET", permission = "support:config:list")]
#[nik::get("/support/config/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<ConfigRes> {
    let id = utils::require_uuid(&id, "id")?;
    let config = SysConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询系统参数详情"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "系统参数不存在"))?;
    Ok(Res::ok(ConfigRes::from_model(&config)?))
}

/// 新建系统参数。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（用于审计字段留痕）。
/// - `req`：系统参数请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`CONFIG_CREATE`，`after_data` 仅记录配置键 / 类型 / 分类
/// 等非敏感元数据，**不含** `config_value`）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回新建系统参数详情。
///
/// # 异常情况
///
/// - 400：参数非法（含「空闲自动退出」键的角色 / 区间校验）；
/// - 409：配置键已存在；
/// - 500：数据库异常。
#[nik::permission(path = "/support/config", method = "POST", permission = "support:config:create")]
#[nik::post("/support/config")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<ConfigSaveReq>,
) -> ResResult<ConfigRes> {
    let config_key = req.config_key.trim();
    // 安全策略键的写入硬约束（0 仅超管 / 上限 60）：新建路径同样要拦，
    // 否则「删掉重建」就绕过了 PUT 上的角色校验。
    check_idle_timeout_write(config_key, req.config_value.as_deref(), &claims).await?;
    if model::count_config_key(config_key, None)
        .await
        .map_err(|e| utils::db_error(e, "校验配置键唯一性"))?
        > 0
    {
        return Err(ApiError::new(409, error_code::CONFIG_KEY_EXISTS, "配置键已存在"));
    }

    let mut config = build_model(&req, None, utils::current_user_uuid(&claims))?;
    config
        .insert(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "新建系统参数"))?;
    model::after_write().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "CONFIG_CREATE",
            "Config",
            config.id.to_string(),
            None,
            // 加密参数无论明文还是密文都不落审计快照，只留下非敏感元数据
            Some(serde_json::json!({
                "config_key": config.config_key,
                "config_type": config.config_type,
                "category": config.category,
            })),
        ),
    );
    Ok(Res::ok(ConfigRes::from_model(&config)?))
}

/// 更新系统参数（全量）。
///
/// # 参数
///
/// - `id`：系统参数 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明。
/// - `req`：系统参数请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`CONFIG_UPDATE`，变更前 / 后快照均仅记录配置键 / 类型 /
/// 分类等非敏感元数据，**不含** `config_value`）；日志写入失败不影响业务结果。
///
/// 「空闲自动退出」参数（[`constants::CONFIG_KEY_IDLE_TIMEOUT`]）是例外：它的值本身
/// 就是安全策略，且用户明确要求「任何修改都写 operation_log（谁 / 什么时候 / 从什么
/// 改到什么 / 是否超管）」，因此该键走 `CONFIG_IDLE_TIMEOUT_UPDATE` 并**记录新旧值**
/// 与是否为超管操作（数值不敏感，记录不会泄漏什么）。
///
/// # 返回值
///
/// 成功返回更新后的系统参数详情。
///
/// # 异常情况
///
/// - 400：ID 或状态值非法；空闲超时参数越界 / 非超管设 0 / 试图改键名；
/// - 403：该参数不允许修改；
/// - 404：系统参数不存在；
/// - 409：配置键与他人重复；
/// - 500：数据库异常。
///
/// # 不接入审批流（用户 2026-11-10）
///
/// 这个接口**直接生效**，不生成审批实例；同时把「0 只能超管设」「上限 60」
/// 写在处理器里硬拦 —— 因此也不存在「走审批流把这个值改成 0」的路径。
#[nik::permission(path = "/support/config/{id}", method = "PUT", permission = "support:config:update")]
#[nik::put("/support/config/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<ConfigSaveReq>,
) -> ResResult<ConfigRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = SysConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询待更新系统参数"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "系统参数不存在"))?;

    if !existing.is_modifiable {
        return Err(ApiError::new(
            403,
            error_code::CONFIG_NOT_MODIFIABLE,
            "该参数不允许修改",
        ));
    }

    let config_key = req.config_key.trim();
    let is_idle_key = existing.config_key == constants::CONFIG_KEY_IDLE_TIMEOUT;
    // 不允许把安全策略键改名：改名后就地变成「普通参数」，0 值校验、审计口径、
    // 前端特殊渲染全部失效（等于用一次改名把功能关掉）。要停用请超管直接设 0。
    if is_idle_key && config_key != constants::CONFIG_KEY_IDLE_TIMEOUT {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "自动退出登录参数的键名不允许修改",
        ));
    }
    // 硬约束：0 仅超管、上限 60 分钟（对所有角色生效）
    check_idle_timeout_write(config_key, req.config_value.as_deref(), &claims).await?;
    if config_key != existing.config_key
        && model::count_config_key(config_key, Some(&id))
            .await
            .map_err(|e| utils::db_error(e, "校验配置键唯一性"))?
            > 0
    {
        return Err(ApiError::new(409, error_code::CONFIG_KEY_EXISTS, "配置键已存在"));
    }

    // 保留创建审计字段，仅更新业务字段
    let mut to_save = build_model(&req, Some(id), utils::current_user_uuid(&claims))?;
    to_save.created_at = existing.created_at;
    to_save.create_by = existing.create_by;
    to_save
        .update(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "更新系统参数"))?;
    model::after_write().await;

    // 空闲超时参数：审计要能回答「谁在什么时候把自动退出从 X 改成 Y、他是不是超管」，
    // 因此单独带值留痕（普通参数仍按老口径不落 config_value）。
    let (operation, before_data, after_data) = if is_idle_key {
        let is_super = caller_is_super_admin(&claims).await?;
        (
            "CONFIG_IDLE_TIMEOUT_UPDATE",
            serde_json::json!({
                "config_key": existing.config_key,
                "config_value": existing.config_value,
                "operator_is_super_admin": is_super,
            }),
            serde_json::json!({
                "config_key": to_save.config_key,
                "config_value": to_save.config_value,
                "operator_is_super_admin": is_super,
            }),
        )
    } else {
        (
            "CONFIG_UPDATE",
            serde_json::json!({
                "config_key": existing.config_key,
                "config_type": existing.config_type,
                "category": existing.category,
            }),
            // 变更后的 config_value 亦不记录，避免加密密钥或敏感配置外泄
            serde_json::json!({
                "config_key": to_save.config_key,
                "config_type": to_save.config_type,
                "category": to_save.category,
            }),
        )
    };
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            operation,
            "Config",
            to_save.id.to_string(),
            Some(before_data),
            Some(after_data),
        ),
    );
    Ok(Res::ok(ConfigRes::from_model(&to_save)?))
}

/// 删除系统参数。
///
/// # 参数
///
/// - `id`：系统参数 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`CONFIG_DELETE`，`before_data` 仅记录被删参数的配置键 /
/// 类型 / 分类等非敏感元数据，**不含** `config_value`）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：系统参数不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/config/{id}",
    method = "DELETE",
    permission = "support:config:delete"
)]
#[nik::delete("/support/config/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let Some(existing) = SysConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|e| utils::db_error(e, "查询待删除系统参数"))?
    else {
        return Err(ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "系统参数不存在"));
    };

    // 安全策略键不允许删除：删掉后读取侧会回退默认 15 分钟（不算关闭功能），
    // 但「先删再建」会绕过 PUT 上的角色校验与带值审计。要停用只能超管显式设 0。
    if existing.config_key == constants::CONFIG_KEY_IDLE_TIMEOUT {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "自动退出登录参数不允许删除（如需关闭自动退出，请超级管理员把值改为 0）",
        ));
    }

    SysConfigModel::of(id)
        .delete(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "删除系统参数"))?;
    model::after_write().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "CONFIG_DELETE",
            "Config",
            id.to_string(),
            Some(serde_json::json!({
                "config_key": existing.config_key,
                "config_type": existing.config_type,
                "category": existing.category,
            })),
            None,
        ),
    );
    Ok(Res::ok_empty())
}

/// 按配置键查询启用配置的值。
///
/// 配置键不存在或对应配置已禁用时返回 `null`，不视为错误。
///
/// # 参数
///
/// - `config_key`：配置键。
///
/// # 返回值
///
/// 成功返回配置值（不存在时为 `null`）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(
    path = "/support/config/value/{config_key}",
    method = "GET",
    permission = "support:config:list"
)]
#[nik::get("/support/config/value/{config_key}")]
pub async fn value(Path(config_key): Path<String>) -> ResResult<Option<String>> {
    // 进程内快照命中时零 IO：这是参数读取得最多的入口（前端多处直接取值）
    if let Some(snapshot) = model::runtime_snapshot() {
        return Ok(Res::ok(snapshot.get(&config_key).cloned()));
    }
    // 按键取值是高频热点，优先读缓存
    if let Some(cached) = model::load_cached_value(&config_key).await {
        return Ok(Res::ok(cached));
    }
    let config = model::find_enabled_config(&config_key)
        .await
        .map_err(|e| utils::db_error(e, "按配置键查询值"))?;
    let value = match config {
        Some(c) => match c.config_value {
            // 加密参数解密后返回明文
            Some(raw) if c.is_encrypted => Some(model::decrypt_config_value(&raw)?),
            other => other,
        },
        None => None,
    };
    model::cache_value(&config_key, &value).await;
    Ok(Res::ok(value))
}

/// 重新装载运行时参数快照（改配置后不重启即可生效）。
///
/// 常规写操作（新建 / 更新 / 删除参数）已自动刷新快照，本接口用于**进程外**变更：
/// 多实例部署时改了 A 实例、B 实例需要立即跟随，或直接改数据库 / 迁移脚本后
/// 强制本实例重新读取。刷新后会广播给所有监听器（如 IP 归属地解析开关）。
///
/// # 返回值
///
/// 成功返回重载后快照中的参数键列表（升序）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(
    path = "/support/config/reload",
    method = "POST",
    permission = "support:config:update"
)]
#[nik::post("/support/config/reload")]
pub async fn reload() -> ResResult<Vec<String>> {
    model::reload_runtime_snapshot()
        .await
        .map_err(|e| utils::db_error(e, "重载运行时参数快照"))?;
    // 快照与 Redis 缓存同源失效，避免 reload 后按键取值接口仍读到旧缓存
    model::invalidate_config_caches().await;
    // 本实例已是最新：对齐版本基线，避免监听任务随后重复重载一次
    model::sync_version_baseline().await;
    Ok(Res::ok(model::runtime_snapshot_keys()))
}

// ==================== 枚举下发 ====================

/// 本模块枚举表：参数值类型。
///
/// # 装配
///
/// 与列表同权限（`support:config:list`）。`config_type` 取值以
/// [`crate::support::config::model::config_type`] 白名单为唯一口径，写入侧
/// [`crate::support::config::model::is_valid_config_type`] 拒绝越界值；前端
/// 「值类型」下拉与按类型渲染的编辑控件统一取自本接口。
///
/// # 返回值
///
/// 统一响应体，`data` 为「枚举名 → 枚举项数组」，结构见 [`crate::shared::enums`]。
#[nik::permission(path = "/support/config/enums", method = "GET", permission = "support:config:list")]
#[nik::get("/support/config/enums")]
pub async fn enums() -> Res<crate::shared::enums::EnumsRes> {
    Res::ok(crate::support::config::model::enums())
}

// ==================== 公共辅助 ====================

/// 「空闲自动退出」参数的写入硬约束（用户 2026-11-10 第二次修正）。
///
/// # 参数
///
/// - `config_key`：本次写入的配置键；不是 [`constants::CONFIG_KEY_IDLE_TIMEOUT`] 时直接放行。
/// - `raw`：写入的文本值（`config_value`）。
/// - `claims`：调用者令牌声明（据此回库取角色）。
///
/// # 返回值
///
/// 通过返回 `Ok(())`；违反约束返回 400 [`ApiError`]，消息直接给用户看。
///
/// # 三条硬约束
///
/// 1. **0（禁用）只有 `SUPER_ADMIN` 能设** —— 其他角色一律 400，不管有没有
///    `support:config:update` 权限。用户原话：「0=禁用不是谁都能设，除了超管，
///    其他人都不允许设置为 0」。否则第一个嫌弹窗烦的管理员就会把它关掉，
///    整个功能形同虚设。
/// 2. **上限 60 分钟** —— 超管也受此限制（70 / 120 / 1440 全部拒绝）。把超时设成
///    一天等于换个方式关掉功能。
/// 3. **不走审批流** —— 校验在处理器里硬拦，不看任何审批实例；这个键也不进流程引擎，
///    所以不存在「发起申请 → 审批通过 → 生效」的路径。
///
/// # 校验顺序
///
/// 先做纯值校验（整数 / 区间），只有值为 0 时才回库查角色：越界值和脏值不该
/// 额外产生一次数据库查询。
pub(crate) async fn check_idle_timeout_write(
    config_key: &str,
    raw: Option<&str>,
    claims: &TokenClaims,
) -> std::result::Result<(), ApiError> {
    if config_key != constants::CONFIG_KEY_IDLE_TIMEOUT {
        return Ok(());
    }
    let value = parse_idle_timeout_value(raw)?;
    validate_idle_timeout_range(value)?;
    if value == 0 && !caller_is_super_admin(claims).await? {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "只有超级管理员可以禁用自动退出登录",
        ));
    }
    Ok(())
}

/// 解析「空闲自动退出」参数的文本值。
///
/// # 参数
///
/// - `raw`：`config_value`（可能为 `None` / 空串 / 非数字）。
///
/// # 返回值
///
/// 解析成功返回整数；空值或非整数返回 400 [`ApiError`]。
pub(crate) fn parse_idle_timeout_value(raw: Option<&str>) -> std::result::Result<i64, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "自动退出时间不能为空（1-60 分钟，0=禁用仅超管可设）",
        ));
    }
    text.parse::<i64>().map_err(|_| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "自动退出时间必须是整数分钟（1-60，0=禁用仅超管可设）",
        )
    })
}

/// 校验取值区间（纯函数，便于单测；0 的角色约束由调用方另行判定）。
///
/// # 参数
///
/// - `value`：解析后的分钟数。
///
/// # 返回值
///
/// `0` 或 `1..=60` 通过；负数与 `>60` 返回 400。
pub(crate) fn validate_idle_timeout_range(value: i64) -> std::result::Result<(), ApiError> {
    if value == 0 || (constants::MIN_IDLE_TIMEOUT_MINUTES..=constants::MAX_IDLE_TIMEOUT_MINUTES).contains(&value) {
        return Ok(());
    }
    Err(idle_range_error())
}

/// 越界 / 负数的统一错误（所有角色、含超管）。
fn idle_range_error() -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        "自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）",
    )
}

/// 调用者是否持有 `SUPER_ADMIN` 角色。
///
/// # 为什么回库而不是看令牌快照
///
/// 令牌里的 `roles` / `permissions` 是签发那一刻的快照。会话守卫虽然已经用库里的
/// 角色复核过通配权限（BUG-SYS-04），但「能不能关闭自动退出」是更高一级的授权，
/// 值得再点查一次角色表 —— 代价只是一次索引查询，且这个键一年改不了几次。
///
/// # 参数
///
/// - `claims`：调用者令牌声明。
///
/// # 返回值
///
/// 是超管返回 `true`；取不到用户 ID 或查库失败返回错误（fail-closed：
/// 查不出角色就当不是超管，宁可拒绝也不要放行）。
pub(crate) async fn caller_is_super_admin(claims: &TokenClaims) -> std::result::Result<bool, ApiError> {
    let Some(user_id) = utils::current_user_uuid(claims) else {
        return Ok(false);
    };
    let role_codes = crate::system::user::model::load_role_codes(&user_id)
        .await
        .map_err(|e| utils::db_error(e, "校验操作者角色"))?;
    Ok(role_codes.iter().any(|code| code == constants::SUPER_ADMIN_ROLE_CODE))
}

/// 由系统参数请求构造模型字段（新建 / 更新共用，不访问数据库）。
///
/// # 参数
///
/// - `req`：系统参数请求体。
/// - `id`：主键（更新时为既有 ID，新建时为 `None`）。
/// - `operator`：当前操作人 ID。
///
/// # 返回值
///
/// 可直接写入的系统参数模型；`config_type` 缺省 `STRING`，`config_level` 缺省 2。
///
/// # 异常情况
///
/// 状态值非 0 / 1 返回 400。
fn build_model(
    req: &ConfigSaveReq,
    id: Option<uuid::Uuid>,
    operator: Option<uuid::Uuid>,
) -> std::result::Result<SysConfigModel, ApiError> {
    let status = match req.status {
        Some(value) => EnableStatus::from_i16(value)?,
        None => EnableStatus::enabled,
    };
    let config_type = {
        let trimmed = req.config_type.trim();
        // 空值按「字符串」兜底；非空值必须落在白名单内——`config_type` 决定前端
        // 渲染哪种编辑控件与读取侧的解析方式，脏值会静默产生不可编辑的参数
        if trimmed.is_empty() {
            String::from(model::config_type::STRING)
        } else if !model::is_valid_config_type(trimmed) {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "值类型只能为 STRING / NUMBER / BOOLEAN / JSON",
            ));
        } else {
            trimmed.to_string()
        }
    };
    let config_level = i16::try_from(req.config_level.unwrap_or(2)).unwrap_or(2);
    let is_encrypted = req.is_encrypted.unwrap_or(false);
    // 标记加密的参数在写入前加密，避免明文落库
    let config_value = match req.config_value.as_deref().map(str::trim).map(str::to_string) {
        Some(plain) if is_encrypted => Some(model::encrypt_config_value(&plain)?),
        other => other,
    };
    let now = chrono::Utc::now();
    Ok(SysConfigModel {
        id: id.unwrap_or_else(uuid::Uuid::now_v7),
        config_key: req.config_key.trim().to_string(),
        config_value,
        config_type,
        description: req.description.as_deref().map(str::trim).map(str::to_string),
        category: req.category.as_deref().map(str::trim).map(str::to_string),
        sort_order: req.sort_order.unwrap_or(0),
        config_level,
        is_modifiable: req.is_modifiable.unwrap_or(true),
        is_encrypted,
        is_restart: req.is_restart.unwrap_or(false),
        status,
        ref_value: req.ref_value.as_deref().map(str::trim).map(str::to_string),
        select_value: req.select_value.as_deref().map(str::trim).map(str::to_string),
        created_at: now,
        updated_at: now,
        create_by: operator,
        update_by: operator,
        deleted_at: None,
    })
}

#[cfg(test)]
mod tests {
    //! 「空闲自动退出」参数写入校验的单测。
    //!
    //! 只测**纯函数**（解析 / 区间）：角色判定要查库，属于真库用例范畴，由
    //! `ERP_DB_URL` 开关下的集成实测覆盖（卡内附 curl 证据）。这样 `cargo test`
    //! 在没有数据库的环境里也能全绿。
    use super::*;

    /// 非本键一律放行解析（普通参数不受空闲超时约束影响）。
    #[test]
    fn parses_plain_integers() {
        assert_eq!(parse_idle_timeout_value(Some("15")).unwrap(), 15);
        assert_eq!(parse_idle_timeout_value(Some(" 60 ")).unwrap(), 60);
        assert_eq!(parse_idle_timeout_value(Some("0")).unwrap(), 0);
    }

    /// 空值 / 非数字一律 400（卡面验收 ③：abc 必须拒绝）。
    #[test]
    fn rejects_empty_and_non_numeric() {
        for raw in [None, Some(""), Some("   "), Some("abc"), Some("1.5"), Some("15m")] {
            let err = parse_idle_timeout_value(raw).expect_err("应拒绝");
            assert_eq!(err.status, 400, "raw={raw:?}");
            assert_eq!(err.code, error_code::PARAM_INVALID);
        }
    }

    /// 区间：0 与 1..=60 合法；负数与 >60 拒绝（超管同样受上限约束）。
    #[test]
    fn range_boundaries() {
        for value in [0, 1, 15, 30, 60] {
            assert!(validate_idle_timeout_range(value).is_ok(), "value={value}");
        }
        for value in [-1, -60, 61, 70, 120, 1440, i64::MAX] {
            let err = validate_idle_timeout_range(value).expect_err("应拒绝");
            assert_eq!(err.status, 400, "value={value}");
            assert!(err.msg.contains("1-60"), "错误文案要写清区间：{}", err.msg);
        }
    }

    /// 上限就是 `constants::MAX_IDLE_TIMEOUT_MINUTES`（60），常量被改大时这条会红。
    #[test]
    fn max_timeout_is_one_hour() {
        assert_eq!(constants::MAX_IDLE_TIMEOUT_MINUTES, 60);
        assert_eq!(constants::MIN_IDLE_TIMEOUT_MINUTES, 1);
        assert_eq!(constants::DEFAULT_IDLE_TIMEOUT_MINUTES, 15);
        assert!(validate_idle_timeout_range(61).is_err(), "61 分钟必须拒绝");
    }
}
