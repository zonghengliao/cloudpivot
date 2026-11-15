//! 考勤导入**路由**（`POST /business/hr/attendance/import` · 设计 §5.2 #23 · 看板卡 t_df5d6d1c）的真库用例。
//!
//! 单测证明的是「本文件没复制导入逻辑」与「权限点真的在种子里」；证明不了这四件事，
//! 只有真库（+ 真 xlsx + 真 multipart）能答：
//!
//! 1. **路由回执与通用上传同构**：走新路由上传 2 行 → `report.total/valid/error = 2/2/0`，
//!    `task_id` 能在 `sys_import_task` 里查到，且 `biz_type` 被钉成 `hr.attendance`
//!    （客户端即使想传 `biz_type` 也没有这个入参）；
//! 2. **确认入库仍走 impexp 的 `{id}/confirm`**：入库 2 行 → 期间头
//!    `employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`
//!    —— **全程没有调任何 recalc 接口**，这就是「委托没有绕开 `persist` 里的重算」的证据；
//! 3. **锁定期间仍被拦**：预检报告里 2 行都带 Excel 行号 + 「已锁定」，确认入库回
//!    422 + `60014`，且期间头与记录数**一个字节没动**（与通用路径行为一致）；
//! 4. **选项快照跟着任务走**：`mode` / `duplicate` 被快照进 `sys_import_task`
//!    （`options.duplicate_strategy`），确认入库只凭 `task_id` 读回来。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@127.0.0.1:15432/hr34_drill \
//!   ERP_DB_URL=同上一行 \
//!   cargo test --lib business::hr::attendance_import_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-19 / HR-20 / HR-22 的教训）
//!
//! - 期间码年份取 2400+（不撞 `uk_biz_att_period`），重跑不冲突；
//! - 清场按 id 删：导入任务（错误明细 → 任务）→ 记录 → 期间 → 员工 → 用户 → 部门
//!   （`biz_attendance_record.employee_id` 是 `RESTRICT`，顺序不能换）；
//! - 操作人用**真实 `sys_user`**（`admin`）：确认入库要 `principal::resolve_for_claims`
//!   与 `create_by` 范围过滤，随机 UUID 会解析不出数据范围（那不是本卡要验的失败）；
//! - 上传文件真的落盘（`{upload_path}/impexp/{task_id}.xlsx`），清场时一并删掉。

use std::collections::HashMap;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_import_handler;
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::impexp::builtin::hr::ATTENDANCE_BIZ_TYPE;
use crate::business::impexp::registry::global_registry;

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
                    println!("考勤导入路由真库用例：已连接 {name}");
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

fn uniq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    now.wrapping_add(seq.wrapping_mul(7_919))
}

/// 期间码：年份取 2400~2899（远离真实数据），月份取 01~12。
fn period_code() -> String {
    let n = uniq();
    let year = 2400 + n % 500;
    let month = 1 + (n / 500) % 12;
    format!("{year:04}-{month:02}")
}

/// 与期间码同年的自然月区间（起 = 1 日，止 = 月末）。
fn month_range(period_code: &str) -> (chrono::NaiveDate, chrono::NaiveDate) {
    let year: i32 = period_code[..4].parse().expect("年份");
    let month: u32 = period_code[5..].parse().expect("月份");
    let start = chrono::NaiveDate::from_ymd_opt(year, month, 1).expect("月初");
    let next = if month == 12 {
        chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("次月初");
    (start, next - chrono::Duration::days(1))
}

fn dec(text: &str) -> rust_decimal::Decimal {
    use std::str::FromStr;
    rust_decimal::Decimal::from_str(text).expect("合法小数")
}

/// 造一份真的 xlsx（表头 = 定义里的模板表头，与用户下载的模板逐字一致）。
///
/// 参数是 `(考勤月, 工号, 姓名, 应出勤, 实际出勤)`；其余列留空（可选列）。
fn attendance_xlsx(rows: &[(&str, &str, &str, &str, &str)]) -> Vec<u8> {
    let definition = global_registry()
        .import_definition(ATTENDANCE_BIZ_TYPE)
        .expect("hr.attendance 应已注册");
    let headers = definition
        .columns()
        .iter()
        .map(|column| column.template_header())
        .collect::<Vec<String>>();
    let header_refs = headers.iter().map(String::as_str).collect::<Vec<&str>>();
    let sheet_rows = rows
        .iter()
        .map(|(period, emp_no, name, should, actual)| {
            let mut row: HashMap<String, String> = HashMap::new();
            for column in definition.columns() {
                let value = match column.key {
                    "period_code" => *period,
                    "emp_no" => *emp_no,
                    "employee_name" => *name,
                    "should_days" => *should,
                    "actual_days" => *actual,
                    _ => "",
                };
                row.insert(column.template_header().to_string(), value.to_string());
            }
            row
        })
        .collect::<Vec<_>>();
    crate::business::impexp::engine::write_sheet(&header_refs, &sheet_rows).expect("造 xlsx")
}

/// 把字节包成上传接口要的 `Multipart`（字段名 `file`，与前端 `FormData` 一致）。
///
/// 不是「模拟」：走的是 axum 自己的 `Multipart::from_request`，所以字段名、
/// `Content-Type` 边界、文件名解析都是真实路径上的那一套。
async fn multipart_of(file_name: &str, bytes: Vec<u8>) -> axum::extract::Multipart {
    use axum::body::Body;
    use axum::extract::{FromRequest, Multipart, Request};

    let boundary = format!("----hr34{}", uniq());
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = Request::builder()
        .method("POST")
        .uri("/business/hr/attendance/import")
        .header(
            axum::http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .expect("构造 multipart 请求");
    Multipart::from_request(request, &())
        .await
        .expect("解析 multipart（字段名 file）")
}

/// 夹具：一个部门 + 两名员工 + 一个考勤期间（草稿）。
struct Fixture {
    period_code: String,
    period_id: Uuid,
    emp1: Uuid,
    emp1_no: String,
    emp1_name: String,
    emp2: Uuid,
    emp2_no: String,
    emp2_name: String,
    dept_code: String,
    user1: Uuid,
    user1_name: String,
    user2_name: String,
}

async fn fixture(tag: &str) -> Fixture {
    use crate::business::hr::attendance_period_dto::PeriodCreateReq;
    use crate::business::hr::attendance_period_handler;
    use crate::business::hr::employee_handler;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::test_helpers;

    let dept_code = format!("HR34-{tag}-{}", uniq());
    let dept_name = format!("HR34 部门 {tag}");
    let dept = test_helpers::ensure_dept(&dept_code, &dept_name).await;

    let mut employees = Vec::new();
    for label in ["a", "b"] {
        let suffix = uniq();
        // 工号上限 32 字符：用时间戳后 8 位（够区分，不越界）
        let short = format!("{}", suffix % 100_000_000);
        let username = format!("hr34.{tag}.{label}.{short}");
        let name = format!("HR34 员工 {tag}{label}{short}");
        let user = test_helpers::ensure_user(&username, &name, Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR34-{label}-{short}")),
                name: Some(name.clone()),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                user_id: Some(user.to_string()),
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建员工");
        let employee_id = Uuid::parse_str(&created.id).expect("员工 id");
        let emp_no = db()
            .scalar_with_params::<String>(
                "SELECT emp_no FROM biz_employee WHERE id = ?::uuid",
                vec![Value::from(&employee_id)],
            )
            .await
            .expect("读工号");
        employees.push((employee_id, emp_no, name, user, username));
    }

    let code = period_code();
    let (start, end) = month_range(&code);
    let period = attendance_period_handler::create_period(
        Operator::named(Some(employees[0].3), "HR34 夹具"),
        &PeriodCreateReq {
            period_code: Some(code.clone()),
            period_start: Some(start.to_string()),
            period_end: Some(end.to_string()),
            remark: Some("HR34 夹具".to_string()),
        },
    )
    .await
    .expect("建考勤期间");

    Fixture {
        period_code: code,
        period_id: Uuid::parse_str(&period.id).expect("期间 id"),
        emp1: employees[0].0,
        emp1_no: employees[0].1.clone(),
        emp1_name: employees[0].2.clone(),
        emp2: employees[1].0,
        emp2_no: employees[1].1.clone(),
        emp2_name: employees[1].2.clone(),
        dept_code,
        user1: employees[0].3,
        user1_name: employees[0].4.clone(),
        user2_name: employees[1].4.clone(),
    }
}

impl Fixture {
    /// 期间头四个汇总列（直接读库）。
    async fn stored_totals(&self) -> (i32, rust_decimal::Decimal, rust_decimal::Decimal, rust_decimal::Decimal) {
        let row = db()
            .query_first_sql(
                "SELECT employee_count, should_days_total, actual_days_total, ot_hours_total \
                   FROM biz_attendance_period WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&self.period_id)],
            )
            .await
            .expect("读期间汇总")
            .expect("期间必有行");
        (
            row.get_typed("employee_count").expect("employee_count"),
            row.get_typed("should_days_total").expect("should_days_total"),
            row.get_typed("actual_days_total").expect("actual_days_total"),
            row.get_typed("ot_hours_total").expect("ot_hours_total"),
        )
    }

    /// 本期记录行数。
    async fn record_count(&self) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_attendance_record WHERE period_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&self.period_id)],
        )
        .await
        .expect("数记录行")
    }

    /// 把期间推到目标态（`DRAFT→SUBMITTED→LOCKED`），用 HR-19 的状态机（不写裸 SQL：
    /// 锁定要过 `attendance_lock` 的判据与 `biz_status_log`，那也是「锁定」这个词的定义）。
    async fn lock(&self) {
        use crate::business::hr::attendance_period_dto::{PeriodStatusReq, STATUS_LOCKED, STATUS_SUBMITTED};
        use crate::business::hr::attendance_period_handler;
        use crate::business::shared::state_machine::Operator;

        attendance_period_handler::change_status(
            Operator::named(Some(self.user1), "HR34 夹具"),
            self.period_id,
            &PeriodStatusReq {
                to_status: Some(STATUS_SUBMITTED.to_string()),
                reason: None,
            },
        )
        .await
        .expect("提交期间");
        attendance_period_handler::change_status(
            Operator::named(Some(self.user1), "HR34 夹具"),
            self.period_id,
            &PeriodStatusReq {
                to_status: Some(STATUS_LOCKED.to_string()),
                reason: None,
            },
        )
        .await
        .expect("锁定期间");
    }

    /// 清场：导入任务（错误明细 → 任务）→ 记录 → 期间 → 员工 → 用户 → 部门。
    async fn clean(&self, task_ids: &[String]) {
        use crate::shared::test_helpers;

        for task_id in task_ids {
            let Ok(id) = Uuid::parse_str(task_id) else { continue };
            // 上传时落盘的文件（不清会越积越多）
            let path = std::path::Path::new(nik::upload_path())
                .join("impexp")
                .join(format!("{id}.xlsx"));
            let _ = tokio::fs::remove_file(path).await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM sys_import_task_error WHERE task_id = ?::uuid".to_string(),
                    vec![Value::from(&id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM sys_import_task WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&id)],
                )
                .await;
        }
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid".to_string(),
                vec![Value::from(&self.period_id)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_status_log WHERE entity_type = 'ATTENDANCE_PERIOD' AND entity_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&self.period_id)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                vec![Value::from(&self.period_id)],
            )
            .await;
        for employee in [self.emp1, self.emp2] {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&employee)],
                )
                .await;
        }
        test_helpers::purge_user(&self.user1_name).await;
        test_helpers::purge_user(&self.user2_name).await;
        test_helpers::purge_dept(&self.dept_code).await;
    }
}

/// 库里的超管 claims（`sub` 必须是真实 `sys_user`：数据范围解析与 `create_by` 都要它）。
async fn admin_claims() -> Option<TokenClaims> {
    let row = db()
        .query_first_sql(
            "SELECT u.id::text AS id FROM sys_user u \
               JOIN sys_user_role ur ON ur.user_id = u.id \
               JOIN sys_role r ON r.id = ur.role_id \
              WHERE u.username = 'admin' AND r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL \
              LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("查 admin");
    let row = row.or_else(|| {
        println!("SKIP: 库里没有 admin + SUPER_ADMIN（先跑 make_full_suite_db.sh）");
        None
    })?;
    let id: String = row.get_typed("id").expect("admin id");
    Some(TokenClaims {
        jti: None,
        sub: id,
        username: Some("admin".to_string()),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    })
}

/// 读任务行（`biz_type` / `mode` / `options` / 状态 / 行数）。
async fn task_row(task_id: &str) -> (String, String, String, String, i32, i32, i32) {
    use std::str::FromStr;
    let id = Uuid::from_str(task_id).expect("任务 id");
    let row = db()
        .query_first_sql(
            "SELECT biz_type, mode, coalesce(options::text, '{}') AS options, status, \
                    total_rows, valid_rows, error_rows \
               FROM sys_import_task WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("读导入任务")
        .expect("任务必有行");
    (
        row.get_typed("biz_type").expect("biz_type"),
        row.get_typed("mode").expect("mode"),
        row.get_typed("options").expect("options"),
        row.get_typed("status").expect("status"),
        row.get_typed("total_rows").expect("total_rows"),
        row.get_typed("valid_rows").expect("valid_rows"),
        row.get_typed("error_rows").expect("error_rows"),
    )
}

// ==================== ① 路由上传 + 确认入库 → 期间头重算 ====================

/// 走新路由上传 2 行 → 回执与通用上传同构 → 确认入库（impexp 通用接口）→
/// 期间头 `employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`。
///
/// 关键点：**全程没有调用任何 recalc 接口**。期间头能对上，只可能是因为委托到了
/// `AttendanceImport::persist` 的同一事务里（重算在那儿）。若有人在业务侧复制一份
/// 「只落库不重算」的实现，这条断言会直接红。
#[tokio::test]
async fn route_upload_then_confirm_recalcs_period_header() {
    use axum::extract::{Extension, Path};

    let Some(_guard) = guard().await else { return };
    let Some(claims) = admin_claims().await else { return };
    let fx = fixture("flow").await;

    let bytes = attendance_xlsx(&[
        (&fx.period_code, &fx.emp1_no, &fx.emp1_name, "22", "20"),
        (&fx.period_code, &fx.emp2_no, &fx.emp2_name, "22", "20"),
    ]);

    // ---- 上传（走新路由的唯一入口）----
    let upload = attendance_import_handler::import_for_attendance(
        None,
        None,
        multipart_of("2026年考勤.xlsx", bytes).await,
        &claims,
    )
    .await
    .expect("新路由上传应返回 200 + report");

    assert_eq!(upload.status, "VALIDATED");
    assert_eq!(upload.report.total, 2, "两行数据");
    assert_eq!(upload.report.valid, 2);
    assert_eq!(upload.report.error, 0, "锁定前两行都能过校验");
    assert!(!upload.task_id.is_empty());

    // 任务行：biz_type 被钉成 hr.attendance，选项快照跟着任务走
    let (biz_type, mode, options, status, total, valid, error) = task_row(&upload.task_id).await;
    assert_eq!(biz_type, ATTENDANCE_BIZ_TYPE, "biz_type 必须是 hr.attendance");
    assert_eq!(mode, "STRICT", "默认 STRICT");
    assert!(options.contains("UPSERT"), "默认重复策略 UPSERT：{options}");
    assert_eq!(status, "VALIDATED");
    assert_eq!((total, valid, error), (2, 2, 0));

    // ---- 确认入库：仍用 impexp 的通用 {id}/confirm（本卡不拆第二个接口）----
    let confirmed = crate::business::impexp::handler::import_confirm(
        Path(upload.task_id.clone()),
        Extension(AuthClaims(claims.clone())),
    )
    .await
    .expect("确认入库应成功（锁定前）");
    assert_eq!(confirmed.code, 0, "确认入库成功：{}", confirmed.msg);
    let confirmed_data = confirmed.data.expect("确认入库必有回执体");
    assert_eq!(confirmed_data.status, "SUCCESS");
    assert_eq!(confirmed_data.imported_rows, 2, "两行都进库");

    // ---- 期间头：没有任何客户端 recalc 也正确 ----
    let (count, should, actual, ot) = fx.stored_totals().await;
    assert_eq!(count, 2, "两人 → employee_count = 2");
    assert_eq!(should, dec("44.00"), "22 + 22");
    assert_eq!(actual, dec("40.00"), "20 + 20");
    assert_eq!(ot, dec("0"), "没填加班");

    // 记录也真的两行（头对了但明细空 = 另一种事故）
    assert_eq!(fx.record_count().await, 2);

    let (_, _, _, status, ..) = task_row(&upload.task_id).await;
    assert_eq!(status, "SUCCESS");

    fx.clean(&[upload.task_id]).await;
}

// ==================== ② 锁定期间：确认入库 422 + Excel 行号 + 库未动 ====================

/// 锁定期间后走同一条路由：上传照常返回报告（锁定不是「解析错误」，预检里查不出来），
/// **确认入库**回 422 + `60014`，报文带 Excel 行号与「已锁定」，期间头与记录数一个字节没动
/// —— 与通用路径完全一致（因为它就是同一条路径）。
#[tokio::test]
async fn locked_period_import_is_rejected_with_excel_row_no() {
    use axum::extract::{Extension, Path};

    let Some(_guard) = guard().await else { return };
    let Some(claims) = admin_claims().await else { return };
    let fx = fixture("locked").await;
    fx.lock().await;

    let bytes = attendance_xlsx(&[
        (&fx.period_code, &fx.emp1_no, &fx.emp1_name, "22", "20"),
        (&fx.period_code, &fx.emp2_no, &fx.emp2_name, "22", "21"),
    ]);
    let upload = attendance_import_handler::import_for_attendance(
        Some("STRICT"),
        Some("UPSERT"),
        multipart_of("locked.xlsx", bytes).await,
        &claims,
    )
    .await
    .expect("上传本身返回 200 + 报告（锁定是入库期的事，见下）");

    // 上传期：两行语法/外键都过（期间存在、工号存在）—— 所以 report 是全绿的
    assert_eq!(upload.report.total, 2);
    assert_eq!(upload.report.error, 0, "锁定与否不是行级校验能看出来的（要看期间状态）");
    let (_, _, _, status, _, _, _) = task_row(&upload.task_id).await;
    assert_eq!(status, "VALIDATED");

    // ---- 确认入库：STRICT → 422 + 60014（第 1 批（行 2–3）失败 + Excel 行号 + 已锁定）----
    let error = crate::business::impexp::handler::import_confirm(
        Path(upload.task_id.clone()),
        Extension(AuthClaims(claims.clone())),
    )
    .await
    .expect_err("锁定期间确认入库必须被拦");
    assert_eq!(error.status, 422);
    assert_eq!(error.code, crate::shared::error_code::IMPORT_BATCH_FAILED);
    assert!(
        error.msg.contains("已锁定"),
        "报文要说清「为什么」——已锁定：{}",
        error.msg
    );
    assert!(
        error.msg.contains("Excel 第 2 行") && error.msg.contains("Excel 第 3 行"),
        "报文要逐行给 Excel 行号（表头占第 1 行，两行数据 = 2、3 行）：{}",
        error.msg
    );

    // ---- 库未动：期间头全 0、0 条记录 ----
    let (count, should, actual, ot) = fx.stored_totals().await;
    assert_eq!(count, 0, "锁定期间 employee_count 不动");
    assert_eq!(should, dec("0.00"));
    assert_eq!(actual, dec("0.00"));
    assert_eq!(ot, dec("0"));
    assert_eq!(fx.record_count().await, 0, "一行都没进库");

    // 任务被打回 FAILED（可重试），并把原因留在任务行上（页面直接可见）
    let (_, _, _, status, _, valid, _) = task_row(&upload.task_id).await;
    assert_eq!(status, "FAILED", "STRICT 入库失败后任务置 FAILED（可重试）");
    assert_eq!(valid, 2, "校验是过的，失败在入库期");
    let message: Option<String> = db()
        .scalar_with_params::<String>(
            "SELECT coalesce(message, '') FROM sys_import_task WHERE id = ?::uuid",
            vec![Value::from(&Uuid::parse_str(&upload.task_id).expect("任务 id"))],
        )
        .await
        .ok();
    let message = message.unwrap_or_default();
    assert!(message.contains("已锁定"), "任务行的 message 要带原因：{message}");

    fx.clean(&[upload.task_id]).await;
}

// ==================== ③ 入参只认 mode / duplicate，biz_type 固定 ====================

/// 传 `duplicate=SKIP` 时任务行快照必须是 SKIP（不是默认 UPSERT）——证明这两个开关
/// 真的透传到了 impexp，而不是在薄封装里被吞掉。
#[tokio::test]
async fn duplicate_and_mode_options_are_snapshotted() {
    let Some(_guard) = guard().await else { return };
    let Some(claims) = admin_claims().await else { return };
    let fx = fixture("opts").await;

    let bytes = attendance_xlsx(&[(&fx.period_code, &fx.emp1_no, &fx.emp1_name, "22", "20")]);
    let upload = attendance_import_handler::import_for_attendance(
        Some("PARTIAL"),
        Some("SKIP"),
        multipart_of("opts.xlsx", bytes).await,
        &claims,
    )
    .await
    .expect("上传应成功");

    let (biz_type, mode, options, _, _, _, _) = task_row(&upload.task_id).await;
    assert_eq!(biz_type, ATTENDANCE_BIZ_TYPE);
    assert_eq!(mode, "PARTIAL", "mode 透传到任务行");
    // jsonb 出库的文本会带空格，按 JSON 解析比对（不赌字面量形态）
    let options: serde_json::Value = serde_json::from_str(&options).expect("options 是 JSON");
    assert_eq!(
        options.get("duplicate_strategy").and_then(|value| value.as_str()),
        Some("SKIP"),
        "重复策略快照必须跟着任务走：{options}"
    );

    fx.clean(&[upload.task_id]).await;
}

// ==================== ④ 期间不存在：预检点名考勤月（不是 500） ====================

/// 考勤月没建期间时，报错要能读懂（点名考勤月 + Excel 行号），不是「导入失败」。
#[tokio::test]
async fn missing_period_is_reported_with_period_code_and_row_no() {
    let Some(_guard) = guard().await else { return };
    let Some(claims) = admin_claims().await else { return };
    let fx = fixture("noperiod").await;

    let ghost = "2899-12".to_string();
    let bytes = attendance_xlsx(&[(&ghost, &fx.emp1_no, &fx.emp1_name, "22", "20")]);
    let upload =
        attendance_import_handler::import_for_attendance(None, None, multipart_of("ghost.xlsx", bytes).await, &claims)
            .await
            .expect("上传返回报告");

    assert_eq!(upload.report.error, 1);
    let message = &upload.report.errors[0].message;
    assert_eq!(upload.report.errors[0].row_no, 2, "Excel 第 2 行");
    assert!(message.contains(&ghost), "报文要点名缺哪个考勤月：{message}");

    fx.clean(&[upload.task_id]).await;
}

/// 夹具自检：本文件的 xlsx 真的能被 impexp 解析（列头 = 模板表头，不缺必需列）。
///
/// 这条不连库：它防的是「夹具构造错了 → 真库用例 SKIP/红在别处」。
#[test]
fn fixture_xlsx_uses_the_template_headers() {
    use crate::business::impexp::engine::SheetReader;
    let bytes = attendance_xlsx(&[("2026-01", "EMP-0001", "张三", "22", "20")]);
    let definition = global_registry().import_definition(ATTENDANCE_BIZ_TYPE).expect("定义");
    let reader = SheetReader::open(&bytes, definition.columns()).expect("模板表头应被接受");
    assert_eq!(reader.data_len(), 1);
    // 「考勤月」列读到的就是第一列的值
    let first = reader.row(0).expect("第一行");
    let period_index = definition
        .columns()
        .iter()
        .position(|column| column.key == "period_code")
        .expect("考勤月列");
    assert_eq!(reader.cell(first, period_index), "2026-01");
    // 组织维度：本文件的夹具都写 SENTINEL_ORG_ID（`Uuid::nil()`，导入定义内部固定它）
    assert!(SENTINEL_ORG_ID.is_nil(), "哨兵组织 id 是全零 uuid");
}
