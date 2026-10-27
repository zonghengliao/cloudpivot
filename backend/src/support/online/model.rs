//! 在线用户会话（缓存承载）。
//!
//! 会话不落数据库：以「访问令牌的 `jti`」为键写入全局 `cache()`
//! （`online:token:{jti}`，TTL 对齐令牌剩余有效期），从而：
//!
//! - 令牌过期即自动从在线列表消失，无需清理任务；
//! - 登出 / 强制下线只需删除键并写入框架同格式的令牌黑名单。
//!
//! 自 CODE-16（B7）起，**refresh 令牌同样登记**（`online:refresh:{jti}`）：此前只有
//! access 令牌在注册表里，`force_logout_user` 枚举不到 refresh，于是「改密 / 停用 /
//! 改角色后旧 refresh 仍能续期」一直是个缺口。两套键共用同一套吊销语义：
//!
//! | 键 | 值 | 吊销动作 |
//! |----|----|----------|
//! | `online:token:{access jti}` | [`OnlineSession`] | 删键 + 拉黑 `jti`（TTL 取剩余期） |
//! | `online:refresh:{refresh jti}` | [`RefreshSession`] | 删键 + 拉黑 `jti`（TTL 取剩余期） |
//!
//! # 失败传播
//!
//! 吊销是安全动作，**不许静默失败**（评审 §1.3）：枚举 / 拉黑任一步失败都通过
//! [`Result`] 传给调用方，由处理器决定返回 500 还是可接受降级（判断依据见各调用点注释）。
//!
//! 数据访问与领域行为集中在本文件，处理器只做业务编排。

use alun::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::constants;

/// 在线用户会话快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSession {
    /// 访问令牌唯一标识（`jti`），同时是缓存键后缀。
    pub token_id: String,
    /// 用户 ID（令牌 `sub`）。
    pub user_id: String,
    /// 用户名。
    pub username: String,
    /// 客户端 IP 文本。
    pub ip: Option<String>,
    /// 登录地点（归属地缓存命中时填充，可能为空）。
    pub location: Option<String>,
    /// 浏览器类型。
    pub browser: Option<String>,
    /// 操作系统类型。
    pub os_type: Option<String>,
    /// 登录时间。
    pub login_time: chrono::DateTime<chrono::Utc>,
    /// 令牌到期时间（Unix 时间戳，秒）。
    pub expire_at: i64,
}

/// 已签发 refresh 令牌的登记快照。
///
/// 只登记吊销所需的最小字段：`user_id`（按用户清理）、`sid`（按会话清理）、
/// `exp`（拉黑 TTL）；令牌正文不落缓存。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshSession {
    /// refresh 令牌唯一标识（`jti`），同时是缓存键后缀。
    pub jti: String,
    /// 用户 ID（令牌 `sub`）。
    pub user_id: String,
    /// 会话标识（令牌 claim `sid`）：同一登录会话内的全部 refresh 令牌共享。
    pub sid: String,
    /// 令牌到期时间（Unix 时间戳，秒），等于 `siat + refresh_ttl`。
    pub exp: i64,
}

/// 在线会话数据访问（缓存命名空间）。
pub struct OnlineSessionModel;

impl OnlineSessionModel {
    /// 登记一条在线会话。
    ///
    /// # 参数
    ///
    /// - `session`：会话快照。
    /// - `ttl_secs`：缓存有效期（秒），应取令牌剩余有效期。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或写入失败时返回错误，由调用方决定是否中断主流程。
    pub async fn record_login(
        session: &OnlineSession,
        ttl_secs: u64,
    ) -> std::result::Result<(), Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg("缓存未初始化，无法登记在线会话".to_string()));
        };
        let key = Self::key(&session.token_id);
        Cache::set_ex(store, &key, session, ttl_secs).await
    }

    /// 移除一条在线会话（登出 / 会话结束时调用）。
    ///
    /// 键不存在不视为错误。
    ///
    /// # 参数
    ///
    /// - `token_id`：访问令牌 `jti`。
    pub async fn record_logout(token_id: &str) {
        let Some(store) = try_cache() else {
            return;
        };
        if let Err(e) = Cache::del(store, &Self::key(token_id)).await {
            tracing::warn!(token_id, error = %e, "移除在线会话失败");
        }
    }

    /// 读取单条在线会话。
    ///
    /// # 参数
    ///
    /// - `token_id`：访问令牌 `jti`。
    ///
    /// # 返回值
    ///
    /// 存在返回 `Some`，不存在或已过期返回 `None`。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或读取失败时返回错误。
    pub async fn find(token_id: &str) -> std::result::Result<Option<OnlineSession>, Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg("缓存未初始化，无法查询在线会话".to_string()));
        };
        Cache::get(store, &Self::key(token_id)).await
    }

    /// 列出全部在线会话（按登录时间倒序）。
    ///
    /// # 返回值
    ///
    /// 在线会话列表；缓存未初始化时返回空列表（在线列表属于旁路观测，
    /// 读取侧允许降级）。
    ///
    /// # 异常情况
    ///
    /// 枚举键失败时返回错误。个别键读取失败只记告警并跳过，不影响整体列表。
    pub async fn list_all() -> std::result::Result<Vec<OnlineSession>, Error> {
        let Some(store) = try_cache() else {
            return Ok(Vec::new());
        };
        Self::collect_all(store).await
    }

    /// 列出全部在线会话（缓存缺失即报错，供吊销路径使用）。
    ///
    /// 与 [`OnlineSessionModel::list_all`] 的差别只在「缓存未初始化」时：这里返回
    /// `Err` 而不是空列表——吊销路径上「拿不到列表」等于「吊销不了」，必须让
    /// 调用方看见，否则就是又一次 fail-open。
    async fn list_all_strict() -> std::result::Result<Vec<OnlineSession>, Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg("缓存未初始化，无法枚举在线会话".to_string()));
        };
        Self::collect_all(store).await
    }

    /// 按 `online:token:*` 枚举并读取在线会话（调用方已确保缓存可用）。
    async fn collect_all(store: &impl Cache) -> std::result::Result<Vec<OnlineSession>, Error> {
        let pattern = format!("{}*", constants::CACHE_KEY_PREFIX_ONLINE);
        let keys = Cache::keys(store, &pattern).await?;
        let mut sessions = Vec::with_capacity(keys.len());
        for key in keys {
            match Cache::get::<OnlineSession>(store, &key).await {
                Ok(Some(session)) => sessions.push(session),
                Ok(None) => {}
                Err(e) => tracing::warn!(key, error = %e, "读取在线会话失败，已跳过"),
            }
        }
        sessions.sort_by_key(|b| std::cmp::Reverse(b.login_time));
        Ok(sessions)
    }

    /// 登记一条 refresh 令牌（登录 / 轮换成功后调用）。
    ///
    /// # 参数
    ///
    /// - `session`：refresh 登记快照。
    /// - `ttl_secs`：缓存有效期（秒），取该令牌剩余有效期。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或写入失败时返回错误。
    pub async fn record_refresh(
        session: &RefreshSession,
        ttl_secs: u64,
    ) -> std::result::Result<(), Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg(
                "缓存未初始化，无法登记 refresh 令牌".to_string(),
            ));
        };
        Cache::set_ex(store, &refresh_key(&session.jti), session, ttl_secs).await
    }

    /// 移除一条 refresh 令牌登记（轮换 / 登出时调用）。
    ///
    /// 键不存在不视为错误。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或删除失败时返回错误——轮换路径上「删不掉旧键」意味着注册表
    /// 会残留一条已作废的令牌，宁可报错也不算成功。
    pub async fn forget_refresh(jti: &str) -> std::result::Result<(), Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg(
                "缓存未初始化，无法移除 refresh 令牌登记".to_string(),
            ));
        };
        Cache::del(store, &refresh_key(jti)).await
    }

    /// 读取单条 refresh 令牌登记（判「该 refresh 是否仍被登记为有效」）。
    ///
    /// # 参数
    ///
    /// - `jti`：refresh 令牌 `jti`。
    ///
    /// # 返回值
    ///
    /// 存在返回 `Some`（仍被登记为有效），不存在或已过期返回 `None`（已吊销 / 已作废）。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或读取失败时返回错误。
    pub async fn find_refresh(jti: &str) -> std::result::Result<Option<RefreshSession>, Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg(
                "缓存未初始化，无法查询 refresh 令牌登记".to_string(),
            ));
        };
        Cache::get(store, &refresh_key(jti)).await
    }

    /// 列出全部 refresh 令牌登记。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或枚举失败时返回错误；个别键读取失败只记告警并跳过。
    pub async fn list_refresh() -> std::result::Result<Vec<RefreshSession>, Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg(
                "缓存未初始化，无法枚举 refresh 令牌".to_string(),
            ));
        };
        let pattern = format!("{}*", constants::CACHE_KEY_PREFIX_ONLINE_REFRESH);
        let keys = Cache::keys(store, &pattern).await?;
        let mut sessions = Vec::with_capacity(keys.len());
        for key in keys {
            match Cache::get::<RefreshSession>(store, &key).await {
                Ok(Some(session)) => sessions.push(session),
                Ok(None) => {}
                Err(e) => tracing::warn!(key, error = %e, "读取 refresh 令牌登记失败，已跳过"),
            }
        }
        Ok(sessions)
    }

    /// 强制下线：移除在线会话并把令牌写入黑名单（立即失效）。
    ///
    /// 黑名单键格式与框架 [`JWT::is_blacklisted`] 保持一致，TTL 取令牌剩余
    /// 有效期，从而认证层的黑名单检查可立即识别。
    ///
    /// # 参数
    ///
    /// - `session`：待下线的在线会话。
    ///
    /// # 异常情况
    ///
    /// 缓存未初始化或写入失败时返回错误。
    pub async fn force_logout(session: &OnlineSession) -> std::result::Result<(), Error> {
        let Some(store) = try_cache() else {
            return Err(Error::Msg("缓存未初始化，无法强制下线".to_string()));
        };
        Cache::del(store, &Self::key(&session.token_id)).await?;
        blacklist_jti(store, &session.token_id, remaining_ttl(session.expire_at)).await
    }

    /// 生成在线会话缓存键。
    ///
    /// # 参数
    ///
    /// - `token_id`：访问令牌 `jti`。
    ///
    /// # 返回值
    ///
    /// 形如 `online:token:{jti}` 的缓存键。
    fn key(token_id: &str) -> String {
        format!("{}{}", constants::CACHE_KEY_PREFIX_ONLINE, token_id)
    }
}

/// 强制下线指定用户的全部已签发令牌（权限被收回 / 停用 / 删除 / 改密时调用）。
///
/// 一次遍历两个注册表：
///
/// 1. `online:token:*` —— 该用户的 access 令牌，删键 + 拉黑；
/// 2. `online:refresh:*` —— 该用户的 refresh 令牌，删键 + 拉黑。
///
/// 只有两者都做，「改密 / 停用后旧 refresh 仍能续期」的缺口才算补上（CODE-16 B7）。
///
/// # 参数
///
/// - `user_id`：目标用户 ID（令牌 `sub`）。
///
/// # 返回值
///
/// 实际下线的令牌数量（access + refresh 合计）。
///
/// # 异常情况
///
/// - 枚举任一注册表失败 → `Err`（吊销不了就必须让调用方知道）；
/// - 个别令牌拉黑失败 → 记 `error` 日志，继续处理其余令牌，最终返回 `Err`，
///   避免「一个坏键让其余会话逃过吊销」。
pub(crate) async fn force_logout_user(user_id: &str) -> std::result::Result<usize, Error> {
    let sessions = OnlineSessionModel::list_all_strict().await?;
    let refresh_sessions = OnlineSessionModel::list_refresh().await?;

    let mut kicked = 0usize;
    let mut failure: Option<Error> = None;

    for session in sessions.into_iter().filter(|s| s.user_id == user_id) {
        match OnlineSessionModel::force_logout(&session).await {
            Ok(()) => kicked += 1,
            Err(e) => {
                tracing::error!(
                    user_id,
                    token_id = %session.token_id,
                    error = %e,
                    "强制下线 access 令牌失败"
                );
                failure = Some(e);
            }
        }
    }

    for session in refresh_sessions
        .into_iter()
        .filter(|s| s.user_id == user_id)
    {
        match revoke_refresh(&session).await {
            Ok(()) => kicked += 1,
            Err(e) => {
                tracing::error!(
                    user_id,
                    refresh_jti = %session.jti,
                    error = %e,
                    "吊销 refresh 令牌失败"
                );
                failure = Some(e);
            }
        }
    }

    match failure {
        Some(e) => Err(e),
        None => Ok(kicked),
    }
}

/// 强制下线除指定用户名外的全部在线用户（菜单 / 角色权限全局变更时调用）。
///
/// 菜单、角色菜单授权等变更会影响一批用户，而 JWT 权限在签发时即固化在令牌中，
/// 无法按权限点批量撤回；因此把受影响的在线令牌（access + refresh）统一拉黑，
/// 令其重新登录获取新权限。通常排除内置超级管理员（权限短路、不受菜单变更影响），
/// 避免管理员改完权限后把自己踢下线。
///
/// # 参数
///
/// - `exclude_usernames`：需要保留在线状态的用户名集合（如 `["admin"]`）。
///
/// # 返回值
///
/// 实际下线的令牌数量（access + refresh 合计）。
///
/// # 异常情况
///
/// 与 [`force_logout_user`] 相同：枚举失败或任一令牌吊销失败都返回 `Err`。
pub(crate) async fn force_logout_all_except(
    exclude_usernames: &[&str],
) -> std::result::Result<usize, Error> {
    let sessions = OnlineSessionModel::list_all_strict().await?;
    let refresh_sessions = OnlineSessionModel::list_refresh().await?;

    // refresh 登记里只有 user_id，需要 user_id → username 才能套用排除规则；
    // 用 access 会话表里的映射（同一用户在两张表里一致）。
    let username_of = |user_id: &str| -> Option<String> {
        sessions
            .iter()
            .find(|s| s.user_id == user_id)
            .map(|s| s.username.clone())
    };

    let mut kicked = 0usize;
    let mut failure: Option<Error> = None;

    for session in &sessions {
        if exclude_usernames.contains(&session.username.as_str()) {
            continue;
        }
        match OnlineSessionModel::force_logout(session).await {
            Ok(()) => kicked += 1,
            Err(e) => {
                tracing::error!(
                    user_id = %session.user_id,
                    token_id = %session.token_id,
                    error = %e,
                    "强制下线 access 令牌失败"
                );
                failure = Some(e);
            }
        }
    }

    for session in &refresh_sessions {
        if let Some(username) = username_of(&session.user_id)
            && exclude_usernames.contains(&username.as_str())
        {
            continue;
        }
        match revoke_refresh(session).await {
            Ok(()) => kicked += 1,
            Err(e) => {
                tracing::error!(
                    user_id = %session.user_id,
                    refresh_jti = %session.jti,
                    error = %e,
                    "吊销 refresh 令牌失败"
                );
                failure = Some(e);
            }
        }
    }

    match failure {
        Some(e) => Err(e),
        None => Ok(kicked),
    }
}

/// 吊销指定会话（`sid`）名下的全部 refresh 令牌（登出时调用）。
///
/// 登出请求手里只有 access 令牌，但 access 与 refresh 共享同一个 `sid`，
/// 因此无需客户端回传 refresh 令牌也能把该会话的续期能力一次收掉
/// （安全审计 CODE-05）。
///
/// # 参数
///
/// - `sid`：会话标识（access 令牌 claim `sid`）。
///
/// # 返回值
///
/// 实际吊销的 refresh 令牌数量。
///
/// # 异常情况
///
/// 枚举失败或任一令牌吊销失败返回 `Err`。
pub(crate) async fn revoke_refresh_by_sid(sid: &str) -> std::result::Result<usize, Error> {
    let sessions = OnlineSessionModel::list_refresh().await?;
    let mut revoked = 0usize;
    let mut failure: Option<Error> = None;
    for session in sessions.iter().filter(|s| s.sid == sid) {
        match revoke_refresh(session).await {
            Ok(()) => revoked += 1,
            Err(e) => {
                tracing::error!(
                    sid,
                    refresh_jti = %session.jti,
                    error = %e,
                    "按会话吊销 refresh 令牌失败"
                );
                failure = Some(e);
            }
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(revoked),
    }
}

/// 吊销单条 refresh 令牌：删登记键 + 写黑名单（TTL 取剩余有效期）。
///
/// # 异常情况
///
/// 缓存未初始化、删键失败或写黑名单失败返回 `Err`。
async fn revoke_refresh(session: &RefreshSession) -> std::result::Result<(), Error> {
    let Some(store) = try_cache() else {
        return Err(Error::Msg(
            "缓存未初始化，无法吊销 refresh 令牌".to_string(),
        ));
    };
    Cache::del(store, &refresh_key(&session.jti)).await?;
    blacklist_jti(store, &session.jti, remaining_ttl(session.exp)).await
}

/// 显式写入令牌黑名单（错误可见）。
///
/// 供「登出时拉黑客户端提交的 refresh 令牌」「轮换时作废旧 refresh」直接使用：
/// 框架的 `JWT::blacklist` 内部吞掉写入错误（`let _ = ...`），吊销路径需要能感知
/// 失败，因此在这里自己写、把错误传上去（评审 §1.3）。
///
/// # 参数
///
/// - `jti`：令牌唯一标识。
/// - `exp`：令牌到期时间（Unix 秒），黑名单 TTL 取其剩余期。
///
/// # 异常情况
///
/// 缓存未初始化或写入失败返回错误。
pub(crate) async fn blacklist_token(jti: &str, exp: i64) -> std::result::Result<(), Error> {
    let Some(store) = try_cache() else {
        return Err(Error::Msg("缓存未初始化，无法写入令牌黑名单".to_string()));
    };
    blacklist_jti(store, jti, remaining_ttl(exp)).await
}

/// 把令牌 `jti` 写入框架格式的黑名单键（`token:blacklist:{jti}`）。
async fn blacklist_jti(
    store: &impl Cache,
    jti: &str,
    ttl_secs: u64,
) -> std::result::Result<(), Error> {
    let key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_BLACKLIST, jti);
    Cache::set_ex(store, &key, &serde_json::json!(true), ttl_secs).await
}

/// 取「距到期的剩余秒数」（已过期按 1 秒，避免写出永久键）。
fn remaining_ttl(expire_at: i64) -> u64 {
    (expire_at - chrono::Utc::now().timestamp()).max(1) as u64
}

/// 生成 refresh 登记缓存键。
fn refresh_key(jti: &str) -> String {
    format!("{}{}", constants::CACHE_KEY_PREFIX_ONLINE_REFRESH, jti)
}
