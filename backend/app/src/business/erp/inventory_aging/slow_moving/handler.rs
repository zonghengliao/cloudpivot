//! 呆滞料标记接口层（M2-10）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/inventory-aging/slow-moving`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/inventory-aging/slow-moving` | `business:erp:inventory-aging:slow-moving` | 按 物料 × 仓库 标记库龄超过 N 天仍有剩余的层，给出呆滞数量 / 参考金额 / 占比 |
//!
//! # 只有一条只读路径，零写 SQL
//!
//! 呆滞是库龄的一个**视图**（同一批层 + 一个天数阈值），不是独立事实：本文件不开事务、
//! 不写库、也不「顺手把呆滞标记落到某张表」—— 落表就要在每次出入库与每次改阈值时维护，
//! 而阈值一改全表过期（见 [`super::dto`] 模块头注释「为什么不新开一张表」）。
//!
//! # 编排顺序（改顺序会改结果，别随手调）
//!
//! ```text
//! 解析参数（非法 → 400 + 66006）→ 解阈值（参数 > 配置 > 缺省）→ 取聚合格
//!   → 先算汇总（截断前全量）→ 再挑呆滞行 → 再截断 → 批量补名字 → hints → 出参
//! ```
//!
//! 「先汇总再截断」是硬要求：先截断再汇总，等于给用户一张「扫过 2000 格、呆滞 2000 格」
//! 的假清单（真库用例 `rows_truncate_but_summary_is_full` 盯着）。
//!
//! # 阈值改哪里
//!
//! 持久化阈值是**系统参数** `erp.inventory.slow-moving-days`（V0426 种 90），在
//! 「系统参数」页（`/support/config`，权限码 `support:config:update`）维护即可 ——
//! 这里**不新开写接口**：一个整数值不值得一套 CRUD，而参数页已经带了审计与快照刷新
//! （与 `erp.fx.gain_loss_account` 同一取舍）。出参 `threshold_config_key` 给出跳转锚点。
//!
//! # 名字怎么补（AGENTS.md 约定 13）
//!
//! 物料 / 仓库是**主数据关联**（要显示「现在叫什么」）⇒ 批量 `LEFT JOIN` 主数据，
//! 一次出表固定 2 条额外 SQL（不是每行各查一次）；软删的主数据按「缺字段」处理（`null`）。
//! 复用 M2-08 的 `material_briefs` / `warehouse_briefs`。
//!
//! # 错误口径（与 ERP 其余查询接口一致）
//!
//! 参数非法（非法 UUID / 非法日期 / 非法阈值）→ **400 + 66006（`ERP_FILTER_INVALID`）**，
//! 绝不静默忽略条件。

use axum::extract::Query;
use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::dto::default_as_of;
use crate::business::erp::inventory_aging::handler::{parse_opt_date, parse_opt_uuid};
use crate::business::erp::inventory_aging::model::{material_briefs, warehouse_briefs};
use crate::business::erp::inventory_aging::slow_moving::dto::{
    self, ROW_LIMIT, SLOW_MOVING_CODE, SLOW_MOVING_NAME, SlowMovingQuery, SlowMovingRes, SlowMovingRowRes,
    THRESHOLD_CONFIG_KEY,
};
use crate::business::erp::inventory_aging::slow_moving::model;
use crate::support::config::model::SysConfigModel;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0426 的 sys_menu.auth_code 逐字一致） ====================

/// 呆滞料标记的读码。
///
/// 与 `…:inventory-aging:list`（批次层明细，V0424）、`…:inventory-aging:report`
/// （分段报表，V0425）**分开**：明细是「哪一批压了多久」、报表是「各档分布」、
/// 呆滞料是「超阈值那部分压了多少钱」。三者回答三个问题，且能看分布 ≠ 该看呆滞清单
/// 的处置动作（催销 / 减值），单开一个码才能在授权树上单独开关。
pub const PERM_SLOW_MOVING: &str = "business:erp:inventory-aging:slow-moving";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const INVENTORY_SLOW_MOVING_PERMISSIONS: [&str; 1] = [PERM_SLOW_MOVING];

// ==================== 呆滞料编排 ====================

/// 呆滞料编排：解析 → 解阈值 → 取数 → 汇总 → 挑行 → 截断 → 补名 → 组装。
///
/// 单测直接调它（不起 HTTP），真库用例也走它 —— 与 M2-08 的 `layer_page_of`、
/// M2-09 的 `report_of` 同款：测的是接口那条路，不是测试自己拼的替身。
pub async fn slow_moving_of(query: SlowMovingQuery) -> std::result::Result<SlowMovingRes, ApiError> {
    let material_id = parse_opt_uuid(query.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(query.warehouse_id.as_deref(), "warehouse_id")?;
    let parsed_as_of = parse_opt_date(query.as_of.as_deref(), "as_of")?;
    let as_of_source = if parsed_as_of.is_some() { "QUERY" } else { "TODAY" };
    let as_of: NaiveDate = parsed_as_of.unwrap_or_else(default_as_of);

    // 阈值：请求参数（已校验）> 系统参数 > 缺省；配置坏了回缺省并留告警
    let param_days = dto::parse_threshold_param(query.days.as_deref())
        .map_err(crate::business::erp::inventory_aging::handler::bad_filter)?;
    let config_days = SysConfigModel::read_text(THRESHOLD_CONFIG_KEY).await;
    let threshold = dto::resolve_threshold(param_days, config_days.as_deref());

    let cells = model::cells(as_of, material_id, warehouse_id, threshold.days)
        .await
        .map_err(|error| utils::db_error(error, "查询呆滞料聚合"))?;

    // 先汇总（截断前全量）→ 再挑呆滞行 → 再截断（顺序不能调，见模块头注释）
    let summary = dto::summarise_cells(&cells);
    let mut rows = dto::build_rows(&cells);
    let row_count = summary.slow_row_count;
    let rows_truncated = dto::truncate_rows(&mut rows);

    // 主数据名字：两条批量查询（不是每行各查一次）
    fill_names(&mut rows).await?;

    let mut hints = vec![
        format!(
            "判定口径：「超 N 天无出库」按**批次库龄**算 —— 某个入库层还有剩余（没被出库吃掉）\
             且库龄 > {} 天即为呆滞；本表只列至少有一层呆滞的 物料 × 仓库。\
             不按「这一格最后一次出库距今多久」判定（那会把新到的货一起算老，\
             或让一笔小额出库把 200 天前的老货洗白）",
            threshold.days
        ),
        format!(
            "阈值来源：{}（当前 {} 天）—— 请求参数 days > 系统参数 {THRESHOLD_CONFIG_KEY} > \
             缺省 {} 天；改全局阈值请到「系统参数」页维护该键",
            threshold.source.as_str(),
            threshold.days,
            dto::DEFAULT_THRESHOLD_DAYS
        ),
        "数量取「层剩余数量」，金额取「层剩余 × 层入库单价」= 参考金额，\
         与账面移动加权成本（biz_inventory.avg_cost）本来就不相等；呆滞判定只看库龄天数，\
         金额只回答占压多少"
            .to_string(),
        "跨物料的 `qty_slow` 是各物料单位数量的直接相加（件 / 公斤 / 米 …），\
         只能看总量级，不能当作「同一个单位」读；占比 `slow_ratio` 才是跨物料可比的"
            .to_string(),
        format!(
            "口径互证：阈值取缺省 {} 天时，本表每格的 `qty_slow` 与「库龄分段报表」\
             的「90 天以上」档数量逐格相等（同一库函数、同一基准日）",
            dto::DEFAULT_THRESHOLD_DAYS
        ),
    ];
    if let Some(warning) = threshold.warning {
        hints.push(warning);
    }
    if summary.scanned_row_count == 0 {
        hints.push(format!(
            "基准日 {as_of} 下没有任何还剩结存的库龄层（没有数据不是出错）：\
             要么仓里是空的，要么这批库存没有入库流水（直接写 biz_inventory 的行看不见）"
        ));
    } else if row_count == 0 {
        hints.push(format!(
            "基准日 {as_of} 下扫过 {} 个 物料 × 仓库，没有一格的库龄超过 {} 天 —— \
             这批库存周转正常，不是出错",
            summary.scanned_row_count, threshold.days
        ));
    }
    if rows_truncated {
        hints.push(format!(
            "共 {row_count} 个呆滞 物料 × 仓库 组合，本表只返回前 {ROW_LIMIT} 个（呆滞金额大的在前）；\
             汇总与占比是全量，不受行截断影响 —— 要看完请按物料或仓库收窄"
        ));
    }

    Ok(SlowMovingRes {
        report_code: SLOW_MOVING_CODE.to_string(),
        report_name: SLOW_MOVING_NAME.to_string(),
        as_of,
        as_of_source: as_of_source.to_string(),
        threshold_days: threshold.days,
        threshold_source: threshold.source.as_str().to_string(),
        threshold_config_key: THRESHOLD_CONFIG_KEY.to_string(),
        summary,
        row_count,
        row_limit: ROW_LIMIT as u64,
        rows_truncated,
        rows,
        hints,
    })
}

/// 补物料 / 仓库名字（固定 2 条 SQL，无 N+1）。
async fn fill_names(rows: &mut [SlowMovingRowRes]) -> std::result::Result<(), ApiError> {
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
            .map_err(|error| utils::db_error(error, "批量查询呆滞料物料"))?
            .into_iter()
            .map(|brief| (brief.id, brief))
            .collect();
    let warehouse_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.warehouse_id).ok())
        .collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> = warehouse_briefs(&warehouse_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询呆滞料仓库"))?
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

/// 呆滞料标记：库龄超过 N 天仍有剩余的 物料 × 仓库（N 可配）。
#[nik::permission(
    path = "/business/erp/inventory-aging/slow-moving",
    method = "GET",
    permission = "business:erp:inventory-aging:slow-moving"
)]
#[nik::get("/business/erp/inventory-aging/slow-moving")]
pub async fn inventory_slow_moving(Query(query): Query<SlowMovingQuery>) -> ResResult<SlowMovingRes> {
    Ok(Res::ok(slow_moving_of(query).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码常量与迁移逐字一致（改码必须同步改 V0426）。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(INVENTORY_SLOW_MOVING_PERMISSIONS, [PERM_SLOW_MOVING]);
        assert_eq!(PERM_SLOW_MOVING, "business:erp:inventory-aging:slow-moving");
        // 与明细码 / 报表码必须是三个不同的开关
        assert_ne!(
            PERM_SLOW_MOVING,
            crate::business::erp::inventory_aging::handler::PERM_LIST
        );
        assert_ne!(
            PERM_SLOW_MOVING,
            crate::business::erp::inventory_aging::report::handler::PERM_REPORT
        );
        println!("[M2-10] 权限码 business:erp:inventory-aging:slow-moving（V0426，与明细/报表码分开）✓");
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

        // 阈值非法同样走 400 + 66006，且点名字段 days
        let error = crate::business::erp::inventory_aging::handler::bad_filter(
            dto::parse_threshold_param(Some("abc")).expect_err("非法阈值必须报错"),
        );
        assert_eq!(error.status, 400);
        assert_eq!(error.code, cloudpivot_kernel::shared::error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("days"), "{}", error.msg);
        println!("[M2-10] 非法 UUID / 日期 / 阈值 → 400 + 66006，文案点名字段 ✓");
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
        assert_eq!(dto::parse_threshold_param(Some("")).expect("空白不报错"), None);
        println!("[M2-10] 空白参数 → 不筛 / 取缺省基准日 / 取配置阈值 ✓");
    }

    /// 补名：空行不查库（0 条 SQL），不 panic。
    #[tokio::test]
    async fn fill_names_is_noop_on_empty_rows() {
        let mut rows: Vec<SlowMovingRowRes> = Vec::new();
        fill_names(&mut rows).await.expect("空行不该报错");
        assert!(rows.is_empty());
        println!("[M2-10] 空结果 → 补名阶段 0 条 SQL ✓");
    }
}
