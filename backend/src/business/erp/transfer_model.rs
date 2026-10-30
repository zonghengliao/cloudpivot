//! 调拨单数据层 + 6 态状态机（批次 D · W3b）。
//!
//! 依据：`backend/migrations/business/V0140__erp_stock_transfer.up.sql`（W3a 落地）。
//! **表结构以 `\d` 实测为准**，卡面的列清单不是权威：
//!
//! ```text
//! biz_stock_transfer            id, transfer_no, from_warehouse_id, to_warehouse_id,
//!                               transit_warehouse_id, status, transfer_date, reason,
//!                               applicant_id, applicant_name, approver_id, approval_instance_id,
//!                               total_qty, total_amount, submitted_at, approved_at, shipped_at,
//!                               received_at, cancelled_at, cancel_reason, remark,
//!                               created_at, updated_at, create_by, update_by, deleted_at
//! biz_stock_transfer_item       id, transfer_id, line_no, material_id, from_location_id,
//!                               to_location_id, qty, shipped_qty, received_qty, unit_cost,
//!                               amount, remark, created_at, updated_at, create_by, update_by
//! biz_stock_transfer_item_serial id, item_id, serial_id, created_at, create_by
//! ```
//!
//! # 三条「卡面没写、但库里有」的硬事实（照卡面写必然挂）
//!
//! 1. **`ck_biz_transfer_state` 把 6 个状态与时间戳绑死了**：
//!
//!    | 状态 | 必须非空 | 必须为空 | 额外 |
//!    |---|---|---|---|
//!    | `DRAFT` | — | 全部时间戳 | — |
//!    | `PENDING` | `submitted_at` | `approved_at`/`shipped_at`/`received_at`/`cancelled_at` | — |
//!    | `APPROVED` | `submitted_at`, `approved_at` | `shipped_at`/`received_at`/`cancelled_at` | — |
//!    | `IN_TRANSIT` | `submitted_at`, `approved_at`, `shipped_at` | `received_at`/`cancelled_at` | `transit_warehouse_id` 非空 |
//!    | `DONE` | 前四个 | `cancelled_at` | `transit_warehouse_id` 非空 |
//!    | `CANCELLED` | `cancelled_at` | — | `cancel_reason` 非空白 |
//!
//!    所以「状态流转」**不是只改一列**：每个目标状态的 `SET` 子句不同（见
//!    [`apply_status`] 里五条静态 SQL），审批驳回打回 `DRAFT` 还要把
//!    `submitted_at` / `approved_at` 清回 NULL。
//! 2. **`biz_stock_transfer_item` 没有 `deleted_at`**：改单只能「先删 serial 行、再删 item 行、
//!    再整批重插」（[`replace_items`]）。
//! 3. **`IN_TRANSIT` / `DONE` 必须有在途仓**：W3a 的 V0139 种了 `WH-TRANSIT`，
//!    发货时由 [`transit_warehouse_id`] 现查，查不到就 `400`（不能把状态改到一半）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 调拨单是仓储作业单据，V0140 的三张表都没有 dept_id / 归属人列，
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:transfer:* 权限码控制。
//! 列表分页仍走 scope::paginate_unscoped（闸①口径），不回落到框架 paginate_with_params。
//! SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置表，按 biz_type / flow_code 精确取 1 行，
//! 不参与数据范围，也不给列表分页。
//! ```
//!
//! # 事务边界
//!
//! 写函数一律吃 `&impl SqlExecutor`（`Db` 与 `Transaction` 都实现），
//! 由 [`crate::business::erp::transfer_handler`] 开事务：**建单 = 取号 + 单头 + 明细 + 序列号**、
//! **提交审批 = 状态写入 + 发起审批单 + 回写 `approval_instance_id`** 必须原子。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::serial_model::period_key_of;
use crate::business::shared::scope;

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
     create_by, update_by, deleted_at";

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

// ==================== 6 态常量（与 DDL 的 CHECK 逐字一致） ====================

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

/// 允许的 6 条边（卡面 §二 ⑤ 的状态机，逐条对应）。
pub const LEGAL_EDGES: [(&str, &str); 6] = [
    (STATUS_DRAFT, STATUS_PENDING),
    (STATUS_PENDING, STATUS_APPROVED),
    (STATUS_PENDING, STATUS_CANCELLED),
    (STATUS_APPROVED, STATUS_IN_TRANSIT),
    (STATUS_IN_TRANSIT, STATUS_DONE),
    (STATUS_DRAFT, STATUS_CANCELLED),
];

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
        })
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
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'TK', '库存调拨单号', 'TK', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '库存调拨单单号：TK-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'TK' AND deleted_at IS NULL)"
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
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
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

/// 分页查询条件（handler 把 DTO 归一化后传进来）。
#[derive(Debug, Clone, Default)]
pub struct TransferQuery {
    /// 状态。
    pub status: Option<String>,
    /// 调出仓库。
    pub from_warehouse_id: Option<Uuid>,
    /// 调入仓库。
    pub to_warehouse_id: Option<Uuid>,
    /// 单号前缀。
    pub transfer_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 分页查询（走 [`scope::paginate_unscoped`]，闸①口径）。
///
/// `transfer_no` 用前缀 `LIKE`（`TK-20261030-%`）：调拨单是低频单据、表很小，
/// 参数化 `LIKE` 走不了 btree 也不算问题；等量级上来再换 `~>=~` / `~<~`（W1b 的仓库树写法）。
pub async fn page(query: &TransferQuery) -> DbResult<(Vec<TransferRow>, i64)> {
    let mut sql =
        format!("SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = query.status.as_deref().map(str::trim)
        && !status.is_empty()
    {
        sql.push_str(" AND status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(id) = query.from_warehouse_id {
        sql.push_str(" AND from_warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = query.to_warehouse_id {
        sql.push_str(" AND to_warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(keyword) = query.transfer_no.as_deref().map(str::trim)
        && !keyword.is_empty()
    {
        sql.push_str(" AND transfer_no LIKE ?");
        params.push(Value::Text(format!("{keyword}%")));
    }

    let (page, page_size) = normalize_page(query.page, query.page_size);
    let result = scope::paginate_unscoped(
        TRANSFER_TARGET,
        &sql,
        params,
        Some(TRANSFER_ORDER),
        page,
        page_size,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(TransferRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按主键取一行（软删过滤恒带）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<TransferRow>> {
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TransferRow::from_row).transpose()
}

/// 按单号取一行（审批回调 / 排障用）。
pub async fn find_by_no(transfer_no: &str) -> DbResult<Option<TransferRow>> {
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} \
          WHERE transfer_no = ? AND deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(transfer_no.to_string())])
        .await?;
    rows.first().map(TransferRow::from_row).transpose()
}

/// 批量取两侧仓库名（列表页一次查完，避免 N+1）。
pub async fn warehouse_names(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let payload = unique
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // SCOPE-EXEMPT: 仓库是全局主数据（V0137 无 dept_id），只按主键批量取名称。
    let rows = db()
        .query_sql(
            "SELECT w.id, w.warehouse_name \
               FROM biz_warehouse w \
               JOIN unnest(string_to_array(?, E'\\n')) AS t(line) ON w.id = t.line::uuid"
                .to_string(),
            vec![Value::Text(payload)],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("warehouse_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 明细（连表带出物料编码 / 名称，按 `line_no` 升序）。
pub async fn items_of(transfer_id: Uuid) -> DbResult<Vec<TransferItemRow>> {
    let sql = format!(
        "SELECT i.id, i.transfer_id, i.line_no, i.material_id, i.from_location_id, \
                i.to_location_id, i.qty, i.shipped_qty, i.received_qty, i.unit_cost, i.amount, \
                i.remark, i.created_at, i.updated_at, i.create_by, i.update_by, \
                m.material_code, m.material_name \
           FROM {ITEM_TABLE} i \
           LEFT JOIN biz_material m ON m.id = i.material_id \
          WHERE i.transfer_id = ?::uuid \
          ORDER BY i.line_no ASC, i.id ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&transfer_id)])
        .await?;
    rows.iter().map(TransferItemRow::from_row).collect()
}

/// 序列号子表（连表带出号文本；按行号 + 号升序）。
pub async fn serials_of(transfer_id: Uuid) -> DbResult<Vec<TransferSerialRow>> {
    let sql = format!(
        "SELECT s.id, s.item_id, s.serial_id, i.line_no, ms.serial_no \
           FROM {ITEM_SERIAL_TABLE} s \
           JOIN {ITEM_TABLE} i ON i.id = s.item_id \
           LEFT JOIN biz_material_serial ms ON ms.id = s.serial_id \
          WHERE i.transfer_id = ?::uuid \
          ORDER BY i.line_no ASC, ms.serial_no ASC NULLS LAST, s.id ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&transfer_id)])
        .await?;
    rows.iter().map(TransferSerialRow::from_row).collect()
}

// ==================== 引用校验（写前面一次查完） ====================

/// 校验一个仓库存在（返回名称）；不存在 → `None`。
pub async fn warehouse_name(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT warehouse_name FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("warehouse_name"))
        .transpose()
}

/// 在途仓 id（`WH-TRANSIT`）；没种/被删 → `None`。
pub async fn transit_warehouse_id() -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_warehouse \
              WHERE warehouse_code = ? AND deleted_at IS NULL ORDER BY level ASC LIMIT 1",
            vec![Value::Text(TRANSIT_WAREHOUSE_CODE.to_string())],
        )
        .await?;
    rows.first().map(|row| row.get_typed("id")).transpose()
}

/// 库位库存行 `(id, warehouse_id)`：`from_location_id` / `to_location_id` 指向
/// `biz_inventory_location.id`（**不是** `biz_location.id` —— 实测外键如此）。
pub async fn inventory_location_ref(id: Uuid) -> DbResult<Option<(Uuid, Uuid)>> {
    let rows = db()
        .find_with_params(
            "SELECT id, warehouse_id FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("id")?, row.get_typed("warehouse_id")?))),
    }
}

/// 物料是否存在（返回编码 / 名称）。
pub async fn material_ref(id: Uuid) -> DbResult<Option<(String, String)>> {
    let rows = db()
        .find_with_params(
            "SELECT material_code, material_name FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed("material_code")?,
            row.get_typed("material_name")?,
        ))),
    }
}

/// 序列号「可用且属于该物料」的计数（**一次查完所有行**，参数是 `itemId|serialId|materialId` 的文本载荷）。
///
/// 判据三条同时成立才算有效：`material_id` 匹配、`status = 'IN_STOCK'`、未软删。
/// 调用方拿返回值与自己提交的序列号总数比对 —— 不等就是「有不存在的 / 不属于该物料的 /
/// 已经出库的」序列号，必须 `400` 而不是让它落库成为脏账。
///
/// # 为什么用文本载荷而不是数组参数
///
/// `Value::Text("{a,b}")` 绑 `?::text[]` 会 `error serializing parameter`（W2b 实测），
/// 文本载荷 + `string_to_array` / `split_part` 是唯一稳定的形态。
pub async fn count_valid_serials(payload: &str) -> DbResult<i64> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) \
               FROM unnest(string_to_array(?, E'\\n')) AS t(line) \
               JOIN biz_material_serial s \
                 ON s.id = split_part(t.line, '|', 2)::uuid \
                AND s.material_id = split_part(t.line, '|', 3)::uuid \
              WHERE s.status = 'IN_STOCK' AND s.deleted_at IS NULL",
            vec![Value::Text(payload.to_string())],
        )
        .await?;
    Ok(count)
}

// ==================== 写：建单 / 改单 / 删单 ====================

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

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 序列号**，调用方负责事务边界。
///
/// # 异常情况
///
/// 数据库异常（唯一键 / CHECK / 外键）→ [`DbError`]（handler 翻译成 409 / 400 / 500）。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewTransfer<'_>,
    items: &[NewItem],
    total_qty: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let transfer_no = next_transfer_no(ex, now).await?;
    let id = Uuid::now_v7();
    // 日期用「两条 SQL 片段」而不是 `COALESCE(?::date, CURRENT_DATE)`：
    // `Value::Null` 绑到 `?::date` 会因为类型推断不出来而在驱动侧编码失败
    // （技能 §3 记的 uuid 版同一个坑）。缺席就写成静态的 `CURRENT_DATE`。
    let (date_sql, date_param) = match new.transfer_date {
        Some(date) => ("?", vec![Value::from(date)]),
        None => ("CURRENT_DATE", Vec::new()),
    };
    let operator_text = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    let mut params = vec![
        Value::from(&id),
        Value::Text(transfer_no),
        Value::from(&new.from_warehouse_id),
        Value::from(&new.to_warehouse_id),
    ];
    params.extend(date_param);
    params.extend(vec![
        Value::Text(new.reason.unwrap_or_default().to_string()),
        Value::Text(
            new.applicant_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        Value::Text(new.applicant_name.unwrap_or_default().to_string()),
        Value::Text(total_qty.to_string()),
        Value::Text(new.remark.unwrap_or_default().to_string()),
        Value::from(now),
        Value::from(now),
        operator_text.clone(),
        operator_text,
    ]);
    ex.exec_sql(
        format!(
            "INSERT INTO {TRANSFER_TABLE} \
                (id, transfer_no, from_warehouse_id, to_warehouse_id, status, \
                 transfer_date, reason, applicant_id, applicant_name, total_qty, total_amount, \
                 remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?::uuid, ?::uuid, 'DRAFT', \
                 {date_sql}, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                 NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,''), ?, ?, \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        params,
    )
    .await?;
    insert_items(ex, id, items, operator).await?;
    Ok(id)
}

/// 整批插入明细 + 序列号（建单 / 改单共用）。
async fn insert_items(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
    items: &[NewItem],
    operator: Option<Uuid>,
) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let item_id: Uuid = ex
            .query_sql(
                format!(
                    "INSERT INTO {ITEM_TABLE} \
                        (id, transfer_id, line_no, material_id, from_location_id, to_location_id, \
                         qty, shipped_qty, received_qty, unit_cost, amount, create_by, update_by) \
                     VALUES (?, ?::uuid, ?, ?::uuid, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::numeric, 0, 0, 0, 0, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                     RETURNING id"
                ),
                vec![
                    Value::from(Uuid::now_v7()),
                    Value::from(&transfer_id),
                    Value::from(line_no),
                    Value::from(&item.material_id),
                    Value::Text(
                        item.from_location_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    Value::Text(
                        item.to_location_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    Value::Text(item.qty.to_string()),
                    Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                    Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await?
            .first()
            .map(|row| row.get_typed::<Uuid>("id"))
            .transpose()?
            .ok_or_else(|| DbError::NotFound)?;

        if item.serial_ids.is_empty() {
            continue;
        }
        // 一行一个序列号载荷：`itemId|serialId`，SQL 侧拆（不走数组参数，见 count_valid_serials）
        let payload = item
            .serial_ids
            .iter()
            .map(|serial_id| format!("{item_id}|{serial_id}"))
            .collect::<Vec<_>>()
            .join("\n");
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_SERIAL_TABLE} (id, item_id, serial_id, create_by) \
                 SELECT gen_random_uuid(), split_part(t.line, '|', 1)::uuid, \
                        split_part(t.line, '|', 2)::uuid, NULLIF(?::text,'')::uuid \
                   FROM unnest(string_to_array(?, E'\\n')) AS t(line)"
            ),
            vec![
                Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(payload),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 改单：**只有 DRAFT**（调用方先判状态），明细整体重建。
///
/// 顺序必须是「序列号子表 → 明细行 → 重插」：`_item_serial` 对 `_item` 是硬外键，
/// 先删父行会撞外键；而明细表没有 `deleted_at`，改单只能物理删。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewTransfer<'_>,
    items: &[NewItem],
    total_qty: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let (date_sql, date_param) = match new.transfer_date {
        Some(date) => ("?", vec![Value::from(date)]),
        None => ("transfer_date", Vec::new()),
    };
    let operator_text = Value::Text(operator.map(|uuid| uuid.to_string()).unwrap_or_default());
    let mut params = vec![
        Value::from(&new.from_warehouse_id),
        Value::from(&new.to_warehouse_id),
    ];
    params.extend(date_param);
    params.extend(vec![
        Value::Text(new.reason.unwrap_or_default().to_string()),
        Value::Text(new.remark.unwrap_or_default().to_string()),
        Value::Text(total_qty.to_string()),
        Value::from(now),
        operator_text,
        Value::from(&id),
    ]);
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET from_warehouse_id = ?::uuid, to_warehouse_id = ?::uuid, \
                        transfer_date = {date_sql}, \
                        reason = NULLIF(?::text,''), \
                        remark = NULLIF(?::text,''), \
                        total_qty = NULLIF(?::text,'')::numeric, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            ),
            params,
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    replace_items(ex, id, items, operator).await?;
    Ok(affected)
}

/// 明细整体重建（改单用）。
async fn replace_items(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
    items: &[NewItem],
    operator: Option<Uuid>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "DELETE FROM {ITEM_SERIAL_TABLE} \
              WHERE item_id IN (SELECT id FROM {ITEM_TABLE} WHERE transfer_id = ?::uuid)"
        ),
        vec![Value::from(&transfer_id)],
    )
    .await?;
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE transfer_id = ?::uuid"),
        vec![Value::from(&transfer_id)],
    )
    .await?;
    insert_items(ex, transfer_id, items, operator).await
}

/// 软删（调用方先判 `DRAFT` / `CANCELLED`）；返回受影响行数。
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TRANSFER_TABLE} \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status IN ('DRAFT','CANCELLED')"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|uuid| uuid.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 写：状态流转 ====================

/// 状态流转落库（**CAS：`WHERE status = 期望的当前态`**）。
///
/// 五个目标各有自己的 `SET` 子句（`ck_biz_transfer_state` 绑死了时间戳），
/// 并且都带 `AND status = <from>` —— 并发下两个人同时推同一张单，只有一个 `affected = 1`，
/// 另一个拿到 409 而不是「两笔都写进去」。
///
/// # 参数
///
/// - `effect`：[`transition_effect`] 的产物（含目标状态与要写的列）；
/// - `from`：调用方读到的当前状态（CAS 期望值）；
/// - `now`：Rust 侧算好的时间点（与变更日志 / 响应里的时间同源）；
/// - `cancel_reason`：仅 `→ CANCELLED` 用（`ck_biz_transfer_state` 要求非空白）；
/// - `transit_warehouse_id`：仅 `→ IN_TRANSIT` 用（该状态起必须非空）；
/// - `approver`：仅 `→ APPROVED` 用（写 `approver_id`）；
/// - `approval_instance_id`：仅 `DRAFT → PENDING` 用（发起审批后回写）。
///
/// # 异常情况
///
/// 数据库异常 / CHECK 冲突 → [`DbError`]；`affected = 0` 表示状态已被别人改走。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    effect: &TransitionEffect,
    cancel_reason: Option<&str>,
    transit_warehouse_id: Option<Uuid>,
    approver: Option<Uuid>,
    approval_instance_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let uuid_text =
        |value: Option<Uuid>| Value::Text(value.map(|id| id.to_string()).unwrap_or_default());
    let (sql, params) = match effect.status {
        STATUS_PENDING => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'PENDING', submitted_at = ?, updated_at = ?, \
                        approval_instance_id = NULLIF(?::text,'')::uuid, \
                        applicant_id = COALESCE(applicant_id, NULLIF(?::text,'')::uuid), \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                uuid_text(approval_instance_id),
                uuid_text(operator),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_APPROVED => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'APPROVED', approved_at = ?, approver_id = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                uuid_text(approver.or(operator)),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CANCELLED => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'CANCELLED', cancelled_at = ?, cancel_reason = ?, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::Text(cancel_reason.unwrap_or_default().to_string()),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_IN_TRANSIT => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'IN_TRANSIT', shipped_at = ?, \
                        transit_warehouse_id = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                uuid_text(transit_warehouse_id),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_DONE => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'DONE', received_at = ?, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 5 个可写目标里（DRAFT 只能由建单产生）"
            )));
        }
    };
    ex.exec_sql(sql, params).await
}

/// 审批结论 → 调拨单（**审批回调专用**，`PENDING` 才动）。
///
/// | 审批结论 | 调拨单 | 依据 |
/// |---|---|---|
/// | `APPROVED` | `PENDING → APPROVED` | 审批通过 = 可发货 |
/// | `REJECTED` | `PENDING → DRAFT`（打回） | `submitted_at` / `approved_at` **必须清回 NULL**（`ck_biz_transfer_state`），审批意见写 `reason` |
///
/// 返回受影响行数（0 = 当前不在 `PENDING`，重复回调 / 已被人工撤回 → 静默跳过）。
pub async fn settle_approval(
    ex: &impl SqlExecutor,
    id: Uuid,
    approved: bool,
    opinion: Option<&str>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    if approved {
        return ex
            .exec_sql(
                format!(
                    "UPDATE {TRANSFER_TABLE} \
                        SET status = 'APPROVED', approved_at = ?, updated_at = ? \
                      WHERE id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL"
                ),
                vec![Value::from(now), Value::from(now), Value::from(&id)],
            )
            .await;
    }
    // 打回：DRAFT 分支要求全部时间戳为 NULL，所以四个时间列 + 取消列一起清空，
    // 审批意见覆盖 reason（列宽 255，超长截断），审批实例 id 清空以便重新提交。
    let reason = opinion.map(|text| text.trim().chars().take(255).collect::<String>());
    ex.exec_sql(
        format!(
            "UPDATE {TRANSFER_TABLE} \
                SET status = 'DRAFT', submitted_at = NULL, approved_at = NULL, \
                    shipped_at = NULL, received_at = NULL, cancelled_at = NULL, \
                    cancel_reason = NULL, approval_instance_id = NULL, \
                    reason = COALESCE(NULLIF(?::text,''), reason), updated_at = ? \
              WHERE id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(reason.unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 审批接入：找已发布的调拨流程 ====================

/// 已发布的调拨审批流程（引擎发起参数的最小集合）。
#[derive(Debug, Clone)]
pub struct PublishedFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`STOCK_TRANSFER`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的调拨流程」。
///
/// 卡面口径：`flow_code = 'stock_transfer'` **或** `biz_type = 'STOCK_TRANSFER'`。
/// 排序让 `biz_type` 精确命中的排在前面（同一个库里两种都配时取业务类型那条），
/// 同组内按 `is_default` / 版本号 / 更新时间取最新。
///
/// `None` = **没配流程**（不是错误）：提交审批照样允许，`PENDING → APPROVED` 由人工推。
pub async fn published_transfer_flow() -> DbResult<Option<PublishedFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'STOCK_TRANSFER' OR lower(d.flow_code) = 'stock_transfer') \
              ORDER BY (d.biz_type = 'STOCK_TRANSFER') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PublishedFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 单号格式：`TK-yyyyMMdd-000001`（卡面 §五 ①）。
    #[test]
    fn transfer_no_matches_card_format() {
        assert_eq!(format_transfer_no("20261030", 1), "TK-20261030-000001");
        assert_eq!(format_transfer_no("20261030", 42), "TK-20261030-000042");
        // 超过 6 位不截断：宁可长一位也不撞号
        assert_eq!(
            format_transfer_no("20261030", 1_000_000),
            "TK-20261030-1000000"
        );
    }

    /// ② 状态机：6 条合法边全部放行。
    #[test]
    fn status_machine_allows_six_card_edges() {
        for (from, to) in LEGAL_EDGES {
            assert!(can_transition(from, to), "{from} → {to} 必须放行");
        }
        assert_eq!(LEGAL_EDGES.len(), 6);
    }

    /// ③ 状态机：跳级 / 回退 / 原地 / 终态再流转全部拒绝（卡面 §五 ⑧⑬）。
    #[test]
    fn status_machine_rejects_jumps_and_terminal() {
        assert!(
            !can_transition(STATUS_DRAFT, STATUS_DONE),
            "DRAFT 不能直接 DONE"
        );
        assert!(
            !can_transition(STATUS_DRAFT, STATUS_APPROVED),
            "DRAFT 不能跳审批"
        );
        assert!(!can_transition(STATUS_PENDING, STATUS_IN_TRANSIT));
        assert!(
            !can_transition(STATUS_APPROVED, STATUS_DONE),
            "必须经过在途"
        );
        assert!(
            !can_transition(STATUS_DONE, STATUS_CANCELLED),
            "DONE 是终态"
        );
        assert!(
            !can_transition(STATUS_CANCELLED, STATUS_DRAFT),
            "CANCELLED 不可复活"
        );
        assert!(
            !can_transition(STATUS_PENDING, STATUS_PENDING),
            "原地流转也算非法"
        );
        // 非法边的文案要点名「当前态的可选去向」
        let message = transition_rejected_message(STATUS_DRAFT, STATUS_DONE);
        assert!(message.contains("PENDING"), "{message}");
        assert!(message.contains("CANCELLED"), "{message}");
        let terminal = transition_rejected_message(STATUS_DONE, STATUS_DRAFT);
        assert!(terminal.contains("终态"), "{terminal}");
    }

    /// ④ 每条合法边写入后仍然满足 `ck_biz_transfer_state`（逐边对照，含在途仓）。
    #[test]
    fn every_legal_edge_keeps_db_state_check_holds() {
        // 流转前的快照就按「当前状态应有的样子」造
        let before_of = |status: &str| StateTimestamps {
            submitted_at: !matches!(status, STATUS_DRAFT),
            approved_at: matches!(status, STATUS_APPROVED | STATUS_IN_TRANSIT | STATUS_DONE),
            shipped_at: matches!(status, STATUS_IN_TRANSIT | STATUS_DONE),
            received_at: matches!(status, STATUS_DONE),
            cancelled_at: matches!(status, STATUS_CANCELLED),
            transit_warehouse_id: matches!(status, STATUS_IN_TRANSIT | STATUS_DONE),
            cancel_reason: matches!(status, STATUS_CANCELLED),
        };
        for (from, to) in LEGAL_EDGES {
            let before = before_of(from);
            assert!(
                state_check_holds(from, &before),
                "夹具自身不满足库约束：{from}"
            );
            let effect = transition_effect(from, to).expect("合法边必须有 effect");
            let after = apply_effect(before, &effect);
            assert!(
                state_check_holds(to, &after),
                "{from} → {to} 写入后会违反 ck_biz_transfer_state：{after:?}"
            );
        }
    }

    /// ⑤ 反向对照：漏写时间戳 / 漏回填在途仓会被 `state_check_holds` 抓住（证明校验不是恒真）。
    #[test]
    fn state_check_catches_missing_timestamps() {
        // IN_TRANSIT 少了 shipped_at
        let broken = StateTimestamps {
            submitted_at: true,
            approved_at: true,
            shipped_at: false,
            received_at: false,
            cancelled_at: false,
            transit_warehouse_id: true,
            cancel_reason: false,
        };
        assert!(!state_check_holds(STATUS_IN_TRANSIT, &broken));
        // IN_TRANSIT 少了在途仓
        let no_transit = StateTimestamps {
            shipped_at: true,
            transit_warehouse_id: false,
            ..broken
        };
        assert!(!state_check_holds(STATUS_IN_TRANSIT, &no_transit));
        // CANCELLED 没有原因
        let no_reason = StateTimestamps {
            cancelled_at: true,
            cancel_reason: false,
            ..no_transit
        };
        assert!(!state_check_holds(STATUS_CANCELLED, &no_reason));
        // 审批驳回打回 DRAFT：留着 submitted_at 就违反约束
        let still_submitted = StateTimestamps {
            submitted_at: true,
            ..no_reason
        };
        assert!(!state_check_holds(STATUS_DRAFT, &still_submitted));
    }

    /// ⑥ 状态中文名覆盖 6 态 + 未知值兜底（列表 `status_label` 不能 panic）。
    #[test]
    fn status_labels_cover_all_states() {
        for status in ALL_STATUSES {
            assert_ne!(status_label(status), "未知状态", "{status} 没有中文名");
        }
        assert_eq!(status_label("WHATEVER"), "未知状态");
    }
}
