//! 考勤导入定义（`hr.attendance`）的**真库**用例（HR-21）。
//!
//! 单测证明的是列契约、纯规则与 SQL 形状；证明不了这五件事，只有真库能答：
//!
//! 1. **验收项「注册到 impexp 框架」之后真的能走完整链路**：`validate()`（结构 + 逐格 +
//!    外键 + 业务规则）→ `persist()`（真 SQL / 真事务）→ 台账里一行不少；
//! 2. **快照取自员工档案**：文件里姓名留空，落库的 `employee_no` / `employee_name` /
//!    `dept_id` 仍是档案里的值（不是空串，也不是文件里的值）；
//! 3. **幂等**：同一份文件重导不产生第二行（`uk_biz_att_rec` 上的 upsert），数值被覆盖；
//!    `SKIP` 策略下已存在的行**一个字段都不动**；
//! 4. **锁定期间一行都进不去**：期间 `LOCKED` 后整批打回、报文带 Excel 行号、
//!    库里 **0 行**（不是"报错了但写进去了"）；
//! 5. **错行能被点名**：工号不存在（框架外键）/ 姓名与档案不符（本定义的预检）都带行号。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr21_card_drill \
//!   ERP_DB_URL=同上一行 \
//!   cargo test --lib business::impexp::builtin::hr -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! - 期间码用 **2400+ 年份**（与 `hr/*_db_tests.rs` 同口径），重跑不撞
//!   `uk_biz_att_period`，也不会污染真实账期的月份；
//! - 工号统一 `HR21IM-` 前缀，清场按 **id** 删（记录 → 期间 → 员工），不按喜欢的前缀扫全库；
//! - 员工 / 期间直接写 SQL（本卡不测员工建档，那已有 HR-10~HR-17 的用例）；
//!   但**外键顺序不能反**：记录 `employee_id` 是 `RESTRICT`。

use std::collections::HashMap;
use std::sync::Arc;

use nik::prelude::*;

use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::impexp::builtin::hr::{ATTENDANCE_BIZ_TYPE, AttendanceImport};
use crate::business::impexp::definition::{
    DuplicateStrategy, ImportColumn, ImportCtx, ImportDefinition, ImportMode, RowValues,
};
use crate::business::impexp::validate::{DbFkResolver, PrecheckReport, ValidationOutcome, global_dicts};

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
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // 关掉连接复用，避免借到上一个 runtime 的死连接。
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
                    println!("HR-21 真库用例：已连接 {name}");
                    true
                }
                Ok(name) => {
                    println!(
                        "SKIP: 全局连接池指向 {name}，与指定的 {wanted} 不一致\
                         （同进程里别的用例先初始化了连接池）；请单独跑 cargo test --lib builtin::hr"
                    );
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

/// 夹具：一个考勤期间 + 两名员工（工号 `HR21IM-*`）。
struct Fixture {
    /// 考勤月（`yyyy-MM`，年份 24xx）。
    period_code: String,
    /// 期间主键。
    period_id: uuid::Uuid,
    /// 员工甲（档案姓名「张三」）。
    emp_a: String,
    /// 员工乙（档案姓名「李四」）。
    emp_b: String,
}

impl Fixture {
    /// 期间码：用当前时间的纳秒派生一个 2400+ 的年份与 01–12 的月份。
    ///
    /// 同一进程里连跑多个用例也不会撞 `uk_biz_att_period (org_id, period_code)`。
    fn next_period_code() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.subsec_nanos())
            .unwrap_or(0);
        let year = 2400 + (nanos % 50);
        let month = 1 + (nanos / 50) % 12;
        format!("{year}-{month:02}")
    }

    /// 造夹具（期间 + 两名员工）。
    async fn seed() -> Self {
        let period_code = Self::next_period_code();
        let period_id = uuid::Uuid::now_v7();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let emp_a = format!("HR21IM-A{stamp}");
        let emp_b = format!("HR21IM-B{stamp}");
        let start = format!("{}-01", &period_code[..7]);
        let end = format!("{}-28", &period_code[..7]);
        db().execute_with_params(
            "INSERT INTO biz_attendance_period \
                 (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')",
            vec![
                Value::from(period_id),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(period_code.clone()),
                Value::from(start),
                Value::from(end),
            ],
        )
        .await
        .expect("插入考勤期间");
        for (emp_no, name) in [(&emp_a, "张三"), (&emp_b, "李四")] {
            db().execute_with_params(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status) \
                 VALUES (gen_random_uuid(), ?::uuid, ?, ?, ?::uuid, ?::text::date, 'ACTIVE')",
                vec![
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(emp_no.clone()),
                    Value::from(name.to_string()),
                    Value::from(uuid::Uuid::now_v7()),
                    Value::from("2024-01-01".to_string()),
                ],
            )
            .await
            .expect("插入员工");
        }
        Self {
            period_code,
            period_id,
            emp_a,
            emp_b,
        }
    }

    /// 清场（按 id 删：记录 → 期间 → 员工；顺序不能换，`employee_id` 是 `RESTRICT`）。
    async fn cleanup(&self) {
        for (sql, params) in [
            (
                "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid",
                vec![Value::from(self.period_id)],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid",
                vec![Value::from(self.period_id)],
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
    }

    /// 甲的完整考勤行（数值：应出勤 22 / 实际 20 / 带薪假 1 / 无薪假 1 / 平时加班 12.5）。
    fn row_a(&self, row_no: u32) -> Vec<String> {
        vec![
            self.period_code.clone(), // 考勤月
            self.emp_a.clone(),       // 工号
            String::new(),            // 姓名（留空 → 落库快照仍取档案）
            "22".to_string(),         // 应出勤
            "20".to_string(),         // 实际出勤
            "1".to_string(),          // 带薪假
            "1".to_string(),          // 无薪假
            "0".to_string(),          // 病假
            "0".to_string(),          // 旷工
            "12.5".to_string(),       // 平时加班
            "8".to_string(),          // 休息日加班
            "0".to_string(),          // 节假日加班
            "2".to_string(),          // 迟到
            "0".to_string(),          // 早退
            format!("HR-21 行 {row_no}"),
        ]
    }

    /// 乙的考勤行（应出勤 22 / 实际 22，其余 0）。
    fn row_b(&self, row_no: u32) -> Vec<String> {
        vec![
            self.period_code.clone(),
            self.emp_b.clone(),
            "李四".to_string(), // 填了姓名 → 与档案一致，允许
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
            format!("HR-21 行 {row_no}"),
        ]
    }
}

/// 造一个导入任务（`persist` 靠它的 `options` 决定 upsert / skip）。
async fn new_task(strategy: DuplicateStrategy, mode: ImportMode) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let simple = id.simple().to_string();
    let task_no = format!("HR21IM-{}", &simple[..16]);
    let options = serde_json::json!({ "duplicate_strategy": strategy.as_str() }).to_string();
    db().execute_with_params(
        "INSERT INTO sys_import_task \
             (id, task_no, biz_type, biz_name, file_path, file_sha256, file_name, file_size, \
              mode, status, options) \
         VALUES (?::uuid, ?, ?, '考勤记录', '/tmp/hr21im.xlsx', repeat('0', 64), 'hr21im.xlsx', 1, \
                 ?, 'VALIDATED', ?::jsonb)",
        vec![
            Value::from(id),
            Value::from(task_no),
            Value::from(ATTENDANCE_BIZ_TYPE),
            Value::from(mode.as_str()),
            Value::Json(options),
        ],
    )
    .await
    .expect("插入导入任务");
    id
}

/// 清掉导入任务行。
async fn cleanup_tasks() {
    if let Err(error) = db()
        .execute_with_params("DELETE FROM sys_import_task WHERE task_no LIKE 'HR21IM-%'", vec![])
        .await
    {
        println!("清理导入任务失败：{error}");
    }
}

/// 造 xlsx（表头取定义自身的模板表头，列顺序自动对齐 —— 顺带验证「模板列 = 解析列」）。
fn bytes_of(definition: &dyn ImportDefinition, rows: Vec<Vec<String>>) -> Vec<u8> {
    let columns: &[ImportColumn] = definition.columns();
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

/// 跑一遍框架的完整预检（结构 + 逐格 + 外键 + 业务规则）。
async fn precheck(bytes: &[u8], ctx: &ImportCtx) -> ValidationOutcome {
    crate::business::impexp::validate::validate(
        &AttendanceImport,
        bytes,
        global_dicts(),
        &DbFkResolver::new(),
        ctx,
        true,
    )
    .await
    .expect("预检不应抛错（错误应落在明细里）")
}

/// 「上传 → 预检 → 入库」一把过；返回（报告, 实际写入行数）。
async fn import(rows: Vec<Vec<String>>, ctx: &ImportCtx) -> (PrecheckReport, usize) {
    let bytes = bytes_of(&AttendanceImport, rows);
    let outcome = precheck(&bytes, ctx).await;
    assert_eq!(
        outcome.report.error, 0,
        "预检应零错误，实际：{:?}",
        outcome.report.errors
    );
    let affected = db()
        .transaction(|tx| async move { AttendanceImport.persist(&tx, &outcome.rows, ctx).await })
        .await
        .expect("入库应成功");
    (outcome.report, affected)
}

/// 台账里这一期的行（按工号升序）：`(工号, 姓名, 来源, 应出勤, 实际出勤, 平时加班)`。
async fn records(period_id: uuid::Uuid) -> Vec<(String, String, String, String, String, String)> {
    let rows = db()
        .find_with_params(
            "SELECT employee_no, employee_name, source, should_days::text AS should_days, \
                    actual_days::text AS actual_days, ot_hours_workday::text AS ot_hours \
               FROM biz_attendance_record \
              WHERE period_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY employee_no",
            vec![Value::from(period_id)],
        )
        .await
        .expect("读台账");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<String>("employee_no").expect("employee_no"),
                row.get_typed::<String>("employee_name").expect("employee_name"),
                row.get_typed::<String>("source").expect("source"),
                row.get_typed::<String>("should_days").expect("should_days"),
                row.get_typed::<String>("actual_days").expect("actual_days"),
                row.get_typed::<String>("ot_hours").expect("ot_hours"),
            )
        })
        .collect()
}

/// 把期间置成某个状态（`DRAFT` / `SUBMITTED` / `LOCKED`）。
async fn set_period_status(period_id: uuid::Uuid, status: &str) {
    db().execute_with_params(
        "UPDATE biz_attendance_period SET status = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid",
        vec![Value::from(status.to_string()), Value::from(period_id)],
    )
    .await
    .expect("改期间状态");
}

fn ctx_of(task_id: uuid::Uuid, mode: ImportMode) -> ImportCtx {
    ImportCtx {
        task_id,
        operator: None,
        mode,
    }
}

#[tokio::test]
async fn import_creates_one_row_per_employee_with_archive_snapshots() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    let (report, affected) = import(vec![fixture.row_a(3), fixture.row_b(4)], &ctx).await;
    assert_eq!(report.total, 2, "应识别出 2 行数据");
    assert_eq!(affected, 2, "应写入 2 行");

    let found = records(fixture.period_id).await;
    assert_eq!(found.len(), 2, "一人一月一行");
    // 甲的姓名留空 → 快照从档案取「张三」；来源固定 IMPORT
    assert_eq!(found[0].0, fixture.emp_a);
    assert_eq!(found[0].1, "张三", "姓名快照必须取自员工档案");
    assert_eq!(found[0].2, "IMPORT");
    assert_eq!(found[0].3, "22.00");
    assert_eq!(found[0].4, "20.00");
    assert_eq!(found[0].5, "12.50");
    // 乙填了姓名（与档案一致）→ 照常入库
    assert_eq!(found[1].0, fixture.emp_b);
    assert_eq!(found[1].1, "李四");
    assert_eq!(found[1].4, "22.00");

    // dept_id 也是从档案取的快照（不是 NULL）
    let dept_missing = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_attendance_record \
              WHERE period_id = ?::uuid AND deleted_at IS NULL AND dept_id IS NULL",
            vec![Value::from(fixture.period_id)],
        )
        .await
        .expect("统计 dept_id");
    assert_eq!(dept_missing, 0, "dept_id 必须从员工档案带过来");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn reimport_upserts_and_never_creates_a_second_row() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    import(vec![fixture.row_a(3), fixture.row_b(4)], &ctx).await;
    let first = records(fixture.period_id).await;

    // 第二遍：甲的实际出勤改成 18，乙的姓名改成空
    let mut row_a = fixture.row_a(3);
    row_a[4] = "18".to_string();
    let mut row_b = fixture.row_b(4);
    row_b[2] = String::new();
    let (_, affected) = import(vec![row_a, row_b], &ctx).await;
    assert_eq!(affected, 2, "upsert 应覆盖 2 行（不是新增）");

    let second = records(fixture.period_id).await;
    assert_eq!(second.len(), 2, "重导不许产生第三行");
    assert_eq!(second[0].4, "18.00", "甲的实际出勤被覆盖");
    assert_eq!(second[1].1, "李四", "姓名快照仍取档案");
    assert_eq!(first[1].1, "李四");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn skip_strategy_leaves_existing_rows_untouched() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;

    let upsert_task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    import(
        vec![fixture.row_a(3), fixture.row_b(4)],
        &ctx_of(upsert_task, ImportMode::Strict),
    )
    .await;
    let before = records(fixture.period_id).await;

    // 换 SKIP 策略 + 改数值重导 → 一行都不写，台账保持原值
    let mut row_a = fixture.row_a(3);
    row_a[4] = "5".to_string();
    let skip_task = new_task(DuplicateStrategy::Skip, ImportMode::Strict).await;
    let (_, affected) = import(vec![row_a], &ctx_of(skip_task, ImportMode::Strict)).await;
    assert_eq!(affected, 0, "SKIP 策略下已存在的行不算写入");

    let after = records(fixture.period_id).await;
    assert_eq!(after, before, "SKIP 策略不许改动任何字段");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn locked_period_rejects_the_whole_batch_and_writes_nothing() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    set_period_status(fixture.period_id, "LOCKED").await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    let bytes = bytes_of(&AttendanceImport, vec![fixture.row_a(3), fixture.row_b(4)]);
    let outcome = precheck(&bytes, &ctx).await;
    assert_eq!(outcome.report.error, 0, "逐格校验看不出「已锁定」，它属于入库预检");

    let error = db()
        .transaction(|tx| async move { AttendanceImport.persist(&tx, &outcome.rows, &ctx).await })
        .await
        .expect_err("锁定期间必须整批拒绝");
    let message = error.to_string();
    assert!(message.contains("已锁定"), "报文要点明锁定：{message}");
    // 两行都被锁定 → 两条都要带行号（表头占第 1 行，数据是第 2 / 3 行）
    assert!(message.contains("Excel 第 2 行"), "报文要带行号：{message}");
    assert!(message.contains("Excel 第 3 行"), "报文要带行号：{message}");

    let found = records(fixture.period_id).await;
    assert!(found.is_empty(), "报错之后库里必须 0 行，实际 {} 行", found.len());

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn unknown_employee_is_reported_with_row_number() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    // 单行文件：第 1 行是表头，数据从第 2 行开始 → 预期行号 2
    let mut row = fixture.row_a(2);
    row[1] = "HR21IM-NOBODY".to_string();
    let bytes = bytes_of(&AttendanceImport, vec![row]);
    let outcome = precheck(&bytes, &ctx).await;

    assert_eq!(outcome.report.error, 1, "工号不存在应报 1 条");
    let issue = &outcome.report.errors[0];
    assert_eq!(issue.row_no, 2, "行号必须是 Excel 真实行号（第 1 行是表头）");
    assert_eq!(issue.column_key, Some("emp_no"));
    assert_eq!(issue.kind.as_str(), "FK_NOT_FOUND");
    assert!(
        issue.message.contains("HR21IM-NOBODY"),
        "报文要点名工号：{}",
        issue.message
    );
    assert!(outcome.rows.is_empty(), "有错行时 STRICT 不应带出可入库的行");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn name_mismatch_is_rejected_with_row_number() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    // 工号是甲、姓名写成「王五」→ 典型的 Excel 插删行错位
    let mut row = fixture.row_a(2);
    row[2] = "王五".to_string();
    let bytes = bytes_of(&AttendanceImport, vec![row]);
    let outcome = precheck(&bytes, &ctx).await;
    assert_eq!(outcome.report.error, 0, "姓名核对在入库预检里，不在逐格校验里");

    let error = db()
        .transaction(|tx| async move { AttendanceImport.persist(&tx, &outcome.rows, &ctx).await })
        .await
        .expect_err("姓名不符必须拒绝");
    let message = error.to_string();
    assert!(message.contains("Excel 第 2 行"), "报文要带行号：{message}");
    assert!(
        message.contains("张三") && message.contains("王五"),
        "报文要给出两边的值：{message}"
    );

    let found = records(fixture.period_id).await;
    assert!(found.is_empty(), "整批回滚：库里必须 0 行");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn missing_period_is_reported_with_row_number() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    let mut row = fixture.row_a(2);
    row[0] = "2499-01".to_string();
    let bytes = bytes_of(&AttendanceImport, vec![row]);
    let outcome = precheck(&bytes, &ctx).await;

    assert_eq!(outcome.report.error, 1, "期间不存在应报 1 条");
    let issue = &outcome.report.errors[0];
    assert_eq!(issue.row_no, 2);
    assert_eq!(issue.column_key, Some("period_code"));
    assert_eq!(issue.kind.as_str(), "FK_NOT_FOUND");
    assert!(issue.message.contains("2499-01"), "{}", issue.message);

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn bad_period_month_is_rejected_before_anything_reaches_the_table() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    // 2026-13：宽正则会放过它（设计 §3.1 ④ 的坑）。
    //
    // 两层校验都会拦，但**先命中框架的外键校验**（`validate.rs` 的顺序是「外键 → 业务规则」，
    // 且外键一旦不过就不再调 `validate_row`）：`2026-13` 在库里当然不存在，所以报
    // `FK_NOT_FOUND`。业务规则（`period_code_valid`）是第二道，专治「库里恰好存在一个
    // 格式非法的期间」这种外键挡不住的情形 —— 那条路径由单测
    // `validate_row_reports_bad_period_with_row_number` 覆盖。
    let mut row = fixture.row_a(2);
    row[0] = "2026-13".to_string();
    let bytes = bytes_of(&AttendanceImport, vec![row]);
    let outcome = precheck(&bytes, &ctx).await;

    assert_eq!(outcome.report.error, 1, "只报一条：外键命中后不再叠加业务规则");
    let issue = &outcome.report.errors[0];
    assert_eq!(issue.row_no, 2);
    assert_eq!(issue.column_key, Some("period_code"));
    assert_eq!(issue.kind.as_str(), "FK_NOT_FOUND");
    assert!(issue.message.contains("2026-13"), "{}", issue.message);
    assert!(outcome.rows.is_empty(), "非法考勤月不许带出可入库的行");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn submitted_period_still_accepts_import() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    // 设计 §6.2：期间下拉只列 DRAFT / SUBMITTED —— 两种都允许导入，锁定才不行
    set_period_status(fixture.period_id, "SUBMITTED").await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    let (_, affected) = import(vec![fixture.row_a(3)], &ctx).await;
    assert_eq!(affected, 1);
    assert_eq!(records(fixture.period_id).await.len(), 1);

    fixture.cleanup().await;
    cleanup_tasks().await;
}

#[tokio::test]
async fn definition_accepts_rows_with_only_the_required_columns() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    // 只填必填 4 列：其余数值列必须落 0（不是 NULL、也不是空串）
    let row = vec![
        fixture.period_code.clone(),
        fixture.emp_a.clone(),
        String::new(),
        "22".to_string(),
        "22".to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    let (_, affected) = import(vec![row], &ctx).await;
    assert_eq!(affected, 1);

    let zeros = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_attendance_record \
              WHERE period_id = ?::uuid AND deleted_at IS NULL \
                AND leave_paid_days = 0 AND leave_unpaid_days = 0 AND sick_days = 0 \
                AND absent_days = 0 AND ot_hours_workday = 0 AND ot_hours_weekend = 0 \
                AND ot_hours_holiday = 0 AND late_count = 0 AND early_leave_count = 0 \
                AND remark IS NULL",
            vec![Value::from(fixture.period_id)],
        )
        .await
        .expect("统计默认值");
    assert_eq!(zeros, 1, "留空的数值列必须落 0，备注落 NULL");

    fixture.cleanup().await;
    cleanup_tasks().await;
}

/// 「同一份文件里同一工号出现两次」由框架的组合唯一校验拦住（`unique_key`）。
#[tokio::test]
async fn duplicate_employee_in_one_file_is_rejected() {
    let Some(_guard) = guard().await else { return };
    let fixture = Fixture::seed().await;
    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);

    let bytes = bytes_of(&AttendanceImport, vec![fixture.row_a(3), fixture.row_a(4)]);
    let outcome = precheck(&bytes, &ctx).await;
    assert_eq!(outcome.report.error, 1, "第二行与第一行重复（同期间同工号）");
    let issue = &outcome.report.errors[0];
    // 文件里两行的 Excel 行号是 2 / 3（表头占第 1 行）
    assert_eq!(issue.row_no, 3);
    assert_eq!(issue.kind.as_str(), "UNIQUE");
    assert!(
        issue.message.contains("第 2 行"),
        "要指出与哪一行重复：{}",
        issue.message
    );

    fixture.cleanup().await;
    cleanup_tasks().await;
}

/// 真库用例用的 `RowValues` 直构造（HR-22 的导入后处理会复用同一条链路）。
#[allow(dead_code)]
fn direct_row(row_no: u32, pairs: &[(&'static str, &str)]) -> RowValues {
    let mut values = RowValues {
        row_no,
        ..Default::default()
    };
    for (key, value) in pairs {
        values.fields.insert(key, value.to_string());
    }
    values
}

/// 让 `Arc` 依赖保持在使用状态（与 `master_data/db_tests.rs` 同一形态，
/// 后续用例若要 `Arc<dyn ImportDefinition>` 直接可用）。
#[allow(dead_code)]
fn as_arc() -> Arc<dyn ImportDefinition> {
    Arc::new(AttendanceImport)
}
