//! 库龄计算接口层（M2-08）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/inventory-aging/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/inventory-aging/page` | `business:erp:inventory-aging:list` | 批次层（库龄层）分页：按物料 / 仓库 / 批号 / 库龄区间查「哪一层还剩多少、放了几天」 |
//!
//! # 只有一条只读路径，零写 SQL
//!
//! 层是由 `biz_inventory_txn` 的 `IN` 流水推导出来的（FIFO，见 [`super::model`] 与
//! `V0424` 的库函数）。本文件不开事务、不写库、也不「顺手补齐一层」——
//! 流水缺了就是缺了，补流水是过账路径的事。
//!
//! # 与 M2-09 / M2-10 的分工
//!
//! 本接口给的是**行级明细**（一层一行，可以按批号 / 库龄区间钻取），
//! 库龄分段报表（0-30/31-60/61-90/90+）在 M2-09、呆滞料标记在 M2-10，
//! 三张卡共用同一个库函数 `biz_inventory_aging_layers(...)`（同一份 FIFO 口径）。
//!
//! # 错误口径（与 ERP 其余查询接口一致）
//!
//! 参数非法（非法 UUID / 日期格式 / 负库龄界 / 区间反了 / 布尔开关不认识）→
//! **400 + 66006（`ERP_FILTER_INVALID`）**，绝不静默忽略条件 ——
//! 「按物料筛」被吞掉会变成「返回全部物料的库龄」的假数据。

use axum::extract::Query;
use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::dto::{self, AgingFilter, AgingLayerRes};
use crate::business::erp::inventory_aging::model::{self, LayerQuery, LayerRow};
use crate::business::erp::model_base;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0424 的 sys_menu.auth_code 逐字一致） ====================

/// 批次层（库龄层）分页的读码。
///
/// 与 `business:erp:inventory:list`（即时库存结存账）分开：库龄是**流水推导出来的另一本账**，
/// 「能看仓里有多少」不等于「能看压在仓里多久」—— 与 V0143 给库存巡检单开 `:audit`、
/// V0421 给在途单开 `inventory-transit:list` 同一取向。
pub const PERM_LIST: &str = "business:erp:inventory-aging:list";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const INVENTORY_AGING_PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 校验原语 ====================

/// `400 + 66006`：筛选项非法。
fn bad_filter(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_FILTER_INVALID, message.into())
}

/// 可选 UUID（空白 → `None`；非法 → 400，**不静默当作没传**）。
fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_filter(format!("{field} 不是合法的 UUID：{text}"))),
    }
}

/// 可选日期（`yyyy-MM-dd`；空白 → `None`；格式不对 → 400）。
fn parse_opt_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_filter(format!("{field} 必须是 yyyy-MM-dd 格式（当前 `{text}`）"))),
    }
}

/// 布尔开关（缺省 `default`；`true/false`、`1/0`、`yes/no`、`on/off`，大小写不敏感）。
///
/// 其它值一律 400：`?include_consumed=treu` 静默当 `false` 会让调用方以为
/// 「已经连着吃满的层一起返回了」，实际只拿到有结存的层 —— 又一种假数据。
fn parse_bool(raw: Option<&str>, field: &str, default: bool) -> std::result::Result<bool, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(default),
        Some(text) => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            _ => Err(bad_filter(format!(
                "{field} 只接受 true/false（或 1/0、yes/no、on/off）：{text}"
            ))),
        },
    }
}

// ==================== 批次层分页 ====================

/// 批次层分页的编排：解析 → 校验 → 取数 → 批量补名 → 映射。
///
/// 单测直接调它（不起 HTTP），真库用例也走它 —— 与 `inventory_transit::handler::page_of` 同款。
pub async fn layer_page_of(filter: AgingFilter) -> std::result::Result<PageData<Vec<AgingLayerRes>>, ApiError> {
    let material_id = parse_opt_uuid(filter.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(filter.warehouse_id.as_deref(), "warehouse_id")?;
    let as_of = parse_opt_date(filter.as_of.as_deref(), "as_of")?.unwrap_or_else(dto::default_as_of);
    dto::ensure_age_range(filter.min_age_days, filter.max_age_days).map_err(bad_filter)?;
    let include_consumed = parse_bool(filter.include_consumed.as_deref(), "include_consumed", false)?;
    // 库龄区间 → 入库日期区间：等价关系写在 `dto::inbound_bounds`（逐天对拍过）
    let (inbound_from, inbound_to) = dto::inbound_bounds(as_of, filter.min_age_days, filter.max_age_days);

    let query = LayerQuery {
        material_id,
        warehouse_id,
        lot_no: dto::normalize_lot_no(filter.lot_no.as_deref()),
        as_of,
        inbound_from,
        inbound_to,
        include_consumed,
    };
    let (rows, total) = model::layer_page(&query, filter.page, filter.page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询库龄批次层"))?;

    // 主数据名字：两条批量查询，一页固定 2 条 SQL（不是每行各查一次）
    let material_ids: Vec<Uuid> = rows.iter().map(|row| row.material_id).collect();
    let materials: std::collections::HashMap<Uuid, model::MaterialBrief> = model::material_briefs(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询库龄层物料"))?
        .into_iter()
        .map(|brief| (brief.id, brief))
        .collect();
    let warehouse_ids: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> = model::warehouse_briefs(&warehouse_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询库龄层仓库"))?
        .into_iter()
        .map(|(id, code, name)| (id, (code, name)))
        .collect();

    let list = rows
        .iter()
        .map(|row| {
            let mut res = AgingLayerRes::of(row, as_of);
            if let Some(brief) = materials.get(&row.material_id) {
                res.material_code = Some(brief.code.clone());
                res.material_name = Some(brief.name.clone());
                res.spec = brief.spec.clone();
                res.unit_name = brief.unit_name.clone();
            }
            if let Some((code, name)) = warehouses.get(&row.warehouse_id) {
                res.warehouse_code = Some(code.clone());
                res.warehouse_name = Some(name.clone());
            }
            res
        })
        .collect();

    let (page, page_size) = model_base::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

impl AgingLayerRes {
    /// 一行 → 出参：库里能直接给的全给（数量 / 单价 / 金额 / 日期），库龄按基准日算，
    /// 主数据名字留空由调用方批量补齐。
    fn of(row: &LayerRow, as_of: NaiveDate) -> Self {
        Self {
            layer_id: row.layer_id.to_string(),
            material_id: row.material_id.to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_code: None,
            warehouse_name: None,
            lot_no: row.lot_no.clone(),
            doc_type: row.doc_type.clone(),
            doc_no: row.doc_no.clone(),
            line_no: row.line_no,
            biz_type: row.biz_type.clone(),
            inbound_date: row.inbound_date,
            age_days: dto::age_days_of(as_of, row.inbound_date),
            qty_in: row.qty_in,
            qty_consumed: row.qty_consumed,
            qty_remaining: row.qty_remaining,
            unit_cost: row.unit_cost,
            amount_remaining: row.amount_remaining,
            posted_at: row.posted_at,
            as_of,
        }
    }
}

// ==================== HTTP 入口 ====================

/// 批次层（库龄层）分页：按物料 / 仓库 / 批号 / 库龄区间查「哪一层还剩多少、放了几天」。
#[nik::permission(
    path = "/business/erp/inventory-aging/page",
    method = "GET",
    permission = "business:erp:inventory-aging:list"
)]
#[nik::get("/business/erp/inventory-aging/page")]
pub async fn inventory_aging_page(Query(filter): Query<AgingFilter>) -> ResResult<PageData<Vec<AgingLayerRes>>> {
    Ok(Res::ok(layer_page_of(filter).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;

    fn dec(text: &str) -> nik::prelude::rust_decimal::Decimal {
        text.parse().expect("定点字面量")
    }

    fn row() -> LayerRow {
        let now = Utc::now();
        LayerRow {
            layer_id: Uuid::from_u128(0x01),
            material_id: Uuid::from_u128(0x02),
            warehouse_id: Uuid::from_u128(0x03),
            lot_no: Some("LOT-20260901".to_string()),
            doc_type: "STOCK_IN".to_string(),
            doc_no: "IN-20260901-000001".to_string(),
            line_no: 1,
            biz_type: "PURCHASE_IN".to_string(),
            inbound_date: NaiveDate::from_ymd_opt(2026, 10, 15).expect("日期"),
            posted_at: now,
            qty_in: dec("200"),
            qty_consumed: dec("80"),
            qty_remaining: dec("120"),
            unit_cost: dec("12.5"),
            amount_remaining: dec("1500"),
        }
    }

    /// 出参映射：四个数量 / 单价 / 金额**原样透传**（不在这里重算 FIFO），库龄按基准日算。
    #[test]
    fn res_passes_quantities_through_and_computes_age() {
        let as_of = NaiveDate::from_ymd_opt(2026, 11, 16).expect("日期");
        let res = AgingLayerRes::of(&row(), as_of);
        assert_eq!(res.qty_in, dec("200"));
        assert_eq!(res.qty_consumed, dec("80"));
        assert_eq!(res.qty_remaining, dec("120"));
        assert_eq!(res.unit_cost, dec("12.5"));
        assert_eq!(res.amount_remaining, dec("1500"));
        assert_eq!(res.age_days, 32);
        assert_eq!(res.as_of, as_of);
        assert_eq!(res.lot_no.as_deref(), Some("LOT-20260901"));
        // 名字由批量补齐填，映射阶段留空（不是「没有名字」而是「还没查」）
        assert!(res.material_name.is_none());
        assert!(res.warehouse_name.is_none());
        println!("[M2-08] AgingLayerRes 映射：数量透传 / 库龄 32 天 / 名字待补齐 ✓");
    }

    /// 负数库龄不夹 0（补录到未来的入库是真数据，夹 0 会伪装成「今天刚入库」）。
    #[test]
    fn future_inbound_keeps_negative_age() {
        let mut row = row();
        row.inbound_date = NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期");
        let as_of = NaiveDate::from_ymd_opt(2026, 11, 16).expect("日期");
        assert_eq!(AgingLayerRes::of(&row, as_of).age_days, -15);
        println!("[M2-08] 入库日期晚于基准日 → 库龄 -15（不夹 0）✓");
    }

    /// 权限码常量与迁移逐字一致（改码必须同步改 V0424）。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(INVENTORY_AGING_PERMISSIONS, [PERM_LIST]);
        assert_eq!(PERM_LIST, "business:erp:inventory-aging:list");
        println!("[M2-08] 权限码 business:erp:inventory-aging:list（V0424）✓");
    }

    /// 非法筛选必须 400 + 66006（不能静默当作没传）。
    #[test]
    fn invalid_filters_are_400_with_erp_filter_invalid() {
        let error = parse_opt_uuid(Some("not-a-uuid"), "material_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("material_id"), "文案要点名字段：{}", error.msg);

        let error = parse_opt_date(Some("2026/11/16"), "as_of").expect_err("非法日期必须报错");
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("as_of"), "{}", error.msg);

        let error = parse_bool(Some("treu"), "include_consumed", false).expect_err("布尔必须严格");
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("include_consumed"), "{}", error.msg);
        println!("[M2-08] 非法 UUID / 日期 / 布尔 → 400 + 66006，文案点名字段 ✓");
    }

    /// 空白参数等于「不筛」（不能变成 `= ''` 或 `= NULL` 把结果清空）。
    #[test]
    fn blank_filters_mean_no_filter() {
        assert!(
            parse_opt_uuid(Some("   "), "material_id")
                .expect("空白不报错")
                .is_none()
        );
        assert!(parse_opt_date(Some(""), "as_of").expect("空白不报错").is_none());
        // 空白 = 用缺省值（而不是「非法的 true」）：缺省 false 就 false，缺省 true 就 true
        assert!(!parse_bool(Some(""), "include_consumed", false).expect("空白不报错"));
        assert!(parse_bool(Some("  "), "include_consumed", true).expect("空白不报错"));
        assert!(parse_bool(Some("YES"), "include_consumed", false).expect("大小写不敏感"));
        println!("[M2-08] 空白参数 → 不筛 + 用缺省；布尔大小写不敏感 ✓");
    }
}
