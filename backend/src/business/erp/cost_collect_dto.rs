//! 成本归集（`biz_cost_collect`）的**纯逻辑**：幂等键 / 行校验 / 增量计划（ARCH-9 §4.3 · B2-6）。
//!
//! 本文件不碰库：所有判据都是「输入 → 输出」的纯函数，可单测（真库用例在
//! `cost_calc_material_db_tests.rs`）。落库口径见 [`crate::business::erp::cost_collect_model`]，
//! calc 第 1 段的编排见 [`crate::business::erp::cost_calc_material`]。
//!
//! # 口径（设计 §4.3 表，逐字落地）
//!
//! | 项 | 值 | 依据 |
//! |---|---|---|
//! | 只扫什么 | `biz_stock_out.out_type='ISSUE'` + `status='POSTED'` + `cost_object_id IS NOT NULL` | §4.3 料行 |
//! | 金额取什么 | `biz_stock_out_item.amount`（过账时写入的**账上真实扣减**） | §4.3 口径说明 ① |
//! | 期间归属 | `biz_stock_out.out_date` 落在期间首末日之间 | §4.3 口径说明 ② |
//! | 幂等键 | `(STOCK_OUT, item_id, 1)` | §4.3 幂等键列 |
//! | 料行必备列 | `material_id` / `qty` / `amount` 全部非空 | `ck_biz_cost_collect_mat` |
//!
//! **为什么不按 `qty × avg_cost` 重算金额**：一张领料单可能跨多个库存桶（多仓 / 多库位 /
//! 多批号），过账时 `biz_stock_out_item.amount` 是按桶逐笔扣减后的**真实差额**，
//! 与 `qty × 单价` 在 4 位舍入下必然有分毫差（`voucher::create_for_stock_out` 的硬红线
//! 同一条理由）。重算会让归集数与 1405 的减少额对不上 ⇒ C-COST-5 巡检天天红。
//!
//! # 幂等的两种语义（都实现，互不冲突）
//!
//! 1. **不重复写**：同一来源行（`item_id`）已归集过就跳过，靠
//!    `uk_biz_cost_collect_src (source_type, source_id, line_no) WHERE deleted_at IS NULL`
//!    兜底（`ON CONFLICT DO NOTHING`），并通过 [`plan`] 先算一遍「该写几行」用于汇报。
//! 2. **结果收敛**：来源已不再符合口径的旧归集行（领料单已作废 / 改挂了别的成本对象）
//!    必须在重跑时被清掉，否则「同一期间重复跑」结果会漂。这类行由 [`plan`] 的
//!    `stale` 给出，由编排层软删（保留 `deleted_at` 留痕，唯一键随之释放）。

use std::collections::HashSet;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

// ==================== 常量（与 DDL / 设计逐字对齐） ====================

/// 成本要素：材料。
pub const ELEMENT_MATERIAL: &str = "MATERIAL";
/// 来源类型：领料出库。
pub const SOURCE_STOCK_OUT: &str = "STOCK_OUT";
/// 出库类型：领用。
pub const OUT_TYPE_ISSUE: &str = "ISSUE";
/// 出库状态：已过账。
pub const OUT_STATUS_POSTED: &str = "POSTED";
/// 料归集行的来源行号（设计 §4.3 幂等键 `(STOCK_OUT, item_id, 1)` 的第三段）。
pub const MATERIAL_LINE_NO: i32 = 1;
/// 数量标度（`biz_stock_out_item.qty NUMERIC(18,4)`）。
pub const QTY_SCALE: u32 = 4;
/// 单价标度（`biz_stock_out_item.unit_cost NUMERIC(18,6)`）。
pub const UNIT_PRICE_SCALE: u32 = 6;
/// 金额标度（`biz_cost_collect.amount NUMERIC(18,4)`）。
pub const AMOUNT_SCALE: u32 = 4;

// ==================== 幂等键 ====================

/// 归集行的幂等键：`(source_type, source_id, line_no)`（与 `uk_biz_cost_collect_src` 同列序）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollectKey {
    /// 来源类型（料行恒为 [`SOURCE_STOCK_OUT`]）。
    pub source_type: String,
    /// 来源行 id（料行 = `biz_stock_out_item.id`）。
    pub source_id: Uuid,
    /// 来源行号（料行恒为 [`MATERIAL_LINE_NO`]）。
    pub line_no: i32,
}

impl CollectKey {
    /// 料行的幂等键。
    pub fn stock_out(item_id: Uuid) -> Self {
        Self {
            source_type: SOURCE_STOCK_OUT.to_string(),
            source_id: item_id,
            line_no: MATERIAL_LINE_NO,
        }
    }

    /// 排错用的可读形态（日志 / 诊断清单，不参与比较）。
    pub fn text(&self) -> String {
        format!("{}:{}:{}", self.source_type, self.source_id, self.line_no)
    }
}

// ==================== 候选行（扫描结果） ====================

/// 一条**候补**领料明细（已过账 + 挂了成本对象 + 落在期间内），由 model 查库产出。
///
/// 字段与 `biz_cost_collect` 的写入列一一对应，便于直接落库与逐列断言。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialLine {
    /// 来源行 id（`biz_stock_out_item.id`，幂等键第二段）。
    pub item_id: Uuid,
    /// 来源单 id（`biz_stock_out.id`，仅用于排错）。
    pub stock_out_id: Uuid,
    /// 来源单号（`biz_stock_out.out_no` 快照）。
    pub out_no: String,
    /// 出库日期（`biz_stock_out.out_date`，决定期间归属）。
    pub out_date: NaiveDate,
    /// 成本对象（`biz_stock_out.cost_object_id`）。
    pub cost_object_id: Uuid,
    /// 物料（`biz_stock_out_item.material_id`）。
    pub material_id: Uuid,
    /// 数量（`biz_stock_out_item.qty`）。
    pub qty: Decimal,
    /// 出库单价快照（`biz_stock_out_item.unit_cost`，移动加权）。
    pub unit_price: Decimal,
    /// 出库金额（`biz_stock_out_item.amount`，**账上真实扣减**，不重算）。
    pub amount: Decimal,
    /// 车间（成本对象的 `workshop_dept_id`；对象缺失时为 `None`）。
    pub dept_id: Option<Uuid>,
    /// 车间名快照（`sys_dept` 实时名优先，落空回落对象上的车间名快照）。
    pub dept_name: Option<String>,
}

impl MaterialLine {
    /// 本行的幂等键。
    pub fn key(&self) -> CollectKey {
        CollectKey::stock_out(self.item_id)
    }

    /// 行校验：不合口径的来源数据**报错**，不静默丢（`66064`）。
    ///
    /// 为什么不在入口拦：这些来源单是过账过的历史数据，走到这里说明账面上已经有
    /// 异常行（数量 ≤ 0 / 金额 < 0 / 物料为空）。静默跳过会让「归集金额 ≠ 领料金额」
    /// 且没人知道为什么 —— 正是 C-COST-5 想抓却抓不出的场景。
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.out_no.trim().is_empty() {
            return Err(format!("来源单 {} 的单号为空", self.stock_out_id));
        }
        if self.material_id.is_nil() {
            return Err(format!("领料单 {} 的明细行物料为空", self.out_no));
        }
        if self.qty <= Decimal::ZERO {
            return Err(format!("领料单 {} 的数量为 {}（必须 > 0）", self.out_no, self.qty));
        }
        if exceeds_scale(self.qty, QTY_SCALE) {
            return Err(format!(
                "领料单 {} 的数量 {} 超过 {QTY_SCALE} 位小数（库列 NUMERIC(18,4)）",
                self.out_no, self.qty
            ));
        }
        if self.unit_price < Decimal::ZERO {
            return Err(format!(
                "领料单 {} 的出库单价为 {}（必须 ≥ 0）",
                self.out_no, self.unit_price
            ));
        }
        if exceeds_scale(self.unit_price, UNIT_PRICE_SCALE) {
            return Err(format!(
                "领料单 {} 的出库单价 {} 超过 {UNIT_PRICE_SCALE} 位小数（库列 NUMERIC(18,6)）",
                self.out_no, self.unit_price
            ));
        }
        if self.amount < Decimal::ZERO {
            return Err(format!(
                "领料单 {} 的出库金额为 {}（必须 ≥ 0）",
                self.out_no, self.amount
            ));
        }
        if exceeds_scale(self.amount, AMOUNT_SCALE) {
            return Err(format!(
                "领料单 {} 的出库金额 {} 超过 {AMOUNT_SCALE} 位小数（库列 NUMERIC(18,4)）",
                self.out_no, self.amount
            ));
        }
        Ok(())
    }

    /// 零金额行（零成本存量领料是合法账务形态：材料进了批、成本是 0）。
    pub fn is_zero_amount(&self) -> bool {
        self.amount.is_zero()
    }
}

/// 小数位数是否超出目标列标度（`50000.0000` 用 `normalize()` 后是 4 位，不是 8 位）。
pub fn exceeds_scale(value: Decimal, scale: u32) -> bool {
    value.normalize().scale() > scale
}

// ==================== 已归集行（用于判断幂等 / 清理） ====================

/// 当期已存在的料归集行（只取 `element='MATERIAL' AND source_type='STOCK_OUT'`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingCollectRow {
    /// 归集行 id（软删时用）。
    pub id: Uuid,
    /// 幂等键。
    pub key: CollectKey,
    /// 来源单号（诊断 / 汇报用）。
    pub source_no: Option<String>,
    /// 归集金额（与来源比对：账上金额被修正过就说明这一行过期了）。
    pub amount: Decimal,
    /// 归集到的成本对象（与来源比对：领料单改挂对象后旧行必须清掉）。
    pub cost_object_id: Uuid,
}

impl ExistingCollectRow {
    /// 这一行是否已经**如实**反映来源（金额与归属都对得上）。
    ///
    /// 只比 `(cost_object_id, amount)`：物料 / 数量 / 日期 / 单号在过账后不可变
    /// （`biz_stock_out` 过账即冻结），可变且影响成本的只有这两项。
    pub fn mirrors(&self, line: &MaterialLine) -> bool {
        self.cost_object_id == line.cost_object_id && self.amount == line.amount
    }
}

// ==================== 增量计划 ====================

/// 一次「扫描 + 对已归集行取差」的结果（纯函数产出，不碰库）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectPlan {
    /// 本次要新写的行（按扫描顺序，键唯一）。
    pub to_insert: Vec<MaterialLine>,
    /// 已归集过、本次跳过的键（幂等）。
    pub already: Vec<CollectKey>,
    /// 需要软删的旧行（来源已不符合口径，或对象 / 金额与来源不再一致）。
    pub stale: Vec<ExistingCollectRow>,
    /// 候补里重复出现的键数（同一明细被扫到两次，正常为 0）。
    pub duplicated: usize,
}

impl CollectPlan {
    /// 待写金额合计。
    pub fn inserted_amount(&self) -> Decimal {
        self.to_insert.iter().map(|line| line.amount).sum()
    }

    /// 待写行里金额为 0 的条数（汇报用：让「为什么这批料的成本是 0」有答案）。
    pub fn zero_amount(&self) -> usize {
        self.to_insert.iter().filter(|line| line.is_zero_amount()).count()
    }

    /// 待清理金额合计（软删掉的旧行金额）。
    pub fn stale_amount(&self) -> Decimal {
        self.stale.iter().map(|row| row.amount).sum()
    }

    /// 是否无事可做（重复跑的最常见形态：`to_insert` 与 `stale` 都空）。
    pub fn is_noop(&self) -> bool {
        self.to_insert.is_empty() && self.stale.is_empty()
    }
}

/// 对「当期候补行」与「当期已归集行」取差。
///
/// - 候补里键重复 ⇒ 只留第一条（`duplicated` 计数），保证插入语句不会自撞唯一键；
/// - 已归集过且**如实反映来源**（对象 + 金额都对）⇒ 进 `already`
///   （**不改金额、不刷 `updated_at`**，重跑无副作用）；
/// - 已归集但已过期 ⇒ 进 `stale`（软删），且若来源仍在 ⇒ 同时进 `to_insert` 重写：
///   * 来源作废 / 改挂别的成本对象 ⇒ 候补里没有这个键；
///   * 来源还在但对象或金额变了（改挂对象、账面金额被修正）⇒ 键在、内容不一致。
///
///   两种情况都必须清旧写新，否则归集数会停在旧值上（「重复跑结果收敛」的半边）；
/// - 键在候补里但已有行过期 ⇒ 旧行进 `stale`、新行进 `to_insert`（同一事务里先删后插）。
pub fn plan(eligible: &[MaterialLine], existing: &[ExistingCollectRow]) -> CollectPlan {
    let mut seen: HashSet<CollectKey> = HashSet::new();
    let mut to_insert = Vec::new();
    let mut already = Vec::new();
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
        match existing_by_key.get(&key) {
            // 已归集且如实反映来源 ⇒ 幂等跳过（不刷 updated_at、不改金额）
            Some(row) if row.mirrors(line) => already.push(key),
            // 键在、内容过期（改挂对象 / 金额被修正）⇒ 清旧写新
            Some(row) => {
                stale.push((*row).clone());
                to_insert.push(line.clone());
            }
            None => to_insert.push(line.clone()),
        }
    }

    let eligible_keys: HashSet<CollectKey> = seen;
    stale.extend(existing.iter().filter(|row| !eligible_keys.contains(&row.key)).cloned());
    // 确定性顺序：按来源单号 + id 排，便于汇报与逐行断言
    stale.sort_by(|a, b| a.source_no.cmp(&b.source_no).then_with(|| a.id.cmp(&b.id)));

    CollectPlan {
        to_insert,
        already,
        stale,
        duplicated,
    }
}

// ==================== 段落结果（编排层出参） ====================

/// calc 第 1 段（料归集）的执行结果。
///
/// 无 HTTP 入口（设计 §5.2 只有 `POST /cost-period/calc` 一个入口，由 calc 编排卡负责），
/// 但要把「扫了几行 / 写了几行 / 跳了几行 / 清了什么」讲清楚，前端的「未归集清单」
/// 与运维排错都靠它。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialCollectOutcome {
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 扫描到的候补行数（已过账 + 挂对象 + 落在期间内）。
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
    /// 新写行里金额为 0 的条数。
    pub zero_amount: usize,
    /// 写后回查：当期料归集行数（`element='MATERIAL' AND source_type='STOCK_OUT'`，未删）。
    pub collected_rows: i64,
    /// 写后回查：当期料归集金额合计。
    pub collected_amount: Decimal,
    /// 过期行明细（最多 [`PURGED_DETAIL_LIMIT`] 条，超出只计数）。
    pub purged_detail: Vec<PurgedRow>,
}

/// 被软删的过期归集行（汇报用）。
#[derive(Debug, Clone, PartialEq)]
pub struct PurgedRow {
    /// 归集行 id。
    pub id: Uuid,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 归集金额。
    pub amount: Decimal,
}

/// [`MaterialCollectOutcome::purged_detail`] 的条数上限。
pub const PURGED_DETAIL_LIMIT: usize = 50;

/// 可选 uuid 的绑参形态：空串让 SQL 里的 `NULLIF(?::text,'')::uuid` 变成 `NULL`。
///
/// （不能用「`Option<Uuid>` 直接绑 `?::uuid`」：nik 的 `Value` 没有 `Option<Uuid>` 变体，
/// 且空串转 uuid 会直接抛 `invalid input syntax for type uuid`。）
pub fn opt_uuid_text(value: Option<Uuid>) -> String {
    value.map(|id| id.to_string()).unwrap_or_default()
}

// ==================== 错误构造 ====================

/// 400 + `66064`：来源领料行的账面数据不合归集口径（静默跳过会让对账永远差一笔）。
pub fn source_invalid(out_no: &str, reason: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_COLLECT_SOURCE_INVALID,
        format!(
            "领料单 {out_no} 的数据无法归集：{reason}。\n\
             料归集只认「已过账 + 已挂成本对象」的领用单，且金额必须取账上真实出库成本。\n\
             请先在这张出库单上把账面数据修正（或作废重开）后重跑计算"
        ),
    )
}

/// 400：期间编码非法（`yyyy-MM`）。
pub fn period_invalid(period_code: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!("期间编码必须是 yyyy-MM（收到「{period_code}」）"),
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn line(item: Uuid, amount: &str) -> MaterialLine {
        MaterialLine {
            item_id: item,
            stock_out_id: Uuid::now_v7(),
            out_no: format!("OUT-{}", &item.to_string()[..8]),
            out_date: day("2026-11-05"),
            cost_object_id: Uuid::now_v7(),
            material_id: Uuid::now_v7(),
            qty: dec("3"),
            unit_price: dec("10.000000"),
            amount: dec(amount),
            dept_id: None,
            dept_name: None,
        }
    }

    /// 已归集行夹具：如实反映来源（对象 / 金额都对得上）。
    fn existing_of(line: &MaterialLine) -> ExistingCollectRow {
        ExistingCollectRow {
            id: Uuid::now_v7(),
            key: line.key(),
            source_no: Some(line.out_no.clone()),
            amount: line.amount,
            cost_object_id: line.cost_object_id,
        }
    }

    /// 已归集行的「已过期」形态（对象换了 / 金额被修正过）。
    fn existing_stale_of(line: &MaterialLine, amount: &str, object: Uuid) -> ExistingCollectRow {
        ExistingCollectRow {
            id: Uuid::now_v7(),
            key: line.key(),
            source_no: Some(line.out_no.clone()),
            amount: dec(amount),
            cost_object_id: object,
        }
    }

    /// 已归集行夹具（来源已从候补里消失：只按来源 id + 单号 + 金额造）。
    fn existing_gone(item: Uuid, no: &str, amount: &str) -> ExistingCollectRow {
        ExistingCollectRow {
            id: Uuid::now_v7(),
            key: CollectKey::stock_out(item),
            source_no: Some(no.to_string()),
            amount: dec(amount),
            cost_object_id: Uuid::now_v7(),
        }
    }

    /// 常量与 DDL 的 CHECK 值域必须逐字一致（改库不改代码会静默写不进 / 写错要素）。
    #[test]
    fn constants_match_the_ddl() {
        assert_eq!(ELEMENT_MATERIAL, "MATERIAL");
        assert_eq!(SOURCE_STOCK_OUT, "STOCK_OUT");
        assert_eq!(OUT_TYPE_ISSUE, "ISSUE");
        assert_eq!(OUT_STATUS_POSTED, "POSTED");
        // 设计 §4.3 幂等键第三段写死 1（不是明细行号：来源 id 已经唯一定位到行）
        assert_eq!(MATERIAL_LINE_NO, 1);
        assert_eq!(QTY_SCALE, 4);
        assert_eq!(UNIT_PRICE_SCALE, 6);
        assert_eq!(AMOUNT_SCALE, 4);
    }

    /// 幂等键由 `item_id` 唯一决定：同一明细行怎么扫都是同一个键。
    #[test]
    fn key_is_stable_for_the_same_item() {
        let item = Uuid::now_v7();
        assert_eq!(CollectKey::stock_out(item), CollectKey::stock_out(item));
        assert_eq!(CollectKey::stock_out(item).text(), format!("STOCK_OUT:{item}:1"));
        let other = Uuid::now_v7();
        assert_ne!(CollectKey::stock_out(item), CollectKey::stock_out(other));
    }

    /// 首次运行：全部写。
    #[test]
    fn first_run_inserts_every_candidate() {
        let a = line(Uuid::now_v7(), "30.0000");
        let b = line(Uuid::now_v7(), "12.5000");
        let result = plan(&[a.clone(), b.clone()], &[]);
        assert_eq!(result.to_insert, vec![a, b]);
        assert!(result.already.is_empty());
        assert!(result.stale.is_empty());
        assert_eq!(result.duplicated, 0);
        assert_eq!(result.inserted_amount(), dec("42.5000"));
        assert!(!result.is_noop());
    }

    /// 验收项「重复跑不重复写」：同一个键第二次跑一律跳过，连金额都不改。
    #[test]
    fn rerun_skips_already_collected_rows() {
        let a = line(Uuid::now_v7(), "30.0000");
        let b = line(Uuid::now_v7(), "12.5000");
        let rows = vec![existing_of(&a), existing_of(&b)];
        let result = plan(&[a.clone(), b.clone()], &rows);
        assert!(result.to_insert.is_empty());
        assert_eq!(result.already.len(), 2);
        assert!(result.stale.is_empty());
        assert!(result.is_noop(), "重复跑必须是无操作");
        assert_eq!(result.inserted_amount(), Decimal::ZERO);
    }

    /// 增量：只补新增的那一行，老行不动。
    #[test]
    fn incremental_run_inserts_only_the_missing_rows() {
        let a = line(Uuid::now_v7(), "30.0000");
        let b = line(Uuid::now_v7(), "12.5000");
        let rows = vec![existing_of(&a)];
        let result = plan(&[a, b.clone()], &rows);
        assert_eq!(result.to_insert, vec![b]);
        assert_eq!(result.already.len(), 1);
        assert!(result.stale.is_empty());
    }

    /// 键在、但对象或金额与来源不一致（改挂对象 / 账面金额被修正）⇒ 清旧写新。
    ///
    /// 这是「重复跑结果收敛」的另一半：不认这一条，归集数会永远停在旧值上。
    #[test]
    fn rows_whose_object_or_amount_changed_are_rewritten() {
        let a = line(Uuid::now_v7(), "30.0000");
        let other_object = Uuid::now_v7();

        // 对象换了（金额一致）
        let rows = vec![existing_stale_of(&a, "30.0000", other_object)];
        let result = plan(std::slice::from_ref(&a), &rows);
        assert_eq!(result.to_insert, vec![a.clone()], "换对象后要重写一行");
        assert_eq!(result.stale.len(), 1, "旧对象上的行要清掉");
        assert_eq!(result.stale[0].cost_object_id, other_object);
        assert!(result.already.is_empty());
        assert_eq!(result.inserted_amount(), dec("30.0000"));

        // 金额变了（对象一致）：账上金额被修正过
        let rows = vec![existing_stale_of(&a, "29.9999", a.cost_object_id)];
        let result = plan(std::slice::from_ref(&a), &rows);
        assert_eq!(result.to_insert.len(), 1);
        assert_eq!(result.stale.len(), 1);
        assert_eq!(result.stale_amount(), dec("29.9999"));
        assert!(result.already.is_empty());
    }

    /// 来源已作废 / 改挂别的对象：旧归集行进 `stale`，等编排层软删。
    #[test]
    fn rows_whose_source_disappeared_are_marked_stale() {
        let a = line(Uuid::now_v7(), "30.0000");
        let gone = Uuid::now_v7();
        let rows = vec![existing_of(&a), existing_gone(gone, "OUT-GONE", "99.0000")];
        let result = plan(&[a], &rows);
        assert!(result.to_insert.is_empty());
        assert_eq!(result.already.len(), 1);
        assert_eq!(result.stale.len(), 1);
        assert_eq!(result.stale[0].key.source_id, gone);
        assert_eq!(result.stale_amount(), dec("99.0000"));
        assert!(!result.is_noop());
    }

    /// 扫描结果里同一个键出现两次（多桶拆行 / SQL 写错 join）只写一行，且数得出来。
    #[test]
    fn duplicated_candidates_collapse_to_one_row() {
        let a = line(Uuid::now_v7(), "30.0000");
        let mut dup = a.clone();
        dup.amount = dec("31.0000"); // 同 key 不同金额：留第一条，绝不写两行
        let result = plan(&[a.clone(), dup], &[]);
        assert_eq!(result.to_insert.len(), 1);
        assert_eq!(result.to_insert[0].amount, dec("30.0000"));
        assert_eq!(result.duplicated, 1);
    }

    /// 金额 0 的领料行照样归集（零成本存量），但要在汇报里点出来。
    #[test]
    fn zero_amount_lines_are_collected_and_reported() {
        let a = line(Uuid::now_v7(), "0");
        let mut b = line(Uuid::now_v7(), "30.0000");
        b.amount = Decimal::ZERO;
        let result = plan(&[a, b], &[]);
        assert_eq!(result.to_insert.len(), 2);
        assert_eq!(result.zero_amount(), 2);
        assert!(result.to_insert[0].is_zero_amount());
    }

    /// 行校验：账面异常一律报错并点名，不静默跳过。
    #[test]
    fn validation_rejects_broken_source_rows() {
        let item = Uuid::now_v7();

        let mut zero_qty = line(item, "30.0000");
        zero_qty.qty = Decimal::ZERO;
        assert!(zero_qty.validate().unwrap_err().contains("数量"));

        let mut negative = line(item, "-1.0000");
        negative.amount = dec("-1.0000");
        assert!(negative.validate().unwrap_err().contains("金额"));

        let mut no_material = line(item, "30.0000");
        no_material.material_id = Uuid::nil();
        assert!(no_material.validate().unwrap_err().contains("物料"));

        let mut too_precise = line(item, "30.00001");
        too_precise.amount = dec("30.00001");
        assert!(too_precise.validate().unwrap_err().contains("4 位小数"));

        let mut negative_price = line(item, "0");
        negative_price.unit_price = dec("-0.000001");
        assert!(negative_price.validate().unwrap_err().contains("单价"));

        let mut empty_no = line(item, "30.0000");
        empty_no.out_no = "  ".to_string();
        assert!(empty_no.validate().unwrap_err().contains("单号"));
    }

    /// 合法的行（含零金额、长小数单价）必须通过。
    #[test]
    fn validation_accepts_normal_rows() {
        let ok = line(Uuid::now_v7(), "30.0000");
        assert!(ok.validate().is_ok());

        let long_price = line(Uuid::now_v7(), "33.3334");
        assert!(long_price.validate().is_ok());
    }

    /// 小数位判定用 `normalize()`：`30.0000` 是 4 位而不是 8 位。
    #[test]
    fn scale_check_normalizes_trailing_zeros() {
        assert!(!exceeds_scale(dec("30.0000"), 4));
        assert!(!exceeds_scale(dec("30"), 4));
        assert!(exceeds_scale(dec("30.00001"), 4));
        assert!(!exceeds_scale(dec("1.123456"), 6));
        assert!(exceeds_scale(dec("1.1234567"), 6));
    }

    /// `stale` 的顺序要确定（汇报与逐行断言不随行序漂）。
    #[test]
    fn stale_rows_are_ordered_deterministically() {
        let older = Uuid::now_v7();
        let newer = Uuid::now_v7();
        let mut first = existing_gone(older, "OUT-B", "1.0000");
        first.id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let mut second = existing_gone(newer, "OUT-A", "2.0000");
        second.id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let result = plan(&[], &[first, second]);
        assert_eq!(result.stale[0].source_no.as_deref(), Some("OUT-A"));
        assert_eq!(result.stale[1].source_no.as_deref(), Some("OUT-B"));
    }

    /// 错误码与 HTTP 状态（前端按码映射文案，改了就断）。
    #[test]
    fn error_constructors_keep_code_and_status() {
        assert_eq!(error_code::ERP_COST_COLLECT_SOURCE_INVALID, 66064);
        let err = source_invalid("OUT-1", "数量为 0");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, 66064);
        assert!(err.msg.contains("OUT-1"));
        assert!(err.msg.contains("数量为 0"));

        let err = period_invalid("2026-13");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("2026-13"));
    }
}
