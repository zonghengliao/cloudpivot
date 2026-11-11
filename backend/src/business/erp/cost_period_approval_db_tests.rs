//! 成本结算审批接线的真库用例（`ERP_DB_URL` 开关）—— B3-3「提交 / 撤回 / 驳回」。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/costb31_drill \
//!   cargo test --lib cost_period_approval -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面四条逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`submit_starts_instance_and_freezes_batch`] | **提交 → 引擎启动实例**：批次 `DRAFT→SUBMITTED` + `approval_instance_id` 落库 + 实例 `PENDING` / `biz_type=COST_SETTLE` / `biz_id=批次` + 待办落到财务经理 + 状态日志 |
//! | 2 | [`reject_returns_batch_to_draft_and_clears_traces`] | **驳回 → 回 `DRAFT` 且清空审批痕迹**：`approval_instance_id` 变 `NULL`、`remark` 追加驳回意见、实例 `REJECTED`、状态日志 `SUBMITTED→DRAFT`、**回草稿后能再提交** |
//! | 3 | [`withdraw_terminates_instance_and_returns_draft`] | **撤回 → 实例终止**：实例 `WITHDRAWN`、批次回 `DRAFT`、`approval_instance_id` 清空、没有残留 `PENDING` 待办 |
//! | 4 | [`two_levels_then_approved_freezes_batch`] | 通过路径（回调的 Appvore 分支）：财务经理 → 财务总监两级，批完 `APPROVED` + 实例 id **保留**作审计 |
//! | 5 | [`submit_requires_draft_and_published_flow`] | 护栏：非草稿再提交 409（`66073`）、没有在跑审批时才起新实例 |
//!
//! # 夹具纪律
//!
//! 期间固定 **2031-07 / 2031-08 / 2031-09**（B3-1 用了 2031-01…06，本模块接着往下用）。
//! 账号用 `b3a3` 前缀 + **固定 UUID**，角色借用共享种子 `FINANCE_MANAGER (…f0005)` /
//! `FINANCE_DIRECTOR (…f0006)`，**整模块串行**（[`DB_LOCK`] = `fixture_hygiene::SEED_ROLE_LOCK`，
//! 与 `contract_approval_tests` 同一把锁：同一批 `sys_user_role` 行 = 同一把锁）。
//! 清场只按 id / 期间删，不按前缀扫全库；审批实例 / 待办 / 状态日志按 FK 顺序删。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_period_bridge;
    use crate::business::erp::cost_period_dto::CostPeriodCreateReq;
    use crate::business::erp::cost_period_handler as handler;
    use crate::business::erp::cost_period_model as model;
    use crate::business::flow::engine;
    use tokio::sync::OnceCell;

    /// 共享角色（`V0182` / `V0379` 种子的固定 UUID）。
    const ROLE_FM: &str = "20000000-0000-4000-8000-0000000f0005";
    const ROLE_FD: &str = "20000000-0000-4000-8000-0000000f0006";

    /// 制单人（建批次 + 提交的人；不是审批人）。
    const U_MAKER: &str = "b3a30001-0000-4000-8000-000000000001";
    /// 财务经理（第一级审批）。
    const U_FM: &str = "b3a30002-0000-4000-8000-000000000002";
    /// 财务总监（第二级审批）。
    const U_FD: &str = "b3a30003-0000-4000-8000-000000000003";

    /// 真库用例必须串行：共用全局 `db()`、同一批 `biz_seq`、同一批 `sys_user_role` 行。
    use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;

    fn uid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
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
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 夹具账号 + 角色绑定（幂等 upsert）。`must_change_password=false` 避免撞登录后的改密门。
    async fn ensure_fixtures() {
        // SCOPE-EXEMPT: 用例夹具按固定 UUID 写 sys_user / sys_user_role。
        for (id, name, role) in [
            (U_MAKER, "b3a3-maker", None),
            (U_FM, "b3a3-finance-manager", Some(ROLE_FM)),
            (U_FD, "b3a3-finance-director", Some(ROLE_FD)),
        ] {
            db().exec_sql(
                "INSERT INTO sys_user (id, username, password, real_name, status, must_change_password) \
                 VALUES (?, ?, 'x', ?, 1, false) \
                 ON CONFLICT (id) DO UPDATE SET real_name = EXCLUDED.real_name, status = 1, deleted_at = NULL"
                    .to_string(),
                vec![
                    Value::from(&uid(id)),
                    Value::Text(name.to_string()),
                    Value::Text(name.to_string()),
                ],
            )
            .await
            .expect("插夹具账号");
            if let Some(role) = role {
                db().exec_sql(
                    "INSERT INTO sys_user_role (id, user_id, role_id) VALUES (gen_random_uuid(), ?, ?) \
                     ON CONFLICT (user_id, role_id) DO NOTHING"
                        .to_string(),
                    vec![Value::from(&uid(id)), Value::from(&uid(role))],
                )
                .await
                .expect("绑夹具角色");
            }
        }
    }

    /// 清场：该期间的批次 + 审批实例 / 待办 / 抄送 / 节点 + 状态日志（FK 顺序）。
    async fn purge(period_code: &str) {
        let period_filter = "SELECT id FROM biz_cost_period WHERE period_code = ?";
        for sql in [
            format!(
                "DELETE FROM biz_flow_cc WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_SETTLE' AND biz_id IN ({period_filter}))"
            ),
            format!(
                "DELETE FROM biz_flow_task WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_SETTLE' AND biz_id IN ({period_filter}))"
            ),
            format!(
                "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_SETTLE' AND biz_id IN ({period_filter}))"
            ),
            format!("DELETE FROM biz_flow_instance WHERE biz_type = 'COST_SETTLE' AND biz_id IN ({period_filter})"),
            format!("DELETE FROM biz_status_log WHERE entity_type = 'COST_PERIOD' AND entity_id IN ({period_filter})"),
            "DELETE FROM biz_cost_period WHERE period_code = ?".to_string(),
        ] {
            db().exec_sql(sql, vec![Value::Text(period_code.to_string())])
                .await
                .expect("清场失败");
        }
    }

    /// 清场：夹具账号 + 角色绑定（必须在实例 / 待办之后删，它们有 FK 指 sys_user）。
    async fn purge_fixtures() {
        for id in [U_MAKER, U_FM, U_FD] {
            db().exec_sql(
                "DELETE FROM sys_user_role WHERE user_id = ?".to_string(),
                vec![Value::from(&uid(id))],
            )
            .await
            .expect("清夹具角色绑定");
            db().exec_sql(
                "DELETE FROM sys_user WHERE id = ?".to_string(),
                vec![Value::from(&uid(id))],
            )
            .await
            .expect("清夹具账号");
        }
    }

    /// 拿锁 + 连库 + 建夹具；返回 `None` 表示没配库（用例直接 `return`）。
    async fn setup(period_code: &str) -> Option<tokio::sync::MutexGuard<'static, ()>> {
        if !ensure_db().await {
            return None;
        }
        let guard = DB_LOCK.lock().await;
        purge(period_code).await;
        ensure_fixtures().await;
        Some(guard)
    }

    fn create_req(period_code: &str) -> CostPeriodCreateReq {
        CostPeriodCreateReq {
            period_code: Some(period_code.to_string()),
            labor_source: Some("MANUAL".to_string()),
            remark: Some("B3-3 审批接线用例".to_string()),
        }
    }

    /// 建一个 `DRAFT` 批次（走真 handler，不手拼 SQL）。
    async fn make_batch(period_code: &str) -> Uuid {
        let detail = handler::create_draft(Some(uid(U_MAKER)), create_req(period_code))
            .await
            .expect("建批次应成功");
        Uuid::parse_str(&detail.id).expect("批次 id")
    }

    /// 批次当前状态（读库，不走 DTO）。
    async fn status_of(id: Uuid) -> String {
        model::find_by_id(id)
            .await
            .expect("查批次失败")
            .expect("批次应存在")
            .status
    }

    /// 批次挂在单据上的审批实例 id（读库）。
    async fn instance_of(id: Uuid) -> Option<Uuid> {
        model::find_by_id(id)
            .await
            .expect("查批次失败")
            .expect("批次应存在")
            .approval_instance_id
    }

    /// 批次的 `remark`。
    async fn remark_of(id: Uuid) -> Option<String> {
        model::find_by_id(id)
            .await
            .expect("查批次失败")
            .expect("批次应存在")
            .remark
    }

    /// 实例状态（读库）。
    async fn instance_status(instance_id: Uuid) -> Option<String> {
        db().scalar_with_params::<Option<String>>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例状态")
    }

    /// 实例下的待办（`(任务 id, 审批人)`，只取 `PENDING`）。
    async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
        let rows = db()
            .query_sql(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' \
                  ORDER BY created_at ASC, id ASC"
                    .to_string(),
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查待办");
        rows.iter()
            .map(|row| Ok((row.get_typed::<Uuid>("id")?, row.get_typed::<Uuid>("assignee_id")?)))
            .collect::<DbResult<Vec<_>>>()
            .expect("解析待办")
    }

    /// 状态日志（`(from, to, reason)`，按写入顺序）。
    async fn logs_of(id: Uuid) -> Vec<(Option<String>, String, Option<String>)> {
        let rows = db()
            .query_sql(
                "SELECT from_status, to_status, reason FROM biz_status_log \
                  WHERE entity_type = 'COST_PERIOD' AND entity_id = ?::uuid \
                  ORDER BY created_at ASC, id ASC"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .expect("读状态日志");
        rows.iter()
            .map(|row| {
                Ok((
                    row.get_typed::<Option<String>>("from_status")?,
                    row.get_typed::<String>("to_status")?,
                    row.get_typed::<Option<String>>("reason")?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()
            .expect("解析状态日志")
    }

    /// 审批同意一票（事务内，与 `flow::handler::approve_task` 同一条引擎路径）。
    async fn approve(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
        db().transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("B3-3 用例同意")).await })
            .await
            .unwrap_or_else(|err| panic!("审批同意失败：{err}"))
    }

    /// 驳回并终止（`ReturnTo::Final` → 实例 `REJECTED`）。
    async fn reject_final(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
        db().transaction(|tx| async move {
            engine::reject(
                &tx,
                task_id,
                operator,
                "驳回：B3-3 用例（分摊基数与车间实际不符，请复核后重提）",
                engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .unwrap_or_else(|err| panic!("审批驳回失败：{err}"))
    }

    // ==================== 用例 ====================

    /// ① 提交 → 引擎启动实例（卡面第一条验收）。
    #[tokio::test]
    async fn submit_starts_instance_and_freezes_batch() {
        const PERIOD: &str = "2031-07";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        assert_eq!(status_of(id).await, "DRAFT");

        let detail = handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("提交应成功");
        assert_eq!(detail.status, "SUBMITTED", "提交后批次必须是审批中");
        let instance = instance_of(id).await.expect("必须挂上审批实例");
        assert_eq!(
            detail.approval_instance_id,
            Some(instance.to_string()),
            "详情必须下发实例 id"
        );
        assert_eq!(instance_status(instance).await.as_deref(), Some("PENDING"));

        // 实例的业务挂载：biz_type / biz_id / biz_no 三者缺一，回调就认不到批次
        let row = db()
            .query_first_sql(
                "SELECT biz_type, biz_id, biz_no FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
                vec![Value::from(&instance)],
            )
            .await
            .expect("查实例")
            .expect("实例存在");
        let biz_type: String = row.get_typed("biz_type").expect("biz_type");
        let biz_id: Uuid = row.get_typed("biz_id").expect("biz_id");
        let biz_no: String = row.get_typed("biz_no").expect("biz_no");
        assert_eq!(biz_type, cost_period_bridge::BIZ_TYPE);
        assert_eq!(biz_id, id, "biz_id 必须是批次 id（回调靠它找单）");
        assert!(biz_no.starts_with("CS-"), "biz_no 应是批次号：{biz_no}");

        // 待办落到财务经理（两级流程的第一级），制单人不在里面
        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1, "第一级只该有一条待办：{tasks:?}");
        assert_eq!(tasks[0].1, uid(U_FM), "第一级待办必须落到财务经理");
        assert_ne!(tasks[0].1, uid(U_MAKER), "制单人不能拿到自己的待办（内控）");

        // 状态日志留痕（建单也会写一条 None→DRAFT，所以看**最后一条**）
        let logs = logs_of(id).await;
        let last = logs.last().expect("提交必须写状态日志");
        assert_eq!(last.0.as_deref(), Some("DRAFT"));
        assert_eq!(last.1, "SUBMITTED");

        purge(PERIOD).await;
        purge_fixtures().await;
    }

    /// ② 驳回 → 回 `DRAFT` 且清空审批痕迹（卡面第二条验收）。
    #[tokio::test]
    async fn reject_returns_batch_to_draft_and_clears_traces() {
        const PERIOD: &str = "2031-08";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("提交应成功");
        let instance = instance_of(id).await.expect("提交后必须有实例");

        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1);
        let out = reject_final(tasks[0].0, uid(U_FM)).await;
        assert_eq!(out.instance_status, "REJECTED", "驳回终止后实例是 REJECTED");
        // 审批事务提交之后才跑业务回调（与 flow::handler::reject_task 同顺序）
        cost_period_bridge::on_cost_settle_decided(out.instance_id, &out.instance_status).await;

        assert_eq!(status_of(id).await, "DRAFT", "驳回必须打回草稿");
        assert_eq!(
            instance_of(id).await,
            None,
            "驳回后必须清空审批痕迹（approval_instance_id = NULL）"
        );
        let remark = remark_of(id).await.unwrap_or_default();
        assert!(
            remark.contains("审批驳回") && remark.contains("分摊基数"),
            "驳回意见必须追加进 remark：{remark}"
        );
        assert!(remark.contains("B3-3 审批接线用例"), "原有备注不许被覆盖：{remark}");
        let logs = logs_of(id).await;
        let back = logs
            .iter()
            .find(|(from, to, _)| from.as_deref() == Some("SUBMITTED") && to == "DRAFT")
            .expect("必须有一条 SUBMITTED→DRAFT 的状态日志");
        assert_eq!(back.0.as_deref(), Some("SUBMITTED"));
        assert!(
            back.2.as_deref().is_some_and(|text| text.contains("驳回")),
            "状态日志要写明驳回原因：{back:?}"
        );

        // 回草稿后能直接再提交（实例已清空 ⇒ 不撞 uk_biz_flow_instance_biz_active）
        let again = handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("回草稿后应能重新提交");
        assert_eq!(again.status, "SUBMITTED");

        purge(PERIOD).await;
        purge_fixtures().await;
    }

    /// ③ 撤回 → 实例终止 + 批次回草稿（卡面第三条验收）。
    #[tokio::test]
    async fn withdraw_terminates_instance_and_returns_draft() {
        const PERIOD: &str = "2031-09";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("提交应成功");
        let instance = instance_of(id).await.expect("提交后必须有实例");
        assert!(!pending_tasks(instance).await.is_empty(), "撤回前应有待办");

        let detail = handler::withdraw_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("撤回应成功");
        assert_eq!(detail.status, "DRAFT", "撤回后批次回草稿");
        assert_eq!(detail.approval_instance_id, None, "撤回后清空审批痕迹");
        assert_eq!(
            instance_status(instance).await.as_deref(),
            Some("WITHDRAWN"),
            "撤回必须终止在跑的实例（否则留下永远等不到回调的孤儿）"
        );
        assert!(
            pending_tasks(instance).await.is_empty(),
            "撤回后不许残留 PENDING 待办（审批人点进去会 409）"
        );
        let remark = remark_of(id).await.unwrap_or_default();
        assert!(remark.contains("审批撤回"), "撤回也要在 remark 留痕：{remark}");

        purge(PERIOD).await;
        purge_fixtures().await;
    }

    /// ④ 两级审批通过 → `APPROVED`（回调的 Approve 分支 + 实例 id 保留作审计）。
    #[tokio::test]
    async fn two_levels_then_approved_freezes_batch() {
        const PERIOD: &str = "2031-07";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("提交应成功");
        let instance = instance_of(id).await.expect("提交后必须有实例");

        // 第一级：财务经理
        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1);
        let out = approve(tasks[0].0, uid(U_FM)).await;
        assert_eq!(out.instance_status, "PENDING", "批完第一站实例还在跑");
        cost_period_bridge::on_cost_settle_decided(out.instance_id, &out.instance_status).await;
        assert_eq!(
            status_of(id).await,
            "SUBMITTED",
            "两级流程第一级通过后批次必须仍在审批中（中间态不许打回草稿）"
        );

        // 第二级：财务总监
        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1, "第二级应有一条待办");
        assert_eq!(tasks[0].1, uid(U_FD), "第二级必须落到财务总监");
        let out = approve(tasks[0].0, uid(U_FD)).await;
        assert_eq!(out.instance_status, "APPROVED");
        cost_period_bridge::on_cost_settle_decided(out.instance_id, &out.instance_status).await;

        assert_eq!(status_of(id).await, "APPROVED", "两级批完才是 APPROVED");
        assert_eq!(
            instance_of(id).await,
            Some(instance),
            "审批通过后实例 id 必须保留（审计：审批通过 FLW-xxx）"
        );

        purge(PERIOD).await;
        purge_fixtures().await;
    }

    /// ⑤ 护栏：非草稿不能提交 / 撤回（`66073`），不是审批中不能撤回。
    #[tokio::test]
    async fn submit_and_withdraw_guard_non_draft_states() {
        const PERIOD: &str = "2031-08";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("第一次提交应成功");

        // 已 SUBMITTED 再提交 → 409 + 66073
        let error = handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect_err("审批中不许重复提交");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_PERIOD_STATUS_GUARD);

        // 撤回之后（DRAFT）再撤 → 409 + 66073
        handler::withdraw_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("撤回应成功");
        let error = handler::withdraw_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect_err("草稿不许撤回");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_PERIOD_STATUS_GUARD);

        purge(PERIOD).await;
        purge_fixtures().await;
    }

    /// ⑥ 自愈：审批中心驳回后业务回调失败（批次停在 `SUBMITTED`）→ 点「撤回」按实例真实结论回写。
    ///
    /// 这正是约定 11 要防的死单：实例 `REJECTED` + 批次 `SUBMITTED`。
    /// 没有这条自愈，用户撤不回（`65024`）、提不了（不是 DRAFT），只能改库。
    #[tokio::test]
    async fn withdraw_self_heals_when_callback_failed() {
        const PERIOD: &str = "2031-09";
        let Some(_guard) = setup(PERIOD).await else { return };
        let id = make_batch(PERIOD).await;
        handler::submit_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("提交应成功");
        let instance = instance_of(id).await.expect("提交后必须有实例");

        // 审批中心驳回，但**故意不跑桥回调**（模拟回调失败）
        let tasks = pending_tasks(instance).await;
        let out = reject_final(tasks[0].0, uid(U_FM)).await;
        assert_eq!(out.instance_status, "REJECTED");
        assert_eq!(
            status_of(id).await,
            "SUBMITTED",
            "前提：回调没跑，批次停在审批中（死单现场）"
        );

        // 用户点「撤回」→ 自愈：按实例真实结论（REJECTED）回写草稿
        let detail = handler::withdraw_period(Some(uid(U_MAKER)), &id.to_string())
            .await
            .expect("实例已终态时撤回应走自愈分支");
        assert_eq!(detail.status, "DRAFT", "自愈必须把批次推到真实结论");
        assert_eq!(instance_of(id).await, None, "自愈同样要清空审批痕迹");
        assert_eq!(
            instance_status(instance).await.as_deref(),
            Some("REJECTED"),
            "自愈不碰实例状态（实例仍是驳回）"
        );

        purge(PERIOD).await;
        purge_fixtures().await;
    }
}
