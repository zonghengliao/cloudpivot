//! 分批法「完工批次结转（完工转出）」（M5-04）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m504_dev \
//!   cargo test --lib cost_batch_finish -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`preview_computes_full_transfer_without_writing`] | 整批完工 ⇒ 本次 = 累计归集全额、单位成本 round 6；**预览不写库** |
//! | 2 | [`finish_writes_one_row_and_replays_idempotently`] | 写 1 行且金额/快照落库；第二次调用 `inserted = false`，行数与 `updated_at` 不动 |
//! | 3 | [`second_transfer_only_moves_the_delta`] | 结转后新进来的成本 ⇒ 第二次只转差额（`transfer_seq = 2`），累计仍等于累计归集 |
//! | 4 | [`wip_blocks_finish_and_writes_nothing`] | 还有在制 ⇒ 409 + `66107` + `WIP_NOT_ZERO`，一行不写；预览 `transferable = false` 但**照样给数** |
//! | 5 | [`settled_item_blocks_finish`] | 计算单已 `SETTLED`（账已走）⇒ 409 + `ALREADY_SETTLED`，一行不写 |
//! | 6 | [`frozen_transfer_period_blocks`] | 结转期间批次 `SUBMITTED` ⇒ 409 + `PERIOD_FROZEN`；`DRAFT` 放行 |
//! | 7 | [`locked_period_blocks_finish`] | 本批有活动的期间已 `SETTLED` ⇒ 409 + `PERIOD_LOCKED` |
//! | 8 | [`draft_labor_and_uncollected_stock_out_block`] | 草稿工时 / 已过账未归集领料 ⇒ 409（成本还会变，不许结转） |
//! | 9 | [`cancelled_batch_and_out_of_scope_are_rejected`] | 作废批 409；别车间用户 404（不是 403，不当探测口） |
//! | 10 | [`zero_cost_and_zero_output_are_rejected`] | 零成本完工 / 完工数 0 ⇒ 409；预览给对应拦截码 |
//! | 11 | [`unique_index_blocks_two_rows_with_same_seq`] | 绕过编排直插「同批同 seq」两行 ⇒ 库层唯一键拦住（并发最后一道闸） |
//! | 12 | [`zz_teardown_leaves_zero_residue`] | 清场后结转单 / 归集 / 工时 / 出库 / 计算单 / 期间批次 / 对象全 0 |
//!
//! # 夹具纪律
//!
//! * 每个夹具独占一个**期间槽**（年份 = `2000 + slot % 80`、月份 = `1 + slot/80 % 12`），
//!   建前先清该槽上一次的残留；
//! * 单号 / 对象号 / 物料编码全部带唯一后缀，绝不按前缀扫全库；
//! * 归集行 / 工时 / 出库单 / 期间批次 / 计算单**直接 SQL 造**：本卡验的是「结转口径 +
//!   写入 + 状态闸」，要造的形态（`SETTLED` 计算单、`SUBMITTED` 期间批次、`DRAFT` 工时）
//!   API 造不出来或要绕很远的路。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_batch_finish::{finish, preview};
    use crate::business::erp::cost::cost_batch_finish_dto::{self as dto, BatchFinishQuery, BatchFinishReq};
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

    fn next_slot() -> u64 {
        static SLOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SLOT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    struct Fixture {
        suffix: String,
        period: String,
        day: String,
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
            let year = 2000 + (slot % 80) as i32;
            let month = 1 + ((slot / 80) % 12) as u32;
            let period = format!("{year:04}-{month:02}");
            Self::reset_period(&period).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let other_dept_code = format!("M504-DEPT2-{suffix}");
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &other_dept_code,
                &format!("M5-04 夹具第二车间 {suffix}"),
            )
            .await;
            let product = crate::test_seed::ensure_material_kind(
                &format!("M504-MAT-{suffix}"),
                &format!("M5-04 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse = crate::test_seed::ensure_warehouse(
                &format!("M54W{}", crate::test_seed::last_chars(&suffix, 8)),
                &format!("M5-04 夹具仓 {suffix}"),
            )
            .await;

            Self {
                suffix,
                period: period.clone(),
                day: format!("{period}-10"),
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

        /// 造成本对象（完工 / 在制 / 状态由用例给）。
        async fn object(&self, status: &str, completed: &str, wip: &str, tag: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 800, ?::text::numeric, ?::text::numeric, \
                             ?::uuid, ?, '{}', ?, {closed_at}, now(), now())",
                    self.day
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!(
                        "CB-M504-{tag}-{}",
                        crate::test_seed::last_chars(&self.suffix, 8)
                    )),
                    Value::from(&self.product),
                    Value::Text(format!(
                        "LOT-M504-{tag}-{}",
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

        /// 造一条归集行（料要物料 + 数量；工 / 费不许挂物料）。
        async fn collect(&self, object: Uuid, element: &str, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            let (material, qty) = if element == "MATERIAL" {
                ("?::uuid".to_string(), "?::text::numeric".to_string())
            } else {
                ("NULL".to_string(), "NULL".to_string())
            };
            let mut params = vec![
                Value::from(&id),
                Value::from(&object),
                Value::Text(self.period.clone()),
                Value::Text(element.to_string()),
                Value::Text(self.day.clone()),
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
                             ?::text::numeric, 'M5-04 夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集行夹具");
            id
        }

        /// 造一条草稿工时（`DRAFT` ⇒ 成本还会变）。
        async fn draft_labor(&self, object: Uuid) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, ?, ?::uuid, ?, \
                         10, 0, 50, round(10 * 50, 4), 'MANUAL', 'DRAFT', 'M5-04 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(self.period.clone()),
                    Value::Text(self.day.clone()),
                    Value::from(&object),
                    Value::Text(format!("M5-04 人员 {}", crate::test_seed::last_chars(&self.suffix, 6))),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                ],
            )
            .await
            .expect("造草稿工时夹具");
            id
        }

        /// 造一张已过账、挂本批、**没有归集行**的领料单（成本还没进筐）。
        async fn posted_stock_out(&self, object: Uuid) -> Uuid {
            let header = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, reason, cost_object_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, 'POSTED', 0, 0, 'M5-04 夹具', ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&header),
                    Value::Text(format!(
                        "M504OUT-{}",
                        crate::test_seed::last_chars(&header.simple().to_string(), 10)
                    )),
                    Value::from(&self.warehouse),
                    Value::Text(self.day.clone()),
                    Value::from(&object),
                ],
            )
            .await
            .expect("造领料单夹具");
            header
        }

        /// 造一张成本核算批次（`SETTLED` / `CLOSED` 需要 `settled_at + finish_voucher_id`）。
        async fn period_batch(&self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
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
                    Value::Text(format!("CS-M504-{}", crate::test_seed::last_chars(&self.suffix, 8))),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本批次夹具");
            id
        }

        /// 造一行计算单（`SETTLED` 必须挂入库单 + 成本守恒）。
        async fn period_item(&self, object: Uuid, period_batch: Uuid, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let (stock_in, finished) = if status == "SETTLED" {
                ("'00000000-0000-0000-0000-000000000003'::uuid", "0")
            } else {
                ("NULL", "0")
            };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                       finished_cost, status, stock_in_id, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, {finished}, ?, {stock_in}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::from(&period_batch),
                    Value::from(&object),
                    Value::from(&self.product),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造计算单夹具");
            id
        }

        fn query(&self) -> BatchFinishQuery {
            BatchFinishQuery {
                period_code: Some(self.period.clone()),
            }
        }

        fn req(&self) -> BatchFinishReq {
            BatchFinishReq {
                period_code: Some(self.period.clone()),
                remark: None,
            }
        }

        async fn finish_rows(&self, object: Uuid) -> i64 {
            db().scalar_with_params(
                "SELECT count(*) FROM biz_cost_batch_finish WHERE object_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&object)],
            )
            .await
            .expect("查结转单")
        }

        async fn finish_total(&self, object: Uuid) -> Decimal {
            db().scalar_with_params(
                "SELECT COALESCE(sum(total_amount), 0) FROM biz_cost_batch_finish \
                  WHERE object_id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&object)],
            )
            .await
            .expect("查结转合计")
        }

        /// 清掉本槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset_period(period: &str) {
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_batch_finish WHERE period_code = ? AND remark LIKE '%M5-04%'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period_item WHERE cost_object_id IN \
                       (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M504-%')"
                        .to_string(),
                    vec![],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect WHERE period_code = ? AND remark = 'M5-04 夹具'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_labor WHERE period_code = ? AND remark = 'M5-04 夹具'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_stock_out WHERE out_no LIKE 'M504OUT-%' \
                        AND out_date >= (? || '-01')::date \
                        AND out_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE period_code = ? AND batch_no LIKE 'CS-M504-%'".to_string(),
                    vec![Value::Text(period.to_string())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB-M504-%' \
                        AND start_date >= (? || '-01')::date \
                        AND start_date < ((? || '-01')::date + interval '1 month')"
                        .to_string(),
                    vec![Value::Text(period.to_string()), Value::Text(period.to_string())],
                )
                .await;
        }

        /// 清场（FK 顺序：结转单 → 计算单 → 归集 → 工时 → 出库 → 期间批次 → 对象）。
        async fn cleanup(&self, objects: &[Uuid], period_batches: &[Uuid]) {
            let list = |ids: &[Uuid]| {
                ids.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !objects.is_empty() {
                let objects_list = list(objects);
                for sql in [
                    format!("DELETE FROM biz_cost_batch_finish WHERE object_id IN ({objects_list})"),
                    format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({objects_list})"),
                    format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects_list})"),
                    format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN ({objects_list})"),
                    format!("DELETE FROM biz_stock_out WHERE cost_object_id IN ({objects_list})"),
                ] {
                    let _ = db().exec_sql(sql, vec![]).await;
                }
            }
            if !period_batches.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_period WHERE id IN ({})", list(period_batches)),
                        vec![],
                    )
                    .await;
            }
            if !objects.is_empty() {
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM biz_cost_object WHERE id IN ({})", list(objects)),
                        vec![],
                    )
                    .await;
            }
        }
    }

    // ==================== 1. 试算：整批全额 ====================

    #[tokio::test]
    async fn preview_computes_full_transfer_without_writing() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P1").await;
        f.collect(object, "MATERIAL", "15000").await;
        f.collect(object, "LABOR", "4000").await;
        f.collect(object, "OVERHEAD", "3311.1104").await;

        let res = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算应成功");
        assert!(
            res.blockers.is_empty(),
            "整批完工 + 有成本 ⇒ 无拦截：{:?}",
            res.blockers
        );
        assert!(res.transferable);
        assert_eq!(res.collected.material, "15000.0000");
        assert_eq!(res.collected.labor, "4000.0000");
        assert_eq!(res.collected.overhead, "3311.1104");
        assert_eq!(res.collected.total, "22311.1104");
        assert_eq!(res.pending.total, "22311.1104", "首次结转 = 累计全额");
        assert_eq!(res.unit_cost, "27.888888", "22311.1104 / 800 = 27.888888（round 6）");
        assert_eq!(res.finished_qty, "800.0000");
        assert_eq!(res.transfer_no, 1);
        assert!(res.history.is_empty());
        assert_eq!(f.finish_rows(object).await, 0, "预览不写库");
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 2. 执行 + 幂等重放 ====================

    #[tokio::test]
    async fn finish_writes_one_row_and_replays_idempotently() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P2").await;
        f.collect(object, "MATERIAL", "15000").await;
        f.collect(object, "LABOR", "4000").await;

        let first = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("结转应成功");
        assert!(first.inserted);
        assert!(first.finish_id.is_some());
        assert_eq!(first.transfer.total, "19000.0000");
        assert!(first.message.contains("第 1 次"), "报文要点名第几次：{}", first.message);
        assert!(
            first.message.contains("23.750000"),
            "报文里的单位成本必须是**本次**的（回查后的 pending 已归零，不能拿它算）：{}",
            first.message
        );
        assert_eq!(first.remaining.total, "0.0000");
        assert_eq!(f.finish_rows(object).await, 1);
        assert_eq!(f.finish_total(object).await, dec("19000"));

        // 库里那条结转单的快照与分量
        let (seq, mat, labor, oh, collected_total, unit_cost): (i32, Decimal, Decimal, Decimal, Decimal, Decimal) =
            db().query_first_sql(
                "SELECT transfer_seq, material_amount, labor_amount, overhead_amount, \
                            collected_total, unit_cost \
                       FROM biz_cost_batch_finish WHERE object_id = ?::uuid AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::from(&object)],
            )
            .await
            .expect("查结转单")
            .map(|row| {
                (
                    row.get_typed("transfer_seq").unwrap(),
                    row.get_typed("material_amount").unwrap(),
                    row.get_typed("labor_amount").unwrap(),
                    row.get_typed("overhead_amount").unwrap(),
                    row.get_typed("collected_total").unwrap(),
                    row.get_typed("unit_cost").unwrap(),
                )
            })
            .expect("结转单应存在");
        assert_eq!(seq, 1);
        assert_eq!(mat, dec("15000"));
        assert_eq!(labor, dec("4000"));
        assert_eq!(oh, Decimal::ZERO);
        assert_eq!(collected_total, dec("19000"), "结转单要存累计归集快照");
        assert_eq!(unit_cost, dec("23.75"));

        // 重放：不写第二行、金额不变
        let second = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("重放应 200");
        assert!(!second.inserted, "无差额时不写空单");
        assert_eq!(second.transfer.total, "0.0000");
        assert_eq!(f.finish_rows(object).await, 1, "幂等：行数不变");
        assert_eq!(f.finish_total(object).await, dec("19000"));
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 3. 差额结转 ====================

    #[tokio::test]
    async fn second_transfer_only_moves_the_delta() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P3").await;
        f.collect(object, "MATERIAL", "15000").await;
        finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("首次结转");

        // 结转之后又进来的真成本（迟到的工时）
        f.collect(object, "LABOR", "1200").await;
        let res = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("差额结转应成功");
        assert!(res.inserted);
        assert_eq!(res.transfer.material, "0.0000", "料没有新成本 ⇒ 不重复转");
        assert_eq!(res.transfer.labor, "1200.0000");
        assert_eq!(res.transfer.total, "1200.0000");
        assert_eq!(res.preview.history.len(), 2);
        assert_eq!(res.preview.history[0].transfer_seq, 2, "历史按新 → 旧");
        assert_eq!(f.finish_rows(object).await, 2);
        assert_eq!(f.finish_total(object).await, dec("16200"), "累计 = 累计归集");
        assert_eq!(res.preview.pending.total, "0.0000");
        assert_eq!(res.preview.transferred.total, "16200.0000");
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 4. 在制拦截 ====================

    #[tokio::test]
    async fn wip_blocks_finish_and_writes_nothing() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "600", "200", "P4").await;
        f.collect(object, "MATERIAL", "15000").await;

        let preview_res = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算应成功（看数不拦）");
        assert!(!preview_res.transferable);
        assert_eq!(preview_res.blockers[0].code, dto::BLOCK_WIP_NOT_ZERO);
        assert_eq!(preview_res.pending.total, "15000.0000", "被拦也照样给数");

        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("在制不为 0 必须拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66107);
        assert!(error.msg.contains("在制"));
        assert!(error.data.is_some(), "错误体要带结构化 blockers");
        assert_eq!(f.finish_rows(object).await, 0, "被拦 ⇒ 一行不写");
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 5. 已随期间结算 ====================

    #[tokio::test]
    async fn settled_item_blocks_finish() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P5").await;
        f.collect(object, "MATERIAL", "15000").await;
        let pb = f.period_batch("DRAFT").await;
        f.period_item(object, pb, "SETTLED").await;

        let res = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算应成功");
        assert!(res.settled_in_books);
        let codes: Vec<_> = res.blockers.iter().map(|b| b.code).collect();
        assert!(codes.contains(&dto::BLOCK_ALREADY_SETTLED), "实际拦截：{codes:?}");

        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("账已走 ⇒ 不许补工作底稿");
        assert_eq!(error.code, 66107);
        assert_eq!(f.finish_rows(object).await, 0);
        f.cleanup(&[object], &[pb]).await;
    }

    // ==================== 6. 期间冻结 ====================

    #[tokio::test]
    async fn frozen_transfer_period_blocks() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P6").await;
        f.collect(object, "MATERIAL", "15000").await;

        // DRAFT 放行
        let pb = f.period_batch("DRAFT").await;
        let ok = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算");
        assert!(ok.blockers.is_empty(), "DRAFT 期间批次是正常在途态：{:?}", ok.blockers);

        // SUBMITTED 冻结核转期间
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'SUBMITTED', \
                    approval_instance_id = '00000000-0000-0000-0000-000000000001'::uuid \
              WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&pb)],
        )
        .await
        .expect("改为 SUBMITTED");
        let blocked = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算");
        assert!(
            blocked.blockers.iter().any(|b| b.code == dto::BLOCK_PERIOD_FROZEN),
            "冻结期间必须拦：{:?}",
            blocked.blockers
        );
        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("冻结必须 409");
        assert_eq!(error.code, 66107);
        assert_eq!(f.finish_rows(object).await, 0);
        f.cleanup(&[object], &[pb]).await;
    }

    // ==================== 7. 期间锁定（已结算期间） ====================

    #[tokio::test]
    async fn locked_period_blocks_finish() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P7").await;
        f.collect(object, "MATERIAL", "15000").await;
        // 本批有活动的期间已经 SETTLED ⇒ 归集基线被固定，再结转会和账上打架
        let pb = f.period_batch("SETTLED").await;

        let res = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算");
        assert!(
            res.blockers.iter().any(|b| b.code == dto::BLOCK_PERIOD_LOCKED),
            "已结算期间必须拦：{:?}",
            res.blockers
        );
        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("已结算期间必须 409");
        assert_eq!(error.code, 66107);
        assert_eq!(f.finish_rows(object).await, 0);
        f.cleanup(&[object], &[pb]).await;
    }

    // ==================== 8. 数据卫生（草稿工时 / 未归集领料） ====================

    #[tokio::test]
    async fn draft_labor_and_uncollected_stock_out_block() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P8").await;
        f.collect(object, "MATERIAL", "15000").await;
        let labor = f.draft_labor(object).await;
        let stock_out = f.posted_stock_out(object).await;

        let res = preview(&f.super_principal(), &object.to_string(), f.query())
            .await
            .expect("试算");
        let codes: Vec<_> = res.blockers.iter().map(|b| b.code).collect();
        assert!(codes.contains(&dto::BLOCK_DRAFT_LABOR), "实际：{codes:?}");
        assert!(codes.contains(&dto::BLOCK_UNCOLLECTED_STOCK_OUT), "实际：{codes:?}");

        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("成本还会变 ⇒ 不许结转");
        assert_eq!(error.code, 66107);
        assert_eq!(f.finish_rows(object).await, 0);

        // 补上：工时确认 + 领料归集（手工补一条归集行指向该出库单）
        db().exec_sql(
            "UPDATE biz_cost_labor SET status = 'CONFIRMED' WHERE id = ?::uuid".to_string(),
            vec![Value::from(&labor)],
        )
        .await
        .expect("确认工时");
        db().exec_sql(
            "INSERT INTO biz_cost_collect (cost_object_id, period_code, element, source_type, \
                                           source_id, source_no, line_no, occur_date, material_id, qty, amount, \
                                           created_at, updated_at) \
             VALUES (?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, 'M504OUT-FIX', 1, ?::text::date, \
                     ?::uuid, 1, 100, now(), now())"
                .to_string(),
            vec![
                Value::from(&object),
                Value::Text(f.period.clone()),
                Value::from(&stock_out),
                Value::Text(f.day.clone()),
                Value::from(&f.product),
            ],
        )
        .await
        .expect("补归集行");
        let ok = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("补齐之后应放行");
        assert!(ok.inserted);
        assert_eq!(ok.transfer.total, "15100.0000", "补归集的 100 也要转出去");
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 9. 作废 / 数据范围 ====================

    #[tokio::test]
    async fn cancelled_batch_and_out_of_scope_are_rejected() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("CANCELLED", "800", "0", "P9").await;
        f.collect(object, "MATERIAL", "15000").await;

        let error = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("作废批不结转");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66107);
        assert!(error.msg.contains("CANCELLED") || error.msg.contains("作废"));

        let error = finish(&f.other_dept_principal(), None, &object.to_string(), f.req())
            .await
            .expect_err("别车间用户看不到");
        assert_eq!(error.status, 404, "不可见报 404 不报 403");
        let error = preview(&f.other_dept_principal(), &object.to_string(), f.query())
            .await
            .expect_err("试算同样 404");
        assert_eq!(error.status, 404);
        assert_eq!(f.finish_rows(object).await, 0);
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 10. 零成本 / 零产出 ====================

    #[tokio::test]
    async fn zero_cost_and_zero_output_are_rejected() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let no_cost = f.object("OPEN", "800", "0", "P10A").await;
        let no_output = f.object("OPEN", "0", "0", "P10B").await;
        f.collect(no_output, "MATERIAL", "15000").await;

        let res = preview(&f.super_principal(), &no_cost.to_string(), f.query())
            .await
            .expect("试算");
        assert!(res.blockers.iter().any(|b| b.code == dto::BLOCK_NO_COST));
        let error = finish(&f.super_principal(), None, &no_cost.to_string(), f.req())
            .await
            .expect_err("零成本完工必须拒");
        assert_eq!(error.code, 66107);

        let res = preview(&f.super_principal(), &no_output.to_string(), f.query())
            .await
            .expect("试算");
        assert!(res.blockers.iter().any(|b| b.code == dto::BLOCK_NO_FINISHED_QTY));
        let error = finish(&f.super_principal(), None, &no_output.to_string(), f.req())
            .await
            .expect_err("没有产出必须拒");
        assert_eq!(error.code, 66107);
        assert_eq!(f.finish_rows(no_cost).await, 0);
        assert_eq!(f.finish_rows(no_output).await, 0);
        f.cleanup(&[no_cost, no_output], &[]).await;
    }

    // ==================== 11. 库层唯一键（并发最后一道闸） ====================

    #[tokio::test]
    async fn unique_index_blocks_two_rows_with_same_seq() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "P11").await;
        let insert = |seq: i32| {
            db().exec_sql(
                "INSERT INTO biz_cost_batch_finish (object_id, object_no, period_code, transfer_seq, \
                                                    product_id, finished_qty, unit_cost, total_amount) \
                 VALUES (?::uuid, 'CB-M504-P11', ?, ?, ?::uuid, 1, 1, 0)"
                    .to_string(),
                vec![
                    Value::from(&object),
                    Value::Text(f.period.clone()),
                    Value::I64(i64::from(seq)),
                    Value::from(&f.product),
                ],
            )
        };
        insert(1).await.expect("第一行应可插");
        let error = insert(1).await.expect_err("同批同 seq 的第二行必须撞唯一键");
        let text = format!("{error}");
        assert!(
            text.contains("uk_biz_cost_batch_finish_seq") || text.to_lowercase().contains("unique"),
            "报错要点名唯一键：{text}"
        );
        insert(2).await.expect("下一个 seq 应可插");
        assert_eq!(f.finish_rows(object).await, 2);
        f.cleanup(&[object], &[]).await;
    }

    // ==================== 12. 清场零残留 ====================

    /// 放在最后跑（`zz_` 前缀）：把本文件造过的形态全清掉，再按 id 回查一张都不剩。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("OPEN", "800", "0", "ZZ").await;
        f.collect(object, "MATERIAL", "15000").await;
        f.collect(object, "LABOR", "4000").await;
        let res = finish(&f.super_principal(), None, &object.to_string(), f.req())
            .await
            .expect("结转");
        assert!(res.inserted);

        f.cleanup(&[object], &[]).await;
        for (label, sql) in [
            (
                "结转单",
                format!("SELECT count(*) FROM biz_cost_batch_finish WHERE object_id = '{object}'::uuid"),
            ),
            (
                "归集行",
                format!("SELECT count(*) FROM biz_cost_collect WHERE cost_object_id = '{object}'::uuid"),
            ),
            (
                "成本对象",
                format!("SELECT count(*) FROM biz_cost_object WHERE id = '{object}'::uuid"),
            ),
        ] {
            let left: i64 = db().scalar_with_params(&sql, vec![]).await.expect("回查残留");
            assert_eq!(left, 0, "{label} 清场后仍有 {left} 行");
        }
    }
}
