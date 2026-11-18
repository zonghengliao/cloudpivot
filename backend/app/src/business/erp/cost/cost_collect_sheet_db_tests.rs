//! 按生产订单/批次归集表（M5-02）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m502_dev \
//!   cargo test --lib cost_collect_sheet -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-02「按生产订单/批次归集表」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`sheet_row_per_batch_with_three_elements_and_totals`] | 一行一批 / 料工费三列 / 行数 / 最后一笔日期 / 整表合计 = 行之和 |
//! | 2 | [`sheet_period_isolation_and_pool_rows_are_excluded`] | 只认本期；费用池行（无成本对象）不进批表、不进合计 |
//! | 3 | [`sheet_filters_status_dept_and_keyword`] | 状态 / 车间 / 关键字（订单号 + 产品名 + 批次号）真的生效 |
//! | 4 | [`sheet_pagination_keeps_totals_whole`] | 翻页不改变合计（合计是整张表，不是当前页） |
//! | 5 | [`sheet_sorts_by_total_and_order_no`] | 两个排序键的顺序（金额倒序 / 订单号倒序） |
//! | 6 | [`sheet_scope_limits_rows_totals_and_detail`] | 数据范围：部门用户只看本部门，明细不可见 ⇒ 404 |
//! | 7 | [`detail_lines_add_up_to_the_sheet_row`] | 明细逐笔 + 三项和 == 归集表该行（同一份数据两个粒度） |
//! | 8 | [`detail_rejects_bad_period_and_unknown_batch`] | 期间非法 400 / 批次不存在 404 / 已软删 404 |
//! | 9 | [`zz_teardown_leaves_zero_residue`] | 清场后按 id 回查：归集行 / 批次 / 部门 全部 0 |
//!
//! # 夹具纪律
//!
//! * **本用例独占一个车间**（`dept_code` 带唯一后缀）：所有断言都按该车间过滤，
//!   别的用例在同一期间造的成本对象掺不进来 —— 这样「整表合计 = 行之和」才是
//!   确定性断言，而不是「跑全量套件时对、单跑时对、两轮之间会漂」的假绿。
//! * **批次号带唯一后缀**（`CB-M502-<标记>-<后缀尾>`）：上一轮失败留下的批次不会让
//!   这一轮撞 `uk_biz_cost_object_no`；也因此本文件**不需要**「按前缀扫全库」的清理
//!   （那种清理会误删并发跑的另一个用例的夹具）。
//! * 期间用 **2031-07 / 2031-08**（远离真实数据与其它夹具的期间段）。
//! * 清场**按 id** 删；部门 / 物料 / 仓库按本用例唯一编码删。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_collect_sheet_dto::{self as dto, SheetDetailQuery, SheetQuery};
    use crate::business::erp::cost::cost_collect_sheet_handler as handler;
    use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
    use cloudpivot_kernel::business::shared::scope::ScopeKind;
    use tokio::sync::OnceCell;

    /// 归集表用的期间（远离真实数据）。
    const PERIOD: &str = "2031-07";
    /// 另一个期间（验证「只认本期」）。
    const OTHER_PERIOD: &str = "2031-08";

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

    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 夹具造出来的批次（id + 批次号：断言按 id 找行，不靠猜批次号）。
    struct Obj {
        id: Uuid,
        no: String,
    }

    /// 夹具：本用例独占的车间 + 第二车间 + 产出品 + 仓库。
    struct Fixture {
        suffix: String,
        dept: Uuid,
        dept_code: String,
        other_dept: Uuid,
        other_dept_code: String,
        product: Uuid,
        product_code: String,
        warehouse: Uuid,
        warehouse_code: String,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = uniq();
            let dept_code = format!("M502-DEPT-{suffix}");
            let other_dept_code = format!("M502-DEPT2-{suffix}");
            let dept =
                cloudpivot_kernel::shared::test_helpers::ensure_dept(&dept_code, &format!("M5-02 夹具车间 {suffix}"))
                    .await;
            let other_dept = cloudpivot_kernel::shared::test_helpers::ensure_dept(
                &other_dept_code,
                &format!("M5-02 夹具第二车间 {suffix}"),
            )
            .await;
            let product_code = format!("M502-MAT-{suffix}");
            let product = crate::test_seed::ensure_material_kind(
                &product_code,
                &format!("M5-02 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse_code = format!("M5W2{}", crate::test_seed::last_chars(&suffix, 8));
            let warehouse =
                crate::test_seed::ensure_warehouse(&warehouse_code, &format!("M5-02 夹具仓 {suffix}")).await;
            Self {
                suffix,
                dept,
                dept_code,
                other_dept,
                other_dept_code,
                product,
                product_code,
                warehouse,
                warehouse_code,
            }
        }

        /// 超管范围（用例验证接口本身；数据范围由 [`sheet_scope_limits_rows_totals_and_detail`] 专测）。
        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        /// 部门范围：只能看 `dept`（`other_dept` 的批次不可见）。
        fn dept_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(self.dept),
                false,
                ScopeKind::Dept,
                vec![self.dept],
                false,
            )
        }

        /// 造一个成本对象（直接 INSERT：本卡只消费这张表，不测它的建单校验）。
        ///
        /// `tag` 只用于人读（`CB-M502-A-xxxxxx`）；断言按返回的 id 找行。
        async fn object(&self, dept: Uuid, status: &str, order_no: Option<&str>, tag: &str) -> Obj {
            let id = Uuid::now_v7();
            let no = format!("CB-M502-{tag}-{}", crate::test_seed::last_chars(&self.suffix, 6));
            // 关批必须有 closed_at（`ck_biz_cost_object_closed`），否则夹具插不进去
            let closed_at = if status == "CLOSED" {
                Value::Text("2031-07-31".to_string())
            } else {
                Value::Null
            };
            db().exec_sql(
                "INSERT INTO biz_cost_object \
                     (id, object_no, object_type, product_id, lot_no, target_warehouse_id, plan_qty, \
                      workshop_dept_id, workshop_dept_name, source_doc_no, start_date, status, closed_at, \
                      created_at, updated_at) \
                 VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, 100, ?::uuid, 'M5-02 夹具车间', ?, \
                         '2031-07-01'::date, ?, ?::text::timestamptz, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(no.clone()),
                    Value::from(&self.product),
                    Value::Text(format!("LOT-{tag}-{}", crate::test_seed::last_chars(&self.suffix, 6))),
                    Value::from(&self.warehouse),
                    Value::from(&dept),
                    order_no
                        .map(|text| Value::Text(text.to_string()))
                        .unwrap_or(Value::Null),
                    Value::Text(status.to_string()),
                    closed_at,
                ],
            )
            .await
            .expect("造成本对象夹具");
            Obj { id, no }
        }

        /// 造一笔归集行（料行带物料+数量，工/费行不带）。
        async fn collect(
            &self,
            object: &Obj,
            period: &str,
            element: &str,
            amount: &str,
            occur_date: &str,
            line_no: i32,
        ) {
            let (material, qty, unit_price) = if element == "MATERIAL" {
                (
                    Value::from(&self.product),
                    Value::Text("10".to_string()),
                    Value::Text("10".to_string()),
                )
            } else {
                (Value::Null, Value::Null, Value::Null)
            };
            db().exec_sql(
                "INSERT INTO biz_cost_collect \
                     (cost_object_id, period_code, element, source_type, source_no, line_no, occur_date, \
                      material_id, qty, unit_price, amount, dept_id, remark) \
                 VALUES (?::uuid, ?, ?, 'MANUAL', ?, ?, ?::text::date, ?::uuid, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::uuid, 'M5-02 真库用例')"
                    .to_string(),
                vec![
                    Value::from(&object.id),
                    Value::Text(period.to_string()),
                    Value::Text(element.to_string()),
                    Value::Text(format!("SHEET-{element}-{line_no}-{period}")),
                    Value::I32(line_no),
                    Value::Text(occur_date.to_string()),
                    material,
                    qty,
                    unit_price,
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造归集行夹具");
        }

        /// 造一条**费用池行**（不挂成本对象，只有折旧/报销/手工费用允许）。
        async fn pool_row(&self, period: &str, amount: &str) {
            db().exec_sql(
                "INSERT INTO biz_cost_collect \
                     (cost_object_id, period_code, element, source_type, occur_date, amount, dept_id, remark) \
                 VALUES (NULL, ?, 'OVERHEAD', 'DEPRECIATION', '2031-07-31'::date, ?::text::numeric, ?::uuid, 'M5-02 池行')"
                    .to_string(),
                vec![
                    Value::Text(period.to_string()),
                    Value::Text(amount.to_string()),
                    Value::from(&self.dept),
                ],
            )
            .await
            .expect("造费用池行夹具");
        }

        /// 归集表查询入口（固定本用例车间 + 期间，可按需覆盖其他字段）。
        fn query(&self) -> SheetQuery {
            SheetQuery {
                period_code: Some(PERIOD.to_string()),
                workshop_dept_id: Some(self.dept.to_string()),
                page: Some(1),
                page_size: Some(50),
                ..Default::default()
            }
        }

        /// 清场：按 id 删（归集 → 批次 → 物料/仓库/部门，顺序即引用方向）。
        async fn cleanup(&self, objects: &[&Obj]) {
            for object in objects {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_collect WHERE cost_object_id = ?::uuid".to_string(),
                        vec![Value::from(&object.id)],
                    )
                    .await;
                let _ = db()
                    .exec_sql(
                        "DELETE FROM biz_cost_object WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&object.id)],
                    )
                    .await;
            }
            // 池行没有对象 id，按「本用例车间 + 两个期间 + 折旧来源」删（只删夹具会写的那一种）
            let _ = db()
                .exec_sql(
                    "DELETE FROM biz_cost_collect \
                      WHERE cost_object_id IS NULL AND source_type = 'DEPRECIATION' \
                        AND period_code IN (?, ?) AND dept_id = ?::uuid"
                        .to_string(),
                    vec![
                        Value::Text(PERIOD.to_string()),
                        Value::Text(OTHER_PERIOD.to_string()),
                        Value::from(&self.dept),
                    ],
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
            for code in [&self.dept_code, &self.other_dept_code] {
                let _ = db()
                    .exec_sql(
                        "DELETE FROM sys_dept WHERE dept_code = ?".to_string(),
                        vec![Value::Text(code.clone())],
                    )
                    .await;
            }
        }
    }

    /// 归集表里按批次 id 找行（**不靠猜批次号**：批次号带唯一后缀）。
    fn row_of<'a>(res: &'a dto::SheetRes, object: &Obj) -> &'a dto::SheetRowRes {
        res.list
            .iter()
            .find(|row| row.object_id == object.id.to_string())
            .unwrap_or_else(|| {
                panic!(
                    "归集表里应当有批次 {}（{}）；实际 {:?}",
                    object.no,
                    object.id,
                    res.list.iter().map(|r| &r.object_no).collect::<Vec<_>>()
                )
            })
    }

    // ==================== 1. 一行一批 / 三要素 / 合计 ====================

    #[tokio::test]
    async fn sheet_row_per_batch_with_three_elements_and_totals() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let a = fx.object(fx.dept, "OPEN", Some("SO-M502-A"), "A").await;
        let b = fx.object(fx.dept, "OPEN", Some("SO-M502-B"), "B").await;
        // 批次 A：料 100 + 50、工 30、费 20（4 行）
        fx.collect(&a, PERIOD, "MATERIAL", "100", "2031-07-05", 1).await;
        fx.collect(&a, PERIOD, "MATERIAL", "50", "2031-07-20", 2).await;
        fx.collect(&a, PERIOD, "LABOR", "30", "2031-07-10", 1).await;
        fx.collect(&a, PERIOD, "OVERHEAD", "20", "2031-07-11", 1).await;
        // 批次 B：本期一条都没归集（是缺口，必须出现在表里）

        let principal = fx.super_principal();
        let res = handler::sheet_of(&principal, fx.query()).await.expect("查归集表");

        assert_eq!(res.period_code, PERIOD);
        assert_eq!(res.total, 2, "两个批次都该在表里（未归集批次不是噪声，是缺口）");

        let row_a = row_of(&res, &a);
        assert_eq!(row_a.amounts.mat, "150.0000", "料 = 100 + 50");
        assert_eq!(row_a.amounts.labor, "30.0000");
        assert_eq!(row_a.amounts.oh, "20.0000");
        assert_eq!(row_a.amounts.total, "200.0000", "合计必须等于三项之和");
        assert_eq!(row_a.line_count, 4);
        assert_eq!(row_a.last_occur_date.as_deref(), Some("2031-07-20"));
        assert_eq!(row_a.source_doc_no.as_deref(), Some("SO-M502-A"), "生产订单号要带出来");
        assert_eq!(row_a.object_no, a.no);
        assert_eq!(row_a.product_code.as_deref(), Some(fx.product_code.as_str()));
        assert_eq!(row_a.workshop_dept_id, fx.dept.to_string());
        assert_eq!(row_a.status, "OPEN");

        let row_b = row_of(&res, &b);
        assert_eq!(row_b.amounts.total, "0.0000");
        assert_eq!(row_b.line_count, 0, "line_count = 0 是「本期一条都没归集」的旗");
        assert_eq!(row_b.last_occur_date, None);

        // 整表合计 = 行之和；batch_count 只数「有归集」的批次
        assert_eq!(res.totals.amounts.mat, "150.0000");
        assert_eq!(res.totals.amounts.labor, "30.0000");
        assert_eq!(res.totals.amounts.oh, "20.0000");
        assert_eq!(res.totals.amounts.total, "200.0000");
        assert_eq!(res.totals.line_count, 4);
        assert_eq!(res.totals.batch_count, 1, "B 没有归集，不算进 batch_count");

        fx.cleanup(&[&a, &b]).await;
    }

    // ==================== 2. 期间隔离 + 池行不进表 ====================

    #[tokio::test]
    async fn sheet_period_isolation_and_pool_rows_are_excluded() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let a = fx.object(fx.dept, "OPEN", Some("SO-M502-P"), "P").await;
        fx.collect(&a, PERIOD, "MATERIAL", "100", "2031-07-05", 1).await;
        fx.collect(&a, OTHER_PERIOD, "MATERIAL", "999", "2031-08-05", 1).await;
        fx.pool_row(PERIOD, "777").await;

        let principal = fx.super_principal();
        let res = handler::sheet_of(&principal, fx.query()).await.expect("查归集表");
        let row = row_of(&res, &a);

        assert_eq!(
            row.amounts.mat, "100.0000",
            "别的期间的归集不许混进本期（只认 period_code）"
        );
        assert_eq!(res.totals.amounts.total, "100.0000", "费用池行不进批表，也不进合计");
        assert_eq!(res.totals.line_count, 1);
        assert!(
            !res.list.iter().any(|r| r.amounts.total == "777.0000"),
            "池行没有成本对象，不该变成任何一行"
        );

        // 另一个期间看到的是那 999
        let mut other = fx.query();
        other.period_code = Some(OTHER_PERIOD.to_string());
        let res_other = handler::sheet_of(&principal, other).await.expect("查另一期间");
        assert_eq!(row_of(&res_other, &a).amounts.mat, "999.0000");

        fx.cleanup(&[&a]).await;
    }

    // ==================== 3. 筛选：状态 / 车间 / 关键字 ====================

    #[tokio::test]
    async fn sheet_filters_status_dept_and_keyword() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let open = fx.object(fx.dept, "OPEN", Some("SO-M502-OPEN"), "O1").await;
        let closed = fx.object(fx.dept, "CLOSED", Some("SO-M502-CLOSED"), "C1").await;
        let elsewhere = fx.object(fx.other_dept, "OPEN", Some("SO-M502-OTHER"), "D1").await;
        for object in [&open, &closed, &elsewhere] {
            fx.collect(object, PERIOD, "MATERIAL", "10", "2031-07-15", 1).await;
        }

        let principal = fx.super_principal();

        // 状态：只返回 OPEN（本车间的 closed 被排除）
        let mut query = fx.query();
        query.status = Some("OPEN".to_string());
        let res = handler::sheet_of(&principal, query).await.expect("状态筛选");
        assert_eq!(res.total, 1);
        assert_eq!(res.list[0].object_id, open.id.to_string());
        // 合计也跟着筛选走（不是全量合计）
        assert_eq!(res.totals.amounts.total, "10.0000");

        let mut query = fx.query();
        query.status = Some("CLOSED".to_string());
        let res = handler::sheet_of(&principal, query).await.expect("状态筛选");
        assert_eq!(res.total, 1);
        assert_eq!(res.list[0].object_id, closed.id.to_string());

        // 车间：本车间两条，别的车间那条不在
        let res = handler::sheet_of(&principal, fx.query()).await.expect("车间筛选");
        assert_eq!(res.total, 2);
        assert!(res.list.iter().all(|row| row.workshop_dept_id == fx.dept.to_string()));

        // 关键字：命中生产订单号
        let mut query = fx.query();
        query.keyword = Some("SO-M502-CLOSED".to_string());
        let res = handler::sheet_of(&principal, query).await.expect("按订单号搜");
        assert_eq!(res.total, 1, "生产订单号必须可搜");
        assert_eq!(res.list[0].object_id, closed.id.to_string());

        // 关键字：命中产出品名称（join 出来的列，不是快照列）
        let mut query = fx.query();
        query.keyword = Some(fx.suffix.clone());
        let res = handler::sheet_of(&principal, query).await.expect("按产品名搜");
        assert_eq!(res.total, 2, "两批同产品，都该命中");

        // 关键字：命中批次号
        let mut query = fx.query();
        query.keyword = Some(open.no.clone());
        let res = handler::sheet_of(&principal, query).await.expect("按批次号搜");
        assert_eq!(res.total, 1);
        assert_eq!(res.list[0].object_id, open.id.to_string());

        // 关键字查不到：0 行、合计 0（不是报错）
        let mut query = fx.query();
        query.keyword = Some("NO-SUCH-THING-M502".to_string());
        let res = handler::sheet_of(&principal, query).await.expect("空结果");
        assert_eq!(res.total, 0);
        assert_eq!(res.totals.amounts.total, "0.0000");
        assert_eq!(res.totals.batch_count, 0);

        fx.cleanup(&[&open, &closed, &elsewhere]).await;
    }

    // ==================== 4. 翻页不改变合计 ====================

    #[tokio::test]
    async fn sheet_pagination_keeps_totals_whole() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let first = fx.object(fx.dept, "OPEN", Some("SO-M502-1"), "P1").await;
        let second = fx.object(fx.dept, "OPEN", Some("SO-M502-2"), "P2").await;
        fx.collect(&first, PERIOD, "MATERIAL", "100", "2031-07-05", 1).await;
        fx.collect(&second, PERIOD, "MATERIAL", "25", "2031-07-06", 1).await;

        let principal = fx.super_principal();
        let mut query = fx.query();
        query.page = Some(1);
        query.page_size = Some(1);
        let page1 = handler::sheet_of(&principal, query.clone()).await.expect("第 1 页");
        query.page = Some(2);
        let page2 = handler::sheet_of(&principal, query).await.expect("第 2 页");

        assert_eq!(
            (page1.total, page1.list.len()),
            (2, 1),
            "总数是筛选集，页大小只影响行数"
        );
        assert_eq!((page2.total, page2.list.len()), (2, 1));
        assert_ne!(page1.list[0].object_id, page2.list[0].object_id, "两页不许重叠");

        // 合计是**整张表**的：两页看到的合计必须一模一样
        assert_eq!(page1.totals.amounts.total, "125.0000");
        assert_eq!(
            page2.totals.amounts.total, "125.0000",
            "合计不能随翻页变（否则用户会以为账在跳）"
        );
        assert_eq!(page1.totals.batch_count, 2);

        fx.cleanup(&[&first, &second]).await;
    }

    // ==================== 5. 排序 ====================

    #[tokio::test]
    async fn sheet_sorts_by_total_and_order_no() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let small = fx.object(fx.dept, "OPEN", Some("SO-M502-AAA"), "S1").await;
        let big = fx.object(fx.dept, "OPEN", Some("SO-M502-ZZZ"), "S2").await;
        fx.collect(&small, PERIOD, "MATERIAL", "10", "2031-07-05", 1).await;
        fx.collect(&big, PERIOD, "MATERIAL", "300", "2031-07-06", 1).await;

        let principal = fx.super_principal();

        // 默认 sort=total：金额大的在前
        let res = handler::sheet_of(&principal, fx.query()).await.expect("默认排序");
        assert_eq!(res.list[0].object_id, big.id.to_string(), "默认按合计金额倒序");
        assert_eq!(res.list[1].object_id, small.id.to_string());

        // sort=order_no：订单号倒序
        let mut query = fx.query();
        query.sort = Some(dto::SORT_ORDER.to_string());
        let res = handler::sheet_of(&principal, query).await.expect("按订单号排序");
        assert_eq!(res.list[0].source_doc_no.as_deref(), Some("SO-M502-ZZZ"));

        // sort=object_no：批次号倒序（两批后缀相同，比的是标记位 S2 > S1）
        let mut query = fx.query();
        query.sort = Some(dto::SORT_OBJECT.to_string());
        let res = handler::sheet_of(&principal, query).await.expect("按批次号排序");
        assert_eq!(res.list[0].object_id, big.id.to_string());

        // 值域外 ⇒ 400（绝不把用户输入拼进 ORDER BY）
        let mut query = fx.query();
        query.sort = Some("amount desc; drop table biz_cost_object".to_string());
        let error = handler::sheet_of(&principal, query)
            .await
            .expect_err("注入式排序必须 400");
        assert_eq!(error.status, 400);

        fx.cleanup(&[&small, &big]).await;
    }

    // ==================== 6. 数据范围 ====================

    #[tokio::test]
    async fn sheet_scope_limits_rows_totals_and_detail() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let mine = fx.object(fx.dept, "OPEN", Some("SO-M502-MINE"), "M1").await;
        let theirs = fx.object(fx.other_dept, "OPEN", Some("SO-M502-THEIRS"), "T1").await;
        fx.collect(&mine, PERIOD, "MATERIAL", "100", "2031-07-05", 1).await;
        fx.collect(&theirs, PERIOD, "MATERIAL", "900", "2031-07-05", 1).await;

        let principal = fx.dept_principal();
        // 查询里故意不带车间条件：范围必须由数据权限注入，而不是靠调用方自觉
        let mut query = fx.query();
        query.workshop_dept_id = None;
        let res = handler::sheet_of(&principal, query).await.expect("部门范围查归集表");

        assert_eq!(res.total, 1, "部门用户只看得到本部门的批次");
        assert_eq!(res.list[0].object_id, mine.id.to_string());
        assert_eq!(
            res.totals.amounts.total, "100.0000",
            "合计也必须按范围裁剪（否则等于泄露别的车间金额）"
        );

        // 明细：本部门可见，别部门的 404（不是 403 —— 403 等于承认 id 存在）
        let detail = handler::detail_of(
            &principal,
            &mine.id.to_string(),
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect("本部门批次可看明细");
        assert_eq!(detail.lines.len(), 1);

        let error = handler::detail_of(
            &principal,
            &theirs.id.to_string(),
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect_err("别的车间的批次必须 404");
        assert_eq!(error.status, 404);

        // 超管看得到两条（证明上一条 404 来自范围，而不是数据不存在）
        let mut all_query = fx.query();
        all_query.workshop_dept_id = None;
        let all = handler::sheet_of(&fx.super_principal(), all_query)
            .await
            .expect("超管查");
        assert!(all.total >= 2, "超管至少能看到这两条；实际 {}", all.total);
        assert!(
            all.list.iter().any(|row| row.object_id == mine.id.to_string()),
            "本部门批次必须在超管视图里"
        );
        assert!(
            all.list.iter().any(|row| row.object_id == theirs.id.to_string()),
            "另一车间的批次必须在超管视图里（证明上一条 404 来自数据范围）"
        );

        fx.cleanup(&[&mine, &theirs]).await;
    }

    // ==================== 7. 明细与表行对得上 ====================

    #[tokio::test]
    async fn detail_lines_add_up_to_the_sheet_row() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let a = fx.object(fx.dept, "OPEN", Some("SO-M502-D"), "D1").await;
        fx.collect(&a, PERIOD, "MATERIAL", "100", "2031-07-05", 1).await;
        fx.collect(&a, PERIOD, "LABOR", "30", "2031-07-10", 1).await;
        fx.collect(&a, PERIOD, "OVERHEAD", "20", "2031-07-11", 1).await;
        fx.collect(&a, OTHER_PERIOD, "MATERIAL", "999", "2031-08-05", 1).await;

        let principal = fx.super_principal();
        let sheet = handler::sheet_of(&principal, fx.query()).await.expect("查归集表");
        let row = row_of(&sheet, &a);

        let detail = handler::detail_of(
            &principal,
            &a.id.to_string(),
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect("查明细");

        assert_eq!(detail.batch.object_id, a.id.to_string());
        assert_eq!(detail.lines.len(), 3, "只含本期三笔（别期的 999 不算）");
        // 排序：料 → 工 → 费
        assert_eq!(
            detail
                .lines
                .iter()
                .map(|line| line.element.as_str())
                .collect::<Vec<_>>(),
            vec!["MATERIAL", "LABOR", "OVERHEAD"]
        );
        assert_eq!(
            detail.lines[0].element_label, "直接材料",
            "要素中文名由后端给，前端不再抄映射"
        );
        assert_eq!(detail.lines[0].source_type_label, "手工录入");
        assert_eq!(detail.lines[0].qty.as_deref(), Some("10.0000"), "料行带数量");
        assert_eq!(detail.lines[1].qty, None, "工行不许有数量（一张表两种语义的边界）");
        assert_eq!(detail.lines[0].material_code.as_deref(), Some(fx.product_code.as_str()));

        // 明细合计 == 表行金额（同一份 biz_cost_collect，两个粒度）
        assert_eq!(detail.totals.mat, row.amounts.mat);
        assert_eq!(detail.totals.labor, row.amounts.labor);
        assert_eq!(detail.totals.oh, row.amounts.oh);
        assert_eq!(detail.totals.total, row.amounts.total);

        fx.cleanup(&[&a]).await;
    }

    // ==================== 8. 非法入参 / 不存在 ====================

    #[tokio::test]
    async fn detail_rejects_bad_period_and_unknown_batch() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let principal = fx.super_principal();

        // 期间非法 ⇒ 400（先校验参数，不先探 404）
        let error = handler::detail_of(
            &principal,
            &Uuid::now_v7().to_string(),
            SheetDetailQuery {
                period_code: Some("2031-7".to_string()),
            },
        )
        .await
        .expect_err("期间非法必须 400");
        assert_eq!(error.status, 400);

        // 期间缺失 ⇒ 400
        let error = handler::detail_of(&principal, &Uuid::now_v7().to_string(), SheetDetailQuery::default())
            .await
            .expect_err("期间必填");
        assert_eq!(error.status, 400);

        // 批次不存在 ⇒ 404
        let error = handler::detail_of(
            &principal,
            &Uuid::now_v7().to_string(),
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect_err("不存在的批次必须 404");
        assert_eq!(error.status, 404);

        // 路径参数不是 uuid ⇒ 404
        let error = handler::detail_of(
            &principal,
            "CB-M502-NOT-A-UUID",
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect_err("非 uuid 路径参数必须 404");
        assert_eq!(error.status, 404);

        // 已软删的批次 ⇒ 404（列表里也没有，归集额也不进合计）
        let gone = fx.object(fx.dept, "OPEN", Some("SO-M502-GONE"), "G1").await;
        fx.collect(&gone, PERIOD, "MATERIAL", "10", "2031-07-05", 1).await;
        db().exec_sql(
            "UPDATE biz_cost_object SET deleted_at = now() WHERE id = ?::uuid".to_string(),
            vec![Value::from(&gone.id)],
        )
        .await
        .expect("软删夹具");
        let res = handler::sheet_of(&principal, fx.query()).await.expect("查归集表");
        assert_eq!(res.total, 0, "软删的批次不该出现在表里");
        assert_eq!(res.totals.amounts.total, "0.0000", "软删批次的归集也不该进合计");
        let error = handler::detail_of(
            &principal,
            &gone.id.to_string(),
            SheetDetailQuery {
                period_code: Some(PERIOD.to_string()),
            },
        )
        .await
        .expect_err("软删批次必须 404");
        assert_eq!(error.status, 404);

        // 列表入参非法 ⇒ 400
        let mut query = fx.query();
        query.status = Some("SETTLED".to_string());
        let error = handler::sheet_of(&principal, query)
            .await
            .expect_err("状态值域外必须 400");
        assert_eq!(error.status, 400);

        fx.cleanup(&[&gone]).await;
    }

    // ==================== 9. 收尾：清场不留残留 ====================

    /// 清场后按 id / 编码回查：归集行 0、池行 0、批次 0、部门 0 —— 本文件不许在演练库里留垃圾。
    ///
    /// 这条用例的「牙齿」在于它自己造数据再删：删漏任何一张表，断言立刻红；
    /// 而且它先断言「清场前确实看得到」（否则「清场后 0 行」是空断言）。
    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let fx = Fixture::build().await;
        let object = fx.object(fx.dept, "OPEN", Some("SO-M502-ZZ"), "Z1").await;
        fx.collect(&object, PERIOD, "MATERIAL", "10", "2031-07-05", 1).await;
        fx.pool_row(PERIOD, "5").await;
        let principal = fx.super_principal();
        let before = handler::sheet_of(&principal, fx.query()).await.expect("清场前仍有数据");
        assert_eq!(before.total, 1, "清场前应能看到这条夹具（否则本用例是空断言）");

        fx.cleanup(&[&object]).await;

        let collects: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect WHERE cost_object_id = ?::uuid",
                vec![Value::from(&object.id)],
            )
            .await
            .expect("数归集行");
        assert_eq!(collects, 0, "归集行没清干净");
        let pool: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_collect \
                  WHERE cost_object_id IS NULL AND source_type = 'DEPRECIATION' AND dept_id = ?::uuid",
                vec![Value::from(&fx.dept)],
            )
            .await
            .expect("数池行");
        assert_eq!(pool, 0, "池行没清干净");
        let objects: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_cost_object WHERE id = ?::uuid",
                vec![Value::from(&object.id)],
            )
            .await
            .expect("数批次");
        assert_eq!(objects, 0, "批次没清干净");
        let depts: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM sys_dept WHERE dept_code = ? OR dept_code = ?",
                vec![
                    Value::Text(fx.dept_code.clone()),
                    Value::Text(fx.other_dept_code.clone()),
                ],
            )
            .await
            .expect("数部门");
        assert_eq!(depts, 0, "夹具部门没清干净");

        // 清场后归集表回到 0 行 0 元（本用例车间已无数据）
        let after = handler::sheet_of(&principal, fx.query()).await.expect("清场后查");
        assert_eq!(after.total, 0);
        assert_eq!(after.totals.amounts.total, "0.0000");
    }
}
