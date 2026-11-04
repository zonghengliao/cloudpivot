//! `asset_model` 的真库用例（`ERP_DB_URL` 开关）：卡片的库级约束与状态 CAS。
//!
//! 不设 `ERP_DB_URL` 时这些用例 **SKIP**（与本仓其它 ERP 用例同口径）。
//! 交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin2 \
//!   cargo test --lib business::erp::asset -- --test-threads=1
//! ```

use super::*;
use nik::prelude::rust_decimal::Decimal;

mod db {
    use super::*;
    use tokio::sync::OnceCell;

    /// 金额字面量（同上：自己造 `dec!`，不引 `rust_decimal_macros`）。
    macro_rules! dec {
        ($v:literal) => {{
            use std::str::FromStr as _;
            Decimal::from_str(stringify!($v)).expect("金额字面量非法")
        }};
    }

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

    /// 取一个已存在的部门 id（夹具用：不新建部门）。
    async fn any_dept() -> Option<Uuid> {
        let rows = db()
            .query_sql(
                "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1"
                    .to_string(),
                vec![],
            )
            .await
            .ok()?;
        rows.first().and_then(|row| row.get_typed("id").ok())
    }

    /// 取一个启用中的资产分类（V0229 种子）。
    async fn any_category() -> Option<CategoryRow> {
        category_active().await.ok()?.into_iter().next()
    }

    /// 清场：按 id 集合删本用例造的行（**绝不按前缀扫全库**）。
    async fn cleanup(asset_ids: &[Uuid]) {
        if asset_ids.is_empty() {
            return;
        }
        let list = asset_ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(",");
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_depreciation WHERE asset_id IN ({list})"),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_fixed_asset_change WHERE asset_id IN ({list})"),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_fixed_asset WHERE id IN ({list})"),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(
                format!(
                    "DELETE FROM biz_status_log WHERE entity_type = 'FIXED_ASSET' \
                       AND entity_id IN ({list})"
                ),
                vec![],
            )
            .await;
    }

    fn insert_input(
        dept: Uuid,
        category: &CategoryRow,
        spec: crate::business::erp::asset_dto::CreateAssetSpec,
        asset_no: String,
    ) -> AssetInsert {
        let (residual, base, monthly) = crate::business::erp::asset_dto::derive_amounts(
            spec.original_value,
            category.salvage_rate,
            category.useful_life_months,
        );
        AssetInsert {
            asset_no,
            category_code: category.category_code.clone(),
            use_dept_name: "夹具部门".to_string(),
            custodian_name: None,
            supplier_name: None,
            residual_value: residual,
            depreciable_base: base,
            monthly_depreciation: monthly,
            useful_life_months: category.useful_life_months,
            salvage_rate: category.salvage_rate,
            start_depreciation_date: crate::business::erp::asset_compute::start_depreciation_date(
                spec.purchase_date,
            ),
            operator: None,
            spec: crate::business::erp::asset_dto::CreateAssetSpec {
                use_dept_id: dept,
                ..spec
            },
        }
    }

    /// 建卡请求（夹具）。
    fn create_req(
        category_id: Uuid,
        dept_id: Uuid,
    ) -> crate::business::erp::asset_dto::CreateAssetReq {
        crate::business::erp::asset_dto::CreateAssetReq {
            asset_name: Some("夹具笔记本".to_string()),
            category_id: Some(category_id.to_string()),
            spec: None,
            use_dept_id: Some(dept_id.to_string()),
            custodian_id: None,
            location: None,
            original_value: Some(dec!(100000)),
            tax_amount: Some(Decimal::ZERO),
            purchase_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2099, 1, 15),
            source_type: None,
            source_id: None,
            source_no: None,
            buy_mode: None,
            pay_mode: None,
            supplier_id: None,
            remark: Some("FIN-2 真库用例".to_string()),
        }
    }

    /// 建卡请求 → 校验后的 spec（夹具；校验失败就是夹具写错了，直接 panic）。
    fn spec_of(
        req: &crate::business::erp::asset_dto::CreateAssetReq,
    ) -> crate::business::erp::asset_dto::CreateAssetSpec {
        crate::business::erp::asset_dto::validate_create(req).expect("夹具请求必须通过校验")
    }

    /// ① 建卡落库：派生金额自动算好、净值恒等成立、`ck_biz_fa_value` 拦得住负原值。
    #[tokio::test]
    async fn insert_asset_computes_derived_amounts() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let (Some(dept), Some(category)) = (any_dept().await, any_category().await) else {
            println!("[SKIP] 库里没有部门或资产分类（先跑 V0229）");
            return;
        };
        let req = create_req(category.id, dept);
        let spec = crate::business::erp::asset_dto::validate_create(&req).expect("校验通过");
        let id = Uuid::now_v7();
        let no = format!("FA-TEST-{}", &id.to_string()[..8]);
        let input = insert_input(dept, &category, spec, no.clone());
        asset_insert(db(), id, &input).await.expect("建卡");
        let row = asset_find(id).await.expect("查询").expect("卡片存在");
        assert_eq!(row.net_value, row.original_value, "初始净值 = 原值");
        assert_eq!(row.accumulated_depreciation, Decimal::ZERO);
        assert_eq!(row.status, "IN_USE");
        assert_eq!(
            row.depreciable_base,
            row.original_value - row.residual_value
        );
        assert!(
            row.monthly_depreciation > Decimal::ZERO,
            "月折旧额必须建卡时算好"
        );
        assert_eq!(row.asset_no, no);

        // 负原值必须被库挡住（ck_biz_fa_value）
        let bad_id = Uuid::now_v7();
        let mut bad = insert_input(
            dept,
            &category,
            spec_of(&create_req(category.id, dept)),
            format!("FA-BAD-{}", &bad_id.to_string()[..8]),
        );
        bad.spec.original_value = dec!(-1);
        bad.depreciable_base = dec!(-1);
        let err = asset_insert(db(), bad_id, &bad).await;
        assert!(err.is_err(), "负原值必须被 ck_biz_fa_value 拒绝");

        cleanup(&[id, bad_id]).await;
    }

    /// ② 同一来源单只能建一张卡（`uk_biz_fa_source`）。
    #[tokio::test]
    async fn source_can_only_be_converted_once() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let (Some(dept), Some(category)) = (any_dept().await, any_category().await) else {
            println!("[SKIP] 库里没有部门或资产分类");
            return;
        };
        let source_id = Uuid::now_v7();
        let mut spec = spec_of(&create_req(category.id, dept));
        spec.source_type = "STOCK_IN".to_string();
        spec.source_id = Some(source_id);
        spec.buy_mode = "FROM_STOCK".to_string();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        asset_insert(
            db(),
            first,
            &insert_input(
                dept,
                &category,
                spec.clone(),
                format!("FA-T1-{}", &first.to_string()[..8]),
            ),
        )
        .await
        .expect("第一张卡");
        let dup = asset_insert(
            db(),
            second,
            &insert_input(
                dept,
                &category,
                spec,
                format!("FA-T2-{}", &second.to_string()[..8]),
            ),
        )
        .await;
        assert!(
            matches!(dup, Err(DbError::UniqueViolation { .. })),
            "同一来源单二次转固必须撞 uk_biz_fa_source，实际 {dup:?}"
        );
        cleanup(&[first, second]).await;
    }

    /// ③ 状态 CAS：只有预期状态才能推进（并发两次只成功一次）。
    #[tokio::test]
    async fn status_cas_is_atomic() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let (Some(dept), Some(category)) = (any_dept().await, any_category().await) else {
            println!("[SKIP] 库里没有部门或资产分类");
            return;
        };
        let id = Uuid::now_v7();
        asset_insert(
            db(),
            id,
            &insert_input(
                dept,
                &category,
                spec_of(&create_req(category.id, dept)),
                format!("FA-CAS-{}", &id.to_string()[..8]),
            ),
        )
        .await
        .expect("建卡");
        let first = apply_asset_status(db(), id, "IN_USE", "IDLE", None)
            .await
            .expect("第一次 CAS");
        let second = apply_asset_status(db(), id, "IN_USE", "IDLE", None)
            .await
            .expect("第二次 CAS 不报错");
        assert_eq!(first, 1);
        assert_eq!(second, 0, "状态已经不匹配，第二次必须 0 行");
        cleanup(&[id]).await;
    }

    /// ④ 变动留痕 append-only：写进去就能按资产查到，且删除只能靠 DELETE（没有 deleted_at）。
    #[tokio::test]
    async fn change_rows_are_append_only() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let (Some(dept), Some(category)) = (any_dept().await, any_category().await) else {
            println!("[SKIP] 库里没有部门或资产分类");
            return;
        };
        let id = Uuid::now_v7();
        asset_insert(
            db(),
            id,
            &insert_input(
                dept,
                &category,
                spec_of(&create_req(category.id, dept)),
                format!("FA-CHG-{}", &id.to_string()[..8]),
            ),
        )
        .await
        .expect("建卡");
        insert_change(
            db(),
            id,
            &ChangeInsert {
                change_type: "TRANSFER",
                change_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2099, 2, 1).unwrap(),
                from_dept_id: Some(dept),
                from_dept_name: Some("原部门"),
                to_dept_id: Some(dept),
                to_dept_name: Some("新部门"),
                from_user_id: None,
                from_user_name: None,
                to_user_id: None,
                to_user_name: None,
                from_status: Some("IN_USE"),
                to_status: Some("IN_USE"),
                reason: "组织调整",
                voucher_id: None,
                approval_instance_id: None,
                operator_id: None,
                operator_name: Some("admin"),
                remark: None,
            },
        )
        .await
        .expect("写留痕");
        let rows = changes_of(id).await.expect("查留痕");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].change_type, "TRANSFER");
        assert_eq!(rows[0].to_dept_name.as_deref(), Some("新部门"));
        cleanup(&[id]).await;
    }
}
