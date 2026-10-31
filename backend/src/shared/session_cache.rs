//! 会话安全字段的短 TTL 缓存（`token_version` / `must_change_password`）。
//!
//! # 为什么需要（FIX-05 / BUG-SYS-04）
//!
//! [`crate::middleware::session_guard`] **每个请求**都要读一次
//! `sys_user.token_version`（权限撤销后旧令牌必须立刻失效，这是唯一的库侧真源）。
//! 一次主键点查本身很便宜，但它是全站热路径（含静态资源之外的一切 API），
//! 因此按卡面口径加一层 **Redis 短 TTL 缓存**（`uv:{user_id}`，默认 60 s）。
//!
//! # 安全语义（缓存绝不能变成旁路）
//!
//! - **库是唯一真源**：缓存只缓存「从库里读到的值」，不参与任何改判；
//! - **变更即失效（写穿删除）**：所有推进 `token_version` 的地方（改密 / 改角色 /
//!   改角色权限 / 改菜单 / 停用 / 删除）在**事务提交之后**删除对应缓存键
//!   （[`invalidate`] / [`invalidate_all`]），因此撤销权限后旧令牌在**同一时刻**就失效，
//!   不需要等 TTL 自然过期；
//! - **失效失败 → 本实例立刻弃用缓存**：`DEL` 出错时把 [`InvalidationGate`] 置「欠账」，
//!   之后本进程读会话字段一律直查库（fail-closed），直到补失效成功；
//! - **缓存不可用（无 Redis / 未初始化）→ 直接查库**：功能完全不受影响，只是慢一点。
//!
//! 残留窗口只有一种：多实例部署下 A 实例的 `DEL` 失败时，B 实例可能继续命中旧值
//! 直到 TTL（60 s）到期。这一窗口是「卡面明确接受的缓存代价」，且**所有写操作都在
//! 库侧同一事务里推进了版本号**，一旦缓存过期就必然拒绝；
//! 单实例（本仓默认部署形态）与 `DEL` 成功的场景窗口为 0。
//!
//! # 没有开关
//!
//! 这里**没有**任何「关闭校验」的配置项：`token_version` 比对在
//! [`crate::middleware::session_guard`] 里无条件执行，本模块只决定「这次比对的值从哪里读」。

use nik::prelude::*;

use crate::shared::cache_gate::InvalidationGate;

/// 缓存键前缀（`uv` = user version，卡面口径 `uv:{user_id}`）。
pub(crate) const CACHE_KEY_PREFIX: &str = "uv:";

/// 缓存 TTL（秒）。60 s 是卡面口径：既能把「每请求一次点查」降为「每分钟一次」，
/// 又能把最坏残留窗口压到一分钟以内。
pub(crate) const CACHE_TTL_SECS: u64 = 60;

/// 失效「欠账」闸门（`DEL` 失败 → 本进程不再读缓存）。
static GATE: InvalidationGate = InvalidationGate::new();

/// 拼出某个用户的缓存键。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 返回值
///
/// `uv:{user_id}`。
pub(crate) fn cache_key(user_id: &uuid::Uuid) -> String {
    format!("{CACHE_KEY_PREFIX}{user_id}")
}

/// 把会话字段编码成缓存值（`{token_version}:{0|1}`）。
///
/// 用纯文本而不是 JSON：值只有两个标量，纯文本省一次 serde 解析，
/// 也让 `redis-cli GET uv:<id>` 直接可读（排障友好）。
fn encode(token_version: i32, must_change_password: bool) -> String {
    format!("{token_version}:{}", i32::from(must_change_password))
}

/// 解析缓存值；格式不认识时返回 `None`（调用方按「未命中」处理并回查库）。
fn decode(raw: &str) -> Option<(i32, bool)> {
    let (version, flag) = raw.split_once(':')?;
    let version = version.trim().parse::<i32>().ok()?;
    match flag.trim() {
        "0" => Some((version, false)),
        "1" => Some((version, true)),
        _ => None,
    }
}

/// 读会话安全字段：缓存优先，未命中 / 缓存不可用 / 欠账时回查数据库。
///
/// # 参数
///
/// - `user_id`：令牌主体。
///
/// # 返回值
///
/// `Some((token_version, must_change_password))`；用户不存在（已删除）返回 `None`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（缓存侧错误一律降级为查库，不向上传播）。
pub(crate) async fn load_session_flags(
    user_id: &uuid::Uuid,
) -> std::result::Result<Option<(i32, bool)>, DbError> {
    let key = cache_key(user_id);
    // 缓存只是加速：读失败、未命中、闸门欠账都直接回查库（库是唯一真源）
    if GATE.cache_readable()
        && let Some(store) = try_cache()
    {
        match Cache::get::<String>(store, &key).await {
            Ok(Some(raw)) => {
                if let Some(flags) = decode(&raw) {
                    return Ok(Some(flags));
                }
                tracing::warn!(key, raw, "会话缓存值格式不认识，忽略并回查数据库");
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(key, error = %error, "读会话缓存失败，回退查库");
            }
        }
    }

    let flags = crate::system::user::model::load_session_flags(user_id).await?;
    if let Some((token_version, must_change_password)) = flags {
        fill(&key, token_version, must_change_password).await;
    }
    Ok(flags)
}

/// 把从库里读到的值写回缓存（失败只告警：缓存不参与判权，写不进去就是多查几次库）。
///
/// # 参数
///
/// - `key`：缓存键；
/// - `token_version`：库里的版本号；
/// - `must_change_password`：库里的首登改密标记。
async fn fill(key: &str, token_version: i32, must_change_password: bool) {
    if !GATE.cache_readable() {
        // 欠账期间不回填：否则会把「可能已过期的值」重新写回缓存
        return;
    }
    let Some(store) = try_cache() else {
        return;
    };
    let raw = encode(token_version, must_change_password);
    if let Err(error) = Cache::set_ex(store, key, &raw, CACHE_TTL_SECS).await {
        tracing::warn!(key, error = %error, "写会话缓存失败（不影响鉴权：DB 是唯一真源）");
    }
}

/// 删除**一个**用户的会话字段缓存（该用户的 `token_version` 刚被推进后调用）。
///
/// # 参数
///
/// - `user_id`：目标用户。
///
/// # 说明
///
/// 必须在业务事务**提交之后**调用：在事务内删，别的实例可能读到未提交的旧值并把它
/// 回填进缓存，提交后就变成「旧值又活了 60 s」。
pub(crate) async fn invalidate(user_id: &uuid::Uuid) {
    let key = cache_key(user_id);
    let Some(store) = try_cache() else {
        // 没有缓存就没有脏值
        GATE.record(true);
        return;
    };
    match Cache::del(store, &key).await {
        Ok(()) => GATE.record(true),
        Err(error) => {
            GATE.record(false);
            tracing::warn!(
                key,
                error = %error,
                cache_invalidation_pending = true,
                "失效会话缓存失败：本实例改为直查库（旧令牌仍会因库侧版本号不符被 401）"
            );
        }
    }
}

/// 清空**全部**会话字段缓存（批量推进 `token_version` 之后调用）。
///
/// # 何时使用
///
/// 「改菜单 / 改角色授权」这类操作会推进**一批**用户的版本号（
/// [`crate::system::user::model::bump_token_version_of_menu_holders_in`] /
/// `bump_token_version_of_role_holders_in`），而模型层在事务里只拿到受影响行数。
/// 与其把用户 ID 列表一路搬出事务（易错），不如整命名空间清空：
///
/// - 这类操作是**低频管理动作**（改菜单 / 改角色授权），不是热路径；
/// - 键空间只包含「最近 60 s 内有请求的用户」，规模与在线用户数同阶；
/// - 语义上最安全：宁可让所有人多查一次库，也不能让任何人继续命中旧版本号。
pub(crate) async fn invalidate_all() {
    let Some(store) = try_cache() else {
        GATE.record(true);
        return;
    };
    let pattern = format!("{CACHE_KEY_PREFIX}*");
    match Cache::delete_pattern(store, &pattern).await {
        Ok(removed) => {
            GATE.record(true);
            tracing::info!(removed, pattern, "权限变更后清空会话字段缓存");
        }
        Err(error) => {
            GATE.record(false);
            tracing::warn!(
                pattern,
                error = %error,
                cache_invalidation_pending = true,
                "批量失效会话缓存失败：本实例改为直查库（旧令牌仍会因库侧版本号不符被 401）"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编码 / 解析必须一一对应（否则会话守卫会拿错版本号 → 误踢或漏踢）。
    #[test]
    fn encode_decode_roundtrip() {
        for (version, must) in [(0, false), (0, true), (7, false), (123, true)] {
            let raw = encode(version, must);
            assert_eq!(decode(&raw), Some((version, must)), "raw={raw}");
        }
    }

    /// 认不出的缓存值必须被当成「未命中」（回查库），不能瞎猜一个版本号。
    #[test]
    fn decode_rejects_garbage() {
        for raw in ["", "abc", "1:", ":1", "1:2", "x:1", "1:true"] {
            assert_eq!(decode(raw), None, "raw={raw} 应判为不可用");
        }
    }

    /// 缓存键必须带 `uv:` 前缀且含用户 ID（卡面口径，便于 `delete_pattern("uv:*")`）。
    #[test]
    fn cache_key_shape() {
        let id = uuid::Uuid::now_v7();
        let key = cache_key(&id);
        assert!(key.starts_with(CACHE_KEY_PREFIX), "key={key}");
        assert!(key.ends_with(&id.to_string()), "key={key}");
    }

    /// TTL 必须是卡面口径的 60 s：改大 = 撤销窗口变长，改小 = 缓存失去意义。
    #[test]
    fn ttl_matches_card() {
        assert_eq!(CACHE_TTL_SECS, 60);
    }

    /// lib 单测环境没有注册缓存（不跑 `App::start()`）：所有读写都必须降级为「不碰缓存」，
    /// 且失效动作不能把闸门误置成「欠账」（否则会把「没有缓存」当成「缓存失效失败」）。
    #[tokio::test]
    async fn no_cache_backend_degrades_without_dirtying_gate() {
        assert!(
            try_cache().is_none(),
            "lib 单测不应注册全局缓存（若这里变红，说明有用例注册了缓存，需改断言）"
        );
        let id = uuid::Uuid::now_v7();
        invalidate(&id).await;
        invalidate_all().await;
        assert!(
            GATE.cache_readable(),
            "没有缓存后端时失效必然「成功」（没有脏值），闸门不该变脏"
        );
    }
}
