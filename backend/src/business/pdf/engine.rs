//! PDF 渲染引擎：常驻 Chromium（CDP）连接池 + 并发信号量。
//!
//! # 架构（为什么是「外部常驻浏览器 + CDP」，而不是进程内 PDF 库）
//!
//! 单据排版全部由 HTML/CSS 完成（模板 = 前端同款技能栈），只有真正的浏览器
//! 才能把 CSS 分页规则（`thead { display: table-header-group }`、`break-inside:
//! avoid`、`font-variant-numeric: tabular-nums`）兑现成 PDF。因此渲染核心是
//! **一个由 systemd 托管的 headless Chromium**（`scripts/pdf/chromium-pdf.service`，
//! `:9222`，每天 03:00 由 `RuntimeMaxSec` 重启），本模块只做三件事：
//!
//! 1. **连接池**：[`PdfEngine`] 持有最多 `PDF_MAX_CONCURRENCY` 条独立 CDP WebSocket
//!    连接（每条连接一个 `Browser` + 一个驱动它的 handler task）。用独立连接而不是
//!    一条连接上开多个 page，是因为单条 WS 上所有命令共用一条发送队列，3 个并发
//!    渲染会互相排队；而且一条连接崩掉不会带走另外两条。
//! 2. **信号量**：[`tokio::sync::Semaphore`] 硬限并发数（默认 3）。Chromium 的
//!    渲染进程吃内存（实测 3 并发 500 行单据峰值 PSS 578MiB），不限并发会被
//!    「一次导出 20 张单」打爆内存。
//! 3. **失效重建**：连接层错误（WS 断开 / CDP 无响应）先丢弃该连接再重试一次；
//!    渲染错误（模板 JS 异常）不重试（重试也会同样失败），直接把错误交上去。
//!
//! # 环境变量（运维口径，见 `docs/PDF导出基础设施.md`）
//!
//! | 变量 | 默认 | 说明 |
//! |------|------|------|
//! | `PDF_CDP_URL` | `http://127.0.0.1:9222` | Chromium CDP 地址；可写 `ws://` 直连 |
//! | `PDF_MAX_CONCURRENCY` | `3` | 并发上限（1–8，越界按边界裁剪并告警） |
//! | `PDF_RENDER_TIMEOUT_MS` | `20000` | 单次渲染总超时（含建连接 + 渲染） |
//!
//! 不走 `config.toml` 的原因：`nik` 的 `AppConfig` 是框架内的强类型结构，二开
//! 代码无法往里加自定义段（改 vendor 就是侵入底座）；`business/storage` 的
//! AES 主密钥同样走环境变量，这里与之保持一致。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use futures_util::StreamExt;
use tokio::task::JoinHandle;

use crate::business::pdf::dto::{PdfError, PdfOpts};

/// 默认 CDP 地址（与 `chromium-pdf.service` 的 `--remote-debugging-port` 对齐）。
pub const DEFAULT_CDP_URL: &str = "http://127.0.0.1:9222";

/// 默认并发上限。
///
/// 为什么是 3：实测（145，7.4G 内存）单次 500 行渲染峰值 PSS 约 190MiB，
/// 3 并发峰值 578MiB；再往上加并发只提升吞吐边际，却把内存风险留给生产。
pub const DEFAULT_MAX_CONCURRENCY: usize = 3;

/// 并发硬上限（防止运维把 `PDF_MAX_CONCURRENCY` 写成 100 把机器打爆）。
pub const MAX_CONCURRENCY_LIMIT: usize = 8;

/// 默认渲染超时（毫秒）。
///
/// 500 行单据实测 332ms，留 60 倍余量给「机器正忙 + 首次冷启动」。
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;

/// 等字体/图片就绪的最长轮询时间（毫秒）。
///
/// `document.fonts.status` 从 `loading` 变 `loaded` 通常在 5ms 内（本地字体），
/// 但首帧布局偶发要几十毫秒；超时也继续出 PDF（宁可有回退字体，也不要 500）。
const FONT_WAIT_MS: u64 = 2_000;

/// 一条 CDP 连接：`Browser` 负责发命令，`handler` task 负责收事件。
struct Conn {
    browser: Browser,
    handler: JoinHandle<()>,
}

impl Conn {
    /// 连接是否还活着（handler 结束 = WS 已断）。
    fn is_alive(&self) -> bool {
        !self.handler.is_finished()
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // 丢弃连接时把事件循环 task 一起收掉，避免 task 泄漏成僵尸
        self.handler.abort();
    }
}

/// 渲染引擎（进程级单例，见 [`global`]）。
pub struct PdfEngine {
    url: String,
    timeout: Duration,
    /// 并发上限（同时是连接池的容量上限）
    limit: usize,
    permits: Arc<tokio::sync::Semaphore>,
    pool: tokio::sync::Mutex<Vec<Conn>>,
    rendered_total: AtomicU64,
    failed_total: AtomicU64,
    active: AtomicU32,
    last_render_ms: AtomicU64,
    last_bytes: AtomicU64,
}

/// 引擎对外暴露的指标快照（`GET /business/pdf/health` 用）。
#[derive(Debug, Clone, Copy)]
pub struct EngineStats {
    /// 最大并发
    pub max_concurrency: u32,
    /// 当前在渲染中的请求数
    pub active: u32,
    /// 连接池空闲连接数
    pub idle_connections: usize,
    /// 累计成功数
    pub rendered_total: u64,
    /// 累计失败数
    pub failed_total: u64,
    /// 最近一次渲染耗时（毫秒）
    pub last_render_ms: u64,
    /// 最近一次 PDF 字节数
    pub last_bytes: u64,
    /// 渲染超时（毫秒）
    pub timeout_ms: u64,
}

impl PdfEngine {
    /// 构造引擎（**不做任何 I/O**，连接在首次渲染时懒建）。
    ///
    /// # 参数
    ///
    /// - `url`：CDP 地址；
    /// - `max_concurrency`：并发上限，会被裁剪到 `1..=MAX_CONCURRENCY_LIMIT`；
    /// - `timeout`：单次渲染总超时。
    pub fn new(url: impl Into<String>, max_concurrency: usize, timeout: Duration) -> Self {
        let limit = max_concurrency.clamp(1, MAX_CONCURRENCY_LIMIT);
        Self {
            url: url.into(),
            timeout,
            limit,
            permits: Arc::new(tokio::sync::Semaphore::new(limit)),
            pool: tokio::sync::Mutex::new(Vec::with_capacity(limit)),
            rendered_total: AtomicU64::new(0),
            failed_total: AtomicU64::new(0),
            active: AtomicU32::new(0),
            last_render_ms: AtomicU64::new(0),
            last_bytes: AtomicU64::new(0),
        }
    }

    /// 按环境变量构造（生产路径）。
    pub fn from_env() -> Self {
        Self::new(cdp_url(), max_concurrency_from_env(), timeout_from_env())
    }

    /// CDP 地址。
    pub fn url(&self) -> &str {
        &self.url
    }

    /// 指标快照。
    pub async fn stats(&self) -> EngineStats {
        let idle = self
            .pool
            .lock()
            .await
            .iter()
            .filter(|c| c.is_alive())
            .count();
        EngineStats {
            max_concurrency: self.limit as u32,
            active: self.active.load(Ordering::Relaxed),
            idle_connections: idle,
            rendered_total: self.rendered_total.load(Ordering::Relaxed),
            failed_total: self.failed_total.load(Ordering::Relaxed),
            last_render_ms: self.last_render_ms.load(Ordering::Relaxed),
            last_bytes: self.last_bytes.load(Ordering::Relaxed),
            timeout_ms: self.timeout.as_millis() as u64,
        }
    }

    /// 探测浏览器版本（建一条连接取 `/json/version` 结果，成功后连接回池）。
    ///
    /// # 返回值
    ///
    /// `(browser, protocol_version)`，例如 `("HeadlessChrome/157.0.8103.38", "1.3")`。
    ///
    /// # 异常情况
    ///
    /// - [`PdfError::Unavailable`]：CDP 连不上（服务没起 / 端口不对 / 进程崩了）。
    pub async fn probe(&self) -> std::result::Result<(String, String), PdfError> {
        let conn = self.checkout().await?;
        let version = match tokio::time::timeout(self.timeout, conn.browser.version()).await {
            Ok(Ok(v)) => {
                // `Product` 形如 `HeadlessChrome/157.0.8103.38`；个别发行版只给 userAgent
                let browser = if v.product.is_empty() {
                    v.user_agent
                } else {
                    v.product
                };
                Ok((browser, v.protocol_version))
            }
            Ok(Err(error)) => Err(PdfError::Unavailable(format!("CDP 取版本失败：{error}"))),
            Err(_) => Err(PdfError::Timeout(self.timeout.as_millis() as u64)),
        };
        if version.is_ok() {
            self.checkin(conn).await;
        }
        version
    }

    /// 渲染 HTML → PDF 字节。
    ///
    /// # 参数
    ///
    /// - `html`：**完整** HTML 文档（含 `<style>`）；调用方负责转义用户输入；
    /// - `opts`：纸张 / 边距 / 缩放等（已由 [`PdfOpts::validate`] 校验）。
    ///
    /// # 返回值
    ///
    /// PDF 文件字节（`%PDF-` 开头）。
    ///
    /// # 异常情况
    ///
    /// - 503 [`PdfError::Unavailable`]：CDP 不可用（已在内部重试一次）；
    /// - 504 [`PdfError::Timeout`]：超过 `PDF_RENDER_TIMEOUT_MS`；
    /// - 500 [`PdfError::Render`]：页面渲染失败。
    pub async fn render_pdf(
        &self,
        html: &str,
        opts: &PdfOpts,
    ) -> std::result::Result<Vec<u8>, PdfError> {
        let permit =
            match tokio::time::timeout(self.timeout, self.permits.clone().acquire_owned()).await {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Err(PdfError::Unavailable("渲染信号量已关闭".into())),
                Err(_) => return Err(PdfError::Timeout(self.timeout.as_millis() as u64)),
            };
        self.active.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let result = self.render_with_retry(html, opts).await;
        let elapsed = started.elapsed().as_millis() as u64;
        self.active.fetch_sub(1, Ordering::Relaxed);
        drop(permit);

        match &result {
            Ok(bytes) => {
                self.rendered_total.fetch_add(1, Ordering::Relaxed);
                self.last_render_ms.store(elapsed, Ordering::Relaxed);
                self.last_bytes.store(bytes.len() as u64, Ordering::Relaxed);
                tracing::info!(
                    elapsed_ms = elapsed,
                    bytes = bytes.len(),
                    page = opts.page.code(),
                    landscape = opts.landscape,
                    "PDF 渲染完成"
                );
            }
            Err(error) => {
                self.failed_total.fetch_add(1, Ordering::Relaxed);
                self.last_render_ms.store(elapsed, Ordering::Relaxed);
                tracing::warn!(elapsed_ms = elapsed, error = %error, "PDF 渲染失败");
            }
        }
        result
    }

    /// 渲染 + 连接层失败重试一次。
    async fn render_with_retry(
        &self,
        html: &str,
        opts: &PdfOpts,
    ) -> std::result::Result<Vec<u8>, PdfError> {
        let conn = self.checkout().await?;
        match self.render_on(&conn, html, opts).await {
            Ok(bytes) => {
                self.checkin(conn).await;
                Ok(bytes)
            }
            Err(error) if error.is_connection_failure() => {
                // 连接已废：Drop 会 abort handler，随后重建一条连接再试
                tracing::warn!(error = %error, "CDP 连接失效，重建后重试一次");
                drop(conn);
                let conn = self.checkout().await?;
                let result = self.render_on(&conn, html, opts).await;
                if result.is_ok() {
                    self.checkin(conn).await;
                }
                result
            }
            Err(error) => {
                // 渲染错误（模板 JS 异常等）与连接无关，连接仍可复用
                if conn.is_alive() {
                    self.checkin(conn).await;
                }
                Err(error)
            }
        }
    }

    /// 在一条连接上完成「建 page → 塞 HTML → 等字体 → printToPDF → 关 page」。
    async fn render_on(
        &self,
        conn: &Conn,
        html: &str,
        opts: &PdfOpts,
    ) -> std::result::Result<Vec<u8>, PdfError> {
        let params = print_params(opts);
        let work = async {
            let page = conn
                .browser
                .new_page("about:blank")
                .await
                .map_err(|error| PdfError::Unavailable(format!("新建页失败：{error}")))?;
            let outcome = async {
                page.set_content(html)
                    .await
                    .map_err(|error| PdfError::Render(format!("写入页面失败：{error}")))?;
                wait_ready(&page).await;
                page.pdf(params)
                    .await
                    .map_err(|error| PdfError::Render(format!("printToPDF 失败：{error}")))
            }
            .await;
            // 无论成功与否都要关掉 target，否则渲染进程会一直挂在内存里
            let _ = page.close().await;
            outcome
        };

        match tokio::time::timeout(self.timeout, work).await {
            Ok(result) => result,
            Err(_) => Err(PdfError::Timeout(self.timeout.as_millis() as u64)),
        }
    }

    /// 取一条可用连接：优先复用池里的，池空则新建。
    async fn checkout(&self) -> std::result::Result<Conn, PdfError> {
        while let Some(conn) = self.pool.lock().await.pop() {
            if conn.is_alive() {
                return Ok(conn);
            }
            // 已断（handler 结束）→ drop（abort 是 no-op）后继续取下一个
        }
        let (browser, mut handler) = Browser::connect(&self.url).await.map_err(|error| {
            PdfError::Unavailable(format!("连接 {url} 失败：{error}", url = self.url))
        })?;
        let handle = tokio::spawn(async move {
            // 事件循环：读到 None（对端关闭）或错误就退出；退出后 is_alive() 变 false
            while let Some(message) = handler.next().await {
                if message.is_err() {
                    break;
                }
            }
        });
        Ok(Conn {
            browser,
            handler: handle,
        })
    }

    /// 归还连接（连接已死或池已满则丢弃）。
    async fn checkin(&self, conn: Conn) {
        if !conn.is_alive() {
            return;
        }
        let mut pool = self.pool.lock().await;
        if pool.len() < self.limit {
            pool.push(conn);
        }
        // 池满（并发上限调小过）→ conn 在此处 drop，Drop 会 abort handler
    }
}

/// 进程级单例。
pub fn global() -> &'static PdfEngine {
    static ENGINE: OnceLock<PdfEngine> = OnceLock::new();
    ENGINE.get_or_init(PdfEngine::from_env)
}

/// 模块级便捷入口（供其它业务模块直接调用，不需要拿单例）。
///
/// # 参数
///
/// - `html`：完整 HTML；
/// - `opts`：渲染参数。
///
/// # 返回值 / 异常情况
///
/// 同 [`PdfEngine::render_pdf`]。
pub async fn render_pdf(html: &str, opts: &PdfOpts) -> std::result::Result<Vec<u8>, PdfError> {
    global().render_pdf(html, opts).await
}

/// 组装 CDP 的 `Page.printToPDF` 参数（**纯函数**，可单测）。
///
/// # 参数
///
/// - `opts`：渲染参数（毫米已在 [`PdfOpts::paper_inches`] 换成英寸）。
///
/// # 返回值
///
/// 可直接交给 `Page::pdf` 的 `PrintToPdfParams`。
pub fn print_params(opts: &PdfOpts) -> PrintToPdfParams {
    let (width_in, height_in) = opts.paper_inches();
    let mut builder = PrintToPdfParams::builder()
        .paper_width(width_in)
        .paper_height(height_in)
        .margin_top(opts.margins.top_mm / 25.4)
        .margin_bottom(opts.margins.bottom_mm / 25.4)
        .margin_left(opts.margins.left_mm / 25.4)
        .margin_right(opts.margins.right_mm / 25.4)
        .print_background(opts.print_background)
        .landscape(opts.landscape)
        .scale(opts.scale)
        .prefer_css_page_size(opts.prefer_css_page_size)
        .display_header_footer(opts.display_header_footer);
    if let Some(template) = &opts.header_template {
        builder = builder.header_template(template.clone());
    }
    if let Some(template) = &opts.footer_template {
        builder = builder.footer_template(template.clone());
    }
    builder.build()
}

/// 等 `document.fonts` 与图片就绪（最多 [`FONT_WAIT_MS`]）。
///
/// 不做失败处理：拿不到就继续出 PDF（回退字体好过 500），只在日志里留痕。
async fn wait_ready(page: &chromiumoxide::page::Page) {
    let deadline = Instant::now() + Duration::from_millis(FONT_WAIT_MS);
    while Instant::now() < deadline {
        let ready: bool = page
            .evaluate(
                "(document.fonts.status === 'loaded') && \
                 Array.prototype.every.call(document.images, (img) => img.complete)",
            )
            .await
            .ok()
            .and_then(|result| result.into_value().ok())
            .unwrap_or(false);
        if ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tracing::debug!("字体/图片等待超时，按当前状态出 PDF");
}

/// CDP 地址（`PDF_CDP_URL`）。
pub fn cdp_url() -> String {
    std::env::var("PDF_CDP_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CDP_URL.to_string())
}

/// 并发上限（`PDF_MAX_CONCURRENCY`，越界裁剪并告警）。
pub fn max_concurrency_from_env() -> usize {
    match std::env::var("PDF_MAX_CONCURRENCY") {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(value) if (1..=MAX_CONCURRENCY_LIMIT).contains(&value) => value,
            Ok(value) => {
                let clamped = value.clamp(1, MAX_CONCURRENCY_LIMIT);
                tracing::warn!(
                    raw = value,
                    clamped,
                    "PDF_MAX_CONCURRENCY 越界，已裁剪到 [{}, {}]",
                    1,
                    MAX_CONCURRENCY_LIMIT
                );
                clamped
            }
            Err(_) => {
                tracing::warn!(raw = %raw, "PDF_MAX_CONCURRENCY 不是数字，回退默认值");
                DEFAULT_MAX_CONCURRENCY
            }
        },
        Err(_) => DEFAULT_MAX_CONCURRENCY,
    }
}

/// 渲染超时（`PDF_RENDER_TIMEOUT_MS`）。
pub fn timeout_from_env() -> Duration {
    let millis = std::env::var("PDF_RENDER_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| (500..=120_000).contains(value))
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::pdf::dto::{PageSize, PdfMargins};

    /// A4 纵向：CDP 参数必须是 8.2677 × 11.6929 英寸、1cm 默认边距换算成 0.3937 英寸。
    #[test]
    fn print_params_map_a4_portrait() {
        let params = print_params(&PdfOpts::default());
        assert!((params.paper_width.unwrap() - 8.2677).abs() < 1e-3);
        assert!((params.paper_height.unwrap() - 11.6929).abs() < 1e-3);
        // 默认 10mm 边距 = 0.3937 英寸
        assert!((params.margin_top.unwrap() - 0.3937).abs() < 1e-3);
        assert!((params.margin_left.unwrap() - 0.3937).abs() < 1e-3);
        assert_eq!(params.landscape, Some(false));
        assert_eq!(params.print_background, Some(true));
        assert_eq!(params.scale, Some(1.0));
        assert_eq!(params.prefer_css_page_size, Some(false));
        assert_eq!(params.display_header_footer, Some(false));
        assert!(params.header_template.is_none());
        assert!(params.footer_template.is_none());
    }

    /// 横向 + 自定义纸张 + 边距 + 缩放 + 页脚模板都要原样落到 CDP 参数。
    #[test]
    fn print_params_map_custom_landscape() {
        let opts = PdfOpts {
            page: PageSize::Custom {
                width_mm: 241.0,
                height_mm: 140.0,
            },
            landscape: true,
            margins: PdfMargins {
                top_mm: 5.0,
                right_mm: 6.0,
                bottom_mm: 7.0,
                left_mm: 8.0,
            },
            scale: 0.85,
            display_header_footer: true,
            footer_template: Some("<span class='pageNumber'></span>".into()),
            ..PdfOpts::default()
        };
        let params = print_params(&opts);
        // 横向 = 交换宽高，所以 paper_width 应是 height_mm=140mm=5.5118in
        assert!((params.paper_width.unwrap() - 140.0 / 25.4).abs() < 1e-6);
        assert!((params.paper_height.unwrap() - 241.0 / 25.4).abs() < 1e-6);
        assert!((params.margin_top.unwrap() - 5.0 / 25.4).abs() < 1e-6);
        assert!((params.margin_bottom.unwrap() - 7.0 / 25.4).abs() < 1e-6);
        assert!((params.margin_left.unwrap() - 8.0 / 25.4).abs() < 1e-6);
        assert!((params.margin_right.unwrap() - 6.0 / 25.4).abs() < 1e-6);
        assert_eq!(params.landscape, Some(true));
        assert_eq!(params.scale, Some(0.85));
        assert_eq!(params.display_header_footer, Some(true));
        assert!(params.footer_template.is_some());
    }

    /// 并发数越界必须裁剪到 1–8（不许把信号量开到 100 打爆机器）。
    #[test]
    fn concurrency_is_clamped() {
        let engine = PdfEngine::new("http://127.0.0.1:9222", 0, Duration::from_secs(1));
        assert_eq!(engine.permits.available_permits(), 1);
        let engine = PdfEngine::new("http://127.0.0.1:9222", 64, Duration::from_secs(1));
        assert_eq!(engine.permits.available_permits(), MAX_CONCURRENCY_LIMIT);
        let engine = PdfEngine::new("http://127.0.0.1:9222", 3, Duration::from_secs(1));
        assert_eq!(engine.permits.available_permits(), 3);
    }

    /// 构造引擎不做 I/O：池空、指标全 0、地址原样保存。
    #[test]
    fn new_engine_has_no_side_effects() {
        let engine = PdfEngine::new("http://127.0.0.1:9222", 3, Duration::from_millis(500));
        assert_eq!(engine.url(), "http://127.0.0.1:9222");
        assert!(engine.pool.try_lock().expect("池锁应可用").is_empty());
        assert_eq!(engine.active.load(Ordering::Relaxed), 0);
        assert_eq!(engine.rendered_total.load(Ordering::Relaxed), 0);
    }

    /// 环境变量默认值（不设时必须是文档里写的那些数）。
    #[test]
    fn env_defaults() {
        // 注意：测试进程环境可能被外部设置，这里只断言「不 panic 且落在合法区间」
        let url = cdp_url();
        assert!(url.starts_with("http") || url.starts_with("ws"));
        let concurrency = max_concurrency_from_env();
        assert!((1..=MAX_CONCURRENCY_LIMIT).contains(&concurrency));
        let timeout = timeout_from_env();
        assert!(timeout >= Duration::from_millis(500));
    }
}
