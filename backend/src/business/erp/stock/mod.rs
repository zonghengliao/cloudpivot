//! 库存单据 PDF 域（PDF-03 / PDF-04）：收货入库单 / 出库单 / 盘点表导出。
//!
//! 拆目录见 P2-24：原 `erp/stock_*.rs` 两个文件整体搬进本目录，模块路径由
//! `erp::stock_*` 变为 `erp::stock::stock_*`，文件内容与逻辑一字未改（只改
//! `mod` / `use` / intra-doc 链接）。
//!
//! * [`stock_pdf`] —— 收货入库单 / 出库单 PDF 导出（PDF-03）：批次号 / 库位 /
//!   来源单据号 / 序列号；取数 + 渲染 + 两个 GET 入口 + 导出审计；
//! * [`stock_take_pdf`] —— 盘点单 PDF 导出（PDF-04）：**唯一**带空白手写栏的单据
//!   （实盘 / 差异 / 备注），实盘数只在合计区以盘盈 / 盘亏出现。
//!
//! 出入库 / 盘点的**业务域**不在这里：它们分别在 [`super::stockin`] /
//! [`super::stockout`] / [`super::stocktake`]，本目录只放"打印"这一层。

pub mod stock_pdf;
pub mod stock_take_pdf;
