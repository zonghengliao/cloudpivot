//! 健康检查接口。
//!
//! 两个端点，语义**刻意分开**（ADR-20261101 §5.4）：
//!
//! | 端点 | 语义 | 状态码 |
//! |------|------|--------|
//! | `GET /health` | **liveness**：进程是否活着。不鉴权、不查库、不看缓存 | 永远 200 |
//! | `GET /health/deps` | **readiness**：依赖（PostgreSQL / 缓存）是否健康 | 全健康 200，任一不健康 **503** |
//!
//! # 为什么 `/health` 不反映依赖状态
//!
//! 三个具体后果（ADR §5.4）：① 一旦有人给 backend 加 `healthcheck`，Redis 抖动
//! 就会变成「容器被反复重启」，把可用性事故放大成数据不可用；② `scripts/dev-*.sh`
//! 依赖它判断「后端活着」，混合依赖状态后这些脚本语义失效；③ 单端点探针无法区分
//! 「进程挂了」与「依赖挂了」，而这恰恰是要区分的两件事。
//!
//! # `/health/deps` 里的 `auth.revocation_source`
//!
//! 恒为 `"db"`：自 ADR-20261101 起令牌吊销判定以 PostgreSQL 为准（`sys_token_blacklist`），
//! 缓存只做加速。这个字段存在的意义是让运维一眼确认「降级时吊销判定没有旁路」。

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use std::time::Instant;

/// 健康检查响应体。
#[derive(serde::Serialize)]
pub struct HealthRes {
    /// 服务状态，固定为 `ok`。
    pub status: String,
    /// 应用名称，取自配置 `app_name`。
    pub app: String,
    /// 当前生效的配置 profile。
    pub profile: String,
    /// 服务端当前时间（RFC 3339）。
    pub time: String,
}

/// 存活探针：返回服务状态与运行环境信息。
///
/// # 装配
///
/// 本接口需要在**全局前缀之外**（根路径 `/health`）暴露，因此不使用
/// `#[nik::get]` 宏（宏声明的路由会被 `[router] prefix` 嵌套），
/// 改由 [`crate::run`] 通过 `App::get("/health", ...)` 手动注册。
///
/// 免认证由 `config.toml` 的 `[middleware.auth].ignore_paths` 中的 `"!/health"`
/// 承担（`!` 表示绝对路径，不参与前缀拼接）。
///
/// # 返回值
///
/// 始终返回 `code = 0` 的统一响应体，`data` 为 [`HealthRes`]。
pub async fn health() -> Res<HealthRes> {
    Res::ok(HealthRes {
        status: "ok".to_string(),
        app: cfg().app_name.clone(),
        profile: cfg().profile.clone(),
        time: chrono::Utc::now().to_rfc3339(),
    })
}

/// 单个依赖的探测结果。
#[derive(serde::Serialize)]
pub struct DepStatus {
    /// 是否健康。
    pub ok: bool,
    /// 探测耗时（毫秒）；仅成功时给出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    /// 失败原因（一句话，不含连接串 / 口令）；仅失败时给出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DepStatus {
    /// 构造一个成功结果（入参是**秒**，输出保留 3 位小数的毫秒）。
    fn up(elapsed_secs: f64) -> Self {
        let millis = elapsed_secs * 1000.0;
        Self {
            ok: true,
            latency_ms: Some((millis * 1000.0).round() / 1000.0),
            error: None,
        }
    }

    /// 构造一个失败结果。
    fn down(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            latency_ms: None,
            error: Some(error.into()),
        }
    }
}

/// 认证相关的降级状态（给运维看「吊销判定是否有旁路」）。
#[derive(serde::Serialize)]
pub struct AuthDeps {
    /// 吊销判定来源，恒为 `db`。
    pub revocation_source: &'static str,
    /// 认证路径的缓存加速器是否降级（等价于 `cache.ok == false`）。
    pub cache_degraded: bool,
}

/// 依赖探针响应体。
#[derive(serde::Serialize)]
pub struct HealthDepsRes {
    /// `ok` / `degraded`。
    pub status: &'static str,
    /// 数据库依赖。
    pub db: DepStatus,
    /// 缓存依赖。
    pub cache: DepStatus,
    /// 认证相关状态。
    pub auth: AuthDeps,
}

/// 依赖探针（readiness）：`GET /health/deps`。
///
/// # 语义
///
/// - 全部健康 → **200**；
/// - 任一依赖不健康 → **503**（可以直接给 LB 当 readiness，而 `/health` 留给 liveness）。
///
/// # 判定口径
///
/// - `db`：真的发一次 `SELECT 1`（不是「连接池里有句柄」）；
/// - `cache`：`Cache::get` 一次探针键（不返回 `Err` 即健康；键不存在是正常的）。
///   缓存**未初始化**（`try_cache() == None`）算不健康——此时认证路径失去加速器，
///   但吊销判定不受影响（`auth.revocation_source = "db"`）。
///
/// # 返回值
///
/// 手工构造 JSON 响应（不走 `Res` 统一包装）：LB / 抓取端只看状态码与字段，
/// 包一层 `code/data` 反而多一层解包。
pub async fn health_deps() -> Response {
    let started = Instant::now();
    let db_status = match db().find_with_params("SELECT 1 AS ok", vec![]).await {
        Ok(_) => DepStatus::up(started.elapsed().as_secs_f64()),
        Err(error) => DepStatus::down(format!("数据库不可用: {error}")),
    };

    let started = Instant::now();
    let cache_status = match try_cache() {
        None => DepStatus::down("缓存未初始化（进程内无缓存实例）"),
        Some(store) => match Cache::get::<serde_json::Value>(store, "health:deps:probe").await {
            Ok(_) => DepStatus::up(started.elapsed().as_secs_f64()),
            Err(error) => DepStatus::down(format!("缓存不可用: {error}")),
        },
    };

    let cache_ok = cache_status.ok;
    let healthy = db_status.ok && cache_ok;

    // ADR §5.4 第一层告警：这条 gauge 持续 60s = 1 → 告警。
    // 未装 recorder（feature 未开）时是 no-op，可安全常开。
    metrics::gauge!("cloudpivot_auth_cache_degraded").set(if cache_ok { 0.0 } else { 1.0 });

    // 表增长观测（ADR §5.5）：`sys_token_blacklist` 的行只活到令牌过期，日清回收；
    // 行数异常增长意味着清理任务没在跑（点查计划也会跟着退化）。
    // 放在这里而不是每请求路径：readiness 探针是低频的，而 count(*) 在这张
    // 「短命行」表上量级很小（稳态与活跃会话同阶）。查失败不影响健康判定。
    if db_status.ok
        && let Ok(rows) = crate::shared::token_revocation::row_count().await
    {
        metrics::gauge!("cloudpivot_token_blacklist_rows").set(rows as f64);
    }

    let body = HealthDepsRes {
        status: if healthy { "ok" } else { "degraded" },
        db: db_status,
        cache: cache_status,
        auth: AuthDeps {
            revocation_source: "db",
            cache_degraded: !cache_ok,
        },
    };

    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let payload = serde_json::to_string(&body).unwrap_or_else(|_| {
        r#"{"status":"degraded","db":{"ok":false},"cache":{"ok":false},"auth":{"revocation_source":"db","cache_degraded":true}}"#
            .to_string()
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Body::from(payload),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 失败结果不带耗时、成功结果不带错误（JSON 消费方按字段有无判定）。
    #[test]
    fn dep_status_shapes_are_exclusive() {
        let up = DepStatus::up(0.0012);
        assert!(up.ok);
        assert_eq!(up.latency_ms, Some(1.2));
        assert!(up.error.is_none());

        let down = DepStatus::down("boom");
        assert!(!down.ok);
        assert!(down.latency_ms.is_none());
        assert_eq!(down.error.as_deref(), Some("boom"));
    }

    /// 响应体字段名是运维契约（`auth.revocation_source` 恒为 `db`）。
    #[test]
    fn health_deps_payload_shape() {
        let body = HealthDepsRes {
            status: "degraded",
            db: DepStatus::up(1.0),
            cache: DepStatus::down("connection refused"),
            auth: AuthDeps {
                revocation_source: "db",
                cache_degraded: true,
            },
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("序列化")).expect("反序列化");
        assert_eq!(json["status"], "degraded");
        assert_eq!(json["db"]["ok"], true);
        assert_eq!(json["cache"]["ok"], false);
        assert_eq!(json["cache"]["error"], "connection refused");
        assert_eq!(json["auth"]["revocation_source"], "db");
        assert_eq!(json["auth"]["cache_degraded"], true);
    }
}
