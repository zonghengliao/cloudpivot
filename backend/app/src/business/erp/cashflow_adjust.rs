//! 间接法调整项配置（M1-15 · 表 `biz_cashflow_adjust_item` / 迁移 `V0416`）。
//!
//! 现金流量表有两种编制口径。**直接法**（`cash_flow.rs` / 底稿 `cashflow_worksheet.rs`）
//! 一行一行数「这笔钱花到哪去了」；**间接法**从**净利润**出发，把「账面利润」与
//! 「经营活动现金净流入」之间的差异原因逐条调回来：
//!
//! ```text
//! 经营活动产生的现金流量净额 = 净利润 + Σ(调整项)
//! ```
//!
//! 本模块只做**调整项的配置**那一件事：哪些行、按什么口径取数、什么顺序、启用与否。
//! **不算金额、不出表**：净利润取利润表（ERP-R2），调整项金额取科目发生额 / 余额变动，
//! 都由 M1-16 / M1-17 实现。这里给它们的契约就是「一行 = 一条口径」。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/cash-flow/adjust-item/list` | `business:erp:cashflow-adjust:list` | 全部调整项（含停用）+ 每行取数自检 + 跨行告警 |
//! | GET | `/business/erp/report/cash-flow/adjust-item/catalog` | 同上 | 字典：取数来源 / 4 个口径 / 模式语法 / 两个固定行 |
//! | POST | `/business/erp/report/cash-flow/adjust-item` | `business:erp:cashflow-adjust:manage` | 新增调整项 |
//! | PUT | `/business/erp/report/cash-flow/adjust-item/{id}` | 同上 | 编辑（`item_code` 不可变） |
//! | PUT | `/business/erp/cash-flow/adjust-item/{id}/toggle` 见下 | 同上 | 启停 |
//!
//! 实际 toggle 路径是 `/business/erp/report/cash-flow/adjust-item/{id}/toggle`（与其余四行同前缀）。
//!
//! # 为什么方向不设「加 / 减」列，只设取数口径
//!
//! 准则里的间接法表每一行都印「加：」，方向由**数值正负**表达（「减少（增加以『−』号
//! 填列）」就是这个意思）。如果再加一个符号列，同一个方向会出现两个真相：符号列说「加」，
//! 口径算出来是负数 —— 到底听谁的？所以方向**只**由口径决定：
//!
//! | 口径 | 值 | 用在 |
//! |------|----|------|
//! | `CREDIT_NET` | 贷方净额（贷 − 借） | 备抵科目计提：1602 累计折旧 / 1702 累计摊销 / 1801 长期待摊费用 |
//! | `DEBIT_NET` | 借方净额（借 − 贷） | 费用与损失：5603 财务费用 / 5111 投资收益 / 1606 固定资产清理（损失为正、收益为负） |
//! | `BALANCE_INCREASE` | 期末 − 期初 | 经营性**负债**增加（少付现金 ⇒ 加项） |
//! | `BALANCE_DECREASE` | 期初 − 期末 | 经营性**资产**减少（多收现金 ⇒ 加项） |
//!
//! 于是「存货增加」自动是负数（减项）、「处置有收益」自动是负数（收益以负号填列），
//! 报表出数代码不需要任何按行特判。
//!
//! # `MANUAL` 行：必须把口径字段留空（由服务端强制）
//!
//! 科目表里没有对应科目（递延所得税、公允价值变动）或需要会计判断的行只能手工填。
//! 这类行 `calc_basis` 与 `account_pattern` **必须都为空**：留着不参与计算的模式串
//! 就是「看起来配了、其实不用」的摆设（`docs/ERP-摆设字段口径.md`）。服务端在写入口
//! 拒掉（400），库层 `ck_biz_cashflow_adjust_shape` 再兜一层。
//!
//! # 配置写错的两种静默失败，都在读接口点名（不靠用户自己看）
//!
//! 1. **模式没命中任何科目** ⇒ 该行恒为 0，用户分不清「本期没发生」与「配错了」。
//!    每行返回 `matched_account_count` + `matched_accounts`（前 20 个）+ `warnings`。
//! 2. **同一科目被两行同时匹配** ⇒ 同一个调整被算两遍，报表「看起来很平」但数字是错的。
//!    列表响应顶层 `warnings` 点名科目与冲突行。完全相同「模式 + 口径」的重复配置
//!    在写入口直接 409 拒掉。
//!
//! # 为什么编码不可变、为什么没有 DELETE
//!
//! * **编码不可变**：`item_code` 是报表行在同一期间不同版本之间的身份（M1-16 落数、
//!   M1-17 单测都按它定位）。允许改码 = 历史数据与配置对不上，而且不会有任何报错。
//! * **没有物理删除**：调整项是报表结构的一部分，删掉一行 = 报表少一行，而且谁也不会
//!   发现少了。不用了请 `toggle` 停用 —— 停用行仍出现在列表里（可以改回来），
//!   未来的出表代码只取启用行。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_cashflow_adjust_item 是报表口径配置（V0416 无 dept_id / 归属人列），
//! biz_account 是全局科目表 —— 两者都是公司级口径，数据范围（本部门 / 本人）表达不出来。
//! 可见性由 business:erp:cashflow-adjust:list 与 :manage 两个码控制，无分页查询。
//! ```
//!
//! # 不碰的东西
//!
//! 不生成凭证、不进流程引擎、不注册 `DocKind`、不读写 `biz_cashflow_item` /
//! `biz_cashflow_worksheet` / `biz_voucher*` / `biz_report_item`。本模块是纯配置 CRUD。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

use crate::business::erp::report;

// ==================== 常量 ====================

/// 调整项配置表（`V0416`）。
pub const TABLE: &str = "biz_cashflow_adjust_item";

/// 科目表（取数自检 / 模式命中数）。
pub const ACCOUNT_TABLE: &str = "biz_account";

/// 读配置权限码（挂「现金流量表」页面 `…001018` 的按钮行，`V0416`）。
pub const PERM_LIST: &str = "business:erp:cashflow-adjust:list";
/// 改配置权限码（同上）。
pub const PERM_MANAGE: &str = "business:erp:cashflow-adjust:manage";
/// 本模块**新增**的权限码清单（供 `perm_menu_gate.py` 对账）。
pub const PERMISSIONS: [&str; 2] = [PERM_LIST, PERM_MANAGE];

/// 取数来源：按科目取数。
pub const SOURCE_ACCOUNT: &str = "ACCOUNT";
/// 取数来源：手工填列。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 全部取数来源。
pub const SOURCES: [&str; 2] = [SOURCE_ACCOUNT, SOURCE_MANUAL];

/// 口径：贷方净额（贷 − 借）。
pub const BASIS_CREDIT_NET: &str = "CREDIT_NET";
/// 口径：借方净额（借 − 贷）。
pub const BASIS_DEBIT_NET: &str = "DEBIT_NET";
/// 口径：余额增加额（期末 − 期初）。
pub const BASIS_BALANCE_INCREASE: &str = "BALANCE_INCREASE";
/// 口径：余额减少额（期初 − 期末）。
pub const BASIS_BALANCE_DECREASE: &str = "BALANCE_DECREASE";
/// 全部口径（顺序 = 字典展示顺序）。
pub const BASES: [&str; 4] = [
    BASIS_CREDIT_NET,
    BASIS_DEBIT_NET,
    BASIS_BALANCE_INCREASE,
    BASIS_BALANCE_DECREASE,
];

/// 科目类别（`biz_account.account_category` 取值；`@CATEGORY:` 语法只认这五个）。
pub const ACCOUNT_CATEGORIES: [&str; 5] = ["ASSET", "LIABILITY", "EQUITY", "COST", "PROFIT_LOSS"];

/// 固定行：净利润（起点，取利润表，**不是**配置行）。
pub const ANCHOR_NET_PROFIT: &str = "NET_PROFIT";
/// 固定行：经营活动产生的现金流量净额（小计 = 净利润 + Σ调整项）。
pub const ANCHOR_OPERATING_NET: &str = "OPERATING_NET";

/// 报表结构式（列表 / 字典接口都返回，前端不必各写一份文档）。
pub const LINE_FORMULA: &str = "净利润 ＋ Σ(启用的调整项) ＝ 经营活动产生的现金流量净额";

/// 编码最大长度（`VARCHAR(32)`）。
pub const MAX_CODE_LEN: usize = 32;
/// 名称最大长度（`VARCHAR(64)`）。
pub const MAX_NAME_CHARS: usize = 64;
/// 模式串最大长度（`VARCHAR(255)`）。
pub const MAX_PATTERN_LEN: usize = 255;
/// 单个调整项返回的「命中科目」样本上限。
pub const MATCH_SAMPLE: usize = 20;
/// `sort_order` 上限（防 9 位数字撑坏前端排序；10 / 15 / 25 这类中间值够用）。
pub const MAX_SORT_ORDER: i32 = 999_999;

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
        format!("间接法调整项不存在（id={id}）"),
    )
}

fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| invalid(format!("{label}「{raw}」不是合法的 UUID")))
}

// ==================== 出参 ====================

/// 科目摘要（命中样本）。
#[derive(Debug, Clone, Serialize)]
pub struct AccountBriefRes {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
}

/// 字典项。
#[derive(Debug, Clone, Serialize)]
pub struct DictRes {
    /// 值（存库的枚举字面量）。
    pub value: String,
    /// 中文名。
    pub label: String,
    /// 口径说明（前端直接展示，避免各处再抄一遍）。
    pub note: String,
}

/// 固定行（净利润 / 小计）：不是配置行，前端按报表形状渲染用。
#[derive(Debug, Clone, Serialize)]
pub struct AnchorRes {
    /// 行编码。
    pub code: String,
    /// 行名称。
    pub name: String,
    /// 角色：`START` 起点 / `SUBTOTAL` 小计。
    pub role: String,
    /// 取值说明。
    pub note: String,
}

/// 一个调整项（维护面视角：含停用行、命中自检、行内告警）。
#[derive(Debug, Clone, Serialize)]
pub struct AdjustItemRes {
    /// 主键。
    pub id: String,
    /// 调整项编码。
    pub item_code: String,
    /// 调整项名称（报表行原文）。
    pub item_name: String,
    /// 取数来源：`ACCOUNT` / `MANUAL`。
    pub source_type: String,
    /// 取数来源中文名。
    pub source_label: String,
    /// 取数口径（`MANUAL` 行为 `None`）。
    pub calc_basis: Option<String>,
    /// 取数口径中文名（`MANUAL` 行为 `None`）。
    pub basis_label: Option<String>,
    /// 科目匹配模式（`MANUAL` 行为 `None`）。
    pub account_pattern: Option<String>,
    /// 报表行顺序。
    pub sort_order: i32,
    /// 是否启用。
    pub is_active: bool,
    /// 模式命中的启用科目数（`MANUAL` 行为 `None`）。
    pub matched_account_count: Option<usize>,
    /// 命中科目样本（最多 [`MATCH_SAMPLE`] 个）。
    pub matched_accounts: Vec<AccountBriefRes>,
    /// 备注（取数口径说明）。
    pub remark: Option<String>,
    /// 更新时间（`yyyy-MM-dd HH:mm:ss`，前端直接显示）。
    pub updated_at: Option<String>,
    /// 行内告警（模式命中 0 个科目等；不阻断保存，但必须让用户看见）。
    pub warnings: Vec<String>,
}

/// 保存结果：改完的行 + 非阻断告警。
#[derive(Debug, Clone, Serialize)]
pub struct AdjustSaveRes {
    /// 保存后的行。
    pub item: AdjustItemRes,
    /// 非阻断告警。
    pub warnings: Vec<String>,
}

/// 列表出参。
#[derive(Debug, Clone, Serialize)]
pub struct AdjustListRes {
    /// 全部调整项（含停用行，按 `sort_order, item_code`）。
    pub items: Vec<AdjustItemRes>,
    /// 总行数。
    pub total: usize,
    /// 启用行数。
    pub active_total: usize,
    /// 两个固定行（净利润 / 小计）。
    pub anchors: Vec<AnchorRes>,
    /// 报表结构式。
    pub formula: String,
    /// 科目目录里启用科目的总数（命中数的分母，让用户知道「0 命中」有多严重）。
    pub account_total: usize,
    /// 跨行告警（无启用调整项 / 同一科目被两行命中 / 完全重复的口径）。
    pub warnings: Vec<String>,
}

/// 字典出参。
#[derive(Debug, Clone, Serialize)]
pub struct AdjustCatalogRes {
    /// 取数来源。
    pub source_types: Vec<DictRes>,
    /// 4 个取数口径。
    pub calc_bases: Vec<DictRes>,
    /// 科目匹配模式语法说明。
    pub pattern_tips: Vec<DictRes>,
    /// 两个固定行。
    pub anchors: Vec<AnchorRes>,
    /// 报表结构式。
    pub formula: String,
    /// 现有行数 / 启用行数。
    pub total: usize,
    /// 现有行数 / 启用行数。
    pub active_total: usize,
}

// ==================== 入参 ====================

/// 列表参数（当前没有筛选：调整项最多几十行，全量返回最省事）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AdjustListQuery {
    /// 传 `true` 时只返回启用行（默认全量，含停用）。
    pub active_only: Option<bool>,
}

/// 新增 / 编辑共用请求体（编辑时 `item_code` 必须与库里一致或省略）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AdjustUpsertReq {
    /// 调整项编码（编辑时不可变）。
    pub item_code: Option<String>,
    /// 调整项名称。
    pub item_name: Option<String>,
    /// 取数来源：`ACCOUNT` / `MANUAL`（省略 = `ACCOUNT`）。
    pub source_type: Option<String>,
    /// 取数口径（`ACCOUNT` 必填，`MANUAL` 不许填）。
    pub calc_basis: Option<String>,
    /// 科目匹配模式（`ACCOUNT` 必填，`MANUAL` 不许填）。
    pub account_pattern: Option<String>,
    /// 报表行顺序（省略 = 新建排在末尾 / 编辑保留原值）。
    pub sort_order: Option<i32>,
    /// 是否启用（省略 = 新建启用 / 编辑保留原值）。
    pub is_active: Option<bool>,
    /// 备注。
    pub remark: Option<String>,
}

/// 启停请求体。
#[derive(Debug, Deserialize)]
pub struct AdjustToggleReq {
    /// 目标状态。
    pub is_active: bool,
}

// ==================== 纯逻辑 ====================

/// 去空白；空串归一成 `None`。
fn trimmed(raw: &Option<String>) -> Option<String> {
    raw.as_ref()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 调整项编码：大写字母开头，只含大写字母 / 数字 / 下划线，`3..=32`。
fn normalize_item_code(raw: &str) -> std::result::Result<String, String> {
    let code = raw.trim().to_ascii_uppercase();
    if code.is_empty() {
        return Err("调整项编码必填（例：ADJ_DEPRECIATION）".to_string());
    }
    if code.len() > MAX_CODE_LEN {
        return Err(format!("调整项编码「{code}」超过 {MAX_CODE_LEN} 个字符"));
    }
    let first = code.chars().next().unwrap_or('_');
    if !first.is_ascii_uppercase() {
        return Err(format!("调整项编码「{code}」必须大写字母开头"));
    }
    if !code
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!("调整项编码「{code}」只能包含大写字母 / 数字 / 下划线"));
    }
    Ok(code)
}

/// 调整项名称：非空、≤64 **字符**（`VARCHAR(64)` 按字符计，中文不占 3 个）。
fn normalize_item_name(raw: &str) -> std::result::Result<String, String> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err("调整项名称必填（报表行原文，例：固定资产折旧）".to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!(
            "调整项名称「{name}」有 {chars} 个字符，超过 {MAX_NAME_CHARS}（VARCHAR(64)）"
        ));
    }
    Ok(name)
}

/// 取数来源（大小写不敏感；只认 `ACCOUNT` / `MANUAL`）。
fn normalize_source_type(raw: &str) -> std::result::Result<String, String> {
    let value = raw.trim().to_ascii_uppercase();
    if SOURCES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(format!(
            "取数来源「{raw}」不合法（可选：{SOURCE_ACCOUNT} 科目取数 / {SOURCE_MANUAL} 手工填列）"
        ))
    }
}

/// 取数口径（大小写不敏感；只认 [`BASES`] 四个）。
fn normalize_calc_basis(raw: &str) -> std::result::Result<String, String> {
    let value = raw.trim().to_ascii_uppercase();
    if BASES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(format!("取数口径「{raw}」不合法（可选：{}）", BASES.join(" / ")))
    }
}

/// 科目匹配模式语法：`1001,1002` / `2221`（含下级）/ `1*` `1%`（前缀）/ `@CATEGORY:类别`。
///
/// 与 `report::report_item_admin::validate_pattern` **同一套文法**（`report::match_account`
/// 的解析侧）。这里是独立实现（那个是私有函数），改文法时两处必须一起改：
/// `http` 层测试用 `report::match_account` 交叉验证过「能过校验的模式一定能被解析」。
fn validate_pattern(pattern: &str) -> std::result::Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("科目匹配模式必填（ACCOUNT 行要有取数口径）".to_string());
    }
    if pattern.chars().count() > MAX_PATTERN_LEN {
        return Err(format!("科目匹配模式超过 {MAX_PATTERN_LEN} 个字符（VARCHAR(255)）"));
    }
    for raw in pattern.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            return Err("科目匹配模式里有空项（逗号之间不能为空）".to_string());
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
        let (prefix, is_prefix) = match token.strip_suffix('*').or_else(|| token.strip_suffix('%')) {
            Some(prefix) => (prefix, true),
            None => (token, false),
        };
        if prefix.is_empty() {
            return Err(format!("科目匹配模式「{token}」的通配前缀为空"));
        }
        if is_prefix && (prefix.ends_with('*') || prefix.ends_with('%')) {
            return Err(format!("科目匹配模式「{token}」只允许一个通配符"));
        }
        if !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(format!(
                "科目匹配模式「{token}」含非法字符（科目编码只允许字母 / 数字 / . / _ / -）"
            ));
        }
    }
    Ok(())
}

/// 行形状：`ACCOUNT` 必须有口径 + 模式；`MANUAL` 两者都必须为空。
///
/// 返回归一化后的 `(calc_basis, account_pattern)`，直接可写库。
fn normalize_shape(
    source_type: &str,
    calc_basis: Option<&str>,
    account_pattern: Option<&str>,
) -> std::result::Result<(Option<String>, Option<String>), String> {
    let basis = calc_basis.map(str::trim).filter(|text| !text.is_empty());
    let pattern = account_pattern.map(str::trim).filter(|text| !text.is_empty());
    if source_type == SOURCE_MANUAL {
        if basis.is_some() || pattern.is_some() {
            return Err(
                "手工填列（MANUAL）行不能带取数口径 / 科目模式：它们不参与计算，留着就是「看起来配了、其实不用」的摆设"
                    .to_string(),
            );
        }
        return Ok((None, None));
    }
    let Some(basis) = basis else {
        return Err(format!(
            "科目取数（ACCOUNT）行必须给取数口径（可选：{}）",
            BASES.join(" / ")
        ));
    };
    let basis = normalize_calc_basis(basis)?;
    let Some(pattern) = pattern else {
        return Err("科目取数（ACCOUNT）行必须给科目匹配模式".to_string());
    };
    validate_pattern(pattern)?;
    Ok((Some(basis), Some(pattern.to_string())))
}

/// 取数来源中文名。
pub fn source_label(code: &str) -> &'static str {
    match code {
        SOURCE_ACCOUNT => "科目取数",
        SOURCE_MANUAL => "手工填列",
        _ => "未知来源",
    }
}

/// 取数口径中文名。
pub fn basis_label(code: &str) -> Option<&'static str> {
    match code {
        BASIS_CREDIT_NET => Some("贷方净额（贷 − 借）"),
        BASIS_DEBIT_NET => Some("借方净额（借 − 贷）"),
        BASIS_BALANCE_INCREASE => Some("余额增加额（期末 − 期初）"),
        BASIS_BALANCE_DECREASE => Some("余额减少额（期初 − 期末）"),
        _ => None,
    }
}

/// 两个固定行（净利润 / 小计）。**不是**配置行，只用于渲染报表形状。
pub fn anchors() -> Vec<AnchorRes> {
    vec![
        AnchorRes {
            code: ANCHOR_NET_PROFIT.to_string(),
            name: "净利润".to_string(),
            role: "START".to_string(),
            note: "起点取利润表（ERP-R2）净利润；不可配置 —— 手填净利润会造出与利润表对不上的报表".to_string(),
        },
        AnchorRes {
            code: ANCHOR_OPERATING_NET.to_string(),
            name: "经营活动产生的现金流量净额".to_string(),
            role: "SUBTOTAL".to_string(),
            note: "= 净利润 + Σ(启用的调整项)；由出表代码算，不落配置".to_string(),
        },
    ]
}

/// 科目目录里的一行。
#[derive(Debug, Clone)]
pub struct AccountOption {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
}

/// 一个模式命中的科目（顺序同科目表：编码升序）。
fn matched_accounts<'a>(pattern: &str, accounts: &'a [AccountOption]) -> Vec<&'a AccountOption> {
    accounts
        .iter()
        .filter(|account| report::match_account(pattern, &account.account_code, &account.account_category))
        .collect()
}

/// 一个调整项（已落库的行）。
#[derive(Debug, Clone)]
pub struct AdjustRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub item_code: String,
    /// 名称。
    pub item_name: String,
    /// 取数来源。
    pub source_type: String,
    /// 取数口径。
    pub calc_basis: Option<String>,
    /// 科目模式。
    pub account_pattern: Option<String>,
    /// 顺序。
    pub sort_order: i32,
    /// 启用位。
    pub is_active: bool,
    /// 备注。
    pub remark: Option<String>,
    /// 更新时间文本。
    pub updated_at: Option<String>,
}

/// 写入用字段集。
#[derive(Debug, Clone)]
pub struct AdjustWrite {
    /// 编码（新建必填；编辑时与库里一致）。
    pub item_code: String,
    /// 名称。
    pub item_name: String,
    /// 取数来源。
    pub source_type: String,
    /// 取数口径。
    pub calc_basis: Option<String>,
    /// 科目模式。
    pub account_pattern: Option<String>,
    /// 顺序。
    pub sort_order: i32,
    /// 启用位。
    pub is_active: bool,
    /// 备注。
    pub remark: Option<String>,
}

/// 行级自检：命中数 + 告警（`MANUAL` 行没有命中概念）。
pub fn row_audit(row: &AdjustRow, accounts: &[AccountOption]) -> (Option<usize>, Vec<String>) {
    let Some(pattern) = row.account_pattern.as_deref() else {
        return (None, Vec::new());
    };
    let hits = matched_accounts(pattern, accounts);
    let mut warnings = Vec::new();
    if hits.is_empty() {
        warnings.push(format!(
            "取数模式「{pattern}」当前没有命中任何启用科目 —— 该行会恒为 0。若科目表里确实还没有这组科目（例如 1231 坏账准备 / 1471 存货跌价准备），属正常；建好科目即自动生效"
        ));
    }
    (Some(hits.len()), warnings)
}

/// 跨行自检：无启用调整项 / 完全重复的口径 / 同一科目被两行命中（会算两遍）。
pub fn audit_rows(rows: &[AdjustRow], accounts: &[AccountOption]) -> Vec<String> {
    let mut warnings = Vec::new();
    let active: Vec<&AdjustRow> = rows.iter().filter(|row| row.is_active).collect();
    if active.is_empty() {
        warnings.push("当前没有任何启用的调整项 —— 间接法只会得到净利润本身，等于没有调整".to_string());
        return warnings;
    }

    // ① 完全相同「模式 + 口径」的重复配置：同一批科目会被算两遍。
    let mut seen: std::collections::HashMap<(String, String), Vec<&str>> = std::collections::HashMap::new();
    for row in &active {
        if let (Some(basis), Some(pattern)) = (row.calc_basis.as_deref(), row.account_pattern.as_deref()) {
            let key = (basis.to_string(), pattern.replace(' ', ""));
            seen.entry(key).or_default().push(row.item_code.as_str());
        }
    }
    let mut duplicates: Vec<(String, String, Vec<&str>)> = seen
        .into_iter()
        .filter(|(_, codes)| codes.len() > 1)
        .map(|((basis, pattern), codes)| (basis, pattern, codes))
        .collect();
    duplicates.sort_by(|a, b| a.1.cmp(&b.1));
    for (basis, pattern, codes) in duplicates {
        warnings.push(format!(
            "取数口径完全重复：{} 行（{}）都按「{basis}」匹配「{pattern}」—— 同一批科目会被算两遍",
            codes.join(" / "),
            codes.len()
        ));
    }

    // ② 同一科目被两行命中（模式部分重叠）：报表会「看起来平、实际多算」。
    let mut owners: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for account in accounts {
        let mut hits: Vec<&str> = active
            .iter()
            .filter(|row| {
                row.account_pattern.as_deref().is_some_and(|pattern| {
                    report::match_account(pattern, &account.account_code, &account.account_category)
                })
            })
            .map(|row| row.item_code.as_str())
            .collect();
        hits.sort_unstable();
        if hits.len() > 1 {
            owners.insert(account.account_code.as_str(), hits);
        }
    }
    let overlap: Vec<(&str, Vec<&str>)> = owners.into_iter().collect();
    let shown = overlap.len().min(5);
    for (code, codes) in overlap.iter().take(shown) {
        warnings.push(format!(
            "科目 {code} 同时被 {} 行匹配（{}）—— 会重复计入调整项，请检查模式是否重叠",
            codes.len(),
            codes.join(" / ")
        ));
    }
    if overlap.len() > shown {
        warnings.push(format!(
            "另有 {} 个科目存在同样的重叠匹配（共 {} 个），此处只列前 {shown} 个",
            overlap.len() - shown,
            overlap.len()
        ));
    }
    warnings
}

/// 行 → 出参。
fn item_res(row: &AdjustRow, accounts: &[AccountOption]) -> AdjustItemRes {
    let (matched_count, warnings) = row_audit(row, accounts);
    let sample: Vec<AccountBriefRes> = row
        .account_pattern
        .as_deref()
        .map(|pattern| {
            matched_accounts(pattern, accounts)
                .into_iter()
                .take(MATCH_SAMPLE)
                .map(|account| AccountBriefRes {
                    account_code: account.account_code.clone(),
                    account_name: account.account_name.clone(),
                    account_category: account.account_category.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    AdjustItemRes {
        id: row.id.to_string(),
        item_code: row.item_code.clone(),
        item_name: row.item_name.clone(),
        source_type: row.source_type.clone(),
        source_label: source_label(&row.source_type).to_string(),
        calc_basis: row.calc_basis.clone(),
        basis_label: row.calc_basis.as_deref().and_then(basis_label).map(str::to_string),
        account_pattern: row.account_pattern.clone(),
        sort_order: row.sort_order,
        is_active: row.is_active,
        matched_account_count: matched_count,
        matched_accounts: sample,
        remark: row.remark.clone(),
        updated_at: row.updated_at.clone(),
        warnings,
    }
}

/// 读全量 + 科目目录，拼列表出参（`active_only` 只影响 `items`，告警仍按全量算）。
pub fn compose_list(rows: &[AdjustRow], accounts: &[AccountOption], active_only: bool) -> AdjustListRes {
    let warnings = audit_rows(rows, accounts);
    let total = rows.len();
    let active_total = rows.iter().filter(|row| row.is_active).count();
    let items: Vec<AdjustItemRes> = rows
        .iter()
        .filter(|row| !active_only || row.is_active)
        .map(|row| item_res(row, accounts))
        .collect();
    AdjustListRes {
        items,
        total,
        active_total,
        anchors: anchors(),
        formula: LINE_FORMULA.to_string(),
        account_total: accounts.len(),
        warnings,
    }
}

/// 字典出参（不碰库，纯常量）。
pub fn compose_catalog(total: usize, active_total: usize) -> AdjustCatalogRes {
    AdjustCatalogRes {
        source_types: vec![
            DictRes {
                value: SOURCE_ACCOUNT.to_string(),
                label: "科目取数".to_string(),
                note: "按科目匹配模式自动取数（期末重算，不需要人工填）".to_string(),
            },
            DictRes {
                value: SOURCE_MANUAL.to_string(),
                label: "手工填列".to_string(),
                note: "科目表里没有对应科目、或需要会计判断的行；必须不带口径与模式".to_string(),
            },
        ],
        calc_bases: BASES
            .iter()
            .map(|basis| DictRes {
                value: (*basis).to_string(),
                label: basis_label(basis).unwrap_or(basis).to_string(),
                note: match *basis {
                    BASIS_CREDIT_NET => "贷方发生额 − 借方发生额；备抵科目计提（累计折旧 / 累计摊销 / 减值准备）用这个",
                    BASIS_DEBIT_NET => {
                        "借方发生额 − 贷方发生额；费用与损失类用这个（损失为正、收益为负，正好是「收益以负号填列」）"
                    }
                    BASIS_BALANCE_INCREASE => "期末余额 − 期初余额；经营性负债增加（少付现金）用这个",
                    _ => "期初余额 − 期末余额；经营性资产减少（多收现金）用这个",
                }
                .to_string(),
            })
            .collect(),
        pattern_tips: vec![
            DictRes {
                value: "1001,1002".to_string(),
                label: "科目编码清单".to_string(),
                note: "精确匹配列出的每个编码".to_string(),
            },
            DictRes {
                value: "2221".to_string(),
                label: "科目 + 全部下级".to_string(),
                note: "命中 2221 及其下级 2221.01 / 2221.02 …".to_string(),
            },
            DictRes {
                value: "14*".to_string(),
                label: "前缀通配".to_string(),
                note: "`*` 或 `%` 结尾表示按编码前缀匹配（14* = 1401…1408）".to_string(),
            },
            DictRes {
                value: "@CATEGORY:PROFIT_LOSS".to_string(),
                label: "按科目类别".to_string(),
                note: format!("可选类别：{}", ACCOUNT_CATEGORIES.join(" / ")),
            },
        ],
        anchors: anchors(),
        formula: LINE_FORMULA.to_string(),
        total,
        active_total,
    }
}

// ==================== 数据层 ====================

/// 可选文本参数：`None` → 空串，SQL 侧 `NULLIF(?::text,'')` 还原 NULL。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 全量调整项（含停用行，按 `sort_order, item_code`）。
pub async fn list_rows(ex: &impl SqlExecutor) -> DbResult<Vec<AdjustRow>> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列），一次取全量。
    let sql = format!(
        "SELECT id, item_code, item_name, source_type, calc_basis, account_pattern, \
                sort_order, is_active, remark, \
                to_char(updated_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI:SS') AS updated_at_text \
           FROM {TABLE} ORDER BY sort_order, item_code"
    );
    let rows = ex.query_sql(sql, Vec::new()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(AdjustRow {
            id: row.get_typed("id")?,
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            source_type: row.get_typed("source_type")?,
            calc_basis: row.get_typed::<Option<String>>("calc_basis").unwrap_or_default(),
            account_pattern: row.get_typed::<Option<String>>("account_pattern").unwrap_or_default(),
            sort_order: row.get_typed("sort_order")?,
            is_active: row.get_typed::<i16>("is_active")? == 1,
            remark: row.get_typed::<Option<String>>("remark").unwrap_or_default(),
            updated_at: row.get_typed::<Option<String>>("updated_at_text").unwrap_or_default(),
        });
    }
    Ok(out)
}

/// 单行（按主键）。
pub async fn find_row(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AdjustRow>> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列）。
    let sql = format!(
        "SELECT id, item_code, item_name, source_type, calc_basis, account_pattern, \
                sort_order, is_active, remark, \
                to_char(updated_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI:SS') AS updated_at_text \
           FROM {TABLE} WHERE id = ?::uuid"
    );
    let rows = ex.query_sql(sql, vec![Value::from(&id)]).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(AdjustRow {
        id: row.get_typed("id")?,
        item_code: row.get_typed("item_code")?,
        item_name: row.get_typed("item_name")?,
        source_type: row.get_typed("source_type")?,
        calc_basis: row.get_typed::<Option<String>>("calc_basis").unwrap_or_default(),
        account_pattern: row.get_typed::<Option<String>>("account_pattern").unwrap_or_default(),
        sort_order: row.get_typed("sort_order")?,
        is_active: row.get_typed::<i16>("is_active")? == 1,
        remark: row.get_typed::<Option<String>>("remark").unwrap_or_default(),
        updated_at: row.get_typed::<Option<String>>("updated_at_text").unwrap_or_default(),
    }))
}

/// 当前最大 `sort_order`（新建默认 max + 10）。
pub async fn max_sort_order(ex: &impl SqlExecutor) -> DbResult<i32> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列）。
    let rows = ex
        .query_sql(
            format!("SELECT COALESCE(MAX(sort_order), 0) AS max_sort FROM {TABLE}"),
            Vec::new(),
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(row.get_typed("max_sort")?),
        None => Ok(0),
    }
}

/// 启用中的科目目录（模式命中自检用；按编码升序）。
pub async fn load_accounts(ex: &impl SqlExecutor) -> DbResult<Vec<AccountOption>> {
    // SCOPE-EXEMPT: biz_account 是全局科目表（无归属列）。
    let sql = format!(
        "SELECT account_code, account_name, account_category \
           FROM {ACCOUNT_TABLE} WHERE deleted_at IS NULL AND status = 1 \
          ORDER BY account_code"
    );
    let rows = ex.query_sql(sql, Vec::new()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(AccountOption {
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
        });
    }
    Ok(out)
}

/// 新增一行。
pub async fn insert_row(ex: &impl SqlExecutor, id: Uuid, spec: &AdjustWrite) -> DbResult<()> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列）。
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, item_code, item_name, source_type, calc_basis, account_pattern, \
                 sort_order, is_active, remark) \
             VALUES (?::uuid, ?::text, ?::text, ?::text, NULLIF(?::text,''), NULLIF(?::text,''), \
                     ?::int, ?::int, NULLIF(?::text,''))"
        ),
        vec![
            Value::from(&id),
            Value::Text(spec.item_code.clone()),
            Value::Text(spec.item_name.clone()),
            Value::Text(spec.source_type.clone()),
            opt_text(spec.calc_basis.as_deref()),
            opt_text(spec.account_pattern.as_deref()),
            Value::from(spec.sort_order),
            Value::from(if spec.is_active { 1i32 } else { 0i32 }),
            opt_text(spec.remark.as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 编辑一行（`item_code` 不可变，故不入 SET）。
pub async fn update_row(ex: &impl SqlExecutor, id: Uuid, spec: &AdjustWrite) -> DbResult<u64> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET item_name = ?::text, source_type = ?::text, \
                    calc_basis = NULLIF(?::text,''), account_pattern = NULLIF(?::text,''), \
                    sort_order = ?::int, is_active = ?::int, remark = NULLIF(?::text,''), \
                    updated_at = now() \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(spec.item_name.clone()),
            Value::Text(spec.source_type.clone()),
            opt_text(spec.calc_basis.as_deref()),
            opt_text(spec.account_pattern.as_deref()),
            Value::from(spec.sort_order),
            Value::from(if spec.is_active { 1i32 } else { 0i32 }),
            opt_text(spec.remark.as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 启停。
pub async fn set_active(ex: &impl SqlExecutor, id: Uuid, active: bool) -> DbResult<u64> {
    // SCOPE-EXEMPT: 报表口径配置表（无归属列）。
    ex.exec_sql(
        format!("UPDATE {TABLE} SET is_active = ?::int, updated_at = now() WHERE id = ?::uuid"),
        vec![Value::from(if active { 1i32 } else { 0i32 }), Value::from(&id)],
    )
    .await
}

// ==================== 业务编排 ====================

/// 口径重复闸：同一「模式 + 口径」在**另一条启用行**上是否已存在。
///
/// 同一批科目被两行按同一口径取数 = 同一笔调整算两遍，报表「看起来很平、数字是错的」。
/// 只拦**完全相同**的模式串（部分重叠交给列表 `warnings` 点名，因为重叠有时是刻意的
/// 分层配置，不该一律拦死）。
fn duplicate_basis_conflict(
    rows: &[AdjustRow],
    source_type: &str,
    calc_basis: Option<&str>,
    account_pattern: Option<&str>,
    exclude: Option<Uuid>,
) -> Option<String> {
    if source_type != SOURCE_ACCOUNT {
        return None;
    }
    let (basis, pattern) = (calc_basis?, account_pattern?);
    let normalized = pattern.replace(' ', "");
    rows.iter()
        .filter(|row| Some(row.id) != exclude)
        .find(|row| {
            row.is_active
                && row.calc_basis.as_deref() == Some(basis)
                && row
                    .account_pattern
                    .as_deref()
                    .is_some_and(|other| other.replace(' ', "") == normalized)
        })
        .map(|row| {
            format!(
                "取数口径重复：「{}」已经按「{basis}」匹配「{pattern}」—— 再加一行会让同一批科目被算两遍",
                row.item_code
            )
        })
}

/// 读「列表 + 科目目录」（五个接口共用）。
async fn load_state() -> std::result::Result<(Vec<AdjustRow>, Vec<AccountOption>), ApiError> {
    let rows = list_rows(db())
        .await
        .map_err(|error| utils::db_error(error, "查询间接法调整项"))?;
    let accounts = load_accounts(db())
        .await
        .map_err(|error| utils::db_error(error, "查询科目目录"))?;
    Ok((rows, accounts))
}

/// 读回一行并拼出参（写接口返回用）。
async fn reload_item(id: Uuid) -> std::result::Result<AdjustItemRes, ApiError> {
    let (rows, accounts) = load_state().await?;
    let row = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| not_found(&id.to_string()))?;
    Ok(item_res(row, &accounts))
}

/// 列表（含停用行 + 每行取数自检 + 跨行告警）。
pub async fn list_adjust_items(active_only: bool) -> std::result::Result<AdjustListRes, ApiError> {
    let (rows, accounts) = load_state().await?;
    Ok(compose_list(&rows, &accounts, active_only))
}

/// 新增（编码唯一；ACCOUNT 行必须有口径 + 模式；MANUAL 行两者必须为空）。
pub async fn create_adjust_item(req: &AdjustUpsertReq) -> std::result::Result<AdjustSaveRes, ApiError> {
    let item_code = normalize_item_code(req.item_code.as_deref().unwrap_or("")).map_err(invalid)?;
    let item_name = normalize_item_name(req.item_name.as_deref().unwrap_or("")).map_err(invalid)?;
    let source_type = normalize_source_type(req.source_type.as_deref().unwrap_or(SOURCE_ACCOUNT)).map_err(invalid)?;
    let (calc_basis, account_pattern) =
        normalize_shape(&source_type, req.calc_basis.as_deref(), req.account_pattern.as_deref()).map_err(invalid)?;

    let (rows, _accounts) = load_state().await?;
    if rows.iter().any(|row| row.item_code == item_code) {
        return Err(conflict(format!(
            "调整项编码「{item_code}」已存在（编码是报表行的身份，M1-16 落数按它定位）"
        )));
    }
    if let Some(message) = duplicate_basis_conflict(
        &rows,
        &source_type,
        calc_basis.as_deref(),
        account_pattern.as_deref(),
        None,
    ) {
        return Err(conflict(message));
    }
    let sort_order = match req.sort_order {
        Some(value) => value,
        None => {
            max_sort_order(db())
                .await
                .map_err(|error| utils::db_error(error, "查询调整项最大排序号"))?
                + 10
        }
    };
    if !(0..=MAX_SORT_ORDER).contains(&sort_order) {
        return Err(invalid(format!("排序号 {sort_order} 超出范围（0 ~ {MAX_SORT_ORDER}）")));
    }

    let spec = AdjustWrite {
        item_code: item_code.clone(),
        item_name: item_name.clone(),
        source_type: source_type.clone(),
        calc_basis: calc_basis.clone(),
        account_pattern: account_pattern.clone(),
        sort_order,
        is_active: req.is_active.unwrap_or(true),
        remark: trimmed(&req.remark),
    };
    let id = Uuid::now_v7();
    insert_row(db(), id, &spec)
        .await
        .map_err(|error| utils::db_error(error, "新增间接法调整项"))?;
    tracing::info!(%id, %item_code, %source_type, "间接法调整项已新增");

    let item = reload_item(id).await?;
    let warnings = item.warnings.clone();
    Ok(AdjustSaveRes { item, warnings })
}

/// 编辑（编码不可变；改名 / 改口径 / 改顺序 / 启停都可）。
pub async fn update_adjust_item(id: &str, req: &AdjustUpsertReq) -> std::result::Result<AdjustSaveRes, ApiError> {
    let id = parse_uuid_field(id, "id")?;
    let (rows, _accounts) = load_state().await?;
    let existing = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| not_found(&id.to_string()))?;

    if let Some(raw) = req.item_code.as_deref() {
        let code = normalize_item_code(raw).map_err(invalid)?;
        if code != existing.item_code {
            return Err(invalid(format!(
                "调整项编码不可修改（现在是「{}」，请求是「{code}」）：编码是报表行的身份，改码会让已落库的数据与配置对不上且不会报错",
                existing.item_code
            )));
        }
    }
    let item_name = match req.item_name.as_deref() {
        Some(raw) => normalize_item_name(raw).map_err(invalid)?,
        None => existing.item_name.clone(),
    };
    let source_type = match req.source_type.as_deref() {
        Some(raw) => normalize_source_type(raw).map_err(invalid)?,
        None => existing.source_type.clone(),
    };
    // 省略的字段按「保留原值」处理，但形状要整行重算：把 ACCOUNT 行改成 MANUAL 时
    // 旧的口径 / 模式必须一起清掉（否则库层 CHECK 直接拒，报错信息远不如这里清楚）。
    let basis_in = match req.calc_basis.as_deref() {
        Some(raw) => Some(raw),
        None => existing.calc_basis.as_deref(),
    };
    let pattern_in = match req.account_pattern.as_deref() {
        Some(raw) => Some(raw),
        None => existing.account_pattern.as_deref(),
    };
    let (calc_basis, account_pattern) = normalize_shape(&source_type, basis_in, pattern_in).map_err(invalid)?;

    if let Some(message) = duplicate_basis_conflict(
        &rows,
        &source_type,
        calc_basis.as_deref(),
        account_pattern.as_deref(),
        Some(id),
    ) {
        return Err(conflict(message));
    }
    let sort_order = req.sort_order.unwrap_or(existing.sort_order);
    if !(0..=MAX_SORT_ORDER).contains(&sort_order) {
        return Err(invalid(format!("排序号 {sort_order} 超出范围（0 ~ {MAX_SORT_ORDER}）")));
    }
    let spec = AdjustWrite {
        item_code: existing.item_code.clone(),
        item_name,
        source_type: source_type.clone(),
        calc_basis,
        account_pattern,
        sort_order,
        is_active: req.is_active.unwrap_or(existing.is_active),
        remark: match req.remark.as_ref() {
            // 传空串 = 显式清空备注；不传 = 保留原值
            Some(_) => trimmed(&req.remark),
            None => existing.remark.clone(),
        },
    };
    let affected = update_row(db(), id, &spec)
        .await
        .map_err(|error| utils::db_error(error, "修改间接法调整项"))?;
    if affected == 0 {
        return Err(not_found(&id.to_string()));
    }
    tracing::info!(%id, item_code = %spec.item_code, "间接法调整项已修改");

    let item = reload_item(id).await?;
    let warnings = item.warnings.clone();
    Ok(AdjustSaveRes { item, warnings })
}

/// 启停（停用行仍出现在列表里，可以改回来）。
pub async fn toggle_adjust_item(id: &str, active: bool) -> std::result::Result<AdjustItemRes, ApiError> {
    let id = parse_uuid_field(id, "id")?;
    let existing = find_row(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询间接法调整项"))?
        .ok_or_else(|| not_found(&id.to_string()))?;
    if existing.is_active != active {
        let affected = set_active(db(), id, active)
            .await
            .map_err(|error| utils::db_error(error, "启停间接法调整项"))?;
        if affected == 0 {
            return Err(not_found(&id.to_string()));
        }
        tracing::info!(%id, item_code = %existing.item_code, active, "间接法调整项启停");
    }
    reload_item(id).await
}

// ==================== 接口 ====================

/// 全部调整项（含停用）+ 每行取数自检 + 跨行告警。
#[nik::permission(
    path = "/business/erp/report/cash-flow/adjust-item/list",
    method = "GET",
    permission = "business:erp:cashflow-adjust:list"
)]
#[nik::get("/business/erp/report/cash-flow/adjust-item/list")]
pub async fn adjust_item_list_api(Query(query): Query<AdjustListQuery>) -> ResResult<AdjustListRes> {
    Ok(Res::ok(list_adjust_items(query.active_only.unwrap_or(false)).await?))
}

/// 字典：取数来源 / 4 个口径 / 模式语法 / 两个固定行。
#[nik::permission(
    path = "/business/erp/report/cash-flow/adjust-item/catalog",
    method = "GET",
    permission = "business:erp:cashflow-adjust:list"
)]
#[nik::get("/business/erp/report/cash-flow/adjust-item/catalog")]
pub async fn adjust_item_catalog_api() -> ResResult<AdjustCatalogRes> {
    let (rows, _accounts) = load_state().await?;
    let active_total = rows.iter().filter(|row| row.is_active).count();
    Ok(Res::ok(compose_catalog(rows.len(), active_total)))
}

/// 新增调整项。
#[nik::permission(
    path = "/business/erp/report/cash-flow/adjust-item",
    method = "POST",
    permission = "business:erp:cashflow-adjust:manage"
)]
#[nik::post("/business/erp/report/cash-flow/adjust-item")]
pub async fn adjust_item_create_api(Json(req): Json<AdjustUpsertReq>) -> ResResult<AdjustSaveRes> {
    Ok(Res::ok(create_adjust_item(&req).await?))
}

/// 编辑调整项（编码不可变）。
#[nik::permission(
    path = "/business/erp/report/cash-flow/adjust-item/{id}",
    method = "PUT",
    permission = "business:erp:cashflow-adjust:manage"
)]
#[nik::put("/business/erp/report/cash-flow/adjust-item/{id}")]
pub async fn adjust_item_update_api(
    Path(id): Path<String>,
    Json(req): Json<AdjustUpsertReq>,
) -> ResResult<AdjustSaveRes> {
    Ok(Res::ok(update_adjust_item(&id, &req).await?))
}

/// 启停调整项。
#[nik::permission(
    path = "/business/erp/report/cash-flow/adjust-item/{id}/toggle",
    method = "PUT",
    permission = "business:erp:cashflow-adjust:manage"
)]
#[nik::put("/business/erp/report/cash-flow/adjust-item/{id}/toggle")]
pub async fn adjust_item_toggle_api(
    Path(id): Path<String>,
    Json(req): Json<AdjustToggleReq>,
) -> ResResult<AdjustItemRes> {
    Ok(Res::ok(toggle_adjust_item(&id, req.is_active).await?))
}

// ==================== 单测（纯逻辑 + 真库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 本卡自建用例的编码前缀（`reset()` 按它清场，不碰 12 行种子）。
    const SCRATCH: &str = "M115_";

    fn dec_account(code: &str, name: &str, category: &str) -> AccountOption {
        AccountOption {
            account_code: code.to_string(),
            account_name: name.to_string(),
            account_category: category.to_string(),
        }
    }

    fn toy_accounts() -> Vec<AccountOption> {
        vec![
            dec_account("1602", "累计折旧", "ASSET"),
            dec_account("1603", "固定资产减值准备", "ASSET"),
            dec_account("2202", "应付账款", "LIABILITY"),
            dec_account("2203", "预收账款", "LIABILITY"),
            dec_account("5603", "财务费用", "PROFIT_LOSS"),
        ]
    }

    fn row(code: &str, basis: &str, pattern: &str, active: bool) -> AdjustRow {
        AdjustRow {
            id: Uuid::now_v7(),
            item_code: code.to_string(),
            item_name: code.to_string(),
            source_type: SOURCE_ACCOUNT.to_string(),
            calc_basis: Some(basis.to_string()),
            account_pattern: Some(pattern.to_string()),
            sort_order: 10,
            is_active: active,
            remark: None,
            updated_at: None,
        }
    }

    // ---------- 纯逻辑 ----------

    /// ① 编码：归一化成大写、拒非法字符 / 超长 / 非字母开头（编码是落数身份，形状必须死）。
    #[test]
    fn item_code_shape_is_strict() {
        assert_eq!(normalize_item_code(" adj_x1 ").expect("合法"), "ADJ_X1");
        for bad in ["", "1ADJ", "ADJ-X", "ADJ X", "调整项"] {
            assert!(normalize_item_code(bad).is_err(), "「{bad}」必须被拒");
        }
        assert!(normalize_item_code(&"A".repeat(33)).is_err(), "33 字符必须被拒");
        assert!(normalize_item_code(&format!("A{}", "1".repeat(31))).is_ok());
    }

    /// ② 名称：非空、≤64 **字符**（中文按字符算，不按字节）。
    #[test]
    fn item_name_counts_chars_not_bytes() {
        assert_eq!(normalize_item_name(" 固定资产折旧 ").expect("合法"), "固定资产折旧");
        assert!(normalize_item_name("   ").is_err());
        assert!(normalize_item_name(&"折".repeat(64)).is_ok(), "64 个汉字合法");
        assert!(normalize_item_name(&"折".repeat(65)).is_err(), "65 个汉字必须被拒");
    }

    /// ③ 来源 / 口径：只认枚举字面量（写错的枚举永远取 0，必须入口拒）。
    #[test]
    fn enums_are_closed() {
        assert_eq!(normalize_source_type("account").expect("合法"), SOURCE_ACCOUNT);
        assert_eq!(normalize_source_type(" Manual ").expect("合法"), SOURCE_MANUAL);
        assert!(normalize_source_type("ACCOUNTING").is_err());
        assert_eq!(
            normalize_calc_basis("balance_decrease").expect("合法"),
            BASIS_BALANCE_DECREASE
        );
        assert!(normalize_calc_basis("CREDIT").is_err());
        assert_eq!(BASES.len(), 4, "四个口径一个都不能少");
    }

    /// ④ 模式语法：清单 / 下级 / 前缀 / 类别四种写法都收，空项与非法类别都拒。
    #[test]
    fn pattern_grammar_accepts_four_forms() {
        for ok in ["1001,1002", "2221", "14*", "14%", "@CATEGORY:PROFIT_LOSS", "1122, 1121"] {
            assert!(validate_pattern(ok).is_ok(), "「{ok}」应合法");
        }
        for bad in ["", "  ", "1001,,1002", "@CATEGORY:NOPE", "*", "14**", "1001;1002"] {
            assert!(validate_pattern(bad).is_err(), "「{bad}」必须被拒");
        }
    }

    /// ⑤ 行形状：ACCOUNT 必须两口径齐全；MANUAL 必须两者皆空（否则就是摆设）。
    #[test]
    fn shape_rule_separates_account_and_manual() {
        let (basis, pattern) = normalize_shape(SOURCE_ACCOUNT, Some("credit_net"), Some("1602")).expect("合法行");
        assert_eq!(basis.as_deref(), Some(BASIS_CREDIT_NET));
        assert_eq!(pattern.as_deref(), Some("1602"));

        assert!(normalize_shape(SOURCE_ACCOUNT, None, Some("1602")).is_err());
        assert!(normalize_shape(SOURCE_ACCOUNT, Some(BASIS_CREDIT_NET), None).is_err());
        assert!(
            normalize_shape(SOURCE_MANUAL, Some(BASIS_CREDIT_NET), None).is_err(),
            "MANUAL 行带口径必须被拒"
        );
        assert!(
            normalize_shape(SOURCE_MANUAL, None, Some("1602")).is_err(),
            "MANUAL 行带模式必须被拒"
        );
        let (basis, pattern) = normalize_shape(SOURCE_MANUAL, None, None).expect("MANUAL 合法");
        assert!(basis.is_none() && pattern.is_none());
    }

    /// ⑥ 行级自检：命中 0 个科目要点名（「没发生」与「配错了」必须能分清）。
    #[test]
    fn row_audit_names_zero_match_rows() {
        let accounts = toy_accounts();
        let (hits, warnings) = row_audit(&row("A", BASIS_CREDIT_NET, "1602", true), &accounts);
        assert_eq!(hits, Some(1));
        assert!(warnings.is_empty());

        let (hits, warnings) = row_audit(&row("B", BASIS_CREDIT_NET, "1231,1471", true), &accounts);
        assert_eq!(hits, Some(0));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("恒为 0"), "{}", warnings[0]);

        let manual = AdjustRow {
            source_type: SOURCE_MANUAL.to_string(),
            calc_basis: None,
            account_pattern: None,
            ..row("C", BASIS_CREDIT_NET, "1602", true)
        };
        assert_eq!(row_audit(&manual, &accounts).0, None, "MANUAL 行没有命中概念");
    }

    /// ⑦ 跨行自检：停用行不计、重复口径点名、同科目被两行命中点名、全停用时告警。
    #[test]
    fn cross_row_audit_catches_double_counting() {
        let accounts = toy_accounts();
        let rows = vec![
            row("A", BASIS_CREDIT_NET, "1602", true),
            // 与 A 完全同口径 ⇒ 同一批科目算两遍
            row("B", BASIS_CREDIT_NET, "1602", true),
            // 与 A 部分重叠（1602）⇒ 科目级告警
            row("C", BASIS_DEBIT_NET, "1602,5603", true),
            // 停用行不参与
            row("D", BASIS_CREDIT_NET, "2202", false),
        ];
        let warnings = audit_rows(&rows, &accounts);
        assert!(warnings.iter().any(|w| w.contains("取数口径完全重复")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("科目 1602 同时被")), "{warnings:?}");
        assert!(
            !warnings.iter().any(|w| w.contains("2202")),
            "停用行不该参与告警：{warnings:?}"
        );

        let stopped = vec![row("D", BASIS_CREDIT_NET, "2202", false)];
        let warnings = audit_rows(&stopped, &accounts);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("没有任何启用的调整项"), "{}", warnings[0]);
    }

    /// ⑧ 两个固定行（净利润 / 小计）+ 结构式：它们是「算出来的」，不是配置行。
    #[test]
    fn anchors_are_fixed_and_not_configurable() {
        let anchors = anchors();
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].code, ANCHOR_NET_PROFIT);
        assert_eq!(anchors[0].role, "START");
        assert_eq!(anchors[1].code, ANCHOR_OPERATING_NET);
        assert_eq!(anchors[1].role, "SUBTOTAL");
        assert!(LINE_FORMULA.contains("净利润"));
        assert!(LINE_FORMULA.contains("经营活动产生的现金流量净额"));
    }

    /// ⑨ 字典：4 个口径 + 2 个来源，值与常量逐字一致（前端不必猜枚举）。
    #[test]
    fn catalog_matches_constants() {
        let catalog = compose_catalog(12, 10);
        assert_eq!(catalog.source_types.len(), 2);
        assert_eq!(catalog.source_types[0].value, SOURCE_ACCOUNT.to_string());
        assert_eq!(catalog.calc_bases.len(), 4);
        let values: Vec<&str> = catalog.calc_bases.iter().map(|d| d.value.as_str()).collect();
        assert_eq!(values, BASES.to_vec());
        assert_eq!(catalog.total, 12);
        assert_eq!(catalog.active_total, 10);
    }

    /// ⑩ 权限码：与 V0416 的 `sys_menu.auth_code` 逐字一致，且只有两个。
    #[test]
    fn permissions_are_well_formed() {
        assert_eq!(PERM_LIST, "business:erp:cashflow-adjust:list");
        assert_eq!(PERM_MANAGE, "business:erp:cashflow-adjust:manage");
        assert_eq!(PERMISSIONS, [PERM_LIST, PERM_MANAGE]);
    }

    /// ⑪ 列表合并：`active_only` 只影响返回的行，不影响 total / active_total / 告警口径。
    #[test]
    fn compose_list_keeps_totals_independent_of_filter() {
        let accounts = toy_accounts();
        let rows = vec![
            row("A", BASIS_CREDIT_NET, "1602", true),
            row("B", BASIS_CREDIT_NET, "2202", false),
        ];
        let all = compose_list(&rows, &accounts, false);
        assert_eq!(all.items.len(), 2);
        assert_eq!(all.total, 2);
        assert_eq!(all.active_total, 1);
        let only_active = compose_list(&rows, &accounts, true);
        assert_eq!(only_active.items.len(), 1);
        assert_eq!(only_active.total, 2, "总数不随筛选变");
        assert_eq!(only_active.active_total, 1);
        assert_eq!(only_active.account_total, accounts.len());
    }

    // ---------- 真库 ----------

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        reset().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清场：只删本卡用例造的行（12 行种子与别人的配置一行不动）。
    async fn reset() {
        db().execute_with_params(
            &format!("DELETE FROM {TABLE} WHERE item_code LIKE ?"),
            vec![Value::Text(format!("{SCRATCH}%"))],
        )
        .await
        .expect("清本卡用例行");
    }

    fn req(code: &str, name: &str, basis: &str, pattern: &str) -> AdjustUpsertReq {
        AdjustUpsertReq {
            item_code: Some(code.to_string()),
            item_name: Some(name.to_string()),
            source_type: Some(SOURCE_ACCOUNT.to_string()),
            calc_basis: Some(basis.to_string()),
            account_pattern: Some(pattern.to_string()),
            sort_order: None,
            is_active: None,
            remark: None,
        }
    }

    /// ⑫ 真库往返：新增 → 列表里在 → 更新 → 启停；编码重复 409、口径重复 409。
    #[tokio::test]
    async fn create_update_toggle_roundtrip() {
        let Some(_guard) = setup().await else {
            return;
        };
        let code = format!("{SCRATCH}ROUNDTRIP");
        let saved = create_adjust_item(&req(&code, "测试折旧", BASIS_CREDIT_NET, "5601"))
            .await
            .expect("新增");
        assert_eq!(saved.item.item_code, code);
        assert!(saved.item.is_active);
        assert_eq!(saved.item.matched_account_count, Some(1), "5601 在标准科目表里必须命中");
        assert_eq!(saved.item.matched_accounts[0].account_code, "5601");
        assert!(saved.item.basis_label.is_some());

        // 列表里能看到（含停用行的全量视角）
        let list = list_adjust_items(false).await.expect("列表");
        assert!(list.items.iter().any(|item| item.item_code == code));
        assert_eq!(list.total, list.items.len());
        assert!(list.account_total > 0);

        // 编辑：改名 + 改口径（模式保留）
        let mut update = req(&code, "测试折旧（改名）", BASIS_CREDIT_NET, "5601");
        update.item_code = Some(code.clone());
        update.calc_basis = Some(BASIS_DEBIT_NET.to_string());
        let saved = update_adjust_item(&saved.item.id, &update).await.expect("编辑");
        assert_eq!(saved.item.item_name, "测试折旧（改名）");
        assert_eq!(saved.item.calc_basis.as_deref(), Some(BASIS_DEBIT_NET));

        // 改编码必须被拒（编码不可变）
        let mut rename = req(&format!("{SCRATCH}RENAMED"), "测试折旧", BASIS_DEBIT_NET, "5601");
        rename.item_code = Some(format!("{SCRATCH}RENAMED"));
        let error = update_adjust_item(&saved.item.id, &rename)
            .await
            .expect_err("改编码必须拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("编码不可修改"), "{}", error.msg);

        // 启停
        let off = toggle_adjust_item(&saved.item.id, false).await.expect("停用");
        assert!(!off.is_active);
        let list = list_adjust_items(true).await.expect("只看启用");
        assert!(
            list.items.iter().all(|item| item.item_code != code),
            "active_only 不该返回停用行"
        );
        let all = list_adjust_items(false).await.expect("全量");
        assert!(
            all.items.iter().any(|item| item.item_code == code),
            "停用不等于消失：全量列表里必须还在（可以改回来）"
        );
        let on = toggle_adjust_item(&saved.item.id, true).await.expect("启用");
        assert!(on.is_active);

        // 编码重复 409
        let error = create_adjust_item(&req(&code, "重复", BASIS_CREDIT_NET, "5601"))
            .await
            .expect_err("编码重复必须拒");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("已存在"), "{}", error.msg);
    }

    /// ⑬ 口径重复闸：同「模式 + 口径」再加一行必须 409（同一批科目被算两遍）。
    #[tokio::test]
    async fn duplicate_basis_is_rejected() {
        let Some(_guard) = setup().await else {
            return;
        };
        let code = format!("{SCRATCH}DUP_A");
        create_adjust_item(&req(&code, "重复口径 A", BASIS_CREDIT_NET, "5601"))
            .await
            .expect("第一行");
        let error = create_adjust_item(&req(&format!("{SCRATCH}DUP_B"), "重复口径 B", BASIS_CREDIT_NET, "5601"))
            .await
            .expect_err("同模式同口径必须拒");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("取数口径重复"), "{}", error.msg);

        // 同模式、不同口径是允许的（分层配置，靠列表 warnings 点名重叠）
        create_adjust_item(&req(
            &format!("{SCRATCH}DUP_C"),
            "同模式不同口径",
            BASIS_DEBIT_NET,
            "5601",
        ))
        .await
        .expect("同模式不同口径应允许");
    }

    /// ⑭ MANUAL 行：带口径必须 400；不带就能存，且不参与命中统计。
    #[tokio::test]
    async fn manual_rows_reject_basis_and_pattern() {
        let Some(_guard) = setup().await else {
            return;
        };
        let mut bad = AdjustUpsertReq {
            item_code: Some(format!("{SCRATCH}MANUAL_BAD")),
            item_name: Some("手工坏行".to_string()),
            source_type: Some(SOURCE_MANUAL.to_string()),
            calc_basis: Some(BASIS_CREDIT_NET.to_string()),
            ..AdjustUpsertReq::default()
        };
        let error = create_adjust_item(&bad).await.expect_err("MANUAL 带口径必须拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("摆设"), "{}", error.msg);

        bad.item_code = Some(format!("{SCRATCH}MANUAL_OK"));
        bad.calc_basis = None;
        let saved = create_adjust_item(&bad).await.expect("MANUAL 合法行");
        assert_eq!(saved.item.source_type, SOURCE_MANUAL);
        assert!(saved.item.calc_basis.is_none());
        assert_eq!(saved.item.matched_account_count, None);
        assert_eq!(saved.item.source_label, "手工填列");
    }

    /// ⑮ 12 行标准模板在库里：4 个口径都用到了，MANUAL 两行存在，全部启用。
    #[tokio::test]
    async fn seed_template_is_complete() {
        let Some(_guard) = setup().await else {
            return;
        };
        let list = list_adjust_items(false).await.expect("列表");
        let seeds: Vec<&AdjustItemRes> = list
            .items
            .iter()
            .filter(|item| item.item_code.starts_with("ADJ_"))
            .collect();
        assert_eq!(seeds.len(), 12, "V0416 应种 12 行标准模板");
        assert!(seeds.iter().all(|item| item.is_active));
        let bases: std::collections::HashSet<&str> =
            seeds.iter().filter_map(|item| item.calc_basis.as_deref()).collect();
        assert_eq!(bases.len(), 4, "四个口径都要在模板里出现过：{bases:?}");
        let manual = seeds.iter().filter(|item| item.source_type == SOURCE_MANUAL).count();
        assert_eq!(manual, 2, "递延所得税 / 其他 两行手工填列");
    }

    /// ⑯ 不存在的 id：编辑 / 启停都 404（不是 500，也不是静默成功）。
    #[tokio::test]
    async fn unknown_id_is_404() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ghost = Uuid::now_v7().to_string();
        let error = toggle_adjust_item(&ghost, true).await.expect_err("不存在的 id");
        assert_eq!(error.status, 404);
        let error = update_adjust_item(&ghost, &req("M115_GHOST", "幽灵", BASIS_CREDIT_NET, "1602"))
            .await
            .expect_err("不存在的 id");
        assert_eq!(error.status, 404);
    }
}
