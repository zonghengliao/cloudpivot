//! 红冲（红字凭证 + 库存回冲 + 入库单作废 + `REVERSED`）的真库用例（`ERP_DB_URL` 开关）—— COST-B3-7。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同一口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_reverse_run_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面五行逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`reverse_end_to_end_in_one_transaction`] | **验收①–④**：三/四张凭证**全部红冲**（原凭证仍 `POSTED` + `reversed_by`）、库存回冲（`PRODUCE_CANCEL` 流水 + 成本账/库位账回到 0）、生产入库单 `CANCELLED`、批次 `REVERSED`、计算单回 `CALC`；4001/4101 本期净发生归零（蓝红同期互抵） |
//! | 2 | [`reverse_then_recalc_does_not_hit_the_unique_key`] | **验收⑤**：红冲后同期间**新建批次 + 真跑 calc**，`uk_biz_cost_period` 不冲突；再插第二张活跃批次仍被唯一键拒（证明不是把键删了） |
//! | 3 | [`reverse_is_rejected_when_stock_already_shipped`] | 库存退不回来（先出库 700 件）⇒ 409 + `66078` + 设计 §4.8 的「走销售退货 / 盘亏」文案，**整笔零写入**（无红字凭证 / 入库单仍 `POSTED` / 批次仍 `SETTLED` / 库存不动） |
//! | 4 | [`reverse_requires_settled_status`] | 状态闸：`APPROVED` 批次红冲 ⇒ 409 + `66073`，零写入 |
//! | 5 | [`reverse_requires_reason_of_at_least_10_chars`] | 原因闸：`None` / 9 字 ⇒ 400 `10001`，零写入 |
//! | 6 | [`second_reverse_is_rejected`] | 幂等：红冲成功后同批次再红冲 ⇒ 409 + `66073`，凭证 / 入库单条数不变 |
//!
//! # 夹具纪律
//!
//! 前缀 `CB37-*`（成本对象 / 物料）/ `CS37-*`（批次号）/ `FIX37-*`（夹具领料凭证单号）/
//! `CB37R-*`（分摊规则）；期间年份按进程内计数器从 **3300** 起（B3-4 用 2400+、B3-5 用 2600+、
//! B3-6 用 2800+，本卡 3300–3599 与它们全部错开，并行跑不撞 `uk_biz_cost_period`）。
//! 清场顺序严格按外键：流水 → 状态日志 → 入库明细 → 入库单 → 凭证分录 → 凭证 → 分摊结果 →
//! 分摊规则 → 计算单 → 归集行 → 台账 → 批次 → 成本对象 → 批次主数据 → 库位账 → 成本账 →
//! 物料 / 仓库 → 夹具车间。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_item;
    use crate::business::erp::cost_reverse_run::{self as rev, ReverseInput};
    use crate::business::erp::cost_settle_finish;
    use crate::business::erp::cost_settle_run::{self as run};
    use crate::business::erp::cost_settle_voucher as cv;
    use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher, STATUS_POSTED};
    use crate::shared::error_code;
    use nik::prelude::chrono::{NaiveDate, Utc};
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

    /// 期间年份：3300 + 进程内序号（每个夹具独占一个会计年度；与 B3-4/5/6 的 2400/2600/2800 段错开）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        3300 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 300).unwrap_or(0)
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

    fn dec(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn money(value: Decimal) -> String {
        format!("{:.*}", 4, value)
    }

    /// 夹具（一个用例一套：独立车间 / 物料 / 仓库 / 期间）。
    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
        ledgers: std::sync::Mutex<Vec<Uuid>>,
        rules: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（全部按前缀限定，绝不动别人的数据）。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_inventory_txn WHERE doc_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')) \
                OR period_code IN (SELECT period_code FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')",
            "DELETE FROM biz_status_log WHERE entity_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')) \
                OR entity_id IN (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%'))",
            "DELETE FROM biz_stock_in WHERE cost_period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')",
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN (\
                SELECT id FROM biz_voucher WHERE source_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%') \
                   OR source_no LIKE 'FIX37-%')",
            "DELETE FROM biz_voucher WHERE source_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%') \
                OR source_no LIKE 'FIX37-%'",
            "DELETE FROM biz_cost_alloc_result WHERE rule_id IN \
                (SELECT id FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB37R-%') \
                OR period_code IN (SELECT period_code FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')",
            "DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB37R-%'",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS37-%')",
            "DELETE FROM biz_cost_collect WHERE dept_name LIKE 'CB37车间%' \
                OR dept_id IN (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB37车间%')",
            "DELETE FROM biz_cost_overhead WHERE dept_name LIKE 'CB37车间%' \
                OR dept_id IN (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB37车间%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS37-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB37-%'",
            "DELETE FROM biz_lot WHERE lot_no LIKE 'CB37LOT-%'",
            "DELETE FROM biz_inventory_location WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB37-%')",
            "DELETE FROM biz_inventory WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB37-%')",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB37-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB37W%'",
            "DELETE FROM sys_dept WHERE dept_name LIKE 'CB37车间%'",
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
            // 自建车间：池行的 `cost_object_id` 为空，只有「夹具车间」能当清理标记（B3-5 经验）
            let dept = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 1, 0, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&dept),
                    Value::Text(format!("CB37D{}", tail_code(&suffix))),
                    Value::Text(format!("CB37车间-{}", &suffix[..8.min(suffix.len())])),
                ],
            )
            .await
            .expect("造夹具车间");
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
                    Value::Text(format!("CB37-{suffix}")),
                    Value::Text(format!("结算编排夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB37W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("结算编排夹具仓 {suffix}")),
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
                ledgers: std::sync::Mutex::new(Vec::new()),
                rules: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        fn batch_no(&self) -> String {
            format!("CS37-{}", &self.suffix[..8.min(self.suffix.len())])
        }

        fn voucher_date(&self) -> NaiveDate {
            NaiveDate::from_ymd_opt(self.year, 11, 30).expect("凭证日期")
        }

        /// 造一个成本对象（产出品 = 夹具物料，完工入库到夹具仓）。
        async fn object(&self) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB37-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 2000, 800, 200, 80, 'ONE_TIME', \
                         ?::uuid, 'CB37车间', ?::text::date, 'OPEN', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
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
        async fn period(&self, status: &str, labor_source: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, \
                                              approval_instance_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, ?, ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(self.batch_no()),
                    Value::Text(status.to_string()),
                    Value::Text(labor_source.to_string()),
                    Value::from(Uuid::now_v7()),
                ],
            )
            .await
            .expect("造结算批次夹具");
            self.periods.lock().expect("批次清单").push(id);
            id
        }

        /// 造计算单行（默认 = 设计 §4.6.2 算例 A：完工 800 / 在制 200（50%））。
        ///
        /// 守恒式由 `ck_biz_cost_item_cons` 钉死：
        /// `finished + wip_end = wip_beg + mat + labor + oh` ⇒ 22311.1104 + 4688.8896 = 27000。
        async fn item(&self, period_id: Uuid, object_id: Uuid) -> Uuid {
            self.item_with(period_id, object_id, "800", "19000", "4000", "4000", "22311.1104")
                .await
        }

        /// 指定数量 / 金额的计算单（单位成本与算例 A 同：19 / 4.444444 / 4.444444）。
        #[allow(clippy::too_many_arguments)]
        async fn item_with(
            &self,
            period_id: Uuid,
            object_id: Uuid,
            completed: &str,
            mat: &str,
            labor: &str,
            oh: &str,
            finished: &str,
        ) -> Uuid {
            let id = Uuid::now_v7();
            let invested = dec(mat) + dec(labor) + dec(oh);
            let wip_end = invested - dec(finished);
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                   wip_beg_qty, wip_beg_cost, completed_qty, wip_end_qty, \
                                                   completion_pct, equiv_material, equiv_conversion, \
                                                   mat_cur, labor_cur, oh_cur, unit_material, unit_labor, \
                                                   unit_overhead, finished_cost, wip_end_cost, \
                                                   unit_finished_cost, stock_in_id, status, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0, 0, ?::text::numeric, 200, 50, 1000, 900, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         19.000000, 4.444444, 4.444444, \
                         ?::text::numeric, ?::text::numeric, 27.888888, NULL, 'CALC', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&period_id),
                    Value::from(&object_id),
                    Value::from(&self.product),
                    Value::Text(completed.to_string()),
                    Value::Text(mat.to_string()),
                    Value::Text(labor.to_string()),
                    Value::Text(oh.to_string()),
                    Value::Text(finished.to_string()),
                    Value::Text(money(wip_end)),
                ],
            )
            .await
            .expect("造计算单夹具");
            id
        }

        /// 造一行工归集行（`element='LABOR'`，按车间）。
        async fn labor_collect(&self, object_id: Uuid, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, line_no, occur_date, amount, dept_id, dept_name, \
                                               created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'LABOR', 'LABOR_TICKET', ?::uuid, 0, ?::text::date, \
                         ?::text::numeric, ?::uuid, 'CB37车间', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object_id),
                    Value::Text(self.period_code()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(format!("{}-11-15", self.year)),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造工归集夹具");
        }

        /// 造一条手工费用台账 + 它的池归集行（`cost_object_id` 为空 ⇒ 池行）。
        async fn ledger_row(&self, debit_account: &str, credit_account: &str, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULL, ?::uuid, 'CB37车间', 'OTHER', ?, ?, \
                         ?::text::numeric, 'MANUAL', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::from(&self.dept),
                    Value::Text(debit_account.to_string()),
                    Value::Text(credit_account.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造手工费用台账夹具");
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, line_no, occur_date, amount, dept_id, dept_name, \
                                               created_at, updated_at) \
                 VALUES (?::uuid, NULL, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, 0, ?::text::date, \
                         ?::text::numeric, ?::uuid, 'CB37车间', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(self.period_code()),
                    Value::from(&id),
                    Value::Text(format!("{}-11-20", self.year)),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造费归集夹具");
            self.ledgers.lock().expect("台账清单").push(id);
            id
        }

        /// 造一条分摊结果（`Σ alloc_amount` 必须等于池合计，否则断言 6 必红）。
        async fn alloc_result(&self, rule_id: Uuid, object_id: Uuid, account: &str, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_result (id, period_code, rule_id, cost_object_id, dept_id, \
                                                    dept_name, account_code, basis_value, basis_total, \
                                                    pool_amount, alloc_amount, rounding_adj, create_by) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, 'CB37车间', ?, 500, 1000, 4000, \
                         ?::text::numeric, 0, ?::uuid)"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::from(&rule_id),
                    Value::from(&object_id),
                    Value::from(&self.dept),
                    Value::Text(account.to_string()),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造分摊结果夹具");
            id
        }

        /// 造一条分摊规则（`4101.01`，按工时；只当分摊结果的 FK 用）。
        async fn alloc_rule(&self, account: &str) -> Uuid {
            let id = Uuid::now_v7();
            let rule_code = format!("CB37R-{}", tail_code(&self.suffix));
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, biz_type, source_dept_id, \
                                                  source_dept_name, account_code, basis, priority, \
                                                  effective_from, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, '结算编排夹具规则', 'OVERHEAD', ?::uuid, 'CB37车间', ?, \
                         'LABOR_HOURS', 10, ?::text::date, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("{rule_code}-{}", &id.to_string()[28..])),
                    Value::from(&self.dept),
                    Value::Text(account.to_string()),
                    Value::Text(format!("{}-01-01", self.year)),
                ],
            )
            .await
            .expect("造分摊规则夹具");
            self.rules.lock().expect("规则清单").push(id);
            id
        }

        async fn account(&self, code: &str) -> AccountRef {
            voucher::account_by_code(db(), code)
                .await
                .expect("取科目")
                .unwrap_or_else(|| panic!("科目 {code} 不存在"))
        }

        /// 夹具前置账：领料凭证「借 4001.01 / 贷 1405」（§4.7.5 第 0 张，B3-8 的真实口径）。
        async fn material_voucher(&self, amount: &str) {
            let debit = self.account("4001.01").await;
            let credit = self.account("1405").await;
            let line = |no: i32, summary: &str, account: AccountRef, debit: Decimal, credit: Decimal| EntryLine {
                line_no: no,
                summary: summary.to_string(),
                account,
                debit,
                credit,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            };
            voucher::post_voucher(
                db(),
                &PostVoucher {
                    voucher_type: "AUTO",
                    source_type: "STOCK_OUT",
                    source_id: None,
                    source_no: Some(Box::leak(
                        format!("FIX37-{}", &self.suffix[..8.min(self.suffix.len())]).into_boxed_str(),
                    )),
                    source_doc_no: None,
                    voucher_date: self.voucher_date(),
                    summary: "FIX37 领料凭证夹具".to_string(),
                    lines: vec![
                        line(1, "生产领料", debit, dec(amount), Decimal::ZERO),
                        line(2, "生产领料", credit, Decimal::ZERO, dec(amount)),
                    ],
                    remark: None,
                    red_of: None,
                    status: STATUS_POSTED,
                },
                Some(self.dept),
                Utc::now(),
            )
            .await
            .expect("夹具领料凭证");
        }

        /// 走生产入口结算（`settle_period`：自己的事务，成功提交 / 失败回滚）。
        async fn settle(&self, period_id: Uuid) -> std::result::Result<run::SettleRun, ApiError> {
            run::settle_period(period_id, Some(self.dept)).await
        }

        /// 批次单头快照（断言用）。
        async fn period_row(&self, period_id: Uuid) -> PeriodSnapshot {
            let row = db()
                .query_first_sql(
                    "SELECT status, settled_at IS NOT NULL AS settled, finish_voucher_id, \
                            labor_voucher_id, overhead_voucher_id, object_count, \
                            total_material::text AS total_material, total_labor::text AS total_labor, \
                            total_overhead::text AS total_overhead, total_cost::text AS total_cost, \
                            total_finished::text AS total_finished, total_wip::text AS total_wip \
                       FROM biz_cost_period WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读批次单头")
                .expect("批次存在");
            PeriodSnapshot {
                status: row.get_typed("status").expect("status"),
                settled: row.get_typed("settled").expect("settled"),
                finish_voucher_id: row.get_typed("finish_voucher_id").expect("finish_voucher_id"),
                labor_voucher_id: row.get_typed("labor_voucher_id").expect("labor_voucher_id"),
                overhead_voucher_id: row.get_typed("overhead_voucher_id").expect("overhead_voucher_id"),
                object_count: row.get_typed("object_count").expect("object_count"),
                total_material: row.get_typed("total_material").expect("total_material"),
                total_labor: row.get_typed("total_labor").expect("total_labor"),
                total_overhead: row.get_typed("total_overhead").expect("total_overhead"),
                total_cost: row.get_typed("total_cost").expect("total_cost"),
                total_finished: row.get_typed("total_finished").expect("total_finished"),
                total_wip: row.get_typed("total_wip").expect("total_wip"),
            }
        }

        /// 本批次（未作废）的入库单（数量, 金额）合计。
        async fn stock_in_totals(&self, period_id: Uuid) -> (Decimal, Decimal) {
            let row = db()
                .query_first_sql(
                    "SELECT COALESCE(sum(total_qty), 0)::text AS qty, \
                            COALESCE(sum(total_amount), 0)::text AS amount \
                       FROM biz_stock_in WHERE cost_period_id = ?::uuid \
                        AND deleted_at IS NULL AND status <> 'CANCELLED'"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读入库合计")
                .expect("合计行");
            (
                row.get_typed::<String>("qty").expect("qty").parse().expect("qty"),
                row.get_typed::<String>("amount")
                    .expect("amount")
                    .parse()
                    .expect("amount"),
            )
        }

        /// 某个期间科目净额（借 − 贷，只算 POSTED；含红字凭证）。
        async fn period_balance(&self, code: &str) -> Decimal {
            let row = db()
                .query_first_sql(
                    "SELECT COALESCE(sum(e.debit_amount - e.credit_amount), 0)::text AS net \
                       FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                      WHERE v.period = ? AND v.status = 'POSTED' AND v.deleted_at IS NULL \
                        AND e.account_code = ?"
                        .to_string(),
                    vec![Value::Text(self.period_code()), Value::Text(code.to_string())],
                )
                .await
                .expect("读期间科目净额")
                .expect("净额行");
            row.get_typed::<String>("net").expect("net").parse().expect("净额金额")
        }

        async fn stock_in_count(&self, period_id: Uuid) -> i64 {
            db().query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_stock_in WHERE cost_period_id = ?::uuid".to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("查入库单条数")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count")
        }

        async fn status_log_count(&self, period_id: Uuid) -> i64 {
            db().query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("查状态日志")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count")
        }

        async fn inventory_amount(&self) -> Decimal {
            let row = db()
                .query_first_sql(
                    "SELECT COALESCE(sum(amount), 0)::text AS amount FROM biz_inventory \
                      WHERE material_id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&self.product)],
                )
                .await
                .expect("读库存金额")
                .expect("合计行");
            row.get_typed::<String>("amount")
                .expect("amount")
                .parse()
                .expect("金额")
        }

        async fn inventory_qty(&self) -> Decimal {
            let row = db()
                .query_first_sql(
                    "SELECT COALESCE(sum(qty), 0)::text AS qty FROM biz_inventory \
                      WHERE material_id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&self.product)],
                )
                .await
                .expect("读库存数量")
                .expect("合计行");
            row.get_typed::<String>("qty").expect("qty").parse().expect("数量")
        }

        /// 清场：按外键顺序删（见模块头）。空清单的 `IN ()` 会语法错，所以逐条判断。
        async fn cleanup(&self) {
            let joined = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let periods = joined(&self.periods);
            let objects = joined(&self.objects);
            let ledgers = joined(&self.ledgers);
            let rules = joined(&self.rules);
            let mut statements: Vec<String> = Vec::new();
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_inventory_txn WHERE doc_id IN \
                        (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!(
                    "DELETE FROM biz_status_log WHERE entity_id IN ({periods}) \
                        OR entity_id IN (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!(
                    "DELETE FROM biz_stock_in_item WHERE stock_in_id IN \
                        (SELECT id FROM biz_stock_in WHERE cost_period_id IN ({periods}))"
                ));
                statements.push(format!("DELETE FROM biz_stock_in WHERE cost_period_id IN ({periods})"));
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
            }
            statements.push(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                    (SELECT id FROM biz_voucher WHERE source_no LIKE 'FIX37-%')"
                    .to_string(),
            );
            statements.push("DELETE FROM biz_voucher WHERE source_no LIKE 'FIX37-%'".to_string());
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                        (SELECT id FROM biz_voucher WHERE source_id IN ({periods}))"
                ));
                statements.push(format!("DELETE FROM biz_voucher WHERE source_id IN ({periods})"));
            }
            if !rules.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_alloc_result WHERE rule_id IN ({rules})"));
                statements.push(format!("DELETE FROM biz_cost_alloc_rule WHERE id IN ({rules})"));
            }
            statements.push(format!(
                "DELETE FROM biz_cost_collect WHERE dept_id = '{}'::uuid OR dept_name LIKE 'CB37车间%'",
                self.dept
            ));
            if ledgers.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_overhead WHERE dept_id = '{}'::uuid",
                    self.dept
                ));
            } else {
                statements.push(format!(
                    "DELETE FROM biz_cost_overhead WHERE id IN ({ledgers}) OR dept_id = '{}'::uuid",
                    self.dept
                ));
            }
            if !periods.is_empty() {
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
            statements.push(format!("DELETE FROM sys_dept WHERE id = '{}'::uuid", self.dept));
            for sql in statements {
                if let Err(error) = db().exec_sql(sql.clone(), vec![]).await {
                    println!("cleanup 失败（{sql}）：{error}");
                }
            }
        }
    }

    /// 批次单头快照。
    #[derive(Debug, Clone)]
    struct PeriodSnapshot {
        status: String,
        settled: bool,
        finish_voucher_id: Option<Uuid>,
        labor_voucher_id: Option<Uuid>,
        overhead_voucher_id: Option<Uuid>,
        object_count: i32,
        total_material: String,
        total_labor: String,
        total_overhead: String,
        total_cost: String,
        total_finished: String,
        total_wip: String,
    }

    /// 一套标准算例 A 夹具：对象 + 批次 + 计算单 + 工归集 + 两张池台账 + 两条分摊结果。
    ///
    /// 返回 `(period_id, object_id)`。`with_material` = 是否补一张领料凭证（4001.01 借方 19000）。
    async fn standard_case(fixture: &Fixture, with_material: bool, labor_source: &str) -> (Uuid, Uuid) {
        let period_id = fixture.period("APPROVED", labor_source).await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;
        fixture.labor_collect(object_id, "4000").await;
        fixture.ledger_row("4101.01", "2241.01", "3000").await;
        fixture.ledger_row("4101.02", "2241.01", "1000").await;
        let rule = fixture.alloc_rule("4101.01").await;
        fixture.alloc_result(rule, object_id, "4101.01", "3000").await;
        fixture.alloc_result(rule, object_id, "4101.02", "1000").await;
        if with_material {
            fixture.material_voucher("19000").await;
        }
        (period_id, object_id)
    }

    fn assert_error_code(error: &ApiError, expected: i32) {
        assert_eq!(error.code, expected, "错误码（报文：{}）", error.msg);
    }

    // ==================== 红冲专用夹具 / 断言助手 ====================

    /// 红冲用例的固定原因（≥10 字；同时被红字凭证摘要 / 入库单作废原因 / 批次原因引用）。
    const REASON: &str = "期间产量录错，整批红冲后重算";

    impl Fixture {
        /// 造一行料归集行（`element='MATERIAL'`）。
        ///
        /// 红冲后「真跑 calc」要求对象有完工就必须有材料（否则 `66068` 有完工无材料），
        /// 而 B3-6 的夹具只造了工 / 费归集行 —— 本卡补这一行（不改 B3-6 的用例）。
        async fn material_collect(&self, object_id: Uuid, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, line_no, occur_date, material_id, qty, amount, \
                                               dept_id, dept_name, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, 0, ?::text::date, \
                         ?::uuid, 950, ?::text::numeric, ?::uuid, 'CB37车间', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object_id),
                    Value::Text(self.period_code()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(format!("{}-11-10", self.year)),
                    Value::from(&self.product),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造料归集夹具");
        }

        /// 走生产入口红冲（`reverse_period`：自己的事务，成功提交 / 失败回滚）。
        async fn reverse(&self, period_id: Uuid, reason: &str) -> std::result::Result<rev::ReverseRun, ApiError> {
            rev::reverse_period(period_id, Some(reason), Some(self.dept)).await
        }

        /// 同期间再建一张 `DRAFT` 批次（= 设计 §5.2 #27「不存在则建 DRAFT」那一步）。
        async fn draft_period(&self, suffix: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', 'MANUAL', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(format!("CS37-{suffix}")),
                ],
            )
            .await
            .expect("同期间再建 DRAFT 批次（uk_biz_cost_period 必须排除 REVERSED）");
            self.periods.lock().expect("批次清单").push(id);
            id
        }

        /// 一张凭证的六元组：（状态, is_red, reverse_of, reversed_by, 期间, 借合计）。
        async fn voucher_facts(&self, id: Uuid) -> (String, i16, Option<Uuid>, Option<Uuid>, String, String) {
            let row = db()
                .query_first_sql(
                    "SELECT status, is_red, reverse_of, reversed_by, period, total_debit::text AS debit \
                       FROM biz_voucher WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&id)],
                )
                .await
                .expect("读凭证")
                .expect("凭证存在");
            (
                row.get_typed("status").expect("status"),
                row.get_typed("is_red").expect("is_red"),
                row.get_typed("reverse_of").expect("reverse_of"),
                row.get_typed("reversed_by").expect("reversed_by"),
                row.get_typed("period").expect("period"),
                row.get_typed("debit").expect("debit"),
            )
        }

        /// 本批次的**红字**凭证条数（`is_red = 1`）。
        async fn red_voucher_count(&self, period_id: Uuid) -> i64 {
            db().query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_voucher \
                  WHERE source_type = 'COST_PERIOD' AND source_id = ?::uuid \
                    AND deleted_at IS NULL AND is_red = 1"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("查红字凭证条数")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count")
        }

        /// 入库单三列：（status, cancelled_at 非空, cancel_reason）。
        async fn stock_in_facts(&self, period_id: Uuid) -> Vec<(String, bool, Option<String>)> {
            let rows = db()
                .query_sql(
                    "SELECT status, cancelled_at IS NOT NULL AS cancelled, cancel_reason \
                       FROM biz_stock_in WHERE cost_period_id = ?::uuid AND deleted_at IS NULL \
                       ORDER BY in_no"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读入库单");
            let mut facts = Vec::with_capacity(rows.len());
            for row in rows.iter() {
                facts.push((
                    row.get_typed("status").expect("status"),
                    row.get_typed("cancelled").expect("cancelled"),
                    row.get_typed("cancel_reason").expect("cancel_reason"),
                ));
            }
            facts
        }

        /// 计算单两列：（status, stock_in_id）。
        async fn item_facts(&self, period_id: Uuid) -> Vec<(String, Option<Uuid>)> {
            let rows = db()
                .query_sql(
                    "SELECT status, stock_in_id FROM biz_cost_period_item \
                      WHERE period_id = ?::uuid ORDER BY cost_object_id"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读计算单");
            let mut facts = Vec::with_capacity(rows.len());
            for row in rows.iter() {
                facts.push((
                    row.get_typed("status").expect("status"),
                    row.get_typed("stock_in_id").expect("stock_in_id"),
                ));
            }
            facts
        }

        /// 本批次入库单的流水条数（按方向 + 业务类型）。
        async fn txn_count(&self, period_id: Uuid, txn_type: &str, biz_type: &str) -> i64 {
            db().query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_inventory_txn t \
                  WHERE t.txn_type = ? AND t.biz_type = ? \
                    AND t.doc_id IN (SELECT id FROM biz_stock_in WHERE cost_period_id = ?::uuid)"
                    .to_string(),
                vec![
                    Value::Text(txn_type.to_string()),
                    Value::Text(biz_type.to_string()),
                    Value::from(&period_id),
                ],
            )
            .await
            .expect("查流水条数")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count")
        }

        /// 批次的红冲三列：（reversed_at 非空, reverse_reason, settled_at 非空）。
        async fn reverse_facts(&self, period_id: Uuid) -> (bool, Option<String>, bool) {
            let row = db()
                .query_first_sql(
                    "SELECT reversed_at IS NOT NULL AS reversed, reverse_reason, \
                            settled_at IS NOT NULL AS settled \
                       FROM biz_cost_period WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&period_id)],
                )
                .await
                .expect("读批次红冲列")
                .expect("批次存在");
            (
                row.get_typed("reversed").expect("reversed_at"),
                row.get_typed("reverse_reason").expect("reverse_reason"),
                row.get_typed("settled").expect("settled_at"),
            )
        }

        /// 库位账数量合计（本夹具物料）。
        async fn location_qty(&self) -> Decimal {
            let row = db()
                .query_first_sql(
                    "SELECT COALESCE(sum(qty), 0)::text AS qty FROM biz_inventory_location \
                      WHERE material_id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&self.product)],
                )
                .await
                .expect("读库位账")
                .expect("合计行");
            row.get_typed::<String>("qty").expect("qty").parse().expect("数量")
        }

        /// 模拟「这批货已经出库 700 件」（真实走唯一写入口：成本账 -700）。
        async fn ship_out(&self, qty: &str, amount: &str) {
            crate::business::erp::inventory_writer::subtract_cost(
                db(),
                self.product,
                self.warehouse,
                None,
                dec(qty),
                dec(amount),
                Utc::now(),
            )
            .await
            .expect("模拟出库（成本账减法）");
        }

        /// 红冲失败的「零写入」判据：无红字凭证 / 入库单仍 POSTED / 批次仍 SETTLED /
        /// 计算单仍 SETTLED 且绑着入库单 / 无反向流水 / 状态日志只剩结算那一条。
        async fn assert_no_reverse_writes(&self, period_id: Uuid) {
            assert_eq!(self.red_voucher_count(period_id).await, 0, "失败的红冲不应生成红字凭证");
            let stock = self.stock_in_facts(period_id).await;
            assert!(
                stock
                    .iter()
                    .all(|(status, cancelled, _)| status == "POSTED" && !cancelled),
                "失败的红冲不应作废入库单：{stock:?}"
            );
            let snapshot = self.period_row(period_id).await;
            assert_eq!(snapshot.status, "SETTLED", "失败的红冲不应改批次状态");
            let (reversed, reason, settled) = self.reverse_facts(period_id).await;
            assert!(!reversed, "失败的红冲不应写 reversed_at");
            assert!(reason.is_none(), "失败的红冲不应写 reverse_reason：{reason:?}");
            assert!(settled, "settled_at 必须原样保留");
            let items = self.item_facts(period_id).await;
            assert!(
                items.iter().all(|(status, id)| status == "SETTLED" && id.is_some()),
                "失败的红冲不应把计算单打回 CALC：{items:?}"
            );
            assert_eq!(
                self.txn_count(period_id, rev::TXN_TYPE_OUT, rev::BIZ_TYPE_PRODUCE_CANCEL)
                    .await,
                0,
                "失败的红冲不应写反向流水"
            );
            assert_eq!(self.status_log_count(period_id).await, 1, "失败的红冲不应写状态日志");
        }
    }

    /// 该批次未红冲的蓝字凭证 id（红冲前拍快照用）。
    async fn blue_ids(period_id: Uuid) -> Vec<Uuid> {
        cv::voucher_ids_of(db(), period_id).await.expect("读蓝字凭证")
    }

    // ==================== 用例 1：端到端（验收①–④） ====================

    /// **验收①–④**：三/四张凭证全部红冲（保留原凭证）+ 库存回冲 + 入库单作废 +
    /// 计算单回 `CALC` + 批次 `REVERSED`；4001.02/.03 与 4101.* 本期净发生归零。
    #[tokio::test]
    async fn reverse_end_to_end_in_one_transaction() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;
        let settled = fixture.settle(period_id).await.expect("结算应成功");
        let blue = blue_ids(period_id).await;
        assert_eq!(blue.len(), 4, "算例 A 结算出四张凭证（人工 / 台账 / 费用 / 完工）");
        assert_eq!(fixture.inventory_qty().await, dec("800"), "结算后库存 800");

        let run = fixture.reverse(period_id, REASON).await.expect("红冲应成功");

        // ---------- ① 逐凭证红冲：四张蓝字 → 四张红字 ----------
        assert_eq!(run.vouchers.len(), 4, "三/四张凭证必须**全部**红冲，不是删");
        assert_eq!(run.stock_ins.len(), 1, "一个成本对象一张入库单");
        assert_eq!(run.totals(), (dec("800"), dec("22311.1104")), "回冲数量金额 = 原入库");
        assert_eq!(run.reset_items, 1, "计算单回到 CALC");
        assert_eq!(fixture.red_voucher_count(period_id).await, 4);
        for pair in &run.vouchers {
            let (status, is_red, reverse_of, reversed_by, period, debit) =
                fixture.voucher_facts(pair.original_id).await;
            assert_eq!(status, "POSTED", "原凭证保持 POSTED（红冲不是改状态）");
            assert_eq!(is_red, 0);
            assert_eq!(reversed_by, Some(pair.red_id), "原凭证必须打上红字凭证指针");
            assert!(reverse_of.is_none());
            assert_eq!(period, fixture.period_code(), "蓝字在当期");

            let (red_status, red_is_red, red_of, red_reversed_by, red_period, red_debit) =
                fixture.voucher_facts(pair.red_id).await;
            assert_eq!(red_status, "POSTED");
            assert_eq!(red_is_red, 1, "红字凭证 is_red=1");
            assert_eq!(red_of, Some(pair.original_id), "红字凭证反向指向原凭证");
            assert!(red_reversed_by.is_none(), "红字凭证不能再被红冲");
            assert_eq!(
                red_period,
                fixture.period_code(),
                "红字凭证必须落在**被红冲的同一期间**（否则本期净发生对不平）"
            );
            assert_eq!(red_debit, debit, "红字凭证借贷合计 = 原凭证（借贷互换、金额不变）");
            println!("[红冲] {} → {}（合计 {debit}）", pair.original_no, pair.red_no);
        }
        // 该批次已没有「未红冲的蓝字」：重结时 ensure_not_settled 不会被它误拦
        assert!(blue_ids(period_id).await.is_empty(), "红冲后不应还有未红冲的蓝字凭证");

        // ---------- ② 库存回冲（成本账 / 库位账 / 流水） ----------
        assert_eq!(fixture.inventory_qty().await, Decimal::ZERO, "成本账数量回冲为 0");
        assert_eq!(fixture.inventory_amount().await, Decimal::ZERO, "成本账金额回冲为 0");
        assert_eq!(fixture.location_qty().await, Decimal::ZERO, "库位账数量回冲为 0");
        assert_eq!(
            fixture
                .txn_count(period_id, rev::TXN_TYPE_OUT, rev::BIZ_TYPE_PRODUCE_CANCEL)
                .await,
            1,
            "反向流水：OUT + PRODUCE_CANCEL"
        );
        assert_eq!(
            fixture.txn_count(period_id, "IN", "PRODUCTION_IN").await,
            1,
            "原入库流水保留（只增不改）"
        );

        // ---------- ③ 生产入库单作废 ----------
        let stock = fixture.stock_in_facts(period_id).await;
        assert_eq!(stock.len(), 1);
        let (status, cancelled, reason) = &stock[0];
        assert_eq!(status, "CANCELLED", "生产入库单作废");
        assert!(cancelled, "cancelled_at 必须有值");
        assert_eq!(reason.as_deref(), Some(REASON), "作废原因 = 红冲原因（可追溯）");
        let (qty_after, amount_after) = fixture.stock_in_totals(period_id).await;
        assert_eq!(qty_after, Decimal::ZERO, "作废的入库单不再计入「已入库」合计");
        assert_eq!(amount_after, Decimal::ZERO);

        // ---------- ④ 计算单回 CALC + 批次 REVERSED ----------
        for (status, stock_in_id) in fixture.item_facts(period_id).await {
            assert_eq!(status, "CALC", "红冲后对象回到未结算（可改数重算）");
            assert!(stock_in_id.is_none(), "stock_in_id 必须清空（否则重结会被 66074 拦）");
        }
        let snapshot = fixture.period_row(period_id).await;
        assert_eq!(snapshot.status, "REVERSED");
        assert!(snapshot.settled, "settled_at 保留（ck_biz_cost_period_settle 要求）");
        assert_eq!(
            snapshot.finish_voucher_id,
            Some(settled.vouchers.finish),
            "完工凭证指针保留（审计要能顺着找到红了哪张蓝字）"
        );
        // 结算回写的合计与凭证指针也保留（红冲只加 reversed_*，不擦结算痕迹）
        assert_eq!(snapshot.object_count, 1);
        assert_eq!(snapshot.total_cost, "27000.0000");
        assert_eq!(snapshot.total_finished, "22311.1104");
        assert_eq!(snapshot.total_wip, "4688.8896");
        assert_eq!(snapshot.labor_voucher_id, settled.vouchers.labor);
        assert_eq!(snapshot.overhead_voucher_id, settled.vouchers.overhead);
        assert_eq!(snapshot.total_material, "19000.0000");
        assert_eq!(snapshot.total_labor, "4000.0000");
        assert_eq!(snapshot.total_overhead, "4000.0000");
        let (reversed, reverse_reason, _) = fixture.reverse_facts(period_id).await;
        assert!(reversed, "reversed_at 必须有值（ck_biz_cost_period_rev）");
        assert_eq!(reverse_reason.as_deref(), Some(REASON));
        let log = db()
            .query_first_sql(
                "SELECT from_status, to_status, reason FROM biz_status_log \
                  WHERE entity_id = ?::uuid ORDER BY created_at DESC, id DESC LIMIT 1"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("读状态日志")
            .expect("状态日志存在");
        assert_eq!(log.get_typed::<String>("from_status").expect("from"), "SETTLED");
        assert_eq!(log.get_typed::<String>("to_status").expect("to"), "REVERSED");
        assert_eq!(
            log.get_typed::<Option<String>>("reason").expect("reason").as_deref(),
            Some(REASON),
            "红冲边必须留原因"
        );
        // 入库单也有一条 POSTED → CANCELLED 的日志（作废留痕）
        let stock_logs: i64 = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_status_log \
                  WHERE entity_type = 'STOCK_IN' AND from_status = 'POSTED' AND to_status = 'CANCELLED' \
                    AND entity_id IN (SELECT id FROM biz_stock_in WHERE cost_period_id = ?::uuid)"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("读入库单日志")
            .expect("count")
            .get_typed::<i64>("n")
            .expect("count");
        assert_eq!(stock_logs, 1, "入库单作废必须留状态日志");

        // ---------- ⑤ 账面对平：只被本批次动过的科目，蓝+红同期互抵后归零 ----------
        assert_eq!(
            fixture.period_balance("4001.02").await,
            Decimal::ZERO,
            "人工：借 4000 贷 4000 冲平"
        );
        assert_eq!(fixture.period_balance("4001.03").await, Decimal::ZERO, "制造费用冲平");
        assert_eq!(
            fixture.period_balance("4101.01").await,
            Decimal::ZERO,
            "池 4101.01 冲平"
        );
        assert_eq!(
            fixture.period_balance("4101.02").await,
            Decimal::ZERO,
            "池 4101.02 冲平"
        );
        // 4001.01 只剩**夹具领料凭证**那笔 19000（它不属于本批次，不在红冲范围 —— 真实链路里
        // 由领料单作废 / 红字领料凭证冲销，B3-8 的职责）
        assert_eq!(
            fixture.period_balance("4001.01").await,
            dec("19000.0000"),
            "4001.01 = 只剩未红冲的领料凭证（本批次完工凭证已冲平）"
        );

        fixture.cleanup().await;
    }

    // ==================== 用例 2：红冲后可重算（验收⑤） ====================

    /// **验收⑤**：红冲后同期间再建批次 + **真跑 calc**，`uk_biz_cost_period` 不冲突；
    /// 且「再插第二张活跃批次」仍被唯一键拒 —— 证明不冲突不是因为把键删了。
    #[tokio::test]
    async fn reverse_then_recalc_does_not_hit_the_unique_key() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, object_id) = standard_case(&fixture, true, "MANUAL").await;
        // 料归集行：红冲后重算必须「有完工也有材料」（否则 66068）
        fixture.material_collect(object_id, "19000").await;
        fixture.settle(period_id).await.expect("结算应成功");
        fixture.reverse(period_id, REASON).await.expect("红冲应成功");

        // ---------- 同期间再建批次：uk 排除 REVERSED ⇒ 成功 ----------
        let second = fixture.draft_period("R02").await;
        assert_ne!(second, period_id);
        // 活跃批次只剩新的那张（红冲的不占位）
        let active: Vec<Uuid> = {
            let rows = db()
                .query_sql(
                    "SELECT id FROM biz_cost_period WHERE period_code = ? AND deleted_at IS NULL \
                        AND status NOT IN ('REVERSED','CANCELLED')"
                        .to_string(),
                    vec![Value::Text(fixture.period_code())],
                )
                .await
                .expect("查活跃批次");
            rows.iter().map(|row| row.get_typed("id").expect("id")).collect()
        };
        assert_eq!(active, vec![second], "红冲批次不再占位，新批次是唯一活跃批次");

        // ---------- 真跑 calc（第 6 段）：写新计算单 ----------
        let outcome = cost_calc_item::calc_items(&fixture.period_code(), Some(fixture.dept))
            .await
            .expect("红冲后重算必须成功（唯一键不许冲突）");
        assert_eq!(outcome.items.len(), 1, "重算写出一条计算单");
        assert_eq!(outcome.batch_no, "CS37-R02".to_string());
        assert_eq!(outcome.total_material, dec("19000"), "料来自新造的归集行");
        let new_items = fixture.item_facts(second).await;
        assert_eq!(new_items.len(), 1, "新批次有计算单");
        assert_eq!(new_items[0].0, "CALC");
        // 旧（红冲）批次的 calc 行仍在（审计），且仍是 CALC 未绑单
        assert_eq!(fixture.item_facts(period_id).await.len(), 1);

        // ---------- 再插第二张活跃批次：唯一键仍然生效 ----------
        let clash = db()
            .exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, 'CS37-R03', 'DRAFT', 'MANUAL', now(), now())"
                    .to_string(),
                vec![Value::from(&Uuid::now_v7()), Value::Text(fixture.period_code())],
            )
            .await;
        assert!(
            clash.is_err(),
            "同期间第二张活跃批次必须被 uk_biz_cost_period 拒（否则「一个期间一个批次」的护栏没了）"
        );

        fixture.cleanup().await;
    }

    // ==================== 用例 3：库存退不回来 ⇒ 整笔回滚 ====================

    /// 货已出库 700 件（成本账只剩 100）⇒ 红冲要退 800 ⇒ 409 + `66078`，
    /// **整笔零写入**（无红字凭证 / 入库单仍 POSTED / 批次仍 SETTLED）。
    #[tokio::test]
    async fn reverse_is_rejected_when_stock_already_shipped() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;
        fixture.settle(period_id).await.expect("结算应成功");
        fixture.ship_out("700", "19522.2216").await;
        assert_eq!(fixture.inventory_qty().await, dec("100"), "出库后只剩 100 件");

        let error = fixture
            .reverse(period_id, REASON)
            .await
            .expect_err("库存不足必须拒绝红冲");
        assert_error_code(&error, error_code::ERP_COST_REVERSE_BLOCKED);
        assert_eq!(error.status, 409, "业务边界是 409 不是 500：{}", error.msg);
        assert!(error.msg.contains("已出库"), "报文要点名货已出库：{}", error.msg);
        assert!(error.msg.contains("销售退货"), "报文要给下一步：{}", error.msg);

        fixture.assert_no_reverse_writes(period_id).await;
        assert_eq!(fixture.inventory_qty().await, dec("100"), "失败的红冲不许动库存");
        assert_eq!(fixture.inventory_amount().await, dec("2788.8888"), "库存金额原样");

        fixture.cleanup().await;
    }

    // ==================== 用例 4–6：三道闸 ====================

    /// 状态闸：`APPROVED`（未结算）批次不能红冲 ⇒ 409 + `66073`，零写入。
    #[tokio::test]
    async fn reverse_requires_settled_status() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await; // 不结算

        let error = fixture.reverse(period_id, REASON).await.expect_err("未结算不能红冲");
        assert_error_code(&error, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert!(error.msg.contains("APPROVED"), "报文点名当前态：{}", error.msg);
        assert!(error.msg.contains("SETTLED"), "报文点名允许态：{}", error.msg);
        assert_eq!(fixture.red_voucher_count(period_id).await, 0);
        assert_eq!(fixture.stock_in_count(period_id).await, 0);
        // 只有状态日志 0 条（从未结算过）
        assert_eq!(fixture.status_log_count(period_id).await, 0);

        fixture.cleanup().await;
    }

    /// 原因闸：`None` / 9 个字 ⇒ 400 `10001`（红冲影响已入账数据，原因要能追溯）。
    #[tokio::test]
    async fn reverse_requires_reason_of_at_least_10_chars() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;
        fixture.settle(period_id).await.expect("结算应成功");

        let none = rev::reverse_period(period_id, None, Some(fixture.dept))
            .await
            .expect_err("无原因必须拒");
        assert_error_code(&none, error_code::PARAM_INVALID);
        let short = fixture.reverse(period_id, "算错了").await.expect_err("9 字以内必须拒");
        assert_error_code(&short, error_code::PARAM_INVALID);
        assert!(short.msg.contains("10"), "报文要给长度要求：{}", short.msg);

        fixture.assert_no_reverse_writes(period_id).await;
        // 场景补充：够 10 字就放行（同一批次）
        fixture.reverse(period_id, REASON).await.expect("够长应放行");

        fixture.cleanup().await;
    }

    /// 幂等：红冲成功后同批次再红冲 ⇒ 409 + `66073`，凭证 / 入库单条数不变。
    #[tokio::test]
    async fn second_reverse_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;
        fixture.settle(period_id).await.expect("结算应成功");
        let first = fixture.reverse(period_id, REASON).await.expect("第一次红冲成功");
        let reds = fixture.red_voucher_count(period_id).await;
        assert_eq!(reds, 4);

        let error = fixture.reverse(period_id, REASON).await.expect_err("已红冲不能再红冲");
        assert_error_code(&error, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert!(error.msg.contains("REVERSED"), "报文点名 REVERSED：{}", error.msg);
        assert_eq!(fixture.red_voucher_count(period_id).await, reds, "红字凭证条数不变");
        assert_eq!(
            fixture
                .txn_count(period_id, rev::TXN_TYPE_OUT, rev::BIZ_TYPE_PRODUCE_CANCEL)
                .await,
            1,
            "反向流水条数不变"
        );
        assert_eq!(fixture.stock_in_facts(period_id).await.len(), first.stock_ins.len());

        fixture.cleanup().await;
    }

    // ---------- 常量对齐（顺带钉住 B3-4/B3-5/B3-6 的口径，避免上游漂移） ----------

    #[test]
    fn fixture_constants_are_aligned() {
        assert_eq!(rev::TXN_TYPE_OUT, "OUT");
        assert_eq!(rev::BIZ_TYPE_PRODUCE_CANCEL, "PRODUCE_CANCEL");
        assert_eq!(cost_settle_finish::IN_TYPE_PRODUCTION, "PRODUCTION");
        assert_eq!(cv::ACCOUNT_FINISHED_GOODS, "1405");
        assert_eq!(cost_settle_finish::STOCK_IN_CANCELLED, "CANCELLED");
        // ReverseInput 只在生产路径构造；这里确认字段语义没被上游改掉
        let _ = std::mem::size_of::<ReverseInput<'static>>();
    }
}
