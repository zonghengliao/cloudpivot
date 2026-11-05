//! 报表项维护面（HTTP）：`biz_report_item` 的列表 / 新增 / 编辑 / 排序 / 启停。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report-item/list` | `business:erp:report-item:list` | 某张报表的全部报表项（含停用行）+ 引用关系 |
//! | GET | `/business/erp/report-item/catalog` | 同上 | 字典：报表 / 所属方 / 层级 / 取数模式示例 |
//! | POST | `/business/erp/report-item` | `business:erp:report-item:manage` | 新增报表项 |
//! | PUT | `/business/erp/report-item/{id}` | 同上 | 编辑报表项（编码不可变） |
//! | PUT | `/business/erp/report-item/{id}/toggle` | 同上 | 启停 |
//! | PUT | `/business/erp/report-item/sort` | 同上 | 整表重排（传全量 id 顺序） |
//! | POST | `/business/erp/report-item/preview` | `business:erp:report-item:list` | 取数预览：这个模式会命中哪些科目 |
//!
//! # 为什么校验全在这里、而不是写进 model
//!
//! 报表项是否合法是**整表性质**，不是单行性质：
//!
//! ① `formula` 只能引用**排在自己前面**的项（求值顺序 = `sort_order`，见
//!    [`super::report::compute`] 第 2 遍），所以改一行的排序可能让另一行的公式变成死引用；
//! ② 公式引用了**已停用**的项时，报表会 500（`report` 只加载 `is_active = 1`）；
//! ③ 取数模式写错（例如 `abc`）不报错，只是永远取 0 —— 用户看不出「没发生」还是「配错了」。
//!
//! 所以每次写库前都把「整张报表 + 本次改动」拼成候选集跑一遍 [`validate_report`]，
//! 任何一条不通过就 400 并把问题原文回给用户；只报警不拦截的（模式命中 0 个科目、
//! 利润表用了非 EQUITY 的所属方）走 `warnings`，保存照做但页面上会提示。
//!
//! # 为什么改完就生效
//!
//! 报表接口每次编制都现查 `biz_report_item`（[`super::report::load_items_of`]），
//! 没有任何缓存层；`biz_report_item` 也不参与任何快照表。改完刷新报表即可看到新口径。
//!
//! # 权限
//!
//! 读：`business:erp:report-item:list`；写：`business:erp:report-item:manage`。
//! 迁移 `V0256` 种两个节点，写权限只授 `SUPER_ADMIN` 与 `FINANCE_MANAGER`
//! （报表口径 = 财务规则，不是普通主数据）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;
use crate::utils;

use super::income_statement as income_report;
use super::report;
use super::report_item_model::{self as model, AccountOption, ItemRow, ItemWrite};

/// 读权限码（与 `V0256` 的 `sys_menu.auth_code` 逐字一致）。
pub const PERM_LIST: &str = "business:erp:report-item:list";
/// 写权限码（同上）。
pub const PERM_MANAGE: &str = "business:erp:report-item:manage";

/// 科目类别（与 `biz_account.account_category` 的取值一致；`report::category_side` 认这五个）。
const ACCOUNT_CATEGORIES: [&str; 5] = ["ASSET", "LIABILITY", "EQUITY", "COST", "PROFIT_LOSS"];

/// 运行时补出来的「未归类差额」三行：不是 `biz_report_item` 的配置行，
/// 但公式可以引用它们（V0183/V0184 的 `TOTAL_*` 就这么写的）。
const RESERVED_CODES: [&str; 3] = [
    report::UNMAPPED_ASSETS,
    report::UNMAPPED_LIABILITIES,
    report::UNMAPPED_EQUITY,
];

/// 报表中文名（库里没有这张表，报表名是代码常量）。
fn report_name(code: &str) -> &'static str {
    match code {
        report::REPORT_CODE => report::REPORT_NAME,
        income_report::REPORT_CODE => income_report::REPORT_NAME,
        _ => "未命名报表",
    }
}

// ==================== 错误出口 ====================

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn not_found(id: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("报表项不存在（id={id}）"),
    )
}

fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| invalid(format!("{label}「{raw}」不是合法的 UUID")))
}

// ==================== 出参 ====================

/// 报表项（维护面）。
#[derive(Debug, Clone, Serialize)]
pub struct ItemRes {
    /// 主键。
    pub id: String,
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 报表项编码。
    pub item_code: String,
    /// 报表项名称。
    pub item_name: String,
    /// 层级。
    pub item_level: i16,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 所属方。
    pub side: String,
    /// 科目匹配模式。
    pub account_pattern: Option<String>,
    /// 计算式。
    pub formula: Option<String>,
    /// 是否减项。
    pub is_deduction: bool,
    /// 是否启用。
    pub is_active: bool,
    /// 排序号。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 更新时间。
    pub updated_at: Option<String>,
    /// 纯标题行（层级 1 且无取数来源 ⇒ 金额恒 NULL）。
    pub is_title: bool,
    /// 引用本项的其它报表项编码（停用前必须为空）。
    pub referenced_by: Vec<String>,
    /// 取数模式当前命中的科目数（`None` = 公式项 / 标题行，不适用）。
    pub matched_account_count: Option<usize>,
}

/// 保存结果：改完的行 + 非阻断告警。
#[derive(Debug, Clone, Serialize)]
pub struct ItemSaveRes {
    /// 保存后的行。
    pub item: ItemRes,
    /// 非阻断告警（模式命中 0 个科目、利润表所属方不是 EQUITY 等）。
    pub warnings: Vec<String>,
}

/// 报表字典。
#[derive(Debug, Clone, Serialize)]
pub struct ReportMetaRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 报表项行数（含停用）。
    pub item_count: usize,
}

/// 通用字典项。
#[derive(Debug, Clone, Serialize)]
pub struct DictItemRes {
    /// 值。
    pub value: String,
    /// 中文名。
    pub label: String,
}

/// 取数模式语法提示（前端表单下方逐条展示，不再各写一份文档）。
#[derive(Debug, Clone, Serialize)]
pub struct PatternTipRes {
    /// 写法。
    pub syntax: String,
    /// 含义。
    pub meaning: String,
    /// 例。
    pub example: String,
}

/// 字典出参。
#[derive(Debug, Clone, Serialize)]
pub struct CatalogRes {
    /// 库里真实存在的报表（页签来源）。
    pub reports: Vec<ReportMetaRes>,
    /// 所属方枚举。
    pub sides: Vec<DictItemRes>,
    /// 层级枚举。
    pub levels: Vec<DictItemRes>,
    /// 取数模式语法。
    pub pattern_tips: Vec<PatternTipRes>,
    /// 报表项总行数。
    pub total_items: usize,
}

/// 取数预览出参。
#[derive(Debug, Clone, Serialize)]
pub struct PreviewRes {
    /// 命中的科目数。
    pub matched_count: usize,
    /// 命中的科目（最多 200 条，按编码升序）。
    pub matched_accounts: Vec<AccountBriefRes>,
    /// 阻断性问题（保存会被拒）。
    pub problems: Vec<String>,
    /// 非阻断告警。
    pub warnings: Vec<String>,
}

/// 科目摘要。
#[derive(Debug, Clone, Serialize)]
pub struct AccountBriefRes {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
}

// ==================== 入参 ====================

/// `GET /business/erp/report-item/list` 参数。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ItemListQuery {
    /// 报表编码（空 = 全部）。
    pub report_code: Option<String>,
}

/// 新增 / 编辑共用请求体。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ItemUpsertReq {
    /// 报表编码（编辑时不可变）。
    pub report_code: Option<String>,
    /// 报表项编码（编辑时不可变）。
    pub item_code: Option<String>,
    /// 名称。
    pub item_name: Option<String>,
    /// 层级。
    pub item_level: Option<i16>,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 所属方。
    pub side: Option<String>,
    /// 取数模式。
    pub account_pattern: Option<String>,
    /// 计算式。
    pub formula: Option<String>,
    /// 是否减项。
    pub is_deduction: Option<bool>,
    /// 是否启用。
    pub is_active: Option<bool>,
    /// 排序号（省略 = 新建排在末尾 / 编辑保留原值）。
    pub sort_order: Option<i32>,
    /// 备注。
    pub remark: Option<String>,
}

/// 启停请求体。
#[derive(Debug, Deserialize)]
pub struct ItemToggleReq {
    /// 目标状态。
    pub is_active: bool,
}

/// 整表重排请求体。
#[derive(Debug, Deserialize)]
pub struct ItemSortReq {
    /// 报表编码。
    pub report_code: String,
    /// 该报表全部报表项的主键，按目标顺序（必须是全量排列）。
    pub ids: Vec<String>,
}

/// 取数预览请求体。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ItemPreviewReq {
    /// 报表编码。
    pub report_code: Option<String>,
    /// 取数模式。
    pub account_pattern: Option<String>,
    /// 计算式。
    pub formula: Option<String>,
    /// 正在编辑的行（编辑时排除它自己，避免自引用误判）。
    pub exclude_id: Option<String>,
    /// 正在编辑的行的排序号（不算就按末尾）。
    pub sort_order: Option<i32>,
}

// ==================== 纯逻辑：校验 ====================

/// 去空白；空串归一成 `None`。
fn trimmed(raw: &Option<String>) -> Option<String> {
    raw.as_ref()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 报表项编码：大写字母开头，只含大写字母 / 数字 / 下划线，≤32（VARCHAR(32)）。
fn validate_item_code(raw: &str) -> std::result::Result<String, String> {
    let code = raw.trim().to_ascii_uppercase();
    if code.is_empty() {
        return Err("报表项编码必填".to_string());
    }
    if code.len() > 32 {
        return Err(format!("报表项编码「{code}」超过 32 个字符"));
    }
    let mut chars = code.chars();
    let first = chars.next().unwrap_or('_');
    if !first.is_ascii_uppercase() {
        return Err(format!("报表项编码「{code}」必须大写字母开头"));
    }
    if !code
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "报表项编码「{code}」只能包含大写字母 / 数字 / 下划线（公式靠它引用）"
        ));
    }
    Ok(code)
}

/// 取数模式语法：`1001,1002` / `2221`（含下级）/ `1*` `1%`（前缀）/ `@CATEGORY:类别`。
fn validate_pattern(pattern: &str) -> std::result::Result<(), String> {
    for raw in pattern.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            return Err("取数模式里有空项（逗号之间不能为空）".to_string());
        }
        if let Some(category) = token.strip_prefix("@CATEGORY:") {
            let category = category.trim();
            if !ACCOUNT_CATEGORIES
                .iter()
                .any(|known| known.eq_ignore_ascii_case(category))
            {
                return Err(format!(
                    "「@CATEGORY:{category}」不是合法科目类别（可选 {}）",
                    ACCOUNT_CATEGORIES.join(" / ")
                ));
            }
            continue;
        }
        let (prefix, is_prefix) = match token.strip_suffix('*').or_else(|| token.strip_suffix('%'))
        {
            Some(prefix) => (prefix, true),
            None => (token, false),
        };
        if prefix.is_empty() {
            return Err(format!("取数模式「{token}」的通配前缀为空"));
        }
        if is_prefix && (prefix.ends_with('*') || prefix.ends_with('%')) {
            return Err(format!("取数模式「{token}」只允许一个通配符"));
        }
        if !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(format!(
                "取数模式「{token}」含非法字符（科目编码只允许字母 / 数字 / . / _ / -）"
            ));
        }
    }
    Ok(())
}

/// 解析计算式里的报表项引用（顺序保留）。语法与 [`super::report::eval_formula`] 同源。
fn formula_operands(formula: &str) -> std::result::Result<Vec<String>, String> {
    let mut operands = Vec::new();
    let mut expect_operand = true;
    for raw in formula.split_whitespace() {
        match raw {
            "+" | "-" => {
                if expect_operand {
                    return Err(format!("算式「{formula}」出现连续运算符"));
                }
                expect_operand = true;
            }
            operand => {
                if !expect_operand {
                    return Err(format!(
                        "算式「{formula}」缺少运算符（`{operand}` 前）—— 只支持空格分隔的 + / -"
                    ));
                }
                operands.push(operand.to_string());
                expect_operand = false;
            }
        }
    }
    if operands.is_empty() {
        return Err(format!("算式「{formula}」不含任何报表项"));
    }
    if expect_operand {
        return Err(format!("算式「{formula}」以运算符结尾"));
    }
    Ok(operands)
}

/// 候选报表项（校验用；不碰库）。
#[derive(Debug, Clone)]
struct Candidate {
    item_code: String,
    item_level: i16,
    parent_code: Option<String>,
    account_pattern: Option<String>,
    formula: Option<String>,
    is_active: bool,
    sort_order: i32,
}

impl Candidate {
    fn from_row(row: &ItemRow) -> Self {
        Self {
            item_code: row.item_code.clone(),
            item_level: row.item_level,
            parent_code: row.parent_code.clone(),
            account_pattern: row.account_pattern.clone(),
            formula: row.formula.clone(),
            is_active: row.is_active,
            sort_order: row.sort_order,
        }
    }
}

/// 整表校验（阻断性问题）。
///
/// 只校验启用行：报表只加载 `is_active = 1`，停用行的公式不会被执行，
/// 但**停用行不能是别人公式的依赖**（由 [`dependents_of`] 单独把关）。
fn validate_report(items: &[Candidate]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut active: Vec<&Candidate> = items.iter().filter(|item| item.is_active).collect();
    active.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.item_code.cmp(&b.item_code))
    });
    let position: std::collections::HashMap<&str, usize> = active
        .iter()
        .enumerate()
        .map(|(index, item)| (item.item_code.as_str(), index))
        .collect();
    let all_codes: std::collections::HashSet<&str> =
        items.iter().map(|item| item.item_code.as_str()).collect();

    for item in &active {
        if let Some(parent) = item.parent_code.as_deref() {
            if parent == item.item_code {
                problems.push(format!("「{}」的上级不能是自己", item.item_code));
            } else if !all_codes.contains(parent) {
                problems.push(format!(
                    "「{}」的上级报表项「{parent}」不存在（同报表内）",
                    item.item_code
                ));
            }
        }
        if item.account_pattern.is_none() && item.formula.is_none() && item.item_level != 1 {
            problems.push(format!(
                "「{}」既没有取数模式也没有计算式 —— 只有层级 1 的大类标题行可以没有来源",
                item.item_code
            ));
        }
        let Some(formula) = item.formula.as_deref() else {
            continue;
        };
        let operands = match formula_operands(formula) {
            Ok(operands) => operands,
            Err(message) => {
                problems.push(format!("「{}」{message}", item.item_code));
                continue;
            }
        };
        let own = position.get(item.item_code.as_str()).copied().unwrap_or(0);
        for operand in operands {
            if operand == item.item_code {
                problems.push(format!("「{}」的计算式引用了自己（成环）", item.item_code));
                continue;
            }
            // 三行「未归类差额」不是配置行，是运行时按类别差额补出来的
            // （`report::compute` 第 1.5 遍插入 UNMAPPED_*），所以它们**不在表里也永远可用**，
            // 位置也不受排序影响 —— 放行，否则 V0183/V0184 种的 TOTal_* 合计行会被误判成死引用。
            if RESERVED_CODES.contains(&operand.as_str()) {
                continue;
            }
            match position.get(operand.as_str()) {
                None => problems.push(format!(
                    "「{}」的计算式引用了不存在或已停用的报表项「{operand}」",
                    item.item_code
                )),
                Some(other) if *other >= own => problems.push(format!(
                    "「{}」的计算式引用了排在自己后面的「{operand}」—— 求值顺序即排序，被引用项必须排在前面（调小它的排序号）",
                    item.item_code
                )),
                Some(_) => {}
            }
        }
    }
    problems.dedup();
    problems
}

/// 引用了 `item_code` 的启用项编码（停用前的阻断判据）。
fn dependents_of(items: &[ItemRow], item_code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in items.iter().filter(|row| row.is_active) {
        let Some(formula) = item.formula.as_deref() else {
            continue;
        };
        if formula.split_whitespace().any(|token| token == item_code) {
            out.push(item.item_code.clone());
        }
    }
    out.sort();
    out
}

/// 命中的科目（与报表计算同一个匹配函数，绝不另写一套）。
fn matched(pattern: &str, accounts: &[AccountOption]) -> Vec<AccountOption> {
    accounts
        .iter()
        .filter(|account| {
            report::match_account(pattern, &account.account_code, &account.account_category)
        })
        .cloned()
        .collect()
}

/// 行 → 出参（`items` 必须是同一张报表的全部行，用于算引用关系）。
fn item_res(row: &ItemRow, items: &[ItemRow], accounts: &[AccountOption]) -> ItemRes {
    let matched_count = row
        .account_pattern
        .as_deref()
        .map(|pattern| matched(pattern, accounts).len());
    ItemRes {
        id: row.id.to_string(),
        report_code: row.report_code.clone(),
        report_name: report_name(&row.report_code).to_string(),
        item_code: row.item_code.clone(),
        item_name: row.item_name.clone(),
        item_level: row.item_level,
        parent_code: row.parent_code.clone(),
        side: row.side.clone(),
        account_pattern: row.account_pattern.clone(),
        formula: row.formula.clone(),
        is_deduction: row.is_deduction,
        is_active: row.is_active,
        sort_order: row.sort_order,
        remark: row.remark.clone(),
        updated_at: row.updated_at.clone(),
        is_title: row.item_level == 1 && row.account_pattern.is_none() && row.formula.is_none(),
        referenced_by: dependents_of(items, &row.item_code)
            .into_iter()
            .filter(|code| *code != row.item_code)
            .collect(),
        matched_account_count: matched_count,
    }
}

// ==================== ① 字典 ====================

/// 维护页字典：报表 / 所属方 / 层级 / 取数模式语法。
#[nik::permission(
    path = "/business/erp/report-item/catalog",
    method = "GET",
    permission = "business:erp:report-item:list"
)]
#[nik::get("/business/erp/report-item/catalog")]
pub async fn report_item_catalog() -> ResResult<CatalogRes> {
    let rows = model::list_items(db(), None)
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;
    let codes = model::report_codes(db())
        .await
        .map_err(|error| utils::db_error(error, "查询报表编码"))?;
    let reports = codes
        .iter()
        .map(|code| ReportMetaRes {
            report_code: code.clone(),
            report_name: report_name(code).to_string(),
            item_count: rows.iter().filter(|row| &row.report_code == code).count(),
        })
        .collect();
    Ok(Res::ok(CatalogRes {
        reports,
        sides: report::SIDES
            .iter()
            .map(|side| DictItemRes {
                value: (*side).to_string(),
                label: match *side {
                    "ASSET" => "资产",
                    "LIABILITY" => "负债",
                    _ => "所有者权益 / 损益",
                }
                .to_string(),
            })
            .collect(),
        levels: vec![
            DictItemRes {
                value: "1".to_string(),
                label: "1 大类（可作纯标题行，金额为空）".to_string(),
            },
            DictItemRes {
                value: "2".to_string(),
                label: "2 小类 / 明细".to_string(),
            },
            DictItemRes {
                value: "3".to_string(),
                label: "3 更细的明细".to_string(),
            },
        ],
        pattern_tips: vec![
            PatternTipRes {
                syntax: "1001,1002".to_string(),
                meaning: "科目编码清单（逗号分隔取并集）".to_string(),
                example: "货币资金 = 库存现金 + 银行存款".to_string(),
            },
            PatternTipRes {
                syntax: "2221".to_string(),
                meaning: "该科目 + 其全部下级（2221 / 2221.01 / 2221.02）".to_string(),
                example: "应交税费含所有明细科目".to_string(),
            },
            PatternTipRes {
                syntax: "1* / 1%".to_string(),
                meaning: "编码前缀匹配（1 开头的全部科目）".to_string(),
                example: "1* = 全部资产类科目".to_string(),
            },
            PatternTipRes {
                syntax: "@CATEGORY:PROFIT_LOSS".to_string(),
                meaning: format!(
                    "按科目类别整类匹配（可选 {}）",
                    ACCOUNT_CATEGORIES.join(" / ")
                ),
                example: "整类损益科目".to_string(),
            },
        ],
        total_items: rows.len(),
    }))
}

// ==================== ② 列表 ====================

/// 报表项列表（含停用行；按 `sort_order` 排，与报表计算顺序一致）。
#[nik::permission(
    path = "/business/erp/report-item/list",
    method = "GET",
    permission = "business:erp:report-item:list"
)]
#[nik::get("/business/erp/report-item/list")]
pub async fn report_item_list(Query(query): Query<ItemListQuery>) -> ResResult<Vec<ItemRes>> {
    let report_code = trimmed(&query.report_code);
    let rows = model::list_items(db(), report_code.as_deref())
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    let list = rows
        .iter()
        .map(|row| item_res(row, &rows, &accounts))
        .collect();
    Ok(Res::ok(list))
}

// ==================== ③ 预览 ====================

/// 取数预览：当前模式会命中哪些科目 / 公式引用了哪些项。
#[nik::permission(
    path = "/business/erp/report-item/preview",
    method = "POST",
    permission = "business:erp:report-item:list"
)]
#[nik::post("/business/erp/report-item/preview")]
pub async fn report_item_preview(Json(req): Json<ItemPreviewReq>) -> ResResult<PreviewRes> {
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    let report_code = trimmed(&req.report_code)
        .ok_or_else(|| invalid("report_code 必填（取数预览要按报表校验公式）"))?;
    let rows = model::list_items(db(), Some(&report_code))
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;

    let pattern = trimmed(&req.account_pattern);
    let formula = trimmed(&req.formula);
    let mut problems = Vec::new();
    let mut warnings = Vec::new();

    if pattern.is_none() && formula.is_none() {
        warnings.push(
            "既没有取数模式也没有计算式：只有层级 1 的大类标题行允许这样（金额恒为空）".to_string(),
        );
    }
    if let Some(message) = pattern
        .as_deref()
        .and_then(|pattern| validate_pattern(pattern).err())
    {
        problems.push(message);
    }
    if let Some(message) = formula
        .as_deref()
        .and_then(|formula| formula_operands(formula).err())
    {
        problems.push(message);
    }

    // 把「正在编辑的这一行」塞进候选集，走与保存完全相同的整表校验。
    let exclude_id = trimmed(&req.exclude_id)
        .map(|raw| parse_uuid_field(&raw, "exclude_id"))
        .transpose()?;
    let existing = exclude_id.and_then(|id| rows.iter().find(|row| row.id == id).cloned());
    let sort_order = req.sort_order.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|row| row.sort_order)
            .unwrap_or_else(|| rows.iter().map(|row| row.sort_order).max().unwrap_or(0) + 10)
    });
    let mut candidates: Vec<Candidate> = rows
        .iter()
        .filter(|row| Some(row.id) != exclude_id)
        .map(Candidate::from_row)
        .collect();
    candidates.push(Candidate {
        item_code: existing
            .as_ref()
            .map(|row| row.item_code.clone())
            .unwrap_or_else(|| "PREVIEW_ITEM".to_string()),
        item_level: existing.as_ref().map(|row| row.item_level).unwrap_or(2),
        parent_code: existing.as_ref().and_then(|row| row.parent_code.clone()),
        account_pattern: pattern.clone(),
        formula: formula.clone(),
        is_active: true,
        sort_order,
    });
    problems.extend(validate_report(&candidates));

    let hits = pattern
        .as_deref()
        .map(|pattern| matched(pattern, &accounts))
        .unwrap_or_default();
    if pattern.is_some() && hits.is_empty() {
        warnings.push("当前取数模式没有命中任何启用科目 —— 启用后这一行会恒为 0（是「没发生」还是「配错了」报表上看不出来）".to_string());
    }

    problems.dedup();
    Ok(Res::ok(PreviewRes {
        matched_count: hits.len(),
        matched_accounts: hits
            .iter()
            .take(200)
            .map(|account| AccountBriefRes {
                account_code: account.account_code.clone(),
                account_name: account.account_name.clone(),
                account_category: account.account_category.clone(),
            })
            .collect(),
        problems,
        warnings,
    }))
}

// ==================== ④ 新增 ====================

/// 新增报表项。
#[nik::permission(
    path = "/business/erp/report-item",
    method = "POST",
    permission = "business:erp:report-item:manage"
)]
#[nik::post("/business/erp/report-item")]
pub async fn report_item_create(Json(req): Json<ItemUpsertReq>) -> ResResult<ItemSaveRes> {
    let report_code = trimmed(&req.report_code).ok_or_else(|| invalid("report_code 必填"))?;
    let codes = model::report_codes(db())
        .await
        .map_err(|error| utils::db_error(error, "查询报表编码"))?;
    if !codes.iter().any(|code| code == &report_code) {
        return Err(invalid(format!(
            "报表「{report_code}」不存在（可选：{}）",
            codes.join(" / ")
        )));
    }
    let item_code = validate_item_code(req.item_code.as_deref().unwrap_or("")).map_err(invalid)?;
    let item_name = trimmed(&req.item_name).ok_or_else(|| invalid("报表项名称必填"))?;
    if item_name.chars().count() > 64 {
        return Err(invalid("报表项名称超过 64 个字符（VARCHAR(64)）"));
    }
    let item_level = req.item_level.unwrap_or(2);
    if !(1..=3).contains(&item_level) {
        return Err(invalid("层级只能是 1 / 2 / 3"));
    }
    let side = trimmed(&req.side)
        .unwrap_or_else(|| "ASSET".to_string())
        .to_ascii_uppercase();
    if !report::SIDES.contains(&side.as_str()) {
        return Err(invalid(format!(
            "所属方「{side}」不合法（可选：{}）",
            report::SIDES.join(" / ")
        )));
    }
    let account_pattern = trimmed(&req.account_pattern);
    let formula = trimmed(&req.formula);
    if account_pattern.is_none() && formula.is_none() && item_level != 1 {
        return Err(invalid(
            "取数模式与计算式必须二选一（只有层级 1 的大类标题行可以两者都不填）",
        ));
    }
    if account_pattern.is_some() && formula.is_some() {
        return Err(invalid(
            "取数模式与计算式只能填一个：一行要么按科目取数、要么引用其它报表项",
        ));
    }
    if let Some(pattern) = account_pattern.as_deref() {
        validate_pattern(pattern).map_err(invalid)?;
    }
    if let Some(formula) = formula.as_deref() {
        formula_operands(formula).map_err(invalid)?;
    }

    let rows = model::list_items(db(), Some(&report_code))
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;
    if rows.iter().any(|row| row.item_code == item_code) {
        return Err(conflict(format!(
            "报表项编码「{item_code}」在 {report_code} 里已存在（同报表内唯一，公式靠它引用）"
        )));
    }
    let sort_order = req
        .sort_order
        .unwrap_or_else(|| rows.iter().map(|row| row.sort_order).max().unwrap_or(0) + 10);

    let candidate = Candidate {
        item_code: item_code.clone(),
        item_level,
        parent_code: trimmed(&req.parent_code),
        account_pattern: account_pattern.clone(),
        formula: formula.clone(),
        is_active: req.is_active.unwrap_or(true),
        sort_order,
    };
    let mut candidates: Vec<Candidate> = rows.iter().map(Candidate::from_row).collect();
    candidates.push(candidate.clone());
    let problems = validate_report(&candidates);
    if !problems.is_empty() {
        return Err(invalid(format!(
            "报表配置校验未通过：{}",
            problems.join("；")
        )));
    }

    let spec = ItemWrite {
        report_code: report_code.clone(),
        item_code: item_code.clone(),
        item_name: item_name.clone(),
        item_level,
        parent_code: candidate.parent_code.clone(),
        side: side.clone(),
        account_pattern: account_pattern.clone(),
        formula: formula.clone(),
        is_deduction: req.is_deduction.unwrap_or(false),
        is_active: candidate.is_active,
        sort_order,
        remark: trimmed(&req.remark),
    };
    let id = Uuid::now_v7();
    model::insert_item(db(), id, &spec)
        .await
        .map_err(|error| utils::db_error(error, "新增报表项"))?;
    tracing::info!(%id, %report_code, %item_code, "报表项已新增");

    let rows = model::list_items(db(), Some(&report_code))
        .await
        .map_err(|error| utils::db_error(error, "读回报表项"))?;
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    let saved = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| not_found(&id.to_string()))?;
    let mut warnings = Vec::new();
    if let Some(pattern) = account_pattern.as_deref() {
        let hits = matched(pattern, &accounts).len();
        if hits == 0 {
            warnings.push(format!(
                "取数模式「{pattern}」没有命中任何启用科目 —— 该行会恒为 0"
            ));
        }
    }
    if report_code == income_report::REPORT_CODE && side != "EQUITY" {
        warnings.push("利润表的报表项通常用 EQUITY 所属方（沿用资产负债表的 ASSET / LIABILITY 会让两表口径不一致）".to_string());
    }
    Ok(Res::ok(ItemSaveRes {
        item: item_res(saved, &rows, &accounts),
        warnings,
    }))
}

// ==================== ⑤ 编辑 ====================

/// 编辑报表项（`report_code` / `item_code` 不可变）。
#[nik::permission(
    path = "/business/erp/report-item/{id}",
    method = "PUT",
    permission = "business:erp:report-item:manage"
)]
#[nik::put("/business/erp/report-item/{id}")]
pub async fn report_item_update(
    Path(id): Path<String>,
    Json(req): Json<ItemUpsertReq>,
) -> ResResult<ItemSaveRes> {
    let id = parse_uuid_field(&id, "id")?;
    let existing = model::find_item(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?
        .ok_or_else(|| not_found(&id.to_string()))?;

    if let Some(raw) = trimmed(&req.report_code).filter(|raw| raw != &existing.report_code) {
        return Err(invalid(format!(
            "报表编码不可修改（当前 {} / 请求 {raw}）：换报表请新建一行",
            existing.report_code
        )));
    }
    if let Some(raw) = trimmed(&req.item_code).filter(|raw| raw != &existing.item_code) {
        return Err(invalid(format!(
            "报表项编码不可修改（当前 {} / 请求 {raw}）：公式靠它引用，改名会让引用变成死链",
            existing.item_code
        )));
    }

    let item_name = trimmed(&req.item_name).unwrap_or_else(|| existing.item_name.clone());
    if item_name.chars().count() > 64 {
        return Err(invalid("报表项名称超过 64 个字符（VARCHAR(64)）"));
    }
    let item_level = req.item_level.unwrap_or(existing.item_level);
    if !(1..=3).contains(&item_level) {
        return Err(invalid("层级只能是 1 / 2 / 3"));
    }
    let side = trimmed(&req.side)
        .unwrap_or_else(|| existing.side.clone())
        .to_ascii_uppercase();
    if !report::SIDES.contains(&side.as_str()) {
        return Err(invalid(format!(
            "所属方「{side}」不合法（可选：{}）",
            report::SIDES.join(" / ")
        )));
    }
    let account_pattern = match &req.account_pattern {
        Some(_) => trimmed(&req.account_pattern),
        None => existing.account_pattern.clone(),
    };
    let formula = match &req.formula {
        Some(_) => trimmed(&req.formula),
        None => existing.formula.clone(),
    };
    if account_pattern.is_none() && formula.is_none() && item_level != 1 {
        return Err(invalid(
            "取数模式与计算式必须二选一（只有层级 1 的大类标题行可以两者都不填）",
        ));
    }
    if account_pattern.is_some() && formula.is_some() {
        return Err(invalid(
            "取数模式与计算式只能填一个：清空其中一个再保存（空串 = 清空）",
        ));
    }
    if let Some(pattern) = account_pattern.as_deref() {
        validate_pattern(pattern).map_err(invalid)?;
    }
    if let Some(formula) = formula.as_deref() {
        formula_operands(formula).map_err(invalid)?;
    }
    let parent_code = match &req.parent_code {
        Some(_) => trimmed(&req.parent_code),
        None => existing.parent_code.clone(),
    };
    if parent_code.as_deref() == Some(existing.item_code.as_str()) {
        return Err(invalid("上级报表项不能是自己"));
    }
    let is_active = req.is_active.unwrap_or(existing.is_active);
    let sort_order = req.sort_order.unwrap_or(existing.sort_order);

    let rows = model::list_items(db(), Some(&existing.report_code))
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;
    let candidate = Candidate {
        item_code: existing.item_code.clone(),
        item_level,
        parent_code: parent_code.clone(),
        account_pattern: account_pattern.clone(),
        formula: formula.clone(),
        is_active,
        sort_order,
    };
    let candidates: Vec<Candidate> = rows
        .iter()
        .map(|row| {
            if row.id == id {
                candidate.clone()
            } else {
                Candidate::from_row(row)
            }
        })
        .collect();
    if !is_active && existing.is_active {
        // 停用：先挡住「被别人公式引用」，再走整表校验（停用后那些公式会变成死引用）。
        let dependents = dependents_of(&rows, &existing.item_code);
        if !dependents.is_empty() {
            return Err(conflict(format!(
                "「{}」被 {} 个报表项的计算式引用（{}），不能停用：请先改掉那些公式",
                existing.item_code,
                dependents.len(),
                dependents.join(" / ")
            )));
        }
    }
    // 停用后整表校验只看启用行；把 candidate 的 is_active 带进去，引用它的公式会在上面被拦。
    let problems = validate_report(&candidates);
    if !problems.is_empty() {
        return Err(invalid(format!(
            "报表配置校验未通过：{}",
            problems.join("；")
        )));
    }

    let spec = ItemWrite {
        report_code: existing.report_code.clone(),
        item_code: existing.item_code.clone(),
        item_name,
        item_level,
        parent_code,
        side: side.clone(),
        account_pattern: account_pattern.clone(),
        formula,
        is_deduction: req.is_deduction.unwrap_or(existing.is_deduction),
        is_active,
        sort_order,
        remark: match &req.remark {
            Some(_) => trimmed(&req.remark),
            None => existing.remark.clone(),
        },
    };
    let affected = model::update_item(db(), id, &spec)
        .await
        .map_err(|error| utils::db_error(error, "修改报表项"))?;
    if affected == 0 {
        return Err(not_found(&id.to_string()));
    }
    tracing::info!(%id, item_code = %existing.item_code, "报表项已修改");

    let rows = model::list_items(db(), Some(&existing.report_code))
        .await
        .map_err(|error| utils::db_error(error, "读回报表项"))?;
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    let saved = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| not_found(&id.to_string()))?;
    let mut warnings = Vec::new();
    if let Some(pattern) = account_pattern.as_deref() {
        let hits = matched(pattern, &accounts).len();
        if hits == 0 {
            warnings.push(format!(
                "取数模式「{pattern}」没有命中任何启用科目 —— 该行会恒为 0"
            ));
        }
    }
    if existing.report_code == income_report::REPORT_CODE && side != "EQUITY" {
        warnings.push("利润表的报表项通常用 EQUITY 所属方".to_string());
    }
    Ok(Res::ok(ItemSaveRes {
        item: item_res(saved, &rows, &accounts),
        warnings,
    }))
}

// ==================== ⑥ 启停 ====================

/// 启停报表项（停用前必须解除其它报表项的公式引用）。
#[nik::permission(
    path = "/business/erp/report-item/{id}/toggle",
    method = "PUT",
    permission = "business:erp:report-item:manage"
)]
#[nik::put("/business/erp/report-item/{id}/toggle")]
pub async fn report_item_toggle(
    Path(id): Path<String>,
    Json(req): Json<ItemToggleReq>,
) -> ResResult<ItemRes> {
    let id = parse_uuid_field(&id, "id")?;
    let existing = model::find_item(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?
        .ok_or_else(|| not_found(&id.to_string()))?;
    if existing.is_active == req.is_active {
        let rows = model::list_items(db(), Some(&existing.report_code))
            .await
            .map_err(|error| utils::db_error(error, "查询报表项"))?;
        let accounts = model::account_catalog(db())
            .await
            .map_err(|error| utils::db_error(error, "查询科目"))?;
        return Ok(Res::ok(item_res(&existing, &rows, &accounts)));
    }
    if !req.is_active {
        let rows = model::list_items(db(), Some(&existing.report_code))
            .await
            .map_err(|error| utils::db_error(error, "查询报表项"))?;
        let dependents = dependents_of(&rows, &existing.item_code);
        if !dependents.is_empty() {
            return Err(conflict(format!(
                "「{}」被 {} 个报表项的计算式引用（{}），不能停用：请先改掉那些公式",
                existing.item_code,
                dependents.len(),
                dependents.join(" / ")
            )));
        }
    }
    let affected = model::set_active(db(), id, req.is_active)
        .await
        .map_err(|error| utils::db_error(error, "启停报表项"))?;
    if affected == 0 {
        return Err(not_found(&id.to_string()));
    }
    tracing::info!(%id, item_code = %existing.item_code, active = req.is_active, "报表项启停");
    let rows = model::list_items(db(), Some(&existing.report_code))
        .await
        .map_err(|error| utils::db_error(error, "读回报表项"))?;
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    let saved = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| not_found(&id.to_string()))?;
    Ok(Res::ok(item_res(saved, &rows, &accounts)))
}

// ==================== ⑦ 排序 ====================

/// 整表重排：按传入的 id 顺序重写 `sort_order`（10 / 20 / 30 …）。
///
/// 为什么传全量 id 而不是「上移一格」：公式求值顺序就是 `sort_order`，
/// 局部交换无法判断「换完之后还有没有公式引用落在自己后面」——
/// 全量顺序一次校验，要么整表合法要么不写库。
#[nik::permission(
    path = "/business/erp/report-item/sort",
    method = "PUT",
    permission = "business:erp:report-item:manage"
)]
#[nik::put("/business/erp/report-item/sort")]
pub async fn report_item_sort(Json(req): Json<ItemSortReq>) -> ResResult<Vec<ItemRes>> {
    let report_code = req.report_code.trim().to_string();
    if report_code.is_empty() {
        return Err(invalid("report_code 必填"));
    }
    let rows = model::list_items(db(), Some(&report_code))
        .await
        .map_err(|error| utils::db_error(error, "查询报表项"))?;
    if rows.is_empty() {
        return Err(invalid(format!("报表「{report_code}」没有任何报表项")));
    }
    let mut ids = Vec::with_capacity(req.ids.len());
    for raw in &req.ids {
        ids.push(parse_uuid_field(raw, "ids")?);
    }
    let wanted: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
    if ids.len() != wanted.len() {
        return Err(invalid("ids 里有重复项"));
    }
    let existing: std::collections::HashSet<Uuid> = rows.iter().map(|row| row.id).collect();
    if wanted != existing {
        return Err(invalid(format!(
            "ids 必须是 {report_code} 全部报表项的一个排列（库里 {} 行，请求 {} 行）",
            existing.len(),
            ids.len()
        )));
    }

    let mut position: std::collections::HashMap<Uuid, i32> = std::collections::HashMap::new();
    for (index, id) in ids.iter().enumerate() {
        position.insert(*id, ((index as i32) + 1) * 10);
    }
    let candidates: Vec<Candidate> = rows
        .iter()
        .map(|row| {
            let mut candidate = Candidate::from_row(row);
            if let Some(value) = position.get(&row.id) {
                candidate.sort_order = *value;
            }
            candidate
        })
        .collect();
    let problems = validate_report(&candidates);
    if !problems.is_empty() {
        return Err(invalid(format!(
            "这个顺序会让报表配置不合法：{}",
            problems.join("；")
        )));
    }

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启排序事务"))?;
    for id in &ids {
        let order = position.get(id).copied().unwrap_or(0);
        if let Err(error) = model::set_sort_order(&tx, *id, order).await {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "写报表项排序"));
        }
    }
    if let Err(error) = tx.commit().await {
        return Err(utils::db_error(error, "提交报表项排序"));
    }
    tracing::info!(%report_code, count = ids.len(), "报表项顺序已重排");

    let rows = model::list_items(db(), Some(&report_code))
        .await
        .map_err(|error| utils::db_error(error, "读回报表项"))?;
    let accounts = model::account_catalog(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目"))?;
    Ok(Res::ok(
        rows.iter()
            .map(|row| item_res(row, &rows, &accounts))
            .collect(),
    ))
}
