//! 导入 / 导出定义（业务侧接入契约）。
//!
//! 业务方只需实现 [`ImportDefinition`]（导入）与 [`ExportDefinition`]（导出），
//! 就自动拥有「模板下载 + 结构校验 + 逐行校验 + 错误明细 + 入库 + 同步导出」全套能力；
//! 模板表头、列映射、类型转换、必填、枚举 / 字典翻译、文件内唯一、外键存在性全部由本
//! 模块按定义驱动，业务代码里不出现 Excel 相关逻辑。
//!
//! # 列定义的三层语义
//!
//! | 层 | 归属 | 谁校验 |
//! |----|------|--------|
//! | 结构（表头缺列 / 多列 / 顺序） | [`crate::business::impexp::engine`] | 框架（E-IMP-02/03/04/05） |
//! | 单元格（必填 / 类型 / 长度 / 枚举 / 字典 / 范围） | 本文件的 [`ColumnKind::parse`] | 框架（逐行） |
//! | 业务规则（跨列约束、库内唯一等） | 业务 `validate_row` | 业务方 |
//!
//! # 行号约定
//!
//! 全部错误的行号都是 **Excel 里看到的真实行号**（第 1 行是表头，从 1 开始计数），
//! 与 PRD §4.2.2 一致；不是「数据第 N 条」。

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use nik::prelude::*;

use crate::shared::error_code;

// ==================== 错误分类 ====================

/// 校验错误的分类（落库 `sys_import_task_error.error_code`）。
///
/// 与 PRD §4.2.3 的 E-IMP 编号一一对应，明细表按此字段做 `GROUP BY` 统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorKind {
    /// 必填项为空（E-IMP-08）。
    Required,
    /// 类型不符（E-IMP-09）。
    Type,
    /// 超长（E-IMP-10）。
    Length,
    /// 枚举值非法（E-IMP-11）。
    Enum,
    /// 数值越界（E-IMP-12）。
    Range,
    /// 日期格式错（E-IMP-16）。
    Date,
    /// 字典值非法（E-IMP-11 的字典变体）。
    Dict,
    /// 文件内重复（E-IMP-13）。
    Unique,
    /// 外键不存在（E-IMP-15）。
    FkNotFound,
    /// 业务规则（`validate_row` 返回）。
    Rule,
    /// 超出单次行数上限（E-IMP-06）。
    RowLimit,
    /// 结构问题（表头缺列等，整表级）。
    Structure,
}

impl ErrorKind {
    /// 落库用的错误码字符串。
    ///
    /// # 返回值
    ///
    /// 全大写短码，如 `REQUIRED` / `FK_NOT_FOUND`。
    pub const fn as_str(&self) -> &'static str {
        match self {
            ErrorKind::Required => "REQUIRED",
            ErrorKind::Type => "TYPE",
            ErrorKind::Length => "LENGTH",
            ErrorKind::Enum => "ENUM",
            ErrorKind::Range => "RANGE",
            ErrorKind::Date => "DATE",
            ErrorKind::Dict => "DICT",
            ErrorKind::Unique => "UNIQUE",
            ErrorKind::FkNotFound => "FK_NOT_FOUND",
            ErrorKind::Rule => "RULE",
            ErrorKind::RowLimit => "ROW_LIMIT",
            ErrorKind::Structure => "STRUCTURE",
        }
    }
}

// ==================== 列类型 ====================

/// 枚举列的一个取值：Excel 里写 `label`，库里存 `code`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumItem {
    /// 中文展示值（模板下拉与导出回显用）。
    pub label: &'static str,
    /// 落库码值。
    pub code: &'static str,
}

/// 列的数据类型（决定模板说明、解析规则与导出格式化）。
#[derive(Debug, Clone, Copy)]
pub enum ColumnKind {
    /// 文本，可选长度上限（字符数，按 `chars().count()` 计）。
    Text {
        /// 最大字符数；`None` 表示不限制。
        max_len: Option<usize>,
    },
    /// 整数，可选闭区间。
    Int {
        /// 下界。
        min: Option<i64>,
        /// 上界。
        max: Option<i64>,
    },
    /// 小数，指定小数位数（超出位数报错，不静默截断）。
    Decimal {
        /// 允许的最大小数位数。
        scale: u32,
        /// 下界。
        min: Option<f64>,
        /// 上界。
        max: Option<f64>,
    },
    /// 布尔（是/否、true/false、1/0、y/n）。
    Bool,
    /// 日期（`YYYY-MM-DD` / `YYYY/MM/DD` / `YYYY.MM.DD` / Excel 日期序列号）。
    Date,
    /// 日期时间（日期后接 `HH:MM` 或 `HH:MM:SS`）。
    DateTime,
    /// 固定枚举（中文值 ↔ 码值）。
    Enum(&'static [EnumItem]),
    /// 字典（`sys_dict_type.dict_code`，label → value）。
    Dict(&'static str),
}

/// 字典翻译表：一次装载，进程内缓存（见 `validate::DictCache`）。
#[derive(Debug, Default, Clone)]
pub struct DictTranslation {
    /// 展示值 → 落库值。
    pub label_to_code: HashMap<String, String>,
    /// 落库值集合（判定「写的是 code 还是 label」）。
    pub code_set: HashSet<String>,
    /// 落库值 → 展示值（导出用）。
    pub code_to_label: HashMap<String, String>,
}

impl DictTranslation {
    /// 由 `(value, label)` 列表构造翻译表。
    ///
    /// # 参数
    ///
    /// - `items`：字典项 `(item_value, item_label)`，顺序无要求。
    ///
    /// # 返回值
    ///
    /// 双向翻译表；重复 value 保留首次出现（与数据库部分唯一索引语义一致）。
    pub fn from_items(items: Vec<(String, String)>) -> Self {
        let mut label_to_code = HashMap::new();
        let mut code_set = HashSet::new();
        let mut code_to_label = HashMap::new();
        for (code, label) in items {
            label_to_code
                .entry(label.clone())
                .or_insert_with(|| code.clone());
            code_to_label.entry(code.clone()).or_insert(label);
            code_set.insert(code);
        }
        Self {
            label_to_code,
            code_set,
            code_to_label,
        }
    }

    /// 命中翻译表的落库值（label 或 code 都认）。
    ///
    /// # 参数
    ///
    /// - `raw`：单元格原文（**已 trim**）。
    ///
    /// # 返回值
    ///
    /// 命中返回落库值，否则返回 `None`。
    pub fn resolve(&self, raw: &str) -> Option<&str> {
        if let Some(code) = self.label_to_code.get(raw) {
            return Some(code.as_str());
        }
        self.code_set.get(raw).map(|code| code.as_str())
    }
}

/// 单元格解析失败的原因。
#[derive(Debug, Clone)]
pub struct FieldIssue {
    /// 错误分类。
    pub kind: ErrorKind,
    /// 面向用户的文案（PRD 附录 A 口径）。
    pub message: String,
}

impl FieldIssue {
    /// 构造一个单元格级问题。
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl ColumnKind {
    /// 类型的中文名（模板「字段说明」sheet 用）。
    ///
    /// # 返回值
    ///
    /// 如 `文本` / `整数` / `小数` / `日期` / `枚举` / `字典`。
    pub const fn type_label(&self) -> &'static str {
        match self {
            ColumnKind::Text { .. } => "文本",
            ColumnKind::Int { .. } => "整数",
            ColumnKind::Decimal { .. } => "小数",
            ColumnKind::Bool => "布尔",
            ColumnKind::Date => "日期",
            ColumnKind::DateTime => "日期时间",
            ColumnKind::Enum(_) => "枚举",
            ColumnKind::Dict(_) => "字典",
        }
    }

    /// 长度 / 范围说明（模板「字段说明」sheet 用）。
    ///
    /// # 返回值
    ///
    /// 如 `≤32 个字符`、`0–9999`、`YYYY-MM-DD`、`是 / 否`。
    pub fn rule_text(&self) -> String {
        match self {
            ColumnKind::Text { max_len: Some(max) } => format!("≤{max} 个字符"),
            ColumnKind::Text { max_len: None } => "不限长度".to_string(),
            ColumnKind::Int { min, max } => format_range(
                min.map(|v| v.to_string()),
                max.map(|v| v.to_string()),
                "整数",
            ),
            ColumnKind::Decimal { scale, min, max } => {
                let base = format_range(
                    min.map(|v| v.to_string()),
                    max.map(|v| v.to_string()),
                    "小数",
                );
                format!("{base}，最多 {scale} 位小数")
            }
            ColumnKind::Bool => "是 / 否".to_string(),
            ColumnKind::Date => "YYYY-MM-DD".to_string(),
            ColumnKind::DateTime => "YYYY-MM-DD HH:MM[:SS]".to_string(),
            ColumnKind::Enum(items) => items
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>()
                .join(" / "),
            ColumnKind::Dict(code) => format!("字典 {code} 的取值"),
        }
    }

    /// 可选值说明（枚举列列出全部中文值，字典列给出字典编码）。
    ///
    /// # 返回值
    ///
    /// 枚举列如 `启用 / 停用`；其它类型返回空串。
    pub fn option_text(&self) -> String {
        match self {
            ColumnKind::Enum(items) => items
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>()
                .join(" / "),
            ColumnKind::Dict(code) => format!("字典 {code}"),
            ColumnKind::Bool => "是 / 否".to_string(),
            _ => String::new(),
        }
    }

    /// 解析单元格原文为**落库值**（已经是规范形式）。
    ///
    /// # 参数
    ///
    /// - `raw`：单元格原文（内部会 trim，含全角空格）。
    /// - `dict`：该列若为 [`ColumnKind::Dict`]，需给出对应字典翻译表。
    ///
    /// # 返回值
    ///
    /// - 空值返回 `Ok(String::new())`（必填判定由调用方做，保证「空」只报 REQUIRED 一条）；
    /// - 合法值返回规范化后的落库字符串。
    ///
    /// # 异常情况
    ///
    /// 类型 / 长度 / 范围 / 枚举 / 字典 / 日期不合法时返回 [`FieldIssue`]。
    pub fn parse(
        &self,
        raw: &str,
        dict: Option<&DictTranslation>,
    ) -> std::result::Result<String, FieldIssue> {
        let text = raw.trim();
        if text.is_empty() {
            return Ok(String::new());
        }
        match self {
            ColumnKind::Text { max_len } => {
                if let Some(max) = max_len {
                    let actual = text.chars().count();
                    if actual > *max {
                        return Err(FieldIssue::new(
                            ErrorKind::Length,
                            format!("长度超限（最多 {max} 个字符，实际 {actual} 个）"),
                        ));
                    }
                }
                Ok(text.to_string())
            }
            ColumnKind::Int { min, max } => {
                let value = text.parse::<i64>().map_err(|_| {
                    FieldIssue::new(ErrorKind::Type, format!("应为整数，实际为「{text}」"))
                })?;
                if let Some(low) = min
                    && value < *low
                {
                    return Err(FieldIssue::new(
                        ErrorKind::Range,
                        format!("数值超出范围（{}，实际 {value}）", self.rule_text()),
                    ));
                }
                if let Some(high) = max
                    && value > *high
                {
                    return Err(FieldIssue::new(
                        ErrorKind::Range,
                        format!("数值超出范围（{}，实际 {value}）", self.rule_text()),
                    ));
                }
                Ok(value.to_string())
            }
            ColumnKind::Decimal { scale, min, max } => {
                let value = text.parse::<f64>().map_err(|_| {
                    FieldIssue::new(ErrorKind::Type, format!("应为数字，实际为「{text}」"))
                })?;
                if !value.is_finite() {
                    return Err(FieldIssue::new(
                        ErrorKind::Type,
                        format!("应为数字，实际为「{text}」"),
                    ));
                }
                let actual_scale = text
                    .split_once('.')
                    .map(|(_, frac)| frac.trim_end_matches('0').chars().count())
                    .unwrap_or(0);
                if actual_scale > *scale as usize {
                    return Err(FieldIssue::new(
                        ErrorKind::Range,
                        format!("小数位数超限（最多 {scale} 位，实际 {actual_scale} 位）"),
                    ));
                }
                if let Some(low) = min
                    && value < *low
                {
                    return Err(FieldIssue::new(
                        ErrorKind::Range,
                        format!("数值超出范围（{}，实际 {value}）", self.rule_text()),
                    ));
                }
                if let Some(high) = max
                    && value > *high
                {
                    return Err(FieldIssue::new(
                        ErrorKind::Range,
                        format!("数值超出范围（{}，实际 {value}）", self.rule_text()),
                    ));
                }
                Ok(text.to_string())
            }
            ColumnKind::Bool => match text.to_ascii_lowercase().as_str() {
                "是" | "true" | "1" | "y" | "yes" | "真" => Ok("true".to_string()),
                "否" | "false" | "0" | "n" | "no" | "假" => Ok("false".to_string()),
                _ => Err(FieldIssue::new(
                    ErrorKind::Type,
                    format!("应为是/否，实际为「{text}」"),
                )),
            },
            ColumnKind::Date => {
                let date = parse_date(text).ok_or_else(|| {
                    FieldIssue::new(
                        ErrorKind::Date,
                        format!("日期格式不正确（应为 YYYY-MM-DD，实际 {text}）"),
                    )
                })?;
                Ok(date.format("%Y-%m-%d").to_string())
            }
            ColumnKind::DateTime => {
                let value = parse_datetime(text).ok_or_else(|| {
                    FieldIssue::new(
                        ErrorKind::Date,
                        format!("日期时间格式不正确（应为 YYYY-MM-DD HH:MM:SS，实际 {text}）"),
                    )
                })?;
                Ok(value.format("%Y-%m-%d %H:%M:%S").to_string())
            }
            ColumnKind::Enum(items) => {
                if let Some(item) = items.iter().find(|item| item.label == text) {
                    return Ok(item.code.to_string());
                }
                if let Some(item) = items.iter().find(|item| item.code == text) {
                    return Ok(item.code.to_string());
                }
                let options = items
                    .iter()
                    .map(|item| item.label)
                    .collect::<Vec<_>>()
                    .join(" / ");
                Err(FieldIssue::new(
                    ErrorKind::Enum,
                    format!("值「{text}」不在可选范围内（{options}）"),
                ))
            }
            ColumnKind::Dict(code) => {
                let Some(dict) = dict else {
                    // 字典未装载属于框架侧问题，按业务规则报错而不是当成「值非法」，
                    // 避免用户看到一个查不到原因的「字典值非法」。
                    return Err(FieldIssue::new(
                        ErrorKind::Rule,
                        format!("字典 {code} 未装载，请稍后重试"),
                    ));
                };
                dict.resolve(text).map(str::to_string).ok_or_else(|| {
                    FieldIssue::new(ErrorKind::Dict, format!("值「{text}」不在字典 {code} 中"))
                })
            }
        }
    }

    /// 把库里的值格式化成 Excel 展示值（导出用）。
    ///
    /// # 参数
    ///
    /// - `stored`：库中值。
    /// - `dict`：字典列需给出翻译表。
    ///
    /// # 返回值
    ///
    /// 展示字符串；布尔转「是/否」，枚举 / 字典转中文，其余原样。
    pub fn format(&self, stored: &str, dict: Option<&DictTranslation>) -> String {
        if stored.is_empty() {
            return String::new();
        }
        match self {
            ColumnKind::Bool => match stored {
                "true" | "1" | "t" => "是".to_string(),
                "false" | "0" | "f" => "否".to_string(),
                other => other.to_string(),
            },
            ColumnKind::Enum(items) => items
                .iter()
                .find(|item| item.code == stored)
                .map(|item| item.label.to_string())
                .unwrap_or_else(|| stored.to_string()),
            ColumnKind::Dict(_) => dict
                .and_then(|dict| dict.code_to_label.get(stored))
                .cloned()
                .unwrap_or_else(|| stored.to_string()),
            ColumnKind::Date => stored
                .split_whitespace()
                .next()
                .unwrap_or(stored)
                .to_string(),
            _ => stored.to_string(),
        }
    }
}

/// 拼接范围说明（`0–9999` / `≥0` / `≤100` / `不限`）。
fn format_range(min: Option<String>, max: Option<String>, unit: &str) -> String {
    match (min, max) {
        (Some(low), Some(high)) => format!("{low}–{high}"),
        (Some(low), None) => format!("≥{low}"),
        (None, Some(high)) => format!("≤{high}"),
        (None, None) => format!("不限（{unit}）"),
    }
}

/// Excel 日期序列号 → 日期（1900 日期系统，纪元 1899-12-30）。
///
/// # 参数
///
/// - `serial`：Excel 内部存储的天数（含小数表示的时间）。
///
/// # 返回值
///
/// 合法范围内返回日期；越界（如 0、负数、超大）返回 `None`。
pub fn excel_serial_to_date(serial: f64) -> Option<NaiveDate> {
    if !serial.is_finite() || !(1.0..=2_958_465.0).contains(&serial) {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(1899, 12, 30)?;
    epoch.checked_add_days(chrono::Days::new(serial.floor() as u64))
}

/// Excel 日期时间序列号 → 日期时间（整数部分为天数、小数部分为当天时间）。
///
/// # 参数
///
/// - `serial`：Excel 内部存储值（如 `46303.5` = 2026-10-28 12:00:00）。
///
/// # 返回值
///
/// 合法范围内返回日期时间，越界返回 `None`。
pub fn excel_serial_to_datetime(serial: f64) -> Option<NaiveDateTime> {
    let date = excel_serial_to_date(serial)?;
    let seconds = ((serial - serial.floor()) * 86_400.0).round() as i64;
    date.and_hms_opt(0, 0, 0)?
        .checked_add_signed(chrono::Duration::seconds(seconds))
}

/// 解析日期文本：支持 `YYYY-MM-DD` / `YYYY/MM/DD` / `YYYY.MM.DD` 与 Excel 序列号。
///
/// # 参数
///
/// - `text`：已 trim 的原文。
///
/// # 返回值
///
/// 解析成功返回日期，失败返回 `None`。
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let normalized = text.replace(['/', '.'], "-");
    if let Ok(date) = NaiveDate::parse_from_str(&normalized, "%Y-%m-%d") {
        return Some(date);
    }
    // Excel 日期单元格在读侧会被转成序列号文本
    if let Ok(serial) = text.parse::<f64>() {
        return excel_serial_to_date(serial);
    }
    None
}

/// 解析日期时间文本：日期部分 + 可选 `HH:MM[:SS]`；纯日期按 `00:00:00` 处理。
///
/// # 参数
///
/// - `text`：已 trim 的原文。
///
/// # 返回值
///
/// 解析成功返回 `NaiveDateTime`，失败返回 `None`。
pub fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    let unified = text.replace(['/', '.', 'T'], "-");
    if let Ok(value) = NaiveDateTime::parse_from_str(&unified, "%Y-%m-%d %H:%M:%S") {
        return Some(value);
    }
    if let Ok(value) = NaiveDateTime::parse_from_str(&unified, "%Y-%m-%d %H:%M") {
        return Some(value);
    }
    // 纯日期
    if let Some(date) = parse_date(text) {
        return date.and_hms_opt(0, 0, 0);
    }
    // Excel 序列号（含小数时间）
    if let Ok(serial) = text.parse::<f64>() {
        return excel_serial_to_datetime(serial);
    }
    None
}

// ==================== 列定义 ====================

/// 外键存在性校验规格（只读底座 / 主数据表，禁止写业务表）。
#[derive(Debug, Clone, Copy)]
pub struct FkSpec {
    /// 被引用的表名（编译期常量，**不接受运行时用户输入**）。
    pub table: &'static str,
    /// 被引用的列名。
    pub column: &'static str,
    /// 展示列名（错误文案里用于说明「请先在某处维护」）。
    pub label: &'static str,
    /// 该表是否带软删列 `deleted_at`（带则只认未删除行）。
    pub soft_delete: bool,
}

/// 导入列定义：决定模板表头、解析类型、必填、字典翻译、唯一性、外键。
#[derive(Debug, Clone, Copy)]
pub struct ImportColumn {
    /// 字段名（解析后 `RowValues` 的键，也是错误明细的 `column_key`）。
    pub key: &'static str,
    /// 中文表头（模板第 1 行；必填列渲染时自动加 `*`）。
    pub header: &'static str,
    /// 是否必填。
    pub required: bool,
    /// 数据类型。
    pub kind: ColumnKind,
    /// 是否参与「文件内唯一」校验（多列同时为 `true` 时按组合唯一）。
    pub unique_key: bool,
    /// 外键存在性规格。
    pub fk: Option<FkSpec>,
    /// 模板示例值。
    pub example: &'static str,
    /// 列宽（字符宽度提示，当前写入器按内容自适应，保留给前端表格）。
    pub width: Option<u16>,
    /// 模板「字段说明」sheet 的补充说明。
    pub remark: &'static str,
}

impl ImportColumn {
    /// 模板表头（必填列带 `*`）。
    ///
    /// # 返回值
    ///
    /// 如 `物料编码*`。
    pub fn template_header(&self) -> String {
        if self.required {
            format!("{}*", self.header)
        } else {
            self.header.to_string()
        }
    }
}

/// 导出列定义。
#[derive(Debug, Clone, Copy)]
pub struct ExportColumn {
    /// 字段名（`ExportRow` 的键）。
    pub key: &'static str,
    /// 中文表头。
    pub header: &'static str,
    /// 数据类型（决定格式化：枚举 / 字典翻译、布尔转是/否、日期去掉时间）。
    pub kind: ColumnKind,
    /// 列宽提示。
    pub width: Option<u16>,
    /// **该列所属的表名**（批次 B · B6 新增）。
    ///
    /// 列级脱敏按 `(表, 列)` 查 `sys_field_mask`：没有表名就无法回答
    /// 「这一列该套哪条规则」（`real_name` 在 `sys_user` 是姓名，在别的表可能是别的语义）。
    /// 架构 §3.7 把它列为导出脱敏的前置硬约束，所以这里是**必填**而不是 `Option`——
    /// 漏填直接编译不过。
    pub table: &'static str,
    /// 是否敏感字段（`true` 时导出必须脱敏；没有生效规则 → 422 fail-closed）。
    pub sensitive: bool,
}

/// 一行已校验通过的数据（只保留落库值）。
#[derive(Debug, Clone, Default)]
pub struct RowValues {
    /// Excel 真实行号（≥2）。
    pub row_no: u32,
    /// 列键 → 规范化落库值。
    pub fields: HashMap<&'static str, String>,
}

impl RowValues {
    /// 取字段值，缺列返回空串。
    ///
    /// # 参数
    ///
    /// - `key`：列键。
    pub fn get(&self, key: &str) -> &str {
        self.fields.get(key).map(String::as_str).unwrap_or("")
    }
}

/// 一行的校验问题（落 `sys_import_task_error`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RowIssue {
    /// Excel 真实行号。
    pub row_no: u32,
    /// 列键（整行级问题为 `None`）。
    pub column_key: Option<&'static str>,
    /// 列中文名（整行级问题为 `None`）。
    pub column_header: Option<&'static str>,
    /// 触发问题的单元格原文（截断到 200 字符）。
    pub raw_value: Option<String>,
    /// 错误分类。
    pub kind: ErrorKind,
    /// 面向用户的文案。
    pub message: String,
}

impl RowIssue {
    /// 构造列级问题。
    pub fn column(row_no: u32, column: &ImportColumn, raw_value: &str, issue: FieldIssue) -> Self {
        Self {
            row_no,
            column_key: Some(column.key),
            column_header: Some(column.header),
            raw_value: Some(truncate(raw_value, 200)),
            kind: issue.kind,
            message: issue.message,
        }
    }

    /// 构造整行级问题（业务规则 / 行数上限等）。
    pub fn row(row_no: u32, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            row_no,
            column_key: None,
            column_header: None,
            raw_value: None,
            kind,
            message: message.into(),
        }
    }
}

/// 截断字符串到指定字符数（错误明细的 `raw_value` 上限 200）。
fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

/// 导入模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 全量事务：有任何失败行则整批不入库（默认，PRD D6）。
    Strict,
    /// 允许部分导入：跳过失败行，最终状态 `PARTIAL`。
    Partial,
}

impl ImportMode {
    /// 字符串 → 模式。
    ///
    /// # 参数
    ///
    /// - `raw`：`STRICT` / `PARTIAL`（大小写不敏感）。
    ///
    /// # 返回值
    ///
    /// 合法时返回模式，非法返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "STRICT" | "" => Some(ImportMode::Strict),
            "PARTIAL" => Some(ImportMode::Partial),
            _ => None,
        }
    }

    /// 落库字符串。
    pub const fn as_str(&self) -> &'static str {
        match self {
            ImportMode::Strict => "STRICT",
            ImportMode::Partial => "PARTIAL",
        }
    }
}

/// 重复数据策略（同一业务编码在库中已存在时怎么处理）。
///
/// 与 [`ImportMode`] 正交：`mode` 决定「校验失败的行怎么办」，本枚举决定
/// 「编码已存在的行怎么办」。二者共同构成一次导入的行为，故都由上传接口的查询串
/// 传入并快照到 `sys_import_task.options`（见 `handler::import_upload`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuplicateStrategy {
    /// 按业务编码 upsert：已存在则更新其余字段（默认，幂等重导的首选）。
    #[default]
    Upsert,
    /// 已存在则跳过（只插入新编码），用于「只补新增、绝不覆盖线上已有主数据」。
    Skip,
}

impl DuplicateStrategy {
    /// 字符串 → 策略（大小写不敏感，空串按默认）。
    ///
    /// # 参数
    ///
    /// - `raw`：`UPSERT` / `SKIP`。
    ///
    /// # 返回值
    ///
    /// 合法时返回策略，非法返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "" | "UPSERT" | "UPDATE" => Some(DuplicateStrategy::Upsert),
            "SKIP" | "IGNORE" => Some(DuplicateStrategy::Skip),
            _ => None,
        }
    }

    /// 落库字符串（写进 `sys_import_task.options.duplicate_strategy`）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            DuplicateStrategy::Upsert => "UPSERT",
            DuplicateStrategy::Skip => "SKIP",
        }
    }
}

/// 导入上下文（传给业务 `validate_row` / `persist`）。
#[derive(Debug, Clone)]
pub struct ImportCtx {
    /// 任务 ID。
    pub task_id: uuid::Uuid,
    /// 操作人。
    pub operator: Option<uuid::Uuid>,
    /// 导入模式。
    pub mode: ImportMode,
}

// ==================== 业务接入 trait ====================

/// 导入定义：业务方实现这一个 trait 即拥有整条导入链路。
#[async_trait]
pub trait ImportDefinition: Send + Sync {
    /// 业务编码，全局唯一（如 `system.dict_item`）。
    fn biz_type(&self) -> &'static str;
    /// 业务名称（模板文件名、页面展示用）。
    fn biz_name(&self) -> &'static str;
    /// 模板版本号（表头变更时递增）。
    fn template_version(&self) -> u32 {
        1
    }
    /// 单次导入行数上限（超过直接拒绝，防 OOM）。
    fn max_rows(&self) -> usize {
        200_000
    }
    /// 分批入库的批大小（每批一个事务）。
    fn batch_size(&self) -> usize {
        2_000
    }
    /// 是否允许部分导入（PRD D6 默认关闭）。
    fn allow_partial(&self) -> bool {
        false
    }
    /// 列定义。
    fn columns(&self) -> &'static [ImportColumn];
    /// 单行业务规则（跨列校验、库内唯一等）；返回 `Vec` 以支持一行多错。
    async fn validate_row(
        &self,
        _row: &RowValues,
        _ctx: &ImportCtx,
    ) -> std::result::Result<(), Vec<RowIssue>> {
        Ok(())
    }
    /// 批量入库：框架已在事务内，业务方只写 INSERT / UPDATE。
    async fn persist(
        &self,
        tx: &Transaction,
        rows: &[RowValues],
        ctx: &ImportCtx,
    ) -> std::result::Result<usize, DbError>;
}

/// 导出的过滤条件（由 handler 从查询串解析，业务方自解释）。
///
/// # 状态过滤的两种口径（卡 `t_93cba3f4`）
///
/// 状态有两个**互不相通**的表达方式，定义必须显式声明自己认哪一种；两样都不认的定义
/// （比如没有状态列的导出）收到任何状态过滤都会被 [`check_export_filter`] 判 **422**：
///
/// | 字段 | 语义 | 谁支持 | 落库形态 |
/// |------|------|--------|----------|
/// | [`Self::status`] | 启用 / 停用（`1` / `0`） | `system.dict_item` / `system.user` | `smallint` |
/// | [`Self::status_code`] | 单据状态文本码（`POSTED` / `SETTLED` …） | `erp.stock_in` / `erp.payable` / `erp.voucher` | `varchar` + `CHECK` |
///
/// **两种都不许静默忽略**：用户传了筛不掉的条件，却拿到全量文件，比报错危险得多。
#[derive(Debug, Clone, Default)]
pub struct ExportFilter {
    /// 关键词。
    pub keyword: Option<String>,
    /// 启用 / 停用过滤（1 启用 / 0 停用；`None` = 不过滤）。
    ///
    /// 只对 [`ExportDefinition::supports_status_flag`] 为 `true` 的定义有效；
    /// ERP 单据的状态是文本码，对它们传这个字段 → 422（提示改用 `status_code`）。
    pub status: Option<i16>,
    /// 单据状态码过滤（`POSTED` / `SETTLED` 这类文本码；`None` = 不过滤）。
    ///
    /// 合法码 = [`ExportDefinition::status_codes`]（与导出列定义里 `status` 列的
    /// `EnumItem` **同一份**，不另抄白名单）。不区分大小写；非法码 → 422 且点名合法码。
    pub status_code: Option<String>,
    /// 只导这些主键（D7 批量导出；空 = 不按主键收窄）。
    ///
    /// 只有声明了 `ids` 语义的定义（见 `builtin::erp_doc`）会在 SQL 里消费它；
    /// 老定义（`system.dict_item` / `system.user`）保持原行为，不会被这个字段影响。
    pub ids: Vec<uuid::Uuid>,
}

/// 导出定义：与导入对称。
#[async_trait]
pub trait ExportDefinition: Send + Sync {
    /// 业务编码。
    fn biz_type(&self) -> &'static str;
    /// 业务名称。
    fn biz_name(&self) -> &'static str;
    /// 列定义。
    fn columns(&self) -> &'static [ExportColumn];
    /// 是否支持「启用 / 停用」整型状态过滤（[`ExportFilter::status`]）。
    ///
    /// 默认 `false`：**没人认的过滤条件必须报错**（否则就是「用户以为筛了、实际拿了全量」）。
    /// 支持的定义（`system.dict_item` / `system.user`，状态列就是 `1` / `0`）显式覆盖成 `true`。
    fn supports_status_flag(&self) -> bool {
        false
    }
    /// 本定义的单据状态码白名单（[`ExportFilter::status_code`] 的合法取值）。
    ///
    /// 默认空 = 不支持按文本状态码过滤。ERP 单据（`builtin::erp_doc`）从导出列定义里
    /// `key == "status"` 的 [`ColumnKind::Enum`] 取码 —— **与导出回显同一份 `EnumItem`**，
    /// 列定义里改了码，白名单自动跟着改（不会出现「中文对、过滤的码不对」）。
    fn status_codes(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// 同步导出的行数上限（超过报 E-EXP-01，异步导出由 A4 提供）。
    fn sync_max_rows(&self) -> usize {
        10_000
    }
    /// 命中行数（用于阈值判定）。
    async fn count(&self, filter: &ExportFilter) -> std::result::Result<i64, DbError>;
    /// keyset 分页取数：`after_id` 为上一页最后一条的主键。
    async fn fetch(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError>;

    // ---------- 带数据权限的入口（批次 B · B6 新增，默认实现向下兼容） ----------
    //
    // 导出的取数必须与列表同口径（否则「列表只看得到本部门 5 个人，导出却拿到全公司」），
    // 而原始签名里没有「谁在导」这个信息。这里以**新增带默认实现的方法**扩展，而不是
    // 改 `count` / `fetch` 的签名：已注册的定义（`system.dict_item`）不需要改动就能继续工作，
    // 需要按范围取数的定义（`system.user`）覆盖这两个方法即可。

    /// 带数据权限的命中行数；默认忽略 `principal`，等价 [`Self::count`]。
    ///
    /// # 参数
    ///
    /// - `filter`：业务过滤条件；
    /// - `principal`：导出发起人的数据范围（`None` = 无法确认发起人）。
    async fn count_scoped(
        &self,
        filter: &ExportFilter,
        principal: Option<&crate::business::shared::principal::ScopedPrincipal>,
    ) -> std::result::Result<i64, DbError> {
        let _ = principal;
        self.count(filter).await
    }

    /// 带数据权限的取数；默认忽略 `principal`，等价 [`Self::fetch`]。
    ///
    /// # 参数
    ///
    /// - `filter`：业务过滤条件；
    /// - `after_id`：上一页最后一条主键（keyset 分页）；
    /// - `limit`：本页行数上限；
    /// - `principal`：导出发起人的数据范围。
    async fn fetch_scoped(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
        principal: Option<&crate::business::shared::principal::ScopedPrincipal>,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
        let _ = principal;
        self.fetch(filter, after_id, limit).await
    }
}

/// 校验「导出状态过滤」是否符合本定义的契约（卡 `t_93cba3f4`）。
///
/// 三个入口都必须先过这一关：同步导出（`handler::export_sync`）、异步建任务
/// （`handler::export_create` / `batch_export::create_by_ids`）、worker 执行（`worker::do_export`）。
///
/// # 为什么必须报错而不是忽略
///
/// `status` 是给「启用 / 停用」设计的 `i16`，而 ERP 单据的状态是**文本码**
/// （`POSTED` / `SETTLED` / `CANCELLED` …）。旧代码在 ERP 单据分支里根本没有 status 子句，
/// 于是 `?status=1` 静默返回**全量**：用户以为筛过，实际拿到全公司数据。
/// 宁可直接 422，也不能给一份口径不同的文件。
///
/// # 参数
///
/// - `definition`：目标导出定义（决定它认哪一种状态过滤）；
/// - `filter`：业务过滤条件。`status_code` 会被**原地规范化**（去空白 + 转大写），
///   保证下游 SQL 绑定的值与这里校验过的值逐字一致；空串按「不过滤」处理。
///
/// # 异常情况
///
/// - `422 + 10001`：契约不匹配 —— `status` 传给了文本码定义、`status_code` 传给了
///   没有白名单的定义、码不在白名单、或启用 / 停用值不是 `0` / `1`。
pub fn check_export_filter(
    definition: &dyn ExportDefinition,
    filter: &mut ExportFilter,
) -> std::result::Result<(), ApiError> {
    let biz_type = definition.biz_type();
    let codes = definition.status_codes();

    if let Some(raw) = filter.status_code.take() {
        let code = raw.trim().to_ascii_uppercase();
        if !code.is_empty() {
            if codes.is_empty() {
                return Err(ApiError::new(
                    422,
                    error_code::PARAM_INVALID,
                    format!(
                        "导出定义「{biz_type}」不支持按状态码过滤（status_code）：\
                         它没有状态码白名单；只有单据状态是文本码的导出（如 erp.voucher）才支持"
                    ),
                ));
            }
            if !codes.iter().any(|allowed| *allowed == code) {
                return Err(ApiError::new(
                    422,
                    error_code::PARAM_INVALID,
                    format!(
                        "状态码「{code}」不是「{biz_type}」的合法取值，只允许：{}（不区分大小写）",
                        codes.join(" / ")
                    ),
                ));
            }
            filter.status_code = Some(code);
        }
    }

    if let Some(status) = filter.status
        && !definition.supports_status_flag()
    {
        let hint = if codes.is_empty() {
            "；该定义也不支持 status（启用 / 停用），可用的过滤条件只有 keyword / ids".to_string()
        } else {
            format!(
                "；它的状态是文本码，请改用 status_code=…（合法码：{}）",
                codes.join(" / ")
            )
        };
        return Err(ApiError::new(
            422,
            error_code::PARAM_INVALID,
            format!("导出定义「{biz_type}」不支持 status={status} 过滤{hint}"),
        ));
    }

    if let Some(status) = filter.status
        && !matches!(status, 0 | 1)
    {
        return Err(ApiError::new(
            422,
            error_code::PARAM_INVALID,
            format!("status 只允许 1（启用）/ 0（停用），收到「{status}」"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &[EnumItem] = &[
        EnumItem {
            label: "启用",
            code: "1",
        },
        EnumItem {
            label: "停用",
            code: "0",
        },
    ];

    fn text(max: usize) -> ColumnKind {
        ColumnKind::Text { max_len: Some(max) }
    }

    #[test]
    fn text_trims_full_width_space_and_counts_chars() {
        let kind = text(4);
        assert_eq!(kind.parse("　ab　", None).expect("应合法"), "ab");
        let long = "物料编码超长";
        let issue = kind.parse(long, None).expect_err("应超长");
        assert_eq!(issue.kind, ErrorKind::Length);
        assert!(issue.message.contains("最多 4 个字符"));
        // 汉字按字符数而非字节数
        assert_eq!(text(4).parse("物料编码", None).expect("应合法"), "物料编码");
    }

    #[test]
    fn empty_value_is_not_a_type_error() {
        // 空值交给必填判定，避免同一次空触发两条错误
        assert_eq!(text(8).parse("   ", None).expect("不应报错"), "");
        assert_eq!(
            ColumnKind::Int {
                min: None,
                max: None
            }
            .parse("", None)
            .expect("不应报错"),
            ""
        );
        assert_eq!(ColumnKind::Date.parse(" ", None).expect("不应报错"), "");
    }

    #[test]
    fn int_range_is_closed_interval() {
        let kind = ColumnKind::Int {
            min: Some(0),
            max: Some(9_999),
        };
        assert_eq!(kind.parse("0", None).expect("下界应合法"), "0");
        assert_eq!(kind.parse("9999", None).expect("上界应合法"), "9999");
        assert_eq!(kind.parse(" 42 ", None).expect("空白应 trim"), "42");
        let low = kind.parse("-1", None).expect_err("下界外");
        assert_eq!(low.kind, ErrorKind::Range);
        assert!(low.message.contains("0–9999"));
        let high = kind.parse("10000", None).expect_err("上界外");
        assert_eq!(high.kind, ErrorKind::Range);
        let not_number = kind.parse("壹佰", None).expect_err("非数字");
        assert_eq!(not_number.kind, ErrorKind::Type);
        assert!(not_number.message.contains("实际为「壹佰」"));
    }

    #[test]
    fn decimal_scale_and_range() {
        let kind = ColumnKind::Decimal {
            scale: 2,
            min: Some(0.0),
            max: Some(100.0),
        };
        assert_eq!(kind.parse("13", None).expect("整数应合法"), "13");
        assert_eq!(kind.parse("13.5", None).expect("一位小数应合法"), "13.5");
        assert_eq!(kind.parse("13.50", None).expect("末尾零不计位数"), "13.50");
        let scale = kind.parse("13.505", None).expect_err("三位小数应拒绝");
        assert_eq!(scale.kind, ErrorKind::Range);
        assert_eq!(
            kind.parse("-0.5", None).expect_err("越界").kind,
            ErrorKind::Range
        );
        assert_eq!(
            kind.parse("abc", None).expect_err("非数字").kind,
            ErrorKind::Type
        );
    }

    #[test]
    fn bool_accepts_chinese_and_english() {
        for raw in ["是", "true", "TRUE", "1", "y", "真"] {
            assert_eq!(ColumnKind::Bool.parse(raw, None).expect("应合法"), "true");
        }
        for raw in ["否", "false", "0", "n", "假"] {
            assert_eq!(ColumnKind::Bool.parse(raw, None).expect("应合法"), "false");
        }
        assert_eq!(
            ColumnKind::Bool.parse("也许", None).expect_err("非法").kind,
            ErrorKind::Type
        );
    }

    #[test]
    fn date_accepts_three_separators_and_excel_serial() {
        assert_eq!(
            ColumnKind::Date.parse("2026-10-28", None).expect("应合法"),
            "2026-10-28"
        );
        assert_eq!(
            ColumnKind::Date.parse("2026/10/28", None).expect("应合法"),
            "2026-10-28"
        );
        assert_eq!(
            ColumnKind::Date.parse("2026.10.28", None).expect("应合法"),
            "2026-10-28"
        );
        // Excel 序列号 46323 = 2026-10-28（纪元 1899-12-30 起算）
        assert_eq!(
            ColumnKind::Date.parse("46323", None).expect("应合法"),
            "2026-10-28"
        );
        let bad = ColumnKind::Date
            .parse("2026/13/45", None)
            .expect_err("非法日期");
        assert_eq!(bad.kind, ErrorKind::Date);
        assert!(bad.message.contains("实际 2026/13/45"));
    }

    #[test]
    fn datetime_accepts_date_only_and_minutes() {
        assert_eq!(
            ColumnKind::DateTime
                .parse("2026-10-28", None)
                .expect("应合法"),
            "2026-10-28 00:00:00"
        );
        assert_eq!(
            ColumnKind::DateTime
                .parse("2026-10-28 12:30", None)
                .expect("应合法"),
            "2026-10-28 12:30:00"
        );
        assert_eq!(
            ColumnKind::DateTime
                .parse("2026/10/28 12:30:05", None)
                .expect("应合法"),
            "2026-10-28 12:30:05"
        );
        assert_eq!(
            ColumnKind::DateTime
                .parse("28/10/2026", None)
                .expect_err("非法")
                .kind,
            ErrorKind::Date
        );
    }

    #[test]
    fn enum_accepts_label_and_code_and_reports_options() {
        let kind = ColumnKind::Enum(STATUS);
        assert_eq!(kind.parse("启用", None).expect("中文应合法"), "1");
        assert_eq!(kind.parse("0", None).expect("码值应合法"), "0");
        let bad = kind.parse("台套", None).expect_err("非法枚举");
        assert_eq!(bad.kind, ErrorKind::Enum);
        assert!(bad.message.contains("启用 / 停用"));
    }

    #[test]
    fn dict_translation_both_directions() {
        let dict = DictTranslation::from_items(vec![
            ("1".to_string(), "启用".to_string()),
            ("0".to_string(), "停用".to_string()),
        ]);
        let kind = ColumnKind::Dict("sys_status");
        assert_eq!(kind.parse("启用", Some(&dict)).expect("label"), "1");
        assert_eq!(kind.parse("0", Some(&dict)).expect("code"), "0");
        assert_eq!(kind.format("0", Some(&dict)), "停用");
        // 字典未装载 → 规则错误（不是「值非法」，避免无从排查）
        assert_eq!(
            kind.parse("启用", None).expect_err("未装载").kind,
            ErrorKind::Rule
        );
        let bad = kind.parse("台套", Some(&dict)).expect_err("不在字典");
        assert_eq!(bad.kind, ErrorKind::Dict);
        assert!(bad.message.contains("sys_status"));
    }

    #[test]
    fn format_translates_codes_for_export() {
        assert_eq!(ColumnKind::Enum(STATUS).format("1", None), "启用");
        assert_eq!(ColumnKind::Enum(STATUS).format("9", None), "9");
        assert_eq!(ColumnKind::Bool.format("true", None), "是");
        assert_eq!(ColumnKind::Bool.format("false", None), "否");
        assert_eq!(
            ColumnKind::Date.format("2026-10-28 00:00:00", None),
            "2026-10-28"
        );
        assert_eq!(text(8).format("", None), "");
    }

    #[test]
    fn rule_and_option_text_are_stable() {
        assert_eq!(text(32).rule_text(), "≤32 个字符");
        assert_eq!(
            ColumnKind::Int {
                min: Some(0),
                max: Some(100)
            }
            .rule_text(),
            "0–100"
        );
        assert_eq!(
            ColumnKind::Int {
                min: Some(0),
                max: None
            }
            .rule_text(),
            "≥0"
        );
        assert_eq!(
            ColumnKind::Int {
                min: None,
                max: Some(9)
            }
            .rule_text(),
            "≤9"
        );
        assert_eq!(ColumnKind::Date.rule_text(), "YYYY-MM-DD");
        assert_eq!(ColumnKind::Enum(STATUS).option_text(), "启用 / 停用");
        assert_eq!(
            ColumnKind::Dict("sys_status").option_text(),
            "字典 sys_status"
        );
        assert_eq!(text(3).option_text(), "");
    }

    #[test]
    fn error_kind_codes_match_prd() {
        assert_eq!(ErrorKind::Required.as_str(), "REQUIRED");
        assert_eq!(ErrorKind::FkNotFound.as_str(), "FK_NOT_FOUND");
        assert_eq!(ErrorKind::RowLimit.as_str(), "ROW_LIMIT");
        assert_eq!(ErrorKind::Structure.as_str(), "STRUCTURE");
    }

    #[test]
    fn import_mode_parsing() {
        assert_eq!(ImportMode::parse("strict"), Some(ImportMode::Strict));
        assert_eq!(ImportMode::parse(""), Some(ImportMode::Strict));
        assert_eq!(ImportMode::parse("PARTIAL"), Some(ImportMode::Partial));
        assert_eq!(ImportMode::parse("oops"), None);
        assert_eq!(ImportMode::Partial.as_str(), "PARTIAL");
    }

    #[test]
    fn raw_value_is_truncated_for_storage() {
        let column = ImportColumn {
            key: "k",
            header: "列",
            required: false,
            kind: text(500),
            unique_key: false,
            fk: None,
            example: "",
            width: None,
            remark: "",
        };
        let long = "x".repeat(260);
        let issue = RowIssue::column(3, &column, &long, FieldIssue::new(ErrorKind::Type, "bad"));
        assert_eq!(
            issue.raw_value.as_deref().map(|v| v.chars().count()),
            Some(200)
        );
        assert_eq!(issue.column_key, Some("k"));
    }

    #[test]
    fn template_header_marks_required_columns() {
        let column = ImportColumn {
            key: "k",
            header: "物料编码",
            required: true,
            kind: text(32),
            unique_key: true,
            fk: None,
            example: "MAT-000001",
            width: Some(18),
            remark: "字母+数字",
        };
        assert_eq!(column.template_header(), "物料编码*");
    }

    // ---------- 导出状态过滤的契约（卡 t_93cba3f4） ----------

    /// 最小导出定义：既没有状态列、也不认整型 `status`
    /// （用来钉「两样都不认的定义，收到任何状态过滤都必须 422」）。
    struct NoStatusExport;

    #[async_trait::async_trait]
    impl ExportDefinition for NoStatusExport {
        fn biz_type(&self) -> &'static str {
            "test.no_status"
        }
        fn biz_name(&self) -> &'static str {
            "无状态导出"
        }
        fn columns(&self) -> &'static [ExportColumn] {
            &[]
        }
        async fn count(&self, _filter: &ExportFilter) -> std::result::Result<i64, DbError> {
            Ok(0)
        }
        async fn fetch(
            &self,
            _filter: &ExportFilter,
            _after_id: Option<&str>,
            _limit: u32,
        ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn unsupported_status_filters_are_rejected_not_ignored() {
        let definition = NoStatusExport;

        // 整型 status：没人认 → 422（静默忽略就是「用户以为筛了、实际拿了全量」）
        let mut filter = ExportFilter {
            status: Some(1),
            ..Default::default()
        };
        let error = check_export_filter(&definition, &mut filter).expect_err("不支持必须报错");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(
            error.msg.contains("test.no_status") && error.msg.contains("keyword"),
            "报错要说明是哪个定义、还能用什么过滤：{}",
            error.msg
        );

        // 文本码：没有白名单 → 同样 422
        let mut filter = ExportFilter {
            status_code: Some("POSTED".to_string()),
            ..Default::default()
        };
        let error = check_export_filter(&definition, &mut filter).expect_err("没有白名单必须报错");
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("status_code"), "{}", error.msg);

        // 什么都不传 → 通过（行为与修复前一致）
        let mut filter = ExportFilter::default();
        check_export_filter(&definition, &mut filter).expect("不传状态不是错误");

        // 默认实现：两种状态过滤都不支持（第三方定义不该默认「看起来支持」）
        assert!(!definition.supports_status_flag());
        assert!(definition.status_codes().is_empty());
    }
}
