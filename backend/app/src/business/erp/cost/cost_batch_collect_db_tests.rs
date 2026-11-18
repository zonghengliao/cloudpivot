//! 分批法「料工费归集到批次」（M5-03）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m503_dev \
//!   cargo test --lib cost_batch_collect -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-03「料工费归集到批次」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`preview_lists_three_sources_with_states`] | 预览三路各一行、状态 `PENDING`、已归集 0、归集后合计 = 三路之和（**不写库**：库里的行数仍是 0） |
//! | 2 | [`collect_writes_three_elements_and_is_idempotent`] | 料/工/费三路各写 1 行、金额取来源账上值；第二次调用 `inserted = 0`、行 id 与 `updated_at` 不动 |
//! | 3 | [`collect_409_when_period_batch_is_frozen`] | 本期批次 `SUBMITTED` ⇒ 409 + `66106`，一行都不写；预览同时给 `collectable = false` 与原因 |
//! | 4 | [`collect_409_when_object_is_closed`] | 批次关批 ⇒ 409；作废同理（`CANCELLED`） |
//! | 5 | [`collect_404_when_object_out_of_scope`] | 部门范围用户看不到别车间的批次 ⇒ 404（不是 403，不当探测口） |
//! | 6 | [`relinked_stock_out_moves_the_collected_row`] | 领料单改挂到本批 ⇒ 旧行软删、新行落本批（幂等键是来源明细 id，不是对象+金额） |
//! | 7 | [`purges_rows_whose_source_no_longer_qualifies`] | 来源作废（`POSTED → DRAFT`）⇒ 重跑把这些行软删，金额归零 |
//! | 8 | [`labor_without_rate_is_reported_not_written`] | 缺工资率（`rate = 0`）的工时 ⇒ 不写 0 金额行，进「不可归集」并给原因 |
//! | 9 | [`pool_rows_and_other_batches_are_untouched`] | 费用池行（无对象）与别的批次的归集行一行不动 |
//! | 10 | [`period_calc_after_batch_collect_does_not_double_count`] | 先按批归集、再跑 calc 料段 ⇒ `inserted = 0`（同一套幂等键，不重复算） |
//! | 11 | [`zz_teardown_leaves_zero_residue`] | 清场后按 id 回查：归集行 / 工时 / 费用 / 出库单 / 对象 / 期间批次全部 0 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个**期间槽**（年份 = `2000 + slot % 80`、月份 = `1 + slot/80 % 12`），
//!   并在建立前清掉该槽上一次运行的残留 —— 否则 `material_source_lines` 会把上次的
//!   领料单一起扫进来，断言当场漂；
//! * 批次号 / 单号 / 物料编码全部带唯一后缀，绝不按前缀扫全库；
//! * 出库单 / 工时 / 费用 / 期间批次**直接 SQL 造**（不经 handler）：本卡验的是「扫描口径 +
//!   归集落库」，且要造出 `POSTED + 挂对象`、`rate = 0` 这类 API 造不出来或要绕远路的形态。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_batch_collect::{collect, preview};
    use crate::business::erp::cost::cost_batch_collect_dto::{
        self as dto, BatchCollectReq, BatchCollectRes, BatchSourceQuery, BatchSourcesRes,
    };
    use crate::business::erp::cost::cost_collect_sheet_dto::SheetBatchRes;
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

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 进程内递增槽位（决定夹具独占的会计期间；理由见模块头）。
    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// 一条出库单要造的形态。
    struct Issue<'a> {
        object: Uuid,
        date: &'a str,
        qty: &'a str,
        amount: &'a str,
        status: &'a str,
    }

    struct Fixture {
        suffix: String,
        period: String,
        in_date: String,
        dept: Uuid,
        dept_name: String,
        other_dept: Uuid,
        other_dept_code: String,
        product: Uuid,
        product_code: String,
        warehouse: Uuid,
        warehouse_code: String,
        unit: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let slot = next_slot();
            let year = 2000 + (slot % 80) as i32;
            let month = 1 + ((slot / 80) % 12) as u32;
            let period = format!("{year:04}-{month:02}");
            Self::reset_period_for(&period).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept_code = format!("M503-DEPT2-{suffix}");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &other_dept_code,
                &format!("M5-03 夹具第二车间 {suffix}"),
            )
            .await;
            let unit = crate::test_seed::ensure_unit().await;
            let product_code = format!("M503-MAT-{suffix}");
            let product = crate::test_seed::ensure_material_kind(
                &product_code,
                &format!("M5-03 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse_code = format!("M53W{}", crate::test_seed::last_chars(&suffix, 8));
            let warehouse =
                crate::test_seed::ensure_warehouse(&warehouse_code, &format!("M5-03 夹具仓 {suffix}")).await;

            Self {
                suffix,
                period,
                in_date: format!("{year:04}-{month:02}-10"),
                dept,
                dept_name,
                other_dept,
                other_dept_code,
                product,
                product_code,
                warehouse,
                warehouse_code,
                unit,
            }
        }

        /// 超管范围（用例验接口本身；数据范围由第 5 条专测）。
        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        /// 部门范围：只能看 `other_dept`（本用例的车间不可见）。
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

        /// 造成本对象（`CLOSED` 必须同时给 `closed_at`）。
        async fn object(&self, status: &str, tag: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, target_warehouse_id, \
                                                  plan_qty, workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?::uuid, 100, ?::uuid, ?, '{}', ?, {closed_at}, now(), now())",
                    self.in_date
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!(
                        "CB-M503-{tag}-{}",
                        crate::test_seed::last_chars(&self.suffix, 8)
                    )),
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

        /// 造一张已过账领料单（一行明细），返回明细 id。
        async fn issue(&self, issue: Issue<'_>) -> Uuid {
            let header = Uuid::now_v7();
            let out_no = format!(
                "M503OUT-{}",
                crate::test_seed::last_chars(&header.simple().to_string(), 10)
            );
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, reason, cost_object_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, ?, 0, 0, 'M5-03 夹具', ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&header),
                    Value::Text(out_no),
                    Value::from(&self.warehouse),
                    Value::Text(issue.date.to_string()),
                    Value::Text(issue.status.to_string()),
                    Value::from(&issue.object),
                ],
            )
            .await
            .expect("造领料单头夹具");
            let item = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, \
                                                  unit_cost, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, 'M5-03 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&item),
                    Value::from(&header),
                    Value::from(&self.product),
                    Value::from(&self.unit),
                    Value::Text(issue.qty.to_string()),
                    Value::Text(issue.amount.to_string()),
                    Value::Text(issue.amount.to_string()),
                ],
            )
            .await
            .expect("造领料明细夹具");
            item
        }

        /// 改挂领料单到别的批次（历史脏数据形态：改挂后旧归集行必须跟着动）。
        async fn relink_issue(&self, item: Uuid, object: Uuid) {
            db().exec_sql(
                "UPDATE biz_stock_out SET cost_object_id = ?::uuid \
                  WHERE id = (SELECT stock_out_id FROM biz_stock_out_item WHERE id = ?::uuid)"
                    .to_string(),
                vec![Value::from(&object), Value::from(&item)],
            )
            .await
            .expect("改挂领料单");
        }

        /// 让领料单退回草稿（= 来源不再合规）。
        async fn unpost_issue(&self, item: Uuid) {
            db().exec_sql(
                "UPDATE biz_stock_out SET status = 'DRAFT' \
                  WHERE id = (SELECT stock_out_id FROM biz_stock_out_item WHERE id = ?::uuid)"
                    .to_string(),
                vec![Value::from(&item)],
            )
            .await
            .expect("领料单退回草稿");
        }

        /// 造一条工时台账行（`amount = round(hours × rate, 4)`，库层 CHECK 要求）。
        async fn labor(&self, object: Uuid, hours: &str, rate: &str, status: &str, day: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, ?, ?::uuid, ?, \
                         ?::text::numeric, 0, ?::text::numeric, \
                         round(?::text::numeric * ?::text::numeric, 4), \
                         'MANUAL', ?, 'M5-03 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period.clone()),
                    Value::Text(day.to_string()),
                    Value::from(&object),
                    Value::Text(format!("M5-03 人员 {}", crate::test_seed::last_chars(&self.suffix, 6))),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(hours.to_string()),
                    Value::Text(rate.to_string()),
                    Value::Text(hours.to_string()),
                    Value::Text(rate.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造工时台账夹具");
            id
        }

        /// 造一条手工费用台账行（`object = None` ⇒ 进车间费用池）。
        async fn overhead(&self, object: Option<Uuid>, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            let object_sql = if object.is_some() { "?::uuid" } else { "NULL" };
            let mut params = vec![Value::from(&id), Value::Text(self.period.clone())];
            if let Some(object) = object {
                params.push(Value::from(&object));
            }
            params.extend([
                Value::from(&self.dept),
                Value::Text(self.dept_name.clone()),
                Value::Text("4101.01".to_string()),
                Value::Text(amount.to_string()),
            ]);
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                    expense_item, debit_account_code, credit_account_code, \
                                                    amount, source_type, remark, created_at, updated_at) \
                     VALUES (?::uuid, ?, {object_sql}, ?::uuid, ?, 'REPAIR', ?, '1002', ?::text::numeric, \
                             'MANUAL', 'M5-03 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造手工费用夹具");
            id
        }

        /// 造一张成本核算批次（期间唯一：每个夹具独占期间，故至多一张活跃）。
        async fn period_batch(&self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            // `SUBMITTED` 之后必须挂审批实例；`SETTLED` 之后还要 `settled_at + finish_voucher_id`
            let approval = if status == "DRAFT" {
                "NULL".to_string()
            } else {
                "'00000000-0000-0000-0000-000000000001'::uuid".to_string()
            };
            let settled = if matches!(status, "SETTLED" | "CLOSED") {
                "now(), '00000000-0000-0000-0000-000000000002'::uuid"
            } else {
                "NULL, NULL"
            };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period (id, period_code, batch_no, status, approval_instance_id, \
                                                   settled_at, finish_voucher_id, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?, {approval}, {settled}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.period.clone()),
                    Value::Text(format!("CS-M503-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        fn source_query(&self) -> BatchSourceQuery {
            BatchSourceQuery {
                period_code: Some(self.period.clone()),
            }
        }

        fn collect_req(&self) -> BatchCollectReq {
            BatchCollectReq {
                period_code: Some(self.period.clone()),
            }
        }

        /// 某来源行在 `biz_cost_collect` 里的行数（含已软删）。
        async fn collect_rows_of(&self, source_id: Uuid) -> i64 {
            db().scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE source_id = ?::uuid",
                vec![Value::from(&source_id)],
            )
            .await
            .expect("查归集行")
        }

        /// 某来源行**还活着**的归集行数（`deleted_at IS NULL`）。
        async fn collect_live_rows(&self, source_id: Uuid) -> i64 {
            db().scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE source_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&source_id)],
            )
            .await
            .expect("查未删的归集行")
        }

        /// 清掉本槽上一次运行的残留（只删本卡能造的形态，绝不按期间盲删别的模块数据）。
        async fn reset_period_for(period: &str) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect WHERE period_code = ? \
                       AND (source_no LIKE 'M503OUT-%' OR source_type = 'LABOR_TICKET' \
                            OR (element = 'OVERHEAD' AND source_type = 'OVERHEAD_TICKET' \
                                AND remark LIKE '%M5-03%'))"
                        .to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_labor WHERE period_code = ? AND remark = 'M5-03 夹具'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_overhead WHERE period_code = ? AND remark = 'M5-03 夹具'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_stock_out WHERE out_no LIKE 'M503OUT-%' \
                        AND out_date >= (? || '-01')::date \
                        AND out_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE period_code = ? AND batch_no LIKE 'CS-M503-%'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M503-%' \
                        AND start_date >= (? || '-01')::date \
                        AND start_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
        }

        /// 清场：按 id 删（FK 顺序：归集 → 工时/费用 → 出库明细/头 → 期间批次 → 对象 → 物料/仓库/部门）。
        async fn cleanup(&self, objects: &[Uuid], period_batches: &[Uuid]) {
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
                let _ = db()
                    .exec_sql(
                        format!(
                            "DELETE FROM biz_cost_collect WHERE source_id IN \
                             (SELECT id FROM biz_stock_out_item WHERE stock_out_id IN \
                               (SELECT id FROM biz_stock_out WHERE cost_object_id IN ({objects_list})))"
                        ),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_overhead WHERE cost_object_id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!(
                            "DELETE FROM biz_stock_out_item WHERE stock_out_id IN \
                                 (SELECT id FROM biz_stock_out WHERE cost_object_id IN ({objects_list}))"
                        ),
                        vec![],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_stock_out WHERE cost_object_id IN ({objects_list})"),
                        vec![],
                    )
                    .await;
            }
            if !period_batches.is_empty() {
                let list = list(period_batches);
                let _ = db()
                    .exec_sql(format!("DELETE FROM biz_cost_period WHERE id IN ({list})"), vec![])
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
            // 池行（无对象）按本用例车间 + 期间 + 金额删
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect \
                      WHERE cost_object_id IS NULL AND period_code = ? AND dept_id = ?::uuid \
                        AND source_type = 'DEPRECIATION'"
                        .to_string(),
                    vec![Value::Text(self.period.clone()), Value::from(&self.dept)],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_overhead WHERE period_code = ? AND cost_object_id IS NULL \
                        AND dept_id = ?::uuid AND remark = 'M5-03 夹具'"
                        .to_string(),
                    vec![Value::Text(self.period.clone()), Value::from(&self.dept)],
                )
                .await;
            for (sql, param) in [
                ("DELETE FROM biz_material WHERE material_code = ?", &self.product_code),
                (
                    "DELETE FROM biz_warehouse WHERE warehouse_code = ?",
                    &self.warehouse_code,
                ),
            ] {
                let _ = db().exec_sql(sql.to_string(), vec![Value::Text(param.clone())]).await;
            }
            for code in [&self.other_dept_code] {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM sys_dept WHERE dept_code = ?".to_string(),
                        vec![Value::Text(code.clone())],
                    )
                    .await;
            }
        }
    }

    /// `deleted_at` 的读取垫片已删除：改用 `count(*) FILTER (deleted_at IS NULL)` 计数
    /// （`Row::get_typed` 不保证能取 TIMESTAMPTZ，也没必要为一条断言引入时间类型）。
    // ==================== 1. 预览 ====================

    #[tokio::test]
    async fn preview_lists_three_sources_with_states() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "PRE").await;
        let item = fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "10",
                amount: "150",
                status: "POSTED",
            })
            .await;
        fixture.labor(object, "8", "30", "CONFIRMED", &fixture.in_date).await;
        fixture.overhead(Some(object), "20").await;

        let res = preview(&fixture.super_principal(), &object.to_string(), fixture.source_query())
            .await
            .expect("预览三路来源");

        assert_eq!(res.period_code, fixture.period);
        assert_eq!(res.batch.object_id, object.to_string());
        assert!(res.collectable, "期间无批次 + 批次 OPEN ⇒ 可归集");
        assert!(res.block_reason.is_none());
        assert!(res.period_batch.is_none(), "没跑过 calc 就没有期间批次");
        assert!(!res.recalc_required);

        assert_eq!(res.material.rows.len(), 1);
        assert_eq!(res.material.rows[0].source_id, item.to_string());
        assert_eq!(res.material.rows[0].state, dto::STATE_PENDING);
        assert_eq!(res.material.rows[0].state_label, "待归集");
        assert_eq!(dec(&res.material.rows[0].amount), dec("150"));

        assert_eq!(res.labor.rows.len(), 1);
        assert_eq!(dec(&res.labor.rows[0].amount), dec("240"), "8h × 30 = 240");
        assert_eq!(res.overhead.rows.len(), 1);
        assert_eq!(dec(&res.overhead.rows[0].amount), dec("20"));

        // 预览不写库：三路「已归集」都是 0，归集后合计 = 三路来源之和
        assert_eq!(res.material.collected_rows, 0);
        assert_eq!(res.labor.collected_rows, 0);
        assert_eq!(res.overhead.collected_rows, 0);
        assert_eq!(dec(&res.totals.total), dec("0"));
        assert_eq!(dec(&res.after_totals.mat), dec("150"));
        assert_eq!(dec(&res.after_totals.labor), dec("240"));
        assert_eq!(dec(&res.after_totals.oh), dec("20"));
        assert_eq!(dec(&res.after_totals.total), dec("410"));
        let written: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("回查归集行数");
        assert_eq!(written, 0, "预览一行都不许写");

        fixture.cleanup(&[object], &[]).await;
    }

    // ==================== 2. 三路归集 + 幂等 ====================

    #[tokio::test]
    async fn collect_writes_three_elements_and_is_idempotent() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "COL").await;
        let item = fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "10",
                amount: "150",
                status: "POSTED",
            })
            .await;
        let labor = fixture.labor(object, "8", "30", "CONFIRMED", &fixture.in_date).await;
        let overhead = fixture.overhead(Some(object), "20").await;

        let first = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("第一次归集");

        assert_eq!(first.material.inserted, 1);
        assert_eq!(first.labor.inserted, 1);
        assert_eq!(first.overhead.inserted, 1);
        assert_eq!(first.material.skipped_existing, 0);
        assert_eq!(dec(&first.material.collected_amount), dec("150"));
        assert_eq!(dec(&first.labor.collected_amount), dec("240"));
        assert_eq!(dec(&first.overhead.collected_amount), dec("20"));
        assert_eq!(dec(&first.totals.total), dec("410"));
        assert_eq!(first.material.collected_rows, 1);
        assert!(first.message.contains("已归集 3 笔"), "{}", first.message);
        assert!(!first.recalc_required, "没有期间批次 ⇒ 不需要重算");

        // 幂等：第二次一行不写、行 id / updated_at 不动
        let ids_before = collect_ids(object).await;
        let updated_before = collect_updated_at(object).await;

        let second = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("第二次归集（幂等）");
        assert_eq!(second.material.inserted, 0);
        assert_eq!(second.labor.inserted, 0);
        assert_eq!(second.overhead.inserted, 0);
        assert_eq!(second.material.skipped_existing, 1);
        assert_eq!(second.labor.skipped_existing, 1);
        assert_eq!(second.overhead.skipped_existing, 1);
        assert_eq!(dec(&second.totals.total), dec("410"));
        assert!(second.message.contains("没有新的可归集行"), "{}", second.message);

        assert_eq!(collect_ids(object).await, ids_before, "重复跑不许重建行");
        assert_eq!(
            collect_updated_at(object).await,
            updated_before,
            "重复跑不许刷 updated_at"
        );

        // 三路来源都点得回原单据（行存在且未软删）
        assert_eq!(fixture.collect_live_rows(item).await, 1);
        assert_eq!(fixture.collect_live_rows(labor).await, 1);
        assert_eq!(fixture.collect_live_rows(overhead).await, 1);

        fixture.cleanup(&[object], &[]).await;
    }

    /// 本批的归集行 id（排序后拼串，用于「重复跑不重建」断言）。
    async fn collect_ids(object: Uuid) -> String {
        let rows = db()
            .query_sql(
                "SELECT id FROM biz_cost_collect WHERE cost_object_id = ?::uuid ORDER BY element, source_type"
                    .to_string(),
                vec![Value::from(&object)],
            )
            .await
            .expect("查归集行 id");
        rows.iter()
            .map(|row| row.get_typed::<Uuid>("id").expect("id").to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    async fn collect_updated_at(object: Uuid) -> String {
        let rows = db()
            .query_sql(
                "SELECT to_char(updated_at, 'YYYY-MM-DD HH24:MI:SS.US') AS updated_at \
                   FROM biz_cost_collect WHERE cost_object_id = ?::uuid ORDER BY element, source_type"
                    .to_string(),
                vec![Value::from(&object)],
            )
            .await
            .expect("查归集行 updated_at");
        rows.iter()
            .map(|row| row.get_typed::<String>("updated_at").expect("updated_at 文本"))
            .collect::<Vec<_>>()
            .join(",")
    }

    // ==================== 3. 期间批次冻结 409 ====================

    #[tokio::test]
    async fn collect_409_when_period_batch_is_frozen() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "FRZ").await;
        fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "1",
                amount: "10",
                status: "POSTED",
            })
            .await;
        let period_batch = fixture.period_batch("SUBMITTED").await;

        let error = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect_err("期间批次审批中必须拒");
        assert_eq!(error.status, 409);
        assert_eq!(
            error.code,
            cloudpivot_kernel::shared::error_code::ERP_COST_BATCH_COLLECT_BLOCKED
        );
        assert!(error.msg.contains("SUBMITTED"), "{}", error.msg);
        assert!(error.msg.contains("撤回"), "{}", error.msg);

        // 预览同时给「不可归集 + 原因」，且不报错
        let res = preview(&fixture.super_principal(), &object.to_string(), fixture.source_query())
            .await
            .expect("预览在冻结期间仍要能看数");
        assert!(!res.collectable);
        assert!(res.block_reason.as_deref().unwrap_or("").contains("已冻结"));
        assert!(res.block_fix.as_deref().unwrap_or("").contains("撤回"));
        assert_eq!(res.period_batch.as_ref().map(|p| p.status.as_str()), Some("SUBMITTED"));
        assert!(res.period_batch.as_ref().is_some_and(|p| p.frozen));

        // 一行都没写
        assert_eq!(collect_ids(object).await, "");

        fixture.cleanup(&[object], &[period_batch]).await;
    }

    // ==================== 4. 批次关批 / 作废 409 ====================

    #[tokio::test]
    async fn collect_409_when_object_is_closed() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let closed = fixture.object("CLOSED", "CLS").await;
        let error = collect(
            &fixture.super_principal(),
            None,
            &closed.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect_err("关批批次不能再归集");
        assert_eq!(error.status, 409);
        assert_eq!(
            error.code,
            cloudpivot_kernel::shared::error_code::ERP_COST_BATCH_COLLECT_BLOCKED
        );
        assert!(error.msg.contains("已关批"), "{}", error.msg);

        let cancelled = fixture.object("CANCELLED", "CAN").await;
        let error = collect(
            &fixture.super_principal(),
            None,
            &cancelled.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect_err("作废批次不能再归集");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("已作废"), "{}", error.msg);

        fixture.cleanup(&[closed, cancelled], &[]).await;
    }

    // ==================== 5. 数据范围 404 ====================

    #[tokio::test]
    async fn collect_404_when_object_out_of_scope() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "SCO").await;

        let error = collect(
            &fixture.other_dept_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect_err("别的车间的批次不可见");
        assert_eq!(error.status, 404, "不可见报 404，不是 403（403 等于承认 id 存在）");

        let error = preview(
            &fixture.other_dept_principal(),
            &object.to_string(),
            fixture.source_query(),
        )
        .await
        .expect_err("预览同样 404");
        assert_eq!(error.status, 404);

        // 非法 id 形状也是 404
        let error = collect(
            &fixture.super_principal(),
            None,
            "CB-M503-NOT-A-UUID",
            fixture.collect_req(),
        )
        .await
        .expect_err("非法 id");
        assert_eq!(error.status, 404);

        fixture.cleanup(&[object], &[]).await;
    }

    // ==================== 6. 领料单改挂批次 ====================

    #[tokio::test]
    async fn relinked_stock_out_moves_the_collected_row() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let from = fixture.object("OPEN", "RLA").await;
        let to = fixture.object("OPEN", "RLB").await;
        let item = fixture
            .issue(Issue {
                object: from,
                date: &fixture.in_date,
                qty: "5",
                amount: "80",
                status: "POSTED",
            })
            .await;

        let first = collect(
            &fixture.super_principal(),
            None,
            &from.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("先归集到 A");
        assert_eq!(dec(&first.material.collected_amount), dec("80"));

        // 改挂到 B：旧行必须被清掉，新行落到 B（否则 ON CONFLICT 会把新行静默吞掉）
        fixture.relink_issue(item, to).await;
        let second = collect(&fixture.super_principal(), None, &to.to_string(), fixture.collect_req())
            .await
            .expect("再归集到 B");
        assert_eq!(second.material.inserted, 1);
        assert_eq!(dec(&second.material.collected_amount), dec("80"));
        // 旧行在**同一次调用里**被软删（`subset_for_object` 把「键命中的外来行」也纳入取舍），
        // 所以这个来源 id 上始终只有 1 条活着的归集行，且它属于 B。
        assert_eq!(fixture.collect_rows_of(item).await, 2, "旧行留痕 + 新行");
        assert_eq!(fixture.collect_live_rows(item).await, 1, "同一来源只许一条活行");

        // A 侧重跑：已经没有该来源的活行 ⇒ 纯无操作（不是「再清一次」）
        let from_after = collect(
            &fixture.super_principal(),
            None,
            &from.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("A 侧重跑");
        assert_eq!(from_after.material.purged, 0);
        assert_eq!(from_after.material.inserted, 0);
        assert_eq!(
            dec(&from_after.material.collected_amount),
            dec("0"),
            "钱跟着来源走，A 上不能留"
        );

        fixture.cleanup(&[from, to], &[]).await;
    }

    // ==================== 7. 来源作废 ⇒ 过期行清理 ====================

    #[tokio::test]
    async fn purges_rows_whose_source_no_longer_qualifies() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "PUR").await;
        let item = fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "2",
                amount: "40",
                status: "POSTED",
            })
            .await;

        collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("首轮归集");

        // 来源退回草稿 ⇒ 不再合规
        fixture.unpost_issue(item).await;
        let again = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("来源作废后重跑");
        assert_eq!(again.material.purged, 1);
        assert_eq!(again.material.inserted, 0);
        assert_eq!(dec(&again.material.purged_amount), dec("40"));
        assert_eq!(dec(&again.material.collected_amount), dec("0"), "软删行不计入合计");
        assert_eq!(fixture.collect_rows_of(item).await, 1, "行留痕（软删不物理删）");
        assert_eq!(fixture.collect_live_rows(item).await, 0, "但已不算活着");

        // 第三次收敛为无操作
        let third = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("第三次跑");
        assert_eq!(third.material.purged, 0);
        assert_eq!(third.material.inserted, 0);

        fixture.cleanup(&[object], &[]).await;
    }

    // ==================== 8. 缺工资率的工时 ====================

    #[tokio::test]
    async fn labor_without_rate_is_reported_not_written() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "NOR").await;
        fixture.labor(object, "6", "0", "CONFIRMED", &fixture.in_date).await;

        let preview_res = preview(&fixture.super_principal(), &object.to_string(), fixture.source_query())
            .await
            .expect("预览");
        assert_eq!(preview_res.labor.rows.len(), 1);
        assert_eq!(preview_res.labor.rows[0].state, dto::STATE_UNCOLLECTABLE);
        assert!(
            preview_res.labor.rows[0]
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("工资率"),
            "{:?}",
            preview_res.labor.rows[0].reason
        );
        assert_eq!(preview_res.labor.uncollected_rows, 1);

        let res = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("归集（缺费率的行跳过，不报错）");
        assert_eq!(res.labor.inserted, 0, "缺费率不许写 0 金额行");
        assert_eq!(res.labor.uncollected_rows, 1);
        assert_eq!(dec(&res.labor.collected_amount), dec("0"));
        assert!(res.message.contains("不合归集口径"), "{}", res.message);
        assert_eq!(collect_ids(object).await, "");

        fixture.cleanup(&[object], &[]).await;
    }

    // ==================== 9. 池行与别的批次不动 ====================

    #[tokio::test]
    async fn pool_rows_and_other_batches_are_untouched() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let mine = fixture.object("OPEN", "MINE").await;
        let other = fixture.object("OPEN", "OTHR").await;
        fixture
            .issue(Issue {
                object: mine,
                date: &fixture.in_date,
                qty: "1",
                amount: "10",
                status: "POSTED",
            })
            .await;
        fixture
            .issue(Issue {
                object: other,
                date: &fixture.in_date,
                qty: "3",
                amount: "90",
                status: "POSTED",
            })
            .await;
        // 费用池行（无对象）
        fixture.overhead(None, "70").await;

        // 别的批次先归集好
        collect(
            &fixture.super_principal(),
            None,
            &other.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("先归集别的批次");
        let object_ids_before = collect_ids(other).await;

        // 本批归集：不许动别的批次、不许动池行
        collect(
            &fixture.super_principal(),
            None,
            &mine.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("归集本批");

        assert_eq!(
            collect_ids(other).await,
            object_ids_before,
            "别的批次的归集行一行不许动"
        );
        let pool_rows: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IS NULL AND period_code = ?",
                vec![Value::Text(fixture.period.clone())],
            )
            .await
            .expect("查池行");
        assert_eq!(
            pool_rows, 0,
            "未挂对象的手工费用不进池行（它不是折旧/报销），也不能被归集到批次"
        );

        fixture.cleanup(&[mine, other], &[]).await;
    }

    // ==================== 10. 与期间 calc 不重复算 ====================

    #[tokio::test]
    async fn period_calc_after_batch_collect_does_not_double_count() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "CAL").await;
        fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "4",
                amount: "60",
                status: "POSTED",
            })
            .await;

        let batch = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("先按批归集");
        assert_eq!(dec(&batch.material.collected_amount), dec("60"));

        // 期间级 calc 料段：同一套幂等键 ⇒ 一行都不再写
        let outcome = crate::business::erp::cost::cost_calc_material::collect_material(&fixture.period, None)
            .await
            .expect("calc 料段");
        assert_eq!(outcome.inserted, 0, "按批归集过的行，calc 必须判成 already");
        assert!(outcome.skipped_existing >= 1);
        assert_eq!(outcome.collected_amount, dec("60"), "合计仍是 60，没有算两遍");

        fixture.cleanup(&[object], &[]).await;
    }

    // ==================== 11. 清场零残留 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let object = fixture.object("OPEN", "ZZZ").await;
        let item = fixture
            .issue(Issue {
                object,
                date: &fixture.in_date,
                qty: "1",
                amount: "10",
                status: "POSTED",
            })
            .await;
        fixture.labor(object, "1", "10", "CONFIRMED", &fixture.in_date).await;
        fixture.overhead(Some(object), "5").await;
        let period_batch = fixture.period_batch("DRAFT").await;

        let collected = collect(
            &fixture.super_principal(),
            None,
            &object.to_string(),
            fixture.collect_req(),
        )
        .await
        .expect("归集");
        // 本期已有 DRAFT 批次 ⇒ 必须提示「重跑 calc 才能让计算单跟上」（不提示就是让人以为账已经变了）
        assert!(collected.recalc_required, "本期有 DRAFT 批次 ⇒ 必须提示重算");
        assert!(
            collected.message.contains("重跑 calc"),
            "提示要能直接照做：{}",
            collected.message
        );

        fixture.cleanup(&[object], &[period_batch]).await;

        for (sql, params) in [
            (
                "SELECT count(*) FROM biz_cost_collect WHERE cost_object_id = ?::uuid OR source_id = ?::uuid",
                vec![Value::from(&object), Value::from(&item)],
            ),
            (
                "SELECT count(*) FROM biz_cost_labor WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object)],
            ),
            (
                "SELECT count(*) FROM biz_cost_overhead WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object)],
            ),
            (
                "SELECT count(*) FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object)],
            ),
            (
                "SELECT count(*) FROM biz_cost_period WHERE id = ?::uuid",
                vec![Value::from(&period_batch)],
            ),
        ] {
            let left: i64 = db().scalar_with_params(sql, params).await.expect("回查残留");
            assert_eq!(left, 0, "清场后残留：{sql}");
        }
    }

    /// 让 `SheetBatchRes` 在断言里可用（避免 unused import 告警）。
    #[test]
    fn batch_res_is_the_shared_shape() {
        fn assert_serialize<T: serde::Serialize>() {}
        assert_serialize::<SheetBatchRes>();
        assert_serialize::<BatchSourcesRes>();
        assert_serialize::<BatchCollectRes>();
    }
}
