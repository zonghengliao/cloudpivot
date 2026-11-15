//! HR 真库夹具卫生（`#[cfg(test)]`）：把**薪资引擎的输入集**统一清成「空库口径」。
//!
//! # 为什么需要它（卡 `t_9a4e5c2a`）
//!
//! 薪资引擎的取数是**全量读**：`salary_compute::calc_with_items` 会把库里
//! `hire_date <= 期末 AND (leave_date IS NULL OR leave_date >= 期初)` 的**所有**未删员工
//! 全部算进一张工资单（设计 §4.8.4：一期整月计入）。于是任何一个 HR 真库用例留下的员工，
//! 都会出现在**后面每一个**薪资用例的工资单里：
//!
//! ```text
//! salary_compute_db_tests 跑完留下 3 个 EMP-000x（由 employee_seed 从别的用例的残留账号种出来的）
//!   ⇒ salary_sheet / salary_override 的「两名员工」断言变 5 人；
//!   ⇒ 更糟：旧员工身上还挂着 `biz_employee_salary` 标准，却没有对应薪资项，
//!      引擎当场报「应发合计（公式项 GROSS 与按项类型聚合）不一致」——400/10001 建单直接失败。
//! ```
//!
//! 单独跑某一模块全绿、整模块一起跑 25 条红（HR-48 证据
//! `docs/evidence/2026-11-13-hr48-full-regression/07_hr_salary_compute_isolated_ok.log`），
//! 就是这个原因。**不是业务缺陷**，是夹具没有统一清场（HR-17 卡已上报过）。
//!
//! # 为什么是「清空输入集」而不是「各清各的前缀」
//!
//! 各模块的 `sweep()` 已经按自己的前缀（`HR34%` / `HR36%` / `HR37%` …）清过自己那一份，
//! 但引擎**不认前缀**：它读的是全表。所以按前缀清场永远清不出「引擎眼里的空库」——
//! 上游模块只要留一行（panic 中断、或像 `employee_seed` 那样从残留账号种出员工），
//! 下游模块的断言必然漂。这里提供**一把统一的清场**：
//!
//! ```text
//! 工资单（连带明细 / 明细项）、考勤记录、考勤期间、合同、请假单、
//! 员工薪资标准、非种子薪资项、员工，最后收掉这些实体留下的孤儿状态日志。
//! ```
//!
//! 清完之后引擎的输入集既空又可预期，任何模块的「本期间只有我这几个人」断言才成立。
//!
//! # 安全闸（别在非演练库上跑）
//!
//! 清场是**破坏性**的：它会删掉库里所有员工。所以 [`reset_engine_inputs`] 先看
//! `current_database()`，库名不含 `drill` / `test` 直接 panic（不是跳过）——
//! 这几个模块本来就只许在演练库上跑（各文件头注释里的 `HR_DB_URL=…/hr37_drill`）。
//! 宁可红，也不许把开发库 / 生产库的人事档案清空。
//!
//! # 并发口径
//!
//! [`ENGINE_LOCK`] 是「薪资引擎输入集」的总锁（同一批全局行 = 同一把锁）。
//! 拿着它的用例独占员工 / 考勤 / 工资单 / 薪资项。按模块顺序串跑（`--test-threads=1`）
//! 时它不产生额外等待；默认 4 线程下它至少保证**薪资类模块之间**不互踩。
//! 清场后会复查 `biz_employee` 必须为 0 行：别的模块在清场窗口里插了行就当场点名
//! 「并发夹具互踩」，而不是让下游用例红在莫名其妙的断言上。

#![cfg(test)]

use nik::prelude::*;

/// 薪资项**种子行**的标记（`V0396__hr_salary_items_seed` 逐字写入 `remark`）。
///
/// 清场只删「不是种子」的薪资项：种子行是出厂配置（17 项，`GROSS` / `NET` 的公式逐项
/// 枚举它们），删了会让后续所有薪资用例红在公式上。判据用迁移里的字面量而不是
/// 「`item_code` 属于某张手抄清单」——种子增删时只需改这一处常量。
pub(crate) const SEED_ITEM_REMARK: &str = "ARCH-9-2 薪资项种子（V0396）";

/// 薪资引擎输入集的总锁（同一批全局行 = 同一把锁）。
///
/// 用例把它取进 `Fixture` 里一直持有到用例结束（`static` 上的 `MutexGuard` 生命周期是
/// `'static`，可以直接存字段）。只加在**写引擎输入**的模块上，不嵌套第二把锁。
pub(crate) static ENGINE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 夹具会碰到的五类实体 → 单据表（收孤儿状态日志用，口径同
/// `business/erp/fixture_hygiene.rs`：`biz_status_log` 没有外键，只能按「引用是否还活着」判）。
const ENTITY_TABLES: &[(&str, &str)] = &[
    ("SALARY_SHEET", "biz_salary_sheet"),
    ("EMPLOYEE", "biz_employee"),
    ("EMPLOYEE_CONTRACT", "biz_employee_contract"),
    ("LEAVE_REQUEST", "biz_leave_request"),
    ("ATTENDANCE_PERIOD", "biz_attendance_period"),
];

/// 一次清场删掉的行数（打证据用：`--nocapture` 时能证明这批用例真的清过场）。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ResetReport {
    pub salary_sheets: i64,
    pub attendance_records: i64,
    pub attendance_periods: i64,
    pub contracts: i64,
    pub leave_requests: i64,
    pub employee_salary: i64,
    pub fixture_items: i64,
    pub employees: i64,
    pub orphan_logs: i64,
}

impl ResetReport {
    /// 一行证据（只在 `--nocapture` 时可见，不影响用例结果）。
    pub(crate) fn describe(&self) -> String {
        format!(
            "[HR 夹具卫生] 清场：工资单 {}/考勤记录 {}/考勤期间 {}/合同 {}/请假 {}/薪资标准 {}/\
             非种子薪资项 {}/员工 {}/孤儿状态日志 {}",
            self.salary_sheets,
            self.attendance_records,
            self.attendance_periods,
            self.contracts,
            self.leave_requests,
            self.employee_salary,
            self.fixture_items,
            self.employees,
            self.orphan_logs
        )
    }
}

async fn count(sql: &str) -> i64 {
    db().query_first_sql(sql.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("[HR 夹具卫生] 计数失败（{sql}）：{err}"))
        .and_then(|row| row.get_typed::<i64>("n").ok())
        .unwrap_or(0)
}

/// 执行一条清场语句；行数取 `DELETE n` 的返回值（失败直接 panic，绝不静默吞错）。
async fn purge(label: &str, sql: &str) -> i64 {
    let deleted = db()
        .exec_sql(sql.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("[HR 夹具卫生] 清场失败（{label}）：{err}\n  SQL: {sql}"));
    i64::try_from(deleted).unwrap_or(0)
}

/// 当前库名（只用来判「是不是演练库」，日志里也不带口令）。
pub(crate) async fn database_name() -> String {
    db().query_first_sql("SELECT current_database() AS n".to_string(), vec![])
        .await
        .expect("[HR 夹具卫生] 查 current_database() 失败")
        .and_then(|row| row.get_typed::<String>("n").ok())
        .unwrap_or_else(|| panic!("[HR 夹具卫生] current_database() 没有返回库名"))
}

/// 断言：当前连接是演练 / 测试库（破坏性清场的前置条件）。
///
/// 不满足就 panic，不是跳过：清场会删掉全部员工，跑错库的代价不可逆，
/// 必须让人一眼看到「你连错库了」，而不是让用例静默变绿。
pub(crate) async fn assert_scratch_db() -> String {
    let name = database_name().await;
    assert!(
        name.contains("drill") || name.contains("test"),
        "[HR 夹具卫生] 拒绝在库 `{name}` 上清场：薪资引擎取数是全量读，本套夹具会删除库里\
         所有员工 / 考勤 / 工资单。请把 HR_DB_URL / ERP_DB_URL 指到演练库（库名含 `drill` 或 `test`，\
         例如 `postgres://…@127.0.0.1:5432/hr37_drill`）"
    );
    name
}

/// 把薪资引擎的输入集清成「空库口径」，返回删掉的行数。
///
/// 顺序按外键从叶子到根（外键实测：`biz_attendance_record` / `biz_employee_salary` /
/// `biz_leave_request` / `biz_employee_contract` / `biz_salary_detail` → `biz_employee` 是
/// RESTRICT；`biz_salary_detail` → `biz_salary_sheet` 是 CASCADE；`biz_salary_sheet` →
/// `biz_attendance_period` 是 RESTRICT，所以工资单必须先删）。
pub(crate) async fn reset_engine_inputs() -> ResetReport {
    let database = assert_scratch_db().await;

    let mut report = ResetReport {
        // 单头删掉后明细 / 明细项随 CASCADE 一起走（`biz_salary_detail_sheet_id_fkey`）。
        salary_sheets: purge("工资单（级联明细 / 明细项）", "DELETE FROM biz_salary_sheet").await,
        attendance_records: purge("考勤记录", "DELETE FROM biz_attendance_record").await,
        // 工资单先没了，期间才删得掉（RESTRICT）。
        attendance_periods: purge("考勤期间", "DELETE FROM biz_attendance_period").await,
        contracts: purge("劳动合同", "DELETE FROM biz_employee_contract").await,
        leave_requests: purge("请假单", "DELETE FROM biz_leave_request").await,
        employee_salary: purge("员工薪资标准", "DELETE FROM biz_employee_salary").await,
        fixture_items: purge(
            "非种子薪资项",
            &format!(
                "DELETE FROM biz_salary_item WHERE remark IS DISTINCT FROM '{}'",
                SEED_ITEM_REMARK
            ),
        )
        .await,
        employees: purge("员工", "DELETE FROM biz_employee").await,
        orphan_logs: 0,
    };

    // 状态日志没有外键：删完业务行后按「引用已失效」收孤儿（否则每轮攒一批，
    // 永远收不走，还会打红别的模块的「整库无孤儿」断言）。
    for (entity_type, table) in ENTITY_TABLES {
        let sql = format!(
            "DELETE FROM biz_status_log WHERE entity_type = '{entity_type}' \
             AND entity_id NOT IN (SELECT id FROM {table})"
        );
        report.orphan_logs += purge(&format!("{entity_type} 孤儿状态日志"), &sql).await;
    }

    // 复查（不信 DELETE 的返回值）：引擎眼里的输入集必须真空。
    let employees_left = count("SELECT count(*) AS n FROM biz_employee").await;
    let foreign_items = count(&format!(
        "SELECT count(*) AS n FROM biz_salary_item WHERE remark IS DISTINCT FROM '{SEED_ITEM_REMARK}'"
    ))
    .await;
    assert_eq!(
        (employees_left, foreign_items),
        (0, 0),
        "[HR 夹具卫生] 清场后仍有残留（库 `{database}`）：员工 {employees_left} 行 / 非种子薪资项 \
         {foreign_items} 行 ⇒ 多半是**并发夹具互踩**（别的模块正在插员工 / 薪资项）。\
         本套用例要求 `--test-threads=1`；同进程并发时请让写员工的模块共用 \
         `hr::fixture_hygiene::ENGINE_LOCK`。{}",
        report.describe()
    );

    println!("{}（库 {database}）", report.describe());
    report
}

/// 一行拿到「独占薪资引擎输入集」：加锁 + 清场，返回的 guard 由用例持有到结束。
///
/// 各模块的 `setup()` / `sweep()` 调它即可：
///
/// ```ignore
/// let Some(fixture) = setup().await else { return };   // setup 内部：let engine = hygiene::exclusive().await;
/// ```
///
/// 清场挂在**每个用例的 setup** 上（不是尾部）：用例 panic 时尾部那行不执行，
/// 但下一个用例的 setup 会把残留一并清掉 —— 这就是「清场挂在壳上」在本套用例里的落地
/// （同 `business/erp/fixture_hygiene.rs` 的口径，见技能 §3）。
pub(crate) async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = ENGINE_LOCK.lock().await;
    let _ = reset_engine_inputs().await;
    guard
}
