//! 分批法「在制批次余额」（M5-05）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m505_dev \
//!   cargo test --lib cost_batch_wip -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`v1_cutoff_excludes_later_periods`] | 余额是「截至某期」的时点量：之后的归集不算进来 |
//! | 2 | [`v2_balance_is_collected_minus_transferred`] | 余额 = 累计归集 − 累计结转（逐要素），本期投入 / 转出另算 |
//! | 3 | [`v3_full_transfer_leaves_zero_balance`] | 全额结转后余额归零、`has_balance = false`、`only_wip` 不再列它 |
//! | 4 | [`v4_real_finish_transfer_zeroes_the_balance`] | **走 M5-04 真接口**结转一次 ⇒ 台账末行期末 = 0（两卡口径闭环） |
//! | 5 | [`v5_ledger_rolls_across_periods`] | 逐期滚动：每行期初 = 上一行期末；末行 = 截止期间；空月补 0 行 |
//! | 6 | [`v6_ledger_summary_equals_balance_sheet_row`] | 台账汇总余额 == 余额表同一行的余额（同一个数两种看法） |
//! | 7 | [`v7_partially_completed_batch_keeps_its_balance`] | **部分完工**（`wip_qty > 0`）成本全留在在制、不报警告 |
//! | 8 | [`v8_finished_but_not_transferred_is_flagged`] | 完工未结转 ⇒ `FINISHED_NOT_TRANSFERRED` |
//! | 9 | [`v9_voided_collect_after_transfer_turns_balance_negative`] | 结转后归集被软删 ⇒ 负余额 + `NEGATIVE_BALANCE` + 合计 `negative_batch_count` |
//! | 10 | [`v10_out_of_scope_is_404_and_absent_from_the_sheet`] | 别车间用户：台账 404、余额表不列（不报 403，不当探测口） |
//! | 11 | [`v11_later_activity_is_reported`] | 截止期之后的归集 / 结转 ⇒ `after_period` + `LATER_PERIOD_ACTIVITY` |
//! | 12 | [`v12_settled_item_is_attached_and_explained`] | 已结算计算单：挂出结算口径在制 + `SETTLED_BY_PERIOD` 说明 |
//! | 13 | [`v13_totals_are_the_whole_table_not_the_page`] | 表尾合计是整张表（`page_size = 1` 时 `batch_count = 3`） |
//! | 14 | [`zz_teardown_leaves_zero_residue`] | 清场后本卡造的行全 0 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个期间槽（年份 = `2000 + slot % 70`，期间取该年 01/02/03 三个月）；
//!   建前先清该槽上一次的残留；
//! * 单号 / 对象号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**，
//!   绝不按「全表有多少行」断言（别的卡的夹具也在同一个库里）；
//! * 归集行 / 结转单 / 计算单 / 期间批次**直接 SQL 造**：本卡验的是「读口径」，
//!   要造的形态（跨期归集、已结算计算单、软删归集行）API 造不出来或要绕很远；
//!   只有 V4 一条**故意**走 `cost_batch_finish::finish` 真接口，用来锁两卡口径一致。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_batch_wip_dto::{self as dto, WipLedgerQuery, WipQuery};
    use crate::business::erp::cost::cost_batch_wip_handler::{balance_of, ledger_of};
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
        Decimal::from_str(text).expect("金额字面量")
    }

    /// 出参里的金额是字符串：按 Decimal 比，避免「1000 vs 1000.0000」这种标度假红。
    fn amount(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("金额字符串无法解析：{text}"))
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
        /// 三个连续期间 01 / 02 / 03（余额表 / 台账的期间轴）。
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
            let other_dept_code = format!("M505-DEPT2-{suffix}");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &other_dept_code,
                &format!("M5-05 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M505-MAT-{suffix}"),
                &format!("M5-05 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M55W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-05 夹具仓 {suffix}"),
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
            format!("CB-M505-{}", self.tag(name))
        }

        /// 造成本对象（完工 / 在制 / 状态由用例给）。
        async fn object(&self, name: &str, status: &str, completed: &str, wip: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 800, ?::text::numeric, ?::text::numeric, \
                             ?::uuid, ?, '{}-01', ?, {closed_at}, now(), now())",
                    self.periods[0]
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&self.product),
                    Value::Text(format!(
                        "LOT-M505-{name}-{}",
                        crate::test_seed::last_chars(&self.suffix, 6)
                    )),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 造一条归集行（料要物料 + 数量；工 / 费不许挂物料），期间可指定。
        async fn collect(&self, object: Uuid, element: &str, amount: &str, period: &str) -> Uuid {
            let id = Uuid::now_v7();
            let (material, qty) = if element == "MATERIAL" {
                ("?::uuid".to_string(), "?::text::numeric".to_string())
            } else {
                ("NULL".to_string(), "NULL".to_string())
            };
            let mut params = vec![
                Value::from(&id),
                Value::from(&object),
                Value::Text(period.to_string()),
                Value::Text(element.to_string()),
                Value::Text(format!("{period}-10")),
            ];
            if element == "MATERIAL" {
                params.push(Value::from(&self.product));
                params.push(Value::Text("10".to_string()));
            }
            params.push(Value::Text(amount.to_string()));
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                   line_no, occur_date, material_id, qty, amount, \
                                                   remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?, 'MANUAL', 0, ?::text::date, {material}, {qty}, \
                             ?::text::numeric, 'M5-05 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集行夹具");
            id
        }

        /// 直接造一张结转单（读口径用例用；V4 走真接口）。
        async fn finish(&self, object: Uuid, period: &str, seq: i32, mat: &str, labor: &str, oh: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_batch_finish (id, object_id, object_no, period_code, transfer_seq, \
                                                    product_id, lot_no, finished_qty, unit_cost, \
                                                    collected_material, collected_labor, collected_overhead, collected_total, \
                                                    material_amount, labor_amount, overhead_amount, total_amount, \
                                                    remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, NULL, 800, 0, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         (?::text::numeric + ?::text::numeric + ?::text::numeric), \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         (?::text::numeric + ?::text::numeric + ?::text::numeric), \
                         'M5-05 夹具 结转', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&object),
                    Value::Text(self.object_no("fin")),
                    Value::Text(period.to_string()),
                    Value::I64(i64::from(seq)),
                    Value::from(&self.product),
                    Value::Text(mat.to_string()),
                    Value::Text(labor.to_string()),
                    Value::Text(oh.to_string()),
                    Value::Text(mat.to_string()),
                    Value::Text(labor.to_string()),
                    Value::Text(oh.to_string()),
                    Value::Text(mat.to_string()),
                    Value::Text(labor.to_string()),
                    Value::Text(oh.to_string()),
                    Value::Text(mat.to_string()),
                    Value::Text(labor.to_string()),
                    Value::Text(oh.to_string()),
                ],
            )
            .await
            .expect("造结转单夹具");
            id
        }

        /// 造一张成本核算批次（`DRAFT` 就够了：本卡只读，不碰状态闸）。
        async fn period_batch(&self, period: &str, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period.to_string()),
                    Value::Text(format!("CS-M505-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        /// 造一行 `SETTLED` 计算单（`ck_biz_cost_item_settled` 要求挂入库单；
        /// `ck_biz_cost_item_cons` 要求 `完工 + 在制 = 本期投入`，所以 `mat_cur` 必须是两者之和）。
        async fn settled_item(&self, object: Uuid, batch: Uuid, finished: &str, wip_end: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                   mat_cur, finished_cost, wip_end_cost, status, stock_in_id, \
                                                   created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, \
                         (?::text::numeric + ?::text::numeric), ?::text::numeric, ?::text::numeric, 'SETTLED', \
                         '00000000-0000-0000-0000-000000000003'::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&batch),
                    Value::from(&object),
                    Value::from(&self.product),
                    Value::Text(finished.to_string()),
                    Value::Text(wip_end.to_string()),
                    Value::Text(finished.to_string()),
                    Value::Text(wip_end.to_string()),
                ],
            )
            .await
            .expect("造计算单夹具");
            id
        }

        /// 余额表查询（关键字收窄到本夹具，绝不按全表行数断言）。
        fn sheet_query(&self, period: &str, name: &str) -> WipQuery {
            WipQuery {
                period_code: Some(period.to_string()),
                keyword: Some(self.tag(name)),
                ..Default::default()
            }
        }

        fn ledger_query(&self, period: &str) -> WipLedgerQuery {
            WipLedgerQuery {
                period_code: Some(period.to_string()),
            }
        }

        /// 清掉该槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let first = &periods[0];
            let third = &periods[2];
            for table in [
                "DELETE FROM biz_cost_batch_finish WHERE remark LIKE 'M5-05 夹具%' AND period_code BETWEEN ? AND ?",
                "DELETE FROM biz_cost_collect WHERE remark = 'M5-05 夹具' AND period_code BETWEEN ? AND ?",
            ] {
                let _ = db()
                    .exec_sql(
                        table.to_string(),
                        vec![Value::Text(first.clone()), Value::Text(third.clone())],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                       (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M505-%' \
                          AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months'))"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M505-%' \
                        AND start_date BETWEEN (? || '-01')::date AND ((? || '-01')::date + interval '3 months')"
                        .to_string(),
                    vec![Value::Text(first.clone()), Value::Text(first.clone())],
                )
                .await;
        }

        /// 清场（FK 顺序：结转单 → 计算单 → 归集 → 期间批次 → 对象）。
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
                format!("DELETE FROM biz_cost_batch_finish WHERE object_id IN ({objects_sql})"),
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
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE ?) + \
                        (SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![
                    Value::Text(format!("%{}", self.tag(""))),
                    Value::Text(format!("%{}", self.tag(""))),
                ],
            )
            .await
            .expect("查残留")
        }
    }

    /// 从余额表里按对象号取一行（找不到就是夹具没造对，直接失败）。
    fn row_of<'a>(sheet: &'a dto::WipBalanceRes, object_no: &str) -> &'a dto::WipRowRes {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("余额表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    // ==================== 1. 截止期间 ====================

    #[tokio::test]
    async fn v1_cutoff_excludes_later_periods() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V1", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "MATERIAL", "500", &f.periods[1]).await;

        let sheet1 = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V1"))
            .await
            .expect("余额表（1 月）");
        let row1 = row_of(&sheet1, &f.object_no("V1"));
        assert_eq!(amount(&row1.collected.total), dec("1000"), "1 月的累计只含 1 月");
        assert_eq!(amount(&row1.balance.total), dec("1000"));
        assert_eq!(amount(&row1.period_in.total), dec("1000"), "本期投入 = 1 月");

        let sheet2 = balance_of(&f.super_principal(), f.sheet_query(&f.periods[1], "V1"))
            .await
            .expect("余额表（2 月）");
        let row2 = row_of(&sheet2, &f.object_no("V1"));
        assert_eq!(amount(&row2.collected.total), dec("1500"), "2 月的累计含 1+2 月");
        assert_eq!(amount(&row2.balance.total), dec("1500"));
        assert_eq!(amount(&row2.period_in.total), dec("500"), "本期投入只有 2 月");

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 2. 余额 = 归集 − 结转 ====================

    #[tokio::test]
    async fn v2_balance_is_collected_minus_transferred() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V2", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "LABOR", "200", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "300", &f.periods[0]).await;
        f.finish(object, &f.periods[0], 1, "400", "100", "0").await;

        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V2"))
            .await
            .expect("余额表");
        let row = row_of(&sheet, &f.object_no("V2"));
        assert_eq!(amount(&row.collected.material), dec("1000"));
        assert_eq!(amount(&row.collected.total), dec("1500"));
        assert_eq!(amount(&row.transferred.total), dec("500"));
        assert_eq!(amount(&row.balance.material), dec("600"));
        assert_eq!(amount(&row.balance.labor), dec("100"));
        assert_eq!(amount(&row.balance.overhead), dec("300"));
        assert_eq!(amount(&row.balance.total), dec("1000"));
        assert_eq!(row.transfer_times, 1);
        assert!(row.has_balance);
        assert!(row.warnings.is_empty(), "在产且余额正常不该报警告：{:?}", row.warnings);
        // 表尾合计与行同源
        assert_eq!(amount(&sheet.totals.balance.total), dec("1000"));
        assert_eq!(sheet.totals.batch_count, 1);
        assert_eq!(sheet.totals.wip_batch_count, 1);
        assert_eq!(sheet.totals.negative_batch_count, 0);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 3. 全额结转归零 ====================

    #[tokio::test]
    async fn v3_full_transfer_leaves_zero_balance() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V3", "CLOSED", "800", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.finish(object, &f.periods[0], 1, "1000", "0", "0").await;

        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V3"))
            .await
            .expect("余额表");
        let row = row_of(&sheet, &f.object_no("V3"));
        assert_eq!(amount(&row.balance.total), dec("0"));
        assert!(!row.has_balance, "已全额结转 ⇒ 没有余额");

        // only_wip 过滤后不再列它
        let mut query = f.sheet_query(&f.periods[0], "V3");
        query.only_wip = Some(true);
        let only = balance_of(&f.super_principal(), query)
            .await
            .expect("余额表（only_wip）");
        assert!(only.list.is_empty(), "only_wip 不该列出余额为 0 的批次");
        assert_eq!(only.totals.batch_count, 0);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 4. 走真接口结转 ====================

    #[tokio::test]
    async fn v4_real_finish_transfer_zeroes_the_balance() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V4", "OPEN", "800", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "LABOR", "200", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "300", &f.periods[0]).await;

        let before = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账（结转前）");
        assert_eq!(amount(&before.summary.balance.total), dec("1500"));

        let finish = crate::business::erp::cost::cost_batch_finish::finish(
            &f.super_principal(),
            Some(Uuid::now_v7()),
            &object.to_string(),
            crate::business::erp::cost::cost_batch_finish_dto::BatchFinishReq {
                period_code: Some(f.periods[0].clone()),
                remark: Some("M5-05 夹具 真结转".to_string()),
            },
        )
        .await
        .expect("M5-04 完工结转");
        assert!(finish.inserted, "首次结转必须真写一行");

        let after = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账（结转后）");
        assert_eq!(amount(&after.summary.balance.total), dec("0"), "结转后余额必须归零");
        assert_eq!(amount(&after.periods.last().expect("末行").closing.total), dec("0"));
        assert_eq!(after.summary.transfer_times, 1);
        assert!(after.warnings.is_empty(), "结转干净后不该有警告：{:?}", after.warnings);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 5. 逐期滚动 ====================

    #[tokio::test]
    async fn v5_ledger_rolls_across_periods() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V5", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "MATERIAL", "500", &f.periods[1]).await;
        f.finish(object, &f.periods[1], 1, "1000", "0", "0").await;

        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[2]))
            .await
            .expect("台账（截至 3 月）");
        assert_eq!(ledger.periods.len(), 3, "01/02/03 三期（3 月无发生也要补 0 行）");
        assert_eq!(ledger.periods[0].period_code, f.periods[0]);
        assert_eq!(ledger.periods[2].period_code, f.periods[2]);
        assert_eq!(amount(&ledger.periods[0].closing.total), dec("1000"));
        assert_eq!(amount(&ledger.periods[1].opening.total), dec("1000"), "期初 = 上期期末");
        assert_eq!(amount(&ledger.periods[1].closing.total), dec("500"));
        assert_eq!(amount(&ledger.periods[2].opening.total), dec("500"));
        assert_eq!(amount(&ledger.periods[2].closing.total), dec("500"));
        assert_eq!(ledger.periods[1].transfer_times, 1);
        assert_eq!(ledger.periods[2].collect_lines, 0, "3 月没有归集");
        assert_eq!(amount(&ledger.summary.collected.total), dec("1500"));
        assert_eq!(amount(&ledger.summary.transferred.total), dec("1000"));
        assert_eq!(amount(&ledger.summary.balance.total), dec("500"));
        assert!(!ledger.truncated);
        assert!(ledger.after_period.is_none(), "截至 3 月之后没有发生");
        assert!(ledger.notes.iter().any(|note| note.contains(&f.periods[2])));

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 6. 台账汇总 == 余额表行 ====================

    #[tokio::test]
    async fn v6_ledger_summary_equals_balance_sheet_row() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V6", "OPEN", "0", "300").await;
        f.collect(object, "MATERIAL", "777.1234", &f.periods[0]).await;
        f.collect(object, "LABOR", "88.0001", &f.periods[1]).await;
        f.collect(object, "OVERHEAD", "0.0003", &f.periods[1]).await;
        f.finish(object, &f.periods[1], 1, "100.0004", "0", "0").await;

        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[1], "V6"))
            .await
            .expect("余额表");
        let row = row_of(&sheet, &f.object_no("V6"));
        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[1]))
            .await
            .expect("台账");

        for (left, right, name) in [
            (&row.collected.material, &ledger.summary.collected.material, "料"),
            (&row.collected.labor, &ledger.summary.collected.labor, "工"),
            (&row.collected.overhead, &ledger.summary.collected.overhead, "费"),
            (&row.transferred.total, &ledger.summary.transferred.total, "已结转"),
            (&row.balance.total, &ledger.summary.balance.total, "余额"),
        ] {
            assert_eq!(amount(left), amount(right), "{name}：余额表与台账必须同数");
        }
        assert_eq!(row.collect_lines, ledger.summary.collect_lines);
        assert_eq!(row.transfer_times, ledger.summary.transfer_times);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 7. 部分完工留余额 ====================

    #[tokio::test]
    async fn v7_partially_completed_batch_keeps_its_balance() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        // 批 800 件：完工 500、在制 300（部分完工）
        let object = f.object("V7", "OPEN", "500", "300").await;
        f.collect(object, "MATERIAL", "2000", &f.periods[0]).await;
        f.collect(object, "LABOR", "400", &f.periods[0]).await;
        f.collect(object, "OVERHEAD", "600", &f.periods[0]).await;

        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V7"))
            .await
            .expect("余额表");
        let row = row_of(&sheet, &f.object_no("V7"));
        assert_eq!(amount(&row.balance.total), dec("3000"), "部分完工 ⇒ 成本全留在在制");
        assert!(row.has_balance);
        assert!(
            row.warnings.is_empty(),
            "部分完工且有成本是正常形态：{:?}",
            row.warnings
        );
        assert_eq!(amount(&row.wip_qty), dec("300"));
        assert_eq!(amount(&row.completed_qty), dec("500"));

        // 结转接口也必须拦住它（M5-04 的口径：wip_qty > 0 一律不许结转）
        let preview = crate::business::erp::cost::cost_batch_finish::preview(
            &f.super_principal(),
            &object.to_string(),
            crate::business::erp::cost::cost_batch_finish_dto::BatchFinishQuery {
                period_code: Some(f.periods[0].clone()),
            },
        )
        .await
        .expect("结转试算");
        assert!(!preview.transferable, "还有在制 ⇒ 不许结转（两卡口径一致）");

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 8. 完工未结转 ====================

    #[tokio::test]
    async fn v8_finished_but_not_transferred_is_flagged() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V8", "OPEN", "800", "0").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;

        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账");
        let warning = ledger
            .warnings
            .iter()
            .find(|warning| warning.code == dto::WARN_FINISHED_NOT_TRANSFERRED)
            .unwrap_or_else(|| panic!("少了完工未结转警告：{:?}", ledger.warnings));
        assert_eq!(warning.level, dto::LEVEL_WARN);
        assert!(warning.hint.contains("/finish"), "提示要给出结转接口：{}", warning.hint);

        // 列表行里也带同一个码（两处判据同源）
        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V8"))
            .await
            .expect("余额表");
        assert!(
            row_of(&sheet, &f.object_no("V8"))
                .warnings
                .iter()
                .any(|code| code == dto::WARN_FINISHED_NOT_TRANSFERRED)
        );

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 9. 负余额 ====================

    #[tokio::test]
    async fn v9_voided_collect_after_transfer_turns_balance_negative() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V9", "CLOSED", "800", "0").await;
        let collect = f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.finish(object, &f.periods[0], 1, "1000", "0", "0").await;

        // 结转之后归集行被作废（软删）—— 账就负了
        db().exec_sql(
            "UPDATE biz_cost_collect SET deleted_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&collect)],
        )
        .await
        .expect("软删归集行");

        let sheet = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V9"))
            .await
            .expect("余额表");
        let row = row_of(&sheet, &f.object_no("V9"));
        assert_eq!(
            amount(&row.balance.material),
            dec("-1000"),
            "负余额必须原样显示，不夹零"
        );
        assert!(row.warnings.iter().any(|code| code == dto::WARN_NEGATIVE_BALANCE));
        assert!(!row.has_balance, "负余额不算「还有余额」");
        assert_eq!(sheet.totals.negative_batch_count, 1, "合计要数出负余额批次");

        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账");
        assert_eq!(amount(&ledger.summary.balance.material), dec("-1000"));
        let warning = ledger
            .warnings
            .iter()
            .find(|warning| warning.code == dto::WARN_NEGATIVE_BALANCE)
            .expect("台账也要报负余额");
        assert!(warning.message.contains("料"), "{}", warning.message);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 10. 数据范围 ====================

    #[tokio::test]
    async fn v10_out_of_scope_is_404_and_absent_from_the_sheet() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V10", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;

        // 别车间用户：台账 404（不报 403 —— 403 等于承认 id 存在）
        let error = ledger_of(
            &f.other_dept_principal(),
            &object.to_string(),
            f.ledger_query(&f.periods[0]),
        )
        .await
        .expect_err("不可见必须 404");
        assert_eq!(error.status, 404);

        // 余额表也不列它
        let sheet = balance_of(&f.other_dept_principal(), f.sheet_query(&f.periods[0], "V10"))
            .await
            .expect("余额表（别车间）");
        assert!(sheet.list.is_empty(), "别车间看不到本批");
        assert_eq!(sheet.totals.batch_count, 0);

        // 自己的车间看得到（对照组：404 来自范围，不是数据不存在）
        let own = balance_of(&f.super_principal(), f.sheet_query(&f.periods[0], "V10"))
            .await
            .expect("余额表（本车间）");
        assert_eq!(own.list.len(), 1);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 11. 截止期之后的发生 ====================

    #[tokio::test]
    async fn v11_later_activity_is_reported() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V11", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1000", &f.periods[0]).await;
        f.collect(object, "MATERIAL", "500", &f.periods[1]).await;
        f.finish(object, &f.periods[2], 1, "200", "0", "0").await;

        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账（截至 1 月）");
        assert_eq!(
            amount(&ledger.summary.balance.total),
            dec("1000"),
            "之后的发生不算进 1 月余额"
        );
        let after = ledger.after_period.expect("之后有发生，必须报出来");
        assert_eq!(amount(&after.collected_total), dec("500"));
        assert_eq!(amount(&after.transferred_total), dec("200"));
        let warning = ledger
            .warnings
            .iter()
            .find(|warning| warning.code == dto::WARN_LATER_PERIOD_ACTIVITY)
            .expect("要有一条「之后还有发生」的说明");
        assert_eq!(warning.level, dto::LEVEL_INFO);

        f.cleanup(&[object], &[]).await;
    }

    // ==================== 12. 已结算计算单 ====================

    #[tokio::test]
    async fn v12_settled_item_is_attached_and_explained() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("V12", "OPEN", "0", "200").await;
        f.collect(object, "MATERIAL", "1600", &f.periods[1]).await;
        let batch = f.period_batch(&f.periods[1], "DRAFT").await;
        f.settled_item(object, batch, "1000", "600").await;

        let ledger = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[1]))
            .await
            .expect("台账（2 月）");
        let settled = ledger.settled.as_ref().expect("要挂出已结算计算单");
        assert_eq!(settled.period_code, f.periods[1]);
        assert_eq!(amount(&settled.wip_end_cost), dec("600"));
        let note = ledger
            .warnings
            .iter()
            .find(|warning| warning.code == dto::WARN_SETTLED_BY_PERIOD)
            .expect("要解释两个口径");
        assert_eq!(note.level, dto::LEVEL_INFO);

        // 截止期间早于结算期间 ⇒ 看不到它（台账是「截至 P」的快照）
        let earlier = ledger_of(&f.super_principal(), &object.to_string(), f.ledger_query(&f.periods[0]))
            .await
            .expect("台账（1 月）");
        assert!(earlier.settled.is_none());

        f.cleanup(&[object], &[batch]).await;
    }

    // ==================== 13. 表尾合计是整张表 ====================

    #[tokio::test]
    async fn v13_totals_are_the_whole_table_not_the_page() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let mut objects = Vec::new();
        for (index, amount_text) in ["1000", "2000", "3000"].iter().enumerate() {
            let object = f.object(&format!("V13{index}"), "OPEN", "0", "100").await;
            f.collect(object, "MATERIAL", amount_text, &f.periods[0]).await;
            objects.push(object);
        }

        // 三个对象号形如 CB-M505-V130-<后缀>：用后缀本身收窄（前缀关键字会把它们分成三组）
        let query = WipQuery {
            period_code: Some(f.periods[0].clone()),
            keyword: Some(crate::test_seed::last_chars(&f.suffix, 8)),
            page_size: Some(1),
            ..Default::default()
        };
        let sheet = balance_of(&f.super_principal(), query)
            .await
            .expect("余额表（一页一条）");
        assert_eq!(sheet.list.len(), 1, "页大小 1 ⇒ 只返回一条");
        assert_eq!(sheet.total, 3);
        assert_eq!(sheet.totals.batch_count, 3, "合计是整张表");
        assert_eq!(amount(&sheet.totals.balance.total), dec("6000"));
        assert_eq!(sheet.totals.wip_batch_count, 3);

        f.cleanup(&objects, &[]).await;
    }

    // ==================== 14. 清场 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("ZZ", "OPEN", "0", "100").await;
        f.collect(object, "MATERIAL", "100", &f.periods[0]).await;
        f.finish(object, &f.periods[0], 1, "50", "0", "0").await;
        assert!(f.residue().await > 0, "造完夹具应当有残留");

        f.cleanup(&[object], &[]).await;
        assert_eq!(f.residue().await, 0, "清场后本夹具的对象与归集行必须全 0");
    }
}
