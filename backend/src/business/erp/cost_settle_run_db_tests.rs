//! 结算编排（状态回写 + 事务内 6 条断言）的真库用例（`ERP_DB_URL` 开关）—— COST-B3-6。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_settle_run_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面三行逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`settle_end_to_end_in_one_transaction`] | **验收①**：一次事务内 入库（1 张 `PRODUCTION` 单 + 三本账）+ 四张凭证 + 状态回写（`SETTLED` / 三个 voucher_id / 合计 / 对象数 / 状态日志），且 **6 条断言逐条通过**；4001 三末级余额 = 3800 / 444.4448 / 444.4448，4101 清零（设计 §4.7.5） |
//! | 2 | [`sixth_assertion_rolls_back_everything`] | **验收②+③**：写完但断言前删一行分摊结果 ⇒ 只有第 6 条（`C-COST-4·守恒`）红、409+`66077`；事务回滚后 **入库单 0 张 / 凭证 0 张 / 库存未动 / 批次仍 APPROVED / 被删的分摊结果也回来了** |
//! | 3 | [`settle_requires_approved_status`] | 状态闸：`DRAFT` 批次结算 ⇒ 409 + `66073`，零写入 |
//! | 4 | [`missing_material_debit_is_caught_by_the_wip_assertion`] | 断言 4 的正面证据：没有领料凭证（4001.01 借方缺失）⇒ 409 + `66077`（`C-COST-3`），零写入 —— 「料钱没进在制」不许结算 |
//! | 5 | [`bypassed_4101_entry_is_caught_before_settling`] | 断言 1 的正面证据：有人绕过成本模块记了一笔 4101 ⇒ 409 + `66065`（`C-COST-1` 池 ↔ 4101 不平），零写入 |
//! | 6 | [`second_settle_is_rejected`] | 幂等：结算成功后同批次再结算 ⇒ 409 + `66073`，凭证 / 入库单条数不变 |
//! | 7 | [`payroll_source_settles_with_three_vouchers`] | `labor_source='PAYROLL'`：人工计提不出（D-13），剩 3 张凭证，6 条断言照样全过 |
//!
//! # 夹具纪律
//!
//! 前缀 `CB36-*`（成本对象 / 物料）/ `CS36-*`（批次号）/ `FIX36-*`（夹具领料凭证单号）/
//! `CB36R-*`（分摊规则）；期间年份按进程内计数器从 **2800** 起（与 B3-4 的 2400+、B3-5 的
//! 2600+ 都错开，并行跑不撞 `uk_biz_cost_period`）。
//! 清场顺序严格按外键：流水 → 状态日志 → 入库明细 → 入库单 → 凭证分录 → 凭证 → 分摊结果 →
//! 分摊规则 → 计算单 → 归集行 → 台账 → 批次 → 成本对象 → 批次主数据 → 库位账 → 成本账 →
//! 物料 / 仓库 → 夹具车间。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_settle_finish;
    use crate::business::erp::cost_settle_run::{self as run, ASSERT_IDS, ASSERT_NAMES, SettleInput};
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

    /// 期间年份：2800 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2800 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 400).unwrap_or(0)
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
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')) \
                OR period_code IN (SELECT period_code FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')",
            "DELETE FROM biz_status_log WHERE entity_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')) \
                OR entity_id IN (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id IN (\
                SELECT id FROM biz_stock_in WHERE cost_period_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%'))",
            "DELETE FROM biz_stock_in WHERE cost_period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')",
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN (\
                SELECT id FROM biz_voucher WHERE source_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%') \
                   OR source_no LIKE 'FIX36-%')",
            "DELETE FROM biz_voucher WHERE source_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%') \
                OR source_no LIKE 'FIX36-%'",
            "DELETE FROM biz_cost_alloc_result WHERE rule_id IN \
                (SELECT id FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB36R-%') \
                OR period_code IN (SELECT period_code FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')",
            "DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB36R-%'",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS36-%')",
            "DELETE FROM biz_cost_collect WHERE dept_name LIKE 'CB36车间%' \
                OR dept_id IN (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB36车间%')",
            "DELETE FROM biz_cost_overhead WHERE dept_name LIKE 'CB36车间%' \
                OR dept_id IN (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB36车间%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS36-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB36-%'",
            "DELETE FROM biz_lot WHERE lot_no LIKE 'CB36LOT-%'",
            "DELETE FROM biz_inventory_location WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB36-%')",
            "DELETE FROM biz_inventory WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'CB36-%')",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB36-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB36W%'",
            "DELETE FROM sys_dept WHERE dept_name LIKE 'CB36车间%'",
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
                    Value::Text(format!("CB36D{}", tail_code(&suffix))),
                    Value::Text(format!("CB36车间-{}", &suffix[..8.min(suffix.len())])),
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
                    Value::Text(format!("CB36-{suffix}")),
                    Value::Text(format!("结算编排夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB36W{}", tail_code(&suffix));
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
            format!("CS36-{}", &self.suffix[..8.min(self.suffix.len())])
        }

        fn voucher_date(&self) -> NaiveDate {
            NaiveDate::from_ymd_opt(self.year, 11, 30).expect("凭证日期")
        }

        /// 结算入参（生命周期靠 `Box::leak`，测试量级可忽略）。
        fn input(&self, period_id: Uuid, labor_source: &'static str) -> SettleInput<'static> {
            SettleInput {
                period_id,
                period_code: Box::leak(self.period_code().into_boxed_str()),
                batch_no: Box::leak(self.batch_no().into_boxed_str()),
                labor_source,
                in_date: self.voucher_date(),
                voucher_date: self.voucher_date(),
                operator: Some(self.dept),
                now: Utc::now(),
            }
        }

        /// 造一个成本对象（产出品 = 夹具物料，完工入库到夹具仓）。
        async fn object(&self) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB36-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 2000, 800, 200, 80, 'ONE_TIME', \
                         ?::uuid, 'CB36车间', ?::text::date, 'OPEN', now(), now())"
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
                         ?::text::numeric, ?::uuid, 'CB36车间', now(), now())"
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
                 VALUES (?::uuid, ?, NULL, ?::uuid, 'CB36车间', 'OTHER', ?, ?, \
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
                         ?::text::numeric, ?::uuid, 'CB36车间', now(), now())"
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
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, 'CB36车间', ?, 500, 1000, 4000, \
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
            let rule_code = format!("CB36R-{}", tail_code(&self.suffix));
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, biz_type, source_dept_id, \
                                                  source_dept_name, account_code, basis, priority, \
                                                  effective_from, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, '结算编排夹具规则', 'OVERHEAD', ?::uuid, 'CB36车间', ?, \
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
                        format!("FIX36-{}", &self.suffix[..8.min(self.suffix.len())]).into_boxed_str(),
                    )),
                    source_doc_no: None,
                    voucher_date: self.voucher_date(),
                    summary: "FIX36 领料凭证夹具".to_string(),
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

        /// 本批次结算凭证条数（未红冲）。
        async fn settled_voucher_count(&self, period_id: Uuid) -> i64 {
            db().query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_voucher \
                  WHERE source_type = 'COST_PERIOD' AND source_id = ?::uuid \
                    AND deleted_at IS NULL AND is_red = 0 AND reversed_by IS NULL"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("查结算凭证条数")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count")
        }

        /// 幂等的「零写入」判据：入库单 0 / 凭证 0 / 库存不变 / 批次状态不变。
        async fn assert_zero_writes(&self, period_id: Uuid, expected_status: &str) {
            let (qty, amount) = self.stock_in_totals(period_id).await;
            assert_eq!(qty, Decimal::ZERO, "回滚后不应有入库单数量");
            assert_eq!(amount, Decimal::ZERO, "回滚后不应有入库单金额");
            assert_eq!(self.settled_voucher_count(period_id).await, 0, "回滚后不应有结算凭证");
            let snapshot = self.period_row(period_id).await;
            assert_eq!(snapshot.status, expected_status, "回滚后批次状态必须原样不动");
            assert!(!snapshot.settled, "回滚后不应有 settled_at");
            assert!(snapshot.finish_voucher_id.is_none(), "回滚后不应有完工凭证指针");
            assert_eq!(self.inventory_amount().await, Decimal::ZERO, "回滚后库存金额必须为 0");
            assert_eq!(self.status_log_count(period_id).await, 0, "回滚后不应有批次状态日志");
            assert_eq!(self.stock_in_count(period_id).await, 0, "回滚后不应有入库单（含作废）");
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
                    (SELECT id FROM biz_voucher WHERE source_no LIKE 'FIX36-%')"
                    .to_string(),
            );
            statements.push("DELETE FROM biz_voucher WHERE source_no LIKE 'FIX36-%'".to_string());
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
                "DELETE FROM biz_cost_collect WHERE dept_id = '{}'::uuid OR dept_name LIKE 'CB36车间%'",
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

    // ==================== 用例 1：端到端 ====================

    /// **验收①**：一次事务内入库 + 四张凭证 + 状态回写，6 条断言逐条通过。
    #[tokio::test]
    async fn settle_end_to_end_in_one_transaction() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;

        let settled = fixture.settle(period_id).await.expect("结算应成功");

        // ---------- 6 条断言：全过，且顺序 / 编号与契约一致 ----------
        assert_eq!(settled.assertions.len(), 6, "卡面要求 6 条断言");
        let ids = settled.assertions.iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids, ASSERT_IDS.to_vec(), "断言顺序是契约");
        for report in &settled.assertions {
            println!("[断言] {} {} —— {}", report.id, report.name, report.detail);
        }

        // ---------- 单头回写 ----------
        let snapshot = fixture.period_row(period_id).await;
        assert_eq!(snapshot.status, "SETTLED");
        assert!(snapshot.settled, "settled_at 必须有值");
        assert_eq!(snapshot.object_count, 1);
        assert_eq!(snapshot.total_material, "19000.0000");
        assert_eq!(snapshot.total_labor, "4000.0000");
        assert_eq!(snapshot.total_overhead, "4000.0000");
        assert_eq!(snapshot.total_cost, "27000.0000");
        assert_eq!(snapshot.total_finished, "22311.1104");
        assert_eq!(snapshot.total_wip, "4688.8896");
        assert_eq!(snapshot.finish_voucher_id, Some(settled.vouchers.finish));
        assert!(snapshot.labor_voucher_id.is_some(), "人工计提凭证指针");
        assert!(snapshot.overhead_voucher_id.is_some(), "制造费用结转凭证指针");

        // ---------- 入库 + 三本账 ----------
        let (qty, amount) = fixture.stock_in_totals(period_id).await;
        assert_eq!(qty, dec("800"));
        assert_eq!(amount, dec("22311.1104"));
        assert_eq!(fixture.inventory_qty().await, dec("800"), "库存数量 +800");
        assert_eq!(
            fixture.inventory_amount().await,
            dec("22311.1104"),
            "库存金额 +22311.1104"
        );

        // ---------- 凭证：4 张（人工 / 台账 / 结转 / 完工） ----------
        assert_eq!(fixture.settled_voucher_count(period_id).await, 4);
        assert!(settled.vouchers.ledger.is_some(), "手工台账凭证应在");

        // ---------- 设计 §4.7.5：4001 三末级余额 + 4101 清零 ----------
        assert_eq!(fixture.period_balance("4001.01").await, dec("3800.0000"));
        assert_eq!(fixture.period_balance("4001.02").await, dec("444.4448"));
        assert_eq!(fixture.period_balance("4001.03").await, dec("444.4448"));
        assert_eq!(fixture.period_balance("4101.01").await, Decimal::ZERO, "4101.01 清零");
        assert_eq!(fixture.period_balance("4101.02").await, Decimal::ZERO, "4101.02 清零");
        let wip_total = fixture.period_balance("4001.01").await
            + fixture.period_balance("4001.02").await
            + fixture.period_balance("4001.03").await;
        assert_eq!(wip_total, dec("4688.8896"), "4001 合计 = Σ wip_end_cost");

        // ---------- 状态日志：APPROVED → SETTLED ----------
        let log = db()
            .query_first_sql(
                "SELECT from_status, to_status FROM biz_status_log WHERE entity_id = ?::uuid \
                  ORDER BY created_at DESC, id DESC LIMIT 1"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("读状态日志")
            .expect("状态日志存在");
        assert_eq!(log.get_typed::<String>("from_status").expect("from"), "APPROVED");
        assert_eq!(log.get_typed::<String>("to_status").expect("to"), "SETTLED");

        fixture.cleanup().await;
    }

    // ==================== 用例 2：第 6 条断言失败 ⇒ 全回滚（核心验收） ====================

    /// **验收②③**：写完 ②–⑤ 后删掉一行分摊结果（真实的「漏摊」缺口）⇒ 只有第 6 条红，
    /// 409 + `66077`；事务回滚后入库单 / 凭证 / 库存 / 状态回写**全部消失**，被删的分摊结果
    /// 也随回滚回来（说明删改本身也在同一个事务里）。
    #[tokio::test]
    async fn sixth_assertion_rolls_back_everything() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, object_id) = standard_case(&fixture, true, "MANUAL").await;
        let input = fixture.input(period_id, "MANUAL");

        let tx = db().begin().await.expect("开启结算事务");
        // ②–⑤ 真实执行（与生产同一个函数）
        let written = run::apply_in_tx(&tx, &input).await.expect("写入阶段应成功");
        // 制造缺口：少一行分摊结果（4301.02 的 1000 没了 ⇒ Σ 分摊 3000 ≠ 池 4000）
        let deleted = tx
            .exec_sql(
                "DELETE FROM biz_cost_alloc_result \
                  WHERE period_code = ? AND cost_object_id = ?::uuid AND account_code = '4101.02'"
                    .to_string(),
                vec![Value::Text(fixture.period_code()), Value::from(&object_id)],
            )
            .await
            .expect("删分摊结果");
        assert_eq!(deleted, 1, "夹具应删掉恰好一行分摊结果");

        // 事务内先确认前 5 条断言都对平（只让第 6 条红）
        let items = cv::batch_items(&tx, period_id).await.expect("读计算单");
        let error = run::run_asserts(&tx, &input, &written.vouchers, &items)
            .await
            .expect_err("第 6 条断言必须红");
        assert_error_code(&error, error_code::ERP_COST_SETTLE_ASSERT);
        assert!(
            error.msg.contains(ASSERT_IDS[5]),
            "报文必须指名是第 6 条：{}",
            error.msg
        );
        assert!(error.msg.contains("整个结算已回滚"), "报文：{}", error.msg);
        println!("[第 6 条断言] {}", error.msg);

        // 回滚：与生产同款语义（settle_period 的失败路径也是 rollback）
        tx.rollback().await.expect("回滚");

        // 回滚后：入库 / 凭证 / 库存 / 状态回写全部消失
        fixture.assert_zero_writes(period_id, "APPROVED").await;
        let items_left = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_cost_period_item \
                  WHERE period_id = ?::uuid AND stock_in_id IS NOT NULL"
                    .to_string(),
                vec![Value::from(&period_id)],
            )
            .await
            .expect("查计算单绑定")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count");
        assert_eq!(items_left, 0, "回滚后计算单不应绑定入库单");
        // 被删的分摊结果随事务回滚回来（证明「删」也在同一事务里）
        let alloc_left = db()
            .query_first_sql(
                "SELECT count(*)::bigint AS n FROM biz_cost_alloc_result WHERE period_code = ?".to_string(),
                vec![Value::Text(fixture.period_code())],
            )
            .await
            .expect("查分摊结果")
            .expect("count 行")
            .get_typed::<i64>("n")
            .expect("count");
        assert_eq!(alloc_left, 2, "回滚后分摊结果应恢复成 2 行");

        fixture.cleanup().await;
    }

    // ==================== 用例 3：状态闸 ====================

    /// `DRAFT` 批次不能结算（设计 §4.1：只有 `APPROVED` 能结）。
    #[tokio::test]
    async fn settle_requires_approved_status() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("DRAFT", "MANUAL").await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;

        let error = fixture.settle(period_id).await.expect_err("DRAFT 不能结算");
        assert_error_code(&error, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert!(error.msg.contains("结算"), "报文：{}", error.msg);
        fixture.assert_zero_writes(period_id, "DRAFT").await;

        fixture.cleanup().await;
    }

    // ==================== 用例 4：断言 4（4001 余额）正面证据 ====================

    /// 没有领料凭证 ⇒ `4001.01` 借方少 19000 ⇒ 断言 4（`C-COST-3`）红，整笔回滚。
    ///
    /// 这条同时是「B3-8 的料钱必须进 4001.01」的执行时证据：料钱进了 6401 也过不了结算。
    #[tokio::test]
    async fn missing_material_debit_is_caught_by_the_wip_assertion() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, false, "MANUAL").await;

        let error = fixture.settle(period_id).await.expect_err("缺料钱必须拦住");
        assert_error_code(&error, error_code::ERP_COST_SETTLE_ASSERT);
        assert!(
            error.msg.contains(ASSERT_IDS[3]),
            "报文必须指名断言 4（{}）：{}",
            ASSERT_IDS[3],
            error.msg
        );
        println!("[断言 4] {}", error.msg);
        fixture.assert_zero_writes(period_id, "APPROVED").await;

        fixture.cleanup().await;
    }

    // ==================== 用例 5：断言 1（池 ↔ 4101）正面证据 ====================

    /// 有人绕过成本模块直接记了一笔 4101 ⇒ 断言 1 红（码 `66065`），整笔回滚。
    #[tokio::test]
    async fn bypassed_4101_entry_is_caught_before_settling() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;

        // 旁路记账：借 4101.09 100 / 贷 2241.01 100（不在池里）
        let debit = fixture.account("4101.09").await;
        let credit = fixture.account("2241.01").await;
        voucher::post_voucher(
            db(),
            &PostVoucher {
                voucher_type: "MANUAL",
                source_type: "MANUAL",
                source_id: None,
                source_no: Some(Box::leak(
                    format!("FIX36-B-{}", &fixture.suffix[..8.min(fixture.suffix.len())]).into_boxed_str(),
                )),
                source_doc_no: None,
                voucher_date: fixture.voucher_date(),
                summary: "FIX36 旁路记账夹具".to_string(),
                lines: vec![
                    EntryLine {
                        line_no: 1,
                        summary: "旁路制造费用".to_string(),
                        account: debit,
                        debit: dec("100"),
                        credit: Decimal::ZERO,
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                        aux_dept_id: Some(fixture.dept),
                    },
                    EntryLine {
                        line_no: 2,
                        summary: "旁路制造费用".to_string(),
                        account: credit,
                        debit: Decimal::ZERO,
                        credit: dec("100"),
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                        aux_dept_id: Some(fixture.dept),
                    },
                ],
                remark: None,
                red_of: None,
                status: STATUS_POSTED,
            },
            Some(fixture.dept),
            Utc::now(),
        )
        .await
        .expect("旁路凭证夹具");

        let error = fixture.settle(period_id).await.expect_err("旁路 4101 必须拦住");
        assert_error_code(&error, error_code::ERP_COST_POOL_LEDGER_MISMATCH);
        println!("[断言 1] {}", error.msg);
        fixture.assert_zero_writes(period_id, "APPROVED").await;

        // 旁路凭证是夹具自己发的（不在事务里），单独清
        db().exec_sql(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                (SELECT id FROM biz_voucher WHERE source_no LIKE 'FIX36-B-%')"
                .to_string(),
            vec![],
        )
        .await
        .expect("清旁路凭证分录");
        db().exec_sql(
            "DELETE FROM biz_voucher WHERE source_no LIKE 'FIX36-B-%'".to_string(),
            vec![],
        )
        .await
        .expect("清旁路凭证");

        fixture.cleanup().await;
    }

    // ==================== 用例 6：幂等 ====================

    /// 结算成功后同批次再结算 ⇒ 409 + `66073`，凭证与入库单条数不变。
    #[tokio::test]
    async fn second_settle_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "MANUAL").await;
        fixture.settle(period_id).await.expect("第一次结算成功");
        let vouchers_before = fixture.settled_voucher_count(period_id).await;
        let stock_ins_before = fixture.stock_in_count(period_id).await;

        let error = fixture.settle(period_id).await.expect_err("第二次必须被拦");
        assert_error_code(&error, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert_eq!(
            fixture.settled_voucher_count(period_id).await,
            vouchers_before,
            "凭证条数不应变化"
        );
        assert_eq!(
            fixture.stock_in_count(period_id).await,
            stock_ins_before,
            "入库单条数不应变化"
        );

        fixture.cleanup().await;
    }

    // ==================== 用例 7：PAYROLL 来源 ====================

    /// `labor_source='PAYROLL'` ⇒ 人工计提不出（D-13：工资凭证由薪资单出），其余 3 张照发，
    /// 6 条断言照样全过（4001.02 借方由薪资侧提供 —— 本夹具用一张同额凭证模拟）。
    #[tokio::test]
    async fn payroll_source_settles_with_three_vouchers() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (period_id, _) = standard_case(&fixture, true, "PAYROLL").await;
        // 薪资侧凭证：借 4001.02 4000 / 贷 2211.01 4000（PAYROLL 时结算不出人工计提）
        let debit = fixture.account("4001.02").await;
        let credit = fixture.account("2211.01").await;
        voucher::post_voucher(
            db(),
            &PostVoucher {
                voucher_type: "AUTO",
                source_type: "PAYROLL",
                source_id: None,
                source_no: Some(Box::leak(
                    format!("FIX36-P-{}", &fixture.suffix[..8.min(fixture.suffix.len())]).into_boxed_str(),
                )),
                source_doc_no: None,
                voucher_date: fixture.voucher_date(),
                summary: "FIX36 工资计提夹具".to_string(),
                lines: vec![
                    EntryLine {
                        line_no: 1,
                        summary: "工资计提".to_string(),
                        account: debit,
                        debit: dec("4000"),
                        credit: Decimal::ZERO,
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                        aux_dept_id: Some(fixture.dept),
                    },
                    EntryLine {
                        line_no: 2,
                        summary: "工资计提".to_string(),
                        account: credit,
                        debit: Decimal::ZERO,
                        credit: dec("4000"),
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                        aux_dept_id: Some(fixture.dept),
                    },
                ],
                remark: None,
                red_of: None,
                status: STATUS_POSTED,
            },
            Some(fixture.dept),
            Utc::now(),
        )
        .await
        .expect("工资凭证夹具");

        let settled = fixture.settle(period_id).await.expect("PAYROLL 应能结算");
        assert!(settled.vouchers.labor.is_none(), "PAYROLL 不出人工计提凭证");
        assert_eq!(fixture.settled_voucher_count(period_id).await, 3, "台账 + 结转 + 完工");
        assert_eq!(settled.assertions.len(), 6);
        let ids = settled.assertions.iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids, ASSERT_IDS.to_vec(), "断言编号与顺序必须在契约内");
        assert_eq!(fixture.period_balance("4001.02").await, dec("444.4448"), "人工在制余额");

        db().exec_sql(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                (SELECT id FROM biz_voucher WHERE source_no LIKE 'FIX36-P-%')"
                .to_string(),
            vec![],
        )
        .await
        .expect("清工资凭证分录");
        db().exec_sql(
            "DELETE FROM biz_voucher WHERE source_no LIKE 'FIX36-P-%'".to_string(),
            vec![],
        )
        .await
        .expect("清工资凭证");
        fixture.cleanup().await;
    }

    // ==================== 用例 8：期间已结账 / 无计算单 ====================

    /// 没有计算单的批次不能结算（409，零写入）—— 防止「点了结算但什么都没算」。
    #[tokio::test]
    async fn empty_batch_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("APPROVED", "MANUAL").await;

        let error = fixture.settle(period_id).await.expect_err("空批次不能结算");
        assert_eq!(error.status, 409, "报文：{}", error.msg);
        assert!(error.msg.contains("没有"), "报文：{}", error.msg);
        fixture.assert_zero_writes(period_id, "APPROVED").await;

        fixture.cleanup().await;
    }

    /// 完工 0 件的「全在制」批次：不生成入库单，完工金额 0 ⇒ 409 + `66075`，零写入。
    #[tokio::test]
    async fn zero_output_batch_is_rejected_by_the_finish_voucher_gate() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("APPROVED", "MANUAL").await;
        let (object_id, _) = fixture.object().await;
        // 全在制：完工 0 / 完工成本 0 / 在制 = 27000
        fixture
            .item_with(period_id, object_id, "0", "19000", "4000", "4000", "0")
            .await;
        fixture.labor_collect(object_id, "4000").await;
        fixture.ledger_row("4101.01", "2241.01", "3000").await;
        fixture.ledger_row("4101.02", "2241.01", "1000").await;
        let rule = fixture.alloc_rule("4101.01").await;
        fixture.alloc_result(rule, object_id, "4101.01", "3000").await;
        fixture.alloc_result(rule, object_id, "4101.02", "1000").await;
        fixture.material_voucher("19000").await;

        let error = fixture.settle(period_id).await.expect_err("完工 0 不能结算");
        assert_error_code(&error, error_code::ERP_COST_FINISH_VOUCHER_EMPTY);
        fixture.assert_zero_writes(period_id, "APPROVED").await;

        fixture.cleanup().await;
    }

    // ---------- 常量对齐（顺带钉住 B3-4/B3-5 的口径，避免上游漂移） ----------

    #[test]
    fn fixture_constants_are_aligned() {
        assert_eq!(ASSERT_NAMES.len(), ASSERT_IDS.len());
        assert_eq!(cost_settle_finish::IN_TYPE_PRODUCTION, "PRODUCTION");
        assert_eq!(cv::ACCOUNT_FINISHED_GOODS, "1405");
    }
}
