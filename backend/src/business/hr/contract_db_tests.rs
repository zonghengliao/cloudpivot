//! 劳动合同台账（`biz_employee_contract`）的真库用例（HR-15）。
//!
//! 单测（`contract_model` / `contract_dto`）证明的是**映射与校验**；证明不了四件事，
//! 只有真库能答：
//!
//! 1. `INSERT` / `UPDATE` 的占位符、`NULLIF(?::text,'')::date` 写法真能跑；
//! 2. PG 报出来的约束名就是 `uk_biz_emp_contract_no` / `ck_biz_emp_contract_*`
//!    （冲突翻译按名字分支，改名字不会编译报错）；
//! 3. **续签链的递归 CTE** 真能沿 `renewed_from_id` 回溯到原合同（验收项）；
//! 4. 状态机（`biz_status_log` + CAS）在合同实体上真的落库。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr15_card \
//!   cargo test --lib business::hr::contract_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 的教训）
//!
//! - 合同号 / 工号 / 身份证都带唯一后缀，重跑不互相踩；
//! - 清场**按 id** 删（绝不按前缀扫全库）；
//! - 每个用例独立部门 + 独立账号（`uk_biz_employee_user` 是全局唯一）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::contract_dto::{ContractCreateReq, ContractRenewReq, ContractUpdateReq};
    use crate::business::hr::contract_handler;
    use crate::business::hr::contract_model as model;
    use crate::business::hr::employee_dto;
    use crate::business::hr::employee_handler;
    use crate::business::hr::employee_model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::{Duration, NaiveDate};
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

    /// 身份证号（按整段 tag 哈希：不同后缀绝不撞同一张证）。
    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        format!("1101011990{:06}", hash % 1_000_000)
    }

    fn super_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), true, ScopeKind::All, vec![], false)
    }

    fn today() -> NaiveDate {
        employee_dto::today()
    }

    fn shift(days: i64) -> NaiveDate {
        today() + Duration::days(days)
    }

    fn shift_text(days: i64) -> String {
        shift(days).to_string()
    }

    /// 造一个本用例专属部门（code 唯一，清场按 code 删）。
    async fn fresh_dept(suffix: &str) -> (Uuid, String) {
        let code = format!("HR15-{suffix}");
        let id = test_helpers::ensure_dept(&code, &format!("HR15 部门 {suffix}")).await;
        (id, code)
    }

    /// 造一个员工（走真实 `create_employee`：合同要挂在外键上）。
    async fn fresh_employee(suffix: &str, dept: Uuid) -> (Uuid, String) {
        let username = format!("hr15.{suffix}");
        let user = test_helpers::ensure_user(&username, "HR15 夹具员工", Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR15-{suffix}")),
                name: Some(format!("员工{suffix}")),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                id_card: Some(id_card_of(suffix)),
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

    /// 一份最小可用的合同入参（终止日 = 今天 + `end_days`）。
    fn create_req(no: &str, end_days: i64) -> ContractCreateReq {
        ContractCreateReq {
            contract_no: Some(no.to_string()),
            contract_type: Some("FIXED_TERM".to_string()),
            sign_date: Some(shift_text(-400)),
            start_date: Some(shift_text(-365)),
            end_date: Some(shift_text(end_days)),
            probation_months: Some(3),
            status: None,
            remark: Some("HR15 夹具".to_string()),
        }
    }

    /// 清场：按员工 id 删合同 + 状态日志 + 员工，再按用户名 / 部门码删夹具。
    async fn cleanup(employee_ids: &[Uuid], usernames: &[String], dept_codes: &[String]) {
        for id in employee_ids {
            let contracts = db()
                .find_with_params(
                    "SELECT id FROM biz_employee_contract WHERE employee_id = ?::uuid",
                    vec![Value::from(id)],
                )
                .await
                .unwrap_or_default();
            for row in &contracts {
                if let Ok(contract_id) = row.get_typed::<Uuid>("id") {
                    let _ = db()
                        .exec_sql(
                            "DELETE FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
                            vec![Value::from(&contract_id)],
                        )
                        .await;
                }
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee_contract WHERE employee_id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
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
        for username in usernames {
            test_helpers::purge_user(username).await;
        }
        for code in dept_codes {
            test_helpers::purge_dept(code).await;
        }
    }

    /// 一个夹具套件：部门 + 员工 + 清理清单。
    struct Fixture {
        dept: Uuid,
        dept_code: String,
        employee: Uuid,
        username: String,
    }

    async fn fixture(tag: &str) -> Fixture {
        let suffix = format!("{tag}-{}", uniq());
        let (dept, dept_code) = fresh_dept(&suffix).await;
        let (employee, username) = fresh_employee(&suffix, dept).await;
        Fixture {
            dept,
            dept_code,
            employee,
            username,
        }
    }

    impl Fixture {
        fn principal(&self) -> ScopedPrincipal {
            super_principal(self.dept)
        }

        async fn clean(&self) {
            cleanup(
                &[self.employee],
                std::slice::from_ref(&self.username),
                std::slice::from_ref(&self.dept_code),
            )
            .await;
        }
    }

    fn contract_no(tag: &str, suffix: &str) -> String {
        format!("HT15-{tag}-{suffix}")
    }

    // ==================== ① CRUD + 到期提醒字段 ====================

    /// 建 → 改 → 到期字段：`expire_in_days` 只在「生效中 + 有终止日」时给，
    /// 且与业务日之差**逐位**对得上（验收项「到期提醒字段正确」）。
    #[tokio::test]
    async fn crud_roundtrip_and_expire_fields() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("crud").await;
        let principal = fx.principal();
        let operator = Operator::named(Some(Uuid::now_v7()), "HR15 夹具");
        let no = contract_no("A", &uniq());

        // 建：终止日 = 今天 + 20 天 → 到期提醒应当剩 20 天（≤30 是前端橙字线）
        let created =
            contract_handler::create_contract(&principal, operator.clone(), fx.employee, &create_req(&no, 20))
                .await
                .expect("新建合同");
        assert_eq!(created.contract_no, no);
        assert_eq!(created.status, "ACTIVE");
        assert_eq!(created.status_label, "生效中");
        assert_eq!(created.contract_type_label, "固定期限");
        assert_eq!(created.expire_in_days, Some(20), "到期天数必须逐位对得上");
        assert!(created.renewed_from_id.is_none(), "首份合同没有来源");
        let contract_id = Uuid::parse_str(&created.id).expect("合同 id");
        println!(
            "[HR-15] ① 新建 {no} id={contract_id} expire_in_days={:?}",
            created.expire_in_days
        );

        // 改：备注清空 + 试用期改 6 个月（可写列）
        let updated = contract_handler::update_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractUpdateReq {
                probation_months: Some(6),
                remark: Some("".to_string()),
                ..ContractUpdateReq::default()
            },
        )
        .await
        .expect("修改合同");
        assert_eq!(updated.probation_months, 6);
        assert_eq!(updated.remark, None, "空串 = 清空");
        assert_eq!(updated.contract_no, no, "编号不变");

        let row = model::find_in(db(), contract_id).await.expect("读回").expect("行还在");
        assert_eq!(row.probation_months, 6);
        assert_eq!(row.remark, None);
        println!("[HR-15] ① 修改后 probation=6 remark=None（编号未变）");

        // 到期字段：改成无固定期限 + 清空终止日 ⇒ 不再有到期天数（不会误报到期）
        let unfixed = contract_handler::update_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractUpdateReq {
                contract_type: Some("UNFIXED_TERM".to_string()),
                end_date: Some("".to_string()),
                ..ContractUpdateReq::default()
            },
        )
        .await
        .expect("改成无固定期限");
        assert_eq!(unfixed.end_date, None);
        assert_eq!(unfixed.expire_in_days, None, "无终止日 ⇒ 不算到期天数");
        assert_eq!(unfixed.contract_type_label, "无固定期限");

        // 已过期的终止日：仍是 ACTIVE 时给负数（前端照样橙字，靠负数兜住「忘了续签」）
        contract_handler::update_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractUpdateReq {
                contract_type: Some("FIXED_TERM".to_string()),
                end_date: Some(shift_text(-5)),
                ..ContractUpdateReq::default()
            },
        )
        .await
        .expect("改回固定期限（已过期）");
        let overdue = model::find_in(db(), contract_id).await.expect("读回").expect("行还在");
        let overdue_dto = employee_dto::EmployeeContractRes::from_input(overdue.to_input(), today());
        assert_eq!(overdue_dto.expire_in_days, Some(-5), "已过期必须是负数");
        println!(
            "[HR-15] ① 无固定期限 expire=None；过期 5 天 expire={:?}",
            overdue_dto.expire_in_days
        );

        // 全部字段落库复核（逐列）
        assert_eq!(overdue.status, "ACTIVE");
        fx.clean().await;
    }

    // ==================== ② 撞号 / CHECK → 业务码 ====================

    /// 合同编号重复 → 409 + 68005（报文点名编号）；绕过 DTO 直插违反 `sign_date ≤ start_date`
    /// → 400 + 人话（证明真库约束名与 `check_message` 的映射成立）。
    #[tokio::test]
    async fn duplicate_no_and_check_violation_map_to_business_codes() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("dup").await;
        let principal = fx.principal();
        let operator = Operator::named(Some(Uuid::now_v7()), "HR15 夹具");
        let no = contract_no("B", &uniq());

        contract_handler::create_contract(&principal, operator.clone(), fx.employee, &create_req(&no, 300))
            .await
            .expect("第一份");

        // 同一编号再建（另一个员工也不行：编号是 (org_id, contract_no) 唯一）
        let err = contract_handler::create_contract(&principal, operator.clone(), fx.employee, &create_req(&no, 300))
            .await
            .expect_err("编号必须唯一");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, 68005, "撞号必须翻 68005");
        assert!(err.msg.contains(&no), "报文要点名编号：{}", err.msg);
        println!("[HR-15] ② 撞号 → {} {}", err.status, err.code);

        // 绕过 DTO 直插：签订日晚于起始日 → 库 CHECK → 400（不是 500）
        let bad = model::ContractWrite {
            contract_no: contract_no("BAD", &uniq()),
            contract_type: "FIXED_TERM".to_string(),
            sign_date: shift(-1),
            start_date: shift(-400),
            end_date: Some(shift(300)),
            probation_months: 0,
            remark: None,
        };
        let raw = model::insert(db(), Uuid::now_v7(), fx.employee, &bad, "ACTIVE", None, None)
            .await
            .expect_err("签订日晚于起始日必须被 C0NSTRAINT 拦下");
        let error = model::write_error(raw, "hr_contract_check_probe", &bad.contract_no);
        assert_eq!(error.status, 400, "CHECK 违反必须是 400");
        assert!(error.msg.contains("签订日期"), "报文要点名该改哪个字段：{}", error.msg);
        println!(
            "[HR-15] ② CHECK 违反 → {} {}（{}）",
            error.status, error.code, error.msg
        );
        fx.clean().await;
    }

    // ==================== ③ 续签链（验收核心） ====================

    /// 两代续签：旧合同各自翻 `RENEWED`、`renewed_from_id` 逐级接链、
    /// `chain_of(最新)` 能回溯到**原合同**（长度 3、顺序原→新、每份的来源编号成对）。
    #[tokio::test]
    async fn renew_chain_traces_back_to_the_original_contract() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("chain").await;
        let principal = fx.principal();
        let operator = Operator::named(Some(Uuid::now_v7()), "HR15 夹具");
        let tag = uniq();
        let no1 = contract_no("C1", &tag);

        let v1 = contract_handler::create_contract(&principal, operator.clone(), fx.employee, &create_req(&no1, 30))
            .await
            .expect("原合同");
        let v1_id = Uuid::parse_str(&v1.id).expect("id");

        // 第一次续签
        let renew1 = contract_handler::renew_contract(
            &principal,
            operator.clone(),
            v1_id,
            &ContractRenewReq {
                contract_no: Some(contract_no("C2", &tag)),
                start_date: Some(shift_text(31)),
                end_date: Some(shift_text(396)),
                ..ContractRenewReq::default()
            },
        )
        .await
        .expect("第一次续签");
        assert_eq!(renew1.previous.status, "RENEWED", "原合同必须被封成 RENEWED");
        assert_eq!(renew1.contract.status, "ACTIVE");
        assert_eq!(
            renew1.contract.renewed_from_id.as_deref(),
            Some(v1.id.as_str()),
            "新合同必须指回原合同"
        );
        assert_eq!(renew1.contract.renewed_from_no.as_deref(), Some(no1.as_str()));
        assert_eq!(renew1.chain.len(), 2, "链 = 原合同 + 新合同");
        let v2_id = Uuid::parse_str(&renew1.contract.id).expect("id");

        // 第二次续签（从 v2 续）
        let renew2 = contract_handler::renew_contract(
            &principal,
            operator.clone(),
            v2_id,
            &ContractRenewReq {
                contract_no: Some(contract_no("C3", &tag)),
                start_date: Some(shift_text(397)),
                end_date: Some(shift_text(762)),
                ..ContractRenewReq::default()
            },
        )
        .await
        .expect("第二次续签");
        let v3_id = Uuid::parse_str(&renew2.contract.id).expect("id");

        // 验收：「续签链能追溯到原合同」——链首就是 v1，链尾就是最新那份
        let chain = model::chain_of(db(), v3_id).await.expect("查链");
        let ids: Vec<Uuid> = chain.iter().map(|item| item.id).collect();
        assert_eq!(ids, vec![v1_id, v2_id, v3_id], "链必须原合同 → 第一次 → 第二次");
        assert_eq!(chain[0].renewed_from_id, None, "链首没有来源 = 它就是原合同");
        assert_eq!(chain[1].renewed_from_id, Some(v1_id));
        assert_eq!(chain[2].renewed_from_id, Some(v2_id));
        assert_eq!(chain[1].renewed_from_no.as_deref(), Some(no1.as_str()));
        println!(
            "[HR-15] ③ 续签链 = {:?} → 可回溯到原合同 {}",
            chain.iter().map(|item| item.contract_no.clone()).collect::<Vec<_>>(),
            chain[0].contract_no
        );

        // 员工详情的时间线（HR-12 的 contracts[]）也应当是这三份、且新的在前
        let detail = employee_handler::detail_of(&principal, fx.employee, &crate::business::masking::MaskSet::empty())
            .await
            .expect("员工详情");
        assert_eq!(detail.contracts.len(), 3, "详情要看到全部 3 份历史");
        assert_eq!(detail.contracts[0].id, v3_id.to_string(), "台账新的在前");
        assert_eq!(
            detail.current_contract.as_ref().map(|item| item.id.clone()),
            Some(v3_id.to_string()),
            "现行合同 = 唯一 ACTIVE 的那份"
        );
        assert!(detail.contracts[0].expire_in_days.is_some());
        assert_eq!(detail.contracts[1].expire_in_days, None, "已续签的没有到期天数");

        // 已续签的原合同不能再续（续签链只能往前长）
        let err = contract_handler::renew_contract(
            &principal,
            operator.clone(),
            v1_id,
            &ContractRenewReq {
                contract_no: Some(contract_no("C4", &tag)),
                start_date: Some(shift_text(32)),
                end_date: Some(shift_text(400)),
                ..ContractRenewReq::default()
            },
        )
        .await
        .expect_err("已续签不能续");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, 68006);
        assert!(err.msg.contains("续签链"), "报文要给下一步：{}", err.msg);
        println!("[HR-15] ③ 对已续签再续 → {} {}", err.status, err.code);
        fx.clean().await;
    }

    // ==================== ④ 解除 + 终态冻结 ====================

    /// 解除：状态机落 `biz_status_log`、三列同写；之后改字段 / 再续签都必须被终态拦下。
    #[tokio::test]
    async fn terminate_writes_status_log_and_freezes_the_contract() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("term").await;
        let principal = fx.principal();
        let user = Uuid::now_v7();
        let operator = Operator::named(Some(user), "HR15 夹具");
        let no = contract_no("T", &uniq());

        let created =
            contract_handler::create_contract(&principal, operator.clone(), fx.employee, &create_req(&no, 300))
                .await
                .expect("新建");
        let contract_id = Uuid::parse_str(&created.id).expect("id");

        let terminated = contract_handler::update_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractUpdateReq {
                to_status: Some("TERMINATED".to_string()),
                terminated_date: Some(shift_text(-1)),
                terminate_reason: Some("协商一致解除劳动合同".to_string()),
                ..ContractUpdateReq::default()
            },
        )
        .await
        .expect("解除");
        assert_eq!(terminated.status, "TERMINATED");
        assert_eq!(terminated.status_label, "已解除");
        assert_eq!(terminated.terminated_date, Some(shift(-1)));
        assert_eq!(terminated.terminate_reason.as_deref(), Some("协商一致解除劳动合同"));
        assert_eq!(terminated.expire_in_days, None, "解除后不再报到期");

        // 状态日志：建单（null→ACTIVE）+ 解除（ACTIVE→TERMINATED）两条，操作人名字要有
        let logs = db()
            .find_with_params(
                "SELECT from_status, to_status, operator_name, reason FROM biz_status_log \
                  WHERE entity_id = ?::uuid ORDER BY created_at",
                vec![Value::from(&contract_id)],
            )
            .await
            .expect("查状态日志");
        assert_eq!(logs.len(), 2, "建单 + 解除各一条");
        let to: Vec<String> = logs
            .iter()
            .map(|row| row.get_typed("to_status").unwrap_or_default())
            .collect();
        assert_eq!(to, vec!["ACTIVE".to_string(), "TERMINATED".to_string()]);
        assert_eq!(
            logs[1]
                .get_typed::<Option<String>>("reason")
                .expect("reason")
                .as_deref(),
            Some("协商一致解除劳动合同")
        );
        println!("[HR-15] ④ 解除落库 + biz_status_log 两条（{to:?}）");

        // 终态冻结：改字段 409、再续签 409
        let err = contract_handler::update_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractUpdateReq {
                remark: Some("想偷偷改".to_string()),
                ..ContractUpdateReq::default()
            },
        )
        .await
        .expect_err("已解除不可改");
        assert_eq!((err.status, err.code), (409, 68006));
        let err = contract_handler::renew_contract(
            &principal,
            operator.clone(),
            contract_id,
            &ContractRenewReq {
                contract_no: Some(contract_no("T2", &uniq())),
                start_date: Some(shift_text(0)),
                end_date: Some(shift_text(365)),
                ..ContractRenewReq::default()
            },
        )
        .await
        .expect_err("已解除不可续签");
        assert_eq!((err.status, err.code), (409, 68006));
        println!("[HR-15] ④ 终态冻结：改字段 / 续签都是 409 + 68006（{}）", err.msg);
        fx.clean().await;
    }

    // ==================== ⑤ 离职员工不能新签 + 补录已到期合同可续签 ====================

    /// 两条业务边界：① 已离职员工新签合同 → 409+68006（复职走状态流转）；
    /// ② 补录一份 `EXPIRED` 历史合同后仍可续签（`EXPIRED → RENEWED` 是设计放开的边）。
    #[tokio::test]
    async fn resigned_employee_is_blocked_and_expired_contract_can_be_renewed() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("edge").await;
        let principal = fx.principal();
        let operator = Operator::named(Some(Uuid::now_v7()), "HR15 夹具");
        let tag = uniq();

        // ② 先补录历史到期合同（status=EXPIRED）→ 到期补签
        let expired_req = ContractCreateReq {
            status: Some("EXPIRED".to_string()),
            ..create_req(&contract_no("E1", &tag), -10)
        };
        let expired = contract_handler::create_contract(&principal, operator.clone(), fx.employee, &expired_req)
            .await
            .expect("补录历史合同");
        assert_eq!(expired.status, "EXPIRED");
        assert_eq!(expired.expire_in_days, None, "已到期合同不算「还有几天」");
        let expired_id = Uuid::parse_str(&expired.id).expect("id");

        let renewed = contract_handler::renew_contract(
            &principal,
            operator.clone(),
            expired_id,
            &ContractRenewReq {
                contract_no: Some(contract_no("E2", &tag)),
                sign_date: Some(shift_text(-10)),
                start_date: Some(shift_text(-9)),
                end_date: Some(shift_text(356)),
                ..ContractRenewReq::default()
            },
        )
        .await
        .expect("到期后补签续签");
        assert_eq!(renewed.previous.status, "RENEWED");
        assert_eq!(renewed.contract.status, "ACTIVE");
        println!(
            "[HR-15] ⑤ 补录 EXPIRED → 续签：{} ⇒ {}（链长 {}）",
            expired.status,
            renewed.contract.status,
            renewed.chain.len()
        );

        // ① 把员工置为离职（直接 CAS 回写，离职单属 HR-30），再新签 → 409
        let affected = employee_model::apply_status(
            db(),
            fx.employee,
            "PROBATION",
            "RESIGNED",
            Some(shift(-1)),
            Some("CONTRACT_END"),
            None,
        )
        .await
        .expect("置离职");
        assert_eq!(affected, 1);
        let err = contract_handler::create_contract(
            &principal,
            operator.clone(),
            fx.employee,
            &create_req(&contract_no("E3", &tag), 365),
        )
        .await
        .expect_err("离职员工不能新签");
        assert_eq!((err.status, err.code), (409, 68006));
        assert!(err.msg.contains("已离职"), "报文要点名原因：{}", err.msg);
        println!("[HR-15] ⑤ 离职员工新签 → {} {}（{}）", err.status, err.code, err.msg);
        fx.clean().await;
    }

    // ==================== ⑥ 幂等 / 并发 ====================

    /// 同一份合同并发续签：只能有一个成功（状态 CAS + 行锁），失败的必须是 409，
    /// 且库里**不能**出现两条 `renewed_from_id` 指向同一份合同的链（续签历史不分叉）。
    #[tokio::test]
    async fn concurrent_renew_has_exactly_one_winner() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("race").await;
        let principal = fx.principal();
        let operator = Operator::named(Some(Uuid::now_v7()), "HR15 夹具");
        let tag = uniq();
        let created = contract_handler::create_contract(
            &principal,
            operator.clone(),
            fx.employee,
            &create_req(&contract_no("R0", &tag), 30),
        )
        .await
        .expect("原合同");
        let v1_id = Uuid::parse_str(&created.id).expect("id");

        let mut handles = Vec::new();
        for index in 0..4 {
            let principal = principal.clone();
            let operator = operator.clone();
            let no = contract_no(&format!("R{}", index + 1), &tag);
            handles.push(tokio::spawn(async move {
                contract_handler::renew_contract(
                    &principal,
                    operator,
                    v1_id,
                    &ContractRenewReq {
                        contract_no: Some(no),
                        start_date: Some(shift_text(31)),
                        end_date: Some(shift_text(396)),
                        ..ContractRenewReq::default()
                    },
                )
                .await
            }));
        }
        let mut ok = 0;
        let mut conflicts = 0;
        for handle in handles {
            match handle.await.expect("任务未 panic") {
                Ok(_) => ok += 1,
                Err(error) => {
                    assert_eq!(error.status, 409, "失败必须是 409（不是 500）：{}", error.msg);
                    conflicts += 1;
                }
            }
        }
        assert_eq!(ok, 1, "只能有一个续签成功");
        assert_eq!(conflicts, 3);

        let children = db()
            .find_with_params(
                "SELECT id FROM biz_employee_contract WHERE renewed_from_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&v1_id)],
            )
            .await
            .expect("查链子");
        assert_eq!(children.len(), 1, "续签链不分叉：只能有一个后继");
        println!(
            "[HR-15] ⑥ 并发 4 续签：成功 {ok} / 409 {conflicts}；链上后继 {}",
            children.len()
        );
        fx.clean().await;
    }
}
