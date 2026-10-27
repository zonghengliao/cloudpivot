//! 服务器运行状态（二开模块 `business/monitor`）。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET | `/api/business/monitor/server-status` | `business:monitor:server-status` |
//!
//! > 处理器宏里的路径**不含** `/api`，前缀由 `config.toml` 的 `[router] prefix` 注入。
//!
//! # 设计约束
//!
//! - **只读旁路**：不写表、不加锁，全部指标现场采集，采样失败降级为 `down` / `0`，
//!   绝不因某个外部依赖抖动把整个接口打成 500（工作台右栏不能因为 Redis 掉了就白屏）。
//! - **仅超管可见**：权限码只授予 `SUPER_ADMIN` 角色（见迁移 `V0020`），
//!   非超管调用 `#[alun::permission]` 校验层直接 403。
//! - **安全红线**：响应只含聚合数值与版本号，不含数据库密码 / 连接串 / Redis 密码 /
//!   内部路径 / 主机地址。

pub mod dto;
pub mod handler;
pub mod model;
