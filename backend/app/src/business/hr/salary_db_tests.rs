//! 员工薪资标准（`biz_employee_salary`）的真库用例（HR-16）。
//!
//! 单测（`salary_model` 的 `ranges_overlap` / `salary_dto` 的 `parse_adjust`）证明的是
//! **判定与校验**；证明不了四件事，只有真库能答：
//!
//! 1. `INSERT` / `UPDATE` / 收口 SQL 的占位符与 `?::numeric` / `?::text::date` 写法真能跑；
//! 2. **验收项：区间重叠被拒（含边界相等）** —— 应用层拦得住，且整笔回滚（库里行数不变）；
//! 3. **验收项：不重叠 / 相邻** —— 相邻（旧止 `== 新起 - 1`）能进，不重叠的历史版本不被动；
//! 4. 收口 / 修正 / 取消三条写路径在库里的样子（`effective_to` 真的变成 `生效日 - 1`）。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr16_card \
//!   cargo test --lib business::hr::salary_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 / HR-15 的教训）
//!
//! - 工号 / 身份证 / 部门码 / 薪资项编码都带唯一后缀，重跑不互相踩；
//! - 清场**按 id / 精确编码**删（绝不按前缀扫全库），且**先删薪资行再删员工**
//!   （`biz_employee_salary.employee_id` 是 `ON DELETE RESTRICT`）；
//! - `biz_salary_item` 的 17 个种子项归 HR-31，本卡只**造自己的**测试项（唯一编码 + 唯一
//!   `sort_order`，`uk_biz_sal_item_sort` 是公司内唯一）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::employee_handler;
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use crate::business::hr::salary_dto::{SalaryAdjustReq, SalaryItemReq};
    use crate::business::hr::salary_handler;
    use crate::business::hr::salary_model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::Decimal;
    use tokio::sync::OnceCell;

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
                    Err(err) => {
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        format!("1101011990{:06}", hash % 1_000_000)
    }

    fn super_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), true, ScopeKind::All, vec![], false)
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn money(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法金额")
    }

    /// 取号段：`sort_order` 公司内唯一，用纳秒尾数避开其它夹具。
    fn next_sort() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u128;
        (900_000 + (now + seq) % 90_000) as i32
    }

    /// 造一个本用例专属薪资项（`biz_salary_item` 的 17 个种子项归 HR-31）。
    async fn fresh_item(tag: &str, name: &str) -> String {
        let code = format!(
            "HR16{}",
            &tag.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>()
        );
        let code = format!("{code}{}", &uniq()[..6]);
        db().exec_sql(
            "INSERT INTO biz_salary_item \
                     (org_id, item_code, item_name, item_type, calc_type, source, sort_order, \
                      debit_account_code, credit_account_code) \
                 VALUES (?::uuid, ?, ?, 'EARNING', 'FIXED', 'EMPLOYEE_STANDARD', ?::int, '5602.10', '2211.01')"
                .to_string(),
            vec![
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(code.clone()),
                Value::Text(name.to_string()),
                Value::from(next_sort()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("造薪资项失败：{err}"));
        code
    }

    /// 造一个员工（走真实 `create_employee`：薪资标准要挂在外键上）。
    async fn fresh_employee(tag: &str, dept: Uuid) -> (Uuid, String) {
        let suffix = format!("{tag}{}", uniq());
        let username = format!("hr16.{suffix}");
        let user = test_helpers::ensure_user(&username, "HR16 夹具员工", Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR16-{suffix}")),
                name: Some(format!("员工{suffix}")),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                id_card: Some(id_card_of(&suffix)),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                user_id: Some(user.to_string()),
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建员工");
        (Uuid::parse_str(&created.id).expect("员工 id"), username)
    }

    /// 造部门。
    async fn fresh_dept(tag: &str) -> (Uuid, String) {
        let code = format!("HR16-{tag}-{}", uniq());
        let id = test_helpers::ensure_dept(&code, &format!("HR16 部门 {tag}")).await;
        (id, code)
    }

    /// 夹具套件：部门 + 员工 + 两个薪资项 + 清理清单。
    struct Fixture {
        dept: Uuid,
        dept_code: String,
        employee: Uuid,
        username: String,
        base: String,
        meal: String,
    }

    async fn fixture(tag: &str) -> Fixture {
        let (dept, dept_code) = fresh_dept(tag).await;
        let (employee, username) = fresh_employee(tag, dept).await;
        let base = fresh_item(&format!("{tag}B"), "HR16 基本工资").await;
        let meal = fresh_item(&format!("{tag}M"), "HR16 餐补").await;
        Fixture {
            dept,
            dept_code,
            employee,
            username,
            base,
            meal,
        }
    }

    impl Fixture {
        fn principal(&self) -> ScopedPrincipal {
            super_principal(self.dept)
        }

        fn operator(&self) -> Operator {
            Operator::named(Some(Uuid::now_v7()), "HR16 夹具")
        }

        /// 清场：薪资行 → 薪资项 → 员工 → 用户 → 部门（顺序不能换，FK 是 RESTRICT）。
        async fn clean(&self) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee_salary WHERE employee_id = ?::uuid".to_string(),
                    vec![Value::from(&self.employee)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.employee)],
                )
                .await;
            for code in [&self.base, &self.meal] {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_salary_item WHERE item_code = ?".to_string(),
                        vec![Value::Text(code.clone())],
                    )
                    .await;
            }
            test_helpers::purge_user(&self.username).await;
            test_helpers::purge_dept(&self.dept_code).await;
        }

        /// 库里该员工该项的有效行数。
        async fn row_count(&self, item_code: &str) -> i64 {
            let row = db()
                .query_first_sql(
                    "SELECT count(*)::bigint AS n FROM biz_employee_salary \
                      WHERE employee_id = ?::uuid AND item_code = ? AND deleted_at IS NULL"
                        .to_string(),
                    vec![Value::from(&self.employee), Value::Text(item_code.to_string())],
                )
                .await
                .expect("行数统计")
                .expect("count 必有行");
            row.get_typed("n").expect("count 列")
        }
    }

    /// 一条「现行标准」（无终止日）入参。
    fn open_ended(item_code: &str, amount: &str) -> SalaryItemReq {
        SalaryItemReq {
            item_code: Some(item_code.to_string()),
            amount: Some(money(amount)),
            effective_to: None,
            remark: Some("HR16 夹具".to_string()),
        }
    }

    /// 一条「明确区间」入参（补登历史 / 预置计划）。
    fn ranged(item_code: &str, amount: &str, to: &str) -> SalaryItemReq {
        SalaryItemReq {
            item_code: Some(item_code.to_string()),
            amount: Some(money(amount)),
            effective_to: Some(to.to_string()),
            remark: None,
        }
    }

    /// 一条「取消」入参（`amount` 缺省）。
    fn cancel(item_code: &str) -> SalaryItemReq {
        SalaryItemReq {
            item_code: Some(item_code.to_string()),
            amount: None,
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

    // ==================== ① 调薪链：插新 + 收口旧 ====================

    /// 首次录入 → 调薪：新行「至今」，旧行 `effective_to = 新生效日 - 1`；
    /// 读接口（#16）与员工详情（HR-12 的 `salary_items[]`）看到同一份投影。
    #[tokio::test]
    async fn adjustment_closes_previous_and_matches_employee_detail() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("chain").await;
        let principal = fx.principal();

        let first = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "5602.10")]),
        )
        .await
        .expect("首次录入");
        assert_eq!(first.written.len(), 1);
        assert!(first.closed.is_empty(), "首次录入没有旧行可收口");
        assert_eq!(first.written[0].amount, money("5602.10"));
        assert!(first.written[0].effective_to.is_none(), "现行标准没有终止日");
        assert_eq!(first.items.len(), 1);

        let second = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-07-01", vec![open_ended(&fx.base, "6800")]),
        )
        .await
        .expect("调薪");
        assert_eq!(second.written.len(), 1);
        assert_eq!(second.written[0].amount, money("6800"));
        assert_eq!(second.closed.len(), 1, "旧开放行必须被收口");
        assert_eq!(second.closed[0].effective_from, day("2026-01-01"));
        assert_eq!(
            second.closed[0].effective_to,
            Some(day("2026-06-30")),
            "旧行止于新生效日的前一天（闭区间）"
        );
        assert_eq!(second.items.len(), 2, "两个版本都在台账里");
        assert_eq!(fx.row_count(&fx.base).await, 2);

        // 读接口：现行 = 2026-07-01 那条（7 月 1 日之后至今）
        let list = salary_handler::salary_list_of(&principal, fx.employee)
            .await
            .expect("读薪资标准");
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.current.len(), 1);
        assert_eq!(list.current[0].amount, money("6800"));
        assert!(list.current[0].current);
        assert!(!list.emp_no.is_empty(), "回执要带工号");
        assert!(
            list.items[0].effective_from > list.items[1].effective_from,
            "台账新的在前"
        );

        // 与员工详情的薪资子表同源（同一份 SQL）
        let detail_items = salary_model::list_by_employee(db(), fx.employee)
            .await
            .expect("读员工薪资");
        assert_eq!(detail_items.len(), 2);
        assert_eq!(detail_items[0].amount, money("6800"));

        println!(
            "[HR-16] ① 调薪链 base={} 版本={} 收口={:?}",
            fx.base,
            fx.row_count(&fx.base).await,
            second.closed[0].effective_to
        );
        fx.clean().await;
    }

    // ==================== ② 相邻 / 不重叠都接受 ====================

    /// **验收项「相邻」**：旧区间止于 `03-30`，新标准起于 `03-31` → 接受且不重叠。
    /// 已收口的历史行（带 `effective_to`）不会被隐式改（调薪只收口开放行）。
    #[tokio::test]
    async fn adjacent_ranges_are_accepted() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("adj").await;
        let principal = fx.principal();

        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![ranged(&fx.base, "5000", "2026-03-30")]),
        )
        .await
        .expect("登记历史区间");

        let next = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-03-31", vec![open_ended(&fx.base, "5500")]),
        )
        .await
        .expect("相邻区间必须接受");
        assert_eq!(next.written.len(), 1);
        assert!(next.closed.is_empty(), "已收口的历史行不许被调薪隐式改");
        assert_eq!(next.items.len(), 2);
        assert_eq!(fx.row_count(&fx.base).await, 2);

        // 写后库里确实没有交集（A3 的口径：相邻不算重叠）
        let versions = salary_model::find_versions(db(), fx.employee, &fx.base)
            .await
            .expect("读版本");
        assert!(salary_model::find_pair_overlap(&versions).is_none());

        println!("[HR-16] ② 相邻 base={} 版本={}", fx.base, versions.len());
        fx.clean().await;
    }

    /// **验收项「不重叠」**：两段完全不搭界的历史区间（中间还有空档）都能进。
    #[tokio::test]
    async fn disjoint_ranges_are_accepted() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("disj").await;
        let principal = fx.principal();

        for (from, to, amount) in [
            ("2025-01-01", "2025-06-30", "4000"),
            ("2025-09-01", "2025-12-31", "4500"),
        ] {
            salary_handler::adjust_salary(
                &principal,
                fx.operator(),
                fx.employee,
                &req(from, vec![ranged(&fx.base, amount, to)]),
            )
            .await
            .unwrap_or_else(|err| panic!("不重叠的 {from}~{to} 必须接受：{}", err.msg));
        }
        assert_eq!(fx.row_count(&fx.base).await, 2);
        let versions = salary_model::find_versions(db(), fx.employee, &fx.base)
            .await
            .expect("读版本");
        assert!(salary_model::find_pair_overlap(&versions).is_none());
        fx.clean().await;
    }

    // ==================== ③ 重叠被拒（回滚，不留半截） ====================

    /// **验收项「重叠被拒」**：在已收口的历史区间内部插新标准 → 409 + `68007`，
    /// 报文点名薪资项与两条区间；**整笔回滚**（行数不变、旧行还是原来的样子）。
    #[tokio::test]
    async fn overlapping_range_is_rejected_and_rolled_back() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("over").await;
        let principal = fx.principal();

        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![ranged(&fx.base, "5000", "2026-06-30")]),
        )
        .await
        .expect("登记历史区间");

        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-04-01", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect_err("落在已收口区间内部必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RANGE_OVERLAP);
        let code = error.code;
        let message = error.msg;
        assert!(message.contains(&fx.base), "要点名薪资项：{message}");
        assert!(message.contains("2026-04-01 ~ 至今"), "要点名本次区间：{message}");
        assert!(message.contains("2026-01-01 ~ 2026-06-30"), "要点名冲突区间：{message}");
        assert!(message.contains("边界相等也算重叠"), "要把口径写给用户：{message}");

        // 整笔回滚：仍然只有一行，且它还是原来的完整区间
        assert_eq!(fx.row_count(&fx.base).await, 1);
        let versions = salary_model::find_versions(db(), fx.employee, &fx.base)
            .await
            .expect("读版本");
        assert_eq!(versions[0].effective_to, Some(day("2026-06-30")));
        assert_eq!(versions[0].amount, money("5000"));

        println!("[HR-16] ③ 重叠被拒 code={} 行数={}", code, versions.len());
        fx.clean().await;
    }

    /// **验收项「含边界相等」**：旧区间止于 `06-30`，新生效日也是 `06-30` →
    /// 06-30 这天会有两个标准，必须拒（闭区间口径）。
    #[tokio::test]
    async fn equal_boundary_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("edge").await;
        let principal = fx.principal();

        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![ranged(&fx.base, "5000", "2026-06-30")]),
        )
        .await
        .expect("登记历史区间");

        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-06-30", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect_err("边界相等（旧止 = 新起）必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RANGE_OVERLAP);
        assert!(error.msg.contains("2026-06-30 ~ 至今"), "{}", error.msg);
        assert_eq!(fx.row_count(&fx.base).await, 1, "被拒后不许留下新行");

        // 换成 07-01（相邻一天）→ 立刻能进，证明拒的是「那一天」，不是暴躁
        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-07-01", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect("改一天就合法");
        assert_eq!(fx.row_count(&fx.base).await, 2);

        println!("[HR-16] ④ 边界相等被拒（06-30 vs 06-30），07-01 相邻放行");
        fx.clean().await;
    }

    /// 已有「更晚生效」的版本时，补登更早的现行标准会与它重叠 → 拒绝（提示先处理更晚那条）。
    #[tokio::test]
    async fn future_version_blocks_earlier_standard() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("future").await;
        let principal = fx.principal();

        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-07-01", vec![open_ended(&fx.base, "6800")]),
        )
        .await
        .expect("先登记 7 月起的标准");

        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-04-01", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect_err("更早的「至今」标准会盖住 7 月那条");
        assert_eq!(error.code, error_code::HR_SALARY_RANGE_OVERLAP);
        assert!(error.msg.contains("2026-07-01 ~ 至今"), "{}", error.msg);
        assert_eq!(fx.row_count(&fx.base).await, 1, "整笔回滚");

        // 补登成「明确区间」就合法（不碰 7 月那条）
        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-04-01", vec![ranged(&fx.base, "6000", "2026-06-30")]),
        )
        .await
        .expect("明确区间补登必须能进");
        assert_eq!(fx.row_count(&fx.base).await, 2);
        let versions = salary_model::find_versions(db(), fx.employee, &fx.base)
            .await
            .expect("读版本");
        assert!(salary_model::find_pair_overlap(&versions).is_none());

        println!("[HR-16] ⑤ 未来版本拦下更早的现行标准，明确区间补登放行");
        fx.clean().await;
    }

    // ==================== ④ 同生效日 = 原地修正（幂等） ====================

    /// 同一生效日重复提交 = 修正金额（`uk_biz_emp_salary` 决定同一天只有一行），
    /// 幂等重放不新增行、不报错，行 id 不变。
    #[tokio::test]
    async fn same_effective_day_corrects_amount_in_place() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("same").await;
        let principal = fx.principal();

        let first = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "5602.10")]),
        )
        .await
        .expect("首次录入");
        assert_eq!(first.written[0].amount, money("5602.10"));

        let again = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect("同一天重提 = 修正，不是冲突");
        assert_eq!(again.written.len(), 1);
        assert!(again.closed.is_empty());
        assert_eq!(again.written[0].amount, money("6000"));
        assert_eq!(fx.row_count(&fx.base).await, 1, "不许新增第二行");
        assert_eq!(again.items.len(), 1);

        // 重放同 payload（幂等）：金额不变、行数不变
        let replay = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "6000")]),
        )
        .await
        .expect("幂等重放");
        assert_eq!(replay.written[0].amount, money("6000"));
        assert_eq!(fx.row_count(&fx.base).await, 1);

        println!("[HR-16] ⑥ 同日修正 5602.10 → 6000，行数恒为 1");
        fx.clean().await;
    }

    // ==================== ⑤ 取消 ====================

    /// 取消：自生效日起不再有该项（旧行收口到 `-1 天`，未来版本软删）；
    /// 对没有标准的项取消 → 409 + `68008`（本次什么都没改）。
    #[tokio::test]
    async fn cancel_closes_standard_and_blocks_empty_cancel() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("cancel").await;
        let principal = fx.principal();

        salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.meal, "800")]),
        )
        .await
        .expect("登记餐补");

        let canceled = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-12-01", vec![cancel(&fx.meal)]),
        )
        .await
        .expect("取消餐补");
        assert_eq!(canceled.canceled, vec![fx.meal.clone()]);
        assert_eq!(canceled.closed.len(), 1);
        assert_eq!(canceled.closed[0].effective_to, Some(day("2026-11-30")));
        assert_eq!(canceled.items.len(), 1, "行还在（历史），只是收口了");
        assert_eq!(canceled.current.len(), 1, "12-01 之前仍然现行");

        // 再取消一次（12-01 起已经没了）→ 68 段「本次什么都没改」
        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-12-01", vec![cancel(&fx.meal)]),
        )
        .await
        .expect_err("没有可取消的标准");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains("没有标准可取消"), "{}", error.msg);

        println!("[HR-16] ⑦ 取消 MEAL 收口到 2026-11-30，重复取消 68008");
        fx.clean().await;
    }

    // ==================== ⑥ 守卫 ====================

    /// 薪资项不存在 → 400（弱引用不建 FK，拼错编码会变成孤儿行）；离职员工 → 409 + 68008。
    #[tokio::test]
    async fn unknown_item_and_resigned_employee_are_blocked() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("guard").await;
        let principal = fx.principal();

        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended("NO_SUCH_ITEM_X", "100")]),
        )
        .await
        .expect_err("薪资项不存在");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不存在或已停用"), "{}", error.msg);

        // 直接改库把员工置为离职（离职流程归 HR-16 之外的卡）；
        // `ck_biz_employee_leave` 是双向的：RESIGNED ⇔ leave_date 非空，且要有 leave_type。
        db().exec_sql(
            "UPDATE biz_employee SET status = 'RESIGNED', leave_date = CURRENT_DATE, leave_type = 'RESIGN' \
                  WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&fx.employee)],
        )
        .await
        .expect("置离职");
        let error = salary_handler::adjust_salary(
            &principal,
            fx.operator(),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "100")]),
        )
        .await
        .expect_err("离职员工不能调薪");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains("已离职"), "{}", error.msg);
        assert_eq!(fx.row_count(&fx.base).await, 0, "被拦下不许留行");

        println!("[HR-16] ⑧ 未知薪资项 400 / 离职员工 409+68008");
        fx.clean().await;
    }

    /// 数据范围：别的部门的人调不了 / 也读不了（**404 不是 403**，403 会泄露「这人存在」）。
    #[tokio::test]
    async fn data_scope_outside_returns_404() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("scope").await;
        let other_dept = Uuid::now_v7();
        let outsider = ScopedPrincipal::for_test(
            Uuid::now_v7(),
            Some(other_dept),
            false,
            ScopeKind::Dept,
            vec![other_dept],
            false,
        );

        let read = salary_handler::salary_list_of(&outsider, fx.employee)
            .await
            .expect_err("范围外读不到");
        assert_eq!(read.status, 404);

        let write = salary_handler::adjust_salary(
            &outsider,
            Operator::named(Some(Uuid::now_v7()), "外部"),
            fx.employee,
            &req("2026-01-01", vec![open_ended(&fx.base, "100")]),
        )
        .await
        .expect_err("范围外调不了");
        assert_eq!(write.status, 404);
        assert_eq!(fx.row_count(&fx.base).await, 0);

        // 本部门（All 范围）能读到空列表（不是 404）
        let list = salary_handler::salary_list_of(&fx.principal(), fx.employee)
            .await
            .expect("本部门可读");
        assert!(list.items.is_empty());
        assert_eq!(list.employee_id, fx.employee.to_string());

        println!("[HR-16] ⑨ 范围外 404（读+写），本部门可读空列表");
        fx.clean().await;
    }
}
