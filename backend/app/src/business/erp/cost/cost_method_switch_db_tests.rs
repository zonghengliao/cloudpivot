//! 成本方法切换（M5-31）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m531_dev \
//!   cargo test --lib cost_method_switch_db_tests -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-31「迁移策略：存量期间不重算」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`switch_keeps_history_and_moves_future`] | **核心**：切换后老期间解析出的方法不变、结果表零改动、老期间照旧能算、新期间按新方法被 66101 拦 |
//! | 2 | [`global_switch_closes_the_seed_version`] | 全公司默认：种子行（1970 起长期有效）被收口到切换日前一天，收尾还原 |
//! | 3 | [`switch_day_inside_an_existing_period_is_rejected`] | 切换日所属期间已有批次 ⇒ 409/66113，报文点名期间与建议日 |
//! | 4 | [`unimplemented_target_needs_explicit_confirm`] | 未实现方法：不给确认 ⇒ 409/66101 且**一行没写**；给了确认 ⇒ 成功 |
//! | 5 | [`second_switch_on_the_same_day_is_an_overlap`] | 同一切换日再切一次 ⇒ 409/66102，第一次的结果不被破坏 |
//! | 6 | [`period_ledgers_aggregate_by_period`] | 台账按期间聚合：批次数 / 状态去重 / 红冲留痕也列出来 |
//!
//! # 夹具纪律
//!
//! · 成本对象与配置行都是**本用例自己造的**（按 id 清场，绝不按前缀扫全库）；
//! · 日期一律「相对今天」算：切换日 = 下月 1 日，存量期间 = 本月 / 上月；
//! · 用例 2 会动种子行（它本来就是要被收口的旧版）→ 先读值、先清场、再断言，
//!   保证断言失败也不会把种子行留在「已收口」状态。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_method_dto::{
        self as method_dto, CostMethodEffectiveQuery, METHOD_JOB, METHOD_STANDARD, METHOD_VARIETY, SCOPE_GLOBAL,
        SCOPE_OBJECT, STATUS_ENABLED,
    };
    use crate::business::erp::cost::cost_method_handler as method_handler;
    use crate::business::erp::cost::cost_method_model as method_model;
    use crate::business::erp::cost::cost_method_switch_dto::{self as dto, CostMethodSwitchQuery, CostMethodSwitchReq};
    use crate::business::erp::cost::cost_method_switch_handler as handler;
    use crate::business::erp::cost::cost_method_switch_model as model;
    use nik::prelude::chrono::{Datelike, Duration, FixedOffset, NaiveDate, Utc};
    use tokio::sync::OnceCell;

    /// 种子行主键（V0459 的 GLOBAL/JOB 长期有效行）。
    const SEED_ID: &str = "20000000-0000-4000-8000-00000000d001";

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

    /// 今日（CST）—— 与 handler 同口径。
    fn today() -> NaiveDate {
        Utc::now()
            .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
            .date_naive()
    }

    /// 下月 1 日（= 建议切换日，也是本文件所有「合法切换」的切换日）。
    fn next_period_start() -> NaiveDate {
        dto::suggested_switch_on(today(), &[])
    }

    /// 今天所在期间的上一个月首日（造「存量期间」用）。
    fn last_period_start() -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(today().year(), today().month(), 1).expect("本月首日");
        first - Duration::days(1) // 上月最后一天
    }

    /// 唯一后缀。
    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 造一个成本对象（直接 INSERT，只满足本表弱引用的存在性）。
    async fn fixture_object(status: &str) -> Uuid {
        let suffix = uniq();
        let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
        let product = crate::test_seed::ensure_material_kind(
            &format!("M531-MAT-{suffix}"),
            &format!("M5-31 夹具产出品 {suffix}"),
            "FINISHED",
        )
        .await;
        let code = format!("M5S{}", crate::test_seed::last_chars(&suffix, 10));
        let warehouse = crate::test_seed::ensure_warehouse(&code, &format!("M5-31 夹具仓 {suffix}")).await;
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_object \
                 (id, object_no, object_type, product_id, target_warehouse_id, plan_qty, \
                  workshop_dept_id, workshop_dept_name, start_date, status, created_at, updated_at) \
             VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?::uuid, 100, ?::uuid, 'M5-31 车间', ?::text::date, ?, now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(format!("CB-M531-{suffix}")),
                Value::from(&product),
                Value::from(&warehouse),
                Value::from(&dept),
                Value::Text(today().to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("造成本对象夹具");
        id
    }

    /// 直接插一条方法配置（绕过 handler 的重叠预检：夹具要的是「切换前的时间线」）。
    async fn insert_config(
        method: &str,
        scope_type: &str,
        scope_id: Option<Uuid>,
        effective_from: NaiveDate,
        effective_to: Option<NaiveDate>,
        status: i16,
    ) -> Uuid {
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_method_config \
                 (id, method, scope_type, scope_id, effective_from, effective_to, status, remark, \
                  created_at, updated_at) \
             VALUES (?::uuid, ?, ?, NULLIF(?::text,'')::uuid, ?::text::date, \
                     NULLIF(?::text,'')::text::date, ?, 'M5-31 夹具', now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(method.to_string()),
                Value::Text(scope_type.to_string()),
                Value::Text(scope_id.map(|value| value.to_string()).unwrap_or_default()),
                Value::Text(effective_from.to_string()),
                Value::Text(effective_to.map(|date| date.to_string()).unwrap_or_default()),
                Value::I16(status),
            ],
        )
        .await
        .expect("插方法配置夹具");
        id
    }

    /// 直接插一个成本期间批次（单头；金额满足两条守恒 CHECK）。
    ///
    /// 状态约束（V0373）：`SUBMITTED` 之后必须挂审批实例；`SETTLED` 之后必须
    /// `settled_at` + `finish_voucher_id`。红冲 / 作废额外要原因。
    async fn fixture_period(period_code: &str, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        let suffix = uniq();
        let needs_approval = matches!(status, "SUBMITTED" | "APPROVED" | "SETTLED" | "CLOSED" | "REVERSED");
        let needs_settle = matches!(status, "SETTLED" | "CLOSED" | "REVERSED");
        let approval = if needs_approval {
            Uuid::now_v7().to_string()
        } else {
            String::new()
        };
        let settled_at = if needs_settle {
            "now()".to_string()
        } else {
            "NULL".to_string()
        };
        let finish_voucher = if needs_settle {
            format!("'{}'::uuid", Uuid::now_v7())
        } else {
            "NULL".to_string()
        };
        let reverse_reason = if status == "REVERSED" {
            "'M5-31 夹具红冲'".to_string()
        } else {
            "NULL".to_string()
        };
        let reversed_at = if status == "REVERSED" {
            "now()".to_string()
        } else {
            "NULL".to_string()
        };
        let cancel_reason = if status == "CANCELLED" {
            "'M5-31 夹具作废'".to_string()
        } else {
            "NULL".to_string()
        };
        db().exec_sql(
            format!(
                "INSERT INTO biz_cost_period \
                     (id, period_code, batch_no, status, object_count, total_material, total_labor, \
                      total_overhead, total_cost, total_finished, total_wip, approval_instance_id, \
                      settled_at, finish_voucher_id, reversed_at, reverse_reason, cancel_reason, \
                      created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 100, 20, 30, 150, 150, 0, \
                         NULLIF(?::text,'')::uuid, {settled_at}, {finish_voucher}, \
                         {reversed_at}, {reverse_reason}, {cancel_reason}, now(), now())"
            ),
            vec![
                Value::from(&id),
                Value::Text(period_code.to_string()),
                Value::Text(format!(
                    "CS-{}-{}",
                    period_code.replace('-', ""),
                    crate::test_seed::last_chars(&suffix, 4)
                )),
                Value::Text(status.to_string()),
                Value::Text(approval),
            ],
        )
        .await
        .expect("插成本期间夹具");
        id
    }

    /// 清场：配置行按 id 删、期间按 id 删、对象按 id 删（共享基线不动）。
    async fn cleanup(configs: &[Uuid], periods: &[Uuid], object: Option<Uuid>) {
        if !configs.is_empty() {
            let list = configs
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(", ");
            if let Err(err) = db()
                .exec_sql(
                    format!("DELETE FROM biz_cost_method_config WHERE id IN ({list})"),
                    vec![],
                )
                .await
            {
                println!("清场失败（配置）：{err}");
            }
        }
        if !periods.is_empty() {
            let list = periods
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(", ");
            if let Err(err) = db()
                .exec_sql(format!("DELETE FROM biz_cost_period WHERE id IN ({list})"), vec![])
                .await
            {
                println!("清场失败（期间）：{err}");
            }
        }
        if let Some(object_id) = object {
            for sql in [
                format!("DELETE FROM biz_cost_method_config WHERE scope_id = '{object_id}'::uuid"),
                format!("DELETE FROM biz_cost_object WHERE id = '{object_id}'::uuid"),
            ] {
                if let Err(err) = db().exec_sql(sql, vec![]).await {
                    println!("清场失败（对象）：{err}");
                }
            }
        }
    }

    /// 期间台账快照（判「结果表零改动」用）：id → (期间码/状态, 总成本)。
    async fn period_snapshot(periods: &[Uuid]) -> Vec<(String, String, String)> {
        if periods.is_empty() {
            return Vec::new();
        }
        let list = periods
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = db()
            .query_sql(
                format!(
                    "SELECT id::text AS id, period_code, status, total_cost::text AS total_cost \
                       FROM biz_cost_period WHERE id IN ({list}) ORDER BY id"
                ),
                vec![],
            )
            .await
            .expect("读期间快照");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<String>("id").expect("id"),
                    format!(
                        "{}/{}",
                        row.get_typed::<String>("period_code").expect("期间"),
                        row.get_typed::<String>("status").expect("状态")
                    ),
                    row.get_typed::<String>("total_cost").expect("金额"),
                )
            })
            .collect()
    }

    /// 期间窗口（月首 / 月末）。
    fn bounds(period_code: &str) -> (NaiveDate, NaiveDate) {
        dto::month_bounds(period_code).expect("期间码")
    }

    /// 1. 核心用例：切换后**存量期间方法不变、结果表零改动**；老期间照旧能算，新期间被方法闸拦。
    #[tokio::test]
    async fn switch_keeps_history_and_moves_future() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let old = insert_config(
            METHOD_JOB,
            SCOPE_OBJECT,
            Some(object),
            "1970-01-01".parse().expect("日期"),
            None,
            STATUS_ENABLED,
        )
        .await;
        let current_period = dto::month_of(today());
        let last_period = dto::month_of(last_period_start());
        let settled = fixture_period(&last_period, "SETTLED").await;
        let draft = fixture_period(&current_period, "DRAFT").await;
        let fixtures = [settled, draft];
        let before = period_snapshot(&fixtures).await;
        let switch_on = next_period_start();

        // 预演：两个存量期间都不重算
        let plan = handler::switch_plan(CostMethodSwitchQuery {
            switch_on: Some(switch_on.to_string()),
            to_method: Some(METHOD_VARIETY.to_string()),
            scope: Some(SCOPE_OBJECT.to_string()),
            scope_id: Some(object.to_string()),
        })
        .await
        .expect("预演");
        assert_eq!(plan.history.len(), 2, "上月 + 本月都是存量期间：{:?}", plan.history);
        assert!(plan.history.iter().all(|row| row.unchanged && !row.recalculated));
        assert!(plan.history_untouched, "存量期间必须零改动");
        assert_eq!(plan.ops.len(), 2, "收口旧版 + 新增新版");
        assert_eq!(plan.history_batches, 2);
        assert!(plan.object_overrides.is_empty(), "对象级切换没有「被覆盖」清单");

        // 执行
        let result = handler::switch_method(
            None,
            CostMethodSwitchReq {
                switch_on: Some(switch_on.to_string()),
                to_method: Some(METHOD_VARIETY.to_string()),
                scope: Some(SCOPE_OBJECT.to_string()),
                scope_id: Some(object.to_string()),
                remark: Some("M5-31 用例：切到品种法".to_string()),
                confirm_unimplemented: true,
            },
        )
        .await
        .expect("执行切换");
        let created = Uuid::parse_str(&result.created_config_id).expect("新版本 id");
        assert_eq!(result.created_effective_from, switch_on.to_string());
        assert_eq!(
            result.closed.as_ref().and_then(|op| op.config_id.clone()),
            Some(old.to_string()),
            "收口的必须是切换日前一天仍生效的那条"
        );

        // ① 配置时间线：旧版收到切换日前一天，新版从切换日起
        let closed_row = method_model::find_by_id(old).await.expect("查旧版").expect("旧版在");
        assert_eq!(
            closed_row.effective_to,
            Some(switch_on - Duration::days(1)),
            "旧版停止日 = 切换日前一天"
        );
        let created_row = method_model::find_by_id(created)
            .await
            .expect("查新版")
            .expect("新版在");
        assert_eq!(created_row.method, METHOD_VARIETY);
        assert_eq!(created_row.effective_from, switch_on);
        assert_eq!(created_row.effective_to, None);
        assert_eq!(created_row.remark.as_deref(), Some("M5-31 用例：切到品种法"));

        // ② 存量期间的口径逐日不变：上月最后一天 / 本月最后一天都还是分批法
        let last_month_end = bounds(&last_period).1;
        let current_month_end = bounds(&current_period).1;
        for on in [last_month_end, current_month_end] {
            let hit = method_handler::effective_of(CostMethodEffectiveQuery {
                cost_object_id: Some(object.to_string()),
                on: Some(on.to_string()),
            })
            .await
            .expect("查生效方法");
            assert_eq!(hit.method, METHOD_JOB, "{on} 是存量期间，方法不许变");
            assert_eq!(hit.source, SCOPE_OBJECT);
        }
        // 切换日起是新方法
        let hit = method_handler::effective_of(CostMethodEffectiveQuery {
            cost_object_id: Some(object.to_string()),
            on: Some(switch_on.to_string()),
        })
        .await
        .expect("查生效方法");
        assert_eq!(hit.method, METHOD_VARIETY);

        // ③ 结果表零改动
        let after = period_snapshot(&fixtures).await;
        assert_eq!(before, after, "切换不许改任何成本期间行（不重算）");

        // ④ 老期间照旧能算（方法闸放行），新期间按新方法被 66101 拦
        let (last_from, last_to) = bounds(&last_period);
        method_handler::ensure_methods_runnable_in_tx(db(), &last_period, last_from, last_to)
            .await
            .expect("存量期间（分批法）必须照旧能算");
        let (cur_from, cur_to) = bounds(&current_period);
        method_handler::ensure_methods_runnable_in_tx(db(), &current_period, cur_from, cur_to)
            .await
            .expect("本月也是分批法，能算");
        let switch_period = dto::month_of(switch_on);
        let (new_from, new_to) = bounds(&switch_period);
        let err = method_handler::ensure_methods_runnable_in_tx(db(), &switch_period, new_from, new_to)
            .await
            .expect_err("切换后新期间是品种法，一期算不出来");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, method_dto::CODE_UNIMPLEMENTED);

        // 收尾：删旧版 + 新版，再删对象与期间
        cleanup(&[old, created], &fixtures, Some(object)).await;
    }

    /// 2. 全公司默认：收口的是种子行；先读值 → 先清场 → 再断言（断言失败也不留脏状态）。
    #[tokio::test]
    async fn global_switch_closes_the_seed_version() {
        if !ensure_db().await {
            return;
        }
        let seed = Uuid::parse_str(SEED_ID).expect("种子 id");
        let switch_on = next_period_start();
        let result = handler::switch_method(
            None,
            CostMethodSwitchReq {
                switch_on: Some(switch_on.to_string()),
                to_method: Some(METHOD_JOB.to_string()),
                scope: None,
                scope_id: None,
                remark: Some("M5-31 用例：全公司口径重述".to_string()),
                confirm_unimplemented: false,
            },
        )
        .await
        .expect("全公司切换（分批法已实现，不需要确认）");
        let created = Uuid::parse_str(&result.created_config_id).expect("新版本 id");
        let closed = result.closed.as_ref().expect("必须收口种子行").clone();

        // 读值
        let seed_after = method_model::find_by_id(seed).await.expect("查种子").expect("种子在");
        let seed_stop = seed_after.effective_to;
        let created_row = method_model::find_by_id(created)
            .await
            .expect("查新版")
            .expect("新版在");
        let created_from = created_row.effective_from;

        // 清场：**先删本次新增的版本，再还原种子行**（顺序反了会被重叠预检拦：种子行改成
        // 长期有效就与「从切换日起的版本」重叠）。还原走直接 SQL（不经过重叠预检），
        // 保证断言失败也能把种子行放回原状。
        cleanup(&[created], &[], None).await;
        db().exec_sql(
            format!(
                "UPDATE biz_cost_method_config SET effective_to = NULL, updated_at = now() WHERE id = '{SEED_ID}'::uuid"
            ),
            vec![],
        )
        .await
        .expect("还原种子行停止日");

        // 断言
        assert_eq!(closed.action, dto::OP_CLOSE);
        assert_eq!(closed.config_id.as_deref(), Some(SEED_ID));
        assert_eq!(seed_stop, Some(switch_on - Duration::days(1)));
        assert_eq!(created_from, switch_on);
        assert!(result.message.contains("不重算") || result.message.contains("没有算过"));
    }

    /// 3. 切换日所属期间已有批次 ⇒ 409/66113；把该期间清掉后同一预演放行。
    #[tokio::test]
    async fn switch_day_inside_an_existing_period_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let switch_on = next_period_start();
        let switch_period = dto::month_of(switch_on);
        let period = fixture_period(&switch_period, "DRAFT").await;
        let query = CostMethodSwitchQuery {
            switch_on: Some(switch_on.to_string()),
            to_method: Some(METHOD_JOB.to_string()),
            scope: Some(SCOPE_GLOBAL.to_string()),
            scope_id: None,
        };
        let err = handler::switch_plan(query.clone())
            .await
            .expect_err("该期间已有批次，不许切");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_SWITCH_GUARD);
        assert!(err.msg.contains(&switch_period), "要点名期间：{}", err.msg);
        assert!(err.msg.contains("DRAFT"), "要点名状态：{}", err.msg);
        let suggested = dto::next_month_start(bounds(&switch_period).0);
        assert!(err.msg.contains(&suggested.to_string()), "要给建议日：{}", err.msg);

        cleanup(&[], &[period], None).await;
        handler::switch_plan(query).await.expect("清掉该期间后同一预演放行");
    }

    /// 4. 未实现方法：不给确认 ⇒ 409/66101 且一行没写；给了确认 ⇒ 成功。
    #[tokio::test]
    async fn unimplemented_target_needs_explicit_confirm() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let old = insert_config(
            METHOD_JOB,
            SCOPE_OBJECT,
            Some(object),
            "1970-01-01".parse().expect("日期"),
            None,
            STATUS_ENABLED,
        )
        .await;
        let switch_on = next_period_start();
        let req = |confirm: bool| CostMethodSwitchReq {
            switch_on: Some(switch_on.to_string()),
            to_method: Some(METHOD_STANDARD.to_string()),
            scope: Some(SCOPE_OBJECT.to_string()),
            scope_id: Some(object.to_string()),
            remark: None,
            confirm_unimplemented: confirm,
        };

        let err = handler::switch_method(None, req(false))
            .await
            .expect_err("必须要求确认");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, method_dto::CODE_UNIMPLEMENTED);
        assert!(
            err.msg.contains("confirm_unimplemented"),
            "要告诉用户怎么确认：{}",
            err.msg
        );
        // 一行没写
        let untouched = method_model::find_by_id(old).await.expect("查旧版").expect("旧版在");
        assert_eq!(untouched.effective_to, None, "被拒时旧版不许被收口");
        let count = db()
            .query_sql(
                format!("SELECT count(*)::bigint AS n FROM biz_cost_method_config WHERE scope_id = '{object}'::uuid"),
                vec![],
            )
            .await
            .expect("数配置行");
        assert_eq!(count[0].get_typed::<i64>("n").expect("计数"), 1, "被拒时不许新增版本");

        let result = handler::switch_method(None, req(true)).await.expect("确认后放行");
        let created = Uuid::parse_str(&result.created_config_id).expect("新版本 id");
        assert!(result.plan.warnings.iter().any(|line| line.contains("66101")));

        cleanup(&[old, created], &[], Some(object)).await;
    }

    /// 5. 同一切换日再切一次 ⇒ 409/66102，第一次的结果不被破坏。
    #[tokio::test]
    async fn second_switch_on_the_same_day_is_an_overlap() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let old = insert_config(
            METHOD_JOB,
            SCOPE_OBJECT,
            Some(object),
            "1970-01-01".parse().expect("日期"),
            None,
            STATUS_ENABLED,
        )
        .await;
        let switch_on = next_period_start();
        let req = |method: &str| CostMethodSwitchReq {
            switch_on: Some(switch_on.to_string()),
            to_method: Some(method.to_string()),
            scope: Some(SCOPE_OBJECT.to_string()),
            scope_id: Some(object.to_string()),
            remark: None,
            confirm_unimplemented: true,
        };
        let first = handler::switch_method(None, req(METHOD_VARIETY))
            .await
            .expect("第一次切换");
        let created = Uuid::parse_str(&first.created_config_id).expect("新版本 id");

        let err = handler::switch_method(None, req(METHOD_STANDARD))
            .await
            .expect_err("同一天不许切第二次");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_OVERLAP);
        assert!(err.msg.contains(&switch_on.to_string()), "{}", err.msg);

        // 第一次的结果没被破坏
        let closed_row = method_model::find_by_id(old).await.expect("查旧版").expect("旧版在");
        assert_eq!(closed_row.effective_to, Some(switch_on - Duration::days(1)));
        let created_row = method_model::find_by_id(created)
            .await
            .expect("查新版")
            .expect("新版在");
        assert_eq!(created_row.method, METHOD_VARIETY, "第二次失败不许改第一次的方法");

        cleanup(&[old, created], &[], Some(object)).await;
    }

    /// 6. 台账按期间聚合：批次数、状态去重排序、红冲留痕也列出来。
    #[tokio::test]
    async fn period_ledgers_aggregate_by_period() {
        if !ensure_db().await {
            return;
        }
        let period_code = dto::month_of(dto::shift_month_start(&dto::month_of(today()), 14).expect("14 个月后"));
        let settled = fixture_period(&period_code, "SETTLED").await;
        let reversed = fixture_period(&period_code, "REVERSED").await;
        let ledgers = model::period_ledgers(db()).await.expect("读台账");
        let hit = ledgers
            .iter()
            .find(|row| row.period_code == period_code)
            .expect("该期间必须在台账里");
        assert_eq!(hit.batch_count, 2, "两条批次都要数进来（含红冲留痕）");
        assert_eq!(hit.statuses, vec!["REVERSED".to_string(), "SETTLED".to_string()]);
        assert!(hit.blocks_switch(), "SETTLED 构成「已有批次」");
        assert_eq!(hit.status_text(), "REVERSED,SETTLED");

        // 只剩留痕行的期间不构成「已有批次」（V0373 唯一键同口径）
        let only_reversed_code = dto::month_of(dto::shift_month_start(&dto::month_of(today()), 15).expect("15 个月后"));
        let only_reversed = fixture_period(&only_reversed_code, "REVERSED").await;
        let ledgers = model::period_ledgers(db()).await.expect("读台账");
        let hit = ledgers
            .iter()
            .find(|row| row.period_code == only_reversed_code)
            .expect("该期间必须在台账里");
        assert!(!hit.blocks_switch(), "只有红冲留痕 ⇒ 不挡切换");

        cleanup(&[], &[settled, reversed, only_reversed], None).await;
    }
}
