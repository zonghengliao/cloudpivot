//! 成本核算批次的真库用例（`ERP_DB_URL` 开关）—— B3-1 结算四件套骨架。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b31 \
//!   cargo test --lib cost_period_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面「状态机 + 非法跃迁 + 权限码」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`create_update_cancel_round_trip`] | 建 `DRAFT`（取号 `CS-yyyyMM-0001`）/ 改人工来源与备注 / 作废（原因 ≥10）/ 作废后再建同期间（`CANCELLED` 不占位）/ 状态日志两条 |
//! | 2 | [`duplicate_active_period_is_rejected`] | 同期间第二条活跃批次 ⇒ 409 + `66072`（`uk_biz_cost_period` 的人话版） |
//! | 3 | [`full_chain_walks_and_illegal_jumps_are_rejected`] | `DRAFT→SUBMITTED→APPROVED→SETTLED→CLOSED` 全链路真跑；`DRAFT→SETTLED` / `CLOSED→REVERSED` 被状态机拒（409）；**库层 CHECK** 也拦「说结算了但没有凭证」 |
//! | 4 | [`reversed_batch_frees_the_period`] | `SETTLED→REVERSED` 后同期间可重建新批次（唯一键排除 `REVERSED`，设计附录 A 断言⑦） |
//! | 5 | [`page_filters_by_period_and_status`] | 列表按期间 / 状态过滤 + 全序（`period_code DESC, batch_no DESC`） |
//!
//! # 夹具纪律
//!
//! 期间固定用 **2031-01 … 2031-09**（本模块专用，别的卡不会用将来的测试月份），
//! 每个用例开头 `DELETE FROM biz_cost_period WHERE period_code = ?` 清场、
//! 结尾按 id 再删一次（`biz_status_log` 按 `entity_id` 删）。**不按前缀扫全库**。
//! 明细 `biz_cost_period_item` 由 `ON DELETE CASCADE` 跟着走（本卡不造明细行，
//! 但 `sheet_items` / `totals_of` 两条 SQL 在用例里真执行过 —— 空结果也算执行，
//! 能抓出列名 / join 写错）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_period_dto::{
        self, CostPeriodCancelReq, CostPeriodCreateReq, CostPeriodPageQuery, CostPeriodUpdateReq,
    };
    use crate::business::erp::cost_period_handler as handler;
    use crate::business::erp::cost_period_model as model;
    use crate::business::shared::state_machine;
    use tokio::sync::OnceCell;

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

    /// 操作人（真库里有外键的表都不写人的 id，这里只做日志留痕，随便给一个）。
    fn operator() -> Option<Uuid> {
        Some(Uuid::now_v7())
    }

    fn state_operator() -> state_machine::Operator {
        state_machine::Operator::named(operator(), "b3-1-tester")
    }

    /// 清场：该期间的批次（明细 CASCADE）+ 状态日志。
    async fn purge(period_code: &str) {
        db().exec_sql(
            "DELETE FROM biz_status_log WHERE entity_type = 'COST_PERIOD' \
               AND entity_id IN (SELECT id FROM biz_cost_period WHERE period_code = ?)"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("清状态日志");
        db().exec_sql(
            "DELETE FROM biz_cost_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("清批次");
    }

    /// 状态日志（按 entity_id 取，按写入顺序）。
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

    fn create_req(period_code: &str) -> CostPeriodCreateReq {
        CostPeriodCreateReq {
            period_code: Some(period_code.to_string()),
            labor_source: None,
            remark: Some("月结核算".to_string()),
        }
    }

    /// 批次号流水（`CS-203101-0004` → `4`）。
    ///
    /// 不硬比 `0001`：`biz_seq` 的流水是**跨轮次持久**的（演练库重跑不清号段），
    /// 硬比 0001 会让「第二次跑」永远红。形状（`CS-yyyyMM-NNNN`）与「单调递增」
    /// 才是要钉的口径；`0001` 的**拼法**由纯单测 `batch_no_is_cs_yyyymm_0001` 钉死。
    fn batch_seq(batch_no: &str) -> i64 {
        batch_no
            .rsplit('-')
            .next()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("批次号不是 CS-yyyyMM-NNNN：{batch_no}"))
    }

    fn assert_batch_shape(batch_no: &str, period_key: &str) {
        let prefix = format!("CS-{period_key}-");
        assert!(batch_no.starts_with(&prefix), "{batch_no} 应以 {prefix} 开头");
        let tail = &batch_no[prefix.len()..];
        assert!(
            tail.len() >= 4 && tail.chars().all(|ch| ch.is_ascii_digit()),
            "{batch_no} 的流水至少 4 位数字"
        );
    }

    // ==================== 用例 ====================

    /// 建 → 改 → 作废 → 再建（`CANCELLED` 不占位）；两条状态日志都在。
    #[tokio::test(flavor = "multi_thread")]
    async fn create_update_cancel_round_trip() {
        if !ensure_db().await {
            return;
        }
        let period = "2031-01";
        purge(period).await;

        // ---- 建 DRAFT ----
        let detail = handler::create_draft(operator(), create_req(period))
            .await
            .expect("建批次");
        assert_eq!(detail.period_code, period);
        assert_batch_shape(&detail.batch_no, "203101");
        assert!(batch_seq(&detail.batch_no) >= 1, "流水从 1 起（{}）", detail.batch_no);
        assert_eq!(detail.status, cost_period_dto::STATUS_DRAFT);
        assert_eq!(detail.labor_source, cost_period_dto::LABOR_SOURCE_MANUAL);
        assert_eq!(
            detail.next_statuses,
            vec!["SUBMITTED".to_string(), "CANCELLED".to_string()],
            "DRAFT 的两条出路"
        );
        assert_eq!(detail.legal_edges.len(), 7);
        assert!(detail.items.is_empty(), "还没算过 → 没有计算单");
        assert_eq!(detail.items_summary.total_cost, "0", "空明细合计为 0");

        let id = Uuid::parse_str(&detail.id).expect("id");
        // 建单留痕（from_status = NULL）
        let logs = logs_of(id).await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0, None);
        assert_eq!(logs[0].1, "DRAFT");

        // ---- 改：人工来源切 PAYROLL + 改备注 ----
        let updated = handler::update_draft(
            operator(),
            &detail.id,
            CostPeriodUpdateReq {
                labor_source: Some("payroll".to_string()),
                remark: Some("改按薪资单取数".to_string()),
            },
        )
        .await
        .expect("改草稿");
        assert_eq!(updated.labor_source, "PAYROLL", "小写入参归一成大写");
        assert_eq!(updated.remark.as_deref(), Some("改按薪资单取数"));

        // ---- 作废：原因太短要 400 ----
        let short = handler::cancel_period(
            operator(),
            &detail.id,
            CostPeriodCancelReq {
                reason: Some("算错".to_string()),
            },
        )
        .await
        .expect_err("原因太短");
        assert_eq!(short.status, 400, "{}", short.msg);

        // ---- 作废成功 ----
        let cancelled = handler::cancel_period(
            operator(),
            &detail.id,
            CostPeriodCancelReq {
                reason: Some("期间的产量录错了，整期重做".to_string()),
            },
        )
        .await
        .expect("作废");
        assert_eq!(cancelled.status, cost_period_dto::STATUS_CANCELLED);
        assert_eq!(cancelled.cancel_reason.as_deref(), Some("期间的产量录错了，整期重做"));
        assert!(cancelled.next_statuses.is_empty(), "作废是终态");

        let logs = logs_of(id).await;
        assert_eq!(logs.len(), 2, "建单 + 作废两条轨迹");
        assert_eq!(logs[1].0.as_deref(), Some("DRAFT"));
        assert_eq!(logs[1].1, "CANCELLED");
        assert_eq!(
            logs[1].2.as_deref(),
            Some("期间的产量录错了，整期重做"),
            "作废原因进状态日志"
        );

        // ---- 作废后再建同期间（CANCELLED 不占唯一键） ----
        let rebuilt = handler::create_draft(operator(), create_req(period))
            .await
            .expect("作废后可重建");
        assert_ne!(rebuilt.id, detail.id);
        assert_batch_shape(&rebuilt.batch_no, "203101");
        assert!(
            batch_seq(&rebuilt.batch_no) > batch_seq(&detail.batch_no),
            "流水单调递增（不复用号）：{} → {}",
            detail.batch_no,
            rebuilt.batch_no
        );
        assert_eq!(rebuilt.status, cost_period_dto::STATUS_DRAFT);

        // 两次作废同一个 id ⇒ 409（状态闸；原因先给够 10 个字，否则先吃 400）
        let again = handler::cancel_period(
            operator(),
            &detail.id,
            CostPeriodCancelReq {
                reason: Some("再作废一次看看会不会被拦".to_string()),
            },
        )
        .await
        .expect_err("已作废不能再作废");
        assert_eq!(again.status, 409);
        assert_eq!(again.code, crate::shared::error_code::ERP_COST_PERIOD_STATUS_GUARD);

        purge(period).await;
    }

    /// 同期间第二条活跃批次 ⇒ 409 + 66072。
    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_active_period_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let period = "2031-02";
        purge(period).await;

        let first = handler::create_draft(operator(), create_req(period))
            .await
            .expect("建第一条");
        let error = handler::create_draft(operator(), create_req(period))
            .await
            .expect_err("同期间第二条必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_PERIOD_DUPLICATE);
        assert!(
            error.msg.contains(&first.batch_no),
            "报文要点名占位的那一批：{}",
            error.msg
        );

        // 快照：库里仍然只有一条
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*)::bigint FROM biz_cost_period \
                  WHERE period_code = ? AND deleted_at IS NULL \
                    AND status NOT IN ('REVERSED','CANCELLED')",
                vec![Value::Text(period.to_string())],
            )
            .await
            .expect("计数");
        assert_eq!(count, 1);

        purge(period).await;
    }

    /// 全链路真跑 + 非法跃迁被拒 + 库层 CHECK 兜底。
    #[tokio::test(flavor = "multi_thread")]
    async fn full_chain_walks_and_illegal_jumps_are_rejected() {
        if !ensure_db().await {
            return;
        }
        let period = "2031-03";
        purge(period).await;

        let detail = handler::create_draft(operator(), create_req(period))
            .await
            .expect("建批次");
        let id = Uuid::parse_str(&detail.id).expect("id");

        // ---- 非法跃迁：DRAFT → SETTLED（免审直过）必须被状态机拒 ----
        let jump = state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "DRAFT",
            "SETTLED",
            state_operator(),
            Some("跳过审批"),
            None,
        )
        .await
        .expect_err("DRAFT → SETTLED 必须 409");
        assert_eq!(jump.status, 409, "{}", jump.msg);

        // ---- 合法链路：DRAFT → SUBMITTED（库层要求审批实例）----
        state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "DRAFT",
            "SUBMITTED",
            state_operator(),
            None,
            None,
        )
        .await
        .expect("提交");
        // ck_biz_cost_period_appr：SUBMITTED 起必须挂审批实例
        let no_instance = db()
            .exec_sql(
                "UPDATE biz_cost_period SET status = 'SUBMITTED', updated_at = now() \
                  WHERE id = ?::uuid AND approval_instance_id IS NULL"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await;
        assert!(
            matches!(no_instance, Err(DbError::CheckViolation { .. })),
            "SUBMITTED 但没有 approval_instance_id 必须被库层 CHECK 拒（拿到 {no_instance:?}）"
        );
        let instance = Uuid::now_v7();
        let affected = db()
            .exec_sql(
                "UPDATE biz_cost_period SET status = 'SUBMITTED', approval_instance_id = ?::uuid, \
                        updated_at = now() WHERE id = ?::uuid AND status = 'DRAFT'"
                    .to_string(),
                vec![Value::from(&instance), Value::from(&id)],
            )
            .await
            .expect("落 SUBMITTED");
        assert_eq!(affected, 1);

        // SUBMITTED → DRAFT（驳回 / 撤回）：不需要原因
        state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "SUBMITTED",
            "DRAFT",
            state_operator(),
            None,
            None,
        )
        .await
        .expect("驳回 / 撤回不需要原因");
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'DRAFT', updated_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("落 DRAFT");

        // 再走一遍到 APPROVED
        for (from, to) in [("DRAFT", "SUBMITTED"), ("SUBMITTED", "APPROVED")] {
            state_machine::transition(
                db(),
                state_machine::ENTITY_COST_PERIOD,
                id,
                &detail.batch_no,
                &model::LEGAL_EDGES,
                from,
                to,
                state_operator(),
                None,
                None,
            )
            .await
            .unwrap_or_else(|error| panic!("{from} → {to} 失败：{}", error.msg));
        }
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'APPROVED', updated_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("落 APPROVED");

        // ---- 结算：状态机 + 库层要求 settled_at / finish_voucher_id ----
        state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "APPROVED",
            "SETTLED",
            state_operator(),
            None,
            None,
        )
        .await
        .expect("APPROVED → SETTLED");
        let no_voucher = db()
            .exec_sql(
                "UPDATE biz_cost_period SET status = 'SETTLED', updated_at = now() \
                  WHERE id = ?::uuid AND finish_voucher_id IS NULL"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await;
        assert!(
            matches!(no_voucher, Err(DbError::CheckViolation { .. })),
            "SETTLED 但没有 finish_voucher_id 必须被库层 CHECK 拒（拿到 {no_voucher:?}）"
        );
        let finish_voucher = Uuid::now_v7();
        let settled = db()
            .exec_sql(
                "UPDATE biz_cost_period SET status = 'SETTLED', settled_at = now(), \
                        settled_by = ?::uuid, finish_voucher_id = ?::uuid, updated_at = now() \
                  WHERE id = ?::uuid AND status = 'APPROVED'"
                    .to_string(),
                vec![
                    Value::from(&operator().expect("结算人")),
                    Value::from(&finish_voucher),
                    Value::from(&id),
                ],
            )
            .await
            .expect("落 SETTLED");
        assert_eq!(settled, 1);

        // ---- 关期：SETTLED → CLOSED ----
        state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "SETTLED",
            "CLOSED",
            state_operator(),
            None,
            None,
        )
        .await
        .expect("SETTLED → CLOSED");
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'CLOSED', updated_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("落 CLOSED");

        // ---- 终态回头路：CLOSED → REVERSED 必须被拒 ----
        let back = state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "CLOSED",
            "REVERSED",
            state_operator(),
            Some("结账后不许红冲"),
            None,
        )
        .await
        .expect_err("CLOSED 是终态");
        assert_eq!(back.status, 409, "{}", back.msg);

        // 最终状态
        let row = model::find_by_id(id).await.expect("查").expect("存在");
        assert_eq!(row.status, "CLOSED");

        // 轨迹：建单 + 提交 + 驳回 + 提交 + 审批 + 结算 + 关期 = 7 条
        let logs = logs_of(id).await;
        assert_eq!(logs.len(), 7, "轨迹条数：{logs:?}");
        assert_eq!(logs[0].0, None);
        assert_eq!(logs[6].1, "CLOSED");

        purge(period).await;
    }

    /// 红冲后同期间可重建（唯一键排除 REVERSED）。
    #[tokio::test(flavor = "multi_thread")]
    async fn reversed_batch_frees_the_period() {
        if !ensure_db().await {
            return;
        }
        let period = "2031-04";
        purge(period).await;

        let detail = handler::create_draft(operator(), create_req(period))
            .await
            .expect("建批次");
        let id = Uuid::parse_str(&detail.id).expect("id");
        // 直接造一个「已结算」行（结算事务属 B3-4，本卡只验唯一键的占位语义）
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'SETTLED', approval_instance_id = ?::uuid, \
                    settled_at = now(), settled_by = ?::uuid, finish_voucher_id = ?::uuid, \
                    updated_at = now() WHERE id = ?::uuid"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&operator().expect("人")),
                Value::from(&Uuid::now_v7()),
                Value::from(&id),
            ],
        )
        .await
        .expect("造已结算行");

        // 红冲：SETTLED → REVERSED（必填原因）
        let no_reason = state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "SETTLED",
            "REVERSED",
            state_operator(),
            None,
            None,
        )
        .await
        .expect_err("红冲必须给原因");
        assert_eq!(no_reason.status, 400, "{}", no_reason.msg);

        state_machine::transition(
            db(),
            state_machine::ENTITY_COST_PERIOD,
            id,
            &detail.batch_no,
            &model::LEGAL_EDGES,
            "SETTLED",
            "REVERSED",
            state_operator(),
            Some("期末产量作废，整批红冲重算"),
            None,
        )
        .await
        .expect("红冲");
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'REVERSED', reversed_at = now(), \
                    reversed_by = ?::uuid, reverse_reason = '期末产量作废，整批红冲重算', \
                    updated_at = now() WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&operator().expect("人")), Value::from(&id)],
        )
        .await
        .expect("落 REVERSED");

        // 同期间重建（若唯一键没排除 REVERSED，这里会 unique_violation）
        let rebuilt = handler::create_draft(operator(), create_req(period))
            .await
            .expect("红冲后同期间可重建");
        assert_eq!(rebuilt.status, cost_period_dto::STATUS_DRAFT);
        assert_ne!(rebuilt.batch_no, detail.batch_no);

        // 现在同期间有一条 REVERSED + 一条 DRAFT，活跃批次数仍为 1
        let active: i64 = db()
            .scalar_with_params(
                "SELECT count(*)::bigint FROM biz_cost_period \
                  WHERE period_code = ? AND deleted_at IS NULL \
                    AND status NOT IN ('REVERSED','CANCELLED')",
                vec![Value::Text(period.to_string())],
            )
            .await
            .expect("计数");
        assert_eq!(active, 1);

        purge(period).await;
    }

    /// 列表过滤 + 全序。
    #[tokio::test(flavor = "multi_thread")]
    async fn page_filters_by_period_and_status() {
        if !ensure_db().await {
            return;
        }
        let (p1, p2) = ("2031-05", "2031-06");
        purge(p1).await;
        purge(p2).await;

        let a = handler::create_draft(operator(), create_req(p1)).await.expect("建 1");
        let b = handler::create_draft(operator(), create_req(p2)).await.expect("建 2");
        handler::cancel_period(
            operator(),
            &b.id,
            CostPeriodCancelReq {
                reason: Some("测试作废：期间选错了".to_string()),
            },
        )
        .await
        .expect("作废 b");

        // 按期间
        let page = handler::page_periods(CostPeriodPageQuery {
            period_code: Some(p1.to_string()),
            ..Default::default()
        })
        .await
        .expect("按期间查");
        assert!(page.list.iter().any(|row| row.id == a.id));
        assert!(page.list.iter().all(|row| row.period_code == p1));

        // 按状态（大写归一）
        let page = handler::page_periods(CostPeriodPageQuery {
            status: Some("cancelled".to_string()),
            ..Default::default()
        })
        .await
        .expect("按状态查");
        assert!(page.list.iter().any(|row| row.id == b.id));
        assert!(page.list.iter().all(|row| row.status == "CANCELLED"));

        // 该期间的两条都在（作废那条不占唯一键，但仍然出现在列表里 —— 审计要看得见）
        let page = handler::page_periods(CostPeriodPageQuery {
            period_code: Some(p2.to_string()),
            ..Default::default()
        })
        .await
        .expect("按期间查 2");
        assert_eq!(page.list.len(), 1, "2031-06 只有一条");

        purge(p1).await;
        purge(p2).await;
    }
}
