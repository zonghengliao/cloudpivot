//! Markdown 内容管线（批次 C · C1）：**渲染 → 净化 → 目录树**。
//!
//! 依据 `docs/批次C-技术架构.md`：§2.4（内容形态与 XSS 四道闸）、§3.2（TOC 锚点）、
//! §5.4.1 B-1（新增 `pulldown-cmark`）、§5.5.1/§5.5.2（A-13~A-16、H-10~H-12 断言）。
//!
//! # 为什么公告与帮助文档**共用**这一份实现
//!
//! 一套渲染器只有一套 bug；两套（前端一套、后端一套）就会出现「同一份 Markdown 两边渲染不一样」。
//! 公告（C2/C3）与帮助文档（C4）的差异只有**配方**（[`Recipe`]）：允许的标签、允许的代码语言、
//! 是否保留 heading 的锚点 `id`。差异全部收在 [`Recipe`] 与 [`sanitize`] 里，渲染算法只有一条。
//!
//! # 安全模型（四道闸里的闸 1 + 闸 2，本文件负责）
//!
//! 1. [`ENABLE_HTML`] **恒为 `false`**：Markdown 里内嵌的原始 HTML（`<div onclick=...>`、
//!    `<script>`、`<iframe>`）一律**降级为文本**，由 `push_html` 转义成 `&lt;div`；
//! 2. 外链图片一律移除（替换为 [`IMAGE_REMOVED`]）：防 SSRF、防追踪像素（`![](http://evil/x.png)`）；
//! 3. 链接协议白名单：只允许 `http` / `https` / `mailto` 与站内相对路径，其余（`javascript:`、
//!    `file:`、`data:`、自定义 scheme）**整段降级为纯文本**（保留文字，去掉 `<a>`）；
//! 4. 渲染完再走 `ammonia` 净化（闸 2 入库前再净化；闸 3 出参再净化由 C2/C4 的 handler 调用
//!    [`render`] 或 [`sanitize_html`] 完成，幂等 —— POC-3 已验证）。
//!
//! # 已知的框架坑（都实测过，别重踩）
//!
//! - `pulldown-cmark` 0.13 **没有**名为 `ENABLE_HTML` 的 `Options` 位：原始 HTML 一定以
//!   [`Event::Html`] / [`Event::InlineHtml`] 进入事件流，而 `html::push_html` 会把它**原样写出**。
//!   所以「关闭 HTML」在实现上就是 [`ENABLE_HTML`] 这个常量：为 `false` 时把 HTML 事件降级成
//!   **文本事件**（转义交给 `push_html`）。改成 `true` 等于给 XSS 开门，**红线**。
//! - `ammonia` 默认把 `id` **静默剥离**（`generic_attributes` 只有 `lang`/`title`），帮助文档的
//!   TOC 锚点必须显式 `add_generic_attributes(&["id"])`（POC-13 实测：默认 `<h2>维护时间</h2>`，
//!   加白后 `<h2 id="h-2-1">维护时间</h2>`）。
//! - `ammonia` 默认 `allowed_classes` 为空，`<code class="language-x">` 的类**也**会被剥离；
//!   要保留只能逐个登记静态字面量（[`language_class`] 因此返回 `&'static str`，不用 `format!`）。
//! - `ammonia` 4.2.x 若同时声明 `a` 的 `rel` 会 **panic**（`assert!(tag_attributes["a"]["rel"].is_none())`），
//!   所以只允许 `target`，`rel="noopener noreferrer"` 由净化器自动补齐。

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

/// `ENABLE_HTML = false`（**硬要求，不得改为 `true`**）。
///
/// 含义见模块文档第 1 条：为 `false` 时，Markdown 里的原始 HTML 一律降级为文本事件
/// （输出 `&lt;div`）；为 `true` 时才会原样透传 —— 那等于把富文本入口重新打开，闸 1 白做。
pub const ENABLE_HTML: bool = false;

/// 编译期红线：把 [`ENABLE_HTML`] 改成 `true` 会**直接编译失败**（不是靠测试兜）。
///
/// 允许 HTML 等于给 XSS 开门（架构 §2.4.3 闸 1 的前提），所以这条断言写在类型层。
const _: () = assert!(!ENABLE_HTML, "ENABLE_HTML 必须恒为 false（红线，见架构 §2.4.3）");

/// 外链图片被移除后写进正文的占位文本（架构 §2.4.3 加固项：图片只允许 storage URL）。
pub const IMAGE_REMOVED: &str = "[图片已移除]";

/// 允许作为图片源的站内前缀（其余一律移除：外链追踪像素 / `data:` / `javascript:`）。
const ALLOWED_IMAGE_PREFIXES: &[&str] = &["/api/business/storage/", "/api/business/announcement/"];

/// 链接协议白名单（架构 §2.4.3：`javascript:` / `file:` / 自定义 scheme 一律降级为纯文本）。
const ALLOWED_LINK_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// 公告配方允许的代码块语言：**运维 / 配置类**（公告里贴的是值班表、命令、SQL、配置）。
///
/// 公告面向全员，代码块只是「偶尔贴一段命令」，语言面收窄到白名单，多一个少一个都要评审。
const ANNOUNCEMENT_CODE_LANGS: &[&str] = &[
    "text",
    "plaintext",
    "bash",
    "shell",
    "sh",
    "json",
    "yaml",
    "toml",
    "ini",
    "sql",
    "diff",
    "markdown",
    "http",
    "csv",
];

/// 帮助配方在公告集合之外**额外**允许的语言（架构 §7 C1：「帮助 = 同上 + 更宽的代码语言」）。
///
/// 帮助文档是技术文档，要贴各语言示例；白名单仍是封闭集合，只是集合更大。
const HELP_ONLY_CODE_LANGS: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "java",
    "go",
    "c",
    "cpp",
    "csharp",
    "php",
    "ruby",
    "kotlin",
    "swift",
    "scala",
    "lua",
    "perl",
    "powershell",
    "dockerfile",
    "makefile",
    "proto",
    "xml",
    "html",
    "css",
    "vue",
    "jsx",
    "tsx",
    "graphql",
    "regex",
    "gradle",
    "properties",
];

/// 三套净化配方（公告 / 帮助 / 纯文本）。
///
/// | 配方 | 图片 | 链接 | 表格 | 代码块语言 | heading `id`（TOC 锚点） |
/// |------|------|------|------|-----------|----------------------|
/// | [`Recipe::Announcement`] | 仅 storage | 协议白名单 | 允许 | 运维/配置 14 种 | **剥离**（公告没有目录） |
/// | [`Recipe::Help`] | 仅 storage | 协议白名单 | 允许 | 公告集合 + 30 种 | **保留** `h-{level}-{index}` |
/// | [`Recipe::PlainText`] | 全部移除 | 全部展平为文字 | 保留文字 | 只留代码文字 | 不产生 |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipe {
    /// 公告（C2/C3）：允许图片 / 链接 / 表格 / 代码块，**不**保留 heading 锚点 `id`。
    Announcement,
    /// 帮助文档（C4）：在公告基础上加更宽的代码语言，并**保留** heading 锚点 `id`（TOC 跳转）。
    Help,
    /// 纯文本（检索 `snippet` / 摘要 / 邮件正文）：所有标签剥掉，只留文字。
    PlainText,
}

impl Recipe {
    /// 配方的稳定名称（写日志 / 断言用）。
    ///
    /// # 返回值
    ///
    /// `"announcement"` / `"help"` / `"plain_text"`。
    pub fn name(self) -> &'static str {
        match self {
            Self::Announcement => "announcement",
            Self::Help => "help",
            Self::PlainText => "plain_text",
        }
    }
}

/// 一个标题的扁平引用（文档顺序），[`Rendered::headings`] 的元素。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadingRef {
    /// **已偏移后**的层级（正文 `#` → `2`）：与输出的 `<h2>` 一致。
    pub level: u8,
    /// 标题纯文本（已折叠空白）。
    pub text: String,
    /// 锚点 `id`，形如 `h-2-1`（`h-{偏移后层级}-{同层序号}`）。
    pub id: String,
}

/// 目录树节点（[`Rendered::toc`] 的元素）：从 [`HeadingRef`] 按层级嵌套出来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocNode {
    /// 偏移后的层级（2..=6）。
    pub level: u8,
    /// 标题纯文本。
    pub text: String,
    /// 锚点 `id`。
    pub id: String,
    /// 子节点（层级更深的下一个标题；层级回退时挂到更外层）。
    pub children: Vec<TocNode>,
}

/// 一次渲染的全部产物：HTML、纯文本、标题列表、目录树，以及被移除内容的计数（埋点用）。
#[derive(Clone, Debug, Default)]
pub struct Rendered {
    /// 净化后的 HTML（可直接落库 / 出参；`Recipe::PlainText` 时是转义后的纯文本）。
    pub content_html: String,
    /// 纯文本（标签全剥，保留段落换行）—— 检索 `snippet` 与摘要都从这里来。
    pub plain_text: String,
    /// 扁平标题列表（文档顺序；已偏移层级、已生成 `id`）。
    pub headings: Vec<HeadingRef>,
    /// 目录树（`Recipe::PlainText` 时恒为空）。
    pub toc: Vec<TocNode>,
    /// 被移除的外链图片数量。
    pub removed_images: u32,
    /// 被降级为纯文本的危险链接数量。
    pub removed_links: u32,
    /// 被转义（而非透传）的原始 HTML 片段数量。
    pub escaped_html: u32,
}

/// 按配方渲染并净化一段 Markdown（本模块的**唯一入口**）。
///
/// # 参数
///
/// - `markdown`：Markdown 源（公告 / 帮助文档的 `content_md`）。
/// - `recipe`：净化配方（[`Recipe`]）。
///
/// # 返回值
///
/// [`Rendered`]：`content_html` / `plain_text` / `headings` / `toc` + 三个移除计数。
///
/// # 幂等
///
/// 对同一输入恒等（无随机、无时间、无 ID 抖动）；对 `content_html` 再净化一次输出不变
/// （ammonia 幂等，POC-3 已验证）—— 这是「闸 2 入库再净化」的前提。
pub fn render(markdown: &str, recipe: Recipe) -> Rendered {
    let walker = Walker::run(markdown, recipe);
    let mut raw_html = String::with_capacity(markdown.len() + markdown.len() / 2);
    html::push_html(&mut raw_html, walker.events.into_iter());

    let plain_text = normalize_plain(&walker.plain);
    let content_html = match recipe {
        // 纯文本：标签全剥，但输出仍需转义（可能被贴进 HTML 里当文本用）
        Recipe::PlainText => escape_html(&plain_text),
        _ => sanitize(&raw_html, recipe),
    };

    Rendered {
        content_html,
        plain_text,
        // 纯文本配方不产生目录（剪藏 / 摘要 / snippet 用不到锚点）
        toc: if recipe == Recipe::PlainText {
            Vec::new()
        } else {
            build_toc(&walker.headings)
        },
        headings: walker.headings,
        removed_images: walker.removed_images,
        removed_links: walker.removed_links,
        escaped_html: walker.escaped_html,
    }
}

/// 公告配方（`Recipe::Announcement`）的便捷入口。
///
/// # 参数
///
/// - `markdown`：公告正文 Markdown。
///
/// # 返回值
///
/// 同 [`render`]。
pub fn render_announcement(markdown: &str) -> Rendered {
    render(markdown, Recipe::Announcement)
}

/// 帮助文档配方（`Recipe::Help`）的便捷入口（保留 heading 锚点 `id`，带 TOC）。
///
/// # 参数
///
/// - `markdown`：帮助文档正文 Markdown。
///
/// # 返回值
///
/// 同 [`render`]。
pub fn render_help(markdown: &str) -> Rendered {
    render(markdown, Recipe::Help)
}

/// 把 Markdown 压成纯文本（检索 `snippet` / 摘要 / 邮件正文用）。
///
/// # 参数
///
/// - `markdown`：任意 Markdown。
///
/// # 返回值
///
/// 标签全剥、空白折叠后的纯文本（`<` / `>` 不会残留）。
pub fn to_plain_text(markdown: &str) -> String {
    render(markdown, Recipe::PlainText).plain_text
}

/// 对**已渲染**的 HTML 再净化一次（闸 3：出参前再净化，幂等）。
///
/// # 参数
///
/// - `content_html`：已经渲染（或落库）的 HTML。
/// - `recipe`：沿用渲染时的配方（决定是否保留 heading 的 `id`）。
///
/// # 返回值
///
/// 净化后的 HTML；输入若是纯文本则原样转义。
pub fn sanitize_html(content_html: &str, recipe: Recipe) -> String {
    match recipe {
        Recipe::PlainText => escape_html(content_html),
        _ => sanitize(content_html, recipe),
    }
}

/// 从扁平标题列表构造目录树（层级嵌套）。
///
/// # 参数
///
/// - `headings`：文档顺序的标题（[`Rendered::headings`]）。
///
/// # 返回值
///
/// 顶层节点列表；层级更深的后继标题挂进前一节点的 `children`，层级回退时逐层出栈。
pub fn build_toc(headings: &[HeadingRef]) -> Vec<TocNode> {
    // 顶层用 parent_level = 1（偏移后层级最小是 2），递归下降即「栈式」嵌套
    build_toc_from(headings, 0, 1).0
}

/// [`build_toc`] 的递归体：返回（本层节点，消费到的下标）。
///
/// # 参数
///
/// - `headings`：扁平标题列表。
/// - `start`：从哪个下标开始。
/// - `parent_level`：父节点的层级（比它浅的兄弟停在这里）。
///
/// # 返回值
///
/// `(节点列表, 下一个待处理下标)`。
fn build_toc_from(headings: &[HeadingRef], start: usize, parent_level: u8) -> (Vec<TocNode>, usize) {
    let mut nodes = Vec::new();
    let mut index = start;
    while index < headings.len() {
        let heading = &headings[index];
        if heading.level <= parent_level {
            break;
        }
        let (children, next) = build_toc_from(headings, index + 1, heading.level);
        nodes.push(TocNode {
            level: heading.level,
            text: heading.text.clone(),
            id: heading.id.clone(),
            children,
        });
        index = next;
    }
    (nodes, index)
}

/// 解析器选项：表格 / 删除线 / 任务列表打开（架构 §2.4.3 固定参数表）。
///
/// 不做语法高亮（服务端只输出 `<pre><code class="language-x">`，前端按需高亮），
/// 不打开 `ENABLE_MATH` / `ENABLE_FOOTNOTES` 等公告用不到的扩展（面越小越好）。
///
/// # 返回值
///
/// `pulldown-cmark` 的选项集合。
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// 标题层级偏移 +1（正文 `#` → `<h2>`）：页面已有 `<h1>` 标题，避免第二个 h1 破坏无障碍大纲。
///
/// # 参数
///
/// - `level`：Markdown 的标题层级（`H1`..`H6`）。
///
/// # 返回值
///
/// 2..=6 的层级（`H6` 封顶在 `6`）。
fn shift_level(level: HeadingLevel) -> u8 {
    let raw = match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    };
    (raw + 1).min(6)
}

/// 语言 token → `<code class="…">` 的类名（**必须是静态字面量**）。
///
/// `ammonia` 的 `allowed_classes` 借用 `&'a str`，`format!` 拼出来的 `String` 既借不长也白分配。
///
/// # 参数
///
/// - `token`：代码块围栏上的第一个词（已小写）。
///
/// # 返回值
///
/// `Some("language-x")`；不在白名单内返回 `None`（该语言退化成裸 `<pre><code>`）。
fn language_class(token: &str) -> Option<&'static str> {
    let class = match token {
        "text" | "plaintext" => "language-text",
        "bash" => "language-bash",
        "shell" => "language-shell",
        "sh" => "language-sh",
        "json" => "language-json",
        "yaml" => "language-yaml",
        "toml" => "language-toml",
        "ini" => "language-ini",
        "sql" => "language-sql",
        "diff" => "language-diff",
        "markdown" => "language-markdown",
        "http" => "language-http",
        "csv" => "language-csv",
        "rust" => "language-rust",
        "python" => "language-python",
        "javascript" => "language-javascript",
        "typescript" => "language-typescript",
        "java" => "language-java",
        "go" => "language-go",
        "c" => "language-c",
        "cpp" => "language-cpp",
        "csharp" => "language-csharp",
        "php" => "language-php",
        "ruby" => "language-ruby",
        "kotlin" => "language-kotlin",
        "swift" => "language-swift",
        "scala" => "language-scala",
        "lua" => "language-lua",
        "perl" => "language-perl",
        "powershell" => "language-powershell",
        "dockerfile" => "language-dockerfile",
        "makefile" => "language-makefile",
        "proto" => "language-proto",
        "xml" => "language-xml",
        "html" => "language-html",
        "css" => "language-css",
        "vue" => "language-vue",
        "jsx" => "language-jsx",
        "tsx" => "language-tsx",
        "graphql" => "language-graphql",
        "regex" => "language-regex",
        "gradle" => "language-gradle",
        "properties" => "language-properties",
        _ => return None,
    };
    Some(class)
}

/// 该配方是否允许这种代码块语言。
///
/// # 参数
///
/// - `recipe`：净化配方。
/// - `token`：代码块围栏上的第一个词（已小写）。
///
/// # 返回值
///
/// 允许返回 `true`（且 [`language_class`] 必然有映射）。
fn lang_allowed(recipe: Recipe, token: &str) -> bool {
    let in_announcement = ANNOUNCEMENT_CODE_LANGS.contains(&token);
    let in_help_only = HELP_ONLY_CODE_LANGS.contains(&token);
    match recipe {
        Recipe::Announcement => in_announcement,
        Recipe::Help => in_announcement || in_help_only,
        Recipe::PlainText => false,
    }
}

/// 该配方需要向 `ammonia` 登记的 `code` 类名全集（静态字面量，见 [`language_class`]）。
///
/// # 参数
///
/// - `recipe`：净化配方。
///
/// # 返回值
///
/// 类名列表（去重；`Recipe::PlainText` 为空）。
fn code_classes(recipe: Recipe) -> Vec<&'static str> {
    let mut classes: Vec<&'static str> = Vec::new();
    if recipe == Recipe::PlainText {
        return classes;
    }
    for token in ANNOUNCEMENT_CODE_LANGS.iter().chain(HELP_ONLY_CODE_LANGS.iter()) {
        if !lang_allowed(recipe, token) {
            continue;
        }
        if let Some(class) = language_class(token)
            && !classes.contains(&class)
        {
            classes.push(class);
        }
    }
    classes
}

/// 图片源是否允许保留：只允许站内 storage（`/api/business/storage/` 或公告附件前缀）。
///
/// # 参数
///
/// - `dest`：Markdown 里的图片 URL。
///
/// # 返回值
///
/// 允许返回 `true`；外链、`data:`、`javascript:` 一律 `false`（替换为 [`IMAGE_REMOVED`]）。
fn image_allowed(dest: &str) -> bool {
    let dest = dest.trim();
    if dest.is_empty() {
        return false;
    }
    ALLOWED_IMAGE_PREFIXES.iter().any(|prefix| dest.starts_with(prefix))
}

/// 链接是否允许保留成 `<a>`：协议白名单 + 站内相对路径。
///
/// # 参数
///
/// - `dest`：Markdown 里的链接 URL。
///
/// # 返回值
///
/// 允许返回 `true`（`http` / `https` / `mailto` / 相对路径 / `#锚点`）；
/// `javascript:` / `file:` / `data:` / 自定义 scheme 返回 `false`（整段降级为纯文本）。
fn link_allowed(dest: &str) -> bool {
    let dest = dest.trim();
    if dest.is_empty() {
        return false;
    }
    // 站内相对路径 / 锚点 / 查询串：渲染器不会给它们加协议，天然安全
    if dest.starts_with('/') || dest.starts_with('#') || dest.starts_with('?') {
        return true;
    }
    match dest.split_once(':') {
        Some((scheme, _)) => {
            let scheme = scheme.to_ascii_lowercase();
            ALLOWED_LINK_SCHEMES.contains(&scheme.as_str())
        }
        // 没有冒号 → 相对路径（`docs/page.md` 之类）
        None => true,
    }
}

/// 走一遍事件流，产出（已改写的）事件序列 + 纯文本 + 标题列表 + 移除计数。
///
/// 这里的「改写」是全部安全逻辑的落点：heading 偏移与 `id`、原始 HTML 降级、外链图片移除、
/// 危险链接降级、代码语言白名单。
struct Walker<'a> {
    /// 净化配方。
    recipe: Recipe,
    /// 改写后的事件序列（交给 `html::push_html`）。
    events: Vec<Event<'a>>,
    /// 纯文本累积（标签全剥）。
    plain: String,
    /// 扁平标题列表（文档顺序）。
    headings: Vec<HeadingRef>,
    /// 当前正在收集的标题（`Start(Heading)` 到 `End(Heading)` 之间）。
    heading: Option<HeadingBuf<'a>>,
    /// 同层序号计数器（下标即偏移后的层级 2..=6）。
    counter: [usize; 7],
    /// 正处在「被移除的图片」内部的深度（图片 alt 里的事件全部丢弃）。
    drop_image_depth: usize,
    /// 正处在「被降级为纯文本的链接」内部的深度（丢 `<a>`，保留内部文字）。
    drop_link_depth: usize,
    /// 已移除的图片数。
    removed_images: u32,
    /// 已降级的链接数。
    removed_links: u32,
    /// 已转义的原始 HTML 片段数。
    escaped_html: u32,
}

/// 正在收集中的标题：`inner` 是标题内部的 inline 事件，等 `End(Heading)` 时整体吐出。
struct HeadingBuf<'a> {
    /// 偏移后的层级。
    level: u8,
    /// 锚点 `id`（`h-{level}-{index}`）。
    id: String,
    /// 标题纯文本（TOC 用）。
    text: String,
    /// 标题内部的 inline 事件（粗体 / 代码 / 链接等）。
    inner: Vec<Event<'a>>,
}

impl<'a> Walker<'a> {
    /// 走完一段 Markdown，返回收集完成的 walker。
    ///
    /// # 参数
    ///
    /// - `markdown`：Markdown 源。
    /// - `recipe`：净化配方。
    ///
    /// # 返回值
    ///
    /// 各字段均已填充的 walker（[`render`] 负责收尾）。
    fn run(markdown: &'a str, recipe: Recipe) -> Self {
        let mut walker = Self {
            recipe,
            events: Vec::new(),
            plain: String::with_capacity(markdown.len()),
            headings: Vec::new(),
            heading: None,
            counter: [0; 7],
            drop_image_depth: 0,
            drop_link_depth: 0,
            removed_images: 0,
            removed_links: 0,
            escaped_html: 0,
        };
        for event in Parser::new_ext(markdown, options()) {
            walker.handle(event);
        }
        walker
    }

    /// 分发单个解析事件。
    ///
    /// # 参数
    ///
    /// - `event`：`pulldown-cmark` 事件。
    fn handle(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag_end) => self.end(tag_end),
            Event::Text(text) => {
                if self.drop_image_depth > 0 {
                    return;
                }
                self.plain.push_str(&text);
                if let Some(heading) = self.heading.as_mut() {
                    heading.text.push_str(&text);
                }
                self.emit(Event::Text(text));
            }
            Event::Code(code) => {
                if self.drop_image_depth > 0 {
                    return;
                }
                self.plain.push_str(&code);
                if let Some(heading) = self.heading.as_mut() {
                    heading.text.push_str(&code);
                }
                self.emit(Event::Code(code));
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                if self.drop_image_depth > 0 {
                    return;
                }
                if ENABLE_HTML {
                    // 只有把红线常量改成 true 才会走到这里（当前不可达）
                    self.emit(Event::Html(raw));
                } else {
                    // 降级成文本事件：push_html 负责转义 → 输出 `&lt;div`
                    self.escaped_html += 1;
                    self.emit(Event::Text(raw));
                }
            }
            Event::SoftBreak => {
                if self.drop_image_depth > 0 {
                    return;
                }
                self.plain.push(' ');
                self.emit(Event::SoftBreak);
            }
            Event::HardBreak => {
                if self.drop_image_depth > 0 {
                    return;
                }
                self.plain.push('\n');
                self.emit(Event::HardBreak);
            }
            other => {
                if self.drop_image_depth > 0 {
                    return;
                }
                self.emit(other);
            }
        }
    }

    /// 处理「开始标签」。
    ///
    /// # 参数
    ///
    /// - `tag`：开始标签。
    fn start(&mut self, tag: Tag<'a>) {
        if self.drop_image_depth > 0 {
            // 图片 alt 里的嵌套图片：只维护深度，不产出任何事件
            if let Tag::Image { .. } = tag {
                self.drop_image_depth += 1;
            }
            return;
        }
        match tag {
            // ① 标题：偏移 +1 并生成稳定锚点 `id`
            Tag::Heading { level, .. } => {
                let shifted = shift_level(level);
                self.counter[shifted as usize] += 1;
                let index = self.counter[shifted as usize];
                self.heading = Some(HeadingBuf {
                    level: shifted,
                    id: format!("h-{shifted}-{index}"),
                    text: String::new(),
                    inner: Vec::new(),
                });
            }
            // ② 图片：外链一律移除（防 SSRF / 追踪像素）
            Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            } => {
                if self.recipe != Recipe::PlainText && image_allowed(&dest_url) {
                    self.emit(Event::Start(Tag::Image {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                } else {
                    self.drop_image_depth = 1;
                    self.removed_images += 1;
                    self.emit(Event::Text(IMAGE_REMOVED.into()));
                }
            }
            // ③ 链接：协议白名单之外整段降级为纯文本（保留文字）
            Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            } => {
                if link_allowed(&dest_url) {
                    self.emit(Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                } else {
                    self.removed_links += 1;
                    self.drop_link_depth += 1;
                }
            }
            // ④ 代码块：语言必须在配方白名单内，否则退化成裸 `<pre><code>`
            Tag::CodeBlock(kind) => {
                let kind = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let token = info.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
                        if lang_allowed(self.recipe, &token) {
                            CodeBlockKind::Fenced(token.into())
                        } else {
                            CodeBlockKind::Fenced("".into())
                        }
                    }
                    CodeBlockKind::Indented => CodeBlockKind::Indented,
                };
                self.emit(Event::Start(Tag::CodeBlock(kind)));
            }
            other => self.emit(Event::Start(other)),
        }
    }

    /// 处理「结束标签」。
    ///
    /// # 参数
    ///
    /// - `tag_end`：结束标签。
    fn end(&mut self, tag_end: TagEnd) {
        if matches!(tag_end, TagEnd::Image) && self.drop_image_depth > 0 {
            self.drop_image_depth -= 1;
            return;
        }
        if self.drop_image_depth > 0 {
            return;
        }
        if matches!(tag_end, TagEnd::Link) && self.drop_link_depth > 0 {
            self.drop_link_depth -= 1;
            return;
        }
        if let TagEnd::Heading(_) = tag_end {
            self.flush_heading();
            return;
        }
        self.emit(Event::End(tag_end));
    }

    /// 标题结束：把缓冲的 inline 事件包进我们自己生成的 `<hN id="..">`，并记账标题。
    fn flush_heading(&mut self) {
        let Some(heading) = self.heading.take() else {
            return;
        };
        let level = heading.level;
        let id = heading.id;
        let text = collapse_ws(&heading.text);
        if !text.is_empty() || !heading.inner.is_empty() {
            self.headings.push(HeadingRef {
                level,
                text: text.clone(),
                id: id.clone(),
            });
        }
        // `id` 由本模块生成（`h-{level}-{index}`，不透传用户输入），可直接写进 HTML
        self.events.push(Event::Html(format!("<h{level} id=\"{id}\">").into()));
        self.events.extend(heading.inner);
        self.events.push(Event::Html(format!("</h{level}>").into()));
        // 标题文本已随 Text/Code 事件进过纯文本（见 `handle`），这里只补一个换行做分隔
        self.plain.push('\n');
    }

    /// 把事件放进「当前标题的缓冲」或「输出序列」。
    ///
    /// # 参数
    ///
    /// - `event`：要产出的事件。
    fn emit(&mut self, event: Event<'a>) {
        match self.heading.as_mut() {
            Some(heading) => heading.inner.push(event),
            None => self.events.push(event),
        }
    }
}

/// 走一遍 `ammonia` 净化（闸 1 的净化 / 闸 2 入库前再净化 / 闸 3 出参再净化，同一份规则）。
///
/// # 参数
///
/// - `raw_html`：渲染出来的 HTML。
/// - `recipe`：净化配方。
///
/// # 返回值
///
/// 净化后的 HTML。
fn sanitize(raw_html: &str, recipe: Recipe) -> String {
    let mut builder = ammonia::Builder::default();
    // ① 只补 `target`：⚠️ 千万不要写成 `&["target", "rel"]`（ammonia 4.2.x 会 panic，
    //    `assert!(tag_attributes["a"]["rel"].is_none())`）；`rel="noopener noreferrer"` 由
    //    净化器的 `link_rel` 自动补齐。
    builder.add_tag_attributes("a", &["target"]);
    // ② 代码块的 `language-*` 类：ammonia 默认 `allowed_classes` 为空，不登记就会被剥掉。
    //    ⚠️ 不要另外把 `class` 写进 `tag_attributes`/`generic_attributes`：ammonia 4.2.0 会
    //    panic（`assert!(tag_attributes[tag]["class"].is_none())`，lib.rs:1898）——
    //    `allowed_classes` 本身就是「按标签登记 class」的专用通道。
    for class in code_classes(recipe) {
        // `once(class)` 而不是 `&[class]`：数组字面量的临时值活不过 `Builder<'a>` 的借用
        builder.add_allowed_classes("code", std::iter::once(class));
    }
    if recipe == Recipe::Help {
        // ③ 帮助文档要 TOC 跳转：`id` 属于 `generic_attributes`，不加白会被**静默剥离**（POC-13）。
        //    安全代价可接受：`id` 只可能来自本模块生成的 `h-{level}-{index}`（原始 HTML 已转义）。
        builder.add_generic_attributes(&["id"]);
    }
    builder.clean(raw_html).to_string()
}

/// 折叠空白：连续空白变一个空格，连续换行变一个换行，去掉行尾空格与首尾空白。
///
/// # 参数
///
/// - `raw`：原始纯文本（可能含大量空白）。
///
/// # 返回值
///
/// 归一化后的纯文本。
fn normalize_plain(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            pending_space = false;
        } else if ch.is_whitespace() {
            pending_space = !out.is_empty() && !out.ends_with('\n');
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// 单行标题文本：所有空白折叠成一个空格并去首尾（TOC 显示用）。
///
/// # 参数
///
/// - `raw`：标题内累积的文本。
///
/// # 返回值
///
/// 单行文本。
fn collapse_ws(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// HTML 转义（`& < > " '`），用于纯文本配方与兜底出参。
///
/// # 参数
///
/// - `text`：原始文本。
///
/// # 返回值
///
/// 转义后的文本。
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C1-01：`#` 必须降为 `<h2>`，且带稳定锚点 `id="h-2-1"`（帮助配方）。
    #[test]
    fn c1_01_heading_offset_plus_one_and_anchor_id() {
        let rendered = render_help("# 维护时间\n\n正文\n");
        assert!(
            rendered.content_html.contains("<h2 id=\"h-2-1\">维护时间</h2>"),
            "heading 偏移 +1 且生成 id 失败：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("<h1"),
            "正文里的 # 不得产出第二个 h1：{}",
            rendered.content_html
        );
        assert_eq!(rendered.headings.len(), 1, "应记账 1 个标题");
        assert_eq!(rendered.headings[0].level, 2);
        println!("[C1-01] # → <h2 id=\"h-2-1\">维护时间</h2> ✓");
    }

    /// C1-02：三个同名 `## 概述` → `id` 序号分别为 `1/2/3`（锚点不得重复，H-12）。
    ///
    /// 注意层级口径：正文 `##` 偏移后是 `<h3>`，所以 id 前缀是 `h-3-`（与 POC-13 实测的
    /// `<h3 id="h-3-1">影响范围</h3>` 一致；`#` 才是 `h-2-`）。
    #[test]
    fn c1_02_heading_ids_unique_when_titles_repeat() {
        let rendered = render_help("## 概述\n\nA\n\n## 概述\n\nB\n\n## 概述\n\nC\n");
        let ids: Vec<&str> = rendered.headings.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["h-3-1", "h-3-2", "h-3-3"], "同层序号必须递增且唯一");
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "锚点 id 不得重复");
        println!("[C1-02] 三个同名 `##` → {ids:?} ✓");
    }

    /// C1-03：公告配方**剥离** heading 的 `id`（公告没有目录，POC-13「公告侧配方无 id」）。
    #[test]
    fn c1_03_announcement_recipe_strips_heading_id() {
        let rendered = render_announcement("# 值班安排\n\n内容\n");
        assert!(
            rendered.content_html.contains("<h2>值班安排</h2>"),
            "公告侧 heading 仍需偏移 +1：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("id="),
            "公告侧不得保留 id（ammonia 默认剥离）：{}",
            rendered.content_html
        );
        // 标题数据（headings）仍然带着 id，供内部埋点 / 未来复用
        assert_eq!(rendered.headings[0].id, "h-2-1");
        println!("[C1-03] 公告侧 {} ✓", rendered.content_html.trim());
    }

    /// C1-04：`<div onclick=...>` 必须输出为 `&lt;div`（A-14：`ENABLE_HTML=false` 的直接断言）。
    #[test]
    fn c1_04_raw_html_div_is_escaped() {
        // `ENABLE_HTML` 的「恒为 false」由模块顶部的编译期断言兜底（const _: () = assert!(...)），
        // 这里只断言**行为**：原始 HTML 必须变成转义文本
        let markdown = "<div onclick=\"alert(1)\">点我</div>\n";
        let rendered = render_announcement(markdown);
        assert!(
            rendered.content_html.contains("&lt;div"),
            "<div 必须被转义成 &lt;div：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("<div"),
            "不得出现可执行的 <div 标签：{}",
            rendered.content_html
        );
        assert!(
            rendered.escaped_html >= 1,
            "转义计数应被记账（escaped_html={}）",
            rendered.escaped_html
        );
        println!("[C1-04] {} → {} ✓", markdown.trim(), rendered.content_html.trim());
    }

    /// C1-05：`<script>alert(1)</script>` 不得产出可执行 script 标签（A-13）。
    #[test]
    fn c1_05_script_tag_never_reaches_output() {
        let rendered = render_help("<script>alert(1)</script>\n\n正常正文\n");
        assert!(
            !rendered.content_html.to_ascii_lowercase().contains("<script"),
            "输出中不得出现 <script 标签：{}",
            rendered.content_html
        );
        assert!(
            rendered.content_html.contains("&lt;script"),
            "script 应以转义文本形式出现：{}",
            rendered.content_html
        );
        assert!(
            !rendered.plain_text.to_ascii_lowercase().contains("<script"),
            "纯文本里也不得残留 script 标签：{}",
            rendered.plain_text
        );
        println!("[C1-05] script → {} ✓", rendered.content_html.trim());
    }

    /// C1-06：`![](http://evil/x.png)` → `[图片已移除]`（A-15：外链图片一律移除）。
    #[test]
    fn c1_06_external_image_replaced_with_placeholder() {
        let rendered = render_announcement("看图：![](http://evil/x.png)\n");
        assert!(
            rendered.content_html.contains(IMAGE_REMOVED),
            "外链图片必须替换为占位文本：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("<img"),
            "外链图片不得留下 <img>：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("evil"),
            "外链 URL 不得出现在输出里：{}",
            rendered.content_html
        );
        assert_eq!(rendered.removed_images, 1, "移除计数应为 1");
        println!("[C1-06] 外链图片 → {} ✓", rendered.content_html.trim());
    }

    /// C1-07：站内 storage 图片必须**保留**（否则公告里的截图全被误杀）。
    #[test]
    fn c1_07_storage_image_is_kept() {
        let rendered = render_announcement("![拓扑](/api/business/storage/file/abc.png)\n");
        assert!(
            rendered
                .content_html
                .contains("<img src=\"/api/business/storage/file/abc.png\""),
            "站内 storage 图片应保留 src：{}",
            rendered.content_html
        );
        assert_eq!(rendered.removed_images, 0, "站内图片不应计入移除");
        println!("[C1-07] 站内图片 → {} ✓", rendered.content_html.trim());
    }

    /// C1-08：`javascript:` 链接必须被移除（去掉 `<a>`，只留文字，A-13）。
    #[test]
    fn c1_08_javascript_link_is_removed() {
        let rendered = render_announcement("[点我](javascript:alert(1))\n");
        assert!(
            !rendered.content_html.to_ascii_lowercase().contains("javascript:"),
            "输出不得残留 javascript: 协议：{}",
            rendered.content_html
        );
        assert!(
            !rendered.content_html.contains("<a"),
            "危险链接不得留下 <a> 标签：{}",
            rendered.content_html
        );
        assert!(
            rendered.content_html.contains("点我"),
            "链接文字要保留（降级为纯文本）：{}",
            rendered.content_html
        );
        assert_eq!(rendered.removed_links, 1, "降级计数应为 1");
        println!("[C1-08] javascript: 链接 → {} ✓", rendered.content_html.trim());
    }

    /// C1-09：`file:///etc/passwd` 等非白名单协议整段降级为纯文本（A-16）。
    #[test]
    fn c1_09_non_whitelisted_scheme_downgraded() {
        let rendered = render_help("[读文件](file:///etc/passwd) 与 [外链](https://example.com/a)\n");
        assert!(
            !rendered.content_html.contains("file:"),
            "file: 协议必须被移除：{}",
            rendered.content_html
        );
        assert!(
            rendered.content_html.contains("读文件"),
            "降级后文字要保留：{}",
            rendered.content_html
        );
        assert!(
            rendered.content_html.contains("<a href=\"https://example.com/a\""),
            "https 链接应保留：{}",
            rendered.content_html
        );
        assert_eq!(rendered.removed_links, 1);
        println!("[C1-09] file: 降级、https 保留 → {} ✓", rendered.content_html.trim());
    }

    /// C1-10：TOC 层级正确（h2 / h3 / h4 嵌套，H-11）。
    #[test]
    fn c1_10_toc_tree_nests_h2_h3_h4() {
        let markdown = "# 一级\n\n## 二级 A\n\n### 三级\n\n#### 四级\n\n## 二级 B\n";
        let rendered = render_help(markdown);
        // 偏移 +1 之后：#→h2、##→h3、###→h4、####→h5
        assert_eq!(rendered.toc.len(), 1, "顶层只有 1 个节点（一级）");
        let top = &rendered.toc[0];
        assert_eq!((top.level, top.text.as_str(), top.id.as_str()), (2, "一级", "h-2-1"));
        assert_eq!(top.children.len(), 2, "一级下有 2 个二级节点");
        assert_eq!(top.children[0].text, "二级 A");
        assert_eq!(top.children[0].level, 3);
        assert_eq!(top.children[0].children[0].text, "三级");
        assert_eq!(top.children[0].children[0].level, 4);
        assert_eq!(
            top.children[0].children[0].children[0].text, "四级",
            "h4 应嵌在 h3 之下"
        );
        assert_eq!(top.children[0].children[0].children[0].level, 5);
        assert_eq!(top.children[1].text, "二级 B");
        assert!(top.children[1].children.is_empty(), "二级 B 没有子节点");
        println!(
            "[C1-10] TOC 顶层 {} 项，二级 {} 项，三级/四级逐层嵌套 ✓",
            rendered.toc.len(),
            top.children.len()
        );
    }

    /// C1-11：纯文本配方全部剥标签（检索 snippet / 摘要用，H-10）。
    #[test]
    fn c1_11_plain_text_recipe_strips_every_tag() {
        let markdown =
            "# 标题\n\n**加粗** 与 `代码` 与 [链接](https://example.com)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let rendered = render(markdown, Recipe::PlainText);
        assert!(
            !rendered.content_html.contains('<'),
            "纯文本配方不得产出任何标签：{}",
            rendered.content_html
        );
        assert!(
            rendered.plain_text.contains("加粗")
                && rendered.plain_text.contains("代码")
                && rendered.plain_text.contains("链接"),
            "文字内容必须保留：{}",
            rendered.plain_text
        );
        assert!(
            !rendered.plain_text.contains('<') && !rendered.plain_text.contains('>'),
            "纯文本里不得残留尖括号：{}",
            rendered.plain_text
        );
        assert!(rendered.toc.is_empty(), "纯文本配方不产生 TOC");
        println!("[C1-11] 纯文本 = {:?} ✓", rendered.plain_text);
    }

    /// C1-12：代码语言白名单 —— 帮助更宽（`rust` 保留类名），公告受限（同类被剥）。
    #[test]
    fn c1_12_help_accepts_wider_code_languages() {
        let markdown = "```rust\nfn main() {}\n```\n\n```sql\nSELECT 1;\n```\n";
        let help = render_help(markdown);
        assert!(
            help.content_html.contains("class=\"language-rust\""),
            "帮助配方应保留 language-rust 类：{}",
            help.content_html
        );
        assert!(
            help.content_html.contains("class=\"language-sql\""),
            "帮助配方应保留 language-sql 类：{}",
            help.content_html
        );

        let announcement = render_announcement(markdown);
        assert!(
            !announcement.content_html.contains("language-rust"),
            "公告配方不得保留 language-rust（白名单外）：{}",
            announcement.content_html
        );
        assert!(
            announcement.content_html.contains("class=\"language-sql\""),
            "公告配方应保留运维类 language-sql：{}",
            announcement.content_html
        );
        assert!(
            announcement.content_html.contains("fn main() {}"),
            "代码文字本身必须保留：{}",
            announcement.content_html
        );
        println!("[C1-12] help 保留 language-rust / sql；announcement 只保留 language-sql ✓");
    }
}
