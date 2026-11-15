//! 员工薪资标准入参 / 回执契约（`biz_employee_salary` · HR-16）。
//!
//! 设计 §5.2 #16 / #17 两条接口的**形状**都在这里：
//!
//! | 接口 | 入参 | 回执 |
//! |---|---|---|
//! | `GET /business/hr/employee/{id}/salary` | — | [`SalaryListRes`]（现行 + 全量历史） |
//! | `POST /business/hr/employee/{id}/salary` | [`SalaryAdjustReq`]（调薪 / 登记 / 修正 / 取消） | [`SalaryAdjustRes`]（写了哪些 / 收口了哪些 / 取消了什么 + 最新全量） |
//!
//! 出参的行投影**复用 HR-11 的 [`EmployeeSalaryRes`]**（`item_code` / `item_name` / `amount` /
//! `effective_from` / `effective_to` / `current` / `remark`）：员工详情的薪资 tab、薪资标准页、
//! 调薪回执因此是**同一份形状** —— 前端一个组件画三处，不会出现「详情有 current、回执没有」。
//!
//! # 为什么一个 POST 覆盖增 / 改 / 删
//!
//! 设计只给了两条接口（#16 读、#17 写），没有「删一条标准」的独立路由。而薪资台账的
//! 三个真实动作离不开同一个入口：
//!
//! | 动作 | 载荷 | 语义 |
//! |---|---|---|
//! | 调薪 / 首次录入 | `amount` + 无 `effective_to` | 插一条「自生效日起至今」的现行标准，旧开放行收口到 `生效日 - 1` |
//! | 补登历史 / 预置计划 | `amount` + `effective_to` | 插一条**明确区间**的标准（一条历史都不动） |
//! | 修正错金额 | 同上，`effective_from` 相同 | `uk_biz_emp_salary` 决定「同一天只有一行」，所以是原地改金额（幂等重放） |
//! | 取消某项 | `amount` 缺省 / `null` | 覆盖生效日的旧行收口到 `-1 天`，起点 ≥ 生效日的行软删 |
//!
//! 多开一条 `DELETE /salary/{id}` 会让「写标准」出现两个入口、两套收口逻辑
//! （`handler::ROUTE_CONTRACT` 的 46 条是设计定稿，本卡不改契约），所以压在同一个 POST 上，
//! 用 `amount` 缺省表达「取消」。
//!
//! # 校验口径
//!
//! - `effective_from` **必填**（`yyyy-MM-dd`）；
//! - `items` 非空、≤ [`MAX_ITEMS`] 条、`item_code` 不许重复（重复会让同一请求里出现两种意图）；
//! - `amount` ≥ 0（库 `ck_biz_emp_salary_amount`）、小数位 ≤ 4（列是 `NUMERIC(18,4)`，
//!   多于 4 位会被库**静默四舍五入** —— 工资标准不许静默改数，所以在这里拦）；
//! - `effective_to` ≥ `effective_from`（库 `ck_biz_emp_salary_range`）；
//! - 取消（`amount` 缺省）时 `effective_to` 必须为空（「从某天起取消」没有截止日的概念）。
//!
//! 薪资项**是否存在**不在这里判（要查库）：接口层用 [`crate::business::hr::salary_model::item_name_of`]
//! 逐项确认 —— 弱引用不建 FK，拼错的编码会变成「永远取不到标准的孤儿行」。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::hr::employee_dto::{EmployeeSalaryRes, SalaryInput};
use crate::shared::error_code;

// ==================== 长度上限（与 V0383 的列宽一致） ====================

/// `item_code VARCHAR(32)`。
pub const MAX_ITEM_CODE: usize = 32;

/// `remark VARCHAR(200)`。
pub const MAX_REMARK: usize = 200;

/// 一次调薪最多几个薪资项（一期 17 个薪资项，留一倍余量；挡住「贴了 500 行进来」）。
pub const MAX_ITEMS: usize = 40;

/// `amount` 的小数位上限（`NUMERIC(18,4)`）。
pub const MAX_AMOUNT_SCALE: u32 = 4;

/// `amount` 的整数位上限（`NUMERIC(18,4)` → 整数部分最多 14 位，即 < 1e14）。
fn amount_ceiling() -> Decimal {
    Decimal::from_i128_with_scale(10_i128.pow(14), 0)
}

// ==================== 入参 ====================

/// 一个薪资项的登记 / 调薪 / 取消。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryItemReq {
    /// 薪资项编码（弱引用 `biz_salary_item.item_code`，必填）。
    pub item_code: Option<String>,
    /// 月标准金额。**缺省或 `null` = 取消该项自 `effective_from` 起的标准**。
    pub amount: Option<Decimal>,
    /// 生效止（含）。缺省 = 至今（现行标准）；有值 = 明确的历史 / 计划区间。
    pub effective_to: Option<String>,
    /// 备注（调薪原因 / 依据文号）。
    pub remark: Option<String>,
}

/// 调薪 / 登记入参（`POST /business/hr/employee/{id}/salary`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryAdjustReq {
    /// 生效起（含，`yyyy-MM-dd`，必填）。调薪日 / 新标准起始日。
    pub effective_from: Option<String>,
    /// 本次要写 / 要取消的薪资项。
    pub items: Option<Vec<SalaryItemReq>>,
}

/// 校验通过的一条写入意图（接口层直接吃这一份，不再碰原始 JSON）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryWrite {
    /// 薪资项编码（已 trim）。
    pub item_code: String,
    /// 月标准金额；`None` = 取消该项自 `effective_from` 起的标准。
    pub amount: Option<Decimal>,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含）；`None` = 至今。
    pub effective_to: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

impl SalaryWrite {
    /// 是否是「取消」意图（`amount` 缺省）。
    pub fn is_cancel(&self) -> bool {
        self.amount.is_none()
    }

    /// 是否是「现行标准」（无终止日 = 至今）。
    pub fn is_open_ended(&self) -> bool {
        self.effective_to.is_none()
    }

    /// 取消时要收口的最后一天（`effective_from - 1`）；日期下界溢出 → `None`。
    pub fn last_day_before(&self) -> Option<NaiveDate> {
        self.effective_from.pred_opt()
    }
}

// ==================== 校验 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn check_len(text: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    if text.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(())
}

/// `yyyy-MM-dd`。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 日期格式应为 yyyy-MM-dd（收到 `{text}`）")))
}

fn parse_optional_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).unwrap_or_default() {
        "" => Ok(None),
        text => Ok(Some(parse_date(text, field)?)),
    }
}

/// 金额校验：非负、小数位 ≤ 4、整数位在 `NUMERIC(18,4)` 范围内。
fn check_amount(amount: Decimal, item_code: &str) -> std::result::Result<Decimal, ApiError> {
    if amount.is_sign_negative() {
        return Err(bad_request(format!(
            "薪资项「{item_code}」的标准金额不能为负数（收到 {amount}）：月标准是正数，\
             扣款项也是正数、由薪资项类型决定借贷方向"
        )));
    }
    if amount.scale() > MAX_AMOUNT_SCALE {
        return Err(bad_request(format!(
            "薪资项「{item_code}」的标准金额最多 {MAX_AMOUNT_SCALE} 位小数（收到 {amount}）：\
             库列是 NUMERIC(18,4)，多出的位数会被静默四舍五入，工资标准不接受静默改数"
        )));
    }
    if amount >= amount_ceiling() {
        return Err(bad_request(format!(
            "薪资项「{item_code}」的标准金额超出 NUMERIC(18,4) 上限（收到 {amount}）"
        )));
    }
    Ok(amount)
}

/// 解析并校验调薪入参 → 每项的写入意图（顺序 = 请求顺序）。
///
/// 一次性把所有字段错误报到**第一个**出问题的项上（报文点名 `items[i].字段`）：
/// 前端把这句话直接铺在表单项下面，HR 不用猜是哪一行填错了。
pub fn parse_adjust(req: &SalaryAdjustReq) -> std::result::Result<(NaiveDate, Vec<SalaryWrite>), ApiError> {
    let effective_from = parse_date(req.effective_from.as_deref().unwrap_or_default(), "effective_from")?;
    let items = req.items.as_deref().unwrap_or_default();
    if items.is_empty() {
        return Err(bad_request(
            "items 不能为空：调薪请给至少一个薪资项（取消某项标准用 amount 缺省）",
        ));
    }
    if items.len() > MAX_ITEMS {
        return Err(bad_request(format!(
            "一次最多提交 {MAX_ITEMS} 个薪资项（收到 {} 个）",
            items.len()
        )));
    }

    let mut writes: Vec<SalaryWrite> = Vec::with_capacity(items.len());
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let field = format!("items[{index}]");
        let item_code = item.item_code.as_deref().map(str::trim).unwrap_or_default();
        if item_code.is_empty() {
            return Err(bad_request(format!("{field}.item_code 不能为空")));
        }
        check_len(item_code, &format!("{field}.item_code"), MAX_ITEM_CODE)?;
        if !seen.insert(item_code.to_string()) {
            return Err(bad_request(format!(
                "薪资项「{item_code}」在本次请求里出现了两次：同一项只能有一个意图（调薪或取消）"
            )));
        }
        let remark = item.remark.as_deref().map(str::trim).unwrap_or_default();
        let remark = if remark.is_empty() {
            None
        } else {
            check_len(remark, &format!("{field}.remark"), MAX_REMARK)?;
            Some(remark.to_string())
        };
        let effective_to = parse_optional_date(item.effective_to.as_deref(), &format!("{field}.effective_to"))?;
        let amount = match item.amount {
            Some(amount) => Some(check_amount(amount, item_code)?),
            None => {
                if effective_to.is_some() {
                    return Err(bad_request(format!(
                        "{field}：取消某项标准（amount 缺省）时不能给 effective_to —— \
                         取消的语义是「自生效日起不再有这一项」，没有截止日"
                    )));
                }
                None
            }
        };
        if let Some(to) = effective_to
            && to < effective_from
        {
            return Err(bad_request(format!(
                "{field}.effective_to（{to}）不能早于 effective_from（{effective_from}）"
            )));
        }
        writes.push(SalaryWrite {
            item_code: item_code.to_string(),
            amount,
            effective_from,
            effective_to,
            remark,
        });
    }
    Ok((effective_from, writes))
}

// ==================== 回执 ====================

/// 一组行 → (全量, 现行) 两份投影（三次回执共用，避免「current 口径」写出两个版本）。
pub fn project(inputs: Vec<SalaryInput>, as_of: NaiveDate) -> (Vec<EmployeeSalaryRes>, Vec<EmployeeSalaryRes>) {
    let items: Vec<EmployeeSalaryRes> = inputs
        .into_iter()
        .map(|input| EmployeeSalaryRes::from_input(input, as_of))
        .collect();
    let current = items.iter().filter(|row| row.current).cloned().collect();
    (items, current)
}

/// `GET /business/hr/employee/{id}/salary` 的回执（设计 §5.2 #16「当前生效标准 + 历史」）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryListRes {
    /// 员工主键。
    pub employee_id: String,
    /// 工号（回执里带一眼能认人的两个字段，前端抽屉标题直接用）。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 业务日（`current` 的判定基准；前端显示「截至 2026-11-12 的现行标准」）。
    pub as_of: NaiveDate,
    /// 全部标准（`effective_from DESC`，含现行与历史）。
    pub items: Vec<EmployeeSalaryRes>,
    /// 现行标准（`as_of` 落在区间内的子集，按薪资项编码排序）。
    pub current: Vec<EmployeeSalaryRes>,
}

impl SalaryListRes {
    /// 由员工信息 + 行投影组装。
    pub fn build(
        employee_id: String,
        emp_no: String,
        name: String,
        inputs: Vec<SalaryInput>,
        as_of: NaiveDate,
    ) -> Self {
        let (items, mut current) = project(inputs, as_of);
        current.sort_by(|left, right| left.item_code.cmp(&right.item_code));
        Self {
            employee_id,
            emp_no,
            name,
            as_of,
            items,
            current,
        }
    }
}

/// `POST /business/hr/employee/{id}/salary` 的回执。
///
/// 三份清单回答三个问题：**写了什么**（`written`，含原地修正）、**收口了哪几条**
/// （`closed`，旧行的 `effective_to` 被改成 `生效日 - 1`）、**取消了哪些项**
/// （`canceled`，薪资项编码）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryAdjustRes {
    /// 员工主键。
    pub employee_id: String,
    /// 工号。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 本次生效日。
    pub effective_from: NaiveDate,
    /// 业务日（`current` 判定基准）。
    pub as_of: NaiveDate,
    /// 本次新增 / 修正的行（修正 = 同生效日原地改金额，`id` 与旧行相同）。
    pub written: Vec<EmployeeSalaryRes>,
    /// 本次被收口的旧行（`effective_to` = `生效日 - 1`）。
    pub closed: Vec<EmployeeSalaryRes>,
    /// 本次取消的薪资项编码。
    pub canceled: Vec<String>,
    /// 写入后的全部标准（前端一次刷新，不用再发 GET）。
    pub items: Vec<EmployeeSalaryRes>,
    /// 写入后的现行标准。
    pub current: Vec<EmployeeSalaryRes>,
}

impl SalaryAdjustRes {
    /// 由员工信息 + 三份清单 + 最新全量组装。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        employee_id: String,
        emp_no: String,
        name: String,
        effective_from: NaiveDate,
        as_of: NaiveDate,
        written: Vec<SalaryInput>,
        closed: Vec<SalaryInput>,
        canceled: Vec<String>,
        inputs: Vec<SalaryInput>,
    ) -> Self {
        let (written, _) = project(written, as_of);
        let (closed, _) = project(closed, as_of);
        let (items, mut current) = project(inputs, as_of);
        current.sort_by(|left, right| left.item_code.cmp(&right.item_code));
        Self {
            employee_id,
            emp_no,
            name,
            effective_from,
            as_of,
            written,
            closed,
            canceled,
            items,
            current,
        }
    }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn money(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法金额")
    }

    fn item(code: &str, amount: Option<&str>) -> SalaryItemReq {
        SalaryItemReq {
            item_code: Some(code.to_string()),
            amount: amount.map(money),
            effective_to: None,
            remark: None,
        }
    }

    fn req(from: &str, items: Vec<SalaryItemReq>) -> SalaryAdjustReq {
        SalaryAdjustReq {
            effective_from: Some(from.to_string()),
            items: Some(items),
        }
    }

    /// 正常调薪：无终止日 = 现行标准（至今）。
    #[test]
    fn parses_open_ended_adjust() {
        let (from, writes) = parse_adjust(&req("2026-12-01", vec![item("BASE", Some("5602.10"))])).expect("解析成功");
        assert_eq!(from, day("2026-12-01"));
        assert_eq!(writes.len(), 1);
        let write = &writes[0];
        assert_eq!(write.item_code, "BASE");
        assert_eq!(write.amount, Some(money("5602.10")));
        assert_eq!(write.effective_from, day("2026-12-01"));
        assert!(write.effective_to.is_none());
        assert!(write.is_open_ended());
        assert!(!write.is_cancel());
        assert_eq!(write.last_day_before(), Some(day("2026-11-30")));
    }

    /// 明确区间（补登历史 / 预置计划）。
    #[test]
    fn parses_explicit_range() {
        let mut row = item("POST", Some("1200"));
        row.effective_to = Some("2026-06-30".to_string());
        let (_, writes) = parse_adjust(&req("2026-01-01", vec![row])).expect("解析成功");
        assert_eq!(writes[0].effective_to, Some(day("2026-06-30")));
        assert!(!writes[0].is_open_ended());
    }

    /// 取消：`amount` 缺省。
    #[test]
    fn parses_cancel_intent() {
        let (_, writes) = parse_adjust(&req("2026-12-01", vec![item("MEAL", None)])).expect("解析成功");
        assert!(writes[0].is_cancel());
        assert_eq!(writes[0].last_day_before(), Some(day("2026-11-30")));
    }

    /// `effective_from` 必填且格式固定。
    #[test]
    fn effective_from_is_required_and_iso() {
        let error = parse_adjust(&SalaryAdjustReq::default()).expect_err("空请求必须拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("effective_from 不能为空"));

        let error = parse_adjust(&req("2026/12/01", vec![item("BASE", Some("1"))])).expect_err("格式错");
        assert!(error.msg.contains("yyyy-MM-dd"), "{}", error.msg);
    }

    /// `items` 空 / 超上限 / 缺 `item_code` / 重复项 都要拒。
    #[test]
    fn items_shape_is_validated() {
        let error = parse_adjust(&req("2026-12-01", vec![])).expect_err("空 items");
        assert!(error.msg.contains("items 不能为空"));

        let many: Vec<SalaryItemReq> = (0..(MAX_ITEMS + 1))
            .map(|i| item(&format!("I{i}"), Some("1")))
            .collect();
        let error = parse_adjust(&req("2026-12-01", many)).expect_err("超上限");
        assert!(error.msg.contains(&MAX_ITEMS.to_string()));

        let error = parse_adjust(&req("2026-12-01", vec![item("", Some("1"))])).expect_err("缺编码");
        assert!(error.msg.contains("items[0].item_code 不能为空"));

        let error =
            parse_adjust(&req("2026-12-01", vec![item("BASE", Some("1")), item("BASE", None)])).expect_err("重复");
        assert!(error.msg.contains("出现了两次"));
    }

    /// 金额三条闸：非负 / 小数位 ≤ 4 / 不超 NUMERIC(18,4)。
    #[test]
    fn amount_guards() {
        let error = parse_adjust(&req("2026-12-01", vec![item("BASE", Some("-1"))])).expect_err("负数");
        assert!(error.msg.contains("不能为负数"));

        let error = parse_adjust(&req("2026-12-01", vec![item("BASE", Some("1.23456"))])).expect_err("5 位小数");
        assert!(error.msg.contains("最多 4 位小数"), "{}", error.msg);

        let error = parse_adjust(&req("2026-12-01", vec![item("BASE", Some("100000000000000"))])).expect_err("超上限");
        assert!(error.msg.contains("NUMERIC(18,4)"));

        // 边界值：0 与 1e14-0.0001 合法
        let (_, writes) = parse_adjust(&req("2026-12-01", vec![item("ZERO", Some("0"))])).expect("0 合法");
        assert_eq!(writes[0].amount, Some(Decimal::ZERO));
        let (_, writes) =
            parse_adjust(&req("2026-12-01", vec![item("BIG", Some("99999999999999.9999"))])).expect("上限内合法");
        assert_eq!(writes[0].amount, Some(money("99999999999999.9999")));
    }

    /// `effective_to` 早于 `effective_from` → 400（库 ck 之前先给人话）。
    #[test]
    fn effective_to_cannot_precede_effective_from() {
        let mut row = item("BASE", Some("1"));
        row.effective_to = Some("2026-11-30".to_string());
        let error = parse_adjust(&req("2026-12-01", vec![row])).expect_err("倒区间");
        assert!(error.msg.contains("不能早于"));
    }

    /// 取消不许带 `effective_to`（语义是「自某日起不再有」，没有截止日）。
    #[test]
    fn cancel_rejects_effective_to() {
        let mut row = item("MEAL", None);
        row.effective_to = Some("2026-12-31".to_string());
        let error = parse_adjust(&req("2026-12-01", vec![row])).expect_err("取消带区间");
        assert!(error.msg.contains("不能给 effective_to"));
    }

    /// 备注限长 / 空白备注归一成 `None`。
    #[test]
    fn remark_is_trimmed_and_bounded() {
        let mut row = item("BASE", Some("1"));
        row.remark = Some("    ".to_string());
        let (_, writes) = parse_adjust(&req("2026-12-01", vec![row])).expect("空白备注");
        assert!(writes[0].remark.is_none());

        let mut row = item("BASE", Some("1"));
        row.remark = Some("x".repeat(MAX_REMARK + 1));
        let error = parse_adjust(&req("2026-12-01", vec![row])).expect_err("超长");
        assert!(error.msg.contains("items[0].remark"));
    }

    /// 回执：`current` 只看业务日落在区间内的行，且 `items` 保序（`effective_from DESC`）。
    #[test]
    fn responses_split_current_from_history() {
        let input = |from: &str, to: Option<&str>, amount: &str| SalaryInput {
            item_code: "BASE".to_string(),
            item_name: Some("基本工资".to_string()),
            amount: money(amount),
            effective_from: day(from),
            effective_to: to.map(day),
            remark: None,
        };
        let as_of = day("2026-11-12");
        let list = SalaryListRes::build(
            "emp-1".to_string(),
            "EMP-0001".to_string(),
            "张三".to_string(),
            vec![
                input("2026-07-01", None, "6000"),
                input("2026-01-01", Some("2026-06-30"), "5602.10"),
            ],
            as_of,
        );
        assert_eq!(list.items.len(), 2, "历史与现行都返回");
        assert_eq!(list.current.len(), 1, "只有一条现行");
        assert_eq!(list.current[0].amount, money("6000"));
        assert_eq!(list.as_of, as_of);

        let adjust = SalaryAdjustRes::build(
            "emp-1".to_string(),
            "EMP-0001".to_string(),
            "张三".to_string(),
            day("2026-12-01"),
            as_of,
            vec![input("2026-12-01", None, "6800")],
            vec![input("2026-07-01", Some("2026-11-30"), "6000")],
            vec!["MEAL".to_string()],
            vec![input("2026-12-01", None, "6800")],
        );
        assert_eq!(adjust.written.len(), 1);
        assert_eq!(adjust.closed.len(), 1);
        assert_eq!(adjust.closed[0].effective_to, Some(day("2026-11-30")));
        assert_eq!(adjust.canceled, vec!["MEAL".to_string()]);
        assert_eq!(adjust.items.len(), 1);
        assert_eq!(adjust.effective_from, day("2026-12-01"));
    }
}
