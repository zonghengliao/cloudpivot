//! 自定义中间件。
//!
//! 存放框架未内建、且与业务域无关的 HTTP 请求前后处理逻辑。框架已由 `config.toml`
//! 驱动装配了安全头 / CORS / 请求日志 / 认证 / 权限 / 限流 / 压缩等标准中间件，
//! 本目录只补充业务特有需求（如操作日志落库）。
//!
//! 装配方式有两种，能力边界完全不同：
//!
//! - [`crate::run`] 中的 `App::layer_from_fn`：作用于框架默认中间件链与 404 / 静态资源
//!   兜底**之后**，位于最外层（能观测到 401 / 403 / 429 与真实端到端耗时），
//!   **读不到**认证层注入的 `AuthClaims`；
//! - [`crate::run`] 中的 `App::route_layer`：位于内置链**之内**、认证层之后，
//!   能读 `AuthClaims`，用于「已通过认证但会话已作废」这类复检。
//!
//! | 中间件 | 职责 | 挂载方式 |
//! |--------|------|----------|
//! | [`operation_log`] | 记录写操作（方法 / 路径 / 操作人 / 耗时 / 结果）到 `operation_logs` | `layer_from_fn`（链外层） |
//! | [`session_guard`] | 会话守卫：令牌版本比对 + 首登强制改密 | `route_layer`（链内层） |
//! | [`auth_guard`] | 吊销复检：黑名单查询失败即拒绝（fail-closed） | `route_layer`（链内层） |

pub mod auth_guard;
pub mod operation_log;
pub mod session_guard;
