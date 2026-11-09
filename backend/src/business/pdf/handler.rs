//! PDF 导出接口（二开模块 `business/pdf`）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/pdf/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/pdf/health` | `business:pdf:render:export` | 渲染服务自检（版本 / 并发 / 累计指标） |
//! | POST | `/business/pdf/sample` | `business:pdf:render:export` | 内置对账单样本（联调 / 验收，不需要业务数据） |
//! | POST | `/business/pdf/render` | `business:pdf:render:export` | 任意 HTML → PDF（运维排障口） |
//!
//! # 权限口径
//!
//! 三个接口统统走 `business:pdf:render:export`，而迁移 `V0343` **只把这个权限码
//! 授予 `SUPER_ADMIN`**。理由：渲染是重资源操作（单次 500 行约 190MiB 内存），
//! 且 `/render` 接受任意 HTML——开放给普通角色等于给了一个「让服务器渲染任意
//! 页面」的口子。真正给业务角色用的单据导出（发票 / 入库单）由 PDF-02 起的卡
//! 各自挂自己的权限码，不共用这个码。
//!
//! # 响应形态
//!
//! PDF 接口返回的是**二进制**（`application/pdf` + `Content-Disposition`），不是
//! 统一响应体 `{code,msg,data}`；出错时才走统一错误出口（`ApiError::into_response`）。
//! 前端必须用 `responseType: 'blob'` 取，失败分支按 `content-type` 判断是 JSON 错误体。

use axum::Json;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use crate::business::pdf::dto::{
    MAX_RENDER_HTML_BYTES, MAX_SAMPLE_ROWS, PdfHealthRes, PdfRenderReq, PdfSampleReq, build_opts,
};
use crate::business::pdf::{engine, template};
use crate::shared::error_code;

/// 渲染服务自检。
///
/// # 返回值
///
/// - `code = 0`：`data` 为版本、并发与累计指标；
///
/// # 异常情况
///
/// - 403：非超管（权限码未授予）；
/// - 503 [`error_code::PDF_RENDER_UNAVAILABLE`]：CDP 连不上（chromium 没起 / 端口错）；
/// - 504 [`error_code::PDF_RENDER_TIMEOUT`]：取版本超时。
#[nik::permission(
    path = "/business/pdf/health",
    method = "GET",
    permission = "business:pdf:render:export"
)]
#[nik::get("/business/pdf/health")]
pub async fn health() -> ResResult<PdfHealthRes> {
    let engine = engine::global();
    let (browser, protocol_version) = engine.probe().await.map_err(|error| error.into_api())?;
    let stats = engine.stats().await;
    Ok(Res::ok(PdfHealthRes {
        cdp_url: engine.url().to_string(),
        browser,
        protocol_version,
        max_concurrency: stats.max_concurrency,
        active: stats.active,
        idle_connections: stats.idle_connections,
        rendered_total: stats.rendered_total,
        failed_total: stats.failed_total,
        last_render_ms: stats.last_render_ms,
        last_document_bytes: stats.last_bytes,
        timeout_ms: stats.timeout_ms,
    }))
}

/// 生成内置「对账单」样本 PDF（不读数据库，纯渲染 —— 验收与压测都靠它）。
///
/// # 请求体
///
/// `{"rows": 500, "page": "A4", "landscape": false}`（全部字段可选）
///
/// # 返回值
///
/// `200 application/pdf`：一份含中文、跨页表格、合计行、签字栏的对账单。
///
/// # 异常情况
///
/// - 400 [`error_code::PDF_OPTIONS_INVALID`]：纸张代码 / 尺寸 / 边距越界；
/// - 422 [`error_code::PDF_TEMPLATE_INVALID`]：模板渲染失败（正常不会发生）；
/// - 503 / 504：同 [`health`]。
#[nik::permission(
    path = "/business/pdf/sample",
    method = "POST",
    permission = "business:pdf:render:export"
)]
#[nik::post("/business/pdf/sample")]
pub async fn sample(Json(req): Json<PdfSampleReq>) -> Response {
    match sample_pdf(req).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 任意 HTML → PDF（排障 / 联调口，仅超管）。
///
/// # 请求体
///
/// ```json
/// {"html": "<!DOCTYPE html>…", "page": "A4", "margin_mm": 10, "filename": "x.pdf"}
/// ```
///
/// # 返回值
///
/// `200 application/pdf`。
///
/// # 异常情况
///
/// - 400 [`error_code::PDF_OPTIONS_INVALID`]：纸张/边距/缩放越界，或 html 为空；
/// - 413 [`error_code::PDF_RENDER_TOO_LARGE`]：html 超过 4MiB；
/// - 503 / 504：同 [`health`]。
#[nik::permission(
    path = "/business/pdf/render",
    method = "POST",
    permission = "business:pdf:render:export"
)]
#[nik::post("/business/pdf/render")]
pub async fn render(Json(req): Json<PdfRenderReq>) -> Response {
    match render_html(req).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// `sample` 的实现（便于单测直接调）。
async fn sample_pdf(req: PdfSampleReq) -> std::result::Result<Response, ApiError> {
    // 行数：给 0 / 超上限都是调用方的错，但样本接口容忍——裁剪到 [1, MAX_SAMPLE_ROWS]
    let rows = req.rows.unwrap_or(20).clamp(1, MAX_SAMPLE_ROWS);
    let opts = build_opts(
        req.page.as_deref(),
        req.landscape,
        req.width_mm,
        req.height_mm,
        None,
        None,
        None,
    )?;
    let mut context = template::sample_statement(rows);
    if let Some(title) = req
        .doc_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        context.doc_title = title.to_string();
    }
    let html = template::render_statement(&context).map_err(|error| error.into_api())?;
    let pdf = engine::render_pdf(&html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let filename = format!("对账单-{rows}行.pdf");
    pdf_response(&pdf, Some(&filename))
}

/// `render` 的实现。
async fn render_html(req: PdfRenderReq) -> std::result::Result<Response, ApiError> {
    if req.html.trim().is_empty() {
        return Err(ApiError::new(400, error_code::PDF_OPTIONS_INVALID, "html 不能为空"));
    }
    if req.html.len() > MAX_RENDER_HTML_BYTES {
        return Err(ApiError::new(
            413,
            error_code::PDF_RENDER_TOO_LARGE,
            format!(
                "html 体积 {} 字节超过上限 {} 字节",
                req.html.len(),
                MAX_RENDER_HTML_BYTES
            ),
        ));
    }
    let opts = build_opts(
        req.page.as_deref(),
        req.landscape,
        req.width_mm,
        req.height_mm,
        req.margin_mm,
        req.scale,
        req.filename.as_deref(),
    )?;
    let pdf = engine::render_pdf(&req.html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let filename = opts.filename.clone().unwrap_or_else(|| "document.pdf".to_string());
    pdf_response(&pdf, Some(&filename))
}

/// 组装 PDF 响应（二进制 + 下载头）。
///
/// # 参数
///
/// - `bytes`：PDF 字节（必须是 `%PDF-` 开头，否则视为内部错误）；
/// - `filename`：下载名（可含中文，走 RFC 5987）。
///
/// # 异常情况
///
/// - 500 [`error_code::INTERNAL_ERROR`]：字节不是 PDF（说明渲染链路出了问题，
///   宁可报错也不要给前端一个打不开的文件）。
///
/// # 为什么是 `pub`
///
/// PDF-02 起的单据导出（`erp::order_pdf`）复用这一份响应组装：四个头
/// （type / length / disposition / no-store）与 `%PDF-` 校验只写一遍。
pub fn pdf_response(bytes: &[u8], filename: Option<&str>) -> std::result::Result<Response, ApiError> {
    if !is_pdf(bytes) {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "渲染结果不是合法 PDF（缺少 %PDF- 文件头）",
        ));
    }
    let mut response = Response::new(Body::from(bytes.to_vec()));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "响应头构造失败"))?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(filename.unwrap_or("document.pdf")),
    );
    // 单据内容含金额/客户名，禁止任何中间层缓存
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

/// 字节是否以 PDF 文件头开始。
fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// `Content-Disposition: attachment`（RFC 6266 / RFC 5987）。
///
/// # 参数
///
/// - `original`：原始文件名（可能是中文）。
///
/// # 返回值
///
/// `attachment; filename="<ASCII 回退名>"; filename*=UTF-8''<百分号编码>`：
/// ASCII 部分把非 ASCII / 引号 / 反斜杠 / 控制字符替换成 `_`（不破坏头语法），
/// 中文走 `filename*`（现代浏览器优先采用）。
fn content_disposition(original: &str) -> HeaderValue {
    let ascii: String = original
        .chars()
        .map(|ch| {
            if ch.is_ascii() && ch != '"' && ch != '\\' && !ch.is_control() {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in original.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if keep {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::pdf::dto::{PageSize, PdfError, PdfOpts};

    /// 中文文件名必须走 RFC 5987 的 `filename*`，ASCII 回退名不许出现控制字符。
    #[test]
    fn content_disposition_encodes_chinese() {
        let value = content_disposition("对账单-500行.pdf");
        let text = value.to_str().expect("头值必须是合法 ASCII");
        assert!(text.starts_with("attachment; filename=\""));
        assert!(text.contains("filename*=UTF-8''"));
        // 中文部分被百分号编码
        assert!(text.contains("%E5%AF%B9%E8%B4%A6%E5%8D%95"));
        assert!(!text.contains("对账单"), "header 里不能出现原始中文");
    }

    /// 文件名里的引号/反斜杠不能破坏头语法。
    #[test]
    fn content_disposition_sanitizes_quotes() {
        let text = content_disposition("a\"b\\c.pdf")
            .to_str()
            .expect("合法 ASCII")
            .to_string();
        assert!(text.contains("filename=\"a_b_c.pdf\""), "实际：{text}");
    }

    /// 非 PDF 字节必须被拒绝（宁 500 也不给前端一个打不开的文件）。
    #[test]
    fn pdf_response_rejects_non_pdf() {
        let error = pdf_response(b"<html>not a pdf</html>", Some("x.pdf")).unwrap_err();
        assert_eq!(error.status, 500);
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    /// 合法 PDF 字节要带齐四个头（type / length / disposition / no-store）。
    #[test]
    fn pdf_response_sets_headers() {
        let response = pdf_response(b"%PDF-1.4\n%fake\n", Some("单据.pdf")).expect("必须成功");
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "application/pdf");
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "15");
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
        assert!(
            headers
                .get(header::CONTENT_DISPOSITION)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("filename*=UTF-8''"),
            "中文文件名必须带 filename*"
        );
    }

    /// `is_pdf` 的边界：文件头必须完整（`%PDF` 不够，要有 `-`）。
    #[test]
    fn pdf_magic_detection() {
        assert!(is_pdf(b"%PDF-1.7"));
        assert!(!is_pdf(b"%PDF"));
        assert!(!is_pdf(b""));
        assert!(!is_pdf(b" %PDF-1.4"));
    }

    /// 空 HTML 必须 400（CDP 对空文档也会出 1 页空白，静默出空白 PDF 更难查）。
    #[tokio::test]
    async fn render_rejects_empty_html() {
        let error = render_html(PdfRenderReq {
            html: "   ".into(),
            page: None,
            landscape: None,
            width_mm: None,
            height_mm: None,
            margin_mm: None,
            scale: None,
            filename: None,
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PDF_OPTIONS_INVALID);
    }

    /// 超限 HTML 必须 413（在碰 chromium 之前就挡掉）。
    #[tokio::test]
    async fn render_rejects_oversized_html() {
        let html = "a".repeat(MAX_RENDER_HTML_BYTES + 1);
        let error = render_html(PdfRenderReq {
            html,
            page: None,
            landscape: None,
            width_mm: None,
            height_mm: None,
            margin_mm: None,
            scale: None,
            filename: None,
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, 413);
        assert_eq!(error.code, error_code::PDF_RENDER_TOO_LARGE);
    }

    /// 样本接口的坏纸张代码必须 400，且不应触达 chromium（这里没有 chromium 也能过）。
    #[tokio::test]
    async fn sample_rejects_bad_page_code() {
        let error = sample_pdf(PdfSampleReq {
            rows: Some(5),
            page: Some("B5".into()),
            landscape: None,
            width_mm: None,
            height_mm: None,
            doc_title: None,
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PDF_OPTIONS_INVALID);
    }

    /// 未装 chromium 的环境里，sample 必须给出 **503**（而不是 panic / 500），
    /// 且错误消息里带 CDP 地址，运维一眼能看出是渲染服务没起。
    #[tokio::test]
    async fn sample_reports_unavailable_when_cdp_is_down() {
        // 指向一个必然没人监听的端口，模拟「chromium 没起」
        let engine = engine::PdfEngine::new("http://127.0.0.1:9", 1, std::time::Duration::from_millis(800));
        let opts = PdfOpts::default();
        let error = engine
            .render_pdf("<html><body>x</body></html>", &opts)
            .await
            .unwrap_err();
        let api = error.into_api();
        assert_eq!(api.status, 503);
        assert_eq!(api.code, error_code::PDF_RENDER_UNAVAILABLE);
        assert!(api.msg.contains("127.0.0.1:9"), "错误消息要带 CDP 地址");
    }

    /// 引擎的失败计数器要真的累加（运维看 health 里的 failed_total）。
    #[tokio::test]
    async fn engine_counts_failures() {
        let engine = engine::PdfEngine::new("http://127.0.0.1:9", 1, std::time::Duration::from_millis(500));
        let _ = engine.render_pdf("<html></html>", &PdfOpts::default()).await;
        let stats = engine.stats().await;
        assert_eq!(stats.failed_total, 1);
        assert_eq!(stats.rendered_total, 0);
        assert_eq!(stats.active, 0, "失败路径必须把在渲染计数减回去");
        assert_eq!(stats.max_concurrency, 1);
    }

    /// `PdfError::Invalid` 之类的引擎侧错误不能被误判成 4xx 以外的语义。
    #[test]
    fn pdf_error_display_is_human_readable() {
        assert_eq!(PdfError::Timeout(20_000).to_string(), "渲染超时（>20000ms）");
        assert!(PdfError::Render("x".into()).to_string().contains("渲染失败"));
    }

    /// 纸张代码回显（health / 日志里能看出用的是哪种纸）。
    #[test]
    fn page_code_round_trip() {
        assert_eq!(PageSize::A3.code(), "A3");
        assert_eq!(PageSize::Letter.code(), "LETTER");
        assert_eq!(
            PageSize::Custom {
                width_mm: 100.0,
                height_mm: 200.0
            }
            .code(),
            "CUSTOM"
        );
    }
}
