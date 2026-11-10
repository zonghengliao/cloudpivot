//! 批次主数据（`biz_lot`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_costb19 \
//!   cargo test --lib lot_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面三条 + 设计 §5.2 #7–#9 逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`create_page_update_and_options_flow`] | C（建）/ R（列表 + 下拉）/ U（部分更新）全通；`*_id` 都带 `*_name`；停用后从下拉消失 |
//! | 2 | [`duplicate_lot_no_on_same_material_is_friendly_409`] | 唯一键冲突 → 409 + `66056` + 人话文案（不含 PG 原文 / 约束名） |
//! | 3 | [`same_lot_no_allowed_on_another_material`] | 唯一键是 `(material_id, lot_no)`，**不是** `lot_no` 全局唯一 |
//! | 4 | [`update_into_existing_lot_no_is_friendly_409`] | 改单撞键同样 409（排除自己那一行不算冲突） |
//! | 5 | [`db_unique_index_still_catches_concurrent_insert`] | 绕过 handler 预检直插 ⇒ 库的 `UniqueViolation` 仍被翻译成 409 + `66056` |
//! | 6 | [`refs_and_dates_are_rejected_with_field_names`] | 物料 / 供应商不存在 → 400 指名；失效日期早于生产日期 → 400 |
//! | 7 | [`options_require_material_and_reject_unknown`] | 下拉接口物料必填（400）；物料不存在 → 400（不是空列表） |
//!
//! # 夹具纪律
//!
//! 全部用 `costb19-<唯一后缀>` 前缀造，清场**按 id** 删（绝不按前缀扫全库）。
//! 依赖的表由 V0137（`biz_material` / `biz_supplier`）+ V0370（`biz_lot`）建。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::lot_dto::{LotCreateReq, LotOptionQuery, LotPageQuery, LotUpdateReq};
    use crate::business::erp::lot_handler as handler;
    use crate::business::erp::lot_model as model;
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

    /// 夹具：两个物料 + 一个供应商（批次只关心「物料级唯一」，所以要两个物料）。
    struct Fixture {
        suffix: String,
        material_raw: Uuid,
        material_finished: Uuid,
        supplier: Uuid,
        /// 造出来的批次 id（清场用）。
        lots: Vec<Uuid>,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let unit: Uuid = db()
                .scalar_with_params("SELECT id FROM biz_unit WHERE deleted_at IS NULL LIMIT 1", vec![])
                .await
                .expect("取计量单位（biz_unit 至少要有 1 行）");
            let material_raw = Uuid::now_v7();
            let material_finished = Uuid::now_v7();
            for (id, code, _name, material_type) in [
                (material_raw, "R", "原料", "RAW"),
                (material_finished, "F", "成品", "FINISHED"),
            ] {
                db().exec_sql(
                    "INSERT INTO biz_material (id, material_code, material_name, unit_id, \
                                              material_type, status, serial_required, created_at, updated_at) \
                     VALUES (?::uuid, ?, ?, ?::uuid, ?, 1, false, now(), now())"
                        .to_string(),
                    vec![
                        Value::from(&id),
                        Value::Text(format!("COSTB19-M-{code}-{suffix}")),
                        Value::Text(format!("批次夹具{material_type} {suffix}")),
                        Value::from(&unit),
                        Value::Text(material_type.to_string()),
                    ],
                )
                .await
                .expect("插夹具物料");
            }
            let supplier = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?, 1, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&supplier),
                    Value::Text(format!("COSTB19-S-{suffix}")),
                    Value::Text(format!("批次夹具供应商 {suffix}")),
                ],
            )
            .await
            .expect("插夹具供应商");
            Self {
                suffix,
                material_raw,
                material_finished,
                supplier,
                lots: Vec::new(),
            }
        }

        /// 批号（带后缀，跨用例不撞）。
        fn lot_no(&self, tag: &str) -> String {
            format!("COSTB19-{tag}-{}", self.suffix)
        }

        /// 记下建出来的批次（清场用）。
        fn track(&mut self, id: &str) {
            self.lots.push(Uuid::parse_str(id).expect("批次 id"));
        }

        /// 按 id 清场（先批次、再供应商、再物料 —— 弱引用没有 FK，但顺序仍按引用链走）。
        async fn cleanup(&self) {
            for id in &self.lots {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_lot WHERE id = ?::uuid".to_string(),
                        vec![Value::from(id)],
                    )
                    .await;
            }
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_supplier WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&self.supplier)],
                )
                .await;
            for material in [self.material_raw, self.material_finished] {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_material WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&material)],
                    )
                    .await;
            }
        }
    }

    fn create_req(fixture: &Fixture, lot_no: &str, material: Uuid) -> LotCreateReq {
        LotCreateReq {
            lot_no: Some(lot_no.to_string()),
            material_id: Some(material.to_string()),
            source_type: Some("PRODUCTION".to_string()),
            source_doc_type: Some("COST_OBJECT".to_string()),
            source_doc_no: Some("CB-20261110-000001".to_string()),
            supplier_id: Some(fixture.supplier.to_string()),
            prod_date: Some("2026-11-01".to_string()),
            expire_date: Some("2027-11-01".to_string()),
            qc_status: Some("PASSED".to_string()),
            remark: Some("首件".to_string()),
            ..Default::default()
        }
    }

    // ==================== 1. C / R / U 全通 + 下拉 ====================

    #[tokio::test]
    async fn create_page_update_and_options_flow() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let lot_no = fixture.lot_no("FLOW");

        // ---- C：建批 ----
        let created = handler::create_lot(None, create_req(&fixture, &lot_no, fixture.material_finished))
            .await
            .expect("建批");
        fixture.track(&created.id);
        assert_eq!(created.lot_no, lot_no);
        assert_eq!(created.status, 1, "缺省启用");
        assert_eq!(created.qc_status, "PASSED");
        assert_eq!(created.source_type, "PRODUCTION");
        assert_eq!(created.source_doc_type.as_deref(), Some("COST_OBJECT"));
        assert_eq!(created.prod_date.as_deref(), Some("2026-11-01"));
        // 约定 13：*_id 必须同时给 *_name
        assert!(
            created
                .material_name
                .as_deref()
                .is_some_and(|name| name.contains(&fixture.suffix)),
            "物料名要 join 出来：{:?}",
            created.material_name
        );
        assert!(
            created
                .material_code
                .as_deref()
                .is_some_and(|code| code.starts_with("COSTB19-M-F")),
            "{:?}",
            created.material_code
        );
        assert!(created.supplier_name.is_some(), "供应商名要 join 出来");

        // ---- R：列表（按物料 + 关键字）----
        let page = handler::page_lots(LotPageQuery {
            keyword: Some(lot_no.clone()),
            material_id: Some(fixture.material_finished.to_string()),
            ..Default::default()
        })
        .await
        .expect("列表");
        assert_eq!(page.total, 1, "按批号关键字应命中 1 行");
        assert_eq!(page.list[0].id, created.id);

        // 关键字按物料编码也能搜到（列表 OR 三列）
        let page = handler::page_lots(LotPageQuery {
            keyword: Some(format!("COSTB19-M-F-{}", fixture.suffix)),
            ..Default::default()
        })
        .await
        .expect("按物料编码搜");
        assert!(page.list.iter().any(|row| row.id == created.id), "物料编码要能搜到");

        // ---- R：按物料取批号（下拉）----
        let options = handler::lot_options(LotOptionQuery {
            material_id: Some(fixture.material_finished.to_string()),
            keyword: None,
        })
        .await
        .expect("下拉");
        assert_eq!(options.len(), 1, "该物料下只有刚建的一个启用批");
        assert_eq!(options[0].lot_no, lot_no);
        assert_eq!(options[0].qc_status, "PASSED");

        // 下拉按关键字筛
        let options = handler::lot_options(LotOptionQuery {
            material_id: Some(fixture.material_finished.to_string()),
            keyword: Some("NOT-EXIST".to_string()),
        })
        .await
        .expect("下拉（无命中）");
        assert!(options.is_empty());

        // ---- U：部分更新（改批号 + 质检 + 清空失效日期 + 停用）----
        let updated = handler::update_lot(
            None,
            &created.id,
            LotUpdateReq {
                lot_no: Some(fixture.lot_no("FLOW2")),
                qc_status: Some("REJECTED".to_string()),
                expire_date: Some(None),
                status: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("改批");
        assert_eq!(updated.lot_no, fixture.lot_no("FLOW2"));
        assert_eq!(updated.qc_status, "REJECTED");
        assert_eq!(updated.expire_date, None, "显式 null 要清空");
        assert_eq!(updated.status, 0, "停用");
        assert_eq!(updated.prod_date.as_deref(), Some("2026-11-01"), "没提的字段不动");
        assert_eq!(updated.material_id, created.material_id, "物料不可改");

        // 停用后从下拉消失（下拉只要启用批）
        let options = handler::lot_options(LotOptionQuery {
            material_id: Some(fixture.material_finished.to_string()),
            keyword: None,
        })
        .await
        .expect("停用后下拉");
        assert!(options.is_empty(), "停用批不该出现在下拉里");

        // 但列表按 status=0 仍能查到（历史保留）
        let page = handler::page_lots(LotPageQuery {
            material_id: Some(fixture.material_finished.to_string()),
            status: Some(0),
            ..Default::default()
        })
        .await
        .expect("停用列表");
        assert_eq!(page.total, 1, "停用批仍在列表里");

        fixture.cleanup().await;
    }

    // ==================== 2. 唯一键冲突 → 友好 409 ====================

    #[tokio::test]
    async fn duplicate_lot_no_on_same_material_is_friendly_409() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let lot_no = fixture.lot_no("DUP");
        let first = handler::create_lot(None, create_req(&fixture, &lot_no, fixture.material_raw))
            .await
            .expect("第一次建批");
        fixture.track(&first.id);

        let error = handler::create_lot(None, create_req(&fixture, &lot_no, fixture.material_raw))
            .await
            .expect_err("同物料同批号必须被拦");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, 66056, "批号重复专用码");
        assert!(error.msg.contains(&lot_no), "文案要点名批号：{}", error.msg);
        assert!(error.msg.contains(&fixture.suffix), "文案要点名物料：{}", error.msg);
        // 不许把 PG 原文 / 约束名甩给用户
        for leak in ["duplicate key", "uk_biz_lot_material_no", "constraint"] {
            assert!(!error.msg.contains(leak), "文案泄漏了 {leak}：{}", error.msg);
        }

        fixture.cleanup().await;
    }

    // ==================== 3. 唯一键是 (material_id, lot_no) ====================

    #[tokio::test]
    async fn same_lot_no_allowed_on_another_material() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let lot_no = fixture.lot_no("SHARE");
        let first = handler::create_lot(None, create_req(&fixture, &lot_no, fixture.material_raw))
            .await
            .expect("原料批");
        fixture.track(&first.id);
        let second = handler::create_lot(None, create_req(&fixture, &lot_no, fixture.material_finished))
            .await
            .expect("同批号挂到另一个物料：合法（唯一键是物料级）");
        fixture.track(&second.id);
        assert_ne!(first.id, second.id);

        fixture.cleanup().await;
    }

    // ==================== 4. 改单撞键 ====================

    #[tokio::test]
    async fn update_into_existing_lot_no_is_friendly_409() {
        if !ensure_db().await {
            return;
        }
        let mut fixture = Fixture::build().await;
        let taken = fixture.lot_no("TAKEN");
        let a = handler::create_lot(None, create_req(&fixture, &taken, fixture.material_raw))
            .await
            .expect("批 A");
        fixture.track(&a.id);
        let b = handler::create_lot(
            None,
            create_req(&fixture, &fixture.lot_no("FREE"), fixture.material_raw),
        )
        .await
        .expect("批 B");
        fixture.track(&b.id);

        // B 改成 A 的批号 ⇒ 409 + 66056
        let error = handler::update_lot(
            None,
            &b.id,
            LotUpdateReq {
                lot_no: Some(taken.clone()),
                ..Default::default()
            },
        )
        .await
        .expect_err("撞键");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(error.code, 66056);
        assert!(error.msg.contains(&taken), "{}", error.msg);

        // 改成自己原来的批号 ⇒ 不算冲突（排除自己那一行）
        let same = handler::update_lot(
            None,
            &b.id,
            LotUpdateReq {
                lot_no: Some(fixture.lot_no("FREE")),
                remark: Some(Some("改备注".to_string())),
                ..Default::default()
            },
        )
        .await
        .expect("改成自己原批号不算冲突");
        assert_eq!(same.remark.as_deref(), Some("改备注"));

        fixture.cleanup().await;
    }

    // ==================== 5. 并发穿透：库的唯一索引兜底 ====================

    #[tokio::test]
    async fn db_unique_index_still_catches_concurrent_insert() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;
        let lot_no = fixture.lot_no("RACE");
        // 绕过 handler 的预检，直接走 model::insert 两次（模拟「两个请求同时通过预检」）
        let spec =
            crate::business::erp::lot_dto::spec_from_create(&create_req(&fixture, &lot_no, fixture.material_raw))
                .expect("规格");
        let first = Uuid::now_v7();
        model::insert(first, &spec, None).await.expect("第一次直插");
        let error = model::insert(Uuid::now_v7(), &spec, None)
            .await
            .expect_err("第二次直插必须撞唯一索引");
        assert!(
            matches!(error, DbError::UniqueViolation { .. }),
            "库层要抛 UniqueViolation（不是 InvalidValue）：{error:?}"
        );
        // 翻译层把它变成 409 + 66056 + 人话
        let api = crate::business::erp::lot_dto::map_write_error(
            error,
            &lot_no,
            &format!("COSTB19-M-R-{} 原料", fixture.suffix),
            "测试",
        );
        assert_eq!(api.status, 409);
        assert_eq!(api.code, 66056);
        assert!(api.msg.contains(&lot_no), "{}", api.msg);

        // 清场（只删这一行 + 夹具）
        let _ = db()
            .exec_sql(
                "DELETE FROM biz_lot WHERE id = ?::uuid".to_string(),
                vec![Value::from(&first)],
            )
            .await;
        fixture.cleanup().await;
    }

    // ==================== 6. 引用 / 日期校验 ====================

    #[tokio::test]
    async fn refs_and_dates_are_rejected_with_field_names() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;

        // 物料不存在 → 400 指名 material_id
        let mut req = create_req(&fixture, &fixture.lot_no("NOMAT"), fixture.material_raw);
        req.material_id = Some(Uuid::now_v7().to_string());
        let error = handler::create_lot(None, req).await.expect_err("物料不存在");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("material_id"), "{}", error.msg);

        // 供应商不存在 → 400 指名 supplier_id
        let mut req = create_req(&fixture, &fixture.lot_no("NOSUP"), fixture.material_raw);
        req.supplier_id = Some(Uuid::now_v7().to_string());
        let error = handler::create_lot(None, req).await.expect_err("供应商不存在");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("supplier_id"), "{}", error.msg);

        // 失效日期早于生产日期 → 400（dto 提前拦，不等库的 CHECK）
        let mut req = create_req(&fixture, &fixture.lot_no("BADDATE"), fixture.material_raw);
        req.prod_date = Some("2026-11-10".to_string());
        req.expire_date = Some("2026-11-09".to_string());
        let error = handler::create_lot(None, req).await.expect_err("日期倒挂");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("expire_date"), "{}", error.msg);

        // 以上三条都不该留下任何行
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_lot WHERE material_id = ?::uuid",
                vec![Value::from(&fixture.material_raw)],
            )
            .await
            .expect("计数");
        assert_eq!(count, 0, "校验失败的请求不许落库");

        fixture.cleanup().await;
    }

    // ==================== 7. 下拉接口的入参守卫 ====================

    #[tokio::test]
    async fn options_require_material_and_reject_unknown() {
        if !ensure_db().await {
            return;
        }
        let fixture = Fixture::build().await;

        // 物料必填
        let error = handler::lot_options(LotOptionQuery {
            material_id: None,
            keyword: None,
        })
        .await
        .expect_err("缺物料");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("material_id"), "{}", error.msg);

        // 物料不存在 → 400（不是「空下拉」：空列表会让用户以为这个料没批）
        let error = handler::lot_options(LotOptionQuery {
            material_id: Some(Uuid::now_v7().to_string()),
            keyword: None,
        })
        .await
        .expect_err("物料不存在");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不存在"), "{}", error.msg);

        // 存在的物料但没批 ⇒ 正常空列表
        let options = handler::lot_options(LotOptionQuery {
            material_id: Some(fixture.material_raw.to_string()),
            keyword: None,
        })
        .await
        .expect("空下拉");
        assert!(options.is_empty());

        fixture.cleanup().await;
    }
}
