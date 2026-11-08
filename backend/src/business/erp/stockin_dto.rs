//! 收货入库单接口契约（批次 D · P2-B1）：建单 / 改单 / 状态流转（过账）/ 详情 / 分页。
//!
//! 与 [`crate::business::erp::dto`] / [`crate::business::erp::purchase_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 日期 `NaiveDate`（`YYYY-MM-DD`）、金额/数量 `Decimal`（**JSON 里是字符串**）、
//! 状态一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 金额只由服务端算
//!
//! [`StockInItemReq`] **没有** `amount` / `tax_amount` / `with_tax_amount` / `cost_price` 字段，
//! serde 默认忽略未知字段 ⇒ 客户端硬塞金额会被静静丢掉（单测钉住这条）。
//! 口径固定为**不含税**（卡面 body 只有 `price_excl`）：`amount = round(qty × price_excl, 2)`、
//! `tax = round(amount × rate/100, 2)`、`with_tax = amount + tax`（精确恒等，
//! 否则 `ck_biz_si_item_tax` 拒写）。
//!
//! # 校验边界
//!
//! | 规则 | 判定 | 依据 |
//! |---|---|---|
//! | 数量 > 0（草稿也不允许 0） | 400 | `ck_biz_si_item_qty CHECK (qty > 0)` |
//! | 单价 ≥ 0（允许 0 = 赠品），禁负 | 400 | — |
//! | 税率 0–100 | 400 | `ck_biz_si_item_rate` |
//! | 明细 1–200 行 | 400 | 防误传超大数组 |
//! | `serial_ids` 非空时**条数必须 = qty**（且 qty 为整数） | 400 | 卡面 ⑩ |
//! | `serial_ids` 单行内不得重复 / 不得空白 / ≤ 64 字 | 400 | `biz_material_serial.serial_no` 唯一键 |
//! | `source_order_id` 有值时，每行必须带 `purchase_order_item_id` | 400 | 过账要按采购**明细行**判超收（P2-3：已收数量由已过账收货单派生，不带上就无从判定） |
//! | **有源单的 `supplier_id` 必须等于采购单的**（可省略，服务端派生） | 400 `66028` | A4 / 设计 §6.2「唯一取值点」：入库单绑采购单，不绑供应商 |
//! | **PURCHASE 无来源单且未声明 `is_sourceless=true`** | 400 `66027` | A4 / AP-4：无源属 E4 例外，必须显式 |
//! | **无源单缺 `remark`（来源说明）** | 400 `66029` | A4 / AP-5：无源要可枚举、能进巡检，也要能被人看懂 |
//!
//! # 权限码（与 `backend/migrations/business/V0147__erp_stock_in_permissions.up.sql` 逐字一致）
//!
//! 6 个码：`list` / `detail` / `create` / `update` / `delete` / `status`。
//! `:status` 独立于 `:update`：**过账会加库存**，是比「改几行明细」重得多的授权。

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::approval_gate::SkipApprovalReq;
use crate::business::erp::stockin_model::{
    STOCK_IN_SUMMARY_PLACEHOLDER, StockInFilter, StockInItemRow, StockInRow, Totals, money,
    unit_price,
};
use crate::shared::error_code;

/// 明细行上限。
pub const MAX_LINES: usize = 200;

/// 备注最大长度（`remark TEXT`，业务口径 500 字）。
pub const MAX_REMARK_LEN: usize = 500;

/// 作废原因最大长度（列 `cancel_reason VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;

/// 单个序列号最大长度（列 `serial_no VARCHAR(64)`）。
pub const MAX_SERIAL_LEN: usize = 64;

/// 本卡的 6 个权限码。
pub const STOCK_IN_PERMISSIONS: [(&str, &str); 6] = [
    ("business:erp:stock-in:list", "收货入库单"),
    ("business:erp:stock-in:detail", "收货入库单详情（含明细）"),
    ("business:erp:stock-in:create", "新建收货入库单"),
    ("business:erp:stock-in:update", "编辑收货入库单（仅草稿）"),
    ("business:erp:stock-in:delete", "删除收货入库单（仅草稿）"),
    (
        "business:erp:stock-in:status",
        "收货入库单状态流转（过账 / 作废）",
    ),
];

/// 状态流转请求里允许的 2 个目标态（卡面 §二）。
pub const TRANSITION_TARGETS: [&str; 2] = ["POSTED", "CANCELLED"];

/// 今天（北京时间）的日期。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

/// 400 快捷构造（文案即 `msg`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 入参 ====================

/// 建单 / 改单请求（改单复用同一形状：整单覆盖）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateStockInReq {
    /// 来源采购单（可空；有值时每行必须带 `purchase_order_item_id`）。
    pub source_order_id: Option<String>,
    /// 来源采购单 id（**卡面 D4 的字段名**；与 [`Self::source_order_id`] 是同一个东西）。
    ///
    /// 为什么留两个名字：库里列叫 `source_order_id`（V0145 建表），而「采购订单 → 收货单」
    /// 的调用方按业务语义叫它 `purchase_order_id`。两个都给时**必须相等**，
    /// 不等直接 400（静默挑一个会让「带错单」这种事故无声无息）。
    pub purchase_order_id: Option<String>,
    /// 供应商（**快照，不由用户决定**）。
    ///
    /// * 有来源采购单（`source_order_id` 非空）：**可以不传** —— 服务端从
    ///   `biz_purchase_order.supplier_id` 派生；传了且与采购单不一致 → 400
    ///   （不静默覆盖：静默改值会让「带错单」这类事故无声无息）；
    /// * 无来源采购单（期初 / 复制 / 其他）：**必填**，且必须同时 `is_sourceless = true`。
    pub supplier_id: Option<String>,
    /// 无源单据标记（AP-5：期初 / 复制 / 其他）。
    ///
    /// 入库必须绑定采购单；确实没有来源单的场景（期初余额、复制为新单、其他）**必须显式声明
    /// 本字段为 `true`**，并填 `remark` 说明原因 —— 不能靠「`source_order_id` 为空」隐式表达，
    /// 否则「无源」既不可枚举也进不了巡检。有来源单时给 `true` → 400。
    pub is_sourceless: Option<bool>,
    /// 收货仓库（必填，列 `NOT NULL`）。
    pub warehouse_id: Option<String>,
    /// 入库日期（缺省 = 今天，北京时间）。
    pub in_date: Option<NaiveDate>,
    /// 发票是否已到（缺省 true，与列默认值一致）。
    pub invoice_received: Option<bool>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（可为空 **当且仅当** 带 `source_order_id`：此时按采购单未收数量自动带出）。
    pub items: Vec<StockInItemReq>,
    /// **只读派生字段**：数量合计。服务端由明细算出，客户端**不能**用它改账。
    ///
    /// 之所以显式收下而不是当成未知字段丢掉：`serde(default)` 会把
    /// `{"total_amount": 999}` 静默解析成「什么都没改」，于是一个「改金额」的请求
    /// 会返回 200（P10 实测：卡面 §五-3 的验收场景当场假绿）。收下来才能
    /// 在入账态给出 409「单据已入账」，在草稿态给出 400「金额由明细派生」。
    pub total_qty: Option<Decimal>,
    /// **只读派生字段**：不含税金额合计（同上）。
    pub total_amount: Option<Decimal>,
    /// **只读派生字段**：税额合计（同上）。
    pub total_tax: Option<Decimal>,
}

/// 建单明细行。
///
/// ⚠️ 没有金额字段：金额一律服务端算。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockInItemReq {
    /// 来源采购明细行（列 `source_item_id`；卡面写作 `purchase_order_item_id`）。
    pub purchase_order_item_id: Option<String>,
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 数量（> 0，4 位标度）。
    pub qty: Option<Decimal>,
    /// 不含税单价（6 位标度；含税单价由 `price_excl × (1+税率)` 派生快照）。
    pub price_excl: Option<Decimal>,
    /// 税率（百分数 0–100，缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 库位（`biz_inventory_location.id`，可空；**兼容旧口径**，新前端请用 `location_dict_id`）。
    pub location_id: Option<String>,
    /// 用户选的库位（`biz_location.id`，可空；P2-4）。
    ///
    /// - 给了：必须属于本单收货仓库、未软删、已启用（否则 400/404）；
    /// - 不给：过账时自动用该仓库的默认库位（`is_default = 1`），没有默认则进
    ///   「未分配」桶并在详情里给出 `location_warnings` 提示。
    pub location_dict_id: Option<String>,
    /// 单品序列号（一物一码物料：非空时**必须**与 `qty` 等长，空则过账时自动取号）。
    pub serial_ids: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockInStatusReq {
    /// 目标状态（`POSTED` / `CANCELLED`）。
    pub status: Option<String>,
    /// 原因（`POSTED → CANCELLED` 必填）。
    pub reason: Option<String>,
    /// 过账时按行号补的单品序列号（**一物一码物料用自己贴的码时走这里**）。
    ///
    /// 空 = 由系统按 `SN-yyyyMMdd-NNNNNN` 取号生成（卡面 ③ 的默认路径）。
    /// 明细表中没有存序列号的列（红线 5 不许改 V0137-V0145），所以自定义码只能在过账时提交。
    pub serials: Vec<StockInLineSerials>,
    /// 「显式跳过审批」片段（ARCH-3 DC-01）：`skip_approval` + `skip_reason`。
    #[serde(flatten, default)]
    pub approval: SkipApprovalReq,
}

/// 一行的单品序列号（过账入参）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockInLineSerials {
    /// 行号（与本单明细的 `line_no` 对应，从 1 起）。
    pub line_no: i32,
    /// 序列号（条数必须等于该行 qty）。
    pub serial_ids: Vec<String>,
}

impl StockInStatusReq {
    /// 目标态（白名单外的值 → 400）。
    pub fn target_status(&self) -> std::result::Result<&str, ApiError> {
        let raw = self.status.as_deref().map(str::trim).unwrap_or_default();
        if raw.is_empty() {
            return Err(bad_request("status 不能为空（POSTED / CANCELLED）"));
        }
        if !TRANSITION_TARGETS.contains(&raw) {
            return Err(bad_request(format!(
                "status = `{raw}` 不在允许的目标态里：{}",
                TRANSITION_TARGETS.join(" / ")
            )));
        }
        Ok(raw)
    }

    /// 原因（去空白；空串 → `None`）。
    pub fn reason_text(&self) -> Option<&str> {
        self.reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

/// 列表筛选（Query 反序列化，与 `dto::PurchaseOrderFilter` 同形状）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StockInPageQuery {
    /// 关键词（单号 / 备注）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 收货仓库。
    pub warehouse_id: Option<String>,
    /// 来源采购单号（模糊匹配，卡面 O3：收货单列表「按采购单号」筛选）。
    pub purchase_order_no: Option<String>,
    /// 入库日期起（含）。
    pub date_from: Option<NaiveDate>,
    /// 入库日期止（含）。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl StockInPageQuery {
    /// 转成数据层筛选条件。
    pub fn to_filter(&self) -> StockInFilter {
        StockInFilter {
            keyword: self.keyword.clone(),
            status: self.status.clone(),
            supplier_id: self.supplier_id.clone(),
            warehouse_id: self.warehouse_id.clone(),
            purchase_order_no: self
                .purchase_order_no
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            date_from: self.date_from,
            date_to: self.date_to,
            page: self.page,
            page_size: self.page_size,
        }
    }
}

// ==================== 校验后的中间形态 ====================

/// 校验通过、金额已算好的一行明细（只差 `unit_id` / `serial_required`，由 handler 查物料补上）。
#[derive(Debug, Clone)]
pub struct PreparedLine {
    /// 物料 id 原文。
    pub material_id_raw: String,
    /// 来源采购明细行 id 原文（空 = 无）。
    pub source_item_id_raw: Option<String>,
    /// 数量（4 位）。
    pub qty: Decimal,
    /// 不含税单价（6 位）。
    pub price_excl: Decimal,
    /// 含税单价（6 位，派生快照）。
    pub price_incl: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 不含税金额（4 位）。
    pub amount: Decimal,
    /// 税额（4 位）。
    pub tax_amount: Decimal,
    /// 价税合计（4 位）。
    pub with_tax_amount: Decimal,
    /// 库位账行 id 原文（旧口径，可为空）。
    pub location_id_raw: Option<String>,
    /// 用户选的库位（`biz_location.id`）原文，可为空。
    pub location_dict_id_raw: Option<String>,
    /// 单品序列号。
    pub serial_ids: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 校验通过、金额已算好的整单。
#[derive(Debug, Clone)]
pub struct PreparedStockIn {
    /// 来源采购单 id 原文。
    pub source_order_id_raw: Option<String>,
    /// 供应商 id 原文（**可空**：有源单时由采购单派生，见
    /// [`crate::business::erp::stockin_handler`] 的 `resolve_parent_pointer`）。
    pub supplier_id_raw: Option<String>,
    /// 仓库 id 原文。
    pub warehouse_id_raw: String,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 发票是否已到。
    pub invoice_received: bool,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（顺序即 `line_no`）。
    pub lines: Vec<PreparedLine>,
    /// 单据汇总。
    pub totals: TotalsSummary,
}

/// 汇总（避免 DTO 层依赖 `stockin_model::NewStockInLine`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TotalsSummary {
    /// 合计数量。
    pub qty: Decimal,
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计。
    pub with_tax: Decimal,
}

impl TotalsSummary {
    /// 转数据层汇总（过账 / 建单共用同一组数）。
    pub fn to_totals(&self) -> Totals {
        Totals {
            qty: self.qty,
            amount: self.amount,
            tax: self.tax,
            with_tax: self.with_tax,
        }
    }
}

/// 金额一律四舍五入到 2 位（与 PG `ROUND(numeric, 2)` 同口径）。
fn money_of(value: Decimal) -> Decimal {
    money(value)
}

/// 单价一律 6 位。
fn unit_price_of(value: Decimal) -> Decimal {
    unit_price(value)
}

impl CreateStockInReq {
    /// 校验 + 算金额（**纯函数**：不做任何数据库查询，单测可直接跑）。
    ///
    /// `allow_empty_items` 由 handler 传：带 `source_order_id` 时允许空明细（按采购单未收数量自动带出）。
    pub fn prepare(
        &self,
        allow_empty_items: bool,
    ) -> std::result::Result<PreparedStockIn, ApiError> {
        // 供应商不再在这里强制：有源单由采购单派生（`resolve_parent_pointer`），
        // 无源单在同一个函数里强制 + 要求显式无源标记。DTO 层只负责「原样带出」。
        let supplier_id_raw = self
            .supplier_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let warehouse_id_raw = require_text(self.warehouse_id.as_deref(), "warehouse_id")?;

        let in_date = self.in_date.unwrap_or_else(today_cst);
        let remark = self
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        if let Some(text) = remark.as_deref()
            && text.chars().count() > MAX_REMARK_LEN
        {
            return Err(bad_request(format!(
                "remark 超过 {MAX_REMARK_LEN} 字（当前 {} 字）",
                text.chars().count()
            )));
        }

        let source_order_id_raw = self
            .source_order_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);

        if self.items.is_empty() && !allow_empty_items {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        }
        if self.items.len() > MAX_LINES {
            return Err(bad_request(format!(
                "items 最多 {MAX_LINES} 行（当前 {} 行）",
                self.items.len()
            )));
        }

        let mut lines = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            lines.push(item.prepare(index, source_order_id_raw.is_some())?);
        }

        let totals = TotalsSummary {
            qty: lines.iter().fold(Decimal::ZERO, |acc, line| acc + line.qty),
            amount: lines
                .iter()
                .fold(Decimal::ZERO, |acc, line| acc + line.amount),
            tax: lines
                .iter()
                .fold(Decimal::ZERO, |acc, line| acc + line.tax_amount),
            with_tax: lines
                .iter()
                .fold(Decimal::ZERO, |acc, line| acc + line.with_tax_amount),
        };

        Ok(PreparedStockIn {
            source_order_id_raw,
            supplier_id_raw,
            warehouse_id_raw,
            in_date,
            invoice_received: self.invoice_received.unwrap_or(true),
            remark,
            lines,
            totals,
        })
    }
}

impl StockInItemReq {
    /// 单行校验 + 算金额。`index` 从 0 起，文案里按「第 N 行」展示。
    fn prepare(
        &self,
        index: usize,
        with_source_order: bool,
    ) -> std::result::Result<PreparedLine, ApiError> {
        let material_id_raw = require_text(
            self.material_id.as_deref(),
            &format!("items[{index}].material_id"),
        )?;
        let qty = self
            .qty
            .ok_or_else(|| bad_request(format!("items[{index}].qty 不能为空")))?;
        if qty <= Decimal::ZERO {
            return Err(bad_request(format!(
                "items[{index}].qty = {qty} 不合法：收货数量必须 > 0（库约束 ck_biz_si_item_qty，\
                 草稿也不允许 0 数量的明细行）"
            )));
        }
        let qty = qty.round_dp(4);
        let price_excl = self
            .price_excl
            .ok_or_else(|| bad_request(format!("items[{index}].price_excl 不能为空")))?;
        if price_excl < Decimal::ZERO {
            return Err(bad_request(format!(
                "items[{index}].price_excl = {price_excl} 不能为负（赠品请填 0）"
            )));
        }
        let price_excl = unit_price_of(price_excl);
        let tax_rate = self.tax_rate.unwrap_or(Decimal::ZERO);
        if tax_rate < Decimal::ZERO || tax_rate > Decimal::ONE_HUNDRED {
            return Err(bad_request(format!(
                "items[{index}].tax_rate = {tax_rate} 不在 0–100 之间（百分数：13 表示 13%）"
            )));
        }

        // 不含税口径：amount → tax（按价款计）→ with_tax = amount + tax（精确恒等）
        let amount = money_of(qty * price_excl);
        let tax_amount = money_of(amount * tax_rate / Decimal::ONE_HUNDRED);
        let with_tax_amount = amount + tax_amount;
        let price_incl =
            unit_price_of(price_excl * (Decimal::ONE + tax_rate / Decimal::ONE_HUNDRED));

        let source_item_id_raw = self
            .purchase_order_item_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        if with_source_order && source_item_id_raw.is_none() {
            return Err(bad_request(format!(
                "items[{index}].purchase_order_item_id 不能为空：带 source_order_id 的收货单\
                 每行必须指明对应的采购明细行（过账要回写该行的已收数量，否则无法判定超收）"
            )));
        }

        let location_id_raw = self
            .location_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let location_dict_id_raw = self
            .location_dict_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);

        let serial_ids = normalize_serials(index, &self.serial_ids, qty)?;

        Ok(PreparedLine {
            material_id_raw,
            source_item_id_raw,
            qty,
            price_excl,
            price_incl,
            tax_rate,
            amount,
            tax_amount,
            with_tax_amount,
            location_id_raw,
            location_dict_id_raw,
            serial_ids,
            remark: self
                .remark
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        })
    }
}

/// 序列号规整 + 校验（卡面 ⑩：条数必须与数量相等）。
fn normalize_serials(
    index: usize,
    raw: &[String],
    qty: Decimal,
) -> std::result::Result<Vec<String>, ApiError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let cleaned: Vec<String> = raw
        .iter()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    if cleaned.len() != raw.len() {
        return Err(bad_request(format!(
            "items[{index}].serial_ids 里有空白的序列号：{} 条输入里只有 {} 条非空",
            raw.len(),
            cleaned.len()
        )));
    }
    if let Some(long) = cleaned
        .iter()
        .find(|serial_no| serial_no.chars().count() > MAX_SERIAL_LEN)
    {
        return Err(bad_request(format!(
            "items[{index}].serial_ids 里的序列号超过 {MAX_SERIAL_LEN} 字：{}",
            long.chars().count()
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(cleaned.len());
    if let Some(dup) = cleaned.iter().find(|serial_no| !seen.insert(*serial_no)) {
        return Err(bad_request(format!(
            "items[{index}].serial_ids 里序列号重复：`{dup}`（同一行里一个码只能用一次）"
        )));
    }
    // 数量必须是整数（一件一码），且与号码条数严格相等（卡面 ⑩）
    let integral: Option<i64> = qty.try_into().ok();
    let Some(count) = integral else {
        return Err(bad_request(format!(
            "items[{index}].qty = {qty} 不是整数：一物一码物料按件入库，数量必须是整件数"
        )));
    };
    if count != i64::try_from(cleaned.len()).unwrap_or(-1) {
        return Err(bad_request(format!(
            "items[{index}] 的序列号条数与数量不符：qty = {qty}（{count} 件），\
             但 serial_ids 给了 {} 条 —— 一件一码要求两者严格相等",
            cleaned.len()
        )));
    }
    Ok(cleaned)
}

/// 必填文本（去空白后为空 → 400）。
fn require_text(raw: Option<&str>, field: &str) -> std::result::Result<String, ApiError> {
    raw.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad_request(format!("{field} 不能为空")))
}

// ==================== 出参 ====================

/// 收货入库单（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct StockInRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub in_no: String,
    /// 入库类型。
    pub in_type: String,
    /// 来源采购单。
    pub source_order_id: Option<String>,
    /// 来源采购单**单号**（`PO-yyyyMMdd-000001`；无来源 / 采购单已删 = `None`）。
    ///
    /// 列表与详情都要显示「来源采购单」，前端不该为了一个单号再发 N 次请求：
    /// 列表走一次 `IN (...)` 批量补齐（`stockin_model::order_nos_of`），详情单查一次。
    pub source_order_no: Option<String>,
    /// 供应商（快照）。
    pub supplier_id: Option<String>,
    /// 供应商名称（读主数据：`biz_supplier` 按 `supplier_id` 快照列取名，**不 JOIN 采购单**）。
    ///
    /// R3-a：列表口径要冻结 —— 显示的是「这次收货时的供应商」，不是「采购单现在填的供应商」。
    pub supplier_name: Option<String>,
    /// 无源单据标记（`true` = 期初 / 复制 / 其他，没有来源采购单）。
    pub is_sourceless: bool,
    /// 收货仓库。
    pub warehouse_id: String,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
    /// 币种。
    pub currency_code: String,
    /// 合计数量。
    pub total_qty: Decimal,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 发票是否已到。
    pub invoice_received: bool,
    /// 凭证（P2-C1 才写，本卡恒 `None`）。
    pub voucher_id: Option<String>,
    /// 应付单（P2-C1 才写，本卡恒 `None`）。
    pub payable_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 过账人。
    pub posted_by: Option<String>,
    /// 作废时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 作废人。
    pub cancelled_by: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 作废审批实例 id（P16；只有「作废审批中」非空，前端据此显示审批进度时间线）。
    pub approval_instance_id: Option<String>,
    /// 创建人（P16 新增：内控要求「作废复核人 ≠ 制单人」，前端据此禁用自审提示）。
    pub create_by: Option<String>,
    /// 制单人姓名（**AUD-02**：实时取现值；`None` = 制单人为空 / 账号已不存在）。
    ///
    /// 列表与详情都由 handler 的 `fill_person_names` 批量回填（一页恒定 1 条 SQL）。
    pub create_by_name: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&StockInRow> for StockInRes {
    fn from(row: &StockInRow) -> Self {
        Self {
            id: row.id.to_string(),
            in_no: row.in_no.clone(),
            in_type: row.in_type.clone(),
            source_order_id: row.source_order_id.map(|v| v.to_string()),
            source_order_no: None,
            supplier_id: row.supplier_id.map(|v| v.to_string()),
            supplier_name: None,
            is_sourceless: row.is_sourceless,
            warehouse_id: row.warehouse_id.to_string(),
            in_date: row.in_date,
            status: row.status.clone(),
            status_label: crate::business::erp::stockin_model::status_label(&row.status)
                .to_string(),
            currency_code: row.currency_code.clone(),
            total_qty: row.total_qty,
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            invoice_received: row.invoice_received,
            voucher_id: row.voucher_id.map(|v| v.to_string()),
            payable_id: row.payable_id.map(|v| v.to_string()),
            remark: row.remark.clone(),
            posted_at: row.posted_at,
            posted_by: row.posted_by.map(|v| v.to_string()),
            cancelled_at: row.cancelled_at,
            cancelled_by: row.cancelled_by.map(|v| v.to_string()),
            cancel_reason: row.cancel_reason.clone(),
            approval_instance_id: row.approval_instance_id.map(|v| v.to_string()),
            create_by: row.create_by.map(|v| v.to_string()),
            create_by_name: None,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// AUD-02：制单人姓名回填（`create_by` → `create_by_name`）。
impl crate::business::shared::user_name::PersonNameRow for StockInRes {
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

/// 收货入库单明细。
#[derive(Debug, Clone, Serialize)]
pub struct StockInItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 来源采购明细行。
    pub source_item_id: Option<String>,
    /// 物料。
    pub material_id: String,
    /// 计量单位。
    pub unit_id: String,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 入库成本单价（不含税，过账后它就是该行的成本依据）。
    pub cost_price: Decimal,
    /// 库位账行（`biz_inventory_location.id`，未过账时可能为空）。
    pub location_id: Option<String>,
    /// 用户选的库位（`biz_location.id`，P2-4；为空 = 未指定）。
    pub location_dict_id: Option<String>,
    /// 库位编码（`biz_location.location_code`，详情接口按 `location_dict_id` 译名）。
    pub location_code: Option<String>,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 批次号。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl From<&StockInItemRow> for StockInItemRes {
    fn from(row: &StockInItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            source_item_id: row.source_item_id.map(|v| v.to_string()),
            material_id: row.material_id.to_string(),
            unit_id: row.unit_id.to_string(),
            qty: row.qty,
            price: row.price,
            price_incl: row.price_incl,
            tax_rate: row.tax_rate,
            amount: row.amount,
            tax_amount: row.tax_amount,
            with_tax_amount: row.with_tax_amount,
            cost_price: row.cost_price,
            location_id: row.location_id.map(|v| v.to_string()),
            location_dict_id: row.location_dict_id.map(|v| v.to_string()),
            location_code: None,
            location_name: None,
            lot_no: row.lot_no.clone(),
            remark: row.remark.clone(),
        }
    }
}

/// 详情：单头 + 明细 + 本单生成的序列号件数。
#[derive(Debug, Clone, Serialize)]
pub struct StockInDetailRes {
    /// 单头。
    pub header: StockInRes,
    /// 明细（行号升序）。
    pub items: Vec<StockInItemRes>,
    /// 本单已生成的序列号件数（非一物一码单恒 0；未过账的单也恒 0）。
    pub serial_count: i64,
    /// 汇总口径说明（前端展示「金额为不含税」用）。
    pub summary: String,
    /// 来源采购单 id（= `header.source_order_id`，卡面 F3 的字段名，前端少拐一道弯）。
    pub purchase_order_id: Option<String>,
    /// 来源采购单号（前端直接显示「来源采购单：PO-xxx」并可点回采购单）。
    pub purchase_order_no: Option<String>,
    /// 来源采购单的合同号（**一跳带出**：`source_order_id → biz_purchase_order.contract_no_snapshot`）。
    ///
    /// 注意：入库单**没有** `contract_id` 列（设计 §6.4），合同在两环之上，只在详情页显示；
    /// 也不 JOIN `biz_contract`（R2.1 推论 2 / AP-6），读的是采购单上冻住的快照。
    pub contract_no: Option<String>,
    /// 库位提示（P2-4）：未分配 / 超容量 —— **只提示不拦截**。
    ///
    /// 由服务端按当前数据现算（草稿态把本单数量算进来），因此过账后再看详情
    /// 不会重复计一遍；前端把它显示成告警条即可。
    pub location_warnings: Vec<String>,
}

impl StockInDetailRes {
    /// 用占位汇总文案构造（handler 统一走这个，避免各接口文案漂移）。
    pub fn with_summary(header: StockInRes, items: Vec<StockInItemRes>, serial_count: i64) -> Self {
        Self {
            items,
            serial_count,
            summary: STOCK_IN_SUMMARY_PLACEHOLDER.to_string(),
            purchase_order_id: header.source_order_id.clone(),
            purchase_order_no: header.source_order_no.clone(),
            contract_no: None,
            location_warnings: Vec::new(),
            header,
        }
    }
}
