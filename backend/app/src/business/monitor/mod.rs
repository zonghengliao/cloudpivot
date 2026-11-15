//! 服务器运行状态（二开模块 `business/monitor`）。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/api/business/monitor/server-status` | `business:monitor:server-status` | 页面数据 |
//! | GET | `/api/system/server-status/diagnostic` | `system:server-status:view` | 诊断包 zip（仅超管） |
//!
//! > 处理器宏里的路径**不含** `/api`，前缀由 `config.toml` 的 `[router] prefix` 注入。
//!
//! # 设计约束
//!
//! - **只读旁路**：不写业务表、不加锁，全部指标现场采集，采样失败降级为 `down` / `0`，
//!   绝不因某个外部依赖抖动把整个接口打成 500（服务器状态页最需要被看到的恰恰是
//!   「数据库/Redis 掉了」，此时把页面打成错误页等于藏起关键信息）。
//! - **仅超管可见**：两个权限码都只授予 `SUPER_ADMIN`（`business:monitor:server-status` 见
//!   `V0020`；`system:server-status:view` 见菜单种子 `S__menu_data.sql` + `S__role_data.sql`），
//!   非超管调用 `#[nik::permission]` 校验层直接 403；诊断包另有
//!   `dto::ensure_super_admin` 第二层判定。
//! - **安全红线**：状态响应只含聚合数值与版本号，不含数据库密码 / 连接串 / Redis 密码 /
//!   内部路径 / 主机地址；诊断包含配置文件与日志，但**一律打码**（`diagnostic::redact`，
//!   有单测盯住「环境变量 / TOML / JSON / URL 凭据 / Bearer / JWT / 长十六进制」六种形态）。
//!
//! # 文件
//!
//! | 文件 | 内容 |
//! |------|------|
//! | [`dto`] | 响应结构与超管判定 |
//! | [`model`] | 采集（CPU / 内存 / 磁盘 / DB / Redis / 主机 / 进程 / 业务量 / Agent 探测）与纯解析函数 |
//! | [`diagnostic`] | 诊断包 zip 组装、打码、日志尾、`/metrics` 快照 |
//! | [`handler`] | 路由 |

pub mod diagnostic;
pub mod dto;
pub mod handler;
pub mod model;
