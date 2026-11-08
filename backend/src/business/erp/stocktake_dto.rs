//! 盘点单接口契约（ARCH-3 §5 **DC-14**）：建单 / 填实盘 / 提交审批 / 过账 / 作废 / 详情 / 分页。
//!
//! 与 `stockin_dto` / `stockout_dto` 同一套约定：主键与外键一律 `String`
//! （`nik::uuid::Uuid` 没开 serde feature）、日期 `NaiveDate`（`YYYY-MM-DD`）、
//! 数量 `Decimal`（JSON 里是字符串）、状态一律大写且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 客户端**不能**传哪些字段
//!
//! | 字段 | 由谁决定 | 为什么 |
//! |------|----------|--------|
//! | `book_qty` | 建单时从 `biz_inventory` **快照** | 账面数要是能被客户端改，盘点就成了「自己填账面数」的游戏 |
//! | `diff_qty` | PG 生成列（`actual_qty − book_qty`） | 口径由库保证，不接受第二次计算 |
//! | `diff_amount` | 过账时按**账上真实差额**写 | 让客户端传金额等于给它一个「自己定价」的入口 |
//! | `take_no` | 取号器（`ST-yyyyMMdd-000001`） | 与其它单据同一个取号实现 |
//!
//! # 校验边界
//!
//! | 规则 | 判定 | 依据 |
//! |---|---|---|
//! | `take_type` 两值之一 | 400 | `ck_biz_st_type` |
//! | 全盘（FULL）不允许传 items | 400 | 全盘 = 按仓库现有库存桶自动快照，传 items 说明用错了类型 |
//! | 抽盘（PARTIAL）必须传 1–2000 行 | 400 | 空抽盘等于什么都没盘 |
//! | 实盘数 ≥ 0 | 400 | `ck_biz_st_item_qty` |
//! | 仓库存在且未删 | 400 `66030` | 弱引用靠应用层校验 |
//! | 作废已过账单据必须写原因 | 400 | 状态机 `STOCK_TAKE_REASON_EDGES` |
//!
//! # 权限码（与 `V0211__erp_stock_take.up.sql` 逐字一致）
//!
//! 6 个码：`list` / `detail` / `create` / `update` / `delete` / `status`。
//! `:status` 独立于 `:update`：**过账会真的动三本账并出凭证**，作废要回滚 + 红冲。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::approval_gate;
use crate::business::shared::state_machine;
use crate::shared::error_code;

use super::stocktake_model::{
    self as model, MAX_LINES, STATUS_CANCELLED, STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED,
    StockTakeFilter, StockTakeItemRow, StockTakeRow, TAKE_TYPES, status_label, take_type_label,
};

/// 备注最大长度（列 `VARCHAR(500)`）。
pub const MAX_REMARK_LEN: usize = 500;

/// 作废原因最大长度（列 `VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;

/// 本模块的 6 个权限码（与迁移逐字一致）。
pub const STOCK_TAKE_PERMISSIONS: [(&str, &str); 6] = [
    ("business:erp:stock-take:list", "盘点单列表"),
    (
        "business:erp:stock-take:detail",
        "盘点单详情（账面 / 实盘 / 差异 / 凭证）",
    ),
    (
        "business:erp:stock-take:create",
        "新建盘点单（账面数自动快照）",
    ),
    (
        "business:erp:stock-take:update",
        "编辑盘点单（仅草稿：填实盘数）",
    ),
    ("business:erp:stock-take:delete", "删除盘点单（仅草稿）"),
    (
        "business:erp:stock-take:status",
        "盘点单状态流转（提交审批 / 过账 / 作废）",
    ),
];

/// 状态流转允许的 3 个目标态（`DRAFT` 也是目标：撤回 / 驳回）。
pub const TRANSITION_TARGETS: [&str; 4] = ["POSTED", "PENDING_POST", "DRAFT", "CANCELLED"];

/// 400 快捷构造。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析 uuid 字符串。
fn parse_uuid_text(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|err| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 不是合法 UUID：{err}"),
        )
    })
}

// ==================== 入参 ====================

/// 抽盘范围的一行（只要物料 / 库位，账面数由服务端快照）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockTakeScopeItemReq {
    /// 物料。
    pub material_id: Option<String>,
    /// 库位账行（`biz_inventory_location.id`，可空 = 未分配桶）。
    pub location_id: Option<String>,
}

impl StockTakeScopeItemReq {
    /// 解析成 `(material_id, location_id)`。
    pub fn parse(&self, index: usize) -> std::result::Result<(Uuid, Option<Uuid>), ApiError> {
        let raw = self
            .material_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                bad_request(format!(
                    "第 {} 行缺 material_id：抽盘必须逐行指定物料",
                    index + 1
                ))
            })?;
        let material_id = parse_uuid_text(raw, &format!("第 {} 行 material_id", index + 1))?;
        let location_id = match self
            .location_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            None => None,
            Some(text) => Some(parse_uuid_text(
                text,
                &format!("第 {} 行 location_id", index + 1),
            )?),
        };
        Ok((material_id, location_id))
    }
}

/// 建单请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateStockTakeReq {
    /// 盘点仓库（必填）。
    pub warehouse_id: Option<String>,
    /// 盘点日期（缺省 = 今天，北京时间）。
    pub take_date: Option<NaiveDate>,
    /// 盘点类型：`FULL` 全盘（缺省）/ `PARTIAL` 抽盘。
    pub take_type: Option<String>,
    /// 备注（盘点范围 / 组织说明）。
    pub remark: Option<String>,
    /// 抽盘范围（`PARTIAL` 必填；`FULL` 必须为空 —— 全盘按仓库现有库存桶自动快照）。
    pub items: Vec<StockTakeScopeItemReq>,
}

impl CreateStockTakeReq {
    /// 盘点仓库（必填）。
    pub fn warehouse(&self) -> std::result::Result<Uuid, ApiError> {
        let raw = self
            .warehouse_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| bad_request("warehouse_id 不能为空：盘点必须指定仓库"))?;
        parse_uuid_text(raw, "warehouse_id")
    }

    /// 盘点类型（缺省 `FULL`）。
    pub fn take_type_value(&self) -> std::result::Result<String, ApiError> {
        let raw = self.take_type.as_deref().map(str::trim).unwrap_or_default();
        let value = if raw.is_empty() {
            model::TAKE_TYPE_FULL.to_string()
        } else {
            raw.to_uppercase()
        };
        if !TAKE_TYPES.contains(&value.as_str()) {
            return Err(bad_request(format!(
                "take_type 只能是 {}（收到 `{value}`）：全盘 / 抽盘",
                TAKE_TYPES.join(" / ")
            )));
        }
        Ok(value)
    }

    /// 备注（去空白 + 长度上限）。
    pub fn remark_text(&self) -> std::result::Result<Option<String>, ApiError> {
        let Some(text) = self
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            return Ok(None);
        };
        if text.chars().count() > MAX_REMARK_LEN {
            return Err(bad_request(format!(
                "备注不能超过 {MAX_REMARK_LEN} 字（当前 {} 字）",
                text.chars().count()
            )));
        }
        Ok(Some(text.to_string()))
    }

    /// 抽盘范围（类型与行数的交叉校验）。
    pub fn scope_items(
        &self,
        take_type: &str,
    ) -> std::result::Result<Vec<(Uuid, Option<Uuid>)>, ApiError> {
        if take_type == model::TAKE_TYPE_FULL {
            if !self.items.is_empty() {
                return Err(bad_request(
                    "全盘（FULL）不接受 items：全盘按该仓现有库存桶自动快照明细；\
                     只盘几个物料请用抽盘（PARTIAL）",
                ));
            }
            return Ok(Vec::new());
        }
        if self.items.is_empty() {
            return Err(bad_request(
                "抽盘（PARTIAL）必须传 items（1 行起）：空抽盘等于什么都没盘",
            ));
        }
        if self.items.len() > MAX_LINES {
            return Err(bad_request(format!(
                "抽盘明细最多 {MAX_LINES} 行（收到 {} 行）",
                self.items.len()
            )));
        }
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| item.parse(index))
            .collect()
    }
}

/// 一行实盘数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockTakeActualReq {
    /// 明细 id（建单时返回的那个）。
    pub item_id: Option<String>,
    /// 实盘数（≥ 0）。
    pub actual_qty: Option<Decimal>,
}

impl StockTakeActualReq {
    /// 解析成 `(item_id, actual_qty)`。
    pub fn parse(&self, index: usize) -> std::result::Result<(Uuid, Decimal), ApiError> {
        let raw = self
            .item_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| bad_request(format!("第 {} 行缺 item_id", index + 1)))?;
        let item_id = parse_uuid_text(raw, &format!("第 {} 行 item_id", index + 1))?;
        let qty = self
            .actual_qty
            .ok_or_else(|| bad_request(format!("第 {} 行缺 actual_qty（实盘数）", index + 1)))?;
        if qty < Decimal::ZERO {
            return Err(bad_request(format!(
                "第 {} 行 actual_qty = {qty}：实盘数不能为负（盘亏记正数，差异由系统算）",
                index + 1
            )));
        }
        Ok((item_id, qty.round_dp(4)))
    }
}

/// 编辑请求（**仅草稿**）：改日期 / 备注 + 逐行填实盘数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpdateStockTakeReq {
    /// 盘点日期（不传 = 不改）。
    pub take_date: Option<NaiveDate>,
    /// 备注（不传 = 不改；传空串 = 清空）。
    pub remark: Option<String>,
    /// 逐行实盘数（不传 = 只改单头）。
    pub lines: Vec<StockTakeActualReq>,
}

impl UpdateStockTakeReq {
    /// 备注（长度校验）。
    pub fn remark_text(&self) -> std::result::Result<Option<&str>, ApiError> {
        let Some(text) = self.remark.as_deref().map(str::trim) else {
            return Ok(None);
        };
        if text.chars().count() > MAX_REMARK_LEN {
            return Err(bad_request(format!(
                "备注不能超过 {MAX_REMARK_LEN} 字（当前 {} 字）",
                text.chars().count()
            )));
        }
        Ok(Some(text))
    }

    /// 逐行实盘数（行数上限 + 逐行解析）。
    pub fn parsed_lines(&self) -> std::result::Result<Vec<(Uuid, Decimal)>, ApiError> {
        if self.lines.len() > MAX_LINES {
            return Err(bad_request(format!(
                "实盘明细最多 {MAX_LINES} 行（收到 {} 行）",
                self.lines.len()
            )));
        }
        self.lines
            .iter()
            .enumerate()
            .map(|(index, line)| line.parse(index))
            .collect()
    }
}

/// 状态流转请求（提交审批 / 过账 / 撤回 / 作废）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockTakeStatusReq {
    /// 目标状态（也接受 `target_status`）。
    pub status: Option<String>,
    /// 别名（两个都给时**必须相等**）。
    pub target_status: Option<String>,
    /// 原因（作废已过账**必填**；进状态日志与红字凭证摘要）。
    pub reason: Option<String>,
    /// 审批「未配置 → 显式跳过」的请求片段（DC-01 口径）。
    #[serde(flatten)]
    pub gate: approval_gate::SkipApprovalReq,
}

impl StockTakeStatusReq {
    /// 目标状态（大写 + 白名单）。
    pub fn target(&self) -> std::result::Result<String, ApiError> {
        let raw = self
            .status
            .as_deref()
            .or(self.target_status.as_deref())
            .map(str::trim)
            .unwrap_or_default();
        if raw.is_empty() {
            return Err(bad_request(
                "status 不能为空：只能是 POSTED / PENDING_POST / DRAFT / CANCELLED",
            ));
        }
        let value = raw.to_uppercase();
        if !TRANSITION_TARGETS.contains(&value.as_str()) {
            return Err(bad_request(format!(
                "status 只能是 {}（收到 `{value}`）",
                TRANSITION_TARGETS.join(" / ")
            )));
        }
        if let (Some(a), Some(b)) = (self.status.as_deref(), self.target_status.as_deref())
            && !a.eq_ignore_ascii_case(b)
        {
            return Err(bad_request(format!(
                "status（{a}）与 target_status（{b}）互相矛盾：只给一个，或给两个相同的值"
            )));
        }
        Ok(value)
    }

    /// 原因文本（去空白；空串 → `None`；长度校验）。
    pub fn reason_text(&self) -> std::result::Result<Option<&str>, ApiError> {
        let Some(text) = self
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            return Ok(None);
        };
        if text.chars().count() > MAX_REASON_LEN {
            return Err(bad_request(format!(
                "原因不能超过 {MAX_REASON_LEN} 字（当前 {} 字）",
                text.chars().count()
            )));
        }
        Ok(Some(text))
    }
}

// ==================== 出参 ====================

/// 列表行。
#[derive(Debug, Clone, Serialize)]
pub struct StockTakeRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub take_no: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 盘点日期。
    pub take_date: String,
    /// 盘点类型。
    pub take_type: String,
    /// 盘点类型中文名。
    pub take_type_label: String,
    /// 状态。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 账面数量合计。
    pub total_book_qty: String,
    /// 实盘数量合计。
    pub total_actual_qty: String,
    /// 盘盈数量合计。
    pub total_gain_qty: String,
    /// 盘亏数量合计。
    pub total_loss_qty: String,
    /// 盘盈金额合计。
    pub total_gain_amount: String,
    /// 盘亏金额合计。
    pub total_loss_amount: String,
    /// 差异金额合计（审批流按它分档）。
    pub total_diff_amount: String,
    /// 盘点差异凭证（未过账 / 零差异为 null）。
    pub voucher_id: Option<String>,
    /// 差异凭证号（`biz_voucher.voucher_no`，如 `PZ-20261106-000002`）。
    ///
    /// 只有 `voucher_id` 的话前端只能显示 UUID 截断（`9256f0e…`），财务拿它
    /// **去凭证列表核不了账**（列表按 `voucher_no` 检索）。`StockTakeRes::of` 是纯
    /// 函数、拿不到库，所以这里默认 `None`，由 handler 回填（见 `fill_voucher_nos`）。
    pub voucher_no: Option<String>,
    /// 红字凭证。
    pub reverse_voucher_id: Option<String>,
    /// 红冲凭证号（作废后才有值，如 `PZR-20261106-000003`）。
    pub reverse_voucher_no: Option<String>,
    /// 提交审批的实例 id。
    pub approval_instance_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC 3339）。
    pub created_at: String,
    /// 制单人。
    pub create_by: Option<String>,
    /// 制单人姓名（**AUD-02**：实时取现值；`None` = 账号已不存在）。
    pub create_by_name: Option<String>,
}

impl StockTakeRes {
    /// 行 → 列表项。
    pub fn of(row: &StockTakeRow) -> Self {
        Self {
            id: row.id.to_string(),
            take_no: row.take_no.clone(),
            warehouse_id: row.warehouse_id.to_string(),
            take_date: row.take_date.to_string(),
            take_type: row.take_type.clone(),
            take_type_label: take_type_label(&row.take_type).to_string(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            total_book_qty: row.total_book_qty.round_dp(4).to_string(),
            total_actual_qty: row.total_actual_qty.round_dp(4).to_string(),
            total_gain_qty: row.total_gain_qty.round_dp(4).to_string(),
            total_loss_qty: row.total_loss_qty.round_dp(4).to_string(),
            total_gain_amount: row.total_gain_amount.round_dp(4).to_string(),
            total_loss_amount: row.total_loss_amount.round_dp(4).to_string(),
            total_diff_amount: row.total_diff_amount().to_string(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            // 凭证号要查 `biz_voucher`，纯函数补不了 —— 由 handler 的 `fill_voucher_nos` 回填。
            voucher_no: None,
            reverse_voucher_id: row.reverse_voucher_id.map(|id| id.to_string()),
            reverse_voucher_no: None,
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at.to_rfc3339(),
            create_by: row.create_by.map(|id| id.to_string()),
            create_by_name: None,
        }
    }
}

/// AUD-02：制单人姓名回填（`create_by` → `create_by_name`）。
impl crate::business::shared::user_name::PersonNameRow for StockTakeRes {
    fn person_ids(&self, out: &mut Vec<Uuid>) {
        if let Some(id) = self
            .create_by
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw).ok())
        {
            out.push(id);
        }
    }

    fn apply_names(&mut self, names: &std::collections::HashMap<Uuid, String>) {
        self.create_by_name = self
            .create_by
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .and_then(|id| names.get(&id).cloned());
    }
}

/// 明细行（含物料展示名）。
#[derive(Debug, Clone, Serialize)]
pub struct StockTakeItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 单位。
    pub unit_id: String,
    /// 库位账行。
    pub location_id: Option<String>,
    /// 账面数（建单时快照）。
    pub book_qty: String,
    /// 实盘数。
    pub actual_qty: String,
    /// 差异 = 实盘 − 账面（负 = 盘亏）。
    pub diff_qty: String,
    /// 账面成本快照。
    pub unit_cost: String,
    /// 差异金额（过账后才是账上真实值）。
    pub diff_amount: String,
    /// 行备注。
    pub remark: Option<String>,
}

impl StockTakeItemRes {
    /// 行 → 明细项。
    pub fn of(
        item: &StockTakeItemRow,
        material: Option<&super::stockout_model::MaterialInfo>,
    ) -> Self {
        Self {
            id: item.id.to_string(),
            line_no: item.line_no,
            material_id: item.material_id.to_string(),
            material_code: material.map(|info| info.material_code.clone()),
            material_name: material.map(|info| info.material_name.clone()),
            unit_id: item.unit_id.to_string(),
            location_id: item.location_id.map(|id| id.to_string()),
            book_qty: item.book_qty.round_dp(4).to_string(),
            actual_qty: item.actual_qty.round_dp(4).to_string(),
            diff_qty: item.diff_qty.round_dp(4).to_string(),
            unit_cost: item.unit_cost.to_string(),
            diff_amount: item.diff_amount.round_dp(4).to_string(),
            remark: item.remark.clone(),
        }
    }
}

/// 详情（单头 + 明细 + 可执行动作）。
#[derive(Debug, Clone, Serialize)]
pub struct StockTakeDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub header: StockTakeRes,
    /// 明细。
    pub items: Vec<StockTakeItemRes>,
    /// 过账时间（RFC 3339）。
    pub posted_at: Option<String>,
    /// 过账人。
    pub posted_by: Option<String>,
    /// 作废时间（RFC 3339）。
    pub cancelled_at: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 当前状态能去哪些状态（有效边表，前端按钮置灰直接用这份）。
    pub next_statuses: Vec<String>,
    /// 有效边表（与 `next_statuses` 同源）。
    pub legal_edges: Vec<(String, String)>,
}

impl StockTakeDetailRes {
    /// 组装详情（纯函数：边表由 handler 取一次传进来）。
    pub fn of(
        row: &StockTakeRow,
        items: &[(
            StockTakeItemRow,
            Option<super::stockout_model::MaterialInfo>,
        )],
        edges: &[state_machine::Edge],
    ) -> Self {
        let available: Vec<(String, String)> = edges
            .iter()
            .filter(|edge| edge.from == row.status.as_str())
            .map(|edge| (edge.from.to_string(), edge.to.to_string()))
            .collect();
        Self {
            header: StockTakeRes::of(row),
            items: items
                .iter()
                .map(|(item, material)| StockTakeItemRes::of(item, material.as_ref()))
                .collect(),
            posted_at: row.posted_at.map(|at| at.to_rfc3339()),
            posted_by: row.posted_by.map(|id| id.to_string()),
            cancelled_at: row.cancelled_at.map(|at| at.to_rfc3339()),
            cancel_reason: row.cancel_reason.clone(),
            next_statuses: available.iter().map(|(_, to)| to.clone()).collect(),
            legal_edges: available,
        }
    }
}

/// 写操作成功后统一返回详情（前端不用再发一次 GET）。
pub type StockTakeEnvelope = StockTakeDetailRes;

/// 分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockTakePageQuery {
    /// 状态。
    pub status: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 盘点类型。
    pub take_type: Option<String>,
    /// 单号关键字。
    pub keyword: Option<String>,
    /// 页码（1 起，缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
}

impl StockTakePageQuery {
    /// 转数据层筛选条件（非法 uuid → 400）。
    pub fn to_filter(&self) -> std::result::Result<StockTakeFilter, ApiError> {
        let warehouse_id = match self
            .warehouse_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            None => None,
            Some(text) => Some(parse_uuid_text(text, "warehouse_id")?),
        };
        Ok(StockTakeFilter {
            status: self.status.clone(),
            warehouse_id,
            take_type: self.take_type.clone(),
            keyword: self.keyword.clone(),
            page: self.page.unwrap_or(1).max(1),
            page_size: self.page_size.unwrap_or(20).clamp(1, 200),
        })
    }
}

/// 分页结果。
#[derive(Debug, Clone, Serialize)]
pub struct StockTakePageRes {
    /// 当前页。
    pub list: Vec<StockTakeRes>,
    /// 总条数。
    pub total: i64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 状态常量再导出（handler / 单测共用，避免各处重新写字符串）。
pub const STATUSES: [&str; 4] = [
    STATUS_DRAFT,
    STATUS_PENDING_POST,
    STATUS_POSTED,
    STATUS_CANCELLED,
];
