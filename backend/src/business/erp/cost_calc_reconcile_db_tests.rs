//! calc 第 4 段（池与 `4101.*` 对账）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_reconcile_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`every_booked_route_is_balanced_and_manual_ticket_is_pending`] | **验收项「差额必须为 0」**：折旧 + 报销池 = 账面 4101.* 借方，逐组平；手工台账计「待结算过账」 |
//! | 2 | [`bypass_voucher_is_flagged_with_its_voucher_no`] | **验收项「不为 0 → 报错并列凭证号」**：旁路凭证 409 + `66065`，报文点名凭证号与科目 |
//! | 3 | [`voucher_without_department_is_flagged`] | 4101 借方没挂车间 ⇒ 单独成组，报文写「（无车间）」 |
//! | 4 | [`reversed_red_draft_and_non_4101_vouchers_are_ignored`] | 已红冲凭证 / 红字凭证 / DRAFT / 5602.* 全不进账面 |
//! | 5 | [`previous_period_voucher_does_not_leak`] | 上期凭证不进本期账面 |
//! | 6 | [`partial_booking_is_a_mismatch`] | 账面只记一部分 ⇒ 1000 vs 800，差额 200 |
//! | 7 | [`offsetting_groups_do_not_cancel_out`] | 总额相抵但逐组不平 ⇒ 仍报错（C-COST-1 按组比） |
//! | 8 | [`pool_by_source_matches_the_collect_segment_pool`] | 对账池侧 = 费归集段 `pool_of_period`（同口径互校）+ 与账面对平 |
//! | 9 | [`empty_period_is_balanced`] | 空期间 0 组、平、不报错 |
//! | 10 | [`settle_stage_requires_the_manual_ticket_to_be_booked`] | 阶段口径：同一批数据 Calc 平、Settle 不平（第四张凭证还没过账） |
//!
//! # 夹具纪律
//!
//! - 每个用例独占一个**期间**（年份 = `2100 + slot % 60`、月份 = `1 + (slot / 60) % 12`），
//!   否则池汇总 / 账面发生额会把别的用例残留算进来；
//! - 清场按「前缀 + 期间」与「本夹具登记的 id」删，绝不按期间盲删（全仓测试并行时
//!   按期间盲删会误伤同模块别的用例）；
//! - 池行由**真跑第 3 段**（`cost_calc_overhead::collect_overhead_in_tx`）写出，不手插
//!   `biz_cost_collect` —— 对账要验的就是「归集段写出来的池」与「凭证借贷」的勾稽；
//! - 凭证直接 SQL 造（要造出 `POSTED`/`DRAFT`/红字/已红冲/无部门这些
//!   `voucher_write` 正常路径造不出的历史形态），本卡验的是**读口径**，不是凭证引擎。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_overhead as collect_segment;
    use crate::business::erp::cost_calc_reconcile as segment;
    use crate::business::erp::cost_overhead_collect_model;
    use crate::business::erp::cost_reconcile_dto::{GroupKind, Stage};
    use crate::business::erp::cost_reconcile_model;
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
        Decimal::from_str(text).expect("金额字面量")
    }

    /// 进程内递增槽位：决定夹具独占的会计期间。
    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// uuid 尾 12 位（v7 前 8 位是时间戳，同毫秒连造会撞唯一键）。
    fn tail(id: &Uuid) -> String {
        id.simple().to_string()[20..].to_ascii_uppercase()
    }

    // ==================== 夹具 ====================

    struct Fixture {
        suffix: String,
        period: String,
        in_date: String,
        prev_period: String,
        dept: Uuid,
        dept_name: String,
        reporter: Uuid,
        /// 本夹具造出来的凭证 / 折旧批次 / 资产卡（清场按 id 删）。
        vouchers: std::sync::Mutex<Vec<Uuid>>,
        batches: std::sync::Mutex<Vec<Uuid>>,
        assets: std::sync::Mutex<Vec<Uuid>>,
    }

    impl Fixture {
        async fn build() -> Self {
            let slot = next_slot();
            let year = 2100 + (slot % 60) as i32;
            let month = 1 + ((slot / 60) % 12) as u32;
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
            let reporter = crate::shared::test_helpers::ensure_any_user().await;

            Self {
                suffix: format!("{}{slot}", period.replace('-', "")),
                period,
                in_date: format!("{year:04}-{month:02}-10"),
                prev_period: format!("{prev_year:04}-{prev_month:02}"),
                dept,
                dept_name,
                reporter,
                vouchers: std::sync::Mutex::new(Vec::new()),
                batches: std::sync::Mutex::new(Vec::new()),
                assets: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 取 `biz_account` 的 (id, name)。科目必须在账套里（4101.0x / 1002 已由 V0183 种好）。
        async fn account(&self, code: &str) -> (Uuid, String) {
            db().query_sql(
                "SELECT id, account_name FROM biz_account WHERE account_code = ? AND deleted_at IS NULL".to_string(),
                vec![Value::Text(code.to_string())],
            )
            .await
            .unwrap_or_else(|error| panic!("查科目 {code} 失败：{error}"))
            .first()
            .map(|row| {
                (
                    row.get_typed("id").expect("科目 id"),
                    row.get_typed("account_name").expect("科目名"),
                )
            })
            .unwrap_or_else(|| panic!("账套里没有科目 {code}（V0183 应当已种）"))
        }

        /// 造一张凭证：`lines` = `(科目, 借方, 贷方, 部门)`，借贷合计必须相等（库层 CHECK）。
        #[allow(clippy::too_many_arguments)]
        async fn voucher(
            &self,
            period: &str,
            date: &str,
            status: &str,
            is_red: i16,
            reverse_of: Option<Uuid>,
            lines: &[(&str, &str, &str, Option<Uuid>)],
        ) -> Uuid {
            let id = Uuid::now_v7();
            let voucher_no = format!("B29-{}-{}", tail(&id), self.suffix);
            let mut total = Decimal::ZERO;
            for (_, debit, _, _) in lines {
                total += dec(debit);
            }
            db().exec_sql(
                "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
                                          source_type, summary, total_debit, total_credit, entry_count, \
                                          status, is_red, reverse_of, reversed_by, created_at, updated_at) \
                 VALUES (?::uuid, '记', ?, ?, ?::text::date, 'MANUAL', 'MANUAL', ?, \
                         ?::text::numeric, ?::text::numeric, ?, ?, ?, NULLIF(?::text,'')::uuid, NULL, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(voucher_no.clone()),
                    Value::Text(period.to_string()),
                    Value::Text(date.to_string()),
                    Value::Text(format!("B29 对账用例凭证 {voucher_no}")),
                    Value::Text(total.to_string()),
                    Value::Text(total.to_string()),
                    Value::I32(i32::try_from(lines.len()).unwrap_or(2)),
                    Value::Text(status.to_string()),
                    Value::I16(is_red),
                    Value::Text(reverse_of.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("造凭证 {voucher_no} 失败：{error}"));

            for (index, (code, debit, credit, dept)) in lines.iter().enumerate() {
                let (account_id, account_name) = self.account(code).await;
                db().exec_sql(
                    "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
                                                    account_name, debit_amount, credit_amount, aux_dept_id) \
                     VALUES (?::uuid, ?, ?, ?::uuid, ?, ?, ?::text::numeric, ?::text::numeric, \
                             NULLIF(?::text,'')::uuid)"
                        .to_string(),
                    vec![
                        Value::from(&id),
                        Value::I32(i32::try_from(index + 1).unwrap_or(1)),
                        Value::Text(format!("B29 第 {} 行", index + 1)),
                        Value::from(&account_id),
                        Value::Text(code.to_string()),
                        Value::Text(account_name),
                        Value::Text(debit.to_string()),
                        Value::Text(credit.to_string()),
                        Value::Text(dept.map(|id| id.to_string()).unwrap_or_default()),
                    ],
                )
                .await
                .unwrap_or_else(|error| panic!("造凭证分录失败：{error}"));
            }

            self.vouchers.lock().expect("凭证登记锁").push(id);
            id
        }

        /// 一笔「来源单据过账时」的正常凭证：借 `4101.x` / 贷 1002（部门挂在借方）。
        async fn expense_or_dep_voucher(&self, account: &str, amount: &str, dept: Option<Uuid>) -> Uuid {
            let date = self.in_date.clone();
            let period = self.period.clone();
            self.voucher(
                &period,
                &date,
                "POSTED",
                0,
                None,
                &[(account, amount, "0", dept), ("1002", "0", amount, None)],
            )
            .await
        }

        /// 造一张报销单（含明细），返回明细 id 列表（每张明细都会进池）。
        async fn expense(&self, items: &[(&str, &str)]) -> Vec<Uuid> {
            let period = self.period.clone();
            let report_id = Uuid::now_v7();
            let report_no = format!("B29EXP-{}-{}", tail(&report_id), self.suffix);
            let mut total = Decimal::ZERO;
            for (_, amount) in items {
                total += dec(amount);
            }
            db().exec_sql(
                "INSERT INTO biz_expense_report (id, report_no, reporter_id, reporter_name, dept_id, dept_name, \
                                                  title, expense_date, period_code, total_amount, total_tax, \
                                                  total_with_tax, item_count, status, paid_amount, unpaid_amount, \
                                                  pay_status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, 'B29 用例报销人', ?::uuid, ?, 'B29 真库用例', ?::text::date, ?, \
                         ?::text::numeric, 0, ?::text::numeric, ?, 'APPROVED', 0, ?::text::numeric, \
                         'UNPAID', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&report_id),
                    Value::Text(report_no),
                    Value::from(&self.reporter),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(self.in_date.clone()),
                    Value::Text(period),
                    Value::Text(total.to_string()),
                    Value::Text(total.to_string()),
                    Value::I32(i32::try_from(items.len()).unwrap_or(1)),
                    Value::Text(total.to_string()),
                ],
            )
            .await
            .expect("造报销单夹具");

            let mut ids = Vec::new();
            for (index, (account, amount)) in items.iter().enumerate() {
                let id = Uuid::now_v7();
                db().exec_sql(
                    "INSERT INTO biz_expense_item (id, report_id, line_no, expense_date, expense_type_id, \
                                                   type_code, type_name, account_id, account_code, account_name, \
                                                   amount, tax_rate, tax_amount, with_tax_amount, summary, \
                                                   created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?::text::date, gen_random_uuid(), 'B29TYPE', 'B29 用例费用类型', \
                             gen_random_uuid(), ?, 'B29 科目', ?::text::numeric, 0, 0, ?::text::numeric, \
                             'B29 摘要', now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&id),
                        Value::from(&report_id),
                        Value::I32(i32::try_from(index + 1).unwrap_or(1)),
                        Value::Text(self.in_date.clone()),
                        Value::Text(account.to_string()),
                        Value::Text(amount.to_string()),
                        Value::Text(amount.to_string()),
                    ],
                )
                .await
                .expect("造报销明细夹具");
                ids.push(id);
            }
            ids
        }

        /// 取（或建）某期间的折旧批次：`uk_biz_dep_batch_period` 一个期间只有一批。
        async fn batch(&self, period: &str) -> Uuid {
            let existing: Option<Uuid> = db()
                .query_sql(
                    "SELECT id FROM biz_depreciation_batch WHERE period_code = ?".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await
                .expect("查折旧批次")
                .first()
                .map(|row| row.get_typed("id").expect("批次 id"));
            if let Some(id) = existing {
                return id;
            }
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_depreciation_batch (id, batch_no, period_code, status, asset_count, \
                                             total_amount, voucher_id, run_at, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'POSTED', 0, 0, gen_random_uuid(), now(), now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(format!("B29DEP-{}-{}", tail(&id), period.replace('-', ""))),
                    Value::Text(period.to_string()),
                ],
            )
            .await
            .expect("造折旧批次夹具");
            self.batches.lock().expect("批次登记锁").push(id);
            id
        }

        /// 造一台在役资产卡 + 一条折旧明细（`POSTED`），返回折旧明细 id。
        async fn depreciation(&self, amount: &str, account: &str) -> Uuid {
            let batch = self.batch(&self.period).await;
            let asset = Uuid::now_v7();
            let asset_no = format!("B29FA-{}-{}", tail(&asset), self.suffix);
            db().exec_sql(
                "INSERT INTO biz_fixed_asset (id, asset_no, asset_name, category_id, category_code, \
                                              use_dept_id, use_dept_name, original_value, residual_value, \
                                              depreciable_base, depreciation_method, useful_life_months, \
                                              monthly_depreciation, start_depreciation_date, \
                                              accumulated_depreciation, net_value, \
                                              depreciation_months_done, purchase_date, source_type, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, 'B29 用例设备', gen_random_uuid(), 'B29CAT', ?::uuid, ?, \
                         10000, 500, 9500, 'STRAIGHT_LINE', 60, 0, ?::text::date, 0, 10000, 0, \
                         ?::text::date, 'MANUAL', 'IN_USE', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&asset),
                    Value::Text(asset_no.clone()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(self.in_date.clone()),
                    Value::Text(self.in_date.clone()),
                ],
            )
            .await
            .expect("造固定资产卡片夹具");
            self.assets.lock().expect("卡片登记锁").push(asset);

            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_depreciation (id, batch_id, asset_id, asset_no, asset_name, period_code, \
                                               dept_id, dept_name, expense_account_id, expense_account_code, \
                                               amount, accumulated_before, accumulated_after, net_value_after, \
                                               is_last_month, status, created_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?, 'B29 用例设备', ?, ?::uuid, ?, gen_random_uuid(), ?, \
                         ?::text::numeric, 0, ?::text::numeric, 10000, 0, 'POSTED', now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::from(&batch),
                    Value::from(&asset),
                    Value::Text(asset_no),
                    Value::Text(self.period.clone()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(account.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造折旧明细夹具");
            id
        }

        /// 造一条手工台账（池行：不挂成本对象）。
        async fn overhead(&self, amount: &str, account: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULL, ?::uuid, ?, 'REPAIR', ?, '1002', ?::text::numeric, 'MANUAL', ?, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period.clone()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(account.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(format!("B29-{}", self.suffix)),
                ],
            )
            .await
            .expect("造手工台账夹具");
            id
        }

        /// 清掉本期间里**本夹具前缀**的残留（跨次运行的兜底；期间槽位会复用）。
        async fn reset_period(period: &str) {
            let stmts = [
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'DEPRECIATION' \
                    AND source_id IN (SELECT id FROM biz_depreciation WHERE asset_no LIKE 'B29FA-%')"
                    .to_string(),
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'EXPENSE' \
                    AND source_id IN (SELECT ei.id FROM biz_expense_item ei \
                                        JOIN biz_expense_report r ON r.id = ei.report_id \
                                       WHERE r.report_no LIKE 'B29EXP-%')"
                    .to_string(),
                "DELETE FROM biz_cost_collect \
                  WHERE source_type = 'OVERHEAD_TICKET' \
                    AND source_id IN (SELECT id FROM biz_cost_overhead WHERE remark LIKE 'B29-%')"
                    .to_string(),
                "DELETE FROM biz_depreciation WHERE asset_no LIKE 'B29FA-%' AND period_code = ?".to_string(),
                "DELETE FROM biz_depreciation_batch WHERE period_code = ?".to_string(),
                "DELETE FROM biz_expense_report WHERE report_no LIKE 'B29EXP-%'".to_string(),
                "DELETE FROM biz_cost_overhead WHERE remark LIKE 'B29-%'".to_string(),
                // 只清「没有折旧明细引用」的孤儿卡片：并行测试不能把别人正在用的卡删掉
                "DELETE FROM biz_fixed_asset \
                  WHERE asset_no LIKE 'B29FA-%' \
                    AND NOT EXISTS (SELECT 1 FROM biz_depreciation dp WHERE dp.asset_id = biz_fixed_asset.id)"
                    .to_string(),
                // 凭证：本夹具前缀（分录随 CASCADE 删）
                "DELETE FROM biz_voucher WHERE voucher_no LIKE 'B29-%'".to_string(),
            ];
            for sql in stmts {
                let params = if sql.contains('?') {
                    vec![Value::Text(period.to_string())]
                } else {
                    vec![]
                };
                let _ = db().exec_sql(sql, params).await;
            }
        }

        /// 跑第 3 段（真跑，不手插 collect），返回池合计。
        async fn collect(&self) -> Decimal {
            let outcome = collect_segment::collect_overhead(&self.period, None)
                .await
                .unwrap_or_else(|error| panic!("费归集失败：{}", error.msg));
            outcome.pool_amount()
        }

        /// 事务内读对账结论（不提交、不回滚，调用方决定）。
        async fn reconcile_in_tx(&self, stage: Stage) -> crate::business::erp::cost_reconcile_dto::ReconcileOutcome {
            let tx = db().begin().await.expect("开启对账事务");
            let outcome = segment::reconcile_in_tx(&tx, &self.period, stage)
                .await
                .unwrap_or_else(|error| panic!("对账失败：{}", error.msg));
            tx.rollback().await.expect("回滚对账事务");
            outcome
        }
    }

    // ==================== 用例 ====================

    /// ① 验收项「差额必须为 0」：折旧 + 报销池与账面 4101.* 借方逐组相等；
    /// 手工台账在 Calc 阶段计「待结算过账」（§4.8 ④：它的凭证在结算时才生成）。
    #[tokio::test]
    async fn every_booked_route_is_balanced_and_manual_ticket_is_pending() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.expense(&[("4101.01", "1000")]).await;
        fixture.depreciation("300", "4101.02").await;
        fixture.overhead("150", "4101.03").await;
        let pool = fixture.collect().await;
        assert_eq!(pool, dec("1450"), "池 = 1000 + 300 + 150");

        // 折旧 / 报销在来源过账时就已生成凭证（都在借方挂车间）
        fixture
            .expense_or_dep_voucher("4101.01", "1000", Some(fixture.dept))
            .await;
        fixture
            .expense_or_dep_voucher("4101.02", "300", Some(fixture.dept))
            .await;

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(outcome.balanced(), "逐组必须平（差额 0）：{}", outcome.summary_line());
        assert_eq!(outcome.pool_amount, dec("1450"));
        assert_eq!(outcome.expected_amount, dec("1300"));
        assert_eq!(outcome.pending_amount, dec("150"));
        assert_eq!(outcome.ledger_amount, dec("1300"));
        assert_eq!(outcome.diff_amount, Decimal::ZERO);
        assert_eq!(outcome.groups.len(), 3, "三个 (车间, 科目) 组");
        let manual = outcome
            .groups
            .iter()
            .find(|group| group.account_code == "4101.03")
            .expect("手工台账组");
        assert_eq!(manual.kind, GroupKind::Pending);
        assert_eq!(manual.pending_amount, dec("150"));

        // 公开入口同样平（不报错）
        let outcome = segment::reconcile_pool_with_ledger(&fixture.period, Stage::Calc)
            .await
            .unwrap_or_else(|error| panic!("对账入口不该报错：{}", error.msg));
        assert!(outcome.balanced());

        Fixture::reset_period(&fixture.period).await;
    }

    /// ② 验收项「不为 0 → 报错并列出差额凭证号」：有人手工凭证记 4101（旁路记账）。
    #[tokio::test]
    async fn bypass_voucher_is_flagged_with_its_voucher_no() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.expense(&[("4101.01", "1000")]).await;
        fixture.collect().await;
        fixture
            .expense_or_dep_voucher("4101.01", "1000", Some(fixture.dept))
            .await;
        // 旁路：财务手工凭证借 4101.04 / 贷 1002，成本模块完全不知道
        let bypass = fixture
            .expense_or_dep_voucher("4101.04", "200", Some(fixture.dept))
            .await;
        let bypass_no: String = db()
            .scalar_with_params(
                "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid",
                vec![Value::from(&bypass)],
            )
            .await
            .expect("取旁路凭证号");

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(!outcome.balanced(), "旁路记账必须报不平");
        assert_eq!(outcome.diff_amount, dec("-200"), "账面多 200");
        assert_eq!(outcome.ledger_amount, dec("1200"));
        let group = outcome
            .groups
            .iter()
            .find(|group| group.account_code == "4101.04")
            .expect("4101.04 要单独成组");
        assert_eq!(group.kind, GroupKind::LedgerOnly);
        assert_eq!(group.voucher_nos, vec![bypass_no.clone()]);
        assert!(outcome.difference_vouchers().contains(&bypass_no));

        let error = segment::reconcile_pool_with_ledger(&fixture.period, Stage::Calc)
            .await
            .expect_err("不平必须 409");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66065);
        println!("【66065 报文】\n{}", error.msg);
        assert!(error.msg.contains(&bypass_no), "报文必须点名凭证号：{}", error.msg);
        assert!(error.msg.contains("4101.04"), "报文必须点名科目：{}", error.msg);
        assert!(error.msg.contains("费用台账"), "报文必须给修法：{}", error.msg);

        Fixture::reset_period(&fixture.period).await;
    }

    /// ③ 4101 借方没挂车间 ⇒ 单独成组，报文写「（无车间）」。
    #[tokio::test]
    async fn voucher_without_department_is_flagged() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.expense_or_dep_voucher("4101.09", "80", None).await;

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(!outcome.balanced());
        let group = outcome
            .groups
            .iter()
            .find(|group| group.account_code == "4101.09")
            .expect("无部门的 4101.09 要成组");
        assert_eq!(group.dept_id, None);
        assert!(group.label().contains("无车间"), "标签：{}", group.label());
        assert_eq!(group.kind, GroupKind::LedgerOnly);
        let error = segment::reconcile_pool_with_ledger(&fixture.period, Stage::Calc)
            .await
            .expect_err("必须报错");
        assert!(error.msg.contains("无车间"), "{}", error.msg);

        Fixture::reset_period(&fixture.period).await;
    }

    /// ④ 已红冲凭证 / 红字凭证 / DRAFT / 非 4101.* 全不进账面。
    #[tokio::test]
    async fn reversed_red_draft_and_non_4101_vouchers_are_ignored() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let date = fixture.in_date.clone();
        let period = fixture.period.clone();

        // 已红冲：蓝字借 4101.04 / 红字贷 4101.04（红字凭证 is_red=1 且 reverse_of 指向原凭证）
        let blue = fixture
            .voucher(
                &period,
                &date,
                "POSTED",
                0,
                None,
                &[("4101.04", "200", "0", Some(fixture.dept)), ("1002", "0", "200", None)],
            )
            .await;
        let red = fixture
            .voucher(
                &period,
                &date,
                "POSTED",
                1,
                Some(blue),
                &[("4101.04", "0", "200", Some(fixture.dept)), ("1002", "200", "0", None)],
            )
            .await;
        db().exec_sql(
            "UPDATE biz_voucher SET reversed_by = ?::uuid WHERE id = ?::uuid".to_string(),
            vec![Value::from(&red), Value::from(&blue)],
        )
        .await
        .expect("回写 reversed_by");

        // DRAFT 凭证（没过账）与 5602.* 科目（不是制造费用）
        fixture
            .voucher(
                &period,
                &date,
                "DRAFT",
                0,
                None,
                &[("4101.09", "90", "0", Some(fixture.dept)), ("1002", "0", "90", None)],
            )
            .await;
        fixture
            .voucher(
                &period,
                &date,
                "POSTED",
                0,
                None,
                &[("5602.01", "70", "0", Some(fixture.dept)), ("1002", "0", "70", None)],
            )
            .await;

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(
            outcome.balanced(),
            "红冲 / 红字 / DRAFT / 5602.* 都不该进账面：{}",
            outcome.summary_line()
        );
        assert_eq!(outcome.ledger_amount, Decimal::ZERO);
        assert_eq!(outcome.groups.len(), 0);

        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑤ 上期凭证不进本期账面。
    #[tokio::test]
    async fn previous_period_voucher_does_not_leak() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let prev = fixture.prev_period.clone();
        let date = fixture.in_date.clone();
        fixture
            .voucher(
                &prev,
                &date,
                "POSTED",
                0,
                None,
                &[("4101.01", "500", "0", Some(fixture.dept)), ("1002", "0", "500", None)],
            )
            .await;

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(outcome.balanced(), "{}", outcome.summary_line());
        assert_eq!(outcome.ledger_amount, Decimal::ZERO);

        Fixture::reset_period(&fixture.prev_period).await;
        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑥ 账面只记一部分 ⇒ 金额不符（差额 = 应已入账 - 账面）。
    #[tokio::test]
    async fn partial_booking_is_a_mismatch() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.expense(&[("4101.01", "1000")]).await;
        fixture.collect().await;
        fixture
            .expense_or_dep_voucher("4101.01", "800", Some(fixture.dept))
            .await;

        let outcome = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(!outcome.balanced());
        assert_eq!(outcome.diff_amount, dec("200"));
        let group = &outcome.groups[0];
        assert_eq!(group.kind, GroupKind::Mismatch);
        assert_eq!(group.expected_amount, dec("1000"));
        assert_eq!(group.ledger_amount, dec("800"));
        assert_eq!(group.diff, dec("200"));

        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑦ 总额相抵但逐组不平 ⇒ 仍报错（C-COST-1 按 (车间, 科目) 分组比）。
    #[tokio::test]
    async fn offsetting_groups_do_not_cancel_out() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        // 池：本车间 4101.01 100；账面：**没挂车间**的 4101.01 100 ⇒ 总额恰好相抵
        fixture.overhead("100", "4101.01").await;
        fixture.collect().await;
        fixture.expense_or_dep_voucher("4101.01", "100", None).await;

        let outcome = fixture.reconcile_in_tx(Stage::Settle).await;
        assert_eq!(outcome.diff_amount, Decimal::ZERO, "总额相抵");
        assert_eq!(outcome.mismatched_groups, 2, "两组都不平");
        assert!(!outcome.balanced(), "总额 0 也必须报错");
        let error = segment::reconcile_pool_with_ledger(&fixture.period, Stage::Settle)
            .await
            .expect_err("必须 409");
        assert!(error.msg.contains("2 组不平"), "{}", error.msg);

        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑧ 对账池侧与费归集段的池口径互校（同口径两份 SQL 不能漂）。
    #[tokio::test]
    async fn pool_by_source_matches_the_collect_segment_pool() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.expense(&[("4101.01", "1000")]).await;
        fixture.depreciation("300", "4101.02").await;
        fixture.overhead("150", "4101.03").await;
        fixture.collect().await;

        let tx = db().begin().await.expect("开启事务");
        let by_source = cost_reconcile_model::pool_by_source(&tx, &fixture.period)
            .await
            .expect("读池侧（按来源）");
        let pool = cost_overhead_collect_model::pool_of_period(&tx, &fixture.period)
            .await
            .expect("读费归集段的池");
        tx.rollback().await.expect("回滚");

        // 本函数按来源拆，费归集段按 (车间,科目) 汇总 ⇒ 汇总后必须逐组相等
        let mut folded: std::collections::BTreeMap<(Uuid, String), Decimal> = std::collections::BTreeMap::new();
        for row in &by_source {
            *folded.entry((row.dept_id, row.account_code.clone())).or_default() += row.amount;
        }
        assert_eq!(folded.len(), pool.len(), "组数一致");
        for row in &pool {
            let got = folded
                .get(&(row.dept_id, row.account_code.clone()))
                .copied()
                .unwrap_or_default();
            assert_eq!(got, row.amount, "{} 的池金额必须一致", row.account_code);
        }
        assert_eq!(by_source.iter().map(|row| row.rows).sum::<i64>(), 3, "三笔池行");

        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑨ 空期间：0 组、平、不报错。
    #[tokio::test]
    async fn empty_period_is_balanced() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let outcome = segment::reconcile_pool_with_ledger(&fixture.period, Stage::Calc)
            .await
            .unwrap_or_else(|error| panic!("空期间不该报错：{}", error.msg));
        assert!(outcome.balanced());
        assert_eq!(outcome.groups.len(), 0);
        assert_eq!(outcome.pool_amount, Decimal::ZERO);
        assert_eq!(outcome.ledger_amount, Decimal::ZERO);

        Fixture::reset_period(&fixture.period).await;
    }

    /// ⑩ 阶段口径：同一批数据 Calc 平、Settle 不平（手工台账的第四张凭证还没过账）。
    #[tokio::test]
    async fn settle_stage_requires_the_manual_ticket_to_be_booked() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        fixture.overhead("150", "4101.03").await;
        fixture.collect().await;

        let calc = fixture.reconcile_in_tx(Stage::Calc).await;
        assert!(calc.balanced(), "{}", calc.summary_line());
        assert_eq!(calc.pending_amount, dec("150"));

        let settle = fixture.reconcile_in_tx(Stage::Settle).await;
        assert!(!settle.balanced(), "结算阶段必须要求手工台账也有账面背书");
        assert_eq!(settle.diff_amount, dec("150"));
        assert_eq!(settle.pending_amount, Decimal::ZERO);
        assert_eq!(settle.groups[0].kind, GroupKind::PoolOnly);

        // 补上结算时那张「借 4101 / 贷 对方科目」的凭证 ⇒ 平
        fixture
            .expense_or_dep_voucher("4101.03", "150", Some(fixture.dept))
            .await;
        let settle = fixture.reconcile_in_tx(Stage::Settle).await;
        assert!(settle.balanced(), "{}", settle.summary_line());

        Fixture::reset_period(&fixture.period).await;
    }
}
