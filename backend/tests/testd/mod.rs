//! 测试D 用例集。
//!
//! | 文件 | 覆盖 |
//! |---|---|
//! | [`impexp`] | 导入导出 I01–I13 |
//! | [`masking`] | 脱敏 X02–X04（导出/详情/回显） |
//! | [`announcement`] | 公告与消息 G01–G10 |
//! | [`storage`] | 文件存储 F01–F09 |
//! | [`project_log`] | 项目 P01–P05 + 变更日志 C01–C06 |
//! | [`mutex_help_ai`] | 互斥 X01–X05 + 帮助 H01–H03 + AI/监控 |
//! | [`linkage`] | 跨模块联动 K01–K08 |

#![allow(dead_code)]

pub mod announcement;
pub mod common;
pub mod impexp;
pub mod linkage;
pub mod masking;
pub mod mutex_help_ai;
pub mod project_log;
pub mod storage;
pub mod xlsx;
