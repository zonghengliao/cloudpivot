//! 菜芦通用后台管理系统 —— 后端服务。
//!
//! # 分层结构
//!
//! ```text
//! src/
//! ├── system/       # 业务容器一：系统管理模块（认证、用户、角色、菜单、部门）
//! ├── support/      # 业务容器二：支撑功能模块（健康检查、字典、参数、文件、日志）
//! ├── middleware/   # 自定义中间件（横切关注点，位于业务容器之外）
//! ├── task/         # 定时/后台任务（横切关注点，位于业务容器之外）
//! ├── utils/        # 项目内纯工具函数（横切关注点，位于业务容器之外）
//! ├── shared/       # 跨模块共享的类型与常量（横切关注点，位于业务容器之外）
//! └── bootstrap.rs  # 启动引导：全局资源就绪后的业务初始化
//! ```
//!
//! # 约定
//!
//! - `system/` 与 `support/` 是**业务容器**，其下每个业务子模块统一遵循
//!   `mod.rs + handler.rs + dto.rs + model.rs` 结构，**不设 service 层**：
//!   业务编排写在 `handler.rs`，数据访问与领域行为写在 `model.rs`。
//! - 中间件、任务、工具、共享类型等横切关注点一律放在业务容器**之外**，避免业务模块
//!   被迫依赖框架装配细节。
//! - 路由使用 `#[alun::get]` / `#[alun::post]` 等宏在处理器上声明，由 [`run`] 中的
//!   `App::scan()` 在启动时统一收集，无需手工注册。
//!
//! # 环境要求
//!
//! Rust 1.94.1+ / edition 2024 / PostgreSQL（主库）。

use alun::prelude::*;

pub mod bootstrap;
pub mod middleware;
pub mod shared;
pub mod support;
pub mod system;
pub mod task;
pub mod utils;

/// 构建并启动 HTTP 服务。
///
/// 启动链按固定顺序装配：
///
/// 1. **启动预检**（[`shared::startup_guard::preflight`]）：在框架初始化任何资源之前，
///    先拒绝「认证已开启但 JWT 签名密钥为空 / 过短」的启动，避免裸 `cargo run` 先死在
///    数据库连接失败、把真正原因掩盖掉；
/// 2. [`App::new`] 读取 `config/config.toml` 与对应 profile 覆盖，并初始化日志；
/// 3. [`App::parse_cli`] 解析 `gen-config` / `print-config` / `profile=xxx` 等命令行参数；
/// 4. [`App::on_startup`] 注册启动钩子，在全局资源就绪、插件启动之前执行业务初始化
///    （其中包含从 `sys_job` 装载定时任务）；
/// 5. [`App::plugin`] 注册自定义插件：启动守卫（[`shared::startup_guard::AuthSecretGuardPlugin`]，
///    用框架权威配置再校验一次，认证已开启而密钥为空 / 过短时在此中止启动）、日志总线
///    （[`support::log::plugin::LogPlugin`]）与调度器（[`task::scheduler`]）；
/// 6. [`App::layer_from_fn`] 注入操作日志中间件（链**外**层：覆盖整站，含 401 / 403 / 429
///    与 404 兜底）；
/// 7. [`App::scan`] 收集宏声明的路由；
/// 8. [`App::start`] 初始化数据库/缓存等资源、启动插件并监听 `[server].listen`。
///
/// # 返回值
///
/// 成功时在服务进程正常退出后返回 `Ok(())`；配置缺失、资源初始化失败或端口占用等
/// 情况返回 [`alun::Error`]，由调用方决定退出码。
///
/// # 异常情况
///
/// 本函数不 panic：所有错误均通过 `Result` 向上传播，禁止 `unwrap` / `expect`。
pub async fn run() -> Result<()> {
    // 启动预检：必须在 App::new() 之前。App::start() 的顺序是「先连数据库/缓存 →
    // 再挂启动钩子 → 再启插件」，而裸 `cargo run`（没注入 ALUN_MIDDLEWARE_AUTH_JWT_SECRET）
    // 会先死在数据库连接失败上，掩盖「密钥未注入」这个真正原因。
    if let Err(violation) = shared::startup_guard::preflight() {
        tracing::error!("{violation}");
        return Err(Error::Config(violation.to_string()));
    }

    App::new()?
        .parse_cli()
        // 根路径存活探针：手动注册以豁免 `[router] prefix`，保持在 `/health`
        .get("/health", support::health::health)
        .on_startup(bootstrap::bootstrap)
        // 启动守卫必须**注册在最前**：插件按注册顺序（无依赖者）依次启动，
        // 密钥不合格时会在其余插件启动之前就中止启动，绝不带着空签名密钥对外提供服务
        .plugin(shared::startup_guard::AuthSecretGuardPlugin::new())
        .plugin(support::log::plugin::LogPlugin::new())
        .plugin(task::scheduler())
        .layer_from_fn(middleware::operation_log::layer)
        .scan()
        .start()
        .await
}
