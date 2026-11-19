//! 标准成本 × 实际成本 **逐物料对比** 的真库用例（`ERP_DB_URL` 开关）—— M5-11。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m511_cmp \
//!   cargo test --lib cost_standard_compare -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-11「标准成本：与实际成本对比 · 逐物料对比」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`comparable_row_matches_arithmetic_and_totals`] | 两侧都在 ⇒ 单位 / 总额双口径逐项核数（标准 20 → 实际 23、差异 300、差异率 15%），要素拆分相加 = 总差异，且**整表合计 = 逐行算数相加**（SQL 与 Rust 两处口径对拍） |
//! | 2 | [`missing_sides_are_classified_with_actionable_hints`] | 四种缺一侧的形态各自归类：有实际没标准 / 未发布不算标准 / 有标准无计算单 / 有标准但完工 0（全在制）；hint 必须给具体动作 |
//! | 3 | [`unpublished_zero_status_and_switch_versions`] | `DRAFT` 卡不参与、`status=0` 不参与、期间内换版 ⇒ 取期末那版 + `switch_in_period=true` + hint 说明基准 |
//! | 4 | [`filters_paging_and_keyword_narrow_the_set`] | 四档筛选（all / comparable / no_standard / no_actual）各自命中对的行；关键字收窄；`page_size=1` 翻页 total 恒定、第二页换行、顺序稳定 |
//! | 5 | [`period_without_batch_lists_standard_side_only`] | 该期间没有成本核算批次 ⇒ `cost_ready=false`、实际侧全空（`NO_ACTUAL`）、`note` 指向「先做成本计算」 |
//! | 6 | [`element_split_is_reported_per_element`] | 三要素差异逐行给出：材料超支、人工节约、费用持平（方向 / 差异率 / 方向文本都要对） |
//!
//! # 夹具纪律
//!
//! · 期间用 `test_seed::year_seq(2800, 60)` 造的**合成年度**（每月一个夹具），不碰真实年度；
//! · 物料走 `test_seed::ensure_material_kind`（编码带进程内唯一后缀）；
//! · 标准成本卡按 SQL 直插（`publish_state='PUBLISHED'` / `status=1` / 生效区间覆盖期末）——
//!   本卡测的是**对比取数**，发布审批链路由 M5-10 的用例覆盖；
//! · 成本对象 / 批次 / 计算单按 SQL 直插，且**必须满足库层守恒**
//!   （`ck_biz_cost_item_cons`：完工 + 期末在制 = 期初 + 本期投入）；
//! · 每个用例自己清场（`Fixture::cleanup`），不按前缀扫全库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_standard_compare_dto::{
        self as dto, DIRECTION_EVEN, DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE, STATE_BOTH, STATE_NO_ACTUAL,
        STATE_NO_STANDARD, StateFilter,
    };
    use crate::business::erp::cost::cost_standard_compare_handler as handler;
    use crate::business::erp::cost::cost_standard_compare_model::{self as model, CompareFilter};
    use nik::prelude::chrono::{Datelike, Duration, NaiveDate};
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr as _;
    use tokio::sync::OnceCell;

    /// 本夹具造出来的成本对象号前缀（清场与学生成对收窄用）。
    const OBJECT_PREFIX: &str = "CB-M511-";

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
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 金额字符串 → `Decimal`（出参一律字符串，避免标度假红）。
    fn amount(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("金额字符串无法解析：{text}"))
    }

    /// 日期（`yyyy-MM-dd`）。
    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 夹具：一个合成年度里的一个会计期间。
    struct Fixture {
        period: String,
        start: NaiveDate,
        end: NaiveDate,
        dept: Uuid,
        dept_name: String,
        warehouse: Uuid,
        materials: Vec<Uuid>,
        objects: Vec<Uuid>,
    }

    impl Fixture {
        /// 取一个独占年度（`year_seq` 每次调用递增，同一次运行内不会撞期间）。
        async fn build() -> Self {
            let year = crate::test_seed::year_seq(2800, 60);
            let period = crate::test_seed::period_code(year, 6);
            let start = date(&format!("{period}-01"));
            let month_end = match start.month() {
                12 => NaiveDate::from_ymd_opt(year + 1, 1, 1),
                month => NaiveDate::from_ymd_opt(year, month + 1, 1),
            }
            .expect("下月 1 号")
            .pred_opt()
            .expect("月末");
            Self::reset(&period).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let suffix = crate::test_seed::uniq_suffix();
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M511W{}", crate::test_seed::tail_code(&suffix, 8)),
                &format!("M5-11 夹具仓 {suffix}"),
            )
            .await;

            Self {
                period,
                start,
                end: month_end,
                dept,
                dept_name,
                warehouse,
                materials: Vec::new(),
                objects: Vec::new(),
            }
        }

        /// 清场：按期间（计算单随批次级联）+ 对象 + 物料（标准卡随物料）删干净。
        async fn reset(period: &str) {
            let objects = format!(
                "(SELECT id FROM biz_cost_object WHERE object_no LIKE '{OBJECT_PREFIX}%' \
                   AND to_char(start_date, 'YYYY-MM') = '{period}')"
            );
            for sql in [
                format!("DELETE FROM biz_cost_period WHERE period_code = '{period}'"),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN {objects}"),
                format!("DELETE FROM biz_cost_object WHERE id IN {objects}"),
                "DELETE FROM biz_cost_standard WHERE material_id IN \
                   (SELECT id FROM biz_material WHERE material_code LIKE 'M511-%')"
                    .to_string(),
            ] {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }

        /// 造物料（编码带夹具尾部，关键字收窄靠它）。
        async fn material(&mut self, tag: &str) -> Uuid {
            let suffix = crate::test_seed::uniq_suffix();
            let code = format!("M511-{tag}-{}", crate::test_seed::last_chars(&suffix, 8));
            let id =
                crate::test_seed::ensure_material_kind(&code, &format!("M5-11 夹具 {tag} {suffix}"), "FINISHED").await;
            self.materials.push(id);
            id
        }

        /// 直插一版标准成本卡（`state` = `PUBLISHED` / `DRAFT`，`status` 1 启用 / 0 失效）。
        #[allow(clippy::too_many_arguments)]
        async fn standard(
            &self,
            material: Uuid,
            version_no: i32,
            from: NaiveDate,
            to: Option<NaiveDate>,
            units: (&str, &str, &str),
            publish_state: &str,
            status: i16,
        ) -> Uuid {
            let (material_cost, labor_cost, overhead_cost) = units;
            let material_cost = amount(material_cost);
            let labor_cost = amount(labor_cost);
            let overhead_cost = amount(overhead_cost);
            let total = material_cost + labor_cost + overhead_cost;
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_standard \
                     (id, material_id, version_no, effective_from, effective_to, \
                      std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost, \
                      std_labor_hours, std_labor_rate, status, publish_state, published_at, \
                      remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?::text::date, NULLIF(?::text, '')::date, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?, ?, NULLIF(?::text, '')::timestamptz, \
                         'M5-11 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&material),
                    Value::I32(version_no),
                    Value::Text(from.to_string()),
                    Value::Text(to.map(|day| day.to_string()).unwrap_or_default()),
                    Value::Text(material_cost.to_string()),
                    Value::Text(labor_cost.to_string()),
                    Value::Text(overhead_cost.to_string()),
                    Value::Text(total.to_string()),
                    // 工时 × 费率 = 人工成本（库层 ck_biz_cost_std_labor 要求恒等）
                    Value::Text("1".to_string()),
                    Value::Text(labor_cost.to_string()),
                    Value::I16(status),
                    Value::Text(publish_state.to_string()),
                    Value::Text(if publish_state == "PUBLISHED" {
                        "2026-01-01T00:00:00Z".to_string()
                    } else {
                        String::new()
                    }),
                ],
            )
            .await
            .expect("直插标准成本卡夹具");
            id
        }

        /// 造成本对象（`completed_qty` 只为抬头好看，对比取的是计算单）。
        async fn object(&mut self, name: &str, product: Uuid) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_object \
                     (id, object_no, object_type, product_id, target_warehouse_id, plan_qty, \
                      completed_qty, wip_qty, workshop_dept_id, workshop_dept_name, start_date, \
                      status, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?::uuid, 1000, 0, 0, ?::uuid, ?, \
                         ?::text::date, 'OPEN', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("{OBJECT_PREFIX}{name}")),
                    Value::from(&product),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(self.start.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.push(id);
            id
        }

        /// 造当期活跃批次（合计列全 0：`total_cost = 0 = 0 + 0` 同时满足两条 CHECK）。
        ///
        /// 状态用 `DRAFT`：`SUBMITTED` 及以上被 `ck_biz_cost_period_appr` 要求带审批实例，
        /// 而本卡只需要「当期有活跃批次」（`active_batch` 只排除 `REVERSED` / `CANCELLED`）。
        async fn batch(&self) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, remark, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', 'M5-11 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period.clone()),
                    Value::Text(format!("CS-{}-M511", self.period.replace('-', ""))),
                ],
            )
            .await
            .expect("造 成本核算批次夹具");
            id
        }

        /// 造计算单一行（**必须满足库层守恒**：完工 + 期末在制 = 期初 + 本期投入）。
        #[allow(clippy::too_many_arguments)]
        async fn item(
            &self,
            batch: Uuid,
            object: Uuid,
            product: Uuid,
            qty: &str,
            amounts: (&str, &str, &str),
            unit: (&str, &str, &str),
            finished: &str,
            wip_end_qty: &str,
            wip_end_cost: &str,
        ) -> Uuid {
            let (mat_cur, labor_cur, oh_cur) = amounts;
            let (unit_material, unit_labor, unit_overhead) = unit;
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period_item \
                     (id, period_id, cost_object_id, product_id, completed_qty, wip_end_qty, \
                      completion_pct, equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur, \
                      unit_material, unit_labor, unit_overhead, finished_cost, wip_end_cost, \
                      unit_finished_cost, status, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                         100, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         'CALC', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&batch),
                    Value::from(&object),
                    Value::from(&product),
                    Value::Text(qty.to_string()),
                    Value::Text(wip_end_qty.to_string()),
                    Value::Text((amount(qty) + amount(wip_end_qty)).to_string()),
                    Value::Text((amount(qty) + amount(wip_end_qty)).to_string()),
                    Value::Text(mat_cur.to_string()),
                    Value::Text(labor_cur.to_string()),
                    Value::Text(oh_cur.to_string()),
                    Value::Text(unit_material.to_string()),
                    Value::Text(unit_labor.to_string()),
                    Value::Text(unit_overhead.to_string()),
                    Value::Text(finished.to_string()),
                    Value::Text(wip_end_cost.to_string()),
                    Value::Text(if amount(qty).is_zero() {
                        "0".to_string()
                    } else {
                        (amount(finished) / amount(qty)).to_string()
                    }),
                ],
            )
            .await
            .expect("造 计算单夹具（守恒不成立会在这里炸）");
            id
        }

        /// 报表筛选（期间 = 本夹具期间；页大小可调）。
        fn filter(&self, state: StateFilter, keyword: Option<&str>, page: u64, page_size: u64) -> CompareFilter {
            CompareFilter {
                period: (self.start, self.end),
                keyword: keyword.map(str::to_string),
                state,
                page,
                page_size,
            }
        }

        /// 跑一次报表。
        async fn report(
            &self,
            state: StateFilter,
            keyword: Option<&str>,
            page: u64,
            page_size: u64,
        ) -> dto::CompareRes {
            handler::compare(&self.period, &self.filter(state, keyword, page, page_size))
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "对比报表失败：{}（{}）内部原因：{:?}",
                        error.msg, error.code, error.internal_detail
                    )
                })
        }

        /// 清场（用例尾声调用）。
        async fn cleanup(&self) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE period_code = ?".to_string(),
                    vec![Value::Text(self.period.clone())],
                )
                .await;
            for object in &self.objects {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_period_item WHERE cost_object_id = ?::uuid".to_string(),
                        vec![Value::from(object)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_object WHERE id = ?::uuid".to_string(),
                        vec![Value::from(object)],
                    )
                    .await;
            }
            for material in &self.materials {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_standard WHERE material_id = ?::uuid".to_string(),
                        vec![Value::from(material)],
                    )
                    .await;
            }
        }
    }

    /// 在报表里按物料 id 找行（找不到就是报表漏了它）。
    fn row_of(report: &dto::CompareRes, material: Uuid) -> &dto::CompareRow {
        let id = material.to_string();
        report.list.iter().find(|row| row.material_id == id).unwrap_or_else(|| {
            panic!(
                "报表里没有物料 {id}；本次共 {} 行：{:?}",
                report.list.len(),
                report
                    .list
                    .iter()
                    .map(|row| row.material_code.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
            )
        })
    }

    // ==================== 用例 ====================

    /// ① 两侧都在：单位 / 总额双口径逐项核数，且合计 = 逐行算数相加（SQL 与 Rust 对拍）。
    #[tokio::test]
    async fn comparable_row_matches_arithmetic_and_totals() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let material = fx.material("BOTH").await;
        fx.standard(material, 1, fx.start, None, ("10", "4", "6"), "PUBLISHED", 1)
            .await;
        let batch = fx.batch().await;
        let object = fx.object("BOTH", material).await;
        // 完工 100 件；实际 1100 / 400 / 800 = 2300（守恒：2300 + 0 = 0 + 1100 + 400 + 800）
        fx.item(
            batch,
            object,
            material,
            "100",
            ("1100", "400", "800"),
            ("11", "4", "8"),
            "2300",
            "0",
            "0",
        )
        .await;

        let report = fx.report(StateFilter::All, None, 1, 200).await;
        assert!(report.cost_ready, "有批次 ⇒ 实际侧就绪");
        assert_eq!(report.batch.as_ref().map(|b| b.status.as_str()), Some("DRAFT"));

        let row = row_of(&report, material);
        assert_eq!(row.state, STATE_BOTH);
        let standard = row.standard.as_ref().expect("标准侧");
        assert_eq!(standard.std_total_cost, "20.000000");
        assert_eq!(standard.effective_from, fx.start.to_string());
        let actual = row.actual.as_ref().expect("实际侧");
        assert_eq!(actual.completed_qty, "100.0000");
        assert_eq!(actual.unit_total, "23.000000");
        assert_eq!(actual.amount_total, "2300.0000");
        assert_eq!(actual.book_amount, "2300.0000");

        let variance = row.variance.as_ref().expect("可比行必须有差异块");
        assert_eq!(variance.std_amount, "2000.0000", "20 × 100");
        assert_eq!(variance.act_amount, "2300.0000");
        assert_eq!(variance.amount, "300.0000");
        assert_eq!(variance.rate.as_deref(), Some("0.150000"), "300 / 2000 = 15%");
        assert_eq!(variance.direction, DIRECTION_UNFAVORABLE);
        assert_eq!(variance.unit_amount, "3.000000");
        assert_eq!(variance.rounding_diff, "0.0000");

        // 合计：本期间只有这一个物料（期间是本夹具独占的合成年度）
        assert_eq!(report.total, 1);
        assert_eq!(report.totals.rows, 1);
        assert_eq!(report.totals.comparable_rows, 1);
        assert_eq!(report.totals.std_amount, "2000.0000");
        assert_eq!(report.totals.act_amount, "2300.0000");
        assert_eq!(report.totals.amount, "300.0000");
        assert_eq!(report.totals.completed_qty, "100.0000");
        assert_eq!(report.totals.completed_qty, variance.completed_qty);

        // ★ 对拍：SQL 合计 == 逐行算数相加（`totals_of`）—— 两处口径不许漂
        let page_calcs: Vec<dto::RowCalc> = fx_cores(&report, &fx).await.iter().map(dto::calc_of).collect();
        let rust = dto::totals_of(&page_calcs);
        assert_eq!(rust.std_total(), amount(&report.totals.std_amount));
        assert_eq!(rust.act_total(), amount(&report.totals.act_amount));
        assert_eq!(rust.amount(), amount(&report.totals.amount));
        assert_eq!(rust.completed_qty, amount(&report.totals.completed_qty));
        assert_eq!(rust.book_amount, amount(&report.totals.book_amount));
        assert_eq!(rust.comparable_rows, report.totals.comparable_rows);
        assert_eq!(rust.no_standard_amount, amount(&report.totals.no_standard_amount));

        fx.cleanup().await;
    }

    /// 报表里每行对应的「取数 + 算数」（对拍用：走模型层同一套 SQL）。
    async fn fx_cores(report: &dto::CompareRes, fx: &Fixture) -> Vec<dto::CompareCore> {
        let filter = fx.filter(StateFilter::All, None, 1, 200);
        let batch_id = report
            .batch
            .as_ref()
            .map(|batch| Uuid::from_str(&batch.id).expect("批次 id"));
        let (rows, _) = model::page(&filter, batch_id).await.expect("模型层取数");
        rows
    }

    /// ② 四种缺一侧的形态各自归类，hint 必须给具体动作。
    #[tokio::test]
    async fn missing_sides_are_classified_with_actionable_hints() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let no_standard = fx.material("NOSTD").await;
        let unpublished = fx.material("DRAFT").await;
        let no_actual = fx.material("NOACT").await;
        let wip_only = fx.material("WIP").await;

        // 只有实际：没建卡
        // 有实际 + 卡是草稿：未发布不算标准（M5-10 的取数闸）
        fx.standard(unpublished, 1, fx.start, None, ("10", "4", "6"), "DRAFT", 1)
            .await;
        // 只有标准：本期没计算单
        fx.standard(no_actual, 1, fx.start, None, ("5", "1", "2"), "PUBLISHED", 1)
            .await;
        // 有标准 + 计算单但完工 0（全在制）
        fx.standard(wip_only, 1, fx.start, None, ("10", "2", "3"), "PUBLISHED", 1)
            .await;

        let batch = fx.batch().await;
        let object = fx.object("NOSTD", no_standard).await;
        fx.item(
            batch,
            object,
            no_standard,
            "10",
            ("100", "40", "60"),
            ("10", "4", "6"),
            "200",
            "0",
            "0",
        )
        .await;
        let object = fx.object("DRAFT", unpublished).await;
        fx.item(
            batch,
            object,
            unpublished,
            "5",
            ("50", "20", "30"),
            ("10", "4", "6"),
            "100",
            "0",
            "0",
        )
        .await;
        let object = fx.object("WIP", wip_only).await;
        // 完工 0、期末在制 50 件：0 + 1000 = 0 + 500 + 200 + 300
        fx.item(
            batch,
            object,
            wip_only,
            "0",
            ("500", "200", "300"),
            ("10", "8", "12"),
            "0",
            "50",
            "1000",
        )
        .await;

        let report = fx.report(StateFilter::All, None, 1, 200).await;
        assert_eq!(report.total, 4, "四个物料都要出现在报表里");

        let row = row_of(&report, no_standard);
        assert_eq!(row.state, STATE_NO_STANDARD);
        assert!(row.variance.is_none(), "不可比不给差异块");
        assert!(row.hint.contains("没有标准成本卡"), "{}", row.hint);
        assert!(row.hint.contains("建一版"), "hint 要给动作：{}", row.hint);

        let row = row_of(&report, unpublished);
        assert_eq!(row.state, STATE_NO_STANDARD, "草稿卡不算标准（审核后才生效）");
        assert!(row.standard.is_none(), "未发布 ⇒ 标准侧为空");
        assert!(row.hint.contains("还没发布"), "{}", row.hint);
        assert!(row.hint.contains("提交发布审批"), "hint 要给动作：{}", row.hint);

        let row = row_of(&report, no_actual);
        assert_eq!(row.state, STATE_NO_ACTUAL);
        assert!(row.actual.is_none(), "没计算单 ⇒ 实际侧为空");
        assert!(row.hint.contains(&fx.period), "要点名期间：{}", row.hint);
        assert!(row.hint.contains("成本核算"), "hint 要给动作：{}", row.hint);

        let row = row_of(&report, wip_only);
        assert_eq!(row.state, STATE_NO_ACTUAL, "完工 0 ⇒ 单位成本无从比较");
        let actual = row.actual.as_ref().expect("有计算单");
        assert_eq!(actual.completed_qty, "0.0000");
        assert_eq!(actual.period_input, "1000.0000", "本期投入 500+200+300");
        assert_eq!(actual.wip_end_cost, "1000.0000");
        assert!(row.hint.contains("完工数量为 0"), "{}", row.hint);

        // 合计：没有可比行 ⇒ 差异率无从比较（null），缺标准金额单列
        assert_eq!(report.totals.comparable_rows, 0);
        assert!(report.totals.rate.is_none(), "没有可比行时差异率必须是 null");
        assert_eq!(report.totals.no_standard_rows, 2);
        assert_eq!(report.totals.no_actual_rows, 2);
        assert_eq!(
            amount(&report.totals.no_standard_amount),
            amount("300"),
            "200（没卡）+ 100（卡没发布）"
        );

        fx.cleanup().await;
    }

    /// ③ 失效卡 / 期间换版：取期末那版，且 `switch_in_period` 与 hint 如实说明。
    #[tokio::test]
    async fn unpublished_zero_status_and_switch_versions() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let switched = fx.material("SWITCH").await;
        let disabled = fx.material("OFF").await;
        let mid = fx.start + Duration::days(9);

        // 期中换版：v1 [期初, 期初+9]，v2 [期初+10, 期末]（相邻不重叠，EXCLUDE 放行）
        fx.standard(switched, 1, fx.start, Some(mid), ("9", "3", "3"), "PUBLISHED", 1)
            .await;
        fx.standard(
            switched,
            2,
            mid + Duration::days(1),
            None,
            ("10", "4", "6"),
            "PUBLISHED",
            1,
        )
        .await;
        // 已发布但被停用（status=0）⇒ 不占窗口、不参与取数
        fx.standard(disabled, 1, fx.start, None, ("10", "4", "6"), "PUBLISHED", 0)
            .await;

        let batch = fx.batch().await;
        let object = fx.object("SWITCH", switched).await;
        fx.item(
            batch,
            object,
            switched,
            "100",
            ("1000", "400", "600"),
            ("10", "4", "6"),
            "2000",
            "0",
            "0",
        )
        .await;
        let object = fx.object("OFF", disabled).await;
        fx.item(
            batch,
            object,
            disabled,
            "10",
            ("100", "40", "60"),
            ("10", "4", "6"),
            "200",
            "0",
            "0",
        )
        .await;

        let report = fx.report(StateFilter::All, None, 1, 200).await;

        let row = row_of(&report, switched);
        assert_eq!(row.state, STATE_BOTH);
        let standard = row.standard.as_ref().expect("标准侧");
        assert_eq!(standard.version_no, 2, "取期末生效的那一版");
        assert_eq!(standard.std_total_cost, "20.000000");
        assert!(standard.switch_in_period, "期间换过版必须标记");
        assert!(row.hint.contains("换过标准成本版本"), "{}", row.hint);
        let variance = row.variance.as_ref().expect("可比");
        assert_eq!(variance.amount, "0.0000", "实际 2000 vs 标准 20 × 100");

        let row = row_of(&report, disabled);
        assert_eq!(row.state, STATE_NO_STANDARD, "停用的卡不参与取数");
        assert!(row.standard.is_none());

        fx.cleanup().await;
    }

    /// ④ 四档筛选 + 关键字 + 翻页：命中对的行，total 恒定，翻页换行。
    #[tokio::test]
    async fn filters_paging_and_keyword_narrow_the_set() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let both = fx.material("F1BOTH").await;
        let only_actual = fx.material("F1ACT").await;
        fx.standard(both, 1, fx.start, None, ("10", "4", "6"), "PUBLISHED", 1)
            .await;
        let batch = fx.batch().await;
        for (material, name) in [(both, "F1BOTH"), (only_actual, "F1ACT")] {
            let object = fx.object(name, material).await;
            fx.item(
                batch,
                object,
                material,
                "10",
                ("120", "40", "60"),
                ("12", "4", "6"),
                "220",
                "0",
                "0",
            )
            .await;
        }

        let all = fx.report(StateFilter::All, None, 1, 200).await;
        assert_eq!(all.total, 2);

        let comparable = fx.report(StateFilter::Comparable, None, 1, 200).await;
        assert_eq!(comparable.total, 1, "只有两侧都在且有差异的那行");
        assert_eq!(row_of(&comparable, both).state, STATE_BOTH);
        assert_eq!(
            row_of(&comparable, both).variance.as_ref().expect("可比").amount,
            "20.0000"
        );

        let no_standard = fx.report(StateFilter::NoStandard, None, 1, 200).await;
        assert_eq!(no_standard.total, 1);
        assert_eq!(row_of(&no_standard, only_actual).state, STATE_NO_STANDARD);

        let no_actual = fx.report(StateFilter::NoActual, None, 1, 200).await;
        assert_eq!(no_actual.total, 0, "本期两个物料都有实际");

        // 关键字：按物料编码收窄（编码带夹具尾部）
        let code = row_of(&all, both).material_code.clone().expect("物料编码");
        let narrowed = fx.report(StateFilter::All, Some(&code), 1, 200).await;
        assert_eq!(narrowed.total, 1, "关键字要真的收窄");
        assert_eq!(row_of(&narrowed, both).material_id, both.to_string());

        // 翻页：page_size=1 时两页各一行，total 恒定，顺序稳定（可比在前）
        let page1 = fx.report(StateFilter::All, None, 1, 1).await;
        let page2 = fx.report(StateFilter::All, None, 2, 1).await;
        assert_eq!(page1.total, 2);
        assert_eq!(page2.total, 2, "翻页不改 total");
        assert_eq!(page1.list.len(), 1);
        assert_eq!(page2.list.len(), 1);
        assert_ne!(page1.list[0].material_id, page2.list[0].material_id, "两页不许是同一行");
        assert_eq!(
            page1.list[0].material_id,
            both.to_string(),
            "可比行排第一（报表先看有差异的）"
        );
        assert_eq!(page1.totals.rows, 2, "合计是全集，不随分页变");
        assert_eq!(page1.totals.amount, all.totals.amount);

        fx.cleanup().await;
    }

    /// ⑤ 期间没有成本核算批次：仍列标准侧，`cost_ready=false`，note 指向「先做成本计算」。
    #[tokio::test]
    async fn period_without_batch_lists_standard_side_only() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let material = fx.material("NOBATCH").await;
        fx.standard(material, 1, fx.start, None, ("10", "4", "6"), "PUBLISHED", 1)
            .await;

        let report = fx.report(StateFilter::All, None, 1, 200).await;
        assert!(!report.cost_ready, "没有批次 ⇒ 实际侧未就绪");
        assert!(report.batch.is_none());
        assert!(report.note.contains("成本核算批次"), "note 要说明：{}", report.note);
        assert!(
            report.note.contains("/business/erp/cost-period/calc"),
            "要给出入口：{}",
            report.note
        );

        assert_eq!(report.total, 1);
        let row = row_of(&report, material);
        assert_eq!(row.state, STATE_NO_ACTUAL);
        assert!(row.actual.is_none());
        assert_eq!(report.totals.comparable_rows, 0);
        assert_eq!(
            amount(&report.totals.std_amount),
            Decimal::ZERO,
            "没有实际就没有标准应发生额"
        );

        fx.cleanup().await;
    }

    /// ⑥ 三要素拆分：材料超支 / 人工节约 / 费用持平，方向与差异率逐项对。
    #[tokio::test]
    async fn element_split_is_reported_per_element() {
        if !ensure_db().await {
            return;
        }
        let mut fx = Fixture::build().await;
        let material = fx.material("ELEM").await;
        fx.standard(material, 1, fx.start, None, ("10", "4", "6"), "PUBLISHED", 1)
            .await;
        let batch = fx.batch().await;
        let object = fx.object("ELEM", material).await;
        // 完工 50：材料 600（标准 500，超 100）、人工 150（标准 200，省 50）、费用 300（持平）
        fx.item(
            batch,
            object,
            material,
            "50",
            ("600", "150", "300"),
            ("12", "3", "6"),
            "1050",
            "0",
            "0",
        )
        .await;

        let report = fx.report(StateFilter::All, None, 1, 200).await;
        let row = row_of(&report, material);
        let variance = row.variance.as_ref().expect("可比");
        assert_eq!(variance.elements.len(), 3);

        let material_el = &variance.elements[0];
        assert_eq!(material_el.element, "MATERIAL");
        assert_eq!(material_el.element_label, "材料");
        assert_eq!(material_el.std_amount, "500.0000");
        assert_eq!(material_el.act_amount, "600.0000");
        assert_eq!(material_el.amount, "100.0000");
        assert_eq!(material_el.rate.as_deref(), Some("0.200000"));
        assert_eq!(material_el.direction, DIRECTION_UNFAVORABLE);
        assert_eq!(material_el.direction_label, "不利");
        assert_eq!(material_el.unit_amount, "2.000000", "单位 12 − 10");

        let labor_el = &variance.elements[1];
        assert_eq!(labor_el.element, "LABOR");
        assert_eq!(labor_el.amount, "-50.0000");
        assert_eq!(labor_el.rate.as_deref(), Some("-0.250000"));
        assert_eq!(labor_el.direction, DIRECTION_FAVORABLE);
        assert_eq!(labor_el.unit_amount, "-1.000000");

        let overhead_el = &variance.elements[2];
        assert_eq!(overhead_el.element, "OVERHEAD");
        assert_eq!(overhead_el.amount, "0.0000");
        assert_eq!(overhead_el.direction, DIRECTION_EVEN);
        assert_eq!(overhead_el.rate.as_deref(), Some("0.000000"), "有基数时 0 差异是 0%");

        // 要素相加 = 总差异（恒等式）
        let sum: Decimal = variance.elements.iter().map(|el| amount(&el.amount)).sum();
        assert_eq!(sum, amount(&variance.amount), "要素相加必须等于总差异");
        assert_eq!(variance.amount, "50.0000", "1050 − 1000");

        // 合计块的三要素同样给出（单位列留空）
        assert_eq!(report.totals.elements.len(), 3);
        assert_eq!(report.totals.elements[0].std_unit, "");
        assert_eq!(report.totals.elements[0].std_amount, "500.0000");

        fx.cleanup().await;
    }
}
