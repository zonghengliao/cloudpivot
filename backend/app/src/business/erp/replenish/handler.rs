//! 补货建议接口层（M2-15）。前缀 `/api` 由 `[router].prefix` 注入，宏内写
//! `/business/erp/replenish/page`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/replenish/page` | `business:erp:replenish:list` | 按 物料 × 仓库 给建议采购量（安全库存 + 在途 + 未清订单） |
//!
//! # 只有一条只读路径，零写 SQL
//!
//! 补货建议是**一张现算的清单**（四个来源的列 → 一次算术），不是独立事实：本文件不开事务、
//! 不写库、也不「顺手把建议量落到某张表」。落表就要在每次出入库、每次到货、每次改水位时
//! 维护，而任何一处漏维护都会让「建议 70」变成一条没人能复核的历史数字 —— 现算永远对得上
//! 当前的四张表，这正是采购下单前要看的那一眼。
//!
//! # 编排顺序（改顺序会改结果，别随手调）
//!
//! ```text
//! 解析参数（非法 → 400 + ERP_FILTER_INVALID）
//!   → 一次扫描（结存 / 水位 / 在途 / 未清订单）
//!   → 逐格算结论（dto::suggest，纯函数）
//!   → 过滤（keep_row：默认只留 PURCHASE）
//!   → 排序（建议量降序 + 三列全序）
//!   → 汇总（**分页前全量**）
//!   → 取本页切片
//!   → hints
//! ```
//!
//! 「先汇总再分页」是硬要求：先分页再汇总，等于给用户一张「本页缺 70」的假合计
//! （与 `slow_moving` 的「先汇总再截断」同一条）。
//!
//! # 为什么不给「生成采购单」这样的写接口
//!
//! 建议量是**建议**：订多少、给哪家供应商、分几批到货，是采购的判断（还有信用额度、
//! 框架协议、最小起订量一堆本版本没有的约束）。把「一键生成采购单」塞进这张卡，等于
//! 顺带定下采购单的生成口径 —— 那是采购模块自己的卡（PRD E2-01）。
//!
//! # 错误口径（与 ERP 其余查询接口一致）
//!
//! 参数非法（非法 UUID / 非法开关值）→ **400 + `ERP_FILTER_INVALID`**，绝不静默忽略条件
//! （`?include_covered=treu` 当 `false` 会让调用方以为「覆盖的行也返回了」）。
//! 读库失败 → 500（`utils::db_error` 统一收口）。

use axum::extract::Query;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::replenish::dto::{
    self, CELL_SCAN_LIMIT, CellFacts, ReplenishFilter, ReplenishRes, ReplenishRowRes, verdict_label,
};
use crate::business::erp::replenish::model::{self, ReplenishCell, ScanFilters};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0430 的 sys_menu.auth_code 逐字一致） ====================

/// 补货建议的读码。
///
/// 与 `business:erp:stock-policy:list`（水位设置）**分开**：能改水位的人与能看补货清单的人
/// 不是同一个岗位（采购看清单、仓管/计划员定水位），单开一个码才能在授权树上单独开关。
/// 它是**读**码而不是写码 —— 本接口一行都不写（`authz_tests::o13` 只拦「写接口挂读码」）。
pub const PERM_LIST: &str = "business:erp:replenish:list";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const REPLENISH_PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 参数解析 ====================

/// `400 + ERP_FILTER_INVALID`：筛选参数非法（不静默忽略）。
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

/// 布尔开关（缺省 `default`；`true/false`、`1/0`、`yes/no`、`on/off`，大小写不敏感）。
///
/// 域外值一律 400：打错的开关静默当 `false` 会让调用方拿到一份**看着对、其实不全**的清单。
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

// ==================== 编排 ====================

/// 补货建议编排：解析 → 扫描 → 逐格算 → 过滤 → 排序 → 汇总 → 分页 → hints。
///
/// 单测与真库用例都直接调它（不起 HTTP），测的是接口那条路。
pub async fn replenish_page_of(filter: ReplenishFilter) -> std::result::Result<ReplenishRes, ApiError> {
    let material_id = parse_opt_uuid(filter.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(filter.warehouse_id.as_deref(), "warehouse_id")?;
    let include_covered = parse_bool(filter.include_covered.as_deref(), "include_covered", false)?;
    let (page, page_size) = dto::normalize_page(filter.page, filter.page_size);

    let filters = ScanFilters {
        material_id,
        warehouse_id,
        keyword: utils::like_pattern(filter.keyword.as_deref()),
    };
    let cells = model::scan_cells(&filters)
        .await
        .map_err(|error| utils::db_error(error, "扫描补货建议"))?;
    let scanned_cells = cells.len();
    let scan_truncated = scanned_cells as i64 >= CELL_SCAN_LIMIT;

    let mut rows: Vec<ReplenishRowRes> = Vec::with_capacity(cells.len());
    for cell in &cells {
        let suggestion = dto::suggest(&CellFacts {
            on_hand: cell.on_hand_qty,
            outbound_transit: cell.outbound_transit_qty,
            open_po: cell.open_po_qty,
            material_safety_stock: cell.material_safety_stock,
            policy: cell.policy,
        });
        if !dto::keep_row(&suggestion, include_covered) {
            continue;
        }
        rows.push(to_row(cell, &suggestion));
    }

    dto::sort_rows(&mut rows);
    let totals = dto::totals_of(&rows);
    let total = rows.len() as u64;
    let page_rows = dto::page_slice(&rows, page, page_size);

    let mut hints = vec![
        "建议采购量 = 目标水位 − 预计可用（预计可用 = 账面结存 − 发货在途 + 未清采购订单未收量）；\
         触发点是最低水位（预计可用严格小于它才补），补到最高水位（未设最高水位则补到最低水位）"
            .to_string(),
        "「在途」= 发货在途（`biz_inventory_in_transit` 里 status=IN_TRANSIT 的量：货已离仓、账面还没扣，\
         与在途库存页同一口径）；「未清订单」= 采购订单已提交/已审批/部分到货且未收完的量。\
         两者不是同一批货，所以分开算"
            .to_string(),
        "未清订单只算 SUBMITTED / APPROVED / PARTIAL_RECEIVED（DRAFT 还没提交、可随时删改，不算供给）；\
         单子被驳回或作废后，下一轮查询建议量会自动回来 —— 这张表每次都是现算的，不落库"
            .to_string(),
        "水位取值走「策略行 > 物料级安全库存 > 不判」三级回退（与安全库存设置页、水位检查同一口径）；\
         最低水位为 0 表示该格明示不预警，就永远不会有建议"
            .to_string(),
        "建议量按 NUMERIC(18,4) 原值给出、**未取整**：下单时请按该物料的计量单位精度 / 最小起订量自行取整".to_string(),
        "同一物料同一仓若有多个成本桶（按库位分成本的仓库），数量按**仓**合计（`inventory_row_count` 是桶数）；\
         补货是按仓补的、不按库位补 —— 否则同一格会被拆成几条建议，每条都单独判水位、建议量凭空变大"
            .to_string(),
    ];
    if include_covered && totals.covered_row_count > 0 {
        hints.push(format!(
            "本次带上了 {} 行「在途覆盖」：账面结存已低于最低水位，但未清订单 + 在途把缺口盖住了 —— \
             这些行**不需要再下单**，出现在列表里是为了解释「为什么这条建议消失了」",
            totals.covered_row_count
        ));
    }
    if scan_truncated {
        hints.push(dto::scan_truncated_hint(scanned_cells));
    }
    if scanned_cells == 0 {
        hints.push(
            "没有扫到任何 物料 × 仓库：要么还没有库存行（`biz_inventory` 为空），要么筛选条件把范围清空了 —— \
             没有库存行的物料不会出现在本表（没有结存可补，请直接按采购需求下单）"
                .to_string(),
        );
    } else if totals.purchase_row_count == 0 {
        hints.push(format!(
            "扫过 {scanned_cells} 格，没有一格跌破再订货点 —— 当前不需要补货（不是出错）；\
             想看「已经跌破水位但被在途盖住」的行，把 include_covered 置为 true"
        ));
    }

    Ok(ReplenishRes {
        scanned_cells: scanned_cells as u64,
        scan_limit: CELL_SCAN_LIMIT as u64,
        scan_truncated,
        total,
        page,
        page_size,
        totals,
        rows: page_rows,
        hints,
    })
}

/// 一格的「事实 + 结论」→ 出参行。
///
/// `verdict_label` 在服务端就翻好：前端三个页面各写一份 `match` 是历史遗留 bug 的温床
/// （一处改了、另两处忘改，同一结论显示两个名字）。
fn to_row(cell: &ReplenishCell, suggestion: &dto::Suggestion) -> ReplenishRowRes {
    ReplenishRowRes {
        inventory_row_count: cell.inventory_row_count,
        material_id: cell.material_id.to_string(),
        material_code: cell.material_code.clone(),
        material_name: cell.material_name.clone(),
        spec: cell.spec.clone(),
        unit_name: cell.unit_name.clone(),
        warehouse_id: cell.warehouse_id.to_string(),
        warehouse_code: cell.warehouse_code.clone(),
        warehouse_name: cell.warehouse_name.clone(),
        on_hand_qty: cell.on_hand_qty,
        outbound_transit_qty: cell.outbound_transit_qty,
        open_po_qty: cell.open_po_qty,
        available_qty: suggestion.available_qty,
        expected_qty: suggestion.expected_qty,
        min_qty: suggestion.min_qty,
        min_source: suggestion.min_source.as_str().to_string(),
        max_qty: suggestion.max_qty,
        target_qty: suggestion.target_qty,
        target_source: suggestion.target_source.to_string(),
        shortage_qty: suggestion.shortage_qty,
        suggested_qty: suggestion.suggested_qty,
        below_min: suggestion.below_min,
        triggered: suggestion.triggered,
        verdict: suggestion.verdict.to_string(),
        verdict_label: verdict_label(suggestion.verdict).to_string(),
    }
}

// ==================== HTTP 入口 ====================

/// 补货建议列表：安全库存 + 发货在途 + 未清采购订单 → 建议采购量。
#[nik::permission(
    path = "/business/erp/replenish/page",
    method = "GET",
    permission = "business:erp:replenish:list"
)]
#[nik::get("/business/erp/replenish/page")]
pub async fn replenish_page_api(Query(filter): Query<ReplenishFilter>) -> ResResult<ReplenishRes> {
    Ok(Res::ok(replenish_page_of(filter).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码常量与迁移逐字一致（改码必须同步改 V0430），且与水位设置码不是同一个开关。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(REPLENISH_PERMISSIONS, [PERM_LIST]);
        assert_eq!(PERM_LIST, "business:erp:replenish:list");
        assert_ne!(PERM_LIST, crate::business::erp::stock_policy::handler::PERM_LIST);
        assert_ne!(PERM_LIST, crate::business::erp::stock_policy::handler::PERM_SAVE);
        println!("[M2-15] 权限码 business:erp:replenish:list（V0430，与水位设置码分开）✓");
    }

    /// 非法筛选必须 400 + `ERP_FILTER_INVALID` 且点名字段（不静默当作没传）。
    #[test]
    fn invalid_filters_are_400_with_erp_filter_invalid() {
        let error = parse_opt_uuid(Some("not-a-uuid"), "material_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("material_id"), "文案要点名字段：{}", error.msg);

        let error = parse_bool(Some("treu"), "include_covered", false).expect_err("非法开关必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("include_covered"), "{}", error.msg);
        println!("[M2-15] 非法 UUID / 非法开关 → 400 + ERP_FILTER_INVALID，文案点名字段 ✓");
    }

    /// 空白参数 = 不筛 / 取默认开关（不能变成 `= ''` 把结果清空）。
    #[test]
    fn blank_filters_mean_no_filter() {
        assert!(
            parse_opt_uuid(Some("   "), "warehouse_id")
                .expect("空白不报错")
                .is_none()
        );
        assert!(parse_opt_uuid(None, "warehouse_id").expect("缺省不报错").is_none());
        assert!(!parse_bool(None, "include_covered", false).expect("缺省"));
        assert!(!parse_bool(Some(""), "include_covered", false).expect("空白取缺省"));
        // 五个别名都认（大小写不敏感）
        for raw in ["true", "TRUE", "1", "yes", "on"] {
            assert!(parse_bool(Some(raw), "include_covered", false).expect(raw));
        }
        for raw in ["false", "FALSE", "0", "no", "off"] {
            assert!(!parse_bool(Some(raw), "include_covered", true).expect(raw));
        }
        println!("[M2-15] 空白 → 不筛 / 缺省开关；true/false 五个别名都认 ✓");
    }
}
