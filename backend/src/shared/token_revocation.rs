//! 令牌吊销的唯一写入口 + 权威判定（ADR-20261101）。
//!
//! # 为什么需要这个模块
//!
//! 单条令牌的吊销事实（`token:blacklist:{jti}`）此前**只**住在 Redis 里，于是有两个洞：
//!
//! 1. **Redis 读不到** → 项目侧复检层 `middleware/auth_guard` 只能 fail-closed 401，
//!    表现是「能登录、登录后什么都干不了」；
//! 2. **Redis 写不进**（noeviction 内存写满）→ 框架 `JWT::blacklist` 用 `let _ =`
//!    吞掉错误（`vendor/nik-web/src/jwt.rs:219-220`），登出照回 200 而令牌实际未失效。
//!
//! ADR 的结论是把「哪条令牌被吊销」变成 **PostgreSQL 侧的事实**（`sys_token_blacklist`），
//! 认证层读库判定，Redis 退化为**读加速器**：
//!
//! | 动作 | 顺序 | 失败语义 |
//! |------|------|----------|
//! | [`revoke`] | **先**写库（权威）→ **再**写缓存（尽力） | 写库失败 = 吊销没生效 → `Err`（调用方 500）；写缓存失败只 `warn` + `auth_cache_degraded=true`，**不改返回码** |
//! | [`is_revoked`] | 只查库 | 查库失败 → `Err`（调用方按 fail-closed 401 处理） |
//!
//! # 为什么放在 `shared/` 而不是 `system/auth/model.rs`
//!
//! 调用方跨两个业务容器：写入口是 `support::online::model`（登出 / 强踢 / 轮换），
//! 读入口是 `middleware::auth_guard`。放进 `system/` 会新开一条 `support → system`
//! 的代码边（实测此前 `support/` 对 `crate::system::` 只有注释引用）。`shared/` 本来
//! 就是横切层，且已有「横切代码直连库」的先例（`shared/startup_guard.rs`）。
//!
//! # 正常路径**不读缓存**（ADR §3.1）
//!
//! 读缓存会引入「库里有、缓存里没有」的判定分裂：登出写库成功但写缓存失败时，
//! 缓存里那条「未吊销」的信号就是错的。因此 [`is_revoked`] 只查库——缓存那份
//! 黑名单只服务于框架自带的检查（`vendor/nik-web/src/middleware/auth.rs:154`）。
//! 这条取舍是 ADR 的核心，**别在实现时"优化"回去**。

use nik::prelude::*;

use crate::shared::constants;

/// 被吊销令牌的类型（`sys_token_blacklist.token_kind`）。
///
/// 库侧**不加 CHECK**（见 V0176 头注释）：诊断字段，给它加约束等于把「枚举没同步」
/// 升级成「登出 500」。取值由本枚举收敛。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlacklistKind {
    /// 访问令牌（`token_type = access`）。
    Access,
    /// 刷新令牌（`token_type = refresh`）。
    Refresh,
}

impl BlacklistKind {
    /// 落库取值（`varchar(8)`）。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

/// 吊销原因（`sys_token_blacklist.reason`，诊断 / 排障用）。
///
/// 同样**不加库侧 CHECK**（理由见 [`BlacklistKind`]）：新增原因只需加一个变体，
/// 不需要迁移；库侧出现未知取值也不会让认证路径报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlacklistReason {
    /// 用户主动登出。
    Logout,
    /// 刷新令牌轮换（旧 refresh 一次作废）。
    Rotate,
    /// 强制下线（管理员踢人 / 权限收回 / 停用 / 检出 refresh 复用）。
    ForceLogout,
}

impl BlacklistReason {
    /// 落库取值（`varchar(32)`）。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Logout => "logout",
            Self::Rotate => "rotate",
            Self::ForceLogout => "force_logout",
        }
    }
}

/// 吊销一条令牌：**先落库（权威），再写缓存（尽力）**。
///
/// # 参数
///
/// - `jti`：令牌唯一标识（JWT claim `jti`）。空串直接返回 `Ok(())`——没有 `jti`
///   的令牌本来就无法被复检，写进去只会污染表；
/// - `user_id`：令牌主体（`sub`），仅用于排障与「按用户查吊销」，可为 `None`；
/// - `kind`：令牌类型；
/// - `reason`：吊销原因；
/// - `exp`：令牌绝对到期时刻（Unix 秒）。库里记**绝对时刻**而不是剩余 TTL，
///   因此行天然只活到令牌失效那一刻，不随写入时机漂移。
///
/// # 返回值
///
/// - `Ok(())`：库侧已生效（缓存写失败也返回 `Ok`——库是权威，见下）；
/// - `Err(_)`：**库写入失败**，即吊销没有生效。调用方必须让调用者看到
///   （登出 → 500），不许静默吞掉。
///
/// # 缓存侧失败的语义
///
/// 只 `warn` + 固定字段 `auth_cache_degraded=true`（沿用 FIX-03 建立的告警口径，
/// 最简告警规则就是 `grep auth_cache_degraded=true`），**不改返回码**：库里已经
/// 生效，登出就是成功了。
///
/// # 幂等
///
/// `ON CONFLICT (jti) DO NOTHING`——登出重试 / 轮换重放写同一 `jti` 不报错
/// （实测同 `jti` 连插两次 → 1 行）。
pub(crate) async fn revoke(
    jti: &str,
    user_id: Option<&str>,
    kind: BlacklistKind,
    reason: BlacklistReason,
    exp: i64,
) -> std::result::Result<(), Error> {
    if jti.trim().is_empty() {
        return Ok(());
    }

    let expire_at = chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0).unwrap_or_else(chrono::Utc::now);

    // ① 权威写入：库。失败即 `Err`——「写不进就是没吊销」。
    //    `user_id` 可空：绑定 uuid 位不能传 `Value::Null`（驱动编码不上），
    //    统一走 `NULLIF(?::text,'')::uuid`（技能 §3 的既有结论）。
    db().execute_with_params(
        "INSERT INTO sys_token_blacklist (jti, user_id, token_kind, reason, expire_at) \
         VALUES (?, NULLIF(?::text, '')::uuid, ?, ?, ?::timestamptz) \
         ON CONFLICT (jti) DO NOTHING",
        vec![
            Value::from(jti),
            Value::from(user_id.unwrap_or("")),
            Value::from(kind.as_str()),
            Value::from(reason.as_str()),
            Value::from(expire_at),
        ],
    )
    .await
    .map_err(|error| Error::Msg(format!("令牌吊销写库失败（jti={jti}）：{error}")))?;

    // ② 尽力加速：缓存。失败只 warn —— 库侧已是事实，缓存只是框架黑名单检查的旁路。
    let degraded = match try_cache() {
        Some(store) => {
            let key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_BLACKLIST, jti);
            let ttl = (exp - chrono::Utc::now().timestamp()).max(1) as u64;
            match Cache::set_ex(store, &key, &serde_json::json!(true), ttl).await {
                Ok(()) => false,
                Err(error) => {
                    tracing::warn!(
                        jti,
                        reason = reason.as_str(),
                        error = %error,
                        auth_cache_degraded = true,
                        "令牌吊销已落库，但写缓存失败（已降级：库侧生效，判定不看缓存）"
                    );
                    true
                }
            }
        }
        None => {
            tracing::warn!(
                jti,
                reason = reason.as_str(),
                auth_cache_degraded = true,
                "令牌吊销已落库，但缓存未初始化（已降级：库侧生效，判定不看缓存）"
            );
            true
        }
    };

    // ADR §5.4：`metrics` 未装 recorder 时是 no-op，可安全常开。
    metrics::gauge!("cloudpivot_auth_cache_degraded").set(if degraded { 1.0 } else { 0.0 });

    Ok(())
}

/// 判定一条令牌是否已被吊销（**只查库**，不看缓存）。
///
/// # 参数
///
/// - `jti`：令牌唯一标识。
///
/// # 返回值
///
/// - `Ok(true)`：库里有该 `jti` 且 `expire_at > now()`（仍在有效期内）→ 已吊销；
/// - `Ok(false)`：未命中，或命中但已过期（清理任务还没回收的行不算吊销——
///   `expire_at > now()` 必须是查询的一部分，否则「删过期行」的动作会变成 401）；
/// - `Err(_)`：查库失败。调用方必须按 fail-closed 处理（401），**不许放行**。
pub(crate) async fn is_revoked(jti: &str) -> DbResult<bool> {
    if jti.trim().is_empty() {
        return Ok(false);
    }
    let rows = db()
        .find_with_params(
            "SELECT 1 AS hit FROM sys_token_blacklist WHERE jti = ? AND expire_at > now() LIMIT 1",
            vec![Value::from(jti)],
        )
        .await?;
    Ok(!rows.is_empty())
}

/// 统计表内当前行数（供可观测指标 `cloudpivot_token_blacklist_rows` 使用）。
///
/// # 返回值
///
/// 行数；查库失败返回 `Err`。
pub(crate) async fn row_count() -> DbResult<i64> {
    db().scalar_with_params("SELECT count(*) FROM sys_token_blacklist", vec![])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 枚举落库取值稳定（库侧无 CHECK，取值变化只影响排障口径，但要防手滑）。
    #[test]
    fn blacklist_kind_and_reason_codes_are_stable() {
        assert_eq!(BlacklistKind::Access.as_str(), "access");
        assert_eq!(BlacklistKind::Refresh.as_str(), "refresh");
        assert_eq!(BlacklistReason::Logout.as_str(), "logout");
        assert_eq!(BlacklistReason::Rotate.as_str(), "rotate");
        assert_eq!(BlacklistReason::ForceLogout.as_str(), "force_logout");
        // 列宽约束：token_kind varchar(8)、reason varchar(32)
        for kind in [BlacklistKind::Access, BlacklistKind::Refresh] {
            assert!(kind.as_str().len() <= 8);
        }
        for reason in [
            BlacklistReason::Logout,
            BlacklistReason::Rotate,
            BlacklistReason::ForceLogout,
        ] {
            assert!(reason.as_str().len() <= 32);
        }
    }

    /// 空 `jti` 不写库也不报错（没有 jti 的令牌本来无法复检）。
    #[tokio::test]
    async fn blank_jti_is_a_noop() {
        assert!(
            revoke(
                "  ",
                None,
                BlacklistKind::Access,
                BlacklistReason::Logout,
                4_000_000_000
            )
            .await
            .is_ok()
        );
        assert!(!is_revoked("").await.unwrap_or(false));
    }
}
