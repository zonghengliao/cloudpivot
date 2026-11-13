//! 薪资计算引擎（`salary_compute`）的真库用例（HR-34）。
//!
//! 单测（`salary_compute` 里的 22 条）证明的是**算法与恒等式**；证明不了四件事，只有真库能答：
//!
//! 1. **试算真的零落库**：单头 / 明细 / 明细项一行都不写，`biz_seq(SAL)` 也不推进
//!    （「试算 100 次把号段跑光」是真实会发生的运维事故）；
//! 2. **落库在事务里**：单头 + 明细 + 明细项 + 状态日志**同一笔**写进去，库层
//!    `ck_biz_sal_sheet_*` / `ck_biz_sal_det_*` 全部通过（恒等式在库里也成立）；
//! 3. **三方对账（巡检 A6 / A7）**：单头合计 = Σ 明细 = Σ 明细项（按类型）；
//! 4. **试算与正式计算同源**：先 `dry_run` 再 `dry_run=false`，逐人实发一分不差。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr34_drill \
//!   cargo test --lib business::hr::salary_compute_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! * 薪资项编码统一前缀 `HR34`（`HR34BASE` / `HR34OT_WD` …）：既不撞 `V0387` 种的 17 个
//!   种子项（`BASE` / `OT_WD` …），也不撞别的卡的真库用例；清场按精确编码删。
//! * 员工工号 `HR34-<纳秒>`；薪资期间固定 `2026-11`（唯一索引是
//!   `(org_id, period_code)`，跑完必须清干净，否则下一轮撞 409 —— 这本身就是被验的行为）。
//! * 引擎的取数（员工 / 标准 / 考勤）是**全量**读，所以本文件的用例必须在只有一个夹具的库上跑
//!   （`--test-threads=1` + 每个用例自建自清）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use nik::prelude::rust_decimal::Decimal;

    use crate::business::hr::fixture_hygiene as hygiene;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_compute::{
        self as engine_mod, AttendanceNumbers, CalcRequest, CalcTotals, ManualEntry, STATUS_CALCULATED,
    };
    use crate::business::hr::salary_item_model;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use tokio::sync::OnceCell;

    /// 夹具编码前缀（清场按它精确删）。
    const PREFIX: &str = "HR34";
    /// 薪资期间（`ck_biz_sal_sheet_code` 只认 `yyyy-MM`）。
    const PERIOD: &str = "2026-11";
    /// 薪资项的计算顺序起点（避开种子项的 10/20/…/170 与别的卡的 900000 段）。
    const SORT_BASE: i32 = 940_000;

    /// 夹具用到的组织哨兵（生产路径由 `model::SENTINEL_ORG_ID` 决定；这里只做夹具写入）。
    fn org() -> Uuid {
        SENTINEL_ORG_ID
    }

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
                        println!("HR34 真库用例已连接：database={database}");
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

    fn amount(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn day(text: &str) -> NaiveDate {
        text.parse().expect("日期字面量")
    }

    /// 11 个夹具薪资项（9 计算项 + 2 汇总项）：编码 / 名称 / 类型 / 算法 / 公式。
    ///
    /// 公式与设计 §4.4.3 逐条同构，只是编码加了 `HR34` 前缀（`{HR34BASE}` 当变量）。
    fn item_rows() -> Vec<(
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    )> {
        vec![
            ("HR34BASE", "基本工资", "EARNING", "FIXED", None),
            ("HR34PERF", "绩效奖金", "EARNING", "INPUT", None),
            (
                "HR34OT_WD",
                "平时加班费",
                "EARNING",
                "FORMULA",
                Some("ROUND({HR34BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)"),
            ),
            (
                "HR34DEDUCT",
                "考勤扣款",
                "PRE_DEDUCT",
                "FORMULA",
                Some("ROUND({HR34BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)"),
            ),
            ("HR34SI_P", "社保(个人)", "WITHHOLD", "INPUT", None),
            ("HR34HF_P", "公积金(个人)", "WITHHOLD", "INPUT", None),
            ("HR34IIT", "个人所得税", "WITHHOLD", "INPUT", None),
            ("HR34SI_C", "单位社保", "EMPLOYER", "INPUT", None),
            ("HR34HF_C", "单位公积金", "EMPLOYER", "INPUT", None),
            (
                "HR34GROSS",
                "应发合计",
                "SUMMARY",
                "FORMULA",
                Some("{HR34BASE}+{HR34PERF}+{HR34OT_WD}-{HR34DEDUCT}"),
            ),
            (
                "HR34NET",
                "实发合计",
                "SUMMARY",
                "FORMULA",
                Some("{HR34GROSS}-{HR34SI_P}-{HR34HF_P}-{HR34IIT}"),
            ),
        ]
    }

    /// 一个夹具：2 名员工 + 10 个薪资项 + 标准 + 考勤期间与记录。
    struct Fixture {
        suffix: String,
        emp_a: Uuid,
        emp_b: Uuid,
        period_id: Uuid,
        operator: Operator,
        /// 引擎输入集的互斥锁 guard（用例结束才释放；字段只为持有生命周期）。
        _engine: tokio::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
        fn emp_a_no(&self) -> String {
            format!("HR34A-{}", self.suffix)
        }

        fn emp_b_no(&self) -> String {
            format!("HR34B-{}", self.suffix)
        }

        /// 张三：BASE 8000 / PERF 1200；should 22 / actual 20 / 无薪假 2 / 加班 10h；
        /// 社保个人 840、公积金个人 960、个税 0、其他 0；单位社保 1680、单位公积金 960。
        fn zhangsan_manual(&self) -> Vec<ManualEntry> {
            let rows = [
                ("HR34PERF", "1200"),
                ("HR34SI_P", "840"),
                ("HR34HF_P", "960"),
                ("HR34IIT", "0"),
                ("HR34SI_C", "1680"),
                ("HR34HF_C", "960"),
            ];
            rows.iter()
                .map(|(code, value)| ManualEntry {
                    employee_id: self.emp_a,
                    item_code: (*code).to_string(),
                    amount: amount(value),
                })
                .collect()
        }
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

    /// 清场 = **统一清空薪资引擎的输入集**（员工 / 考勤期间与记录 / 薪资标准 / 工资单 /
    /// 非种子薪资项），返回引擎互斥锁的 guard（本用例期间独占输入集）。
    ///
    /// 为什么不再按 `HR34%` 前缀清：引擎的取数是**全量读**（`calc_with_items` 会把库里所有
    /// 在期员工都算进一张工资单），前缀清场清不出「引擎眼里的空库」—— 上游模块留下的
    /// 员工 / 标准会让本模块的「两名员工」「缺考勤要点名」断言漂掉（卡 `t_9a4e5c2a`：
    /// 整模块跑 25 条红、单独跑全绿）。清场口径见
    /// [`crate::business::hr::fixture_hygiene`]。
    async fn sweep() -> tokio::sync::MutexGuard<'static, ()> {
        hygiene::exclusive().await
    }

    async fn setup() -> Option<Fixture> {
        if !ensure_db().await {
            return None;
        }
        let engine = sweep().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let period_id = Uuid::now_v7();

        for (id, no, name) in [
            (emp_a, format!("HR34A-{suffix}"), "张三"),
            (emp_b, format!("HR34B-{suffix}"), "李四"),
        ] {
            db().exec_sql(
                "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                          employment_type, id_card_type) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 1, ?::uuid, ?::text::date, 'ACTIVE', 'FULL_TIME', 'ID_CARD')"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&org()),
                    Value::Text(no),
                    Value::Text(name.to_string()),
                    Value::from(&dept),
                    Value::Text("2020-01-01".to_string()),
                ],
            )
            .await
            .expect("插员工");
        }

        // 10 个薪资项（科目按类型必填：EARNING/PRE_DEDUCT/EMPLOYER 借+贷、WITHHOLD 只贷、SUMMARY 不要）
        for (index, (code, name, item_type, calc_type, formula)) in item_rows().iter().enumerate() {
            let (debit, credit) = match *item_type {
                "WITHHOLD" => (None, Some("2241.03")),
                "SUMMARY" => (None, None),
                // 单位承担项靠**贷方科目**归位到 si_company / hf_company（编码带 HR34 前缀，
                // 不是种子项 `SI_COMPANY` / `HF_COMPANY`，正好验「科目兜底」这条路）
                "EMPLOYER" if *code == "HR34SI_C" => (Some("5602.11"), Some("2211.02")),
                "EMPLOYER" => (Some("5602.12"), Some("2211.03")),
                _ => (Some("5602.10"), Some("2211.01")),
            };
            db().exec_sql(
                "INSERT INTO biz_salary_item (id, org_id, item_code, item_name, item_type, calc_type, source, \
                                             formula, sort_order, debit_account_code, credit_account_code, \
                                             is_taxable, is_active, is_system) \
                 VALUES (?::uuid, ?::uuid, ?, ?, ?, ?, 'INPUT', NULLIF(?::text, ''), ?, \
                         NULLIF(?::text, ''), NULLIF(?::text, ''), 1, 1, 0)"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&org()),
                    Value::Text((*code).to_string()),
                    Value::Text((*name).to_string()),
                    Value::Text((*item_type).to_string()),
                    Value::Text((*calc_type).to_string()),
                    Value::Text(formula.unwrap_or_default().to_string()),
                    Value::from(SORT_BASE + i32::try_from(index).unwrap_or(0) * 10),
                    Value::Text(debit.unwrap_or_default().to_string()),
                    Value::Text(credit.unwrap_or_default().to_string()),
                ],
            )
            .await
            .unwrap_or_else(|err| panic!("插薪资项 {code}: {err}"));
        }

        // 标准：张三 BASE 8000（李四不配标准 → 全部按 0，用来验「未配标准」）
        db().exec_sql(
            "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'HR34BASE', ?::text::numeric, ?::text::date)"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&emp_a),
                Value::Text("8000".to_string()),
                Value::Text("2020-01-01".to_string()),
            ],
        )
        .await
        .expect("插薪资标准");

        // 考勤期间（DRAFT：薪资试算允许 DRAFT 期间，锁定是过账的要求）
        db().exec_sql(
            "INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, 'DRAFT')"
                .to_string(),
            vec![
                Value::from(&period_id),
                Value::from(&org()),
                Value::Text(PERIOD.to_string()),
                Value::Text("2026-11-01".to_string()),
                Value::Text("2026-11-30".to_string()),
            ],
        )
        .await
        .expect("插考勤期间");

        // 考勤记录：只给张三（李四没有 → `missing_attendance` 要报出来）
        db().exec_sql(
            "INSERT INTO biz_attendance_record (id, org_id, period_id, employee_id, employee_no, employee_name, \
                                                dept_id, should_days, actual_days, leave_unpaid_days, \
                                                ot_hours_workday, source) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, '张三', ?::uuid, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, 'MANUAL')"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&period_id),
                Value::from(&emp_a),
                Value::Text(format!("HR34A-{suffix}")),
                Value::from(&dept),
                Value::Text("22".to_string()),
                Value::Text("20".to_string()),
                Value::Text("2".to_string()),
                Value::Text("10".to_string()),
            ],
        )
        .await
        .expect("插考勤记录");

        // 操作人用库里真实存在的超管（状态日志要 join 用户名；空库首启时回退成 None）
        let operator_id = db()
            .query_first_sql(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
                vec![],
            )
            .await
            .expect("查操作人")
            .map(|row| row.get_typed::<Uuid>("id").expect("用户 id"));

        Some(Fixture {
            suffix,
            emp_a,
            emp_b,
            period_id,
            operator: Operator::named(operator_id, "hr34-test"),
            _engine: engine,
        })
    }

    /// 夹具读回：只取本文件的薪资项（`HR34` 前缀），顺序 = `sort_order`（引擎要求）。
    async fn fixture_items() -> Vec<salary_item_model::SalaryItemRow> {
        salary_item_model::list_all(db())
            .await
            .expect("读薪资项")
            .into_iter()
            .filter(|row| row.item_code.starts_with(PREFIX))
            .collect()
    }

    async fn cleanup(fixture: &Fixture) {
        // 从叶子删起：明细项 / 明细随单头级联 → 先删单头；再考勤记录、标准、薪资项、员工
        // 状态日志没有外键：按「即将删掉的单」先收，否则每轮留孤儿
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_status_log WHERE entity_type = 'SALARY_SHEET' AND entity_id IN \
                    (SELECT id FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid)"
                    .to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid".to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_record WHERE period_id = ?::uuid".to_string(),
                vec![Value::from(&fixture.period_id)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_attendance_period WHERE id = ?::uuid".to_string(),
                vec![Value::from(&fixture.period_id)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_employee_salary WHERE employee_id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_item WHERE item_code LIKE 'HR34%'".to_string(),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)".to_string(),
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            )
            .await;
        let _ = fixture.suffix.clone();
    }

    async fn count(sql: &str, params: Vec<Value>) -> i64 {
        db().query_first_sql(sql.to_string(), params)
            .await
            .expect("计数查询")
            .map(|row| row.get_typed::<i64>("n").expect("count"))
            .unwrap_or(0)
    }

    // ---- ① 试算：零落库 + 不推进号段 ----

    #[tokio::test]
    async fn preview_writes_nothing_and_keeps_the_seq() {
        let Some(fixture) = setup().await else { return };
        let items = fixture_items().await;
        let mut req = CalcRequest::preview(PERIOD, Some(fixture.period_id));
        req.manual = fixture.zhangsan_manual();

        let seq_before = engine_mod::seq_current_no(db()).await.expect("读号段");
        let outcome = engine_mod::calc_with_items(fixture.operator.clone(), &req, &items)
            .await
            .expect("试算必须成功");

        assert!(outcome.dry_run);
        assert!(outcome.sheet.is_none(), "试算不许建单");
        assert_eq!(outcome.period_start, day("2026-11-01"));
        assert_eq!(outcome.period_end, day("2026-11-30"));
        assert_eq!(outcome.attendance_period_id, Some(fixture.period_id));

        // 张三：8000 + 1200 + 689.66 − 735.63 = 9154.03；代扣 1800 → 实发 7354.03
        let zhangsan = outcome
            .slips
            .iter()
            .find(|slip| slip.head.id == fixture.emp_a)
            .expect("张三必须在表里");
        assert_eq!(zhangsan.earning_amount, amount("9889.66"), "收入合计");
        assert_eq!(zhangsan.pre_deduct_amount, amount("735.63"), "考勤扣款");
        assert_eq!(zhangsan.gross_amount, amount("9154.03"), "应发");
        assert_eq!(zhangsan.withhold_amount, amount("1800"), "代扣");
        assert_eq!(zhangsan.net_amount, amount("7354.03"), "实发");
        assert_eq!(zhangsan.si_company, amount("1680"));
        assert_eq!(zhangsan.hf_company, amount("960"));
        assert!(zhangsan.check().is_ok(), "逐人恒等式：{:?}", zhangsan.check().err());
        // 明细项：每人 9 行（11 个启用项 − 2 个 SUMMARY）
        assert_eq!(zhangsan.items.len(), 9);
        assert_eq!(outcome.item_count, 9);

        // 李四：没有标准、没有考勤 → 全 0，但**进表**且被点名
        let lisi = outcome
            .slips
            .iter()
            .find(|slip| slip.head.id == fixture.emp_b)
            .expect("李四也要在表里（没考勤不等于没这个员工）");
        assert_eq!(lisi.gross_amount, Decimal::ZERO);
        assert_eq!(lisi.attendance, AttendanceNumbers::default());
        assert!(
            lisi.remark.as_deref().unwrap_or_default().contains("无考勤记录"),
            "{:?}",
            lisi.remark
        );
        assert_eq!(
            outcome.missing_attendance,
            vec![format!("{} 李四", fixture.emp_b_no())],
            "缺考勤的人要点名"
        );
        assert_eq!(outcome.totals.employee_count, 2);

        // 零落库（三张表 + 状态日志）
        assert_eq!(sheet_count().await, 0, "试算不许写单头");
        assert_eq!(detail_count().await, 0, "试算不许写明细");
        assert_eq!(detail_item_count().await, 0, "试算不许写明细项");
        assert_eq!(
            engine_mod::seq_current_no(db()).await.expect("读号段"),
            seq_before,
            "试算不许推进 biz_seq(SAL)（试算 100 次会把号段跑光）"
        );

        cleanup(&fixture).await;
    }

    // ---- ② 落库：事务内三张表 + 恒等式在库里成立 ----

    #[tokio::test]
    async fn persist_writes_header_details_and_items_with_identities() {
        let Some(fixture) = setup().await else { return };
        let items = fixture_items().await;
        let mut req = CalcRequest::persist(PERIOD, Some(fixture.period_id));
        req.manual = fixture.zhangsan_manual();
        req.remark = Some("HR-34 真库用例".to_string());

        let outcome = engine_mod::calc_with_items(fixture.operator.clone(), &req, &items)
            .await
            .expect("建单必须成功");
        let sheet = outcome.sheet.clone().expect("非试算必须建单");
        assert_eq!(
            sheet.status, STATUS_CALCULATED,
            "试算完就是 CALCULATED（DRAFT→CALCULATED）"
        );
        assert!(
            sheet.sheet_no.starts_with("SAL-202611-"),
            "单号形态 SAL-yyyyMM-NNNNNN：{}",
            sheet.sheet_no
        );

        // 单头
        let header = db()
            .query_first_sql(
                "SELECT sheet_no, period_code, period_start, period_end, status, employee_count, \
                        earning_total, pre_deduct_total, gross_total, withhold_total, net_total, \
                        si_company_total, hf_company_total, attendance_period_id, calculated_at \
                   FROM biz_salary_sheet WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&sheet.id)],
            )
            .await
            .expect("读单头")
            .expect("单头必须存在");
        assert_eq!(header.get_typed::<String>("status").expect("status"), STATUS_CALCULATED);
        assert_eq!(header.get_typed::<i32>("employee_count").expect("人数"), 2);
        let totals = CalcTotals {
            employee_count: 2,
            earning_total: header.get_typed("earning_total").expect("earning"),
            pre_deduct_total: header.get_typed("pre_deduct_total").expect("pre"),
            gross_total: header.get_typed("gross_total").expect("gross"),
            withhold_total: header.get_typed("withhold_total").expect("withhold"),
            net_total: header.get_typed("net_total").expect("net"),
            si_company_total: header.get_typed("si_company_total").expect("si"),
            hf_company_total: header.get_typed("hf_company_total").expect("hf"),
        };
        assert_eq!(totals.gross_total, amount("9154.03"), "单头应发");
        assert_eq!(totals.net_total, amount("7354.03"), "单头实发");
        assert_eq!(totals.withhold_total, amount("1800"), "单头代扣");
        assert_eq!(
            totals.accrual_total(),
            amount("11794.03"),
            "计提总额 = 应发 + 单位社保 + 单位公积金"
        );
        assert!(
            header
                .get_typed::<Option<DateTime<Utc>>>("calculated_at")
                .expect("calculated_at")
                .is_some(),
            "calculated_at 必须落"
        );

        // 明细两行
        assert_eq!(detail_count_of(sheet.id).await, 2);
        // 明细项 2 人 × 9 项
        assert_eq!(detail_item_count_of(sheet.id).await, 18);

        // 逐行恒等式（库层 CHECK 已经保证，这里再证明「我们写进去的就是这个数」）
        let rows = db()
            .query_sql(
                "SELECT employee_id, employee_no, employee_name, should_days, actual_days, leave_unpaid_days, \
                        ot_hours, earning_amount, pre_deduct_amount, gross_amount, withhold_amount, net_amount \
                   FROM biz_salary_detail WHERE sheet_id = ?::uuid ORDER BY employee_no"
                    .to_string(),
                vec![Value::from(&sheet.id)],
            )
            .await
            .expect("读明细");
        assert_eq!(rows.len(), 2);
        let zhangsan = rows
            .iter()
            .find(|row| row.get_typed::<Uuid>("employee_id").expect("id") == fixture.emp_a)
            .expect("张三的明细");
        assert_eq!(
            zhangsan.get_typed::<String>("employee_no").expect("工号"),
            fixture.emp_a_no()
        );
        assert_eq!(zhangsan.get_typed::<String>("employee_name").expect("姓名"), "张三");
        assert_eq!(
            zhangsan.get_typed::<Decimal>("should_days").expect("应出勤"),
            amount("22")
        );
        assert_eq!(
            zhangsan.get_typed::<Decimal>("actual_days").expect("实出勤"),
            amount("20")
        );
        assert_eq!(
            zhangsan.get_typed::<Decimal>("ot_hours").expect("加班小时"),
            amount("10")
        );
        assert_eq!(
            zhangsan.get_typed::<Decimal>("gross_amount").expect("应发"),
            amount("9154.03")
        );
        assert_eq!(
            zhangsan.get_typed::<Decimal>("net_amount").expect("实发"),
            amount("7354.03")
        );

        // A7：Σ 明细项（按类型） = 明细行
        let item_sum: Decimal = db()
            .query_first_sql(
                "SELECT COALESCE(SUM(CASE WHEN item_type = 'EARNING' THEN amount END), 0) \
                      - COALESCE(SUM(CASE WHEN item_type = 'PRE_DEDUCT' THEN amount END), 0) AS n \
                   FROM biz_salary_detail_item WHERE sheet_id = ?::uuid AND detail_id = \
                        (SELECT id FROM biz_salary_detail WHERE sheet_id = ?::uuid AND employee_id = ?::uuid)"
                    .to_string(),
                vec![
                    Value::from(&sheet.id),
                    Value::from(&sheet.id),
                    Value::from(&fixture.emp_a),
                ],
            )
            .await
            .expect("A7 求和")
            .expect("一行")
            .get_typed::<Decimal>("n")
            .expect("n");
        assert_eq!(item_sum, amount("9154.03"), "A7：Σ EARNING − Σ PRE_DEDUCT = 应发");

        // 明细项：公式快照 + 代入说明 + line_no 连续
        let ot = db()
            .query_first_sql(
                "SELECT amount, expr_snapshot, calc_note, line_no FROM biz_salary_detail_item \
                  WHERE sheet_id = ?::uuid AND item_code = 'HR34OT_WD' AND detail_id = \
                        (SELECT id FROM biz_salary_detail WHERE sheet_id = ?::uuid AND employee_id = ?::uuid)"
                    .to_string(),
                vec![
                    Value::from(&sheet.id),
                    Value::from(&sheet.id),
                    Value::from(&fixture.emp_a),
                ],
            )
            .await
            .expect("读明细项")
            .expect("OT 项必须落库");
        assert_eq!(ot.get_typed::<Decimal>("amount").expect("加班费"), amount("689.66"));
        assert!(
            ot.get_typed::<String>("expr_snapshot")
                .expect("公式快照")
                .contains("ATT_OT_WD"),
            "公式原文要存快照（改公式不改历史）"
        );
        assert!(ot.get_typed::<String>("calc_note").expect("说明").contains("689.66"));
        assert!(ot.get_typed::<i32>("line_no").expect("行号") > 0);

        // SUMMARY 项不落明细项（11 项里 9 项落地）
        assert_eq!(
            count(
                "SELECT count(*) AS n FROM biz_salary_detail_item WHERE sheet_id = ?::uuid \
                  AND item_code IN ('HR34GROSS', 'HR34NET')",
                vec![Value::from(&sheet.id)],
            )
            .await,
            0,
            "SUMMARY 项不许落明细项（值在 detail 的 gross/net 列上）"
        );

        // 状态日志：建单 + DRAFT→CALCULATED 两条
        assert!(
            count(
                "SELECT count(*) AS n FROM biz_status_log \
                  WHERE entity_type = 'SALARY_SHEET' AND entity_id = ?::uuid",
                vec![Value::from(&sheet.id)],
            )
            .await
                >= 2,
            "建单日志 + 试算流转日志都要有"
        );

        // 号段推进了 1
        assert!(engine_mod::seq_current_no(db()).await.expect("读号段").unwrap_or(0) > 0);

        cleanup(&fixture).await;
    }

    // ---- ③ 同期间第二张单：409，且不留第二行 ----

    #[tokio::test]
    async fn duplicate_period_is_rejected_with_the_existing_no() {
        let Some(fixture) = setup().await else { return };
        let items = fixture_items().await;
        let req = CalcRequest::persist(PERIOD, Some(fixture.period_id));
        let first = engine_mod::calc_with_items(fixture.operator.clone(), &req, &items)
            .await
            .expect("第一张要成功");
        let first_no = first.sheet.clone().expect("单号").sheet_no;

        let error = engine_mod::calc_with_items(fixture.operator.clone(), &req, &items)
            .await
            .expect_err("同期间第二张必须 409");
        assert_eq!(error.status, 409, "{error:?}");
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains(&first_no), "报文要点名已有单号：{}", error.msg);
        assert_eq!(sheet_count().await, 1, "不许写出第二张单头");

        cleanup(&fixture).await;
    }

    // ---- ④ 验收第 3 条：试算与正式计算同源 ----

    #[tokio::test]
    async fn preview_and_persist_produce_the_same_numbers() {
        let Some(fixture) = setup().await else { return };
        let items = fixture_items().await;
        let mut preview_req = CalcRequest::preview(PERIOD, Some(fixture.period_id));
        preview_req.manual = fixture.zhangsan_manual();
        let preview = engine_mod::calc_with_items(fixture.operator.clone(), &preview_req, &items)
            .await
            .expect("先试算");

        let mut persist_req = CalcRequest::persist(PERIOD, Some(fixture.period_id));
        persist_req.manual = fixture.zhangsan_manual();
        let persisted = engine_mod::calc_with_items(fixture.operator.clone(), &persist_req, &items)
            .await
            .expect("再落库");

        assert_eq!(preview.totals, persisted.totals, "合计必须一分不差");
        assert_eq!(preview.slips.len(), persisted.slips.len());
        for (before, after) in preview.slips.iter().zip(persisted.slips.iter()) {
            assert_eq!(before.head.id, after.head.id);
            assert_eq!(before.gross_amount, after.gross_amount, "{} 应发", before.head.emp_no);
            assert_eq!(before.net_amount, after.net_amount, "{} 实发", before.head.emp_no);
            let amounts: Vec<Decimal> = before.items.iter().map(|item| item.amount).collect();
            let persisted_amounts: Vec<Decimal> = after.items.iter().map(|item| item.amount).collect();
            assert_eq!(amounts, persisted_amounts, "逐项金额必须一致");
        }

        // 库里再验一遍（A6：单头 = Σ 明细）
        let sheet = persisted.sheet.clone().expect("单号");
        let header_gross: Decimal = db()
            .query_first_sql(
                "SELECT gross_total AS n FROM biz_salary_sheet WHERE id = ?::uuid".to_string(),
                vec![Value::from(&sheet.id)],
            )
            .await
            .expect("读单头")
            .expect("一行")
            .get_typed("n")
            .expect("gross_total");
        let detail_gross: Decimal = db()
            .query_first_sql(
                "SELECT COALESCE(SUM(gross_amount), 0) AS n FROM biz_salary_detail WHERE sheet_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&sheet.id)],
            )
            .await
            .expect("读明细合计")
            .expect("一行")
            .get_typed("n")
            .expect("gross");
        assert_eq!(header_gross, detail_gross, "A6：单头应发 = Σ 明细应发");

        cleanup(&fixture).await;
    }

    // ---- ⑤ 数据问题在写库之前被拦 ----

    #[tokio::test]
    async fn overlapping_standard_blocks_the_run_before_writing() {
        let Some(fixture) = setup().await else { return };
        // 给张三再插一条同项、区间重叠的标准
        db().exec_sql(
            "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'HR34BASE', ?::text::numeric, ?::text::date)"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&fixture.emp_a),
                Value::Text("9000".to_string()),
                Value::Text("2026-11-01".to_string()),
            ],
        )
        .await
        .expect("插重叠标准");

        let items = fixture_items().await;
        let req = CalcRequest::persist(PERIOD, Some(fixture.period_id));
        let error = engine_mod::calc_with_items(fixture.operator.clone(), &req, &items)
            .await
            .expect_err("区间重叠必须拦");
        assert_eq!(error.status, 400, "{error:?}");
        assert!(error.msg.contains("HR34BASE"), "{}", error.msg);
        assert!(
            error.msg.contains("区间重叠") || error.msg.contains("生效区间"),
            "{}",
            error.msg
        );
        assert_eq!(sheet_count().await, 0, "参数问题不许落半张单");

        // 人工项编码不存在同样在写库前拦
        let mut bad = CalcRequest::persist(PERIOD, Some(fixture.period_id));
        bad.manual = vec![ManualEntry {
            employee_id: fixture.emp_a,
            item_code: "HR34NOPE".to_string(),
            amount: amount("1"),
        }];
        let error = engine_mod::calc_with_items(fixture.operator.clone(), &bad, &items)
            .await
            .expect_err("错编码必须拦");
        assert!(error.msg.contains("HR34NOPE"), "{}", error.msg);
        assert_eq!(sheet_count().await, 0);

        cleanup(&fixture).await;
    }

    // ---- ⑥ 没有薪资项 / 期间不一致 ----

    #[tokio::test]
    async fn empty_item_table_is_refused_and_period_mismatch_is_named() {
        let Some(fixture) = setup().await else { return };

        let error = engine_mod::calc_with_items(fixture.operator.clone(), &CalcRequest::preview(PERIOD, None), &[])
            .await
            .expect_err("没有薪资项必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("薪资项"), "{}", error.msg);

        // 薪资期间与考勤期间不一致 → 点名两个期间（按 11 月的工资算 10 月的考勤）
        let items = fixture_items().await;
        let error = engine_mod::calc_with_items(
            fixture.operator.clone(),
            &CalcRequest::preview("2026-10", Some(fixture.period_id)),
            &items,
        )
        .await
        .expect_err("期间不一致必须拦");
        assert_eq!(error.status, 400, "{error:?}");
        assert!(
            error.msg.contains("2026-10") && error.msg.contains(PERIOD),
            "{}",
            error.msg
        );

        cleanup(&fixture).await;
    }

    // ---- 计数小工具（都按 org + 期间收口，避免别的卡的夹具干扰） ----

    async fn sheet_count() -> i64 {
        count(
            "SELECT count(*) AS n FROM biz_salary_sheet WHERE org_id = ?::uuid AND period_code = ?",
            vec![Value::from(&org()), Value::Text(PERIOD.to_string())],
        )
        .await
    }

    async fn detail_count() -> i64 {
        count(
            "SELECT count(*) AS n FROM biz_salary_detail d JOIN biz_salary_sheet s ON s.id = d.sheet_id \
              WHERE s.org_id = ?::uuid AND s.period_code = ?",
            vec![Value::from(&org()), Value::Text(PERIOD.to_string())],
        )
        .await
    }

    async fn detail_item_count() -> i64 {
        count(
            "SELECT count(*) AS n FROM biz_salary_detail_item i JOIN biz_salary_sheet s ON s.id = i.sheet_id \
              WHERE s.org_id = ?::uuid AND s.period_code = ?",
            vec![Value::from(&org()), Value::Text(PERIOD.to_string())],
        )
        .await
    }

    async fn detail_count_of(sheet_id: Uuid) -> i64 {
        count(
            "SELECT count(*) AS n FROM biz_salary_detail WHERE sheet_id = ?::uuid",
            vec![Value::from(&sheet_id)],
        )
        .await
    }

    async fn detail_item_count_of(sheet_id: Uuid) -> i64 {
        count(
            "SELECT count(*) AS n FROM biz_salary_detail_item WHERE sheet_id = ?::uuid",
            vec![Value::from(&sheet_id)],
        )
        .await
    }
}
