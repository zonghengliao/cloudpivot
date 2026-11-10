//! calc 第 2 段（工归集）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_labor_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`collects_only_confirmed_labor_of_the_period`] | **验收项「只取 CONFIRMED 状态」**：`DRAFT` / 上期 / 已软删三类行一行都不写；金额取台账 `amount`（= `round(hours × rate, 4)`） |
//! | 2 | [`rerun_does_not_duplicate_and_leaves_rows_untouched`] | 重复跑 `inserted=0 / skipped=全部`，行 id 与 `updated_at` 不动 |
//! | 3 | [`missing_rate_rows_go_to_the_uncollected_list`] | **验收项「缺费率的行进未归集清单而不是算 0」**：`rate = 0` 的行不写 collect、金额合计里没有它、未归集清单带工时与修复建议 |
//! | 4 | [`purges_rows_whose_source_no_longer_qualifies`] | 工时行被作废后重跑 ⇒ 旧归集行软删（留痕），第三次跑收敛为无操作 |
//! | 5 | [`relinked_cost_object_moves_the_collected_amount`] | 工时行改挂别的成本对象 ⇒ 旧行清掉、新行落到新对象（幂等键是工时行 id） |
//! | 6 | [`period_without_confirmed_labor_is_a_clean_noop`] | 空期间：一行不写、不报错、金额 0；非法期间 400 |
//!
//! # 夹具纪律
//!
//! - 每个夹具独占一个**期间**（年份 = `2000 + slot % 80`、月份 = `1 + (slot / 80) % 12`），
//!   否则 `labor_collect_rows_of_period` 会把别的用例残留算进来；
//! - 清场**按前缀 + id 删**（绝不按期间盲删），顺序按 FK：
//!   归集（按工时行来源）→ 工时台账 → 归集（按成本对象）→ 成本对象；
//! - 工时台账行**直接 SQL 造**（不经 handler）：本卡要验「扫描口径」，
//!   且要造出 `rate = 0`（费率未维护）这类 API 层拦不到、但历史数据里确实存在的形态。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_labor as segment;
    use crate::business::erp::cost_labor_collect_dto::LaborCollectOutcome;
    use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
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

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 进程内递增槽位：决定夹具独占的会计期间（跨次运行的残留由 [`Fixture::reset_period`] 清掉）。
    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// 造一条工时台账要给的形态。
    struct Labor {
        /// `DRAFT` / `CONFIRMED`
        status: &'static str,
        /// 期间（`yyyy-MM`），默认用夹具当期。
        period: Option<String>,
        /// 工作日期（`yyyy-MM-dd`）。
        date: String,
        /// 工时 / 费率（金额由 SQL 的 `round(hours × rate, 4)` 算，与库 CHECK 同引擎）。
        hours: &'static str,
        rate: &'static str,
        /// 挂的成本对象。
        object: Uuid,
        /// 软删（历史脏数据形态）。
        deleted: bool,
    }

    struct Fixture {
        suffix: String,
        period: String,
        in_date: String,
        prev_period: String,
        prev_date: String,
        dept: Uuid,
        dept_name: String,
        product: Uuid,
        warehouse: Uuid,
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

            Self {
                suffix: format!("{}{slot}", period.replace('-', "")),
                period,
                in_date: format!("{year:04}-{month:02}-10"),
                prev_period: format!("{prev_year:04}-{prev_month:02}"),
                prev_date: format!("{prev_year:04}-{prev_month:02}-10"),
                dept,
                dept_name,
                // 成本对象的 product_id / target_warehouse_id 是**弱引用**（无 FK），
                // 本段用不到物料与仓库，给占位 uuid 即可（少造两张表的两行夹具）。
                product: Uuid::now_v7(),
                warehouse: Uuid::now_v7(),
            }
        }

        /// 造成本对象。
        async fn object(&self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?::uuid, ?::uuid, 100, ?::uuid, ?, '{}', ?, {closed_at}, now(), now())",
                    self.in_date
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB27-{}-{}", &id.simple().to_string()[20..], self.suffix)),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 造一条工时台账行（金额由 SQL 算，与 `ck_biz_cost_labor_amount` 同引擎）。
        async fn labor(&self, row: Labor) -> Uuid {
            let id = Uuid::now_v7();
            let period = row.period.clone().unwrap_or_else(|| self.period.clone());
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, ?, ?::uuid, ?, \
                         ?::text::numeric, 0, ?::text::numeric, \
                         round(?::text::numeric * ?::text::numeric, 4), \
                         'MANUAL', ?, ?, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period),
                    Value::Text(row.date),
                    Value::from(&row.object),
                    Value::Text(format!("B27人员{}", &self.suffix)),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(row.hours.to_string()),
                    Value::Text(row.rate.to_string()),
                    Value::Text(row.hours.to_string()),
                    Value::Text(row.rate.to_string()),
                    Value::Text(row.status.to_string()),
                    Value::Text(format!("B27-{}", self.suffix)),
                ],
            )
            .await
            .expect("造工时台账夹具");

            if row.deleted {
                db().exec_sql(
                    "UPDATE biz_cost_labor SET deleted_at = now() WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&id)],
                )
                .await
                .expect("软删工时台账夹具");
            }
            id
        }

        /// 清掉本期间里**本卡前缀**的残留（跨次运行的兜底；期间槽位会复用）。
        async fn reset_period(period: &str) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect \
                      WHERE source_type = 'LABOR_TICKET' \
                        AND source_id IN (SELECT id FROM biz_cost_labor WHERE remark LIKE 'B27-%')"
                        .to_string(),
                    vec![],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_labor WHERE remark LIKE 'B27-%' AND period_code = ?".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect \
                      WHERE period_code = ? AND source_type = 'LABOR_TICKET' \
                        AND cost_object_id IN (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB27-%')"
                        .to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object \
                      WHERE object_no LIKE 'CB27-%' \
                        AND start_date >= (? || '-01')::date \
                        AND start_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
        }

        /// 清场：按 id 删（FK 顺序：归集 → 工时台账 → 成本对象）。
        async fn cleanup(&self, objects: &[Uuid], labors: &[Uuid]) {
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !labors.is_empty() {
                let labors_list = list(labors);
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_collect WHERE source_id IN ({labors_list})"),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_labor WHERE id IN ({labors_list})"),
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
        }
    }

    /// 一条工归集行（断言用）。
    #[derive(Debug)]
    struct CollectRow {
        id: Uuid,
        period_code: String,
        element: String,
        source_type: String,
        source_id: Uuid,
        line_no: i32,
        occur_date: NaiveDate,
        cost_object_id: Uuid,
        material_id: Option<Uuid>,
        qty: Option<Decimal>,
        amount: Decimal,
        dept_id: Option<Uuid>,
        dept_name: Option<String>,
        remark: Option<String>,
        deleted: bool,
    }

    /// 当期工归集行（`live` = 只看未删）。
    async fn rows_of_period(period: &str, live: bool) -> Vec<CollectRow> {
        let sql = format!(
            "SELECT id, period_code, element, source_type, source_id, line_no, occur_date, \
                    cost_object_id, material_id, qty, amount, dept_id, dept_name, remark, \
                    (deleted_at IS NOT NULL) AS deleted \
               FROM biz_cost_collect \
              WHERE period_code = ? AND element = 'LABOR' AND source_type = 'LABOR_TICKET' {} \
              ORDER BY occur_date, source_id, id",
            if live { "AND deleted_at IS NULL" } else { "" }
        );
        db().query_sql(sql, vec![Value::Text(period.to_string())])
            .await
            .expect("查工归集行")
            .iter()
            .map(|row| CollectRow {
                id: row.get_typed("id").expect("id"),
                period_code: row.get_typed("period_code").expect("period"),
                element: row.get_typed("element").expect("element"),
                source_type: row.get_typed("source_type").expect("source_type"),
                source_id: row.get_typed("source_id").expect("source_id"),
                line_no: row.get_typed("line_no").expect("line_no"),
                occur_date: row.get_typed("occur_date").expect("occur_date"),
                cost_object_id: row.get_typed("cost_object_id").expect("cost_object_id"),
                material_id: row.get_typed("material_id").expect("material_id"),
                qty: row.get_typed("qty").expect("qty"),
                amount: row.get_typed("amount").expect("amount"),
                dept_id: row.get_typed("dept_id").expect("dept_id"),
                dept_name: row.get_typed("dept_name").expect("dept_name"),
                remark: row.get_typed("remark").expect("remark"),
                deleted: row.get_typed("deleted").expect("deleted"),
            })
            .collect()
    }

    /// `(id, updated_at)` 快照，验「重复跑不刷时间戳」。
    async fn stamps(period: &str) -> Vec<(Uuid, DateTime<Utc>)> {
        db().query_sql(
            "SELECT id, updated_at FROM biz_cost_collect \
              WHERE period_code = ? AND deleted_at IS NULL ORDER BY id"
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

    /// 跑一段（收窄失败信息）。
    async fn run(period: &str) -> LaborCollectOutcome {
        segment::collect_labor(period, None)
            .await
            .unwrap_or_else(|error| panic!("工归集失败：{} / {}", error.status, error.msg))
    }

    // ==================== 用例 ====================

    /// ① 验收项「只取 CONFIRMED 状态」：`DRAFT` / 上期 / 已软删一律不写。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collects_only_confirmed_labor_of_the_period() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let other = fx.object("OPEN").await;

        // 该归集的两行（同一对象、两天、各 8 小时 × 50 = 400.0000）
        let first = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "8",
                rate: "50.000000",
                object,
                deleted: false,
            })
            .await;
        // 第二行费率带小数：1.5 × 33.333333 = 49.9999995 → round(…,4) = 50.0000
        let second = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "1.5",
                rate: "33.333333",
                object,
                deleted: false,
            })
            .await;

        // 不该扫的三类：草稿 / 上期 / 已软删
        let draft = fx
            .labor(Labor {
                status: "DRAFT",
                period: None,
                date: fx.in_date.clone(),
                hours: "8",
                rate: "50.000000",
                object: other,
                deleted: false,
            })
            .await;
        let prev = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: Some(fx.prev_period.clone()),
                date: fx.prev_date.clone(),
                hours: "8",
                rate: "50.000000",
                object: other,
                deleted: false,
            })
            .await;
        let soft_deleted = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "8",
                rate: "50.000000",
                object: other,
                deleted: true,
            })
            .await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 2, "只有两行已确认的当期工时该被扫到");
        assert_eq!(outcome.inserted, 2);
        assert_eq!(outcome.skipped_existing, 0);
        assert_eq!(outcome.purged, 0);
        assert_eq!(outcome.collected_rows, 2);
        assert_eq!(outcome.collected_amount, dec("450.0000"));
        assert_eq!(outcome.inserted_amount, dec("450.0000"));
        assert!(outcome.uncollected.is_empty(), "本例没有缺费率行");
        assert!(outcome.is_clean());

        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.period_code, fx.period);
            assert_eq!(row.element, "LABOR");
            assert_eq!(row.source_type, "LABOR_TICKET");
            assert_eq!(row.line_no, 0, "工行来源行号恒为 0（设计 §4.3 幂等键）");
            assert!(row.source_id == first || row.source_id == second);
            assert_eq!(row.occur_date, day(&fx.in_date), "occur_date 取工作日期");
            assert_eq!(row.cost_object_id, object);
            assert!(row.material_id.is_none(), "工行不许挂物料（ck_biz_cost_collect_mat）");
            assert!(row.qty.is_none(), "工行不许带数量");
            assert_eq!(row.dept_id, Some(fx.dept));
            assert_eq!(row.dept_name.as_deref(), Some(&*fx.dept_name));
            let remark = row.remark.as_deref().expect("工时行备注非空");
            assert!(remark.contains("工时台账"), "备注要能看懂这一行是什么：{remark}");
            assert!(remark.contains("B27人员"), "备注带员工姓名快照：{remark}");
            assert!(!row.deleted);
        }
        // 验收项「金额 = 工时 × 费率」：取台账金额（= round(hours × rate, 4)）
        let first_row = rows.iter().find(|row| row.source_id == first).expect("第一行");
        assert_eq!(first_row.amount, dec("400.0000"));
        let second_row = rows.iter().find(|row| row.source_id == second).expect("第二行");
        assert_eq!(second_row.amount, dec("50.0000"), "1.5 × 33.333333 舍入到 4 位");

        fx.cleanup(&[object, other], &[first, second, draft, prev, soft_deleted])
            .await;
        assert!(rows_of_period(&fx.period, false).await.is_empty(), "清场后本期间零残留");
    }

    /// ② 重复跑不重复写：`inserted=0 / skipped=全部`，行 id 与 `updated_at` 不动。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rerun_does_not_duplicate_and_leaves_rows_untouched() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let a = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "8",
                rate: "50.000000",
                object,
                deleted: false,
            })
            .await;
        let b = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "2",
                rate: "12.500000",
                object,
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 2);
        let before = rows_of_period(&fx.period, true).await;
        let stamps_before = stamps(&fx.period).await;

        let second = run(&fx.period).await;
        assert_eq!(second.scanned, 2);
        assert_eq!(second.inserted, 0, "重复跑不许再写");
        assert_eq!(second.skipped_existing, 2);
        assert_eq!(second.purged, 0);
        assert_eq!(second.collected_rows, 2, "行数没有翻倍");
        assert_eq!(second.collected_amount, dec("425.0000"));

        let after = rows_of_period(&fx.period, true).await;
        assert_eq!(after.len(), before.len());
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(old.id, new.id, "同一工时行必须还是同一归集行（幂等键撞上就跳过）");
            assert_eq!(old.amount, new.amount);
        }
        assert_eq!(stamps(&fx.period).await, stamps_before, "重复跑不许刷 updated_at");

        fx.cleanup(&[object], &[a, b]).await;
    }

    /// ③ 验收项「缺费率的行进未归集清单而不是算 0」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_rate_rows_go_to_the_uncollected_list() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;

        let good = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "8",
                rate: "50.000000",
                object,
                deleted: false,
            })
            .await;
        // 费率未维护的已确认工时（台账层合法：ck_biz_cost_labor_rate 只要求 ≥ 0）
        let no_rate = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "4",
                rate: "0",
                object,
                deleted: false,
            })
            .await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 2, "两行都已确认、都在当期");
        assert_eq!(outcome.inserted, 1, "只有有费率的那一行能写");
        assert_eq!(outcome.collected_rows, 1);
        assert_eq!(outcome.collected_amount, dec("400.0000"), "缺费率那笔不许按 0 计入");
        assert_eq!(outcome.inserted_amount, dec("400.0000"));

        assert_eq!(outcome.uncollected_rows(), 1, "缺费率行要进未归集清单");
        let item = &outcome.uncollected[0];
        assert_eq!(item.labor_id, no_rate);
        assert_eq!(item.cost_object_id, object);
        assert_eq!(item.hours, dec("4"));
        assert_eq!(item.rate, Decimal::ZERO);
        assert!(item.reason.contains("工资率为 0"), "{}", item.reason);
        assert!(item.hint.is_some(), "要告诉财务怎么修");
        assert_eq!(outcome.uncollected_hours, dec("4"));
        assert!(!outcome.is_clean());

        // 库里确实没有 0 金额的工归集行
        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_id, good, "写进去的是有费率的那一行");
        assert!(
            rows.iter().all(|row| !row.amount.is_zero()),
            "任何 0 金额的工归集行都不许出现（否则等于静默按 0 计）"
        );

        fx.cleanup(&[object], &[good, no_rate]).await;
    }

    /// ④ 工时行作废（软删）后重跑 ⇒ 旧归集行软删留痕，第三次跑收敛。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn purges_rows_whose_source_no_longer_qualifies() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let keep = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "1",
                rate: "20.000000",
                object,
                deleted: false,
            })
            .await;
        let cancel_me = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "5",
                rate: "6.000000",
                object,
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 2);
        assert_eq!(first.collected_amount, dec("50.0000"));

        // 工时行作废（历史数据里退回 / 软删的形态）
        db().exec_sql(
            "UPDATE biz_cost_labor SET deleted_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&cancel_me)],
        )
        .await
        .expect("软删工时行");

        let second = run(&fx.period).await;
        assert_eq!(second.scanned, 1);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.skipped_existing, 1);
        assert_eq!(second.purged, 1, "来源作废的旧归集行必须清掉");
        assert_eq!(second.purged_amount, dec("30.0000"));
        assert_eq!(second.purged_detail.len(), 1);
        assert_eq!(second.collected_rows, 1);
        assert_eq!(second.collected_amount, dec("20.0000"));

        // 软删留痕：行还在表里，只是 deleted_at 有值
        let all = rows_of_period(&fx.period, false).await;
        assert_eq!(all.len(), 2, "留痕：软删行仍可见");
        let purged = all.iter().find(|row| row.source_id == cancel_me).expect("被清的行");
        assert!(purged.deleted);

        let third = run(&fx.period).await;
        assert_eq!(third.purged, 0, "第三次跑已经收敛");
        assert_eq!(third.inserted, 0);
        assert_eq!(third.skipped_existing, 1);

        fx.cleanup(&[object], &[keep, cancel_me]).await;
    }

    /// ⑤ 工时行改挂别的成本对象 ⇒ 旧行清掉、新行落到新对象。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relinked_cost_object_moves_the_collected_amount() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let first_object = fx.object("OPEN").await;
        let second_object = fx.object("OPEN").await;
        let labor = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "2",
                rate: "15.000000",
                object: first_object,
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 1);
        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows[0].cost_object_id, first_object);

        db().exec_sql(
            "UPDATE biz_cost_labor SET cost_object_id = ?::uuid WHERE id = ?::uuid".to_string(),
            vec![Value::from(&second_object), Value::from(&labor)],
        )
        .await
        .expect("改挂成本对象");

        let second = run(&fx.period).await;
        assert_eq!(second.purged, 1, "旧对象上的归集行要清掉");
        assert_eq!(second.inserted, 1, "新对象上要重新落一行");
        assert_eq!(second.collected_rows, 1);
        assert_eq!(second.collected_amount, dec("30.0000"), "总额不变，只是换了归属");

        let rows = rows_of_period(&fx.period, true).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cost_object_id, second_object);
        assert_eq!(rows[0].source_id, labor);

        fx.cleanup(&[first_object, second_object], &[labor]).await;
    }

    /// ⑥ 空期间：一行不写、不报错；非法期间入口就 400。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn period_without_confirmed_labor_is_a_clean_noop() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 0);
        assert_eq!(outcome.inserted, 0);
        assert_eq!(outcome.collected_rows, 0);
        assert_eq!(outcome.collected_amount, Decimal::ZERO);
        assert!(outcome.uncollected.is_empty());

        // 非法期间必须在入口就 400（不开事务）
        let error = segment::collect_labor("2026-13", None)
            .await
            .expect_err("非法期间必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::PARAM_INVALID);
        fx.cleanup(&[], &[]).await;
    }

    /// ⑦ 两段互不踩：料段写的行不会被工段当成自己的（element / source_type 双重过滤）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn labor_segment_ignores_material_rows_of_the_same_period() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let labor = fx
            .labor(Labor {
                status: "CONFIRMED",
                period: None,
                date: fx.in_date.clone(),
                hours: "1",
                rate: "10.000000",
                object,
                deleted: false,
            })
            .await;

        // 手工塞一条料归集行（同期间、同对象），模拟「料段已经跑过」
        // 料行受 `ck_biz_cost_collect_mat` 约束：`material_id` / `qty` 必须非空（弱引用，给占位 uuid）
        db().exec_sql(
            "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                           source_id, line_no, occur_date, material_id, qty, amount) \
             VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', NULL, 1, ?::text::date, ?::uuid, 1, 99)"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&object),
                Value::Text(fx.period.clone()),
                Value::Text(fx.in_date.clone()),
                Value::from(&Uuid::now_v7()),
            ],
        )
        .await
        .expect("塞一条料归集行");

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 1, "只扫工时台账，不看料行");
        assert_eq!(outcome.inserted, 1);
        assert_eq!(
            outcome.collected_amount,
            dec("10.0000"),
            "工段回查只算 element='LABOR'，料行的 99 不能混进来"
        );
        assert_eq!(rows_of_period(&fx.period, true).await.len(), 1);

        // 清掉那条料行（本用例自己造的）
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_cost_collect WHERE period_code = ? AND element = 'MATERIAL'".to_string(),
                vec![Value::Text(fx.period.clone())],
            )
            .await;
        fx.cleanup(&[object], &[labor]).await;
    }
}
