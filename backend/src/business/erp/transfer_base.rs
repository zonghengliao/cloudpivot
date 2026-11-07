// ============================================================================
// transfer_base.rs —— 底座：表名/状态常量、6 态状态机、审批闸、行结构与列清单、取号、建单契约
//
// 由 `transfer_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model::period_key_of;
use crate::business::shared::scope;
use crate::business::shared::state_machine;

use super::transfer_scan::{is_in_transit_overdue, overdue_elapsed_days};

// ==================== 表名 / 列清单 ====================

/// 单据头表。
pub const TRANSFER_TABLE: &str = "biz_stock_transfer";

/// 明细表。
pub const ITEM_TABLE: &str = "biz_stock_transfer_item";

/// 序列号子表。
pub const ITEM_SERIAL_TABLE: &str = "biz_stock_transfer_item_serial";

/// 单据头列清单（[`TransferRow::from_row`] 与所有 SELECT 共用）。
pub const TRANSFER_COLUMNS: &str = "id, transfer_no, from_warehouse_id, to_warehouse_id, \
     transit_warehouse_id, status, transfer_date, reason, applicant_id, applicant_name, \
     approver_id, approval_instance_id, total_qty, total_amount, submitted_at, approved_at, \
     shipped_at, received_at, cancelled_at, cancel_reason, remark, created_at, updated_at, \
     create_by, update_by, deleted_at, overdue_alerted_at, overdue_alert_count";

/// 明细则列清单。
pub const ITEM_COLUMNS: &str = "id, transfer_id, line_no, material_id, from_location_id, \
     to_location_id, qty, shipped_qty, received_qty, unit_cost, amount, remark, created_at, \
     updated_at, create_by, update_by";

/// 列表排序：日期新的在前，`id` 兜底保证翻页稳定。
pub const TRANSFER_ORDER: &str = "ORDER BY transfer_date DESC, created_at DESC, id ASC";

/// 分页锚点（三张表都不参与数据权限，理由见文件头 `SCOPE-EXEMPT`）。
pub const TRANSFER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TRANSFER_TABLE);

/// 在途仓编码（W3a · V0139 的种子，`warehouse_type = 'IN_TRANSIT'`）。
pub const TRANSIT_WAREHOUSE_CODE: &str = "WH-TRANSIT";

/// 取号器里的单据类型（`biz_seq.seq_type`）。
pub const SEQ_TYPE: &str = "TK";

/// 单号前缀（`TK-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "TK";

/// 流水号补零宽度（6 位；超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

// ==================== 在途超期（LNK-F12 · V0171） ====================

/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 待审批。
pub const STATUS_PENDING: &str = "PENDING";
/// 审批通过。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 在途（已发货未收货）。
pub const STATUS_IN_TRANSIT: &str = "IN_TRANSIT";
/// 已完成（已收货；W3c 在这一步做库存双写）。
pub const STATUS_DONE: &str = "DONE";
/// 已取消。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 全部 6 态（顺序即生命周期顺序）。
pub const ALL_STATUSES: [&str; 6] = [
    STATUS_DRAFT,
    STATUS_PENDING,
    STATUS_APPROVED,
    STATUS_IN_TRANSIT,
    STATUS_DONE,
    STATUS_CANCELLED,
];

/// 允许的 6 条边（卡面 §二 ⑤ 的状态机，逐条对应；P10 起唯一真相在 `shared/state_machine`）。
pub const LEGAL_EDGES: [(&str, &str); 6] = state_machine::TRANSFER_EDGES;

/// 状态中文名（前端列表 / 报错文案共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_PENDING => "待审批",
        STATUS_APPROVED => "已审批",
        STATUS_IN_TRANSIT => "在途",
        STATUS_DONE => "已完成",
        STATUS_CANCELLED => "已取消",
        _ => "未知状态",
    }
}

// ==================== 状态机（纯逻辑） ====================

/// 一次流转要写的列（`true` = 必须写非空值）。
///
/// 这是 [`crate::business::erp::transfer_handler::change_transfer_status`] 的**唯一判据**：
/// 与 `ck_biz_transfer_state` 逐条对应，单测拿 [`state_check_holds`] 逐边验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionEffect {
    /// 目标状态。
    pub status: &'static str,
    /// 本次要写 `submitted_at`。
    pub submitted_at: bool,
    /// 本次要写 `approved_at`。
    pub approved_at: bool,
    /// 本次要写 `shipped_at`。
    pub shipped_at: bool,
    /// 本次要写 `received_at`。
    pub received_at: bool,
    /// 本次要写 `cancelled_at` + `cancel_reason`。
    pub cancelled: bool,
    /// 本次要回填 `transit_warehouse_id`（`IN_TRANSIT` 起必须非空）。
    pub needs_transit_warehouse: bool,
}

/// 这条边合不合法（**纯函数**，6 条边之外一律 `false`）。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES
        .iter()
        .any(|(start, end)| *start == from && *end == to)
}

/// 非法流转的中文文案（`409`，点名当前态 → 目标态 + 列出合法去向）。
pub fn transition_rejected_message(from: &str, to: &str) -> String {
    let allowed: Vec<&str> = LEGAL_EDGES
        .iter()
        .filter(|(start, _)| *start == from)
        .map(|(_, end)| *end)
        .collect();
    let next = if allowed.is_empty() {
        format!("{}（{from}）是终态，不能再流转", status_label(from))
    } else {
        format!(
            "{}（{from}）可以流转到：{}",
            status_label(from),
            allowed.join(" / ")
        )
    };
    format!(
        "非法状态流转：{} → {to}（{}）。{next}",
        status_label(from),
        status_label(to)
    )
}

/// 合法边 → 需要写的列；非法边 → `None`。
pub fn transition_effect(from: &str, to: &str) -> Option<TransitionEffect> {
    if !can_transition(from, to) {
        return None;
    }
    let mut effect = TransitionEffect {
        status: match to {
            STATUS_PENDING => STATUS_PENDING,
            STATUS_APPROVED => STATUS_APPROVED,
            STATUS_IN_TRANSIT => STATUS_IN_TRANSIT,
            STATUS_DONE => STATUS_DONE,
            STATUS_CANCELLED => STATUS_CANCELLED,
            _ => return None,
        },
        submitted_at: false,
        approved_at: false,
        shipped_at: false,
        received_at: false,
        cancelled: false,
        needs_transit_warehouse: false,
    };
    match (from, to) {
        (STATUS_DRAFT, STATUS_PENDING) => effect.submitted_at = true,
        (STATUS_PENDING, STATUS_APPROVED) => effect.approved_at = true,
        (STATUS_PENDING, STATUS_CANCELLED) | (STATUS_DRAFT, STATUS_CANCELLED) => {
            effect.cancelled = true;
        }
        (STATUS_APPROVED, STATUS_IN_TRANSIT) => {
            effect.shipped_at = true;
            effect.needs_transit_warehouse = true;
        }
        (STATUS_IN_TRANSIT, STATUS_DONE) => effect.received_at = true,
        _ => return None,
    }
    Some(effect)
}

/// 审批门闸的拦截文案（`409 + 65030`；handler 直接用它当 `ApiError.msg`）。
pub const FLOW_GATE_BLOCKED_MESSAGE: &str = "调拨单正在审批中，请等待审批结果（到「审批中心」处理该待办）。如需人工放行，请先撤回审批再操作";

/// 人工路径是否被**审批门闸**拦住（`409 + 65030`，纯函数）。
///
/// # 判据（三条同时成立才拦）
///
/// 1. 本次是 `PENDING → APPROVED`（唯一一条「人工能跳过审批」的边）；
/// 2. 单据挂着审批实例（`approval_instance_id` 非空）；
/// 3. 该实例仍是 `PENDING`（审批还没落终态）。
///
/// # 为什么需要它
///
/// 种了 `stock_transfer` 流程之后，`DRAFT → PENDING` 会自动发起审批单 —— 但只要不拦，
/// 谁都能再点一次「批准」把单据直接推到 `APPROVED`，审批单仍是 `PENDING`、
/// 待办永远挂在审批人身上（脏待办 + 单据状态与审批结论不一致）。
/// 码值与任务侧 D3a 的门闸相同（`65030`），前端按同一个业务码给提示。
///
/// # 什么时候**不**拦
///
/// - 没配流程（降级路径：`approval_instance_id` 为空）→ 人工推 `APPROVED` 是唯一出路，放行；
/// - 实例已落终态（`APPROVED` / `REJECTED` / …）→ 单据状态与审批结论不一致，
///   这时放行人工推是**修数据**，拦住只会让单据卡死；
/// - `PENDING → CANCELLED`（撤回）→ 必须放行，撤回正是「不想审了」的正规出口
///   （撤回会终止在跑的审批单，见 `transfer_handler::apply_transition`）。
pub fn flow_gate_blocked(
    from: &str,
    to: &str,
    approval_instance_id: Option<Uuid>,
    instance_status: Option<&str>,
) -> bool {
    if !(from == STATUS_PENDING && to == STATUS_APPROVED) {
        return false;
    }
    if approval_instance_id.is_none() {
        return false;
    }
    matches!(
        instance_status.map(str::trim),
        Some(status) if status.eq_ignore_ascii_case("PENDING")
    )
}

/// 一行的时间戳快照（用于**逐字验证** `ck_biz_transfer_state`）。
#[derive(Debug, Clone, Copy)]
pub struct StateTimestamps {
    /// `submitted_at` 是否非空。
    pub submitted_at: bool,
    /// `approved_at` 是否非空。
    pub approved_at: bool,
    /// `shipped_at` 是否非空。
    pub shipped_at: bool,
    /// `received_at` 是否非空。
    pub received_at: bool,
    /// `cancelled_at` 是否非空。
    pub cancelled_at: bool,
    /// `transit_warehouse_id` 是否非空。
    pub transit_warehouse_id: bool,
    /// `cancel_reason` 是否非空白。
    pub cancel_reason: bool,
}

/// `ck_biz_transfer_state` 的 Rust 复刻（逐字对应迁移里的 `CASE status WHEN … THEN …`）。
///
/// 用途：单测对每条合法边做「流转后这一行仍然满足库约束」的前置验证 ——
/// 提前在纯逻辑层抓住「少写一个时间戳」这类必然 500 的实现错误。
pub fn state_check_holds(status: &str, ts: &StateTimestamps) -> bool {
    match status {
        STATUS_DRAFT => {
            !ts.submitted_at
                && !ts.approved_at
                && !ts.shipped_at
                && !ts.received_at
                && !ts.cancelled_at
        }
        STATUS_PENDING => {
            ts.submitted_at
                && !ts.approved_at
                && !ts.shipped_at
                && !ts.received_at
                && !ts.cancelled_at
        }
        STATUS_APPROVED => {
            ts.submitted_at
                && ts.approved_at
                && !ts.shipped_at
                && !ts.received_at
                && !ts.cancelled_at
        }
        STATUS_IN_TRANSIT => {
            ts.submitted_at
                && ts.approved_at
                && ts.shipped_at
                && !ts.received_at
                && ts.transit_warehouse_id
                && !ts.cancelled_at
        }
        STATUS_DONE => {
            ts.submitted_at
                && ts.approved_at
                && ts.shipped_at
                && ts.received_at
                && ts.transit_warehouse_id
                && !ts.cancelled_at
        }
        STATUS_CANCELLED => ts.cancelled_at && ts.cancel_reason,
        _ => false,
    }
}

/// 把一次流转叠到「流转前的时间戳快照」上，得到流转后的快照（纯函数，单测用它验证库约束）。
pub fn apply_effect(before: StateTimestamps, effect: &TransitionEffect) -> StateTimestamps {
    StateTimestamps {
        submitted_at: before.submitted_at || effect.submitted_at,
        approved_at: before.approved_at || effect.approved_at,
        shipped_at: before.shipped_at || effect.shipped_at,
        received_at: before.received_at || effect.received_at,
        cancelled_at: before.cancelled_at || effect.cancelled,
        transit_warehouse_id: before.transit_warehouse_id || effect.needs_transit_warehouse,
        cancel_reason: before.cancel_reason || effect.cancelled,
    }
}

// ==================== 行结构 ====================

/// `biz_stock_transfer` 行（27 列，与 V0140 逐列对齐）。
#[derive(Debug, Clone)]
pub struct TransferRow {
    /// 主键。
    pub id: Uuid,
    /// 单号 `TK-yyyyMMdd-XXXXXX`（未删除内唯一）。
    pub transfer_no: String,
    /// 调出仓库。
    pub from_warehouse_id: Uuid,
    /// 调入仓库。
    pub to_warehouse_id: Uuid,
    /// 在途仓（`IN_TRANSIT` / `DONE` 非空）。
    pub transit_warehouse_id: Option<Uuid>,
    /// 状态（6 态之一）。
    pub status: String,
    /// 调拨日期。
    pub transfer_date: NaiveDate,
    /// 原因。
    pub reason: Option<String>,
    /// 申请人。
    pub applicant_id: Option<Uuid>,
    /// 申请人姓名快照。
    pub applicant_name: Option<String>,
    /// 审批人。
    pub approver_id: Option<Uuid>,
    /// 审批实例（提交审批后回写）。
    pub approval_instance_id: Option<Uuid>,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 金额合计。
    pub total_amount: Decimal,
    /// 提交审批时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 审批通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 发货时间。
    pub shipped_at: Option<DateTime<Utc>>,
    /// 收货时间。
    pub received_at: Option<DateTime<Utc>>,
    /// 取消时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 取消原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间。
    pub deleted_at: Option<DateTime<Utc>>,
    /// 最近一次「在途超期」告警时间（`NULL` = 从未提醒过；V0171）。
    pub overdue_alerted_at: Option<DateTime<Utc>>,
    /// 累计「在途超期」告警次数（V0171；同时是通知幂等键的序号段）。
    pub overdue_alert_count: i16,
}

impl TransferRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            transfer_no: row.get_typed("transfer_no")?,
            from_warehouse_id: row.get_typed("from_warehouse_id")?,
            to_warehouse_id: row.get_typed("to_warehouse_id")?,
            transit_warehouse_id: row.get_typed("transit_warehouse_id")?,
            status: row.get_typed("status")?,
            transfer_date: row.get_typed("transfer_date")?,
            reason: row.get_typed("reason")?,
            applicant_id: row.get_typed("applicant_id")?,
            applicant_name: row.get_typed("applicant_name")?,
            approver_id: row.get_typed("approver_id")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            total_qty: row.get_typed("total_qty")?,
            total_amount: row.get_typed("total_amount")?,
            submitted_at: row.get_typed("submitted_at")?,
            approved_at: row.get_typed("approved_at")?,
            shipped_at: row.get_typed("shipped_at")?,
            received_at: row.get_typed("received_at")?,
            cancelled_at: row.get_typed("cancelled_at")?,
            cancel_reason: row.get_typed("cancel_reason")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
            overdue_alerted_at: row.get_typed("overdue_alerted_at")?,
            overdue_alert_count: row.get_typed("overdue_alert_count")?,
        })
    }

    /// 是否「超期在途」（口径见 [`is_in_transit_overdue`]）。
    pub fn is_overdue(&self, now: DateTime<Utc>, threshold_days: i64) -> bool {
        is_in_transit_overdue(&self.status, self.shipped_at, now, threshold_days)
    }

    /// 已压天数（非在途 / 无发货时间 → `None`；用于列表「超期 N 天」）。
    pub fn elapsed_days(&self, now: DateTime<Utc>) -> Option<i64> {
        let shipped_at = self.shipped_at?;
        if self.status != STATUS_IN_TRANSIT {
            return None;
        }
        Some(overdue_elapsed_days(shipped_at, now))
    }

    /// 当前行的时间戳快照（喂给 [`state_check_holds`]）。
    pub fn timestamps(&self) -> StateTimestamps {
        StateTimestamps {
            submitted_at: self.submitted_at.is_some(),
            approved_at: self.approved_at.is_some(),
            shipped_at: self.shipped_at.is_some(),
            received_at: self.received_at.is_some(),
            cancelled_at: self.cancelled_at.is_some(),
            transit_warehouse_id: self.transit_warehouse_id.is_some(),
            cancel_reason: self
                .cancel_reason
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
        }
    }
}

/// `biz_stock_transfer_item` 行。
#[derive(Debug, Clone)]
pub struct TransferItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属调拨单。
    pub transfer_id: Uuid,
    /// 行号（`1..=N`）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 调出库位（`biz_inventory_location.id`）。
    pub from_location_id: Option<Uuid>,
    /// 调入库位（`biz_inventory_location.id`）。
    pub to_location_id: Option<Uuid>,
    /// 数量。
    pub qty: Decimal,
    /// 已发数量（W3c 写）。
    pub shipped_qty: Decimal,
    /// 已收数量（W3c 写）。
    pub received_qty: Decimal,
    /// 单位成本。
    pub unit_cost: Decimal,
    /// 金额。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 物料编码（连表带出，不属于本表列）。
    pub material_code: Option<String>,
    /// 物料名称（连表带出）。
    pub material_name: Option<String>,
}

impl TransferItemRow {
    /// 由查询行构造（查询里带了 `m.material_code` / `m.material_name`）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            transfer_id: row.get_typed("transfer_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            from_location_id: row.get_typed("from_location_id")?,
            to_location_id: row.get_typed("to_location_id")?,
            qty: row.get_typed("qty")?,
            shipped_qty: row.get_typed("shipped_qty")?,
            received_qty: row.get_typed("received_qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("amount")?,
            remark: row.get_typed("remark")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
        })
    }
}

/// `biz_stock_transfer_item_serial` 行（连表带出序列号文本）。
#[derive(Debug, Clone)]
pub struct TransferSerialRow {
    /// 子表主键。
    pub id: Uuid,
    /// 明细 id。
    pub item_id: Uuid,
    /// 序列号 id（`biz_material_serial.id`）。
    pub serial_id: Uuid,
    /// 序列号文本（`biz_material_serial.serial_no`，可能为空 = 历史脏行）。
    pub serial_no: Option<String>,
    /// 明细行号（便于分组）。
    pub line_no: i32,
}

impl TransferSerialRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            item_id: row.get_typed("item_id")?,
            serial_id: row.get_typed("serial_id")?,
            serial_no: row.get_typed("serial_no")?,
            line_no: row.get_typed("line_no")?,
        })
    }
}

// ==================== 取号（`TK-yyyyMMdd-000001`） ====================

/// 组装单号（`TK-{周期键}-{6 位流水}`）。
pub fn format_transfer_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// `biz_seq(seq_type='TK')` 那一行不存在时幂等补一行（V0137 只种了 `PO` / `SN`）。
pub(super) async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'TK', '库存调拨单号', 'TK', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '库存调拨单单号：TK-yyyyMMdd-000001，按天（CST）滚动' \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='TK')` 一个号，返回新号；`None` = 序列表没这行。
///
/// 三条分支（与 W2b 取号器同口径，**第三个分支是防重号的关键**）：
///
/// ```text
/// period_key = 本次 → 接着上一段（current_no + 1）
/// period_key < 本次 → 进入更新的一天：重置为 1
/// period_key > 本次 → 周期键回退（时钟回拨 / 手工改库）→ 绝不重置，继续累加
/// ```
pub(super) async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'TK' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 取一个调拨单号（周期键按北京时间）。
///
/// # 异常情况
///
/// 取号器行缺失且补行失败 / 数据库异常 → [`DbError`]（调用方整笔回滚，不消耗号）。
pub async fn next_transfer_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_transfer_no(&period_key, no)),
        // 补行成功还取不到 → 并发删行或者脏库，报错而不是返回重号
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='TK') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 查询：列表 / 详情 / 明细 / 序列号 ====================

/// 建单入参（已通过 DTO 校验，UUID 都已解析成 `Uuid`）。
#[derive(Debug, Clone)]
pub struct NewTransfer<'a> {
    /// 调出仓库。
    pub from_warehouse_id: Uuid,
    /// 调入仓库。
    pub to_warehouse_id: Uuid,
    /// 调拨日期（`None` = 今天 CST）。
    pub transfer_date: Option<NaiveDate>,
    /// 原因。
    pub reason: Option<&'a str>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 申请人。
    pub applicant_id: Option<Uuid>,
    /// 申请人姓名。
    pub applicant_name: Option<&'a str>,
}

/// 一条待落库的明细（`serial_ids` 已解析）。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 物料。
    pub material_id: Uuid,
    /// 调出库位。
    pub from_location_id: Option<Uuid>,
    /// 调入库位。
    pub to_location_id: Option<Uuid>,
    /// 数量（已按 4 位标度）。
    pub qty: Decimal,
    /// 随行序列号。
    pub serial_ids: Vec<Uuid>,
}
