//! 标准成本「发布（审核后生效）」的真库用例（`ERP_DB_URL` 开关）—— M5-10。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m510_pub \
//!   cargo test --lib cost_standard_publish -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`submit_starts_instance_but_card_is_not_live_yet`] | 提交 → `DRAFT→PENDING` + 实例 `PENDING`（`biz_type=COST_STANDARD_PUBLISH` / `biz_id=卡头`）+ 待办落到财务经理 + **审批中取不到标准**（审核后生效的负向证明） |
//! | 2 | [`approve_publishes_and_becomes_effective`] | 审批通过 → `PENDING→PUBLISHED` + `status=1` + `published_at/published_by` + 实例 id **保留**作审计 + `GET effective` 取到这一版 |
//! | 3 | [`reject_returns_to_draft_and_clears_instance`] | 驳回 → 回 `DRAFT` + 清实例指针 + 驳回意见进 `remark` + 实例 `REJECTED` + 仍取不到标准 |
//! | 4 | [`withdraw_terminates_and_returns_draft`] | 撤回 → 实例 `WITHDRAWN` + 卡回 `DRAFT` + 零残留待办（同事务，不留死单） |
//! | 5 | [`publish_guard_blocks_every_wrong_state`] | 状态闸：重复提交 409/66111、草稿撤回 409、审批中改/删/置生效 409（`66111`）、已发布卡取数正常 |
//! | 6 | [`internal_control_uses_create_by_as_maker`] | 内控闸的制单人来源 = `biz_cost_standard.create_by`（与「谁不能自审」同源） |
//!
//! # 夹具纪律
//!
//! · 物料用 `test_seed::ensure_material_kind`（编码带进程内唯一后缀），跑完只删本卡的标准成本行；
//! · 账号 `m510` 前缀 + 固定 UUID，角色借用共享种子 `FINANCE_MANAGER (…f0005)`；
//! · 整模块串行（[`DB_LOCK`] = `fixture_hygiene::SEED_ROLE_LOCK`，与其它动角色绑定的用例同一把锁）；
//! · 日期用「相对今天」，不写死。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_standard_dto::{
        self as dto, StandardCreateReq, StandardEffectiveQuery, StandardItemReq, StandardStatusReq, StandardUpdateReq,
    };
    use crate::business::erp::cost::cost_standard_handler as handler;
    use crate::business::erp::cost::cost_standard_model as model;
    use crate::business::erp::cost::cost_standard_publish as publish;
    use crate::business::flow::engine;
    use nik::prelude::chrono::{Duration, FixedOffset, NaiveDate, Utc};
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr as _;
    use tokio::sync::OnceCell;

    /// 夹具账号的用户名（`test_seed::ensure_user` 按它派生确定性 id；不手写 `sys_user` INSERT ——
    /// 主数据手写 INSERT 有预算闸，见 `docs/architecture/P2-62-数据准备脚本化.md`）。
    const USER_MAKER: &str = "m510.cost-maker";
    /// 财务经理（审批人）的用户名。
    const USER_FM: &str = "m510.finance-manager";
    /// 共享角色编码（`V0379` 种子的固定 role_code；审批节点按它解析成员）。
    const ROLE_FM_CODE: &str = "FINANCE_MANAGER";

    /// 真库用例必须串行：共用全局 `db()`、同一批 `sys_user_role` 行。
    use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;

    /// 制单人 id（首次调用建账号；后续复用同一行）。
    async fn maker() -> Uuid {
        static ID: OnceCell<Uuid> = OnceCell::const_new();
        *ID.get_or_init(|| async {
            crate::test_seed::ensure_user(USER_MAKER, "M510 标准成本制单人", None).await
        })
        .await
    }

    /// 财务经理 id（首次调用建账号 + 绑 `FINANCE_MANAGER`）。
    async fn fm() -> Uuid {
        static ID: OnceCell<Uuid> = OnceCell::const_new();
        *ID.get_or_init(|| async {
            let user = crate::test_seed::ensure_user(USER_FM, "M510 标准成本财务经理", None).await;
            crate::test_seed::bind_role(user, ROLE_FM_CODE).await;
            user
        })
        .await
    }

    /// 今日（CST）—— 与 handler 同口径。
    fn today() -> NaiveDate {
        Utc::now()
            .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
            .date_naive()
    }

    fn day(offset: i64) -> NaiveDate {
        today() + Duration::days(offset)
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
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 夹具账号 + 角色绑定（幂等；账号走 `test_seed::ensure_user`，不手写 `sys_user` INSERT）。
    async fn ensure_fixtures() {
        let _ = maker().await;
        let _ = fm().await;
    }

    /// 清场：本模块造的审批实例链（按 `biz_type` + 夹具发起人）+ 卡头 + 物料。
    async fn purge(materials: &[Uuid]) {
        for sql in [
            "DELETE FROM biz_flow_cc WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH' AND initiator_id = ?::uuid)"
                .to_string(),
            "DELETE FROM biz_flow_task WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH' AND initiator_id = ?::uuid)"
                .to_string(),
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH' AND initiator_id = ?::uuid)"
                .to_string(),
            "DELETE FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH' AND initiator_id = ?::uuid"
                .to_string(),
        ] {
            db().exec_sql(sql, vec![Value::from(&maker().await)])
                .await
                .expect("清审批实例链");
        }
        for material_id in materials {
            db().exec_sql(
                "DELETE FROM biz_cost_standard WHERE material_id = ?::uuid".to_string(),
                vec![Value::from(material_id)],
            )
            .await
            .expect("清标准成本行");
        }
    }

    /// 拿锁 + 连库 + 建夹具；`None` = 没配库，用例直接 `return`。
    ///
    /// ⚠️ 必须在**造物料之前**调用：`fixture_material` 会打库，库没初始化时 `db()` 直接 panic
    /// （M5-08 的用例也是这个顺序）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        if !ensure_db().await {
            return None;
        }
        let guard = DB_LOCK.lock().await;
        ensure_fixtures().await;
        purge(&[]).await;
        Some(guard)
    }

    async fn fixture_material(prefix: &str) -> Uuid {
        let suffix = crate::test_seed::uniq_suffix();
        crate::test_seed::ensure_material_kind(
            &format!("M510-{prefix}-{}", crate::test_seed::last_chars(&suffix, 10)),
            &format!("M5-10 夹具 {prefix} {suffix}"),
            "FINISHED",
        )
        .await
    }

    fn item(component: Uuid, qty: &str, price: &str) -> StandardItemReq {
        StandardItemReq {
            component_material_id: Some(component.to_string()),
            std_qty: Some(Decimal::from_str(qty).expect("数量")),
            std_price: Some(Decimal::from_str(price).expect("单价")),
        }
    }

    fn create_req(
        material: Uuid,
        from: NaiveDate,
        to: Option<NaiveDate>,
        items: Vec<StandardItemReq>,
    ) -> StandardCreateReq {
        StandardCreateReq {
            material_id: Some(material.to_string()),
            effective_from: Some(from.to_string()),
            effective_to: to.map(|date| date.to_string()),
            std_labor_hours: Some(Decimal::from_str("2.5").expect("工时")),
            std_labor_rate: Some(Decimal::from_str("31.111111").expect("费率")),
            std_overhead_cost: Some(Decimal::from_str("12.345678").expect("费用")),
            items: Some(items),
            status: None,
            remark: Some("M5-10 真库用例".to_string()),
            ..Default::default()
        }
    }

    /// 建一版草稿（走真 handler）。
    async fn make_draft(material: Uuid, component: Uuid, from: NaiveDate, to: Option<NaiveDate>) -> (Uuid, String) {
        let detail = handler::create_standard(
            Some(maker().await),
            create_req(material, from, to, vec![item(component, "2", "5.5")]),
        )
        .await
        .expect("建标准成本卡应成功");
        assert_eq!(
            detail.head.publish_state,
            dto::PUBLISH_DRAFT,
            "新建的卡必须是草稿（审核后生效）"
        );
        (Uuid::parse_str(&detail.head.id).expect("id"), detail.head.id)
    }

    /// 卡头当前发布状态 / 状态 / 实例指针（读库）。
    async fn row_of(id: Uuid) -> model::StandardRow {
        model::find_by_id(id).await.expect("查卡头").expect("卡头应存在")
    }

    /// 实例状态。
    async fn instance_status(instance_id: Uuid) -> Option<String> {
        db().scalar_with_params::<Option<String>>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例状态")
    }

    /// 实例上的 `PENDING` 待办。
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

    /// 某物料某天取标准（走真 handler；`Err` 就是「取不到」）。
    async fn effective_on(material: Uuid, date: NaiveDate) -> std::result::Result<String, i32> {
        let query = StandardEffectiveQuery {
            material_id: Some(material.to_string()),
            period_code: None,
            on_date: Some(date.to_string()),
        };
        match handler::effective_of(query).await {
            Ok(res) => Ok(res.version.head.id),
            Err(err) => Err(i32::from(err.status)),
        }
    }

    /// 把实例上所有待办批到终态，并按生产口径**在事务之后**跑业务回调。
    ///
    /// 两点与生产逐字对齐：
    /// 1. **以该待办的 assignee 身份**同意（引擎会校验「这条待办是不是你的」，用错人回 `65028`）；
    /// 2. 回调在**审批事务提交之后**跑 —— 生产里这一步是
    ///    `flow::handler` 的 `doc_reader::dispatch_settled` 扇出到本桥，单测里直接调本桥，
    ///    与 `cost_period_approval_db_tests` 同款（注册表要在 bootstrap 才填，单测进程里是空的）。
    async fn approve_all(instance_id: Uuid) {
        for (task_id, assignee) in pending_tasks(instance_id).await {
            let out = db()
                .transaction(|tx| async move { engine::approve(&tx, task_id, assignee, Some("M5-10 同意")).await })
                .await
                .expect("审批同意失败");
            assert!(out.rejected.is_none(), "审批被引擎拒绝：{:?}", out.rejected);
            publish::on_publish_decided(out.instance_id, &out.instance_status).await;
        }
    }

    // ==================== 用例 ====================

    /// 1. 提交 → 实例起来，但卡**还没生效**（审核后生效的负向证明）。
    #[tokio::test]
    async fn submit_starts_instance_but_card_is_not_live_yet() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("SUBMIT").await;
        let component = fixture_material("C1").await;

        let (id, id_text) = make_draft(material, component, day(-1), Some(day(30))).await;
        // 草稿：即使 status=1 也**不许**参与取数（发布轴没走完）
        assert_eq!(
            row_of(id).await.status,
            dto::STATUS_ACTIVE,
            "草稿默认仍占住窗口（status=1）"
        );
        assert_eq!(effective_on(material, day(0)).await, Err(404), "草稿不得参与取数");

        let detail = publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("提交发布应成功");
        assert_eq!(detail.head.publish_state, dto::PUBLISH_PENDING);
        assert_eq!(detail.head.status_label, "审批中（未生效）");

        let instance = row_of(id).await.publish_instance_id.expect("提交必须挂实例");
        assert_eq!(detail.head.publish_instance_id, Some(instance.to_string()));
        assert_eq!(instance_status(instance).await.as_deref(), Some("PENDING"));

        // 实例的业务挂载：biz_type / biz_id / biz_no 三者缺一，回调就认不到卡
        let row = db()
            .query_first_sql(
                "SELECT biz_type, biz_id, biz_no FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
                vec![Value::from(&instance)],
            )
            .await
            .expect("查实例")
            .expect("实例存在");
        assert_eq!(
            row.get_typed::<String>("biz_type").expect("biz_type"),
            publish::BIZ_TYPE
        );
        assert_eq!(row.get_typed::<Uuid>("biz_id").expect("biz_id"), id);
        let biz_no: String = row.get_typed("biz_no").expect("biz_no");
        assert!(biz_no.contains("v1"), "biz_no 应带版本号：{biz_no}");

        // 待办落到财务经理；制单人（发起人）不在待办里
        let tasks = pending_tasks(instance).await;
        let fm_id = fm().await;
        let maker_id = maker().await;
        assert!(
            tasks.iter().any(|(_, assignee)| *assignee == fm_id),
            "财务经理必须拿到待办：{tasks:?}"
        );
        assert!(
            !tasks.iter().any(|(_, assignee)| *assignee == maker_id),
            "发起人不能拿到自己的待办：{tasks:?}"
        );
        // 审批中取不到标准
        assert_eq!(effective_on(material, day(0)).await, Err(404), "审批中不得参与取数");

        purge(&[material, component]).await;
    }

    /// 2. 审批通过 → 发布并参与取数（验收核心）。
    #[tokio::test]
    async fn approve_publishes_and_becomes_effective() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("APPROVE").await;
        let component = fixture_material("C2").await;

        let (id, id_text) = make_draft(material, component, day(-1), Some(day(30))).await;
        publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("提交应成功");
        let instance = row_of(id).await.publish_instance_id.expect("实例");
        approve_all(instance).await;

        let row = row_of(id).await;
        assert_eq!(row.publish_state, dto::PUBLISH_PUBLISHED, "审批通过必须发布");
        assert_eq!(row.status, dto::STATUS_ACTIVE, "发布即参与取数");
        assert!(row.published_at.is_some(), "必须落发布时刻");
        assert_eq!(row.publish_instance_id, Some(instance), "通过后实例 id 保留作审计");
        assert_eq!(instance_status(instance).await.as_deref(), Some("APPROVED"));

        let effective = effective_on(material, day(0)).await.expect("发布后必须取得到标准");
        assert_eq!(effective, id_text, "取到的必须是这一版");

        purge(&[material, component]).await;
    }

    /// 3. 驳回 → 回草稿 + 清实例指针 + 意见留痕。
    #[tokio::test]
    async fn reject_returns_to_draft_and_clears_instance() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("REJECT").await;
        let component = fixture_material("C3").await;

        let (id, id_text) = make_draft(material, component, day(-1), Some(day(30))).await;
        publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("提交应成功");
        let instance = row_of(id).await.publish_instance_id.expect("实例");
        let fm_id = fm().await;
        let task = pending_tasks(instance)
            .await
            .into_iter()
            .find(|(_, assignee)| *assignee == fm_id)
            .map(|(task_id, _)| task_id)
            .expect("财务经理待办");
        let out = db()
            .transaction(|tx| async move {
                engine::reject(
                    &tx,
                    task,
                    fm().await,
                    "口径复核不过：费率取错了",
                    engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .expect("驳回失败");
        assert_eq!(out.instance_status, "REJECTED");
        publish::on_publish_decided(out.instance_id, &out.instance_status).await;

        let row = row_of(id).await;
        assert_eq!(row.publish_state, dto::PUBLISH_DRAFT, "驳回必须回草稿");
        assert_eq!(row.publish_instance_id, None, "驳回必须清空审批痕迹");
        let remark = row.remark.unwrap_or_default();
        assert!(remark.contains("发布驳回"), "驳回意见必须留痕：{remark}");
        assert!(remark.contains("费率取错了"), "必须带审批人原话：{remark}");
        assert!(remark.contains("M5-10 真库用例"), "原有备注不许被覆盖：{remark}");
        assert_eq!(effective_on(material, day(0)).await, Err(404), "驳回后不生效");
        // 回草稿后能再提交（不死单）
        publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("驳回后必须能重新提交");

        purge(&[material, component]).await;
    }

    /// 4. 撤回 → 实例终止 + 卡回草稿 + 零残留待办（同事务）。
    #[tokio::test]
    async fn withdraw_terminates_and_returns_draft() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("WITHDRAW").await;
        let component = fixture_material("C4").await;

        let (id, id_text) = make_draft(material, component, day(-1), Some(day(30))).await;
        publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("提交应成功");
        let instance = row_of(id).await.publish_instance_id.expect("实例");

        let detail = publish::withdraw_publish(Some(maker().await), &id_text)
            .await
            .expect("撤回应成功");
        assert_eq!(detail.head.publish_state, dto::PUBLISH_DRAFT, "撤回后回草稿");
        assert_eq!(detail.head.publish_instance_id, None, "撤回必须清空实例指针");
        assert_eq!(instance_status(instance).await.as_deref(), Some("WITHDRAWN"));
        assert!(pending_tasks(instance).await.is_empty(), "撤回后不许留残余待办");
        assert!(
            detail.head.remark.as_deref().unwrap_or_default().contains("发布撤回"),
            "撤回必须留痕：{:?}",
            detail.head.remark
        );
        assert_eq!(effective_on(material, day(0)).await, Err(404), "撤回后不生效");

        purge(&[material, component]).await;
    }

    /// 5. 状态闸：错误的发布状态一律 409 + `66111`，不给人留后门。
    #[tokio::test]
    async fn publish_guard_blocks_every_wrong_state() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("GUARD").await;
        let component = fixture_material("C5").await;

        let (id, id_text) = make_draft(material, component, day(-1), Some(day(30))).await;

        // ① 草稿不许「撤回」（没有在跑的审批）
        let err = publish::withdraw_publish(Some(maker().await), &id_text)
            .await
            .expect_err("草稿撤回必须被拒");
        assert_eq!((err.status, err.code), (409, dto::CODE_PUBLISH_GUARD));

        // ② 草稿不许手工置生效（否则发布审批就是一堆纸）
        let err = handler::set_status(
            Some(maker().await),
            &id_text,
            StandardStatusReq {
                status: Some(dto::STATUS_ACTIVE),
                reason: None,
            },
        )
        .await
        .expect_err("未发布不许置生效");
        assert_eq!((err.status, err.code), (409, dto::CODE_PUBLISH_GUARD));
        assert!(err.msg.contains("/publish"), "报文必须给出路：{}", err.msg);

        // ③ 提交后：重复提交 / 改口径 / 删除 全部 409
        publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect("提交应成功");
        let err = publish::submit_publish(Some(maker().await), &id_text)
            .await
            .expect_err("重复提交必须被拒");
        assert_eq!((err.status, err.code), (409, dto::CODE_PUBLISH_GUARD));

        let err = handler::update_standard(
            Some(maker().await),
            &id_text,
            StandardUpdateReq {
                remark: Some("审批中改备注也要拦".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("审批中不许改口径");
        assert_eq!((err.status, err.code), (409, dto::CODE_PUBLISH_GUARD));

        let err = handler::delete_standard(Some(maker().await), &id_text)
            .await
            .expect_err("审批中不许删");
        assert_eq!((err.status, err.code), (409, dto::CODE_PUBLISH_GUARD));

        // ④ 发布后：生效/失效照常可用（status 轴独立），取数跟着 status 走
        let instance = row_of(id).await.publish_instance_id.expect("实例");
        approve_all(instance).await;
        assert_eq!(effective_on(material, day(0)).await, Ok(id_text.clone()));
        let deactivated = handler::set_status(
            Some(maker().await),
            &id_text,
            StandardStatusReq {
                status: Some(dto::STATUS_INACTIVE),
                reason: Some("M5-10 用例失效".into()),
            },
        )
        .await
        .expect("已发布卡可以失效");
        assert_eq!(deactivated.version.head.status, dto::STATUS_INACTIVE);
        assert_eq!(
            deactivated.version.head.publish_state,
            dto::PUBLISH_PUBLISHED,
            "失效不改发布轴"
        );
        assert_eq!(effective_on(material, day(0)).await, Err(404), "失效后取不到标准");
        handler::set_status(
            Some(maker().await),
            &id_text,
            StandardStatusReq {
                status: Some(dto::STATUS_ACTIVE),
                reason: None,
            },
        )
        .await
        .expect("已发布的卡可以再启用（同一版复用）");

        purge(&[material, component]).await;
    }

    /// 6. 内控：制单人来源 = `create_by`（与「谁不能自审」同源）。
    #[tokio::test]
    async fn internal_control_uses_create_by_as_maker() {
        let Some(_guard) = setup().await else { return };
        let material = fixture_material("SELF").await;
        let component = fixture_material("C6").await;

        let (id, _id_text) = make_draft(material, component, day(-1), Some(day(30))).await;
        let creator = publish::internal_control_creator(Some(id)).await.expect("查制单人");
        assert_eq!(creator, Some(maker().await), "内控的制单人必须是 create_by");
        assert_eq!(publish::internal_control_creator(None).await.expect("无 biz_id"), None);

        purge(&[material, component]).await;
    }
}
