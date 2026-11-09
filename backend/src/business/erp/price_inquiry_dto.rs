//! 询比价契约（P2-01）：询价单建单 / 报价录入 / 定标 / 转采购订单。
//!
//! 与 [`crate::business::erp::purchase_dto`] 同一套约定：主键与外键一律 `String`
//! （`nik::uuid::Uuid` 没开 serde feature）、日期 `NaiveDate`（`YYYY-MM-DD`）、
//! 金额/数量 `Decimal`（**JSON 里是字符串**）、状态一律大写字符串且与 DDL 的
//! `CHECK` 逐字一致。
//!
//! # 金额只由服务端算
//!
//! 报价请求里**只有不含税单价** `price_excl` 与税率 `tax_rate`；含税单价
//! `price_incl` 由 [`price_incl_of`] 现算（6 位舍入）后落库 —— 客户端硬塞
//! `price_incl` 会被 serde 静态忽略（[`QuoteItemReq`] 里根本没有这个字段）。
//! 比价表要能横向比，含税价必须与不含税价同源，否则两条口径会在页面上打架。
//!
//! # 定标的三条规则（全部有纯函数 + 单测钉住）
//!
//! ```text
//! ① 每条明细至少 2 家报价才能「正常定标」；不足 2 家必须给 single_source_reason（≥10 字），
//!    并落 award_mode = 'SINGLE_SOURCE'（例外留痕，不静默放过）；
//! ② 中标组合必须与已录报价一一对上（供应商没给这条明细报价 → 409，不替它编价）；
//! ③ 中标按**明细行**表达：一条明细只有一家中标供应商，不同明细可以给不同供应商
//!    ⇒ 转采购订单时按供应商分组，一张询价单拆成多张采购单（卡面业务规则 3）。
//! ```
//!
//! # 权限码（与 `V0321__price_inquiry.up.sql` 逐字一致）
//!
//! `business:erp:price-inquiry:{list,create,quote,award,to-po}`。

use std::collections::{BTreeMap, BTreeSet};

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 域常量 ====================

/// 草稿：刚建单，还没有任何报价。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 报价中：至少录入过一家报价（首次录入自动推进）。
pub const STATUS_QUOTING: &str = "QUOTING";
/// 已定标：每条明细都有中标供应商，可以转采购订单。
pub const STATUS_AWARDED: &str = "AWARDED";
/// 已转单：已生成采购订单（幂等闸，不能再转第二次）。
pub const STATUS_CONVERTED: &str = "CONVERTED";

/// 全部状态（与 `ck_biz_piq_status` 逐字一致）。
pub const STATUSES: [&str; 4] = [STATUS_DRAFT, STATUS_QUOTING, STATUS_AWARDED, STATUS_CONVERTED];

/// 多家比价后定标。
pub const AWARD_MODE_MULTI: &str = "MULTI";
/// 单源例外（不足 2 家报价，必须留理由）。
pub const AWARD_MODE_SINGLE_SOURCE: &str = "SINGLE_SOURCE";

/// 定标所需的最少报价家数。
pub const MIN_BIDDERS: usize = 2;

/// 单源例外理由的最短长度（与 `ck_biz_piq_award_reason` 逐字一致）。
pub const MIN_AWARD_REASON_LEN: usize = 10;

/// 明细分行上限 / 报价行上限（一次提交，防超大数组）。
pub const MAX_LINES: usize = 200;

/// 备注最大长度。
pub const MAX_REMARK_LEN: usize = 500;

/// 本模块 5 个权限码（`perm_menu_gate` 要求每个都在 `sys_menu` 有承载行）。
pub const PRICE_INQUIRY_PERMISSIONS: [&str; 5] = [
    "business:erp:price-inquiry:list",
    "business:erp:price-inquiry:create",
    "business:erp:price-inquiry:quote",
    "business:erp:price-inquiry:award",
    "business:erp:price-inquiry:to-po",
];

/// 今天（北京时间）的日期。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

// ==================== 入参 ====================

/// 新建询价单。
///
/// `warehouse_id` 是卡面之外补的必填项：转采购订单时 `biz_purchase_order.warehouse_id`
/// 是 `NOT NULL`，询价单不记住收货仓库就只能让用户转单时再猜一次。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreatePriceInquiryReq {
    /// 询价日期（缺省 = 今天，北京时间）。
    pub inquiry_date: Option<NaiveDate>,
    /// 采购员（缺省 = 当前登录人）。
    pub buyer_id: Option<String>,
    /// 收货仓库（必填）。
    pub warehouse_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（不能为空）。
    pub items: Vec<PriceInquiryItemReq>,
}

/// 询价明细行（数量 > 0；单位由物料带出，客户端不许传）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PriceInquiryItemReq {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 询价数量（> 0）。
    pub qty: Option<Decimal>,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PriceInquiryPageQuery {
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
    /// 状态精确过滤。
    pub status: Option<String>,
    /// 关键字：询价单号 / 备注 / 采购员 / 供应商名。
    pub keyword: Option<String>,
    /// 询价日期起（含）。
    pub date_from: Option<NaiveDate>,
    /// 询价日期止（含）。
    pub date_to: Option<NaiveDate>,
}

/// 数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct PriceInquiryFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
    /// 日期起。
    pub date_from: Option<NaiveDate>,
    /// 日期止。
    pub date_to: Option<NaiveDate>,
}

impl PriceInquiryPageQuery {
    /// 入参校验 + 归一化（域值乱码一律 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<PriceInquiryFilter, ApiError> {
        let status = match self.status.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(raw) => Some(normalize_status(raw)?),
            None => None,
        };
        if let (Some(from), Some(to)) = (self.date_from, self.date_to)
            && from > to
        {
            return Err(bad_request(format!("date_from（{from}）不能晚于 date_to（{to}）")));
        }
        Ok(PriceInquiryFilter {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
            status,
            keyword: self
                .keyword
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            date_from: self.date_from,
            date_to: self.date_to,
        })
    }
}

/// 录入一家供应商对若干明细的报价（同一供应商可一次提交全部明细）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QuoteReq {
    /// 供应商（必填）。
    pub supplier_id: Option<String>,
    /// 报价行（不能为空；重复录入同一条明细 = 改价）。
    pub items: Vec<QuoteItemReq>,
}

/// 单条报价。
///
/// ⚠️ 没有 `price_incl`：含税单价一律服务端算（见模块文档）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QuoteItemReq {
    /// 询价明细 id（必填）。
    pub item_id: Option<String>,
    /// 不含税单价（≥ 0，允许 0 = 赠送/Low-ball 报价）。
    pub price_excl: Option<Decimal>,
    /// 税率（0–100，缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 交期天数（≥ 0，可选）。
    pub delivery_days: Option<i32>,
    /// 承诺到货日（可选）。
    pub delivery_date: Option<NaiveDate>,
    /// 报价备注。
    pub remark: Option<String>,
}

/// 定标请求。
///
/// 两种写法等价：
/// * `{ "supplier_id": "…" }` —— 全部明细都给这一家（最常见）；
/// * `{ "items": [{"item_id": "…", "supplier_id": "…"} …] }` —— 按明细分别定标
///   （多供应商分组，转单时拆成多张采购单）。
///
/// 两种都传时**以 `items` 为准**（显式明细定标更精确），`supplier_id` 被忽略。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AwardReq {
    /// 单一供应商定标（快捷写法）。
    pub supplier_id: Option<String>,
    /// 按明细定标。
    pub items: Vec<AwardItemReq>,
    /// 单源例外理由（某条明细不足 2 家报价时必填，≥10 字）。
    pub single_source_reason: Option<String>,
}

/// 按明细定标的一行。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AwardItemReq {
    /// 询价明细 id。
    pub item_id: Option<String>,
    /// 中标供应商 id。
    pub supplier_id: Option<String>,
}

/// 转采购订单请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToPoReq {
    /// 下单日期（缺省 = 今天，北京时间）。
    pub order_date: Option<NaiveDate>,
    /// 收货仓库覆盖（缺省 = 询价单头上的仓库）。
    pub warehouse_id: Option<String>,
    /// 采购单备注前缀（缺省自动拼「由询价单 xxx 转单」）。
    pub remark: Option<String>,
}

// ==================== 纯逻辑（定标规则） ====================

/// 一条明细的中标结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AwardPair {
    /// 询价明细 id。
    pub item_id: Uuid,
    /// 中标供应商 id。
    pub supplier_id: Uuid,
}

/// 定标方式判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwardPlan {
    /// 逐行中标组合（按明细顺序）。
    pub pairs: Vec<AwardPair>,
    /// 报价家数不足 2 的明细（空 = 全部走多家比价）。
    pub single_source_items: Vec<Uuid>,
    /// 定标方式：`MULTI` / `SINGLE_SOURCE`。
    pub mode: &'static str,
}

/// 解析定标请求成「逐行中标组合」。
///
/// 规则：
/// * 请求必须覆盖**全部**明细行（部分定标会让转单逻辑无从判断剩余行，宁可 400）；
/// * 同一明细不能出现两次（用 `BTreeSet` 判重）；
/// * 两种写法都为空 → 400 指名要什么。
pub fn resolve_award(req: &AwardReq, item_ids: &[Uuid]) -> std::result::Result<Vec<AwardPair>, ApiError> {
    if item_ids.is_empty() {
        return Err(bad_request("询价单没有任何明细，无法定标"));
    }
    let mut pairs: Vec<AwardPair> = Vec::new();
    if !req.items.is_empty() {
        let mut seen: BTreeSet<Uuid> = BTreeSet::new();
        for (index, item) in req.items.iter().enumerate() {
            let item_id = parse_uuid_field(item.item_id.as_deref(), &format!("items[{index}].item_id"))?;
            let supplier_id = parse_uuid_field(item.supplier_id.as_deref(), &format!("items[{index}].supplier_id"))?;
            if !item_ids.contains(&item_id) {
                return Err(bad_request(format!(
                    "items[{index}].item_id（{item_id}）不属于本询价单"
                )));
            }
            if !seen.insert(item_id) {
                return Err(bad_request(format!(
                    "items[{index}].item_id（{item_id}）重复出现：一条明细只能有一个中标供应商"
                )));
            }
            pairs.push(AwardPair { item_id, supplier_id });
        }
        let covered: BTreeSet<Uuid> = pairs.iter().map(|pair| pair.item_id).collect();
        let missing: Vec<String> = item_ids
            .iter()
            .filter(|id| !covered.contains(id))
            .map(Uuid::to_string)
            .collect();
        if !missing.is_empty() {
            return Err(bad_request(format!(
                "定标必须覆盖全部明细：以下明细没有指定中标供应商：{}",
                missing.join(", ")
            )));
        }
        return Ok(pairs);
    }
    let supplier_id = parse_uuid_field(req.supplier_id.as_deref(), "supplier_id")?;
    Ok(item_ids
        .iter()
        .map(|item_id| AwardPair {
            item_id: *item_id,
            supplier_id,
        })
        .collect())
}

/// 报价家数不足 2 的明细（按 `awards` 里出现的明细判断）。
///
/// `per_item_suppliers` = 每条明细「报过价的供应商集合」（来自库，不是请求）。
pub fn single_source_items(per_item_suppliers: &BTreeMap<Uuid, BTreeSet<Uuid>>, awards: &[AwardPair]) -> Vec<Uuid> {
    awards
        .iter()
        .filter(|pair| {
            per_item_suppliers
                .get(&pair.item_id)
                .map(|set| set.len() < MIN_BIDDERS)
                .unwrap_or(true)
        })
        .map(|pair| pair.item_id)
        .collect()
}

/// 中标组合里「该供应商没给这条明细报价」的行（409 的依据，不替供应商编价）。
pub fn unmatched_awards(quoted: &BTreeSet<(Uuid, Uuid)>, awards: &[AwardPair]) -> Vec<AwardPair> {
    awards
        .iter()
        .filter(|pair| !quoted.contains(&(pair.item_id, pair.supplier_id)))
        .copied()
        .collect()
}

/// 含税单价 = 不含税单价 × (1 + 税率/100)，6 位舍入。
pub fn price_incl_of(price_excl: Decimal, tax_rate: Decimal) -> Decimal {
    let factor = Decimal::ONE + tax_rate / Decimal::from(100);
    (price_excl * factor).round_dp(6)
}

/// 税率校验：0–100（与列 CHECK 同口径）。
pub fn check_tax(raw: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    let value = raw.unwrap_or_default();
    if value.is_sign_negative() || value > Decimal::from(100) {
        return Err(bad_request(format!("tax_rate 必须在 0–100 之间（收到 {value}）")));
    }
    Ok(value.round_dp(4))
}

/// 单价校验：≥ 0，6 位标度。
pub fn check_price(price_excl: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let value = price_excl.ok_or_else(|| bad_request(format!("{field} 必填")))?;
    if value.is_sign_negative() {
        return Err(bad_request(format!("{field} 不能为负（收到 {value}）")));
    }
    Ok(value.round_dp(6))
}

/// 数量校验：> 0（草稿也不许 0，与 `ck_biz_piqi_qty` 同口径）。
pub fn check_qty(qty: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let value = qty.ok_or_else(|| bad_request(format!("{field} 必填")))?;
    if value <= Decimal::ZERO {
        return Err(bad_request(format!(
            "{field} 必须大于 0（收到 {value}）：不需要的行请直接删掉"
        )));
    }
    Ok(value.round_dp(4))
}

/// 交期天数校验：≥ 0。
pub fn check_delivery_days(raw: Option<i32>) -> std::result::Result<Option<i32>, ApiError> {
    match raw {
        Some(value) if value < 0 => Err(bad_request(format!("delivery_days 不能为负（收到 {value}）"))),
        other => Ok(other),
    }
}

/// 单源例外理由校验：给定就必须 ≥10 字（与 `ck_biz_piq_award_reason` 同口径）。
pub fn check_award_reason(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 理由是否满足单源例外的最小长度。
pub fn reason_is_long_enough(reason: Option<&str>) -> bool {
    reason
        .map(str::trim)
        .map(|text| text.chars().count() >= MIN_AWARD_REASON_LEN)
        .unwrap_or(false)
}

/// 按供应商分组（转采购订单用）：`供应商 → 明细 id 列表`。
///
/// 用 `BTreeMap` 保证**顺序确定**（同一询价单重复转单/同一断言不会因 HashMap 顺序抖动）。
pub fn group_by_supplier(pairs: &[AwardPair]) -> BTreeMap<Uuid, Vec<Uuid>> {
    let mut grouped: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
    for pair in pairs {
        grouped.entry(pair.supplier_id).or_default().push(pair.item_id);
    }
    grouped
}

/// 状态归一化（大小写与首尾空格自动收敛；未知值 400 并列出合法值）。
pub fn normalize_status(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim().to_ascii_uppercase();
    if STATUSES.contains(&text.as_str()) {
        Ok(text)
    } else {
        Err(bad_request(format!(
            "status 只能是 {}（收到 {raw}）",
            STATUSES.join(" / ")
        )))
    }
}

/// 状态中文标签。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_QUOTING => "报价中",
        STATUS_AWARDED => "已定标",
        STATUS_CONVERTED => "已转采购单",
        _ => "未知状态",
    }
}

/// 定标方式标签。
pub fn award_mode_label(mode: Option<&str>) -> &'static str {
    match mode {
        Some(AWARD_MODE_MULTI) => "多家比价",
        Some(AWARD_MODE_SINGLE_SOURCE) => "单源例外",
        _ => "未定标",
    }
}

/// 备注清洗（空白串 → `None`，超长 400）。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "remark 最长 {MAX_REMARK_LEN} 字（收到 {} 字）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

// ==================== 出参 ====================

/// 询价单列表行。
#[derive(Debug, Clone, Serialize)]
pub struct PriceInquiryRowRes {
    /// 主键。
    pub id: String,
    /// 询价单号。
    pub inquiry_no: String,
    /// 询价日期。
    pub inquiry_date: NaiveDate,
    /// 采购员 id。
    pub buyer_id: Option<String>,
    /// 采购员姓名（快照）。
    pub buyer_name: Option<String>,
    /// 收货仓库 id。
    pub warehouse_id: String,
    /// 收货仓库名（join 派生）。
    pub warehouse_name: String,
    /// 状态。
    pub status: String,
    /// 状态标签。
    pub status_label: String,
    /// 明细行数。
    pub item_count: i64,
    /// 已报价供应商家数。
    pub supplier_count: i64,
    /// 定标方式。
    pub award_mode: Option<String>,
    /// 定标方式标签。
    pub award_mode_label: String,
    /// 单一供应商定标时的中标供应商。
    pub award_supplier_id: Option<String>,
    /// 中标供应商名（join 派生）。
    pub award_supplier_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 定标时间。
    pub awarded_at: Option<String>,
    /// 已生成的采购订单（JSONB 原样透出）。
    pub po_refs: serde_json::Value,
    /// 创建时间。
    pub created_at: String,
}

/// 询价明细行。
#[derive(Debug, Clone, Serialize)]
pub struct PriceInquiryItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名。
    pub unit_name: String,
    /// 数量。
    pub qty: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 报价行。
#[derive(Debug, Clone, Serialize)]
pub struct PriceQuoteRes {
    /// 主键。
    pub id: String,
    /// 明细 id。
    pub item_id: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名。
    pub supplier_name: String,
    /// 不含税单价。
    pub price_excl: Decimal,
    /// 含税单价（服务端算）。
    pub price_incl: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 交期天数。
    pub delivery_days: Option<i32>,
    /// 承诺到货日。
    pub delivery_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
    /// 是否中标。
    pub is_awarded: bool,
}

/// 比价表的一格（同物料 × 供应商）。
#[derive(Debug, Clone, Serialize)]
pub struct CompareQuoteRes {
    /// 报价 id。
    pub quote_id: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名。
    pub supplier_name: String,
    /// 不含税单价。
    pub price_excl: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 交期天数。
    pub delivery_days: Option<i32>,
    /// 承诺到货日。
    pub delivery_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
    /// 是否本明细最低不含税价（并列都算）。
    pub is_lowest: bool,
    /// 是否中标。
    pub is_awarded: bool,
}

/// 比价表的一行（一条询价明细横向并排所有报价）。
#[derive(Debug, Clone, Serialize)]
pub struct CompareRowRes {
    /// 明细 id。
    pub item_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位名。
    pub unit_name: String,
    /// 数量。
    pub qty: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 本行报价家数。
    pub bidder_count: usize,
    /// 是否满足「至少 2 家」。
    pub has_competition: bool,
    /// 各行报价（按供应商名排序）。
    pub quotes: Vec<CompareQuoteRes>,
}

/// 询价单详情。
#[derive(Debug, Clone, Serialize)]
pub struct PriceInquiryDetailRes {
    /// 单头。
    pub head: PriceInquiryRowRes,
    /// 明细。
    pub items: Vec<PriceInquiryItemRes>,
    /// 全部报价（扁平）。
    pub quotes: Vec<PriceQuoteRes>,
    /// 比价表（横向）。
    pub comparison: Vec<CompareRowRes>,
    /// 是否可以转采购订单。
    pub can_to_po: bool,
    /// 不能转单的原因（可以转时为 `None`）。
    pub to_po_blocked_reason: Option<String>,
}

/// 转单生成的一张采购订单。
#[derive(Debug, Clone, Serialize)]
pub struct CreatedPurchaseOrderRes {
    /// 采购订单 id。
    pub po_id: String,
    /// 采购订单号。
    pub po_no: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名。
    pub supplier_name: String,
    /// 行数。
    pub line_count: usize,
    /// 价税合计。
    pub total_with_tax: Decimal,
}

/// 转采购订单结果。
#[derive(Debug, Clone, Serialize)]
pub struct ToPoRes {
    /// 询价单 id。
    pub inquiry_id: String,
    /// 询价单号。
    pub inquiry_no: String,
    /// 转单后的状态。
    pub status: String,
    /// 生成的采购订单（按供应商分组，一组一张）。
    pub purchase_orders: Vec<CreatedPurchaseOrderRes>,
}

// ==================== 小工具 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 入参里的 uuid 字符串：空 → 400 指名，非法 → 400 指名（绝不静默当没传）。
pub fn parse_uuid_field(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

// ==================== 单测（C7：`#[cfg(test)] mod tests`） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("十进制字面量")
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    // ---------- 金额 / 数量 ----------

    #[test]
    fn price_incl_is_server_side_and_rounds_to_six() {
        assert_eq!(price_incl_of(dec("10"), dec("13")), dec("11.300000"));
        assert_eq!(price_incl_of(dec("0"), dec("13")), dec("0"));
        // 100/3 类除不尽的场景：6 位舍入，不留 28 位尾巴
        assert_eq!(
            price_incl_of(dec("1"), dec("13")),
            Decimal::from_str("1.130000").unwrap()
        );
        assert_eq!(price_incl_of(dec("99.999999"), dec("0")), dec("99.999999"));
    }

    #[test]
    fn qty_must_be_positive() {
        assert!(check_qty(Some(dec("0")), "items[0].qty").is_err());
        assert!(check_qty(Some(dec("-1")), "items[0].qty").is_err());
        assert!(check_qty(None, "items[0].qty").is_err());
        assert_eq!(check_qty(Some(dec("3.5")), "items[0].qty").unwrap(), dec("3.5000"));
    }

    #[test]
    fn price_must_not_be_negative_and_tax_in_range() {
        assert!(check_price(Some(dec("-0.01")), "price_excl").is_err());
        assert_eq!(check_price(Some(dec("0")), "price_excl").unwrap(), dec("0"));
        assert!(check_tax(Some(dec("-1"))).is_err());
        assert!(check_tax(Some(dec("100.01"))).is_err());
        assert_eq!(check_tax(Some(dec("100"))).unwrap(), dec("100.0000"));
        assert_eq!(check_tax(None).unwrap(), dec("0.0000"));
    }

    #[test]
    fn delivery_days_must_not_be_negative() {
        assert!(check_delivery_days(Some(-1)).is_err());
        assert_eq!(check_delivery_days(Some(0)).unwrap(), Some(0));
        assert_eq!(check_delivery_days(None).unwrap(), None);
    }

    // ---------- 定标解析 ----------

    #[test]
    fn award_shortcut_assigns_every_line() {
        let items = vec![uuid(1), uuid(2), uuid(3)];
        let req = AwardReq {
            supplier_id: Some(uuid(9).to_string()),
            ..Default::default()
        };
        let pairs = resolve_award(&req, &items).unwrap();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().all(|pair| pair.supplier_id == uuid(9)));
        assert_eq!(pairs.iter().map(|pair| pair.item_id).collect::<Vec<_>>(), items);
    }

    #[test]
    fn award_per_line_supports_split_suppliers() {
        let items = vec![uuid(1), uuid(2)];
        let req = AwardReq {
            items: vec![
                AwardItemReq {
                    item_id: Some(uuid(1).to_string()),
                    supplier_id: Some(uuid(7).to_string()),
                },
                AwardItemReq {
                    item_id: Some(uuid(2).to_string()),
                    supplier_id: Some(uuid(8).to_string()),
                },
            ],
            ..Default::default()
        };
        let pairs = resolve_award(&req, &items).unwrap();
        let grouped = group_by_supplier(&pairs);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped.get(&uuid(7)).unwrap(), &vec![uuid(1)]);
        assert_eq!(grouped.get(&uuid(8)).unwrap(), &vec![uuid(2)]);
    }

    #[test]
    fn award_must_cover_all_lines() {
        let items = vec![uuid(1), uuid(2)];
        let req = AwardReq {
            items: vec![AwardItemReq {
                item_id: Some(uuid(1).to_string()),
                supplier_id: Some(uuid(7).to_string()),
            }],
            ..Default::default()
        };
        let error = resolve_award(&req, &items).unwrap_err();
        assert!(error.msg.contains("必须覆盖全部明细"), "实际：{}", error.msg);
    }

    #[test]
    fn award_rejects_duplicate_line_and_foreign_item() {
        let items = vec![uuid(1)];
        let dup = AwardReq {
            items: vec![
                AwardItemReq {
                    item_id: Some(uuid(1).to_string()),
                    supplier_id: Some(uuid(7).to_string()),
                },
                AwardItemReq {
                    item_id: Some(uuid(1).to_string()),
                    supplier_id: Some(uuid(8).to_string()),
                },
            ],
            ..Default::default()
        };
        assert!(resolve_award(&dup, &items).unwrap_err().msg.contains("重复出现"));

        let foreign = AwardReq {
            items: vec![AwardItemReq {
                item_id: Some(uuid(99).to_string()),
                supplier_id: Some(uuid(7).to_string()),
            }],
            ..Default::default()
        };
        assert!(
            resolve_award(&foreign, &items)
                .unwrap_err()
                .msg
                .contains("不属于本询价单")
        );
    }

    #[test]
    fn award_requires_some_target() {
        let items = vec![uuid(1)];
        let empty = AwardReq::default();
        assert!(resolve_award(&empty, &items).is_err());
        assert!(resolve_award(&empty, &[]).is_err());
    }

    // ---------- 比价规则 ----------

    #[test]
    fn single_source_detection_uses_quote_count_per_line() {
        let mut per_item: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        per_item.insert(uuid(1), [uuid(7), uuid(8)].into_iter().collect());
        per_item.insert(uuid(2), [uuid(7)].into_iter().collect());
        let awards = vec![
            AwardPair {
                item_id: uuid(1),
                supplier_id: uuid(7),
            },
            AwardPair {
                item_id: uuid(2),
                supplier_id: uuid(7),
            },
            AwardPair {
                item_id: uuid(3),
                supplier_id: uuid(7),
            },
        ];
        // 明细 2 只有 1 家、明细 3 连报价都没有 → 都算单源
        let gaps = single_source_items(&per_item, &awards);
        assert_eq!(gaps, vec![uuid(2), uuid(3)]);
    }

    #[test]
    fn unmatched_awards_flag_supplier_without_quote() {
        let quoted: BTreeSet<(Uuid, Uuid)> = [(uuid(1), uuid(7)), (uuid(2), uuid(8))].into_iter().collect();
        let awards = vec![
            AwardPair {
                item_id: uuid(1),
                supplier_id: uuid(7),
            },
            AwardPair {
                item_id: uuid(2),
                supplier_id: uuid(7),
            },
        ];
        let unmatched = unmatched_awards(&quoted, &awards);
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].item_id, uuid(2));
        assert_eq!(unmatched[0].supplier_id, uuid(7));
    }

    #[test]
    fn award_reason_needs_at_least_ten_chars() {
        assert!(!reason_is_long_enough(None));
        assert!(!reason_is_long_enough(Some("   ")));
        assert!(!reason_is_long_enough(Some("独家代理")));
        assert!(reason_is_long_enough(Some("唯一授权代理商，无第二家可报价")));
        // 中文字符按字符数算，不是字节数（10 个汉字 = 30 字节）
        assert!(reason_is_long_enough(Some("一二三四五六七八九十")));
    }

    #[test]
    fn group_by_supplier_is_deterministic() {
        let pairs = vec![
            AwardPair {
                item_id: uuid(2),
                supplier_id: uuid(8),
            },
            AwardPair {
                item_id: uuid(1),
                supplier_id: uuid(7),
            },
        ];
        let grouped = group_by_supplier(&pairs);
        let keys: Vec<Uuid> = grouped.keys().copied().collect();
        assert_eq!(keys, vec![uuid(7), uuid(8)]); // BTreeMap 按 uuid 升序
    }

    // ---------- 状态 / 文案 ----------

    #[test]
    fn status_normalization_and_labels() {
        assert_eq!(normalize_status(" draft ").unwrap(), STATUS_DRAFT);
        assert_eq!(normalize_status("converted").unwrap(), STATUS_CONVERTED);
        assert!(normalize_status("PAID").is_err());
        for status in STATUSES {
            assert_ne!(status_label(status), "未知状态");
        }
        assert_eq!(status_label(STATUS_AWARDED), "已定标");
        assert_eq!(award_mode_label(Some(AWARD_MODE_SINGLE_SOURCE)), "单源例外");
        assert_eq!(award_mode_label(None), "未定标");
    }

    #[test]
    fn page_filter_normalizes_and_rejects_bad_dates() {
        let query = PriceInquiryPageQuery {
            page: None,
            page_size: None,
            status: Some(" quoting ".into()),
            keyword: Some("  PIQ ".into()),
            date_from: Some(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()),
            date_to: Some(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
        };
        assert!(query.filter().is_err());

        let ok = PriceInquiryPageQuery {
            page: Some(2),
            page_size: Some(50),
            status: Some("quoting".into()),
            keyword: Some(" PIQ ".into()),
            date_from: None,
            date_to: None,
        };
        let filter = ok.filter().unwrap();
        assert_eq!(filter.page, 2);
        assert_eq!(filter.page_size, 50);
        assert_eq!(filter.status.as_deref(), Some(STATUS_QUOTING));
        assert_eq!(filter.keyword.as_deref(), Some("PIQ"));
    }

    #[test]
    fn remark_is_trimmed_and_capped() {
        assert_eq!(clean_remark(Some("  ")).unwrap(), None);
        assert_eq!(clean_remark(Some(" 比价 ")).unwrap(), Some("比价".into()));
        let long = "长".repeat(MAX_REMARK_LEN + 1);
        assert!(clean_remark(Some(&long)).is_err());
    }

    #[test]
    fn permissions_are_the_five_documented_codes() {
        assert_eq!(PRICE_INQUIRY_PERMISSIONS.len(), 5);
        for code in PRICE_INQUIRY_PERMISSIONS {
            assert!(code.starts_with("business:erp:price-inquiry:"));
        }
    }
}
