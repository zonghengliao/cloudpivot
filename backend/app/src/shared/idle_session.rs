//! 空闲超时（自动退出登录）的会话活动时间戳。
//!
//! # 为什么需要这一层
//!
//! 需求原话（2026-11-10）：「要有自动退出登录……防止非法使用」。前端做空闲检测
//! 只能防「人不在了但浏览器还开着」；它**挡不住**「改 localStorage / 禁 JS / 直接
//! curl 拿旧 token」。因此服务端必须自己记「这个会话最后一次**用户操作**是什么时候」，
//! 超过 `sys_config.auth.idle_timeout_minutes` 未操作即拒绝一切请求（401
//! [`crate::shared::error_code::AUTH_IDLE_TIMEOUT`]）。
//!
//! # 存什么、存在哪里
//!
//! - 键：`idle:sid:{sid}`（`sid` 是登录时生成、轮换时原样继承的会话标识
//!   [`crate::shared::constants::CLAIM_SESSION_ID`]）；
//! - 值：最后一次用户操作的 Unix 秒（纯文本，`redis-cli GET` 直接可读）；
//! - TTL：[`IDLE_KEY_TTL_SECS`]（7 天，与 refresh 会话绝对上限同阶）。
//!
//! 键按**会话**而非按令牌：一次登录内的 refresh 轮换共享同一个 `sid`，
//! 不会因为换令牌把空闲计时重置。
//!
//! # 什么算「用户操作」（关键）
//!
//! 只认两类信号，缺一不可地排除后台轮询：
//!
//! 1. 请求头 [`HEADER_USER_ACTIVE`] = `1`：前端在「最近有过 DOM 事件（鼠标 / 键盘 /
//!    滚动 / 触摸 / 路由切换）」时给请求打标；首页未读轮询、看板自动刷新**不打标**；
//! 2. 写方法（`POST` / `PUT` / `PATCH` / `DELETE`）：本系统的轮询全是 `GET`，
//!    写请求必然是人在操作。
//!
//! 其余 `GET` 一律只**校验**不**续期** —— 这是「打开首页挂机 2 小时也必须退出」
//! （卡面验收 ⑪）能成立的前提。
//!
//! # 降级语义
//!
//! 缓存不可用（无 Redis / 未初始化 / 读错误）时**放行**：本模块只是会话安全的最小
//! 增量，不能因为缓存抖动把整站变成 401（吊销状态另有库侧真源，
//! 见 [`crate::shared::token_revocation`]）。放行时打 `warn` 日志，方便运维发现
//! 「空闲超时暂时失效」。
//!
//! # 无开关
//!
//! 这里没有「关闭校验」的独立开关：禁用只能靠超管把参数设成 `0`
//! （已审计，见 `support::config::handler`）。

use axum::http::{HeaderMap, Method};
use nik::prelude::*;

use crate::shared::constants;
use crate::support::config::model::SysConfigModel;

/// 空闲时间戳缓存键前缀（完整键形如 `idle:sid:{sid}`）。
pub(crate) const CACHE_KEY_PREFIX_IDLE: &str = "idle:sid:";

/// 键保留时长（秒）：7 天。
///
/// 必须**远大于**空闲超时上限（60 分钟），否则「用户走了几天再回来」会因为键过期
/// 被当成「没有记录 → 首次放行」，等于超时不生效。取 7 天与 refresh 会话绝对上限
/// 同阶：更久之后会话本来也不能续期了。
pub(crate) const IDLE_KEY_TTL_SECS: u64 = 7 * 24 * 3600;

/// 前端用来声明「本次请求由用户操作触发」的请求头名。
///
/// 值必须为 `1`（其它值一律不算，避免误传 `0` / `false` 反而续期）。
pub const HEADER_USER_ACTIVE: &str = "x-user-active";

/// 空闲判定的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 会话仍活跃（或本模块降级放行）。
    Active,
    /// 空闲超过阈值：调用方必须 401 并清掉会话。
    Idle {
        /// 已空闲的秒数（仅用于日志 / 错误文案）。
        idle_secs: i64,
    },
}

/// 拼出会话的空闲时间戳缓存键。
///
/// # 参数
///
/// - `sid`：会话标识（令牌 claim `sid`）。
///
/// # 返回值
///
/// `idle:sid:{sid}`。
pub(crate) fn cache_key(sid: &str) -> String {
    format!("{CACHE_KEY_PREFIX_IDLE}{sid}")
}

/// 把从配置读到的原始值规整成生效的空闲超时分钟数。
///
/// # 参数
///
/// - `raw`：`sys_config.auth.idle_timeout_minutes` 的解析结果；`None` 表示参数
///   缺失 / 被停用 / 不是整数。
///
/// # 返回值
///
/// - `0`：禁用自动退出（超管显式设置，原样保留）；
/// - `1..=60`：生效值；
/// - 其余（负数 / 超上限 / 脏值）：回退 [`constants::DEFAULT_IDLE_TIMEOUT_MINUTES`]。
///
/// 脏值回退默认值而不是回退 0，是刻意的**安全默认**：宁可多踢一次，也不能因为
/// 有人往库里写了个 `-1` 就让功能整体失效。
pub(crate) fn normalise_timeout(raw: Option<i64>) -> i64 {
    match raw {
        Some(value) if (0..=constants::MAX_IDLE_TIMEOUT_MINUTES).contains(&value) => value,
        _ => constants::DEFAULT_IDLE_TIMEOUT_MINUTES,
    }
}

/// 读取当前生效的空闲超时分钟数（走参数快照，未命中回源数据库）。
///
/// # 返回值
///
/// `0` = 禁用；否则为分钟数。读库失败按参数「不存在」处理（回退默认值）。
pub(crate) async fn timeout_minutes() -> i64 {
    normalise_timeout(SysConfigModel::read_i64(constants::CONFIG_KEY_IDLE_TIMEOUT).await)
}

/// 「即将退出」提前提示窗口的下限（秒）。
///
/// 超时设为 1 分钟时窗口不能是 60 秒（那等于一分钟内全程在提示），取 20 秒：
/// 用户有一整个阶段 1 可以点「继续使用」。
pub(crate) const MIN_WARN_SECS: i64 = 20;

/// 「即将退出」提前提示窗口的上限（秒）：一分钟。
pub(crate) const MAX_WARN_SECS: i64 = 60;

/// 计算「即将退出」提示提前多少秒出现。
///
/// # 参数
///
/// - `timeout_minutes`：生效的空闲超时分钟数（`0` = 禁用）。
///
/// # 返回值
///
/// 生效窗口的 1/5，钳在 [`MIN_WARN_SECS`]..=[`MAX_WARN_SECS`]；禁用时返回 `0`。
///
/// 取 1/5 而不是固定 60 秒：超时设成 5 分钟时提前 60 秒提示会「刚操作完就弹窗」，
/// 反而像故障；按比例给窗口，最短也留 20 秒可点「继续使用」。
pub fn warn_seconds(timeout_minutes: i64) -> i64 {
    if timeout_minutes <= 0 {
        return 0;
    }
    (timeout_minutes * 60 / 5).clamp(MIN_WARN_SECS, MAX_WARN_SECS)
}

/// 本次请求是否由「用户操作」触发（决定要不要刷新空闲计时）。
///
/// # 参数
///
/// - `method`：HTTP 方法；
/// - `headers`：请求头。
///
/// # 返回值
///
/// 命中 [`HEADER_USER_ACTIVE`]（值为 `1`）或是写方法时返回 `true`。
///
/// # 为什么写方法也算
///
/// 本系统的自动轮询（首页指标 / 未读数 / 看板）全部是 `GET`；`POST` / `PUT` /
/// `PATCH` / `DELETE` 都是人点出来的动作。把它们计入可以让「只用表单不移动鼠标」
/// 的场景（例如长表单里连续 keyboard 输入后直接提交）不会被误踢。
pub(crate) fn is_user_activity(method: &Method, headers: &HeaderMap) -> bool {
    let flagged = headers
        .get(HEADER_USER_ACTIVE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim() == "1")
        .unwrap_or(false);
    if flagged {
        return true;
    }
    matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE)
}

/// 校验会话是否还在空闲窗口内，并在确属用户操作时刷新时间戳。
///
/// # 参数
///
/// - `sid`：会话标识（缺 `sid` 的旧令牌调用方应直接跳过本检查）；
/// - `user_active`：本次请求是否算用户操作（见 [`is_user_activity`]）。
///
/// # 返回值
///
/// [`Verdict::Idle`] 表示已超时（调用方必须 401）；其余情况返回 [`Verdict::Active`]。
///
/// # 副作用
///
/// - 首次见到该 `sid`（上线前签发的会话 / 键被回收）时写入当前时刻并放行；
/// - `user_active = true` 且未超时时刷新时间戳；
/// - 缓存不可用 / 读失败：放行（见模块头「降级语义」）。
pub(crate) async fn check_and_touch(sid: &str, user_active: bool) -> Verdict {
    let timeout = timeout_minutes().await;
    // 0 = 超管显式禁用：不记录、不拦截
    if timeout <= 0 {
        return Verdict::Active;
    }
    let Some(store) = try_cache() else {
        tracing::warn!("空闲超时校验降级：未注册缓存后端，本次请求按放行处理（自动退出登录暂时不生效）");
        return Verdict::Active;
    };

    let key = cache_key(sid);
    let now = chrono::Utc::now().timestamp();
    match Cache::get::<String>(store, &key).await {
        Ok(Some(raw)) => {
            let Some(last_active) = raw.trim().parse::<i64>().ok() else {
                tracing::warn!(%key, raw, "空闲时间戳格式不认识，按「无记录」重置");
                touch(store, &key, now).await;
                return Verdict::Active;
            };
            let idle_secs = now.saturating_sub(last_active);
            if idle_secs > timeout.saturating_mul(60) {
                return Verdict::Idle { idle_secs };
            }
            if user_active {
                touch(store, &key, now).await;
            }
            Verdict::Active
        }
        Ok(None) => {
            // 没有记录：上线前签发的会话 / 键已被回收。写一个当前时刻后放行，
            // 从这一刻起才开始计时（否则老会话会在上线瞬间被全体踢下线）。
            touch(store, &key, now).await;
            Verdict::Active
        }
        Err(error) => {
            tracing::warn!(%key, error = %error, "读空闲时间戳失败，本次按放行处理");
            Verdict::Active
        }
    }
}

/// 写入 / 刷新某会话的空闲时间戳（失败只告警：下次请求会再写一次）。
///
/// # 参数
///
/// - `store`：缓存后端（`SharedCache` 实现了 [`Cache`] 的任一实现）；
/// - `key`：缓存键；
/// - `now`：当前 Unix 秒。
async fn touch<C: Cache + ?Sized>(store: &C, key: &str, now: i64) {
    let value = now.to_string();
    if let Err(error) = store.set_ex(key, &value, IDLE_KEY_TTL_SECS).await {
        tracing::warn!(%key, error = %error, "写空闲时间戳失败（下次请求会重试）");
    }
}

/// 登出 / 强制下线时清掉会话的空闲记录。
///
/// # 参数
///
/// - `sid`：会话标识。
///
/// # 说明
///
/// 只做清理，**不参与判定**：泄露的旧令牌靠吊销名单拦（
/// [`crate::shared::token_revocation`]），空闲记录删不删都不影响「登出后旧 token 401」。
/// 删除失败只打日志，不影响登出结果。
pub(crate) async fn forget(sid: &str) {
    let Some(store) = try_cache() else {
        return;
    };
    let key = cache_key(sid);
    if let Err(error) = Cache::del(store, &key).await {
        tracing::warn!(%key, error = %error, "清理空闲时间戳失败");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺省 / 脏值一律回退「默认 15 分钟」，且**绝不**回退成 0（0 = 关闭功能）。
    #[test]
    fn normalise_falls_back_to_default_not_zero() {
        assert_eq!(normalise_timeout(None), 15);
        assert_eq!(normalise_timeout(Some(-1)), 15, "负数必须是回退默认值");
        assert_eq!(normalise_timeout(Some(1441)), 15, "超上限必须是回退默认值");
        assert_eq!(normalise_timeout(Some(i64::MAX)), 15);
    }

    /// 合法取值原样保留（含超管专属的 0），上限 60 —— 超管也设不了 70/120/1440。
    #[test]
    fn normalise_keeps_valid_values() {
        for value in [0, 1, 15, 30, 60] {
            assert_eq!(normalise_timeout(Some(value)), value, "value={value}");
        }
        // 用户 2026-11-10：「最大不能超过 60 分钟」—— 超过上限一律回退默认值，
        // 不能因为库里被塞了 1440 就让「一天不退出」变合法
        assert_eq!(normalise_timeout(Some(61)), 15, "61 分钟超上限");
        assert_eq!(normalise_timeout(Some(1440)), 15, "1440 分钟超上限");
    }

    /// 提示窗口随超时按比例缩放，且始终落在 20..=60 秒；禁用时不提示。
    #[test]
    fn warn_window_scales_and_clamps() {
        assert_eq!(warn_seconds(0), 0, "禁用时不提示");
        assert_eq!(warn_seconds(1), 20, "1 分钟 → 下限 20 秒");
        assert_eq!(warn_seconds(2), 24);
        assert_eq!(warn_seconds(5), 60, "5 分钟 → 上限 60 秒");
        assert_eq!(warn_seconds(15), 60);
        assert_eq!(warn_seconds(60), 60, "60 分钟也只提前 60 秒");
    }

    /// 缓存键必须带 `idle:sid:` 前缀且含会话标识（便于按前缀清空）。
    #[test]
    fn cache_key_shape() {
        let key = cache_key("abc-123");
        assert_eq!(key, "idle:sid:abc-123");
        assert!(key.starts_with(CACHE_KEY_PREFIX_IDLE));
    }

    /// 键保留时长必须远大于超时上限（否则「走了很久再回来」会被当成无记录放行）。
    #[test]
    fn key_ttl_outlives_max_timeout() {
        let max_timeout_secs = (constants::MAX_IDLE_TIMEOUT_MINUTES as u64) * 60;
        assert!(
            IDLE_KEY_TTL_SECS > max_timeout_secs * 2,
            "ttl={IDLE_KEY_TTL_SECS} max_timeout={max_timeout_secs} —— 键必须比超时窗口活得久"
        );
    }

    /// 用户操作判定：只有显式 `x-user-active: 1` 与写方法算操作。
    #[test]
    fn user_activity_detection() {
        let mut headers = HeaderMap::new();
        assert!(!is_user_activity(&Method::GET, &headers), "轮询 GET 不算操作");
        headers.insert(HEADER_USER_ACTIVE, "1".parse().unwrap());
        assert!(is_user_activity(&Method::GET, &headers), "打标 GET 算操作");
        headers.insert(HEADER_USER_ACTIVE, "0".parse().unwrap());
        assert!(!is_user_activity(&Method::GET, &headers), "0 不算操作");
        headers.insert(HEADER_USER_ACTIVE, "false".parse().unwrap());
        assert!(!is_user_activity(&Method::GET, &headers), "非 1 不算操作");
    }

    /// 写方法即使不打标也算用户操作（表单提交不一定伴随鼠标移动）。
    #[test]
    fn write_methods_count_as_activity() {
        let headers = HeaderMap::new();
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(is_user_activity(&method, &headers), "{method} 应算用户操作");
        }
        assert!(!is_user_activity(&Method::OPTIONS, &headers));
    }
}
