//! 单据 PDF **渲染收尾**的共用件（PDF-04：调拨单 / 盘点单 / 采购退货单 / 销售退货单）。
//!
//! # 边界
//!
//! 只做「HTML 已经拼好 → 交给 Chromium → 拿回字节 + 文件名 + 行数 + 金额 + 耗时」。
//! 取数、上下文组装、权限、审计分别在各单据自己的 `*_pdf` 模块里
//! （PDF-03 的 `stock_pdf` / `contract_pdf` 早于本文件落地，未回改，避免波及已验收代码）。
//!
//! # 为什么要单独一个文件
//!
//! 四个单据的上下文结构体不同，但**收尾三件事完全一样**：文件名净化、渲染计时、
//! 审计要的 `row_count` / `total_amount`。复制四遍只会让「字节数算错 / 耗时算错」
//! 这类低级错误有四个落点。

use std::time::Instant;

use nik::prelude::rust_decimal::Decimal;

use crate::business::erp::doc_pdf_model;
use crate::business::pdf::PdfOpts;
use crate::business::pdf::engine;

/// 一次单据导出的结果（HTTP 层据此组响应头与审计行）。
#[derive(Debug, Clone)]
pub struct RenderedDoc {
    /// PDF 字节
    pub bytes: Vec<u8>,
    /// 下载文件名（取自 [`PdfOpts::filename`]）
    pub filename: String,
    /// 单号
    pub doc_no: String,
    /// 明细行数
    pub row_count: usize,
    /// 合计金额（元；审计口径）
    pub total_amount: Decimal,
    /// 渲染耗时（毫秒，只含 Chromium 那一段）
    pub duration_ms: u64,
}

/// 文件名：`<单据名>-<单号>.pdf`（单号里的路径字符已净化）。
pub fn pdf_filename(doc_label: &str, doc_no: &str) -> String {
    format!(
        "{}-{}.pdf",
        doc_label,
        doc_pdf_model::sanitize_filename(doc_no)
    )
}

/// HTML → PDF 字节，并组装 [`RenderedDoc`]。
///
/// # 参数
///
/// - `html`：已经渲染好的 HTML（模板层保证）；
/// - `doc_no`：单号（回填到结果里，审计用）；
/// - `opts`：渲染参数（**必须已带 filename**，否则下载名会空）；
/// - `row_count` / `total_amount`：审计字段。
///
/// # 异常情况
///
/// 原样透传 [`crate::business::pdf::dto::PdfError`]（422 模板 / 503 不可用 / 504 超时），
/// 由调用方 [`crate::business::pdf::dto::PdfError::into_api`] 转 HTTP。
pub async fn render_document(
    html: &str,
    doc_no: &str,
    opts: &PdfOpts,
    row_count: usize,
    total_amount: Decimal,
) -> std::result::Result<RenderedDoc, crate::business::pdf::PdfError> {
    let filename = opts.filename.clone().unwrap_or_default();
    let started = Instant::now();
    let bytes = engine::render_pdf(html, opts).await?;
    Ok(RenderedDoc {
        bytes,
        filename,
        doc_no: doc_no.to_string(),
        row_count,
        total_amount,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 下载名 = 单据名 + 净化后的单号；路径分隔符 / 引号必须换掉。
    #[test]
    fn filename_is_sanitized() {
        assert_eq!(
            pdf_filename("调拨单", "TK-20261108-000001"),
            "调拨单-TK-20261108-000001.pdf"
        );
        assert_eq!(pdf_filename("盘点单", "ST/1:x"), "盘点单-ST_1_x.pdf");
        assert_eq!(pdf_filename("采购退货单", "PRT\n1"), "采购退货单-PRT_1.pdf");
    }
}
