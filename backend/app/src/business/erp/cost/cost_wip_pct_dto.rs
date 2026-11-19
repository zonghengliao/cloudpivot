//! 在制品「完工率录入（按工序）」· 契约层与纯逻辑（成本核算 · M5-21）。
//!
//! 设计依据：ARCH-9 §4.6（在制与完工：加工约当 = 完工 + 期末在制 × 完工率）+ M5-20
//! （`cost_wip_equiv_dto`）模块头里写死的交接「工序级完工率落地后按加权汇总写回对象级
//! 即可，本表不需要改」。
//!
//! # 这个模块回答什么问题
//!
//! 一张生产批里还压着 40 件在制，车间报「下料完、焊接 60%、装配没开始」——
//! 这 40 件到底折成多少件完工品？答案就一个数：**综合完工率**。
//!
//! ```text
//! 综合完工率 = Σ(工序权重 × 工序完工率) / Σ工序权重      ← 加权平均法
//! 加工约当   = 本期完工 + 期末在制 × 综合完工率           ← 由 M5-20 消费
//! ```
//!
//! 权重缺省 1 ⇒ 综合完工率就是各工序完工率的算术平均；车间填了「工时定额」时
//! 干活多的工序说话更重（例：下料 100%（权 1）/ 焊接 50%（权 2）/ 装配 0%（权 1）
//! ⇒ (100×1 + 50×2 + 0×1) / 4 = 50%）。
//!
//! # 为什么对象级完工率是**派生物**，不是又一个手填框
//!
//! `biz_cost_object.completion_pct` 只有一个数（V0370 列注释：「期末在制完工进度%」），
//! 而车间真正掌握的事实是**逐工序**的进度。本卡把录入面挪到工序级，保存时在**同一个事务**
//! 里把加权汇总写回对象级 —— 于是：
//!
//! * 约当产量表（M5-20）/ 计算单（M5-06 第 6 段）读到的仍是同一个列，**零改动**；
//! * 不存在「静默的两本账」：对象级只有一个由本模块维护的写入口。`PUT /cost-object/{id}/output`
//!   仍然可以改这一列（它是「当前快照」的兜底入口，本卡不改那条路径），但一旦两侧不一致，
//!   本模块的 `GET` 会直接把它标出来 —— `rollup.matched = false` + `diff` + 一条
//!   「谁改过对象级」的提醒，保存一次工序即回写一致（见 [`warnings_of`] 最后一条）。
//!
//! # 为什么不带期间维度（与对象级同一口径）
//!
//! `biz_cost_object.completion_pct` 是**当前快照**（不是「某期的完工率」），M5-20 拿它
//! 与当期计算单对拍、漂移了标 `drift`。工序行若自带 `period_code`，同一批就会同时存在
//! 「11 月的工序行」和「12 月的工序行」，而对象抬头只能存一个数 —— 立刻变成两本账。
//! 所以本表与对象级同生共死：一次保存 = 覆盖当前工序进度 + 回写对象级。
//! 按期间留档的完工率在**计算单**里（`biz_cost_period_item.completion_pct`），那是结算
//! 那一刻的法定快照。
//!
//! # 保存语义：整表覆盖（按 seq 对齐）
//!
//! `PUT /cost-object/{id}/operations` 收的是**整张工序表**：payload 里有几行就是几行。
//!
//! * 同 seq 已存在 ⇒ 更新（值没变则不写库、不写审计）；
//! * payload 里新出现的 seq ⇒ 插入；
//! * 库里存在但 payload 没给的 seq ⇒ **软删**（这是「删掉一道工序」的唯一路径，
//!   没有单独 DELETE 接口：少一行就少一道工序，语义唯一）。
//!
//! 为什么不做「逐行增删改」四件套：工序表的形状是「这些工序 + 各自进度」，逐行接口会
//! 让「删一道工序」和「漏传一行」在服务端无法区分（漏传 = 悄悄删掉），整表覆盖把
//! 这件事交回给用户 —— 界面上看见的就是保存后的样子。
//!
//! # 值域（与库层 CHECK 逐条对应，提前给 400 而不是 500）
//!
//! | 列 | 值域 | 越界 |
//! |---|---|---|
//! | `seq` | 1–999，同一批内唯一 | 400 + 点名重复的号 |
//! | `operation_name` | 去空白后 1–64 字 | 400 |
//! | `weight` | (0, 99999999.9999]，最多 4 位小数 | 400（公式分母不能是 0） |
//! | `completion_pct` | 0–100，最多 2 位小数 | 400 |
//! | `remark` | ≤ 255 字 | 400 |
//! | 行数 | ≤ 200 | 400（再多不是工序，是 BOM 明细） |

use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_object_dto;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（复用成本对象，零新增码） ====================

/// 读工序完工率（**复用** `business:erp:cost-object:list`）。
///
/// 理由与 M5-02 / M5-05 / M5-20 完全一致：每一行都是某个成本对象的一列进度，
/// 能读成本对象的人本来就该看到「这 40 件在制折几件完工」。新开 `cost-wip:*`
/// 会带来两个问题：① 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；
/// ② 先插一条指向不存在组件的菜单行 ⇒ `n08` 判红。
/// 页面码 / 接口码解耦（PERM-01）只在 HR 试点，成本域另开卡。
///
/// 与 `cost_object_handler::PERM_LIST` **必须逐字相同**（单测钉住，防止两处漂移）。
pub const PERM_READ: &str = "business:erp:cost-object:list";

/// 录工序完工率（**复用** `business:erp:cost-object:update`）。
///
/// 这与「填写本期产量与在制」（`PUT /cost-object/{id}/output`）是同一件事的两半：
/// 产量 / 在制数量是「干了多少件」，完工率是「干到哪一步」。
/// 与 `cost_object_handler::PERM_UPDATE` **必须逐字相同**（单测钉住）。
pub const PERM_WRITE: &str = "business:erp:cost-object:update";

/// 本模块全部权限码。
pub const PERMISSIONS: [&str; 2] = [PERM_READ, PERM_WRITE];

/// 变更日志的动作标签（`sys_change_log.action_label`）。
///
/// 与「填写本期产量与在制」（`cost_object_dto::OUTPUT_ACTION_LABEL`）分开：
/// 审计页里要能一眼分出「改的是数量」还是「改的是工序进度」。
pub const ACTION_LABEL: &str = "录入工序完工率";

// ==================== 口径文案（前端页脚直接用，别在前端手抄） ====================

/// 综合完工率的公式说明。
pub const FORMULA: &str = "综合完工率 = Σ(工序权重 × 工序完工率) ÷ Σ工序权重（权重缺省 1 = 算术平均）";

/// 权重语义说明（前端列头 tooltip）。
pub const FORMULA_WEIGHT: &str =
    "权重一般填标准工时 / 定额工时：干活多的工序对综合完工率影响更大；全填 1 时就是各工序完工率的算术平均";

// ==================== 值域 / 标度 ====================

/// 完工率标度（`biz_cost_object.completion_pct` 与 `biz_cost_object_operation.completion_pct`
/// 都是 `NUMERIC(5,2)`）。
pub const PCT_SCALE: u32 = cost_object_dto::PCT_SCALE;

/// 完工率上限。
pub const PCT_CEILING: i32 = 100;

/// 权重标度（`NUMERIC(12,4)`）。
pub const WEIGHT_SCALE: u32 = 4;

/// 权重上限（`NUMERIC(12,4)` 的整数位上限）。
pub const WEIGHT_CEILING: &str = "99999999.9999";

/// 工序序号上限（库 CHECK 同值）。
pub const MAX_SEQ: i32 = 999;

/// 一次保存最多多少道工序（防御：再多不是工序，是 BOM 明细）。
pub const MAX_ROWS: usize = 200;

/// 工序名最大字符数（`VARCHAR(64)`）。
pub const NAME_MAX_CHARS: usize = 64;

/// 备注最大字符数（`VARCHAR(255)`）。
pub const REMARK_MAX_CHARS: usize = 255;

/// 取舍规则：四舍五入（与成本域其它「按列标度收口」的地方一致）。
pub const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 入参 ====================

/// 整表保存工序完工率（payload 有几行就是几行，缺的 seq 会被软删）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OperationSaveReq {
    /// 工序行（**按 `seq` 对齐**，顺序无关）。
    pub rows: Vec<OperationRowReq>,
}

/// 一道工序的入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OperationRowReq {
    /// 工序序号（1–999；同一批内唯一，必填）。
    pub seq: Option<i32>,
    /// 工序名（去空白后 1–64 字，必填）。
    pub operation_name: Option<String>,
    /// 工作量权重（缺省 1；必须 > 0）。
    pub weight: Option<Decimal>,
    /// 本工序完工率 0–100（必填；整表保存下缺失按 0 处理会让用户「一保存就归零」，
    /// 所以要求显式给值）。
    pub completion_pct: Option<Decimal>,
    /// 备注（≤ 255 字）。
    pub remark: Option<String>,
}

// ==================== 归一化规格 ====================

/// 归一化后的一道工序（落库 / 单测 / 汇总都拿它）。
#[derive(Debug, Clone, PartialEq)]
pub struct OperationSpec {
    /// 工序序号。
    pub seq: i32,
    /// 工序名（已去首尾空白）。
    pub operation_name: String,
    /// 工作量权重（> 0）。
    pub weight: Decimal,
    /// 本工序完工率（0–100，≤ 2 位小数）。
    pub completion_pct: Decimal,
    /// 备注（空白 → `None`）。
    pub remark: Option<String>,
}

/// 入库行（含主键 / 时间，回查用）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOperation {
    /// 主键。
    pub id: Uuid,
    /// 归一化内容。
    pub spec: OperationSpec,
    /// 最后修改时间（RFC3339，出参直接用）。
    pub updated_at: String,
}

// ==================== 归一化 ====================

/// 入参 → 归一化规格（**纯逻辑**：值域、去重、排序；失败给 400 + 指名）。
///
/// 排序按 `seq` 升序：出参顺序与前台表格顺序必须由服务端定死，不依赖 payload 顺序
/// （前端拖行 / 乱序提交时，回查结果才不会跳）。
pub fn specs_of(req: &OperationSaveReq) -> std::result::Result<Vec<OperationSpec>, ApiError> {
    if req.rows.len() > MAX_ROWS {
        return Err(bad_request(format!(
            "一次最多保存 {MAX_ROWS} 道工序（收到 {} 行）：再多就不是工序，是 BOM 明细了",
            req.rows.len()
        )));
    }
    let mut specs: Vec<OperationSpec> = Vec::with_capacity(req.rows.len());
    for (index, row) in req.rows.iter().enumerate() {
        let at = index + 1;
        let seq = row
            .seq
            .ok_or_else(|| bad_request(format!("第 {at} 行缺少 seq（工序序号 1–{MAX_SEQ}）")))?;
        if !(1..=MAX_SEQ).contains(&seq) {
            return Err(bad_request(format!(
                "第 {at} 行的 seq（工序序号）必须在 1–{MAX_SEQ} 之间（收到 {seq}）"
            )));
        }
        let name = cost_labor_dto::clean_optional(row.operation_name.clone())
            .ok_or_else(|| bad_request(format!("第 {at} 行（seq {seq}）的 operation_name（工序名）不能为空")))?;
        let name_len = name.chars().count();
        if name_len > NAME_MAX_CHARS {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 operation_name 最多 {NAME_MAX_CHARS} 字（收到 {name_len} 字）"
            )));
        }
        let weight = row.weight.unwrap_or(Decimal::ONE);
        if weight <= Decimal::ZERO {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 weight（权重）必须 > 0：综合完工率的分母是权重合计，\
                 权重 0 会让公式无定义（收到 {weight}）"
            )));
        }
        if weight.scale() > WEIGHT_SCALE {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 weight 最多 {WEIGHT_SCALE} 位小数（列是 NUMERIC(12,4)，收到 {weight}）"
            )));
        }
        let ceiling = weight_ceiling();
        if weight > ceiling {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 weight 不能超过 {WEIGHT_CEILING}（列是 NUMERIC(12,4)，收到 {weight}）"
            )));
        }
        let pct = row.completion_pct.ok_or_else(|| {
            bad_request(format!(
                "第 {at} 行（seq {seq}）缺少 completion_pct（工序完工率 0–{PCT_CEILING}）：\
                 整表保存下不会把缺失当成 0，否则「少传一列」= 悄悄把进度清零"
            ))
        })?;
        if pct < Decimal::ZERO || pct > Decimal::from(PCT_CEILING) {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 completion_pct 必须在 0–{PCT_CEILING} 之间（收到 {pct}）"
            )));
        }
        if pct.scale() > PCT_SCALE {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 completion_pct 最多 {PCT_SCALE} 位小数（列是 NUMERIC(5,2)，收到 {pct}）"
            )));
        }
        let remark = cost_labor_dto::clean_optional(row.remark.clone());
        if let Some(text) = remark.as_deref()
            && text.chars().count() > REMARK_MAX_CHARS
        {
            return Err(bad_request(format!(
                "第 {at} 行（seq {seq}）的 remark 最多 {REMARK_MAX_CHARS} 字（收到 {} 字）",
                text.chars().count()
            )));
        }
        specs.push(OperationSpec {
            seq,
            operation_name: name,
            weight,
            completion_pct: pct,
            remark,
        });
    }

    specs.sort_by_key(|spec| spec.seq);
    if let Some(pair) = specs.windows(2).find(|pair| pair[0].seq == pair[1].seq) {
        return Err(bad_request(format!(
            "工序序号 {} 重复出现：同一批里一个序号只能有一道工序（库层有部分唯一索引，先在这里拦下）",
            pair[0].seq
        )));
    }
    Ok(specs)
}

/// 权重上限（`WEIGHT_CEILING` 的 `Decimal` 形态；解析失败说明常量被写坏，直接 panic 更响）。
fn weight_ceiling() -> Decimal {
    use std::str::FromStr;
    Decimal::from_str(WEIGHT_CEILING).expect("权重上限字面量必须是合法十进制")
}

// ==================== 综合完工率（加权平均） ====================

/// 加权汇总结果。
///
/// `pct` 是**写回对象级**的值（2 位小数）；`pct_unrounded` 是未收口的商，
/// 只用于出参核对（「50.005 收成 50.01」这种差异要能解释清楚）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rollup {
    /// Σ权重。
    pub weight_sum: Decimal,
    /// Σ(权重 × 工序完工率)。
    pub weighted_sum: Decimal,
    /// 未收口的商（`weighted_sum / weight_sum`，6 位小数）。
    pub pct_unrounded: Decimal,
    /// 写回对象级的综合完工率（0–100，2 位小数）。
    pub pct: Decimal,
}

/// 综合完工率（**纯函数**）：空表 → `None`（没有工序就没有「加权」这回事）。
///
/// 数学上 `pct` 一定在 0–100（每道工序都在值域内、权重都 > 0），这里仍显式夹到
/// `[0, 100]`：库列是 `NUMERIC(5,2)`，多一分都会写不进去，宁可夹也不让保存 500。
pub fn rollup_of(specs: &[OperationSpec]) -> Option<Rollup> {
    if specs.is_empty() {
        return None;
    }
    let mut weight_sum = Decimal::ZERO;
    let mut weighted_sum = Decimal::ZERO;
    for spec in specs {
        weight_sum += spec.weight;
        weighted_sum += spec.weight * spec.completion_pct;
    }
    if weight_sum <= Decimal::ZERO {
        // 归一化已保证每行 weight > 0，走到这里说明有人绕过了 `specs_of`：不静默给 0
        return None;
    }
    let quotient = (weighted_sum / weight_sum).round_dp_with_strategy(6, ROUNDING);
    let mut pct = quotient.round_dp_with_strategy(PCT_SCALE, ROUNDING);
    if pct < Decimal::ZERO {
        pct = Decimal::ZERO;
    }
    if pct > Decimal::from(PCT_CEILING) {
        pct = Decimal::from(PCT_CEILING);
    }
    Some(Rollup {
        weight_sum,
        weighted_sum,
        pct_unrounded: quotient,
        pct,
    })
}

/// 单道工序对综合完工率的贡献（`权重 × 完工率 / Σ权重`，4 位小数）。
///
/// 出参里逐行列出来，是为了让「为什么汇总成 50%」在界面上可核：
/// 每行贡献相加 = 综合完工率（尾差最多 `0.0001 × 行数`，收口到 2 位后一致）。
pub fn contribution_of(spec: &OperationSpec, rollup: &Rollup) -> Decimal {
    if rollup.weight_sum <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    (spec.weight * spec.completion_pct / rollup.weight_sum).round_dp_with_strategy(4, ROUNDING)
}

/// 单道工序的权重占比（%，4 位小数）。
pub fn share_of(spec: &OperationSpec, rollup: &Rollup) -> Decimal {
    if rollup.weight_sum <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    (spec.weight / rollup.weight_sum * Decimal::from(100)).round_dp_with_strategy(4, ROUNDING)
}

// ==================== 可编辑性（状态闸） ====================

/// 录入闸：对象当前能不能改工序进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditGate {
    /// 可改。
    Editable,
    /// 不可改（带原因）。
    Locked(String),
}

impl EditGate {
    /// 是否可写。
    pub fn editable(&self) -> bool {
        matches!(self, Self::Editable)
    }

    /// 出参 `blocked_reason`。
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Editable => None,
            Self::Locked(reason) => Some(reason.clone()),
        }
    }

    /// 出参 `state`（前端判按钮禁用 / 锁图标）。
    pub fn state(&self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Locked(_) => "locked",
        }
    }
}

/// 判定录入闸（**纯函数**）：与 `PUT /cost-object/{id}/output` 同一套口径 ——
///
/// 1. 非 `OPEN`（已关批 / 已作废）⇒ 锁：批的完工率已经随结转变成入库金额；
/// 2. 该对象参与过的期间已结算（`SETTLED` / `CLOSED`）⇒ 锁：完工率已经进了计算单，
///    再改就是改写已入账成本（要改先红冲该期间批次）。
///
/// 两件事**都不报 403**：这是业务状态不允许，不是「你没权限」。handler 里转 409。
pub fn gate_of(status: &str, settled: &[(String, String)]) -> EditGate {
    if status != cost_object_dto::STATUS_OPEN {
        let label = match status {
            cost_object_dto::STATUS_CLOSED => "已关批",
            cost_object_dto::STATUS_CANCELLED => "已作废",
            other => other,
        };
        return EditGate::Locked(format!(
            "成本对象当前是「{label}」：只有「在产」的批能录工序完工率（已关批的成本已结转，要复工请新建批）"
        ));
    }
    if !settled.is_empty() {
        let list = settled
            .iter()
            .map(|(period, status)| format!("{period}({status})"))
            .collect::<Vec<_>>()
            .join("、");
        return EditGate::Locked(format!(
            "该批参与的成本批次已结算（{list}）：完工率已经进了计算单与约当产量，\
             再改就是改写已入账成本。如需更正，请先红冲该期间的成本批次，再回来改"
        ));
    }
    EditGate::Editable
}

// ==================== 提醒（只提示不拦） ====================

/// 提醒（**纯函数**；每条都是「用户该知道但不该被拦」的事）。
///
/// | 条件 | 为什么提示 |
/// |---|---|
/// | 期末在制 > 0 且加权完工率 = 0 | 跑计算会被 `66067` 拦下（在制 > 0 但完工进度 0） |
/// | 期末在制 = 0 却有工序进度 | 本期没有在制，完工率不参与约当（材料与加工都等于完工数） |
/// | 只有一行且权重任意 | 单工序时综合完工率 = 该工序完工率，权重不起作用 |
/// | 权重全相同 | 综合完工率就是各工序完工率的算术平均（用户可能想填工时定额） |
/// | 对象级 ≠ 工序汇总 | 有人用 `PUT /output` 直接改过对象级（或历史遗留），保存一次即回写一致 |
pub fn warnings_of(
    wip_qty: Decimal,
    object_pct: Decimal,
    specs: &[OperationSpec],
    rollup: Option<&Rollup>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(rollup) = rollup else {
        return warnings;
    };
    if wip_qty > Decimal::ZERO && rollup.pct == Decimal::ZERO {
        warnings.push(format!(
            "期末在制 {wip_qty} 但工序加权完工率是 0%：跑期间计算会被 66067 拦下\
             （「期末在制 > 0 但完工进度为 0」），请把已开工的工序进度填上"
        ));
    }
    if wip_qty == Decimal::ZERO {
        warnings.push(
            "当前期末在制是 0：本期完工率不参与约当产量（材料与加工约当都等于本期完工数）。\
             跨月还有在制时，记得回来更新工序进度"
                .to_string(),
        );
    }
    if specs.len() == 1 {
        warnings.push("只有一道工序：综合完工率就等于该工序的完工率，权重不起作用".to_string());
    } else if specs.len() > 1 && specs.iter().all(|spec| spec.weight == specs[0].weight) {
        warnings.push(
            "所有工序权重相同：综合完工率就是各工序完工率的算术平均。\
             想让干活多的工序说话更重，请把 weight 填成工时定额"
                .to_string(),
        );
    }
    if object_pct != rollup.pct {
        warnings.push(format!(
            "对象级完工率 {object_pct}% 与工序加权汇总 {}% 不一致：有人直接改过对象级\
             （PUT /cost-object/{{id}}/output）。保存本表会把对象级回写成汇总值",
            rollup.pct
        ));
    }
    warnings
}

// ==================== 出参 ====================

/// 成本对象抬头（录入页顶部那一块）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationObjectView {
    /// 主键。
    pub id: String,
    /// 对象号（生产批号 `CB-yyyyMMdd-NNNNNN`）。
    pub object_no: String,
    /// 对象类型（一期恒 `BATCH`）。
    pub object_type: String,
    /// 产出品编码（join 主数据实时取名）。
    pub product_code: Option<String>,
    /// 产出品名称（join 主数据实时取名）。
    pub product_name: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 生产车间（join 现名，落空回落建单快照）。
    pub workshop_dept_name: Option<String>,
    /// 状态（`OPEN` / `CLOSED` / `CANCELLED`）。
    pub status: String,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 对象级完工率（= 本表汇总回写的那个数）。
    pub completion_pct: String,
    /// 投料方式（`ONE_TIME` / `AS_PROGRESS`）。
    pub material_input_mode: String,
    /// 能不能改（状态闸）。
    pub editable: bool,
    /// 不能改的原因（可改时为 `null`）。
    pub blocked_reason: Option<String>,
}

/// 一道工序的出参。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationRowView {
    /// 行主键。
    pub id: String,
    /// 工序序号。
    pub seq: i32,
    /// 工序名。
    pub operation_name: String,
    /// 权重。
    pub weight: String,
    /// 本工序完工率。
    pub completion_pct: String,
    /// 权重占比%（4 位小数）。
    pub share_pct: String,
    /// 对综合完工率的贡献（百分点，4 位小数）。
    pub contribution_pct: String,
    /// 备注。
    pub remark: Option<String>,
    /// 最后修改时间（RFC3339）。
    pub updated_at: String,
}

/// 加权汇总出参。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupView {
    /// 工序行数。
    pub row_count: usize,
    /// Σ权重。
    pub weight_sum: String,
    /// Σ(权重 × 完工率)。
    pub weighted_sum: String,
    /// 未收口的商（6 位小数；用来解释尾差）。
    pub pct_unrounded: String,
    /// 写回对象级的综合完工率（2 位小数）。
    pub pct: String,
    /// 对象级当前值。
    pub object_pct: String,
    /// 两者是否一致（不一致说明对象级被人直接改过）。
    pub matched: bool,
    /// 差额（对象级 − 汇总，带符号；4 位小数）。
    pub diff: String,
}

/// 工序完工率录入 / 查询的完整出参。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRes {
    /// 成本对象抬头。
    pub object: OperationObjectView,
    /// 工序行（按 seq 升序）。
    pub rows: Vec<OperationRowView>,
    /// 加权汇总（没有工序时 `null`）。
    pub rollup: Option<RollupView>,
    /// 本次保存是否有任何写入（GET 恒 `false`）。
    pub changed: bool,
    /// 本次插入的行数。
    pub inserted: usize,
    /// 本次更新的行数。
    pub updated: usize,
    /// 本次软删的行数。
    pub deleted: usize,
    /// `editable` / `locked`（前端判按钮）。
    pub state: String,
    /// 公式说明。
    pub formula: String,
    /// 权重说明。
    pub weight_hint: String,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
    /// 页脚说明（一句话讲清这张表现在是什么状态）。
    pub note: String,
}

impl OperationRes {
    /// 空表时的页脚（没录过工序）。
    pub fn empty_note(object_no: &str) -> String {
        format!(
            "成本对象 {object_no} 还没有工序：对象级完工率沿用最近一次「填写本期产量与在制」\
             手填的值。录第一道工序保存后，对象级完工率改由本表加权汇总自动回写"
        )
    }

    /// 有工序时的页脚。
    pub fn filled_note(object_no: &str, row_count: usize, pct: &str) -> String {
        format!(
            "成本对象 {object_no} 共 {row_count} 道工序，加权综合完工率 {pct}%（已回写对象级）。\
             约当产量表（M5-20）与期间计算读到的就是这个数"
        )
    }
}

// ==================== 小工具 ====================

/// 十进制 → 出参字符串（去掉无意义的尾零，但保留至少整数位）。
///
/// 与成本域其它出参一致：前端只做展示与比较，不做二次运算（公式在后端）。
pub fn text_of(value: Decimal) -> String {
    value.normalize().to_string()
}

/// 固定标度的字符串（尾差解释用）。
pub fn fixed_of(value: Decimal, scale: u32) -> String {
    format!("{:.1$}", value, scale as usize)
}

/// 400：入参非法。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：业务状态不允许（沿用成本对象的口径）。
pub fn conflict(message: impl Into<String>) -> ApiError {
    cost_object_dto::conflict(message)
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("十进制字面量")
    }

    fn spec(seq: i32, weight: &str, pct: &str) -> OperationSpec {
        OperationSpec {
            seq,
            operation_name: format!("工序{seq}"),
            weight: dec(weight),
            completion_pct: dec(pct),
            remark: None,
        }
    }

    fn req(rows: Vec<OperationRowReq>) -> OperationSaveReq {
        OperationSaveReq { rows }
    }

    fn row(seq: i32, name: &str, weight: Option<&str>, pct: &str) -> OperationRowReq {
        OperationRowReq {
            seq: Some(seq),
            operation_name: Some(name.to_string()),
            weight: weight.map(dec),
            completion_pct: Some(dec(pct)),
            remark: None,
        }
    }

    // ---------- 归一化 ----------

    #[test]
    fn defaults_weight_to_one_and_sorts_by_seq() {
        let specs = specs_of(&req(vec![
            row(3, "装配", None, "0"),
            row(1, "下料", None, "100"),
            row(2, "焊接", None, "50"),
        ]))
        .expect("合法入参");
        assert_eq!(specs.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(specs[0].weight, Decimal::ONE, "缺省权重 = 1（算术平均）");
    }

    #[test]
    fn duplicate_seq_is_rejected_with_the_offending_number() {
        let error = specs_of(&req(vec![row(2, "焊接", None, "10"), row(2, "装配", None, "20")]))
            .expect_err("重复 seq 必须被拦");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains('2'), "报文要点名重复的序号：{}", error.msg);
    }

    #[test]
    fn seq_out_of_range_is_rejected() {
        for seq in [0, -1, MAX_SEQ + 1] {
            let error = specs_of(&req(vec![row(seq, "下料", None, "10")])).expect_err("越界 seq");
            assert_eq!(error.status, 400);
            assert!(error.msg.contains("seq"), "{}", error.msg);
        }
    }

    #[test]
    fn blank_name_is_rejected() {
        let error = specs_of(&req(vec![row(1, "   ", None, "10")])).expect_err("空工序名");
        assert!(error.msg.contains("operation_name"), "{}", error.msg);
    }

    #[test]
    fn name_over_64_chars_is_rejected() {
        let long = "工".repeat(NAME_MAX_CHARS + 1);
        let error = specs_of(&req(vec![row(1, &long, None, "10")])).expect_err("超长工序名");
        assert!(error.msg.contains("64"), "{}", error.msg);
    }

    #[test]
    fn zero_or_negative_weight_is_rejected() {
        for weight in ["0", "-1"] {
            let error = specs_of(&req(vec![row(1, "下料", Some(weight), "10")])).expect_err("权重必须 > 0");
            assert!(error.msg.contains("weight"), "{}", error.msg);
        }
    }

    #[test]
    fn weight_scale_and_ceiling_are_enforced() {
        let scale = specs_of(&req(vec![row(1, "下料", Some("1.23456"), "10")])).expect_err("5 位小数");
        assert!(scale.msg.contains("4 位小数"), "{}", scale.msg);
        let ceiling = specs_of(&req(vec![row(1, "下料", Some("100000000"), "10")])).expect_err("超上限");
        assert!(ceiling.msg.contains(WEIGHT_CEILING), "{}", ceiling.msg);
    }

    #[test]
    fn completion_pct_is_required_not_defaulted_to_zero() {
        let mut input = row(1, "下料", None, "10");
        input.completion_pct = None;
        let error = specs_of(&req(vec![input])).expect_err("完工率必填");
        assert!(error.msg.contains("completion_pct"), "{}", error.msg);
    }

    #[test]
    fn completion_pct_range_and_scale_are_enforced() {
        let high = specs_of(&req(vec![row(1, "下料", None, "100.01")])).expect_err("超 100");
        assert!(high.msg.contains("0–100"), "{}", high.msg);
        let negative = specs_of(&req(vec![row(1, "下料", None, "-0.01")])).expect_err("负数");
        assert!(negative.msg.contains("0–100"), "{}", negative.msg);
        let fine = specs_of(&req(vec![row(1, "下料", None, "33.333")])).expect_err("3 位小数");
        assert!(fine.msg.contains("2 位小数"), "{}", fine.msg);
    }

    #[test]
    fn remark_blank_becomes_none_and_long_remark_is_rejected() {
        let mut input = row(1, "下料", None, "10");
        input.remark = Some("   ".to_string());
        let specs = specs_of(&req(vec![input])).expect("合法");
        assert_eq!(specs[0].remark, None, "空白备注应当是 null，不是空串");

        let mut long = row(1, "下料", None, "10");
        long.remark = Some("备".repeat(REMARK_MAX_CHARS + 1));
        let error = specs_of(&req(vec![long])).expect_err("超长备注");
        assert!(error.msg.contains("255"), "{}", error.msg);
    }

    #[test]
    fn too_many_rows_is_rejected() {
        let rows = (1..=(MAX_ROWS as i32 + 1))
            .map(|seq| row(seq, "工序", None, "10"))
            .collect::<Vec<_>>();
        let error = specs_of(&req(rows)).expect_err("超行数");
        assert!(error.msg.contains(&MAX_ROWS.to_string()), "{}", error.msg);
    }

    #[test]
    fn empty_payload_is_legal_and_means_clear() {
        let specs = specs_of(&req(Vec::new())).expect("清空工序是合法操作");
        assert!(specs.is_empty());
        assert_eq!(rollup_of(&specs), None);
    }

    // ---------- 加权汇总 ----------

    #[test]
    fn equal_weights_give_arithmetic_mean() {
        let specs = vec![spec(1, "1", "100"), spec(2, "1", "50"), spec(3, "1", "0")];
        let rollup = rollup_of(&specs).expect("有工序就有汇总");
        assert_eq!(rollup.pct, dec("50"), "(100+50+0)/3 = 50");
        assert_eq!(rollup.weight_sum, dec("3"));
        assert_eq!(rollup.weighted_sum, dec("150"));
    }

    #[test]
    fn weights_shift_the_result_towards_the_heavy_operation() {
        let specs = vec![spec(1, "1", "100"), spec(2, "2", "50"), spec(3, "1", "0")];
        let rollup = rollup_of(&specs).expect("有工序就有汇总");
        assert_eq!(rollup.pct, dec("50"), "(100×1 + 50×2 + 0×1) / 4 = 50");
        assert_eq!(rollup.weight_sum, dec("4"));
    }

    #[test]
    fn rounding_is_half_away_from_zero_to_two_decimals() {
        // (100 + 100 + 1) / 3 = 67.0 整；换一组：(1 + 1 + 1 + 1 + 1 + 1 + 1) / 7 之类
        let specs = vec![spec(1, "1", "33.33"), spec(2, "1", "33.34"), spec(3, "1", "33.335")];
        // 33.335 超过 2 位小数，走 specs_of 会被拒；这里直接验证 rollup 的收口
        let rollup = rollup_of(&specs).expect("有工序就有汇总");
        assert_eq!(rollup.pct_unrounded, dec("33.335").round_dp_with_strategy(6, ROUNDING));
        assert_eq!(rollup.pct, dec("33.34"), "33.335 → 33.34（四舍五入）");

        let thirds = vec![spec(1, "1", "100"), spec(2, "1", "100"), spec(3, "1", "1")];
        let rollup = rollup_of(&thirds).expect("有工序就有汇总");
        assert_eq!(rollup.pct, dec("67"), "201/3 = 67 整");
    }

    #[test]
    fn single_operation_is_its_own_pct_whatever_the_weight() {
        for weight in ["1", "0.0001", "99999"] {
            let rollup = rollup_of(&[spec(1, weight, "42.5")]).expect("有工序就有汇总");
            assert_eq!(rollup.pct, dec("42.5"), "单工序权重不影响结果（{weight}）");
        }
    }

    #[test]
    fn all_finished_is_exactly_one_hundred() {
        let rollup =
            rollup_of(&[spec(1, "3", "100"), spec(2, "0.5", "100"), spec(3, "7", "100")]).expect("有工序就有汇总");
        assert_eq!(rollup.pct, dec("100"));
    }

    #[test]
    fn contribution_and_share_add_up() {
        let specs = vec![spec(1, "1", "100"), spec(2, "2", "50"), spec(3, "1", "0")];
        let rollup = rollup_of(&specs).expect("有工序就有汇总");
        let total: Decimal = specs.iter().map(|spec| contribution_of(spec, &rollup)).sum();
        assert_eq!(total, dec("50"), "逐行贡献之和 = 综合完工率（4 位小数内）");
        let shares: Decimal = specs.iter().map(|spec| share_of(spec, &rollup)).sum();
        assert_eq!(shares, dec("100"), "权重占比之和 = 100%");
        assert_eq!(share_of(&specs[1], &rollup), dec("50"), "权重 2 / 合计 4 = 50%");
    }

    #[test]
    fn rollup_of_empty_is_none() {
        assert_eq!(rollup_of(&[]), None, "没有工序就没有加权汇总");
    }

    // ---------- 状态闸 ----------

    #[test]
    fn open_object_without_settled_periods_is_editable() {
        let gate = gate_of(cost_object_dto::STATUS_OPEN, &[]);
        assert!(gate.editable());
        assert_eq!(gate.state(), "editable");
        assert!(gate.reason().is_none());
    }

    #[test]
    fn closed_and_cancelled_objects_are_locked_with_label() {
        for (status, label) in [
            (cost_object_dto::STATUS_CLOSED, "已关批"),
            (cost_object_dto::STATUS_CANCELLED, "已作废"),
        ] {
            let gate = gate_of(status, &[]);
            assert!(!gate.editable());
            assert_eq!(gate.state(), "locked");
            let reason = gate.reason().expect("锁必须有原因");
            assert!(reason.contains(label), "报文要点名状态：{reason}");
        }
    }

    #[test]
    fn settled_periods_lock_the_object_even_when_open() {
        let gate = gate_of(
            cost_object_dto::STATUS_OPEN,
            &[("2026-10".to_string(), "SETTLED".to_string())],
        );
        assert!(!gate.editable());
        let reason = gate.reason().expect("锁必须有原因");
        assert!(reason.contains("2026-10"), "报文要点名已结算期间：{reason}");
        assert!(reason.contains("红冲"), "要给出路：{reason}");
    }

    // ---------- 提醒 ----------

    #[test]
    fn warns_when_wip_without_progress() {
        let specs = vec![spec(1, "1", "0")];
        let rollup = rollup_of(&specs);
        let warnings = warnings_of(dec("40"), dec("0"), &specs, rollup.as_ref());
        assert!(
            warnings.iter().any(|text| text.contains("66067")),
            "在制 > 0 且进度 0 必须提示 66067：{warnings:?}"
        );
    }

    #[test]
    fn warns_when_no_wip_even_if_progress_is_filled() {
        let specs = vec![spec(1, "1", "80")];
        let rollup = rollup_of(&specs);
        let warnings = warnings_of(Decimal::ZERO, dec("80"), &specs, rollup.as_ref());
        assert!(
            warnings.iter().any(|text| text.contains("期末在制是 0")),
            "没在制时要说明完工率不参与：{warnings:?}"
        );
    }

    #[test]
    fn warns_when_all_weights_are_equal() {
        let specs = vec![spec(1, "1", "100"), spec(2, "1", "0")];
        let rollup = rollup_of(&specs);
        let warnings = warnings_of(dec("10"), dec("50"), &specs, rollup.as_ref());
        assert!(
            warnings.iter().any(|text| text.contains("算术平均")),
            "权重全相同要提示：{warnings:?}"
        );
    }

    #[test]
    fn warns_when_object_level_was_edited_out_of_band() {
        let specs = vec![spec(1, "1", "100"), spec(2, "1", "0")];
        let rollup = rollup_of(&specs);
        let warnings = warnings_of(dec("10"), dec("80"), &specs, rollup.as_ref());
        assert!(
            warnings.iter().any(|text| text.contains("不一致")),
            "对象级被直接改过要提示：{warnings:?}"
        );
        // 一致时不应该有这条
        let clean = warnings_of(dec("10"), dec("50"), &specs, rollup.as_ref());
        assert!(!clean.iter().any(|text| text.contains("不一致")), "{clean:?}");
    }

    #[test]
    fn no_warnings_without_rows() {
        assert!(warnings_of(dec("10"), dec("0"), &[], None).is_empty());
    }

    // ---------- 文案 ----------

    #[test]
    fn text_of_drops_trailing_zeros_but_keeps_value() {
        assert_eq!(text_of(dec("50.00")), "50");
        assert_eq!(text_of(dec("33.3350")), "33.335");
        assert_eq!(fixed_of(dec("50"), 4), "50.0000");
        assert_eq!(fixed_of(dec("0.0001"), 4), "0.0001");
    }

    #[test]
    fn permission_codes_match_the_cost_object_handler() {
        // 复用码写在两个文件里：一处改了另一处没改 = 路由挂在一个没人持有的码上（403）
        use crate::business::erp::cost::cost_object_handler;
        assert_eq!(PERM_READ, cost_object_handler::PERM_LIST);
        assert_eq!(PERM_WRITE, cost_object_handler::PERM_UPDATE);
        assert_eq!(PERMISSIONS.len(), 2, "本模块只用两个既有码，不新增");
    }

    #[test]
    fn notes_name_the_object_and_the_rollup() {
        let empty = OperationRes::empty_note("CB-20261119-000001");
        assert!(empty.contains("CB-20261119-000001"));
        assert!(empty.contains("还没有工序"));
        let filled = OperationRes::filled_note("CB-20261119-000001", 3, "50");
        assert!(filled.contains("3 道工序"));
        assert!(filled.contains("50%"));
    }
}
