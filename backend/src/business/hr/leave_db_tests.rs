//! 请假单（`biz_leave_request`）的真库用例（HR-24）。
//!
//! 单测（`leave_dto` / `leave_model` 的 `#[cfg(test)]`）证明的是**校验与拼装**；
//! 证明不了五件事，只有真库能答：
//!
//! 1. `INSERT` / `UPDATE` / 状态 CAS 的占位符与 `?::text::timestamptz`、`?::text::numeric`、
//!    `?::int` 写法真能跑（写错只会在运行时炸，且只在某条分支上）；
//! 2. **验收项「字段与设计 §3.1 ⑥ 一致」**：17 个落库列逐个回读，值与快照都对得上
//!    （`employee_no` / `employee_name` 抄的是员工档当时的快照，`dept_name` 走实时 join）；
//! 3. 状态机四条边真能落库：`DRAFT→SUBMITTED→APPROVED→CANCELLED`、`SUBMITTED→DRAFT`（撤回），
//!    且 `submitted_at` / `cancelled_at` / `cancel_reason` 按边写、跨边保留；
//! 4. `ck_biz_leave_cancel` 真拦「作废不给原因」；`uk_biz_leave_no` 真翻 `409 + 68011`；
//! 5. 分页真按 `dept_id` 数据权限裁（`SCOPED_TABLES` 登记生效）；
//!    已批无薪假天数真的只数 `APPROVED AND paid = 0`。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr24_card \
//!   cargo test --lib business::hr::leave_db -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-14 / HR-15 / HR-16 / HR-19 的教训）
//!
//! - 部门码 / 用户名带唯一后缀，重跑不互相踩；
//! - 清场**按 id 删**（绝不按前缀扫全库），顺序：请假单 → 状态日志 → 员工 → 用户 → 部门
//!   （`biz_leave_request.employee_id` 是 `ON DELETE RESTRICT`，先删单再删人）；
//! - 员工走真实 `employee_handler::create_employee`（请假单挂在员工外键上，
//!   自己拼 INSERT 会漏掉 `uk_biz_employee_*` 的口径）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::employee_dto;
    use crate::business::hr::employee_handler;
    use crate::business::hr::leave_dto::{
        self, LeaveCancelReq, LeaveCreateReq, LeavePageQuery, LeaveUpdateReq, STATUS_APPROVED, STATUS_CANCELLED,
        STATUS_DRAFT, STATUS_SUBMITTED,
    };
    use crate::business::hr::leave_model::{self as model, LeaveInsert};
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::{SCOPED_TABLES, ScopeKind};
    use crate::shared::test_helpers;
    use nik::prelude::chrono::{DateTime, Duration, FixedOffset, Utc};
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

    /// 全局唯一后缀（同一进程内多次调用不会重）。
    fn uniq() -> u64 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        now.wrapping_add(seq.wrapping_mul(7_919))
    }

    /// 北京时间的一个「不与真实数据搅在一起」的请假窗口：固定 2026-11 的某两天。
    ///
    /// 用固定窗口而不是「今天」：请假天数与区间校验都按自然日算，
    /// 用今天会让「跨月/跨年」的用例在 12-31 那天变红。
    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        let start = leave_dto::parse_datetime("2026-11-10 09:00", "start").expect("合法时间");
        let end = leave_dto::parse_datetime("2026-11-11 18:00", "end").expect("合法时间");
        (start, end)
    }

    fn money(text: &str) -> Decimal {
        use nik::prelude::rust_decimal::prelude::FromStr;
        Decimal::from_str(text).expect("合法小数")
    }

    fn super_principal(dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(Uuid::now_v7(), Some(dept), true, ScopeKind::All, vec![], false)
    }

    /// 只看得见本部门（`Dept` 范围）的普通账号。
    fn dept_principal(user: Uuid, dept: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, Some(dept), false, ScopeKind::Dept, vec![dept], false)
    }

    struct Fixture {
        dept: Uuid,
        dept_code: String,
        dept_name: String,
        employee: Uuid,
        emp_no: String,
        emp_name: String,
        username: String,
        user: Uuid,
    }

    async fn fresh_dept(tag: &str) -> (Uuid, String, String) {
        let code = format!("HR24-{tag}-{}", uniq());
        let name = format!("HR24 部门 {tag}");
        let id = test_helpers::ensure_dept(&code, &name).await;
        (id, code, name)
    }

    async fn fresh_employee(tag: &str, dept: Uuid) -> (Uuid, String, String, String, Uuid) {
        let suffix = uniq();
        let username = format!("hr24.{tag}.{suffix}");
        let name = format!("HR24 员工 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, Some(dept)).await;
        let created = employee_handler::create_employee(
            Some(user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(format!("HR24-{tag}-{suffix}")),
                name: Some(name.clone()),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                user_id: Some(user.to_string()),
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建员工");
        let employee = Uuid::parse_str(&created.id).expect("员工 id");
        let emp_no = created.emp_no.clone();
        (employee, emp_no, name, username, user)
    }

    async fn fixture(tag: &str) -> Fixture {
        let (dept, dept_code, dept_name) = fresh_dept(tag).await;
        let (employee, emp_no, emp_name, username, user) = fresh_employee(tag, dept).await;
        Fixture {
            dept,
            dept_code,
            dept_name,
            employee,
            emp_no,
            emp_name,
            username,
            user,
        }
    }

    impl Fixture {
        fn create_req(&self) -> LeaveCreateReq {
            let (start, end) = window();
            LeaveCreateReq {
                employee_id: Some(self.employee.to_string()),
                leave_type: Some("ANNUAL".to_string()),
                start_time: Some(employee_dto_time(start)),
                end_time: Some(employee_dto_time(end)),
                duration_days: Some(money("2")),
                paid: Some(leave_dto::PAID_YES),
                reason: Some("HR24 夹具：回老家".to_string()),
                remark: Some("HR24 夹具".to_string()),
            }
        }

        /// 建一张单（取号 + 落库，与 HR-25 的事务体同一套调用）。
        async fn create(&self, req: &LeaveCreateReq) -> Uuid {
            let draft = leave_dto::create_of(req).expect("入参合法");
            let id = Uuid::now_v7();
            let leave_no = model::next_leave_no(db(), Utc::now()).await.expect("取号");
            model::insert(
                db(),
                LeaveInsert {
                    id,
                    leave_no: &leave_no,
                    employee_no: &self.emp_no,
                    employee_name: &self.emp_name,
                    dept_id: Some(self.dept),
                    draft: &draft,
                },
                Some(self.user),
            )
            .await
            .expect("建请假单");
            id
        }
    }

    async fn cleanup(fixtures: &[&Fixture]) {
        for fx in fixtures {
            // 状态日志先删（它按 entity_id 指向请假单；单删了就查不到了）
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_status_log WHERE entity_type = 'LEAVE_REQUEST' AND entity_id IN \
                     (SELECT id FROM biz_leave_request WHERE employee_id = ?::uuid)"
                        .to_string(),
                    vec![Value::from(&fx.employee)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_leave_request WHERE employee_id = ?::uuid".to_string(),
                    vec![Value::from(&fx.employee)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&fx.employee)],
                )
                .await;
            test_helpers::purge_user(&fx.username).await;
            test_helpers::purge_dept(&fx.dept_code).await;
        }
    }

    /// 时间 → 入参字符串（北京时间，与前端 TDesign 的格式一致）。
    fn employee_dto_time(value: DateTime<Utc>) -> String {
        let offset = FixedOffset::east_opt(employee_dto::TZ_OFFSET_SECS).expect("+08:00");
        value.with_timezone(&offset).format("%Y-%m-%d %H:%M").to_string()
    }

    /// 1. 落库字段与设计 §3.1 ⑥ 逐列对得上 + 快照 / join 口径正确。
    #[tokio::test]
    async fn insert_and_read_back_every_column() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("rt").await;
        let req = fx.create_req();
        let id = fx.create(&req).await;

        let row = model::find_in(db(), id).await.expect("读单").expect("单必在");
        // 业务列
        assert!(
            row.leave_no.starts_with("LV-") && row.leave_no.contains('-'),
            "单号形态：{}",
            row.leave_no
        );
        assert_eq!(row.leave_type, leave_dto::LEAVE_ANNUAL);
        assert_eq!(row.duration_days, money("2"));
        assert_eq!(row.paid, leave_dto::PAID_YES);
        assert_eq!(row.status, STATUS_DRAFT, "新建恒为草稿");
        assert_eq!(row.reason.as_deref(), Some("HR24 夹具：回老家"));
        assert_eq!(row.remark.as_deref(), Some("HR24 夹具"));
        assert!(row.approval_instance_id.is_none(), "草稿没有审批实例");
        assert!(row.submitted_at.is_none() && row.cancelled_at.is_none());
        // 员工快照（抄的是员工档，不是前端传的）
        assert_eq!(row.employee_id, fx.employee);
        assert_eq!(row.employee_no, fx.emp_no);
        assert_eq!(row.employee_name, fx.emp_name);
        // 部门：id 落库 + 名字实时 join
        assert_eq!(row.dept_id, Some(fx.dept));
        assert_eq!(row.dept_name, fx.dept_name, "dept_name 必须是 join 出来的真名");
        // 时间：往返不丢精度（入参精确到分，秒为 0）
        let (start, end) = window();
        assert_eq!(row.start_time.timestamp(), start.timestamp());
        assert_eq!(row.end_time.timestamp(), end.timestamp());
        // 出参标签
        let res = row.to_res();
        assert_eq!(res.status_label, "草稿");
        assert_eq!(res.leave_type_label, "年假");
        assert_eq!(res.paid_label, "带薪");
        assert!(!res.locked);
        assert_eq!(res.dept_name, fx.dept_name);

        cleanup(&[&fx]).await;
    }

    /// 2. 改草稿只动可改列；状态 CAS + 四条边真能走，审计列按边写、跨边保留。
    #[tokio::test]
    async fn draft_edit_then_state_machine_round_trip() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("sm").await;
        let id = fx.create(&fx.create_req()).await;

        // ---- 改草稿：类型 / 天数 / 无薪 ----
        let current = model::find_in(db(), id).await.expect("读").expect("在");
        let patch = leave_dto::patch_of(
            &LeaveUpdateReq {
                leave_type: Some("PERSONAL".to_string()),
                duration_days: Some(money("1.5")),
                paid: Some(leave_dto::PAID_NO),
                ..LeaveUpdateReq::default()
            },
            &current.draft_state(),
        )
        .expect("改草稿合法");
        assert!(patch.changed);
        let affected = model::update(db(), id, &patch, Some(fx.user)).await.expect("改");
        assert_eq!(affected, 1, "草稿可改");
        let after = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(after.leave_type, "PERSONAL");
        assert_eq!(after.duration_days, money("1.5"));
        assert_eq!(after.paid, leave_dto::PAID_NO);
        assert_eq!(after.start_time, current.start_time, "没传的列必须保持原值");

        // ---- DRAFT → SUBMITTED ----
        let now = Utc::now();
        let flipped = model::set_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
            .await
            .expect("提交");
        assert_eq!(flipped, 1);
        let submitted = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(submitted.status, STATUS_SUBMITTED);
        assert!(submitted.submitted_at.is_some(), "提交要写 submitted_at");
        assert_eq!(submitted.submitted_by, Some(fx.user), "提交人要落库");

        // 已提交状态下改草稿必须 0 行（库层守卫，不只靠 handler）
        let stale_patch = leave_dto::patch_of(&LeaveUpdateReq::default(), &submitted.draft_state()).expect("patch");
        assert_eq!(
            model::update(db(), id, &stale_patch, Some(fx.user)).await.expect("改"),
            0,
            "非草稿不许改"
        );

        // ---- SUBMITTED → DRAFT（撤回）：submitted_at 保留（COALESCE） ----
        assert_eq!(
            model::set_status(db(), id, STATUS_SUBMITTED, STATUS_DRAFT, None, Some(fx.user))
                .await
                .expect("撤回"),
            1
        );
        let withdrawn = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(withdrawn.status, STATUS_DRAFT);
        assert_eq!(withdrawn.submitted_at, submitted.submitted_at, "撤回不该抹掉提交时间");

        // ---- 再提交 → 批准 ----
        model::set_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
            .await
            .expect("再提交");
        let resubmitted = model::find_in(db(), id).await.expect("读").expect("在");
        assert!(
            resubmitted.submitted_at > submitted.submitted_at,
            "重新提交要覆盖成新的提交时间"
        );
        let instance = Uuid::now_v7();
        assert_eq!(
            model::set_approval_instance(db(), id, Some(instance), Some(fx.user))
                .await
                .expect("回写实例"),
            1
        );
        let with_instance = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(with_instance.approval_instance_id, Some(instance));
        assert_eq!(
            model::set_status(db(), id, STATUS_SUBMITTED, STATUS_APPROVED, None, Some(fx.user))
                .await
                .expect("批准"),
            1
        );
        let approved = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(approved.status, STATUS_APPROVED);
        assert!(approved.is_locked() && !approved.is_editable(), "已批准是锁死态");

        // 已批准不许再改（库层 status='DRAFT' 守卫）
        let locked_patch = leave_dto::patch_of(&LeaveUpdateReq::default(), &approved.draft_state()).expect("patch");
        assert_eq!(
            model::update(db(), id, &locked_patch, Some(fx.user)).await.expect("改"),
            0
        );
        // 已批准不许软删
        assert_eq!(model::soft_delete(db(), id, Some(fx.user)).await.expect("删"), 0);

        // ---- APPROVED → CANCELLED（销假，必须带原因） ----
        let cancel = leave_dto::cancel_of(&LeaveCancelReq {
            reason: Some("员工提前返岗，销假".to_string()),
        })
        .expect("合法原因");
        assert_eq!(
            model::set_status(
                db(),
                id,
                STATUS_APPROVED,
                STATUS_CANCELLED,
                Some(&cancel.reason),
                Some(fx.user)
            )
            .await
            .expect("销假"),
            1
        );
        let cancelled = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(cancelled.status, STATUS_CANCELLED);
        assert_eq!(cancelled.cancel_reason.as_deref(), Some("员工提前返岗，销假"));
        assert_eq!(cancelled.cancelled_by, Some(fx.user));
        assert!(cancelled.cancelled_at.is_some());
        assert_eq!(cancelled.submitted_at, resubmitted.submitted_at, "销假不该抹掉提交时间");

        // 作废后再改 / 再删都不行
        let cancelled_patch = leave_dto::patch_of(&LeaveUpdateReq::default(), &cancelled.draft_state()).expect("patch");
        assert_eq!(
            model::update(db(), id, &cancelled_patch, Some(fx.user))
                .await
                .expect("改"),
            0
        );
        assert_eq!(model::soft_delete(db(), id, Some(fx.user)).await.expect("删"), 0);

        // CAS 用过期旧状态 → 0 行（并发保护）
        assert_eq!(
            model::set_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
                .await
                .expect("过期 CAS"),
            0,
            "旧状态 CAS 必须影响 0 行"
        );
        println!("[PASS] 状态机四条边 + 撤回保留审计列（耗时 {:?}）", Utc::now() - now);

        cleanup(&[&fx]).await;
    }

    /// 3. 草稿软删：库里行还在（`deleted_at` 置位），但读不出来；再建同员工新单不受影响。
    #[tokio::test]
    async fn soft_delete_keeps_the_row_but_hides_it() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("sd").await;
        let id = fx.create(&fx.create_req()).await;
        assert_eq!(model::soft_delete(db(), id, Some(fx.user)).await.expect("软删"), 1);
        assert!(model::find_in(db(), id).await.expect("读").is_none(), "软删后读不到");
        // 行还在（软删不是硬删：附件与状态日志按 entity_id 关联）
        let row = db()
            .query_first_sql(
                "SELECT deleted_at FROM biz_leave_request WHERE id = ?::uuid".to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("读 deleted_at")
            .expect("行还在");
        assert!(
            row.get_typed::<Option<DateTime<Utc>>>("deleted_at")
                .expect("列")
                .is_some()
        );
        // 重复软删幂等（第二次 0 行）
        assert_eq!(model::soft_delete(db(), id, Some(fx.user)).await.expect("再删"), 0);

        cleanup(&[&fx]).await;
    }

    /// 4. 作废不给原因必须被库层 `ck_biz_leave_cancel` 拦下，且翻成 400 人话。
    #[tokio::test]
    async fn cancel_without_reason_is_rejected_by_the_database() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("ck").await;
        let id = fx.create(&fx.create_req()).await;
        model::set_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
            .await
            .expect("提交");
        model::set_status(db(), id, STATUS_SUBMITTED, STATUS_APPROVED, None, Some(fx.user))
            .await
            .expect("批准");

        // 绕过 DTO 直接写 → 库层必须报 check_violation（接口层已拦，这里是最后兜底）
        let err = model::set_status(db(), id, STATUS_APPROVED, STATUS_CANCELLED, None, Some(fx.user))
            .await
            .expect_err("作废不给原因必须被库拒");
        let api = model::write_error(err, "作废请假单", "LV-TEST");
        assert_eq!(api.status, 400);
        assert!(api.msg.contains("作废原因"), "报文要点名字段：{}", api.msg);

        // 状态没被改坏（整条语句回滚）
        let row = model::find_in(db(), id).await.expect("读").expect("在");
        assert_eq!(row.status, STATUS_APPROVED);

        cleanup(&[&fx]).await;
    }

    /// 5. 单号撞唯一键 → 409 + `68011`（报文点名单号）。
    #[tokio::test]
    async fn duplicate_leave_no_maps_to_409() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("dup").await;
        let first = fx.create(&fx.create_req()).await;
        let first_no = model::find_in(db(), first).await.expect("读").expect("在").leave_no;

        // 用同一个号再插一张（模拟「取号与建单不在同一事务」或手工插行）
        let draft = leave_dto::create_of(&fx.create_req()).expect("合法");
        let err = model::insert(
            db(),
            LeaveInsert {
                id: Uuid::now_v7(),
                leave_no: &first_no,
                employee_no: &fx.emp_no,
                employee_name: &fx.emp_name,
                dept_id: Some(fx.dept),
                draft: &draft,
            },
            Some(fx.user),
        )
        .await
        .expect_err("同号必须撞唯一键");
        let api = model::write_error(err, "建请假单", &first_no);
        assert_eq!(api.status, 409);
        assert_eq!(api.code, 68011, "必须翻成 HR_LEAVE_NO_DUPLICATE");
        assert!(api.msg.contains(&first_no), "报文要点名单号：{}", api.msg);

        cleanup(&[&fx]).await;
    }

    /// 6. 分页按 `dept_id` 数据权限裁；`mine` 只看本人。
    #[tokio::test]
    async fn page_respects_dept_scope_and_mine_flag() {
        if !ensure_db().await {
            return;
        }
        let fx_a = fixture("pa").await;
        let fx_b = fixture("pb").await;
        let a_id = fx_a.create(&fx_a.create_req()).await;
        let b_id = fx_b.create(&fx_b.create_req()).await;

        assert!(
            SCOPED_TABLES.contains(&model::LEAVE_TARGET),
            "biz_leave_request 必须登记进 SCOPED_TABLES，否则分页绕过数据权限"
        );

        // 超管（All 范围）看得见两张
        let all = super_principal(fx_a.dept);
        let (rows, total) = model::page(&leave_dto::LeaveFilter::default(), 1, 200, &all)
            .await
            .expect("超管分页");
        assert!(total >= 2, "超管应至少看到两张（实际 {total}）");
        assert!(rows.iter().any(|row| row.id == a_id));
        assert!(rows.iter().any(|row| row.id == b_id));

        // 只看得见本部门（Dept 范围）：A 部门的账号看不到 B 部门那张
        let scoped = dept_principal(fx_a.user, fx_a.dept);
        let (rows, _) = model::page(&leave_dto::LeaveFilter::default(), 1, 200, &scoped)
            .await
            .expect("部门分页");
        assert!(rows.iter().any(|row| row.id == a_id), "本部门的单必须看得见");
        assert!(
            rows.iter().all(|row| row.id != b_id),
            "跨部门的单不该出现在列表里（数据权限失效）"
        );
        assert!(rows.iter().all(|row| row.dept_id == Some(fx_a.dept)));

        // mine=true：只看「自己这份员工档」的假。principal 的 user_id 必须是**本人**的账号
        // （`page` 用 `principal.user_id()` 换 `biz_employee.user_id`，换成随机 uuid 就查不到）。
        let mine = leave_dto::filter_of(&LeavePageQuery {
            mine: Some(true),
            ..LeavePageQuery::default()
        })
        .expect("筛选合法");
        let mine_principal = ScopedPrincipal::for_test(fx_a.user, Some(fx_a.dept), true, ScopeKind::All, vec![], false);
        let (rows, _) = model::page(&mine, 1, 200, &mine_principal).await.expect("我的请假");
        assert!(rows.iter().any(|row| row.id == a_id), "自己的假要看得见");
        assert!(rows.iter().all(|row| row.id != b_id), "别人的假不该出现");

        cleanup(&[&fx_a, &fx_b]).await;
    }

    /// 7. 已批无薪假天数：只数 `APPROVED AND paid = 0`，且按区间相交。
    #[tokio::test]
    async fn approved_unpaid_days_counts_only_approved_unpaid() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("days").await;

        // 已批 + 无薪 2 天（应计入）
        let mut unpaid = fx.create_req();
        unpaid.paid = Some(leave_dto::PAID_NO);
        let unpaid_id = fx.create(&unpaid).await;
        model::set_status(db(), unpaid_id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
            .await
            .expect("提交");
        model::set_status(db(), unpaid_id, STATUS_SUBMITTED, STATUS_APPROVED, None, Some(fx.user))
            .await
            .expect("批准");

        // 已批 + 带薪 2 天（不该计入：薪资扣款只看 paid）
        let mut paid = fx.create_req();
        paid.paid = Some(leave_dto::PAID_YES);
        let paid_id = fx.create(&paid).await;
        model::set_status(db(), paid_id, STATUS_DRAFT, STATUS_SUBMITTED, None, Some(fx.user))
            .await
            .expect("提交");
        model::set_status(db(), paid_id, STATUS_SUBMITTED, STATUS_APPROVED, None, Some(fx.user))
            .await
            .expect("批准");

        // 草稿 + 无薪 2 天（不该计入：没批准）
        let mut draft = fx.create_req();
        draft.paid = Some(leave_dto::PAID_NO);
        fx.create(&draft).await;

        let (start, _) = window();
        let from = start - Duration::days(1);
        let to = start + Duration::days(7);
        let total = model::approved_unpaid_days(db(), fx.employee, from, to)
            .await
            .expect("汇总无薪假");
        assert_eq!(total, money("2"), "只应数已批准的无薪假");

        // 区间不相交 ⇒ 0（窗口挪远）
        let far = model::approved_unpaid_days(
            db(),
            fx.employee,
            start + Duration::days(60),
            start + Duration::days(90),
        )
        .await
        .expect("区间外汇总");
        assert_eq!(far, Decimal::ZERO);

        cleanup(&[&fx]).await;
    }

    /// 8. 单号取号：连续两次取到不同号，形态 `LV-yyyyMMdd-000001`。
    #[tokio::test]
    async fn next_leave_no_is_monotonic_and_well_formed() {
        if !ensure_db().await {
            return;
        }
        let now = Utc::now();
        let first = model::next_leave_no(db(), now).await.expect("取号1");
        let second = model::next_leave_no(db(), now).await.expect("取号2");
        assert_ne!(first, second, "同一事务外连续取号必须不重");
        let offset = nik::prelude::chrono::FixedOffset::east_opt(employee_dto::TZ_OFFSET_SECS).expect("+08:00");
        let day = now.with_timezone(&offset).format("%Y%m%d").to_string();
        assert!(
            first.starts_with(&format!("LV-{day}-")),
            "单号要按北京时间的日期滚动：{first}"
        );
        let seq = first.rsplit('-').next().expect("流水段");
        assert_eq!(seq.len(), model::NO_SEQ_WIDTH, "6 位补零：{first}");
    }
}
