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
//! ## B2-2 导入（xlsx）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 11 | [`import_writes_every_row_when_the_file_is_clean`] | 干净文件全部入库；金额 / 部门名快照 / `IMPORT+DRAFT` 逐列对得上 |
//! | 12 | [`one_bad_row_rejects_the_whole_batch_and_nothing_is_written`] | **验收项**「全有或全无 + 逐行错误」：一行错 → 零入库，行号/列名/原因齐 |
//! | 13 | [`closed_object_rejects_the_batch_with_a_business_reason`] | 已关批对象不可记工时（409 语义以行错误给出） |
//! | 14 | [`date_outside_selected_period_rejects_the_batch`] | §4.5.3 第 2 列：日期必须落在所选期间 |
//! | 15 | [`employee_number_must_resolve_against_employee_master`] | 工号必须解析到员工主数据，解析不到**报错**（ARCH-9-2 上线后口径反转，原名 `..._until_arch9_2_is_online`） |
//! | 16 | [`duplicated_dept_name_requires_the_code_instead_of_guessing`] | 部门重名 → 要求改填编码，不猜车间 |
//!
//! ## B2-3 费率取数（`biz_cost_rate` 三级优先级）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 17 | [`company_rate_fills_blank_rate`] | **验收项**「只有公司级」：留空取到公司级 + 出参回填来源；显式费率不被覆盖 |
//! | 18 | [`dept_rate_beats_company_rate`] | **验收项**「有车间级」：车间级压过公司级 |
//! | 19 | [`cost_object_rate_beats_dept_and_company`] | **验收项**「有对象级」：对象级压过车间级 / 公司级 |
//! | 20 | [`missing_rate_fails_loudly_and_writes_nothing`] | **验收项**「缺费率报错不按 0」：`422` + `66059` + 零落库 |
//! | 21 | [`another_depts_rate_is_not_reused`] | 别的车间的费率不许串味（报文说明有几条被跳过） |
//! | 22 | [`expired_future_and_disabled_rates_are_ignored`] | 生效期两端 + 停用状态都算「没命中」 |
//! | 23 | [`newest_effective_version_wins`] | 同一级里生效更晚的版本赢 |
//! | 24 | [`import_fills_blank_rate_per_row`] | 导入留空 ⇒ 逐行取数（车间级 55 而非公司级 40），金额按取到的费率算 |
//! | 25 | [`import_rejects_the_batch_when_rate_cannot_be_resolved`] | 导入三级全空 ⇒ 整批拒绝，错误落在「工资率」列 |
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

    // ==================== 导入（B2-2：xlsx 解析 + 全有或全无 + 逐行错误） ====================

    use crate::business::erp::cost_labor_import::{self as import, ImportContext, RawRow};

    /// 夹具对象的 `object_no`（导入表里填的就是这个号）。
    async fn object_no_of(object: Uuid) -> String {
        let rows = db()
            .query_sql(
                "SELECT object_no FROM biz_cost_object WHERE id = ?::uuid".to_string(),
                vec![Value::from(&object)],
            )
            .await
            .expect("查对象号");
        rows.first()
            .expect("对象存在")
            .get_typed("object_no")
            .expect("object_no")
    }

    /// 部门编码 / 名称（导入表里两种填法都要能用）。
    async fn dept_code_name(dept: Uuid) -> (String, String) {
        let rows = db()
            .query_sql(
                "SELECT dept_code, dept_name FROM sys_dept WHERE id = ?::uuid".to_string(),
                vec![Value::from(&dept)],
            )
            .await
            .expect("查部门");
        let row = rows.first().expect("部门存在");
        (
            row.get_typed("dept_code").expect("dept_code"),
            row.get_typed("dept_name").expect("dept_name"),
        )
    }

    /// 造一行导入数据（`line` 只是元数据：xlsx 写入器按数组顺序落行）。
    fn import_row(
        object_no: &str,
        dept: &str,
        date: &str,
        employee: &str,
        hours: &str,
        machine: &str,
        rate: &str,
    ) -> RawRow {
        RawRow {
            line: 0,
            object_no: object_no.to_string(),
            work_date: date.to_string(),
            employee_no: String::new(),
            employee_name: employee.to_string(),
            dept: dept.to_string(),
            hours: hours.to_string(),
            machine_hours: machine.to_string(),
            rate: rate.to_string(),
        }
    }

    /// 某个对象下导入进来的行（按日期排，便于逐字段断言）。
    #[allow(clippy::type_complexity)]
    async fn imported_rows(object: Uuid) -> Vec<(String, String, String, String, String, String, String, String)> {
        let rows = db()
            .query_sql(
                "SELECT work_date::text AS work_date, employee_name, dept_name, \
                        hours::text AS hours, machine_hours::text AS machine_hours, \
                        rate::text AS rate, amount::text AS amount, \
                        source_type || '/' || status AS flags \
                   FROM biz_cost_labor \
                  WHERE cost_object_id = ?::uuid AND deleted_at IS NULL \
                  ORDER BY work_date"
                    .to_string(),
                vec![Value::from(&object)],
            )
            .await
            .expect("查导入结果");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("work_date").expect("work_date"),
                    row.get_typed("employee_name").expect("employee_name"),
                    row.get_typed("dept_name").expect("dept_name"),
                    row.get_typed("hours").expect("hours"),
                    row.get_typed("machine_hours").expect("machine_hours"),
                    row.get_typed("rate").expect("rate"),
                    row.get_typed("amount").expect("amount"),
                    row.get_typed("flags").expect("flags"),
                )
            })
            .collect()
    }

    /// ⑪ **验收项**：干净文件 → 全部入库；金额 / 快照 / 来源 / 状态逐列对得上。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn import_writes_every_row_when_the_file_is_clean() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let no = object_no_of(object).await;
        let (code, name) = dept_code_name(fx.dept).await;

        let rows = vec![
            import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5"),
            // 部门填**编码**、机器工时留空 ⇒ 都要能用
            import_row(&no, &code, "2026-11-03", "王五", "8", "", "50"),
            // 费率除不尽 ⇒ 金额由库侧 round(hours*rate,4) 求值
            import_row(&no, &name, "2026-11-04", "赵六", "70", "0", "33.333333"),
        ];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(
            &bytes,
            &ImportContext {
                period_code: Some("2026-11".to_string()),
                operator: None,
            },
        )
        .await
        .expect("导入");
        assert_eq!(result.success, 3, "三行全通才入库");
        assert_eq!(result.failed, 0);
        assert!(result.rows.is_empty(), "成功时不该带错误明细");

        let stored = imported_rows(object).await;
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].0, "2026-11-02");
        assert_eq!(stored[0].1, "李四");
        assert_eq!(stored[0].2, name, "部门名快照来自 sys_dept");
        assert_eq!(stored[0].3, "7.50");
        assert_eq!(stored[0].4, "3.00");
        assert_eq!(stored[0].5, "48.500000");
        assert_eq!(stored[0].6, "363.7500", "7.5 × 48.5");
        assert_eq!(stored[0].7, "IMPORT/DRAFT", "导入落 DRAFT 等人工确认");

        assert_eq!(stored[1].4, "0.00", "机器工时留空 = 0");
        assert_eq!(stored[1].6, "400.0000", "8 × 50");

        assert_eq!(stored[2].6, "2333.3333", "round(70 × 33.333333, 4) 由库侧求值");

        fx.cleanup(&[object]).await;
    }

    /// ⑫ **验收项**：任何一行有错 → 整批拒绝（一条都不入库），返回行号 + 列名 + 原因。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_bad_row_rejects_the_whole_batch_and_nothing_is_written() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let no = object_no_of(object).await;
        let (_, name) = dept_code_name(fx.dept).await;

        let rows = vec![
            import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5"),
            // 第 3 行：成本对象号不存在
            import_row("CB-NOT-EXIST-0001", &name, "2026-11-03", "王五", "8", "", "50"),
            import_row(&no, &name, "2026-11-04", "赵六", "7", "", "50"),
        ];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入返回逐行错误（不是 500）");

        assert_eq!(result.success, 0, "全有或全无：一行有错就一条都不入库");
        assert_eq!(result.failed, 1, "只有一行有错");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].line, 3, "行号是 Excel 真实行号");
        assert_eq!(result.rows[0].column, "成本对象号");
        assert!(result.rows[0].message.contains("不存在"), "{}", result.rows[0].message);
        assert_eq!(live_rows(object).await, 0, "整批拒绝后库里不能有半批数据");

        fx.cleanup(&[object]).await;
    }

    /// ⑬ 已关批的成本对象不能记工时（同一批里混一行都不行）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closed_object_rejects_the_batch_with_a_business_reason() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let closed = fx.object_in(fx.dept, "CLOSED").await;
        let no = object_no_of(closed).await;
        let (_, name) = dept_code_name(fx.dept).await;

        let rows = vec![import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5")];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入");
        assert_eq!((result.success, result.failed), (0, 1));
        let message = &result.rows[0].message;
        assert!(message.contains("只有 OPEN"), "{message}");
        assert!(message.contains("已关批"), "要说清为什么不能记：{message}");
        assert_eq!(live_rows(closed).await, 0);

        fx.cleanup(&[closed]).await;
    }

    /// ⑭ 日期不在所选期间 → 整批拒绝，并指出该选哪个期间（§4.5.3 第 2 列）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn date_outside_selected_period_rejects_the_batch() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let no = object_no_of(object).await;
        let (_, name) = dept_code_name(fx.dept).await;

        let rows = vec![import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5")];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(
            &bytes,
            &ImportContext {
                period_code: Some("2026-10".to_string()),
                operator: None,
            },
        )
        .await
        .expect("导入");
        assert_eq!((result.success, result.failed), (0, 1));
        assert_eq!(result.rows[0].column, "日期");
        assert!(result.rows[0].message.contains("2026-10"), "{}", result.rows[0].message);
        assert!(
            result.rows[0].message.contains("请把期间选成 2026-11"),
            "{}",
            result.rows[0].message
        );
        assert_eq!(live_rows(object).await, 0);

        fx.cleanup(&[object]).await;
    }

    /// ⑮ 工号必须能解析到员工主数据（ARCH-9-2 已上线，口径 2026-11-13 反转）。
    ///
    /// 历史：这条用例原名 `employee_number_is_rejected_until_arch9_2_is_online`，
    /// 断言的是「`biz_employee` 还没上线 ⇒ 填了工号就报错，不静默丢掉」。
    /// HR 批（V0389 起）已把 `biz_employee` 落库，`employee_table_present()` 恒为真 ⇒
    /// 旧断言的前提不存在了（全量真库档必红）。
    ///
    /// 现在的口径（与 `cost_labor_import::attach_employee_issues` 的两个分支一一对应）：
    ///   · 员工主数据**在**（现行生产/全量库）⇒ 工号必须能在 `biz_employee` 解析到，
    ///     解析不到 ⇒ 该行报「在员工主数据里不存在」，整批拒绝、零落库；
    ///   · 员工主数据**不在**（ARCH-9-2 之前的库）⇒ 仍是「工号无处可存」的拒绝分支。
    ///     该分支无法在现行库上构造（不能为了跑一条用例去 DROP 生产同构的表），
    ///     所以本用例用 `employee_table_present()` **显式断言当前处于哪一支**，
    ///     而不是依赖「报错文案里含 ARCH9-2」这种会随版本过期的字符串。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn employee_number_must_resolve_against_employee_master() {
        if !ensure_db().await {
            return;
        }
        // 前置断言：库里的形态必须是「员工主数据已上线」，否则本用例的口径不成立
        let present = model::employee_table_present().await.expect("查 biz_employee 是否存在");
        assert!(
            present,
            "本用例的前提是 `biz_employee` 已存在（ARCH-9-2 已上线）；\
             库形态不符说明跑在旧库上，请换全量迁移库或另开用例覆盖「主数据未上线」分支"
        );

        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let no = object_no_of(object).await;
        let (_, name) = dept_code_name(fx.dept).await;

        let mut row = import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5");
        row.employee_no = "E1001".to_string();
        let bytes = import::build_rows_xlsx(&[row]).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入");
        assert_eq!((result.success, result.failed), (0, 1));
        assert_eq!(result.rows[0].column, "员工工号");
        assert!(
            result.rows[0].message.contains("在员工主数据里不存在"),
            "工号解析失败必须点名「不存在」而不是静默丢字段：{}",
            result.rows[0].message
        );
        assert!(
            result.rows[0].message.contains("E1001"),
            "报错要把工号回显出来（用户才知道改哪一行）：{}",
            result.rows[0].message
        );
        assert_eq!(live_rows(object).await, 0, "工号解析不到时整批不许落库");

        fx.cleanup(&[object]).await;
    }

    /// ⑯ 部门重名 → 要求改填部门编码（不猜是哪个车间）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicated_dept_name_requires_the_code_instead_of_guessing() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object_in(fx.dept, "OPEN").await;
        let no = object_no_of(object).await;
        let (_, name) = dept_code_name(fx.dept).await;

        // 再造一个与主夹具车间**同名**的部门
        let twin = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order, created_at, updated_at) \
             VALUES (?::uuid, ?, ?, 1, 0, now(), now())"
                .to_string(),
            vec![
                Value::from(&twin),
                Value::Text(format!("FXTW-{}", fx.suffix)),
                Value::Text(name.clone()),
            ],
        )
        .await
        .expect("造同名部门");

        let rows = vec![import_row(&no, &name, "2026-11-02", "李四", "7.5", "3", "48.5")];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入");
        assert_eq!((result.success, result.failed), (0, 1));
        assert_eq!(result.rows[0].column, "工作中心（部门）");
        assert!(
            result.rows[0].message.contains("改填部门编码"),
            "{}",
            result.rows[0].message
        );
        assert_eq!(live_rows(object).await, 0);

        let _ = db()
            .exec_sql(
                "DELETE FROM sys_dept WHERE id = ?::uuid".to_string(),
                vec![Value::from(&twin)],
            )
            .await;
        fx.cleanup(&[object]).await;
    }

    // ==================== B2-3 费率取数（对象 > 车间 > 公司） ====================
    //
    // 这一组用例必须**串行**跑：`uk_biz_cost_rate` 是
    // `(rate_type, scope, coalesce(scope_id, 0), effective_from)`，公司级行的
    // `scope_id` 恒空 ⇒ 两个用例同时插「公司级 + 2026-01-01」会撞唯一索引；
    // 更糟的是「缺费率报错」用例会被别的用例留下的公司级费率污染（恒有兜底）。
    // 所以用进程内互斥锁串行 + 前后各清一遍 B2-3 造的费率行（按 remark 前缀认）。

    use crate::business::erp::cost_rate_dto::{RATE_TYPE_LABOR, SCOPE_COMPANY, SCOPE_COST_OBJECT, SCOPE_DEPT};

    /// 费率用例的串行锁。
    static RATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 取锁（返回值必须活到用例结束）。
    async fn rate_lock() -> tokio::sync::MutexGuard<'static, ()> {
        RATE_LOCK.lock().await
    }

    /// B2-3 造的费率行标记（清场按它认，绝不按前缀扫全表）。
    const RATE_MARK: &str = "B2-3 真库用例";

    /// 清掉 B2-3 留下的费率行（用例前后各一次 ⇒ 重复跑也干净）。
    async fn wipe_test_rates() {
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_cost_rate WHERE remark LIKE '{RATE_MARK}%'"),
                vec![],
            )
            .await;
    }

    /// 造一个**本用例专用**的部门（不与其它用例 / 历史数据共享，避免费率串味）。
    async fn new_dept(suffix: &str, tag: &str) -> Uuid {
        let id = Uuid::now_v7();
        let tail = fx_suffix_code(suffix);
        db().exec_sql(
            "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order, created_at, updated_at) \
             VALUES (?::uuid, ?, ?, 1, 0, now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(format!("FXR{tag}{tail}")),
                Value::Text(format!("费率夹具车间 {tag} {tail}")),
            ],
        )
        .await
        .expect("造费率夹具车间");
        id
    }

    /// 删掉本用例造的部门。
    async fn drop_dept(id: Uuid) {
        let _ = db()
            .exec_sql(
                "DELETE FROM sys_dept WHERE id = ?::uuid".to_string(),
                vec![Value::from(&id)],
            )
            .await;
    }

    /// 造一条工资率（`scope_id`：`COMPANY` 必须为空）。
    ///
    /// 费率 / 日期 / 状态直接写成 SQL 字面量：`Value` 只对文本 / UUID 有直通转换，
    /// 绑 numeric 会被驱动拒（`cannot bind parameter to column type numeric`）；
    /// 这里的值全是本文件里的常量夹具，不存在注入面。
    async fn insert_rate(
        scope: &str,
        scope_id: Option<Uuid>,
        rate: &str,
        from: &str,
        to: Option<&str>,
        status: i16,
        suffix: &str,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let scope_id_sql = scope_id.map_or_else(|| "NULL".to_string(), |id| format!("'{id}'::uuid"));
        let to_sql = to.map_or_else(|| "NULL".to_string(), |text| format!("'{text}'::date"));
        db().exec_sql(
            format!(
                "INSERT INTO biz_cost_rate (id, rate_type, scope, scope_id, rate, effective_from, \
                                          effective_to, status, remark) \
                 VALUES (?::uuid, ?, ?, {scope_id_sql}, {rate}::numeric, '{from}'::date, {to_sql}, {status}, ?)"
            ),
            vec![
                Value::from(&id),
                Value::Text(RATE_TYPE_LABOR.to_string()),
                Value::Text(scope.to_string()),
                Value::Text(format!("{RATE_MARK} {suffix}")),
            ],
        )
        .await
        .expect("造费率夹具");
        id
    }

    /// 「留空费率」的建单请求（B2-3 的主场景）。
    fn blank_rate_req(fx: &Fixture, object: Uuid, dept: Uuid) -> CostLaborCreateReq {
        let mut req = fx.create_req(object, dept);
        req.rate = None;
        req
    }

    /// **验收项 ①**：只有公司级费率 ⇒ 留空时用公司级，出参回填来源，金额按取到的费率算。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn company_rate_fills_blank_rate() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "C1").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_COMPANY, None, "42.5", "2026-01-01", None, 1, &fx.suffix).await;

        let created = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect("公司级兜底");
        assert_eq!(created.rate, "42.500000", "取到公司级费率（库标度 6 位）");
        assert_eq!(created.amount, "340.0000", "金额 = 8 × 42.5，仍由服务端算");
        assert_eq!(created.rate_scope.as_deref(), Some(SCOPE_COMPANY));
        assert_eq!(created.rate_scope_label.as_deref(), Some("公司（全公司）"));

        // 显式填了费率 ⇒ 以填的为准（取数只补空，不覆盖人工判断）
        let mut explicit = blank_rate_req(&fx, object, dept);
        explicit.rate = Some(dec("50"));
        let created = handler::create_labor(None, explicit).await.expect("显式费率");
        assert_eq!(created.rate, "50.000000");
        assert_eq!(created.amount, "400.0000");
        assert_eq!(created.rate_scope, None, "人工填的费率没有「取数来源」");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// **验收项 ②**：有车间级 ⇒ 车间级压过公司级。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dept_rate_beats_company_rate() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "D1").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_COMPANY, None, "40", "2026-01-01", None, 1, &fx.suffix).await;
        insert_rate(SCOPE_DEPT, Some(dept), "55", "2026-01-01", None, 1, &fx.suffix).await;

        let created = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect("车间级");
        assert_eq!(created.rate, "55.000000", "车间级优先于公司级");
        assert_eq!(created.amount, "440.0000");
        assert_eq!(created.rate_scope.as_deref(), Some(SCOPE_DEPT));
        let label = created.rate_scope_label.clone().expect("来源文案");
        assert!(label.starts_with("车间（"), "{label}");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// **验收项 ③**：有对象级 ⇒ 对象级压过车间级与公司级。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cost_object_rate_beats_dept_and_company() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "O1").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_COMPANY, None, "40", "2026-01-01", None, 1, &fx.suffix).await;
        insert_rate(SCOPE_DEPT, Some(dept), "55", "2026-01-01", None, 1, &fx.suffix).await;
        insert_rate(SCOPE_COST_OBJECT, Some(object), "70", "2026-01-01", None, 1, &fx.suffix).await;

        let created = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect("对象级");
        assert_eq!(created.rate, "70.000000", "对象级优先于车间级 / 公司级");
        assert_eq!(created.amount, "560.0000");
        assert_eq!(created.rate_scope.as_deref(), Some(SCOPE_COST_OBJECT));
        assert!(
            created
                .rate_scope_label
                .as_deref()
                .is_some_and(|label| label.starts_with("成本对象（")),
            "{:?}",
            created.rate_scope_label
        );

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// **验收项 ④**：三级全空 ⇒ `422` + `66059` 明确报错，**一行都不落库、不按 0 计**。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_rate_fails_loudly_and_writes_nothing() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "M1").await;
        let object = fx.object_in(dept, "OPEN").await;

        let error = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect_err("三级全空必须报错");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, 66059);
        // 报文要点名「谁 / 哪天 / 查了哪几级 / 怎么修」
        assert!(error.msg.contains("CB21-"), "要点名成本对象：{}", error.msg);
        assert!(error.msg.contains("2026-11-06"), "要点名日期：{}", error.msg);
        assert!(error.msg.contains("成本对象 → 车间 → 公司"), "{}", error.msg);
        assert!(error.msg.contains("怎么修"), "{}", error.msg);
        assert!(!error.msg.contains("0 元"), "不许按 0 计：{}", error.msg);
        assert_eq!(live_rows(object).await, 0, "报错时不许落库（更不许落一个 0 费率行）");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
    }

    /// 别的车间的费率不许串到本车间（串了成本单价就是错的，且没人看得出来）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_depts_rate_is_not_reused() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "X1").await;
        let other = new_dept(&fx.suffix, "X2").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_DEPT, Some(other), "66", "2026-01-01", None, 1, &fx.suffix).await;

        let error = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect_err("别的车间的费率不算数");
        assert_eq!(error.code, 66059);
        assert!(
            error.msg.contains("同类型费率共 1 条"),
            "要说明有一条被跳过：{}",
            error.msg
        );
        assert_eq!(live_rows(object).await, 0);

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        drop_dept(other).await;
        wipe_test_rates().await;
    }

    /// 生效期：过期 / 未生效 / 已停用都不参与取数（只认覆盖工作日期且启用中）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_future_and_disabled_rates_are_ignored() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "E1").await;
        let object = fx.object_in(dept, "OPEN").await;
        // 已过期（10-31 结束，工作日期 11-06）
        insert_rate(
            SCOPE_COMPANY,
            None,
            "40",
            "2026-01-01",
            Some("2026-10-31"),
            1,
            &fx.suffix,
        )
        .await;
        // 还没生效
        insert_rate(SCOPE_COMPANY, None, "41", "2026-12-01", None, 1, &fx.suffix).await;
        // 停用
        insert_rate(SCOPE_COMPANY, None, "42", "2026-01-02", None, 0, &fx.suffix).await;

        let error = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect_err("三条都不该被采用");
        assert_eq!(error.code, 66059);
        assert!(error.msg.contains("同类型费率共 3 条"), "{}", error.msg);

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// 版本化：同一级里**生效更晚的版本**赢（费率涨了以后新工时用新费率）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn newest_effective_version_wins() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "V1").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_COMPANY, None, "40", "2026-01-01", None, 1, &fx.suffix).await;
        insert_rate(SCOPE_COMPANY, None, "50", "2026-11-01", None, 1, &fx.suffix).await;

        let created = handler::create_labor(None, blank_rate_req(&fx, object, dept))
            .await
            .expect("最新版本");
        assert_eq!(created.rate, "50.000000", "11 月的工时用 11-01 起生效的 50");
        assert_eq!(created.amount, "400.0000");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// 导入：工资率列留空 ⇒ 逐行按「对象 > 车间 > 公司」取数（同一批可以不同车间不同费率）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn import_fills_blank_rate_per_row() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "I1").await;
        let object = fx.object_in(dept, "OPEN").await;
        insert_rate(SCOPE_COMPANY, None, "40", "2026-01-01", None, 1, &fx.suffix).await;
        insert_rate(SCOPE_DEPT, Some(dept), "55", "2026-01-01", None, 1, &fx.suffix).await;

        let object_no: String = db()
            .scalar_with_params(
                "SELECT object_no FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("取对象号");
        let dept_code: String = db()
            .scalar_with_params(
                "SELECT dept_code FROM sys_dept WHERE id = ?::uuid",
                vec![Value::from(&dept)],
            )
            .await
            .expect("取部门编码");

        // 工资率列留空（第 8 列传空串）
        let rows = vec![import_row(&object_no, &dept_code, "2026-11-02", "王五", "7.5", "3", "")];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入");
        assert_eq!((result.success, result.failed), (1, 0), "{:?}", result.rows);

        let imported_row = db()
            .find_with_params(
                "SELECT rate, amount FROM biz_cost_labor WHERE cost_object_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&object)],
            )
            .await
            .expect("查导入行")
            .into_iter()
            .next()
            .expect("导入行存在");
        let rate: Decimal = imported_row.get_typed("rate").expect("rate");
        let amount: Decimal = imported_row.get_typed("amount").expect("amount");
        assert_eq!(rate, dec("55"), "车间级取得 55（不是公司级 40）");
        assert_eq!(amount, dec("412.5"), "round(7.5 × 55, 4) = 412.5000");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
        wipe_test_rates().await;
    }

    /// 导入：留空且三级全空 ⇒ 整批拒绝，错误落在「工资率」列（行号 + 人话）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn import_rejects_the_batch_when_rate_cannot_be_resolved() {
        if !ensure_db().await {
            return;
        }
        let _lock = rate_lock().await;
        wipe_test_rates().await;

        let fx = Fixture::build().await;
        let dept = new_dept(&fx.suffix, "J1").await;
        let object = fx.object_in(dept, "OPEN").await;
        let object_no: String = db()
            .scalar_with_params(
                "SELECT object_no FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("取对象号");
        let dept_code: String = db()
            .scalar_with_params(
                "SELECT dept_code FROM sys_dept WHERE id = ?::uuid",
                vec![Value::from(&dept)],
            )
            .await
            .expect("取部门编码");

        let rows = vec![import_row(&object_no, &dept_code, "2026-11-02", "王五", "7.5", "3", "")];
        let bytes = import::build_rows_xlsx(&rows).expect("造 xlsx");
        let result = import::import_labors(&bytes, &ImportContext::default())
            .await
            .expect("导入（业务拒绝不是错误）");
        assert_eq!((result.success, result.failed), (0, 1), "{:?}", result.rows);
        assert_eq!(result.rows[0].column, "工资率");
        assert!(
            result.rows[0].message.contains("成本对象 → 车间 → 公司"),
            "{}",
            result.rows[0].message
        );
        assert_eq!(live_rows(object).await, 0, "整批拒绝时一行都不许落库");

        fx.cleanup(&[object]).await;
        drop_dept(dept).await;
    }
}
