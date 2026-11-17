// ============================================================================
// framework_agreement_db_tests.rs —— 框架协议表结构（M4-07）的真库用例
//
// 卡：t_7eb1c440（M4-07【M4·采购】框架协议：表结构（有效期 + 价格条款 + 数量上限））
//
// # 覆盖什么（每条都对应卡面 / V0450 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `schema_shape_matches_migration` | 两张表在、列数 29/13、外键 ON DELETE CASCADE、8 个索引都在 |
// | `validity_window_ordered_and_effectiveness_is_time_based` | 卡面「有效期」：起 > 止被拒；「是否生效」只由 status + 起止日现算（不落 EXPIRED 状态） |
// | `agreement_no_format_is_enforced` | 协议号只能是 `FWA-yyyyMMdd-000001`（取号形状写在库层） |
// | `status_domain_is_three_values_and_submitted_needs_instance` | 状态只有 DRAFT/SUBMITTED/APPROVED；EXPIRED/TERMINATED 插不进；SUBMITTED 无审批实例被拒；APPROVED 无实例合法 |
// | `price_terms_defaults_and_floating_needs_rule` | 卡面「价格条款」：FIXED/CNY/不含税/容差 0 的默认值；FLOATING 无（或空白）调价规则被拒；币种形状、容差 0–1 |
// | `quantity_caps_null_means_unlimited_and_zero_rejected` | 卡面「数量上限」：NULL = 不限、写 0 被拒、写了就必须 > 0 |
// | `material_scope_is_unique_per_agreement` | 卡面「物料范围」：同协议同物料第二行被拒；换协议可同物料；行号单内唯一 |
// | `executed_may_exceed_cap_and_totals_recalc_rolls_up` | 已执行量超上限必须写得进去（超限拒绝属 M4-08）；`recalc_totals` 把明细合计滚到单头；负值被拒 |
// | `delete_agreement_cascades_items` | 删单头连带删物料范围（不留孤儿行） |
// | `next_agreement_no_uses_fwa_seq` | 取号行可用：连续两个号递增且能过库层单号 CHECK |
// | `supplier_snapshot_and_live_name_coexist` | 同一供应商可有多份协议（库层不做唯一）；快照与实时现名并存（约定 13） |
// | `zz_teardown_purges_fixtures` | 清场：`M407-` 夹具 0 残留 |
//
// # 为什么夹具走裸 SQL 而不走「建单接口」
//
// 本卡**没有建单接口**（M4-07 只交表结构，CRUD 与订单引用属 M4-08 之后）。
// 要验的正是库层约束本身，所以夹具直接 INSERT，用 `assert_rejected()` 断言坏数据被
// CHECK / UNIQUE / FK 拒掉。主数据（供应商 / 物料 / 单位）一律走 `crate::test_seed::ensure_*`
// （门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m407_drill" cargo test --lib \
//   business::erp::purchase::framework_agreement_db_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::framework_agreement_model as model;
use crate::test_seed;

/// 夹具单号池：`9xxxxx` 段 + 1999-01-01 的日期段，绝不与真实取号（当天日期）撞号。
const NO_BASE: &str = "FWA-19990101-9";
/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const NAME_PREFIX: &str = "M407-";
/// 夹具供应商编码。
const SUPPLIER_CODE: &str = "M407-SUP";
/// 夹具物料编码。
const MATERIAL_CODE: &str = "M407-MAT";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：只删本卡的协议（物料范围靠 `ON DELETE CASCADE` 跟着走）。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM {0} WHERE agreement_name LIKE '{NAME_PREFIX}%'",
            model::AGREEMENT_TABLE
        ),
        vec![],
    )
    .await;
}

/// 夹具单号：`FWA-19990101-9NNNNN`（6 位流水，满足 `ck_biz_fwa_no_fmt`）。
///
/// 用例内**每插一张协议都要一个新号**：`uk_biz_framework_agreement_no` 是存活唯一，
/// 两张夹具协议用同一个号会直接撞唯一索引（实测踩过：`fixture_no()` 统一发号才算安全）。
fn agreement_no(seq: u32) -> String {
    format!("{NO_BASE}{seq:05}")
}

/// 夹具发号器：进程内单调递增 —— 同一用例里插入多张协议时各拿一个新号。
fn fixture_no() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(100);
    agreement_no(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// 一家夹具供应商。
async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUPPLIER_CODE, "框架协议夹具供应商").await
}

/// 一个夹具物料。
async fn material() -> Uuid {
    test_seed::ensure_material(MATERIAL_CODE, "框架协议夹具物料").await
}

/// 一张协议（裸 SQL；本卡没有建单接口）。
struct NewAgreement<'a> {
    no: &'a str,
    name: String,
    supplier: Uuid,
    start: &'a str,
    end: &'a str,
    status: &'a str,
    instance: Option<Uuid>,
    price_mode: &'a str,
    adjust_rule: Option<&'a str>,
}

/// 插一张协议，返回 id。
async fn insert_agreement(head: &NewAgreement<'_>) -> Uuid {
    let id = Uuid::now_v7();
    let no = if head.no.is_empty() {
        fixture_no()
    } else {
        head.no.to_string()
    };
    exec(
        "INSERT INTO biz_framework_agreement \
            (id, agreement_no, agreement_name, supplier_id, supplier_name, \
             start_date, end_date, sign_date, price_mode, price_adjust_rule, \
             status, approval_instance_id) \
         VALUES (?, ?::text, ?::text, ?::uuid, NULLIF(?::text,''), \
                 ?::text::date, ?::text::date, ?::text::date, \
                 ?::text, NULLIF(?::text,''), ?::text, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::Text(no),
            Value::Text(head.name.clone()),
            Value::from(&head.supplier),
            Value::Text(format!("{NAME_PREFIX}供应商快照")),
            Value::Text(head.start.to_string()),
            Value::Text(head.end.to_string()),
            Value::Text(head.start.to_string()),
            Value::Text(head.price_mode.to_string()),
            head.adjust_rule
                .map(|r| Value::Text(r.to_string()))
                .unwrap_or(Value::Null),
            Value::Text(head.status.to_string()),
            Value::Text(head.instance.map(|id| id.to_string()).unwrap_or_default()),
        ],
    )
    .await;
    id
}

/// 插一行物料范围，返回 id。
async fn insert_item(agreement_id: Uuid, line_no: i32, material_id: Uuid, price: &str, max_qty: Option<&str>) -> Uuid {
    let unit = test_seed::ensure_unit().await;
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_framework_agreement_item \
            (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty) \
         VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, ?::text::numeric, 0.13, ?::text::numeric)",
        vec![
            Value::from(&id),
            Value::from(&agreement_id),
            Value::I32(line_no),
            Value::from(&material_id),
            Value::from(&unit),
            Value::Text(price.to_string()),
            match max_qty {
                Some(qty) => Value::Text(qty.to_string()),
                None => Value::Null,
            },
        ],
    )
    .await;
    id
}

/// 期望被库层拒掉的写入：返回错误文本（断言里看得到是哪条约束拦的）。
async fn assert_rejected(sql: &str, params: Vec<Value>, why: &str) -> String {
    // SCOPE-EXEMPT: 用例直插夹具表，不涉及数据范围。
    match db().execute_with_params(sql, params).await {
        Ok(_) => panic!("{why}：库层居然接受了这条写入（约束失效）"),
        Err(error) => error.to_string(),
    }
}

/// 单列计数（`SELECT count(*)`）。
async fn count_of(sql: &str) -> i64 {
    db().query_sql(sql.to_string(), vec![])
        .await
        .expect("查计数")
        .first()
        .and_then(|row| row.get_typed::<i64>("n").ok())
        .unwrap_or(0)
}

// ==================== 用例 ====================

/// 表形状：两表在、列数、外键级联、索引（库层与 V0450 逐条对齐）。
#[tokio::test]
async fn schema_shape_matches_migration() {
    let Some(_guard) = setup().await else { return };

    let tables = count_of(
        "SELECT count(*) AS n FROM pg_class c JOIN pg_namespace n2 ON n2.oid = c.relnamespace \
          WHERE n2.nspname = 'public' AND c.relkind = 'r' \
            AND c.relname IN ('biz_framework_agreement','biz_framework_agreement_item')",
    )
    .await;
    assert_eq!(tables, 2, "V0450 的两张表必须都在");

    let head_cols = count_of(
        "SELECT count(*) AS n FROM information_schema.columns \
          WHERE table_schema='public' AND table_name='biz_framework_agreement'",
    )
    .await;
    assert_eq!(
        head_cols, 29,
        "单头 29 列（有效期 3 + 价格条款 5 + 上限/已执行 4 + 主数据 4 + 审计 5 …）"
    );

    let item_cols = count_of(
        "SELECT count(*) AS n FROM information_schema.columns \
          WHERE table_schema='public' AND table_name='biz_framework_agreement_item'",
    )
    .await;
    assert_eq!(item_cols, 13, "明细 13 列");

    // 外键必须是 CASCADE（删单头留明细 = 孤儿行）
    let delete_rule: String = db()
        .query_sql(
            "SELECT rc.delete_rule \
               FROM information_schema.referential_constraints rc \
              WHERE rc.constraint_schema='public' AND rc.constraint_name='fk_biz_fwai_agreement'"
                .to_string(),
            vec![],
        )
        .await
        .expect("查外键")
        .first()
        .and_then(|row| row.get_typed("delete_rule").ok())
        .expect("外键 fk_biz_fwai_agreement 必须存在");
    assert_eq!(delete_rule, "CASCADE");

    let indexes = count_of(
        "SELECT count(DISTINCT indexname) AS n FROM pg_indexes \
          WHERE schemaname='public' AND indexname IN \
            ('uk_biz_framework_agreement_no','idx_biz_fwa_supplier','idx_biz_fwa_period','idx_biz_fwa_status', \
             'uk_biz_fwai_material','uk_biz_fwai_line','idx_biz_fwai_material')",
    )
    .await;
    assert_eq!(indexes, 7, "V0450 的 7 个索引一个都不能少");

    // 编号唯一索引必须是部分索引（软删后可重建同号协议）
    let partial = count_of(
        "SELECT count(*) AS n FROM pg_indexes \
          WHERE schemaname='public' AND indexname='uk_biz_framework_agreement_no' \
            AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'",
    )
    .await;
    assert_eq!(partial, 1, "协议号唯一索引必须带软删条件");

    println!("[M4-07] 表形状：2 表 / 29+13 列 / FK CASCADE / 7 索引 / 部分唯一 ✓");
}

/// 卡面「有效期」：起止必须同序；是否生效只看 status + 起止日（不落 EXPIRED 状态）。
#[tokio::test]
async fn validity_window_ordered_and_effectiveness_is_time_based() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 起 > 止 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date) \
         VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date)",
        vec![
            Value::Text(agreement_no(1)),
            Value::Text(format!("{NAME_PREFIX}起止颠倒")),
            Value::from(&sup),
            Value::Text("2026-12-31".to_string()),
            Value::Text("2026-01-01".to_string()),
        ],
        "有效期起晚于止",
    )
    .await;
    assert!(
        err.contains("ck_biz_fwa_period"),
        "应命中 ck_biz_fwa_period，实际：{err}"
    );

    // 同一天起止（单日协议）合法
    let single_day = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}单日协议"),
        supplier: sup,
        start: "2026-06-01",
        end: "2026-06-01",
        status: "APPROVED",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;

    // 读回来：起止日原样、状态 APPROVED；生效判据按 model 的纯函数（与库层同一份口径）
    let row = model::find_agreement(db(), single_day)
        .await
        .expect("读协议")
        .expect("协议必须读得到");
    assert_eq!(row.start_date, date("2026-06-01"));
    assert_eq!(row.end_date, date("2026-06-01"));
    assert!(row.is_effective_on(date("2026-06-01")), "单日协议当天必须生效");
    assert!(!row.is_effective_on(date("2026-06-02")), "单日协议次日必须失效");
    assert!(
        !row.is_effective_on(date("2025-06-01")),
        "未开始的协议不许生效（有效期是时间的函数，不是状态列）"
    );

    println!("[M4-07] 有效期：起 <= 止被库层钉住；生效判据 = APPROVED 且落在起止日之间（含边界）✓");
}

/// 协议号形状写在库层（`ck_biz_fwa_no_fmt`），取号规则一改坏会在这里先炸。
#[tokio::test]
async fn agreement_no_format_is_enforced() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    for bad in [
        "FA-20260101-000001",
        "FWA-2026-1",
        "FWA-20260101-1",
        "FWX-20260101-000001",
    ] {
        assert_rejected(
            "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date) \
             VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date)",
            vec![
                Value::Text(bad.to_string()),
                Value::Text(format!("{NAME_PREFIX}坏单号 {bad}")),
                Value::from(&sup),
                Value::Text("2026-01-01".to_string()),
                Value::Text("2026-12-31".to_string()),
            ],
            &format!("单号形状 {bad}"),
        )
        .await;
    }

    // 正样本：合法形状能插
    let ok = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}合法单号"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    assert!(model::find_agreement(db(), ok).await.expect("读").is_some());

    println!("[M4-07] 协议号：只认 FWA-yyyyMMdd-000001（6 位流水），其它形状全被库层拒 ✓");
}

/// 状态域只有三个；SUBMITTED 必须有审批实例；APPROVED 无实例合法；死状态插不进。
#[tokio::test]
async fn status_domain_is_three_values_and_submitted_needs_instance() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    for dead in ["EXPIRED", "TERMINATED", "CLOSED"] {
        let err = assert_rejected(
            "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status) \
             VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, ?::text)",
            vec![
                Value::Text(agreement_no(2)),
                Value::Text(format!("{NAME_PREFIX}死状态 {dead}")),
                Value::from(&sup),
                Value::Text("2026-01-01".to_string()),
                Value::Text("2026-12-31".to_string()),
                Value::Text(dead.to_string()),
            ],
            &format!("状态域外的 {dead}"),
        )
        .await;
        assert!(
            err.contains("ck_biz_fwa_status"),
            "应命中 ck_biz_fwa_status，实际：{err}"
        );
    }

    // SUBMITTED 没实例 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status) \
         VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, 'SUBMITTED')",
        vec![
            Value::Text(agreement_no(3)),
            Value::Text(format!("{NAME_PREFIX}审批中无实例")),
            Value::from(&sup),
            Value::Text("2026-01-01".to_string()),
            Value::Text("2026-12-31".to_string()),
        ],
        "SUBMITTED 没有审批实例",
    )
    .await;
    assert!(
        err.contains("ck_biz_fwa_instance"),
        "应命中 ck_biz_fwa_instance，实际：{err}"
    );

    // SUBMITTED + 实例 → 可插；APPROVED 无实例（显式跳过审批）→ 可插
    let submitted = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}审批中"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "SUBMITTED",
        instance: Some(Uuid::now_v7()),
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    assert!(model::find_agreement(db(), submitted).await.expect("读").is_some());

    println!("[M4-07] 状态域：只有 DRAFT/SUBMITTED/APPROVED；EXPIRED/TERMINATED/CLOSED 插不进；SUBMITTED 必带实例 ✓");
}

/// 卡面「价格条款」：默认值 + FLOATING 必须有调价规则 + 币种/容差形状。
#[tokio::test]
async fn price_terms_defaults_and_floating_needs_rule() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    let plain = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}默认条款"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let row = model::find_agreement(db(), plain)
        .await
        .expect("读")
        .expect("协议必须读得到");
    assert_eq!(row.price_mode, "FIXED", "定价方式默认 FIXED");
    assert_eq!(row.price_currency, "CNY", "币种默认 CNY");
    assert!(!row.price_includes_tax, "默认不含税（与采购订单口径一致）");
    assert_eq!(row.price_tolerance, dec("0"), "容差默认 0（必须 <= 协议价）");
    assert!(row.price_adjust_rule.is_none(), "固定价不需要调价规则");

    // FLOATING 没有调价规则 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode) \
         VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, 'FLOATING')",
        vec![
            Value::Text(agreement_no(4)),
            Value::Text(format!("{NAME_PREFIX}浮动无规则")),
            Value::from(&sup),
            Value::Text("2026-01-01".to_string()),
            Value::Text("2026-12-31".to_string()),
        ],
        "FLOATING 但没有调价规则",
    )
    .await;
    assert!(
        err.contains("ck_biz_fwa_adjust"),
        "应命中 ck_biz_fwa_adjust，实际：{err}"
    );

    // FLOATING 规则是空白串 → 拒（3VL / 空串陷阱）
    assert_rejected(
        "INSERT INTO biz_framework_agreement \
            (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode, price_adjust_rule) \
         VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, 'FLOATING', '   ')",
        vec![
            Value::Text(agreement_no(5)),
            Value::Text(format!("{NAME_PREFIX}浮动空白规则")),
            Value::from(&sup),
            Value::Text("2026-01-01".to_string()),
            Value::Text("2026-12-31".to_string()),
        ],
        "FLOATING 的调价规则是空白串",
    )
    .await;

    // FLOATING + 规则 → 可插
    let floating = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}浮动协议"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FLOATING",
        adjust_rule: Some("随我的钢铁指数月度均价上浮，不超过 3%"),
    })
    .await;
    let row = model::find_agreement(db(), floating)
        .await
        .expect("读")
        .expect("浮动协议");
    assert_eq!(row.price_mode, "FLOATING");
    assert!(row.price_adjust_rule.as_deref().unwrap_or_default().contains("指数"));

    // 币种 / 容差越界 → 拒
    for (col, value) in [
        ("price_currency", "'rmb'"),
        ("price_tolerance", "1.5"),
        ("price_tolerance", "-0.1"),
    ] {
        assert_rejected(
            &format!(
                "INSERT INTO biz_framework_agreement \
                    (agreement_no, agreement_name, supplier_id, start_date, end_date, {col}) \
                 VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, {value})"
            ),
            vec![
                Value::Text(agreement_no(6)),
                Value::Text(format!("{NAME_PREFIX}坏{col}")),
                Value::from(&sup),
                Value::Text("2026-01-01".to_string()),
                Value::Text("2026-12-31".to_string()),
            ],
            &format!("{col} = {value}"),
        )
        .await;
    }

    println!("[M4-07] 价格条款：FIXED/CNY/不含税/容差 0 默认；FLOATING 必须有调价规则；币种 ISO、容差 0–1 ✓");
}

/// 卡面「数量上限」：NULL = 不限；写 0 被拒；写了必须 > 0。
#[tokio::test]
async fn quantity_caps_null_means_unlimited_and_zero_rejected() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 单头总量/总额：默认 NULL（不限）
    let head = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}不限量协议"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let row = model::find_agreement(db(), head).await.expect("读").expect("协议");
    assert!(row.total_max_qty.is_none(), "总量上限默认 NULL = 不限");
    assert!(row.total_max_amount.is_none(), "总额上限默认 NULL = 不限");

    // 单头写 0 → 拒（0 不是「不限」）
    for col in ["total_max_qty", "total_max_amount"] {
        let err = assert_rejected(
            &format!(
                "INSERT INTO biz_framework_agreement \
                    (agreement_no, agreement_name, supplier_id, start_date, end_date, {col}) \
                 VALUES (?::text, ?::text, ?::uuid, ?::text::date, ?::text::date, 0)"
            ),
            vec![
                Value::Text(agreement_no(7)),
                Value::Text(format!("{NAME_PREFIX}零{col}")),
                Value::from(&sup),
                Value::Text("2026-01-01".to_string()),
                Value::Text("2026-12-31".to_string()),
            ],
            &format!("{col} = 0"),
        )
        .await;
        assert!(err.contains("ck_biz_fwa_caps"), "应命中 ck_biz_fwa_caps，实际：{err}");
    }

    // 明细 max_qty：NULL = 不限可行；0 → 拒
    let mat = material().await;
    let unlimited = insert_item(head, 1, mat, "10.5", None).await;
    let items = model::items_of_agreement(db(), head).await.expect("读明细");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, unlimited);
    assert!(items[0].max_qty.is_none(), "明细上限 NULL = 不限");
    assert!(items[0].remaining_qty().is_none(), "不限量的行没有「剩余量」");

    assert_rejected(
        "INSERT INTO biz_framework_agreement_item \
            (agreement_id, line_no, material_id, unit_id, agreed_price, max_qty) \
         VALUES (?::uuid, 2, ?::uuid, ?::uuid, 1, 0)",
        vec![
            Value::from(&head),
            Value::from(&test_seed::ensure_material("M407-MAT3", "框架协议夹具物料三").await),
            Value::from(&test_seed::ensure_unit().await),
        ],
        "明细上限 0",
    )
    .await;

    println!("[M4-07] 数量上限：NULL = 不限（单头两级 + 明细一级）；写 0 被库层拒（0 不是不限）✓");
}

/// 卡面「物料范围」：同协议同物料一行；换协议可同物料；行号单内唯一。
#[tokio::test]
async fn material_scope_is_unique_per_agreement() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material().await;

    let a = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}协议甲"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let b = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}协议乙"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;

    insert_item(a, 1, mat, "10", Some("1000")).await;

    // 同协议同物料第二行 → 拒（会出现两个协议价）
    let err = assert_rejected(
        "INSERT INTO biz_framework_agreement_item \
            (agreement_id, line_no, material_id, unit_id, agreed_price) \
         VALUES (?::uuid, 2, ?::uuid, ?::uuid, 12)",
        vec![
            Value::from(&a),
            Value::from(&mat),
            Value::from(&test_seed::ensure_unit().await),
        ],
        "同一协议同物料的第二行",
    )
    .await;
    assert!(
        err.contains("uk_biz_fwai_material"),
        "应命中 uk_biz_fwai_material，实际：{err}"
    );

    // 另一份协议同物料 → 可以（不同协议各自一个协议价）
    insert_item(b, 1, mat, "11", None).await;
    assert_eq!(model::items_of_agreement(db(), b).await.expect("读乙").len(), 1);

    // 行号单内唯一
    let other_mat = test_seed::ensure_material("M407-MAT2", "框架协议夹具物料二").await;
    assert_rejected(
        "INSERT INTO biz_framework_agreement_item \
            (agreement_id, line_no, material_id, unit_id, agreed_price) \
         VALUES (?::uuid, 1, ?::uuid, ?::uuid, 13)",
        vec![
            Value::from(&a),
            Value::from(&other_mat),
            Value::from(&test_seed::ensure_unit().await),
        ],
        "同协议重复行号",
    )
    .await;

    // 按物料取协议行（M4-08 的入口）
    let found = model::find_item_by_material(db(), a, mat)
        .await
        .expect("查物料协议行")
        .expect("协议甲必须能按物料找到");
    assert_eq!(found.agreed_price, dec("10"));
    assert_eq!(found.max_qty, Some(dec("1000")));
    assert_eq!(found.tax_rate, dec("0.13"));

    println!("[M4-07] 物料范围：同协议同物料一行 / 换协议可同物料 / 行号单内唯一；find_item_by_material 可命中 ✓");
}

/// 已执行量超上限必须写得进去（超限拒绝属 M4-08）；派生合计由 `recalc_totals` 滚到单头。
#[tokio::test]
async fn executed_may_exceed_cap_and_totals_recalc_rolls_up() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material().await;

    let head = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}执行量协议"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "APPROVED",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let item = insert_item(head, 1, mat, "10", Some("100")).await;

    // 默认已执行 0，单头合计 0
    let row = model::find_agreement(db(), head).await.expect("读").expect("协议");
    assert_eq!(row.total_executed_qty, dec("0"));
    assert_eq!(row.total_executed_amount, dec("0"));

    // add_executed 累加（M4-08 的写入口）
    let now = Utc::now();
    model::add_executed(db(), item, dec("60"), dec("600"), now)
        .await
        .expect("回写执行量");
    model::add_executed(db(), item, dec("60"), dec("600"), now)
        .await
        .expect("回写执行量（第二次）");

    let items = model::items_of_agreement(db(), head).await.expect("读明细");
    assert_eq!(items[0].executed_qty, dec("120"), "累加，不是覆盖");
    assert_eq!(items[0].executed_amount, dec("1200"));
    assert_eq!(
        items[0].remaining_qty(),
        Some(dec("-20")),
        "超上限后剩余为负（库层刻意不拦，超限拒绝属 M4-08）"
    );

    // 派生合计滚到单头
    assert!(
        model::recalc_totals(db(), head).await.expect("重算"),
        "协议存在必须返回 true"
    );
    let row = model::find_agreement(db(), head).await.expect("读").expect("协议");
    assert_eq!(row.total_executed_qty, dec("120"));
    assert_eq!(row.total_executed_amount, dec("1200"));

    // 负值 → 拒
    assert_rejected(
        "UPDATE biz_framework_agreement_item SET executed_qty = -1 WHERE id = ?::uuid",
        vec![Value::from(&item)],
        "负已执行量",
    )
    .await;

    println!("[M4-07] 执行量：超上限写得进去（库层不拦）；recalc_totals 滚到单头；负值被拒 ✓");
}

/// 删单头连带删物料范围（不留孤儿行）。
#[tokio::test]
async fn delete_agreement_cascades_items() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material().await;

    let head = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}级联删除"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let item = insert_item(head, 1, mat, "9.9", Some("10")).await;
    assert_eq!(model::items_of_agreement(db(), head).await.expect("读").len(), 1);

    exec(
        "DELETE FROM biz_framework_agreement WHERE id = ?::uuid",
        vec![Value::from(&head)],
    )
    .await;

    let left = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_framework_agreement_item WHERE id = ?::uuid".to_string(),
            vec![Value::from(&item)],
        )
        .await
        .expect("查残留")
        .first()
        .and_then(|row| row.get_typed::<i64>("n").ok())
        .unwrap_or(-1);
    assert_eq!(left, 0, "删单头后物料范围必须跟着走（ON DELETE CASCADE）");

    println!("[M4-07] 级联：删协议连带删物料范围，孤儿行 0 ✓");
}

/// 取号：连续两个号递增，且两个号都能过库层单号 CHECK。
#[tokio::test]
async fn next_agreement_no_uses_fwa_seq() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let now = Utc::now();

    let first = model::next_agreement_no(db(), now).await.expect("取号 1");
    let second = model::next_agreement_no(db(), now).await.expect("取号 2");
    assert!(first.starts_with("FWA-"), "前缀必须是 FWA：{first}");
    assert_ne!(first, second, "两次取号必须不同");
    let n1: i64 = first.rsplit('-').next().and_then(|s| s.parse().ok()).expect("流水号");
    let n2: i64 = second.rsplit('-').next().and_then(|s| s.parse().ok()).expect("流水号");
    assert_eq!(n2, n1 + 1, "同日取号必须递增（{first} → {second}）");

    // 取到的号必须能过库层 CHECK（形状与约束一致，不是自己编的格式）
    insert_agreement(&NewAgreement {
        no: &first,
        name: format!("{NAME_PREFIX}取号落库"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    assert!(
        model::find_agreement_by_no(db(), &first)
            .await
            .expect("按号查")
            .is_some(),
        "取号 {first} 必须能落库并按号查回来"
    );

    println!("[M4-07] 取号：biz_seq(FWA) 递增且形状过 ck_biz_fwa_no_fmt（{first} → {second}）✓");
}

/// 同一供应商可有多份协议；快照名与实时现名并存（约定 13）。
#[tokio::test]
async fn supplier_snapshot_and_live_name_coexist() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    let first = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}同供应商甲"),
        supplier: sup,
        start: "2026-01-01",
        end: "2026-06-30",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    let second = insert_agreement(&NewAgreement {
        no: "",
        name: format!("{NAME_PREFIX}同供应商乙"),
        supplier: sup,
        start: "2026-07-01",
        end: "2026-12-31",
        status: "DRAFT",
        instance: None,
        price_mode: "FIXED",
        adjust_rule: None,
    })
    .await;
    assert_ne!(first, second);

    let row = model::find_agreement(db(), first).await.expect("读").expect("协议");
    assert_eq!(row.supplier_id, sup);
    assert_eq!(
        row.supplier_name.as_deref(),
        Some("M407-供应商快照"),
        "签约主体名走快照（历史事实）"
    );
    assert_eq!(
        row.supplier_display_name, "M407-供应商快照",
        "展示名回退到快照（主数据被软删时也显示得出）"
    );

    println!("[M4-07] 供应商：同供应商可多份协议（库层不做 supplier 唯一）；快照 + 实时现名并存 ✓");
}

/// 清场检查：`M407-` 夹具 0 残留（跑在最后，按名字排序 `zz_` 保证顺序）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left = count_of(&format!(
        "SELECT count(*) AS n FROM {0} WHERE agreement_name LIKE '{NAME_PREFIX}%'",
        model::AGREEMENT_TABLE
    ))
    .await;
    assert_eq!(left, 0, "M407- 协议 0 残留");
    println!("[M4-07] 清场：M407- 夹具 0 残留 ✓");
}
