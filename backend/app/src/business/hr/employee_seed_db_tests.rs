//! 「从登录账号批量生成员工档」（`seed-from-users`）的真库用例（HR-17）。
//!
//! 单测（`employee_seed` 的 `classify` / `build_write` / 出参序列化）证明的是**判定与形态**；
//! 只有真库能证明卡面的三个验收项：
//!
//! | 验收项 | 用例 |
//! |---|---|
//! | **dry-run 只报告不写库** | [`dry_run_writes_nothing_and_eats_no_number`] —— 库行数不变、号段不动 |
//! | **批量建不回滚已成功的** | [`failure_isolates_earlier_successes`] —— 中间一个被库层拒绝，前后两个照样落地，号段只前进 2 |
//! | **`user_id` 回写正确** | [`creates_active_employees_and_writes_back_user_id`] —— 逐列读回 `user_id` / `status` / `emp_no` / `dept_id` / `hire_date` |
//!
//! 另有两条边界：跳过原因（已有档案 / 停用 / 机器人 / 没姓名）与
//! 「部门筛选 + 数据范围」[`skips_existing_disabled_robot_and_nameless`] /
//! [`dept_filter_and_data_scope`]。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr17_card \
//!   cargo test --lib business::hr::employee_seed_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律
//!
//! - 用户名 / 部门码带唯一后缀（`hr17.*` + 纳秒尾数），重跑不互相踩；
//! - 清场按**用户名 / 精确 id** 删（绝不按前缀扫全库），顺序：员工 → 账号 → 部门；
//! - `biz_employee.name` 上会临时挂一个触发器（[`BOOM_TRIGGER`]）来**制造一次真实的
//!   库层失败**：升号段能证明「失败的那条不吃号」，而这条只能靠让 `INSERT` 真的炸一次。
//!   触发器只对哨兵姓名生效，用例结束（与下次开始）都删掉。

use nik::prelude::*;

#[cfg(test)]
mod db {
    use super::*;
    use crate::business::hr::employee_seed::{self, EmployeeSeedReq};
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::shared::test_helpers;
    use nik::uuid::Uuid;
    use tokio::sync::OnceCell;

    /// 人为失败的哨兵姓名（触发器只拒这一个名字；单测/别的用例不会用它）。
    const BOOM_TRIGGER: &str = "hr17_seed_boom_trg";
    /// 触发函数名。
    const BOOM_FUNCTION: &str = "hr17_seed_boom";

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

    fn day(text: &str) -> nik::prelude::chrono::NaiveDate {
        nik::prelude::chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    /// 全量范围的主体（`is_super = true` → `scope::filter_for` 恒真）。
    fn super_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), true, ScopeKind::All, vec![], false)
    }

    /// 造部门（`sys_dept` 走既有夹具，按 `dept_code` 精确清场）。
    async fn fresh_dept(tag: &str) -> (Uuid, String) {
        let code = format!("HR17-{tag}-{}", uniq());
        let id = test_helpers::ensure_dept(&code, &format!("HR17 部门 {tag}")).await;
        (id, code)
    }

    /// 造一个登录账号（状态 / 类型 / 姓名都可控 —— 停止 / 机器人 / 无名都靠它）。
    async fn fresh_user(
        tag: &str,
        real_name: Option<&str>,
        dept: Option<Uuid>,
        status: i16,
        user_type: &str,
    ) -> (Uuid, String) {
        let username = format!("hr17.{tag}.{}", uniq());
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO sys_user \
                     (id, username, password, real_name, mobile, email, dept_id, status, user_type) \
                 VALUES (?, ?, 'hr17-fixture-not-a-real-hash', NULLIF(?::text,''), \
                         '13900000000', ?, NULLIF(?::text,'')::uuid, ?::int, ?)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(username.clone()),
                Value::Text(real_name.unwrap_or_default().to_string()),
                Value::Text(format!("{username}@example.com")),
                Value::Text(dept.map(|d| d.to_string()).unwrap_or_default()),
                Value::from(i32::from(status)),
                Value::Text(user_type.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("造账号 {username} 失败：{err}"));
        (id, username)
    }

    /// 清场：员工（按 user_id）→ 账号（按 username）→ 部门（按 dept_code）。
    async fn clean(users: &[String], depts: &[String]) {
        for username in users {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE user_id IN \
                       (SELECT id FROM sys_user WHERE username = ?)"
                        .to_string(),
                    vec![Value::Text(username.clone())],
                )
                .await;
            test_helpers::purge_user(username).await;
        }
        for code in depts {
            test_helpers::purge_dept(code).await;
        }
    }

    /// 员工行数（**只数本套夹具的账号**：全库计数会被并行跑的其它用例干扰）。
    async fn employee_count_of(usernames: &[String]) -> i64 {
        let mut total = 0_i64;
        for username in usernames {
            if employee_of(username).await.is_some() {
                total += 1;
            }
        }
        total
    }

    /// `EMP` 号段的当前流水（取号会推进它；dry-run 与失败都不该推进）。
    async fn seq_current() -> i64 {
        let row = db()
            .query_first_sql(
                "SELECT current_no::bigint AS n FROM biz_seq WHERE seq_type = 'EMP'".to_string(),
                vec![],
            )
            .await
            .expect("查号段")
            .expect("EMP 号段行必须存在（V0386 种入）");
        row.get_typed("n").expect("current_no 列")
    }

    /// 读某账号名下的员工档（`deleted_at IS NULL`）。
    async fn employee_of(username: &str) -> Option<Row> {
        db().query_first_sql(
            "SELECT e.id, e.emp_no, e.name, e.status, e.user_id, e.dept_id, e.hire_date, \
                        e.create_by, e.employment_type, e.remark, \
                        COALESCE(u.username, '') AS username \
                   FROM biz_employee e JOIN sys_user u ON u.id = e.user_id \
                  WHERE u.username = ? AND e.deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(username.to_string())],
        )
        .await
        .expect("查员工档")
    }

    /// 在 `preview[]` 里按用户名找一行。
    fn item_of<'a>(res: &'a employee_seed::EmployeeSeedRes, username: &str) -> &'a employee_seed::SeedItem {
        res.preview
            .iter()
            .find(|item| item.username == username)
            .unwrap_or_else(|| panic!("preview 里没有 {username}"))
    }

    /// 只看本套夹具的 `preview[]` 行（按唯一 tag 前缀过滤）。
    ///
    /// **为什么不直接断言 `res.created` / `res.skipped`**：真库里的候选集是「全库所有
    /// 还没有员工档的 human 账号」——别的卡的真库用例会留下自己的夹具账号（这个演练库上
    /// 就有 `hr10.*` 的残留），并行/串行都不影响本卡的语义，但会让**全局计数**漂移。
    /// 所以断言一律落在「我自己那几条」上；全局计数只用来验证**不变量**
    /// （例如「号段前进数 == 成功条数」）。
    fn mine<'a>(res: &'a employee_seed::EmployeeSeedRes, prefix: &str) -> Vec<&'a employee_seed::SeedItem> {
        res.preview
            .iter()
            .filter(|item| item.username.starts_with(prefix))
            .collect()
    }

    /// `preview[]` 里 `action=CREATE` 的条数（= 本次会建 / 已建的条数）。
    fn create_count(res: &employee_seed::EmployeeSeedRes) -> usize {
        res.preview
            .iter()
            .filter(|item| item.action == employee_seed::SeedAction::Create)
            .count()
    }

    /// 装「人为失败」触发器（只拒哨兵姓名，别的行原样放行）。
    async fn arm_boom(sentinel: &str) {
        disarm_boom().await;
        db().exec_sql(
            format!(
                "CREATE OR REPLACE FUNCTION {BOOM_FUNCTION}() RETURNS trigger AS $boom$ \
                     BEGIN IF NEW.name = '{sentinel}' THEN \
                       RAISE EXCEPTION 'hr17 drill: 人为拒绝（验收「批量建不回滚已成功的」）'; \
                     END IF; RETURN NEW; END $boom$ LANGUAGE plpgsql"
            ),
            vec![],
        )
        .await
        .expect("建触发函数");
        db().exec_sql(
            format!(
                "CREATE TRIGGER {BOOM_TRIGGER} BEFORE INSERT ON biz_employee \
                     FOR EACH ROW EXECUTE FUNCTION {BOOM_FUNCTION}()"
            ),
            vec![],
        )
        .await
        .expect("建触发器");
    }

    /// 卸触发器（幂等；用例开始时也调一次，兜住上一轮 panic 留下的）。
    async fn disarm_boom() {
        let _ = db()
            .exec_sql(format!("DROP TRIGGER IF EXISTS {BOOM_TRIGGER} ON biz_employee"), vec![])
            .await;
        let _ = db()
            .exec_sql(format!("DROP FUNCTION IF EXISTS {BOOM_FUNCTION}()"), vec![])
            .await;
    }

    // ==================== ① dry-run 只报告不写库 ====================

    /// **验收①**：`dry_run=true` 出预览，但一行都不写、号段一格都不动。
    #[tokio::test]
    async fn dry_run_writes_nothing_and_eats_no_number() {
        if !ensure_db().await {
            return;
        }
        disarm_boom().await;
        let (dept, dept_code) = fresh_dept("DRY").await;
        let (alice_id, alice) = fresh_user("dry.a", Some("张爱丽"), Some(dept), 1, "HUMAN").await;
        let (_bob_id, bob) = fresh_user("dry.b", Some("李博"), Some(dept), 1, "HUMAN").await;
        let users = vec![alice.clone(), bob.clone()];

        let principal = super_principal(dept);
        let before_seq = seq_current().await;

        let req = EmployeeSeedReq {
            dept_id: Some(dept.to_string()),
            hire_date: Some("2024-03-01".to_string()),
            dry_run: Some(true),
        };
        let res = employee_seed::seed_from_users(&principal, None, &req)
            .await
            .expect("dry-run 不该报错");

        assert!(res.dry_run);
        assert_eq!(res.created, 0, "dry-run 的 created 必须是 0（验收：只报告不写库）");
        assert_eq!(res.failed, 0);
        assert!(res.planned_create >= 2, "两人都该进计划：{res:?}");
        assert_eq!(res.preview.len(), res.scanned);
        assert!(res.next_no.is_some(), "dry-run 应给下一个工号（peek 不占号）");
        for username in [&alice, &bob] {
            let item = item_of(&res, username);
            assert_eq!(item.action, employee_seed::SeedAction::Create);
            assert!(item.emp_no.is_none(), "预览不占号：emp_no 应为空");
            assert!(item.employee_id.is_none(), "预览不落库：employee_id 应为空");
            assert!(item.message.contains("将建档"), "{}", item.message);
        }
        // 库里一定没有：账号名下无档案、号段没动。
        assert_eq!(employee_count_of(&users).await, 0, "dry-run 写了库！");
        assert_eq!(seq_current().await, before_seq, "dry-run 吃了号！");
        assert!(employee_of(&alice).await.is_none());
        assert_eq!(item_of(&res, &alice).user_id, alice_id.to_string());

        clean(&users, &[dept_code]).await;
    }

    // ==================== ② 批量建档 + user_id 回写 ====================

    /// **验收③**：落库后 `user_id` 指向账号（且状态 / 工号 / 部门 / 入职日期 / 留痕都对）；
    /// **幂等**：再点一次 `created = 0`，全部 `HAS_EMPLOYEE`。
    #[tokio::test]
    async fn creates_active_employees_and_writes_back_user_id() {
        if !ensure_db().await {
            return;
        }
        disarm_boom().await;
        let (dept, dept_code) = fresh_dept("CREATE").await;
        let tag = uniq();
        let prefix = format!("hr17.{tag}");
        let (u1, name1) = fresh_user(&format!("{tag}.a"), Some("王小明"), Some(dept), 1, "HUMAN").await;
        let (u2, name2) = fresh_user(&format!("{tag}.b"), Some("赵小红"), Some(dept), 1, "HUMAN").await;
        let users = vec![name1.clone(), name2.clone()];

        let principal = super_principal(dept);
        let before_seq = seq_current().await;
        let req = EmployeeSeedReq {
            dept_id: Some(dept.to_string()),
            hire_date: Some("2023-07-01".to_string()),
            dry_run: Some(false),
        };
        let res = employee_seed::seed_from_users(&principal, Some(u1), &req)
            .await
            .expect("批量建档");
        assert!(!res.dry_run);
        assert_eq!(res.failed, 0, "不该有失败：{res:?}");
        // 不变量：`created` = preview 里 CREATE 的条数（本套两条都该建）。
        assert_eq!(create_count(&res), res.created, "created 与 preview 不一致：{res:?}");
        let mine_items = mine(&res, &prefix);
        assert_eq!(mine_items.len(), 2, "本套两条候选都该出现在预览里：{res:?}");
        assert!(
            mine_items
                .iter()
                .all(|item| item.action == employee_seed::SeedAction::Create),
            "两个候选都该建档：{mine_items:?}"
        );
        assert!(res.next_no.is_none(), "真实执行不需要 peek 工号");

        // **号段恰好前进成功条数**（失败 / 回滚不吃号；用 `res.created` 而不是写死 2，
        // 这样库里别的卡残留夹具被顺带建出来时，断言依然证的是同一条性质）。
        assert_eq!(
            seq_current().await,
            before_seq + res.created as i64,
            "号段前进数必须等于成功条数"
        );

        for (user_id, username) in [(&u1, &name1), (&u2, &name2)] {
            let item = item_of(&res, username);
            assert_eq!(item.action, employee_seed::SeedAction::Create);
            let emp_no = item.emp_no.clone().expect("真实执行必须回填工号");
            assert!(emp_no.starts_with("EMP-"), "工号形态：{emp_no}");
            let emp_id = item.employee_id.clone().expect("真实执行必须回填 employee_id");

            let row = employee_of(username).await.expect("员工档应当已落库");
            assert_eq!(
                row.get_typed::<Uuid>("id").expect("id").to_string(),
                emp_id,
                "回执里的 employee_id 与库里的档案不一致"
            );
            // **验收③**：user_id 回写正确（= 账号 id，且 join 得到同一个 username）。
            let bound: Uuid = row.get_typed("user_id").expect("user_id 列");
            assert_eq!(bound, *user_id, "{username} 的 user_id 没回写");
            assert_eq!(row.get_typed::<String>("username").expect("join"), *username);
            assert_eq!(row.get_typed::<String>("emp_no").expect("emp_no"), emp_no);
            assert_eq!(row.get_typed::<String>("status").expect("status"), "ACTIVE");
            assert_eq!(
                row.get_typed::<String>("employment_type").expect("用工形式"),
                "FULL_TIME"
            );
            let emp_dept: Uuid = row.get_typed("dept_id").expect("dept_id");
            assert_eq!(emp_dept, dept, "员工部门应取账号部门");
            let hire: nik::prelude::chrono::NaiveDate = row.get_typed("hire_date").expect("hire_date");
            assert_eq!(hire, day("2023-07-01"), "入职日期应取入参");
            let remark: Option<String> = row.get_typed("remark").expect("remark");
            assert!(
                remark.as_deref().unwrap_or_default().contains(username),
                "留痕里应说明来自哪个账号：{remark:?}"
            );
            let created_by: Option<Uuid> = row.get_typed("create_by").expect("create_by");
            assert_eq!(created_by, Some(u1), "create_by 应是操作人");
        }

        // 幂等：再点一次，0 新建、全员 HAS_EMPLOYEE。
        let again = employee_seed::seed_from_users(&principal, Some(u1), &req)
            .await
            .expect("二次执行");
        assert_eq!(create_count(&again), 0, "重复点不该再建：{again:?}");
        assert_eq!(again.created, 0);
        for username in [&name1, &name2] {
            let item = item_of(&again, username);
            assert_eq!(item.action, employee_seed::SeedAction::Skip);
            assert_eq!(item.reason.as_deref(), Some("HAS_EMPLOYEE"));
            assert!(item.message.contains("EMP-"), "报文要点名已有工号：{}", item.message);
        }
        assert_eq!(
            seq_current().await,
            before_seq + res.created as i64,
            "幂等重放不该再吃号"
        );

        clean(&users, &[dept_code]).await;
    }

    // ==================== ③ 跳过原因 ====================

    /// 已有档案 / 停用账号 / 机器人账号 / 没姓名：四种都进 `SKIP`，一条都不写库。
    #[tokio::test]
    async fn skips_existing_disabled_robot_and_nameless() {
        if !ensure_db().await {
            return;
        }
        disarm_boom().await;
        let (dept, dept_code) = fresh_dept("SKIP").await;
        let tag = uniq();
        let prefix = format!("hr17.{tag}");
        // ① 已有档案：先手工建一个正常员工档。
        let (existing_id, existing) = fresh_user(&format!("{tag}.has"), Some("老员工"), Some(dept), 1, "HUMAN").await;
        let created = crate::business::hr::employee_handler::create_employee(
            Some(existing_id),
            &crate::business::hr::employee_handler::EmployeeCreateReq {
                name: Some("老员工".to_string()),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2020-01-01".to_string()),
                user_id: Some(existing_id.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("先建一个已有档案的账号");
        // ② 停用账号 ③ 机器人 ④ 没姓名 ⑤ 正常（对照组）
        let (_d_id, disabled) = fresh_user(&format!("{tag}.dis"), Some("停用者"), Some(dept), 0, "HUMAN").await;
        let (_r_id, robot) = fresh_user(&format!("{tag}.rob"), Some("机器人"), Some(dept), 1, "ROBOT").await;
        let (_n_id, nameless) = fresh_user(&format!("{tag}.noname"), None, Some(dept), 1, "HUMAN").await;
        let (_ok_id, healthy) = fresh_user(&format!("{tag}.ok"), Some("新同事"), Some(dept), 1, "HUMAN").await;
        let users = vec![
            existing.clone(),
            disabled.clone(),
            robot.clone(),
            nameless.clone(),
            healthy.clone(),
        ];

        let principal = super_principal(dept);
        let req = EmployeeSeedReq {
            dept_id: Some(dept.to_string()),
            hire_date: Some("2024-01-01".to_string()),
            dry_run: Some(false),
        };
        let res = employee_seed::seed_from_users(&principal, Some(existing_id), &req)
            .await
            .expect("批量建档");
        assert_eq!(res.failed, 0, "跳过不是失败：{res:?}");
        assert_eq!(create_count(&res), res.created, "created 与 preview 不一致：{res:?}");
        // 只看本套：5 条里 1 条 CREATE（对照组）、4 条 SKIP。
        let mine_items = mine(&res, &prefix);
        assert_eq!(mine_items.len(), 5, "本套五条候选都该出现：{res:?}");
        assert_eq!(
            mine_items
                .iter()
                .filter(|item| item.action == employee_seed::SeedAction::Skip)
                .count(),
            4,
            "四条都该跳过：{mine_items:?}"
        );
        assert_eq!(
            mine_items
                .iter()
                .filter(|item| item.action == employee_seed::SeedAction::Create)
                .count(),
            1,
            "只有对照组该建：{mine_items:?}"
        );

        let cases = [
            (&existing, "HAS_EMPLOYEE"),
            (&disabled, "USER_DISABLED"),
            (&robot, "NOT_HUMAN"),
            (&nameless, "NAME_MISSING"),
        ];
        for (username, code) in cases {
            let item = item_of(&res, username);
            assert_eq!(item.action, employee_seed::SeedAction::Skip, "{username}");
            assert_eq!(item.reason.as_deref(), Some(code), "{username} 的原因码");
            assert!(!item.message.trim().is_empty(), "{username} 缺文案");
        }
        // 已有档案的那个仍指向原档案（没有被覆盖 / 复制）。
        let row = employee_of(&existing).await.expect("原档案还在");
        let emp_id: Uuid = row.get_typed("id").expect("id");
        assert_eq!(emp_id.to_string(), created.id, "跳过不该动原档案（更不该新建第二条）");
        // 对照组真的建出来了。
        assert!(employee_of(&healthy).await.is_some(), "对照组应已建档");

        clean(&users, &[dept_code]).await;
        // 手工建的档案与它的一并清掉（`clean` 已按用户名删员工 + 账号）。
    }

    // ==================== ④ 失败隔离 ====================

    /// **验收②**：中间一个被库层拒绝，前后两个照样落地；失败那条**不吃号**。
    #[tokio::test]
    async fn failure_isolates_earlier_successes() {
        if !ensure_db().await {
            return;
        }
        let (dept, dept_code) = fresh_dept("ISO").await;
        let tag = uniq();
        let prefix = format!("hr17.{tag}");
        // 顺序 = created_at, username：a → b（会炸）→ c
        let (_a_id, user_a) = fresh_user(&format!("{tag}.a"), Some("甲"), Some(dept), 1, "HUMAN").await;
        let sentinel = format!("HR17-BOOM-{}", uniq());
        let (_b_id, user_b) = fresh_user(&format!("{tag}.b"), Some(&sentinel), Some(dept), 1, "HUMAN").await;
        let (_c_id, user_c) = fresh_user(&format!("{tag}.c"), Some("丙"), Some(dept), 1, "HUMAN").await;
        let users = vec![user_a.clone(), user_b.clone(), user_c.clone()];

        arm_boom(&sentinel).await;
        let principal = super_principal(dept);
        let before_seq = seq_current().await;
        let req = EmployeeSeedReq {
            dept_id: Some(dept.to_string()),
            hire_date: Some("2022-05-06".to_string()),
            dry_run: Some(false),
        };
        let res = employee_seed::seed_from_users(&principal, None, &req)
            .await
            .expect("单条失败不该冒到 HTTP 层");
        disarm_boom().await;

        // 只看本套三条：a / c 建（Create）、b 失败（Failed）。
        let mine_items = mine(&res, &prefix);
        assert_eq!(mine_items.len(), 3, "本套三条候选都该出现：{res:?}");
        assert_eq!(
            mine_items
                .iter()
                .filter(|item| item.action == employee_seed::SeedAction::Create)
                .count(),
            2,
            "前后两个必须落地（不回滚已成功的）：{mine_items:?}"
        );
        assert_eq!(
            mine_items
                .iter()
                .filter(|item| item.action == employee_seed::SeedAction::Failed)
                .count(),
            1,
            "中间那个该失败：{mine_items:?}"
        );
        assert_eq!(
            mine_items
                .iter()
                .filter(|item| item.action == employee_seed::SeedAction::Skip)
                .count(),
            0,
            "这三条都不该走跳过分支：{mine_items:?}"
        );

        // 成功的那两个在库里，且 user_id 都对。
        assert!(employee_of(&user_a).await.is_some(), "a 已被回滚（违反验收②）");
        assert!(employee_of(&user_c).await.is_some(), "c 已回滚（失败的后续条目被吞）");
        assert!(employee_of(&user_b).await.is_none(), "b 不该留下半条");
        // 失败行点名原因 + 业务码 + 「怎么往下走」。
        let failed = item_of(&res, &user_b);
        assert_eq!(failed.action, employee_seed::SeedAction::Failed);
        assert_eq!(failed.reason.as_deref(), Some("CREATE_FAILED"));
        assert!(failed.error_code.is_some(), "失败行要给业务码");
        assert!(failed.emp_no.is_none(), "失败的行没有工号");
        assert!(
            failed.message.contains("已单独回滚"),
            "报文要告诉 HR 怎么办：{}",
            failed.message
        );
        assert!(
            !failed.message.contains("hr17 drill"),
            "库层内部细节不许出现在响应报文里：{}",
            failed.message
        );
        // **失败那条不吃号**：号段前进数 == 成功条数（库里别的残留桩顺带建出来的也各吃一个）。
        assert_eq!(
            seq_current().await,
            before_seq + res.created as i64,
            "失败条目吃掉了号段（号段前进数必须等于成功条数）"
        );

        clean(&users, &[dept_code]).await;
    }

    // ==================== ⑤ 部门筛选 + 数据范围 ====================

    /// 传 `dept_id` = 只处理「本部门 + 无部门」，且无部门的账号落到该部门；
    /// 不传 = 无部门账号进 `DEPT_MISSING`；
    /// 数据范围不是全部的人只能从自己看得到的账号里生成。
    #[tokio::test]
    async fn dept_filter_and_data_scope() {
        if !ensure_db().await {
            return;
        }
        disarm_boom().await;
        let (dept_a, code_a) = fresh_dept("DA").await;
        let (dept_b, code_b) = fresh_dept("DB").await;
        let (_a_id, in_a) = fresh_user("dept.a", Some("甲部门的人"), Some(dept_a), 1, "HUMAN").await;
        let (_b_id, in_b) = fresh_user("dept.b", Some("乙部门的人"), Some(dept_b), 1, "HUMAN").await;
        let (_n_id, no_dept) = fresh_user("dept.none", Some("没部门的人"), None, 1, "HUMAN").await;
        let users = vec![in_a.clone(), in_b.clone(), no_dept.clone()];

        // ① 传 dept_a：只出「甲部门 + 无部门」，无部门的落甲部门。
        let principal_all = super_principal(dept_a);
        let req_a = EmployeeSeedReq {
            dept_id: Some(dept_a.to_string()),
            hire_date: Some("2021-09-01".to_string()),
            dry_run: Some(true),
        };
        let res = employee_seed::seed_from_users(&principal_all, None, &req_a)
            .await
            .expect("按部门预览");
        let names: Vec<&str> = res.preview.iter().map(|i| i.username.as_str()).collect();
        assert!(names.contains(&in_a.as_str()), "本部门账号应在范围内：{res:?}");
        assert!(
            names.contains(&no_dept.as_str()),
            "无部门账号应可被缺省部门收编：{res:?}"
        );
        assert!(!names.contains(&in_b.as_str()), "别的部门账号不该被拉进来：{res:?}");
        // 无部门那条的预览部门 = 缺省部门（且显示部门名，不是空串）。
        let homeless = item_of(&res, &no_dept);
        assert_eq!(homeless.dept_id.as_deref(), Some(dept_a.to_string().as_str()));
        assert!(
            !homeless.dept_name.trim().is_empty(),
            "无部门账号的预览要显示缺省部门名"
        );

        // ② 不传 dept_id：无部门账号 DEPT_MISSING（不猜部门）。
        let req_none = EmployeeSeedReq {
            dept_id: None,
            hire_date: None,
            dry_run: Some(true),
        };
        let res = employee_seed::seed_from_users(&principal_all, None, &req_none)
            .await
            .expect("不带部门预览");
        let homeless = item_of(&res, &no_dept);
        assert_eq!(homeless.reason.as_deref(), Some("DEPT_MISSING"));
        assert!(homeless.message.contains("dept_id"), "{}", homeless.message);

        // ③ 数据范围只到甲部门的人：乙部门那个连候选都不该出现。
        let scoped = ScopedPrincipal::for_test(
            Uuid::now_v7(),
            Some(dept_a),
            false,
            ScopeKind::Dept,
            vec![dept_a],
            false,
        );
        let res = employee_seed::seed_from_users(&scoped, None, &req_none)
            .await
            .expect("带范围预览");
        let names: Vec<&str> = res.preview.iter().map(|i| i.username.as_str()).collect();
        assert!(!names.contains(&in_b.as_str()), "数据范围外的账号不该成为候选：{res:?}");

        clean(&users, &[code_a, code_b]).await;
    }
}
