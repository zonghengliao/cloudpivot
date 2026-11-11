//! 成本计算单导出 xlsx（成本核算 · ARCH-9 §5.2 #32 / §5.4「导出」· COST-B4-1）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//!
//! | 章节 | 本模块用到的地方 |
//! |---|---|
//! | §5.2 #32 | `GET /business/erp/cost-period/{id}/cost-sheet?format=xlsx`，权限 `cost-period:list` |
//! | §5.4 | 「导出：成本计算单 xlsx（后端 `xlsx` feature 已启用）」 |
//! | §4.6 | 计算单口径（约当产量 / 单位成本 / 完工转出 / 期末在制）—— 导出**只搬运、不重算** |
//!
//! # 只有一条数据源：`cost_period_handler::detail_of`
//!
//! 导出件的主表逐格来自页面步③ 用的那个 `CostPeriodDetailRes`（`items` + `items_summary`），
//! 合计行取 `items_summary`（页面表尾的「投入 = 完工转出 + 期末在制」同款）。
//! **不另写一份 SQL 重算**：两套算口径 = 两套数，「导出和页面不一样」是财务最不能接受的 bug。
//!
//! 唯一多出来的一列是「其中：分摊转入」—— 它不在 `biz_cost_period_item` 上（那张表没有分摊列，
//! 分摊已经汇总进 `oh_cur`，见 `cost_period_sheet_model` 模块头），只从
//! `biz_cost_alloc_result` 按 `(period_code, cost_object_id)` 聚合出来后**解释**制造费用的构成。
//!
//! # 为什么走 `impexp::engine::write_workbook` 而不是 impexp 的导出注册表
//!
//! 与报表导出的理由同构（`report.rs` 的余额表导出注释）：注册表的筛选只有 keyword/status，
//! 没有「批次 id」维度；而且本导出是**一次性把整张计算单拉出来**（不受分页限制），
//! 不是「表扫描 + keyset 分页」。所以直接复用同一个 xlsx 写出器与同一套错误码。
//!
//! # 大对象（1000 行）不超时
//!
//! 全程只有 4 次库读（单头 / 计算单 / 现算合计 / 分摊结果）与一次 O(n) 内存拼装，
//! 没有逐行查库、没有 N+1。1000 行实测见
//! `docs/evidence/2026-11-11-cost-b4-1-cost-sheet-export/`。
//!
//! # 数字是文本还是数字
//!
//! 与本仓其余导出（报表 / 账龄 / 现金流量）一致：**金额与数量都写成格式化文本**
//! （千分位 / 固定小数位）。理由有二：
//! 1. 与页面显示逐字一致 —— 验收要求「数字与页面一致」，页面就是 `19,000.00` 这种写法；
//! 2. 金额是 `NUMERIC(18,4)`，落成 Excel 浮点会丢精度（导出件比账面少一分钱是事故）。

use std::collections::HashMap;
use std::str::FromStr;

use axum::extract::{Path, Query};
use axum::response::Response;
use nik::prelude::chrono::Local;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::Deserialize;

use crate::business::erp::cost_period_dto;
use crate::business::erp::cost_period_dto::{CostPeriodDetailRes, CostSheetItemRes};
use crate::business::erp::cost_period_handler::detail_of;
use crate::business::erp::cost_period_sheet_model::{self as model, AllocSheetRow};
use crate::business::erp::report::money_text;
use crate::business::impexp::engine::{WorkbookSheet, write_workbook};
use crate::shared::error_code;

// ==================== 表头（主表 20 列） ====================
//
// 列序 = 「成本对象 → 期初 → 料 / 工 / 费（含分摊）→ 约当 → 单位成本 → 完工 → 在制 → 状态」，
// 与页面步③ 的读表顺序一致（人从左往右读，先看是谁、再看待摊进来的钱、最后看结果）。

/// 成本对象号（`biz_cost_object.object_no`）。
pub const COL_OBJECT_NO: &str = "成本对象号";
/// 产出品名称（`biz_material.material_name`）。
pub const COL_PRODUCT: &str = "产出品";
/// 计算单状态（`CALC` 已计算 / `SETTLED` 已结算）。
pub const COL_ITEM_STATUS: &str = "计算单状态";
/// 期初在制数量。
pub const COL_WIP_BEG_QTY: &str = "期初在制数量";
/// 期初在制成本。
pub const COL_WIP_BEG_COST: &str = "期初在制成本";
/// 本期材料投入。
pub const COL_MATERIAL: &str = "本期材料投入";
/// 本期人工投入。
pub const COL_LABOR: &str = "本期人工投入";
/// 本期制造费用（**含**分摊转入）。
pub const COL_OVERHEAD: &str = "本期制造费用";
/// 其中：分摊转入（只解释构成，不参与计算）。
pub const COL_ALLOC: &str = "其中：分摊转入";
/// 约当产量（材料）。
pub const COL_EQ_MATERIAL: &str = "约当产量（材料）";
/// 约当产量（转换成本）。
pub const COL_EQ_CONVERSION: &str = "约当产量（转换成本）";
/// 单位材料成本（6 位）。
pub const COL_UNIT_MATERIAL: &str = "单位材料成本";
/// 单位人工成本（6 位）。
pub const COL_UNIT_LABOR: &str = "单位人工成本";
/// 单位制造费用（6 位）。
pub const COL_UNIT_OVERHEAD: &str = "单位制造费用";
/// 本期完工数量。
pub const COL_COMPLETED_QTY: &str = "完工数量";
/// 完工转出成本。
pub const COL_FINISHED: &str = "完工转出成本";
/// 期末在制数量。
pub const COL_WIP_END_QTY: &str = "期末在制数量";
/// 期末在制完工进度 %。
pub const COL_WIP_END_PCT: &str = "期末在制完工进度(%)";
/// 期末在制成本。
pub const COL_WIP_END_COST: &str = "期末在制成本";
/// 完工入库单价（结算时写进 `biz_stock_in_item.cost_price`）。
pub const COL_UNIT_FINISHED: &str = "完工入库单价";

/// 主表列顺序（表头字符串即列名，`write_workbook` 按它取值）。
pub const MAIN_HEADERS: [&str; 20] = [
    COL_OBJECT_NO,
    COL_PRODUCT,
    COL_ITEM_STATUS,
    COL_WIP_BEG_QTY,
    COL_WIP_BEG_COST,
    COL_MATERIAL,
    COL_LABOR,
    COL_OVERHEAD,
    COL_ALLOC,
    COL_EQ_MATERIAL,
    COL_EQ_CONVERSION,
    COL_UNIT_MATERIAL,
    COL_UNIT_LABOR,
    COL_UNIT_OVERHEAD,
    COL_COMPLETED_QTY,
    COL_FINISHED,
    COL_WIP_END_QTY,
    COL_WIP_END_PCT,
    COL_WIP_END_COST,
    COL_UNIT_FINISHED,
];

/// 主表 sheet 名。
pub const MAIN_SHEET: &str = "成本计算单";
/// 分摊明细 sheet 名。
pub const ALLOC_SHEET: &str = "分摊明细";

/// 分摊明细列（一行 = 一条规则 × 一个对象 × 一个科目）。
pub const COL_ALLOC_OBJECT_NO: &str = "成本对象号";
/// 被摊车间（结果表里的名字快照）。
pub const COL_ALLOC_DEPT: &str = "被摊车间";
/// 命中规则。
pub const COL_ALLOC_RULE: &str = "分摊规则";
/// 被摊制造费用科目。
pub const COL_ALLOC_ACCOUNT: &str = "被摊科目";
/// 本对象基数。
pub const COL_ALLOC_BASIS: &str = "基数";
/// 全车间基数合计（快照）。
pub const COL_ALLOC_BASIS_TOTAL: &str = "全车间基数";
/// 待分摊总额（快照）。
pub const COL_ALLOC_POOL: &str = "待分摊总额";
/// 摊到本对象的金额（含尾差）。
pub const COL_ALLOC_AMOUNT: &str = "分摊金额";
/// 尾差调整（最大余数法）。
pub const COL_ALLOC_ADJ: &str = "尾差调整";

/// 分摊明细列顺序。
pub const ALLOC_HEADERS: [&str; 9] = [
    COL_ALLOC_OBJECT_NO,
    COL_ALLOC_DEPT,
    COL_ALLOC_RULE,
    COL_ALLOC_ACCOUNT,
    COL_ALLOC_BASIS,
    COL_ALLOC_BASIS_TOTAL,
    COL_ALLOC_POOL,
    COL_ALLOC_AMOUNT,
    COL_ALLOC_ADJ,
];

// ==================== 数值格式化（纯逻辑） ====================

/// `NUMERIC(18,4)` 数量 → 最多 4 位小数、去掉尾随 0（与页面 `qtyText` 同款：`800.0000` → `800`）。
///
/// 解析失败**原样回显**而不是当 0：库里出现非数字说明数据坏了，导出件把坏值带出去
/// 比悄悄写个 0 更好查（`0` 会看起来像「这期没数」）。
pub fn qty_text(raw: &str) -> String {
    match Decimal::from_str(raw.trim()) {
        Ok(value) => trim_zeros(value.round_dp(4)),
        Err(_) => raw.to_string(),
    }
}

/// `NUMERIC(18,6)` 单位成本 → 固定 6 位小数（与页面 `unitText` 同款；单价四舍五入会丢钱）。
pub fn unit_text(raw: &str) -> String {
    match Decimal::from_str(raw.trim()) {
        Ok(value) => format!("{:.6}", value.round_dp(6)),
        Err(_) => raw.to_string(),
    }
}

/// 金额（4 位 → 千分位 + 2 位小数）。与报表导出复用同一个 `report::money_text`，
/// 全仓导出的金额写法只有一份实现。
pub fn amount_text(raw: &str) -> String {
    money_text(Decimal::from_str(raw.trim()).ok())
}

/// 百分比：`50.00` → `50`（最多 4 位小数，与数量同口径）。
pub fn pct_text(raw: &str) -> String {
    match Decimal::from_str(raw.trim()) {
        Ok(value) => format!("{}%", trim_zeros(value.round_dp(4))),
        Err(_) => raw.to_string(),
    }
}

/// 去掉尾随 0（`800.0000` → `800`，`0.0000` → `0`）。
fn trim_zeros(value: Decimal) -> String {
    let text = value.to_string();
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// 计算单状态中文名（与前端 `COST_ITEM_STATUS_LABELS` 逐字一致）。
pub fn item_status_label(status: &str) -> &'static str {
    match status {
        "CALC" => "已计算",
        "SETTLED" => "已结算",
        _ => "未知状态",
    }
}

// ==================== 主表拼装（纯逻辑，单测覆盖） ====================

/// 按成本对象汇总「分摊转入」（导出主表的「其中：分摊转入」列）。
///
/// 键用 `cost_object_id` 的字符串形式（与 `CostSheetItemRes.cost_object_id` 同形），
/// 不做 UUID 解析：解析失败就当没摊过，比整张导出 400 好。
pub fn alloc_by_object(allocs: &[AllocSheetRow]) -> HashMap<String, Decimal> {
    let mut map: HashMap<String, Decimal> = HashMap::new();
    for row in allocs {
        let key = row.cost_object_id.to_string();
        let entry = map.entry(key).or_insert(Decimal::ZERO);
        *entry += row.alloc_amount;
    }
    map
}

/// 主表行：1 行表头信息 + N 行计算单 + 1 行合计（与报表导出的形状一致）。
///
/// * 表头信息行让文件脱离页面也能看懂（批次号 / 期间 / 批次状态 / 制单人 / 导出时间）；
/// * 合计行取 `detail.items_summary`（= 页面表尾的现算合计），
///   并用「投入 = 完工转出 + 期末在制 ✓/✗」把守恒结论写进文件。
pub fn main_rows(detail: &CostPeriodDetailRes, alloc: &HashMap<String, Decimal>) -> Vec<HashMap<String, String>> {
    let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(detail.items.len() + 2);

    // ① 表头信息行（一行塞 5 格；列名不复用，避免同一个 key 被覆盖两遍）
    let mut info: HashMap<String, String> = HashMap::new();
    info.insert(COL_OBJECT_NO.to_string(), format!("批次：{}", detail.batch_no));
    info.insert(COL_PRODUCT.to_string(), format!("期间：{}", detail.period_code));
    info.insert(
        COL_ITEM_STATUS.to_string(),
        format!("批次状态：{}", cost_period_dto::status_label(&detail.status)),
    );
    info.insert(
        COL_WIP_BEG_QTY.to_string(),
        format!("制单：{}", detail.created_by_name.as_deref().unwrap_or("—")),
    );
    info.insert(
        COL_WIP_BEG_COST.to_string(),
        format!("导出：{}", Local::now().format("%Y-%m-%d %H:%M:%S")),
    );
    rows.push(info);

    // ② 计算单行（逐格取自 `items`，不重算）
    let mut total_beg_cost = Decimal::ZERO;
    let mut total_alloc = Decimal::ZERO;
    for item in &detail.items {
        total_beg_cost += decimal_of(&item.wip_beg_cost);
        let alloc_amount = alloc.get(&item.cost_object_id).copied().unwrap_or(Decimal::ZERO);
        total_alloc += alloc_amount;
        rows.push(item_row(item, alloc_amount));
    }

    // ③ 合计行（口径 = `items_summary`，与页面表尾同一份数）
    let summary = &detail.items_summary;
    let mut total: HashMap<String, String> = HashMap::new();
    total.insert(
        COL_OBJECT_NO.to_string(),
        format!("合计（{} 个对象）", summary.object_count),
    );
    total.insert(COL_WIP_BEG_COST.to_string(), money_text(Some(total_beg_cost)));
    total.insert(COL_MATERIAL.to_string(), amount_text(&summary.total_material));
    total.insert(COL_LABOR.to_string(), amount_text(&summary.total_labor));
    total.insert(COL_OVERHEAD.to_string(), amount_text(&summary.total_overhead));
    total.insert(COL_ALLOC.to_string(), money_text(Some(total_alloc)));
    total.insert(COL_FINISHED.to_string(), amount_text(&summary.total_finished));
    total.insert(COL_WIP_END_COST.to_string(), amount_text(&summary.total_wip));
    total.insert(
        COL_ITEM_STATUS.to_string(),
        conservation_text(&summary.total_cost, &summary.total_finished, &summary.total_wip),
    );
    rows.push(total);

    rows
}

/// 一行计算单 → 导出行。
fn item_row(item: &CostSheetItemRes, alloc_amount: Decimal) -> HashMap<String, String> {
    let mut row: HashMap<String, String> = HashMap::with_capacity(MAIN_HEADERS.len());
    row.insert(
        COL_OBJECT_NO.to_string(),
        item.object_no.clone().unwrap_or_else(|| item.cost_object_id.clone()),
    );
    row.insert(COL_PRODUCT.to_string(), item.product_name.clone().unwrap_or_default());
    row.insert(COL_ITEM_STATUS.to_string(), item_status_label(&item.status).to_string());
    row.insert(COL_WIP_BEG_QTY.to_string(), qty_text(&item.wip_beg_qty));
    row.insert(COL_WIP_BEG_COST.to_string(), amount_text(&item.wip_beg_cost));
    row.insert(COL_MATERIAL.to_string(), amount_text(&item.mat_cur));
    row.insert(COL_LABOR.to_string(), amount_text(&item.labor_cur));
    row.insert(COL_OVERHEAD.to_string(), amount_text(&item.oh_cur));
    row.insert(COL_ALLOC.to_string(), money_text(Some(alloc_amount)));
    row.insert(COL_EQ_MATERIAL.to_string(), qty_text(&item.equiv_material));
    row.insert(COL_EQ_CONVERSION.to_string(), qty_text(&item.equiv_conversion));
    row.insert(COL_UNIT_MATERIAL.to_string(), unit_text(&item.unit_material));
    row.insert(COL_UNIT_LABOR.to_string(), unit_text(&item.unit_labor));
    row.insert(COL_UNIT_OVERHEAD.to_string(), unit_text(&item.unit_overhead));
    row.insert(COL_COMPLETED_QTY.to_string(), qty_text(&item.completed_qty));
    row.insert(COL_FINISHED.to_string(), amount_text(&item.finished_cost));
    row.insert(COL_WIP_END_QTY.to_string(), qty_text(&item.wip_end_qty));
    row.insert(COL_WIP_END_PCT.to_string(), pct_text(&item.completion_pct));
    row.insert(COL_WIP_END_COST.to_string(), amount_text(&item.wip_end_cost));
    row.insert(COL_UNIT_FINISHED.to_string(), unit_text(&item.unit_finished_cost));
    row
}

/// 守恒结论文案：`投入 = 完工转出 + 期末在制 ✓`（与页面表尾同一个式子）。
fn conservation_text(total_cost: &str, total_finished: &str, total_wip: &str) -> String {
    let left = decimal_of(total_cost);
    let right = decimal_of(total_finished) + decimal_of(total_wip);
    let mark = if left == right { "✓" } else { "✗" };
    format!(
        "投入 {} = 完工转出 {} + 期末在制 {} {mark}",
        money_text(Some(left)),
        money_text(Some(decimal_of(total_finished))),
        money_text(Some(decimal_of(total_wip)))
    )
}

/// 分摊明细行（一行分摊结果 = 一行）。
pub fn alloc_rows(allocs: &[AllocSheetRow]) -> Vec<HashMap<String, String>> {
    allocs
        .iter()
        .map(|row| {
            let mut out: HashMap<String, String> = HashMap::with_capacity(ALLOC_HEADERS.len());
            out.insert(
                COL_ALLOC_OBJECT_NO.to_string(),
                row.object_no.clone().unwrap_or_else(|| row.cost_object_id.to_string()),
            );
            out.insert(COL_ALLOC_DEPT.to_string(), row.dept_name.clone());
            out.insert(
                COL_ALLOC_RULE.to_string(),
                match (&row.rule_code, &row.rule_name) {
                    (Some(code), Some(name)) => format!("{code} {name}"),
                    (Some(code), None) => code.clone(),
                    (None, Some(name)) => name.clone(),
                    (None, None) => "—".to_string(),
                },
            );
            out.insert(COL_ALLOC_ACCOUNT.to_string(), row.account_code.clone());
            out.insert(COL_ALLOC_BASIS.to_string(), qty_text(&row.basis_value.to_string()));
            out.insert(
                COL_ALLOC_BASIS_TOTAL.to_string(),
                qty_text(&row.basis_total.to_string()),
            );
            out.insert(COL_ALLOC_POOL.to_string(), amount_text(&row.pool_amount.to_string()));
            out.insert(COL_ALLOC_AMOUNT.to_string(), amount_text(&row.alloc_amount.to_string()));
            out.insert(COL_ALLOC_ADJ.to_string(), amount_text(&row.rounding_adj.to_string()));
            out
        })
        .collect()
}

/// 拼出导出文件（主表 + 有分摊时才追加「分摊明细」）。
///
/// 没有分摊结果时不写空 sheet：一个只有表头的 sheet 会让人以为「本期一分钱没摊」，
/// 而真实情况通常是「还没做过分摊」—— 那是两件事。
pub fn build_workbook(
    detail: &CostPeriodDetailRes,
    allocs: &[AllocSheetRow],
) -> std::result::Result<Vec<u8>, ApiError> {
    let alloc_map = alloc_by_object(allocs);
    let main_data = main_rows(detail, &alloc_map);
    let mut sheets: Vec<WorkbookSheet<'_>> = Vec::with_capacity(2);
    sheets.push(WorkbookSheet::new(MAIN_SHEET, &MAIN_HEADERS, &main_data));
    let alloc_data = alloc_rows(allocs);
    if !alloc_data.is_empty() {
        sheets.push(WorkbookSheet::new(ALLOC_SHEET, &ALLOC_HEADERS, &alloc_data));
    }
    write_workbook(&sheets)
}

/// 导出文件名：`成本计算单_{批次号}.xlsx`。
///
/// 批次号本身含期间（`CS-202610-0001`），再拼期间就是重复信息；不带时间戳是故意的
/// —— 同一批次的导出件应当同名覆盖（避免本地攒出 `_20261111173000` 一堆副本）。
pub fn file_name(detail: &CostPeriodDetailRes) -> String {
    format!("成本计算单_{}.xlsx", detail.batch_no)
}

/// `format` 入参归一化：缺省即为 `xlsx`，其它值 400（不静默降级成 xlsx）。
pub fn normalize_format(raw: Option<&str>) -> std::result::Result<&'static str, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok("xlsx"),
        Some(text) if text.eq_ignore_ascii_case("xlsx") => Ok("xlsx"),
        Some(text) => Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("format 只支持 xlsx（收到「{text}」）：设计 §5.2 #32 只定义了成本计算单 xlsx 导出"),
        )),
    }
}

/// 宽松解析：解析不了当 0（只用于**合计行**的累加；单行显示仍走 [`qty_text`] 等原样回显）。
fn decimal_of(raw: &str) -> Decimal {
    Decimal::from_str(raw.trim()).unwrap_or(Decimal::ZERO)
}

// ==================== HTTP 入口 ====================

/// 成本计算单导出查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostSheetExportQuery {
    /// 导出格式（设计 §5.2 #32 的 `?format=xlsx`；缺省 xlsx）。
    pub format: Option<String>,
}

/// 成本计算单导出（xlsx 文件流，不走 `Res<T>` 统一响应体）。
#[nik::permission(
    path = "/business/erp/cost-period/{id}/cost-sheet",
    method = "GET",
    permission = "business:erp:cost-period:list"
)]
#[nik::get("/business/erp/cost-period/{id}/cost-sheet")]
pub async fn cost_period_cost_sheet_api(
    Path(id): Path<String>,
    Query(query): Query<CostSheetExportQuery>,
) -> std::result::Result<Response, ApiError> {
    normalize_format(query.format.as_deref())?;
    let id = cost_period_dto::parse_uuid(&id, "id")?;
    // 同一份数据源：详情页与导出都是 detail_of（单头 + 计算单 + 现算合计 + 有效边）
    let detail = detail_of(id).await?;
    let allocs = model::alloc_sheet_rows(&detail.period_code)
        .await
        .map_err(|error| crate::utils::db_error(error, "查询成本分摊结果"))?;
    let bytes = build_workbook(&detail, &allocs)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &file_name(&detail),
    ))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost_period_handler::PERM_LIST;
    use nik::uuid::Uuid;

    /// 造一行计算单（§4.7.5 算例 A 的完工 / 在制拆分：19000 + 4000 + 4000 = 27000）。
    fn item(object_no: &str, mat: &str, labor: &str, oh: &str, finished: &str, wip: &str) -> CostSheetItemRes {
        CostSheetItemRes {
            id: Uuid::nil().to_string(),
            cost_object_id: Uuid::nil().to_string(),
            object_no: Some(object_no.to_string()),
            product_name: Some("A 型电机".to_string()),
            wip_beg_qty: "0.0000".to_string(),
            wip_beg_cost: "0.0000".to_string(),
            completed_qty: "800.0000".to_string(),
            wip_end_qty: "200.0000".to_string(),
            completion_pct: "50.00".to_string(),
            equiv_material: "1000.0000".to_string(),
            equiv_conversion: "900.0000".to_string(),
            mat_cur: mat.to_string(),
            labor_cur: labor.to_string(),
            oh_cur: oh.to_string(),
            unit_material: "19.000000".to_string(),
            unit_labor: "4.444444".to_string(),
            unit_overhead: "4.444444".to_string(),
            finished_cost: finished.to_string(),
            wip_end_cost: wip.to_string(),
            unit_finished_cost: "27.888888".to_string(),
            stock_in_id: None,
            status: "CALC".to_string(),
        }
    }

    fn detail(items: Vec<CostSheetItemRes>) -> CostPeriodDetailRes {
        let mat: Decimal = items.iter().map(|item| decimal_of(&item.mat_cur)).sum();
        let labor: Decimal = items.iter().map(|item| decimal_of(&item.labor_cur)).sum();
        let oh: Decimal = items.iter().map(|item| decimal_of(&item.oh_cur)).sum();
        let finished: Decimal = items.iter().map(|item| decimal_of(&item.finished_cost)).sum();
        let wip: Decimal = items.iter().map(|item| decimal_of(&item.wip_end_cost)).sum();
        let cost = mat + labor + oh;
        CostPeriodDetailRes {
            id: Uuid::nil().to_string(),
            org_id: Uuid::nil().to_string(),
            period_code: "2026-10".to_string(),
            batch_no: "CS-202610-0001".to_string(),
            status: "DRAFT".to_string(),
            labor_source: "MANUAL".to_string(),
            object_count: i32::try_from(items.len()).unwrap_or(0),
            total_material: "0".to_string(),
            total_labor: "0".to_string(),
            total_overhead: "0".to_string(),
            total_cost: "0".to_string(),
            total_finished: "0".to_string(),
            total_wip: "0".to_string(),
            approval_instance_id: None,
            labor_voucher_id: None,
            overhead_voucher_id: None,
            finish_voucher_id: None,
            settled_at: None,
            settled_by: None,
            settled_by_name: None,
            reversed_at: None,
            reverse_reason: None,
            cancel_reason: None,
            remark: None,
            created_by_name: Some("张三".to_string()),
            created_at: "2026-11-11T00:00:00+08:00".to_string(),
            updated_at: "2026-11-11T00:00:00+08:00".to_string(),
            items_summary: cost_period_dto::CostPeriodSummary {
                object_count: i64::try_from(items.len()).unwrap_or(0),
                total_material: mat.to_string(),
                total_labor: labor.to_string(),
                total_overhead: oh.to_string(),
                total_cost: cost.to_string(),
                total_finished: finished.to_string(),
                total_wip: wip.to_string(),
            },
            items,
            next_statuses: vec!["SUBMITTED".to_string()],
            legal_edges: vec![("DRAFT".to_string(), "SUBMITTED".to_string())],
        }
    }

    fn alloc(object_no: &str, amount: &str) -> AllocSheetRow {
        AllocSheetRow {
            cost_object_id: Uuid::nil(),
            object_no: Some(object_no.to_string()),
            dept_name: "装配车间".to_string(),
            rule_code: Some("AR-001".to_string()),
            rule_name: Some("装配车间制造费用按工时".to_string()),
            account_code: "4101.01".to_string(),
            basis_value: Decimal::from(120),
            basis_total: Decimal::from(360),
            pool_amount: Decimal::from(3000),
            alloc_amount: Decimal::from_str(amount).expect("金额"),
            rounding_adj: Decimal::ZERO,
        }
    }

    // ---------- 列覆盖（卡面验收：对象 / 料 / 工 / 费 / 分摊 / 完工 / 在制） ----------

    #[test]
    fn headers_cover_all_required_dimensions() {
        let joined = MAIN_HEADERS.join("|");
        for keyword in ["成本对象号", "材料", "人工", "制造费用", "分摊", "完工", "在制"] {
            assert!(
                joined.contains(keyword),
                "导出列必须含「{keyword}」，当前表头：{joined}"
            );
        }
        // 约当产量与单位成本是计算单的核心口径（设计 §5.2 #32「对象 × 要素 × 约当 × 单位成本」）
        assert!(joined.contains("约当产量"));
        assert!(joined.contains("单位材料成本"));
        // 20 列，无重复列名（HashMap key 重复会静默丢列）
        let unique: std::collections::HashSet<&&str> = MAIN_HEADERS.iter().collect();
        assert_eq!(unique.len(), MAIN_HEADERS.len(), "表头有重复列名");
    }

    // ---------- 数字与页面一致（同一份数据源） ----------

    #[test]
    fn rows_match_detail_and_summary_numbers() {
        let sheet = detail(vec![item(
            "CB-202610-0001",
            "19000.0000",
            "4000.0000",
            "4000.0000",
            "22311.1104",
            "4688.8896",
        )]);
        let rows = main_rows(&sheet, &HashMap::new());
        // 1 表头信息 + 1 计算单 + 1 合计
        assert_eq!(rows.len(), 3);

        let line = &rows[1];
        assert_eq!(line.get(COL_OBJECT_NO).map(String::as_str), Some("CB-202610-0001"));
        assert_eq!(line.get(COL_MATERIAL).map(String::as_str), Some("19,000.00"));
        assert_eq!(line.get(COL_LABOR).map(String::as_str), Some("4,000.00"));
        assert_eq!(line.get(COL_OVERHEAD).map(String::as_str), Some("4,000.00"));
        assert_eq!(
            line.get(COL_ALLOC).map(String::as_str),
            Some("0.00"),
            "没有分摊时该列为 0"
        );
        assert_eq!(line.get(COL_FINISHED).map(String::as_str), Some("22,311.11"));
        assert_eq!(line.get(COL_WIP_END_COST).map(String::as_str), Some("4,688.89"));
        assert_eq!(line.get(COL_WIP_END_QTY).map(String::as_str), Some("200"));
        assert_eq!(line.get(COL_WIP_END_PCT).map(String::as_str), Some("50%"));
        assert_eq!(line.get(COL_UNIT_MATERIAL).map(String::as_str), Some("19.000000"));
        assert_eq!(line.get(COL_UNIT_FINISHED).map(String::as_str), Some("27.888888"));
        assert_eq!(line.get(COL_ITEM_STATUS).map(String::as_str), Some("已计算"));

        // 合计行 = `items_summary`（= 页面表尾），且守恒结论写进文件
        let total = &rows[2];
        assert_eq!(total.get(COL_OBJECT_NO).map(String::as_str), Some("合计（1 个对象）"));
        assert_eq!(total.get(COL_MATERIAL).map(String::as_str), Some("19,000.00"));
        assert_eq!(total.get(COL_FINISHED).map(String::as_str), Some("22,311.11"));
        assert_eq!(total.get(COL_WIP_END_COST).map(String::as_str), Some("4,688.89"));
        let check = total.get(COL_ITEM_STATUS).expect("守恒结论");
        assert!(check.ends_with('✓'), "算例 A 必须平：{check}");

        // 表头信息行（文件脱离页面也能看懂）
        let info = &rows[0];
        assert_eq!(
            info.get(COL_OBJECT_NO).map(String::as_str),
            Some("批次：CS-202610-0001")
        );
        assert_eq!(info.get(COL_PRODUCT).map(String::as_str), Some("期间：2026-10"));
        assert!(info.get(COL_ITEM_STATUS).expect("状态").contains("草稿"));
    }

    #[test]
    fn alloc_column_comes_from_alloc_results_not_from_overhead() {
        // 制造费用 4000 里含分摊 3000 → 列上写 3000，而「本期制造费用」仍是 4000（不重算不覆盖）
        let mut line = item(
            "CB-202610-0001",
            "19000.0000",
            "4000.0000",
            "4000.0000",
            "22311.1104",
            "4688.8896",
        );
        line.cost_object_id = Uuid::nil().to_string();
        let rows = main_rows(
            &detail(vec![line]),
            &alloc_by_object(&[alloc("CB-202610-0001", "3000")]),
        );
        assert_eq!(rows[1].get(COL_ALLOC).map(String::as_str), Some("3,000.00"));
        assert_eq!(rows[1].get(COL_OVERHEAD).map(String::as_str), Some("4,000.00"));
        assert_eq!(
            rows[2].get(COL_ALLOC).map(String::as_str),
            Some("3,000.00"),
            "合计行也要有分摊"
        );
    }

    #[test]
    fn alloc_rows_render_rule_and_amounts() {
        let rows = alloc_rows(&[alloc("CB-202610-0001", "3000.3334")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get(COL_ALLOC_RULE).map(String::as_str),
            Some("AR-001 装配车间制造费用按工时")
        );
        assert_eq!(rows[0].get(COL_ALLOC_ACCOUNT).map(String::as_str), Some("4101.01"));
        assert_eq!(rows[0].get(COL_ALLOC_AMOUNT).map(String::as_str), Some("3,000.33"));
        assert_eq!(rows[0].get(COL_ALLOC_BASIS).map(String::as_str), Some("120"));
        assert_eq!(rows[0].get(COL_ALLOC_BASIS_TOTAL).map(String::as_str), Some("360"));
    }

    // ---------- 格式化口径（与页面 qtyText / unitText 对齐） ----------

    #[test]
    fn number_formatting_matches_page() {
        assert_eq!(qty_text("800.0000"), "800");
        assert_eq!(qty_text("0.0000"), "0");
        assert_eq!(qty_text("22311.1104"), "22311.1104");
        assert_eq!(qty_text("坏值"), "坏值", "坏值原样回显，不当 0");
        assert_eq!(unit_text("19"), "19.000000");
        assert_eq!(unit_text("4.4444444"), "4.444444", "单位成本 6 位，四舍五入");
        assert_eq!(amount_text("19000"), "19,000.00");
        assert_eq!(amount_text("-1234.5"), "-1,234.50");
        assert_eq!(pct_text("50.00"), "50%");
        assert_eq!(pct_text("33.3333"), "33.3333%");
    }

    #[test]
    fn format_param_only_accepts_xlsx() {
        assert_eq!(normalize_format(None).expect("缺省"), "xlsx");
        assert_eq!(normalize_format(Some("")).expect("空串"), "xlsx");
        assert_eq!(normalize_format(Some(" XLSX ")).expect("大小写"), "xlsx");
        let error = normalize_format(Some("csv")).expect_err("csv 应 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("只支持 xlsx"), "报文要说明只支持什么：{}", error.msg);
    }

    #[test]
    fn file_name_uses_batch_no() {
        let sheet = detail(vec![]);
        assert_eq!(file_name(&sheet), "成本计算单_CS-202610-0001.xlsx");
    }

    // ---------- 路由与权限注解成对（设计 §5.2 #32） ----------

    #[test]
    fn route_and_permission_are_paired() {
        let source = include_str!("cost_period_sheet.rs");
        let path = "/business/erp/cost-period/{id}/cost-sheet";
        assert!(source.contains(&format!("#[nik::get(\"{path}\")]")), "路由注解缺失");
        assert!(
            !source.contains(&format!("#[nik::get(\"/api{path}\")]")),
            "路由宏路径不含 /api 前缀（由 [router].prefix 注入）"
        );
        let permission = format!("path = \"{path}\",\n    method = \"GET\",\n    permission = \"{PERM_LIST}\"");
        assert!(
            source.contains(&permission),
            "导出必须挂列表权限码（设计 §5.2 #32）：能看计算单的人才能导出"
        );
    }

    // ---------- 大对象：1000 行不超时 ----------

    #[test]
    fn thousand_rows_sheet_builds_fast_and_complete() {
        let items: Vec<CostSheetItemRes> = (0..1000)
            .map(|index| {
                let mut row = item(
                    &format!("CB-202610-{index:04}"),
                    "19000.0000",
                    "4000.0000",
                    "4000.0000",
                    "22311.1104",
                    "4688.8896",
                );
                row.cost_object_id = format!("00000000-0000-0000-0000-{index:012}");
                row
            })
            .collect();
        let sheet = detail(items);
        let started = std::time::Instant::now();
        let bytes = build_workbook(&sheet, &[]).expect("应能生成 xlsx");
        let elapsed = started.elapsed();

        // 1000 行的纯内存拼装 + 写出：卡面门槛是「不超时」，本机实测远低于 1s，
        // 阈值放到 30s 只为兜住「退化成逐行查库 / O(n²)」这类结构性回归。
        assert!(elapsed.as_secs() < 30, "1000 行导出耗时 {elapsed:?}，疑似退化");
        assert!(bytes.starts_with(b"PK"), "xlsx 是 zip 容器");
        assert!(bytes.len() > 10_000, "1000 行文件不该只有 {} 字节", bytes.len());

        // 行数：1 表头信息 + 1000 + 1 合计（漏行 = 静默丢数据）
        let rows = main_rows(&sheet, &HashMap::new());
        assert_eq!(rows.len(), 1002);
        assert_eq!(
            rows.last().and_then(|row| row.get(COL_OBJECT_NO)).map(String::as_str),
            Some("合计（1000 个对象）")
        );
    }

    #[test]
    fn workbook_has_second_sheet_only_when_alloc_present() {
        let sheet = detail(vec![item(
            "CB-202610-0001",
            "19000.0000",
            "4000.0000",
            "4000.0000",
            "22311.1104",
            "4688.8896",
        )]);
        let without = build_workbook(&sheet, &[]).expect("无分摊也要能导出");
        let with = build_workbook(&sheet, &[alloc("CB-202610-0001", "3000")]).expect("有分摊");

        let count = |bytes: &[u8]| -> usize {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("xlsx 是 zip");
            (0..archive.len())
                .filter(|index| {
                    archive
                        .by_index(*index)
                        .map(|file| file.name().starts_with("xl/worksheets/sheet"))
                        .unwrap_or(false)
                })
                .count()
        };
        assert_eq!(count(&without), 1, "没有分摊时不写空 sheet");
        assert_eq!(count(&with), 2, "有分摊时追加「分摊明细」");
    }
}
