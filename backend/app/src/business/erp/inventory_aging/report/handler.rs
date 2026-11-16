//! 库龄分段报表接口层（M2-09）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/inventory-aging/report`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/inventory-aging/report` | `business:erp:inventory-aging:report` | 按 物料 × 仓库 汇总 0-30 / 31-60 / 61-90 / 90 天以上 四档的剩余数量与参考金额 |
//!
//! # 只有一条只读路径，零写 SQL
//!
//! 报表是 `biz_inventory_aging_layers(...)`（V0424 · M2-08）的聚合视图：本文件不开事务、
//! 不写库、也不「顺手修一层」。一次聚合查询本身就是一个快照，所以不学应收 / 应付账龄
//! 开 `REPEATABLE READ`（那两张表要分多条 SQL 取明细与汇总，才需要事务兜住一致性）。
//!
//! # 编排顺序（改顺序会改结果，别随手调）
//!
//! ```text
//! 解析参数（非法 → 400 + 66006）→ 取聚合格 → 透视成行 → 先算合计 → 再截断行
//!                                              ↓                ↓
//!                                     （行序在最老货在前）   （合计是全量，行是前 2000 条）
//! → 批量补物料 / 仓库名字（固定 2 条 SQL）→ 组装 hints → 出参
//! ```
//!
//! 「先算合计再截断」是硬要求：先截断再合计，就等于给用户一张「合计也少了一截」的假报表
//! （单测 `summaries_are_full_even_when_rows_truncate` 用真库盯着）。
//!
//! # 名字怎么补（AGENTS.md 约定 13）
//!
//! 物料 / 仓库是**主数据关联**（要显示「现在叫什么」）⇒ 批量 `LEFT JOIN` 主数据，
//! 一次出表固定 2 条额外 SQL（不是每行各查一次）；软删的主数据按「缺字段」处理（`null`），
//! 历史库龄行照常可读。复用 M2-08 的 `material_briefs` / `warehouse_briefs`。
//!
//! # 错误口径（与 ERP 其余查询接口一致）
//!
//! 参数非法（非法 UUID / 非法日期）→ **400 + 66006（`ERP_FILTER_INVALID`）**，
//! 绝不静默忽略条件 —— 「按物料筛」被吞掉会变成「全公司物料库龄」的假数据。

use axum::extract::Query;
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::dto::default_as_of;
use crate::business::erp::inventory_aging::handler::{parse_opt_date, parse_opt_uuid};
use crate::business::erp::inventory_aging::model::{material_briefs, warehouse_briefs};
use crate::business::erp::inventory_aging::report::dto::{
    self, InventoryAgingReportQuery, InventoryAgingReportRes, MaterialWarehouseAgingRes, REPORT_CODE, REPORT_NAME,
    ROW_LIMIT,
};
use crate::business::erp::inventory_aging::report::model;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0425 的 sys_menu.auth_code 逐字一致） ====================

/// 库龄分段报表的读码。
///
/// 与 `business:erp:inventory-aging:list`（批次层明细，V0424）**分开**：
/// 明细能按批号钻到「哪一批压了多久」，报表是「全公司按物料 × 仓库的库龄与金额分布」，
/// 是两张表。与应收账龄 `business:erp:receivable:aging`、应付账龄
/// `business:erp:report:payable-aging` 单开报表码同一取向。
pub const PERM_REPORT: &str = "business:erp:inventory-aging:report";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const INVENTORY_AGING_REPORT_PERMISSIONS: [&str; 1] = [PERM_REPORT];

// ==================== 报表编排 ====================

/// 报表编排：解析 → 取数 → 透视 → 合计 → 截断 → 补名 → 组装。
///
/// 单测直接调它（不起 HTTP），真库用例也走它 —— 与 M2-08 的 `layer_page_of` 同款：
/// 测的是接口那条路，不是测试自己拼的替身。
pub async fn report_of(query: InventoryAgingReportQuery) -> std::result::Result<InventoryAgingReportRes, ApiError> {
    let material_id = parse_opt_uuid(query.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(query.warehouse_id.as_deref(), "warehouse_id")?;
    let parsed_as_of = parse_opt_date(query.as_of.as_deref(), "as_of")?;
    // 来源要回显（`QUERY` = 请求里传了；`TODAY` = 留空取 CST 今天）：
    // 用户传了却没生效时，只有这一列能看出来
    let as_of_source = if parsed_as_of.is_some() { "QUERY" } else { "TODAY" };
    let as_of: NaiveDate = parsed_as_of.unwrap_or_else(default_as_of);

    let cells = model::cells(as_of, material_id, warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询库龄分段聚合"))?;

    // 透视 → **先合计** → 再截断（顺序不能调，见模块头注释）
    let mut rows = dto::pivot_rows(cells);
    let summary = dto::summarise_rows(&rows);
    let (material_count, warehouse_count) = dto::distinct_counts(&rows);
    let row_count = i64::try_from(rows.len()).unwrap_or(0);
    let qty_total: Decimal = summary.iter().map(|cell| cell.qty).sum();
    let amount_total: Decimal = summary.iter().map(|cell| cell.amount).sum();
    let layer_count: i64 = summary.iter().map(|cell| cell.layer_count).sum();
    let rows_truncated = dto::truncate_rows(&mut rows);

    // 主数据名字：两条批量查询（不是每行各查一次）
    fill_names(&mut rows).await?;

    let mut hints = vec![
        "口径：只统计还剩结存的库龄层（被 FIFO 出库吃满的层不计）；\
         库龄 = 基准日 − 入库业务日期，按 0-30 / 31-60 / 61-90 / 90 天以上 四档汇总"
            .to_string(),
        "数量取「层剩余数量」，金额取「层剩余 × 层入库单价」= 参考金额，\
         与账面移动加权成本（biz_inventory.avg_cost）本来就不相等；库龄是数量分析，金额只作参考"
            .to_string(),
        "跨物料的 `qty_total` 是各物料单位数量的直接相加（件 / 公斤 / 米 …），\
         只能看总量级，不能当作「同一个单位」读"
            .to_string(),
        "库龄不会为负：库龄层只回「业务日期 ≤ 基准日」的流水，补录到未来的入库要到基准日之后才出现".to_string(),
    ];
    if row_count == 0 {
        hints.push(format!(
            "基准日 {as_of} 下没有还剩结存的库龄层（没有数据不是出错）：\
             要么仓里是空的，要么这批库存没有入库流水（直接写 biz_inventory 的行看不见）"
        ));
    }
    if rows_truncated {
        hints.push(format!(
            "共 {row_count} 个 物料 × 仓库 组合，本表只返回前 {ROW_LIMIT} 个（最老货在前）；\
             档位合计与总量是全量，不受行截断影响 —— 要看完请按物料或仓库收窄"
        ));
    }

    Ok(InventoryAgingReportRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        as_of,
        as_of_source: as_of_source.to_string(),
        buckets: dto::bucket_defs(),
        summary,
        qty_total,
        amount_total,
        layer_count,
        row_count,
        material_count,
        warehouse_count,
        row_limit: ROW_LIMIT as u64,
        rows_truncated,
        rows,
        hints,
    })
}

/// 补物料 / 仓库名字（固定 2 条 SQL，无 N+1）。
async fn fill_names(rows: &mut [MaterialWarehouseAgingRes]) -> std::result::Result<(), ApiError> {
    if rows.is_empty() {
        return Ok(());
    }
    let material_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.material_id).ok())
        .collect();
    let materials: std::collections::HashMap<Uuid, crate::business::erp::inventory_aging::model::MaterialBrief> =
        material_briefs(&material_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询库龄报表物料"))?
            .into_iter()
            .map(|brief| (brief.id, brief))
            .collect();
    let warehouse_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.warehouse_id).ok())
        .collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> = warehouse_briefs(&warehouse_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询库龄报表仓库"))?
        .into_iter()
        .map(|(id, code, name)| (id, (code, name)))
        .collect();

    for row in rows.iter_mut() {
        if let Some(id) = Uuid::parse_str(&row.material_id).ok()
            && let Some(brief) = materials.get(&id)
        {
            row.material_code = Some(brief.code.clone());
            row.material_name = Some(brief.name.clone());
            row.spec = brief.spec.clone();
            row.unit_name = brief.unit_name.clone();
        }
        if let Some(id) = Uuid::parse_str(&row.warehouse_id).ok()
            && let Some((code, name)) = warehouses.get(&id)
        {
            row.warehouse_code = Some(code.clone());
            row.warehouse_name = Some(name.clone());
        }
    }
    Ok(())
}

// ==================== HTTP 入口 ====================

/// 库龄分段报表：按 物料 × 仓库 汇总 0-30 / 31-60 / 61-90 / 90 天以上 四档。
#[nik::permission(
    path = "/business/erp/inventory-aging/report",
    method = "GET",
    permission = "business:erp:inventory-aging:report"
)]
#[nik::get("/business/erp/inventory-aging/report")]
pub async fn inventory_aging_report(
    Query(query): Query<InventoryAgingReportQuery>,
) -> ResResult<InventoryAgingReportRes> {
    Ok(Res::ok(report_of(query).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码常量与迁移逐字一致（改码必须同步改 V0425）。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(INVENTORY_AGING_REPORT_PERMISSIONS, [PERM_REPORT]);
        assert_eq!(PERM_REPORT, "business:erp:inventory-aging:report");
        // 与明细码必须不同（两张表、两个开关）
        assert_ne!(PERM_REPORT, crate::business::erp::inventory_aging::handler::PERM_LIST);
        println!("[M2-09] 权限码 business:erp:inventory-aging:report（V0425，与明细码分开）✓");
    }

    /// 非法筛选必须 400 + 66006，且文案点名字段（不能静默当作没传）。
    #[test]
    fn invalid_filters_are_400_with_erp_filter_invalid() {
        let error = parse_opt_uuid(Some("not-a-uuid"), "material_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, cloudpivot_kernel::shared::error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("material_id"), "文案要点名字段：{}", error.msg);

        let error = parse_opt_date(Some("20261116"), "as_of").expect_err("非法日期必须报错");
        assert_eq!(error.code, cloudpivot_kernel::shared::error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("as_of"), "{}", error.msg);
        println!("[M2-09] 非法 UUID / 日期 → 400 + 66006，文案点名字段 ✓");
    }

    /// 空白参数 = 不筛（不能变成 `= ''` 把结果清空）。
    #[test]
    fn blank_filters_mean_no_filter() {
        assert!(
            parse_opt_uuid(Some("   "), "material_id")
                .expect("空白不报错")
                .is_none()
        );
        assert!(parse_opt_date(Some(""), "as_of").expect("空白不报错").is_none());
        println!("[M2-09] 空白参数 → 不筛 / 取缺省基准日 ✓");
    }

    /// 补名：空行不查库（0 条 SQL），不 panic。
    #[tokio::test]
    async fn fill_names_is_noop_on_empty_rows() {
        let mut rows: Vec<MaterialWarehouseAgingRes> = Vec::new();
        fill_names(&mut rows).await.expect("空行不该报错");
        assert!(rows.is_empty());
        println!("[M2-09] 空结果 → 补名阶段 0 条 SQL ✓");
    }
}
