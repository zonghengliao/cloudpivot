//! HR 模块数据访问层（骨架 · HR-09）。
//!
//! 本文件只落两件事：**10 张表的名单**（[`HR_TABLES`]）与**建表体检**（[`table_presence`]）。
//! 员工 / 合同 / 考勤 / 请假 / 薪资的数据访问按卡拆到同级文件
//! （`employee_model.rs` / `attendance_model.rs` / `leave_model.rs` / `salary_*_model.rs`），
//! 与 `business/erp/` 的组织方式一致 —— 一个「10 张表 46 个接口」的模块把所有 SQL 挤进
//! 单个 `model.rs`，到第 5 张卡之后就是不可 review 的巨文件。
//!
//! # 10 张表（`V0383__hr_tables`，DDL 已在真库跑通，见设计 §3.1 / §7.5）
//!
//! | # | 表 | 职责 | 首个落地卡 |
//! |:-:|---|---|---|
//! | ① | `biz_employee` | 员工主档（含身份证 / 工资卡，出参脱敏） | HR-10 |
//! | ② | `biz_employee_contract` | 劳动合同台账（1:N，链式续签 `renewed_from_id`） | HR-15 |
//! | ③ | `biz_employee_salary` | 员工薪资标准（行版本台账，`effective_from/to` 不重叠） | HR-16 |
//! | ④ | `biz_attendance_period` | 考勤月度批次（锁定型作业批次） | HR-19 |
//! | ⑤ | `biz_attendance_record` | 考勤台账（一人一期间一行，天数 + 加班小时） | HR-20 |
//! | ⑥ | `biz_leave_request` | 请假单（`hr_leave` 流程的业务单据） | HR-24 |
//! | ⑦ | `biz_salary_item` | 薪资项配置（公式 + 科目，`expr_snapshot` 保历史） | HR-31 |
//! | ⑧ | `biz_salary_sheet` | 薪资单头（一公司一期间一张，`uk_biz_salary_sheet_period`） | HR-36 |
//! | ⑨ | `biz_salary_detail` | 薪资明细（一人一行汇总，汇总恒等式的 CHECK 在这一层） | HR-36 |
//! | ⑩ | `biz_salary_detail_item` | 薪资明细项（一人一行 × 每薪资项，公式解释层） | HR-36 |
//!
//! 名单与 DDL 的一致性由 `table_list_matches_v0383_ddl` 单测**直接对拍迁移文件**兜底：
//! 少写一张表（或写错表名）会让体检、巡检、后续各卡的 SQL 全部跟着错，这类错在
//! 运行时表现为「接口 500 / 巡检 0 行」而不是编译错误，必须用测试钉住。
//!
//! # 时间 / 金额口径
//!
//! - 日期 `DATE`（`hire_date` / `period_start` …）→ `chrono::NaiveDate`；时间戳 `TIMESTAMPTZ`
//!   → `DateTime<Utc>`，出参按 ISO8601 `+08:00` 序列化（设计 §5.1）。
//! - 金额 / 天数 / 小时一律 `NUMERIC` → `rust_decimal::Decimal` → 出参字符串
//!   （`"9954.03"`，绝不下发 `f64`：JS 浮点会让「工资合计 = 明细之和」在界面上差一分）。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 员工主档 `biz_employee`（HR-10 / HR-11 / HR-12 / HR-17）。
pub const TABLE_EMPLOYEE: &str = "biz_employee";

/// 劳动合同台账 `biz_employee_contract`（HR-15）。
pub const TABLE_EMPLOYEE_CONTRACT: &str = "biz_employee_contract";

/// 员工薪资标准 `biz_employee_salary`（HR-16）。
pub const TABLE_EMPLOYEE_SALARY: &str = "biz_employee_salary";

/// 考勤月度批次 `biz_attendance_period`（HR-19 / HR-23）。
pub const TABLE_ATTENDANCE_PERIOD: &str = "biz_attendance_period";

/// 考勤台账 `biz_attendance_record`（HR-20 / HR-21 / HR-22）。
pub const TABLE_ATTENDANCE_RECORD: &str = "biz_attendance_record";

/// 请假单 `biz_leave_request`（HR-24 / HR-25 / HR-27）。
pub const TABLE_LEAVE_REQUEST: &str = "biz_leave_request";

/// 薪资项配置 `biz_salary_item`（HR-31 / HR-32 / HR-35）。
pub const TABLE_SALARY_ITEM: &str = "biz_salary_item";

/// 薪资单头 `biz_salary_sheet`（HR-36 / HR-39 / HR-40）。
pub const TABLE_SALARY_SHEET: &str = "biz_salary_sheet";

/// 薪资明细 `biz_salary_detail`（HR-36 / HR-37）。
pub const TABLE_SALARY_DETAIL: &str = "biz_salary_detail";

/// 薪资明细项 `biz_salary_detail_item`（HR-34 / HR-36 / HR-42）。
pub const TABLE_SALARY_DETAIL_ITEM: &str = "biz_salary_detail_item";

/// 本模块的 10 张表，**顺序与建表迁移 §3.1 的编号一致**（①~⑩）。
///
/// 顺序固定成「主档 → 台账 → 单据 → 薪资三层」而不是字典序，是为了让
/// [`table_presence`] 的体检报告、巡检脚本的清单、以及后续卡新建外键时的检索顺序都能
/// 一眼对上设计文档的编号。
pub const HR_TABLES: [&str; 10] = [
    TABLE_EMPLOYEE,
    TABLE_EMPLOYEE_CONTRACT,
    TABLE_EMPLOYEE_SALARY,
    TABLE_ATTENDANCE_PERIOD,
    TABLE_ATTENDANCE_RECORD,
    TABLE_LEAVE_REQUEST,
    TABLE_SALARY_ITEM,
    TABLE_SALARY_SHEET,
    TABLE_SALARY_DETAIL,
    TABLE_SALARY_DETAIL_ITEM,
];

/// 组织 id 哨兵（多组织未落地前的常量值）。
///
/// `V0383` 的 10 张表是 `org_id UUID NOT NULL` 且**没有 DEFAULT**（与
/// `biz_cost_period.org_id` 的 `DEFAULT '0000…'` 不同），所以每条 `INSERT` 都必须**显式**
/// 带 `org_id`。多组织（ARCH-9-1）落地后这里换成令牌里的真实组织 id，调用方（各卡的
/// `INSERT`）不用改 —— 这就是把它做成常量的唯一理由：让「以后换一处」成立。
///
/// 设计 §2.4 的原话是「若 HR 先上线（不推荐），只需把 `org_id` 换成常量列」。
pub const SENTINEL_ORG_ID: Uuid = Uuid::nil();

/// 生成「10 张表在库里是否存在」的 SQL（一次往返，`$1..$n` 逐个绑定表名）。
///
/// 用 `to_regclass` 而不是 `SELECT … FROM <表>`：表不存在时前者返回 NULL，
/// 后者抛 `42P01` 让整个探针 500 —— 而探针的**用途恰恰就是**告诉运维「迁移没跑」
/// （同 [`crate::business::erp::cost_labor_model::employee_table_present`] 的取舍）。
/// 表名走参数绑定（`$n::text`）+ `format('public.%I', …)` 转义，不拼字符串。
pub fn presence_sql(table_count: usize) -> String {
    let values = (1..=table_count)
        .map(|index| format!("(${index}::text)"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT t.name AS table_name, \
                (to_regclass(format('public.%I', t.name)) IS NOT NULL)::text AS present \
           FROM (VALUES {values}) AS t(name)"
    )
}

/// 逐张检查 [`HR_TABLES`] 是否已建（顺序与 [`HR_TABLES`] 一致）。
///
/// # 返回值
///
/// `Vec<(表名, 是否存在)>`，长度恒为 `HR_TABLES.len()`；SQL 没回的行按「不存在」处理
/// （体检宁可误报「缺表」也不能误报「就绪」）。
///
/// # 异常情况
///
/// 只有数据库连接 / 语法级失败会上抛 `DbError`；「表不存在」不是错误。
pub async fn table_presence() -> DbResult<Vec<(&'static str, bool)>> {
    let params = HR_TABLES
        .iter()
        .map(|name| Value::Text((*name).to_string()))
        .collect::<Vec<_>>();
    let rows = db().query_sql(presence_sql(HR_TABLES.len()), params).await?;

    let mut present_by_name = std::collections::HashMap::with_capacity(HR_TABLES.len());
    for row in &rows {
        let name: String = row.get_typed("table_name")?;
        // `present` 走 `::text` 再比较，不依赖驱动对 bool 的映射（与 cost 模块同口径）
        let present: String = row.get_typed("present")?;
        present_by_name.insert(name, present == "true");
    }

    Ok(HR_TABLES
        .iter()
        .map(|name| (*name, present_by_name.get(*name).copied().unwrap_or(false)))
        .collect())
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：名单与 DDL 一旦分叉，这里先红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    /// 从迁移文件里抽出所有 `CREATE TABLE IF NOT EXISTS biz_*` 的表名。
    fn ddl_tables() -> Vec<String> {
        let pattern = regex::Regex::new(r"CREATE TABLE IF NOT EXISTS\s+(biz_[A-Za-z0-9_]+)").expect("正则字面量合法");
        pattern
            .captures_iter(V0383_DDL)
            .map(|capture| capture[1].to_string())
            .collect()
    }

    /// 名单必须与 `V0383` 的 DDL **完全等价**（既不能少写，也不能多写）。
    ///
    /// 多写的后果：体检报「缺表」→ 运维以为迁移没跑完，实际是名单里有个不存在的表名；
    /// 少写的后果：那张表坏了也没人体检得到，直到某个接口 500。
    #[test]
    fn table_list_matches_v0383_ddl() {
        let mut from_ddl = ddl_tables();
        from_ddl.sort();
        let mut from_const = HR_TABLES.map(|name| name.to_string()).to_vec();
        from_const.sort();
        assert_eq!(from_ddl.len(), 10, "V0383 应建 10 张表，实际 {}", from_ddl.len());
        assert_eq!(from_const, from_ddl, "HR_TABLES 与 V0383 DDL 不一致");
    }

    /// 10 个常量名与 [`HR_TABLES`] 是同一批（防止「加了常量忘了进名单」）。
    #[test]
    fn constants_match_the_list() {
        let mut constants = vec![
            TABLE_EMPLOYEE,
            TABLE_EMPLOYEE_CONTRACT,
            TABLE_EMPLOYEE_SALARY,
            TABLE_ATTENDANCE_PERIOD,
            TABLE_ATTENDANCE_RECORD,
            TABLE_LEAVE_REQUEST,
            TABLE_SALARY_ITEM,
            TABLE_SALARY_SHEET,
            TABLE_SALARY_DETAIL,
            TABLE_SALARY_DETAIL_ITEM,
        ];
        constants.sort_unstable();
        let mut listed = HR_TABLES.to_vec();
        listed.sort_unstable();
        assert_eq!(constants, listed);
    }

    /// 名单无重复、全部 `biz_` 前缀、且按设计编号（①~⑩）的固定顺序排列。
    ///
    /// 顺序被钉住是为了让体检报告与设计 §3.1 的编号能逐行对上 —— 有人往中间插一行，
    /// 报告就从「第 ④ 行缺表」变成「第 ⑤ 行」，排障时按编号找会找错。
    #[test]
    fn list_is_unique_prefixed_and_ordered() {
        let mut sorted = HR_TABLES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), HR_TABLES.len(), "HR_TABLES 有重复表名");
        assert!(HR_TABLES.iter().all(|name| name.starts_with("biz_")));
        assert_eq!(HR_TABLES[0], "biz_employee");
        assert_eq!(HR_TABLES[9], "biz_salary_detail_item");
        assert_eq!(HR_TABLES[6], "biz_salary_item");
    }

    /// 体检 SQL：每个表名一个占位符（不拼字符串）、不越界、用 `to_regclass` 判存在。
    #[test]
    fn presence_sql_binds_one_placeholder_per_table() {
        let sql = presence_sql(HR_TABLES.len());
        for index in 1..=HR_TABLES.len() {
            assert!(sql.contains(&format!("(${index}::text)")), "缺 ${index} 占位符：{sql}");
        }
        assert!(!sql.contains("$11"), "占位符越界（10 张表却有 $11）：{sql}");
        assert!(sql.contains("to_regclass"), "必须用 to_regclass 判存在：{sql}");
        assert!(sql.contains("format('public.%I'"), "表名必须走标识符转义：{sql}");
        // 表名不能直接出现在 SQL 文本里（那是字符串拼接，遇到奇怪的库名/表名会撞）
        for table in HR_TABLES {
            assert!(!sql.contains(table), "SQL 文本里不应出现字面表名 {table}：{sql}");
        }
    }

    /// 哨兵组织 id 是全零 —— 与 `biz_cost_period.org_id` 的 DEFAULT 同值，多组织落地前
    /// 所有 HR 行的 `org_id` 都是它，唯一键 `(org_id, …)` 才不会因为「每次随机一个 uuid」
    /// 而失效（那会让工号重复检测彻底失灵）。
    #[test]
    fn sentinel_org_id_is_nil() {
        assert_eq!(SENTINEL_ORG_ID, Uuid::nil());
        assert_eq!(SENTINEL_ORG_ID.to_string(), "00000000-0000-0000-0000-000000000000");
    }
}
