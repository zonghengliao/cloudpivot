//! 调拨单接口契约（批次 D · W3b）：分页筛选 / 建单 / 改单 / 状态流转 / 详情 / 序列号分组的出入参。
//!
//! 与 [`crate::business::erp::dto`] / [`crate::business::erp::serial_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 时间戳 `DateTime<Utc>`（JSON RFC3339）、日期 `NaiveDate`（`YYYY-MM-DD`）、
//! 数量金额一律 `Decimal`（**JSON 里是字符串**，与 W1b 定下的口径一致）、
//! 枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 表结构依据（**开工先 `\d` 实测**，不是照卡面猜）
//!
//! `backend/migrations/business/V0140__erp_stock_transfer.up.sql` 建的三张表实测列：
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
//! 卡面没写、但库里有且**会改变实现**的两处（已落地，见 [`crate::business::erp::transfer_model`]）：
//!
//! 1. `ck_biz_transfer_state`：6 个状态各自绑死了「哪几个时间戳必须非空」。
//!    `IN_TRANSIT` / `DONE` 还额外要求 `transit_warehouse_id IS NOT NULL`，
//!    `CANCELLED` 要求 `cancel_reason` 非空白 —— 所以状态流转**不是只改 status 一列**。
//! 2. `biz_stock_transfer_item` 没有 `deleted_at`：改单只能「先删后插」重建明细行。
//!
//! # 与卡面的差异（逐条）
//!
//! 1. 卡面 §二 ③ 的建单 body 只有 `from_warehouse_id / to_warehouse_id / transfer_date? /
//!    reason? / items[]`：本实现多接受一个可选 `remark`（库里确有该列），
//!    不传时按 NULL 落库，不影响卡面契约。
//! 2. 卡面 §二 ④ 的改单没有单独 body 定义，直接复用建单 body（`UpdateTransferReq` 是别名）：
//!    改单语义是「整单覆盖」，前端不必区分两套形状。
//! 3. 卡面 §五 ⑰「权限：无码 → 403」在单测里落成**权限码表与迁移逐字一致**的断言
//!    （真 403 由 `scripts/verify/w3b_transfer_api.sh` 用只授 `:list` 的账号打 `POST` 验）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::transfer_model::{
    ALL_STATUSES, STATUS_CANCELLED, TransferItemRow, TransferRow, TransferSerialRow,
};

// ==================== 口径常量 ====================

/// 默认每页条数（与 `erp::dto` 一致）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数硬上限（调拨单是单据头，条数比明细少，200 足够）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// `reason` / `cancel_reason` 的最大字符数（列宽 `VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;

/// 单张调拨单的明细行上限（单据头 + 明细一次提交，防误传超大数组）。
pub const MAX_LINES: usize = 200;

/// 权限码（与 `backend/migrations/business/V0144__erp_transfer_permission.up.sql` 逐字一致）。
///
/// 6 码制：`:status` 独立于 `:update` —— 状态流转会触发审批发起 / 库存双写，
/// 与「改几行明细」不是同一类授权。
pub const TRANSFER_PERMISSIONS: [(&str, &str); 6] = [
    ("business:erp:transfer:list", "调拨单列表"),
    ("business:erp:transfer:detail", "调拨单详情"),
    ("business:erp:transfer:create", "新建调拨单"),
    ("business:erp:transfer:update", "编辑调拨单"),
    ("business:erp:transfer:delete", "删除调拨单"),
    ("business:erp:transfer:status", "调拨单状态流转"),
];

// ==================== ① 分页筛选 ====================

/// 调拨单分页筛选（卡面 §二 ①）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TransferFilter {
    /// 状态（`DRAFT` / `PENDING` / `APPROVED` / `IN_TRANSIT` / `DONE` / `CANCELLED`）。
    pub status: Option<String>,
    /// 调出仓库 id。
    pub from_warehouse_id: Option<String>,
    /// 调入仓库 id。
    pub to_warehouse_id: Option<String>,
    /// 调拨单号（前缀模糊，扫半截号也能搜到）。
    pub transfer_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for TransferFilter {
    fn default() -> Self {
        Self {
            status: None,
            from_warehouse_id: None,
            to_warehouse_id: None,
            transfer_no: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl TransferFilter {
    /// 归一化并校验（非法 UUID / 非法 status → `400`，**绝不静默忽略条件**）。
    ///
    /// 非法 UUID 静默跳过会把「按仓库筛」变成「返回全部单据」，属于静默错数据；
    /// 非法 status 同理（拼错一个字母就返回空列表最容易被当成「没数据」）。
    pub fn validated(self) -> std::result::Result<Self, ApiError> {
        check_optional_uuid(self.from_warehouse_id.as_deref(), "from_warehouse_id")?;
        check_optional_uuid(self.to_warehouse_id.as_deref(), "to_warehouse_id")?;
        if let Some(status) = trimmed(self.status.as_deref())
            && !ALL_STATUSES.contains(&status)
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!(
                    "status 只能是 {} 之一（收到 {status}）",
                    ALL_STATUSES.join(" / ")
                ),
            ));
        }
        if let Some(keyword) = trimmed(self.transfer_no.as_deref())
            && keyword.chars().count() > 32
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                "transfer_no 最长 32 个字符",
            ));
        }
        Ok(self)
    }
}

// ==================== ② 建单 / 改单 ====================

/// 一条调拨明细。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TransferItemReq {
    /// 物料 id（必填，`biz_material.id`）。
    pub material_id: String,
    /// 调出库位（可空，指向 `biz_inventory_location.id`；必须属于调出仓库）。
    #[serde(default)]
    pub from_location_id: Option<String>,
    /// 调入库位（可空，指向 `biz_inventory_location.id`；必须属于调入仓库）。
    #[serde(default)]
    pub to_location_id: Option<String>,
    /// 调拨数量（`> 0`，最多 4 位小数，与 `NUMERIC(18,4)` 同标度）。
    pub qty: Decimal,
    /// 本次随行的序列号 id（一物一码物料用；按明细挂在 `_item_serial` 子表）。
    #[serde(default)]
    pub serial_ids: Vec<String>,
}

impl TransferItemReq {
    /// 单行校验（**不碰数据库**）：物料 UUID / 数量 / 库位 UUID / 序列号 UUID。
    pub fn validate(&self, line_no: usize) -> std::result::Result<(), ApiError> {
        require_uuid(&self.material_id, &format!("items[{line_no}].material_id"))?;
        if self.qty <= Decimal::ZERO {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("items[{line_no}].qty 必须 > 0（收到 {}）", self.qty),
            ));
        }
        // 数量标度：`NUMERIC(18,4)` 会把第 5 位四舍五入 —— 静默改数量比报错更坏。
        if self.qty.normalize().scale() > 4 {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("items[{line_no}].qty 最多 4 位小数（收到 {}）", self.qty),
            ));
        }
        check_optional_uuid(
            self.from_location_id.as_deref(),
            &format!("items[{line_no}].from_location_id"),
        )?;
        check_optional_uuid(
            self.to_location_id.as_deref(),
            &format!("items[{line_no}].to_location_id"),
        )?;
        for (index, serial_id) in self.serial_ids.iter().enumerate() {
            require_uuid(serial_id, &format!("items[{line_no}].serial_ids[{index}]"))?;
        }
        Ok(())
    }

    /// 本行数量（`Decimal`，落库时按 4 位标度写）。
    pub fn qty(self) -> Decimal {
        self.qty.round_dp(4)
    }
}

/// 建单 / 改单入参（改单复用同一形状：整单覆盖）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateTransferReq {
    /// 调出仓库 id。
    pub from_warehouse_id: String,
    /// 调入仓库 id。
    pub to_warehouse_id: String,
    /// 调拨日期（可空，缺省 = 今天 CST）。
    #[serde(default)]
    pub transfer_date: Option<NaiveDate>,
    /// 调拨原因 / 说明（≤255）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 备注（≤1000）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（1..=200 行）。
    #[serde(default)]
    pub items: Vec<TransferItemReq>,
}

/// 改单入参：与建单同形状（卡面 §二 ④ 未单列 body）。
pub type UpdateTransferReq = CreateTransferReq;

impl CreateTransferReq {
    /// 整单校验（**不碰数据库**，顺序即错误优先级）。
    ///
    /// 卡面 §五 的 ②③④ 三条 400 全在这里：
    /// ② 调出 = 调入（DB 也有 `ck_transfer_diff_wh`，这里先给可读文案）；
    /// ③ 明细数量 ≤ 0；④ 明细为空。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_uuid(&self.from_warehouse_id, "from_warehouse_id")?;
        require_uuid(&self.to_warehouse_id, "to_warehouse_id")?;
        if self.from_warehouse_id.trim() == self.to_warehouse_id.trim() {
            return Err(ApiError::new(
                400,
                error_code_param(),
                "from_warehouse_id 与 to_warehouse_id 不能相同：调拨是「从一个仓到另一个仓」，\
                 同仓变动请走库存调整而不是调拨单",
            ));
        }
        if self.items.is_empty() {
            return Err(ApiError::new(
                400,
                error_code_param(),
                "items 不能为空：至少一条调拨明细",
            ));
        }
        if self.items.len() > MAX_LINES {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("明细最多 {MAX_LINES} 行（收到 {}）", self.items.len()),
            ));
        }
        if let Some(reason) = trimmed(self.reason.as_deref())
            && reason.chars().count() > MAX_REASON_LEN
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("reason 最长 {MAX_REASON_LEN} 个字符"),
            ));
        }
        if let Some(remark) = trimmed(self.remark.as_deref())
            && remark.chars().count() > 1000
        {
            return Err(ApiError::new(
                400,
                error_code_param(),
                "remark 最长 1000 个字符",
            ));
        }
        for (index, item) in self.items.iter().enumerate() {
            item.validate(index)?;
        }
        // 同一个序列号不能出现在两行里（同一行内重复由 `uk_transfer_item_serial` 兜，
        // 跨行重复库拦不住 —— 但语义上「一件货分两行调走」必然是录入错误）。
        let mut seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for (index, item) in self.items.iter().enumerate() {
            for serial_id in &item.serial_ids {
                if let Some(first) = seen.insert(serial_id.trim(), index) {
                    return Err(ApiError::new(
                        400,
                        error_code_param(),
                        format!(
                            "序列号 {serial_id} 同时出现在 items[{first}] 与 items[{index}]：\
                             一个序列号只能随一行调拨"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 全单数量合计（落 `total_qty`，`NUMERIC(18,4)`）。
    pub fn total_qty(&self) -> Decimal {
        self.items
            .iter()
            .map(|item| item.qty.round_dp(4))
            .sum::<Decimal>()
    }
}

// ==================== ③ 状态流转 ====================

/// 状态流转入参（卡面 §二 ⑤）。
#[derive(Debug, Clone, Deserialize)]
pub struct TransferStatusReq {
    /// 目标状态。
    pub status: String,
    /// 原因（`→ CANCELLED` 时**必填**：库里 `ck_biz_transfer_state` 要求
    /// `cancel_reason` 非空白；其余目标忽略该字段）。
    #[serde(default)]
    pub reason: Option<String>,
}

impl TransferStatusReq {
    /// 归一化目标状态（非法值 → `400`，非法边 → 由数据层的状态机给 `409`）。
    pub fn target_status(&self) -> std::result::Result<&'static str, ApiError> {
        let raw = self.status.trim().to_ascii_uppercase();
        ALL_STATUSES
            .iter()
            .copied()
            .find(|status| *status == raw)
            .ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code_param(),
                    format!(
                        "status 只能是 {} 之一（收到 {}）",
                        ALL_STATUSES.join(" / "),
                        self.status
                    ),
                )
            })
    }

    /// 取消原因（`→ CANCELLED` 必填、≤255；其它目标返回 `None`）。
    pub fn cancel_reason(&self) -> std::result::Result<Option<String>, ApiError> {
        if self.target_status()? != STATUS_CANCELLED {
            return Ok(None);
        }
        let reason = trimmed(self.reason.as_deref()).ok_or_else(|| {
            ApiError::new(
                400,
                error_code_param(),
                "取消调拨单必须给 reason（库里 ck_biz_transfer_state 要求 cancel_reason 非空白）",
            )
        })?;
        if reason.chars().count() > MAX_REASON_LEN {
            return Err(ApiError::new(
                400,
                error_code_param(),
                format!("reason 最长 {MAX_REASON_LEN} 个字符"),
            ));
        }
        Ok(Some(reason.to_string()))
    }
}

// ==================== ④ 出参 ====================

/// 调拨单列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct TransferRes {
    /// 主键。
    pub id: String,
    /// 调拨单号（`TK-yyyyMMdd-000001`）。
    pub transfer_no: String,
    /// 调出仓库 id。
    pub from_warehouse_id: String,
    /// 调出仓库名称（连表带出，前端列表直接显示）。
    pub from_warehouse_name: Option<String>,
    /// 调入仓库 id。
    pub to_warehouse_id: String,
    /// 调入仓库名称。
    pub to_warehouse_name: Option<String>,
    /// 在途仓 id（`IN_TRANSIT` / `DONE` 必非空）。
    pub transit_warehouse_id: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 调拨日期。
    pub transfer_date: NaiveDate,
    /// 原因。
    pub reason: Option<String>,
    /// 申请人 id。
    pub applicant_id: Option<String>,
    /// 申请人姓名。
    pub applicant_name: Option<String>,
    /// 审批实例 id（提交审批后回写）。
    pub approval_instance_id: Option<String>,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 金额合计（本卡不维护成本，恒 0）。
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
}

impl TransferRes {
    /// 由数据层行 + 两侧仓库名组装（`(from_name, to_name)` 由详情查询带出）。
    pub fn of(row: &TransferRow, from_name: Option<String>, to_name: Option<String>) -> Self {
        Self {
            id: row.id.to_string(),
            transfer_no: row.transfer_no.clone(),
            from_warehouse_id: row.from_warehouse_id.to_string(),
            from_warehouse_name: from_name,
            to_warehouse_id: row.to_warehouse_id.to_string(),
            to_warehouse_name: to_name,
            transit_warehouse_id: row.transit_warehouse_id.map(|id| id.to_string()),
            status: row.status.clone(),
            status_label: crate::business::erp::transfer_model::status_label(&row.status)
                .to_string(),
            transfer_date: row.transfer_date,
            reason: row.reason.clone(),
            applicant_id: row.applicant_id.map(|id| id.to_string()),
            applicant_name: row.applicant_name.clone(),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            total_qty: row.total_qty.round_dp(4),
            total_amount: row.total_amount.round_dp(4),
            submitted_at: row.submitted_at,
            approved_at: row.approved_at,
            shipped_at: row.shipped_at,
            received_at: row.received_at,
            cancelled_at: row.cancelled_at,
            cancel_reason: row.cancel_reason.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 一条明细（含该行的序列号 id 列表）。
#[derive(Debug, Clone, Serialize)]
pub struct TransferItemRes {
    /// 明细主键。
    pub id: String,
    /// 行号（`1..=N`）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（连表带出）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 调出库位 id。
    pub from_location_id: Option<String>,
    /// 调入库位 id。
    pub to_location_id: Option<String>,
    /// 调拨数量。
    pub qty: Decimal,
    /// 已发数量（W3c 发货时写，本卡不动）。
    pub shipped_qty: Decimal,
    /// 已收数量（W3c 收货时写，本卡不动）。
    pub received_qty: Decimal,
    /// 本行随行序列号 id（有序）。
    pub serial_ids: Vec<String>,
}

/// 调拨单详情 = 卡片 + 明细（明细里带序列号 id）。
#[derive(Debug, Clone, Serialize)]
pub struct TransferDetailRes {
    /// 单据头（所有卡片字段）。
    #[serde(flatten)]
    pub transfer: TransferRes,
    /// 明细（按 `line_no` 升序）。
    pub items: Vec<TransferItemRes>,
}

/// 序列号分组出参（`GET /business/erp/transfer/{id}/serials`）。
#[derive(Debug, Clone, Serialize)]
pub struct TransferSerialsRes {
    /// 调拨单 id。
    pub transfer_id: String,
    /// 调拨单号。
    pub transfer_no: String,
    /// 涉及的序列号总数。
    pub total: usize,
    /// 按明细分组（`line_no` 升序）。
    pub groups: Vec<TransferSerialGroupRes>,
}

/// 一个明细行的序列号集合。
#[derive(Debug, Clone, Serialize)]
pub struct TransferSerialGroupRes {
    /// 明细主键。
    pub item_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 该行的序列号。
    pub serials: Vec<TransferSerialRes>,
}

/// 一个序列号（回读 `biz_material_serial` 的号）。
#[derive(Debug, Clone, Serialize)]
pub struct TransferSerialRes {
    /// `biz_stock_transfer_item_serial.id`。
    pub id: String,
    /// `biz_material_serial.id`。
    pub serial_id: String,
    /// 序列号（`SN-yyyyMMdd-000001`）。
    pub serial_no: Option<String>,
}

impl TransferItemRes {
    /// 由数据层行 + 物料名 + 该行序列号 id 组装。
    pub fn of(
        row: &TransferItemRow,
        material_code: Option<String>,
        material_name: Option<String>,
        serial_ids: Vec<String>,
    ) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            material_code,
            material_name,
            from_location_id: row.from_location_id.map(|id| id.to_string()),
            to_location_id: row.to_location_id.map(|id| id.to_string()),
            qty: row.qty.round_dp(4),
            shipped_qty: row.shipped_qty.round_dp(4),
            received_qty: row.received_qty.round_dp(4),
            serial_ids,
        }
    }
}

impl TransferSerialRes {
    /// 由子表行组装。
    pub fn of(row: &TransferSerialRow) -> Self {
        Self {
            id: row.id.to_string(),
            serial_id: row.serial_id.to_string(),
            serial_no: row.serial_no.clone(),
        }
    }
}

// ==================== 内部辅助 ====================

/// 参数错误码（`10001`，与其余模块一致）。
fn error_code_param() -> i32 {
    crate::shared::error_code::PARAM_INVALID
}

/// 去空白 + 空串归一成 `None`。
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// 必填 UUID 字段校验。
fn require_uuid(value: &str, field: &str) -> std::result::Result<(), ApiError> {
    let text = value.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code_param(),
            format!("{field} 不能为空"),
        ));
    }
    if nik::uuid::Uuid::parse_str(text).is_err() {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        ));
    }
    Ok(())
}

/// 可空 UUID 字段校验（空串 / 缺席都放过）。
fn check_optional_uuid(value: Option<&str>, field: &str) -> std::result::Result<(), ApiError> {
    if let Some(text) = trimmed(value)
        && nik::uuid::Uuid::parse_str(text).is_err()
    {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uuid_text(tail: &str) -> String {
        format!("d3b00000-0000-4000-8000-{tail:0>12}")
    }

    fn item(material_tail: &str, qty: &str) -> TransferItemReq {
        TransferItemReq {
            material_id: uuid_text(material_tail),
            from_location_id: None,
            to_location_id: None,
            qty: Decimal::from_str(qty).expect("夹具数量"),
            serial_ids: Vec::new(),
        }
    }

    fn req(from: &str, to: &str, items: Vec<TransferItemReq>) -> CreateTransferReq {
        CreateTransferReq {
            from_warehouse_id: uuid_text(from),
            to_warehouse_id: uuid_text(to),
            transfer_date: None,
            reason: None,
            remark: None,
            items,
        }
    }

    /// ① 建单体：调出 = 调入 → 400（与 DB 的 `ck_transfer_diff_wh` 同口径，先给可读文案）。
    #[test]
    fn create_rejects_same_warehouse() {
        let error = req("1", "1", vec![item("2", "3")])
            .validate()
            .expect_err("同仓调拨必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不能相同"), "{}", error.msg);
    }

    /// ② 建单体：明细为空 → 400。
    #[test]
    fn create_rejects_empty_items() {
        let error = req("1", "2", Vec::new())
            .validate()
            .expect_err("没有明细必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("items 不能为空"), "{}", error.msg);
    }

    /// ③ 建单体：数量 ≤ 0 → 400；第 5 位小数 → 400（不允许静默四舍五入）。
    #[test]
    fn create_rejects_non_positive_and_over_precise_qty() {
        let zero = req("1", "2", vec![item("3", "0")])
            .validate()
            .expect_err("qty = 0 必须 400");
        assert_eq!(zero.status, 400);
        assert!(zero.msg.contains("必须 > 0"), "{}", zero.msg);

        let over = req("1", "2", vec![item("3", "1.00001")])
            .validate()
            .expect_err("5 位小数必须 400");
        assert_eq!(over.status, 400);
        assert!(over.msg.contains("最多 4 位小数"), "{}", over.msg);
    }

    /// ④ 建单体：同一序列号出现在两行 → 400（库里的唯一键只按 (item_id, serial_id) 拦不住跨行）。
    #[test]
    fn create_rejects_serial_used_by_two_lines() {
        let mut first = item("3", "1");
        first.serial_ids = vec![uuid_text("9")];
        let mut second = item("4", "1");
        second.serial_ids = vec![uuid_text("9")];
        let error = req("1", "2", vec![first, second])
            .validate()
            .expect_err("跨行复用序列号必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("同时出现在"), "{}", error.msg);
    }

    /// ⑤ 合计数量按 4 位标度累加（`NUMERIC(18,4)`）。
    #[test]
    fn total_qty_is_summed_with_four_decimals() {
        let body = req(
            "1",
            "2",
            vec![item("3", "1.5"), item("4", "2.25"), item("5", "0.0001")],
        );
        assert_eq!(body.total_qty().to_string(), "3.7501");
    }

    /// ⑥ 分页筛选：非法 status → 400（拼错就返回空列表最容易被当成「没数据」）。
    #[test]
    fn filter_rejects_unknown_status() {
        let filter = TransferFilter {
            status: Some("pending".to_string()),
            ..Default::default()
        };
        // 小写会被归一化吗？不会：筛选条件是**精确值**，必须与 DDL 的 CHECK 一致。
        let error = filter.validated().expect_err("小写状态必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("DRAFT"), "{}", error.msg);
    }

    /// ⑦ 状态流转入参：`→ CANCELLED` 必须给 reason（DB 的 `ck_biz_transfer_state` 要求）。
    #[test]
    fn status_req_requires_cancel_reason_only_for_cancelled() {
        let cancel = TransferStatusReq {
            status: "cancelled".to_string(),
            reason: None,
        };
        assert_eq!(
            cancel.target_status().expect("大小写归一"),
            STATUS_CANCELLED
        );
        let error = cancel
            .cancel_reason()
            .expect_err("取消必须给原因（文案要说清为什么）");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("ck_biz_transfer_state"), "{}", error.msg);

        let approve = TransferStatusReq {
            status: "APPROVED".to_string(),
            reason: None,
        };
        assert_eq!(approve.cancel_reason().expect("非取消目标不看原因"), None);
    }

    /// ⑧ 权限码表：6 条、`business:erp:transfer:` 前缀、无重复（与 V0144 逐字一致由 verify 脚本再核一次）。
    #[test]
    fn permission_codes_match_card() {
        assert_eq!(TRANSFER_PERMISSIONS.len(), 6);
        let mut codes: Vec<&str> = TRANSFER_PERMISSIONS.iter().map(|(code, _)| *code).collect();
        assert!(
            codes
                .iter()
                .all(|code| code.starts_with("business:erp:transfer:"))
        );
        codes.sort_unstable();
        let unique = {
            let mut dedup = codes.clone();
            dedup.dedup();
            dedup
        };
        assert_eq!(unique.len(), codes.len(), "权限码不得重复");
        for action in ["list", "detail", "create", "update", "delete", "status"] {
            assert!(
                codes.contains(&format!("business:erp:transfer:{action}").as_str()),
                "缺 action {action}"
            );
        }
    }
}
