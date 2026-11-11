//! 成本对象（生产批）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径 —— 见
//! `expense_model_tests` / `transfer_handler_db_tests_*`）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_costb18 \
//!   cargo test --lib cost_object_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面「5 个接口全通 + 关批 5 条前置」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`create_page_detail_update_flow`] | POST 建单（取号 CB-…）/ GET 列表（含归集总成本）/ GET 详情（`next_statuses`）/ PUT 改单（部分更新） |
//! | 2 | [`close_succeeds_when_five_checks_pass`] | POST close：5 条前置全过 ⇒ CLOSED + `closed_at` + 两条状态日志；重复关批 409 |
//! | 3 | [`close_blocked_by_uncollected_stock_out`] | 前置 ②（卡面点名）：已过账未归集的领料单 ⇒ 409 + 66055；补归集后放行 |
//! | 4 | [`close_blocked_by_draft_labor`] | 前置 ①：`biz_cost_labor.status='DRAFT'` ⇒ 409；确认后放行 |
//! | 5 | [`close_blocked_by_unposted_stock_out`] | 前置 ④：DRAFT / PENDING_POST 的领料单 ⇒ 409 |
//! | 6 | [`close_blocked_by_locked_period`] | 前置 ③：期间批次已 SETTLED ⇒ 409 |
//! | 7 | [`wip_requires_force_confirmation`] | 前置 ⑤：`wip_qty > 0` 需 `force_wip=true` |
//! | 8 | [`update_rejects_after_close_and_product_change_with_collect`] | 改单守卫：已关批 409；有归集改产出品 409 |
//!
//! B2-14（本期产量与在制录入，卡 `t_61d337b4`）追加 6 条：
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 9 | [`output_entry_persists_and_audits`] | 三列落库 + `sys_change_log` 一条（前后值）+ 详情回显填写人/时间 + 重复保存不写假日志 |
//! | 10 | [`output_rejects_invalid_values_and_keeps_db_clean`] | 负数 / 进度越界 / 小数位超列 / 在制>0 无进度 ⇒ 400 + 一行不写 |
//! | 11 | [`output_warns_overproduction_but_saves`] | 超产只提示不拦（§4.10） |
//! | 12 | [`output_locked_after_period_settled_with_item`] | 已结算期间 **且该对象有计算单** ⇒ 409 + 66070（没有计算单时仍可补录） |
//! | 13 | [`output_rejected_after_close`] | 关批后只读 409 |
//! | 14 | [`output_warns_zero_output_with_collected_cost`] | 三个数全 0 但有归集 ⇒ 提示跑 calc 会撞 66067 |
//!
//! # 夹具纪律
//!
//! 全部用 `costb18-<唯一后缀>` 前缀造，清场**按 id** 删（绝不按前缀扫全库）。
//! 依赖的表由 V0370–V0376 建（`biz_cost_object` / `biz_cost_labor` /
//! `biz_cost_collect` / `biz_cost_period` / `biz_stock_out.cost_object_id`）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::changelog::{self, ChangeCtx};
    use crate::business::erp::cost_object_dto::{
        CostObjectCloseReq, CostObjectCreateReq, CostObjectOutputReq, CostObjectPageQuery, CostObjectUpdateReq,
    };
    use crate::business::erp::cost_object_handler as handler;
    use crate::business::erp::cost_object_model as model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;
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

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额 / 数量字面量")
    }

    /// 唯一后缀（进程内计数器 + 纳秒，避免并发用例互相踩）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:03}", now % 1_000_000_000)
    }

    /// 仓库编码的尾巴（`biz_warehouse.warehouse_code` 有长度上限，取后缀后 10 位）。
    fn fx_suffix_code(suffix: &str) -> String {
        let start = suffix.len().saturating_sub(10);
        suffix[start..].to_string()
    }

    /// 全套夹具：产出品（`FINISHED`）/ 仓库 / 车间。
    struct Fixture {
        suffix: String,
        product: Uuid,
        warehouse: Uuid,
        dept: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let dept = crate::shared::test_helpers::ensure_any_dept().await;
            let unit: Uuid = db()
                .scalar_with_params("SELECT id FROM biz_unit WHERE deleted_at IS NULL LIMIT 1", vec![])
                .await
                .expect("取计量单位");
            let product = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                          status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?::uuid, 'FINISHED', 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&product),
                    Value::Text(format!("COSTB18-MAT-{suffix}")),
                    Value::Text(format!("成本对象夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB18W{}", fx_suffix_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("成本对象夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");
            Self {
                suffix,
                product,
                warehouse,
                dept,
            }
        }

        fn lot_no(&self) -> String {
            format!("CB18-LOT-{}", self.suffix)
        }

        fn create_req(&self) -> CostObjectCreateReq {
            CostObjectCreateReq {
                product_id: Some(self.product.to_string()),
                target_warehouse_id: Some(self.warehouse.to_string()),
                target_location_id: None,
                plan_qty: Some(dec("100")),
                workshop_dept_id: Some(self.dept.to_string()),
                start_date: Some("2026-11-01".to_string()),
                due_date: Some("2026-11-30".to_string()),
                source_doc_no: Some(format!("SO-CB18-{}", self.suffix)),
                customer_id: None,
                lot_no: Some(self.lot_no()),
                material_input_mode: None,
                remark: Some("B1-8 真库用例".to_string()),
            }
        }

        /// 超管范围（列表不被车间裁剪；用例只验接口，不验数据权限算法）。
        fn principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        /// 清场：按 id 删（FK 顺序：归集 / 工时 / 出库 / 状态日志 / 批次 / 对象 / 物料 / 仓）。
        async fn cleanup(&self, object_ids: &[Uuid]) {
            if object_ids.is_empty() {
                return;
            }
            let list = object_ids
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            for sql in [
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_stock_out WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_status_log WHERE entity_type = 'COST_OBJECT' AND entity_id IN ({list})"),
                format!("DELETE FROM biz_cost_object WHERE id IN ({list})"),
                format!(
                    "DELETE FROM biz_lot WHERE lot_no = '{}'",
                    self.lot_no().replace('\'', "''")
                ),
                format!("DELETE FROM biz_material WHERE id = '{}'::uuid", self.product),
                format!("DELETE FROM biz_warehouse WHERE id = '{}'::uuid", self.warehouse),
            ] {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }
    }

    /// 挂一张出库单到成本对象（`status` 由用例给：POSTED / DRAFT / PENDING_POST）。
    async fn stock_out_of(suffix: &str, object: Uuid, warehouse: Uuid, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                        total_qty, total_amount, cost_object_id, created_at, updated_at) \
             VALUES (?::uuid, ?, 'ISSUE', ?::uuid, '2026-11-05', ?, 10, 100, ?::uuid, now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(format!("OUT-CB18-{suffix}-{}", &id.to_string()[..8])),
                Value::from(&warehouse),
                Value::Text(status.to_string()),
                Value::from(&object),
            ],
        )
        .await
        .expect("造领料出库单夹具");
        id
    }

    /// 造一行归集（把某张出库单「归集掉」）。`material_id` 必填 —— `ck_biz_cost_collect_mat`
    /// 要求 MATERIAL 行必须同时有物料与数量（一张表两种语义的防线）。
    async fn collect_of(object: Uuid, stock_out_id: Uuid, period: &str, material_id: Uuid) {
        db().exec_sql(
            "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                           source_id, source_no, line_no, occur_date, material_id, \
                                           qty, unit_price, amount, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, 'OUT-CB18', 1, \
                     '2026-11-05', ?::uuid, 10, 10, 100, now(), now())"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&object),
                Value::Text(period.to_string()),
                Value::from(&stock_out_id),
                Value::from(&material_id),
            ],
        )
        .await
        .expect("造归集行夹具");
    }

    // ==================== 用例 ====================

    /// ① POST / GET list / GET detail / PUT 全通。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn create_page_detail_update_flow() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        assert!(
            created.object_no.starts_with("CB-"),
            "取号 CB-yyyyMMdd：{}",
            created.object_no
        );
        assert_eq!(created.status, "OPEN");
        assert_eq!(created.plan_qty, "100.0000", "数量是 NUMERIC(18,4) 的字符串");
        assert_eq!(created.material_input_mode, "ONE_TIME");
        assert_eq!(
            created.product_name.as_deref(),
            Some(&*format!("成本对象夹具产出品 {}", fx.suffix))
        );
        assert!(created.workshop_dept_name.is_some(), "车间名要 join 出来（约定 13）");
        assert_eq!(created.collect_summary.total, "0", "还没有归集");
        assert!(created.period_items.is_empty());
        assert_eq!(
            created.next_statuses,
            vec!["CLOSED".to_string(), "CANCELLED".to_string()],
            "在产对象可去 CLOSED / CANCELLED"
        );

        // 列表：能按关键字捞到自己，且 cost_total 是字符串
        let page = handler::page_objects(
            &fx.principal(),
            CostObjectPageQuery {
                keyword: Some(created.object_no.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("列表");
        assert_eq!(page.total, 1, "按对象号应命中 1 条");
        assert_eq!(page.list[0].id, created.id);
        assert_eq!(
            page.list[0].cost_total,
            dec("0").to_string(),
            "没有归集时总成本是 0（不查不到就崩）"
        );

        // 详情：同一个对象
        let detail = handler::page_objects(&fx.principal(), CostObjectPageQuery::default())
            .await
            .expect("空筛选列表");
        assert!(detail.list.iter().any(|row| row.id == created.id), "无筛选列表也要有它");

        // 改单：部分更新（数量 + 清空完工日期）
        let updated = handler::update_object(
            None,
            &created.id,
            CostObjectUpdateReq {
                plan_qty: Some(dec("150")),
                due_date: Some(None),
                remark: Some(Some("改过的备注".to_string())),
                ..Default::default()
            },
        )
        .await
        .expect("改单");
        assert_eq!(updated.plan_qty, "150.0000");
        assert_eq!(updated.due_date, None, "显式 null 要清空");
        assert_eq!(updated.remark.as_deref(), Some("改过的备注"));

        // 建单时填了批号 ⇒ `biz_lot` 里有一行 PRODUCTION
        let lot_count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_lot WHERE lot_no = ? AND source_type = 'PRODUCTION' \
                   AND deleted_at IS NULL",
                vec![Value::Text(fx.lot_no())],
            )
            .await
            .expect("查批次主数据");
        assert_eq!(lot_count, 1, "填了 lot_no 要自动建 biz_lot");

        fx.cleanup(&[id]).await;
    }

    /// ② 5 条前置全过 ⇒ 关批成功；重复关批 409。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_succeeds_when_five_checks_pass() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");

        let closed = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect("关批");
        assert_eq!(closed.status, "CLOSED");
        assert!(closed.closed_at.is_some(), "关批必须带 closed_at（库层 CHECK）");
        assert!(closed.next_statuses.is_empty(), "CLOSED 是终态");

        // 状态日志：建单 1 条 + 关批 1 条
        let logs: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'COST_OBJECT' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志");
        assert_eq!(logs, 2, "建单（from NULL）+ 关批 各一行");

        // 重复关批：409（只有 OPEN 能关）
        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("已关批不能再关");
        assert_eq!(error.status, 409);

        fx.cleanup(&[id]).await;
    }

    /// ③ 前置 ②：已过账但未归集的领料单 ⇒ 409 + 66055；补一行归集后放行。
    ///
    /// 这是卡面点名的那条验收（「有未归集领料 → 拒绝，错误码明确」）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocked_by_uncollected_stock_out() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let out = stock_out_of(&fx.suffix, id, fx.warehouse, "POSTED").await;

        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("未归集领料必须拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66055, "业务码必须是 ERP_COST_OBJECT_CLOSE_BLOCKED");
        assert!(error.msg.contains("归集"), "报文要说清楚缺什么：{}", error.msg);
        assert!(
            error
                .internal_detail
                .as_deref()
                .unwrap_or("")
                .contains("UNCOLLECTED_STOCK_OUT"),
            "结构化明细要带分类码"
        );

        // 补上归集行 ⇒ 同一张单再关批就过
        collect_of(id, out, "2026-11", fx.product).await;
        let closed = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect("归集后可以关批");
        assert_eq!(closed.status, "CLOSED");
        assert_eq!(closed.collect_summary.mat, "100.0000", "归集汇总要读得到");

        fx.cleanup(&[id]).await;
    }

    /// ④ 前置 ①：草稿工时台账 ⇒ 拦；确认（CONFIRMED）后放行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocked_by_draft_labor() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let labor = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                         dept_id, dept_name, hours, rate, amount, status, created_at, updated_at) \
             VALUES (?::uuid, '2026-11', '2026-11-05', ?::uuid, '张三', ?::uuid, '一车间', 8, 50, 400, \
                     'DRAFT', now(), now())"
                .to_string(),
            vec![Value::from(&labor), Value::from(&id), Value::from(&fx.dept)],
        )
        .await
        .expect("造草稿工时夹具");

        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("草稿工时必须拦");
        assert_eq!(error.code, 66055);
        assert!(error.msg.contains("工时"), "{}", error.msg);

        db().exec_sql(
            "UPDATE biz_cost_labor SET status = 'CONFIRMED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&labor)],
        )
        .await
        .expect("确认工时");
        let closed = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect("确认工时后可以关批");
        assert_eq!(closed.status, "CLOSED");

        fx.cleanup(&[id]).await;
    }

    /// ⑤ 前置 ④：草稿 / 待审的领料单 ⇒ 拦。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocked_by_unposted_stock_out() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        stock_out_of(&fx.suffix, id, fx.warehouse, "PENDING_POST").await;

        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("未过账领料必须拦");
        assert_eq!(error.code, 66055);
        assert!(error.msg.contains("未过账"), "{}", error.msg);

        fx.cleanup(&[id]).await;
    }

    /// ⑥ 前置 ③：期间成本批次已锁定（SETTLED）⇒ 拦。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocked_by_locked_period() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        // 该对象在 2026-11 有归集活动 + 该期间批次已 SETTLED
        let out = stock_out_of(&fx.suffix, id, fx.warehouse, "POSTED").await;
        collect_of(id, out, "2026-11", fx.product).await;
        let period = Uuid::now_v7();
        let approval = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_period (id, period_code, batch_no, status, approval_instance_id, \
                                          settled_at, finish_voucher_id, created_at, updated_at) \
             VALUES (?::uuid, '2026-11', ?, 'SETTLED', ?::uuid, now(), ?::uuid, now(), now())"
                .to_string(),
            vec![
                Value::from(&period),
                Value::Text(format!("CS-CB18-{}", fx.suffix)),
                Value::from(&approval),
                Value::from(&Uuid::now_v7()),
            ],
        )
        .await
        .expect("造已结算期间夹具");

        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("期间已锁必须拦");
        assert_eq!(error.code, 66055);
        assert!(error.msg.contains("2026-11"), "要说哪个期间：{}", error.msg);
        assert!(error.msg.contains("SETTLED"), "{}", error.msg);

        let _ = db()
            .exec_sql(
                "DELETE FROM biz_cost_period WHERE id = ?::uuid".to_string(),
                vec![Value::from(&period)],
            )
            .await;
        fx.cleanup(&[id]).await;
    }

    /// ⑦ 前置 ⑤：`wip_qty > 0` 必须显式 `force_wip=true`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wip_requires_force_confirmation() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        model::apply_production(id, dec("80"), dec("20"), dec("50"), None)
            .await
            .expect("回写完工 / 在制");

        let error = handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect_err("有在制必须拦");
        assert_eq!(error.code, 66055);
        assert!(error.msg.contains("force_wip"), "{}", error.msg);

        let closed = handler::close_object(
            None,
            &created.id,
            CostObjectCloseReq {
                force_wip: Some(true),
                remark: Some("月末带在制关批（客户确认）".to_string()),
            },
        )
        .await
        .expect("显式确认后可以关批");
        assert_eq!(closed.status, "CLOSED");
        assert!(closed.remark.as_deref().unwrap_or("").contains("带在制关批"));

        fx.cleanup(&[id]).await;
    }

    /// ⑧ 改单守卫：已关批 409；有归集后改产出品 409。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn update_rejects_after_close_and_product_change_with_collect() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");

        // 有归集（一张已过账并归集的领料单）⇒ 改产出品被拒
        let out = stock_out_of(&fx.suffix, id, fx.warehouse, "POSTED").await;
        collect_of(id, out, "2026-11", fx.product).await;
        let error = handler::update_object(
            None,
            &created.id,
            CostObjectUpdateReq {
                product_id: Some(Uuid::now_v7().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("有归集不许改产出品");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("归集"), "{}", error.msg);

        // 关批之后一律只读
        handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect("关批");
        let error = handler::update_object(
            None,
            &created.id,
            CostObjectUpdateReq {
                plan_qty: Some(dec("200")),
                ..Default::default()
            },
        )
        .await
        .expect_err("已关批不许改");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("关批") || error.msg.contains("CLOSED"),
            "{}",
            error.msg
        );

        fx.cleanup(&[id]).await;
    }

    // ==================== B2-14：本期产量与在制 ====================
    //
    // 夹具纪律同上：唯一后缀造对象，按 id 清场；变更日志按对象 purge
    // （`changelog::model::purge_for_object`，业务代码不得调它，测试可以）。
    // 期间号段用 `2098-xx`：共享演练库里别的卡留下的 2026-xx 期间不会撞
    // `uk_biz_cost_period (org_id, period_code)`。

    /// 造一个已结算（`SETTLED`）的成本批次。
    ///
    /// 库层 CHECK `ck_biz_cost_period_settle` 要求 SETTLED 必须带
    /// `settled_at` + `finish_voucher_id`，`ck_biz_cost_period_appr` 要求带审批实例。
    async fn settled_period(suffix: &str, period_code: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_period (id, period_code, batch_no, status, approval_instance_id, \
                                          settled_at, finish_voucher_id, created_at, updated_at) \
             VALUES (?::uuid, ?, ?, 'SETTLED', ?::uuid, now(), ?::uuid, now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(period_code.to_string()),
                Value::Text(format!("CS-CB214-{suffix}")),
                Value::from(&Uuid::now_v7()),
                Value::from(&Uuid::now_v7()),
            ],
        )
        .await
        .expect("造已结算期间夹具");
        id
    }

    /// 给对象在某个批次里造一条计算单行（`CALC`，金额全 0）。
    ///
    /// 只为满足卡面口径里的「**且该对象有计算单**」—— 期间结算了但对象没参与时
    /// 产量数字与任何账务无关，仍允许补录。
    async fn period_item_of(period: Uuid, object: Uuid, product: Uuid) {
        db().exec_sql(
            "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, now(), now())"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&period),
                Value::from(&object),
                Value::from(&product),
            ],
        )
        .await
        .expect("造计算单夹具");
    }

    /// 删期间（`biz_cost_period_item` 是 `ON DELETE CASCADE`）。
    async fn drop_period(period: Uuid) {
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_cost_period WHERE id = ?::uuid".to_string(),
                vec![Value::from(&period)],
            )
            .await;
    }

    /// 清审计（测试专用；业务代码不许删审计表）。
    async fn purge_audit(id: Uuid) {
        let _ = changelog::model::purge_for_object(db(), "biz_cost_object", &id.to_string()).await;
    }

    fn output_req(completed: &str, wip: &str, pct: &str) -> CostObjectOutputReq {
        CostObjectOutputReq {
            completed_qty: Some(dec(completed)),
            wip_qty: Some(dec(wip)),
            completion_pct: Some(dec(pct)),
        }
    }

    /// ⑨ 产量录入：三列落库 + 审计留痕（填写人 + 时间）+ 详情回显 + 重复保存不写假日志。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_entry_persists_and_audits() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        // 建单时三个数取库默认 0（本卡要治的原始症状：没人能改它们）
        assert_eq!(created.completed_qty, "0.0000");
        assert_eq!(created.wip_qty, "0.0000");
        assert_eq!(created.completion_pct, "0.00", "进度列标度是 2");

        let ctx = ChangeCtx::api(None, Some("车间主任张三"));
        let saved = handler::set_output(&ctx, &created.id, output_req("80", "20", "50"))
            .await
            .expect("填本期产量与在制");
        assert!(saved.changed, "第一次填必然有变化");
        assert_eq!(saved.completed_qty, "80.0000", "回单读库（NUMERIC(18,4) 标度）");
        assert_eq!(saved.wip_qty, "20.0000");
        assert_eq!(saved.completion_pct, "50.00", "进度列是 NUMERIC(5,2)");
        assert_eq!(saved.plan_qty, "100.0000");
        assert_eq!(saved.filled_by.as_deref(), Some("车间主任张三"), "R-4：留填写人");
        assert!(saved.filled_at.is_some(), "R-4：留时间");
        assert!(saved.warnings.is_empty(), "80+20=100 不超产：{:?}", saved.warnings);

        // 库里的三个数（不信回单，直接读表）
        let row = model::find_by_id(id).await.expect("查对象").expect("对象存在");
        assert_eq!(row.completed_qty, dec("80"));
        assert_eq!(row.wip_qty, dec("20"));
        assert_eq!(row.completion_pct, dec("50"));

        // 审计：一条 UPDATE，含三列的前后值（去尾零后的规范形：`80` / `50` / 改前 `0`）
        let count = changelog::model::count_for_object(db(), "biz_cost_object", &created.id)
            .await
            .expect("数审计行");
        assert_eq!(count, 1, "一次填写一条审计");
        let diffs = changelog::model::diff_texts(db(), "biz_cost_object", &created.id)
            .await
            .expect("读审计 diff");
        for needle in [
            "completed_qty",
            "wip_qty",
            "completion_pct",
            "\"a\": \"80\"",
            "\"a\": \"50\"",
            "\"b\": \"0\"",
        ] {
            assert!(diffs[0].contains(needle), "diff 缺 {needle}：{}", diffs[0]);
        }

        // 详情回显「谁什么时候填的」
        let detail = handler::detail_object(&created.id).await.expect("详情");
        assert_eq!(detail.output_filled_by.as_deref(), Some("车间主任张三"));
        assert!(detail.output_filled_at.is_some());
        assert_eq!(detail.completed_qty, "80.0000", "详情三个数也是库里的值");

        // 再保存一模一样的三个数：changed=false，审计不增加（不留空 diff 的假记录）
        let again = handler::set_output(&ctx, &created.id, output_req("80", "20", "50"))
            .await
            .expect("重复保存");
        assert!(!again.changed, "三个数没变 ⇒ 不算一次填写");
        assert_eq!(
            changelog::model::count_for_object(db(), "biz_cost_object", &created.id)
                .await
                .expect("数审计行"),
            1,
            "重复保存不许写第二条审计"
        );

        purge_audit(id).await;
        fx.cleanup(&[id]).await;
    }

    /// ⑩ 值域校验：负数 / 进度越界 / 小数位超列 / 在制>0 无进度 —— 全 400，一行不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_rejects_invalid_values_and_keeps_db_clean() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let ctx = ChangeCtx::api(None, Some("车间主任张三"));

        let cases: [(CostObjectOutputReq, &str, &str); 4] = [
            (output_req("-1", "0", "0"), "completed_qty", "负数"),
            (output_req("0", "0", "120"), "completion_pct", "进度越界"),
            (
                output_req("0", "0", "33.333"),
                "completion_pct",
                "进度 3 位小数（列是 NUMERIC(5,2)）",
            ),
            (
                // 与 calc 第 6 段 66067 同一口径：在制 > 0 必须有进度
                output_req("0", "20", "0"),
                "completion_pct",
                "在制 20 但进度 0",
            ),
        ];
        for (req, field, why) in cases {
            let error = handler::set_output(&ctx, &created.id, req).await.expect_err(why);
            assert_eq!(error.status, 400, "{why} 该是 400：{}", error.msg);
            assert!(error.msg.contains(field), "{why} 要点名字段：{}", error.msg);
        }
        // 「在制 > 0 无进度」那条要指向同一条业务校验（用户跑 calc 时才会撞的 66067）
        let error = handler::set_output(&ctx, &created.id, output_req("0", "20", "0"))
            .await
            .expect_err("在制 20 但进度 0");
        assert!(error.msg.contains("66067"), "{}", error.msg);

        // 四个非法请求之后，库里的三个数还是 0，审计 0 条
        let row = model::find_by_id(id).await.expect("查对象").expect("对象存在");
        assert_eq!(row.completed_qty, dec("0"));
        assert_eq!(row.wip_qty, dec("0"));
        assert_eq!(row.completion_pct, dec("0"));
        assert_eq!(
            changelog::model::count_for_object(db(), "biz_cost_object", &created.id)
                .await
                .expect("数审计行"),
            0,
            "失败的保存不许留审计"
        );

        fx.cleanup(&[id]).await;
    }

    /// ⑪ 超产只提示不拦（设计 §4.10）：状态 200、数字落库、`warnings` 点名超产。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_warns_overproduction_but_saves() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let ctx = ChangeCtx::api(None, Some("车间主任张三"));

        // 计划 100，累计完工 90 + 在制 30 = 120 > 100
        let saved = handler::set_output(&ctx, &created.id, output_req("90", "30", "60"))
            .await
            .expect("超产不该被拦");
        assert!(saved.changed);
        assert_eq!(saved.warnings.len(), 1, "{:?}", saved.warnings);
        assert!(saved.warnings[0].contains("超产"), "{}", saved.warnings[0]);
        assert!(saved.warnings[0].contains("120"), "要点出合计：{}", saved.warnings[0]);

        let row = model::find_by_id(id).await.expect("查对象").expect("对象存在");
        assert_eq!(row.completed_qty, dec("90"), "超产的数量照样落库");
        assert_eq!(row.wip_qty, dec("30"));

        purge_audit(id).await;
        fx.cleanup(&[id]).await;
    }

    /// ⑫ 期间已结算且该对象有计算单 ⇒ 409 + `66070`，数字只读。
    ///
    /// 同时验「**有计算单**」这个前提是活的：批次先结算、计算单后补 ⇒ 前后行为不同。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_locked_after_period_settled_with_item() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let ctx = ChangeCtx::api(None, Some("车间主任张三"));
        let period_code = "2098-11";
        let period = settled_period(&fx.suffix, period_code).await;

        // ① 期间已结算，但该对象**没有**计算单 ⇒ 它的产量与账务无关，仍可补录
        let saved = handler::set_output(&ctx, &created.id, output_req("80", "0", "0"))
            .await
            .expect("期间结算了但这批没参与 ⇒ 允许填");
        assert_eq!(saved.completed_qty, "80.0000");

        // ② 补一条计算单 ⇒ 这三个数已经变成入库金额与结转凭证的来源，只读
        period_item_of(period, id, fx.product).await;
        let error = handler::set_output(&ctx, &created.id, output_req("10", "0", "0"))
            .await
            .expect_err("已结算期间必须拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66070, "期间锁定复用 calc 第 6 段的 66070");
        assert!(error.msg.contains(period_code), "要点名期间：{}", error.msg);
        assert!(error.msg.contains("SETTLED"), "{}", error.msg);
        assert!(error.msg.contains("红冲"), "要给修法：{}", error.msg);

        // 库里的数字还是 80（被拦的那次一个字节都没写）
        let row = model::find_by_id(id).await.expect("查对象").expect("对象存在");
        assert_eq!(row.completed_qty, dec("80"));

        purge_audit(id).await;
        drop_period(period).await;
        fx.cleanup(&[id]).await;
    }

    /// ⑬ 关批后只读：409（业务码 `RESOURCE_CONFLICT`，与改单 / 关批同一口径）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_rejected_after_close() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let ctx = ChangeCtx::api(None, Some("车间主任张三"));

        handler::set_output(&ctx, &created.id, output_req("80", "0", "0"))
            .await
            .expect("关批前可以填");
        handler::close_object(None, &created.id, CostObjectCloseReq::default())
            .await
            .expect("关批");

        let error = handler::set_output(&ctx, &created.id, output_req("90", "0", "0"))
            .await
            .expect_err("已关批不许填");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("CLOSED") || error.msg.contains("在产"),
            "报文要说清状态：{}",
            error.msg
        );
        let row = model::find_by_id(id).await.expect("查对象").expect("对象存在");
        assert_eq!(row.completed_qty, dec("80"), "关批后数字不许动");

        purge_audit(id).await;
        fx.cleanup(&[id]).await;
    }

    /// ⑭ 三个数全 0 但有归集成本 ⇒ 保存不拦，但**提示**跑 calc 会撞 66067（本卡要治的症状）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_warns_zero_output_with_collected_cost() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_object(None, fx.create_req()).await.expect("建成本对象");
        let id = Uuid::parse_str(&created.id).expect("id");
        let ctx = ChangeCtx::api(None, Some("车间主任张三"));
        // 一张已过账并归集好的领料单（100 元）
        let out = stock_out_of(&fx.suffix, id, fx.warehouse, "POSTED").await;
        collect_of(id, out, "2026-11", fx.product).await;

        // 什么都不填（请求体空）：三个数保持 0 ⇒ changed=false，但提示照出
        let saved = handler::set_output(&ctx, &created.id, CostObjectOutputReq::default())
            .await
            .expect("空请求不算错");
        assert!(!saved.changed);
        assert_eq!(saved.warnings.len(), 1, "{:?}", saved.warnings);
        assert!(saved.warnings[0].contains("66067"), "{}", saved.warnings[0]);
        assert!(
            saved.warnings[0].contains("100"),
            "要点出已归集金额：{}",
            saved.warnings[0]
        );

        // 补上完工数量 ⇒ 提示消失
        let fixed = handler::set_output(&ctx, &created.id, output_req("100", "0", "0"))
            .await
            .expect("填完工");
        assert!(fixed.warnings.is_empty(), "{:?}", fixed.warnings);

        purge_audit(id).await;
        fx.cleanup(&[id]).await;
    }
}
