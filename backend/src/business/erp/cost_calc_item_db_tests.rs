//! calc 第 6 段「约当产量与成本计算单」的真库用例（`ERP_DB_URL` 开关，B2-11）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_item_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`example_a_lands_in_the_database`] | **验收①「§4.6 算例 A 的死数字」**：库里逐列核（约当 1000/900、单位 19/4.444444/4.444444、完工 22311.1104、在制 4688.8896） |
//! | 2 | [`identity_holds_and_recalc_is_idempotent`] | **验收②「恒等式」** + 重算不翻倍（删 CALC 重灌） |
//! | 3 | [`beginning_wip_carries_over_from_prior_settled_item`] | **验收③「期初在制结转」**：上期 wip 200 / 4688.8896 → 本期期初；本期完工 = 累计 1300 − 前期 800 |
//! | 4 | [`missing_progress_is_rejected`] | 期末在制 200 但进度 0 ⇒ 409 + `66067`，一行不写 |
//! | 5 | [`finished_without_material_is_rejected`] | 有完工无材料 ⇒ 409 + `66068` |
//! | 6 | [`reversed_prior_period_blocks_carryover`] | 上期批次已红冲 ⇒ 409 + `66069` |
//! | 7 | [`locked_period_keeps_old_rows`] | 批次 SUBMITTED ⇒ 409 + `66070`，旧计算单**原样保留** |
//! | 8 | [`missing_batch_is_409`] | 该期间没有批次 ⇒ 409 + `66071` |
//!
//! # 夹具纪律
//!
//! 全部用 `CB211-*` / `CS211-*` 前缀；期间年份按进程内计数器分配（2200+），
//! 避免 `uk_biz_cost_period` 在夹具之间撞键。清场按登记 id 删，顺序按 FK：
//! 计算单 → 批次 → 归集 → 成本对象 → 物料 / 仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_item;
    use crate::business::erp::cost_period_item_dto::ItemRunOutcome;
    use crate::shared::error_code;
    use nik::prelude::rust_decimal::Decimal;
    use tokio::sync::OnceCell;

    /// 连库（进程内只初始化一次）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
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

    /// 唯一后缀（纳秒 + 进程内计数器）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:03}", now % 1_000_000_000)
    }

    /// 期间年份：2200 + 进程内序号（每个夹具独占一个会计年度，且与别的成本卡错开）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2200 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 800).unwrap_or(0)
    }

    /// 编码尾巴（物料 / 仓库编码有长度上限）。
    fn tail_code(suffix: &str) -> String {
        suffix
            .chars()
            .filter(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }

    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（`CB211-*` / `CS211-*` 前缀）。只在每个夹具建之前跑一次。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS211-%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS211-%'",
            "DELETE FROM biz_cost_collect WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB211-%')",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB211-%'",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB211-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB211W%'",
        ];
        for sql in statements {
            if let Err(error) = db().exec_sql(sql.to_string(), vec![]).await {
                println!("sweep 失败（{sql}）：{error}");
            }
        }
    }

    impl Fixture {
        async fn build() -> Self {
            sweep_leftovers().await;
            let suffix = uniq();
            let year = next_year();
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
                    Value::Text(format!("CB211-{suffix}")),
                    Value::Text(format!("计算单夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB211W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("计算单夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");
            Self {
                suffix,
                year,
                dept,
                product,
                warehouse,
                objects: std::sync::Mutex::new(Vec::new()),
                periods: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 本期期间（11 月）。
        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        /// 上期期间（10 月）。
        fn prior_period_code(&self) -> String {
            format!("{}-10", self.year)
        }

        /// 造一个成本对象（`completed_qty` = **累计**完工，`wip_qty` = 期末在制，`pct` = 完工进度）。
        async fn object(&self, completed: &str, wip: &str, pct: &str) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB211-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 2000, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, 'ONE_TIME', ?::uuid, '计算单夹具车间', ?::text::date, 'OPEN', \
                         now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(format!("{}-01-01", self.year)),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            (id, object_no)
        }

        /// 造一条归集行（`MATERIAL` 必须带物料 + 数量，`LABOR` / `OVERHEAD` 必须不带）。
        async fn collect(&self, object_id: Uuid, element: &str, amount: &str) {
            let period_code = self.period_code();
            let is_material = element == "MATERIAL";
            let source_type = if is_material {
                "STOCK_OUT"
            } else if element == "LABOR" {
                "LABOR_TICKET"
            } else {
                "ALLOC"
            };
            // 料行带物料 + 数量 + 单价（`ck_biz_cost_collect_mat` 要求）；工/费行必须是 NULL。
            let material_cols = if is_material {
                "?::uuid, 10, ?::text::numeric"
            } else {
                "NULL, NULL, NULL"
            };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&object_id),
                Value::Text(period_code.clone()),
                Value::Text(element.to_string()),
                Value::Text(source_type.to_string()),
                Value::from(&Uuid::now_v7()),
                Value::Text(format!("B211-{}", &Uuid::now_v7().simple().to_string()[20..])),
                Value::Text(format!("{period_code}-15")),
            ];
            if is_material {
                params.push(Value::from(&self.product));
                params.push(Value::Text("1".to_string()));
            }
            params.push(Value::Text(amount.to_string()));
            params.push(Value::from(&self.dept));
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                   source_id, source_no, line_no, occur_date, material_id, qty, \
                                                   unit_price, amount, dept_id, dept_name, remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, 0, ?::text::date, {material_cols}, \
                             ?::text::numeric, ?::uuid, '计算单夹具车间', 'B211-夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集夹具");
        }

        /// 造本期批次（`DRAFT`）。
        async fn period(&self) -> Uuid {
            self.batch(&self.period_code(), "DRAFT").await
        }

        /// 造上期批次（`SETTLED`，可当期初来源）+ 该对象的上期计算单（算例 A 那一行）。
        async fn prior_settled_with_item(&self, object_id: Uuid) -> Uuid {
            let period_id = self.batch(&self.prior_period_code(), "SETTLED").await;
            // 上期计算单 = 设计 §4.6.2 算例 A：完工 800 / 在制 200（50%）/ 料 19000 / 工 4000 / 费 4000
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                   wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, \
                                                   completion_pct, equiv_material, equiv_conversion, \
                                                   mat_cur, labor_cur, oh_cur, unit_material, unit_labor, \
                                                   unit_overhead, finished_cost, wip_end_cost, \
                                                   unit_finished_cost, stock_in_id, status, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0, 0, 800, 200, 50, 1000, 900, \
                         19000, 4000, 4000, 19.000000, 4.444444, 4.444444, 22311.1104, 4688.8896, \
                         27.888888, ?::uuid, 'SETTLED', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&period_id),
                    Value::from(&object_id),
                    Value::from(&self.product),
                    Value::from(&Uuid::now_v7()),
                ],
            )
            .await
            .expect("造上期计算单夹具");
            period_id
        }

        /// 造一条**被红冲**的上期批次 + 该对象的上期计算单（设计 §4.6.3 要拦住的情形）。
        async fn prior_reversed_with_item(&self, object_id: Uuid) -> Uuid {
            let period_id = self.batch(&self.prior_period_code(), "REVERSED").await;
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                   completed_qty, wip_beg_qty, wip_beg_cost, wip_end_qty, \
                                                   completion_pct, mat_cur, labor_cur, oh_cur, finished_cost, \
                                                   wip_end_cost, status, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 10, 0, 0, 0, 0, 100, 0, 0, 100, 0, \
                         'CALC', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&period_id),
                    Value::from(&object_id),
                    Value::from(&self.product),
                ],
            )
            .await
            .expect("造红冲上期计算单夹具");
            period_id
        }

        /// 造一个批次（指定期间 + 状态）。
        ///
        /// 四道状态关联 CHECK 必须一起满足（不是本卡口径，是 V0373 的库层约束）：
        /// - `ck_biz_cost_period_appr`：`SUBMITTED/APPROVED/SETTLED/CLOSED/REVERSED` 必须有 `approval_instance_id`
        /// - `ck_biz_cost_period_settle`：`SETTLED/CLOSED/REVERSED` 必须有 `settled_at` + `finish_voucher_id`
        /// - `ck_biz_cost_period_rev`：`REVERSED` 必须有 `reversed_at` + `reverse_reason`
        ///
        /// 所以先插 `DRAFT`，再按目标状态补齐列、最后改状态（一条 UPDATE 里改完）。
        async fn batch(&self, period_code: &str, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period_code.to_string()),
                    Value::Text(format!("CS211-{}-{}", period_code.replace('-', ""), &self.suffix[..4])),
                ],
            )
            .await
            .expect("造成本批次夹具");
            self.periods.lock().expect("批次清单").push(id);

            let needs_approval = matches!(status, "SUBMITTED" | "APPROVED" | "SETTLED" | "CLOSED" | "REVERSED");
            let needs_settled = matches!(status, "SETTLED" | "CLOSED" | "REVERSED");
            let needs_reverse = status == "REVERSED";
            let mut sets = vec!["status = ?".to_string()];
            let mut params: Vec<Value> = vec![Value::Text(status.to_string())];
            if needs_approval {
                sets.push("approval_instance_id = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
            }
            if needs_settled {
                sets.push("settled_at = now()".to_string());
                sets.push("settled_by = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
                sets.push("finish_voucher_id = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
            }
            if needs_reverse {
                sets.push("reversed_at = now()".to_string());
                sets.push("reverse_reason = '夹具红冲'".to_string());
            }
            if status != "DRAFT" {
                params.push(Value::from(&id));
                db().exec_sql(
                    format!("UPDATE biz_cost_period SET {} WHERE id = ?::uuid", sets.join(", ")),
                    params,
                )
                .await
                .expect("改批次状态");
            }
            id
        }

        /// 把本期批次改成 `SUBMITTED`（`ck_biz_cost_period_appr` 要求有审批实例）。
        async fn submit_period(&self) {
            db().exec_sql(
                "UPDATE biz_cost_period SET status = 'SUBMITTED', approval_instance_id = ?::uuid \
                  WHERE period_code = ? AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::from(&Uuid::now_v7()), Value::Text(self.period_code())],
            )
            .await
            .expect("提交批次");
        }

        /// 读回本期计算单（原始 SQL，字符串形态，避免「用被测的读函数验被测的写函数」）。
        async fn stored(&self) -> Vec<StoredRow> {
            let rows = db()
                .query_sql(
                    "SELECT i.cost_object_id, i.wip_beg_qty::text AS wip_beg_qty, \
                            i.wip_beg_cost::text AS wip_beg_cost, i.completed_qty::text AS completed_qty, \
                            i.wip_end_qty::text AS wip_end_qty, i.completion_pct::text AS completion_pct, \
                            i.equiv_material::text AS equiv_material, \
                            i.equiv_conversion::text AS equiv_conversion, i.mat_cur::text AS mat_cur, \
                            i.labor_cur::text AS labor_cur, i.oh_cur::text AS oh_cur, \
                            i.unit_material::text AS unit_material, i.unit_labor::text AS unit_labor, \
                            i.unit_overhead::text AS unit_overhead, \
                            i.finished_cost::text AS finished_cost, i.wip_end_cost::text AS wip_end_cost, \
                            i.unit_finished_cost::text AS unit_finished_cost, i.status \
                       FROM biz_cost_period_item i \
                       JOIN biz_cost_period p ON p.id = i.period_id \
                      WHERE p.period_code = ? ORDER BY i.cost_object_id"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await
                .expect("读计算单");
            rows.iter()
                .map(|row| StoredRow {
                    cost_object_id: row.get_typed("cost_object_id").expect("对象"),
                    wip_beg_qty: row.get_typed("wip_beg_qty").expect("期初数量"),
                    wip_beg_cost: row.get_typed("wip_beg_cost").expect("期初成本"),
                    completed_qty: row.get_typed("completed_qty").expect("完工"),
                    wip_end_qty: row.get_typed("wip_end_qty").expect("在制"),
                    completion_pct: row.get_typed("completion_pct").expect("进度"),
                    equiv_material: row.get_typed("equiv_material").expect("约当料"),
                    equiv_conversion: row.get_typed("equiv_conversion").expect("约当工"),
                    mat_cur: row.get_typed("mat_cur").expect("料"),
                    labor_cur: row.get_typed("labor_cur").expect("工"),
                    oh_cur: row.get_typed("oh_cur").expect("费"),
                    unit_material: row.get_typed("unit_material").expect("单位料"),
                    unit_labor: row.get_typed("unit_labor").expect("单位工"),
                    unit_overhead: row.get_typed("unit_overhead").expect("单位费"),
                    finished_cost: row.get_typed("finished_cost").expect("完工成本"),
                    wip_end_cost: row.get_typed("wip_end_cost").expect("在制成本"),
                    unit_finished_cost: row.get_typed("unit_finished_cost").expect("完工单价"),
                    status: row.get_typed("status").expect("状态"),
                })
                .collect()
        }

        /// 清场：按 id 删（FK 顺序见模块头）。
        async fn cleanup(&self) {
            let joined = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let objects = joined(&self.objects);
            let periods = joined(&self.periods);
            let mut statements = Vec::new();
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            statements.push(format!("DELETE FROM biz_material WHERE id = '{}'::uuid", self.product));
            statements.push(format!(
                "DELETE FROM biz_warehouse WHERE id = '{}'::uuid",
                self.warehouse
            ));
            for sql in statements {
                if let Err(error) = db().exec_sql(sql.clone(), vec![]).await {
                    println!("cleanup 失败（{sql}）：{error}");
                }
            }
        }
    }

    /// 库里读回的一行（`NUMERIC` 全是字符串形态）。
    struct StoredRow {
        cost_object_id: Uuid,
        wip_beg_qty: String,
        wip_beg_cost: String,
        completed_qty: String,
        wip_end_qty: String,
        completion_pct: String,
        equiv_material: String,
        equiv_conversion: String,
        mat_cur: String,
        labor_cur: String,
        oh_cur: String,
        unit_material: String,
        unit_labor: String,
        unit_overhead: String,
        finished_cost: String,
        wip_end_cost: String,
        unit_finished_cost: String,
        status: String,
    }

    /// 库里 `NUMERIC(18,4)` 读回是 `"1500.0000"`，算法算出来是 `"1500"` ⇒ 统一规范化再比。
    fn money(value: &str, scale: usize) -> String {
        let parsed: Decimal = value.parse().expect("金额");
        format!("{:.*}", scale, parsed)
    }

    /// 跑一次第 6 段（失败时打印报文，省一轮盲猜）。
    async fn run(fx: &Fixture) -> ItemRunOutcome {
        match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!(
                    "计算单失败：{} / {} / {:?}",
                    error.status, error.msg, error.internal_detail
                );
            }
        }
    }

    // ==================== 用例 ====================

    /// ① ★ **§4.6 算例 A**：料 19000 / 工 4000 / 费 4000、完工 800、在制 200（50%）
    /// → 库里逐列等于设计表格的死数字。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn example_a_lands_in_the_database() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, _) = fx.object("800", "200", "50").await;
        fx.collect(object, "MATERIAL", "19000").await;
        fx.collect(object, "LABOR", "4000").await;
        fx.collect(object, "OVERHEAD", "4000").await;
        fx.period().await;

        let outcome = run(&fx).await;
        assert_eq!(outcome.object_count(), 1, "一个对象一张计算单");
        assert_eq!(money(&outcome.total_cost.to_string(), 4), "27000.0000");
        assert_eq!(money(&outcome.total_finished.to_string(), 4), "22311.1104");
        assert_eq!(money(&outcome.total_wip.to_string(), 4), "4688.8896");

        let stored = fx.stored().await;
        assert_eq!(stored.len(), 1);
        let row = &stored[0];
        assert_eq!(row.cost_object_id, object);
        assert_eq!(row.status, "CALC", "算完是 CALC（结算才置 SETTLED）");
        assert_eq!(money(&row.wip_beg_qty, 4), "0.0000", "首期无期初");
        assert_eq!(money(&row.wip_beg_cost, 4), "0.0000");
        assert_eq!(money(&row.completed_qty, 4), "800.0000");
        assert_eq!(money(&row.wip_end_qty, 4), "200.0000");
        assert_eq!(money(&row.completion_pct, 2), "50.00");
        assert_eq!(money(&row.equiv_material, 4), "1000.0000", "800 + 200（材料一次投入）");
        assert_eq!(money(&row.equiv_conversion, 4), "900.0000", "800 + 200×50%");
        assert_eq!(money(&row.mat_cur, 4), "19000.0000");
        assert_eq!(money(&row.labor_cur, 4), "4000.0000");
        assert_eq!(money(&row.oh_cur, 4), "4000.0000");
        assert_eq!(money(&row.unit_material, 6), "19.000000");
        assert_eq!(money(&row.unit_labor, 6), "4.444444");
        assert_eq!(money(&row.unit_overhead, 6), "4.444444");
        assert_eq!(money(&row.finished_cost, 4), "22311.1104");
        assert_eq!(money(&row.wip_end_cost, 4), "4688.8896");
        assert_eq!(money(&row.unit_finished_cost, 6), "27.888888", "= 22311.1104 / 800");

        fx.cleanup().await;
    }

    /// ② ★ **恒等式**（库里读回来的值自证）+ 重算幂等（第二次不翻倍、金额不变）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_holds_and_recalc_is_idempotent() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, _) = fx.object("800", "200", "50").await;
        fx.collect(object, "MATERIAL", "19000").await;
        fx.collect(object, "LABOR", "4000").await;
        fx.collect(object, "OVERHEAD", "4000").await;
        fx.period().await;

        run(&fx).await;
        let first = fx.stored().await;
        assert_eq!(first.len(), 1);

        let second = run(&fx).await;
        let stored = fx.stored().await;
        assert_eq!(stored.len(), 1, "重算 = 删 CALC 重灌，行数不翻倍");
        assert_eq!(second.purged, 1, "第二次要先清掉上一轮那 1 行");
        for row in &stored {
            let prev = first.first().expect("第一轮的行");
            assert_eq!(row.finished_cost, prev.finished_cost, "重算金额必须收敛");
            assert_eq!(row.wip_end_cost, prev.wip_end_cost);
            assert_eq!(row.unit_finished_cost, prev.unit_finished_cost);
            // 恒等式：完工 + 在制 = 期初 + 料 + 工 + 费（库里读回来的字符串自证）
            let outflow = row.finished_cost.parse::<Decimal>().expect("完工")
                + row.wip_end_cost.parse::<Decimal>().expect("在制");
            let inflow = row.wip_beg_cost.parse::<Decimal>().expect("期初")
                + row.mat_cur.parse::<Decimal>().expect("料")
                + row.labor_cur.parse::<Decimal>().expect("工")
                + row.oh_cur.parse::<Decimal>().expect("费");
            assert_eq!(outflow, inflow, "完工 + 期末在制 = 期初在制 + 本期归集");
        }

        fx.cleanup().await;
    }

    /// ③ ★ **期初在制结转（跨月）**：上期已结算 wip 200 / 4688.8896 → 本期期初；
    /// 本期完工 = 累计 1300 − 前期 800 = 500；期末在制 0 ⇒ 尾差进完工。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn beginning_wip_carries_over_from_prior_settled_item() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        // 累计完工 1300（上期 800 + 本期 500），本期期末在制 0
        let (object, _) = fx.object("1300", "0", "0").await;
        fx.prior_settled_with_item(object).await;
        fx.collect(object, "MATERIAL", "5000").await;
        fx.collect(object, "LABOR", "2000").await;
        fx.collect(object, "OVERHEAD", "2000").await;
        fx.period().await;

        let outcome = run(&fx).await;
        let stored = fx.stored().await;
        assert_eq!(stored.len(), 1);
        let row = &stored[0];
        assert_eq!(money(&row.wip_beg_qty, 4), "200.0000", "上期期末在制数量结转到本期");
        assert_eq!(money(&row.wip_beg_cost, 4), "4688.8896", "上期期末在制成本结转到本期");
        assert_eq!(money(&row.completed_qty, 4), "500.0000", "累计 1300 − 前期 800");
        assert_eq!(money(&row.wip_end_qty, 4), "0.0000");
        assert_eq!(money(&row.equiv_material, 4), "500.0000");
        assert_eq!(money(&row.equiv_conversion, 4), "500.0000", "在制 0 ⇒ 约当就是完工数");
        // 期初要素还原：料 19×200=3800、工 4.444444×100=444.4444、费 = 差额 444.4452
        assert_eq!(money(&row.unit_material, 6), "17.600000", "(3800 + 5000) / 500");
        assert_eq!(money(&row.unit_labor, 6), "4.888889", "(444.4444 + 2000) / 500");
        assert_eq!(money(&row.unit_overhead, 6), "4.888890", "(444.4452 + 2000) / 500");
        assert_eq!(
            money(&row.finished_cost, 4),
            "13688.8896",
            "在制数量 0 ⇒ 尾差进完工（在制成本留 0）"
        );
        assert_eq!(
            money(&row.wip_end_cost, 4),
            "0.0000",
            "期末没有在制数量，不该有在制成本"
        );
        assert_eq!(money(&row.unit_finished_cost, 6), "27.377779", "= 13688.8896 / 500");
        assert_eq!(
            money(&outcome.total_cost.to_string(), 4),
            "9000.0000",
            "本期投入（期初 4688.8896 另计）"
        );
        assert_eq!(
            money(&outcome.total_finished.to_string(), 4),
            "13688.8896",
            "期初 + 本期全部转出"
        );

        fx.cleanup().await;
    }

    /// ④ 期末在制 > 0 但进度没填 ⇒ 409 + `66067`，**一行不写**。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_progress_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, object_no) = fx.object("800", "200", "0").await;
        fx.collect(object, "MATERIAL", "19000").await;
        fx.collect(object, "LABOR", "4000").await;
        fx.period().await;

        let error = match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(_) => {
                fx.cleanup().await;
                panic!("缺完工进度必须 409");
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_OUTPUT_QTY_INVALID, "66067");
        assert!(error.msg.contains(&object_no), "报文要点名对象：{}", error.msg);
        assert!(error.msg.contains("完工进度未填"), "{}", error.msg);
        assert!(fx.stored().await.is_empty(), "被拒时一行都不许写");

        fx.cleanup().await;
    }

    /// ⑤ 有完工数量、一笔材料都没归集 ⇒ 409 + `66068`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finished_without_material_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, object_no) = fx.object("100", "0", "0").await;
        fx.collect(object, "LABOR", "500").await;
        fx.collect(object, "OVERHEAD", "500").await;
        fx.period().await;

        let error = match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(_) => {
                fx.cleanup().await;
                panic!("零成本完工必须 409");
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_FINISHED_WITHOUT_MATERIAL, "66068");
        assert!(error.msg.contains(&object_no), "{}", error.msg);
        assert!(fx.stored().await.is_empty());

        fx.cleanup().await;
    }

    /// ⑥ 上期批次已红冲 ⇒ 409 + `66069`（设计 §4.6.3：不许自动取数）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reversed_prior_period_blocks_carryover() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, object_no) = fx.object("100", "0", "0").await;
        fx.prior_reversed_with_item(object).await;
        fx.collect(object, "MATERIAL", "1000").await;
        fx.period().await;

        let error = match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(_) => {
                fx.cleanup().await;
                panic!("上期已红冲必须 409");
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_PRIOR_PERIOD_REVERSED, "66069");
        assert!(error.msg.contains(&object_no), "{}", error.msg);
        assert!(
            error.msg.contains(&fx.prior_period_code()),
            "报文要点名红冲的期间：{}",
            error.msg
        );

        fx.cleanup().await;
    }

    /// ⑦ 批次已提交 ⇒ 409 + `66070`，**旧计算单原样保留**（不许留半算状态）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn locked_period_keeps_old_rows() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, _) = fx.object("800", "200", "50").await;
        fx.collect(object, "MATERIAL", "19000").await;
        fx.collect(object, "LABOR", "4000").await;
        fx.collect(object, "OVERHEAD", "4000").await;
        fx.period().await;

        run(&fx).await;
        let before = fx.stored().await;
        assert_eq!(before.len(), 1);

        fx.submit_period().await;
        let error = match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(_) => {
                fx.cleanup().await;
                panic!("批次已提交必须 409");
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_ITEM_PERIOD_LOCKED, "66070");
        let after = fx.stored().await;
        assert_eq!(after.len(), 1, "被拒时旧计算单不能被删");
        assert_eq!(
            money(&after[0].finished_cost, 4),
            money(&before[0].finished_cost, 4),
            "旧计算单金额原样保留"
        );

        fx.cleanup().await;
    }

    /// ⑧ 该期间没有批次 ⇒ 409 + `66071`（`period_id` 是外键，无处安放）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_batch_is_409() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object, _) = fx.object("800", "200", "50").await;
        fx.collect(object, "MATERIAL", "19000").await;
        // 故意不建批次

        let error = match cost_calc_item::calc_items(&fx.period_code(), None).await {
            Ok(_) => {
                fx.cleanup().await;
                panic!("没有批次必须 409");
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_PERIOD_BATCH_MISSING, "66071");
        assert!(error.msg.contains(&fx.period_code()), "{}", error.msg);

        fx.cleanup().await;
    }
}
