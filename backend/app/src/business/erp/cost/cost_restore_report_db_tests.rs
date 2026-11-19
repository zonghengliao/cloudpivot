//! 成本还原「还原后报表：料工费占比」（M5-28）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m528_dev \
//!   cargo test --lib cost_restore_report -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_dept_groups_are_sums_of_their_rows`] | 按车间分组：A 组 = 成品 F 一行、B 组 = 成品 G 一行；金额 / 占比 / 主导要素 |
//! | 2 | [`v2_totals_equal_sum_of_groups`] | 整表合计 = 两组之和（`group_by=all` 与 `dept` 两种口径一致） |
//! | 3 | [`v3_object_groups_reconcile_with_rollup_table`] | 按对象分组逐行等于**明细表**（M5-27）同一对象的三要素与不彻底 |
//! | 4 | [`v4_state_filter_and_paging`] | `state=no_semi` 只留 B 组；`page_size=1` 合计仍是整表 |
//! | 5 | [`v5_other_dept_scope_sees_only_itself`] | 数据范围：别车间看不到 A 组，也不报 403 |
//! | 6 | [`zz_teardown_leaves_zero_residue`] | 本夹具按对象号 / 批号前缀清场，零残留 |
//!
//! # 夹具纪律
//!
//! * 本夹具独占期间槽（年份 = `1700 + slot % 80`，期间取该年 01 / 02）——
//!   与 M5-27 的 `1800 + slot`、M5-26 的 `1900 + slot`、M5-23 / M5-22 的 `2000 + slot` 错开，
//!   避免撞 `uk_biz_cost_period`；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，列表断言一律用 `keyword` 收窄到本夹具；
//! * 计算单用 `cost_period_item_dto::compose`（与真 calc 同一段代码）算好再落库 ——
//!   「还原后的金额」不能靠在测试里手抄数字来成立。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{self as item_dto, BegCost, ItemInput, OutputInput};
    use crate::business::erp::cost::cost_restore_report_dto::{ReportFilter, ReportQuery, ReportRes};
    use crate::business::erp::cost::cost_restore_report_handler::{filter_of, report_of};
    use crate::business::erp::cost::cost_restore_rollup_dto::{RollupFilter, RollupQuery};
    use crate::business::erp::cost::cost_restore_rollup_handler::{filter_of as rollup_filter_of, rollup_table_of};
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
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字面量：{text}"))
    }

    /// 出参里的金额是字符串：按 Decimal 比，避免「1000 vs 1000.0000」这种标度假红。
    fn num(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字符串无法解析：{text}"))
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
        /// 两个连续期间：01 = 半成品 S 的产出期，02 = 成品 F / G 的投入期。
        periods: [String; 2],
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        other_dept_name: String,
        unit: Uuid,
        warehouse: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 1700 + (slot % 80) as i32;
            let periods = [format!("{year:04}-01"), format!("{year:04}-02")];
            Self::reset().await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept_name = format!("M5-28 夹具第二车间 {suffix}");
            let other_dept =
                cloudpivot_kernel::shared::test_helpers::ensure_dept(&format!("M528-DEPT2-{suffix}"), &other_dept_name)
                    .await;
            let unit = crate::test_seed::ensure_unit().await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M528W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-28 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
                other_dept_name,
                unit,
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

        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M528-{}", self.tag(name))
        }

        fn lot(&self, name: &str) -> String {
            format!("LOT-M528-{}", self.tag(name))
        }

        /// 造物料（`RAW` / `SEMI` / `FINISHED`）。
        async fn material(&self, kind: &str, name: &str) -> Uuid {
            crate::test_seed::ensure_material_kind(
                &format!("M528-{kind}-{}-{name}", crate::test_seed::last_chars(&self.suffix, 6)),
                &format!("M5-28 夹具 {kind} {name} {}", self.suffix),
                kind,
            )
            .await
        }

        /// 造成本对象（数量 / 在制 / 完工率同时写到对象上，与计算单一致）。
        async fn object(
            &self,
            name: &str,
            product: Uuid,
            lot: Option<&str>,
            dept: Uuid,
            dept_name: &str,
            completed: &str,
        ) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  completion_pct, material_input_mode, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 1000, ?::text::numeric, \
                             0, 0, 'ONE_TIME', \
                             ?::uuid, ?, '{}-01', 'OPEN', now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&product),
                    lot.map(|text| Value::Text(text.to_string()))
                        .unwrap_or(Value::Text(String::new())),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::from(&dept),
                    Value::Text(dept_name.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 造一条**带批号**的领料行（先造一张已过账领料单明细，再让归集行指向它）。
        async fn collect_material_with_lot(
            &self,
            object: Uuid,
            period: &str,
            material: Uuid,
            lot: &str,
            qty: &str,
            amount: &str,
        ) {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let out_no = format!("OUT-M528-{}-{seq}", crate::test_seed::last_chars(&self.suffix, 6));
            let stock_out = Uuid::now_v7();
            let item = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, 'POSTED', \
                         ?::text::numeric, ?::text::numeric, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&stock_out),
                    Value::Text(out_no.clone()),
                    Value::from(&self.warehouse),
                    Value::Text(format!("{period}-10")),
                    Value::Text(qty.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造领料单夹具");
            db().exec_sql(
                "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, \
                                                  unit_cost, amount, lot_no, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&item),
                    Value::from(&stock_out),
                    Value::from(&material),
                    Value::from(&self.unit),
                    Value::Text(qty.to_string()),
                    Value::Text((dec(amount) / dec(qty)).round_dp(6).to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(lot.to_string()),
                ],
            )
            .await
            .expect("造领料明细夹具");
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                source_id, source_no, line_no, occur_date, material_id, \
                                                qty, unit_price, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, ?, 1, ?::text::date, \
                         ?::uuid, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         'M5-28 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::from(&item),
                    Value::Text(out_no),
                    Value::Text(format!("{period}-10")),
                    Value::from(&material),
                    Value::Text(qty.to_string()),
                    Value::Text((dec(amount) / dec(qty)).round_dp(6).to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造带批号归集行夹具");
        }

        /// 造一条工 / 费归集行（不许挂物料）。
        async fn collect_element(&self, object: Uuid, period: &str, element: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                line_no, occur_date, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, ?::text::numeric, \
                         'M5-28 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::Text(element.to_string()),
                    Value::Text(format!("{period}-10")),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造工 / 费归集行夹具");
        }

        /// 造成本核算批次。
        async fn batch(&self, period: &str, tag: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period.to_string()),
                    Value::Text(format!(
                        "CS-M528-{tag}-{}",
                        crate::test_seed::last_chars(&self.suffix, 8)
                    )),
                ],
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        /// 用 **真 calc 的 `compose`** 算一张计算单再落库（不是手抄数字）。
        #[allow(clippy::too_many_arguments)]
        async fn item(
            &self,
            object: Uuid,
            object_name: &str,
            batch: Uuid,
            product: Uuid,
            period: &str,
            completed: &str,
            mat: &str,
            labor: &str,
            oh: &str,
        ) {
            let input = ItemInput {
                period_code: period.to_string(),
                cost_object_id: object,
                object_no: self.object_no(object_name),
                product_id: product,
                output: OutputInput {
                    cumulative_completed_qty: dec(completed),
                    prior_completed_qty: Decimal::ZERO,
                    wip_end_qty: Decimal::ZERO,
                    completion_pct: Decimal::ZERO,
                    prior_wip_qty: Decimal::ZERO,
                    material_input_mode: item_dto::INPUT_MODE_ONE_TIME.to_string(),
                },
                beg: BegCost::default(),
                mat_cur: dec(mat),
                labor_cur: dec(labor),
                oh_cur: dec(oh),
            };
            let calc = item_dto::compose(&input).expect("夹具计算单必须可算（compose 拒绝了输入）");
            db().exec_sql(
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
                         ?::text::numeric, ?::text::numeric, 'CALC', NULL, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&batch),
                    Value::from(&object),
                    Value::from(&product),
                    Value::Text(calc.wip_beg_qty.to_string()),
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
                ],
            )
            .await
            .expect("造计算单夹具");
        }

        /// 清掉本卡之前跑过的残留（只清本卡能造的形态，按前缀 / 批号）。
        async fn reset() {
            for statement in [
                "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M528-%')",
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-28 夹具'",
                "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-M528-%'",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M528-%'",
                "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M528-%'",
            ] {
                let _ = db().exec_sql(statement.to_string(), vec![]).await;
            }
        }

        /// 清场（FK 顺序：计算单 → 归集 → 领料单 → 对象 → 批次）。
        async fn cleanup(&self, objects: &[Uuid], batches: &[Uuid]) {
            let ids = |list: &[Uuid]| {
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !objects.is_empty() {
                let list = ids(objects);
                for statement in [
                    format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({list})"),
                    format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
                ] {
                    db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
                }
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-M528-%'".to_string(),
                    vec![],
                )
                .await;
            if !objects.is_empty() {
                db().exec_sql(
                    format!("DELETE FROM biz_cost_object WHERE id IN ({})", ids(objects)),
                    vec![],
                )
                .await
                .expect("清场（成本对象）");
            }
            if !batches.is_empty() {
                db().exec_sql(
                    format!("DELETE FROM biz_cost_period WHERE id IN ({})", ids(batches)),
                    vec![],
                )
                .await
                .expect("清场（批次）");
            }
        }

        /// 本夹具残留行数（清场断言用：按对象号 / 批号前缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE 'CB-M528-%') + \
                        (SELECT count(*) FROM biz_cost_period WHERE batch_no LIKE 'CS-M528-%') + \
                        (SELECT count(*) FROM biz_cost_collect WHERE remark = 'M5-28 夹具') + \
                        (SELECT count(*) FROM biz_stock_out WHERE out_no LIKE 'OUT-M528-%')",
                vec![],
            )
            .await
            .expect("残留统计")
        }
    }

    // ---------- 夹具：S（半成品）+ F（一车间成品）+ G（二车间成品，全外购料） ----------

    struct Chain {
        fixture: Fixture,
        /// 成本对象（S / F / G）。
        objects: Vec<Uuid>,
        batches: Vec<Uuid>,
    }

    /// ```text
    /// 期间 01：S 批（一车间）—— 外购 R 500 + 工 200 + 费 300 = 1000，完工 100 ⇒ 结构 500/200/300
    /// 期间 02：F 批（一车间）—— S 400（批号命中） + 外购 R 100 + 工 100 + 费 100 = 700
    ///          G 批（二车间）—— 外购 R 300 + 工 50 + 费 50 = 400
    /// ```
    ///
    /// 还原后：F = 料 300（100 + 200）/ 工 180（100 + 80）/ 费 220（100 + 120）；G 不动。
    async fn build_chain() -> Chain {
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R").await;
        let semi = fixture.material("SEMI", "S").await;
        let finished = fixture.material("FINISHED", "F").await;
        let finished_g = fixture.material("FINISHED", "G").await;

        let batch_s = fixture.batch(&fixture.periods[0], "P1").await;
        let batch_f = fixture.batch(&fixture.periods[1], "P2").await;
        let other_dept_name = fixture.other_dept_name.clone();

        let lot_s = fixture.lot("S");
        let lot_f = fixture.lot("F");
        let lot_g = fixture.lot("G");
        let dept_name = fixture.dept_name.clone();
        let object_s = fixture
            .object("S", semi, Some(&lot_s), fixture.dept, &dept_name, "100")
            .await;
        let object_f = fixture
            .object("F", finished, Some(&lot_f), fixture.dept, &dept_name, "70")
            .await;
        let object_g = fixture
            .object(
                "G",
                finished_g,
                Some(&lot_g),
                fixture.other_dept,
                &other_dept_name,
                "50",
            )
            .await;

        // S 批：外购 R 500 + 工 200 + 费 300
        fixture
            .collect_material_with_lot(object_s, &fixture.periods[0], raw, &fixture.lot("R1"), "100", "500")
            .await;
        fixture
            .collect_element(object_s, &fixture.periods[0], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_s, &fixture.periods[0], "OVERHEAD", "300")
            .await;
        fixture
            .item(
                object_s,
                "S",
                batch_s,
                semi,
                &fixture.periods[0],
                "100",
                "500",
                "200",
                "300",
            )
            .await;

        // F 批：S 400（批号对得上）+ 外购 R 100 + 工 100 + 费 100
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[1], semi, &lot_s, "40", "400")
            .await;
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[1], raw, &fixture.lot("R2"), "20", "100")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[1], "LABOR", "100")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[1], "OVERHEAD", "100")
            .await;
        fixture
            .item(
                object_f,
                "F",
                batch_f,
                finished,
                &fixture.periods[1],
                "70",
                "500",
                "100",
                "100",
            )
            .await;

        // G 批（二车间）：外购 R 300 + 工 50 + 费 50
        fixture
            .collect_material_with_lot(object_g, &fixture.periods[1], raw, &fixture.lot("R3"), "60", "300")
            .await;
        fixture
            .collect_element(object_g, &fixture.periods[1], "LABOR", "50")
            .await;
        fixture
            .collect_element(object_g, &fixture.periods[1], "OVERHEAD", "50")
            .await;
        fixture
            .item(
                object_g,
                "G",
                batch_f,
                finished_g,
                &fixture.periods[1],
                "50",
                "300",
                "50",
                "50",
            )
            .await;

        Chain {
            objects: vec![object_s, object_f, object_g],
            batches: vec![batch_s, batch_f],
            fixture,
        }
    }

    // ---------- 调用入口 ----------

    async fn report(
        principal: &ScopedPrincipal,
        period: &str,
        keyword: &str,
        group_by: &str,
        state: &str,
        page_size: Option<u64>,
    ) -> ReportRes {
        let filter: ReportFilter = filter_of(&ReportQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            group_by: Some(group_by.to_string()),
            state: Some(state.to_string()),
            page: None,
            page_size,
            max_depth: None,
        })
        .expect("报表筛选参数合法");
        report_of(principal, &filter).await.expect("报表查询")
    }

    fn group_of<'a>(
        res: &'a ReportRes,
        key: &str,
    ) -> &'a crate::business::erp::cost::cost_restore_report_dto::GroupRow {
        res.groups.iter().find(|group| group.key == key).unwrap_or_else(|| {
            panic!(
                "报表里没有这个分组：{key}；实际 = {:?}",
                res.groups.iter().map(|group| &group.key).collect::<Vec<_>>()
            )
        })
    }

    // ==================== 用例 ====================

    /// ① 按车间分组：A 组 = F 一行（还原后 300/180/220），B 组 = G 一行（全外购料）。
    #[tokio::test]
    async fn v1_dept_groups_are_sums_of_their_rows() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let res = report(
            &fixture.super_principal(),
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "all",
            None,
        )
        .await;

        assert!(res.cost_ready, "本期有批次");
        assert_eq!(res.group_by, "dept");
        assert_eq!(res.total, 2, "两个车间两组");
        assert_eq!(res.page_size, 50, "报表缺省一页 50 个分组");
        assert_eq!(res.basis, crate::business::erp::cost::cost_restore_report_dto::BASIS);
        assert!(res.note.contains("占比分母"), "页脚必须写清口径：{}", res.note);
        assert_eq!(res.group_options.len(), 4);

        let dept_a = group_of(&res, &fixture.dept.to_string());
        assert_eq!(dept_a.label, fixture.dept_name);
        assert_eq!(dept_a.object_count, 1);
        assert_eq!(dept_a.before.material, "500");
        assert_eq!(dept_a.before.labor, "100");
        assert_eq!(dept_a.before.overhead, "100");
        assert_eq!(dept_a.before.total, "700");
        assert_eq!(dept_a.restored.material, "300");
        assert_eq!(dept_a.restored.labor, "180");
        assert_eq!(dept_a.restored.overhead, "220");
        assert_eq!(dept_a.restored.total, "700");
        assert_eq!(dept_a.restored.unresolved, "0");
        assert_eq!(dept_a.restored.denominator, "700");
        assert_eq!(dept_a.restored_labor, "80", "从半成品里拆出来的人工 400 × 200/1000");
        assert_eq!(dept_a.restored_overhead, "120");
        assert_eq!(dept_a.semi_in_material, "400");
        assert_eq!(dept_a.semi_share.as_deref(), Some("80.00"));
        assert_eq!(dept_a.restored_rows, 1);
        assert_eq!(dept_a.partial_rows, 0);
        assert_eq!(dept_a.dominant, "material");
        assert_eq!(dept_a.check, "0");
        // 300/700 = 42.86、180/700 = 25.71、220/700 = 31.43
        assert_eq!(dept_a.restored.material_share.as_deref(), Some("42.86"));
        assert_eq!(dept_a.restored.labor_share.as_deref(), Some("25.71"));
        assert_eq!(dept_a.restored.overhead_share.as_deref(), Some("31.43"));
        assert_eq!(dept_a.share_sum.as_deref(), Some("100.00"));
        // 还原前 500/700 = 71.43 ⇒ 材料 −28.57pp、人工 +11.42pp、费 +17.14pp（三者和 = +0.01 舍入）
        assert_eq!(dept_a.shift.material_pp.as_deref(), Some("-28.57"));
        assert_eq!(dept_a.shift.labor_pp.as_deref(), Some("11.42"));
        assert_eq!(dept_a.shift.overhead_pp.as_deref(), Some("17.14"));

        let dept_b = group_of(&res, &fixture.other_dept.to_string());
        assert_eq!(dept_b.label, fixture.other_dept_name);
        assert_eq!(dept_b.object_count, 1);
        assert_eq!(dept_b.before.material, "300");
        assert_eq!(dept_b.restored.material, "300", "全外购料：还原前后一致");
        assert_eq!(dept_b.restored.total, "400");
        assert_eq!(dept_b.no_semi_rows, 1);
        assert_eq!(dept_b.dominant, "material");
        assert_eq!(dept_b.share_sum.as_deref(), Some("100.00"));
    }

    /// ② 整表合计 = 两组之和；`group_by=all` 给同一批数字（只一行）。
    #[tokio::test]
    async fn v2_totals_equal_sum_of_groups() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let res = report(
            &fixture.super_principal(),
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "all",
            None,
        )
        .await;

        // 合计：还原前 800/150/150 = 1100；还原后 600/230/270 = 1100
        assert_eq!(res.totals.object_count, 2);
        assert_eq!(res.totals.before.material, "800");
        assert_eq!(res.totals.before.labor, "150");
        assert_eq!(res.totals.before.total, "1100");
        assert_eq!(res.totals.restored.material, "600");
        assert_eq!(res.totals.restored.labor, "230");
        assert_eq!(res.totals.restored.overhead, "270");
        assert_eq!(res.totals.restored.total, "1100");
        assert_eq!(res.totals.check, "0");
        assert_eq!(res.totals.restored_rows, 1);
        assert_eq!(res.totals.no_semi_rows, 1);

        // 合计 = 分组之和（金额 / 行数）
        for group in &res.groups {
            assert_eq!(group.check, "0", "{} 不守恒", group.label);
        }
        let sum_material = res
            .groups
            .iter()
            .fold(Decimal::ZERO, |acc, group| acc + num(&group.restored.material));
        let sum_before = res
            .groups
            .iter()
            .fold(Decimal::ZERO, |acc, group| acc + num(&group.before.total));
        let sum_objects: u64 = res.groups.iter().map(|group| group.object_count).sum();
        assert_eq!(sum_material, num(&res.totals.restored.material));
        assert_eq!(sum_before, num(&res.totals.before.total));
        assert_eq!(sum_objects, res.totals.object_count);

        // 整表口径单独查一次：一行、金额与合计完全一致
        let whole = report(
            &fixture.super_principal(),
            &fixture.periods[1],
            &fixture.tag(""),
            "all",
            "all",
            None,
        )
        .await;
        assert_eq!(whole.total, 1);
        assert_eq!(whole.groups.len(), 1);
        assert_eq!(whole.groups[0].key, "ALL");
        assert_eq!(whole.groups[0].before.total, res.totals.before.total);
        assert_eq!(whole.groups[0].restored.material, res.totals.restored.material);
        assert_eq!(whole.totals.before.total, res.totals.before.total);
        // 600/1100 = 54.55、230/1100 = 20.91、270/1100 = 24.55（各自 2 位 ⇒ 合计 100.01）
        assert_eq!(whole.groups[0].restored.material_share.as_deref(), Some("54.55"));
        assert_eq!(whole.groups[0].restored.labor_share.as_deref(), Some("20.91"));
        assert_eq!(whole.groups[0].restored.overhead_share.as_deref(), Some("24.55"));
        assert_eq!(whole.groups[0].share_sum.as_deref(), Some("100.01"), "舍入差如实暴露");
    }

    /// ③ 按对象分组：逐行等于明细表（M5-27）同一行的三要素与不彻底（一张报表两个口径不能打架）。
    #[tokio::test]
    async fn v3_object_groups_reconcile_with_rollup_table() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let principal = fixture.super_principal();

        let by_object = report(&principal, &fixture.periods[1], &fixture.tag(""), "object", "all", None).await;
        assert_eq!(by_object.total, 2, "两个成本对象两组");

        let rollup_filter: RollupFilter = rollup_filter_of(&RollupQuery {
            period_code: Some(fixture.periods[1].clone()),
            keyword: Some(fixture.tag("")),
            state: Some("all".to_string()),
            page: None,
            page_size: None,
            max_depth: None,
        })
        .expect("明细表筛选参数合法");
        let rollup = rollup_table_of(&principal, &rollup_filter).await.expect("明细表查询");
        assert_eq!(rollup.total, 2);
        assert_eq!(rollup.list.len(), 2);

        for row in &rollup.list {
            let group = by_object
                .groups
                .iter()
                .find(|group| group.label == row.object_no)
                .unwrap_or_else(|| panic!("报表里没有对象 {}", row.object_no));
            assert_eq!(group.before.material, row.before.material);
            assert_eq!(group.before.labor, row.before.labor);
            assert_eq!(group.before.overhead, row.before.overhead);
            assert_eq!(group.before.total, row.before.total);
            assert_eq!(group.restored.material, row.after.material);
            assert_eq!(group.restored.labor, row.after.labor);
            assert_eq!(group.restored.overhead, row.after.overhead);
            assert_eq!(group.restored.total, row.after.total);
            assert_eq!(group.restored.unresolved, row.unresolved);
            assert_eq!(group.restored_labor, row.restored_labor);
            assert_eq!(group.restored_overhead, row.restored_overhead);
            assert_eq!(group.object_count, 1);
            assert_eq!(group.check, "0");
            assert!(group.hint.as_deref().unwrap_or("").contains("批号"), "对象行带副标");
        }
        // 对象组的金额之和 = 整表合计（同一批加法）
        let sum = by_object
            .groups
            .iter()
            .fold(Decimal::ZERO, |acc, group| acc + num(&group.restored.total));
        assert_eq!(sum, num(&by_object.totals.restored.total));
    }

    /// ④ 状态筛选只留命中的行；`page_size=1` 时合计仍是整表（不受分页影响）。
    #[tokio::test]
    async fn v4_state_filter_and_paging() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let principal = fixture.super_principal();

        let only_no_semi = report(
            &principal,
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "no_semi",
            None,
        )
        .await;
        assert_eq!(only_no_semi.total, 1, "只有二车间那个全外购料的对象");
        assert_eq!(only_no_semi.totals.before.total, "400");
        assert_eq!(only_no_semi.totals.object_count, 1);

        let only_restored = report(
            &principal,
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "restored",
            None,
        )
        .await;
        assert_eq!(only_restored.total, 1);
        assert_eq!(only_restored.totals.restored.labor, "180");

        let only_partial = report(
            &principal,
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "partial",
            None,
        )
        .await;
        assert_eq!(only_partial.total, 0, "本夹具没有还原不彻底的行");
        assert_eq!(only_partial.totals.object_count, 0);
        assert_eq!(only_partial.totals.restored.material, "0");
        assert_eq!(only_partial.totals.restored.denominator, "0");
        assert_eq!(only_partial.totals.share_sum, None, "空表不画假 0.00%");

        // 分页：一页一组，但合计是整表
        let paged = report(
            &principal,
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "all",
            Some(1),
        )
        .await;
        assert_eq!(paged.page_size, 1);
        assert_eq!(paged.groups.len(), 1, "只有当前页");
        assert_eq!(paged.total, 2, "分组总数仍是 2");
        assert_eq!(paged.totals.object_count, 2, "合计是整表");
        assert_eq!(paged.totals.before.total, "1100");
    }

    /// ⑤ 数据范围：二车间的人只看得到自己的组，A 组不出现，也不报 403。
    #[tokio::test]
    async fn v5_other_dept_scope_sees_only_itself() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let scoped = report(
            &fixture.other_dept_principal(),
            &fixture.periods[1],
            &fixture.tag(""),
            "dept",
            "all",
            None,
        )
        .await;

        assert_eq!(scoped.total, 1, "只有二车间一组");
        assert_eq!(scoped.groups[0].key, fixture.other_dept.to_string());
        assert_eq!(scoped.groups[0].before.total, "400");
        assert!(
            scoped.groups.iter().all(|group| group.key != fixture.dept.to_string()),
            "不许带出一车间的组"
        );
        assert_eq!(scoped.totals.before.total, "400", "合计也只算看得见的");
    }

    /// ⑥ 清场后本夹具零残留。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        assert!(fixture.residue().await > 0, "夹具本身有数据");
        fixture.cleanup(&chain.objects, &chain.batches).await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }
}
