//! 在制品「完工率录入（按工序）」（M5-21）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m521_dev \
//!   cargo test --lib cost_wip_pct -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_weighted_rollup_writes_back_to_object`] | 按工序录入 → 加权汇总（1×100 + 2×50 + 1×0）/4 = 50% → **同一个事务**回写对象级；逐行占比 / 贡献 |
//! | 2 | [`v2_replay_is_idempotent`] | 同一份 payload 再保存：`changed = false`、零写入、审计**不多一行** |
//! | 3 | [`v3_missing_seq_is_soft_deleted_and_pct_recomputed`] | 整表覆盖：payload 少一个 seq ⇒ 该行软删、对象级按新集合重算 |
//! | 4 | [`v4_closed_and_settled_objects_are_rejected`] | 状态闸：已关批 409 / 已结算期间 409 + `66070`，且**一行都没写** |
//! | 5 | [`v5_other_workshop_cannot_see_or_write`] | 数据范围：别车间 GET / PUT 都 404（不是 403 探测口），零写入 |
//! | 6 | [`v6_wip_without_progress_and_bad_rows_are_rejected`] | 在制 > 0 但汇总 0 ⇒ 400（会撞 66067）；重复 seq / 3 位小数 ⇒ 400 |
//! | 7 | [`v7_clearing_operations_keeps_object_pct`] | 清空工序 = 全软删；对象级**保持**最后的值（回到手填模式，不归零） |
//! | 8 | [`v8_out_of_band_object_pct_shows_drift`] | 有人用 `PUT …/output` 直接改对象级 ⇒ GET 报「不一致」、`matched = false` |
//! | 9 | [`zz_teardown_leaves_zero_residue`] | 清场后本卡造的行（工序 / 对象 / 审计）全 0 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `2000 + slot % 70`，期间取该年 01/02/03 三个月）；
//!   建前先清该槽上一次的残留；
//! * 对象号 / 物料编码全部带唯一后缀，本卡只按**自己的对象 id** 断言，绝不数全表；
//! * 审计行（`sys_change_log`）也随夹具一起清（本次保存会在里面留下工序行 + 对象级的 diff）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::changelog::ChangeCtx;
    use crate::business::erp::cost::cost_wip_pct_dto::{self as dto, OperationRowReq, OperationSaveReq};
    use crate::business::erp::cost::cost_wip_pct_handler::{operations_of, save_operations};
    use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
    use cloudpivot_kernel::business::shared::scope::ScopeKind;
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

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数量字面量")
    }

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// 审计上下文（操作人固定一个随机 id：`sys_change_log.operator_id` 不查外键）。
    fn ctx() -> ChangeCtx<'static> {
        ChangeCtx::api(Some(Uuid::now_v7()), Some("M5-21 夹具"))
    }

    struct Fixture {
        suffix: String,
        periods: [String; 3],
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        /// 本夹具造过的对象（清场用）。
        objects: std::sync::Mutex<Vec<Uuid>>,
        periods_created: std::sync::Mutex<Vec<Uuid>>,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 2000 + (slot % 70) as i32;
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
            ];
            Self::reset(&periods).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &format!("M521-DEPT2-{suffix}"),
                &format!("M5-21 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M521-MAT-{suffix}"),
                &format!("M5-21 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M52W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-21 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
                product,
                warehouse,
                objects: std::sync::Mutex::new(Vec::new()),
                periods_created: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        fn other_dept_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(self.other_dept),
                false,
                ScopeKind::Dept,
                vec![self.other_dept],
                false,
            )
        }

        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        /// 造成本对象（状态 / 完工 / 在制 / 对象级完工率由用例给）。
        async fn object(&self, name: &str, status: &str, completed: &str, wip: &str, pct: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  completion_pct, material_input_mode, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 800, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, 'ONE_TIME', \
                             ?::uuid, ?, '{}-01', ?, {closed_at}, now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB-M521-{}", self.tag(name))),
                    Value::from(&self.product),
                    Value::Text(format!(
                        "LOT-M521-{name}-{}",
                        crate::test_seed::last_chars(&self.suffix, 6)
                    )),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("objects").push(id);
            id
        }

        /// 造一张已结算的成本批次 + 挂到该对象的计算单（状态闸用）。
        async fn settle_period(&self, period: &str, object: Uuid) {
            let batch = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, \
                                       approval_instance_id, settled_at, finish_voucher_id, \
                                       created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'SETTLED', ?::uuid, now(), ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&batch),
                    Value::Text(period.to_string()),
                    Value::Text(format!("CS-M521-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                    Value::from(&Uuid::now_v7()),
                    Value::from(&Uuid::now_v7()),
                ],
            )
            .await
            .expect("造已结算批次夹具");
            self.periods_created.lock().expect("periods").push(batch);

            let (equiv_material, equiv_conversion) =
                crate::business::erp::cost::cost_period_item_dto::equivalents(dec("10"), dec("40"), dec("0"));
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                    completed_qty, wip_end_qty, completion_pct, \
                                                    equiv_material, equiv_conversion, status, \
                                                    stock_in_id, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 10, 40, 0, \
                         ?::text::numeric, ?::text::numeric, 'SETTLED', ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&batch),
                    Value::from(&object),
                    Value::from(&self.product),
                    Value::Text(equiv_material.round_dp(4).to_string()),
                    Value::Text(equiv_conversion.round_dp(4).to_string()),
                    Value::from(&Uuid::now_v7()),
                ],
            )
            .await
            .expect("造计算单夹具");
        }

        /// 读对象级完工率（真库值，不信出参）。
        async fn object_pct(&self, object: Uuid) -> Decimal {
            db().scalar_with_params::<Decimal>(
                "SELECT completion_pct FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("查对象级完工率")
        }

        /// 工序行数（活行 / 含软删）。
        async fn row_counts(&self, object: Uuid) -> (i64, i64) {
            let live: i64 = db()
                .scalar_with_params(
                    "SELECT count(*) FROM biz_cost_object_operation \
                      WHERE cost_object_id = ?::uuid AND deleted_at IS NULL",
                    vec![Value::from(&object)],
                )
                .await
                .expect("数活行");
            let all: i64 = db()
                .scalar_with_params(
                    "SELECT count(*) FROM biz_cost_object_operation WHERE cost_object_id = ?::uuid",
                    vec![Value::from(&object)],
                )
                .await
                .expect("数总行");
            (live, all)
        }

        /// 某对象相关的审计行数（工序行 + 对象级）。
        async fn audit_rows(&self, object: Uuid) -> i64 {
            db().scalar_with_params::<i64>(
                "SELECT count(*) FROM sys_change_log \
                  WHERE resource = 'biz_cost_object_operation' \
                    AND resource_id IN (SELECT id::text FROM biz_cost_object_operation WHERE cost_object_id = ?::uuid)",
                vec![Value::from(&object)],
            )
            .await
            .expect("数审计行")
        }

        /// 清掉该期间槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let first = &periods[0];
            let third = &periods[2];
            for statement in [
                "DELETE FROM biz_cost_object_operation WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M521-%' \
                      AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M521-%' \
                      AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M521-%' \
                    AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')",
            ] {
                let _ = db()
                    .exec_sql(
                        statement.to_string(),
                        vec![Value::Text(first.clone()), Value::Text(first.clone())],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M521-%' \
                        AND period_code BETWEEN ? AND ?"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(third.clone())],
                )
                .await;
        }

        /// 清场（FK 顺序：工序 → 审计 → 计算单 → 期间批次 → 对象）。
        async fn cleanup(&self) {
            let objects = self.objects.lock().expect("objects").clone();
            let periods = self.periods_created.lock().expect("periods").clone();
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if objects.is_empty() {
                return;
            }
            let objects_sql = list(&objects);
            db().exec_sql(
                format!(
                    "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object_operation' \
                       AND resource_id IN (SELECT id::text FROM biz_cost_object_operation \
                                            WHERE cost_object_id IN ({objects_sql}))"
                ),
                vec![],
            )
            .await
            .expect("清场（工序审计）");
            db().exec_sql(
                format!(
                    "DELETE FROM sys_change_log WHERE resource = 'biz_cost_object' \
                       AND resource_id IN ({})",
                    objects.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(",")
                ),
                vec![],
            )
            .await
            .expect("清场（对象审计）");
            for statement in [
                format!("DELETE FROM biz_cost_object_operation WHERE cost_object_id IN ({objects_sql})"),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({objects_sql})"),
            ] {
                db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
            }
            if !periods.is_empty() {
                db().exec_sql(
                    format!("DELETE FROM biz_cost_period WHERE id IN ({})", list(&periods)),
                    vec![],
                )
                .await
                .expect("清场（期间批次）");
            }
            db().exec_sql(
                format!("DELETE FROM biz_cost_object WHERE id IN ({objects_sql})"),
                vec![],
            )
            .await
            .expect("清场（成本对象）");
        }

        /// 本夹具残留行数（按对象号后缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            let pattern = Value::Text(format!("%{}", self.tag("")));
            db().scalar_with_params::<i64>(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE ?) + \
                        (SELECT count(*) FROM biz_cost_object_operation WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) + \
                        (SELECT count(*) FROM biz_cost_period_item WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![pattern.clone(), pattern.clone(), pattern],
            )
            .await
            .expect("查残留")
        }
    }

    // ---------- 入参构造 ----------

    fn row(seq: i32, name: &str, weight: Option<&str>, pct: &str) -> OperationRowReq {
        OperationRowReq {
            seq: Some(seq),
            operation_name: Some(name.to_string()),
            weight: weight.map(dec),
            completion_pct: Some(dec(pct)),
            remark: None,
        }
    }

    fn request(rows: Vec<OperationRowReq>) -> OperationSaveReq {
        OperationSaveReq { rows }
    }

    /// 三个工序的标准夹具（权重 1 / 2 / 1，完工率 100 / 50 / 0 ⇒ 综合 50%）。
    fn standard_rows() -> Vec<OperationRowReq> {
        vec![
            row(1, "下料", Some("1"), "100"),
            row(2, "焊接", Some("2"), "50"),
            row(3, "装配", Some("1"), "0"),
        ]
    }

    async fn save(principal: &ScopedPrincipal, object: Uuid, rows: Vec<OperationRowReq>) -> dto::OperationRes {
        let ctx = ctx();
        save_operations(principal, &ctx, &object.to_string(), request(rows))
            .await
            .expect("保存工序完工率")
    }

    // ==================== 用例 ====================

    /// 1. 按工序录入 → 加权汇总 → 同一事务回写对象级完工率。
    #[tokio::test]
    async fn v1_weighted_rollup_writes_back_to_object() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V1", "OPEN", "10", "40", "0").await;

        let res = save(&principal, object, standard_rows()).await;
        assert!(res.changed, "首次保存必须有写入");
        assert_eq!((res.inserted, res.updated, res.deleted), (3, 0, 0));
        assert_eq!(res.rows.len(), 3);
        assert_eq!(res.rows.iter().map(|row| row.seq).collect::<Vec<_>>(), vec![1, 2, 3]);

        let rollup = res.rollup.expect("三道工序必须有汇总");
        assert_eq!(rollup.weight_sum, "4");
        assert_eq!(rollup.weighted_sum, "200", "1×100 + 2×50 + 1×0 = 200");
        assert_eq!(rollup.pct, "50", "200 / 4 = 50%");
        assert!(rollup.matched, "保存后对象级与汇总必须一致");
        assert_eq!(rollup.diff, "0.0000");
        assert_eq!(res.object.completion_pct, "50");

        // 真库值（不信出参）：对象级已回写
        assert_eq!(fixture.object_pct(object).await, dec("50"));

        // 逐行占比 / 贡献可核：权重 2 的那道工序占 50%、贡献 25 个百分点
        let welding = res.rows.iter().find(|row| row.seq == 2).expect("焊接");
        assert_eq!(welding.share_pct, "50.0000");
        assert_eq!(welding.contribution_pct, "25.0000");
        let total: Decimal = res.rows.iter().map(|row| dec(&row.contribution_pct)).sum();
        assert_eq!(total, dec("50"), "逐行贡献之和 = 综合完工率");

        // 审计：3 行工序新增 + 1 行对象级完工率
        assert_eq!(fixture.audit_rows(object).await, 3, "三道工序各一条新增审计");
        let object_audit: i64 = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM sys_change_log \
                  WHERE resource = 'biz_cost_object' AND resource_id = ? AND action_label = ?",
                vec![
                    Value::Text(object.to_string()),
                    Value::Text(dto::ACTION_LABEL.to_string()),
                ],
            )
            .await
            .expect("数对象级审计");
        assert_eq!(object_audit, 1, "对象级完工率回写要留一条审计（§8.3 R-4 口径）");

        // GET 走同一条读路径，数字必须与保存回单一致
        let read = operations_of(&principal, &object.to_string()).await.expect("读工序");
        assert_eq!(read.rollup.expect("有工序").pct, "50");
        assert!(!read.changed);
        assert_eq!(read.state, "editable");

        fixture.cleanup().await;
    }

    /// 2. 幂等重放：同一份 payload 再保存不写库、不写审计。
    #[tokio::test]
    async fn v2_replay_is_idempotent() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V2", "OPEN", "10", "40", "0").await;

        let first = save(&principal, object, standard_rows()).await;
        assert!(first.changed);
        let audit_after_first = fixture.audit_rows(object).await;
        let updated_at: String = db()
            .scalar_with_params::<String>(
                "SELECT max(updated_at)::text FROM biz_cost_object_operation WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("取 updated_at");

        let second = save(&principal, object, standard_rows()).await;
        assert!(!second.changed, "同一份 payload 再保存不应算修改");
        assert_eq!((second.inserted, second.updated, second.deleted), (0, 0, 0));
        assert_eq!(second.rollup.expect("有工序").pct, "50");
        assert_eq!(
            fixture.audit_rows(object).await,
            audit_after_first,
            "幂等重放不写空审计"
        );

        let updated_at_after: String = db()
            .scalar_with_params::<String>(
                "SELECT max(updated_at)::text FROM biz_cost_object_operation WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("取 updated_at");
        assert_eq!(updated_at, updated_at_after, "没改动就不该碰 updated_at");

        fixture.cleanup().await;
    }

    /// 3. 整表覆盖：payload 少一个 seq ⇒ 软删，对象级按新集合重算。
    #[tokio::test]
    async fn v3_missing_seq_is_soft_deleted_and_pct_recomputed() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V3", "OPEN", "10", "40", "0").await;

        save(&principal, object, standard_rows()).await;
        assert_eq!(fixture.row_counts(object).await, (3, 3));

        // 删掉 3 号装配（payload 不再给 seq=3）
        let trimmed = vec![row(1, "下料", Some("1"), "100"), row(2, "焊接", Some("2"), "50")];
        let res = save(&principal, object, trimmed).await;
        assert_eq!(
            (res.inserted, res.updated, res.deleted),
            (0, 0, 1),
            "少的那道工序被软删"
        );
        assert_eq!(res.rows.len(), 2);
        let rollup = res.rollup.expect("还有两道工序");
        assert_eq!(rollup.weight_sum, "3");
        assert_eq!(rollup.pct, "66.67", "(100×1 + 50×2) / 3 = 66.666… → 66.67");
        assert_eq!(fixture.object_pct(object).await, dec("66.67"));

        // 库里：活行 2、总行 3（软删行留着审计）
        assert_eq!(fixture.row_counts(object).await, (2, 3));

        fixture.cleanup().await;
    }

    /// 4. 状态闸：已关批 / 已结算期间都 409，且零写入。
    #[tokio::test]
    async fn v4_closed_and_settled_objects_are_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();

        let closed = fixture.object("V4-CLOSED", "CLOSED", "800", "0", "100").await;
        let error = save_operations(&principal, &ctx(), &closed.to_string(), request(standard_rows()))
            .await
            .expect_err("已关批必须拒绝");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("已关批"), "{}", error.msg);
        assert_eq!(fixture.row_counts(closed).await, (0, 0), "拒绝时一行都不能写");

        let settled = fixture.object("V4-SETTLED", "OPEN", "10", "40", "0").await;
        fixture.settle_period(&fixture.periods[0], settled).await;
        let error = save_operations(&principal, &ctx(), &settled.to_string(), request(standard_rows()))
            .await
            .expect_err("已结算期间必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(
            error.code,
            crate::business::erp::cost::cost_object_dto::PERIOD_LOCKED_CODE
        );
        assert!(
            error.msg.contains(&fixture.periods[0]),
            "报文要点名已结算期间：{}",
            error.msg
        );
        assert_eq!(fixture.row_counts(settled).await, (0, 0), "拒绝时一行都不能写");

        fixture.cleanup().await;
    }

    /// 5. 数据范围：别车间看不见也写不了（404，不是 403）。
    #[tokio::test]
    async fn v5_other_workshop_cannot_see_or_write() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let owner = fixture.super_principal();
        let outsider = fixture.other_dept_principal();
        let object = fixture.object("V5", "OPEN", "10", "40", "0").await;

        let error = operations_of(&outsider, &object.to_string())
            .await
            .expect_err("别车间不能读");
        assert_eq!(error.status, 404, "单条入口不可见 = 404（不当探测口）");

        let error = save_operations(&outsider, &ctx(), &object.to_string(), request(standard_rows()))
            .await
            .expect_err("别车间不能写");
        assert_eq!(error.status, 404);
        assert_eq!(fixture.row_counts(object).await, (0, 0), "越权写入必须零落库");

        // 本车间自己能读能写（对照组，证明 404 来自范围而不是对象不存在）
        save(&owner, object, standard_rows()).await;
        assert_eq!(fixture.row_counts(object).await, (3, 3));

        fixture.cleanup().await;
    }

    /// 6. 录入侧值域：在制 > 0 且汇总 0 / 重复 seq / 3 位小数全部 400。
    #[tokio::test]
    async fn v6_wip_without_progress_and_bad_rows_are_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V6", "OPEN", "10", "40", "0").await;

        // 在制 40 但所有工序 0% ⇒ 400（否则跑计算会撞 66067）
        let all_zero = vec![row(1, "下料", Some("1"), "0"), row(2, "焊接", Some("1"), "0")];
        let error = save_operations(&principal, &ctx(), &object.to_string(), request(all_zero))
            .await
            .expect_err("在制 > 0 且进度 0 必须拒绝");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("66067"), "报文要点名 66067：{}", error.msg);

        let duplicates = vec![row(1, "下料", Some("1"), "50"), row(1, "焊接", Some("1"), "50")];
        let error = save_operations(&principal, &ctx(), &object.to_string(), request(duplicates))
            .await
            .expect_err("重复 seq 必须拒绝");
        assert_eq!(error.status, 400);

        let fine_scale = vec![row(1, "下料", Some("1"), "33.333")];
        let error = save_operations(&principal, &ctx(), &object.to_string(), request(fine_scale))
            .await
            .expect_err("3 位小数必须拒绝");
        assert_eq!(error.status, 400);

        assert_eq!(fixture.row_counts(object).await, (0, 0), "全部被拦，零落库");
        assert_eq!(fixture.object_pct(object).await, Decimal::ZERO, "对象级也不能被改");

        fixture.cleanup().await;
    }

    /// 7. 清空工序 = 全软删；对象级保持最后的值（回到手填模式，不归零）。
    #[tokio::test]
    async fn v7_clearing_operations_keeps_object_pct() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V7", "OPEN", "10", "40", "0").await;

        save(&principal, object, standard_rows()).await;
        assert_eq!(fixture.object_pct(object).await, dec("50"));

        let res = save(&principal, object, Vec::new()).await;
        assert_eq!((res.inserted, res.updated, res.deleted), (0, 0, 3), "三道工序全部软删");
        assert!(res.changed);
        assert!(res.rollup.is_none(), "没有工序就没有汇总");
        assert_eq!(res.object.completion_pct, "50", "对象级保持最后的值");
        assert_eq!(fixture.row_counts(object).await, (0, 3), "活行 0、软删行 3");
        assert!(res.note.contains("还没有工序"), "{}", res.note);
        assert_eq!(fixture.object_pct(object).await, dec("50"), "库里也不能被归零");

        fixture.cleanup().await;
    }

    /// 8. 有人用 `PUT …/output` 直接改对象级 ⇒ GET 报「不一致」。
    #[tokio::test]
    async fn v8_out_of_band_object_pct_shows_drift() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("V8", "OPEN", "10", "40", "0").await;
        save(&principal, object, standard_rows()).await;

        // 模拟「绕过工序直接改对象级」（产量录入面板改完工率）
        db().exec_sql(
            "UPDATE biz_cost_object SET completion_pct = 80 WHERE id = ?::uuid".to_string(),
            vec![Value::from(&object)],
        )
        .await
        .expect("改对象级夹具");

        let res = operations_of(&principal, &object.to_string()).await.expect("读工序");
        let rollup = res.rollup.expect("有工序");
        assert_eq!(rollup.pct, "50");
        assert_eq!(rollup.object_pct, "80");
        assert!(!rollup.matched);
        assert_eq!(rollup.diff, "30.0000");
        assert!(
            res.warnings.iter().any(|text| text.contains("不一致")),
            "必须提醒两侧不一致：{:?}",
            res.warnings
        );

        // 再保存一次（哪怕内容没变也不回写？—— 内容没变 → 不写库；显式改一行才回写）
        // 这里改一道工序的进度触发回写：80% 被拉回 50%
        let mut tweak = standard_rows();
        tweak[0].completion_pct = Some(dec("100"));
        let saved = save(&principal, object, tweak).await;
        assert_eq!(saved.rollup.expect("有工序").pct, "50");
        assert_eq!(fixture.object_pct(object).await, dec("50"), "保存一次即回写一致");

        fixture.cleanup().await;
    }

    /// 9. 清场零残留。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let principal = fixture.super_principal();
        let object = fixture.object("ZZ", "OPEN", "10", "40", "0").await;
        save(&principal, object, standard_rows()).await;
        fixture.cleanup().await;

        assert_eq!(fixture.residue().await, 0, "清场后本卡造的行必须全 0");
        let orphan_audit: i64 = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM sys_change_log WHERE resource = 'biz_cost_object_operation' \
                   AND resource_id NOT IN (SELECT id::text FROM biz_cost_object_operation)",
                vec![],
            )
            .await
            .expect("数孤儿审计");
        assert_eq!(orphan_audit, 0, "工序审计不能留孤儿行");
    }
}
