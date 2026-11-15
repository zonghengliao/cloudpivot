//! 工号取号的真库用例（HR-14）：不重复 / 不跳号 / 并发安全 / 回滚不耗号 / 撞号顺延。
//!
//! 单元测试（[`crate::business::hr::employee_no`] 的 `mod tests`）能证明号段登记、
//! 出参形状、路由与契约对齐，但证明不了本卡**唯一真正重要**的四件事 —— 它们全在
//! `biz_seq` 行锁 + `biz_employee.emp_no` 唯一键之间：
//!
//! 1. **不重复**：6 个事务同时取号，拿到的工号互不相同；
//! 2. **不跳号**：正常新建 5 个人，工号是 `n+1 … n+5` 连续（取号与 INSERT 同事务）；
//! 3. **失败不耗号**：新建在 INSERT 阶段失败（撞身份证）⇒ `current_no` 回到原值，
//!    下一次新建拿到的是**同一个**号（不是「跳过一格」）；
//! 4. **撞号顺延**：老库里已有 `EMP-0007`（手工/历史档案）⇒ 取号跳到 `EMP-0008`，
//!    不会撞 `uk_biz_employee_no`。
//!
//! 不设环境变量时全部 SKIP（不连真库，也不假装绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr14_card \
//!   cargo test --lib business::hr::employee_no_db -- --test-threads=1
//! ```
//!
//! # 夹具纪律
//!
//! * 员工全部用 `HR14-<唯一后缀>` 造姓名 / 身份证 / 部门码，清场**按 id 精确删**；
//! * 用例之间**共享同一个 `EMP` 号段**（生产号段就是它，不能另开一条测试号段 ——
//!   那样测的就不是真实配置了），所以每个用例开头 `isolate_baseline()`：清掉本卡
//!   上一轮 panic 遗留的夹具、把 `current_no` **只抬不降**地推到「库里已有 `EMP-` 工号
//!   的最大流水」之上，并在结尾恢复快照 —— 于是「连续 +1」这条硬断言在**任何**库上
//!   都成立（老库有 `EMP-0007` 也不会被重发）；
//! * 号段是全局的 ⇒ 真库用例必须 `--test-threads=1` 串行跑（同 HR-12 的真库用例）。

use nik::prelude::*;

mod db {
    use super::*;
    use crate::business::hr::employee_handler as handler;
    use crate::business::hr::employee_no;
    use crate::shared::test_helpers;
    use nik::uuid::Uuid;
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址，用例直接跳过。
    ///
    /// 连接池放大到 16：并发用例要同时开 6 个事务，默认 10 太贴着上限。
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
                config.pool.max_connections = 16;
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

    /// 唯一后缀（纳秒 + 进程内计数器）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    /// 身份证号（按整段 tag 哈希：不同后缀绝不撞同一张证 —— HR-10 踩过的坑）。
    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        format!("110101{:010}", hash % 10_000_000_000)
    }

    /// 本用例专属部门（code 唯一，清场按 code 删）。
    async fn fresh_dept(suffix: &str, tag: &str) -> (Uuid, String) {
        let code = format!("HR14-{tag}-{suffix}");
        let id = test_helpers::ensure_dept(&code, &format!("HR14 部门 {tag}")).await;
        (id, code)
    }

    /// 一份最小可用新建入参（`emp_no` 由调用方给；`None` = 交给取号器）。
    ///
    /// `remark` 打上 `HR14-FIXTURE-<后缀>`：上一轮 panic 没清场留下的孤儿行，
    /// 下一次跑由 [`isolate_baseline`] 按这个标记清掉（按 id 清是主路径，标记是兜底）。
    fn create_req(suffix: &str, dept: Uuid, emp_no: Option<String>) -> handler::EmployeeCreateReq {
        handler::EmployeeCreateReq {
            emp_no,
            name: Some(format!("员工{suffix}")),
            gender: Some(1),
            id_card: Some(id_card_of(suffix)),
            mobile: Some("13900000000".to_string()),
            dept_id: Some(dept.to_string()),
            hire_date: Some("2024-03-01".to_string()),
            remark: Some(format!("HR14-FIXTURE-{suffix}")),
            ..handler::EmployeeCreateReq::default()
        }
    }

    /// 安全转 u32：取号流水一定在 `u32` 内，越界只可能是库被改坏了。
    fn suffix_of(seq: &str) -> u32 {
        seq.rsplit('-')
            .next()
            .and_then(|tail| tail.parse::<u32>().ok())
            .unwrap_or_else(|| panic!("工号形态应为 EMP-0001，实际 {seq}"))
    }

    /// 当前 `biz_seq.current_no`（`EMP` 行）。
    async fn current_no() -> i64 {
        let row = db()
            .query_first_sql(
                "SELECT current_no::bigint AS current_no FROM biz_seq \
                  WHERE seq_type = 'EMP' AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查 EMP 号段")
            .expect("biz_seq 里必须有 EMP 号段（V0386 未应用？）");
        row.get_typed::<i64>("current_no").expect("current_no")
    }

    /// 直接把 `current_no` 写回快照值（清场用：不留下被试号段推进过的痕迹）。
    async fn restore_current_no(no: i64) {
        db().exec_sql(
            "UPDATE biz_seq SET current_no = ? WHERE seq_type = 'EMP' AND deleted_at IS NULL".to_string(),
            vec![Value::from(no)],
        )
        .await
        .expect("恢复 EMP 号段快照");
    }

    /// 把「本次跑」的号段基线整理干净，返回要恢复的快照值。
    ///
    /// 三件事：
    ///
    /// 1. 清掉上一轮 **panic 没清场**留下的本卡夹具（`remark LIKE 'HR14-FIXTURE-%'`）——
    ///    不清就会让「连号 +1」的断言看到别人占过的号；
    /// 2. 把 `current_no` 抬到「库里已有 `EMP-` 工号的最大流水」之上（**只抬不降**）——
    ///    老库里手工建的 `EMP-0007` 不会被重新发一次；
    /// 3. 返回这个值作为快照，用例结束时 [`cleanup`] 恢复它。
    async fn isolate_baseline() -> i64 {
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_employee WHERE remark LIKE 'HR14-FIXTURE-%'".to_string(),
                vec![],
            )
            .await
            .expect("清上一轮遗留夹具");
        let row = db()
            .query_first_sql(
                "SELECT COALESCE(MAX(NULLIF(substring(emp_no from '^EMP-([0-9]+)$'), '')::bigint), 0)::bigint AS max_no \
                   FROM biz_employee WHERE emp_no LIKE 'EMP-%' AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("算已有工号最大流水")
            .expect("聚合一定有一行");
        let max_no = row.get_typed::<i64>("max_no").expect("max_no");
        let current = current_no().await;
        let baseline = current.max(max_no);
        if baseline != current {
            restore_current_no(baseline).await;
        }
        println!("[HR-14] 基线：已有 EMP 最大流水={max_no}，current_no {current} → {baseline}");
        baseline
    }

    /// 清场：按 id 删员工（+ 防御性清状态日志），按 code 删部门，最后恢复号段快照。
    async fn cleanup(employee_ids: &[Uuid], dept_codes: &[String], snapshot: i64) {
        for id in employee_ids {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
        }
        for code in dept_codes {
            test_helpers::purge_dept(code).await;
        }
        restore_current_no(snapshot).await;
    }

    // ==================== ① 号段口径 + 预览不耗号 ====================

    /// `EMP` 号段就是设计定的那一条：`EMP` / `none` / `GLOBAL` / 4 位 / 启用。
    /// 预览（`next-no`）**不动** `current_no`：点十次按钮得到的都是同一个号。
    #[tokio::test]
    async fn emp_seq_rule_matches_design_and_preview_is_free() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let row = db()
            .query_first_sql(
                "SELECT prefix, date_pattern, period_key, pad_width::int AS pad_width, \
                        separator, status::int AS status \
                   FROM biz_seq WHERE seq_type = 'EMP' AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查 EMP 规则")
            .expect("EMP 号段行");
        assert_eq!(row.get_typed::<String>("prefix").expect("prefix"), "EMP");
        assert_eq!(
            row.get_typed::<String>("date_pattern").expect("date_pattern"),
            "none",
            "工号不带日期段（D-15）"
        );
        assert_eq!(
            row.get_typed::<String>("period_key").expect("period_key"),
            "GLOBAL",
            "工号永不按期间重置"
        );
        assert_eq!(
            row.get_typed::<i32>("pad_width").expect("pad_width"),
            4,
            "EMP-0001 四位"
        );
        assert_eq!(row.get_typed::<String>("separator").expect("separator"), "-");
        assert_eq!(row.get_typed::<i32>("status").expect("status"), 1, "号段启用中");

        let first = employee_no::preview_emp_no().await.expect("预览工号");
        let second = employee_no::preview_emp_no().await.expect("再预览一次");
        assert_eq!(first, second, "预览不耗号：连点两次是同一个号");
        assert_eq!(current_no().await, snapshot, "预览之后 current_no 不动");
        assert!(first.starts_with("EMP-"), "预览形态 EMP-xxxx：{first}");
        println!("[HR-14] ① 号段口径 = EMP/none/GLOBAL/4/启用，预览={first}（current_no 未动）");
    }

    // ==================== ② 连续新建：不重复、不跳号 ====================

    /// 连建 5 个人（`emp_no` 留空）⇒ 工号 `n+1 … n+5` 连续、无重复、无空号。
    #[tokio::test]
    async fn consecutive_creates_are_unique_and_gap_free() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "CONS").await;

        let mut created: Vec<Uuid> = Vec::new();
        let mut numbers: Vec<u32> = Vec::new();
        for round in 0..5 {
            let req = create_req(&format!("{suffix}-{round}"), dept, None);
            let res = handler::create_employee(None, &req)
                .await
                .expect("新建员工（自动取号）");
            let id = Uuid::parse_str(&res.id).expect("回执 id");
            created.push(id);
            assert!(
                res.emp_no.starts_with("EMP-"),
                "自动取号形态应为 EMP-xxxx，实际 {}",
                res.emp_no
            );
            numbers.push(suffix_of(&res.emp_no));
        }
        println!("[HR-14] ② 连建 5 个工号 = {numbers:?}");

        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), numbers.len(), "工号不得重复：{numbers:?}");

        for pair in sorted.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "不跳号：相邻两次取号必须连续（{numbers:?}）");
        }

        // 员工真的落库了（取号不是「只发了号」）
        let stored = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_employee \
                  WHERE deleted_at IS NULL AND id = ANY(string_to_array(?, ',')::uuid[])"
                    .to_string(),
                vec![Value::Text(
                    created.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(","),
                )],
            )
            .await
            .expect("数落库员工")
            .expect("count");
        assert_eq!(stored.get_typed::<i64>("n").expect("n"), 5, "5 个员工全部落库");

        cleanup(&created, &[dept_code], snapshot).await;
    }

    // ==================== ③ 并发取号：不重复、不冲突 ====================

    /// 6 个事务同时「取号 + 插入 + 提交」⇒ 拿到的工号互不相同且连续。
    ///
    /// 这条用例证明的是 `biz_seq` 行锁：`UPDATE … RETURNING` 让并发事务排队，
    /// 后来者看到的是前者**已提交**的员工行（查重能命中）⇒ 不可能发同一个号。
    #[tokio::test]
    async fn concurrent_creates_never_collide() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "CONC").await;

        let mut handles = Vec::new();
        for task in 0..6 {
            let req = create_req(&format!("{suffix}-c{task}"), dept, None);
            handles.push(tokio::spawn(async move { handler::create_employee(None, &req).await }));
        }
        let mut numbers: Vec<u32> = Vec::new();
        let mut created: Vec<Uuid> = Vec::new();
        for (task, handle) in handles.into_iter().enumerate() {
            let res = handle
                .await
                .expect("并发任务不 panic")
                .unwrap_or_else(|error| panic!("并发新建第 {task} 个失败：{}", error.msg));
            created.push(Uuid::parse_str(&res.id).expect("回执 id"));
            numbers.push(suffix_of(&res.emp_no));
        }
        numbers.sort_unstable();
        println!("[HR-14] ③ 并发 6 个工号 = {numbers:?}");

        let mut distinct = numbers.clone();
        distinct.dedup();
        assert_eq!(distinct.len(), 6, "并发取号不得重号：{numbers:?}");
        for pair in numbers.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "并发下也不能跳号：{numbers:?}");
        }

        cleanup(&created, &[dept_code], snapshot).await;
    }

    // ==================== ④ 回滚不耗号 ====================

    /// 事务里取号后回滚 ⇒ `current_no` 回到原值，下一次取号拿到**同一个**号。
    #[tokio::test]
    async fn rollback_does_not_burn_a_number() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;

        let tx = db().begin().await.expect("开启事务");
        let first = employee_no::generate_emp_no(&tx).await.expect("事务内取号");
        tx.rollback().await.expect("回滚");
        assert_eq!(
            current_no().await,
            snapshot,
            "回滚后 current_no 必须回到快照（号不被白吃）"
        );

        let tx = db().begin().await.expect("再开事务");
        let second = employee_no::generate_emp_no(&tx).await.expect("再取一次号");
        tx.rollback().await.expect("回滚");
        assert_eq!(second, first, "回滚掉的号会被下一个事务重新拿到：{first} / {second}");
        assert_eq!(current_no().await, snapshot, "current_no 仍不动");
        println!("[HR-14] ④ 回滚不耗号：两次都拿到 {first}，current_no 保持 {snapshot}");
    }

    // ==================== ⑤ 撞号顺延 ====================

    /// 库里有 `EMP-<下一个号>`（历史/手工档案）⇒ 取号跳过它，不会撞唯一键。
    #[tokio::test]
    async fn taken_number_is_skipped() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "TAKEN").await;

        let candidate = employee_no::preview_emp_no().await.expect("预览");
        println!("[HR-14] ⑤ 预览候选 = {candidate}");

        // 手工占掉这个号（模拟老库/手工档案）
        let blocker =
            handler::create_employee(None, &create_req(&format!("{suffix}-b"), dept, Some(candidate.clone())))
                .await
                .expect("手工工号建档");
        let blocker_id = Uuid::parse_str(&blocker.id).expect("回执 id");
        assert_eq!(blocker.emp_no, candidate, "手工工号原样保留");

        // 再自动取号 ⇒ 必须跳过被占的号
        let taken = handler::create_employee(None, &create_req(&format!("{suffix}-a"), dept, None))
            .await
            .expect("自动取号（撞号顺延）");
        let taken_id = Uuid::parse_str(&taken.id).expect("回执 id");
        println!("[HR-14] ⑤ 顺延后 = {}（被占 {}）", taken.emp_no, candidate);
        assert_ne!(taken.emp_no, candidate, "自动取号不得发出已被占用的号");
        assert!(
            suffix_of(&taken.emp_no) > suffix_of(&candidate),
            "顺延：{} 应大于 {}",
            taken.emp_no,
            candidate
        );

        cleanup(&[blocker_id, taken_id], &[dept_code], snapshot).await;
    }

    // ==================== ⑥ 取号与 INSERT 同事务（失败不跳号） ====================

    /// A 自动取号成功；B 用**同一张身份证**新建（INSERT 阶段撞 `uk_biz_employee_idcard`）
    /// ⇒ B 失败，但 `current_no` 回到 A 之后的值；C 拿到的号正好是 **A+1**
    /// （没有为 B 白留一格）。这是本卡「不许跳号」的核心断言。
    #[tokio::test]
    async fn failed_create_does_not_skip_a_number() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "TXN").await;

        let req = create_req(&format!("{suffix}-a"), dept, None);
        let a = handler::create_employee(None, &req).await.expect("新建 A");
        let a_no = suffix_of(&a.emp_no);
        let after_a = current_no().await;
        assert_eq!(after_a, snapshot + 1, "A 之后 current_no 前进一格");

        // B：同身份证（撞 uk_biz_employee_idcard），INSERT 阶段才失败 —— 取号已经发生过了
        let mut b_req = create_req(&format!("{suffix}-a"), dept, None);
        b_req.name = Some(format!("员工B{suffix}"));
        let b_error = handler::create_employee(None, &b_req)
            .await
            .expect_err("同身份证必须失败");
        assert_eq!(b_error.status, 409, "身份证冲突应是 409，实际 {}", b_error.msg);
        assert_eq!(
            current_no().await,
            after_a,
            "B 失败后 current_no 必须回到 A 之后（不许为失败的新建白留一格）"
        );

        let c = handler::create_employee(None, &create_req(&format!("{suffix}-c"), dept, None))
            .await
            .expect("新建 C");
        let c_no = suffix_of(&c.emp_no);
        println!("[HR-14] ⑥ A={} B=失败 C={}（after_a={after_a}）", a.emp_no, c.emp_no);
        assert_eq!(c_no, a_no + 1, "失败的新建不占号：C 应正好是 A+1 —— 否则就是跳号");

        cleanup(
            &[
                Uuid::parse_str(&a.id).expect("id A"),
                Uuid::parse_str(&c.id).expect("id C"),
            ],
            &[dept_code],
            snapshot,
        )
        .await;
    }

    // ==================== ⑦ create 走真库：手工号 / 自动号各一条 ====================

    /// 手工工号（企业自有 `E2003`）与自动工号能在同一张表里共存，且自动号不清空手工号。
    #[tokio::test]
    async fn manual_and_auto_numbers_coexist() {
        if !ensure_db().await {
            return;
        }
        let snapshot = isolate_baseline().await;
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "MIX").await;

        let manual_no = format!("E{}", &suffix[&suffix.len() - 6..]);
        let manual = handler::create_employee(None, &create_req(&format!("{suffix}-m"), dept, Some(manual_no.clone())))
            .await
            .expect("手工工号建档");
        assert_eq!(manual.emp_no, manual_no, "手工工号原样保留");
        assert_eq!(current_no().await, snapshot, "手工工号不消耗 EMP 号段");

        let auto = handler::create_employee(None, &create_req(&format!("{suffix}-a"), dept, None))
            .await
            .expect("自动取号");
        assert!(auto.emp_no.starts_with("EMP-"), "自动号形态：{}", auto.emp_no);
        println!("[HR-14] ⑦ 手工={}（不耗号） 自动={}（+1 格）", manual_no, auto.emp_no);

        cleanup(
            &[
                Uuid::parse_str(&manual.id).expect("id manual"),
                Uuid::parse_str(&auto.id).expect("id auto"),
            ],
            &[dept_code],
            snapshot,
        )
        .await;
    }
}
