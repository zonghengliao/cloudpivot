//! 在途库存查询接口（M2-04）的出入参契约。
//!
//! 卡：t_f1bce846（M2-04【M2·仓库】在途库存：查询接口）；拆卡依据
//! `docs/planning/20261113-新卡清单.md` §M2 的 M2-04 行。表结构见
//! `V0420__inventory_in_transit.up.sql`（M2-01），写入路径见
//! [`crate::business::erp::shipment::shipment_transit`]（M2-02 发货过账 / M2-03 签收确认）。
//!
//! # 只有「读」这一类契约，没有任何 `Req`
//!
//! 本模块**不写库**：在途行的产生（发货过账 `shipped_qty`）与结转（签收 `received_qty`）
//! 分别由 M2-02 / M2-03 的 handler 负责，写入口只有那两处。这里全是查询参数与出参。
//!
//! # 出参里的 `*_id` 一律成对返回 `*_name`（AGENTS.md 约定 13）
//!
//! 在途明细是**业务展示类**（要显示「现在这个物料 / 仓库 / 客户叫什么」），所以名字一律
//! 实时 join 主数据，而不是存快照。软删的主数据按「缺字段」处理（`null`），
//! 与 `shipment_handler::ShipmentRes` 的 `customer_code` / `warehouse_code` 同口径 ——
//! 历史在途行照常可读，只是不再显示已删主数据的名字。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ==================== 明细分页 ====================

/// 在途明细分页筛选（`GET /business/erp/inventory-transit/page`）。
///
/// 所有字段留空 = 不加该条件；**`status` 留空 ≠ 不加条件**：留空取默认值
/// [`DEFAULT_STATUS`]（`IN_TRANSIT`），否则一次「查在途」会把已签收 / 已作废的
/// 历史行全翻出来，数字看着像在途、其实早就签完了（静默多看，比少看危险）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TransitFilter {
    /// 物料 id（精确 UUID）。
    pub material_id: Option<String>,
    /// 发货仓库 id（精确 UUID；即在途行的 `warehouse_id`，不是收货方所在地）。
    pub warehouse_id: Option<String>,
    /// 客户 id（精确 UUID；经 `biz_shipment.customer_id` 关联）。
    pub customer_id: Option<String>,
    /// 发货单 id（精确 UUID）—— 从发货单详情「查看在途」跳过来时用。
    pub shipment_id: Option<String>,
    /// 状态（精确：`IN_TRANSIT` / `RECEIVED` / `CANCELLED`；留空 = `IN_TRANSIT`）。
    pub status: Option<String>,
    /// 发货日期下界（`yyyy-MM-dd`，含当天）—— 在途账龄筛选用。
    pub ship_date_from: Option<String>,
    /// 发货日期上界（`yyyy-MM-dd`，含当天）。
    pub ship_date_to: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

/// `status` 留空时的默认值（在途库存的本义：还在路上的）。
pub const DEFAULT_STATUS: &str = crate::business::erp::shipment::shipment_transit::STATUS_IN_TRANSIT;

/// 一行在途明细（列表 / 详情同构）。
///
/// `shipped_qty` / `received_qty` / `qty_in_transit` 三个数量**直接取库里的列**
/// （`qty_in_transit` 是 PG 生成列，任何写入路径都写不出「不符」的值），
/// 本模块一行算术都不做 —— M2-01 的迁移注释已经把这条口径钉死在表上。
#[derive(Debug, Clone, Serialize)]
pub struct TransitRes {
    /// 在途行主键。
    pub id: String,
    /// 发货单 id。
    pub shipment_id: String,
    /// 发货单号（发货单软删后为 `null`）。
    pub ship_no: Option<String>,
    /// 客户 id（发货单软删后为 `null`：客户信息随发货单头走）。
    pub customer_id: Option<String>,
    /// 客户编码（客户软删后为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称（客户软删后为 `null`）。
    pub customer_name: Option<String>,
    /// 发货仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（仓库软删后为 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（仓库软删后为 `null`）。
    pub warehouse_name: Option<String>,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（物料软删后为 `null`）。
    pub material_code: Option<String>,
    /// 物料名称（物料软删后为 `null`）。
    pub material_name: Option<String>,
    /// 规格型号（物料主数据缺失时为 `null`）。
    pub spec: Option<String>,
    /// 计量单位名（物料主数据缺失 / 单位已删时为 `null`）。
    pub unit_name: Option<String>,
    /// 发货日期（`yyyy-MM-dd`）。
    pub ship_date: String,
    /// 在途天数 = `CURRENT_DATE - ship_date`（账龄视角；由 PG 算，跨时区不会差一天）。
    pub age_days: i32,
    /// 发货数量（累计上限）。
    pub shipped_qty: Decimal,
    /// 已签收数量。
    pub received_qty: Decimal,
    /// 当前在途量 = `shipped_qty - received_qty`（PG 生成列）。
    pub qty_in_transit: Decimal,
    /// 状态编码。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 全部签收时间（`RECEIVED` 才有值）。
    pub received_at: Option<DateTime<Utc>>,
    /// 备注（签收 / 作废原因等）。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

// ==================== 汇总 ====================

/// 在途汇总筛选（`GET /business/erp/inventory-transit/summary`）。
///
/// 与 [`TransitFilter`] 的差别：没有 `status`。汇总恒为**未签收在途**
/// （`status = 'IN_TRANSIT'`）—— 它是「这个物料/仓库还有多少在路上」的口径，
/// 把已签收 / 已作废混进来就不是在途了。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TransitSummaryFilter {
    /// 物料 id（精确 UUID）。
    pub material_id: Option<String>,
    /// 发货仓库 id（精确 UUID）。
    pub warehouse_id: Option<String>,
    /// 客户 id（精确 UUID；经 `biz_shipment.customer_id` 关联）。
    pub customer_id: Option<String>,
}

/// 一行汇总（按 物料 × 发货仓 分组）。
#[derive(Debug, Clone, Serialize)]
pub struct TransitSummaryRow {
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（物料软删后为 `null`）。
    pub material_code: Option<String>,
    /// 物料名称（物料软删后为 `null`）。
    pub material_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 发货仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（仓库软删后为 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（仓库软删后为 `null`）。
    pub warehouse_name: Option<String>,
    /// 未签收在途量合计。
    pub qty_in_transit: Decimal,
    /// 发货量合计。
    pub shipped_qty: Decimal,
    /// 已签收量合计。
    pub received_qty: Decimal,
    /// 涉及的发货单张数（`count(DISTINCT shipment_id)`）。
    pub shipments: i64,
    /// 最早一张发货单的发货日期（`yyyy-MM-dd`）—— 账龄从这里看。
    pub first_ship_date: String,
    /// 最早那批货的在途天数（`CURRENT_DATE - 最早 ship_date`）。
    pub max_age_days: i32,
}

/// 在途汇总出参。
#[derive(Debug, Clone, Serialize)]
pub struct TransitSummaryRes {
    /// 汇总行（按在途量倒序；超过 [`crate::business::erp::inventory_transit::model::SUMMARY_ROW_LIMIT`] 会截断）。
    pub rows: Vec<TransitSummaryRow>,
    /// 本次返回的汇总行数。
    pub row_count: u64,
    /// 是否被截断（`rows` 到了上限，说明还有别的 物料×仓库 组合没返回；
    /// **`total_qty_in_transit` 不受截断影响**，它按筛选条件全量算）。
    pub truncated: bool,
    /// 全部匹配组的未签收在途量合计（不受 `rows` 截断影响）。
    pub total_qty_in_transit: Decimal,
    /// 全部匹配组涉及的发货单张数（`count(DISTINCT shipment_id)`）。
    pub total_shipments: i64,
    /// 全部匹配组的在途行数。
    pub total_lines: i64,
}

// ==================== 纯逻辑校验（单测直接调，不碰库） ====================

/// `status` 归一化：留空 → [`DEFAULT_STATUS`]；非域内值 → `Err(可读文案)`。
///
/// 为什么非法值必须 400 而不是静默当成默认值：把 `status=RECIEVED`（拼错）静默当
/// `IN_TRANSIT` 会让调用方以为「签收没生效」，反过来把 `status=in_transit` 静默当
/// 默认值则看着对、其实没有任何校验 —— 两种都是假数据。宁可直接报错。
pub fn normalize_status(raw: Option<&str>) -> std::result::Result<String, String> {
    let status = raw.map(str::trim).filter(|text| !text.is_empty());
    match status {
        None => Ok(DEFAULT_STATUS.to_string()),
        Some(text) => {
            if crate::business::erp::shipment::shipment_transit::is_valid_status(text) {
                Ok(text.to_string())
            } else {
                Err(format!(
                    "status 必须是 {} 之一（当前 `{text}`）",
                    crate::business::erp::shipment::shipment_transit::STATUSES.join(" / ")
                ))
            }
        }
    }
}

/// 日期区间校验：`from > to` → `Err`（否则静默返回 0 行，用户会以为「这段时间没有在途」）。
pub fn ensure_date_range(
    from: Option<nik::prelude::chrono::NaiveDate>,
    to: Option<nik::prelude::chrono::NaiveDate>,
) -> std::result::Result<(), String> {
    match (from, to) {
        (Some(from), Some(to)) if from > to => Err(format!(
            "ship_date_from（{from}）不能晚于 ship_date_to（{to}）：区间反了会查出 0 行"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `status` 留空 = 默认在途；三种合法值原样通过（含首尾空格 trim）。
    #[test]
    fn normalize_status_defaults_to_in_transit() {
        assert_eq!(normalize_status(None).as_deref(), Ok("IN_TRANSIT"));
        assert_eq!(normalize_status(Some("   ")).as_deref(), Ok("IN_TRANSIT"));
        assert_eq!(normalize_status(Some(" RECEIVED ")).as_deref(), Ok("RECEIVED"));
        assert_eq!(normalize_status(Some("CANCELLED")).as_deref(), Ok("CANCELLED"));
        println!("[M2-04] status 留空 → IN_TRANSIT；三值原样通过 ✓");
    }

    /// 非法状态值必须报错并列出合法值（不静默当默认值）。
    #[test]
    fn normalize_status_rejects_unknown() {
        for bad in ["RECIEVED", "in_transit", "SHIPPED", "ALL", "0"] {
            let error = normalize_status(Some(bad)).expect_err("非法状态必须报错");
            assert!(error.contains(bad), "文案要点名非法值：{error}");
            assert!(error.contains("IN_TRANSIT"), "文案要列出合法值：{error}");
        }
        println!("[M2-04] 非法 status → Err（文案列出 IN_TRANSIT / RECEIVED / CANCELLED）✓");
    }

    /// 日期区间反了必须报错（静默 0 行是假数据）。
    #[test]
    fn date_range_rejects_reversed() {
        use nik::prelude::chrono::NaiveDate;
        let d = |text: &str| NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期");
        assert!(ensure_date_range(Some(d("2026-11-01")), Some(d("2026-11-30"))).is_ok());
        assert!(ensure_date_range(Some(d("2026-11-01")), None).is_ok());
        assert!(ensure_date_range(None, Some(d("2026-11-30"))).is_ok());
        assert!(ensure_date_range(None, None).is_ok());
        let error = ensure_date_range(Some(d("2026-11-30")), Some(d("2026-11-01"))).expect_err("区间反了必须报错");
        assert!(error.contains("2026-11-30"), "文案要带出实际值：{error}");
        println!("[M2-04] ship_date_from > ship_date_to → Err ✓");
    }

    /// 默认状态必须与表上的状态域逐字一致（防止哪天迁移改了值、这里悄悄漂移）。
    #[test]
    fn default_status_matches_transit_domain() {
        assert_eq!(DEFAULT_STATUS, "IN_TRANSIT");
        assert!(crate::business::erp::shipment::shipment_transit::is_valid_status(
            DEFAULT_STATUS
        ));
    }
}
