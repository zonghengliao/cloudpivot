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
        tera.add_raw_template(CONTRACT_NAME, CONTRACT_TEMPLATE)
            .expect("内置合同模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(STOCK_NAME, STOCK_TEMPLATE)
            .expect("内置出入库单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(TRANSFER_NAME, TRANSFER_TEMPLATE)
            .expect("内置调拨单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(STOCK_TAKE_NAME, STOCK_TAKE_TEMPLATE)
            .expect("内置盘点单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(RETURN_NAME, RETURN_TEMPLATE)
            .expect("内置退货单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(APPROVAL_NAME, APPROVAL_TEMPLATE)
            .expect("内置审批单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(TASK_NAME, TASK_TEMPLATE)
            .expect("内置派工单模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(REPORT_BALANCE_NAME, REPORT_BALANCE_TEMPLATE)
            .expect("内置资产负债表模板语法错误（编译期常量，改模板后必须跑 cargo test）");
        tera.add_raw_template(REPORT_AMOUNT_NAME, REPORT_AMOUNT_TEMPLATE)
            .expect("内置利润表/现金流量表模板语法错误（编译期常量，改模板后必须跑 cargo test）");
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
            remark: "对账无误请于 7 个工作日内盖章回传；逾期视为确认。《合同》第 12 条、附件二……".to_string(),
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
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("对账单上下文序列化失败：{error}")))?;
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
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
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
            currency_cn: if is_cny { "人民币".to_string() } else { String::new() },
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
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("单据上下文序列化失败：{error}")))?;
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

// ==================== ③ 合同模板（PDF-03：合同） ====================

/// 内嵌的合同模板（条款正文 + 签章位 + 附件清单）。
pub const CONTRACT_TEMPLATE: &str = include_str!("templates/contract.html.tera");

/// 合同模板的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const CONTRACT_NAME: &str = "contract.html";

/// 内嵌的出入库单模板（收货入库单 / 出库单共用）。
pub const STOCK_TEMPLATE: &str = include_str!("templates/stock.html.tera");

/// 出入库单模板的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const STOCK_NAME: &str = "stock.html";

/// 内嵌的调拨单模板（PDF-04）。
pub const TRANSFER_TEMPLATE: &str = include_str!("templates/transfer.html.tera");

/// 调拨单模板的注册名。
pub const TRANSFER_NAME: &str = "transfer.html";

/// 内嵌的盘点单模板（PDF-04：**唯一**带空白手写栏的单据）。
pub const STOCK_TAKE_TEMPLATE: &str = include_str!("templates/stock_take.html.tera");

/// 盘点单模板的注册名。
pub const STOCK_TAKE_NAME: &str = "stock_take.html";

/// 内嵌的退货单模板（采购退货单 / 销售退货单共用）。
pub const RETURN_TEMPLATE: &str = include_str!("templates/return_doc.html.tera");

/// 退货单模板的注册名。
pub const RETURN_NAME: &str = "return_doc.html";

/// 内嵌的审批单模板（PDF-05：审批轨迹时间轴 + 每级意见 + 各级签字位）。
pub const APPROVAL_TEMPLATE: &str = include_str!("templates/approval.html.tera");

/// 审批单模板的注册名。
pub const APPROVAL_NAME: &str = "approval.html";

/// 内嵌的工作任务派工单模板（PDF-06：任务信息 + 描述全文 + 检查项 + 签字栏）。
pub const TASK_TEMPLATE: &str = include_str!("templates/task.html.tera");

/// 派工单模板的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const TASK_NAME: &str = "task.html";

/// 内嵌的资产负债表模板（PDF-07：左右两栏 + 期末 / 期初两列）。
pub const REPORT_BALANCE_TEMPLATE: &str = include_str!("templates/report_balance.html.tera");

/// 资产负债表模板的注册名。
pub const REPORT_BALANCE_NAME: &str = "report_balance.html";

/// 内嵌的利润表 / 现金流量表模板（PDF-07：项目 / 本期金额 / 本年累计）。
pub const REPORT_AMOUNT_TEMPLATE: &str = include_str!("templates/report_amount.html.tera");

/// 利润表 / 现金流量表模板的注册名。
pub const REPORT_AMOUNT_NAME: &str = "report_amount.html";

/// 盘点表「空白栏」的填表说明（**与模板里的实际列一一对应**）。
///
/// # 为什么要把这句话当常量
///
/// 卡面把「实盘数量／差异／备注 三栏留空供现场手写」定为验收①。
/// 文案与列名分散在模板与单测里，改名时必然漏一处；抽成常量后，
/// 模板渲染与单测断言引用同一份字符串。
pub const STOCK_TAKE_BLANK_HINT: &str =
    "「实盘数量」「差异」「备注」三栏留空，由盘点人在现场手写（本表为现场盘点用表，不预印系统值）。";

/// 合同没有单独的「付款方式」列时打在 PDF 上的兜底文案。
///
/// # 为什么是一句「以合同条款约定为准」而不是编一个值
///
/// `biz_contract`（V0195 + V0209）**没有**付款方式 / 账期列，本系统也没有维护
/// 合同级的付款计划。打印时不能凭空写「月结 30 天」——
/// 那会变成一份与真实合同不符的凭证。所以：备注里能识别出
/// 「付款方式：…」就印那个值（见 [`labeled_value`]），识别不出就如实说明。
pub const PAYMENT_TERMS_EMPTY: &str = "以合同条款约定为准（本系统未单独登记付款方式）";

/// 合同没有正文（备注）时的提示：条款以纸质件 / 扫描件为准。
pub const CONTRACT_BODY_EMPTY_HINT: &str =
    "本合同未在系统中录入条款正文；条款以双方签章的纸质合同为准（扫描件见下方附件清单）。";

/// 合同没有关联附件时的提示。
pub const CONTRACT_ATTACHMENT_EMPTY_HINT: &str = "（暂无关联附件；如需归档请在合同详情页上传扫描件或补充协议）";

/// 签署页的骑缝章提示（**措辞本身是条件式的**：多页才需要骑缝章）。
///
/// # 为什么不按真实页数动态显示
///
/// 页数只有 Chromium 打完才知道（`footerTemplate` 里的 `totalPages` 拿不回来），
/// 在 HTML 里判断「是否多页」会变成「渲染 → 数页 → 再渲染」的两倍开销。
/// 单页合同上出现一句「多页请加盖骑缝章」是行业通行写法（它是一条**条件指令**，
/// 不是对页数的断言），因此固定在签署页印出。
pub const CONTRACT_SEAL_NOTE: &str = "多页合同请在第 1 页至末页右侧骑缝处逐页加盖骑缝章\
（章面须压住接缝、相邻页章体相连）；单页合同在签署页盖章即可。";

/// 合同附件清单的一行。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractAttachment {
    /// 序号（从 1 开始，按手动排序）
    pub seq: u32,
    /// 文件名（`sys_file.original_name` 优先）
    pub file_name: String,
    /// 文件角色标签（合同扫描件 / 补充协议 / 验收单 / 其他）
    pub role_label: String,
    /// 大小（人类可读：`12.3 KB`）
    pub size: String,
    /// 上传人
    pub uploader: String,
    /// 上传时间（`yyyy-MM-dd HH:mm`，北京时间）
    pub uploaded_at: String,
}

/// 合同的渲染上下文（合同模板变量的一一对应结构体）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractContext {
    /// 公司名
    pub company: String,
    /// 页眉标题（「合 同」）
    pub doc_title: String,
    /// 单据名（文件名 / 日志用）
    pub doc_label: String,
    /// 合同号
    pub doc_no: String,
    /// 合同名称
    pub contract_name: String,
    /// 概要字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 甲方信息
    pub party_a_rows: Vec<OrderMetaField>,
    /// 乙方信息
    pub party_b_rows: Vec<OrderMetaField>,
    /// 甲方名称（签章位抬头）
    pub party_a_name: String,
    /// 乙方名称（签章位抬头）
    pub party_b_name: String,
    /// 币种代码
    pub currency: String,
    /// 币种符号
    pub currency_symbol: String,
    /// 大写金额前缀（人民币 / 空串）
    pub currency_cn: String,
    /// 合同金额（小写，两位小数 + 千分位）
    pub amount: String,
    /// 合同金额（大写）
    pub amount_cn: String,
    /// 合同期限（`2026-01-01 至 2026-12-31`）
    pub term_range: String,
    /// 付款方式（备注里识别到的值，或 [`PAYMENT_TERMS_EMPTY`]）
    pub payment_terms: String,
    /// 条款正文（**全文**，不做任何截断）
    pub body: String,
    /// 是否有正文（决定印正文还是提示语）
    pub has_body: bool,
    /// 无正文时的提示语
    pub body_empty_hint: String,
    /// 附件清单
    pub attachments: Vec<ContractAttachment>,
    /// 附件份数（展示串）
    pub attachment_count: String,
    /// 无附件时的提示语
    pub attachment_empty_hint: String,
    /// 骑缝章提示
    pub seal_note: String,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识（页脚）
    pub engine: String,
}

/// [`ContractContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct ContractContextArgs {
    /// 合同号
    pub doc_no: String,
    /// 合同名称
    pub contract_name: String,
    /// 概要字段（按传入顺序每 3 组一行）
    pub meta: Vec<OrderMetaField>,
    /// 甲方字段
    pub party_a: Vec<OrderMetaField>,
    /// 乙方字段
    pub party_b: Vec<OrderMetaField>,
    /// 甲方名称
    pub party_a_name: String,
    /// 乙方名称
    pub party_b_name: String,
    /// 币种代码
    pub currency_code: String,
    /// 合同金额（元）
    pub amount: Decimal,
    /// 合同期限（见 [`term_range_of`]）
    pub term_range: String,
    /// 付款方式（见 [`payment_terms_of`]）
    pub payment_terms: String,
    /// 条款正文
    pub body: String,
    /// 附件清单
    pub attachments: Vec<ContractAttachment>,
    /// 打印时间
    pub printed_at: String,
}

impl ContractContext {
    /// 组装上下文。金额的「小写 / 大写」出自同一个整数「分」，逐位自洽。
    pub fn new(args: ContractContextArgs) -> Self {
        let amount_fen = yuan_to_fen(args.amount);
        let currency_code = args.currency_code.trim().to_ascii_uppercase();
        let is_cny = currency_code == "CNY" || currency_code.is_empty();
        let body = args.body.trim_end().to_string();
        let attachment_count = format_int(args.attachments.len() as i64);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: "合 同".to_string(),
            doc_label: "合同".to_string(),
            doc_no: args.doc_no,
            contract_name: args.contract_name,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            party_a_rows: args.party_a,
            party_b_rows: args.party_b,
            party_a_name: args.party_a_name,
            party_b_name: args.party_b_name,
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
            currency_cn: if is_cny { "人民币".to_string() } else { String::new() },
            amount: format_money(amount_fen),
            amount_cn: amount_upper(amount_fen),
            term_range: args.term_range,
            payment_terms: args.payment_terms,
            has_body: !body.is_empty(),
            body,
            body_empty_hint: CONTRACT_BODY_EMPTY_HINT.to_string(),
            attachments: args.attachments,
            attachment_count,
            attachment_empty_hint: CONTRACT_ATTACHMENT_EMPTY_HINT.to_string(),
            seal_note: CONTRACT_SEAL_NOTE.to_string(),
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染合同 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败（缺变量 / 语法错）。
pub fn render_contract(ctx: &ContractContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("合同上下文序列化失败：{error}")))?;
    tera()
        .render(CONTRACT_NAME, &context)
        .map_err(|error| PdfError::Template(format!("合同模板渲染失败：{error}")))
}

/// 合同期限展示串。
///
/// # 参数
///
/// - `start`：生效日（`yyyy-MM-dd`，无则 `None`）；
/// - `end`：到期日。
///
/// # 返回值
///
/// 两段都齐 → `2026-01-01 至 2026-12-31`；只有一端 → `2026-01-01 起` / `至 2026-12-31`；
/// 都没有 → `—`（**不编日期**：草稿合同本来就可能没有期限）。
pub fn term_range_of(start: Option<&str>, end: Option<&str>) -> String {
    let start = start.map(str::trim).filter(|text| !text.is_empty());
    let end = end.map(str::trim).filter(|text| !text.is_empty());
    match (start, end) {
        (Some(from), Some(to)) => format!("{from} 至 {to}"),
        (Some(from), None) => format!("{from} 起"),
        (None, Some(to)) => format!("至 {to}"),
        (None, None) => "—".to_string(),
    }
}

/// 从一段自由文本里识别「标签：值」形态的字段（付款方式的唯一取值路径）。
///
/// # 参数
///
/// - `text`：合同备注（条款正文）；
/// - `labels`：可接受的标签（如 `["付款方式", "付款条件", "结算方式"]`，长标签优先）。
///
/// # 返回值
///
/// 命中时返回值部分（同一行内、去首尾空白）；未命中 `None`。
///
/// # 为什么按行扫而不是正则全串扫
///
/// 条款正文是人工录入的多行文本，`付款方式：` 可能出现在行首，也可能出现在
/// 「1. 付款方式：」这种带序号的句子中间；按行扫 + 行内首次命中已经能覆盖
/// 两种写法，且不会把下一行的内容吞进来。
pub fn labeled_value(text: &str, labels: &[&str]) -> Option<String> {
    for line in text.lines() {
        for label in labels {
            let Some(position) = line.find(label) else {
                continue;
            };
            let rest = &line[position + label.len()..];
            let value = rest
                .trim_start_matches([':', '：', ' ', '\u{3000}', '-', '—', '－', '='])
                .trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 合同备注 → 付款方式（识别不到就用 [`PAYMENT_TERMS_EMPTY`]）。
pub fn payment_terms_of(remark: Option<&str>) -> String {
    const LABELS: [&str; 5] = ["付款方式", "付款条件", "结算方式", "支付方式", "付款安排"];
    remark
        .and_then(|text| labeled_value(text, &LABELS))
        .unwrap_or_else(|| PAYMENT_TERMS_EMPTY.to_string())
}

/// 字节数 → 人类可读（`512 B` / `12.3 KB` / `4.7 MB`）。
///
/// 附件清单要让「有没有扫描件、是不是空文件」一眼可见，用字节数没人看得懂。
pub fn format_size(bytes: i64) -> String {
    if bytes < 0 {
        return "—".to_string();
    }
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < MB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / MB)
    }
}

/// 合同 PDF 的页脚模板（CDP `footerTemplate`）：单号 + 页码 + 骑缝章提示位。
///
/// 页码只能是 CDP 的 `.pageNumber` / `.totalPages`（`@page` 计数器在
/// `printToPDF` 下不生效），字号与字体必须显式写，否则页脚不可见。
pub fn contract_footer(doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>合同 {}　（多页请加盖骑缝章）</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        escape_html(doc_no)
    )
}

/// 出入库单 PDF 的页脚模板：单号 + 页码。
pub fn stock_footer(kind: StockDocKind, doc_no: &str) -> String {
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

// ==================== ④ 出入库单模板（PDF-03：收货入库单 / 出库单） ====================

/// 出入库单种类（决定标题、表名、合计口径与签字栏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockDocKind {
    /// 收货入库单（`biz_stock_in`）
    StockIn,
    /// 出库单（`biz_stock_out`）
    StockOut,
}

impl StockDocKind {
    /// 页眉标题。
    pub fn title(self) -> &'static str {
        match self {
            StockDocKind::StockIn => "收 货 入 库 单",
            StockDocKind::StockOut => "出 库 单",
        }
    }

    /// 单据名（文件名 / 日志用）。
    pub fn label(self) -> &'static str {
        match self {
            StockDocKind::StockIn => "收货入库单",
            StockDocKind::StockOut => "出库单",
        }
    }

    /// `shared::doc_kind` 的落库值。
    pub fn doc_kind_str(self) -> &'static str {
        match self {
            StockDocKind::StockIn => "STOCK_IN",
            StockDocKind::StockOut => "STOCK_OUT",
        }
    }

    /// 单据表名（`operation_logs.target_type`）。
    pub fn table(self) -> &'static str {
        match self {
            StockDocKind::StockIn => "biz_stock_in",
            StockDocKind::StockOut => "biz_stock_out",
        }
    }

    /// 是否有税额口径（入库单有金额 / 税额 / 价税合计三档；出库单只有成本结转金额）。
    pub fn has_tax(self) -> bool {
        matches!(self, StockDocKind::StockIn)
    }

    /// 签字栏（收货 / 发货两套，顺序即打印顺序）。
    pub fn sign_caps(self) -> Vec<String> {
        match self {
            StockDocKind::StockIn => vec![
                "收货人：".to_string(),
                "质检：".to_string(),
                "仓管：".to_string(),
                "供应商送货人：".to_string(),
                "制单：".to_string(),
            ],
            StockDocKind::StockOut => vec![
                "发货人：".to_string(),
                "领用人／收货人：".to_string(),
                "仓管：".to_string(),
                "审核：".to_string(),
                "制单：".to_string(),
            ],
        }
    }
}

/// 明细行（数量 / 金额已是**展示字符串**，模板不做任何计算）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StockItem {
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
    /// 单价（入库 = 不含税进价；出库 = 成本单价）
    pub price: String,
    /// 金额（入库 = 不含税金额；出库 = 成本结转金额）
    pub amount: String,
    /// 批次号（`—` 表示本行没有批次）
    pub lot_no: String,
    /// 库位编码（`未分配` 表示未指定库位）
    pub location: String,
}

/// 序列号清单的一行（按物料分组）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerialGroup {
    /// 序号
    pub seq: u32,
    /// 物料编码
    pub material_code: String,
    /// 物料名称
    pub material_name: String,
    /// 本单登记的序列号（顿号分隔）
    pub numbers: String,
}

/// 一张出入库单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名
    pub doc_label: String,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 明细
    pub items: Vec<StockItem>,
    /// 序列号清单
    pub serial_groups: Vec<SerialGroup>,
    /// 是否有序列号清单（决定整段是否出现）
    pub has_serials: bool,
    /// 行数合计
    pub row_count: String,
    /// 数量合计
    pub total_qty: String,
    /// 金额合计（入库：不含税；出库：成本结转）
    pub total_amount: String,
    /// 税额合计（出库为空串）
    pub total_tax: String,
    /// 价税合计（出库为空串）
    pub total_with_tax: String,
    /// 大写金额
    pub amount_cn: String,
    /// 大写对应的金额小写（入库 = 价税合计；出库 = 成本结转合计）
    pub amount_for_upper: String,
    /// 是否展示税额行
    pub show_tax: bool,
    /// 币种代码
    pub currency: String,
    /// 币种符号
    pub currency_symbol: String,
    /// 大写金额前缀
    pub currency_cn: String,
    /// 事由
    pub reason: String,
    /// 备注
    pub remark: String,
    /// 签字栏
    pub sign_caps: Vec<String>,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`StockContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct StockContextArgs {
    /// 单据种类
    pub kind: StockDocKind,
    /// 单号
    pub doc_no: String,
    /// 单据头字段
    pub meta: Vec<OrderMetaField>,
    /// 币种代码
    pub currency_code: String,
    /// 明细
    pub items: Vec<StockItem>,
    /// 序列号清单
    pub serial_groups: Vec<SerialGroup>,
    /// 数量合计（原始值）
    pub total_qty: Decimal,
    /// 金额合计（元）
    pub total_amount: Decimal,
    /// 税额合计（元；出库传 0）
    pub total_tax: Decimal,
    /// 价税合计（元；出库传 0）
    pub total_with_tax: Decimal,
    /// 事由
    pub reason: String,
    /// 备注
    pub remark: String,
    /// 打印时间
    pub printed_at: String,
}

impl StockContext {
    /// 组装上下文。大写金额取「价税合计」（入库）/「成本结转合计」（出库）。
    pub fn new(args: StockContextArgs) -> Self {
        let currency_code = args.currency_code.trim().to_ascii_uppercase();
        let is_cny = currency_code == "CNY" || currency_code.is_empty();
        let upper_of = if args.kind.has_tax() {
            args.total_with_tax
        } else {
            args.total_amount
        };
        let upper_fen = yuan_to_fen(upper_of);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: args.kind.title().to_string(),
            doc_label: args.kind.label().to_string(),
            doc_no: args.doc_no,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            has_serials: !args.serial_groups.is_empty(),
            serial_groups: args.serial_groups,
            row_count: format_int(args.items.len() as i64),
            items: args.items,
            total_qty: format_quantity(args.total_qty),
            total_amount: format_money(yuan_to_fen(args.total_amount)),
            total_tax: if args.kind.has_tax() {
                format_money(yuan_to_fen(args.total_tax))
            } else {
                String::new()
            },
            total_with_tax: if args.kind.has_tax() {
                format_money(yuan_to_fen(args.total_with_tax))
            } else {
                String::new()
            },
            amount_for_upper: format_money(upper_fen),
            amount_cn: amount_upper(upper_fen),
            show_tax: args.kind.has_tax(),
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
            currency_cn: if is_cny { "人民币".to_string() } else { String::new() },
            reason: args.reason,
            remark: args.remark,
            sign_caps: args.kind.sign_caps(),
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染出入库单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_stock(ctx: &StockContext) -> Result<String, PdfError> {
    let context = Context::from_serialize(ctx)
        .map_err(|error| PdfError::Template(format!("出入库单上下文序列化失败：{error}")))?;
    tera()
        .render(STOCK_NAME, &context)
        .map_err(|error| PdfError::Template(format!("出入库单模板渲染失败：{error}")))
}

/// 单据 PDF 的通用渲染参数（A4 纵向 + 页脚页码；下边距 16mm 给页脚留打印区）。
fn doc_pdf_opts(footer: String, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    use crate::business::pdf::dto::{PageSize, PdfMargins, PdfOpts};
    PdfOpts {
        page: PageSize::A4,
        landscape: false,
        prefer_css_page_size: false,
        margins: PdfMargins {
            top_mm: 12.0,
            right_mm: 10.0,
            bottom_mm: 16.0,
            left_mm: 10.0,
        },
        display_header_footer: true,
        header_template: Some("<span></span>".to_string()),
        footer_template: Some(footer),
        filename,
        ..PdfOpts::default()
    }
}

/// 合同 PDF 的渲染参数（A4 纵向 + 页脚：页码 + 骑缝章提示位）。
pub fn contract_pdf_opts(doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(contract_footer(doc_no), filename)
}

/// 出入库单 PDF 的渲染参数。
pub fn stock_pdf_opts(
    kind: StockDocKind,
    doc_no: &str,
    filename: Option<String>,
) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(stock_footer(kind, doc_no), filename)
}

/// 生成「样本合同」上下文（联调 / 验收用，数据确定性、不依赖数据库）。
///
/// # 参数
///
/// - `attachments`：附件份数（0 = 走「无附件」分支）。
pub fn sample_contract(attachments: u32) -> ContractContext {
    let roles = ["合同扫描件", "补充协议", "验收单", "其他"];
    let files: Vec<ContractAttachment> = (0..attachments)
        .map(|index| ContractAttachment {
            seq: index + 1,
            file_name: format!("采购合同-HT-20261108-000001-第{}页.pdf", index + 1),
            role_label: roles[(index % 4) as usize].to_string(),
            size: format_size(1024 * (128 + index as i64 * 37)),
            uploader: "张三".to_string(),
            uploaded_at: format!("2026-11-0{} 10:{:02}", index % 8 + 1, index % 60),
        })
        .collect();
    ContractContext::new(ContractContextArgs {
        doc_no: "HT-20261108-000001".to_string(),
        contract_name: "不锈钢管件年度采购合同".to_string(),
        meta: vec![
            OrderMetaField::new("合同号", "HT-20261108-000001"),
            OrderMetaField::new("合同类型", "采购合同"),
            OrderMetaField::new("状态", "已签订"),
            OrderMetaField::new("负责人", "李四"),
            OrderMetaField::new("签订日期", "2026-11-01"),
            OrderMetaField::new("制单人", "张三"),
        ],
        party_a: vec![
            OrderMetaField::new("名称", COMPANY_NAME),
            OrderMetaField::new("地址", "上海市浦东新区张江路 100 号"),
            OrderMetaField::new("联系人", "王五"),
            OrderMetaField::new("电话", "021-5000 0000"),
        ],
        party_b: vec![
            OrderMetaField::new("名称", "北京某某机电设备有限公司"),
            OrderMetaField::new("地址", "北京市朝阳区建国路 8 号"),
            OrderMetaField::new("联系人", "赵六"),
            OrderMetaField::new("电话", "010-6000 0000"),
        ],
        party_a_name: COMPANY_NAME.to_string(),
        party_b_name: "北京某某机电设备有限公司".to_string(),
        currency_code: "CNY".to_string(),
        amount: Decimal::new(123_456_789, 2),
        term_range: term_range_of(Some("2026-11-01"), Some("2027-10-31")),
        payment_terms: "货到验收合格后 30 日内电汇支付，质保金 5% 于质保期满后支付".to_string(),
        body: "第一条 标的物\n\
               乙方向甲方供应不锈钢管件若干，具体规格、数量以甲方下达的采购订单为准。\n\n\
               第二条 质量标准\n\
               产品须符合 GB/T 12459-2017 标准，随货提供材质证明书与出厂检验报告。\n\n\
               第三条 交货方式\n\
               乙方负责送货至甲方指定仓库，运费、保险费由乙方承担；风险自验收合格时转移。\n\n\
               第四条 付款方式\n\
               货到验收合格后 30 日内电汇支付，质保金 5% 于质保期满后支付。\n\n\
               第五条 违约责任\n\
               逾期交货的，每逾期一日按合同总额的 0.5‰ 支付违约金，累计不超过合同总额的 5%。"
            .to_string(),
        attachments: files,
        printed_at: "2026-11-08 19:30".to_string(),
    })
}

/// 生成「样本出入库单」上下文（联调 / 验收用）。
///
/// # 参数
///
/// - `kind`：单据种类；
/// - `rows`：明细行数。
pub fn sample_stock(kind: StockDocKind, rows: u32) -> StockContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_qty = Decimal::ZERO;
    let mut total_amount = Decimal::ZERO;
    let mut total_tax = Decimal::ZERO;
    for index in 0..rows {
        let qty = Decimal::from(((index % 9) + 1) as i64 * 5);
        let price = Decimal::new(1234 + (index % 37) as i64 * 11, 2);
        let amount = (price * qty).round_dp(2);
        let tax = (amount * Decimal::new(13, 2) / Decimal::from(100)).round_dp(2);
        total_qty += qty;
        total_amount += amount;
        total_tax += tax;
        items.push(StockItem {
            seq: index + 1,
            material_code: format!("MAT-{:05}", index % 97 + 1),
            material_name: format!("{}Φ{}mm 不锈钢法兰", names[(index % 4) as usize], index % 90 + 10),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            qty: format_quantity(qty),
            price: format_quantity(price),
            amount: format_money(yuan_to_fen(amount)),
            lot_no: format!("LOT2026110{}", index % 9 + 1),
            location: format!("A-{:02}-{:02}", index % 6 + 1, index % 12 + 1),
        });
    }
    let serial_groups = if kind == StockDocKind::StockIn && rows > 0 {
        vec![SerialGroup {
            seq: 1,
            material_code: items[0].material_code.clone(),
            material_name: items[0].material_name.clone(),
            numbers: "SN-20261108-000001、SN-20261108-000002、SN-20261108-000003".to_string(),
        }]
    } else {
        Vec::new()
    };
    StockContext::new(StockContextArgs {
        kind,
        doc_no: match kind {
            StockDocKind::StockIn => "IN-20261108-000001".to_string(),
            StockDocKind::StockOut => "OUT-20261108-000001".to_string(),
        },
        meta: vec![
            OrderMetaField::new(
                "单号",
                match kind {
                    StockDocKind::StockIn => "IN-20261108-000001",
                    StockDocKind::StockOut => "OUT-20261108-000001",
                },
            ),
            OrderMetaField::new("日期", "2026-11-08"),
            OrderMetaField::new("状态", "已过账"),
            OrderMetaField::new(
                match kind {
                    StockDocKind::StockIn => "供应商",
                    StockDocKind::StockOut => "客户",
                },
                "北京某某机电设备有限公司",
            ),
            OrderMetaField::new("仓库", "上海一号仓"),
            OrderMetaField::new(
                "来源单据号",
                match kind {
                    StockDocKind::StockIn => "PO-20261108-000004",
                    StockDocKind::StockOut => "SH-20261108-000003",
                },
            ),
            OrderMetaField::new("经办人", "李四"),
            OrderMetaField::new(
                "类型",
                match kind {
                    StockDocKind::StockIn => "采购入库",
                    StockDocKind::StockOut => "领用",
                },
            ),
            OrderMetaField::new("库位", "A-01-03 / A-01-04"),
        ],
        currency_code: "CNY".to_string(),
        items,
        serial_groups,
        total_qty,
        total_amount,
        total_tax,
        total_with_tax: total_amount + total_tax,
        reason: match kind {
            StockDocKind::StockIn => "采购订单 PO-20261108-000004 到货验收".to_string(),
            StockDocKind::StockOut => "生产领用（一号线检修）".to_string(),
        },
        remark: "收货时外包装完好，随货附材质证明书。".to_string(),
        printed_at: "2026-11-08 19:30".to_string(),
    })
}

// ==================== ⑤ 调拨单模板（PDF-04：仓间调拨） ====================

/// 调拨明细行（数量 / 金额已是**展示字符串**，模板不做任何计算）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferItem {
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
    /// 调拨数量（恒正；方向由调出 / 调入仓库表达）
    pub qty: String,
    /// 调出库位编码（`未分配` = 未指定库位）
    pub from_location: String,
    /// 调入库位编码
    pub to_location: String,
    /// 单位成本（调出仓移动加权均价快照）
    pub unit_cost: String,
    /// 金额（= 数量 × 单位成本快照）
    pub amount: String,
}

/// 一张调拨单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名
    pub doc_label: String,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 调出仓库名（路线条 + 验收②）
    pub from_warehouse: String,
    /// 调入仓库名（路线条 + 验收②）
    pub to_warehouse: String,
    /// 在途仓名（未发货 / 未指定时为空串，路线条不显示「在途经」）
    pub transit_warehouse: String,
    /// 明细
    pub items: Vec<TransferItem>,
    /// 行数合计
    pub row_count: String,
    /// 数量合计
    pub total_qty: String,
    /// 成本金额合计
    pub total_amount: String,
    /// 大写金额
    pub amount_cn: String,
    /// 大写对应的小写金额
    pub amount_for_upper: String,
    /// 币种代码
    pub currency: String,
    /// 币种符号
    pub currency_symbol: String,
    /// 大写金额前缀（人民币 / 空）
    pub currency_cn: String,
    /// 调拨事由
    pub reason: String,
    /// 备注
    pub remark: String,
    /// 签字栏
    pub sign_caps: Vec<String>,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`TransferContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct TransferContextArgs {
    /// 单号
    pub doc_no: String,
    /// 单据头字段
    pub meta: Vec<OrderMetaField>,
    /// 调出仓库名
    pub from_warehouse: String,
    /// 调入仓库名
    pub to_warehouse: String,
    /// 在途仓名（无则 `None`）
    pub transit_warehouse: Option<String>,
    /// 明细
    pub items: Vec<TransferItem>,
    /// 数量合计（原始值）
    pub total_qty: Decimal,
    /// 成本金额合计（元）
    pub total_amount: Decimal,
    /// 调拨事由
    pub reason: String,
    /// 备注
    pub remark: String,
    /// 打印时间
    pub printed_at: String,
}

impl TransferContext {
    /// 组装上下文。金额口径 = 成本金额（**无税额**：调拨是原价转移，不是购销）。
    pub fn new(args: TransferContextArgs) -> Self {
        let fen = yuan_to_fen(args.total_amount);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: "调 拨 单".to_string(),
            doc_label: "调拨单".to_string(),
            doc_no: args.doc_no,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            from_warehouse: args.from_warehouse,
            to_warehouse: args.to_warehouse,
            transit_warehouse: args.transit_warehouse.unwrap_or_default(),
            row_count: format_int(args.items.len() as i64),
            items: args.items,
            total_qty: format_quantity(args.total_qty),
            total_amount: format_money(fen),
            amount_for_upper: format_money(fen),
            amount_cn: amount_upper(fen),
            currency: "CNY".to_string(),
            currency_symbol: "￥".to_string(),
            currency_cn: "人民币".to_string(),
            reason: args.reason,
            remark: args.remark,
            sign_caps: vec![
                "调出仓管：".to_string(),
                "调入仓管：".to_string(),
                "经办人：".to_string(),
                "审核：".to_string(),
                "制单：".to_string(),
            ],
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染调拨单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_transfer(ctx: &TransferContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("调拨单上下文序列化失败：{error}")))?;
    tera()
        .render(TRANSFER_NAME, &context)
        .map_err(|error| PdfError::Template(format!("调拨单模板渲染失败：{error}")))
}

/// 调拨单页脚（单号 + 「第 X 页 / 共 Y 页」）。
pub fn transfer_footer(doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        "调拨单",
        escape_html(doc_no)
    )
}

/// 调拨单 PDF 的渲染参数（A4 纵向）。
pub fn transfer_pdf_opts(doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(transfer_footer(doc_no), filename)
}

/// 生成「样本调拨单」上下文（联调 / 验收用，数据确定性、不依赖数据库）。
pub fn sample_transfer(rows: u32) -> TransferContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_qty = Decimal::ZERO;
    let mut total_amount = Decimal::ZERO;
    for index in 0..rows {
        let qty = Decimal::from(((index % 9) + 1) as i64 * 3);
        let cost = Decimal::new(123_456 + (index % 37) as i64 * 1100, 4);
        let amount = (cost * qty).round_dp(2);
        total_qty += qty;
        total_amount += amount;
        items.push(TransferItem {
            seq: index + 1,
            material_code: format!("MAT-{:05}", index % 97 + 1),
            material_name: format!("{}Φ{}mm 不锈钢法兰", names[(index % 4) as usize], index % 90 + 10),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            qty: format_quantity(qty),
            from_location: format!("A-{:02}-{:02}", index % 6 + 1, index % 12 + 1),
            to_location: format!("B-{:02}-{:02}", index % 6 + 1, index % 12 + 1),
            unit_cost: format_quantity(cost),
            amount: format_money(yuan_to_fen(amount)),
        });
    }
    TransferContext::new(TransferContextArgs {
        doc_no: "TK-20261108-000001".to_string(),
        meta: vec![
            OrderMetaField::new("单号", "TK-20261108-000001"),
            OrderMetaField::new("调拨日期", "2026-11-08"),
            OrderMetaField::new("状态", "已签收"),
            OrderMetaField::new("调出仓库", "上海一号仓"),
            OrderMetaField::new("调入仓库", "上海二号仓"),
            OrderMetaField::new("在途仓", "在途仓"),
            OrderMetaField::new("申请人", "李四"),
            OrderMetaField::new("审批人", "王五"),
            OrderMetaField::new("经办人", "李四"),
        ],
        from_warehouse: "上海一号仓".to_string(),
        to_warehouse: "上海二号仓".to_string(),
        transit_warehouse: Some("在途仓".to_string()),
        items,
        total_qty,
        total_amount,
        reason: "二号仓备货，平衡区域库存".to_string(),
        remark: "调拨前已在两仓完成实物清点。".to_string(),
        printed_at: "2026-11-08 19:30".to_string(),
    })
}

// ==================== ⑥ 盘点单模板（PDF-04：盘点表，留空白手写栏） ====================

/// 盘点明细行。
///
/// # 为什么**没有** `actual_qty` / `diff_qty` 字段
///
/// 盘点表是拿去仓库现场手写的表：实盘数、差异、备注三栏必须留空（验收①）。
/// 结构体里干脆不放这三个字段 —— 模板想印也印不出来，
/// 「有人顺手把系统值填进去」这条错路在类型层面就被堵死。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StockTakeItem {
    /// 序号
    pub seq: u32,
    /// 物料编码
    pub material_code: String,
    /// 物料名称
    pub material_name: String,
    /// 规格
    pub spec: String,
    /// 计量单位
    pub unit: String,
    /// 库位编码（`未分配` = 未分配库位桶）
    pub location: String,
    /// 账面数量（建单时快照）
    pub book_qty: String,
}

/// 一张盘点单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockTakeContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名
    pub doc_label: String,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 明细（只有账面数量，实盘 / 差异 / 备注在模板里留空）
    pub items: Vec<StockTakeItem>,
    /// 行数合计
    pub row_count: String,
    /// 账面数量合计
    pub total_book_qty: String,
    /// 盘点类型（全盘 / 抽盘）
    pub take_type: String,
    /// 是否展示差异合计（盘盈 / 盘亏任一非零）
    pub show_diff: bool,
    /// 盘盈数量合计
    pub total_gain_qty: String,
    /// 盘亏数量合计（绝对值）
    pub total_loss_qty: String,
    /// 差异金额合计（盘盈 + 盘亏）
    pub total_diff_amount: String,
    /// 币种代码
    pub currency: String,
    /// 空白栏填表说明
    pub blank_hint: String,
    /// 盘点范围 / 备注
    pub remark: String,
    /// 签字栏
    pub sign_caps: Vec<String>,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`StockTakeContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct StockTakeContextArgs {
    /// 单号
    pub doc_no: String,
    /// 单据头字段
    pub meta: Vec<OrderMetaField>,
    /// 明细
    pub items: Vec<StockTakeItem>,
    /// 账面数量合计
    pub total_book_qty: Decimal,
    /// 盘点类型（FULL / PARTIAL）
    pub take_type: String,
    /// 盘盈数量合计
    pub gain_qty: Decimal,
    /// 盘亏数量合计
    pub loss_qty: Decimal,
    /// 差异金额合计（盘盈金额 + 盘亏金额）
    pub diff_amount: Decimal,
    /// 盘点范围 / 备注
    pub remark: String,
    /// 打印时间
    pub printed_at: String,
}

impl StockTakeContext {
    /// 组装上下文。
    ///
    /// # 差异区块的口径
    ///
    /// 只有**库里已经有差异**（单据已过账 / 已录实盘）时才打印盘盈盘亏合计，
    /// 否则一张全空白的盘点表上出现「盘盈 0 / 盘亏 0」会让人误以为已经盘过。
    pub fn new(args: StockTakeContextArgs) -> Self {
        let has_diff = !args.gain_qty.is_zero() || !args.loss_qty.is_zero() || !args.diff_amount.is_zero();
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: "盘 点 表".to_string(),
            doc_label: "盘点单".to_string(),
            doc_no: args.doc_no,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            row_count: format_int(args.items.len() as i64),
            items: args.items,
            total_book_qty: format_quantity(args.total_book_qty),
            take_type: match args.take_type.as_str() {
                "FULL" => "全盘",
                "PARTIAL" => "抽盘",
                other => other,
            }
            .to_string(),
            show_diff: has_diff,
            total_gain_qty: format_quantity(args.gain_qty),
            total_loss_qty: format_quantity(args.loss_qty),
            total_diff_amount: format_money(yuan_to_fen(args.diff_amount)),
            currency: "CNY".to_string(),
            blank_hint: STOCK_TAKE_BLANK_HINT.to_string(),
            remark: args.remark,
            sign_caps: vec![
                "盘点人：".to_string(),
                "复核人：".to_string(),
                "仓管主管：".to_string(),
                "财务：".to_string(),
                "制单：".to_string(),
            ],
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染盘点单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_stock_take(ctx: &StockTakeContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("盘点单上下文序列化失败：{error}")))?;
    tera()
        .render(STOCK_TAKE_NAME, &context)
        .map_err(|error| PdfError::Template(format!("盘点单模板渲染失败：{error}")))
}

/// 盘点单页脚。
pub fn stock_take_footer(doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        "盘点表",
        escape_html(doc_no)
    )
}

/// 盘点单 PDF 的渲染参数（A4 纵向 —— 现场盘点必须能夹在写字板上）。
pub fn stock_take_pdf_opts(doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(stock_take_footer(doc_no), filename)
}

/// 生成「样本盘点表」上下文（联调 / 验收用）。
pub fn sample_stock_take(rows: u32) -> StockTakeContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_book = Decimal::ZERO;
    for index in 0..rows {
        let book = Decimal::from(((index % 9) + 1) as i64 * 7);
        total_book += book;
        items.push(StockTakeItem {
            seq: index + 1,
            material_code: format!("MAT-{:05}", index % 97 + 1),
            material_name: format!("{}Φ{}mm 不锈钢法兰", names[(index % 4) as usize], index % 90 + 10),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            location: format!("A-{:02}-{:02}", index % 6 + 1, index % 12 + 1),
            book_qty: format_quantity(book),
        });
    }
    StockTakeContext::new(StockTakeContextArgs {
        doc_no: "ST-20261108-000001".to_string(),
        meta: vec![
            OrderMetaField::new("单号", "ST-20261108-000001"),
            OrderMetaField::new("盘点日期", "2026-11-08"),
            OrderMetaField::new("状态", "待过账"),
            OrderMetaField::new("仓库", "上海一号仓"),
            OrderMetaField::new("盘点类型", "全盘"),
            OrderMetaField::new("制单", "李四"),
        ],
        items,
        total_book_qty: total_book,
        take_type: "FULL".to_string(),
        gain_qty: Decimal::ZERO,
        loss_qty: Decimal::ZERO,
        diff_amount: Decimal::ZERO,
        remark: "年末全盘，范围 = 一号仓全部库位".to_string(),
        printed_at: "2026-11-08 19:30".to_string(),
    })
}

// ==================== ⑦ 退货单模板（PDF-04：采购退货 / 销售退货） ====================

/// 退货单种类（决定标题、表名、专有列与签字栏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnDocKind {
    /// 采购退货单（`biz_purchase_return`）
    Purchase,
    /// 销售退货单（`biz_sales_return`）
    Sales,
}

impl ReturnDocKind {
    /// 页眉标题。
    pub fn title(self) -> &'static str {
        match self {
            ReturnDocKind::Purchase => "采 购 退 货 单",
            ReturnDocKind::Sales => "销 售 退 货 单",
        }
    }

    /// 单据名（文件名 / 日志用）。
    pub fn label(self) -> &'static str {
        match self {
            ReturnDocKind::Purchase => "采购退货单",
            ReturnDocKind::Sales => "销售退货单",
        }
    }

    /// `shared::doc_kind` 的落库值（`operation_logs.detail.doc_type`）。
    pub fn doc_kind_str(self) -> &'static str {
        match self {
            ReturnDocKind::Purchase => "PURCHASE_RETURN",
            ReturnDocKind::Sales => "SALES_RETURN",
        }
    }

    /// 单据表名（`operation_logs.target_type`）。
    pub fn table(self) -> &'static str {
        match self {
            ReturnDocKind::Purchase => "biz_purchase_return",
            ReturnDocKind::Sales => "biz_sales_return",
        }
    }

    /// 明细是否打印「价税合计」列（采购退货逐行有；销售退货不存行级价税合计）。
    pub fn has_line_with_tax(self) -> bool {
        matches!(self, ReturnDocKind::Purchase)
    }

    /// 明细是否打印「序列号」列（销售退货一物一码；采购退货按原入库库位退）。
    pub fn has_serials(self) -> bool {
        matches!(self, ReturnDocKind::Sales)
    }

    /// 签字栏。
    pub fn sign_caps(self) -> Vec<String> {
        match self {
            ReturnDocKind::Purchase => vec![
                "退货经办：".to_string(),
                "仓管：".to_string(),
                "质检：".to_string(),
                "供应商签收：".to_string(),
                "制单：".to_string(),
            ],
            ReturnDocKind::Sales => vec![
                "退货经办：".to_string(),
                "仓管：".to_string(),
                "质检：".to_string(),
                "客户签收：".to_string(),
                "制单：".to_string(),
            ],
        }
    }
}

/// 退货明细行（数量 / 金额已是展示字符串）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReturnItem {
    /// 序号
    pub seq: u32,
    /// 物料编码
    pub material_code: String,
    /// 物料名称
    pub material_name: String,
    /// 规格
    pub spec: String,
    /// 计量单位
    pub unit: String,
    /// 退货数量
    pub qty: String,
    /// 不含税单价
    pub price: String,
    /// 不含税金额
    pub amount: String,
    /// 税率（百分数）
    pub tax_rate: String,
    /// 税额
    pub tax_amount: String,
    /// 价税合计（销售退货打 `—`：库里只有行级不含税金额与税额）
    pub with_tax: String,
    /// 库位编码（`整仓` = 未指定库位）
    pub location: String,
    /// 序列号（顿号分隔；没有则 `—`）
    pub serials: String,
}

/// 一张退货单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名
    pub doc_label: String,
    /// 单号
    pub doc_no: String,
    /// 单据头字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 退货原因（单独一条醒目横幅 —— 验收③）
    pub reason: String,
    /// 明细
    pub items: Vec<ReturnItem>,
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
    /// 大写金额（取价税合计）
    pub amount_cn: String,
    /// 大写对应的小写金额
    pub amount_for_upper: String,
    /// 币种代码
    pub currency: String,
    /// 币种符号
    pub currency_symbol: String,
    /// 大写金额前缀
    pub currency_cn: String,
    /// 是否打「价税合计」列
    pub show_with_tax: bool,
    /// 是否打「序列号」列
    pub show_serials: bool,
    /// 备注
    pub remark: String,
    /// 签字栏
    pub sign_caps: Vec<String>,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`ReturnContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct ReturnContextArgs {
    /// 单据种类
    pub kind: ReturnDocKind,
    /// 单号
    pub doc_no: String,
    /// 单据头字段
    pub meta: Vec<OrderMetaField>,
    /// 退货原因
    pub reason: String,
    /// 明细
    pub items: Vec<ReturnItem>,
    /// 数量合计
    pub total_qty: Decimal,
    /// 不含税金额合计
    pub total_amount: Decimal,
    /// 税额合计
    pub total_tax: Decimal,
    /// 价税合计
    pub total_with_tax: Decimal,
    /// 备注
    pub remark: String,
    /// 打印时间
    pub printed_at: String,
}

impl ReturnContext {
    /// 组装上下文；大写金额取**价税合计**（与购销单据同一口径）。
    pub fn new(args: ReturnContextArgs) -> Self {
        let fen = yuan_to_fen(args.total_with_tax);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: args.kind.title().to_string(),
            doc_label: args.kind.label().to_string(),
            doc_no: args.doc_no,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            reason: args.reason,
            row_count: format_int(args.items.len() as i64),
            items: args.items,
            total_qty: format_quantity(args.total_qty),
            total_amount: format_money(yuan_to_fen(args.total_amount)),
            total_tax: format_money(yuan_to_fen(args.total_tax)),
            total_with_tax: format_money(fen),
            amount_for_upper: format_money(fen),
            amount_cn: amount_upper(fen),
            currency: "CNY".to_string(),
            currency_symbol: "￥".to_string(),
            currency_cn: "人民币".to_string(),
            show_with_tax: args.kind.has_line_with_tax(),
            show_serials: args.kind.has_serials(),
            remark: args.remark,
            sign_caps: args.kind.sign_caps(),
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染退货单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_return_doc(ctx: &ReturnContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("退货单上下文序列化失败：{error}")))?;
    tera()
        .render(RETURN_NAME, &context)
        .map_err(|error| PdfError::Template(format!("退货单模板渲染失败：{error}")))
}

/// 退货单页脚。
pub fn return_footer(kind: ReturnDocKind, doc_no: &str) -> String {
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

/// 退货单 PDF 的渲染参数（**A4 横向**：12 列明细纵向装不下，见模板头注释）。
pub fn return_pdf_opts(
    kind: ReturnDocKind,
    doc_no: &str,
    filename: Option<String>,
) -> crate::business::pdf::dto::PdfOpts {
    let mut opts = doc_pdf_opts(return_footer(kind, doc_no), filename);
    opts.landscape = true;
    opts
}

/// 生成「样本退货单」上下文（联调 / 验收用）。
pub fn sample_return(kind: ReturnDocKind, rows: u32) -> ReturnContext {
    let names = ["球阀", "法兰", "弯头", "三通"];
    let mut items = Vec::with_capacity(rows as usize);
    let mut total_qty = Decimal::ZERO;
    let mut total_amount = Decimal::ZERO;
    let mut total_tax = Decimal::ZERO;
    for index in 0..rows {
        let qty = Decimal::from(((index % 5) + 1) as i64 * 2);
        let price = Decimal::new(123_400 + (index % 37) as i64 * 1100, 4);
        let amount = (price * qty).round_dp(2);
        let tax = (amount * Decimal::new(13, 2) / Decimal::from(100)).round_dp(2);
        total_qty += qty;
        total_amount += amount;
        total_tax += tax;
        items.push(ReturnItem {
            seq: index + 1,
            material_code: format!("MAT-{:05}", index % 97 + 1),
            material_name: format!("{}Φ{}mm 不锈钢法兰", names[(index % 4) as usize], index % 90 + 10),
            spec: format!("DN{}/PN16", 50 + (index % 8) * 10),
            unit: ["件", "台", "套", "米"][(index % 4) as usize].to_string(),
            qty: format_quantity(qty),
            price: format_quantity(price),
            amount: format_money(yuan_to_fen(amount)),
            tax_rate: "13".to_string(),
            tax_amount: format_money(yuan_to_fen(tax)),
            with_tax: match kind {
                ReturnDocKind::Purchase => format_money(yuan_to_fen(amount + tax)),
                ReturnDocKind::Sales => "—".to_string(),
            },
            location: format!("A-{:02}-{:02}", index % 6 + 1, index % 12 + 1),
            serials: match kind {
                ReturnDocKind::Sales => "SN-20261108-000001、SN-20261108-000002".to_string(),
                ReturnDocKind::Purchase => String::new(),
            },
        });
    }
    ReturnContext::new(ReturnContextArgs {
        kind,
        doc_no: match kind {
            ReturnDocKind::Purchase => "PRT-20261108-000001".to_string(),
            ReturnDocKind::Sales => "SR-20261108-000001".to_string(),
        },
        meta: match kind {
            ReturnDocKind::Purchase => vec![
                OrderMetaField::new("单号", "PRT-20261108-000001"),
                OrderMetaField::new("退货日期", "2026-11-08"),
                OrderMetaField::new("状态", "已确认"),
                OrderMetaField::new("供应商", "北京某某机电设备有限公司"),
                OrderMetaField::new("退货仓库", "上海一号仓"),
                OrderMetaField::new("经办人", "李四"),
                OrderMetaField::new("原单号（入库）", "IN-20261108-000001"),
                OrderMetaField::new("原单号（采购订单）", "PO-20261108-000004"),
                OrderMetaField::new("退货类型", "采购退货"),
            ],
            ReturnDocKind::Sales => vec![
                OrderMetaField::new("单号", "SR-20261108-000001"),
                OrderMetaField::new("退货日期", "2026-11-08"),
                OrderMetaField::new("状态", "已过账"),
                OrderMetaField::new("客户", "上海某某贸易有限公司"),
                OrderMetaField::new("退回仓库", "上海二号仓"),
                OrderMetaField::new("经办人", "张三"),
                OrderMetaField::new("原单号（销售订单）", "SO-20261108-000002"),
                OrderMetaField::new("原单号（发货单）", "SH-20261108-000003"),
                OrderMetaField::new("退货类型", "质量退货"),
            ],
        },
        reason: match kind {
            ReturnDocKind::Purchase => "阀体密封面有砂眼，供应商同意退货".to_string(),
            ReturnDocKind::Sales => "质量（QUALITY）".to_string(),
        },
        items,
        total_qty,
        total_amount,
        total_tax,
        total_with_tax: total_amount + total_tax,
        remark: "退货运输由对方承担，附质检报告一份。".to_string(),
        printed_at: "2026-11-08 19:30".to_string(),
    })
}

// ==================== ⑦ 审批单模板（PDF-05：审批轨迹） ====================

/// 一条审批轨迹（模板里的一行）。
///
/// # 为什么「降级标记」是独立字段而不是拼进 `opinion`
///
/// 卡面（ARCH-8）要求打印件上能看出「已跳过（自审）」/「已升级至 XX」/「兜底审批人」，
/// 但审批意见本身是审批人写的原文，**不许被系统改写**。两者混在一个字段里，
/// 要么污染意见（审计失真），要么被前端/单测误判成「意见里写了这句话」。
/// 因此 `opinion` 保持原文，标记走 `tag` + `tag_kind`（独立一行、独立配色）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalTrace {
    /// 级次（`第 1 级`）
    pub level: String,
    /// 退回轮次标记（`第 2 轮`；首轮为空串）
    pub round_note: String,
    /// 节点名（快照）
    pub node_name: String,
    /// 审批人（姓名快照；系统代点为「系统」）
    pub approver: String,
    /// 结果（通过 / 驳回 / 跳过 / 待审批 / 已转办 / 已作废）
    pub result: String,
    /// 结果配色类（`is-approve` / `is-reject` / `is-skip` / `is-pending`）
    pub cls: String,
    /// 时间（`yyyy-MM-dd HH:mm`；未处理为空则印 `—`）
    pub at: String,
    /// 审批意见（**原文**，不做任何加工）
    pub opinion: String,
    /// 系统代点标注（`系统代点` / 空）
    pub auto_note: String,
    /// 降级标记（ARCH-8：`已跳过（自审）` / `已升级至…` / `兜底审批人` / `委托代批`；空 = 无）
    pub tag: String,
    /// 标记配色类（`is-skip` / `is-escalate` / `is-fallback` / `is-delegate`；空 = 无）
    pub tag_kind: String,
}

/// 一级的签字位（空白栏，由审批人现场手写）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalSignSlot {
    /// 级次（`第 1 级`）
    pub level: String,
    /// 节点名
    pub node_name: String,
    /// 该级审批人（多个用「、」连）
    pub approver: String,
}

/// 一张审批单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名
    pub doc_label: String,
    /// 单号（`biz_flow_instance.instance_no`）
    pub doc_no: String,
    /// 审批结论（`已通过` / `已驳回` / `审批中` …）
    pub status: String,
    /// 结论配色类（`is-approve` / `is-reject` / `is-pending` / `is-void`）
    pub status_kind: String,
    /// 单据信息字段（每行 3 组）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 审批轨迹（按轮次 + 落待办时间排序）
    pub traces: Vec<ApprovalTrace>,
    /// 各级签字位
    pub sign_slots: Vec<ApprovalSignSlot>,
    /// 轨迹条数（展示串，千分位）
    pub trace_count: String,
    /// 级数（展示串）
    pub degree_count: String,
    /// 结束说明（`result_comment`，可为空）
    pub result_comment: String,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`ApprovalContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct ApprovalContextArgs {
    /// 单号
    pub doc_no: String,
    /// 审批结论文案
    pub status: String,
    /// 结论配色类
    pub status_kind: String,
    /// 单据信息字段
    pub meta: Vec<OrderMetaField>,
    /// 审批轨迹
    pub traces: Vec<ApprovalTrace>,
    /// 各级签字位
    pub sign_slots: Vec<ApprovalSignSlot>,
    /// 结束说明
    pub result_comment: String,
    /// 打印时间
    pub printed_at: String,
}

impl ApprovalContext {
    /// 组装上下文（标题 / 公司名 / 计数都在这里定死，调用方只给数据）。
    pub fn new(args: ApprovalContextArgs) -> Self {
        let trace_count = format_int(args.traces.len() as i64);
        let degree_count = format_int(args.sign_slots.len() as i64);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: "审 批 单".to_string(),
            doc_label: "审批单".to_string(),
            doc_no: args.doc_no,
            status: args.status,
            status_kind: args.status_kind,
            meta_rows: args.meta.chunks(3).map(<[OrderMetaField]>::to_vec).collect(),
            trace_count,
            degree_count,
            traces: args.traces,
            sign_slots: args.sign_slots,
            result_comment: args.result_comment,
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染审批单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_approval(ctx: &ApprovalContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("审批单上下文序列化失败：{error}")))?;
    tera()
        .render(APPROVAL_NAME, &context)
        .map_err(|error| PdfError::Template(format!("审批单模板渲染失败：{error}")))
}

/// 审批单页脚（单号 + 「第 X 页 / 共 Y 页」）。
pub fn approval_footer(doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        "审批单",
        escape_html(doc_no)
    )
}

/// 审批单 PDF 的渲染参数（A4 纵向：轨迹 6 列 + 意见列吃剩余宽度）。
pub fn approval_pdf_opts(doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(approval_footer(doc_no), filename)
}

/// 生成「样本审批单」上下文（联调 / 验收用，覆盖 ARCH-8 三种降级标记）。
pub fn sample_approval() -> ApprovalContext {
    let trace = |level: &str,
                 round_note: &str,
                 node: &str,
                 who: &str,
                 result: &str,
                 cls: &str,
                 at: &str,
                 opinion: &str,
                 tag: &str,
                 tag_kind: &str| ApprovalTrace {
        level: level.to_string(),
        round_note: round_note.to_string(),
        node_name: node.to_string(),
        approver: who.to_string(),
        result: result.to_string(),
        cls: cls.to_string(),
        at: at.to_string(),
        opinion: opinion.to_string(),
        auto_note: String::new(),
        tag: tag.to_string(),
        tag_kind: tag_kind.to_string(),
    };

    let traces = vec![
        trace(
            "第 1 级",
            "",
            "部门经理审批",
            "王五",
            "跳过",
            "is-skip",
            "2026-11-08 09:12",
            "制单人不可审批本人单据",
            "已跳过（自审）",
            "is-skip",
        ),
        trace(
            "第 1 级",
            "",
            "部门经理审批",
            "赵六",
            "通过",
            "is-approve",
            "2026-11-08 09:40",
            "金额与合同条款一致，同意",
            "",
            "",
        ),
        trace(
            "第 2 级",
            "",
            "总经理审批",
            "孙七",
            "通过",
            "is-approve",
            "2026-11-08 10:05",
            "同意，按流程执行",
            "已升级至 总经理（原角色无人）",
            "is-escalate",
        ),
        trace(
            "第 3 级",
            "",
            "财务复核",
            "周八",
            "通过",
            "is-approve",
            "2026-11-08 10:30",
            "票据已核",
            "兜底审批人",
            "is-fallback",
        ),
    ];
    ApprovalContext::new(ApprovalContextArgs {
        doc_no: "FLW-20261108-00012345".to_string(),
        status: "已通过".to_string(),
        status_kind: "is-approve".to_string(),
        meta: vec![
            OrderMetaField::new("单据类型", "PURCHASE_ORDER"),
            OrderMetaField::new("单号", "FLW-20261108-00012345"),
            OrderMetaField::new("业务单号", "PO-20261108-000004"),
            OrderMetaField::new("金额", "12,345.67"),
            OrderMetaField::new("申请人", "张三"),
            OrderMetaField::new("申请时间", "2026-11-08 09:00"),
            OrderMetaField::new("流程", "采购订单审批（v1）"),
            OrderMetaField::new("状态", "已通过"),
            OrderMetaField::new("完成时间", "2026-11-08 10:30"),
        ],
        traces,
        sign_slots: vec![
            ApprovalSignSlot {
                level: "第 1 级".to_string(),
                node_name: "部门经理审批".to_string(),
                approver: "王五、赵六".to_string(),
            },
            ApprovalSignSlot {
                level: "第 2 级".to_string(),
                node_name: "总经理审批".to_string(),
                approver: "孙七".to_string(),
            },
            ApprovalSignSlot {
                level: "第 3 级".to_string(),
                node_name: "财务复核".to_string(),
                approver: "周八".to_string(),
            },
        ],
        result_comment: String::new(),
        printed_at: "2026-11-08 21:40".to_string(),
    })
}

// ==================== ⑧ 工作任务派工单模板（PDF-06） ====================

/// 派工单的「检查项」一行（= 任务的一条直接子任务）。
///
/// # 为什么只有「序号 / 标题 / 负责人 / 截止日」，没有「已完成」
///
/// 卡面验收②：检查项要「显示为勾选框（**空白供手填**）」。派工单是发给接收人
/// 拿去现场用的纸质单据，勾选框由接收人完成一项画一项；把库里的 `status`
/// 预先印成 ☑ 会让「系统里已标完成」和「纸上已勾」混为一谈，纸质件就不再是
/// 接收人的确认凭证。所以本结构体**刻意不携带子任务状态**——
/// 模板想印也印不出来（与盘点表的空白三栏同一手法）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskChecklistItem {
    /// 序号（1 起，展示串）
    pub seq: String,
    /// 检查项标题（子任务标题）
    pub title: String,
    /// 负责人姓名（子任务负责人；已删号则空）
    pub owner: String,
    /// 截止日（`yyyy-MM-dd`；无则 `—`）
    pub due: String,
}

/// 签字栏的一格（标签在上、预印提示在下，签名区留空）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSignCell {
    /// 栏名（`派工人` / `接收人` / `完成确认` / `日期`）
    pub label: String,
    /// 预印提示（该由谁签 / 验收口径；空 = 无）
    pub hint: String,
}

/// 一张派工单的渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// 公司名
    pub company: String,
    /// 页眉标题
    pub doc_title: String,
    /// 单据名（下载文件名的前缀）
    pub doc_label: String,
    /// 任务号（`biz_task.task_no`）
    pub doc_no: String,
    /// 任务标题（整行大字）
    pub subject: String,
    /// 状态文案（`待开始` / `进行中` / `待验收` / `已完成` / `已取消`）
    pub status: String,
    /// 状态配色类（`is-todo` / `is-doing` / `is-review` / `is-done` / `is-void`）
    pub status_kind: String,
    /// 任务信息字段（每行 2 组：中文字段名 + 姓名 / 项目名较长，3 组会把值挤成两行）
    pub meta_rows: Vec<Vec<OrderMetaField>>,
    /// 任务描述**原文**（换行 / 缩进原样保留，模板 `white-space: pre-wrap`）
    pub description: String,
    /// 描述是否非空（空 → 印「没有填写描述」的占位块）
    pub has_description: bool,
    /// 检查项
    pub checklist: Vec<TaskChecklistItem>,
    /// 是否有检查项（无 → 整节不出现，不印空表）
    pub has_checklist: bool,
    /// 检查项条数（展示串）
    pub checklist_count: String,
    /// 签字栏（固定 4 格：派工人 / 接收人 / 完成确认 / 日期）
    pub sign_cells: Vec<TaskSignCell>,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// [`TaskContext`] 的构造入参。
#[derive(Debug, Clone)]
pub struct TaskContextArgs {
    /// 任务号
    pub doc_no: String,
    /// 任务标题
    pub subject: String,
    /// 状态文案
    pub status: String,
    /// 状态配色类
    pub status_kind: String,
    /// 任务信息字段
    pub meta: Vec<OrderMetaField>,
    /// 任务描述原文
    pub description: String,
    /// 检查项
    pub checklist: Vec<TaskChecklistItem>,
    /// 签字栏
    pub sign_cells: Vec<TaskSignCell>,
    /// 打印时间
    pub printed_at: String,
}

impl TaskContext {
    /// 组装上下文（标题 / 公司名 / 计数都在这里定死，调用方只给数据）。
    pub fn new(args: TaskContextArgs) -> Self {
        let checklist_count = format_int(args.checklist.len() as i64);
        Self {
            company: COMPANY_NAME.to_string(),
            doc_title: "工 作 任 务 派 工 单".to_string(),
            doc_label: "派工单".to_string(),
            doc_no: args.doc_no,
            subject: args.subject,
            status: args.status,
            status_kind: args.status_kind,
            meta_rows: args.meta.chunks(2).map(<[OrderMetaField]>::to_vec).collect(),
            has_description: !args.description.trim().is_empty(),
            description: args.description,
            has_checklist: !args.checklist.is_empty(),
            checklist_count,
            checklist: args.checklist,
            sign_cells: args.sign_cells,
            printed_at: args.printed_at,
            engine: "Chromium · CDP printToPDF".to_string(),
        }
    }
}

/// 渲染派工单 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_task(ctx: &TaskContext) -> Result<String, PdfError> {
    let context =
        Context::from_serialize(ctx).map_err(|error| PdfError::Template(format!("派工单上下文序列化失败：{error}")))?;
    tera()
        .render(TASK_NAME, &context)
        .map_err(|error| PdfError::Template(format!("派工单模板渲染失败：{error}")))
}

/// 派工单页脚（单号 + 「第 X 页 / 共 Y 页」）。
pub fn task_footer(doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 10mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        "派工单",
        escape_html(doc_no)
    )
}

/// 派工单 PDF 的渲染参数（A4 纵向：现场夹在写字板上用，横向夹板放不下）。
pub fn task_pdf_opts(doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    doc_pdf_opts(task_footer(doc_no), filename)
}

/// 生成「样本派工单」上下文（联调 / 验收用，数据确定性、不依赖数据库）。
///
/// # 参数
///
/// - `checklist`：检查项条数（0 = 走「无检查项」分支，整节不出现）。
pub fn sample_task(checklist: u32) -> TaskContext {
    let items = (1..=checklist)
        .map(|index| TaskChecklistItem {
            seq: index.to_string(),
            title: format!("核对第 {index} 台阀体的密封面并拍照留档"),
            owner: if index % 2 == 0 { "王五" } else { "赵六" }.to_string(),
            due: format!("2026-11-{:02}", 10 + index),
        })
        .collect();

    TaskContext::new(TaskContextArgs {
        doc_no: "TSK-202611-000123".to_string(),
        subject: "云枢 3 号泵站阀门到货验收与安装".to_string(),
        status: "进行中".to_string(),
        status_kind: "is-doing".to_string(),
        meta: vec![
            OrderMetaField::new("任务号", "TSK-202611-000123"),
            OrderMetaField::new("优先级", "P1（高）"),
            OrderMetaField::new("截止日期", "2026-11-20"),
            OrderMetaField::new("负责人", "张工"),
            OrderMetaField::new("协作人", "王五、赵六"),
            OrderMetaField::new("所属项目", "PRJ-2026-011 泵站改造"),
            OrderMetaField::new("状态", "进行中"),
            OrderMetaField::new("创建人", "李经理"),
            OrderMetaField::new("分派人", "李经理"),
            OrderMetaField::new("计划开始", "2026-11-09"),
            OrderMetaField::new("预估工时", "16.0 小时"),
            OrderMetaField::new("部门", "设备部"),
        ],
        description: "1. 到货后核对装箱单与实物数量，逐台登记序列号。\n\
                      2. 检查阀体密封面无划伤、砂眼，发现问题拍照并挂红牌。\n\
                      3. 安装前确认管道已泄压、盲板已加装。\n\
                      \n\
                      注意：夜间施工需提前一天报安全科备案。"
            .to_string(),
        checklist: items,
        sign_cells: vec![
            TaskSignCell {
                label: "派工人".to_string(),
                hint: "李经理".to_string(),
            },
            TaskSignCell {
                label: "接收人".to_string(),
                hint: "张工".to_string(),
            },
            TaskSignCell {
                label: "完成确认".to_string(),
                hint: "验收人：李经理".to_string(),
            },
            TaskSignCell {
                label: "日期".to_string(),
                hint: String::new(),
            },
        ],
        printed_at: "2026-11-08 22:05".to_string(),
    })
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

// ==================== 财务报表（PDF-07） ====================

/// 三张财务报表共用的页眉（卡面要求：公司名 / 报表名称 / 编制单位 / 日期 / 单位：元）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportHead {
    /// 公司名（页眉左侧大字）
    pub company: String,
    /// 报表名称（居中大字）
    pub doc_title: String,
    /// 金额单位（`单位：元`）
    pub unit_label: String,
    /// 编制单位
    pub preparer: String,
    /// 会计期间（`2026年11月`）
    pub period_label: String,
    /// 报表日期（期末日，`2026-11-30`）
    pub date_label: String,
    /// 期间状态（`未结账` / `已结账` / `未开账·按自然月推算`）
    pub status_label: String,
}

/// 资产负债表的一格：项目名 + 期末 + 期初（金额**已格式化**，空白项是 `-`）。
///
/// 金额在 `erp::report_pdf` 里就格式化成字符串，模板只负责排版 —— 模板层
/// 一旦参与数字计算，三张报表就会各自漂出不同的舍入口径（PDF-01 起的约定）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReportCell {
    /// 项目名（减项已带「减：」前缀，只补一次）
    pub label: String,
    /// 期末余额（已格式化；空白项 `-`）
    pub end: String,
    /// 期初余额（已格式化；空白项 `-`）
    pub begin: String,
    /// 项目名单元格的样式类（`is-major` / `is-detail` / `is-deduction` / `is-blank`）
    pub cls: String,
}

impl ReportCell {
    /// 空行（对面没有对应项目时用来补位，整格透明）。
    pub fn blank() -> Self {
        Self {
            cls: "is-blank".to_string(),
            ..Self::default()
        }
    }

    /// 一格（`label` 已定稿，两个金额也已格式化）。
    pub fn new(label: impl Into<String>, end: impl Into<String>, begin: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            end: end.into(),
            begin: begin.into(),
            cls: String::new(),
        }
    }

    /// 追加样式类（层级 / 减项；空串不加，避免出现 `class=" "`）。
    pub fn with_cls(mut self, cls: &str) -> Self {
        if !cls.is_empty() {
            if !self.cls.is_empty() {
                self.cls.push(' ');
            }
            self.cls.push_str(cls);
        }
        self
    }
}

/// 资产负债表的一行：左（资产）/ 右（负债及所有者权益）两栏按行号对齐。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceReportRow {
    /// 左栏（资产）
    pub left: ReportCell,
    /// 右栏（负债及所有者权益）
    pub right: ReportCell,
    /// 整行样式（任一侧为合计行 → `is-total`）
    pub cls: String,
}

/// 勾稽校验块（三张报表共用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportCheckBlock {
    /// 结论行首句（加粗）
    pub title: String,
    /// 逐条明细（已格式化，模板逐行打印）
    pub lines: Vec<String>,
    /// 样式类：`is-ok`（平）/ `is-bad`（不平）
    pub cls: String,
}

/// 资产负债表渲染上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceReportContext {
    /// 页眉
    pub head: ReportHead,
    /// 报表名（下载文件名前缀）
    pub doc_label: String,
    /// 报表号（`BALANCE_SHEET-2026-11`，页脚与文件名用）
    pub doc_no: String,
    /// 期末列表头（`期末余额`）
    pub end_label: String,
    /// 期初列表头（`期初余额`）
    pub begin_label: String,
    /// 主体行（两栏已对齐）
    pub rows: Vec<BalanceReportRow>,
    /// 勾稽校验
    pub check: ReportCheckBlock,
    /// 附注（留抵税额重分类等）
    pub notes: Vec<String>,
    /// 是否有附注
    pub has_notes: bool,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// 利润表 / 现金流量表的一行（项目 + 两列金额，`kind` 决定视觉）。
///
/// `code` 是报表项编码（`NET_PROFIT` / `OPER_SALES_RECEIPTS` / …），**模板不打印**，
/// 但审计与单测靠它定位（行名里可能含别的行名，例如「未归类损益（未计入净利润…）」
/// 本身含「净利润」三个字，按名字找行会找错）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmountReportRow {
    /// 报表项编码（可审计；不打印）
    pub code: String,
    /// 项目名（分节标题 / 明细 / 小计 / 合计 / 说明原文）
    pub label: String,
    /// 行类型：`section` / `line` / `subtotal` / `total` / `note`
    pub kind: String,
    /// 整行样式类（由 `kind` 推出，模板直接用）
    pub cls: String,
    /// 项目名单元格的缩进类（`l1` / `l2` / `l3`）
    pub level_cls: String,
    /// 本期金额（已格式化；`note` 行为空）
    pub current: String,
    /// 本年累计（已格式化；`note` 行为空）
    pub ytd: String,
}

impl AmountReportRow {
    /// 按行类型推出样式类（`kind` 与 `cls` 由同一处产生，模板不必写判断）。
    pub fn styled(code: impl Into<String>, label: impl Into<String>, kind: &str, level: u8) -> Self {
        let level_cls = match level {
            0 | 1 => "l1",
            2 => "l2",
            _ => "l3",
        };
        Self {
            code: code.into(),
            label: label.into(),
            kind: kind.to_string(),
            cls: format!("is-{kind}"),
            level_cls: level_cls.to_string(),
            current: String::new(),
            ytd: String::new(),
        }
    }

    /// 填两列金额（已格式化）。
    pub fn with_amounts(mut self, current: impl Into<String>, ytd: impl Into<String>) -> Self {
        self.current = current.into();
        self.ytd = ytd.into();
        self
    }
}

/// 利润表 / 现金流量表渲染上下文（两张报表共用一份模板）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmountReportContext {
    /// 页眉
    pub head: ReportHead,
    /// 报表名（下载文件名前缀）
    pub doc_label: String,
    /// 报表号（`INCOME_STATEMENT-2026-11`）
    pub doc_no: String,
    /// 第一列列名（`本期金额`）
    pub current_label: String,
    /// 第二列列名（`本年累计`）
    pub ytd_label: String,
    /// 主体行
    pub rows: Vec<AmountReportRow>,
    /// 勾稽校验
    pub check: ReportCheckBlock,
    /// 附注（口径说明 / 告警）
    pub notes: Vec<String>,
    /// 是否有附注
    pub has_notes: bool,
    /// 打印时间
    pub printed_at: String,
    /// 引擎标识
    pub engine: String,
}

/// 渲染资产负债表 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_balance_report(ctx: &BalanceReportContext) -> Result<String, PdfError> {
    let context = Context::from_serialize(ctx)
        .map_err(|error| PdfError::Template(format!("资产负债表上下文序列化失败：{error}")))?;
    tera()
        .render(REPORT_BALANCE_NAME, &context)
        .map_err(|error| PdfError::Template(format!("资产负债表模板渲染失败：{error}")))
}

/// 渲染利润表 / 现金流量表 HTML。
///
/// # 异常情况
///
/// - [`PdfError::Template`]：模板渲染失败。
pub fn render_amount_report(ctx: &AmountReportContext) -> Result<String, PdfError> {
    let context = Context::from_serialize(ctx)
        .map_err(|error| PdfError::Template(format!("财务报表上下文序列化失败：{error}")))?;
    tera()
        .render(REPORT_AMOUNT_NAME, &context)
        .map_err(|error| PdfError::Template(format!("财务报表模板渲染失败：{error}")))
}

/// 财务报表页脚（报表名 + 报表号 + 「第 X 页 / 共 Y 页」）。
pub fn report_footer(doc_label: &str, doc_no: &str) -> String {
    format!(
        "<div style=\"width:100%;padding:0 8mm;font-family:'Noto Sans SC',sans-serif;\
         font-size:8pt;color:#86909c;display:flex;justify-content:space-between;\">\
         <span>{} {}</span>\
         <span>第 <span class=\"pageNumber\"></span> 页 / 共 <span class=\"totalPages\"></span> 页</span>\
         </div>",
        escape_html(doc_label),
        escape_html(doc_no)
    )
}

/// 财务报表 PDF 的渲染参数（A4 纵向：与会计凭证、账簿装订同一尺寸）。
pub fn report_pdf_opts(doc_label: &str, doc_no: &str, filename: Option<String>) -> crate::business::pdf::dto::PdfOpts {
    let mut opts = doc_pdf_opts(report_footer(doc_label, doc_no), filename);
    // 6 列（资产负债表）要在 210mm 里排下，左右边距收到 8mm（模板 @page 同值）
    opts.margins.left_mm = 8.0;
    opts.margins.right_mm = 8.0;
    opts
}

/// 生成「样本资产负债表」上下文（联调 / 验收用，不依赖数据库）。
pub fn sample_balance_report() -> BalanceReportContext {
    let mut rows = Vec::new();
    let left = [
        ("流动资产", "is-major"),
        ("  货币资金", ""),
        ("  应收账款", ""),
        ("流动资产合计", "is-total"),
        ("非流动资产", "is-major"),
        ("  固定资产净值", ""),
        ("非流动资产合计", "is-total"),
        ("资产总计", "is-total"),
    ];
    let right = [
        ("流动负债", "is-major"),
        ("  应付账款", ""),
        ("流动负债合计", "is-total"),
        ("所有者权益", "is-major"),
        ("  实收资本", ""),
        ("  未分配利润", ""),
        ("所有者权益合计", "is-total"),
        ("负债和所有者权益总计", "is-total"),
    ];
    for index in 0..left.len().max(right.len()) {
        let cell = |source: &[(&str, &str)], index: usize| match source.get(index) {
            Some((label, cls)) => ReportCell::new(*label, "1,234,567.89", "1,000,000.00").with_cls(cls),
            None => ReportCell::blank(),
        };
        rows.push(BalanceReportRow {
            left: cell(&left, index),
            right: cell(&right, index),
            cls: String::new(),
        });
    }
    BalanceReportContext {
        head: ReportHead {
            company: COMPANY_NAME.to_string(),
            doc_title: "资 产 负 债 表".to_string(),
            unit_label: "单位：元".to_string(),
            preparer: COMPANY_NAME.to_string(),
            period_label: "2026年11月".to_string(),
            date_label: "2026-11-30".to_string(),
            status_label: "未结账".to_string(),
        },
        doc_label: "资产负债表".to_string(),
        doc_no: "BALANCE_SHEET-2026-11".to_string(),
        end_label: "期末余额".to_string(),
        begin_label: "期初余额".to_string(),
        rows,
        check: ReportCheckBlock {
            title: "勾稽校验：资产总计 ≡ 负债和所有者权益总计".to_string(),
            lines: vec![
                "资产总计 1,234,567.89".to_string(),
                "负债和所有者权益总计 1,234,567.89".to_string(),
                "差额 0.00 —— 借贷平衡".to_string(),
            ],
            cls: "is-ok".to_string(),
        },
        notes: Vec::new(),
        has_notes: false,
        printed_at: "2026-11-08 22:05".to_string(),
        engine: "Chromium · CDP printToPDF".to_string(),
    }
}

/// 生成「样本利润表 / 现金流量表」上下文（`kind` 决定列名与行结构）。
pub fn sample_amount_report(kind: AmountReportKind) -> AmountReportContext {
    let (doc_label, doc_no, title) = match kind {
        AmountReportKind::Income => ("利润表", "INCOME_STATEMENT-2026-11", "利 润 表"),
        AmountReportKind::CashFlow => ("现金流量表", "CASH_FLOW-2026-11", "现 金 流 量 表"),
    };
    let rows = vec![
        AmountReportRow::styled("REVENUE", "一、营业收入", "section", 1).with_amounts("1,234,567.89", "9,876,543.21"),
        AmountReportRow::styled("COST_OF_SALES", "  减：营业成本", "line", 2)
            .with_amounts("800,000.00", "6,400,000.00"),
        AmountReportRow::styled("ADMIN_EXPENSE", "  管理费用", "line", 2).with_amounts("-", "-"),
        AmountReportRow::styled("NET_PROFIT", "二、净利润", "total", 1).with_amounts("434,567.89", "3,476,543.21"),
    ];
    AmountReportContext {
        head: ReportHead {
            company: COMPANY_NAME.to_string(),
            doc_title: title.to_string(),
            unit_label: "单位：元".to_string(),
            preparer: COMPANY_NAME.to_string(),
            period_label: "2026年11月".to_string(),
            date_label: "2026-11-30".to_string(),
            status_label: "未结账".to_string(),
        },
        doc_label: doc_label.to_string(),
        doc_no: doc_no.to_string(),
        current_label: "本期金额".to_string(),
        ytd_label: "本年累计".to_string(),
        rows,
        check: ReportCheckBlock {
            title: "勾稽校验：净利润（本年累计）≡ 未分配利润本年变动".to_string(),
            lines: vec!["净利润 3,476,543.21 —— 一致".to_string()],
            cls: "is-ok".to_string(),
        },
        notes: Vec::new(),
        has_notes: false,
        printed_at: "2026-11-08 22:05".to_string(),
        engine: "Chromium · CDP printToPDF".to_string(),
    }
}

/// 两张「项目 / 本期 / 本年累计」报表的类别（列名与标题不同，模板相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountReportKind {
    /// 利润表
    Income,
    /// 现金流量表
    CashFlow,
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
            assert_eq!(amount_upper(fen), expected, "金额 {fen} 分（{}）", format_money(fen));
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
        assert!(html.contains("display: table-header-group"), "表头重复规则必须在");
        assert!(html.contains("break-inside: avoid"), "合计/签字栏防断裂规则必须在");
        assert!(html.contains("tabular-nums"), "等宽数字必须在");
        assert!(html.contains("Noto Sans SC"), "字体族必须是静态 glyf 的 Noto Sans SC");
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
        assert!(!html.contains("<script>alert(1)</script>"), "脚本标签必须被转义");
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
        assert!(html.contains("display: table-header-group"), "跨页表头重复规则必须在");
        assert!(html.contains("break-inside: avoid"), "防断裂规则必须在");
        assert!(html.contains("tabular-nums"), "等宽数字必须在");
        assert!(html.contains("Noto Sans SC"), "中文字体族必须在");

        // 单据头 6 个字段（3 列 × 2 行）
        for label in ["单号", "日期", "状态", "供应商", "仓库", "经办人"] {
            assert!(html.contains(label), "单据头缺字段：{label}");
        }
        // 明细九列的表头
        for head in ["物料编码", "物料名称", "规格", "单位", "数量", "单价", "金额", "备注"] {
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
            (12345678901234, "壹仟贰佰叁拾肆亿伍仟陆佰柒拾捌万玖仟零壹拾贰元叁角肆分"),
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
        assert_eq!(format_quantity(Decimal::new(12345670001, 4)), "1,234,567.0001");
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
        assert_eq!(opts.filename.as_deref(), Some("采购订单-PO-20261108-000001.pdf"));
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

    // ==================== 合同模板（PDF-03） ====================

    /// 四份模板全部注册成功（语法错在测试里炸）。
    #[test]
    fn all_templates_register() {
        let names: Vec<&str> = tera().get_template_names().collect();
        for name in [STATEMENT_NAME, ORDER_NAME, CONTRACT_NAME, STOCK_NAME] {
            assert!(names.contains(&name), "模板未注册：{name}");
        }
    }

    /// 合同 HTML：甲乙双方 / 金额 / 期限 / 付款方式 / 正文 / 附件清单 /
    /// 签字盖章位（甲方盖章 / 乙方盖章 / 日期）/ 骑缝章提示，一个都不能少。
    #[test]
    fn renders_sample_contract_html() {
        let ctx = sample_contract(3);
        let html = render_contract(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("<html lang=\"zh-CN\">"));
        assert!(html.contains("合 同"), "页眉标题必须渲染");
        assert!(html.contains(COMPANY_NAME));
        assert!(html.contains("HT-20261108-000001"));
        assert!(html.contains("不锈钢管件年度采购合同"), "合同名称必须在");

        // 概要 + 甲乙双方
        for label in ["合同类型", "状态", "负责人", "签订日期", "制单人"] {
            assert!(html.contains(label), "合同概要缺字段：{label}");
        }
        assert!(html.contains("甲 方") && html.contains("乙 方"));
        assert!(html.contains("北京某某机电设备有限公司"));
        assert!(html.contains("上海市浦东新区张江路 100 号"));

        // 金额 / 期限 / 付款方式
        assert!(html.contains("1,234,567.89"), "小写金额必须千分位");
        assert!(
            html.contains("壹佰贰拾叁万肆仟伍佰陆拾柒元捌角玖分"),
            "大写金额必须与小数同源"
        );
        assert!(html.contains("人民币"));
        assert!(html.contains("2026-11-01 至 2027-10-31"), "期限必须在");
        assert!(html.contains("付款方式"));
        assert!(html.contains("货到验收合格后 30 日内电汇支付"));

        // 正文全文（五条条款一条不缺 —— 「完整渲染」的正面断言）
        for clause in [
            "第一条 标的物",
            "第二条 质量标准",
            "第三条 交货方式",
            "第四条",
            "第五条 违约责任",
        ] {
            assert!(html.contains(clause), "条款正文缺：{clause}");
        }

        // 附件清单
        assert!(html.contains("附件清单（共 3 份）"));
        assert!(html.contains("采购合同-HT-20261108-000001-第1页.pdf"));
        assert!(html.contains("合同扫描件"));
        assert!(html.contains("补充协议"));
        assert!(html.contains("KB"), "附件大小必须人类可读");

        // 签字盖章位
        assert!(html.contains("甲方（盖章）"));
        assert!(html.contains("乙方（盖章）"));
        assert!(html.contains("单位公章 / 合同专用章"));
        assert_eq!(
            html.matches("日期：________年____月____日").count(),
            2,
            "甲乙双方各一个日期位"
        );
        // 骑缝章提示
        assert!(html.contains("骑缝章"), "骑缝章提示必须在");
        assert!(html.contains(CONTRACT_SEAL_NOTE));

        assert!(html.contains("tabular-nums"), "等宽数字必须在");
        assert!(html.contains("Noto Sans SC"));
        assert!(html.contains("break-inside: avoid"), "盖章位防断裂规则必须在");
        assert!(!html.contains("class=\"pageNumber\""), "页码必须交给 footerTemplate");
    }

    /// 正文很长（多段 + 空行）时**一字不丢**：40 段全部出现，顺序不变。
    #[test]
    fn contract_body_is_rendered_in_full() {
        let mut ctx = sample_contract(0);
        let paragraphs: Vec<String> = (1..=40)
            .map(|index| format!("第{index}段：本段落用于验证长正文不截断。"))
            .collect();
        ctx.body = paragraphs.join("\n\n");
        ctx.has_body = true;
        let html = render_contract(&ctx).expect("渲染必须成功");
        for paragraph in &paragraphs {
            assert!(html.contains(paragraph), "正文段落丢失：{paragraph}");
        }
        // 正文容器必须保留换行（pre-wrap），否则多段会挤成一坨
        assert!(html.contains("white-space: pre-wrap"));
    }

    /// 没有正文 / 没有附件时走提示分支，而不是留一片空白。
    #[test]
    fn contract_without_body_or_attachments_shows_hint() {
        let mut ctx = sample_contract(0);
        ctx.body = "   ".to_string();
        ctx.has_body = false;
        let html = render_contract(&ctx).expect("渲染必须成功");
        assert!(html.contains(CONTRACT_BODY_EMPTY_HINT));
        assert!(html.contains(CONTRACT_ATTACHMENT_EMPTY_HINT));
        assert!(html.contains("附件清单（共 0 份）"));
        // 盖章位仍然要在（草稿合同也要能签字）
        assert!(html.contains("甲方（盖章）"));
    }

    /// 合同里的用户数据必须被转义（正文里写 `<script>` 不能执行）。
    #[test]
    fn contract_content_is_escaped() {
        let mut ctx = sample_contract(1);
        ctx.body = "<script>alert(1)</script>".to_string();
        ctx.contract_name = "<b>假合同</b>".to_string();
        ctx.attachments[0].file_name = "<img src=x onerror=alert(1)>.pdf".to_string();
        let html = render_contract(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<b>假合同</b>"));
        assert!(!html.contains("<img src=x onerror=alert(1)>.pdf"));
    }

    /// 期限：两端 / 只有起 / 只有止 / 都没有（不编日期）。
    #[test]
    fn term_range_cases() {
        assert_eq!(
            term_range_of(Some("2026-01-01"), Some("2026-12-31")),
            "2026-01-01 至 2026-12-31"
        );
        assert_eq!(term_range_of(Some("2026-01-01"), None), "2026-01-01 起");
        assert_eq!(term_range_of(None, Some("2026-12-31")), "至 2026-12-31");
        assert_eq!(term_range_of(None, None), "—");
        assert_eq!(term_range_of(Some("  "), Some("")), "—");
    }

    /// 付款方式：能从备注里识别就印识别值，识别不到就如实说明（**不编值**）。
    #[test]
    fn payment_terms_extraction() {
        assert_eq!(
            payment_terms_of(Some("第四条 付款方式：货到验收后 30 日内电汇付清\n第五条 …")),
            "货到验收后 30 日内电汇付清"
        );
        assert_eq!(payment_terms_of(Some("结算方式 - 月结 60 天")), "月结 60 天");
        assert_eq!(payment_terms_of(Some("1. 付款条件: 预付 30%")), "预付 30%");
        // 没有这个字段 / 只有标签没有值 → 兜底文案
        assert_eq!(
            payment_terms_of(Some("本合同自双方签字盖章之日起生效")),
            PAYMENT_TERMS_EMPTY
        );
        assert_eq!(payment_terms_of(Some("付款方式：")), PAYMENT_TERMS_EMPTY);
        assert_eq!(payment_terms_of(None), PAYMENT_TERMS_EMPTY);
    }

    /// 附件大小人类可读。
    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 128 + 512), "128.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(-1), "—");
    }

    /// 合同页脚：两个 Chromium 内建页码类 + 骑缝章提示 + 显式字号（默认 0 = 看不见）。
    #[test]
    fn contract_footer_has_pages_and_seal_hint() {
        let footer = contract_footer("HT-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("第 ") && footer.contains(" 页 / 共 "));
        assert!(footer.contains("骑缝章"), "页脚要有骑缝章提示位");
        assert!(footer.contains("font-size:8pt"));
        assert!(footer.contains("Noto Sans SC"));
        assert!(footer.contains("HT-20261108-000001"));
        // 单号里的尖括号不能破坏页脚 HTML
        let escaped = contract_footer("<b>HT</b>");
        assert!(!escaped.contains("<b>HT</b>"));
        assert!(escaped.contains("&lt;b&gt;HT&lt;/b&gt;"));
    }

    /// 合同渲染参数：A4 纵向 + 页脚开 + 下边距 ≥ 14mm。
    #[test]
    fn contract_pdf_opts_are_a4_with_footer() {
        let opts = contract_pdf_opts("HT-20261108-000001", Some("合同-HT-20261108-000001.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape);
        assert!(opts.display_header_footer);
        assert!(opts.footer_template.is_some());
        assert!(opts.margins.bottom_mm >= 14.0);
        assert!(!opts.prefer_css_page_size);
        assert_eq!(opts.filename.as_deref(), Some("合同-HT-20261108-000001.pdf"));
    }

    // ==================== 出入库单模板（PDF-03） ====================

    /// 入库单 HTML：仓库 / 库位 / 经办人 / 供应商 / 来源单据号 / 批次号 / 收货签字栏。
    #[test]
    fn renders_sample_stock_in_html() {
        let ctx = sample_stock(StockDocKind::StockIn, 3);
        let html = render_stock(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("收 货 入 库 单"));
        assert!(html.contains(COMPANY_NAME));
        for label in ["单号", "日期", "状态", "供应商", "仓库", "来源单据号", "经办人"] {
            assert!(html.contains(label), "单据头缺字段：{label}");
        }
        assert!(html.contains("PO-20261108-000004"), "来源单据号必须打印");
        // 明细列（含批次号 / 库位）
        for head in [
            "物料编码",
            "物料名称",
            "规格",
            "单位",
            "数量",
            "单价",
            "金额",
            "批次号",
            "库位",
        ] {
            assert!(html.contains(head), "明细表缺列：{head}");
        }
        assert!(html.contains("LOT20261101"), "批次号必须打印");
        assert!(html.contains("A-01-01"), "库位必须打印");
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            3,
            "3 行明细必须各出一个序号单元格"
        );
        // 合计四要素 + 大写
        for total in ["数量合计", "金额合计", "税额", "价税合计", "价税合计（大写）"] {
            assert!(html.contains(total), "合计行缺：{total}");
        }
        assert!(html.contains("人民币"));
        // 收货签字栏
        for cap in ["收货人：", "质检：", "仓管：", "供应商送货人：", "制单："] {
            assert!(html.contains(cap), "签字栏缺：{cap}");
        }
        // 序列号清单（一物一码）
        assert!(html.contains("序列号清单（一物一码）"));
        assert!(html.contains("SN-20261108-000001"));
        assert!(html.contains("display: table-header-group"), "跨页表头重复规则必须在");
        assert!(!html.contains("class=\"pageNumber\""));
    }

    /// 出库单 HTML：标题 / 来源单据号 / 客户 / 发货签字栏；**不出现税额行**（成本口径）。
    #[test]
    fn renders_sample_stock_out_html() {
        let ctx = sample_stock(StockDocKind::StockOut, 50);
        let html = render_stock(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("出 库 单"));
        assert!(!html.contains("收 货 入 库 单"));
        assert!(html.contains("客户"));
        assert!(html.contains("SH-20261108-000003"), "出库单必须打印来源单据号");
        assert!(html.contains("领用"), "出库类型必须打印");
        assert!(html.contains("成本结转合计"));
        assert!(html.contains("成本金额（大写）"));
        assert!(!html.contains("价税合计"), "出库单没有税额口径，不许出现价税合计");
        assert!(!html.contains("税额"), "出库单不许出现税额行");
        for cap in ["发货人：", "领用人／收货人：", "仓管：", "审核：", "制单："] {
            assert!(html.contains(cap), "发货签字栏缺：{cap}");
        }
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            50,
            "50 行明细必须全部渲染（跨页由浏览器分页，不是截断）"
        );
        // 出库单不登记序列号 → 整段不出现（按标题断言：模板内联 CSS 的注释里也有这个词）
        assert!(!html.contains("序列号清单（一物一码）"));
    }

    /// 明细里的用户数据必须被转义。
    #[test]
    fn stock_item_content_is_escaped() {
        let mut ctx = sample_stock(StockDocKind::StockIn, 1);
        ctx.items[0].material_name = "<script>alert(1)</script>".to_string();
        ctx.items[0].lot_no = "\"lot\"".to_string();
        let html = render_stock(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    /// 出入库单页脚与渲染参数。
    #[test]
    fn stock_footer_and_opts() {
        let footer = stock_footer(StockDocKind::StockOut, "OUT-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("出库单 OUT-20261108-000001"));
        assert!(footer.contains("font-size:8pt"));

        let opts = stock_pdf_opts(StockDocKind::StockIn, "IN-1", Some("收货入库单-IN-1.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(opts.display_header_footer);
        assert!(opts.margins.bottom_mm >= 14.0);
        assert_eq!(opts.filename.as_deref(), Some("收货入库单-IN-1.pdf"));
    }

    /// 单据种类 → 标题 / 表名 / 税额口径 / 签字栏 一一对应（拼错就等于打错单据）。
    #[test]
    fn stock_doc_kind_metadata() {
        assert_eq!(StockDocKind::StockIn.title(), "收 货 入 库 单");
        assert_eq!(StockDocKind::StockOut.title(), "出 库 单");
        assert_eq!(StockDocKind::StockIn.table(), "biz_stock_in");
        assert_eq!(StockDocKind::StockOut.table(), "biz_stock_out");
        assert_eq!(StockDocKind::StockIn.doc_kind_str(), "STOCK_IN");
        assert_eq!(StockDocKind::StockOut.doc_kind_str(), "STOCK_OUT");
        assert!(StockDocKind::StockIn.has_tax());
        assert!(!StockDocKind::StockOut.has_tax());
        assert_ne!(
            StockDocKind::StockIn.sign_caps(),
            StockDocKind::StockOut.sign_caps(),
            "收货 / 发货签字栏必须不同"
        );
    }

    // ==================== 调拨单模板（PDF-04） ====================

    /// 调拨单 HTML：调出 / 调入仓库（验收②）+ 双库位列 + 成本金额口径 + 两仓签字栏。
    #[test]
    fn renders_sample_transfer_html() {
        let ctx = sample_transfer(3);
        let html = render_transfer(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("调 拨 单"));
        assert!(html.contains(COMPANY_NAME));
        assert_eq!(ctx.doc_title, "调 拨 单");
        assert_eq!(ctx.doc_label, "调拨单");

        // 验收②：调出 / 调入仓库必须在单据头里各占一格（标签 + 值都打出来）
        for label in ["调出仓库", "调入仓库", "在途仓", "调拨日期"] {
            assert!(html.contains(label), "单据头缺字段：{label}");
        }
        assert!(html.contains("上海一号仓"), "调出仓库名必须打印");
        assert!(html.contains("上海二号仓"), "调入仓库名必须打印");
        // 路线条：调出 ➜ 调入（同一行里两个仓库名都要在）
        assert!(html.contains("调出：上海一号仓"));
        assert!(html.contains("调入：上海二号仓"));
        assert!(html.contains("在途经"));

        // 明细列（调出库位 / 调入库位 / 单位成本）
        for head in [
            "物料编码",
            "物料名称",
            "规格",
            "单位",
            "数量",
            "调出库位",
            "调入库位",
            "单位成本",
            "金额",
        ] {
            assert!(html.contains(head), "明细表缺列：{head}");
        }
        assert!(html.contains("A-01-01"), "调出库位必须打印");
        assert!(html.contains("B-01-01"), "调入库位必须打印");
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            3,
            "3 行明细必须各出一个序号单元格"
        );
        // 成本口径：没有税额三档，不许出现价税合计
        assert!(html.contains("成本金额合计"));
        assert!(html.contains("成本金额（大写）"));
        assert!(!html.contains("价税合计"), "调拨是原价转移，不该有价税合计");
        // 两仓签字栏
        for cap in ["调出仓管：", "调入仓管：", "经办人：", "审核：", "制单："] {
            assert!(html.contains(cap), "签字栏缺：{cap}");
        }
        assert!(html.contains("display: table-header-group"), "跨页表头重复规则必须在");
        assert!(!html.contains("class=\"pageNumber\""));
    }

    /// 在途仓为空（草稿 / 未发货）时路线条不显示「在途经」，也不许出现空白括号。
    #[test]
    fn transfer_route_hides_missing_transit_warehouse() {
        let mut ctx = sample_transfer(1);
        ctx.transit_warehouse = String::new();
        let html = render_transfer(&ctx).expect("模板渲染必须成功");
        assert!(html.contains("调出：上海一号仓"));
        assert!(!html.contains("在途经"), "没有在途仓就不许印「在途经」");
    }

    /// 调拨单页脚与渲染参数（A4 纵向）。
    #[test]
    fn transfer_footer_and_opts() {
        let footer = transfer_footer("TK-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("调拨单 TK-20261108-000001"));
        assert!(footer.contains("font-size:8pt"));

        let opts = transfer_pdf_opts("TK-1", Some("调拨单-TK-1.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape, "调拨单 10 列，纵向够用");
        assert!(opts.display_header_footer);
        assert!(opts.margins.bottom_mm >= 14.0);
        assert_eq!(opts.filename.as_deref(), Some("调拨单-TK-1.pdf"));
    }

    // ==================== 盘点单模板（PDF-04） ====================

    /// 验收①：盘点表必须有「实盘数量 / 差异 / 备注」**三列空白手写栏**。
    #[test]
    fn renders_sample_stock_take_with_blank_columns() {
        let ctx = sample_stock_take(4);
        let html = render_stock_take(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("盘 点 表"));
        assert_eq!(ctx.doc_label, "盘点单");
        assert!(html.contains("全盘"), "盘点类型必须打印");

        // 三列手写栏的表头必须在
        for head in ["实盘数量", "差异", "备注", "账面数量"] {
            assert!(html.contains(head), "明细表缺列：{head}");
        }
        // 每行三个空白单元格，内容恒为空（验收①的硬判据）
        assert_eq!(
            html.matches("<td class=\"w-actual blank\"></td>").count(),
            4,
            "4 行明细必须各留一个「实盘数量」空白格"
        );
        assert_eq!(
            html.matches("<td class=\"w-diff blank\"></td>").count(),
            4,
            "4 行明细必须各留一个「差异」空白格"
        );
        assert_eq!(
            html.matches("<td class=\"w-note blank\"></td>").count(),
            4,
            "4 行明细必须各留一个「备注」空白格"
        );
        // 空白格不许被任何数字填上（打印件就是拿去手写的）
        assert!(!html.contains("w-actual blank\">0"));
        assert!(!html.contains("w-diff blank\">0"));

        // 账面数量要打出来（现场比对用）
        assert_eq!(ctx.items[0].book_qty, "7");
        assert_eq!(
            html.matches("<td class=\"c-seq\">").count(),
            4,
            "4 行明细必须各出一个序号单元格"
        );
        // 填表说明 + 签字栏
        assert!(html.contains(STOCK_TAKE_BLANK_HINT));
        for cap in ["盘点人：", "复核人：", "仓管主管：", "财务：", "制单："] {
            assert!(html.contains(cap), "签字栏缺：{cap}");
        }
        // 没有差异时不印「盘盈 / 盘亏 0」（会被误读成已经盘过）
        assert!(!html.contains("盘盈数量合计"));
    }

    /// 已录实盘 / 已过账（有差异）时才印盘盈盘亏合计。
    #[test]
    fn stock_take_shows_diff_only_when_present() {
        let mut ctx = sample_stock_take(2);
        ctx.show_diff = false;
        let html = render_stock_take(&ctx).expect("渲染必须成功");
        assert!(!html.contains("盘盈数量合计"));

        let ctx = StockTakeContext::new(StockTakeContextArgs {
            doc_no: "ST-20261108-000002".to_string(),
            meta: vec![OrderMetaField::new("仓库", "上海一号仓")],
            items: vec![StockTakeItem {
                seq: 1,
                material_code: "MAT-00001".to_string(),
                material_name: "球阀".to_string(),
                spec: "DN50".to_string(),
                unit: "台".to_string(),
                location: "A-01-01".to_string(),
                book_qty: "100".to_string(),
            }],
            total_book_qty: Decimal::from(100),
            take_type: "PARTIAL".to_string(),
            gain_qty: Decimal::from(2),
            loss_qty: Decimal::from(10),
            diff_amount: Decimal::new(1234, 2),
            remark: String::new(),
            printed_at: "2026-11-08 19:30".to_string(),
        });
        assert!(ctx.show_diff);
        assert_eq!(ctx.take_type, "抽盘");
        let html = render_stock_take(&ctx).expect("渲染必须成功");
        assert!(html.contains("盘盈数量合计"));
        assert!(html.contains("盘亏数量合计"));
        assert!(html.contains("差异金额合计"));
        assert!(html.contains("12.34"));
    }

    /// 盘点单页脚与渲染参数（A4 纵向 —— 现场要夹在写字板上）。
    #[test]
    fn stock_take_footer_and_opts() {
        let footer = stock_take_footer("ST-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("盘点表 ST-20261108-000001"));

        let opts = stock_take_pdf_opts("ST-1", Some("盘点单-ST-1.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape);
        assert!(opts.margins.bottom_mm >= 14.0);
    }

    // ==================== 退货单模板（PDF-04） ====================

    /// 验收③（采购侧）：原单号 + 退货原因是显眼的一块；逐行价税合计列在。
    #[test]
    fn renders_sample_purchase_return_html() {
        let ctx = sample_return(ReturnDocKind::Purchase, 2);
        let html = render_return_doc(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("采 购 退 货 单"));
        assert!(!html.contains("销 售 退 货 单"));
        // 原单号：入库单号 + 采购订单号（验收③）
        assert!(html.contains("原单号（入库）"));
        assert!(html.contains("原单号（采购订单）"));
        assert!(html.contains("IN-20261108-000001"));
        assert!(html.contains("PO-20261108-000004"));
        // 退货原因横幅
        assert!(html.contains("退货原因："));
        assert!(html.contains("阀体密封面有砂眼"));
        // 供应商 / 仓库
        assert!(html.contains("供应商"));
        assert!(html.contains("北京某某机电设备有限公司"));
        assert!(html.contains("退货仓库"));
        // 明细：采购退货逐行有价税合计，没有序列号列（th 带列宽 class，table-layout: fixed 需要）
        assert!(html.contains("<th class=\"c-with-tax\">价税合计</th>"));
        assert!(!html.contains("class=\"c-sn\">序列号"));
        assert_eq!(html.matches("<td class=\"c-seq\">").count(), 2, "2 行明细必须全部渲染");
        for head in ["数量", "单价", "金额", "税率(%)", "税额", "库位"] {
            assert!(html.contains(head), "明细表缺列：{head}");
        }
        // 合计四要素 + 大写（大写标签写「价税合计大写」：加括号会在窄格子里折行）
        for label in ["数量合计", "金额合计", "税额", "价税合计大写"] {
            assert!(html.contains(label), "合计行缺：{label}");
        }
        assert!(html.contains("人民币"));
        for cap in ["退货经办：", "仓管：", "质检：", "供应商签收：", "制单："] {
            assert!(html.contains(cap), "签字栏缺：{cap}");
        }
        assert!(html.contains("display: table-header-group"));
    }

    /// 验收③（销售侧）：原销售订单 / 原发货单 + 退货类型；逐行没有价税合计，但有序列号列。
    #[test]
    fn renders_sample_sales_return_html() {
        let ctx = sample_return(ReturnDocKind::Sales, 3);
        let html = render_return_doc(&ctx).expect("模板渲染必须成功");

        assert!(html.contains("销 售 退 货 单"));
        assert!(html.contains("原单号（销售订单）"));
        assert!(html.contains("原单号（发货单）"));
        assert!(html.contains("SO-20261108-000002"));
        assert!(html.contains("SH-20261108-000003"));
        assert!(html.contains("退货原因："));
        assert!(html.contains("质量（QUALITY）"));
        assert!(html.contains("客户"));
        assert!(html.contains("上海某某贸易有限公司"));
        // 销售退货没有行级价税合计，但有序列号
        assert!(!html.contains("class=\"c-with-tax\">价税合计"));
        assert!(html.contains("<th class=\"c-sn\">序列号</th>"));
        assert!(html.contains("SN-20261108-000001"));
        assert_eq!(html.matches("<td class=\"c-seq\">").count(), 3);
        for cap in ["退货经办：", "仓管：", "质检：", "客户签收：", "制单："] {
            assert!(html.contains(cap), "签字栏缺：{cap}");
        }
    }

    /// 退货单里的用户数据必须被转义。
    #[test]
    fn return_content_is_escaped() {
        let mut ctx = sample_return(ReturnDocKind::Purchase, 1);
        ctx.reason = "<script>alert(1)</script>".to_string();
        ctx.items[0].material_name = "<b>假物料</b>".to_string();
        let html = render_return_doc(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<b>假物料</b>"));
    }

    /// 退货单页脚与渲染参数：**A4 横向**（12 列纵向装不下）。
    #[test]
    fn return_footer_and_opts_are_landscape() {
        let footer = return_footer(ReturnDocKind::Purchase, "PRT-20261108-000001");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("采购退货单 PRT-20261108-000001"));

        for kind in [ReturnDocKind::Purchase, ReturnDocKind::Sales] {
            let opts = return_pdf_opts(kind, "X-1", Some("退货单-X-1.pdf".to_string()));
            assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
            assert!(opts.landscape, "退货单 12 列必须横向");
            assert!(opts.display_header_footer);
            assert!(opts.margins.bottom_mm >= 14.0);
        }
    }

    /// 退货单种类 → 标题 / 表名 / 专有列 / 签字栏 一一对应。
    #[test]
    fn return_doc_kind_metadata() {
        assert_eq!(ReturnDocKind::Purchase.title(), "采 购 退 货 单");
        assert_eq!(ReturnDocKind::Sales.title(), "销 售 退 货 单");
        assert_eq!(ReturnDocKind::Purchase.label(), "采购退货单");
        assert_eq!(ReturnDocKind::Sales.label(), "销售退货单");
        assert_eq!(ReturnDocKind::Purchase.table(), "biz_purchase_return");
        assert_eq!(ReturnDocKind::Sales.table(), "biz_sales_return");
        assert_eq!(ReturnDocKind::Purchase.doc_kind_str(), "PURCHASE_RETURN");
        assert_eq!(ReturnDocKind::Sales.doc_kind_str(), "SALES_RETURN");
        assert!(ReturnDocKind::Purchase.has_line_with_tax());
        assert!(!ReturnDocKind::Sales.has_line_with_tax());
        assert!(!ReturnDocKind::Purchase.has_serials());
        assert!(ReturnDocKind::Sales.has_serials());
        assert_ne!(
            ReturnDocKind::Purchase.sign_caps(),
            ReturnDocKind::Sales.sign_caps(),
            "供应商签收 / 客户签收 必须分开"
        );
    }

    // ==================== 审批单模板（PDF-05） ====================

    /// 审批单模板必须注册进 tera 并能渲染（语法错在单测里就炸）。
    #[test]
    fn approval_template_registers_and_renders() {
        let ctx = sample_approval();
        let html = render_approval(&ctx).expect("审批单模板渲染必须成功");

        assert!(html.contains("审 批 单"));
        assert_eq!(ctx.doc_label, "审批单");
        assert!(html.contains(COMPANY_NAME));
        assert!(html.contains("FLW-20261108-00012345"));
        // 单据信息的六个必备字段（卡面「单据信息」段）
        for label in [
            "单据类型",
            "单号",
            "业务单号",
            "金额",
            "申请人",
            "申请时间",
            "流程",
            "状态",
        ] {
            assert!(html.contains(label), "单据头缺字段：{label}");
        }
        assert!(html.contains("12,345.67"), "金额必须打印（千分位）");
        assert!(html.contains("张三"), "申请人必须打印");
        assert!(html.contains("2026-11-08 09:00"), "申请时间必须打印");
        // 轨迹表头六列
        for head in ["级次", "节点", "审批人", "结果", "时间", "意见"] {
            assert!(html.contains(head), "轨迹表缺列：{head}");
        }
        // 4 条轨迹 = 4 个 trace-row；意见原文必须逐条打印
        assert_eq!(html.matches("class=\"trace-row\"").count(), 4);
        assert!(html.contains("金额与合同条款一致，同意"));
        assert!(html.contains("同意，按流程执行"));
        // 签字位：3 级各一行 + 签字栏留空（不得预印任何签名）
        assert!(html.contains("审批人签字"));
        assert!(html.contains("签名（留空手写）"));
        assert!(html.contains("由各级审批人在纸质件上现场手写签名"));
        assert_eq!(
            html.matches("<td class=\"blank\"></td>").count(),
            6,
            "3 级 × 签名+日期 两格"
        );
        // 页脚页码由 CDP footerTemplate 输出，HTML 里不该有占位符
        assert!(!html.contains("class=\"pageNumber\""));
        assert!(html.contains("display: table-header-group"));
    }

    /// ARCH-8 三种降级标记都必须能被打印出来（卡面「特别要求」）。
    #[test]
    fn approval_renders_arch8_downgrade_tags() {
        let html = render_approval(&sample_approval()).expect("渲染必须成功");
        assert!(html.contains("已跳过（自审）"), "自审跳过标记必须打印");
        assert!(
            html.contains("已升级至 总经理（原角色无人）"),
            "升级标记必须打印（含原角色无人的原因）"
        );
        assert!(html.contains("兜底审批人"), "兜底标记必须打印");
        // 标记行与轨迹行是**两种行**：塞进结果格会被 40pt 挤成三行
        assert_eq!(html.matches("class=\"tag-row").count(), 3);
        // 三条标记各自的配色类
        assert!(html.contains("tag-row is-skip"));
        assert!(html.contains("tag-row is-escalate"));
        assert!(html.contains("tag-row is-fallback"));
    }

    /// 驳回流程：结论横幅 + 轨迹结果 + 驳回意见三者都要出现。
    #[test]
    fn approval_renders_rejected_flow() {
        let mut ctx = sample_approval();
        ctx.status = "已驳回".to_string();
        ctx.status_kind = "is-reject".to_string();
        ctx.result_comment = "总经理驳回：单价高于年度框架协议".to_string();
        ctx.traces = vec![ApprovalTrace {
            level: "第 1 级".to_string(),
            round_note: String::new(),
            node_name: "部门经理审批".to_string(),
            approver: "王五".to_string(),
            result: "驳回".to_string(),
            cls: "is-reject".to_string(),
            at: "2026-11-08 09:20".to_string(),
            opinion: "单价比框架协议高 8%，先与供应商重谈。".to_string(),
            auto_note: String::new(),
            tag: String::new(),
            tag_kind: String::new(),
        }];
        let html = render_approval(&ctx).expect("渲染必须成功");
        assert!(html.contains("已驳回"));
        assert!(html.contains("verdict is-reject"));
        assert!(html.contains("总经理驳回：单价高于年度框架协议"));
        assert!(html.contains("单价比框架协议高 8%，先与供应商重谈。"));
        assert_eq!(html.matches("class=\"trace-row\"").count(), 1);
        assert_eq!(html.matches("class=\"tag-row").count(), 0, "无降级 → 无标记行");
    }

    /// 退回轮次要能看出来（同一节点第 2 轮 = 另一条轨迹），空轨迹给提示不印空表。
    #[test]
    fn approval_round_note_and_empty_trace() {
        let mut ctx = sample_approval();
        ctx.traces[1].round_note = "第 2 轮".to_string();
        let html = render_approval(&ctx).expect("渲染必须成功");
        assert!(html.contains("第 2 轮"));

        let mut empty = sample_approval();
        empty.traces.clear();
        empty.sign_slots.clear();
        empty.trace_count = format_int(0);
        empty.degree_count = format_int(0);
        let html = render_approval(&empty).expect("空轨迹也必须渲染成功");
        assert!(html.contains("暂无审批记录"));
        assert!(html.contains("本单没有审批节点，无需签字"));
        assert!(!html.contains("class=\"trace-row\""));
    }

    /// 审批意见 / 节点名都要转义（意见是自由文本，可能带 `<script>`）。
    #[test]
    fn approval_content_is_escaped() {
        let mut ctx = sample_approval();
        ctx.traces[0].opinion = "<script>alert(1)</script>".to_string();
        ctx.traces[0].node_name = "A&B 审批".to_string();
        let html = render_approval(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("A&amp;B 审批"));
    }

    /// 审批单页脚与渲染参数（A4 纵向）。
    #[test]
    fn approval_footer_and_opts() {
        let footer = approval_footer("FLW-20261108-00012345");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("审批单 FLW-20261108-00012345"));
        assert!(footer.contains("font-size:8pt"));

        let opts = approval_pdf_opts("FLW-1", Some("审批单-FLW-1.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape, "轨迹 6 列 + 意见列，纵向够用");
        assert!(opts.display_header_footer);
        assert!(opts.margins.bottom_mm >= 14.0);
        assert_eq!(opts.filename.as_deref(), Some("审批单-FLW-1.pdf"));
    }

    // ==================== 派工单模板（PDF-06） ====================

    /// 验收①②：任务信息齐全 + 检查项每条一个**空白**勾选框 + 签字栏 4 格。
    #[test]
    fn renders_sample_task_html() {
        let ctx = sample_task(3);
        let html = render_task(&ctx).expect("派工单模板渲染必须成功");

        // 页眉标题 + 任务标题（整行）
        assert!(html.contains("工 作 任 务 派 工 单"));
        assert!(html.contains("云枢 3 号泵站阀门到货验收与安装"));
        assert!(html.contains("任务号 TSK-202611-000123"));

        // 任务信息：卡面点名的 7 个字段一个不能少
        for label in ["任务号", "优先级", "截止日期", "负责人", "协作人", "所属项目", "状态"] {
            assert!(
                html.contains(&format!("<td class=\"label\">{label}</td>")),
                "任务信息缺字段：{label}"
            );
        }
        assert!(html.contains("P1（高）"), "优先级要带人读标签，不只印 P1");
        assert!(html.contains("王五、赵六"), "协作人多名用「、」连");
        assert!(html.contains("PRJ-2026-011 泵站改造"));

        // 描述原文（含空行与缩进，用 pre-wrap 保留）
        assert!(html.contains("逐台登记序列号"));
        assert!(html.contains("夜间施工需提前一天报安全科备案"));

        // 检查项：3 行，每行恰好一个空白方框；不出现任何「已勾」记号
        assert_eq!(
            html.matches("<td class=\"c-box\"><span class=\"cb\"></span></td>")
                .count(),
            3
        );
        assert!(!html.contains('☑'), "勾选框必须留空，不许预印已勾");
        assert!(!html.contains('☐'), "勾选框用 CSS 画框，不依赖字体里的 ☐ 字形");
        assert!(!html.contains("已完成"), "检查项不许印子任务状态");

        // 签字栏：4 格，标签 + 预印提示，签名区全空
        for label in ["派工人", "接收人", "完成确认", "日期"] {
            assert!(html.contains(&format!("<th>{label}")), "签字栏缺：{label}");
        }
        assert_eq!(html.matches("<td class=\"blank\"></td>").count(), 4);
        assert!(html.contains("验收人：李经理"));
    }

    /// 没有子任务 → 整个「检查项」节不出现（不印空表、不印 0 项）。
    #[test]
    fn task_without_checklist_hides_section() {
        let ctx = sample_task(0);
        assert!(!ctx.has_checklist);
        let html = render_task(&ctx).expect("渲染必须成功");
        assert!(!html.contains("class=\"check\""));
        assert!(
            !html.contains("检查项<span class=\"count\""),
            "没检查项就不该出现「检查项」小节标题"
        );
        // 签字栏与描述照旧
        assert!(html.contains("签字栏"));
        assert!(html.contains("逐台登记序列号"));
    }

    /// 描述为空 → 印占位说明（不印空白框，让人以为打印坏了）。
    #[test]
    fn task_without_description_shows_placeholder() {
        let mut ctx = sample_task(1);
        ctx.description = "   \n  ".to_string();
        let rebuilt = TaskContext::new(TaskContextArgs {
            doc_no: ctx.doc_no.clone(),
            subject: ctx.subject.clone(),
            status: ctx.status.clone(),
            status_kind: ctx.status_kind.clone(),
            meta: Vec::new(),
            description: ctx.description.clone(),
            checklist: Vec::new(),
            sign_cells: Vec::new(),
            printed_at: ctx.printed_at.clone(),
        });
        assert!(!rebuilt.has_description);
        let html = render_task(&rebuilt).expect("渲染必须成功");
        assert!(html.contains("本任务没有填写描述"));
        assert!(!html.contains("class=\"desc\""));
    }

    /// 标题 / 描述 / 检查项标题都是自由文本，必须转义。
    #[test]
    fn task_content_is_escaped() {
        let mut ctx = sample_task(1);
        ctx.subject = "<script>alert('x')</script>".to_string();
        ctx.description = "A&B <img src=x onerror=alert(1)>".to_string();
        ctx.checklist[0].title = "<b>不要加粗</b>".to_string();
        let html = render_task(&ctx).expect("渲染必须成功");
        assert!(!html.contains("<script>alert('x')</script>"));
        assert!(!html.contains("<img src=x"));
        assert!(!html.contains("<b>不要加粗</b>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("A&amp;B"));
    }

    /// 验收③的模板侧：长描述逐行都在，且换行原样保留（跨页交给 Chromium，
    /// 模板**刻意不加** break-inside: avoid，加了这个断言就该红）。
    #[test]
    fn task_long_description_is_verbatim() {
        let mut ctx = sample_task(0);
        let lines: Vec<String> = (1..=120).map(|i| format!("第 {i:03} 行：现场检查记录")).collect();
        ctx.description = lines.join("\n");
        let html = render_task(&ctx).expect("渲染必须成功");
        for line in &lines {
            assert!(html.contains(line), "长描述丢行：{line}");
        }
        // 原文的换行必须留给 CSS pre-wrap，而不是被模板折成一行
        assert!(html.contains("第 001 行：现场检查记录\n第 002 行"));
        let start = html.find(".desc {").expect("模板里必须有 .desc 样式");
        let end = start + html[start..].find('}').expect("样式块必须闭合");
        let desc_css = &html[start..end];
        assert!(
            !desc_css.contains("break-inside"),
            "描述块不许加 break-inside: avoid（长描述会整块被挤到下一页）：{desc_css}"
        );
        assert!(desc_css.contains("white-space: pre-wrap"), "换行要靠 pre-wrap 保留");
    }

    /// 派工单页脚与渲染参数（A4 纵向）。
    #[test]
    fn task_footer_and_opts() {
        let footer = task_footer("TSK-202611-000123");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("派工单 TSK-202611-000123"));
        assert!(footer.contains("font-size:8pt"));

        let opts = task_pdf_opts("TSK-1", Some("派工单-TSK-1.pdf".to_string()));
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape, "现场夹在写字板上，必须纵向");
        assert!(opts.display_header_footer);
        assert!(opts.margins.bottom_mm >= 14.0);
        assert_eq!(opts.filename.as_deref(), Some("派工单-TSK-1.pdf"));
    }

    /// 资产负债表模板：两个表头（资产 / 负债及所有者权益）、两列金额、
    /// 表尾三格签字、勾稽块都必须在渲染结果里（卡面表头 / 表尾 / 验收②）。
    #[test]
    fn balance_template_renders_paper_layout() {
        let ctx = sample_balance_report();
        let html = render_balance_report(&ctx).expect("资产负债表模板必须渲染成功");
        assert!(html.contains("资 产 负 债 表"), "页眉大标题");
        assert!(html.contains("编制单位："), "表头必须有编制单位");
        assert!(html.contains("单位：元"), "表头必须有金额单位");
        assert!(html.contains("2026年11月"), "表头必须有期间");
        assert!(html.contains("2026-11-30"), "表头必须有报表日期");
        // 左右两栏的表头（卡面：资产 | 负债及所有者权益）
        assert!(html.contains(">资产<"));
        assert!(html.contains("负债及所有者权益"));
        // 期末 / 期初两列各出现两次（左右各一）
        assert_eq!(html.matches(">期末余额<").count(), 2);
        assert_eq!(html.matches(">期初余额<").count(), 2);
        // 表尾三格
        for label in ["制表人：", "审核人：", "日期："] {
            assert!(html.contains(label), "表尾缺「{label}」");
        }
        // 勾稽校验块
        assert!(html.contains("勾稽校验"));
        assert!(html.contains("is-ok"));
        // 六列 = 6 个 <col>，且金额用等宽数字
        assert_eq!(html.matches("<col style=\"width:").count(), 6);
        assert!(html.contains("tabular-nums"));
        assert!(html.contains("display: table-header-group"), "换页要重复表头");
    }

    /// 利润表 / 现金流量表模板：项目 / 本期金额 / 本年累计 三列 + 合计行加粗。
    #[test]
    fn amount_template_renders_three_columns() {
        for kind in [AmountReportKind::Income, AmountReportKind::CashFlow] {
            let ctx = sample_amount_report(kind);
            let title = ctx.head.doc_title.clone();
            let html = render_amount_report(&ctx).expect("财务报表模板必须渲染成功");
            assert!(html.contains(&title), "页眉大标题丢了：{title}");
            assert!(html.contains(">项目<"), "第一列必须是「项目」");
            assert!(html.contains(">本期金额<"));
            assert!(html.contains(">本年累计<"));
            assert!(html.contains("is-total"), "合计行要有样式类（加粗）");
            assert!(html.contains(">制表人：<") || html.contains("制表人："));
            assert_eq!(html.matches("<col style=\"width:").count(), 3);
            // `note` 行必须跨三列（不印金额）
            assert!(html.contains("colspan=\"3\"") || !ctx.rows.iter().any(|row| row.kind == "note"));
        }
    }

    /// 财务报表页脚与渲染参数：A4 纵向、左右各 8mm（资产负债表 6 列要排下）、
    /// 页脚带页码占位符与报表号。
    #[test]
    fn report_footer_and_opts() {
        let footer = report_footer("资产负债表", "BALANCE_SHEET-2026-11");
        assert!(footer.contains("class=\"pageNumber\""));
        assert!(footer.contains("class=\"totalPages\""));
        assert!(footer.contains("BALANCE_SHEET-2026-11"));
        let opts = report_pdf_opts(
            "资产负债表",
            "BALANCE_SHEET-2026-11",
            Some("资产负债表-BALANCE_SHEET-2026-11.pdf".to_string()),
        );
        assert_eq!(opts.page, crate::business::pdf::dto::PageSize::A4);
        assert!(!opts.landscape, "财务报表与会计凭证同尺寸装订：A4 纵向");
        assert!(opts.display_header_footer);
        assert_eq!(opts.margins.left_mm, 8.0, "6 列要在 210mm 里排下");
        assert_eq!(opts.margins.right_mm, 8.0);
        assert!(opts.margins.bottom_mm >= 14.0, "页脚要有打印区");
        assert_eq!(opts.filename.as_deref(), Some("资产负债表-BALANCE_SHEET-2026-11.pdf"));
    }
}
