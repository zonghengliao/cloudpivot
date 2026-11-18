//! 分批法「完工批次结转（完工转出）」· 契约层与纯逻辑（成本核算 · M5-04）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.6（在制与完工）、
//! §4.7.4（完工入库结转借贷科目）、§4.11（C-COST-2 / C-COST-3 两条不变量）。
//!
//! # 这个模块回答什么问题
//!
//! 一批货做完了（车间在「产量与在制」里把 `wip_qty` 填成 0、`completed_qty` 填成产出数），
//! 这一批**累计归集**的料 / 工 / 费要**整批转出**成完工成本：
//!
//! ```text
//! 本次结转 = 累计归集（三要素） − 已结转（历史结转单之和）
//! 单位成本 = 本次结转合计 / 完工数量        （round 6，与 §4.6 ⑤ 同标度）
//! ```
//!
//! # 分批法为什么是「全额转出」而不是「约当产量分摊」
//!
//! 约当产量法（calc 第 6 段 `cost_calc_item`）回答的是「料工费在完工与在制之间怎么分」，
//! 那是品种法 / 在制品的算法。分批法的核算单位是**批**：批没完 → 一分不转（成本全留在在制）；
//! 批完了 → 累计全转（在制归零）。所以本模块只有一条硬闸：**`wip_qty > 0` 一律拒绝结转**，
//! 报文写清「分批法不按约当分摊，请等整批完工，或改用期间结算（calc 约当产量）」——
//! 绝不在这里偷偷按比例摊一刀（那会让「这批到底花了多少」变成猜的）。
//!
//! # 为什么允许「差额结转」（同一批可以结转多次）
//!
//! 结转之后仍可能有**迟到的真成本**（结转后才过账的领料、才确认的工时）。这时候把批次
//! 当成「已经结死」会让真金白银永久留在在制里。所以每次结转只写**差额**：
//! `累计归集 − 已结转`。差为 0 ⇒ 幂等返回（`inserted = false`），不写空单、也不报错。
//!
//! # 为什么这里**不出凭证、不建入库单**（边界，不是待办）
//!
//! ARCH-9 的 D-1 定死了「存货计价只有一本账」与「成本对象层是工作底稿，唯一出口是
//! 完工入库金额 + 三张结转凭证」，而那条出口由**期间结算**（B3-4/B3-5/B3-6）独占：
//! 完工入库单必须挂 `cost_period_id`（`ck_biz_si_production` 库层约束），结转凭证必须与
//! 入库金额同源（C-COST-2）。批次级结转若自己再发一张「借 1405 / 贷 4001」的凭证：
//!
//! * C-COST-2（`Σ 生产入库金额 = 完工凭证借 1405`）当场恒红 —— 有 1405 借方却没有入库单；
//! * C-COST-3（`4001 余额 = Σ 最新已结算在制 + 在途`）也恒红 —— 有人贷记 4001 却没减在制。
//!
//! 所以本卡把「批次完工转出」做成**成本工作底稿**（`biz_cost_batch_finish`），
//! 账务出口仍然是期间结算；两者的分工写进出参 `warnings`（用户在界面上能看见）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_batch_collect_dto::{self as collect_dto, PeriodBatchRef};
use crate::business::erp::cost::cost_collect_sheet_dto::SheetBatchRes;
use crate::business::erp::cost::cost_object_dto;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（复用 · 与 M5-02 / M5-03 同口径） ====================

/// 读结转试算（**复用** `cost-object:list`：同一批数据，不多露一分）。
pub const PERM_READ: &str = collect_dto::PERM_READ;

/// 执行结转（**复用** `cost-period:calc`：「算成本」的写动作，与 M5-03 归集同码）。
///
/// 为什么不复用一个新码：新码要种 `sys_menu`，而批次成本页属 M5-07；先插一条指向
/// 不存在组件的菜单行，侧栏点开就是 404。为什么不复用 `cost-period:settle`
/// （结算（入库+凭证））：本卡**不出凭证、不建入库单**（见模块头），拿「结算」的码
/// 会让人以为这里动了账。
pub const PERM_FINISH: &str = collect_dto::PERM_COLLECT;

/// 本模块全部权限码。
pub const PERMISSIONS: [&str; 2] = [PERM_READ, PERM_FINISH];

// ==================== 值域 / 标度 ====================

/// 要素：料（与归集表同一套字面量，不另起一套）。
pub const ELEMENT_MATERIAL: &str = collect_dto::ELEMENT_MATERIAL;
/// 要素：工。
pub const ELEMENT_LABOR: &str = collect_dto::ELEMENT_LABOR;
/// 要素：费。
pub const ELEMENT_OVERHEAD: &str = collect_dto::ELEMENT_OVERHEAD;

/// 金额标度（`NUMERIC(18,4)`）。
pub const AMOUNT_SCALE: u32 = 4;

/// 单位成本标度（`NUMERIC(18,6)`，与 §4.6 ⑤「round(finished / FQ, 6)」一致）。
pub const UNIT_COST_SCALE: u32 = 6;

/// 结转被拦下的统一业务码（`66107`，见 `shared::error_code`）。
pub const BLOCKED_CODE: i32 = error_code::ERP_COST_BATCH_FINISH_BLOCKED;

// ==================== 拦截分类码（前端据此给不同引导） ====================

/// 批次已作废。
pub const BLOCK_CANCELLED: &str = "BATCH_CANCELLED";
/// 该批已随**期间结算**结转（计算单已 `SETTLED`）：账已经走了，别再补一张工作底稿。
pub const BLOCK_ALREADY_SETTLED: &str = "ALREADY_SETTLED";
/// 还有在制（`wip_qty > 0`）：分批法整批完工后才结转。
pub const BLOCK_WIP_NOT_ZERO: &str = cost_object_dto::BLOCK_WIP_NOT_ZERO;
/// 完工数量为 0：没有产出就不存在「完工转出」。
pub const BLOCK_NO_FINISHED_QTY: &str = "NO_FINISHED_QTY";
/// 累计归集为 0：零成本完工是数据错误（§4.9「有完工但没投料」同款）。
pub const BLOCK_NO_COST: &str = "NO_COLLECTED_COST";
/// 还有草稿工时（成本还会变）。
pub const BLOCK_DRAFT_LABOR: &str = cost_object_dto::BLOCK_DRAFT_LABOR;
/// 已过账但未归集的领料（成本还没进筐）。
pub const BLOCK_UNCOLLECTED_STOCK_OUT: &str = cost_object_dto::BLOCK_UNCOLLECTED_STOCK_OUT;
/// 还有未过账（草稿 / 待审）的领料单（过账后会改成本）。
pub const BLOCK_UNPOSTED_STOCK_OUT: &str = cost_object_dto::BLOCK_UNPOSTED_STOCK_OUT;
/// 本批有活动的期间已被期间结算锁定（`APPROVED` 之后）。
pub const BLOCK_PERIOD_LOCKED: &str = cost_object_dto::BLOCK_PERIOD_LOCKED;
/// 结转期间自身的成本批次已冻结（`SUBMITTED` 之后）。
pub const BLOCK_PERIOD_FROZEN: &str = "PERIOD_FROZEN";

// ==================== 入参 ====================

/// 结转试算的查询参数（`period_code` 必填：结转单要记「这笔结转落在哪个会计期间」）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchFinishQuery {
    /// 结转期间 `yyyy-MM`。
    pub period_code: Option<String>,
}

/// 执行结转的请求体。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchFinishReq {
    /// 结转期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 备注（写进结转单；留痕用，不参与计算）。
    pub remark: Option<String>,
}

// ==================== 金额三要素（纯值对象） ====================

/// 料 / 工 / 费三要素金额。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Amounts {
    /// 料。
    pub material: Decimal,
    /// 工。
    pub labor: Decimal,
    /// 费。
    pub overhead: Decimal,
}

impl Amounts {
    /// 三项合计。
    pub fn total(&self) -> Decimal {
        self.material + self.labor + self.overhead
    }

    /// 是否三项全 0。
    pub fn is_zero(&self) -> bool {
        self.total().is_zero()
    }

    /// 逐要素相减并**夹到 0**（`self − other`）。
    ///
    /// 夹零的理由：历史脏数据（结转单被手工改过、来源行被软删）可能让「已结转 > 累计归集」。
    /// 这时差额应当是 0（不再转），而不是负数（负结转 = 凭空冲回成本，比少转危险）。
    pub fn saturating_sub(&self, other: &Self) -> Self {
        let clip = |value: Decimal| if value < Decimal::ZERO { Decimal::ZERO } else { value };
        Self {
            material: clip(self.material - other.material),
            labor: clip(self.labor - other.labor),
            overhead: clip(self.overhead - other.overhead),
        }
    }
}

/// 三要素金额的出参形态（金额一律字符串，防前端 JS number 精度丢失）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AmountsRes {
    /// 料。
    pub material: String,
    /// 工。
    pub labor: String,
    /// 费。
    pub overhead: String,
    /// 合计。
    pub total: String,
}

impl AmountsRes {
    /// 由值对象构造（标度统一到 4 位）。
    pub fn of(amounts: &Amounts) -> Self {
        Self {
            material: amount_text(amounts.material),
            labor: amount_text(amounts.labor),
            overhead: amount_text(amounts.overhead),
            total: amount_text(amounts.total()),
        }
    }
}

// ==================== 判定用的事实（查库结果） ====================

/// 结转判定需要的事实，全部由 model 层查好后塞进来（纯函数只认这个结构）。
///
/// 拆成「事实 + 判定」两段的原因与 `cost_object_dto::CloseFacts` 一致：真库用例在没配
/// `ERP_DB_URL` 时是 SKIP，判定若只靠真库就等于没测。
#[derive(Debug, Clone)]
pub struct FinishFacts {
    /// 批次当前状态（`OPEN` / `CLOSED` / `CANCELLED`）。
    pub object_status: String,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 累计归集（全期间、活行）。
    pub collected: Amounts,
    /// 已结转（历史结转单之和）。
    pub transferred: Amounts,
    /// 该批已 `SETTLED` 的计算单行数（`biz_cost_period_item`）。
    pub settled_items: i64,
    /// 草稿工时行数。
    pub draft_labor: i64,
    /// 已过账未归集的领料单数。
    pub uncollected_stock_out: i64,
    /// 未过账领料单数。
    pub unposted_stock_out: i64,
    /// 该对象有活动、且期间批次已锁定的期间 `(period_code, status)`。
    pub locked_periods: Vec<(String, String)>,
    /// 结转期间的成本批次（无 = 该期还没建批次）。
    pub period_batch: Option<PeriodBatchRef>,
}

/// 一条结转拦截。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FinishBlocker {
    /// 分类码（见本文件 `BLOCK_*`）。
    pub code: &'static str,
    /// 一句话说清为什么不能结转（点名数字 / 期间号）。
    pub message: String,
    /// 怎么修（用户能在界面上自救）。
    pub fix: String,
}

/// 结转判定（**纯函数**；空 = 可结转）。
///
/// 顺序有意为之：**先说这批货本身能不能转**（作废 / 已结算 / 还有在制 / 没产出 / 没成本），
/// 再说「数据还差什么」（草稿工时、未归集领料、未过账领料、期间锁定）。
/// 反过来的话，用户会先去补一堆数据，补完才发现这批根本没完工。
pub fn evaluate_finish(facts: &FinishFacts) -> Vec<FinishBlocker> {
    let mut blockers = Vec::new();

    if facts.object_status == cost_object_dto::STATUS_CANCELLED {
        blockers.push(FinishBlocker {
            code: BLOCK_CANCELLED,
            message: format!(
                "批次状态是「{}」：作废批次不结转（它的归集成本已无意义）",
                facts.object_status
            ),
            fix: "换一个在产的批次；确属错作废请重建批次后重新归集".to_string(),
        });
    }

    if facts.settled_items > 0 {
        blockers.push(FinishBlocker {
            code: BLOCK_ALREADY_SETTLED,
            message: format!(
                "该批已有 {} 行计算单是「已结算」（`biz_cost_period_item.status = 'SETTLED'`）：\
                 这批成本已经随期间结算生成完工入库单与结转凭证",
                facts.settled_items
            ),
            fix: "账务已经走过就不再补工作底稿：要改数请先红冲该期间结算，再按新口径重结".to_string(),
        });
    }

    if facts.wip_qty > Decimal::ZERO {
        blockers.push(FinishBlocker {
            code: BLOCK_WIP_NOT_ZERO,
            message: format!(
                "期末在制数量是 {}（不为 0）：分批法按批结转，批没完就一分不转（不做约当产量分摊）",
                quantity_text(facts.wip_qty)
            ),
            fix: "等整批完工后再结转；若这批确实允许带在制出货，请走期间结算（calc 约当产量口径）".to_string(),
        });
    }

    if facts.completed_qty <= Decimal::ZERO {
        blockers.push(FinishBlocker {
            code: BLOCK_NO_FINISHED_QTY,
            message: format!(
                "累计完工数量是 {}：没有产出就没有「完工转出」可结",
                quantity_text(facts.completed_qty)
            ),
            fix: "先在成本对象「产量与在制」里录入本期完工数量，再来结转".to_string(),
        });
    }

    if facts.collected.is_zero() {
        blockers.push(FinishBlocker {
            code: BLOCK_NO_COST,
            message: "该批累计归集金额是 0：零成本完工是数据错误（料 / 工 / 费一条都没进这个筐）".to_string(),
            fix: "先归集料工费（分批法归集接口，或跑期间 calc 归集段）再结转；不要结转 0 元完工".to_string(),
        });
    }

    if facts.draft_labor > 0 {
        blockers.push(FinishBlocker {
            code: BLOCK_DRAFT_LABOR,
            message: format!(
                "还有 {} 条草稿工时台账没确认（`biz_cost_labor.status = 'DRAFT'`）：结转后人工成本还会变",
                facts.draft_labor
            ),
            fix: "先确认（或删除）这些工时行，再结转".to_string(),
        });
    }

    if facts.uncollected_stock_out > 0 {
        blockers.push(FinishBlocker {
            code: BLOCK_UNCOLLECTED_STOCK_OUT,
            message: format!(
                "还有 {} 张领料出库单已过账但没有归集到本批：材料成本还没进筐，现在结转会少转",
                facts.uncollected_stock_out
            ),
            fix: "先对本批做归集（分批法归集接口 / 期间 calc 料段），再结转".to_string(),
        });
    }

    if facts.unposted_stock_out > 0 {
        blockers.push(FinishBlocker {
            code: BLOCK_UNPOSTED_STOCK_OUT,
            message: format!(
                "还有 {} 张挂在本批上的领料出库单未过账（草稿 / 待审）：过账后成本会涨",
                facts.unposted_stock_out
            ),
            fix: "先过账或作废这些领料单，再结转".to_string(),
        });
    }

    if !facts.locked_periods.is_empty() {
        let list = facts
            .locked_periods
            .iter()
            .map(|(period, status)| format!("{period}({status})"))
            .collect::<Vec<_>>()
            .join("、");
        blockers.push(FinishBlocker {
            code: BLOCK_PERIOD_LOCKED,
            message: format!(
                "本批有活动的期间成本批次已锁定：{list}。这些期间的归集基线已被审批 / 结算固定，\
                 再结转会和账上口径打架"
            ),
            fix: "等该期间回到 DRAFT 再结转；已结算的期间请用红冲 + 重结（账务口径以结算为准）".to_string(),
        });
    }

    // 结转期间自身被冻结（与 M5-03 归集同一套闸门：SUBMITTED 之后不该再动成本数）
    if let Some(period) = &facts.period_batch
        && period.is_frozen()
    {
        blockers.push(FinishBlocker {
            code: BLOCK_PERIOD_FROZEN,
            message: format!(
                "结转期间的成本批次 {} 当前是「{}」：本期间成本数已冻结，不能再写结转",
                period.batch_no, period.status
            ),
            fix: "撤回该期间批次（回到 DRAFT）后再结转；已结算的请走红冲".to_string(),
        });
    }

    blockers
}

// ==================== 结转计划（纯函数） ====================

/// 一次结转的计算结果。
#[derive(Debug, Clone, PartialEq)]
pub struct TransferPlan {
    /// 本次结转金额（三要素）。
    pub transfer: Amounts,
    /// 完工数量（结转口径 = 批次累计完工数）。
    pub finished_qty: Decimal,
    /// 本次单位成本 = 本次结转合计 / 完工数量（round 6）。
    pub unit_cost: Decimal,
    /// 结转后累计单位成本 = (已结转 + 本次) / 完工数量（round 6）。
    pub cumulative_unit_cost: Decimal,
    /// 本次是否有钱可转（差为 0 ⇒ 幂等空转）。
    pub transferable: bool,
    /// 本次结转的序号（历史次数 + 1）。
    pub transfer_no: i32,
}

/// 结转计划（**纯函数**）：`本次 = 累计归集 − 已结转`，逐要素夹零。
///
/// `history_times` 是这块批次已有几张结转单，用于给本次编号（同批第 n 次结转）。
pub fn plan_transfer(
    collected: &Amounts,
    transferred: &Amounts,
    finished_qty: Decimal,
    history_times: i32,
) -> TransferPlan {
    let transfer = collected.saturating_sub(transferred);
    let cumulative = Amounts {
        material: transferred.material + transfer.material,
        labor: transferred.labor + transfer.labor,
        overhead: transferred.overhead + transfer.overhead,
    };
    TransferPlan {
        transferable: !transfer.is_zero(),
        unit_cost: unit_cost_of(transfer.total(), finished_qty),
        cumulative_unit_cost: unit_cost_of(cumulative.total(), finished_qty),
        transfer,
        finished_qty,
        transfer_no: history_times.max(0) + 1,
    }
}

/// 单位成本（`amount / qty`，round 6；`qty <= 0` 一律 0，不造 `NaN` 语义）。
pub fn unit_cost_of(amount: Decimal, qty: Decimal) -> Decimal {
    if qty <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    (amount / qty).round_dp(UNIT_COST_SCALE)
}

// ==================== 出参 ====================

/// 一条历史结转单。
#[derive(Debug, Clone, Serialize)]
pub struct FinishHistoryRes {
    /// 结转单 id。
    pub id: String,
    /// 第几次结转（同批从 1 开始）。
    pub transfer_seq: i32,
    /// 结转期间。
    pub period_code: String,
    /// 结转时点的完工数量。
    pub finished_qty: String,
    /// 本次单位成本。
    pub unit_cost: String,
    /// 本次结转金额（三要素）。
    pub amounts: AmountsRes,
    /// 结转时间（RFC3339）。
    pub created_at: String,
    /// 操作人（`display_name` 快照不存，实时 join 用户名）。
    pub created_by_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 结转试算出参（GET）。
#[derive(Debug, Clone, Serialize)]
pub struct BatchFinishPreviewRes {
    /// 批次抬头（与 M5-02 归集表同一形状，前端一套渲染）。
    pub batch: SheetBatchRes,
    /// 结转期间。
    pub period_code: String,
    /// 结转期间的成本批次（无 = 该期还没建批次；那时不拦结转 —— 工作底稿不对账期间）。
    pub period_batch: Option<collect_dto::PeriodBatchRes>,
    /// 累计归集（全期间）。
    pub collected: AmountsRes,
    /// 已结转（历史结转单之和）。
    pub transferred: AmountsRes,
    /// 本次将结转。
    pub pending: AmountsRes,
    /// 完工数量。
    pub finished_qty: String,
    /// 本次单位成本。
    pub unit_cost: String,
    /// 结转后累计单位成本。
    pub cumulative_unit_cost: String,
    /// 本次是否能转（`false` = 被拦或没有差额，看 `blockers` / `message`）。
    pub transferable: bool,
    /// 本次序号（第几次结转）。
    pub transfer_no: i32,
    /// 该批是否已随期间结算结转（账已走）。
    pub settled_in_books: bool,
    /// 历史结转单（新 → 旧）。
    pub history: Vec<FinishHistoryRes>,
    /// 拦截清单（空 = 可结转）。
    pub blockers: Vec<FinishBlocker>,
    /// 提醒（不拦，但用户要知道）：口径边界 / 后续动作。
    pub warnings: Vec<String>,
}

/// 执行结转出参（POST）。
#[derive(Debug, Clone, Serialize)]
pub struct BatchFinishRes {
    /// 本次是否真的写了结转单（`false` = 幂等空转：没有差额 / 重放同一请求）。
    pub inserted: bool,
    /// 一句话结论（幂等空转时说明原因）。
    pub message: String,
    /// 本次写入的结转单 id（幂等空转时为 `None`）。
    pub finish_id: Option<String>,
    /// 本次结转金额（幂等空转时全 0）。
    pub transfer: AmountsRes,
    /// 结转后仍未结转的差额（正常应为 0；不为 0 说明期间内又进来了新成本）。
    pub remaining: AmountsRes,
    /// 结转后的最新口径（回查，**不回显请求里的数字**）。
    pub preview: BatchFinishPreviewRes,
}

// ==================== 拦截 → HTTP ====================

/// 一组拦截 → `409 + 66107`（错误体 `data.blockers` 带结构化清单，前端可逐条展示）。
pub fn not_transferable(object_no: &str, blockers: &[FinishBlocker]) -> ApiError {
    let detail = blockers
        .iter()
        .map(|item| format!("[{}] {} ⇒ {}", item.code, item.message, item.fix))
        .collect::<Vec<_>>()
        .join("；");
    let payload = serde_json::json!({ "blockers": blockers });
    ApiError::new(409, BLOCKED_CODE, format!("批次 {object_no} 不能完工结转：{detail}")).with_data(payload)
}

// ==================== 小工具 ====================

/// 金额文本（**统一补到 4 位**，与库列 `NUMERIC(18,4)` 的标度一致）。
///
/// 为什么不直接用 `Decimal::to_string()`（归集表那边就是这么做的）：同一份出参里，
/// 来自库的值带 4 位标度（`15000.0000`）、计算出来的差额可能只有 0 位（`0`）——
/// 前端比较 / 展示要面对两种形状。本接口的出参**全部**走这里，形状恒定。
pub fn amount_text(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(AMOUNT_SCALE))
}

/// 数量文本（同样补到 4 位：`800` → `"800.0000"`）。
pub fn quantity_text(value: Decimal) -> String {
    amount_text(value)
}

/// 单位成本文本（补到 6 位，与 `NUMERIC(18,6)` 及设计 §4.6 ⑤ 的 round-6 口径一致）。
pub fn unit_cost_text(value: Decimal) -> String {
    format!("{:.6}", value.round_dp(UNIT_COST_SCALE))
}

/// 期间必填校验（复用 M5-03 的同一份实现：缺失 / 形状非法 / 月份越界一律 400）。
pub fn period_of(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    collect_dto::period_of(raw)
}

/// 备注清洗（去首尾空白；空串 → `None`；长度上限 200）。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > 200 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "备注最多 200 个字符".to_string(),
        ));
    }
    Ok(Some(text.to_string()))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        use std::str::FromStr;
        Decimal::from_str(text).expect("金额字面量")
    }

    fn facts() -> FinishFacts {
        FinishFacts {
            object_status: cost_object_dto::STATUS_OPEN.to_string(),
            wip_qty: Decimal::ZERO,
            completed_qty: dec("800"),
            collected: Amounts {
                material: dec("15000"),
                labor: dec("4000"),
                overhead: dec("4000"),
            },
            transferred: Amounts::default(),
            settled_items: 0,
            draft_labor: 0,
            uncollected_stock_out: 0,
            unposted_stock_out: 0,
            locked_periods: Vec::new(),
            period_batch: None,
        }
    }

    #[test]
    fn permission_codes_are_the_two_reused_ones() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(PERM_FINISH, "business:erp:cost-period:calc");
        assert_eq!(PERMISSIONS, [PERM_READ, PERM_FINISH]);
        assert_eq!(BLOCKED_CODE, 66107);
    }

    #[test]
    fn a_finished_batch_with_full_cost_has_no_blocker() {
        assert!(evaluate_finish(&facts()).is_empty(), "整批完工 + 有成本 ⇒ 可结转");
    }

    #[test]
    fn cancelled_batch_is_blocked_first() {
        let mut f = facts();
        f.object_status = cost_object_dto::STATUS_CANCELLED.to_string();
        let blockers = evaluate_finish(&f);
        assert_eq!(blockers[0].code, BLOCK_CANCELLED, "作废要排在第一位");
        assert!(blockers[0].message.contains("CANCELLED"));
        assert!(!blockers[0].fix.is_empty(), "必须给怎么修");
    }

    #[test]
    fn settled_in_books_blocks_the_working_paper() {
        let mut f = facts();
        f.settled_items = 2;
        let blockers = evaluate_finish(&f);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_ALREADY_SETTLED);
        assert!(blockers[0].message.contains('2'), "要点名几条计算单已结算");
    }

    #[test]
    fn wip_blocks_because_job_costing_does_not_prorate() {
        let mut f = facts();
        f.wip_qty = dec("200");
        let blockers = evaluate_finish(&f);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_WIP_NOT_ZERO);
        assert!(blockers[0].message.contains("200"), "要点名在制数量");
        assert!(blockers[0].message.contains("约当"), "要说明分批法不做约当分摊");
    }

    #[test]
    fn zero_output_and_zero_cost_are_both_blocked() {
        let mut f = facts();
        f.completed_qty = Decimal::ZERO;
        f.collected = Amounts::default();
        let codes: Vec<_> = evaluate_finish(&f).iter().map(|b| b.code).collect();
        assert!(codes.contains(&BLOCK_NO_FINISHED_QTY));
        assert!(codes.contains(&BLOCK_NO_COST));
    }

    #[test]
    fn pending_sources_block_the_transfer() {
        let mut f = facts();
        f.draft_labor = 3;
        f.uncollected_stock_out = 1;
        f.unposted_stock_out = 2;
        let codes: Vec<_> = evaluate_finish(&f).iter().map(|b| b.code).collect();
        assert_eq!(
            codes,
            vec![BLOCK_DRAFT_LABOR, BLOCK_UNCOLLECTED_STOCK_OUT, BLOCK_UNPOSTED_STOCK_OUT]
        );
    }

    #[test]
    fn locked_periods_block_and_are_named() {
        let mut f = facts();
        f.locked_periods = vec![("2026-10".to_string(), "SETTLED".to_string())];
        let blockers = evaluate_finish(&f);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].message.contains("2026-10(SETTLED)"));
    }

    #[test]
    fn frozen_transfer_period_blocks() {
        let mut f = facts();
        f.period_batch = Some(PeriodBatchRef {
            batch_no: "CS-202611-0001".to_string(),
            status: "APPROVED".to_string(),
        });
        let blockers = evaluate_finish(&f);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, BLOCK_PERIOD_FROZEN);
        assert!(blockers[0].message.contains("CS-202611-0001"));
    }

    #[test]
    fn draft_transfer_period_does_not_block() {
        let mut f = facts();
        f.period_batch = Some(PeriodBatchRef {
            batch_no: "CS-202611-0001".to_string(),
            status: "DRAFT".to_string(),
        });
        assert!(evaluate_finish(&f).is_empty(), "DRAFT 期间批次是正常在途态");
    }

    #[test]
    fn plan_is_first_time_full_amount() {
        let collected = facts().collected;
        let plan = plan_transfer(&collected, &Amounts::default(), dec("800"), 0);
        assert_eq!(plan.transfer, collected);
        assert!(plan.transferable);
        assert_eq!(plan.transfer_no, 1);
        // 23000 / 800 = 28.75
        assert_eq!(plan.unit_cost, dec("28.75"));
        assert_eq!(plan.cumulative_unit_cost, dec("28.75"));
    }

    #[test]
    fn plan_second_time_is_the_delta_only() {
        let collected = facts().collected;
        let transferred = Amounts {
            material: dec("15000"),
            labor: dec("1000"),
            overhead: Decimal::ZERO,
        };
        let plan = plan_transfer(&collected, &transferred, dec("800"), 1);
        assert_eq!(plan.transfer.material, Decimal::ZERO);
        assert_eq!(plan.transfer.labor, dec("3000"));
        assert_eq!(plan.transfer.overhead, dec("4000"));
        assert_eq!(plan.transfer.total(), dec("7000"));
        assert_eq!(plan.transfer_no, 2);
        assert!(plan.transferable);
    }

    #[test]
    fn plan_is_idempotent_when_nothing_new() {
        let collected = facts().collected;
        let plan = plan_transfer(&collected, &collected, dec("800"), 1);
        assert!(!plan.transferable, "差为 0 不写空单");
        assert_eq!(plan.transfer.total(), Decimal::ZERO);
        assert_eq!(plan.unit_cost, Decimal::ZERO);
    }

    #[test]
    fn plan_clips_negative_delta_instead_of_reversing_cost() {
        let collected = Amounts {
            material: dec("100"),
            labor: Decimal::ZERO,
            overhead: Decimal::ZERO,
        };
        let transferred = Amounts {
            material: dec("300"),
            labor: Decimal::ZERO,
            overhead: Decimal::ZERO,
        };
        let plan = plan_transfer(&collected, &transferred, dec("10"), 1);
        assert_eq!(plan.transfer.material, Decimal::ZERO, "已结转多于归集时夹到 0");
        assert!(!plan.transferable);
    }

    #[test]
    fn unit_cost_rounds_to_six_and_guards_zero_qty() {
        assert_eq!(unit_cost_of(dec("22311.1104"), dec("800")), dec("27.888888"));
        assert_eq!(unit_cost_of(dec("100"), Decimal::ZERO), Decimal::ZERO);
        assert_eq!(unit_cost_of(Decimal::ZERO, dec("10")), Decimal::ZERO);
    }

    #[test]
    fn amounts_res_pads_to_four_decimals_and_totals() {
        let res = AmountsRes::of(&Amounts {
            material: dec("15000"),
            labor: dec("3555.5552"),
            overhead: dec("0"),
        });
        assert_eq!(res.material, "15000.0000", "统一补 4 位：来自库与算出来的形状一致");
        assert_eq!(res.labor, "3555.5552", "4 位标度的值不能被截断");
        assert_eq!(res.overhead, "0.0000");
        assert_eq!(res.total, "18555.5552");
    }

    #[test]
    fn unit_cost_text_pads_to_six_decimals() {
        assert_eq!(unit_cost_text(dec("28.75")), "28.750000");
        assert_eq!(unit_cost_text(dec("27.888888")), "27.888888");
        assert_eq!(unit_cost_text(Decimal::ZERO), "0.000000");
        assert_eq!(quantity_text(dec("800")), "800.0000");
    }

    #[test]
    fn blocked_error_carries_structured_detail() {
        let mut f = facts();
        f.wip_qty = dec("200");
        let error = not_transferable("CB-20261101-0001", &evaluate_finish(&f));
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66107);
        assert!(error.msg.contains("CB-20261101-0001"));
        let data = error.data.expect("错误体要带 data.blockers");
        let blockers = data
            .get("blockers")
            .and_then(|value| value.as_array())
            .expect("blockers 数组");
        assert!(!blockers.is_empty());
        assert!(blockers[0].get("code").is_some());
        assert!(blockers[0].get("fix").is_some());
    }

    #[test]
    fn remark_is_trimmed_and_bounded() {
        assert_eq!(clean_remark(Some("  完工  ")).unwrap(), Some("完工".to_string()));
        assert_eq!(clean_remark(Some("   ")).unwrap(), None);
        assert_eq!(clean_remark(None).unwrap(), None);
        assert!(clean_remark(Some(&"长".repeat(201))).is_err());
    }

    #[test]
    fn period_is_required_and_well_formed() {
        assert!(period_of(None).is_err());
        assert!(period_of(Some("2026-13")).is_err());
        assert_eq!(period_of(Some(" 2026-11 ")).unwrap(), "2026-11");
    }
}
