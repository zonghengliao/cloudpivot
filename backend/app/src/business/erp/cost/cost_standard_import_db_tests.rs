//! 标准成本卡 **xlsx 批量导入**的真库用例（`ERP_DB_URL` 开关，M5-09）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m509_import \
//!   cargo test --lib cost_standard_import_db -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（M5-09「标准成本：维护页 CRUD · 批量导入」）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`import_creates_one_version_per_group`] | 一组 = 一版：料 = Σ明细、工 = round(工时×费率,6)、生效区间 = 整期、明细行号从 1 起；无 BOM 的组也建得出来 |
//! | 2 | [`import_rejects_unknown_material_and_writes_nothing`] | **全有或全无**：坏一组 ⇒ 好的一组也不入库（库里 0 行） |
//! | 3 | [`import_rejects_period_already_covered`] | 期间已被既有启用版本占用 ⇒ 整批拒绝，报文给三条出路，库里不多行 |
//! | 4 | [`import_two_adjacent_periods_makes_two_versions`] | 同一物料相邻两期 ⇒ 两版，版本号 1 / 2（端点相接放行） |
//! | 5 | [`import_rejects_service_material`] | 服务类物料不能建标准成本（与单条录入同一条规则） |
//! | 6 | [`write_all_rolls_back_every_group_on_failure`] | **事务边界**：合成一组必然撞重叠的输入 ⇒ 前一组写进去的也回滚（库里 0 行） |
//!
//! # 夹具纪律
//!
//! · 物料用 `test_seed::ensure_material_kind` 造，**编码自己给**（Excel 里人写的就是编码），
//!   带进程内唯一后缀，跑完只删本卡的标准成本行（硬删，明细随卡头 CASCADE）；
//! · 期间一律「相对今天」算（`period_of_date(today + 30 天)`）：写死 `2026-11` 过几个月
//!   就会变成「已生效历史期」，`已生效锁改` 之类的口径会跟着变；
//! · 每个用例自己清场，不按前缀扫全库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::erp::cost::cost_standard_dto::{self as dto, StandardCreateReq, StandardItemReq};
    use crate::business::erp::cost::cost_standard_handler as handler;
    use crate::business::erp::cost::cost_standard_import::{
        self as import, GroupDraft, HeadNumbers, ItemDraft, RawRow, RefIndex, StandardImportRes,
    };
    use crate::business::erp::cost::cost_standard_model::MaterialRef;
    use nik::prelude::chrono::{Duration, FixedOffset, NaiveDate, Utc};
    use nik::prelude::rust_decimal::Decimal;
    use std::collections::HashMap;
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

    /// 某天所在期间（`yyyy-MM`）。
    fn period(offset_days: i64) -> String {
        dto::period_of_date(day(offset_days))
    }

    /// 唯一后缀。
    fn uniq() -> String {
        crate::test_seed::uniq_suffix()
    }

    /// 造物料并**返回它的编码**（Excel 里写的就是它）。
    async fn fixture_material(prefix: &str, material_type: &str) -> String {
        let code = format!("M509{prefix}-{}", crate::test_seed::last_chars(&uniq(), 10));
        crate::test_seed::ensure_material_kind(&code, &format!("M5-09 夹具 {prefix} {code}"), material_type).await;
        code
    }

    /// 清场：删本卡的标准成本行（明细随卡头 CASCADE）；物料等共享基线不删。
    async fn cleanup(materials: &[Uuid]) {
        for material_id in materials {
            if let Err(err) = db()
                .exec_sql(
                    "DELETE FROM biz_cost_standard WHERE material_id = ?::uuid".to_string(),
                    vec![Value::from(material_id)],
                )
                .await
            {
                println!("清场失败（不影响断言）：{err}");
            }
        }
    }

    /// 按编码取物料 id（清场用）。
    async fn material_id_of(code: &str) -> Uuid {
        let rows = db()
            .find_with_params(
                "SELECT id FROM biz_material WHERE material_code = ? AND deleted_at IS NULL",
                vec![Value::Text(code.to_string())],
            )
            .await
            .expect("按编码查物料");
        rows.first()
            .map(|row| row.get_typed::<Uuid>("id").expect("id"))
            .unwrap_or_else(|| panic!("物料 {code} 不存在"))
    }

    /// 某物料的全部存活版本（版本号 / 生效区间文本 / 三个金额文本 / 状态 / 明细行数）。
    struct HeadRow {
        version_no: i32,
        from: String,
        to: Option<String>,
        material_cost: String,
        labor_cost: String,
        overhead_cost: String,
        total_cost: String,
        status: i16,
        items: i64,
    }

    async fn heads_of(material: Uuid) -> Vec<HeadRow> {
        let rows = db()
            .find_with_params(
                "SELECT t.version_no, t.effective_from::text AS from_text, t.effective_to::text AS to_text, \
                        t.std_material_cost::text AS material_cost, t.std_labor_cost::text AS labor_cost, \
                        t.std_overhead_cost::text AS overhead_cost, t.std_total_cost::text AS total_cost, \
                        t.status, \
                        (SELECT count(*) FROM biz_cost_standard_item i WHERE i.standard_id = t.id)::bigint AS items \
                   FROM biz_cost_standard t \
                  WHERE t.material_id = ?::uuid AND t.deleted_at IS NULL \
                  ORDER BY t.version_no ASC",
                vec![Value::from(&material)],
            )
            .await
            .expect("查标准成本版本");
        rows.iter()
            .map(|row| HeadRow {
                version_no: row.get_typed("version_no").expect("version_no"),
                from: row.get_typed("from_text").expect("from"),
                to: row.get_typed("to_text").ok(),
                material_cost: row.get_typed("material_cost").expect("material_cost"),
                labor_cost: row.get_typed("labor_cost").expect("labor_cost"),
                overhead_cost: row.get_typed("overhead_cost").expect("overhead_cost"),
                total_cost: row.get_typed("total_cost").expect("total_cost"),
                status: row.get_typed("status").expect("status"),
                items: row.get_typed("items").expect("items"),
            })
            .collect()
    }

    /// 一行导入数据（卡头三要素固定：工时 2.5 / 费率 31.111111 / 制造费用 12.345678）。
    fn row(line: u32, material: &str, period: &str, component: &str, qty: &str, price: &str) -> RawRow {
        RawRow {
            line,
            material_code: material.to_string(),
            period_code: period.to_string(),
            std_labor_hours: "2.5".to_string(),
            std_labor_rate: "31.111111".to_string(),
            std_overhead_cost: "12.345678".to_string(),
            component_code: component.to_string(),
            std_qty: qty.to_string(),
            std_price: price.to_string(),
            remark: "M5-09 真库用例".to_string(),
        }
    }

    /// 把行写成真 xlsx 再走完整导入链路（解析 → 校验 → 单事务写入）。
    async fn import_rows(rows: &[RawRow]) -> StandardImportRes {
        let bytes = import::build_rows_xlsx(rows).expect("写 xlsx");
        import::import_standards(&bytes, None)
            .await
            .expect("导入不该抛 HTTP 错（结构错才会）")
    }

    /// 断言「一条都没入库」时给出可读的报文。
    fn assert_nothing_written(res: &StandardImportRes) {
        assert_eq!(res.success, 0, "全有或全无：一行都不该入库");
        assert_eq!(res.versions_created, 0, "全有或全无：一版都不该建");
        assert!(res.failed > 0, "必须报出有错的行");
        assert!(!res.rows.is_empty(), "必须给出逐行错误");
    }

    // ==================== 用例 ====================

    /// 1. 一组 = 一版：三要素算数 / 生效区间 = 整期 / 明细行号 / 无 BOM 的组也建得出来。
    #[tokio::test]
    async fn import_creates_one_version_per_group() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let empty_product = fixture_material("FS", "FINISHED").await;
        let comp_a = fixture_material("RA", "RAW").await;
        let comp_b = fixture_material("RB", "RAW").await;
        let period_code = period(30);
        let (start, end) = dto::period_window(&period_code).expect("期间");

        let res = import_rows(&[
            row(2, &product, &period_code, &comp_a, "3", "5.5"),
            row(3, &product, &period_code, &comp_b, "2", "0.333333"),
            // 第二版没有材料耗用：三列全空是合法口径
            row(4, &empty_product, &period_code, "", "", ""),
        ])
        .await;

        assert_eq!(res.success, 3, "3 行数据全部入库：{:?}", res.rows);
        assert_eq!(res.versions_created, 2, "两组 = 两版");
        assert_eq!(res.failed, 0);

        let product_id = material_id_of(&product).await;
        let heads = heads_of(product_id).await;
        assert_eq!(heads.len(), 1);
        let head = &heads[0];
        let end_text = end.to_string();
        assert_eq!(head.version_no, 1, "第一版版本号 = 1");
        assert_eq!(head.from, start.to_string(), "生效起始日 = 期初");
        assert_eq!(head.to.as_deref(), Some(end_text.as_str()), "停止日 = 期末");
        assert_eq!(head.status, dto::STATUS_ACTIVE, "导入默认启用");
        assert_eq!(head.material_cost, "17.166666", "3 × 5.5 + 2 × 0.333333");
        assert_eq!(
            head.labor_cost, "77.777778",
            "2.5 × 31.111111 = 77.7777775 → 半数远离零"
        );
        assert_eq!(head.overhead_cost, "12.345678");
        assert_eq!(head.total_cost, "107.290122", "总 = 料 + 工 + 费（库层 CHECK 同口径）");
        assert_eq!(head.items, 2, "标准 BOM 两行");

        // 明细行号从 1 起、带耗用物料编码（约定 13：*_id 必须带 *_name）
        let detail = handler::detail_standard(&heads_id(product_id).await)
            .await
            .expect("详情");
        assert_eq!(detail.items.len(), 2);
        assert_eq!(detail.items[0].line_no, 1);
        assert_eq!(detail.items[1].line_no, 2);
        assert_eq!(detail.items[0].std_amount, "16.500000");
        assert_eq!(
            detail.items[1].std_amount, "0.666666",
            "2 × 0.333333 = 0.666666（round 到 6 位）"
        );
        assert!(
            detail.items[0]
                .component_material_code
                .as_deref()
                .is_some_and(|code| code == comp_a),
            "明细带耗用物料编码：{:?}",
            detail.items[0]
        );

        // 无 BOM 的那一版：料 = 0，工 / 费照算
        let empty_id = material_id_of(&empty_product).await;
        let empty_heads = heads_of(empty_id).await;
        assert_eq!(empty_heads.len(), 1);
        assert_eq!(empty_heads[0].material_cost, "0.000000");
        assert_eq!(empty_heads[0].items, 0);
        assert_eq!(empty_heads[0].total_cost, "90.123456", "0 + 77.777778 + 12.345678");

        cleanup(&[product_id, empty_id]).await;
        assert!(heads_of(product_id).await.is_empty(), "清场零残留");
    }

    /// 2. 全有或全无：坏一组 ⇒ 好的一组也不入库。
    #[tokio::test]
    async fn import_rejects_unknown_material_and_writes_nothing() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let comp = fixture_material("RA", "RAW").await;
        let period_code = period(30);

        let res = import_rows(&[
            // 好的一组
            row(2, &product, &period_code, &comp, "1", "2"),
            // 坏的一组：物料编码不存在
            row(3, "M509-NOT-EXIST-XXXX", &period_code, &comp, "1", "2"),
        ])
        .await;

        assert_nothing_written(&res);
        assert_eq!(res.failed, 1, "只有第 3 行有错");
        assert_eq!(res.rows[0].line, 3);
        assert_eq!(res.rows[0].column, import::HEADER_MATERIAL_CODE);
        assert!(res.rows[0].message.contains("不存在"), "{}", res.rows[0].message);

        let product_id = material_id_of(&product).await;
        assert!(
            heads_of(product_id).await.is_empty(),
            "好的一组也一条都不许入库（半批导入会让标准成本少一版而没人发现）"
        );
        cleanup(&[product_id]).await;
    }

    /// 3. 期间已被既有启用版本占用 ⇒ 整批拒绝，报文给三条出路。
    #[tokio::test]
    async fn import_rejects_period_already_covered() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let product_id = material_id_of(&product).await;
        let period_code = period(30);

        // 先手工建一版覆盖这个期间
        let existing = handler::create_standard(
            None,
            StandardCreateReq {
                material_id: Some(product_id.to_string()),
                period_code: Some(period_code.clone()),
                std_labor_hours: Some(Decimal::from_str("1").expect("工时")),
                std_labor_rate: Some(Decimal::from_str("10").expect("费率")),
                std_overhead_cost: Some(Decimal::ZERO),
                items: Some(Vec::<StandardItemReq>::new()),
                remark: Some("既有版本".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("先建一版");

        let res = import_rows(&[row(2, &product, &period_code, "", "", "")]).await;
        assert_nothing_written(&res);
        assert_eq!(res.rows[0].column, import::HEADER_PERIOD);
        assert!(
            res.rows[0].message.contains("已经有启用版本 v1"),
            "报文要点名既有版本：{}",
            res.rows[0].message
        );
        assert!(
            res.rows[0].message.contains("三条出路"),
            "报文要给改法：{}",
            res.rows[0].message
        );

        let heads = heads_of(product_id).await;
        assert_eq!(heads.len(), 1, "库里还是只有既有那一版");
        assert_eq!(heads[0].version_no, existing.head.version_no);

        cleanup(&[product_id]).await;
    }

    /// 4. 同一物料相邻两期 ⇒ 两版，版本号 1 / 2（端点相接放行）。
    #[tokio::test]
    async fn import_two_adjacent_periods_makes_two_versions() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let comp = fixture_material("RA", "RAW").await;
        let first = period(30);
        let second = period(70);

        let res = import_rows(&[
            row(2, &product, &first, &comp, "1", "2"),
            row(3, &product, &second, &comp, "2", "2"),
        ])
        .await;

        assert_eq!(res.success, 2, "{:?}", res.rows);
        assert_eq!(res.versions_created, 2);

        let product_id = material_id_of(&product).await;
        let heads = heads_of(product_id).await;
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].version_no, 1);
        assert_eq!(heads[1].version_no, 2, "版本号在同物料内递增");
        assert_eq!(heads[0].material_cost, "2.000000");
        assert_eq!(heads[1].material_cost, "4.000000", "新版用量 2 × 2");

        cleanup(&[product_id]).await;
    }

    /// 5. 服务类物料不能建标准成本（与单条录入同一条规则，报文点名 SERVICE）。
    #[tokio::test]
    async fn import_rejects_service_material() {
        if !ensure_db().await {
            return;
        }
        let service = fixture_material("SV", "SERVICE").await;
        let period_code = period(30);

        let res = import_rows(&[row(2, &service, &period_code, "", "", "")]).await;
        assert_nothing_written(&res);
        assert!(
            res.rows[0].message.contains("SERVICE"),
            "报文要点名服务类：{}",
            res.rows[0].message
        );

        let service_id = material_id_of(&service).await;
        assert!(heads_of(service_id).await.is_empty());
        cleanup(&[service_id]).await;
    }

    /// 6. 事务边界：一组必然撞重叠 ⇒ 前一组写进去的也回滚。
    ///
    /// 校验层（`validate_refs`）看的是**库里的既有版本**，看不到「同一文件里另一组」；
    /// 本用例用两个指向同一物料同一期间的合成组（`scan_groups` 不会产出这种输入，
    /// 这里直接构造）把「第二组撞重叠 ⇒ 整批回滚」这条路径逼出来 —— 这正是
    /// `write_all` 一个事务包住所有组的意义。
    #[tokio::test]
    async fn write_all_rolls_back_every_group_on_failure() {
        if !ensure_db().await {
            return;
        }
        let product = fixture_material("FG", "FINISHED").await;
        let product_id = material_id_of(&product).await;
        let period_code = period(30);
        let (start, end) = dto::period_window(&period_code).expect("期间");

        let group = |line: u32| GroupDraft {
            material_code: product.clone(),
            period_code: period_code.clone(),
            window: (start, end),
            lines: vec![line],
            head: HeadNumbers {
                hours: Decimal::from_str("2.5").expect("工时"),
                rate: Decimal::from_str("31.111111").expect("费率"),
                overhead: Decimal::from_str("12.345678").expect("费用"),
                remark: None,
            },
            items: Vec::<ItemDraft>::new(),
        };
        let groups = vec![group(2), group(3)];
        let mut materials = HashMap::new();
        materials.insert(
            product.clone(),
            (
                product_id,
                MaterialRef {
                    material_code: product.clone(),
                    material_name: "M5-09 夹具".to_string(),
                    material_type: "FINISHED".to_string(),
                    status: 1,
                },
            ),
        );
        let refs = RefIndex { materials };

        let err = import::write_all(&groups, &refs, None)
            .await
            .expect_err("第二组与第一组同窗口 ⇒ 必须撞重叠");
        assert_eq!(err.code, dto::CODE_OVERLAP, "撞的是 66108：{}", err.msg);
        assert!(err.msg.contains("整批已回滚"), "报文要说清后果：{}", err.msg);
        assert!(err.msg.contains(&product), "报文要点名物料：{}", err.msg);

        assert!(
            heads_of(product_id).await.is_empty(),
            "第一组已经写进去的也必须回滚（事务边界在导入侧）"
        );
        cleanup(&[product_id]).await;
    }

    /// 取某物料的唯一版本 id（详情用）。
    async fn heads_id(material: Uuid) -> String {
        let rows = db()
            .find_with_params(
                "SELECT id::text AS id FROM biz_cost_standard \
                  WHERE material_id = ?::uuid AND deleted_at IS NULL ORDER BY version_no ASC LIMIT 1",
                vec![Value::from(&material)],
            )
            .await
            .expect("查版本 id");
        rows.first()
            .map(|row| row.get_typed::<String>("id").expect("id"))
            .unwrap_or_else(|| panic!("物料 {material} 没有版本"))
    }
}
