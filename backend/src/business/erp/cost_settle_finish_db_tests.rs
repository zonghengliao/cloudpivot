//! 结算 ①「完工入库」的真库用例（`ERP_DB_URL` 开关）—— COST-B3-4。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_settle_finish_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面三行逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`production_stock_in_and_three_ledgers`] | **验收①②③一起**：`in_type='PRODUCTION'` + 直接 `POSTED` + 无采购单/无供应商/无应付；明细数量金额单价批号；`biz_inventory`（数量+金额+均价）/ `biz_inventory_location`（数量）/ `biz_lot`（批次）三本账同额；流水 `PRODUCTION_IN` 落**结算期间**；计算单回 `SETTLED` + 绑单；`null → POSTED` 状态日志 |
//! | 2 | [`second_settle_is_rejected_and_writes_nothing`] | 幂等闸 ①：同批次第二次 409 + `66074`，库存/入库单/流水一条都不多 |
//! | 3 | [`cancelled_stock_in_frees_recalc`] | 幂等闸 ① 排除 `CANCELLED`：红冲作废后同批次可重新结算（不撞死单） |
//! | 4 | [`zero_completed_qty_creates_nothing`] | 完工 0 件不建空入库单（`ck_biz_si_item_qty` 也要求 `qty > 0`），返回空清单 |
//! | 5 | [`missing_unit_is_rejected_without_writes`] | 产出品主数据缺失 ⇒ 400 + `10001`，零写入（不猜单位、不落 0） |
//! | 6 | [`inventory_accumulates_per_bucket`] | 两个对象同物料同仓 ⇒ 库位账两个桶（指定库位 + 未分配桶）分别加、成本账移动加权、流水两条、批次两条 |
//! | 7 | [`settled_item_is_not_picked_again`] | 幂等闸 ② 的读取侧：`SETTLED` 行不再进待入库清单 |
//!
//! # 夹具纪律
//!
//! 前缀 `CB34-*`（成本对象 / 物料）/ `CS34-*`（批次号）；期间年份按进程内计数器从 **2400** 起
//! （与 B2-11 的 2200+、B3-1 的 2031 都错开，并行跑不撞 `uk_biz_cost_period`）。
//! 清场顺序严格按外键：流水 → 入库明细 → 入库单 → 计算单 → 批次 → 成本对象 → 批次主数据
//! → 库位账 → 成本账 → 物料 / 仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_settle_finish::{self, SettleContext};
    use crate::shared::error_code;
    use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
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

    /// 期间年份：2400 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2400 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 400).unwrap_or(0)
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

    /// 库里读回的字符串金额 → `Decimal`。
    fn dec(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    /// 规范化成固定小数位再比（`NUMERIC(18,4)` 读回是 `"800.0000"`，算法算出来是 `"800"`）。
    fn money(value: &str, scale: usize) -> String {
        format!("{:.*}", scale, dec(value))
    }

    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        unit: Uuid,
        product: Uuid,
        warehouse: Uuid,
        /// `biz_inventory_location.id`（成本对象指定的完工入库库位行）
        location_row: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
        stock_ins: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（`CB34-*` / `CS34-*` 前缀）。每个夹具建之前跑一次。
    ///
    /// 全部**按前缀**限定：`biz_stock_in` 对批次是弱引用（无 FK），所以删批次前必须
    /// 先按 `cost_period_id` 把入库单 / 明细 / 流水 / 状态日志清掉，否则它们会变成孤儿行
    /// 留在库里（对别的卡的表零影响 —— 只碰本夹具的 `CS34-%` 批次）。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_inventory_txn WHERE doc_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS34-%'))",
            "DELETE FROM biz_status_log WHERE entity_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS34-%'))",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS34-%'))",
            "DELETE FROM biz_stock_in WHERE cost_period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS34-%')",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS34-%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS34-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB34-%'",
            "DELETE FROM biz_lot WHERE lot_no LIKE 'CB34LOT-%'",
            "DELETE FROM biz_inventory_location WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB34-%')",
            "DELETE FROM biz_inventory WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB34-%')",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB34-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB34W%'",
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
                    Value::Text(format!("CB34-{suffix}")),
                    Value::Text(format!("完工入库夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB34W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("完工入库夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");
            // 指定库位行（qty 从 0 开始；`add_location_qty` 的分支 A 按主键加）。
            // ★ `location_id` 必须**非空**：`location_id IS NULL` 的那一行是「未分配库位桶」
            //   （`uk_biz_inv_loc … NULLS NOT DISTINCT`），拿它当「指定库位」两个桶就混成一个了。
            let location_row = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, \
                                                     locked_qty, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0, 0, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&location_row),
                    Value::from(&product),
                    Value::from(&warehouse),
                    Value::from(&Uuid::now_v7()),
                ],
            )
            .await
            .expect("造库位账行夹具");
            Self {
                suffix,
                year,
                dept,
                unit,
                product,
                warehouse,
                location_row,
                objects: std::sync::Mutex::new(Vec::new()),
                periods: std::sync::Mutex::new(Vec::new()),
                stock_ins: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 本期期间码（11 月）。
        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        /// 批次号（`uk_biz_cost_period_no` 要求全库唯一）。
        fn batch_no(&self) -> String {
            format!("CS34-{}", &self.suffix[..8.min(self.suffix.len())])
        }

        /// 造一个成本对象（`completed_qty` = 累计完工；批号 / 库位可空）。
        async fn object(&self, lot_no: Option<&str>, location_id: Option<Uuid>) -> (Uuid, String) {
            self.object_of(self.product, lot_no, location_id).await
        }

        /// 指定产出品的成本对象（用于「物料主数据缺失」用例）。
        async fn object_of(&self, product: Uuid, lot_no: Option<&str>, location_id: Option<Uuid>) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB34-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, lot_no, target_warehouse_id, \
                                              target_location_id, plan_qty, completed_qty, wip_qty, \
                                              completion_pct, material_input_mode, workshop_dept_id, \
                                              workshop_dept_name, start_date, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, NULLIF(?::text,'')::varchar, ?::uuid, NULLIF(?::text,'')::uuid, 2000, 800, 200, 80, 'ONE_TIME', \
                         ?::uuid, '完工入库夹具车间', ?::text::date, 'OPEN', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&product),
                    Value::Text(lot_no.unwrap_or("").to_string()),
                    Value::from(&self.warehouse),
                    Value::Text(
                        location_id
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    ),
                    Value::from(&self.dept),
                    Value::Text(format!("{}-01-01", self.year)),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            (id, object_no)
        }

        /// 造本期批次（`APPROVED`：结算的前置状态；`ck_biz_cost_period_appr` 要求挂实例）。
        async fn period(&self) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, \
                                              approval_instance_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'APPROVED', 'MANUAL', ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(self.batch_no()),
                    Value::from(Uuid::now_v7()),
                ],
            )
            .await
            .expect("造结算批次夹具");
            self.periods.lock().expect("批次清单").push(id);
            id
        }

        /// 造一行计算单（默认 = 设计 §4.6.2 算例 A：完工 800 / 在制 200（50%））。
        ///
        /// 守恒式由 `ck_biz_cost_item_cons` 钉死：
        /// `finished + wip_end = wip_beg + mat + labor + oh` ⇒ 22311.1104 + 4688.8896 = 27000。
        async fn item(&self, period_id: Uuid, object_id: Uuid, product: Uuid) {
            self.item_with(period_id, object_id, product, "800", "22311.1104", "27.888888")
                .await
        }

        /// 指定数量 / 金额 / 单价的计算单（金额必须自洽：`finished + wip_end = 27000`）。
        async fn item_with(
            &self,
            period_id: Uuid,
            object_id: Uuid,
            product: Uuid,
            completed: &str,
            finished: &str,
            unit_finished: &str,
        ) {
            // wip_end 由守恒式反推（本期投入固定 27000：19000 + 4000 + 4000）
            let invested = dec("27000");
            let wip_end = invested - dec(finished);
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                   wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, \
                                                   completion_pct, equiv_material, equiv_conversion, \
                                                   mat_cur, labor_cur, oh_cur, unit_material, unit_labor, \
                                                   unit_overhead, finished_cost, wip_end_cost, \
                                                   unit_finished_cost, stock_in_id, status, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0, 0, ?::text::numeric, 200, 50, 1000, 900, \
                         19000, 4000, 4000, 19.000000, 4.444444, 4.444444, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, NULL, 'CALC', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&period_id),
                    Value::from(&object_id),
                    Value::from(&product),
                    Value::Text(completed.to_string()),
                    Value::Text(finished.to_string()),
                    Value::Text(wip_end.round_dp(4).to_string()),
                    Value::Text(unit_finished.to_string()),
                ],
            )
            .await
            .expect("造计算单夹具");
        }

        /// 跑一次完结入库（失败时打印报文，省一轮盲猜）。
        async fn run(&self, period_id: Uuid) -> Vec<cost_settle_finish::FinishStockIn> {
            let period_code = self.period_code();
            let batch_no = self.batch_no();
            let ctx = SettleContext {
                period_id,
                period_code: &period_code,
                batch_no: &batch_no,
                in_date: NaiveDate::from_ymd_opt(self.year, 11, 30).expect("入库日期"),
                operator: Some(self.dept),
                now: Utc::now(),
            };
            match cost_settle_finish::generate(db(), &ctx).await {
                Ok(rows) => {
                    let ids = rows.iter().map(|row| row.stock_in_id).collect::<Vec<_>>();
                    self.stock_ins.lock().expect("入库单清单").extend(ids);
                    rows
                }
                Err(error) => panic!("完工入库失败：{} / {}{}", error.status, error.code, error.msg),
            }
        }

        /// 清场：按外键顺序删（见模块头）。
        async fn cleanup(&self) {
            let joined = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let stocks = joined(&self.stock_ins);
            let periods = joined(&self.periods);
            let objects = joined(&self.objects);
            let mut statements = Vec::new();
            if !stocks.is_empty() {
                statements.push(format!("DELETE FROM biz_inventory_txn WHERE doc_id IN ({stocks})"));
                statements.push(format!("DELETE FROM biz_status_log WHERE entity_id IN ({stocks})"));
                statements.push(format!("DELETE FROM biz_stock_in_item WHERE stock_in_id IN ({stocks})"));
                statements.push(format!("DELETE FROM biz_stock_in WHERE id IN ({stocks})"));
            }
            if !periods.is_empty() {
                // 入库单可能没登记进 stock_ins（失败路径），按批次再扫一遍
                statements.push(format!(
                    "DELETE FROM biz_inventory_txn WHERE doc_id IN \
                        (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!(
                    "DELETE FROM biz_status_log WHERE entity_id IN \
                        (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!(
                    "DELETE FROM biz_stock_in_item WHERE stock_in_id IN \
                        (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!("DELETE FROM biz_stock_in WHERE cost_period_id IN ({periods})"));
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            if !objects.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            statements.push(format!(
                "DELETE FROM biz_lot WHERE material_id = '{}'::uuid",
                self.product
            ));
            statements.push(format!(
                "DELETE FROM biz_inventory_location WHERE material_id = '{}'::uuid",
                self.product
            ));
            statements.push(format!(
                "DELETE FROM biz_inventory WHERE material_id = '{}'::uuid",
                self.product
            ));
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

        /// 该批次的入库单条数（不看作废）。
        async fn stock_in_count(&self, period_id: Uuid) -> i64 {
            db().scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_stock_in WHERE cost_period_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&period_id)],
            )
            .await
            .expect("数入库单")
        }

        /// 取单列文本（金额列读回是规范化字符串）。
        async fn text(&self, sql: &str, params: Vec<Value>) -> String {
            db().scalar_with_params::<String>(sql, params)
                .await
                .unwrap_or_else(|error| panic!("SQL 取值失败（{sql}）：{error}"))
        }
    }

    // ==================== 用例 ====================

    /// ① ★ 卡面三行验收一次过：`PRODUCTION` + 三本账（数量/金额/批次）同额 + 计算单回写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn production_stock_in_and_three_ledgers() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let lot_no = format!("CB34LOT-{}", &fx.suffix[..8]);
        let (object_id, object_no) = fx.object(Some(&lot_no), Some(fx.location_row)).await;
        let period_id = fx.period().await;
        fx.item(period_id, object_id, fx.product).await;

        let rows = fx.run(period_id).await;
        assert_eq!(rows.len(), 1, "一个成本对象 = 一张完工入库单");
        let produced = &rows[0];
        assert_eq!(produced.cost_object_id, object_id);
        assert!(
            produced.in_no.starts_with("IN-"),
            "入库单号走 IN- 号段：{}",
            produced.in_no
        );
        assert_eq!(money(&produced.qty.to_string(), 4), "800.0000");
        assert_eq!(money(&produced.amount.to_string(), 4), "22311.1104");

        // ---------- 入库单头：类型 / 状态 / 来源 ----------
        let header = db()
            .query_first_sql(
                "SELECT in_type, status, cost_object_id, cost_period_id, source_order_id, supplier_id, \
                        is_sourceless, payable_id, voucher_id, invoice_received, warehouse_id, \
                        total_qty::text AS total_qty, total_amount::text AS total_amount, \
                        total_tax::text AS total_tax, total_with_tax::text AS total_with_tax, \
                        posted_at, posted_by, remark \
                   FROM biz_stock_in WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&produced.stock_in_id)],
            )
            .await
            .expect("读入库单")
            .expect("入库单必须存在");
        assert_eq!(
            header.get_typed::<String>("in_type").unwrap(),
            "PRODUCTION",
            "★ 卡面验收①：入库单类型 PRODUCTION"
        );
        assert_eq!(header.get_typed::<String>("status").unwrap(), "POSTED");
        assert_eq!(header.get_typed::<Uuid>("cost_object_id").unwrap(), object_id);
        assert_eq!(header.get_typed::<Uuid>("cost_period_id").unwrap(), period_id);
        assert!(
            header.get_typed::<Option<Uuid>>("source_order_id").unwrap().is_none(),
            "完工入库没有采购单（V0382 起库层放行）"
        );
        assert!(
            header.get_typed::<Option<Uuid>>("supplier_id").unwrap().is_none(),
            "完工入库没有供应商"
        );
        assert!(!header.get_typed::<bool>("is_sourceless").unwrap());
        assert!(
            header.get_typed::<Option<Uuid>>("payable_id").unwrap().is_none(),
            "★ 不走收货过账路径 ⇒ 不生成应付单"
        );
        assert!(
            header.get_typed::<Option<Uuid>>("voucher_id").unwrap().is_none(),
            "凭证是 B3-5 的卡，本卡不写"
        );
        assert!(!header.get_typed::<bool>("invoice_received").unwrap());
        assert_eq!(header.get_typed::<Uuid>("warehouse_id").unwrap(), fx.warehouse);
        assert_eq!(money(&header.get_typed::<String>("total_qty").unwrap(), 4), "800.0000");
        assert_eq!(
            money(&header.get_typed::<String>("total_amount").unwrap(), 4),
            "22311.1104"
        );
        assert_eq!(money(&header.get_typed::<String>("total_tax").unwrap(), 4), "0.0000");
        assert_eq!(
            money(&header.get_typed::<String>("total_with_tax").unwrap(), 4),
            "22311.1104",
            "ck_biz_si_tax：with_tax = amount + tax"
        );
        assert!(
            header
                .get_typed::<Option<DateTime<Utc>>>("posted_at")
                .unwrap()
                .is_some()
        );
        assert_eq!(header.get_typed::<Option<Uuid>>("posted_by").unwrap(), Some(fx.dept));
        let remark = header
            .get_typed::<Option<String>>("remark")
            .unwrap()
            .unwrap_or_default();
        assert!(remark.contains(&object_no), "备注带对象号：{remark}");

        // ---------- 明细：数量 / 金额 / 单价 / 批号 / 税率 ----------
        let line = db()
            .query_first_sql(
                "SELECT line_no, material_id, unit_id, qty::text AS qty, price::text AS price, \
                        price_incl::text AS price_incl, tax_rate::text AS tax_rate, \
                        amount::text AS amount, tax_amount::text AS tax_amount, \
                        with_tax_amount::text AS with_tax_amount, cost_price::text AS cost_price, \
                        location_id, lot_no \
                   FROM biz_stock_in_item WHERE stock_in_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&produced.stock_in_id)],
            )
            .await
            .expect("读入库明细")
            .expect("入库明细必须存在");
        assert_eq!(line.get_typed::<i32>("line_no").unwrap(), 1);
        assert_eq!(line.get_typed::<Uuid>("material_id").unwrap(), fx.product);
        assert_eq!(line.get_typed::<Uuid>("unit_id").unwrap(), fx.unit);
        assert_eq!(money(&line.get_typed::<String>("qty").unwrap(), 4), "800.0000");
        assert_eq!(
            money(&line.get_typed::<String>("cost_price").unwrap(), 6),
            "27.888888",
            "入库单价 = 计算单 unit_finished_cost"
        );
        assert_eq!(money(&line.get_typed::<String>("amount").unwrap(), 4), "22311.1104");
        assert_eq!(money(&line.get_typed::<String>("tax_rate").unwrap(), 4), "0.0000");
        assert_eq!(money(&line.get_typed::<String>("tax_amount").unwrap(), 4), "0.0000");
        assert_eq!(
            money(&line.get_typed::<String>("with_tax_amount").unwrap(), 4),
            "22311.1104"
        );
        assert_eq!(
            line.get_typed::<Option<Uuid>>("location_id").unwrap(),
            Some(fx.location_row)
        );
        assert_eq!(
            line.get_typed::<Option<String>>("lot_no").unwrap().as_deref(),
            Some(lot_no.as_str())
        );

        // ---------- 三本账 #1：成本账（数量 + 金额 + 均价） ----------
        let cost = db()
            .query_first_sql(
                "SELECT qty::text AS qty, amount::text AS amount, avg_cost::text AS avg_cost \
                   FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND location_id = ?::uuid"
                    .to_string(),
                vec![
                    Value::from(&fx.product),
                    Value::from(&fx.warehouse),
                    Value::from(&fx.location_row),
                ],
            )
            .await
            .expect("读成本账")
            .expect("成本账行必须存在");
        assert_eq!(money(&cost.get_typed::<String>("qty").unwrap(), 4), "800.0000");
        assert_eq!(money(&cost.get_typed::<String>("amount").unwrap(), 4), "22311.1104");
        assert_eq!(
            money(&cost.get_typed::<String>("avg_cost").unwrap(), 6),
            "27.888888",
            "均价 = 金额 / 数量（PG numeric 口径）"
        );

        // ---------- 三本账 #2：库位账（数量，加在成本对象指定的库位行上） ----------
        let loc = db()
            .scalar_with_params::<String>(
                "SELECT qty::text FROM biz_inventory_location WHERE id = ?::uuid",
                vec![Value::from(&fx.location_row)],
            )
            .await
            .expect("读库位账");
        assert_eq!(money(&loc, 4), "800.0000", "库位账与成本账同额");

        // ---------- 三本账 #3：批次账（biz_lot，生产批） ----------
        let lot = db()
            .query_first_sql(
                "SELECT source_type, source_doc_type, source_doc_id, source_doc_no, material_id \
                   FROM biz_lot WHERE material_id = ?::uuid AND lot_no = ? AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::from(&fx.product), Value::Text(lot_no.clone())],
            )
            .await
            .expect("读批次主数据")
            .expect("批次主数据必须存在（成本对象有批号）");
        assert_eq!(lot.get_typed::<String>("source_type").unwrap(), "PRODUCTION");
        assert_eq!(lot.get_typed::<String>("source_doc_type").unwrap(), "COST_PERIOD");
        assert_eq!(lot.get_typed::<Uuid>("source_doc_id").unwrap(), period_id);
        assert_eq!(
            lot.get_typed::<Option<String>>("source_doc_no").unwrap().as_deref(),
            Some(fx.batch_no().as_str())
        );

        // ---------- 流水：PRODUCTION_IN / IN / STOCK_IN / 落结算期间 ----------
        let txn = db()
            .query_first_sql(
                "SELECT txn_type, biz_type, doc_type, doc_id, doc_no, line_no, qty::text AS qty, \
                        amount::text AS amount, unit_cost::text AS unit_cost, \
                        before_qty::text AS before_qty, after_qty::text AS after_qty, \
                        after_amount::text AS after_amount, period_code \
                   FROM biz_inventory_txn WHERE doc_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&produced.stock_in_id)],
            )
            .await
            .expect("读库存流水")
            .expect("库存流水必须存在");
        assert_eq!(txn.get_typed::<String>("txn_type").unwrap(), "IN");
        assert_eq!(
            txn.get_typed::<String>("biz_type").unwrap(),
            "PRODUCTION_IN",
            "词表里的生产入库（不是 PURCHASE_IN）"
        );
        assert_eq!(txn.get_typed::<String>("doc_type").unwrap(), "STOCK_IN");
        assert_eq!(txn.get_typed::<String>("doc_no").unwrap(), produced.in_no);
        assert_eq!(money(&txn.get_typed::<String>("qty").unwrap(), 4), "800.0000");
        assert_eq!(money(&txn.get_typed::<String>("before_qty").unwrap(), 4), "0.0000");
        assert_eq!(money(&txn.get_typed::<String>("after_qty").unwrap(), 4), "800.0000");
        assert_eq!(
            money(&txn.get_typed::<String>("after_amount").unwrap(), 4),
            "22311.1104"
        );
        assert_eq!(
            txn.get_typed::<Option<String>>("period_code").unwrap().as_deref(),
            Some(fx.period_code().as_str()),
            "流水落会计期间（不是「今天」）"
        );

        // ---------- 计算单回写：SETTLED + 绑单 ----------
        let item = db()
            .query_first_sql(
                "SELECT status, stock_in_id FROM biz_cost_period_item \
                  WHERE period_id = ?::uuid AND cost_object_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&period_id), Value::from(&object_id)],
            )
            .await
            .expect("读计算单")
            .expect("计算单必存在");
        assert_eq!(item.get_typed::<String>("status").unwrap(), "SETTLED");
        assert_eq!(
            item.get_typed::<Option<Uuid>>("stock_in_id").unwrap(),
            Some(produced.stock_in_id)
        );

        // ---------- 状态日志：null → POSTED（建单留痕） ----------
        let log = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_id = ?::uuid AND entity_type = 'STOCK_IN' \
                    AND from_status IS NULL AND to_status = 'POSTED'",
                vec![Value::from(&produced.stock_in_id)],
            )
            .await
            .expect("数状态日志");
        assert_eq!(log, 1, "建单留痕：null → POSTED 一条");

        // ---------- 反查接口：按批次取回入库单 ----------
        let ids = cost_settle_finish::stock_in_ids_of(db(), period_id)
            .await
            .expect("按批次反查入库单");
        assert_eq!(ids, vec![produced.stock_in_id], "红冲要按 cost_period_id 反查");

        fx.cleanup().await;
    }

    /// ② 幂等闸 ①：同批次第二次结算 ⇒ 409 + `66074`，一条账都不多。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_settle_is_rejected_and_writes_nothing() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _no) = fx.object(Some("CB34LOT-SECOND"), None).await;
        let period_id = fx.period().await;
        fx.item(period_id, object_id, fx.product).await;
        let first = fx.run(period_id).await;
        assert_eq!(first.len(), 1);

        let period_code = fx.period_code();
        let batch_no = fx.batch_no();
        let ctx = SettleContext {
            period_id,
            period_code: &period_code,
            batch_no: &batch_no,
            in_date: NaiveDate::from_ymd_opt(fx.year, 11, 30).expect("入库日期"),
            operator: Some(fx.dept),
            now: Utc::now(),
        };
        let error = cost_settle_finish::generate(db(), &ctx)
            .await
            .expect_err("第二次结算必须被拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_FINISH_ALREADY_SETTLED);
        assert_eq!(error.code, 66074);

        assert_eq!(fx.stock_in_count(period_id).await, 1, "入库单没有变多");
        let qty = fx
            .text(
                "SELECT qty::text FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&fx.product), Value::from(&fx.warehouse)],
            )
            .await;
        assert_eq!(money(&qty, 4), "800.0000", "库存没有翻倍");
        let txns = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_inventory_txn WHERE material_id = ?::uuid",
                vec![Value::from(&fx.product)],
            )
            .await
            .expect("数流水");
        assert_eq!(txns, 1, "流水没有多写");

        fx.cleanup().await;
    }

    /// ③ 幂等闸 ① 排除 `CANCELLED`：红冲作废后同批次可重新结算（不撞死单）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_stock_in_frees_recalc() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _no) = fx.object(Some("CB34LOT-CANCEL"), None).await;
        let period_id = fx.period().await;
        fx.item(period_id, object_id, fx.product).await;
        let first = fx.run(period_id).await;

        // 模拟 B3-7 红冲后的形态：入库单 CANCELLED + 计算单回 CALC 且解绑
        db().exec_sql(
            "UPDATE biz_stock_in SET status = 'CANCELLED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&first[0].stock_in_id)],
        )
        .await
        .expect("作废入库单");
        db().exec_sql(
            "UPDATE biz_cost_period_item SET status = 'CALC', stock_in_id = NULL \
              WHERE period_id = ?::uuid"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await
        .expect("计算单回退");

        let second = fx.run(period_id).await;
        assert_eq!(second.len(), 1, "作废后可以重新结算");
        assert_ne!(second[0].stock_in_id, first[0].stock_in_id);
        assert_eq!(fx.stock_in_count(period_id).await, 2, "一作废 + 一新生");
        let live = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_stock_in WHERE cost_period_id = ?::uuid \
                  AND deleted_at IS NULL AND status <> 'CANCELLED'",
                vec![Value::from(&period_id)],
            )
            .await
            .expect("数有效入库单");
        assert_eq!(live, 1, "有效入库单只有一张");

        fx.cleanup().await;
    }

    /// ④ 完工 0 件不建空入库单，返回空清单（本期无产出是合法状态）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_completed_qty_creates_nothing() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _no) = fx.object(None, None).await;
        let period_id = fx.period().await;
        // 完工 0 / 全部留在在制：finished 0 + wip_end 27000 = 27000
        fx.item_with(period_id, object_id, fx.product, "0", "0", "0").await;

        let rows = fx.run(period_id).await;
        assert!(rows.is_empty(), "没有完工数量 ⇒ 不生成入库单");
        assert_eq!(fx.stock_in_count(period_id).await, 0);
        let status = fx
            .text(
                "SELECT status FROM biz_cost_period_item WHERE period_id = ?::uuid",
                vec![Value::from(&period_id)],
            )
            .await;
        assert_eq!(status, "CALC", "计算单保持 CALC（没入库就不许标 SETTLED）");

        fx.cleanup().await;
    }

    /// ⑤ 产出品主数据缺失（物料行不存在）⇒ 400 + `10001`，零写入。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_unit_is_rejected_without_writes() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let ghost_product = Uuid::now_v7(); // 故意不建 biz_material
        let (object_id, _no) = fx.object_of(ghost_product, None, None).await;
        let period_id = fx.period().await;
        fx.item(period_id, object_id, ghost_product).await;

        let period_code = fx.period_code();
        let batch_no = fx.batch_no();
        let ctx = SettleContext {
            period_id,
            period_code: &period_code,
            batch_no: &batch_no,
            in_date: NaiveDate::from_ymd_opt(fx.year, 11, 30).expect("入库日期"),
            operator: Some(fx.dept),
            now: Utc::now(),
        };
        let error = cost_settle_finish::generate(db(), &ctx)
            .await
            .expect_err("物料缺失必须报错，不能猜单位");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("计量单位"), "报文要点名缺什么：{}", error.msg);
        assert_eq!(fx.stock_in_count(period_id).await, 0, "零写入");
        let status = fx
            .text(
                "SELECT status FROM biz_cost_period_item WHERE period_id = ?::uuid",
                vec![Value::from(&period_id)],
            )
            .await;
        assert_eq!(status, "CALC");

        fx.cleanup().await;
    }

    /// ⑥ 三个对象同物料同仓：同桶累加（移动加权）+ 未分配桶独立 + 流水/批次逐条。
    ///
    /// `biz_inventory` 的唯一键是 `(material, warehouse, COALESCE(location_id, 零uuid))`
    /// （`uk_biz_inv_mat_wh_loc`）—— 成本账**也按库位分行**，不是「一个物料一行」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inventory_accumulates_per_bucket() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let lot_a = format!("CB34LOT-A{}", &fx.suffix[..6]);
        let lot_b = format!("CB34LOT-B{}", &fx.suffix[..6]);
        let lot_c = format!("CB34LOT-C{}", &fx.suffix[..6]);
        let (obj_a, _) = fx.object(Some(&lot_a), Some(fx.location_row)).await;
        let (obj_b, _) = fx.object(Some(&lot_b), Some(fx.location_row)).await;
        let (obj_c, _) = fx.object(Some(&lot_c), None).await;
        let period_id = fx.period().await;
        // A：800 件 / 22311.1104（指定库位）；B：100 件 / 1000.0000（同库位，同桶累加）；
        // C：50 件 / 500.0000（未填库位 ⇒ 未分配桶）
        fx.item(period_id, obj_a, fx.product).await;
        fx.item_with(period_id, obj_b, fx.product, "100", "1000.0000", "10.000000")
            .await;
        fx.item_with(period_id, obj_c, fx.product, "50", "500.0000", "10.000000")
            .await;

        let rows = fx.run(period_id).await;
        assert_eq!(rows.len(), 3, "三个对象 = 三张入库单");
        let (qty, amount) = cost_settle_finish::FinishStockIn::totals(&rows);
        assert_eq!(money(&qty.to_string(), 4), "950.0000");
        assert_eq!(
            money(&amount.to_string(), 4),
            "23811.1104",
            "合计 = 三张入库单金额之和（B3-6 回写 total_finished 用）"
        );

        // 成本账（指定库位桶）：两笔同桶累加 + 移动加权
        let cost = db()
            .query_first_sql(
                "SELECT qty::text AS qty, amount::text AS amount, avg_cost::text AS avg_cost \
                   FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND location_id = ?::uuid"
                    .to_string(),
                vec![
                    Value::from(&fx.product),
                    Value::from(&fx.warehouse),
                    Value::from(&fx.location_row),
                ],
            )
            .await
            .expect("读成本账")
            .expect("成本账行（指定库位桶）");
        assert_eq!(money(&cost.get_typed::<String>("qty").unwrap(), 4), "900.0000");
        assert_eq!(money(&cost.get_typed::<String>("amount").unwrap(), 4), "23311.1104");
        assert_eq!(
            money(&cost.get_typed::<String>("avg_cost").unwrap(), 6),
            "25.901234",
            "23311.1104 / 900 = 25.90123378 → 6 位四舍五入"
        );

        // 成本账（未分配库位桶）：独立一行
        let ghost = db()
            .query_first_sql(
                "SELECT qty::text AS qty, amount::text AS amount \
                   FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND location_id IS NULL"
                    .to_string(),
                vec![Value::from(&fx.product), Value::from(&fx.warehouse)],
            )
            .await
            .expect("读成本账")
            .expect("成本账行（未分配桶）");
        assert_eq!(money(&ghost.get_typed::<String>("qty").unwrap(), 4), "50.0000");
        assert_eq!(money(&ghost.get_typed::<String>("amount").unwrap(), 4), "500.0000");

        // 库位账：指定库位 900 + 未分配桶 50（与成本账逐桶对齐）
        let loc = fx
            .text(
                "SELECT qty::text FROM biz_inventory_location WHERE id = ?::uuid",
                vec![Value::from(&fx.location_row)],
            )
            .await;
        assert_eq!(money(&loc, 4), "900.0000");
        let bucket = fx
            .text(
                "SELECT qty::text FROM biz_inventory_location \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
                vec![Value::from(&fx.product), Value::from(&fx.warehouse)],
            )
            .await;
        assert_eq!(money(&bucket, 4), "50.0000", "未分配库位桶");

        // 流水 3 条、批次 3 条
        let txns = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_inventory_txn \
                  WHERE material_id = ?::uuid AND biz_type = 'PRODUCTION_IN'",
                vec![Value::from(&fx.product)],
            )
            .await
            .expect("数流水");
        assert_eq!(txns, 3);
        let lots = db()
            .scalar_with_params::<i64>(
                "SELECT count(*) FROM biz_lot WHERE material_id = ?::uuid AND source_type = 'PRODUCTION'",
                vec![Value::from(&fx.product)],
            )
            .await
            .expect("数批次");
        assert_eq!(lots, 3, "三个对象三个批号 → 三条批次主数据");

        fx.cleanup().await;
    }

    /// ⑦ 幂等闸 ② 的读取侧：已 `SETTLED` 的行不再进待入库清单。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settled_item_is_not_picked_again() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _no) = fx.object(None, None).await;
        let period_id = fx.period().await;
        fx.item(period_id, object_id, fx.product).await;
        let pending = cost_settle_finish::pending_lines(db(), period_id)
            .await
            .expect("读待入库清单");
        assert_eq!(pending.len(), 1);

        fx.run(period_id).await;
        let after = cost_settle_finish::pending_lines(db(), period_id)
            .await
            .expect("读待入库清单");
        assert!(after.is_empty(), "结算后不再有待入库行");

        fx.cleanup().await;
    }
}
