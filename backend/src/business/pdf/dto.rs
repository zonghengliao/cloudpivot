//! PDF 导出的参数 / 错误 / 响应结构。
//!
//! # 为什么参数是「毫米 + 枚举」而不是直接透传 CDP 的英寸
//!
//! Chromium 的 `Page.printToPDF` 全部用**英寸**（`paperWidth` / `marginTop` …），
//! 而业务侧（打印、装订、信封）一律按**毫米**说话：A4 = 210×297mm、页边距 10mm。
//! 让每个调用方自己乘 `1/25.4` 是错误扩散源（写错一位就是 1mm 与 25.4mm 的差别），
//! 所以换算**只在这里做一次**，且 `<PdfOpts>::paper_inches()` 是唯一入口。
//!
//! # 取值边界是硬约束，不是建议
//!
//! `scale`（0.1–2.0）、纸张尺寸（不低于 50mm、不超过 2000mm）、边距（0–100mm）
//! 都在 [`PdfOpts::validate`] 里挡掉。CDP 对越界值不会报错，而是**静默出一张
//! 空白或极小的纸**（实测：`paperWidth=0` 得到 1 页空白 PDF），这种「成功但错」
//! 比 500 更难查，因此必须在入口拒绝。

use serde::{Deserialize, Serialize};

use nik::prelude::*;

use crate::shared::error_code;

/// 毫米 → 英寸的换算常数（1 英寸 = 25.4 毫米）。
const MM_PER_INCH: f64 = 25.4;

/// 纸张尺寸。
///
/// `Custom` 用毫米表示，便于业务侧直接写「210×297」。
#[derive(Debug, Clone, PartialEq)]
pub enum PageSize {
    /// A3（297×420mm）
    A3,
    /// A4（210×297mm）—— 默认值，与国内单据/发票的通用规格一致。
    A4,
    /// A5（148×210mm）
    A5,
    /// Letter（8.5×11in）
    Letter,
    /// 自定义（毫米，纵向尺寸；`landscape` 时由调用方交换宽高语义，见 [`PdfOpts::paper_inches`]）。
    Custom { width_mm: f64, height_mm: f64 },
}

impl Default for PageSize {
    fn default() -> Self {
        Self::A4
    }
}

impl PageSize {
    /// 纸张纵向尺寸（毫米，宽 × 高）。
    ///
    /// # 返回值
    ///
    /// `(宽, 高)`，单位毫米，**恒为纵向**（横向由 [`PdfOpts::landscape`] 在出参数时交换）。
    pub fn size_mm(&self) -> (f64, f64) {
        match self {
            Self::A3 => (297.0, 420.0),
            Self::A4 => (210.0, 297.0),
            Self::A5 => (148.0, 210.0),
            Self::Letter => (215.9, 279.4),
            Self::Custom {
                width_mm,
                height_mm,
            } => (*width_mm, *height_mm),
        }
    }

    /// 解析纸张代码（大小写不敏感）。
    ///
    /// # 参数
    ///
    /// - `code`：`A3` / `A4` / `A5` / `LETTER` / `CUSTOM`。
    ///
    /// # 异常情况
    ///
    /// - 400（[`error_code::PDF_OPTIONS_INVALID`]）：不认识的代码。`CUSTOM` 的宽高
    ///   由请求体另外两个字段给出，不走本函数。
    pub fn parse(code: &str) -> std::result::Result<Self, ApiError> {
        match code.trim().to_ascii_uppercase().as_str() {
            "A3" => Ok(Self::A3),
            "A4" => Ok(Self::A4),
            "A5" => Ok(Self::A5),
            "LETTER" => Ok(Self::Letter),
            "CUSTOM" => Ok(Self::Custom {
                width_mm: 210.0,
                height_mm: 297.0,
            }),
            other => Err(ApiError::new(
                400,
                error_code::PDF_OPTIONS_INVALID,
                format!("纸张代码不支持：{other}（可选 A3 / A4 / A5 / LETTER / CUSTOM）"),
            )),
        }
    }

    /// 纸张代码（回显用）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::A3 => "A3",
            Self::A4 => "A4",
            Self::A5 => "A5",
            Self::Letter => "LETTER",
            Self::Custom { .. } => "CUSTOM",
        }
    }
}

/// 页边距（毫米）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfMargins {
    /// 上边距（毫米）
    pub top_mm: f64,
    /// 右边距（毫米）
    pub right_mm: f64,
    /// 下边距（毫米）
    pub bottom_mm: f64,
    /// 左边距（毫米）
    pub left_mm: f64,
}

impl Default for PdfMargins {
    /// 统一 10mm：A4 上 10mm 边距给了约 190×277mm 的正文区，是 78 列明细表
    /// 常见排版下最紧但不挤的取值（12mm 起 500 行会多出 1 页）。
    fn default() -> Self {
        Self {
            top_mm: 10.0,
            right_mm: 10.0,
            bottom_mm: 10.0,
            left_mm: 10.0,
        }
    }
}

impl PdfMargins {
    /// 四边同值构造。
    pub fn all(mm: f64) -> Self {
        Self {
            top_mm: mm,
            right_mm: mm,
            bottom_mm: mm,
            left_mm: mm,
        }
    }
}

/// PDF 渲染参数（业务侧唯一入口）。
#[derive(Debug, Clone, PartialEq)]
pub struct PdfOpts {
    /// 纸张
    pub page: PageSize,
    /// 是否横向
    pub landscape: bool,
    /// 是否打印背景色/背景图（表格斑马纹、单元格底色依赖它；默认开）
    pub print_background: bool,
    /// 是否让 CSS 里的 `@page { size: … }` 覆盖本结构里的纸张尺寸
    ///
    /// 默认 **false**：单据的纸张由后端参数决定，模板里的 `@page` 只作为
    /// 「浏览器直接打印」时的兜底，避免模板 CSS 悄悄改掉纸张。
    pub prefer_css_page_size: bool,
    /// 页边距
    pub margins: PdfMargins,
    /// 缩放（0.1–2.0，默认 1.0）
    pub scale: f64,
    /// 是否输出页眉页脚（CDP 自带的时间/标题/页码）
    ///
    /// 默认 **false**：页眉页脚模板必须内联 HTML，且中文页码排版不可控；
    /// 单据的页码由模板自己画（`@page` 计数器在 Chromium 下不生效，走 CSS 分页的
    /// 单据请用 tfoot 重复打印「第 N 页」占位）。
    pub display_header_footer: bool,
    /// 页眉模板（`display_header_footer = true` 时才生效）
    pub header_template: Option<String>,
    /// 页脚模板（同上）
    pub footer_template: Option<String>,
    /// 下载文件名（`Content-Disposition`；中文走 RFC 5987 编码）
    pub filename: Option<String>,
}

impl Default for PdfOpts {
    fn default() -> Self {
        Self {
            page: PageSize::A4,
            landscape: false,
            print_background: true,
            prefer_css_page_size: false,
            margins: PdfMargins::default(),
            scale: 1.0,
            display_header_footer: false,
            header_template: None,
            footer_template: None,
            filename: None,
        }
    }
}

impl PdfOpts {
    /// 纸张的英寸尺寸（已按 `landscape` 交换宽高）。
    ///
    /// # 返回值
    ///
    /// `(宽英寸, 高英寸)`，直接喂 CDP 的 `paperWidth` / `paperHeight`。
    pub fn paper_inches(&self) -> (f64, f64) {
        let (w_mm, h_mm) = self.page.size_mm();
        let (w_in, h_in) = (w_mm / MM_PER_INCH, h_mm / MM_PER_INCH);
        if self.landscape {
            (h_in, w_in)
        } else {
            (w_in, h_in)
        }
    }

    /// 校验取值边界。
    ///
    /// # 异常情况
    ///
    /// - 400（[`error_code::PDF_OPTIONS_INVALID`]）：纸张尺寸 / 边距 / 缩放越界。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        let (w_mm, h_mm) = self.page.size_mm();
        for (label, value) in [("宽", w_mm), ("高", h_mm)] {
            if !value.is_finite() || !(50.0..=2000.0).contains(&value) {
                return Err(ApiError::new(
                    400,
                    error_code::PDF_OPTIONS_INVALID,
                    format!("纸张{label}度必须在 50–2000 毫米之间，当前 {value}"),
                ));
            }
        }
        for (label, value) in [
            ("上", self.margins.top_mm),
            ("右", self.margins.right_mm),
            ("下", self.margins.bottom_mm),
            ("左", self.margins.left_mm),
        ] {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(ApiError::new(
                    400,
                    error_code::PDF_OPTIONS_INVALID,
                    format!("{label}边距必须在 0–100 毫米之间，当前 {value}"),
                ));
            }
        }
        // 边距吃掉整张纸时 CDP 会返回空内容，这里提前挡掉
        let (paper_w_in, _) = self.paper_inches();
        let paper_w_mm = paper_w_in * MM_PER_INCH;
        if self.margins.left_mm + self.margins.right_mm >= paper_w_mm {
            return Err(ApiError::new(
                400,
                error_code::PDF_OPTIONS_INVALID,
                format!(
                    "左右边距之和（{}mm）已超过纸张宽度（{paper_w_mm:.1}mm）",
                    self.margins.left_mm + self.margins.right_mm
                ),
            ));
        }
        if !(0.1..=2.0).contains(&self.scale) {
            return Err(ApiError::new(
                400,
                error_code::PDF_OPTIONS_INVALID,
                format!("缩放必须在 0.1–2.0 之间，当前 {}", self.scale),
            ));
        }
        if self.display_header_footer
            && self.header_template.is_none()
            && self.footer_template.is_none()
        {
            return Err(ApiError::new(
                400,
                error_code::PDF_OPTIONS_INVALID,
                "display_header_footer 为真时必须至少给出一个页眉或页脚模板",
            ));
        }
        Ok(())
    }
}

/// PDF 渲染错误（引擎层出口）。
///
/// 业务 handler 用 [`PdfError::into_api`] 转成统一响应体；引擎内部不直接造
/// [`ApiError`]，因为同一种失败（例如 CDP 掉线）在不同上下文下可能是 502 或 503。
#[derive(Debug)]
pub enum PdfError {
    /// CDP 连不上 / 进程崩了 / 连接被重置（可重试）
    Unavailable(String),
    /// 渲染超时（`PDF_RENDER_TIMEOUT_MS`）
    Timeout(u64),
    /// 模板渲染失败（缺变量 / 语法错）
    Template(String),
    /// 参数非法（边界值由 [`PdfOpts::validate`] 挡，这里是引擎侧的兜底）
    Invalid(String),
    /// 页面渲染失败（JS 异常 / CDP 返回错误）
    Render(String),
}

impl PdfError {
    /// 转为统一响应出口。
    ///
    /// # 返回值
    ///
    /// - `Unavailable` → 503 + [`error_code::PDF_RENDER_UNAVAILABLE`]（运维信号：chromium 挂了）
    /// - `Timeout` → 504 + [`error_code::PDF_RENDER_TIMEOUT`]
    /// - `Template` → 422 + [`error_code::PDF_TEMPLATE_INVALID`]
    /// - `Invalid` → 400 + [`error_code::PDF_OPTIONS_INVALID`]
    /// - `Render` → 500 + [`error_code::INTERNAL_ERROR`]
    pub fn into_api(self) -> ApiError {
        match self {
            Self::Unavailable(message) => ApiError::new(
                503,
                error_code::PDF_RENDER_UNAVAILABLE,
                format!("PDF 渲染服务不可用：{message}"),
            ),
            Self::Timeout(ms) => ApiError::new(
                504,
                error_code::PDF_RENDER_TIMEOUT,
                format!("PDF 渲染超时（>{ms}ms）"),
            ),
            Self::Template(message) => {
                ApiError::new(422, error_code::PDF_TEMPLATE_INVALID, message)
            }
            Self::Invalid(message) => ApiError::new(400, error_code::PDF_OPTIONS_INVALID, message),
            Self::Render(message) => ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("PDF 渲染失败：{message}"),
            ),
        }
    }

    /// 是否属于「连接层面的失败」（可丢弃连接后重试一次）。
    pub fn is_connection_failure(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "渲染服务不可用：{m}"),
            Self::Timeout(ms) => write!(f, "渲染超时（>{ms}ms）"),
            Self::Template(m) => write!(f, "模板错误：{m}"),
            Self::Invalid(m) => write!(f, "参数错误：{m}"),
            Self::Render(m) => write!(f, "渲染失败：{m}"),
        }
    }
}

impl std::error::Error for PdfError {}

// ==================== HTTP DTO ====================

/// `GET /business/pdf/health` 响应：渲染服务自检。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfHealthRes {
    /// CDP 地址（不含凭据，仅主机端口）
    pub cdp_url: String,
    /// Chromium 版本串（如 `HeadlessChrome/157.0.8103.38`）
    pub browser: String,
    /// CDP 协议版本（如 `1.3`）
    pub protocol_version: String,
    /// 最大并发（信号量许可数）
    pub max_concurrency: u32,
    /// 当前在渲染中的请求数
    pub active: u32,
    /// 连接池里的空闲连接数
    pub idle_connections: usize,
    /// 累计成功渲染数（进程启动以来）
    pub rendered_total: u64,
    /// 累计失败数
    pub failed_total: u64,
    /// 最近一次渲染耗时（毫秒）
    pub last_render_ms: u64,
    /// 最近一次渲染的 PDF 字节数
    pub last_document_bytes: u64,
    /// 渲染超时（毫秒）
    pub timeout_ms: u64,
}

/// `POST /business/pdf/sample` 请求：生成内置「对账单」样本（验证 / 联调用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfSampleReq {
    /// 明细行数（1–5000，默认 20）
    pub rows: Option<u32>,
    /// 纸张代码（默认 A4）
    pub page: Option<String>,
    /// 是否横向
    pub landscape: Option<bool>,
    /// 自定义纸张宽度（毫米，`page=CUSTOM` 时必填）
    pub width_mm: Option<f64>,
    /// 自定义纸张高度（毫米，`page=CUSTOM` 时必填）
    pub height_mm: Option<f64>,
    /// 单据标题（默认「对 账 单」）
    pub doc_title: Option<String>,
}

/// `POST /business/pdf/render` 请求：把任意 HTML 渲染成 PDF（运维 / 联调口）。
///
/// # 安全约束
///
/// - 只授给 `SUPER_ADMIN`（权限码 `business:pdf:render:export` 仅授予超管）；
/// - `html` 上限 [`MAX_RENDER_HTML_BYTES`]，超限 413；
/// - 渲染期间页面禁网：模板里不能引外链 CSS/图片（Chromium 侧只保证不主动联网，
///   但**不要**把用户输入直接拼进 HTML —— 这是调用方的责任）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfRenderReq {
    /// 完整 HTML 文档
    pub html: String,
    /// 纸张代码（默认 A4）
    pub page: Option<String>,
    /// 是否横向
    pub landscape: Option<bool>,
    /// 自定义纸张宽度（毫米）
    pub width_mm: Option<f64>,
    /// 自定义纸张高度（毫米）
    pub height_mm: Option<f64>,
    /// 页边距（毫米，四边同值；默认 10）
    pub margin_mm: Option<f64>,
    /// 缩放（0.1–2.0）
    pub scale: Option<f64>,
    /// 下载文件名（默认 `document.pdf`）
    pub filename: Option<String>,
}

/// `POST /business/pdf/render` 的 HTML 体积上限（4 MiB）。
///
/// 参考：500 行对账单渲染前的 HTML 约 210KB，发票级单据 < 50KB，4MiB 足够
/// 容纳 2000 行明细，同时挡住「上传一个大 HTML 让 chromium 吃满内存」。
pub const MAX_RENDER_HTML_BYTES: usize = 4 * 1024 * 1024;

/// 样本单据允许的最大行数（再多不是「样本」，而且 5000 行 A4 要 140 页）。
pub const MAX_SAMPLE_ROWS: u32 = 5000;

/// 由请求体攒出 [`PdfOpts`]（纸张 / 横向 / 自定义尺寸 / 边距 / 缩放）。
///
/// # 参数
///
/// - `page`：纸张代码，`None` 取 A4；
/// - `landscape`：是否横向，`None` 取纵向；
/// - `width_mm` / `height_mm`：`page=CUSTOM` 时的毫米尺寸；
/// - `margin_mm`：四边同值边距，`None` 取 10mm；
/// - `scale`：`None` 取 1.0；
/// - `filename`：下载名。
///
/// # 异常情况
///
/// - 400（[`error_code::PDF_OPTIONS_INVALID`]）：纸张代码不认识 / CUSTOM 缺宽高 / 越界。
pub fn build_opts(
    page: Option<&str>,
    landscape: Option<bool>,
    width_mm: Option<f64>,
    height_mm: Option<f64>,
    margin_mm: Option<f64>,
    scale: Option<f64>,
    filename: Option<&str>,
) -> std::result::Result<PdfOpts, ApiError> {
    let page_code = page
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("A4");
    let mut size = PageSize::parse(page_code)?;
    if page_code.eq_ignore_ascii_case("CUSTOM") {
        match (width_mm, height_mm) {
            (Some(w), Some(h)) => {
                size = PageSize::Custom {
                    width_mm: w,
                    height_mm: h,
                }
            }
            _ => {
                return Err(ApiError::new(
                    400,
                    error_code::PDF_OPTIONS_INVALID,
                    "page=CUSTOM 时必须同时给出 width_mm 与 height_mm",
                ));
            }
        }
    }
    let opts = PdfOpts {
        page: size,
        landscape: landscape.unwrap_or(false),
        print_background: true,
        prefer_css_page_size: false,
        margins: match margin_mm {
            Some(mm) => PdfMargins::all(mm),
            None => PdfMargins::default(),
        },
        scale: scale.unwrap_or(1.0),
        display_header_footer: false,
        header_template: None,
        footer_template: None,
        filename: filename
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    opts.validate()?;
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A4 = 210×297mm = 8.2677×11.6929in（CDP 的 paperWidth/paperHeight 口径）。
    #[test]
    fn a4_converts_to_inches() {
        let opts = PdfOpts::default();
        let (w, h) = opts.paper_inches();
        assert!((w - 8.2677).abs() < 1e-3, "A4 宽应为 8.2677 英寸，实际 {w}");
        assert!(
            (h - 11.6929).abs() < 1e-3,
            "A4 高应为 11.6929 英寸，实际 {h}"
        );
    }

    /// 横向 = 纵向交换宽高（不是把纸转 90° 后再缩放）。
    #[test]
    fn landscape_swaps_dimensions() {
        let mut opts = PdfOpts::default();
        let portrait = opts.paper_inches();
        opts.landscape = true;
        let landscape = opts.paper_inches();
        assert!((landscape.0 - portrait.1).abs() < 1e-9);
        assert!((landscape.1 - portrait.0).abs() < 1e-9);
        assert!(landscape.0 > landscape.1, "横向时宽必须大于高");
    }

    /// 各预设纸张的毫米尺寸与公开规格一致。
    #[test]
    fn preset_sizes_match_spec() {
        assert_eq!(PageSize::A3.size_mm(), (297.0, 420.0));
        assert_eq!(PageSize::A4.size_mm(), (210.0, 297.0));
        assert_eq!(PageSize::A5.size_mm(), (148.0, 210.0));
        assert_eq!(PageSize::Letter.size_mm(), (215.9, 279.4));
        assert_eq!(
            PageSize::Custom {
                width_mm: 100.0,
                height_mm: 150.0
            }
            .size_mm(),
            (100.0, 150.0)
        );
    }

    /// 纸张代码解析大小写不敏感，未知代码必须 400 而不是静默用 A4。
    #[test]
    fn page_code_parsing() {
        assert_eq!(PageSize::parse("a4").unwrap(), PageSize::A4);
        assert_eq!(PageSize::parse("A5").unwrap(), PageSize::A5);
        assert_eq!(PageSize::parse("letter").unwrap(), PageSize::Letter);
        assert_eq!(PageSize::parse(" CUSTOM ").unwrap().code(), "CUSTOM");
        let error = PageSize::parse("B5").unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PDF_OPTIONS_INVALID);
    }

    /// 越界参数必须挡在入口（CDP 对越界值静默出空白纸）。
    #[test]
    fn validate_rejects_out_of_range() {
        let mut opts = PdfOpts::default();
        opts.scale = 0.05;
        assert_eq!(
            opts.validate().unwrap_err().code,
            error_code::PDF_OPTIONS_INVALID
        );

        let mut opts = PdfOpts::default();
        opts.scale = 2.5;
        assert!(opts.validate().is_err());

        let mut opts = PdfOpts::default();
        opts.margins = PdfMargins::all(120.0);
        assert!(opts.validate().is_err());

        let mut opts = PdfOpts::default();
        opts.page = PageSize::Custom {
            width_mm: 10.0,
            height_mm: 297.0,
        };
        assert!(opts.validate().is_err(), "纸宽 10mm 必须拒绝");

        let mut opts = PdfOpts::default();
        opts.page = PageSize::Custom {
            width_mm: f64::NAN,
            height_mm: 297.0,
        };
        assert!(opts.validate().is_err(), "NaN 尺寸必须拒绝");
    }

    /// 边距吃掉整张纸时必须拒绝（否则 CDP 返回空内容）。
    #[test]
    fn validate_rejects_margins_larger_than_paper() {
        let mut opts = PdfOpts::default();
        opts.page = PageSize::Custom {
            width_mm: 100.0,
            height_mm: 100.0,
        };
        opts.margins = PdfMargins {
            top_mm: 10.0,
            right_mm: 60.0,
            bottom_mm: 10.0,
            left_mm: 60.0,
        };
        assert!(opts.validate().is_err());
    }

    /// 页眉页脚开关为真却没有任何模板 → 400（CDP 会静默忽略，出无页眉的 PDF）。
    #[test]
    fn header_footer_requires_template() {
        let mut opts = PdfOpts::default();
        opts.display_header_footer = true;
        assert!(opts.validate().is_err());
        opts.footer_template = Some("<div class='pageNumber'></div>".into());
        assert!(opts.validate().is_ok());
    }

    /// `build_opts` 覆盖请求体到 PdfOpts 的整条映射。
    #[test]
    fn build_opts_maps_request_fields() {
        let opts = build_opts(None, None, None, None, None, None, None).unwrap();
        assert_eq!(opts.page, PageSize::A4);
        assert_eq!(opts.margins, PdfMargins::default());
        assert_eq!(opts.scale, 1.0);
        assert!(opts.print_background, "背景色默认必须开（表格斑马纹）");

        let opts = build_opts(
            Some("CUSTOM"),
            Some(true),
            Some(100.0),
            Some(150.0),
            Some(5.0),
            Some(0.9),
            Some(" 发票.pdf "),
        )
        .unwrap();
        assert_eq!(
            opts.page,
            PageSize::Custom {
                width_mm: 100.0,
                height_mm: 150.0
            }
        );
        assert!(opts.landscape);
        assert_eq!(opts.margins, PdfMargins::all(5.0));
        assert_eq!(opts.scale, 0.9);
        assert_eq!(opts.filename.as_deref(), Some("发票.pdf"));

        // 空白文件名视同没给
        let opts = build_opts(Some("A4"), None, None, None, None, None, Some("   ")).unwrap();
        assert!(opts.filename.is_none());
    }

    /// `page=CUSTOM` 缺宽高必须 400。
    #[test]
    fn build_opts_requires_custom_dimensions() {
        let error =
            build_opts(Some("CUSTOM"), None, Some(210.0), None, None, None, None).unwrap_err();
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("width_mm"));
    }

    /// 错误 → 响应出口的映射（状态码 + 业务码成对，不给前端猜）。
    #[test]
    fn pdf_error_maps_to_api_error() {
        let cases = [
            (
                PdfError::Unavailable("x".into()),
                503,
                error_code::PDF_RENDER_UNAVAILABLE,
            ),
            (
                PdfError::Timeout(20000),
                504,
                error_code::PDF_RENDER_TIMEOUT,
            ),
            (
                PdfError::Template("x".into()),
                422,
                error_code::PDF_TEMPLATE_INVALID,
            ),
            (
                PdfError::Invalid("x".into()),
                400,
                error_code::PDF_OPTIONS_INVALID,
            ),
            (
                PdfError::Render("x".into()),
                500,
                error_code::INTERNAL_ERROR,
            ),
        ];
        for (error, status, code) in cases {
            let api = error.into_api();
            assert_eq!(api.status, status);
            assert_eq!(api.code, code);
        }
    }

    /// 只有连接层失败才允许「丢连接 + 重试一次」。
    #[test]
    fn only_unavailable_is_retryable() {
        assert!(PdfError::Unavailable("x".into()).is_connection_failure());
        assert!(!PdfError::Timeout(1).is_connection_failure());
        assert!(!PdfError::Render("x".into()).is_connection_failure());
        assert!(!PdfError::Template("x".into()).is_connection_failure());
        assert!(!PdfError::Invalid("x".into()).is_connection_failure());
    }
}
