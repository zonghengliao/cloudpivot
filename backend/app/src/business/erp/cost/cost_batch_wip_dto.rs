//! 分批法「在制批次余额」· 契约层与纯逻辑（成本核算 · M5-05）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §4.6（在制与完工：分批法下未完工批次的投入全部留在在制）、
//! §4.11（C-COST-3：4001 期末余额 = 在制合计）、
//! 以及 V0461 表注释里钉死的口径「**在制余额 = 累计归集 − Σ 结转单活行**」。
//!
//! # 这个模块回答什么问题
//!
//! 一批货**没完工**（或完工了但没结转）时，它已经在料 / 工 / 费上花了多少钱、
//! 其中多少已经转出去、**还剩多少挂在在制**：
//!
//! ```text
//! 在制余额（截止期间 P） = Σ 归集(period_code ≤ P) − Σ 结转(period_code ≤ P)   ← 逐要素
//! 期末余额（期间 P）     = 期初余额 + 本期投入 − 本期转出
//! ```
//!
//! 这两条是同一个数的两种看法：**累计式**回答「这批现在挂着多少」，
//! **滚动式**回答「这个月动了多少」。两个接口各给一种，数字必须自洽 ——
//! 单测里逐条钉住（`closing_at_cutoff_equals_cumulative`）。
//!
//! # 为什么余额**不夹零**
//!
//! M5-04 结转时按要素夹零（`本次 = max(0, 累计 − 已结转)`，不让转出为负）。
//! 但**报表**不能夹零：余额一旦是负数，说明「转出去的比归集进来的还多」——
//! 只可能来自归集行事后被作废 / 软删（领料单作废、工时被打回），或者有人直接改库。
//! 夹成 0 会把这件事藏起来，账上看不出来；显示负数 + 打 [`WARN_NEGATIVE_BALANCE`]
//! 才让人知道「这批要查」。所以 DTO 层的 [`balance_of`] **原样相减**，夹零只发生在
//! 写入侧（M5-04）。
//!
//! # 为什么本卡只有读接口
//!
//! 「未完工留余额」不需要写任何东西 —— 不结转，成本自然留在在制。真正要写的是
//! M5-03（归集进来）与 M5-04（结转出去），两张卡都已经落地。本卡再开一个
//! 「改余额」的口子会立刻变成第二本账：余额是**推出来的**，能改就等于能凭空造在制。
//! 与 M5-02（归集表）同款：只读视图，写入口在单据侧。
//!
//! # 数据权限
//!
//! 与归集表 / 结转单**同一份**范围（`biz_cost_object.workshop_dept_id`），
//! 权限码也**复用** `business:erp:cost-object:list`：本接口不多露一列别人车间的成本。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_batch_collect_dto as collect_dto;
use crate::business::erp::cost::cost_batch_finish_dto::Amounts;
use crate::business::erp::cost::cost_collect_sheet_dto::{self as sheet_dto, SheetBatchRes};
use crate::business::erp::cost::cost_object_dto;

// ==================== 权限码（复用） ====================

/// 读在制余额（**复用** `business:erp:cost-object:list`）。
///
/// 理由与 M5-02 / M5-03 / M5-04 完全一致：本接口的每一行都是一个成本对象，
/// 每一列都能从「成本对象列表 + 归集明细 + 结转单」用同一个码读出来；
/// 新开码只会多一条指向 M5-07 页面的死菜单。
pub const PERM_READ: &str = collect_dto::PERM_READ;

/// 本模块全部权限码。
pub const PERMISSIONS: [&str; 1] = [PERM_READ];

// ==================== 值域 / 标度 ====================

/// 金额标度（`NUMERIC(18,4)`，与归集表 / 结转单同标度）。
pub const AMOUNT_SCALE: u32 = 4;

/// 台账最多回放多少个有发生的期间（防御：某个夹具把首笔归集造在 1900 年时，
/// 不能一次吐 1500 行）。超了就从**最早**开始丢，并把丢掉的部分折进期初余额
/// —— 期初 / 期末仍然对得上，只是不再逐月列出远古空转期间。
pub const MAX_LEDGER_PERIODS: usize = 120;

/// 排序：在制余额倒序（默认 —— 打开就看「哪批押着最多钱」）。
pub const SORT_BALANCE: &str = "balance";
/// 排序：批次号倒序。
pub const SORT_OBJECT: &str = "object_no";
/// 排序：生产订单号倒序。
pub const SORT_ORDER: &str = "order_no";
/// 排序：开工日期倒序。
pub const SORT_START: &str = "start_date";

/// 排序白名单（用户输入永远不进 SQL 文本，见 [`order_by_sql`]）。
pub const SORTS: [&str; 4] = [SORT_BALANCE, SORT_OBJECT, SORT_ORDER, SORT_START];

/// 默认排序。
pub const DEFAULT_SORT: &str = SORT_BALANCE;

// ==================== 警告码（不是错误码：只读接口不拦人） ====================

/// 某要素「已结转 > 累计归集」：余额为负，账要查。
pub const WARN_NEGATIVE_BALANCE: &str = "NEGATIVE_BALANCE";
/// 整批已完工（`wip_qty = 0`、`completed_qty > 0`）但批次还是 `OPEN`，余额还挂着。
pub const WARN_FINISHED_NOT_TRANSFERRED: &str = "FINISHED_NOT_TRANSFERRED";
/// 还有在制数量（`wip_qty > 0`）但余额是 0：成本缺口（料没归集 / 工没报 / 费没摊）。
pub const WARN_WIP_WITHOUT_COST: &str = "WIP_WITHOUT_COST";
/// 已关批（`CLOSED`）却还有余额。
pub const WARN_CLOSED_WITH_BALANCE: &str = "CLOSED_WITH_BALANCE";
/// 已作废（`CANCELLED`）却还有余额：作废批的在制必须清掉，否则在制虚高。
pub const WARN_CANCELLED_WITH_BALANCE: &str = "CANCELLED_WITH_BALANCE";
/// 截止期间之后还有归集 / 结转：本表是「截至 P」的快照，不是现在。
pub const WARN_LATER_PERIOD_ACTIVITY: &str = "LATER_PERIOD_ACTIVITY";
/// 本批已随**期间结算**（约当产量）算过：期间结算口径的在制与批次口径可能不等。
pub const WARN_SETTLED_BY_PERIOD: &str = "SETTLED_BY_PERIOD";

/// 级别：必须看的异常。
pub const LEVEL_WARN: &str = "WARN";
/// 级别：解释性说明（不改数，但别让人误会）。
pub const LEVEL_INFO: &str = "INFO";

// ==================== 入参 ====================

/// `GET /business/erp/cost-collect-sheet/wip-balance` 的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WipQuery {
    /// 会计期间 `yyyy-MM`（**必填**：余额是「截止某期」的量）。
    pub period_code: Option<String>,
    /// 关键字：批次号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称（模糊）。
    pub keyword: Option<String>,
    /// 生产车间（精确；`sys_dept.id`）。
    pub workshop_dept_id: Option<String>,
    /// 批次状态（`ALL` / `OPEN` / `CLOSED` / `CANCELLED`；默认 `ALL`）。
    pub status: Option<String>,
    /// 只看还有余额的批次（`true`）；不传 = 全部（含余额已归零的批次）。
    pub only_wip: Option<bool>,
    /// 排序（[`SORTS`] 之一；默认 `balance`）。
    pub sort: Option<String>,
    /// 页码（从 1 开始；缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省取模块默认值，上限 200）。
    pub page_size: Option<u64>,
}

/// `GET /business/erp/cost-collect-sheet/{object_id}/wip-ledger` 的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WipLedgerQuery {
    /// 截止会计期间 `yyyy-MM`（**必填**）。
    pub period_code: Option<String>,
}

/// 归一化后的筛选条件。
#[derive(Debug, Clone)]
pub struct WipFilter {
    /// 截止期间 `yyyy-MM`（已校验）。
    pub period_code: String,
    /// 关键字（已 trim；`None` = 不筛）。
    pub keyword: Option<String>,
    /// 生产车间。
    pub workshop_dept_id: Option<Uuid>,
    /// 状态（`None` = 不限）。
    pub status: Option<String>,
    /// 只看有余额。
    pub only_wip: bool,
    /// 排序键。
    pub sort: String,
    /// 页码（≥ 1）。
    pub page: u64,
    /// 每页条数（1–200）。
    pub page_size: u64,
}

// ==================== 归一化（纯逻辑） ====================

/// 期间必填 + `yyyy-MM` 形状校验（**复用**归集表那一份，报文也同一句）。
pub fn period_of(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    sheet_dto::period_of(raw)
}

/// 列表入参 → 归一化筛选。
pub fn filter_of(query: &WipQuery) -> std::result::Result<WipFilter, ApiError> {
    let period_code = period_of(query.period_code.as_deref())?;

    let status = match cost_object_dto::clean_optional(query.status.clone()) {
        None => None,
        Some(raw) => {
            let upper = raw.to_ascii_uppercase();
            if !sheet_dto::STATUSES.contains(&upper.as_str()) {
                return Err(cost_object_dto::bad_request(format!(
                    "status 只认 {:?}（收到「{raw}」）；ALL = 不限状态",
                    sheet_dto::STATUSES
                )));
            }
            if upper == sheet_dto::STATUS_ALL {
                None
            } else {
                Some(upper)
            }
        }
    };

    let sort = match cost_object_dto::clean_optional(query.sort.clone()) {
        None => DEFAULT_SORT.to_string(),
        Some(raw) => {
            let lower = raw.to_ascii_lowercase();
            if !SORTS.contains(&lower.as_str()) {
                return Err(cost_object_dto::bad_request(format!(
                    "sort 只认 {SORTS:?}（收到「{raw}」）"
                )));
            }
            lower
        }
    };

    let (page, page_size) =
        crate::business::erp::model::normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));

    Ok(WipFilter {
        period_code,
        keyword: cost_object_dto::clean_optional(query.keyword.clone()),
        workshop_dept_id: cost_object_dto::parse_opt_uuid(query.workshop_dept_id.as_deref(), "workshop_dept_id")?,
        status,
        only_wip: query.only_wip.unwrap_or(false),
        sort,
        page,
        page_size,
    })
}

/// 排序键 → `ORDER BY` 原文（**常量返回**，用户输入永不进 SQL）。
///
/// 每个排序都以 `t.object_no DESC, t.id ASC` 收尾：没有全序时翻页会漏行 / 重复行。
pub fn order_by_sql(sort: &str) -> &'static str {
    match sort {
        SORT_OBJECT => "ORDER BY t.object_no DESC, t.id ASC",
        SORT_ORDER => "ORDER BY COALESCE(t.source_doc_no, '') DESC, t.object_no DESC, t.id ASC",
        SORT_START => "ORDER BY t.start_date DESC, t.object_no DESC, t.id ASC",
        _ => concat!(
            "ORDER BY (",
            "COALESCE(c.mat_amount, 0) + COALESCE(c.labor_amount, 0) + COALESCE(c.oh_amount, 0)",
            " - COALESCE(f.mat_amount, 0) - COALESCE(f.labor_amount, 0) - COALESCE(f.oh_amount, 0)",
            ") DESC, t.object_no DESC, t.id ASC"
        ),
    }
}

// ==================== 金额运算（纯值对象） ====================

/// 在制余额 = 累计归集 − 累计结转（**逐要素，不夹零**，见模块头）。
pub fn balance_of(collected: &Amounts, transferred: &Amounts) -> Amounts {
    Amounts {
        material: collected.material - transferred.material,
        labor: collected.labor - transferred.labor,
        overhead: collected.overhead - transferred.overhead,
    }
}

/// 三要素是否有任一为负（余额为负 = 异常，见 [`WARN_NEGATIVE_BALANCE`]）。
pub fn any_negative(amounts: &Amounts) -> bool {
    amounts.material < Decimal::ZERO || amounts.labor < Decimal::ZERO || amounts.overhead < Decimal::ZERO
}

/// 逐要素相加（期初 + 本期投入 − 本期转出 的滚动式）。
pub fn plus(left: &Amounts, right: &Amounts) -> Amounts {
    Amounts {
        material: left.material + right.material,
        labor: left.labor + right.labor,
        overhead: left.overhead + right.overhead,
    }
}

/// 逐要素相减。
pub fn minus(left: &Amounts, right: &Amounts) -> Amounts {
    Amounts {
        material: left.material - right.material,
        labor: left.labor - right.labor,
        overhead: left.overhead - right.overhead,
    }
}

// ==================== 台账（滚动式） ====================

/// 一个期间的流量（归集进来 / 结转出去）。
#[derive(Debug, Clone, Default)]
pub struct PeriodFlow {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 本期投入（三要素）。
    pub inflow: Amounts,
    /// 本期投入的归集行数。
    pub collect_lines: i64,
    /// 本期转出（三要素）。
    pub outflow: Amounts,
    /// 本期结转次数。
    pub transfer_times: i64,
}

/// 台账的一行（一个期间）。
#[derive(Debug, Clone, Serialize)]
pub struct LedgerPeriodRes {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 期初余额（= 本期间开始前的累计余额）。
    pub opening: WipAmountsRes,
    /// 本期投入。
    pub inflow: WipAmountsRes,
    /// 本期转出。
    pub outflow: WipAmountsRes,
    /// 期末余额 = 期初 + 本期投入 − 本期转出。
    pub closing: WipAmountsRes,
    /// 本期归集行数（0 = 本月该批一条都没归集）。
    pub collect_lines: i64,
    /// 本期结转次数。
    pub transfer_times: i64,
}

/// 台账的滚动结果。
#[derive(Debug, Clone)]
pub struct LedgerSeries {
    /// 第一行之前（被截断掉的那段）的余额。
    pub opening: Amounts,
    /// 逐期间行（升序，末行一定是截止期间）。
    pub rows: Vec<LedgerPeriodRes>,
    /// 最后一行的期末余额（= 截止期间的累计余额）。
    pub closing: Amounts,
    /// 是否因为超过 [`MAX_LEDGER_PERIODS`] 而丢掉了最早的若干期间。
    pub truncated: bool,
}

/// 两份按期间聚合的流量合成一条期间轴（同期间相加，升序）。
///
/// 归集与结转是两个来源、两张表：同一个月可能只有归集、只有结转、或两者都有。
/// 台账必须把它们**对齐到期间轴**上，否则「期初 = 上期期末」在缺月的月份就断了。
pub fn merge_flows(collect: Vec<PeriodFlow>, finish: Vec<PeriodFlow>) -> Vec<PeriodFlow> {
    let mut all = collect;
    all.extend(finish);
    all.sort_by(|left, right| left.period_code.cmp(&right.period_code));
    let mut merged: Vec<PeriodFlow> = Vec::with_capacity(all.len());
    for flow in all {
        match merged.last_mut() {
            Some(last) if last.period_code == flow.period_code => {
                last.inflow = plus(&last.inflow, &flow.inflow);
                last.collect_lines += flow.collect_lines;
                last.outflow = plus(&last.outflow, &flow.outflow);
                last.transfer_times += flow.transfer_times;
            }
            _ => merged.push(flow),
        }
    }
    merged
}

/// 期间序列：`period` 的下一期。形状非法（`2026-1` / `2026-13` / 空）返回 `None`。
pub fn next_period(period: &str) -> Option<String> {
    use nik::prelude::chrono::Datelike;
    let (start, _) = crate::business::erp::ledger::month_range(period)?;
    let next = if start.month() == 12 {
        nik::prelude::chrono::NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)?
    } else {
        nik::prelude::chrono::NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)?
    };
    Some(format!("{:04}-{:02}", next.year(), next.month()))
}

/// 把期间流量回放成「期初 / 投入 / 转出 / 期末」的滚动序列。
///
/// * `flows`：已按期间升序合并好的流量，**必须**只含截止期间及以前的行；
/// * `cutoff`：截止期间 —— 即使该月没有任何发生，也要补一行 0，
///   否则用户会以为「这个月的数据丢了」；
/// * 超过 [`MAX_LEDGER_PERIODS`] 时从最早丢起，丢掉的部分折进 [`LedgerSeries::opening`]
///   （期初 / 期末仍然精确，只是不再逐月列出远古期间）。
pub fn ledger_series(flows: Vec<PeriodFlow>, cutoff: &str) -> LedgerSeries {
    let mut flows = flows;
    flows.retain(|flow| flow.period_code.as_str() <= cutoff);
    if !flows.iter().any(|flow| flow.period_code == cutoff) {
        flows.push(PeriodFlow {
            period_code: cutoff.to_string(),
            ..Default::default()
        });
        flows.sort_by(|left, right| left.period_code.cmp(&right.period_code));
    }

    let mut truncated = false;
    let mut running = Amounts::default();
    while flows.len() > MAX_LEDGER_PERIODS {
        // 丢掉最早的一期并把它的净流量折进 running —— 期初余额因此仍然精确。
        let dropped = flows.remove(0);
        running = plus(&running, &minus(&dropped.inflow, &dropped.outflow));
        truncated = true;
    }
    let opening = running;

    let mut rows = Vec::with_capacity(flows.len());
    for flow in flows {
        let period_opening = running;
        let closing = plus(&period_opening, &minus(&flow.inflow, &flow.outflow));
        rows.push(LedgerPeriodRes {
            period_code: flow.period_code,
            opening: WipAmountsRes::of(&period_opening),
            inflow: WipAmountsRes::of(&flow.inflow),
            outflow: WipAmountsRes::of(&flow.outflow),
            closing: WipAmountsRes::of(&closing),
            collect_lines: flow.collect_lines,
            transfer_times: flow.transfer_times,
        });
        running = closing;
    }

    LedgerSeries {
        opening,
        rows,
        closing: running,
        truncated,
    }
}

// ==================== 警告判定（纯逻辑） ====================

/// 一条警告。
#[derive(Debug, Clone, Serialize)]
pub struct WipWarningRes {
    /// 机器码（前端据此给不同图标 / 引导）。
    pub code: String,
    /// 级别：`WARN` / `INFO`。
    pub level: String,
    /// 说清「哪里不对」。
    pub message: String,
    /// 说清「怎么办」。
    pub hint: String,
}

impl WipWarningRes {
    fn new(code: &str, level: &str, message: String, hint: String) -> Self {
        Self {
            code: code.to_string(),
            level: level.to_string(),
            message,
            hint,
        }
    }
}

/// 判定警告需要的「此刻事实」（全部来自库，不在 DTO 里猜）。
#[derive(Debug, Clone)]
pub struct WipFacts<'a> {
    /// 批次状态 `OPEN` / `CLOSED` / `CANCELLED`。
    pub status: &'a str,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 在制余额（已相减，未夹零）。
    pub balance: Amounts,
}

/// 批次维度的警告（列表行与台账共用一套判据 —— 两处口径不同就是两个 bug）。
pub fn evaluate_warnings(facts: &WipFacts<'_>) -> Vec<WipWarningRes> {
    let mut warnings = Vec::new();
    let total = facts.balance.total();

    if any_negative(&facts.balance) {
        warnings.push(WipNewNeg::build(&facts.balance));
    }

    if facts.status == sheet_dto::STATUS_CANCELLED && !total.is_zero() {
        warnings.push(WipWarningRes::new(
            WARN_CANCELLED_WITH_BALANCE,
            LEVEL_WARN,
            format!("批次已作废，仍挂着在制余额 {total}"),
            "作废批的在制必须转出或清理（成本不能留在已作废的批次上），否则 4001 在制虚高、C-COST-3 会红".to_string(),
        ));
    } else if facts.status == sheet_dto::STATUS_CLOSED && !total.is_zero() {
        warnings.push(WipWarningRes::new(
            WARN_CLOSED_WITH_BALANCE,
            LEVEL_WARN,
            format!("批次已关批，仍挂着在制余额 {total}"),
            "关批前置本应拦住「有在制就关批」；要么关批后又归集了成本，要么有人直接改了库 —— 请核对本批归集与结转明细"
                .to_string(),
        ));
    } else if facts.status == sheet_dto::STATUS_OPEN
        && facts.wip_qty.is_zero()
        && facts.completed_qty > Decimal::ZERO
        && !total.is_zero()
    {
        warnings.push(WipWarningRes::new(
            WARN_FINISHED_NOT_TRANSFERRED,
            LEVEL_WARN,
            format!(
                "整批已完工（完工 {}，在制 0）但还有在制余额 {total} 没转出",
                facts.completed_qty
            ),
            "完工转出走「完工批次结转」（POST /business/erp/cost-collect-sheet/{id}/finish）；分批法不按约当产量分摊，未结转的成本会一直挂在在制".to_string(),
        ));
    }

    if facts.wip_qty > Decimal::ZERO && total.is_zero() {
        warnings.push(WipWarningRes::new(
            WARN_WIP_WITHOUT_COST,
            LEVEL_INFO,
            format!("还有在制数量 {}，但归集成本为 0", facts.wip_qty),
            "成本缺口：料是否已领并归集、工时是否已确认、制造费用是否已分摊？三路都要走单据，不要在这里补金额"
                .to_string(),
        ));
    }

    warnings
}

/// 余额有负要素时的警告（单独一层壳，避免 [`evaluate_warnings`] 里堆太多字面量）。
struct WipNewNeg;

impl WipNewNeg {
    fn build(balance: &Amounts) -> WipWarningRes {
        let mut parts = Vec::new();
        if balance.material < Decimal::ZERO {
            parts.push(format!("料 {}", balance.material));
        }
        if balance.labor < Decimal::ZERO {
            parts.push(format!("工 {}", balance.labor));
        }
        if balance.overhead < Decimal::ZERO {
            parts.push(format!("费 {}", balance.overhead));
        }
        WipWarningRes::new(
            WARN_NEGATIVE_BALANCE,
            LEVEL_WARN,
            format!("在制余额为负（{}）：已结转的金额超过了累计归集", parts.join("，")),
            "只可能是结转之后归集行又被作废 / 软删（领料单作废、工时被打回），或有人直接改库。请按归集明细逐笔核对，不要用调整分录把它抹平".to_string(),
        )
    }
}

// ==================== 已结算计算单（信息，用于口径解释） ====================

/// 结算口径的事实（`biz_cost_period_item.status = 'SETTLED'` 的最新一行）。
#[derive(Debug, Clone)]
pub struct SettledFacts {
    /// 结算期间 `yyyy-MM`。
    pub period_code: String,
    /// 完工转出金额（期间结算口径）。
    pub finished_cost: Decimal,
    /// 期末在制金额（期间结算口径，约当产量法）。
    pub wip_end_cost: Decimal,
}

/// 期间结算口径的说明性警告：批次口径与约当产量口径**本来就可能不等**。
pub fn settled_note(settled: &SettledFacts, batch_balance: &Amounts) -> WipWarningRes {
    WipWarningRes::new(
        WARN_SETTLED_BY_PERIOD,
        LEVEL_INFO,
        format!(
            "本批已随期间结算（{}）按约当产量算过一次：结算口径的在制 {}，批次口径余额 {}",
            settled.period_code,
            settled.wip_end_cost,
            batch_balance.total()
        ),
        "两个口径回答的问题不同：期间结算把成本在完工与在制之间按约当产量分摊（账务出口），批次口径是「这批累计花了多少 − 已整批转出多少」（工作底稿）。差额来自结算后新增的归集或批次级结转，不是错账".to_string(),
    )
}

// ==================== 出参 ====================

/// 三要素金额 + 合计（合计由三项相加，不另算一遍）。
#[derive(Debug, Clone, Serialize)]
pub struct WipAmountsRes {
    /// 料。
    pub material: String,
    /// 工。
    pub labor: String,
    /// 费。
    pub overhead: String,
    /// 合计 = 料 + 工 + 费。
    pub total: String,
}

impl WipAmountsRes {
    /// 由三要素构造。
    pub fn of(amounts: &Amounts) -> Self {
        Self {
            material: amounts.material.to_string(),
            labor: amounts.labor.to_string(),
            overhead: amounts.overhead.to_string(),
            total: amounts.total().to_string(),
        }
    }
}

/// 在制余额表的一行（一个批次）。
#[derive(Debug, Clone, Serialize)]
pub struct WipRowRes {
    /// 成本对象 id（台账接口用）。
    pub object_id: String,
    /// 批次号。
    pub object_no: String,
    /// 对象类型（一期恒 `BATCH`）。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: String,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 完工进度%。
    pub completion_pct: String,
    /// 批次状态。
    pub status: String,
    /// 开工日期。
    pub start_date: String,
    /// 计划完工日期。
    pub due_date: Option<String>,
    /// 截止期间的**累计归集**（料 / 工 / 费 / 合计）。
    pub collected: WipAmountsRes,
    /// 截止期间的**累计已结转**。
    pub transferred: WipAmountsRes,
    /// **在制余额** = 累计归集 − 累计已结转（逐要素，不夹零）。
    pub balance: WipAmountsRes,
    /// 本期投入（= 截止期间这一期的归集）。
    pub period_in: WipAmountsRes,
    /// 本期转出（= 截止期间这一期的结转）。
    pub period_out: WipAmountsRes,
    /// 累计归集行数。
    pub collect_lines: i64,
    /// 累计结转次数。
    pub transfer_times: i64,
    /// 首次归集期间（`None` = 一次都没归集过）。
    pub first_period: Option<String>,
    /// 最后一次归集期间。
    pub last_period: Option<String>,
    /// 最近一次结转时间（RFC3339）。
    pub last_transfer_at: Option<String>,
    /// 是否还有余额（`balance.total > 0`）。
    pub has_balance: bool,
    /// 警告码（完整报文见台账接口；列表里只给码，前端点开看详情）。
    pub warnings: Vec<String>,
}

/// 在制余额表的表尾合计（**整张表**，不是当前页）。
#[derive(Debug, Clone, Serialize)]
pub struct WipTotalsRes {
    /// 累计归集合计。
    pub collected: WipAmountsRes,
    /// 累计已结转合计。
    pub transferred: WipAmountsRes,
    /// 在制余额合计（= 累计归集 − 累计已结转）。
    pub balance: WipAmountsRes,
    /// 本期投入合计。
    pub period_in: WipAmountsRes,
    /// 本期转出合计。
    pub period_out: WipAmountsRes,
    /// 符合筛选的批次数。
    pub batch_count: i64,
    /// 其中还有余额的批次数。
    pub wip_batch_count: i64,
    /// 其中余额为负的批次数（> 0 ⇒ 立刻查）。
    pub negative_batch_count: i64,
}

/// 在制余额表响应。
#[derive(Debug, Clone, Serialize)]
pub struct WipBalanceRes {
    /// 截止期间。
    pub period_code: String,
    /// 表尾合计（整张表）。
    pub totals: WipTotalsRes,
    /// 当前页。
    pub list: Vec<WipRowRes>,
    /// 总条数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 截止期间之后还有发生时的说明（本表是「截至 P」的快照）。
#[derive(Debug, Clone, Serialize)]
pub struct LaterActivityRes {
    /// 之后各期累计归集合计。
    pub collected_total: String,
    /// 之后各期累计结转合计。
    pub transferred_total: String,
}

/// 台账响应。
#[derive(Debug, Clone, Serialize)]
pub struct WipLedgerRes {
    /// 截止期间。
    pub period_code: String,
    /// 批次抬头。
    pub batch: SheetBatchRes,
    /// 第一行之前的期初余额（只有被截断时才非 0）。
    pub opening: WipAmountsRes,
    /// 截止期间的累计归集 / 已结转 / 余额（与余额表该行**必然相等**）。
    pub summary: WipSummaryRes,
    /// 逐期间滚动行（升序，末行 = 截止期间）。
    pub periods: Vec<LedgerPeriodRes>,
    /// 是否因期间过多而截断了最早的若干期。
    pub truncated: bool,
    /// 截止期间之后还有发生（`None` = 之后没有任何归集 / 结转）。
    pub after_period: Option<LaterActivityRes>,
    /// 最新一条已结算计算单（期间结算口径），`None` = 没结算过。
    pub settled: Option<SettledRes>,
    /// 警告（含口径解释）。
    pub warnings: Vec<WipWarningRes>,
    /// 说清这张表的边界，前端直接展示。
    pub notes: Vec<String>,
}

/// 台账的汇总行。
#[derive(Debug, Clone, Serialize)]
pub struct WipSummaryRes {
    /// 累计归集（截止期间）。
    pub collected: WipAmountsRes,
    /// 累计已结转（截止期间）。
    pub transferred: WipAmountsRes,
    /// 在制余额。
    pub balance: WipAmountsRes,
    /// 累计归集行数。
    pub collect_lines: i64,
    /// 累计结转次数。
    pub transfer_times: i64,
}

/// 已结算计算单（期间结算口径）。
#[derive(Debug, Clone, Serialize)]
pub struct SettledRes {
    /// 结算期间。
    pub period_code: String,
    /// 完工转出。
    pub finished_cost: String,
    /// 期末在制（约当产量口径）。
    pub wip_end_cost: String,
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn amounts(material: &str, labor: &str, overhead: &str) -> Amounts {
        Amounts {
            material: dec(material),
            labor: dec(labor),
            overhead: dec(overhead),
        }
    }

    fn flow(period: &str, inflow: Amounts, outflow: Amounts) -> PeriodFlow {
        PeriodFlow {
            period_code: period.to_string(),
            inflow,
            collect_lines: if inflow.is_zero() { 0 } else { 1 },
            outflow,
            transfer_times: if outflow.is_zero() { 0 } else { 1 },
        }
    }

    fn query(period: &str) -> WipQuery {
        WipQuery {
            period_code: Some(period.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn period_is_required_and_shaped() {
        let error = filter_of(&WipQuery::default()).expect_err("缺期间必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("必填"), "报文要说清必填：{}", error.msg);

        let error = filter_of(&query("2026-13")).expect_err("13 月必须 400");
        assert_eq!(error.status, 400);

        let filter = filter_of(&query(" 2026-11 ")).expect("合法期间");
        assert_eq!(filter.period_code, "2026-11");
    }

    #[test]
    fn only_wip_defaults_to_false_and_status_all_normalizes_to_none() {
        let filter = filter_of(&query("2026-11")).expect("默认参数");
        assert!(!filter.only_wip, "默认要列出余额已归零的批次（它们是「转完了」的证据）");
        assert!(filter.status.is_none(), "ALL 归一成 None，SQL 里少一个假谓词");
        assert_eq!(filter.sort, DEFAULT_SORT);

        let filter = filter_of(&WipQuery {
            period_code: Some("2026-11".to_string()),
            only_wip: Some(true),
            status: Some("all".to_string()),
            ..Default::default()
        })
        .expect("显式参数");
        assert!(filter.only_wip);
        assert!(filter.status.is_none());
    }

    #[test]
    fn unknown_status_and_sort_are_400() {
        let error = filter_of(&WipQuery {
            period_code: Some("2026-11".to_string()),
            status: Some("DONE".to_string()),
            ..Default::default()
        })
        .expect_err("DONE 不在值域");
        assert_eq!(error.status, 400);

        let error = filter_of(&WipQuery {
            period_code: Some("2026-11".to_string()),
            sort: Some("balance; DROP TABLE".to_string()),
            ..Default::default()
        })
        .expect_err("排序注入必须 400");
        assert_eq!(error.status, 400);
    }

    #[test]
    fn order_by_sql_is_constant_and_never_echoes_input() {
        for sort in SORTS {
            let sql = order_by_sql(sort);
            assert!(sql.starts_with("ORDER BY"), "{sort} → {sql}");
            assert!(!sql.contains("DROP"), "{sort} 的 SQL 里出现了用户输入");
        }
        // 未知值兜底到默认排序（值域已在 filter_of 拦，这里只保证不会拼出空串）
        assert!(order_by_sql("whatever").contains("ORDER BY"));
    }

    #[test]
    fn balance_subtracts_elementwise_without_clamping() {
        let collected = amounts("100.0000", "50.0000", "25.0000");
        let transferred = amounts("40.0000", "10.0000", "5.0000");
        let balance = balance_of(&collected, &transferred);
        assert_eq!(balance, amounts("60.0000", "40.0000", "20.0000"));
        assert_eq!(balance.total(), dec("120.0000"));
        assert!(!any_negative(&balance));

        // 转超了 ⇒ 负数原样保留 + 判为异常（夹零会把「账错了」藏起来）
        let over = balance_of(&transferred, &collected);
        assert_eq!(over.material, dec("-60.0000"));
        assert!(any_negative(&over));
    }

    #[test]
    fn merge_flows_aligns_two_sources_on_the_same_period_axis() {
        let merged = merge_flows(
            vec![
                flow("2026-11", amounts("100", "0", "0"), Amounts::default()),
                flow("2026-10", amounts("50", "20", "0"), Amounts::default()),
            ],
            vec![flow("2026-11", Amounts::default(), amounts("30", "0", "0"))],
        );
        assert_eq!(merged.len(), 2, "同期间必须合并成一行");
        assert_eq!(merged[0].period_code, "2026-10", "升序");
        assert_eq!(merged[1].inflow.material, dec("100"));
        assert_eq!(merged[1].outflow.material, dec("30"));
        assert_eq!(merged[1].collect_lines, 1);
        assert_eq!(merged[1].transfer_times, 1);
    }

    #[test]
    fn ledger_rolls_forward_and_appends_the_cutoff_period_even_when_empty() {
        let series = ledger_series(
            vec![
                flow("2026-09", amounts("1000", "200", "300"), Amounts::default()),
                flow("2026-10", Amounts::default(), amounts("1000", "200", "300")),
            ],
            "2026-11",
        );
        assert_eq!(series.rows.len(), 3, "09 / 10 / 11 三期（11 月无发生也要补 0 行）");
        assert_eq!(series.rows[0].period_code, "2026-09");
        assert_eq!(series.rows[0].opening.total, "0");
        assert_eq!(series.rows[0].closing.total, "1500");
        // 期初 = 上期期末（滚动式的最基本性质）
        assert_eq!(series.rows[1].opening.total, "1500");
        assert_eq!(series.rows[1].closing.total, "0");
        assert_eq!(series.rows[2].opening.total, "0");
        assert_eq!(series.rows[2].closing.total, "0");
        assert!(!series.truncated);
        assert_eq!(series.opening.total(), Decimal::ZERO);
        assert_eq!(series.closing.total(), Decimal::ZERO);
    }

    #[test]
    fn ledger_closing_at_cutoff_equals_cumulative_balance() {
        // 同一批：10 月投入 1500、11 月又投 500、12 月转出 1000；截至 11 月
        let flows = merge_flows(
            vec![
                flow("2026-10", amounts("1000", "200", "300"), Amounts::default()),
                flow("2026-11", amounts("400", "100", "0"), Amounts::default()),
                flow("2026-12", amounts("700", "0", "0"), Amounts::default()),
            ],
            vec![flow("2026-12", Amounts::default(), amounts("1000", "0", "0"))],
        );
        let series = ledger_series(flows.clone(), "2026-11");
        // 12 月（截止之后）必须被排除：余额是「截至 11 月」的量
        assert_eq!(series.rows.len(), 2);
        let cumulative = balance_of(&amounts("1400", "300", "300"), &Amounts::default());
        assert_eq!(series.closing, cumulative);
        assert_eq!(series.rows.last().expect("末行").closing.total, "2000");
    }

    #[test]
    fn ledger_truncates_oldest_periods_into_the_opening_balance() {
        // 造 130 期有发生的流量（超过 MAX_LEDGER_PERIODS = 120）
        let mut flows = Vec::new();
        let mut period = "2000-01".to_string();
        for _ in 0..130 {
            flows.push(flow(&period, amounts("10", "0", "0"), Amounts::default()));
            period = next_period(&period).expect("下一个月");
        }
        let cutoff = flows.last().expect("末条").period_code.clone();
        let series = ledger_series(flows, &cutoff);
        assert!(series.truncated, "超上限必须自报截断");
        assert_eq!(series.rows.len(), MAX_LEDGER_PERIODS);
        // 丢掉 10 期 × 10 = 100 折进期初；期末仍是全量 1300
        assert_eq!(series.opening.material, dec("100"));
        assert_eq!(series.closing.material, dec("1300"));
        assert_eq!(series.rows[0].opening.material, "100");
    }

    #[test]
    fn next_period_rolls_over_the_year_and_rejects_garbage() {
        assert_eq!(next_period("2026-11"), Some("2026-12".to_string()));
        assert_eq!(next_period("2026-12"), Some("2027-01".to_string()));
        assert_eq!(next_period("2026-13"), None);
        assert_eq!(next_period("2026-1"), None);
        assert_eq!(next_period(""), None);
    }

    fn facts<'a>(status: &'a str, completed: &'a str, wip: &'a str, balance: Amounts) -> WipFacts<'a> {
        WipFacts {
            status: match status {
                "OPEN" => sheet_dto::STATUS_OPEN,
                "CLOSED" => sheet_dto::STATUS_CLOSED,
                _ => sheet_dto::STATUS_CANCELLED,
            },
            completed_qty: dec(completed),
            wip_qty: dec(wip),
            balance,
        }
    }

    #[test]
    fn open_batch_still_in_progress_has_no_warning() {
        let warnings = evaluate_warnings(&facts("OPEN", "0", "200", amounts("100", "0", "0")));
        assert!(warnings.is_empty(), "在产且成本正常 ⇒ 不该报任何警告：{warnings:?}");
    }

    #[test]
    fn finished_and_not_transferred_is_flagged() {
        let warnings = evaluate_warnings(&facts("OPEN", "800", "0", amounts("100", "0", "0")));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WARN_FINISHED_NOT_TRANSFERRED);
        assert_eq!(warnings[0].level, LEVEL_WARN);
        assert!(warnings[0].hint.contains("/finish"), "提示要给出结转接口");
    }

    #[test]
    fn closed_and_cancelled_with_balance_are_flagged_with_their_own_codes() {
        let closed = evaluate_warnings(&facts("CLOSED", "800", "0", amounts("100", "0", "0")));
        assert_eq!(closed[0].code, WARN_CLOSED_WITH_BALANCE);

        let cancelled = evaluate_warnings(&facts("CANCELLED", "0", "0", amounts("100", "0", "0")));
        assert_eq!(cancelled[0].code, WARN_CANCELLED_WITH_BALANCE);

        // 关批且余额为 0 ⇒ 正常（这正是关批的正确形态）
        assert!(evaluate_warnings(&facts("CLOSED", "800", "0", Amounts::default())).is_empty());
    }

    #[test]
    fn wip_qty_without_cost_is_reported_as_a_gap() {
        let warnings = evaluate_warnings(&facts("OPEN", "0", "200", Amounts::default()));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WARN_WIP_WITHOUT_COST);
        assert_eq!(warnings[0].level, LEVEL_INFO);
    }

    #[test]
    fn negative_balance_wins_and_names_the_elements() {
        let warnings = evaluate_warnings(&facts("OPEN", "0", "0", amounts("-5", "3", "0")));
        assert_eq!(warnings[0].code, WARN_NEGATIVE_BALANCE);
        assert!(warnings[0].message.contains("料"));
        assert!(!warnings[0].message.contains("费"), "只有负的要素才点名");
    }

    #[test]
    fn settled_note_explains_the_two_calibers() {
        let note = settled_note(
            &SettledFacts {
                period_code: "2026-10".to_string(),
                finished_cost: dec("1000"),
                wip_end_cost: dec("500"),
            },
            &amounts("1600", "0", "0"),
        );
        assert_eq!(note.code, WARN_SETTLED_BY_PERIOD);
        assert_eq!(note.level, LEVEL_INFO);
        assert!(note.message.contains("2026-10"));
        assert!(note.message.contains("500"));
        assert!(note.message.contains("1600"));
    }

    #[test]
    fn amounts_res_total_is_the_sum_of_the_three() {
        let res = WipAmountsRes::of(&amounts("1.0000", "2.0000", "3.0000"));
        assert_eq!(res.total, "6.0000");
    }

    #[test]
    fn permission_is_reused_from_cost_object_list() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(PERMISSIONS, [PERM_READ]);
    }
}
