//! 薪资项入参 / 回执契约（`biz_salary_item` · HR-31）。
//!
//! 设计 §5.2 的四条接口形状全在这里（`/api` 前缀由框架注入）：
//!
//! | # | 方法 | 路径 | 权限码 | 入参 | 回执 |
//! |:--:|---|---|---|---|---|
//! | 32 | GET | `/business/hr/salary-item/page` | `business:hr:salary-item:list` | [`SalaryItemPageQuery`] | `PageData<Vec<SalaryItemRes>>` |
//! | 33 | POST | `/business/hr/salary-item` | `business:hr:salary-item:create` | [`SalaryItemWriteReq`] | [`SalaryItemRes`] |
//! | 34 | PUT | `/business/hr/salary-item/{id}` | `business:hr:salary-item:update` | [`SalaryItemWriteReq`] | [`SalaryItemRes`] |
//! | 35 | POST | `/business/hr/salary-item/validate` | `business:hr:salary-item:update` | [`SalaryItemValidateReq`] | [`SalaryItemValidateRes`] |
//!
//! # 校验分两层（这是本卡的核心）
//!
//! | 层 | 在哪 | 管什么 |
//! |---|---|---|
//! | **结构层** | [`crate::business::hr::salary_item_formula`] | 长度 ≤500、字符白名单、括号配对、`{VAR}` 名字合法、禁用 SQL 片段 |
//! | **语义层** | 本文件 [`check_formula`] | 未定义变量（对着「已有项编码 ∪ `ATT_*`」判）、环依赖、依赖拓扑序 |
//! | **业务层** | 本文件 [`parse_write`] | 类型 / 算法 / 科目按 `item_type` 分档必填、`SUMMARY` 必须带公式、公式列宽、备注长度 |
//!
//! 结构层与语义层对「保存」和「检查按钮」（§5.2 #35）是**同一份代码** —— 配置页点「检查」
//! 通过、点保存却被拒（或反过来）是最伤信任的一类不一致。
//!
//! # 为什么不在这里求值
//!
//! 求值（`Decimal` 精度、惰性 `IF`、除零报错）是 HR-32 / HR-34 的活；本卡只回答
//! 「这个公式能不能存」。两条路径共用同一套 `{VAR}` 抽取口径（[`salary_item_formula::extract_refs`]），
//! 否则「配置页说没环、算工资时说有环」。
//!
//! # 科目为什么不查存在性
//!
//! 科目码（`5602.10` / `2211.01`）的权威在 `erp_account`（是否末级、是否启用），查它要跨模块
//! 读表。本卡只校验**形状**（非空、≤30 字符、字符集），存在性校验留给薪资过账（HR-39）——
//! 过账时才真正需要「这个科目能用」，那时报错也来得及，且报文能带「科目不存在 / 非末级」。

use std::collections::BTreeSet;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::salary_item_formula as formula;
use crate::shared::error_code;

// ==================== 域的取值（与 V0383 的 CHECK 逐字一致） ====================

/// 收入项（加项）。
pub const ITEM_TYPE_EARNING: &str = "EARNING";

/// 应发前扣减（考勤扣款这类「算少一点」的项，设计 §4.4.1 明确它不是 `WITHHOLD`）。
pub const ITEM_TYPE_PRE_DEDUCT: &str = "PRE_DEDUCT";

/// 代扣项（发的时候扣走，如社保个人 / 个税）。
pub const ITEM_TYPE_WITHHOLD: &str = "WITHHOLD";

/// 单位承担（不进实发，进计提凭证）。
pub const ITEM_TYPE_EMPLOYER: &str = "EMPLOYER";

/// 汇总项（`GROSS` / `NET`，不落 `detail_item`）。
pub const ITEM_TYPE_SUMMARY: &str = "SUMMARY";

/// `item_type` 五档 + 中文名（顺序 = `ck_biz_sal_item_type`）。
pub const ITEM_TYPES: [(&str, &str); 5] = [
    (ITEM_TYPE_EARNING, "收入项"),
    (ITEM_TYPE_PRE_DEDUCT, "应发前扣减"),
    (ITEM_TYPE_WITHHOLD, "代扣项"),
    (ITEM_TYPE_EMPLOYER, "单位承担"),
    (ITEM_TYPE_SUMMARY, "汇总项"),
];

/// 取标准（`biz_employee_salary`）。
pub const CALC_TYPE_FIXED: &str = "FIXED";

/// 按公式算。
pub const CALC_TYPE_FORMULA: &str = "FORMULA";

/// 手工录 / 导入。
pub const CALC_TYPE_INPUT: &str = "INPUT";

/// `calc_type` 三档 + 中文名（顺序 = `ck_biz_sal_item_calc`）。
pub const CALC_TYPES: [(&str, &str); 3] = [
    (CALC_TYPE_FIXED, "取薪资标准"),
    (CALC_TYPE_FORMULA, "公式计算"),
    (CALC_TYPE_INPUT, "手工录入"),
];

/// `source` 五个取值 + 中文名（顺序 = `ck_biz_sal_item_source`）。
pub const SOURCES: [(&str, &str); 5] = [
    ("EMPLOYEE_STANDARD", "员工薪资标准"),
    ("ATTENDANCE", "考勤"),
    ("INPUT", "手工录入"),
    ("SYSTEM", "系统计算"),
    ("MES", "MES 结算汇总"),
];

/// 未指定 `source` 时的默认值（与 `V0383` 的列默认值 `INPUT` 同一口径）。
pub const DEFAULT_SOURCE: &str = "INPUT";

/// 公式长度上限（`biz_salary_item.formula VARCHAR(500)`，按**字符**算）。
///
/// 这里是 [`formula::MAX_FORMULA_LEN`] 的公开别名：调用方（db 用例 / 前端契约文档）不用
/// 为了拿一个常量再 `use` 一层公式模块，且只有一处定义（改了迁移不会漏改第二处）。
pub const MAX_FORMULA_LEN: usize = formula::MAX_FORMULA_LEN;

/// 编码长度上限（`item_code VARCHAR(32)`）。
pub const MAX_ITEM_CODE_LEN: usize = 32;

/// 名称长度上限（`item_name VARCHAR(50)`）。
pub const MAX_ITEM_NAME_LEN: usize = 50;

/// 科目码长度上限（`debit_account_code` / `credit_account_code VARCHAR(30)`）。
pub const MAX_ACCOUNT_CODE_LEN: usize = 30;

/// 备注长度上限（`remark VARCHAR(500)`）。
pub const MAX_REMARK_LEN: usize = 500;

/// `sort_order` 上限（`INT`，给个业务上限免得有人填 `2147483647` 把后面的项挤没位置）。
pub const MAX_SORT_ORDER: i32 = 999_999;

/// 列表默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 列表每页上限（配置表只有十几行，200 足够且防打爆）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 域值 → 中文名（未知值原样返回，便于报错时显示用户输入）。
pub fn label_of(table: &[(&'static str, &'static str)], code: &str) -> String {
    table
        .iter()
        .find(|(value, _)| *value == code)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// 是否在域内。
fn in_domain(table: &[(&'static str, &'static str)], code: &str) -> bool {
    table.iter().any(|(value, _)| *value == code)
}

/// 域内取值的清单文本（报错时给「只能是 …」）。
fn domain_list(table: &[(&'static str, &'static str)]) -> String {
    table
        .iter()
        .map(|(value, label)| format!("{value}（{label}）"))
        .collect::<Vec<_>>()
        .join(" / ")
}

// ==================== 列表入参 ====================

/// 列表查询串（设计 §5.2 #32 + §5.3 筛选）。
///
/// 全部可选：配置页打开时无筛选；`item_type` / `calc_type` 精确匹配（前端是下拉），
/// `keyword` 对编码 / 名称做 `ILIKE`，`is_active` 是 `0/1`（筛「已停用」的项）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryItemPageQuery {
    /// 页码（从 1 开始，缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
    /// 关键字（编码 / 名称模糊）。
    pub keyword: Option<String>,
    /// 类型过滤（`EARNING` / `PRE_DEDUCT` / …）。
    pub item_type: Option<String>,
    /// 算法过滤（`FIXED` / `FORMULA` / `INPUT`）。
    pub calc_type: Option<String>,
    /// 启用过滤（`1` 启用 / `0` 停用）。
    pub is_active: Option<i32>,
}

/// 归一化后的列表过滤条件（[`build_list_query`] 的入参，纯数据、可单测）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalaryItemFilter {
    /// 关键字（已 trim，空串归 `None`）。
    pub keyword: Option<String>,
    /// 类型。
    pub item_type: Option<String>,
    /// 算法。
    pub calc_type: Option<String>,
    /// 启用标记。
    pub is_active: Option<i32>,
}

/// 页码 / 每页条数的归一化（与 `attendance_period_dto::normalize_page` 同一口径）。
pub fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

/// 查询串 → [`SalaryItemFilter`]（域值非法直接 400，**不静默忽略**：
/// 前端把 `EARNING` 拼成 `EARNINGS` 时应该看到报错，而不是「筛完还是全量」）。
pub fn filter_of(query: &SalaryItemPageQuery) -> std::result::Result<SalaryItemFilter, ApiError> {
    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let item_type = match query.item_type.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(value) if in_domain(&ITEM_TYPES, value) => Some(value.to_string()),
        Some(value) => {
            return Err(param_invalid(format!(
                "item_type `{value}` 不合法：只能是 {}",
                domain_list(&ITEM_TYPES)
            )));
        }
        None => None,
    };
    let calc_type = match query.calc_type.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(value) if in_domain(&CALC_TYPES, value) => Some(value.to_string()),
        Some(value) => {
            return Err(param_invalid(format!(
                "calc_type `{value}` 不合法：只能是 {}",
                domain_list(&CALC_TYPES)
            )));
        }
        None => None,
    };
    if let Some(flag) = query.is_active {
        if flag != 0 && flag != 1 {
            return Err(param_invalid(format!("is_active 只能是 0 或 1（收到 {flag}）")));
        }
    }
    Ok(SalaryItemFilter {
        keyword,
        item_type,
        calc_type,
        is_active: query.is_active,
    })
}

// ==================== 写接口入参 ====================

/// 新建 / 修改薪资项的入参（`PUT` 是**整行覆盖**：名称 / 类型 / 算法必填）。
///
/// `item_code` 只在新建时生效 —— 修改时传了也必须与原值相同（编码是被公式 `{CODE}` 与
/// `biz_employee_salary.item_code` 弱引用的键，改名会让引用它的公式与薪资标准一起失效）。
///
/// `is_taxable` / `is_active` 缺省时：新建取 `true`，修改**保持原值**（不传 ≠ 停用）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryItemWriteReq {
    /// 编码（新建必填；只会出现大写字母/数字/下划线，首字符必须是字母）。
    pub item_code: Option<String>,
    /// 名称（必填，≤50 字符）。
    pub item_name: Option<String>,
    /// 类型（必填）。
    pub item_type: Option<String>,
    /// 算法（必填）。
    pub calc_type: Option<String>,
    /// 取数来源（缺省 `INPUT`）。
    pub source: Option<String>,
    /// 公式（`calc_type = FORMULA` 时必填，≤500 字符；其它算法必须留空）。
    pub formula: Option<String>,
    /// 计算顺序（缺省：新建取「当前最大 + 10」，修改保持原值）。公司内唯一。
    pub sort_order: Option<i32>,
    /// 借方科目。
    pub debit_account_code: Option<String>,
    /// 贷方科目。
    pub credit_account_code: Option<String>,
    /// 是否计入个税应税收入。
    pub is_taxable: Option<bool>,
    /// 是否启用。
    pub is_active: Option<bool>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改时用来「缺省即保持原值」的两个开关。
#[derive(Debug, Clone, Copy)]
pub struct ExistingFlags {
    /// 原 `is_taxable`。
    pub is_taxable: i16,
    /// 原 `is_active`。
    pub is_active: i16,
}

/// 校验通过、可直接落库的薪资项（列宽 / 域值 / 科目分档都已归一化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryItemDraft {
    /// 编码。
    pub item_code: String,
    /// 名称（已 trim）。
    pub item_name: String,
    /// 类型。
    pub item_type: String,
    /// 算法。
    pub calc_type: String,
    /// 来源。
    pub source: String,
    /// 公式（`None` = 该算法不用公式）。
    pub formula: Option<String>,
    /// 计算顺序。
    pub sort_order: i32,
    /// 借方科目。
    pub debit_account_code: Option<String>,
    /// 贷方科目。
    pub credit_account_code: Option<String>,
    /// 1 计入应税收入 / 0 不计入。
    pub is_taxable: i16,
    /// 1 启用 / 0 停用。
    pub is_active: i16,
    /// 备注。
    pub remark: Option<String>,
}

/// 写接口的校验（新建与修改共用；`existing` 为 `None` 时按新建的缺省口径）。
///
/// # 校验顺序与分组
///
/// 一次把**所有**字段问题收齐再报（`；` 分隔），而不是「改一个、再提交、再报一个」。
///
/// # 异常情况
///
/// 返回 `400 + 10001`，报文点名要改哪个字段、当前值是什么。
pub fn parse_write(
    req: &SalaryItemWriteReq,
    item_code: &str,
    fallback_sort: i32,
    existing: Option<ExistingFlags>,
) -> std::result::Result<SalaryItemDraft, ApiError> {
    let mut errors: Vec<String> = Vec::new();

    // ---- 编码（修改时由调用方传入原值，这里只校验形状） ----
    let code = item_code.trim();
    if code.is_empty() {
        errors.push("薪资项编码 item_code 必填（例：`BASE`、`OT_WD`）".to_string());
    } else {
        if !formula::is_var_name(code) {
            errors.push(format!(
                "薪资项编码 `{code}` 不合法：只允许大写字母开头 + 大写字母/数字/下划线（例：`BASE`、`OT_WD`）—— \
                 公式里按 `{{编码}}` 引用，小写或带 `-` 的编码引用不到"
            ));
        }
        if code.chars().count() > MAX_ITEM_CODE_LEN {
            errors.push(format!(
                "薪资项编码最长 {MAX_ITEM_CODE_LEN} 个字符（`{code}` 有 {} 个）",
                code.chars().count()
            ));
        }
        if formula::att_var_names().iter().any(|var| var == code) {
            errors.push(format!(
                "薪资项编码 `{code}` 与考勤变量同名：`ATT_*` 里已经代表考勤口径的名字不能再当薪资项（公式里一个名字两个含义），\
                 请换个编码（如 `ATT_BONUS` 这种「ATT_ 开头但不撞白名单」的写法是允许的）"
            ));
        }
    }

    // ---- 名称 ----
    let name = req.item_name.as_deref().map(str::trim).unwrap_or_default();
    if name.is_empty() {
        errors.push("薪资项名称 item_name 必填".to_string());
    } else if name.chars().count() > MAX_ITEM_NAME_LEN {
        errors.push(format!(
            "薪资项名称最长 {MAX_ITEM_NAME_LEN} 个字符（收到 {} 个）",
            name.chars().count()
        ));
    }

    // ---- 类型 / 算法 ----
    let item_type = req.item_type.as_deref().map(str::trim).unwrap_or_default();
    if !in_domain(&ITEM_TYPES, item_type) {
        errors.push(format!(
            "类型 item_type 不合法（收到 `{item_type}`）：只能是 {}",
            domain_list(&ITEM_TYPES)
        ));
    }
    let calc_type = req.calc_type.as_deref().map(str::trim).unwrap_or_default();
    if !in_domain(&CALC_TYPES, calc_type) {
        errors.push(format!(
            "算法 calc_type 不合法（收到 `{calc_type}`）：只能是 {}",
            domain_list(&CALC_TYPES)
        ));
    }
    if item_type == ITEM_TYPE_SUMMARY && calc_type != CALC_TYPE_FORMULA && in_domain(&CALC_TYPES, calc_type) {
        errors.push(format!(
            "汇总项（`{ITEM_TYPE_SUMMARY}`）必须用 `{CALC_TYPE_FORMULA}`：它的值只能由其它项按公式汇总出来\
             （设计 §4.4.3 的 `GROSS` / `NET` 就是这种）"
        ));
    }

    // ---- 来源 ----
    let source = match req.source.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(value) => value.to_string(),
        None => DEFAULT_SOURCE.to_string(),
    };
    if !in_domain(&SOURCES, &source) {
        errors.push(format!(
            "取数来源 source 不合法（收到 `{source}`）：只能是 {}",
            domain_list(&SOURCES)
        ));
    }

    // ---- 公式（长度 / 结构；语义留给 check_formula） ----
    let raw_formula = req.formula.as_deref().map(str::trim).filter(|text| !text.is_empty());
    let formula_text = match (calc_type, raw_formula) {
        (CALC_TYPE_FORMULA, Some(text)) => {
            let structural = formula::structural_errors(text);
            if structural.is_empty() {
                Some(text.to_string())
            } else {
                errors.extend(structural);
                Some(text.to_string())
            }
        }
        (CALC_TYPE_FORMULA, None) => {
            errors.push(format!(
                "算法是 `{CALC_TYPE_FORMULA}` 时公式 formula 必填（库约束 `ck_biz_sal_item_formula` 也要求非空）"
            ));
            None
        }
        (_, Some(_)) => {
            errors.push(format!(
                "算法是 `{calc_type}` 时公式 formula 必须留空：只有 `{CALC_TYPE_FORMULA}` 的项才按公式算，\
                 填了也不会被执行（容易被误读成「已生效」）"
            ));
            None
        }
        (_, None) => None,
    };

    // ---- 排序 ----
    let sort_order = req.sort_order.unwrap_or(fallback_sort);
    if sort_order <= 0 || sort_order > MAX_SORT_ORDER {
        errors.push(format!(
            "计算顺序 sort_order 必须在 1~{MAX_SORT_ORDER} 之间（收到 {sort_order}）：它决定试算时的稳定顺序"
        ));
    }

    // ---- 科目（按类型分档，与 `ck_biz_sal_item_account` 同口径） ----
    let debit = normalize_account(
        req.debit_account_code.as_deref(),
        "借方科目 debit_account_code",
        &mut errors,
    );
    let credit = normalize_account(
        req.credit_account_code.as_deref(),
        "贷方科目 credit_account_code",
        &mut errors,
    );
    match item_type {
        ITEM_TYPE_EARNING | ITEM_TYPE_PRE_DEDUCT | ITEM_TYPE_EMPLOYER => {
            if debit.is_none() {
                errors.push(format!(
                    "类型 `{item_type}`（{}）必须给借方科目与贷方科目：过账要按科目生成凭证",
                    label_of(&ITEM_TYPES, item_type)
                ));
            }
            if credit.is_none() {
                errors.push(format!(
                    "类型 `{item_type}`（{}）必须给贷方科目：过账要按科目生成凭证",
                    label_of(&ITEM_TYPES, item_type)
                ));
            }
        }
        ITEM_TYPE_WITHHOLD => {
            if credit.is_none() {
                errors.push(format!(
                    "类型 `{ITEM_TYPE_WITHHOLD}`（代扣项）必须给贷方科目（贷方 = 社保中心 / 税务等债权主体）"
                ));
            }
            if debit.is_some() {
                errors.push(format!(
                    "类型 `{ITEM_TYPE_WITHHOLD}`（代扣项）只填贷方科目，借方由试算按项目自身生成（收到借方 `{}`）",
                    debit.clone().unwrap_or_default()
                ));
            }
        }
        ITEM_TYPE_SUMMARY => {
            if debit.is_some() || credit.is_some() {
                errors.push(format!(
                    "类型 `{ITEM_TYPE_SUMMARY}`（汇总项）不填科目：它的金额由具体项汇总而来，过账时按具体项各自科目走\
                     （收到借方 `{}` / 贷方 `{}`）",
                    debit.clone().unwrap_or_else(|| "—".to_string()),
                    credit.clone().unwrap_or_else(|| "—".to_string())
                ));
            }
        }
        _ => {}
    }

    // ---- 备注 ----
    let remark = req
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    if let Some(text) = &remark {
        if text.chars().count() > MAX_REMARK_LEN {
            errors.push(format!(
                "备注最长 {MAX_REMARK_LEN} 个字符（收到 {} 个）",
                text.chars().count()
            ));
        }
    }

    // ---- 两个开关（缺省：新建 true / 修改保持原值） ----
    let is_taxable = flag_of(req.is_taxable, existing.map(|flags| flags.is_taxable));
    let is_active = flag_of(req.is_active, existing.map(|flags| flags.is_active));

    if !errors.is_empty() {
        return Err(param_invalid(errors.join("；")));
    }

    Ok(SalaryItemDraft {
        item_code: code.to_string(),
        item_name: name.to_string(),
        item_type: item_type.to_string(),
        calc_type: calc_type.to_string(),
        source,
        formula: formula_text,
        sort_order,
        debit_account_code: debit,
        credit_account_code: credit,
        is_taxable,
        is_active,
        remark,
    })
}

/// 科目码归一化（trim / 空串归 `None` / 长度 / 字符集）。
fn normalize_account(raw: Option<&str>, field: &str, errors: &mut Vec<String>) -> Option<String> {
    let text = raw.map(str::trim).filter(|value| !value.is_empty())?;
    if text.chars().count() > MAX_ACCOUNT_CODE_LEN {
        errors.push(format!(
            "{field} 最长 {MAX_ACCOUNT_CODE_LEN} 个字符（收到 {})",
            text.chars().count()
        ));
        return Some(text.to_string());
    }
    if let Some(bad) = text
        .chars()
        .find(|ch| !(ch.is_ascii_digit() || ch.is_ascii_uppercase() || *ch == '.' || *ch == '-' || *ch == '_'))
    {
        errors.push(format!(
            "{field} `{text}` 里有非法字符 `{bad}`：科目码只允许数字、大写字母、`.`、`-`、`_`（例：`5602.10`）"
        ));
    }
    Some(text.to_string())
}

/// 布尔入参 → `SMALLINT`（`None` 时取 `fallback`，再缺省 `1`）。
fn flag_of(value: Option<bool>, fallback: Option<i16>) -> i16 {
    match value {
        Some(true) => 1,
        Some(false) => 0,
        None => fallback.unwrap_or(1),
    }
}

// ==================== 公式检查（语义层） ====================

/// 依赖图里的一行（只取公式检查用得上的三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryItemSnapshot {
    /// 编码。
    pub item_code: String,
    /// 公式（`None` = 非 FORMULA 项）。
    pub formula: Option<String>,
    /// 1 启用 / 0 停用（停用的项不算「可引用」）。
    pub is_active: i16,
}

/// 公式检查结果（保存与「检查按钮」共用）。
#[derive(Debug, Clone, Default)]
pub struct SalaryItemCheck {
    /// 全部问题（结构 + 语义），空 = 通过。
    pub errors: Vec<String>,
    /// 公式引用的全部变量（保序去重）。
    pub refs: Vec<String>,
    /// 其中属于「已有薪资项」的引用。
    pub item_refs: Vec<String>,
    /// 其中属于考勤口径的引用（`ATT_*`）。
    pub att_refs: Vec<String>,
    /// 引用了但不可用的变量。
    pub undefined: Vec<String>,
    /// 依赖顺序（依赖在前，末位是本项；`cycle` 非空时为空）。
    pub dependency_order: Vec<String>,
    /// 环（首尾相同，形如 `A → B → A`）。
    pub cycle: Option<Vec<String>>,
}

impl SalaryItemCheck {
    /// 是否通过。
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 公式语义检查：结构问题 + 未定义变量 + 环依赖 + 依赖拓扑序。
///
/// # 参数
///
/// - `text`：待检查的公式原文；
/// - `self_code`：本项的编码（新建时就是要建的编码；修改 / 检查已有项时是它的编码）。
///   传它是为了①自引用能判成环而不是「未定义变量」②拓扑序能把本项排在依赖之后；
/// - `existing`：库里**全部**薪资项（停用的也传：停用项自己的公式仍参与成环判断，
///   只是它的编码不算「可引用」）。
///
/// # 口径
///
/// * 可引用变量 = 启用中的薪资项编码 ∪ [`formula::att_var_names`] ∪ 本项编码；
/// * 现有项的公式若抽引用失败（历史脏数据），**跳过它的边**而不是整笔报错 ——
///   本接口不该被别的项的坏公式挡住；坏数据由巡检（HR-45）报。
pub fn check_formula(text: &str, self_code: Option<&str>, existing: &[SalaryItemSnapshot]) -> SalaryItemCheck {
    let mut check = SalaryItemCheck {
        errors: formula::structural_errors(text),
        ..SalaryItemCheck::default()
    };
    let refs = match formula::extract_refs(text) {
        Ok(refs) => refs,
        Err(message) => {
            // 引用都抽不出来就不往下判（后面的「未定义 / 环」在残缺引用上全是噪音）
            check.errors.push(message);
            return check;
        }
    };

    let att: BTreeSet<String> = formula::att_var_names().into_iter().collect();
    let mut allowed: BTreeSet<String> = existing
        .iter()
        .filter(|snapshot| snapshot.is_active == 1)
        .map(|snapshot| snapshot.item_code.clone())
        .collect();
    allowed.extend(att.iter().cloned());
    if let Some(code) = self_code {
        allowed.insert(code.to_string());
    }

    check.undefined = formula::undefined_refs(&refs, &allowed);
    if !check.undefined.is_empty() {
        check
            .errors
            .push(formula::undefined_message(&check.undefined, &allowed));
    }
    check.item_refs = refs.iter().filter(|name| !att.contains(*name)).cloned().collect();
    check.att_refs = refs.iter().filter(|name| att.contains(*name)).cloned().collect();

    // ---- 依赖图：库里的项（本项用新公式替换）+ 本项 ----
    let mut nodes: Vec<formula::DepNode> = Vec::with_capacity(existing.len() + 1);
    for snapshot in existing {
        if self_code.is_some_and(|code| code == snapshot.item_code) {
            continue;
        }
        let deps = snapshot
            .formula
            .as_deref()
            .and_then(|text| formula::extract_refs(text).ok())
            .unwrap_or_default();
        nodes.push((snapshot.item_code.clone(), deps));
    }
    if let Some(code) = self_code {
        nodes.push((code.to_string(), refs.clone()));
    }

    check.cycle = formula::first_cycle(&nodes);
    if let Some(cycle) = &check.cycle {
        check.errors.push(format!(
            "公式存在循环依赖：{}（改环上任意一项的引用即可解开；薪资试算遇到环会死循环，所以配置期就挡）",
            cycle.join(" → ")
        ));
        return check;
    }
    if let Some(order) = formula::topo_order(&nodes) {
        check.dependency_order = closure_order(&order, &nodes, self_code, &check.item_refs);
    }
    check.refs = refs;
    check
}

/// 从完整拓扑序里裁出「本项 + 它（直接/间接）依赖的项」，保持依赖在前的顺序。
///
/// 设计 §6 的例子是 `BASE → OT_WD → GROSS`，而不是把十几项全列出来 —— 用户只关心
/// 「我这一个公式依赖谁、谁先算」。
fn closure_order(
    order: &[String],
    nodes: &[formula::DepNode],
    self_code: Option<&str>,
    item_refs: &[String],
) -> Vec<String> {
    let edges: std::collections::HashMap<&str, Vec<String>> =
        nodes.iter().map(|(code, deps)| (code.as_str(), deps.clone())).collect();
    let mut seeds: Vec<String> = Vec::new();
    if let Some(code) = self_code {
        seeds.push(code.to_string());
    }
    for name in item_refs {
        if !seeds.contains(name) {
            seeds.push(name.clone());
        }
    }
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = seeds;
    while let Some(code) = queue.pop() {
        if !reachable.insert(code.clone()) {
            continue;
        }
        if let Some(deps) = edges.get(code.as_str()) {
            for dep in deps {
                if edges.contains_key(dep.as_str()) && !reachable.contains(dep) {
                    queue.push(dep.clone());
                }
            }
        }
    }
    order
        .iter()
        .filter(|code| reachable.contains(code.as_str()))
        .cloned()
        .collect()
}

// ==================== 公式检查接口（§5.2 #35） ====================

/// 公式检查入参。
///
/// `id` 与 `item_code` 二选一：改已有项时传 `id`（后端自己查编码，免得前端传错）；
/// 新建时前端还没 id，传 `item_code` 即可（没填也能检查，只是拓扑序里没有本项）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryItemValidateReq {
    /// 待检查的公式（必填）。
    pub formula: Option<String>,
    /// 待检查项的编码（新建时用）。
    pub item_code: Option<String>,
    /// 待检查项的 id（修改已有项时用，优先级高于 `item_code`）。
    pub id: Option<String>,
}

/// 检查结果回执（`ok=false` 也返回 200：这是「查询结论」，不是请求错误）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryItemValidateRes {
    /// 是否通过。
    pub ok: bool,
    /// 问题清单（结构 + 语义，可直接逐条显示）。
    pub errors: Vec<String>,
    /// 引用的变量（保序去重）。
    pub refs: Vec<String>,
    /// 其中属于已有薪资项的。
    pub item_refs: Vec<String>,
    /// 其中属于考勤口径的。
    pub att_refs: Vec<String>,
    /// 未定义变量。
    pub undefined: Vec<String>,
    /// 依赖顺序（依赖在前，末位本项）。
    pub dependency_order: Vec<String>,
    /// 环（首尾相同）。
    pub cycle: Option<Vec<String>>,
    /// 可用变量清单（前端变量面板：点一下插进公式）。
    pub available: Vec<SalaryItemVarOption>,
}

/// 可用变量（前端「变量」面板的一行）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryItemVarOption {
    /// 变量名（`BASE` / `ATT_OT_WD`）。
    pub name: String,
    /// 中文名（`基本工资` / `平时加班小时`）。
    pub label: String,
    /// 来源（`ITEM` 薪资项 / `ATTENDANCE` 考勤）。
    pub kind: String,
    /// 该变量当前是否可引用（停用的薪资项 `false`）。
    pub usable: bool,
}

/// 可用变量清单（薪资项 ∪ 考勤变量）。
pub fn available_vars(existing: &[SalaryItemSnapshot]) -> Vec<SalaryItemVarOption> {
    let mut out: Vec<SalaryItemVarOption> = existing
        .iter()
        .map(|snapshot| SalaryItemVarOption {
            name: snapshot.item_code.clone(),
            label: snapshot.item_code.clone(),
            kind: "ITEM".to_string(),
            usable: snapshot.is_active == 1,
        })
        .collect();
    for (name, _column, label) in formula::ATT_VARS {
        out.push(SalaryItemVarOption {
            name: name.to_string(),
            label: label.to_string(),
            kind: "ATTENDANCE".to_string(),
            usable: true,
        });
    }
    out
}

/// 组回执。
pub fn validate_res(check: SalaryItemCheck, existing: &[SalaryItemSnapshot]) -> SalaryItemValidateRes {
    SalaryItemValidateRes {
        ok: check.ok(),
        errors: check.errors,
        refs: check.refs,
        item_refs: check.item_refs,
        att_refs: check.att_refs,
        undefined: check.undefined,
        dependency_order: check.dependency_order,
        cycle: check.cycle,
        available: available_vars(existing),
    }
}

/// 公式检查不通过 → `400 + 10001`（保存路径用；检查按钮路径不报错，只回 `ok=false`）。
pub fn formula_error(check: &SalaryItemCheck) -> ApiError {
    param_invalid(format!("薪资项公式校验未通过：{}", check.errors.join("；")))
}

// ==================== 回执 ====================

/// 薪资项回执（列表与单条同一形状）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryItemRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub item_code: String,
    /// 名称。
    pub item_name: String,
    /// 类型码值。
    pub item_type: String,
    /// 类型中文名。
    pub item_type_label: String,
    /// 算法码值。
    pub calc_type: String,
    /// 算法中文名。
    pub calc_type_label: String,
    /// 来源码值。
    pub source: String,
    /// 来源中文名。
    pub source_label: String,
    /// 公式。
    pub formula: Option<String>,
    /// 公式引用的项编码（前端列表里显示「依赖：BASE / POST」；抽不出来时为空）。
    pub depends_on: Vec<String>,
    /// 计算顺序。
    pub sort_order: i32,
    /// 借方科目。
    pub debit_account_code: Option<String>,
    /// 贷方科目。
    pub credit_account_code: Option<String>,
    /// 是否计入应税收入。
    pub is_taxable: bool,
    /// 是否启用。
    pub is_active: bool,
    /// 系统项（不可删 / 不可停用）。
    pub is_system: bool,
    /// 备注。
    pub remark: Option<String>,
    /// 创建人 id。
    pub create_by: Option<String>,
    /// 创建人姓名（join 实时取）。
    pub create_by_name: Option<String>,
    /// 最后修改人 id。
    pub update_by: Option<String>,
    /// 最后修改人姓名。
    pub update_by_name: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl SalaryItemRes {
    /// 行 → 回执（`depends_on` 顺手抽出来，前端不用自己解析公式）。
    pub fn from_row(row: &crate::business::hr::salary_item_model::SalaryItemRow) -> Self {
        let depends_on = row
            .formula
            .as_deref()
            .and_then(|text| formula::extract_refs(text).ok())
            .map(|refs| {
                refs.into_iter()
                    .filter(|name| formula::att_var_label(name).is_none())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id: row.id.to_string(),
            item_code: row.item_code.clone(),
            item_name: row.item_name.clone(),
            item_type_label: label_of(&ITEM_TYPES, &row.item_type),
            calc_type_label: label_of(&CALC_TYPES, &row.calc_type),
            source_label: label_of(&SOURCES, &row.source),
            item_type: row.item_type.clone(),
            calc_type: row.calc_type.clone(),
            source: row.source.clone(),
            formula: row.formula.clone(),
            depends_on,
            sort_order: row.sort_order,
            debit_account_code: row.debit_account_code.clone(),
            credit_account_code: row.credit_account_code.clone(),
            is_taxable: row.is_taxable == 1,
            is_active: row.is_active == 1,
            is_system: row.is_system == 1,
            remark: row.remark.clone(),
            create_by: row.create_by.map(|id| id.to_string()),
            create_by_name: row.create_by_name.clone(),
            update_by: row.update_by.map(|id| id.to_string()),
            update_by_name: row.update_by_name.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 错误原语 ====================

/// `400 + 10001`：入参不合法。
pub fn param_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 路径参数里的 uuid。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    Uuid::parse_str(text).map_err(|_| param_invalid(format!("{field} 必须是合法的 uuid（收到 `{text}`）")))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// `V0383` 建表原文（编译期嵌入）：列宽 / 域值 / CHECK 名逐条对拍。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn formula_req(formula: &str) -> SalaryItemWriteReq {
        SalaryItemWriteReq {
            item_code: Some("OT_WD".to_string()),
            item_name: Some("平时加班费".to_string()),
            item_type: Some(ITEM_TYPE_EARNING.to_string()),
            calc_type: Some(CALC_TYPE_FORMULA.to_string()),
            formula: Some(formula.to_string()),
            sort_order: Some(50),
            debit_account_code: Some("5602.10".to_string()),
            credit_account_code: Some("2211.01".to_string()),
            ..SalaryItemWriteReq::default()
        }
    }

    // ---- 验收项：域值 / 列宽与迁移对拍 ----

    /// 域值与 `V0383` 的 CHECK 列表逐字一致（多一个 / 少一个都红）。
    #[test]
    fn domains_match_v0383_checks() {
        for (value, _) in ITEM_TYPES {
            assert!(
                V0383_DDL.contains(&format!("'{value}'")),
                "item_type `{value}` 不在 V0383 里"
            );
        }
        for (value, _) in CALC_TYPES {
            assert!(
                V0383_DDL.contains(&format!("'{value}'")),
                "calc_type `{value}` 不在 V0383 里"
            );
        }
        for (value, _) in SOURCES {
            assert!(
                V0383_DDL.contains(&format!("'{value}'")),
                "source `{value}` 不在 V0383 里"
            );
        }
        assert!(V0383_DDL.contains(&format!("formula               VARCHAR({MAX_FORMULA_LEN})")));
        assert!(V0383_DDL.contains(&format!("item_name             VARCHAR({MAX_ITEM_NAME_LEN})")));
        assert!(V0383_DDL.contains(&format!("item_code             VARCHAR({MAX_ITEM_CODE_LEN})")));
        assert!(V0383_DDL.contains(&format!("debit_account_code    VARCHAR({MAX_ACCOUNT_CODE_LEN})")));
        assert!(V0383_DDL.contains("uk_biz_sal_item_code"));
        assert!(V0383_DDL.contains("uk_biz_sal_item_sort"));
    }

    /// 设计 §4.4.3 的种子公式：每一条都必须**过得了保存校验**（否则 HR-35 的种子迁移会被自己挡住）。
    #[test]
    fn design_seed_formulas_pass_write_validation() {
        let cases: [(&str, &str, &str); 6] = [
            (
                "OT_WD",
                ITEM_TYPE_EARNING,
                "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)",
            ),
            (
                "ATT_BONUS",
                ITEM_TYPE_EARNING,
                "IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)",
            ),
            (
                "ATT_DEDUCT",
                ITEM_TYPE_PRE_DEDUCT,
                "ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)",
            ),
            (
                "GROSS",
                ITEM_TYPE_SUMMARY,
                "{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{ATT_BONUS}-{ATT_DEDUCT}",
            ),
            (
                "NET",
                ITEM_TYPE_SUMMARY,
                "{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}",
            ),
            ("IIT", ITEM_TYPE_WITHHOLD, "{GROSS} * 0.03"),
        ];
        // 设计 §4.4.3 的 17 个种子项（本项自己会被 check_formula 按 self_code 跳过）
        let existing: Vec<SalaryItemSnapshot> = [
            "BASE",
            "POST",
            "MEAL",
            "PERF",
            "OT_WD",
            "OT_WE",
            "OT_HO",
            "ATT_BONUS",
            "ATT_DEDUCT",
            "SI_PERSONAL",
            "HF_PERSONAL",
            "IIT",
            "OTHER_WITHHOLD",
            "SI_COMPANY",
            "HF_COMPANY",
            "GROSS",
            "NET",
        ]
        .iter()
        .map(|code| SalaryItemSnapshot {
            item_code: (*code).to_string(),
            formula: None,
            is_active: 1,
        })
        .collect();
        assert_eq!(existing.len(), 17, "设计 §4.4.3 是 15 个计算项 + 2 个汇总项");
        for (code, item_type, text) in cases {
            let mut req = formula_req(text);
            req.item_code = Some(code.to_string());
            req.item_type = Some(item_type.to_string());
            if item_type == ITEM_TYPE_SUMMARY {
                req.debit_account_code = None;
                req.credit_account_code = None;
            }
            if item_type == ITEM_TYPE_WITHHOLD {
                req.debit_account_code = None;
            }
            let draft = parse_write(&req, code, 10, None)
                .unwrap_or_else(|error| panic!("设计种子公式 {code} 被写接口挡住：{}", error.msg));
            let check = check_formula(&draft.formula.clone().unwrap_or_default(), Some(code), &existing);
            assert!(check.ok(), "{code} 语义检查未过：{:?}", check.errors);
        }
    }

    // ---- 验收项：公式字段长度校验 ----

    /// 500 字符通过 / 501 字符被拒，报文点名「最长 500」。
    #[test]
    fn formula_length_is_enforced_on_write() {
        let formula_500 = format!("{}+1", "1".repeat(MAX_FORMULA_LEN - 2));
        assert_eq!(formula_500.chars().count(), MAX_FORMULA_LEN);
        let req = formula_req(&formula_500);
        assert!(parse_write(&req, "OT_WD", 50, None).is_ok(), "500 字符必须通过");

        let formula_501 = format!("{}+11", "1".repeat(MAX_FORMULA_LEN - 2));
        assert_eq!(formula_501.chars().count(), MAX_FORMULA_LEN + 1);
        let error = parse_write(&formula_req(&formula_501), "OT_WD", 50, None).expect_err("501 必须被拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("最长 500"), "{}", error.msg);
    }

    /// 非 FORMULA 项填了公式 → 拒；FORMULA 项不填 → 拒。
    #[test]
    fn formula_is_required_exactly_for_formula_calc_type() {
        let mut req = formula_req("");
        req.formula = None;
        let error = parse_write(&req, "OT_WD", 50, None).expect_err("FORMULA 必须有公式");
        assert!(error.msg.contains("公式 formula 必填"), "{}", error.msg);

        let mut fixed = formula_req("{BASE} + 1");
        fixed.calc_type = Some(CALC_TYPE_FIXED.to_string());
        let error = parse_write(&fixed, "BASE", 10, None).expect_err("非 FORMULA 不许带公式");
        assert!(error.msg.contains("必须留空"), "{}", error.msg);
    }

    /// 编码口径：必须大写、不许与考勤变量同名、≤32。
    #[test]
    fn item_code_shape_is_enforced() {
        let mut req = formula_req("{BASE}");
        req.item_code = Some("ot_wd".to_string());
        let error = parse_write(&req, "ot_wd", 50, None).expect_err("小写编码必须拒");
        assert!(
            error.msg.contains("不合法") && error.msg.contains("大写"),
            "{}",
            error.msg
        );

        let error = parse_write(&req, "ATT_OT_WD", 50, None).expect_err("与考勤变量同名必须拒");
        assert!(error.msg.contains("与考勤变量同名"), "{}", error.msg);

        // `ATT_` 开头但不撞白名单的编码是**合法**的（设计 §4.4.3 的种子就有 ATT_BONUS / ATT_DEDUCT）
        assert!(parse_write(&req, "ATT_BONUS", 50, None).is_ok(), "ATT_BONUS 必须合法");

        let long_code = "A".repeat(MAX_ITEM_CODE_LEN + 1);
        let error = parse_write(&req, &long_code, 50, None).expect_err("超长编码必须拒");
        assert!(error.msg.contains("最长 32"), "{}", error.msg);

        assert!(parse_write(&req, "OT_WD_2", 50, None).is_ok(), "合法编码必须过");
    }

    /// 科目按类型分档：EARNING 要借+贷、WITHHOLD 只要贷、SUMMARY 都不要。
    #[test]
    fn accounts_follow_item_type() {
        let mut earning = formula_req("{BASE} + 1");
        earning.calc_type = Some(CALC_TYPE_FIXED.to_string());
        earning.formula = None;
        earning.debit_account_code = None;
        let error = parse_write(&earning, "BASE", 10, None).expect_err("EARNING 缺借方必须拒");
        assert!(error.msg.contains("必须给借方科目"), "{}", error.msg);

        let mut withhold = formula_req("{GROSS} * 0.03");
        withhold.item_type = Some(ITEM_TYPE_WITHHOLD.to_string());
        let error = parse_write(&withhold, "IIT", 120, None).expect_err("WITHHOLD 不许填借方");
        assert!(error.msg.contains("只填贷方科目"), "{}", error.msg);
        withhold.debit_account_code = None;
        let draft = parse_write(&withhold, "IIT", 120, None).expect("WITHHOLD 只填贷方");
        assert_eq!(draft.credit_account_code.as_deref(), Some("2211.01"));
        assert_eq!(draft.debit_account_code, None);

        let mut summary = formula_req("{BASE}+{POST}");
        summary.item_type = Some(ITEM_TYPE_SUMMARY.to_string());
        let error = parse_write(&summary, "GROSS", 160, None).expect_err("SUMMARY 不许填科目");
        assert!(error.msg.contains("不填科目"), "{}", error.msg);
        summary.debit_account_code = None;
        summary.credit_account_code = None;
        assert!(parse_write(&summary, "GROSS", 160, None).is_ok());
    }

    /// 汇总项必须用公式（`SUMMARY` + `INPUT` 是「没法算的项」）。
    #[test]
    fn summary_requires_formula_calc_type() {
        let mut req = formula_req("{BASE}+{POST}");
        req.item_type = Some(ITEM_TYPE_SUMMARY.to_string());
        req.calc_type = Some(CALC_TYPE_INPUT.to_string());
        req.debit_account_code = None;
        req.credit_account_code = None;
        let error = parse_write(&req, "GROSS", 160, None).expect_err("SUMMARY+INPUT 必须拒");
        assert!(error.msg.contains("必须用 `FORMULA`"), "{}", error.msg);
    }

    /// 两个开关：新建缺省 true；修改缺省保持原值（不传 ≠ 停用）。
    #[test]
    fn flags_default_to_true_on_create_and_keep_on_update() {
        let draft = parse_write(&formula_req("{BASE}"), "OT_WD", 50, None).expect("新建");
        assert_eq!(draft.is_active, 1);
        assert_eq!(draft.is_taxable, 1);

        let kept = parse_write(
            &formula_req("{BASE}"),
            "OT_WD",
            50,
            Some(ExistingFlags {
                is_taxable: 0,
                is_active: 0,
            }),
        )
        .expect("修改");
        assert_eq!(kept.is_active, 0, "不传 is_active 要保持原值 0");
        assert_eq!(kept.is_taxable, 0);

        let mut req = formula_req("{BASE}");
        req.is_active = Some(false);
        let off = parse_write(&req, "OT_WD", 50, None).expect("停用");
        assert_eq!(off.is_active, 0);
    }

    /// 备注 / 名称 / 排序的边界。
    #[test]
    fn text_and_sort_boundaries() {
        let mut req = formula_req("{BASE}");
        req.item_name = Some("名".repeat(MAX_ITEM_NAME_LEN + 1));
        let error = parse_write(&req, "OT_WD", 50, None).expect_err("名称超长");
        assert!(error.msg.contains("最长 50"), "{}", error.msg);

        let mut req = formula_req("{BASE}");
        req.remark = Some("备".repeat(MAX_REMARK_LEN + 1));
        let error = parse_write(&req, "OT_WD", 50, None).expect_err("备注超长");
        assert!(error.msg.contains("最长 500"), "{}", error.msg);

        let mut req = formula_req("{BASE}");
        req.sort_order = Some(0);
        let error = parse_write(&req, "OT_WD", 50, None).expect_err("排序 0");
        assert!(error.msg.contains("sort_order 必须在 1~"), "{}", error.msg);
    }

    // ---- 语义层：未定义变量 / 环 / 拓扑 ----

    /// 拼错编码 → 未定义变量被点名，且给可用清单（设计 §6 的报文口径）。
    #[test]
    fn undefined_variable_is_named() {
        let existing = vec![
            SalaryItemSnapshot {
                item_code: "BASE".to_string(),
                formula: None,
                is_active: 1,
            },
            SalaryItemSnapshot {
                item_code: "POST".to_string(),
                formula: None,
                is_active: 1,
            },
        ];
        let check = check_formula("ROUND({BASEE} / 21.75, 2)", Some("OT_WD"), &existing);
        assert!(!check.ok());
        assert_eq!(check.undefined, vec!["BASEE".to_string()]);
        assert!(
            check
                .errors
                .iter()
                .any(|m| m.contains("`{BASEE}`") && m.contains("BASE")),
            "{:?}",
            check.errors
        );
    }

    /// 停用的项不算可引用（否则公式引用了「已停用」的项，试算静默算 0）。
    #[test]
    fn inactive_item_is_not_referencable() {
        let existing = vec![SalaryItemSnapshot {
            item_code: "OLD".to_string(),
            formula: None,
            is_active: 0,
        }];
        let check = check_formula("{OLD} + 1", Some("NEW"), &existing);
        assert_eq!(check.undefined, vec!["OLD".to_string()]);
    }

    /// 互引成环被点名，且拓扑序为空。
    #[test]
    fn cycle_is_detected() {
        let existing = vec![SalaryItemSnapshot {
            item_code: "BASE".to_string(),
            formula: Some("{OT_WD} * 2".to_string()),
            is_active: 1,
        }];
        let check = check_formula("{BASE} + 1", Some("OT_WD"), &existing);
        assert!(!check.ok());
        let cycle = check.cycle.expect("必须报环");
        assert_eq!(cycle.first(), cycle.last());
        assert!(check.dependency_order.is_empty());
        assert!(
            check.errors.iter().any(|m| m.contains("循环依赖")),
            "{:?}",
            check.errors
        );
    }

    /// 自引用是环（`A = {A} + 1`）。
    #[test]
    fn self_reference_is_a_cycle() {
        let check = check_formula("{BASE} + 1", Some("BASE"), &[]);
        assert!(!check.ok());
        assert!(check.cycle.is_some());
        assert!(
            check.undefined.is_empty(),
            "自引用不该被报成未定义变量：{:?}",
            check.undefined
        );
    }

    /// 拓扑序只列「本项 + 依赖闭包」，依赖在前、本项在末位（设计 §6 的 `BASE → OT_WD → GROSS`）。
    #[test]
    fn dependency_order_is_the_closure_ending_with_self() {
        let existing = vec![
            SalaryItemSnapshot {
                item_code: "BASE".to_string(),
                formula: None,
                is_active: 1,
            },
            SalaryItemSnapshot {
                item_code: "OT_WD".to_string(),
                formula: Some("ROUND({BASE} / 21.75, 2)".to_string()),
                is_active: 1,
            },
            SalaryItemSnapshot {
                item_code: "PERF".to_string(),
                formula: None,
                is_active: 1,
            },
        ];
        let check = check_formula("{OT_WD} + {PERF}", Some("GROSS"), &existing);
        assert!(check.ok(), "{:?}", check.errors);
        // 拓扑序稳定：同为「无依赖」的项按输入（= 库里的 sort_order）先后，依赖永远在前、本项在末位
        assert_eq!(
            check.dependency_order,
            vec![
                "BASE".to_string(),
                "PERF".to_string(),
                "OT_WD".to_string(),
                "GROSS".to_string()
            ]
        );
        let at = |code: &str| {
            check
                .dependency_order
                .iter()
                .position(|item| item == code)
                .unwrap_or(usize::MAX)
        };
        assert!(at("BASE") < at("OT_WD"), "依赖在前：{:?}", check.dependency_order);
        assert_eq!(at("GROSS"), check.dependency_order.len() - 1, "本项在末位");
        assert_eq!(check.item_refs, vec!["OT_WD".to_string(), "PERF".to_string()]);
        assert!(check.att_refs.is_empty());
    }

    /// 考勤变量算「可达」但不算「薪资项引用」。
    #[test]
    fn attendance_vars_are_split_from_item_refs() {
        let check = check_formula(
            "ROUND({BASE} / 21.75 * {ATT_OT_WD}, 2)",
            Some("OT_WD"),
            &[SalaryItemSnapshot {
                item_code: "BASE".to_string(),
                formula: None,
                is_active: 1,
            }],
        );
        assert!(check.ok(), "{:?}", check.errors);
        assert_eq!(check.item_refs, vec!["BASE".to_string()]);
        assert_eq!(check.att_refs, vec!["ATT_OT_WD".to_string()]);
    }

    /// 历史脏数据（别的项公式抽引用失败）不该挡住本项检查。
    #[test]
    fn broken_sibling_formula_does_not_block_check() {
        let existing = vec![
            SalaryItemSnapshot {
                item_code: "BASE".to_string(),
                formula: None,
                is_active: 1,
            },
            SalaryItemSnapshot {
                item_code: "BROKEN".to_string(),
                formula: Some("{UNCLOSED".to_string()),
                is_active: 1,
            },
        ];
        let check = check_formula("{BASE} + 1", Some("OT_WD"), &existing);
        assert!(check.ok(), "{:?}", check.errors);
    }

    /// 可用变量清单 = 全部薪资项（含停用，标 unusable）+ 11 个考勤变量。
    #[test]
    fn available_vars_lists_items_and_attendance() {
        let existing = vec![
            SalaryItemSnapshot {
                item_code: "BASE".to_string(),
                formula: None,
                is_active: 1,
            },
            SalaryItemSnapshot {
                item_code: "OLD".to_string(),
                formula: None,
                is_active: 0,
            },
        ];
        let vars = available_vars(&existing);
        assert_eq!(vars.len(), 2 + formula::ATT_VARS.len());
        assert!(vars.iter().any(|v| v.name == "BASE" && v.usable && v.kind == "ITEM"));
        assert!(vars.iter().any(|v| v.name == "OLD" && !v.usable));
        assert!(vars.iter().any(|v| v.name == "ATT_OT_WD" && v.label == "平时加班小时"));
    }

    /// 列表过滤：域值非法报 400（不静默忽略），关键字 trim，is_active 只认 0/1。
    #[test]
    fn filter_of_validates_domains() {
        let query = SalaryItemPageQuery {
            keyword: Some("  加班  ".to_string()),
            item_type: Some("EARNING".to_string()),
            ..SalaryItemPageQuery::default()
        };
        let filter = filter_of(&query).expect("合法");
        assert_eq!(filter.keyword.as_deref(), Some("加班"));
        assert_eq!(filter.item_type.as_deref(), Some("EARNING"));

        let bad_type = SalaryItemPageQuery {
            item_type: Some("EARNINGS".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&bad_type).expect_err("域外类型").status, 400);

        let bad_flag = SalaryItemPageQuery {
            is_active: Some(2),
            ..Default::default()
        };
        assert_eq!(filter_of(&bad_flag).expect_err("is_active 只认 0/1").status, 400);
    }

    /// 分页归一化。
    #[test]
    fn page_normalizes() {
        assert_eq!(normalize_page(None, None), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(Some(0), Some(0)), (1, 1));
        assert_eq!(normalize_page(Some(3), Some(9_999)), (3, MAX_PAGE_SIZE));
    }
}
