//! 单据 HTML 模板（tera）+ 金额格式化 / 人民币大写。
//!
//! # 为什么模板用 `include_str!` 内嵌，而不是运行时读磁盘
//!
//! 部署形态是「一个二进制 + 一个 dist 目录」，工作目录不可控（systemd 的
//! `WorkingDirectory`、容器挂载点、`cargo run` 与 `systemctl start` 都不一样）。
//! 生产上出现「本地能出 PDF、上线后 500 模板不存在」几乎全是相对路径踩的坑，
//! 所以模板在编译期就进了二进制（[`STATEMENT_TEMPLATE`]），没有任何运行期文件依赖。
//!
//! # 金额为什么在这里算
//!
//! PDF 里的金额有四种写法，且必须**互相自洽**：
//!
//! | 写法 | 例子 | 来源 |
//! |------|------|------|
//! | 千分位小数 | `1,234,567.89` | [`format_money`] |
//! | 数量千分位整数 | `12,345` | [`format_int`] |
//! | 人民币大写 | `壹佰贰拾叁万肆仟伍佰陆拾柒元捌角玖分` | [`amount_upper`] |
//! | 等宽数字（模板 CSS） | `font-variant-numeric: tabular-nums` | 模板 |
//!
//! 前端有同款实现（`frontend/src/utils/money.ts`），两边口径必须一致：**以「分」
//! 为整数做运算**，绝不出现 f64（0.1 + 0.2 的经典问题会在合计行上被放大成 1 分钱差错，
//! 对账场景不可接受）。

use std::sync::OnceLock;

use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

use crate::business::pdf::dto::PdfError;

/// 内嵌的对账单模板（打印样式与分页约束都在里面，见文件头注释）。
pub const STATEMENT_TEMPLATE: &str = include_str!("templates/statement.html.tera");

/// 内嵌模板在 tera 里的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const STATEMENT_NAME: &str = "statement.html";

/// 内嵌的 ERP 单据模板（采购订单 / 销售订单共用，PDF-02）。
pub const ORDER_TEMPLATE: &str = include_str!("templates/order.html.tera");

/// 单据模板的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const ORDER_NAME: &str = "order.html";

/// 单据页眉里的公司名（与对账单同一家，改名前请同步 `docs/`）。
pub const COMPANY_NAME: &str = "云枢（上海）科技有限公司";

/// 进程级 tera 实例（模板编译一次）。
fn tera() -> &'static Tera {
    static TERA: OnceLock<Tera> = OnceLock::new();
    TERA.get_or_init(|| {
        let mut tera = Tera::default();
        // 注册失败只可能是模板语法错——那是编译期就能发现的常量字符串，
        // 这里 expect 是合理的（跑不起来比出一份错 PDF 好）。
        tera.add_raw_template(STATEMENT_NAME, STATEMENT_TEMPLATE)
            .expect("内置单据模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(ORDER_NAME, ORDER_TEMPLATE)
            .expect("内置订单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera
    })
}

/// 对账单明细行。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatementItem {
    /// 序号（从 1 开始）
    pub seq: u32,
    /// 品名
    pub name: String,
    /// 规格
    pub spec: String,
    /// 单位
    pub unit: String,
    /// 数量（千分位整数，已格式化）
    pub qty: String,
    /// 单价（两位小数，已格式化）
    pub price: String,
    /// 金额（两位小数，已格式化）
    pub amount: String,
}

/// 对账单渲染上下文（模板变量的一一对应结构体）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatementContext {
    /// 公司名
    pub company: String,
    /// 单据标题
    pub doc_title: String,
    /// 单号
    pub doc_no: String,
    /// 客户
    pub customer: String,
    /// 期间
    pub period: String,
    /// 打印时间
    pub printed_at: String,
    /// 明细行
    pub items: Vec<StatementItem>,
    /// 行数合计
    pub row_count: String,
    /// 数量合计
    pub total_qty: String,
    /// 金额合计（千分位小数）
    pub total_amount: String,
    /// 金额大写
    pub total_amount_cn: String,
    /// 制单人
    pub sign_maker: String,
    /// 审核人
    pub sign_checker: String,
    /// 备注
    pub remark: String,
    /// 引擎标识（页脚）
    pub engine: String,
}

impl StatementContext {
    /// 用明细行与合计金额组装上下文。
    ///
    /// # 参数
    ///
    /// - `items`：明细行（已格式化）；
    /// - `total_qty`：数量合计（原始整数，用于千分位格式化）；
    /// - `total_amount_fen`：金额合计（**分**，同时用于小数与大写两种写法）。
    pub fn new(items: Vec<StatementItem>, total_qty: i64, total_amount_fen: i64) -> Self {
        Self {
            company: "云枢（上海）科技有限公司".to_string(),
            doc_title: "对 账 单".to_string(),
            doc_no: "AR-2026-11-0001".to_string(),
            customer: "北京某某机电设备有限公司".to_string(),
            period: "2026-10-01 至 2026-10-31".to_string(),
            printed_at: "2026-11-08 16:40".to_string(),
            row_count: format_int(items.len() as i64),
            items,
            total_qty: format_int(total_qty),
            total_amount: format_money(total_amount_fen),
            total_amount_cn: amount_upper(total_amount_fen),
            sign_maker: "李四".to_string(),
            sign_checker: "王五".to_string(),
            remark: "对账无误请于 7 个工作日内盖章回传；逾期视为确认。《合同》第 12 条、附件二……"
                .to_string(),
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染对账单 HTML。
///
/// # 参数
///
/// - `ctx`：上下文。
///
/// # 返回值
///
/// 完整 HTML 文档（可直接交给 [`crate::business::pdf::engine::render_pdf`]）。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败（缺变量 / 语法错）。
pub fn render_statement(ctx: &StatementContext) -> Result<String, PdfError> {
    let context = Context::from_serialize(ctx)
        .map_err(|error| PdfError::Template(format!("对账单上下文序列化失败：{error}")))?;
    tera()
        .render(STATEMENT_NAME, &context)
        .map_err(|error| PdfError::Template(format!("对账单模板渲染失败：{error}")))
}

/// 按名渲染任意已注册模板（供后续单据复用）。
///
/// # 参数
///
/// - `name`：模板注册名（目前只有 [`STATEMENT_NAME`]）；
/// - `context`：模板变量。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板不存在或渲染失败。
pub fn render(name: &str, context: &Context) -> Result<String, PdfError> {
    tera()
        .render(name, context)
        .map_err(|error| PdfError::Template(format!("模板 {name} 渲染失败：{error}")))
}

// ==================== ② ERP 单据模板（PDF-02：采购订单 / 销售订单） ====================

/// 单据种类（决定页眉标题、下载名与审计里的 `doc_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDocKind {
    /// 采购订单（`biz_purchase_order`）
    Purchase,
    /// 销售订单（`biz_sales_order`）
    Sales,
}

impl OrderDocKind {
    /// 页眉标题（字间留白是中文单据的排版惯例）。
    pub fn title(self) -> &'static str {
        match self {
            OrderDocKind::Purchase => "采 购 订 单",
            OrderDocKind::Sales => "销 售 订 单",
        }
    }

    /// 单据名（文件名 / 日志用，不带空格）。
    pub fn label(self) -> &'static str {
        match self {
            OrderDocKind::Purchase => "采购订单",
            OrderDocKind::Sales => "销售订单",
        }
    }

    /// `shared::doc_kind::DocKind` 的落库值（大写下划线，**只在这里派生**）。
    pub fn doc_kind_str(self) -> &'static str {
        match self {
            OrderDocKind::Purchase => "PURCHASE_ORDER",
            OrderDocKind::Sales => "SALE_ORDER",
        }
    }

    /// 单据表名（`operation_logs.target_type` 的取值，与 `DocKind::table()` 同口径）。
    pub fn table(self) -> &'static str {
        match self {
            OrderDocKind::Purchase => "biz_purchase_order",
            OrderDocKind::Sales => "biz_sales_order",
        }
    }
}

/// 单据头的一个字段（`单号` / `日期` / `状态` …）。标签写在左、值写在右。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderMetaField {
    /// 字段名（中文，如「单号」）
    pub label: String,
    /// 字段值（已格式化；`—` 表示该单据没有这个字段）
    pub value: String,
}

impl OrderMetaField {
    /// 便捷构造。
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// 明细行（金额/数量已是**展示字符串**，模板不做任何计算）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderItem {
    /// 序号（从 1 开始，跨页连续）
    pub seq: u32,
    /// 物料编码
    pub material_code: String,
    /// 物料名称
    pub material_name: String,
    /// 规格
    pub spec: String,
    /// 计量单位
    pub unit: String,
    /// 数量（去尾零 + 千分位）
    pub qty: String,
    /// 单价（最多 4 位小数，千分位）
    pub price: String,
    /// 金额（两位小数 + 千分位）
    pub amount: String,
    /// 备注
    pub remark: String,
}

/// 一张单据的渲染上下文（订单模板变量的一一对应结构体）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderContext {
    /// 公司名
    pub company: String,
    /// 页眉标题（「采 购 订 单」）
    pub doc_title: String,
    /// 单据名（文件名 / 日志用）
    pub doc_kind_label: String,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（已按每行 3 组切好）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 币种代码（`CNY` / `USD` …）
    pub currency: String,
    /// 币种符号（`￥` / `USD `）
    pub currency_symbol: String,
    /// 大写金额前缀（`人民币`；外币为空串 —— 别把美元写成人民币）
    pub currency_cn: String,
    /// 明细
    pub items: Vec<OrderItem>,
    /// 行数合计
    pub row_count: String,
    /// 数量合计
    pub total_qty: String,
    /// 不含税金额合计
    pub total_amount: String,
    /// 税额合计
    pub total_tax: String,
    /// 价税合计
    pub total_with_tax: String,
    /// 价税合计大写
    pub total_with_tax_cn: String,
    /// 备注
    pub remark: String,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识（页脚）
    pub engine: String,
}

/// [`OrderContext`] 的构造入参（避免 15 个位置参数的调用）。
#[derive(Debug, Clone)]
pub struct OrderContextArgs {
    /// 单据种类
    pub kind: OrderDocKind,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（按传入顺序每 3 组一行）
    pub meta: Vec<OrderMetaField>,
    /// 币种代码（`CNY` / `USD` …）
    pub currency_code: String,
    /// 明细
    pub items: Vec<OrderItem>,
    /// 数量合计（原始值，本结构负责格式化）
    pub total_qty: Decimal,
    /// 不含税金额合计（元）
    pub total_amount: Decimal,
    /// 税额合计（元）
    pub total_tax: Decimal,
    /// 价税合计（元）
    pub total_with_tax: Decimal,
    /// 备注
    pub remark: String,
    /// 打印时间（`yyyy-MM-dd HH:mm`，调用方按时区格式化）
    pub printed_at: String,
}

impl OrderContext {
    /// 组装上下文。金额统一走 [`yuan_to_fen`]（定点的「元」→ 整数「分」），
    /// 大写与小数两种写法出自同一个整数，保证逐位自洽。
    pub fn new(args: OrderContextArgs) -> Self {
        let with_tax_fen = yuan_to_fen(args.total_with_tax);
        let currency_code = args.currency_code.trim().to_ascii_uppercase();
        let is_cny = currency_code == "CNY" || currency_code.is_empty();
        let row_count = format_int(args.items.len() as i64);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: args.kind.title().to_string(),
            doc_kind_label: args.kind.label().to_string(),
            doc_no: args.doc_no,
            meta_rows: args
                .meta
                .chunks(3)
                .map(<[OrderMetaField]>::to_vec)
                .collect(),
            currency: if currency_code.is_empty() {
                "CNY".to_string()
            } else {
                currency_code.clone()
            },
            currency_symbol: if is_cny {
                "￥".to_string()
            } else {
                format!("{currency_code} ")
            },
            currency_cn: if is_cny {
                "人民币".to_string()
            } else {
                String::new()
            },
            items: args.items,
            row_count,
            total_qty: format_quantity(args.total_qty),
            total_amount: format_money(yuan_to_fen(args.total_amount)),
            total_tax: format_money(yuan_to_fen(args.total_tax)),
            total_with_tax: format_money(with_tax_fen),
            total_with_tax_cn: amount_upper(with_tax_fen),
            remark: args.remark,
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染单据 HTML（采购 / 销售订单共用同一份模板）。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败（缺变量 / 语法错）。
pub fn render_order(ctx: &OrderContext) -> Result<String, PdfError> {
    let context = Context::from_serialize(ctx)
        .map_err(|error| PdfError::Template(format!("单据上下文序列化失败：{error}")))?;
    tera()
        .render(ORDER_NAME, &context)
        .map_err(|error| PdfError::Template(format!("单据模板渲染失败：{error}")))
}

/// 单据 PDF 的页脚模板（CDP `footerTemplate`）：`第 X 页 / 共 Y 页`。
///
/// # 为什么页码不用 HTML 画
///
/// `@page` 的计数器在 Chromium 的 `printToPDF` 下不生效，HTML 里也拿不到总页数；
/// 只有 CDP 的 `footerTemplate` 认 `.pageNumber` / `.totalPages` 两个内建类，
/// 且 `displayHeaderFooter = true` 时才输出。字体必须显式写（Chromium 页脚的
/// 默认字号是 0，不写 `font-size` 会得到一条看不见的页脚）。
///
/// # 参数
///
/// - `kind`：单据种类（页脚左侧的单据名）；
/// - `doc_no`：单号（会被 HTML 转义）。
pub fn order_footer(kind: OrderDocKind, doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        kind.label(),
        escape_html(doc_no)
    )
}

/// 单据 PDF 的渲染参数（A4 纵向 + 页脚页码；下边距加大给页脚留出打印区）。
///
/// # 参数
///
/// - `kind`：单据种类（页脚文案用）；
/// - `doc_no`：单号（页脚左侧）；
/// - `filename`：下载文件名（`Content-Disposition`，中文走 RFC 5987）。
///
/// # 为什么 `prefer_css_page_size = false`
///
/// 纸张由这里的参数决定（A4），模板里的 `@page` 只作「浏览器直接打印」的兜底 ——
/// 否则模板 CSS 能悄悄改掉后端定好的纸张。
pub fn order_pdf_opts(
    kind: OrderDocKind,
    doc_no: &str,
    filename: Option<String>,
) -> crate::business::pdf::dto::PdfOpts {
    use crate::business::pdf::dto::{PageSize, PdfMargins, PdfOpts};
    PdfOpts {
        page: PageSize::A4,
        landscape: false,
        prefer_css_page_size: false,
        margins: PdfMargins {
            top_mm: 12.0,
            right_mm: 10.0,
            // 页脚（第 X 页 / 共 Y 页）画在下边距里，16mm 才不会被正文压住
            bottom_mm: 16.0,
            left_mm: 10.0,
        },
        display_header_footer: true,
        // 页眉留在 HTML 里（第 1 页的公司名 + 标题），CDP 页眉留空
        header_template: Some("<span></span>".to_string()),
        footer_template: Some(order_footer(kind, doc_no)),
        filename,
        ..PdfOpts::default()
    }
}

/// 极简 HTML 转义（用于**拼进 HTML 片段**的少量字段：页脚里的单号）。
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

/// 生成「样本单据」上下文（联调 / 验收用，数据确定性、不依赖数据库）。
///
/// # 参数
///
/// - `kind`：单据种类；
/// - `rows`：明细行数（调用方负责裁剪到合理范围）。
///
/// # 返回值
///
/// 每行数量 `(i % 9 + 1) * 5`、单价 `12.34 + (i % 37) * 0.11`，
/// 合计按行汇总（整数分累加，与真实单据同一口径）。
pub fn sample_order(kind: OrderDocKind, rows: u32) -> OrderContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_qty = Decimal::ZERO;
    let mut total_amount = Decimal::ZERO;
    let mut total_tax = Decimal::ZERO;
    for index in 0..rows {
        let qty = Decimal::from(((index % 9) + 1) as i64 * 5);
        // 单价：12.34 元起步，最大 16.30 元（两位小数，整数分的精确值）
        let price = Decimal::new(1234 + (index % 37) as i64 * 11, 2);
        let amount = (price * qty).round_dp(2);
        // 13% 税（逐行先舍入再汇总，与 purchase_model::line_amounts 同口径）
        let tax = (amount * Decimal::new(13, 2) / Decimal::from(100)).round_dp(2);
        total_qty += qty;
        total_amount += amount;
        total_tax += tax;
        items.push(OrderItem {
            seq: index + 1,
            material_code: format!("MAT-{:05}", index % 97 + 1),
            material_name: format!(
                "{}Φ{}mm 不锈钢法兰（含税）",
                names[(index % 4) as usize],
                index % 90 + 10
            ),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            qty: format_quantity(qty),
            price: format_quantity(price),
            amount: format_money(yuan_to_fen(amount)),
            remark: if index % 5 == 0 {
                "急件".to_string()
            } else {
                String::new()
            },
        });
    }
    OrderContext::new(OrderContextArgs {
        kind,
        doc_no: match kind {
            OrderDocKind::Purchase => "PO-20261108-000001".to_string(),
            OrderDocKind::Sales => "SO-20261108-000001".to_string(),
        },
        meta: vec![
            OrderMetaField::new("单号", "PO-20261108-000001"),
            OrderMetaField::new("日期", "2026-11-08"),
            OrderMetaField::new("状态", "已审批"),
            OrderMetaField::new(
                match kind {
                    OrderDocKind::Purchase => "供应商",
                    OrderDocKind::Sales => "客户",
                },
                "北京某某机电设备有限公司",
            ),
            OrderMetaField::new("仓库", "上海一号仓"),
            OrderMetaField::new("经办人", "李四"),
        ],
        currency_code: "CNY".to_string(),
        items,
        total_qty,
        total_amount,
        total_tax,
        total_with_tax: total_amount + total_tax,
        remark: "按合同约定分批发货，《采购合同》第 12 条。".to_string(),
        printed_at: "2026-11-08 18:30".to_string(),
    })
}

/// 生成「样本对账单」上下文（联调 / 验收用，数据确定性、不依赖数据库）。
///
/// # 参数
///
/// - `rows`：明细行数（调用方负责裁剪到 `1..=MAX_SAMPLE_ROWS`）。
///
/// # 返回值
///
/// 上下文；每行数量 `(i % 9 + 1) * 5`、单价 `12.34 + (i % 37) * 0.11`，
/// 合计金额按行汇总——**用整数分累加**，与真实单据同一口径。
pub fn sample_statement(rows: u32) -> StatementContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_qty: i64 = 0;
    let mut total_fen: i64 = 0;
    for index in 0..rows {
        let qty = ((index % 9) + 1) as i64 * 5;
        // 单价：1234 分 + (i % 37) * 11 分（即 12.34 元起步，最大 16.30 元）
        let price_fen = 1234 + (index % 37) as i64 * 11;
        let amount_fen = qty * price_fen;
        total_qty += qty;
        total_fen += amount_fen;
        items.push(StatementItem {
            seq: index + 1,
            name: format!(
                "{}Φ{}mm 不锈钢法兰（含税）",
                names[(index % 4) as usize],
                index % 90 + 10
            ),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            qty: format_int(qty),
            price: format_money(price_fen),
            amount: format_money(amount_fen),
        });
    }
    StatementContext::new(items, total_qty, total_fen)
}

/// 千分位整数（`1234567` → `1,234,567`，负数保留符号）。
pub fn format_int(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, ch) in digits.chars().enumerate() {
        // 每 3 位插一个千分位分隔符（clippy: 用 is_multiple_of 而不是 % == 0）
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// 「分」→ 千分位两位小数（`12345678` → `123,456.78`）。
///
/// 负数按「符号 + 绝对值」格式化：`-12345` 分必须是 `-123.45`，不能走
/// `div_euclid`（那会得到 `-124.55`，红字单据的金额就错了）。
pub fn format_money(fen: i64) -> String {
    let sign = if fen < 0 { "-" } else { "" };
    let abs = fen.unsigned_abs();
    format!("{sign}{}.{:02}", format_int((abs / 100) as i64), abs % 100)
}

/// 「元」→「分」（四舍五入到分，负数保留符号）。
///
/// # 为什么不用 `value * 100` 再 `to_i64`
///
/// `Decimal` 的默认舍入策略是**银行家舍入**（`.005` → `.00`），而单据金额是
/// 四舍五入（`.005` → `.01`）。差 1 分在合计行 / 大写金额上会被放大成审计问题，
/// 所以这里显式指定 [`RoundingStrategy::MidpointAwayFromZero`]。
///
/// # 边界
///
/// - 入参超过 `i64` 实用范围（> 9.2e16 元）时按 `0` 处理并**只可能发生在脏数据**，
///   业务上的金额上限（`NUMERIC(18,4)`）远小于它；
/// - 小数位多于 2 位时先就近到分。
pub fn yuan_to_fen(value: Decimal) -> i64 {
    let rounded = value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    let text = rounded.to_string();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, text.as_str()),
    };
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    let yuan: i64 = int_part.parse().unwrap_or(0);
    let mut fen = 0i64;
    let frac: Vec<char> = frac_part.chars().collect();
    if let Some(first) = frac.first() {
        fen += first.to_digit(10).unwrap_or(0) as i64 * 10;
    }
    if let Some(second) = frac.get(1) {
        fen += second.to_digit(10).unwrap_or(0) as i64;
    }
    sign * (yuan * 100 + fen)
}

/// 数量 / 单价 → 展示串：最多 4 位小数（去尾零）+ 千分位（`1234.5000` → `1,234.5`）。
///
/// # 为什么数量不用 [`format_int`]
///
/// 采购 / 销售明细的数量是 `NUMERIC(18,4)`（可以 1.5 米、0.25 千克）；
/// 单价是 `NUMERIC(18,6)`。用整数格式会静默截断小数 —— 一张 0.5 米的单子
/// 打成「0 米」是硬伤。这里按 4 位小数展示（单据打印惯例），
/// 尾零去掉，`10.0000` 显示为 `10`。
pub fn format_quantity(value: Decimal) -> String {
    let rounded = value.round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero);
    let text = rounded.normalize().to_string();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.as_str()),
    };
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    let int_value: i64 = int_part.parse().unwrap_or(0);
    let mut out = format!("{sign}{}", format_int(int_value));
    let frac = frac_part.trim_end_matches('0');
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out
}

const DIGITS: [char; 10] = ['零', '壹', '贰', '叁', '肆', '伍', '陆', '柒', '捌', '玖'];
const SMALL_UNITS: [&str; 4] = ["", "拾", "佰", "仟"];
const BIG_UNITS: [&str; 4] = ["", "万", "亿", "万亿"];

/// 「分」→ 人民币大写（`123456789` → `壹佰贰拾叁万肆仟伍佰陆拾柒元捌角玖分`）。
///
/// # 规则（与银行票据口径一致）
///
/// - 没有角分 → 末尾 `整`；有角无分 → `…X角整`；有分 → `…X角Y分`；
/// - 中间连续的零只写一个「零」，末尾的零不写；
/// - 金额为 0 → `零元整`；
/// - 负数 → 前置 `负`（退款 / 红字单据用）。
pub fn amount_upper(fen: i64) -> String {
    if fen == 0 {
        return "零元整".to_string();
    }
    let negative = fen < 0;
    let total = fen.unsigned_abs();
    let yuan = total / 100;
    let jiao = (total / 10) % 10;
    let cent = total % 10;

    let mut out = String::new();
    if negative {
        out.push('负');
    }
    out.push_str(&yuan_upper(yuan));
    out.push('元');
    match (jiao, cent) {
        (0, 0) => out.push('整'),
        (0, c) => {
            if yuan > 0 {
                out.push('零');
            }
            out.push(DIGITS[c as usize]);
            out.push('分');
        }
        (j, 0) => {
            out.push(DIGITS[j as usize]);
            out.push('角');
            out.push('整');
        }
        (j, c) => {
            out.push(DIGITS[j as usize]);
            out.push('角');
            out.push(DIGITS[c as usize]);
            out.push('分');
        }
    }
    out
}

/// 元的部分转大写（不含「元」字）。
fn yuan_upper(mut yuan: u64) -> String {
    if yuan == 0 {
        return String::from(DIGITS[0]);
    }
    // 按 4 位一节从低位往高位切
    let mut sections = Vec::new();
    while yuan > 0 {
        sections.push((yuan % 10_000) as u32);
        yuan /= 10_000;
    }
    let mut out = String::new();
    let mut pending_zero = false;
    for (index, section) in sections.iter().enumerate().rev() {
        if *section == 0 {
            // 这一节全是 0：只有当后面还有非零节时才需要一个「零」
            if !out.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        if !out.is_empty() && (pending_zero || *section < 1000) {
            out.push(DIGITS[0]);
        }
        pending_zero = false;
        out.push_str(&section_upper(*section));
        out.push_str(BIG_UNITS[index]);
    }
    out
}

/// 4 位以内的节转大写（`1020` → `壹仟零贰拾`）。
fn section_upper(section: u32) -> String {
    let digits: Vec<char> = format!("{section:04}").chars().collect();
    let mut out = String::new();
    let mut pending_zero = false;
    for (index, ch) in digits.iter().enumerate() {
        let digit = ch.to_digit(10).unwrap_or(0) as usize;
        let position = 3 - index;
        if digit == 0 {
            if !out.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        if pending_zero {
            out.push(DIGITS[0]);
            pending_zero = false;
        }
        out.push(DIGITS[digit]);
        out.push_str(SMALL_UNITS[position]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 千分位格式化：整数、金额、负数、0。
    #[test]
    fn money_and_int_formatting() {
        assert_eq!(format_int(0), "0");
        assert_eq!(format_int(999), "999");
        assert_eq!(format_int(1000), "1,000");
        assert_eq!(format_int(1234567), "1,234,567");
        assert_eq!(format_int(-1234), "-1,234");

        assert_eq!(format_money(0), "0.00");
        assert_eq!(format_money(5), "0.05");
        assert_eq!(format_money(1234), "12.34");
        assert_eq!(format_money(123456789), "1,234,567.89");
        // 负数必须「符号 + 绝对值」：-12345 分 = -123.45（不是 -124.55）
        assert_eq!(format_money(-12345), "-123.45");
        assert_eq!(format_money(-5), "-0.05");
        assert_eq!(format_money(-100), "-1.00");
    }

    /// 人民币大写：验收点（卡面第 14 项）逐例钉死。
    #[test]
    fn amount_upper_matches_bank_style() {
        let cases = [
            (0, "零元整"),
            (1, "零元壹分"),
            (50, "零元伍角整"),
            (1234, "壹拾贰元叁角肆分"),
            (100000, "壹仟元整"),
            (100200, "壹仟零贰元整"),
            (123456789, "壹佰贰拾叁万肆仟伍佰陆拾柒元捌角玖分"),
            (100000000, "壹佰万元整"),
            (10000000, "壹拾万元整"),
            (10000000000, "壹亿元整"),
            (10, "零元壹角整"),
            (-1234, "负壹拾贰元叁角肆分"),
            (10020, "壹佰元贰角整"),
        ];
        for (fen, expected) in cases {
            assert_eq!(
                amount_upper(fen),
                expected,
                "金额 {fen} 分（{}）",
                format_money(fen)
            );
        }
    }

    /// 内嵌模板必须能被 tera 编译并通过（语法错在测试里就炸，而不是上线后）。
    #[test]
    fn embedded_template_compiles() {
        let names: Vec<&str> = tera().get_template_names().collect();
        assert!(names.contains(&STATEMENT_NAME));
    }

    /// 渲染样本单据：关键结构必须都在（表头重复靠 thead、合计、签字栏、等宽数字）。
    #[test]
    fn renders_sample_statement_html() {
        let ctx = sample_statement(3);
        let html = render_statement(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("<html lang=\"zh-CN\">"));
        assert!(
            html.contains("display: table-header-group"),
            "表头重复规则必须在"
        );
        assert!(
            html.contains("break-inside: avoid"),
            "合计/签字栏防断裂规则必须在"
        );
        assert!(html.contains("tabular-nums"), "等宽数字必须在");
        assert!(
            html.contains("Noto Sans SC"),
            "字体族必须是静态 glyf 的 Noto Sans SC"
        );
        assert!(html.contains("对 账 单"));
        assert!(html.contains("球阀Φ10mm"), "明细行内容必须渲染");
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            3,
            "3 行明细必须各出一个序号单元格"
        );
        assert!(html.contains("金额大写"));
        assert!(html.contains("人民币"), "大写金额必须带人民币前缀");
        assert!(
            html.contains("（￥"),
            "大写金额后面必须附小写数字（￥…），否则对账时无法逐位核对"
        );
        assert!(html.contains("客户确认（盖章）"), "签字栏必须在");
    }

    /// 行数不同时行数合计/金额合计必须跟着变（不是写死的样例值）。
    #[test]
    fn totals_follow_row_count() {
        let ctx = sample_statement(100);
        assert_eq!(ctx.items.len(), 100);
        assert_eq!(ctx.row_count, "100");
        // 数量合计：每 9 行一个 5..45 的循环，100 行 = 11 个完整循环 + 1 行
        let expected_qty: i64 = (0..100).map(|i| ((i % 9) + 1) as i64 * 5).sum();
        assert_eq!(ctx.total_qty, format_int(expected_qty));
        // 金额合计 = 各行金额之和（整数分累加，不许有浮点误差）
        let expected_fen: i64 = (0..100)
            .map(|i| {
                let qty = ((i % 9) + 1) as i64 * 5;
                let price = 1234 + (i % 37) as i64 * 11;
                qty * price
            })
            .sum();
        assert_eq!(ctx.total_amount, format_money(expected_fen));
        assert_eq!(ctx.total_amount_cn, amount_upper(expected_fen));
    }

    /// 明细里的用户数据必须被 HTML 转义（模板 `.html` 后缀 → tera 自动转义）。
    #[test]
    fn item_content_is_escaped() {
        let mut ctx = sample_statement(1);
        ctx.items[0].name = "<script>alert(1)</script>".to_string();
        let html = render_statement(&ctx).expect("渲染必须成功");
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "脚本标签必须被转义"
        );
        assert!(html.contains("&lt;script&gt;"), "转义后应为实体");
    }

    /// 0 行也不能 panic（合计行仍要出，金额大写为「零元整」）。
    #[test]
    fn zero_rows_is_renderable() {
        let ctx = sample_statement(0);
        let html = render_statement(&ctx).expect("0 行也必须能渲染");
        assert!(html.contains("零元整"));
        assert!(html.contains("行数合计"));
    }

    // ==================== 单据（采购 / 销售订单）模板 ====================

    /// 两份模板都必须注册成功（语法错在测试里就炸）。
    #[test]
    fn both_templates_register() {
        let names: Vec<&str> = tera().get_template_names().collect();
        assert!(names.contains(&STATEMENT_NAME));
        assert!(names.contains(&ORDER_NAME));
    }

    /// 采购订单 HTML：页眉 / 单据头 / 九列明细 / 合计四要素 / 大写 / 五个签字位 /
    /// 表头重复 / 等宽数字，一个都不能少（验收①的结构面）。
    #[test]
    fn renders_sample_purchase_order_html() {
        let ctx = sample_order(OrderDocKind::Purchase, 3);
        let html = render_order(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("<html lang=\"zh-CN\">"));
        assert!(html.contains("采 购 订 单"), "页眉标题必须渲染");
        assert!(html.contains(COMPANY_NAME), "页眉公司名必须在");
        assert!(
            html.contains("display: table-header-group"),
            "跨页表头重复规则必须在"
        );
        assert!(html.contains("break-inside: avoid"), "防断裂规则必须在");
        assert!(html.contains("tabular-nums"), "等宽数字必须在");
        assert!(html.contains("Noto Sans SC"), "中文字体族必须在");

        // 单据头 6 个字段（3 列 × 2 行）
        for label in ["单号", "日期", "状态", "供应商", "仓库", "经办人"] {
            assert!(html.contains(label), "单据头缺字段：{label}");
        }
        // 明细九列的表头
        for head in [
            "物料编码",
            "物料名称",
            "规格",
            "单位",
            "数量",
            "单价",
            "金额",
            "备注",
        ] {
            assert!(html.contains(head), "明细表缺列：{head}");
        }
        // 合计四要素 + 大写
        for total in ["数量合计", "金额合计", "税额", "价税合计"] {
            assert!(html.contains(total), "合计行缺：{total}");
        }
        assert!(html.contains("人民币"), "大写金额必须带人民币前缀");
        assert!(html.contains("（￥"), "大写金额后面必须附小写数字");
        // 五个签字位（空白手签）
        for sign in ["制单：", "审核：", "批准：", "仓管：", "客户签收："] {
            assert!(html.contains(sign), "签字栏缺：{sign}");
        }
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            3,
            "3 行明细必须各出一个序号单元格"
        );
        // 页码不在 HTML 里（由 CDP footerTemplate 出），HTML 里出现说明改坏了
        assert!(
            !html.contains("class=\"pageNumber\""),
            "页码必须交给 footerTemplate，HTML 里不许有占位符元素"
        );
    }

    /// 销售订单用同一份模板，只有标题不同（验收②的版式一致）。
    #[test]
    fn renders_sample_sales_order_html() {
        let html = render_order(&sample_order(OrderDocKind::Sales, 50)).expect("渲染必须成功");
        assert!(html.contains("销 售 订 单"));
        assert!(!html.contains("采 购 订 单"));
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            50,
            "50 行明细必须全部渲染（跨页由浏览器分页，不是截断）"
        );
    }

    /// 单据头按每行 3 组切分；字段数不是 3 的倍数时最后一行少一格（不补假数据）。
    #[test]
    fn meta_rows_are_chunked_by_three() {
        let six = sample_order(OrderDocKind::Purchase, 0);
        assert_eq!(six.meta_rows.len(), 2);
        assert!(six.meta_rows.iter().all(|row| row.len() == 3));

        let mut args = OrderContextArgs {
            kind: OrderDocKind::Purchase,
            doc_no: "PO-1".to_string(),
            meta: (0..5)
                .map(|index| OrderMetaField::new(format!("L{index}"), format!("V{index}")))
                .collect(),
            currency_code: "CNY".to_string(),
            items: Vec::new(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::ZERO,
            total_tax: Decimal::ZERO,
            total_with_tax: Decimal::ZERO,
            remark: String::new(),
            printed_at: "2026-11-08 18:30".to_string(),
        };
        let five = OrderContext::new(args.clone());
        assert_eq!(five.meta_rows.len(), 2);
        assert_eq!(five.meta_rows[0].len(), 3);
        assert_eq!(five.meta_rows[1].len(), 2);

        // 外币：不写「人民币」前缀，符号用币种代码
        args.currency_code = "USD".to_string();
        let usd = OrderContext::new(args);
        assert_eq!(usd.currency_cn, "");
        assert_eq!(usd.currency_symbol, "USD ");
        assert_eq!(usd.currency, "USD");
    }

    /// 金额大写 5 个边界（验收③）：整数 / 带角 / 带分 / 零 / 大额。
    ///
    /// 期望值由**独立实现**（Python 版人民币大写，同一份规则）算出后写死在这里，
    /// 不是「拿函数自己的输出当答案」。
    #[test]
    fn amount_upper_five_boundaries() {
        let cases = [
            // 整数：12,345.00
            (1234500i64, "壹万贰仟叁佰肆拾伍元整"),
            // 带角：12,345.50
            (1234550, "壹万贰仟叁佰肆拾伍元伍角整"),
            // 带分：12,345.57
            (1234557, "壹万贰仟叁佰肆拾伍元伍角柒分"),
            // 零：0.00
            (0, "零元整"),
            // 大额：123,456,789,012.34（跨亿位，中间有零节）
            (
                12345678901234,
                "壹仟贰佰叁拾肆亿伍仟陆佰柒拾捌万玖仟零壹拾贰元叁角肆分",
            ),
        ];
        for (fen, expected) in cases {
            assert_eq!(amount_upper(fen), expected, "金额 {fen} 分大写不对");
        }
        // 大写与小数两种写法必须同源（同一次 yuan_to_fen 的结果）
        let ctx = OrderContext::new(OrderContextArgs {
            kind: OrderDocKind::Purchase,
            doc_no: "PO-1".to_string(),
            meta: Vec::new(),
            currency_code: "CNY".to_string(),
            items: Vec::new(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::new(1000000, 2),
            total_tax: Decimal::new(130000, 2),
            total_with_tax: Decimal::new(1130000, 2),
            remark: String::new(),
            printed_at: "2026-11-08 18:30".to_string(),
        });
        assert_eq!(ctx.total_amount, "10,000.00");
        assert_eq!(ctx.total_tax, "1,300.00");
        assert_eq!(ctx.total_with_tax, "11,300.00");
        assert_eq!(ctx.total_with_tax_cn, "壹万壹仟叁佰元整");
    }

    /// 「元」→「分」：四舍五入（不是银行家舍入），负数保留符号。
    #[test]
    fn yuan_to_fen_rounds_half_up() {
        assert_eq!(yuan_to_fen(Decimal::ZERO), 0);
        assert_eq!(yuan_to_fen(Decimal::new(1234, 2)), 1234); // 12.34
        assert_eq!(yuan_to_fen(Decimal::new(5, 3)), 1); // 0.005 → 0.01（银行家舍入会得 0）
        assert_eq!(yuan_to_fen(Decimal::new(15, 3)), 2); // 0.015 → 0.02
        assert_eq!(yuan_to_fen(Decimal::new(-123455, 3)), -12346); // -123.455 → -123.46
        assert_eq!(yuan_to_fen(Decimal::new(12345678, 4)), 123457); // 1234.5678 → 1234.57
    }

    /// 数量/单价展示：去尾零 + 千分位，且**不截断小数**（0.5 米不能打成 0 米）。
    #[test]
    fn quantity_formatting_keeps_decimals() {
        assert_eq!(format_quantity(Decimal::new(120000, 4)), "12"); // 12.0000
        assert_eq!(format_quantity(Decimal::new(12345000, 4)), "1,234.5"); // 1234.5000
        assert_eq!(format_quantity(Decimal::new(2500, 4)), "0.25");
        assert_eq!(
            format_quantity(Decimal::new(12345670001, 4)),
            "1,234,567.0001"
        );
        assert_eq!(format_quantity(Decimal::new(-1500, 4)), "-0.15");
    }

    /// 页脚模板：必须有 Chromium 的两个内建类 + 显式字号（默认字号 0 = 看不见）。
    #[test]
    fn footer_template_has_page_numbers_and_font_size() {
        let footer = order_footer(OrderDocKind::Sales, "SO-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("第 ") && footer.contains(" 页 / 共 ") && footer.contains(" 页"));
        assert!(footer.contains("font-size:8pt"), "页脚必须显式写字号");
        assert!(footer.contains("Noto Sans SC"), "页脚必须指定中文字体");
        assert!(footer.contains("销售订单 SO-20261108-000001"));

        // 单号里的尖括号不能破坏页脚 HTML
        let escaped = order_footer(OrderDocKind::Purchase, "<b>PO</b>");
        assert!(!escaped.contains("<b>PO</b>"));
        assert!(escaped.contains("&lt;b&gt;PO&lt;/b&gt;"));
    }

    /// 单据渲染参数：A4 纵向、页脚开、下边距加高、封面页眉交给 HTML。
    #[test]
    fn order_pdf_opts_are_a4_with_footer() {
        let opts = order_pdf_opts(
            OrderDocKind::Purchase,
            "PO-20261108-000001",
            Some("采购订单-PO-20261108-000001.pdf".to_string()),
        );
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape);
        assert!(opts.display_header_footer);
        assert!(opts.footer_template.is_some());
        assert!(opts.margins.bottom_mm >= 14.0, "页脚要有打印区");
        assert!(!opts.prefer_css_page_size, "纸张由后端参数决定");
        assert_eq!(
            opts.filename.as_deref(),
            Some("采购订单-PO-20261108-000001.pdf")
        );
    }

    /// 明细里的用户数据必须被转义（物料名里出现 `<script>` 不能真的执行）。
    #[test]
    fn order_item_content_is_escaped() {
        let mut ctx = sample_order(OrderDocKind::Purchase, 1);
        ctx.items[0].material_name = "<script>alert(1)</script>".to_string();
        ctx.items[0].remark = "\"quoted\"".to_string();
        let html = render_order(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
