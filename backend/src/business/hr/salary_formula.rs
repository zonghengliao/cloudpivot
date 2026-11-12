//! 薪资公式 **DSL 的解析与求值**（`biz_salary_item.formula` · HR-32，设计 §4.4.2）。
//!
//! 设计定稿的手写解析器：**tokenizer → 递归下降 → `Decimal` 求值 → 惰性 `IF`**。
//! 不引第三方表达式库（`AGENTS.md` 约定 3 只允许 `nik`；`evalexpr` / `rhai` 带函数注册、
//! 字符串与循环，安全面远大于本文件）。
//!
//! # 语法（设计 §4.4.2 的 EBNF，逐条实现）
//!
//! ```text
//! expr     := or_expr
//! or_expr  := and_expr ( '||' and_expr )*
//! and_expr := cmp_expr ( '&&' cmp_expr )*
//! cmp_expr := add_expr [ ( '==' | '!=' | '>=' | '<=' | '>' | '<' ) add_expr ]
//! add_expr := mul_expr ( ( '+' | '-' ) mul_expr )*
//! mul_expr := unary   ( ( '*' | '/' ) unary   )*
//! unary    := '-' unary | primary
//! primary  := NUMBER | VAR | FUNC '(' args ')' | '(' expr ')'
//! VAR      := '{' [A-Z][A-Z0-9_]* '}'
//! FUNC     := ROUND(x[,n]) | MIN(a,b) | MAX(a,b) | ABS(x) | CEIL(x) | FLOOR(x) | IF(cond,a,b)
//! NUMBER   := [0-9]+ ('.' [0-9]+)?
//! ```
//!
//! 实现上的三条补充（都是「设计没写死、但必须定」的口径）：
//!
//! 1. **比较不连写**：`{A} < {B} < {C}` 直接报错，要求写成 `{A} < {B} && {B} < {C}`。
//!    EBNF 的 `cmp_expr` 最多一个比较符；连写在 `Excel` 里是合法习惯，静默取左边会算出
//!    「看起来对、其实少一半条件」的数字。
//! 2. **函数名大小写宽容**：`round(...)` 也能解析（客户从 Excel 抄）。配置页门禁
//!    （[`super::salary_item_formula::structural_errors`]）仍然把大小写漂移报成错误，
//!    两层是有意不同的：解析器「能读就得读对」，配置门禁「存进去的必须统一」。
//! 3. **`ROUND` 的小数位上限 = 中间精度**（[`MID_SCALE`]=10）：`ROUND(x, 15)` 会在下一步被
//!    中间的 10 位收口吃掉，允许它只会让客户以为「多写几位更准」。
//!
//! # 求值约定（设计 §4.4.2「每条都有理由」那一表，逐条落地）
//!
//! | 约定 | 本文件的落地 |
//! |---|---|
//! | 全程 `Decimal`，中间 10 位，最终 `ROUND_HALF_UP` 到 2 位 | 每个运算后 [`keep`]（10 位，`MidpointAwayFromZero`）；[`Expr::eval_number`] 出口 2 位 |
//! | 除零 → **报错** | [`ErrorKind::DivideByZero`]，报文点名「应出勤 0 天请用 `IF` 兜底」 |
//! | 未定义变量 → 报错 + 可用变量清单 | [`ErrorKind::UndefinedVar`]，报文带 `{变量}` 与可用清单 |
//! | `IF` 两分支**都不求值**（惰性） | 只求值 `cond` 选中那一支 —— 否则 `IF({ATT_SHOULD_DAYS} == 0, 0, {BASE}/{ATT_SHOULD_DAYS})` 必除零 |
//! | 前端不做求值 | 前端只调接口；本文件是**唯一**的求值实现 |
//!
//! **`&&` / `||` 也短路**（与 `IF` 同口径）：`false && X` 不求值 `X`。
//! 这不改变任何真值（逻辑上是等价的），但能避免「右边写了个不会走到的除零」在真值已知时炸掉。
//!
//! # 精度铁律（设计 §4.4.4 的实测断言，见单测）
//!
//! - `ROUND(8000 / 21.75 * 2, 2) = 735.63`，**不是** `ROUND(367.82 * 2, 2) = 735.64`。
//!   日薪 `367.82` 只是**展示**用的四舍五入；中间结果提前收口就与客户 Excel 差 1 分钱（风险 R2）。
//! - `ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2) = 689.66`（BASE=8000、OT_WD=10 小时）。
//!
//! # 给 HR-33 / HR-34 的接口契约
//!
//! | 谁 | 用什么 | 说明 |
//! |---|---|---|
//! | HR-33（变量解析 / 依赖图 / 未定义变量） | [`Expr`] 只需 [`parse`]；静态检查继续用 [`super::salary_item_formula::extract_refs`] / [`super::salary_item_formula::first_cycle`] / [`super::salary_item_formula::topo_order`] | **不要**另写一套变量口径 |
//! | HR-34（计算引擎） | [`evaluate`]（一步到位）或 [`parse`] + [`Expr::eval_number`]（同一公式算 42 个人时只解析一次） | 变量值用 [`VarScope`]（`BTreeMap` / `HashMap` 已实现） |
//! | HR-44（配置页「检查」按钮） | [`parse`] 的错误报文直接展示 | 语法层；变量可达性另调 `check_formula` |
//!
//! **求值层的「未定义变量」只覆盖真正被求值到的分支**（惰性的必然结果）。配置期的
//! 全量未定义变量检查是 HR-33 / HR-31 的静态检查（`extract_refs` + `undefined_refs`）——
//! 两处都要有，缺一个就会漏。
//!
//! # 已知不一致（本卡不改别的卡的文件，只登记）
//!
//! [`super::salary_item_formula::is_allowed_char`] 的字符白名单里含 `%` `!` `=` `&` `|`
//! 单字符，而设计的 EBNF **没有** `%` 取模、也没有单个 `=`/`!`/`&`/`|`。本解析器对这五个
//! 字符给**可照着改**的报错（「相等要写 `==`」「不支持 `%`，百分数请写 `0.13`」），
//! 结构检查则放行。也就是说：`{BASE} % 2` 能过结构检查（HR-31），但一定过不了解析
//! （本卡）—— 配置页会先后看到两条不同层级的结论。建议后续把白名单收窄到 EBNF 的字符集
//! （改 [`super::salary_item_formula`] 属 HR-31 的文件，本卡不动）。
//!
//! # 错误出口
//!
//! 本文件是**纯函数**（不连库、不碰 `ApiError`）：错误一律 [`FormulaError`]（含
//! [`ErrorKind`] 与可直接展示给用户的中文报文），由调用方决定翻成 handler 的 `ResResult`。

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use nik::prelude::rust_decimal::prelude::FromStr;
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};

use crate::business::hr::salary_item_formula::is_var_name;

// ==================== 常量 ====================

/// 中间结果的小数位（设计 §4.4.2：「中间结果保留 10 位小数」）。
///
/// 为什么是 10 而不是 `Decimal` 的 28 位：28 位会让「同一公式在不同算力/不同版本库」
/// 出现末位差异，而 10 位已经远超工资精度需求（1 分钱 = 2 位），也让
/// `ROUND(8000/21.75*2, 2)` 稳定等于 `735.63`。
pub const MID_SCALE: u32 = 10;

/// 最终金额的小数位（薪资明细列是 `NUMERIC(18,4)`，工资条按 2 位出）。
pub const RESULT_SCALE: u32 = 2;

/// 公式最大**嵌套**层数（括号 / 函数参数 / 一元负号各算一层，防栈溢出）。
///
/// 递归下降「一层括号 = 一帧」。500 字符的公式最多也就几十层，32 已经远超真实配置；
/// 超了报错而不是把进程栈打穿（栈溢出是**进程级**崩溃，会把整个后端拖下水，不是一条 400）。
///
/// 注意：`MAX_DEPTH` 只管**语法嵌套**，不管语法树的**深度**。`1 + 1 + … + 1`
/// （100 项平铺加法）没有任何嵌套，但语法树是一条 100 层深的左倾链 —— 那种情况由
/// [`EVAL_MAX_DEPTH`] 管，别把两者混成一个数（混了就会出现「平铺 40 项加法被判成
/// 嵌套过深」的假拒绝）。
pub const MAX_DEPTH: usize = 32;

/// 求值期的递归深度上限（语法树深度，不是括号层数）。
///
/// 一条 500 字符的公式最多约 250 个操作数 ⇒ 左倾链最坏约 250 层；给到 512 是
/// 「VARCHAR(500) 里能写出来的公式一定算得动」，同时仍然挡住恶意构造的超深树。
/// 512 帧 `eval_inner` 的栈占用远小于线程默认 2 MiB（实测这类纯函数每帧只有几十字节），
/// 不会像解析期那样撑爆栈。
pub const EVAL_MAX_DEPTH: usize = 512;

/// 舍入策略：`ROUND_HALF_UP`。
///
/// `rust_decimal` 把它叫 `MidpointAwayFromZero`（恰好在中点时「离开零」= 0.5 向上、
/// -0.5 向下 = 财务口径的四舍五入）。与 `erp/stockin_base.rs` 的 `ROUNDING` 同一取值。
pub const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 错误 ====================

/// 错误分类（handler 翻状态码 / 前端决定展示样式时用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 空公式。
    Empty,
    /// 字符级：不认识的字符 / 数字写法 / 变量名不合法 / 缺 `}`。
    Token,
    /// 语法级：括号不配对 / 多余内容 / 缺操作数 / 未知函数 / 比较连写。
    Parse,
    /// 函数参数个数不对。
    Arity,
    /// 嵌套过深。
    Depth,
    /// 变量不在作用域里。
    UndefinedVar,
    /// 除数为 0。
    DivideByZero,
    /// 类型不匹配（布尔参与算术 / `IF` 条件不是比较）。
    TypeError,
    /// `Decimal` 溢出（> 28 位有效数字，真实公式不可能，防的是恶意构造）。
    Overflow,
}

impl ErrorKind {
    /// 稳定的机器码（前端 / 测试断言用，不随文案改动）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Empty => "EMPTY",
            Self::Token => "TOKEN",
            Self::Parse => "PARSE",
            Self::Arity => "ARITY",
            Self::Depth => "DEPTH",
            Self::UndefinedVar => "UNDEFINED_VAR",
            Self::DivideByZero => "DIVIDE_BY_ZERO",
            Self::TypeError => "TYPE",
            Self::Overflow => "OVERFLOW",
        }
    }
}

/// 公式错误：分类 + **可直接展示给用户**的中文报文。
///
/// 报文口径照抄设计 §6「公式编辑器」：错在哪、为什么、怎么改（用户点「检查」要能照着改），
/// 而不是 `parse error at 7`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaError {
    kind: ErrorKind,
    message: String,
}

impl FormulaError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// 带「第 N 个字符」定位的语法类错误（`at` 是 1 起的**字符**位置，与
    /// [`super::salary_item_formula::structural_errors`] 的定位口径一致）。
    fn at(kind: ErrorKind, at: usize, message: impl Into<String>) -> Self {
        let message = if at == 0 {
            message.into()
        } else {
            format!("第 {at} 个字符：{}", message.into())
        };
        Self { kind, message }
    }

    /// 错误分类。
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// 中文报文（可能含 `第 N 个字符` / 可用变量清单）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 未定义变量：点名变量 + 列出可用变量（设计 §4.4.2 / §6 的报错口径）。
    fn undefined(name: &str, available: &[String]) -> Self {
        let list = if available.is_empty() {
            "（当前没有任何可用变量：这项公式不能引用 {…}）".to_string()
        } else {
            available.join(" / ")
        };
        Self::new(
            ErrorKind::UndefinedVar,
            format!(
                "公式引用了未定义的变量 `{{{name}}}`：可用变量是「已有薪资项编码 + 考勤变量」—— {list}；\
                 薪资项编码必须是大写（`{{base}}` 也引用不到 `BASE`）"
            ),
        )
    }
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FormulaError {}

// ==================== tokenizer ====================

/// 词法单元的种类。位置信息在 [`Token::at`]（1 起的字符下标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `12` / `1.5`
    Number,
    /// `{BASE}`
    Var,
    /// `ROUND` / `round`（裸标识符，必须是函数名，见 [`Func`]）
    Ident,
    LParen,
    RParen,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    And,
    Or,
}

impl TokenKind {
    /// 展示用符号（报错时报文里点名「`==` 后面缺操作数」）。
    pub const fn symbol(&self) -> &'static str {
        match self {
            Self::Number => "数字",
            Self::Var => "变量",
            Self::Ident => "函数名",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::Comma => ",",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::And => "&&",
            Self::Or => "||",
        }
    }
}

/// 一个词法单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// 种类。
    pub kind: TokenKind,
    /// 原文（`Number` 是数字字面量、`Var` 是花括号里的名字、`Ident` 是函数名原文）。
    pub text: String,
    /// 在公式里的**字符**位置（1 起）。
    pub at: usize,
}

impl Token {
    fn new(kind: TokenKind, text: impl Into<String>, at: usize) -> Self {
        Self {
            kind,
            text: text.into(),
            at,
        }
    }
}

/// 词法分析：公式 → 词法单元序列（空白被丢弃）。
///
/// 这里**不做**语法判断（括号配对、参数个数都不管），只做三件事：
/// 切词、数字/变量名合法性、给每类「写错了」的字符一条能照着改的报错。
///
/// # 为什么 `%` 出现在字符白名单里却在这里报错
///
/// 结构检查（[`super::salary_item_formula::is_allowed_char`]）按设计文档的字符表放行 `%`，
/// 但设计的 EBNF 没有取模运算 —— 到了求值层必须有个确定答案。这里给「请写小数」的报错，
/// 比静默当成 `0` 或等到月底试算才炸都要好。
pub fn tokenize(formula: &str) -> Result<Vec<Token>, FormulaError> {
    let chars: Vec<char> = formula.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        let at = index + 1;
        if ch.is_whitespace() {
            index += 1;
            continue;
        }

        // ---- 数字：`[0-9]+ ('.' [0-9]+)?` ----
        if ch.is_ascii_digit() {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            if index < chars.len() && chars[index] == '.' {
                index += 1;
                if index >= chars.len() || !chars[index].is_ascii_digit() {
                    let literal: String = chars[start..index].iter().collect();
                    return Err(FormulaError::at(
                        ErrorKind::Token,
                        at,
                        format!("数字 `{literal}` 的小数点后面缺数字：小数点两边都要有数字（如 `1.5`、`0.13`）"),
                    ));
                }
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
            }
            let literal: String = chars[start..index].iter().collect();
            tokens.push(Token::new(TokenKind::Number, literal, at));
            continue;
        }

        // ---- 变量：`{NAME}` ----
        if ch == '{' {
            let start = index;
            let mut cursor = index + 1;
            while cursor < chars.len() && chars[cursor] != '}' {
                cursor += 1;
            }
            if cursor >= chars.len() {
                let rest: String = chars[start..].iter().collect();
                return Err(FormulaError::at(
                    ErrorKind::Token,
                    at,
                    format!("变量 `{rest}` 缺少 `}}`：变量必须写成 `{{BASE}}` 这种完整形式"),
                ));
            }
            let name: String = chars[start + 1..cursor].iter().collect();
            if !is_var_name(&name) {
                return Err(FormulaError::at(
                    ErrorKind::Token,
                    at,
                    format!(
                        "变量名 `{{{name}}}` 不合法：只允许大写字母开头 + 大写字母/数字/下划线（例：`{{BASE}}`、`{{ATT_OT_WD}}`）"
                    ),
                ));
            }
            tokens.push(Token::new(TokenKind::Var, name, at));
            index = cursor + 1;
            continue;
        }
        if ch == '}' {
            return Err(FormulaError::at(
                ErrorKind::Token,
                at,
                "多出来的 `}`：变量要写成 `{BASE}`（先左花括号后右花括号）",
            ));
        }

        // ---- 函数名（裸标识符） ----
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let text: String = chars[start..index].iter().collect();
            tokens.push(Token::new(TokenKind::Ident, text, at));
            continue;
        }

        // ---- 双字符运算符优先（`==` 不能切成一 `=` 一 `=`） ----
        let next = chars.get(index + 1).copied();
        let double = match (ch, next) {
            ('=', Some('=')) => Some((TokenKind::Eq, "==")),
            ('!', Some('=')) => Some((TokenKind::Ne, "!=")),
            ('>', Some('=')) => Some((TokenKind::Ge, ">=")),
            ('<', Some('=')) => Some((TokenKind::Le, "<=")),
            ('&', Some('&')) => Some((TokenKind::And, "&&")),
            ('|', Some('|')) => Some((TokenKind::Or, "||")),
            _ => None,
        };
        if let Some((kind, text)) = double {
            tokens.push(Token::new(kind, text, at));
            index += 2;
            continue;
        }

        let single = match ch {
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            ',' => Some(TokenKind::Comma),
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '>' => Some(TokenKind::Gt),
            '<' => Some(TokenKind::Lt),
            _ => None,
        };
        if let Some(kind) = single {
            tokens.push(Token::new(kind, ch.to_string(), at));
            index += 1;
            continue;
        }

        // ---- 剩下的都是「写错了」，逐个给可操作的提示 ----
        let hint = match ch {
            '=' => "`=` 不是比较运算符：相等要写 `==`（单个 `=` 在公式里没有含义）",
            '!' => "`!` 单独不合法：不等要写 `!=`",
            '&' => "`&` 单独不合法：逻辑与要写 `&&`",
            '|' => "`|` 单独不合法：逻辑或要写 `||`",
            '%' => "不支持 `%`：公式 DSL 只有 `+ - * /`（要算百分数请直接写小数，如 `* 0.13`）",
            _ => {
                return Err(FormulaError::at(
                    ErrorKind::Token,
                    at,
                    format!(
                        "`{ch}` 不在允许的字符里：只允许数字、`{{变量}}`、函数 {}、`( ) ,` 与 `+ - * / == != > >= < <= && ||`",
                        Func::names().join(" / ")
                    ),
                ));
            }
        };
        return Err(FormulaError::at(ErrorKind::Token, at, hint));
    }
    Ok(tokens)
}

// ==================== 语法树 ====================

/// 二元算术运算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`（除零报错，不算 0）
    Div,
}

impl BinOp {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
        }
    }
}

/// 比较运算（结果 [`Value::Bool`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

impl CmpOp {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    fn apply(self, left: Decimal, right: Decimal) -> bool {
        match self {
            Self::Eq => left == right,
            Self::Ne => left != right,
            Self::Gt => left > right,
            Self::Ge => left >= right,
            Self::Lt => left < right,
            Self::Le => left <= right,
        }
    }
}

/// 逻辑运算（两边都必须是 [`Value::Bool`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    /// `&&`（短路）
    And,
    /// `||`（短路）
    Or,
}

impl LogicOp {
    const fn symbol(self) -> &'static str {
        match self {
            Self::And => "&&",
            Self::Or => "||",
        }
    }
}

/// 白名单函数（设计 §4.4.2 的 `FUNC` 产生式，逐个抄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    /// `ROUND(x[, n])`，`n` 缺省 2（[`RESULT_SCALE`]），取值 0..=[`MID_SCALE`]
    Round,
    /// `MIN(a, b)`
    Min,
    /// `MAX(a, b)`
    Max,
    /// `ABS(x)`
    Abs,
    /// `CEIL(x)` 向上取整
    Ceil,
    /// `FLOOR(x)` 向下取整
    Floor,
    /// `IF(cond, a, b)` —— **惰性**：只求值选中那一支
    If,
}

impl Func {
    /// 全部函数名（报错时给完整清单，避免用户瞎猜）。
    pub const fn names() -> [&'static str; 7] {
        ["ROUND", "MIN", "MAX", "ABS", "CEIL", "FLOOR", "IF"]
    }

    /// 名字（大写）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Round => "ROUND",
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::Abs => "ABS",
            Self::Ceil => "CEIL",
            Self::Floor => "FLOOR",
            Self::If => "IF",
        }
    }

    /// 签名（报参数个数错误时用）。
    pub const fn signature(self) -> &'static str {
        match self {
            Self::Round => "ROUND(x) 或 ROUND(x, n)",
            Self::Min => "MIN(a, b)",
            Self::Max => "MAX(a, b)",
            Self::Abs => "ABS(x)",
            Self::Ceil => "CEIL(x)",
            Self::Floor => "FLOOR(x)",
            Self::If => "IF(条件, 真值, 假值)",
        }
    }

    /// 按名字查函数（**大小写不敏感**，见模块文档第 2 条）。
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "ROUND" => Some(Self::Round),
            "MIN" => Some(Self::Min),
            "MAX" => Some(Self::Max),
            "ABS" => Some(Self::Abs),
            "CEIL" => Some(Self::Ceil),
            "FLOOR" => Some(Self::Floor),
            "IF" => Some(Self::If),
            _ => None,
        }
    }

    /// 这个函数接受几个参数（`ROUND` 是 1~2）。
    const fn accepts(self, count: usize) -> bool {
        match self {
            Self::Round => count == 1 || count == 2,
            Self::Abs | Self::Ceil | Self::Floor => count == 1,
            Self::Min | Self::Max => count == 2,
            Self::If => count == 3,
        }
    }

    fn arity_error(self, count: usize, at: usize) -> FormulaError {
        FormulaError::at(
            ErrorKind::Arity,
            at,
            format!(
                "函数 `{}` 的参数个数不对（收到 {count} 个）：正确写法是 `{}`",
                self.name(),
                self.signature()
            ),
        )
    }
}

/// 语法树节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 数字字面量。
    Num(Decimal),
    /// `{VAR}` 引用。
    Var(String),
    /// 一元负号。
    Neg(Box<Expr>),
    /// 算术。
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// 比较。
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    /// 逻辑。
    Logic(LogicOp, Box<Expr>, Box<Expr>),
    /// 函数调用（参数个数已在解析期校验）。
    Call(Func, Vec<Expr>),
}

// ==================== 递归下降 ====================

/// 语法分析：公式 → 语法树。
///
/// 解析期就挡住的错误：空公式、未知字符、括号不配对、未知函数、参数个数不对、
/// 比较连写、`ROUND` 字面小数位越界、嵌套过深、结尾多余内容。
/// **不**判的：变量是否存在（求值/静态检查的事）、分支类型是否一致（求值期才知道）。
pub fn parse(formula: &str) -> Result<Expr, FormulaError> {
    if formula.trim().is_empty() {
        return Err(FormulaError::new(
            ErrorKind::Empty,
            "公式不能为空：`calc_type = FORMULA` 的薪资项必须给公式（金额项建议写成 `{BASE}` 这种引用）",
        ));
    }
    let tokens = tokenize(formula)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let expr = parser.parse_or()?;
    if let Some(token) = parser.peek() {
        let message = if matches!(token.kind, TokenKind::RParen) {
            "多出来的 `)`：左括号比右括号少".to_string()
        } else if matches!(token.kind, TokenKind::Comma) {
            "多出来的 `,`：逗号只用在函数的参数之间".to_string()
        } else {
            format!("多余的 `{}`：公式到这里已经结束了（是否漏了运算符？）", token.text)
        };
        return Err(FormulaError::at(ErrorKind::Parse, token.at, message));
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<TokenKind> {
        self.peek().map(|token| token.kind)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    /// 是某类词就吃掉。
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.peek_kind() == Some(kind) {
            self.pos += 1;
            return true;
        }
        false
    }

    /// 必须是某类词，否则报「缺什么」。
    fn expect(
        &mut self,
        kind: TokenKind,
        message: impl FnOnce(Option<&Token>) -> String,
    ) -> Result<Token, FormulaError> {
        match self.next() {
            Some(token) if token.kind == kind => Ok(token),
            other => {
                let at = other.as_ref().map(|token| token.at).unwrap_or(0);
                Err(FormulaError::at(ErrorKind::Parse, at, message(other.as_ref())))
            }
        }
    }

    /// 进一层（括号 / 一元 / 函数参数都会进）。超限报错而不是爆栈。
    fn enter(&mut self) -> Result<(), FormulaError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(FormulaError::new(
                ErrorKind::Depth,
                format!(
                    "公式嵌套超过 {MAX_DEPTH} 层：请拆成多个薪资项（中间值用别的 item_code 承载），\
                     嵌套过深的公式解析会撑爆调用栈"
                ),
            ));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// `or_expr := and_expr ( '||' and_expr )*`
    ///
    /// **不在这里计层**：进到本层的原因（`(` / 函数参数 / 一元 `-`）已经在调用点
    /// [`Self::enter`] 过；这里再计一次就成了「一层括号 = 2 帧」，[`MAX_DEPTH`] 实际只剩一半
    /// —— 实测 30 层括号会被误判成 60 层而报「嵌套过深」。
    fn parse_or(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_and()?;
        while self.eat(TokenKind::Or) {
            let right = self.parse_and()?;
            left = Expr::Logic(LogicOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `and_expr := cmp_expr ( '&&' cmp_expr )*`
    fn parse_and(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_cmp()?;
        while self.eat(TokenKind::And) {
            let right = self.parse_cmp()?;
            left = Expr::Logic(LogicOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `cmp_expr := add_expr [ cmp_op add_expr ]` —— **最多一个**，连写报错。
    fn parse_cmp(&mut self) -> Result<Expr, FormulaError> {
        let left = self.parse_add()?;
        let Some((op, at)) = self.peek_cmp_op() else {
            return Ok(left);
        };
        self.pos += 1;
        let right = self.parse_add()?;
        if let Some((_, next_at)) = self.peek_cmp_op() {
            return Err(FormulaError::at(
                ErrorKind::Parse,
                next_at,
                format!(
                    "比较运算符不能连写（第 {at} 个字符的 `{}` 与这里的比较连在一起了）：\
                     请用 `&&` 连接两个比较，例如 `{{A}} < {{B}} && {{B}} < {{C}}`",
                    op.symbol()
                ),
            ));
        }
        Ok(Expr::Cmp(op, Box::new(left), Box::new(right)))
    }

    fn peek_cmp_op(&self) -> Option<(CmpOp, usize)> {
        let token = self.peek()?;
        let op = match token.kind {
            TokenKind::Eq => CmpOp::Eq,
            TokenKind::Ne => CmpOp::Ne,
            TokenKind::Gt => CmpOp::Gt,
            TokenKind::Ge => CmpOp::Ge,
            TokenKind::Lt => CmpOp::Lt,
            TokenKind::Le => CmpOp::Le,
            _ => return None,
        };
        Some((op, token.at))
    }

    /// `add_expr := mul_expr ( ( '+' | '-' ) mul_expr )*` —— 左结合。
    fn parse_add(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Plus) => BinOp::Add,
                Some(TokenKind::Minus) => BinOp::Sub,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    /// `mul_expr := unary ( ( '*' | '/' ) unary )*` —— 左结合。
    fn parse_mul(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Star) => BinOp::Mul,
                Some(TokenKind::Slash) => BinOp::Div,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    /// `unary := '-' unary | primary`
    fn parse_unary(&mut self) -> Result<Expr, FormulaError> {
        if self.peek_kind() == Some(TokenKind::Minus) {
            self.pos += 1;
            self.enter()?;
            let result = self.parse_unary();
            self.leave();
            return Ok(Expr::Neg(Box::new(result?)));
        }
        self.parse_primary()
    }

    /// `primary := NUMBER | VAR | FUNC '(' args ')' | '(' expr ')'`
    fn parse_primary(&mut self) -> Result<Expr, FormulaError> {
        let Some(token) = self.next() else {
            return Err(FormulaError::new(
                ErrorKind::Parse,
                "公式在这里就结束了，缺一个操作数（数字 / `{变量}` / 函数）：是否漏了右半边？",
            ));
        };
        match token.kind {
            TokenKind::Number => {
                let value = Decimal::from_str(&token.text).map_err(|_| {
                    FormulaError::at(ErrorKind::Token, token.at, format!("数字 `{}` 无法解析", token.text))
                })?;
                Ok(Expr::Num(value))
            }
            TokenKind::Var => Ok(Expr::Var(token.text)),
            TokenKind::Ident => self.parse_call(&token),
            TokenKind::LParen => {
                self.enter()?;
                let result = self.parse_or();
                self.leave();
                let inner = result?;
                self.expect(TokenKind::RParen, |found| match found {
                    Some(found) => format!("这里应该是 `)`，收到 `{}`", found.text),
                    None => "公式结尾缺一个 `)`：左括号没有被关闭".to_string(),
                })?;
                Ok(inner)
            }
            TokenKind::Plus => Err(FormulaError::at(
                ErrorKind::Parse,
                token.at,
                "一元 `+` 不支持：正数直接写数字（`5`），要相加请写 `a + b`",
            )),
            TokenKind::RParen => Err(FormulaError::at(
                ErrorKind::Parse,
                token.at,
                "多出来的 `)`：这里应该是一个操作数（数字 / `{变量}` / 函数）",
            )),
            TokenKind::Comma => Err(FormulaError::at(
                ErrorKind::Parse,
                token.at,
                "`,` 前面缺一个参数：逗号只用在函数的参数之间",
            )),
            other => Err(FormulaError::at(
                ErrorKind::Parse,
                token.at,
                format!(
                    "运算符 `{}` 前面缺操作数：公式不能以运算符开头或连着两个运算符",
                    other.symbol()
                ),
            )),
        }
    }

    /// 函数调用：参数个数在**解析期**校验（配错参数个数不该等试算）。
    fn parse_call(&mut self, token: &Token) -> Result<Expr, FormulaError> {
        let Some(func) = Func::from_name(&token.text) else {
            return Err(FormulaError::at(
                ErrorKind::Parse,
                token.at,
                format!(
                    "`{}` 既不是函数也不是变量：函数只能是 {}；引用薪资项/考勤要写成 `{{{}}}`",
                    token.text,
                    Func::names().join(" / "),
                    token.text.to_ascii_uppercase()
                ),
            ));
        };
        self.expect(TokenKind::LParen, |found| match found {
            Some(found) => format!("函数 `{}` 后面要跟 `(`，收到 `{}`", func.name(), found.text),
            None => format!("函数 `{}` 后面缺 `(` 与参数", func.name()),
        })?;
        let mut args: Vec<Expr> = Vec::new();
        if !self.eat(TokenKind::RParen) {
            loop {
                self.enter()?;
                let result = self.parse_or();
                self.leave();
                args.push(result?);
                if self.eat(TokenKind::Comma) {
                    continue;
                }
                self.expect(TokenKind::RParen, |found| match found {
                    Some(found) => format!(
                        "函数 `{}` 的参数列表应该是 `)`（收到 `{}`）：参数之间用 `,` 分隔",
                        func.name(),
                        found.text
                    ),
                    None => format!("函数 `{}` 缺少 `)`：参数列表没有关闭", func.name()),
                })?;
                break;
            }
        }
        if !func.accepts(args.len()) {
            return Err(func.arity_error(args.len(), token.at));
        }
        if func == Func::Round {
            // `ROUND(x, 2)` 里的小数位：字面量在解析期就查（配置页存之前就能挡），
            // 表达式形式（`ROUND(x, 1+1)`）留给求值期查。
            if let Some(Expr::Num(scale)) = args.get(1) {
                scale_of(*scale, token.at)?;
            }
        }
        Ok(Expr::Call(func, args))
    }
}

// ==================== 求值 ====================

/// 求值结果：数值或布尔。
///
/// 布尔只出现在「比较 / 逻辑」节点上，薪资项金额必须是数值 ——
/// 所以 [`Expr::eval_number`] 遇到布尔会报错，而不是把 `true` 当 1。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// 数值（`Decimal`，精度见 [`MID_SCALE`]）。
    Num(Decimal),
    /// 布尔（比较 / 逻辑的结果，只能喂给 `IF` 的条件或 `&&` / `||`）。
    Bool(bool),
}

/// 变量值来源（HR-34 计算引擎按人逐项填进来）。
///
/// 只要「按名字取值」与「有哪些名字」两件事：
/// - [`Self::value_of`] 取不到 → 求值报 [`ErrorKind::UndefinedVar`]；
/// - [`Self::available`] 只用于组报错报文（设计 §4.4.2 要求带可用清单）。
pub trait VarScope {
    /// 取变量值（`None` = 未定义）。
    fn value_of(&self, name: &str) -> Option<Decimal>;

    /// 可用变量名（**排序后**返回，报文稳定可比对）。
    fn available(&self) -> Vec<String>;
}

impl VarScope for BTreeMap<String, Decimal> {
    fn value_of(&self, name: &str) -> Option<Decimal> {
        self.get(name).copied()
    }

    fn available(&self) -> Vec<String> {
        self.keys().cloned().collect()
    }
}

impl VarScope for HashMap<String, Decimal> {
    fn value_of(&self, name: &str) -> Option<Decimal> {
        self.get(name).copied()
    }

    fn available(&self) -> Vec<String> {
        let mut names: Vec<String> = self.keys().cloned().collect();
        names.sort();
        names
    }
}

/// 中间结果收口到 [`MID_SCALE`]：设计 §4.4.2「中间结果保留 10 位小数」。
///
/// 每个运算后都收口（而不是等到最后）：这样同一公式在任何机器、任何 `rust_decimal`
/// 版本上都算出同一个数，且不会被 28 位有效数字的上限「默默截断」。
fn keep(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(MID_SCALE, ROUNDING)
}

/// `ROUND` 的小数位：必须是 `0..=MID_SCALE` 的整数。
fn scale_of(scale: Decimal, at: usize) -> Result<u32, FormulaError> {
    if scale.fract() != Decimal::ZERO {
        return Err(FormulaError::at(
            ErrorKind::Parse,
            at,
            format!("`ROUND` 的第二个参数（小数位数）必须是整数，收到 `{scale}`"),
        ));
    }
    if scale < Decimal::ZERO || scale > Decimal::from(MID_SCALE) {
        return Err(FormulaError::at(
            ErrorKind::Parse,
            at,
            format!(
                "`ROUND` 的小数位数只能是 0~{MID_SCALE}（收到 `{scale}`）：中间精度是 {MID_SCALE} 位，再多的位会在下一步被收口吃掉"
            ),
        ));
    }
    u32::try_from(scale)
        .map_err(|_| FormulaError::at(ErrorKind::Parse, at, format!("`ROUND` 的小数位数 `{scale}` 无法解析")))
}

impl Expr {
    /// 求值，返回 [`Value`]（可能是布尔；`IF` 的条件要用它）。
    ///
    /// **惰性**：`IF` 只求值被选中那一支；`&&` / `||` 左值能定结果时不求右值。
    /// 因此「未求值分支里的除零 / 未定义变量」不会报错 —— 这是设计要的效果
    /// （`IF({ATT_SHOULD_DAYS} == 0, 0, {BASE}/{ATT_SHOULD_DAYS})` 必须能算）。
    pub fn eval(&self, vars: &dyn VarScope) -> Result<Value, FormulaError> {
        self.eval_inner(vars, 0)
    }

    /// 求值成**金额**：非数值结果报错，出口按 [`RESULT_SCALE`] 收口（`ROUND_HALF_UP`）。
    ///
    /// 落库金额（`biz_salary_detail_item.amount` / `biz_salary_detail.gross_amount`）
    /// 一律用这个出口。
    pub fn eval_number(&self, vars: &dyn VarScope) -> Result<Decimal, FormulaError> {
        match self.eval(vars)? {
            Value::Num(value) => Ok(value.round_dp_with_strategy(RESULT_SCALE, ROUNDING)),
            Value::Bool(value) => Err(FormulaError::new(
                ErrorKind::TypeError,
                format!(
                    "公式的结果是布尔值 `{value}`（比较/逻辑运算）：薪资项金额必须是数值，\
                     比较只能出现在 `IF(条件, 真值, 假值)` 的条件里"
                ),
            )),
        }
    }

    /// 求值成**未按 2 位收口**的数值（保留 [`MID_SCALE`] 位）。
    ///
    /// 只给「按人导出计算过程」（`calc_note`）/ 排错核对用；**落库金额不要用这个**，
    /// 否则 `NUMERIC(18,4)` 里会留下 10 位小数，工资条四舍五入后与凭证差 1 分。
    pub fn eval_exact(&self, vars: &dyn VarScope) -> Result<Decimal, FormulaError> {
        match self.eval(vars)? {
            Value::Num(value) => Ok(value),
            Value::Bool(value) => Err(FormulaError::new(
                ErrorKind::TypeError,
                format!("公式的结果是布尔值 `{value}`：这里要的是数值"),
            )),
        }
    }

    fn eval_inner(&self, vars: &dyn VarScope, depth: usize) -> Result<Value, FormulaError> {
        if depth > EVAL_MAX_DEPTH {
            return Err(FormulaError::new(
                ErrorKind::Depth,
                format!("公式的语法树深度超过 {EVAL_MAX_DEPTH} 层（平铺的运算太多）：请拆成多个薪资项"),
            ));
        }
        match self {
            Self::Num(value) => Ok(Value::Num(*value)),
            Self::Var(name) => match vars.value_of(name) {
                Some(value) => Ok(Value::Num(value)),
                None => Err(FormulaError::undefined(name, &vars.available())),
            },
            Self::Neg(inner) => {
                let value = Self::expect_num(inner.eval_inner(vars, depth + 1)?, "一元 `-`")?;
                // `Decimal` 的取负不会溢出（有符号位对称），不需要 checked_neg
                Ok(Value::Num(-value))
            }
            Self::Bin(op, left, right) => {
                let left = Self::expect_num(left.eval_inner(vars, depth + 1)?, op.symbol())?;
                let right = Self::expect_num(right.eval_inner(vars, depth + 1)?, op.symbol())?;
                let result = match op {
                    BinOp::Add => left.checked_add(right),
                    BinOp::Sub => left.checked_sub(right),
                    BinOp::Mul => left.checked_mul(right),
                    BinOp::Div => {
                        if right == Decimal::ZERO {
                            return Err(FormulaError::new(
                                ErrorKind::DivideByZero,
                                format!(
                                    "除零：`/` 右边的值是 0（`{left}` / 0）。薪资公式里除零必须报错，\
                                     不能静默算 0 —— 应出勤 0 天这类场景请用 `IF({{ATT_SHOULD_DAYS}} == 0, 0, …)` 兜底"
                                ),
                            ));
                        }
                        left.checked_div(right)
                    }
                };
                result.map(|value| Value::Num(keep(value))).ok_or_else(|| {
                    FormulaError::new(ErrorKind::Overflow, format!("`{}` 运算结果超出数值范围", op.symbol()))
                })
            }
            Self::Cmp(op, left, right) => {
                let left = Self::expect_num(left.eval_inner(vars, depth + 1)?, op.symbol())?;
                let right = Self::expect_num(right.eval_inner(vars, depth + 1)?, op.symbol())?;
                Ok(Value::Bool(op.apply(left, right)))
            }
            Self::Logic(op, left, right) => {
                let left_value = match left.eval_inner(vars, depth + 1)? {
                    Value::Bool(value) => value,
                    Value::Num(value) => return Err(Self::logic_type_error(*op, "左边", value)),
                };
                // 短路：`false && X` / `true || X` 不求值右边（与 IF 同口径，见模块文档）
                match (op, left_value) {
                    (LogicOp::And, false) => Ok(Value::Bool(false)),
                    (LogicOp::Or, true) => Ok(Value::Bool(true)),
                    _ => match right.eval_inner(vars, depth + 1)? {
                        Value::Bool(value) => Ok(Value::Bool(value)),
                        Value::Num(value) => Err(Self::logic_type_error(*op, "右边", value)),
                    },
                }
            }
            Self::Call(func, args) => Self::eval_call(*func, args, vars, depth),
        }
    }

    fn eval_call(func: Func, args: &[Expr], vars: &dyn VarScope, depth: usize) -> Result<Value, FormulaError> {
        match func {
            Func::If => {
                // 条件先求；两支**只求一支**（设计 §4.4.2 的惰性约定）
                let condition = match args[0].eval_inner(vars, depth + 1)? {
                    Value::Bool(value) => value,
                    Value::Num(value) => {
                        return Err(FormulaError::new(
                            ErrorKind::TypeError,
                            format!(
                                "`IF` 的第一个参数必须是比较结果（如 `{{ATT_ABSENT_DAYS}} == 0`），\
                                 收到数值 `{value}`：如果要判断「不为 0」，请写 `!= 0`"
                            ),
                        ));
                    }
                };
                let branch = if condition { &args[1] } else { &args[2] };
                let value = branch.eval_inner(vars, depth + 1)?;
                match value {
                    Value::Num(number) => Ok(Value::Num(number)),
                    Value::Bool(_) => Err(FormulaError::new(
                        ErrorKind::TypeError,
                        "`IF` 的真/假分支必须是数值（金额）：分支里写成了比较表达式",
                    )),
                }
            }
            Func::Round => {
                let value = Self::num_arg(args, 0, vars, depth, func)?;
                let scale = match args.get(1) {
                    None => RESULT_SCALE,
                    Some(expr) => {
                        let raw = expr.eval_inner(vars, depth + 1)?;
                        let raw = match raw {
                            Value::Num(number) => number,
                            Value::Bool(_) => {
                                return Err(FormulaError::new(
                                    ErrorKind::TypeError,
                                    "`ROUND` 的第二个参数（小数位数）必须是数值",
                                ));
                            }
                        };
                        scale_of(raw, 0)?
                    }
                };
                Ok(Value::Num(value.round_dp_with_strategy(scale, ROUNDING)))
            }
            Func::Min => {
                let left = Self::num_arg(args, 0, vars, depth, func)?;
                let right = Self::num_arg(args, 1, vars, depth, func)?;
                Ok(Value::Num(if left <= right { left } else { right }))
            }
            Func::Max => {
                let left = Self::num_arg(args, 0, vars, depth, func)?;
                let right = Self::num_arg(args, 1, vars, depth, func)?;
                Ok(Value::Num(if left >= right { left } else { right }))
            }
            Func::Abs => {
                let value = Self::num_arg(args, 0, vars, depth, func)?;
                Ok(Value::Num(value.abs()))
            }
            Func::Ceil => {
                let value = Self::num_arg(args, 0, vars, depth, func)?;
                Ok(Value::Num(value.ceil()))
            }
            Func::Floor => {
                let value = Self::num_arg(args, 0, vars, depth, func)?;
                Ok(Value::Num(value.floor()))
            }
        }
    }

    fn num_arg(
        args: &[Expr],
        index: usize,
        vars: &dyn VarScope,
        depth: usize,
        func: Func,
    ) -> Result<Decimal, FormulaError> {
        match args[index].eval_inner(vars, depth + 1)? {
            Value::Num(value) => Ok(value),
            Value::Bool(_) => Err(FormulaError::new(
                ErrorKind::TypeError,
                format!(
                    "函数 `{}` 的第 {} 个参数必须是数值：分支里写成了比较表达式",
                    func.name(),
                    index + 1
                ),
            )),
        }
    }

    fn logic_type_error(op: LogicOp, side: &str, value: Decimal) -> FormulaError {
        FormulaError::new(
            ErrorKind::TypeError,
            format!(
                "逻辑运算符 `{}` 的{side}是数值 `{value}`：两边必须是比较结果（如 `{{ATT_ABSENT_DAYS}} == 0`），\
                 数值请先写成比较式",
                op.symbol()
            ),
        )
    }

    fn expect_num(value: Value, what: &str) -> Result<Decimal, FormulaError> {
        match value {
            Value::Num(number) => Ok(number),
            Value::Bool(flag) => Err(FormulaError::new(
                ErrorKind::TypeError,
                format!(
                    "运算符 `{what}` 的操作数是布尔值 `{flag}`（比较结果）：比较只能出现在 `IF(…)` 的条件里，\
                     不能参与算术"
                ),
            )),
        }
    }
}

/// 一步到位：解析 + 求值 + 按 2 位收口（最常见用法）。
///
/// 同一公式要算很多人（HR-34 试算 42 人 × 15 项）时，先 [`parse`] 一次再反复
/// [`Expr::eval_number`]，别在循环里调本函数。
pub fn evaluate(formula: &str, vars: &dyn VarScope) -> Result<Decimal, FormulaError> {
    parse(formula)?.eval_number(vars)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::salary_item_formula;

    /// 测试用金额字面量（`Decimal::from_str`，**不用 f64**：`0.1+0.2` 那套坑不进单测）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试字面量必须是合法 Decimal")
    }

    /// 测试用作用域：`&[("BASE", "8000"), …]`。
    fn scope(pairs: &[(&str, &str)]) -> BTreeMap<String, Decimal> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), dec(value)))
            .collect()
    }

    /// 设计 §4.4.4 的张三（BASE=8000 / POST=500 / MEAL=300 / PERF=1200 / 考勤 2 天事假 / 加班 10 小时）。
    fn zhangsan() -> BTreeMap<String, Decimal> {
        scope(&[
            ("BASE", "8000"),
            ("POST", "500"),
            ("MEAL", "300"),
            ("PERF", "1200"),
            ("OT_WE", "0"),
            ("OT_HO", "0"),
            ("SI_PERSONAL", "840"),
            ("HF_PERSONAL", "960"),
            ("IIT", "0"),
            ("OTHER_WITHHOLD", "0"),
            ("ATT_SHOULD_DAYS", "22"),
            ("ATT_ACTUAL_DAYS", "20"),
            ("ATT_LEAVE_UNPAID_DAYS", "2"),
            ("ATT_ABSENT_DAYS", "0"),
            ("ATT_OT_WD", "10"),
            ("ATT_OT_WE", "0"),
            ("ATT_OT_HO", "0"),
        ])
    }

    // ---- 1. 优先级 / 结合性 ----

    /// 乘除先于加减、括号优先、同优先级**左结合**（`10-4-3=3` 而不是 `9`）。
    #[test]
    fn precedence_and_left_associativity() {
        let vars = scope(&[]);
        assert_eq!(evaluate("1 + 2 * 3", &vars).expect("1+2*3"), dec("7"));
        assert_eq!(evaluate("(1 + 2) * 3", &vars).expect("(1+2)*3"), dec("9"));
        assert_eq!(evaluate("10 - 4 - 3", &vars).expect("左结合"), dec("3"));
        assert_eq!(evaluate("100 / 10 / 5", &vars).expect("左结合除法"), dec("2"));
        assert_eq!(evaluate("2 + 3 * 4 - 6 / 3", &vars).expect("混合"), dec("12"));
    }

    // ---- 2. 负数 / 一元负号 ----

    /// 负数与一元负号：`-2*3=-6`、`10--5=15`、`{BASE} * -1` 取负。
    #[test]
    fn negative_numbers_and_unary_minus() {
        let vars = scope(&[("BASE", "8000")]);
        assert_eq!(evaluate("-2 * 3", &vars).expect("前缀负号"), dec("-6"));
        assert_eq!(evaluate("10 - -5", &vars).expect("减负数"), dec("15"));
        assert_eq!(evaluate("{BASE} * -1", &vars).expect("乘负数"), dec("-8000"));
        // 一元负号比乘除更紧：`-2*3` 是 `(-2)*3`，不是 `-(2*3)` 的差别体现不出来，
        // 但 `-{BASE}/2` 必须是 -4000
        assert_eq!(evaluate("-{BASE} / 2", &vars).expect("负号先算"), dec("-4000"));
    }

    // ---- 3. 精度铁律（设计 §4.4.4 / 风险 R2） ----

    /// **中间结果不许提前四舍五入**：`ROUND(8000/21.75*2, 2)=735.63`，
    /// 若先把日薪收成 `367.82` 再乘 2 会得到 `735.64`（与客户 Excel 差 1 分钱）。
    #[test]
    fn no_premature_rounding_in_middle_steps() {
        let vars = scope(&[("BASE", "8000")]);
        let whole = evaluate("ROUND({BASE} / 21.75 * 2, 2)", &vars).expect("整链");
        assert_eq!(whole, dec("735.63"), "中间 10 位精度：先算完再收口");
        // 反证：提前收口（= 客户手工 Excel 的常见写法）会得到 735.64
        let premature = evaluate("ROUND(ROUND({BASE} / 21.75, 2) * 2, 2)", &vars).expect("提前收口");
        assert_eq!(premature, dec("735.64"), "这就是 1 分钱差异的来源（R2）");
        assert_ne!(whole, premature, "两条路径必须可区分");
    }

    /// 设计 §4.4.4 的加班费：`ROUND({BASE}/21.75/8*{ATT_OT_WD}*1.5, 2) = 689.66`。
    #[test]
    fn overtime_formula_matches_design_worked_example() {
        let vars = zhangsan();
        let ot = evaluate("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)", &vars).expect("平时加班费");
        assert_eq!(ot, dec("689.66"), "设计 §4.4.4 实测值");
        // 日薪 / 时薪的中间值（10 位）：367.8160919540 / 45.9770114943
        // 用 `eval_exact`：`evaluate` 是**落库金额出口**（2 位收口），拿它断言 10 位必然红。
        assert_eq!(
            parse("ROUND({BASE} / 21.75, 10)")
                .expect("日薪公式")
                .eval_exact(&vars)
                .expect("日薪"),
            dec("367.8160919540")
        );
        assert_eq!(
            parse("ROUND({BASE} / 21.75 / 8, 10)")
                .expect("时薪公式")
                .eval_exact(&vars)
                .expect("时薪"),
            dec("45.9770114943")
        );
    }

    /// 最终收口是 `ROUND_HALF_UP`：`1/8 = 0.125 → 0.13`（银行家舍入会给 0.12）。
    #[test]
    fn final_result_rounds_half_up() {
        let vars = scope(&[]);
        assert_eq!(evaluate("1 / 8", &vars).expect("0.125"), dec("0.13"), "0.125 必须进位");
        assert_eq!(
            evaluate("1 / 200", &vars).expect("0.005"),
            dec("0.01"),
            "0.005 必须进位"
        );
        // 负数侧：-0.125 → -0.13（away from zero，不是向零截断）
        assert_eq!(evaluate("-1 / 8", &vars).expect("-0.125"), dec("-0.13"));
        // 未按 2 位收口的出口保留 10 位（`evaluate` 已经收口到 2 位，所以这里必须走 `eval_exact`）
        let exact = parse("1 / 8").expect("解析").eval_exact(&vars).expect("exact 求值");
        assert_eq!(exact, dec("0.125"), "eval_exact 不收口到 2 位");
    }

    // ---- 4. 除零 ----

    /// 除零**报错**（不是 0、不是 NULL）：报文体说明「请用 IF 兜底」。
    #[test]
    fn division_by_zero_is_an_error() {
        let vars = scope(&[("BASE", "8000"), ("ATT_SHOULD_DAYS", "0")]);
        let error = evaluate("{BASE} / 0", &vars).expect_err("除零必须报错");
        assert_eq!(error.kind(), ErrorKind::DivideByZero);
        assert!(error.message().contains("除零"), "{}", error.message());
        assert!(error.message().contains("IF"), "报文要给兜底写法：{}", error.message());

        let error = evaluate("{BASE} / {ATT_SHOULD_DAYS}", &vars).expect_err("变量除零也要报错");
        assert_eq!(error.kind(), ErrorKind::DivideByZero);
        assert_eq!(ErrorKind::DivideByZero.code(), "DIVIDE_BY_ZERO");
    }

    // ---- 5. 惰性 IF ----

    /// 设计 §4.4.2 的典型场景：应出勤 0 天时 `IF` 必须短路，否则必除零。
    #[test]
    fn if_is_lazy_and_avoids_division_by_zero() {
        let vars = scope(&[("BASE", "8000"), ("ATT_SHOULD_DAYS", "0")]);
        let formula = "IF({ATT_SHOULD_DAYS} == 0, 0, {BASE} / {ATT_SHOULD_DAYS})";
        assert_eq!(evaluate(formula, &vars).expect("短路"), Decimal::ZERO);
        // 同一公式在应出勤 22 天时走另一支
        let vars = scope(&[("BASE", "8000"), ("ATT_SHOULD_DAYS", "22")]);
        assert_eq!(evaluate(formula, &vars).expect("非零分支"), dec("363.64"));
    }

    /// 惰性的必然结果：**未选中分支里的未定义变量不报错**（配置期由静态检查兜底，见模块文档）。
    #[test]
    fn if_does_not_touch_undefined_var_in_untaken_branch() {
        let vars = scope(&[("ATT_ABSENT_DAYS", "0")]);
        let bonus =
            evaluate("IF({ATT_ABSENT_DAYS} == 0, 200, {NOT_DEFINED_ANYWHERE})", &vars).expect("未选中分支不求值");
        assert_eq!(bonus, dec("200"));
        // 反过来选中那一支就必须报未定义变量
        let vars = scope(&[("ATT_ABSENT_DAYS", "1")]);
        let error = evaluate("IF({ATT_ABSENT_DAYS} == 0, 200, {NOT_DEFINED_ANYWHERE})", &vars)
            .expect_err("选中分支必须报未定义");
        assert_eq!(error.kind(), ErrorKind::UndefinedVar);
    }

    /// `IF` 的第一个参数必须是**比较结果**，写数值要给出可照着改的提示。
    #[test]
    fn if_condition_must_be_a_comparison() {
        let vars = scope(&[("BASE", "8000")]);
        let error = evaluate("IF({BASE}, 1, 2)", &vars).expect_err("数值条件必须报错");
        assert_eq!(error.kind(), ErrorKind::TypeError);
        assert!(error.message().contains("比较"), "{}", error.message());
        // 三目嵌套是合法的（参数就是表达式）
        assert_eq!(
            evaluate("IF({BASE} > 9000, 1, IF({BASE} > 5000, 2, 3))", &vars).expect("嵌套 IF"),
            dec("2")
        );
    }

    // ---- 6. 比较 / 逻辑 ----

    /// 六个比较符都要能算，且结果是布尔。
    #[test]
    fn comparison_operators_return_bool() {
        let vars = scope(&[("A", "3"), ("B", "3")]);
        let cases = [
            ("{A} == {B}", true),
            ("{A} != {B}", false),
            ("{A} >= {B}", true),
            ("{A} <= {B}", true),
            ("{A} > {B}", false),
            ("{A} < {B}", false),
        ];
        for (formula, expected) in cases {
            assert_eq!(
                parse(formula).expect("解析").eval(&vars).expect("求值"),
                Value::Bool(expected),
                "{formula}"
            );
        }
        // 比较符没空格也要认（`{A}==3`）
        assert_eq!(
            parse("{A}==3").expect("解析").eval(&vars).expect("求值"),
            Value::Bool(true)
        );
    }

    /// `&&` / `||` 短路：右边写了个必然除零，左值已定结果时不许炸。
    #[test]
    fn logic_operators_short_circuit() {
        let vars = scope(&[("ATT_ABSENT_DAYS", "1")]);
        let false_and = parse("{ATT_ABSENT_DAYS} == 0 && 1 / 0 == 0")
            .expect("解析")
            .eval(&vars)
            .expect("false && … 不求右边");
        assert_eq!(false_and, Value::Bool(false));

        let vars = scope(&[("ATT_ABSENT_DAYS", "0")]);
        let true_or = parse("{ATT_ABSENT_DAYS} == 0 || 1 / 0 == 0")
            .expect("解析")
            .eval(&vars)
            .expect("true || … 不求右边");
        assert_eq!(true_or, Value::Bool(true));

        // 两边都要求值时不短路 → 除零照常报错
        let vars = scope(&[("ATT_ABSENT_DAYS", "1")]);
        let error = parse("{ATT_ABSENT_DAYS} == 0 || 1 / 0 == 0")
            .expect("解析")
            .eval(&vars)
            .expect_err("false || … 必须求右边");
        assert_eq!(error.kind(), ErrorKind::DivideByZero);
    }

    /// 设计 §4.4.3 的全勤奖公式：无旷工且无无薪假 → 200，否则 0。
    #[test]
    fn attendance_bonus_seed_formula() {
        let formula = "IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)";
        let clean = scope(&[("ATT_ABSENT_DAYS", "0"), ("ATT_LEAVE_UNPAID_DAYS", "0")]);
        assert_eq!(evaluate(formula, &clean).expect("全勤"), dec("200"));
        // 设计 §4.4.4 的张三有 2 天事假 → 0
        assert_eq!(evaluate(formula, &zhangsan()).expect("有假"), dec("0"));
        let late = scope(&[("ATT_ABSENT_DAYS", "1"), ("ATT_LEAVE_UNPAID_DAYS", "0")]);
        assert_eq!(evaluate(formula, &late).expect("旷工"), dec("0"));
    }

    // ---- 7. 函数 ----

    /// `MIN` / `MAX` / `ABS` / `CEIL` / `FLOOR` 的值（含负数侧取整语义）。
    #[test]
    fn scalar_functions_behave() {
        let vars = scope(&[]);
        assert_eq!(evaluate("MIN(3, 5)", &vars).expect("MIN"), dec("3"));
        assert_eq!(evaluate("MAX(3, 5)", &vars).expect("MAX"), dec("5"));
        assert_eq!(evaluate("MIN(-3, 5)", &vars).expect("MIN 负数"), dec("-3"));
        assert_eq!(evaluate("ABS(-2.5)", &vars).expect("ABS"), dec("2.5"));
        assert_eq!(evaluate("CEIL(2.1)", &vars).expect("CEIL"), dec("3"));
        assert_eq!(evaluate("FLOOR(2.9)", &vars).expect("FLOOR"), dec("2"));
        // 负数侧：CEIL 向 +∞（-2.1 → -2）、FLOOR 向 -∞（-2.1 → -3）
        assert_eq!(evaluate("CEIL(-2.1)", &vars).expect("CEIL 负数"), dec("-2"));
        assert_eq!(evaluate("FLOOR(-2.1)", &vars).expect("FLOOR 负数"), dec("-3"));
        // 函数可嵌套
        assert_eq!(evaluate("MAX(ABS(-7), CEIL(2.1))", &vars).expect("嵌套"), dec("7"));
    }

    /// `ROUND` 的一位/两位写法、缺省 2 位、以及小数位非法（越界 / 非整数）。
    #[test]
    fn round_scale_rules() {
        let vars = scope(&[("BASE", "8000")]);
        assert_eq!(evaluate("ROUND(1 / 3)", &vars).expect("缺省 2 位"), dec("0.33"));
        // 4 位 / 10 位一律走 `eval_exact`：`ROUND(x, 4)` 作为**整条公式**时落库出口仍收口到 2 位
        // （金额列只存到分），10 位只在「下一步还要接着算」时才存在。
        assert_eq!(
            parse("ROUND(1 / 3, 4)").expect("解析").eval_exact(&vars).expect("4 位"),
            dec("0.3333")
        );
        assert_eq!(
            parse("ROUND({BASE} / 21.75, 10)")
                .expect("解析")
                .eval_exact(&vars)
                .expect("10 位"),
            dec("367.8160919540")
        );
        assert_eq!(evaluate("ROUND(1 / 3, 4)", &vars).expect("整条公式落库"), dec("0.33"));
        // 越界：11 位（> MID_SCALE）在解析期就拦
        let error = parse("ROUND({BASE}, 11)").expect_err("11 位非法");
        assert_eq!(error.kind(), ErrorKind::Parse);
        assert!(error.message().contains("0~10"), "{}", error.message());
        // 非整数位数：解析期（字面量）
        let error = parse("ROUND({BASE}, 1.5)").expect_err("小数位数必须是整数");
        assert!(error.message().contains("整数"), "{}", error.message());
        // 非整数位数：求值期（表达式算出来的）
        let error = evaluate("ROUND({BASE}, 1 + 0.5)", &vars).expect_err("求值期也要拦");
        assert!(error.message().contains("整数"), "{}", error.message());
        // 负数位数：解析期拦（`-1` 是一元负号 + 字面量，不是字面量 → 求值期拦）
        let error = evaluate("ROUND({BASE}, 0 - 1)", &vars).expect_err("负数位数非法");
        assert!(error.message().contains("0~10"), "{}", error.message());
    }

    /// 函数参数个数在**解析期**校验，报文带正确签名。
    #[test]
    fn function_arity_is_checked_at_parse_time() {
        let cases = [
            ("ROUND(1, 2, 3)", "ROUND"),
            ("MIN(1)", "MIN"),
            ("MAX(1, 2, 3)", "MAX"),
            ("ABS(1, 2)", "ABS"),
            ("CEIL()", "CEIL"),
            ("IF(1 == 1, 2)", "IF"),
        ];
        for (formula, func) in cases {
            let error = parse(formula).expect_err(&format!("{formula} 参数个数应报错"));
            assert_eq!(error.kind(), ErrorKind::Arity, "{formula}");
            assert!(error.message().contains(func), "{formula}: {}", error.message());
        }
        // 参数个数对就过
        assert!(parse("ROUND(1)").is_ok());
        assert!(parse("MIN(1, 2)").is_ok());
        assert!(parse("IF(1 == 1, 2, 3)").is_ok());
    }

    // ---- 8. 变量与未定义 ----

    /// 未定义变量：点名 + 给可用清单（设计 §4.4.2 的报文口径）。
    #[test]
    fn undefined_variable_is_named_with_available_list() {
        let vars = scope(&[("BASE", "8000"), ("POST", "500")]);
        let error = evaluate("{BASEE} + 1", &vars).expect_err("拼错编码");
        assert_eq!(error.kind(), ErrorKind::UndefinedVar);
        assert!(error.message().contains("`{BASEE}`"), "点名变量：{}", error.message());
        assert!(
            error.message().contains("BASE") && error.message().contains("POST"),
            "给清单：{}",
            error.message()
        );
        assert_eq!(ErrorKind::UndefinedVar.code(), "UNDEFINED_VAR");

        // 作用域为空时也一样报（不是 panic）
        let empty = scope(&[]);
        let error = evaluate("{BASE}", &empty).expect_err("空作用域");
        assert_eq!(error.kind(), ErrorKind::UndefinedVar);
        assert!(error.message().contains("没有任何可用变量"), "{}", error.message());
    }

    /// 变量名必须大写（与 HR-31 的 `is_var_name` 同一口径）：`{base}` 在**词法**层就报。
    #[test]
    fn lowercase_variable_is_rejected_at_tokenize() {
        let error = parse("{base} + 1").expect_err("小写变量");
        assert_eq!(error.kind(), ErrorKind::Token);
        assert!(error.message().contains("不合法"), "{}", error.message());
        // 空白与多余花括号也要给位置
        assert!(parse("{ BASE } + 1").expect_err("带空格").message().contains("不合法"));
        assert!(parse("{BASE + 1").expect_err("缺右花括号").message().contains("缺少"));
        assert!(parse("BASE} + 1").expect_err("多右花括号").message().contains("多出来"));
    }

    // ---- 9. 词法 / 语法错误报文 ----

    /// 空公式 / 纯空格 → `Empty`（与 HR-31 的「不能为空」同一结论）。
    #[test]
    fn empty_formula_is_rejected() {
        for formula in ["", "   ", "\n\t"] {
            let error = parse(formula).expect_err("空公式");
            assert_eq!(error.kind(), ErrorKind::Empty);
            assert!(error.message().contains("不能为空"), "{}", error.message());
        }
    }

    /// 「写错了」的字符逐个给可操作的提示（`=` / `!` / `%` / 引号 / 中文）。
    #[test]
    fn typo_characters_get_actionable_messages() {
        let cases = [
            ("{BASE} = 1", "`==`"),
            ("{BASE} ! 1", "`!=`"),
            ("{BASE} & 1", "`&&`"),
            ("{BASE} | 1", "`||`"),
            ("{BASE} % 2", "0.13"),
            ("{BASE} + '1'", "不在允许的字符里"),
            ("{BASE} + 基本工资", "不在允许的字符里"),
            ("1.", "小数点后面缺数字"),
        ];
        for (formula, hint) in cases {
            let error = parse(formula).expect_err(formula);
            assert!(
                error.message().contains(hint),
                "{formula} 的报文应含「{hint}」，实际：{}",
                error.message()
            );
            assert_eq!(error.kind(), ErrorKind::Token, "{formula}");
        }
    }

    /// 括号不配对 / 比较连写 / 末端多余内容 / 裸标识符 都要报。
    #[test]
    fn structural_syntax_errors_are_reported() {
        let missing = parse("ROUND({BASE}, 2").expect_err("缺右括号");
        assert_eq!(missing.kind(), ErrorKind::Parse);
        assert!(missing.message().contains(")"), "{}", missing.message());

        let extra = parse("({BASE}))").expect_err("多右括号");
        assert!(extra.message().contains("多出来"), "{}", extra.message());

        let chained = parse("{A} < {B} < {C}").expect_err("比较连写");
        assert!(chained.message().contains("连写"), "{}", chained.message());
        assert!(chained.message().contains("&&"), "{}", chained.message());

        let bare = parse("BASE + 1").expect_err("裸标识符");
        assert!(bare.message().contains("函数"), "{}", bare.message());

        let garbage = parse("+ 1").expect_err("一元加号");
        assert!(garbage.message().contains("一元 `+`"), "{}", garbage.message());

        let trailing = parse("{BASE} 1").expect_err("漏运算符");
        assert!(trailing.message().contains("多余"), "{}", trailing.message());
    }

    /// 词法层带**字符**位置（不是字节）：`  {BASE} + 1` 里变量在第 3 个字符。
    #[test]
    fn tokens_carry_char_positions() {
        let tokens = tokenize("  {BASE} + 1").expect("合法");
        assert_eq!(
            tokens.iter().map(|token| (token.kind, token.at)).collect::<Vec<_>>(),
            vec![(TokenKind::Var, 3), (TokenKind::Plus, 10), (TokenKind::Number, 12),]
        );
        assert_eq!(tokens[0].text, "BASE");
        // 中文按字符算：位置不会因为 UTF-8 字节而错位（中文本身非法，报错位置在第 1 个字符）
        let error = parse("基本工资 + 1").expect_err("中文字符");
        assert!(error.message().starts_with("第 1 个字符"), "{}", error.message());
    }

    /// 嵌套深度上限：超了报错，而不是把进程栈打穿。
    #[test]
    fn nesting_depth_is_capped() {
        let deep = format!("{}1{}", "(".repeat(MAX_DEPTH + 2), ")".repeat(MAX_DEPTH + 2));
        let error = parse(&deep).expect_err("过深必须报错");
        assert_eq!(error.kind(), ErrorKind::Depth);
        assert!(error.message().contains("嵌套"), "{}", error.message());
        // 30 层还能算（一层括号只算一层 —— 曾经被算成两层，30 层直接报「超 32」）
        let ok = format!("{}1{}", "(".repeat(30), ")".repeat(30));
        assert_eq!(evaluate(&ok, &scope(&[])).expect("30 层"), Decimal::ONE);
        // 刚好 32 层也过（边界不多不少）
        let edge = format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(evaluate(&edge, &scope(&[])).expect("32 层"), Decimal::ONE);
    }

    /// **平铺长链**（无括号、无嵌套）不受 [`MAX_DEPTH`] 限制：语法树深，但嵌套层数是 1。
    ///
    /// 反例（改之前）：求值期沿用了 `MAX_DEPTH=32` 当**树深**上限 ⇒ 33 项以上的
    /// `{BASE}+{POST}+…` 汇总公式被判「嵌套过深」，而设计 §4.4.3 的应发合计本身就是长链。
    #[test]
    fn long_flat_chain_is_not_mistaken_for_deep_nesting() {
        let vars = scope(&[("BASE", "1")]);
        let hundred = vec!["1"; 100].join(" + ");
        assert_eq!(evaluate(&hundred, &scope(&[])).expect("100 项平铺"), dec("100"));
        // 变量引用交替也算同一件事
        let mixed = vec!["{BASE}"; 200].join(" + ");
        assert_eq!(evaluate(&mixed, &vars).expect("200 项变量平铺"), dec("200"));
        // 500 字符上限内最长的写法：`{BASE}` 是 6 个字符 ⇒ 70 项 = 70×6+69 = 489 字符
        let longest = vec!["{BASE}"; 70].join("+");
        assert!(longest.chars().count() <= 500, "长度：{}", longest.chars().count());
        assert_eq!(evaluate(&longest, &vars).expect("70 项紧凑写法"), dec("70"));
    }

    // ---- 10. 与设计种子公式 / HR-31 结构检查的一致性 ----

    /// 设计 §4.4.3 的 8 条公式（6 条计算项 + 2 条汇总项）全部能解析、能求值，
    /// 并且与设计 §4.4.4 的实测数字一致（GROSS=9954.03 / NET=8154.03）。
    #[test]
    fn design_seed_formulas_parse_and_match_worked_example() {
        // 种子公式逐条抄自设计 §4.4.3（HR-35 的迁移照这个种）
        let formulas = [
            "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)",
            "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)",
            "ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)",
            "IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)",
            "ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)",
            "{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}",
            "{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}",
        ];
        for formula in formulas {
            assert!(parse(formula).is_ok(), "设计种子公式必须能解析：{formula}");
        }

        // 设计 §4.4.4 的张三：逐项算完再算汇总，与文档表格逐个对拍
        let mut vars = zhangsan();
        let ot_wd = evaluate(formulas[0], &vars).expect("OT_WD");
        assert_eq!(ot_wd, dec("689.66"));
        let att_bonus = evaluate(formulas[3], &vars).expect("ATT_BONUS");
        assert_eq!(att_bonus, dec("0"), "有 2 天事假 → 全勤奖 0");
        let att_deduct = evaluate(formulas[4], &vars).expect("ATT_DEDUCT");
        assert_eq!(att_deduct, dec("735.63"), "设计实测值（不是 735.64）");

        // 汇总项依赖上一步的结果（HR-34 的拓扑求值就是这么喂变量的）
        vars.insert("OT_WD".to_string(), ot_wd);
        vars.insert("ATT_BONUS".to_string(), att_bonus);
        vars.insert("ATT_DEDUCT".to_string(), att_deduct);
        let gross = evaluate(formulas[5], &vars).expect("GROSS");
        assert_eq!(gross, dec("9954.03"), "应发合计（设计 §4.4.4）");
        vars.insert("GROSS".to_string(), gross);
        let net = evaluate(formulas[6], &vars).expect("NET");
        assert_eq!(net, dec("8154.03"), "实发合计（设计 §4.4.4）");
    }

    /// 本文件的函数白名单必须与 HR-31 结构检查的 `FUNCS` 一致 ——
    /// 两套函数集会让「结构检查放行、解析器报未知函数」的公式进库。
    #[test]
    fn function_set_matches_salary_item_formula() {
        let mut ours: Vec<&str> = Func::names().to_vec();
        let mut theirs: Vec<&str> = salary_item_formula::FUNCS.to_vec();
        assert_eq!(ours.len(), theirs.len(), "函数个数不一致");
        ours.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(ours, theirs, "函数白名单漂移");
        // 每个函数名都能被解析器认出来（含大小写宽容）
        for name in Func::names() {
            assert!(Func::from_name(name).is_some(), "{name} 认不出");
            assert!(
                Func::from_name(&name.to_ascii_lowercase()).is_some(),
                "{name} 小写认不出"
            );
        }
    }

    /// 结构检查放行的种子公式，解析器也必须放行（两层门禁不能互相打脸）。
    #[test]
    fn parser_agrees_with_structural_check_on_seeds() {
        let formulas = [
            "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)",
            "IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)",
            "{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}",
        ];
        for formula in formulas {
            assert!(
                salary_item_formula::structural_errors(formula).is_empty(),
                "结构检查应当放行：{formula}"
            );
            assert!(parse(formula).is_ok(), "解析器也必须放行：{formula}");
        }
    }

    /// 顶层的布尔结果不能当金额（`{BASE} > 1` 不是工资）。
    #[test]
    fn boolean_top_level_result_is_rejected() {
        let vars = scope(&[("BASE", "8000")]);
        let error = evaluate("{BASE} > 1", &vars).expect_err("布尔结果");
        assert_eq!(error.kind(), ErrorKind::TypeError);
        assert!(error.message().contains("布尔值"), "{}", error.message());
        // `IF` 的假分支写成比较也要报（金额项不能是 true/false）
        let error = evaluate("IF(1 == 1, 1 == 1, 0)", &vars).expect_err("分支是布尔");
        assert_eq!(error.kind(), ErrorKind::TypeError);
    }

    /// 算术里混进布尔、逻辑里混进数值，都要报类型错误（不能静默当 1 / 0）。
    #[test]
    fn type_mismatches_are_rejected() {
        let vars = scope(&[("BASE", "8000")]);
        let error = evaluate("{BASE} + (1 == 1)", &vars).expect_err("布尔参与算术");
        assert_eq!(error.kind(), ErrorKind::TypeError);
        assert!(error.message().contains("布尔值"), "{}", error.message());

        let error = evaluate("({BASE} == 8000) && {BASE}", &vars).expect_err("数值参与逻辑");
        assert_eq!(error.kind(), ErrorKind::TypeError);
        assert!(error.message().contains("比较结果"), "{}", error.message());

        let error = evaluate("ABS(1 == 1)", &vars).expect_err("布尔喂给函数");
        assert_eq!(error.kind(), ErrorKind::TypeError);
    }

    /// `HashMap` 也能当作用域（报文里的可用清单必须**排序**，否则每次报错文案都变）。
    #[test]
    fn hashmap_scope_is_supported_and_sorted() {
        let mut vars: HashMap<String, Decimal> = HashMap::new();
        for name in ["POST", "BASE", "ATT_OT_WD"] {
            vars.insert(name.to_string(), dec("1"));
        }
        assert_eq!(
            VarScope::available(&vars),
            vec!["ATT_OT_WD".to_string(), "BASE".to_string(), "POST".to_string()]
        );
        assert_eq!(
            evaluate("{BASE} + {POST} + {ATT_OT_WD}", &vars).expect("求和"),
            dec("3")
        );
    }

    /// 词法/语法错误实现 `std::error::Error`（HR-34 的 `?` 能往上抛）。
    #[test]
    fn formula_error_is_a_std_error() {
        fn as_error(formula: &str) -> Box<dyn std::error::Error> {
            Box::new(parse(formula).expect_err("必错"))
        }
        let error = as_error("{BASE");
        assert!(error.to_string().contains("缺少"), "{error}");
    }
}
