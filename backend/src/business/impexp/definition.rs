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

use alun::prelude::*;
use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};

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
    if !serial.is_finite() || serial < 1.0 || serial > 2_958_465.0 {
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
    /// 是否敏感字段（导出脱敏由 A4/A7 的权限与 Mask 能力接管；此处仅标记）。
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
#[derive(Debug, Clone, Default)]
pub struct ExportFilter {
    /// 关键词。
    pub keyword: Option<String>,
    /// 状态过滤（1 启用 / 0 停用；`None` 表示不过滤）。
    pub status: Option<i16>,
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
}
