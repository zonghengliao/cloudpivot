//! PDF 导出（PDF-01）：Chromium（CDP）+ HTML 模板 → PDF。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`dto`] | 纸张 / 边距 / 缩放参数、错误类型、HTTP 请求响应结构 |
//! | [`engine`] | 常驻 Chromium 连接池 + 并发信号量 + `Page.printToPDF` |
//! | [`template`] | 内嵌的 tera 模板、金额千分位 / 人民币大写 |
//! | [`handler`] | 自检 / 样本 / 任意 HTML 渲染三个接口（仅超管） |
//! | `templates/` | 单据模板（`include_str!` 编译进二进制，运行期零文件依赖） |
//!
//! # 一句话链路
//!
//! ```text
//! 业务模块 → template::render_*(ctx) → HTML
//!         → engine::render_pdf(html, opts) → (信号量) → (连接池) → CDP
//!         → Page.setContent → document.fonts 就绪 → Page.printToPDF → Vec<u8>
//! ```
//!
//! # 运维前置
//!
//! 必须有一台跑着 `chromium-pdf.service` 的本机（`:9222`），安装脚本
//! `scripts/pdf/install-pdf-infra.sh`；缺什么用 `GET /api/business/pdf/health`
//! 一眼看出来（它会直接返回 503 + 具体原因）。详细说明见
//! `docs/PDF导出基础设施.md`。
//!
//! # 与其它模块的关系
//!
//! 本模块**不读业务数据**：调用方（发票 / 入库单 / 对账单）自己查库、自己组装
//! [`template::StatementContext`] 之类的上下文字典，然后只要两步：
//!
//! ```ignore
//! let html = crate::business::pdf::template::render_statement(&ctx)?;
//! let pdf = crate::business::pdf::engine::render_pdf(&html, &opts).await?;
//! ```

pub mod dto;
pub mod engine;
pub mod handler;
pub mod template;

/// 内部各部分的公共出口（业务模块只需 `use crate::business::pdf::{PdfOpts, render_pdf}`）。
pub use dto::{PageSize, PdfError, PdfMargins, PdfOpts};
pub use engine::render_pdf;
pub use template::{amount_upper, format_int, format_money};
