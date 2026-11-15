//! 员工敏感字段明文 + 审计留痕的真库用例（HR-13）。
//!
//! 本卡的三条验收（`明文接口要权限 + 写审计` / `审计含：谁、什么时候、看了谁的什么字段` /
//! `sys_field_mask 接线验证`）里，只有第一条的「权限」能靠纯单测证明（宏声明 + 契约对拍）。
//! 另外两条必须真跑库：
//!
//! 1. **审计真的落了、字段真的对**：`audit_logs` 的 `user_id / username / created_at /
//!    target_type / target_id / operation / detail` 七个列都要按口径填上 ——
//!    这条链路断了不会报错，只会「审计页上什么都查不到」；
//! 2. **`sys_field_mask` 真的接线了**（设计 §8.3 R1）：把库里 `biz_employee.id_card` 的
//!    `mask_param` 从 `6,4` 改成 `3,2`，**同一条 handler 路径**出的打码值必须立刻跟着变；
//!    改回来又变回去。写死在代码里的打码值过不了这条（它只会永远 `6,4`）。
//!
//! 不设环境变量时全部 SKIP（不连真库，也不假装绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr13_drill \
//!   cargo test --lib business::hr::employee_sensitive_db -- --test-threads=1
//! ```
//!
//! # 夹具纪律（同 HR-10 / HR-12）
//!
//! 全部用 `HR13-<唯一后缀>` 造，清场**按 id / 唯一用户名 / 部门码**删（绝不按前缀扫全库）。
//! 审计行是追加写、不属于业务夹具，清场时按 `id` 精确删掉本用例插的那几条
//! （否则每次跑测试都会在 audit_logs 里堆垃圾，且「审计条数」断言会因为历史数据变红）。

use nik::prelude::*;

mod db {
    use super::*;
    use crate::business::hr::employee_dto;
    use crate::business::hr::employee_handler as handler;
    use crate::business::hr::employee_sensitive as sensitive;
    use crate::business::masking::{self, MaskSet, masker::MaskType, registry};
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::shared::test_helpers;
    use nik::prelude::chrono::{DateTime, Utc};
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

    /// 身份证号（18 位；按整段 tag 哈希，不同后缀不撞同一张证）。
    fn id_card_of(tag: &str) -> String {
        let hash: u64 = tag.bytes().fold(146_959_810_273_937_871_u64, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
        format!("1101011990{:06}", hash % 1_000_000)
    }

    /// 工资卡号（16 位数字，便于断言「前 4 后 4」）。
    fn bank_account_of(tag: &str) -> String {
        let hash: u64 = tag
            .bytes()
            .fold(7_u64, |acc, byte| acc.wrapping_mul(31).wrapping_add(u64::from(byte)));
        format!("62220200{:08}", hash % 100_000_000)
    }

    /// 造一个本用例专属部门（code 唯一，清场按 code 删）。
    async fn fresh_dept(suffix: &str, tag: &str) -> (Uuid, String) {
        let code = format!("HR13-{tag}-{suffix}");
        let id = test_helpers::ensure_dept(&code, &format!("HR13 部门 {tag}")).await;
        (id, code)
    }

    /// 造一个本用例专属账号（明文留痕里的「谁」就是这个账号）。
    async fn fresh_user(suffix: &str, dept: Uuid) -> (Uuid, String) {
        let username = format!("hr13.{suffix}");
        let id = test_helpers::ensure_user(&username, "HR13 夹具员工", Some(dept)).await;
        (id, username)
    }

    /// 一份最小可用的新建入参（真实过 `create_employee` 的校验与落库）。
    fn create_req(suffix: &str, dept: Uuid, with_bank: bool) -> handler::EmployeeCreateReq {
        handler::EmployeeCreateReq {
            emp_no: Some(format!("HR13-{suffix}")),
            name: Some(format!("员工{suffix}")),
            gender: Some(1),
            birth_date: Some("1990-01-01".to_string()),
            id_card: Some(id_card_of(suffix)),
            mobile: Some("13800000009".to_string()),
            email: Some(format!("hr13-{suffix}@example.com")),
            dept_id: Some(dept.to_string()),
            position_code: Some("ENGINEER".to_string()),
            job_level: Some("P5".to_string()),
            education: Some("BACHELOR".to_string()),
            hire_date: Some("2024-03-01".to_string()),
            probation_end_date: Some("2024-06-01".to_string()),
            bank_name: with_bank.then(|| "工商银行".to_string()),
            bank_account: with_bank.then(|| bank_account_of(suffix)),
            bank_account_name: with_bank.then(|| format!("员工{suffix}")),
            ..handler::EmployeeCreateReq::default()
        }
    }

    /// 超管 principal（`ALL` 范围）。
    fn super_principal(user: Uuid, dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, Some(dept), true, ScopeKind::All, vec![], false)
    }

    /// 部门范围 principal（只看 `dept_ids` 里的部门）。
    fn dept_principal(user: Uuid, dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, Some(dept), false, ScopeKind::Dept, vec![dept], false)
    }

    /// 读者上下文：谁在查（`user_id` / `username` 与 audit_logs 断言逐字对齐）。
    fn reader(user: Uuid, username: &str) -> sensitive::ReaderContext {
        sensitive::ReaderContext {
            user_id: Some(user),
            username: Some(username.to_string()),
            request_id: Uuid::now_v7().to_string(),
            ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("hr13-real-db-test".to_string()),
        }
    }

    /// 清场：按 id 删员工与合同/状态日志，按用户名 / 部门码删夹具，按 id 删本用例写的审计行。
    async fn cleanup(employee_ids: &[Uuid], usernames: &[String], dept_codes: &[String], audit_ids: &[String]) {
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
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(id)],
                )
                .await;
        }
        for audit_id in audit_ids {
            let _ = db()
                .exec_sql(
                    "DELETE FROM audit_logs WHERE id = ?::text::uuid".to_string(),
                    vec![Value::Text(audit_id.clone())],
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

    /// 本用例插的审计条数（按 `target_id` + 操作码精确数：不受历史数据影响）。
    async fn audit_rows(
        employee_id: Uuid,
    ) -> Vec<(
        String,
        Option<Uuid>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    )> {
        let rows = db()
            .find_with_params(
                "SELECT id::text AS id, user_id, username, operation, target_type, target_id \
                   FROM audit_logs WHERE target_id = ? AND operation = ? \
                  ORDER BY created_at ASC",
                vec![
                    Value::Text(employee_id.to_string()),
                    Value::Text(sensitive::AUDIT_OPERATION.to_string()),
                ],
            )
            .await
            .expect("查 audit_logs");
        rows.iter()
            .map(|row| {
                Ok((
                    row.get_typed::<String>("id")?,
                    row.get_typed::<Option<Uuid>>("user_id")?,
                    row.get_typed::<Option<String>>("username")?,
                    row.get_typed::<String>("operation")?,
                    row.get_typed::<Option<String>>("target_type")?,
                    row.get_typed::<Option<String>>("target_id")?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()
            .expect("解析审计行")
    }

    // ==================== ① 明文 + 审计七列 ====================

    /// 建员工 → 查明文 → `audit_logs` 的**七个列**全部按口径落库 → 清理。
    ///
    /// 这条用例同时钉住三件事：
    ///
    /// - 明文接口真的返回明文（`id_card` / `bank_account` 两个字段各一次）；
    /// - **谁**（`user_id` + `username`）/ **什么时候**（`created_at` 落库且与回执一致）/
    ///   **看了谁的什么字段**（`target_type` + `target_id` + `operation` + `detail.field`）；
    /// - 审计 `detail` **不含明文**（审计表不是第二个明文出口）。
    #[tokio::test]
    async fn plaintext_read_writes_full_audit_trail() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "audit").await;
        let (user, username) = fresh_user(&suffix, dept).await;
        let principal = super_principal(user, dept);

        let created = handler::create_employee(Some(user), &create_req(&suffix, dept, true))
            .await
            .expect("建员工夹具");
        let employee_id = Uuid::parse_str(&created.id).expect("id 是 UUID");
        println!("[HR-13] 夹具 emp_no={} id={employee_id}", created.emp_no);

        let reader_ctx = reader(user, &username);
        // 明文走的是「规则集合」——用一个真实的 `MaskSet`（超管 = 豁免，打码值即明文；
        // 这里关心的是**明文与留痕**，打码值的规则接线由 ③ 的另一条用例钉住）。
        let plain_card = sensitive::read_sensitive(
            &principal,
            &reader_ctx,
            employee_id,
            sensitive::SensitiveField::IdCard,
            &masking::MaskSet::empty(),
        )
        .await
        .expect("查明身份证明文");
        assert_eq!(plain_card.field, "id_card");
        assert_eq!(
            plain_card.value.as_deref(),
            Some(id_card_of(&suffix).as_str()),
            "明文接口返回的必须是库里的原文"
        );
        assert_eq!(plain_card.emp_no, created.emp_no);
        assert_eq!(plain_card.name, format!("员工{suffix}"));
        assert!(
            plain_card.audited_at.ends_with("+08:00"),
            "审计时间必须是 CST：{}",
            plain_card.audited_at
        );

        let plain_bank = sensitive::read_sensitive(
            &principal,
            &reader_ctx,
            employee_id,
            sensitive::SensitiveField::BankAccount,
            &masking::MaskSet::empty(),
        )
        .await
        .expect("查工资卡明文");
        assert_eq!(plain_bank.field, "bank_account");
        assert_eq!(plain_bank.value.as_deref(), Some(bank_account_of(&suffix).as_str()));

        // ---- 审计七列 ----
        let rows = audit_rows(employee_id).await;
        let audit_ids: Vec<String> = rows.iter().map(|row| row.0.clone()).collect();
        println!("[HR-13] 本用例落了 {} 条审计：{audit_ids:?}", rows.len());

        assert_eq!(rows.len(), 2, "两个字段各留一条痕（多看一个字段就多一条）");
        assert!(
            audit_ids.contains(&plain_card.audit_id),
            "回执 audit_id 必须是库里的那一行"
        );
        assert!(audit_ids.contains(&plain_bank.audit_id));
        for (id, user_id, username_of_row, operation, target_type, target_id) in &rows {
            assert_eq!(*user_id, Some(user), "谁：user_id 必须是查看人（审计行 {id}）");
            assert_eq!(
                username_of_row.as_deref(),
                Some(username.as_str()),
                "谁：username 快照（审计行 {id}）"
            );
            assert_eq!(operation, sensitive::AUDIT_OPERATION);
            assert_eq!(target_type.as_deref(), Some(sensitive::AUDIT_TARGET_TYPE));
            assert_eq!(target_id.as_deref(), Some(employee_id.to_string().as_str()), "看了谁");
        }

        // ---- detail：什么字段 + 工号 / 姓名 / 请求号，且不含明文 ----
        let detail_rows = db()
            .find_with_params(
                "SELECT detail::text AS detail, created_at, request_id FROM audit_logs \
                  WHERE target_id = ? AND operation = ? ORDER BY created_at ASC",
                vec![
                    Value::Text(employee_id.to_string()),
                    Value::Text(sensitive::AUDIT_OPERATION.to_string()),
                ],
            )
            .await
            .expect("查审计 detail");
        let fields: Vec<String> = detail_rows
            .iter()
            .map(|row| {
                let text = row.get_typed::<String>("detail").expect("detail 文本");
                let value: serde_json::Value = serde_json::from_str(&text).expect("detail 是 JSON");
                assert_eq!(value["employee_id"], employee_id.to_string());
                assert_eq!(value["emp_no"], created.emp_no, "审计 detail 要带工号（人对得上）");
                assert_eq!(value["employee_name"], format!("员工{suffix}"));
                assert_eq!(value["dept_id"], dept.to_string());
                assert_eq!(value["found"], true);
                let request_id = row.get_typed::<String>("request_id").expect("request_id");
                assert_eq!(value["request_id"], request_id, "detail 与列必须同一个请求号");
                assert!(
                    request_id.chars().count() <= sensitive::MAX_REQUEST_ID,
                    "request_id 超列宽：{request_id}"
                );
                let _created_at: DateTime<Utc> = row.get_typed("created_at").expect("created_at");
                // 审计表不是第二个明文出口
                assert!(!text.contains(&id_card_of(&suffix)), "detail 泄漏身份证明文：{text}");
                assert!(
                    !text.contains(&bank_account_of(&suffix)),
                    "detail 泄漏工资卡明文：{text}"
                );
                value["field"].as_str().expect("field").to_string()
            })
            .collect();
        assert_eq!(fields, vec!["id_card".to_string(), "bank_account".to_string()]);

        // ---- 详情 / 列表继续只出打码值（本卡不放宽任何既有出参）----
        let detail = handler::detail_of(&principal, employee_id, &masking::MaskSet::empty())
            .await
            .expect("详情");
        let text = serde_json::to_string(&detail).expect("序列化详情");
        assert!(!text.contains(&id_card_of(&suffix)), "详情里出现了明文身份证：{text}");
        assert!(
            !text.contains(&bank_account_of(&suffix)),
            "详情里出现了明文工资卡：{text}"
        );
        assert!(
            detail.sensitive_masked.id_card_masked.is_some(),
            "详情必须有打码值（否则前端「查看明文」按钮都不显示）"
        );

        cleanup(&[employee_id], &[username], &[dept_code], &audit_ids).await;
    }

    // ==================== ② 越权不返回、也不留痕 ====================

    /// 别部门的人查明文 → **404 且一条审计都不写**。
    ///
    /// 「查不到也留痕」是错的口径：那会让审计页被暴力试探的 404 淹掉，
    /// 真实查看反而被埋。越权尝试由 `operation_logs`（请求流水）负责。
    #[tokio::test]
    async fn out_of_scope_read_is_404_and_writes_no_audit() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept_a, code_a) = fresh_dept(&suffix, "a").await;
        let (dept_b, code_b) = fresh_dept(&suffix, "b").await;
        let (owner, owner_name) = fresh_user(&suffix, dept_a).await;
        let (outsider, outsider_name) = fresh_user(&format!("{suffix}b"), dept_b).await;

        let created = handler::create_employee(Some(owner), &create_req(&suffix, dept_a, true))
            .await
            .expect("建员工夹具");
        let employee_id = Uuid::parse_str(&created.id).expect("id 是 UUID");

        // 只可见 B 部门的人去查 A 部门的档案
        let principal = dept_principal(outsider, dept_b);
        let error = sensitive::read_sensitive(
            &principal,
            &reader(outsider, &outsider_name),
            employee_id,
            sensitive::SensitiveField::IdCard,
            &masking::MaskSet::empty(),
        )
        .await
        .expect_err("范围外必须报错");
        assert_eq!(error.status, 404, "范围外一律 404（403 会泄露 id 存在性）");
        assert_eq!(error.code, crate::shared::error_code::RESOURCE_NOT_FOUND);

        let rows = audit_rows(employee_id).await;
        assert!(rows.is_empty(), "越权尝试不该写明文审计：{rows:?}");

        // 超管对照：同一档案能查到（证明 404 来自范围而不是档案不存在）
        let super_p = super_principal(owner, dept_a);
        let ok = sensitive::read_sensitive(
            &super_p,
            &reader(owner, &owner_name),
            employee_id,
            sensitive::SensitiveField::IdCard,
            &masking::MaskSet::empty(),
        )
        .await
        .expect("超管可见");
        cleanup(
            &[employee_id],
            &[owner_name, outsider_name],
            &[code_a, code_b],
            &[ok.audit_id],
        )
        .await;
    }

    // ==================== ③ 未登记字段：返回 null，但仍然留痕 ====================

    /// 没登记工资卡 → `value = null`（不是空串、不是报错），且审计仍写一条 `found=false`。
    ///
    /// 「查了但没查到」也是审计事实：有人反复查一个空字段，本身是要被看见的行为。
    #[tokio::test]
    async fn unregistered_field_returns_null_and_still_audits() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "empty").await;
        let (user, username) = fresh_user(&suffix, dept).await;
        let principal = super_principal(user, dept);

        let created = handler::create_employee(Some(user), &create_req(&suffix, dept, false))
            .await
            .expect("建员工夹具（无工资卡）");
        let employee_id = Uuid::parse_str(&created.id).expect("id 是 UUID");

        let res = sensitive::read_sensitive(
            &principal,
            &reader(user, &username),
            employee_id,
            sensitive::SensitiveField::BankAccount,
            &masking::MaskSet::empty(),
        )
        .await
        .expect("未登记不是错误");
        assert!(res.value.is_none(), "未登记 → null：{:?}", res.value);
        assert!(res.masked_value.is_none());

        let rows = audit_rows(employee_id).await;
        assert_eq!(rows.len(), 1, "查空字段也要留痕");
        let detail_text = db()
            .query_first_sql(
                "SELECT detail::text AS detail FROM audit_logs WHERE id = ?::text::uuid".to_string(),
                vec![Value::Text(res.audit_id.clone())],
            )
            .await
            .expect("查审计行")
            .expect("审计行存在")
            .get_typed::<String>("detail")
            .expect("detail 文本");
        let value: serde_json::Value = serde_json::from_str(&detail_text).expect("合法 JSON");
        assert_eq!(value["found"], false, "审计要如实记「没查到」");

        cleanup(
            &[employee_id],
            &[username],
            &[dept_code],
            &rows.iter().map(|row| row.0.clone()).collect::<Vec<_>>(),
        )
        .await;
    }

    // ==================== ④ `sys_field_mask` 接线验证（设计 §8.3 R1）====================

    /// 库里规则改参数 → handler 出的打码值立刻跟着变；改回来又变回去。
    ///
    /// # 为什么这条是「接线」的证据而不是「算法」的证据
    ///
    /// 算法由 HR-11 的纯单测钉住（`6,4` → `110101****1234`）。本条要证的是
    /// **规则来自 `sys_field_mask` 表**：把 `mask_param` 从 `6,4` 改成 `3,2` 后，
    /// 同一条 `detail_of` 出参必须变成「前 3 后 2」——写死常量的实现无论如何都过不了。
    ///
    /// # 为什么不用 `UPDATE` 后立刻断言
    ///
    /// 先把「改后的值」和「改回后的值」都算出来，**恢复原值**再断言：
    /// 断言 panic 也不会把规则留在改坏的状态（夹具库里躺着一条 `3,2` 会污染后续所有卡）。
    #[tokio::test]
    async fn mask_rule_change_drives_masked_value() {
        if !ensure_db().await {
            return;
        }
        let suffix = uniq();
        let (dept, dept_code) = fresh_dept(&suffix, "mask").await;
        let (user, username) = fresh_user(&suffix, dept).await;
        let principal = super_principal(user, dept);
        let created = handler::create_employee(Some(user), &create_req(&suffix, dept, true))
            .await
            .expect("建员工夹具");
        let employee_id = Uuid::parse_str(&created.id).expect("id 是 UUID");
        let id_card = id_card_of(&suffix);

        // 原始参数（`V0388` 种的是 6,4）
        let original: Option<String> = db()
            .query_first_sql(
                "SELECT mask_param FROM sys_field_mask \
                  WHERE table_name = 'biz_employee' AND column_name = 'id_card' \
                    AND role_id IS NULL AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查 sys_field_mask")
            .expect("V0388 应种 biz_employee.id_card 的全局规则")
            .get_typed("mask_param")
            .expect("mask_param");
        let original = original.unwrap_or_else(|| employee_dto::DEFAULT_ID_CARD_PARAM.to_string());
        println!("[HR-13] 库里 biz_employee.id_card 的 mask_param = {original:?}");

        // ---- 基线：按库里原始规则出的打码值 ----
        let mask_before = load_mask(user).await;
        let before = handler::detail_of(&principal, employee_id, &mask_before)
            .await
            .expect("详情（原规则）")
            .sensitive_masked
            .id_card_masked
            .expect("有打码值");

        // ---- 改参数为 3,2，清缓存后重新取规则 ----
        set_rule_param("3,2").await;
        masking::model::invalidate_all().await;
        let mask_after = load_mask(user).await;
        let changed = {
            let detail = handler::detail_of(&principal, employee_id, &mask_after)
                .await
                .expect("详情（改后规则）");
            // 明文一秒都不出（打码列只可能是打码形态）
            let text = serde_json::to_string(&detail).expect("序列化");
            assert!(!text.contains(&id_card), "改规则后详情泄漏明文：{text}");
            detail.sensitive_masked.id_card_masked.expect("有打码值")
        };

        // ---- 恢复原值（断言之前先恢复）----
        set_rule_param(&original).await;
        masking::model::invalidate_all().await;
        let mask_restored = load_mask(user).await;
        let restored = handler::detail_of(&principal, employee_id, &mask_restored)
            .await
            .expect("详情（恢复后规则）")
            .sensitive_masked
            .id_card_masked
            .expect("有打码值");

        println!("[HR-13] 打码值 before={before} / 改 3,2 后={changed} / 恢复后={restored}");

        // ---- 断言：规则驱动 ----
        assert_ne!(
            before, changed,
            "改了 sys_field_mask.mask_param 但打码值没变 ⇒ 脱敏没接线（设计 §8.3 R1 的怀疑成真）"
        );
        let prefix: String = id_card.chars().take(3).collect();
        let tail: String = id_card.chars().skip(id_card.chars().count() - 2).collect();
        assert!(
            changed.starts_with(&prefix) && changed.ends_with(&tail),
            "3,2 应当保留前 3 后 2：原文 {id_card} / 打码 {changed}"
        );
        assert_eq!(restored, before, "恢复 mask_param 后打码值必须回到基线");
        assert!(
            before.starts_with(&id_card.chars().take(6).collect::<String>()),
            "原始规则 6,4 应当保留前 6：{before}"
        );

        cleanup(&[employee_id], &[username], &[dept_code], &[]).await;
    }

    /// 按用户加载脱敏规则集合（与 handler 内 `masking::resolve` 同一条取数路径，
    /// 只是不做「超管 / 明版权限」豁免判定 —— 本用例要看到**规则本身**的效果）。
    async fn load_mask(user: Uuid) -> MaskSet {
        registry::load_user_set(&user).await.expect("加载脱敏规则集合")
    }

    /// 把 `biz_employee.id_card` 的全局规则参数改成给定值（用例内互斥使用）。
    async fn set_rule_param(param: &str) {
        let affected = db()
            .exec_sql(
                "UPDATE sys_field_mask SET mask_param = ? \
                  WHERE table_name = 'biz_employee' AND column_name = 'id_card' \
                    AND role_id IS NULL AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::Text(param.to_string())],
            )
            .await
            .expect("改 mask_param");
        assert_eq!(affected, 1, "应当恰好改到一条规则（V0388 的 biz_employee.id_card）");
    }

    /// 打码类型必须是「身份证」——规则被人改成别的类型时，本条用例先红，
    /// 免得把「类型换了」误判成「接线断了」。
    #[tokio::test]
    async fn id_card_rule_stays_the_id_card_type() {
        if !ensure_db().await {
            return;
        }
        let mask_type: i16 = db()
            .query_first_sql(
                "SELECT mask_type FROM sys_field_mask \
                  WHERE table_name = 'biz_employee' AND column_name = 'id_card' \
                    AND role_id IS NULL AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查 sys_field_mask")
            .expect("V0388 应种 biz_employee.id_card 的全局规则")
            .get_typed("mask_type")
            .expect("mask_type");
        assert_eq!(
            mask_type,
            MaskType::IdCard.as_i16(),
            "biz_employee.id_card 的 mask_type 必须是 2（身份证）"
        );
    }

    /// 未使用的导入守卫：`employee_dto` / `MaskType` 在上面的用例里都用到了，
    /// 这里只是让「文件被裁剪后编译仍能定位缺哪一处」时有个明确落点。
    #[allow(dead_code)]
    fn _type_anchor(_: MaskType) {}
}
