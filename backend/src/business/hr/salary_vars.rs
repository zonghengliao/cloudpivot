//! 薪资**变量解析 + 依赖拓扑**（`biz_salary_item` · HR-33）。
//!
//! 设计 §4.4.2 的公式里只有两种变量：**项引用**（`{BASE}` = 另一个薪资项的编码）与
//! **考勤变量**（`{ATT_OT_WD}` = 当期考勤值，白名单见
//! [`super::salary_item_formula::ATT_VARS`]）。本文件把它们**解析成一张可执行的计算计划**：
//!
//! | 步骤 | 做什么 | 出错会怎样 |
//! |---|---|---|
//! | 变量解析 | `{X}` → `Item(X)` / `Attendance(var, 列)` | 未定义 → [`VarIssue::Undefined`]（点名变量 + 可用清单） |
//! | 语法 | 复用 HR-32 的 [`salary_formula::parse`] | 语法错 → [`VarIssue::Syntax`]（点名薪资项 + 报错） |
//! | 拓扑 | 复用 HR-31 的 [`salary_item_formula::first_cycle`] / [`topo_order`] | 有环 → [`VarIssue::Cycle`]（报出环路径） |
//! | 出计划 | [`ComputePlan`]：`order`（依赖在前）+ 每项的语法树 / 考勤绑定 | 任一条错 ⇒ `Err(Vec<VarIssue>)`，**不出半张计划** |
//!
//! # 为什么在 HR-31 / HR-32 之上还要这一层
//!
//! * HR-31 的 [`super::salary_item_dto::check_formula`] 是**单项配置期**检查：一次只看一个公式
//!   （加上库里其它项当背景），回的是给「检查按钮」看的问题清单；
//! * HR-32 的 [`salary_formula::parse`] 是**单条公式**的语法树 + 求值，不知道哪个名字是薪资项、
//!   哪个是考勤列；
//! * 试算（HR-34）要的是**整张薪资项表的执行计划**：按拓扑序逐项求值，且每项都要知道
//!   「公式里那些 `{…}` 分别去哪儿取数」。这就是 [`ComputePlan`]。
//!
//! **不重写解析器、不重写图算法**：语法树来自 HR-32，环/拓扑来自 HR-31 —— 两套口径会让
//! 「配置页说没环、算工资时说有环」。
//!
//! # 口径（每条都有理由）
//!
//! * **只有启用项进计划**：`is_active = 0` 的项不参与试算（设计 §5.2 #34 停用语义），
//!   所以它的公式坏了也不该挡住别人；但**被启用项引用**时必须报错 ——
//!   报文单独区分「已停用」（`{PERF}` 在库里、只是停用）与「根本没这个编码」（`{BASEE}`），
//!   两种修法完全不同（启用 vs 改公式）。
//! * **变量名即编码**：`{X}` 的 `X` 只能是 `biz_salary_item.item_code`
//!   （`[A-Z][A-Z0-9_]*`，与 HR-31 的 `is_var_name` 同口径），不存在「大小写不敏感」的别名。
//! * **一次性报全部问题**：配置期要能一轮改完（与 `structural_errors` 的取向一致），
//!   而不是「修好未定义、再发现环、再发现语法」。
//! * **拓扑序稳定**：入度为 0 的项按入参顺序取（HR-34 传 `ORDER BY sort_order` 的行），
//!   同一份配置每次排出同一个序 —— 前端展示、凭证行号都靠它。
//!
//! # 错误出口
//!
//! 纯函数（不连库、不碰 `ApiError`）：错误一律 [`VarIssue`]（自带中文报文），
//! 由调用方翻成 handler 的 `ResResult` / 试算前的 400。

use std::collections::{BTreeMap, BTreeSet};

use nik::prelude::rust_decimal::Decimal;

use crate::business::hr::salary_formula::{self, Expr};
use crate::business::hr::salary_item_dto::CALC_TYPE_FORMULA;
use crate::business::hr::salary_item_formula::{self as formula, DepNode};
use crate::business::hr::salary_item_model::SalaryItemRow;

/// 一个薪资项的解析输入（DB 行 → 本结构，见 [`plan_from_rows`]）。
///
/// 只带「解析变量」用得上的列：编码 / 名称（报文点人）/ 算法 / 公式 / 是否启用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpec {
    /// 编码 —— 公式里的变量名就是它。
    pub item_code: String,
    /// 名称（报文点人：`薪资项 OT_WD（平时加班费）`）。
    pub item_name: String,
    /// 算法：`FIXED` / `FORMULA` / `INPUT`（只有 `FORMULA` 有公式）。
    pub calc_type: String,
    /// 公式原文（`calc_type <> 'FORMULA'` 时应为空）。
    pub formula: Option<String>,
    /// 是否启用（`is_active = 1`）。停用项不进计划，但被引用时要报错。
    pub is_active: bool,
}

impl ItemSpec {
    /// 造一个启用项（单测 / HR-34 的取数适配器用）。
    pub fn new(
        item_code: impl Into<String>,
        item_name: impl Into<String>,
        calc_type: impl Into<String>,
        formula: Option<&str>,
    ) -> Self {
        Self {
            item_code: item_code.into(),
            item_name: item_name.into(),
            calc_type: calc_type.into(),
            formula: formula.map(str::to_string),
            is_active: true,
        }
    }

    /// 置为停用（链式，便于单测里表达「这一项被停用了」）。
    pub fn stopped(mut self) -> Self {
        self.is_active = false;
        self
    }
}

/// 一条「考勤变量 → 考勤台账列」的绑定。
///
/// HR-34 拿它去 `biz_attendance_record` 取当期值：变量名是公式里写的，列名是真正要读的字段
/// （`ATT_OT_WD` → `ot_hours_workday`）。两者都给出，报文与取数都不用再查白名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttBinding {
    /// 公式里的变量名（`ATT_OT_WD`）。
    pub var: String,
    /// `biz_attendance_record` 的列名（`ot_hours_workday`）。
    pub column: &'static str,
    /// 中文名（`平时加班小时`）。
    pub label: &'static str,
}

impl AttBinding {
    /// 按变量名取绑定（不在白名单里 → `None`，调用方据此报「未定义变量」）。
    pub fn of(var: &str) -> Option<Self> {
        formula::ATT_VARS
            .iter()
            .find(|(name, _, _)| *name == var)
            .map(|(name, column, label)| Self {
                var: (*name).to_string(),
                column: *column,
                label: *label,
            })
    }
}

/// 解析后的一个薪资项。
#[derive(Debug, Clone)]
pub struct ResolvedItem {
    /// 原始输入。
    pub spec: ItemSpec,
    /// 语法树（只有 `FORMULA` 项有；HR-34 一个公式只解析一次、算 N 个人）。
    pub expr: Option<Expr>,
    /// 公式里引用的**其它薪资项编码**（保序去重，不含 `ATT_*`）。
    pub item_refs: Vec<String>,
    /// 公式里引用的**考勤变量**（保序去重，按公式出现顺序）。
    pub att_refs: Vec<AttBinding>,
}

impl ResolvedItem {
    /// 是否公式项。
    pub fn is_formula(&self) -> bool {
        self.expr.is_some()
    }
}

/// 变量解析 / 拓扑的问题（一次性返回全部，配置期一轮改完）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarIssue {
    /// 引用了不可用的变量：`undefined` = 库里根本没有；`inactive` = 有但已停用。
    Undefined {
        /// 出问题的薪资项编码。
        item_code: String,
        /// 出问题的薪资项名称。
        item_name: String,
        /// 库里找不到的变量名（保序）。
        undefined: Vec<String>,
        /// 库里存在但已停用的编码（保序）。
        inactive: Vec<String>,
        /// 当时可用的变量清单（排序后，报文用）。
        allowed: Vec<String>,
    },
    /// 依赖成环（`path` 首尾相同，形如 `A → B → A`）。
    Cycle {
        /// 环上的编码序列（首尾相同）。
        path: Vec<String>,
        /// 环上编码对应的名称（`CODE=名称`，报文用）。
        legends: Vec<String>,
    },
    /// 公式语法错（HR-32 解析器结论）。
    Syntax {
        /// 出问题的薪资项编码。
        item_code: String,
        /// 出问题的薪资项名称。
        item_name: String,
        /// 解析器报文（含「第 N 个字符」定位）。
        message: String,
    },
    /// 花括号残缺，引用都抽不出来（`{BASE` / `{}` / `{base}`）。
    Unparsable {
        /// 出问题的薪资项编码。
        item_code: String,
        /// 出问题的薪资项名称。
        item_name: String,
        /// 报文（可照着改）。
        message: String,
    },
    /// 同一批数据里编码重复（库里有 `uk_biz_sal_item_code`，出现即是加载口径错）。
    Duplicate {
        /// 重复的编码。
        item_code: String,
    },
}

impl VarIssue {
    /// 可直接展示给用户的中文报文。
    pub fn message(&self) -> String {
        match self {
            Self::Undefined {
                item_code,
                item_name,
                undefined,
                inactive,
                allowed,
            } => {
                let mut parts: Vec<String> = Vec::new();
                if !undefined.is_empty() {
                    parts.push(format!(
                        "引用了未定义的变量 {}（库里没有这个薪资项编码，也没有这个考勤变量）",
                        quote_names(undefined)
                    ));
                }
                if !inactive.is_empty() {
                    parts.push(format!(
                        "引用了已停用的薪资项 {}（停用项不参与试算：请先启用它，或把公式改成别的项）",
                        quote_names(inactive)
                    ));
                }
                format!(
                    "薪资项 `{item_code}`（{item_name}）的公式{}；可用变量是「启用中的薪资项编码 + 考勤变量」—— {}",
                    parts.join("，并且"),
                    allowed.join(" / ")
                )
            }
            Self::Cycle { path, legends } => format!(
                "薪资项公式存在循环依赖：{}；环上的项互相引用，试算会死循环 —— 改环上任意一项的公式即可解开（{}）",
                path.join(" → "),
                legends.join("；")
            ),
            Self::Syntax {
                item_code,
                item_name,
                message,
            } => format!("薪资项 `{item_code}`（{item_name}）的公式语法有误：{message}"),
            Self::Unparsable {
                item_code,
                item_name,
                message,
            } => format!("薪资项 `{item_code}`（{item_name}）的公式引用写法有误：{message}"),
            Self::Duplicate { item_code } => format!(
                "薪资项编码 `{item_code}` 在本批数据里出现了不止一次：同一公司内编码必须唯一\
                 （库索引 `uk_biz_sal_item_code`），重复会让「引用哪一个」与计算顺序都不确定"
            ),
        }
    }

    /// 是否属于「变量不可达」（未定义 / 停用）—— 前端变量面板高亮用。
    pub fn is_unreachable_var(&self) -> bool {
        matches!(self, Self::Undefined { .. })
    }
}

impl std::fmt::Display for VarIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

fn quote_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{{{name}}}`"))
        .collect::<Vec<_>>()
        .join("、")
}

/// 计算计划：拓扑序 + 每项的解析结果。
#[derive(Debug, Clone)]
pub struct ComputePlan {
    order: Vec<String>,
    items: BTreeMap<String, ResolvedItem>,
}

impl ComputePlan {
    /// 拓扑序（依赖在前）。HR-34 照这个顺序逐项求值。
    pub fn order(&self) -> &[String] {
        &self.order
    }

    /// 取一项的解析结果。
    pub fn get(&self, item_code: &str) -> Option<&ResolvedItem> {
        self.items.get(item_code)
    }

    /// 全部项（编码 → 解析结果）。
    pub fn items(&self) -> impl Iterator<Item = (&String, &ResolvedItem)> {
        self.items.iter()
    }

    /// 项数。
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// 是否空计划（一个启用项都没有）。
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// 公式项个数。
    pub fn formula_count(&self) -> usize {
        self.items.values().filter(|item| item.is_formula()).count()
    }

    /// 计划里**实际被引用到**的考勤变量，按白名单顺序（= 考勤台账列序）去重。
    ///
    /// HR-34 的取数清单：只为这些列读 `biz_attendance_record`，不必整行搬。
    pub fn att_bindings(&self) -> Vec<AttBinding> {
        let used: BTreeSet<&str> = self
            .items
            .values()
            .flat_map(|item| item.att_refs.iter().map(|binding| binding.var.as_str()))
            .collect();
        formula::ATT_VARS
            .iter()
            .filter(|(var, _, _)| used.contains(var))
            .map(|(name, column, label)| AttBinding {
                var: (*name).to_string(),
                column,
                label,
            })
            .collect()
    }

    /// 计划里全部「可用变量名」（启用项编码 + 考勤变量），排序后。
    ///
    /// 试算报「未定义变量」时用同一份清单，与配置期报文口径一致。
    pub fn allowed_vars(&self) -> BTreeSet<String> {
        let mut allowed: BTreeSet<String> = self.items.keys().cloned().collect();
        allowed.extend(formula::att_var_names());
        allowed
    }
}

/// 把 DB 行转成输入并建计划（HR-34 的一行式入口）。
pub fn plan_from_rows(rows: &[SalaryItemRow]) -> std::result::Result<ComputePlan, Vec<VarIssue>> {
    let specs: Vec<ItemSpec> = rows
        .iter()
        .map(|row| ItemSpec {
            item_code: row.item_code.clone(),
            item_name: row.item_name.clone(),
            calc_type: row.calc_type.clone(),
            formula: row.formula.clone(),
            is_active: row.is_active == 1,
        })
        .collect();
    build_plan(&specs)
}

/// 建计算计划：变量解析（项引用 + `ATT_*` 白名单）→ 环检测 → 拓扑序。
///
/// # 参数
///
/// `specs`：**全部**薪资项（停用的也要传 —— 用来区分「停用」与「不存在」）。
/// 顺序即拓扑序的稳定次序，HR-34 传 `ORDER BY sort_order`。
///
/// # 返回
///
/// * `Ok(plan)`：全部启用项解析通过且无环；
/// * `Err(issues)`：**全部**问题（语法 / 未定义 / 停用引用 / 环 / 编码重复），一条不藏。
pub fn build_plan(specs: &[ItemSpec]) -> std::result::Result<ComputePlan, Vec<VarIssue>> {
    let mut issues: Vec<VarIssue> = Vec::new();

    // ---- 编码唯一（库里有唯一索引；重复说明加载口径错，继续跑会把「引用谁」变成随机） ----
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for spec in specs {
        if !seen.insert(spec.item_code.as_str()) {
            issues.push(VarIssue::Duplicate {
                item_code: spec.item_code.clone(),
            });
        }
    }

    let att_names: BTreeSet<String> = formula::att_var_names().into_iter().collect();
    let inactive_codes: BTreeSet<&str> = specs
        .iter()
        .filter(|spec| !spec.is_active)
        .map(|spec| spec.item_code.as_str())
        .collect();
    let active_specs: Vec<&ItemSpec> = specs.iter().filter(|spec| spec.is_active).collect();
    let active_codes: BTreeSet<String> = active_specs.iter().map(|spec| spec.item_code.clone()).collect();
    let mut allowed: BTreeSet<String> = active_codes.clone();
    allowed.extend(att_names.iter().cloned());
    let allowed_list: Vec<String> = allowed.iter().cloned().collect();

    // ---- 逐项解析变量 ----
    let mut items: BTreeMap<String, ResolvedItem> = BTreeMap::new();
    let mut nodes: Vec<DepNode> = Vec::with_capacity(active_specs.len());
    for spec in &active_specs {
        let mut resolved = ResolvedItem {
            spec: (*spec).clone(),
            expr: None,
            item_refs: Vec::new(),
            att_refs: Vec::new(),
        };
        if spec.calc_type == CALC_TYPE_FORMULA {
            let text = spec.formula.as_deref().unwrap_or_default();
            match formula::extract_refs(text) {
                Ok(refs) => {
                    let mut undefined: Vec<String> = Vec::new();
                    let mut inactive: Vec<String> = Vec::new();
                    for name in refs {
                        if let Some(binding) = AttBinding::of(&name) {
                            // 考勤变量：白名单命中即「可达」，不需要库里有行
                            resolved.att_refs.push(binding);
                        } else if active_codes.contains(&name) {
                            resolved.item_refs.push(name);
                        } else if inactive_codes.contains(name.as_str()) {
                            inactive.push(name);
                        } else {
                            undefined.push(name);
                        }
                    }
                    if !undefined.is_empty() || !inactive.is_empty() {
                        issues.push(VarIssue::Undefined {
                            item_code: spec.item_code.clone(),
                            item_name: spec.item_name.clone(),
                            undefined,
                            inactive,
                            allowed: allowed_list.clone(),
                        });
                    }
                    // 语法树（HR-34 复用；这里也顺便把「结构检查放过、解析器不认」的写法挡住，
                    // 例如 `%` / 单个 `=`）
                    match salary_formula::parse(text) {
                        Ok(expr) => resolved.expr = Some(expr),
                        Err(error) => issues.push(VarIssue::Syntax {
                            item_code: spec.item_code.clone(),
                            item_name: spec.item_name.clone(),
                            message: error.message().to_string(),
                        }),
                    }
                }
                Err(message) => {
                    issues.push(VarIssue::Unparsable {
                        item_code: spec.item_code.clone(),
                        item_name: spec.item_name.clone(),
                        message,
                    });
                }
            }
        }
        nodes.push((spec.item_code.clone(), resolved.item_refs.clone()));
        items.insert(spec.item_code.clone(), resolved);
    }

    // ---- 环 / 拓扑（先环：有环时拓扑排不出来，报环路径才有用） ----
    if let Some(path) = formula::first_cycle(&nodes) {
        let legends = path
            .iter()
            .map(|code| {
                let name = items.get(code).map(|item| item.spec.item_name.as_str()).unwrap_or("");
                format!("{code}={name}")
            })
            .collect();
        issues.push(VarIssue::Cycle { path, legends });
    }

    if !issues.is_empty() {
        return Err(issues);
    }

    let Some(order) = formula::topo_order(&nodes) else {
        // 无环时不可能走到这里；真到了说明两套图算法口径漂了 —— 报成环而不是静默出半张计划
        return Err(vec![VarIssue::Cycle {
            path: Vec::new(),
            legends: Vec::new(),
        }]);
    };
    Ok(ComputePlan { order, items })
}

/// 求值用的小工具（HR-34 也用同一套，单测里证明计划可执行）：
/// 按拓扑序把公式项算出来写回 `scope`，非公式项（FIXED / INPUT）由调用方预先放好。
///
/// 只做「顺序 + 作用域」这一件事：除零 / 未定义变量由 HR-32 的求值器报错，
/// 取数（薪资标准 / 考勤 / 手工录入）是 HR-34 的事。
pub fn eval_formulas(
    plan: &ComputePlan,
    scope: &mut BTreeMap<String, Decimal>,
) -> std::result::Result<(), salary_formula::FormulaError> {
    for code in plan.order() {
        let Some(item) = plan.get(code) else {
            continue;
        };
        let Some(expr) = item.expr.as_ref() else {
            continue;
        };
        let value = expr.eval_number(&*scope)?;
        scope.insert(code.clone(), value);
    }
    Ok(())
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 设计 §4.4.3 的 17 个种子项（15 计算项 + 2 汇总项），逐条照抄。
    fn seed_specs() -> Vec<ItemSpec> {
        vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("POST", "岗位津贴", "FIXED", None),
            ItemSpec::new("MEAL", "餐补", "FIXED", None),
            ItemSpec::new("PERF", "绩效奖金", "INPUT", None),
            ItemSpec::new(
                "OT_WD",
                "平时加班费",
                "FORMULA",
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)"),
            ),
            ItemSpec::new(
                "OT_WE",
                "休息日加班费",
                "FORMULA",
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)"),
            ),
            ItemSpec::new(
                "OT_HO",
                "节假日加班费",
                "FORMULA",
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)"),
            ),
            ItemSpec::new(
                "ATT_BONUS",
                "全勤奖",
                "FORMULA",
                Some("IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)"),
            ),
            ItemSpec::new(
                "ATT_DEDUCT",
                "考勤扣款",
                "FORMULA",
                Some("ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)"),
            ),
            ItemSpec::new("SI_PERSONAL", "社保(个人)", "INPUT", None),
            ItemSpec::new("HF_PERSONAL", "公积金(个人)", "INPUT", None),
            ItemSpec::new("IIT", "个人所得税", "INPUT", None),
            ItemSpec::new("OTHER_WITHHOLD", "其他代扣", "INPUT", None),
            ItemSpec::new("SI_COMPANY", "单位社保", "INPUT", None),
            ItemSpec::new("HF_COMPANY", "单位公积金", "INPUT", None),
            ItemSpec::new(
                "GROSS",
                "应发合计",
                "FORMULA",
                Some("{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}"),
            ),
            ItemSpec::new(
                "NET",
                "实发合计",
                "FORMULA",
                Some("{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}"),
            ),
        ]
    }

    fn position(order: &[String], code: &str) -> usize {
        order.iter().position(|item| item == code).unwrap_or(usize::MAX)
    }

    // ---- 验收项：正常（无未定义、无环，依赖在前 + 变量解析正确） ----

    #[test]
    fn seed_plan_resolves_variables_and_orders_dependencies() {
        let plan = build_plan(&seed_specs()).expect("设计自己的 17 项必须建得出计划");
        assert_eq!(plan.len(), 17);
        assert_eq!(plan.formula_count(), 7, "5 个计算项 + GROSS/NET");

        let order = plan.order();
        for (earlier, later) in [
            ("BASE", "OT_WD"),
            ("OT_WD", "GROSS"),
            ("ATT_DEDUCT", "GROSS"),
            ("GROSS", "NET"),
            ("SI_PERSONAL", "NET"),
        ] {
            assert!(
                position(order, earlier) < position(order, later),
                "{earlier} 必须先于 {later} 算：{order:?}"
            );
        }
        // 拓扑序稳定：同一份输入再排一次结果一致
        let again = build_plan(&seed_specs()).expect("第二次也必须成功");
        assert_eq!(again.order(), order);

        // 变量解析：项引用 / 考勤变量各归各位
        let ot = plan.get("OT_WD").expect("OT_WD 在计划里");
        assert_eq!(ot.item_refs, vec!["BASE".to_string()]);
        assert_eq!(
            ot.att_refs.iter().map(|b| b.var.as_str()).collect::<Vec<_>>(),
            vec!["ATT_OT_WD"]
        );
        assert_eq!(ot.att_refs[0].column, "ot_hours_workday");
        assert!(ot.is_formula());

        // 非公式项没有语法树（HR-34 按 calc_type 取数）
        let base = plan.get("BASE").expect("BASE 在计划里");
        assert!(base.expr.is_none());
        assert!(base.item_refs.is_empty() && base.att_refs.is_empty());

        // 取数清单：只含被引用到的考勤变量，按白名单（台账列）顺序
        let bindings = plan.att_bindings();
        let vars: Vec<&str> = bindings.iter().map(|b| b.var.as_str()).collect();
        assert_eq!(
            vars,
            vec![
                "ATT_LEAVE_UNPAID_DAYS",
                "ATT_ABSENT_DAYS",
                "ATT_OT_WD",
                "ATT_OT_WE",
                "ATT_OT_HO"
            ],
            "白名单顺序：{bindings:?}"
        );
        let columns: Vec<&str> = bindings.iter().map(|b| b.column).collect();
        assert!(columns.contains(&"leave_unpaid_days"));
        assert!(columns.contains(&"absent_days"));
        assert!(!columns.contains(&"late_count"), "没被引用到的列不该出现在取数清单里");
    }

    /// 计划可执行：按拓扑序算出设计 §4.4.4 的张三实测算例（689.66 / 735.63 / 9954.03 / 8154.03）。
    #[test]
    fn seed_plan_evaluates_design_example() {
        let plan = build_plan(&seed_specs()).expect("计划");
        let mut scope: BTreeMap<String, Decimal> = BTreeMap::new();
        // 取数部分（HR-34 的活）：薪资标准 + 手工录入
        for (code, value) in [
            ("BASE", 8000),
            ("POST", 500),
            ("MEAL", 300),
            ("PERF", 1200),
            ("SI_PERSONAL", 840),
            ("HF_PERSONAL", 960),
            ("IIT", 0),
            ("OTHER_WITHHOLD", 0),
            ("SI_COMPANY", 1680),
            ("HF_COMPANY", 960),
        ] {
            scope.insert(code.to_string(), Decimal::from(value));
        }
        // 考勤：should 22 / actual 20 / 无薪假 2 / 平时加班 10h
        scope.insert("ATT_SHOULD_DAYS".to_string(), Decimal::from(22));
        scope.insert("ATT_ACTUAL_DAYS".to_string(), Decimal::from(20));
        scope.insert("ATT_LEAVE_UNPAID_DAYS".to_string(), Decimal::from(2));
        scope.insert("ATT_ABSENT_DAYS".to_string(), Decimal::from(0));
        scope.insert("ATT_OT_WD".to_string(), Decimal::from(10));
        scope.insert("ATT_OT_WE".to_string(), Decimal::from(0));
        scope.insert("ATT_OT_HO".to_string(), Decimal::from(0));

        eval_formulas(&plan, &mut scope).expect("种子公式必须都能算");

        assert_eq!(scope["OT_WD"], Decimal::new(68966, 2), "平时加班费");
        assert_eq!(scope["ATT_DEDUCT"], Decimal::new(73563, 2), "考勤扣款");
        assert_eq!(scope["ATT_BONUS"], Decimal::ZERO, "有 2 天事假 → 无全勤奖");
        assert_eq!(scope["GROSS"], Decimal::new(995403, 2), "应发合计");
        assert_eq!(scope["NET"], Decimal::new(815403, 2), "实发合计");
    }

    // ---- 验收项：环检测报出环路径 ----

    /// 三项成环：报出 `A → B → C → A`（首尾相同）+ 每项名称。
    #[test]
    fn cycle_reports_full_path_with_names() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("A", "津贴", "FORMULA", Some("{B} + 1")),
            ItemSpec::new("B", "奖金", "FORMULA", Some("{C} * 2")),
            ItemSpec::new("C", "补助", "FORMULA", Some("{A} - 3")),
        ];
        let issues = build_plan(&specs).expect_err("有环必须报错");
        let cycle = issues
            .iter()
            .find_map(|issue| match issue {
                VarIssue::Cycle { path, legends } => Some((path.clone(), legends.clone())),
                _ => None,
            })
            .expect("必须报 Cycle");
        assert_eq!(cycle.0.len(), 4, "A → B → C → A：{cycle:?}");
        assert_eq!(cycle.0.first(), cycle.0.last(), "首尾相同的环：{cycle:?}");
        assert_eq!(cycle.0.len() - 1, 3, "环上三项");
        assert!(cycle.1.iter().any(|legend| legend.contains("A=津贴")));
        let message = issues
            .iter()
            .find(|issue| matches!(issue, VarIssue::Cycle { .. }))
            .expect("Cycle")
            .message();
        assert!(message.contains(" → "), "报文要画出环路径：{message}");
        assert!(message.contains("津贴") && message.contains("补助"), "{message}");
    }

    /// 自引用也是环（`A = {A} + 1`），且报文点名 A。
    #[test]
    fn self_reference_is_a_cycle() {
        let specs = vec![ItemSpec::new("A", "津贴", "FORMULA", Some("{A} + 1"))];
        let issues = build_plan(&specs).expect_err("自引用必须报错");
        let cycle = issues
            .iter()
            .find_map(|issue| match issue {
                VarIssue::Cycle { path, .. } => Some(path.clone()),
                _ => None,
            })
            .expect("必须报 Cycle");
        assert_eq!(cycle, vec!["A".to_string(), "A".to_string()]);
    }

    // ---- 验收项：未定义变量报错含变量名 ----

    /// `{BASEE}` 拼错 → 报未定义变量，点名 `{BASEE}` + 可用清单 + 出问题的项。
    #[test]
    fn undefined_variable_is_named_with_available_list() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new(
                "OT_WD",
                "平时加班费",
                "FORMULA",
                Some("ROUND({BASEE} / 21.75 * 1.5, 2)"),
            ),
        ];
        let issues = build_plan(&specs).expect_err("未定义变量必须报错");
        let undefined = issues
            .iter()
            .find_map(|issue| match issue {
                VarIssue::Undefined {
                    item_code,
                    item_name,
                    undefined,
                    ..
                } => Some((item_code.clone(), item_name.clone(), undefined.clone())),
                _ => None,
            })
            .expect("必须报 Undefined");
        assert_eq!(undefined.0, "OT_WD");
        assert_eq!(undefined.1, "平时加班费");
        assert_eq!(undefined.2, vec!["BASEE".to_string()]);

        let message = issues[0].message();
        assert!(message.contains("`{BASEE}`"), "报文必须含变量名：{message}");
        assert!(message.contains("OT_WD") && message.contains("平时加班费"), "{message}");
        assert!(message.contains("BASE"), "报文要给可用清单：{message}");
        assert!(
            message.contains("ATT_OT_WD"),
            "可用清单要含考勤变量（否则用户不知道能引用什么）：{message}"
        );
    }

    /// 引用了停用项：与「不存在」分开报，修法不同（启用 vs 改公式）。
    #[test]
    fn inactive_reference_is_distinguished_from_undefined() {
        let specs = vec![
            ItemSpec::new("PERF", "绩效奖金", "INPUT", None).stopped(),
            ItemSpec::new("GROSS", "应发合计", "FORMULA", Some("{PERF} + 100")),
        ];
        let issues = build_plan(&specs).expect_err("引用停用项必须报错");
        let (undefined, inactive) = issues
            .iter()
            .find_map(|issue| match issue {
                VarIssue::Undefined {
                    undefined, inactive, ..
                } => Some((undefined.clone(), inactive.clone())),
                _ => None,
            })
            .expect("必须报 Undefined");
        assert!(undefined.is_empty(), "PERF 在库里，只是停用：{undefined:?}");
        assert_eq!(inactive, vec!["PERF".to_string()]);
        let message = issues[0].message();
        assert!(message.contains("已停用"), "{message}");
        assert!(message.contains("`{PERF}`"), "{message}");
    }

    /// 停用项自己公式坏了不影响建计划（它不参与试算）；但它不进拓扑序。
    #[test]
    fn stopped_items_are_not_planned() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("OLD", "废弃项", "FORMULA", Some("{NOPE} + 1")).stopped(),
        ];
        let plan = build_plan(&specs).expect("停用项的坏公式不挡计划");
        assert_eq!(plan.len(), 1);
        assert!(plan.get("OLD").is_none());
        assert_eq!(plan.order(), ["BASE".to_string()]);
    }

    // ---- 其它错误：语法 / 残缺引用 / 重复编码 ----

    /// 语法层错误也点名薪资项（结构检查放过、解析器不认的写法：`%`）。
    #[test]
    fn syntax_error_names_the_item() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("BONUS", "全勤奖", "FORMULA", Some("{BASE} % 2")),
        ];
        let issues = build_plan(&specs).expect_err("`%` 不在 DSL 里");
        let syntax = issues
            .iter()
            .find(|issue| matches!(issue, VarIssue::Syntax { .. }))
            .expect("必须报 Syntax");
        let message = syntax.message();
        assert!(message.contains("BONUS") && message.contains("全勤奖"), "{message}");
        assert!(message.contains("`%`") || message.contains('%'), "{message}");
    }

    /// 花括号残缺（`{BASE`）走 Unparsable，报文含项名。
    #[test]
    fn broken_braces_are_reported() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("X", "津贴", "FORMULA", Some("{BASE + 1")),
        ];
        let issues = build_plan(&specs).expect_err("残缺引用必须报错");
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, VarIssue::Unparsable { item_code, .. } if item_code == "X")),
            "{issues:?}"
        );
    }

    /// 编码重复：同一批数据里出现两次 →报错（库索引保证不了加载口径）。
    #[test]
    fn duplicate_codes_are_reported() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new("BASE", "基本工资(旧)", "FIXED", None),
        ];
        let issues = build_plan(&specs).expect_err("编码重复必须报错");
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, VarIssue::Duplicate { item_code } if item_code == "BASE")),
            "{issues:?}"
        );
    }

    /// 空计划：一个启用项都没有（新公司还没配薪资项）。
    #[test]
    fn empty_plan_is_ok() {
        let plan = build_plan(&[]).expect("空输入不该报错");
        assert!(plan.is_empty());
        assert!(plan.att_bindings().is_empty());
        let all_stopped = vec![ItemSpec::new("BASE", "基本工资", "FIXED", None).stopped()];
        let plan = build_plan(&all_stopped).expect("全部停用也不该报错");
        assert!(plan.is_empty());
    }

    /// 考勤变量名必须在白名单里；`ATT_*` 写错（`ATT_OT_WD2`）报未定义而不是被当成薪资项。
    #[test]
    fn misspelled_attendance_variable_is_undefined() {
        let specs = vec![
            ItemSpec::new("BASE", "基本工资", "FIXED", None),
            ItemSpec::new(
                "OT_WD",
                "平时加班费",
                "FORMULA",
                Some("ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD2} * 1.5, 2)"),
            ),
        ];
        let issues = build_plan(&specs).expect_err("写错考勤变量必须报错");
        let undefined = issues
            .iter()
            .find_map(|issue| match issue {
                VarIssue::Undefined { undefined, .. } => Some(undefined.clone()),
                _ => None,
            })
            .expect("必须报 Undefined");
        assert_eq!(undefined, vec!["ATT_OT_WD2".to_string()]);
        assert!(AttBinding::of("ATT_OT_WD").is_some());
        assert!(AttBinding::of("ATT_OT_WD2").is_none());
    }

    /// 白名单里每一列都能解析成「变量 → 台账列」的绑定（HR-34 的取数地图）。
    #[test]
    fn every_whitelisted_var_binds_a_column() {
        for (var, column, label) in formula::ATT_VARS {
            let binding = AttBinding::of(var).unwrap_or_else(|| panic!("白名单变量解析不到：{var}"));
            assert_eq!(binding.var, var);
            assert_eq!(binding.column, column);
            assert_eq!(binding.label, label);
        }
    }
}
