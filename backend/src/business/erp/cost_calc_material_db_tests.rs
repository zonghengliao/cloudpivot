//! calc 第 1 段（料归集）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_material_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`collects_only_posted_issue_with_cost_object`] | **验收项「只扫已过账 + 挂成本对象」**：7 类不该扫的行（无对象 / 草稿 / 待过账 / 报废 / 上期 / 已软删）一行都不写；金额取 `item.amount`（`qty × 单价` 与之不等也认账上值） |
//! | 2 | [`rerun_does_not_duplicate_and_leaves_rows_untouched`] | **验收项「重复跑不重复写」**：第二次 `inserted=0 / skipped=全部`，行 id 与 `updated_at` 不动 |
//! | 3 | [`purges_rows_whose_source_no_longer_qualifies`] | 来源作废后重跑 ⇒ 旧归集行软删（`deleted_at` 留下），当期总额随之减少，第三次跑收敛为无操作 |
//! | 4 | [`relinked_cost_object_moves_the_collected_amount`] | 领料单改挂别的成本对象 ⇒ 旧行清掉、新行落到新对象（证明幂等键是「来源明细 id」而不是「对象 + 金额」） |
//! | 5 | [`zero_amount_lines_are_collected_and_counted`] | 零成本存量领料照样归集（`amount = 0`），但在汇报里单独计数，不当作「没扫到」 |
//! | 6 | [`period_without_posted_issue_is_a_clean_noop`] | 空期间：一行不写、不报错、金额 0（重算的常态） |
//!
//! # 夹具纪律
//!
//! - 每个夹具独占一个**期间**（年份 = `2000 + tail % 80`、月份 = `1 + tail % 12`），
//!   否则 `collect_rows_of_period` 会把别的用例残留算进来；
//! - 清场**按 id 删**（绝不按前缀扫全库），顺序按 FK：
//!   归集 → 出库明细 → 出库单 → 成本对象 → 物料 / 仓库；
//! - 出库单**直接 SQL 造**（不经 handler）：本卡要验的是「扫描口径」，且要造出
//!   `POSTED + 报废挂对象`、`软删单头` 这类 API 造不出来的历史脏数据形态。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_material as segment;
    use crate::business::erp::cost_collect_dto::{self as dto, MaterialCollectOutcome};
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
        Decimal::from_str(text).expect("金额 / 数量字面量")
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
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

    /// 进程内递增槽位：决定夹具独占的会计期间。
    ///
    /// **不能用时间戳推期间**：期间只有 80×12 = 960 个槽，跨次运行必然撞
    /// （撞了就会把上次残留的领料单一起扫进来，断言当场漂）。槽位从 0 递增，
    /// 同一进程里并发跑的用例各占一个期间；跨次运行的残留由
    /// [`Fixture::reset_period`] 清掉。
    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// 造一张出库单要给的形态。
    struct Doc {
        /// `DRAFT` / `PENDING_POST` / `POSTED` / `CANCELLED`
        status: &'static str,
        /// `ISSUE` / `SCRAP` / `LOSS` / `OTHER`
        out_type: &'static str,
        /// 挂的成本对象（`None` = 不挂）。
        object: Option<Uuid>,
        /// 出库日期（`yyyy-MM-dd`）。
        date: String,
        /// 明细：`(qty, unit_cost, amount)`。
        lines: Vec<(&'static str, &'static str, &'static str)>,
        /// 单头软删（历史脏数据形态）。
        deleted: bool,
    }

    struct Fixture {
        suffix: String,
        period: String,
        in_date: String,
        prev_date: String,
        dept: Uuid,
        dept_name: String,
        product: Uuid,
        warehouse: Uuid,
        unit: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 2000 + (slot % 80) as i32;
            let month = 1 + ((slot / 80) % 12) as u32;
            let (prev_year, prev_month) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
            let period = format!("{year:04}-{month:02}");
            // 跨次运行的残留先清掉（期间槽位会复用），否则候选集里会混进上次的领料单
            Self::reset_period_for(&period).await;

            let dept = crate::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");

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
                    Value::Text(format!("B26-MAT-{suffix}")),
                    Value::Text(format!("料归集夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");

            let warehouse = Uuid::now_v7();
            let code = format!("B26W{}", &suffix[suffix.len().saturating_sub(8)..]);
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("料归集夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");

            Self {
                suffix,
                period,
                in_date: format!("{year:04}-{month:02}-10"),
                prev_date: format!("{prev_year:04}-{prev_month:02}-10"),
                dept,
                dept_name,
                product,
                warehouse,
                unit,
            }
        }

        /// 造成本对象（`CLOSED` 必须同时给 `closed_at`）。
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
                    Value::Text(format!("CB26-{}-{}", &id.simple().to_string()[20..], self.suffix)),
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

        /// 造一张出库单（返回 `(单头 id, 明细 id 列表)`）。
        async fn stock_out(&self, doc: Doc) -> (Uuid, Vec<Uuid>) {
            let header = Uuid::now_v7();
            let out_no = format!("B26OUT-{}", &header.simple().to_string()[20..]);
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, reason, cost_object_id, \
                                            created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?::uuid, ?::text::date, ?, 0, 0, 'B2-6 夹具', \
                         NULLIF(?::text,'')::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&header),
                    Value::Text(out_no.clone()),
                    Value::Text(doc.out_type.to_string()),
                    Value::from(&self.warehouse),
                    Value::Text(doc.date.clone()),
                    Value::Text(doc.status.to_string()),
                    Value::Text(dto::opt_uuid_text(doc.object)),
                ],
            )
            .await
            .expect("造出库单夹具");

            if doc.deleted {
                db().exec_sql(
                    "UPDATE biz_stock_out SET deleted_at = now() WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&header)],
                )
                .await
                .expect("软删出库单夹具");
            }

            let mut items = Vec::new();
            for (index, (qty, unit_cost, amount)) in doc.lines.iter().enumerate() {
                let item = Uuid::now_v7();
                db().exec_sql(
                    "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, \
                                                     qty, unit_cost, amount, remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::uuid, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, 'B2-6 夹具', now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&item),
                        Value::from(&header),
                        Value::I32((index + 1) as i32),
                        Value::from(&self.product),
                        Value::from(&self.unit),
                        Value::Text(qty.to_string()),
                        Value::Text(unit_cost.to_string()),
                        Value::Text(amount.to_string()),
                    ],
                )
                .await
                .expect("造出库明细夹具");
                items.push(item);
            }
            (header, items)
        }

        /// 清掉本期间里**本卡夹具前缀**的残留（跨次运行的兜底）。
        ///
        /// 只删带本卡前缀的行（`B26OUT-` 单号 / `CB26-` 对象号），绝不按期间盲删：
        /// 期间槽位跨次运行会复用，上一次失败留下的领料单会被重新扫进来，断言当场漂。
        async fn reset_period_for(period: &str) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect WHERE period_code = ? AND source_no LIKE 'B26OUT-%'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_stock_out \
                      WHERE out_no LIKE 'B26OUT-%' \
                        AND out_date >= (? || '-01')::date \
                        AND out_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object \
                      WHERE object_no LIKE 'CB26-%' \
                        AND start_date >= (? || '-01')::date \
                        AND start_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
        }

        /// 清场：按 id 删（FK 顺序：归集 → 出库明细 → 出库单 → 对象 → 物料 / 仓库）。
        async fn cleanup(&self, objects: &[Uuid], headers: &[Uuid]) {
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !objects.is_empty() {
                let objects_list = list(objects);
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
            }
            if !headers.is_empty() {
                let headers_list = list(headers);
                let _ = db()
                    .exec_sql(
                        format!(
                            "DELETE FROM biz_cost_collect WHERE source_id IN \
                             (SELECT id FROM biz_stock_out_item WHERE stock_out_id IN ({headers_list}))"
                        ),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_stock_out_item WHERE stock_out_id IN ({headers_list})"),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_stock_out WHERE id IN ({headers_list})"),
                        vec![],
                    )
                    .await;
            }
            if !objects.is_empty() {
                let objects_list = list(objects);
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_object WHERE id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    format!("DELETE FROM biz_material WHERE id = '{}'::uuid", self.product),
                    vec![],
                )
                .await;
            let _ = db()
                .exec_sql(
                    format!("DELETE FROM biz_warehouse WHERE id = '{}'::uuid", self.warehouse),
                    vec![],
                )
                .await;
        }
    }

    /// 一条归集行（断言用）。
    #[derive(Debug)]
    struct CollectRow {
        id: Uuid,
        period_code: String,
        element: String,
        source_type: String,
        source_id: Uuid,
        source_no: String,
        line_no: i32,
        occur_date: NaiveDate,
        cost_object_id: Uuid,
        material_id: Uuid,
        qty: Decimal,
        unit_price: Decimal,
        amount: Decimal,
        dept_id: Option<Uuid>,
        dept_name: Option<String>,
        deleted: bool,
    }

    /// 当期**未删**的料归集行（按来源单号 + 行号排，断言顺序确定）。
    async fn live_rows(period: &str) -> Vec<CollectRow> {
        let sql = "SELECT id, period_code, element, source_type, source_id, source_no, line_no, \
                          occur_date, cost_object_id, material_id, qty, unit_price, amount, \
                          dept_id, dept_name, (deleted_at IS NOT NULL) AS deleted \
                     FROM biz_cost_collect \
                    WHERE period_code = ? AND element = 'MATERIAL' AND source_type = 'STOCK_OUT' \
                      AND deleted_at IS NULL \
                    ORDER BY source_no, line_no, id";
        db().query_sql(sql.to_string(), vec![Value::Text(period.to_string())])
            .await
            .expect("查归集行")
            .iter()
            .map(|row| CollectRow {
                id: row.get_typed("id").expect("id"),
                period_code: row.get_typed("period_code").expect("period"),
                element: row.get_typed("element").expect("element"),
                source_type: row.get_typed("source_type").expect("source_type"),
                source_id: row.get_typed("source_id").expect("source_id"),
                source_no: row.get_typed("source_no").expect("source_no"),
                line_no: row.get_typed("line_no").expect("line_no"),
                occur_date: row.get_typed("occur_date").expect("occur_date"),
                cost_object_id: row.get_typed("cost_object_id").expect("cost_object_id"),
                material_id: row.get_typed("material_id").expect("material_id"),
                qty: row.get_typed("qty").expect("qty"),
                unit_price: row.get_typed("unit_price").expect("unit_price"),
                amount: row.get_typed("amount").expect("amount"),
                dept_id: row.get_typed("dept_id").expect("dept_id"),
                dept_name: row.get_typed("dept_name").expect("dept_name"),
                deleted: row.get_typed("deleted").expect("deleted"),
            })
            .collect()
    }

    /// 全部归集行（含已软删，验「软删留痕」用）。
    async fn all_rows(period: &str) -> Vec<CollectRow> {
        let sql = "SELECT id, period_code, element, source_type, source_id, source_no, line_no, \
                          occur_date, cost_object_id, material_id, qty, unit_price, amount, \
                          dept_id, dept_name, (deleted_at IS NOT NULL) AS deleted \
                     FROM biz_cost_collect \
                    WHERE period_code = ? AND element = 'MATERIAL' AND source_type = 'STOCK_OUT' \
                    ORDER BY source_no, line_no, id";
        db().query_sql(sql.to_string(), vec![Value::Text(period.to_string())])
            .await
            .expect("查全部归集行")
            .iter()
            .map(|row| CollectRow {
                id: row.get_typed("id").expect("id"),
                period_code: row.get_typed("period_code").expect("period"),
                element: row.get_typed("element").expect("element"),
                source_type: row.get_typed("source_type").expect("source_type"),
                source_id: row.get_typed("source_id").expect("source_id"),
                source_no: row.get_typed("source_no").expect("source_no"),
                line_no: row.get_typed("line_no").expect("line_no"),
                occur_date: row.get_typed("occur_date").expect("occur_date"),
                cost_object_id: row.get_typed("cost_object_id").expect("cost_object_id"),
                material_id: row.get_typed("material_id").expect("material_id"),
                qty: row.get_typed("qty").expect("qty"),
                unit_price: row.get_typed("unit_price").expect("unit_price"),
                amount: row.get_typed("amount").expect("amount"),
                dept_id: row.get_typed("dept_id").expect("dept_id"),
                dept_name: row.get_typed("dept_name").expect("dept_name"),
                deleted: row.get_typed("deleted").expect("deleted"),
            })
            .collect()
    }

    /// 跑一段（收窄失败信息）。
    async fn run(period: &str) -> MaterialCollectOutcome {
        segment::collect_material(period, None)
            .await
            .unwrap_or_else(|error| panic!("料归集失败：{} / {}", error.status, error.msg))
    }

    // ==================== 用例 ====================

    /// ① 验收项「只扫已过账 + 挂成本对象的领料」+「金额取账上真实出库成本」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collects_only_posted_issue_with_cost_object() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let other = fx.object("OPEN").await;

        // 该扫的两行：同一张已过账领用单的两条明细（一单多行 ⇒ 两行归集，各自独立幂等）
        // 第一行故意让 amount ≠ qty × unit_cost（30.0001 vs 3 × 10 = 30）：
        // 验收项要求「金额取领料单实际出库成本」，即账上真实扣减，不重算。
        let (good_header, good_items) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("3", "10.000000", "30.0001"), ("2", "5.000000", "10.0000")],
                deleted: false,
            })
            .await;

        // 不该扫的六类
        let (no_object, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: None,
                date: fx.in_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: false,
            })
            .await;
        let (draft, _) = fx
            .stock_out(Doc {
                status: "DRAFT",
                out_type: "ISSUE",
                object: Some(other),
                date: fx.in_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: false,
            })
            .await;
        let (pending, _) = fx
            .stock_out(Doc {
                status: "PENDING_POST",
                out_type: "ISSUE",
                object: Some(other),
                date: fx.in_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: false,
            })
            .await;
        // 报废/盘亏必进 1901（B1-10 禁挂对象），这里直接 SQL 造出「挂着对象的报废单」脏数据，
        // 验扫描口径不会把它们当料成本（out_type 必须 = ISSUE）。
        let (scrap, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "SCRAP",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: false,
            })
            .await;
        let (other_period, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.prev_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: false,
            })
            .await;
        let (soft_deleted, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("1", "10.000000", "10.0000")],
                deleted: true,
            })
            .await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 2, "只该扫到那两条领用明细");
        assert_eq!(outcome.inserted, 2);
        assert_eq!(outcome.skipped_existing, 0);
        assert_eq!(outcome.purged, 0);
        assert_eq!(outcome.collected_rows, 2);
        assert_eq!(outcome.collected_amount, dec("40.0001"));
        assert_eq!(outcome.inserted_amount, dec("40.0001"));
        assert_eq!(outcome.zero_amount, 0);

        let rows = live_rows(&fx.period).await;
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.period_code, fx.period);
            assert_eq!(row.element, "MATERIAL");
            assert_eq!(row.source_type, "STOCK_OUT");
            assert_eq!(row.line_no, 1, "料行来源行号恒为 1（设计 §4.3 幂等键）");
            assert!(good_items.contains(&row.source_id), "来源必须是明细行 id");
            assert!(row.source_no.starts_with("B26OUT-"));
            assert_eq!(row.occur_date, day(&fx.in_date));
            assert_eq!(row.cost_object_id, object);
            assert_eq!(row.material_id, fx.product);
            assert_eq!(row.dept_id, Some(fx.dept), "车间取成本对象上的车间");
            assert_eq!(row.dept_name.as_deref(), Some(&*fx.dept_name));
            assert!(!row.deleted);
        }
        // 金额 / 数量 / 单价逐行对账上值
        let first = rows.iter().find(|row| row.source_id == good_items[0]).expect("第一行");
        assert_eq!(first.qty, dec("3"));
        assert_eq!(first.unit_price, dec("10.000000"));
        assert_eq!(first.amount, dec("30.0001"), "取 item.amount，不是 3 × 10");
        assert_ne!(first.amount, first.qty * first.unit_price, "刻意不等，证明没重算");
        let second = rows.iter().find(|row| row.source_id == good_items[1]).expect("第二行");
        assert_eq!(second.qty, dec("2"));
        assert_eq!(second.amount, dec("10.0000"));

        let all = [
            good_header,
            no_object,
            draft,
            pending,
            scrap,
            other_period,
            soft_deleted,
        ];
        fx.cleanup(&[object, other], &all).await;
        assert!(all_rows(&fx.period).await.is_empty(), "清场后本期间零残留");
    }

    /// ② 验收项「重复跑不重复写」：第二次是无操作，行 id / 金额 / `updated_at` 一律不动。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rerun_does_not_duplicate_and_leaves_rows_untouched() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let (header, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("3", "10.000000", "30.0000"), ("4", "2.500000", "10.0000")],
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 2);
        let before = live_rows(&fx.period).await;
        let stamps: Vec<(Uuid, DateTime<Utc>)> = {
            let listed = db()
                .query_sql(
                    "SELECT id, updated_at FROM biz_cost_collect \
                      WHERE period_code = ? AND deleted_at IS NULL ORDER BY id"
                        .to_string(),
                    vec![Value::Text(fx.period.to_string())],
                )
                .await
                .expect("取更新时间");
            listed
                .iter()
                .map(|row| {
                    (
                        row.get_typed("id").expect("id"),
                        row.get_typed("updated_at").expect("updated_at"),
                    )
                })
                .collect()
        };

        let second = run(&fx.period).await;
        assert_eq!(second.scanned, 2);
        assert_eq!(second.inserted, 0, "重复跑不许再写");
        assert_eq!(second.skipped_existing, 2);
        assert_eq!(second.purged, 0);
        assert_eq!(second.collected_rows, 2, "行数没有翻倍");
        assert_eq!(second.collected_amount, dec("40.0000"));

        let after = live_rows(&fx.period).await;
        assert_eq!(after.len(), before.len());
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(old.id, new.id, "同一来源行必须还是同一行（幂等键撞上就跳过）");
            assert_eq!(old.amount, new.amount);
        }
        let stamps_after: Vec<(Uuid, DateTime<Utc>)> = {
            let listed = db()
                .query_sql(
                    "SELECT id, updated_at FROM biz_cost_collect \
                      WHERE period_code = ? AND deleted_at IS NULL ORDER BY id"
                        .to_string(),
                    vec![Value::Text(fx.period.to_string())],
                )
                .await
                .expect("取更新时间");
            listed
                .iter()
                .map(|row| {
                    (
                        row.get_typed("id").expect("id"),
                        row.get_typed("updated_at").expect("updated_at"),
                    )
                })
                .collect()
        };
        assert_eq!(stamps, stamps_after, "重复跑不许刷 updated_at");

        fx.cleanup(&[object], &[header]).await;
    }

    /// ③ 来源作废 ⇒ 重跑把旧归集行软删（留痕），第三次跑收敛为无操作。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn purges_rows_whose_source_no_longer_qualifies() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let (keep, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("1", "20.000000", "20.0000")],
                deleted: false,
            })
            .await;
        let (cancel_me, cancel_items) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("5", "6.000000", "30.0000")],
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 2);
        assert_eq!(first.collected_amount, dec("50.0000"));

        // 领料单作废（真实路径会同时出红字凭证；这里只关心归集口径）
        db().exec_sql(
            "UPDATE biz_stock_out SET status = 'CANCELLED', cancel_reason = 'B2-6 夹具作废', \
                    cancelled_at = now() WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&cancel_me)],
        )
        .await
        .expect("作废领料单");

        let second = run(&fx.period).await;
        assert_eq!(second.scanned, 1);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.skipped_existing, 1);
        assert_eq!(second.purged, 1, "来源作废的旧归集行必须清掉");
        assert_eq!(second.purged_amount, dec("30.0000"));
        assert_eq!(second.purged_detail.len(), 1);
        assert!(second.purged_detail[0].source_no.is_some());
        assert_eq!(second.collected_rows, 1);
        assert_eq!(second.collected_amount, dec("20.0000"));

        // 软删留痕：行还在表里，只是 deleted_at 有值（唯一键随之释放）
        let all = all_rows(&fx.period).await;
        assert_eq!(all.len(), 2, "留痕：软删行仍可见");
        let purged = all
            .iter()
            .find(|row| row.source_id == cancel_items[0])
            .expect("被清的行");
        assert!(purged.deleted, "旧归集行应被软删而不是物理删");

        let third = run(&fx.period).await;
        assert_eq!(third.purged, 0, "第三次跑已经收敛");
        assert_eq!(third.inserted, 0);
        assert_eq!(third.skipped_existing, 1);

        fx.cleanup(&[object], &[keep, cancel_me]).await;
    }

    /// ④ 领料单改挂别的成本对象 ⇒ 旧行清掉、新行落到新对象（幂等键是来源明细 id）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relinked_cost_object_moves_the_collected_amount() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let first_object = fx.object("OPEN").await;
        let second_object = fx.object("OPEN").await;
        let (header, items) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(first_object),
                date: fx.in_date.clone(),
                lines: vec![("2", "15.000000", "30.0000")],
                deleted: false,
            })
            .await;

        let first = run(&fx.period).await;
        assert_eq!(first.inserted, 1);
        let rows = live_rows(&fx.period).await;
        assert_eq!(rows[0].cost_object_id, first_object);

        // 改挂到另一个成本对象（B1-10 建单 / 改单允许挂对象；这里是同一个来源明细换归属）
        db().exec_sql(
            "UPDATE biz_stock_out SET cost_object_id = ?::uuid WHERE id = ?::uuid".to_string(),
            vec![Value::from(&second_object), Value::from(&header)],
        )
        .await
        .expect("改挂成本对象");

        let second = run(&fx.period).await;
        assert_eq!(second.purged, 1, "旧对象上的归集行要清掉");
        assert_eq!(second.inserted, 1, "新对象上要重新落一行");
        assert_eq!(second.collected_rows, 1);
        assert_eq!(second.collected_amount, dec("30.0000"), "总额不变，只是换了归属");

        let rows = live_rows(&fx.period).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cost_object_id, second_object);
        assert_eq!(rows[0].source_id, items[0]);

        fx.cleanup(&[first_object, second_object], &[header]).await;
    }

    /// ⑤ 零成本存量领料照样归集（`amount = 0`），但汇报里单独计数。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_amount_lines_are_collected_and_counted() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object("OPEN").await;
        let (header, _) = fx
            .stock_out(Doc {
                status: "POSTED",
                out_type: "ISSUE",
                object: Some(object),
                date: fx.in_date.clone(),
                lines: vec![("2", "0.000000", "0.0000"), ("1", "9.000000", "9.0000")],
                deleted: false,
            })
            .await;

        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 2);
        assert_eq!(outcome.inserted, 2);
        assert_eq!(
            outcome.zero_amount, 1,
            "零金额行要单独数出来（否则『成本为什么是 0』没人能答）"
        );
        assert_eq!(outcome.collected_amount, dec("9.0000"));
        let rows = live_rows(&fx.period).await;
        assert_eq!(rows.iter().filter(|row| row.amount.is_zero()).count(), 1);

        fx.cleanup(&[object], &[header]).await;
    }

    /// ⑥ 空期间：一行不写、不报错（重算的常态）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn period_without_posted_issue_is_a_clean_noop() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let outcome = run(&fx.period).await;
        assert_eq!(outcome.scanned, 0);
        assert_eq!(outcome.inserted, 0);
        assert_eq!(outcome.collected_rows, 0);
        assert_eq!(outcome.collected_amount, Decimal::ZERO);

        // 非法期间必须在入口就 400（不开事务）
        let error = segment::collect_material("2026-13", None)
            .await
            .expect_err("非法期间必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::PARAM_INVALID);
        fx.cleanup(&[], &[]).await;
    }
}
