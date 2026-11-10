//! 工时台账（`biz_cost_labor`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_costb21 \
//!   cargo test --lib cost_labor_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`crud_confirm_flow_end_to_end`] | C / R / U / 确认全通；`*_id` 都带 `*_name`；金额 = 工时×费率 |
//! | 2 | [`confirm_twice_does_not_double`] | **验收项**「确认幂等（重复确认不翻倍）」：两次确认后仍是 1 行、金额不变、`updated_at` 不动 |
//! | 3 | [`db_check_maps_amount_mismatch_to_66058`] | **验收项**「金额 CHECK 触发 → 工时×费率与金额不符」：绕过 handler 直插 → 库抛 23514 → 翻译成 400 + 66058 |
//! | 4 | [`repeating_rate_rounds_to_four_decimals`] | 真实工资率除不尽（33.333333）也要能插进去，金额按 4 位四舍五入 |
//! | 5 | [`object_must_exist_and_be_open`] | 对象不存在 → 400/66002；已关批 → 409/66057 |
//! | 6 | [`confirmed_row_is_readonly`] | 已确认的行改 / 删都是 409 |
//! | 7 | [`soft_deleted_draft_disappears_from_list`] | 软删后列表查不到（`deleted_at` 生效） |
//! | 8 | [`period_and_work_date_must_agree`] | 日期不在所选期间 → 400 |
//! | 9 | [`declared_amount_mismatch_is_rejected_at_entry`] | 客户端传错金额 → 400 + 66058（不落到库层） |
//! | 10 | [`list_is_scoped_by_dept`] | 数据权限：别的部门看不见（`scope::paginate` 真的裁剪了） |
//!
//! # 夹具纪律
//!
//! 全部用 `costb21-<唯一后缀>` 前缀造，清场**按 id** 删（绝不按前缀扫全库）；
//! 删表顺序按 FK：工时 → 成本对象 → 物料 / 仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_labor_dto::{self, CostLaborCreateReq, CostLaborPageQuery, CostLaborUpdateReq};
    use crate::business::erp::cost_labor_handler as handler;
    use crate::business::erp::cost_labor_model as model;
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
        Decimal::from_str(text).expect("金额 / 工时字面量")
    }

    /// 唯一后缀（纳秒 + 进程内计数器，避免并发用例互相踩）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:03}", now % 1_000_000_000)
    }

    /// 仓库编码的尾巴（`biz_warehouse.warehouse_code` 有长度上限）。
    fn fx_suffix_code(suffix: &str) -> String {
        let start = suffix.len().saturating_sub(10);
        suffix[start..].to_string()
    }

    /// 全套夹具：产出品 / 仓库 / 车间（外加**第二个**车间，用来验数据权限裁剪）。
    struct Fixture {
        suffix: String,
        product: Uuid,
        warehouse: Uuid,
        dept: Uuid,
        other_dept: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let dept = crate::shared::test_helpers::ensure_any_dept().await;
            // 第二个部门：数据权限用例要验「别的部门看不见」。
            // 空库重放出来的库里 sys_dept 只有 1 行，所以自己造一个（清场时删掉）。
            let other_dept = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 1, 0, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&other_dept),
                    Value::Text(format!("FX21-{suffix}")),
                    Value::Text(format!("工时夹具旁部门 {suffix}")),
                ],
            )
            .await
            .expect("造第二个部门夹具");
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
                    Value::Text(format!("COSTB21-MAT-{suffix}")),
                    Value::Text(format!("工时夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB21W{}", fx_suffix_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("工时夹具仓 {suffix}")),
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
                other_dept,
            }
        }

        /// 直接造一张成本对象（避免依赖 B1-8 的取号 / 批次逻辑）。
        ///
        /// `status='CLOSED'` 必须同时给 `closed_at`（`ck_biz_cost_object_closed`），
        /// 所以这里按状态决定 `closed_at` 的值。
        async fn object_in(&self, dept: Uuid, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?::uuid, ?::uuid, 100, ?::uuid, '工时夹具车间', '2026-11-01', ?, \
                             {closed_at}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB21-{}-{}", &id.to_string()[..8], self.suffix)),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::from(&dept),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 数据范围主体：`is_super=false` 才会真正注入部门过滤（超管恒真）。
        fn principal(&self, kind: ScopeKind, dept: Option<Uuid>, is_super: bool) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), dept, is_super, kind, Vec::new(), false)
        }

        fn create_req(&self, object: Uuid, dept: Uuid) -> CostLaborCreateReq {
            CostLaborCreateReq {
                period_code: Some("2026-11".to_string()),
                work_date: Some("2026-11-06".to_string()),
                cost_object_id: Some(object.to_string()),
                employee_id: None,
                employee_name: Some("张三".to_string()),
                dept_id: Some(dept.to_string()),
                hours: Some(dec("8")),
                machine_hours: None,
                rate: Some(dec("50")),
                amount: None,
                remark: Some(format!("B2-1 真库用例 {}", self.suffix)),
            }
        }

        /// 清场：按 id 删（FK 顺序：工时 → 成本对象 → 物料 / 仓库）。
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
                format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_object WHERE id IN ({list})"),
                format!("DELETE FROM biz_material WHERE id = '{}'::uuid", self.product),
                format!("DELETE FROM biz_warehouse WHERE id = '{}'::uuid", self.warehouse),
                format!("DELETE FROM sys_dept WHERE id = '{}'::uuid", self.other_dept),
            ] {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }
    }

    /// 某个对象下的活工时行数（幂等断言用）。
    async fn live_rows(object: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_cost_labor \
              WHERE cost_object_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&object)],
        )
        .await
        .expect("数工时行")
    }

    // ==================== 用例 ====================

    /// ① 建 / 列表 / 改 / 确认全通，名字与金额口径正确。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn crud_confirm_flow_end_to_end() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;

        let created = handler::create_labor(None, fx.create_req(object, fx.dept))
            .await
            .expect("建工时");
        assert_eq!(created.status, "DRAFT");
        assert_eq!(created.source_type, "MANUAL");
        // 金额 / 工时 / 费率是**字符串**出参，标度与库列一致
        // （amount NUMERIC(18,4) → "400.0000"；hours NUMERIC(10,2) → "8.00"；
        //  与 `cost_object` 的 plan_qty "100.0000" 同一口径）
        assert_eq!(created.amount, "400.0000", "金额 = 8 × 50（服务端算，客户端不填）");
        assert_eq!(created.hours, "8.00");
        // 约定 13：*_id 都带 *_name
        assert!(
            created
                .cost_object_no
                .as_deref()
                .is_some_and(|no| no.starts_with("CB21-")),
            "{:?}",
            created.cost_object_no
        );
        assert_eq!(
            created.product_name.as_deref(),
            Some(&*format!("工时夹具产出品 {}", fx.suffix))
        );
        assert!(created.dept_name.as_deref().is_some_and(|name| !name.is_empty()));
        assert_eq!(created.cost_object_status.as_deref(), Some("OPEN"));

        // 列表：按对象 + 期间筛
        let page = handler::page_labors(
            &fx.principal(ScopeKind::All, Some(fx.dept), true),
            CostLaborPageQuery {
                period_code: Some("2026-11".to_string()),
                cost_object_id: Some(object.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("列表");
        assert_eq!(page.total, 1, "按对象 + 期间应命中 1 条");
        assert_eq!(page.list[0].id, created.id);
        assert_eq!(page.list[0].amount, "400.0000");

        // 改单：只改工时 ⇒ 金额跟着重算（9.5 × 50 = 475）
        let updated = handler::update_labor(
            None,
            &created.id,
            CostLaborUpdateReq {
                hours: Some(dec("9.5")),
                ..Default::default()
            },
        )
        .await
        .expect("改工时");
        assert_eq!(updated.hours, "9.50");
        assert_eq!(updated.amount, "475.0000", "改了工时金额必须重算");

        // 改单：改费率 ⇒ 金额同样重算
        let updated = handler::update_labor(
            None,
            &created.id,
            CostLaborUpdateReq {
                rate: Some(dec("60")),
                ..Default::default()
            },
        )
        .await
        .expect("改费率");
        assert_eq!(updated.amount, "570.0000", "9.5 × 60");

        // 确认
        let confirmed = handler::confirm_labor(None, &created.id).await.expect("确认");
        assert_eq!(confirmed.status, "CONFIRMED");
        assert_eq!(confirmed.amount, "570.0000", "确认不许动金额");

        let id = Uuid::parse_str(&created.id).expect("id");
        fx.cleanup(&[object, id]).await;
    }

    /// ② **验收项**：重复确认不翻倍（行数 / 金额 / updated_at 三重断言）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_twice_does_not_double() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let created = handler::create_labor(None, fx.create_req(object, fx.dept))
            .await
            .expect("建工时");
        let id = Uuid::parse_str(&created.id).expect("id");

        let first = handler::confirm_labor(None, &created.id).await.expect("第一次确认");
        assert_eq!(first.status, "CONFIRMED");
        let after_first = model::find_by_id(id).await.expect("查行").expect("行在");

        let second = handler::confirm_labor(None, &created.id).await.expect("第二次确认");
        let after_second = model::find_by_id(id).await.expect("查行").expect("行在");

        // ① 只可能有一行（确认不建新行）
        assert_eq!(live_rows(object).await, 1, "重复确认不许产生第二行");
        // ② 金额不变（不翻倍）
        assert_eq!(second.amount, first.amount, "重复确认金额不许变");
        assert_eq!(after_second.amount, after_first.amount);
        assert_eq!(after_second.amount, dec("400"), "仍是 8 × 50");
        // ③ 第二次确认不写库 ⇒ updated_at 不动
        assert_eq!(
            after_second.updated_at, after_first.updated_at,
            "幂等返回不该改 updated_at（说明第二次没进写路径）"
        );
        assert_eq!(after_second.status, "CONFIRMED");

        // ④ 第三次也一样（判定是纯函数，不累积）
        let third = handler::confirm_labor(None, &created.id).await.expect("第三次确认");
        assert_eq!(third.amount, "400.0000");
        assert_eq!(live_rows(object).await, 1);

        // ⑤ 已确认后改 / 删都被拦（幂等不等于可写）
        let update_error = handler::update_labor(
            None,
            &created.id,
            CostLaborUpdateReq {
                hours: Some(dec("10")),
                ..Default::default()
            },
        )
        .await
        .expect_err("已确认不许改");
        assert_eq!(update_error.status, 409);
        let delete_error = handler::delete_labor(None, &created.id)
            .await
            .expect_err("已确认不许删");
        assert_eq!(delete_error.status, 409);

        fx.cleanup(&[object]).await;
    }

    /// ③ **验收项**：绕过 handler 直插错金额 ⇒ 库的 23514 被翻译成「工时×费率与金额不符」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn db_check_maps_amount_mismatch_to_66058() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;

        // 直接插一行 amount=401（hours=8 × rate=50 应为 400）
        let raw: DbResult<u64> = db()
            .exec_sql(
                "INSERT INTO biz_cost_labor (period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             remark) \
                 VALUES ('2026-11', '2026-11-06', ?::uuid, '直插张三', ?::uuid, '工时夹具车间', \
                         8, 0, 50, 401, ?)"
                    .to_string(),
                vec![
                    Value::from(&object),
                    Value::from(&fx.dept),
                    Value::Text(format!("B2-1 直插 {}", fx.suffix)),
                ],
            )
            .await;
        let err = raw.expect_err("amount != hours × rate 必须被库拦下");
        assert!(
            matches!(
                &err,
                DbError::CheckViolation { constraint, .. }
                    if constraint.as_deref() == Some("ck_biz_cost_labor_amount")
            ),
            "应当是 ck_biz_cost_labor_amount 的 CheckViolation，实际：{err}"
        );

        // 翻译成对外的 400 + 66058「工时×费率与金额不符」
        let spec = cost_labor_dto::spec_from_create(&fx.create_req(object, fx.dept), "工时夹具车间").expect("规格");
        let error = cost_labor_dto::map_write_error(err, &spec, "真库用例");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66058);
        assert!(error.msg.contains("工时×费率与金额不符"), "{}", error.msg);
        assert!(error.msg.contains("400"), "要点名期望值：{}", error.msg);

        // 库里没有留下脏行
        assert_eq!(live_rows(object).await, 0, "被拦的行不该落库");

        fx.cleanup(&[object]).await;
    }

    /// ④ 真实工资率除不尽：金额按 4 位四舍五入，能插进去（scale=2 会恒假）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repeating_rate_rounds_to_four_decimals() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;

        let mut req = fx.create_req(object, fx.dept);
        req.hours = Some(dec("1"));
        req.rate = Some(dec("33.333333"));
        let created = handler::create_labor(None, req).await.expect("除不尽的工资率");
        assert_eq!(
            created.amount, "33.3333",
            "round(1 × 33.333333, 4) = 33.3333（SQL 与 CHECK 同引擎求出）"
        );

        // 客户端拿 Rust 算出的期望值回传核对 ⇒ 必须被接受（两侧舍入一致）
        let mut req = fx.create_req(object, fx.dept);
        req.hours = Some(dec("1"));
        req.rate = Some(dec("33.333333"));
        req.amount = Some(dec("33.3333"));
        assert!(
            handler::create_labor(None, req).await.is_ok(),
            "Rust 算出的期望值与库算出的一致，应放行"
        );

        fx.cleanup(&[object]).await;
    }

    /// ⑤ 成本对象必须存在且 `OPEN`：不存在 → 400/66002，已关批 → 409/66057。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn object_must_exist_and_be_open() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        // 不存在
        let missing = Uuid::now_v7();
        let error = handler::create_labor(None, fx.create_req(missing, fx.dept))
            .await
            .expect_err("对象不存在");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66002);
        assert!(error.msg.contains(&missing.to_string()), "{}", error.msg);

        // 已关批
        let closed = fx.object_in(fx.dept, "CLOSED").await;
        let error = handler::create_labor(None, fx.create_req(closed, fx.dept))
            .await
            .expect_err("已关批");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66057);
        assert!(error.msg.contains("已关批"), "{}", error.msg);
        assert!(error.msg.contains("CB21-"), "要点名对象号：{}", error.msg);

        fx.cleanup(&[closed]).await;
    }

    /// ⑥ 已确认的行改 / 删都是 409（不是 500，也不是静默成功）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirmed_row_is_readonly() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let created = handler::create_labor(None, fx.create_req(object, fx.dept))
            .await
            .expect("建工时");
        handler::confirm_labor(None, &created.id).await.expect("确认");

        let update_error = handler::update_labor(
            None,
            &created.id,
            CostLaborUpdateReq {
                hours: Some(dec("12")),
                ..Default::default()
            },
        )
        .await
        .expect_err("已确认不许改");
        assert_eq!(update_error.status, 409);
        assert!(update_error.msg.contains("CONFIRMED"), "{}", update_error.msg);

        let delete_error = handler::delete_labor(None, &created.id)
            .await
            .expect_err("已确认不许删");
        assert_eq!(delete_error.status, 409);

        // 值没被改
        let id = Uuid::parse_str(&created.id).expect("id");
        let row = model::find_by_id(id).await.expect("查行").expect("行在");
        assert_eq!(row.hours, dec("8"));

        fx.cleanup(&[object]).await;
    }

    /// ⑦ 软删：草稿删掉后列表查不到、行还留痕（`deleted_at` 非空）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn soft_deleted_draft_disappears_from_list() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let created = handler::create_labor(None, fx.create_req(object, fx.dept))
            .await
            .expect("建工时");
        handler::delete_labor(None, &created.id).await.expect("删草稿");

        assert_eq!(live_rows(object).await, 0, "软删后活行应为 0");
        let page = handler::page_labors(
            &fx.principal(ScopeKind::All, Some(fx.dept), true),
            CostLaborPageQuery {
                cost_object_id: Some(object.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("列表");
        assert_eq!(page.total, 0, "软删的行不该出现在列表里");

        // 行还在（审计留痕）
        let id = Uuid::parse_str(&created.id).expect("id");
        let gone = model::find_by_id(id).await.expect("查行");
        assert!(gone.is_none(), "find_by_id 不该再返回软删行");

        fx.cleanup(&[object]).await;
    }

    /// ⑧ 日期必须落在所选期间内（设计 §4.5.3）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn period_and_work_date_must_agree() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;

        let mut req = fx.create_req(object, fx.dept);
        req.work_date = Some("2026-12-01".to_string());
        let error = handler::create_labor(None, req).await.expect_err("跨月");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不在所选期间"), "{}", error.msg);

        fx.cleanup(&[object]).await;
    }

    /// ⑨ 客户端传错金额：入口就 400 + 66058，不落到库层。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn declared_amount_mismatch_is_rejected_at_entry() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;

        let mut req = fx.create_req(object, fx.dept);
        req.amount = Some(dec("401"));
        let error = handler::create_labor(None, req).await.expect_err("金额不符");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66058);
        assert!(error.msg.contains("工时×费率与金额不符"), "{}", error.msg);
        assert_eq!(live_rows(object).await, 0, "入口拦下的不该落库");

        fx.cleanup(&[object]).await;
    }

    /// ⑩ 数据权限：别的部门看不见本部门的工时行（`scope::paginate` 真在裁剪）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_is_scoped_by_dept() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        handler::create_labor(None, fx.create_req(object, fx.dept))
            .await
            .expect("建工时");

        // 超管（All）看得见
        let all = handler::page_labors(
            &fx.principal(ScopeKind::All, Some(fx.dept), true),
            CostLaborPageQuery {
                cost_object_id: Some(object.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("超管列表");
        assert_eq!(all.total, 1);

        // 只看**别的**部门 ⇒ 看不见（`dept_id` 是数据权限部门列）
        let other = handler::page_labors(
            &fx.principal(ScopeKind::Dept, Some(fx.other_dept), false),
            CostLaborPageQuery {
                cost_object_id: Some(object.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("别部门列表");
        assert_eq!(other.total, 0, "别的车间不该看见本车间的工时台账");

        // 本部门 ⇒ 看得见
        let mine = handler::page_labors(
            &fx.principal(ScopeKind::Dept, Some(fx.dept), false),
            CostLaborPageQuery {
                cost_object_id: Some(object.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("本部门列表");
        assert_eq!(mine.total, 1);

        fx.cleanup(&[object]).await;
    }
}
