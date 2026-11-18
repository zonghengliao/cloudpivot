//! 成本方法配置（`biz_cost_method_config`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@127.0.0.1:5432/cp_m501_dev \
//!   cargo test --lib cost_method_db_tests -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-01「按成本对象选方法」）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`seed_row_is_the_phase_one_default`] | V0459 种子行在（GLOBAL/JOB），生效方法查询落到它 |
//! | 2 | [`create_list_and_effective_for_object`] | 建对象级配置 → 列表出参（含对象号）→ 生效查询报未实现 |
//! | 3 | [`overlap_rejected_and_adjacent_window_accepted`] | **验收项**「口径唯一」：重叠 409/66102；相邻放行 |
//! | 4 | [`same_effective_day_is_blocked_by_the_unique_index`] | 库层唯一索引兜底（并发下 handler 预检会双双通过） |
//! | 5 | [`effective_window_is_inclusive_on_both_ends`] | 生效期两端都是「含」；区间外落回默认 |
//! | 6 | [`scope_validation_rejects_bad_targets`] | 全局带对象 / 对象不存在 / 对象已作废 ⇒ 400/66103 |
//! | 7 | [`delete_rules_protect_effective_history`] | **验收项**「已生效不许删」409/66104；未来版本可删 |
//! | 8 | [`update_rules_protect_effective_history`] | **验收项**「已生效不许改口径」409/66105；停止日/停用可改 |
//! | 9 | [`switch_method_needs_closing_the_previous_version`] | 完整切换流程：先收旧版停止日 → 再建新版 |
//! | 10 | [`calc_guard_blocks_unimplemented_method`] | **验收项**「不静默按分批法算」：计算入口 409/66101 |
//! | 11 | [`page_filters_and_keyword`] | 列表筛选（范围 / 方法 / 状态 / 对象 / 关键字）真的生效 |
//!
//! # 夹具纪律
//!
//! · 成本对象用**直接 INSERT** 造（只为本表的外键弱引用服务，不测成本对象自身的校验）；
//! · 配置行按 `id` / `scope_id` 删（绝不按前缀扫全库）；日期一律用「相对今天」算，
//!   不用写死的 2026-xx-xx —— 写死的日期过一年就会把「已生效」类用例全判错；
//! · 种子行（`…d001`，GLOBAL/JOB）**只读不动**：需要「全局口径」的用例走直接 INSERT 造探针行，
//!   用完按 id 删；唯一例外是 [`switch_method_needs_closing_the_previous_version`]
//!   （那条用例本身就是「切换流程」，收尾必须把种子的 `effective_to` 还原成长期有效）。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_method_dto::{
        self as dto, CostMethodCreateReq, CostMethodEffectiveQuery, CostMethodPageQuery, CostMethodUpdateReq,
        METHOD_JOB, METHOD_STANDARD, METHOD_VARIETY, SCOPE_GLOBAL, SCOPE_OBJECT,
    };
    use crate::business::erp::cost::cost_method_handler as handler;
    use crate::business::erp::cost::cost_method_model as model;
    use nik::prelude::chrono::{Duration, FixedOffset, NaiveDate, Utc};
    use std::str::FromStr;
    use tokio::sync::OnceCell;

    /// 种子行主键（V0459）。
    const SEED_ID: &str = "20000000-0000-4000-8000-00000000d001";

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

    /// 今日（CST）—— 与 handler 同口径。
    fn today() -> NaiveDate {
        Utc::now()
            .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
            .date_naive()
    }

    /// 相对今天偏移若干天。
    fn day(offset: i64) -> NaiveDate {
        today() + Duration::days(offset)
    }

    /// 唯一后缀（进程内计数器 + 纳秒）。
    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 造一个成本对象（直接 INSERT，只满足本表弱引用的存在性）。
    async fn fixture_object(status: &str) -> Uuid {
        let suffix = uniq();
        let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
        let product = crate::test_seed::ensure_material_kind(
            &format!("M501-MAT-{suffix}"),
            &format!("M5-01 夹具产出品 {suffix}"),
            "FINISHED",
        )
        .await;
        let code = format!("M5W{}", crate::test_seed::last_chars(&suffix, 10));
        let warehouse = crate::test_seed::ensure_warehouse(&code, &format!("M5-01 夹具仓 {suffix}")).await;
        let id = Uuid::now_v7();
        db().exec_sql(
            "INSERT INTO biz_cost_object \
                 (id, object_no, object_type, product_id, target_warehouse_id, plan_qty, \
                  workshop_dept_id, workshop_dept_name, start_date, status, created_at, updated_at) \
             VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?::uuid, 100, ?::uuid, 'M5-01 车间', ?::text::date, ?, now(), now())"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(format!("CB-M501-{suffix}")),
                Value::from(&product),
                Value::from(&warehouse),
                Value::from(&dept),
                Value::Text(today().to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("造成本对象夹具");
        id
    }

    /// 清场：先删该对象的配置行，再删对象本身（物料 / 仓库是共享基线，不删）。
    async fn cleanup_object(object: Uuid) {
        for sql in [
            format!("DELETE FROM biz_cost_method_config WHERE scope_id = '{object}'::uuid"),
            format!("DELETE FROM biz_cost_object WHERE id = '{object}'::uuid"),
        ] {
            if let Err(err) = db().exec_sql(sql, vec![]).await {
                println!("清场失败（不影响断言）：{err}");
            }
        }
    }

    /// 直接 INSERT 一条配置（绕过 handler 的重叠预检；测库层唯一键与拦截清单用）。
    async fn insert_raw(
        method: &str,
        scope_type: &str,
        scope_id: Option<Uuid>,
        from: NaiveDate,
        to: Option<NaiveDate>,
        status: i16,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let scope = scope_id.map(|id| id.to_string()).unwrap_or_default();
        let end = to.map(|date| date.to_string()).unwrap_or_default();
        db().exec_sql(
            "INSERT INTO biz_cost_method_config \
                 (id, method, scope_type, scope_id, effective_from, effective_to, status, remark) \
             VALUES (?::uuid, ?, ?, NULLIF(?::text,'')::uuid, ?::text::date, \
                     NULLIF(?::text,'')::text::date, ?, 'M5-01 真库用例探针')"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(method.to_string()),
                Value::Text(scope_type.to_string()),
                Value::Text(scope),
                Value::Text(from.to_string()),
                Value::Text(end),
                Value::I16(status),
            ],
        )
        .await
        .expect("直接插入配置探针");
        id
    }

    /// 按 id 删配置（硬删，测试专用；生产走软删）。
    async fn purge_config(ids: &[Uuid]) {
        for id in ids {
            if let Err(err) = db()
                .exec_sql(
                    format!("DELETE FROM biz_cost_method_config WHERE id = '{id}'::uuid"),
                    vec![],
                )
                .await
            {
                println!("清场失败（不影响断言）：{err}");
            }
        }
    }

    /// 断言是某个业务码的错误。
    fn expect_err<T: std::fmt::Debug>(result: std::result::Result<T, ApiError>, status: u16, code: i32, ctx: &str) {
        match result {
            Ok(value) => panic!("{ctx}：预期 {status}/{code}，实际成功（{value:?}）"),
            Err(error) => {
                assert_eq!(error.status, status, "{ctx}：HTTP 状态（报文 {}）", error.msg);
                assert_eq!(error.code, code, "{ctx}：业务码（报文 {}）", error.msg);
            }
        }
    }

    fn create_req(
        method: &str,
        scope: &str,
        scope_id: Option<Uuid>,
        from: NaiveDate,
        to: Option<NaiveDate>,
        remark: &str,
    ) -> CostMethodCreateReq {
        CostMethodCreateReq {
            method: Some(method.to_string()),
            scope: Some(scope.to_string()),
            scope_id: scope_id.map(|id| id.to_string()),
            effective_from: Some(from.to_string()),
            effective_to: to.map(|date| date.to_string()),
            status: None,
            remark: Some(remark.to_string()),
        }
    }

    fn effective_query(scope_id: Option<Uuid>, on: NaiveDate) -> CostMethodEffectiveQuery {
        CostMethodEffectiveQuery {
            cost_object_id: scope_id.map(|id| id.to_string()),
            on: Some(on.to_string()),
        }
    }

    // ==================== 用例 ====================

    /// 1. 种子行（GLOBAL/JOB）在，且「什么都不配」时生效方法查询落到它。
    #[tokio::test]
    async fn seed_row_is_the_phase_one_default() {
        if !ensure_db().await {
            return;
        }
        let seed = Uuid::from_str(SEED_ID).expect("种子行 id");
        let rows = db()
            .find_with_params(
                "SELECT method, scope_type, scope_id, status FROM biz_cost_method_config \
                  WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&seed)],
            )
            .await
            .expect("查种子行");
        assert_eq!(rows.len(), 1, "V0459 的默认口径种子行必须存在");
        let row = &rows[0];
        assert_eq!(row.get_typed::<String>("method").expect("method"), METHOD_JOB);
        assert_eq!(row.get_typed::<String>("scope_type").expect("scope"), SCOPE_GLOBAL);
        assert!(
            row.get_typed::<Option<Uuid>>("scope_id").expect("scope_id").is_none(),
            "全局口径不该带对象"
        );
        assert_eq!(row.get_typed::<i16>("status").expect("status"), 1);

        let res = handler::effective_of(effective_query(None, today()))
            .await
            .expect("查生效方法");
        assert_eq!(res.method, METHOD_JOB);
        assert_eq!(res.source, dto::SOURCE_GLOBAL, "命中种子行");
        assert!(res.method_implemented, "分批法一期已实现");
        assert_eq!(res.config_id.as_deref(), Some(SEED_ID));
    }

    /// 2. 建对象级配置 → 列表出参（对象号 / 未实现提示）→ 生效查询按日期切换。
    #[tokio::test]
    async fn create_list_and_effective_for_object() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let created = handler::create_config(
            None,
            create_req(METHOD_VARIETY, SCOPE_OBJECT, Some(object), day(30), None, "M5-01 case2"),
        )
        .await
        .expect("建对象级配置");
        assert_eq!(created.method, METHOD_VARIETY);
        assert_eq!(created.scope, SCOPE_OBJECT);
        assert_eq!(created.scope_id.as_deref(), Some(object.to_string().as_str()));
        assert!(
            created.scope_object_no.is_some(),
            "列表必须带对象号（约定 13：*_id 带 *_name）"
        );
        assert!(!created.method_implemented, "品种法一期未实现");
        assert!(
            created.method_note.contains("三期"),
            "说明要写清排期：{}",
            created.method_note
        );

        let page = handler::page_configs(CostMethodPageQuery {
            scope: Some(SCOPE_OBJECT.to_string()),
            scope_id: Some(object.to_string()),
            ..Default::default()
        })
        .await
        .expect("分页");
        assert_eq!(page.total, 1);
        assert_eq!(page.list[0].id, created.id);

        // 生效日之前仍按全局默认（种子行 JOB）
        let before = handler::effective_of(effective_query(Some(object), day(29)))
            .await
            .expect("查生效方法");
        assert_eq!(before.method, METHOD_JOB);
        assert_eq!(before.source, dto::SOURCE_GLOBAL);

        // 生效日起是品种法，且明确告知「这期算不出来」
        let after = handler::effective_of(effective_query(Some(object), day(30)))
            .await
            .expect("查生效方法");
        assert_eq!(after.method, METHOD_VARIETY);
        assert_eq!(after.source, dto::SOURCE_OBJECT);
        assert!(!after.method_implemented);
        assert!(
            after.engine_hint.contains("66101"),
            "结论要点名业务码：{}",
            after.engine_hint
        );
        assert!(after.cost_object_no.is_some(), "要带对象号");

        cleanup_object(object).await;
    }

    /// 3. 重叠 409/66102；相邻窗口放行（旧版止于前一日，新版次日生效）。
    #[tokio::test]
    async fn overlap_rejected_and_adjacent_window_accepted() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        handler::create_config(
            None,
            create_req(
                METHOD_JOB,
                SCOPE_OBJECT,
                Some(object),
                day(-90),
                Some(day(-30)),
                "M5-01 case3-a",
            ),
        )
        .await
        .expect("建第一段");

        // 跨过第一段停止日的窗口 ⇒ 重叠
        expect_err(
            handler::create_config(
                None,
                create_req(
                    METHOD_STANDARD,
                    SCOPE_OBJECT,
                    Some(object),
                    day(-60),
                    Some(day(-10)),
                    "M5-01 case3-b",
                ),
            )
            .await,
            409,
            dto::CODE_OVERLAP,
            "重叠窗口必须拒绝",
        );

        // 紧接第一段停止日的次日 ⇒ 放行（关闭与开启严丝合缝是切换的标准做法）
        let adjacent = handler::create_config(
            None,
            create_req(
                METHOD_VARIETY,
                SCOPE_OBJECT,
                Some(object),
                day(-29),
                None,
                "M5-01 case3-c",
            ),
        )
        .await
        .expect("相邻窗口必须放行");
        assert_eq!(adjacent.effective_from, day(-29).to_string());

        cleanup_object(object).await;
    }

    /// 4. 同对象同生效日：handler 预检 409；直接 INSERT 由库层唯一索引兜底。
    #[tokio::test]
    async fn same_effective_day_is_blocked_by_the_unique_index() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        handler::create_config(
            None,
            create_req(METHOD_JOB, SCOPE_OBJECT, Some(object), day(-5), None, "M5-01 case4-a"),
        )
        .await
        .expect("建第一条");

        expect_err(
            handler::create_config(
                None,
                create_req(
                    METHOD_VARIETY,
                    SCOPE_OBJECT,
                    Some(object),
                    day(-5),
                    None,
                    "M5-01 case4-b",
                ),
            )
            .await,
            409,
            dto::CODE_OVERLAP,
            "同生效日的第二条必须拒绝",
        );

        // 绕过 handler 直插：库层 `uk_biz_cmc_object_effective` 必须拦住
        let err = db()
            .exec_sql(
                "INSERT INTO biz_cost_method_config (id, method, scope_type, scope_id, effective_from, status) \
                 VALUES (gen_random_uuid(), 'STANDARD', 'COST_OBJECT', ?::uuid, ?::text::date, 1)"
                    .to_string(),
                vec![Value::from(&object), Value::Text(day(-5).to_string())],
            )
            .await
            .expect_err("库层唯一索引必须拦");
        let text = format!("{err:?}");
        assert!(
            text.contains("23505") || text.contains("duplicate") || text.to_lowercase().contains("unique"),
            "应是唯一键冲突：{text}"
        );

        cleanup_object(object).await;
    }

    /// 5. 生效期两端含；区间外落回全局默认。
    #[tokio::test]
    async fn effective_window_is_inclusive_on_both_ends() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        handler::create_config(
            None,
            create_req(
                METHOD_VARIETY,
                SCOPE_OBJECT,
                Some(object),
                day(-10),
                Some(day(-2)),
                "M5-01 case5",
            ),
        )
        .await
        .expect("建窗口");

        for (offset, expect_method, ctx) in [
            (-11, METHOD_JOB, "生效日前一天 → 默认"),
            (-10, METHOD_VARIETY, "生效日当天 → 命中"),
            (-2, METHOD_VARIETY, "停止日当天 → 命中"),
            (-1, METHOD_JOB, "停止日次日 → 默认"),
        ] {
            let res = handler::effective_of(effective_query(Some(object), day(offset)))
                .await
                .expect("查生效方法");
            assert_eq!(res.method, expect_method, "{ctx}（{offset}）");
        }

        cleanup_object(object).await;
    }

    /// 6. 范围参数三种非法形态都 400/66103。
    #[tokio::test]
    async fn scope_validation_rejects_bad_targets() {
        if !ensure_db().await {
            return;
        }
        // ① 全局却带对象
        expect_err(
            handler::create_config(
                None,
                create_req(
                    METHOD_JOB,
                    SCOPE_GLOBAL,
                    Some(Uuid::now_v7()),
                    day(10),
                    None,
                    "M5-01 case6-a",
                ),
            )
            .await,
            400,
            dto::CODE_SCOPE_INVALID,
            "全局范围不许带对象",
        );

        // ② 对象不存在
        expect_err(
            handler::create_config(
                None,
                create_req(
                    METHOD_JOB,
                    SCOPE_OBJECT,
                    Some(Uuid::now_v7()),
                    day(10),
                    None,
                    "M5-01 case6-b",
                ),
            )
            .await,
            400,
            dto::CODE_SCOPE_INVALID,
            "不存在的对象必须拒绝",
        );

        // ③ 对象已作废
        let cancelled = fixture_object("CANCELLED").await;
        expect_err(
            handler::create_config(
                None,
                create_req(
                    METHOD_JOB,
                    SCOPE_OBJECT,
                    Some(cancelled),
                    day(10),
                    None,
                    "M5-01 case6-c",
                ),
            )
            .await,
            400,
            dto::CODE_SCOPE_INVALID,
            "作废对象必须拒绝",
        );
        cleanup_object(cancelled).await;
    }

    /// 7. 已生效不许删（409/66104）；未来版本可删且删后查不到。
    #[tokio::test]
    async fn delete_rules_protect_effective_history() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let past = insert_raw(METHOD_JOB, SCOPE_OBJECT, Some(object), day(-20), None, 1).await;
        let future = insert_raw(METHOD_VARIETY, SCOPE_OBJECT, Some(object), day(20), None, 1).await;

        expect_err(
            handler::delete_config(None, &past.to_string()).await,
            409,
            dto::CODE_DELETE_IN_USE,
            "已生效的配置不许删",
        );
        assert!(
            model::find_by_id(past).await.expect("查").is_some(),
            "被拒之后行必须还在"
        );

        handler::delete_config(None, &future.to_string())
            .await
            .expect("未来版本可以删");
        assert!(model::find_by_id(future).await.expect("查").is_none(), "软删后查不到");

        cleanup_object(object).await;
    }

    /// 8. 已生效不许改方法 / 生效日（409/66105）；停止日 / 状态 / 备注可以改。
    #[tokio::test]
    async fn update_rules_protect_effective_history() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let id = insert_raw(METHOD_JOB, SCOPE_OBJECT, Some(object), day(-20), None, 1).await;

        expect_err(
            handler::update_config(
                None,
                &id.to_string(),
                CostMethodUpdateReq {
                    method: Some(METHOD_STANDARD.to_string()),
                    ..Default::default()
                },
            )
            .await,
            409,
            dto::CODE_LOCKED_EFFECTIVE,
            "已生效的版本不许改方法",
        );

        expect_err(
            handler::update_config(
                None,
                &id.to_string(),
                CostMethodUpdateReq {
                    effective_from: Some(day(-30).to_string()),
                    ..Default::default()
                },
            )
            .await,
            409,
            dto::CODE_LOCKED_EFFECTIVE,
            "已生效的版本不许改生效日",
        );

        // 收停止日 = 切换的标准前置动作，必须允许
        let closed = handler::update_config(
            None,
            &id.to_string(),
            CostMethodUpdateReq {
                effective_to: Some(day(-1).to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("收停止日必须允许");
        assert_eq!(closed.effective_to.as_deref(), Some(day(-1).to_string().as_str()));

        // 备注也能改
        let remarked = handler::update_config(
            None,
            &id.to_string(),
            CostMethodUpdateReq {
                remark: Some("切换前的旧口径".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("改备注必须允许");
        assert_eq!(remarked.remark.as_deref(), Some("切换前的旧口径"));

        // 停用之后：该对象在窗口内落回全局默认
        handler::update_config(
            None,
            &id.to_string(),
            CostMethodUpdateReq {
                status: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("停用必须允许");
        let res = handler::effective_of(effective_query(Some(object), day(-10)))
            .await
            .expect("查生效方法");
        assert_eq!(res.method, METHOD_JOB, "停用的行不参与判定");

        cleanup_object(object).await;
    }

    /// 9. 完整切换流程：新版本与旧版重叠 ⇒ 先收旧版停止日，再建新版。
    ///
    /// 这条用例是**运维路径的说明书**：用户报「建不出来」时，界面上的正确操作就是这两步。
    /// 用的是种子行（GLOBAL/JOB，开区间）—— 收尾必须把它的 `effective_to` 还原成长期有效。
    #[tokio::test]
    async fn switch_method_needs_closing_the_previous_version() {
        if !ensure_db().await {
            return;
        }
        let seed = Uuid::from_str(SEED_ID).expect("种子行 id");

        // ① 直接建新版 ⇒ 与种子行的开区间重叠
        expect_err(
            handler::create_config(
                None,
                create_req(METHOD_VARIETY, SCOPE_GLOBAL, None, day(30), None, "M5-01 case9"),
            )
            .await,
            409,
            dto::CODE_OVERLAP,
            "旧版还开着就建新版必须被拦",
        );

        // ② 收旧版停止日（唯一允许改的字段之一）
        handler::update_config(
            None,
            SEED_ID,
            CostMethodUpdateReq {
                effective_to: Some(day(29).to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("收种子的停止日");

        // ③ 再建新版 ⇒ 放行
        let new_id = handler::create_config(
            None,
            create_req(METHOD_VARIETY, SCOPE_GLOBAL, None, day(30), None, "M5-01 case9"),
        )
        .await
        .expect("收口后建新版必须放行")
        .id;

        // ④ 生效方法按日期切换
        let before = handler::effective_of(effective_query(None, day(29))).await.expect("查");
        assert_eq!(before.method, METHOD_JOB);
        assert_eq!(before.source, dto::SOURCE_GLOBAL);
        let after = handler::effective_of(effective_query(None, day(30))).await.expect("查");
        assert_eq!(after.method, METHOD_VARIETY);
        assert!(!after.method_implemented);

        // ⑤ 收尾：删新版 + 种子停止日还原成长期有效（remark 不动：None = 不改）
        purge_config(&[Uuid::from_str(&new_id).expect("新配置 id")]).await;
        handler::update_config(
            None,
            SEED_ID,
            CostMethodUpdateReq {
                effective_to: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("还原种子行停止日");
        let restored = model::find_by_id(seed).await.expect("查种子").expect("种子在");
        assert!(restored.effective_to.is_none(), "收尾必须还原成长期有效");
    }

    /// 10. 计算入口的方法闸：本期有非 JOB 配置 ⇒ 409/66101；停用后放行。
    #[tokio::test]
    async fn calc_guard_blocks_unimplemented_method() {
        if !ensure_db().await {
            return;
        }
        // 独立期间 + 独立窗口，避免与其它用例的探针行互相干扰
        let period = "2027-06";
        let start = NaiveDate::from_ymd_opt(2027, 6, 1).expect("日期");
        let end = NaiveDate::from_ymd_opt(2027, 6, 30).expect("日期");
        let probe = insert_raw(METHOD_STANDARD, SCOPE_GLOBAL, None, start, Some(end), 1).await;

        let err = handler::ensure_methods_runnable_in_tx(db(), period, start, end)
            .await
            .expect_err("标准成本必须被拦");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_UNIMPLEMENTED);
        assert!(err.msg.contains("全公司默认"), "要点名范围：{}", err.msg);
        assert!(err.msg.contains(period), "要点名期间：{}", err.msg);

        // 真的走一遍计算入口（calc 在开算前就被拦，库里一个字节都没动）
        let calc_err = crate::business::erp::cost::cost_calc_period::calc_period(Some(period), None)
            .await
            .expect_err("calc_period 必须被同一道闸拦下");
        assert_eq!(calc_err.code, dto::CODE_UNIMPLEMENTED);

        // 停用后放行
        db().exec_sql(
            format!("UPDATE biz_cost_method_config SET status = 0 WHERE id = '{probe}'::uuid"),
            vec![],
        )
        .await
        .expect("停用探针");
        handler::ensure_methods_runnable_in_tx(db(), period, start, end)
            .await
            .expect("停用后必须放行");

        purge_config(&[probe]).await;
    }

    /// 11. 列表筛选（范围 / 方法 / 状态 / 对象 / 关键字）真的生效。
    #[tokio::test]
    async fn page_filters_and_keyword() {
        if !ensure_db().await {
            return;
        }
        let object = fixture_object("OPEN").await;
        let visible = handler::create_config(
            None,
            create_req(
                METHOD_VARIETY,
                SCOPE_OBJECT,
                Some(object),
                day(40),
                Some(day(45)),
                "M5-01 case11 关键词",
            ),
        )
        .await
        .expect("建可见行");
        let hidden = handler::create_config(
            None,
            create_req(
                METHOD_JOB,
                SCOPE_OBJECT,
                Some(object),
                day(46),
                None,
                "M5-01 case11 另一行",
            ),
        )
        .await
        .expect("建第二行");

        let by_method = handler::page_configs(CostMethodPageQuery {
            scope: Some(SCOPE_OBJECT.to_string()),
            method: Some(METHOD_VARIETY.to_string()),
            scope_id: Some(object.to_string()),
            ..Default::default()
        })
        .await
        .expect("按方法筛");
        assert_eq!(by_method.total, 1);
        assert_eq!(by_method.list[0].id, visible.id);

        let by_keyword = handler::page_configs(CostMethodPageQuery {
            scope_id: Some(object.to_string()),
            keyword: Some("关键词".to_string()),
            ..Default::default()
        })
        .await
        .expect("按关键字筛");
        assert_eq!(by_keyword.total, 1);
        assert_eq!(by_keyword.list[0].id, visible.id);

        let by_status = handler::page_configs(CostMethodPageQuery {
            scope_id: Some(object.to_string()),
            status: Some(1),
            ..Default::default()
        })
        .await
        .expect("按状态筛");
        assert_eq!(by_status.total, 2, "两行都是启用");

        // 停用第二行后按状态筛只剩一行
        handler::update_config(
            None,
            &Uuid::from_str(&hidden.id).expect("id").to_string(),
            CostMethodUpdateReq {
                status: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("停用第二行");
        let enabled = handler::page_configs(CostMethodPageQuery {
            scope_id: Some(object.to_string()),
            status: Some(1),
            ..Default::default()
        })
        .await
        .expect("按状态筛");
        assert_eq!(enabled.total, 1);
        assert_eq!(enabled.list[0].id, visible.id);

        cleanup_object(object).await;
    }
}
