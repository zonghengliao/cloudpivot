//! 跨模块共享的类型与常量。
//!
//! 存放被多个业务模块复用的定义，避免在各模块重复声明或形成循环引用。
//!
//! | 项 | 内容 | 交付阶段 |
//! |----|------|----------|
//! | [`ai_actor`] | AI 执行者（`user_type='ROBOT'`）的硬拒判定：禁登录 / 禁删任务 / 禁改数据权限 / 禁管 AI 用户 | 批次 C+ · T5 |
//! | [`cache_gate`] | 缓存失效「欠账」闸门：失效失败时跳过缓存并补失效（BUG-SYS-03） | 批次 G · FIX-03 |
//! | [`constants`] | 通用业务常量（状态枚举值、默认分页、超级管理员标识、字典 / 模块编码） | P1 |
//! | [`error_code`] | 业务错误码常量（映射到 `Res::fail` / `ApiError`） | P1 |
//! | [`enums`] | 后端下发枚举的统一词汇层（`EnumItem` / `EnumsRes` / 配色常量） | P1 |
//! | [`session_cache`] | 会话安全字段（`token_version` / 首登改密）的 60 s 缓存 + 变更即失效（FIX-05） | 批次 G · FIX-05 |
//! | [`startup_guard`] | 启动守卫：认证已开启而 JWT 签名密钥为空 / 过短时拒绝启动 | P0 安全 |

pub mod ai_actor;
pub mod cache_gate;
pub mod constants;
pub mod enums;
pub mod error_code;
pub mod session_cache;
pub mod startup_guard;
