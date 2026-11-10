//! calc 第 3 段「费归集」的**纯逻辑**：幂等键 / 行校验 / 增量计划 / 池汇总 / 未归集清单
//! （ARCH-9 §4.3 费三路 · B2-8）。
//!
//! 本文件不碰库：所有判据都是「输入 → 输出」的纯函数，可单测（真库用例在
//! `cost_calc_overhead_db_tests.rs`）。落库口径见
//! [`crate::business::erp::cost_overhead_collect_model`]，calc 第 3 段的编排见
//! [`crate::business::erp::cost_calc_overhead`]。
//!
//! # 口径（设计 §4.3 表的三行「费」，逐字落地）
//!
//! | 来源 | 单据口径 | `source_type` | 幂等键 | 去处 |
//! |---|---|---|---|---|
//! | 折旧 | `biz_depreciation.status='POSTED'` + 当期 + `expense_account_code LIKE '4101.%'` | `DEPRECIATION` | `(DEPRECIATION, dep_id, 0)` | **车间费用池**（`cost_object_id` 为空） |
//! | 报销 | `biz_expense_report.status IN ('APPROVED','PAID')` + 当期 + `item.account_code LIKE '4101.%'` | `EXPENSE` | `(EXPENSE, item_id, 0)` | **车间费用池** |
//! | 手工 | `biz_cost_overhead` 当期（未软删） | `OVERHEAD_TICKET` | `(OVERHEAD_TICKET, overhead_id, 0)` | 挂了对象 → **直接归集**；没挂 → **池** |
//!
//! # 三条容易做错的口径
//!
//! 1. **金额取来源单据的账上值，绝不重算**（§4.3 口径说明 ①）：
//!    折旧取 `biz_depreciation.amount`（计提时写入并已生成凭证），报销取明细的
//!    `amount`（**不含税**，与 `expense_bridge` 生成凭证时借记费用科目的金额同源），
//!    手工台账取 `biz_cost_overhead.amount`。段里再乘一遍/拆一次价税就必然与 4101 的账面数分毫。
//! 2. **只有「折旧 / 报销 / 手工」三种来源允许不挂成本对象**（`ck_biz_cost_collect_obj`）：
//!    池行 `cost_object_id` 必须为空，直接行必须非空 —— 由 [`OverheadLine::is_direct`] 一个判据决定，
//!    不接受外部布尔。
//! 3. **`account_code` 一律 `4101.*`**：池按 (车间, 科目) 汇总（§4.4.3 ①），科目不在 4101 的费用
//!    进不了成本（设计风险 R-2，与 B2-4 费用台账的三层科目校验同一条理由）。取数 SQL 已过滤，
//!    [`OverheadLine::validate`] 再兜一层（历史脏数据 / 有人改 SQL）。
//!
//! # 「金额为 0」为什么不写归集行
//!
//! `biz_cost_collect.amount` 允许 0（`CHECK amount >= 0`），但一张 0 元池行对分摊没有任何意义，
//! 却会让「池里有多少笔」与「来源单据有多少笔」永远对不上。与工段「缺费率不写 0」（D-7 不猜）
//! 同一处理：**进未归集清单让人看见**，不静默写 0。

use std::collections::{BTreeMap, HashSet};

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::{self as collect, CollectKey, PurgedRow, exceeds_scale};
use crate::shared::error_code;

// ==================== 常量（与 DDL / 设计逐字对齐） ====================

/// 成本要素：制造费用。
pub const ELEMENT_OVERHEAD: &str = "OVERHEAD";
/// 来源类型：折旧计提。
pub const SOURCE_DEPRECIATION: &str = "DEPRECIATION";
/// 来源类型：费用报销。
pub const SOURCE_EXPENSE: &str = "EXPENSE";
/// 来源类型：手工制造费用台账。
pub const SOURCE_OVERHEAD_TICKET: &str = "OVERHEAD_TICKET";
/// 费归集行的来源行号（设计 §4.3 幂等键 `(…, …, 0)` 的第三段，三路都是 0）。
pub const OVERHEAD_LINE_NO: i32 = 0;
/// 制造费用科目前缀（`ck_biz_cost_oh_debit` / 池汇总口径）。
pub const ACCOUNT_PREFIX: &str = "4101.";
/// 金额小数位（`biz_cost_collect.amount NUMERIC(18,4)`）。
pub const AMOUNT_SCALE: u32 = collect::AMOUNT_SCALE;
/// 备注列长度（`biz_cost_collect.remark VARCHAR(255)`）。
pub const REMARK_MAX: usize = 255;
/// 三种费来源（**不含** `ALLOC`：分摊结果是第 ⑥ 步的事，属 B2-9/编排卡）。
pub const OVERHEAD_SOURCES: [&str; 3] = [SOURCE_OVERHEAD_TICKET, SOURCE_DEPRECIATION, SOURCE_EXPENSE];

// ==================== 幂等键 ====================

/// 费归集行的幂等键（`(source_type, source_id, 0)`，与 `uk_biz_cost_collect_src` 同列序）。
pub fn overhead_key(source_type: &str, source_id: Uuid) -> CollectKey {
    CollectKey {
        source_type: source_type.to_string(),
        source_id,
        line_no: OVERHEAD_LINE_NO,
    }
}

// ==================== 候选行（扫描结果） ====================

/// 一条**候补**费归集行（折旧明细 / 报销明细 / 手工台账），由 model 查库产出。
///
/// 字段与 `biz_cost_collect` 的写入列一一对应，便于直接落库与逐列断言。
#[derive(Debug, Clone, PartialEq)]
pub struct OverheadLine {
    /// 来源类型（[`SOURCE_DEPRECIATION`] / [`SOURCE_EXPENSE`] / [`SOURCE_OVERHEAD_TICKET`]）。
    pub source_type: String,
    /// 来源行 id（折旧明细 id / 报销明细 id / 手工台账 id，幂等键第二段）。
    pub source_id: Uuid,
    /// 来源单号快照：报销取报销单号，折旧取资产编号，手工台账没有单号列 ⇒ `None`。
    pub source_no: Option<String>,
    /// 人话来源描述（报错 / 备注 / 未归集清单用，例如「折旧计提 2026-11 机床A」）。
    pub subject: String,
    /// 发生日期（折旧取批次计提日、报销取费用日期、手工台账取录入日）。
    pub occur_date: NaiveDate,
    /// 成本对象：`Some` = 直接归集（只有手工台账能给），`None` = 车间费用池。
    pub cost_object_id: Option<Uuid>,
    /// 归属车间（折旧取资产计提部门、报销取报销单部门、手工台账取台账车间）。
    pub dept_id: Uuid,
    /// 车间名（`sys_dept` 实时名优先，落空回落单据快照）。
    pub dept_name: Option<String>,
    /// 制造费用科目（`4101.*`，池按 (车间, 科目) 汇总就靠它）。
    pub account_code: String,
    /// 归集金额（**来源单据的账上值**，不重算）。
    pub amount: Decimal,
}

impl OverheadLine {
    /// 本行的幂等键。
    pub fn key(&self) -> CollectKey {
        overhead_key(&self.source_type, self.source_id)
    }

    /// 是否直接计入成本对象（`false` = 进车间费用池待分摊）。
    pub fn is_direct(&self) -> bool {
        self.cost_object_id.is_some_and(|id| !id.is_nil())
    }

    /// 可读的来源描述（报错文案用）。
    pub fn label(&self) -> String {
        format!("{}（{}）", self.subject, source_label(&self.source_type))
    }

    /// 写进 `biz_cost_collect.remark` 的人话快照。
    ///
    /// 归集表里只有 `source_type + source_id`，没有科目列也没有费用项目名；
    /// 财务在「归集明细」里要能一眼看出这一行是什么，所以把科目 + 车间 + 来源写全。
    pub fn remark(&self) -> String {
        let text = format!(
            "{} {} 科目{} 车间{}{}",
            source_label(&self.source_type),
            self.subject,
            self.account_code,
            self.dept_name.as_deref().unwrap_or("-"),
            if self.is_direct() { " 直接计入对象" } else { "" }
        );
        text.chars().take(REMARK_MAX).collect()
    }

    /// 行校验：不合口径的来源数据**报错**，不静默丢（`66064`）。
    ///
    /// 走到这里说明来源账面上已有异常行（金额为负 / 超列标度 / 科目不在 4101.* / 没车间）。
    /// 静默跳过会让「归集费用 ≠ 4101 发生额」且没人知道为什么。
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !OVERHEAD_SOURCES.contains(&self.source_type.as_str()) {
            return Err(format!(
                "来源类型「{}」不属于费归集（只认 DEPRECIATION / EXPENSE / OVERHEAD_TICKET）",
                self.source_type
            ));
        }
        if self.source_id.is_nil() {
            return Err(format!("{} 没有来源行 id", self.subject));
        }
        if !self.account_code.starts_with(ACCOUNT_PREFIX) {
            return Err(format!(
                "{} 的科目「{}」不在 {}* 内（非制造费用科目的金额进不了成本）",
                self.label(),
                self.account_code,
                ACCOUNT_PREFIX
            ));
        }
        if self.dept_id.is_nil() {
            return Err(format!("{} 没有归属车间，费用池无法按车间汇总", self.label()));
        }
        if self.cost_object_id.is_some_and(|object| object.is_nil()) {
            return Err(format!(
                "{} 的成本对象为空 uuid（要么不挂、要么挂真实对象）",
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
        Ok(())
    }

    /// 这一行为什么**不能**归集（`None` = 可归集）。
    ///
    /// 只有一种形态：金额为 0（见模块头「金额为 0 为什么不写归集行」）。
    pub fn uncollected_reason(&self) -> Option<String> {
        if self.amount.is_zero() {
            return Some(format!(
                "{} 的金额为 0，未写归集行（0 元池行会让「来源单据笔数」与「池行笔数」永远对不上；\
                 请核对这张单据是否该计提 / 该报销）",
                self.label()
            ));
        }
        None
    }
}

/// 来源类型的中文名（文案 / 备注共用，改词只改这里）。
fn source_label(source_type: &str) -> &'static str {
    match source_type {
        SOURCE_DEPRECIATION => "折旧计提",
        SOURCE_EXPENSE => "费用报销",
        SOURCE_OVERHEAD_TICKET => "手工费用",
        _ => "费用",
    }
}

// ==================== 未归集清单（费侧） ====================

/// 一条进「未归集清单」的费用行（金额为 0）。
///
/// 无 HTTP 入口（`GET /cost-period/{id}/uncollected` 属 B2-12），由
/// [`crate::business::erp::cost_calc_overhead::collect_overhead`] 的返回值带出，
/// 在 calc 编排卡的出参里透传。
#[derive(Debug, Clone, PartialEq)]
pub struct UncollectedOverhead {
    /// 来源类型。
    pub source_type: String,
    /// 来源行 id（点回原行）。
    pub source_id: Uuid,
    /// 来源单号（可为空）。
    pub source_no: Option<String>,
    /// 人话来源描述。
    pub subject: String,
    /// 车间 id。
    pub dept_id: Uuid,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 制造费用科目。
    pub account_code: String,
    /// 发生日期。
    pub occur_date: NaiveDate,
    /// 未归集的金额（= 0，留着便于前端统一列宽）。
    pub amount: Decimal,
    /// 为什么没归集（人话）。
    pub reason: String,
    /// 怎么修（由编排层填）。
    pub hint: Option<String>,
}

// ==================== 已归集行（费段） ====================

/// 一条当期已归集的**费行**（从 `biz_cost_collect` 读回）。
///
/// 不复用 `cost_collect_dto::ExistingCollectRow`：那张结构的 `cost_object_id` 是 `Uuid`
/// （料 / 工行必有对象），而费行的池行**没有对象**（`NULL`）——用它会读空值失败或把池行
/// 误判成过期。费段自带一个 `Option<Uuid>` 的版本，互不影响。
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingOverheadRow {
    /// 归集行 id（软删时用）。
    pub id: Uuid,
    /// 幂等键。
    pub key: CollectKey,
    /// 来源单号（诊断 / 汇报用）。
    pub source_no: Option<String>,
    /// 归集金额（与来源比对：账上金额被修正过就说明这一行过期了）。
    pub amount: Decimal,
    /// 归集到的成本对象（`None` = 车间费用池行）。
    pub cost_object_id: Option<Uuid>,
}

// ==================== 增量计划 ====================

/// 一次「扫描 + 对已归集行取差」的结果（纯函数产出，不碰库）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverheadPlan {
    /// 本次要新写的行（按扫描顺序，键唯一）。
    pub to_insert: Vec<OverheadLine>,
    /// 已归集过、本次跳过的键（幂等）。
    pub already: Vec<CollectKey>,
    /// 需要软删的旧行（来源已不符合口径，或归属 / 金额与来源不再一致）。
    pub stale: Vec<ExistingOverheadRow>,
    /// 候补里重复出现的键数（同一来源行被扫到两次，正常为 0）。
    pub duplicated: usize,
    /// 不能归集、要进「未归集清单」的行（金额为 0）。
    pub uncollected: Vec<UncollectedOverhead>,
}

impl OverheadPlan {
    /// 待写金额合计。
    pub fn inserted_amount(&self) -> Decimal {
        self.to_insert.iter().map(|line| line.amount).sum()
    }

    /// 待写池行金额合计。
    pub fn inserted_pool_amount(&self) -> Decimal {
        self.to_insert
            .iter()
            .filter(|line| !line.is_direct())
            .map(|line| line.amount)
            .sum()
    }

    /// 待写直接行金额合计。
    pub fn inserted_direct_amount(&self) -> Decimal {
        self.to_insert
            .iter()
            .filter(|line| line.is_direct())
            .map(|line| line.amount)
            .sum()
    }

    /// 待清理金额合计（软删掉的旧行金额）。
    pub fn stale_amount(&self) -> Decimal {
        self.stale.iter().map(|row| row.amount).sum()
    }

    /// 是否无事可做（重复跑的最常见形态）。
    pub fn is_noop(&self) -> bool {
        self.to_insert.is_empty() && self.stale.is_empty() && self.uncollected.is_empty()
    }
}

/// 对「当期候补费用行」与「当期已归集费行」取差。
///
/// - 金额为 0 的行 ⇒ 进 `uncollected`，**不写 collect**；这类行**不参与**「已归集 / 过期」
///   判定（用单独的 `collectable` 集合），历史上若有它的旧归集行会被当过期清掉
///   （否则「金额被改成 0」之后旧金额永远留着）；
/// - 候补里键重复 ⇒ 只留第一条（`duplicated` 计数），保证插入语句不会自撞唯一键；
/// - 已归集且**如实反映来源**（归属 + 金额都对）⇒ 进 `already`（不改金额、不刷 `updated_at`）；
/// - 已归集但已过期（来源作废 / 改挂对象 / 金额被修正）⇒ 进 `stale`（软删）；
///   其中「键还在候补里但内容变了」的同时进 `to_insert` 重写（同一事务里先删后插）。
pub fn plan_of(eligible: &[OverheadLine], existing: &[ExistingOverheadRow]) -> OverheadPlan {
    let mut seen: HashSet<CollectKey> = HashSet::new();
    let mut collectable: HashSet<CollectKey> = HashSet::new();
    let mut to_insert = Vec::new();
    let mut already = Vec::new();
    let mut uncollected = Vec::new();
    let mut duplicated = 0usize;

    let existing_by_key: std::collections::HashMap<&CollectKey, &ExistingOverheadRow> =
        existing.iter().map(|row| (&row.key, row)).collect();
    let mut stale: Vec<ExistingOverheadRow> = Vec::new();

    for line in eligible {
        let key = line.key();
        if !seen.insert(key.clone()) {
            duplicated += 1;
            continue;
        }
        // 先判「能不能归集」：0 元的行既不写 collect，也不算「已归集」
        if let Some(reason) = line.uncollected_reason() {
            uncollected.push(UncollectedOverhead {
                source_type: line.source_type.clone(),
                source_id: line.source_id,
                source_no: line.source_no.clone(),
                subject: line.subject.clone(),
                dept_id: line.dept_id,
                dept_name: line.dept_name.clone(),
                account_code: line.account_code.clone(),
                occur_date: line.occur_date,
                amount: line.amount,
                reason,
                hint: None,
            });
            continue;
        }
        // 能归集的键才参与「已归集 / 过期」判定
        collectable.insert(key.clone());
        match existing_by_key.get(&key) {
            Some(row) if mirrors(row, line) => already.push(key),
            Some(row) => {
                stale.push((*row).clone());
                to_insert.push(line.clone());
            }
            None => to_insert.push(line.clone()),
        }
    }

    let eligible_keys: HashSet<CollectKey> = collectable;
    stale.extend(existing.iter().filter(|row| !eligible_keys.contains(&row.key)).cloned());
    // 确定性顺序：来源类型 → 来源 id → 归集行 id（便于汇报与逐行断言）
    stale.sort_by(|a, b| {
        a.key
            .source_type
            .cmp(&b.key.source_type)
            .then_with(|| a.key.source_id.cmp(&b.key.source_id))
            .then_with(|| a.id.cmp(&b.id))
    });
    uncollected.sort_by(|a, b| {
        a.occur_date
            .cmp(&b.occur_date)
            .then_with(|| a.source_type.cmp(&b.source_type))
            .then_with(|| a.source_id.cmp(&b.source_id))
    });

    OverheadPlan {
        to_insert,
        already,
        stale,
        duplicated,
        uncollected,
    }
}

/// 已归集行是否**如实**反映来源（归属与金额都对得上）。
///
/// 只比 `(cost_object_id, amount)`：费来源行的车间 / 科目在单据上改不了（折旧与报销
/// 都走过账，手工台账录错是「删了重录」），可变且影响成本的只有「挂不挂 / 挂哪个对象」
/// 与金额本身。
fn mirrors(row: &ExistingOverheadRow, line: &OverheadLine) -> bool {
    row.cost_object_id == line.cost_object_id && row.amount == line.amount
}

// ==================== 池（按车间 + 科目） ====================

/// 池行：某车间某科目在某期间待分摊的金额。
///
/// 与 `cost_alloc_rule_model::PoolRow`（B2-5 试算用）**口径相同、职责不同**：
/// 那个是「读库现算待摊池」（试算面），这个是「本段刚归集的池行按科目汇总」
/// （归集面出参，带车间名与笔数，供 calc 编排卡与对账用）。
#[derive(Debug, Clone, PartialEq)]
pub struct PoolAmount {
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名（`sys_dept` 实时名优先，落空回落单据快照）。
    pub dept_name: Option<String>,
    /// 被摊科目（`4101.*`）。
    pub account_code: String,
    /// 池金额。
    pub amount: Decimal,
    /// 汇入该池行的笔数。
    pub rows: i64,
}

/// 把**扫描到的池行**（不含直接行）按 (车间, 科目) 汇总（纯函数）。
///
/// 真库侧的同一口径实现在 `cost_overhead_collect_model::pool_of_period`（走已落库的
/// `biz_cost_collect` 反查来源科目）；两边在真库用例里互相校验 —— 只改一边就会红。
pub fn pool_of(lines: &[OverheadLine]) -> Vec<PoolAmount> {
    let mut map: BTreeMap<(Uuid, String), (Option<String>, Decimal, i64)> = BTreeMap::new();
    for line in lines.iter().filter(|line| !line.is_direct()) {
        let entry =
            map.entry((line.dept_id, line.account_code.clone()))
                .or_insert((line.dept_name.clone(), Decimal::ZERO, 0));
        entry.1 += line.amount;
        entry.2 += 1;
    }
    map.into_iter()
        .map(|((dept_id, account_code), (dept_name, amount, rows))| PoolAmount {
            dept_id,
            dept_name,
            account_code,
            amount,
            rows,
        })
        .collect()
}

// ==================== 段落结果（编排层出参） ====================

/// calc 第 3 段（费归集）的执行结果。
///
/// 无 HTTP 入口（设计 §5.2 只有 `POST /cost-period/calc` 一个入口，属 calc 编排卡），
/// 但要把「扫了几行 / 写了几行 / 池里有多少 / 哪些没归集」讲清楚。
#[derive(Debug, Clone, PartialEq)]
pub struct OverheadCollectOutcome {
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 扫描到的候补行数（三路来源合计）。
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
    /// 其中进车间费用池的金额（`cost_object_id IS NULL`，待分摊）。
    pub inserted_pool_amount: Decimal,
    /// 其中直接计入成本对象的金额。
    pub inserted_direct_amount: Decimal,
    /// 本次软删金额合计。
    pub purged_amount: Decimal,
    /// 写后回查：当期费归集行数（三路来源，未删）。
    pub collected_rows: i64,
    /// 写后回查：当期费归集金额合计。
    pub collected_amount: Decimal,
    /// 写后回查：当期车间费用池按 (车间, 科目) 汇总（验收项「成池」）。
    pub pool: Vec<PoolAmount>,
    /// 未归集清单（金额为 0），不含在 `collected_*` 里。
    pub uncollected: Vec<UncollectedOverhead>,
    /// 过期行明细（最多 [`collect::PURGED_DETAIL_LIMIT`] 条，超出只计数）。
    pub purged_detail: Vec<PurgedRow>,
}

impl OverheadCollectOutcome {
    /// 未归集条数。
    pub fn uncollected_rows(&self) -> usize {
        self.uncollected.len()
    }

    /// 池金额合计（三路池行的总和）。
    pub fn pool_amount(&self) -> Decimal {
        self.pool.iter().map(|row| row.amount).sum()
    }

    /// 是否干净跑完（没有未归集行）。
    pub fn is_clean(&self) -> bool {
        self.uncollected.is_empty()
    }
}

// ==================== 错误构造 ====================

/// 400 + `66064`：来源单据/台账的账面数据不合归集口径。
pub fn overhead_invalid(line: &OverheadLine, reason: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_COLLECT_SOURCE_INVALID,
        format!(
            "{}的数据无法归集：{reason}。\n\
             费归集只认「折旧（POSTED）/ 报销（APPROVED、PAID）/ 手工台账」且科目落在 4101.* 的单据。\n\
             请先修正这张单据（作废重录或改科目）后重跑计算",
            line.label()
        ),
    )
}

/// 400：期间编码非法（`yyyy-MM`）。与料 / 工归集共用同一文案口径。
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

    /// 一条可归集的折旧行（池行）。
    fn dep(amount: &str) -> OverheadLine {
        OverheadLine {
            source_type: SOURCE_DEPRECIATION.to_string(),
            source_id: Uuid::now_v7(),
            source_no: Some("FA26-0001".to_string()),
            subject: "折旧计提 2026-11 机床A".to_string(),
            occur_date: day("2026-11-01"),
            cost_object_id: None,
            dept_id: Uuid::now_v7(),
            dept_name: Some("机加工车间".to_string()),
            account_code: "4101.01".to_string(),
            amount: dec(amount),
        }
    }

    /// 一条可归集的报销行（池行）。
    fn exp(amount: &str) -> OverheadLine {
        OverheadLine {
            source_type: SOURCE_EXPENSE.to_string(),
            source_id: Uuid::now_v7(),
            source_no: Some("EXP26-0001".to_string()),
            subject: "报销单 EXP26-0001 车间电费".to_string(),
            occur_date: day("2026-11-08"),
            cost_object_id: None,
            dept_id: Uuid::now_v7(),
            dept_name: Some("机加工车间".to_string()),
            account_code: "4101.02".to_string(),
            amount: dec(amount),
        }
    }

    /// 一条手工台账行（`direct` = 是否挂成本对象）。
    fn oh(amount: &str, direct: bool) -> OverheadLine {
        OverheadLine {
            source_type: SOURCE_OVERHEAD_TICKET.to_string(),
            source_id: Uuid::now_v7(),
            source_no: None,
            subject: "手工台账 2026-11 修理费".to_string(),
            occur_date: day("2026-11-15"),
            cost_object_id: if direct { Some(Uuid::now_v7()) } else { None },
            dept_id: Uuid::now_v7(),
            dept_name: Some("机加工车间".to_string()),
            account_code: "4101.04".to_string(),
            amount: dec(amount),
        }
    }

    /// 已归集行夹具：如实反映来源。
    fn existing_of(line: &OverheadLine) -> ExistingOverheadRow {
        ExistingOverheadRow {
            id: Uuid::now_v7(),
            key: line.key(),
            source_no: line.source_no.clone(),
            amount: line.amount,
            cost_object_id: line.cost_object_id,
        }
    }

    /// 常量与 DDL / 设计必须逐字一致。
    #[test]
    fn constants_match_the_ddl_and_design() {
        assert_eq!(ELEMENT_OVERHEAD, "OVERHEAD");
        assert_eq!(SOURCE_DEPRECIATION, "DEPRECIATION");
        assert_eq!(SOURCE_EXPENSE, "EXPENSE");
        assert_eq!(SOURCE_OVERHEAD_TICKET, "OVERHEAD_TICKET");
        // 设计 §4.3 三路费的幂等键第三段都是 0
        assert_eq!(OVERHEAD_LINE_NO, 0);
        assert_eq!(ACCOUNT_PREFIX, "4101.");
        assert_eq!(AMOUNT_SCALE, 4);
        // 值域必须在 V0371 的 ck_biz_cost_collect_src 里
        assert_eq!(OVERHEAD_SOURCES.len(), 3);
        for source in OVERHEAD_SOURCES {
            assert!(
                [
                    "STOCK_OUT",
                    "LABOR_TICKET",
                    "OVERHEAD_TICKET",
                    "DEPRECIATION",
                    "EXPENSE",
                    "ALLOC",
                    "MANUAL"
                ]
                .contains(&source),
                "{source} 不在 ck_biz_cost_collect_src 的值域内"
            );
        }
        // 三路与本卡的要素组合必须唯一（不许与料 / 工段撞）
        assert_ne!(ELEMENT_OVERHEAD, collect::ELEMENT_MATERIAL);
        assert_eq!(overhead_key(SOURCE_DEPRECIATION, Uuid::nil()).line_no, 0);
    }

    /// 幂等键由 (来源类型, 来源行 id) 唯一决定：三个来源同 id 也互不冲突。
    #[test]
    fn key_is_disjoint_across_the_three_sources() {
        let id = Uuid::now_v7();
        let keys: Vec<CollectKey> = OVERHEAD_SOURCES.iter().map(|s| overhead_key(s, id)).collect();
        for (i, a) in keys.iter().enumerate() {
            for b in keys.iter().skip(i + 1) {
                assert_ne!(a, b, "三路来源的幂等键不许相等");
            }
        }
        assert_eq!(keys[0].line_no, 0);
        assert_ne!(overhead_key(SOURCE_EXPENSE, id), CollectKey::stock_out(id));
    }

    /// 首次运行：三路全写，池 / 直接金额分开统计。
    #[test]
    fn first_run_inserts_every_candidate_and_splits_pool_from_direct() {
        let d = dep("1000.0000");
        let e = exp("200.0000");
        let direct = oh("300.0000", true);
        let pooled = oh("400.0000", false);
        let plan = plan_of(&[d.clone(), e.clone(), direct.clone(), pooled.clone()], &[]);
        assert_eq!(plan.to_insert, vec![d, e, direct, pooled]);
        assert!(plan.already.is_empty());
        assert!(plan.stale.is_empty());
        assert!(plan.uncollected.is_empty());
        assert_eq!(plan.inserted_amount(), dec("1900.0000"));
        // 池 = 1000 + 200 + 400，直接 = 300
        assert_eq!(plan.inserted_pool_amount(), dec("1600.0000"));
        assert_eq!(plan.inserted_direct_amount(), dec("300.0000"));
        assert!(!plan.is_noop());
    }

    /// 重复跑：已归集且如实反映来源 ⇒ 跳过，不改金额。
    #[test]
    fn rerun_skips_already_collected_rows() {
        let d = dep("1000.0000");
        let e = exp("200.0000");
        let rows = vec![existing_of(&d), existing_of(&e)];
        let plan = plan_of(&[d, e], &rows);
        assert!(plan.to_insert.is_empty());
        assert_eq!(plan.already.len(), 2);
        assert!(plan.stale.is_empty());
        assert!(plan.is_noop(), "重复跑必须是无操作");
        assert_eq!(plan.inserted_amount(), Decimal::ZERO);
    }

    /// 增量：只补新增的那一路。
    #[test]
    fn incremental_run_inserts_only_missing_rows() {
        let d = dep("1000.0000");
        let e = exp("200.0000");
        let plan = plan_of(&[d.clone(), e.clone()], &[existing_of(&d)]);
        assert_eq!(plan.to_insert, vec![e]);
        assert_eq!(plan.already.len(), 1);
        assert!(plan.stale.is_empty());
    }

    /// 键在、但归属或金额与来源不一致 ⇒ 清旧写新（重复跑结果收敛）。
    #[test]
    fn rows_whose_attribution_or_amount_changed_are_rewritten() {
        let d = dep("1000.0000");
        // 金额被修正
        let stale_row = ExistingOverheadRow {
            id: Uuid::now_v7(),
            key: d.key(),
            source_no: d.source_no.clone(),
            amount: dec("999.0000"),
            cost_object_id: d.cost_object_id,
        };
        let plan = plan_of(std::slice::from_ref(&d), &[stale_row]);
        assert_eq!(plan.to_insert, vec![d.clone()]);
        assert_eq!(plan.stale.len(), 1);
        assert_eq!(plan.stale_amount(), dec("999.0000"));

        // 手工台账从「池」改成「挂对象」（直接归集）
        let pooled = oh("400.0000", false);
        let old = existing_of(&pooled);
        let mut direct = pooled.clone();
        direct.cost_object_id = Some(Uuid::now_v7());
        let changed = plan_of(&[direct.clone()], &[old]);
        assert_eq!(changed.to_insert, vec![direct]);
        assert_eq!(changed.stale.len(), 1, "挂载关系变了必须清旧行");
    }

    /// 来源消失（单据作废 / 台账删除）⇒ 旧归集行进 `stale`。
    #[test]
    fn rows_whose_source_disappeared_are_marked_stale() {
        let keep = dep("1000.0000");
        let gone = ExistingOverheadRow {
            id: Uuid::now_v7(),
            key: overhead_key(SOURCE_EXPENSE, Uuid::now_v7()),
            source_no: Some("EXP26-9999".to_string()),
            amount: dec("500.0000"),
            cost_object_id: None,
        };
        let plan = plan_of(std::slice::from_ref(&keep), &[existing_of(&keep), gone]);
        assert!(plan.to_insert.is_empty());
        assert_eq!(plan.already.len(), 1);
        assert_eq!(plan.stale.len(), 1);
        assert_eq!(plan.stale_amount(), dec("500.0000"));
    }

    /// 验收项「金额为 0 不写归集行」：进未归集清单，且历史旧行被清掉。
    #[test]
    fn zero_amount_rows_go_to_uncollected_and_purge_their_old_rows() {
        let zero = dep("0");
        assert!(zero.validate().is_ok(), "0 元在账面上合法（库只禁负数）");
        let old = ExistingOverheadRow {
            id: Uuid::now_v7(),
            key: zero.key(),
            source_no: zero.source_no.clone(),
            amount: dec("123.0000"),
            cost_object_id: None,
        };
        let plan = plan_of(std::slice::from_ref(&zero), &[old]);
        assert!(plan.to_insert.is_empty(), "0 元行不许写归集行");
        assert_eq!(plan.uncollected.len(), 1);
        assert!(plan.uncollected[0].reason.contains("金额为 0"));
        assert_eq!(plan.uncollected[0].account_code, "4101.01");
        assert_eq!(plan.stale.len(), 1, "0 元行不许留着旧金额");
        assert_eq!(plan.stale_amount(), dec("123.0000"));
    }

    /// 候补里同一个键出现两次 ⇒ 只写一行且数得出来。
    #[test]
    fn duplicated_candidates_collapse_to_one_row() {
        let a = dep("1000.0000");
        let mut dup = a.clone();
        dup.amount = dec("2000.0000");
        let plan = plan_of(&[a.clone(), dup], &[]);
        assert_eq!(plan.to_insert.len(), 1);
        assert_eq!(plan.to_insert[0].amount, dec("1000.0000"));
        assert_eq!(plan.duplicated, 1);
    }

    /// 未归集清单顺序确定（按发生日期 → 来源类型 → 来源 id）。
    #[test]
    fn uncollected_rows_are_ordered_deterministically() {
        let mut later = exp("0");
        later.occur_date = day("2026-11-20");
        let mut earlier = dep("0");
        earlier.occur_date = day("2026-11-02");
        let plan = plan_of(&[later, earlier], &[]);
        assert_eq!(plan.uncollected.len(), 2);
        assert_eq!(plan.uncollected[0].occur_date, day("2026-11-02"));
        assert_eq!(plan.uncollected[1].occur_date, day("2026-11-20"));
    }

    /// 验收项「按 (车间, 科目) 汇总成池」：同车间同科目合并，不同车间 / 科目分开；
    /// **直接行不进池**（否则会被分摊一次、等于重复计入成本）。
    #[test]
    fn pool_groups_by_dept_and_account_and_excludes_direct_rows() {
        let mut a = dep("1000.0000");
        let mut b = exp("200.0000");
        b.dept_id = a.dept_id;
        b.account_code = "4101.01".to_string();
        a.dept_name = Some("机加工车间".to_string());
        b.dept_name = Some("机加工车间".to_string());
        let mut direct = oh("300.0000", true);
        direct.dept_id = a.dept_id;
        direct.account_code = "4101.01".to_string();

        let pool = pool_of(&[a, b, direct]);
        assert_eq!(pool.len(), 1, "同车间同科目必须合成一行");
        assert_eq!(pool[0].account_code, "4101.01");
        assert_eq!(pool[0].amount, dec("1200.0000"));
        assert_eq!(pool[0].rows, 2);
        assert_eq!(pool[0].dept_name.as_deref(), Some("机加工车间"));

        // 不同科目 / 不同车间各成一行，且按 (车间, 科目) 排序
        let mut other_dept = dep("50.0000");
        other_dept.account_code = "4101.02".to_string();
        let pool = pool_of(&[other_dept.clone(), dep("10.0000")]);
        assert_eq!(pool.len(), 2);
        assert!(
            pool.windows(2)
                .all(|pair| (pair[0].dept_id, &pair[0].account_code) <= (pair[1].dept_id, &pair[1].account_code)),
            "池必须按 (车间, 科目) 有序：{pool:?}"
        );
        assert_eq!(pool.iter().map(|row| row.amount).sum::<Decimal>(), dec("60.0000"));
    }

    /// 空输入 ⇒ 空池（不报错、不产生 0 行）。
    #[test]
    fn empty_input_yields_empty_pool() {
        assert!(pool_of(&[]).is_empty());
        assert!(plan_of(&[], &[]).is_noop());
    }

    /// 行校验：账面异常一律报错并点名，不静默跳过。
    #[test]
    fn validation_rejects_broken_source_rows() {
        let mut bad_account = dep("1000.0000");
        bad_account.account_code = "5602.01".to_string();
        assert!(bad_account.validate().unwrap_err().contains("4101."));

        let mut no_dept = dep("1000.0000");
        no_dept.dept_id = Uuid::nil();
        assert!(no_dept.validate().unwrap_err().contains("车间"));

        let negative = dep("-1");
        assert!(negative.validate().unwrap_err().contains("金额"));

        let too_precise = dep("1.00001");
        assert!(too_precise.validate().unwrap_err().contains("4 位小数"));

        let mut nil_object = oh("100.0000", false);
        nil_object.cost_object_id = Some(Uuid::nil());
        assert!(nil_object.validate().unwrap_err().contains("空 uuid"));

        let mut nil_source = dep("1000.0000");
        nil_source.source_id = Uuid::nil();
        assert!(nil_source.validate().unwrap_err().contains("来源行"));

        let mut wrong_type = dep("1000.0000");
        wrong_type.source_type = "STOCK_OUT".to_string();
        assert!(wrong_type.validate().unwrap_err().contains("不属于费归集"));

        assert!(dep("0").validate().is_ok(), "0 元是「未归集」不是「非法」");
        assert!(oh("100.0000", true).validate().is_ok());
    }

    /// `is_direct` 只看成本对象（不接受外部布尔），池 / 直接判据唯一。
    #[test]
    fn direct_flag_is_derived_from_cost_object() {
        assert!(!dep("1000.0000").is_direct());
        assert!(!exp("100.0000").is_direct());
        assert!(!oh("100.0000", false).is_direct());
        assert!(oh("100.0000", true).is_direct());
    }

    /// 备注带人话上下文（来源 / 科目 / 车间），且不超列宽。
    #[test]
    fn remark_carries_human_context_within_column_width() {
        let direct = oh("100.0000", true);
        let remark = direct.remark();
        assert!(remark.contains("手工费用"));
        assert!(remark.contains("4101.04"));
        assert!(remark.contains("机加工车间"));
        assert!(remark.contains("直接计入对象"));
        assert!(remark.chars().count() <= REMARK_MAX);

        let dep = dep("1000.0000");
        assert!(dep.remark().contains("折旧计提"));
        assert!(!dep.remark().contains("直接计入对象"));
    }

    /// 错误码与 HTTP 状态（前端按码映射文案，改了就断）。
    #[test]
    fn error_constructors_keep_code_and_status() {
        assert_eq!(error_code::ERP_COST_COLLECT_SOURCE_INVALID, 66064);
        let err = overhead_invalid(&dep("1000.0000"), "科目不在 4101.*");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, 66064);
        assert!(err.msg.contains("折旧计提"), "报文要点名来源：{}", err.msg);

        let err = period_invalid("2026-13");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("2026-13"));
    }
}
