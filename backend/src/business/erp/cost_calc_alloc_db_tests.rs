//! calc 第 5 段「分摊计算」的真库用例（`ERP_DB_URL` 开关，B2-10）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_alloc_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`four_bases_allocate_per_object`] | **验收①「四种基数都能算」**：LABOR_HOURS / MACHINE_HOURS / OUTPUT_QTY / MATERIAL_COST 逐对象核数 + Σ = 池 |
//! | 2 | [`remainder_lands_on_one_stored_row`] | **验收②「最大余数法收尾差」**：库里只一行 `rounding_adj ≠ 0`，Σ = 池 |
//! | 3 | [`zero_pool_rule_is_skipped`] | **验收③「池为 0」**：跳过 + 进产出清单，不写 0 元行 |
//! | 4 | [`pool_without_participant_is_409`] | **验收③「不除零」**：池 > 0 无在产对象 → 409 + `66062`，一行不写 |
//! | 5 | [`zero_basis_total_is_409`] | 池 > 0 但机器工时合计 0 → 409 + `66063`（不除零） |
//! | 6 | [`recompute_is_idempotent_and_repools`] | 重算两次行数不翻倍；池变了重算后金额跟着变（整删重灌） |
//! | 7 | [`locked_period_cannot_be_recomputed`] | 批次 SUBMITTED → 409 + `66066`，旧结果**原样保留**（不许留半算状态） |
//! | 8 | [`uncovered_pool_is_diagnosed`] | 有池无规则 → 诊断不报错，其它规则照常摊 |
//!
//! # 夹具纪律
//!
//! 全部用 `CB210-<唯一后缀>` 造；**期间年份按进程内计数器分配**（2100+），
//! 避免 `uk_biz_cost_period` 在多夹具之间撞键。清场按登记 id 删，顺序按 FK：
//! 分摊结果 → 分摊规则 → 计算单 → 批次 → 归集 → 工时 → 费用台账 → 成本对象 → 科目 / 物料 / 仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_alloc_calc_dto::{AllocRunOutcome, RuleOutcome};
    use crate::business::erp::cost_calc_alloc;
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

    /// 期间年份：2100 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2100 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 800).unwrap_or(0)
    }

    /// 编码尾巴（科目 / 仓库编码有长度上限）。
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

    /// 全套夹具：车间 / 产出品 / 仓库 / 科目 / 对象 / 规则 / 池 / 工时 / 批次。
    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        accounts: std::sync::Mutex<Vec<Uuid>>,
        overheads: std::sync::Mutex<Vec<Uuid>>,
        rules: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（`CB210-*` / `CS210-*` / `OH210-*` / `SO210-*` 前缀）。
    ///
    /// 只在每个夹具建之前跑一次：一次性把上一次运行 panic 留下的行按 FK 顺序删干净，
    /// 否则 `uk_biz_cost_period` 会让本轮撞键（实测：`2100-11` 已存在）。
    /// 全部按自己造的前缀 / 名称删，**不会**碰到别人的数据。
    async fn sweep_leftovers() {
        let statements = [
            "DELETE FROM biz_cost_alloc_result WHERE rule_id IN \
                (SELECT id FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB210-%')",
            "DELETE FROM biz_cost_alloc_result WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB210-%')",
            "DELETE FROM biz_cost_overhead WHERE id IN \
                (SELECT source_id FROM biz_cost_collect WHERE source_no LIKE 'OH210-%' AND source_id IS NOT NULL)",
            "DELETE FROM biz_cost_collect WHERE source_no LIKE 'OH210-%' OR source_no LIKE 'SO210-%'",
            "DELETE FROM biz_cost_collect WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB210-%')",
            "DELETE FROM biz_cost_period_item WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS210-%')",
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS210-%'",
            "DELETE FROM biz_cost_labor WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB210-%')",
            "DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'CB210-%'",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB210-%'",
            "DELETE FROM biz_account WHERE account_name LIKE '分摊夹具科目 %'",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB210-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB210W%'",
        ];
        for sql in statements {
            if let Err(error) = db().exec_sql(sql.to_string(), vec![]).await {
                println!("sweep 失败（{sql}）：{error}");
            }
        }
    }

    impl Fixture {
        async fn build() -> Self {
            // 先清掉上轮崩溃留下的夹具（按前缀扫，绝不扫全库）。
            // 为什么必须有：`uk_biz_cost_period` 每期间只允许一个活跃批次，
            // 上一次失败留下的 `2100-11` 批次会让本轮直接撞键（实测发生过）。
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
                    Value::Text(format!("CB210-{suffix}")),
                    Value::Text(format!("分摊夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB210W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("分摊夹具仓 {suffix}")),
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
                accounts: std::sync::Mutex::new(Vec::new()),
                overheads: std::sync::Mutex::new(Vec::new()),
                rules: std::sync::Mutex::new(Vec::new()),
                periods: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 夹具期间（11 月）。
        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        /// 本夹具的规则编码前缀。
        ///
        /// 真库（演练库）里可能还有**别的卡**留下的规则：它们在本期间没有池行，
        /// 会出现在 `zero_pool` 里。断言必须只看自己的规则，否则「库里有多少条规则」
        /// 这种环境相关的东西会让用例时红时绿。
        fn rule_prefix(&self) -> String {
            format!("CB210-{}-", &self.suffix[..6])
        }

        /// 产出里属于本夹具的规则。
        fn my_rules<'a>(&'a self, outcome: &'a AllocRunOutcome) -> Vec<&'a RuleOutcome> {
            let prefix = self.rule_prefix();
            outcome
                .rules
                .iter()
                .filter(|rule| rule.rule_code.starts_with(&prefix))
                .collect()
        }

        /// 本夹具规则的 (池合计, 分摊合计)。
        fn my_totals(&self, outcome: &AllocRunOutcome) -> (Decimal, Decimal) {
            self.my_rules(outcome)
                .iter()
                .fold((Decimal::ZERO, Decimal::ZERO), |(pool, alloc), rule| {
                    (pool + rule.pool_amount, alloc + rule.alloc_total)
                })
        }

        /// 造一个启用末级科目 `4101.<8 位数字><tail>`（尾段必须是数字）。
        async fn account(&self, tail: &str) -> String {
            let code = format!("4101.{}{}", tail_code(&self.suffix), tail);
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                                          account_category, balance_direction, is_leaf, status, \
                                          created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, NULL, 2, 'COST', 'DEBIT', 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(code.clone()),
                    Value::Text(format!("分摊夹具科目 {code}")),
                ],
            )
            .await
            .expect("造科目夹具");
            self.accounts.lock().expect("科目清单").push(id);
            code
        }

        /// 造一张成本对象（`OPEN` / `CLOSED`）。
        async fn object(&self, status: &str) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB210-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?::uuid, ?::uuid, 100, ?::uuid, '分摊夹具车间', ?::text::date, ?, \
                             {closed_at}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(format!("{}-11-01", self.year)),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            (id, object_no)
        }

        /// 造一条「车间费用池」来源：手工费用台账行 + 归集行（科目从台账反查）。
        async fn pool(&self, account_code: &str, amount: &str) -> Uuid {
            let period_code = self.period_code();
            let overhead_id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULL, ?::uuid, '分摊夹具车间', 'OTHER', ?, '1002', \
                         ?::text::numeric, 'MANUAL', '分摊夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&overhead_id),
                    Value::Text(period_code.clone()),
                    Value::from(&self.dept),
                    Value::Text(account_code.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造费用台账夹具");
            self.overheads.lock().expect("台账清单").push(overhead_id);
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, amount, dept_id, \
                                               dept_name, remark, created_at, updated_at) \
                 VALUES (?::uuid, NULL, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, ?, 0, ?::text::date, \
                         ?::text::numeric, ?::uuid, '分摊夹具车间', '分摊夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(period_code.clone()),
                    Value::from(&overhead_id),
                    Value::Text(format!("OH210-{}", &overhead_id.to_string()[28..])),
                    Value::Text(format!("{period_code}-28")),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造归集夹具");
            overhead_id
        }

        /// 造工时行（`CONFIRMED`）。
        async fn labor(&self, object_id: Uuid, hours: &str, machine_hours: &str) {
            let period_code = self.period_code();
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, '分摊夹具员工', ?::uuid, '分摊夹具车间', \
                         ?::text::numeric, ?::text::numeric, 30.000000, \
                         round(?::text::numeric * 30.000000, 4), 'MANUAL', 'CONFIRMED', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(period_code.clone()),
                    Value::Text(format!("{period_code}-15")),
                    Value::from(&object_id),
                    Value::from(&self.dept),
                    Value::Text(hours.to_string()),
                    Value::Text(machine_hours.to_string()),
                    Value::Text(hours.to_string()),
                ],
            )
            .await
            .expect("造工时夹具");
        }

        /// 造料归集行（`element='MATERIAL'`，让对象成为「有投入」的参与对象）。
        async fn material(&self, object_id: Uuid, amount: &str) {
            let period_code = self.period_code();
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, material_id, qty, \
                                               unit_price, amount, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, ?, 0, ?::text::date, \
                         ?::uuid, 10, 1.000000, ?::text::numeric, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object_id),
                    Value::Text(period_code.clone()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(format!("SO210-{}", &object_id.to_string()[28..])),
                    Value::Text(format!("{period_code}-10")),
                    Value::from(&self.product),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造料归集夹具");
        }

        /// 造成本批次（`DRAFT`）+ 每个对象一张计算单（`completed_qty` 由调用方给）。
        async fn period(&self, items: &[(Uuid, &str)]) -> Uuid {
            let period_code = self.period_code();
            let period_id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&period_id),
                    Value::Text(period_code.clone()),
                    Value::Text(format!("CS210-{}-{}", period_code.replace('-', ""), &self.suffix[..4])),
                ],
            )
            .await
            .expect("造成本批次夹具");
            self.periods.lock().expect("批次清单").push(period_id);
            for (object_id, qty) in items {
                db().exec_sql(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                       completed_qty, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&Uuid::now_v7()),
                        Value::from(&period_id),
                        Value::from(object_id),
                        Value::from(&self.product),
                        Value::Text(qty.to_string()),
                    ],
                )
                .await
                .expect("造计算单夹具");
            }
            period_id
        }

        /// 改批次状态（模拟「提交审批」）。
        ///
        /// `ck_biz_cost_period_appr` 要求 `SUBMITTED / APPROVED / SETTLED / CLOSED` 必须有
        /// `approval_instance_id`，所以一并补上（这不是本卡的口径，是库层约束）。
        async fn set_period_status(&self, status: &str) {
            let needs_approval = matches!(status, "SUBMITTED" | "APPROVED" | "SETTLED" | "CLOSED");
            let sql = if needs_approval {
                "UPDATE biz_cost_period SET status = ?, approval_instance_id = ?::uuid WHERE period_code = ?"
            } else {
                "UPDATE biz_cost_period SET status = ? WHERE period_code = ?"
            };
            let params = if needs_approval {
                vec![
                    Value::Text(status.to_string()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(self.period_code()),
                ]
            } else {
                vec![Value::Text(status.to_string()), Value::Text(self.period_code())]
            };
            db().exec_sql(sql.to_string(), params).await.expect("改批次状态");
        }

        /// 直接建规则（绕过 handler：本卡只测分摊段，规则 CRUD 是 B2-5 的验收项）。
        async fn rule(&self, code_tail: &str, account_code: &str, basis: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, source_dept_id, \
                                                   source_dept_name, account_code, basis, \
                                                   effective_from, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?::uuid, '分摊夹具车间', ?, ?, ?::text::date, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB210-{}-{code_tail}", &self.suffix[..6])),
                    Value::Text(format!("分摊夹具规则 {code_tail}")),
                    Value::from(&self.dept),
                    Value::Text(account_code.to_string()),
                    Value::Text(basis.to_string()),
                    Value::Text(format!("{}-01-01", self.year)),
                ],
            )
            .await
            .expect("造分摊规则夹具");
            self.rules.lock().expect("规则清单").push(id);
            id
        }

        /// 读回当期分摊结果：`(科目, 对象) → (基数, 分摊额, 尾差)`。
        async fn stored_rows(&self) -> Vec<(String, Uuid, String, String, String)> {
            let rows = db()
                .query_sql(
                    "SELECT account_code, cost_object_id, basis_value::text AS bv, \
                            alloc_amount::text AS aa, rounding_adj::text AS ra \
                       FROM biz_cost_alloc_result WHERE period_code = ? \
                      ORDER BY account_code, cost_object_id"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await
                .expect("读分摊结果");
            rows.iter()
                .map(|row| {
                    (
                        row.get_typed::<String>("account_code").expect("科目"),
                        row.get_typed::<Uuid>("cost_object_id").expect("对象"),
                        row.get_typed::<String>("bv").expect("基数"),
                        row.get_typed::<String>("aa").expect("分摊额"),
                        row.get_typed::<String>("ra").expect("尾差"),
                    )
                })
                .collect()
        }

        /// 当期 ALLOC 归集行的 `(对象, 金额)` 合计与行数。
        async fn alloc_collect(&self) -> (i64, String) {
            let rows = db()
                .query_sql(
                    "SELECT count(*)::bigint AS n, coalesce(sum(amount), 0)::text AS total \
                       FROM biz_cost_collect \
                      WHERE period_code = ? AND source_type = 'ALLOC' AND deleted_at IS NULL"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await
                .expect("读 ALLOC 归集行");
            let row = rows.first().expect("聚合行");
            (
                row.get_typed::<i64>("n").expect("行数"),
                row.get_typed::<String>("total").expect("合计"),
            )
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
            let rules = joined(&self.rules);
            let periods = joined(&self.periods);
            let overheads = joined(&self.overheads);
            let accounts = joined(&self.accounts);
            let mut statements = vec![format!(
                "DELETE FROM biz_cost_alloc_result WHERE period_code = '{}'",
                self.period_code()
            )];
            if !rules.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_alloc_rule WHERE id IN ({rules})"));
            }
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            statements.push(format!(
                "DELETE FROM biz_cost_collect WHERE period_code = '{}'",
                self.period_code()
            ));
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_labor WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            if !overheads.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_overhead WHERE id IN ({overheads})"));
            }
            if !accounts.is_empty() {
                statements.push(format!("DELETE FROM biz_account WHERE id IN ({accounts})"));
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

    /// `alloc_amount` 在库里是 `NUMERIC(18,4)`，读回来是 `"600.0000"` 这种形态。
    fn money(value: &str) -> String {
        let parsed = value.parse::<f64>().expect("金额");
        format!("{parsed:.4}")
    }

    /// 库里读回的 `NUMERIC(18,4)` 与算法算出的 `Decimal` 标度常常不同
    /// （`1500.0000` vs `1500`）：比较前统一规范到 4 位小数。
    fn amount_of(fx: &Fixture, outcome: &AllocRunOutcome, rule_code_tail: &str, object_id: Uuid) -> String {
        let prefix = fx.rule_prefix();
        let rule = outcome
            .rules
            .iter()
            .find(|rule| rule.rule_code.starts_with(&prefix) && rule.rule_code.ends_with(rule_code_tail))
            .unwrap_or_else(|| panic!("产出里没有本夹具的规则 {rule_code_tail}"));
        let line = rule
            .lines
            .iter()
            .find(|line| line.cost_object_id == object_id)
            .unwrap_or_else(|| panic!("规则 {rule_code_tail} 没有对象 {object_id}"));
        money(&line.alloc_amount.to_string())
    }

    // ==================== 用例 ====================

    /// ① **四种基数都能算**：同一批对象、四个科目、四种标准，逐对象核数 + Σ = 池。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn four_bases_allocate_per_object() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        // 三个对象：两种工时 + 三种料成本 + 三种完工数量，四组数字互不相同，
        // 这样「标准取错」不可能靠巧合通过。
        let (o1, _) = fx.object("OPEN").await;
        let (o2, _) = fx.object("OPEN").await;
        let (o3, _) = fx.object("CLOSED").await;
        fx.labor(o1, "60", "10").await;
        fx.labor(o2, "20", "30").await;
        fx.labor(o3, "20", "60").await;
        fx.material(o1, "600").await;
        fx.material(o2, "200").await;
        fx.material(o3, "200").await;
        fx.period(&[(o1, "50"), (o2, "30"), (o3, "20")]).await;

        let a_labor = fx.account("01").await;
        let a_machine = fx.account("02").await;
        let a_output = fx.account("03").await;
        let a_material = fx.account("04").await;
        fx.rule("L", &a_labor, "LABOR_HOURS").await;
        fx.rule("M", &a_machine, "MACHINE_HOURS").await;
        fx.rule("Q", &a_output, "OUTPUT_QTY").await;
        fx.rule("C", &a_material, "MATERIAL_COST").await;
        for account in [&a_labor, &a_machine, &a_output, &a_material] {
            fx.pool(account, "1000").await;
        }

        let outcome = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("分摊失败：{} {}", error.status, error.msg);
            }
        };
        let mine = fx.my_rules(&outcome);
        assert_eq!(mine.len(), 4, "四条规则都要摊出去");
        let (pool_total, alloc_total) = fx.my_totals(&outcome);
        assert_eq!(pool_total, alloc_total, "Σ摊 = Σ池");
        assert_eq!(money(&pool_total.to_string()), "4000.0000");
        assert_eq!(outcome.result_rows, 12, "4 规则 × 3 对象");
        assert_eq!(outcome.collect_rows, 12);
        assert!(
            !outcome
                .zero_pool
                .iter()
                .any(|rule| rule.rule_code.starts_with(&fx.rule_prefix())),
            "本夹具的规则都有池，不该进 zero_pool"
        );
        assert!(outcome.uncovered.is_empty(), "本期间没有「有池无规则」的科目");

        // 工时 60/20/20（合计 100）→ 600/200/200
        for (object, expect) in [(o1, "600"), (o2, "200"), (o3, "200")] {
            assert_eq!(amount_of(&fx, &outcome, "-L", object), money(expect), "按工时分摊");
        }
        // 机器工时 10/30/60（合计 100）→ 100/300/600
        for (object, expect) in [(o1, "100"), (o2, "300"), (o3, "600")] {
            assert_eq!(amount_of(&fx, &outcome, "-M", object), money(expect), "按机器工时分摊");
        }
        // 完工数量 50/30/20（合计 100）→ 500/300/200
        for (object, expect) in [(o1, "500"), (o2, "300"), (o3, "200")] {
            assert_eq!(amount_of(&fx, &outcome, "-Q", object), money(expect), "按完工产量分摊");
        }
        // 料成本 600/200/200（合计 1000）→ 600/200/200
        for (object, expect) in [(o1, "600"), (o2, "200"), (o3, "200")] {
            assert_eq!(amount_of(&fx, &outcome, "-C", object), money(expect), "按直接材料分摊");
        }

        // 逐规则 Σ = 池（快照断言，出参层）；基数合计按标准各不相同
        for rule in &mine {
            let expect_basis = if rule.rule_code.ends_with("-C") {
                "1000.0000"
            } else {
                "100.0000"
            };
            assert_eq!(
                money(&rule.pool_amount.to_string()),
                "1000.0000",
                "{} 池",
                rule.rule_code
            );
            assert_eq!(
                money(&rule.alloc_total.to_string()),
                "1000.0000",
                "{} 摊不平",
                rule.rule_code
            );
            assert_eq!(
                money(&rule.basis_total.to_string()),
                expect_basis,
                "{} 基数合计",
                rule.rule_code
            );
        }

        // 库里逐对象核数（不只信内存里的产出）：一张期望表覆盖 4 科目 × 3 对象
        let stored = fx.stored_rows().await;
        assert_eq!(stored.len(), 12);
        let expected: Vec<(String, Uuid, &str, &str)> = vec![
            (a_labor.clone(), o1, "60", "600"),
            (a_labor.clone(), o2, "20", "200"),
            (a_labor.clone(), o3, "20", "200"),
            (a_machine.clone(), o1, "10", "100"),
            (a_machine.clone(), o2, "30", "300"),
            (a_machine.clone(), o3, "60", "600"),
            (a_output.clone(), o1, "50", "500"),
            (a_output.clone(), o2, "30", "300"),
            (a_output.clone(), o3, "20", "200"),
            (a_material.clone(), o1, "600", "600"),
            (a_material.clone(), o2, "200", "200"),
            (a_material.clone(), o3, "200", "200"),
        ];
        for (account, object, basis, alloc, adjust) in &stored {
            let row = expected
                .iter()
                .find(|(acc, id, _, _)| acc == account && *id == *object)
                .unwrap_or_else(|| panic!("库里多出 unexpected 行：{account} / {object}"));
            assert_eq!(money(basis), money(row.2), "科目 {account} 的基数落库值");
            assert_eq!(money(alloc), money(row.3), "科目 {account} 的分摊额落库值");
            assert_eq!(money(adjust), "0.0000", "整除场景不该有尾差");
        }

        // ALLOC 归集行：12 行、合计 4000（计算单将来按它汇总）
        let (rows, total) = fx.alloc_collect().await;
        assert_eq!(rows, 12);
        assert_eq!(money(&total), "4000.0000");

        fx.cleanup().await;
    }

    /// ② **最大余数法收尾差**：1000 摊给 3 个等基数对象 → 库里只一行带尾差，Σ = 池。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remainder_lands_on_one_stored_row() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        let (o2, _) = fx.object("OPEN").await;
        let (o3, _) = fx.object("OPEN").await;
        for object in [o1, o2, o3] {
            fx.labor(object, "10", "0").await;
        }
        let account = fx.account("01").await;
        fx.rule("L", &account, "LABOR_HOURS").await;
        fx.pool(&account, "1000").await;

        let outcome = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("分摊失败：{} {}", error.status, error.msg);
            }
        };
        let mine = fx.my_rules(&outcome);
        assert_eq!(mine.len(), 1, "本夹具一条规则");
        let rule = mine[0];
        assert_eq!(money(&rule.alloc_total.to_string()), "1000.0000", "Σ摊必须等于池");
        assert_eq!(
            money(&rule.rounding_total.to_string()),
            "0.0001",
            "1000/3 的余数是 0.0001"
        );

        let stored = fx.stored_rows().await;
        assert_eq!(stored.len(), 3);
        let adjusted = stored
            .iter()
            .filter(|(_, _, _, _, adjust)| money(adjust) != "0.0000")
            .collect::<Vec<_>>();
        assert_eq!(adjusted.len(), 1, "尾差只能落在一行：{stored:?}");
        assert_eq!(money(&adjusted[0].4), "0.0001");
        let total = stored
            .iter()
            .map(|(_, _, _, alloc, _)| alloc.parse::<f64>().expect("分摊额"))
            .sum::<f64>();
        assert!((total - 1000.0).abs() < 1e-9, "库里 Σ 摊 = {total}");
        // 三行是 333.3334 / 333.3333 / 333.3333（不能是 333.33 × 3 = 999.99）
        let mut amounts = stored
            .iter()
            .map(|(_, _, _, alloc, _)| money(alloc))
            .collect::<Vec<_>>();
        amounts.sort();
        assert_eq!(
            amounts,
            vec!["333.3333".to_string(), "333.3333".to_string(), "333.3334".to_string()]
        );

        fx.cleanup().await;
    }

    /// ③ **池为 0**：规则在、池没有 → 跳过并记进产出，一行不写（不是静默跳过）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_pool_rule_is_skipped() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        fx.labor(o1, "10", "5").await;
        let account = fx.account("01").await;
        fx.rule("L", &account, "LABOR_HOURS").await;
        // 不造池

        let outcome = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("池为 0 不该报错：{} {}", error.status, error.msg);
            }
        };
        assert!(fx.my_rules(&outcome).is_empty(), "池为 0 不该产出分摊结果");
        let skipped = outcome
            .zero_pool
            .iter()
            .find(|rule| rule.rule_code.starts_with(&fx.rule_prefix()))
            .expect("本夹具的规则应进 zero_pool");
        assert_eq!(skipped.account_code, account);
        assert!(skipped.reason.contains("池 = 0"));
        assert_eq!(outcome.result_rows, 0);
        assert_eq!(money(&outcome.total_pool.to_string()), "0.0000");
        assert!(fx.stored_rows().await.is_empty(), "池为 0 不许写 0 元结果行");

        fx.cleanup().await;
    }

    /// ④ **池 > 0 但无在产对象**：409 + `66062`，一行不写（不许「挂池里留到下期」）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pool_without_participant_is_409() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        // 对象存在但当期没有料 / 工投入 → 不算参与分摊
        let _ = fx.object("OPEN").await;
        let account = fx.account("01").await;
        fx.rule("L", &account, "LABOR_HOURS").await;
        fx.pool(&account, "4000").await;

        let error = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => {
                fx.cleanup().await;
                panic!("无在产对象必须 409，实际成功：{}", outcome.summary_line());
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_NO_OBJECT);
        assert!(error.msg.contains("4000"), "报文点名金额：{}", error.msg);
        assert!(error.msg.contains(&account), "报文点名科目：{}", error.msg);
        assert!(error.msg.contains("没有在产成本对象"), "报文说清原因：{}", error.msg);
        assert!(fx.stored_rows().await.is_empty(), "失败必须一行不写");

        fx.cleanup().await;
    }

    /// ⑤ **基数合计 0**：池 > 0、对象参与，但机器工时全是 0 → 409 + `66063`（不除零）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_basis_total_is_409() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        let (o2, _) = fx.object("OPEN").await;
        fx.labor(o1, "10", "0").await;
        fx.labor(o2, "20", "0").await;
        let account = fx.account("01").await;
        fx.rule("M", &account, "MACHINE_HOURS").await;
        fx.pool(&account, "1000").await;

        let error = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => {
                fx.cleanup().await;
                panic!("基数合计 0 必须 409，实际成功：{}", outcome.summary_line());
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_BASIS_ZERO);
        assert!(error.msg.contains("机器工时"), "报文点名标准：{}", error.msg);
        assert!(fx.stored_rows().await.is_empty());

        fx.cleanup().await;
    }

    /// ⑥ **重算幂等**：跑两次行数不翻倍；池变了重算后金额跟着变（整删重灌）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recompute_is_idempotent_and_repools() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        let (o2, _) = fx.object("OPEN").await;
        fx.material(o1, "600").await;
        fx.material(o2, "400").await;
        let account = fx.account("01").await;
        fx.rule("C", &account, "MATERIAL_COST").await;
        fx.pool(&account, "1000").await;

        let first = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("首跑失败：{} {}", error.status, error.msg);
            }
        };
        assert_eq!(first.result_rows, 2);
        assert_eq!(first.purged_rows, 0, "首跑没有旧行可清");
        assert_eq!(amount_of(&fx, &first, "-C", o1), "600.0000");
        assert_eq!(amount_of(&fx, &first, "-C", o2), "400.0000");

        // 第二次：行数不翻倍、金额不变（幂等）
        let second = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("重算失败：{} {}", error.status, error.msg);
            }
        };
        assert_eq!(second.result_rows, 2);
        assert_eq!(second.purged_rows, 2, "重算要先清掉上一轮的 2 行");
        assert_eq!(fx.stored_rows().await.len(), 2, "重算不许翻倍");
        assert_eq!(amount_of(&fx, &second, "-C", o1), "600.0000");
        let (rows, total) = fx.alloc_collect().await;
        assert_eq!(rows, 2, "ALLOC 归集行也要跟着整删重灌");
        assert_eq!(money(&total), "1000.0000");

        // 池追加 500 → 重算后按新池摊（600:400 → 900:600）
        fx.pool(&account, "500").await;
        let third = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("追加池后重算失败：{} {}", error.status, error.msg);
            }
        };
        assert_eq!(money(&third.total_pool.to_string()), "1500.0000");
        assert_eq!(amount_of(&fx, &third, "-C", o1), "900.0000");
        assert_eq!(amount_of(&fx, &third, "-C", o2), "600.0000");
        let (rows, total) = fx.alloc_collect().await;
        assert_eq!(rows, 2);
        assert_eq!(money(&total), "1500.0000");

        fx.cleanup().await;
    }

    /// ⑦ **批次已提交 → 不许重算**：409 + `66066`，且旧结果原样保留（不留半算状态）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn locked_period_cannot_be_recomputed() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        fx.material(o1, "1000").await;
        let account = fx.account("01").await;
        fx.rule("C", &account, "MATERIAL_COST").await;
        fx.pool(&account, "800").await;
        fx.period(&[(o1, "10")]).await;

        let first = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("DRAFT 期间应能摊：{} {}", error.status, error.msg);
            }
        };
        assert_eq!(first.result_rows, 1);

        fx.set_period_status("SUBMITTED").await;
        let error = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => {
                fx.cleanup().await;
                panic!("SUBMITTED 期间不该允许重算：{}", outcome.summary_line());
            }
            Err(error) => error,
        };
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_PERIOD_LOCKED);
        assert!(error.msg.contains("SUBMITTED"), "报文带状态：{}", error.msg);
        assert!(error.msg.contains("撤回"), "报文带修法：{}", error.msg);

        let stored = fx.stored_rows().await;
        assert_eq!(stored.len(), 1, "被拒后旧结果必须原样保留（不许先删后失败）");
        assert_eq!(money(&stored[0].3), "800.0000");

        fx.cleanup().await;
    }

    /// ⑧ **有池无规则** → 诊断不报错；同期间其它有规则的科目照常摊。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn uncovered_pool_is_diagnosed() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (o1, _) = fx.object("OPEN").await;
        fx.material(o1, "100").await;
        let covered_account = fx.account("01").await;
        let orphan_account = fx.account("09").await;
        fx.rule("C", &covered_account, "MATERIAL_COST").await;
        fx.pool(&covered_account, "100").await;
        fx.pool(&orphan_account, "250").await;

        let outcome = match cost_calc_alloc::allocate_overhead(&fx.period_code(), None).await {
            Ok(outcome) => outcome,
            Err(error) => {
                fx.cleanup().await;
                panic!("未覆盖池是配置缺失，不该报错：{} {}", error.status, error.msg);
            }
        };
        assert_eq!(fx.my_rules(&outcome).len(), 1, "有规则的那条照常摊");
        let mine_uncovered = outcome
            .uncovered
            .iter()
            .filter(|row| row.account_code == orphan_account)
            .collect::<Vec<_>>();
        assert_eq!(mine_uncovered.len(), 1, "没规则覆盖的池必须列出来");
        assert_eq!(money(&mine_uncovered[0].amount.to_string()), "250.0000");
        assert!(mine_uncovered[0].reason.contains("没有生效的分摊规则覆盖"));
        assert_eq!(fx.stored_rows().await.len(), 1, "只写覆盖到的那个科目");

        fx.cleanup().await;
    }
}
