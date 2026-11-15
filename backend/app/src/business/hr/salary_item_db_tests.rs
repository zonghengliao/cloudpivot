//! 薪资项（`biz_salary_item`）的真库用例（HR-31）。
//!
//! 单测（`salary_item_dto` / `salary_item_formula` / `salary_item_model`）证明的是
//! **判定与拼装**；证明不了五件事，只有真库能答：
//!
//! 1. 四条接口（#32 列表 / #33 新建 / #34 修改 / #35 检查）的 SQL、占位符、`?::smallint`
//!    写法真能跑，且**落库逐列等于入参**；
//! 2. **验收项：公式长度校验**（500 过 / 501 拒）在真实保存路径上生效，且拒的时候**一行都没落库**；
//! 3. **验收项：公式语义**（未定义变量 / 环）在保存路径上被拦，在检查接口上回 `ok=false` 而不是 400；
//! 4. 编码 / 计算顺序的唯一性在应用层与库层**都**生效（前者给人话，后者给 409）；
//! 5. 三道守卫（系统项不许停用、被公式引用的项不许停用、编码不可改）真的拦住，且**不留半成品**。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr31_drill \
//!   cargo test --lib business::hr::salary_item_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 / HR-15 / HR-16 的教训）
//!
//! - 编码带唯一后缀（`HR31...` + 纳秒尾数），`sort_order` 从 900000 起随机 —— 重跑不互相踩，
//!   也不撞 HR-35 将来种的 17 个种子项（10/20/…/170）；
//! - 清场**按精确编码删**（绝不按前缀扫全库），先删引用方再删被引用方（本表无 FK，顺序不致命，
//!   但保持「从叶子删起」的习惯）；
//! - 系统项（`is_system=1`）由迁移种下，测试里**手改一行**来模拟（接口不许设置这个列）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::salary_item_dto::{self, SalaryItemValidateReq, SalaryItemWriteReq};
    use crate::business::hr::salary_item_handler;
    use crate::business::hr::salary_item_model;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use crate::shared::test_helpers;
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
                    Ok(connection) => {
                        // 连上就打一行：`--nocapture` 时能证明这批用例**真的**跑了，
                        // 而不是走了下面的「未设库地址 / 连不上 → SKIP」分支（不假绿）。
                        // 只打库名：URL 里带口令，日志里不许出现凭据。
                        let database = url.rsplit('/').next().unwrap_or("?");
                        println!("HR31 真库用例已连接：database={database}");
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

    /// 夹具：一个操作人（真 `sys_user`，用来验 `*_by_name` 的 join）+ 一批编码。
    struct Fixture {
        username: String,
        operator: Operator,
        codes: Vec<String>,
    }

    async fn fixture(tag: &str) -> Fixture {
        let username = format!("hr31.{tag}.{}", uniq());
        let user = test_helpers::ensure_user(&username, "HR31 夹具操作人", None).await;
        Fixture {
            username,
            operator: Operator::named(Some(user), "HR31 夹具操作人"),
            codes: Vec::new(),
        }
    }

    impl Fixture {
        /// 取一个本用例专属编码（`HR31` + tag + 尾数；tag 截到 8 字符，总长 ≤ 32 且全大写）。
        fn code(&mut self, tag: &str) -> String {
            let suffix = uniq();
            let tag: String = tag.chars().filter(|ch| ch.is_ascii_alphanumeric()).take(8).collect();
            let code = format!("HR31{}{suffix}", tag.to_uppercase());
            assert!(
                code.chars().count() <= salary_item_dto::MAX_ITEM_CODE_LEN,
                "夹具编码超长：{code}"
            );
            self.codes.push(code.clone());
            code
        }

        /// 本用例专属计算顺序（避开种子项的 10..170 与别的并发用例）。
        fn sort(&self) -> i32 {
            let suffix: i64 = uniq().parse().unwrap_or(0);
            900_000 + (suffix % 90_000) as i32
        }

        /// 清场：按精确编码删（引用方先删，虽然本表无 FK）。
        async fn clean(&self) {
            for code in self.codes.iter().rev() {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_salary_item WHERE item_code = ?".to_string(),
                        vec![Value::Text(code.clone())],
                    )
                    .await;
            }
            test_helpers::purge_user(&self.username).await;
        }

        /// 库里该编码的行数（未软删）。
        async fn count(&self, code: &str) -> i64 {
            let row = db()
                .query_first_sql(
                    "SELECT count(*)::bigint AS n FROM biz_salary_item \
                      WHERE item_code = ? AND deleted_at IS NULL"
                        .to_string(),
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("行数统计")
                .expect("count 必有行");
            row.get_typed("n").expect("count 列")
        }
    }

    /// 一条 EARNING + FIXED（取薪资标准）的入参。
    fn fixed_item(code: &str, sort: i32) -> SalaryItemWriteReq {
        SalaryItemWriteReq {
            item_code: Some(code.to_string()),
            item_name: Some(format!("HR31 固定项 {code}")),
            item_type: Some("EARNING".to_string()),
            calc_type: Some("FIXED".to_string()),
            source: Some("EMPLOYEE_STANDARD".to_string()),
            formula: None,
            sort_order: Some(sort),
            debit_account_code: Some("5602.10".to_string()),
            credit_account_code: Some("2211.01".to_string()),
            ..SalaryItemWriteReq::default()
        }
    }

    /// 一条 EARNING + FORMULA 的入参。
    fn formula_item(code: &str, sort: i32, formula: &str) -> SalaryItemWriteReq {
        SalaryItemWriteReq {
            calc_type: Some("FORMULA".to_string()),
            source: Some("ATTENDANCE".to_string()),
            formula: Some(formula.to_string()),
            ..fixed_item(code, sort)
        }
    }

    // ==================== ① 新建 → 落库逐列 / 列表可见 ====================

    /// 新建公式项：逐列落库、`create_by_name` join 到位、列表能按编码搜到、`depends_on` 解析正确。
    #[tokio::test]
    async fn create_persists_columns_and_shows_in_page() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("create").await;
        let base = fx.code("base");
        let ot = fx.code("ot");
        let sort_base = fx.sort();
        let sort_ot = sort_base + 1;

        let base_res = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort_base))
            .await
            .expect("建基本工资项");
        assert_eq!(base_res.item_code, base);
        assert!(!base_res.is_system, "接口不许建系统项");
        assert!(base_res.is_active);

        let formula = format!("ROUND({{{base}}} / 21.75 / 8 * {{ATT_OT_WD}} * 1.5, 2)");
        let ot_res = salary_item_handler::create_item(fx.operator.clone(), &formula_item(&ot, sort_ot, &formula))
            .await
            .expect("建加班费项");
        assert_eq!(ot_res.item_type, "EARNING");
        assert_eq!(ot_res.item_type_label, "收入项");
        assert_eq!(ot_res.calc_type_label, "公式计算");
        assert_eq!(ot_res.formula.as_deref(), Some(formula.as_str()));
        assert_eq!(
            ot_res.depends_on,
            vec![base.clone()],
            "depends_on 只列薪资项引用，不含 ATT_*"
        );
        assert_eq!(ot_res.sort_order, sort_ot);
        assert_eq!(ot_res.debit_account_code.as_deref(), Some("5602.10"));
        let operator_id = fx.operator.id.expect("夹具操作人必有 id").to_string();
        assert_eq!(ot_res.create_by.as_deref(), Some(operator_id.as_str()));
        assert_eq!(
            ot_res.create_by_name.as_deref(),
            Some("HR31 夹具操作人"),
            "创建人姓名要 join 实时取（约定 13）"
        );

        // 库里逐列回读
        let row = salary_item_model::find(db(), Uuid::parse_str(&ot_res.id).expect("uuid"))
            .await
            .expect("查库")
            .expect("行存在");
        assert_eq!(row.item_code, ot);
        assert_eq!(row.formula.as_deref(), Some(formula.as_str()));
        assert_eq!(row.is_system, 0);
        assert_eq!(row.is_taxable, 1);
        assert_eq!(row.is_active, 1);
        assert_eq!(row.source, "ATTENDANCE");

        // 列表：关键字搜编码命中，条件里带上 is_active=1 / EARNING
        let page = salary_item_dto::SalaryItemPageQuery {
            page: Some(1),
            page_size: Some(200),
            keyword: Some(ot.clone()),
            item_type: Some("EARNING".to_string()),
            is_active: Some(1),
            calc_type: None,
        };
        let (list, total, _, _) = salary_item_handler::list_items(&page).await.expect("列表");
        assert!(total >= 1);
        assert_eq!(list.len(), 1, "关键字精确到编码应只命中一条");
        assert_eq!(list[0].item_code, ot);
        assert_eq!(list[0].depends_on, vec![base.clone()]);

        fx.clean().await;
    }

    // ==================== ② 验收项：公式长度 / 语义拦截且零落库 ====================

    /// 501 字符公式被拒（400 + 「最长 500」），库里没有该编码的行。
    #[tokio::test]
    async fn overlong_formula_is_rejected_without_row() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("long").await;
        let code = fx.code("long");
        let sort = fx.sort();
        let long = format!("{}+11", "1".repeat(salary_item_dto::MAX_FORMULA_LEN - 2));

        let error = salary_item_handler::create_item(fx.operator.clone(), &formula_item(&code, sort, &long))
            .await
            .expect_err("501 字符必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("最长 500"), "{}", error.msg);
        assert_eq!(fx.count(&code).await, 0, "被拒的请求不许留下半条数据");

        // 500 字符通过（边界等价类：刚好满列宽）
        let ok = format!("{}+1", "1".repeat(salary_item_dto::MAX_FORMULA_LEN - 2));
        let res = salary_item_handler::create_item(fx.operator.clone(), &formula_item(&code, sort, &ok))
            .await
            .expect("500 字符必须通过");
        assert_eq!(res.formula.as_deref(), Some(ok.as_str()));
        assert_eq!(fx.count(&code).await, 1);

        fx.clean().await;
    }

    /// 未定义变量 / 环依赖：保存路径 400 点名，检查接口 200 + `ok=false`。
    #[tokio::test]
    async fn semantic_errors_block_save_and_flag_validate() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("semantic").await;
        let base = fx.code("base");
        let typo = fx.code("typo");
        let a = fx.code("cyca");
        let b = fx.code("cycb");
        let sort1 = fx.sort();
        let sort2 = sort1 + 1;
        let sort3 = sort1 + 2;
        let sort4 = sort1 + 3;

        salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1))
            .await
            .expect("建基本工资项");

        // ① 拼错编码 → 保存被拒，报文点名 `{XXX}`
        let typo_formula = format!("ROUND({{{base}E}} / 21.75, 2)");
        let error = salary_item_handler::create_item(fx.operator.clone(), &formula_item(&typo, sort2, &typo_formula))
            .await
            .expect_err("未定义变量必须被拒");
        assert_eq!(error.status, 400);
        assert!(
            error.msg.contains("未定义的变量") && error.msg.contains(&base),
            "{}",
            error.msg
        );
        assert_eq!(fx.count(&typo).await, 0);

        // ② 检查接口同一份公式 → 200 + ok=false + undefined 点名（前端逐条显示）
        let validate = salary_item_handler::validate_formula(&SalaryItemValidateReq {
            formula: Some(typo_formula.clone()),
            item_code: Some(typo.clone()),
            id: None,
        })
        .await
        .expect("检查接口不报 400");
        assert!(!validate.ok);
        assert_eq!(validate.undefined.len(), 1);
        assert!(
            validate.errors.iter().any(|m| m.contains("未定义的变量")),
            "{:?}",
            validate.errors
        );
        assert!(
            validate.available.iter().any(|var| var.name == base && var.usable),
            "可用变量清单要含已建的 {base}"
        );

        // ③ 环：A 引用 B，B 再引用 A → 第二笔被拒
        let formula_a = format!("{{{b}}} + 1");
        let error = salary_item_handler::create_item(fx.operator.clone(), &formula_item(&a, sort3, &formula_a))
            .await
            .expect_err("B 还不存在，A 引用 B 应报未定义变量");
        assert!(error.msg.contains("未定义的变量"), "{}", error.msg);
        // 先建 B（引用已存在的基本工资项），成功
        let formula_b = format!("{{{base}}} * 2");
        salary_item_handler::create_item(fx.operator.clone(), &formula_item(&b, sort3, &formula_b))
            .await
            .expect("建 B");
        // 建 A 引用 B（成功）
        salary_item_handler::create_item(fx.operator.clone(), &formula_item(&a, sort4, &formula_a))
            .await
            .expect("建 A 引用 B");

        // 把 B 改成引用 A → 成环，必须被拒且 B 的公式原样
        let b_id = id_of(&b).await;
        let cycle = format!("{{{a}}} + 1");
        let error = salary_item_handler::update_item(fx.operator.clone(), b_id, &formula_item(&b, sort3, &cycle))
            .await
            .expect_err("成环必须被拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("循环依赖"), "{}", error.msg);
        let row = salary_item_model::find(db(), b_id)
            .await
            .expect("查库")
            .expect("行存在");
        assert_eq!(row.formula.as_deref(), Some(formula_b.as_str()), "拒绝后公式必须原样");

        fx.clean().await;
    }

    /// 按编码取 id（真库用例里反复要用）。
    async fn id_of(code: &str) -> Uuid {
        db().query_first_sql(
            "SELECT id FROM biz_salary_item WHERE item_code = ?".to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await
        .expect("查 id")
        .expect("行存在")
        .get_typed("id")
        .expect("id 列")
    }

    // ==================== ③ 唯一性：编码 / 计算顺序 ====================

    /// 重复编码 / 重复顺序都是 409 + 10003，且报文点出占用者。
    #[tokio::test]
    async fn code_and_sort_conflicts_are_reported() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("conflict").await;
        let base = fx.code("base");
        let other = fx.code("other");
        let sort1 = fx.sort();

        salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1))
            .await
            .expect("第一次建");

        // 同编码
        let same_code = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1 + 1))
            .await
            .expect_err("编码重复必须被拒");
        assert_eq!(same_code.status, 409);
        assert_eq!(same_code.code, error_code::RESOURCE_CONFLICT);
        assert!(same_code.msg.contains(&base), "报文要点出占用者：{}", same_code.msg);

        // 同顺序（编码不同）
        let same_sort = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&other, sort1))
            .await
            .expect_err("顺序重复必须被拒");
        assert_eq!(same_sort.status, 409);
        assert!(
            same_sort.msg.contains(&base) && same_sort.msg.contains(&sort1.to_string()),
            "{}",
            same_sort.msg
        );

        // 库层兜底：绕过应用层直接插同顺序 → UniqueViolation 被翻成 409
        let raw = db()
            .exec_sql(
                "INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, source, \
                      sort_order, debit_account_code, credit_account_code) \
                 VALUES (?::uuid, ?, 'HR31 直插', 'EARNING', 'FIXED', 'INPUT', ?::int, '5602.10', '2211.01')"
                    .to_string(),
                vec![
                    Value::from(&crate::business::hr::model::SENTINEL_ORG_ID),
                    Value::Text(format!("{other}X")),
                    Value::from(sort1),
                ],
            )
            .await
            .expect_err("库层唯一索引必须拦住同顺序");
        let mapped = salary_item_model::write_error(raw, "直插撞顺序");
        assert_eq!(mapped.status, 409);
        assert!(mapped.msg.contains("计算顺序"), "{}", mapped.msg);
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_item WHERE item_code = ?".to_string(),
                vec![Value::Text(format!("{other}X"))],
            )
            .await;

        fx.clean().await;
    }

    // ==================== ④ 修改：落库 / 编码不可改 / 停用守卫 ====================

    /// 改名 + 改公式成功并落库；改编码被 409 挡住（一行都没动）。
    #[tokio::test]
    async fn update_keeps_code_immutable_and_persists_changes() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("update").await;
        let base = fx.code("base");
        let sort1 = fx.sort();
        let created = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1))
            .await
            .expect("建项");
        let id = Uuid::parse_str(&created.id).expect("uuid");

        let mut rename = fixed_item(&base, sort1);
        rename.item_name = Some("HR31 改名后".to_string());
        let mut rename_formula = rename.clone();
        rename_formula.calc_type = Some("FORMULA".to_string());
        rename_formula.formula = Some(format!("{{{base}}} * 1"));
        let error = salary_item_handler::update_item(fx.operator.clone(), id, &rename_formula)
            .await
            .expect_err("自引用是环，必须被拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("循环依赖"), "{}", error.msg);

        let updated = salary_item_handler::update_item(fx.operator.clone(), id, &rename)
            .await
            .expect("改名");
        assert_eq!(updated.item_name, "HR31 改名后");
        assert_eq!(updated.item_code, base, "编码不变");
        let row = salary_item_model::find(db(), id).await.expect("查库").expect("行存在");
        assert_eq!(row.item_name, "HR31 改名后");
        assert!(
            row.update_by_name.as_deref() == Some("HR31 夹具操作人"),
            "改动人姓名 join 到位"
        );

        // 改编码 → 409 + 68008，库里编码不变
        let mut rekey = fixed_item(&base, sort1);
        rekey.item_code = Some(format!("{}Z", &base[..base.len().saturating_sub(1)]));
        let error = salary_item_handler::update_item(fx.operator.clone(), id, &rekey)
            .await
            .expect_err("改编码必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains("编码不可修改"), "{}", error.msg);
        assert_eq!(fx.count(&base).await, 1);

        fx.clean().await;
    }

    /// 停用守卫：被别的项公式引用的项不许停用（报文点名引用方）；没人用的项可以停用。
    #[tokio::test]
    async fn deactivation_is_blocked_while_referenced() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("deact").await;
        let base = fx.code("base");
        let user = fx.code("user");
        let sort1 = fx.sort();
        let sort2 = sort1 + 1;

        let base_res = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1))
            .await
            .expect("建基本工资项");
        salary_item_handler::create_item(
            fx.operator.clone(),
            &formula_item(&user, sort2, &format!("{{{base}}} + 10")),
        )
        .await
        .expect("建引用方");
        let base_id = Uuid::parse_str(&base_res.id).expect("uuid");

        let mut off = fixed_item(&base, sort1);
        off.is_active = Some(false);
        let error = salary_item_handler::update_item(fx.operator.clone(), base_id, &off)
            .await
            .expect_err("被引用不许停用");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_SALARY_RULE_BLOCKED);
        assert!(error.msg.contains(&user), "报文要点名引用方：{}", error.msg);
        let row = salary_item_model::find(db(), base_id)
            .await
            .expect("查库")
            .expect("行存在");
        assert_eq!(row.is_active, 1, "拒绝后 is_active 必须原样");

        // 把引用方停用（它自己没人引用），再停基本工资项
        let user_id = id_of(&user).await;
        let mut off_user = formula_item(&user, sort2, &format!("{{{base}}} + 10"));
        off_user.is_active = Some(false);
        salary_item_handler::update_item(fx.operator.clone(), user_id, &off_user)
            .await
            .expect("没人引用的项可以停用");
        salary_item_handler::update_item(fx.operator.clone(), base_id, &off)
            .await
            .expect("引用方停用后可以停用被引用项");

        fx.clean().await;
    }

    /// 系统项（模拟 `GROSS` / `NET`）：不许停用、不许改类型。
    #[tokio::test]
    async fn system_item_cannot_be_disabled_or_retyped() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("system").await;
        let code = fx.code("sys");
        let sort1 = fx.sort();
        let created = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&code, sort1))
            .await
            .expect("建项");
        let id = Uuid::parse_str(&created.id).expect("uuid");
        // 接口不许设置 is_system → 手改成系统项，模拟迁移种下的 GROSS / NET
        db().exec_sql(
            "UPDATE biz_salary_item SET is_system = 1 WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("标成系统项");

        let mut off = fixed_item(&code, sort1);
        off.is_active = Some(false);
        let error = salary_item_handler::update_item(fx.operator.clone(), id, &off)
            .await
            .expect_err("系统项不许停用");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("不许停用"), "{}", error.msg);

        let mut retype = fixed_item(&code, sort1);
        retype.item_type = Some("PRE_DEDUCT".to_string());
        let error = salary_item_handler::update_item(fx.operator.clone(), id, &retype)
            .await
            .expect_err("系统项不许改类型");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("类型 / 算法不可改"), "{}", error.msg);

        fx.clean().await;
    }

    // ==================== ⑤ 检查接口（#35）：依赖顺序 / id 解析 / 404 ====================

    /// `dependency_order` 依赖在前、末位本项；传 id 时后端自己查编码；id 不存在 → 404。
    #[tokio::test]
    async fn validate_returns_dependency_order_and_resolves_id() {
        if !ensure_db().await {
            return;
        }
        let mut fx = fixture("validate").await;
        let base = fx.code("base");
        let ot = fx.code("ot");
        let sort1 = fx.sort();
        let base_res = salary_item_handler::create_item(fx.operator.clone(), &fixed_item(&base, sort1))
            .await
            .expect("建基本工资项");
        let base_id = Uuid::parse_str(&base_res.id).expect("uuid");

        let check = salary_item_handler::validate_formula(&SalaryItemValidateReq {
            formula: Some(format!("ROUND({{{base}}} / 21.75 * {{ATT_OT_WD}}, 2)")),
            item_code: Some(ot.clone()),
            id: None,
        })
        .await
        .expect("检查");
        assert!(check.ok, "{:?}", check.errors);
        assert_eq!(
            check.dependency_order,
            vec![base.clone(), ot.clone()],
            "依赖在前、本项在末位（设计 §6 的 BASE → OT_WD）"
        );
        assert_eq!(check.item_refs, vec![base.clone()]);
        assert_eq!(check.att_refs, vec!["ATT_OT_WD".to_string()]);

        // 传 id → 后端按 id 查编码（自引用判成环，而不是「未定义变量」）
        let self_check = salary_item_handler::validate_formula(&SalaryItemValidateReq {
            formula: Some(format!("{{{base}}} * 2")),
            item_code: None,
            id: Some(base_id.to_string()),
        })
        .await
        .expect("检查自引用");
        assert!(!self_check.ok, "自引用必须报环");
        assert!(
            self_check.undefined.is_empty(),
            "自引用不该报未定义变量：{:?}",
            self_check.undefined
        );
        assert!(self_check.cycle.is_some());

        // 空公式 → 400（请求本身有问题，不是「公式错」）
        let empty = salary_item_handler::validate_formula(&SalaryItemValidateReq::default())
            .await
            .expect_err("空公式 → 400");
        assert_eq!(empty.status, 400);

        // id 不存在 → 404
        let missing = salary_item_handler::validate_formula(&SalaryItemValidateReq {
            formula: Some("{X} + 1".to_string()),
            item_code: None,
            id: Some(Uuid::now_v7().to_string()),
        })
        .await
        .expect_err("id 不存在 → 404");
        assert_eq!(missing.status, 404);

        fx.clean().await;
    }
}
