//! 成本还原「半成品成本结构表」（M5-26）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m526_dev \
//!   cargo test --lib cost_restore_structure -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_three_level_structure_is_layered`] | 成品 → 半成品 → 二级半成品 → 外购料：逐层金额 / 结构比例 / 批号命中 / 每层三要素合计 = 该层金额 |
//! | 2 | [`v2_lot_miss_falls_back_to_period_average`] | 领料行没有批号 ⇒ 同月平均结构、**不下钻**、给出提醒 |
//! | 3 | [`v3_semi_without_calculation_is_no_structure`] | 自制半成品但产出批次没计算 ⇒ `is_semi = true` + `elements = null` + 行状态 `no_structure`（**不当外购料**） |
//! | 4 | [`v4_purchased_only_row_is_flat`] | 全是外购料 ⇒ `no_semi`、一层到底、金额全额进材料 |
//! | 5 | [`v5_cycle_is_cut_and_terminates`] | A 用 B、B 用 A ⇒ 闭环截断、行状态 `cycle`、不死循环 |
//! | 6 | [`v6_depth_limit_cuts_only_when_needed`] | `max_depth=1` 截断并给 `DEPTH_CUT`；`max_depth=6` 同一行**不**误报截断 |
//! | 7 | [`v7_other_workshop_sees_nothing`] | 数据范围：别车间用户看不到本车间的行（不出现，不是 403 探测口） |
//! | 8 | [`v8_totals_are_whole_table_and_filters_narrow`] | 合计是整张表（`page_size = 1` 时 `row_count = 2`）；状态筛选 / 关键字收窄 |
//! | 9 | [`v9_reversed_producer_period_has_no_structure`] | 产出批次被红冲 ⇒ 结构取不到（`no_structure`），不拿作废的账拆钱 |
//! | 10 | [`zz_teardown_leaves_zero_residue`] | 本夹具按对象号 / 批号前缀清场，零残留 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `1900 + slot % 80`，期间取该年 01/02/03 三个月）——
//!   与 M5-23 / M5-22 的 `2000 + slot` 年段错开，避免撞 `uk_biz_cost_period`；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**；
//! * 计算单用 `cost_period_item_dto::compose`（与真 calc 同一段代码）算好再落库 ——
//!   「结构 = 完工成本 × 比例」这条断言不能靠在测试里手抄数字来成立。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{self as item_dto, BegCost, ItemInput, OutputInput};
    use crate::business::erp::cost::cost_restore_structure_dto::{
        self as dto, NodeView, RestoreQuery, RestoreRes, Row,
    };
    use crate::business::erp::cost::cost_restore_structure_handler::{filter_of, structure_table_of};
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

    /// 出参里的金额 / 数量是字符串：按 Decimal 比，避免「1000 vs 1000.0000」这种标度假红。
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
            let year = 1900 + (slot % 80) as i32;
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
                &format!("M526-DEPT2-{suffix}"),
                &format!("M5-26 夹具第二车间 {suffix}"),
            )
            .await;
            let unit = crate::test_seed::ensure_unit().await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M526W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-26 夹具仓 {suffix}"),
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

        /// 本夹具的标签（对象号 / 批号里带它，列表断言靠它收窄）。
        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", crate::test_seed::last_chars(&self.suffix, 8))
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M526-{}", self.tag(name))
        }

        fn lot(&self, name: &str) -> String {
            format!("LOT-M526-{}", self.tag(name))
        }

        /// 造物料（`RAW` / `SEMI` / `FINISHED`）。
        async fn material(&self, kind: &str, name: &str) -> Uuid {
            crate::test_seed::ensure_material_kind(
                &format!("M526-{kind}-{}-{name}", crate::test_seed::last_chars(&self.suffix, 6)),
                &format!("M5-26 夹具 {kind} {name} {}", self.suffix),
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
                         ?::text::numeric, ?::text::numeric, 'M5-26 夹具', now(), now())"
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
            let out_no = format!("OUT-M526-{}-{seq}", crate::test_seed::last_chars(&self.suffix, 6));
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
                         'M5-26 夹具', now(), now())"
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
                         'M5-26 夹具', now(), now())"
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

        /// 造成本核算批次（`REVERSED` 要挂审批 + 结算 + 凭证 + 红冲原因，库层硬约束）。
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
                    "CS-M526-{tag}-{}",
                    crate::test_seed::last_chars(&self.suffix, 8)
                )),
                Value::Text(status.to_string()),
            ];
            if needs_approval {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            if status == "REVERSED" {
                params.push(Value::Text("M5-26 夹具红冲".to_string()));
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
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M526-%')",
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-26 夹具'",
                "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-M526-%'",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M526-%'",
                "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M526-%'",
            ] {
                let _ = db().exec_sql(statement.to_string(), vec![]).await;
            }
        }

        /// 清场（FK 顺序：计算单 → 归集 → 领料单 → 期间批次 → 对象）。
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
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE 'CB-M526-%') + \
                        (SELECT count(*) FROM biz_cost_period WHERE batch_no LIKE 'CS-M526-%') + \
                        (SELECT count(*) FROM biz_cost_collect WHERE remark = 'M5-26 夹具')",
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
    ) -> RestoreRes {
        let filter = filter_of(&RestoreQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            state: Some(state.to_string()),
            page: None,
            page_size,
            max_depth,
        })
        .expect("筛选参数合法");
        structure_table_of(principal, &filter).await.expect("结构表查询")
    }

    fn row_of<'a>(res: &'a RestoreRes, object_no: &str) -> &'a Row {
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

    fn find<'a>(nodes: &'a [NodeView], code: &str) -> &'a NodeView {
        nodes
            .iter()
            .find(|node| node.material_code.as_deref() == Some(code))
            .unwrap_or_else(|| {
                panic!(
                    "这一层没有该物料：{code}；实际 = {:?}",
                    nodes
                        .iter()
                        .filter_map(|node| node.material_code.clone())
                        .collect::<Vec<_>>()
                )
            })
    }

    fn material_code(fixture: &Fixture, kind: &str, name: &str) -> String {
        format!(
            "M526-{kind}-{}-{name}",
            crate::test_seed::last_chars(&fixture.suffix, 6)
        )
    }

    /// 三要素合计必须等于节点金额（尾差进制造费用，不能加不齐）。
    fn assert_elements_add_up(node: &NodeView) {
        let elements = node.elements.as_ref().expect("该节点应有三要素");
        let sum = num(&elements.material) + num(&elements.labor) + num(&elements.overhead);
        assert_eq!(
            sum,
            num(&node.amount),
            "节点 {} 三要素加不齐",
            node.material_code.clone().unwrap_or_default()
        );
        assert_eq!(sum, num(&elements.total));
    }

    /// 造「原料 → 二级半成品 S2 → 半成品 S → 成品 F」这条 3 层链。
    ///
    /// ```text
    /// R(外购 5/件)
    ///   └─ S2 批：R 100 件 = 500 + 工 100 + 费 400 = 1000，完工 50 → 单位 20
    ///        └─ S 批：S2 20 件 = 400 + R 20 件 = 100 + 工 200 + 费 300 = 1000，完工 100 → 单位 10
    ///             └─ F 批：S 40 件 = 400 + R 20 件 = 100 + 工 100 + 费 100 = 700，完工 70 → 单位 10
    /// ```
    struct Chain {
        fixture: Fixture,
        raw: String,
        s2: String,
        semi: String,
        object_s: Uuid,
        /// 成品批的本期核算批次（v2 / v8 复用，别再给同一期间建第二个批次）。
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
            object_s,
            batch_f,
            fixture,
        }
    }

    // ==================== 用例 ====================

    /// ① 成品 → 半成品 → 二级半成品 → 外购料：逐层金额 / 结构 / 批号命中。
    #[tokio::test]
    async fn v1_three_level_structure_is_layered() {
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

        // 材料投入 500 = 自制半成品 400 + 外购料 100
        assert_eq!(num(&row.material_cost), dec("500"));
        assert_eq!(num(&row.semi_cost), dec("400"));
        assert_eq!(num(&row.purchased_cost), dec("100"));
        assert_eq!(row.semi_share.as_deref(), Some("80.00"));
        assert_eq!(row.state, "semi", "结构完整：{}", row.state_label);
        assert!(row.issues.is_empty(), "{:?}", row.issues);
        assert_eq!(row.depth, 3, "成品 → 半成品 → 二级半成品 → 外购料 = 3 层");
        assert_eq!(row.node_count, 5, "2 + 2 + 1");
        // 本期完工成本来自计算单（插件的 CALC 快照）
        assert_eq!(row.finished_cost.as_deref().map(num), Some(dec("700")));

        // ---- 第 1 层：S 400（80%）+ R 100（20%）----
        let semi = find(&row.tree, &chain.semi);
        assert_eq!(num(&semi.amount), dec("400"));
        assert_eq!(semi.share_pct.as_deref(), Some("80.00"));
        assert_eq!(num(&semi.qty), dec("40"));
        assert_eq!(semi.unit_cost.as_deref().map(num), Some(dec("10.000000")), "400/40");
        assert!(semi.is_semi);
        assert_eq!(semi.source.as_deref(), Some("batch"), "批号对得上就按批");
        let producer = semi.producer.as_ref().expect("半成品有产出批次");
        assert_eq!(producer.object_nos, fixture.object_no("S"));
        assert_eq!(producer.period_code, fixture.periods[1]);
        assert_eq!(num(&producer.finished_cost), dec("1000"));
        assert_eq!(num(&producer.completed_qty), dec("100"));
        assert_eq!(producer.unit_finished_cost.as_deref().map(num), Some(dec("10.000000")));
        // 结构比例 (500,200,300)/1000 × 400 = (200,80,120)
        let elements = semi.elements.as_ref().expect("半成品有三要素");
        assert_eq!(num(&elements.material), dec("200"));
        assert_eq!(num(&elements.labor), dec("80"));
        assert_eq!(num(&elements.overhead), dec("120"));
        assert_elements_add_up(semi);

        let raw_leaf = find(&row.tree, &chain.raw);
        assert_eq!(num(&raw_leaf.amount), dec("100"));
        assert_eq!(raw_leaf.share_pct.as_deref(), Some("20.00"));
        assert!(!raw_leaf.is_semi, "外购料没有产出批次");
        assert!(raw_leaf.producer.is_none());
        assert!(raw_leaf.children.is_empty());
        assert_eq!(
            raw_leaf.elements.as_ref().map(|item| num(&item.material)),
            Some(dec("100")),
            "外购料全额进材料"
        );

        // ---- 第 2 层：S 批自己的投入 = S2 400（80%）+ R 100（20%）----
        let s2 = find(&semi.children, &chain.s2);
        assert_eq!(num(&s2.amount), dec("400"));
        assert_eq!(s2.source.as_deref(), Some("batch"));
        let elements = s2.elements.as_ref().expect("S2 有三要素");
        // (500,100,400)/1000 × 400 = (200,40,160)
        assert_eq!(num(&elements.material), dec("200"));
        assert_eq!(num(&elements.labor), dec("40"));
        assert_eq!(num(&elements.overhead), dec("160"));
        assert_elements_add_up(s2);

        // ---- 第 3 层：S2 批自己的投入 = R 500（唯一个节点）----
        assert_eq!(s2.children.len(), 1);
        let deep_raw = &s2.children[0];
        assert_eq!(deep_raw.level, 3);
        assert_eq!(deep_raw.share_pct.as_deref(), Some("100.00"));
        assert_eq!(num(&deep_raw.amount), dec("500"));
        assert!(!deep_raw.is_semi);
        assert_elements_add_up(deep_raw);

        // ---- 逐层汇总 ----
        assert_eq!(row.layers.len(), 3);
        assert_eq!(num(&row.layers[0].amount), dec("500"), "第 1 层 = 材料投入");
        assert_eq!(num(&row.layers[0].elements.material), dec("300"), "S 200 + R 100");
        assert_eq!(num(&row.layers[0].elements.labor), dec("80"));
        assert_eq!(num(&row.layers[0].elements.overhead), dec("120"));
        assert_eq!(row.layers[0].incomplete_count, 0);
        assert_eq!(num(&row.layers[1].amount), dec("500"), "第 2 层 = S 批自身的投入");
        assert_eq!(num(&row.layers[1].elements.material), dec("300"), "S2 200 + R 100");
        assert_eq!(num(&row.layers[2].amount), dec("500"), "第 3 层 = S2 批自身的投入");
        assert_eq!(num(&row.layers[2].elements.material), dec("500"));

        // ---- 合计 ----
        assert_eq!(res.totals.row_count, 1);
        assert_eq!(res.totals.semi_row_count, 1);
        assert_eq!(num(&res.totals.material_cost), dec("500"));
        assert_eq!(num(&res.totals.semi_cost), dec("400"));
        assert_eq!(res.totals.max_depth, 3);

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0, "清场后零残留");
    }

    /// ② 领料行没有批号 ⇒ 同月平均结构、不下钻、给出提醒。
    #[tokio::test]
    async fn v2_lot_miss_falls_back_to_period_average() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        let finished = fixture.material("FINISHED", "F2").await;
        let batch = chain.batch_f;
        let object = fixture
            .object("F2", finished, Some(&fixture.lot("F2")), "40", "0", "0")
            .await;
        // 手工料行：没有来源单据 ⇒ 没有批号（走同月平均结构）
        let semi_material: Uuid = db()
            .scalar_with_params(
                "SELECT product_id FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&chain.object_s)],
            )
            .await
            .expect("取半成品物料 id");
        fixture
            .collect_material(object, &fixture.periods[2], semi_material, "40", "400")
            .await;
        fixture
            .item(
                object,
                "F2",
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
            &fixture.tag("F2"),
            None,
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("F2"));
        assert_eq!(row.state, "semi", "结构能取到（同月平均），只是没下钻");
        assert_eq!(row.node_count, 1, "不下钻：只有第 1 层一个节点");
        let node = &row.tree[0];
        assert!(node.is_semi);
        assert_eq!(node.source.as_deref(), Some("period_avg"));
        assert_eq!(node.source_label.as_deref(), Some("同月平均"));
        assert!(node.children.is_empty(), "同月平均不继续下钻");
        let producer = node.producer.as_ref().expect("有产出批次");
        assert_eq!(producer.batch_count, 1);
        assert!(producer.object_id.is_none(), "平均结构不指向单一批次");
        // 结构仍是 S 批的 (500,200,300)/1000 × 400
        let elements = node.elements.as_ref().expect("有三要素");
        assert_eq!(num(&elements.material), dec("200"));
        assert_eq!(num(&elements.labor), dec("80"));
        assert_eq!(num(&elements.overhead), dec("120"));
        assert_elements_add_up(node);
        assert!(
            row.warnings.iter().any(|text| text.contains("没有批号")),
            "要提醒没下钻：{:?}",
            row.warnings
        );

        fixture.cleanup(&chain.objects).await;
        fixture.cleanup(&[object]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ③ 自制半成品但产出批次没计算 ⇒ `is_semi` + `elements = null` + `no_structure`。
    #[tokio::test]
    async fn v3_semi_without_calculation_is_no_structure() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R").await;
        let uncosted = fixture.material("SEMI", "SX").await;
        let finished = fixture.material("FINISHED", "FX").await;
        let batch = fixture.batch(&fixture.periods[0], "DRAFT", "P1X").await;

        // 产出 SX 的成本对象：有对象、没有计算单（还没算成本）
        let producer = fixture
            .object("SX", uncosted, Some(&fixture.lot("SX")), "0", "0", "0")
            .await;
        fixture
            .collect_material(producer, &fixture.periods[0], raw, "10", "100")
            .await;
        // 消耗 SX 的成品批
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
        assert_eq!(row.state, "no_structure", "缺结构要单列一档");
        assert_eq!(num(&row.semi_cost), dec("300"), "自制半成品金额照记（不能当外购料）");
        assert_eq!(num(&row.purchased_cost), dec("0"));
        let node = &row.tree[0];
        assert!(
            node.is_semi,
            "有成本对象产出过 ⇒ 是自制半成品（与「有没有计算单」无关）"
        );
        assert!(node.elements.is_none(), "结构取不到就给 null，不画 0");
        assert!(node.source.is_none());
        assert!(
            row.issues.iter().any(|issue| issue.code == dto::CODE_NO_STRUCTURE),
            "要报 NO_STRUCTURE：{:?}",
            row.issues
        );
        assert_eq!(res.totals.no_structure_row_count, 1);
        assert!(res.note.contains("取不到完工成本结构"), "{}", res.note);

        fixture.cleanup(&[object, producer]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ④ 全是外购料 ⇒ `no_semi`、一层到底、金额全额进材料。
    #[tokio::test]
    async fn v4_purchased_only_row_is_flat() {
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
        assert_eq!(num(&row.material_cost), dec("250"));
        assert_eq!(num(&row.semi_cost), dec("0"));
        assert_eq!(num(&row.purchased_cost), dec("250"));
        assert_eq!(row.semi_share.as_deref(), Some("0.00"));
        assert_eq!(row.depth, 1, "一层到底");
        assert_eq!(row.node_count, 1);
        let node = &row.tree[0];
        assert!(!node.is_semi && node.children.is_empty());
        assert_elements_add_up(node);
        assert_eq!(num(&node.elements.as_ref().expect("有三要素").material), dec("250"));
        assert!(row.issues.is_empty());

        fixture.cleanup(&[object]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑤ A 用 B、B 用 A ⇒ 闭环截断、行状态 `cycle`、不死循环。
    #[tokio::test]
    async fn v5_cycle_is_cut_and_terminates() {
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
        // A 吃 B、B 吃 A（批号互相指向）
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
        assert_eq!(row.state, "cycle", "闭环要单列一档");
        assert_eq!(row.depth, 2, "A → B → 又回到 A：在第 2 层截断，不该越钻越深");
        let b_node = &row.tree[0];
        assert_eq!(b_node.level, 1);
        let loop_node = &b_node.children[0];
        assert_eq!(loop_node.level, 2);
        assert!(loop_node.truncated, "回到起点的那个节点要标截断");
        assert!(
            loop_node.issues.iter().any(|issue| issue.code == dto::CODE_CYCLE),
            "节点上要报 CYCLE：{:?}",
            loop_node.issues
        );
        assert!(
            row.issues.iter().any(|issue| issue.code == dto::CODE_CYCLE),
            "行级也要冒出来：{:?}",
            row.issues
        );
        assert_eq!(res.totals.cycle_row_count, 1);
        assert!(res.note.contains("闭环"), "{}", res.note);

        fixture.cleanup(&[object_a, object_b]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑥ `max_depth=1` 截断并给 `DEPTH_CUT`；`max_depth=6` 同一行不误报截断。
    #[tokio::test]
    async fn v6_depth_limit_cuts_only_when_needed() {
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
        assert_eq!(
            shallow_row.state, "truncated",
            "第 1 层的半成品下面还有半成品 ⇒ 没钻到底"
        );
        assert_eq!(shallow_row.depth, 1);
        assert!(shallow_row.tree[0].truncated);
        assert!(
            shallow_row.issues.iter().any(|issue| issue.code == dto::CODE_DEPTH_CUT),
            "要报 DEPTH_CUT：{:?}",
            shallow_row.issues
        );
        assert!(shallow.note.contains("max_depth"), "{}", shallow.note);

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
        assert_eq!(deep_row.state, "semi", "钻到底了就不该报截断");
        assert_eq!(deep_row.depth, 3);

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

        fixture.cleanup(&chain.objects).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑧ 合计是整张表（`page_size = 1` 时 `row_count` 仍是全表）；状态筛选 / 关键字收窄。
    #[tokio::test]
    async fn v8_totals_are_whole_table_and_filters_narrow() {
        if !ensure_db().await {
            return;
        }
        let chain = build_chain().await;
        let fixture = &chain.fixture;
        // 再加一个「缺结构」的行，让分档有意义
        let uncosted = fixture.material("SEMI", "SU").await;
        let finished = fixture.material("FINISHED", "FU").await;
        let batch = chain.batch_f;
        let producer = fixture
            .object("SU", uncosted, Some(&fixture.lot("SU")), "0", "0", "0")
            .await;
        let object = fixture
            .object("FU", finished, Some(&fixture.lot("FU")), "10", "0", "0")
            .await;
        fixture
            .collect_material(object, &fixture.periods[2], uncosted, "10", "200")
            .await;
        fixture
            .item(
                object,
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

        let keyword = "CB-M526-";
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
        assert_eq!(all.total, 2, "筛选后两行（成品 + 缺结构）");
        assert_eq!(all.totals.row_count, 2, "合计是整张表，不受分页影响");
        assert_eq!(all.totals.semi_row_count, 1);
        assert_eq!(all.totals.no_structure_row_count, 1);
        assert_eq!(all.totals.blocked_row_count, 1);
        assert_eq!(num(&all.totals.material_cost), dec("700"), "500 + 200");

        let semi_only = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "semi",
            keyword,
            None,
            None,
        )
        .await;
        assert_eq!(semi_only.total, 1);
        assert_eq!(semi_only.list[0].object_no, fixture.object_no("F"));

        let blocked = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "blocked",
            keyword,
            None,
            None,
        )
        .await;
        assert_eq!(blocked.total, 1);
        assert_eq!(blocked.list[0].object_no, fixture.object_no("FU"));

        let narrowed = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("FU"),
            None,
            None,
        )
        .await;
        assert_eq!(narrowed.total, 1, "关键字收窄");

        fixture.cleanup(&chain.objects).await;
        fixture.cleanup(&[object, producer]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑨ 产出批次被红冲 ⇒ 结构取不到（不拿作废的账拆钱）。
    #[tokio::test]
    async fn v9_reversed_producer_period_has_no_structure() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let raw = fixture.material("RAW", "R9").await;
        let semi = fixture.material("SEMI", "S9").await;
        let finished = fixture.material("FINISHED", "F9").await;
        let reversed_batch = fixture.batch(&fixture.periods[1], "REVERSED", "P2R").await;
        let batch = fixture.batch(&fixture.periods[2], "DRAFT", "P3R").await;

        let producer = fixture
            .object("S9", semi, Some(&fixture.lot("S9")), "10", "0", "0")
            .await;
        fixture
            .collect_material(producer, &fixture.periods[1], raw, "10", "100")
            .await;
        fixture
            .item(
                producer,
                "S9",
                reversed_batch,
                semi,
                &fixture.periods[1],
                "10",
                "100",
                "0",
                "0",
            )
            .await;

        let object = fixture
            .object("F9", finished, Some(&fixture.lot("F9")), "10", "0", "0")
            .await;
        fixture
            .collect_material_with_lot(object, &fixture.periods[2], semi, &fixture.lot("S9"), "10", "150")
            .await;
        fixture
            .item(
                object,
                "F9",
                batch,
                finished,
                &fixture.periods[2],
                "10",
                "150",
                "0",
                "0",
            )
            .await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[2],
            "all",
            &fixture.tag("F9"),
            None,
            None,
        )
        .await;
        let row = row_of(&res, &fixture.object_no("F9"));
        assert_eq!(row.state, "no_structure", "红冲批次的账不能拿来拆结构");
        let node = &row.tree[0];
        assert!(node.is_semi, "仍是自制半成品（对象存在）");
        assert!(node.elements.is_none());
        assert!(node.producer.is_none(), "红冲批次不进候选");
        assert!(row.issues.iter().any(|issue| issue.code == dto::CODE_NO_STRUCTURE));

        fixture.cleanup(&[object, producer]).await;
        Fixture::reset().await;
        assert_eq!(fixture.residue().await, 0);
    }

    /// ⑩ 清场断言（前面每个用例都自己清了；这里兜一次底，防止漏清污染后续卡）。
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
            "M5-26 夹具行（CB-M526- / CS-M526- / 备注 M5-26 夹具）必须为 0"
        );
    }
}
