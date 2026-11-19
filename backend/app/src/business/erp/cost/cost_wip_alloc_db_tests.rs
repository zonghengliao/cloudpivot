//! 在制品「料工费在完工与在制之间分配」（M5-22）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m522_dev \
//!   cargo test --lib cost_wip_alloc -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_splits_three_elements_by_equivalence`] | 料 / 工 / 费三要素各自「池 ÷ 约当 × 完工数」+ 在制倒挤 + 守恒 |
//! | 2 | [`v2_prior_settled_rows_become_beginning_elements`] | 期初在制按上期单位成本**还原到三要素**（不是整笔挂一个数） |
//! | 3 | [`v3_agrees_with_the_real_calc_item`] | 与真 calc（`compose` 算出的）计算单逐字段对拍一致 |
//! | 4 | [`v4_changing_a_collect_amount_flags_drift`] | 算完又改本期归集 ⇒ `单位材料成本` / `完工转出成本` 漂移 |
//! | 5 | [`v5_all_finished_puts_the_whole_pool_into_finished`] | 期末在制 0：整池进完工、在制 0（尾差不留在在制） |
//! | 6 | [`v6_wip_without_pct_is_blocked_not_zero`] | 在制 > 0 但完工率 0：金额给 `null` 不给 0，带「怎么修」 |
//! | 7 | [`v7_no_batch_still_shows_the_trial_table`] | 没建批次：照出试算视图（`cost_ready = false`、`calc = null`） |
//! | 8 | [`v8_other_workshop_sees_nothing`] | 数据范围：别车间用户看不到本车间的行（不出现，不是 403 探测口） |
//! | 9 | [`v9_totals_are_the_whole_table_and_filters_narrow`] | 合计是整张表（`page_size = 1` 时 `row_count = 3`）；五档筛选 |
//! | 10 | [`zz_teardown_leaves_zero_residue`] | 本夹具按对象号清场，零残留 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `2000 + slot % 70`，期间取该年 01/02/03 三个月）；
//!   建前先清该槽上一次的残留；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**；
//! * 计算单用 `cost_period_item_dto::compose`（与真 calc 同一段代码）算好再落库，
//!   不是手抄几个数字 —— 否则「对拍一致」这个断言会变成同义反复。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{
        self as item_dto, BegCost, ItemCalc, ItemInput, OutputInput,
    };
    use crate::business::erp::cost::cost_wip_alloc_dto::{self as dto, AllocQuery, AllocRes, Element};
    use crate::business::erp::cost::cost_wip_alloc_handler::{alloc_table_of, filter_of};
    use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
    use cloudpivot_kernel::business::shared::scope::ScopeKind;
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

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数量字面量")
    }

    /// 出参里的数量是字符串：按 Decimal 比，避免「1000 vs 1000.0000」这种标度假红。
    fn qty(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数量字符串无法解析：{text}"))
    }

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    struct Fixture {
        suffix: String,
        /// 三个连续期间 01 / 02 / 03（跨月 / 期初来源）。
        periods: [String; 3],
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 2000 + (slot % 70) as i32;
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
            ];
            Self::reset(&periods).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &format!("M522-DEPT2-{suffix}"),
                &format!("M5-22 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M522-MAT-{suffix}"),
                &format!("M5-22 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M52W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-22 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
                product,
                warehouse,
            }
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        fn other_dept_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(self.other_dept),
                false,
                ScopeKind::Dept,
                vec![self.other_dept],
                false,
            )
        }

        /// 本夹具的标签（对象号里带它，列表断言靠它收窄）。
        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M522-{}", self.tag(name))
        }

        /// 造成本对象（完工 / 在制 / 完工率 / 状态由用例给）。
        async fn object(&self, name: &str, status: &str, completed: &str, wip: &str, pct: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  completion_pct, material_input_mode, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 800, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, 'ONE_TIME', \
                             ?::uuid, ?, '{}-01', ?, {closed_at}, now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&self.product),
                    Value::Text(format!(
                        "LOT-M522-{name}-{}",
                        crate::test_seed::last_chars(&self.suffix, 6)
                    )),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 改完工率 / 在制（模拟「算完又改数」）。
        async fn patch_object(&self, id: Uuid, wip: &str, pct: &str) {
            db().exec_sql(
                "UPDATE biz_cost_object SET wip_qty = ?::text::numeric, \
                   completion_pct = ?::text::numeric, updated_at = now() WHERE id = ?::uuid"
                    .to_string(),
                vec![
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&id),
                ],
            )
            .await
            .expect("改成本对象夹具");
        }

        /// 造一条归集行（料行必须挂物料 + 数量；工 / 费行不许挂）。
        async fn collect(&self, object: Uuid, element: &str, amount: &str, period: &str) {
            let (material, qty) = if element == "MATERIAL" {
                ("?::uuid", "10")
            } else {
                ("NULL", "NULL")
            };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&object),
                Value::Text(period.to_string()),
                Value::Text(element.to_string()),
                Value::Text(format!("{period}-10")),
            ];
            if element == "MATERIAL" {
                params.push(Value::from(&self.product));
            }
            params.push(Value::Text(amount.to_string()));
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                  line_no, occur_date, material_id, qty, amount, \
                                                  remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, {material}, \
                             {qty}, ?::text::numeric, 'M5-22 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集行夹具");
        }

        /// 改一条归集行的金额（模拟「算完又改归集」）。
        async fn patch_collect(&self, object: Uuid, element: &str, amount: &str) {
            db().exec_sql(
                "UPDATE biz_cost_collect SET amount = ?::text::numeric \
                  WHERE cost_object_id = ?::uuid AND element = ? AND remark = 'M5-22 夹具'"
                    .to_string(),
                vec![
                    Value::Text(amount.to_string()),
                    Value::from(&object),
                    Value::Text(element.to_string()),
                ],
            )
            .await
            .expect("改归集行夹具");
        }

        /// 造一张成本核算批次（`SETTLED` 要挂审批 + 结算时间 + 完工凭证，库层硬约束）。
        async fn period_batch(&self, period: &str, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let settled = if status == "SETTLED" { "now()" } else { "NULL" };
            let approval = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let voucher = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let mut params = vec![
                Value::from(&id),
                Value::Text(period.to_string()),
                Value::Text(format!("CS-M522-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                Value::Text(status.to_string()),
            ];
            if status == "SETTLED" {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period (id, period_code, batch_no, status, \
                                                  approval_instance_id, settled_at, finish_voucher_id, \
                                                  created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?, {approval}, {settled}, {voucher}, now(), now())"
                ),
                params,
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        /// 用 **真 calc 的 `compose`** 算一张计算单再落库（不是手抄数字）。
        #[allow(clippy::too_many_arguments)]
        async fn calc_item(
            &self,
            object: Uuid,
            batch: Uuid,
            status: &str,
            period: &str,
            completed: &str,
            wip: &str,
            pct: &str,
            mat: &str,
            labor: &str,
            oh: &str,
            beg: BegCost,
            beg_qty: &str,
        ) -> ItemCalc {
            let input = ItemInput {
                period_code: period.to_string(),
                cost_object_id: object,
                object_no: format!("{:?}", object),
                product_id: self.product,
                output: OutputInput {
                    cumulative_completed_qty: dec(completed),
                    prior_completed_qty: Decimal::ZERO,
                    wip_end_qty: dec(wip),
                    completion_pct: dec(pct),
                    prior_wip_qty: dec(beg_qty),
                    material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
                },
                beg,
                mat_cur: dec(mat),
                labor_cur: dec(labor),
                oh_cur: dec(oh),
            };
            let calc = item_dto::compose(&input).expect("夹具计算单必须可算（compose 拒绝了输入）");
            let stock_in = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&batch),
                Value::from(&object),
                Value::from(&self.product),
                Value::Text(beg_qty.to_string()),
                Value::Text(calc.wip_beg_cost.to_string()),
                Value::Text(calc.completed_qty.to_string()),
                Value::Text(calc.wip_end_qty.to_string()),
                Value::Text(calc.completion_pct.to_string()),
                Value::Text(calc.equiv_material.to_string()),
                Value::Text(calc.equiv_conversion.to_string()),
                Value::Text(calc.mat_cur.to_string()),
                Value::Text(calc.labor_cur.to_string()),
                Value::Text(calc.oh_cur.to_string()),
                Value::Text(calc.unit_material.to_string()),
                Value::Text(calc.unit_labor.to_string()),
                Value::Text(calc.unit_overhead.to_string()),
                Value::Text(calc.finished_cost.to_string()),
                Value::Text(calc.wip_end_cost.to_string()),
                Value::Text(calc.unit_finished_cost.to_string()),
                Value::Text(status.to_string()),
            ];
            if status == "SETTLED" {
                params.push(Value::from(&Uuid::now_v7()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                        wip_beg_qty, wip_beg_cost, completed_qty, \
                                                        wip_end_qty, completion_pct, equiv_material, \
                                                        equiv_conversion, mat_cur, labor_cur, oh_cur, \
                                                        unit_material, unit_labor, unit_overhead, \
                                                        finished_cost, wip_end_cost, unit_finished_cost, \
                                                        status, stock_in_id, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?, {stock_in}, now(), now())"
                ),
                params,
            )
            .await
            .expect("造计算单夹具");
            calc
        }

        /// 清掉该槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let first = &periods[0];
            let third = &periods[2];
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect WHERE remark = 'M5-22 夹具' AND period_code BETWEEN ? AND ?"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(third.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                       (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M522-%' \
                          AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M522-%' \
                        AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M522-%' \
                        AND period_code BETWEEN ? AND ?"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(third.clone())],
                )
                .await;
        }

        /// 清场（FK 顺序：计算单 → 归集 → 期间批次 → 对象）。
        async fn cleanup(&self, objects: &[Uuid], period_batches: &[Uuid]) {
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if objects.is_empty() {
                return;
            }
            let objects_sql = list(objects);
            for statement in [
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({objects_sql})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects_sql})"),
            ] {
                db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
            }
            if !period_batches.is_empty() {
                db().exec_sql(
                    format!("DELETE FROM biz_cost_period WHERE id IN ({})", list(period_batches)),
                    vec![],
                )
                .await
                .expect("清场（期间批次）");
            }
            db().exec_sql(
                format!("DELETE FROM biz_cost_object WHERE id IN ({objects_sql})"),
                vec![],
            )
            .await
            .expect("清场（成本对象）");
        }

        /// 本夹具残留行数（清场断言用：按对象号后缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            let pattern = Value::Text(format!("%{}", self.tag("")));
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE ?) + \
                        (SELECT count(*) FROM biz_cost_period_item WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) \
                      + (SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![pattern.clone(), pattern.clone(), pattern],
            )
            .await
            .expect("查残留")
        }
    }

    /// 归一化 → 取数（走真实 handler 代码路径，只跳过认证）。
    async fn table_of(
        principal: &ScopedPrincipal,
        period: &str,
        tag: &str,
        state: Option<&str>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> AllocRes {
        let query = AllocQuery {
            period_code: Some(period.to_string()),
            keyword: Some(tag.to_string()),
            state: state.map(str::to_string),
            page,
            page_size,
        };
        let filter = filter_of(&query).expect("入参归一化");
        alloc_table_of(principal, &filter).await.expect("在制品分配表")
    }

    /// 按对象号取一行（找不到就是夹具没造对，直接失败）。
    fn row_of<'a>(sheet: &'a AllocRes, object_no: &str) -> &'a dto::AllocRow {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    fn element_of(row: &dto::AllocRow, element: Element) -> &dto::ElementView {
        row.elements
            .iter()
            .find(|item| item.element == element.as_str())
            .unwrap_or_else(|| panic!("行里缺要素 {}", element.as_str()))
    }

    // ==================== 1. 三要素分别分配 ====================

    #[tokio::test]
    async fn v1_splits_three_elements_by_equivalence() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V1", "OPEN", "800", "200", "50").await;
        // 料 10000（材料约当 1000）→ 10/件；工 4500、费 2700（加工约当 900）→ 5 / 3
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;
        f.collect(object, "LABOR", "4500", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "2700", &f.periods[0]).await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V1"), None, None, None).await;
        assert!(!sheet.cost_ready, "没建批次 ⇒ 试算视图");
        let row = row_of(&sheet, &f.object_no("V1"));

        assert_eq!(qty(&row.completed_qty), dec("800"));
        assert_eq!(qty(&row.wip_qty), dec("200"));
        assert_eq!(qty(row.equiv_material.as_deref().unwrap()), dec("1000"));
        assert_eq!(qty(row.equiv_conversion.as_deref().unwrap()), dec("900"));
        assert_eq!(qty(row.pool_total.as_deref().unwrap()), dec("17200"));
        assert_eq!(qty(row.finished_total.as_deref().unwrap()), dec("14400"));
        assert_eq!(qty(row.wip_total.as_deref().unwrap()), dec("2800"));
        assert_eq!(qty(&row.cur_total), dec("17200"));

        let material = element_of(row, Element::Material);
        assert_eq!(qty(material.rate.as_deref().unwrap()), dec("10"));
        assert_eq!(qty(material.finished.as_deref().unwrap()), dec("8000"));
        assert_eq!(qty(material.wip.as_deref().unwrap()), dec("2000"));
        let labor = element_of(row, Element::Labor);
        assert_eq!(qty(labor.equiv.as_deref().unwrap()), dec("900"));
        assert_eq!(qty(labor.finished.as_deref().unwrap()), dec("4000"));
        assert_eq!(qty(labor.wip.as_deref().unwrap()), dec("500"));
        let overhead = element_of(row, Element::Overhead);
        assert_eq!(qty(overhead.finished.as_deref().unwrap()), dec("2400"));
        assert_eq!(qty(overhead.wip.as_deref().unwrap()), dec("300"));
        assert!(material.balanced && labor.balanced && overhead.balanced);
        // 整行守恒：完工 + 在制 = 池
        assert_eq!(
            qty(row.finished_total.as_deref().unwrap()) + qty(row.wip_total.as_deref().unwrap()),
            qty(row.pool_total.as_deref().unwrap())
        );
        assert_eq!(row.state, "wip");
        assert!(row.issues.is_empty(), "正常行不该有问题：{:?}", row.issues);
        assert!(row.calc.is_none(), "没批次不该有计算单快照");

        // 合计与行同源
        assert_eq!(sheet.totals.row_count, 1);
        assert_eq!(qty(&sheet.totals.pool_material), dec("10000"));
        assert_eq!(qty(&sheet.totals.pool_labor), dec("4500"));
        assert_eq!(qty(&sheet.totals.pool_overhead), dec("2700"));
        assert_eq!(qty(&sheet.totals.finished_total), dec("14400"));
        assert_eq!(qty(&sheet.totals.wip_total), dec("2800"));
        assert!(sheet.note.contains("试算"), "没批次要在页脚说清楚：{}", sheet.note);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 2. 期初还原到三要素 ====================

    #[tokio::test]
    async fn v2_prior_settled_rows_become_beginning_elements() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V2", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[1]).await;
        f.collect(object, "LABOR", "4500", &f.periods[1]).await;
        f.collect(object, "OVERHEAD", "2700", &f.periods[1]).await;

        // 上期已结算：完工 0、期末在制 100 @50%、料 2000 / 工 300 / 费 700
        // ⇒ 单位成本 20 / 6 / 14，期末在制成本 3000（材料 2000、人工 300、费用 700）
        let prior_batch = f.period_batch(&f.periods[0], "SETTLED").await;
        let prior = f
            .calc_item(
                object,
                prior_batch,
                "SETTLED",
                &f.periods[0],
                "0",
                "100",
                "50",
                "2000",
                "300",
                "700",
                BegCost::default(),
                "0",
            )
            .await;
        assert_eq!(prior.wip_end_cost, dec("3000"));
        assert_eq!(prior.unit_material, dec("20"));
        assert_eq!(prior.unit_labor, dec("6"));

        let sheet = table_of(&f.super_principal(), &f.periods[1], &f.tag("V2"), None, None, None).await;
        let row = row_of(&sheet, &f.object_no("V2"));
        // 期初来源点名到上期
        let source = row.prior.as_ref().expect("有已结算上期就该有期初来源");
        assert_eq!(source.period_code, f.periods[0]);
        assert_eq!(qty(&source.wip_end_cost), dec("3000"));
        // 期初按要素还原：料 20×100=2000、工 6×(100×50%)=300、费取尾差 700
        assert_eq!(qty(row.beg_total.as_deref().unwrap()), dec("3000"));
        let material = element_of(row, Element::Material);
        assert_eq!(qty(material.beg.as_deref().unwrap()), dec("2000"));
        assert_eq!(qty(material.pool.as_deref().unwrap()), dec("12000"));
        assert_eq!(qty(material.rate.as_deref().unwrap()), dec("12"), "12000 / 1000");
        assert_eq!(qty(element_of(row, Element::Labor).beg.as_deref().unwrap()), dec("300"));
        assert_eq!(
            qty(element_of(row, Element::Overhead).beg.as_deref().unwrap()),
            dec("700")
        );
        // 池 = 期初 + 本期投入
        assert_eq!(qty(row.pool_total.as_deref().unwrap()), dec("20200"));
        assert_eq!(
            qty(row.finished_total.as_deref().unwrap()) + qty(row.wip_total.as_deref().unwrap()),
            dec("20200")
        );

        f.cleanup(&[object], &[prior_batch]).await;
    }

    // ==================== 3. 与真计算单对拍 ====================

    #[tokio::test]
    async fn v3_agrees_with_the_real_calc_item() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V3", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;
        f.collect(object, "LABOR", "4500", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "2700", &f.periods[0]).await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(
            object,
            batch,
            "CALC",
            &f.periods[0],
            "800",
            "200",
            "50",
            "10000",
            "4500",
            "2700",
            BegCost::default(),
            "0",
        )
        .await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V3"), None, None, None).await;
        assert!(sheet.cost_ready, "有批次 ⇒ 对拍一列有意义");
        let row = row_of(&sheet, &f.object_no("V3"));
        let calc = row.calc.as_ref().expect("有计算单就该挂快照");
        assert!(calc.agrees, "同一组输入算出的同一组分配必须一致：{:?}", calc.drift);
        assert!(calc.drift.is_empty());
        assert_eq!(qty(&calc.finished_cost), dec("14400"));
        assert_eq!(qty(&calc.wip_end_cost), dec("2800"));
        assert_eq!(qty(&calc.unit_material), dec("10"));
        assert!(!row.recalc_required);
        assert_eq!(sheet.totals.drift_row_count, 0);

        f.cleanup(&[object], &[batch]).await;
    }

    // ==================== 4. 改归集 ⇒ 漂移 ====================

    #[tokio::test]
    async fn v4_changing_a_collect_amount_flags_drift() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V4", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;
        f.collect(object, "LABOR", "4500", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "2700", &f.periods[0]).await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(
            object,
            batch,
            "CALC",
            &f.periods[0],
            "800",
            "200",
            "50",
            "10000",
            "4500",
            "2700",
            BegCost::default(),
            "0",
        )
        .await;
        // 算完给材料再补 1000（10 / 件 → 11 / 件）
        f.patch_collect(object, "MATERIAL", "11000").await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V4"), None, None, None).await;
        let row = row_of(&sheet, &f.object_no("V4"));
        assert!(row.recalc_required, "改过归集就要重算");
        let calc = row.calc.as_ref().expect("计算单快照");
        assert!(!calc.agrees);
        let fields: Vec<&str> = calc.drift.iter().map(|item| item.field.as_str()).collect();
        assert!(fields.contains(&"单位材料成本"), "字段：{fields:?}");
        assert!(fields.contains(&"完工转出成本"), "字段：{fields:?}");
        let unit = calc
            .drift
            .iter()
            .find(|item| item.field == "单位材料成本")
            .expect("必须有单位材料成本差异");
        assert_eq!(qty(&unit.table_value), dec("11"));
        assert_eq!(qty(&unit.calc_value), dec("10"));
        assert!(calc.note.contains("重算"), "说明要指向重算：{}", calc.note);

        let drifted = table_of(
            &f.super_principal(),
            &f.periods[0],
            &f.tag("V4"),
            Some("drift"),
            None,
            None,
        )
        .await;
        assert_eq!(drifted.total, 1, "drift 档要能一屏筛出");

        f.cleanup(&[object], &[batch]).await;
    }

    // ==================== 5. 全部完工 ====================

    #[tokio::test]
    async fn v5_all_finished_puts_the_whole_pool_into_finished() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V5", "OPEN", "500", "0", "0").await;
        f.collect(object, "MATERIAL", "5000", &f.periods[0]).await;
        f.collect(object, "LABOR", "1500", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "700", &f.periods[0]).await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V5"), None, None, None).await;
        let row = row_of(&sheet, &f.object_no("V5"));
        assert_eq!(row.state, "finished");
        assert_eq!(qty(&row.completion_pct), dec("0"));
        // 期初 0 + 本期 7200 ⇒ 全部进完工，在制 0
        assert_eq!(qty(row.finished_total.as_deref().unwrap()), dec("7200"));
        assert_eq!(qty(row.wip_total.as_deref().unwrap()), Decimal::ZERO);
        assert_eq!(qty(&sheet.totals.finished_total), dec("7200"));
        assert_eq!(qty(&sheet.totals.wip_total), Decimal::ZERO);
        assert!(
            row.warnings.iter().any(|text| text.contains("全部完工")),
            "要提示尾差进完工"
        );

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 6. 在制无完工率 ⇒ 空值 ====================

    #[tokio::test]
    async fn v6_wip_without_pct_is_blocked_not_zero() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V6", "OPEN", "800", "200", "0").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;
        f.collect(object, "LABOR", "4500", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "2700", &f.periods[0]).await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V6"), None, None, None).await;
        let row = row_of(&sheet, &f.object_no("V6"));
        assert_eq!(row.state, "blocked");
        assert_eq!(row.issues[0].code, "WIP_PCT_MISSING");
        assert!(!row.issues[0].fix.is_empty(), "要给怎么修");
        // 金额不给 0（0 会被读成「没有成本」）
        assert_eq!(row.finished_total, None);
        assert_eq!(row.wip_total, None);
        for item in &row.elements {
            assert_eq!(item.finished, None, "{} 不该给金额", item.label);
            assert_eq!(item.wip, None);
            assert_eq!(item.rate, None);
        }
        // 材料侧分母仍在（它是事实），但整行不分钱
        assert_eq!(qty(row.equiv_material.as_deref().unwrap()), dec("1000"));
        assert_eq!(row.equiv_conversion, None);
        // 合计：金额跳过、数量照算
        assert_eq!(sheet.totals.blocked_row_count, 1);
        assert_eq!(qty(&sheet.totals.completed_qty), dec("800"));
        assert_eq!(qty(&sheet.totals.pool_total), Decimal::ZERO);
        assert!(sheet.totals.note.contains("1 行"), "{}", sheet.totals.note);

        // 改完工率 50% ⇒ 立刻能分
        f.patch_object(object, "200", "50").await;
        let fixed = table_of(&f.super_principal(), &f.periods[0], &f.tag("V6"), None, None, None).await;
        let row = row_of(&fixed, &f.object_no("V6"));
        assert_eq!(row.state, "wip");
        assert_eq!(qty(row.finished_total.as_deref().unwrap()), dec("14400"));

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 7. 没批次 = 试算视图 ====================

    #[tokio::test]
    async fn v7_no_batch_still_shows_the_trial_table() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V7", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;

        let sheet = table_of(&f.super_principal(), &f.periods[0], &f.tag("V7"), None, None, None).await;
        assert!(!sheet.cost_ready);
        assert!(sheet.batch.is_none());
        let row = row_of(&sheet, &f.object_no("V7"));
        assert!(row.calc.is_none());
        assert!(!row.recalc_required, "没有计算单就没有漂移");
        // 只在材料一侧有钱：工 / 费池 0 ⇒ 分配额 0（不是算不出来）
        assert_eq!(qty(row.finished_total.as_deref().unwrap()), dec("8000"));
        assert_eq!(qty(row.wip_total.as_deref().unwrap()), dec("2000"));

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 8. 数据范围 ====================

    #[tokio::test]
    async fn v8_other_workshop_sees_nothing() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V8", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;

        let mine = table_of(&f.super_principal(), &f.periods[0], &f.tag("V8"), None, None, None).await;
        assert_eq!(mine.total, 1);
        let others = table_of(&f.other_dept_principal(), &f.periods[0], &f.tag("V8"), None, None, None).await;
        assert_eq!(others.total, 0, "别车间一行都看不到（不出现，不是 403）");
        assert!(others.list.is_empty());

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 9. 合计与筛选 ====================

    #[tokio::test]
    async fn v9_totals_are_the_whole_table_and_filters_narrow() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let wip = f.object("V9A", "OPEN", "800", "200", "50").await;
        let finished = f.object("V9B", "OPEN", "500", "0", "0").await;
        let blocked = f.object("V9C", "OPEN", "100", "50", "0").await;
        f.collect(wip, "MATERIAL", "10000", &f.periods[0]).await;
        f.collect(wip, "LABOR", "4500", &f.periods[0]).await;
        f.collect(wip, "OVERHEAD", "2700", &f.periods[0]).await;
        f.collect(finished, "MATERIAL", "5000", &f.periods[0]).await;
        f.collect(blocked, "MATERIAL", "1000", &f.periods[0]).await;

        // page_size = 1：合计仍是整张表（关键字用夹具后缀收窄，三个对象号只共享后缀）
        let page = table_of(&f.super_principal(), &f.periods[0], &f.tag(""), None, Some(1), Some(1)).await;
        assert_eq!(page.list.len(), 1);
        assert_eq!(page.total, 3);
        assert_eq!(page.totals.row_count, 3);
        assert_eq!(page.totals.wip_row_count, 1);
        assert_eq!(page.totals.finished_row_count, 1);
        assert_eq!(page.totals.blocked_row_count, 1);
        assert_eq!(qty(&page.totals.pool_material), dec("15000"), "金额不含 blocked 行");
        assert_eq!(qty(&page.totals.pool_labor), dec("4500"));
        assert_eq!(qty(&page.totals.pool_overhead), dec("2700"));
        assert_eq!(
            qty(&page.totals.finished_total) + qty(&page.totals.wip_total),
            qty(&page.totals.pool_total)
        );

        for (state, expected) in [("wip", 1_u64), ("finished", 1), ("blocked", 1), ("drift", 0)] {
            let filtered = table_of(&f.super_principal(), &f.periods[0], &f.tag(""), Some(state), None, None).await;
            assert_eq!(filtered.total, expected, "state={state}");
        }

        f.cleanup(&[wip, finished, blocked], &[]).await;
    }

    // ==================== 10. 清场 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("ZZ", "OPEN", "800", "200", "50").await;
        f.collect(object, "MATERIAL", "10000", &f.periods[0]).await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(
            object,
            batch,
            "CALC",
            &f.periods[0],
            "800",
            "200",
            "50",
            "10000",
            "0",
            "0",
            BegCost::default(),
            "0",
        )
        .await;
        assert!(f.residue().await > 0, "清场前应有残留");

        f.cleanup(&[object], &[batch]).await;
        assert_eq!(f.residue().await, 0, "清场后本夹具零残留");
    }
}
