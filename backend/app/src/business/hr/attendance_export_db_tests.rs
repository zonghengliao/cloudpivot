//! 考勤台账导出（`GET /business/hr/attendance/export`）的真库用例（HR-30）。
//!
//! 单测证明了列的形状（20 列 / 锁定标签 / 数值字面值）；证明不了这三件事，只有真库 + 真 xlsx 能答：
//!
//! 1. **验收项「导出列完整」**：100 人一期导出的 xlsx 读回来，表头逐字等于
//!    [`EXPORT_HEADERS`]、数据行数 = 库里记录数、每个数值单元格与库里的值对得上；
//! 2. **验收项「锁定状态列正确」**：同一期在 `DRAFT` 与 `LOCKED` 两种状态下各导一次，
//!    整表每一行的「锁定状态」列跟着变（`未锁定` → `已锁定`），期间状态列同步；
//! 3. **验收项「大期间（100 人）不超时」**：100 人端到端（取数 + 生成 xlsx）耗时打出来并断言上限；
//!    另有「部门范围不越权」——导出的表里不能出现别部门的人（数据越权会跟着 Excel 传出去）。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr30_drill \
//!   cargo test --lib business::hr::attendance_export_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 读回 xlsx 为什么用 `calamine` 直读
//!
//! `impexp::engine::SheetReader` 是按**导入定义**（`&'static [ImportColumn]`）读的，
//! 导出表不是导入模板，套不进去。`calamine` 是本仓已有的直接依赖
//! （`backend/Cargo.toml`：与 `nik-utils` 的 xlsx feature 同一版本），在测试里直读
//! 单元格是「验证导出内容」最直接的方式 —— 否则只能断言「字节非空」，
//! 那证明不了列对不对。
//!
//! # 夹具纪律
//!
//! - 期间码取 2400~2899 年（远离真实数据），重跑不撞 `uk_biz_att_period`；
//! - 100 个员工**一条 SQL 批量插**（`generate_series`）：逐个走 `employee_handler::create_employee`
//!   会把 100 次建号开销算进耗时里，测出来的就不是导出的速度了；
//! - 清场**按 id 删**（绝不按前缀扫全库），顺序：记录 → 期间 → 员工 → 部门。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::attendance_export::{self, EXPORT_HEADERS};
    use crate::business::hr::attendance_period_dto::{
        PeriodCreateReq, PeriodStatusReq, STATUS_DRAFT, STATUS_LOCKED, STATUS_SUBMITTED,
    };
    use crate::business::hr::attendance_period_handler;
    use crate::business::hr::attendance_record_dto::{RecordFilter, RecordNumbers};
    use crate::business::hr::attendance_record_model as model;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::test_helpers;
    use calamine::{Data, Reader, Xlsx};
    use nik::prelude::chrono::{Duration, NaiveDate};
    use nik::prelude::rust_decimal::Decimal;
    use std::io::Cursor;
    use tokio::sync::OnceCell;

    /// 验收里的「大期间」人数。
    const BIG_PERIOD_HEADS: i32 = 100;

    /// 端到端耗时上限（秒）：验收只说「不超时」，这里钉一个宽松但真实的上限 ——
    /// 100 行导出正常是几十毫秒量级，3 秒已经给了 30 倍余量（CI 机器慢也放得过）。
    const EXPORT_BUDGET_SECS: u64 = 3;

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
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(error) => {
                        println!("SKIP: 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
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

    /// 期间码：年份取 2400~2899（远离真实数据与演示库），月份取 01~12。
    fn period_code() -> String {
        let n = uniq();
        let year = 2400 + n % 500;
        let month = 1 + (n / 500) % 12;
        format!("{year:04}-{month:02}")
    }

    fn month_range(period_code: &str) -> (NaiveDate, NaiveDate) {
        let year: i32 = period_code[..4].parse().expect("年份");
        let month: u32 = period_code[5..].parse().expect("月份");
        let start = NaiveDate::from_ymd_opt(year, month, 1).expect("月初");
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .expect("次月初");
        (start, next - Duration::days(1))
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法小数")
    }

    fn super_principal() -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), None, true, ScopeKind::All, vec![], false)
    }

    fn dept_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), false, ScopeKind::Dept, vec![dept], false)
    }

    fn operator() -> Operator {
        Operator::named(Some(Uuid::now_v7()), "HR30 夹具")
    }

    /// 夹具：一个部门 + N 名员工 + 一期 + N 行记录。
    struct Fixture {
        dept: Uuid,
        other_dept: Uuid,
        dept_code: String,
        other_dept_code: String,
        code: String,
        period_id: Uuid,
        employees: Vec<(Uuid, String, String)>,
        record_ids: Vec<Uuid>,
    }

    impl Fixture {
        /// 每行都用同一份数值（`应 22 / 实 20 / 无薪假 2 / 平时加班 10.5`），
        /// 这样「每个单元格都对得上库里」是按列比对而不是按行猜。
        fn numbers() -> RecordNumbers {
            RecordNumbers {
                should_days: dec("22"),
                actual_days: dec("20"),
                leave_paid_days: dec("0"),
                leave_unpaid_days: dec("2"),
                sick_days: dec("0"),
                absent_days: dec("0"),
                ot_hours_workday: dec("10.5"),
                ot_hours_weekend: dec("4"),
                ot_hours_holiday: dec("0"),
                late_count: 1,
                early_leave_count: 0,
            }
        }

        async fn build(heads: i32) -> Self {
            let tag = format!("HR30-{}", uniq());
            let dept_code = format!("{tag}-D");
            let other_dept_code = format!("{tag}-O");
            let dept = test_helpers::ensure_dept(&dept_code, "HR30 生产部").await;
            let other_dept = test_helpers::ensure_dept(&other_dept_code, "HR30 别部门").await;

            // 100 个员工一条 SQL 批量插（逐个走 create_employee 会把建号开销算进导出耗时）
            let sql = format!(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status, employment_type) \
                 SELECT gen_random_uuid(), ?::uuid, '{tag}-' || lpad(n::text, 3, '0'), '{tag} 员工' || n, \
                        ?::uuid, DATE '2024-03-01', 'ACTIVE', 'FULL_TIME' \
                   FROM generate_series(1, {heads}) AS n"
            );
            db().exec_sql(sql, vec![Value::from(&SENTINEL_ORG_ID), Value::from(&dept)])
                .await
                .expect("批量插员工");

            let rows = db()
                .query_sql(
                    "SELECT id, emp_no, name FROM biz_employee \
                      WHERE emp_no LIKE ? AND deleted_at IS NULL ORDER BY emp_no"
                        .to_string(),
                    vec![Value::from(format!("{tag}-%"))],
                )
                .await
                .expect("读回夹具员工");
            let employees: Vec<(Uuid, String, String)> = rows
                .iter()
                .map(|row| {
                    (
                        row.get_typed("id").expect("id"),
                        row.get_typed("emp_no").expect("emp_no"),
                        row.get_typed("name").expect("name"),
                    )
                })
                .collect();
            assert_eq!(employees.len(), usize::try_from(heads).expect("heads"), "员工数");

            // 期间（状态机从 DRAFT 起步）
            let code = period_code();
            let (start, end) = month_range(&code);
            let created = attendance_period_handler::create_period(
                operator(),
                &PeriodCreateReq {
                    period_code: Some(code.clone()),
                    period_start: Some(start.to_string()),
                    period_end: Some(end.to_string()),
                    remark: Some("HR30 夹具".to_string()),
                },
            )
            .await
            .expect("建考勤期间");
            let period_id = Uuid::parse_str(&created.id).expect("期间 id");

            // N 行记录
            let numbers = Self::numbers();
            let mut record_ids = Vec::with_capacity(employees.len());
            for (employee_id, emp_no, name) in &employees {
                let id = Uuid::now_v7();
                model::insert(
                    db(),
                    id,
                    &model::NewRecord {
                        period_id,
                        employee_id: *employee_id,
                        employee_no: emp_no,
                        employee_name: name,
                        dept_id: Some(dept),
                        numbers: &numbers,
                        source: "IMPORT",
                        remark: Some("HR30 夹具导入"),
                        operator: Some(Uuid::now_v7()),
                    },
                )
                .await
                .expect("插考勤记录");
                record_ids.push(id);
            }

            Self {
                dept,
                other_dept,
                dept_code,
                other_dept_code,
                code,
                period_id,
                employees,
                record_ids,
            }
        }

        fn filter(&self) -> RecordFilter {
            RecordFilter {
                period_id: self.period_id,
                dept_id: None,
                keyword: None,
            }
        }

        /// 把期间推到目标状态（`DRAFT→SUBMITTED→LOCKED`）：幂等，已经在该状态就什么都不做
        /// （`SUBMITTED→SUBMITTED` 会被状态机当 409 拒绝，用例里先提交再锁定是两步）。
        async fn drive_status(&self, to_status: &str) {
            let mut current = self.period_status().await;
            if to_status == STATUS_LOCKED && current == STATUS_DRAFT {
                attendance_period_handler::change_status(
                    operator(),
                    self.period_id,
                    &PeriodStatusReq {
                        to_status: Some(STATUS_SUBMITTED.to_string()),
                        reason: None,
                    },
                )
                .await
                .expect("提交期间");
                current = STATUS_SUBMITTED.to_string();
            }
            if current == to_status {
                return;
            }
            attendance_period_handler::change_status(
                operator(),
                self.period_id,
                &PeriodStatusReq {
                    to_status: Some(to_status.to_string()),
                    reason: None,
                },
            )
            .await
            .expect("流转期间状态");
        }

        async fn period_status(&self) -> String {
            let row = db()
                .query_first_sql(
                    "SELECT status FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.period_id)],
                )
                .await
                .expect("读期间")
                .expect("期间必有行");
            row.get_typed("status").expect("status")
        }

        /// 清场（按 id 删；FK 顺序：记录 → 期间 → 员工 → 部门）。
        async fn clean(&self) {
            for record_id in &self.record_ids {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_attendance_record WHERE id = ?::uuid".to_string(),
                        vec![Value::from(record_id)],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.period_id)],
                )
                .await;
            for (employee_id, _, _) in &self.employees {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                        vec![Value::from(employee_id)],
                    )
                    .await;
            }
            let _ = test_helpers::purge_dept(&self.dept_code).await;
            let _ = test_helpers::purge_dept(&self.other_dept_code).await;
        }
    }

    /// 读回 xlsx：返回（表头, 数据行），每行是 20 个单元格文本。
    fn read_sheet(bytes: &[u8]) -> (Vec<String>, Vec<Vec<String>>) {
        let mut workbook = Xlsx::new(Cursor::new(bytes)).expect("xlsx 必须是可读的 zip 容器");
        let name = workbook.sheet_names().first().cloned().expect("xlsx 至少一个 sheet");
        let range = workbook.worksheet_range(&name).expect("读 sheet");
        let mut rows: Vec<Vec<String>> = range.rows().map(|row| row.iter().map(cell_text).collect()).collect();
        assert!(!rows.is_empty(), "xlsx 至少要有表头行");
        let header = rows.remove(0);
        (header, rows)
    }

    /// 单元格 → 文本（数字去掉尾随 `.0`，`calamine` 把整数读成 `Float`）。
    fn cell_text(cell: &Data) -> String {
        match cell {
            Data::Empty => String::new(),
            Data::String(text) => text.clone(),
            Data::Float(value) => {
                attendance_export::num_text(Decimal::from_str_exact(&format!("{value}")).unwrap_or(Decimal::ZERO))
            }
            Data::Int(value) => value.to_string(),
            other => other.to_string(),
        }
    }

    fn column(header: &[String], name: &str) -> usize {
        header
            .iter()
            .position(|text| text == name)
            .unwrap_or_else(|| panic!("表头里没有「{name}」：{header:?}"))
    }

    /// 验收 ①②③：100 人一期 → 列完整 / 数值对得上 / 耗时在上限内。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_100_rows_is_fast_and_complete() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build(BIG_PERIOD_HEADS).await;

        let started = std::time::Instant::now();
        let (rows, total) = model::export_rows(&fixture.filter(), &super_principal(), 5000)
            .await
            .expect("取导出行");
        let period_status = fixture.period_status().await;
        let bytes = attendance_export::build_workbook(&rows, &fixture.code, &period_status).expect("生成 xlsx");
        let elapsed = started.elapsed();

        println!(
            "[HR-30] 100 人一期导出：{} 行 / {} 字节 / 耗时 {:?}",
            rows.len(),
            bytes.len(),
            elapsed
        );

        assert_eq!(
            rows.len(),
            usize::try_from(BIG_PERIOD_HEADS).expect("heads"),
            "行数 = 人数"
        );
        assert_eq!(total, i64::from(BIG_PERIOD_HEADS), "总数含数据权限后的条数");
        assert!(
            elapsed < std::time::Duration::from_secs(EXPORT_BUDGET_SECS),
            "100 人一期导出耗时 {elapsed:?} 超过上限 {EXPORT_BUDGET_SECS}s"
        );

        // ① 列完整：表头逐字等于常量（顺序也一致）
        let (header, data) = read_sheet(&bytes);
        assert_eq!(
            header,
            EXPORT_HEADERS.map(str::to_string).to_vec(),
            "表头必须逐列等于 EXPORT_HEADERS"
        );
        assert_eq!(
            data.len(),
            usize::try_from(BIG_PERIOD_HEADS).expect("heads"),
            "数据行数"
        );

        // ② 数值列与库里的值对得上（列索引按表头取，不写死下标）
        let idx_no = column(&header, "工号");
        let idx_should = column(&header, "应出勤(天)");
        let idx_actual = column(&header, "实际出勤(天)");
        let idx_unpaid = column(&header, "无薪假(天)");
        let idx_ot = column(&header, "加班合计(小时)");
        let idx_late = column(&header, "迟到(次)");
        let idx_month = column(&header, "考勤月");
        let idx_dept = column(&header, "部门");
        let idx_lock = column(&header, "锁定状态");
        let idx_status = column(&header, "期间状态");
        let idx_source = column(&header, "来源");
        for (index, line) in data.iter().enumerate() {
            assert_eq!(line[idx_month], fixture.code, "第 {} 行考勤月", index + 1);
            assert_eq!(line[idx_no], fixture.employees[index].1, "第 {} 行工号", index + 1);
            assert_eq!(line[idx_should], "22");
            assert_eq!(line[idx_actual], "20");
            assert_eq!(line[idx_unpaid], "2");
            assert_eq!(line[idx_ot], "14.5", "10.5 + 4 + 0");
            assert_eq!(line[idx_late], "1");
            assert_eq!(line[idx_dept], "HR30 生产部");
            assert_eq!(line[idx_source], "导入");
            // ③ 锁定状态列：此刻期间是 DRAFT（没提交也没锁），整表「未锁定」
            assert_eq!(line[idx_lock], attendance_export::LOCK_LABEL_UNLOCKED);
            assert_eq!(line[idx_status], "草稿");
        }

        fixture.clean().await;
    }

    /// 验收②：同一期锁定前后各导一次，「锁定状态」列与「期间状态」列跟着变。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn locked_period_marks_every_row_locked() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build(2).await;

        let draft_status = fixture.period_status().await;
        assert_eq!(draft_status, STATUS_DRAFT);
        let (rows, _) = model::export_rows(&fixture.filter(), &super_principal(), 5000)
            .await
            .expect("取导出行");
        let bytes = attendance_export::build_workbook(&rows, &fixture.code, &draft_status).expect("xlsx");
        let (header, data) = read_sheet(&bytes);
        let idx_lock = column(&header, "锁定状态");
        assert!(
            data.iter()
                .all(|line| line[idx_lock] == attendance_export::LOCK_LABEL_UNLOCKED)
        );

        // 提交（仍未锁定：不能当工资基线）
        fixture.drive_status(STATUS_SUBMITTED).await;
        let submitted_status = fixture.period_status().await;
        assert_eq!(submitted_status, STATUS_SUBMITTED);
        let (rows, _) = model::export_rows(&fixture.filter(), &super_principal(), 5000)
            .await
            .expect("取导出行");
        let bytes = attendance_export::build_workbook(&rows, &fixture.code, &submitted_status).expect("xlsx");
        let (header, data) = read_sheet(&bytes);
        let idx_lock = column(&header, "锁定状态");
        let idx_status = column(&header, "期间状态");
        assert!(
            data.iter()
                .all(|line| line[idx_lock] == attendance_export::LOCK_LABEL_UNLOCKED),
            "SUBMITTED 不是工资基线，锁定状态必须仍是「未锁定」"
        );
        assert!(data.iter().all(|line| line[idx_status] == "已提交"));

        // 锁定
        fixture.drive_status(STATUS_LOCKED).await;
        let locked_status = fixture.period_status().await;
        assert_eq!(locked_status, STATUS_LOCKED);
        let (rows, _) = model::export_rows(&fixture.filter(), &super_principal(), 5000)
            .await
            .expect("取导出行");
        let bytes = attendance_export::build_workbook(&rows, &fixture.code, &locked_status).expect("xlsx");
        let (header, data) = read_sheet(&bytes);
        let idx_lock = column(&header, "锁定状态");
        let idx_status = column(&header, "期间状态");
        assert!(
            data.iter()
                .all(|line| line[idx_lock] == attendance_export::LOCK_LABEL_LOCKED),
            "锁定后每一行都必须是「已锁定」"
        );
        assert!(data.iter().all(|line| line[idx_status] == "已锁定"));
        // 文件名也跟着带锁定态（HR 下载一堆表时靠名字区分）
        assert!(attendance_export::file_name(&fixture.code, true).contains("已锁定"));

        fixture.clean().await;
    }

    /// 数据权限：部门范围导出不能带出别部门的人（越权会跟着 Excel 传出去）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dept_scope_does_not_leak_other_departments() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build(3).await;

        // 别人部门的两行记录（同一期间）：超管导得到，部门范围导不到
        let numbers = Fixture::numbers();
        let other_no = format!("HR30-O-{}", uniq() % 1_000_000);
        let (other_emp, other_no, other_name) = {
            let sql = "INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status, employment_type) \
                       VALUES (gen_random_uuid(), ?::uuid, ?, ?, ?::uuid, DATE '2024-03-01', 'ACTIVE', 'FULL_TIME') \
                       RETURNING id";
            let row = db()
                .query_first_sql(
                    sql.to_string(),
                    vec![
                        Value::from(&SENTINEL_ORG_ID),
                        Value::from(other_no.clone()),
                        Value::from("HR30 别部门员工"),
                        Value::from(&fixture.other_dept),
                    ],
                )
                .await
                .expect("插别部门员工")
                .expect("RETURNING 必有行");
            let id: Uuid = row.get_typed("id").expect("id");
            (id, other_no, "HR30 别部门员工".to_string())
        };
        let other_record = Uuid::now_v7();
        model::insert(
            db(),
            other_record,
            &model::NewRecord {
                period_id: fixture.period_id,
                employee_id: other_emp,
                employee_no: &other_no,
                employee_name: &other_name,
                dept_id: Some(fixture.other_dept),
                numbers: &numbers,
                source: "MANUAL",
                remark: None,
                operator: None,
            },
        )
        .await
        .expect("插别部门记录");

        let (all, total_all) = model::export_rows(&fixture.filter(), &super_principal(), 5000)
            .await
            .expect("超管取数");
        assert_eq!(all.len(), 4, "超管：3 人 + 别部门 1 人");
        assert_eq!(total_all, 4);

        let (mine, total_mine) = model::export_rows(&fixture.filter(), &dept_principal(fixture.dept), 5000)
            .await
            .expect("部门范围取数");
        assert_eq!(mine.len(), 3, "部门范围：只有本部门 3 人");
        assert_eq!(total_mine, 3);
        assert!(
            mine.iter().all(|row| row.employee_no != other_no),
            "导出的表里出现了别部门的人（数据越权）"
        );

        // 清场：别部门那行记录与员工
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_record WHERE id = ?::uuid".to_string(),
                vec![Value::from(&other_record)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                vec![Value::from(&other_emp)],
            )
            .await;
        fixture.clean().await;
    }
}
