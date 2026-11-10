//! 手工费用台账（`biz_cost_overhead`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b24 \
//!   cargo test --lib cost_overhead_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`create_list_delete_flow_end_to_end`] | **CRUD**（C/R/D）：建单 → 列表（期间 / 费用项目 / 关键字）→ 软删 → 列表消失 → 再删 404；`*_id` 都带 `*_name` |
//! | 2 | [`debit_account_outside_4101_is_rejected`] | **验收项**「科目不在 `4101.*` → 拒绝」：400 + `66060` + 零落库 |
//! | 3 | [`debit_account_must_exist_be_leaf_and_enabled`] | 账套现状三层：不存在 / 非末级 / 已停用都 400 + `66060`，末级启用科目放行 |
//! | 4 | [`amount_must_be_positive_at_entry_and_in_the_db`] | **验收项**「金额必须 > 0」：入口 0 / 负数 → 400 + `66061`；绕过入口直插 → `ck_biz_cost_oh_amount` → 同码 |
//! | 5 | [`cost_object_must_exist_and_be_open`] | 对象不存在 → 400/`66002`；已关批 → 409/`66057`；在制 → 落库并回显对象号 |
//! | 6 | [`deleting_a_collected_row_is_refused`] | 已被归集的行不许删（409 点名期间），把归集行删掉后才能删 |
//!
//! # 夹具纪律
//!
//! 全部用 `B24-<唯一后缀>` 前缀造，清场**按 id** 删（绝不按前缀扫全库）；
//! 删表顺序按 FK：归集 → 费用台账 → 成本对象 → 科目 / 物料 / 仓库 / 部门。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_overhead_dto;
    use crate::business::erp::cost_overhead_dto::{CostOverheadCreateReq, CostOverheadPageQuery};
    use crate::business::erp::cost_overhead_handler as handler;
    use crate::business::erp::cost_overhead_model as model;
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

    /// 唯一后缀（纳秒 + 进程内计数器，避免并发用例互相踩）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:03}", now % 1_000_000_000)
    }

    /// 仓库编码的尾巴（`biz_warehouse.warehouse_code` 有长度上限）。
    fn fx_suffix_code(suffix: &str) -> String {
        let start = suffix.len().saturating_sub(10);
        suffix[start..].to_string()
    }

    /// 全套夹具：车间 / 产出品 / 仓库 + **登记过的**行、对象、科目 id（清场用）。
    struct Fixture {
        suffix: String,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        rows: std::sync::Mutex<Vec<Uuid>>,
        objects: std::sync::Mutex<Vec<Uuid>>,
        accounts: std::sync::Mutex<Vec<Uuid>>,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let dept = crate::shared::test_helpers::ensure_any_dept().await;
            let unit: Uuid = db()
                .scalar_with_params("SELECT id FROM biz_unit WHERE deleted_at IS NULL LIMIT 1", vec![])
                .await
                .expect("取计量单位");
            let product = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                          status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?::uuid, 'FINISHED', 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&product),
                    Value::Text(format!("COSTB24-MAT-{suffix}")),
                    Value::Text(format!("费用夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB24W{}", fx_suffix_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("费用夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");
            Self {
                suffix,
                dept,
                product,
                warehouse,
                rows: std::sync::Mutex::new(Vec::new()),
                objects: std::sync::Mutex::new(Vec::new()),
                accounts: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn remember_row(&self, id: &str) {
            self.rows
                .lock()
                .expect("行清单")
                .push(Uuid::parse_str(id).expect("行 id"));
        }

        /// 直接造一张成本对象（避免依赖成本对象的取号 / 批次逻辑）。
        ///
        /// `status='CLOSED'` 必须同时给 `closed_at`（`ck_biz_cost_object_closed`）。
        async fn object_in(&self, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?::uuid, ?::uuid, 100, ?::uuid, '费用夹具车间', '2026-11-01', ?, \
                             {closed_at}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(format!("CB24-{}-{}", &id.to_string()[28..], self.suffix)),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            id
        }

        /// 造一个「临时科目」（`code` 自带唯一尾巴，避免撞 uk_biz_account_code）。
        ///
        /// 返回 id。`is_leaf` / `status` 可指定，用来覆盖「非末级 / 已停用」两条拒绝路径。
        /// **smallint 列写 SQL 字面量**：驱动不接受把文本绑到 int2（真库用例的老坑）。
        async fn account(&self, code: &str, is_leaf: i16, status: i16, parent: Option<Uuid>) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                format!(
                    "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                                              account_category, balance_direction, is_leaf, status, \
                                              created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, NULLIF(?::text,'')::uuid, 2, 'COST', 'DEBIT', {is_leaf}, \
                             {status}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(code.to_string()),
                    Value::Text(format!("费用夹具科目 {code}")),
                    Value::Text(parent.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await
            .expect("造科目夹具");
            self.accounts.lock().expect("科目清单").push(id);
            id
        }

        fn account_code(&self, tail: &str) -> String {
            format!("4101.{}{}", fx_suffix_code(&self.suffix), tail)
        }

        /// 建单入参：默认是一行「车间费用池」的水电费（不挂成本对象）。
        fn create_req(&self) -> CostOverheadCreateReq {
            CostOverheadCreateReq {
                period_code: Some("2026-11".to_string()),
                cost_object_id: None,
                dept_id: Some(self.dept.to_string()),
                expense_item: Some("UTILITY".to_string()),
                debit_account_code: Some("4101.02".to_string()),
                credit_account_code: Some("1002".to_string()),
                amount: Some(dec("4000")),
                remark: Some(format!("B2-4 真库用例 {}", self.suffix)),
            }
        }

        /// 清场：按 id 删（FK 顺序：归集 → 费用台账 → 成本对象 → 科目 / 物料 / 仓库）。
        async fn cleanup(&self) {
            let ids = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let rows = ids(&self.rows);
            let objects = ids(&self.objects);
            let accounts = ids(&self.accounts);
            let mut statements = Vec::new();
            if !rows.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_collect WHERE source_id IN ({rows})"));
                statements.push(format!("DELETE FROM biz_cost_overhead WHERE id IN ({rows})"));
            }
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            if !accounts.is_empty() {
                statements.push(format!("DELETE FROM biz_account WHERE id IN ({accounts})"));
            }
            statements.push(format!("DELETE FROM biz_material WHERE id = '{}'::uuid", self.product));
            statements.push(format!(
                "DELETE FROM biz_warehouse WHERE id = '{}'::uuid",
                self.warehouse
            ));
            for sql in statements {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }
    }

    /// 活行数（软删不算）。
    async fn live_rows(id: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_cost_overhead WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .expect("数费用台账行")
    }

    // ==================== 用例 ====================

    /// ① **验收项 CRUD**：建 / 列表 / 软删，名字与金额口径正确。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn create_list_delete_flow_end_to_end() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        let created = handler::create_overhead(None, fx.create_req())
            .await
            .expect("建费用台账行");
        fx.remember_row(&created.id);
        assert_eq!(created.period_code, "2026-11");
        assert_eq!(created.expense_item, "UTILITY");
        assert_eq!(
            created.expense_item_label.as_deref(),
            Some("水电费"),
            "费用项目名来自字典 cost_expense_item"
        );
        // 金额是字符串出参，标度与库列一致（NUMERIC(18,4)）
        assert_eq!(created.amount, "4000.0000");
        // 约定 13：*_id 都带 *_name
        assert_eq!(created.debit_account_code, "4101.02");
        assert_eq!(
            created.debit_account_name.as_deref(),
            Some("制造费用-水电费"),
            "科目名 join 自 biz_account"
        );
        assert_eq!(created.credit_account_code, "1002");
        assert_eq!(created.credit_account_name.as_deref(), Some("银行存款"));
        assert!(
            created.dept_name.as_deref().is_some_and(|name| !name.is_empty()),
            "{:?}",
            created.dept_name
        );
        // 不挂成本对象 = 车间费用池（合法形态）
        assert!(created.cost_object_id.is_none());
        assert!(created.cost_object_no.is_none());
        assert_eq!(created.source_type, "MANUAL");

        // 列表：期间 + 费用项目
        let page = handler::page_overheads(CostOverheadPageQuery {
            period_code: Some("2026-11".to_string()),
            expense_item: Some("UTILITY".to_string()),
            dept_id: Some(fx.dept.to_string()),
            ..Default::default()
        })
        .await
        .expect("列表");
        assert!(page.total >= 1, "本期本车间的水电费至少 1 条");
        let hit = page
            .list
            .iter()
            .find(|row| row.id == created.id)
            .expect("新建的行要出现在列表里");
        assert_eq!(hit.amount, "4000.0000");

        // 关键字：按借方科目编码查得到
        let page = handler::page_overheads(CostOverheadPageQuery {
            keyword: Some("4101.02".to_string()),
            dept_id: Some(fx.dept.to_string()),
            ..Default::default()
        })
        .await
        .expect("关键字列表");
        assert!(
            page.list.iter().any(|row| row.id == created.id),
            "按借方科目编码应能搜到"
        );

        // 软删
        let id = Uuid::parse_str(&created.id).expect("id");
        handler::delete_overhead(None, &created.id).await.expect("删");
        assert_eq!(live_rows(id).await, 0, "软删后不该再有活行");
        let page = handler::page_overheads(CostOverheadPageQuery {
            period_code: Some("2026-11".to_string()),
            dept_id: Some(fx.dept.to_string()),
            ..Default::default()
        })
        .await
        .expect("删后列表");
        assert!(
            !page.list.iter().any(|row| row.id == created.id),
            "软删后列表里不该再有这行"
        );
        // 再删一次 → 404（不是静默成功）
        let error = handler::delete_overhead(None, &created.id).await.expect_err("重复删除");
        assert_eq!(error.status, 404);
        // 库里仍是软删（行还在，可追溯）
        let raw: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_overhead WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("数行");
        assert_eq!(raw, 1, "软删不物理删，行要留着可追溯");

        fx.cleanup().await;
    }

    /// ② **验收项**「科目不在 `4101.*` → 拒绝」：400 + 66060，且不落库。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn debit_account_outside_4101_is_rejected() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        for bad in ["5602.01", "6401", "1405", "4101"] {
            let mut req = fx.create_req();
            req.debit_account_code = Some(bad.to_string());
            let error = handler::create_overhead(None, req)
                .await
                .expect_err("非 4101 科目必须被拒");
            assert_eq!(error.status, 400, "{bad}");
            assert_eq!(error.code, 66060, "{bad}");
            assert!(error.msg.contains("4101"), "{bad}: {}", error.msg);
            assert!(error.msg.contains(bad) || bad == "4101", "{bad}: {}", error.msg);
        }

        // 一条都没落库
        let rows: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_overhead WHERE remark = ?",
                vec![Value::Text(format!("B2-4 真库用例 {}", fx.suffix))],
            )
            .await
            .expect("数行");
        assert_eq!(rows, 0, "被拒的输入不该落库");

        fx.cleanup().await;
    }

    /// ③ 账套现状三层校验：不存在 / 非末级 / 已停用都拒绝，末级启用科目放行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn debit_account_must_exist_be_leaf_and_enabled() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        // (a) 前缀对、账套里没有
        let mut req = fx.create_req();
        let missing = fx.account_code("99");
        req.debit_account_code = Some(missing.clone());
        let error = handler::create_overhead(None, req).await.expect_err("不存在的科目");
        assert_eq!(error.code, 66060);
        assert!(error.msg.contains("不存在"), "{}", error.msg);
        assert!(error.msg.contains(&missing), "要点名编码：{}", error.msg);

        // (b) 非末级（汇总科目）
        let parent_code = fx.account_code("");
        let parent = fx.account(&parent_code, 0, 1, None).await;
        let mut req = fx.create_req();
        req.debit_account_code = Some(parent_code.clone());
        let error = handler::create_overhead(None, req).await.expect_err("非末级科目");
        assert_eq!(error.code, 66060);
        assert!(error.msg.contains("末级"), "{}", error.msg);
        assert!(error.msg.contains(&parent_code), "{}", error.msg);

        // (c) 末级但已停用
        let disabled_code = format!("{parent_code}.02");
        let _disabled = fx.account(&disabled_code, 1, 0, Some(parent)).await;
        let mut req = fx.create_req();
        req.debit_account_code = Some(disabled_code.clone());
        let error = handler::create_overhead(None, req).await.expect_err("停用科目");
        assert_eq!(error.code, 66060);
        assert!(error.msg.contains("停用"), "{}", error.msg);

        // (d) 末级 + 启用 ⇒ 放行（证明前三层不是把所有科目都拦了）
        let ok_code = format!("{parent_code}.01");
        let _ok = fx.account(&ok_code, 1, 1, Some(parent)).await;
        let mut req = fx.create_req();
        req.debit_account_code = Some(ok_code.clone());
        let created = handler::create_overhead(None, req).await.expect("末级启用科目");
        fx.remember_row(&created.id);
        assert_eq!(created.debit_account_code, ok_code);

        // 对方科目也走同一层校验（不存在就拒绝，别等到结算找不到科目）
        let mut req = fx.create_req();
        req.credit_account_code = Some("9999.99".to_string());
        let error = handler::create_overhead(None, req).await.expect_err("对方科目不存在");
        assert_eq!(error.code, 66060);
        assert!(error.msg.contains("credit_account_code"), "{}", error.msg);

        fx.cleanup().await;
    }

    /// ④ **验收项**「金额必须 > 0」：入口拦 + 库层 CHECK 兜底（同一条人话码）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn amount_must_be_positive_at_entry_and_in_the_db() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        for bad in ["0", "0.0000", "-1", "-4000.5"] {
            let mut req = fx.create_req();
            req.amount = Some(dec(bad));
            let error = handler::create_overhead(None, req).await.expect_err("非正金额必须被拒");
            assert_eq!(error.status, 400, "{bad}");
            assert_eq!(error.code, 66061, "{bad}");
            assert!(error.msg.contains("大于 0"), "{bad}: {}", error.msg);
        }
        // 5 位小数也不许静默四舍五入
        let mut req = fx.create_req();
        req.amount = Some(dec("12.34567"));
        let error = handler::create_overhead(None, req).await.expect_err("超精度金额");
        assert_eq!(error.code, 66061);
        assert!(error.msg.contains("四舍五入"), "{}", error.msg);

        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_overhead WHERE remark = ?",
                vec![Value::Text(format!("B2-4 真库用例 {}", fx.suffix))],
            )
            .await
            .expect("数行");
        assert_eq!(count, 0, "被拒的金额不该落库");

        // 绕过 handler 直插 amount = 0 ⇒ 库抛 ck_biz_cost_oh_amount ⇒ 翻译成同一条码
        let raw: DbResult<u64> = db()
            .exec_sql(
                "INSERT INTO biz_cost_overhead (period_code, dept_id, dept_name, expense_item, \
                                                debit_account_code, credit_account_code, amount, remark) \
                 VALUES ('2026-11', ?::uuid, '费用夹具车间', 'UTILITY', '4101.02', '1002', 0, ?)"
                    .to_string(),
                vec![
                    Value::from(&fx.dept),
                    Value::Text(format!("B2-4 直插零金额 {}", fx.suffix)),
                ],
            )
            .await;
        let err = raw.expect_err("amount = 0 必须被库拦下");
        assert!(
            matches!(
                &err,
                DbError::CheckViolation { constraint, .. }
                    if constraint.as_deref() == Some("ck_biz_cost_oh_amount")
            ),
            "应当是 ck_biz_cost_oh_amount 的 CheckViolation，实际：{err}"
        );
        let spec = cost_overhead_dto::spec_from_create(&fx.create_req(), "费用夹具车间").expect("规格");
        let error = cost_overhead_dto::map_write_error(err, &spec, "真库用例");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66061);
        assert!(error.msg.contains("大于 0"), "{}", error.msg);

        // 借方科目 CHECK 的绕过路径同样翻译成 66060
        let raw: DbResult<u64> = db()
            .exec_sql(
                "INSERT INTO biz_cost_overhead (period_code, dept_id, dept_name, expense_item, \
                                                debit_account_code, credit_account_code, amount, remark) \
                 VALUES ('2026-11', ?::uuid, '费用夹具车间', 'UTILITY', '5602.01', '1002', 100, ?)"
                    .to_string(),
                vec![
                    Value::from(&fx.dept),
                    Value::Text(format!("B2-4 直插非 4101 {}", fx.suffix)),
                ],
            )
            .await;
        let err = raw.expect_err("debit 不在 4101.* 必须被库拦下");
        assert!(
            matches!(
                &err,
                DbError::CheckViolation { constraint, .. }
                    if constraint.as_deref() == Some("ck_biz_cost_oh_debit")
            ),
            "应当是 ck_biz_cost_oh_debit 的 CheckViolation，实际：{err}"
        );
        let error = cost_overhead_dto::map_write_error(err, &spec, "真库用例");
        assert_eq!(error.code, 66060);

        fx.cleanup().await;
    }

    /// ⑤ 成本对象：不存在 → 400/66002；已关批 → 409/66057；在制 → 落库并回显对象号。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cost_object_must_exist_and_be_open() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;

        // (a) 不存在
        let mut req = fx.create_req();
        req.cost_object_id = Some(Uuid::now_v7().to_string());
        let error = handler::create_overhead(None, req).await.expect_err("对象不存在");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66002);

        // (b) 已关批
        let closed = fx.object_in("CLOSED").await;
        let mut req = fx.create_req();
        req.cost_object_id = Some(closed.to_string());
        let error = handler::create_overhead(None, req).await.expect_err("已关批");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66057);
        assert!(error.msg.contains("已关批"), "{}", error.msg);

        // (c) 在制 ⇒ 直接归集到该批（不是池行）
        let open = fx.object_in("OPEN").await;
        let mut req = fx.create_req();
        req.cost_object_id = Some(open.to_string());
        let created = handler::create_overhead(None, req).await.expect("在制对象可归集");
        fx.remember_row(&created.id);
        assert_eq!(created.cost_object_id.as_deref(), Some(open.to_string().as_str()));
        assert!(
            created
                .cost_object_no
                .as_deref()
                .is_some_and(|no| no.starts_with("CB24-")),
            "{:?}",
            created.cost_object_no
        );
        assert_eq!(created.cost_object_status.as_deref(), Some("OPEN"));
        // 这一行已有对象 ⇒ 落库的 cost_object_id 非空（池行的判据在 model 里）
        let row = model::find_by_id(Uuid::parse_str(&created.id).expect("id"))
            .await
            .expect("查行")
            .expect("行在");
        assert!(!row.is_pool_row(), "挂了对象就不是池行");
        assert!(
            row.debit_account_name.is_some() && row.credit_account_name.is_some(),
            "两个科目名都要 join 出来"
        );

        fx.cleanup().await;
    }

    /// ⑥ 已被归集的行不许删（409 点名期间）；归集行删掉后才能删（重算后的正常路径）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleting_a_collected_row_is_refused() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let created = handler::create_overhead(None, fx.create_req())
            .await
            .expect("建费用台账行");
        fx.remember_row(&created.id);
        let id = Uuid::parse_str(&created.id).expect("id");

        // 模拟 calc 第 5 段：把这条费用归集成「车间费用池」行（cost_object_id 为空）
        db().exec_sql(
            "INSERT INTO biz_cost_collect (period_code, element, source_type, source_id, source_no, \
                                           line_no, occur_date, amount, dept_id, dept_name, remark) \
             VALUES ('2026-11', 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, ?, 0, '2026-11-30', 4000, \
                     ?::uuid, '费用夹具车间', ?)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(format!("OH-{}", fx.suffix)),
                Value::from(&fx.dept),
                Value::Text(format!("B2-4 归集夹具 {}", fx.suffix)),
            ],
        )
        .await
        .expect("造归集夹具");

        let error = handler::delete_overhead(None, &created.id)
            .await
            .expect_err("已归集不许删");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("2026-11"), "要点名期间：{}", error.msg);
        assert!(error.msg.contains("归集"), "{}", error.msg);
        assert_eq!(live_rows(id).await, 1, "被拦下时行必须还在");

        // 归集行删掉（= 该期间 DRAFT 重算先删当期归集）⇒ 可以删
        db().exec_sql(
            "UPDATE biz_cost_collect SET deleted_at = now() WHERE source_id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await
        .expect("删归集夹具");
        handler::delete_overhead(None, &created.id)
            .await
            .expect("归集行删掉后可以删");
        assert_eq!(live_rows(id).await, 0);

        fx.cleanup().await;
    }
}
