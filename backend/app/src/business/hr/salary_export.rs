//! 工资表导出 xlsx（设计 §5.2 #45 · HR-42）。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 45 | GET | `/business/hr/salary/{id}/export` | `business:hr:salary:export` | HR-42 |
//!
//! 取数在 [`crate::business::hr::salary_sheet_model::export_details`] /
//! [`crate::business::hr::salary_sheet_model::export_items`]（整单两条 SQL，无 N+1），
//! 本文件的职责只有三件：**列的形状**、**部门分组与小计**、**xlsx 的落地**。
//!
//! # 三个验收项各自落在哪
//!
//! | 验收 | 落点 | 证据 |
//! |---|---|---|
//! | **列 = 薪资项** | [`item_columns`]：启用项（`is_active = 1` 且非 `SUMMARY`）按 `sort_order` 各占一列，表头取**当月快照名**；后接 5 个汇总列 | 单测 `item_columns_follow_enabled_order_and_wrap_sheet_only_items` / `subtotal_and_total_rows_are_last`；真库用例读回表头逐字对拍 |
//! | **按部门分组小计** | [`build_table`]：一行一人 → 每个部门后插一行「小计」→ 末尾一行「合计」 | 单测 `subtotal_row_follows_its_department` / `grand_total_equals_the_sum_of_every_employee`；真库用例断言「小计 = 本部门各行之和」「合计 = 单头 5 个金额列」 |
//! | **大表不超时** | 两次 SQL（明细一次、明细项一次）+ 一次 `write_sheet`，全程无逐行查询 | 真库用例 `export_500_heads_is_fast_and_complete`：500 人 × 15 项端到端耗时与行数一起断言 |
//!
//! # 列为什么是「启用项 + 当月多出来的项」
//!
//! 设计 §5.3 定的是「全部启用薪资项（`item_code` 为列，`item_name` 为表头）+ 5 个汇总列」，
//! 月份之间列位置**必须**稳定 —— HR 是做月环比、做 `VLOOKUP` 的，列一漂就全错。
//! 所以前缀永远是「当前启用项按 `sort_order`」。
//!
//! 但只管启用项会**丢数据**：某个项在 10 月发过钱，11 月被停用，10 月那张单导出时
//! 那一列就消失了 —— 工资表是审计凭证（AGENTS.md 约定 13），不能因为「现在不用了」就
//! 从历史表上抹掉。所以当月单上出现、而启用列表里没有的项，**按当月行号追加到末尾**。
//! 表头取当月快照名（`biz_salary_detail_item.item_name`），而不是配置页现在的名字。
//!
//! 反过来，`SUMMARY` 项（`GROSS` / `NET`）**不当列**：它们本来就不落 `detail_item`
//! （引擎只落明细项），当列只会得到两列永远空白的单元格，而这两个数字由末 5 列的
//! 「应发合计 / 实发合计」承载 —— 同一笔钱在表里出现两次、其中一次必然是空的，
//! 是最容易被 HR 当成「系统算错了」的形态。
//!
//! # 数值口径
//!
//! 一律走 [`num_text`]：两位小数、去尾零。`num_text` 的输出会被
//! `nik_utils::export` 的 `classify` 认成数字（`"9889.66"` → 数值单元格），所以 HR
//! 拿到的是**能直接 SUM 的真数字**，不是文本；同时同值必然同形（`22` 不会写成 `22.00`），
//! 文本比对也对得上。单测 `num_text_matches_the_attendance_exporter` 钉住这一点
//! （考勤导出用同一套口径，两张表放一起核不能出现两种写法）。
//!
//! # 上限与截断
//!
//! 单次导出上限 [`EXPORT_MAX_ROWS`] 人（一期 5000 人已是「集团级月度工资表」的规模）。
//! 超限时**不静默截断**：响应头带 `x-export-truncated: true` 与 `x-export-total`，
//! 让人能看出「这份表不全」。设计 §5.3 的异步导出任务（`sys_export_task` + 下载中心）
//! 留给数据量真的到那一步时再做 —— 本卡只做同步下载（与考勤导出同一取向）。
//!
//! # 截断与「合计」行的一处克制
//!
//! 被截断时，末尾的「合计」行是**已导出这些人**的合计，而不是全单合计（否则 Excel 里
//! 「合计 ≠ 上面各行之和」，那是最坏的一种假）。所以截断时把「合计」行的姓名列写成
//! 「合计（前 N 人）」，并在响应头里点名 `x-export-truncated`。
//!
//! # 数据范围
//!
//! `biz_salary_sheet` 按设计 §4.7 **不登记**数据权限（单头是全公司合计，`SELF` 语义不成立），
//! 可见性只由 `business:hr:salary:export` 权限码控制 —— 与 HR-36 的列表 / 详情同一口径。
//! 明细表 `biz_salary_detail` 的范围登记（设计 D-12）落在 `/salary/my` 的卡上，本卡不改
//! `shared/scope.rs`（跨模块改动要报，不自己动）。

use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};

use axum::extract::Path;
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::salary_item_dto::ITEM_TYPE_SUMMARY;
use crate::business::hr::salary_item_model;
use crate::business::hr::salary_sheet_dto::{self as dto, SalaryTotalsRes};
use crate::business::hr::salary_sheet_model::{self as model, SalaryExportDetailRow, SalaryExportItemRow};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 导出工资表（与 `V0387` 的 `sys_menu.auth_code` 逐字一致）。
pub const PERM_EXPORT: &str = "business:hr:salary:export";

/// 路由路径（不含 `/api` 前缀；与 [`crate::business::hr::handler::ROUTE_CONTRACT`] 同源）。
pub const EXPORT_PATH: &str = "/business/hr/salary/{id}/export";

/// 单次导出上限（人）。
pub const EXPORT_MAX_ROWS: u64 = 5000;

/// 前 3 列：员工身份（工号 / 姓名是快照列，部门名是本月工资表的快照 —— AGENTS.md 约定 13）。
pub const EMPLOYEE_HEADERS: [&str; 3] = ["工号", "姓名", "部门"];

/// 末 5 列：汇总（顺序即列顺序）。
///
/// 与单头 7 个金额列的关系：`应发合计 = gross`、`代扣合计 = withhold`、
/// `实发合计 = net`、`单位社保合计 = si_company`、`单位公积金合计 = hf_company`。
/// `earning_total` / `pre_deduct_total` 不出列 —— 它们是应发的组成，已经在项列里逐项可见。
///
/// 两个「单位…」列带「合计」后缀是**刻意的**：种子里 `SI_COMPANY` 的项名就叫「单位社保」、
/// `HF_COMPANY` 就叫「单位公积金」（`V0396`），表头若也用同名字，每个客户的第一张工资表
/// 都会出现「单位社保」和「单位社保（SI_COMPANY）」两列孪生列 —— 加个「合计」后缀
/// 既说清了这一列是合计，也避开了与项名的正面冲突（去重逻辑仍在，防客户自定义项撞名）。
pub const SUMMARY_HEADERS: [&str; 5] = ["应发合计", "代扣合计", "实发合计", "单位社保合计", "单位公积金合计"];

/// 部门小计行的姓名列取值。
pub const SUBTOTAL_LABEL: &str = "小计";

/// 全单合计行的姓名列取值。
pub const GRAND_TOTAL_LABEL: &str = "合计";

/// 没有部门的人归到这一组（与明细页 / 部门汇总接口同一文案）。
pub const EMPTY_DEPT_LABEL: &str = "(未分配部门)";

// ==================== 纯函数（不连库） ====================

/// 数值列的字面值：最多两位小数、去掉尾随零（`22.00 → 22`、`7.50 → 7.5`）。
///
/// 与 [`crate::business::hr::attendance_export::num_text`] 同一口径：两张导出表
/// 放在一起核对时，同值不同形会被 HR 当成两拨数据（`VLOOKUP` 按文本匹配）。
pub fn num_text(value: Decimal) -> String {
    value.round_dp(2).normalize().to_string()
}

/// 一个可用作列的薪资项（已过滤：启用 且 非 `SUMMARY`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnabledItem {
    /// 项编码（列标识）。
    pub item_code: String,
    /// 项名称（配置页现在的名字；当月单上有快照名时以快照为准）。
    pub item_name: String,
}

/// 一列薪资项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemColumn {
    /// 项编码（取值键）。
    pub item_code: String,
    /// 表头（去重后；正常情况下等于项名称）。
    pub header: String,
}

/// 一行的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// 一名员工。
    Employee,
    /// 一个部门的小计（紧跟在本部门员工行之后）。
    DeptSubtotal,
    /// 全单合计（最后一行）。
    GrandTotal,
}

/// 一行导出数据。
#[derive(Debug, Clone)]
pub struct ExportLine {
    /// 行角色。
    pub kind: LineKind,
    /// 工号列（小计 / 合计行为空）。
    pub employee_no: String,
    /// 姓名列（小计行「小计」、合计行「合计」）。
    pub employee_name: String,
    /// 部门列（部门名快照）。
    pub dept_name: String,
    /// 数值列（顺序 = `item_columns` 然后 `SUMMARY_HEADERS`；缺项为空串）。
    pub numbers: Vec<String>,
}

/// 一张待落盘的工资表。
#[derive(Debug)]
pub struct ExportTable {
    /// 表头（3 员工列 + N 项列 + 5 汇总列，顺序即 xlsx 列顺序）。
    pub headers: Vec<String>,
    /// 项列（表头的中间段）。
    pub item_columns: Vec<ItemColumn>,
    /// 数据行（员工行 / 小计行 / 合计行，按部门分组）。
    pub lines: Vec<ExportLine>,
    /// 员工行数（= 表里的人数）。
    pub employee_count: usize,
    /// 部门数（= 小计行数）。
    pub dept_count: usize,
    /// 是否被 [`EXPORT_MAX_ROWS`] 截断。
    pub truncated: bool,
}

impl ExportTable {
    /// 一行 → `HashMap`（键必须与 [`ExportTable::headers`] 逐字一致）。
    pub fn row_map(&self, line: &ExportLine) -> HashMap<String, String> {
        let mut map: HashMap<String, String> = HashMap::with_capacity(self.headers.len());
        map.insert(EMPLOYEE_HEADERS[0].to_string(), line.employee_no.clone());
        map.insert(EMPLOYEE_HEADERS[1].to_string(), line.employee_name.clone());
        map.insert(EMPLOYEE_HEADERS[2].to_string(), line.dept_name.clone());
        for (index, column) in self.item_columns.iter().enumerate() {
            map.insert(
                column.header.clone(),
                line.numbers.get(index).cloned().unwrap_or_default(),
            );
        }
        let base = self.item_columns.len();
        for (offset, header) in SUMMARY_HEADERS.iter().enumerate() {
            map.insert(
                (*header).to_string(),
                line.numbers.get(base + offset).cloned().unwrap_or_default(),
            );
        }
        map
    }

    /// 整张表 → xlsx 字节（写不出 → 500，报文说清是「生成 Excel 失败」而不是业务错）。
    pub fn to_workbook(&self) -> std::result::Result<Vec<u8>, ApiError> {
        let header_refs: Vec<&str> = self.headers.iter().map(String::as_str).collect();
        let rows: Vec<HashMap<String, String>> = self.lines.iter().map(|line| self.row_map(line)).collect();
        crate::business::impexp::engine::write_sheet(&header_refs, &rows)
    }
}

/// 部门分组键：空 / 空白 / `None` 一律归到 [`EMPTY_DEPT_LABEL`]。
fn dept_key(dept_name: Option<&str>) -> String {
    dept_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| EMPTY_DEPT_LABEL.to_string())
}

/// 表头去重：重复会让 xlsx 只剩一列、另一个值被吞（`HashMap` 键冲突）。
///
/// 三类冲突都要防：同类项重名（两个项都叫「补贴」）、项名撞员工列（「部门」）、
/// 项名撞汇总列（「实发合计」）。撞了就补 `（项编码）`，仍撞再加序号。
fn dedupe_headers(columns: &mut [ItemColumn]) {
    let mut used: HashSet<String> = EMPLOYEE_HEADERS
        .iter()
        .chain(SUMMARY_HEADERS.iter())
        .map(|header| (*header).to_string())
        .collect();
    for column in columns.iter_mut() {
        let trimmed = column.header.trim();
        let base = if trimmed.is_empty() {
            column.item_code.clone()
        } else {
            trimmed.to_string()
        };
        let mut candidate = base.clone();
        if used.contains(&candidate) {
            candidate = format!("{base}（{}）", column.item_code);
            let mut serial = 2;
            while used.contains(&candidate) {
                candidate = format!("{base}（{}-{serial}）", column.item_code);
                serial += 1;
            }
        }
        used.insert(candidate.clone());
        column.header = candidate;
    }
}

/// 决定「列 = 哪些薪资项」与列顺序。
///
/// 1. 先是启用项（`enabled` 已按 `sort_order` 排好，本函数不再排序）；
/// 2. 当月单上出现、而启用列表里没有的项：按当月最小行号追加到末尾；
/// 3. 全表头去重。
///
/// 表头优先取**当月快照名**（`biz_salary_detail_item.item_name`）：工资表是审计凭证，
/// 项在 10 月叫什么，10 月那张表上就该叫什么。
pub fn item_columns(enabled: &[EnabledItem], items: &[SalaryExportItemRow]) -> Vec<ItemColumn> {
    // 当月单上出现过的项：code → (快照名, 最小行号)
    let mut on_sheet: BTreeMap<&str, (&str, i32)> = BTreeMap::new();
    for item in items {
        on_sheet
            .entry(item.item_code.as_str())
            .and_modify(|(_, line)| *line = (*line).min(item.line_no))
            .or_insert((item.item_name.as_str(), item.line_no));
    }

    let enabled_codes: HashSet<&str> = enabled.iter().map(|item| item.item_code.as_str()).collect();
    let mut columns: Vec<ItemColumn> = Vec::with_capacity(enabled.len() + on_sheet.len());
    for item in enabled {
        let header = on_sheet
            .get(item.item_code.as_str())
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_else(|| item.item_name.clone());
        columns.push(ItemColumn {
            item_code: item.item_code.clone(),
            header,
        });
    }

    let mut extras: Vec<(&str, &str, i32)> = on_sheet
        .iter()
        .filter(|(code, _)| !enabled_codes.contains(*code))
        .map(|(code, (name, line))| (*code, *name, *line))
        .collect();
    extras.sort_by_key(|(code, _, line)| (*line, *code));
    for (code, name, _) in extras {
        columns.push(ItemColumn {
            item_code: code.to_string(),
            header: name.to_string(),
        });
    }

    dedupe_headers(&mut columns);
    columns
}

/// 一个累加器：某一段（一个部门 / 全单）的金额合计。
///
/// `item_seen` 记「这一项在该段里至少出现过一次」：全员都没有这一项时小计留空，
/// 而不是写一个 `0` —— 写 `0` 等于断言「算过，是 0」，而事实可能是「这一项这个月没参与计算」。
struct Tally {
    items: Vec<Decimal>,
    item_seen: Vec<bool>,
    summary: [Decimal; 5],
}

impl Tally {
    fn new(columns: usize) -> Self {
        Self {
            items: vec![Decimal::ZERO; columns],
            item_seen: vec![false; columns],
            summary: [Decimal::ZERO; 5],
        }
    }

    /// 把一行的数值累加进来（`numbers` 顺序 = 项列然后 5 汇总列）。
    fn add(&mut self, numbers: &[String]) {
        for (index, text) in numbers.iter().enumerate() {
            // 空串 = 这一项这一行没有值（不参与累加，也不标记 seen）
            if text.is_empty() {
                continue;
            }
            let Ok(value) = Decimal::from_str_exact(text) else {
                // 理论上不可能（值都出自 `num_text`）；真出现时宁可漏加一笔也别整表 500
                continue;
            };
            if index < self.items.len() {
                self.items[index] += value;
                self.item_seen[index] = true;
            } else if let Some(slot) = self.summary.get_mut(index - self.items.len()) {
                *slot += value;
            }
        }
    }

    /// 累加器 → 数值列（项列：没见过就空串；汇总列：永远给数字）。
    fn numbers(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.items.len() + 5);
        for (index, value) in self.items.iter().enumerate() {
            out.push(if self.item_seen[index] {
                num_text(*value)
            } else {
                String::new()
            });
        }
        out.extend(self.summary.iter().map(|value| num_text(*value)));
        out
    }
}

/// 一名员工的数值列（缺失的项留空：不写 `0`，见 [`Tally`] 的说明）。
fn employee_numbers(row: &SalaryExportDetailRow, amounts: Option<&Vec<Option<Decimal>>>, width: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(width + 5);
    for index in 0..width {
        let value = amounts.and_then(|amounts| amounts.get(index).copied().flatten());
        out.push(value.map(num_text).unwrap_or_default());
    }
    out.push(num_text(row.gross_amount));
    out.push(num_text(row.withhold_amount));
    out.push(num_text(row.net_amount));
    out.push(num_text(row.si_company));
    out.push(num_text(row.hf_company));
    out
}

/// 把「整单明细 + 明细项 + 启用项」拼成一张可落盘的表。
///
/// # 行的形状
///
/// ```text
/// 工号    姓名   部门     <项列…>     应发 代扣 实发 单位社保 单位公积金
/// A-001   张三   生产部   …            …
/// A-002   李四   生产部   …            …
///         小计   生产部   Σ            Σ    Σ    Σ    Σ        Σ
/// A-009   王五   技术部   …            …
///         小计   技术部   Σ            Σ    Σ    Σ    Σ        Σ
///         合计            Σ            Σ    Σ    Σ    Σ        Σ
/// ```
///
/// 分组顺序：部门名升序，[`EMPTY_DEPT_LABEL`] 永远最后（与部门汇总接口同一规则）。
/// 组内按工号升序（工号是 HR 对 Excel 的锚点）。
pub fn build_table(
    details: &[SalaryExportDetailRow],
    items: &[SalaryExportItemRow],
    enabled: &[EnabledItem],
    truncated: bool,
) -> ExportTable {
    let item_columns = item_columns(enabled, items);
    let width = item_columns.len();

    // 项编码 → 列下标
    let index_of: HashMap<&str, usize> = item_columns
        .iter()
        .enumerate()
        .map(|(index, column)| (column.item_code.as_str(), index))
        .collect();

    // 明细 id → 每列金额（一次遍历；`uk_biz_sal_ditem(detail_id, item_code)` 保证不重）
    let mut amounts: HashMap<Uuid, Vec<Option<Decimal>>> = HashMap::with_capacity(details.len());
    for item in items {
        let Some(&index) = index_of.get(item.item_code.as_str()) else {
            continue;
        };
        let slot = amounts.entry(item.detail_id).or_insert_with(|| vec![None; width]);
        if let Some(cell) = slot.get_mut(index) {
            *cell = Some(item.amount);
        }
    }

    // 部门分组（`BTreeMap` 只用来收拢，顺序在下面显式排 —— 空部门要放最后）
    let mut groups: BTreeMap<String, Vec<&SalaryExportDetailRow>> = BTreeMap::new();
    for row in details {
        groups.entry(dept_key(row.dept_name.as_deref())).or_default().push(row);
    }
    let mut ordered: Vec<(String, Vec<&SalaryExportDetailRow>)> = groups.into_iter().collect();
    ordered.sort_by(|left, right| {
        let left_key = (left.0 == EMPTY_DEPT_LABEL, left.0.as_str());
        let right_key = (right.0 == EMPTY_DEPT_LABEL, right.0.as_str());
        left_key.cmp(&right_key)
    });

    let mut lines: Vec<ExportLine> = Vec::with_capacity(details.len() + ordered.len() + 1);
    let mut grand = Tally::new(width);
    for (dept_name, mut rows) in ordered {
        // 组内工号升序（SQL 已排好，这里再排一次是为了让纯函数与 SQL 顺序无关 —— 单测能虐它）
        rows.sort_by(|left, right| left.employee_no.cmp(&right.employee_no).then(left.id.cmp(&right.id)));

        let mut subtotal = Tally::new(width);
        for row in &rows {
            let numbers = employee_numbers(row, amounts.get(&row.id), width);
            subtotal.add(&numbers);
            grand.add(&numbers);
            lines.push(ExportLine {
                kind: LineKind::Employee,
                employee_no: row.employee_no.clone(),
                employee_name: row.employee_name.clone(),
                dept_name: dept_name.clone(),
                numbers,
            });
        }
        lines.push(ExportLine {
            kind: LineKind::DeptSubtotal,
            employee_no: String::new(),
            employee_name: SUBTOTAL_LABEL.to_string(),
            dept_name: dept_name.clone(),
            numbers: subtotal.numbers(),
        });
    }

    // 只在真有人时才出「合计」行：零人单出一行 0 是噪音，且会被当成「全员工资是 0」
    if !details.is_empty() {
        let label = if truncated {
            format!("{GRAND_TOTAL_LABEL}（已导出 {EXPORT_MAX_ROWS} 人）")
        } else {
            GRAND_TOTAL_LABEL.to_string()
        };
        lines.push(ExportLine {
            kind: LineKind::GrandTotal,
            employee_no: String::new(),
            employee_name: label,
            dept_name: String::new(),
            numbers: grand.numbers(),
        });
    }

    let mut headers: Vec<String> = EMPLOYEE_HEADERS.iter().map(|header| (*header).to_string()).collect();
    headers.extend(item_columns.iter().map(|column| column.header.clone()));
    headers.extend(SUMMARY_HEADERS.iter().map(|header| (*header).to_string()));

    ExportTable {
        headers,
        item_columns,
        lines,
        employee_count: details.len(),
        dept_count: groups_len(&details_depts(details)),
        truncated,
    }
}

/// 空部门归一的去重列表（只为算「小计行数」，不值得单独跑一次 SQL）。
fn details_depts(details: &[SalaryExportDetailRow]) -> BTreeMap<String, usize> {
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    for row in details {
        *map.entry(dept_key(row.dept_name.as_deref())).or_insert(0) += 1;
    }
    map
}

fn groups_len(map: &BTreeMap<String, usize>) -> usize {
    map.len()
}

/// 下载文件名（中文名，`xlsx_response` 会做 RFC 5987 编码）。
///
/// 带期间码与单号：HR 一次下载一堆表时靠文件名区分（`工资表_2026-10_SAL-202610-000001.xlsx`）。
pub fn file_name(period_code: &str, sheet_no: &str) -> String {
    format!("工资表_{period_code}_{sheet_no}.xlsx")
}

/// `404 + 10002`：薪资单不存在。
fn not_found(sheet_id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("薪资单 {sheet_id} 不存在（可能已删除，或 id 抄错）"),
    )
}

/// 当前启用、且能占一列的薪资项（`is_active = 1` 且非 `SUMMARY`，按 `sort_order`）。
///
/// `list_all` 已按 `sort_order, item_code` 排好：配置页上看到的顺序就是表里的列顺序。
async fn enabled_items() -> std::result::Result<Vec<EnabledItem>, ApiError> {
    let items = salary_item_model::list_all(db())
        .await
        .map_err(|error| utils::db_error(error, "查询启用的薪资项"))?;
    Ok(items
        .into_iter()
        .filter(|item| item.is_active == 1 && item.item_type != ITEM_TYPE_SUMMARY)
        .map(|item| EnabledItem {
            item_code: item.item_code,
            item_name: item.item_name,
        })
        .collect())
}

/// 一次导出的全部产物（表 + 响应头要的证据），真库用例直接调这一层。
#[derive(Debug)]
pub struct ExportBundle {
    /// 拼好的表（行 / 列 / 小计 / 合计）。
    pub table: ExportTable,
    /// 单号。
    pub sheet_no: String,
    /// 薪资期间。
    pub period_code: String,
    /// 单据状态。
    pub status: String,
    /// 库里该单的明细行数（判截断用）。
    pub total_rows: i64,
    /// 单头 7 个金额列 vs `SUM(明细)` 是否一致。
    ///
    /// `false` 时**照样导出**（这是诊断信息，不是错误：界面要能拿到文件去核对），
    /// 但响应头会点名，避免 HR 拿着「和系统数字不一样」的表去发工资。
    pub consistent: bool,
}

impl ExportBundle {
    /// 整张表 → xlsx 字节。
    pub fn to_workbook(&self) -> std::result::Result<Vec<u8>, ApiError> {
        self.table.to_workbook()
    }
}

// ==================== §5.2 #45 导出 ====================

/// 导出工资表（设计 §5.2 #45）。
///
/// # 参数
///
/// - `id`（**必填**，路径参数）：导哪一张薪资单。
///
/// # 返回值
///
/// xlsx 字节流。响应头带便于核对 / 排障的证据：
/// `x-salary-sheet`（单号）、`x-salary-period`（期间）、`x-salary-status`（状态）、
/// `x-export-rows`（导出人数）、`x-export-depts`（部门数）、`x-export-total`（库里的明细行数）、
/// `x-salary-consistent`（单头与明细是否一致）、超上限时另有 `x-export-truncated`。
///
/// # 异常情况
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | `id` 不是 uuid | 400 | `10001` |
/// | 薪资单不存在或已软删 | 404 | `10002` |
/// | 数据库异常 | 500 | `10003` / 驱动码 |
/// | xlsx 生成失败 | 500 | `10003` |
#[nik::permission(
    path = "/business/hr/salary/{id}/export",
    method = "GET",
    permission = "business:hr:salary:export"
)]
#[nik::get("/business/hr/salary/{id}/export")]
pub async fn export(Path(id): Path<String>) -> Response {
    match export_response(&id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 导出实现（把读库 / 拼表 / 造响应从 handler 里拆出来，真库用例直接调 [`export_bundle`]）。
pub async fn export_response(id: &str) -> std::result::Result<Response, ApiError> {
    let sheet_id = dto::parse_uuid(id, "id")?;
    let bundle = export_bundle(sheet_id).await?;
    let bytes = bundle.to_workbook()?;

    let mut response =
        crate::business::impexp::handler::xlsx_response(bytes, &file_name(&bundle.period_code, &bundle.sheet_no));
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-salary-sheet", bundle.sheet_no.clone()),
        ("x-salary-period", bundle.period_code.clone()),
        ("x-salary-status", bundle.status.clone()),
        ("x-export-rows", bundle.table.employee_count.to_string()),
        ("x-export-depts", bundle.table.dept_count.to_string()),
        ("x-export-total", bundle.total_rows.to_string()),
    ] {
        if let Ok(header_value) = HeaderValue::from_str(&value) {
            headers.insert(name, header_value);
        }
    }
    headers.insert(
        "x-salary-consistent",
        HeaderValue::from_static(if bundle.consistent { "true" } else { "false" }),
    );
    if bundle.table.truncated {
        headers.insert("x-export-truncated", HeaderValue::from_static("true"));
    }
    Ok(response)
}

/// 导出取数 + 拼表（不涉及 HTTP）。
///
/// 顺序是刻意的：**先**读明细合计（人数 → 判截断，并对账单头），**再**读明细与明细项。
/// 两次读之间有人改了明细时，表里的行数与 `total_rows` 会对不上 —— 那正是
/// `x-export-truncated` / `x-salary-consistent` 存在的意义：让「这份表可能不全 / 可能和单头
/// 不一致」能被看见，而不是把一次竞态变成一份「看起来很完整」的假表。
pub async fn export_bundle(sheet_id: Uuid) -> std::result::Result<ExportBundle, ApiError> {
    let sheet = model::find_sheet(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "查询待导出的薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;

    let detail_totals = model::totals_of_details(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总待导出的薪资明细"))?;
    let total_rows = i64::from(detail_totals.employee_count);
    let truncated = u64::try_from(total_rows).unwrap_or(u64::MAX) > EXPORT_MAX_ROWS;

    let details = model::export_details(sheet_id, EXPORT_MAX_ROWS)
        .await
        .map_err(|error| utils::db_error(error, "导出薪资明细"))?;
    let items = model::export_items(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "导出薪资明细项"))?;
    let enabled = enabled_items().await?;

    let table = build_table(&details, &items, &enabled, truncated);
    let consistent = dto::diff_totals(&sheet.totals(), &SalaryTotalsRes::of_details(&detail_totals)).is_empty();

    Ok(ExportBundle {
        table,
        sheet_no: sheet.sheet_no,
        period_code: sheet.period_code,
        status: sheet.status,
        total_rows,
        consistent,
    })
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::handler::ROUTE_CONTRACT;
    use nik::prelude::rust_decimal::Decimal;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法小数")
    }

    fn detail(id: Uuid, no: &str, name: &str, dept: Option<&str>, gross: &str, net: &str) -> SalaryExportDetailRow {
        let gross = dec(gross);
        let net = dec(net);
        // 库层恒等式：应发 = 实发 + 代扣；代扣 = 个人社保 + 公积金 + 个税 + 其他
        let withhold = gross - net;
        SalaryExportDetailRow {
            id,
            employee_no: no.to_string(),
            employee_name: name.to_string(),
            dept_name: dept.map(str::to_string),
            gross_amount: gross,
            withhold_amount: withhold,
            net_amount: net,
            si_company: dec("1680"),
            hf_company: dec("960"),
        }
    }

    fn item(detail_id: Uuid, code: &str, name: &str, amount: &str, line_no: i32) -> SalaryExportItemRow {
        SalaryExportItemRow {
            detail_id,
            item_code: code.to_string(),
            item_name: name.to_string(),
            amount: dec(amount),
            line_no,
        }
    }

    fn enabled(pairs: &[(&str, &str)]) -> Vec<EnabledItem> {
        pairs
            .iter()
            .map(|(code, name)| EnabledItem {
                item_code: (*code).to_string(),
                item_name: (*name).to_string(),
            })
            .collect()
    }

    fn sample_enabled() -> Vec<EnabledItem> {
        enabled(&[
            ("BASE", "基本工资"),
            ("PERF", "绩效奖金"),
            ("SI_PERSONAL", "社保(个人)"),
        ])
    }

    /// 两名员工、同部门；张三有三项，李四只有基本工资。
    fn two_people() -> (Vec<SalaryExportDetailRow>, Vec<SalaryExportItemRow>, Vec<EnabledItem>) {
        let zhang = Uuid::now_v7();
        let li = Uuid::now_v7();
        let details = vec![
            detail(zhang, "A-002", "张三", Some("生产部"), "9000", "8000"),
            detail(li, "A-001", "李四", Some("生产部"), "5000", "4600"),
        ];
        let items = vec![
            item(zhang, "BASE", "基本工资", "8000", 10),
            item(zhang, "PERF", "绩效奖金", "1000", 40),
            item(zhang, "SI_PERSONAL", "社保(个人)", "-400", 100),
            item(li, "BASE", "基本工资", "5000", 10),
            item(li, "SI_PERSONAL", "社保(个人)", "-400", 100),
        ];
        let enabled = sample_enabled();
        (details, items, enabled)
    }

    // ---- 列 ----

    /// 列 = 启用项（按传入顺序）+ 当月多出来的项（按当月行号）+ 5 个汇总列。
    #[test]
    fn item_columns_follow_enabled_order_and_wrap_sheet_only_items() {
        let (_, mut items, enabled) = two_people();
        // 当月还发过一个后来停用的老项（行号 30，插在中间）
        items.push(item(items[0].detail_id, "OLD_BONUS", "季度奖(2025停用)", "300", 30));
        let columns = item_columns(&enabled, &items);

        let codes: Vec<&str> = columns.iter().map(|column| column.item_code.as_str()).collect();
        assert_eq!(
            codes,
            vec!["BASE", "PERF", "SI_PERSONAL", "OLD_BONUS"],
            "启用项在前，历史项追加在后"
        );
        let headers: Vec<&str> = columns.iter().map(|column| column.header.as_str()).collect();
        assert_eq!(
            headers,
            vec!["基本工资", "绩效奖金", "社保(个人)", "季度奖(2025停用)"],
            "表头取当月快照名"
        );
    }

    /// 当月快照名优先于配置页现在的名字（工资表是审计凭证：10 月叫什么就显示什么）。
    #[test]
    fn sheet_snapshot_name_wins_over_current_item_name() {
        let sheet_id = Uuid::now_v7();
        let items = vec![item(sheet_id, "BASE", "基本工资(旧名)", "8000", 10)];
        let enabled = enabled(&[("BASE", "基本工资")]);
        let columns = item_columns(&enabled, &items);
        assert_eq!(columns[0].header, "基本工资(旧名)");
    }

    /// 当月单上什么都没有时，列仍然按启用项铺开（HR 需要一张有列的空表去核对）。
    #[test]
    fn columns_survive_an_empty_sheet() {
        let columns = item_columns(&sample_enabled(), &[]);
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].header, "基本工资");
    }

    /// 表头必须唯一，且不能撞员工列 / 汇总列（撞了 xlsx 只留一列，另一个值被吞）。
    #[test]
    fn headers_never_collide() {
        let sheet_id = Uuid::now_v7();
        let items = vec![
            item(sheet_id, "D1", "补贴", "1", 10),
            item(sheet_id, "D2", "补贴", "2", 20),
            item(sheet_id, "D3", "部门", "3", 30),
            item(sheet_id, "D4", "实发合计", "4", 40),
        ];
        let columns = item_columns(&[], &items);
        let headers: Vec<&str> = columns.iter().map(|column| column.header.as_str()).collect();
        assert_eq!(headers.len(), 4);
        let mut sorted = headers.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "表头重复：{headers:?}");
        // 第一个「补贴」原样保留，后来者补项编码
        assert_eq!(headers[0], "补贴");
        assert!(headers.contains(&"补贴（D2）"), "{headers:?}");
        assert!(headers.contains(&"部门（D3）"), "{headers:?}");
        assert!(headers.contains(&"实发合计（D4）"), "{headers:?}");

        // 整张表的表头也不能有重复
        let (details, items, enabled) = two_people();
        let table = build_table(&details, &items, &enabled, false);
        let mut table_headers = table.headers.clone();
        table_headers.sort();
        table_headers.dedup();
        assert_eq!(
            table_headers.len(),
            table.headers.len(),
            "整表表头重复：{:?}",
            table.headers
        );
    }

    /// 表头形状：3 员工列 + N 项列 + 5 汇总列（顺序固定）。
    #[test]
    fn header_shape_is_employee_items_summary() {
        let (details, items, enabled) = two_people();
        let table = build_table(&details, &items, &enabled, false);
        assert_eq!(table.headers.len(), 3 + 3 + 5);
        assert_eq!(&table.headers[..3], &EMPLOYEE_HEADERS);
        assert_eq!(
            &table.headers[3..6],
            &["基本工资".to_string(), "绩效奖金".to_string(), "社保(个人)".to_string()]
        );
        assert_eq!(&table.headers[6..], &SUMMARY_HEADERS.map(str::to_string));
    }

    // ---- 分组与小计 ----

    /// 两个部门：行序 = 部门（升序）→ 该部门员工（工号升序）→ 小计 → … → 合计。
    ///
    /// 部门名的「升序」是 **UTF-8 字节序**（`技术部` 在 `生产部` 之前）：导出不依赖库的
    /// collation（同一份数据换个库实例排序就变，会让两张月的表行序不同），代价是不按拼音排。
    #[test]
    fn subtotal_row_follows_its_department() {
        let a = detail(Uuid::now_v7(), "B-002", "王五", Some("技术部"), "7000", "6000");
        let b = detail(Uuid::now_v7(), "A-001", "李四", Some("生产部"), "5000", "4600");
        let c = detail(Uuid::now_v7(), "A-002", "张三", Some("生产部"), "9000", "8000");
        let details = vec![a, b, c];
        let table = build_table(&details, &[], &sample_enabled(), false);

        let shape: Vec<(&str, &str, &str)> = table
            .lines
            .iter()
            .map(|line| {
                (
                    line.dept_name.as_str(),
                    line.employee_no.as_str(),
                    line.employee_name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("技术部", "B-002", "王五"),
                ("技术部", "", SUBTOTAL_LABEL),
                ("生产部", "A-001", "李四"),
                ("生产部", "A-002", "张三"),
                ("生产部", "", SUBTOTAL_LABEL),
                ("", "", GRAND_TOTAL_LABEL),
            ]
        );
        assert_eq!(table.dept_count, 2);
        assert_eq!(table.employee_count, 3);
    }

    /// 小计的每一项 = 本部门各行之和（汇总列也一样）。
    #[test]
    fn subtotal_row_sums_its_department_only() {
        let p1 = Uuid::now_v7();
        let p2 = Uuid::now_v7();
        let t1 = Uuid::now_v7();
        let details = vec![
            detail(p1, "A-001", "李四", Some("生产部"), "5000", "4600"),
            detail(p2, "A-002", "张三", Some("生产部"), "9000", "8000"),
            detail(t1, "B-001", "王五", Some("技术部"), "7000", "6000"),
        ];
        let items = vec![
            item(p1, "BASE", "基本工资", "5000", 10),
            item(p2, "BASE", "基本工资", "8000", 10),
            item(p2, "PERF", "绩效奖金", "1000", 40),
            item(t1, "BASE", "基本工资", "7000", 10),
        ];
        let table = build_table(&details, &items, &sample_enabled(), false);

        let base_index = 0;
        let perf_index = 1;
        let subtotal = table
            .lines
            .iter()
            .position(|line| line.kind == LineKind::DeptSubtotal && line.dept_name == "生产部")
            .map(|index| &table.lines[index])
            .expect("生产部小计行");
        assert_eq!(subtotal.numbers[base_index], "13000", "5000 + 8000");
        assert_eq!(subtotal.numbers[perf_index], "1000");
        assert_eq!(subtotal.numbers[3], "14000", "应发 5000 + 9000");
        assert_eq!(subtotal.numbers[4], "1400", "代扣 400 + 1000");
        assert_eq!(subtotal.numbers[5], "12600", "实发 4600 + 8000");

        let tech = table
            .lines
            .iter()
            .find(|line| line.kind == LineKind::DeptSubtotal && line.dept_name == "技术部")
            .expect("技术部小计行");
        // 技术部没人有 PERF：留空，而不是写 0
        assert_eq!(tech.numbers[perf_index], "", "没人有这一项时小计留空");
        assert_eq!(tech.numbers[base_index], "7000");
    }

    /// 合计行 = 所有员工之和 = 各部门小计之和（两条路径必须一致）。
    #[test]
    fn grand_total_equals_the_sum_of_every_employee() {
        let p1 = Uuid::now_v7();
        let p2 = Uuid::now_v7();
        let details = vec![
            detail(p1, "A-001", "李四", Some("生产部"), "5000", "4600"),
            detail(p2, "B-001", "王五", Some("技术部"), "7000", "6000"),
        ];
        let items = vec![
            item(p1, "BASE", "基本工资", "5000", 10),
            item(p2, "BASE", "基本工资", "7000", 10),
        ];
        let table = build_table(&details, &items, &sample_enabled(), false);

        let total = table.lines.last().expect("合计行");
        assert_eq!(total.kind, LineKind::GrandTotal);
        assert_eq!(total.employee_name, GRAND_TOTAL_LABEL);
        assert_eq!(total.employee_no, "");
        assert_eq!(total.numbers[0], "12000");
        assert_eq!(total.numbers[3], "12000", "应发 5000 + 7000");
        assert_eq!(total.numbers[5], "10600", "实发 4600 + 6000");
        assert_eq!(total.numbers[6], "3360", "单位社保 1680 × 2");
        assert_eq!(total.numbers[7], "1920", "单位公积金 960 × 2");

        // 各部门小计之和 = 合计（同一口径的两条路径）
        let subtotal_sum: Decimal = table
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::DeptSubtotal)
            .map(|line| dec(&line.numbers[3]))
            .sum();
        assert_eq!(num_text(subtotal_sum), total.numbers[3]);
    }

    /// 没部门的人归到「(未分配部门)」，且永远排在最后。
    #[test]
    fn empty_department_is_grouped_and_sorted_last() {
        let a = detail(Uuid::now_v7(), "A-001", "李四", Some("生产部"), "5000", "4600");
        let b = detail(Uuid::now_v7(), "A-002", "张三", None, "6000", "5000");
        let c = detail(Uuid::now_v7(), "A-003", "王五", Some("   "), "7000", "6000");
        let table = build_table(&[a, b, c], &[], &sample_enabled(), false);

        // 生产部 1 人 → 1 行 + 小计；(未分配部门) 2 人 → 2 行 + 小计；最后 1 行合计
        let depts: Vec<&str> = table
            .lines
            .iter()
            .filter(|line| line.kind != LineKind::GrandTotal)
            .map(|line| line.dept_name.as_str())
            .collect();
        assert_eq!(
            depts,
            vec!["生产部", "生产部", EMPTY_DEPT_LABEL, EMPTY_DEPT_LABEL, EMPTY_DEPT_LABEL,],
            "空白与 None 都归到 (未分配部门)，且该组排在最后"
        );
        assert_eq!(table.dept_count, 2, "空白与 None 是同一个部门");
        // 最后一行合计
        assert_eq!(table.lines.last().map(|line| line.kind), Some(LineKind::GrandTotal));
    }

    /// 零人单：只有表头，没有小计 / 合计行（一行 0 会被当成「全员工资是 0」）。
    #[test]
    fn empty_sheet_has_headers_only() {
        let table = build_table(&[], &[], &sample_enabled(), false);
        assert!(table.lines.is_empty());
        assert_eq!(table.employee_count, 0);
        assert_eq!(table.dept_count, 0);
        assert_eq!(table.headers.len(), 3 + 3 + 5);
    }

    /// 员工行：每一项落到自己的列，缺项留空；5 个汇总列永远有值。
    #[test]
    fn employee_line_fills_columns_and_blanks_missing_items() {
        let (details, items, enabled) = two_people();
        let table = build_table(&details, &items, &enabled, false);
        let li = table
            .lines
            .iter()
            .find(|line| line.employee_no == "A-001")
            .expect("李四那一行");
        assert_eq!(li.employee_name, "李四");
        assert_eq!(li.dept_name, "生产部");
        assert_eq!(li.numbers[0], "5000", "基本工资");
        assert_eq!(li.numbers[1], "", "李四没有绩效奖金 → 留空而不是 0");
        assert_eq!(li.numbers[2], "-400", "社保（负数原样保留）");
        assert_eq!(li.numbers[3], "5000", "应发");
        assert_eq!(li.numbers[4], "400", "代扣");
        assert_eq!(li.numbers[5], "4600", "实发");
    }

    /// 员工行按工号升序（工号是 HR 对 Excel 的锚点，乱序会让 VLOOKUP 找不到人）。
    #[test]
    fn employee_rows_are_sorted_by_employee_no() {
        let one = detail(Uuid::now_v7(), "A-010", "十号", Some("生产部"), "1", "1");
        let two = detail(Uuid::now_v7(), "A-002", "二号", Some("生产部"), "1", "1");
        let three = detail(Uuid::now_v7(), "A-005", "五号", Some("生产部"), "1", "1");
        let table = build_table(&[one, two, three], &[], &sample_enabled(), false);
        let nos: Vec<&str> = table
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Employee)
            .map(|line| line.employee_no.as_str())
            .collect();
        assert_eq!(nos, vec!["A-002", "A-005", "A-010"]);
    }

    /// 截断时「合计」行必须自曝其短：写成「合计（已导出 5000 人）」，
    /// 否则 Excel 里「合计 ≠ 上面各行之和」，是最坏的一种假。
    #[test]
    fn truncated_total_row_says_so() {
        let details = vec![detail(Uuid::now_v7(), "A-001", "李四", Some("生产部"), "5000", "4600")];
        let table = build_table(&details, &[], &sample_enabled(), true);
        let total = table.lines.last().expect("合计行");
        assert_eq!(
            total.employee_name,
            format!("{GRAND_TOTAL_LABEL}（已导出 {EXPORT_MAX_ROWS} 人）")
        );
        assert!(table.truncated);
    }

    // ---- 数值 / 落盘 ----

    /// 数值字面值：两位小数、去尾零；且与考勤导出同一口径（两张表放一起核不能两种写法）。
    #[test]
    fn num_text_matches_the_attendance_exporter() {
        for text in [
            "0", "0.00", "22.00", "19.50", "9889.666", "1.005", "1.015", "-400.00", "13000",
        ] {
            let value = dec(text);
            assert_eq!(
                num_text(value),
                crate::business::hr::attendance_export::num_text(value),
                "工资表与考勤表的数值口径必须一致：{text}"
            );
        }
        assert_eq!(num_text(dec("22.00")), "22");
        assert_eq!(num_text(dec("19.50")), "19.5");
        assert_eq!(num_text(Decimal::ZERO), "0");
        assert_eq!(num_text(dec("-400.00")), "-400");
    }

    /// 每一行都能摊成 `HashMap` 且键集合与表头逐字一致（漏一个键就是空单元格）。
    #[test]
    fn every_line_maps_onto_every_header() {
        let (details, items, enabled) = two_people();
        let table = build_table(&details, &items, &enabled, false);
        for line in &table.lines {
            let map = table.row_map(line);
            assert_eq!(map.len(), table.headers.len(), "行映射的键数 ≠ 表头数");
            for header in &table.headers {
                assert!(map.contains_key(header), "缺列 {header}");
            }
            assert_eq!(map[EMPLOYEE_HEADERS[0]], line.employee_no);
            assert_eq!(map[EMPLOYEE_HEADERS[2]], line.dept_name);
        }
    }

    /// 真的能写出 xlsx：zip 容器魔数、非空；零人单也要能导出（HR 要拿表头去核）。
    #[test]
    fn workbook_is_a_real_xlsx_zip() {
        let (details, items, enabled) = two_people();
        let table = build_table(&details, &items, &enabled, false);
        let bytes = table.to_workbook().expect("生成 xlsx");
        assert!(bytes.len() > 1024, "xlsx 太小了，可能是空容器：{} 字节", bytes.len());
        assert_eq!(&bytes[..4], b"PK\x03\x04", "xlsx 必须是 zip 容器");

        let empty = build_table(&[], &[], &sample_enabled(), false);
        let empty_bytes = empty.to_workbook().expect("空表也要能导出");
        assert_eq!(&empty_bytes[..4], b"PK\x03\x04");
    }

    /// 文件名带期间码与单号（HR 一次下载一堆表时靠名字区分）。
    #[test]
    fn file_name_carries_period_and_sheet_no() {
        assert_eq!(
            file_name("2026-10", "SAL-202610-000001"),
            "工资表_2026-10_SAL-202610-000001.xlsx"
        );
    }

    /// 权限码 / 路径 / 卡号必须与 `ROUTE_CONTRACT` 的那一行逐字一致。
    ///
    /// 码写错的后果不是编译错误，而是线上「超管之外的账号全 403」，且看起来像「权限没配」。
    #[test]
    fn contract_row_matches_this_card() {
        let spec = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.path == EXPORT_PATH)
            .expect("导出路由必须已在 ROUTE_CONTRACT 里登记");
        assert_eq!(spec.method, "GET");
        assert_eq!(spec.permission, Some(PERM_EXPORT));
        assert_eq!(spec.card, "HR-42");
    }

    /// 汇总列的键与顺序：末 5 列必须是应发 / 代扣 / 实发 / 单位社保 / 单位公积金。
    #[test]
    fn summary_headers_are_the_last_five_columns() {
        assert_eq!(SUMMARY_HEADERS.len(), 5);
        assert_eq!(
            SUMMARY_HEADERS,
            ["应发合计", "代扣合计", "实发合计", "单位社保合计", "单位公积金合计"]
        );
        // 与员工列不重名、彼此不重名
        let mut all: Vec<&str> = EMPLOYEE_HEADERS.iter().chain(SUMMARY_HEADERS.iter()).copied().collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "员工列与汇总列有重名");
    }

    /// 汇总列取的是单头那 5 个金额（口径唯一的定义处就在这条断言里）：
    /// 应发 = `gross_amount`、代扣 = `withhold_amount`、实发 = `net_amount`。
    #[test]
    fn summary_columns_read_the_five_detail_amounts() {
        let row = detail(Uuid::now_v7(), "A-001", "李四", Some("生产部"), "5000", "4600");
        let numbers = employee_numbers(&row, None, 0);
        assert_eq!(
            numbers,
            vec![
                "5000".to_string(),
                "400".to_string(),
                "4600".to_string(),
                "1680".to_string(),
                "960".to_string()
            ]
        );
        // 应发 = 实发 + 代扣（库层恒等式，导出不能自己另立一套）
        assert_eq!(row.gross_amount, row.net_amount + row.withhold_amount);
    }
}
