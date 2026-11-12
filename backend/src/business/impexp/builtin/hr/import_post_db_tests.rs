//! 考勤导入**后处理**（`hr.attendance` · HR-22）的**真库**用例。
//!
//! 单测证明的是 SQL 形状与文案；证明不了下面这几件事，只有真库能答：
//!
//! 1. **验收①「校验失败整批拒绝」**：无薪假 > 已批准的无薪请假单天数时，该行**摘出**
//!    可入库集合、计入 `error`（`STRICT` 模式据此一行都不落库），库里 0 行；
//! 2. **验收②「错误报告含行号 + 原因」**：`RowIssue` 的 `row_no` 是 Excel 真实行号
//!    （表头占第 1 行）、`column_key = leave_unpaid_days`、文案给出「填了多少 / 批了多少 /
//!    多出多少」（原样进 `sys_import_task_error`，错误明细页与错误行 xlsx 直接可见）；
//! 3. **验收③「汇总重算正确」**：`persist` 在同一事务里把 `employee_count` /
//!    `should_days_total` / `actual_days_total` / `ot_hours_total` 全量覆盖；重导（upsert）
//!    后汇总跟着变；一份文件跨两个月时**每个期间各自重算**；
//! 4. **口径**：只有 `APPROVED` + `paid = 0` 的假进基线（`DRAFT` 未批、带薪假都不算）；
//! 5. **开关**：`hr.attendance.require_leave_match = FALSE` 时放过校验，但汇总照做。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr22_drill \
//!   ERP_DB_URL=同上一行 \
//!   cargo test --lib business::impexp::builtin::hr -- --test-threads=1 --nocapture
//! ```
//!
//! 夹具纪律：期间码用 2400+ 年份（不撞 `uk_biz_att_period`），工号 `HR22IM-` 前缀，
//! 清场按工号删（记录 → 假单 → 期间 → 员工，`employee_id` 是 `RESTRICT` 不能反）。

use std::collections::HashMap;
use std::sync::Arc;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::impexp::builtin::hr::import_post::CONFIG_REQUIRE_LEAVE_MATCH;
use crate::business::impexp::builtin::hr::{ATTENDANCE_BIZ_TYPE, AttendanceImport};
use crate::business::impexp::definition::{DuplicateStrategy, ImportColumn, ImportCtx, ImportDefinition, ImportMode};
use crate::business::impexp::validate::{self, DbFkResolver, ValidationOutcome, global_dicts};

/// 真库用例串行（共用进程级 `db()`）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 初始化进程级数据库连接；`false` = 没配库地址 / 连到的库不是它指定的那一个。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("HR_DB_URL").or_else(|_| std::env::var("ERP_DB_URL")) else {
                println!("SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）");
                return false;
            };
            let wanted = url
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            if try_db().is_none() {
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => {
                        if let Err(error) = set_db(connection) {
                            println!("SKIP: set_db 失败（{error}）");
                            return false;
                        }
                    }
                    Err(error) => {
                        println!("SKIP: 库连接失败（{error}）");
                        return false;
                    }
                }
            }
            match db()
                .scalar_with_params::<String>("SELECT current_database()", vec![])
                .await
            {
                Ok(name) if name == wanted => {
                    println!("HR-22 真库用例：已连接 {name}");
                    true
                }
                Ok(name) => {
                    println!("SKIP: 全局连接池指向 {name}，与指定的 {wanted} 不一致");
                    false
                }
                Err(error) => {
                    println!("SKIP: 读 current_database 失败（{error}）");
                    false
                }
            }
        })
        .await
}

/// 拿串行锁 + 连库；没库返回 `None`（用例退化为 SKIP）。
async fn guard() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let lock = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    Some(lock)
}

/// 夹具：一个考勤期间 + 两名员工（工号 `HR22IM-*`，无请假单）。
struct Fixture {
    /// 主考勤月（`yyyy-MM`，年份 24xx）。
    period_code: String,
    /// 主期间主键。
    period_id: Uuid,
    /// 跨月用例额外建的期间（`(code, id)`）。
    extra_periods: Vec<(String, Uuid)>,
    /// 员工甲。
    emp_a: String,
    /// 员工乙。
    emp_b: String,
}

impl Fixture {
    /// 期间码：当前纳秒派生，2400+ 年份 / 01–12 月份（同进程连跑不撞唯一键）。
    fn next_period_code() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.subsec_nanos())
            .unwrap_or(0);
        let year = 2400 + (nanos % 50);
        let month = 1 + (nanos / 50) % 12;
        format!("{year}-{month:02}")
    }

    /// 造夹具（期间 + 两名员工），**不造请假单**（每个用例自己按需加）。
    async fn seed() -> Self {
        let period_code = Self::next_period_code();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let emp_a = format!("HR22IM-A{stamp}");
        let emp_b = format!("HR22IM-B{stamp}");
        let period_id = Self::insert_period(&period_code).await;
        for (emp_no, name) in [(&emp_a, "张三"), (&emp_b, "李四")] {
            db().execute_with_params(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status) \
                 VALUES (gen_random_uuid(), ?::uuid, ?, ?, ?::uuid, ?::text::date, 'ACTIVE')",
                vec![
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(emp_no.clone()),
                    Value::from(name.to_string()),
                    Value::from(Uuid::now_v7()),
                    Value::from("2024-01-01".to_string()),
                ],
            )
            .await
            .expect("插入员工");
        }
        Self {
            period_code,
            period_id,
            extra_periods: Vec::new(),
            emp_a,
            emp_b,
        }
    }

    /// 建一个期间（`period_start` = 01、`period_end` = 28）。
    async fn insert_period(period_code: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_attendance_period \
                 (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')",
            vec![
                Value::from(id),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(period_code.to_string()),
                Value::from(format!("{}-01", &period_code[..7])),
                Value::from(format!("{}-28", &period_code[..7])),
            ],
        )
        .await
        .expect("插入考勤期间");
        id
    }

    /// 再建一个期间（跨月用例），返回期间码。
    ///
    /// 月份取主期间的下一个月（12 月 → 次年 1 月），年份同样落在 24xx 段：
    /// `period_code` 有 `^[0-9]{4}-(0[1-9]|1[0-2])$` 的 CHECK，不能编 `-extra-` 这种后缀。
    async fn add_period(&mut self) -> String {
        let (year, month) = self.period_code.split_at(4);
        let month_no = month.trim_start_matches('-').parse::<u32>().unwrap_or(1);
        let (year, month_no) = if month_no >= 12 {
            (year.parse::<u32>().unwrap_or(2400) + 1, 1)
        } else {
            (year.parse::<u32>().unwrap_or(2400), month_no + 1)
        };
        let code = format!("{year}-{month_no:02}");
        let id = Self::insert_period(&code).await;
        self.extra_periods.push((code.clone(), id));
        code
    }

    /// 加一张请假单（`paid = 0` 无薪 / `1` 带薪；`status` 直接写，不走状态机）。
    async fn add_leave(&self, emp_no: &str, days: &str, paid: i32, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        // 单号取 uuid 的**尾部** 12 位：v7 的前 12 位是毫秒时间戳，同一毫秒内两次
        // `now_v7()` 前缀相同 → 用前缀拼单号会撞 `uk_biz_leave_no`（实测踩过）。
        let simple = id.simple().to_string();
        let leave_no = format!("LV22-{}", &simple[simple.len() - 12..]);
        db().execute_with_params(
            "INSERT INTO biz_leave_request \
                 (id, org_id, leave_no, employee_id, employee_no, employee_name, leave_type, \
                  start_time, end_time, duration_days, paid, status) \
             SELECT ?::uuid, ?::uuid, ?, e.id, e.emp_no, e.name, 'PERSONAL', \
                    ?::text::date, ?::text::date, ?::text::numeric, ?::text::int, ? \
               FROM biz_employee e \
              WHERE e.org_id = ?::uuid AND e.emp_no = ? AND e.deleted_at IS NULL",
            vec![
                Value::from(id),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(leave_no),
                Value::from(format!("{}-05", &self.period_code[..7])),
                Value::from(format!("{}-06", &self.period_code[..7])),
                Value::from(days.to_string()),
                Value::from(paid.to_string()),
                Value::from(status.to_string()),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(emp_no.to_string()),
            ],
        )
        .await
        .expect("插入请假单");
        id
    }

    /// 清场（记录 → 假单 → 期间 → 员工；`employee_id` 是 `RESTRICT`，顺序不能换）。
    async fn cleanup(&self) {
        let periods = std::iter::once((self.period_code.clone(), self.period_id))
            .chain(self.extra_periods.iter().cloned())
            .collect::<Vec<_>>();
        for (_, period_id) in &periods {
            if let Err(error) = db()
                .execute_with_params(
                    "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid",
                    vec![Value::from(*period_id)],
                )
                .await
            {
                println!("清理考勤记录失败：{error}");
            }
        }
        for (sql, params) in [
            (
                "DELETE FROM biz_leave_request WHERE org_id = ?::uuid AND employee_no IN (?, ?)",
                vec![
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(self.emp_a.clone()),
                    Value::from(self.emp_b.clone()),
                ],
            ),
            (
                "DELETE FROM biz_employee WHERE emp_no = ? OR emp_no = ?",
                vec![Value::from(self.emp_a.clone()), Value::from(self.emp_b.clone())],
            ),
        ] {
            if let Err(error) = db().execute_with_params(sql, params).await {
                println!("清理失败（{sql}）：{error}");
            }
        }
        for (_, period_id) in &periods {
            if let Err(error) = db()
                .execute_with_params(
                    "DELETE FROM biz_attendance_period WHERE id = ?::uuid",
                    vec![Value::from(*period_id)],
                )
                .await
            {
                println!("清理考勤期间失败：{error}");
            }
        }
    }

    /// 甲的考勤行：应出勤 22 / 实际 20 / 带薪假 1 / 无薪假 `unpaid` / 平时加班 12.5 / 休息日 8。
    fn row_a(&self, row_no: u32, unpaid: &str) -> Vec<String> {
        vec![
            self.period_code.clone(),
            self.emp_a.clone(),
            String::new(),
            "22".to_string(),
            "20".to_string(),
            "1".to_string(),
            unpaid.to_string(),
            "0".to_string(),
            "0".to_string(),
            "12.5".to_string(),
            "8".to_string(),
            "0".to_string(),
            "2".to_string(),
            "0".to_string(),
            format!("HR-22 行 {row_no}"),
        ]
    }

    /// 乙的考勤行：应出勤 22 / 实际 22，其余 0。
    fn row_b(&self, row_no: u32) -> Vec<String> {
        vec![
            self.period_code.clone(),
            self.emp_b.clone(),
            String::new(),
            "22".to_string(),
            "22".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            format!("HR-22 行 {row_no}"),
        ]
    }
}

/// 造一个导入任务（`persist` 靠它的 `options` 决定 upsert / skip）。
async fn new_task(strategy: DuplicateStrategy, mode: ImportMode) -> Uuid {
    let id = Uuid::now_v7();
    let simple = id.simple().to_string();
    let options = serde_json::json!({ "duplicate_strategy": strategy.as_str() }).to_string();
    db().execute_with_params(
        "INSERT INTO sys_import_task \
             (id, task_no, biz_type, biz_name, file_path, file_sha256, file_name, file_size, \
              mode, status, options) \
         VALUES (?::uuid, ?, ?, '考勤记录', '/tmp/hr22im.xlsx', repeat('0', 64), 'hr22im.xlsx', 1, \
                 ?, 'VALIDATED', ?::jsonb)",
        vec![
            Value::from(id),
            Value::from(format!("HR22IM-{}", &simple[..16])),
            Value::from(ATTENDANCE_BIZ_TYPE),
            Value::from(mode.as_str()),
            Value::Json(options),
        ],
    )
    .await
    .expect("插入导入任务");
    id
}

/// 清掉本文件的导入任务行。
async fn cleanup_tasks() {
    if let Err(error) = db()
        .execute_with_params("DELETE FROM sys_import_task WHERE task_no LIKE 'HR22IM-%'", vec![])
        .await
    {
        println!("清理导入任务失败：{error}");
    }
}

/// 造 xlsx（表头取定义自身的模板表头，列顺序自动对齐）。
fn bytes_of(rows: Vec<Vec<String>>) -> Vec<u8> {
    let columns: &[ImportColumn] = AttendanceImport.columns();
    let headers = columns
        .iter()
        .map(|column| column.template_header())
        .collect::<Vec<_>>();
    let header_refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
    let maps = rows
        .into_iter()
        .map(|values| {
            let mut map = HashMap::new();
            for (index, header) in headers.iter().enumerate() {
                map.insert(header.clone(), values.get(index).cloned().unwrap_or_default());
            }
            map
        })
        .collect::<Vec<_>>();
    Export::to_xlsx_bytes(header_refs.as_slice(), &maps).expect("造 xlsx")
}

/// 跑一遍框架的完整校验（结构 + 逐格 + 外键 + 业务规则 + **批次级校验**）。
async fn precheck(rows: Vec<Vec<String>>, ctx: &ImportCtx) -> ValidationOutcome {
    let bytes = bytes_of(rows);
    validate::validate(
        &AttendanceImport,
        &bytes,
        global_dicts(),
        &DbFkResolver::new(),
        ctx,
        true,
    )
    .await
    .expect("预检不应抛错（业务错误应落在明细里）")
}

/// 台账行数。
async fn record_count(period_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_attendance_record WHERE period_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(period_id)],
    )
    .await
    .expect("统计台账")
}

/// 期间的四个汇总列：`(employee_count, should_days_total, actual_days_total, ot_hours_total)`。
async fn totals(period_id: Uuid) -> (String, String, String, String) {
    let row = db()
        .find_with_params(
            "SELECT employee_count::text AS cnt, should_days_total::text AS should_days, \
                    actual_days_total::text AS actual_days, ot_hours_total::text AS ot_hours \
               FROM biz_attendance_period WHERE id = ?::uuid",
            vec![Value::from(period_id)],
        )
        .await
        .expect("读期间汇总")
        .into_iter()
        .next()
        .expect("期间必须存在");
    (
        row.get_typed("cnt").expect("cnt"),
        row.get_typed("should_days").expect("should_days"),
        row.get_typed("actual_days").expect("actual_days"),
        row.get_typed("ot_hours").expect("ot_hours"),
    )
}

/// 把校验通过的行入库（`STRICT` 语义：只在 `error == 0` 时走）。
async fn persist(rows: &[crate::business::impexp::definition::RowValues], ctx: &ImportCtx) -> usize {
    db().transaction(|tx| async move { AttendanceImport.persist(&tx, rows, ctx).await })
        .await
        .expect("入库应成功")
}

/// 设开关（每个用例自己设，互不依赖上一个用例留在库里的值）。
async fn set_switch(enabled: bool) {
    let value = if enabled { "TRUE" } else { "FALSE" };
    let updated = db()
        .execute_with_params(
            "UPDATE sys_config SET config_value = ? WHERE config_key = ? AND deleted_at IS NULL",
            vec![
                Value::from(value.to_string()),
                Value::from(CONFIG_REQUIRE_LEAVE_MATCH.to_string()),
            ],
        )
        .await
        .expect("改开关");
    assert_eq!(updated, 1, "V0388 必须已种下 {CONFIG_REQUIRE_LEAVE_MATCH}");
}

fn ctx_of(task_id: Uuid) -> ImportCtx {
    ImportCtx {
        task_id,
        operator: None,
        mode: ImportMode::Strict,
    }
}

#[tokio::test]
async fn unpaid_over_approved_is_rejected_with_row_number_and_reason() {
    let Some(_guard) = guard().await else { return };
    set_switch(true).await;
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task);

    // 甲填 3 天无薪假，但一张已批无薪请假单都没有 → 必须被拦
    let outcome = precheck(vec![fixture.row_a(2, "3")], &ctx).await;
    assert_eq!(
        outcome.report.error, 1,
        "无薪假超批必须报 1 条：{:?}",
        outcome.report.errors
    );
    assert_eq!(outcome.report.valid, 0);
    let issue = &outcome.report.errors[0];
    assert_eq!(issue.row_no, 2, "行号必须是 Excel 真实行号（表头占第 1 行）");
    assert_eq!(issue.column_key, Some("leave_unpaid_days"));
    assert_eq!(issue.kind.as_str(), "RULE");
    assert!(issue.message.contains(&fixture.emp_a), "要点名工号：{}", issue.message);
    assert!(
        issue.message.contains(&fixture.period_code),
        "要点名考勤月：{}",
        issue.message
    );
    assert!(issue.message.contains("只有 0 天"), "要给出已批天数：{}", issue.message);
    assert!(issue.message.contains("多出 3 天"), "要给出差额：{}", issue.message);
    assert!(outcome.rows.is_empty(), "STRICT 语义下该行不许进入可入库集合");

    // 整批拒绝：库里一行都没有、汇总仍是 0
    assert_eq!(record_count(fixture.period_id).await, 0, "被拒的批次不许落库");
    assert_eq!(
        totals(fixture.period_id).await,
        (
            "0".to_string(),
            "0.00".to_string(),
            "0.00".to_string(),
            "0.00".to_string()
        ),
        "被拒的批次不许改动汇总"
    );

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn approved_unpaid_leave_passes_and_totals_are_recalculated() {
    let Some(_guard) = guard().await else { return };
    set_switch(true).await;
    let fixture = Fixture::seed().await;
    fixture.add_leave(&fixture.emp_a, "3", 0, "APPROVED").await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task);

    // 甲无薪假 3 = 已批 3（边界相等要放过）；乙无薪假 0
    let outcome = precheck(vec![fixture.row_a(2, "3"), fixture.row_b(3)], &ctx).await;
    assert_eq!(
        outcome.report.error, 0,
        "无薪假 3 ≤ 已批 3，不许拦：{:?}",
        outcome.report.errors
    );
    let affected = persist(&outcome.rows, &ctx).await;
    assert_eq!(affected, 2);
    assert_eq!(record_count(fixture.period_id).await, 2);

    // 汇总重算：人数 2 / 应出勤 22+22 / 实际 20+22 / 加班 12.5+8
    assert_eq!(
        totals(fixture.period_id).await,
        (
            "2".to_string(),
            "44.00".to_string(),
            "42.00".to_string(),
            "20.50".to_string()
        ),
        "汇总四列必须按本批记录全量覆盖"
    );

    // 重导（upsert）：甲的实际出勤 20 → 18，汇总必须跟着变（不是脏增量）
    let mut row_a = fixture.row_a(2, "3");
    row_a[4] = "18".to_string();
    let outcome = precheck(vec![row_a, fixture.row_b(3)], &ctx).await;
    assert_eq!(outcome.report.error, 0);
    let affected = persist(&outcome.rows, &ctx).await;
    assert_eq!(affected, 2, "upsert 覆盖 2 行");
    assert_eq!(record_count(fixture.period_id).await, 2, "重导不许产生第三行");
    assert_eq!(
        totals(fixture.period_id).await,
        (
            "2".to_string(),
            "44.00".to_string(),
            "40.00".to_string(),
            "20.50".to_string()
        ),
        "重导后汇总必须重算（实际出勤 18+22）"
    );

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn only_approved_unpaid_leaves_count() {
    let Some(_guard) = guard().await else { return };
    set_switch(true).await;
    let fixture = Fixture::seed().await;
    // 一张未批准的无薪假（5 天）+ 一张已批准的带薪假（5 天）：都不该进基线
    fixture.add_leave(&fixture.emp_a, "5", 0, "DRAFT").await;
    fixture.add_leave(&fixture.emp_a, "5", 1, "APPROVED").await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task);

    let outcome = precheck(vec![fixture.row_a(2, "2")], &ctx).await;
    assert_eq!(
        outcome.report.error, 1,
        "未批 / 带薪的假不能当扣款依据：{:?}",
        outcome.report.errors
    );
    let issue = &outcome.report.errors[0];
    assert!(issue.message.contains("只有 0 天"), "{}", issue.message);
    assert_eq!(record_count(fixture.period_id).await, 0);

    // 补一张已批准的无薪假 2 天 → 同一份文件放行
    fixture.add_leave(&fixture.emp_a, "2", 0, "APPROVED").await;
    let outcome = precheck(vec![fixture.row_a(2, "2")], &ctx).await;
    assert_eq!(outcome.report.error, 0, "{:?}", outcome.report.errors);
    assert_eq!(persist(&outcome.rows, &ctx).await, 1);
    assert_eq!(record_count(fixture.period_id).await, 1);

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn switch_off_skips_the_check_but_still_recalculates() {
    let Some(_guard) = guard().await else { return };
    set_switch(false).await;
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task);

    // 开关关闭：3 天无薪假没有任何假单也放过
    let outcome = precheck(vec![fixture.row_a(2, "3")], &ctx).await;
    assert_eq!(outcome.report.error, 0, "开关关闭时不许拦：{:?}", outcome.report.errors);
    assert_eq!(persist(&outcome.rows, &ctx).await, 1);
    assert_eq!(
        totals(fixture.period_id).await,
        (
            "1".to_string(),
            "22.00".to_string(),
            "20.00".to_string(),
            "20.50".to_string()
        ),
        "开关只管校验，汇总重算照做"
    );

    set_switch(true).await;
    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn each_period_of_one_file_is_recalculated() {
    let Some(_guard) = guard().await else { return };
    set_switch(true).await;
    let mut fixture = Fixture::seed().await;
    let second = fixture.add_period().await;
    let second_id = fixture
        .extra_periods
        .iter()
        .find(|(code, _)| code == &second)
        .map(|(_, id)| *id)
        .expect("额外期间");
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task);

    // 两份文件合成的「跨月文件」：甲在主期间、乙在第二期间
    let mut row_b_second = fixture.row_b(3);
    row_b_second[0] = second.clone();
    let outcome = precheck(vec![fixture.row_a(2, "0"), row_b_second], &ctx).await;
    assert_eq!(outcome.report.error, 0, "{:?}", outcome.report.errors);
    assert_eq!(persist(&outcome.rows, &ctx).await, 2);

    // 两个期间的汇总各自重算到位（不是只算第一个 / 只算最后一个）
    assert_eq!(
        totals(fixture.period_id).await,
        (
            "1".to_string(),
            "22.00".to_string(),
            "20.00".to_string(),
            "20.50".to_string()
        ),
        "主期间汇总"
    );
    assert_eq!(
        totals(second_id).await,
        (
            "1".to_string(),
            "22.00".to_string(),
            "22.00".to_string(),
            "0.00".to_string()
        ),
        "第二期间汇总"
    );

    fixture.cleanup().await;
    cleanup_tasks().await;
}

/// 让 `Arc` 依赖保持在使用状态（与 `master_data/db_tests.rs` 同一形态，
/// 后续用例若要 `Arc<dyn ImportDefinition>` 直接可用）。
#[allow(dead_code)]
fn as_arc() -> Arc<dyn ImportDefinition> {
    Arc::new(AttendanceImport)
}
