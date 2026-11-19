//! 在制品「约当产量表」（M5-20）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m520_dev \
//!   cargo test --lib cost_wip_equiv -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_pct_times_qty_is_the_equivalence`] | 完工率 × 数量：材料（WQ 按 100%）/ 加工（WQ × 完工率）两条分母 + 本期完工 |
//! | 2 | [`v2_prior_settled_completion_is_deducted`] | 跨月对象**不能用累计完工**：本期完工 = 累计 − 前期已结算完工合计 |
//! | 3 | [`v3_agrees_with_the_real_calc_item`] | 与真计算单逐字段对拍一致 ⇒ `agrees = true`、不需要重算 |
//! | 4 | [`v4_touching_pct_after_calc_flags_drift`] | 算完又改完工率 ⇒ `drift` 命中「完工率」、`state = drift` 可筛出 |
//! | 5 | [`v5_wip_without_pct_is_blocked_not_zero`] | 在制 > 0 但完工率 0：加工约当给 `null` 不给 0，`blocked` + 问题码 |
//! | 6 | [`v6_cost_without_output_is_listed_and_blocked`] | 有归集无产量的对象**不能被漏掉**：出现在表里并标 `NO_OUTPUT` |
//! | 7 | [`v7_no_batch_still_shows_the_trial_sheet`] | 没建批次：照出试算视图（`cost_ready = false`、`calc = null`、页脚说明） |
//! | 8 | [`v8_other_workshop_sees_nothing`] | 数据范围：别车间用户看不到本车间的行（不出现，不是 403 探测口） |
//! | 9 | [`v9_totals_are_the_whole_table_and_filters_narrow`] | 合计是整张表（`page_size = 1` 时 `row_count = 3`）；四档筛选各自命中 |
//! | 10 | [`zz_teardown_leaves_zero_residue`] | 清场后本卡造的行全 0 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `2000 + slot % 70`，期间取该年 01/02/03 三个月）；
//!   建前先清该槽上一次的残留；
//! * 对象号 / 批号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**，
//!   绝不按「全表有多少行」断言（别的卡的夹具也在同一个库里）；
//! * 计算单 / 期间批次**直接 SQL 造**：本卡验的是「读口径 + 对拍」，要造的形态
//!   （前期已结算、算完又改数）用 API 造不出来或要绕很远；不验写路径。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_wip_equiv_dto::{self as dto, EquivQuery, EquivRes};
    use crate::business::erp::cost::cost_wip_equiv_handler::{equiv_table_of, filter_of};
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
        /// 三个连续期间 01 / 02 / 03（跨月扣减 / 期间轴）。
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
                &format!("M520-DEPT2-{suffix}"),
                &format!("M5-20 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M520-MAT-{suffix}"),
                &format!("M5-20 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M52W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-20 夹具仓 {suffix}"),
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
            format!("CB-M520-{}", self.tag(name))
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
                        "LOT-M520-{name}-{}",
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

        /// 造一条归集行（判 `NO_OUTPUT` / 让对象进表用）。
        async fn collect(&self, object: Uuid, amount: &str, period: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               line_no, occur_date, material_id, qty, amount, \
                                               remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'MANUAL', 0, ?::text::date, \
                         ?::uuid, 10, ?::text::numeric, 'M5-20 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object),
                    Value::Text(period.to_string()),
                    Value::Text(format!("{period}-10")),
                    Value::from(&self.product),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造归集行夹具");
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
                Value::Text(format!("CS-M520-{}", crate::test_seed::last_chars(&self.suffix, 8))),
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

        /// 造一行计算单（成本全 0 ⇒ 库层守恒式天然成立）。
        async fn calc_item(&self, object: Uuid, batch: Uuid, status: &str, completed: &str, wip: &str, pct: &str) {
            let (equiv_material, equiv_conversion) =
                crate::business::erp::cost::cost_period_item_dto::equivalents(dec(completed), dec(wip), dec(pct));
            let stock_in = if status == "SETTLED" { "?::uuid" } else { "NULL" };
            let mut params = vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&batch),
                Value::from(&object),
                Value::from(&self.product),
                Value::Text(completed.to_string()),
                Value::Text(wip.to_string()),
                Value::Text(pct.to_string()),
                Value::Text(equiv_material.round_dp(4).to_string()),
                Value::Text(equiv_conversion.round_dp(4).to_string()),
                Value::Text(status.to_string()),
            ];
            if status == "SETTLED" {
                params.push(Value::from(&Uuid::now_v7()));
            }
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                        completed_qty, wip_end_qty, completion_pct, \
                                                        equiv_material, equiv_conversion, status, \
                                                        stock_in_id, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::text::numeric, ?, {stock_in}, \
                             now(), now())"
                ),
                params,
            )
            .await
            .expect("造计算单夹具");
        }

        /// 清掉该槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let first = &periods[0];
            let third = &periods[2];
            for statement in [
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-20 夹具' AND period_code BETWEEN ? AND ?",
                "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M520-%' \
                      AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))",
                "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M520-%' \
                    AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')",
            ] {
                let params = if statement.contains("remark") {
                    vec![Value::Text(first.clone()), Value::Text(third.clone())]
                } else {
                    vec![Value::Text(first.clone()), Value::Text(first.clone())]
                };
                let _ = db().exec_sql(statement.to_string(), params).await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS-M520-%' \
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
    async fn sheet(
        principal: &ScopedPrincipal,
        period: &str,
        tag: &str,
        state: Option<&str>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> EquivRes {
        let query = EquivQuery {
            period_code: Some(period.to_string()),
            keyword: Some(tag.to_string()),
            state: state.map(str::to_string),
            page,
            page_size,
        };
        let filter = filter_of(&query).expect("入参归一化");
        equiv_table_of(principal, &filter).await.expect("约当产量表")
    }

    /// 按对象号取一行（找不到就是夹具没造对，直接失败）。
    fn row_of<'a>(sheet: &'a EquivRes, object_no: &str) -> &'a dto::EquivRow {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    // ==================== 1. 完工率 × 数量 ====================

    #[tokio::test]
    async fn v1_pct_times_qty_is_the_equivalence() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V1", "OPEN", "800", "200", "50").await;

        let sheet = sheet(&f.super_principal(), &f.periods[0], &f.tag("V1"), None, None, None).await;
        assert!(!sheet.cost_ready, "没建批次 ⇒ 试算视图");
        let row = row_of(&sheet, &f.object_no("V1"));

        // 本期完工 = 累计 800（没有前期已结算）
        assert_eq!(qty(&row.completed_qty), dec("800"));
        // 材料约当 = 完工 + 在制 × 100% = 800 + 200
        assert_eq!(row.wip_equiv_material.as_deref().map(qty), Some(dec("200")));
        assert_eq!(row.equiv_material.as_deref().map(qty), Some(dec("1000")));
        // 加工约当 = 完工 + 在制 × 完工率 = 800 + 200 × 50% = 900
        assert_eq!(row.wip_equiv_conversion.as_deref().map(qty), Some(dec("100")));
        assert_eq!(row.equiv_conversion.as_deref().map(qty), Some(dec("900")));
        assert_eq!(row.state, "wip");
        assert!(!row.recalc_required, "没批次就没有对拍，不该报漂移");
        assert!(row.issues.is_empty(), "正常行不该有问题：{:?}", row.issues);
        // 完工率填了、在制也在 ⇒ 不该有「完工率白填」这类提示
        assert!(row.warnings.is_empty(), "不该有提醒：{:?}", row.warnings);

        // 合计与行同源
        assert_eq!(sheet.totals.row_count, 1);
        assert_eq!(sheet.totals.wip_row_count, 1);
        assert_eq!(qty(&sheet.totals.completed_qty), dec("800"));
        assert_eq!(qty(&sheet.totals.wip_qty), dec("200"));
        assert_eq!(qty(&sheet.totals.equiv_material), dec("1000"));
        assert_eq!(qty(&sheet.totals.equiv_conversion), dec("900"));
        assert!(sheet.note.contains("试算"), "没批次要在页脚说清楚：{}", sheet.note);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 2. 跨月：前期已结算扣减 ====================

    #[tokio::test]
    async fn v2_prior_settled_completion_is_deducted() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        // 累计完工 1000：一期已结算完工 300、期末在制 100；本期在制 200、完工率 25%
        let object = f.object("V2", "OPEN", "1000", "200", "25").await;
        let prior_batch = f.period_batch(&f.periods[0], "SETTLED").await;
        f.calc_item(object, prior_batch, "SETTLED", "300", "100", "50").await;

        let second = sheet(&f.super_principal(), &f.periods[1], &f.tag("V2"), None, None, None).await;
        let row = row_of(&second, &f.object_no("V2"));
        assert_eq!(qty(&row.cumulative_completed_qty), dec("1000"));
        assert_eq!(qty(&row.prior_completed_qty), dec("300"), "前期已结算完工合计");
        assert_eq!(qty(&row.completed_qty), dec("700"), "本期完工不能用累计数");
        // 材料约当 = 700 + 200；加工约当 = 700 + 200 × 25% = 750
        assert_eq!(row.equiv_material.as_deref().map(qty), Some(dec("900")));
        assert_eq!(row.equiv_conversion.as_deref().map(qty), Some(dec("750")));

        // 同一对象在**前期**看：前期已结算只看 `period_code < 本期`，不含本期
        let first = sheet(&f.super_principal(), &f.periods[0], &f.tag("V2"), None, None, None).await;
        let row_first = row_of(&first, &f.object_no("V2"));
        assert_eq!(qty(&row_first.prior_completed_qty), dec("0"), "前期没有更早的已结算");
        assert_eq!(qty(&row_first.completed_qty), dec("1000"));

        f.cleanup(&[object], &[prior_batch]).await;
    }

    // ==================== 3. 与真计算单对拍一致 ====================

    #[tokio::test]
    async fn v3_agrees_with_the_real_calc_item() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V3", "OPEN", "800", "200", "50").await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(object, batch, "CALC", "800", "200", "50").await;

        let sheet = sheet(&f.super_principal(), &f.periods[0], &f.tag("V3"), None, None, None).await;
        assert!(sheet.cost_ready, "有批次 ⇒ 对拍一列有意义");
        assert_eq!(sheet.batch.as_ref().map(|b| b.status.as_str()), Some("DRAFT"));
        let row = row_of(&sheet, &f.object_no("V3"));
        let calc = row.calc.as_ref().expect("有计算单就该挂快照");
        assert!(calc.agrees, "同一组输入算出的同一组约当，必须一致：{:?}", calc.drift);
        assert!(calc.drift.is_empty());
        assert_eq!(qty(&calc.equiv_material), dec("1000"));
        assert_eq!(qty(&calc.equiv_conversion), dec("900"));
        assert!(!row.recalc_required);
        assert_eq!(sheet.totals.drift_row_count, 0);

        f.cleanup(&[object], &[batch]).await;
    }

    // ==================== 4. 算完又改完工率 ⇒ 漂移 ====================

    #[tokio::test]
    async fn v4_touching_pct_after_calc_flags_drift() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V4", "OPEN", "800", "200", "50").await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(object, batch, "CALC", "800", "200", "50").await;
        // 算完又把完工率从 50% 改成 60%（在制数量没变）
        f.patch_object(object, "200", "60").await;

        let table = sheet(&f.super_principal(), &f.periods[0], &f.tag("V4"), None, None, None).await;
        let row = row_of(&table, &f.object_no("V4"));
        assert!(row.recalc_required, "改过完工率就要重算");
        let calc = row.calc.as_ref().expect("计算单快照");
        assert!(!calc.agrees);
        let pct_drift = calc
            .drift
            .iter()
            .find(|item| item.field == "完工率")
            .unwrap_or_else(|| panic!("必须报完工率差异：{:?}", calc.drift));
        assert_eq!(qty(&pct_drift.table_value), dec("60"));
        assert_eq!(qty(&pct_drift.calc_value), dec("50"));
        // 约当也跟着变了：加工约当 800 + 200 × 60% = 920（计算单还是 900）
        assert_eq!(row.equiv_conversion.as_deref().map(qty), Some(dec("920")));
        assert!(
            calc.drift.iter().any(|item| item.field == "加工约当"),
            "约当变了也要报出来：{:?}",
            calc.drift
        );
        assert!(calc.note.contains("重算"), "说明要指向重算：{}", calc.note);

        // 四档筛选里的 drift 能一屏筛出来
        let drifted = sheet(
            &f.super_principal(),
            &f.periods[0],
            &f.tag("V4"),
            Some("drift"),
            None,
            None,
        )
        .await;
        assert_eq!(drifted.total, 1);
        assert_eq!(drifted.list[0].object_no, f.object_no("V4"));
        assert_eq!(table.totals.drift_row_count, 1);

        f.cleanup(&[object], &[batch]).await;
    }

    // ==================== 5. 在制 > 0 但完工率 0 ⇒ 算不出来（不是 0） ====================

    #[tokio::test]
    async fn v5_wip_without_pct_is_blocked_not_zero() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V5", "OPEN", "100", "200", "0").await;

        let table = sheet(&f.super_principal(), &f.periods[0], &f.tag("V5"), None, None, None).await;
        let row = row_of(&table, &f.object_no("V5"));
        assert_eq!(row.state, "blocked");
        assert!(
            row.issues.iter().any(|issue| issue.code == "WIP_PCT_MISSING"),
            "必须指出缺完工率：{:?}",
            row.issues
        );
        // 材料侧不看完工率 ⇒ 有数；加工侧算不出来 ⇒ null（**不是 0**）
        assert_eq!(row.equiv_material.as_deref().map(qty), Some(dec("300")));
        assert!(
            row.equiv_conversion.is_none(),
            "加工约当给 null 不给 0：{:?}",
            row.equiv_conversion
        );
        assert!(row.wip_equiv_conversion.is_none());
        // 合计：数量进、约当整行不进（只加材料侧会给出「半个分母」）
        assert_eq!(table.totals.blocked_row_count, 1);
        assert_eq!(qty(&table.totals.wip_qty), dec("200"), "在制数量是事实，照样进合计");
        assert_eq!(qty(&table.totals.equiv_material), dec("0"), "算不出来的行不进约当合计");
        assert_eq!(qty(&table.totals.equiv_conversion), dec("0"));

        // blocked 档筛得到它
        let blocked = sheet(
            &f.super_principal(),
            &f.periods[0],
            &f.tag("V5"),
            Some("blocked"),
            None,
            None,
        )
        .await;
        assert_eq!(blocked.total, 1);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 6. 有归集无产量 ⇒ 在表里、标 NO_OUTPUT ====================

    #[tokio::test]
    async fn v6_cost_without_output_is_listed_and_blocked() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V6", "OPEN", "0", "0", "0").await;
        f.collect(object, "1234.5678", &f.periods[0]).await;

        let sheet = sheet(&f.super_principal(), &f.periods[0], &f.tag("V6"), None, None, None).await;
        let row = row_of(&sheet, &f.object_no("V6"));
        assert_eq!(row.state, "blocked", "有投入没产量：最需要人看一眼的那一行");
        assert!(
            row.issues.iter().any(|issue| issue.code == "NO_OUTPUT"),
            "必须标 NO_OUTPUT：{:?}",
            row.issues
        );
        assert!(row.equiv_material.is_none() && row.equiv_conversion.is_none());
        assert_eq!(sheet.totals.blocked_row_count, 1);
        assert!(sheet.note.contains("算不出来"), "页脚要点出这类行：{}", sheet.note);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 7. 没建批次 ⇒ 试算视图照出 ====================

    #[tokio::test]
    async fn v7_no_batch_still_shows_the_trial_sheet() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V7", "OPEN", "0", "400", "40").await;

        let sheet = sheet(&f.super_principal(), &f.periods[1], &f.tag("V7"), None, None, None).await;
        assert!(!sheet.cost_ready);
        assert!(sheet.batch.is_none(), "没批次就回 null，不要编一个批次号");
        let row = row_of(&sheet, &f.object_no("V7"));
        assert!(row.calc.is_none(), "没批次就没有对拍快照");
        assert!(!row.recalc_required, "没得比就不该报漂移");
        // 本期无完工、全在制：材料 400、加工 400 × 40% = 160
        assert_eq!(row.equiv_material.as_deref().map(qty), Some(dec("400")));
        assert_eq!(row.equiv_conversion.as_deref().map(qty), Some(dec("160")));
        assert_eq!(row.state, "wip");
        assert!(
            row.warnings.iter().any(|w| w.contains("本期没有完工")),
            "全在制要提示一句：{:?}",
            row.warnings
        );
        assert!(sheet.note.contains("试算"));

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

        // 超管 / 财务看得见
        let mine = sheet(&f.super_principal(), &f.periods[0], &f.tag("V8"), None, None, None).await;
        assert_eq!(mine.total, 1);

        // 别车间（本部门范围）看不见，而且**不是 403**：列表没有探测口
        let other = sheet(&f.other_dept_principal(), &f.periods[0], &f.tag("V8"), None, None, None).await;
        assert_eq!(other.total, 0, "别车间的对象不该出现在表里");
        assert!(other.list.is_empty());
        assert_eq!(other.totals.row_count, 0, "合计也不能泄漏行数");

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 9. 合计是整张表 + 四档筛选 ====================

    #[tokio::test]
    async fn v9_totals_are_the_whole_table_and_filters_narrow() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        // 一行有在制 / 一行全完工 / 一行算不出来
        let wip = f.object("V9W", "OPEN", "0", "100", "50").await;
        let finished = f.object("V9F", "OPEN", "800", "0", "0").await;
        let blocked = f.object("V9B", "OPEN", "100", "200", "0").await;

        // 关键字用「-后缀」把本夹具三行全收进来（对象号是 CB-M520-V9W/V9F/V9B-后缀）
        let keyword = f.tag("");

        // page_size = 1：合计仍是整张表（不受分页影响）
        let page1 = sheet(&f.super_principal(), &f.periods[0], &keyword, None, Some(1), Some(1)).await;
        assert_eq!(page1.total, 3);
        assert_eq!(page1.list.len(), 1, "每页 1 行");
        assert_eq!(page1.totals.row_count, 3, "合计是整张表的");
        assert_eq!(page1.totals.wip_row_count, 1);
        assert_eq!(page1.totals.finished_row_count, 1);
        assert_eq!(page1.totals.blocked_row_count, 1);
        // 完工合计 = 0 + 800 + 100；在制合计 = 100 + 0 + 200
        assert_eq!(qty(&page1.totals.completed_qty), dec("900"));
        assert_eq!(qty(&page1.totals.wip_qty), dec("300"));
        // 材料约当合计 = 100 + 800（blocked 行整行不进约当合计）
        assert_eq!(qty(&page1.totals.equiv_material), dec("900"));
        // 加工约当合计 = 50 + 800
        assert_eq!(qty(&page1.totals.equiv_conversion), dec("850"));

        // 第二页是另一行，合计不变
        let page2 = sheet(&f.super_principal(), &f.periods[0], &keyword, None, Some(2), Some(1)).await;
        assert_eq!(page2.list.len(), 1);
        assert_ne!(page1.list[0].object_no, page2.list[0].object_no);
        assert_eq!(page2.totals.row_count, 3, "翻页不改合计");

        // 四档筛选
        let only_wip = sheet(&f.super_principal(), &f.periods[0], &keyword, Some("wip"), None, None).await;
        assert_eq!(only_wip.total, 1);
        assert_eq!(only_wip.list[0].object_no, f.object_no("V9W"));
        let only_finished = sheet(
            &f.super_principal(),
            &f.periods[0],
            &keyword,
            Some("finished"),
            None,
            None,
        )
        .await;
        assert_eq!(only_finished.total, 1);
        assert_eq!(only_finished.list[0].object_no, f.object_no("V9F"));
        let only_blocked = sheet(
            &f.super_principal(),
            &f.periods[0],
            &keyword,
            Some("blocked"),
            None,
            None,
        )
        .await;
        assert_eq!(only_blocked.total, 1);
        assert_eq!(only_blocked.list[0].object_no, f.object_no("V9B"));
        // 没有漂移行（没批次）⇒ drift 档空，且合计跟着筛选走（筛完的整张表）
        let only_drift = sheet(&f.super_principal(), &f.periods[0], &keyword, Some("drift"), None, None).await;
        assert_eq!(only_drift.total, 0);
        assert_eq!(only_drift.totals.row_count, 0);

        f.cleanup(&[wip, finished, blocked], &[]).await;
    }

    // ==================== 10. 清场零残留 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("ZZ", "OPEN", "800", "200", "50").await;
        f.collect(object, "1000", &f.periods[0]).await;
        let batch = f.period_batch(&f.periods[0], "DRAFT").await;
        f.calc_item(object, batch, "CALC", "800", "200", "50").await;
        assert!(f.residue().await > 0, "夹具造出来就该有行");

        f.cleanup(&[object], &[batch]).await;
        assert_eq!(f.residue().await, 0, "清场后本卡造的行必须全 0");
    }
}
