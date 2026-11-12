//! 请假单接口层（`biz_leave_request`）的真库用例（HR-25）。
//!
//! 单测（`leave_handler` 的作废三岔口 / 状态机边 / 权限码 / 表单变量）证明的是**映射与常量**；
//! 证明不了五件事，只有真库能答：
//!
//! 1. **6 个接口全通**：建 / 改 / 查 / 列表 / 提交 / 作废 的 SQL 与事务真能跑
//!    （`?::text::timestamptz`、`?::text::numeric`、`NULLIF(?::text,'')::uuid` 这些写法
//!    只要有一处错，接口就是 500 而不是 400）；
//! 2. **状态机正确**（本卡验收项）：`DRAFT → SUBMITTED → DRAFT`（撤回）、
//!    `APPROVED → CANCELLED`（销假）真能走，非法边（`SUBMITTED → CANCELLED`、
//!    锁死态再改）真被拒，每次流转都在 `biz_status_log` 里；
//! 3. **验收项「单测覆盖撤回 / 作废边界」的真库那一半**：
//!    - 撤回：终止在跑实例 + 回草稿 + 清实例引用 ⇒ 可以**再提交一次**（不撞
//!      `uk_biz_flow_instance_biz_active`）；
//!    - 作废：`APPROVED → CANCELLED` 必须带原因（库 `ck_biz_leave_cancel`），原因落 `cancel_reason`；
//!    - 删除：草稿软删（`deleted_at`），状态**不变**，再操作就是 404。
//! 4. **没有已发布流程时提交是 fail-closed**（设计 §4.3-3）：409 + `66033`，
//!    单据留在 `DRAFT` 且**没有**实例（不留死单）；
//! 5. **详情带审批轨迹**：实例 + 时间线 + 审批人姓名（`AGENTS.md` 约定 13 的审计列）。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@127.0.0.1:15432/hr25_leave_drill \
//!   cargo test --lib business::hr::leave_handler_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-19 / HR-24 的教训）
//!
//! - 员工走真实 `employee_handler::create_employee`（请假单的 `employee_id` 是
//!   `RESTRICT` 外键，自己拼 INSERT 会漏掉 `uk_biz_employee_*` 的口径）；
//! - 清场**按 id 删**（绝不按前缀扫全库），FK 顺序：
//!   审批待办 → 实例节点 → 实例 → 请假单 → 状态日志 → 员工 → 用户 → 部门；
//! - 审批流程用**测试内建的已发布流程**（`hr_leave` 的正式种子属 **HR-26**，
//!   本卡只消费不种）。没有流程的那条用例会在自己前后把流程「停用 / 恢复」，
//!   这样用例的执行顺序不影响结果。

use nik::prelude::*;

#[cfg(test)]
mod db {
    use super::*;
    use crate::business::hr::employee_handler;
    use crate::business::hr::leave_dto::{
        self, LeaveCancelReq, LeaveCreateReq, LeavePageQuery, LeaveSubmitReq, LeaveUpdateReq,
    };
    use crate::business::hr::leave_handler;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use crate::business::shared::state_machine::{self, Operator};
    use crate::shared::error_code;
    use crate::shared::test_helpers;
    use nik::prelude::rust_decimal::Decimal;
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

    fn uniq() -> u64 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        now.wrapping_add(seq.wrapping_mul(7_919))
    }

    /// 夹具：一个部门 + 一名员工（带登录账号） + 一名审批人（只有账号，不是员工）。
    struct Fixture {
        dept: Uuid,
        dept_code: String,
        emp: Uuid,
        emp_no: String,
        emp_name: String,
        emp_username: String,
        emp_user: Uuid,
        approver: Uuid,
        approver_name: String,
    }

    async fn fresh_user(tag: &str, dept: Option<Uuid>) -> (Uuid, String, String) {
        let suffix = uniq();
        let username = format!("hr25.{tag}.{suffix}");
        let name = format!("HR25 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, dept).await;
        (user, username, name)
    }

    async fn fixture(tag: &str) -> Fixture {
        let dept_code = format!("HR25-{tag}-{}", uniq());
        let dept = test_helpers::ensure_dept(&dept_code, &format!("HR25 部门 {tag}")).await;
        let (emp_user, emp_username, emp_name) = fresh_user(&format!("{tag}.emp"), Some(dept)).await;
        let emp_no = format!("H25{:012x}", uniq() % 0xffff_ffff_ffff);
        let created = employee_handler::create_employee(
            Some(emp_user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(emp_no.clone()),
                name: Some(emp_name.clone()),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                dept_id: Some(dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                user_id: Some(emp_user.to_string()),
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建员工");
        let emp = Uuid::parse_str(&created.id).expect("员工 id");
        let (approver, _, approver_name) = fresh_user(&format!("{tag}.appr"), Some(dept)).await;
        Fixture {
            dept,
            dept_code,
            emp,
            emp_no,
            emp_name,
            emp_username,
            emp_user,
            approver,
            approver_name,
        }
    }

    impl Fixture {
        fn operator(&self) -> Operator {
            Operator::named(Some(self.emp_user), self.emp_name.clone())
        }

        fn create_req(&self) -> LeaveCreateReq {
            LeaveCreateReq {
                employee_id: Some(self.emp.to_string()),
                leave_type: Some("ANNUAL".to_string()),
                start_time: Some("2026-11-10 09:00".to_string()),
                end_time: Some("2026-11-11 18:00".to_string()),
                duration_days: Some(Decimal::new(20, 1)),
                paid: Some(1),
                reason: Some("家里有事".to_string()),
                remark: Some("HR25 夹具".to_string()),
            }
        }

        fn principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(
                self.emp_user,
                Some(self.dept),
                false,
                ScopeKind::Dept,
                vec![self.dept],
                false,
            )
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(self.emp_user, Some(self.dept), true, ScopeKind::All, vec![], false)
        }

        /// 该请假单的库内状态（软删也看得见 —— 用来看 `deleted_at`）。
        async fn state(&self, leave_id: Uuid) -> (String, bool, Option<String>, Option<Uuid>, Option<String>) {
            let row = db()
                .query_first_sql(
                    "SELECT status, deleted_at IS NOT NULL AS deleted, cancel_reason, \
                            approval_instance_id, employee_no \
                       FROM biz_leave_request WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&leave_id)],
                )
                .await
                .expect("读请假单状态")
                .expect("请假单必有行");
            (
                row.get_typed("status").expect("status"),
                row.get_typed("deleted").expect("deleted"),
                row.get_typed("cancel_reason").expect("cancel_reason"),
                row.get_typed("approval_instance_id").expect("approval_instance_id"),
                row.get_typed("employee_no").expect("employee_no"),
            )
        }

        /// 该单的状态日志（`from_status, to_status` 序列，按时间升序）。
        async fn log_edges(&self, leave_id: Uuid) -> Vec<(Option<String>, String)> {
            let rows = db()
                .find_with_params(
                    "SELECT from_status, to_status FROM biz_status_log \
                      WHERE entity_type = 'LEAVE_REQUEST' AND entity_id = ?::uuid \
                      ORDER BY created_at, id",
                    vec![Value::from(&leave_id)],
                )
                .await
                .expect("读状态日志");
            rows.iter()
                .map(|row| {
                    (
                        row.get_typed("from_status").expect("from_status"),
                        row.get_typed("to_status").expect("to_status"),
                    )
                })
                .collect()
        }

        /// 库里在跑的实例（`None` = 没有）。
        async fn running_instance(&self, leave_id: Uuid) -> Option<(Uuid, String)> {
            let row = db()
                .query_first_sql(
                    "SELECT id, status FROM biz_flow_instance \
                      WHERE biz_type = ? AND biz_id = ?::uuid AND deleted_at IS NULL \
                      ORDER BY started_at DESC LIMIT 1"
                        .to_string(),
                    vec![
                        Value::Text(leave_handler::FLOW_BIZ_TYPE.to_string()),
                        Value::from(&leave_id),
                    ],
                )
                .await
                .expect("读审批实例");
            row.map(|row| {
                (
                    row.get_typed("id").expect("实例 id"),
                    row.get_typed("status").expect("实例状态"),
                )
            })
        }

        /// 绕过接口把单据直接推到某状态（**仅用于构造前置态**：审批通过属 HR-27 的桥，
        /// 本卡没有「审批通过」的接口；撤回 / 作废的**断言**仍然走真实 handler）。
        async fn force_status(&self, leave_id: Uuid, from: &str, to: &str) {
            let row = db()
                .query_first_sql(
                    "SELECT leave_no FROM biz_leave_request WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&leave_id)],
                )
                .await
                .expect("读单号")
                .expect("必有行");
            let leave_no: String = row.get_typed("leave_no").expect("leave_no");
            state_machine::transition(
                db(),
                state_machine::ENTITY_LEAVE_REQUEST,
                leave_id,
                &leave_no,
                &state_machine::LEAVE_EDGES,
                from,
                to,
                self.operator(),
                Some("夹具直推状态"),
                None,
            )
            .await
            .expect("夹具状态流转");
            db().exec_sql(
                "UPDATE biz_leave_request SET status = ?, updated_at = now() WHERE id = ?::uuid".to_string(),
                vec![Value::Text(to.to_string()), Value::from(&leave_id)],
            )
            .await
            .expect("回写夹具状态");
        }

        /// 清场：审批待办 → 实例节点 → 实例 → 请假单 → 状态日志 → 员工 → 用户 → 部门。
        async fn clean(&self, leave_ids: &[Uuid]) {
            for leave_id in leave_ids {
                for sql in [
                    "DELETE FROM biz_flow_task WHERE instance_id IN \
                       (SELECT id FROM biz_flow_instance WHERE biz_id = ?::uuid)",
                    "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
                       (SELECT id FROM biz_flow_instance WHERE biz_id = ?::uuid)",
                    "DELETE FROM biz_flow_instance WHERE biz_id = ?::uuid",
                ] {
                    let _ = db().exec_sql(sql.to_string(), vec![Value::from(leave_id)]).await;
                }
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_status_log WHERE entity_type = 'LEAVE_REQUEST' AND entity_id = ?::uuid"
                            .to_string(),
                        vec![Value::from(leave_id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_leave_request WHERE id = ?::uuid".to_string(),
                        vec![Value::from(leave_id)],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.emp)],
                )
                .await;
            test_helpers::purge_user(&self.emp_username).await;
            test_helpers::purge_user(&self.approver_name).await;
            test_helpers::purge_dept(&self.dept_code).await;
        }
    }

    // ==================== 审批流程夹具（测试内建，正式种子属 HR-26） ====================

    /// 为本次用例**重建**一条已发布的 `hr_leave` 流程：`START → 审批人(USER) → END`。
    ///
    /// 只写 `biz_flow_def` + `biz_flow_def_version`（运行时引擎只读版本的 `graph_snapshot`，
    /// 见 `flow::engine::load_snapshot`），所以不需要 `biz_flow_node` / `biz_flow_edge` 行。
    ///
    /// # 为什么「重建」而不是「复用」
    ///
    /// 审批人是**每个用例自己的夹具账号**（快照里写死了 `assignees[0].ref_id`）。复用上一轮
    /// 那条流程，待办会派给上一个用例已经清理掉的用户 —— 待办生成成功、但审批人是幽灵账号。
    /// 所以每轮先删掉**没有被实例引用**的旧定义（有实例引用时 FK 会拒绝删除，那种情况下
    /// 用例本该在清场阶段就删干净实例），再插一份新的。
    async fn seed_leave_flow(approver: Uuid) {
        let stale = db()
            .find_with_params(
                "SELECT d.id FROM biz_flow_def d \
                  WHERE d.flow_code = ? AND d.deleted_at IS NULL \
                    AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i WHERE i.def_id = d.id)",
                vec![Value::Text(leave_handler::FLOW_CODE.to_string())],
            )
            .await
            .expect("查旧流程定义");
        for row in &stale {
            let id: Uuid = row.get_typed("id").expect("流程 id");
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid".to_string(),
                    vec![Value::from(&id)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_flow_def WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&id)],
                )
                .await;
        }
        let def_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let snapshot = serde_json::json!({
            "schema_version": 1,
            "nodes": [
                {
                    "key": "s", "name": "开始", "node_type": "START",
                    "assignee_type": null, "approval_mode": null, "approve_ratio": null,
                    "reject_veto": false, "join_mode": "AND", "fork_mode": "AND",
                    "form_var_key": null, "superior_levels": 0,
                    "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW",
                    "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND",
                    "remind_before_hours": null, "max_remind_times": 0, "cc_on_approved": false,
                    "x": 0, "y": 0, "sort_order": 0, "config": {},
                    "assignees": []
                },
                {
                    "key": "n1", "name": "直属上级审批", "node_type": "APPROVAL",
                    "assignee_type": "USER", "approval_mode": "ANY", "approve_ratio": null,
                    "reject_veto": false, "join_mode": "AND", "fork_mode": "AND",
                    "form_var_key": null, "superior_levels": 0,
                    "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW",
                    "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND",
                    "remind_before_hours": null, "max_remind_times": 0, "cc_on_approved": false,
                    "x": 200, "y": 0, "sort_order": 1, "config": {},
                    "assignees": [
                        {"assignee_type": "USER", "ref_id": approver.to_string(), "sort_order": 0}
                    ]
                },
                {
                    "key": "e", "name": "结束", "node_type": "END",
                    "assignee_type": null, "approval_mode": null, "approve_ratio": null,
                    "reject_veto": false, "join_mode": "AND", "fork_mode": "AND",
                    "form_var_key": null, "superior_levels": 0,
                    "empty_assignee": "FALLBACK_ADMIN", "self_approval": "ALLOW",
                    "allow_robot": false, "timeout_hours": null, "timeout_action": "REMIND",
                    "remind_before_hours": null, "max_remind_times": 0, "cc_on_approved": false,
                    "x": 400, "y": 0, "sort_order": 2, "config": {},
                    "assignees": []
                }
            ],
            "edges": [
                {"key": "e1", "from": "s", "to": "n1", "condition": null, "label": null,
                 "priority": 0, "is_default": false},
                {"key": "e2", "from": "n1", "to": "e", "condition": null, "label": null,
                 "priority": 0, "is_default": false}
            ]
        });
        db().exec_sql(
            "INSERT INTO biz_flow_def \
                 (id, flow_code, flow_name, biz_type, category, description, status, \
                  current_version_id, current_version_no, is_default, allow_auto_approve, \
                  created_at, updated_at) \
             VALUES (?::uuid, ?, '请假审批（HR25 夹具）', ?, 'HR', 'HR-25 真库用例内建，不进迁移', \
                     'PUBLISHED', ?::uuid, 1, TRUE, FALSE, now(), now())"
                .to_string(),
            vec![
                Value::from(&def_id),
                Value::Text(leave_handler::FLOW_CODE.to_string()),
                Value::Text(leave_handler::FLOW_BIZ_TYPE.to_string()),
                Value::from(&version_id),
            ],
        )
        .await
        .expect("插流程定义");
        db().exec_sql(
            "INSERT INTO biz_flow_def_version \
                 (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot, \
                  node_count, edge_count, change_note, published_at, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, 1, 'PUBLISHED', TRUE, repeat('0', 64), ?::text::jsonb, 3, 2, \
                     'HR25 夹具', now(), now(), now())"
                .to_string(),
            vec![
                Value::from(&version_id),
                Value::from(&def_id),
                Value::Text(snapshot.to_string()),
            ],
        )
        .await
        .expect("插流程版本");
    }

    /// 把 `hr_leave` 流程临时下线（`status='DRAFT'`），返回恢复用的 id 列表。
    ///
    /// 为什么不用「直接假定库里没有流程」：用例执行顺序不该影响结果。这条
    /// 帮助函数让「没有已发布流程」这件事**在本用例内必然成立**，跑完再恢复。
    async fn park_leave_flows() -> Vec<Uuid> {
        let rows = db()
            .find_with_params(
                "SELECT id FROM biz_flow_def \
                  WHERE deleted_at IS NULL AND status = 'PUBLISHED' \
                    AND (biz_type = ? OR lower(flow_code) = ?)",
                vec![
                    Value::Text(leave_handler::FLOW_BIZ_TYPE.to_string()),
                    Value::Text(leave_handler::FLOW_CODE.to_string()),
                ],
            )
            .await
            .expect("查已发布流程");
        let ids: Vec<Uuid> = rows.iter().map(|row| row.get_typed("id").expect("流程 id")).collect();
        for id in &ids {
            db().exec_sql(
                "UPDATE biz_flow_def SET status = 'DRAFT', updated_at = now() WHERE id = ?::uuid".to_string(),
                vec![Value::from(id)],
            )
            .await
            .expect("下线流程");
        }
        ids
    }

    async fn restore_leave_flows(ids: &[Uuid]) {
        for id in ids {
            db().exec_sql(
                "UPDATE biz_flow_def SET status = 'PUBLISHED', updated_at = now() WHERE id = ?::uuid".to_string(),
                vec![Value::from(id)],
            )
            .await
            .expect("恢复流程");
        }
    }

    fn leave_id_of(res: &leave_dto::LeaveRes) -> Uuid {
        Uuid::parse_str(&res.id).expect("回执 id")
    }

    // ==================== ① 建单：取号 + 员工快照 ====================

    /// 建单落库：单号 `LV-…`、状态 DRAFT、员工三列快照来自员工档（不是前端）、
    /// 建单日志一条、列表能查到。
    #[tokio::test]
    async fn create_copies_employee_snapshot_and_takes_no() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("create").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        assert!(created.leave_no.starts_with("LV-"), "单号前缀：{}", created.leave_no);
        assert_eq!(created.status, leave_dto::STATUS_DRAFT);
        assert_eq!(created.status_label, "草稿");
        assert!(!created.locked);
        assert_eq!(created.employee_id, fx.emp.to_string());
        assert_eq!(created.employee_no, fx.emp_no, "工号必须抄员工档（不信前端）");
        assert_eq!(created.employee_name, fx.emp_name, "姓名必须抄员工档");
        assert_eq!(created.dept_id.as_deref(), Some(fx.dept.to_string().as_str()));
        assert!(
            created.dept_name.contains("HR25 部门"),
            "部门名实时 join：{}",
            created.dept_name
        );
        assert_eq!(created.leave_type_label, "年假");
        assert_eq!(created.paid_label, "带薪");
        assert_eq!(created.duration_days, Decimal::new(20, 1));
        assert!(created.approval_instance_id.is_none(), "草稿没有实例");

        // 库里的快照列与出参一致（不是只有出参做了映射）
        let (status, deleted, _, instance, employee_no) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_DRAFT);
        assert!(!deleted);
        assert!(instance.is_none());
        assert_eq!(employee_no.as_deref(), Some(fx.emp_no.as_str()));

        // 建单日志一条：from=NULL, to=DRAFT
        let edges = fx.log_edges(leave_id).await;
        assert_eq!(edges.len(), 1, "建单要写一条 biz_status_log");
        assert_eq!(edges[0].0, None);
        assert_eq!(edges[0].1, leave_dto::STATUS_DRAFT);

        // 列表能查到（按工号过滤）
        let filter = leave_dto::filter_of(&LeavePageQuery {
            keyword: Some(fx.emp_no.clone()),
            ..LeavePageQuery::default()
        })
        .expect("合法查询");
        let (list, total) = leave_handler::page_leaves(&fx.super_principal(), &filter, 1, 20)
            .await
            .expect("列表");
        assert_eq!(total, 1);
        assert_eq!(list[0].id, created.id);

        fx.clean(&[leave_id]).await;
    }

    /// 已离职员工不能建请假单（409 + 68012，报文点名员工）。
    #[tokio::test]
    async fn create_rejects_resigned_employee() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("resigned").await;
        db().exec_sql(
            "UPDATE biz_employee SET status = 'RESIGNED', leave_date = DATE '2026-11-01', \
                    leave_type = 'RESIGN' WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&fx.emp)],
        )
        .await
        .expect("把员工改成离职");

        let error = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect_err("离职员工必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_LEAVE_RULE_BLOCKED);
        assert!(error.msg.contains(&fx.emp_no), "报文要点名员工：{}", error.msg);

        fx.clean(&[]).await;
    }

    // ==================== ② 改草稿 ====================

    /// 改草稿：部分更新（只传天数）按合并后的整体校验；锁死态（已提交）被 409 挡住，
    /// 且**库里的值逐列未变**。
    #[tokio::test]
    async fn update_merges_partial_and_blocks_locked() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("update").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        // 只改天数 + 事由：其余字段保持库里的值
        let updated = leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &LeaveUpdateReq {
                duration_days: Some(Decimal::new(15, 1)),
                reason: Some("  改成一天半  ".to_string()),
                ..LeaveUpdateReq::default()
            },
        )
        .await
        .expect("改草稿");
        assert_eq!(updated.duration_days, Decimal::new(15, 1));
        assert_eq!(updated.reason.as_deref(), Some("改成一天半"), "事由 trim 后落库");
        assert_eq!(updated.leave_type, "ANNUAL", "没传的字段保持不变");
        assert_eq!(updated.start_time, created.start_time);
        assert_eq!(updated.employee_id, created.employee_id, "员工不可改");

        // 天数超过跨度 → 400（契约层校验，不是库层 CHECK）
        let too_long = leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &LeaveUpdateReq {
                duration_days: Some(Decimal::new(90, 1)),
                ..LeaveUpdateReq::default()
            },
        )
        .await
        .expect_err("天数超过跨度必须拒绝");
        assert_eq!(too_long.status, 400, "{}", too_long.msg);

        // 提交后（SUBMITTED）改草稿 → 409 + 68012，库里值不变
        seed_leave_flow(fx.approver).await;
        leave_handler::submit_leave(fx.operator(), leave_id, &LeaveSubmitReq::default())
            .await
            .expect("提交审批");
        let blocked = leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &LeaveUpdateReq {
                duration_days: Some(Decimal::new(25, 1)),
                ..LeaveUpdateReq::default()
            },
        )
        .await
        .expect_err("审批中的单不能改");
        assert_eq!(blocked.status, 409);
        assert_eq!(blocked.code, error_code::HR_LEAVE_RULE_BLOCKED);

        // 库里天数仍是 1.5（没被偷偷改掉）
        let row = db()
            .query_first_sql(
                "SELECT duration_days::text AS days FROM biz_leave_request WHERE id = ?::uuid".to_string(),
                vec![Value::from(&leave_id)],
            )
            .await
            .expect("读天数")
            .expect("必有行");
        assert_eq!(row.get_typed::<String>("days").expect("days"), "1.50");

        fx.clean(&[leave_id]).await;
    }

    // ==================== ③ 提交：没有流程 = fail-closed ====================

    /// 没有已发布的 `hr_leave` 流程 → 409 + 66033，单据留在 DRAFT，**没有实例**。
    ///
    /// 这条是设计 §4.3-3 的 fail-closed 口径：不许「没流程就直接生效」，
    /// 也不许留下「实例已建、状态没变」的中间态（AGENTS.md 约定 11）。
    #[tokio::test]
    async fn submit_without_flow_is_fail_closed() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("noflow").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        let parked = park_leave_flows().await;
        let error = leave_handler::submit_leave(fx.operator(), leave_id, &LeaveSubmitReq::default())
            .await
            .expect_err("没有流程必须拒绝");
        restore_leave_flows(&parked).await;

        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::APPROVAL_FLOW_NOT_CONFIGURED);
        assert!(
            error.msg.contains(leave_handler::FLOW_CODE),
            "报文要点名流程编码：{}",
            error.msg
        );
        // 单据仍是草稿、没有实例、没有新增状态日志（没留半条腿）
        let (status, deleted, _, instance, _) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_DRAFT, "失败后必须留在草稿（可改可删）");
        assert!(!deleted);
        assert!(instance.is_none(), "不许建出实例");
        assert!(fx.running_instance(leave_id).await.is_none(), "库里也不许有实例");
        assert_eq!(fx.log_edges(leave_id).await.len(), 1, "只有建单那一条日志");

        // 失败后仍可正常操作（不是死单）：能改、能删
        leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &LeaveUpdateReq {
                reason: Some("流程没配，先改一下".to_string()),
                ..LeaveUpdateReq::default()
            },
        )
        .await
        .expect("失败后仍可改草稿");

        fx.clean(&[leave_id]).await;
    }

    // ==================== ④ 提交 → 撤回（本卡验收：撤回边界） ====================

    /// 提交建实例 → 撤回终止实例并回草稿 → **能再提交一次**（不撞唯一索引）。
    #[tokio::test]
    async fn submit_starts_instance_then_withdraw_terminates() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("withdraw").await;
        seed_leave_flow(fx.approver).await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        let submitted = leave_handler::submit_leave(fx.operator(), leave_id, &LeaveSubmitReq::default())
            .await
            .expect("提交审批");
        assert_eq!(submitted.status, leave_dto::STATUS_SUBMITTED);
        assert_eq!(submitted.status_label, "审批中");
        let instance_id =
            Uuid::parse_str(submitted.approval_instance_id.as_deref().expect("提交后必须挂实例")).expect("实例 id");
        assert!(submitted.submitted_at.is_some());
        assert!(submitted.submitted_by.is_some());
        // 实例在跑 + 生成了一条待办给夹具审批人
        let (running, status) = fx.running_instance(leave_id).await.expect("库里必须有实例");
        assert_eq!(running, instance_id);
        assert_eq!(status, "PENDING");
        let tasks = db()
            .find_with_params(
                "SELECT assignee_id, status FROM biz_flow_task WHERE instance_id = ?::uuid",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("读审批待办");
        assert_eq!(tasks.len(), 1, "单节点单审批人 ⇒ 一条待办");
        assert_eq!(
            tasks[0].get_typed::<Uuid>("assignee_id").expect("assignee"),
            fx.approver
        );

        // 撤回：必须走真实 handler（action=WITHDRAWN）
        let withdrawn = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("填错了，重新提".to_string()),
            },
        )
        .await
        .expect("撤回");
        assert_eq!(withdrawn.action, "WITHDRAWN");
        assert_eq!(withdrawn.action_label, "已撤回（回到草稿）");
        assert_eq!(withdrawn.leave.status, leave_dto::STATUS_DRAFT);
        assert!(withdrawn.leave.approval_instance_id.is_none(), "撤回要清实例引用");
        assert_eq!(withdrawn.instance_status.as_deref(), Some("CANCELLED"));

        // 实例已终止（不是删掉：审批轨迹要能回看）
        let (_, status) = fx.running_instance(leave_id).await.expect("实例仍在（已终止）");
        assert_eq!(status, "CANCELLED");
        // 待办不再挂着
        let pending = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING'"
                    .to_string(),
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("读待办数")
            .expect("count 必有行");
        assert_eq!(
            pending.get_typed::<i64>("n").expect("n 列"),
            0,
            "撤回后不许留下孤儿待办"
        );

        // 状态日志：建单 → 提交 → 撤回
        let edges = fx.log_edges(leave_id).await;
        assert_eq!(
            edges
                .iter()
                .map(|(from, to)| (from.clone(), to.clone()))
                .collect::<Vec<_>>(),
            vec![
                (None, "DRAFT".to_string()),
                (Some("DRAFT".to_string()), "SUBMITTED".to_string()),
                (Some("SUBMITTED".to_string()), "DRAFT".to_string()),
            ]
        );

        // **再提交一次**：撞不到 uk_biz_flow_instance_biz_active（旧实例已 CANCELLED）
        let resubmitted = leave_handler::submit_leave(
            fx.operator(),
            leave_id,
            &LeaveSubmitReq {
                remark: Some("重新提交".to_string()),
            },
        )
        .await
        .expect("撤回后必须能重提");
        assert_eq!(resubmitted.status, leave_dto::STATUS_SUBMITTED);
        assert_ne!(
            resubmitted.approval_instance_id.as_deref(),
            Some(instance_id.to_string().as_str()),
            "重提是新实例"
        );

        fx.clean(&[leave_id]).await;
    }

    /// 撤回的边界：`SUBMITTED` 但单据上**没有实例引用**（历史脏数据 / 实例被硬删）时，
    /// 撤回仍要回草稿，且不报错（`instance_status = None`）。
    #[tokio::test]
    async fn withdraw_without_instance_still_returns_to_draft() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("withdrawnoin").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);
        fx.force_status(leave_id, leave_dto::STATUS_DRAFT, leave_dto::STATUS_SUBMITTED)
            .await;

        let withdrawn = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("没有实例也要能撤回".to_string()),
            },
        )
        .await
        .expect("撤回");
        assert_eq!(withdrawn.action, "WITHDRAWN");
        assert_eq!(withdrawn.leave.status, leave_dto::STATUS_DRAFT);
        assert!(withdrawn.instance_status.is_none());

        fx.clean(&[leave_id]).await;
    }

    // ==================== ⑤ 作废边界 ====================

    /// 草稿作废 = 软删：`deleted_at` 有值、状态**不变**、列表看不到、再操作 404。
    #[tokio::test]
    async fn cancel_draft_soft_deletes() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("delete").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        let deleted = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("写错了，删掉".to_string()),
            },
        )
        .await
        .expect("删草稿");
        assert_eq!(deleted.action, "DELETED");
        assert_eq!(deleted.action_label, "草稿已删除");
        assert_eq!(deleted.leave.status, leave_dto::STATUS_DRAFT, "软删不改状态");
        assert!(deleted.instance_status.is_none());

        // 软删不是硬删：行还在，`deleted_at` 有值
        let (status, is_deleted, _, _, _) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_DRAFT);
        assert!(is_deleted, "必须写 deleted_at（软删）");

        // 列表 / 详情都看不到
        let filter = leave_dto::filter_of(&LeavePageQuery {
            keyword: Some(fx.emp_no.clone()),
            ..LeavePageQuery::default()
        })
        .expect("合法查询");
        let (list, total) = leave_handler::page_leaves(&fx.super_principal(), &filter, 1, 20)
            .await
            .expect("列表");
        assert_eq!(total, 0, "软删后不进列表");
        assert!(list.is_empty());
        let detail = leave_handler::detail_of(leave_id).await;
        assert!(detail.is_err(), "软删后详情 404");

        // 再操作（删除 / 撤回 / 作废）→ 404
        let again = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("再删一次试试".to_string()),
            },
        )
        .await;
        assert_eq!(again.expect_err("已删的单不能再操作").status, 404);

        fx.clean(&[leave_id]).await;
    }

    /// 已批准的假作废（销假）：原因必填 ≥5 字 → 进 `CANCELLED` 并把原因落 `cancel_reason`；
    /// 再作废一次 → 409。
    #[tokio::test]
    async fn cancel_approved_requires_reason_then_moves_to_cancelled() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("cancel").await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);
        // 前置态：审批通过（属 HR-27 的桥，这里直推）
        fx.force_status(leave_id, leave_dto::STATUS_DRAFT, leave_dto::STATUS_SUBMITTED)
            .await;
        fx.force_status(leave_id, leave_dto::STATUS_SUBMITTED, leave_dto::STATUS_APPROVED)
            .await;

        // 锁定态不能改（APPROVED 是 lock）
        let blocked = leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &LeaveUpdateReq {
                duration_days: Some(Decimal::new(10, 1)),
                ..LeaveUpdateReq::default()
            },
        )
        .await;
        assert_eq!(
            blocked.expect_err("已批准不能改").code,
            error_code::HR_LEAVE_RULE_BLOCKED
        );

        // 原因太短 → 400（设计 §5.2 #31 的 ≥5 字口径）
        let short = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("改期".to_string()),
            },
        )
        .await;
        assert_eq!(short.expect_err("原因太短").status, 400);

        // 正常作废
        let cancelled = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("员工实际未离岗，销假".to_string()),
            },
        )
        .await
        .expect("销假");
        assert_eq!(cancelled.action, "CANCELLED");
        assert_eq!(cancelled.leave.status, leave_dto::STATUS_CANCELLED);
        assert_eq!(cancelled.leave.status_label, "已作废");
        assert!(cancelled.leave.locked, "CANCELLED 是锁死态");
        assert_eq!(cancelled.leave.cancel_reason.as_deref(), Some("员工实际未离岗，销假"));
        assert!(cancelled.leave.cancelled_at.is_some());
        assert!(cancelled.leave.cancelled_by.is_some());

        // 库里三列都对得上（`ck_biz_leave_cancel` 要求 CANCELLED 必带原因）
        let (status, _, cancel_reason, _, _) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_CANCELLED);
        assert_eq!(cancel_reason.as_deref(), Some("员工实际未离岗，销假"));

        // 状态日志最后一条是 APPROVED → CANCELLED，且带原因
        let last = db()
            .query_first_sql(
                "SELECT from_status, to_status, reason FROM biz_status_log \
                  WHERE entity_type = 'LEAVE_REQUEST' AND entity_id = ?::uuid \
                  ORDER BY created_at DESC, id DESC LIMIT 1"
                    .to_string(),
                vec![Value::from(&leave_id)],
            )
            .await
            .expect("读最后日志")
            .expect("必有行");
        assert_eq!(
            last.get_typed::<Option<String>>("from_status").expect("from"),
            Some("APPROVED".to_string())
        );
        assert_eq!(last.get_typed::<String>("to_status").expect("to"), "CANCELLED");
        assert_eq!(
            last.get_typed::<Option<String>>("reason").expect("reason").as_deref(),
            Some("员工实际未离岗，销假")
        );

        // 再作废一次 → 409（已作废过）
        let again = leave_handler::cancel_leave(
            fx.operator(),
            leave_id,
            &LeaveCancelReq {
                reason: Some("再销一次假".to_string()),
            },
        )
        .await;
        let again = again.expect_err("已作废的单不能再作废");
        assert_eq!(again.status, 409);
        assert_eq!(again.code, error_code::HR_LEAVE_RULE_BLOCKED);

        fx.clean(&[leave_id]).await;
    }

    // ==================== ⑥ 详情：审批轨迹 ====================

    /// 详情返回单据本体 + 实例 + 时间线（审批人姓名来自流程快照列）。
    #[tokio::test]
    async fn detail_returns_approval_trace() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("detail").await;
        seed_leave_flow(fx.approver).await;
        let created = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建请假单");
        let leave_id = leave_id_of(&created);

        // 草稿：没有审批轨迹
        let draft = leave_handler::detail_of(leave_id).await.expect("详情");
        assert!(draft.approval.is_none());
        assert_eq!(draft.approval_history, 0);
        assert_eq!(draft.leave.leave_no, created.leave_no);

        leave_handler::submit_leave(fx.operator(), leave_id, &LeaveSubmitReq::default())
            .await
            .expect("提交审批");
        let detail = leave_handler::detail_of(leave_id).await.expect("详情");
        assert_eq!(detail.approval_history, 1);
        let approval = detail.approval.expect("提交后必须有审批轨迹");
        assert_eq!(approval.flow_code, leave_handler::FLOW_CODE);
        assert_eq!(approval.status, "PENDING");
        assert_eq!(approval.status_label, "审批中");
        assert_eq!(approval.timeline.len(), 1, "单节点一条时间线");
        let task = &approval.timeline[0];
        assert_eq!(task.node_key, "n1");
        assert_eq!(task.node_name, "直属上级审批", "节点名取实例节点快照");
        assert_eq!(task.assignee_id, fx.approver.to_string());
        assert!(!task.assignee_name.trim().is_empty(), "审批人姓名不许是空（约定 13）");
        assert_eq!(task.status, "PENDING");
        assert_eq!(task.status_label, "待审批");
        assert!(task.completed_at.is_none());

        fx.clean(&[leave_id]).await;
    }

    // ==================== ⑦ 列表：数据权限 ====================

    /// 列表按部门裁：另一个部门的请假单部门范围看不到；超管全看；
    /// `mine=true` 只出本人那份。
    #[tokio::test]
    async fn page_scopes_by_dept_and_mine() {
        if !ensure_db().await {
            return;
        }
        let fx = fixture("scope").await;
        let mine = leave_handler::create_leave(fx.operator(), &fx.create_req())
            .await
            .expect("建本部门请假单");

        // 另一个部门 + 另一名员工 + 另一张单（同一账号建，方便验证「部门裁」与「mine 裁」）
        let other_dept_code = format!("HR25-scope-other-{}", uniq());
        let other_dept = test_helpers::ensure_dept(&other_dept_code, "HR25 部门 other").await;
        let other_emp_no = format!("H25{:012x}", uniq() % 0xffff_ffff_ffff);
        let other_emp_name = format!("HR25 他人{}", uniq());
        let other_created = employee_handler::create_employee(
            Some(fx.emp_user),
            &employee_handler::EmployeeCreateReq {
                emp_no: Some(other_emp_no.clone()),
                name: Some(other_emp_name.clone()),
                gender: Some(1),
                birth_date: Some("1990-01-01".to_string()),
                dept_id: Some(other_dept.to_string()),
                hire_date: Some("2024-03-01".to_string()),
                // 同一个登录账号挂第二份员工档会被 uk_biz_employee_user 挡住 ⇒ 不挂账号
                user_id: None,
                ..employee_handler::EmployeeCreateReq::default()
            },
        )
        .await
        .expect("建他人员工");
        let other_emp = Uuid::parse_str(&other_created.id).expect("员工 id");
        let other = leave_handler::create_leave(
            fx.operator(),
            &LeaveCreateReq {
                employee_id: Some(other_emp.to_string()),
                ..fx.create_req()
            },
        )
        .await
        .expect("建他部门请假单");

        // ===== 断言口径：按**本夹具的工号**过滤，不靠「HR25」这种会命中历史残留的关键字 =====
        let query = |keyword: String, mine: bool| {
            leave_dto::filter_of(&LeavePageQuery {
                keyword: Some(keyword),
                mine: Some(mine),
                ..LeavePageQuery::default()
            })
            .expect("合法查询")
        };

        // ① 部门范围：本部门的单看得到，跨部门的单看不到
        let (list, total) = leave_handler::page_leaves(&fx.principal(), &query(fx.emp_no.clone(), false), 1, 200)
            .await
            .expect("部门列表");
        assert_eq!(total, 1, "部门范围看得到自己部门的单");
        assert_eq!(list[0].id, mine.id);
        let (cross, cross_total) =
            leave_handler::page_leaves(&fx.principal(), &query(other_emp_no.clone(), false), 1, 200)
                .await
                .expect("部门列表（跨部门）");
        assert_eq!(cross_total, 0, "部门范围看不到别的部门的单");
        assert!(cross.is_empty());

        // ② 超管：两张都看得到
        let (all_mine, total_mine) =
            leave_handler::page_leaves(&fx.super_principal(), &query(fx.emp_no.clone(), false), 1, 200)
                .await
                .expect("超管列表");
        assert_eq!(total_mine, 1);
        assert_eq!(all_mine[0].id, mine.id);
        let (all_other, total_other) =
            leave_handler::page_leaves(&fx.super_principal(), &query(other_emp_no.clone(), false), 1, 200)
                .await
                .expect("超管列表（他部门）");
        assert_eq!(total_other, 1);
        assert_eq!(all_other[0].id, other.id);

        // ③ mine=true：只看「我这份员工档」的假（他人那张虽然也是我建的，但不是我请的）
        let (mine_list, mine_total) =
            leave_handler::page_leaves(&fx.super_principal(), &query(fx.emp_no.clone(), true), 1, 200)
                .await
                .expect("我的请假");
        assert_eq!(mine_total, 1, "mine=true 出本人那份");
        assert_eq!(mine_list[0].id, mine.id);
        let (not_mine, not_mine_total) =
            leave_handler::page_leaves(&fx.super_principal(), &query(other_emp_no.clone(), true), 1, 200)
                .await
                .expect("我的请假（他人）");
        assert_eq!(not_mine_total, 0, "mine=true 不出别人请的假");
        assert!(not_mine.is_empty());

        // 清场（他人那张 + 他人员工 + 他人部门）
        let other_id = leave_id_of(&other);
        let mine_id = leave_id_of(&mine);
        fx.clean(&[mine_id, other_id]).await;
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                vec![Value::from(&other_emp)],
            )
            .await;
        test_helpers::purge_dept(&other_dept_code).await;
    }
}
