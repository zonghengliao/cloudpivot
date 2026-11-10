//! calc 第 2 段「工归集」的**纯逻辑**：幂等键 / 行校验 / 增量计划 / 未归集清单（ARCH-9 §4.3 · B2-7）。
//!
//! 本文件不碰库：所有判据都是「输入 → 输出」的纯函数，可单测（真库用例在
//! `cost_calc_labor_db_tests.rs`）。落库口径见 [`crate::business::erp::cost_collect_model`]，
//! calc 第 2 段的编排见 [`crate::business::erp::cost_calc_labor`]。
//!
//! # 口径（设计 §4.3 表，逐字落地）
//!
//! | 项 | 值 | 依据 |
//! |---|---|---|
//! | 只扫什么 | `biz_cost_labor.status='CONFIRMED'` + 未软删 + `period_code` = 当期 | §4.3 工行 |
//! | 金额取什么 | `biz_cost_labor.amount`（录入时写入的 `round(hours × rate, 4)` 快照） | §3.1 ④ + §4.3 口径说明 ① |
//! | 费率从哪来 | `biz_cost_labor.rate` 快照（录入 / 导入时由 B2-3 按「对象 > 车间 > 公司」取数并落快照） | §4.5.4 |
//! | 期间归属 | `biz_cost_labor.period_code`（录入时选，跨月补录要选对期间） | §4.3 口径说明 ② |
//! | 幂等键 | `(LABOR_TICKET, labor_id, 0)` | §4.3 幂等键列 |
//! | 工行必备列 | `cost_object_id` 非空、`material_id` / `qty` 必须为空 | `ck_biz_cost_collect_obj` / `ck_biz_cost_collect_mat` |
//!
//! # 为什么金额取台账上的 `amount` 而不是在这里再乘一遍
//!
//! `biz_cost_labor.amount` 由库层 `ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))`
//! 钉死等于「工时 × 工资率」；`rate` 又是录入时由 B2-3 取数后落的**快照**（`§4.5.4` 的三级优先级）。
//! 归集段再乘一遍只会引入第二个舍入点（Rust 的 `Decimal` 与 PG `numeric` 虽同口径，但多了
//! 一处「谁算的」的分歧），而且会让「台账 400.00 / 归集 400.0001」这种账实不符变得可能。
//! 所以：**金额照抄台账**，`validate()` 里再用 [`cost_labor_dto::expected_amount`] 复核一遍
//! （与 CHECK 同引擎同舍入），不一致就是账面数据坏了 ⇒ 报错而不是照抄。
//!
//! # 「缺费率」为什么不写 0 金额的归集行（验收项）
//!
//! 一期成本核算**不接受 0 工资率的工时行参与归集**：`rate = 0` 在账面上有两个完全不同的
//! 含义 —— 「这一小时真的不值钱」与「财务还没配费率」，两者在库里长得一模一样。静默按 0
//! 归集正是设计 D-7「不猜」明令禁止的（算错的成本比算不出来的成本危险得多）。
//! 因此这类行**不写 collect**，而是进 [`LaborCollectOutcome::uncollected`]（「未归集清单」
//! 的工侧来源，由 B2-12 的 `GET /cost-period/{id}/uncollected` 展示给财务）。

use std::collections::HashSet;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::{
    self as collect, CollectKey, ExistingCollectRow, PurgedRow, exceeds_scale,
};
use crate::business::erp::cost_labor_dto;
use crate::shared::error_code;

// ==================== 常量（与 DDL / 设计逐字对齐） ====================

/// 成本要素：人工。
pub const ELEMENT_LABOR: &str = "LABOR";
/// 来源类型：工时台账。
pub const SOURCE_LABOR_TICKET: &str = "LABOR_TICKET";
/// 工归集行的来源行号（设计 §4.3 幂等键 `(LABOR_TICKET, labor_id, 0)` 的第三段）。
///
/// 与料行（恒为 `1`）不同：工行是一人一天一批一行，来源 id 已唯一定位到行，
/// 第三段固定 0（设计写死，不许改成明细行号）。
pub const LABOR_LINE_NO: i32 = 0;
/// 工时小数位（`biz_cost_labor.hours NUMERIC(10,2)`）。
pub const HOURS_SCALE: u32 = cost_labor_dto::HOURS_SCALE;
/// 费率小数位（`biz_cost_labor.rate NUMERIC(18,6)`）。
pub const RATE_SCALE: u32 = cost_labor_dto::RATE_SCALE;
/// 金额小数位（`biz_cost_collect.amount NUMERIC(18,4)`）。
pub const AMOUNT_SCALE: u32 = collect::AMOUNT_SCALE;
/// 备注列长度（`biz_cost_collect.remark VARCHAR(255)`）。
pub const REMARK_MAX: usize = 255;

// ==================== 幂等键 ====================

/// 工归集行的幂等键（`(LABOR_TICKET, labor_id, 0)`，与 `uk_biz_cost_collect_src` 同列序）。
pub fn labor_key(labor_id: Uuid) -> CollectKey {
    CollectKey {
        source_type: SOURCE_LABOR_TICKET.to_string(),
        source_id: labor_id,
        line_no: LABOR_LINE_NO,
    }
}

// ==================== 候选行（扫描结果） ====================

/// 一条**候补**工时台账行（`CONFIRMED` + 当期 + 未软删），由 model 查库产出。
///
/// 字段与 `biz_cost_collect` 的写入列一一对应，便于直接落库与逐列断言。
#[derive(Debug, Clone, PartialEq)]
pub struct LaborLine {
    /// 来源行 id（`biz_cost_labor.id`，幂等键第二段）。
    pub labor_id: Uuid,
    /// 工作日期（`biz_cost_labor.work_date`，写 `occur_date`）。
    pub work_date: NaiveDate,
    /// 成本对象（`biz_cost_labor.cost_object_id`，必填）。
    pub cost_object_id: Uuid,
    /// 成本对象号（`biz_cost_object.object_no`，仅用于报错 / 未归集清单的人话文案）。
    pub cost_object_no: Option<String>,
    /// 员工姓名快照（`biz_cost_labor.employee_name`）。
    pub employee_name: String,
    /// 工作中心（`biz_cost_labor.dept_id`）。
    pub dept_id: Uuid,
    /// 工作中心名（`sys_dept` 实时名优先，落空回落台账快照）。
    pub dept_name: Option<String>,
    /// 人工工时（`biz_cost_labor.hours`）。
    pub hours: Decimal,
    /// 机器工时（`biz_cost_labor.machine_hours`，仅进备注：它是分摊基数不是金额）。
    pub machine_hours: Decimal,
    /// 工资率快照（`biz_cost_labor.rate`，录入时由 B2-3 取数）。
    pub rate: Decimal,
    /// 人工金额（`biz_cost_labor.amount` = `round(hours × rate, 4)`，**照抄不重算**）。
    pub amount: Decimal,
    /// 台账来源（`MANUAL` / `IMPORT` / `MES`，仅进备注）。
    pub source_type: String,
}

impl LaborLine {
    /// 本行的幂等键。
    pub fn key(&self) -> CollectKey {
        labor_key(self.labor_id)
    }

    /// 可读的来源描述（备注 / 报错文案用）。
    pub fn label(&self) -> String {
        format!(
            "工时台账 {} {}（{} 小时）",
            self.work_date, self.employee_name, self.hours
        )
    }

    /// 写进 `biz_cost_collect.remark` 的人话快照。
    ///
    /// 工时台账**没有单号列**（设计 §3.1 ④），所以 `source_no` 留空、靠 `source_id`
    /// 精确点回工时行；人可读的上下文（日期 / 员工 / 工时 / 费率 / 来源）放这里，
    /// 让「归集明细」列表在没有 join 的情况下也能看懂这一行是什么。
    pub fn remark(&self) -> String {
        let text = format!(
            "工时台账 {} {} {}h×{}（{}）",
            self.work_date,
            self.employee_name,
            self.hours.normalize(),
            self.rate.normalize(),
            self.source_type
        );
        text.chars().take(REMARK_MAX).collect()
    }

    /// 库层 CHECK 复算的应有金额（`round(hours × rate, 4)`，与 `ck_biz_cost_labor_amount` 同引擎）。
    pub fn expected_amount(&self) -> std::result::Result<Decimal, ApiError> {
        cost_labor_dto::expected_amount(self.hours, self.rate)
    }

    /// 行校验：不合口径的来源数据**报错**，不静默丢（`66064`）。
    ///
    /// 走到这里说明工时台账上已经有账面异常行（工时 ≤ 0 / 金额 ≠ 工时 × 费率 / 字段超列标度）。
    /// 静默跳过会让「归集人工成本 ≠ 台账人工合计」且没人知道为什么 —— 正是巡检想抓却抓不出的场景。
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.cost_object_id.is_nil() {
            return Err(format!("{} 没有成本对象", self.label()));
        }
        if self.dept_id.is_nil() {
            return Err(format!("{} 没有工作中心", self.label()));
        }
        if self.employee_name.trim().is_empty() {
            return Err(format!("工时行 {} 的员工姓名为空", self.labor_id));
        }
        if self.hours <= Decimal::ZERO {
            return Err(format!("{} 的工时为 {}（必须 > 0）", self.label(), self.hours));
        }
        if exceeds_scale(self.hours, HOURS_SCALE) {
            return Err(format!(
                "{} 的工时超过 {HOURS_SCALE} 位小数（库列 NUMERIC(10,2)）",
                self.label()
            ));
        }
        if self.rate < Decimal::ZERO {
            return Err(format!("{} 的工资率为 {}（必须 ≥ 0）", self.label(), self.rate));
        }
        if exceeds_scale(self.rate, RATE_SCALE) {
            return Err(format!(
                "{} 的工资率超过 {RATE_SCALE} 位小数（库列 NUMERIC(18,6)）",
                self.label()
            ));
        }
        if self.amount < Decimal::ZERO {
            return Err(format!("{} 的金额为 {}（必须 ≥ 0）", self.label(), self.amount));
        }
        if exceeds_scale(self.amount, AMOUNT_SCALE) {
            return Err(format!(
                "{} 的金额超过 {AMOUNT_SCALE} 位小数（库列 NUMERIC(18,4)）",
                self.label()
            ));
        }
        let expected = self.expected_amount().map_err(|error| error.msg.clone())?;
        if expected != self.amount {
            return Err(format!(
                "{} 的金额 {} 与「工时 × 工资率」算出的 {}（round(…, {AMOUNT_SCALE})）不符",
                self.label(),
                self.amount,
                expected
            ));
        }
        Ok(())
    }

    /// 这一行为什么**不能**归集（`None` = 可归集）。
    ///
    /// 两种「算出来也是 0」的形态都不写 collect 行（见模块头「为什么缺费率不写 0」）：
    /// 1. `rate = 0`：费率未维护（与「真的不值钱」在库上无法区分）；
    /// 2. `rate > 0` 但 `round(hours × rate, 4) = 0`：工时极小导致金额舍入成 0，写下去等于记了 0 元。
    pub fn uncollected_reason(&self) -> Option<String> {
        if self.rate.is_zero() {
            return Some(format!(
                "工资率为 0：成本对象 {} / 工作中心 {} 在 {} 没有可用的工资率（费率取数「对象 > 车间 > 公司」三级都未命中）",
                self.cost_object_no.as_deref().unwrap_or("-"),
                self.dept_name.as_deref().unwrap_or("-"),
                self.work_date
            ));
        }
        if self.amount.is_zero() {
            return Some(format!(
                "工时 {} 小时 × 工资率 {} 元/小时 舍入到 {AMOUNT_SCALE} 位后金额为 0，未写归集行",
                self.hours.normalize(),
                self.rate.normalize()
            ));
        }
        None
    }
}

// ==================== 未归集清单（工侧） ====================

/// 一条进「未归集清单」的工时行（缺费率 / 金额舍入为 0）。
///
/// 无 HTTP 入口（`GET /cost-period/{id}/uncollected` 属 B2-12），本结构由
/// [`crate::business::erp::cost_calc_labor::collect_labor`] 的返回值带出，
/// 并在 calc 编排卡的出参里透传。
#[derive(Debug, Clone, PartialEq)]
pub struct UncollectedLabor {
    /// 工时台账行 id（点回原行）。
    pub labor_id: Uuid,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 成本对象号。
    pub cost_object_no: Option<String>,
    /// 工作中心 id（费率取数要用它走「车间级」，只给人看的车间名不能代替）。
    pub dept_id: Uuid,
    /// 员工姓名快照。
    pub employee_name: String,
    /// 工作中心名。
    pub dept_name: Option<String>,
    /// 工作日期。
    pub work_date: NaiveDate,
    /// 未归集的工时（财务最关心「差多少小时」）。
    pub hours: Decimal,
    /// 台账上的工资率（0 = 未维护）。
    pub rate: Decimal,
    /// 为什么没归集（人话）。
    pub reason: String,
    /// 怎么修（编排层查 B2-3 后填：现在能不能取到费率 / 该去配哪一级）。
    pub hint: Option<String>,
}

// ==================== 增量计划 ====================

/// 一次「扫描 + 对已归集行取差」的结果（纯函数产出，不碰库）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaborPlan {
    /// 本次要新写的行（按扫描顺序，键唯一）。
    pub to_insert: Vec<LaborLine>,
    /// 已归集过、本次跳过的键（幂等）。
    pub already: Vec<CollectKey>,
    /// 需要软删的旧行（来源已不符合口径，或对象 / 金额与来源不再一致）。
    pub stale: Vec<ExistingCollectRow>,
    /// 候补里重复出现的键数（同一工时行被扫到两次，正常为 0）。
    pub duplicated: usize,
    /// 不能归集、要进「未归集清单」的行（缺费率 / 金额舍入为 0）。
    pub uncollected: Vec<UncollectedLabor>,
}

impl LaborPlan {
    /// 待写金额合计。
    pub fn inserted_amount(&self) -> Decimal {
        self.to_insert.iter().map(|line| line.amount).sum()
    }

    /// 待清理金额合计（软删掉的旧行金额）。
    pub fn stale_amount(&self) -> Decimal {
        self.stale.iter().map(|row| row.amount).sum()
    }

    /// 未归集工时合计（「这一期有多少人工没算进成本」）。
    pub fn uncollected_hours(&self) -> Decimal {
        self.uncollected.iter().map(|row| row.hours).sum()
    }

    /// 是否无事可做（重复跑的最常见形态：该写该删该报的都空）。
    pub fn is_noop(&self) -> bool {
        self.to_insert.is_empty() && self.stale.is_empty() && self.uncollected.is_empty()
    }
}

/// 对「当期候补工时行」与「当期已归集工行」取差。
///
/// - 缺费率 / 金额舍入为 0 的行 ⇒ 进 `uncollected`，**不写 collect**（验收项）；
///   这类行**不参与**「已归集 / 过期」判定 ⇒ 历史上若有它的旧归集行，那行走 `stale` 被清掉
///   （否则「费率被清零」的脏数据会永远留着旧金额）；
/// - 候补里键重复 ⇒ 只留第一条（`duplicated` 计数），保证插入语句不会自撞唯一键；
/// - 已归集过且**如实反映来源**（对象 + 金额都对）⇒ 进 `already`（不改金额、不刷 `updated_at`）；
/// - 已归集但已过期（来源作废 / 改挂对象 / 金额对不上）⇒ 进 `stale`（软删）；
///   其中「键还在候补里但内容变了」的同时进 `to_insert` 重写（同一事务里先删后插）。
pub fn plan_of(eligible: &[LaborLine], existing: &[ExistingCollectRow]) -> LaborPlan {
    let mut seen: HashSet<CollectKey> = HashSet::new();
    let mut collectable: HashSet<CollectKey> = HashSet::new();
    let mut to_insert = Vec::new();
    let mut already = Vec::new();
    let mut uncollected = Vec::new();
    let mut duplicated = 0usize;

    let existing_by_key: std::collections::HashMap<&CollectKey, &ExistingCollectRow> =
        existing.iter().map(|row| (&row.key, row)).collect();
    let mut stale: Vec<ExistingCollectRow> = Vec::new();

    for line in eligible {
        let key = line.key();
        if !seen.insert(key.clone()) {
            duplicated += 1;
            continue;
        }
        // 先判「能不能归集」：缺费率的行既不写 collect，也不算「已归集」
        if let Some(reason) = line.uncollected_reason() {
            uncollected.push(UncollectedLabor {
                labor_id: line.labor_id,
                cost_object_id: line.cost_object_id,
                cost_object_no: line.cost_object_no.clone(),
                dept_id: line.dept_id,
                employee_name: line.employee_name.clone(),
                dept_name: line.dept_name.clone(),
                work_date: line.work_date,
                hours: line.hours,
                rate: line.rate,
                reason,
                hint: None,
            });
            continue;
        }
        // 能归集的键才参与「已归集 / 过期」判定：缺费率的行历史上若有归集行，
        // 那个旧行必须被当成「来源已不符合口径」清掉（否则 0 费率又留着旧金额）
        collectable.insert(key.clone());
        match existing_by_key.get(&key) {
            // 已归集且如实反映来源 ⇒ 幂等跳过（不刷 updated_at、不改金额）
            Some(row) if mirrors(row, line) => already.push(key),
            // 键在、内容过期（改挂对象 / 金额被修正）⇒ 清旧写新
            Some(row) => {
                stale.push((*row).clone());
                to_insert.push(line.clone());
            }
            None => to_insert.push(line.clone()),
        }
    }

    let eligible_keys: HashSet<CollectKey> = collectable;
    stale.extend(existing.iter().filter(|row| !eligible_keys.contains(&row.key)).cloned());
    // 确定性顺序：按工作日期 + 归集行 id 排，便于汇报与逐行断言
    stale.sort_by(|a, b| a.source_no.cmp(&b.source_no).then_with(|| a.id.cmp(&b.id)));
    // 未归集清单：按工作日期 + 工时行 id 排（财务按天排查）
    uncollected.sort_by(|a, b| a.work_date.cmp(&b.work_date).then_with(|| a.labor_id.cmp(&b.labor_id)));

    LaborPlan {
        to_insert,
        already,
        stale,
        duplicated,
        uncollected,
    }
}

/// 已归集行是否**如实**反映来源（归属与金额都对得上）。
///
/// 只比 `(cost_object_id, amount)`：工时行的员工 / 日期 / 工时 / 费率在 `CONFIRMED`
/// 之后不可改（只读），可变且影响成本的只有归属与金额。
fn mirrors(row: &ExistingCollectRow, line: &LaborLine) -> bool {
    row.cost_object_id == line.cost_object_id && row.amount == line.amount
}

// ==================== 段落结果（编排层出参） ====================

/// calc 第 2 段（工归集）的执行结果。
///
/// 无 HTTP 入口（设计 §5.2 只有 `POST /cost-period/calc` 一个入口，属 calc 编排卡），
/// 但要把「扫了几行 / 写了几行 / 跳了几行 / 清了几行 / 哪些没归集」讲清楚：
/// 前端的「未归集清单」（B2-12）与运维排错都靠它。
#[derive(Debug, Clone, PartialEq)]
pub struct LaborCollectOutcome {
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 扫描到的候补行数（`CONFIRMED` + 当期 + 未软删）。
    pub scanned: usize,
    /// 本次新写的行数。
    pub inserted: usize,
    /// 已归集过、本次跳过的行数（幂等）。
    pub skipped_existing: usize,
    /// 本次软删的过期行数（来源已不符合口径）。
    pub purged: usize,
    /// 候补里重复的键数（正常 0）。
    pub duplicated: usize,
    /// 本次新写金额合计。
    pub inserted_amount: Decimal,
    /// 本次软删金额合计。
    pub purged_amount: Decimal,
    /// 写后回查：当期工归集行数（`element='LABOR' AND source_type='LABOR_TICKET'`，未删）。
    pub collected_rows: i64,
    /// 写后回查：当期工归集金额合计。
    pub collected_amount: Decimal,
    /// 未归集清单（缺费率 / 金额舍入为 0），不含在 `collected_*` 里。
    pub uncollected: Vec<UncollectedLabor>,
    /// 未归集工时合计。
    pub uncollected_hours: Decimal,
    /// 过期行明细（最多 [`PURGED_DETAIL_LIMIT`] 条，超出只计数）。
    pub purged_detail: Vec<PurgedRow>,
}

impl LaborCollectOutcome {
    /// 未归集条数（「这一期有几笔人工没进成本」）。
    pub fn uncollected_rows(&self) -> usize {
        self.uncollected.len()
    }

    /// 是否干净跑完（没有未归集行）。
    pub fn is_clean(&self) -> bool {
        self.uncollected.is_empty()
    }
}

// ==================== 错误构造 ====================

/// 400 + `66064`：工时台账行的账面数据不合归集口径（静默跳过会让对账永远差一笔）。
pub fn labor_invalid(line: &LaborLine, reason: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_COLLECT_SOURCE_INVALID,
        format!(
            "{}的数据无法归集：{reason}。\n\
             工归集只认「已确认（CONFIRMED）」的工时台账行，且金额必须等于「工时 × 工资率」（4 位舍入）。\n\
             请先在工时台账上修正这一行（已确认的行只读，需作废重录）后重跑计算",
            line.label()
        ),
    )
}

/// 400：期间编码非法（`yyyy-MM`）。与料归集共用同一文案口径。
pub fn period_invalid(period_code: &str) -> ApiError {
    collect::period_invalid(period_code)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 一条可归集的工时行（8 小时 × 50 = 400.0000）。
    fn line(hours: &str, rate: &str, amount: &str) -> LaborLine {
        LaborLine {
            labor_id: Uuid::now_v7(),
            work_date: day("2026-11-05"),
            cost_object_id: Uuid::now_v7(),
            cost_object_no: Some("CB26-0001".to_string()),
            employee_name: "张伟".to_string(),
            dept_id: Uuid::now_v7(),
            dept_name: Some("机加工车间".to_string()),
            hours: dec(hours),
            machine_hours: dec("0"),
            rate: dec(rate),
            amount: dec(amount),
            source_type: cost_labor_dto::SOURCE_MANUAL.to_string(),
        }
    }

    /// 已归集行夹具：如实反映来源。
    fn existing_of(row: &LaborLine) -> ExistingCollectRow {
        ExistingCollectRow {
            id: Uuid::now_v7(),
            key: row.key(),
            source_no: None,
            amount: row.amount,
            cost_object_id: row.cost_object_id,
        }
    }

    /// 常量与 DDL / 设计必须逐字一致（改库不改代码会静默写不进 / 串到别的段）。
    #[test]
    fn constants_match_the_ddl_and_design() {
        assert_eq!(ELEMENT_LABOR, "LABOR");
        assert_eq!(SOURCE_LABOR_TICKET, "LABOR_TICKET");
        // 设计 §4.3 幂等键写死 `(LABOR_TICKET, labor_id, 0)`：第三段是 0，不是 1
        assert_eq!(LABOR_LINE_NO, 0);
        assert_eq!(HOURS_SCALE, 2);
        assert_eq!(RATE_SCALE, 6);
        assert_eq!(AMOUNT_SCALE, 4);
        // 只归集已确认的工时（设计 §4.3 + ck_biz_cost_labor_status）
        assert_eq!(cost_labor_dto::STATUS_CONFIRMED, "CONFIRMED");
    }

    /// 幂等键由 `labor_id` 唯一决定，且与本段常量一致。
    #[test]
    fn key_is_stable_and_uses_line_no_zero() {
        let id = Uuid::now_v7();
        assert_eq!(labor_key(id), labor_key(id));
        assert_eq!(labor_key(id).source_type, "LABOR_TICKET");
        assert_eq!(labor_key(id).line_no, 0);
        assert_ne!(labor_key(id), labor_key(Uuid::now_v7()));
        // 与料行的键（line_no = 1）不可能相等
        assert_ne!(labor_key(id), CollectKey::stock_out(id));
    }

    /// 首次运行：全部写，金额合计 = 各行台账金额之和。
    #[test]
    fn first_run_inserts_every_candidate() {
        let a = line("8", "50.000000", "400.0000");
        let b = line("1.5", "33.333333", "50.0000");
        let plan = plan_of(&[a.clone(), b.clone()], &[]);
        assert_eq!(plan.to_insert, vec![a, b]);
        assert!(plan.already.is_empty());
        assert!(plan.stale.is_empty());
        assert!(plan.uncollected.is_empty());
        assert_eq!(plan.inserted_amount(), dec("450.0000"));
        assert!(!plan.is_noop());
    }

    /// 验收项「只取 CONFIRMED 状态」的纯函数侧：不给 DRAFT 行任何入口（扫描条件在 model）。
    #[test]
    fn draft_rows_never_reach_the_plan() {
        // 计划层不认「状态」——状态是取数条件（SQL 里 status='CONFIRMED'）。
        // 这里钉住 model 侧的 SQL 常量不被改掉（真库用例另有断言）。
        assert!(
            crate::business::erp::cost_collect_model::LABOR_SOURCE_SQL.contains("l.status = 'CONFIRMED'"),
            "工归集必须只扫已确认的工时"
        );
    }

    /// 验收项「金额 = 工时 × 费率」：金额照抄台账，但必须与「工时 × 费率」一致。
    #[test]
    fn amount_must_equal_hours_times_rate() {
        let ok = line("8", "50.000000", "400.0000");
        assert!(ok.validate().is_ok());
        assert_eq!(ok.expected_amount().expect("期望金额"), dec("400.0000"));

        // 4 位舍入：1 小时 × 33.333333 = 33.333333 → 33.3333
        let rounded = line("1", "33.333333", "33.3333");
        assert!(rounded.validate().is_ok());
        assert_eq!(rounded.expected_amount().expect("期望金额"), dec("33.3333"));

        // 金额被改成别的数 ⇒ 报错（不照抄）
        let tampered = line("8", "50.000000", "399.9999");
        let reason = tampered.validate().expect_err("金额与工时×费率不符必须报错");
        assert!(reason.contains("不符"), "报文要点明不符：{reason}");
    }

    /// 验收项「缺费率进未归集清单而不是算 0」：rate = 0 的行不写 collect。
    #[test]
    fn zero_rate_rows_go_to_uncollected_instead_of_being_written() {
        let good = line("8", "50.000000", "400.0000");
        let no_rate = line("4", "0", "0");
        let plan = plan_of(&[good.clone(), no_rate.clone()], &[]);

        assert_eq!(plan.to_insert, vec![good], "缺费率行不许写归集行");
        assert_eq!(plan.uncollected.len(), 1, "要进未归集清单");
        assert!(plan.already.is_empty());
        assert!(plan.stale.is_empty());

        let item = &plan.uncollected[0];
        assert_eq!(item.labor_id, no_rate.labor_id);
        assert_eq!(item.hours, dec("4"));
        assert_eq!(item.rate, Decimal::ZERO);
        assert!(item.reason.contains("工资率为 0"), "{}", item.reason);
        assert!(item.reason.contains("CB26-0001"), "报文要点名成本对象");
        assert_eq!(plan.uncollected_hours(), dec("4"));
        // 金额合计里没有这笔 0
        assert_eq!(plan.inserted_amount(), dec("400.0000"));
    }

    /// 极小工时 × 小费率舍入成 0 也进未归集清单（写下去等于记 0 元）。
    #[test]
    fn amounts_that_round_to_zero_are_not_written() {
        let tiny = line("0.01", "0.000001", "0");
        assert!(tiny.validate().is_ok(), "台账层面这行合法（CHECK 也放行）");
        let plan = plan_of(&[tiny], &[]);
        assert!(plan.to_insert.is_empty());
        assert_eq!(plan.uncollected.len(), 1);
        assert!(
            plan.uncollected[0].reason.contains("金额为 0"),
            "{}",
            plan.uncollected[0].reason
        );
    }

    /// 重复跑：已归集且如实反映来源 ⇒ 跳过，不改金额、不刷 `updated_at`。
    #[test]
    fn rerun_skips_already_collected_rows() {
        let a = line("8", "50.000000", "400.0000");
        let b = line("2", "12.500000", "25.0000");
        let rows = vec![existing_of(&a), existing_of(&b)];
        let plan = plan_of(&[a, b], &rows);
        assert!(plan.to_insert.is_empty());
        assert_eq!(plan.already.len(), 2);
        assert!(plan.stale.is_empty());
        assert!(plan.uncollected.is_empty());
        assert!(plan.is_noop(), "重复跑必须是无操作");
        assert_eq!(plan.inserted_amount(), Decimal::ZERO);
    }

    /// 增量：只补新增的那一行。
    #[test]
    fn incremental_run_inserts_only_missing_rows() {
        let a = line("8", "50.000000", "400.0000");
        let b = line("2", "12.500000", "25.0000");
        let plan = plan_of(&[a.clone(), b.clone()], &[existing_of(&a)]);
        assert_eq!(plan.to_insert, vec![b]);
        assert_eq!(plan.already.len(), 1);
        assert!(plan.stale.is_empty());
    }

    /// 键在、但对象或金额与来源不一致 ⇒ 清旧写新（重复跑结果收敛）。
    #[test]
    fn rows_whose_object_or_amount_changed_are_rewritten() {
        let a = line("8", "50.000000", "400.0000");
        let other_object = Uuid::now_v7();

        // 对象换了
        let stale_row = ExistingCollectRow {
            id: Uuid::now_v7(),
            key: a.key(),
            source_no: None,
            amount: a.amount,
            cost_object_id: other_object,
        };
        let plan = plan_of(std::slice::from_ref(&a), &[stale_row]);
        assert_eq!(plan.to_insert, vec![a.clone()]);
        assert_eq!(plan.stale.len(), 1);
        assert_eq!(plan.stale[0].cost_object_id, other_object);
        assert!(plan.already.is_empty());

        // 金额变了
        let stale_row = ExistingCollectRow {
            id: Uuid::now_v7(),
            key: a.key(),
            source_no: None,
            amount: dec("399.0000"),
            cost_object_id: a.cost_object_id,
        };
        let changed = plan_of(std::slice::from_ref(&a), &[stale_row]);
        assert_eq!(changed.to_insert.len(), 1);
        assert_eq!(changed.stale.len(), 1);
        assert_eq!(changed.stale_amount(), dec("399.0000"));
    }

    /// 来源消失（工时行被删 / 被退回草稿）⇒ 旧归集行进 `stale`。
    #[test]
    fn rows_whose_source_disappeared_are_marked_stale() {
        let keep = line("8", "50.000000", "400.0000");
        let gone = ExistingCollectRow {
            id: Uuid::now_v7(),
            key: labor_key(Uuid::now_v7()),
            source_no: None,
            amount: dec("100.0000"),
            cost_object_id: Uuid::now_v7(),
        };
        let plan = plan_of(std::slice::from_ref(&keep), &[existing_of(&keep), gone]);
        assert!(plan.to_insert.is_empty());
        assert_eq!(plan.already.len(), 1);
        assert_eq!(plan.stale.len(), 1);
        assert_eq!(plan.stale_amount(), dec("100.0000"));

        // 缺费率的行如果历史上被归集过（费率后来被清零的脏数据形态），旧行同样要清掉
        let no_rate = line("4", "0", "0");
        let old = ExistingCollectRow {
            id: Uuid::now_v7(),
            key: no_rate.key(),
            source_no: None,
            amount: dec("200.0000"),
            cost_object_id: no_rate.cost_object_id,
        };
        let zero_rate = plan_of(&[no_rate], &[old]);
        assert_eq!(zero_rate.uncollected.len(), 1);
        assert_eq!(zero_rate.stale.len(), 1, "缺费率的行不许留着旧金额");
    }

    /// 候补里同一个键出现两次 ⇒ 只写一行且数得出来。
    #[test]
    fn duplicated_candidates_collapse_to_one_row() {
        let a = line("8", "50.000000", "400.0000");
        let mut dup = a.clone();
        dup.amount = dec("800.0000"); // 同 key 不同金额：留第一条
        let plan = plan_of(&[a.clone(), dup], &[]);
        assert_eq!(plan.to_insert.len(), 1);
        assert_eq!(plan.to_insert[0].amount, dec("400.0000"));
        assert_eq!(plan.duplicated, 1);
    }

    /// 未归集清单的顺序要确定（按工作日期 + 工时行 id）。
    #[test]
    fn uncollected_rows_are_ordered_deterministically() {
        let mut later = line("4", "0", "0");
        later.work_date = day("2026-11-20");
        let mut earlier = line("6", "0", "0");
        earlier.work_date = day("2026-11-02");
        let plan = plan_of(&[later, earlier], &[]);
        assert_eq!(plan.uncollected.len(), 2);
        assert_eq!(plan.uncollected[0].work_date, day("2026-11-02"));
        assert_eq!(plan.uncollected[1].work_date, day("2026-11-20"));
        assert_eq!(plan.uncollected_hours(), dec("10"));
    }

    /// 行校验：账面异常一律报错并点名，不静默跳过。
    #[test]
    fn validation_rejects_broken_source_rows() {
        let mut zero_hours = line("8", "50.000000", "400.0000");
        zero_hours.hours = Decimal::ZERO;
        assert!(zero_hours.validate().unwrap_err().contains("工时"));

        let mut negative_rate = line("8", "50.000000", "400.0000");
        negative_rate.rate = dec("-1");
        assert!(negative_rate.validate().unwrap_err().contains("工资率"));

        let mut no_object = line("8", "50.000000", "400.0000");
        no_object.cost_object_id = Uuid::nil();
        assert!(no_object.validate().unwrap_err().contains("成本对象"));

        let mut no_dept = line("8", "50.000000", "400.0000");
        no_dept.dept_id = Uuid::nil();
        assert!(no_dept.validate().unwrap_err().contains("工作中心"));

        let mut no_name = line("8", "50.000000", "400.0000");
        no_name.employee_name = "  ".to_string();
        assert!(no_name.validate().unwrap_err().contains("姓名"));

        let mut too_precise = line("8", "50.000000", "400.0000");
        too_precise.hours = dec("8.005");
        assert!(too_precise.validate().unwrap_err().contains("2 位小数"));

        let mut rate_precise = line("8", "50.000000", "400.0000");
        rate_precise.rate = dec("50.0000001");
        assert!(rate_precise.validate().unwrap_err().contains("6 位小数"));
    }

    /// 备注：带人话上下文（日期 / 员工 / 工时 / 费率 / 台账来源），且不超列宽。
    #[test]
    fn remark_carries_human_context_within_column_width() {
        let mut row = line("8", "50.000000", "400.0000");
        row.source_type = cost_labor_dto::SOURCE_IMPORT.to_string();
        let remark = row.remark();
        assert!(remark.contains("2026-11-05"));
        assert!(remark.contains("张伟"));
        assert!(remark.contains("8h"));
        assert!(remark.contains("50"));
        assert!(remark.contains("IMPORT"));
        assert!(remark.chars().count() <= REMARK_MAX);
    }

    /// 错误码与 HTTP 状态（前端按码映射文案，改了就断）。
    #[test]
    fn error_constructors_keep_code_and_status() {
        assert_eq!(error_code::ERP_COST_COLLECT_SOURCE_INVALID, 66064);
        let row = line("8", "50.000000", "399.9999");
        let err = labor_invalid(&row, "金额不符");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, 66064);
        assert!(err.msg.contains("张伟"), "报文要点名是哪一行");
        assert!(err.msg.contains("金额不符"));

        let err = period_invalid("2026-13");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("2026-13"));
    }
}
