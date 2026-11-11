//! 员工主档（`biz_employee`）的真库用例（HR-10）。
//!
//! 单元测试（`employee_model.rs` 的 `mod tests`）证明**映射逻辑**是对的，但证明不了两件事：
//!
//! 1. PG 实际报出来的约束名是不是 `uk_biz_employee_no` / `uk_biz_employee_idcard` /
//!    `uk_biz_employee_user`（驱动从报文里解析约束名，解析规则一改就静默失效）；
//! 2. CRUD 的 SQL 真的能跑（36 个占位符、`NULLIF(?::text,'')::date` 这类写法只有真跑才知道）。
//!
//! 所以本文件**必须真跑**，不设环境变量时全部 SKIP：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr10_card \
//!   cargo test --lib business::hr::employee_model_db -- --test-threads=1
//! ```
//!
//! 环境变量优先取 `HR_DB_URL`，回退 `ERP_DB_URL`（与 ERP 真库用例共用同一台演练库，
//! 不必为 HR 再配一套）。库要求：迁移已跑完（`cargo run` 一次即可，见 `hr/model.rs` 的体检探针）。
//!
//! # 夹具纪律
//!
//! 全部用 `HR10-<唯一后缀>` 造，清场**按 id** 删（绝不按前缀扫全库）；
//! 工号 / 身份证 / 账号三者都带唯一后缀，重跑不会互相踩。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::employee_model as model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::NaiveDate;
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

    /// 唯一后缀（纳秒 + 进程内计数器，避免并发用例互相踩）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    /// 取后缀里的数字尾巴（`emp_no` / `mobile` 都有长度上限）。
    fn digits(suffix: &str) -> String {
        suffix.chars().filter(char::is_ascii_digit).collect()
    }

    /// 一个合法身份证号（18 位以内：6 位地址 + 8 位生日 + 6 位顺序）。
    ///
    /// **必须按整段 tag 求值**（不是只取数字）：`draft("a123")` 与 `draft("b123")`
    /// 只差一个前缀字母，若身份证只由数字算，两者会撞同一张身份证 —— 夹具自己
    /// 撞出来的 `uk_biz_employee_idcard` 会让「想测工号冲突」的用例红在错误的地方
    /// （本卡第一轮真库跑就是这么红的）。
    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        // 6 位顺序号：不同 tag 撞同一张身份证的概率 1e-6，够用且仍在 18 位长度内
        format!("1101011990{:06}", hash % 1_000_000)
    }

    /// 造一个**本用例专属**的登录账号（`sys_user` 一行），并返回 id。
    ///
    /// 不用 `ensure_dept_user()` 的共享账号：`uk_biz_employee_user` 是全局唯一，
    /// 共享账号会让「上一个用例还没清场」变成下一个用例红在账号冲突上（实测踩过）。
    async fn fresh_user(suffix: &str) -> Uuid {
        test_helpers::ensure_user(&format!("hr10.{suffix}"), "HR10 夹具员工", None).await
    }

    /// 造一份草稿（`emp_no` 带唯一后缀）。
    fn draft(suffix: &str, dept: Uuid, user: Option<Uuid>) -> model::EmployeeWrite {
        model::EmployeeWrite {
            emp_no: format!("HR10-{suffix}"),
            name: format!("员工{suffix}"),
            gender: 1,
            birth_date: NaiveDate::from_ymd_opt(1990, 1, 1),
            id_card: Some(id_card_of(suffix)),
            id_card_type: "ID_CARD".to_string(),
            mobile: Some(format!("138{}", &digits(suffix)[..8.min(digits(suffix).len())])),
            email: Some(format!("hr10-{suffix}@example.com")),
            dept_id: dept,
            position_code: Some("DEV".to_string()),
            job_level: Some("P6".to_string()),
            employment_type: "FULL_TIME".to_string(),
            hire_date: NaiveDate::from_ymd_opt(2026, 1, 5).expect("合法日期"),
            probation_end_date: NaiveDate::from_ymd_opt(2026, 4, 5),
            education: Some("BACHELOR".to_string()),
            school: Some("某大学".to_string()),
            major: Some("计算机".to_string()),
            graduate_date: NaiveDate::from_ymd_opt(2012, 6, 30),
            native_place: Some("北京".to_string()),
            address: Some("北京市海淀区某路 1 号".to_string()),
            emergency_contact: Some("紧急联系人".to_string()),
            emergency_phone: Some("13900000000".to_string()),
            emergency_relation: Some("配偶".to_string()),
            bank_name: Some("工商银行".to_string()),
            bank_branch: Some("中关村支行".to_string()),
            bank_account: Some("6222020000000000001".to_string()),
            bank_account_name: Some(format!("员工{suffix}")),
            user_id: user,
            remark: Some("HR-10 真库用例夹具".to_string()),
        }
    }

    /// 按 id 硬删（测试夹具专用；业务侧只软删）。
    async fn drop_rows(ids: &[Uuid]) {
        for id in ids {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
        }
    }

    // ==================== ① CRUD 全通 ====================

    /// 建 → 读 → 改 → 软删 → 键复用：一次跑完 `biz_employee` 的完整生命周期。
    ///
    /// 覆盖三件容易做错的事：
    /// - 详情能 join 到部门名与账号名（AGENTS.md 约定 13：主数据 join 实时取名）；
    /// - 改完之后 `emp_no` **没变**（工号不可改）；
    /// - 软删之后三个唯一键都**释放**（同工号 / 同身份证 / 同账号可以重新建档 = 离职返聘）。
    #[tokio::test]
    async fn crud_roundtrip_and_soft_delete_frees_unique_keys() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let user = fresh_user(&suffix).await;
        let operator = Some(user);

        let first = Uuid::now_v7();
        let mut write = draft(&suffix, dept, Some(user));
        model::insert(db(), first, &write, operator).await.expect("① 建员工");

        // ② 读：全字段 + 部门名 / 账号名（join 实时）
        let row = model::find(first).await.expect("② 查详情").expect("刚建的行应当能查到");
        assert_eq!(row.emp_no, write.emp_no);
        assert_eq!(row.name, write.name);
        assert_eq!(row.status, "PROBATION", "新建默认试用期");
        assert_eq!(row.dept_id, dept);
        assert!(!row.dept_name.is_empty(), "部门名必须 join 到（否则前端显示短 uuid）");
        assert_eq!(row.user_id, Some(user));
        assert!(!row.username.is_empty(), "账号名必须 join 到");
        assert_eq!(
            row.id_card.as_deref(),
            Some(write.id_card.clone().expect("夹具带身份证").as_str())
        );
        assert_eq!(row.create_by, operator);
        assert!(row.leave_date.is_none());

        // 唯一键查重（新建前置校验用）
        let brief = model::existing_by_emp_no(db(), &write.emp_no)
            .await
            .expect("查重")
            .expect("应当查得到");
        assert_eq!(brief.id, first);
        assert_eq!(
            model::existing_by_id_card(db(), write.id_card.as_deref().expect("身份证"))
                .await
                .expect("按身份证查重")
                .expect("应当查得到")
                .id,
            first
        );

        // 列表：关键字命中工号（超管 principal：列表还要验证 scope 片段能正常注入）
        let principal = ScopedPrincipal::for_test(user, Some(dept), true, ScopeKind::All, vec![], false);
        let (list, total) = model::page(
            &model::EmployeeFilter {
                keyword: Some(write.emp_no.clone()),
                ..model::EmployeeFilter::default()
            },
            1,
            10,
            &principal,
        )
        .await
        .expect("③ 列表");
        assert!(total >= 1);
        let listed = list.iter().find(|item| item.id == first).expect("列表里应当有新员工");
        assert_eq!(listed.dept_name, row.dept_name);
        assert_eq!(listed.username, row.username);

        // ④ 改：只改能改的列（姓名 / 手机 / 备注），工号不许跟着变
        let original_no = write.emp_no.clone();
        write.name = format!("改名{suffix}");
        write.mobile = Some("13700000000".to_string());
        write.remark = Some("改过".to_string());
        write.emp_no = "HR10-SHOULD-NOT-APPLY".to_string();
        let affected = model::update(db(), first, &write, operator).await.expect("④ 改");
        assert_eq!(affected, 1, "改一行应当返回 1");
        let after = model::find(first).await.expect("改后读回").expect("改后还在");
        assert_eq!(after.name, format!("改名{suffix}"));
        assert_eq!(after.mobile.as_deref(), Some("13700000000"));
        assert_eq!(after.emp_no, original_no, "工号不可改（UPDATE_SQL 不含 emp_no）");

        // 改不存在的 id → 0 行（调用方据此报 404，而不是假成功）
        let ghost = Uuid::now_v7();
        assert_eq!(
            model::update(db(), ghost, &write, operator)
                .await
                .expect("改不存在的行"),
            0
        );

        // ⑤ 软删
        assert_eq!(model::soft_delete(db(), first, operator).await.expect("⑤ 软删"), 1);
        assert!(
            model::find(first).await.expect("软删后查").is_none(),
            "软删后详情查不到"
        );
        assert!(
            model::existing_by_emp_no(db(), &original_no)
                .await
                .expect("软删后查重")
                .is_none(),
            "软删后工号查重应当为空"
        );
        assert_eq!(
            model::soft_delete(db(), first, operator).await.expect("重复软删"),
            0,
            "已删的行再删一次返回 0（幂等）"
        );

        // ⑥ 三个唯一键释放：同工号 / 同身份证 / 同账号可以重新建档（离职返聘）
        let second = Uuid::now_v7();
        let mut revive = draft(&suffix, dept, Some(user));
        revive.emp_no = original_no.clone();
        revive.id_card = Some(id_card_of(&suffix));
        model::insert(db(), second, &revive, operator)
            .await
            .expect("软删后同工号/同身份证/同账号必须可以复用");

        drop_rows(&[first, second]).await;
    }

    // ==================== ② 三个唯一键的真实冲突 ====================

    /// 工号重复 → `uk_biz_employee_no` → `409 + 68001`，报文点名工号。
    #[tokio::test]
    async fn duplicate_emp_no_translates_to_68001() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let mut one = draft(&suffix, dept, None);
        let mut two = draft(&format!("b{suffix}"), dept, None);
        // 身份证由 tag 哈希生成，`b…` 与基准 already 不同 —— 这里只让工号相同
        assert_ne!(one.id_card, two.id_card, "夹具自检：两行的身份证必须不同");
        two.emp_no = one.emp_no.clone();
        one.user_id = None;
        two.user_id = None;

        model::insert(db(), first, &one, None).await.expect("第一条");
        let err = model::insert(db(), second, &two, None)
            .await
            .expect_err("同工号必须被拒");
        let key = model::classify_unique(match &err {
            DbError::UniqueViolation { constraint, .. } => constraint.as_deref(),
            other => panic!("期望 UniqueViolation，实际 {other:?}"),
        })
        .expect("约束名必须能被识别");
        assert_eq!(key, model::EmployeeUniqueKey::EmpNo, "应当识别成工号冲突");

        let api = model::write_error(err, "hr_employee_insert", &model::WriteValues::of(&two));
        assert_eq!(api.status, 409);
        assert_eq!(api.code, crate::shared::error_code::HR_EMPLOYEE_NO_DUPLICATE);
        assert!(api.msg.contains(&two.emp_no), "报文要点名撞的工号：{}", api.msg);
        assert!(api.msg.contains("工号"), "报文要说人话：{}", api.msg);

        drop_rows(&[first]).await;
    }

    /// 身份证重复 → `uk_biz_employee_idcard` → `409 + 68002`，且报文里**不出现明文身份证**。
    #[tokio::test]
    async fn duplicate_id_card_translates_to_68002_without_leaking_plaintext() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let one = draft(&format!("i{suffix}"), dept, None);
        let mut two = draft(&format!("j{suffix}"), dept, None);
        two.id_card = one.id_card.clone(); // 只有身份证相同
        two.emp_no = format!("HR10-J{suffix}");

        model::insert(db(), first, &one, None).await.expect("第一条");
        let err = model::insert(db(), second, &two, None)
            .await
            .expect_err("同身份证必须被拒");
        let plaintext = two.id_card.clone().expect("夹具带身份证");
        let key = model::classify_unique(match &err {
            DbError::UniqueViolation { constraint, .. } => constraint.as_deref(),
            other => panic!("期望 UniqueViolation，实际 {other:?}"),
        })
        .expect("约束名必须能被识别");
        assert_eq!(key, model::EmployeeUniqueKey::IdCard);

        let api = model::write_error(err, "hr_employee_insert", &model::WriteValues::of(&two));
        assert_eq!(api.status, 409);
        assert_eq!(api.code, crate::shared::error_code::HR_EMPLOYEE_ID_CARD_DUPLICATE);
        assert!(api.msg.contains("身份证"), "报文要说人话：{}", api.msg);
        assert!(
            !api.msg.contains(&plaintext),
            "报文绝不许出现明文身份证（设计 D-14）：{}",
            api.msg
        );
        assert!(
            api.msg.contains(&model::mask_id_card(&plaintext)),
            "报文要点名脱敏后的身份证：{}",
            api.msg
        );

        drop_rows(&[first]).await;
    }

    /// 账号重复 → `uk_biz_employee_user` → `409 + 68003`；两条路径都要翻：
    /// ①新建时就带重复账号；②`bind-user` 把账号改绑到另一个已有档案。
    #[tokio::test]
    async fn duplicate_user_translates_to_68003() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let user = fresh_user(&suffix).await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();

        let one = draft(&format!("u{suffix}"), dept, Some(user));
        model::insert(db(), first, &one, None).await.expect("第一条（带账号）");

        // ① 新建时同账号 → 冲突（身份证由 tag 哈希生成，`v…` 与基准不同，只撞账号）
        let mut two = draft(&format!("v{suffix}"), dept, Some(user));
        two.emp_no = format!("HR10-V{suffix}");
        let err = model::insert(db(), second, &two, None)
            .await
            .expect_err("同账号必须被拒");
        let api = model::write_error(err, "hr_employee_insert", &model::WriteValues::of(&two));
        assert_eq!(api.status, 409);
        assert_eq!(api.code, crate::shared::error_code::HR_EMPLOYEE_ACCOUNT_BOUND);
        assert!(api.msg.contains("账号"), "报文要说人话：{}", api.msg);

        // ② 先建一个没账号的员工，再 bind-user 到同一个账号 → 同一个约束、同一个码
        let plain = draft(&format!("w{suffix}"), dept, None);
        model::insert(db(), second, &plain, None)
            .await
            .expect("第二条（无账号）");
        let err = model::set_user_id(db(), second, Some(user), None)
            .await
            .expect_err("改绑到已占用账号必须被拒");
        let api = model::write_error(err, "hr_employee_bind_user", &model::WriteValues::default());
        assert_eq!(api.status, 409);
        assert_eq!(api.code, crate::shared::error_code::HR_EMPLOYEE_ACCOUNT_BOUND);

        drop_rows(&[first, second]).await;
    }

    // ==================== ③ CHECK 约束与状态 CAS ====================

    /// 「标了离职但没给离职日期」→ 库里的 `ck_biz_employee_leave` 拒绝 → `400` + 人话。
    ///
    /// 这条不是唯一键，但同一套 `write_error` 出口要能翻 —— 否则用户拿到的是 500。
    #[tokio::test]
    async fn resign_without_date_is_a_check_violation_in_human_words() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let id = Uuid::now_v7();
        let write = draft(&format!("r{suffix}"), dept, None);

        let err = model::insert_with_status(db(), id, &write, None, "RESIGNED", None, None)
            .await
            .expect_err("状态 RESIGNED 但无离职日期必须被库拒掉");
        let api = model::write_error(err, "hr_employee_insert", &model::WriteValues::of(&write));
        assert_eq!(api.status, 400);
        assert_eq!(api.code, crate::shared::error_code::PARAM_INVALID);
        assert!(api.msg.contains("离职日期"), "报文要点名该改哪个字段：{}", api.msg);

        // 补上离职日期与类型 → 能建（同一个 CHECK 的反向也成立）
        model::insert_with_status(
            db(),
            id,
            &write,
            None,
            "RESIGNED",
            Some(NaiveDate::from_ymd_opt(2026, 6, 30).expect("合法日期")),
            Some("RESIGN"),
        )
        .await
        .expect("补全离职三列后应当能建");
        let row = model::find(id).await.expect("查").expect("刚建的行");
        assert_eq!(row.status, "RESIGNED");
        assert_eq!(row.leave_type.as_deref(), Some("RESIGN"));

        drop_rows(&[id]).await;
    }

    /// 状态回写是 CAS：拿旧的 `from_status` 做条件，并发第二个请求改不动（返回 0 行）。
    #[tokio::test]
    async fn apply_status_is_a_cas_update() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let id = Uuid::now_v7();
        let write = draft(&format!("s{suffix}"), dept, None);
        model::insert(db(), id, &write, None).await.expect("建员工");

        // 试用期 → 在职
        assert_eq!(
            model::apply_status(db(), id, "PROBATION", "ACTIVE", None, None, None)
                .await
                .expect("转正"),
            1
        );
        assert_eq!(model::find(id).await.expect("查").expect("还在").status, "ACTIVE");

        // 第二个请求仍拿着旧状态 PROBATION → 0 行（乐观锁生效）
        assert_eq!(
            model::apply_status(db(), id, "PROBATION", "ACTIVE", None, None, None)
                .await
                .expect("重复流转"),
            0,
            "旧状态作为条件时必须改不动（并发安全）"
        );

        // 在职 → 离职：离职日期 / 类型必须一写（否则 CHECK 会拒）
        assert_eq!(
            model::apply_status(
                db(),
                id,
                "ACTIVE",
                "RESIGNED",
                Some(NaiveDate::from_ymd_opt(2026, 5, 31).expect("合法日期")),
                Some("RESIGN"),
                None
            )
            .await
            .expect("离职"),
            1
        );
        let row = model::find(id).await.expect("查").expect("还在");
        assert_eq!(row.status, "RESIGNED");
        assert!(row.leave_date.is_some());

        drop_rows(&[id]).await;
    }

    /// 三个唯一键必须**互不误判**：同一张表上，工号冲突不能翻成身份证冲突。
    ///
    /// 做法：一次跑三条冲突，逐条比对业务码 —— 若 `classify_unique` 的匹配有重叠
    /// （比如用了 `starts_with`），这里会当场红。
    #[tokio::test]
    async fn three_keys_are_mutually_exclusive() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let user = fresh_user(&suffix).await;
        let base = draft(&format!("x{suffix}"), dept, Some(user));
        let base_id = Uuid::now_v7();
        model::insert(db(), base_id, &base, None).await.expect("基线");

        // 只撞工号（`y…` 的身份证由 tag 哈希得到，与基线不同）
        let mut by_no = draft(&format!("y{suffix}"), dept, None);
        by_no.emp_no = base.emp_no.clone();
        let plain_id = Uuid::now_v7();
        let err = model::insert(db(), plain_id, &by_no, None).await.expect_err("撞工号");
        assert_eq!(
            model::write_error(err, "hr", &model::WriteValues::of(&by_no)).code,
            crate::shared::error_code::HR_EMPLOYEE_NO_DUPLICATE
        );

        // 只撞身份证
        let mut by_card = draft(&format!("z{suffix}"), dept, None);
        by_card.emp_no = format!("HR10-Z{suffix}");
        by_card.id_card = base.id_card.clone();
        let err = model::insert(db(), Uuid::now_v7(), &by_card, None)
            .await
            .expect_err("撞身份证");
        assert_eq!(
            model::write_error(err, "hr", &model::WriteValues::of(&by_card)).code,
            crate::shared::error_code::HR_EMPLOYEE_ID_CARD_DUPLICATE
        );

        // 只撞账号（`q…` 的身份证由 tag 哈希得到，与基线不同）
        let mut by_user = draft(&format!("q{suffix}"), dept, Some(user));
        by_user.emp_no = format!("HR10-Q{suffix}");
        let err = model::insert(db(), Uuid::now_v7(), &by_user, None)
            .await
            .expect_err("撞账号");
        assert_eq!(
            model::write_error(err, "hr", &model::WriteValues::of(&by_user)).code,
            crate::shared::error_code::HR_EMPLOYEE_ACCOUNT_BOUND
        );

        drop_rows(&[base_id, plain_id]).await;
    }
}
