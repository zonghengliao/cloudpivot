//! 工资表导出（`GET /business/hr/salary/{id}/export`）的真库用例（HR-42）。
//!
//! 单测证明了列的形状、分组规则与小计算法；证明不了这三件事，只有真库 + 真 xlsx 能答：
//!
//! 1. **验收项「列 = 薪资项」**：引擎建出来的真单导出的 xlsx 读回来，表头逐字等于
//!    `工号/姓名/部门` + `V0396` 那 15 个非汇总项名（按 `sort_order`）+ 5 个汇总列；
//! 2. **验收项「按部门分组小计」**：3 个部门（含「没有部门」）500 人，导出的表里
//!    「小计」行数 = 部门数、「合计」行在最后，且每个部门的小计 = 库里该部门的 `SUM(...)`；
//! 3. **验收项「大表不超时」**：500 人 × 15 项端到端（取数 + 拼表 + 生成 xlsx）耗时打出来并断言上限。
//!
//! 不设库地址时全部 SKIP（**不假绿**）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr42_drill \
//!   cargo test --lib business::hr::salary_export_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 读回 xlsx 为什么用 `calamine` 直读
//!
//! `impexp::engine::SheetReader` 是按**导入定义**（`&'static [ImportColumn]`）读的，导出表
//! 不是导入模板，套不进去。`calamine` 是本仓已有的直接依赖（与 `nik-utils` 的 xlsx feature
//! 同一版本），直读单元格是「验证导出内容」最直接的方式 —— 否则只能断言「字节非空」。
//!
//! # 夹具纪律
//!
//! * 工号前缀 `HR42`，薪资期间各用例分开（`2026-07` 引擎单 / `2026-08` 空单 / `2026-09` 大单）：
//!   `uk_biz_sal_sheet_period` 是「一公司一期间一张单」，同一期间会让两个用例互相踩；
//! * 大单的 500 人**一条 SQL 批量插**（`generate_series`）：走建员工接口会把 500 次取号 +
//!   校验算进耗时里，测出来的就不是导出的速度；
//! * `biz_employee.dept_id` 是**弱引用**（`V0383` 不建 FK），大单夹具直接用随机 uuid；
//!   明细行的 `dept_name` 是快照列，按它分组 —— 与生产路径同口径（部门改名后工资表不漂移）；
//! * 清场按前缀 / 按 id 删（绝不按名字扫全库）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use crate::business::hr::fixture_hygiene as hygiene;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_export::{self, EXPORT_MAX_ROWS, PERM_EXPORT};
    use crate::business::hr::salary_sheet_dto::{SalaryManualReq, SalarySheetCalcReq};
    use crate::business::hr::salary_sheet_handler;
    use crate::business::hr::salary_sheet_model;
    use crate::business::shared::state_machine::Operator;
    use calamine::{Data, Reader, Xlsx};
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::Decimal;
    use std::io::Cursor;
    use tokio::sync::OnceCell;

    /// 夹具工号前缀（清场按它精确删）。
    const PREFIX: &str = "HR42";
    /// 引擎建单用例的期间（`ck_biz_sal_sheet_code` 只认 `yyyy-MM`）。
    const PERIOD_ENGINE: &str = "2026-07";
    /// 空单用例的期间。
    const PERIOD_EMPTY: &str = "2026-08";
    /// 大单用例的期间。
    const PERIOD_BIG: &str = "2026-09";

    /// 大单人数（验收里「大表」的量级：一期 500 人是集团级月度工资表的规模）。
    const BIG_HEADS: i32 = 500;
    /// 大单里没有部门的人数（验证「(未分配部门) 排在最后」）。
    const BIG_NO_DEPT: i32 = 1;
    /// 大单两个有部门的组。
    const BIG_DEPT_A: i32 = 300;
    /// 第二个部门的实际人数 = 总数 − 前两组。
    const _BIG_DEPT_B: i32 = BIG_HEADS - BIG_DEPT_A - BIG_NO_DEPT;

    /// 端到端耗时上限（秒）：500 人 × 15 项正常是「秒级」以下，这里给足余量（CI 机器慢也放得过），
    /// 但它能抓住真正的退化（逐行查询 / 逐行 `SUM` 会立刻把它顶穿）。
    const EXPORT_BUDGET_SECS: u64 = 15;

    /// `V0396` 种子里的 15 个非汇总项（表头必须是这 15 个名字，按 `sort_order`）。
    const SEED_ITEMS: [&str; 15] = [
        "基本工资",
        "岗位津贴",
        "餐补",
        "绩效奖金",
        "平时加班费",
        "休息日加班费",
        "节假日加班费",
        "全勤奖",
        "考勤扣款",
        "社保(个人)",
        "公积金(个人)",
        "个人所得税",
        "其他代扣",
        "单位社保",
        "单位公积金",
    ];

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址或连不上，用例直接跳过。
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
                        // 连上就打一行：`--nocapture` 时能证明这批用例**真的**跑了（不假绿）。
                        // 只打库名：URL 里带口令，日志里不许出现凭据。
                        let database = url.rsplit('/').next().unwrap_or("?");
                        println!("HR42 真库用例已连接：database={database}");
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

    /// 唯一后缀（纳秒 + 进程内序号）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    fn org() -> Uuid {
        SENTINEL_ORG_ID
    }

    fn money(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn day(text: &str) -> NaiveDate {
        text.parse().expect("日期字面量")
    }

    // ---- xlsx 读回 ----

    /// 读出第一个 sheet 的全部行（每行按列序的文本）。
    fn read_sheet(bytes: &[u8]) -> Vec<Vec<String>> {
        let mut workbook = Xlsx::new(Cursor::new(bytes)).expect("xlsx 必须是可读的 zip 容器");
        let range = workbook
            .worksheet_range_at(0)
            .expect("至少要有一个 sheet")
            .expect("sheet 可读");
        range
            .rows()
            .map(|row| row.iter().map(cell_text).collect::<Vec<String>>())
            .collect()
    }

    /// 单元格 → 文本（`calamine` 把整数读成 `Float`，`13000.0` 要还原成 `13000`）。
    fn cell_text(cell: &Data) -> String {
        match cell {
            Data::Empty => String::new(),
            Data::String(text) => text.clone(),
            Data::Float(value) => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    format!("{}", *value as i64)
                } else {
                    // 与 `num_text` 同口径：去掉尾随零
                    let text = format!("{value:.4}");
                    text.trim_end_matches('0').trim_end_matches('.').to_string()
                }
            }
            Data::Int(value) => value.to_string(),
            other => other.to_string(),
        }
    }

    /// 列头 → 列下标（表头逐字匹配，找不到就 panic 点名）。
    fn column_of(header_row: &[String], header: &str) -> usize {
        header_row
            .iter()
            .position(|value| value == header)
            .unwrap_or_else(|| panic!("表头里没有「{header}」：{header_row:?}"))
    }

    /// 找出所有「小计」行（姓名列 = 小计）的下标。
    fn subtotal_rows(rows: &[Vec<String>], name_column: usize) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .skip(1)
            .filter(|(_, row)| row.get(name_column).map(String::as_str) == Some("小计"))
            .map(|(index, _)| index)
            .collect()
    }

    async fn dept_id() -> Uuid {
        let row = db()
            .query_first_sql(
                "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY dept_name LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查部门")
            .expect("库里至少要有一个部门（bootstrap 会建「总部」）");
        row.get_typed("id").expect("部门 id")
    }

    async fn operator() -> Operator {
        let operator_id = db()
            .query_first_sql(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查操作人")
            .map(|row| row.get_typed::<Uuid>("id").expect("用户 id"));
        Operator::named(operator_id, "hr42-test")
    }

    /// 引擎建单的夹具：2 名员工（同部门），标准 + 考勤期间 + 考勤记录齐备。
    struct EngineFixture {
        suffix: String,
        emp_a: Uuid,
        emp_b: Uuid,
        period_id: Uuid,
        dept: Uuid,
        /// 引擎输入集的互斥锁 guard（用例结束才释放；字段只为持有生命周期）。
        _engine: tokio::sync::MutexGuard<'static, ()>,
    }

    impl EngineFixture {
        fn calc_req(&self) -> SalarySheetCalcReq {
            // 人工项全部用**种子项编码**：`GROSS` / `NET` 的公式只认这些编码，
            // 换一套编码就会「公式侧 0、聚合侧有值」而被引擎拒掉（HR-36 的实测结论）。
            let entries = [
                ("PERF", "1200"),
                ("SI_PERSONAL", "840"),
                ("HF_PERSONAL", "960"),
                ("IIT", "0"),
                ("SI_COMPANY", "1680"),
                ("HF_COMPANY", "960"),
            ];
            let manual: Vec<SalaryManualReq> = entries
                .iter()
                .flat_map(|(code, value)| {
                    [self.emp_a, self.emp_b]
                        .into_iter()
                        .map(move |employee| SalaryManualReq {
                            employee_id: Some(employee.to_string()),
                            item_code: Some((*code).to_string()),
                            amount: Some(money(value)),
                        })
                })
                .collect();
            SalarySheetCalcReq {
                period_code: Some(PERIOD_ENGINE.to_string()),
                attendance_period_id: Some(self.period_id.to_string()),
                dry_run: Some(false),
                remark: Some("HR42 真库用例（引擎单）".to_string()),
                manual: Some(manual),
            }
        }
    }

    async fn engine_fixture() -> Option<EngineFixture> {
        if !ensure_db().await {
            return None;
        }
        // 统一清场：引擎全量读，按 `HR42%` 前缀清不出「引擎眼里的空库」（卡 t_9a4e5c2a）
        let engine = hygiene::exclusive().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let period_id = Uuid::now_v7();

        for (id, no, name) in [
            (emp_a, format!("HR42A-{suffix}"), "张三"),
            (emp_b, format!("HR42B-{suffix}"), "李四"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                          employment_type, id_card_type) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 1, ?::uuid, ?::text::date, 'ACTIVE', 'FULL_TIME', 'ID_CARD')"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&org()),
                    Value::Text(no.clone()),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("2020-01-01".to_string()),
                ],
            )
            .await
            .expect("插员工");

            // 标准：BASE 8000（两人同标准，合计好核）
            db().exec_sql(
                "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, 'BASE', ?::text::numeric, ?::text::date)"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::from(&id),
                    Value::Text("8000".to_string()),
                    Value::Text("2020-01-01".to_string()),
                ],
            )
            .await
            .expect("插薪资标准");
        }

        // 考勤期间 + 两人的考勤记录（应出勤 22 / 实出勤 20 / 无薪假 2 / 加班 10 小时）
        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')"
                .to_string(),
            vec![
                Value::from(&period_id),
                Value::from(&org()),
                Value::Text(PERIOD_ENGINE.to_string()),
                Value::Text("2026-07-01".to_string()),
                Value::Text("2026-07-31".to_string()),
            ],
        )
        .await
        .expect("插考勤期间");

        for (id, no, name) in [
            (emp_a, format!("HR42A-{suffix}"), "张三"),
            (emp_b, format!("HR42B-{suffix}"), "李四"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_attendance_record (id, org_id, period_id, employee_id, employee_no, \
                                                    employee_name, dept_id, should_days, actual_days, \
                                                    leave_unpaid_days, ot_hours_workday, source) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, ?, ?::uuid, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 'MANUAL')"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::from(&period_id),
                    Value::from(&id),
                    Value::Text(no),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("22".to_string()),
                    Value::Text("20".to_string()),
                    Value::Text("2".to_string()),
                    Value::Text("10".to_string()),
                ],
            )
            .await
            .expect("插考勤记录");
        }

        Some(EngineFixture {
            suffix,
            emp_a,
            emp_b,
            period_id,
            dept,
            _engine: engine,
        })
    }

    async fn cleanup_engine(fixture: &EngineFixture) {
        for (sql, params) in [
            (
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid",
                vec![Value::Text(PERIOD_ENGINE.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid",
                vec![Value::from(&fixture.period_id)],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid",
                vec![Value::from(&fixture.period_id)],
            ),
            (
                "DELETE FROM biz_employee_salary WHERE employee_id IN (?::uuid, ?::uuid)",
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
            (
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
        ] {
            let _ = db().exec_sql(sql.to_string(), params).await;
        }
        let _ = fixture.suffix.as_str();
        let _ = fixture.dept;
    }

    async fn count(sql: &str, params: Vec<Value>) -> i64 {
        db().query_first_sql(sql.to_string(), params)
            .await
            .expect("计数查询")
            .map(|row| row.get_typed::<i64>("n").expect("count"))
            .unwrap_or(0)
    }

    /// 空单夹具：一张 `CALCULATED` 的薪资单，零明细（模拟「建单后明细被清空 / 还没算人」）。
    async fn empty_sheet() -> Uuid {
        let sheet_id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_salary_sheet (id, org_id, sheet_no, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?, ?::text::date, ?::text::date, 'CALCULATED')"
                .to_string(),
            vec![
                Value::from(&sheet_id),
                Value::from(&org()),
                Value::Text(format!("HR42-EMPTY-{}", uniq())),
                Value::Text(PERIOD_EMPTY.to_string()),
                Value::Text("2026-08-01".to_string()),
                Value::Text("2026-08-31".to_string()),
            ],
        )
        .await
        .expect("插空单");
        sheet_id
    }

    /// 大单夹具：`BIG_HEADS` 人（两个部门 + 1 个无部门）× 15 个明细项，全部一条 SQL 批量插。
    struct BigFixture {
        sheet_id: Uuid,
        dept_a_name: String,
        dept_b_name: String,
        /// 引擎输入集的互斥锁 guard（用例结束才释放；字段只为持有生命周期）。
        _engine: tokio::sync::MutexGuard<'static, ()>,
    }

    async fn big_fixture() -> Option<BigFixture> {
        if !ensure_db().await {
            return None;
        }
        // 统一清场：引擎全量读，按 `HR42%` 前缀清不出「引擎眼里的空库」（卡 t_9a4e5c2a）
        let engine = hygiene::exclusive().await;
        let suffix = uniq();
        let sheet_id = Uuid::now_v7();
        let dept_a_name = format!("HR42生产部{suffix}");
        let dept_b_name = format!("HR42技术部{suffix}");

        db().exec_sql(
            "INSERT INTO biz_salary_sheet (id, org_id, sheet_no, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?, ?::text::date, ?::text::date, 'CALCULATED')"
                .to_string(),
            vec![
                Value::from(&sheet_id),
                Value::from(&org()),
                Value::Text(format!("HR42-BIG-{suffix}")),
                Value::Text(PERIOD_BIG.to_string()),
                Value::Text("2026-09-01".to_string()),
                Value::Text("2026-09-30".to_string()),
            ],
        )
        .await
        .expect("插大单");

        // 三批员工：部门 A / 部门 B / 无部门（`dept_id` 是弱引用，这里给随机 uuid 即可）
        for (group, heads) in [
            ("A", BIG_DEPT_A),
            ("B", BIG_HEADS - BIG_DEPT_A - BIG_NO_DEPT),
            ("N", BIG_NO_DEPT),
        ] {
            db().exec_sql(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                          employment_type, id_card_type) \
                 SELECT gen_random_uuid(), ?::uuid, ? || '-' || lpad(g::text, 4, '0'), \
                        '批量' || g::text, 1, gen_random_uuid(), '2020-01-01', 'ACTIVE', 'FULL_TIME', 'ID_CARD' \
                   FROM generate_series(1, ?::int) AS g"
                    .to_string(),
                vec![
                    Value::from(&org()),
                    Value::Text(format!("HR42-B-{group}-{suffix}")),
                    Value::I32(heads),
                ],
            )
            .await
            .expect("批量插员工");
        }

        // 明细：一人一行，5 个金额列满足库层三条恒等式（应发 = 收入 − 税前扣；代扣 = 四项之和；
        // 应发 = 实发 + 代扣）。金额按工号末四位递增，让「小计 ≠ 单人金额」这件事能被断言。
        for (group, dept_name) in [
            ("A", Some(dept_a_name.clone())),
            ("B", Some(dept_b_name.clone())),
            ("N", None),
        ] {
            let dept_value = dept_name.clone().map(Value::Text).unwrap_or(Value::Null);
            db().exec_sql(
                "INSERT INTO biz_salary_detail (id, org_id, sheet_id, employee_id, employee_no, employee_name, \
                                                dept_id, dept_name, earning_amount, pre_deduct_amount, gross_amount, \
                                                social_personal, fund_personal, iit_amount, other_withhold, \
                                                withhold_amount, net_amount, si_company, hf_company) \
                 SELECT gen_random_uuid(), ?::uuid, ?::uuid, e.id, e.emp_no, e.name, \
                        NULL, ?::text, \
                        5000 + 10 * g, 0, 5000 + 10 * g, \
                        400, 100, 50, 0, 550, 5000 + 10 * g - 550, 1680, 960 \
                   FROM (SELECT id, emp_no, name, \
                                row_number() OVER (ORDER BY emp_no) AS g \
                           FROM biz_employee WHERE emp_no LIKE ?) e"
                    .to_string(),
                vec![
                    Value::from(&org()),
                    Value::from(&sheet_id),
                    dept_value,
                    Value::Text(format!("HR42-B-{group}-{suffix}%")),
                ],
            )
            .await
            .expect("批量插明细");
        }

        // 明细项：每人 15 项（与 V0396 的 15 个非汇总项同名同类型，行号一致）
        db().exec_sql(
            "INSERT INTO biz_salary_detail_item (id, org_id, sheet_id, detail_id, item_code, item_name, \
                                                 item_type, calc_type, amount, line_no) \
             SELECT gen_random_uuid(), ?::uuid, ?::uuid, d.id, v.code, v.name, v.itype, 'INPUT', \
                    v.amount + 0 * d.net_amount, v.line_no \
               FROM biz_salary_detail d \
               CROSS JOIN (VALUES \
                    ('BASE', '基本工资', 'EARNING', 10, 5000::numeric), \
                    ('POST', '岗位津贴', 'EARNING', 20, 0::numeric), \
                    ('MEAL', '餐补', 'EARNING', 30, 0::numeric), \
                    ('PERF', '绩效奖金', 'EARNING', 40, 0::numeric), \
                    ('OT_WD', '平时加班费', 'EARNING', 50, 0::numeric), \
                    ('OT_WE', '休息日加班费', 'EARNING', 60, 0::numeric), \
                    ('OT_HO', '节假日加班费', 'EARNING', 70, 0::numeric), \
                    ('ATT_BONUS', '全勤奖', 'EARNING', 80, 0::numeric), \
                    ('ATT_DEDUCT', '考勤扣款', 'PRE_DEDUCT', 90, 0::numeric), \
                    ('SI_PERSONAL', '社保(个人)', 'WITHHOLD', 100, 400::numeric), \
                    ('HF_PERSONAL', '公积金(个人)', 'WITHHOLD', 110, 100::numeric), \
                    ('IIT', '个人所得税', 'WITHHOLD', 120, 50::numeric), \
                    ('OTHER_WITHHOLD', '其他代扣', 'WITHHOLD', 130, 0::numeric), \
                    ('SI_COMPANY', '单位社保', 'EMPLOYER', 140, 1680::numeric), \
                    ('HF_COMPANY', '单位公积金', 'EMPLOYER', 150, 960::numeric) \
               ) AS v(code, name, itype, line_no, amount) \
              WHERE d.sheet_id = ?::uuid"
                .to_string(),
            vec![Value::from(&org()), Value::from(&sheet_id), Value::from(&sheet_id)],
        )
        .await
        .expect("批量插明细项");

        // 单头汇总按明细重算（`recalc_sheet_totals` 就是生产路径用的那一份）
        salary_sheet_model::recalc_sheet_totals(db(), sheet_id)
            .await
            .expect("重算单头");

        Some(BigFixture {
            sheet_id,
            dept_a_name,
            dept_b_name,
            _engine: engine,
        })
    }

    async fn cleanup_big(fixture: &BigFixture) {
        for (sql, params) in [
            (
                "DELETE FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&fixture.sheet_id)],
            ),
            (
                format!("DELETE FROM biz_employee WHERE emp_no LIKE '{PREFIX}%'"),
                vec![],
            ),
        ] {
            let _ = db().exec_sql(sql, params).await;
        }
        let _ = &fixture.dept_a_name;
        let _ = &fixture.dept_b_name;
    }

    // ---- ① 引擎建的真的单：表头 = 薪资项，合计 = 单头 ----

    #[tokio::test]
    async fn export_of_a_real_sheet_has_item_columns_and_ties_to_the_header() {
        let Some(fixture) = engine_fixture().await else { return };
        let created = salary_sheet_handler::persist(operator().await, &fixture.calc_req())
            .await
            .expect("建单必须成功");
        let sheet = created.sheet.expect("建单回执必须带单头");
        let sheet_id = Uuid::parse_str(&sheet.id).expect("单头 id");

        let bundle = salary_export::export_bundle(sheet_id).await.expect("导出必须成功");
        assert!(bundle.consistent, "刚建的单，单头必须与明细一致");
        assert_eq!(bundle.total_rows, 2, "两名员工");

        let bytes = bundle.to_workbook().expect("生成 xlsx");
        let rows = read_sheet(&bytes);

        // ① 列 = 薪资项：3 + 15 + 5
        let header = &rows[0];
        assert_eq!(header.len(), 3 + SEED_ITEMS.len() + 5, "表头列数：{header:?}");
        assert_eq!(&header[..3], &["工号", "姓名", "部门"]);
        assert_eq!(
            &header[3..3 + SEED_ITEMS.len()],
            &SEED_ITEMS.map(str::to_string),
            "项列必须是 V0396 的 15 个非汇总项，且按 sort_order"
        );
        assert_eq!(
            &header[3 + SEED_ITEMS.len()..],
            &salary_export::SUMMARY_HEADERS.map(str::to_string),
            "末 5 列必须是汇总列"
        );
        // 汇总项（GROSS / NET）不能当列：它们不落 detail_item，当列只会是永远空白的孪生列，
        // 而这两个数字已经由末 5 列的「应发合计 / 实发合计」承载
        let item_slice = &header[3..3 + SEED_ITEMS.len()];
        assert!(
            !item_slice.contains(&"应发合计".to_string()),
            "GROSS 不该占一列：{item_slice:?}"
        );
        assert!(
            !item_slice.contains(&"实发合计".to_string()),
            "NET 不该占一列：{item_slice:?}"
        );

        // ② 行：2 名员工 + 1 个部门小计 + 1 个合计
        assert_eq!(
            rows.len(),
            1 + 2 + 1 + 1,
            "数据行：{:?}",
            rows.iter().map(|r| &r[1]).collect::<Vec<_>>()
        );
        let name_column = column_of(header, "姓名");
        let subtotals = subtotal_rows(&rows, name_column);
        assert_eq!(subtotals.len(), 1, "两人同部门 → 1 行小计");
        assert_eq!(rows[subtotals[0]][name_column], "小计");
        assert_eq!(rows.last().expect("末行")[name_column], "合计");

        // ③ 合计行 = 单头 5 个金额列（一分不差）
        let header_row = db()
            .query_first_sql(
                "SELECT gross_total, withhold_total, net_total, si_company_total, hf_company_total \
                   FROM biz_salary_sheet WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&sheet_id)],
            )
            .await
            .expect("查单头金额")
            .expect("单头必须存在");
        let total_row = rows.last().expect("合计行");
        for (header_name, stored) in [
            ("应发合计", "gross_total"),
            ("代扣合计", "withhold_total"),
            ("实发合计", "net_total"),
            ("单位社保合计", "si_company_total"),
            ("单位公积金合计", "hf_company_total"),
        ] {
            let column = column_of(header, header_name);
            let expected: Decimal = header_row.get_typed(stored).expect("单头金额");
            let actual = money(&total_row[column]);
            assert_eq!(
                actual, expected,
                "合计行「{header_name}」= {actual}，单头 = {expected}（Excel 与系统必须对得上）"
            );
        }

        // ④ 每个员工的项金额 = 库里 detail_item 的金额（列与项一一对应）
        let detail_rows: Vec<&Vec<String>> = rows[1..]
            .iter()
            .filter(|row| row[name_column] != "小计" && row[name_column] != "合计")
            .collect();
        for row in &detail_rows {
            let emp_no = row[0].clone();
            let per_item = count(
                "SELECT count(*) AS n FROM biz_salary_detail_item i \
                   JOIN biz_salary_detail d ON d.id = i.detail_id \
                  WHERE d.sheet_id = ?::uuid AND d.employee_no = ?",
                vec![Value::from(&sheet_id), Value::Text(emp_no.clone())],
            )
            .await;
            assert_eq!(per_item as usize, SEED_ITEMS.len(), "{emp_no} 的明细项条数");

            // 逐项对拍：库里的 amount → 表里的单元格
            for item_name in SEED_ITEMS {
                let column = column_of(header, item_name);
                let stored = db()
                    .query_first_sql(
                        "SELECT i.amount FROM biz_salary_detail_item i \
                           JOIN biz_salary_detail d ON d.id = i.detail_id \
                          WHERE d.sheet_id = ?::uuid AND d.employee_no = ? AND i.item_name = ?"
                            .to_string(),
                        vec![
                            Value::from(&sheet_id),
                            Value::Text(emp_no.clone()),
                            Value::Text(item_name.to_string()),
                        ],
                    )
                    .await
                    .expect("查明细项金额")
                    .expect("明细项必须存在");
                let amount: Decimal = stored.get_typed("amount").expect("金额");
                assert_eq!(
                    money(&row[column]),
                    amount,
                    "{emp_no} 的「{item_name}」：表里 {} ≠ 库里 {amount}",
                    row[column]
                );
            }
        }

        cleanup_engine(&fixture).await;
    }

    // ---- ② 空单：只有表头 ----

    #[tokio::test]
    async fn export_of_an_empty_sheet_is_headers_only() {
        if !ensure_db().await {
            return;
        }
        let _engine = hygiene::exclusive().await;
        let sheet_id = empty_sheet().await;

        let bundle = salary_export::export_bundle(sheet_id).await.expect("空单也要能导出");
        assert_eq!(bundle.table.employee_count, 0);
        assert_eq!(bundle.table.dept_count, 0);
        assert!(
            bundle.table.lines.is_empty(),
            "零人单不写小计 / 合计行（一行 0 会被当成全员工资是 0）"
        );

        let rows = read_sheet(&bundle.to_workbook().expect("生成 xlsx"));
        assert_eq!(rows.len(), 1, "只有表头一行");
        assert_eq!(rows[0].len(), 3 + SEED_ITEMS.len() + 5);
        assert_eq!(rows[0][0], "工号");

        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&sheet_id)],
            )
            .await;
    }

    // ---- ③ 500 人 × 15 项：分组小计对得上、且不超时 ----

    #[tokio::test]
    async fn export_500_heads_groups_by_department_and_stays_on_budget() {
        let Some(fixture) = big_fixture().await else { return };
        let sheet_id = fixture.sheet_id;

        let started = std::time::Instant::now();
        let bundle = salary_export::export_bundle(sheet_id).await.expect("导出必须成功");
        let bytes = bundle.to_workbook().expect("生成 xlsx");
        let elapsed = started.elapsed();

        println!(
            "HR42 大单导出：{BIG_HEADS} 人 × {} 项 → {} 字节 / {} ms",
            SEED_ITEMS.len(),
            bytes.len(),
            elapsed.as_millis()
        );
        assert!(bundle.consistent, "大单也必须与单头一致");
        assert!(
            elapsed < std::time::Duration::from_secs(EXPORT_BUDGET_SECS),
            "导出 {BIG_HEADS} 人用了 {elapsed:?}，超过 {EXPORT_BUDGET_SECS}s 上限"
        );

        // 表结构：500 员工行 + 3 部门小计 + 1 合计
        assert_eq!(bundle.table.employee_count, BIG_HEADS as usize);
        assert_eq!(bundle.table.dept_count, 3, "两个部门 + (未分配部门)");
        assert_eq!(
            bundle.table.lines.len(),
            BIG_HEADS as usize + 3 + 1,
            "行数 = 员工 + 小计 + 合计"
        );
        assert!(
            bundle.table.lines.len() < EXPORT_MAX_ROWS as usize,
            "本用例不应触发截断"
        );
        assert!(!bundle.table.truncated);

        let rows = read_sheet(&bytes);
        let header = &rows[0];
        assert_eq!(header.len(), 3 + SEED_ITEMS.len() + 5);
        let name_column = column_of(header, "姓名");
        let dept_column = column_of(header, "部门");
        let gross_column = column_of(header, "应发合计");
        let base_column = column_of(header, "基本工资");

        // 小计行数 = 部门数，且「(未分配部门)」在最后
        let subtotals = subtotal_rows(&rows, name_column);
        assert_eq!(subtotals.len(), 3, "每个部门一行小计");
        let dept_names: Vec<&str> = subtotals
            .iter()
            .map(|index| rows[*index][dept_column].as_str())
            .collect();
        // 前两个部门名的顺序按 UTF-8 字节序（导出不依赖库 collation，见 `build_table` 的说明）
        let mut expected_depts = vec![fixture.dept_a_name.as_str(), fixture.dept_b_name.as_str()];
        expected_depts.sort_unstable();
        expected_depts.push(salary_export::EMPTY_DEPT_LABEL);
        assert_eq!(dept_names, expected_depts, "部门按名称升序，(未分配部门) 永远最后");
        assert_eq!(rows.last().expect("合计行")[name_column], "合计");

        // 每个部门的小计 = 库里该部门的 SUM（两条独立路径对拍）
        for index in &subtotals {
            let row = &rows[*index];
            let dept_name = row[dept_column].clone();
            let stored = if dept_name == salary_export::EMPTY_DEPT_LABEL {
                db().query_first_sql(
                    "SELECT COALESCE(SUM(gross_amount), 0) AS gross, COALESCE(SUM(net_amount), 0) AS net \
                       FROM biz_salary_detail WHERE sheet_id = ?::uuid AND dept_name IS NULL"
                        .to_string(),
                    vec![Value::from(&sheet_id)],
                )
                .await
                .expect("查部门合计（空部门）")
                .expect("有行")
            } else {
                db().query_first_sql(
                    "SELECT COALESCE(SUM(gross_amount), 0) AS gross, COALESCE(SUM(net_amount), 0) AS net \
                       FROM biz_salary_detail WHERE sheet_id = ?::uuid AND dept_name = ?"
                        .to_string(),
                    vec![Value::from(&sheet_id), Value::Text(dept_name.clone())],
                )
                .await
                .expect("查部门合计")
                .expect("有行")
            };
            let gross: Decimal = stored.get_typed("gross").expect("应发合计");
            let net: Decimal = stored.get_typed("net").expect("实发合计");
            assert_eq!(money(&row[gross_column]), gross, "「{dept_name}」小计的应发合计");
            let net_column = column_of(header, "实发合计");
            assert_eq!(money(&row[net_column]), net, "「{dept_name}」小计的实发合计");
            // 基本工资：每人 5000，小计必须是人数 × 5000（不是单人的金额）
            let members = count(
                "SELECT count(*) AS n FROM biz_salary_detail WHERE sheet_id = ?::uuid AND \
                 ((? = '(未分配部门)' AND dept_name IS NULL) OR dept_name = ?)",
                vec![
                    Value::from(&sheet_id),
                    Value::Text(dept_name.clone()),
                    Value::Text(dept_name.clone()),
                ],
            )
            .await;
            assert_eq!(
                money(&row[base_column]),
                money("5000") * Decimal::from(members),
                "「{dept_name}」的 {members} 人，基本工资小计"
            );
        }

        // 合计行 = 各部门小计之和 = 单头（两条独立路径再对一次）
        let total_row = rows.last().expect("合计行");
        let sheet_totals = db()
            .query_first_sql(
                "SELECT gross_total, net_total, withhold_total FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&sheet_id)],
            )
            .await
            .expect("查单头")
            .expect("单头必须存在");
        let expected_gross: Decimal = sheet_totals.get_typed("gross_total").expect("应发");
        assert_eq!(money(&total_row[gross_column]), expected_gross, "合计行应发 = 单头应发");
        let subtotal_sum: Decimal = subtotals.iter().map(|index| money(&rows[*index][gross_column])).sum();
        assert_eq!(subtotal_sum, expected_gross, "各部门小计之和 = 合计 = 单头");

        cleanup_big(&fixture).await;
    }

    // ---- ④ 入参 / 不存在 ----

    #[tokio::test]
    async fn export_rejects_bad_id_and_missing_sheet() {
        if !ensure_db().await {
            return;
        }

        let error = salary_export::export_response("not-a-uuid")
            .await
            .expect_err("非 uuid 必须 400");
        assert_eq!(error.status, 400, "报文：{}", error.msg);

        let missing = Uuid::now_v7();
        let error = salary_export::export_response(&missing.to_string())
            .await
            .expect_err("不存在的单必须 404");
        assert_eq!(error.status, 404, "报文：{}", error.msg);
        assert!(error.msg.contains(&missing.to_string()), "报文要点名 id：{}", error.msg);

        // 权限码与 V0387 的种子逐字一致（写错只会表现为线上 403）
        assert_eq!(PERM_EXPORT, "business:hr:salary:export");
        let _ = day("2026-07-01");
    }
}
