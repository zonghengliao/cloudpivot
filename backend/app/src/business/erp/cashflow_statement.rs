//! 由**底稿**生成现金流量表（M1-16 · 直接法三栏报表：项目 / 本期金额 / 上期金额）。
//!
//! 上游：`V0414`（M1-13 底稿表 `biz_cashflow_worksheet`）、`V0415`（M1-14 底稿录入 + 勾稽）、
//! `V0416`（M1-15 间接法调整项配置）。本模块**只读**底稿，**不写一个字节**。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/cash-flow/statement?period=2026-11` | `business:erp:cashflow-statement:list` | 由底稿汇总出的现金流量表（本期 + 上期两列）+ 勾稽结果 |
//! | GET | `/business/erp/report/cash-flow/statement/export?period=2026-11` | `business:erp:cashflow-statement:export` | 同一份报表导出 xlsx（出表闸：未闭合 400 + 66087） |
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`（AGENTS.md 约定 8）。
//!
//! # 「三栏」是哪三栏（口径先钉死）
//!
//! ```text
//! 项目 | 本期金额 | 上期金额
//! ```
//!
//! `上期` = **上一会计期间**（`report::previous_period_of`：`2026-11` → `2026-10`），
//! 与资产负债表 / 利润表的「上期金额」同一口径 —— 全仓只有一份「上期」定义（`V0414` 的
//! `idx_biz_cashflow_ws_item_code` 注释就是为这一列建的：「跨期对比一行项目的历史
//! （M1-16 报表「上期金额」、底稿复核）」）。
//!
//! 上期**只认会计日历里真有期间行的期间**（与 `report::build_snapshot_of` 逐字同规则）：
//! 上期没开账 ⇒ 整列 `null` + `warnings` 点名，**绝不替用户推导一个不存在的报告期**，
//! 也绝不把「上期没编」画成「上期是 0」—— 0 与「没编」对报表是相反的意思。
//!
//! # 数据来源只有底稿（不读凭证、不二次分类）
//!
//! 行集合 = **本期底稿的合并结果**（`cashflow_worksheet::compose_sheet`：配置项 ∪ 已录行，
//! 已录行以快照为准，配置里已删的已录行照样出）∪ **上期底稿里本期已没有的行**。
//! 三条合并规则直接复用 M1-14 的实现，不在这里重写一遍 —— 重写就会出两份「哪一行该出现」
//! 的真相，而这两份真相迟早会分叉。
//!
//! 金额口径也与底稿一致：`FLOW` 行的 `amount` 恒 ≥ 0，方向由 `direction`（INFLOW / OUTFLOW）
//! 表达；**本模块不做任何加减号推断**，也不会把「流出」金额取负再求和（那会让
//! 「经营活动流入合计」出现负流入，两栏互相抵消，报表口径当场失真）。
//!
//! ## 为什么本模块不复用 ERP-R3（`cash_flow.rs`，按凭证直接分类）
//!
//! 两者是**两套并行的编制路径**，不是新旧版本：
//!
//! | | ERP-R3 `cash_flow` | 本模块 M1-16 |
//! |---|---|---|
//! | 取数 | 凭证分录 + 对方科目分类规则（`biz_cashflow_item.account_pattern`） | **底稿行金额**（会计逐行编制的结果） |
//! | 列 | 本期金额 / 本年累计 | 本期金额 / **上期金额** |
//! | 谁说了算 | 分类算法 | 编制底稿的人 |
//!
//! 底稿路径的意义正是「算法分不出来的，人来定」：一笔复合收款的对方科目同时命中
//! 应收账款与预收账款时，R3 只能按规则硬分；底稿里会计可以直接写成报表要求的口径。
//! 因此本表**不拿 R3 的数去覆盖底稿**，也不做「两边必须相等」的断言 —— 那是 M1-24
//! （收口 E2E）要观察的差异，不是本卡要抹平的噪声。
//!
//! # 出表闸（`check.balanced`）
//!
//! ```text
//! 期初余额 + 流入合计 − 流出合计 = 期末余额      （本期，差 0 才算闭合）
//! 净增加额 + 期初余额           = 期末余额      （报表自身的加减关系）
//! ```
//!
//! 第一条由 `cashflow_worksheet::compose_sheet` 算（复用 M1-14 的 `reconcile`，精确到 4 位
//! 小数，**绝不四舍五入抹平**），第二条由本模块用**报表自己的**两列再算一次 —— 两条都要真，
//! 否则一张「小计加错」的表也能靠底稿勾稽过闸。
//!
//! | 入口 | 未闭合时 | 为什么 |
//! |------|----------|--------|
//! | `GET …/statement` | **200** + `check.balanced=false` + 逐条 `problems` | 页面要能打开一张还没编完的表，否则会计没有入口去补（与 M1-14「GET 不因配置坏了而报错」同理） |
//! | `GET …/statement/export` | **400 + 66087** | 出表 = 对外报出。差一分钱就是错表，宁可导不出来并告诉人差在哪 |

use axum::extract::Query;
use axum::response::Response;
use nik::prelude::chrono::{DateTime, Local, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::cash_flow::{self, ACTIVITIES};
use crate::business::erp::cashflow_worksheet::{
    self, BalanceRes, CheckRes, ConfigItem, DIRECTION_INFLOW, DIRECTION_OUTFLOW, LINE_CLOSING, LINE_OPENING,
    WorksheetRes,
};
use crate::business::erp::report;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 报表编码（出参 / 导出文件名用）。
pub const REPORT_CODE: &str = "CASH_FLOW_WORKSHEET";

/// 报表名称（页头 / 导出文件名）。
pub const REPORT_NAME: &str = "现金流量表";

/// 数据来源标记：本表由底稿汇总（不是凭证分类）。
pub const SOURCE_WORKSHEET: &str = "WORKSHEET";

/// 本期列名。
pub const CURRENT_LABEL: &str = "本期金额";
/// 上期列名（上一会计期间）。
pub const PREVIOUS_LABEL: &str = "上期金额";

/// 读接口权限码（`V0417` 种，挂「现金流量表」页面 `…001018`）。
pub const PERM_STATEMENT_LIST: &str = "business:erp:cashflow-statement:list";
/// 导出接口权限码（同上）。
pub const PERM_STATEMENT_EXPORT: &str = "business:erp:cashflow-statement:export";

/// 本模块**新增**的权限码清单（供 `perm_menu_gate.py` 对账）。
pub const PERMISSIONS: [&str; 2] = [PERM_STATEMENT_LIST, PERM_STATEMENT_EXPORT];

/// 取数口径说明（原样展示在页头 / 导出件首行：口径必须跟着报表走）。
pub const SOURCE_NOTE: &str = "本表由「现金流底稿」逐行汇总：本期金额取本期底稿，上期金额取上一期间底稿；\
     不读凭证、不做二次分类。出表前底稿必须闭合（期初 + 流入 − 流出 = 期末）。";

/// 行的显示前缀：明细行缩进（纸面报表原文的层级感，导出件里靠缩进表达）。
pub const INDENT: &str = "    ";

// ==================== 入参 ====================

/// 出表查询参数（`period` 缺省 = 当前自然月，与底稿页同一约定）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
    /// 是否隐藏「本期与上期都为 0 且都没录」的行（默认 `false`：纸面报表把所有项目行都印出来）。
    pub hide_zero: Option<bool>,
}

// ==================== 出参 ====================

/// 一栏「本期 / 上期」的金额对。
///
/// `previous == None` 只有两种来源，都表达「这一列没有数」而不是 0：
/// ① 上一期间在会计日历里不存在（没开账）；② 该行只在本期底稿里出现、上期没有对应编制。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct StatementAmountRes {
    /// 本期金额。
    pub current: Decimal,
    /// 上期金额（`None` = 无上期口径 / 该行上期没有编制）。
    pub previous: Option<Decimal>,
}

/// 现金流量表的一行（含两栏金额与底稿痕迹）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementLineRes {
    /// 项目编码（`biz_cashflow_item.item_code`；底稿里配置已删的行也保留原编码）。
    pub item_code: String,
    /// 项目名称（底稿快照优先：配置改名后历史底稿仍按当时名称呈现）。
    pub item_name: String,
    /// 活动：`OPERATING` / `INVESTING` / `FINANCING`。
    pub activity: String,
    /// 方向：`INFLOW` / `OUTFLOW`（决定它落在流入区还是流出区）。
    pub direction: String,
    /// 显示顺序（底稿快照）。
    pub sort_order: i32,
    /// 本期金额（未录入 = 0）。
    pub current: Decimal,
    /// 上期金额（`None` = 上期无底稿 / 上期没有这一行）。
    pub previous: Option<Decimal>,
    /// 本期是否真的录过（`false` = 配置里有、本期按 0 列示）。
    pub current_entered: bool,
    /// 上期是否真的录过。
    pub previous_entered: bool,
    /// 对应的配置项是否仍启用（`false` = 已停用 / 已删除，历史行仍然列示）。
    pub item_active: bool,
    /// 本期编制说明。
    pub remark: Option<String>,
}

/// 一个活动分区（经营 / 投资 / 筹资）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementSectionRes {
    /// 活动编码。
    pub activity: String,
    /// 活动标题（纸面报表原文，含「一、」前缀）。
    pub activity_name: String,
    /// 流入明细行。
    pub inflow: Vec<StatementLineRes>,
    /// 现金流入小计。
    pub inflow_total: StatementAmountRes,
    /// 流出明细行。
    pub outflow: Vec<StatementLineRes>,
    /// 现金流出小计。
    pub outflow_total: StatementAmountRes,
    /// 净额 = 流入小计 − 流出小计。
    pub net: StatementAmountRes,
}

/// 余额行（期初 / 期末）：`None` 表示底稿里还没录，**不按 0 列示**。
#[derive(Debug, Clone, Serialize)]
pub struct StatementBalanceRes {
    /// `OPENING` / `CLOSING`。
    pub line_type: String,
    /// 行名称。
    pub item_name: String,
    /// 本期余额（`None` = 本期底稿未录）。
    pub current: Option<Decimal>,
    /// 上期余额（`None` = 上期无底稿 / 上期未录）。
    pub previous: Option<Decimal>,
}

/// 出表勾稽结果（两条等式 + 上期参考状态）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementCheckRes {
    /// 本期是否闭合（= 底稿勾稽闭合 **且** 报表加减关系成立）。导出闸就看这一位。
    pub balanced: bool,
    /// 上期是否闭合（`None` = 没有可比的上期底稿；上期不闭合不阻塞本期出表，只提示）。
    pub previous_balanced: Option<bool>,
    /// 报表自身加减关系：`净增加额 + 期初 = 期末`（本期）。
    pub arithmetic_ok: bool,
    /// 报表加减关系的差额（`期初 + 净增加额 − 期末`；缺余额时 `None`）。
    pub diff: Option<Decimal>,
    /// 一句话结论（前端直接显示 / 导出件印在末行）。
    pub message: String,
    /// 本期底稿勾稽（复用 M1-14 的 `CheckRes`，含 `problems[]`）。
    pub current: CheckRes,
    /// 上期底稿勾稽（`None` = 无上期底稿）。
    pub previous: Option<CheckRes>,
}

/// 现金流量表出参（三栏报表）。
#[derive(Debug, Clone, Serialize)]
pub struct CashStatementRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 编制单位。
    pub company_name: String,
    /// 金额单位。
    pub unit: String,
    /// 会计期间（`yyyy-MM`）。
    pub period: String,
    /// 期间中文（`2026年11月`）。
    pub period_name: String,
    /// 期间状态（`OPEN` / `CLOSED`）。
    pub period_status: String,
    /// 期间开始日。
    pub period_start: nik::prelude::chrono::NaiveDate,
    /// 期间结束日。
    pub period_end: nik::prelude::chrono::NaiveDate,
    /// 上期期间（`yyyy-MM`；上期没开账时为 `null`）。
    pub previous_period: Option<String>,
    /// 上期期间状态（同上）。
    pub previous_period_status: Option<String>,
    /// 本期列名。
    pub current_label: String,
    /// 上期列名。
    pub previous_label: String,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 数据来源（恒 `WORKSHEET`）。
    pub source: String,
    /// 取数口径说明。
    pub source_note: String,
    /// 三大活动分区（顺序固定 经营 → 投资 → 筹资）。
    pub sections: Vec<StatementSectionRes>,
    /// 现金及现金等价物净增加额。
    pub net_increase: StatementAmountRes,
    /// 加：期初现金及现金等价物余额。
    pub opening: StatementBalanceRes,
    /// 期末现金及现金等价物余额。
    pub closing: StatementBalanceRes,
    /// 出表勾稽。
    pub check: StatementCheckRes,
    /// 列示的明细行数（不含被 `hide_zero` 隐藏的）。
    pub line_count: usize,
    /// 被 `hide_zero` 隐藏的行数（`0` = 全量列示）。
    pub hidden_zero_lines: usize,
    /// 告警（上期未开账 / 上期未闭合 / 项目活动口径跨期不一致 / 配置已停用行）。
    pub warnings: Vec<String>,
}

// ==================== 纯逻辑（可单测，不碰库）====================

/// 两栏合计：本期直接求和；上期**只在上期期间存在时才出值**（不存在 → `None`，绝不当 0）。
///
/// `previous_available` 是「上一期间在会计日历里存在」的标志。上期存在时，没有编制的小计
/// 如实为 `0`（列在、数是 0）；上期不存在时整列为 `null`（列不存在）。这两种情况在纸面上
/// 是相反的结论，所以不能靠「有没有行」来推断。
pub fn sum_lines(lines: &[StatementLineRes], previous_available: bool) -> StatementAmountRes {
    let current: Decimal = lines.iter().map(|line| line.current).sum();
    let previous = if previous_available {
        Some(lines.iter().filter_map(|line| line.previous).sum())
    } else {
        None
    };
    StatementAmountRes { current, previous }
}

/// 两栏相减（小计用）：任一侧有值才出值。
pub fn subtract(left: StatementAmountRes, right: StatementAmountRes) -> StatementAmountRes {
    StatementAmountRes {
        current: left.current - right.current,
        previous: match (left.previous, right.previous) {
            (Some(left), Some(right)) => Some(left - right),
            _ => None,
        },
    }
}

/// 两栏相加（净增加额汇总三块活动用）。
pub fn add(left: StatementAmountRes, right: StatementAmountRes) -> StatementAmountRes {
    StatementAmountRes {
        current: left.current + right.current,
        previous: match (left.previous, right.previous) {
            (Some(left), Some(right)) => Some(left + right),
            _ => None,
        },
    }
}

/// 活动排序权重（`OPERATING` → 0 / `INVESTING` → 1 / `FINANCING` → 2；未知的排最后）。
pub fn activity_rank(activity: &str) -> usize {
    ACTIVITIES
        .iter()
        .position(|(code, _)| *code == activity)
        .unwrap_or(ACTIVITIES.len())
}

/// 把「本期底稿行 ∪ 上期底稿行」并成报表行（**纯逻辑**）。
///
/// 合并规则（三条，都有理由）：
///
/// 1. **行归属以本期快照为准**：项目名 / 活动 / 方向 / 顺序取本期底稿（`compose_sheet` 已经
///    做过「已录行取快照」），因为报表说的是本期。
/// 2. **只在上期存在的行也要出**：那是上期的编制结果，静默丢掉就是丢数；本期金额按 0 列示
///    但 `current_entered = false`，前端能看出「本期没编这一行」。
/// 3. **活动 / 方向跨期不一致要吼**：同一编码上期是流入、本期变成流出（配置被人改过方向），
///    上期金额只能按本期归属列示 —— 这不是能静默处理的事，写进 `warnings` 让人确认口径。
pub fn merge_lines(current: &WorksheetRes, previous: Option<&WorksheetRes>) -> (Vec<StatementLineRes>, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let mut merged: Vec<StatementLineRes> = Vec::new();

    for row in &current.rows {
        merged.push(StatementLineRes {
            item_code: row.item_code.clone(),
            item_name: row.item_name.clone(),
            activity: row.activity.clone(),
            direction: row.direction.clone(),
            sort_order: row.sort_order,
            current: row.amount,
            previous: None,
            current_entered: row.entered,
            previous_entered: false,
            item_active: row.item_active,
            remark: row.remark.clone(),
        });
    }

    if let Some(previous) = previous {
        for row in &previous.rows {
            if let Some(line) = merged.iter_mut().find(|line| line.item_code == row.item_code) {
                line.previous = Some(row.amount);
                line.previous_entered = row.entered;
                if line.activity != row.activity || line.direction != row.direction {
                    warnings.push(format!(
                        "项目「{}」（{}）的活动 / 方向在本期与上期不一致（上期 {}/{}，本期 {}/{}）：\
                         上期金额按本期归属列示，请确认跨期口径",
                        line.item_name, line.item_code, row.activity, row.direction, line.activity, line.direction
                    ));
                }
                continue;
            }
            merged.push(StatementLineRes {
                item_code: row.item_code.clone(),
                item_name: row.item_name.clone(),
                activity: row.activity.clone(),
                direction: row.direction.clone(),
                sort_order: row.sort_order,
                current: Decimal::ZERO,
                previous: Some(row.amount),
                current_entered: false,
                previous_entered: row.entered,
                item_active: row.item_active,
                remark: None,
            });
            warnings.push(format!(
                "项目「{}」（{}）只在上期底稿里存在（本期配置与底稿都没有）：按上期快照列示、本期金额 0，\
                 历史编制结果不静默丢弃",
                row.item_name, row.item_code
            ));
        }
    }

    merged.sort_by(|left, right| {
        activity_rank(&left.activity)
            .cmp(&activity_rank(&right.activity))
            .then(left.sort_order.cmp(&right.sort_order))
            .then(left.item_code.cmp(&right.item_code))
    });
    (merged, warnings)
}

/// 该行是否值得列示（`hide_zero` 判据）：两栏都是 0 **且**两边都没录过 ⇒ 可以不印。
///
/// 「录过 0」与「没录」对报表是相反的意思，所以只要有一边 `entered`，即使金额为 0 也保留。
fn line_visible(line: &StatementLineRes) -> bool {
    !line.current.is_zero()
        || line.previous.is_some_and(|value| !value.is_zero())
        || line.current_entered
        || line.previous_entered
}

/// 出表勾稽（纯逻辑）：底稿闭合 + 报表加减关系，两条都要真。
pub fn build_check(
    current: &WorksheetRes,
    previous: Option<&WorksheetRes>,
    net_increase: StatementAmountRes,
    opening: &StatementBalanceRes,
    closing: &StatementBalanceRes,
) -> StatementCheckRes {
    let diff = match (opening.current, closing.current) {
        (Some(open), Some(close)) => Some(open + net_increase.current - close),
        _ => None,
    };
    let arithmetic_ok = diff.is_some_and(|value| value.is_zero());
    let balanced = current.check.balanced && arithmetic_ok;

    let mut reasons: Vec<String> = Vec::new();
    if !current.check.balanced {
        reasons.push(current.check.message.clone());
    }
    if !arithmetic_ok {
        reasons.push(match diff {
            Some(value) => format!(
                "报表加减关系不平：期初 + 净增加额 − 期末 差 {}",
                report::diff_text(value)
            ),
            None => "期初或期末余额未录入，算不出「净增加额 + 期初 = 期末」".to_string(),
        });
    }

    let message = if balanced {
        format!(
            "勾稽闭合：期初 {} + 流入 {} − 流出 {} = 期末 {}",
            report::money_text(opening.current),
            report::money_text(Some(current.check.inflow)),
            report::money_text(Some(current.check.outflow)),
            report::money_text(closing.current)
        )
    } else {
        reasons.join("；")
    };

    StatementCheckRes {
        balanced,
        previous_balanced: previous.map(|sheet| sheet.check.balanced),
        arithmetic_ok,
        diff,
        message,
        current: current.check.clone(),
        previous: previous.map(|sheet| sheet.check.clone()),
    }
}

/// 报表主体（纯逻辑）：底稿 → 三栏报表。
///
/// 入参是两个期间的**整张底稿**（已经过 `compose_sheet` 合并配置与已录行），
/// 出参是可直接序列化的报表体（不含页头的编制单位 / 编制时间，那些由接口层补）。
pub fn compose_statement(
    period: &str,
    current: &WorksheetRes,
    previous: Option<&WorksheetRes>,
    hide_zero: bool,
) -> StatementBody {
    let (lines, mut warnings) = merge_lines(current, previous);
    let previous_available = previous.is_some();

    let mut sections: Vec<StatementSectionRes> = Vec::new();
    let mut net_increase = StatementAmountRes {
        current: Decimal::ZERO,
        previous: if previous_available { Some(Decimal::ZERO) } else { None },
    };
    let mut hidden_zero_lines = 0usize;
    let mut line_count = 0usize;

    for (code, name) in ACTIVITIES {
        let inflow_all: Vec<StatementLineRes> = lines
            .iter()
            .filter(|line| line.activity == *code && line.direction == DIRECTION_INFLOW)
            .cloned()
            .collect();
        let outflow_all: Vec<StatementLineRes> = lines
            .iter()
            .filter(|line| line.activity == *code && line.direction == DIRECTION_OUTFLOW)
            .cloned()
            .collect();

        let inflow_total = sum_lines(&inflow_all, previous_available);
        let outflow_total = sum_lines(&outflow_all, previous_available);
        let net = subtract(inflow_total, outflow_total);
        net_increase = add(net_increase, net);

        let (inflow, hidden_in) = visible(inflow_all, hide_zero);
        let (outflow, hidden_out) = visible(outflow_all, hide_zero);
        hidden_zero_lines += hidden_in + hidden_out;
        line_count += inflow.len() + outflow.len();

        sections.push(StatementSectionRes {
            activity: (*code).to_string(),
            activity_name: (*name).to_string(),
            inflow,
            inflow_total,
            outflow,
            outflow_total,
            net,
        });
    }

    if !previous_available && let Some(code) = report::previous_period_of(period) {
        warnings.push(format!(
            "上一期间 {code} 在会计日历里没有期间行（未开账）：上期金额整列为空，\
                 报表不替用户推导一个不存在的报告期"
        ));
    }

    if let Some(previous) = previous
        && !previous.check.balanced
    {
        warnings.push(format!(
            "上期底稿未闭合（{}）：上期金额仅供参考，不影响本期出表",
            previous.check.message
        ));
    }

    let unentered = current.rows.iter().filter(|row| !row.entered).count();
    if unentered > 0 {
        warnings.push(format!(
            "本期有 {unentered} 个配置项目还没录底稿（按 0 列示）；需要列示的控制行请补齐底稿"
        ));
    }
    let inactive = lines.iter().filter(|line| !line.item_active).count();
    if inactive > 0 {
        warnings.push(format!(
            "有 {inactive} 行的现金流量项目已在配置中停用 / 删除：按底稿快照照常列示（历史编制结果不静默丢弃）"
        ));
    }
    if current.rows.is_empty() && current.opening.is_none() && current.closing.is_none() {
        warnings.push(format!("本期（{period}）底稿尚未编制：本表为空表，请先在底稿页录入"));
    }

    let opening = StatementBalanceRes {
        line_type: LINE_OPENING.to_string(),
        item_name: cashflow_worksheet::OPENING_NAME.to_string(),
        current: current.opening.as_ref().map(|row: &BalanceRes| row.amount),
        previous: previous.and_then(|sheet| sheet.opening.as_ref().map(|row| row.amount)),
    };
    let closing = StatementBalanceRes {
        line_type: LINE_CLOSING.to_string(),
        item_name: cashflow_worksheet::CLOSING_NAME.to_string(),
        current: current.closing.as_ref().map(|row| row.amount),
        previous: previous.and_then(|sheet| sheet.closing.as_ref().map(|row| row.amount)),
    };
    let check = build_check(current, previous, net_increase, &opening, &closing);

    StatementBody {
        sections,
        net_increase,
        opening,
        closing,
        check,
        warnings,
        line_count,
        hidden_zero_lines,
    }
}

/// 报表主体（页头之外的整张表）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementBody {
    /// 三大活动分区。
    pub sections: Vec<StatementSectionRes>,
    /// 净增加额。
    pub net_increase: StatementAmountRes,
    /// 期初余额。
    pub opening: StatementBalanceRes,
    /// 期末余额。
    pub closing: StatementBalanceRes,
    /// 勾稽。
    pub check: StatementCheckRes,
    /// 告警。
    pub warnings: Vec<String>,
    /// 列示行数。
    pub line_count: usize,
    /// 被隐藏的零行数。
    pub hidden_zero_lines: usize,
}

/// 按需过滤零行（返回「列示的行」与「被隐藏的行数」）。
fn visible(lines: Vec<StatementLineRes>, hide_zero: bool) -> (Vec<StatementLineRes>, usize) {
    if !hide_zero {
        return (lines, 0);
    }
    let mut kept: Vec<StatementLineRes> = Vec::new();
    let mut hidden = 0usize;
    for line in lines {
        if line_visible(&line) {
            kept.push(line);
        } else {
            hidden += 1;
        }
    }
    (kept, hidden)
}

/// 导出闸（纯逻辑）：未闭合就不许出表。返回 `Err` 的报文点名差在哪。
pub fn ensure_exportable(statement: &CashStatementRes) -> std::result::Result<(), ApiError> {
    if statement.check.balanced {
        return Ok(());
    }
    let mut message = format!(
        "现金流量表 {} 未闭合，不能出表：{}",
        statement.period, statement.check.message
    );
    let problems: Vec<String> = statement
        .check
        .current
        .problems
        .iter()
        .map(|problem| problem.message.clone())
        .collect();
    if !problems.is_empty() {
        message.push_str("。待补：");
        message.push_str(&problems.join("；"));
    }
    message.push_str("。请到「现金流底稿」页把期初 / 期末与明细行补齐后再出表");
    Err(ApiError::new(
        400,
        error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED,
        message,
    ))
}

// ==================== 数据层（只读底稿）====================

/// 读一个期间的整张底稿；`required = false` 时期间不存在返回 `None`（上期对比用）。
pub async fn load_sheet_of(
    tx: &Transaction,
    period: &str,
    config: &[ConfigItem],
    required: bool,
) -> DbResult<Option<WorksheetRes>> {
    let status = cashflow_worksheet::load_period_status(tx, period).await?;
    let Some(status) = status else {
        if required {
            return Err(DbError::InvalidValue(format!(
                "会计期间 {period} 不存在（biz_accounting_period 无记录）：现金流量表只能对已开账的期间编制，\
                 请先在「会计期间」里开启该期间"
            )));
        }
        return Ok(None);
    };
    let stored = cashflow_worksheet::load_rows(tx, period).await?;
    Ok(Some(cashflow_worksheet::compose_sheet(
        period, &status, config, &stored,
    )))
}

/// 在事务里编制整张报表（期间校验 + 上期对比 + 底稿合并）。
pub async fn build_statement(
    tx: &Transaction,
    period: &str,
    hide_zero: bool,
    company_name: &str,
) -> DbResult<CashStatementRes> {
    let config = cashflow_worksheet::load_config_items(tx).await?;
    let current = load_sheet_of(tx, period, &config, true)
        .await?
        .ok_or_else(|| DbError::InvalidValue(format!("会计期间 {period} 不存在")))?;
    let previous = match report::previous_period_of(period) {
        Some(code) => load_sheet_of(tx, &code, &config, false).await?,
        None => None,
    };

    let body = compose_statement(period, &current, previous.as_ref(), hide_zero);
    let (period_start, period_end) =
        report::month_range(period).ok_or_else(|| DbError::InvalidValue(format!("期间 {period} 无法解析为自然月")))?;

    Ok(CashStatementRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        company_name: company_name.to_string(),
        unit: "元".to_string(),
        period: period.to_string(),
        period_name: report::period_name_of(period),
        period_status: current.period_status.clone(),
        period_start,
        period_end,
        previous_period: previous.as_ref().map(|sheet| sheet.period.clone()),
        previous_period_status: previous.as_ref().map(|sheet| sheet.period_status.clone()),
        current_label: CURRENT_LABEL.to_string(),
        previous_label: PREVIOUS_LABEL.to_string(),
        generated_at: Utc::now(),
        source: SOURCE_WORKSHEET.to_string(),
        source_note: SOURCE_NOTE.to_string(),
        sections: body.sections,
        net_increase: body.net_increase,
        opening: body.opening,
        closing: body.closing,
        check: body.check,
        line_count: body.line_count,
        hidden_zero_lines: body.hidden_zero_lines,
        warnings: body.warnings,
    })
}

// ==================== 接口层 ====================

/// 期间参数规整（缺省 = 当前自然月，与底稿页同一约定）。
fn period_of(raw: Option<&str>) -> String {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text.to_string(),
        None => cashflow_worksheet::current_period(),
    }
}

/// 在 REPEATABLE READ 快照里编制报表，并把数据层错误翻成 HTTP 错误。
pub(crate) async fn snapshot_or_error(
    period: &str,
    hide_zero: bool,
) -> std::result::Result<CashStatementRes, ApiError> {
    if report::parse_period(period).is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("period 必须是 yyyy-MM（月份 01–12），收到「{period}」"),
        ));
    }
    let company = cash_flow::company_name().await;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启现金流量表（底稿）快照事务"))?;
    // ★ 与服务端其余报表同一手法：整张报表在一个快照里算完，`SET TRANSACTION` 必须是
    //   `BEGIN` 之后的第一条语句（PG 规定，否则 25001）。
    if let Err(error) = tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置现金流量表（底稿）事务隔离级别"));
    }
    match build_statement(&tx, period, hide_zero, &company).await {
        Ok(statement) => {
            if let Err(error) = tx.commit().await {
                return Err(utils::db_error(error, "提交现金流量表（底稿）快照事务"));
            }
            Ok(statement)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            match error {
                DbError::InvalidValue(message) if message.contains("不存在") => {
                    Err(ApiError::new(400, error_code::ERP_PERIOD_NOT_FOUND, message))
                }
                DbError::InvalidValue(message) => Err(ApiError::new(
                    500,
                    error_code::ERP_REPORT_CONFIG_INVALID,
                    format!("{message}（period={period}）"),
                )),
                other => Err(utils::db_error(other, "编制现金流量表（底稿）")),
            }
        }
    }
}

/// 由底稿生成的现金流量表（三栏：项目 / 本期金额 / 上期金额）。
#[nik::permission(
    path = "/business/erp/report/cash-flow/statement",
    method = "GET",
    permission = "business:erp:cashflow-statement:list"
)]
#[nik::get("/business/erp/report/cash-flow/statement")]
pub async fn statement_get_api(Query(query): Query<StatementQuery>) -> ResResult<CashStatementRes> {
    let period = period_of(query.period.as_deref());
    Ok(Res::ok(
        snapshot_or_error(&period, query.hide_zero.unwrap_or(false)).await?,
    ))
}

/// 现金流量表导出（xlsx）。**出表闸**：未闭合直接 400 + 66087，不生成文件。
#[nik::permission(
    path = "/business/erp/report/cash-flow/statement/export",
    method = "GET",
    permission = "business:erp:cashflow-statement:export"
)]
#[nik::get("/business/erp/report/cash-flow/statement/export")]
pub async fn statement_export_api(Query(query): Query<StatementQuery>) -> std::result::Result<Response, ApiError> {
    let period = period_of(query.period.as_deref());
    let statement = snapshot_or_error(&period, query.hide_zero.unwrap_or(false)).await?;
    ensure_exportable(&statement)?;
    let (headers, rows) = export_table(&statement);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &format!(
            "{REPORT_NAME}_{}_{}.xlsx",
            statement.period,
            Local::now().format("%Y%m%d%H%M%S")
        ),
    ))
}

/// 把报表拍成「项目 / 本期金额 / 上期金额」三栏的表格行（与纸面现金流量表同构）。
pub fn export_table(report: &CashStatementRes) -> (Vec<&'static str>, Vec<std::collections::HashMap<String, String>>) {
    const HEADERS: [&str; 3] = ["项目", CURRENT_LABEL, PREVIOUS_LABEL];
    let mut rows: Vec<std::collections::HashMap<String, String>> = Vec::new();

    let mut push = |name: String, current: String, previous: String| {
        let mut row = std::collections::HashMap::new();
        row.insert("项目".to_string(), name);
        row.insert(CURRENT_LABEL.to_string(), current);
        row.insert(PREVIOUS_LABEL.to_string(), previous);
        rows.push(row);
    };

    push(
        format!("编制单位：{}", report.company_name),
        String::new(),
        String::new(),
    );
    push(
        format!("会计期间：{}", report.period_name),
        format!("单位：{}", report.unit),
        String::new(),
    );
    push(
        format!("数据来源：{}", report.source_note),
        String::new(),
        String::new(),
    );

    for section in &report.sections {
        push(section.activity_name.clone(), String::new(), String::new());
        for line in &section.inflow {
            push(
                format!("{INDENT}{}", line.item_name),
                report::money_text(Some(line.current)),
                report::money_text(line.previous),
            );
        }
        push(
            format!("{INDENT}现金流入小计"),
            report::money_text(Some(section.inflow_total.current)),
            report::money_text(section.inflow_total.previous),
        );
        for line in &section.outflow {
            push(
                format!("{INDENT}{}", line.item_name),
                report::money_text(Some(line.current)),
                report::money_text(line.previous),
            );
        }
        push(
            format!("{INDENT}现金流出小计"),
            report::money_text(Some(section.outflow_total.current)),
            report::money_text(section.outflow_total.previous),
        );
        push(
            format!("{INDENT}{}产生的现金流量净额", activity_net_name(&section.activity)),
            report::money_text(Some(section.net.current)),
            report::money_text(section.net.previous),
        );
    }

    push(
        "现金及现金等价物净增加额".to_string(),
        report::money_text(Some(report.net_increase.current)),
        report::money_text(report.net_increase.previous),
    );
    push(
        format!("加：{}", report.opening.item_name),
        report::money_text(report.opening.current),
        report::money_text(report.opening.previous),
    );
    push(
        report.closing.item_name.clone(),
        report::money_text(report.closing.current),
        report::money_text(report.closing.previous),
    );
    push(
        format!(
            "勾稽：{}（{}）",
            report.check.message,
            if report.check.balanced {
                "已闭合"
            } else {
                "未闭合"
            }
        ),
        String::new(),
        String::new(),
    );

    (HEADERS.to_vec(), rows)
}

/// 活动简称（拼「经营活动产生的现金流量净额」这类标题行）。
pub fn activity_net_name(activity: &str) -> &'static str {
    match activity {
        "OPERATING" => "经营活动",
        "INVESTING" => "投资活动",
        "FINANCING" => "筹资活动",
        _ => "其他活动",
    }
}

// ==================== 单测（纯逻辑 + 真库）====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cashflow_worksheet::{TotalsRes, WorksheetRowRes};
    use nik::prelude::chrono::NaiveDate;

    /// 真库用例的期间（`1913-xx` 是刻意选的死年份：模板库里没有，也不会与业务冲突）。
    const CURRENT_PERIOD: &str = "1913-05";
    /// 上期（`previous_period_of("1913-05")` = `1913-04`）。
    const PREVIOUS_PERIOD: &str = "1913-04";

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点数解析")
    }

    fn row(code: &str, name: &str, activity: &str, direction: &str, amount: &str, sort: i32) -> WorksheetRowRes {
        WorksheetRowRes {
            line_type: "FLOW".to_string(),
            item_code: code.to_string(),
            item_name: name.to_string(),
            activity: activity.to_string(),
            direction: direction.to_string(),
            amount: dec(amount),
            sort_order: sort,
            remark: None,
            entered: true,
            item_active: true,
        }
    }

    fn balance(kind: &str, name: &str, amount: &str) -> BalanceRes {
        BalanceRes {
            line_type: kind.to_string(),
            item_name: name.to_string(),
            amount: dec(amount),
            remark: None,
        }
    }

    /// 搭一张底稿（只关注报表关心的量：行 / 期初 / 期末 / 勾稽）。
    fn sheet(period: &str, rows: Vec<WorksheetRowRes>, opening: Option<&str>, closing: Option<&str>) -> WorksheetRes {
        let opening_row = opening.map(|amount| balance(LINE_OPENING, cashflow_worksheet::OPENING_NAME, amount));
        let closing_row = closing.map(|amount| balance(LINE_CLOSING, cashflow_worksheet::CLOSING_NAME, amount));
        let flows: Vec<cashflow_worksheet::FlowLine> = rows
            .iter()
            .map(|row| cashflow_worksheet::FlowLine {
                item_code: row.item_code.clone(),
                item_name: row.item_name.clone(),
                activity: row.activity.clone(),
                direction: row.direction.clone(),
                amount: row.amount,
            })
            .collect();
        let check = cashflow_worksheet::reconcile(
            opening_row.as_ref().map(|row| row.amount),
            &flows,
            closing_row.as_ref().map(|row| row.amount),
        );
        let inflow: Decimal = rows
            .iter()
            .filter(|row| row.direction == DIRECTION_INFLOW)
            .map(|row| row.amount)
            .sum();
        let outflow: Decimal = rows
            .iter()
            .filter(|row| row.direction == DIRECTION_OUTFLOW)
            .map(|row| row.amount)
            .sum();
        WorksheetRes {
            period: period.to_string(),
            period_status: "OPEN".to_string(),
            editable: true,
            rows,
            opening: opening_row,
            closing: closing_row,
            activities: Vec::new(),
            totals: TotalsRes {
                inflow,
                outflow,
                net: inflow - outflow,
            },
            check,
        }
    }

    // ---------- 纯逻辑 ----------

    /// ① 权限码：与 `V0417` 的 `sys_menu.auth_code` 逐字一致，且只有两个。
    #[test]
    fn permissions_are_well_formed() {
        assert_eq!(PERM_STATEMENT_LIST, "business:erp:cashflow-statement:list");
        assert_eq!(PERM_STATEMENT_EXPORT, "business:erp:cashflow-statement:export");
        assert_eq!(PERMISSIONS, [PERM_STATEMENT_LIST, PERM_STATEMENT_EXPORT]);
        assert_eq!(REPORT_CODE, "CASH_FLOW_WORKSHEET");
    }

    /// ② 三栏拼装：本期取本期底稿、上期取上期底稿；小计 / 净额 / 净增加额 / 余额都对得上。
    #[test]
    fn compose_puts_current_and_previous_in_two_columns() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![
                row("A", "销售商品收到的现金", "OPERATING", DIRECTION_INFLOW, "1200", 101),
                row("B", "购买商品支付的现金", "OPERATING", DIRECTION_OUTFLOW, "700", 201),
            ],
            Some("500"),
            Some("1000"),
        );
        let previous = sheet(
            PREVIOUS_PERIOD,
            vec![
                row("A", "销售商品收到的现金", "OPERATING", DIRECTION_INFLOW, "900", 101),
                row("B", "购买商品支付的现金", "OPERATING", DIRECTION_OUTFLOW, "400", 201),
            ],
            Some("0"),
            Some("500"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, Some(&previous), false);

        assert_eq!(body.sections.len(), 3, "经营 / 投资 / 筹资三块固定顺序");
        let operating = &body.sections[0];
        assert_eq!(operating.activity, "OPERATING");
        assert_eq!(operating.inflow.len(), 1);
        assert_eq!(operating.inflow[0].current, dec("1200"));
        assert_eq!(operating.inflow[0].previous, Some(dec("900")));
        assert_eq!(operating.inflow_total.current, dec("1200"));
        assert_eq!(operating.inflow_total.previous, Some(dec("900")));
        assert_eq!(operating.outflow_total.current, dec("700"));
        assert_eq!(operating.net.current, dec("500"));
        assert_eq!(operating.net.previous, Some(dec("500")));
        assert_eq!(body.net_increase.current, dec("500"));
        assert_eq!(body.net_increase.previous, Some(dec("500")));
        assert_eq!(body.opening.current, Some(dec("500")));
        assert_eq!(body.opening.previous, Some(dec("0")));
        assert_eq!(body.closing.current, Some(dec("1000")));
        assert_eq!(body.closing.previous, Some(dec("500")));
        assert!(body.check.balanced, "{}", body.check.message);
        assert_eq!(body.check.previous_balanced, Some(true));
        assert_eq!(body.check.diff, Some(Decimal::ZERO));
        assert!(body.check.message.contains("勾稽闭合"), "{}", body.check.message);
    }

    /// ③ 上期无底稿：整列 `null`（**不是 0**），并有告警点名上一期间。
    #[test]
    fn missing_previous_period_leaves_column_null() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![row("A", "流入行", "OPERATING", DIRECTION_INFLOW, "100", 101)],
            Some("0"),
            Some("100"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, None, false);
        assert!(body.check.balanced);
        assert_eq!(body.net_increase.previous, None);
        assert_eq!(body.opening.previous, None);
        assert_eq!(body.closing.previous, None);
        assert_eq!(body.sections[0].inflow_total.previous, None);
        assert_eq!(
            body.sections[0].inflow[0].previous, None,
            "上期没编 ⇒ 该行上期金额为 null"
        );
        assert!(
            body.warnings.iter().any(|warning| warning.contains("1913-04")),
            "上期未开账必须点名：{:?}",
            body.warnings
        );
    }

    /// ④ 只在上期存在的行也要出（本期 0 / 未录），且吼一声不静默丢数。
    #[test]
    fn previous_only_rows_survive() {
        let current = sheet(CURRENT_PERIOD, vec![], Some("0"), Some("0"));
        let previous = sheet(
            PREVIOUS_PERIOD,
            vec![row("OLD", "上期才有的项目", "INVESTING", DIRECTION_OUTFLOW, "30", 351)],
            Some("0"),
            Some("0"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, Some(&previous), false);
        let investing = &body.sections[1];
        let line = investing
            .outflow
            .iter()
            .find(|line| line.item_code == "OLD")
            .expect("上期独有的行不能消失");
        assert_eq!(line.current, Decimal::ZERO);
        assert!(!line.current_entered);
        assert_eq!(line.previous, Some(dec("30")));
        assert_eq!(investing.outflow_total.previous, Some(dec("30")), "上期小计要含它");
        assert!(
            body.warnings
                .iter()
                .any(|warning| warning.contains("只在上期底稿里存在")),
            "{:?}",
            body.warnings
        );
    }

    /// ⑤ 活动 / 方向跨期不一致：上期金额按本期归属列示，并点名。
    #[test]
    fn cross_period_direction_change_is_reported() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![row("X", "换过方向的项目", "OPERATING", DIRECTION_OUTFLOW, "10", 201)],
            Some("0"),
            Some("-10"),
        );
        let previous = sheet(
            PREVIOUS_PERIOD,
            vec![row("X", "换过方向的项目", "OPERATING", DIRECTION_INFLOW, "5", 201)],
            Some("0"),
            Some("5"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, Some(&previous), false);
        let operating = &body.sections[0];
        assert!(operating.inflow.is_empty(), "本期是流出 ⇒ 不进流入区");
        assert_eq!(operating.outflow[0].previous, Some(dec("5")), "上期金额按本期归属列示");
        assert!(
            body.warnings.iter().any(|warning| warning.contains("活动 / 方向")),
            "{:?}",
            body.warnings
        );
    }

    /// ⑥ 差 0.0001 就不闭合（绝不四舍五入），`diff` 精确给出，导出闸 400 + 66087。
    #[test]
    fn check_is_exact_and_gates_export() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![row("A", "流入行", "OPERATING", DIRECTION_INFLOW, "100.0001", 101)],
            Some("0"),
            Some("100"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, None, false);
        assert!(!body.check.balanced);
        assert_eq!(body.check.diff, Some(dec("0.0001")));
        assert!(!body.check.arithmetic_ok);

        let statement = CashStatementRes {
            report_code: REPORT_CODE.to_string(),
            report_name: REPORT_NAME.to_string(),
            company_name: "云枢".to_string(),
            unit: "元".to_string(),
            period: CURRENT_PERIOD.to_string(),
            period_name: report::period_name_of(CURRENT_PERIOD),
            period_status: "OPEN".to_string(),
            period_start: NaiveDate::from_ymd_opt(1913, 5, 1).expect("起始日"),
            period_end: NaiveDate::from_ymd_opt(1913, 5, 31).expect("结束日"),
            previous_period: None,
            previous_period_status: None,
            current_label: CURRENT_LABEL.to_string(),
            previous_label: PREVIOUS_LABEL.to_string(),
            generated_at: Utc::now(),
            source: SOURCE_WORKSHEET.to_string(),
            source_note: SOURCE_NOTE.to_string(),
            sections: body.sections,
            net_increase: body.net_increase,
            opening: body.opening,
            closing: body.closing,
            check: body.check,
            line_count: body.line_count,
            hidden_zero_lines: body.hidden_zero_lines,
            warnings: body.warnings,
        };
        let error = ensure_exportable(&statement).expect_err("未闭合必须拦");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED);
        assert!(error.msg.contains("未闭合"), "{}", error.msg);
        assert!(error.msg.contains("0.0001"), "报文要带精确差额：{}", error.msg);
    }

    /// ⑦ 缺期初 / 缺期末：余额列 `null`、勾稽不闭合、导出被拦（0 与「忘了录」不是一个意思）。
    #[test]
    fn missing_balances_are_null_not_zero() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![row("A", "流入行", "OPERATING", DIRECTION_INFLOW, "100", 101)],
            None,
            None,
        );
        let body = compose_statement(CURRENT_PERIOD, &current, None, false);
        assert_eq!(body.opening.current, None);
        assert_eq!(body.closing.current, None);
        assert_eq!(body.check.diff, None);
        assert!(!body.check.balanced);
        assert!(!body.check.arithmetic_ok);
        assert!(
            body.check
                .current
                .problems
                .iter()
                .any(|problem| problem.code == "OPENING_MISSING"),
            "缺期初要点名"
        );
    }

    /// ⑧ `hide_zero` 只隐藏「两栏 0 且都没录」的行；录过 0 的行必须留着。
    #[test]
    fn hide_zero_keeps_entered_zero_rows() {
        let mut not_entered = row("B", "没录的项目", "OPERATING", DIRECTION_OUTFLOW, "0", 201);
        not_entered.entered = false;
        let entered_zero = row("C", "录过 0 的项目", "OPERATING", DIRECTION_OUTFLOW, "0", 202);
        let current = sheet(
            CURRENT_PERIOD,
            vec![
                row("A", "有金额", "OPERATING", DIRECTION_INFLOW, "100", 101),
                not_entered,
                entered_zero,
            ],
            Some("0"),
            Some("100"),
        );
        let shown = compose_statement(CURRENT_PERIOD, &current, None, false);
        assert_eq!(shown.sections[0].outflow.len(), 2);
        assert_eq!(shown.hidden_zero_lines, 0);

        let hidden = compose_statement(CURRENT_PERIOD, &current, None, true);
        assert_eq!(hidden.sections[0].outflow.len(), 1, "只留下「录过 0」的那行");
        assert_eq!(hidden.hidden_zero_lines, 1);
        assert_eq!(
            hidden.sections[0].outflow_total.current,
            Decimal::ZERO,
            "小计不受隐藏影响"
        );
    }

    /// ⑨ 导出表格：三栏表头 + 小计 / 净额 / 期初期末行都在，上期为 `null` 时给空串。
    #[test]
    fn export_table_is_three_columns() {
        let current = sheet(
            CURRENT_PERIOD,
            vec![
                row("A", "销售商品收到的现金", "OPERATING", DIRECTION_INFLOW, "1200", 101),
                row("B", "购买商品支付的现金", "OPERATING", DIRECTION_OUTFLOW, "700", 201),
            ],
            Some("500"),
            Some("1000"),
        );
        let body = compose_statement(CURRENT_PERIOD, &current, None, false);
        let statement = CashStatementRes {
            report_code: REPORT_CODE.to_string(),
            report_name: REPORT_NAME.to_string(),
            company_name: "云枢".to_string(),
            unit: "元".to_string(),
            period: CURRENT_PERIOD.to_string(),
            period_name: report::period_name_of(CURRENT_PERIOD),
            period_status: "OPEN".to_string(),
            period_start: NaiveDate::from_ymd_opt(1913, 5, 1).expect("起始日"),
            period_end: NaiveDate::from_ymd_opt(1913, 5, 31).expect("结束日"),
            previous_period: None,
            previous_period_status: None,
            current_label: CURRENT_LABEL.to_string(),
            previous_label: PREVIOUS_LABEL.to_string(),
            generated_at: Utc::now(),
            source: SOURCE_WORKSHEET.to_string(),
            source_note: SOURCE_NOTE.to_string(),
            sections: body.sections,
            net_increase: body.net_increase,
            opening: body.opening,
            closing: body.closing,
            check: body.check,
            line_count: body.line_count,
            hidden_zero_lines: body.hidden_zero_lines,
            warnings: body.warnings,
        };
        let (headers, rows) = export_table(&statement);
        assert_eq!(headers, vec!["项目", "本期金额", "上期金额"]);
        let names: Vec<&str> = rows.iter().map(|row| row["项目"].as_str()).collect();
        assert!(names.iter().any(|name| name.contains("一、经营活动产生的现金流量")));
        assert!(names.iter().any(|name| name.contains("现金流入小计")));
        assert!(names.iter().any(|name| name.contains("现金流出小计")));
        assert!(names.iter().any(|name| name.contains("经营活动产生的现金流量净额")));
        assert!(names.contains(&"现金及现金等价物净增加额"));
        assert!(names.iter().any(|name| name.contains("期初现金及现金等价物余额")));
        assert!(names.iter().any(|name| name.contains("期末现金及现金等价物余额")));
        let total_row = rows
            .iter()
            .find(|row| row["项目"].contains("现金流入小计"))
            .expect("流入小计行");
        assert_eq!(total_row["本期金额"], "1,200.00");
        assert_eq!(total_row["上期金额"], "", "上期无底稿 ⇒ 空串，不印 0.00");
    }

    // ---------- 真库 ----------

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        reset().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清场 + 铺两个期间（本期 OPEN、上期 OPEN）。
    async fn reset() {
        for sql in [
            "DELETE FROM biz_cashflow_worksheet WHERE period_code IN ('1913-05', '1913-04')",
            "DELETE FROM biz_accounting_period WHERE period_code IN ('1913-05', '1913-04')",
        ] {
            db().execute_with_params(sql, Vec::new())
                .await
                .unwrap_or_else(|error| panic!("清场失败（{sql}）：{error}"));
        }
        for period in [PREVIOUS_PERIOD, CURRENT_PERIOD] {
            let year: i32 = period[..4].parse().expect("年");
            let month: u32 = period[5..].parse().expect("月");
            let start = NaiveDate::from_ymd_opt(year, month, 1).expect("期间起始日");
            let next_first = if month == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)
            }
            .expect("下月一日");
            let end = next_first.pred_opt().expect("期间结束日");
            db().execute_with_params(
                "INSERT INTO biz_accounting_period \
                     (id, period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (gen_random_uuid(), ?, ?, ?, ?::text::date, ?::text::date, 'OPEN')",
                vec![
                    Value::I16(year as i16),
                    Value::I16(month as i16),
                    Value::Text(period.to_string()),
                    Value::Text(start.to_string()),
                    Value::Text(end.to_string()),
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("铺期间 {period} 失败：{error}"));
        }
    }

    /// 真库保存一张底稿（走 M1-14 的写入口，保证报表读到的就是底稿的真实形状）。
    async fn save(period: &str, items: Vec<(&str, &str)>, opening: Option<&str>, closing: Option<&str>) {
        let req = cashflow_worksheet::SaveWorksheetReq {
            period: period.to_string(),
            opening: opening.map(|amount| cashflow_worksheet::BalanceIn {
                amount: dec(amount),
                remark: None,
            }),
            closing: closing.map(|amount| cashflow_worksheet::BalanceIn {
                amount: dec(amount),
                remark: None,
            }),
            items: items
                .into_iter()
                .map(|(code, amount)| cashflow_worksheet::WorksheetItemIn {
                    item_code: code.to_string(),
                    amount: dec(amount),
                    remark: None,
                })
                .collect(),
        };
        cashflow_worksheet::save_sheet(&req, None)
            .await
            .unwrap_or_else(|error| panic!("保存 {period} 底稿失败：{} {}", error.code, error.msg));
    }

    /// ⑩ 真库出表：本期 + 上期两张底稿 → 三栏报表，金额 / 小计 / 净增加额 / 余额逐项对账。
    #[tokio::test]
    async fn statement_reads_two_periods_from_real_worksheet() {
        let Some(_guard) = setup().await else {
            return;
        };
        save(
            PREVIOUS_PERIOD,
            vec![
                ("OPER_SALES_RECEIPTS", "900.0000"),
                ("OPER_PURCHASE_PAYMENTS", "400.0000"),
            ],
            Some("0.0000"),
            Some("500.0000"),
        )
        .await;
        save(
            CURRENT_PERIOD,
            vec![
                ("OPER_SALES_RECEIPTS", "1200.0000"),
                ("OPER_PURCHASE_PAYMENTS", "700.0000"),
            ],
            Some("500.0000"),
            Some("1000.0000"),
        )
        .await;

        let statement = snapshot_or_error(CURRENT_PERIOD, false).await.expect("真库出表");
        assert_eq!(statement.period, CURRENT_PERIOD);
        assert_eq!(statement.previous_period.as_deref(), Some(PREVIOUS_PERIOD));
        assert_eq!(statement.source, SOURCE_WORKSHEET);
        assert!(statement.check.balanced, "{}", statement.check.message);

        let operating = statement
            .sections
            .iter()
            .find(|section| section.activity == "OPERATING")
            .expect("经营活动分区");
        let sales = operating
            .inflow
            .iter()
            .find(|line| line.item_code == "OPER_SALES_RECEIPTS")
            .expect("销售收款行");
        assert_eq!(sales.current, dec("1200"));
        assert_eq!(sales.previous, Some(dec("900")));
        assert_eq!(sales.item_name, "销售商品、提供劳务收到的现金", "快照取自配置");
        let purchase = operating
            .outflow
            .iter()
            .find(|line| line.item_code == "OPER_PURCHASE_PAYMENTS")
            .expect("采购付款行");
        assert_eq!(purchase.current, dec("700"));
        assert_eq!(purchase.previous, Some(dec("400")));
        assert_eq!(operating.net.current, dec("500"));
        assert_eq!(operating.net.previous, Some(dec("500")));
        assert_eq!(statement.net_increase.current, dec("500"));
        assert_eq!(statement.opening.current, Some(dec("500")));
        assert_eq!(statement.closing.current, Some(dec("1000")));
        assert_eq!(statement.opening.previous, Some(dec("0")));
        assert_eq!(statement.closing.previous, Some(dec("500")));
        // 没录的配置行按 0 列示但 entered = false
        let unentered = statement
            .sections
            .iter()
            .flat_map(|section| section.inflow.iter().chain(section.outflow.iter()))
            .filter(|line| !line.current_entered)
            .count();
        assert!(unentered > 0, "配置里还有没录的项目，必须列示并标 entered=false");
    }

    /// ⑪ 真库：本期不闭合 → GET 照常（200 语义）+ 导出闸 400 + 66087；期间不存在 → 66010。
    #[tokio::test]
    async fn unbalanced_worksheet_blocks_export_but_not_read() {
        let Some(_guard) = setup().await else {
            return;
        };
        save(
            PREVIOUS_PERIOD,
            vec![("OPER_SALES_RECEIPTS", "900.0000")],
            Some("0.0000"),
            Some("900.0000"),
        )
        .await;
        save(
            CURRENT_PERIOD,
            vec![("OPER_SALES_RECEIPTS", "1200.0000")],
            Some("0.0000"),
            Some("1100.0000"),
        )
        .await;

        let statement = snapshot_or_error(CURRENT_PERIOD, false)
            .await
            .expect("未闭合也能读（页面要能打开去补）");
        assert!(!statement.check.balanced);
        assert_eq!(statement.check.diff, Some(dec("100")));
        assert!(!statement.check.current.problems.is_empty(), "问题逐条点名");
        let error = ensure_exportable(&statement).expect_err("未闭合不许出表");
        assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED);
        assert!(error.msg.contains(CURRENT_PERIOD), "{}", error.msg);

        let missing = snapshot_or_error("1913-09", false).await.expect_err("不存在的期间");
        assert_eq!(missing.status, 400);
        assert_eq!(missing.code, error_code::ERP_PERIOD_NOT_FOUND);
    }

    /// ⑫ 真库：上期期间不存在 ⇒ 上期整列 null + 告警（绝不按 0 或按 1 推导）。
    #[tokio::test]
    async fn absent_previous_period_is_reported() {
        let Some(_guard) = setup().await else {
            return;
        };
        db().execute_with_params(
            "DELETE FROM biz_accounting_period WHERE period_code = '1913-04'",
            Vec::new(),
        )
        .await
        .expect("删掉上期期间行");
        save(
            CURRENT_PERIOD,
            vec![("OPER_SALES_RECEIPTS", "1200.0000")],
            Some("0.0000"),
            Some("1200.0000"),
        )
        .await;

        let statement = snapshot_or_error(CURRENT_PERIOD, false)
            .await
            .expect("上期缺失不影响本期出表");
        assert!(statement.check.balanced, "{}", statement.check.message);
        assert_eq!(statement.previous_period, None);
        assert_eq!(statement.opening.previous, None);
        assert_eq!(statement.closing.previous, None);
        let sales = statement
            .sections
            .iter()
            .flat_map(|section| section.inflow.iter())
            .find(|line| line.item_code == "OPER_SALES_RECEIPTS")
            .expect("销售收款行");
        assert_eq!(sales.previous, None);
        assert!(
            statement.warnings.iter().any(|warning| warning.contains("没有期间行")),
            "{:?}",
            statement.warnings
        );
    }

    /// ⑬ 真库：`hide_zero=true` 隐藏未录入的零行，小计不变。
    #[tokio::test]
    async fn hide_zero_drops_empty_config_rows() {
        let Some(_guard) = setup().await else {
            return;
        };
        save(
            CURRENT_PERIOD,
            vec![("OPER_SALES_RECEIPTS", "1200.0000")],
            Some("1200.0000"),
            Some("2400.0000"),
        )
        .await;
        let full = snapshot_or_error(CURRENT_PERIOD, false).await.expect("全量");
        let lean = snapshot_or_error(CURRENT_PERIOD, true).await.expect("隐藏零行");
        assert!(lean.line_count < full.line_count, "隐藏后行数必须变少");
        assert_eq!(lean.hidden_zero_lines, full.line_count - lean.line_count);
        assert_eq!(
            lean.net_increase.current, full.net_increase.current,
            "隐藏零行不许改变小计"
        );
        let operating_full = &full.sections[0];
        let operating_lean = &lean.sections[0];
        assert_eq!(operating_full.inflow_total, operating_lean.inflow_total);
        assert_eq!(lean.check.balanced, full.check.balanced);
    }

    /// ⑭ 真库：底稿里配置已停用的行仍出表（`item_active = false`），并有告警。
    #[tokio::test]
    async fn inactive_item_still_appears() {
        let Some(_guard) = setup().await else {
            return;
        };
        const SCRATCH: &str = "M116_SCRATCH_ITEM";
        db().execute_with_params(
            "DELETE FROM biz_cashflow_item WHERE item_code = ?",
            vec![Value::Text(SCRATCH.to_string())],
        )
        .await
        .expect("清临时配置");
        db().execute_with_params(
            "INSERT INTO biz_cashflow_item \
                 (id, item_code, item_name, activity, direction, account_pattern, sort_order, is_active) \
             VALUES (gen_random_uuid(), ?, '本卡临时项目', 'OPERATING', 'INFLOW', 'T_116', 902, 1)",
            vec![Value::Text(SCRATCH.to_string())],
        )
        .await
        .expect("铺临时配置");
        save(
            CURRENT_PERIOD,
            vec![("OPER_SALES_RECEIPTS", "100.0000"), (SCRATCH, "50.0000")],
            Some("0.0000"),
            Some("150.0000"),
        )
        .await;
        db().execute_with_params(
            "UPDATE biz_cashflow_item SET is_active = 0 WHERE item_code = ?",
            vec![Value::Text(SCRATCH.to_string())],
        )
        .await
        .expect("停用临时配置");

        let statement = snapshot_or_error(CURRENT_PERIOD, false).await.expect("出表");
        let line = statement
            .sections
            .iter()
            .flat_map(|section| section.inflow.iter())
            .find(|line| line.item_code == SCRATCH)
            .expect("停用后历史行不能消失");
        assert_eq!(line.current, dec("50"));
        assert!(!line.item_active);
        assert!(
            statement.warnings.iter().any(|warning| warning.contains("停用")),
            "{:?}",
            statement.warnings
        );
        db().execute_with_params(
            "UPDATE biz_cashflow_item SET is_active = 1 WHERE item_code = ?",
            vec![Value::Text(SCRATCH.to_string())],
        )
        .await
        .expect("恢复临时配置");
        db().execute_with_params(
            "DELETE FROM biz_cashflow_item WHERE item_code = ?",
            vec![Value::Text(SCRATCH.to_string())],
        )
        .await
        .expect("清临时配置");
    }
}
