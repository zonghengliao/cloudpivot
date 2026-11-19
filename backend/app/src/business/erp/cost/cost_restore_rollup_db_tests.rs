//! 成本还原「逐层还原到料工费」（M5-27）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m527_dev \
//!   cargo test --lib cost_restore_rollup -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_three_level_chain_restores_to_purchased_material`] | 三层链还原后 = 外购料 220 / 人工 196 / 制造费用 284；逐层台账 3 层；半成品明细 2 条；守恒 |
//! | 2 | [`v2_purchased_only_row_needs_no_restore`] | 全外购料 ⇒ `no_semi`、还原前后一致、不彻底 0 |
//! | 3 | [`v3_semi_without_structure_is_partial`] | 半成品没计算 ⇒ `partial`，金额停在半成品形态（**不当外购料**） |
//! | 4 | [`v4_period_average_semi_stops_at_that_level`] | 批号对不上 ⇒ 工费照样拆出、材料部分留在不彻底里 |
//! | 5 | [`v5_cycle_is_partial_and_terminates`] | A 用 B、B 用 A ⇒ `partial`、不死循环 |
//! | 6 | [`v6_depth_limit_only_cuts_the_unfinished_part`] | `max_depth=1` ⇒ `partial`；同 `max_depth=6` ⇒ `restored` |
//! | 7 | [`v7_other_workshop_sees_nothing`] | 数据范围：别车间看不到本车间的行 |
//! | 8 | [`v8_totals_are_whole_table_and_filter_narrows`] | `page_size=1` 时合计仍是整表；`restored` / `partial` / `no_semi` 分档 |
//! | 9 | [`zz_teardown_leaves_zero_residue`] | 本夹具按对象号 / 批号前缀清场，零残留 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `1800 + slot % 80`，期间取该年 01/02/03）——
//!   与 M5-26 的 `1900 + slot`、M5-23 / M5-22 的 `2000 + slot` 错开，避免撞 `uk_biz_cost_period`；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，列表断言一律用 `keyword` 收窄到本夹具；
//! * 计算单用 `cost_period_item_dto::compose`（与真 calc 同一段代码）算好再落库 ——
//!   「结构 = 完工成本 × 比例」这条断言不能靠在测试里手抄数字来成立。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{self as item_dto, BegCost, ItemInput, OutputInput};
    use crate::business::erp::cost::cost_restore_rollup_dto::{RollupFilter, RollupQuery, RollupRes, Row};
    use crate::business::erp::cost::cost_restore_rollup_handler::{filter_of, rollup_table_of};
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
        /// 三个连续期间 01 / 02 / 03（二级半成品 → 半成品 → 成品）。
        periods: [String; 3],
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        unit: Uuid,
        warehouse: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 1800 + (slot % 80) as i32;
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
            ];
            Self::reset().await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &format!("M527-DEPT2-{suffix}"),
                &format!("M5-27 夹具第二车间 {suffix}"),
            )
            .await;
            let unit = crate::test_seed::ensure_unit().await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M527W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-27 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
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
            format!("CB-M527-{}", self.tag(name))
        }

        fn lot(&self, name: &str) -> String {
            format!("LOT-M527-{}", self.tag(name))
        }

        /// 造物料（`RAW` / `SEMI` / `FINISHED`）。
        async fn material(&self, kind: &str, name: &str) -> Uuid {
            crate::test_seed::ensure_material_kind(
                &format!("M527-{kind}-{}-{name}", crate::test_seed::last_chars(&self.suffix, 6)),
                &format!("M5-27 夹具 {kind} {name} {}", self.suffix),
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
            completed: &str,
            wip: &str,
            pct: &str,
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
                             ?::text::numeric, ?::text::numeric, 'ONE_TIME', \
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
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 造一条**手工**料行（没有来源单据 ⇒ 没有批号，走同月平均结构）。
        async fn collect_material(&self, object: Uuid, period: &str, material: Uuid, qty: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                line_no, occur_date, material_id, qty, amount, \
                                                remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'MANUAL', 0, ?::text::date, ?::uuid, \
                         ?::text::numeric, ?::text::numeric, 'M5-27 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::Text(format!("{period}-10")),
                    Value::from(&material),
                    Value::Text(qty.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造手工料行夹具");
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
        ) -> Uuid {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let out_no = format!("OUT-M527-{}-{seq}", crate::test_seed::last_chars(&self.suffix, 6));
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
                         'M5-27 夹具', now(), now())"
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
            stock_out
        }

        /// 造一条工 / 费归集行（不许挂物料）。
        async fn collect_element(&self, object: Uuid, period: &str, element: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                line_no, occur_date, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, ?::text::numeric, \
                         'M5-27 夹具', now(), now())"
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
        async fn batch(&self, period: &str, status: &str, tag: &str) -> Uuid {
            let id = Uuid::now_v7();
            let needs_approval = matches!(status, "SETTLED" | "REVERSED" | "CLOSED");
            let settled = if needs_approval { "now()" } else { "NULL" };
            let approval = if needs_approval { "?::uuid" } else { "NULL" };
            let voucher = if needs_approval { "?::uuid" } else { "NULL" };
            let reversed_at = if status == "REVERSED" { "now()" } else { "NULL" };
            let reason = if status == "REVERSED" { "?::text" } else { "NULL" };
            let mut params = vec![
                Value::from(&id),
                Value::Text(period.to_string()),
                Value::Text(format!(
                    "CS-M527-{tag}-{}",
                    crate::test_seed::last_chars(&self.suffix, 8)
                )),
                Value::Text(status.to_string()),
            ];
            if needs_approval {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            if status == "REVERSED" {
                params.push(Value::Text("M5-27 夹具红冲".to_string()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period (id, period_code, batch_no, status, \
                                                  approval_instance_id, settled_at, finish_voucher_id, \
                                                  reversed_at, reverse_reason, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?, {approval}, {settled}, {voucher}, {reversed_at}, {reason}, now(), now())"
                ),
                params,
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
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M527-%')",
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-27 夹具'",
                "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-M527-%'",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M527-%'",
                "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M527-%'",
            ] {
                let _ = db().exec_sql(statement.to_string(), vec![]).await;
            }
        }

        /// 清场（FK 顺序：计算单 → 归集）。
        async fn cleanup(&self, objects: &[Uuid]) {
            if objects.is_empty() {
                return;
            }
            let list = objects
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            for statement in [
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
            ] {
                db().exec_sql(statement, vec![]).await.expect("清场（依赖行）");
            }
        }

        /// 本夹具残留行数（清场断言用：按对象号 / 批号前缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE 'CB-M527-%') + \
                        (SELECT count(*) FROM biz_cost_period WHERE batch_no LIKE 'CS-M527-%') + \
                        (SELECT count(*) FROM biz_cost_collect WHERE remark = 'M5-27 夹具')",
                vec![],
            )
            .await
            .expect("残留统计")
        }
    }

    // ---------- 调用入口 ----------

    async fn table(
        principal: &ScopedPrincipal,
        period: &str,
        state: &str,
        keyword: &str,
        max_depth: Option<u32>,
        page_size: Option<u64>,
    ) -> RollupRes {
        let filter: RollupFilter = filter_of(&RollupQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            state: Some(state.to_string()),
            page: None,
            page_size,
            max_depth,
        })
        .expect("筛选参数合法");
        rollup_table_of(principal, &filter).await.expect("还原表查询")
    }

    fn row_of<'a>(res: &'a RollupRes, object_no: &str) -> &'a Row {
        res.list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| {
                panic!(
                    "表里没有这个对象：{object_no}；实际 = {:?}",
                    res.list.iter().map(|row| &row.object_no).collect::<Vec<_>>()
                )
            })
    }

    /// 每行必须守恒：还原后合计 + 不彻底 = 还原前合计。
    fn assert_balanced(row: &Row) {
        let before = num(&row.before.total);
        let after = num(&row.after.total);
        let unresolved = num(&row.unresolved);
        assert_eq!(
            after + unresolved,
            before,
            "{} 守恒失败：还原后 {after} + 不彻底 {unresolved} ≠ 还原前 {before}",
            row.object_no
        );
        let sum = num(&row.before.material) + num(&row.before.labor) + num(&row.before.overhead);
        assert_eq!(sum, before, "还原前三要素加不齐");
        let sum = num(&row.after.material) + num(&row.after.labor) + num(&row.after.overhead);
        assert_eq!(sum, after, "还原后三要素加不齐");
        // 逐层台账每行也自洽
        for flow in &row.flows {
            let sum = num(&flow.purchased)
                + num(&flow.labor)
                + num(&flow.overhead)
                + num(&flow.carried_down)
                + num(&flow.unresolved);
            assert_eq!(sum, num(&flow.imputed), "第 {} 层台账不自洽", flow.level);
        }
    }

    fn material_code(fixture: &Fixture, kind: &str, name: &str) -> String {
        format!(
            "M527-{kind}-{}-{name}",
            crate::test_seed::last_chars(&fixture.suffix, 6)
        )
    }

    /// `R(外购) → S2 → S → F` 这条三层链。
    ///
    /// ```text
    /// R(外购 5/件)
    ///   └─ S2 批：R 100 件 = 500 + 工 100 + 费 400 = 1000，完工 50 → 结构 500/100/400
    ///        └─ S 批：S2 20 件 = 400 + R 20 件 = 100 + 工 200 + 费 300 = 1000，完工 100 → 结构 500/200/300
    ///             └─ F 批：S 40 件 = 400 + R 20 件 = 100 + 工 100 + 费 100 = 700，完工 70
    /// ```
    struct Chain {
        fixture: Fixture,
        semi: String,
        s2: String,
        raw: String,
        batch_f: Uuid,
        objects: Vec<Uuid>,
    }

    async fn build_chain() -> Chain {
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R").await;
        let s2 = fixture.material("SEMI", "S2").await;
        let semi = fixture.material("SEMI", "S").await;
        let finished = fixture.material("FINISHED", "F").await;

        let batch_s2 = fixture.batch(&fixture.periods[0], "DRAFT", "P1").await;
        let batch_s = fixture.batch(&fixture.periods[1], "DRAFT", "P2").await;
        let batch_f = fixture.batch(&fixture.periods[2], "DRAFT", "P3").await;

        let lot_s2 = fixture.lot("S2");
        let lot_s = fixture.lot("S");
        let object_s2 = fixture.object("S2", s2, Some(&lot_s2), "50", "0", "0").await;
        let object_s = fixture.object("S", semi, Some(&lot_s), "100", "0", "0").await;
        let object_f = fixture
            .object("F", finished, Some(&fixture.lot("F")), "70", "0", "0")
            .await;

        // S2 批：外购原料 500 + 工 100 + 费 400
        fixture
            .collect_material_with_lot(object_s2, &fixture.periods[0], raw, &fixture.lot("R1"), "100", "500")
            .await;
        fixture
            .collect_element(object_s2, &fixture.periods[0], "LABOR", "100")
            .await;
        fixture
            .collect_element(object_s2, &fixture.periods[0], "OVERHEAD", "400")
            .await;
        fixture
            .item(
                object_s2,
                "S2",
                batch_s2,
                s2,
                &fixture.periods[0],
                "50",
                "500",
                "100",
                "400",
            )
            .await;

        // S 批：S2 400（批号对得上）+ 外购原料 100 + 工 200 + 费 300
        fixture
            .collect_material_with_lot(object_s, &fixture.periods[1], s2, &lot_s2, "20", "400")
            .await;
        fixture
            .collect_material_with_lot(object_s, &fixture.periods[1], raw, &fixture.lot("R2"), "20", "100")
            .await;
        fixture
            .collect_element(object_s, &fixture.periods[1], "LABOR", "200")
            .await;
        fixture
            .collect_element(object_s, &fixture.periods[1], "OVERHEAD", "300")
            .await;
        fixture
            .item(
                object_s,
                "S",
                batch_s,
                semi,
                &fixture.periods[1],
                "100",
                "500",
                "200",
                "300",
            )
            .await;

        // F 批：S 400（批号对得上）+ 外购原料 100 + 工 100 + 费 100
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[2], semi, &lot_s, "40", "400")
            .await;
        fixture
            .collect_material_with_lot(object_f, &fixture.periods[2], raw, &fixture.lot("R3"), "20", "100")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[2], "LABOR", "100")
            .await;
        fixture
            .collect_element(object_f, &fixture.periods[2], "OVERHEAD", "100")
            .await;
        fixture
            .item(
                object_f,
                "F",
                batch_f,
                finished,
                &fixture.periods[2],
                "70",
                "500",
                "100",
                "100",
            )
            .await;

        Chain {
            raw: material_code(&fixture, "RAW", "R"),
            s2: material_code(&fixture, "SEMI", "S2"),
            semi: material_code(&fixture, "SEMI", "S"),
            objects: vec![object_s2, object_s, object_f],
            batch_f,
            fixture,
        }
    }

    // ==================== 用例 ====================

    /// ① 三层链：还原后 = 外购料 220 / 人工 196 / 制造费用 284；台账 3 层、半成品 2 条、守恒。
    #[tokio::test]
    async fn v1_three_level_chain_restores_to_purchased_material() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F"),
            None,
            None,
        )
        .await;

        assert_eq!(res.total, 1, "关键字收窄到本夹具的成品对象");
        assert!(res.cost_ready, "本期有批次");
        let row = row_of(&res, &fixture.object_no("F"));

        // 还原前：材料 500（含半成品 400）+ 本期人工 100 + 本期制造费用 100
        assert_eq!(num(&row.before.material), dec("500"));
        assert_eq!(num(&row.before.labor), dec("100"), "人工来自本期归集");
        assert_eq!(num(&row.before.overhead), dec("100"));
        assert_eq!(num(&row.before.total), dec("700"));
        assert_eq!(num(&row.semi_in_material), dec("400"));
        assert_eq!(num(&row.purchased_in_material), dec("100"));
        assert_eq!(row.semi_share.as_deref(), Some("80.00"));

        // 还原后：外购料 220 / 人工 196 / 制造费用 284（见夹具注释的推导）
        assert_eq!(
            num(&row.after.material),
            dec("220"),
            "100 + 40（S 里的 R）+ 80（S2 里的 R）"
        );
        assert_eq!(
            num(&row.after.labor),
            dec("196"),
            "80（S 结构）+ 16（S2 结构）+ 本期 100"
        );
        assert_eq!(num(&row.after.overhead), dec("284"), "120 + 64 + 本期 100");
        assert_eq!(num(&row.after.total), dec("700"), "还原不动总额");
        assert_eq!(num(&row.unresolved), Decimal::ZERO);
        assert_eq!(row.state, "restored", "{}", row.state_label);
        assert!(row.issues.is_empty(), "{:?}", row.issues);
        assert_balanced(row);

        // 还原差额：材料 -280，人工 +96，费 +184，合计 0
        assert_eq!(num(&row.delta.material), dec("-280"));
        assert_eq!(num(&row.delta.labor), dec("96"));
        assert_eq!(num(&row.delta.overhead), dec("184"));
        assert_eq!(num(&row.delta.total), Decimal::ZERO);
        assert_eq!(num(&row.restored_labor), dec("96"));
        assert_eq!(num(&row.restored_overhead), dec("184"));

        // 逐层台账：三层，第 1 层 = 本期材料投入
        assert_eq!(row.depth, 3);
        assert_eq!(row.flows.len(), 3);
        assert_eq!(num(&row.flows[0].imputed), dec("500"));
        assert_eq!(num(&row.flows[0].purchased), dec("100"));
        assert_eq!(num(&row.flows[0].labor), dec("80"));
        assert_eq!(num(&row.flows[0].overhead), dec("120"));
        assert_eq!(num(&row.flows[0].carried_down), dec("200"));
        assert_eq!(num(&row.flows[1].imputed), dec("200"), "第 2 层是上一层的材料部分");
        assert_eq!(num(&row.flows[1].purchased), dec("40"));
        assert_eq!(num(&row.flows[1].labor), dec("16"));
        assert_eq!(num(&row.flows[1].overhead), dec("64"));
        assert_eq!(num(&row.flows[1].carried_down), dec("80"));
        assert_eq!(num(&row.flows[2].imputed), dec("80"));
        assert_eq!(num(&row.flows[2].purchased), dec("80"));

        // 半成品明细：S（第 1 层）与 S2（第 2 层）
        assert_eq!(row.semis.len(), 2);
        assert_eq!(row.semis[0].material_code.as_deref(), Some(chain.semi.as_str()));
        assert_eq!(num(&row.semis[0].imputed), dec("400"));
        assert_eq!(num(&row.semis[0].elements.material), dec("120"));
        assert_eq!(num(&row.semis[0].elements.labor), dec("96"));
        assert_eq!(num(&row.semis[0].elements.overhead), dec("184"));
        assert!(!row.semis[0].stopped, "S 钻到底了");
        assert_eq!(row.semis[1].material_code.as_deref(), Some(chain.s2.as_str()));
        assert_eq!(num(&row.semis[1].imputed), dec("160"), "被卷积的是父层拨下来的份额");
        assert_eq!(num(&row.semis[1].elements.material), dec("80"));
        // 外购料不出现在半成品明细里
        assert!(
            row.semis
                .iter()
                .all(|item| item.material_code.as_deref() != Some(chain.raw.as_str()))
        );

        // 整表合计
        assert_eq!(res.totals.row_count, 1);
        assert_eq!(res.totals.restored_row_count, 1);
        assert_eq!(num(&res.totals.check), Decimal::ZERO, "合计也要守恒");
        assert_eq!(num(&res.totals.after_material), dec("220"));
        assert_eq!(num(&res.totals.semi_in_material), dec("400"));
        assert_eq!(res.totals.max_depth, 3);
        assert!(res.formula_restore.contains("尾差进制造费用"));

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ② 全外购料 ⇒ `no_semi`、还原前后一致。
    #[tokio::test]
    async fn v2_purchased_only_row_needs_no_restore() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R4").await;
        let finished = fixture.material("FINISHED", "F4").await;
        let batch = fixture.batch(&fixture.periods[0], "DRAFT", "P1F4").await;
        let object = fixture
            .object("F4", finished, Some(&fixture.lot("F4")), "10", "0", "0")
            .await;
        fixture
            .collect_material(object, &fixture.periods[0], raw, "10", "250")
            .await;
        fixture
            .collect_element(object, &fixture.periods[0], "LABOR", "50")
            .await;
        fixture
            .collect_element(object, &fixture.periods[0], "OVERHEAD", "50")
            .await;
        fixture
            .item(
                object,
                "F4",
                batch,
                finished,
                &fixture.periods[0],
                "10",
                "250",
                "50",
                "50",
            )
            .await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[0],
            "all",
            &fixture.tag("F4"),
            None,
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("F4"));
        assert_eq!(row.state, "no_semi");
        assert_eq!(num(&row.before.material), dec("250"));
        assert_eq!(num(&row.after.material), dec("250"), "全是外购料：材料不缩水");
        assert_eq!(num(&row.after.labor), dec("50"));
        assert_eq!(num(&row.after.overhead), dec("50"));
        assert_eq!(num(&row.unresolved), Decimal::ZERO);
        assert!(row.semis.is_empty());
        assert_eq!(row.flows.len(), 1, "一层到底");
        assert!(row.issues.is_empty());
        assert_balanced(row);

        fixture.cleanup(&[object]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ③ 半成品没计算 ⇒ `partial`，金额停在半成品形态（不当外购料）。
    #[tokio::test]
    async fn v3_semi_without_structure_is_partial() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R5").await;
        let uncosted = fixture.material("SEMI", "SX").await;
        let finished = fixture.material("FINISHED", "FX").await;
        let batch = fixture.batch(&fixture.periods[0], "DRAFT", "P1X").await;

        // 产出 SX 的成本对象：有对象、没有计算单
        let producer = fixture
            .object("SX", uncosted, Some(&fixture.lot("SX")), "0", "0", "0")
            .await;
        fixture
            .collect_material(producer, &fixture.periods[0], raw, "10", "100")
            .await;
        let object = fixture
            .object("FX", finished, Some(&fixture.lot("FX")), "10", "0", "0")
            .await;
        fixture
            .collect_material(object, &fixture.periods[0], uncosted, "10", "300")
            .await;
        fixture
            .item(
                object,
                "FX",
                batch,
                finished,
                &fixture.periods[0],
                "10",
                "300",
                "0",
                "0",
            )
            .await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[0],
            "all",
            &fixture.tag("FX"),
            None,
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("FX"));
        assert_eq!(row.state, "partial", "{}", row.state_label);
        assert_eq!(num(&row.after.material), Decimal::ZERO, "缺结构不当外购料");
        assert_eq!(num(&row.unresolved), dec("300"), "整笔停在半成品形态");
        assert_eq!(num(&row.after.total), Decimal::ZERO);
        assert_eq!(num(&row.before.total), dec("300"));
        assert_balanced(row);
        assert!(row.semis[0].stopped);
        assert!(
            row.warnings.iter().any(|text| text.contains("取不到完工成本结构")),
            "{:?}",
            row.warnings
        );
        assert_eq!(res.totals.partial_row_count, 1);

        fixture.cleanup(&[object, producer]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ④ 批号对不上 ⇒ 同月平均结构：工费照样拆出，材料部分留在不彻底里。
    #[tokio::test]
    async fn v4_period_average_semi_stops_at_that_level() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let finished = fixture.material("FINISHED", "F6").await;
        let batch = chain.batch_f;
        let object = fixture
            .object("F6", finished, Some(&fixture.lot("F6")), "40", "0", "0")
            .await;
        let semi_material: Uuid = db()
            .scalar_with_params(
                "SELECT product_id FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&chain.objects[1])],
            )
            .await
            .expect("取半成品物料 id");
        // 手工料行：没有来源单据 ⇒ 没有批号
        fixture
            .collect_material(object, &fixture.periods[2], semi_material, "40", "400")
            .await;
        fixture
            .item(
                object,
                "F6",
                batch,
                finished,
                &fixture.periods[2],
                "40",
                "400",
                "0",
                "0",
            )
            .await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F6"),
            None,
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("F6"));
        assert_eq!(row.state, "partial", "材料部分没得钻，但工费拆出来了");
        // 结构仍是 S 批的 500/200/300 ⇒ 400 × 比例 = (200, 80, 120)
        assert_eq!(num(&row.after.labor), dec("80"));
        assert_eq!(num(&row.after.overhead), dec("120"));
        assert_eq!(num(&row.after.material), Decimal::ZERO, "同月平均不下钻");
        assert_eq!(num(&row.unresolved), dec("200"));
        assert_balanced(row);
        assert_eq!(row.semis[0].source.as_deref(), Some("period_avg"));
        assert!(row.semis[0].stopped);

        fixture.cleanup(&chain.objects).await;
        fixture.cleanup(&[object]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑤ A 用 B、B 用 A ⇒ `partial`、不死循环。
    #[tokio::test]
    async fn v5_cycle_is_partial_and_terminates() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let mat_a = fixture.material("SEMI", "CA").await;
        let mat_b = fixture.material("SEMI", "CB").await;
        let batch = fixture.batch(&fixture.periods[0], "DRAFT", "P1C").await;
        let object_a = fixture
            .object("CA", mat_a, Some(&fixture.lot("CA")), "10", "0", "0")
            .await;
        let object_b = fixture
            .object("CB", mat_b, Some(&fixture.lot("CB")), "10", "0", "0")
            .await;
        fixture
            .collect_material_with_lot(object_a, &fixture.periods[0], mat_b, &fixture.lot("CB"), "10", "100")
            .await;
        fixture
            .collect_material_with_lot(object_b, &fixture.periods[0], mat_a, &fixture.lot("CA"), "10", "90")
            .await;
        fixture
            .item(object_a, "CA", batch, mat_a, &fixture.periods[0], "10", "100", "0", "0")
            .await;
        fixture
            .item(object_b, "CB", batch, mat_b, &fixture.periods[0], "10", "90", "0", "0")
            .await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[0],
            "all",
            &fixture.tag("CA"),
            Some(6),
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("CA"));
        assert_eq!(row.state, "partial", "闭环 ⇒ 有金额停在半成品形态");
        assert!(num(&row.unresolved) > Decimal::ZERO);
        assert_balanced(row);
        assert!(row.depth <= 3, "闭环不许越钻越深（实际 {}）", row.depth);

        fixture.cleanup(&[object_a, object_b]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑥ `max_depth=1` ⇒ `partial`（第一层的半成品没拆开）；`max_depth=6` ⇒ `restored`。
    #[tokio::test]
    async fn v6_depth_limit_only_cuts_the_unfinished_part() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;

        let shallow = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F"),
            Some(1),
            None,
        )
        .await;
        let shallow_row = row_of(&shallow, &fixture.object_no("F"));
        assert_eq!(shallow_row.state, "partial", "第 1 层的半成品没拆开");
        assert_eq!(shallow_row.depth, 1);
        // 只剩第 1 层能拆：材料 100（外购）+ 人工 80 + 费 120，材料部分 200 停在半成品形态
        assert_eq!(num(&shallow_row.after.material), dec("100"));
        assert_eq!(num(&shallow_row.after.labor), dec("180"), "80 还原 + 100 本期");
        assert_eq!(num(&shallow_row.after.overhead), dec("220"), "120 还原 + 100 本期");
        assert_eq!(num(&shallow_row.unresolved), dec("200"));
        assert_balanced(shallow_row);
        assert!(shallow_row.warnings.iter().any(|text| text.contains("截断")));

        let deep = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F"),
            Some(6),
            None,
        )
        .await;
        let deep_row = row_of(&deep, &fixture.object_no("F"));
        assert_eq!(deep_row.state, "restored");
        assert_eq!(num(&deep_row.unresolved), Decimal::ZERO);
        assert_eq!(num(&deep_row.after.material), dec("220"));

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑦ 数据范围：别车间用户看不到本车间的行。
    #[tokio::test]
    async fn v7_other_workshop_sees_nothing() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;

        let res = table(
            &fixture.other_dept_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F"),
            None,
            None,
        )
        .await;
        assert_eq!(res.total, 0, "看不见的对象不出现在表里（也不报 403）");
        assert!(res.list.is_empty());
        assert_eq!(num(&res.totals.before_total), Decimal::ZERO);

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑧ 合计是整张表（`page_size = 1` 时仍全表）；`restored` / `partial` / `no_semi` 分档。
    #[tokio::test]
    async fn v8_totals_are_whole_table_and_filter_narrows() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        // 再加一个「缺结构」的行（partial）与一个「全外购料」的行（no_semi）
        let uncosted = fixture.material("SEMI", "SU").await;
        let finished = fixture.material("FINISHED", "FU").await;
        let raw = fixture.material("RAW", "RU").await;
        let batch = chain.batch_f;
        let producer = fixture
            .object("SU", uncosted, Some(&fixture.lot("SU")), "0", "0", "0")
            .await;
        let partial_object = fixture
            .object("FU", finished, Some(&fixture.lot("FU")), "10", "0", "0")
            .await;
        fixture
            .collect_material(partial_object, &fixture.periods[2], uncosted, "10", "200")
            .await;
        fixture
            .item(
                partial_object,
                "FU",
                batch,
                finished,
                &fixture.periods[2],
                "10",
                "200",
                "0",
                "0",
            )
            .await;
        let no_semi_object = fixture
            .object("F7", finished, Some(&fixture.lot("F7")), "10", "0", "0")
            .await;
        fixture
            .collect_material(no_semi_object, &fixture.periods[2], raw, "10", "80")
            .await;
        fixture
            .item(
                no_semi_object,
                "F7",
                batch,
                finished,
                &fixture.periods[2],
                "10",
                "80",
                "0",
                "0",
            )
            .await;

        let keyword = "CB-M527-";
        let all = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            keyword,
            None,
            Some(1),
        )
        .await;
        assert_eq!(all.page_size, 1);
        assert_eq!(all.list.len(), 1, "当前页只有一行");
        assert_eq!(all.total, 3, "筛选后三行");
        assert_eq!(all.totals.row_count, 3, "合计是整张表，不受分页影响");
        assert_eq!(all.totals.restored_row_count, 1);
        assert_eq!(all.totals.partial_row_count, 1);
        assert_eq!(all.totals.no_semi_row_count, 1);
        assert_eq!(
            num(&all.totals.check),
            Decimal::ZERO,
            "整表也要守恒（还原后 + 不彻底 = 还原前）"
        );
        // 还原前合计 = 700（成品）+ 200（缺结构）+ 80（全外购料）= 980
        assert_eq!(num(&all.totals.before_total), dec("980"));
        assert_eq!(num(&all.totals.after_total), dec("780"), "缺结构的 200 停在半成品形态");
        assert_eq!(num(&all.totals.unresolved), dec("200"));

        let restored = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "restored",
            keyword,
            None,
            None,
        )
        .await;
        assert_eq!(restored.total, 1);
        assert_eq!(restored.list[0].object_no, fixture.object_no("F"));

        let partial = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "partial",
            keyword,
            None,
            None,
        )
        .await;
        assert_eq!(partial.total, 1);
        assert_eq!(partial.list[0].object_no, fixture.object_no("FU"));

        let no_semi = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "no_semi",
            keyword,
            None,
            None,
        )
        .await;
        assert_eq!(no_semi.total, 1);
        assert_eq!(no_semi.list[0].object_no, fixture.object_no("F7"));

        fixture.cleanup(&chain.objects).await;
        fixture.cleanup(&[partial_object, no_semi_object, producer]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑨ 清场断言（前面每个用例都自己清了；这里兜一次底，防止漏清污染后续卡）。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        Fixture::reset().await;
        let fixture = Fixture::build().await;
        fixture.cleanup(&[]).await;
        assert_eq!(
            fixture.residue().await,
            0,
            "M5-27 夹具行（CB-M527- / CS-M527- / 备注 M5-27 夹具）必须为 0"
        );
    }
}
