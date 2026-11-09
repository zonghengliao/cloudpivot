//! 财务报表 **PDF 导出**（PDF-07）：资产负债表 / 利润表 / 现金流量表。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/{type}/pdf?period=yyyy-MM` | `business:erp:report:export` | 报表 PDF（`type` = `balance-sheet` / `income-statement` / `cash-flow`） |
//!
//! # 为什么复用 `business:erp:report:export` 而不是新种权限点
//!
//! 三张报表的 **xlsx 导出**早就挂在 `business:erp:report:export` 上（V0183 种子
//! `…1016`，V0184 / V0185 明确写着「导出按钮复用 R1 的码」）。PDF 与 xlsx 是
//! **同一份数据的两种纸面**，不是两种能力：另立 `:pdf` 码只会造出「能导 Excel、
//! 点 PDF 就 403」的哑弹，还要同步迁移 + 角色矩阵 + 前端按钮显隐三处。
//! 本模块**只读**，因此不加任何新权限码、不加任何迁移。
//!
//! # 口径：不重算，只重排
//!
//! 三张报表的金额、勾稽校验、未归类明细全部由既有模块算出：
//! [`crate::business::erp::report`]（R1）/ [`crate::business::erp::income_statement`]（R2）/
//! [`crate::business::erp::cash_flow`]（R3）。本模块**一行取数 SQL 都没有**，
//! 只做两件事：
//!
//! 1. 调各自的 `snapshot_or_error`（REPEATABLE READ 快照 + 同一套错误码 66010/66011/66012）
//!    → `into_response` 拿结构体；
//! 2. 把结构体**重排**成纸面报表的行（左右两栏 / 三列表），金额按
//!    [`money_or_dash`] 格式化后交给模板。
//!
//! 这样做的收益是可证的：**PDF 上的数与页面上、Excel 里的数是同一批 Decimal**，
//! 不存在「页面对、打印件差一分」的可能（对比：重写一遍取数逻辑必然两套口径）。
//!
//! # 卡面四条验收的落点
//!
//! | 卡面 | 落点 |
//! |------|------|
//! | ① 三张报表各导一份 PDF | 一个路由 + [`ReportKind`] 三分支，**每张报表的期初/本期/累计都取自各自模块** |
//! | ② 借贷平衡（资产负债表左右相等） | 两侧总计行**对齐到同一行**（见 [`align_sides`]）+ 表下勾稽块逐条打印两侧总计与差额 |
//! | ③ 千分位 + 两位小数 | [`money_or_dash`]（先 `round_dp(2)` 再定点打印，绝不截断） |
//! | ④ 空白项显示「-」 | [`money_or_dash`]：`None` 与**零**都印 `-`（中国会计报表惯例：零不印 0.00） |
//!
//! # 期初 / 期末的语义（**别按字面理解**）
//!
//! 资产负债表两列是 `期末余额` 与 `期初余额`：
//!
//! ```text
//! 期末余额 = 期间结束日的科目余额（R1 的 amount）
//! 期初余额 = 上期期末余额（R1 的 previous_amount；无上期期间时为 None → "-"）
//! ```
//!
//! 「期初」取的是**上期期末**，这是会计上的等价定义（上期末结转即本期初），
//! 也是 R1 `attach_previous` 已经在算的东西 —— 本模块不再单独查询期初余额，
//! 否则两处口径迟早漂开。**无上期期间时印 `-` 并在附注里明说**，绝不假装是 0
//! （见 [`balance_notes`]）。
//!
//! # 审计
//!
//! 导出是 GET，`middleware::operation_log` 只记写方法。渲染成功后补一行
//! `EXPORT_REPORT_PDF`（`target_type = biz_report`，`detail.source = PDF-07`）。
//! 报表不是单据、没有 `biz_*` 主键，所以 `target_id` 存本次导出事件的 v7 id，
//! 业务定位靠 `detail.report_type` + `detail.period` + `doc_no`。
//! 写日志失败只记 error，不改已经生成的响应。

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::doc_pdf_model;
use crate::business::erp::doc_pdf_render::{self, RenderedDoc};
use crate::business::erp::income_statement;
use crate::business::erp::{cash_flow, report};
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, AmountReportContext, AmountReportRow, BalanceReportContext, BalanceReportRow, ReportCell, ReportCheckBlock,
    ReportHead,
};
use crate::shared::error_code;

/// 路由权限码（与三张报表的 xlsx 导出同一个动作码，见模块注释）。
pub const PERMISSION: &str = "business:erp:report:export";

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_REPORT_PDF";

/// `operation_logs.target_type`（报表族：三张报表都是它的成员）。
pub const TARGET_TYPE: &str = "biz_report";

/// 审计 `detail.source`（追卡片用）。
pub const SOURCE: &str = "PDF-07";

/// 报表类别（路由上的 `{type}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    /// 资产负债表（`balance-sheet`）
    BalanceSheet,
    /// 利润表（`income-statement`）
    IncomeStatement,
    /// 现金流量表（`cash-flow`）
    CashFlow,
}

impl ReportKind {
    /// 路由段 → 报表类别。
    ///
    /// 只认卡面写死的三个值，外加下划线写法（`balance_sheet`）做兼容 ——
    /// 大小写不敏感，因为 URL 大小写不该决定能不能导出。**不认识就返回 `None`**：
    /// 调用方必须报 400，绝不"猜一个最像的"（猜错等于给用户一份别的报表）。
    pub fn parse(raw: &str) -> Option<Self> {
        let key = raw.trim().to_ascii_lowercase().replace('_', "-");
        match key.as_str() {
            "balance-sheet" => Some(Self::BalanceSheet),
            "income-statement" => Some(Self::IncomeStatement),
            "cash-flow" => Some(Self::CashFlow),
            _ => None,
        }
    }

    /// 路由段（规范写法，回给前端 / 写进审计）。
    pub fn slug(self) -> &'static str {
        match self {
            Self::BalanceSheet => "balance-sheet",
            Self::IncomeStatement => "income-statement",
            Self::CashFlow => "cash-flow",
        }
    }

    /// 报表编码（与 `biz_report_item.report_code` 同值）。
    pub fn code(self) -> &'static str {
        match self {
            Self::BalanceSheet => "BALANCE_SHEET",
            Self::IncomeStatement => "INCOME_STATEMENT",
            Self::CashFlow => "CASH_FLOW",
        }
    }

    /// 报表中文名（页眉标题 / 下载文件名）。
    pub fn label(self) -> &'static str {
        match self {
            Self::BalanceSheet => "资产负债表",
            Self::IncomeStatement => "利润表",
            Self::CashFlow => "现金流量表",
        }
    }

    /// 页眉大标题（字间留空，与纸面报表一致）。
    pub fn headline(self) -> &'static str {
        match self {
            Self::BalanceSheet => "资 产 负 债 表",
            Self::IncomeStatement => "利 润 表",
            Self::CashFlow => "现 金 流 量 表",
        }
    }
}

/// 全部合法 `type` 的人话清单（400 的 `msg` 里用，省得调用方去翻文档）。
pub const KIND_HINT: &str = "balance-sheet / income-statement / cash-flow";

/// 查询参数：`period` 必填（`yyyy-MM`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ReportPdfQuery {
    /// 会计期间（`2026-11`）
    pub period: Option<String>,
}

// ==================== 金额格式化 ====================

/// 报表金额：**空白项与零都印 `-`**，其余千分位 + 两位小数。
///
/// # 为什么零也印 `-`（卡面验收④）
///
/// 中国会计报表的纸面惯例就是「零不印 0.00，印一横」：一列几十行里
/// 只有三五个 0.00 会把真正的金额淹掉。这条规则同时覆盖了「无数据」（`None`）
/// 和「算出来就是 0」，所以模板层不需要再判断一次。
///
/// # 为什么先 `round_dp(2)` 再打印
///
/// `format!("{:.2}", Decimal)` 是**截断**（12.3456 → 12.34），打印件少一分钱
/// 是事故。与 [`report::money_text`] 同一口径（同一个 `round_dp` + 同一个千分位拼装），
/// 保证 PDF / Excel / 页面三处金额写法完全一致。
pub fn money_or_dash(value: Option<Decimal>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let rounded = value.round_dp(2);
    if rounded.is_zero() {
        return "-".to_string();
    }
    report::money_text(Some(rounded))
}

/// 校验块里的金额：**零照印 `0.00`**（差额是不是 0 必须一眼看见）。
pub fn money_plain(value: Decimal) -> String {
    report::money_text(Some(value.round_dp(2)))
}

/// 金额是否为零（`None` 视为零）。
///
/// 用于「这一节要不要印」的判断（现金流量表的「其他」节）。
pub fn is_blank(value: Option<Decimal>) -> bool {
    value.map(|value| value.round_dp(2).is_zero()).unwrap_or(true)
}

/// 期间状态的中文（`DERIVED` 是报表侧独有的"未开账·按自然月推算"）。
pub fn period_status_label(status: &str) -> &'static str {
    match status {
        "CLOSED" => "已结账",
        "DERIVED" => "未开账·按自然月推算",
        "OPEN" => "未结账",
        _ => "状态未知",
    }
}

// ==================== 资产负债表：两栏对齐 ====================

/// 报表行 → 纸面单元格（项目名 + 期末 + 期初）。
pub fn cell_of(line: &report::ReportLineRes) -> ReportCell {
    let mut cell = ReportCell::new(
        report::display_name(line),
        money_or_dash(line.amount),
        money_or_dash(line.previous_amount),
    );
    if line.level == 1 {
        cell = cell.with_cls("is-major");
    }
    if line.level >= 3 {
        cell = cell.with_cls("is-detail");
    }
    if line.is_deduction {
        cell = cell.with_cls("is-deduction");
    }
    if line.is_total {
        cell = cell.with_cls("is-total");
    }
    cell
}

/// 两栏按行号对齐，并让**两侧的总计行落在同一行**（卡面验收②的版面前提）。
///
/// # 规则
///
/// 资产侧（21 行）通常比负债及权益侧（16 行）长。若简单按下标 zip，
/// `资产总计` 会落在第 21 行、`负债和所有者权益总计` 落在第 16 行 —— 纸面上
/// 两个总计一高一低，看起来就像「右边少了 5 行没算」。所以：
///
/// * 短的一栏在**自己的最后一行（总计行）之前**补空行，把总计行顶到与对面同一行；
/// * 任一侧为空时不做补位（不做「凭空造一栏」这种事）。
///
/// 空行整格透明（`is-blank`），不印边框里的假项目名。
pub fn align_sides(left: &[ReportCell], right: &[ReportCell]) -> Vec<BalanceReportRow> {
    let mut left_cells = left.to_vec();
    let mut right_cells = right.to_vec();
    let diff = left_cells.len() as isize - right_cells.len() as isize;
    if diff > 0 && !right_cells.is_empty() {
        let at = right_cells.len() - 1;
        for _ in 0..diff {
            right_cells.insert(at, ReportCell::blank());
        }
    } else if diff < 0 && !left_cells.is_empty() {
        let at = left_cells.len() - 1;
        for _ in 0..-diff {
            left_cells.insert(at, ReportCell::blank());
        }
    }
    let total = left_cells.len().max(right_cells.len());
    let mut rows = Vec::with_capacity(total);
    for index in 0..total {
        let left = left_cells.get(index).cloned().unwrap_or_else(ReportCell::blank);
        let right = right_cells.get(index).cloned().unwrap_or_else(ReportCell::blank);
        let emphatic = left.cls.contains("is-total") || right.cls.contains("is-total");
        rows.push(BalanceReportRow {
            left,
            right,
            cls: if emphatic {
                "is-total".to_string()
            } else {
                String::new()
            },
        });
    }
    rows
}

/// 资产负债表附注：无上期数据 / 期间是推算出来的，都要**写在纸面上**。
pub fn balance_notes(data: &report::BalanceSheetRes) -> Vec<String> {
    let mut notes = data.notes.clone();
    if data.previous_period.is_none() {
        notes.push(
            "本表无上期期间数据，「期初余额」列印「-」表示未取到上期期末余额，**不是 0**；\
             需要期初对比时请先在「会计期间」里开账并结账上月"
                .to_string(),
        );
    }
    if data.period_status == report::PERIOD_DERIVED {
        notes.push(format!(
            "会计期间 {} 未在「会计期间」里建立，本表按自然月（{} ~ {}）推算编制",
            data.period, data.period_start, data.period_end
        ));
    }
    for hint in data.check.hints.iter().take(3) {
        notes.push(hint.clone());
    }
    notes
}

/// 资产负债表上下文（纯逻辑：结构体进，渲染上下文出）。
pub fn build_balance_context(report: &report::BalanceSheetRes, printed_at: String) -> BalanceReportContext {
    let left: Vec<ReportCell> = report.assets.iter().map(cell_of).collect();
    let right: Vec<ReportCell> = report
        .liabilities
        .iter()
        .chain(report.equity.iter())
        .map(cell_of)
        .collect();
    let rows = align_sides(&left, &right);
    let check = report.check.clone();
    let mut lines = vec![
        format!("资产总计（期末）      {}", money_plain(check.assets_total)),
        format!(
            "负债和所有者权益总计（期末）  {}",
            money_plain(check.liability_equity_total)
        ),
    ];
    if check.balanced {
        lines.push(format!(
            "差额 {} —— 借贷平衡（资产 ≡ 负债 + 所有者权益）",
            money_plain(check.diff)
        ));
    } else {
        lines.push(format!(
            "差额 {} —— **不平**：资产合计与负债+权益合计不相等，本表不得作为正式报表出具",
            money_plain(check.diff)
        ));
    }
    let notes = balance_notes(report);
    let has_notes = !notes.is_empty();
    BalanceReportContext {
        head: ReportHead {
            company: report.company_name.clone(),
            doc_title: ReportKind::BalanceSheet.headline().to_string(),
            unit_label: format!("单位：{}", report.unit),
            preparer: report.company_name.clone(),
            period_label: report.period_name.clone(),
            date_label: report.period_end.to_string(),
            status_label: period_status_label(&report.period_status).to_string(),
        },
        doc_label: report.report_name.clone(),
        doc_no: format!("{}-{}", report.report_code, report.period),
        end_label: "期末余额".to_string(),
        begin_label: "期初余额".to_string(),
        rows,
        check: ReportCheckBlock {
            title: "勾稽校验：资产总计 ≡ 负债和所有者权益总计".to_string(),
            lines,
            cls: if check.balanced { "is-ok" } else { "is-bad" }.to_string(),
        },
        notes,
        has_notes,
        printed_at,
        engine: "Chromium · CDP printToPDF".to_string(),
    }
}

/// 资产负债表（`GET /api/business/erp/report/balance-sheet/pdf`）。
pub async fn export_balance_sheet(period: Option<&str>) -> std::result::Result<RenderedDoc, ApiError> {
    let period = report::validate_period(period)?;
    let snapshot = report::snapshot_or_error(&period).await?;
    let company = report::company_name().await;
    let data = report::into_response(report::REPORT_CODE, report::REPORT_NAME, &company, "元", snapshot);
    let ctx = build_balance_context(&data, doc_pdf_model::printed_at_now());
    let html = template::render_balance_report(&ctx).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&ctx.doc_label, &ctx.doc_no);
    let opts = template::report_pdf_opts(&ctx.doc_label, &ctx.doc_no, Some(filename));
    doc_pdf_render::render_document(&html, &ctx.doc_no, &opts, ctx.rows.len(), data.check.assets_total)
        .await
        .map_err(|error| error.into_api())
}

// ==================== 利润表 / 现金流量表：三列表 ====================

/// 利润表上下文。
pub fn build_income_context(report: &income_statement::IncomeStatementRes, printed_at: String) -> AmountReportContext {
    let rows: Vec<AmountReportRow> = report
        .items
        .iter()
        .map(|line| {
            let kind = if line.is_total { "total" } else { "line" };
            AmountReportRow::styled(
                line.item_code.clone(),
                line.item_name.clone(),
                kind,
                line.level.max(1) as u8,
            )
            .with_amounts(money_or_dash(Some(line.current)), money_or_dash(Some(line.ytd)))
        })
        .collect();
    let check = &report.check;
    let mut lines = vec![
        format!("净利润（本年累计）          {}", money_plain(check.net_profit_ytd)),
        format!(
            "未分配利润本年变动          {}（期末 {} − 年初 {}）",
            money_plain(check.equity_change),
            money_plain(check.undistributed_profit_end),
            money_plain(check.undistributed_profit_year_start)
        ),
    ];
    lines.push(if check.balanced {
        format!("差额 {} —— 两表勾稽一致", money_plain(check.diff))
    } else {
        format!(
            "差额 {} —— 与资产负债表「未分配利润」本年变动不符，需查明未归类损益",
            money_plain(check.diff)
        )
    });
    let mut notes: Vec<String> = check.hints.to_vec();
    if !check.unmapped.is_empty() {
        let names: Vec<String> = check
            .unmapped
            .iter()
            .map(|row| format!("{} {}", row.account_code, row.account_name))
            .collect();
        notes.push(format!(
            "未归类损益科目 {} 个：{}",
            check.unmapped.len(),
            names.join("、")
        ));
    }
    if report.period_status == "DERIVED" {
        notes.push(format!(
            "会计期间 {} 未在「会计期间」里建立，本表按自然月推算编制",
            report.period
        ));
    }
    AmountReportContext {
        head: ReportHead {
            company: report.company_name.clone(),
            doc_title: ReportKind::IncomeStatement.headline().to_string(),
            unit_label: format!("单位：{}", report.unit),
            preparer: report.company_name.clone(),
            period_label: report.period_name.clone(),
            date_label: report.period_end.to_string(),
            status_label: period_status_label(&report.period_status).to_string(),
        },
        doc_label: report.report_name.clone(),
        doc_no: format!("{}-{}", report.report_code, report.period),
        current_label: report.current_label.clone(),
        ytd_label: report.ytd_label.clone(),
        rows,
        check: ReportCheckBlock {
            title: "勾稽校验：净利润（本年累计）≡ 资产负债表「未分配利润」本年变动".to_string(),
            lines,
            cls: if check.balanced { "is-ok" } else { "is-bad" }.to_string(),
        },
        has_notes: !notes.is_empty(),
        notes,
        printed_at,
        engine: "Chromium · CDP printToPDF".to_string(),
    }
}

/// 现金流量表的一节（经营 / 投资 / 筹资 / 其他）：流入明细 → 流入小计 →
/// 流出明细 → 流出小计 → 本节净额。
fn push_cash_section(rows: &mut Vec<AmountReportRow>, section: &cash_flow::ActivitySectionRes) {
    rows.push(
        AmountReportRow::styled(
            format!("SECTION_{}", section.activity),
            section.activity_name.clone(),
            "section",
            1,
        )
        .with_amounts(
            money_or_dash(Some(section.net.current)),
            money_or_dash(Some(section.net.ytd)),
        ),
    );
    for (label, lines, total) in [
        ("INFLOW_TOTAL", &section.inflow, &section.inflow_total),
        ("OUTFLOW_TOTAL", &section.outflow, &section.outflow_total),
    ] {
        for line in lines.iter() {
            rows.push(
                AmountReportRow::styled(line.item_code.clone(), line.item_name.clone(), "line", 3)
                    .with_amounts(money_or_dash(Some(line.current)), money_or_dash(Some(line.ytd))),
            );
        }
        let caption = if label == "INFLOW_TOTAL" {
            "流入小计"
        } else {
            "流出小计"
        };
        rows.push(
            AmountReportRow::styled(
                format!("{}_{label}", section.activity),
                format!("  {caption}"),
                "subtotal",
                2,
            )
            .with_amounts(money_or_dash(Some(total.current)), money_or_dash(Some(total.ytd))),
        );
    }
    rows.push(
        AmountReportRow::styled(
            format!("{}_NET", section.activity),
            format!("  {}净额", cash_flow::activity_label(&section.activity)),
            "total",
            2,
        )
        .with_amounts(
            money_or_dash(Some(section.net.current)),
            money_or_dash(Some(section.net.ytd)),
        ),
    );
}

/// 「其他（未分类）」节是否值得印：有明细行，或净额不为零。
pub fn other_section_visible(section: &cash_flow::ActivitySectionRes) -> bool {
    !section.inflow.is_empty()
        || !section.outflow.is_empty()
        || !is_blank(Some(section.net.current))
        || !is_blank(Some(section.net.ytd))
}

/// 现金流量表上下文。
pub fn build_cash_flow_context(report: &cash_flow::CashFlowRes, printed_at: String) -> AmountReportContext {
    let mut rows: Vec<AmountReportRow> = Vec::new();
    for section in [&report.operating, &report.investing, &report.financing] {
        push_cash_section(&mut rows, section);
    }
    // 「其他」是未分类兜底（+ 分录不平差额）。全零且没有明细时整节不印 ——
    // 印一屏「-」只会让看报表的人以为漏了数据；真要提示，`warnings` 会在附注里点名。
    if other_section_visible(&report.other) {
        push_cash_section(&mut rows, &report.other);
    }
    for (code, label, pair, kind) in [
        (
            "NET_INCREASE",
            "现金及现金等价物净增加额",
            &report.net_increase,
            "total",
        ),
        (
            "OPENING_BALANCE",
            "加：期初现金及现金等价物余额",
            &report.opening_balance,
            "line",
        ),
        (
            "CLOSING_BALANCE",
            "期末现金及现金等价物余额",
            &report.closing_balance,
            "total",
        ),
    ] {
        rows.push(
            AmountReportRow::styled(code, label, kind, 1)
                .with_amounts(money_or_dash(Some(pair.current)), money_or_dash(Some(pair.ytd))),
        );
    }
    let check = &report.check;
    let mut lines = vec![
        format!("期末现金及现金等价物余额      {}", money_plain(check.closing_balance)),
        format!(
            "资产负债表「货币资金」        {}",
            money_plain(check.balance_sheet_cash)
        ),
    ];
    lines.push(if check.balanced {
        format!("差额 {} —— 与资产负债表货币资金一致", money_plain(check.diff))
    } else {
        format!(
            "差额 {} —— 与资产负债表货币资金不符，请核对现金类科目（1001 / 1002）",
            money_plain(check.diff)
        )
    });
    let mut notes: Vec<String> = report.warnings.clone();
    notes.push(report.simplified_note.clone());
    for hint in check.hints.iter().take(3) {
        if !notes.contains(hint) {
            notes.push(hint.clone());
        }
    }
    AmountReportContext {
        head: ReportHead {
            company: report.company_name.clone(),
            doc_title: ReportKind::CashFlow.headline().to_string(),
            unit_label: format!("单位：{}", report.unit),
            preparer: report.company_name.clone(),
            period_label: report.period_name.clone(),
            date_label: report.period_end.to_string(),
            status_label: period_status_label(&report.period_status).to_string(),
        },
        doc_label: report.report_name.clone(),
        doc_no: format!("{}-{}", report.report_code, report.period),
        current_label: report.current_label.clone(),
        ytd_label: report.ytd_label.clone(),
        has_notes: !notes.is_empty(),
        rows,
        check: ReportCheckBlock {
            title: "勾稽校验：期末现金余额 ≡ 资产负债表「货币资金」".to_string(),
            lines,
            cls: if check.balanced { "is-ok" } else { "is-bad" }.to_string(),
        },
        notes,
        printed_at,
        engine: "Chromium · CDP printToPDF".to_string(),
    }
}

/// 利润表（`GET /api/business/erp/report/income-statement/pdf`）。
pub async fn export_income_statement(period: Option<&str>) -> std::result::Result<RenderedDoc, ApiError> {
    let period = report::validate_period(period)?;
    let snapshot = income_statement::snapshot_or_error(&period).await?;
    let company = income_statement::company_name().await;
    let data = income_statement::into_response(&company, "元", snapshot);
    let ctx = build_income_context(&data, doc_pdf_model::printed_at_now());
    let html = template::render_amount_report(&ctx).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&ctx.doc_label, &ctx.doc_no);
    let opts = template::report_pdf_opts(&ctx.doc_label, &ctx.doc_no, Some(filename));
    doc_pdf_render::render_document(&html, &ctx.doc_no, &opts, ctx.rows.len(), data.check.net_profit_ytd)
        .await
        .map_err(|error| error.into_api())
}

/// 现金流量表（`GET /api/business/erp/report/cash-flow/pdf`）。
pub async fn export_cash_flow(period: Option<&str>) -> std::result::Result<RenderedDoc, ApiError> {
    let period = report::validate_period(period)?;
    let snapshot = cash_flow::snapshot_or_error(&period).await?;
    let company = cash_flow::company_name().await;
    let data = cash_flow::into_response(&company, "元", snapshot);
    let ctx = build_cash_flow_context(&data, doc_pdf_model::printed_at_now());
    let html = template::render_amount_report(&ctx).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&ctx.doc_label, &ctx.doc_no);
    let opts = template::report_pdf_opts(&ctx.doc_label, &ctx.doc_no, Some(filename));
    doc_pdf_render::render_document(&html, &ctx.doc_no, &opts, ctx.rows.len(), data.closing_balance.current)
        .await
        .map_err(|error| error.into_api())
}

/// 按类别导出（HTTP 层与单测共用的唯一入口）。
pub async fn export_report(kind: ReportKind, period: Option<&str>) -> std::result::Result<RenderedDoc, ApiError> {
    match kind {
        ReportKind::BalanceSheet => export_balance_sheet(period).await,
        ReportKind::IncomeStatement => export_income_statement(period).await,
        ReportKind::CashFlow => export_cash_flow(period).await,
    }
}

// ==================== HTTP 入口 ====================

/// 财务报表 PDF（`GET /api/business/erp/report/{type}/pdf?period=yyyy-MM`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`error_code::PERMISSION_DENIED`]：缺 `business:erp:report:export`（中间件）；
/// - 400 [`error_code::PARAM_INVALID`]：`type` 不在 [`KIND_HINT`] 里 / `period` 缺失或不是 `yyyy-MM`；
/// - 400 [`error_code::ERP_PERIOD_NOT_FOUND`]（66010）：该期间没有账也没有开账（与页面同一错误）；
/// - 400 [`error_code::ERP_OPENING_UNBALANCED`]（66012）：期初不平衡（与页面同一错误）；
/// - 500 [`error_code::ERP_REPORT_CONFIG_INVALID`]（66011）：`biz_report_item` 配置坏了；
/// - 422 / 503 / 504：渲染引擎（模板 / Chromium 不可用 / 超时）。
#[nik::permission(
    path = "/business/erp/report/{type}/pdf",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/report/{type}/pdf")]
pub async fn report_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(kind_raw): Path<String>,
    Query(query): Query<ReportPdfQuery>,
) -> Response {
    let started = Instant::now();
    let Some(kind) = ReportKind::parse(&kind_raw) else {
        return ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "报表类型「{kind_raw}」不支持，合法值：{KIND_HINT}（例：/api/business/erp/report/balance-sheet/pdf?period=2026-11）"
            ),
        )
        .into_response();
    };
    match export_report(kind, query.period.as_deref()).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        report = kind.code(),
                        bytes = rendered.bytes.len(),
                        "财务报表 PDF 响应组装失败"
                    );
                    return error.into_response();
                }
            };
            audit(kind, &headers, &connect_info, &claims, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    kind: ReportKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    rendered: &RenderedDoc,
    started: Instant,
) {
    doc_pdf_model::ExportLogOwned {
        user_id: crate::utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: TARGET_TYPE.to_string(),
        // 报表没有单据主键：target_id 存本次导出事件的 id，业务定位靠 detail + doc_no
        doc_id: Uuid::now_v7(),
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "report_type": kind.slug(),
            "report_code": kind.code(),
            "source": SOURCE,
            "row_count": rendered.row_count,
            "total_amount": rendered.total_amount.to_string(),
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/report/{}/pdf", kind.slug()),
        client_ip: crate::utils::client_ip(headers, Some(connect_info)),
        user_agent: crate::utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};
    use std::str::FromStr;

    // ---------- 纯逻辑用例（不连库） ----------

    /// `{type}` 三个合法值 + 兼容写法；**不认识必须 None**（绝不猜一个最像的）。
    #[test]
    fn kind_parse_accepts_three_types_and_rejects_others() {
        assert_eq!(ReportKind::parse("balance-sheet"), Some(ReportKind::BalanceSheet));
        assert_eq!(ReportKind::parse("income-statement"), Some(ReportKind::IncomeStatement));
        assert_eq!(ReportKind::parse("cash-flow"), Some(ReportKind::CashFlow));
        // 兼容：下划线 / 大小写 / 前后空格
        assert_eq!(ReportKind::parse(" Balance_Sheet "), Some(ReportKind::BalanceSheet));
        assert_eq!(ReportKind::parse("CASH_FLOW"), Some(ReportKind::CashFlow));
        // 不合法
        for raw in [
            "",
            "balance",
            "income",
            "cashflow",
            "profit",
            "资产负债表",
            "balance-sheetx",
        ] {
            assert_eq!(ReportKind::parse(raw), None, "「{raw}」不该被认成报表类型");
        }
        // 三张报表的元数据彼此不重样（路由段 / 编码 / 中文名）
        let kinds = [
            ReportKind::BalanceSheet,
            ReportKind::IncomeStatement,
            ReportKind::CashFlow,
        ];
        let mut slugs: Vec<&str> = kinds.iter().map(|kind| kind.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), 3);
        assert_eq!(ReportKind::BalanceSheet.code(), report::REPORT_CODE);
        assert_eq!(ReportKind::BalanceSheet.label(), "资产负债表");
        assert_eq!(ReportKind::IncomeStatement.label(), "利润表");
        assert_eq!(ReportKind::CashFlow.label(), "现金流量表");
        assert!(KIND_HINT.contains("balance-sheet"));
    }

    /// 金额：千分位 + 两位小数，负数带 `-`，**先 round_dp(2) 再打印**（不截断）。
    #[test]
    fn money_is_thousands_with_two_decimals() {
        assert_eq!(money_or_dash(Some(Decimal::from(0))), "-");
        assert_eq!(money_or_dash(Some(Decimal::from(1))), "1.00");
        assert_eq!(money_or_dash(Some(Decimal::from(999))), "999.00");
        assert_eq!(money_or_dash(Some(Decimal::from(1000))), "1,000.00");
        assert_eq!(
            money_or_dash(Some(Decimal::from_str("1234567.891").expect("合法小数"))),
            "1,234,567.89"
        );
        assert_eq!(
            money_or_dash(Some(Decimal::from_str("-1234567.895").expect("合法小数"))),
            "-1,234,567.90"
        );
        // 截断会少一分钱：12.3456 必须四舍五入成 12.35，而不是 12.34
        assert_eq!(
            money_or_dash(Some(Decimal::from_str("12.3456").expect("合法小数"))),
            "12.35"
        );
        // 校验块里的零照印 0.00（差额是不是 0 要一眼看见）
        assert_eq!(money_plain(Decimal::ZERO), "0.00");
        assert_eq!(money_plain(Decimal::from_str("1234.5").expect("合法小数")), "1,234.50");
    }

    /// 卡面验收④：空白项（`None`）与零都印 `-`，绝不印 `0.00` 或空串。
    #[test]
    fn blank_and_zero_amounts_are_dash() {
        assert_eq!(money_or_dash(None), "-");
        assert_eq!(money_or_dash(Some(Decimal::ZERO)), "-");
        // 不足一分的金额四舍五入后为零 → 也是空白
        assert_eq!(money_or_dash(Some(Decimal::from_str("0.004").expect("合法小数"))), "-");
        assert!(is_blank(None));
        assert!(is_blank(Some(Decimal::ZERO)));
        assert!(!is_blank(Some(Decimal::from(1))));
    }

    fn cell(label: &str, amount: &str, cls: &str) -> ReportCell {
        ReportCell::new(label, amount, amount).with_cls(cls)
    }

    /// 卡面验收②：两侧总计必须落在**同一行**（短的一栏在总计行之前补空行）。
    #[test]
    fn align_sides_pushes_totals_to_the_same_row() {
        let left = vec![
            cell("流动资产", "-", "is-major"),
            cell("  货币资金", "1,000.00", ""),
            cell("资产总计", "1,000.00", "is-total"),
        ];
        let right = vec![
            cell("流动负债", "-", "is-major"),
            cell("  应付账款", "1,000.00", ""),
            cell("负债合计", "1,000.00", "is-total"),
            cell("所有者权益", "-", "is-major"),
            cell("  实收资本", "1,000.00", ""),
            cell("所有者权益合计", "1,000.00", "is-total"),
            cell("负债和所有者权益总计", "1,000.00", "is-total"),
        ];
        let rows = align_sides(&left, &right);
        assert_eq!(rows.len(), 7, "行数取两侧的较大者");
        // 左侧在最后一行（总计）之前补了 4 行空行
        assert_eq!(rows[1].left.label, "  货币资金");
        for row in rows.iter().take(6).skip(2) {
            assert_eq!(row.left.cls, "is-blank", "补位行必须是透明空格");
            assert_eq!(row.left.label, "");
            assert_eq!(row.left.end, "");
        }
        let last = rows.last().expect("必须有行");
        assert_eq!(last.left.label, "资产总计");
        assert_eq!(last.right.label, "负债和所有者权益总计");
        assert_eq!(last.left.end, last.right.end, "两栏总计必须相等（借贷平衡）");
        assert_eq!(last.cls, "is-total");
        // 反向：右侧更长时给左侧补位（不写死「资产一定更长」）
        let rows = align_sides(&right, &left);
        assert_eq!(rows.last().expect("必须有行").right.label, "资产总计");
    }

    /// 行 → 单元格：层级 / 减项 / 合计的样式类与「减：」只补一次。
    #[test]
    fn cell_of_applies_level_and_deduction_classes() {
        let line = |level: i16, name: &str, deduction: bool, total: bool| report::ReportLineRes {
            item_code: "X".to_string(),
            item_name: name.to_string(),
            level,
            parent_code: None,
            amount: Some(Decimal::from(100)),
            previous_amount: None,
            is_deduction: deduction,
            is_total: total,
            detail: None,
            accounts: Vec::new(),
        };
        let major = cell_of(&line(1, "流动资产", false, false));
        assert!(major.cls.contains("is-major"));
        assert_eq!(major.end, "100.00");
        assert_eq!(major.begin, "-", "上期无数据 → 期初列必须是 -");

        let detail = cell_of(&line(3, "明细", false, false));
        assert!(detail.cls.contains("is-detail"));

        let deduction = cell_of(&line(2, "累计折旧", true, false));
        assert!(deduction.cls.contains("is-deduction"));
        assert_eq!(deduction.label, "减：累计折旧");
        // 名字里已经带「减：」的不再补第二次
        let already = cell_of(&line(2, "减：累计摊销", true, false));
        assert_eq!(already.label, "减：累计摊销");

        let total = cell_of(&line(1, "资产总计", false, true));
        assert!(total.cls.contains("is-total"));
    }

    /// 期间状态文案（`DERIVED` 是报表侧独有的"未开账·按自然月推算"）。
    #[test]
    fn period_status_labels() {
        assert_eq!(period_status_label("OPEN"), "未结账");
        assert_eq!(period_status_label("CLOSED"), "已结账");
        assert_eq!(period_status_label("DERIVED"), "未开账·按自然月推算");
        assert_eq!(period_status_label("WHATEVER"), "状态未知");
    }

    /// 现金流量表「其他」节：有明细或净额非零才印（全零不印一屏 `-`）。
    #[test]
    fn other_section_only_when_nonempty() {
        let section = |inflow: Vec<cash_flow::CashLineRes>, net: Decimal| cash_flow::ActivitySectionRes {
            activity: "OTHER".to_string(),
            activity_name: "四、其他".to_string(),
            inflow,
            inflow_total: cash_flow::AmountPair { current: net, ytd: net },
            outflow: Vec::new(),
            outflow_total: cash_flow::AmountPair {
                current: Decimal::ZERO,
                ytd: Decimal::ZERO,
            },
            net: cash_flow::AmountPair { current: net, ytd: net },
        };
        assert!(!other_section_visible(&section(Vec::new(), Decimal::ZERO)));
        assert!(other_section_visible(&section(Vec::new(), Decimal::from(5))));
        let line = cash_flow::CashLineRes {
            item_code: "OTHER_1122".to_string(),
            item_name: "应收账款".to_string(),
            current: Decimal::ZERO,
            ytd: Decimal::ZERO,
            accounts: vec!["1122".to_string()],
            detail: None,
        };
        assert!(other_section_visible(&section(vec![line], Decimal::ZERO)));
    }

    // ---------- 真库用例（ERP_DB_URL；没设时与 R1/R2/R3 同样早退） ----------

    const FIXTURE_WORD: &str = "PDF07";

    /// 夹具期次：**2001-xx 归本模块独占**（`2026` = 生产/全仓共享期间，绝不碰）。
    ///
    /// 为什么（本用例在 E2E-FIX-2 全量跑时红掉的真实原因，卡 t_5904f851）：
    /// 断言钉死了「本期净利润恰为 500」和「上期无凭证 ⇒ 期初列印 `-`」，
    /// 而 R1 的「期初」= **期间开始日之前的历史累计**（`load_accounts` 的 `net_before`），
    /// 所以任何 2026-xx 期间在共享库上都不可能「上期无凭证」——别的模块夹具
    /// （P2D1 等）早把凭证记进了 2026-09/10/11（实测残留：2026-10 有 3 张 POSTED、
    /// 货币资金 700.00），于是期初成了 `700.00` 而不是 `-`。
    /// 这不是模板/PDF 形状断言过期，是**夹具期间选错**：要证明「上期无凭证 ⇒ 期初印 `-`」，
    /// 期间必须**早于库里所有凭证**。
    ///
    /// 为什么是 2001-02 而不是更早的 1999/2000：
    /// * `2000-12` 是 `income_statement` / `cash_flow` 的「空期间全 0」夹具期
    ///   （它们断言该期 `current` / `ytd` 全 0）——往里记凭证会把它们的用例打红；
    /// * 2001 年之后到 2021 年之间全库无凭证（实测最早凭证 2021-05-05），
    ///   所以 2001-02 的「期初」天然为 0（印 `-`），且不污染任何真实期间；
    /// * 本用例收尾会 `purge_fixture()` 删掉这两张凭证，退出后对后续用例零影响。
    const FIXTURE_PERIOD: &str = "2001-02";
    const FIXTURE_PREV_PERIOD: &str = "2001-01";
    const FIXTURE_STAMP: &str = "2001-02-09 10:00";

    /// 建夹具：`2001-02` 期间 + 两张 POSTED 凭证（含现金 / 权益 / 收入）。
    ///
    /// ```text
    /// 2001-02-05  借 1001 库存现金 1,000 / 贷 3001 实收资本 1,000
    /// 2001-02-06  借 1122 应收账款   500 / 贷 5001 主营业务收入 500
    /// ⇒ 资产 1,500 = 负债+权益 1,500（借贷平衡）；净利润 500 = 未分配利润本年变动 500
    /// ```
    async fn setup_fixture() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let statements = [
            format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'"),
            format!(
                "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                 start_date, end_date, status) \
                 VALUES (2001, 2, '{FIXTURE_PERIOD}', '2001-02-01', '2001-02-28', 'OPEN') \
                 ON CONFLICT (period_code) DO NOTHING"
            ),
            format!(
                "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                 start_date, end_date, status) \
                 VALUES (2001, 1, '{FIXTURE_PREV_PERIOD}', '2001-01-01', '2001-01-31', 'CLOSED') \
                 ON CONFLICT (period_code) DO NOTHING"
            ),
        ];
        for sql in &statements {
            db().execute_with_params(sql, vec![])
                .await
                .unwrap_or_else(|error| panic!("夹具 SQL 失败：{error}\n{sql}"));
        }
        post_voucher("PDF07-1", "2001-02-05", "1001", "3001", 1000).await;
        post_voucher("PDF07-2", "2001-02-06", "1122", "5001", 500).await;
        // fail-closed：两张凭证必须真的进库，否则后面全是「空表也绿」的假证明
        let count: i64 = db()
            .scalar(&format!(
                "SELECT count(*) FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}' AND status = 'POSTED'"
            ))
            .await
            .unwrap_or_else(|error| panic!("夹具自检失败：{error}"));
        assert_eq!(count, 2, "夹具凭证必须正好 2 张 POSTED");
        Some(guard)
    }

    /// 收尾：删掉本夹具的两张凭证。
    ///
    /// 这两张凭证记在 **2001-02**，虽在真实数据之前，但 R1 的「期初 = 期间开始日之前的
    /// 历史累计」意味着它们会成为**所有 2001-02 之后期间**的期初；本模块串行跑，
    /// 留在库里会污染后面用例（尤其 `income_statement` / `cash_flow` 的绝对值断言）。
    /// 所以每条真库用例收尾都清一次（setup 也会先清，重复跑不积累）。
    async fn purge_fixture() {
        db().execute_with_params(
            &format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'"),
            vec![],
        )
        .await
        .unwrap_or_else(|error| panic!("清夹具失败：{error}"));
    }

    /// 建一张 POSTED 凭证（两行分录：借 / 贷），科目按 `account_code` 取。
    async fn post_voucher(no: &str, date: &str, debit_code: &str, credit_code: &str, amount: i64) {
        let sql = format!(
            "WITH v AS ( \
                INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary, \
                                         total_debit, total_credit, entry_count, status) \
                VALUES ('{FIXTURE_WORD}', '{no}', '{FIXTURE_PERIOD}', DATE '{date}', 'PDF07 夹具', \
                        {amount}, {amount}, 2, 'POSTED') RETURNING id \
             ), d AS ( \
                SELECT id, account_code, account_name FROM biz_account \
                 WHERE account_code = '{debit_code}' AND deleted_at IS NULL \
             ), c AS ( \
                SELECT id, account_code, account_name FROM biz_account \
                 WHERE account_code = '{credit_code}' AND deleted_at IS NULL \
             ) \
             INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
                                            account_name, debit_amount, credit_amount) \
             SELECT v.id, 1, 'PDF07 借方', d.id, d.account_code, d.account_name, {amount}, 0 FROM v, d \
             UNION ALL \
             SELECT v.id, 2, 'PDF07 贷方', c.id, c.account_code, c.account_name, 0, {amount} FROM v, c"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("建凭证失败：{error}\n{sql}"));
    }

    /// 金额串必须符合「千分位 + 两位小数」或就是 `-`（卡面验收③④的机器判据）。
    fn assert_amount_shape(text: &str, what: &str) {
        if text == "-" {
            return;
        }
        let negative = text.starts_with('-');
        let body = if negative { &text[1..] } else { text };
        let (integer, fraction) = body
            .split_once('.')
            .unwrap_or_else(|| panic!("{what} 金额「{text}」没有小数点"));
        assert_eq!(fraction.len(), 2, "{what} 金额「{text}」必须两位小数");
        assert!(
            fraction.chars().all(|ch| ch.is_ascii_digit()),
            "{what} 金额「{text}」小数位不是数字"
        );
        let groups: Vec<&str> = integer.split(',').collect();
        assert!((1..=3).contains(&groups.len()), "{what} 金额「{text}」千分位分组数异常");
        assert!(
            !groups[0].is_empty() && groups[0].len() <= 3 && groups[0].chars().all(|c| c.is_ascii_digit()),
            "{what} 金额「{text}」整数首位分组非法"
        );
        for group in groups.iter().skip(1) {
            assert_eq!(group.len(), 3, "{what} 金额「{text}」千分位必须是 3 位一组");
            assert!(
                group.chars().all(|c| c.is_ascii_digit()),
                "{what} 金额「{text}」千分位含非数字"
            );
        }
        // 三位以上必须真的打了逗号（否则 `1,234.00` 会写成 `1234.00`）
        if integer.len() > 3 {
            assert!(integer.contains(','), "{what} 金额「{text}」缺千分位");
        }
    }

    /// 真库：三张报表都能从真数据编出上下文，且格式化 / 空白 / 平衡三条验收成立。
    ///
    /// 注意断言口径：**只钉「契约」不钉「具体数字」**——同一个测试库可能被别的模块
    /// 的真库用例写入过凭证（`DB_LOCK` 只保证串行，不做跨模块清场），所以
    /// 「资产总计 = 1,500.00」这种绝对值在整仓 `cargo test` 里会假红。
    /// 精确金额的验收在 145 隔离库上由 curl E2E 断言（证据在 `docs/evidence/`）。
    #[tokio::test]
    async fn pdf07_real_three_reports_agree_on_shape() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        // ---------- ① 资产负债表 ----------
        let snapshot = report::snapshot_or_error(FIXTURE_PERIOD)
            .await
            .expect("资产负债表快照必须成功");
        let sheet = report::into_response(report::REPORT_CODE, report::REPORT_NAME, "测试公司", "元", snapshot);
        let ctx = build_balance_context(&sheet, FIXTURE_STAMP.to_string());
        assert_eq!(ctx.doc_no, format!("BALANCE_SHEET-{FIXTURE_PERIOD}"));
        assert_eq!(ctx.head.doc_title, "资 产 负 债 表");
        assert_eq!(ctx.head.unit_label, "单位：元");
        assert_eq!(ctx.end_label, "期末余额");
        assert_eq!(ctx.begin_label, "期初余额");
        assert!(ctx.check.cls == "is-ok", "借贷必须平：{:?}", ctx.check);
        let last = ctx.rows.last().expect("资产负债表不能为空");
        assert_eq!(last.left.label, "资产总计");
        assert_eq!(last.right.label, "负债和所有者权益总计");
        assert_eq!(
            last.left.end, last.right.end,
            "卡面验收②：两侧总计必须相等\n左={}\n右={}",
            last.left.end, last.right.end
        );
        assert_ne!(last.left.end, "-", "本期有凭证，资产总计不可能是空白");
        for row in &ctx.rows {
            // 补位空行（`is-blank`）整格透明，不参与金额格式校验
            if row.left.cls.contains("is-blank") {
                assert_eq!(row.left.label, "", "补位行不许有项目名");
            } else {
                assert_amount_shape(&row.left.end, "资产·期末");
                assert_amount_shape(&row.left.begin, "资产·期初");
            }
            if row.right.cls.contains("is-blank") {
                assert_eq!(row.right.label, "", "补位行不许有项目名");
            } else {
                assert_amount_shape(&row.right.end, "负债权益·期末");
                assert_amount_shape(&row.right.begin, "负债权益·期初");
            }
        }
        // 本期有货币资金（1001），期初（2001-01 无凭证）必须是 `-` 而不是 0.00。
        let cash = ctx
            .rows
            .iter()
            .find(|row| row.left.label.contains("货币资金"))
            .expect("资产侧必须有「货币资金」");
        assert_ne!(cash.left.end, "-", "本期有现金凭证，货币资金必须有数");
        assert_eq!(cash.left.begin, "-", "上期无凭证 → 期初列必须印「-」");

        // ---------- ② 利润表 ----------
        let snapshot = income_statement::snapshot_or_error(FIXTURE_PERIOD)
            .await
            .expect("利润表快照必须成功");
        let income = income_statement::into_response("测试公司", "元", snapshot);
        let ctx = build_income_context(&income, FIXTURE_STAMP.to_string());
        assert_eq!(ctx.doc_no, format!("INCOME_STATEMENT-{FIXTURE_PERIOD}"));
        assert_eq!(ctx.head.doc_title, "利 润 表");
        assert_eq!(ctx.current_label, "本期金额");
        assert_eq!(ctx.ytd_label, "本年累计");
        assert!(!ctx.rows.is_empty(), "利润表不能为空");
        let revenue = ctx
            .rows
            .iter()
            .find(|row| row.label.contains("营业收入"))
            .expect("必须有营业收入行");
        assert_ne!(revenue.current, "-", "本期有 5001 收入凭证");
        for row in &ctx.rows {
            assert_amount_shape(&row.current, "利润表·本期");
            assert_amount_shape(&row.ytd, "利润表·本年累计");
        }
        let net = ctx
            .rows
            .iter()
            .find(|row| row.code == "NET_PROFIT")
            .expect("必须有净利润行（NET_PROFIT）");
        assert_eq!(net.kind, "total", "净利润必须是合计行（加粗）");
        assert_eq!(net.current, "500.00", "净利润本期 = 500（收入 500，无费用）");

        // ---------- ③ 现金流量表 ----------
        let snapshot = cash_flow::snapshot_or_error(FIXTURE_PERIOD)
            .await
            .expect("现金流量表快照必须成功");
        let cashflow = cash_flow::into_response("测试公司", "元", snapshot);
        let ctx = build_cash_flow_context(&cashflow, FIXTURE_STAMP.to_string());
        assert_eq!(ctx.doc_no, format!("CASH_FLOW-{FIXTURE_PERIOD}"));
        assert_eq!(ctx.head.doc_title, "现 金 流 量 表");
        assert!(!ctx.rows.is_empty());
        for row in &ctx.rows {
            if row.kind == "note" {
                continue;
            }
            assert_amount_shape(&row.current, "现金流量表·本期");
            assert_amount_shape(&row.ytd, "现金流量表·本年累计");
        }
        // 三个活动节必须都在（经营 / 投资 / 筹资）
        for keyword in ["经营活动", "投资活动", "筹资活动"] {
            assert!(
                ctx.rows.iter().any(|row| row.label.contains(keyword)),
                "现金流量表缺「{keyword}」节"
            );
        }
        assert!(
            ctx.rows
                .iter()
                .any(|row| row.label.contains("期末现金及现金等价物余额")),
            "必须有「期末现金及现金等价物余额」行"
        );
        assert!(ctx.has_notes, "简化口径说明必须随表出现（不许静默）");
        purge_fixture().await;
    }

    /// 真库：期间不存在 → 400 + 66010（与页面同一错误码，不是 500），三张报表一致。
    #[tokio::test]
    async fn pdf07_real_missing_period_is_400_66010() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        for kind in [
            ReportKind::BalanceSheet,
            ReportKind::IncomeStatement,
            ReportKind::CashFlow,
        ] {
            let error = export_report(kind, Some("1998-07"))
                .await
                .expect_err("1998-07 既没开账也没凭证，必须报错");
            assert_eq!(error.status, 400, "{:?} 期间不存在必须 400", kind);
            assert_eq!(
                error.code,
                error_code::ERP_PERIOD_NOT_FOUND,
                "{:?} 必须用 66010，与页面同码",
                kind
            );
        }
        // period 缺失 / 格式错 → 400 + 10001（在连库之前就拦掉）
        for period in [None, Some(""), Some("2026-13"), Some("2026-11-01")] {
            let error = export_report(ReportKind::BalanceSheet, period)
                .await
                .expect_err("非法 period 必须报错");
            assert_eq!(error.status, 400);
            assert_eq!(error.code, error_code::PARAM_INVALID);
        }
        purge_fixture().await;
    }

    /// 真库：`biz_report_item` 的三套配置都在（报表 PDF 依赖它们，配置缺了要 66011）。
    #[tokio::test]
    async fn pdf07_real_report_items_seeded_for_all_three() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        for code in ["BALANCE_SHEET", "INCOME_STATEMENT"] {
            let items: i64 = db()
                .scalar(&format!(
                    "SELECT count(*) FROM biz_report_item WHERE report_code = '{code}' \
                     AND is_active = 1"
                ))
                .await
                .unwrap_or_else(|error| panic!("查报表项失败：{error}"));
            assert!(items > 0, "{code} 必须有启用的报表项，实际 {items}");
        }
        // 现金流量表不用 biz_report_item，走 biz_cashflow_item
        let cash_items: i64 = db()
            .scalar("SELECT count(*) FROM biz_cashflow_item WHERE is_active = 1")
            .await
            .unwrap_or_else(|error| panic!("查现金流量分类失败：{error}"));
        assert!(cash_items > 0, "现金流量分类配置不能为空");
        // 夹具用的四个科目必须存在（不然凭证插不进去 → 夹具自检会先炸）
        for code in ["1001", "1122", "3001", "5001"] {
            let found: i64 = db()
                .scalar(&format!(
                    "SELECT count(*) FROM biz_account WHERE account_code = '{code}' AND deleted_at IS NULL"
                ))
                .await
                .unwrap_or_else(|error| panic!("查科目失败：{error}"));
            assert_eq!(found, 1, "科目 {code} 必须存在且唯一");
        }
        purge_fixture().await;
    }
}
