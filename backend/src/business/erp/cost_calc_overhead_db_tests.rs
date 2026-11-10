//! calc 第 3 段（费归集）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_overhead_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`collects_the_three_sources_and_builds_the_pool`] | **验收项三路**：折旧 `POSTED` / 报销 `APPROVED` / 手工台账各自归集，金额 = 来源账上值；池按 (车间, 科目) 汇总 |
//! | 2 | [`ignores_non_posted_and_non_4101_sources`] | 折旧 `REVERSED` / 报销 `DRAFT,CANCELLED` / 上期 / 5602.* 科目 / 已软删台账 → 一行都不写 |
//! | 3 | [`hand_ledger_with_cost_object_is_collected_directly`] | **验收项「直接行」**：挂了对象的手工台账 `cost_object_id` 非空、**不进池** |
//! | 4 | [`rerun_is_idempotent_and_leaves_rows_untouched`] | 重复跑 `inserted=0 / skipped=全部`，行 id 与 `updated_at` 不动 |
//! | 5 | [`purges_rows_whose_source_no_longer_qualifies`] | 折旧红冲 / 报销作废 / 台账软删 ⇒ 旧归集行软删（留痕），第三次跑收敛为无操作 |
//! | 6 | [`zero_amount_rows_go_to_the_uncollected_list`] | 0 元报销明细不写 collect、进未归集清单、带修复建议 |
//! | 7 | [`pool_matches_b2_5_preview_and_scanned_lines`] | 池三方互校：本段池 = B2-5 试算池（同口径 SQL）= 扫描行纯函数汇总 |
//! | 8 | [`empty_period_is_a_clean_noop`] | 空期间一行不写、不报错；非法期间 400 |
//!
//! # 夹具纪律
//!
//! - 每个夹具独占一个**期间**（年份 = `2000 + slot % 80`、月份 = `1 + (slot / 80) % 12`），
//!   否则池汇总与已归集行会把别的用例残留算进来；
//! - 清场**按前缀 + 期间删**（绝不按期间盲删），顺序按 FK：
//!   归集（三路来源）→ 折旧明细 → 折旧批次 → 报销单（明细 CASCADE）→ 手工台账 →
//!   固定资产卡片 → 归集（按成本对象）→ 成本对象；
//! - 三路来源行**直接 SQL 造**（不经 handler）：本卡要验「扫描口径」，
//!   且要造出 `REVERSED` 折旧、`CANCELLED` 报销、5602.* 科目这些 API 层拦不到、
//!   但历史数据里确实存在的形态。
//! - 折旧批次一个期间只有一批（`uk_biz_dep_batch_period`），夹具按期间 get-or-create。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_overhead as segment;
    use crate::business::erp::cost_overhead_collect_dto::{
        OverheadCollectOutcome, SOURCE_DEPRECIATION, SOURCE_EXPENSE, SOURCE_OVERHEAD_TICKET,
    };
    use nik::prelude::chrono::{DateTime, Utc};
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
        Decimal::from_str(text).expect("金额字面量")
    }

    /// 进程内递增槽位：决定夹具独占的会计期间。
    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// uuid 尾 12 位（v7 前 8 位是时间戳，同毫秒连造会撞唯一键）。
    fn tail(id: &Uuid) -> String {
        id.simple().to_string()[20..].to_ascii_uppercase()
    }

    // ==================== 夹具 ====================

    struct Fixture {
        suffix: String,
        period: String,
        in_date: String,
        prev_period: String,
        dept: Uuid,
        dept_name: String,
        reporter: Uuid,
        /// 本夹具造出来的折旧批次 / 固定资产卡片（清场按 id 删，绝不按前缀扫全库 ——
        /// 全仓测试并行时按前缀删会误伤同模块别的用例）。
        batches: std::sync::Mutex<Vec<Uuid>>,
        assets: std::sync::Mutex<Vec<Uuid>>,
    }

    impl Fixture {
        async fn build() -> Self {
            let slot = next_slot();
            let year = 2000 + (slot % 80) as i32;
            let month = 1 + ((slot / 80) % 12) as u32;
            let (prev_year, prev_month) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
            let period = format!("{year:04}-{month:02}");
            Self::reset_period(&period).await;

            let dept = crate::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let reporter = crate::shared::test_helpers::ensure_any_user().await;

            Self {
                suffix: format!("{}{slot}", period.replace('-', "")),
                period,
                in_date: format!("{year:04}-{month:02}-10"),
                prev_period: format!("{prev_year:04}-{prev_month:02}"),
                dept,
                dept_name,
                reporter,
                batches: std::sync::Mutex::new(Vec::new()),
                assets: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 造成本对象（直接行要用）。
        async fn object(&self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  created_at, updated_at) \
                     VALUES (?::uuid, ?, gen_random_uuid(), gen_random_uuid(), 100, ?::uuid, ?, '{}', ?, now(), now())",
                    self.in_date
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB28-{}-{}", tail(&id), self.suffix)),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 取（或建）某期间的折旧批次：`uk_biz_dep_batch_period` 一个期间只有一批。
        async fn batch(&self, period: &str) -> Uuid {
            let existing: Option<Uuid> = db()
                .query_sql(
                    "SELECT id FROM biz_depreciation_batch WHERE period_code = ?".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await
                .expect("查折旧批次")
                .first()
                .map(|row| row.get_typed("id").expect("批次 id"));
            if let Some(id) = existing {
                return id;
            }
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_depreciation_batch (id, batch_no, period_code, status, asset_count, \
                                             total_amount, voucher_id, run_at, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'POSTED', 0, 0, gen_random_uuid(), now(), now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("B28DEP-{}-{}", tail(&id), period.replace('-', ""))),
                    Value::Text(period.to_string()),
                ],
            )
            .await
            .expect("造折旧批次夹具");
            self.batches.lock().expect("批次登记锁").push(id);
            id
        }
        /// 造一台在役资产卡（金额口径满足 `ck_biz_fa_*`）。
        async fn asset(&self, no_suffix: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_fixed_asset (id, asset_no, asset_name, category_id, category_code, \
                                                  use_dept_id, use_dept_name, original_value, residual_value, \
                                                  depreciable_base, depreciation_method, useful_life_months, \
                                                  monthly_depreciation, start_depreciation_date, \
                                                  accumulated_depreciation, net_value, \
                                                  depreciation_months_done, purchase_date, source_type, status, \
                                                  created_at, updated_at) \
                     VALUES (?::uuid, ?, 'B28 用例设备', gen_random_uuid(), 'B28CAT', ?::uuid, ?, \
                             10000, 500, 9500, 'STRAIGHT_LINE', 60, 0, '{}', 0, 10000, 0, '{}', \
                             'MANUAL', 'IN_USE', now(), now())",
                    self.in_date, self.in_date
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("B28FA-{}-{no_suffix}", tail(&id))),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                ],
            )
            .await
            .expect("造固定资产卡片夹具");
            self.assets.lock().expect("卡片登记锁").push(id);
            id
        }

        /// 造一条折旧明细。`status` = `POSTED` / `REVERSED`。
        async fn depreciation(&self, period: &str, amount: &str, account: &str, status: &str, no_suffix: &str) -> Uuid {
            let batch = self.batch(period).await;
            let asset = self.asset(no_suffix).await;
            let id = Uuid::now_v7();
            let asset_no: String = db()
                .scalar_with_params(
                    "SELECT asset_no FROM biz_fixed_asset WHERE id = ?::uuid",
                    vec![Value::from(&asset)],
                )
                .await
                .expect("取资产编号");
            db().exec_sql(
                "INSERT INTO biz_depreciation (id, batch_id, asset_id, asset_no, asset_name, period_code, \
                                               dept_id, dept_name, expense_account_id, expense_account_code, \
                                               amount, accumulated_before, accumulated_after, net_value_after, \
                                               is_last_month, status, created_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?, 'B28 用例设备', ?, ?::uuid, ?, gen_random_uuid(), ?, \
                          ?::text::numeric, 0, ?::text::numeric, 10000, 0, ?, now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&batch),
                    Value::from(&asset),
                    Value::Text(asset_no),
                    Value::Text(period.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(account.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造折旧明细夹具");
            id
        }

        /// 造一张报销单（含明细），返回 `(report_id, item_ids)`。
        ///
        /// `items` = `(科目, 不含税金额, 税额)`；单头合计按明细加总（库有多道恒等式）。
        async fn expense(&self, period: &str, status: &str, items: &[(&str, &str, &str)]) -> (Uuid, Vec<Uuid>) {
            let report_id = Uuid::now_v7();
            let report_no = format!("B28EXP-{}-{}", tail(&report_id), period.replace('-', ""));
            let mut total = Decimal::ZERO;
            let mut tax_total = Decimal::ZERO;
            for (_, amount, tax) in items {
                total += dec(amount);
                tax_total += dec(tax);
            }
            let with_tax = total + tax_total;
            let cancel_reason = if status == "CANCELLED" { "用例作废" } else { "" };
            db().exec_sql(
                "INSERT INTO biz_expense_report (id, report_no, reporter_id, reporter_name, dept_id, dept_name, \
                                                  title, expense_date, period_code, total_amount, total_tax, \
                                                  total_with_tax, item_count, status, paid_amount, unpaid_amount, \
                                                  pay_status, cancel_reason, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, 'B28 用例报销人', ?::uuid, ?, 'B28 真库用例', ?::text::date, ?, \
                          ?::text::numeric, ?::text::numeric, ?::text::numeric, ?, ?, 0, ?::text::numeric, \
                          'UNPAID', NULLIF(?::text,''), now(), now())"
                    .to_string(),
                vec![
                    Value::from(&report_id),
                    Value::Text(report_no),
                    Value::from(&self.reporter),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(self.in_date.clone()),
                    Value::Text(period.to_string()),
                    Value::Text(total.to_string()),
                    Value::Text(tax_total.to_string()),
                    Value::Text(with_tax.to_string()),
                    Value::I32(i32::try_from(items.len()).unwrap_or(1)),
                    Value::Text(status.to_string()),
                    Value::Text(with_tax.to_string()),
                    Value::Text(cancel_reason.to_string()),
                ],
            )
            .await
            .expect("造报销单夹具");

            let mut ids = Vec::new();
            for (index, (account, amount, tax)) in items.iter().enumerate() {
                let id = Uuid::now_v7();
                db().exec_sql(
                    "INSERT INTO biz_expense_item (id, report_id, line_no, expense_date, expense_type_id, \
                                                   type_code, type_name, account_id, account_code, account_name, \
                                                   amount, tax_rate, tax_amount, with_tax_amount, summary, \
                                                   created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?::text::date, gen_random_uuid(), 'B28TYPE', 'B28 用例费用类型', \
                             gen_random_uuid(), ?, 'B28 科目', ?::text::numeric, 1, ?::text::numeric, \
                             ?::text::numeric, 'B28 摘要', now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&id),
                        Value::from(&report_id),
                        Value::I32(i32::try_from(index + 1).unwrap_or(1)),
                        Value::Text(self.in_date.clone()),
                        Value::Text(account.to_string()),
                        Value::Text(amount.to_string()),
                        Value::Text(tax.to_string()),
                        Value::Text((dec(amount) + dec(tax)).to_string()),
                    ],
                )
                .await
                .expect("造报销明细夹具");
                ids.push(id);
            }
            (report_id, ids)
        }

        /// 造一条手工台账。`object` 非空 = 直接归集，空 = 进池。
        async fn overhead(&self, period: &str, amount: &str, account: &str, object: Option<Uuid>) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                    expense_item, debit_account_code, credit_account_code, \
                                                    amount, source_type, remark, created_at, updated_at) \
                     VALUES (?::uuid, ?, {}, ?::uuid, ?, 'REPAIR', ?, '1002', ?::text::numeric, 'MANUAL', \
                             'B28-{}', now(), now())",
                    if object.is_some() { "?::uuid" } else { "NULL" },
                    self.suffix
                ),
                {
                    let mut params = vec![Value::from(&id), Value::Text(period.to_string())];
                    if let Some(object) = object {
                        params.push(Value::from(&object));
                    }
                    params.extend([
                        Value::from(&self.dept),
                        Value::Text(self.dept_name.clone()),
                        Value::Text(account.to_string()),
                        Value::Text(amount.to_string()),
                    ]);
                    params
                },
            )
            .await
            .expect("造手工台账夹具");
            id
        }

        /// 清掉本期间里**本卡前缀**的残留（跨次运行的兜底；期间槽位会复用）。
        async fn reset_period(period: &str) {
            for sql in [
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'DEPRECIATION' \
                    AND source_id IN (SELECT id FROM biz_depreciation WHERE asset_no LIKE 'B28FA-%')"
                    .to_string(),
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'EXPENSE' \
                    AND source_id IN (SELECT ei.id FROM biz_expense_item ei \
                                        JOIN biz_expense_report r ON r.id = ei.report_id \
                                       WHERE r.report_no LIKE 'B28EXP-%')"
                    .to_string(),
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'OVERHEAD_TICKET' \
                    AND source_id IN (SELECT id FROM biz_cost_overhead WHERE remark LIKE 'B28-%')"
                    .to_string(),
                "DELETE FROM biz_depreciation WHERE asset_no LIKE 'B28FA-%' AND period_code = ?".to_string(),
                "DELETE FROM biz_depreciation_batch WHERE period_code = ?".to_string(),
                "DELETE FROM biz_expense_report WHERE report_no LIKE 'B28EXP-%'".to_string(),
                "DELETE FROM biz_cost_overhead WHERE remark LIKE 'B28-%'".to_string(),
                // 只清「没有折旧明细引用」的孤儿卡片：并行测试不能把别人正在用的卡删掉
                "DELETE FROM biz_fixed_asset \
                  WHERE asset_no LIKE 'B28FA-%' \
                    AND NOT EXISTS (SELECT 1 FROM biz_depreciation dp WHERE dp.asset_id = biz_fixed_asset.id)"
                    .to_string(),
            ] {
                let params = if sql.contains('?') {
                    vec![Value::Text(period.to_string())]
                } else {
                    vec![]
                };
                let _ = db().exec_sql(sql, params).await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect \
                      WHERE cost_object_id IN (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB28-%')"
                        .to_string(),
                    vec![],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB28-%'".to_string(),
                    vec![],
                )
                .await;
        }

        /// 清场：按 id 删（FK 顺序：归集 → 三个来源 → 成本对象）。
        async fn cleanup(&self, objects: &[Uuid], dep: &[Uuid], reports: &[Uuid], overheads: &[Uuid]) {
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            for ids in [dep, overheads] {
                if !ids.is_empty() {
                    let _ = db()
                        .exec_sql(
                            format!("DELETE FROM biz_cost_collect WHERE source_id IN ({})", list(ids)),
                            vec![],
                        )
                        .await;
                }
            }
            if !reports.is_empty() {
                let reports_list = list(reports);
                let _ = db()
                    .exec_sql(
                        format!(
                            "DELETE FROM biz_cost_collect WHERE source_id IN \
                               (SELECT id FROM biz_expense_item WHERE report_id IN ({reports_list}))"
                        ),
                        vec![],
                    )
                    .await;
            }
            if !dep.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_depreciation WHERE id IN ({})", list(dep)),
                        vec![],
                    )
                    .await;
            }
            if !overheads.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_overhead WHERE id IN ({})", list(overheads)),
                        vec![],
                    )
                    .await;
            }
            if !reports.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_expense_report WHERE id IN ({})", list(reports)),
                        vec![],
                    )
                    .await;
            }
            if !objects.is_empty() {
                let objects_list = list(objects);
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_object WHERE id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
            }
            // 折旧批次（CASCADE 掉本批次的明细行）与资产卡：按**注册过的 id** 删，
            // 不按 `B28FA-%` 前缀扫全库（并行测试会误伤同模块别的用例）
            let batches = self.batches.lock().expect("批次登记锁").clone();
            if !batches.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_depreciation WHERE batch_id IN ({})", list(&batches)),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_depreciation_batch WHERE id IN ({})", list(&batches)),
                        vec![],
                    )
                    .await;
            }
            let assets = self.assets.lock().expect("卡片登记锁").clone();
            if !assets.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_fixed_asset WHERE id IN ({})", list(&assets)),
                        vec![],
                    )
                    .await;
            }
        }
    }

    // ==================== 查询辅助 ====================

    /// 一条费归集行（断言用）。
    #[derive(Debug)]
    struct CollectRow {
        id: Uuid,
        element: String,
        source_type: String,
        source_id: Uuid,
        line_no: i32,
        cost_object_id: Option<Uuid>,
        material_id: Option<Uuid>,
        qty: Option<Decimal>,
        amount: Decimal,
        dept_id: Option<Uuid>,
        remark: Option<String>,
        deleted: bool,
    }

    /// 当期费归集行（`live` = 只看未删）。
    async fn rows_of_period(period: &str, live: bool) -> Vec<CollectRow> {
        let sql = format!(
            "SELECT id, element, source_type, source_id, line_no, cost_object_id, material_id, qty, \
                    amount, dept_id, remark, (deleted_at IS NOT NULL) AS deleted \
               FROM biz_cost_collect \
              WHERE period_code = ? AND element = 'OVERHEAD' \
                AND source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET') {} \
              ORDER BY source_type, source_id, id",
            if live { "AND deleted_at IS NULL" } else { "" }
        );
        db().query_sql(sql, vec![Value::Text(period.to_string())])
            .await
            .expect("查费归集行")
            .iter()
            .map(|row| CollectRow {
                id: row.get_typed("id").expect("id"),
                element: row.get_typed("element").expect("element"),
                source_type: row.get_typed("source_type").expect("source_type"),
                source_id: row.get_typed("source_id").expect("source_id"),
                line_no: row.get_typed("line_no").expect("line_no"),
                cost_object_id: row.get_typed("cost_object_id").expect("cost_object_id"),
                material_id: row.get_typed("material_id").expect("material_id"),
                qty: row.get_typed("qty").expect("qty"),
                amount: row.get_typed("amount").expect("amount"),
                dept_id: row.get_typed("dept_id").expect("dept_id"),
                remark: row.get_typed("remark").expect("remark"),
                deleted: row.get_typed("deleted").expect("deleted"),
            })
            .collect()
    }

    /// 当期费归集行按来源类型计数。
    fn count_of(rows: &[CollectRow], source_type: &str) -> usize {
        rows.iter().filter(|row| row.source_type == source_type).count()
    }

    /// 金额合计。
    fn sum_of(rows: &[CollectRow]) -> Decimal {
        rows.iter().map(|row| row.amount).sum()
    }

    /// `(id, updated_at)` 快照，验「重复跑不刷时间戳」。
    async fn stamps(period: &str) -> Vec<(Uuid, DateTime<Utc>)> {
        db().query_sql(
            "SELECT id, updated_at FROM biz_cost_collect \
              WHERE period_code = ? AND element = 'OVERHEAD' AND deleted_at IS NULL ORDER BY id"
                .to_string(),
            vec![Value::Text(period.to_string())],
        )
        .await
        .expect("取更新时间")
        .iter()
        .map(|row| {
            (
                row.get_typed("id").expect("id"),
                row.get_typed("updated_at").expect("updated_at"),
            )
        })
        .collect()
    }

    /// 跑一段（收窄失败信息：带上 `internal_detail`，否则只看到「服务器内部错误」）。
    async fn run(period: &str) -> OverheadCollectOutcome {
        segment::collect_overhead(period, None).await.unwrap_or_else(|error| {
            panic!(
                "费归集失败：{} / {} / {:?}",
                error.status, error.msg, error.internal_detail
            )
        })
    }

    /// 池里某 (车间, 科目) 的金额。
    fn pool_amount(outcome: &OverheadCollectOutcome, dept: Uuid, account: &str) -> Decimal {
        outcome
            .pool
            .iter()
            .find(|row| row.dept_id == dept && row.account_code == account)
            .map(|row| row.amount)
            .unwrap_or(Decimal::ZERO)
    }

    // ==================== 用例 ====================

    /// ① 三路来源全归集 + 池按 (车间, 科目) 汇总（验收项主体）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collects_the_three_sources_and_builds_the_pool() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        // 折旧 1000（4101.01）
        let dep = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "A1")
            .await;
        // 报销两笔：水电 300（4101.02）
        let (report, items) = fx
            .expense(&fx.period, "APPROVED", &[("4101.02", "300.0000", "0")])
            .await;
        // 手工台账：池行 400（4101.04）
        let pooled = fx.overhead(&fx.period, "400.0000", "4101.04", None).await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 3, "三路各一行");
        assert_eq!(outcome.inserted, 3);
        assert_eq!(outcome.skipped_existing, 0);
        assert_eq!(outcome.purged, 0);
        assert_eq!(outcome.uncollected_rows(), 0);
        assert_eq!(outcome.inserted_amount, dec("1700.0000"));
        assert_eq!(outcome.inserted_pool_amount, dec("1700.0000"), "三行全是池行");
        assert_eq!(outcome.inserted_direct_amount, Decimal::ZERO);

        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 3);
        assert_eq!(count_of(&rows, SOURCE_DEPRECIATION), 1);
        assert_eq!(count_of(&rows, SOURCE_EXPENSE), 1);
        assert_eq!(count_of(&rows, SOURCE_OVERHEAD_TICKET), 1);
        assert_eq!(sum_of(&rows), dec("1700.0000"), "归集金额 = 来源账上值之和");
        for row in &rows {
            assert_eq!(row.element, "OVERHEAD");
            assert_eq!(row.line_no, 0, "费三路的幂等键第三段都是 0");
            assert!(row.cost_object_id.is_none(), "池行不许挂对象");
            assert!(row.material_id.is_none(), "费行不许挂物料");
            assert!(row.qty.is_none());
            assert_eq!(row.dept_id, Some(fx.dept));
            assert!(row.remark.as_deref().unwrap_or("").contains("4101."));
        }
        // 逐路金额对得上账（金额取来源账上值，不重算）
        let dep_row = rows
            .iter()
            .find(|row| row.source_type == SOURCE_DEPRECIATION)
            .expect("折旧行");
        assert_eq!(dep_row.source_id, dep);
        assert_eq!(dep_row.amount, dec("1000.0000"));
        let exp_row = rows
            .iter()
            .find(|row| row.source_type == SOURCE_EXPENSE)
            .expect("报销行");
        assert_eq!(exp_row.source_id, items[0]);
        assert_eq!(exp_row.amount, dec("300.0000"), "报销取不含税金额");
        let oh_row = rows
            .iter()
            .find(|row| row.source_type == SOURCE_OVERHEAD_TICKET)
            .expect("手工台账行");
        assert_eq!(oh_row.source_id, pooled);
        assert_eq!(oh_row.amount, dec("400.0000"));

        // 验收项：按 (车间, 科目) 汇总成池
        assert_eq!(outcome.pool.len(), 3, "三个不同科目各成一行");
        assert_eq!(pool_amount(&outcome, fx.dept, "4101.01"), dec("1000.0000"));
        assert_eq!(pool_amount(&outcome, fx.dept, "4101.02"), dec("300.0000"));
        assert_eq!(pool_amount(&outcome, fx.dept, "4101.04"), dec("400.0000"));
        assert_eq!(outcome.pool_amount(), dec("1700.0000"));
        assert!(outcome.pool.iter().all(|row| row.rows == 1));
        assert!(outcome.pool.iter().all(|row| row.dept_name.is_some()));

        fx.cleanup(&[], &[dep], &[report], &[pooled]).await;
    }

    /// ② 口径闸：非 POSTED 折旧 / 非 APPROVED 报销 / 上期 / 5602.* 科目 / 已软删台账 全不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ignores_non_posted_and_non_4101_sources() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        // 该写的只有这一行折旧
        let good = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "G1")
            .await;
        // 已红冲的折旧
        let reversed = fx
            .depreciation(&fx.period, "777.0000", "4101.01", "REVERSED", "R1")
            .await;
        // 上期的折旧（POSTED，但期间不对）
        let prev = fx
            .depreciation(&fx.prev_period, "888.0000", "4101.01", "POSTED", "P1")
            .await;
        // 科目不在 4101.*：不进成本（不是错误，是配置）
        let not_cost = fx.depreciation(&fx.period, "666.0000", "5602.01", "POSTED", "N1").await;

        // 报销：草稿 / 作废 / 上期 / 5602.* 明细
        let (draft, _) = fx.expense(&fx.period, "DRAFT", &[("4101.02", "100.0000", "0")]).await;
        let (cancelled, _) = fx
            .expense(&fx.period, "CANCELLED", &[("4101.02", "200.0000", "0")])
            .await;
        let (prev_report, _) = fx
            .expense(&fx.prev_period, "APPROVED", &[("4101.02", "300.0000", "0")])
            .await;
        let (not_cost_report, _) = fx
            .expense(&fx.period, "APPROVED", &[("5602.01", "400.0000", "0")])
            .await;

        // 手工台账：已软删
        let deleted_oh = fx.overhead(&fx.period, "555.0000", "4101.04", None).await;
        db().exec_sql(
            "UPDATE biz_cost_overhead SET deleted_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&deleted_oh)],
        )
        .await
        .expect("软删台账夹具");

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 1, "只有 good 那一行符合口径");
        assert_eq!(outcome.inserted, 1);
        assert_eq!(outcome.inserted_amount, dec("1000.0000"));

        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_id, good);
        // 逐类点名：这些都不许出现
        for (name, id) in [
            ("已红冲折旧", reversed),
            ("上期折旧", prev),
            ("5602 折旧", not_cost),
            ("软删台账", deleted_oh),
        ] {
            assert!(!rows.iter().any(|row| row.source_id == id), "{name} 不该进归集");
        }
        // 上期的也不许串到本期
        assert!(rows_of_period(&fx.prev_period, true).await.is_empty());

        fx.cleanup(
            &[],
            &[good, reversed, prev, not_cost],
            &[draft, cancelled, prev_report, not_cost_report],
            &[deleted_oh],
        )
        .await;
    }

    /// ③ 验收项「直接行」：挂了成本对象的手工台账直接计入对象、不进池。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hand_ledger_with_cost_object_is_collected_directly() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let direct = fx.overhead(&fx.period, "600.0000", "4101.04", Some(object)).await;
        let pooled = fx.overhead(&fx.period, "150.0000", "4101.04", None).await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.inserted, 2);
        assert_eq!(outcome.inserted_direct_amount, dec("600.0000"));
        assert_eq!(outcome.inserted_pool_amount, dec("150.0000"));
        assert_eq!(outcome.inserted_amount, dec("750.0000"));

        let rows = rows_of_period(&fx.period, true).await;
        let direct_row = rows.iter().find(|row| row.source_id == direct).expect("直接行");
        assert_eq!(direct_row.cost_object_id, Some(object), "直接行必须挂对象");
        let pooled_row = rows.iter().find(|row| row.source_id == pooled).expect("池行");
        assert!(pooled_row.cost_object_id.is_none());

        // 池里只有池行那 150，直接行不进池（否则会既直接计入又参与分摊）
        assert_eq!(outcome.pool.len(), 1);
        assert_eq!(outcome.pool[0].amount, dec("150.0000"));
        assert_eq!(outcome.pool[0].rows, 1);
        assert_eq!(outcome.collected_amount, dec("750.0000"), "回查含直接行");

        fx.cleanup(&[object], &[], &[], &[direct, pooled]).await;
    }

    /// ④ 幂等：重复跑不重复写、不刷 `updated_at`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rerun_is_idempotent_and_leaves_rows_untouched() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let dep = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "I1")
            .await;
        let (report, items) = fx
            .expense(&fx.period, "PAID", &[("4101.02", "300.0000", "18.0000")])
            .await;
        let oh = fx.overhead(&fx.period, "400.0000", "4101.04", None).await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 3);
        assert_eq!(first.collected_rows, 3);
        let before = stamps(&fx.period).await;

        let second = run(&fx.period).await;
        assert_eq!(second.inserted, 0, "重复跑不许重复写");
        assert_eq!(second.skipped_existing, 3);
        assert_eq!(second.purged, 0);
        assert_eq!(second.scanned, 3);
        assert_eq!(second.collected_amount, first.collected_amount);
        let after = stamps(&fx.period).await;
        assert_eq!(before, after, "幂等跳过不许刷 updated_at / id");

        // 报销明细金额 = 不含税 300（18 元税不进成本）
        let rows = rows_of_period(&fx.period, true).await;
        let exp_row = rows
            .iter()
            .find(|row| row.source_type == SOURCE_EXPENSE)
            .expect("报销行");
        assert_eq!(exp_row.source_id, items[0]);
        assert_eq!(exp_row.amount, dec("300.0000"));

        fx.cleanup(&[], &[dep], &[report], &[oh]).await;
    }

    /// ⑤ 收敛：来源作废后重跑 ⇒ 旧行软删（留痕），第三次跑无操作。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn purges_rows_whose_source_no_longer_qualifies() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let dep = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "S1")
            .await;
        let (report, _) = fx
            .expense(&fx.period, "APPROVED", &[("4101.02", "300.0000", "0")])
            .await;
        let oh = fx.overhead(&fx.period, "400.0000", "4101.04", None).await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 3);
        let live_ids: Vec<Uuid> = rows_of_period(&fx.period, true)
            .await
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(live_ids.len(), 3);
        db().exec_sql(
            "UPDATE biz_depreciation SET status = 'REVERSED', reversed_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&dep)],
        )
        .await
        .expect("红冲折旧");
        db().exec_sql(
            "UPDATE biz_expense_report SET status = 'CANCELLED', cancel_reason = '用例作废', \
                    cancelled_at = now() WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&report)],
        )
        .await
        .expect("作废报销");
        db().exec_sql(
            "UPDATE biz_cost_overhead SET deleted_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&oh)],
        )
        .await
        .expect("软删台账");

        let second = run(&fx.period).await;
        assert_eq!(second.scanned, 0);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.purged, 3, "三路来源都不合口径 ⇒ 三行全清");
        assert_eq!(second.purged_amount, dec("1700.0000"));
        assert_eq!(second.collected_rows, 0);
        assert!(second.pool.is_empty());
        assert!(second.purged_detail.len() <= 3);

        // 软删留痕（可追溯），不是物理删除
        let all = rows_of_period(&fx.period, false).await;
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|row| row.deleted), "旧行必须软删留痕");
        let purged_ids: Vec<Uuid> = all.iter().map(|row| row.id).collect();
        assert_eq!(purged_ids.len(), live_ids.len(), "软删不许换行 id");
        assert!(live_ids.iter().all(|id| purged_ids.contains(id)));

        // 第三次跑：收敛为无操作
        let third = run(&fx.period).await;
        assert_eq!(third.inserted, 0);
        assert_eq!(third.purged, 0);
        assert!(third.is_clean());

        fx.cleanup(&[], &[dep], &[report], &[oh]).await;
    }

    /// ⑥ 0 元报销明细：不写 collect，进未归集清单并带修复建议。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_amount_rows_go_to_the_uncollected_list() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let good = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "Z1")
            .await;
        // 明细不含税金额 0、税额 13（`with_tax > 0` 的库约束下唯一能造出的 0 元形态）
        let (report, items) = fx.expense(&fx.period, "APPROVED", &[("4101.02", "0", "13.0000")]).await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 2);
        assert_eq!(outcome.inserted, 1, "0 元行不许写归集行");
        assert_eq!(outcome.inserted_amount, dec("1000.0000"));
        assert_eq!(outcome.uncollected_rows(), 1);
        let item = &outcome.uncollected[0];
        assert_eq!(item.source_type, SOURCE_EXPENSE);
        assert_eq!(item.source_id, items[0]);
        assert!(item.reason.contains("金额为 0"), "{}", item.reason);
        assert_eq!(item.account_code, "4101.02");
        assert!(item.hint.as_deref().unwrap_or("").contains("核对"), "要给出怎么修");
        assert!(!outcome.is_clean());

        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 1);
        assert!(!rows.iter().any(|row| row.amount.is_zero()), "池里不许出现 0 元行");

        fx.cleanup(&[], &[good], &[report], &[]).await;
    }

    /// ⑦ 池三方互校：本段池 = B2-5 试算池（同口径 SQL）= 扫描行纯函数汇总。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pool_matches_b2_5_preview_and_scanned_lines() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let dep = fx
            .depreciation(&fx.period, "1000.0000", "4101.01", "POSTED", "M1")
            .await;
        let (report, _) = fx
            .expense(&fx.period, "APPROVED", &[("4101.02", "300.0000", "0")])
            .await;
        let object = fx.object("OPEN").await;
        let direct = fx.overhead(&fx.period, "600.0000", "4101.04", Some(object)).await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.inserted, 3);

        // (a) 纯函数：扫描到的池行按 (车间, 科目) 汇总
        let scanned = crate::business::erp::cost_overhead_collect_model::all_source_lines(db(), &fx.period)
            .await
            .expect("扫候选行");
        let pure = crate::business::erp::cost_overhead_collect_dto::pool_of(&scanned);
        let mut pure_sorted = pure.clone();
        pure_sorted.sort_by(|a, b| (a.dept_id, &a.account_code).cmp(&(b.dept_id, &b.account_code)));
        let mut db_sorted = outcome.pool.clone();
        db_sorted.sort_by(|a, b| (a.dept_id, &a.account_code).cmp(&(b.dept_id, &b.account_code)));
        assert_eq!(
            pure_sorted
                .iter()
                .map(|row| (row.dept_id, row.account_code.clone(), row.amount))
                .collect::<Vec<_>>(),
            db_sorted
                .iter()
                .map(|row| (row.dept_id, row.account_code.clone(), row.amount))
                .collect::<Vec<_>>(),
            "池 = 扫描行纯函数汇总（直接行既不在扫描池里、也不在库里）"
        );

        // (b) B2-5 试算池（`cost_alloc_rule_model::pool_rows` 用全局 db()，口径与本段逐条相同）
        let preview = crate::business::erp::cost_alloc_rule_model::pool_rows(&fx.period)
            .await
            .expect("B2-5 试算池");
        let preview_total: Decimal = preview.iter().map(|row| row.amount).sum();
        assert_eq!(preview_total, outcome.pool_amount(), "两个池口径必须一致");
        assert_eq!(
            preview_total,
            dec("1300.0000"),
            "池 = 折旧 1000 + 报销 300（直接行 600 不进池）"
        );

        fx.cleanup(&[object], &[dep], &[report], &[direct]).await;
    }

    /// ⑧ 空期间：一行不写、不报错；非法期间 400。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_period_is_a_clean_noop() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 0);
        assert_eq!(outcome.inserted, 0);
        assert_eq!(outcome.collected_rows, 0);
        assert_eq!(outcome.collected_amount, Decimal::ZERO);
        assert!(outcome.pool.is_empty());
        assert!(outcome.is_clean());

        let err = segment::collect_overhead("2026-13", None)
            .await
            .expect_err("非法期间必须 400");
        assert_eq!(err.status, 400);

        fx.cleanup(&[], &[], &[], &[]).await;
    }
}
