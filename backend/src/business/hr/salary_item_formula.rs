//! 薪资项公式的**结构检查 + 变量抽取 + 依赖图**（`biz_salary_item.formula` · HR-31）。
//!
//! 设计 §4.4.2 的公式 DSL 是**受限表达式**（白名单字符、白名单函数、`{VAR}` 引用）。
//! 本文件只做**不需要求值**的那一半：
//!
//! | 做什么 | 不做什么（谁做） |
//! |---|---|
//! | 长度 ≤ 500（`VARCHAR(500)` 列宽） | 数值求值 / `Decimal` 精度 / 惰性 `IF` → **HR-32** 的 `salary_formula.rs` |
//! | 字符白名单 + 禁用 SQL 片段（`;` `--` `/*`）| 递归下降语法树 → 同上 |
//! | `{VAR}` 名字合法性 + 抽取 | 变量取值（薪资标准 / 考勤）→ **HR-34** 的 `salary_compute.rs` |
//! | 括号 / 花括号配对、裸标识符必须是函数名 | — |
//! | 未定义变量（对着「已有薪资项 + `ATT_*` 白名单」判） | — |
//! | 依赖拓扑 + 环检测（`A={B}+1` / `B={A}+1`） | — |
//!
//! # 为什么「结构检查」也要独立一层，而不是等 HR-32 的解析器
//!
//! 「配置页的『检查』按钮」（HR-44）与「新建 / 改公式」（本卡 §5.2 #33 / #34）**今天就要有反馈**：
//! 公式抄错一个字符（`{BASEE}`）时用户必须立刻知道「未定义变量 `{BASEE}`，可用：BASE/POST/…」，
//! 而不是先存进库、等月底试算时才炸。这一层不依赖求值器，是**纯字符串 + 图**的活，
//! 先把「拼错编码 / 括号没配平 / 写了 SQL 片段 / 环依赖」四类错误挡住。
//!
//! > **给 HR-32 / HR-33 的接口契约**：[`extract_refs`]（引用抽取）、[`first_cycle`]（环）、
//! > [`topo_order`]（拓扑序）三个函数**请直接复用**，不要另写一套 —— 两套变量口径会让
//! > 「配置页说没环、算工资时说有环」。HR-33 接数值解析时只需在
//! > [`crate::business::hr::salary_item_dto::check_formula`] 的 `errors` 后面追加解析器结论。
//!
//! # `ATT_*` 白名单为什么在这里
//!
//! 设计 §4.4.2 的公式里出现两种变量：**项引用**（`{BASE}` = 别的薪资项编码）与
//! **考勤变量**（`{ATT_OT_WD}` = 当期考勤值）。考勤变量没有 `biz_salary_item` 行可查，
//! 只能靠白名单判「这个变量可不可达」。白名单逐列取自
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_attendance_record`
//! （列名 + 中文名一起给，前端变量面板直接展示）—— 少一列就是「客户在公式里引用不到该口径」。

use std::collections::{BTreeSet, HashMap};

// ==================== 长度 / 黑白名单 ====================

/// 公式长度上限（`biz_salary_item.formula VARCHAR(500)`，按**字符**算）。
///
/// 按字符而不是字节：`VARCHAR(500)` 在 PostgreSQL 里是 500 **字符**，
/// 中文变量说明写进公式（本不该有，但客户会写）时按字节算会误报超长。
pub const MAX_FORMULA_LEN: usize = 500;

/// 明令禁止的片段（设计 §4.4.2：「禁用 `;` `--` `/*` 与 SQL 关键字」，
/// 与 `flow::linkage::spec` 的 `ck_flow_linkage_no_sql_key` 同一口径）。
pub const FORBIDDEN_FRAGMENTS: [(&str, &str); 3] =
    [(";", "分号 `;`"), ("--", "SQL 行注释 `--`"), ("/*", "SQL 块注释 `/*`")];

/// 禁用关键字（整词匹配、忽略大小写）。
///
/// 只列「出现在公式里必然是注入尝试或抄错」的那些；不列 `AND` / `OR` 之类
/// （DSL 用的是 `&&` / `||`，但客户从 Excel 抄来的 `AND(...)` 会撞函数名，见 [`FUNCS`]）。
pub const SQL_KEYWORDS: [&str; 12] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "TRUNCATE", "UNION", "EXEC", "EXECUTE", "GRANT", "CREATE",
];

/// 允许的函数名（设计 §4.4.2 的 `FUNC` 产生式，逐个抄）。
pub const FUNCS: [&str; 7] = ["ROUND", "MIN", "MAX", "ABS", "CEIL", "FLOOR", "IF"];

/// `ATT_*` 考勤变量白名单：`(变量名, biz_attendance_record 列, 中文名)`。
///
/// 顺序 = 考勤台账的列顺序（前端变量面板照这个顺序列，用户找得到）。
pub const ATT_VARS: [(&str, &str, &str); 11] = [
    ("ATT_SHOULD_DAYS", "should_days", "应出勤天数"),
    ("ATT_ACTUAL_DAYS", "actual_days", "实际出勤天数"),
    ("ATT_LEAVE_PAID_DAYS", "leave_paid_days", "带薪假天数"),
    ("ATT_LEAVE_UNPAID_DAYS", "leave_unpaid_days", "无薪假天数（扣款依据）"),
    ("ATT_SICK_DAYS", "sick_days", "病假天数"),
    ("ATT_ABSENT_DAYS", "absent_days", "旷工天数"),
    ("ATT_OT_WD", "ot_hours_workday", "平时加班小时"),
    ("ATT_OT_WE", "ot_hours_weekend", "休息日加班小时"),
    ("ATT_OT_HO", "ot_hours_holiday", "节假日加班小时"),
    ("ATT_LATE_COUNT", "late_count", "迟到次数"),
    ("ATT_EARLY_COUNT", "early_leave_count", "早退次数"),
];

/// 全部 `ATT_*` 变量名（判「未定义变量」时与已有项编码合并使用）。
pub fn att_var_names() -> Vec<String> {
    ATT_VARS.iter().map(|(name, _, _)| (*name).to_string()).collect()
}

/// 单个 `ATT_*` 的中文名（前端变量面板 / 报错时解释「`{ATT_OT_WD}` 是什么」）。
pub fn att_var_label(name: &str) -> Option<&'static str> {
    ATT_VARS
        .iter()
        .find(|(var, _, _)| *var == name)
        .map(|(_, _, label)| *label)
}

/// 一个字符是否在公式的白名单里（设计 §4.4.2 的 EBNF 逐条对照）。
///
/// 允许：数字、字母、下划线、空格、`{}`、`()`、`,`、`.`、`+ - * /`、`< > = ! & | %`。
/// 其它（引号、`$`、`:`、`#`、中文、emoji …）一律拒 —— 宁可让用户改公式，
/// 也不许让「求值器不认识的东西」静默进库。
fn is_allowed_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || ch == '_'
        || ch == ' '
        || ch == '{'
        || ch == '}'
        || ch == '('
        || ch == ')'
        || ch == ','
        || ch == '.'
        || ch == '+'
        || ch == '-'
        || ch == '*'
        || ch == '/'
        || ch == '<'
        || ch == '>'
        || ch == '='
        || ch == '!'
        || ch == '&'
        || ch == '|'
        || ch == '%'
}

/// 是否是合法的变量名（`{VAR}` 里的 `VAR`）：`[A-Z][A-Z0-9_]*`。
///
/// 必须与薪资项编码同口径 —— 项引用的变量名**就是** `biz_salary_item.item_code`，
/// 两套字符集会让「编码合法但引用不了」的项出现。
pub fn is_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

/// 是否是「以运算符结尾」（`{BASE} +` 这种半截公式）。
fn ends_with_operator(formula: &str) -> bool {
    formula
        .trim_end()
        .chars()
        .next_back()
        .is_some_and(|ch| "+-*/,<>=!&|".contains(ch))
}

// ==================== 引用抽取 ====================

/// 抽取公式里的项 / 变量引用（`{BASE}` → `BASE`），保序去重。
///
/// # 异常情况
///
/// - `{` 没有对应的 `}`；
/// - `{}` 空引用；
/// - `}` 前面没有 `{`；
/// - 变量名不合法（[`is_var_name`]）。
///
/// 前三种是「用户少打一个字」，第四种是大小写写错（`{base}`）—— 四条都返回**能照着改**的中文。
pub fn extract_refs(formula: &str) -> std::result::Result<Vec<String>, String> {
    let bytes = formula.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                let rest = &formula[index + 1..];
                let Some(end) = rest.find('}') else {
                    return Err(format!(
                        "公式里的 `{{` 没有配对的 `}}`（从第 {} 个字符起：`{}`）：变量必须写成 {{BASE}} 这种完整形式",
                        index + 1,
                        rest.trim()
                    ));
                };
                let name = &rest[..end];
                if name.trim().is_empty() {
                    return Err("公式里有空的 `{}`：花括号里要写变量名（例：`{BASE}`）".to_string());
                }
                if name != name.trim() {
                    return Err(format!(
                        "变量名 `{{{name}}}` 两头多了空格：请写成 `{{{}}}`",
                        name.trim()
                    ));
                }
                if !is_var_name(name) {
                    return Err(format!(
                        "变量名 `{{{name}}}` 不合法：只允许大写字母开头 + 大写字母/数字/下划线（例：`{{BASE}}`、`{{ATT_OT_WD}}`）；\
                         薪资项编码要用小写请先改成大写，否则公式引用不到它"
                    ));
                }
                if !out.iter().any(|seen| seen == name) {
                    out.push(name.to_string());
                }
                index += 1 + end + 1;
            }
            b'}' => {
                return Err(format!(
                    "公式第 {} 个字符是多出来的 `}}`：变量要写成 `{{BASE}}`（先左括号后右括号）",
                    index + 1
                ));
            }
            _ => index += 1,
        }
    }
    Ok(out)
}

// ==================== 结构检查 ====================

/// 结构检查：长度 / 字符白名单 / 禁用片段 / 括号配对 / 裸标识符 / 首尾运算符。
///
/// 返回**全部**问题（不是第一个）：配置页一次把能改的都列出来，用户改一轮就过，
/// 而不是「改一个、再点检查、再改一个」。
///
/// 不检查（留给 HR-32 的解析器）：运算符结合性、函数参数个数、分支类型一致性。
pub fn structural_errors(formula: &str) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let text = formula.trim();
    if text.is_empty() {
        errors.push("公式不能为空：`calc_type = FORMULA` 的薪资项必须给公式".to_string());
        return errors;
    }
    let len = text.chars().count();
    if len > MAX_FORMULA_LEN {
        errors.push(format!(
            "公式最长 {MAX_FORMULA_LEN} 个字符（收到 {len} 个）：库列是 `VARCHAR(500)`，超长会被截断"
        ));
    }
    for (fragment, label) in FORBIDDEN_FRAGMENTS {
        if text.contains(fragment) {
            errors.push(format!("公式里不许出现{label}：公式是受限表达式，不接受任何 SQL 片段"));
        }
    }

    // 字符白名单 + 括号配对 + 裸标识符
    let mut paren_depth: i32 = 0;
    let mut brace_depth: i32 = 0;
    let mut in_ref = false;
    let mut ident = String::new();
    let mut ident_is_ref = false;
    for (position, ch) in text.char_indices() {
        if ch == '{' {
            flush_ident(&mut ident, ident_is_ref, &mut errors);
            in_ref = true;
            ident_is_ref = true;
            brace_depth += 1;
            continue;
        }
        if ch == '}' {
            if in_ref {
                ident.clear();
            }
            ident_is_ref = false;
            in_ref = false;
            brace_depth -= 1;
            continue;
        }
        if in_ref {
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            ident.push(ch);
            continue;
        }
        flush_ident(&mut ident, ident_is_ref, &mut errors);
        match ch {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth < 0 {
                    errors.push(format!(
                        "公式第 {} 个字符多了一个 `)`：括号不配对（每个 `(` 都要有 `)`）",
                        position + 1
                    ));
                    paren_depth = 0;
                }
            }
            _ => {
                if !is_allowed_char(ch) {
                    errors.push(format!(
                        "公式第 {} 个字符 `{ch}` 不在允许的字符里：只允许数字、大写字母、`{{}}`、`()`、`,`、`.`、`+ - * /`、`< > = ! & | %`",
                        position + 1
                    ));
                }
            }
        }
    }
    flush_ident(&mut ident, ident_is_ref, &mut errors);
    if paren_depth > 0 {
        errors.push("公式里 `(` 没有配对的 `)`：括号不配对".to_string());
    }
    if brace_depth != 0 || in_ref {
        errors.push("公式里的 `{` 与 `}` 数量不配对：变量要写成 `{BASE}`".to_string());
    }
    if text.contains("()") {
        errors.push("公式里有空的 `()`：函数要带参数（例：`ROUND(x, 2)`）".to_string());
    }
    if ends_with_operator(text) {
        errors.push("公式以运算符结尾（少写了右边的东西）：请补全表达式".to_string());
    }
    errors
}

/// 收一个「裸标识符」：只允许函数名，其余一律当「忘了写花括号的变量」。
fn flush_ident(ident: &mut String, is_ref: bool, errors: &mut Vec<String>) {
    if ident.is_empty() {
        return;
    }
    let token = std::mem::take(ident);
    if is_ref {
        return;
    }
    let upper = token.to_ascii_uppercase();
    if SQL_KEYWORDS.contains(&upper.as_str()) {
        errors.push(format!(
            "公式里出现了 SQL 关键字 `{token}`：公式是受限表达式，不接受任何 SQL 片段"
        ));
        return;
    }
    if FUNCS.contains(&upper.as_str()) {
        // 函数名大小写宽容（客户从 Excel 抄的 `round(...)` 也算），但**不算**漂移：
        // 求值器只认大写，所以提示一次让用户统一。
        if token != upper {
            errors.push(format!(
                "函数名请用大写 `{upper}`（收到 `{token}`）：公式里的大小写是敏感的"
            ));
        }
        return;
    }
    errors.push(format!(
        "公式里的 `{token}` 既不是函数也缺了花括号：引用薪资项/考勤要写成 `{{{token}}}`，函数只能是 {}",
        FUNCS.join(" / ")
    ));
}

// ==================== 未定义变量 ====================

/// 引用了但「不在可用集合里」的变量（未定义变量 → 报错，设计 §4.4.2）。
///
/// `allowed` 由调用方组成：**已有且启用的薪资项编码** ∪ [`att_var_names`]。
/// 单项级别的错误报文要带「可用清单」，否则用户只知道错、不知道写什么（设计 §6 的例子）。
pub fn undefined_refs(refs: &[String], allowed: &BTreeSet<String>) -> Vec<String> {
    refs.iter()
        .filter(|name| !allowed.contains(name.as_str()))
        .cloned()
        .collect()
}

/// 未定义变量的报错文案（点名变量 + 给可用清单，设计 §6「公式编辑器」的 ✅/❌ 交互）。
pub fn undefined_message(undefined: &[String], allowed: &BTreeSet<String>) -> String {
    let list = allowed.iter().cloned().collect::<Vec<_>>().join(" / ");
    format!(
        "公式引用了未定义的变量 {}：可用变量是「已有薪资项编码 + 考勤变量」—— {list}",
        undefined
            .iter()
            .map(|name| format!("`{{{name}}}`"))
            .collect::<Vec<_>>()
            .join("、")
    )
}

// ==================== 依赖图：环 + 拓扑序 ====================

/// 一条依赖边：`(薪资项编码, 它公式里引用的编码)`。
///
/// 只含**内部**依赖（引用到别的薪资项）；引用 `ATT_*` 或未定义变量时不出现在这里
/// （未定义变量由 [`undefined_refs`] 单独报）。
pub type DepNode = (String, Vec<String>);

/// 把「引用列表」收敛成图节点（去掉 `ATT_*` 与自引用以外的外部引用由调用方决定）。
///
/// 这里只做去重 + 保持输入顺序，判「哪些引用算内部」在 [`first_cycle`] / [`topo_order`]
/// 里按「节点集合是否包含该编码」判定 —— 一个函数一个口径，避免两处各写一遍。
fn edges_of<'a>(nodes: &'a [DepNode], deps: &'a [String]) -> Vec<&'a str> {
    let known: BTreeSet<&str> = nodes.iter().map(|(code, _)| code.as_str()).collect();
    let mut out: Vec<&str> = Vec::new();
    for dep in deps {
        if known.contains(dep.as_str()) && !out.contains(&dep.as_str()) {
            out.push(dep.as_str());
        }
    }
    out
}

/// 找第一个依赖环：返回环上的编码序列（首尾相同，便于报文里画成 `A → B → A`）。
///
/// 设计 §4.4.2：「环依赖 → 报错（拓扑排序时检测），报错时点名环上的项」。
/// `A={B}+1` 且 `B={A}+1` 在试算时是死循环 —— 必须在配置期挡住，而不是等月底跑不动。
///
/// 为什么**报环**而不是只报「有环」：满屏 17 个薪资项，用户看不出是哪两项互相引用。
pub fn first_cycle(nodes: &[DepNode]) -> Option<Vec<String>> {
    let index_of: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, (code, _))| (code.as_str(), index))
        .collect();
    // 0 = 未访问，1 = 在当前 DFS 栈上，2 = 已完成
    let mut color = vec![0_u8; nodes.len()];
    let mut path: Vec<usize> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();

    for start in 0..nodes.len() {
        if color[start] != 0 {
            continue;
        }
        color[start] = 1;
        path.push(start);
        stack.push((start, 0));
        while let Some((node, cursor)) = stack.pop() {
            let deps = edges_of(nodes, &nodes[node].1);
            if cursor < deps.len() {
                stack.push((node, cursor + 1));
                let next = index_of[deps[cursor]];
                match color[next] {
                    0 => {
                        color[next] = 1;
                        path.push(next);
                        stack.push((next, 0));
                    }
                    1 => {
                        let position = path.iter().position(|&seen| seen == next).unwrap_or(0);
                        let mut cycle: Vec<String> = path[position..].iter().map(|&at| nodes[at].0.clone()).collect();
                        cycle.push(nodes[next].0.clone());
                        return Some(cycle);
                    }
                    _ => {}
                }
            } else {
                color[node] = 2;
                path.pop();
            }
        }
    }
    None
}

/// 拓扑序（依赖在前）：`BASE → OT_WD → GROSS`（设计 §6「依赖顺序」）。
///
/// `None` = 有环（先调 [`first_cycle`] 拿环上的项再报错）。
/// 稳定：入度为 0 的节点按**输入顺序**取，同一份配置每次排出同一个序
/// （前端拿它当「计算顺序」展示，抖动会让人以为配置变了）。
pub fn topo_order(nodes: &[DepNode]) -> Option<Vec<String>> {
    let index_of: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, (code, _))| (code.as_str(), index))
        .collect();
    let mut indegree = vec![0_usize; nodes.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, (_, deps)) in nodes.iter().enumerate() {
        for dep in edges_of(nodes, deps) {
            let from = index_of[dep];
            indegree[index] += 1;
            dependents[from].push(index);
        }
    }
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    let mut ready: Vec<usize> = (0..nodes.len()).filter(|&index| indegree[index] == 0).collect();
    let mut cursor = 0;
    while cursor < ready.len() {
        let node = ready[cursor];
        cursor += 1;
        order.push(nodes[node].0.clone());
        for &next in &dependents[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }
    if order.len() == nodes.len() { Some(order) } else { None }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// `V0383` 的建表原文（编译期嵌入）：`VARCHAR(500)` 与考勤列名在这里对拍。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    /// 设计 §4.4.3 的 17 个种子公式（一条一条抄自设计文档）：全部必须过结构检查。
    ///
    /// 这条用例的价值：本卡的结构检查要是把「设计自己的公式」判成错的，
    /// HR-35 的种子迁移与客户照抄都会撞墙。
    const SEED_FORMULAS: [&str; 6] = [
        "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)",
        "ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)",
        "ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)",
        "IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)",
        "ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)",
        "{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}",
    ];

    fn allowed(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|code| (*code).to_string()).collect()
    }

    // ---- 验收项：长度校验 ----

    /// 公式长度按**字符**算，500 通过 / 501 拒（`VARCHAR(500)` 列宽）。
    #[test]
    fn formula_length_limit_is_500_chars() {
        // 用「合法表达式」而不是 `"A".repeat(500)` 去测边界：裸标识符本身就该报错（见
        // `bare_identifiers_must_be_functions`），混在一起测会让「长度超限」这条断言假红。
        let ok = format!("{}11", "1+".repeat((MAX_FORMULA_LEN - 2) / 2));
        assert_eq!(ok.chars().count(), MAX_FORMULA_LEN);
        assert!(
            structural_errors(&ok).is_empty(),
            "500 字符的合法公式必须通过：{:?}",
            structural_errors(&ok)
        );
        let too_long = format!("{}111", "1+".repeat((MAX_FORMULA_LEN - 2) / 2));
        assert_eq!(too_long.chars().count(), MAX_FORMULA_LEN + 1);
        let errors = structural_errors(&too_long);
        assert!(
            errors.iter().any(|message| message.contains("最长 500")),
            "501 字符必须报超长：{errors:?}"
        );
    }

    /// 中文按字符算（`{` 里不可能有中文，但 `remark` 同类问题在 dto 层会复用这个口径）。
    #[test]
    fn length_counts_chars_not_bytes() {
        let formula = format!("{}+{{BASE}}", "1".repeat(MAX_FORMULA_LEN - 8));
        assert_eq!(formula.chars().count(), MAX_FORMULA_LEN - 1);
        assert!(structural_errors(&formula).is_empty(), "刚好 500 字符：{formula}");
    }

    /// 列宽必须与 `V0383` 一致（改了迁移不跟着改这里 → 501 字符在库层被截断）。
    #[test]
    fn column_width_matches_migration() {
        assert!(
            V0383_DDL.contains(&format!("formula               VARCHAR({MAX_FORMULA_LEN})")),
            "biz_salary_item.formula 的列宽变了？（V0383 里应出现 VARCHAR({MAX_FORMULA_LEN})）"
        );
    }

    // ---- 验收项：结构检查 ----

    /// 设计自己的 6 条公式全部通过（含 `ROUND` 两位小数、`IF` 惰性分支、纯加减）。
    #[test]
    fn seed_formulas_pass_structural_check() {
        for formula in SEED_FORMULAS {
            let errors = structural_errors(formula);
            assert!(errors.is_empty(), "设计种子公式被判错：{formula} → {errors:?}");
            assert!(extract_refs(formula).is_ok(), "引用抽取必须成功：{formula}");
        }
    }

    /// 空公式 / 纯空格：报「不能为空」（`calc_type=FORMULA` 时必填，设计 §3.1 ⑦ 的 CHECK）。
    #[test]
    fn empty_formula_is_rejected() {
        assert!(structural_errors("").iter().any(|m| m.contains("不能为空")));
        assert!(structural_errors("   ").iter().any(|m| m.contains("不能为空")));
    }

    /// SQL 片段一律拒（`;` / `--` / `/*` / 关键字）。
    #[test]
    fn sql_fragments_are_rejected() {
        for formula in ["{BASE}; DROP TABLE biz_salary_item", "{BASE} -- x", "{BASE} /* x */"] {
            let errors = structural_errors(formula);
            assert!(!errors.is_empty(), "必须拒：{formula}");
        }
        // `/` 是合法运算符（除法），但 `/*` 不是
        assert!(structural_errors("{BASE} / 21.75").is_empty(), "除法是合法运算");
        let errors = structural_errors("{BASE} UNION {POST}");
        assert!(errors.iter().any(|m| m.contains("SQL 关键字")), "{errors:?}");
    }

    /// 括号不配对：多 `(` / 多 `)` 都要报，且报文带位置。
    #[test]
    fn unbalanced_parens_are_rejected() {
        let missing = structural_errors("ROUND({BASE}, 2");
        assert!(missing.iter().any(|m| m.contains("没有配对的 `)`")), "{missing:?}");
        let extra = structural_errors("ROUND({BASE}, 2))");
        assert!(extra.iter().any(|m| m.contains("多了一个 `)`")), "{extra:?}");
    }

    /// 裸标识符：函数名放行，非函数名判「缺花括号」。
    #[test]
    fn bare_identifiers_must_be_functions() {
        assert!(structural_errors("ROUND({BASE} / 21.75, 2)").is_empty());
        let errors = structural_errors("BASE + {POST}");
        assert!(
            errors.iter().any(|m| m.contains("`BASE`") && m.contains("花括号")),
            "裸 BASE 必须被判成「忘了写花括号」：{errors:?}"
        );
        // 小写函数名：宽容报错（提示改成大写），仍然算错误 —— 求值器只认大写
        let lowered = structural_errors("round({BASE}, 2)");
        assert!(lowered.iter().any(|m| m.contains("函数名请用大写")), "{lowered:?}");
    }

    /// 字符白名单：引号 / 中文 / `$` 一律拒（防止把 SQL 片段或说明文字写进公式）。
    #[test]
    fn illegal_characters_are_rejected() {
        for formula in ["{BASE} + '1'", "{BASE} + 基本工资", "{BASE} + $1"] {
            let errors = structural_errors(formula);
            assert!(
                errors.iter().any(|m| m.contains("不在允许的字符里")),
                "必须拒非法字符：{formula} → {errors:?}"
            );
        }
    }

    /// 半截公式：以运算符结尾要报（`{BASE} +` 是「还没写完」）。
    #[test]
    fn trailing_operator_is_rejected() {
        assert!(structural_errors("{BASE} +").iter().any(|m| m.contains("运算符结尾")));
        assert!(structural_errors("{BASE} * {POST}").is_empty());
    }

    // ---- 引用抽取 ----

    /// 抽取 + 去重 + 保序。
    #[test]
    fn extract_refs_dedupes_and_keeps_order() {
        let refs = extract_refs("{BASE} + {POST} + {BASE}").expect("合法公式");
        assert_eq!(refs, vec!["BASE".to_string(), "POST".to_string()]);
        assert!(
            extract_refs("{ATT_OT_WD} * 1.5")
                .expect("考勤变量")
                .contains(&"ATT_OT_WD".to_string())
        );
    }

    /// 坏引用三条：未闭合 / 空引用 / 多余 `}` / 名字不合法。
    #[test]
    fn broken_refs_are_reported_with_reason() {
        assert!(extract_refs("{BASE").expect_err("未闭合").contains("没有配对"));
        assert!(extract_refs("{} + 1").expect_err("空引用").contains("空的"));
        assert!(extract_refs("BASE}").expect_err("多余右括号").contains("多出来"));
        let lowered = extract_refs("{base}").expect_err("小写");
        assert!(lowered.contains("不合法"), "{lowered}");
        assert!(extract_refs("{ ATT_OT_WD }").expect_err("带空格").contains("空格"));
        assert!(is_var_name("ATT_OT_WD"));
        assert!(!is_var_name("1BASE"));
        assert!(!is_var_name(""));
        assert!(!is_var_name("BASE-1"));
    }

    /// `ATT_*` 白名单的每一列都必须真在 `V0383` 的 `biz_attendance_record` 里。
    ///
    /// 白名单是「客户能在公式里引用什么考勤口径」的唯一来源：多一列 = 公式能引用到
    /// 不存在的列，试算时才炸；少一列 = 某个合法口径配不出来。
    #[test]
    fn att_whitelist_matches_attendance_record_columns() {
        for (var, column, label) in ATT_VARS {
            assert!(is_var_name(var), "变量名不合法：{var}");
            assert!(var.starts_with("ATT_"), "考勤变量统一 ATT_ 前缀：{var}");
            assert!(!label.is_empty(), "{var} 缺中文名");
            // 列名在 DDL 里作为整词出现（`should_days` 之类）
            assert!(
                V0383_DDL.contains(&format!("{column} ")) || V0383_DDL.contains(&format!("{column},")),
                "考勤列 {column}（{var}）不在 V0383 的 biz_attendance_record 里"
            );
        }
        assert_eq!(att_var_names().len(), ATT_VARS.len());
        assert_eq!(att_var_label("ATT_OT_WD"), Some("平时加班小时"));
        assert_eq!(att_var_label("NOPE"), None);
    }

    // ---- 未定义变量 ----

    /// 未定义变量点名 + 给可用清单（设计 §6 的报文口径）。
    #[test]
    fn undefined_refs_are_named_with_available_list() {
        let refs = extract_refs("{BASE} + {BASEE}").expect("结构合法");
        let mut allowed = allowed(&["BASE", "POST"]);
        allowed.extend(att_var_names());
        let undefined = undefined_refs(&refs, &allowed);
        assert_eq!(undefined, vec!["BASEE".to_string()]);
        let message = undefined_message(&undefined, &allowed);
        assert!(message.contains("`{BASEE}`"), "{message}");
        assert!(message.contains("BASE") && message.contains("ATT_OT_WD"), "{message}");

        // 考勤变量在可用集合里 → 不算未定义
        let att = extract_refs("{ATT_LEAVE_UNPAID_DAYS}").expect("结构合法");
        assert!(undefined_refs(&att, &allowed).is_empty());
    }

    // ---- 环 / 拓扑 ----

    /// 自引用 + 两项互引都是环，报文能画出 `A → B → A`。
    #[test]
    fn cycles_are_detected_and_named() {
        let self_ref = vec![("BASE".to_string(), vec!["BASE".to_string()])];
        let cycle = first_cycle(&self_ref).expect("自引用是环");
        assert_eq!(cycle, vec!["BASE".to_string(), "BASE".to_string()]);

        let mutual = vec![
            ("A".to_string(), vec!["B".to_string()]),
            ("B".to_string(), vec!["A".to_string()]),
        ];
        let cycle = first_cycle(&mutual).expect("互引是环");
        assert_eq!(cycle.first(), cycle.last(), "首尾相同：{cycle:?}");
        assert_eq!(cycle.len(), 3, "A → B → A：{cycle:?}");
        assert!(topo_order(&mutual).is_none(), "有环排不出拓扑序");
    }

    /// 无环：设计种子链路 `BASE → OT_WD → GROSS → NET` 的拓扑序依赖在前。
    #[test]
    fn topo_order_puts_dependencies_first() {
        let nodes = vec![
            ("NET".to_string(), vec!["GROSS".to_string(), "IIT".to_string()]),
            ("GROSS".to_string(), vec!["BASE".to_string(), "OT_WD".to_string()]),
            ("OT_WD".to_string(), vec!["BASE".to_string(), "ATT_OT_WD".to_string()]),
            ("IIT".to_string(), vec![]),
            ("BASE".to_string(), vec![]),
        ];
        assert!(first_cycle(&nodes).is_none());
        let order = topo_order(&nodes).expect("无环");
        let at = |code: &str| order.iter().position(|item| item == code).unwrap_or(usize::MAX);
        assert!(at("BASE") < at("OT_WD"), "BASE 要先算：{order:?}");
        assert!(at("OT_WD") < at("GROSS"), "OT_WD 要先算：{order:?}");
        assert!(at("GROSS") < at("NET"), "GROSS 要先算：{order:?}");
        assert_eq!(order.len(), nodes.len());
        // 稳定：同一份输入排两次结果一致
        assert_eq!(topo_order(&nodes), Some(order));
    }

    /// 外部引用（`ATT_*` / 不存在的编码）不进图：不影响拓扑，也不制造假环。
    #[test]
    fn external_refs_do_not_enter_the_graph() {
        let nodes = vec![
            ("OT_WD".to_string(), vec!["ATT_OT_WD".to_string(), "NOPE".to_string()]),
            ("GROSS".to_string(), vec!["OT_WD".to_string()]),
        ];
        assert!(first_cycle(&nodes).is_none(), "考勤变量不是环");
        assert_eq!(topo_order(&nodes).expect("无环").len(), 2);
    }
}
