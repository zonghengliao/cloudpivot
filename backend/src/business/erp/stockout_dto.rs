//! 出库单接口契约（ARCH-3 DC-13）：建单 / 改单 / 状态流转（提交 / 过账 / 作废）/ 详情 / 分页。
//!
//! 与 `stockin_dto` / `purchase_dto` 同一套约定：主键与外键一律 `String`
//! （`nik::uuid::Uuid` 没开 serde feature）、日期 `NaiveDate`（`YYYY-MM-DD`）、
//! 数量 `Decimal`（JSON 里是字符串）、状态一律大写且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 单价与金额**不由客户端传**
//!
//! [`StockOutItemReq`] 只有 `material_id` / `qty` / `location_id` / `lot_no` / `remark`。
//! 出库成本 = **账上移动加权单价 × 数量**的真实差额，由过账时从 `biz_inventory`
//! 读出来的 `before/after` 决定。让客户端传单价等于给它一个「自己定价」的入口 ——
//! 那正是「库存账与存货科目对不上」的经典成因（B2 BLOCKER）。
//!
//! # 校验边界
//!
//! | 规则 | 判定 | 依据 |
//! |---|---|---|
//! | 数量 > 0（草稿也不允许 0） | 400 | `ck_biz_so_item_qty CHECK (qty > 0)` |
//! | 明细 1–200 行 | 400 | 防误传超大数组 |
//! | `out_type` 四值之一 | 400 | `ck_biz_so_type` |
//! | 仓库存在且未删 | 400 `66030`（引用缺失） | 弱引用靠应用层校验 |
//! | 物料存在且未删 | 400 `66030` | 同上 |
//! | **一物一码物料禁止走出库单** | 400 | 序列号件数必须由 `serial_handler` 维护，否则不变式②红 |
//! | 库位账行（`location_id`）属于本物料本仓 | 400 | `biz_inventory_location` 三列唯一 |
//! | 作废已过账单据必须写原因 | 400 | 状态机 `STOCK_OUT_REASON_EDGES` |
//!
//! # 权限码（与 `V0203__erp_stock_out.up.sql` 逐字一致）
//!
//! 6 个码：`list` / `detail` / `create` / `update` / `delete` / `status`。
//! `:status` 独立于 `:update`：**过账会真的减库存并生成凭证**，是比「改几行明细」
//! 重得多的授权（与 `business:erp:stock-in:status` 同口径）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::stockout_model::{self as model, CostObjectRef, MaterialInfo};
use crate::business::shared::state_machine;
use crate::shared::error_code;

use super::stockout_base::{
    OUT_TYPE_ISSUE, OUT_TYPES, STATUS_CANCELLED, STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED, StockOutFilter,
    StockOutItemRow, StockOutRow, debit_account_of_doc, out_type_label, status_label,
};

/// 明细行上限。
pub const MAX_LINES: usize = 200;

/// 备注最大长度（`remark TEXT`，业务口径 500 字）。
pub const MAX_REMARK_LEN: usize = 500;

/// 出库事由 / 作废原因最大长度（列 `VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;

/// 本卡的 6 个权限码（与迁移逐字一致）。
pub const STOCK_OUT_PERMISSIONS: [(&str, &str); 6] = [
    ("business:erp:stock-out:list", "出库单列表"),
    ("business:erp:stock-out:detail", "出库单详情（含明细与成本结转金额）"),
    ("business:erp:stock-out:create", "新建出库单（领用 / 报废 / 盘亏）"),
    ("business:erp:stock-out:update", "编辑出库单（仅草稿）"),
    ("business:erp:stock-out:delete", "删除出库单（仅草稿）"),
    ("business:erp:stock-out:status", "出库单状态流转（提交 / 过账 / 作废）"),
];

/// 状态流转允许的 3 个目标态。
///
/// `DRAFT` 也在内：`PENDING_POST → DRAFT` 是「撤回 / 驳回」，
/// 没有它，待过账的单据在没人过账时会永远卡住。
pub const TRANSITION_TARGETS: [&str; 4] = ["POSTED", "PENDING_POST", "DRAFT", "CANCELLED"];

/// 400 快捷构造（文案即 `msg`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 入参 ====================

/// 建单 / 改单请求（改单复用同一形状：整单覆盖）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateStockOutReq {
    /// 出库类型：`ISSUE` 领用 / `SCRAP` 报废 / `LOSS` 盘亏 / `OTHER` 其他（缺省领用）。
    pub out_type: Option<String>,
    /// 出库仓库（必填）。
    pub warehouse_id: Option<String>,
    /// 出库日期（缺省 = 今天，北京时间）。
    pub out_date: Option<NaiveDate>,
    /// 出库事由（领用用途 / 报废原因 / 盘亏说明）。
    pub reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 成本对象（ARCH-9 B1-10）：挂了就是**生产领料**，过账借方落 `4001.01`
    /// 生产成本-直接材料（不挂 → 原 6401 逻辑，存量行为零变化）。
    ///
    /// 建单必须是 `status = 'OPEN'` 的成本对象（`ensure_cost_object_usable` 校验），
    /// 且只有 `out_type = ISSUE` 的单据允许挂（报废 / 盘亏是资产损失，要进 1901）。
    /// 改单是**整单覆盖**：不传 / 传空串 = 清空（与 `reason` / `remark` 同口径）。
    pub cost_object_id: Option<String>,
    /// 明细（1–200 行）。
    pub items: Vec<StockOutItemReq>,
}

impl CreateStockOutReq {
    /// 出库类型（缺省 `ISSUE`，并校验四值白名单）。
    pub fn out_type_value(&self) -> std::result::Result<String, ApiError> {
        let raw = self.out_type.as_deref().map(str::trim).unwrap_or_default();
        let value = if raw.is_empty() {
            "ISSUE".to_string()
        } else {
            raw.to_uppercase()
        };
        if !OUT_TYPES.contains(&value.as_str()) {
            return Err(bad_request(format!(
                "out_type 只能是 {}（收到 `{value}`）：领用 / 报废 / 盘亏 / 其他",
                OUT_TYPES.join(" / ")
            )));
        }
        Ok(value)
    }

    /// 出库日期（缺省今天 CST）。
    pub fn out_date_value(&self) -> NaiveDate {
        self.out_date
            .unwrap_or_else(|| (Utc::now() + nik::prelude::chrono::Duration::hours(8)).date_naive())
    }

    /// 事由 / 备注的长度校验。
    pub fn check_texts(&self) -> std::result::Result<(), ApiError> {
        if let Some(reason) = self.reason.as_deref()
            && reason.chars().count() > MAX_REASON_LEN
        {
            return Err(bad_request(format!(
                "出库事由最长 {MAX_REASON_LEN} 字（当前 {} 字）",
                reason.chars().count()
            )));
        }
        if let Some(remark) = self.remark.as_deref()
            && remark.chars().count() > MAX_REMARK_LEN
        {
            return Err(bad_request(format!(
                "备注最长 {MAX_REMARK_LEN} 字（当前 {} 字）",
                remark.chars().count()
            )));
        }
        Ok(())
    }

    /// 明细行数 / 数量 / 文本的**纯逻辑**校验（不查库）。
    pub fn check_items(&self) -> std::result::Result<(), ApiError> {
        if self.items.is_empty() {
            return Err(bad_request("items 不能为空：出库单至少要有一行明细"));
        }
        if self.items.len() > MAX_LINES {
            return Err(bad_request(format!(
                "明细最多 {MAX_LINES} 行（当前 {} 行）",
                self.items.len()
            )));
        }
        for (index, item) in self.items.iter().enumerate() {
            let line_no = index + 1;
            if item.material_id.trim().is_empty() {
                return Err(bad_request(format!("第 {line_no} 行 material_id 不能为空")));
            }
            if item.qty <= Decimal::ZERO {
                return Err(bad_request(format!(
                    "第 {line_no} 行数量必须 > 0（收到 {}）：0 数量出库没有业务含义，也不该占一行",
                    item.qty
                )));
            }
            if let Some(remark) = item.remark.as_deref()
                && remark.chars().count() > MAX_REASON_LEN
            {
                return Err(bad_request(format!("第 {line_no} 行备注最长 {MAX_REASON_LEN} 字")));
            }
        }
        Ok(())
    }

    /// 成本对象 id（空白 / `None` → `None` = 不挂；非法 UUID → 400）。
    pub fn cost_object_id_value(&self) -> std::result::Result<Option<uuid::Uuid>, ApiError> {
        match self
            .cost_object_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            None => Ok(None),
            Some(text) => Ok(Some(crate::business::erp::cost_object_dto::parse_uuid(
                text,
                "cost_object_id",
            )?)),
        }
    }
}

// ==================== 成本对象校验（ARCH-9 B1-10） ====================

/// 成本对象状态的**中文名**（错误文案用）。
///
/// 值域来自 `cost_object_dto::STATUSES`（与 `ck_biz_cost_object_status` 逐字一致）；
/// 文案刻意与「关批」区分：`CLOSED` 说「已关批」而不是「已关闭」，用户才知道
/// 是「这批已经结完成本了，不能再往里投料」。
pub fn cost_object_status_label(status: &str) -> &'static str {
    match status {
        super::cost_object_dto::STATUS_OPEN => "在制",
        super::cost_object_dto::STATUS_CLOSED => "已关批",
        super::cost_object_dto::STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 领料单挂成本对象的**纯逻辑**校验（不查库，查库结果由 handler 传进来）。
///
/// # 三条规则（ARCH-9 §4.7.1 + §4.3）
///
/// | 规则 | 结果 | 依据 |
/// |---|---|---|
/// | 不挂对象（`None`） | 放行 | 存量行为：过账借 6401，不进成本核算 |
/// | 只有 `ISSUE` 能挂 | 400 `10001` | 报废 / 盘亏是资产损失（进 1901），且料归集只扫 `out_type='ISSUE'`，挂了也永远归集不到 |
/// | 对象必须存在 | 400 `66002` | 弱引用，库层没有物理外键 |
/// | 对象必须 `status = 'OPEN'` | 409 `66057` | 已关批 / 已作废的对象不能再投料（关批后成本已结转） |
///
/// 为什么「存在性」与「状态」给不同 HTTP 码：前者是**入参指错了**（客户端 bug /
/// 单据已删），后者是**业务规则拒绝**（对象确实存在，就是不让挂）—— 前端提示与
/// 用户下一步动作完全不同（改 id vs 换对象）。
pub fn ensure_cost_object_usable(
    out_type: &str,
    cost_object_id: Option<uuid::Uuid>,
    object: Option<&CostObjectRef>,
) -> std::result::Result<(), ApiError> {
    let Some(cost_object_id) = cost_object_id else {
        return Ok(());
    };
    if out_type != OUT_TYPE_ISSUE {
        return Err(bad_request(format!(
            "只有领用单（out_type=ISSUE）可以挂成本对象：本单是{}单。\
             报废 / 盘亏是资产损失（借 1901 待处理财产损溢），也不进成本核算的料归集",
            out_type_label(out_type)
        )));
    }
    let Some(object) = object else {
        return Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!("cost_object_id 指向的成本对象 {cost_object_id} 不存在或已删除"),
        ));
    };
    if object.status != super::cost_object_dto::STATUS_OPEN {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_OBJECT_NOT_OPEN,
            format!(
                "成本对象 {} 当前是「{}」（{}），不能投料：只有 OPEN（在制）的对象可以挂领料单。\
                 已关批的对象成本已经结转过，再投料会与企业所得税口径的在制余额对不上；\
                 请换一个在制的成本对象，或新建一批",
                object.object_no,
                cost_object_status_label(&object.status),
                object.status
            ),
        ));
    }
    Ok(())
}

/// 一行明细的入参（**没有单价 / 金额 / 单位**：由服务端按物料与账上均价决定）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockOutItemReq {
    /// 物料。
    pub material_id: String,
    /// 出库数量（> 0）。
    pub qty: Decimal,
    /// 库位账行（`biz_inventory_location.id`，可空 = 未分配库位桶）。
    pub location_id: Option<String>,
    /// 批号（可空）。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockOutStatusReq {
    /// 目标状态：`POSTED` / `PENDING_POST` / `DRAFT` / `CANCELLED`（也接受 `target_status`）。
    pub status: Option<String>,
    /// 别名（与 `status` 同值；两个都给时**必须相等**）。
    pub target_status: Option<String>,
    /// 原因（作废已过账单据**必填**；进状态日志与红字凭证摘要）。
    pub reason: Option<String>,
}

impl StockOutStatusReq {
    /// 目标状态（大写，白名单校验）。
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

    /// 原因文本（去空白；空串 → `None`）。
    pub fn reason_text(&self) -> Option<&str> {
        self.reason.as_deref().map(str::trim).filter(|text| !text.is_empty())
    }
}

/// 分页查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockOutPageQuery {
    /// 关键词（单号 / 事由 / 备注）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 出库类型。
    pub out_type: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 出库日期起。
    pub date_from: Option<NaiveDate>,
    /// 出库日期止。
    pub date_to: Option<NaiveDate>,
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

impl StockOutPageQuery {
    /// 转成 model 层筛选条件。
    pub fn to_filter(&self) -> StockOutFilter {
        StockOutFilter {
            keyword: self.keyword.clone(),
            status: self.status.as_deref().map(|text| text.trim().to_uppercase()),
            out_type: self.out_type.as_deref().map(|text| text.trim().to_uppercase()),
            warehouse_id: self.warehouse_id.clone(),
            date_from: self.date_from,
            date_to: self.date_to,
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

// ==================== 出参 ====================

/// 列表行。
#[derive(Debug, Clone, Serialize)]
pub struct StockOutRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub out_no: String,
    /// 出库类型。
    pub out_type: String,
    /// 出库类型中文名。
    pub out_type_label: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 出库日期。
    pub out_date: String,
    /// 状态。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 数量合计。
    pub total_qty: String,
    /// 成本结转金额合计（草稿为 0，过账后才是账上真实差额）。
    pub total_amount: String,
    /// 凭证 id（未过账 / 零成本为 null）。
    pub voucher_id: Option<String>,
    /// 出库事由。
    pub reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 单据来源类型（`SALES` = 销售发货过账自动生成；手工建单为 `null`）。
    ///
    /// 销售出库的 `out_type` 固定 `OTHER`，**只能靠本字段区分**「销售发货」与
    /// 「手工其他出库」，前端列表 / 详情 / 追溯发货单都用它。
    pub source_type: Option<String>,
    /// 来源单据 id（销售链 = 发货单 id）。
    pub source_id: Option<String>,
    /// 成本对象（ARCH-9 B1-10；`null` = 未挂，即存量口径）。
    pub cost_object_id: Option<String>,
    /// 成本对象号（`CB-yyyyMMdd-NNNNNN`，实时取；主数据已删 / 未挂 → `null`）。
    ///
    /// 约定 13「主数据关联 → join 实时取名」：`biz_cost_object` 没有独立名称列，
    /// 对象号就是用户认的那个名字，所以这里只补对象号（不另造 name）。
    pub cost_object_no: Option<String>,
    /// **复算出来的过账借方科目**编码（`4001.01` 挂对象 / `6401` | `1901` 不挂）。
    ///
    /// 给前端「选完成本对象借方科目提示会变」用（B1-12）：口径与过账完全同一份：
    /// `stockout_base::debit_account_of_doc`（不另写一份映射，避免前后端两套）。
    pub debit_account_code: String,
    /// 借方科目名称（`生产成本-直接材料` / `主营业务成本` / `待处理财产损溢`）。
    pub debit_account_name: String,
    /// 创建时间（RFC 3339）。
    pub created_at: String,
    /// 制单人。
    pub create_by: Option<String>,
    /// 制单人姓名（**AUD-02**：实时取现值；`None` = 账号已不存在）。
    pub create_by_name: Option<String>,
}

impl StockOutRes {
    /// 行 → 列表项。
    pub fn of(row: &StockOutRow) -> Self {
        // 借方科目**复算**（ARCH-9 §4.7.1）：挂成本对象的领用单 → 4001.01，其余原逻辑。
        let (debit_account_code, debit_account_name) = debit_account_of_doc(&row.out_type, row.cost_object_id);
        Self {
            id: row.id.to_string(),
            out_no: row.out_no.clone(),
            out_type: row.out_type.clone(),
            out_type_label: out_type_label(&row.out_type).to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            out_date: row.out_date.to_string(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            total_qty: row.total_qty.round_dp(4).to_string(),
            total_amount: row.total_amount.round_dp(4).to_string(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            reason: row.reason.clone(),
            remark: row.remark.clone(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.map(|id| id.to_string()),
            cost_object_id: row.cost_object_id.map(|id| id.to_string()),
            // 对象号由 `fill_cost_object_labels` 批量回填（列表 / 详情各一次 SQL）
            cost_object_no: None,
            debit_account_code: debit_account_code.to_string(),
            debit_account_name: debit_account_name.to_string(),
            created_at: row.created_at.to_rfc3339(),
            create_by: row.create_by.map(|id| id.to_string()),
            create_by_name: None,
        }
    }
}

/// AUD-02：制单人姓名回填（`create_by` → `create_by_name`）。
impl crate::business::shared::user_name::PersonNameRow for StockOutRes {
    fn person_ids(&self, out: &mut Vec<nik::uuid::Uuid>) {
        if let Some(id) = self
            .create_by
            .as_deref()
            .and_then(|raw| nik::uuid::Uuid::parse_str(raw).ok())
        {
            out.push(id);
        }
    }

    fn apply_names(&mut self, names: &std::collections::HashMap<nik::uuid::Uuid, String>) {
        self.create_by_name = self
            .create_by
            .as_deref()
            .and_then(|raw| nik::uuid::Uuid::parse_str(raw).ok())
            .and_then(|id| names.get(&id).cloned());
    }
}

/// 明细行（含物料展示名与库位账行）。
#[derive(Debug, Clone, Serialize)]
pub struct StockOutItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 物料编码（详情读出来的，便于列表直接展示）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 单位。
    pub unit_id: String,
    /// 出库数量。
    pub qty: String,
    /// 过账时的移动加权单价快照（草稿为 0）。
    pub unit_cost: String,
    /// 过账时账上真实扣减金额（草稿为 0）。
    pub amount: String,
    /// 库位账行（`biz_inventory_location.id`）。
    pub location_id: Option<String>,
    /// 批号。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl StockOutItemRes {
    /// 行 → 明细项。
    pub fn of(item: &StockOutItemRow, material: Option<&MaterialInfo>) -> Self {
        Self {
            id: item.id.to_string(),
            line_no: item.line_no,
            material_id: item.material_id.to_string(),
            material_code: material.map(|info| info.material_code.clone()),
            material_name: material.map(|info| info.material_name.clone()),
            unit_id: item.unit_id.to_string(),
            qty: item.qty.round_dp(4).to_string(),
            unit_cost: item.unit_cost.to_string(),
            amount: item.amount.round_dp(4).to_string(),
            location_id: item.location_id.map(|id| id.to_string()),
            lot_no: item.lot_no.clone(),
            remark: item.remark.clone(),
        }
    }
}

/// 详情（单头 + 明细 + 可执行动作）。
#[derive(Debug, Clone, Serialize)]
pub struct StockOutDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub header: StockOutRes,
    /// 明细。
    pub items: Vec<StockOutItemRes>,
    /// 过账时间（RFC 3339）。
    pub posted_at: Option<String>,
    /// 作废时间（RFC 3339）。
    pub cancelled_at: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 当前状态能去哪些状态（前端按钮置灰直接用这份，不在前端写第二份状态机）。
    ///
    /// **D3 起读的是「有效边」**（`state_machine::effective_edges` = BASE_EDGES ∩ 配置）：
    /// 管理端把 `DRAFT → POSTED` 配成禁用之后，这里就不再出现 `POSTED`，前端按钮随之消失
    /// —— 不再是「按钮还在、点下去 409」。
    pub next_statuses: Vec<String>,
    /// 有效边表（与 `next_statuses` 同源，可供调试 / 单测对照）。
    pub legal_edges: Vec<(String, String)>,
    /// 库存提示（P1-FIX-2）：草稿 / 待过账的单据里，**哪一行不够、差多少、现有货在哪些库位**。
    ///
    /// 只提示不拦截（草稿可以先建、等货到再出），过账时由 409 兜底。空数组 = 没发现缺口。
    pub stock_warnings: Vec<String>,
}

impl StockOutDetailRes {
    /// 组装详情。
    ///
    /// `edges` 必须是**有效边**（`state_machine::effective_edges` / `edge_resolution`），
    /// `stock_warnings` 由 handler 查库算出（本函数保持纯函数性质：不碰数据库）。
    pub fn of(
        row: &StockOutRow,
        items: &[(StockOutItemRow, Option<MaterialInfo>)],
        edges: &[state_machine::Edge],
        stock_warnings: Vec<String>,
    ) -> Self {
        let available: Vec<(String, String)> = edges
            .iter()
            .filter(|edge| edge.from == row.status.as_str())
            .map(|edge| (edge.from.to_string(), edge.to.to_string()))
            .collect();
        Self {
            header: StockOutRes::of(row),
            items: items
                .iter()
                .map(|(item, material)| StockOutItemRes::of(item, material.as_ref()))
                .collect(),
            posted_at: row.posted_at.map(|at| at.to_rfc3339()),
            cancelled_at: row.cancelled_at.map(|at| at.to_rfc3339()),
            cancel_reason: row.cancel_reason.clone(),
            next_statuses: available.iter().map(|(_, to)| to.clone()).collect(),
            legal_edges: available,
            stock_warnings,
        }
    }
}

/// 建单 / 改单 / 状态流转成功后统一返回详情（与收货单同款：前端不用再发一次 GET）。
pub type StockOutEnvelope = StockOutDetailRes;

/// 分页结果（与其它列表接口同形：`list` + `total`）。
#[derive(Debug, Clone, Serialize)]
pub struct StockOutPageRes {
    /// 当前页。
    pub list: Vec<StockOutRes>,
    /// 总条数。
    pub total: i64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 把明细 + 物料信息拼成 DTO（详情 / 建单返回共用）。
///
/// `edges` 传**有效边**（`state_machine::effective_edges`）：前端按钮跟着配置走，
/// 而不是跟着代码里的边表走（D1 §8.1 的已知边界，D3 收口）。
/// `stock_warnings` 由 handler 查库算出（缺口 / 库位分布，见 P1-FIX-2）。
pub fn detail_of(
    row: &StockOutRow,
    items: Vec<StockOutItemRow>,
    materials: &std::collections::HashMap<uuid::Uuid, MaterialInfo>,
    edges: &[state_machine::Edge],
    stock_warnings: Vec<String>,
) -> StockOutDetailRes {
    let pairs: Vec<(StockOutItemRow, Option<MaterialInfo>)> = items
        .into_iter()
        .map(|item| {
            let info = materials.get(&item.material_id).cloned();
            (item, info)
        })
        .collect();
    StockOutDetailRes::of(row, &pairs, edges, stock_warnings)
}

/// 提前校验「该状态的单据不能再改」（400，附中文状态名）。
pub fn ensure_draft(status: &str) -> std::result::Result<(), ApiError> {
    if status != STATUS_DRAFT {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "出库单当前是「{}」（{}），只有草稿（DRAFT）可以编辑 / 删除；\
                 要改已提交或已过账的单据请先撤回或作废重开",
                status_label(status),
                status
            ),
        ));
    }
    Ok(())
}

/// 状态 → 是否已过账（handler 分流用，集中一处免得写错字符串）。
pub fn is_posted(status: &str) -> bool {
    status == STATUS_POSTED
}

/// 状态 → 是否待过账。
pub fn is_pending_post(status: &str) -> bool {
    status == STATUS_PENDING_POST
}

/// 状态 → 是否已作废。
pub fn is_cancelled(status: &str) -> bool {
    status == STATUS_CANCELLED
}

/// 由行构造列表 DTO（handler 用）。
pub fn list_res(rows: &[StockOutRow]) -> Vec<StockOutRes> {
    rows.iter().map(StockOutRes::of).collect()
}

/// 批量取物料信息（详情用，一次查询而不是 N 次）。
pub async fn materials_of(
    material_ids: &[uuid::Uuid],
) -> DbResult<std::collections::HashMap<uuid::Uuid, MaterialInfo>> {
    let mut map = std::collections::HashMap::new();
    for id in material_ids {
        if map.contains_key(id) {
            continue;
        }
        if let Some(info) = model::material_info(db(), *id).await? {
            map.insert(*id, info);
        }
    }
    Ok(map)
}

/// 批量回填成本对象号（`cost_object_id` → `cost_object_no`），**整页一次 SQL**。
///
/// 与 `user_name::fill_person_names` 同一个模式：收集 id → 查一次 → 回填，
/// 绝不在循环里查库（列表一页 100 张单 = 100 次查询是典型的 N+1）。
/// 主数据已删 / 未挂 → 保持 `None`（前端显示 `-`，绝不显示 UUID —— 约定 13）。
pub async fn fill_cost_object_labels(list: &mut [StockOutRes]) -> DbResult<()> {
    let ids: Vec<uuid::Uuid> = list
        .iter()
        .filter_map(|row| row.cost_object_id.as_deref())
        .filter_map(|raw| uuid::Uuid::parse_str(raw).ok())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let names = model::cost_object_labels(db(), &ids).await?;
    for row in list.iter_mut() {
        if let Some(id) = row
            .cost_object_id
            .as_deref()
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
        {
            row.cost_object_no = names.get(&id).cloned();
        }
    }
    Ok(())
}

/// 当前操作时间（测试里可替换；目前恒为 `Utc::now()`）。
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_no(status: &str) -> CostObjectRef {
        CostObjectRef {
            object_no: "CB-20261110-000001".to_string(),
            status: status.to_string(),
        }
    }

    /// 挂一个 **OPEN** 的成本对象 → 放行（卡面验收「可选成本对象」）。
    #[test]
    fn issue_with_open_cost_object_is_accepted() {
        let object = object_no(super::super::cost_object_dto::STATUS_OPEN);
        ensure_cost_object_usable(OUT_TYPE_ISSUE, Some(uuid::Uuid::now_v7()), Some(&object))
            .expect("OPEN 对象必须放行");
    }

    /// 不挂对象 → 放行（存量行为；不查库、不看 out_type）。
    #[test]
    fn no_cost_object_is_always_accepted() {
        for out_type in OUT_TYPES {
            ensure_cost_object_usable(out_type, None, None)
                .unwrap_or_else(|error| panic!("{out_type} 不挂对象必须放行：{} {}", error.code, error.msg));
        }
    }

    /// 挂**已关批**（CLOSED）的对象 → 409 + 66057，报文点名对象号与状态。
    #[test]
    fn closed_cost_object_is_rejected_with_409() {
        let error = ensure_cost_object_usable(
            OUT_TYPE_ISSUE,
            Some(uuid::Uuid::now_v7()),
            Some(&object_no(super::super::cost_object_dto::STATUS_CLOSED)),
        )
        .expect_err("已关批对象必须被拒");
        assert_eq!(error.status, 409, "业务规则拒绝用 409（不是参数错）");
        assert_eq!(error.code, error_code::ERP_COST_OBJECT_NOT_OPEN);
        assert_eq!(error.code, 66057);
        assert!(
            error.msg.contains("CB-20261110-000001"),
            "报文必须点名成本对象：{}",
            error.msg
        );
        assert!(error.msg.contains("已关批"), "报文必须说清状态：{}", error.msg);
        println!("[B1-10] 已关批对象被拦：{} {}", error.code, error.msg);
    }

    /// 挂**已作废**（CANCELLED）的对象 → 同样 409 + 66057。
    #[test]
    fn cancelled_cost_object_is_rejected_with_409() {
        let error = ensure_cost_object_usable(
            OUT_TYPE_ISSUE,
            Some(uuid::Uuid::now_v7()),
            Some(&object_no(super::super::cost_object_dto::STATUS_CANCELLED)),
        )
        .expect_err("已作废对象必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_OBJECT_NOT_OPEN);
        assert!(error.msg.contains("已作废"), "{}", error.msg);
    }

    /// 对象不存在 → 400 + 66002（弱引用缺失，与「状态不对」分开）。
    #[test]
    fn missing_cost_object_is_rejected_with_400() {
        let error = ensure_cost_object_usable(OUT_TYPE_ISSUE, Some(uuid::Uuid::now_v7()), None)
            .expect_err("不存在的对象必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_REFERENCE_MISSING);
        assert!(error.msg.contains("不存在或已删除"), "{}", error.msg);
    }

    /// 非领用单挂对象 → 400（报废 / 盘亏是资产损失，进去也归集不到料）。
    #[test]
    fn non_issue_doc_cannot_attach_cost_object() {
        let object = object_no(super::super::cost_object_dto::STATUS_OPEN);
        for out_type in ["SCRAP", "LOSS", "OTHER"] {
            let error = ensure_cost_object_usable(out_type, Some(uuid::Uuid::now_v7()), Some(&object))
                .expect_err("非领用单挂对象必须被拒");
            assert_eq!(error.status, 400, "{out_type}");
            assert_eq!(error.code, error_code::PARAM_INVALID);
        }
    }

    /// 入参三态：空串 / 空白 = 不挂；非法 UUID = 400；合法 = `Some`。
    #[test]
    fn cost_object_id_value_handles_three_states() {
        let mut req = CreateStockOutReq::default();
        assert_eq!(req.cost_object_id_value().expect("缺省"), None);
        req.cost_object_id = Some("   ".to_string());
        assert_eq!(req.cost_object_id_value().expect("空白"), None);
        req.cost_object_id = Some("not-a-uuid".to_string());
        assert_eq!(req.cost_object_id_value().expect_err("非法").status, 400);
        let id = uuid::Uuid::now_v7();
        req.cost_object_id = Some(id.to_string());
        assert_eq!(req.cost_object_id_value().expect("合法"), Some(id));
    }

    /// 出参的借方科目**复算**：挂对象 → 4001.01；不挂 → 原逻辑（6401 / 1901）。
    #[test]
    fn response_debit_account_reflects_cost_object() {
        let mut row = StockOutRow {
            id: uuid::Uuid::now_v7(),
            out_no: "OUT-TEST-000001".to_string(),
            out_type: OUT_TYPE_ISSUE.to_string(),
            warehouse_id: uuid::Uuid::now_v7(),
            out_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 10).expect("日期"),
            status: STATUS_DRAFT.to_string(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::ZERO,
            voucher_id: None,
            reason: None,
            remark: None,
            posted_at: None,
            posted_by: None,
            cancelled_at: None,
            cancelled_by: None,
            cancel_reason: None,
            source_type: None,
            source_id: None,
            cost_object_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
        };
        let plain = StockOutRes::of(&row);
        assert_eq!(plain.debit_account_code, "6401", "不挂对象沿用原逻辑");
        assert_eq!(plain.cost_object_id, None);
        assert_eq!(plain.cost_object_no, None);

        row.cost_object_id = Some(uuid::Uuid::now_v7());
        let attached = StockOutRes::of(&row);
        assert_eq!(attached.debit_account_code, "4001.01", "挂对象 → 生产成本-直接材料");
        assert_eq!(attached.debit_account_name, "生产成本-直接材料");
        assert!(attached.cost_object_id.is_some());

        // 报废单挂了对象也不改借方（1901 是资产损失）
        row.out_type = "SCRAP".to_string();
        assert_eq!(StockOutRes::of(&row).debit_account_code, "1901");
    }

    /// 出参 JSON 的**键名**就是前端契约（B1-12 要直接读这 4 个字段）。
    ///
    /// `serde` 默认按字段名序列化，改字段名等于改契约 —— 这条钉住键名，避免
    /// 「后端改了名、前端拿到 null、界面上借方科目提示永远不变」这类静默漂移。
    #[test]
    fn response_json_exposes_cost_object_and_debit_keys() {
        let mut row = StockOutRow {
            id: uuid::Uuid::now_v7(),
            out_no: "OUT-TEST-000002".to_string(),
            out_type: OUT_TYPE_ISSUE.to_string(),
            warehouse_id: uuid::Uuid::now_v7(),
            out_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 10).expect("日期"),
            status: STATUS_DRAFT.to_string(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::ZERO,
            voucher_id: None,
            reason: None,
            remark: None,
            posted_at: None,
            posted_by: None,
            cancelled_at: None,
            cancelled_by: None,
            cancel_reason: None,
            source_type: None,
            source_id: None,
            cost_object_id: Some(uuid::Uuid::now_v7()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
        };
        let value = serde_json::to_value(StockOutRes::of(&row)).expect("序列化");
        for key in [
            "cost_object_id",
            "cost_object_no",
            "debit_account_code",
            "debit_account_name",
        ] {
            assert!(value.get(key).is_some(), "出参 JSON 缺键 {key}：{value}");
        }
        assert_eq!(value["debit_account_code"], "4001.01");
        // 未挂时 cost_object_id 是 null（不是空串 —— 前端 `if (!id)` 两种都能判，但契约要稳）
        row.cost_object_id = None;
        let value = serde_json::to_value(StockOutRes::of(&row)).expect("序列化");
        assert!(value["cost_object_id"].is_null());
    }
}
