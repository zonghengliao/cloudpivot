//! 导入导出引擎（批次 A · A3）。
//!
//! # 一句话说清
//!
//! 导入导出不是「某个页面的按钮」，而是**配置驱动的通用通道**：业务方只登记
//! 「我要导什么字段、哪些必填、拿什么判重、外键指向哪张表」，模板生成、结构校验、
//! 逐行校验、错误明细、入库、同步导出全部复用本模块。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`definition`] | 业务接入契约：列类型 / 解析 / 格式化 / 字典翻译 / 两个 trait |
//! | [`registry`] | 定义注册表（进程级只读单例，`biz_type` → 定义） |
//! | [`engine`] | xlsx 解析（表头映射 + 逐行读取）与写入 |
//! | [`validate`] | 逐行校验、文件内唯一、**外键批量校验**（SQL 次数 = 外键列数） |
//! | [`template`] | 模板生成（表头 / 示例 / 字段说明三要素）与错误行导出 |
//! | [`model`] | 任务与错误明细的数据访问 |
//! | [`dto`] | 请求 / 响应 DTO |
//! | [`handler`] | HTTP 接口（模板 / 预检 / 确认 / 查询 / 同步导出 / 异步任务） |
//! | [`worker`] | 异步导出执行器：`SKIP LOCKED` 认领 + 分块进度 + 失败退避重试 |
//! | [`cleanup`] | 僵尸任务回收（30 min 无心跳）+ 过期产物清理（下载 410） |
//! | [`builtin`] | 内置真实业务定义（`sys_dict_item`） |
//!
//! # 交付边界（A3 + A4 合并后的现状）
//!
//! - **异步导出**（`worker` / `cleanup` / 接口层取消重试下载）：A4 已交付；
//! - **导入仍是两步式同步链路**（上传即预检 → 确认入库），未改异步：
//!   改它会推翻 A3 已验收的「上传返回完整预检报告」契约，留待单独卡；
//! - **CSV 超大档（> 20 万行）流式导入**：未做；
//! - **导出脱敏与明文权限**：未做（`ExportColumn.sensitive` 只做标记）；
//! - 前端页面：A7。
//!
//! # 性能取舍（对应验收 4）
//!
//! xlsx 格式本身没有 SAX 式流式读（`calamine` 的 `worksheet_range` 整表建 `Range`，
//! 架构文档 §3.4 已实测 10 万行 ≈ 220 MB 峰值）。本模块在此约束下做到：
//! ①**不产生第二份行集合**（逐行 `Range::rows()`，行处理完即释放，不建 `Vec<RowValues>`）；
//! ②外键取值**聚合去重**后才发 SQL；③错误明细有上限（2000 条）且有界。

pub mod batch_export;
pub mod builtin;
pub mod cleanup;
pub mod definition;
pub mod dto;
pub mod engine;
pub mod handler;
pub mod model;
pub mod registry;
pub mod template;
pub mod validate;
pub mod worker;

pub use definition::{ExportDefinition, ImportDefinition};
pub use registry::global_registry;
