//! calc **编排**（`POST /cost-period/calc`）的真库用例（`ERP_DB_URL` 开关，COST-B2-15）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<演练库> \
//!   cargo test --lib cost_calc_period_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面「验收」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`first_call_creates_draft_and_lands_all_six_stages`] | 验收① ②：无批次 ⇒ 建 `DRAFT` + 六段落库 + 出参含 `items[]`，金额 = 设计 §4.6.2 算例 A |
//! | 2 | [`second_call_recalcs_in_place_without_doubling`] | 验收③：重复调用幂等（同一批次、行数不变、金额不变） |
//! | 3 | [`non_draft_status_is_rejected_with_the_next_step`] | 验收④：`SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED` ⇒ 409 + `66073`，文案点名撤回 / 红冲 / 反结账 |
//! | 4 | [`reversed_batch_can_be_recalculated`] | 验收⑤：红冲后（唯一键不占位）同期间再算 ⇒ 200 + 新 `DRAFT` 批次，金额与第一轮一致 |
//! | 5 | [`missing_progress_is_a_locatable_error_and_leaves_no_draft`] | 验收⑥：缺进度 ⇒ 409 + `66067` 点名对象，且**连批次都不留**（事务回滚 / 不留死单） |
//! | 6 | [`cancelled_batch_does_not_block_a_new_round`] | `CANCELLED` 不占位 ⇒ 再算建新 `DRAFT`（作废后能重来） |
//!
//! # 夹具纪律
//!
//! 全部用 `CB215-*` / `CS215-*` 前缀；期间年份按进程内计数器分配（2500+），与别的成本卡错开。
//! 归集行直接插 `source_type='MANUAL'` + `source_id IS NULL`：这样料 / 工 / 费三段自己的
//! 「取差收敛」看不到它们（它们只认 STOCK_OUT / LABOR_TICKET / 折旧·报销·手工费用这几类
//! 来源行），而计算单段**按 element 汇总**照样看得到 —— 正好把「编排串起来」这件事隔离出来测，
//! 不重复验上游归集口径（那是 B2-6/7/8 的事）。
//!
//! 清场按登记 id 删，顺序按 FK：计算单 → 批次 → 归集 → 成本对象 → 物料 / 仓库 → 状态日志。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost_calc_period;
    use crate::shared::error_code;
    use tokio::sync::OnceCell;

    /// 连库（进程内只初始化一次）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
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

    /// 唯一后缀（纳秒 + 进程内计数器）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:03}", now % 1_000_000_000)
    }

    /// 期间年份：2500 + 进程内序号（每个夹具独占一个会计年度）。
    fn next_year() -> i32 {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        2500 + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 400).unwrap_or(0)
    }

    /// 编码尾巴（物料 / 仓库编码有长度上限）。
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

    struct Fixture {
        suffix: String,
        year: i32,
        dept: Uuid,
        product: Uuid,
        warehouse: Uuid,
        objects: std::sync::Mutex<Vec<Uuid>>,
        batches: std::sync::Mutex<Vec<Uuid>>,
        collects: std::sync::Mutex<Vec<Uuid>>,
    }

    /// 清掉**上轮崩溃**留下的夹具（`CB215-*` / `CS215-*` 前缀 + calc 自动建的批次）。
    ///
    /// calc 自己建批次走的是真实取号（`CS-yyyyMM-NNNN`），前缀不是 `CS215-`，所以不能只按
    /// batch_no 扫：先把「挂在本卡夹具对象上的计算单所属批次」找出来一起删（明细随批次
    /// `ON DELETE CASCADE` 走），否则 `biz_cost_object` 被 RESTRICT 挡住删不掉。
    async fn sweep_leftovers() {
        let statements = [
            // ① 状态日志（先删，避免指向已删批次）
            "DELETE FROM biz_status_log WHERE entity_type = 'COST_PERIOD' AND entity_id IN (\
                SELECT id FROM biz_cost_period WHERE batch_no LIKE 'CS215-%' \
                 OR id IN (SELECT period_id FROM biz_cost_period_item WHERE cost_object_id IN \
                            (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB215-%')))",
            // ② 批次（明细 CASCADE）：本卡夹具批次 + 挂在夹具对象上的批次
            "DELETE FROM biz_cost_period WHERE batch_no LIKE 'CS215-%'",
            "DELETE FROM biz_cost_period WHERE id IN (\
                SELECT period_id FROM biz_cost_period_item WHERE cost_object_id IN \
                   (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB215-%'))",
            // ③ 归集 / 分摊 / 对象 / 主数据
            "DELETE FROM biz_cost_alloc_result WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB215-%')",
            "DELETE FROM biz_cost_collect WHERE cost_object_id IN \
                (SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB215-%')",
            "DELETE FROM biz_cost_object WHERE object_no LIKE 'CB215-%'",
            "DELETE FROM biz_material WHERE material_code LIKE 'CB215-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'CB215W%'",
        ];
        for sql in statements {
            if let Err(error) = db().exec_sql(sql.to_string(), vec![]).await {
                println!("sweep 失败（{sql}）：{error}");
            }
        }
    }

    impl Fixture {
        async fn build() -> Self {
            sweep_leftovers().await;
            let suffix = uniq();
            let year = next_year();
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
                    Value::Text(format!("CB215-{suffix}")),
                    Value::Text(format!("编排夹具产出品 {suffix}")),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("造产出品夹具");
            let warehouse = Uuid::now_v7();
            let code = format!("CB215W{}", tail_code(&suffix));
            db().exec_sql(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                            is_leaf, level, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text(format!("编排夹具仓 {suffix}")),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("造仓库夹具");
            Self {
                suffix,
                year,
                dept,
                product,
                warehouse,
                objects: std::sync::Mutex::new(Vec::new()),
                batches: std::sync::Mutex::new(Vec::new()),
                collects: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// 期间（`-11` 是本期）。
        fn period_code(&self) -> String {
            format!("{}-11", self.year)
        }

        /// 造一个成本对象（算例 A 的产量形态：完工 800 / 在制 200 / 进度 50%）。
        async fn object(&self, completed: &str, wip: &str, pct: &str) -> (Uuid, String) {
            let id = Uuid::now_v7();
            let object_no = format!("CB215-{}-{}", &id.to_string()[28..], &self.suffix[..6]);
            db().exec_sql(
                "INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty, \
                                              completed_qty, wip_qty, completion_pct, material_input_mode, \
                                              workshop_dept_id, workshop_dept_name, start_date, status, \
                                              created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 2000, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, 'ONE_TIME', ?::uuid, '编排夹具车间', ?::text::date, 'OPEN', \
                         now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(object_no.clone()),
                    Value::from(&self.product),
                    Value::from(&self.warehouse),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::Text(pct.to_string()),
                    Value::from(&self.dept),
                    Value::Text(format!("{}-01-01", self.year)),
                ],
            )
            .await
            .expect("造成本对象夹具");
            self.objects.lock().expect("对象清单").push(id);
            (id, object_no)
        }

        /// 插一条归集行（`source_type='MANUAL'` + `source_id` 空：各段自己的取差看不到它）。
        async fn collect(&self, object_id: Uuid, element: &str, amount: &str) -> Uuid {
            let id = Uuid::now_v7();
            let period_code = self.period_code();
            // 料行必须带物料 + 数量（`ck_biz_cost_collect_mat`），工 / 费行必须不带。
            let (material_id, qty, unit_price) = if element == "MATERIAL" {
                ("?::uuid".to_string(), "1000".to_string(), "19.000000".to_string())
            } else {
                ("NULL".to_string(), "NULL".to_string(), "NULL".to_string())
            };
            let mut params = vec![
                Value::from(&id),
                Value::from(&object_id),
                Value::Text(period_code.clone()),
                Value::Text(element.to_string()),
                Value::Text("MANUAL".to_string()),
                Value::Text(format!("{period_code}-15")),
            ];
            if element == "MATERIAL" {
                params.push(Value::from(&self.product));
            }
            params.push(Value::Text(amount.to_string()));
            params.push(Value::from(&self.dept));
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_collect (id, cost_object_id, period_code, element, source_type, \
                                                   source_id, source_no, line_no, occur_date, material_id, qty, \
                                                   unit_price, amount, dept_id, dept_name, remark, created_at, updated_at) \
                     VALUES (?::uuid, ?::uuid, ?, ?, ?, NULL, 'B215-夹具', 0, ?::text::date, {material_id}, \
                             {qty}, {unit_price}, ?::text::numeric, ?::uuid, '编排夹具车间', 'B215-夹具', now(), now())"
                ),
                params,
            )
            .await
            .expect("造归集夹具");
            self.collects.lock().expect("归集清单").push(id);
            id
        }

        /// 造一个批次（指定期间 + 状态）。四道状态关联 CHECK 得一起满足（V0373 的库层约束）。
        async fn batch(&self, period_code: &str, status: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period_code.to_string()),
                    Value::Text(format!("CS215-{}-{}", period_code.replace('-', ""), &self.suffix[..4])),
                ],
            )
            .await
            .expect("造成本批次夹具");
            self.batches.lock().expect("批次清单").push(id);

            let needs_approval = matches!(status, "SUBMITTED" | "APPROVED" | "SETTLED" | "CLOSED" | "REVERSED");
            let needs_settled = matches!(status, "SETTLED" | "CLOSED" | "REVERSED");
            let mut sets = vec!["status = ?".to_string()];
            let mut params: Vec<Value> = vec![Value::Text(status.to_string())];
            if needs_approval {
                sets.push("approval_instance_id = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
            }
            if needs_settled {
                sets.push("settled_at = now()".to_string());
                sets.push("settled_by = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
                sets.push("finish_voucher_id = ?::uuid".to_string());
                params.push(Value::from(&Uuid::now_v7()));
            }
            if status == "REVERSED" {
                sets.push("reversed_at = now()".to_string());
                sets.push("reverse_reason = '夹具红冲'".to_string());
            }
            if status == "CANCELLED" {
                // `ck_biz_cost_period_cancel`：说作废就得有原因，同一条 UPDATE 里给
                sets.push("cancel_reason = '夹具作废'".to_string());
            }
            if status != "DRAFT" {
                params.push(Value::from(&id));
                db().exec_sql(
                    format!("UPDATE biz_cost_period SET {} WHERE id = ?::uuid", sets.join(", ")),
                    params,
                )
                .await
                .expect("改批次状态");
            }
            id
        }

        /// 读回期间内**活跃批次**的状态。
        ///
        /// 造完夹具必须用它确认「那一行真的变成我要的状态」——否则 CHECK 约束把 UPDATE 挡掉、
        /// 行还停在 `DRAFT`，用例会把「calc 在 DRAFT 上重算」误判成「calc 放行了 PENDING」。
        async fn status_of(&self, period_code: &str) -> String {
            db().scalar_with_params(
                "SELECT status FROM biz_cost_period WHERE period_code = ? AND deleted_at IS NULL",
                vec![Value::Text(period_code.to_string())],
            )
            .await
            .expect("读回批次状态")
        }

        /// 硬插第二张活跃批次（`DRAFT`），返回 DB 结果 —— 用来验证部分唯一索引的「活跃」定义。
        ///
        /// 不复用 [`Fixture::batch`]：这里要的就是**失败**，不能 `expect` 掉。
        async fn try_insert_second_active(&self, period_code: &str) -> DbResult<u64> {
            db().exec_sql(
                "INSERT INTO biz_cost_period (id, period_code, batch_no, status, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, 'DRAFT', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::Text(period_code.to_string()),
                    Value::Text(format!(
                        "CS215-DUP-{}-{}",
                        period_code.replace('-', ""),
                        &self.suffix[..4]
                    )),
                ],
            )
            .await
        }

        /// 期间内的活跃批次（`REVERSED` / `CANCELLED` 不算）。
        async fn active_batches(&self) -> Vec<(String, String)> {
            let rows = db()
                .query_sql(
                    "SELECT batch_no, status FROM biz_cost_period \
                      WHERE period_code = ? AND deleted_at IS NULL \
                        AND status NOT IN ('REVERSED','CANCELLED')"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await
                .expect("读活跃批次");
            rows.iter()
                .map(|row| Ok((row.get_typed::<String>("batch_no")?, row.get_typed::<String>("status")?)))
                .collect::<DbResult<Vec<_>>>()
                .expect("解析活跃批次")
        }

        /// 本期计算单（原始 SQL，字符串形态：不用被测的读函数验被测的写函数）。
        async fn items(&self) -> Vec<Vec<String>> {
            let rows = db()
                .query_sql(
                    "SELECT i.status, i.completed_qty::text, i.wip_end_qty::text, \
                            i.finished_cost::text, i.wip_end_cost::text, i.mat_cur::text, \
                            i.labor_cur::text, i.oh_cur::text \
                       FROM biz_cost_period_item i \
                       JOIN biz_cost_period p ON p.id = i.period_id \
                      WHERE p.period_code = ? ORDER BY i.finished_cost DESC"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await
                .expect("读计算单");
            rows.iter()
                .map(|row| {
                    Ok(vec![
                        row.get_typed::<String>("status")?,
                        row.get_typed::<String>("completed_qty")?,
                        row.get_typed::<String>("wip_end_qty")?,
                        row.get_typed::<String>("finished_cost")?,
                        row.get_typed::<String>("wip_end_cost")?,
                        row.get_typed::<String>("mat_cur")?,
                        row.get_typed::<String>("labor_cur")?,
                        row.get_typed::<String>("oh_cur")?,
                    ])
                })
                .collect::<DbResult<Vec<_>>>()
                .expect("解析计算单")
        }

        /// 清场（按登记 id；明细 / 状态日志先删）。
        ///
        /// 每类 id 都先 `drain` 到局部 `Vec` 再 `await`：`MutexGuard` 不能跨 await 持有
        /// （clippy `await_holding_lock`；真跨了会把整个测试并发拖成串行）。
        async fn cleanup(&self) {
            let collects: Vec<Uuid> = self.collects.lock().expect("归集清单").drain(..).collect();
            for id in collects {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_collect WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&id)],
                    )
                    .await;
            }
            let batches: Vec<Uuid> = self.batches.lock().expect("批次清单").drain(..).collect();
            for id in batches {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_period_item WHERE period_id = ?::uuid".to_string(),
                        vec![Value::from(&id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_status_log WHERE entity_type = 'COST_PERIOD' AND entity_id = ?::uuid"
                            .to_string(),
                        vec![Value::from(&id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_period WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&id)],
                    )
                    .await;
            }
            // calc 自己建的批次（不在登记表里）
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period_item WHERE period_id IN \
                        (SELECT id FROM biz_cost_period WHERE period_code = ?)"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_status_log WHERE entity_type = 'COST_PERIOD' AND entity_id IN \
                        (SELECT id FROM biz_cost_period WHERE period_code = ?)"
                        .to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await;
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_period WHERE period_code = ?".to_string(),
                    vec![Value::Text(self.period_code())],
                )
                .await;
            let objects: Vec<Uuid> = self.objects.lock().expect("对象清单").drain(..).collect();
            for id in objects {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_object WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&id)],
                    )
                    .await;
            }
            for (table, column) in [("biz_material", "id"), ("biz_warehouse", "id")] {
                let id = if table == "biz_material" {
                    self.product
                } else {
                    self.warehouse
                };
                let _ = db()
                    .exec_sql(
                        format!("DELETE FROM {table} WHERE {column} = ?::uuid"),
                        vec![Value::from(&id)],
                    )
                    .await;
            }
        }
    }

    /// 验收① ②：首次调用建 `DRAFT` + 六段落库，金额 = 设计 §4.6.2 算例 A。
    #[tokio::test]
    async fn first_call_creates_draft_and_lands_all_six_stages() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, object_no) = fx.object("800", "200", "50").await;
        fx.collect(object_id, "MATERIAL", "19000").await;
        fx.collect(object_id, "LABOR", "4000").await;
        fx.collect(object_id, "OVERHEAD", "4000").await;

        let res = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("首次 calc 必须成功");

        // ---------- 出参 ----------
        assert!(res.calc.batch_created, "首次调用必须建批次");
        assert_eq!(res.detail.status, "DRAFT", "算完仍是草稿（等提交审批）");
        assert_eq!(res.detail.period_code, fx.period_code());
        assert_eq!(res.detail.batch_no, res.calc.batch_no, "详情与摘要同一张批次");
        assert!(
            res.detail.batch_no.starts_with("CS-"),
            "批次号 CS-yyyyMM-NNNN：{}",
            res.detail.batch_no
        );
        assert_eq!(res.detail.items.len(), 1, "一个对象一行计算单");
        assert_eq!(
            res.detail.items[0].object_no.as_deref(),
            Some(object_no.as_str()),
            "items 要带对象号（防前端显示 uuid）"
        );
        assert_eq!(res.calc.items.rows, 1);
        assert_eq!(res.calc.items.skipped, 0);

        // ---------- 六段都真的跑了（各段自己的回查口径） ----------
        //
        // 段摘要里的 `amount` 是**该段自己**的回查口径（料只看 `STOCK_OUT` 来源、工只看
        // `LABOR_TICKET`、费只看折旧/报销/手工三路），而本夹具的归集行是 `MANUAL`（故意
        // 让各段的「取差收敛」碰不到它，见文件头）。所以这里断言「段跑了、没乱动别人的行」，
        // 金额以计算单的 `items.total_*`（按 element 汇总）为准。
        assert_eq!(res.calc.material.inserted, 0, "夹具行不是 STOCK_OUT ⇒ 料段无新增");
        assert_eq!(res.calc.material.purged, 0, "料段不许删别人的行");
        assert_eq!(res.calc.labor.purged, 0, "工段不许删别人的行");
        assert_eq!(res.calc.overhead.purged, 0, "费段不许删别人的行");
        assert_eq!(res.calc.reconcile.diff, "0.0000", "池空 / 账面空 ⇒ 对平");
        assert_eq!(res.calc.alloc.amount, "0.0000", "池 0 ⇒ 无可摊（跳过，不写 0 元行）");

        // ---------- 死数字（设计 §4.6.2 算例 A） ----------
        assert_eq!(res.calc.items.total_material, "19000.0000");
        assert_eq!(res.calc.items.total_labor, "4000.0000");
        assert_eq!(res.calc.items.total_overhead, "4000.0000");
        assert_eq!(res.calc.items.total_cost, "27000.0000");
        assert_eq!(res.calc.items.total_finished, "22311.1104");
        assert_eq!(res.calc.items.total_wip, "4688.8896");

        // ---------- 库里真的落了（不信内存） ----------
        let stored = fx.items().await;
        assert_eq!(stored.len(), 1, "库里 1 行计算单");
        let row = &stored[0];
        assert_eq!(row[0], "CALC", "计算单状态 CALC（结算才置 SETTLED）");
        assert_eq!(row[1], "800.0000", "完工数量");
        assert_eq!(row[2], "200.0000", "期末在制数量");
        assert_eq!(row[3], "22311.1104", "完工转出");
        assert_eq!(row[4], "4688.8896", "期末在制成本");
        assert_eq!(row[5], "19000.0000", "料投入");
        assert_eq!(row[6], "4000.0000", "工投入");
        assert_eq!(row[7], "4000.0000", "费投入");

        let active = fx.active_batches().await;
        assert_eq!(active.len(), 1, "同期间只该有 1 张活跃批次");
        assert_eq!(active[0].1, "DRAFT");

        fx.cleanup().await;
    }

    /// 验收③：重复调用原地重算 —— 同一批次、行数不变、金额不翻倍。
    #[tokio::test]
    async fn second_call_recalcs_in_place_without_doubling() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _) = fx.object("800", "200", "50").await;
        fx.collect(object_id, "MATERIAL", "19000").await;
        fx.collect(object_id, "LABOR", "4000").await;
        fx.collect(object_id, "OVERHEAD", "4000").await;

        let first = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("第一次 calc");
        let second = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("第二次 calc");

        assert!(first.calc.batch_created);
        assert!(!second.calc.batch_created, "已有 DRAFT ⇒ 原地重算，不另建批次");
        assert_eq!(second.calc.batch_no, first.calc.batch_no, "同一张批次");
        assert_eq!(second.calc.items.purged, 1, "重算前清掉 1 行旧计算单");
        assert_eq!(second.detail.items.len(), 1, "行数不变（不是累加）");
        assert_eq!(second.calc.items.total_finished, "22311.1104", "金额不变");
        assert_eq!(second.calc.items.total_wip, "4688.8896", "金额不变");

        assert_eq!(fx.active_batches().await.len(), 1, "仍然只有 1 张活跃批次");
        assert_eq!(fx.items().await.len(), 1, "库里也只有 1 行");

        fx.cleanup().await;
    }

    /// 验收④：已提交 / 已审批 / 已结算 / 已关期 ⇒ 409 + `66073`，文案点名下一步。
    #[tokio::test]
    async fn non_draft_status_is_rejected_with_the_next_step() {
        if !ensure_db().await {
            return;
        }
        for (status, hint) in [
            ("SUBMITTED", "撤回"),
            ("APPROVED", "审批"),
            ("SETTLED", "红冲"),
            ("CLOSED", "反结账"),
        ] {
            let fx = Fixture::build().await;
            let (object_id, _) = fx.object("800", "200", "50").await;
            fx.collect(object_id, "MATERIAL", "19000").await;
            fx.batch(&fx.period_code(), status).await;

            let error = cost_calc_period::calc_period(Some(&fx.period_code()), None)
                .await
                .expect_err("非 DRAFT 必须拒绝");
            assert_eq!(error.status, 409, "{status} 应 409：{}", error.msg);
            assert_eq!(error.code, error_code::ERP_COST_PERIOD_STATUS_GUARD, "{status}");
            assert!(error.msg.contains(status), "{status} 报文要点名当前状态：{}", error.msg);
            assert!(error.msg.contains(hint), "{status} 报文要指向「{hint}」：{}", error.msg);

            // 拒绝得干净：没有偷偷建第二张批次，计算单也没写
            assert_eq!(fx.active_batches().await.len(), 1, "{status}：不该多出批次");
            assert!(fx.items().await.is_empty(), "{status}：一行计算单都不该写");

            fx.cleanup().await;
        }
    }

    /// 验收⑤：红冲后（唯一键不占位）同期间再算 ⇒ 200 + 新 `DRAFT` 批次。
    #[tokio::test]
    async fn reversed_batch_can_be_recalculated() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _) = fx.object("800", "200", "50").await;
        fx.collect(object_id, "MATERIAL", "19000").await;
        fx.collect(object_id, "LABOR", "4000").await;
        fx.collect(object_id, "OVERHEAD", "4000").await;

        let first = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("第一轮 calc");
        let first_batch = first.calc.batch_no.clone();

        // 把这一批打成 REVERSED（红冲后的真实形态：唯一键不占位）
        db().exec_sql(
            "UPDATE biz_cost_period SET status = 'REVERSED', reversed_at = now(), \
                    reverse_reason = '夹具红冲', approval_instance_id = ?::uuid, \
                    settled_at = now(), settled_by = ?::uuid, finish_voucher_id = ?::uuid \
              WHERE batch_no = ?"
                .to_string(),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&Uuid::now_v7()),
                Value::from(&Uuid::now_v7()),
                Value::Text(first_batch.clone()),
            ],
        )
        .await
        .expect("红冲第一轮批次");
        // 红冲会把计算单打回 CALC 并保留（设计 §4.8 反向路径）
        db().exec_sql(
            "UPDATE biz_cost_period_item SET status = 'CALC' WHERE period_id IN \
                (SELECT id FROM biz_cost_period WHERE batch_no = ?)"
                .to_string(),
            vec![Value::Text(first_batch.clone())],
        )
        .await
        .expect("计算单回 CALC");

        let second = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("红冲后重算必须 200");
        assert!(second.calc.batch_created, "红冲后的批次不占位 ⇒ 建新 DRAFT");
        assert_ne!(second.calc.batch_no, first_batch, "新批次号必须不同于被红冲的那一批");
        assert_eq!(second.calc.items.total_finished, "22311.1104", "重算金额与红冲前一致");
        assert_eq!(second.detail.status, "DRAFT");
        assert_eq!(
            fx.active_batches().await.len(),
            1,
            "活跃批次仍是 1 张（另一张是 REVERSED）"
        );

        fx.cleanup().await;
    }

    /// 验收⑥：缺数据 ⇒ 可定位 409（点名对象）+ **不留死单**（连批次都不留）。
    #[tokio::test]
    async fn missing_progress_is_a_locatable_error_and_leaves_no_draft() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        // 期末在制 200 但进度 0 ⇒ 第 6 段 66067（与 B2-11 同一条判据）
        let (object_id, object_no) = fx.object("800", "200", "0").await;
        fx.collect(object_id, "MATERIAL", "19000").await;

        let error = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect_err("缺进度必须拒绝");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_COST_OUTPUT_QTY_INVALID);
        assert!(error.msg.contains(&object_no), "报文要点名对象：{}", error.msg);
        assert!(error.msg.contains("第 6 段"), "报文要标出是哪一段失败：{}", error.msg);

        // 关键：整笔回滚 ⇒ 自动建的那张 DRAFT 也不在了（否则期间被唯一键占住、成了死单）
        assert!(
            fx.active_batches().await.is_empty(),
            "失败不许留「建了但没算」的空批次：{:?}",
            fx.active_batches().await
        );
        assert!(fx.items().await.is_empty(), "一行计算单都不该写");
        // 归集行（夹具 MANUAL 行）不受影响
        let kept: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE period_code = ? AND deleted_at IS NULL",
                vec![Value::Text(fx.period_code())],
            )
            .await
            .expect("数归集行");
        assert_eq!(kept, 1, "归集行不该被这次失败的 calc 带走");

        fx.cleanup().await;
    }

    /// `CANCELLED` 不占位 ⇒ 作废后再算能建新批次（用户不需要工程师改库）。
    #[tokio::test]
    async fn cancelled_batch_does_not_block_a_new_round() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let (object_id, _) = fx.object("800", "200", "50").await;
        fx.collect(object_id, "MATERIAL", "19000").await;
        let cancelled = fx.batch(&fx.period_code(), "CANCELLED").await;
        assert!(!cancelled.is_nil(), "作废批次夹具（原因已随状态一起写）");

        let res = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("作废后能重来");
        assert!(res.calc.batch_created, "作废的批次不占位 ⇒ 建新 DRAFT");
        assert_eq!(res.detail.status, "DRAFT");
        assert_eq!(res.calc.items.total_cost, "19000.0000", "只有料，工费为 0");

        fx.cleanup().await;
    }

    /// 验收⑤ + 「`PENDING` 算不算活跃」：**唯一索引与 calc 对「活跃」的判定必须一致**。
    ///
    /// 部分唯一索引 `uk_biz_cost_period` 的谓词是 `status NOT IN ('REVERSED','CANCELLED')`，
    /// 所以 `DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED` **都**占位；
    /// 只有 `REVERSED` / `CANCELLED` 不占位。逐状态实测：
    ///
    /// | 已有批次的状态 | 再插一张 `DRAFT` | calc |
    /// |---|---|---|
    /// | `DRAFT` ~ `CLOSED`（5 个活跃态） | 唯一键拒绝 | 见下一条用例（非 DRAFT ⇒ 409） |
    /// | `REVERSED` / `CANCELLED` | 允许（同期间可重建） | 200 + 新批次 |
    ///
    /// 这条用例同时守住「造完夹具先读回状态」：CHECK 约束把 UPDATE 挡掉时立刻红，
    /// 不会让「行还是 DRAFT」把后面的判定糊过去。
    #[tokio::test]
    async fn active_batch_definition_matches_the_unique_index() {
        if !ensure_db().await {
            return;
        }
        for status in crate::business::erp::cost_period_dto::STATUSES {
            let fx = Fixture::build().await;
            fx.batch(&fx.period_code(), status).await;
            assert_eq!(
                fx.status_of(&fx.period_code()).await,
                status,
                "{status}：夹具没写进库（CHECK 约束挡住了？），后面的判定全部无意义"
            );

            let live = !matches!(status, "REVERSED" | "CANCELLED");
            let inserted = fx.try_insert_second_active(&fx.period_code()).await;
            if live {
                assert!(
                    matches!(inserted, Err(DbError::UniqueViolation { .. })),
                    "{status} 是活跃批次 ⇒ 同期间插第二张 DRAFT 必须撞唯一键，实际：{inserted:?}"
                );
                assert_eq!(fx.active_batches().await.len(), 1, "{status}：活跃批次仍是 1 张");
            } else {
                assert!(
                    inserted.is_ok(),
                    "{status} 不占位 ⇒ 同期间可以插新 DRAFT，实际：{inserted:?}"
                );
                assert_eq!(
                    fx.active_batches().await.len(),
                    1,
                    "{status}：新插的那张 DRAFT 是唯一活跃批次"
                );
            }
            fx.cleanup().await;
        }
    }

    /// 验收①~④：`calc` 对**每一个**状态都显式表态（不允许任何一种状态默默当草稿重算）。
    ///
    /// - `DRAFT` ⇒ 200，原地重算（`batch_created = false`，批次数不变）
    /// - `SUBMITTED`（= 审批中，卡面误写成 `PENDING`）/ `APPROVED` / `SETTLED` / `CLOSED`
    ///   ⇒ 409 + `66073`，不建批次、不写计算单
    /// - `REVERSED` / `CANCELLED` ⇒ 200，新建批次（不占位）
    #[tokio::test]
    async fn calc_answers_every_status_explicitly() {
        use crate::business::erp::cost_period_dto::{self as period_dto, STATUSES};
        if !ensure_db().await {
            return;
        }
        for status in STATUSES {
            let fx = Fixture::build().await;
            fx.batch(&fx.period_code(), status).await;
            assert_eq!(
                fx.status_of(&fx.period_code()).await,
                status,
                "{status}：夹具没写进库，用例判定无效"
            );

            let outcome = cost_calc_period::calc_period(Some(&fx.period_code()), None).await;
            match status {
                period_dto::STATUS_DRAFT => {
                    let res = outcome.unwrap_or_else(|e| panic!("DRAFT 上重算必须 200：{e:?}"));
                    assert!(!res.calc.batch_created, "DRAFT ⇒ 原地重算，不另建批次");
                    assert_eq!(res.detail.status, period_dto::STATUS_DRAFT);
                    assert_eq!(fx.active_batches().await.len(), 1, "批次数不变");
                }
                period_dto::STATUS_REVERSED | period_dto::STATUS_CANCELLED => {
                    let res = outcome.unwrap_or_else(|e| panic!("{status} 不占位 ⇒ 必须能建新批次：{e:?}"));
                    assert!(res.calc.batch_created, "{status} ⇒ 新建 DRAFT");
                    assert_eq!(res.detail.status, period_dto::STATUS_DRAFT);
                    assert_eq!(fx.active_batches().await.len(), 1, "{status}：只有新建的那张是活跃的");
                }
                locked => {
                    let error = outcome.expect_err(&format!("{locked} 必须拒绝重算"));
                    assert_eq!(error.status, 409, "{locked}：{}", error.msg);
                    assert_eq!(error.code, crate::shared::error_code::ERP_COST_PERIOD_STATUS_GUARD);
                    assert!(error.msg.contains(locked), "{locked}：报文要点名状态：{}", error.msg);
                    assert_eq!(fx.active_batches().await.len(), 1, "{locked}：不许多出批次");
                    assert!(fx.items().await.is_empty(), "{locked}：一行计算单都不许写");
                }
            }
            fx.cleanup().await;
        }
    }

    /// 这一次误报的根因：`PENDING` **不是**本模块的状态，库层 CHECK 直接拒掉这条 UPDATE。
    ///
    /// 于是行仍是 `DRAFT`、`calc` 返回 200 是**正确**的（在草稿上原地重算）。
    /// 用真库把这层守住：任何人再造「PENDING 批次」当夹具，必须先过 `ck_biz_cost_period_status`。
    #[tokio::test]
    async fn status_outside_the_enum_cannot_be_written() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        fx.batch(&fx.period_code(), "DRAFT").await;

        let rejected = db()
            .exec_sql(
                "UPDATE biz_cost_period SET status = 'PENDING' WHERE period_code = ?".to_string(),
                vec![Value::Text(fx.period_code())],
            )
            .await;
        assert!(
            rejected.is_err(),
            "PENDING 不在 ck_biz_cost_period_status 值域里，UPDATE 必须被库层拒绝"
        );
        assert_eq!(
            fx.status_of(&fx.period_code()).await,
            "DRAFT",
            "被拒的 UPDATE 不能改到行：状态还是 DRAFT"
        );

        // 状态没变 ⇒ calc 在 DRAFT 上重算，200 是**正确行为**（不是放行 PENDING）
        let res = cost_calc_period::calc_period(Some(&fx.period_code()), None)
            .await
            .expect("DRAFT 上重算必须 200");
        assert!(!res.calc.batch_created, "原地重算");
        assert_eq!(fx.active_batches().await.len(), 1, "同期间仍只有 1 张活跃批次");

        fx.cleanup().await;
    }
}
