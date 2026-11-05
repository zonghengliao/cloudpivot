//! 认证模块的数据访问与领域逻辑。
//!
//! 按项目分层约定：**需要编写 SQL 的查询与领域行为集中在本文件**；仅调用框架
//! 快捷方法的简单读写（如按用户名 / 主键查用户、`of(...).set(...).update` 解锁）
//! 由 `handler.rs` 直接执行。认证过程读写的 `sys_user` / `sys_role` /
//! `sys_user_role` / `sys_menu` / `sys_role_menu` / `sys_config` 均复用各自模块的
//! 强类型模型，不重复定义表结构。

use nik::prelude::*;

use crate::shared::constants::{self, EnableStatus};
use crate::support::config::model::SysConfigModel;
use crate::system::menu::model::SysMenuModel;
use crate::system::role::model::SysRoleModel;
use crate::system::user::model::SysUserModel;

// ==================== 角色 / 权限 / 菜单 ====================

/// 用户角色列表缓存键。
///
/// # 参数
///
/// - `user_id`：用户 ID。
fn cache_key_user_roles(user_id: &uuid::Uuid) -> String {
    format!("{}{}", constants::CACHE_KEY_PREFIX_USER_ROLES, user_id)
}

/// 用户权限码列表缓存键。
fn cache_key_user_perms(user_id: &uuid::Uuid) -> String {
    format!("{}{}", constants::CACHE_KEY_PREFIX_USER_PERMS, user_id)
}

/// 用户可见菜单列表缓存键。
fn cache_key_user_menus(user_id: &uuid::Uuid) -> String {
    format!("{}{}", constants::CACHE_KEY_PREFIX_USER_MENUS, user_id)
}

/// 用户的三个主体上下文缓存键（角色 / 权限码 / 可见菜单）。
///
/// 抽成数组是为了让「按用户清理」与「按前缀批量清理」共用同一份键定义 ——
/// 少写一个键就是「漏失效 ⇒ 授权后仍然 403」，必须只有一处真值源。
///
/// # 参数
///
/// - `user_id`：用户 ID。
fn principal_keys(user_id: &uuid::Uuid) -> [String; 3] {
    [
        cache_key_user_roles(user_id),
        cache_key_user_perms(user_id),
        cache_key_user_menus(user_id),
    ]
}

/// [`principal_keys`] 的只读别名：单测在兄弟模块里，需要能直接断言「三个键都清到了」。
#[cfg(test)]
pub(crate) fn principal_keys_for(user_id: &uuid::Uuid) -> [String; 3] {
    principal_keys(user_id)
}

/// 主体上下文缓存的三条通配模式（批量兜底清理用）。
///
/// 只在「反查持有者失败」或「无法枚举持有者」时使用：`delete_pattern` 在 Redis 上是
/// `KEYS` 语义的重操作，键空间却只有最近 300 s 活跃过的用户，低频管理动作可以承受。
///
/// # 返回值
///
/// 三个「前缀 + `*`」模式，与 [`principal_keys`] 的前缀一一对应。
pub(crate) fn principal_patterns() -> [String; 3] {
    [
        format!("{}*", constants::CACHE_KEY_PREFIX_USER_ROLES),
        format!("{}*", constants::CACHE_KEY_PREFIX_USER_PERMS),
        format!("{}*", constants::CACHE_KEY_PREFIX_USER_MENUS),
    ]
}

/// 缓存里承载的「用户角色精简投影」。
///
/// `SysRoleModel` 未派生 serde（枚举字段仅实现 `TypeMapper`），不宜直接入缓存；
/// 而登录路径实际只用到 `role_code` 与 `status`（`role_codes` 就靠这两个字段），
/// 故用一个最小投影替代，既满足 serde 约束又避免把整个模型塞进缓存。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedRole {
    /// 角色编码。
    role_code: String,
    /// 是否启用（`role_codes` 仅保留启用角色）。
    enabled: bool,
}

impl CachedRole {
    /// 由角色模型投影。
    fn from_model(role: &SysRoleModel) -> Self {
        Self {
            role_code: role.role_code.clone(),
            enabled: role.status == EnableStatus::enabled,
        }
    }
}

/// 读取缓存值；缓存不可用或未命中返回 `None`（调用方回退查库）。
async fn cache_get<T>(key: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned + Send,
{
    let store = try_cache()?;
    Cache::get(store, key).await.ok().flatten()
}

/// 写入缓存；失败只告警，不影响主流程（缓存非主数据源）。
async fn cache_set<T>(key: &str, value: &T)
where
    T: serde::Serialize + Send + Sync + ?Sized,
{
    let Some(store) = try_cache() else {
        return;
    };
    if let Err(e) = Cache::set_ex(store, key, value, constants::CACHE_PRINCIPAL_TTL_SECS).await {
        tracing::warn!(key, error = %e, "写入主体上下文缓存失败");
    }
}

/// 失效指定用户的角色 / 权限 / 菜单三项缓存。
///
/// 授权类变更（用户角色分配、角色菜单授权、角色启停、菜单增删改）后调用。
/// 缓存未初始化时直接跳过（无缓存即无脏读）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
pub(crate) async fn invalidate_user_principal(user_id: &uuid::Uuid) {
    let Some(store) = try_cache() else {
        return;
    };
    for key in principal_keys(user_id) {
        if let Err(e) = Cache::del(store, &key).await {
            tracing::warn!(key, error = %e, "失效主体上下文缓存失败");
        }
    }
}

/// 失效多个用户的主体上下文缓存（角色被批量改授权时使用）。
///
/// # 参数
///
/// - `user_ids`：用户 ID 列表。
pub(crate) async fn invalidate_users_principal(user_ids: &[uuid::Uuid]) {
    for user_id in user_ids {
        invalidate_user_principal(user_id).await;
    }
}

/// 按前缀批量失效**全部**用户的主体上下文缓存，返回删除的键数。
///
/// # 为什么需要一条「全清」通道
///
/// 按持有者精确清理依赖「先反查、后删除」两步：反查一旦失败（库抖动 / 连接被占满），
/// 缓存里的旧权限码就会一直留到 TTL（300 s）——表现正是「授权了但接口还是 403」。
/// 反查失败时唯一安全的做法是退化成整命名空间清理（宁可多清，不可漏清）。
///
/// 无缓存（启动降级 / 单测）时直接返回 `0`：没有旧值可残留。
///
/// # 返回值
///
/// 累计删除的键数（仅用于日志与验收断言）。
pub(crate) async fn invalidate_all_principals() -> u64 {
    let Some(store) = try_cache() else {
        return 0;
    };
    let mut deleted = 0u64;
    for pattern in principal_patterns() {
        match Cache::delete_pattern(store, &pattern).await {
            Ok(n) => deleted += n,
            Err(e) => tracing::warn!(
                pattern = %pattern,
                error = %e,
                cache_invalidation_pending = true,
                "批量失效主体上下文缓存失败（授权已落库，最长 300 s 后由 TTL 兜底）"
            ),
        }
    }
    deleted
}

/// 反查「持有某角色 / 被授予某菜单」的用户 ID 列表。
///
/// # 参数
///
/// - `sql`：反查用的常量 SQL（只允许本文件的四条常量，不接收外部输入）。
/// - `id`：角色 ID 或菜单 ID。
/// - `label`：日志标签（`role` / `menu`）。
///
/// # 返回值
///
/// 去重后的用户 ID。`None` 表示**反查失败**（库抖动 / 连接不可用）——调用方必须据此
/// 退化为 [`invalidate_all_principals`]；`Some(vec![])` 表示「确实没有持有者」，
/// 此时没有任何缓存需要清理（不要为此触发一次 `KEYS` 扫描）。
///
/// SCOPE-EXEMPT: 查的是**关联表** `sys_user_role` / `sys_role_menu`（门禁按子串匹配
/// `FROM sys_user` 会把它算成 `sys_user`），用途是「哪些主体的缓存要失效」——
/// 系统内部的缓存维护，与「当前用户能看到哪些数据」无关；注入范围会导致漏失效（越权）。
async fn holder_ids_by(
    sql: &'static str,
    id: &uuid::Uuid,
    label: &'static str,
) -> Option<Vec<uuid::Uuid>> {
    let rows = match db().find_with_params(sql, vec![Value::from(id)]).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(?id, holder = label, error = %e, "反查缓存持有者失败");
            return None;
        }
    };
    let mut user_ids: Vec<uuid::Uuid> = Vec::with_capacity(rows.len());
    for row in rows {
        match row.get_typed::<uuid::Uuid>("user_id") {
            Ok(id) => user_ids.push(id),
            Err(e) => tracing::warn!(?id, holder = label, error = %e, "解析持有者 ID 失败，跳过"),
        }
    }
    Some(user_ids)
}

/// 反查持有某角色的用户 ID（角色授权变更 / 角色启停后清理缓存）。
pub(crate) async fn role_holder_ids(role_id: &uuid::Uuid) -> Option<Vec<uuid::Uuid>> {
    holder_ids_by(ROLE_HOLDER_SQL, role_id, "role").await
}

/// 反查被授予了某菜单的用户 ID（菜单权限码 / 状态变更后清理缓存）。
///
/// 语义是「用户 → 角色 → 菜单」两跳：改一条菜单的 `auth_code`，命中的不是一两个用户，
/// 而是所有经角色拿到这条菜单的人。
pub(crate) async fn menu_holder_ids(menu_id: &uuid::Uuid) -> Option<Vec<uuid::Uuid>> {
    holder_ids_by(MENU_HOLDER_SQL, menu_id, "menu").await
}

/// 持有某角色的用户。
const ROLE_HOLDER_SQL: &str = "SELECT DISTINCT user_id FROM sys_user_role WHERE role_id = ?";

/// 经角色被授予某菜单的用户。
const MENU_HOLDER_SQL: &str = "SELECT DISTINCT ur.user_id FROM sys_user_role ur \
                               INNER JOIN sys_role_menu rm ON rm.role_id = ur.role_id \
                               WHERE rm.menu_id = ?";

/// 按角色 ID 失效持有该角色的**全部**用户的主体上下文缓存。
///
/// 角色菜单授权变化后，受影响的不是单个用户，而是所有持有该角色的用户。
/// 为避免漏失效导致越权，这里按 `sys_user_role` 反查全部用户逐个清理
/// （授权变更是低频操作，反查成本可接受）。
///
/// SCOPE-EXEMPT: 查的是**关联表** `sys_user_role`（门禁按子串匹配 `FROM sys_user`
/// 会把它算成 `sys_user`），用途是按角色反查「哪些主体的缓存要失效」——
/// 系统内部的缓存维护，与「当前用户能看到哪些数据」无关；注入范围会导致漏失效（越权）。
///
/// # 参数
///
/// - `role_id`：角色 ID。
pub(crate) async fn invalidate_role_holders(role_id: &uuid::Uuid) {
    match role_holder_ids(role_id).await {
        // 反查失败：宁可整命名空间全清，也不能让旧权限码留到 TTL ——
        // 「授权后仍 403」的根子就在这里。
        None => {
            tracing::warn!(?role_id, "反查角色持有者失败，退化为整命名空间失效主体缓存");
            invalidate_all_principals().await;
        }
        // 角色确实没有持有者：没有缓存需要清（别为此触发一次 KEYS 扫描）。
        Some(user_ids) if user_ids.is_empty() => {
            tracing::debug!(?role_id, "角色无持有者，无需失效主体缓存");
        }
        Some(user_ids) => {
            tracing::debug!(
                ?role_id,
                users = user_ids.len(),
                "失效角色持有者的主体上下文缓存"
            );
            invalidate_users_principal(&user_ids).await;
        }
    }
}

/// 按菜单 ID 失效「经角色拿到这条菜单」的全部用户的主体上下文缓存。
///
/// 菜单是权限来源：改 `auth_code` / 停用 / 删除都会改变一批人的权限码集合。
/// 这条清理必须与 [`crate::system::user::model::bump_token_version_of_menu_holders_in`]
/// 的库侧作废配对 —— 只推进 `token_version` 不清缓存的话，用户被迫重新登录后
/// 仍会从缓存里读到旧权限码，最长 300 s 继续 403。
///
/// # 参数
///
/// - `menu_id`：菜单 ID。
pub(crate) async fn invalidate_menu_holders(menu_id: &uuid::Uuid) {
    match menu_holder_ids(menu_id).await {
        None => {
            tracing::warn!(?menu_id, "反查菜单持有者失败，退化为整命名空间失效主体缓存");
            invalidate_all_principals().await;
        }
        Some(user_ids) if user_ids.is_empty() => {
            tracing::debug!(?menu_id, "菜单无持有者，无需失效主体缓存");
        }
        Some(user_ids) => {
            tracing::debug!(
                ?menu_id,
                users = user_ids.len(),
                "失效菜单持有者的主体上下文缓存"
            );
            invalidate_users_principal(&user_ids).await;
        }
    }
}

/// 查询用户当前生效的角色编码（仅启用角色），带缓存。
///
/// 登录 / 刷新 / `me` / `menus` 都要用角色编码判定超级管理员，而原查询是
/// `sys_role` 的 `IN (SELECT ... sys_user_role)` 子查询 —— 属最高频读路径，故缓存。
/// 缓存内只保留 [`CachedRole`]（编码 + 启用位），登录路径不需要其余字段。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 返回值
///
/// 启用角色的 `role_code` 列表（顺序与数据库返回一致）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]；缓存异常不影响结果（回退查库）。
pub(crate) async fn load_role_codes(
    user_id: &uuid::Uuid,
) -> std::result::Result<Vec<String>, DbError> {
    let key = cache_key_user_roles(user_id);
    if let Some(cached) = cache_get::<Vec<CachedRole>>(&key).await {
        return Ok(cached
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| r.role_code)
            .collect());
    }
    let roles = load_roles_uncached(user_id).await?;
    let projection = roles.iter().map(CachedRole::from_model).collect::<Vec<_>>();
    cache_set(&key, &projection).await;
    Ok(projection
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| r.role_code)
        .collect())
}

/// 查询用户当前生效的角色（不走缓存，供内部装载 / 回填复用）。
///
/// SCOPE-EXEMPT: 装载的是**当前登录者本人**的角色（`user_id` 取自令牌 `sub`），
/// 这是权限快照的构成部分 —— 「算权限」的查询不能再被「权限」过滤，否则会自我循环。
async fn load_roles_uncached(
    user_id: &uuid::Uuid,
) -> std::result::Result<Vec<SysRoleModel>, DbError> {
    SysRoleModel::find_by(
        db(),
        "id IN (SELECT role_id FROM sys_user_role \
                WHERE user_id = ? AND (expire_time IS NULL OR expire_time > now()))",
        (user_id,),
    )
    .await
}

/// 判断角色编码中是否包含超级管理员。
///
/// # 参数
///
/// - `codes`：角色编码列表。
///
/// # 返回值
///
/// 含 [`constants::SUPER_ADMIN_ROLE_CODE`] 时返回 `true`。
pub(crate) fn is_super_admin(codes: &[String]) -> bool {
    codes.iter().any(|c| c == constants::SUPER_ADMIN_ROLE_CODE)
}

/// 查询用户权限码列表。
///
/// # 参数
///
/// - `user_id`：用户 ID。
/// - `super_admin`：是否超级管理员（是则短路为 `["*:*:*"]`，不查授权表）。
///
/// # 返回值
///
/// 去重后的权限码列表。
///
/// # 异常情况
///
/// 数据库异常或行缺少 `auth_code` 列时返回 [`DbError`]。
pub(crate) async fn load_permissions(
    user_id: &uuid::Uuid,
    super_admin: bool,
) -> std::result::Result<Vec<String>, DbError> {
    if super_admin {
        // 通配码供**框架的** `#[nik::permission]` 校验层短路使用；业务侧的超管判定
        // （数据权限 / 脱敏 / 菜单）一律以角色编码为准（见 shared/principal.rs），
        // 不读取本通配码，故二者不构成双真值源。
        return Ok(vec!["*:*:*".to_string()]);
    }

    // 权限码由「用户 → 角色 → 菜单」三表 JOIN 得出，是登录后最高频的读路径，故缓存。
    let key = cache_key_user_perms(user_id);
    if let Some(cached) = cache_get(&key).await {
        return Ok(cached);
    }
    let permissions = load_permissions_uncached(user_id).await?;
    cache_set(&key, &permissions).await;
    Ok(permissions)
}

/// 查询用户权限码列表（不走缓存，供内部装载 / 回填复用）。
async fn load_permissions_uncached(
    user_id: &uuid::Uuid,
) -> std::result::Result<Vec<String>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT DISTINCT m.auth_code AS auth_code FROM sys_menu m \
             INNER JOIN sys_role_menu rm ON rm.menu_id = m.id \
             INNER JOIN sys_user_role ur ON ur.role_id = rm.role_id \
             WHERE ur.user_id = ? AND m.deleted_at IS NULL AND m.status = 1 \
               AND m.auth_code <> '' \
               AND (ur.expire_time IS NULL OR ur.expire_time > now())",
            vec![Value::from(user_id)],
        )
        .await?;

    let mut permissions: Vec<String> = Vec::with_capacity(rows.len());
    for row in rows {
        let code: String = row.get_typed("auth_code")?;
        if !code.is_empty() && !permissions.contains(&code) {
            permissions.push(code);
        }
    }
    // 兼容期双码：库里只存规范码（`V0241` 把合同域就地改名成 `business:erp:contract:*`），
    // 这里给**每个已授予的码**补上它的历史别名 —— 老令牌 / 前端老包 / 第三方脚本里
    // 仍在用的 `erp:contract:*` 因此继续放行。收口只此一处（见 `shared::permission_alias`）。
    crate::shared::permission_alias::expand(&mut permissions);
    Ok(permissions)
}

/// 登录 / 刷新签发令牌时使用的**新鲜**权限快照。
///
/// # 为什么签发路径必须绕过缓存（P1-FIX-3）
///
/// 接口级 403 只认**令牌里的权限快照**（`nik-web` 的 `has_permission` 不查库），
/// 而令牌是登录 / 刷新时签发的。若签发时读的是 `perm:user_perms:{uid}` 缓存：
/// 授权写库后**没人删这个键**（例如运维直连 SQL 补授权、或别的实例写的），
/// 用户重新登录拿到的仍是旧权限码 → 继续 403，直到 TTL（300 s）过期；
/// 单机部署时运维只能「重启实例」清进程内缓存 —— 这就是本卡的现象。
///
/// 因此签发路径直读库（库是唯一真源），并把读到的真值**写穿**回缓存，
/// 让随后的 `me` / `menus` 与令牌快照保持一致（不会出现「按钮出现但接口 403」）。
///
/// 代价：登录 / 刷新各多两条查询（本来就在同一张表上，且这两个接口不在热路径）。
#[derive(Debug, Clone)]
pub(crate) struct PrincipalSnapshot {
    /// 启用角色的编码（决定是否超管）。
    pub role_codes: Vec<String>,
    /// 是否超级管理员（持有 `SUPER_ADMIN` 角色）。
    pub super_admin: bool,
    /// 权限码集合（超管为通配 `*:*:*`）。
    pub permissions: Vec<String>,
}

/// 直读库组装签发用的权限快照，并把结果写穿到主体上下文缓存。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 返回值
///
/// [`PrincipalSnapshot`]。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（调用方回 500，不签发携带空权限的令牌）。
pub(crate) async fn load_principal_snapshot(
    user_id: &uuid::Uuid,
) -> std::result::Result<PrincipalSnapshot, DbError> {
    let roles = load_roles_uncached(user_id).await?;
    let projection: Vec<CachedRole> = roles.iter().map(CachedRole::from_model).collect();
    let role_codes: Vec<String> = projection
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.role_code.clone())
        .collect();
    let super_admin = is_super_admin(&role_codes);
    let permissions = if super_admin {
        vec!["*:*:*".to_string()]
    } else {
        load_permissions_uncached(user_id).await?
    };

    // 写穿：让 me / menus 与本次签发的快照一致。
    // 超管的通配码**不入缓存** —— 一旦他后来被摘掉超管角色，缓存里的 `*:*:*`
    // 会被非超管分支读到，那就是提权（缓存键是同一个）。
    cache_set(&cache_key_user_roles(user_id), &projection).await;
    if !super_admin {
        cache_set(&cache_key_user_perms(user_id), &permissions).await;
    }

    Ok(PrincipalSnapshot {
        role_codes,
        super_admin,
        permissions,
    })
}

/// 查询用户可见菜单（仅目录与菜单，不含按钮），超级管理员为全部启用菜单，带缓存。
///
/// 返回 `MenuRes` 而非 `SysMenuModel`：后者未派生 serde，而调用方本来就要立即
/// 转成 `MenuRes` 再建树，直接缓存投影可省去一次转换，也满足缓存的 serde 约束。
///
/// # 参数
///
/// - `user_id`：用户 ID。
/// - `super_admin`：是否超级管理员。
///
/// # 返回值
///
/// 已按 `sort_order` 升序的扁平菜单列表。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]；缓存异常不影响结果（回退查库）。
pub(crate) async fn load_user_menus(
    user_id: &uuid::Uuid,
    super_admin: bool,
) -> std::result::Result<Vec<crate::system::menu::dto::MenuRes>, DbError> {
    // 超管的可见菜单是「全部启用目录与菜单」，与用户无关；但仍按用户键缓存，
    // 避免超管与非超管共用一键时读到彼此的结果。
    let key = cache_key_user_menus(user_id);
    if let Some(cached) = cache_get(&key).await {
        return Ok(cached);
    }
    let menus = load_user_menus_uncached(user_id, super_admin).await?;
    let nodes: Vec<crate::system::menu::dto::MenuRes> = menus
        .iter()
        .map(crate::system::menu::dto::MenuRes::from)
        .collect();
    cache_set(&key, &nodes).await;
    Ok(nodes)
}

/// 查询用户可见菜单（不走缓存，供内部装载 / 回填复用）。
async fn load_user_menus_uncached(
    user_id: &uuid::Uuid,
    super_admin: bool,
) -> std::result::Result<Vec<SysMenuModel>, DbError> {
    if super_admin {
        SysMenuModel::find_by(
            db(),
            "menu_type IN (1, 2) AND status = 1 ORDER BY sort_order ASC",
            (),
        )
        .await
    } else {
        SysMenuModel::find_by(
            db(),
            "menu_type IN (1, 2) AND status = 1 \
             AND id IN (SELECT rm.menu_id FROM sys_role_menu rm \
                        INNER JOIN sys_user_role ur ON ur.role_id = rm.role_id \
                        WHERE ur.user_id = ? \
                          AND (ur.expire_time IS NULL OR ur.expire_time > now())) \
             ORDER BY sort_order ASC",
            (user_id,),
        )
        .await
    }
}

// ==================== 登录状态维护 ====================

/// 记录一次登录失败：累加失败计数，达到阈值时锁定账号。
///
/// # 参数
///
/// - `user_id`：用户 ID。
/// - `fail_count`：累加后的失败次数。
/// - `is_lock`：是否达到锁定阈值。
/// - `now`：失败发生时间（锁定时写入 `lock_time`）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub(crate) async fn record_login_failure(
    user_id: &uuid::Uuid,
    fail_count: i16,
    is_lock: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<(), DbError> {
    let mut changeset = SysUserModel::of(user_id).set(SysUserModel::login_fail_count(), fail_count);
    if is_lock {
        changeset = changeset
            .set(SysUserModel::is_locked(), true)
            .set(SysUserModel::lock_time(), Some(now));
    }
    changeset.update(db()).await.map(|_| ())
}

// ==================== 登录安全策略 ====================

/// 读取登录安全参数：连续失败锁定阈值与自动解锁时长。
///
/// 取值优先来自 `sys_config`（[`constants::CONFIG_KEY_LOGIN_MAX_FAIL`] /
/// [`constants::CONFIG_KEY_LOGIN_LOCK_MINUTES`]，由种子迁移 `S__config_data` 写入）；
/// 参数缺失、被停用或解析失败时回退 [`constants`] 中的默认常量，因此参数不全
/// 不会导致登录不可用。
///
/// # 返回值
///
/// `(连续失败锁定阈值, 自动解锁时长分钟)`。
pub(crate) async fn login_security_policy() -> (i16, i64) {
    let max_fail = SysConfigModel::read_i64(constants::CONFIG_KEY_LOGIN_MAX_FAIL)
        .await
        .and_then(|value| i16::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(constants::MAX_LOGIN_FAIL_COUNT);
    let lock_minutes = SysConfigModel::read_i64(constants::CONFIG_KEY_LOGIN_LOCK_MINUTES)
        .await
        .filter(|value| *value > 0)
        .unwrap_or(constants::LOCK_MINUTES);
    (max_fail, lock_minutes)
}
