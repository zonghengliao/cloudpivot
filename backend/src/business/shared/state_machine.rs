//! 统一状态机（批次 2 · P10 · 老六「五条铁律」之 ③④）。
//!
//! 依据卡面 §二：**所有单据的状态变更必须走同一个入口**，入口负责
//! 「边表校验 → reason 校验 → 写 `biz_status_log`」，副作用（加库存 / 生成凭证）
//! 由调用方在**同一个事务**里接着做。
//!
//! # 为什么要有这个文件（开工实测的三个洞）
//!
//! ```text
//! ① 状态机可被绕过：
//!    purchase_model::LEGAL_EDGES 只有 4 条边，而 RECEIVED 是
//!    stockin_model::sync_purchase_status 用一句 `UPDATE … SET status = CASE …`
//!    直接写出来的 —— 完全没走 can_transition()。
//!    绕过一次 = 状态机不存在。
//! ② 没有状态变更日志：五张单据表只有 created_by / update_by / updated_at，
//!    谁在什么时候改了状态查不到。
//! ③ 入账后还能改：POSTED / RECEIVED / SETTLED 状态下金额、数量、明细没有硬约束。
//! ```
//!
//! # 三个公开能力
//!
//! | 函数 | 职责 |
//! |------|------|
//! | [`transition`] | 唯一状态变更入口：校验 + 写日志 |
//! | [`transition_if_changed`] | 同上，但**状态没变就跳过**（业务性推进专用，返回 `Ok(None)`） |
//! | [`log_created`] | 建单留痕（`from_status = NULL`） |
//! | [`ensure_writable`] | 入账态只读闸（各 `update_*` 入口调用） |
//!
//! # 边表的唯一真相
//!
//! 五个实体的合法边、需要 reason 的边、入账态清单都写在 [`Entity`] 里，
//! 各 model 保留自己的 `LEGAL_EDGES` 常量（指向本文件的常量数组），
//! [`transition`] 会**核对**传进来的 `edges` 与注册表完全一致 ——
//! 谁想「顺手多放一条边」会立刻拿到 500 而不是静默放行（硬红线 5 的机器化）。
//!
//! # 日志只增不改
//!
//! `biz_status_log`（V0159）没有 `deleted_at` / `updated_at`，代码里也只有 INSERT：
//! 日志与业务变更同事务，任一方失败整笔回滚，「业务改了没日志」不可能出现。
//!
//! # 自环
//!
//! `from == to` **不是状态变更**（如 `PARTIAL_RECEIVED → PARTIAL_RECEIVED` 继续部分收货）。
//! [`transition`] 一律拒绝自环并给出 409 —— 它只写「真实变化」的日志；
//! 业务流程里正常的原地推进用 [`transition_if_changed`]（显式声明「允许没变」，
//! 状态相同则跳过、只留业务流水）。**别在调用点自己 `if from != to { transition() }`**
//! ——那种写法无法与本模块的注册表核对共存，也没法在代码里一眼看出「这里是故意的」。
//!
//! 反面教材（LNK-P08）：`payable_payment` 无条件调 `transition`，于是「分三期付款」的
//! 第二期 `PARTIAL_PAID → PARTIAL_PAID` 被 409 挡住 —— 正常业务路径直接断。

use std::result::Result;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

// ==================== 实体类型 ====================

/// 实体类型：收货入库单。
pub const ENTITY_STOCK_IN: &str = "STOCK_IN";
/// 实体类型：采购订单。
pub const ENTITY_PURCHASE_ORDER: &str = "PURCHASE_ORDER";
/// 实体类型：库存调拨单。
pub const ENTITY_TRANSFER: &str = "TRANSFER";
/// 实体类型：应付单。
pub const ENTITY_PAYABLE: &str = "PAYABLE";
/// 实体类型：凭证。
pub const ENTITY_VOUCHER: &str = "VOUCHER";

/// 收货入库单合法边（与 `stockin_model::LEGAL_EDGES` 同源）。
///
/// P16 起多出 3 条**作废审批**边（六本账反向操作必须先复核）：
/// ```text
/// POSTED          → PENDING_CANCEL   发起作废审批（只挂审批单，账不动）
/// PENDING_CANCEL  → CANCELLED        审批通过 → 才执行 P9 的六本账回滚
/// PENDING_CANCEL  → POSTED           驳回 / 申请人撤回 → 回到已过账，账一点没动
/// ```
///
/// P-FLOW-3A 起再多 4 条**过账审批**边（入库链：先审后过账）：
/// ```text
/// DRAFT          → PENDING_POST   提交过账审批（只挂审批单，库存一分没加）
/// PENDING_POST   → POSTED         审批通过 → 才执行 post_in_tx（六本账一起动）
/// PENDING_POST   → DRAFT          驳回 / 撤回 → 打回草稿，账一点没动
/// PENDING_POST   → CANCELLED      审批中作废（撤审批单 + 单据作废，从未入过账所以不冲账）
/// ```
pub const STOCK_IN_EDGES: [(&str, &str); 10] = [
    ("DRAFT", "POSTED"),
    ("DRAFT", "CANCELLED"),
    ("POSTED", "CANCELLED"),
    ("POSTED", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
    ("PENDING_CANCEL", "POSTED"),
    ("DRAFT", "PENDING_POST"),
    ("PENDING_POST", "POSTED"),
    ("PENDING_POST", "DRAFT"),
    ("PENDING_POST", "CANCELLED"),
];

/// 采购订单合法边（P10 · 卡面 §三 的 8 条 + 收货白名单必需的 2 条）。
///
/// 相对 P2-A2 的 4 条补了 `SUBMITTED→APPROVED` / `APPROVED→PARTIAL_RECEIVED` /
/// `APPROVED→RECEIVED` / `PARTIAL_RECEIVED→RECEIVED`。
/// **故意不补自环** `PARTIAL_RECEIVED→PARTIAL_RECEIVED`：继续部分收货不是状态变更，
/// 由调用方判「状态没变就不调 transition」，只写收货流水（卡面 §三 的 ⚠️）。
///
/// # 为什么比卡面的 8 条多两条（`SUBMITTED→*_RECEIVED`）
///
/// 开工实测：`APPROVED` **没有任何写入方** —— `purchase_dto::TRANSITION_TARGETS`
/// 只放行 `SUBMITTED / CLOSED / CANCELLED`，审批接入也没接采购单，而全库现存采购单
/// 的初始提交态就是 `SUBMITTED`（P2-B1 的收货白名单 `purchase_receivable` 本来就收
/// `SUBMITTED`）。若严格只登记卡面的 8 条，**所有真实采购单收货都会 409**
/// —— 那不是「收紧」而是把功能打断（P9/P2-B1 的收货链路当场不可用）。
/// 所以这两条边是**把既有生产行为登记进状态机**，不是为过测试放宽；`DRAFT → RECEIVED`
/// 依旧非法（单测 ⑯ 钉死）。
///
/// # D4 追加的第 11 条（`RECEIVED → PARTIAL_RECEIVED`）
///
/// 收货单可以作废（`POSTED → CANCELLED`，P9/P16 的既有能力）。作废之后：
/// 库存 / 成本六本账回滚、采购明细的 `received_qty` 回退 —— 此时采购单如果还是
/// `RECEIVED`，就变成「已收货却没有一分已收数量」，而且 `RECEIVED` 不在收货白名单里，
/// **老板再也收不进这批货**（D4 要解决的正是"收不了货"）。
///
/// 所以补这条回退边：**只在收货单作废的回退路径上使用**（`sync_purchase_status`
/// 的 `reason` 写明是哪张收货单作废触发的）。正向的 `DRAFT → RECEIVED` 依旧非法。
pub const PURCHASE_ORDER_EDGES: [(&str, &str); 13] = [
    ("DRAFT", "SUBMITTED"),
    ("DRAFT", "CANCELLED"),
    ("SUBMITTED", "APPROVED"),
    ("SUBMITTED", "CLOSED"),
    ("SUBMITTED", "CANCELLED"),
    ("SUBMITTED", "PARTIAL_RECEIVED"),
    ("SUBMITTED", "RECEIVED"),
    // P-FLOW-3A：审批驳回 → 打回草稿（改完可重新提交，`DRAFT → SUBMITTED` 已在上方）
    ("SUBMITTED", "DRAFT"),
    ("APPROVED", "PARTIAL_RECEIVED"),
    ("APPROVED", "RECEIVED"),
    // P-FLOW-3A：审批通过后取消（`PURCHASE_ORDER_REASON_EDGES` 早就登记了这条，缺边=真流转 409）
    ("APPROVED", "CANCELLED"),
    ("PARTIAL_RECEIVED", "RECEIVED"),
    ("RECEIVED", "PARTIAL_RECEIVED"),
];

/// 库存调拨单合法边（与 `transfer_model::LEGAL_EDGES` 同源，P5 的 6 条）。
pub const TRANSFER_EDGES: [(&str, &str); 6] = [
    ("DRAFT", "PENDING"),
    ("PENDING", "APPROVED"),
    ("PENDING", "CANCELLED"),
    ("APPROVED", "IN_TRANSIT"),
    ("IN_TRANSIT", "DONE"),
    ("DRAFT", "CANCELLED"),
];

/// 应付单合法边（P7 的付款累计 + 作废）。
pub const PAYABLE_EDGES: [(&str, &str); 4] = [
    ("OPEN", "PARTIAL_PAID"),
    ("OPEN", "SETTLED"),
    ("PARTIAL_PAID", "SETTLED"),
    ("OPEN", "CANCELLED"),
];

/// 凭证没有「状态流转」：一落库就是 `POSTED`，更正靠**红冲开新单**，不原地改状态。
pub const VOUCHER_EDGES: [(&str, &str); 0] = [];

/// 需要 reason 的边（作废已入账单据：红冲必须能追溯到为什么）。
///
/// P16：作废拆成「发起（POSTED→PENDING_CANCEL）」和「执行（PENDING_CANCEL→CANCELLED）」两步，
/// 两步都要原因 —— 前者是**申请理由**（审批人据此判断），后者是**记账依据**（红冲凭证的摘要）。
pub const STOCK_IN_REASON_EDGES: [(&str, &str); 3] = [
    ("POSTED", "CANCELLED"),
    ("POSTED", "PENDING_CANCEL"),
    ("PENDING_CANCEL", "CANCELLED"),
];
/// 需要 reason 的边：已提交的采购单作废。
pub const PURCHASE_ORDER_REASON_EDGES: [(&str, &str); 2] =
    [("SUBMITTED", "CANCELLED"), ("APPROVED", "CANCELLED")];
/// 需要 reason 的边：调拨单取消。
pub const TRANSFER_REASON_EDGES: [(&str, &str); 2] =
    [("DRAFT", "CANCELLED"), ("PENDING", "CANCELLED")];
/// 需要 reason 的边：应付作废（会计上作废必须留痕）。
pub const PAYABLE_REASON_EDGES: [(&str, &str); 1] = [("OPEN", "CANCELLED")];
/// 凭证没有流转边，自然也没有 reason 边。
pub const VOUCHER_REASON_EDGES: [(&str, &str); 0] = [];

/// 入账态（这些状态下不允许改业务字段，见 [`ensure_writable`]）。
///
/// `PENDING_CANCEL` 也锁：作废审批中如果还能改数量 / 金额 / 明细，审批人批的就是一份
/// 已经变过的数据（审批对象与执行对象不是同一张单），复核等于没做。
///
/// `PENDING_POST`（P-FLOW-3A）同理且更严格：过账审批批的是「这一版的数量与金额」，
/// 批完立刻按单据落库存 / 凭证 / 应付，中途改一行就是「审 A 记 B」。
pub const STOCK_IN_LOCKED: [&str; 3] = ["POSTED", "PENDING_CANCEL", "PENDING_POST"];
/// 采购订单入账态：收过货的单据不能再改金额 / 明细。
pub const PURCHASE_ORDER_LOCKED: [&str; 2] = ["RECEIVED", "PARTIAL_RECEIVED"];
/// 调拨单入账态：审批通过即锁定（在途 / 完成都不可改）。
pub const TRANSFER_LOCKED: [&str; 3] = ["APPROVED", "IN_TRANSIT", "DONE"];
/// 应付入账态：已结清 / 已作废都锁定（作废的单子当然不能再改）。
pub const PAYABLE_LOCKED: [&str; 2] = ["SETTLED", "CANCELLED"];
/// 凭证入账态：已记账。
pub const VOUCHER_LOCKED: [&str; 1] = ["POSTED"];

/// 单据类型注册表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    /// 收货入库单 `biz_stock_in`。
    StockIn,
    /// 采购订单 `biz_purchase_order`。
    PurchaseOrder,
    /// 库存调拨单 `biz_stock_transfer`。
    Transfer,
    /// 应付单 `biz_payable`。
    Payable,
    /// 凭证 `biz_voucher`。
    Voucher,
}

impl Entity {
    /// 全部实体（注册表全量，未知类型就是对表外的字符串）。
    pub const ALL: [Entity; 5] = [
        Entity::StockIn,
        Entity::PurchaseOrder,
        Entity::Transfer,
        Entity::Payable,
        Entity::Voucher,
    ];

    /// 落库的 `entity_type`。
    pub const fn as_str(self) -> &'static str {
        match self {
            Entity::StockIn => ENTITY_STOCK_IN,
            Entity::PurchaseOrder => ENTITY_PURCHASE_ORDER,
            Entity::Transfer => ENTITY_TRANSFER,
            Entity::Payable => ENTITY_PAYABLE,
            Entity::Voucher => ENTITY_VOUCHER,
        }
    }

    /// 字符串 → 实体。**未知一律 `None`**（调用方必须报错，不许静默）。
    pub fn parse(raw: &str) -> Option<Entity> {
        Entity::ALL.into_iter().find(|item| item.as_str() == raw)
    }

    /// 中文名（错误文案 / 日志展示共用）。
    pub const fn label(self) -> &'static str {
        match self {
            Entity::StockIn => "收货入库单",
            Entity::PurchaseOrder => "采购订单",
            Entity::Transfer => "库存调拨单",
            Entity::Payable => "应付单",
            Entity::Voucher => "凭证",
        }
    }

    /// 合法边表（唯一真相）。
    pub const fn edges(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Entity::StockIn => &STOCK_IN_EDGES,
            Entity::PurchaseOrder => &PURCHASE_ORDER_EDGES,
            Entity::Transfer => &TRANSFER_EDGES,
            Entity::Payable => &PAYABLE_EDGES,
            Entity::Voucher => &VOUCHER_EDGES,
        }
    }

    /// 需要 reason 的边。
    pub const fn reason_edges(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Entity::StockIn => &STOCK_IN_REASON_EDGES,
            Entity::PurchaseOrder => &PURCHASE_ORDER_REASON_EDGES,
            Entity::Transfer => &TRANSFER_REASON_EDGES,
            Entity::Payable => &PAYABLE_REASON_EDGES,
            Entity::Voucher => &VOUCHER_REASON_EDGES,
        }
    }

    /// 入账态（只读）。
    pub const fn locked_statuses(self) -> &'static [&'static str] {
        match self {
            Entity::StockIn => &STOCK_IN_LOCKED,
            Entity::PurchaseOrder => &PURCHASE_ORDER_LOCKED,
            Entity::Transfer => &TRANSFER_LOCKED,
            Entity::Payable => &PAYABLE_LOCKED,
            Entity::Voucher => &VOUCHER_LOCKED,
        }
    }

    /// 某状态能去的下一个状态（空 = 终态）。
    pub fn next_statuses(self, from: &str) -> Vec<&'static str> {
        self.edges()
            .iter()
            .filter(|(f, _)| *f == from)
            .map(|(_, t)| *t)
            .collect()
    }

    /// 合法边判定。
    pub fn can_transition(self, from: &str, to: &str) -> bool {
        self.edges().iter().any(|(f, t)| *f == from && *t == to)
    }

    /// 这条边是否必须有 reason。
    pub fn needs_reason(self, from: &str, to: &str) -> bool {
        self.reason_edges()
            .iter()
            .any(|(f, t)| *f == from && *t == to)
    }

    /// 该状态是否入账态。
    pub fn is_locked(self, status: &str) -> bool {
        self.locked_statuses().contains(&status)
    }
}

// ==================== 操作人与返回 ====================

/// 操作人（`operator` 入参的统一形态）。
///
/// 卡面写的是 `&Principal`，但仓里没有这个类型：handler 拿到的是框架的
/// [`TokenClaims`]，而 model / 事务体只带 `Option<Uuid>`。为了让**两边都能直接传**，
/// 这里收一个 `impl Into<Operator>`：`None` / `Some(uuid)` / `&TokenClaims` 都行。
#[derive(Debug, Clone, Default)]
pub struct Operator {
    /// 用户 id（落 `operator_id`）。
    pub id: Option<Uuid>,
    /// 用户名（落 `operator_name`：日志的可读性靠它，权威关联仍靠 id）。
    pub name: Option<String>,
}

impl Operator {
    /// 只有 id（model / 事务体路径）。
    pub fn of(id: Option<Uuid>) -> Self {
        Self { id, name: None }
    }

    /// id + 用户名。
    pub fn named(id: Option<Uuid>, name: impl Into<String>) -> Self {
        let name = name.into();
        let name = name.trim();
        Self {
            id,
            name: if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            },
        }
    }
}

impl From<Option<Uuid>> for Operator {
    fn from(value: Option<Uuid>) -> Self {
        Operator::of(value)
    }
}

impl From<Uuid> for Operator {
    fn from(value: Uuid) -> Self {
        Operator::of(Some(value))
    }
}

impl From<&TokenClaims> for Operator {
    fn from(claims: &TokenClaims) -> Self {
        Operator::named(
            crate::utils::current_user_uuid(claims),
            claims.username.clone().unwrap_or_default(),
        )
    }
}

/// 一次状态变更的返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    /// 变更前状态。
    pub from: String,
    /// 变更后状态。
    pub to: String,
    /// `biz_status_log` 的主键（调用方 trace / 断言用）。
    pub log_id: Uuid,
}

/// `biz_status_log` 的一行（轨迹查询）。
#[derive(Debug, Clone)]
pub struct StatusLogRow {
    /// 主键（与 [`TransitionOutcome::log_id`] 对应）。
    pub id: Uuid,
    /// 单据类型。
    pub entity_type: String,
    /// 单据 id。
    pub entity_id: Uuid,
    /// 单号。
    pub entity_no: String,
    /// 变更前（NULL = 建单）。
    pub from_status: Option<String>,
    /// 变更后。
    pub to_status: String,
    /// 操作人 id。
    pub operator_id: Option<Uuid>,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 变更原因。
    pub reason: Option<String>,
    /// 关联单据类型。
    pub biz_ref_type: Option<String>,
    /// 关联单号。
    pub biz_ref_no: Option<String>,
    /// 写入时间。
    pub created_at: DateTime<Utc>,
}

// ==================== 唯一入口 ====================

/// 未知实体类型：**明确报错**，不静默放过（卡面单测 ⑤）。
fn unknown_entity(entity_type: &str) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!(
            "未知的单据类型 `{entity_type}`：状态机只认 {}（shared/state_machine.rs 的实体注册表），\
             新实体必须先在注册表登记合法边 / 入账态，再用 transition() 改状态",
            Entity::ALL
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        ),
    )
}

/// 非法流转的 409 文案（点名「当前态的可选去向」）。
pub fn rejection_message(entity_type: &str, from: &str, to: &str) -> String {
    let Some(entity) = Entity::parse(entity_type) else {
        return format!("单据类型 {entity_type} 未在状态机注册，不能流转到 {to}");
    };
    let allowed = entity.next_statuses(from);
    if allowed.is_empty() {
        return format!(
            "{}当前是「{from}」，没有可流转的下一个状态：它已是终态，或该状态由其它流程维护",
            entity.label()
        );
    }
    format!(
        "{}当前是「{from}」，不能直接流转到「{to}」：允许的去向是 {}",
        entity.label(),
        allowed
            .iter()
            .map(|status| format!("「{status}」"))
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

/// 注册表核对：未知实体 / 边表与注册表不一致 → **500**（配置漂移，不是用户错）。
fn check_registration(entity_type: &str, edges: &[(&str, &str)]) -> Result<Entity, ApiError> {
    let entity = Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    if edges != entity.edges() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!(
                "{entity_type} 的边表与状态机注册表不一致：{} 传了 {} 条，注册表是 {} 条。\
                 改边表必须改 shared/state_machine.rs 的注册表（唯一真相），不能在调用点私自放宽",
                entity_type,
                edges.len(),
                entity.edges().len()
            ),
        ));
    }
    Ok(entity)
}

/// **唯一状态变更入口**：校验边 → 校验 reason → 写 `biz_status_log`。
///
/// 调用方必须在**已经开着的事务**里调用，并在返回后继续做副作用
/// （加库存 / 生成凭证 / 改状态本身）；任一步失败整笔回滚，日志不会半条落地。
///
/// # 参数
///
/// * `entity_type` —— `STOCK_IN` / `PURCHASE_ORDER` / `TRANSFER` / `PAYABLE` / `VOUCHER`
/// * `edges` —— 调用方 model 里的 `LEGAL_EDGES`（会与注册表核对，防「偷偷多放一条边」）
/// * `biz_ref` —— 触发本次变更的关联单据 `(类型, 单号)`，如 `("STOCK_IN", "IN-…")`
///
/// # 错误
///
/// * 未知 `entity_type` / 边表与注册表不一致 / 单号为空 → **500**（配置漂移，不是用户错）
/// * 非法边（含自环）→ **409** + 当前态可选去向
/// * 需要 reason 的边缺 reason → **400**
#[allow(clippy::too_many_arguments)]
pub async fn transition(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    edges: &[(&str, &str)],
    from: &str,
    to: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<TransitionOutcome, ApiError> {
    let entity = check_registration(entity_type, edges)?;
    let no = entity_no.trim();
    if no.is_empty() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{entity_type} 的状态变更缺少单号：状态日志没有单号就查不到轨迹"),
        ));
    }
    if from == to {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "{} {} 的状态没有变化（{from} → {to}）：原地流转不是状态变更，\
                 不要写状态日志（重复部分收货请只写业务流水）",
                entity.label(),
                no
            ),
        ));
    }
    if !entity.can_transition(from, to) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            rejection_message(entity_type, from, to),
        ));
    }

    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    if entity.needs_reason(from, to) && reason.is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{} {} 从 {from} 流转到 {to} 必须给出原因（reason）：\
                 该动作会影响已入账的数据或已生效的承诺，原因要能追溯到为什么",
                entity.label(),
                no
            ),
        ));
    }

    let operator = operator.into();
    let log_id = insert_log(
        ex,
        entity_type,
        entity_id,
        no,
        Some(from),
        to,
        &operator,
        reason,
        biz_ref,
    )
    .await?;
    Ok(TransitionOutcome {
        from: from.to_string(),
        to: to.to_string(),
        log_id,
    })
}

/// **「状态真的变了才写日志」的入口**：业务性推进（部分付款 / 继续部分收货）用它。
///
/// | `from` vs `to` | 行为 |
/// |---|---|
/// | 不同 | 等价于 [`transition`]，写一行 `biz_status_log`，返回 `Ok(Some(outcome))` |
/// | 相同（原地流转） | **不写日志**，返回 `Ok(None)` —— 这是「没变」而不是「失败」 |
///
/// # 为什么不让 [`transition`] 自己放过自环（LNK-P08 的教训）
///
/// `from == to` 的 409 是 P10 铁律③的刹车：`transition` 是**五个实体共用的唯一入口**，
/// 放宽它会让所有「调用点写错了」的回归都变成静默的空日志。
/// 但「同一状态下的第二次业务动作」是**正常业务**（应付第二期部分付款
/// `PARTIAL_PAID → PARTIAL_PAID`、采购单继续部分收货），它本来就不该产生状态日志。
///
/// 所以「允许原地不动」这件事必须由**调用点显式声明**（用本函数而不是 [`transition`]），
/// 语义一目了然：`Ok(None)` = 状态没变、只该留业务流水。
///
/// 注册表核对（未知实体 / 边表漂移 500）在自环短路**之前**做——
/// 否则调用点的实体名写错会被自环静默吞掉。
// 与 [`transition`] 同款签名（同一批入参 + 同一套校验），参数数量由状态机协议决定；
// 拆结构体会让两个入口的调用点写法不一致，反而更容易写错。
#[allow(clippy::too_many_arguments)]
pub async fn transition_if_changed(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    edges: &[(&str, &str)],
    from: &str,
    to: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<Option<TransitionOutcome>, ApiError> {
    check_registration(entity_type, edges)?;
    if from == to {
        return Ok(None);
    }
    let outcome = transition(
        ex,
        entity_type,
        entity_id,
        entity_no,
        edges,
        from,
        to,
        operator,
        reason,
        biz_ref,
    )
    .await?;
    Ok(Some(outcome))
}

/// 建单留痕（`from_status = NULL` → `to_status`）。
///
/// 建单本身不是「状态流转」（没有 from），所以单独一个入口，写同构的一行日志，
/// 让「null→DRAFT」「null→OPEN」「null→POSTED」也能在轨迹里看到。
pub async fn log_created(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    to_status: &str,
    operator: impl Into<Operator>,
    reason: Option<&str>,
) -> Result<Uuid, ApiError> {
    Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    let no = entity_no.trim();
    if no.is_empty() || to_status.trim().is_empty() {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{entity_type} 的建单日志缺单号或目标状态（单号 / 状态是轨迹的唯一线索）"),
        ));
    }
    let operator = operator.into();
    insert_log(
        ex,
        entity_type,
        entity_id,
        no,
        None,
        to_status,
        &operator,
        reason.map(str::trim).filter(|text| !text.is_empty()),
        None,
    )
    .await
}

/// 写一行 `biz_status_log`（**只有 INSERT，没有 UPDATE**）。
#[allow(clippy::too_many_arguments)]
async fn insert_log(
    ex: &impl SqlExecutor,
    entity_type: &str,
    entity_id: Uuid,
    entity_no: &str,
    from_status: Option<&str>,
    to_status: &str,
    operator: &Operator,
    reason: Option<&str>,
    biz_ref: Option<(&str, &str)>,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let (ref_type, ref_no) = match biz_ref {
        Some((kind, no)) => (kind.trim().to_string(), no.trim().to_string()),
        None => (String::new(), String::new()),
    };
    ex.exec_sql(
        "INSERT INTO biz_status_log \
            (id, entity_type, entity_id, entity_no, from_status, to_status, \
             operator_id, operator_name, reason, biz_ref_type, biz_ref_no, created_at) \
         VALUES (?, ?, ?::uuid, ?, NULLIF(?::text,''), ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
                 NULLIF(?::text,''), ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(entity_type.to_string()),
            Value::from(&entity_id),
            Value::Text(entity_no.to_string()),
            Value::Text(from_status.unwrap_or_default().to_string()),
            Value::Text(to_status.to_string()),
            Value::Text(
                operator
                    .id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(operator.name.clone().unwrap_or_default()),
            Value::Text(reason.unwrap_or_default().to_string()),
            Value::Text(ref_type),
            Value::Text(ref_no),
            Value::from(Utc::now()),
        ],
    )
    .await
    .map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("写状态变更日志失败（{entity_type} {entity_no} → {to_status}）：{error}"),
        )
    })?;
    Ok(id)
}

// ==================== 入账后只读 ====================

/// 入账态只读的 409 文案。
pub fn locked_message(entity_type: &str, status: &str, entity_no: &str) -> String {
    let label = Entity::parse(entity_type)
        .map(|item| item.label())
        .unwrap_or("单据");
    format!(
        "单据已入账（状态 {status}），金额/数量/明细不可修改。\n 如需更正请先作废或红冲后重开。\
         （{label} {entity_no}；白名单：remark / status / updated_at / update_by / 系统回写字段）"
    )
}

/// 入账态只读闸：**各 `update_*` 入口必须调**（不靠前端隐藏按钮）。
///
/// 未登记的实体类型同样 500 —— 新单据忘了登记就等于没有只读保护，必须早失败。
pub fn ensure_writable(entity_type: &str, status: &str, entity_no: &str) -> Result<(), ApiError> {
    let entity = Entity::parse(entity_type).ok_or_else(|| unknown_entity(entity_type))?;
    if entity.is_locked(status) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            locked_message(entity_type, status, entity_no),
        ));
    }
    Ok(())
}

// ==================== 轨迹查询 ====================

/// 按单号读完整轨迹（时间正序）。
///
/// 详情页「状态历史」与 e2e 断言共用；`entity_no` 是人对得上的唯一标识，
/// 单据软删 / 硬删之后日志仍然查得到。
pub async fn trail_of_no(ex: &impl SqlExecutor, entity_no: &str) -> DbResult<Vec<StatusLogRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, entity_type, entity_id, entity_no, from_status, to_status, operator_id, \
                    operator_name, reason, biz_ref_type, biz_ref_no, created_at \
               FROM biz_status_log WHERE entity_no = ? ORDER BY created_at ASC, id ASC"
                .to_string(),
            vec![Value::Text(entity_no.to_string())],
        )
        .await?;
    rows.iter().map(StatusLogRow::from_row).collect()
}

impl StatusLogRow {
    /// 行 → 结构体（可空列一律 `get_typed::<Option<_>>`）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            entity_type: row.get_typed("entity_type")?,
            entity_id: row.get_typed("entity_id")?,
            entity_no: row.get_typed("entity_no")?,
            from_status: row.get_typed::<Option<String>>("from_status")?,
            to_status: row.get_typed("to_status")?,
            operator_id: row.get_typed::<Option<Uuid>>("operator_id")?,
            operator_name: row.get_typed::<Option<String>>("operator_name")?,
            reason: row.get_typed::<Option<String>>("reason")?,
            biz_ref_type: row.get_typed::<Option<String>>("biz_ref_type")?,
            biz_ref_no: row.get_typed::<Option<String>>("biz_ref_no")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

// ==================== 单测（真库）====================
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0159 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// export ERP_DB_URL='postgres://p10role:p10pass@127.0.0.1:5432/cloudpivot_p10'
// cargo test --lib business::shared::state_machine
// ```
//
// 夹具：`entity_no` 一律 `P10SM-` 前缀，跑前跑后按前缀清；`biz_status_log` 没有外键，
// 所以不需要造真实单据行 —— 这也正好证明「日志比业务行活得久」。
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    const PREFIX: &str = "P10SM-";

    /// 连库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    async fn cleanup() {
        db().execute_with_params(
            "DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SM-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理状态日志失败：{err}"));
        db().execute_with_params(
            "DELETE FROM biz_status_log WHERE entity_no LIKE 'P10SMV-%'",
            vec![],
        )
        .await
        .unwrap();
    }

    fn op() -> Operator {
        Operator::named(
            Some(Uuid::from_str("00000000-0000-4000-8000-0000000000a1").unwrap()),
            "p10_tester",
        )
    }

    async fn count_of(entity_no: &str) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_status_log WHERE entity_no = ?",
            vec![Value::Text(entity_no.to_string())],
        )
        .await
        .unwrap()
    }

    /// ① 合法边通过，写一行字段完整的 `biz_status_log`。
    #[tokio::test]
    async fn legal_edge_writes_complete_log_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}A");
        let outcome = transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            Some((ENTITY_STOCK_IN, "IN-20261031-000001")),
        )
        .await
        .unwrap();
        assert_eq!(outcome.from, "DRAFT");
        assert_eq!(outcome.to, "POSTED");

        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 1);
        let row = &trail[0];
        assert_eq!(row.entity_type, ENTITY_STOCK_IN);
        assert_eq!(row.entity_id, id);
        assert_eq!(row.entity_no, no);
        assert_eq!(row.from_status.as_deref(), Some("DRAFT"));
        assert_eq!(row.to_status, "POSTED");
        assert_eq!(row.operator_name.as_deref(), Some("p10_tester"));
        assert!(row.operator_id.is_some(), "操作人 id 必须落库");
        assert_eq!(row.biz_ref_type.as_deref(), Some(ENTITY_STOCK_IN));
        assert_eq!(row.biz_ref_no.as_deref(), Some("IN-20261031-000001"));
        assert_eq!(row.id, outcome.log_id, "返回的 log_id 必须是真落库的那一行");
    }

    /// ② 非法边 → 409，文案点名当前态可选去向。
    #[tokio::test]
    async fn illegal_edge_is_conflict_and_names_targets() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}B"),
            &STOCK_IN_EDGES,
            "POSTED",
            "DRAFT",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("POSTED"), "{}", err.msg);
        assert!(err.msg.contains("CANCELLED"), "可选去向要点名：{}", err.msg);
        assert_eq!(count_of(&format!("{PREFIX}B")).await, 0, "非法边不写日志");
    }

    /// ③ 需要 reason 的边缺 reason → 400。
    #[tokio::test]
    async fn reason_required_edge_without_reason_is_bad_request() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}C"),
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("reason"), "{}", err.msg);
        assert_eq!(count_of(&format!("{PREFIX}C")).await, 0);

        // 给了 reason 就放行
        transition(
            db(),
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &format!("{PREFIX}C"),
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            Some("客户退货"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(count_of(&format!("{PREFIX}C")).await, 1);
    }

    /// ④ 同一单据连续两次流转 → 两行日志，时间顺序正确。
    #[tokio::test]
    async fn two_transitions_write_two_ordered_rows() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}D");
        transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap();
        transition(
            db(),
            ENTITY_STOCK_IN,
            id,
            &no,
            &STOCK_IN_EDGES,
            "POSTED",
            "CANCELLED",
            op(),
            Some("红冲重开"),
            None,
        )
        .await
        .unwrap();

        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].to_status, "POSTED");
        assert_eq!(trail[1].to_status, "CANCELLED");
        assert_eq!(trail[1].from_status.as_deref(), Some("POSTED"));
        assert!(
            trail[0].created_at <= trail[1].created_at,
            "日志必须能按时间还原顺序：{:?} / {:?}",
            trail[0].created_at,
            trail[1].created_at
        );
    }

    /// ⑤ 未知 entity_type → 明确报错（不静默）。
    #[tokio::test]
    async fn unknown_entity_type_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            "NOT_A_DOCUMENT",
            Uuid::now_v7(),
            &format!("{PREFIX}E"),
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500, "{}", err.msg);
        assert!(err.msg.contains("未知的单据类型"), "{}", err.msg);
        assert!(
            err.msg.contains(ENTITY_PURCHASE_ORDER),
            "要点名注册表：{}",
            err.msg
        );
    }

    /// ⑥ 按 entity_no 能查完整轨迹（多单据互不串）。
    #[tokio::test]
    async fn trail_by_entity_no_is_complete() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no_a = format!("{PREFIX}F1");
        let no_b = format!("{PREFIX}F2");
        for (no, to) in [
            (&no_a, "SUBMITTED"),
            (&no_a, "CLOSED"),
            (&no_b, "SUBMITTED"),
        ] {
            let from = if to == "CLOSED" { "SUBMITTED" } else { "DRAFT" };
            transition(
                db(),
                ENTITY_PURCHASE_ORDER,
                Uuid::now_v7(),
                no,
                &PURCHASE_ORDER_EDGES,
                from,
                to,
                op(),
                None,
                None,
            )
            .await
            .unwrap();
        }
        let trail_a = trail_of_no(db(), &no_a).await.unwrap();
        assert_eq!(trail_a.len(), 2);
        assert_eq!(trail_a[0].from_status.as_deref(), Some("DRAFT"));
        assert_eq!(trail_a[1].to_status, "CLOSED");
        let trail_b = trail_of_no(db(), &no_b).await.unwrap();
        assert_eq!(trail_b.len(), 1, "别的单号不能串进来");
    }

    /// ⑦ 事务内失败 → 日志一起回滚（不写半条）。
    #[tokio::test]
    async fn log_rolls_back_with_transaction() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}G");
        let tx = db().begin().await.unwrap();
        transition(
            &tx,
            ENTITY_STOCK_IN,
            Uuid::now_v7(),
            &no,
            &STOCK_IN_EDGES,
            "DRAFT",
            "POSTED",
            op(),
            None,
            None,
        )
        .await
        .unwrap();
        // 事务内可见
        let inside: i64 = tx
            .scalar_sql(
                "SELECT count(*) FROM biz_status_log WHERE entity_no = ?".to_string(),
                vec![Value::Text(no.clone())],
            )
            .await
            .unwrap();
        assert_eq!(inside, 1, "事务内必须能看到自己写的日志");
        tx.rollback().await.unwrap();
        assert_eq!(count_of(&no).await, 0, "回滚后不能留下半条日志");
    }

    /// ⑯ 非法边 `DRAFT → RECEIVED`（采购订单）→ 409。
    #[tokio::test]
    async fn purchase_order_draft_to_received_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = transition(
            db(),
            ENTITY_PURCHASE_ORDER,
            Uuid::now_v7(),
            &format!("{PREFIX}H"),
            &PURCHASE_ORDER_EDGES,
            "DRAFT",
            "RECEIVED",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("DRAFT") && err.msg.contains("RECEIVED"),
            "{}",
            err.msg
        );
        assert_eq!(count_of(&format!("{PREFIX}H")).await, 0);
    }

    /// ⑰ 自环护栏**不放宽**：`transition` 收到 `from == to` 仍然 409（LNK-P08 修的是调用点，
    /// 不是护栏）。这条用例是护栏的回归锁 —— 谁想「顺手放宽 state_machine」会先看到它红。
    #[tokio::test]
    async fn self_loop_is_still_rejected_by_transition() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}I");
        let err = transition(
            db(),
            ENTITY_PAYABLE,
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("状态没有变化"), "{}", err.msg);
        assert_eq!(count_of(&no).await, 0, "自环绝不能写日志");
    }

    /// ⑱ `transition_if_changed` 原地流转 → `Ok(None)` + **零日志**（分期付款第二期）。
    ///
    /// 同一位置也要挡住「实体名写错」：注册表核对在自环短路**之前**做，
    /// 否则调用点把 `PAYABLE` 写成 `PAYABLES` 会被自环静默吞掉。
    #[tokio::test]
    async fn transition_if_changed_skips_self_loop() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let no = format!("{PREFIX}J");
        let outcome = transition_if_changed(
            db(),
            ENTITY_PAYABLE,
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            Some("第二期部分付款"),
            None,
        )
        .await
        .unwrap();
        assert!(outcome.is_none(), "状态没变必须返回 Ok(None)");
        assert_eq!(count_of(&no).await, 0, "原地流转不许写状态日志");

        // 实体名写错 → 500，不被自环短路掩盖
        let err = transition_if_changed(
            db(),
            "PAYABLES",
            Uuid::now_v7(),
            &no,
            &PAYABLE_EDGES,
            "PARTIAL_PAID",
            "PARTIAL_PAID",
            op(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 500, "{}", err.msg);
        assert!(err.msg.contains("未知的单据类型"), "{}", err.msg);
    }

    /// ⑲ `transition_if_changed` 真变化 → `Ok(Some(..))`，日志与 `transition` 同款落库。
    #[tokio::test]
    async fn transition_if_changed_writes_when_changed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = Uuid::now_v7();
        let no = format!("{PREFIX}K");
        let outcome = transition_if_changed(
            db(),
            ENTITY_PAYABLE,
            id,
            &no,
            &PAYABLE_EDGES,
            "OPEN",
            "PARTIAL_PAID",
            op(),
            Some("首期部分付款"),
            Some((ENTITY_PAYABLE, "PM-P10SM-000001")),
        )
        .await
        .unwrap()
        .expect("状态真的变了，必须返回 Some");
        let trail = trail_of_no(db(), &no).await.unwrap();
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].id, outcome.log_id);
        assert_eq!(trail[0].from_status.as_deref(), Some("OPEN"));
        assert_eq!(trail[0].to_status, "PARTIAL_PAID");
        assert_eq!(trail[0].biz_ref_no.as_deref(), Some("PM-P10SM-000001"));
    }

    /// ⑫ POSTED 凭证改摘要 → 409（入账后只读，凭证侧）。
    #[tokio::test]
    async fn posted_voucher_summary_change_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 直接造一张 POSTED 凭证（本用例只验只读闸，不验分录平衡）
        let id = Uuid::now_v7();
        let text = id.to_string();
        let no = format!("P10SMV-{}", &text[text.len() - 8..]);
        db().execute_with_params(
            "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 voucher_type, source_type, summary, total_debit, total_credit, entry_count, \
                 status, is_red) \
             VALUES (?::uuid, '记', ?, '2026-10', CURRENT_DATE, 'AUTO', 'STOCK_IN', '测试摘要', \
                 0, 0, 2, 'POSTED', 0)",
            vec![Value::from(&id), Value::Text(no.clone())],
        )
        .await
        .unwrap();

        let err = crate::business::erp::voucher_write::update_summary(id, Some("改过的摘要"), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已入账"), "{}", err.msg);
        let summary: String = db()
            .scalar_with_params(
                "SELECT summary FROM biz_voucher WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(summary, "测试摘要", "409 时不能真改库");

        db().execute_with_params(
            "DELETE FROM biz_voucher WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    }
}
