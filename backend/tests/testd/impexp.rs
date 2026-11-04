//! 测试D · 导入导出（I01–I13）。
//!
//! 全部落在 `system.dict_item`（唯一内建导入定义）+ `system.user`（内建导出定义，带三个敏感列）
//! 与真库 `cloudpivot_test` 上。用例自清理（前缀 `TD-I*`）。

use std::collections::HashMap;

use axum::body::to_bytes;
use axum::extract::Query;
use calamine::Reader as _;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::impexp::{
    definition::ImportCtx, definition::ImportMode, engine, global_registry, model as impexp_model,
    template, validate, worker,
};

use crate::testd::common::{self as c, Ctx};

/// 内建导入定义编码。
const DICT_BIZ: &str = "system.dict_item";
/// 内建导出定义编码（用户通讯录，三个敏感列）。
const USER_BIZ: &str = "system.user";

// ==================== 辅助 ====================

/// 把一份「表头 + 行」写成 xlsx。
fn xlsx(headers: &[&str], rows: &[Vec<&str>]) -> Vec<u8> {
    let rows: Vec<HashMap<String, String>> = rows
        .iter()
        .map(|cells| {
            headers
                .iter()
                .zip(cells.iter())
                .map(|(h, v)| (h.to_string(), v.to_string()))
                .collect()
        })
        .collect();
    engine::write_sheet(headers, &rows).expect("生成 xlsx")
}

/// 读 xlsx 的第一个 sheet → 二维文本（含表头）。
fn read_xlsx(bytes: &[u8]) -> Vec<Vec<String>> {
    use calamine::{Xlsx, open_workbook_from_rs};
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut workbook: Xlsx<_> = open_workbook_from_rs(cursor).expect("打开 xlsx");
    let name = workbook
        .sheet_names()
        .first()
        .cloned()
        .expect("xlsx 至少一个 sheet");
    let range = workbook.worksheet_range(&name).expect("读 sheet");
    range
        .rows()
        .map(|row| row.iter().map(|cell| cell.to_string()).collect())
        .collect()
}

/// dict_item 的导入表头（与定义顺序一致）。
fn dict_headers() -> Vec<&'static str> {
    global_registry()
        .import_definition(DICT_BIZ)
        .expect("内建定义存在")
        .columns()
        .iter()
        .map(|column| column.header)
        .collect()
}

/// 把文件落到临时目录，返回绝对路径。
async fn persist(bytes: &[u8], name: &str) -> String {
    let dir = std::env::temp_dir().join("td-testd");
    tokio::fs::create_dir_all(&dir).await.expect("建临时目录");
    let path = dir.join(name);
    tokio::fs::write(&path, bytes).await.expect("写临时文件");
    path.to_string_lossy().to_string()
}

/// 建一个「已预检」的导入任务（等价于 upload 之后的状态）。
async fn seed_import_task(ctx: &Ctx, bytes: &[u8], operator: Uuid, mode: ImportMode) -> Uuid {
    let id = Uuid::now_v7();
    let path = persist(bytes, &format!("{}.xlsx", id)).await;
    let task_no = impexp_model::build_import_task_no(&id);
    impexp_model::insert_import_task(&impexp_model::NewImportTask {
        id,
        task_no: &task_no,
        biz_type: DICT_BIZ,
        biz_name: "字典项",
        file_path: &path,
        file_sha256: &worker::sha256_hex(bytes),
        file_name: &ctx.key("import.xlsx"),
        file_size: bytes.len() as i64,
        mode: mode.as_str(),
        operator: Some(operator),
        options: Some("{}"),
    })
    .await
    .expect("写导入任务行");
    impexp_model::update_precheck_result(&id, "VALIDATED", 0, 0, 0, None, None)
        .await
        .expect("置为已校验");
    id
}

/// 跑一次预检（不落库）。
async fn precheck(bytes: &[u8], mode: ImportMode) -> validate::PrecheckReport {
    let definition = global_registry().import_definition(DICT_BIZ).expect("定义");
    let ctx = ImportCtx {
        task_id: Uuid::now_v7(),
        operator: None,
        mode,
    };
    validate::precheck(
        definition.as_ref(),
        bytes,
        validate::global_dicts(),
        &validate::DbFkResolver::new(),
        &ctx,
    )
    .await
    .expect("预检不应结构性失败")
}

/// 一个数据有效行（字典编码用夹具里真实存在的 dict_type）。
fn good_row(code: &str, value: &str, label: &str) -> Vec<String> {
    vec![
        code.to_string(),
        value.to_string(),
        label.to_string(),
        String::new(),
        "1".to_string(),
        "否".to_string(),
        "启用".to_string(),
    ]
}

// ============================================================================
// I01 全链路：模板 → 填数据 → 预检 → 确认入库
// ============================================================================

/// I01 ★ 导入任务全链路（模板下载 → 填数据 → 上传 → 校验 → 确认入库）。
#[tokio::test]
async fn i01_import_chain_template_precheck_confirm() {
    let Some(()) = c::ready().await else { return };
    c::case("I01", |ctx| async move {
        let role = c::make_role(&ctx, "IMP", 1).await;
        let user = c::make_user(&ctx, "importer", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        // ① 模板可下载且含表头
        let definition = global_registry().import_definition(DICT_BIZ).expect("定义");
        let tpl = template::build_template(definition.as_ref()).expect("生成模板");
        let tpl_rows = read_xlsx(&tpl);
        assert!(!tpl_rows.is_empty(), "模板没有任何行");
        let headers: Vec<String> = tpl_rows[0].clone();
        assert!(
            headers.iter().any(|h| h.starts_with("字典编码")),
            "模板缺表头：{headers:?}"
        );

        // ② 按同一套表头填两行数据
        let headers_ref: Vec<&str> = dict_headers();
        let label_a = ctx.key("项A");
        let label_b = ctx.key("项B");
        let rows = [
            good_row(&dict_code, "10", &label_a),
            good_row(&dict_code, "11", &label_b),
        ];
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers_ref, &rows_ref);

        // ③ 预检
        let report = precheck(&bytes, ImportMode::Strict).await;
        assert_eq!(report.total, 2, "预检行数不符");
        assert_eq!(report.error, 0, "预检出现错误行：{:?}", report.errors);

        // ④ 确认入库
        let task = seed_import_task(&ctx, &bytes, user, ImportMode::Strict).await;
        let res = impexp_handler::import_confirm(
            axum::extract::Path(task.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect("确认入库");
        let data = res.data.expect("确认入库返回空 data");
        assert_eq!(data.status, "SUCCESS", "入库状态：{}", data.status);
        assert_eq!(data.imported_rows, 2, "实际入库 {} 行", data.imported_rows);

        // ⑤ 库里真的有两行，且字段映射正确（名称 / 状态 / 排序都落到对应列）
        let stored = c::count(
            "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid AND item_label LIKE ?",
            vec![Value::from(&dict), Value::Text(format!("{}%", ctx.key("项")))],
        )
        .await;
        assert_eq!(stored, 2, "字典项落库数量不符");
        let status = c::text(
            "SELECT status::text AS v FROM sys_dict_item WHERE item_label = ?::text",
            vec![Value::Text(label_a.clone())],
        )
        .await;
        assert_eq!(status, "1", "状态列映射错误（Excel「启用」应落 '1'）");
        let sort = c::count(
            "SELECT sort_order AS n FROM sys_dict_item WHERE item_label = ?",
            vec![Value::Text(label_a.clone())],
        )
        .await;
        assert_eq!(sort, 1, "排序号列映射错误");

    })
    .await;
}

// ============================================================================
// I02 校验失败 → 错误报表行号列号准确
// ============================================================================

/// I02 ★ 校验失败 → 错误报表可下载，行号 / 列号准确。
#[tokio::test]
async fn i02_error_report_row_and_column() {
    let Some(()) = c::ready().await else { return };
    c::case("I02", |ctx| async move {
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        let headers = dict_headers();
        let rows = [
            good_row(&dict_code, "20", &ctx.key("正常行")),
            // 第 3 行（表头算第 1 行）：字典项名称必填却为空
            good_row(&dict_code, "21", ""),
            // 第 4 行：排序号写非数字 → 列级错误
            vec![
                dict_code.clone(),
                "22".to_string(),
                ctx.key("坏排序").to_string(),
                String::new(),
                "不是数字".to_string(),
                "否".to_string(),
                "启用".to_string(),
            ],
        ];
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers, &rows_ref);

        let report = precheck(&bytes, ImportMode::Strict).await;
        assert_eq!(report.total, 3, "总行数应为 3，实际 {}", report.total);
        assert_eq!(report.error, 2, "错误行数应为 2，实际 {}", report.error);

        // 行号：Excel 真实行号（表头 1 → 数据从 2 开始）
        let missing = report
            .errors
            .iter()
            .find(|issue| issue.row_no == 3)
            .expect("第 3 行缺失必填的错误未报出");
        assert_eq!(
            missing.column_header,
            Some("字典项名称"),
            "错误未定位到正确列：{:?}",
            missing.column_header
        );
        let bad_int = report
            .errors
            .iter()
            .find(|issue| issue.row_no == 4)
            .expect("第 4 行排序号非法的错误未报出");
        assert_eq!(bad_int.column_header, Some("排序号"));

        // 错误报表可生成且带「行号 / 列名」两列
        let sheet = template::build_error_sheet(&report.errors).expect("生成错误报表");
        let rows_out = read_xlsx(&sheet);
        assert!(rows_out.len() >= 3, "错误报表行数不足：{}", rows_out.len());
        let header = &rows_out[0];
        assert!(
            header.iter().any(|h| h.contains("行号")) && header.iter().any(|h| h.contains("列")),
            "错误报表表头缺行号/列名：{header:?}"
        );
        let row_nos: Vec<&String> = rows_out.iter().skip(1).map(|row| &row[0]).collect();
        assert!(
            row_nos.iter().any(|no| no.as_str() == "3")
                && row_nos.iter().any(|no| no.as_str() == "4"),
            "错误报表里的行号不准确：{row_nos:?}"
        );
    })
    .await;
}

// ============================================================================
// I03 事务边界：STRICT 全不进 / PARTIAL 部分进
// ============================================================================

/// I03 ★ 部分成功部分失败 → 事务边界（STRICT 全不进，PARTIAL 只进好行）。
#[tokio::test]
async fn i03_strict_vs_partial_transaction_boundary() {
    let Some(()) = c::ready().await else { return };
    c::case("I03", |ctx| async move {
        let role = c::make_role(&ctx, "IMP", 1).await;
        let user = c::make_user(&ctx, "importer", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        let headers = dict_headers();
        let rows = [
            good_row(&dict_code, "30", &ctx.key("好行")),
            good_row(&dict_code, "31", ""), // 缺必填
        ];
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers, &rows_ref);

        // ---- STRICT：整批中止，一行都不进 ----
        let strict_task = seed_import_task(&ctx, &bytes, user, ImportMode::Strict).await;
        let err = impexp_handler::import_confirm(
            axum::extract::Path(strict_task.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect_err("STRICT 有失败行必须整批中止");
        assert_eq!(
            err.status, 422,
            "STRICT 中止状态码应为 422，实际 {}",
            err.status
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid",
                vec![Value::from(&dict)]
            )
            .await,
            0,
            "STRICT 模式下一行都不该入库（事务边界被破坏）"
        );
        let strict_status = c::text(
            "SELECT status AS v FROM sys_import_task WHERE id = ?::uuid",
            vec![Value::from(&strict_task)],
        )
        .await;
        assert_eq!(strict_status, "FAILED", "STRICT 失败后任务状态应为 FAILED");

        // ---- PARTIAL：好行入库，坏行跳过，任务状态 PARTIAL ----
        let partial_task = seed_import_task(&ctx, &bytes, user, ImportMode::Partial).await;
        let res = impexp_handler::import_confirm(
            axum::extract::Path(partial_task.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect("PARTIAL 应允许部分入库");
        let data = res.data.expect("部分导入返回空 data");
        assert_eq!(data.status, "PARTIAL", "部分导入状态应为 PARTIAL");
        assert_eq!(
            data.imported_rows, 1,
            "部分导入应只进 1 行，实际 {}",
            data.imported_rows
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid",
                vec![Value::from(&dict)]
            )
            .await,
            1,
            "PARTIAL 模式应恰好进 1 行"
        );
    })
    .await;
}

// ============================================================================
// I04 一万行不超时 / 不 OOM
// ============================================================================

/// I04 ★ 超大文件（1 万行）：预检在时限内完成且行数准确。
#[tokio::test]
async fn i04_ten_thousand_rows() {
    let Some(()) = c::ready().await else { return };
    c::case("I04", |ctx| async move {
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        let headers = dict_headers();
        let mut rows: Vec<Vec<String>> = Vec::with_capacity(10_000);
        for index in 0..10_000 {
            rows.push(good_row(
                &dict_code,
                &format!("V{index}"),
                &ctx.key(&format!("行{index}")),
            ));
        }
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers, &rows_ref);
        assert!(
            bytes.len() > 100_000,
            "1 万行 xlsx 体量异常：{} 字节",
            bytes.len()
        );

        let started = std::time::Instant::now();
        let report = precheck(&bytes, ImportMode::Strict).await;
        let elapsed = started.elapsed();

        assert_eq!(report.total, 10_000, "预检行数不符：{}", report.total);
        assert_eq!(report.error, 0, "1 万行合法数据不该有错误行");
        assert!(
            elapsed < std::time::Duration::from_secs(120),
            "1 万行预检耗时 {} ms，超出 120 s 上限",
            elapsed.as_millis()
        );
        println!(
            "[I04] 1 万行预检：{} ms，外键 SQL {} 条",
            elapsed.as_millis(),
            report.fk_sql_statements
        );
    })
    .await;
}

// ============================================================================
// I05 重复导入：幂等（ON CONFLICT DO UPDATE）
// ============================================================================

/// I05 ★ 导入重复数据 → 去重还是报错（实测：按 (字典编码, 值) 幂等更新，不报错）。
#[tokio::test]
async fn i05_duplicate_import_is_idempotent() {
    let Some(()) = c::ready().await else { return };
    c::case("I05", |ctx| async move {
        let role = c::make_role(&ctx, "IMP", 1).await;
        let user = c::make_user(&ctx, "importer", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        let headers = dict_headers();
        let rows = [good_row(&dict_code, "40", &ctx.key("重复项"))];
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers, &rows_ref);

        // 第一次
        let first = seed_import_task(&ctx, &bytes, user, ImportMode::Strict).await;
        impexp_handler::import_confirm(
            axum::extract::Path(first.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect("首次导入");
        let after_first = c::count(
            "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;
        assert_eq!(after_first, 1);

        // 第二次（同一份文件）
        let second = seed_import_task(&ctx, &bytes, user, ImportMode::Strict).await;
        impexp_handler::import_confirm(
            axum::extract::Path(second.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect("重复导入不该报错（幂等更新）");
        let after_second = c::count(
            "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;
        assert_eq!(after_second, 1, "重复导入产生了重复行（去重失效）");

        // 文件内重复（同一文件里两行相同）→ 预检阶段就要报 UNIQUE，不等到入库
        let dup_rows = [
            good_row(&dict_code, "41", &ctx.key("文件内重复")),
            good_row(&dict_code, "41", &ctx.key("文件内重复")),
        ];
        let dup_ref: Vec<Vec<&str>> = dup_rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let dup_bytes = xlsx(&headers, &dup_ref);
        let report = precheck(&dup_bytes, ImportMode::Strict).await;
        assert!(
            report.error >= 1,
            "文件内重复键未被预检拦下（total={} valid={})",
            report.total,
            report.valid
        );
    })
    .await;
}

// ============================================================================
// I06 导入到 biz_task：定义未实现（需求缺口）
// ============================================================================

/// I06 ★ 导入任务到 `biz_task` 的字段映射 —— **当前未实现**（无该导入定义）。
///
/// 这颗用例钉住「事实」而不是「期望」：注册表里没有任务导入定义，所以任何
/// `/business/impexp/import/upload?biz_type=task` 都会 404 + 60011。
/// 若要交付该能力，需业务方实现 `ImportDefinition` 并注册（见报告 BUG-D-01，P2 需求缺口）。
#[tokio::test]
async fn i06_task_import_definition_missing() {
    let Some(()) = c::ready().await else { return };
    let registry = global_registry();
    let candidates = [
        "biz.task",
        "business.task",
        "task",
        "system.task",
        "biz_task",
    ];
    let found: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|code| registry.import_definition(code).is_some())
        .collect();
    assert!(
        found.is_empty(),
        "任务导入定义已注册（{found:?}）：请把本用例改成「模板→映射→入库」全链路"
    );

    // 已注册的导入定义清单（供报告引用）
    let registered: Vec<&'static str> = registry
        .briefs()
        .iter()
        .filter(|brief| brief.supports_import)
        .map(|brief| brief.biz_type)
        .collect();
    println!("[I06] 已注册导入定义：{registered:?}");
    // 2026-11-04（DEPLOY dep-v217）复核：P1-6 起主数据导入交付了 6 个定义
    // （供应商/物料/单位/科目/仓库/库位），本清单 tripwire 当时没跟着更新 ⇒ 假红。
    // 6 个定义是有意交付的（见 docs/ERP-导入导出口径.md 与 P1-6 卡片），故此处补齐期望值；
    // 注册顺序不是稳定契约（业务定义先注册、`system.dict_item` 在后），比较前排序。
    let mut got = registered.clone();
    got.sort_unstable();
    let mut want = vec![
        DICT_BIZ,
        "business.erp.account",
        "business.erp.location",
        "business.erp.material",
        "business.erp.supplier",
        "business.erp.unit",
        "business.erp.warehouse",
    ];
    want.sort_unstable();
    assert_eq!(got, want, "内建导入定义清单变化，请复核本用例");
}

// ============================================================================
// I07 同步导出
// ============================================================================

/// I07 导出任务：同步导出 → 文件可下载、内容正确（含枚举中文化）。
#[tokio::test]
async fn i07_export_sync_content() {
    let Some(()) = c::ready().await else { return };
    c::case("I07", |ctx| async move {
        let role = c::make_role(&ctx, "EXP", 1).await;
        let user = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        // 直接造两行字典项
        for (value, label, status) in [("01", "已启用", 1_i16), ("02", "已停用", 0_i16)] {
            c::exec_with(
                "INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, is_default, status) \
                 VALUES (gen_random_uuid(), ?::uuid, ?, ?, 0, false, ?)",
                vec![
                    Value::from(&dict),
                    Value::Text(value.to_string()),
                    Value::Text(ctx.key(label)),
                    Value::I16(status),
                ],
            )
            .await;
        }

        let response = impexp_handler::export_sync(
            c::claims(user, "exporter", &[]),
            Query(impexp_dto::ExportSyncQuery {
                biz_type: DICT_BIZ.to_string(),
                keyword: Some(ctx.key("已")),
                status: None,
            }),
        )
        .await;
        assert_eq!(response.status(), 200, "同步导出 HTTP 状态应为 200");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读响应体");
        let rows = read_xlsx(&body);
        assert!(rows.len() >= 3, "导出文件行数不足：{}", rows.len());
        let flat: Vec<String> = rows.concat();
        assert!(
            flat.iter().any(|cell| cell == &ctx.key("已启用")),
            "导出文件缺少夹具数据：{flat:?}"
        );
        // 状态列被翻成中文（1 → 启用 / 0 → 停用）
        assert!(
            flat.iter().any(|cell| cell == "启用") && flat.iter().any(|cell| cell == "停用"),
            "状态枚举未中文化：{flat:?}"
        );

    })
    .await;
}

// ============================================================================
// I08 异步导出：创建 → worker 认领 → 完成 → 下载
// ============================================================================

/// I08 异步导出全链路：建任务 → worker 跑一轮 → SUCCESS → 产物可下载。
#[tokio::test]
async fn i08_export_async_worker_and_download() {
    let Some(()) = c::ready().await else { return };
    c::case("I08", |ctx| async move {
        let role = c::make_role(&ctx, "EXP", 1).await;
        let user = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        c::exec_with(
            "INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, is_default, status) \
             VALUES (gen_random_uuid(), ?::uuid, '99', ?, 0, false, '1')",
            vec![Value::from(&dict), Value::Text(ctx.key("异步导出项"))],
        )
        .await;

        // 建异步导出任务（CSV，便于断言文本）
        let task_id = Uuid::now_v7();
        let task_no = impexp_model::build_export_task_no(&task_id);
        impexp_model::insert_export_task(&impexp_model::NewExportTask {
            id: task_id,
            task_no: &task_no,
            biz_type: DICT_BIZ,
            biz_name: "字典项",
            export_name: &ctx.key("export"),
            format: "CSV",
            query_params: Some("{}"),
            columns_snapshot: Some("[]"),
            max_attempts: 3,
            operator: Some(user),
        })
        .await
        .expect("建导出任务");

        let dir = std::env::temp_dir().join(format!("td-testd-export-{}", ctx.tag));
        tokio::fs::create_dir_all(&dir).await.expect("建产物目录");
        let options = worker::WorkerOptions {
            worker_id: format!("testd-{}", ctx.tag),
            export_dir: dir.clone(),
            ttl_seconds: 600,
            max_attempts: 3,
            backoff_seconds: 1,
            batch_size: 1000,
            claim_limit: 20,
            max_rows: 100_000,
        };
        // 反复跑几轮（其他卡可能也在跑 worker，可能先认领走别人的任务）
        for _ in 0..5 {
            worker::run_once_with(&options).await.expect("worker 一轮");
            let status = c::text(
                "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
                vec![Value::from(&task_id)],
            )
            .await;
            if status == "SUCCESS" || status == "FAILED" {
                break;
            }
        }

        let status = c::text(
            "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert_eq!(status, "SUCCESS", "异步导出未成功完成，状态 {status}");
        let file_path = c::text(
            "SELECT file_path AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        let bytes = tokio::fs::read(&file_path).await.expect("读产物文件");
        let content = String::from_utf8_lossy(&bytes);
        assert!(
            content.contains(&ctx.key("异步导出项")),
            "产物内容缺夹具数据：{content}"
        );
        assert_eq!(
            c::text(
                "SELECT progress::text AS v FROM sys_export_task WHERE id = ?::uuid",
                vec![Value::from(&task_id)]
            )
            .await,
            "100",
            "成功任务进度应为 100"
        );

        // 下载接口：成功任务返回 200 且带文件
        let response = impexp_handler::export_download(
            c::claims(user, "exporter", &[]),
            axum::extract::Path(task_id.to_string()),
        )
        .await;
        assert_eq!(response.status(), 200, "下载接口状态 {}", response.status());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    })
    .await;
}

// ============================================================================
// I09 导出权限
// ============================================================================

/// I09 ★ 导出权限：没有 `business:impexp:export:create` 的角色不能导出。
///
/// 分两层验：
/// ① 权限码必须在 `sys_menu` 里登记（迁移 V 系列），否则框架层无从校验；
/// ② 令牌不携带该码时 `has_permission` 必须为假 —— 这正是
///    `#[nik::permission]` 中间件 403 的判据（真 403 由 `scripts/verify/testd_http.sh` 在
///    启动的服务进程上验，见报告）。
#[tokio::test]
async fn i09_export_permission_code_enforced() {
    let Some(()) = c::ready().await else { return };
    c::case("I09", |ctx| async move {
        for code in [
            "business:impexp:export:create",
            "business:impexp:export:download",
            "business:impexp:import:confirm",
        ] {
            let n = c::count(
                "SELECT count(*) AS n FROM sys_menu WHERE auth_code = ?",
                vec![Value::Text(code.to_string())],
            )
            .await;
            assert_eq!(
                n, 1,
                "权限码 {code} 未在 sys_menu 登记（框架层将无法拦截越权）"
            );
        }

        let role = c::make_role(&ctx, "NOPERM", 4).await;
        let user = c::make_user(&ctx, "noperm", "HUMAN").await;
        c::bind_role(user, role).await;
        let claims = c::token_claims(user, "noperm", &["business:impexp:import:upload"]);
        assert!(
            !claims.has_permission("business:impexp:export:create"),
            "无导出权限的令牌被判成有权限"
        );
        assert!(claims.has_permission("business:impexp:import:upload"));
    })
    .await;
}

// ============================================================================
// I11 导出失败 → 可重试
// ============================================================================

/// I11 导出失败 → 任务失败状态 + 重试接口把它重新排队。
#[tokio::test]
async fn i11_failed_export_can_retry() {
    let Some(()) = c::ready().await else { return };
    c::case("I11", |ctx| async move {
        let role = c::make_role(&ctx, "EXP", 1).await;
        let user = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(user, role).await;

        let task_id = Uuid::now_v7();
        let task_no = impexp_model::build_export_task_no(&task_id);
        impexp_model::insert_export_task(&impexp_model::NewExportTask {
            id: task_id,
            task_no: &task_no,
            biz_type: DICT_BIZ,
            biz_name: "字典项",
            export_name: &ctx.key("fail"),
            format: "CSV",
            query_params: Some("{}"),
            columns_snapshot: Some("[]"),
            max_attempts: 3,
            operator: Some(user),
        })
        .await
        .expect("建导出任务");

        // 模拟一次失败（worker 认领后进入 RUNNING，再因终态原因失败）
        c::exec_with(
            "UPDATE sys_export_task SET status = 'RUNNING', attempt_count = 3 WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        impexp_model::fail_export_task_terminal(&task_id, "磁盘写满（测试注入）")
            .await
            .expect("置为失败");
        let status = c::text(
            "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert_eq!(status, "FAILED", "失败任务状态应为 FAILED");
        let message = c::text(
            "SELECT coalesce(message,'') AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert!(!message.is_empty(), "失败任务没有留下失败原因");

        let res = impexp_handler::export_retry(
            c::claims(user, "exporter", &[]),
            axum::extract::Path(task_id.to_string()),
        )
        .await
        .expect("失败任务应可重试");
        let retried = res.data.expect("重试返回空 data");
        assert_eq!(
            retried.status, "PENDING",
            "重试后状态应为 PENDING，实际 {}",
            retried.status
        );
        let attempts = c::count(
            "SELECT attempt_count AS n FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert_eq!(attempts, 0, "重试应清零尝试计数（当前 {attempts}）");

        // SUCCESS 任务不允许重试（状态机）
        c::exec_with(
            "UPDATE sys_export_task SET status = 'SUCCESS' WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        let err = impexp_handler::export_retry(
            c::claims(user, "exporter", &[]),
            axum::extract::Path(task_id.to_string()),
        )
        .await
        .expect_err("成功任务不该可重试");
        assert_eq!(err.status, 409, "非法状态重试应为 409，实际 {}", err.status);
    })
    .await;
}

// ============================================================================
// I12 产物过期 → 410
// ============================================================================

/// I12 导出产物过期（30 分钟后）→ 下载返回 410。
#[tokio::test]
async fn i12_expired_artifact_returns_410() {
    let Some(()) = c::ready().await else { return };
    c::case("I12", |ctx| async move {
        let role = c::make_role(&ctx, "EXP", 1).await;
        let user = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(user, role).await;

        let task_id = Uuid::now_v7();
        let task_no = impexp_model::build_export_task_no(&task_id);
        impexp_model::insert_export_task(&impexp_model::NewExportTask {
            id: task_id,
            task_no: &task_no,
            biz_type: DICT_BIZ,
            biz_name: "字典项",
            export_name: &ctx.key("expired"),
            format: "CSV",
            query_params: Some("{}"),
            columns_snapshot: Some("[]"),
            max_attempts: 3,
            operator: Some(user),
        })
        .await
        .expect("建导出任务");
        // 造一个「已完成但产物已过期」的任务
        let path = persist(b"a,b\n1,2\n", &format!("{task_id}.csv")).await;
        c::exec_with(
            "UPDATE sys_export_task SET status = 'SUCCESS', progress = 100, file_path = ?, \
                    file_name = 'expired.csv', file_size = 8, finished_at = now(), \
                    expire_at = now() - interval '100 years' WHERE id = ?::uuid",
            vec![Value::Text(path.clone()), Value::from(&task_id)],
        )
        .await;

        let response = impexp_handler::export_download(
            c::claims(user, "exporter", &[]),
            axum::extract::Path(task_id.to_string()),
        )
        .await;
        assert_eq!(
            response.status(),
            410,
            "过期产物应返回 410 GONE，实际 {}",
            response.status()
        );

        // 懒过期：下载命中过期 → 任务置 EXPIRED + 物理文件回收（不需要等每日清理任务）
        let after = c::text(
            "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert_eq!(
            after, "EXPIRED",
            "命中过期后任务状态应置 EXPIRED，实际 {after}"
        );
        assert!(
            tokio::fs::metadata(&path).await.is_err(),
            "过期产物的物理文件没有被回收"
        );

        // 每日清理任务：另一条「已过期但状态仍 SUCCESS」的行必须被扫到并回收
        let stale_id = Uuid::now_v7();
        let stale_no = impexp_model::build_export_task_no(&stale_id);
        impexp_model::insert_export_task(&impexp_model::NewExportTask {
            id: stale_id,
            task_no: &stale_no,
            biz_type: DICT_BIZ,
            biz_name: "字典项",
            export_name: &ctx.key("stale"),
            format: "CSV",
            query_params: Some("{}"),
            columns_snapshot: Some("[]"),
            max_attempts: 3,
            operator: Some(user),
        })
        .await
        .expect("建过期任务");
        let stale_path = persist(b"x\n", &format!("{stale_id}.csv")).await;
        c::exec_with(
            "UPDATE sys_export_task SET status = 'SUCCESS', file_path = ?, file_name = 'stale.csv', \
                    file_size = 2, expire_at = now() - interval '100 years' WHERE id = ?::uuid",
            vec![Value::Text(stale_path.clone()), Value::from(&stale_id)],
        )
        .await;
        let expired = cloudpivot::business::impexp::cleanup::expire_artifacts(100)
            .await
            .expect("过期清理");
        assert!(
            expired.iter().any(|item| item.id == stale_id.to_string()),
            "过期清理没有扫到该任务（返回 {} 条）",
            expired.len()
        );

    })
    .await;
}

// ============================================================================
// I13 模板与目标表字段不一致 → 报错不崩
// ============================================================================

/// I13 ★ 导入模板与目标表字段不一致 → 结构化报错（不 panic、不半截入库）。
#[tokio::test]
async fn i13_header_mismatch_reports_structured_error() {
    let Some(()) = c::ready().await else { return };
    c::case("I13", |_ctx| async move {
        // 表头全部写错
        let bytes = xlsx(&["错的列1", "错的列2"], &[vec!["a", "b"]]);
        let definition = global_registry().import_definition(DICT_BIZ).expect("定义");
        let ctx_import = ImportCtx {
            task_id: Uuid::now_v7(),
            operator: None,
            mode: ImportMode::Strict,
        };
        let error = validate::precheck(
            definition.as_ref(),
            &bytes,
            validate::global_dicts(),
            &validate::DbFkResolver::new(),
            &ctx_import,
        )
        .await
        .expect_err("表头完全不匹配必须报结构性错误");
        assert_eq!(
            error.status, 422,
            "结构性错误应为 422，实际 {}",
            error.status
        );
        assert!(
            error.msg.contains("字典编码") || error.msg.contains("缺"),
            "错误文案没有点出缺失的列：{}",
            error.msg
        );

        // 非 xlsx（纯文本）→ 也要结构化报错而不是 panic（实测 400 + 格式错误，不是 500）
        let junk = validate::precheck(
            definition.as_ref(),
            b"not a xlsx at all",
            validate::global_dicts(),
            &validate::DbFkResolver::new(),
            &ctx_import,
        )
        .await
        .expect_err("非 xlsx 文件必须报错");
        assert_eq!(
            junk.status, 400,
            "非 xlsx 应为 400（格式错误），实际 {}",
            junk.status
        );
        assert!(
            !junk.msg.is_empty(),
            "非 xlsx 的错误文案为空，用户无从判断原因"
        );
    })
    .await;
}

// ============================================================================
// I14 导入文件被篡改 → SHA-256 校验失败
// ============================================================================

/// I14 上传后文件被替换 → 确认入库时必须因 SHA-256 不符而拒绝（防「预检过的是 A，入库的是 B」）。
#[tokio::test]
async fn i14_tampered_import_file_rejected() {
    let Some(()) = c::ready().await else { return };
    c::case("I14", |ctx| async move {
        let role = c::make_role(&ctx, "IMP", 1).await;
        let user = c::make_user(&ctx, "importer", "HUMAN").await;
        c::bind_role(user, role).await;
        let dict = c::make_dict_type(&ctx, "DICT").await;
        let dict_code = c::text(
            "SELECT dict_code AS v FROM sys_dict_type WHERE id = ?::uuid",
            vec![Value::from(&dict)],
        )
        .await;

        let headers = dict_headers();
        let rows = [good_row(&dict_code, "50", &ctx.key("原始行"))];
        let rows_ref: Vec<Vec<&str>> = rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let bytes = xlsx(&headers, &rows_ref);
        let task = seed_import_task(&ctx, &bytes, user, ImportMode::Strict).await;

        // 篡改：把落盘文件换成另一份内容（哈希不变的是数据库里那条记录）
        let path = c::text(
            "SELECT file_path AS v FROM sys_import_task WHERE id = ?::uuid",
            vec![Value::from(&task)],
        )
        .await;
        let tampered_rows = [good_row(&dict_code, "51", &ctx.key("被替换的行"))];
        let tampered_ref: Vec<Vec<&str>> = tampered_rows
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let tampered = xlsx(&headers, &tampered_ref);
        tokio::fs::write(&path, &tampered).await.expect("篡改文件");

        let err = impexp_handler::import_confirm(
            axum::extract::Path(task.to_string()),
            c::claims(user, "importer", &[]),
        )
        .await
        .expect_err("文件被篡改时必须拒绝入库");
        assert_eq!(
            err.status, 409,
            "应为 409（内容与上传时不一致），实际 {}",
            err.status
        );
        assert!(
            err.msg.contains("SHA-256") || err.msg.contains("不一致"),
            "错误文案没说明原因：{}",
            err.msg
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM sys_dict_item WHERE dict_type_id = ?::uuid",
                vec![Value::from(&dict)]
            )
            .await,
            0,
            "篡改的文件竟然入库成功"
        );
    })
    .await;
}

// ============================================================================
// 模块别名
// ============================================================================

use cloudpivot::business::impexp::dto as impexp_dto;
use cloudpivot::business::impexp::handler as impexp_handler;
