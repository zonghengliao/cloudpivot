//! 「未归集清单」的真库用例（`ERP_DB_URL` 开关，B2-12）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_uncollected_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`four_blocks_report_the_real_gaps`] | **验收①②**：四块各自只捞自己的缺口（含「正例不进清单」：挂了对象 / 带费率的工时 / 已摊完的池 / 草稿出库单） |
//! | 2 | [`stock_out_gap_carries_doc_id_and_no`] | **验收②**：「缺成本对象的领料」每行都带可点击跳转的来源单据 id + 单号 + 金额合计 |
//! | 3 | [`pool_reason_progresses_no_rule_object_allocated`] | 池缺口三态推进：无规则 ⇒ 有规则无对象 ⇒ 未摊完 ⇒ 摊完消失（口径 = 设计 §6.2.2 三条文案） |
//! | 4 | [`by_batch_and_by_period_share_one_shape`] | 设计 #33（按批次 id）与按期间入口出参一致；没有批次时 `batch = None`；不存在 id ⇒ 404 |
//! | 5 | [`settled_batch_is_flagged`] | 批次 `SETTLED` ⇒ `settled = true`（缺口降级为「历史解释」） |
//! | 6 | [`draft_labor_is_reported_as_not_confirmed`] | 已录未确认（DRAFT）不等于「没录」：文案与字段都对 |
//! | 7 | [`empty_period_returns_zero_blocks`] | 空期间 ⇒ 四块都是 `count = 0` 的空块（接口永远 200，不阻断） |
//!
//! # 夹具纪律
//!
//! 全部用 `CB212-*` / `CS212-*` / `CR212-*` / `OUT-CB212-*` 前缀；期间年份按进程内计数器
//! 分配（2500+，避开 B2-11 的 2200+ 段）。清场按导出状态里的 id 删，FK 顺序：
//! 分摊结果 → 归集 → 工时 → 费用台账 → 出库明细/单 → 批次 → 成本对象 → 分摊规则 → 物料/仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_uncollected_dto::{
        self as dto, REASON_NO_OBJECT, REASON_NO_RULE, REASON_NOT_ALLOCATED,
    };
    use crate::business::erp::cost_uncollected_handler;
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

    /// 期间年份：2500 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2500 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 800).unwrap_or(0)
    }

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

    /// 池缺口的科目（`4101.*` 是费用池唯一合法前缀）。
    const POOL_ACCOUNT: &str = "4101.09";

    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        unit: Uuid,
        material: Uuid,
        warehouse: Uuid,
        objects: Vec<Uuid>,
        stock_outs: Vec<Uuid>,
        overheads: Vec<Uuid>,
        periods: Vec<Uuid>,
        rules: Vec<Uuid>,
    }

    /// 清掉**上轮崩溃**留下的夹具（前缀固定，跨用例幂等）。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_cost_alloc_result WHERE rule_id IN \
                (SELECT id FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CR212%')",
            "DELETE FROM biz_cost_collect WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB212-%')",
            "DELETE FROM biz_cost_collect WHERE source_id IN \
                (SELECT id FROM biz_cost_overhead WHERE remark = 'CB212-夹具')",
            "DELETE FROM biz_cost_labor WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB212-%')",
            "DELETE FROM biz_cost_overhead WHERE remark = 'CB212-夹具'",
            "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-CB212-%'",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS212-%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS212-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB212-%'",
            "DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CR212%'",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB212-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB212W%'",
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

            let material = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                          status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?::uuid, 'RAW', 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&material),
                    Value::Text(format!("CB212-{suffix}")),
                    Value::Text(format!("未归集清单夹具物料 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造物料夹具");

            let warehouse = Uuid::now_v7();
            let code = format!("CB212W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("未归集清单夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");

            Self {
                suffix,
                year,
                dept,
                unit,
                material,
                warehouse,
                objects: Vec::new(),
                stock_outs: Vec::new(),
                overheads: Vec::new(),
                periods: Vec::new(),
                rules: Vec::new(),
            }
        }

        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        /// 造一个 `OPEN` 成本对象。
        async fn object(&mut self) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB212-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1000, 0, 0, 0, 'ONE_TIME', ?::uuid, \
                         '未归集夹具车间', ?::text::date, 'OPEN', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.material),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(format!("{}-10-01", self.year)),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.push(id);
            (id, object_no)
        }

        /// 造一条料归集行（让对象「本期有活动」⇒ 进 `objects_of_period`）。
        async fn material_collect(&self, object_id: Uuid, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, material_id, qty, \
                                               unit_price, amount, dept_id, dept_name, remark, \
                                               created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, ?, 0, ?::text::date, \
                         ?::uuid, 1, ?::text::numeric, ?::text::numeric, ?::uuid, '未归集夹具车间', \
                         'CB212-夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object_id),
                    Value::Text(self.period_code()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(format!("CB212-SO-{}", &self.suffix[..6])),
                    Value::Text(format!("{}-11-05", self.year)),
                    Value::from(&self.material),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造料归集夹具");
        }

        /// 造一条工时（`rate` = 0 ⇒ 缺费率；`DRAFT` ⇒ 未确认）。
        async fn labor(&self, object_id: Uuid, hours: &str, rate: &str, status: &str) -> Uuid {
            // 库层 CHECK 钉死 `amount = round(hours × rate, 4)`，夹具必须自己算对
            let hours_dec: Decimal = hours.parse().expect("工时");
            let rate_dec: Decimal = rate.parse().expect("费率");
            let rounded = (hours_dec * rate_dec).round_dp(4);
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_id, \
                                             employee_name, dept_id, dept_name, hours, machine_hours, rate, \
                                             amount, source_type, status, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, ?::uuid, '张伟', ?::uuid, '未归集夹具车间', \
                         ?::text::numeric, 0, ?::text::numeric, ?::text::numeric, 'MANUAL', ?, \
                         'CB212-夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(format!("{}-11-06", self.year)),
                    Value::from(&object_id),
                    Value::from(&Uuid::now_v7()),
                    Value::from(&self.dept),
                    Value::Text(hours.to_string()),
                    Value::Text(rate.to_string()),
                    Value::Text(rounded.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造工时夹具");
            id
        }

        /// 造一张出库单（`object` = `None` ⇒ 未挂成本对象）。
        async fn stock_out(&mut self, object: Option<Uuid>, status: &str, amount: &str) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let out_no = format!("OUT-CB212-{}-{}", &self.suffix[..6], self.stock_outs.len() + 1);
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, reason, created_at, updated_at, cost_object_id) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, ?, 10, ?::text::numeric, \
                         '生产领料', now(), now(), NULLIF(?::text,'')::uuid)"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(out_no.clone()),
                    Value::from(&self.warehouse),
                    Value::Text(format!("{}-11-07", self.year)),
                    Value::Text(status.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(object.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await
            .expect("造出库单夹具");
            db().exec_sql(
                "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, \
                                                 unit_cost, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, 10, ?::text::numeric, ?::text::numeric, \
                         'CB212-夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&id),
                    Value::from(&self.material),
                    Value::from(&self.unit),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造出库明细夹具");
            self.stock_outs.push(id);
            (id, out_no)
        }

        /// 造一条「车间费用池」：费用台账行 + 对应的 `OVERHEAD` 归集行（`cost_object_id` 为空）。
        async fn pool_row(&mut self, amount: &str) {
            let overhead_id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULL, ?::uuid, '未归集夹具车间', 'OTHER', ?, '1002', \
                         ?::text::numeric, 'MANUAL', 'CB212-夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&overhead_id),
                    Value::Text(self.period_code()),
                    Value::from(&self.dept),
                    Value::Text(POOL_ACCOUNT.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造费用台账夹具");
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, amount, dept_id, \
                                               dept_name, remark, created_at, updated_at) \
                 VALUES (?::uuid, NULL, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, ?, 0, ?::text::date, \
                         ?::text::numeric, ?::uuid, '未归集夹具车间', 'CB212-夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(self.period_code()),
                    Value::from(&overhead_id),
                    Value::Text(format!("CB212-OH-{}", &self.suffix[..6])),
                    Value::Text(format!("{}-11-08", self.year)),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造池归集夹具");
            self.overheads.push(overhead_id);
        }

        /// 造一条生效的分摊规则（车间 + 池科目）。
        async fn rule(&mut self) {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, biz_type, source_dept_id, \
                                                   source_dept_name, account_code, basis, priority, \
                                                   effective_from, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, '未归集夹具规则', 'OVERHEAD', ?::uuid, '未归集夹具车间', ?, \
                         'LABOR_HOURS', 10, ?::text::date, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CR212-{}", &self.suffix[..10])),
                    Value::from(&self.dept),
                    Value::Text(POOL_ACCOUNT.to_string()),
                    Value::Text(format!("{}-11-01", self.year)),
                ],
            )
            .await
            .expect("造分摊规则夹具");
            self.rules.push(id);
        }

        /// 造一条分摊结果（把池摊干净）。
        async fn allocate(&self, rule_id: Uuid, object_id: Uuid, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_result (id, period_code, rule_id, cost_object_id, dept_id, \
                                                     dept_name, account_code, basis_value, basis_total, \
                                                     pool_amount, alloc_amount, rounding_adj, created_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, '未归集夹具车间', ?, 10, 10, \
                         ?::text::numeric, ?::text::numeric, 0, now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(self.period_code()),
                    Value::from(&rule_id),
                    Value::from(&object_id),
                    Value::from(&self.dept),
                    Value::Text(POOL_ACCOUNT.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造分摊结果夹具");
        }

        /// 造本期批次（默认 `DRAFT`）。
        async fn batch(&mut self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(format!(
                        "CS212-{}-{}",
                        self.period_code().replace('-', ""),
                        &self.suffix[..4]
                    )),
                ],
            )
            .await
            .expect("造成本批次夹具");
            self.periods.push(id);
            if status != "DRAFT" {
                // 四道状态关联 CHECK：先插 DRAFT 再补齐列、最后改状态
                let mut sets = vec!["status = ?".to_string()];
                let mut params: Vec<Value> = vec![Value::Text(status.to_string())];
                sets.push("approval_instance_id = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
                if matches!(status, "SETTLED" | "CLOSED") {
                    sets.push("settled_at = now()".to_string());
                    sets.push("finish_voucher_id = ?::uuid".to_string());
                    params.push(Value::from(&Uuid::now_v7()));
                }
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

        /// 清场：先按前缀整片清（覆盖本夹具造的一切），再删主数据。
        async fn cleanup(&mut self) {
            let objects = self
                .objects
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            let periods = self
                .periods
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            let rules = self
                .rules
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            let mut statements: Vec<String> = Vec::new();
            if !rules.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_alloc_result WHERE rule_id IN ({rules})"));
            }
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_alloc_result WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!(
                    "DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!(
                    "DELETE FROM biz_cost_labor WHERE cost_object_id IN ({objects})"
                ));
            }
            statements.push(
                "DELETE FROM biz_cost_collect WHERE source_id IN \
                 (SELECT id FROM biz_cost_overhead WHERE remark = 'CB212-夹具')"
                    .to_string(),
            );
            statements.push("DELETE FROM biz_cost_overhead WHERE remark = 'CB212-夹具'".to_string());
            statements.push("DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-CB212-%'".to_string());
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            if !objects.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            if !rules.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_alloc_rule WHERE id IN ({rules})"));
            }
            statements.push(format!(
                "DELETE FROM biz_material WHERE material_code = 'CB212-{}'",
                self.suffix
            ));
            statements.push(format!(
                "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB212W%' AND warehouse_name LIKE '%{}%'",
                self.suffix
            ));
            for sql in statements {
                if let Err(error) = db().exec_sql(sql.clone(), vec![]).await {
                    println!("cleanup 失败（{sql}）：{error}");
                }
            }
            self.objects.clear();
            self.periods.clear();
            self.rules.clear();
            self.stock_outs.clear();
            self.overheads.clear();
        }
    }

    // ==================== 用例 ====================

    /// **验收①②**：四块各自只捞自己的缺口。
    #[tokio::test]
    async fn four_blocks_report_the_real_gaps() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;

        // ① 未挂对象的领料：两张 POSTED ISSUE（一张挂对象、一张 DRAFT 都**不该**进清单）
        let (gap_id, gap_no) = fixture.stock_out(None, "POSTED", "1200.0000").await;
        let _ = fixture.stock_out(None, "POSTED", "300.0000").await;
        let with_object = fixture.object().await.0;
        let _ = fixture.stock_out(Some(with_object), "POSTED", "999.0000").await;
        let _ = fixture.stock_out(None, "DRAFT", "0.0000").await;

        // ② 缺工时：obj_with_material 有料无工 ⇒ 缺口；obj_with_labor 有确认工时 ⇒ 不是缺口
        let obj_gap = fixture.object().await.0;
        fixture.material_collect(obj_gap, "5000.0000").await;
        let obj_ok = fixture.object().await.0;
        fixture.material_collect(obj_ok, "1000.0000").await;
        fixture.labor(obj_ok, "8", "25.5", "CONFIRMED").await;

        // ③ 缺费率：已确认但 rate = 0
        let zero_rate_id = fixture.labor(obj_ok, "4", "0", "CONFIRMED").await;

        // ④ 未摊完的池：1 张车间费用 3000，无规则
        fixture.pool_row("3000.0000").await;

        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口不该失败（设计：不阻断）");

        // ① 两张 POSTED ISSUE 未挂对象（挂对象的与草稿的不进清单）
        assert_eq!(res.stock_out_without_object.count, 2, "只数未挂对象的已过账领料");
        assert_eq!(res.stock_out_without_object.amount_total, "1500.0000");
        assert!(
            res.stock_out_without_object
                .rows
                .iter()
                .any(|row| row.stock_out_id == gap_id.to_string()
                    && row.doc_no == gap_no
                    && row.item_count == 1
                    && row.qty_total == "10.0000"),
            "缺口行必须带 id / 单号 / 明细数 / 数量：{:?}",
            res.stock_out_without_object.rows
        );

        // ② 只有 obj_gap 缺工时（obj_ok 有确认工时；with_object 也缺 —— 它只有领料单没归集行）
        let missing: Vec<&str> = res
            .labor_missing
            .rows
            .iter()
            .map(|row| row.object_no.as_str())
            .collect();
        assert_eq!(res.labor_missing.count, 1, "只有 obj_gap 进缺工时块：{missing:?}");
        assert_eq!(
            res.labor_missing.amount_total, "5000.0000",
            "在制金额口径 = 本期材料投入"
        );
        assert!(
            res.labor_missing.rows[0].hint.contains("本期没有工时记录"),
            "{}",
            res.labor_missing.rows[0].hint
        );

        // ③ 一行缺费率，工时 4
        assert_eq!(res.labor_rate_missing.count, 1);
        assert_eq!(res.labor_rate_missing.rows[0].labor_id, zero_rate_id.to_string());
        assert_eq!(
            res.labor_rate_missing.rows[0].rate, "0.000000",
            "工资率是 NUMERIC(18,6)，出参原样（前端按 6 位显示，不四舍五入）"
        );
        assert_eq!(res.labor_rate_missing.hours_total, "4.00");
        assert!(
            res.labor_rate_missing.rows[0].hint.contains("费率"),
            "{}",
            res.labor_rate_missing.rows[0].hint
        );

        // ④ 池：无规则
        assert_eq!(res.pool_unallocated.count, 1);
        assert_eq!(res.pool_unallocated.rows[0].reason, REASON_NO_RULE);
        assert_eq!(res.pool_unallocated.rows[0].remaining_amount, "3000.0000");
        assert_eq!(res.pool_unallocated.amount_total, "3000.0000");

        // 合计 = 四块之和
        assert_eq!(res.gap_count, 5, "2 + 1 + 1 + 1");
        assert_eq!(res.gap_count, res.total_gaps());
        assert!(!res.settled);

        fixture.cleanup().await;
    }

    /// **验收②**：「缺成本对象的领料」每行都有可点击跳转的来源单据。
    #[tokio::test]
    async fn stock_out_gap_carries_doc_id_and_no() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let (id, no) = fixture.stock_out(None, "POSTED", "88.5000").await;

        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        let row = res
            .stock_out_without_object
            .rows
            .iter()
            .find(|row| row.stock_out_id == id.to_string())
            .expect("刚造的单必须在清单里");

        assert_eq!(row.doc_no, no);
        assert_eq!(row.amount_total, "88.5000");
        assert_eq!(
            row.jump,
            format!("/business/erp/stock-out?out_no={no}"),
            "跳转要能定位到这张单"
        );
        assert_eq!(row.doc_date, format!("{}-11-07", fixture.year));
        assert!(row.hint.contains(&no), "怎么修要能点名单据：{}", row.hint);

        fixture.cleanup().await;
    }

    /// 池缺口三态推进（设计 §6.2.2 三条文案的判定依据）。
    #[tokio::test]
    async fn pool_reason_progresses_no_rule_object_allocated() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        // 池先有 2000，本车间**没有**在产对象 ⇒ 无规则
        fixture.pool_row("2000.0000").await;
        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        assert_eq!(res.pool_unallocated.rows[0].reason, REASON_NO_RULE);

        // 加规则（车间 + 4101.09）：有规则但车间没有在产对象 ⇒ 无对象
        fixture.rule().await;
        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        assert_eq!(
            res.pool_unallocated.rows[0].reason, REASON_NO_OBJECT,
            "有规则但车间无在产对象"
        );

        // 造一个本期有归集行的在产对象 ⇒ 变成「未摊完」
        let object = fixture.object().await.0;
        fixture.material_collect(object, "100.0000").await;
        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        assert_eq!(
            res.pool_unallocated.rows[0].reason, REASON_NOT_ALLOCATED,
            "有规则 + 有对象 ⇒ 只剩「还没摊」"
        );
        assert!(
            res.pool_unallocated.rows[0].jump.is_none(),
            "未摊完的动作在本页（点计算）"
        );

        // 写分摊结果把池摊干净 ⇒ 缺口消失（一分钱都不许剩）
        let rule_id = fixture.rules[0];
        fixture.allocate(rule_id, object, "2000.0000").await;
        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        assert_eq!(res.pool_unallocated.count, 0, "摊完就不该还在清单里");
        assert!(res.pool_unallocated.rows.is_empty());

        fixture.cleanup().await;
    }

    /// 设计 #33（按批次 id）与按期间入口出参一致；无批次 ⇒ `batch = None`；未知 id ⇒ 404。
    #[tokio::test]
    async fn by_batch_and_by_period_share_one_shape() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        fixture.stock_out(None, "POSTED", "50.0000").await;

        // ① 还没建批次：按期间查得到，batch = None
        let by_period = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("按期间自查");
        assert!(by_period.batch.is_none(), "批次没建之前不该有 batch");
        assert_eq!(by_period.stock_out_without_object.count, 1);
        assert_eq!(by_period.period_start, format!("{}-11-01", fixture.year));
        assert_eq!(by_period.period_end, format!("{}-11-30", fixture.year));

        // ② 建批次后按 id 查：批次信息 + 同一份缺口
        let batch_id = fixture.batch("DRAFT").await;
        let by_batch = cost_uncollected_handler::uncollected_by_batch(&batch_id.to_string())
            .await
            .expect("按批次自查");
        let brief = by_batch.batch.as_ref().expect("批次信息");
        assert_eq!(brief.id, batch_id.to_string());
        assert_eq!(brief.status, "DRAFT");
        assert_eq!(by_batch.period_code, by_period.period_code);
        assert_eq!(by_batch.gap_count, by_period.gap_count, "两条入口口径必须一致");

        // ③ 未知批次 ⇒ 404，且文案指向按期间入口
        let missing = Uuid::now_v7();
        let err = cost_uncollected_handler::uncollected_by_batch(&missing.to_string())
            .await
            .expect_err("不存在的批次必须 404");
        assert_eq!(err.status, 404);
        assert!(err.msg.contains("period_code="), "{}", err.msg);

        fixture.cleanup().await;
    }

    /// 已结算批次 ⇒ `settled = true`（缺口降级为历史解释）。
    #[tokio::test]
    async fn settled_batch_is_flagged() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let batch_id = fixture.batch("SETTLED").await;

        let res = cost_uncollected_handler::uncollected_by_batch(&batch_id.to_string())
            .await
            .expect("按批次自查");
        assert!(res.settled, "SETTLED 批次必须标记已结算");
        assert_eq!(res.batch.as_ref().expect("批次").status, "SETTLED");

        fixture.cleanup().await;
    }

    /// 已录未确认（DRAFT）≠ 没录：文案与字段都要区分。
    #[tokio::test]
    async fn draft_labor_is_reported_as_not_confirmed() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let object = fixture.object().await.0;
        fixture.material_collect(object, "700.0000").await;
        fixture.labor(object, "6", "30", "DRAFT").await;

        let res = cost_uncollected_handler::uncollected_by_period(&fixture.period_code())
            .await
            .expect("自查接口");
        let row = res
            .labor_missing
            .rows
            .iter()
            .find(|row| row.cost_object_id == object.to_string())
            .expect("只有草稿工时的对象也在缺工时清单里");
        assert_eq!(row.draft_labor_rows, 1);
        assert!(row.hint.contains("草稿工时"), "{}", row.hint);
        assert!(
            !row.hint.contains("本期没有工时记录"),
            "已录未确认不能说没录：{}",
            row.hint
        );
        assert_eq!(
            res.labor_rate_missing.count, 0,
            "DRAFT 工时不该进缺费率块（只认 CONFIRMED）"
        );

        fixture.cleanup().await;
    }

    /// 空期间 ⇒ 四块都是 0（接口永远 200，不阻断）。
    #[tokio::test]
    async fn empty_period_returns_zero_blocks() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        // 造一个完全没有数据的期间（12 月）
        let period = format!("{}-12", fixture.year);
        let res = cost_uncollected_handler::uncollected_by_period(&period)
            .await
            .expect("空期间也要 200");
        assert_eq!(res.gap_count, 0);
        assert_eq!(res.stock_out_without_object.count, 0);
        assert_eq!(res.labor_missing.count, 0);
        assert_eq!(res.labor_rate_missing.count, 0);
        assert_eq!(res.pool_unallocated.count, 0);
        assert_eq!(res.stock_out_without_object.kind, dto::KIND_STOCK_OUT);
        assert_eq!(res.pool_unallocated.kind, dto::KIND_POOL);
        assert!(!res.stock_out_without_object.truncated);

        fixture.cleanup().await;
    }
}
