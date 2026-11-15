//! 员工档案 6 个接口的真库用例（HR-12）。
//!
//! 单元测试（`employee_handler.rs` 的 `mod tests`）能证明校验 / 合并 / 状态守卫这些**纯逻辑**
//! 是对的，但证明不了三件只有真库才知道的事：
//!
//! 1. **数据权限真的生效**：`DEPT` 范围只出本部门、`SELF` 范围只出本人 —— 这要两张部门、
//!    两个账号、两个员工的真实行才测得出来（片段拼错 / 别名写错在纯单测里看不出来）；
//! 2. **六个接口的 SQL 真能跑**：详情的三段 join（部门 / 账号 / 字典）、合同与薪资标准的
//!    子表投影、状态机写 `biz_status_log` + CAS 回写；
//! 3. **越权一律 404**：单条接口（详情 / 改 / 删 / 状态 / 绑账号）每个都要判可见性，
//!    漏一个就能按 id 拖走别人的档案。
//!
//! 不设环境变量时全部 SKIP（不连真库，也不假装绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr12_card \
//!   cargo test --lib business::hr::employee_handler_db -- --test-threads=1
//! ```
//!
//! # 夹具纪律（同 HR-10）
//!
//! 全部用 `HR12-<唯一后缀>` 造，清场**按 id / 唯一用户名**删（绝不按前缀扫全库）；
//! 工号 / 身份证 / 账号三者都带唯一后缀，重跑不会互相踩。

use nik::prelude::*;

mod db {
    use super::*;
    use crate::business::hr::employee_handler as handler;
    use crate::business::hr::employee_model as model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::NaiveDate;
    use nik::uuid::Uuid;
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

    /// 身份证号（按整段 tag 哈希：不同后缀绝不撞同一张证）。
    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        format!("1101011990{:06}", hash % 1_000_000)
    }

    /// 造一个本用例专属账号并返回 id。
    async fn fresh_user(suffix: &str, dept: Uuid) -> Uuid {
        test_helpers::ensure_user(&format!("hr12.{suffix}"), "HR12 夹具员工", Some(dept)).await
    }

    /// 造一个本用例专属部门（code 唯一，清场按 code 删）。
    async fn fresh_dept(suffix: &str, tag: &str) -> (Uuid, String) {
        let code = format!("HR12-{tag}-{suffix}");
        let id = test_helpers::ensure_dept(&code, &format!("HR12 部门 {tag}")).await;
        (id, code)
    }

    /// 一份最小可用的新建入参（真实过 `create_employee` 的校验与落库）。
    fn create_req(suffix: &str, dept: Uuid, user: Option<Uuid>) -> handler::EmployeeCreateReq {
        handler::EmployeeCreateReq {
            emp_no: Some(format!("HR12-{suffix}")),
            name: Some(format!("员工{suffix}")),
            gender: Some(1),
            birth_date: Some("1990-01-01".to_string()),
            id_card: Some(id_card_of(suffix)),
            mobile: Some("13800000000".to_string()),
            email: Some(format!("hr12-{suffix}@example.com")),
            dept_id: Some(dept.to_string()),
            position_code: Some("ENGINEER".to_string()),
            job_level: Some("P5".to_string()),
            education: Some("BACHELOR".to_string()),
            hire_date: Some("2024-03-01".to_string()),
            probation_end_date: Some("2024-06-01".to_string()),
            bank_name: Some("工商银行".to_string()),
            bank_account: Some("6222021234567890".to_string()),
            bank_account_name: Some(format!("员工{suffix}")),
            user_id: user.map(|id| id.to_string()),
            ..handler::EmployeeCreateReq::default()
        }
    }

    /// 超管 principal（`ALL` 范围，能看全部员工）。
    fn super_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), true, ScopeKind::All, vec![], false)
    }

    /// 部门范围 principal（只看 `dept_ids` 里的部门）。
    fn dept_principal(user: Uuid, dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, Some(dept), false, ScopeKind::Dept, vec![dept], false)
    }

    /// 「仅本人」principal（只看自己那份档案）。部门集合为空 + `self_only = true`。
    fn self_principal(user: Uuid, dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, Some(dept), false, ScopeKind::SelfOnly, vec![], true)
    }

    /// 落一份劳动合同（HR-15 的写接口未落地，夹具直接插；`status = 'ACTIVE'` 可转正）。
    async fn insert_contract(employee_id: Uuid, suffix: &str, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_employee_contract \
                (id, org_id, employee_id, contract_no, contract_type, sign_date, start_date, end_date, \
                 probation_months, status, remark) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?, 'FIXED_TERM', ?::text::date, ?::text::date, ?::text::date, \
                 3, ?, 'HR-12 真库用例夹具')"
                .to_string(),
            vec![
                Value::from(&id),
                Value::from(&crate::business::hr::model::SENTINEL_ORG_ID),
                Value::from(&employee_id),
                Value::Text(format!("HT-HR12-{suffix}")),
                Value::Text("2024-03-01".to_string()),
                Value::Text("2024-03-01".to_string()),
                Value::Text("2027-02-28".to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("插劳动合同夹具");
        id
    }

    /// 落一条薪资标准 + 一个薪资项（验证详情子表的 join 取名）。
    ///
    /// `biz_salary_item.sort_order` 是 `(org_id, sort_order)` 唯一：用 item_code 的哈希算一个
    /// 稳定值，并在插入前按 `item_code` 先清一次（上一轮中途 panic 留下的行会挡住插入）。
    async fn insert_salary(employee_id: Uuid, suffix: &str) -> String {
        let item_code = format!("HR12-ITEM-{suffix}");
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_salary_item WHERE item_code = ?".to_string(),
                vec![Value::Text(item_code.clone())],
            )
            .await;
        let hash = item_code
            .bytes()
            .fold(7_u32, |acc, byte| acc.wrapping_mul(31).wrapping_add(u32::from(byte)));
        let sort_order = i32::try_from(hash % 1_000_000).unwrap_or(0) + 3_000_000;
        db().exec_sql(
            "INSERT INTO biz_salary_item \
                (org_id, item_code, item_name, item_type, calc_type, source, sort_order, \
                 debit_account_code, credit_account_code, is_active, is_system) \
             VALUES (?::uuid, ?, '基本工资', 'EARNING', 'FIXED', 'EMPLOYEE_STANDARD', ?::int, '6602', '2211', 1, 0)"
                .to_string(),
            vec![
                Value::from(&crate::business::hr::model::SENTINEL_ORG_ID),
                Value::Text(item_code.clone()),
                Value::from(sort_order),
            ],
        )
        .await
        .expect("插薪资项夹具");
        db().exec_sql(
            "INSERT INTO biz_employee_salary \
                (org_id, employee_id, item_code, amount, effective_from, remark) \
             VALUES (?::uuid, ?::uuid, ?, 8000.0000, ?::text::date, 'HR-12 真库用例夹具')"
                .to_string(),
            vec![
                Value::from(&crate::business::hr::model::SENTINEL_ORG_ID),
                Value::from(&employee_id),
                Value::Text(item_code.clone()),
                Value::Text("2024-03-01".to_string()),
            ],
        )
        .await
        .expect("插薪资标准夹具");
        item_code
    }

    /// 清场：按 id 删主子表 + 状态日志，按用户名 / 部门码删夹具账号与部门。
    async fn cleanup(employee_ids: &[Uuid], usernames: &[String], dept_codes: &[String], item_codes: &[String]) {
        for id in employee_ids {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee_contract WHERE employee_id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee_salary WHERE employee_id = ?::uuid".to_string(),
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
        for code in item_codes {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_salary_item WHERE item_code = ?".to_string(),
                    vec![Value::Text(code.clone())],
                )
                .await;
        }
        for username in usernames {
            test_helpers::purge_user(username).await;
        }
        for code in dept_codes {
            test_helpers::purge_dept(code).await;
        }
    }

    /// 一个「随便什么都能看到」的脱敏集合（只用于读，不影响写路径）。
    fn mask() -> crate::business::masking::MaskSet {
        crate::business::masking::MaskSet::empty()
    }

    // ==================== ① 六接口全通 ====================

    /// 建 → 详情 → 列表 → 改 → 删 + 绑账号，六个接口在同一条路径上跑通。
    ///
    /// 断言的重点（都是只有真库才暴露的东西）：
    /// - 详情里 `dept_name` / `username` 是 **join 出来的实时值**，不是 uuid；
    /// - 详情里**只有** `*_masked`，明文字节不出现（`id_card` / `bank_account`）；
    /// - 合同 / 薪资标准子表能投影出来（`EmployeeContractRes` 的 12 列 + 薪资项名 join）；
    /// - 改成改名不改工号；软删后详情 / 改名都 404。
    #[tokio::test]
    async fn six_endpoints_roundtrip() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "A").await;
        let username = format!("hr12.{suffix}");
        let user = fresh_user(&suffix, dept).await;
        let operator = Some(user);
        let principal = super_principal(dept);

        // ③ 新建
        let created = handler::create_employee(operator, &create_req(&suffix, dept, Some(user)))
            .await
            .expect("新建员工");
        let id = Uuid::parse_str(&created.id).expect("回执 id");
        assert_eq!(created.emp_no, format!("HR12-{suffix}"));
        println!("[HR-12] ① 新建 id={id} emp_no={}", created.emp_no);

        // 合同 + 薪资夹具（HR-15/HR-16 的写接口未落地，这里直接插；详情只读投影它们）
        let contract_id = insert_contract(id, &suffix, "ACTIVE").await;
        let item_code = insert_salary(id, &suffix).await;

        // ② 详情
        let detail = handler::detail_of(&principal, id, &mask()).await.expect("详情");
        assert_eq!(detail.emp_no, created.emp_no);
        assert_eq!(detail.name, format!("员工{suffix}"));
        assert_eq!(detail.status, "PROBATION", "新档一律试用期");
        assert!(
            !detail.dept_name.is_empty() && detail.dept_name.contains("HR12"),
            "部门名 join 实时"
        );
        assert_eq!(detail.username.as_deref(), Some(username.as_str()), "账号名 join 实时");
        assert_eq!(detail.position_name.as_deref(), Some("工程师"), "岗位字典名");
        assert_eq!(detail.education_name.as_deref(), Some("本科"), "学历字典名");
        assert_eq!(detail.gender_label, "男");
        assert_eq!(detail.employment_type_label, "正式");
        // 敏感字段：只出打码值，明文一字节不出去
        let masked_id_card = detail.sensitive_masked.id_card_masked.clone().expect("身份证打码值");
        assert!(masked_id_card.contains('*'), "身份证必须打码：{masked_id_card}");
        assert!(
            !masked_id_card.contains(&id_card_of(&suffix)[6..14]),
            "打码值不得含明文中段：{masked_id_card}"
        );
        assert!(
            detail
                .sensitive_masked
                .bank_account_masked
                .as_deref()
                .is_some_and(|text| text.contains('*')),
            "工资卡必须打码"
        );
        let contract_json = serde_json::to_string(&detail).expect("详情可序列化");
        assert!(
            !contract_json.contains(&id_card_of(&suffix)),
            "详情 JSON 不得含明文身份证"
        );
        assert!(!contract_json.contains("6222021234567890"), "详情 JSON 不得含明文卡号");
        // 子表
        assert_eq!(detail.contracts.len(), 1, "合同子表要投影出来");
        assert_eq!(detail.contracts[0].id, contract_id.to_string());
        assert_eq!(detail.contracts[0].status_label, "生效中");
        assert!(detail.contracts[0].expire_in_days.is_some(), "生效合同要给到期天数");
        assert_eq!(
            detail.current_contract.as_ref().map(|item| item.id.clone()),
            Some(contract_id.to_string()),
            "现行合同 = ACTIVE 里最晚的一份"
        );
        assert_eq!(detail.salary_items.len(), 1, "薪资标准子表要投影出来");
        assert_eq!(detail.salary_items[0].item_name, "基本工资", "薪资项名 join");
        assert!(detail.salary_items[0].current, "2024-03-01 起的标准对今天生效");
        println!(
            "[HR-12] ② 详情 dept={} user={} 合同={} 薪资项={} masked_id_card={}",
            detail.dept_name,
            detail.username.clone().unwrap_or_default(),
            detail.contracts.len(),
            detail.salary_items.len(),
            detail.sensitive_masked.id_card_masked.clone().unwrap_or_default()
        );

        // ① 列表
        let filter = handler::filter_of(&handler::EmployeePageQuery {
            keyword: Some(format!("HR12-{suffix}")),
            ..handler::EmployeePageQuery::default()
        })
        .expect("筛选");
        let (list, total) = handler::page_employees(&principal, &mask(), &filter, 1, 20)
            .await
            .expect("列表");
        assert_eq!(total, 1, "关键字应只命中本用例这一行");
        assert_eq!(list[0].id, id.to_string());
        assert!(!list[0].dept_name.is_empty());
        assert_eq!(list[0].status_label, "试用期");
        assert!(
            list[0].contract_end_date.is_some(),
            "列表要带现行合同到期日（前端 30 天橙字靠它）"
        );
        println!(
            "[HR-12] ① 列表 total={total} contract_urgent={}",
            list[0].contract_urgent()
        );

        // ④ 修改：改名 + 清空邮箱；工号不许跟着变
        let updated = handler::update_employee(
            &principal,
            operator,
            id,
            &handler::EmployeeUpdateReq {
                name: Some(format!("改名{suffix}")),
                email: Some(String::new()),
                mobile: None,
                ..handler::EmployeeUpdateReq::default()
            },
        )
        .await
        .expect("修改员工");
        assert_eq!(updated.id, id.to_string());
        let row = model::find(id).await.expect("读回").expect("还在");
        assert_eq!(row.name, format!("改名{suffix}"));
        assert_eq!(row.email, None, "空串 = 清空");
        assert_eq!(row.mobile.as_deref(), Some("13800000000"), "不传 = 不改");
        assert_eq!(row.emp_no, created.emp_no, "工号不可改");

        // ⑧ 绑账号：撞键 → 409 + 68003；解绑 → 再绑回来
        let other_suffix = format!("{suffix}b");
        let other_user = fresh_user(&other_suffix, dept).await;
        let second = handler::create_employee(operator, &create_req(&other_suffix, dept, None))
            .await
            .expect("第二个员工");
        let second_id = Uuid::parse_str(&second.id).expect("回执 id");
        handler::bind_user(
            &principal,
            operator,
            second_id,
            &handler::EmployeeBindUserReq {
                user_id: Some(other_user.to_string()),
            },
        )
        .await
        .expect("绑账号");
        let conflict = handler::bind_user(
            &principal,
            operator,
            id,
            &handler::EmployeeBindUserReq {
                user_id: Some(other_user.to_string()),
            },
        )
        .await
        .expect_err("一个账号不能挂两份档案");
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, 68003, "账号冲突必须翻 68003");
        let unbound = handler::bind_user(
            &principal,
            operator,
            id,
            &handler::EmployeeBindUserReq { user_id: None },
        )
        .await
        .expect("解绑");
        assert_eq!(unbound.user_id, None);
        assert_eq!(unbound.username, None);
        println!("[HR-12] ⑧ 绑账号冲突 409+68003 ✓ 解绑 ✓");

        // ⑤ 删除（软删）：详情 404、再删 404、列表查不到
        handler::delete_employee(&principal, operator, id).await.expect("软删");
        let gone = handler::detail_of(&principal, id, &mask()).await.expect_err("删后详情");
        assert_eq!(gone.status, 404);
        let again = handler::delete_employee(&principal, operator, id)
            .await
            .expect_err("重复删");
        assert_eq!(again.status, 404);
        let (list_after, total_after) = handler::page_employees(&principal, &mask(), &filter, 1, 20)
            .await
            .expect("删后列表");
        assert!(
            list_after.iter().all(|item| item.id != id.to_string()),
            "软删的行不能出现在列表里"
        );
        // 关键字是 LIKE 前缀匹配，所以同前缀的第二个员工还在 —— 说明「只裁掉了被删的那一行」。
        assert_eq!(total_after, 1, "列表只剩另一个同前缀员工（软删的那行已消失）");
        assert_eq!(list_after[0].id, second_id.to_string());
        println!("[HR-12] ⑤ 软删 → 详情 404、列表 1（被删的行消失）✓");

        cleanup(
            &[id, second_id],
            &[username, format!("hr12.{other_suffix}")],
            &[dept_code],
            &[item_code],
        )
        .await;
    }

    // ==================== ② 状态流转 + 状态日志 ====================

    /// 转正 / 停薪 / 复职三条主链：守卫生效、`biz_status_log` 落库、CAS 防并发、
    /// 离职被挡在状态接口之外（唯一入口是离职单审批）。
    #[tokio::test]
    async fn status_lifecycle_and_guards() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "S").await;
        let username = format!("hr12.{suffix}");
        let user = fresh_user(&suffix, dept).await;
        let principal = super_principal(dept);
        let operator = Operator::named(Some(user), "HR12 夹具");

        let created = handler::create_employee(Some(user), &create_req(&suffix, dept, None))
            .await
            .expect("新建");
        let id = Uuid::parse_str(&created.id).expect("回执 id");

        // ① 没有生效合同 → 转正 400（设计 §4.2.1 的守卫）
        let no_contract = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("ACTIVE".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect_err("无合同不能转正");
        assert_eq!(no_contract.status, 400);
        assert!(no_contract.msg.contains("劳动合同"), "{}", no_contract.msg);

        // ② 补一份生效合同 → 转正成功，试用期截止日跟着生效日走
        insert_contract(id, &suffix, "ACTIVE").await;
        let promoted = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("ACTIVE".to_string()),
                effective_date: Some("2024-05-01".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect("转正");
        assert_eq!(promoted.status, "ACTIVE");
        assert_eq!(promoted.status_label, "在职");
        let row = model::find(id).await.expect("读回").expect("还在");
        assert_eq!(row.status, "ACTIVE");
        assert_eq!(
            row.probation_end_date,
            NaiveDate::from_ymd_opt(2024, 5, 1),
            "转正生效日回写试用期截止日"
        );

        // 状态日志：转正写了一条（from PROBATION → ACTIVE，带操作人姓名快照）
        let logs = db()
            .query_sql(
                "SELECT from_status, to_status, operator_name FROM biz_status_log \
                  WHERE entity_id = ?::uuid ORDER BY created_at"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志");
        assert_eq!(logs.len(), 1, "转正应当写一条状态日志");
        assert_eq!(
            logs[0].get_typed::<String>("from_status").expect("读 from_status"),
            "PROBATION"
        );
        assert_eq!(
            logs[0].get_typed::<String>("to_status").expect("读 to_status"),
            "ACTIVE"
        );
        assert_eq!(
            logs[0]
                .get_typed::<Option<String>>("operator_name")
                .expect("读 operator_name"),
            Some("HR12 夹具".to_string()),
            "操作人姓名是快照列（AGENTS.md 约定 13）"
        );

        // ③ 停薪：原因太短 → 400；够长 → 成功
        let short = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("SUSPENDED".to_string()),
                reason: Some("有事".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect_err("原因太短");
        assert_eq!(short.status, 400);
        let suspended = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("SUSPENDED".to_string()),
                reason: Some("出国进修一年".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect("停薪留职");
        assert_eq!(suspended.status, "SUSPENDED");
        let log_row = db()
            .query_sql(
                "SELECT reason FROM biz_status_log WHERE entity_id = ?::uuid \
                  ORDER BY created_at DESC LIMIT 1"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("查原因");
        assert_eq!(
            log_row[0]
                .get_typed::<Option<String>>("reason")
                .expect("读 reason")
                .as_deref(),
            Some("出国进修一年"),
            "停薪原因要进状态日志"
        );

        // ④ 复职
        let back = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("ACTIVE".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect("复职");
        assert_eq!(back.status, "ACTIVE");

        // ⑤ 离职：状态接口一律 409（唯一入口是离职单审批）
        let resign = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("RESIGNED".to_string()),
                reason: Some("个人原因辞职".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect_err("状态接口不能直接离职");
        assert_eq!(resign.status, 409);
        assert!(resign.msg.contains("离职单"), "{}", resign.msg);

        // ⑥ 非法边（ACTIVE → PROBATION）由状态机挡下（409，不是 handler 硬判）
        let illegal = handler::change_status(
            &principal,
            operator.clone(),
            id,
            &handler::EmployeeStatusReq {
                to_status: Some("PROBATION".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect_err("非法边");
        assert_eq!(illegal.status, 409, "非法边应当是 409：{}", illegal.msg);

        // ⑦ 已离职档案只读这条只对真正 RESIGNED 的行生效（夹具直接置状态验证口径）
        db().exec_sql(
            "UPDATE biz_employee SET status = 'RESIGNED', leave_date = ?::text::date, \
                leave_type = 'RESIGN' WHERE id = ?::uuid"
                .to_string(),
            vec![Value::Text("2025-01-31".to_string()), Value::from(&id)],
        )
        .await
        .expect("夹具置离职");
        let locked = handler::update_employee(
            &principal,
            Some(user),
            id,
            &handler::EmployeeUpdateReq {
                name: Some("想改离职档".to_string()),
                ..handler::EmployeeUpdateReq::default()
            },
        )
        .await
        .expect_err("离职档只读");
        assert_eq!(locked.status, 409);
        assert!(locked.msg.contains("只读"), "{}", locked.msg);

        let log_total = db()
            .query_sql(
                "SELECT count(*) AS n FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("数状态日志")[0]
            .get_typed::<i64>("n")
            .expect("读 count");
        assert_eq!(log_total, 3, "转正 / 停薪 / 复职各一条（离职与非法边都没写进去）");
        println!("[HR-12] ⑥ 状态链：转正/停薪/复职 ✓ 状态日志 {log_total} 条（含姓名快照） 离职拦截 ✓ 非法边 409 ✓");
        cleanup(&[id], &[username], &[dept_code], &[]).await;
    }

    // ==================== ③ 数据权限：只能看本部门 ====================

    /// 本卡验收项：部门范围 / 本人范围两种口径下，**列表与单条一起裁**，
    /// 且越权一律 404（不是 403，不泄露存在性）。
    #[tokio::test]
    async fn dept_and_self_scope_are_enforced() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept_a, code_a) = fresh_dept(&suffix, "DA").await;
        let (dept_b, code_b) = fresh_dept(&suffix, "DB").await;
        let user_a = fresh_user(&format!("{suffix}.a"), dept_a).await;
        let user_b = fresh_user(&format!("{suffix}.b"), dept_b).await;

        let a = handler::create_employee(Some(user_a), &create_req(&format!("{suffix}a"), dept_a, None))
            .await
            .expect("A 部门员工");
        let a_id = Uuid::parse_str(&a.id).expect("A id");
        let b = handler::create_employee(Some(user_b), &create_req(&format!("{suffix}b"), dept_b, None))
            .await
            .expect("B 部门员工");
        let b_id = Uuid::parse_str(&b.id).expect("B id");

        // ① A 部门的 HR（DEPT 范围 = dept_a）：列表只看得到 A
        let hr_a = dept_principal(user_a, dept_a);
        let filter = handler::filter_of(&handler::EmployeePageQuery {
            keyword: Some(format!("HR12-{suffix}")),
            ..handler::EmployeePageQuery::default()
        })
        .expect("筛选");
        let (list, total) = handler::page_employees(&hr_a, &mask(), &filter, 1, 50)
            .await
            .expect("A 的列表");
        assert_eq!(total, 1, "本部门范围只应看到 1 人，实际 {total}（看到别部门 = 越权）");
        assert_eq!(list[0].id, a_id.to_string());
        println!("[HR-12] ③ DEPT 范围：列表 total={total}（本部门 1 / 别部门 0）");

        // ② 单条也要裁：A 部门看得到 A，看不 B（详情 / 改 / 删 / 状态 / 绑账号 五个入口逐个验）
        handler::detail_of(&hr_a, a_id, &mask()).await.expect("本部门详情");
        let denied_detail = handler::detail_of(&hr_a, b_id, &mask())
            .await
            .expect_err("别部门详情必须拒绝");
        assert_eq!(denied_detail.status, 404, "越权应当是 404（不是 403，不泄露存在性）");

        let denied_update = handler::update_employee(
            &hr_a,
            Some(user_a),
            b_id,
            &handler::EmployeeUpdateReq {
                name: Some("越权改名".to_string()),
                ..handler::EmployeeUpdateReq::default()
            },
        )
        .await
        .expect_err("别部门不能改");
        assert_eq!(denied_update.status, 404);

        let denied_delete = handler::delete_employee(&hr_a, Some(user_a), b_id)
            .await
            .expect_err("别部门不能删");
        assert_eq!(denied_delete.status, 404);

        let denied_status = handler::change_status(
            &hr_a,
            Operator::of(Some(user_a)),
            b_id,
            &handler::EmployeeStatusReq {
                to_status: Some("SUSPENDED".to_string()),
                reason: Some("越权停薪试试".to_string()),
                ..handler::EmployeeStatusReq::default()
            },
        )
        .await
        .expect_err("别部门不能改状态");
        assert_eq!(denied_status.status, 404);

        let denied_bind = handler::bind_user(
            &hr_a,
            Some(user_a),
            b_id,
            &handler::EmployeeBindUserReq { user_id: None },
        )
        .await
        .expect_err("别部门不能绑账号");
        assert_eq!(denied_bind.status, 404);
        println!("[HR-12] ③ 单条 5 个入口对别部门全部 404 ✓");

        // ③ 别部门的行**没有被改动**（越权请求不能留下任何副作用）
        let b_row = model::find(b_id).await.expect("读 B").expect("B 还在（没被越权删除）");
        assert_eq!(b_row.name, format!("员工{suffix}b"), "越权改名不能生效");
        assert_eq!(b_row.status, "PROBATION", "越权状态变更不能生效");

        // ④ 「仅本人」范围：自己的档案看得到，同部门的同事看不到
        let a2 = handler::create_employee(Some(user_a), &create_req(&format!("{suffix}c"), dept_a, Some(user_a)))
            .await
            .expect("A 部门第二人（绑定 user_a）");
        let a2_id = Uuid::parse_str(&a2.id).expect("A2 id");
        let self_only = self_principal(user_a, dept_a);
        let (self_list, self_total) = handler::page_employees(&self_only, &mask(), &filter, 1, 50)
            .await
            .expect("本人列表");
        assert_eq!(self_total, 1, "仅本人范围只应看到 1 份档案，实际 {self_total}");
        assert_eq!(self_list[0].id, a2_id.to_string(), "本人 = user_id 等于自己的那一份");
        handler::detail_of(&self_only, a2_id, &mask()).await.expect("本人详情");
        let denied_self = handler::detail_of(&self_only, a_id, &mask())
            .await
            .expect_err("同部门同事的档案在「仅本人」范围不可见");
        assert_eq!(denied_self.status, 404);
        println!("[HR-12] ③ SELF 范围：列表 total={self_total}（本人 1 / 同事 0）✓");

        // ⑤ 超管 / ALL 范围看得到全部（对照组：上面的裁剪不是「谁都被裁」）
        let all_see = super_principal(dept_a);
        let (all_list, all_total) = handler::page_employees(&all_see, &mask(), &filter, 1, 50)
            .await
            .expect("ALL 列表");
        assert_eq!(all_total, 3, "ALL 范围应看到三个夹具员工，实际 {all_total}");
        assert_eq!(all_list.len(), 3);
        println!("[HR-12] ③ ALL 范围：列表 total={all_total} ✓");

        cleanup(
            &[a_id, b_id, a2_id],
            &[format!("hr12.{suffix}.a"), format!("hr12.{suffix}.b")],
            &[code_a, code_b],
            &[],
        )
        .await;
    }
}
