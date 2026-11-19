//! 在制品「期末在制余额 + 结转核对」（M5-23）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m523_dev \
//!   cargo test --lib cost_wip_balance -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_unsettled_balance_is_live_and_flagged`] | 没批次：余额是试算（`source=live`）、标「未入账」、三要素加起来 = 余额 |
//! | 2 | [`v2_settled_balance_takes_the_book_snapshot`] | 已结算：三个金额列取计算单快照，`ledger` 有账面，结转「待下期」 |
//! | 3 | [`v3_next_period_takes_the_balance_over`] | 下期算完且期初一分不差 ⇒ **已结转** |
//! | 4 | [`v4_tampered_next_beg_is_a_mismatch`] | 下期期初被改 0.0001 ⇒ **结转不齐**（差额给数） |
//! | 5 | [`v5_closed_object_with_wip_is_flagged`] | 关批仍挂余额 ⇒ **关批挂账**（死余额） |
//! | 6 | [`v6_reversed_batch_invalidates_the_balance`] | 本期批次红冲 ⇒ **已红冲**，账面余额作废 |
//! | 7 | [`v7_other_workshop_sees_nothing`] | 数据范围：别车间用户看不到本车间的行（不出现，不是 403 探测口） |
//! | 8 | [`v8_totals_are_the_whole_table_and_filters_narrow`] | 合计是整张表（`page_size = 1` 时 `row_count = 3`）；十档筛选 |
//! | 9 | [`v9_blocked_rows_have_no_money`] | 算不出来的行：余额给空值不给 0，且不进金额合计 |
//! | 10 | [`zz_teardown_leaves_zero_residue`] | 本夹具按对象号清场，零残留 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `2000 + slot % 70`，期间取该年 01/02/03 三个月）；
//!   建前先清该槽上一次的残留；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**；
//! * 计算单用 `cost_period_item_dto::compose`（与真 calc 同一段代码）算好再落库，
//!   不是手抄几个数字 —— 否则「余额 = 计算单快照」这条断言会变成同义反复。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_period_item_dto::{
        self as item_dto, BegCost, ItemCalc, ItemInput, OutputInput,
    };
    use crate::business::erp::cost::cost_wip_balance_dto::{self as dto, BalanceQuery, BalanceRes};
    use crate::business::erp::cost::cost_wip_balance_handler::{balance_table_of, filter_of};
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
        /// 三个连续期间 01 / 02 / 03（跨月结转）。
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
                &format!("M523-DEPT2-{suffix}"),
                &format!("M5-23 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M523-MAT-{suffix}"),
                &format!("M5-23 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                other_dept,
                product,
                warehouse: Uuid::nil(),
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
            format!("CB-M523-{}", self.tag(name))
        }

        /// 造成本对象（完工 / 在制 / 完工率 / 状态由用例给）。
        async fn object(&self, name: &str, status: &str, completed: &str, wip: &str, pct: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            let warehouse = if self.warehouse.is_nil() {
                crate::test_seed::ensure_warehouse(
                    &format!("M53W{}", crate::test_seed::last_chars(&self.suffix, 8)),
                    &format!("M5-23 夹具仓 {}", self.suffix),
                )
                .await
            } else {
                self.warehouse
            };
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
                        "LOT-M523-{name}-{}",
                        crate::test_seed::last_chars(&self.suffix, 6)
                    )),
                    Value::from(&warehouse),
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

        /// 造一条归集行（料行必须挂物料 + 数量；工 / 费行不许挂）。
        async fn collect(&self, object: Uuid, element: &str, amount: &str, period: &str) {
            let (material, quantity) = if element == "MATERIAL" {
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
                             {quantity}, ?::text::numeric, 'M5-23 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集行夹具");
        }

        /// 造一张成本核算批次（`SETTLED` / `REVERSED` 要挂审批 + 结算时间 + 完工凭证，库层硬约束）。
        async fn period_batch(&self, period: &str, status: &str, tag: &str) -> Uuid {
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
                    "CS-M523-{tag}-{}",
                    crate::test_seed::last_chars(&self.suffix, 8)
                )),
                Value::Text(status.to_string()),
            ];
            if needs_approval {
                params.push(Value::from(&Uuid::now_v7()));
                params.push(Value::from(&Uuid::now_v7()));
            }
            if status == "REVERSED" {
                params.push(Value::Text("M5-23 夹具红冲".to_string()));
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
                object_no: format!("{object:?}"),
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
                    "DELETE FROM biz_cost_collect WHERE remark = 'M5-23 夹具' AND period_code BETWEEN ? AND ?"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(third.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                       (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M523-%' \
                          AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M523-%' \
                        AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M523-%' \
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
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) + \
                        (SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![pattern.clone(), pattern.clone(), pattern],
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
        page_size: Option<u64>,
    ) -> BalanceRes {
        let filter = filter_of(&BalanceQuery {
            period_code: Some(period.to_string()),
            keyword: Some(keyword.to_string()),
            state: Some(state.to_string()),
            page: None,
            page_size,
        })
        .expect("筛选参数合法");
        balance_table_of(principal, &filter).await.expect("余额表查询")
    }

    fn row<'a>(res: &'a BalanceRes, object_no: &str) -> &'a dto::BalanceRow {
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

    /// 设计算例 A 的输入（完工 800 / 在制 200 / 完工率 50% / 料 19000 / 工 4000 / 费 4000）。
    async fn seed_case_a(fixture: &Fixture, object: Uuid, period: &str) {
        fixture.collect(object, "MATERIAL", "19000", period).await;
        fixture.collect(object, "LABOR", "4000", period).await;
        fixture.collect(object, "OVERHEAD", "4000", period).await;
    }

    // ==================== 用例 ====================

    /// ① 没批次：余额是试算、标「未入账」、三要素加起来 = 余额。
    #[tokio::test]
    async fn v1_unsettled_balance_is_live_and_flagged() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("V1", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &fixture.periods[0]).await;

        let res = table(
            &fixture.super_principal(),
            &fixture.periods[0],
            "wip",
            &fixture.tag("V1"),
            None,
        )
        .await;

        assert_eq!(res.total, 1, "关键字收窄到本夹具的唯一对象");
        assert!(!res.cost_ready, "没批次 ⇒ 试算视图");
        assert!(res.batch.is_none());
        let row = row(&res, &fixture.object_no("V1"));
        assert_eq!(row.source, "live");
        assert_eq!(row.carry.state, "unsettled");
        assert!(row.ledger.is_none(), "没结算就没有账面快照");
        // 算例 A：完工 22311.1104 / 在制 4688.8896
        assert_eq!(qty(row.wip_total.as_deref().expect("余额")), dec("4688.8896"));
        assert_eq!(qty(row.finished_total.as_deref().expect("完工")), dec("22311.1104"));
        // 期初 0 + 投入 27000 = 完工 + 在制
        assert_eq!(
            qty(row.beg_total.as_deref().expect("期初")) + qty(row.cur_total.as_deref().expect("投入")),
            qty(row.finished_total.as_deref().expect("完工")) + qty(row.wip_total.as_deref().expect("余额"))
        );
        // 三要素加起来 = 余额
        let elements: Decimal = row
            .elements
            .iter()
            .map(|item| qty(item.wip.as_deref().expect("分要素余额")))
            .sum();
        assert_eq!(elements, dec("4688.8896"));
        assert!(row.elements_available);
        // 合计：未入账 1 行、金额就是这笔
        assert_eq!(res.totals.unsettled_count, 1);
        assert_eq!(qty(&res.totals.unbooked_total), dec("4688.8896"));
        assert!(res.note.contains("未入账"), "{}", res.note);

        fixture.cleanup(&[object], &[]).await;
    }

    /// ② 已结算：三个金额列取计算单快照；下期还没算 ⇒ 待结转。
    #[tokio::test]
    async fn v2_settled_balance_takes_the_book_snapshot() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("V2", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &period).await;
        let batch = fixture.period_batch(&period, "SETTLED", "V2").await;
        let calc = fixture
            .calc_item(
                object,
                batch,
                "SETTLED",
                &period,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost {
                    material: Decimal::ZERO,
                    labor: Decimal::ZERO,
                    overhead: Decimal::ZERO,
                    total: Decimal::ZERO,
                },
                "0",
            )
            .await;

        let res = table(&fixture.super_principal(), &period, "wip", &fixture.tag("V2"), None).await;
        assert!(res.cost_ready, "有活跃批次");
        assert_eq!(res.batch.as_ref().map(|item| item.status.as_str()), Some("SETTLED"));
        let row = row(&res, &fixture.object_no("V2"));
        assert_eq!(row.source, "settled", "已结算 ⇒ 账面优先");
        let ledger = row.ledger.as_ref().expect("账面快照");
        assert_eq!(ledger.status, "SETTLED");
        assert_eq!(qty(&ledger.wip_end_cost), calc.wip_end_cost);
        assert!(ledger.agrees, "同一组数算的，不该漂移");
        assert_eq!(qty(row.wip_total.as_deref().expect("余额")), calc.wip_end_cost);
        assert_eq!(row.carry.state, "pending", "下期还没算");
        assert_eq!(
            row.carry.to_period.as_deref(),
            Some(fixture.periods[1].as_str()),
            "要指出下一期是哪个期间"
        );
        assert_eq!(res.totals.pending_count, 1);

        fixture.cleanup(&[object], &[batch]).await;
    }

    /// ③ 下期算完且期初一分不差 ⇒ 已结转。
    #[tokio::test]
    async fn v3_next_period_takes_the_balance_over() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (p1, p2) = (fixture.periods[0].clone(), fixture.periods[1].clone());
        let object = fixture.object("V3", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &p1).await;
        let batch1 = fixture.period_batch(&p1, "SETTLED", "V3A").await;
        let calc1 = fixture
            .calc_item(
                object,
                batch1,
                "SETTLED",
                &p1,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost {
                    material: Decimal::ZERO,
                    labor: Decimal::ZERO,
                    overhead: Decimal::ZERO,
                    total: Decimal::ZERO,
                },
                "0",
            )
            .await;

        // 下期：期初 = 本期账面期末（一分不差），本期完工 100 / 期末在制 100
        let batch2 = fixture.period_batch(&p2, "DRAFT", "V3B").await;
        let beg = BegCost {
            material: Decimal::ZERO,
            labor: Decimal::ZERO,
            overhead: calc1.wip_end_cost,
            total: calc1.wip_end_cost,
        };
        fixture
            .calc_item(
                object, batch2, "CALC", &p2, "100", "100", "50", "5000", "1000", "1000", beg, "200",
            )
            .await;

        let res = table(&fixture.super_principal(), &p1, "wip", &fixture.tag("V3"), None).await;
        let row = row(&res, &fixture.object_no("V3"));
        assert_eq!(row.carry.state, "carried", "hint={}", row.carry.hint);
        assert_eq!(row.carry.to_period.as_deref(), Some(p2.as_str()));
        assert_eq!(
            qty(row.carry.to_beg_cost.as_deref().expect("下期期初")),
            calc1.wip_end_cost
        );
        assert_eq!(qty(row.carry.diff_cost.as_deref().expect("差额")), Decimal::ZERO);
        assert!(!row.carry.needs_attention);
        assert_eq!(res.totals.carried_count, 1);

        fixture.cleanup(&[object], &[batch1, batch2]).await;
    }

    /// ④ 下期期初被改 0.0001 ⇒ 结转不齐（差额给数）。
    #[tokio::test]
    async fn v4_tampered_next_beg_is_a_mismatch() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let (p1, p2) = (fixture.periods[0].clone(), fixture.periods[1].clone());
        let object = fixture.object("V4", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &p1).await;
        let batch1 = fixture.period_batch(&p1, "SETTLED", "V4A").await;
        let calc1 = fixture
            .calc_item(
                object,
                batch1,
                "SETTLED",
                &p1,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost::default(),
                "0",
            )
            .await;
        let batch2 = fixture.period_batch(&p2, "DRAFT", "V4B").await;
        fixture
            .calc_item(
                object,
                batch2,
                "CALC",
                &p2,
                "100",
                "100",
                "50",
                "5000",
                "1000",
                "1000",
                BegCost {
                    material: Decimal::ZERO,
                    labor: Decimal::ZERO,
                    overhead: calc1.wip_end_cost,
                    total: calc1.wip_end_cost,
                },
                "200",
            )
            .await;

        // 模拟「有人手工改了计算单期初」：期初与期末同时 +0.0001 —— 库层 `ck_biz_cost_item_cons`
        // 要求「完工 + 期末 = 期初 + 投入」，只改期初会被约束直接拒掉（这本身是好事，写进断言口径）。
        let tampered = db()
            .exec_sql(
                "UPDATE biz_cost_period_item \
                SET wip_beg_cost = wip_beg_cost + 0.0001, wip_end_cost = wip_end_cost + 0.0001 \
              WHERE period_id = ?::uuid AND cost_object_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&batch2), Value::from(&object)],
            )
            .await
            .expect("制造结转差额（期初与期末一起动，才不撞守恒约束）");
        assert_eq!(tampered, 1, "恰好改到那一行");

        // 反证：只改期初会被库层守恒约束拦下（不许再出现「改一格就漂账」的数据）
        let alone = db()
            .exec_sql(
                "UPDATE biz_cost_period_item SET wip_beg_cost = wip_beg_cost + 1 \
                  WHERE period_id = ?::uuid AND cost_object_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&batch2), Value::from(&object)],
            )
            .await;
        assert!(alone.is_err(), "只改期初必须被 ck_biz_cost_item_cons 拒绝");

        let res = table(&fixture.super_principal(), &p1, "wip", &fixture.tag("V4"), None).await;
        let row = row(&res, &fixture.object_no("V4"));
        assert_eq!(row.carry.state, "mismatch", "hint={}", row.carry.hint);
        assert_eq!(qty(row.carry.diff_cost.as_deref().expect("差额")), dec("0.0001"));
        assert!(row.carry.needs_attention);
        assert!(row.carry.hint.contains("红冲"), "要给怎么修：{}", row.carry.hint);
        assert_eq!(res.totals.mismatch_count, 1);
        assert_eq!(qty(&res.totals.mismatch_abs_total), dec("0.0001"));

        fixture.cleanup(&[object], &[batch1, batch2]).await;
    }

    /// ⑤ 关批仍挂余额 ⇒ 关批挂账（永远结不掉的死余额）。
    #[tokio::test]
    async fn v5_closed_object_with_wip_is_flagged() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("V5", "CLOSED", "800", "200", "50").await;
        seed_case_a(&fixture, object, &period).await;
        let batch = fixture.period_batch(&period, "SETTLED", "V5").await;
        fixture
            .calc_item(
                object,
                batch,
                "SETTLED",
                &period,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost::default(),
                "0",
            )
            .await;

        let res = table(&fixture.super_principal(), &period, "wip", &fixture.tag("V5"), None).await;
        let row = row(&res, &fixture.object_no("V5"));
        assert_eq!(row.object_status, "CLOSED");
        assert_eq!(row.carry.state, "closed_wip", "hint={}", row.carry.hint);
        assert!(row.carry.hint.contains("OPEN"), "要给怎么修：{}", row.carry.hint);
        assert_eq!(res.totals.closed_wip_count, 1);
        assert!(qty(&res.totals.closed_wip_total) > Decimal::ZERO);

        fixture.cleanup(&[object], &[batch]).await;
    }

    /// ⑥ 本期批次红冲 ⇒ 已红冲（账面余额作废）。
    #[tokio::test]
    async fn v6_reversed_batch_invalidates_the_balance() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("V6", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &period).await;
        let batch = fixture.period_batch(&period, "REVERSED", "V6").await;
        fixture
            .calc_item(
                object,
                batch,
                "SETTLED",
                &period,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost::default(),
                "0",
            )
            .await;

        let res = table(&fixture.super_principal(), &period, "wip", &fixture.tag("V6"), None).await;
        assert!(res.batch.is_none(), "红冲批次不占位");
        let reversed = res.reversed_batch.as_ref().expect("要报出被红冲的那条批次");
        assert_eq!(reversed.status, "REVERSED");
        let row = row(&res, &fixture.object_no("V6"));
        assert_eq!(row.carry.state, "reversed", "hint={}", row.carry.hint);
        assert!(row.carry.hint.contains("红冲"), "{}", row.carry.hint);
        assert_eq!(res.totals.reversed_count, 1);
        assert!(res.note.contains("已红冲"), "{}", res.note);

        fixture.cleanup(&[object], &[batch]).await;
    }

    /// ⑦ 数据范围：别车间用户看不到本车间的行（不出现，不是 403）。
    #[tokio::test]
    async fn v7_other_workshop_sees_nothing() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("V7", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &period).await;

        let mine = table(&fixture.super_principal(), &period, "wip", &fixture.tag("V7"), None).await;
        assert_eq!(mine.total, 1, "本车间看得到");
        let other = table(
            &fixture.other_dept_principal(),
            &period,
            "wip",
            &fixture.tag("V7"),
            None,
        )
        .await;
        assert_eq!(other.total, 0, "别车间看不见（不是 403，是不出现）");
        assert!(other.list.is_empty());

        fixture.cleanup(&[object], &[]).await;
    }

    /// ⑧ 合计是整张表（`page_size = 1` 时 `row_count = 3`）；十档筛选。
    #[tokio::test]
    async fn v8_totals_are_the_whole_table_and_filters_narrow() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let a = fixture.object("V8A", "OPEN", "800", "200", "50").await;
        let b = fixture.object("V8B", "OPEN", "800", "200", "50").await;
        let c = fixture.object("V8C", "OPEN", "800", "200", "0").await; // 完工率 0 ⇒ 算不出来
        seed_case_a(&fixture, a, &period).await;
        seed_case_a(&fixture, b, &period).await;
        seed_case_a(&fixture, c, &period).await;

        let tag = fixture.tag("");
        let paged = table(&fixture.super_principal(), &period, "wip", &tag, Some(1)).await;
        assert_eq!(paged.list.len(), 1, "每页 1 行");
        assert_eq!(paged.total, 3, "命中 3 行");
        assert_eq!(paged.totals.row_count, 3, "合计是整张表，不受分页影响");
        assert_eq!(paged.totals.unsettled_count, 2);
        assert_eq!(paged.totals.blocked_count, 1);
        // 金额合计只含能算的两行（4688.8896 × 2）
        assert_eq!(qty(&paged.totals.wip_total), dec("9377.7792"));
        assert_eq!(qty(&paged.totals.wip_qty), dec("600"), "数量含全部行");

        let blocked = table(&fixture.super_principal(), &period, "blocked", &tag, None).await;
        assert_eq!(blocked.total, 1, "blocked 档只有那一行");
        assert_eq!(blocked.list[0].object_no, fixture.object_no("V8C"));

        let cleared = table(&fixture.super_principal(), &period, "cleared", &tag, None).await;
        assert_eq!(cleared.total, 0, "没有「全部完工」的行");

        let all = table(&fixture.super_principal(), &period, "all", &tag, None).await;
        assert_eq!(all.total, 3);

        fixture.cleanup(&[a, b, c], &[]).await;
    }

    /// ⑨ 算不出来的行：余额给空值不给 0，且不进金额合计。
    #[tokio::test]
    async fn v9_blocked_rows_have_no_money() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("V9", "OPEN", "800", "200", "0").await;
        seed_case_a(&fixture, object, &period).await;

        let res = table(&fixture.super_principal(), &period, "wip", &fixture.tag("V9"), None).await;
        let row = row(&res, &fixture.object_no("V9"));
        assert_eq!(row.carry.state, "blocked");
        assert!(row.wip_total.is_none(), "算不出来给空值，不是 0");
        assert!(!row.issues.is_empty(), "要带问题码与怎么修");
        assert!(row.carry.hint.contains("修"), "{}", row.carry.hint);
        assert_eq!(qty(&res.totals.wip_total), Decimal::ZERO, "金额合计跳过它");
        assert_eq!(qty(&res.totals.wip_qty), dec("200"), "数量照进（它是事实）");

        fixture.cleanup(&[object], &[]).await;
    }

    /// ⑩ 本夹具清场后零残留。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let period = fixture.periods[0].clone();
        let object = fixture.object("ZZ", "OPEN", "800", "200", "50").await;
        seed_case_a(&fixture, object, &period).await;
        let batch = fixture.period_batch(&period, "SETTLED", "ZZ").await;
        fixture
            .calc_item(
                object,
                batch,
                "SETTLED",
                &period,
                "800",
                "200",
                "50",
                "19000",
                "4000",
                "4000",
                BegCost::default(),
                "0",
            )
            .await;

        fixture.cleanup(&[object], &[batch]).await;

        assert_eq!(fixture.residue().await, 0, "清场后本夹具零残留");
    }
}
