//! 业务模块容器（二开业务）。
//!
//! `system/` 与 `support/` 属底座，二开业务一律落在本容器下，每个子模块统一遵循
//! `mod.rs + handler.rs + dto.rs + model.rs` 四件套，不设 service 层。
//!
//! | 子模块 | 职责 |
//! |--------|------|
//! | [`monitor`] | 服务器运行状态（CPU / 内存 / 磁盘 / 数据库 / Redis），仅超管可读 |

pub mod monitor;
