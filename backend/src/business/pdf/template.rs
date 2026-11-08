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

use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

use crate::business::pdf::dto::PdfError;

/// 内嵌的对账单模板（打印样式与分页约束都在里面，见文件头注释）。
pub const STATEMENT_TEMPLATE: &str = include_str!("templates/statement.html.tera");

/// 内嵌模板在 tera 里的注册名（`.html` 后缀 → 启用 HTML 自动转义）。
pub const STATEMENT_NAME: &str = "statement.html";

/// 进程级 tera 实例（模板编译一次）。
fn tera() -> &'static Tera {
    static TERA: OnceLock<Tera> = OnceLock::new();
    TERA.get_or_init(|| {
        let mut tera = Tera::default();
        // 注册失败只可能是模板语法错——那是编译期就能发现的常量字符串，
        // 这里 expect 是合理的（跑不起来比出一份错 PDF 好）。
        tera.add_raw_template(STATEMENT_NAME, STATEMENT_TEMPLATE)
            .expect("内置单据模板语法错误（编译期常量，改模板后必须跑 cargo test）");
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
}
