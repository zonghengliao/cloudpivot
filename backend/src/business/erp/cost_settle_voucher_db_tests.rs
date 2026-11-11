//! 结算 ②「三张凭证」的真库用例（`ERP_DB_URL` 开关）—— COST-B3-5。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_settle_voucher_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面四行逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`three_vouchers_match_the_design_example`] | **验收①②③一起**：人工计提 / 制造费用结转 / 完工结转三张逐行科目与金额 = 设计 §4.7.5 表；借贷平衡；4001 三末级余额 = 3800 / 444.4448 / 444.4448、合计 = 计算单 `wip_end_cost`；4101 清零；`biz_stock_in.voucher_id` 回写；四张凭证同 `source_type='COST_PERIOD'` + 同 `source_id` |
//! | 2 | [`payroll_source_emits_only_the_finish_voucher`] | `labor_source='PAYROLL'` ⇒ 人工计提不生成（§4.7.2 末段）；无台账 ⇒ 台账凭证不生成；无费用 ⇒ 结转不生成：只剩 1 张完工凭证 |
//! | 3 | [`zero_finished_is_rejected_without_writes`] | 完工成本 0 ⇒ 409 + `66075`，一张凭证都不落（`ck_biz_cost_period_settle` 要求结算必须有完工凭证） |
//! | 4 | [`item_mismatch_is_rejected_without_writes`] | 归集人工 3900 vs 计算单 4000 ⇒ 409 + `66076`，零写入（账账不符不出凭证） |
//! | 5 | [`second_settle_is_rejected`] | 幂等闸：同批次第二次 409 + `66074`，凭证条数不变 |
//! | 6 | [`direct_overhead_row_is_cleared_too`] | 偏差 1 的正面证据：池行 + **挂成本对象的直接行**都在结转凭证里，4101 两个科目都清零、Σ借 = `item.oh_cur` |
//!
//! # 夹具纪律
//!
//! 前缀 `CB35-*`（成本对象）/ `CS35-*`（批次号）/ `FIX35-*`（夹具领料凭证单号）；期间年份按进程内
//! 计数器从 **2600** 起（与 B2-11 的 2200+、B3-1 的 2031、B3-4 的 2400+ 都错开，并行跑不撞
//! `uk_biz_cost_period`）。清场顺序严格按外键：凭证分录 → 凭证 → 入库单 → 计算单 → 归集行 →
//! 台账 → 批次 → 成本对象。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_settle_voucher::{self as cv, SettledVouchers, VoucherContext};
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

    /// 期间年份：2600 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2600 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 400).unwrap_or(0)
    }

    fn dec(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn money(value: Decimal) -> String {
        format!("{:.*}", 4, value)
    }

    /// 一行凭证分录的快照（断言用）。
    #[derive(Debug, Clone)]
    struct EntryRow {
        account_code: String,
        debit: Decimal,
        credit: Decimal,
        aux_dept_id: Option<Uuid>,
        aux_material_id: Option<Uuid>,
        aux_warehouse_id: Option<Uuid>,
    }

    /// 一张凭证的单头（断言用）。
    #[derive(Debug, Clone)]
    struct VoucherRow {
        id: Uuid,
        summary: String,
        status: String,
        source_type: String,
        source_id: Option<Uuid>,
        total_debit: Decimal,
        total_credit: Decimal,
        entries: Vec<EntryRow>,
    }

    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
        vouchers: std::sync::Mutex<Vec<Uuid>>,
        stock_ins: std::sync::Mutex<Vec<Uuid>>,
        ledgers: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（前缀限定，绝不动别人的数据）。
    ///
    /// 顺序按外键：凭证分录 → 凭证 → 入库单 → 归集行（按夹具车间）→ 台账 → 计算单 → 批次 →
    /// 成本对象 → 夹具车间。`biz_cost_overhead` / `biz_cost_collect` 对成本对象是 **RESTRICT**，
    /// 所以必须清在对象之前；池行的 `cost_object_id` 为空，只能按**夹具车间**删（这也是本夹具
    /// 自建 `sys_dept` 的原因：共享车间没法当清理标记）。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN (\
                SELECT id FROM biz_voucher WHERE source_id IN \
                    (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS35-%') \
                   OR source_no LIKE 'FIX35-%')",
            "DELETE FROM biz_voucher WHERE source_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS35-%') \
                OR source_no LIKE 'FIX35-%'",
            "DELETE FROM biz_stock_in WHERE cost_period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS35-%')",
            "DELETE FROM biz_cost_collect WHERE dept_id IN \
                (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB35车间%') \
                OR dept_name LIKE 'CB35车间%' \
                OR source_id IN (SELECT id FROM biz_cost_overhead WHERE dept_name LIKE 'CB35车间%')",
            "DELETE FROM biz_cost_overhead WHERE dept_id IN \
                (SELECT id FROM sys_dept WHERE dept_name LIKE 'CB35车间%') \
                OR dept_name LIKE 'CB35车间%'",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS35-%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS35-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB35-%'",
            "DELETE FROM sys_dept WHERE dept_name LIKE 'CB35车间%'",
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
            // 自建车间：池行的 `cost_object_id` 为空，只有「夹具车间」能当清理标记
            let dept = Uuid::now_v7();
            let dept_code = format!("CB35D{}", &suffix[..8.min(suffix.len())]);
            db().exec_sql(
                "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 1, 0, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&dept),
                    Value::Text(dept_code),
                    Value::Text(format!("CB35车间-{}", &suffix[..8.min(suffix.len())])),
                ],
            )
            .await
            .expect("造夹具车间");
            Self {
                suffix,
                year,
                dept,
                product: Uuid::now_v7(),
                warehouse: Uuid::now_v7(),
                objects: std::sync::Mutex::new(Vec::new()),
                periods: std::sync::Mutex::new(Vec::new()),
                vouchers: std::sync::Mutex::new(Vec::new()),
                stock_ins: std::sync::Mutex::new(Vec::new()),
                ledgers: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        fn batch_no(&self) -> String {
            format!("CS35-{}", &self.suffix[..8.min(self.suffix.len())])
        }

        fn voucher_date(&self) -> NaiveDate {
            NaiveDate::from_ymd_opt(self.year, 11, 30).expect("凭证日期")
        }

        fn context(&self, period_id: Uuid, labor_source: &'static str) -> VoucherContext<'static> {
            // 生命周期：把期间码 / 批次号泄漏成 'static 只为测试好写，量级可忽略
            VoucherContext {
                period_id,
                period_code: Box::leak(self.period_code().into_boxed_str()),
                batch_no: Box::leak(self.batch_no().into_boxed_str()),
                voucher_date: self.voucher_date(),
                labor_source,
                operator: Some(self.dept),
                now: Utc::now(),
            }
        }

        /// 造一个成本对象（本卡不需要库位 / 批号）。
        async fn object(&self) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB35-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 2000, 800, 200, 80, 'ONE_TIME', \
                         ?::uuid, 'CB35车间', ?::text::date, 'OPEN', now(), now())"
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
        async fn period(&self, labor_source: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, labor_source, \
                                              approval_instance_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'APPROVED', ?, ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(self.batch_no()),
                    Value::Text(labor_source.to_string()),
                    Value::from(Uuid::now_v7()),
                ],
            )
            .await
            .expect("造结算批次夹具");
            self.periods.lock().expect("批次清单").push(id);
            id
        }

        /// 造计算单行（默认 = 设计 §4.6.2 算例 A）。
        async fn item(&self, period_id: Uuid, object_id: Uuid) -> Uuid {
            self.item_with(
                period_id,
                object_id,
                "800",
                "19000",
                "4000",
                "4000",
                ("19.000000", "4.444444", "4.444444"),
                "22311.1104",
            )
            .await
        }

        /// 指定投入 / 单位成本 / 完工金额的计算单（`wip_end` = 投入 − 完工，守恒式由调用方保证）。
        #[allow(clippy::too_many_arguments)]
        async fn item_with(
            &self,
            period_id: Uuid,
            object_id: Uuid,
            completed: &str,
            mat: &str,
            labor: &str,
            oh: &str,
            units: (&str, &str, &str),
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
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, \
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
                    Value::Text(units.0.to_string()),
                    Value::Text(units.1.to_string()),
                    Value::Text(units.2.to_string()),
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
                         ?::text::numeric, ?::uuid, 'CB35车间', now(), now())"
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

        /// 造一条手工费用台账 + 它的归集行（`cost_object_id` 为空 ⇒ 池行；非空 ⇒ 直接行）。
        async fn ledger_row(
            &self,
            debit_account: &str,
            credit_account: &str,
            amount: &str,
            cost_object: Option<Uuid>,
        ) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULLIF(?::text,'')::uuid, ?::uuid, 'CB35车间', 'OTHER', ?, ?, \
                         ?::text::numeric, 'MANUAL', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period_code()),
                    Value::Text(cost_object.map(|v| v.to_string()).unwrap_or_default()),
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
                 VALUES (?::uuid, NULLIF(?::text,'')::uuid, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, 0, \
                         ?::text::date, ?::text::numeric, ?::uuid, 'CB35车间', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(cost_object.map(|v| v.to_string()).unwrap_or_default()),
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

        /// 造一张完工入库单（B3-4 的产物；本卡只验 `voucher_id` 回写）。
        async fn stock_in(&self, period_id: Uuid, object_id: Uuid, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, \
                                           total_qty, total_amount, total_tax, total_with_tax, \
                                           invoice_received, cost_object_id, cost_period_id, \
                                           created_at, updated_at) \
                 VALUES (?::uuid, ?, 'PRODUCTION', ?::uuid, ?::text::date, 'POSTED', \
                         800, ?::text::numeric, 0, ?::text::numeric, FALSE, ?::uuid, ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("IN35-{}", &self.suffix[..8.min(self.suffix.len())])),
                    Value::from(&self.warehouse),
                    Value::Text(self.voucher_date().to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                    Value::from(&object_id),
                    Value::from(&period_id),
                ],
            )
            .await
            .expect("造完工入库单夹具");
            self.stock_ins.lock().expect("入库单清单").push(id);
            id
        }

        /// 夹具前置账：领料凭证「借 4001.01 / 贷 1405」（B3-8 的口径，§4.7.5 第 0 张）。
        async fn material_voucher(&self, amount: &str) -> Uuid {
            let debit = self.account("4001.01").await;
            let credit = self.account("1405").await;
            let id = voucher::post_voucher(
                db(),
                &PostVoucher {
                    voucher_type: "AUTO",
                    source_type: "STOCK_OUT",
                    source_id: None,
                    source_no: Some(Box::leak(
                        format!("FIX35-{}", &self.suffix[..8.min(self.suffix.len())]).into_boxed_str(),
                    )),
                    source_doc_no: None,
                    voucher_date: self.voucher_date(),
                    summary: "FIX35 领料凭证夹具".to_string(),
                    lines: vec![
                        EntryLine {
                            line_no: 1,
                            summary: "生产领料".to_string(),
                            account: debit,
                            debit: dec(amount),
                            credit: Decimal::ZERO,
                            aux_supplier_id: None,
                            aux_material_id: None,
                            aux_warehouse_id: None,
                            aux_dept_id: None,
                        },
                        EntryLine {
                            line_no: 2,
                            summary: "生产领料".to_string(),
                            account: credit,
                            debit: Decimal::ZERO,
                            credit: dec(amount),
                            aux_supplier_id: None,
                            aux_material_id: None,
                            aux_warehouse_id: None,
                            aux_dept_id: None,
                        },
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
            self.vouchers.lock().expect("凭证清单").push(id);
            id
        }

        async fn account(&self, code: &str) -> AccountRef {
            voucher::account_by_code(db(), code)
                .await
                .expect("取科目")
                .unwrap_or_else(|| panic!("科目 {code} 不存在"))
        }

        /// 跑一次「三张凭证」（**与生产同款事务语义**：成功才提交，失败整笔回滚）。
        async fn run(
            &self,
            period_id: Uuid,
            labor_source: &'static str,
        ) -> std::result::Result<SettledVouchers, ApiError> {
            let ctx = self.context(period_id, labor_source);
            let tx = db().begin().await.expect("开启结算事务");
            match cv::post_all(&tx, &ctx).await {
                Ok(rows) => {
                    tx.commit().await.expect("提交结算事务");
                    let mut list = self.vouchers.lock().expect("凭证清单");
                    for id in [rows.labor, rows.ledger, rows.overhead, Some(rows.finish)]
                        .into_iter()
                        .flatten()
                    {
                        list.push(id);
                    }
                    Ok(rows)
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    Err(error)
                }
            }
        }

        /// 读一张凭证（单头 + 分录，分录按行号）。
        async fn voucher(&self, id: Uuid) -> VoucherRow {
            let header = db()
                .query_first_sql(
                    "SELECT id, summary, status, source_type, source_id, total_debit, total_credit \
                       FROM biz_voucher WHERE id = ?::uuid"
                        .to_string(),
                    vec![Value::from(&id)],
                )
                .await
                .expect("读凭证单头")
                .expect("凭证存在");
            let entries = db()
                .query_sql(
                    "SELECT e.account_code, e.debit_amount, e.credit_amount, e.aux_dept_id, \
                            e.aux_material_id, e.aux_warehouse_id \
                       FROM biz_voucher_entry e WHERE e.voucher_id = ?::uuid ORDER BY e.line_no ASC"
                        .to_string(),
                    vec![Value::from(&id)],
                )
                .await
                .expect("读凭证分录");
            VoucherRow {
                id,
                summary: header.get_typed("summary").expect("summary"),
                status: header.get_typed("status").expect("status"),
                source_type: header.get_typed("source_type").expect("source_type"),
                source_id: header.get_typed("source_id").expect("source_id"),
                total_debit: header.get_typed("total_debit").expect("total_debit"),
                total_credit: header.get_typed("total_credit").expect("total_credit"),
                entries: entries
                    .iter()
                    .map(|row| EntryRow {
                        account_code: row.get_typed("account_code").expect("account_code"),
                        debit: row.get_typed("debit_amount").expect("debit"),
                        credit: row.get_typed("credit_amount").expect("credit"),
                        aux_dept_id: row.get_typed("aux_dept_id").expect("aux_dept_id"),
                        aux_material_id: row.get_typed("aux_material_id").expect("aux_material_id"),
                        aux_warehouse_id: row.get_typed("aux_warehouse_id").expect("aux_warehouse_id"),
                    })
                    .collect(),
            }
        }

        /// 一组凭证在某个科目上的净额（借−贷），可指定辅助核算部门。
        async fn balance(&self, vouchers: &[Uuid], code: &str) -> Decimal {
            let list = vouchers
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            let row = db()
                .query_first_sql(
                    format!(
                        "SELECT COALESCE(sum(debit_amount - credit_amount), 0)::text AS net \
                           FROM biz_voucher_entry WHERE voucher_id IN ({list}) AND account_code = ?"
                    ),
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("读科目净额")
                .expect("净额行");
            row.get_typed::<String>("net").expect("净额").parse().expect("净额金额")
        }

        /// 该批次已发的（未红冲）结算凭证条数。
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

        async fn cleanup(&self) {
            let joined = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let vouchers = joined(&self.vouchers);
            let periods = joined(&self.periods);
            let objects = joined(&self.objects);
            let ledgers = joined(&self.ledgers);
            let stocks = joined(&self.stock_ins);
            let mut statements = Vec::new();
            if !vouchers.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_voucher_entry WHERE voucher_id IN ({vouchers})"
                ));
                statements.push(format!("DELETE FROM biz_voucher WHERE id IN ({vouchers})"));
            }
            if !periods.is_empty() {
                // 失败路径上凭证可能没登记进 vouchers（例如 post_all 中途报错），按批次再扫一遍
                statements.push(format!(
                    "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                        (SELECT id FROM biz_voucher WHERE source_id IN ({periods}) \
                            OR source_no LIKE 'FIX35-%')"
                ));
                statements.push(format!(
                    "DELETE FROM biz_voucher WHERE source_id IN ({periods}) OR source_no LIKE 'FIX35-%'"
                ));
            }
            if !stocks.is_empty() {
                statements.push(format!("DELETE FROM biz_stock_in WHERE id IN ({stocks})"));
            }
            if !periods.is_empty() {
                statements.push(format!("DELETE FROM biz_stock_in WHERE cost_period_id IN ({periods})"));
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
            }
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects})"
                ));
            }
            // 剩余（池行 / 直接行）按夹具车间删：`dept_id` 命中夹具车间，或快照名带夹具前缀
            statements.push(format!(
                "DELETE FROM biz_cost_collect WHERE dept_id = '{}'::uuid OR dept_name LIKE 'CB35车间%'",
                self.dept
            ));
            if !ledgers.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_overhead WHERE id IN ({ledgers})"));
            }
            statements.push(format!(
                "DELETE FROM biz_cost_overhead WHERE dept_id = '{}'::uuid",
                self.dept
            ));
            if !periods.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            if !objects.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            // 车间最后删（归集行 / 台账都按它清过）
            statements.push(format!("DELETE FROM sys_dept WHERE id = '{}'::uuid", self.dept));
            for sql in statements {
                if let Err(error) = db().exec_sql(sql.clone(), vec![]).await {
                    println!("cleanup 失败（{sql}）：{error}");
                }
            }
        }
    }

    /// 找某张凭证里某个科目的金额（多行则相加）。
    fn amount_of(voucher: &VoucherRow, code: &str, debit_side: bool) -> Decimal {
        voucher
            .entries
            .iter()
            .filter(|entry| entry.account_code == code)
            .map(|entry| if debit_side { entry.debit } else { entry.credit })
            .sum()
    }

    fn assert_balanced(voucher: &VoucherRow) {
        let debit: Decimal = voucher.entries.iter().map(|entry| entry.debit).sum();
        let credit: Decimal = voucher.entries.iter().map(|entry| entry.credit).sum();
        assert_eq!(debit, credit, "凭证「{}」借贷不平", voucher.summary);
        assert_eq!(voucher.total_debit, debit, "单头借方 ≠ 分录合计");
        assert_eq!(voucher.total_credit, credit, "单头贷方 ≠ 分录合计");
        assert_eq!(voucher.status, "POSTED", "自动凭证必须落 POSTED");
        assert_eq!(voucher.source_type, "COST_PERIOD");
    }

    // ==================== 用例 1：三张凭证 = 设计 §4.7.5 ====================

    #[tokio::test(flavor = "multi_thread")]
    async fn three_vouchers_match_the_design_example() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("MANUAL").await;
        let (object_id, _) = fixture.object().await;
        let item_id = fixture.item(period_id, object_id).await;
        // 工：算例 A 的人工 4000（设计 §4.7.5 第 1 张）
        fixture.labor_collect(object_id, "4000").await;
        // 费：手工台账两行（折旧 3000 → 4101.01 / 水电 1000 → 4101.02），合计 = item.oh_cur = 4000
        fixture.ledger_row("4101.01", "2241.01", "3000", None).await;
        fixture.ledger_row("4101.02", "2241.01", "1000", None).await;
        // 前置账：领料凭证（§4.7.5 第 0 张，由 B3-8 落地）
        let material = fixture.material_voucher("19000").await;
        let stock_in = fixture.stock_in(period_id, object_id, "22311.1104").await;

        let settled = fixture.run(period_id, "MANUAL").await.expect("三张凭证应成功");

        // ---------- ① 人工计提：借 4001.02 4000（车间）/ 贷 2211.01 4000 ----------
        let labor_id = settled.labor.expect("应生成人工计提凭证");
        let labor = fixture.voucher(labor_id).await;
        assert_balanced(&labor);
        assert_eq!(labor.entries.len(), 2);
        assert_eq!(amount_of(&labor, "4001.02", true), dec("4000"));
        assert_eq!(amount_of(&labor, "2211.01", false), dec("4000"));
        let dept_line = labor
            .entries
            .iter()
            .find(|entry| entry.account_code == "4001.02")
            .expect("借方行");
        assert_eq!(dept_line.aux_dept_id, Some(fixture.dept), "借方 aux_dept_id = 车间");
        assert!(labor.summary.contains("人工计提"), "摘要：{}", labor.summary);

        // ---------- ② 手工台账：借 4101 / 贷 对方科目 ----------
        let ledger_id = settled.ledger.expect("有台账行 ⇒ 应有台账凭证");
        let ledger = fixture.voucher(ledger_id).await;
        assert_balanced(&ledger);
        assert_eq!(amount_of(&ledger, "4101.01", true), dec("3000"));
        assert_eq!(amount_of(&ledger, "4101.02", true), dec("1000"));
        assert_eq!(amount_of(&ledger, "2241.01", false), dec("4000"));

        // ---------- ③ 制造费用结转：借 4001.03（按车间科目）/ 贷 4101 清零 ----------
        let overhead_id = settled.overhead.expect("应生成制造费用结转凭证");
        let overhead = fixture.voucher(overhead_id).await;
        assert_balanced(&overhead);
        assert_eq!(amount_of(&overhead, "4001.03", true), dec("4000"), "= Σ item.oh_cur");
        assert_eq!(amount_of(&overhead, "4101.01", false), dec("3000"));
        assert_eq!(amount_of(&overhead, "4101.02", false), dec("1000"));
        assert!(
            overhead
                .entries
                .iter()
                .filter(|entry| entry.account_code == "4001.03")
                .all(|entry| entry.aux_dept_id == Some(fixture.dept)),
            "4001.03 借方逐行挂车间"
        );

        // ---------- ④ 完工结转：借 1405 / 贷 4001.01·.02·.03 ----------
        let finish = fixture.voucher(settled.finish).await;
        assert_balanced(&finish);
        assert_eq!(amount_of(&finish, "1405", true), dec("22311.1104"), "= Σ finished_cost");
        assert_eq!(amount_of(&finish, "4001.01", false), dec("15200.0000"));
        assert_eq!(amount_of(&finish, "4001.02", false), dec("3555.5552"));
        assert_eq!(amount_of(&finish, "4001.03", false), dec("3555.5552"));
        let goods_line = finish
            .entries
            .iter()
            .find(|entry| entry.account_code == "1405")
            .expect("1405 行");
        assert_eq!(goods_line.aux_material_id, Some(fixture.product), "aux_material_id");
        assert_eq!(goods_line.aux_warehouse_id, Some(fixture.warehouse), "aux_warehouse_id");
        assert_eq!(settled.finish_total, dec("22311.1104"));
        assert_eq!(
            settled.finished_by_element,
            (dec("15200.0000"), dec("3555.5552"), dec("3555.5552"))
        );

        // ---------- 入库单挂凭证（§4.7.4 末段） ----------
        let voucher_of_stock_in: Option<Uuid> = db()
            .query_first_sql(
                "SELECT voucher_id FROM biz_stock_in WHERE id = ?::uuid".to_string(),
                vec![Value::from(&stock_in)],
            )
            .await
            .expect("读入库单凭证号")
            .expect("入库单存在")
            .get_typed("voucher_id")
            .expect("voucher_id");
        assert_eq!(voucher_of_stock_in, Some(settled.finish), "完工结转凭证 = 入库单凭证");

        // ---------- 4001 三末级余额 = 设计 §4.7.5 表（含前置领料凭证）----------
        let all = vec![material, labor_id, ledger_id, overhead_id, settled.finish];
        assert_eq!(money(fixture.balance(&all, "4001.01").await), "3800.0000");
        assert_eq!(money(fixture.balance(&all, "4001.02").await), "444.4448");
        assert_eq!(money(fixture.balance(&all, "4001.03").await), "444.4448");
        let wip: Decimal = fixture.balance(&all, "4001.01").await
            + fixture.balance(&all, "4001.02").await
            + fixture.balance(&all, "4001.03").await;
        let item_wip: String = db()
            .scalar_with_params(
                "SELECT wip_end_cost::text FROM biz_cost_period_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)],
            )
            .await
            .expect("读计算单期末在制");
        assert_eq!(
            money(wip),
            money(dec(&item_wip)),
            "4001 合计 = 计算单期末在制（C-COST-3）"
        );
        assert_eq!(money(wip), "4688.8896");

        // ---------- 4101 清零（C-COST-1）----------
        assert!(fixture.balance(&all, "4101.01").await.is_zero(), "4101.01 应清零");
        assert!(fixture.balance(&all, "4101.02").await.is_zero(), "4101.02 应清零");

        // ---------- 四张凭证同源（红冲按它反查）----------
        for voucher in [&labor, &ledger, &overhead, &finish] {
            assert_eq!(voucher.source_id, Some(period_id), "凭证必须挂结算批次");
        }
        assert_eq!(fixture.settled_voucher_count(period_id).await, 4);
        let ids = cv::voucher_ids_of(db(), period_id).await.expect("按批次查凭证");
        assert_eq!(ids.len(), 4);
        for voucher in [&labor, &ledger, &overhead, &finish] {
            assert!(ids.contains(&voucher.id), "voucher_ids_of 应含 {}", voucher.id);
        }

        fixture.cleanup().await;
    }

    // ==================== 用例 2：PAYROLL / 无费用 ⇒ 只有完工凭证 ====================

    #[tokio::test(flavor = "multi_thread")]
    async fn payroll_source_emits_only_the_finish_voucher() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("PAYROLL").await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;
        // 人工走工资单：即使有工归集行也不出人工计提凭证
        fixture.labor_collect(object_id, "4000").await;
        // 费用侧完全没有（item.oh_cur = 4000 是夹具数字，这里只验「无归集 ⇒ 不出凭证」）

        let settled = fixture.run(period_id, "PAYROLL").await.expect("应成功");
        assert!(settled.labor.is_none(), "PAYROLL ⇒ 不出人工计提凭证（§4.7.2 末段）");
        assert!(settled.overhead.is_none(), "无费归集 ⇒ 不出制造费用结转凭证");
        assert!(settled.ledger.is_none(), "无台账 ⇒ 不出台账凭证");
        assert_eq!(fixture.settled_voucher_count(period_id).await, 1, "只剩完工结转一张");
        assert_balanced(&fixture.voucher(settled.finish).await);

        fixture.cleanup().await;
    }

    // ==================== 用例 3：完工成本 0 ⇒ 409 + 66075 ====================

    #[tokio::test(flavor = "multi_thread")]
    async fn zero_finished_is_rejected_without_writes() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("MANUAL").await;
        let (object_id, _) = fixture.object().await;
        // 全部投入都留在在制：完工 0
        fixture
            .item_with(
                period_id,
                object_id,
                "0",
                "19000",
                "4000",
                "4000",
                ("19.000000", "4.444444", "4.444444"),
                "0",
            )
            .await;
        fixture.labor_collect(object_id, "4000").await;

        let error = fixture.run(period_id, "MANUAL").await.expect_err("完工 0 应拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_FINISH_VOUCHER_EMPTY, "66075");
        assert_eq!(fixture.settled_voucher_count(period_id).await, 0, "一张凭证都不该落");

        fixture.cleanup().await;
    }

    // ==================== 用例 4：账账不符 ⇒ 409 + 66076 ====================

    #[tokio::test(flavor = "multi_thread")]
    async fn item_mismatch_is_rejected_without_writes() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("MANUAL").await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;
        // 工归集 3900 而计算单写 4000（改过归集行 / 计算单不是最后一次 calc）
        fixture.labor_collect(object_id, "3900").await;

        let error = fixture.run(period_id, "MANUAL").await.expect_err("账账不符应拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_VOUCHER_MISMATCH, "66076");
        assert!(error.msg.contains("人工"), "报文应点名要素：{}", error.msg);
        assert_eq!(fixture.settled_voucher_count(period_id).await, 0, "一张凭证都不该落");

        fixture.cleanup().await;
    }

    // ==================== 用例 5：幂等闸 ====================

    #[tokio::test(flavor = "multi_thread")]
    async fn second_settle_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("MANUAL").await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;
        fixture.labor_collect(object_id, "4000").await;
        fixture.ledger_row("4101.01", "2241.01", "4000", None).await;

        fixture.run(period_id, "MANUAL").await.expect("第一次应成功");
        let before = fixture.settled_voucher_count(period_id).await;
        assert_eq!(before, 4);

        let error = fixture.run(period_id, "MANUAL").await.expect_err("第二次应拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_FINISH_ALREADY_SETTLED, "66074");
        assert_eq!(fixture.settled_voucher_count(period_id).await, before, "凭证条数不该变");

        fixture.cleanup().await;
    }

    // ==================== 用例 6：直接行也清零（偏差 1 的正面证据）====================

    #[tokio::test(flavor = "multi_thread")]
    async fn direct_overhead_row_is_cleared_too() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period_id = fixture.period("MANUAL").await;
        let (object_id, _) = fixture.object().await;
        fixture.item(period_id, object_id).await;
        // 池行 2000（4101.01）+ 挂对象的直接行 2000（4101.02）⇒ 合计 4000 = item.oh_cur
        fixture.ledger_row("4101.01", "2241.01", "2000", None).await;
        fixture.ledger_row("4101.02", "2241.01", "2000", Some(object_id)).await;

        let settled = fixture.run(period_id, "MANUAL").await.expect("应成功");
        let overhead = fixture.voucher(settled.overhead.expect("制造费用结转")).await;
        assert_balanced(&overhead);
        assert_eq!(
            amount_of(&overhead, "4001.03", true),
            dec("4000"),
            "借方含池行 + 直接行（否则 Σ ≠ item.oh_cur，4001.03 余额永不等于期末在制）"
        );
        assert_eq!(overhead.entries.len(), 4, "两行借方 + 两行贷方");
        assert_eq!(amount_of(&overhead, "4101.01", false), dec("2000"));
        assert_eq!(amount_of(&overhead, "4101.02", false), dec("2000"));

        // 4101 两个科目都被清零（台账凭证 + 结转凭证互相抵消）
        let all = vec![settled.ledger.expect("台账凭证"), settled.overhead.expect("结转凭证")];
        assert!(fixture.balance(&all, "4101.01").await.is_zero());
        assert!(fixture.balance(&all, "4101.02").await.is_zero());

        fixture.cleanup().await;
    }
}
