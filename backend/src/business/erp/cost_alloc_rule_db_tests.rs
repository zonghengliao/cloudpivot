//! 分摊规则（`biz_cost_alloc_rule`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b24 \
//!   cargo test --lib cost_alloc_rule_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`rule_crud_and_effective_version_selection`] | **CRUD**（C/R/U/D）+ **卡面验收项①「规则按生效期选最新版本」**：11 月取 11-01 的版本、10 月仍取旧版本；停用 / 未生效版本不参与 |
//! | 2 | [`preview_uses_all_four_bases`] | **卡面验收项③「四种分配标准各一例」**：LABOR_HOURS / MACHINE_HOURS / OUTPUT_QTY / MATERIAL_COST 的基数逐对象取对 + 守恒 |
//! | 3 | [`preview_rounds_remainder_onto_one_row`] | 最大余数法：1000 / 1:1:1 → 333.3334 / 333.3333 / 333.3333，尾差只在唯一一行、Σ = 池 |
//! | 4 | [`preview_refuses_pool_without_object_or_basis`] | 池 > 0 且无在产对象 → 409 + `66062`；池 > 0 且基数合计 0 → 409 + `66063` |
//! | 5 | [`referenced_rule_cannot_be_edited_or_deleted`] | 已产生分摊结果的规则：改口径 / 删除 → 409；改名字 → 放行；删掉结果后可删 |
//! | 6 | [`unassigned_pool_is_reported_not_swallowed`] | 来源单据被删的池行进 `unallocated` 诊断（不静默蒸发） |
//!
//! # 夹具纪律
//!
//! 全部用 `B25-<唯一后缀>` 造，清场**按 id** 删（绝不按前缀扫全库）；
//! 删表顺序按 FK：分摊结果 → 分摊规则 → 计算单 → 批次 → 归集 → 工时 → 费用台账 →
//! 成本对象 → 科目 / 物料 / 仓库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_alloc_rule_dto::{
        self as dto, CostAllocPreviewReq, CostAllocRuleCreateReq, CostAllocRulePageQuery, CostAllocRuleUpdateReq,
    };
    use crate::business::erp::cost_alloc_rule_handler as handler;
    use crate::business::erp::cost_alloc_rule_model as model;
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

    /// 编码尾巴（`account_code` / `warehouse_code` 有长度上限）。
    fn tail_code(suffix: &str) -> String {
        suffix
            .chars()
            .filter(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }

    /// 全套夹具：车间 / 产出品 / 仓库 / 科目 / 对象 / 规则 / 归集 / 工时 / 批次，全部登记 id。
    struct Fixture {
        suffix: String,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        rules: std::sync::Mutex<Vec<Uuid>>,
        objects: std::sync::Mutex<Vec<Uuid>>,
        accounts: std::sync::Mutex<Vec<Uuid>>,
        overheads: std::sync::Mutex<Vec<Uuid>>,
        periods: std::sync::Mutex<Vec<Uuid>>,
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
                    Value::Text(format!("COSTB25-{suffix}")),
                    Value::Text(format!("分摊夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB25W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("分摊夹具仓 {suffix}")),
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
                rules: std::sync::Mutex::new(Vec::new()),
                objects: std::sync::Mutex::new(Vec::new()),
                accounts: std::sync::Mutex::new(Vec::new()),
                overheads: std::sync::Mutex::new(Vec::new()),
                periods: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn remember(list: &std::sync::Mutex<Vec<Uuid>>, id: &str) {
            list.lock().expect("清单").push(Uuid::parse_str(id).expect("id"));
        }

        /// 造一个启用末级科目 `4101.<8 位数字><tail>`（`tail` 区分不同规则，**必须是数字**：
        /// `is_manufacturing_account` 要求每段都是数字，字母尾巴会被当场 400 拒掉）。
        async fn account(&self, tail: &str) -> String {
            let code = format!("4101.{}{}", tail_code(&self.suffix), tail);
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                                          account_category, balance_direction, is_leaf, status, \
                                          created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, NULL, 2, 'COST', 'DEBIT', 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(code.clone()),
                    Value::Text(format!("分摊夹具科目 {code}")),
                ],
            )
            .await
            .expect("造科目夹具");
            self.accounts.lock().expect("科目清单").push(id);
            code
        }

        /// 造一张成本对象。
        async fn object(&self, status: &str) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB25-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, status, \
                                                  closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?::uuid, ?::uuid, 100, ?::uuid, '分摊夹具车间', '2026-11-01', ?, \
                             {closed_at}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::from(&self.dept),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            (id, object_no)
        }

        /// 造一条「车间费用池」来源：手工费用台账行 + 归集行（科目从台账反查）。
        async fn pool(&self, account_code: &str, period_code: &str, amount: &str) -> Uuid {
            let overhead_id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, NULL, ?::uuid, '分摊夹具车间', 'OTHER', ?, '1002', \
                         ?::text::numeric, 'MANUAL', '分摊夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&overhead_id),
                    Value::Text(period_code.to_string()),
                    Value::from(&self.dept),
                    Value::Text(account_code.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造费用台账夹具");
            self.overheads.lock().expect("台账清单").push(overhead_id);
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, amount, dept_id, \
                                               dept_name, remark, created_at, updated_at) \
                 VALUES (?::uuid, NULL, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, ?, 0, ?::text::date, \
                         ?::text::numeric, ?::uuid, '分摊夹具车间', '分摊夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(period_code.to_string()),
                    Value::from(&overhead_id),
                    Value::Text(format!("OH25-{}", &overhead_id.to_string()[28..])),
                    Value::Text(format!("{period_code}-28")),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造归集夹具");
            overhead_id
        }

        /// 造工时行（`status='CONFIRMED'`）。
        async fn labor(&self, object_id: Uuid, hours: &str, machine_hours: &str, period_code: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, '分摊夹具员工', ?::uuid, '分摊夹具车间', \
                         ?::text::numeric, ?::text::numeric, 30.000000, \
                         round(?::text::numeric * 30.000000, 4), 'MANUAL', 'CONFIRMED', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(period_code.to_string()),
                    Value::Text(format!("{period_code}-15")),
                    Value::from(&object_id),
                    Value::from(&self.dept),
                    Value::Text(hours.to_string()),
                    Value::Text(machine_hours.to_string()),
                    Value::Text(hours.to_string()),
                ],
            )
            .await
            .expect("造工时夹具");
        }

        /// 造料归集行（`element='MATERIAL'`）。
        async fn material(&self, object_id: Uuid, period_code: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                               source_id, source_no, line_no, occur_date, material_id, qty, \
                                               unit_price, amount, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, 'MATERIAL', 'STOCK_OUT', ?::uuid, ?, 0, ?::text::date, \
                         ?::uuid, 10, 1.000000, ?::text::numeric, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&object_id),
                    Value::Text(period_code.to_string()),
                    Value::from(&Uuid::now_v7()),
                    Value::Text(format!("SO25-{}", &object_id.to_string()[28..])),
                    Value::Text(format!("{period_code}-10")),
                    Value::from(&self.product),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造料归集夹具");
        }

        /// 造一个成本批次 + 每个对象一张计算单（`completed_qty` 由调用方给）。
        async fn period(&self, period_code: &str, items: &[(Uuid, &str)]) -> Uuid {
            let period_id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&period_id),
                    Value::Text(period_code.to_string()),
                    Value::Text(format!("CS25-{}-{}", &period_code.replace('-', ""), &self.suffix[..4])),
                ],
            )
            .await
            .expect("造成本批次夹具");
            self.periods.lock().expect("批次清单").push(period_id);
            for (object_id, qty) in items {
                db().exec_sql(
                    "INSERT INTO biz_cost_period_item (id, period_id, cost_object_id, product_id, \
                                                       completed_qty, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&Uuid::now_v7()),
                        Value::from(&period_id),
                        Value::from(object_id),
                        Value::from(&self.product),
                        Value::Text(qty.to_string()),
                    ],
                )
                .await
                .expect("造计算单夹具");
            }
            period_id
        }

        /// 夹具专属会计期间：年份由后缀推出（2000–2089）。
        ///
        /// 为什么不用固定的 `2026-11`：`uk_biz_cost_period` 对「同期间只有一个活跃批次」建了
        /// 唯一索引，多个夹具（或上一次跑失败的残留）用同一个期间就会互相撞。年份按后缀推，
        /// 每个夹具各占一个期间。
        fn period_year(&self) -> i32 {
            let digits: String = self.suffix.chars().filter(|ch| ch.is_ascii_digit()).collect();
            let tail = digits
                .chars()
                .rev()
                .take(2)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
                .parse::<i32>()
                .unwrap_or(0);
            2000 + tail % 90
        }

        /// 夹具期间（11 月）。
        fn period_code(&self) -> String {
            format!("{}-11", self.period_year())
        }

        /// 夹具期间内的某月 1 日（`month` = 1..=12）。
        fn date_of(&self, month: u8) -> String {
            format!("{}-{month:02}-01", self.period_year())
        }

        /// 建规则入参。
        fn rule_req(&self, code_tail: &str, account_code: &str, basis: &str, from: &str) -> CostAllocRuleCreateReq {
            CostAllocRuleCreateReq {
                rule_code: Some(format!("B25-{}-{code_tail}", &self.suffix[..6])),
                rule_name: Some(format!("分摊夹具规则 {code_tail}")),
                dept_id: Some(self.dept.to_string()),
                account_code: Some(account_code.to_string()),
                basis: Some(basis.to_string()),
                priority: Some(10),
                effective_from: Some(from.to_string()),
                effective_to: None,
                status: Some(1),
                remark: Some("B2-5 真库用例".to_string()),
            }
        }

        /// 清场：按 id 删（FK 顺序见模块头）。
        async fn cleanup(&self) {
            let ids = |list: &std::sync::Mutex<Vec<Uuid>>| {
                let list = list.lock().expect("清单");
                list.iter()
                    .map(|id| format!("'{id}'::uuid"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let rules = ids(&self.rules);
            let objects = ids(&self.objects);
            let accounts = ids(&self.accounts);
            let overheads = ids(&self.overheads);
            let periods = ids(&self.periods);
            let mut statements = Vec::new();
            if !rules.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_alloc_result WHERE rule_id IN ({rules})"));
                statements.push(format!("DELETE FROM biz_cost_alloc_rule WHERE id IN ({rules})"));
            }
            if !periods.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN ({periods})"
                ));
                statements.push(format!("DELETE FROM biz_cost_period WHERE id IN ({periods})"));
            }
            if !objects.is_empty() {
                statements.push(format!(
                    "DELETE FROM biz_cost_collect WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!(
                    "DELETE FROM biz_cost_labor WHERE cost_object_id IN ({objects})"
                ));
                statements.push(format!("DELETE FROM biz_cost_object WHERE id IN ({objects})"));
            }
            if !overheads.is_empty() {
                statements.push(format!("DELETE FROM biz_cost_collect WHERE source_id IN ({overheads})"));
                statements.push(format!("DELETE FROM biz_cost_overhead WHERE id IN ({overheads})"));
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

    fn preview_req(period_code: &str, rule_ids: Vec<Uuid>) -> CostAllocPreviewReq {
        CostAllocPreviewReq {
            period_code: Some(period_code.to_string()),
            rule_ids: Some(rule_ids.into_iter().map(|id| id.to_string()).collect()),
        }
    }

    // ==================== 用例 ====================

    /// ① **CRUD + 卡面验收项「规则按生效期选最新版本」**。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rule_crud_and_effective_version_selection() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let account = fx.account("01").await;
        let period = fx.period_code();
        let january = fx.date_of(1);
        let february = fx.date_of(2);
        let october = format!("{}-10", fx.period_year());
        let november_from = fx.date_of(11);
        let rule_prefix = format!("B25-{}", &fx.suffix[..6]);

        let old = handler::create_rule(None, fx.rule_req("V1", &account, "LABOR_HOURS", &january))
            .await
            .expect("建旧版本");
        Fixture::remember(&fx.rules, &old.id);
        assert_eq!(old.effective_from, january);
        let dept_name = crate::business::erp::cost_overhead_model::dept_name_of(fx.dept)
            .await
            .expect("查车间名")
            .expect("车间存在");
        assert_eq!(
            old.source_dept_name.as_deref(),
            Some(dept_name.as_str()),
            "车间名快照取自 sys_dept"
        );

        let new = handler::create_rule(None, fx.rule_req("V2", &account, "MACHINE_HOURS", &november_from))
            .await
            .expect("建新版本");
        Fixture::remember(&fx.rules, &new.id);

        // 同 (车间, 科目, 生效日) 的第二条版本 → 409
        let duplicate = handler::create_rule(None, fx.rule_req("V2B", &account, "MACHINE_HOURS", &november_from))
            .await
            .expect_err("同一天两条版本");
        assert_eq!(duplicate.status, 409, "{}", duplicate.msg);
        assert!(duplicate.msg.contains("已经有一条规则"), "{}", duplicate.msg);

        // 编码重复 → 409（唯一索引映射）
        let mut same_code = fx.rule_req("V1", &account, "LABOR_HOURS", &february);
        same_code.rule_code = Some(new.rule_code.clone());
        let taken = handler::create_rule(None, same_code).await.expect_err("编码重复");
        assert_eq!(taken.status, 409, "{}", taken.msg);
        assert!(taken.msg.contains("已被占用"), "{}", taken.msg);

        // 列表：同族两条版本都在；带期间筛 → 两条都覆盖该期间
        let all = handler::page_rules(CostAllocRulePageQuery {
            keyword: Some(rule_prefix),
            ..Default::default()
        })
        .await
        .expect("列表");
        assert_eq!(all.total, 2, "同一规则族的两条版本都在");

        let in_november = handler::page_rules(CostAllocRulePageQuery {
            period_code: Some(period.clone()),
            dept_id: Some(fx.dept.to_string()),
            account_code: Some(account.clone()),
            ..Default::default()
        })
        .await
        .expect("按期间筛");
        assert_eq!(in_november.total, 2, "两条版本都覆盖 11 月（取最新的是试算的事）");

        // 试算：11 月取新版本，10 月取旧版本（**卡面验收项①**）
        let both = vec![
            Uuid::parse_str(&old.id).expect("旧版本 id"),
            Uuid::parse_str(&new.id).expect("新版本 id"),
        ];
        let november = handler::preview_allocations(preview_req(&period, both.clone()))
            .await
            .expect("11 月试算");
        let group = november
            .groups
            .iter()
            .find(|group| group.account_code == account)
            .expect("11 月的规则组");
        assert_eq!(group.rule_id, new.id.to_string(), "11 月取生效更晚的版本");
        assert_eq!(group.basis, "MACHINE_HOURS");

        let october_preview = handler::preview_allocations(preview_req(&october, both.clone()))
            .await
            .expect("10 月试算");
        let group = october_preview
            .groups
            .iter()
            .find(|group| group.account_code == account)
            .expect("10 月的规则组");
        assert_eq!(group.rule_id, old.id.to_string(), "10 月仍取 1 月生效的旧版本");
        assert_eq!(group.basis, "LABOR_HOURS");

        // 停用新版本 → 11 月回落到旧版本
        handler::update_rule(
            None,
            &new.id,
            CostAllocRuleUpdateReq {
                status: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("停用新版本");
        let november = handler::preview_allocations(preview_req(&period, both))
            .await
            .expect("停用后 11 月试算");
        let group = november
            .groups
            .iter()
            .find(|group| group.account_code == account)
            .expect("规则组");
        assert_eq!(group.rule_id, old.id.to_string(), "停用的版本不参与选择");

        // 改名（PUT 允许）+ 删除（U / D）
        let renamed = handler::update_rule(
            None,
            &old.id,
            CostAllocRuleUpdateReq {
                rule_name: Some("改名后的分摊规则".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("改规则");
        assert_eq!(renamed.rule_name, "改名后的分摊规则");

        handler::delete_rule(None, &old.id).await.expect("删规则");
        let gone = handler::delete_rule(None, &old.id).await.expect_err("重复删 → 404");
        assert_eq!(gone.status, 404);

        fx.cleanup().await;
    }

    /// ② **卡面验收项「四种分配标准各一例」**：四条规则同车间同期、四个科目。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_uses_all_four_bases() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let period_code = fx.period_code();
        let period = period_code.as_str();

        let labor_account = fx.account("01").await;
        let machine_account = fx.account("02").await;
        let qty_account = fx.account("03").await;
        let material_account = fx.account("04").await;

        // 三个参与分摊的对象 + 一个「没投入」的对象（不许被摊）
        let (object_a, _) = fx.object("OPEN").await;
        let (object_b, _) = fx.object("CLOSED").await;
        let (object_c, _) = fx.object("OPEN").await;
        let (idle, _) = fx.object("OPEN").await;

        // 工时：A 6h/2mh、B 3h/1mh、C 1h/3mh（idle 无工时）
        fx.labor(object_a, "6", "2", period).await;
        fx.labor(object_b, "3", "1", period).await;
        fx.labor(object_c, "1", "3", period).await;
        // 料：A 1000、B 2000、C 3000（idle 无料）
        fx.material(object_a, period, "1000").await;
        fx.material(object_b, period, "2000").await;
        fx.material(object_c, period, "3000").await;
        // 完工数量：A 60、B 20、C 20
        fx.period(period, &[(object_a, "60"), (object_b, "20"), (object_c, "20")])
            .await;
        // 四个科目的池各 1000
        for account in [&labor_account, &machine_account, &qty_account, &material_account] {
            fx.pool(account, period, "1000").await;
        }

        let rules = [
            ("L", &labor_account, "LABOR_HOURS"),
            ("M", &machine_account, "MACHINE_HOURS"),
            ("Q", &qty_account, "OUTPUT_QTY"),
            ("C", &material_account, "MATERIAL_COST"),
        ];
        let mut rule_ids = Vec::new();
        for (tail, account, basis) in rules {
            let created = handler::create_rule(None, fx.rule_req(tail, account, basis, &fx.date_of(1)))
                .await
                .expect("建规则");
            Fixture::remember(&fx.rules, &created.id);
            rule_ids.push(Uuid::parse_str(&created.id).expect("id"));
        }

        let preview = handler::preview_allocations(preview_req(period, rule_ids))
            .await
            .expect("四种标准试算");
        assert_eq!(preview.groups.len(), 4, "四条规则各一组");
        assert_eq!(preview.total_pool_amount, "4000.0000");
        assert_eq!(preview.total_alloc_amount, "4000.0000", "守恒：Σ alloc = Σ pool");

        let row_of = |group: &dto::CostAllocPreviewGroup, object_id: Uuid| {
            group
                .rows
                .iter()
                .find(|row| row.cost_object_id == object_id.to_string())
                .unwrap_or_else(|| panic!("{} 缺对象 {object_id}", group.rule_code))
                .clone()
        };

        // LABOR_HOURS：基数 6 / 3 / 1（合计 10）→ 600 / 300 / 100
        let group = preview
            .groups
            .iter()
            .find(|group| group.account_code == labor_account)
            .expect("人工工时组");
        assert_eq!(group.basis_total, "10.0000");
        assert_eq!(row_of(group, object_a).basis_value, "6.0000");
        assert_eq!(row_of(group, object_a).alloc_amount, "600.0000");
        assert_eq!(row_of(group, object_b).alloc_amount, "300.0000");
        assert_eq!(row_of(group, object_c).alloc_amount, "100.0000");
        assert!(
            !group.rows.iter().any(|row| row.cost_object_id == idle.to_string()),
            "没投入的对象不参与分摊"
        );

        // MACHINE_HOURS：基数 2 / 1 / 3（合计 6）→ 333.3333… 按最大余数法
        let group = preview
            .groups
            .iter()
            .find(|group| group.account_code == machine_account)
            .expect("机器工时组");
        assert_eq!(group.basis_total, "6.0000");
        assert_eq!(row_of(group, object_b).basis_value, "1.0000");
        assert_eq!(group.alloc_total, "1000.0000");
        assert_eq!(
            group.rows.iter().filter(|row| row.rounding_adj != "0.0000").count(),
            1,
            "尾差只落一行：{:?}",
            group.rows
        );

        // OUTPUT_QTY：基数 60 / 20 / 20（合计 100）→ 600 / 200 / 200
        let group = preview
            .groups
            .iter()
            .find(|group| group.account_code == qty_account)
            .expect("完工数量组");
        assert_eq!(group.basis_total, "100.0000");
        assert_eq!(row_of(group, object_a).alloc_amount, "600.0000");
        assert_eq!(row_of(group, object_b).alloc_amount, "200.0000");
        assert_eq!(row_of(group, object_c).alloc_amount, "200.0000");

        // MATERIAL_COST：基数 1000 / 2000 / 3000（合计 6000）→ 166.6667 / 333.3333 / 500
        let group = preview
            .groups
            .iter()
            .find(|group| group.account_code == material_account)
            .expect("材料成本组");
        assert_eq!(group.basis_total, "6000.0000");
        assert_eq!(row_of(group, object_a).alloc_amount, "166.6667");
        assert_eq!(row_of(group, object_b).alloc_amount, "333.3333");
        assert_eq!(row_of(group, object_c).alloc_amount, "500.0000");
        assert_eq!(group.alloc_total, "1000.0000");

        fx.cleanup().await;
    }

    /// ③ 最大余数法：池 1000、基数 1:1:1 → 333.3334 / 333.3333 / 333.3333（§4.4.4 算例 B2 的库版）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_rounds_remainder_onto_one_row() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let period_code = fx.period_code();
        let period = period_code.as_str();
        let account = fx.account("05").await;
        let (object_a, _) = fx.object("OPEN").await;
        let (object_b, _) = fx.object("OPEN").await;
        let (object_c, _) = fx.object("OPEN").await;
        // 三个对象各 1 小时，池 1000（除不尽）
        fx.labor(object_a, "1", "0", period).await;
        fx.labor(object_b, "1", "0", period).await;
        fx.labor(object_c, "1", "0", period).await;
        fx.pool(&account, period, "1000").await;

        let rule = handler::create_rule(None, fx.rule_req("R", &account, "LABOR_HOURS", &fx.date_of(1)))
            .await
            .expect("建规则");
        Fixture::remember(&fx.rules, &rule.id);

        let preview = handler::preview_allocations(preview_req(period, vec![Uuid::parse_str(&rule.id).expect("id")]))
            .await
            .expect("试算");
        let group = preview
            .groups
            .iter()
            .find(|group| group.rule_code == rule.rule_code)
            .expect("规则组");
        let mut amounts = group
            .rows
            .iter()
            .map(|row| row.alloc_amount.clone())
            .collect::<Vec<_>>();
        amounts.sort();
        assert_eq!(amounts, ["333.3333", "333.3333", "333.3334"]);
        assert_eq!(group.alloc_total, "1000.0000", "Σ alloc = pool");
        assert_eq!(group.rounding_total, "0.0001", "尾差合计 = 0.0001");

        fx.cleanup().await;
    }

    /// ④ 池 > 0 但无在产对象 / 基数合计 0 → 两条硬错误（设计 §4.4.3 不许静默留池）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_refuses_pool_without_object_or_basis() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let period_code = fx.period_code();
        let period = period_code.as_str();

        // (a) 有池、没对象
        let account = fx.account("06").await;
        fx.pool(&account, period, "4000").await;
        let rule = handler::create_rule(None, fx.rule_req("N", &account, "LABOR_HOURS", &fx.date_of(1)))
            .await
            .expect("建规则");
        Fixture::remember(&fx.rules, &rule.id);
        let error = handler::preview_allocations(preview_req(period, vec![Uuid::parse_str(&rule.id).expect("id")]))
            .await
            .expect_err("无在产对象");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_ALLOC_NO_OBJECT);
        assert!(error.msg.contains("4000"), "{}", error.msg);

        // (b) 有对象、有工时，但规则按机器工时摊而机器工时全是 0
        let machine_account = fx.account("07").await;
        let (object_a, object_no) = fx.object("OPEN").await;
        fx.labor(object_a, "5", "0", period).await;
        fx.pool(&machine_account, period, "800").await;
        let machine_rule = handler::create_rule(
            None,
            fx.rule_req("Z", &machine_account, "MACHINE_HOURS", &fx.date_of(1)),
        )
        .await
        .expect("建机器工时规则");
        Fixture::remember(&fx.rules, &machine_rule.id);
        let error = handler::preview_allocations(CostAllocPreviewReq {
            period_code: Some(period.to_string()),
            rule_ids: Some(vec![machine_rule.id.clone()]),
        })
        .await
        .expect_err("机器工时基数全 0");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, crate::shared::error_code::ERP_COST_ALLOC_BASIS_ZERO);
        assert!(error.msg.contains("机器工时"), "{}", error.msg);
        assert!(
            error.msg.contains(&object_no) || error.msg.contains("基数合计为 0"),
            "{}",
            error.msg
        );

        fx.cleanup().await;
    }

    /// ⑤ 已产生分摊结果的规则：改口径 / 删除 → 409；改名字 → 放行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn referenced_rule_cannot_be_edited_or_deleted() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let period_code = fx.period_code();
        let period = period_code.as_str();
        let account = fx.account("08").await;
        let (object_id, object_no) = fx.object("OPEN").await;
        let rule = handler::create_rule(None, fx.rule_req("U", &account, "LABOR_HOURS", &fx.date_of(1)))
            .await
            .expect("建规则");
        Fixture::remember(&fx.rules, &rule.id);
        let rule_id = Uuid::parse_str(&rule.id).expect("id");

        // 造一条分摊结果（模拟 calc 第 5 段跑过）
        db().exec_sql(
            "INSERT INTO biz_cost_alloc_result (id, period_code, rule_id, cost_object_id, dept_id, \
                                                dept_name, account_code, basis_value, basis_total, \
                                                pool_amount, alloc_amount, rounding_adj, created_at) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, '分摊夹具车间', ?, 1, 1, 100, 100, 0, now())"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(period.to_string()),
                Value::from(&rule_id),
                Value::from(&object_id),
                Value::from(&fx.dept),
                Value::Text(account.clone()),
            ],
        )
        .await
        .expect("造分摊结果夹具");

        let delete_error = handler::delete_rule(None, &rule.id).await.expect_err("有结果不许删");
        assert_eq!(delete_error.status, 409, "{}", delete_error.msg);
        assert!(delete_error.msg.contains(period), "{}", delete_error.msg);

        let edit_error = handler::update_rule(
            None,
            &rule.id,
            CostAllocRuleUpdateReq {
                basis: Some("MACHINE_HOURS".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("有结果不许改口径");
        assert_eq!(edit_error.status, 409, "{}", edit_error.msg);
        assert!(edit_error.msg.contains("新建一条规则"), "{}", edit_error.msg);

        // 改名 / 停用仍允许（不影响历史结果）
        let renamed = handler::update_rule(
            None,
            &rule.id,
            CostAllocRuleUpdateReq {
                rule_name: Some("改名（历史结果不变）".to_string()),
                status: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("改名");
        assert_eq!(renamed.rule_name, "改名（历史结果不变）");

        // 删掉结果后可以删规则
        db().exec_sql(
            "DELETE FROM biz_cost_alloc_result WHERE rule_id = ?::uuid".to_string(),
            vec![Value::from(&rule_id)],
        )
        .await
        .expect("删结果");
        handler::delete_rule(None, &rule.id).await.expect("删规则");
        assert!(!object_no.is_empty());

        fx.cleanup().await;
    }

    /// ⑥ 来源单据被删的池行 → `unallocated` 诊断（不静默吞掉）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unassigned_pool_is_reported_not_swallowed() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let period_code = fx.period_code();
        let period = period_code.as_str();

        // 一条「来源台账已不存在」的归集行（source_id 指向随机 uuid）
        db().exec_sql(
            "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                           source_id, source_no, line_no, occur_date, amount, dept_id, \
                                           dept_name, remark, created_at, updated_at) \
             VALUES (?::uuid, NULL, ?, 'OVERHEAD', 'OVERHEAD_TICKET', ?::uuid, 'OH25-GHOST', 0, \
                     ?::text::date, 777.0000, ?::uuid, '分摊夹具车间', '来源已删', now(), now())"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(period.to_string()),
                Value::from(&Uuid::now_v7()),
                Value::Text(format!("{period}-28")),
                Value::from(&fx.dept),
            ],
        )
        .await
        .expect("造幽灵池行");

        // 显式指定一条本夹具自己的规则（池在别的科目上），让试算只走确定路径；
        // 幽灵池行仍然会出现在 `unallocated` 诊断里。
        let account = fx.account("09").await;
        let rule = handler::create_rule(None, fx.rule_req("G", &account, "LABOR_HOURS", &fx.date_of(1)))
            .await
            .expect("建规则");
        Fixture::remember(&fx.rules, &rule.id);

        let preview = handler::preview_allocations(preview_req(period, vec![Uuid::parse_str(&rule.id).expect("id")]))
            .await
            .expect("试算");
        let ghost = preview
            .unallocated
            .iter()
            .find(|row| row.pool_amount == "777.0000")
            .expect("幽灵池行必须出现在诊断里");
        assert!(ghost.account_code.is_none(), "科目未知");
        assert!(ghost.reason.contains("来源单据已被删除"), "{}", ghost.reason);

        // 清掉这条池行（没有登记在 fixtures 里，按 id 删不了 → 用 source_no 定位）
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_cost_collect WHERE source_no = 'OH25-GHOST' AND dept_id = ?::uuid".to_string(),
                vec![Value::from(&fx.dept)],
            )
            .await;
        fx.cleanup().await;
    }

    /// 静态不变量：模型层 SQL 的「池来源 → 科目反查」三路齐备（写漏一路 = 某科目池永远 0）。
    #[test]
    fn pool_sources_are_all_three() {
        assert_eq!(model::POOL_SOURCES.len(), 3);
        assert_eq!(model::PARTICIPANT_STATUSES, ["OPEN", "CLOSED"]);
        assert_eq!(dto::BASES.len(), 4);
    }
}
