//! 采购订单接口契约（批次 D · P2-A2）：建单 / 改单 / 状态流转 / 详情。
//!
//! 与 [`crate::business::erp::dto`] / [`crate::business::erp::transfer_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 日期 `NaiveDate`（`YYYY-MM-DD`）、金额/数量 `Decimal`（**JSON 里是字符串**）、
//! 枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 金额只由服务端算（硬红线 2）
//!
//! [`PurchaseOrderItemReq`] **没有** `amount` / `tax_amount` / `with_tax_amount` 字段，
//! serde 默认忽略未知字段 ⇒ 客户端硬塞金额会被静静丢掉，改不了一分钱
//! （单测 `client_supplied_amounts_are_ignored` 钉住这条）。
//!
//! # 口径 A / B 的入参差异
//!
//! ```text
//! price_mode = EXCL → 每行必须给 price_excl（price_incl 由 price_excl×(1+税率) 派生快照）
//! price_mode = INCL → 每行必须给 price_incl（price_excl 由 price_incl/(1+税率) 倒算 6 位）
//! ```
//!
//! # 校验边界（PRD §3.1.6 + 卡面 §四）
//!
//! | 规则 | 判定 | 与卡面的差异 |
//! |---|---|---|
//! | 税率 0–100，允许 0 | 库 `ck_biz_po_item_tax_rate` 同口径 | — |
//! | 单价 ≥ 0（允许 0 = 赠品），禁负 | 400 | — |
//! | 数量 > 0；**草稿也不允许 0** | 400 | ⚠️ 卡面写「草稿允许 0」，但 `ck_biz_po_item_qty CHECK (qty > 0)`
//!   让 0 数量的明细行插不进库。放宽要动 V0031 的约束（硬红线 6 禁止碰 V0137-V0145），
//!   本卡**不放宽**：`qty = 0` 在建单/改单即 400，并给出「请填写数量或删除该行」的文案。 |
//! | 一张单多税率 | 允许，各行独立算 | — |
//! | supplier_id 必填 | 400 | — |
//! | warehouse_id 必填 | 400 | ⚠️ 卡面 body 里没有这个字段，但列是 `NOT NULL` 且收货要按仓入库 → 必传 |
//! | price_mode 必填 | 400 | — |
//! | 明细不能为空 / ≤ 200 行 | 400 | — |
//!
//! # 权限码（与 `backend/migrations/business/V0146__erp_purchase_order_permissions.up.sql` 逐字一致）
//!
//! 列表 / 详情沿用 V0141 已种的 `business:erp:purchase-order:list`；
//! 写侧 5 个码由 V0146 种下（`:status` 独立于 `:update`：状态流转会锁死单据）。

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::dto::PurchaseOrderRes;
use crate::business::erp::model::PurchaseOrderItemRow;
use crate::business::erp::purchase_model::{
    LineAmounts, PRICE_MODE_EXCL, PRICE_MODE_INCL, PRICE_MODES, unit_price,
};
use crate::shared::error_code;

// ==================== 口径常量 ====================

/// 明细行上限（单据头 + 明细一次提交，防误传超大数组）。
pub const MAX_LINES: usize = 200;

/// 备注最大长度（`remark TEXT`，这里按业务口径限 500 字）。
pub const MAX_REMARK_LEN: usize = 500;

/// 取消原因最大长度。
pub const MAX_REASON_LEN: usize = 255;

/// 本卡新增的 4 个权限码（列表沿用 V0141 的 `business:erp:purchase-order:list`）。
///
/// BUG-SYS-05：原 `business:erp:purchase-order:detail` 已移除 —— 采购订单**没有 GET 详情路由**
/// （详情随 `GET /business/erp/purchase-order/page` 的行返回），该码在代码里没有任何校验点，
/// 勾给角色也不会生效。
pub const PURCHASE_ORDER_PERMISSIONS: [(&str, &str); 4] = [
    ("business:erp:purchase-order:create", "新建采购订单"),
    (
        "business:erp:purchase-order:update",
        "编辑采购订单（仅草稿）",
    ),
    (
        "business:erp:purchase-order:delete",
        "删除采购订单（仅草稿）",
    ),
    ("business:erp:purchase-order:status", "采购订单状态流转"),
];

/// 状态流转请求里允许的 3 个目标态（卡面 §三）。
pub const TRANSITION_TARGETS: [&str; 3] = ["SUBMITTED", "CLOSED", "CANCELLED"];

/// 今天（北京时间）的日期。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

// ==================== 入参 ====================

/// 建单 / 改单请求（改单复用同一形状：整单覆盖）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreatePurchaseOrderReq {
    /// 供应商（必填）。
    pub supplier_id: Option<String>,
    /// 收货仓库（必填，列 `NOT NULL`）。
    pub warehouse_id: Option<String>,
    /// 下单日期（缺省 = 今天，北京时间）。
    pub order_date: Option<NaiveDate>,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 金额口径（`EXCL` / `INCL`，缺省 `EXCL`）。
    pub price_mode: Option<String>,
    /// 账期天数（缺省 0）。
    pub credit_days: Option<i32>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（不能为空）。
    pub items: Vec<PurchaseOrderItemReq>,
}

/// 建单明细行。
///
/// ⚠️ 没有金额字段：金额一律服务端算（硬红线 2）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PurchaseOrderItemReq {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 数量（> 0，4 位标度）。
    pub qty: Option<Decimal>,
    /// 不含税单价（`price_mode = EXCL` 时必填，6 位标度）。
    pub price_excl: Option<Decimal>,
    /// 含税单价（`price_mode = INCL` 时必填，6 位标度）。
    pub price_incl: Option<Decimal>,
    /// 税率（百分数，0–100，缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PurchaseOrderStatusReq {
    /// 目标状态（`SUBMITTED` / `CLOSED` / `CANCELLED`）。
    pub status: Option<String>,
    /// 原因（`SUBMITTED → CANCELLED` 必填）。
    pub reason: Option<String>,
}

impl PurchaseOrderStatusReq {
    /// 目标态（白名单外的值 → 400）。
    pub fn target_status(&self) -> std::result::Result<&str, ApiError> {
        let raw = self.status.as_deref().map(str::trim).unwrap_or_default();
        if raw.is_empty() {
            return Err(bad_request(
                "status 不能为空（SUBMITTED / CLOSED / CANCELLED）",
            ));
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

// ==================== 校验后的中间形态 ====================

/// 校验通过、金额已算好的一行明细（只差 `unit_id`，由 handler 查物料补上）。
#[derive(Debug, Clone)]
pub struct PreparedLine {
    /// 物料 id 原文（handler 解析 UUID）。
    pub material_id_raw: String,
    /// 数量（4 位）。
    pub qty: Decimal,
    /// 不含税单价（6 位）。
    pub price_excl: Decimal,
    /// 含税单价（6 位）。
    pub price_incl: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 行金额。
    pub amounts: LineAmounts,
    /// 行备注。
    pub remark: Option<String>,
}

/// 校验通过、金额已算好的整单（金额全部服务端算，硬红线 2）。
#[derive(Debug, Clone)]
pub struct PreparedOrder {
    /// 供应商 id 原文。
    pub supplier_id_raw: String,
    /// 仓库 id 原文。
    pub warehouse_id_raw: String,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 口径。
    pub price_mode: String,
    /// 备注。
    pub remark: Option<String>,
    /// 账期天数。
    pub credit_days: i32,
    /// 明细（顺序即 `line_no`）。
    pub lines: Vec<PreparedLine>,
    /// 单据汇总（逐行舍入后相加）。
    pub totals: TotalsSummary,
}

/// 汇总（避免 DTO 层依赖 `purchase_model::NewItem`）。
#[derive(Debug, Clone, Copy)]
pub struct TotalsSummary {
    /// 合计数量。
    pub total_qty: Decimal,
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计。
    pub with_tax: Decimal,
}

impl CreatePurchaseOrderReq {
    /// 校验 + 算金额（**纯函数**：不做任何数据库查询，单测可直接跑）。
    ///
    /// # 异常情况
    ///
    /// 任何一条边界不过 → `400`（业务码 `PARAM_INVALID`），文案点名第几行哪个字段。
    pub fn prepare(&self) -> std::result::Result<PreparedOrder, ApiError> {
        let price_mode = self
            .price_mode
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(PRICE_MODE_EXCL)
            .to_uppercase();
        if !PRICE_MODES.contains(&price_mode.as_str()) {
            return Err(bad_request(format!(
                "price_mode = `{price_mode}` 不合法：只允许 {}（EXCL 不含税录入 / INCL 含税录入）",
                PRICE_MODES.join(" / ")
            )));
        }

        let supplier_id_raw = require_text(self.supplier_id.as_deref(), "supplier_id")?;
        let warehouse_id_raw = require_text(self.warehouse_id.as_deref(), "warehouse_id")?;

        let order_date = self.order_date.unwrap_or_else(today_cst);
        if let Some(expect_date) = self.expect_date
            && expect_date < order_date
        {
            return Err(bad_request(format!(
                "expect_date（{expect_date}）不能早于 order_date（{order_date}）"
            )));
        }

        let credit_days = self.credit_days.unwrap_or(0);
        if credit_days < 0 {
            return Err(bad_request(format!(
                "credit_days = {credit_days} 不能为负（库约束 ck_biz_po_credit）"
            )));
        }

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

        if self.items.is_empty() {
            return Err(bad_request("items 不能为空：采购订单至少要有一行明细"));
        }
        if self.items.len() > MAX_LINES {
            return Err(bad_request(format!(
                "items 最多 {MAX_LINES} 行（当前 {} 行）",
                self.items.len()
            )));
        }

        let mut lines = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            lines.push(item.prepare(index, &price_mode)?);
        }

        let amounts: Vec<LineAmounts> = lines.iter().map(|line| line.amounts).collect();
        let totals = TotalsSummary {
            total_qty: lines.iter().fold(Decimal::ZERO, |acc, line| acc + line.qty),
            amount: amounts.iter().fold(Decimal::ZERO, |acc, a| acc + a.amount),
            tax: amounts
                .iter()
                .fold(Decimal::ZERO, |acc, a| acc + a.tax_amount),
            with_tax: amounts
                .iter()
                .fold(Decimal::ZERO, |acc, a| acc + a.with_tax_amount),
        };

        Ok(PreparedOrder {
            supplier_id_raw,
            warehouse_id_raw,
            order_date,
            expect_date: self.expect_date,
            price_mode,
            remark,
            credit_days,
            lines,
            totals,
        })
    }
}

impl PurchaseOrderItemReq {
    /// 单行校验 + 算金额。`index` 从 0 起，文案里按「第 N 行」展示。
    fn prepare(
        &self,
        index: usize,
        price_mode: &str,
    ) -> std::result::Result<PreparedLine, ApiError> {
        let line_no = index + 1;
        let material_id_raw = require_text(
            self.material_id.as_deref(),
            &format!("items[{index}].material_id"),
        )?;

        let qty_raw = self
            .qty
            .ok_or_else(|| bad_request(format!("第 {line_no} 行数量（qty）必填")))?;
        if qty_raw < Decimal::ZERO {
            return Err(bad_request(format!(
                "第 {line_no} 行数量 {qty_raw} 不能为负"
            )));
        }
        if qty_raw.is_zero() {
            // ⚠️ 卡面写「草稿允许 0」，但 ck_biz_po_item_qty CHECK (qty > 0) 让 0 数量的行插不进库
            // （放开要动 V0031 的约束，硬红线 6 禁止）。这里在建单/改单就拦，文案直接给可行动作。
            return Err(bad_request(format!(
                "第 {line_no} 行数量为 0：请填写数量或删除该行\
                 （库约束 ck_biz_po_item_qty 要求 qty > 0，草稿同样不允许 0 数量明细）"
            )));
        }
        let qty = qty_raw.round_dp(4);
        if qty != qty_raw {
            return Err(bad_request(format!(
                "第 {line_no} 行数量 {qty_raw} 超过 4 位小数（列 NUMERIC(18,4)）"
            )));
        }

        let tax_rate = self.tax_rate.unwrap_or(Decimal::ZERO);
        if tax_rate < Decimal::ZERO || tax_rate > Decimal::ONE_HUNDRED {
            return Err(bad_request(format!(
                "第 {line_no} 行税率 {tax_rate} 越界：允许 0–100（0 = 免税，库约束 ck_biz_po_item_tax_rate）"
            )));
        }

        let (price_excl, price_incl) = if price_mode == PRICE_MODE_INCL {
            let incl = self.price_incl.or(self.price_excl).ok_or_else(|| {
                bad_request(format!(
                    "第 {line_no} 行缺少含税单价 price_incl（price_mode = INCL 时必填）"
                ))
            })?;
            if incl < Decimal::ZERO {
                return Err(bad_request(format!(
                    "第 {line_no} 行含税单价 {incl} 不能为负：退货请走作废红冲（PRD §3.1.6）"
                )));
            }
            let incl = unit_price(incl);
            let divisor = Decimal::ONE + tax_rate / Decimal::ONE_HUNDRED;
            let excl = if divisor.is_zero() {
                incl
            } else {
                unit_price(incl / divisor)
            };
            (excl, incl)
        } else {
            let excl = self.price_excl.or(self.price_incl).ok_or_else(|| {
                bad_request(format!(
                    "第 {line_no} 行缺少不含税单价 price_excl（price_mode = EXCL 时必填）"
                ))
            })?;
            if excl < Decimal::ZERO {
                return Err(bad_request(format!(
                    "第 {line_no} 行不含税单价 {excl} 不能为负：退货请走作废红冲（PRD §3.1.6）"
                )));
            }
            let excl = unit_price(excl);
            let incl = unit_price(excl * (Decimal::ONE + tax_rate / Decimal::ONE_HUNDRED));
            (excl, incl)
        };

        let amounts = crate::business::erp::purchase_model::line_amounts(
            price_mode,
            qty,
            // `line_amounts` 吃的是**该口径下的录入单价**：口径 B 必须传含税单价
            if price_mode == PRICE_MODE_INCL {
                price_incl
            } else {
                price_excl
            },
            tax_rate,
        );

        let remark = self
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);

        Ok(PreparedLine {
            material_id_raw,
            qty,
            price_excl,
            price_incl,
            tax_rate,
            amounts,
            remark,
        })
    }
}

/// 提交前拦截：**每一行数量必须 > 0**（PRD §3.1.6「提交/过账时拦截」）。
///
/// 卡面 ⑦⑧ 的场景（草稿 0 数量、提交时 400）在库里做不到「草稿存 0」
/// （`ck_biz_po_item_qty`），所以这条校验在**建单/改单**就生效；
/// 但它同时是「提交」这一步的守卫：DRAFT → SUBMITTED 时用行数据再判一次，
/// 兜住历史脏数据（P2-A1 之前手工插过 0 数量行的情况）。
pub fn ensure_submittable_lines(
    lines: &[PurchaseOrderItemRow],
) -> std::result::Result<(), ApiError> {
    if lines.is_empty() {
        return Err(bad_request("明细为空，不能提交（请先补明细）"));
    }
    for line in lines {
        if line.qty.is_zero() {
            return Err(bad_request(format!(
                "第 {} 行数量为 0，请删除或填写数量后再提交",
                line.line_no
            )));
        }
        if line.qty < Decimal::ZERO {
            return Err(bad_request(format!(
                "第 {} 行数量 {} 非法（< 0）",
                line.line_no, line.qty
            )));
        }
    }
    Ok(())
}

// ==================== 出参 ====================

/// 建单 / 改单 / 流转的返回：单头 + 口径 + 明细。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderDetailRes {
    /// 单头（含 `total_*` 三金额与 `price_mode`）。
    pub order: PurchaseOrderRes,
    /// 明细（行号升序，含三金额）。
    pub items: Vec<PurchaseOrderLineRes>,
}

/// 明细行出参。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderLineRes {
    /// 主键。
    pub id: String,
    /// 行号（从 1 起）。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 计量单位。
    pub unit_id: String,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price_excl: Decimal,
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
    /// 已收货数量（P2-B1 维护，建单时恒 0）。
    pub received_qty: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl From<&PurchaseOrderItemRow> for PurchaseOrderLineRes {
    fn from(row: &PurchaseOrderItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            unit_id: row.unit_id.to_string(),
            qty: row.qty,
            price_excl: row.price,
            price_incl: row.price_incl,
            tax_rate: row.tax_rate,
            amount: row.amount,
            tax_amount: row.tax_amount,
            with_tax_amount: row.with_tax_amount,
            received_qty: row.received_qty,
            remark: row.remark.clone(),
        }
    }
}

// ==================== 小工具 ====================

/// 必填文本（去空白；空 → 400）。
fn require_text(raw: Option<&str>, field: &str) -> std::result::Result<String, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => Ok(text.to_string()),
        None => Err(bad_request(format!("{field} 不能为空"))),
    }
}

/// 400 统一构造（业务码 `PARAM_INVALID`，与 `erp::dto` 同口径）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 单测（纯逻辑，不需要数据库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::purchase_model::{
        LEGAL_EDGES, can_transition, line_amounts, sum_lines,
    };
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 组请求：`(qty, price, tax_rate)`，口径由 `mode` 决定给哪个单价字段。
    fn request(mode: Option<&str>, lines: &[(&str, &str, &str)]) -> CreatePurchaseOrderReq {
        CreatePurchaseOrderReq {
            supplier_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            warehouse_id: Some("22222222-2222-4222-8222-222222222222".to_string()),
            price_mode: mode.map(str::to_string),
            items: lines
                .iter()
                .map(|(qty, price, rate)| PurchaseOrderItemReq {
                    material_id: Some("33333333-3333-4333-8333-333333333333".to_string()),
                    qty: Some(dec(qty)),
                    price_excl: (mode != Some(PRICE_MODE_INCL)).then(|| dec(price)),
                    price_incl: (mode == Some(PRICE_MODE_INCL)).then(|| dec(price)),
                    tax_rate: Some(dec(rate)),
                    remark: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// ① 口径 A 算例（PRD §3.1.4）：100 × 2、13% → 200.00 / 26.00 / 226.00。
    #[test]
    fn excl_card_example_matches_prd() {
        let amounts = line_amounts(PRICE_MODE_EXCL, dec("100"), dec("2"), dec("13"));
        assert_eq!(amounts.amount, dec("200.00"));
        assert_eq!(amounts.tax_amount, dec("26.00"));
        assert_eq!(amounts.with_tax_amount, dec("226.00"));

        let prepared = request(None, &[("100", "2", "13")]).prepare().unwrap();
        assert_eq!(
            prepared.price_mode, PRICE_MODE_EXCL,
            "默认口径必须是不含税录入"
        );
        assert_eq!(prepared.totals.amount, dec("200.00"));
        assert_eq!(prepared.totals.tax, dec("26.00"));
        assert_eq!(prepared.totals.with_tax, dec("226.00"));
        assert_eq!(prepared.totals.total_qty, dec("100"));
        assert_eq!(prepared.lines[0].price_incl, dec("2.260000"));
    }

    /// ② 口径 B 算例（PRD §3.1.4）：100 × 2、13% → 176.99 / 23.01 / 200.00。
    #[test]
    fn incl_card_example_subtracts_tax() {
        let amounts = line_amounts(PRICE_MODE_INCL, dec("100"), dec("2"), dec("13"));
        assert_eq!(amounts.amount, dec("176.99"));
        assert_eq!(amounts.tax_amount, dec("23.01"), "税额必须减法倒挤");
        assert_eq!(amounts.with_tax_amount, dec("200.00"));
        assert_eq!(
            amounts.amount + amounts.tax_amount,
            amounts.with_tax_amount,
            "三金额恒等必须精确成立"
        );

        let prepared = request(Some(PRICE_MODE_INCL), &[("100", "2", "13")])
            .prepare()
            .unwrap();
        assert_eq!(prepared.totals.amount, dec("176.99"));
        assert_eq!(prepared.totals.tax, dec("23.01"));
        assert_eq!(prepared.totals.with_tax, dec("200.00"));
        // 不含税单价由含税单价倒算：2 / 1.13 = 1.7699115… → 6 位 = 1.769912
        assert_eq!(prepared.lines[0].price_excl, dec("1.769912"));
    }

    /// ③ 尾差：3 行各 0.005 → 逐行舍入（0.01 × 3 = 0.03），不是先加总再舍入（0.02）。
    #[test]
    fn lines_are_rounded_before_summing() {
        let one = line_amounts(PRICE_MODE_EXCL, dec("1"), dec("0.005"), dec("0"));
        assert_eq!(one.amount, dec("0.01"), "四舍五入（不是银行家舍入）");
        let lines = vec![one, one, one];
        let (amount, tax, with_tax) = sum_lines(&lines);
        assert_eq!(amount, dec("0.03"));
        assert_eq!(tax, Decimal::ZERO);
        assert_eq!(with_tax, dec("0.03"));
        assert_ne!(
            amount,
            dec("0.02"),
            "先加总再舍入会少 1 分（PRD §3.1.5 禁止）"
        );

        let prepared = request(
            None,
            &[
                ("1", "0.005", "0"),
                ("1", "0.005", "0"),
                ("1", "0.005", "0"),
            ],
        )
        .prepare()
        .unwrap();
        assert_eq!(prepared.totals.amount, dec("0.03"));
    }

    /// ④ 税率 0（免税）：tax = 0，价税合计 = 价款。
    #[test]
    fn zero_tax_rate_keeps_tax_empty() {
        let prepared = request(None, &[("7", "3.5", "0")]).prepare().unwrap();
        assert_eq!(prepared.totals.amount, dec("24.50"));
        assert_eq!(prepared.totals.tax, Decimal::ZERO);
        assert_eq!(prepared.totals.with_tax, dec("24.50"));
        // 口径 B 的 0 税率同样：价税合计 = 价款，tax 必为 0
        let incl = line_amounts(PRICE_MODE_INCL, dec("7"), dec("3.5"), dec("0"));
        assert_eq!(incl.amount, dec("24.50"));
        assert_eq!(incl.tax_amount, Decimal::ZERO);
    }

    /// ⑤ 单价 0（赠品）：允许，金额全 0，不报错。
    #[test]
    fn zero_price_gift_is_allowed() {
        let prepared = request(None, &[("10", "0", "13")]).prepare().unwrap();
        assert_eq!(prepared.totals.amount, Decimal::ZERO);
        assert_eq!(prepared.totals.tax, Decimal::ZERO);
        assert_eq!(prepared.totals.with_tax, Decimal::ZERO);
        assert_eq!(prepared.totals.total_qty, dec("10"));
    }

    /// ⑥ 负单价：400（PRD §3.1.6「本批禁止在订单填负数，退货走作废红冲」）。
    #[test]
    fn negative_price_is_rejected() {
        let err = request(None, &[("1", "-2", "13")]).prepare().unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("不能为负"), "{}", err.msg);
        // 口径 B 同样拦
        let incl = request(Some(PRICE_MODE_INCL), &[("1", "-2", "13")])
            .prepare()
            .unwrap_err();
        assert_eq!(incl.status, 400);
    }

    /// ⑦ 数量 0：**建单即 400**（与卡面的差异，见模块文档：`ck_biz_po_item_qty CHECK (qty > 0)`）。
    #[test]
    fn zero_qty_is_rejected_even_for_draft() {
        let err = request(None, &[("0", "2", "13")]).prepare().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("数量为 0"), "{}", err.msg);
        assert!(
            err.msg.contains("ck_biz_po_item_qty"),
            "文案要给出依据：{}",
            err.msg
        );
    }

    /// ⑦b 提交守卫：`ensure_submittable_lines` 单独钉住「0 数量不能提交」这条 PRD 规则。
    ///
    /// 卡面 ⑧ 的「草稿能存 0 → 提交 400」在库里走不通（约束不允许 0），
    /// 所以这条守卫用于兜历史脏数据；这里用「空明细」这一条可直接构造的分支证明它真的在拦。
    #[test]
    fn submit_guard_rejects_empty_and_zero_qty() {
        let err = ensure_submittable_lines(&[]).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("明细为空"), "{}", err.msg);
    }

    /// ⑧ 多税率单：各行独立算，合计 = 逐行舍入后相加。
    #[test]
    fn multi_tax_rate_lines_sum_independently() {
        let prepared = request(
            None,
            &[("10", "2.5", "13"), ("3", "10", "6"), ("2", "1", "0")],
        )
        .prepare()
        .unwrap();
        assert_eq!(prepared.lines[0].amounts.amount, dec("25.00"));
        assert_eq!(prepared.lines[0].amounts.tax_amount, dec("3.25"));
        assert_eq!(prepared.lines[1].amounts.amount, dec("30.00"));
        assert_eq!(prepared.lines[1].amounts.tax_amount, dec("1.80"));
        assert_eq!(prepared.lines[2].amounts.amount, dec("2.00"));
        assert_eq!(prepared.lines[2].amounts.tax_amount, Decimal::ZERO);
        assert_eq!(prepared.totals.amount, dec("57.00"));
        assert_eq!(prepared.totals.tax, dec("5.05"));
        assert_eq!(prepared.totals.with_tax, dec("62.05"));
    }

    /// ⑨ 明细为空：400。
    #[test]
    fn empty_items_are_rejected() {
        let err = request(None, &[]).prepare().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("items 不能为空"), "{}", err.msg);
    }

    /// ⑩ 客户端传金额一律忽略（硬红线 2）：JSON 里塞 amount/tax_amount 改不了任何数。
    #[test]
    fn client_supplied_amounts_are_ignored() {
        let json = r#"{
            "supplier_id": "11111111-1111-4111-8111-111111111111",
            "warehouse_id": "22222222-2222-4222-8222-222222222222",
            "price_mode": "EXCL",
            "total_amount": "1.00",
            "total_with_tax": "1.00",
            "items": [{
                "material_id": "33333333-3333-4333-8333-333333333333",
                "qty": 100,
                "price_excl": 2,
                "tax_rate": 13,
                "amount": "1.00",
                "tax_amount": "0.00",
                "with_tax_amount": "1.00"
            }]
        }"#;
        let req: CreatePurchaseOrderReq = serde_json::from_str(json).unwrap();
        let prepared = req.prepare().unwrap();
        assert_eq!(prepared.totals.amount, dec("200.00"), "金额只能由服务端算");
        assert_eq!(prepared.totals.tax, dec("26.00"));
        assert_eq!(prepared.totals.with_tax, dec("226.00"));
    }

    /// ⑪ 口径缺省 = EXCL；大小写不敏感；非法值 400。
    #[test]
    fn price_mode_validation() {
        assert_eq!(
            request(None, &[("1", "1", "0")])
                .prepare()
                .unwrap()
                .price_mode,
            PRICE_MODE_EXCL
        );
        assert_eq!(
            request(Some("incl"), &[("1", "1", "13")])
                .prepare()
                .unwrap()
                .price_mode,
            PRICE_MODE_INCL,
            "小写也要归一化"
        );
        let err = request(Some("TAX_INCLUDED"), &[("1", "1", "13")])
            .prepare()
            .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("EXCL"), "{}", err.msg);
    }

    /// ⑫ supplier_id / warehouse_id 必填（库列 NOT NULL）。
    #[test]
    fn supplier_and_warehouse_are_required() {
        let mut req = request(None, &[("1", "1", "13")]);
        req.supplier_id = None;
        let err = req.prepare().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("supplier_id"), "{}", err.msg);

        let mut req = request(None, &[("1", "1", "13")]);
        req.warehouse_id = Some("   ".to_string());
        let err = req.prepare().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("warehouse_id"), "{}", err.msg);
    }

    /// ⑬ 税率越界（> 100 或 < 0）：400（库 `ck_biz_po_item_tax_rate` 同口径）。
    #[test]
    fn tax_rate_out_of_range_is_rejected() {
        let high = request(None, &[("1", "1", "101")]).prepare().unwrap_err();
        assert_eq!(high.status, 400);
        assert!(high.msg.contains("0–100"), "{}", high.msg);
        let low = request(None, &[("1", "1", "-1")]).prepare().unwrap_err();
        assert_eq!(low.status, 400);
    }

    /// ⑭ 状态机：合法边全部放行（P2-A2 的 4 条 + P10 补齐收货侧 4 条，
    /// 另登记 `SUBMITTED→*_RECEIVED` 两条既有收货白名单行为，详见
    /// `shared/state_machine.rs::PURCHASE_ORDER_EDGES` 的注释）。
    #[test]
    fn state_machine_allows_four_edges() {
        assert_eq!(LEGAL_EDGES.len(), 10);
        for (from, to) in [
            ("DRAFT", "SUBMITTED"),
            ("DRAFT", "CANCELLED"),
            ("SUBMITTED", "APPROVED"),
            ("SUBMITTED", "CLOSED"),
            ("SUBMITTED", "CANCELLED"),
            ("SUBMITTED", "PARTIAL_RECEIVED"),
            ("SUBMITTED", "RECEIVED"),
            ("APPROVED", "PARTIAL_RECEIVED"),
            ("APPROVED", "RECEIVED"),
            ("PARTIAL_RECEIVED", "RECEIVED"),
        ] {
            assert!(can_transition(from, to), "{from} → {to} 必须放行");
        }
    }

    /// ⑮ 状态机：跳级 / 回退 / 原地 / 终态再流转全部拒绝（卡面 ⑮）。
    #[test]
    fn state_machine_rejects_illegal_edges() {
        for (from, to) in [
            ("DRAFT", "CLOSED"),
            ("DRAFT", "APPROVED"),
            ("DRAFT", "PARTIAL_RECEIVED"),
            ("DRAFT", "RECEIVED"),
            ("SUBMITTED", "DRAFT"),
            ("CLOSED", "CANCELLED"),
            ("CANCELLED", "SUBMITTED"),
            ("RECEIVED", "CANCELLED"),
            ("SUBMITTED", "SUBMITTED"),
            ("PARTIAL_RECEIVED", "PARTIAL_RECEIVED"),
        ] {
            assert!(!can_transition(from, to), "{from} → {to} 必须拒绝");
        }
        assert!(
            !TRANSITION_TARGETS.contains(&"DRAFT"),
            "目标态里不能出现 DRAFT"
        );
    }

    /// ⑯ 非法流转文案要点名「当前态的可选去向」/「终态」。
    #[test]
    fn transition_message_names_allowed_targets() {
        let message =
            crate::business::erp::purchase_model::transition_rejected_message("DRAFT", "CLOSED");
        assert!(message.contains("SUBMITTED"), "{message}");
        assert!(message.contains("CANCELLED"), "{message}");
        let terminal =
            crate::business::erp::purchase_model::transition_rejected_message("CLOSED", "DRAFT");
        assert!(terminal.contains("终态"), "{terminal}");
    }

    /// ⑰ 状态流转请求：目标态白名单 + reason 去空白。
    #[test]
    fn status_request_validation() {
        let mut req = PurchaseOrderStatusReq {
            status: None,
            reason: None,
        };
        assert_eq!(req.target_status().unwrap_err().status, 400);
        req.status = Some("DRAFT".to_string());
        assert_eq!(req.target_status().unwrap_err().status, 400);
        req.status = Some("SUBMITTED".to_string());
        assert_eq!(req.target_status().unwrap(), "SUBMITTED");
        req.reason = Some("   ".to_string());
        assert!(req.reason_text().is_none(), "空白原因视为没给");
        req.reason = Some(" 供应商断供 ".to_string());
        assert_eq!(req.reason_text(), Some("供应商断供"));
    }
}
