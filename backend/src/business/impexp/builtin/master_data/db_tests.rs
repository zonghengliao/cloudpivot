//! 主数据导入的**真库**用例（`ERP_DB_URL` 开关；不设则静默 SKIP，见文件尾说明）。
//!
//! # 覆盖什么
//!
//! 每条定义至少「1 条成功 + 1 条校验失败」，并且刻意走**框架的真实链路**：
//! `validate()`（结构 + 逐行 + 外键 + 业务规则）→ `persist()`（真 SQL / 真事务）。
//! 只测 `validate_row` 无法证明「错误行带行号 + 具体原因」真的回到了报告里。
//!
//! # 为什么每个用例自带唯一的编码前缀
//!
//! 本文件与其它模块的用例共用同一个库（同一进程、多个测试线程）。所有写入的编码
//! 都以 `P16MD` 开头并在用例结束时删除，避免污染「按全表口径断言」的既有用例
//! （`business/erp/cash_flow.rs::r3_real_config_covers_all_accounts` 这类）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use nik::prelude::*;

use crate::business::impexp::builtin::master_data::{
    account::AccountImport, location::LocationImport, material::MaterialImport, supplier::SupplierImport,
    unit::UnitImport, warehouse::WarehouseImport,
};
use crate::business::impexp::definition::{
    DuplicateStrategy, ImportColumn, ImportCtx, ImportDefinition, ImportMode, RowValues,
};
use crate::business::impexp::validate::{DbFkResolver, PrecheckReport, ValidationOutcome, global_dicts};

/// 真库用例串行（共用进程级 `db()`）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 初始化进程级数据库连接；`false` = 没配 `ERP_DB_URL` / 连到的库不是它指定的那一个。
///
/// # 为什么要核对 `current_database()`
///
/// `set_db` 是进程级 `OnceLock`（先到先得）。若同一进程里别的模块的用例先把全局连接池
/// 初始化到**另一个**库，本文件再调 `set_db` 会得到「数据库资源已初始化」——
/// 此时如果直接当成成功，用例就会写进别人的库并一路变绿（假绿）。
/// 所以这里显式比对库名，不一致就打印 SKIP 并以「未连库」处理：
/// 跑本组用例请用独立进程（`cargo test --lib master_data`）+ 独立库。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            let wanted = url
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            if try_db().is_none() {
                let mut config = DatabaseConfig::from_url(&url);
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
                // 关掉连接复用，避免借到上一个 runtime 的死连接。
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => {
                        if let Err(error) = set_db(connection) {
                            println!("SKIP: set_db 失败（{error}）");
                            return false;
                        }
                    }
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        return false;
                    }
                }
            }
            match db()
                .scalar_with_params::<String>("SELECT current_database()", vec![])
                .await
            {
                Ok(name) if name == wanted => {
                    println!("P16-6 真库用例：已连接 {name}");
                    true
                }
                Ok(name) => {
                    println!(
                        "SKIP: 全局连接池指向 {name}，与 ERP_DB_URL 指定的 {wanted} 不一致\
                         （同进程里别的用例先初始化了连接池）；请单独跑 cargo test --lib master_data"
                    );
                    false
                }
                Err(error) => {
                    println!("SKIP: 读 current_database 失败（{error}）");
                    false
                }
            }
        })
        .await
}

/// 拿串行锁 + 连库；没库返回 `None`（用例退化为 SKIP）。
async fn guard() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let lock = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    Some(lock)
}

/// 清掉本文件造的所有数据（编码统一 `P16MD` 前缀）。
async fn cleanup() {
    for sql in [
        "DELETE FROM biz_location WHERE location_code LIKE 'P16MD%'",
        "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'P16MD%'",
        "DELETE FROM biz_material WHERE material_code LIKE 'P16MD%'",
        "DELETE FROM biz_supplier WHERE supplier_code LIKE 'P16MD%'",
        "DELETE FROM biz_unit WHERE unit_code LIKE 'P16MD%'",
        "DELETE FROM biz_account WHERE account_code LIKE 'P16MD%'",
        "DELETE FROM sys_import_task WHERE task_no LIKE 'P16MD%'",
    ] {
        if let Err(error) = db().execute_with_params(sql, vec![]).await {
            println!("清理失败（{sql}）：{error}");
        }
    }
}

/// 造一个带重复数据策略的导入任务（`persist` 靠它决定 upsert / skip）。
async fn new_task(strategy: DuplicateStrategy, mode: ImportMode) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    // task_no 是 varchar(32)：取 uuid 的 16 位十六进制（同一毫秒内连建两个也不会撞
    // `uk_sys_import_task_no`；8 位前缀实测撞过）
    let simple = id.simple().to_string();
    let task_no = format!("P16MD-{}", &simple[..16]);
    let options = serde_json::json!({ "duplicate_strategy": strategy.as_str() }).to_string();
    db().execute_with_params(
        "INSERT INTO sys_import_task \
             (id, task_no, biz_type, biz_name, file_path, file_sha256, file_name, file_size, \
              mode, status, options) \
         VALUES (?::uuid, ?, 'business.erp.test', '主数据用例', '/tmp/p16md.xlsx', \
                 repeat('0', 64), 'p16md.xlsx', 1, ?, 'VALIDATED', ?::jsonb)",
        vec![
            Value::from(id),
            Value::from(task_no),
            Value::from(mode.as_str()),
            Value::Json(options),
        ],
    )
    .await
    .expect("插入导入任务");
    id
}

/// 直接把一组行灌进定义（跳过 Excel 解析，用于构造「解析期才能发现」的边界）。
async fn persist_rows(
    definition: Arc<dyn ImportDefinition>,
    rows: Vec<RowValues>,
    ctx: ImportCtx,
) -> std::result::Result<usize, DbError> {
    db().transaction(|tx| async move { definition.persist(&tx, &rows, &ctx).await })
        .await
}

/// 造 xlsx（表头取定义自身的模板表头，列顺序自动对齐）。
fn bytes_of(definition: &dyn ImportDefinition, rows: Vec<Vec<&str>>) -> Vec<u8> {
    let columns: &[ImportColumn] = definition.columns();
    let headers = columns
        .iter()
        .map(|column| column.template_header())
        .collect::<Vec<_>>();
    let header_refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
    let maps = rows
        .into_iter()
        .map(|values| {
            let mut map = HashMap::new();
            for (index, header) in headers.iter().enumerate() {
                map.insert(header.clone(), values.get(index).copied().unwrap_or("").to_string());
            }
            map
        })
        .collect::<Vec<_>>();
    Export::to_xlsx_bytes(header_refs.as_slice(), &maps).expect("造 xlsx")
}

/// 跑一遍框架的完整预检（结构 + 逐行 + 外键 + 业务规则）。
async fn precheck(definition: Arc<dyn ImportDefinition>, bytes: &[u8], ctx: &ImportCtx) -> ValidationOutcome {
    crate::business::impexp::validate::validate(
        definition.as_ref(),
        bytes,
        global_dicts(),
        &DbFkResolver::new(),
        ctx,
        true,
    )
    .await
    .expect("预检不应抛错（错误应落在明细里）")
}

/// 「上传 → 预检 → 入库」一把过；返回（报告, 实际入库行数）。
async fn import(
    definition: Arc<dyn ImportDefinition>,
    rows: Vec<Vec<&str>>,
    ctx: &ImportCtx,
) -> (PrecheckReport, usize) {
    let bytes = bytes_of(definition.as_ref(), rows);
    let outcome = precheck(Arc::clone(&definition), &bytes, ctx).await;
    assert_eq!(
        outcome.report.error, 0,
        "预检应零错误，实际：{:?}",
        outcome.report.errors
    );
    let affected = db()
        .transaction(|tx| {
            let definition = Arc::clone(&definition);
            let rows = outcome.rows.clone();
            let ctx = ctx.clone();
            async move { definition.persist(&tx, &rows, &ctx).await }
        })
        .await
        .expect("入库应成功");
    (outcome.report, affected)
}

fn ctx_of(task_id: uuid::Uuid, mode: ImportMode) -> ImportCtx {
    ImportCtx {
        task_id,
        operator: None,
        mode,
    }
}

async fn count(sql: &str) -> i64 {
    db().scalar_with_params(sql, vec![]).await.expect("计数查询")
}

fn row(row_no: u32, pairs: &[(&'static str, &str)]) -> RowValues {
    let mut values = RowValues {
        row_no,
        ..Default::default()
    };
    for (key, value) in pairs {
        values.fields.insert(key, (*value).to_string());
    }
    values
}

// ==================== ① 供应商 ====================

#[tokio::test]
async fn supplier_import_is_idempotent_and_reports_row_numbers() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let task = new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await;
    let ctx = ctx_of(task, ImportMode::Strict);
    let definition: Arc<dyn ImportDefinition> = Arc::new(SupplierImport);
    let rows = vec![
        vec![
            "P16MD-SUP1",
            "北京甲供应链",
            "甲供应链",
            "张三",
            "13800000001",
            "",
            "100000",
            "启用",
        ],
        vec!["P16MD-SUP2", "上海乙贸易", "", "", "", "91310000MA1FL0000X", "", "停用"],
        vec!["P16MD-SUP3", "广州丙实业", "", "", "", "", "0", ""],
    ];

    let (report, affected) = import(Arc::clone(&definition), rows.clone(), &ctx).await;
    assert_eq!(report.total, 3);
    assert_eq!(affected, 3, "3 行都应入库");
    let total = count("SELECT COUNT(*) FROM biz_supplier WHERE supplier_code LIKE 'P16MD%'").await;
    assert_eq!(total, 3);

    // 重复导入同一份文件：行数不变（幂等），内容按编码更新
    let rows_changed = vec![
        vec!["P16MD-SUP1", "北京甲供应链（改）", "", "", "", "", "200000", "启用"],
        vec!["P16MD-SUP2", "上海乙贸易", "", "", "", "", "", "停用"],
        vec!["P16MD-SUP3", "广州丙实业", "", "", "", "", "0", ""],
    ];
    let (_, again) = import(Arc::clone(&definition), rows_changed, &ctx).await;
    assert_eq!(again, 3, "upsert 模式影响 3 行");
    let total = count("SELECT COUNT(*) FROM biz_supplier WHERE supplier_code LIKE 'P16MD%'").await;
    assert_eq!(total, 3, "重复导入不得产生新行");
    let name = db()
        .scalar_with_params::<String>(
            "SELECT supplier_name FROM biz_supplier WHERE supplier_code = 'P16MD-SUP1'",
            vec![],
        )
        .await
        .expect("读回名称");
    assert_eq!(name, "北京甲供应链（改）");

    // 校验失败：缺编码 / 信用额度为负 → 行号 + 列名 + 原因
    let bad = bytes_of(
        definition.as_ref(),
        vec![
            vec!["P16MD-SUP9", "正常行", "", "", "", "", "", "启用"],
            vec!["", "缺编码", "", "", "", "", "", "启用"],
            vec!["P16MD-SUP8", "负额度", "", "", "", "", "-1", "启用"],
        ],
    );
    let outcome = precheck(Arc::clone(&definition), &bad, &ctx).await;
    assert_eq!(outcome.report.error, 2);
    let required = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.kind.as_str() == "REQUIRED")
        .expect("应有必填错误");
    assert_eq!(required.row_no, 3, "行号必须是 Excel 里看到的第 3 行");
    assert_eq!(required.column_header, Some("供应商编码"));
    assert_eq!(required.message, "必填项为空");
    let range = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.kind.as_str() == "RANGE")
        .expect("应有范围错误");
    assert_eq!(range.row_no, 4);
    assert!(range.message.contains("超出范围"));

    cleanup().await;
}

#[tokio::test]
async fn supplier_skip_strategy_keeps_existing_rows() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let upsert_ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(SupplierImport);
    import(
        Arc::clone(&definition),
        vec![vec!["P16MD-S1", "原名", "", "", "", "", "", "启用"]],
        &upsert_ctx,
    )
    .await;

    let skip_ctx = ctx_of(
        new_task(DuplicateStrategy::Skip, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let (_, affected) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD-S1", "想覆盖的名字", "", "", "", "", "", "启用"],
            vec!["P16MD-S2", "新增的行", "", "", "", "", "", "启用"],
        ],
        &skip_ctx,
    )
    .await;
    assert_eq!(affected, 1, "SKIP 模式只应插入新编码那 1 行");
    let name = db()
        .scalar_with_params::<String>(
            "SELECT supplier_name FROM biz_supplier WHERE supplier_code = 'P16MD-S1'",
            vec![],
        )
        .await
        .expect("读回名称");
    assert_eq!(name, "原名", "SKIP 模式不得覆盖已存在的行");

    cleanup().await;
}

// ==================== ② 物料 ====================

#[tokio::test]
async fn material_import_100_rows_is_fast_and_checks_unit_fk() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    // 单位用迁移种下的 PCS（`V0137 §5.1`），不自己造共享主数据
    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(MaterialImport);
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(100);
    for index in 1..=100 {
        rows.push(vec![
            format!("P16MD-MAT{index:03}"),
            format!("测试物料 {index}"),
            "M8x40".to_string(),
            "PCS".to_string(),
            "原料".to_string(),
            "移动加权平均".to_string(),
            format!("{index}"),
            "启用".to_string(),
        ]);
    }
    let rows = rows
        .iter()
        .map(|values| values.iter().map(String::as_str).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let bytes = bytes_of(definition.as_ref(), rows);

    let started = Instant::now();
    let outcome = precheck(Arc::clone(&definition), &bytes, &ctx).await;
    assert_eq!(outcome.report.error, 0, "{:?}", outcome.report.errors);
    assert_eq!(outcome.report.total, 100);
    assert_eq!(
        outcome.report.fk_sql_statements, 1,
        "外键批量校验应只发 1 条 SQL（SQL 次数 = 外键列数）"
    );
    let affected = db()
        .transaction(|tx| {
            let definition = Arc::clone(&definition);
            let rows = outcome.rows.clone();
            let ctx = ctx.clone();
            async move { definition.persist(&tx, &rows, &ctx).await }
        })
        .await
        .expect("入库 100 行");
    let elapsed = started.elapsed();
    println!(
        "P16-6 实测：100 行物料 预检+入库 耗时 {:?}（预检 {} ms）",
        elapsed, outcome.report.elapsed_ms
    );
    assert_eq!(affected, 100);
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "100 行物料导入耗时必须 < 5 秒，实测 {elapsed:?}"
    );

    // 单位不存在 → 外键错误（行号 + 列名 + 「请先在计量单位中维护」）
    let bad = bytes_of(
        definition.as_ref(),
        vec![
            vec!["P16MD-MATX", "坏单位", "", "NOPE-UNIT", "", "", "", ""],
            vec!["P16MD-MATY", "坏分类", "", "PCS", "包材", "", "", ""],
        ],
    );
    let outcome = precheck(Arc::clone(&definition), &bad, &ctx).await;
    assert_eq!(outcome.report.error, 2);
    let fk = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.kind.as_str() == "FK_NOT_FOUND")
        .expect("应有外键错误");
    assert_eq!(fk.row_no, 2);
    assert_eq!(fk.column_header, Some("基本单位"));
    assert!(fk.message.contains("计量单位"));
    let enum_issue = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.kind.as_str() == "ENUM")
        .expect("应有枚举错误");
    assert_eq!(enum_issue.row_no, 3);

    cleanup().await;
}

// ==================== ③ 计量单位 ====================

#[tokio::test]
async fn unit_import_resolves_base_unit_inside_the_same_file() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(UnitImport);
    // 辅助单位排在基础单位**前面**：同文件引用必须照样能解析
    let (_, affected) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD-U-BOX", "箱", "否", "P16MD-U-PCS", "12", "启用"],
            vec!["P16MD-U-PCS", "个", "是", "", "", "启用"],
        ],
        &ctx,
    )
    .await;
    assert_eq!(affected, 2);
    let (rate, base_code): (String, String) = {
        let rows = db()
            .find_with_params(
                "SELECT b.unit_code::text AS base_code, u.rate::text AS rate \
                 FROM biz_unit u JOIN biz_unit b ON b.id = u.base_unit_id \
                 WHERE u.unit_code = 'P16MD-U-BOX'",
                vec![],
            )
            .await
            .expect("读回基准单位");
        let row = rows.into_iter().next().expect("应有基准单位");
        (
            row.get_typed("rate").expect("rate"),
            row.get_typed("base_code").expect("code"),
        )
    };
    assert_eq!(base_code, "P16MD-U-PCS");
    assert_eq!(rate, "12.000000");

    // 基准单位不存在 → 带行号的错误（绝不静默丢行）
    let error = persist_rows(
        Arc::clone(&definition),
        vec![row(
            5,
            &[
                ("unit_code", "P16MD-U-X"),
                ("unit_name", "坏辅助单位"),
                ("is_base", "false"),
                ("base_unit_code", "NOPE-UNIT"),
                ("rate", "3"),
            ],
        )],
        ctx.clone(),
    )
    .await
    .expect_err("基准单位不存在应报错");
    let text = format!("{error}");
    assert!(text.contains("第 5 行"), "错误必须带行号：{text}");
    assert!(text.contains("NOPE-UNIT"));

    cleanup().await;
}

#[tokio::test]
async fn unit_rule_errors_surface_in_the_precheck_report() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(UnitImport);
    let bytes = bytes_of(
        definition.as_ref(),
        vec![
            vec!["P16MD-U-A", "辅助单位缺换算率", "否", "PCS", "", "启用"],
            vec!["P16MD-U-B", "基础单位带基准", "是", "PCS", "1", "启用"],
        ],
    );
    let outcome = precheck(Arc::clone(&definition), &bytes, &ctx).await;
    assert_eq!(outcome.report.error, 2);
    let missing_rate = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.row_no == 2)
        .expect("第 2 行应有错");
    assert_eq!(missing_rate.kind.as_str(), "RULE");
    assert!(missing_rate.message.contains("必须填写换算率"));
    let base_conflict = outcome
        .report
        .errors
        .iter()
        .find(|issue| issue.row_no == 3)
        .expect("第 3 行应有错");
    assert!(base_conflict.message.contains("不能填写基准单位编码"));

    cleanup().await;
}

// ==================== ④ 会计科目 ====================

#[tokio::test]
async fn account_import_derives_level_and_revives_soft_deleted_rows() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(AccountImport);
    // 子级排在父级前面，验证级次靠推导而不是靠行序
    let (_, affected) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD.01", "测试二级科目", "资产", "借", "是", "P16MD", "启用"],
            vec!["P16MD", "测试一级科目", "资产", "借", "否", "", "启用"],
        ],
        &ctx,
    )
    .await;
    assert_eq!(affected, 2);
    let levels = db()
        .find_with_params(
            "SELECT account_code::text AS code, level::text AS level \
             FROM biz_account WHERE account_code LIKE 'P16MD%' ORDER BY account_code",
            vec![],
        )
        .await
        .expect("读回级次");
    let map = levels
        .iter()
        .map(|row| {
            (
                row.get_typed::<String>("code").expect("code"),
                row.get_typed::<String>("level").expect("level"),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(map.get("P16MD").map(String::as_str), Some("1"));
    assert_eq!(map.get("P16MD.01").map(String::as_str), Some("2"));

    // 软删后重导：全局唯一索引命中原行 → 复活而不是新建
    db().execute_with_params(
        "UPDATE biz_account SET deleted_at = CURRENT_TIMESTAMP WHERE account_code = 'P16MD.01'",
        vec![],
    )
    .await
    .expect("软删");
    let (_, again) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD.01", "测试二级科目", "资产", "借", "是", "P16MD", "启用"],
            vec!["P16MD", "测试一级科目", "资产", "借", "否", "", "启用"],
        ],
        &ctx,
    )
    .await;
    assert_eq!(again, 2);
    let alive = count("SELECT COUNT(*) FROM biz_account WHERE account_code LIKE 'P16MD%' AND deleted_at IS NULL").await;
    assert_eq!(alive, 2, "重导应把软删行复活，而不是留下 1 条死行");

    // 上级科目是自身 → 规则错误（带行号 + 列名）
    let bytes = bytes_of(
        definition.as_ref(),
        vec![vec!["P16MD.LOOP", "自引用", "资产", "借", "是", "P16MD.LOOP", "启用"]],
    );
    let outcome = precheck(Arc::clone(&definition), &bytes, &ctx).await;
    assert_eq!(outcome.report.error, 1);
    assert_eq!(outcome.report.errors[0].row_no, 2);
    assert!(outcome.report.errors[0].message.contains("不能是自身"));

    cleanup().await;
}

// ==================== ⑤ 仓库 ====================

#[tokio::test]
async fn warehouse_import_writes_materialized_path() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(WarehouseImport);
    let (_, affected) = import(
        Arc::clone(&definition),
        vec![
            vec![
                "P16MD-WH1",
                "测试一号仓",
                "原料仓",
                "北京市海淀区",
                "李四",
                "是",
                "启用",
            ],
            vec!["P16MD-WH2", "测试二号仓", "", "", "", "", ""],
        ],
        &ctx,
    )
    .await;
    assert_eq!(affected, 2);
    let rows = db()
        .find_with_params(
            "SELECT warehouse_code::text AS code, warehouse_path::text AS path, \
                    level::text AS level, is_leaf::text AS leaf, \
                    allow_negative::text AS negative, warehouse_type::text AS wtype \
             FROM biz_warehouse WHERE warehouse_code LIKE 'P16MD%' ORDER BY warehouse_code",
            vec![],
        )
        .await
        .expect("读回仓库");
    assert_eq!(rows.len(), 2);
    let first = &rows[0];
    let path: String = first.get_typed("path").expect("path");
    assert!(path.starts_with('/') && path.ends_with('/'));
    assert_eq!(path.matches('/').count(), 2, "物化路径应是 /id/ 形态：{path}");
    assert_eq!(first.get_typed::<String>("level").expect("level"), "1");
    assert_eq!(first.get_typed::<String>("leaf").expect("leaf"), "1");
    assert_eq!(first.get_typed::<String>("negative").expect("neg"), "1");
    assert_eq!(first.get_typed::<String>("wtype").expect("type"), "RAW");

    // 仓库类型非法 → 枚举错误
    let bad = bytes_of(
        definition.as_ref(),
        vec![vec!["P16MD-WH9", "坏类型仓", "虚拟仓", "", "", "", ""]],
    );
    let outcome = precheck(Arc::clone(&definition), &bad, &ctx).await;
    assert_eq!(outcome.report.error, 1);
    assert_eq!(outcome.report.errors[0].row_no, 2);
    assert_eq!(outcome.report.errors[0].kind.as_str(), "ENUM");

    cleanup().await;
}

// ==================== ⑥ 库位 ====================

#[tokio::test]
async fn location_import_requires_leaf_warehouse_and_single_default() {
    let Some(_guard) = guard().await else { return };
    cleanup().await;

    // 造两个仓库：父仓（非叶子）+ 子仓（叶子）
    db().execute_with_params(
        "INSERT INTO biz_warehouse \
             (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, parent_id) \
         VALUES ('f16d0000-0000-4000-8000-000000000001'::uuid, 'P16MD-WP', '父仓', \
                 '/f16d0000-0000-4000-8000-000000000001/', 1, 0, 1, NULL), \
                ('f16d0000-0000-4000-8000-000000000002'::uuid, 'P16MD-WC', '子仓', \
                 '/f16d0000-0000-4000-8000-000000000001/f16d0000-0000-4000-8000-000000000002/', \
                 2, 1, 1, 'f16d0000-0000-4000-8000-000000000001'::uuid)",
        vec![],
    )
    .await
    .expect("造仓库");

    let ctx = ctx_of(
        new_task(DuplicateStrategy::Upsert, ImportMode::Strict).await,
        ImportMode::Strict,
    );
    let definition: Arc<dyn ImportDefinition> = Arc::new(LocationImport);

    // 非叶子仓库 → 行级规则错误
    let bad = bytes_of(
        definition.as_ref(),
        vec![vec!["P16MD-L0", "P16MD-WP", "挂在父仓上", "", "", "", ""]],
    );
    let outcome = precheck(Arc::clone(&definition), &bad, &ctx).await;
    assert_eq!(outcome.report.error, 1);
    assert_eq!(outcome.report.errors[0].row_no, 2);
    assert!(
        outcome.report.errors[0].message.contains("叶子仓库"),
        "实际文案：{}",
        outcome.report.errors[0].message
    );

    // 叶子仓库 → 正常入库，且「默认库位」在同仓内唯一
    let (_, affected) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD-L1", "P16MD-WC", "A 区一排一列", "存储", "是", "1000", "启用"],
            vec!["P16MD-L2", "P16MD-WC", "A 区一排二列", "", "否", "", "启用"],
        ],
        &ctx,
    )
    .await;
    assert_eq!(affected, 2);
    // 再导一次，把默认位换到 L2：同仓只能剩一个默认
    let (_, _) = import(
        Arc::clone(&definition),
        vec![
            vec!["P16MD-L1", "P16MD-WC", "A 区一排一列", "存储", "否", "1000", "启用"],
            vec!["P16MD-L2", "P16MD-WC", "A 区一排二列", "", "是", "", "启用"],
        ],
        &ctx,
    )
    .await;
    let defaults = db()
        .find_with_params(
            "SELECT location_code::text AS code FROM biz_location \
             WHERE location_code LIKE 'P16MD%' AND is_default = 1 AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("读回默认库位");
    assert_eq!(defaults.len(), 1, "每仓最多一个默认库位");
    assert_eq!(defaults[0].get_typed::<String>("code").expect("code"), "P16MD-L2");

    // 同一份文件里两行都标默认 → 前置报错（不做半截写入）
    let error = persist_rows(
        Arc::clone(&definition),
        vec![
            row(
                6,
                &[
                    ("location_code", "P16MD-L8"),
                    ("warehouse_code", "P16MD-WC"),
                    ("is_default", "true"),
                ],
            ),
            row(
                9,
                &[
                    ("location_code", "P16MD-L9"),
                    ("warehouse_code", "P16MD-WC"),
                    ("is_default", "true"),
                ],
            ),
        ],
        ctx.clone(),
    )
    .await
    .expect_err("同仓两个默认库位应报错");
    let text = format!("{error}");
    assert!(text.contains("第 6 行") && text.contains("第 9 行"), "文案：{text}");

    cleanup().await;
}
