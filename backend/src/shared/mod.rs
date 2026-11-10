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
//! | [`permission_alias`] | 权限码兼容别名：旧码 `erp:contract:*` ↔ 规范码 `business:erp:contract:*` 装载时互扩 | P2-4 |
//! | [`session_cache`] | 会话安全字段（`token_version` / 首登改密）的 60 s 缓存 + 变更即失效（FIX-05） | 批次 G · FIX-05 |
//! | [`startup_guard`] | 启动守卫：认证已开启而 JWT 签名密钥为空 / 过短时拒绝启动 | P0 安全 |
//! | [`token_revocation`] | 令牌吊销的唯一写入口（先库后缓存）与权威判定（只查库，ADR-20261101） | 批次 G · 令牌吊销落库 |

pub mod ai_actor;
pub mod cache_gate;
pub mod constants;
pub mod enums;
pub mod error_code;
pub mod idle_session;
pub mod permission_alias;
pub mod session_cache;
pub mod startup_guard;
pub mod token_revocation;

/// 真库用例的前置体检（TST-02）：`require_seed_data()` —— 空库缺种子时点名缺什么、怎么补。
#[cfg(test)]
pub mod test_helpers;
