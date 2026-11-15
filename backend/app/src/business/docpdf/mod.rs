//! 单据 PDF 导出的**公共件**（P2-34 从 `erp/` 搬到独立域）。
//!
//! # 为什么单独一个域
//!
//! 这两个文件被三个域用：`erp`（合同 / 收货 / 出库 / 盘点 / 调拨 / 退货 / 报表 PDF）、
//! `flow`（审批单 PDF `instance_pdf`）、`task`（任务 PDF `task_pdf`）。原先它们在 `erp/` 里，
//! `flow` / `task` 为了格式化一行时间就得直连 `erp` —— 那两条边正是 P2-B 要摘的。
//!
//! 放到独立域而不是 `business::pdf`：`pdf` 的边界是「**不读业务数据**的渲染能力」
//! （模板 / Chromium / 字节流），而 `doc_pdf_model` 要批量补名称、查序列号、写导出留痕
//! —— 那是**读业务表**的取数件。混进 `pdf` 会把那条边界糊掉。
//!
//! 依赖方向：`docpdf` → `pdf`（渲染）+ `shared`（日志 / 错误码）；
//! `erp` / `flow` / `task` → `docpdf`。**没有**回边。
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`doc_pdf_model`] | 取数公共件：批量补名称（库位 / 仓库 / 供应商 / 客户）、序列号清单、导出留痕、时间与文件名口径 |
//! | [`doc_pdf_render`] | 渲染收尾：HTML → Chromium → [`doc_pdf_render::RenderedDoc`]（文件名 / 行数 / 金额 / 耗时） |

pub mod doc_pdf_model;
pub mod doc_pdf_render;
