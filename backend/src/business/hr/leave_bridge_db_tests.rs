//! 请假审批回调桥（`leave_bridge`）的真库用例（HR-27）。
//!
//! 单测（`leave_bridge` 的 `#[cfg(test)] mod tests`）证明的是**映射与判据**；
//! 证明不了四件事，只有真库能答：
//!
//! 1. **审批通过真的回写**：`SUBMITTED → APPROVED` + `biz_status_log` 一条 + 实例引用保留
//!    + 重复回调幂等（不重复写日志、不炸）；
//! 2. **驳回真的打回且能重提**：`SUBMITTED → DRAFT`、驳回意见进状态日志、**再提交一次不撞
//!    `uk_biz_flow_instance_biz_active`**（AGENTS.md 约定 11：不许留死单）；
//! 3. **自审批真的被拦**：发起人本人 403、请假员工本人（HR 代提场景）403、无关审批人放行；
//! 4. **实例终止**：`cancel_approval_of` 把在跑实例推成 `CANCELLED` 且待办不再挂着
//!    （`leave_handler::cancel_leave` 走的就是这条，撤回后能重提）。
//!
//! 另加一条**陈旧回调**用例：撤回后重提（新实例在跑）时，旧实例的迟到结论不许改单据。
//!
//! 不设库地址时全部 SKIP（不假绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr27_drill \
//!   cargo test --lib business::hr::leave_bridge_db_tests -- --test-threads=1 --nocapture
//! ```
//!
//! # 夹具纪律（HR-10 / HR-19 / HR-24 / HR-25 的教训）
//!
//! - 员工走真实 `employee_handler::create_employee`（请假单的 `employee_id` 是 `RESTRICT` 外键）；
//! - 清场**按 id 删**（绝不按前缀扫全库），FK 顺序：审批待办 → 实例节点 → 实例 →
//!   请假单 → 状态日志 → 员工 → 用户 → 部门 → 本用例自己建的流程定义；
//! - 审批流程用**本文件内建的已发布流程**（单节点 = 夹具审批人），
//!   正式种子（V0393 的 `INITIATOR_SUPERIOR` + 人事经理两段式）属 HR-26，本卡只消费。

use nik::prelude::*;

#[cfg(test)]
mod db {
    use super::*;
    use crate::business::flow::engine;
    use crate::business::hr::employee_handler;
    use crate::business::hr::leave_bridge::{self, BIZ_TYPE};
    use crate::business::hr::leave_dto::{self, LeaveCancelReq, LeaveCreateReq, LeaveSubmitReq};
    use crate::business::hr::leave_handler;
    use crate::business::shared::state_machine::Operator;
    use crate::shared::error_code;
    use crate::shared::test_helpers;
    use nik::prelude::rust_decimal::Decimal;
    use nik::uuid::Uuid;
    use tokio::sync::{Mutex, OnceCell};

    /// 本文件内建流程定义上的标记（清场时按它删，绝不误删别的用例 / 正式种子的流程）。
    const FIXTURE_NOTE: &str = "HR-27 回调用例内建，不进迁移";

    /// 同一进程里的真库用例串行跑（共用一套 `biz_seq` / 流程定义表）。
    static DB_LOCK: Mutex<()> = Mutex::const_new(());

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

    /// 夹具：一个部门 + 一名员工（带登录账号） + 一名纯审批人（只有账号，不是员工）。
    struct Fixture {
        dept_code: String,
        emp: Uuid,
        emp_name: String,
        emp_username: String,
        emp_user: Uuid,
        approver: Uuid,
        approver_username: String,
        approver_name: String,
    }

    async fn fresh_user(tag: &str, dept: Option<Uuid>) -> (Uuid, String, String) {
        let suffix = uniq();
        let username = format!("hr27.{tag}.{suffix}");
        let name = format!("HR27 {tag}{suffix}");
        let user = test_helpers::ensure_user(&username, &name, dept).await;
        (user, username, name)
    }

    async fn fixture(tag: &str) -> Fixture {
        let dept_code = format!("HR27-{tag}-{}", uniq());
        let dept = test_helpers::ensure_dept(&dept_code, &format!("HR27 部门 {tag}")).await;
        let (emp_user, emp_username, emp_name) = fresh_user(&format!("{tag}.emp"), Some(dept)).await;
        let emp_no = format!("H27{:012x}", uniq() % 0xffff_ffff_ffff);
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
        let (approver, approver_username, approver_name) = fresh_user(&format!("{tag}.appr"), Some(dept)).await;
        Fixture {
            dept_code,
            emp,
            emp_name,
            emp_username,
            emp_user,
            approver,
            approver_username,
            approver_name,
        }
    }

    impl Fixture {
        fn operator(&self) -> Operator {
            Operator::named(Some(self.emp_user), self.emp_name.clone())
        }

        fn approver_operator(&self, id: Uuid) -> Operator {
            Operator::named(Some(id), self.approver_name.clone())
        }

        fn create_req(&self, reason: &str) -> LeaveCreateReq {
            LeaveCreateReq {
                employee_id: Some(self.emp.to_string()),
                leave_type: Some("ANNUAL".to_string()),
                start_time: Some("2026-11-10 09:00".to_string()),
                end_time: Some("2026-11-11 18:00".to_string()),
                duration_days: Some(Decimal::new(20, 1)),
                paid: Some(1),
                reason: Some(reason.to_string()),
                remark: Some("HR27 夹具".to_string()),
            }
        }

        /// 走真 handler 建单。
        async fn make_leave(&self, reason: &str) -> Uuid {
            let created = leave_handler::create_leave(self.operator(), &self.create_req(reason))
                .await
                .expect("建请假单");
            Uuid::parse_str(&created.id).expect("请假单 id")
        }

        /// 走真 handler 提交（发起人 = `initiator`，可为代提）。
        async fn submit(&self, leave_id: Uuid, initiator: Operator) -> leave_dto::LeaveRes {
            leave_handler::submit_leave(initiator, leave_id, &LeaveSubmitReq::default())
                .await
                .expect("提交审批")
        }

        /// 该单的库内状态：`(status, approval_instance_id)`。
        async fn state(&self, leave_id: Uuid) -> (String, Option<Uuid>) {
            let row = db()
                .query_first_sql(
                    "SELECT status, approval_instance_id FROM biz_leave_request WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&leave_id)],
                )
                .await
                .expect("读请假单状态")
                .expect("请假单必有行");
            (
                row.get_typed("status").expect("status"),
                row.get_typed("approval_instance_id").expect("approval_instance_id"),
            )
        }

        /// 状态日志（`(from, to, reason)`，按写入顺序）。
        async fn logs(&self, leave_id: Uuid) -> Vec<(Option<String>, String, Option<String>)> {
            let rows = db()
                .find_with_params(
                    "SELECT from_status, to_status, reason FROM biz_status_log \
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
                        row.get_typed("reason").expect("reason"),
                    )
                })
                .collect()
        }

        /// 该单最新一张实例：`(id, status)`。
        async fn instance_of(&self, leave_id: Uuid) -> Option<(Uuid, String)> {
            let row = db()
                .query_first_sql(
                    "SELECT id, status FROM biz_flow_instance \
                      WHERE biz_type = ? AND biz_id = ?::uuid AND deleted_at IS NULL \
                      ORDER BY started_at DESC LIMIT 1"
                        .to_string(),
                    vec![Value::Text(leave_bridge::BIZ_TYPE.to_string()), Value::from(&leave_id)],
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

        /// 该实例上还在跑的待办：`(task_id, assignee_id)`。
        async fn pending_tasks(&self, instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
            let rows = db()
                .find_with_params(
                    "SELECT id, assignee_id FROM biz_flow_task \
                      WHERE instance_id = ?::uuid AND status = 'PENDING' \
                      ORDER BY created_at, id",
                    vec![Value::from(&instance_id)],
                )
                .await
                .expect("读待办");
            rows.iter()
                .map(|row| {
                    (
                        row.get_typed("id").expect("task id"),
                        row.get_typed("assignee_id").expect("assignee"),
                    )
                })
                .collect()
        }

        /// 清场（按 id，FK 顺序；流程定义只删本文件内建的）。
        async fn clean(&self, leave_ids: &[Uuid]) {
            for leave_id in leave_ids {
                for sql in [
                    "DELETE FROM biz_flow_task WHERE instance_id IN \
                       (SELECT id FROM biz_flow_instance WHERE biz_id = ?::uuid)",
                    "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
                       (SELECT id FROM biz_flow_instance WHERE biz_id = ?::uuid)",
                    "DELETE FROM biz_flow_instance WHERE biz_id = ?::uuid",
                    "DELETE FROM biz_status_log WHERE entity_type = 'LEAVE_REQUEST' AND entity_id = ?::uuid",
                    "DELETE FROM biz_leave_request WHERE id = ?::uuid",
                ] {
                    let _ = db().exec_sql(sql.to_string(), vec![Value::from(leave_id)]).await;
                }
            }
            // 本文件内建、且已经没人引用的流程定义（按 version 的 change_note 认领，绝不误删
            // 正式种子 V0393 或别的用例的流程）
            let defs = db()
                .find_with_params(
                    "SELECT d.id FROM biz_flow_def d \
                      WHERE d.flow_code = ? AND d.deleted_at IS NULL \
                        AND EXISTS (SELECT 1 FROM biz_flow_def_version v \
                                     WHERE v.def_id = d.id AND v.change_note = ?) \
                        AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i WHERE i.def_id = d.id)",
                    vec![
                        Value::Text(leave_bridge::FLOW_CODE.to_string()),
                        Value::Text(FIXTURE_NOTE.to_string()),
                    ],
                )
                .await
                .unwrap_or_default();
            for row in &defs {
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
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_employee WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.emp)],
                )
                .await;
            test_helpers::purge_user(&self.emp_username).await;
            test_helpers::purge_user(&self.approver_username).await;
            test_helpers::purge_dept(&self.dept_code).await;
        }
    }

    // ==================== 审批流程夹具（本文件内建；正式种子属 HR-26） ====================

    /// 为本次用例重建一条已发布的 `hr_leave` 流程：`START → 审批人(USER) → END`。
    ///
    /// 只写 `biz_flow_def` + `biz_flow_def_version`（运行时引擎只读版本的 `graph_snapshot`，
    /// 见 `flow::engine::load_snapshot`），所以不需要 `biz_flow_node` / `biz_flow_edge` 行。
    /// 审批人写死成本用例的夹具账号 —— 否则「待办派给谁」取决于部门 leader 的现状，
    /// 用例会随环境漂移。
    async fn seed_leave_flow(approver: Uuid) {
        // 先清掉**所有**没人引用的 `hr_leave` 定义（`uk_biz_flow_def_code` 是 flow_code 全局唯一，
        // 同一时刻只能有一条定义 —— V0393 的正式种子也一样）。有实例引用的定义删不掉（FK 会拒），
        // 那种情况说明上一个用例的清场没跑完，本轮直接复用会带着别人的审批人，故让插入报错暴露问题。
        if let Ok(rows) = db()
            .find_with_params(
                "SELECT d.id FROM biz_flow_def d \
                  WHERE d.flow_code = ? AND d.deleted_at IS NULL \
                    AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i WHERE i.def_id = d.id)",
                vec![Value::Text(leave_bridge::FLOW_CODE.to_string())],
            )
            .await
        {
            for row in &rows {
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
             VALUES (?::uuid, ?, '请假审批（HR27 夹具）', ?, 'HR', 'HR-27 真库用例内建', \
                     'PUBLISHED', ?::uuid, 1, TRUE, FALSE, now(), now())"
                .to_string(),
            vec![
                Value::from(&def_id),
                Value::Text(leave_bridge::FLOW_CODE.to_string()),
                Value::Text(BIZ_TYPE.to_string()),
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
                     ?, now(), now(), now())"
                .to_string(),
            vec![
                Value::from(&version_id),
                Value::from(&def_id),
                Value::Text(snapshot.to_string()),
                Value::Text(FIXTURE_NOTE.to_string()),
            ],
        )
        .await
        .expect("插流程版本");
    }

    // ==================== 引擎调用（与 flow::handler 同顺序：先引擎，后业务桥） ====================

    async fn approve_one(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
        db().transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("同意")).await })
            .await
            .unwrap_or_else(|err| panic!("审批同意失败：{err}"))
    }

    async fn reject_final(task_id: Uuid, operator: Uuid, comment: &str) -> engine::ActionOut {
        db().transaction(
            |tx| async move { engine::reject(&tx, task_id, operator, comment, engine::ReturnTo::Final).await },
        )
        .await
        .unwrap_or_else(|err| panic!("审批驳回失败：{err}"))
    }

    // ==================== ① 审批通过 → SUBMITTED→APPROVED（本卡验收 1/3） ====================

    /// 通过：状态回写 `APPROVED` + 状态日志一条 + 实例引用保留；重复回调幂等；不动考勤。
    #[tokio::test]
    async fn approve_writes_back_and_keeps_trace() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let fx = fixture("approve").await;
        seed_leave_flow(fx.approver).await;
        let leave_id = fx.make_leave("HR27 通过用例").await;
        let submitted = fx.submit(leave_id, fx.operator()).await;
        assert_eq!(submitted.status, leave_dto::STATUS_SUBMITTED);

        let (instance, status) = fx.instance_of(leave_id).await.expect("提交后必须有实例");
        assert_eq!(status, "PENDING");
        let tasks = fx.pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1, "单节点单审批人 ⇒ 一条待办：{tasks:?}");
        assert_eq!(tasks[0].1, fx.approver, "待办必须落在夹具审批人");

        // 与 flow::handler::approve_task 同顺序：引擎事务提交 → 业务桥回调
        let out = approve_one(tasks[0].0, fx.approver).await;
        assert_eq!(out.instance_status, "APPROVED");
        let touched = leave_bridge::on_leave_settled(out.instance_id, &out.instance_status).await;
        assert_eq!(touched, Some(leave_id), "审批通过必须回写请假单");

        let (status, inst_ref) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_APPROVED);
        assert_eq!(inst_ref, Some(instance), "通过后实例引用保留（审批轨迹要能回看）");
        let logs = fx.logs(leave_id).await;
        let passed = logs
            .iter()
            .find(|(from, to, _)| from.as_deref() == Some("SUBMITTED") && to == "APPROVED")
            .expect("必须有一条 SUBMITTED→APPROVED 的状态日志");
        assert!(
            passed.2.as_deref().unwrap_or_default().contains("请假审批通过"),
            "状态日志要写人话原因：{:?}",
            passed.2
        );

        // 幂等：重复回调不重复写日志、不炸
        assert_eq!(
            leave_bridge::on_leave_settled(out.instance_id, "APPROVED").await,
            None,
            "重复回调必须静默跳过"
        );
        assert_eq!(fx.logs(leave_id).await.len(), logs.len(), "重复回调不许再写状态日志");

        // 设计 §4.3：请假审批通过**不改考勤**（考勤由导入 / 人工体现）
        let row = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_attendance_record WHERE employee_id = ?::uuid".to_string(),
                vec![Value::from(&fx.emp)],
            )
            .await
            .expect("读考勤条数")
            .expect("count 必有行");
        assert_eq!(
            row.get_typed::<i64>("n").expect("n 列"),
            0,
            "请假审批通过不许顺手动考勤"
        );

        fx.clean(&[leave_id]).await;
    }

    // ==================== ② 驳回 → 回草稿且能重提（本卡验收 2/3） ====================

    /// 驳回：`SUBMITTED→DRAFT` + 意见进状态日志 + **能再提交一次**（不留死单）。
    #[tokio::test]
    async fn reject_backs_to_draft_and_allows_resubmit() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let fx = fixture("reject").await;
        seed_leave_flow(fx.approver).await;
        let leave_id = fx.make_leave("HR27 驳回用例").await;
        fx.submit(leave_id, fx.operator()).await;
        let (instance, _) = fx.instance_of(leave_id).await.expect("提交后必须有实例");
        let tasks = fx.pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1);

        let comment = "驳回：材料不全，请补齐诊断证明后重提";
        let out = reject_final(tasks[0].0, fx.approver, comment).await;
        assert_eq!(out.instance_status, "REJECTED");
        let touched = leave_bridge::on_leave_settled(out.instance_id, &out.instance_status).await;
        assert_eq!(touched, Some(leave_id), "驳回必须回写请假单");

        let (status, _) = fx.state(leave_id).await;
        assert_eq!(status, leave_dto::STATUS_DRAFT, "驳回必须打回草稿（可改可重提）");
        let logs = fx.logs(leave_id).await;
        let back = logs
            .iter()
            .find(|(from, to, _)| from.as_deref() == Some("SUBMITTED") && to == "DRAFT")
            .expect("必须有一条 SUBMITTED→DRAFT 的状态日志");
        let reason = back.2.as_deref().unwrap_or_default();
        assert!(reason.contains("审批驳回"), "状态日志要点明是审批驳回：{reason}");
        assert!(reason.contains("诊断证明"), "驳回意见必须落到状态日志：{reason}");

        // 不留死单：驳回后能改、能重提（新实例不同于旧实例，不撞 uk_biz_flow_instance_biz_active）
        leave_handler::update_leave(
            fx.operator(),
            leave_id,
            &leave_dto::LeaveUpdateReq {
                reason: Some("补齐诊断证明后重提".to_string()),
                ..leave_dto::LeaveUpdateReq::default()
            },
        )
        .await
        .expect("驳回后草稿必须可改");
        let resubmitted = fx.submit(leave_id, fx.operator()).await;
        assert_eq!(resubmitted.status, leave_dto::STATUS_SUBMITTED);
        let new_instance =
            Uuid::parse_str(resubmitted.approval_instance_id.as_deref().expect("重提后必须挂新实例")).expect("实例 id");
        assert_ne!(new_instance, instance, "重提必须新建实例（旧实例已 REJECTED）");
        let (latest, latest_status) = fx.instance_of(leave_id).await.expect("重提后必须有实例");
        assert_eq!(latest, new_instance);
        assert_eq!(latest_status, "PENDING");
        // 旧实例还留着（审计轨迹），状态是 REJECTED
        let old = db()
            .query_first_sql(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
                vec![Value::from(&instance)],
            )
            .await
            .expect("读旧实例")
            .expect("旧实例必须还在（审计）");
        assert_eq!(old.get_typed::<String>("status").expect("status"), "REJECTED");

        // 旧实例的**迟到回调**不许改单据（新实例在跑，单据是 SUBMITTED → 但挂的是新实例）
        assert_eq!(
            leave_bridge::on_leave_settled(Some(instance), "REJECTED").await,
            None,
            "旧实例的结论必须被陈旧回调闸挡掉"
        );
        let (still, still_ref) = fx.state(leave_id).await;
        assert_eq!(still, leave_dto::STATUS_SUBMITTED, "陈旧回调不许把在跑的单打回草稿");
        assert_eq!(still_ref, Some(new_instance));

        fx.clean(&[leave_id]).await;
    }

    // ==================== ③ 自审批被拦（本卡验收 3/3） ====================

    /// 自审批：发起人本人 403、请假员工本人 403（HR 代提）、无关审批人放行、无关待办零影响。
    #[tokio::test]
    async fn guard_blocks_self_approval() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let fx = fixture("selfappr").await;
        seed_leave_flow(fx.approver).await;

        // 情形 A：员工自己提交的假 → 他自己不能批
        let leave_a = fx.make_leave("HR27 自审批 A").await;
        fx.submit(leave_a, fx.operator()).await;
        let (instance_a, _) = fx.instance_of(leave_a).await.expect("实例 A");
        let tasks_a = fx.pending_tasks(instance_a).await;
        assert_eq!(tasks_a.len(), 1);
        let error = leave_bridge::guard_self_approval(tasks_a[0].0, fx.emp_user)
            .await
            .expect_err("员工不能批自己的请假单");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(error.msg.contains("发起人"), "报文要说明为什么拦：{}", error.msg);
        // 审批人本人放行
        leave_bridge::guard_self_approval(tasks_a[0].0, fx.approver)
            .await
            .expect("无关审批人必须放行");

        // 情形 B：HR 代提（发起人 = 审批人账号），请假员工本人仍然不能批自己的单
        let leave_b = fx.make_leave("HR27 自审批 B").await;
        fx.submit(leave_b, fx.approver_operator(fx.approver)).await;
        let (instance_b, _) = fx.instance_of(leave_b).await.expect("实例 B");
        let tasks_b = fx.pending_tasks(instance_b).await;
        assert_eq!(tasks_b.len(), 1);
        let error = leave_bridge::guard_self_approval(tasks_b[0].0, fx.emp_user)
            .await
            .expect_err("员工不能批自己的请假单（代提场景）");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(error.msg.contains("自己"), "报文要指向「员工本人」：{}", error.msg);

        // 情形 C：与本业务无关的待办零影响（待办不存在 → 放行，交给后续流程报 404）
        leave_bridge::guard_self_approval(Uuid::now_v7(), fx.emp_user)
            .await
            .expect("无关待办不许被本桥拦住");

        fx.clean(&[leave_a, leave_b]).await;
    }

    // ==================== ④ 实例终止（撤回 / 作废） ====================

    /// `cancel_approval_of` 把在跑实例推成 `CANCELLED` 且待办不再挂着；
    /// `leave_handler::cancel_leave` 的撤回路径走的就是它（HR-25 行为不回归）。
    #[tokio::test]
    async fn cancel_approval_of_terminates_running_instance() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let fx = fixture("terminate").await;
        seed_leave_flow(fx.approver).await;

        // ① 直接调桥：在跑实例 → CANCELLED，待办被收尾
        let leave_a = fx.make_leave("HR27 终止 A").await;
        fx.submit(leave_a, fx.operator()).await;
        let (instance_a, _) = fx.instance_of(leave_a).await.expect("实例 A");
        let row = crate::business::hr::leave_model::find_in(db(), leave_a)
            .await
            .expect("读请假单")
            .expect("请假单必有行");
        let tx = db().begin().await.expect("开启事务");
        let terminated = leave_bridge::cancel_approval_of(&tx, &row).await.expect("终止实例");
        tx.commit().await.expect("提交事务");
        assert_eq!(terminated.as_deref(), Some("CANCELLED"));
        let (_, status_a) = fx.instance_of(leave_a).await.expect("实例 A 仍在");
        assert_eq!(status_a, "CANCELLED", "实例必须被终止（不是删掉，轨迹要留）");
        assert!(fx.pending_tasks(instance_a).await.is_empty(), "终止后不许留下孤儿待办");

        // ② 走真 handler 的撤回：状态回草稿 + 实例已终止 + 可以重提
        let leave_b = fx.make_leave("HR27 终止 B").await;
        fx.submit(leave_b, fx.operator()).await;
        let (instance_b, _) = fx.instance_of(leave_b).await.expect("实例 B");
        let withdrawn = leave_handler::cancel_leave(
            fx.operator(),
            leave_b,
            &LeaveCancelReq {
                reason: Some("填错了，重新提".to_string()),
            },
        )
        .await
        .expect("撤回");
        assert_eq!(withdrawn.action, "WITHDRAWN");
        assert_eq!(withdrawn.leave.status, leave_dto::STATUS_DRAFT);
        assert_eq!(withdrawn.instance_status.as_deref(), Some("CANCELLED"));
        let (_, status_b) = fx.instance_of(leave_b).await.expect("实例 B 仍在");
        assert_eq!(status_b, "CANCELLED");
        // 撤回后能重提（不撞 PENDING 部分唯一索引）
        let resubmitted = fx.submit(leave_b, fx.operator()).await;
        assert_eq!(resubmitted.status, leave_dto::STATUS_SUBMITTED);
        let reborn =
            Uuid::parse_str(resubmitted.approval_instance_id.as_deref().expect("重提后必须挂新实例")).expect("实例 id");
        assert_ne!(reborn, instance_b);

        // 撤回（终止）后旧实例的迟到回调：单据已挂新实例 → 忽略
        assert_eq!(
            leave_bridge::on_leave_settled(Some(instance_b), "WITHDRAWN").await,
            None,
            "旧实例的迟到回调必须被挡掉"
        );
        assert_eq!(fx.state(leave_b).await.0, leave_dto::STATUS_SUBMITTED);

        fx.clean(&[leave_a, leave_b]).await;
    }
}
