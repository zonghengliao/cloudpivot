//! 分批法「料工费归集到批次」· 契约层与纯逻辑（成本核算 · M5-03）。
//!
//! 本文件不碰库：判据（三路候选怎么算「已归集 / 待归集 / 不可归集」、状态闸怎么判、
//! 哪些旧归集行该跟着这次动）全部是「输入 → 输出」的纯函数，可单测。
//! 取数见 [`crate::business::erp::cost::cost_batch_collect_model`]，
//! 编排见 [`crate::business::erp::cost::cost_batch_collect`]。
//!
//! # 这张卡与 M5-02（归集表）的分工
//!
//! | 卡 | 给谁 | 能不能写 |
//! |---|---|---|
//! | M5-02 `GET /cost-collect-sheet`、`GET …/{id}` | 看「一批花了多少」 | **只读**：归集只能由单据产生，不给「直接改归集额」的口子 |
//! | **M5-03** `GET …/{id}/sources`、`POST …/{id}/collect` | 看「这批的三路来源各是哪几笔、哪些还没归集」+ **把本批这一期的料/工/费归集进来** | 写，但只写**由单据推导出的**归集行（不接任何金额入参） |
//!
//! 关键一句：**`POST …/collect` 的入参只有 `period_code`**。金额、行数、归集到哪
//! 个要素全部由来源单据决定（领料单 / 工时台账 / 费用台账），与 calc（期间全量扫）
//! 用的是同一套幂等键与同一组写入原语 —— 所以「先按批次归集、再跑期间 calc」
//! 不会算两遍，calc 只会把已经写进去的这批行判成 `already` 跳过。
//!
//! # 与 calc 的关系（为什么还要这个接口）
//!
//! `POST /cost-period/calc` 是**期间级**的六段流水线（料→工→费→对账→分摊→计算单），
//! 一次算全期间、清当期重灌。分批法的日常动作却是**按批**看账：
//! 「我这一批昨天领了三张料、报了 12 个工，钱进来了没有？」—— 为此跑一遍全期间 calc
//! 既慢又重（会重算别人的批次、可能被池对账拦下）。本卡给的是**只碰一个批次**的
//! 窄口径写入：不动期间批次、不动别的对象、不碰分摊与计算单。
//!
//! 代价与提示（写在出参里，不藏着）：
//!
//! * 归集行变了但 `biz_cost_period_item` 不会自动重算 ⇒ 如果本期已有 `DRAFT` 批次，
//!   出参 `recalc_required = true`，提示「要让计算单跟上请重跑 calc」；
//! * 期间批次一旦离开 `DRAFT`（`SUBMITTED/APPROVED/SETTLED/CLOSED`）就**冻结**，
//!   本卡直接 409 拒（报文本卡独有码 `66106`），不给人「审批后再改数」的缝。
//!
//! # 三路来源（ARCH-9 §4.3，与 calc 第 1/2/3 段逐条同源）
//!
//! | 要素 | 取数 | 幂等键 | 备注 |
//! |---|---|---|---|
//! | `MATERIAL` | `biz_stock_out_item`（`out_type='ISSUE'`、`status='POSTED'`、`h.cost_object_id` = 本批） | `(STOCK_OUT, item_id, 1)` | 金额取 `item.amount`（账上真实扣减），不重算 |
//! | `LABOR` | `biz_cost_labor`（`status='CONFIRMED'`、`period_code` 本期、对象 = 本批） | `(LABOR_TICKET, labor_id, 0)` | 缺费率（`rate = 0`）**不写 0 金额行**，进「不可归集」清单 |
//! | `OVERHEAD` | `biz_cost_overhead`（本期、`cost_object_id` = 本批） | `(OVERHEAD_TICKET, overhead_id, 0)` | 只收**直接挂了本批**的手工费用；折旧/报销/未挂对象的费用进车间费用池，由分摊段处理 |
//!
//! 折旧 / 报销两路恒进池（`cost_object_id IS NULL`），**不属本卡**：它们是「车间共用费用，
//! 等分摊」，硬挂到某个批次上等于凭空分账。本卡只收「明确记在某一批上」的钱。
//!
//! # 已归集行怎么取舍（为什么不是「只看本批的行」）
//!
//! 三路都按「本期 + 本批 + **键命中的外来行**」取已有归集行：
//!
//! * 对象 = 本批的旧行 ⇒ 参与「已归集 / 过期软删」判定（与 calc 同一份
//!   `plan` 纯函数）；
//! * 对象 ≠ 本批、但**幂等键正好命中本批这次要写的键** ⇒ 也要取出来（否则
//!   `INSERT … ON CONFLICT DO NOTHING` 会被那行旧值静默吞掉，领料单改挂批次后
//!   永远归集不到新批次 —— 这正是 calc 的「改挂对象」用例抓的那条路）；
//! * 其他对象、其他键的旧行 ⇒ **一行都不碰**（分批归集只管自己这一批）。
//!
//! 判据落在 [`subset_for_object`]，执行顺序（先软删、再插入）落在编排层。
//!
//! # 为什么没有「手工调金额」的口子
//!
//! 与 M5-02 模块头同一条理由：手工补料走领料单、手工补工走工时台账、手工补费走费用台账
//! ——三条路都有单据、有审计、有幂等键。开一个「直接往批次上填钱」的接口，
//! 等于让成本能绕过单据凭空出现，且永远对不回 4001 的账面发生额。

use std::collections::HashSet;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_collect_dto::{self as collect, CollectKey, ExistingCollectRow};
use crate::business::erp::cost::cost_collect_sheet_dto::{self as sheet, SheetAmounts, SheetBatchRes};
use crate::business::erp::cost::cost_object_dto;
use crate::business::erp::cost::cost_overhead_collect_dto::ExistingOverheadRow;
use crate::business::erp::cost::cost_period_dto;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（复用 · 不新增码、不新增菜单） ====================
//
// 为什么复用：设计 §5.3 的权限码清单是定稿，新开码必须种 `sys_menu` 才有意义，
// 而页面属 M5-07 —— 插一条指向不存在组件的菜单会让侧栏点开 404（与 M5-02 同口径）。
//
// * 读（三路来源清单）：`cost-object:list` —— 与 M5-02 的归集表/明细同码，
//   同一批数据不多露一分；
// * 写（归集执行）：`cost-period:calc` —— 语义就是「算成本」：calc 的第 1/2/3 段
//   做的正是这件事，本接口只是把它收窄到单个批次。反过来给个新码，等于让
//   「能算全期间」的人不能算单批、或让「能算单批」的人能绕过 calc 的口径。

/// 读三路来源（**复用** `cost-object:list`）。
pub const PERM_READ: &str = "business:erp:cost-object:list";

/// 执行归集（**复用** `cost-period:calc`）。
pub const PERM_COLLECT: &str = "business:erp:cost-period:calc";

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形，便于汇总脚本收集）。
pub const PERMISSIONS: [&str; 2] = [PERM_READ, PERM_COLLECT];

// ==================== 要素 ====================

/// 直接材料。
pub const ELEMENT_MATERIAL: &str = collect::ELEMENT_MATERIAL;
/// 直接人工。
pub const ELEMENT_LABOR: &str = "LABOR";
/// 制造费用。
pub const ELEMENT_OVERHEAD: &str = "OVERHEAD";

// ==================== 一行来源的三种状态 ====================

/// 已经归集过、且如实反映来源（本次不会重写）。
pub const STATE_COLLECTED: &str = "COLLECTED";
/// 本次会写进去（新行，或「来源变了、旧行会被清掉重写」）。
pub const STATE_PENDING: &str = "PENDING";
/// 来源本身不合归集口径（工时缺费率 / 费用金额为 0）：**不写**，给出原因。
pub const STATE_UNCOLLECTABLE: &str = "UNCOLLECTABLE";

/// 状态 → 中文（前端直出，不用自己翻译）。
pub fn state_label(state: &str) -> &'static str {
    match state {
        STATE_COLLECTED => "已归集",
        STATE_PENDING => "待归集",
        STATE_UNCOLLECTABLE => "不可归集",
        _ => "未知",
    }
}

/// 某一路候选行的状态判定（纯函数，便于单测）：已归集 > 待归集 > 不可归集。
///
/// 已归集优先：一行既在 `already` 又在 `uncollected` 是不可能的（`plan` 的互斥保证），
/// 但**判据顺序写死在这里**，避免以后有人改 plan 时两侧语义漂移。
pub fn line_state(
    key: &CollectKey,
    already: &HashSet<CollectKey>,
    pending: &HashSet<CollectKey>,
) -> (&'static str, &'static str) {
    let state = if already.contains(key) {
        STATE_COLLECTED
    } else if pending.contains(key) {
        STATE_PENDING
    } else {
        STATE_UNCOLLECTABLE
    };
    (state, state_label(state))
}

// ==================== 状态闸（期间批次 / 批次自身） ====================

/// 一离开 `DRAFT` 就冻结的期间批次状态（设计 §4.1：`APPROVED` 之后数据冻结；
/// 而 `SUBMITTED` 在审批中，改数会让审批人看到的数与落库的数不一致 ⇒ 一并冻结）。
///
/// `CANCELLED` / `REVERSED` **不在**此列：那是「这个批次已经废掉」，期间回到可重算状态
/// （V0373 的唯一键也排除它们）。
pub const FROZEN_PERIOD_STATUSES: [&str; 4] = [
    cost_period_dto::STATUS_SUBMITTED,
    cost_period_dto::STATUS_APPROVED,
    cost_period_dto::STATUS_SETTLED,
    cost_period_dto::STATUS_CLOSED,
];

/// 一条归集闸门（`None` = 放行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectBlock {
    /// 一句话说清「为什么不能归集」（点名批次号 / 期间批次号与当前状态）。
    pub reason: String,
    /// 怎么修（给前端 tooltip / 报错第二段，用户在界面上就能自救）。
    pub fix: String,
}

/// 期间批次的现状（只带闸门需要的两列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodBatchRef {
    /// 批次号 `CS-yyyyMM-0001`。
    pub batch_no: String,
    /// 状态。
    pub status: String,
}

impl PeriodBatchRef {
    /// 该期间批次是否已冻结（`SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED`）。
    pub fn is_frozen(&self) -> bool {
        FROZEN_PERIOD_STATUSES.contains(&self.status.as_str())
    }
}

/// 归集闸门判定（纯函数）。
///
/// 顺序有意为之：**先说批次自身**（关批 / 作废）再说期间批次 —— 批次已作废时
/// 「本期批次已提交」不是用户该先处理的事，先开对批次才是。
pub fn block_of(batch_no: &str, object_status: &str, period: Option<&PeriodBatchRef>) -> Option<CollectBlock> {
    if object_status == cost_object_dto::STATUS_CLOSED {
        return Some(CollectBlock {
            reason: format!("批次 {batch_no} 已关批（CLOSED），关批后成本不再变动"),
            fix: "关批批次只读：需要继续投入请在制品开新批次；确属错关请走关批撤销后重开".to_string(),
        });
    }
    if object_status == cost_object_dto::STATUS_CANCELLED {
        return Some(CollectBlock {
            reason: format!("批次 {batch_no} 已作废（CANCELLED），不能再归集"),
            fix: "作废批次只留痕：请改挂到在产的批次上，或新建批次后重开领料/工时".to_string(),
        });
    }
    if let Some(period) = period
        && period.is_frozen()
    {
        let label = cost_period_dto::status_label(&period.status);
        return Some(CollectBlock {
            reason: format!(
                "本期成本批次 {} 状态「{label}」（{}），数据已冻结，不能再归集",
                period.batch_no, period.status
            ),
            fix: if period.status == cost_period_dto::STATUS_SUBMITTED {
                "审批中：先撤回（withdraw）回到草稿，归集完再提交".to_string()
            } else if period.status == cost_period_dto::STATUS_APPROVED {
                "已审批：数据冻结（设计 §4.1），不许再改；确需调整请作废后重建批次".to_string()
            } else {
                "已结算/已关期：先红冲该批次（reverse），同期间可重建批次再归集".to_string()
            },
        });
    }
    None
}

/// 409 + `66106`：归集被状态闸拦下（报文两段：为什么 + 怎么修）。
pub fn not_collectable(block: &CollectBlock) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_BATCH_COLLECT_BLOCKED,
        format!("{}\n需要怎么做：{}", block.reason, block.fix),
    )
}

// ==================== 已归集行的取舍（纯逻辑） ====================

/// 「已经有归集行的来源」三种形态的公共面（料/工一张结构、费一张结构）。
pub trait ExistingCollectRef {
    /// 归集行 id（软删时用）。
    fn row_id(&self) -> Uuid;
    /// 幂等键。
    fn collect_key(&self) -> &CollectKey;
    /// 归集到的成本对象（费行的池行为 `None`）。
    fn object_id(&self) -> Option<Uuid>;
}

impl ExistingCollectRef for ExistingCollectRow {
    fn row_id(&self) -> Uuid {
        self.id
    }

    fn collect_key(&self) -> &CollectKey {
        &self.key
    }

    fn object_id(&self) -> Option<Uuid> {
        Some(self.cost_object_id)
    }
}

impl ExistingCollectRef for ExistingOverheadRow {
    fn row_id(&self) -> Uuid {
        self.id
    }

    fn collect_key(&self) -> &CollectKey {
        &self.key
    }

    fn object_id(&self) -> Option<Uuid> {
        self.cost_object_id
    }
}

/// 从「本期全部已归集行」里筛出**这次该看的那些行**（纯函数，见模块头末段三条）。
///
/// 保留条件（或）：
///
/// 1. 归集到的对象 = 本批（参与已归集 / 过期判定）；
/// 2. 幂等键命中 `candidate_keys`（哪怕对象是别的批次：源单改挂后旧行必须被清掉）。
///
/// 其余行原样留给别的批次 —— 分批归集不许「顺手清理」别人的行。
pub fn subset_for_object<R>(existing: &[R], object_id: Uuid, candidate_keys: &[CollectKey]) -> Vec<R>
where
    R: ExistingCollectRef + Clone,
{
    let keys: HashSet<&CollectKey> = candidate_keys.iter().collect();
    existing
        .iter()
        .filter(|row| row.object_id() == Some(object_id) || keys.contains(row.collect_key()))
        .cloned()
        .collect()
}

// ==================== 请求 ====================

/// 三路来源预览的查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchSourceQuery {
    /// 会计期间 `yyyy-MM`（必填：归集额是「期间 × 批次」的量）。
    pub period_code: Option<String>,
}

/// 归集执行的入参。
///
/// **只有期间**：金额 / 行数 / 归集到哪个要素全部由来源单据决定，不接受外部指定
/// （否则等于绕过单据凭空造成本）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchCollectReq {
    /// 会计期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
}

/// 期间编码归一（必填 + `yyyy-MM` 形状 + 真实月）：与 M5-02 归集表**同一个**校验入口，
/// 两处口径不许分叉。
pub fn period_of(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    sheet::period_of(raw)
}

// ==================== 出参：预览 ====================

/// 期间批次（本期是否已有成本批次、状态、能不能改）。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodBatchRes {
    /// 批次号 `CS-yyyyMM-0001`。
    pub batch_no: String,
    /// 状态码。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 是否已冻结（冻结就不能再归集；本字段为 `true` 时 `collectable` 必为 `false`）。
    pub frozen: bool,
}

impl PeriodBatchRes {
    /// 由闸门结构构造。
    pub fn of(period: &PeriodBatchRef) -> Self {
        Self {
            batch_no: period.batch_no.clone(),
            status: period.status.clone(),
            status_label: cost_period_dto::status_label(&period.status).to_string(),
            frozen: period.is_frozen(),
        }
    }
}

/// 一路来源里的一行（领料明细 / 工时行 / 手工费用行）。
#[derive(Debug, Clone, Serialize)]
pub struct SourceLineRes {
    /// 要素（`MATERIAL` / `LABOR` / `OVERHEAD`）。
    pub element: &'static str,
    /// 来源类型（`STOCK_OUT` / `LABOR_TICKET` / `OVERHEAD_TICKET`）。
    pub source_type: String,
    /// 来源行 id（点回原单据）。
    pub source_id: String,
    /// 来源单号（工时台账无单号列 ⇒ `None`）。
    pub source_no: Option<String>,
    /// 发生日期（料 = 出库日 / 工 = 工作日期 / 费 = 录入日）。
    pub occur_date: String,
    /// 人话描述（「领料单 OUT-… 物料 X 10」「工时台账 2026-11-03 张三 8h×30」）。
    pub label: String,
    /// 数量（只有料行有）。
    pub qty: Option<String>,
    /// 单价（只有料行有）。
    pub unit_price: Option<String>,
    /// 工时（只有工行有）。
    pub hours: Option<String>,
    /// 工资率（只有工行有）。
    pub rate: Option<String>,
    /// 物料 id（只有料行有）。
    pub material_id: Option<String>,
    /// 物料编码（只有料行有，批量 join 补名）。
    pub material_code: Option<String>,
    /// 物料名称（同上）。
    pub material_name: Option<String>,
    /// 制造费用科目（只有费行有）。
    pub account_code: Option<String>,
    /// 车间名（快照/实时名，见各段口径）。
    pub dept_name: Option<String>,
    /// 归集状态（[`STATE_COLLECTED`] / [`STATE_PENDING`] / [`STATE_UNCOLLECTABLE`]）。
    pub state: &'static str,
    /// 状态中文。
    pub state_label: &'static str,
    /// 不可归集的原因（只有 `UNCOLLECTABLE` 有）。
    pub reason: Option<String>,
    /// 归集金额（不可归集行为来源金额，仅作对照；真正写库的金额见 `pending_amount`）。
    pub amount: String,
}

/// 一个要素的三路汇总。
#[derive(Debug, Clone, Serialize)]
pub struct ElementSourcesRes {
    /// 要素。
    pub element: &'static str,
    /// 要素中文。
    pub element_label: &'static str,
    /// 候选来源行（已按日期 + 单号排序）。
    pub rows: Vec<SourceLineRes>,
    /// 候选行数。
    pub rows_total: usize,
    /// 候选金额合计（= 已归集 + 待归集 + 不可归集）。
    pub amount_total: String,
    /// 当前已归集的行数（本批本期）。
    pub collected_rows: i64,
    /// 当前已归集的金额（本批本期）。
    pub collected_amount: String,
    /// 本次将写入的行数。
    pub pending_rows: usize,
    /// 本次将写入的金额。
    pub pending_amount: String,
    /// 本次将被软删的过期行数（来源作废 / 改挂 / 金额被修正）。
    pub purged_rows: usize,
    /// 本次将被软删的金额。
    pub purged_amount: String,
    /// 不可归集的行数（缺费率 / 金额为 0）。
    pub uncollected_rows: usize,
    /// 不可归集的金额（参照用）。
    pub uncollected_amount: String,
    /// 归集后本批本期该要素合计 = `collected_amount − purged_amount + pending_amount`。
    pub after_amount: String,
}

/// 「料工费归集到批次」的来源预览。
#[derive(Debug, Clone, Serialize)]
pub struct BatchSourcesRes {
    /// 会计期间。
    pub period_code: String,
    /// 批次抬头（与 M5-02 明细同一个结构，前端一套组件通用）。
    pub batch: SheetBatchRes,
    /// 本期成本批次（没有则 `None`：还没跑过 calc）。
    pub period_batch: Option<PeriodBatchRes>,
    /// 现在能不能归集（闸门 + 期间批次状态）。
    pub collectable: bool,
    /// 不能归集的原因（`collectable = false` 时必有）。
    pub block_reason: Option<String>,
    /// 不能归集怎么修。
    pub block_fix: Option<String>,
    /// 归集后是否需要重算期间批次（本期已有 `DRAFT` 批次 ⇒ `true`）。
    pub recalc_required: bool,
    /// 直接材料。
    pub material: ElementSourcesRes,
    /// 直接人工。
    pub labor: ElementSourcesRes,
    /// 制造费用（只含直接挂本批的手工费用）。
    pub overhead: ElementSourcesRes,
    /// 三项合计（当前已归集口径）。
    pub totals: SheetAmounts,
    /// 归集后三项合计。
    pub after_totals: SheetAmounts,
}

// ==================== 出参：执行 ====================

/// 一个要素的归集执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct ElementCollectRes {
    /// 要素。
    pub element: &'static str,
    /// 要素中文。
    pub element_label: &'static str,
    /// 扫描到的候选行数（本批本期）。
    pub scanned: usize,
    /// 本次新写的行数。
    pub inserted: usize,
    /// 已归集过、本次跳过的行数（幂等）。
    pub skipped_existing: usize,
    /// 本次软删的过期行数。
    pub purged: usize,
    /// 本次新写金额。
    pub inserted_amount: String,
    /// 本次软删金额。
    pub purged_amount: String,
    /// 写后回查：本批本期该要素的归集行数。
    pub collected_rows: i64,
    /// 写后回查：本批本期该要素的归集金额。
    pub collected_amount: String,
    /// 不可归集行数（缺费率 / 金额为 0），**不含在 `collected_*` 里**。
    pub uncollected_rows: usize,
    /// 不可归集金额（参照用）。
    pub uncollected_amount: String,
}

/// 单批次归集的执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct BatchCollectRes {
    /// 会计期间。
    pub period_code: String,
    /// 批次抬头。
    pub batch: SheetBatchRes,
    /// 本期成本批次（没有则 `None`）。
    pub period_batch: Option<PeriodBatchRes>,
    /// 归集行变了但期间批次未重算 ⇒ 提示重跑 calc。
    pub recalc_required: bool,
    /// 直接材料。
    pub material: ElementCollectRes,
    /// 直接人工。
    pub labor: ElementCollectRes,
    /// 制造费用。
    pub overhead: ElementCollectRes,
    /// 写后本批本期三项合计（回查值，不是内存累加）。
    pub totals: SheetAmounts,
    /// 人话结论（前端 Toast 直出）。
    pub message: String,
}

/// 要素中文名（出参与报文共用）。
pub fn element_label(element: &str) -> &'static str {
    match element {
        ELEMENT_MATERIAL => "直接材料",
        ELEMENT_LABOR => "直接人工",
        _ => "制造费用",
    }
}

/// 金额 → 文本（固定 4 位小数的口径由 `amount_text` 保证，与 M5-02 同源）。
pub fn amount_text(value: Decimal) -> String {
    sheet::amount_text(value)
}

/// `collected − purged + pending`（归集后合计，纯函数）。
///
/// 夹到 0：理论上不会为负（旧行金额来自同一批来源），但浮点/标度不该让前端看到
/// 「−0.0000」。夹零只影响展示，不影响落库（落库值一律回查）。
pub fn after_amount(collected: Decimal, purged: Decimal, pending: Decimal) -> Decimal {
    let value = collected - purged + pending;
    if value < Decimal::ZERO { Decimal::ZERO } else { value }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_object_dto::STATUS_CANCELLED;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn key(source_type: &str, line_no: i32) -> CollectKey {
        CollectKey {
            source_type: source_type.to_string(),
            source_id: Uuid::now_v7(),
            line_no,
        }
    }

    fn existing(key: CollectKey, object: Uuid, amount: &str) -> ExistingCollectRow {
        ExistingCollectRow {
            id: Uuid::now_v7(),
            key,
            source_no: Some("OUT-1".to_string()),
            amount: Decimal::from_str(amount).expect("金额"),
            cost_object_id: object,
        }
    }

    fn overhead_existing(key: CollectKey, object: Option<Uuid>) -> ExistingOverheadRow {
        ExistingOverheadRow {
            id: Uuid::now_v7(),
            key,
            source_no: None,
            amount: Decimal::from_str("10").expect("金额"),
            cost_object_id: object,
        }
    }

    // ---------- 权限码 ----------

    #[test]
    fn permissions_are_reused_from_existing_codes() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(PERM_COLLECT, "business:erp:cost-period:calc");
        assert_eq!(PERMISSIONS.len(), 2);
        // 两个码都是 V0378 已种进 `sys_menu` 的码，本卡不新增菜单
        assert!(PERMISSIONS.iter().all(|code| code.starts_with("business:erp:")));
    }

    // ---------- 状态判定 ----------

    #[test]
    fn line_state_prefers_collected_then_pending_then_uncollectable() {
        let collected = key("STOCK_OUT", 1);
        let pending = key("STOCK_OUT", 1);
        let blocked = key("STOCK_OUT", 1);
        let already: HashSet<CollectKey> = [collected.clone()].into_iter().collect();
        let to_insert: HashSet<CollectKey> = [pending.clone()].into_iter().collect();

        assert_eq!(
            line_state(&collected, &already, &to_insert),
            (STATE_COLLECTED, "已归集")
        );
        assert_eq!(line_state(&pending, &already, &to_insert), (STATE_PENDING, "待归集"));
        assert_eq!(
            line_state(&blocked, &already, &to_insert),
            (STATE_UNCOLLECTABLE, "不可归集")
        );
        assert_eq!(state_label("WHATEVER"), "未知");
    }

    // ---------- 闸门 ----------

    #[test]
    fn open_object_and_no_period_batch_is_allowed() {
        assert_eq!(block_of("CB-1", cost_object_dto::STATUS_OPEN, None), None);
    }

    #[test]
    fn draft_period_batch_is_allowed() {
        let period = PeriodBatchRef {
            batch_no: "CS-202611-0001".to_string(),
            status: cost_period_dto::STATUS_DRAFT.to_string(),
        };
        assert!(!period.is_frozen());
        assert_eq!(block_of("CB-1", cost_object_dto::STATUS_OPEN, Some(&period)), None);
    }

    #[test]
    fn frozen_period_batches_are_blocked_with_actionable_fix() {
        for status in FROZEN_PERIOD_STATUSES {
            let period = PeriodBatchRef {
                batch_no: "CS-202611-0001".to_string(),
                status: status.to_string(),
            };
            let block = block_of("CB-9", cost_object_dto::STATUS_OPEN, Some(&period)).expect("冻结状态必须拦下");
            assert!(block.reason.contains("CS-202611-0001"), "报文要点名期间批次号");
            assert!(block.reason.contains(status), "报文要点名状态码");
            assert!(!block.fix.is_empty(), "必须给怎么修");
        }
    }

    #[test]
    fn cancelled_and_reversed_period_batches_do_not_block() {
        for status in [cost_period_dto::STATUS_CANCELLED, cost_period_dto::STATUS_REVERSED] {
            let period = PeriodBatchRef {
                batch_no: "CS-202611-0001".to_string(),
                status: status.to_string(),
            };
            assert!(
                block_of("CB-1", cost_object_dto::STATUS_OPEN, Some(&period)).is_none(),
                "{status} 是已废批次，期间回到可重算状态"
            );
        }
    }

    #[test]
    fn closed_or_cancelled_object_is_blocked_before_period_check() {
        // 批次已关批 + 期间批次也冻结时，先说的是「批次关批」（那才是先要处理的）
        let period = PeriodBatchRef {
            batch_no: "CS-202611-0001".to_string(),
            status: cost_period_dto::STATUS_SETTLED.to_string(),
        };
        let closed = block_of("CB-7", cost_object_dto::STATUS_CLOSED, Some(&period)).expect("必须拦");
        assert!(closed.reason.contains("已关批"));
        assert!(!closed.reason.contains("CS-202611-0001"));

        let cancelled = block_of("CB-8", STATUS_CANCELLED, None).expect("必须拦");
        assert!(cancelled.reason.contains("已作废"));
        assert!(cancelled.fix.contains("新建批次"));
    }

    #[test]
    fn not_collectable_uses_the_new_error_code() {
        let block = CollectBlock {
            reason: "批次已关批".to_string(),
            fix: "开新批次".to_string(),
        };
        let error = not_collectable(&block);
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_BATCH_COLLECT_BLOCKED);
        assert!(error.msg.contains("批次已关批"));
        assert!(error.msg.contains("需要怎么做"));
    }

    // ---------- 已归集行取舍 ----------

    #[test]
    fn subset_keeps_own_rows_and_key_hits_but_not_others() {
        let me = Uuid::now_v7();
        let other = Uuid::now_v7();
        let mine = existing(key("STOCK_OUT", 1), me, "10");
        let foreign_hit = existing(key("STOCK_OUT", 1), other, "10");
        let foreign_other = existing(key("STOCK_OUT", 1), other, "10");
        let candidates = vec![mine.key.clone(), foreign_hit.key.clone()];

        let rows = vec![mine.clone(), foreign_hit.clone(), foreign_other.clone()];
        let subset = subset_for_object(&rows, me, &candidates);

        assert_eq!(subset.len(), 2, "本批行 + 键命中的外来行");
        assert!(subset.iter().any(|row| row.id == mine.id));
        assert!(subset.iter().any(|row| row.id == foreign_hit.id));
        assert!(
            !subset.iter().any(|row| row.id == foreign_other.id),
            "别的批次、且键没命中的行一行都不许动"
        );
    }

    #[test]
    fn subset_works_for_overhead_rows_with_null_object() {
        let me = Uuid::now_v7();
        let pool = overhead_existing(key("DEPRECIATION", 0), None);
        let mine = overhead_existing(key("OVERHEAD_TICKET", 0), Some(me));
        let candidates = vec![mine.key.clone()];

        let subset = subset_for_object(&[pool.clone(), mine.clone()], me, &candidates);
        assert_eq!(subset.len(), 1);
        assert_eq!(subset[0].id, mine.id, "池行（无对象）不参与本批归集");
    }

    #[test]
    fn subset_is_empty_when_nothing_matches() {
        let me = Uuid::now_v7();
        let other = Uuid::now_v7();
        let rows = vec![existing(key("STOCK_OUT", 1), other, "1")];
        assert!(subset_for_object(&rows, me, &[]).is_empty());
    }

    // ---------- 合计与文案 ----------

    #[test]
    fn after_amount_subtracts_purged_and_floors_at_zero() {
        let dec = |text: &str| Decimal::from_str(text).expect("金额");
        assert_eq!(
            after_amount(dec("100"), dec("20"), dec("5")),
            dec("85"),
            "归集后 = 已归集 − 过期 + 新增"
        );
        assert_eq!(
            after_amount(dec("0"), dec("10"), dec("0")),
            Decimal::ZERO,
            "不许出现负数"
        );
    }

    #[test]
    fn element_labels_match_the_sheet_vocabulary() {
        assert_eq!(element_label("MATERIAL"), "直接材料");
        assert_eq!(element_label("LABOR"), "直接人工");
        assert_eq!(element_label("OVERHEAD"), "制造费用");
        // 与 M5-02 归集表的中文名逐字一致（同一列在两页上叫两个名会让人以为不是一个数）
        assert_eq!(element_label("MATERIAL"), sheet::element_label("MATERIAL"));
        assert_eq!(element_label("OVERHEAD"), sheet::element_label("OVERHEAD"));
    }

    #[test]
    fn period_is_required_and_must_be_month_shaped() {
        let empty = period_of(None).expect_err("缺期间必须 400");
        assert_eq!(empty.status, 400);
        let blank = period_of(Some("   ")).expect_err("空白期间必须 400");
        assert_eq!(blank.status, 400);
        let bad = period_of(Some("2026-13")).expect_err("非法月份必须 400");
        assert_eq!(bad.status, 400);
        assert_eq!(period_of(Some(" 2031-07 ")).expect("合法期间"), "2031-07");
    }
}
