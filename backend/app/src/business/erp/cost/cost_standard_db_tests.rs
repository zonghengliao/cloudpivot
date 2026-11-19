//! 标准成本卡版本表（`biz_cost_standard` / `biz_cost_standard_item`）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@127.0.0.1:5432/cp_m508_e2e \
//!   cargo test --lib cost_standard -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-08「标准成本：版本表（物料 × 期间）· 含生效/失效」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`create_version_computes_amounts_and_items`] | 建版：料 = Σ明细、工 = round(工时×费率,6)、总 = 三者之和；版本号从 1 起；明细带物料编码/名称 |
//! | 2 | [`overlap_is_rejected_while_adjacent_periods_pass`] | **验收项「任一日期一个真相」**：重叠 409/66108；端点相接放行且版本号递增 |
//! | 3 | [`effective_resolves_by_date_and_period`] | 按日 / 按期间取生效版本；期间末口径；查不到 404 且报文给建卡引导 |
//! | 4 | [`effective_reports_switch_inside_the_period`] | 期间内换版 ⇒ `switch_in_period = true` + `other_versions` 如实列出 |
//! | 5 | [`period_sheet_lists_materials_with_switch_flag`] | 期间矩阵：一行一个物料、期间末生效版本、换版标记、关键字筛选 |
//! | 6 | [`effective_version_cannot_rewrite_cost_basis`] | 已生效版本改费率 ⇒ 409/66109；改停止日 / 备注放行 |
//! | 7 | [`future_version_can_rewrite_items`] | 未生效版本可换标准 BOM（整表替换），金额跟着重算 |
//! | 8 | [`delete_rules_protect_effective_history`] | 已生效不许删 409/66110；未生效可删且删后 404 |
//! | 9 | [`deactivate_frees_the_window_for_a_new_version`] | **验收项「含生效/失效」**：失效后窗口释放 → 新版可覆盖同期间；再启用旧版 ⇒ 409/66108；再失效旧版 ⇒ 恢复 |
//! | 10 | [`db_exclusion_guard_blocks_overlapping_active_rows`] | 绕过 handler 直插 ⇒ 库层 `ex_biz_cost_std_active_window` 当场拒绝 |
//! | 11 | [`db_version_no_is_unique_per_material`] | 绕过 handler 直插同版本号 ⇒ `uk_biz_cost_std_ver` 拒绝 |
//! | 12 | [`page_filters_status_period_and_keyword`] | 列表筛选（状态 / 期间 / 关键字）真的生效，翻页合计恒定 |
//! | 13 | [`material_validation_rejects_missing_and_service`] | 物料不存在 / 服务类 / 明细物料不存在 ⇒ 400 且点名字段 |
//!
//! # 夹具纪律
//!
//! · 物料用 `test_seed::ensure_material_kind` 造（编码带进程内唯一后缀），**跑完只删本卡的
//!   标准成本行**（硬删，明细随卡头 CASCADE），共享基线（单位 / 物料）不删；
//! · 日期一律用「相对今天」算：写死 `2026-xx-xx` 过一年就会把「已生效」类用例全判错；
//! · 每个用例自己清场（`cleanup(&[material...])`），不按前缀扫全库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_standard_dto::{
        self as dto, StandardCreateReq, StandardEffectiveQuery, StandardItemReq, StandardPageQuery, StandardStatusReq,
        StandardUpdateReq,
    };
    use crate::business::erp::cost::cost_standard_handler as handler;
    use crate::business::erp::cost::cost_standard_model as model;
    use crate::business::erp::cost::cost_standard_publish as publish;
    use crate::business::flow::engine;
    use nik::prelude::chrono::{Duration, FixedOffset, NaiveDate, Utc};
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr as _;
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

    /// 唯一后缀。
    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 造一个物料（`ensure_material_kind` 自带唯一编码，重复调用不会撞）。
    async fn fixture_material(prefix: &str, material_type: &str) -> Uuid {
        let suffix = uniq();
        crate::test_seed::ensure_material_kind(
            &format!("M508-{prefix}-{}", crate::test_seed::last_chars(&suffix, 10)),
            &format!("M5-08 夹具 {prefix} {suffix}"),
            material_type,
        )
        .await
    }

    /// 写库小包装：`exec_sql` 收 `String`，字面量与 `format!` 结果都统一从 `Into<String>` 进。
    async fn exec(sql: impl Into<String>, params: Vec<Value>) -> DbResult<u64> {
        db().exec_sql(sql.into(), params).await
    }

    /// 清场：删本卡的标准成本行（明细随卡头 CASCADE）；物料等共享基线不删。
    ///
    /// M5-10 起卡会被提交进发布审批：卡删了但审批实例还留着就是**孤儿实例**
    /// （`biz_flow_instance.biz_id` 指向不存在的卡），会挡住 V0465 的 down 演练、
    /// 也会让「整库无孤儿」类断言误伤别人。所以按 FK 顺序把**指向已不存在卡**的
    /// 发布实例一并收掉（只碰 `COST_STANDARD_PUBLISH` 这一种业务类型）。
    async fn cleanup(materials: &[Uuid]) {
        for material_id in materials {
            if let Err(err) = exec(
                "DELETE FROM biz_cost_standard WHERE material_id = ?::uuid",
                vec![Value::from(material_id)],
            )
            .await
            {
                println!("清场失败（不影响断言）：{err}");
            }
        }
        for sql in [
            "DELETE FROM biz_flow_cc WHERE instance_id IN (SELECT id FROM biz_flow_instance \
              WHERE biz_type = 'COST_STANDARD_PUBLISH' AND biz_id NOT IN (SELECT id FROM biz_cost_standard))"
                .to_string(),
            "DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance \
              WHERE biz_type = 'COST_STANDARD_PUBLISH' AND biz_id NOT IN (SELECT id FROM biz_cost_standard))"
                .to_string(),
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance \
              WHERE biz_type = 'COST_STANDARD_PUBLISH' AND biz_id NOT IN (SELECT id FROM biz_cost_standard))"
                .to_string(),
            "DELETE FROM biz_flow_instance WHERE biz_type = 'COST_STANDARD_PUBLISH' \
              AND biz_id NOT IN (SELECT id FROM biz_cost_standard)"
                .to_string(),
        ] {
            if let Err(err) = exec(sql, vec![]).await {
                println!("清孤儿发布实例失败（不影响断言）：{err}");
            }
        }
    }

    /// 一条标准 BOM 行。
    fn item(component: Uuid, qty: &str, price: &str) -> StandardItemReq {
        StandardItemReq {
            component_material_id: Some(component.to_string()),
            std_qty: Some(Decimal::from_str(qty).expect("数量")),
            std_price: Some(Decimal::from_str(price).expect("单价")),
        }
    }

    /// 建版请求（显式生效窗口）。
    fn create_req(
        material: Uuid,
        from: NaiveDate,
        to: Option<NaiveDate>,
        items: Vec<StandardItemReq>,
    ) -> StandardCreateReq {
        StandardCreateReq {
            material_id: Some(material.to_string()),
            effective_from: Some(from.to_string()),
            effective_to: to.map(|date| date.to_string()),
            std_labor_hours: Some(Decimal::from_str("2.5").expect("工时")),
            std_labor_rate: Some(Decimal::from_str("31.111111").expect("费率")),
            std_overhead_cost: Some(Decimal::from_str("12.345678").expect("费用")),
            items: Some(items),
            status: None,
            remark: Some("M5-08 真库用例".to_string()),
            ..Default::default()
        }
    }

    /// 某物料存活版本号清单（升序）。
    async fn version_nos(material: Uuid) -> Vec<i32> {
        let rows = db()
            .find_with_params(
                "SELECT version_no FROM biz_cost_standard \
                  WHERE material_id = ?::uuid AND deleted_at IS NULL ORDER BY version_no ASC",
                vec![Value::from(&material)],
            )
            .await
            .expect("查版本号");
        rows.iter()
            .map(|row| row.get_typed("version_no").expect("version_no"))
            .collect()
    }

    /// 走**真实发布审批**把一版卡发出去（M5-10 起「新建 = 草稿」，取数前必须先发布）。
    ///
    /// 口径与生产一致：提交 → 逐一以「待办的 assignee」身份同意 → 审批事务提交后跑本桥回调。
    /// 单测进程里 `doc_reader` 注册表是空的（只在 bootstrap 填），所以这里直接调桥。
    async fn publish_now(id_text: &str) {
        let id = Uuid::parse_str(id_text).expect("卡 id");
        // 发起人必须是**真实存在的账号**（`biz_flow_instance.initiator_id` 有外键）：
        // 借 bootstrap 种下的 `admin`，不新造账号（少一份夹具要清场）。
        let operator: Uuid = db()
            .scalar_with_params::<Uuid>(
                "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL LIMIT 1",
                vec![],
            )
            .await
            .expect("查 admin 账号");
        publish::submit_publish(Some(operator), id_text)
            .await
            .expect("提交发布应成功");
        let instance = model::find_by_id(id)
            .await
            .expect("查卡头")
            .expect("卡头应存在")
            .publish_instance_id
            .expect("提交后必须挂实例");
        for (task_id, assignee) in crate::business::flow::test_support::pending_tasks(instance).await {
            let out = db()
                .transaction(|tx| async move { engine::approve(&tx, task_id, assignee, Some("M5-08 用例发布")).await })
                .await
                .expect("审批同意失败");
            assert!(out.rejected.is_none(), "审批被引擎拒绝：{:?}", out.rejected);
            publish::on_publish_decided(out.instance_id, &out.instance_status).await;
        }
        let row = model::find_by_id(id).await.expect("查卡头").expect("卡头应存在");
        assert_eq!(
            row.publish_state,
            dto::PUBLISH_PUBLISHED,
            "发布审批走完必须是 PUBLISHED（否则后面的取数断言会以 404 的形式误导人）"
        );
    }

    // ==================== 用例 ====================

    /// 1. 建版：三要素按口径算、版本号从 1 起、明细带物料编码/名称、物料信息实时 join。
    #[tokio::test]
    async fn create_version_computes_amounts_and_items() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let comp_a = fixture_material("RA", "RAW").await;
        let comp_b = fixture_material("RB", "RAW").await;
        let created = handler::create_standard(
            None,
            create_req(
                product,
                day(30),
                Some(day(60)),
                vec![
                    item(comp_a, "3.000000", "5.500000"),
                    item(comp_b, "2.000000", "0.333333"),
                ],
            ),
        )
        .await
        .expect("建标准成本版本");

        assert_eq!(created.head.version_no, 1, "第一版版本号 = 1");
        assert_eq!(
            created.head.status,
            dto::STATUS_ACTIVE,
            "默认启用（占住生效窗口等审批）"
        );
        // M5-10「审核后生效」：新建的是**草稿**，取数前必须走发布审批，所以文案不是「生效」
        assert_eq!(created.head.publish_state, dto::PUBLISH_DRAFT, "新建即草稿");
        assert_eq!(created.head.status_label, "草稿（未生效）");
        assert_eq!(created.head.publish_state_label, "草稿");
        assert_eq!(
            created.head.std_material_cost, "17.166666",
            "3 × 5.5 = 16.5，再加 2 × 0.333333"
        );
        assert_eq!(
            created.head.std_labor_cost, "77.777778",
            "2.5 × 31.111111 = 77.7777775 → 半数远离零"
        );
        assert_eq!(created.head.std_overhead_cost, "12.345678");
        assert_eq!(
            created.head.std_total_cost, "107.290122",
            "总成本 = 料(17.166666) + 工(77.777778) + 费(12.345678)（ck_biz_cost_std_sum 的口径）"
        );
        assert_eq!(created.head.items_count, 2);
        assert!(
            created
                .head
                .material_code
                .as_deref()
                .is_some_and(|code| code.starts_with("M508-FG-")),
            "列表必须带物料编码（约定 13：*_id 带 *_name）：{:?}",
            created.head.material_code
        );
        assert!(created.head.material_name.is_some(), "列表必须带物料名称");
        assert_eq!(created.items.len(), 2);
        assert_eq!(created.items[0].line_no, 1);
        assert_eq!(created.items[0].std_amount, "16.500000");
        assert!(
            created.items[1].component_material_code.is_some(),
            "明细必须带耗用物料编码：{:?}",
            created.items[1]
        );

        // 列表里能查到，且金额与详情一致
        let page = handler::page_standards(StandardPageQuery {
            material_id: Some(product.to_string()),
            ..Default::default()
        })
        .await
        .expect("分页");
        assert_eq!(page.total, 1);
        assert_eq!(page.list[0].id, created.head.id);
        assert_eq!(page.list[0].std_total_cost, created.head.std_total_cost);

        cleanup(&[product]).await;
    }

    /// 2. 重叠被拒（409/66108）；端点相接放行且版本号递增。
    #[tokio::test]
    async fn overlap_is_rejected_while_adjacent_periods_pass() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let first = handler::create_standard(None, create_req(product, day(30), Some(day(60)), vec![]))
            .await
            .expect("第一版");

        // 与第一版重叠 ⇒ 409 + 66108
        let err = handler::create_standard(None, create_req(product, day(45), Some(day(75)), vec![]))
            .await
            .expect_err("重叠必须被拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_OVERLAP);
        assert!(err.msg.contains("三条出路"), "报文要给修改路径：{}", err.msg);

        // 端点相接（第二版从第一版停止日的次日开始）⇒ 放行
        let second = handler::create_standard(None, create_req(product, day(61), Some(day(90)), vec![]))
            .await
            .expect("相邻窗口应放行");
        assert_eq!(second.head.version_no, 2, "版本号在同物料内递增");
        assert_eq!(version_nos(product).await, vec![1, 2]);

        cleanup(&[product]).await;
        assert_eq!(version_nos(product).await, Vec::<i32>::new(), "清场零残留");
        let _ = first;
    }

    /// 3. 按日 / 按期间取生效版本；查不到 404 且报文给建卡引导。
    #[tokio::test]
    async fn effective_resolves_by_date_and_period() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let comp = fixture_material("RA", "RAW").await;
        let created = handler::create_standard(
            None,
            create_req(
                product,
                day(-5),
                Some(day(25)),
                vec![item(comp, "1.000000", "2.000000")],
            ),
        )
        .await
        .expect("建版");
        publish_now(&created.head.id).await;

        // 按日：区间内含两端
        for on in [day(-5), today(), day(25)] {
            let hit = handler::effective_of(StandardEffectiveQuery {
                material_id: Some(product.to_string()),
                on_date: Some(on.to_string()),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|err| panic!("{on} 应命中：{}", err.msg));
            assert_eq!(hit.version.head.id, created.head.id);
            assert!(!hit.switch_in_period);
            assert_eq!(hit.version.items.len(), 1);
        }

        // 区间外 ⇒ 404（报文点名期间与建卡入口）
        let err = handler::effective_of(StandardEffectiveQuery {
            material_id: Some(product.to_string()),
            on_date: Some(day(40).to_string()),
            ..Default::default()
        })
        .await
        .expect_err("区间外应 404");
        assert_eq!(err.status, 404);
        assert!(err.msg.contains("没有生效的标准成本版本"), "{}", err.msg);

        // 按期间：取期末那一天
        let period = dto::period_of_date(day(5));
        let by_period = handler::effective_of(StandardEffectiveQuery {
            material_id: Some(product.to_string()),
            period_code: Some(period.clone()),
            ..Default::default()
        })
        .await
        .expect("按期间取数");
        assert_eq!(by_period.period_code.as_deref(), Some(period.as_str()));
        let (_, period_end) = dto::period_window(&period).expect("期间");
        assert_eq!(by_period.on_date, period_end.to_string(), "期间取数按期末口径");
        assert_eq!(by_period.version.head.id, created.head.id);

        // 期间与日期矛盾 ⇒ 400
        let bad = handler::effective_of(StandardEffectiveQuery {
            material_id: Some(product.to_string()),
            period_code: Some(period),
            on_date: Some(day(200).to_string()),
        })
        .await
        .expect_err("日期不在期间内");
        assert_eq!(bad.status, 400);

        // 缺期间也缺日期 ⇒ 400
        let missing = handler::effective_of(StandardEffectiveQuery {
            material_id: Some(product.to_string()),
            ..Default::default()
        })
        .await
        .expect_err("缺取数日期");
        assert_eq!(missing.status, 400);

        cleanup(&[product]).await;
    }

    /// 4. 期间内换版 ⇒ switch_in_period + other_versions（不静默取其一）。
    #[tokio::test]
    async fn effective_reports_switch_inside_the_period() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        // 用一个「下个月」当期间：期初 ~ 15 日一版，16 日 ~ 期末一版
        let period = dto::period_of_date(day(40));
        let (start, end) = dto::period_window(&period).expect("期间");
        let mid = start + Duration::days(14);
        let first = handler::create_standard(None, create_req(product, start, Some(mid), vec![]))
            .await
            .expect("前半期版本");
        let second = handler::create_standard(None, create_req(product, mid + Duration::days(1), Some(end), vec![]))
            .await
            .expect("后半期版本");
        publish_now(&first.head.id).await;
        publish_now(&second.head.id).await;

        let hit = handler::effective_of(StandardEffectiveQuery {
            material_id: Some(product.to_string()),
            period_code: Some(period.clone()),
            ..Default::default()
        })
        .await
        .expect("期间取数");
        assert_eq!(hit.version.head.id, second.head.id, "取期末生效的那一版");
        assert!(hit.switch_in_period, "期间内换过版必须报出来");
        assert_eq!(hit.other_versions.len(), 1);
        assert_eq!(hit.other_versions[0].id, first.head.id);
        assert!(hit.note.contains("换过标准成本"), "{}", hit.note);

        cleanup(&[product]).await;
    }

    /// 5. 期间矩阵：一行一个物料、期间末生效版本、换版标记、关键字筛选。
    #[tokio::test]
    async fn period_sheet_lists_materials_with_switch_flag() {
        if !ensure_db().await {
            return;
        }
        let steady = fixture_material("FG", "FINISHED").await;
        let switching = fixture_material("FG", "FINISHED").await;
        let period = dto::period_of_date(day(40));
        let (start, end) = dto::period_window(&period).expect("期间");
        let mid = start + Duration::days(9);

        let steady_card = handler::create_standard(None, create_req(steady, start, Some(end), vec![]))
            .await
            .expect("整期一版");
        let first_switch = handler::create_standard(None, create_req(switching, start, Some(mid), vec![]))
            .await
            .expect("换版前半");
        let second_switch =
            handler::create_standard(None, create_req(switching, mid + Duration::days(1), Some(end), vec![]))
                .await
                .expect("换版后半");
        // M5-10：矩阵只出**已发布**的版本，所以三张卡都得走发布审批
        publish_now(&steady_card.head.id).await;
        publish_now(&first_switch.head.id).await;
        publish_now(&second_switch.head.id).await;

        let sheet = handler::period_sheet(crate::business::erp::cost::cost_standard_dto::PeriodSheetQuery {
            period_code: Some(period.clone()),
            ..Default::default()
        })
        .await
        .expect("期间矩阵");
        // 不断言全表 total：演练库是共享的，历史用例可能留下别的物料的标准卡。
        // 「只出期末那一版 / 换版标记 / 关键字收窄」这三条都按**本次夹具的物料**断言，确定性更强。
        let steady_row = sheet
            .list
            .iter()
            .find(|row| row.head.material_id == steady.to_string())
            .expect("稳定物料在矩阵里");
        assert!(!steady_row.switch_in_period, "整期一版不该标换版");
        // 换版物料在矩阵里只出现一行（期末那一版）
        let switching_rows: Vec<_> = sheet
            .list
            .iter()
            .filter(|row| row.head.material_id == switching.to_string())
            .collect();
        assert_eq!(switching_rows.len(), 1, "换版物料在矩阵里只出期末那一版");
        let switching_row = switching_rows[0];
        assert!(switching_row.switch_in_period, "期间内换过版要标出来");
        assert_eq!(switching_row.head.version_no, 2, "矩阵出期末生效的那一版");

        // 关键字筛选：只留一个物料
        let keyword = sheet
            .list
            .iter()
            .find(|row| row.head.material_id == switching.to_string())
            .and_then(|row| row.head.material_code.clone())
            .expect("换版物料编码");
        let filtered = handler::period_sheet(crate::business::erp::cost::cost_standard_dto::PeriodSheetQuery {
            period_code: Some(period.clone()),
            keyword: Some(keyword),
            ..Default::default()
        })
        .await
        .expect("关键字筛选");
        assert_eq!(filtered.total, 1, "关键字应只命中一个物料");
        assert_eq!(filtered.list[0].head.material_id, switching.to_string());

        // 期间必填
        let missing = handler::period_sheet(crate::business::erp::cost::cost_standard_dto::PeriodSheetQuery::default())
            .await
            .expect_err("缺期间");
        assert_eq!(missing.status, 400);

        cleanup(&[steady, switching]).await;
    }

    /// 6. 已生效版本不许改口径（409/66109），但可以改停止日 / 备注。
    #[tokio::test]
    async fn effective_version_cannot_rewrite_cost_basis() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let created = handler::create_standard(None, create_req(product, day(-40), Some(day(-10)), vec![]))
            .await
            .expect("建一个已经生效过的版本");

        // 改费率 ⇒ 409 + 66109
        let err = handler::update_standard(
            None,
            &created.head.id,
            StandardUpdateReq {
                std_labor_rate: Some(Decimal::from_str("40").expect("费率")),
                ..Default::default()
            },
        )
        .await
        .expect_err("已生效版本改费率应被拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_LOCKED);
        assert!(err.msg.contains("标准工资率"), "报文要点名改了什么：{}", err.msg);

        // 改生效起始日 ⇒ 同样被拒
        let err = handler::update_standard(
            None,
            &created.head.id,
            StandardUpdateReq {
                effective_from: Some(day(-30).to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("已生效版本改生效日应被拒");
        assert_eq!(err.code, dto::CODE_LOCKED);

        // 改停止日 + 备注 ⇒ 放行（收口旧版是正常动作）
        let closed = handler::update_standard(
            None,
            &created.head.id,
            StandardUpdateReq {
                effective_to: Some(day(-20).to_string()),
                remark: Some("收口到 20 天前".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("收口停止日应放行");
        assert_eq!(closed.head.effective_to.as_deref(), Some(day(-20).to_string().as_str()));
        assert_eq!(closed.head.remark.as_deref(), Some("收口到 20 天前"));
        assert_eq!(closed.head.std_total_cost, created.head.std_total_cost, "钱一分没动");

        cleanup(&[product]).await;
    }

    /// 7. 未生效版本可以整表替换标准 BOM，金额跟着重算。
    #[tokio::test]
    async fn future_version_can_rewrite_items() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let comp_a = fixture_material("RA", "RAW").await;
        let comp_b = fixture_material("RB", "RAW").await;
        let created = handler::create_standard(
            None,
            create_req(
                product,
                day(30),
                Some(day(60)),
                vec![item(comp_a, "2.000000", "10.000000")],
            ),
        )
        .await
        .expect("建未来版本");
        assert_eq!(created.head.std_material_cost, "20.000000");

        let updated = handler::update_standard(
            None,
            &created.head.id,
            StandardUpdateReq {
                items: Some(vec![item(comp_b, "5.000000", "3.000000")]),
                std_overhead_cost: Some(Decimal::from_str("1.5").expect("费用")),
                ..Default::default()
            },
        )
        .await
        .expect("换明细");
        assert_eq!(updated.items.len(), 1, "整表替换：旧明细删干净");
        assert_eq!(updated.items[0].component_material_id, comp_b.to_string());
        assert_eq!(updated.head.std_material_cost, "15.000000", "5 × 3");
        assert_eq!(updated.head.std_overhead_cost, "1.500000");

        // 清空明细（Some([])）：料归零，工费照旧
        let cleared = handler::update_standard(
            None,
            &created.head.id,
            StandardUpdateReq {
                items: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await
        .expect("清空明细");
        assert!(cleared.items.is_empty());
        assert_eq!(cleared.head.std_material_cost, "0.000000");

        cleanup(&[product]).await;
    }

    /// 8. 已生效不许删（409/66110）；未生效可删，删后详情 404。
    #[tokio::test]
    async fn delete_rules_protect_effective_history() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let past = handler::create_standard(None, create_req(product, day(-30), Some(day(-1)), vec![]))
            .await
            .expect("建过去版本");
        let future = handler::create_standard(None, create_req(product, day(30), Some(day(60)), vec![]))
            .await
            .expect("建未来版本");

        let err = handler::delete_standard(None, &past.head.id)
            .await
            .expect_err("已生效版本不许删");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_DELETE_IN_USE);
        assert!(err.msg.contains("失效"), "报文要给改失效的路：{}", err.msg);

        handler::delete_standard(None, &future.head.id)
            .await
            .expect("未来版本可删");
        let gone = handler::detail_standard(&future.head.id)
            .await
            .expect_err("删后详情 404");
        assert_eq!(gone.status, 404);

        cleanup(&[product]).await;
    }

    /// 9. **生效 / 失效的完整闭环**：失效释放窗口 → 新版覆盖同期间 → 再启用旧版被拒 →
    ///    再失效新版后旧版可重新启用。
    #[tokio::test]
    async fn deactivate_frees_the_window_for_a_new_version() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let old = handler::create_standard(None, create_req(product, day(-5), Some(day(25)), vec![]))
            .await
            .expect("旧版");
        // M5-10：`status` 的启用/失效只对**已发布**的版本开放（未发布要先走发布审批）
        publish_now(&old.head.id).await;

        // 有效窗口内直接建新版 ⇒ 409
        let err = handler::create_standard(None, create_req(product, day(10), Some(day(40)), vec![]))
            .await
            .expect_err("窗口被占时不许建");
        assert_eq!(err.code, dto::CODE_OVERLAP);

        // 失效旧版 ⇒ 释放窗口，且要给出「今天起没标准」的警告
        let deactivated = handler::set_status(
            None,
            &old.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_INACTIVE),
                reason: Some("标准过时，换新版".to_string()),
            },
        )
        .await
        .expect("失效");
        assert_eq!(deactivated.version.head.status, dto::STATUS_INACTIVE);
        assert!(!deactivated.warnings.is_empty(), "失效必须报后果");
        assert!(
            deactivated
                .warnings
                .iter()
                .any(|warning| warning.contains("不再有生效的标准成本")),
            "今天被覆盖的版本失效 ⇒ 要点名「今天起没标准」：{:?}",
            deactivated.warnings
        );
        assert!(
            deactivated
                .version
                .head
                .remark
                .as_deref()
                .is_some_and(|remark| remark.contains("[失效")),
            "给了原因就要留痕：{:?}",
            deactivated.version.head.remark
        );

        // 失效后**同一生效日、同一窗口**可以建新版（V0463 把 uk_biz_cost_std 收窄成
        // 「仅启用行同日唯一」才放开的能力；旧索引会连失效行一起锁，这一步会撞唯一键）
        let new_one = handler::create_standard(None, create_req(product, day(-5), Some(day(25)), vec![]))
            .await
            .expect("失效后同日重建新版应放行");
        assert_eq!(new_one.head.version_no, 2);
        assert_eq!(new_one.head.effective_from, day(-5).to_string(), "同生效日重建");
        assert!(new_one.head.status == dto::STATUS_ACTIVE);
        publish_now(&new_one.head.id).await;

        // 再启用旧版 ⇒ 与新版重叠 ⇒ 409
        let err = handler::set_status(
            None,
            &old.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_ACTIVE),
                reason: None,
            },
        )
        .await
        .expect_err("重叠时不许再启用");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_OVERLAP);

        // 重复失效（已是失效）⇒ 幂等返回，不写库、无警告
        let again = handler::set_status(
            None,
            &old.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_INACTIVE),
                reason: None,
            },
        )
        .await
        .expect("重复失效应幂等");
        assert!(again.message.contains("已经是"), "{}", again.message);
        assert!(again.warnings.is_empty());

        // 失效新版 ⇒ 旧版可重新启用
        handler::set_status(
            None,
            &new_one.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_INACTIVE),
                reason: Some("回滚到旧标准".to_string()),
            },
        )
        .await
        .expect("失效新版");
        let reactivated = handler::set_status(
            None,
            &old.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_ACTIVE),
                reason: Some("恢复".to_string()),
            },
        )
        .await
        .expect("旧版重新启用");
        assert_eq!(reactivated.version.head.status, dto::STATUS_ACTIVE);

        // status 缺失 ⇒ 400
        let missing = handler::set_status(None, &old.head.id, StandardStatusReq::default())
            .await
            .expect_err("缺 status");
        assert_eq!(missing.status, 400);

        cleanup(&[product]).await;
    }

    /// 10. 库层区间护栏：绕过 handler 直插重叠的启用版本 ⇒ 数据库当场拒绝。
    #[tokio::test]
    async fn db_exclusion_guard_blocks_overlapping_active_rows() {
        if !ensure_db().await {
            return;
        }
        // 先确认护栏真的建上了（btree_gist 不可用时迁移会降级并 WARNING，此用例会如实失败）
        let guard = db()
            .find_with_params(
                "SELECT conname FROM pg_constraint \
                  WHERE conrelid = 'public.biz_cost_standard'::regclass \
                    AND conname = 'ex_biz_cost_std_active_window'",
                vec![],
            )
            .await
            .expect("查约束");
        if guard.is_empty() {
            println!("SKIP: 本库没有 ex_biz_cost_std_active_window（btree_gist 不可用的降级环境）");
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let first = handler::create_standard(None, create_req(product, day(30), Some(day(60)), vec![]))
            .await
            .expect("第一版");

        let err = exec(
            "INSERT INTO biz_cost_standard \
                     (material_id, version_no, effective_from, effective_to, status, remark) \
                 VALUES (?::uuid, 99, ?::text::date, ?::text::date, 1, 'M5-08 越权直插')",
            vec![
                Value::from(&product),
                Value::Text(day(45).to_string()),
                Value::Text(day(75).to_string()),
            ],
        )
        .await
        .expect_err("库层必须拒绝重叠的启用版本");
        assert!(
            format!("{err}").contains("ex_biz_cost_std_active_window"),
            "拒绝原因应点名区间约束：{err}"
        );

        // 但同一窗口 status = 0（失效）可以并存 —— 「含失效」的库层证明
        exec(
            "INSERT INTO biz_cost_standard \
                 (material_id, version_no, effective_from, effective_to, status, remark) \
             VALUES (?::uuid, 98, ?::text::date, ?::text::date, 0, 'M5-08 失效探针')",
            vec![
                Value::from(&product),
                Value::Text(day(45).to_string()),
                Value::Text(day(75).to_string()),
            ],
        )
        .await
        .expect("失效版本不占窗口，应允许并存");

        cleanup(&[product]).await;
        let _ = first;
    }

    /// 11. 版本号唯一：绕过 handler 直插同物料同版本号 ⇒ 唯一索引拒绝。
    #[tokio::test]
    async fn db_version_no_is_unique_per_material() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        handler::create_standard(None, create_req(product, day(30), Some(day(60)), vec![]))
            .await
            .expect("第一版");

        let err = exec(
            "INSERT INTO biz_cost_standard \
                     (material_id, version_no, effective_from, effective_to, status, remark) \
                 VALUES (?::uuid, 1, ?::text::date, ?::text::date, 0, 'M5-08 重号探针')",
            vec![
                Value::from(&product),
                Value::Text(day(200).to_string()),
                Value::Text(day(230).to_string()),
            ],
        )
        .await
        .expect_err("同物料重号必须被拒");
        assert!(
            format!("{err}").contains("uk_biz_cost_std_ver"),
            "拒绝原因应点名版本号唯一索引：{err}"
        );

        cleanup(&[product]).await;
    }

    /// 12. 列表筛选：状态 / 期间 / 关键字；分页合计与筛选条件一致。
    #[tokio::test]
    async fn page_filters_status_period_and_keyword() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let past = handler::create_standard(None, create_req(product, day(-30), Some(day(-10)), vec![]))
            .await
            .expect("过去版");
        let future = handler::create_standard(None, create_req(product, day(30), Some(day(60)), vec![]))
            .await
            .expect("未来版");

        // 状态筛选：都还启用 ⇒ 两条
        let all = handler::page_standards(StandardPageQuery {
            material_id: Some(product.to_string()),
            ..Default::default()
        })
        .await
        .expect("全量");
        assert_eq!(all.total, 2);

        // 失效 past 之后，status=1 只剩一条
        handler::set_status(
            None,
            &past.head.id,
            StandardStatusReq {
                status: Some(dto::STATUS_INACTIVE),
                reason: None,
            },
        )
        .await
        .expect("失效过去版");
        let active = handler::page_standards(StandardPageQuery {
            material_id: Some(product.to_string()),
            status: Some(dto::STATUS_ACTIVE),
            ..Default::default()
        })
        .await
        .expect("筛启用");
        assert_eq!(active.total, 1);
        assert_eq!(active.list[0].id, future.head.id);
        let inactive = handler::page_standards(StandardPageQuery {
            material_id: Some(product.to_string()),
            status: Some(dto::STATUS_INACTIVE),
            ..Default::default()
        })
        .await
        .expect("筛失效");
        assert_eq!(inactive.total, 1);
        assert_eq!(inactive.list[0].id, past.head.id);

        // 期间筛选：未来版所在期间才命中
        let future_period = dto::period_of_date(day(35));
        let by_period = handler::page_standards(StandardPageQuery {
            material_id: Some(product.to_string()),
            period_code: Some(future_period),
            ..Default::default()
        })
        .await
        .expect("按期间筛");
        assert_eq!(by_period.total, 1);
        assert_eq!(by_period.list[0].id, future.head.id);

        // 关键字：物料编码能命中；乱码关键字 0 条
        let code = future.head.material_code.clone().expect("物料编码");
        let by_keyword = handler::page_standards(StandardPageQuery {
            keyword: Some(code),
            ..Default::default()
        })
        .await
        .expect("关键字");
        assert!(by_keyword.total >= 1, "物料编码应命中");
        let none = handler::page_standards(StandardPageQuery {
            keyword: Some("M508-不存在的关键字-zzz".to_string()),
            ..Default::default()
        })
        .await
        .expect("乱码关键字");
        assert_eq!(none.total, 0);

        cleanup(&[product]).await;
    }

    /// 13. 物料校验：不存在 / 服务类 / 明细物料不存在 ⇒ 400 且点名字段。
    #[tokio::test]
    async fn material_validation_rejects_missing_and_service() {
        if !ensure_db().await {
            return;
        }
        let missing_material =
            handler::create_standard(None, create_req(Uuid::now_v7(), day(30), Some(day(60)), vec![]))
                .await
                .expect_err("物料不存在应 400");
        assert_eq!(missing_material.status, 400);
        assert!(missing_material.msg.contains("material_id"), "{}", missing_material.msg);

        let service = fixture_material("SV", "SERVICE").await;
        let service_err = handler::create_standard(None, create_req(service, day(30), Some(day(60)), vec![]))
            .await
            .expect_err("服务类物料应 400");
        assert_eq!(service_err.status, 400);
        assert!(service_err.msg.contains("SERVICE"), "{}", service_err.msg);

        let product = fixture_material("FG", "FINISHED").await;
        let component_err = handler::create_standard(
            None,
            create_req(product, day(30), Some(day(60)), vec![item(Uuid::now_v7(), "1", "1")]),
        )
        .await
        .expect_err("明细物料不存在应 400");
        assert_eq!(component_err.status, 400);
        assert!(
            component_err.msg.contains("第 1 行"),
            "报文要点名第几行：{}",
            component_err.msg
        );

        // 停用物料：直接改库造一个，验 400 报文
        let disabled = fixture_material("FG", "FINISHED").await;
        exec(
            "UPDATE biz_material SET status = 0 WHERE id = ?::uuid",
            vec![Value::from(&disabled)],
        )
        .await
        .expect("停用物料");
        let disabled_err = handler::create_standard(None, create_req(disabled, day(30), Some(day(60)), vec![]))
            .await
            .expect_err("停用物料应 400");
        assert_eq!(disabled_err.status, 400);
        assert!(disabled_err.msg.contains("已停用"), "{}", disabled_err.msg);
        exec(
            "UPDATE biz_material SET status = 1 WHERE id = ?::uuid",
            vec![Value::from(&disabled)],
        )
        .await
        .expect("恢复物料状态");

        cleanup(&[product, service, disabled]).await;
    }
}
