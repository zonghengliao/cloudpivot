//! 七种脱敏类型 + 纯函数实现（批次 B · B6）。
//!
//! # 为什么是「纯函数」而不是「服务」
//!
//! 脱敏值只由 `(类型, 参数, 原文)` 决定，与请求、租户、数据库都无关。把它做成纯函数
//! 意味着七种类型的正确性与边界（空值 / 过短 / 多字节 / 幂等）可以在**无库**环境下
//! 逐条断言——这正是本卡验收第一条要求的形态。
//!
//! # D7：为什么绝不调用 `nik_utils::Mask::mask_by_type`
//!
//! `mask_by_type` 的兜底分支是
//! `format!("{}***{}", &value[..2], &value[len - 2..])`——按**字节**切片。
//! 输入 `"张三"`（6 字节、2 个字符）时 `&value[..2]` 落在字符中间，**直接 panic**
//! （`byte index 2 is not a char boundary`，实测于 `nik-utils-0.4.2/src/mask.rs:198`）。
//! 因此本模块**只调用 `Mask` 的具名方法**（`mobile` / `email` / `id_card` / `name` /
//! `bank_card`），并且对其中仍然按字节切片的 `bank_card` / `email` 外加一层
//! 「仅当纯 ASCII 时才交给框架」的前置判定。
//!
//! 这条约束由 `scripts/check.sh` 的同款思路守着：仓库里 `grep -rn mask_by_type`
//! 只能命中本段注释（见本卡验收第 2 条）。
//!
//! # 四条统一约束（写进 [`tests`] 的每个用例）
//!
//! 1. 空值 / 纯空白 → 原样返回（不产生 `"" → "****"` 的噪音）；
//! 2. **永不 panic**：所有切片走 `chars()` / `char_indices()` 边界；
//! 3. 幂等：`apply(apply(x)) == apply(x)`（避免二次脱敏把掩码吃掉）；
//! 4. 长度不变性：不满足最小长度的值**原样返回**而不是报错——脏数据不该让列表接口 500。

use std::sync::LazyLock;

use nik_utils::Mask;
use regex::Regex;

use crate::shared::error_code;

/// 脱敏类型（与 `sys_field_mask.mask_type` 的 1..7 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaskType {
    /// 1 手机号：`13812345678` → `138****5678`（参数 `"保留前N,保留后M"`，默认 `3,4`）。
    Mobile,
    /// 2 身份证：`110101199001011234` → `1101**********1234`（默认 `4,4`）。
    IdCard,
    /// 3 邮箱：`alice@mail.com` → `a***@mail.com`（无参数）。
    Email,
    /// 4 姓名：`张三丰` → `张**`（参数 `"保留前N"`，默认 `1`）。
    Name,
    /// 5 银行卡：`6222021234567890` → `6222 **** 7890`（默认 `4,4`）。
    BankCard,
    /// 6 金额：`48620.00` → `48000~49000`（参数 `"区间宽度"`，默认 `1000`）。
    Amount,
    /// 7 自定义正则：参数 `"pattern|group"`，保留捕获组文本、其余替换为 `*`。
    Regex,
}

impl MaskType {
    /// 七种类型（顺序即 `mask_type` 的 1..7，供自检 / 前端下拉使用）。
    pub const ALL: &'static [MaskType] = &[
        MaskType::Mobile,
        MaskType::IdCard,
        MaskType::Email,
        MaskType::Name,
        MaskType::BankCard,
        MaskType::Amount,
        MaskType::Regex,
    ];

    /// 由库里的 `SMALLINT` 还原；超出 1..7 → `None`（检查约束已保证不会发生，
    /// 但解析侧仍不 panic，见模块头「约束 2」）。
    ///
    /// # 参数
    ///
    /// - `raw`：`sys_field_mask.mask_type` 的整数值。
    pub fn from_i16(raw: i16) -> Option<Self> {
        match raw {
            1 => Some(MaskType::Mobile),
            2 => Some(MaskType::IdCard),
            3 => Some(MaskType::Email),
            4 => Some(MaskType::Name),
            5 => Some(MaskType::BankCard),
            6 => Some(MaskType::Amount),
            7 => Some(MaskType::Regex),
            _ => None,
        }
    }

    /// 落库值（1..7）。
    pub fn as_i16(self) -> i16 {
        match self {
            MaskType::Mobile => 1,
            MaskType::IdCard => 2,
            MaskType::Email => 3,
            MaskType::Name => 4,
            MaskType::BankCard => 5,
            MaskType::Amount => 6,
            MaskType::Regex => 7,
        }
    }

    /// 稳定英文编码（日志 / 前端 i18n 键）。
    pub fn code(self) -> &'static str {
        match self {
            MaskType::Mobile => "mobile",
            MaskType::IdCard => "id_card",
            MaskType::Email => "email",
            MaskType::Name => "name",
            MaskType::BankCard => "bank_card",
            MaskType::Amount => "amount",
            MaskType::Regex => "regex",
        }
    }

    /// 中文标签（前端下拉、错误文案）。
    pub fn label(self) -> &'static str {
        match self {
            MaskType::Mobile => "手机号",
            MaskType::IdCard => "身份证",
            MaskType::Email => "邮箱",
            MaskType::Name => "姓名",
            MaskType::BankCard => "银行卡",
            MaskType::Amount => "金额",
            MaskType::Regex => "自定义正则",
        }
    }

    /// 参数形态说明（`sys_field_mask.mask_param` 的合法写法，错误文案里回显给管理员）。
    pub fn param_hint(self) -> &'static str {
        match self {
            MaskType::Mobile | MaskType::IdCard | MaskType::BankCard => {
                "保留前N,保留后M（如 \"3,4\"）"
            }
            MaskType::Name => "保留前N（如 \"1\"）",
            MaskType::Amount => "区间宽度（如 \"1000\"）",
            MaskType::Regex => "正则与保留组（如 \"^(\\\\w{2}).*$|1\"）",
            MaskType::Email => "无参数（留空）",
        }
    }

    /// 缺省参数（`mask_param` 为空时生效，与架构 §3.2 表格逐条一致）。
    ///
    /// `Email` 返回 `None`（无参数概念）；`Regex` 返回 `None`（必须显式给出，
    /// 缺省时 [`apply`] 报 61005 而不是去猜一个正则）。
    pub fn default_param(self) -> Option<&'static str> {
        match self {
            MaskType::Mobile => Some("3,4"),
            MaskType::IdCard => Some("4,4"),
            MaskType::Name => Some("1"),
            MaskType::BankCard => Some("4,4"),
            MaskType::Amount => Some("1000"),
            MaskType::Email | MaskType::Regex => None,
        }
    }
}

/// 脱敏失败的原因（**必须显式报错**，不许静默返回明文——本卡红线 fail-closed）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskError {
    /// 参数格式不符（`mask_param` 不是 `"N,M"` / 数字 / 正则参数）。
    Param {
        /// 出错的类型。
        ty: MaskType,
        /// 原始参数（`None` 表示缺失）。
        param: Option<String>,
        /// 人能看懂的原因。
        reason: String,
    },
    /// 自定义正则编译失败 / 捕获组越界。
    Regex {
        /// 正则原文。
        pattern: String,
        /// 原因（编译错误原文 / 组序号越界说明）。
        reason: String,
    },
    /// 规则本身不可用（例如库里 `mask_type` 被改成了 1..7 之外的值）。
    ///
    /// 这类错误**不该发生**（`sys_field_mask` 有 `CHECK (mask_type BETWEEN 1 AND 7)`），
    /// 出现即意味着有人绕过应用直接改库；记录成独立变体是为了让日志一眼区分
    /// 「管理员填错参数」与「数据被破坏」。
    Rule {
        /// 原因。
        reason: String,
    },
    /// **导出 fail-closed**：敏感列没有任何生效规则（HTTP 422 + 61008）。
    ///
    /// 只在导出路径产生（架构 §3.4 闸 3）：宁可让导出失败，也不要出一份明文 Excel。
    MissingRule {
        /// 表名。
        table: String,
        /// 列名。
        column: String,
    },
}

impl MaskError {
    /// HTTP 状态码：参数问题 400，正则 / 规则 / 缺规则 422（与 `error_code` 段的注释一致）。
    pub fn http_status(&self) -> u16 {
        match self {
            MaskError::Param { .. } => 400,
            MaskError::Regex { .. } | MaskError::Rule { .. } | MaskError::MissingRule { .. } => 422,
        }
    }

    /// 业务错误码（`shared/error_code.rs` 的 6100x 段，**不得改值**）。
    pub fn error_code(&self) -> i32 {
        match self {
            MaskError::Param { .. } | MaskError::Rule { .. } => error_code::FIELD_MASK_TYPE_INVALID,
            MaskError::Regex { .. } => error_code::FIELD_MASK_REGEX_INVALID,
            MaskError::MissingRule { .. } => error_code::FIELD_MASK_RULE_MISSING,
        }
    }

    /// 面向管理员的文案（带参数示例，便于当场改对）。
    pub fn message(&self) -> String {
        match self {
            MaskError::Param { ty, param, reason } => format!(
                "脱敏类型「{}」的参数不合法：{}（当前值 {}，应写 {}）",
                ty.label(),
                reason,
                param.as_deref().unwrap_or("空"),
                ty.param_hint()
            ),
            MaskError::Regex { pattern, reason } => {
                format!("自定义正则不合法（{reason}）：{pattern}")
            }
            MaskError::Rule { reason } => format!("脱敏规则不可用：{reason}"),
            MaskError::MissingRule { table, column } => format!(
                "敏感列 {table}.{column} 没有生效的脱敏规则，已按 fail-closed 拒绝本次导出\
                 （请联系管理员在「字段脱敏」里补规则）"
            ),
        }
    }
}

/// 执行一次脱敏（七种类型全覆盖）。
///
/// # 参数
///
/// - `ty`：脱敏类型；
/// - `param`：`sys_field_mask.mask_param`（`None` / 空串时用 [`MaskType::default_param`]）；
/// - `raw`：原文（出参里的明文值）。
///
/// # 返回值
///
/// 脱敏后的字符串。空值 / 纯空白、以及「短于最小长度」的脏数据**原样返回**。
///
/// # 异常情况
///
/// 只返回 [`MaskError`]，**永不 panic**。调用方拿到 `Err` 必须走 fail-closed
/// （报错或全掩码），**不许**把 `raw` 当结果用。
pub fn apply(
    ty: MaskType,
    param: Option<&str>,
    raw: &str,
) -> std::result::Result<String, MaskError> {
    // 约束 1 + 4：空 / 纯空白原样返回（不做任何统计，避免产生噪音掩码）
    if raw.trim().is_empty() {
        return Ok(raw.to_string());
    }
    match ty {
        MaskType::Mobile => {
            let (front, back) = keep_range(ty, param, (3, 4))?;
            if (front, back) == (3, 4) {
                // 复用框架具名方法（D7：绝不走 mask_by_type）
                Ok(Mask::mobile(raw))
            } else {
                Ok(keep_ends(raw, front, back, "****"))
            }
        }
        MaskType::IdCard => {
            let (front, back) = keep_range(ty, param, (4, 4))?;
            if (front, back) == (4, 4) {
                Ok(Mask::id_card(raw))
            } else {
                Ok(keep_ends(raw, front, back, "****"))
            }
        }
        MaskType::Email => Ok(mask_email(raw)),
        MaskType::Name => {
            let keep = keep_front(ty, param)?;
            if keep == 1 {
                // `Mask::name` 按 `chars()` 处理，POC-9 已验证多字节安全
                Ok(Mask::name(raw))
            } else {
                Ok(keep_prefix(raw, keep))
            }
        }
        MaskType::BankCard => {
            let (front, back) = keep_range(ty, param, (4, 4))?;
            // `Mask::bank_card` 内部是 `&card[..4]`（字节切片）——只有纯 ASCII 才安全，
            // 否则落回本模块的按字符实现（D7 的同一条理由）
            if (front, back) == (4, 4) && raw.is_ascii() {
                Ok(Mask::bank_card(raw))
            } else {
                Ok(keep_ends(raw, front, back, " **** "))
            }
        }
        MaskType::Amount => mask_amount(raw, param),
        MaskType::Regex => mask_regex(raw, param),
    }
}

// ==================== 参数解析（纯逻辑） ====================

/// 解析 `"保留前N,保留后M"`；缺省走 [`MaskType::default_param`]。
fn keep_range(
    ty: MaskType,
    param: Option<&str>,
    default: (usize, usize),
) -> std::result::Result<(usize, usize), MaskError> {
    let text = param.map(str::trim).filter(|item| !item.is_empty());
    let Some(text) = text else {
        return Ok(default);
    };
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(MaskError::Param {
            ty,
            param: Some(text.to_string()),
            reason: "应为两段「保留前N,保留后M」".to_string(),
        });
    }
    let front = parse_usize(ty, text, parts[0])?;
    let back = parse_usize(ty, text, parts[1])?;
    Ok((front, back))
}

/// 解析 `"保留前N"`；缺省 1。
fn keep_front(ty: MaskType, param: Option<&str>) -> std::result::Result<usize, MaskError> {
    let text = param.map(str::trim).filter(|item| !item.is_empty());
    let Some(text) = text else {
        return Ok(1);
    };
    parse_usize(ty, text, text)
}

fn parse_usize(ty: MaskType, param: &str, raw: &str) -> std::result::Result<usize, MaskError> {
    raw.parse::<usize>().map_err(|_| MaskError::Param {
        ty,
        param: Some(param.to_string()),
        reason: format!("「{raw}」不是非负整数"),
    })
}

// ==================== 各类型的字符安全实现 ====================

/// 保留前 `front`、后 `back` 个**字符**，中间用 `joiner` 填充。
///
/// 长度不足 `front + back` 时原样返回（约束 4：脏数据不 500，也不 panic）。
fn keep_ends(raw: &str, front: usize, back: usize, joiner: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= front + back {
        return raw.to_string();
    }
    let head: String = chars[..front].iter().collect();
    let tail: String = chars[chars.len() - back..].iter().collect();
    format!("{head}{joiner}{tail}")
}

/// 保留前 `keep` 个字符，其余替换为 `*`（字符数不变）。
fn keep_prefix(raw: &str, keep: usize) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= keep {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    out.extend(chars[..keep].iter());
    for _ in keep..chars.len() {
        out.push('*');
    }
    out
}

/// 邮箱脱敏：保留本地部分首字符 + 完整域名。
///
/// 本地部分只有 1–2 个字符时整段打成一个 `*`（`ab@x.com` → `*@x.com`）。
fn mask_email(raw: &str) -> String {
    let Some(at) = raw.find('@') else {
        return raw.to_string();
    };
    let local = &raw[..at];
    let domain = &raw[at..];
    // 纯 ASCII 且本地部分 > 2 时交给框架（D7：具名方法，不用 mask_by_type）
    if local.is_ascii() && local.chars().count() > 2 {
        return Mask::email(raw);
    }
    let local_chars: Vec<char> = local.chars().collect();
    if local_chars.len() <= 2 {
        format!("*{domain}")
    } else {
        let head: String = local_chars[..1].iter().collect();
        format!("{head}***{domain}")
    }
}

/// 金额区间化：按 `width` 取整到区间，输出 `下界~上界`。
///
/// # 为什么不用 f64
///
/// 金额精度是批次 A 立的规矩（`Decimal` 只走定点）。这里把小数串解析成
/// **定点整数**（放大 10^6 倍），区间划分全部走 `i128` 整数运算，
/// 全程不出现浮点，也不引入新 crate。
///
/// 非数字原文（含已经脱敏过的 `48000~49000`）原样返回 —— 这同时保证了幂等。
fn mask_amount(raw: &str, param: Option<&str>) -> std::result::Result<String, MaskError> {
    const SCALE: u32 = 6;
    let width_text = param.map(str::trim).filter(|item| !item.is_empty());
    let width = match width_text {
        None => 1000_u64,
        Some(text) => text.parse::<u64>().map_err(|_| MaskError::Param {
            ty: MaskType::Amount,
            param: Some(text.to_string()),
            reason: "区间宽度应为正整数".to_string(),
        })?,
    };
    if width == 0 {
        return Err(MaskError::Param {
            ty: MaskType::Amount,
            param: width_text.map(str::to_string),
            reason: "区间宽度不能为 0".to_string(),
        });
    }
    let Some(scaled) = parse_fixed(raw, SCALE) else {
        // 非数字：原样返回（长度不变性 + 幂等）
        return Ok(raw.to_string());
    };
    let bucket = i128::from(width) * 10_i128.pow(SCALE);
    let lower = scaled.div_euclid(bucket) * bucket;
    let upper = lower + bucket;
    Ok(format!(
        "{}~{}",
        format_fixed(lower, SCALE),
        format_fixed(upper, SCALE)
    ))
}

/// 把十进制串解析成放大 `10^scale` 倍的定点整数；非法返回 `None`。
fn parse_fixed(raw: &str, scale: u32) -> Option<i128> {
    let text = raw.trim();
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if body.is_empty() {
        return None;
    }
    let mut split = body.split('.');
    let int_part = split.next()?;
    let frac_part = split.next().unwrap_or("");
    if split.next().is_some() {
        return None;
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let int_value: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    // 小数位超过 scale 的部分按「截断」处理（金额区间化不需要更高精度）
    let mut frac = String::with_capacity(scale as usize);
    for c in frac_part.chars().take(scale as usize) {
        frac.push(c);
    }
    while frac.len() < scale as usize {
        frac.push('0');
    }
    let frac_value: i128 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    let magnitude = int_value
        .checked_mul(10_i128.pow(scale))?
        .checked_add(frac_value)?;
    Some(if negative { -magnitude } else { magnitude })
}

/// 把放大 `10^scale` 倍的定点整数格式化成「去掉多余 0」的十进制串。
fn format_fixed(value: i128, scale: u32) -> String {
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let factor = 10_u128.pow(scale);
    let int_part = magnitude / factor;
    let mut frac_part = format!("{:0width$}", magnitude % factor, width = scale as usize);
    while frac_part.ends_with('0') {
        frac_part.pop();
    }
    let sign = if negative { "-" } else { "" };
    if frac_part.is_empty() {
        format!("{sign}{int_part}")
    } else {
        format!("{sign}{int_part}.{frac_part}")
    }
}

/// 自定义正则脱敏。
///
/// 参数形如 `"^(\\w{2}).*$|1"`：最后一段 `|` 之后是**保留的捕获组序号**，
/// 之前是正则原文。用「最后一个 `|`」切分是关键——正则自身含 `|`（如 `a|b`）时
/// 从第一个 `|` 切会切坏表达式。
fn mask_regex(raw: &str, param: Option<&str>) -> std::result::Result<String, MaskError> {
    let text = param.map(str::trim).filter(|item| !item.is_empty());
    let Some(text) = text else {
        return Err(MaskError::Param {
            ty: MaskType::Regex,
            param: None,
            reason: "自定义正则必须给出参数".to_string(),
        });
    };
    let (pattern, group) = match text.rsplit_once('|') {
        Some((pattern, group)) => (pattern.trim(), group.trim()),
        None => (text, "0"),
    };
    let group: usize = group.parse().map_err(|_| MaskError::Param {
        ty: MaskType::Regex,
        param: Some(text.to_string()),
        reason: format!("保留组「{group}」不是非负整数"),
    })?;
    static CACHE: LazyLock<std::sync::Mutex<std::collections::BTreeMap<String, Regex>>> =
        LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let regex = {
        let mut guard = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(cached) = guard.get(pattern) {
            cached.clone()
        } else {
            let compiled = Regex::new(pattern).map_err(|error| MaskError::Regex {
                pattern: pattern.to_string(),
                reason: error.to_string(),
            })?;
            // 规则表极小（< 50 行），但接口可能被反复试运行；上限只是防呆
            if guard.len() < 256 {
                guard.insert(pattern.to_string(), compiled.clone());
            }
            compiled
        }
    };
    let Some(captures) = regex.captures(raw) else {
        // 不匹配不是错误（约束 4）：脏数据原样返回，幂等也靠这一条
        return Ok(raw.to_string());
    };
    if group >= captures.len() {
        return Err(MaskError::Regex {
            pattern: pattern.to_string(),
            reason: format!(
                "保留组 {group} 越界（该正则只有 {} 个捕获组）",
                captures.len().saturating_sub(1)
            ),
        });
    }
    let kept = captures.get(group).map(|m| m.as_str()).unwrap_or("");
    Ok(keep_prefix_by_kept(raw, kept))
}

/// 保留 `kept`（捕获组文本），其余字符替换为 `*`（总字符数不变）。
fn keep_prefix_by_kept(raw: &str, kept: &str) -> String {
    let total = raw.chars().count();
    let keep = kept.chars().count().min(total);
    let mut out = String::with_capacity(raw.len());
    out.push_str(kept);
    for _ in keep..total {
        out.push('*');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(ty: MaskType, param: Option<&str>, raw: &str) -> String {
        apply(ty, param, raw).expect("脱敏不应失败")
    }

    // ---------- 1. 手机号 ----------

    #[test]
    fn mobile_default_and_custom() {
        assert_eq!(masked(MaskType::Mobile, None, "13812345678"), "138****5678");
        assert_eq!(
            masked(MaskType::Mobile, Some("3,4"), "13812345678"),
            "138****5678"
        );
        // 自定义保留位（框架的 Mask::mobile 只支持 3,4，其余走本模块的按字符实现）
        assert_eq!(
            masked(MaskType::Mobile, Some("4,2"), "13812345678"),
            "1381****78"
        );
        // 幂等
        assert_eq!(masked(MaskType::Mobile, None, "138****5678"), "138****5678");
    }

    #[test]
    fn mobile_boundaries_do_not_panic() {
        // 过短（5 位）：原样返回，不 panic
        assert_eq!(masked(MaskType::Mobile, None, "13812"), "13812");
        // 恰好 7 位：`Mask::mobile` 的下界（3+4 = 7，首尾重叠）
        assert_eq!(masked(MaskType::Mobile, None, "1381234"), "138****1234");
        // 6 位仍在下界之外 → 原样
        assert_eq!(masked(MaskType::Mobile, None, "138123"), "138123");
        // 空值 / 纯空白
        assert_eq!(masked(MaskType::Mobile, None, ""), "");
        assert_eq!(masked(MaskType::Mobile, None, "   "), "   ");
        // 多字节字符（D7 的事故输入形态）
        assert_eq!(masked(MaskType::Mobile, None, "张三"), "张三");
    }

    // ---------- 2. 身份证 ----------

    #[test]
    fn id_card_default_and_short() {
        // 框架 `Mask::id_card` 的口径：保留前 4 后 4，中间固定 4 个星号（实现在
        // `nik-utils-0.4.2/src/mask.rs`，与架构表里的示例星号个数不同——以代码为准）
        assert_eq!(
            masked(MaskType::IdCard, None, "110101199001011234"),
            "1101****1234"
        );
        assert_eq!(
            masked(MaskType::IdCard, Some("6,4"), "110101199001011234"),
            "110101****1234"
        );
        // 多字节：按字符切片，不 panic
        assert_eq!(
            masked(MaskType::IdCard, None, "身份证号甲乙丙丁戊己"),
            "身份证号****丙丁戊己"
        );
        assert_eq!(masked(MaskType::IdCard, None, "1234"), "1234");
    }

    // ---------- 3. 邮箱 ----------

    #[test]
    fn email_variants() {
        assert_eq!(
            masked(MaskType::Email, None, "alice@mail.com"),
            "a***@mail.com"
        );
        assert_eq!(masked(MaskType::Email, None, "ab@mail.com"), "*@mail.com");
        // 无 @ → 原样
        assert_eq!(
            masked(MaskType::Email, None, "not-an-email"),
            "not-an-email"
        );
        // 多字节本地部分：Mask::email 会按字节切，这里走字符安全分支
        assert_eq!(
            masked(MaskType::Email, None, "张三丰@mail.com"),
            "张***@mail.com"
        );
        // 幂等
        assert_eq!(
            masked(MaskType::Email, None, "a***@mail.com"),
            "a***@mail.com"
        );
    }

    // ---------- 4. 姓名 ----------

    #[test]
    fn name_variants() {
        assert_eq!(masked(MaskType::Name, None, "张三丰"), "张**");
        assert_eq!(masked(MaskType::Name, Some("2"), "张三丰"), "张三*");
        // 单字符 / 空：原样
        assert_eq!(masked(MaskType::Name, None, "张"), "张");
        // 幂等
        assert_eq!(masked(MaskType::Name, None, "张**"), "张**");
    }

    // ---------- 5. 银行卡 ----------

    #[test]
    fn bank_card_variants() {
        assert_eq!(
            masked(MaskType::BankCard, None, "6222021234567890"),
            "6222 **** 7890"
        );
        // 多字节：Mask::bank_card 按字节切片会 panic，这里必须走字符安全分支
        assert_eq!(
            masked(MaskType::BankCard, None, "卡号甲乙丙丁戊己庚辛"),
            "卡号甲乙 **** 戊己庚辛"
        );
        // 过短：原样
        assert_eq!(masked(MaskType::BankCard, None, "6222"), "6222");
        // 幂等
        assert_eq!(
            masked(MaskType::BankCard, None, "6222 **** 7890"),
            "6222 **** 7890"
        );
    }

    // ---------- 6. 金额 ----------

    #[test]
    fn amount_buckets_without_float() {
        assert_eq!(masked(MaskType::Amount, None, "48620.00"), "48000~49000");
        // 自定义区间宽度
        assert_eq!(
            masked(MaskType::Amount, Some("100"), "48620.00"),
            "48600~48700"
        );
        // 负数向下取整（div_euclid）
        assert_eq!(
            masked(MaskType::Amount, Some("1000"), "-1500.50"),
            "-2000~-1000"
        );
        // 非数字（含已脱敏的区间串）原样 → 幂等
        assert_eq!(masked(MaskType::Amount, None, "报价面议"), "报价面议");
        assert_eq!(masked(MaskType::Amount, None, "48000~49000"), "48000~49000");
        // 空值
        assert_eq!(masked(MaskType::Amount, None, ""), "");
    }

    // ---------- 7. 自定义正则 ----------

    #[test]
    fn regex_keep_group() {
        assert_eq!(
            masked(MaskType::Regex, Some(r"^(\w{2}).*$|1"), "AB12345"),
            "AB*****"
        );
        // 保留组 0（整段匹配）
        assert_eq!(
            masked(MaskType::Regex, Some(r"^\w{2}|0"), "AB12345"),
            "AB*****"
        );
        // 正则自身含 `|`（从最后一个 `|` 切分才切得对）
        assert_eq!(
            masked(MaskType::Regex, Some(r"^(AB|CD).*$|1"), "AB12345"),
            "AB*****"
        );
        // 不匹配 → 原样（不报错、幂等）
        assert_eq!(
            masked(MaskType::Regex, Some(r"^ZZ.*$|0"), "AB12345"),
            "AB12345"
        );
        // 多字节：按字符计数，不 panic
        assert_eq!(
            masked(MaskType::Regex, Some(r"^(..).*$|1"), "张三丰李四"),
            "张三***"
        );
        // 幂等
        assert_eq!(
            masked(MaskType::Regex, Some(r"^(\w{2}).*$|1"), "AB*****"),
            "AB*****"
        );
    }

    // ---------- 参数错误：必须报错，绝不放明文 ----------

    #[test]
    fn invalid_params_report_errors() {
        let bad = apply(MaskType::Mobile, Some("3"), "13812345678").expect_err("参数应为两段");
        assert_eq!(bad.http_status(), 400);
        assert_eq!(bad.error_code(), error_code::FIELD_MASK_TYPE_INVALID);
        assert!(
            bad.message().contains("保留前N,保留后M"),
            "{}",
            bad.message()
        );

        let bad_amount = apply(MaskType::Amount, Some("0"), "1.00").expect_err("宽度不能为 0");
        assert_eq!(bad_amount.error_code(), error_code::FIELD_MASK_TYPE_INVALID);

        let bad_regex =
            apply(MaskType::Regex, Some("^(unclosed|1"), "x").expect_err("正则应编译失败");
        assert_eq!(bad_regex.http_status(), 422);
        assert_eq!(bad_regex.error_code(), error_code::FIELD_MASK_REGEX_INVALID);

        let out_of_range =
            apply(MaskType::Regex, Some(r"^(\w{2}).*$|5"), "AB12345").expect_err("组越界");
        assert_eq!(
            out_of_range.error_code(),
            error_code::FIELD_MASK_REGEX_INVALID
        );
    }

    // ---------- 七种类型 × 幂等 / 不 panic（汇总断言） ----------

    #[test]
    fn all_types_are_idempotent_and_never_panic() {
        let samples: [&str; 6] = [
            "13812345678",
            "110101199001011234",
            "alice@mail.com",
            "张三丰",
            "6222021234567890",
            "48620.00",
        ];
        // Regex 必须显式给参数，单独在 `regex_keep_group` 里覆盖幂等；其余六类
        // 用缺省参数各扫一遍，断言「一次 == 二次」（幂等）
        for ty in MaskType::ALL.iter().filter(|ty| **ty != MaskType::Regex) {
            for raw in samples {
                let once = apply(*ty, None, raw)
                    .unwrap_or_else(|error| panic!("{} 不应失败：{}", ty.code(), error.message()));
                let twice = apply(*ty, None, &once).unwrap_or_else(|error| {
                    panic!("{} 二次脱敏不应失败：{}", ty.code(), error.message())
                });
                assert_eq!(
                    once,
                    twice,
                    "{} 不幂等：{raw:?} → {once:?} → {twice:?}",
                    ty.code()
                );
            }
        }
        // 多字节 + 极短串不得 panic（逐类型扫一遍；Regex 用非匹配正则避开参数缺失）
        for ty in MaskType::ALL {
            let param = match ty {
                MaskType::Regex => Some(r"^(\w{2}).*$|1"),
                _ => None,
            };
            for raw in ["", " ", "张", "张三", "…", "🔒", "13812345678"] {
                let _ = apply(*ty, param, raw);
            }
        }
    }

    #[test]
    fn type_codes_are_stable() {
        for (index, ty) in MaskType::ALL.iter().enumerate() {
            assert_eq!(ty.as_i16(), (index + 1) as i16, "mask_type 必须与下标一致");
            assert_eq!(MaskType::from_i16(ty.as_i16()), Some(*ty));
        }
        assert_eq!(MaskType::from_i16(0), None);
        assert_eq!(MaskType::from_i16(8), None);
    }

    #[test]
    fn fixed_point_parsing_is_exact() {
        assert_eq!(parse_fixed("48620.00", 6), Some(48_620_000_000));
        assert_eq!(parse_fixed("0.1", 6), Some(100_000));
        assert_eq!(parse_fixed("-0.1", 6), Some(-100_000));
        assert_eq!(parse_fixed("1.2.3", 6), None);
        assert_eq!(parse_fixed("abc", 6), None);
        // 0.1 + 0.2 这类浮点误差在定点上不存在
        assert_eq!(format_fixed(100_000 + 200_000, 6), "0.3");
    }
}
