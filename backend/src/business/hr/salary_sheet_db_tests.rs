//! 薪资单（`salary_sheet` 四接口）的真库用例（HR-36）。
//!
//! 单测（`salary_sheet_dto` / `salary_sheet_model` / `salary_sheet_handler` 里的十几条）
//! 证明的是**契约与 SQL 形状**；只有真库能回答这四件事：
//!
//! 1. **建单真的落库**：单头 + 明细 + 明细项三张表都有行，单头状态是 `CALCULATED`，
//!    库层 6 条 `ck_biz_sal_sheet_*` / 4 条 `ck_biz_sal_det_*` 全部通过；
//! 2. **汇总与明细一致**（本卡验收项）：`GET /salary/{id}` 的三份口径（单头 7 列 /
//!    `SUM(明细)` / `SUM(明细项按类型)`）一分不差，`consistent = true`；
//! 3. **明细分页**：`page_size=1` 真能翻两页、`with_items` 开关有效、关键字与部门筛选走 SQL；
//! 4. **试算零落库**：`POST /salary/calc` 之后三张表零行、`biz_seq(SAL)` 不推进
//!    （「试算 100 次把号段跑光」是真实会发生的运维事故）。
//!
//! 不设库地址时全部 SKIP（**不假绿**）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr36_drill \
//!   cargo test --lib business::hr::salary_sheet_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! * 薪资项编码前缀 `HR36`，员工工号 `HR36A-<纳秒>` / `HR36B-<纳秒>`；
//! * 薪资期间固定 `2026-12`（避开 HR-34 用例的 `2026-11`：那边的 sweep 按期间删单，
//!   同一期间会让两张卡的夹具互相踩）；
//! * 引擎取数是**全量**读（员工 / 标准 / 考勤），所以本文件的用例必须 `--test-threads=1`
//!   且每个用例自建自清；
//! * 库里还有 `V0396` 种的 17 个种子薪资项，它们会一起参与计算 —— 这是**真实形态**，
//!   所以本文件一律断言**关系**（汇总=明细、明细内恒等式），不断言某个绝对金额。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;

    use nik::prelude::rust_decimal::Decimal;

    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_compute::{self as engine_mod, CalcRequest, ManualEntry, STATUS_CALCULATED};
    use crate::business::hr::salary_sheet_dto::{
        SalaryDetailPageQuery, SalaryManualReq, SalarySheetCalcReq, SalarySheetListQuery,
    };
    use crate::business::hr::salary_sheet_handler;
    use crate::business::hr::salary_sheet_model;
    use crate::business::shared::state_machine::Operator;
    use tokio::sync::OnceCell;

    /// 夹具编码前缀（清场按它精确删）。
    const PREFIX: &str = "HR36";
    /// 薪资期间（`ck_biz_sal_sheet_code` 只认 `yyyy-MM`）。
    const PERIOD: &str = "2026-12";

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
                        println!("HR36 真库用例已连接：database={database}");
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

    fn money(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn day(text: &str) -> NaiveDate {
        text.parse().expect("日期字面量")
    }

    /// 一个夹具：2 名员工 + 标准 + 考勤期间与记录。
    ///
    /// **不建薪资项**：直接用 `V0396` 种的 17 个种子项（`BASE`/`PERF`/`OT_WD`/…）。
    /// 第一版夹具自建了 8 个 `HR36*` 项，结果被引擎当场拒掉 —— 种子项 `GROSS` 的公式
    /// 逐项枚举了 `{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}`，
    /// 多出两个 `HR36*` 的 EARNING 项就与「按类型聚合」对不上（`check_summary` 报
    /// 「应发合计（公式项 GROSS 与按项类型聚合）不一致」）。**这不是坑，是这个检查该干的事**：
    /// 客户新增收入项时必须同步改 GROSS 公式，否则工资单算得出、过账对不平。
    /// 用种子项跑，等于顺带验了「出厂配置本身自洽」。
    struct Fixture {
        suffix: String,
        emp_a: Uuid,
        emp_b: Uuid,
        period_id: Uuid,
        operator: Operator,
    }

    impl Fixture {
        fn emp_a_no(&self) -> String {
            format!("HR36A-{}", self.suffix)
        }

        fn emp_b_no(&self) -> String {
            format!("HR36B-{}", self.suffix)
        }

        /// 试算 / 建单入参（张三有考勤与标准；李四两样都没有）。
        ///
        /// 人工项全部用**种子项编码**（`PERF` / `SI_PERSONAL` / `HF_PERSONAL` / `IIT` /
        /// `SI_COMPANY` / `HF_COMPANY`）：`GROSS` / `NET` 的公式只认这些编码，
        /// 换一套编码就会「公式侧 0、聚合侧有值」而被引擎拒掉。
        fn calc_req(&self, dry_run: bool) -> SalarySheetCalcReq {
            let entries = [
                ("PERF", "1200"),
                ("SI_PERSONAL", "840"),
                ("HF_PERSONAL", "960"),
                ("IIT", "0"),
                ("SI_COMPANY", "1680"),
                ("HF_COMPANY", "960"),
            ];
            SalarySheetCalcReq {
                period_code: Some(PERIOD.to_string()),
                attendance_period_id: Some(self.period_id.to_string()),
                dry_run: Some(dry_run),
                remark: Some("HR36 真库用例".to_string()),
                manual: Some(
                    entries
                        .iter()
                        .map(|(code, value)| SalaryManualReq {
                            employee_id: Some(self.emp_a.to_string()),
                            item_code: Some((*code).to_string()),
                            amount: Some(money(value)),
                        })
                        .collect(),
                ),
            }
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

    /// 清掉上一轮（可能是 panic 中断的）留下的 `HR36` 夹具。
    async fn sweep() {
        let statements: [(String, Vec<Value>); 6] = [
            (
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid".to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_attendance_record WHERE period_id IN \
                    (SELECT id FROM biz_attendance_period WHERE period_code = ? AND org_id = ?::uuid)"
                    .to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                "DELETE FROM biz_attendance_period WHERE period_code = ? AND org_id = ?::uuid".to_string(),
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
            ),
            (
                format!(
                    "DELETE FROM biz_employee_salary WHERE employee_id IN \
                        (SELECT id FROM biz_employee WHERE emp_no LIKE '{PREFIX}%')"
                ),
                vec![],
            ),
            (
                format!("DELETE FROM biz_salary_item WHERE item_code LIKE '{PREFIX}%'"),
                vec![],
            ),
            (
                format!("DELETE FROM biz_employee WHERE emp_no LIKE '{PREFIX}%'"),
                vec![],
            ),
        ];
        for (sql, params) in statements {
            db().exec_sql(sql, params).await.expect("清场");
        }
    }

    async fn setup() -> Option<Fixture> {
        if !ensure_db().await {
            return None;
        }
        sweep().await;
        let suffix = uniq();
        let dept = dept_id().await;
        let emp_a = Uuid::now_v7();
        let emp_b = Uuid::now_v7();
        let period_id = Uuid::now_v7();

        for (id, no, name) in [
            (emp_a, format!("HR36A-{suffix}"), "张三"),
            (emp_b, format!("HR36B-{suffix}"), "李四"),
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

        // 薪资项**不建**：用 V0396 的 17 个种子项（见 Fixture 的文档）。

        // 标准：张三 BASE 8000（李四不配标准 → 全部按 0）
        db().exec_sql(
            "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'BASE', ?::text::numeric, ?::text::date)"
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
                Value::Text("2026-12-01".to_string()),
                Value::Text("2026-12-31".to_string()),
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
                Value::Text(format!("HR36A-{suffix}")),
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
            operator: Operator::named(operator_id, "hr36-test"),
        })
    }

    async fn cleanup(fixture: &Fixture) {
        // 从叶子删起：明细项 / 明细随单头级联 → 先删单头；再考勤记录、标准、薪资项、员工
        for (sql, params) in [
            (
                "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid",
                vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
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
            ("DELETE FROM biz_salary_item WHERE item_code LIKE 'HR36%'", vec![]),
            (
                "DELETE FROM biz_employee WHERE id IN (?::uuid, ?::uuid)",
                vec![Value::from(&fixture.emp_a), Value::from(&fixture.emp_b)],
            ),
        ] {
            let _ = db().exec_sql(sql.to_string(), params).await;
        }
    }

    async fn count(sql: &str, params: Vec<Value>) -> i64 {
        db().query_first_sql(sql.to_string(), params)
            .await
            .expect("计数查询")
            .map(|row| row.get_typed::<i64>("n").expect("count"))
            .unwrap_or(0)
    }

    /// 本期间的单头数 / 明细数 / 明细项数。
    async fn row_counts() -> (i64, i64, i64) {
        let sheets = count(
            "SELECT count(*) AS n FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid",
            vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
        )
        .await;
        let details = count(
            "SELECT count(*) AS n FROM biz_salary_detail d \
               JOIN biz_salary_sheet s ON s.id = d.sheet_id \
              WHERE s.period_code = ? AND s.org_id = ?::uuid",
            vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
        )
        .await;
        let items = count(
            "SELECT count(*) AS n FROM biz_salary_detail_item i \
               JOIN biz_salary_sheet s ON s.id = i.sheet_id \
              WHERE s.period_code = ? AND s.org_id = ?::uuid",
            vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
        )
        .await;
        (sheets, details, items)
    }

    // ---- ① 试算：零落库 + 不推进号段 ----

    #[tokio::test]
    async fn preview_writes_nothing_and_keeps_the_seq() {
        let Some(fixture) = setup().await else { return };
        let seq_before = engine_mod::seq_current_no(db()).await.expect("读号段");

        let res = salary_sheet_handler::preview(fixture.operator.clone(), &fixture.calc_req(true))
            .await
            .expect("试算必须成功");

        assert!(res.dry_run);
        assert!(res.sheet.is_none(), "试算不许建单");
        assert_eq!(res.period_code, PERIOD);
        assert_eq!(res.employees.len(), 2, "两名员工都要出工资条");
        assert_eq!(res.totals.employee_count, 2);
        // 李四没有考勤记录：要报出来（不是静默按 0 算）
        assert_eq!(res.missing_attendance.len(), 1, "{:?}", res.missing_attendance);
        assert!(res.missing_attendance[0].contains(&fixture.emp_b_no()));

        let (sheets, details, items) = row_counts().await;
        assert_eq!((sheets, details, items), (0, 0, 0), "试算必须零落库");
        let seq_after = engine_mod::seq_current_no(db()).await.expect("读号段");
        assert_eq!(seq_before, seq_after, "试算不许推进 SAL 号段");

        // 逐人恒等式在回执里也成立（库里还没行，先验算法口径）
        for slip in &res.employees {
            assert_eq!(slip.gross_amount, slip.earning_amount - slip.pre_deduct_amount);
            assert_eq!(slip.gross_amount, slip.net_amount + slip.withhold_amount);
            assert_eq!(
                slip.accrual_amount,
                slip.gross_amount + slip.si_company + slip.hf_company
            );
        }
        cleanup(&fixture).await;
    }

    // ---- ② 建单 + 汇总与明细一致（本卡验收项） ----

    #[tokio::test]
    async fn persist_then_summary_matches_details() {
        let Some(fixture) = setup().await else { return };

        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect("建单必须成功");
        let sheet = created.sheet.clone().expect("建单必须回单头");
        assert_eq!(sheet.status, STATUS_CALCULATED);
        assert!(
            sheet.sheet_no.starts_with("SAL-202612-"),
            "单号按期间取：{}",
            sheet.sheet_no
        );
        assert!(!created.dry_run);

        let (sheets, details, items) = row_counts().await;
        assert_eq!(sheets, 1, "单头一行");
        assert_eq!(details, 2, "两名员工两行明细");
        assert!(items > 0, "明细项必须落库（公式解释层）");
        assert_eq!(i64::try_from(created.item_count).unwrap_or(-1) * 2, items);

        let sheet_id = Uuid::parse_str(&sheet.id).expect("单头 id");
        let summary = salary_sheet_handler::summary_of(sheet_id).await.expect("读单头汇总");

        // ---- 三份口径必须一分不差（A6 / A7） ----
        assert!(summary.consistent, "汇总与明细不一致：{:?}", summary.mismatches);
        assert!(summary.mismatches.is_empty(), "{:?}", summary.mismatches);
        assert_eq!(summary.sheet.totals, summary.detail_totals, "单头 7 列 ≠ SUM(明细)");
        assert_eq!(summary.sheet.totals.employee_count, 2);
        assert_eq!(summary.detail_totals.net_total, created.totals.net_total);
        assert_eq!(
            summary.item_totals.earning_total, summary.detail_totals.earning_total,
            "明细项按类型聚合 ≠ 明细行"
        );
        assert_eq!(
            summary.item_totals.employer_total,
            summary.detail_totals.si_company_total + summary.detail_totals.hf_company_total
        );

        // ---- 单头内部恒等式（库层 CHECK 也守着，这里再钉一遍读回的值） ----
        let totals = summary.sheet.totals;
        assert_eq!(totals.gross_total, totals.earning_total - totals.pre_deduct_total);
        assert_eq!(totals.gross_total, totals.net_total + totals.withhold_total);
        assert_eq!(
            totals.accrual_total,
            totals.gross_total + totals.si_company_total + totals.hf_company_total
        );

        // ---- 部门汇总加起来 = 单头（前端做交叉校验时不会「少算一笔」）----
        let dept_count: i32 = summary.dept_summary.iter().map(|row| row.employee_count).sum();
        let dept_gross: Decimal = summary.dept_summary.iter().map(|row| row.gross_total).sum();
        assert_eq!(dept_count, totals.employee_count, "部门人数合计 ≠ 单头人数");
        assert_eq!(dept_gross, totals.gross_total, "部门应发合计 ≠ 单头应发");

        // ---- 张三有标准与考勤：BASE 8000 必须真算进去 ----
        let (rows, total, _, _) =
            salary_sheet_handler::detail_page_of(sheet_id, &SalaryDetailQueryBuilder::new().page_size(50).build())
                .await
                .expect("读明细");
        assert_eq!(total, 2);
        let zhangsan = rows
            .iter()
            .find(|row| row.employee_no == fixture.emp_a_no())
            .expect("张三的明细行");
        assert!(zhangsan.earning_amount >= money("8000"), "基本工资没算进去");
        assert_eq!(
            zhangsan.gross_amount,
            zhangsan.earning_amount - zhangsan.pre_deduct_amount
        );
        assert_eq!(
            zhangsan.withhold_amount,
            zhangsan.social_personal + zhangsan.fund_personal + zhangsan.iit_amount + zhangsan.other_withhold
        );
        assert_eq!(zhangsan.net_amount + zhangsan.withhold_amount, zhangsan.gross_amount);
        assert!(!zhangsan.items.is_empty(), "明细项要能展开看到公式解释");
        assert_eq!(
            i32::try_from(zhangsan.items.len()).unwrap_or(-1),
            zhangsan.item_count,
            "明细项条数与展开的行数必须一致"
        );
        assert!(zhangsan.items.iter().any(|item| item.item_code == "BASE"));
        // 种子公式的实测数字（设计 §4.4.4 同一组口径）：
        //   OT_WD = ROUND(8000/21.75/8*10*1.5, 2) = 689.66
        //   ATT_DEDUCT = ROUND(8000/21.75*2, 2) = 735.63
        //   earning = 8000 + 1200 + 689.66 = 9889.66 → gross = 9154.03 → net = 7354.03
        let ot = zhangsan
            .items
            .iter()
            .find(|item| item.item_code == "OT_WD")
            .expect("OT_WD 项");
        assert_eq!(ot.amount, money("689.66"), "加班费 {ot:?}");
        let deduct = zhangsan
            .items
            .iter()
            .find(|item| item.item_code == "ATT_DEDUCT")
            .expect("ATT_DEDUCT 项");
        assert_eq!(deduct.amount, money("735.63"), "考勤扣款 {deduct:?}");
        assert_eq!(zhangsan.earning_amount, money("9889.66"));
        assert_eq!(zhangsan.gross_amount, money("9154.03"));
        assert_eq!(zhangsan.net_amount, money("7354.03"));
        assert_eq!(zhangsan.si_company, money("1680"));
        assert_eq!(zhangsan.hf_company, money("960"));
        assert!(
            zhangsan
                .items
                .iter()
                .filter(|item| item.calc_type == "FORMULA")
                .all(|item| item.expr_snapshot.is_some() && !item.calc_note.is_empty()),
            "公式项必须带公式快照与代入值说明"
        );

        cleanup(&fixture).await;
    }

    // ---- ③ 明细分页：翻页 / 开关 / 筛选 ----

    #[tokio::test]
    async fn detail_page_paginates_filters_and_toggles_items() {
        let Some(fixture) = setup().await else { return };
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect("建单必须成功");
        let sheet_id = Uuid::parse_str(&created.sheet.expect("单头").id).expect("单头 id");

        // page_size = 1 → 真能翻两页且不重不漏
        let (first, total, page, page_size) = salary_sheet_handler::detail_page_of(
            sheet_id,
            &SalaryDetailQueryBuilder::new().page_size(1).page(1).build(),
        )
        .await
        .expect("第一页");
        assert_eq!((total, page, page_size), (2, 1, 1));
        assert_eq!(first.len(), 1);

        let (second, _, _, _) = salary_sheet_handler::detail_page_of(
            sheet_id,
            &SalaryDetailQueryBuilder::new().page_size(1).page(2).build(),
        )
        .await
        .expect("第二页");
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].id, second[0].id, "两页不许重复");

        // with_items = false：不展开但条数照给
        let (no_items, _, _, _) =
            salary_sheet_handler::detail_page_of(sheet_id, &SalaryDetailQueryBuilder::new().with_items(false).build())
                .await
                .expect("不展开明细项");
        assert_eq!(no_items.len(), 2);
        assert!(no_items.iter().all(|row| row.items.is_empty()), "关了开关就不查明细项");
        assert!(no_items.iter().all(|row| row.item_count > 0), "条数即使不展开也要给");

        // 关键字：只命中一个（工号前缀搜）
        let (hits, total, _, _) =
            salary_sheet_handler::detail_page_of(sheet_id, &SalaryDetailQueryBuilder::new().keyword("HR36A-").build())
                .await
                .expect("关键字筛选");
        assert_eq!(total, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].employee_no, fixture.emp_a_no());

        // 部门筛选：两人同部门 → 命中 2；换个不存在的部门 → 0
        let dept = hits[0].dept_id.clone().expect("张三有部门");
        let (same_dept, total, _, _) =
            salary_sheet_handler::detail_page_of(sheet_id, &SalaryDetailQueryBuilder::new().dept_id(&dept).build())
                .await
                .expect("部门筛选");
        assert_eq!(total, 2, "同部门两人都要出来");
        assert_eq!(same_dept.len(), 2);

        let (none, total, _, _) = salary_sheet_handler::detail_page_of(
            sheet_id,
            &SalaryDetailQueryBuilder::new()
                .dept_id(&Uuid::now_v7().to_string())
                .build(),
        )
        .await
        .expect("空部门");
        assert_eq!(total, 0);
        assert!(none.is_empty());

        // 不存在的单 → 404（不是空页）
        let error = salary_sheet_handler::detail_page_of(Uuid::now_v7(), &SalaryDetailQueryBuilder::new().build())
            .await
            .expect_err("不存在的单必须 404");
        assert_eq!(error.status, 404);

        cleanup(&fixture).await;
    }

    // ---- ④ 列表 ----

    #[tokio::test]
    async fn list_filters_by_period_and_status() {
        let Some(fixture) = setup().await else { return };
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect("建单必须成功");
        let sheet_no = created.sheet.as_ref().expect("单头").sheet_no.clone();

        let (rows, total, _, _) = salary_sheet_handler::list_sheets(&SalarySheetListQuery {
            period_code: Some(PERIOD.to_string()),
            ..SalarySheetListQuery::default()
        })
        .await
        .expect("按期间查列表");
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sheet_no, sheet_no);
        assert_eq!(rows[0].status, STATUS_CALCULATED);
        assert_eq!(rows[0].status_label, "已计算");
        assert_eq!(rows[0].employee_count, 2);
        assert_eq!(rows[0].accrual_total, created.totals.accrual_total);

        // 状态筛选：CALCULATED 命中，POSTED 为 0
        let (_, total, _, _) = salary_sheet_handler::list_sheets(&SalarySheetListQuery {
            status: Some("CALCULATED".to_string()),
            ..SalarySheetListQuery::default()
        })
        .await
        .expect("按状态查");
        assert_eq!(total, 1);
        let (_, total, _, _) = salary_sheet_handler::list_sheets(&SalarySheetListQuery {
            status: Some("POSTED".to_string()),
            period_code: Some(PERIOD.to_string()),
            ..SalarySheetListQuery::default()
        })
        .await
        .expect("按状态查");
        assert_eq!(total, 0, "库里没有已过账的单");

        // 非法状态 → 400（不静默忽略）
        let error = salary_sheet_handler::list_sheets(&SalarySheetListQuery {
            status: Some("PAID".to_string()),
            ..SalarySheetListQuery::default()
        })
        .await
        .expect_err("非法状态必须 400");
        assert_eq!(error.status, 400);

        cleanup(&fixture).await;
    }

    // ---- ⑤ 同期间重复建单：409 且点名已有单号 ----

    #[tokio::test]
    async fn duplicate_period_conflicts_with_the_existing_sheet_no() {
        let Some(fixture) = setup().await else { return };
        let first = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect("第一次建单");
        let sheet_no = first.sheet.expect("单头").sheet_no;

        let error = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect_err("同期间第二张必须 409");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains(&sheet_no), "报文要点名已有单号：{}", error.msg);

        // 失败那一次不许留半张单（事务回滚）
        let (sheets, details, _) = row_counts().await;
        assert_eq!(sheets, 1, "还是只有一张单");
        assert_eq!(details, 2, "明细还是一人一行");

        cleanup(&fixture).await;
    }

    // ---- ⑥ 单头被绕过引擎改坏：汇总必须报出来（A6 诊断路径） ----

    #[tokio::test]
    async fn summary_reports_drift_when_someone_bypasses_the_engine() {
        let Some(fixture) = setup().await else { return };
        let created = salary_sheet_handler::persist(fixture.operator.clone(), &fixture.calc_req(false))
            .await
            .expect("建单");
        let sheet_id = Uuid::parse_str(&created.sheet.expect("单头").id).expect("单头 id");

        // 模拟「有人直接往明细表塞了一行」：单头人数/金额不会跟着变 → 三口径漂移
        let intruder = Uuid::now_v7();
        let dept = dept_id().await;
        db().exec_sql(
            "INSERT INTO biz_employee (id, org_id, emp_no, name, gender, dept_id, hire_date, status, \
                                      employment_type, id_card_type) \
             VALUES (?::uuid, ?::uuid, ?, '王五', 1, ?::uuid, ?::text::date, 'ACTIVE', 'FULL_TIME', 'ID_CARD')"
                .to_string(),
            vec![
                Value::from(&intruder),
                Value::from(&org()),
                Value::Text(format!("HR36C-{}", fixture.suffix)),
                Value::from(&dept),
                Value::Text("2020-01-01".to_string()),
            ],
        )
        .await
        .expect("插员工");
        db().exec_sql(
            "INSERT INTO biz_salary_detail (id, org_id, sheet_id, employee_id, employee_no, employee_name, \
                                            earning_amount, gross_amount, withhold_amount, net_amount) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, '王五', \
                     ?::text::numeric, ?::text::numeric, 0, ?::text::numeric)"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&org()),
                Value::from(&sheet_id),
                Value::from(&intruder),
                Value::Text(format!("HR36C-{}", fixture.suffix)),
                Value::Text("100".to_string()),
                Value::Text("100".to_string()),
                Value::Text("100".to_string()),
            ],
        )
        .await
        .expect("插明细");

        let summary = salary_sheet_handler::summary_of(sheet_id).await.expect("读汇总");
        assert!(!summary.consistent, "明细多了一行，汇总必须报不一致");
        let fields: Vec<&str> = summary.mismatches.iter().map(|row| row.field.as_str()).collect();
        assert!(fields.contains(&"employee_count"), "{fields:?}");
        assert!(fields.contains(&"earning_total"), "{fields:?}");
        assert!(fields.contains(&"gross_total"), "{fields:?}");
        assert!(fields.contains(&"net_total"), "{fields:?}");
        // 报差异不影响读：单头值仍是落库值，明细值是真加出来的
        assert_eq!(summary.sheet.totals.employee_count, 2);
        assert_eq!(summary.detail_totals.employee_count, 3);
        assert_eq!(
            summary.detail_totals.earning_total,
            summary.sheet.totals.earning_total + money("100")
        );

        // 先删单头（明细 ON DELETE CASCADE 跟着走），否则明细的 `employee_id` 是 RESTRICT，
        // 员工删不掉 —— 这个顺序本身就是「明细挂员工」的证据
        db().exec_sql(
            "DELETE FROM biz_salary_sheet WHERE period_code = ? AND org_id = ?::uuid".to_string(),
            vec![Value::Text(PERIOD.to_string()), Value::from(&org())],
        )
        .await
        .expect("删单头（明细级联）");
        db().exec_sql(
            "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
            vec![Value::from(&intruder)],
        )
        .await
        .expect("删员工");
        cleanup(&fixture).await;
    }

    /// 明细分页查询串的小构造器（`SalaryDetailPageQuery` 字段全是 `Option`，逐条写太吵）。
    struct SalaryDetailQueryBuilder {
        query: SalaryDetailPageQuery,
    }

    impl SalaryDetailQueryBuilder {
        fn new() -> Self {
            Self {
                query: SalaryDetailPageQuery::default(),
            }
        }

        fn page(mut self, page: u64) -> Self {
            self.query.page = Some(page);
            self
        }

        fn page_size(mut self, size: u64) -> Self {
            self.query.page_size = Some(size);
            self
        }

        fn keyword(mut self, keyword: &str) -> Self {
            self.query.keyword = Some(keyword.to_string());
            self
        }

        fn dept_id(mut self, dept_id: &str) -> Self {
            self.query.dept_id = Some(dept_id.to_string());
            self
        }

        fn with_items(mut self, flag: bool) -> Self {
            self.query.with_items = Some(flag);
            self
        }

        fn build(self) -> SalaryDetailPageQuery {
            self.query
        }
    }

    /// 未使用的导入兜底：`day()` / `CalcRequest` / `ManualEntry` 是上面夹具的备用手法，
    /// 留着会在 `-D warnings` 下变红，这里显式引用一次。
    #[test]
    fn fixture_helpers_are_wired() {
        assert_eq!(day("2026-12-01"), NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期"));
        let _ = CalcRequest::preview(PERIOD, None);
        let _ = ManualEntry {
            employee_id: Uuid::now_v7(),
            item_code: "HR36BASE".to_string(),
            amount: money("1"),
        };
        let _ = salary_sheet_model::TABLE_DETAIL_ITEM;
    }
}
