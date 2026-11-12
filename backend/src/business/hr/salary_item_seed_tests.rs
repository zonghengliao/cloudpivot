//! 薪资项种子（`V0396__hr_salary_items_seed`）的验收用例（HR-35，设计 §4.4.3 / §4.4.4）。
//!
//! 这张卡的三个验收项在这里落地：
//!
//! | 验收项 | 在哪验 |
//! |---|---|
//! | **17 项齐全** | `mod tests` 直接解析**迁移文件本身**（`include_str!`）：17 行逐列对拍设计表；`mod db` 再查真库 |
//! | **公式能解析** | `mod tests` 用 HR-32 的 `salary_formula`（tokenizer + 递归下降 + `Decimal` 求值）逐条解析，并按 §4.4.4 的试算数字算一遍；`mod db` 对**库里存着的**公式再算一遍 |
//! | **科目指向正确** | `mod db` 关联 `biz_account`：10 个科目编码真实存在 / 末级 / 启用，且类别与余额方向符合设计 |
//!
//! # 为什么盯着迁移文件验，而不是在 Rust 里再抄一份 17 项的常量
//!
//! 抄一份 = 两个事实源：迁移里改了公式、Rust 常量没改，用例照样绿。
//! 这里 `include_str!` 把 up/down 两个 SQL 拉进编译单元（仓内既有做法：
//! `erp/cost_settle_finish.rs`、`flow/linkage/tests_presets.rs`），
//! 所以断言的是**真正会被执行的那份 SQL**。
//!
//! # 真库用例的开关（不设库地址就 SKIP，不假绿）
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr35_drill \
//!   cargo test --lib business::hr::salary_item_seed -- --test-threads=1 --nocapture
//! ```
//!
//! 库要建在 **145 测试机**（99 是 Hermes 主机，不允许建演练库）。
//! 空库首启即自动应用迁移 ⇒ 用例跑的就是 V0396 刚种下去的那 17 行。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::hr::salary_formula;
use crate::business::hr::salary_item_dto::SalaryItemSnapshot;
use crate::business::hr::salary_item_formula::{self, DepNode};

/// 迁移 up（`include_str!`：SQL 改动会触发重编译，断言不会和文件走散）。
const UP: &str = include_str!("../../../migrations/business/V0396__hr_salary_items_seed.up.sql");
/// 迁移 down（回滚要覆盖同样的 17 个编码，否则滚一半留一半）。
const DOWN: &str = include_str!("../../../migrations/business/V0396__hr_salary_items_seed.down.sql");

/// 种子行的期望编码顺序（= 设计 §4.4.3 表自上而下 = `sort_order` 10/20/…/170）。
const EXPECTED_CODES: [&str; 17] = [
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
];

/// 种子里出现的 10 个科目编码（9 个工资专用由 V0384 种 + 1221 由 V0183 种）。
const EXPECTED_ACCOUNTS: [&str; 10] = [
    "2211.01", "2211.02", "2211.03", "2221.03", "2241.03", "2241.04", "5602.10", "5602.11", "5602.12", "1221",
];

/// 组织哨兵：**必须**就是 handler 写入用的那个常量。
///
/// 种子若落在别的 org，`uk_biz_sal_item_code (org_id, item_code)` 判不到这 17 行 ⇒
/// 用户在「薪资项」页能再建一个 `BASE`，`{BASE}` 取哪一行由 `sort_order` 决定 —— 工资表静默算错。
const SENTINEL_ORG: Uuid = SENTINEL_ORG_ID;

/// 迁移里的一行种子（列顺序与 `INSERT` 的 `VALUES` 一致）。
#[derive(Debug, Clone, PartialEq)]
struct SeedRow {
    item_code: String,
    item_name: String,
    item_type: String,
    calc_type: String,
    source: String,
    formula: Option<String>,
    sort_order: i32,
    debit: Option<String>,
    credit: Option<String>,
    is_taxable: i32,
    is_system: i32,
}

impl SeedRow {
    /// 公式引用（`{VAR}` 抽取；含 `ATT_*` 考勤变量）。
    fn refs(&self) -> Vec<String> {
        let Some(formula) = self.formula.as_deref() else {
            return Vec::new();
        };
        salary_item_formula::extract_refs(formula).expect("种子公式的 `{}` 必须配对")
    }
}

/// 切一个 VALUES 元组的字段：按顶层 `,` 切，跳过引号内与括号内的那些。
///
/// 必须做这层处理：薪资项名称里有括号（`社保(个人)`）、公式里有逗号和括号
/// （`ROUND({BASE} / 21.75, 2)`）—— 裸 `split(',')` 会把一行切成十几段。
fn split_fields(inner: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                // SQL 里 `''` 是转义后的单引号
                if in_quote && chars.peek() == Some(&'\'') {
                    cur.push('\'');
                    chars.next();
                } else {
                    in_quote = !in_quote;
                }
            }
            '(' if !in_quote => {
                depth += 1;
                cur.push(ch);
            }
            ')' if !in_quote => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if !in_quote && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// `NULL` → `None`，其余按文本（引号已由 [`split_fields`] 剥掉）。
fn opt(field: &str) -> Option<String> {
    if field.eq_ignore_ascii_case("NULL") {
        None
    } else {
        Some(field.to_string())
    }
}

/// 从 up.sql 里把 17 行种子解析出来。
///
/// 定位要**从 `INSERT INTO biz_salary_item` 之后**再找 `FROM (VALUES`：
/// 前置护栏里也有 `FROM (VALUES ('2211.01'), …)`（科目清单），从文件头搜会撞上它。
fn seed_rows() -> Vec<SeedRow> {
    let head = "FROM (VALUES";
    let tail = ") AS v(item_code";
    let anchor = UP
        .find("INSERT INTO biz_salary_item")
        .expect("V0396 up.sql 丢了 `INSERT INTO biz_salary_item`");
    let start = anchor + UP[anchor..].find(head).expect("V0396 up.sql 丢了 `FROM (VALUES`") + head.len();
    let end = start + UP[start..].find(tail).expect("V0396 up.sql 丢了 `) AS v(item_code`");
    let region = &UP[start..end];

    let mut rows: Vec<SeedRow> = Vec::new();
    for line in region.lines() {
        let line = line.trim();
        if !line.starts_with('(') {
            continue; // 空行 / 注释行
        }
        let line = line.trim_end_matches(',').trim();
        let inner = line
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or_else(|| panic!("种子行不是完整的元组：{line}"));
        let fields = split_fields(inner);
        assert_eq!(
            fields.len(),
            11,
            "种子行应有 11 个字段（编码/名称/类型/算法/来源/公式/顺序/借/贷/计税/系统）：{line}"
        );
        rows.push(SeedRow {
            item_code: fields[0].clone(),
            item_name: fields[1].clone(),
            item_type: fields[2].clone(),
            calc_type: fields[3].clone(),
            source: fields[4].clone(),
            formula: opt(&fields[5]),
            sort_order: fields[6]
                .parse()
                .unwrap_or_else(|_| panic!("sort_order 不是整数：{}", fields[6])),
            debit: opt(&fields[7]),
            credit: opt(&fields[8]),
            is_taxable: fields[9]
                .parse()
                .unwrap_or_else(|_| panic!("is_taxable 不是整数：{}", fields[9])),
            is_system: fields[10]
                .parse()
                .unwrap_or_else(|_| panic!("is_system 不是整数：{}", fields[10])),
        });
    }
    rows
}

/// 把种子行投影成「库里已有项」的快照（供 `check_formula` 判未定义变量 / 环）。
fn snapshots_of(rows: &[SeedRow]) -> Vec<SalaryItemSnapshot> {
    rows.iter()
        .map(|row| SalaryItemSnapshot {
            item_code: row.item_code.clone(),
            formula: row.formula.clone(),
            is_active: 1,
        })
        .collect()
}

/// 依赖图节点（引用收敛成「只留本集合内的项」—— `ATT_*` 不是节点，由 `edges_of` 过滤）。
fn dep_nodes(rows: &[SeedRow]) -> Vec<DepNode> {
    rows.iter().map(|row| (row.item_code.clone(), row.refs())).collect()
}

/// 按拓扑序把 17 项的公式**真算一遍**：`init` 给固定项 / 手工项 / 考勤变量的值，
/// 公式项（含 GROSS、NET）边算边填回变量表。
fn compute(rows: &[SeedRow], init: &[(&str, &str)]) -> Vec<(String, Decimal)> {
    let mut vars: std::collections::BTreeMap<String, Decimal> = std::collections::BTreeMap::new();
    for (name, text) in init {
        vars.insert((*name).to_string(), decimal(text));
    }
    let nodes = dep_nodes(rows);
    let order = salary_item_formula::topo_order(&nodes).expect("种子里不许有环依赖");
    for code in order {
        let row = rows
            .iter()
            .find(|row| row.item_code == code)
            .expect("拓扑序里出现了不存在的编码");
        if row.calc_type != "FORMULA" {
            continue; // FIXED / INPUT：值来自 init
        }
        let formula = row.formula.as_deref().expect("FORMULA 项必须有公式");
        let value = salary_formula::evaluate(formula, &vars)
            .unwrap_or_else(|err| panic!("薪资项 {code} 的公式算不出来：{err}"));
        vars.insert(row.item_code.clone(), value);
    }
    vars.into_iter().collect()
}

/// 测试里的「期望小数」：直接按字面量解析，避免浮点。
fn decimal(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap_or_else(|err| panic!("{text} 不是合法小数：{err}"))
}

// ============================================================================
// 纯单测：解析迁移文件本身（不连库）
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::salary_item_dto;

    /// ① 验收：**17 项齐全**——顺序 / 编码 / 名称 / 类型 / 算法 / 顺序值逐列对拍设计 §4.4.3。
    #[test]
    fn seventeen_items_match_design_4_4_3() {
        let rows = seed_rows();
        assert_eq!(rows.len(), 17, "设计 §4.4.3 是一期 17 项（15 计算项 + 2 汇总项）");
        assert_eq!(
            rows.iter().map(|row| row.item_code.as_str()).collect::<Vec<_>>(),
            EXPECTED_CODES.to_vec(),
            "编码或顺序与设计表不一致"
        );

        // sort_order = 10/20/…/170（10 递增，与设计表「#」列一一对应）
        for (index, row) in rows.iter().enumerate() {
            let want = (index as i32 + 1) * 10;
            assert_eq!(row.sort_order, want, "{} 的 sort_order 应为 {want}", row.item_code);
        }

        // 名称逐字对拍（工资条表头就是它，错一个字客户就会来问）
        for (code, name) in [
            ("BASE", "基本工资"),
            ("POST", "岗位津贴"),
            ("MEAL", "餐补"),
            ("PERF", "绩效奖金"),
            ("OT_WD", "平时加班费"),
            ("OT_WE", "休息日加班费"),
            ("OT_HO", "节假日加班费"),
            ("ATT_BONUS", "全勤奖"),
            ("ATT_DEDUCT", "考勤扣款"),
            ("SI_PERSONAL", "社保(个人)"),
            ("HF_PERSONAL", "公积金(个人)"),
            ("IIT", "个人所得税"),
            ("OTHER_WITHHOLD", "其他代扣"),
            ("SI_COMPANY", "单位社保"),
            ("HF_COMPANY", "单位公积金"),
            ("GROSS", "应发合计"),
            ("NET", "实发合计"),
        ] {
            let row = rows.iter().find(|row| row.item_code == code).expect("编码必须存在");
            assert_eq!(row.item_name, name, "{code} 的名称与设计不一致");
        }

        // 五类项的数量与设计 §4.4.3 末段一致：8 收入 + 1 应发前扣减 + 4 代扣 + 2 单位承担 + 2 汇总
        let count_of = |item_type: &str| rows.iter().filter(|row| row.item_type == item_type).count();
        assert_eq!(count_of("EARNING"), 8, "EARNING 应为 8 项");
        assert_eq!(count_of("PRE_DEDUCT"), 1, "PRE_DEDUCT 应为 1 项（考勤扣款）");
        assert_eq!(count_of("WITHHOLD"), 4, "WITHHOLD 应为 4 项（社保/公积金/个税/其他）");
        assert_eq!(count_of("EMPLOYER"), 2, "EMPLOYER 应为 2 项（单位社保/单位公积金）");
        assert_eq!(count_of("SUMMARY"), 2, "SUMMARY 应为 2 项（GROSS/NET）");

        // 系统项只有 GROSS / NET（`salary_item_handler` 的「不许删 / 不许停用」靠它拦）
        for row in &rows {
            let want = i32::from(row.item_type == "SUMMARY");
            assert_eq!(
                row.is_system, want,
                "{} 的 is_system 不对（只有汇总项是系统项）",
                row.item_code
            );
        }

        // 计税口径：EARNING 计税，其余不计（PRE_DEDUCT 的符号已在应发恒等式里，标 1 会反向）
        for row in &rows {
            let want = i32::from(row.item_type == "EARNING");
            assert_eq!(row.is_taxable, want, "{} 的 is_taxable 不对", row.item_code);
        }

        // calc_type 与设计表的「算法」列一致
        for (code, want) in [
            ("BASE", "FIXED"),
            ("POST", "FIXED"),
            ("MEAL", "FIXED"),
            ("PERF", "INPUT"),
            ("OT_WD", "FORMULA"),
            ("OT_WE", "FORMULA"),
            ("OT_HO", "FORMULA"),
            ("ATT_BONUS", "FORMULA"),
            ("ATT_DEDUCT", "FORMULA"),
            ("SI_PERSONAL", "INPUT"),
            ("HF_PERSONAL", "INPUT"),
            ("IIT", "INPUT"),
            ("OTHER_WITHHOLD", "INPUT"),
            ("SI_COMPANY", "INPUT"),
            ("HF_COMPANY", "INPUT"),
            ("GROSS", "FORMULA"),
            ("NET", "FORMULA"),
        ] {
            let row = rows.iter().find(|row| row.item_code == code).expect("编码必须存在");
            assert_eq!(row.calc_type, want, "{code} 的 calc_type 应为 {want}");
        }

        // source 与 calc_type 自洽：FIXED ⇒ 取薪资标准；汇总 ⇒ SYSTEM；公式类 ⇒ 考勤；手工 ⇒ INPUT
        for row in &rows {
            let want = match (row.calc_type.as_str(), row.item_type.as_str()) {
                ("FIXED", _) => "EMPLOYEE_STANDARD",
                ("FORMULA", "SUMMARY") => "SYSTEM",
                ("FORMULA", _) => "ATTENDANCE",
                ("INPUT", _) => "INPUT",
                other => panic!("{} 的 calc_type 意外：{other:?}", row.item_code),
            };
            assert_eq!(row.source, want, "{} 的 source 应为 {want}", row.item_code);
        }
    }

    /// ② 验收：**科目指向正确**（结构层：哪些项必须有科目、哪些只贷不借、哪些不要科目）。
    #[test]
    fn accounts_follow_item_type() {
        let rows = seed_rows();
        for row in &rows {
            match row.item_type.as_str() {
                "EARNING" | "PRE_DEDUCT" | "EMPLOYER" => {
                    let debit = row.debit.as_deref().expect("收入/扣减/单位承担项必须有借方科目");
                    assert!(
                        debit.starts_with("5602."),
                        "{} 的借方应是工资费用（5602.x），实际 {debit}",
                        row.item_code
                    );
                    assert!(row.credit.is_some(), "{} 必须给贷方科目", row.item_code);
                }
                "WITHHOLD" => {
                    assert!(row.debit.is_none(), "{} 是代扣项，不该有借方科目", row.item_code);
                    assert!(row.credit.is_some(), "{} 必须给贷方科目", row.item_code);
                }
                "SUMMARY" => {
                    assert!(
                        row.debit.is_none() && row.credit.is_none(),
                        "{} 是汇总项，不要科目",
                        row.item_code
                    );
                    assert!(
                        row.formula.is_some(),
                        "{} 是汇总项，必须有公式（库层 ck_biz_sal_item_account 也这么要求）",
                        row.item_code
                    );
                }
                other => panic!("{} 的 item_type 意外：{other}", row.item_code),
            }
        }

        // 逐条对拍设计 §4.4.3 的「借 / 贷」两列
        for (code, debit, credit) in [
            ("BASE", Some("5602.10"), Some("2211.01")),
            ("POST", Some("5602.10"), Some("2211.01")),
            ("MEAL", Some("5602.10"), Some("2211.01")),
            ("PERF", Some("5602.10"), Some("2211.01")),
            ("OT_WD", Some("5602.10"), Some("2211.01")),
            ("OT_WE", Some("5602.10"), Some("2211.01")),
            ("OT_HO", Some("5602.10"), Some("2211.01")),
            ("ATT_BONUS", Some("5602.10"), Some("2211.01")),
            ("ATT_DEDUCT", Some("5602.10"), Some("2211.01")),
            ("SI_PERSONAL", None, Some("2241.03")),
            ("HF_PERSONAL", None, Some("2241.04")),
            ("IIT", None, Some("2221.03")),
            ("OTHER_WITHHOLD", None, Some("1221")),
            ("SI_COMPANY", Some("5602.11"), Some("2211.02")),
            ("HF_COMPANY", Some("5602.12"), Some("2211.03")),
            ("GROSS", None, None),
            ("NET", None, None),
        ] {
            let row = rows.iter().find(|row| row.item_code == code).expect("编码必须存在");
            assert_eq!(row.debit.as_deref(), debit, "{code} 的借方科目与设计不一致");
            assert_eq!(row.credit.as_deref(), credit, "{code} 的贷方科目与设计不一致");
        }

        // 引用的科目编码必须都在 EXPECTED_ACCOUNTS 里（新增科目 ⇒ 必须同步扩 V0384 的断言）
        for row in &rows {
            for code in [row.debit.as_deref(), row.credit.as_deref()].into_iter().flatten() {
                assert!(
                    EXPECTED_ACCOUNTS.contains(&code),
                    "{} 引用了设计外的科目 {code}（V0384 没种它，过账会 400）",
                    row.item_code
                );
            }
        }
    }

    /// ③ 验收：**公式能解析**——7 条公式（5 个计算项 + 2 个汇总项）过 HR-32 的解析器（词法 + 递归下降），
    /// 且过 HR-31 的结构/语义门禁（长度 / 白名单字符 / 函数名 / 引用可解析 / 无环）。
    #[test]
    fn every_formula_parses_and_passes_the_gate() {
        let rows = seed_rows();
        let snapshots = snapshots_of(&rows);
        let mut formula_n = 0;

        for row in &rows {
            let Some(formula) = row.formula.as_deref() else {
                assert_ne!(row.calc_type, "FORMULA", "{} 是 FORMULA 项但没有公式", row.item_code);
                continue;
            };
            formula_n += 1;

            // 结构门禁（配置页「检查」按钮走的同一套）
            let structural = salary_item_formula::structural_errors(formula);
            assert!(
                structural.is_empty(),
                "{} 的公式没过结构门禁：{structural:?}",
                row.item_code
            );

            // 语义门禁：未定义变量 / 环（对着全部 17 项 + `ATT_*` 白名单判）
            let check = salary_item_dto::check_formula(formula, Some(&row.item_code), &snapshots);
            assert!(check.ok(), "{} 的公式没过语义门禁：{:?}", row.item_code, check.errors);
            assert!(
                check.undefined.is_empty(),
                "{} 引用了未定义变量：{:?}",
                row.item_code,
                check.undefined
            );
            assert!(
                check.cycle.is_none(),
                "{} 的公式引入了环：{:?}",
                row.item_code,
                check.cycle
            );

            // 真解析（HR-32 的 tokenizer + 递归下降）：语法错在这里就该炸
            let parsed = salary_formula::parse(formula);
            assert!(parsed.is_ok(), "{} 的公式解析失败：{:?}", row.item_code, parsed.err());
        }
        assert_eq!(formula_n, 7, "公式项应为 7 条（5 计算 + 2 汇总）");
    }

    /// ④ 依赖图：无环、拓扑序覆盖全部 17 项，且被引用者一定排在引用者前面。
    #[test]
    fn dependency_graph_is_acyclic_and_orders_sensibly() {
        let rows = seed_rows();
        let nodes = dep_nodes(&rows);
        assert!(salary_item_formula::first_cycle(&nodes).is_none(), "种子公式里不许有环");

        let order = salary_item_formula::topo_order(&nodes).expect("拓扑排序应成功");
        assert_eq!(order.len(), 17, "拓扑序应覆盖全部 17 项");

        let position = |code: &str| order.iter().position(|item| item == code).expect("拓扑序缺项");
        for (dep, user) in [
            ("BASE", "OT_WD"),
            ("BASE", "ATT_DEDUCT"),
            ("BASE", "GROSS"),
            ("ATT_BONUS", "GROSS"),
            ("ATT_DEDUCT", "GROSS"),
            ("GROSS", "NET"),
            ("IIT", "NET"),
        ] {
            assert!(position(dep) < position(user), "{dep} 应排在 {user} 之前计算");
        }
    }

    /// ⑤ 「公式能解析」的最强形态：拿设计 §4.4.4 的**真实数字**跑一遍。
    ///
    /// 设计写明「中间结果不许提前四舍五入」：`ROUND(367.82 × 2, 2) = 735.64`，而
    /// `ROUND(8000/21.75×2, 2) = 735.63` —— 差 1 分钱客户就会打电话来，断言取后者。
    #[test]
    fn trial_numbers_match_design_4_4_4() {
        let rows = seed_rows();
        let values = compute(
            &rows,
            &[
                ("BASE", "8000"),
                ("POST", "500"),
                ("MEAL", "300"),
                ("PERF", "1200"),
                ("SI_PERSONAL", "840"),
                ("HF_PERSONAL", "960"),
                ("IIT", "0"),
                ("OTHER_WITHHOLD", "0"),
                ("SI_COMPANY", "1680"),
                ("HF_COMPANY", "960"),
                ("ATT_SHOULD_DAYS", "22"),
                ("ATT_ACTUAL_DAYS", "20"),
                ("ATT_LEAVE_UNPAID_DAYS", "2"),
                ("ATT_ABSENT_DAYS", "0"),
                ("ATT_OT_WD", "10"),
                ("ATT_OT_WE", "0"),
                ("ATT_OT_HO", "0"),
            ],
        );
        let get = |code: &str| -> Decimal {
            values
                .iter()
                .find(|(name, _)| name == code)
                .unwrap_or_else(|| panic!("{code} 没算出来"))
                .1
        };

        assert_eq!(get("OT_WD"), decimal("689.66"), "平时加班费（10h × 1.5 倍）");
        assert_eq!(get("OT_WE"), decimal("0"), "休息日加班（0h）");
        assert_eq!(get("OT_HO"), decimal("0"), "节假日加班（0h）");
        assert_eq!(get("ATT_BONUS"), decimal("0"), "有 2 天事假 ⇒ 全勤奖 0");
        assert_eq!(get("ATT_DEDUCT"), decimal("735.63"), "考勤扣款（不许提前四舍五入）");
        assert_eq!(get("GROSS"), decimal("9954.03"), "应发 = ΣEARNING − PRE_DEDUCT");
        assert_eq!(get("NET"), decimal("8154.03"), "实发 = 应发 − 代扣");
        // 计提总额 = 应发 + 单位社保 + 单位公积金（§4.4.4 末行）
        assert_eq!(
            get("GROSS") + get("SI_COMPANY") + get("HF_COMPANY"),
            decimal("12594.03"),
            "计提总额"
        );
    }

    /// 全勤奖的 `IF` 必须真按条件分流（`{ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0`）。
    #[test]
    fn attendance_bonus_switches_on_absence_and_unpaid_leave() {
        let rows = seed_rows();
        let bonus = rows
            .iter()
            .find(|row| row.item_code == "ATT_BONUS")
            .expect("全勤奖必须在种子里");
        let formula = bonus.formula.as_deref().expect("全勤奖有公式");

        let mut vars: std::collections::BTreeMap<String, Decimal> = std::collections::BTreeMap::new();
        vars.insert("ATT_ABSENT_DAYS".to_string(), Decimal::ZERO);
        vars.insert("ATT_LEAVE_UNPAID_DAYS".to_string(), Decimal::ZERO);
        assert_eq!(
            salary_formula::evaluate(formula, &vars).expect("全勤应算出 200"),
            decimal("200"),
            "无旷工无无薪假 ⇒ 全勤奖 200"
        );

        // 旷工 1 天
        vars.insert("ATT_ABSENT_DAYS".to_string(), decimal("1"));
        assert_eq!(
            salary_formula::evaluate(formula, &vars).expect("旷工 1 天应算出 0"),
            decimal("0"),
            "旷工 1 天 ⇒ 全勤奖 0"
        );

        // 无薪假 0.5 天（&& 的右分支也得生效）
        vars.insert("ATT_ABSENT_DAYS".to_string(), Decimal::ZERO);
        vars.insert("ATT_LEAVE_UNPAID_DAYS".to_string(), decimal("0.5"));
        assert_eq!(
            salary_formula::evaluate(formula, &vars).expect("有 0.5 天无薪假应算出 0"),
            decimal("0"),
            "有 0.5 天事假 ⇒ 全勤奖 0"
        );
    }

    /// ⑥ 幂等语义与回滚覆盖面：up 用 `WHERE NOT EXISTS`（带组织哨兵 + 软删谓词），down 覆盖同 17 个编码。
    #[test]
    fn migration_is_idempotent_and_down_covers_all_codes() {
        assert!(UP.contains("WHERE NOT EXISTS"), "up 必须用 WHERE NOT EXISTS 保证幂等");
        assert!(
            UP.contains("s.org_id = '00000000-0000-0000-0000-000000000000'::uuid"),
            "up 的幂等判定必须带组织哨兵（唯一键是 (org_id, item_code)）"
        );
        assert!(
            UP.contains("s.deleted_at IS NULL"),
            "幂等判定必须带软删谓词（唯一索引也是软删唯一）"
        );
        assert!(
            !UP.contains("ON CONFLICT"),
            "本迁移用 NOT EXISTS 幂等，不用 ON CONFLICT（要保住客户已改过的行）"
        );
        // 哨兵值必须与 Rust 常量一致（两处不一致 ⇒ 唯一键判不到种子）
        assert!(
            UP.contains(&SENTINEL_ORG.to_string()),
            "up.sql 的组织常量必须等于 SENTINEL_ORG_ID（{SENTINEL_ORG}）"
        );

        // down：17 个编码全在删除名单里 + 三条拒绝条件都写了
        let codes_line = DOWN.split("ARRAY[").nth(1).expect("down 必须有编码数组");
        for code in EXPECTED_CODES {
            assert!(codes_line.contains(&format!("'{code}'")), "down 的编码数组漏了 {code}");
        }
        assert!(DOWN.contains("biz_employee_salary"), "down 必须拦「薪资标准还在引用」");
        assert!(
            DOWN.contains("biz_salary_detail_item"),
            "down 必须拦「历史明细还在引用」"
        );
        assert!(
            DOWN.contains("'POSTED', 'REVERSED'"),
            "down 必须拦「已过账/已红冲的薪资单」"
        );
        assert!(
            DOWN.matches("RAISE EXCEPTION").count() >= 3,
            "down 的三条拒绝条件都要 RAISE EXCEPTION"
        );
        assert!(DOWN.contains("DELETE FROM biz_salary_item"), "down 必须真删种子行");
    }

    /// 解析器本身要有牙：字段切分 / NULL 识别错，上面的断言就都是假绿。
    #[test]
    fn parser_teeth_sanity() {
        let fields = split_fields("'社保(个人)', 'ROUND({BASE} / 21.75, 2)', NULL, 100");
        assert_eq!(fields, vec!["社保(个人)", "ROUND({BASE} / 21.75, 2)", "NULL", "100"]);
        assert_eq!(opt("NULL"), None);
        assert_eq!(opt("ROUND({BASE}, 2)"), Some("ROUND({BASE}, 2)".to_string()));
    }
}

// ============================================================================
// 真库用例（不开库地址 ⇒ SKIP，不假绿）
// ============================================================================
#[cfg(test)]
mod db {
    use super::*;
    use crate::business::hr::salary_item_model;
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("HR_DB_URL").or_else(|_| std::env::var("ERP_DB_URL")) else {
                    println!("SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => {
                        let database = url.rsplit('/').next().unwrap_or("?");
                        println!("HR35 真库用例已连接：database={database}");
                        set_db(connection).is_ok()
                    }
                    Err(err) => {
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 取一行里的文本列（SQL 里显式 `::text` 投影，避免 smallint/int 的类型差异）。
    fn text(row: &Row, column: &str) -> Option<String> {
        row.get_typed::<Option<String>>(column).ok().flatten()
    }

    /// 17 个种子项的查询（真库）——列全部 `::text` 投影。
    const SEED_SELECT: &str = "\
SELECT item_code, item_name, item_type, calc_type, source, formula, sort_order::text AS sort_order, \
       debit_account_code, credit_account_code, is_taxable::text AS is_taxable, \
       is_system::text AS is_system, org_id::text AS org_id \
  FROM biz_salary_item \
 WHERE org_id = ?::uuid AND deleted_at IS NULL \
   AND item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS', \
                     'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD', \
                     'SI_COMPANY','HF_COMPANY','GROSS','NET') \
 ORDER BY biz_salary_item.sort_order";

    async fn seed_list() -> Vec<Row> {
        db().query_sql(SEED_SELECT.to_string(), vec![Value::from(&SENTINEL_ORG)])
            .await
            .expect("查 17 个薪资项种子")
    }

    /// ① 验收：17 行真在库里，组织是**哨兵全零**，且逐列等于设计（本迁移种的那批）。
    #[tokio::test]
    async fn seed_rows_land_with_sentinel_org() {
        if !ensure_db().await {
            return;
        }
        let list = seed_list().await;
        assert_eq!(list.len(), 17, "库里应有 17 个薪资项种子（未软删、组织哨兵）");

        let want_org = SENTINEL_ORG.to_string();
        let seeded = seed_rows();
        for (index, expected) in seeded.iter().enumerate() {
            let actual = &list[index];
            let code = expected.item_code.as_str();
            assert_eq!(text(actual, "item_code").as_deref(), Some(code), "第 {index} 行编码");
            assert_eq!(
                text(actual, "item_name").as_deref(),
                Some(expected.item_name.as_str()),
                "{code} 名称"
            );
            assert_eq!(
                text(actual, "item_type").as_deref(),
                Some(expected.item_type.as_str()),
                "{code} 类型"
            );
            assert_eq!(
                text(actual, "calc_type").as_deref(),
                Some(expected.calc_type.as_str()),
                "{code} 算法"
            );
            assert_eq!(
                text(actual, "source").as_deref(),
                Some(expected.source.as_str()),
                "{code} 来源"
            );
            assert_eq!(text(actual, "formula"), expected.formula.clone(), "{code} 公式");
            assert_eq!(
                text(actual, "sort_order").as_deref(),
                Some(expected.sort_order.to_string().as_str()),
                "{code} 顺序"
            );
            assert_eq!(
                text(actual, "debit_account_code"),
                expected.debit.clone(),
                "{code} 借方"
            );
            assert_eq!(
                text(actual, "credit_account_code"),
                expected.credit.clone(),
                "{code} 贷方"
            );
            assert_eq!(
                text(actual, "is_taxable").as_deref(),
                Some(expected.is_taxable.to_string().as_str()),
                "{code} 计税"
            );
            assert_eq!(
                text(actual, "is_system").as_deref(),
                Some(expected.is_system.to_string().as_str()),
                "{code} 系统项"
            );
            // ★ 组织必须与 handler 写入用的 SENTINEL_ORG_ID 一致，否则唯一键判不到它们
            assert_eq!(
                text(actual, "org_id").as_deref(),
                Some(want_org.as_str()),
                "{code} 的 org_id 不是哨兵 ⇒ 用户能建出同编码的第二个项"
            );
        }
    }

    /// ② 验收：**公式能解析**——库里存着的那 7 条公式，逐条过解析器 + 语义门禁。
    #[tokio::test]
    async fn formulas_in_db_parse_and_resolve() {
        if !ensure_db().await {
            return;
        }
        let snapshots = salary_item_model::list_snapshots(db()).await.expect("取依赖图快照");
        let seeded: Vec<SalaryItemSnapshot> = snapshots
            .iter()
            .filter(|snapshot| EXPECTED_CODES.contains(&snapshot.item_code.as_str()))
            .cloned()
            .collect();
        assert_eq!(seeded.len(), 17, "库里 17 个种子项都要在依赖图快照里");

        let mut formula_n = 0;
        for snapshot in &seeded {
            let Some(formula) = snapshot.formula.as_deref() else {
                continue;
            };
            formula_n += 1;
            assert!(
                salary_formula::parse(formula).is_ok(),
                "{} 的公式解析失败：{:?}",
                snapshot.item_code,
                salary_formula::parse(formula).err()
            );
            assert!(
                salary_item_formula::structural_errors(formula).is_empty(),
                "{} 的公式没过结构门禁",
                snapshot.item_code
            );
            let check =
                crate::business::hr::salary_item_dto::check_formula(formula, Some(&snapshot.item_code), &snapshots);
            assert!(
                check.ok(),
                "{} 的公式没过语义门禁：{:?}",
                snapshot.item_code,
                check.errors
            );
            assert!(check.cycle.is_none(), "{} 的公式引入了环", snapshot.item_code);
        }
        assert_eq!(formula_n, 7, "库里应有 7 条公式（5 计算 + 2 汇总）");
    }

    /// ③ 验收：**科目指向正确**——10 个科目编码在 `biz_account` 里真实、末级、启用，
    /// 且类别/方向符合设计（借 5602.1x 费用 / 贷 2211.0x、2241.0x、2221.03 负债 / 1221 资产）。
    #[tokio::test]
    async fn accounts_point_to_real_leaf_active_accounts() {
        if !ensure_db().await {
            return;
        }
        for code in EXPECTED_ACCOUNTS {
            let list = db()
                .query_sql(
                    "SELECT account_code, account_category, balance_direction, \
                            is_leaf::text AS is_leaf, status::text AS status \
                       FROM biz_account WHERE account_code = ? AND deleted_at IS NULL"
                        .to_string(),
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("查科目");
            let row = list
                .first()
                .unwrap_or_else(|| panic!("薪资项引用的科目 {code} 不存在（V0384__hr_accounts 没跑？）"));
            assert_eq!(text(row, "account_code").as_deref(), Some(code), "科目编码");
            assert_eq!(
                text(row, "is_leaf").as_deref(),
                Some("1"),
                "科目 {code} 必须末级（非末级过账会被记账闸 400）"
            );
            assert_eq!(text(row, "status").as_deref(), Some("1"), "科目 {code} 必须启用");

            let (want_category, want_direction) = if code.starts_with("5602.") {
                ("PROFIT_LOSS", "DEBIT")
            } else if code == "1221" {
                // 其他应收款：「其他代扣」贷 1221 = 冲减借支应收（贷资产 = 减资产，方向是对的）
                ("ASSET", "DEBIT")
            } else {
                ("LIABILITY", "CREDIT")
            };
            assert_eq!(
                text(row, "account_category").as_deref(),
                Some(want_category),
                "科目 {code} 类别"
            );
            assert_eq!(
                text(row, "balance_direction").as_deref(),
                Some(want_direction),
                "科目 {code} 余额方向"
            );
        }
    }

    /// ④ 拿**库里存的公式**按设计 §4.4.4 再算一遍（DB → 公式 → 金额，全链路）。
    #[tokio::test]
    async fn trial_numbers_from_db_rows() {
        if !ensure_db().await {
            return;
        }
        let list = seed_list().await;
        assert_eq!(list.len(), 17, "17 个种子项都要取到");

        // 只保留算工资需要的三列（calc_type / formula），值由 init 提供
        let db_rows: Vec<SeedRow> = list
            .iter()
            .map(|row| SeedRow {
                item_code: text(row, "item_code").expect("编码"),
                item_name: text(row, "item_name").unwrap_or_default(),
                item_type: text(row, "item_type").unwrap_or_default(),
                calc_type: text(row, "calc_type").expect("算法"),
                source: text(row, "source").unwrap_or_default(),
                formula: text(row, "formula"),
                sort_order: 0,
                debit: None,
                credit: None,
                is_taxable: 0,
                is_system: 0,
            })
            .collect();

        let values = compute(
            &db_rows,
            &[
                ("BASE", "8000"),
                ("POST", "500"),
                ("MEAL", "300"),
                ("PERF", "1200"),
                ("SI_PERSONAL", "840"),
                ("HF_PERSONAL", "960"),
                ("IIT", "0"),
                ("OTHER_WITHHOLD", "0"),
                ("SI_COMPANY", "1680"),
                ("HF_COMPANY", "960"),
                ("ATT_SHOULD_DAYS", "22"),
                ("ATT_ACTUAL_DAYS", "20"),
                ("ATT_LEAVE_UNPAID_DAYS", "2"),
                ("ATT_ABSENT_DAYS", "0"),
                ("ATT_OT_WD", "10"),
                ("ATT_OT_WE", "0"),
                ("ATT_OT_HO", "0"),
            ],
        );
        let get = |code: &str| -> Decimal {
            values
                .iter()
                .find(|(name, _)| name == code)
                .unwrap_or_else(|| panic!("{code} 没算出来"))
                .1
        };

        assert_eq!(get("OT_WD"), decimal("689.66"), "平时加班费");
        assert_eq!(
            get("ATT_DEDUCT"),
            decimal("735.63"),
            "考勤扣款（10 位中间精度，不提前四舍五入）"
        );
        assert_eq!(get("GROSS"), decimal("9954.03"), "应发");
        assert_eq!(get("NET"), decimal("8154.03"), "实发");
    }

    /// ⑤ 种子必须**占住命名空间**：编码与计算顺序在应用层查重时都查得到。
    ///
    /// 这一段是 org 哨兵写错的直接探针：若种子落在别的 org，`code_owner("BASE")` 会返回 `None`，
    /// 用户在页面上再建一个 `BASE` 会成功 ⇒ 同编码两行，工资表静默算错。
    #[tokio::test]
    async fn seed_occupies_code_and_sort_namespace() {
        if !ensure_db().await {
            return;
        }
        for code in EXPECTED_CODES {
            let owner = salary_item_model::code_owner(db(), code, None)
                .await
                .expect("查编码占用");
            assert_eq!(
                owner.as_deref(),
                Some(code),
                "编码 {code} 应被种子占用（org 哨兵写错了吗？）"
            );
        }
        for (index, code) in EXPECTED_CODES.iter().enumerate() {
            let sort = (index as i32 + 1) * 10;
            let owner = salary_item_model::sort_owner(db(), sort, None)
                .await
                .expect("查顺序占用");
            assert_eq!(owner.as_deref(), Some(*code), "顺序 {sort} 应被 {code} 占用");
        }
        // 下一个可用顺序 = 180（种子占到 170）
        let next = salary_item_model::next_sort_order(db()).await.expect("取下一个顺序");
        assert_eq!(next, 180, "17 个种子占到 170 ⇒ 下一个可用顺序 180");
    }

    /// ⑥ 系统项与守卫：`GROSS`/`NET` 是系统项且启用；`BASE` 被 5 条种子公式引用。
    #[tokio::test]
    async fn system_items_and_referrers_are_guarded() {
        if !ensure_db().await {
            return;
        }
        let list = seed_list().await;
        for code in ["GROSS", "NET", "BASE"] {
            let row = list
                .iter()
                .find(|row| text(row, "item_code").as_deref() == Some(code))
                .unwrap_or_else(|| panic!("{code} 必须在库里"));
            let want_system = if code == "BASE" { "0" } else { "1" };
            assert_eq!(
                text(row, "is_system").as_deref(),
                Some(want_system),
                "{code} 的 is_system"
            );
        }

        // `BASE` 被 5 条种子公式引用（OT_WD / OT_WE / OT_HO / ATT_DEDUCT / GROSS）⇒ 停用它必须先拦
        let referrers = salary_item_model::formula_referrers(db(), "BASE", None)
            .await
            .expect("查引用方");
        assert!(
            referrers.len() >= 5,
            "BASE 至少被 5 条种子公式引用，实际：{referrers:?}"
        );
        for code in ["OT_WD", "OT_WE", "OT_HO", "ATT_DEDUCT", "GROSS"] {
            assert!(
                referrers.iter().any(|item| item == code),
                "{code} 应被识别为 BASE 的引用方"
            );
        }
    }
}
