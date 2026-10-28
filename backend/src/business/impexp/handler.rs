//! 导入导出接口（批次 A · A3）。
//!
//! # 接口清单（前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/impexp/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/impexp/def/list` | `business:impexp:def:list` | 已注册定义（前端下拉） |
//! | GET | `/impexp/import/template` | `business:impexp:import:template` | 下载导入模板（三要素 xlsx） |
//! | POST | `/impexp/import/upload` | `business:impexp:import:upload` | 上传 + 同步预检，返回任务与报告 |
//! | GET | `/impexp/import/page` | `business:impexp:import:list` | 导入任务分页 |
//! | GET | `/impexp/import/{id}` | `business:impexp:import:list` | 任务详情（含错误摘要） |
//! | GET | `/impexp/import/{id}/errors` | `business:impexp:import:list` | 错误明细分页（行号 / 列 / 原因） |
//! | GET | `/impexp/import/{id}/errors/file` | `business:impexp:import:download` | 下载「错误行」xlsx |
//! | POST | `/impexp/import/{id}/confirm` | `business:impexp:import:confirm` | 确认入库（批事务） |
//! | GET | `/impexp/export/sync` | `business:impexp:export:create` | 同步导出（`sync_max_rows` 内） |
//!
//! 异步导出 / 取消 / 重试 / 清理属 A4，本卡不提供。
//!
//! # 事务边界（架构文档 §3.4）
//!
//! - 上传只写任务行 + 校验结果，不碰业务表；
//! - 确认入库**按批事务**：每批 `batch_size` 行动一个事务，失败只回滚该批；
//!   `STRICT` 模式遇第一批失败即终止，并如实回写「第 n 批（行 x–y）失败，前 m 行已入库」。
//!
//! # 文件落地
//!
//! 上传文件写 `{upload_path}/impexp/{task_id}.xlsx` 并记录 SHA-256；确认入库时重新读盘、
//! 重新校验（**校验结果不信任客户端**），并核对文件未被篡改。

use std::collections::HashMap;

use alun::prelude::*;
use axum::extract::Multipart;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::business::impexp::definition::{ExportFilter, ImportCtx, ImportMode, RowIssue};
use crate::business::impexp::dto::{
    ConfirmRes, ErrorPageQuery, ExportSyncQuery, ImportErrorRes, ImportPageQuery, ImportTaskRes,
    TemplateQuery, UploadQuery, UploadRes,
};
use crate::business::impexp::model::{self, ImportTaskFilter, NewImportTask};
use crate::business::impexp::registry::{DefinitionBrief, ensure_mode_allowed, global_registry};
use crate::business::impexp::template;
use crate::business::impexp::validate::{self, DbFkResolver, global_dicts};
use crate::shared::error_code;
use crate::utils;

/// xlsx 的 Content-Type。
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// 一次预检最多落库的错误明细条数（`validate::MAX_ERRORS`）。
const MAX_PERSIST_ERRORS: usize = validate::MAX_ERRORS;

/// 错误摘要 JSON 里保留的条数（PRD：前 100 条，列表页直接回显）。
const SUMMARY_LIMIT: usize = 100;

// ==================== 定义列表 ====================

/// 已注册的导入 / 导出定义。
///
/// # 返回值
///
/// 按 `biz_type` 升序的定义摘要列表。
///
/// # 异常情况
///
/// 403：无 `business:impexp:def:list`。
#[alun::permission(
    path = "/business/impexp/def/list",
    method = "GET",
    permission = "business:impexp:def:list"
)]
#[alun::get("/business/impexp/def/list")]
pub async fn def_list() -> ResResult<Vec<DefinitionBrief>> {
    Ok(Res::ok(global_registry().briefs()))
}

// ==================== 模板下载 ====================

/// 下载导入模板。
///
/// # 参数
///
/// - `query`：`biz_type`（业务编码）。
///
/// # 返回值
///
/// xlsx 文件流（`Content-Disposition` 带中文名，RFC 5987 编码）。
///
/// # 异常情况
///
/// 404 + `60011`：业务编码未注册；500：模板生成失败。
#[alun::permission(
    path = "/business/impexp/import/template",
    method = "GET",
    permission = "business:impexp:import:template"
)]
#[alun::get("/business/impexp/import/template")]
pub async fn import_template(Query(query): Query<TemplateQuery>) -> Response {
    match build_template_response(&query.biz_type) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 模板响应构造（handler 与单测共用）。
fn build_template_response(biz_type: &str) -> std::result::Result<Response, ApiError> {
    let definition = global_registry()
        .import_definition(biz_type.trim())
        .ok_or_else(|| def_not_found(biz_type))?;
    let bytes = template::build_template(definition.as_ref())?;
    Ok(xlsx_response(
        bytes,
        &template::template_file_name(definition.as_ref()),
    ))
}

// ==================== 上传 + 预检 ====================

/// 上传导入文件并同步预检。
///
/// # 参数
///
/// - `query`：`biz_type` / `mode`。
/// - `claims`：当前用户（任务归属）。
/// - `multipart`：multipart 表单，字段名固定 `file`。
///
/// # 返回值
///
/// 任务 ID + 完整预检报告（`total / valid / error`、错误明细、外键 SQL 次数、耗时）。
///
/// # 异常情况
///
/// - 400/422：文件格式 / 表头 / 空文件（E-IMP-01/02/03）；
/// - 404 + `60011`：定义未注册；
/// - 422 + `60012`：超过 `max_rows`；
/// - 413：超过上传大小上限。
#[alun::permission(
    path = "/business/impexp/import/upload",
    method = "POST",
    permission = "business:impexp:import:upload"
)]
#[alun::post("/business/impexp/import/upload")]
pub async fn import_upload(
    Query(query): Query<UploadQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    multipart: Multipart,
) -> ResResult<UploadRes> {
    let definition = global_registry()
        .import_definition(query.biz_type.trim())
        .ok_or_else(|| def_not_found(&query.biz_type))?;
    let mode = ImportMode::parse(query.mode.as_deref().unwrap_or("")).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "mode 只能是 STRICT 或 PARTIAL",
        )
    })?;
    let mode = ensure_mode_allowed(definition.allow_partial(), mode)?;

    let (file_name, bytes) = read_upload_file(multipart).await?;
    let max_bytes = (cfg().upload.max_size_mb as usize).saturating_mul(1024 * 1024);
    if bytes.len() > max_bytes {
        return Err(ApiError::new(
            413,
            error_code::STORAGE_SIZE_EXCEEDED,
            format!("文件超过上传上限 {} MB", cfg().upload.max_size_mb),
        ));
    }

    let task_id = uuid::Uuid::now_v7();
    let task_no = model::build_import_task_no(&task_id);
    let file_sha256 = sha256_hex(&bytes);
    let file_path = persist_upload(task_id, &bytes).await?;

    model::insert_import_task(&NewImportTask {
        id: task_id,
        task_no: &task_no,
        biz_type: definition.biz_type(),
        biz_name: definition.biz_name(),
        file_path: &file_path,
        file_sha256: &file_sha256,
        file_name: &file_name,
        file_size: bytes.len() as i64,
        mode: mode.as_str(),
        operator: utils::current_user_uuid(&claims),
        options: Some("{}"),
    })
    .await
    .map_err(|error| utils::db_error(error, "创建导入任务"))?;

    let ctx = ImportCtx {
        task_id,
        operator: utils::current_user_uuid(&claims),
        mode,
    };
    let resolver = DbFkResolver::new();
    let outcome = match validate::precheck(
        definition.as_ref(),
        &bytes,
        global_dicts(),
        &resolver,
        &ctx,
    )
    .await
    {
        Ok(report) => report,
        Err(error) => {
            // 结构性问题也要把任务置为失败并留痕，方便运营回看「他到底传了什么」
            let _ =
                model::update_precheck_result(&task_id, "FAILED", 0, 0, 0, None, Some(&error.msg))
                    .await;
            return Err(error);
        }
    };

    model::insert_task_errors(
        &task_id,
        &outcome.errors[..outcome.errors.len().min(MAX_PERSIST_ERRORS)],
    )
    .await
    .map_err(|error| utils::db_error(error, "写入导入错误明细"))?;
    let summary = serde_json::to_string(&summary_rows(&outcome.errors)).unwrap_or_default();
    model::update_precheck_result(
        &task_id,
        "VALIDATED",
        outcome.total as i32,
        outcome.valid as i32,
        outcome.error as i32,
        Some(&summary),
        None,
    )
    .await
    .map_err(|error| utils::db_error(error, "回写预检结果"))?;

    Ok(Res::ok(UploadRes {
        task_id: task_id.to_string(),
        task_no,
        status: "VALIDATED".to_string(),
        report: outcome,
    }))
}

// ==================== 任务查询 ====================

/// 导入任务分页。
///
/// # 参数
///
/// - `query`：分页与过滤条件（默认只看自己创建的任务）。
///
/// # 返回值
///
/// 按创建时间倒序的分页数据。
///
/// # 异常情况
///
/// 500：数据库异常。
#[alun::permission(
    path = "/business/impexp/import/page",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[alun::get("/business/impexp/import/page")]
pub async fn import_page(
    Query(query): Query<ImportPageQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<PageData<Vec<ImportTaskRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let only_mine = query.only_mine.unwrap_or(true);
    let filter = ImportTaskFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        status: utils::trimmed(query.status.as_deref()).map(str::to_string),
        biz_type: utils::trimmed(query.biz_type.as_deref()).map(str::to_string),
        create_by: if only_mine {
            utils::current_user_uuid(&claims)
        } else {
            None
        },
    };
    let result = model::page_import_tasks(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询导入任务"))?;
    let list = result
        .list
        .into_iter()
        .map(ImportTaskRes::from_model)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 导入任务详情。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 任务详情（含前 100 条错误摘要）。
///
/// # 异常情况
///
/// 404：任务不存在；400：ID 格式非法。
#[alun::permission(
    path = "/business/impexp/import/{id}",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[alun::get("/business/impexp/import/{id}")]
pub async fn import_detail(Path(id): Path<String>) -> ResResult<ImportTaskRes> {
    let task = load_task(&id).await?;
    Ok(Res::ok(ImportTaskRes::from_model(task)))
}

/// 错误明细分页。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `query`：分页参数。
///
/// # 返回值
///
/// 按行号升序的错误明细（行号 + 列名 + 原因）。
///
/// # 异常情况
///
/// 404：任务不存在。
#[alun::permission(
    path = "/business/impexp/import/{id}/errors",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[alun::get("/business/impexp/import/{id}/errors")]
pub async fn import_errors(
    Path(id): Path<String>,
    Query(query): Query<ErrorPageQuery>,
) -> ResResult<PageData<Vec<ImportErrorRes>>> {
    let task = load_task(&id).await?;
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_task_errors(&task.id, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询错误明细"))?;
    let list = result
        .list
        .into_iter()
        .map(ImportErrorRes::from_model)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 下载「错误行」xlsx。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 三列（行号 / 列名 / 错误原因）的 xlsx。
///
/// # 异常情况
///
/// 404：任务不存在。
#[alun::permission(
    path = "/business/impexp/import/{id}/errors/file",
    method = "GET",
    permission = "business:impexp:import:download"
)]
#[alun::get("/business/impexp/import/{id}/errors/file")]
pub async fn import_errors_file(Path(id): Path<String>) -> Response {
    match errors_file_response(&id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 错误行文件响应构造。
async fn errors_file_response(id: &str) -> std::result::Result<Response, ApiError> {
    let task = load_task(id).await?;
    let rows = model::list_task_errors(&task.id, MAX_PERSIST_ERRORS as i64)
        .await
        .map_err(|error| utils::db_error(error, "查询错误明细"))?;
    let lines = rows
        .into_iter()
        .map(|row| {
            (
                row.row_no as u32,
                row.column_header.or(row.column_key),
                row.error_message,
            )
        })
        .collect::<Vec<_>>();
    let bytes = template::build_error_sheet_rows(&lines)?;
    Ok(xlsx_response(
        bytes,
        &format!("{}_错误明细.xlsx", task.task_no),
    ))
}

// ==================== 确认入库 ====================

/// 确认入库。
///
/// # 参数
///
/// - `id`：任务 ID。
/// - `claims`：当前用户（审计字段）。
///
/// # 返回值
///
/// 入库结果（状态 / 实际入库行数）。
///
/// # 异常情况
///
/// - 409 + `60013`：状态不是 `VALIDATED`（状态机非法跃迁）；
/// - 422 + `60014`：`STRICT` 模式下存在失败行或某批入库失败；
/// - 404：任务不存在 / 文件已丢失。
#[alun::permission(
    path = "/business/impexp/import/{id}/confirm",
    method = "POST",
    permission = "business:impexp:import:confirm"
)]
#[alun::post("/business/impexp/import/{id}/confirm")]
pub async fn import_confirm(
    Path(id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<ConfirmRes> {
    let task = load_task(&id).await?;
    if task.status != "VALIDATED" {
        return Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            format!(
                "当前任务状态为 {}，只有校验完成（VALIDATED）的任务可以确认入库",
                task.status
            ),
        ));
    }
    let definition = global_registry()
        .import_definition(&task.biz_type)
        .ok_or_else(|| def_not_found(&task.biz_type))?;
    let file_path = task.file_path.clone().ok_or_else(|| {
        ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "导入文件已丢失，请重新上传",
        )
    })?;
    let bytes = tokio::fs::read(&file_path).await.map_err(|error| {
        ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("读取导入文件失败：{error}"),
        )
    })?;
    if let Some(expected) = task.file_sha256.as_deref()
        && sha256_hex(&bytes) != expected
    {
        return Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            "导入文件内容与上传时不一致（SHA-256 校验失败），请重新上传",
        ));
    }

    let mode = ImportMode::parse(&task.mode).unwrap_or(ImportMode::Strict);
    let ctx = ImportCtx {
        task_id: task.id,
        operator: utils::current_user_uuid(&claims),
        mode,
    };
    // 重新校验：不信任上传阶段的结论（文件可能被替换、字典 / 外键在此期间变化）
    let outcome = validate::validate(
        definition.as_ref(),
        &bytes,
        global_dicts(),
        &DbFkResolver::new(),
        &ctx,
        true,
    )
    .await?;

    if mode == ImportMode::Strict && outcome.report.error > 0 {
        let message = format!(
            "存在 {} 行校验失败，本批次未开启「允许部分导入」，已全部中止，请修正后重传",
            outcome.report.error
        );
        model::update_import_result(&task.id, "FAILED", 0, Some(&message))
            .await
            .map_err(|error| utils::db_error(error, "回写入库结果"))?;
        return Err(ApiError::new(422, error_code::IMPORT_BATCH_FAILED, message));
    }

    model::update_status(&task.id, "IMPORTING", None)
        .await
        .map_err(|error| utils::db_error(error, "更新任务状态"))?;

    let batch_size = definition.batch_size().max(1);
    let total_batches = outcome.rows.len().div_ceil(batch_size);
    let mut imported = 0usize;

    for (batch_index, batch) in outcome.rows.chunks(batch_size).enumerate() {
        let rows = batch.to_vec();
        let definition_ref = std::sync::Arc::clone(&definition);
        let ctx_ref = ctx.clone();
        let result = db()
            .transaction(|tx| async move { definition_ref.persist(&tx, &rows, &ctx_ref).await })
            .await;
        match result {
            Ok(affected) => imported += affected,
            Err(error) => {
                let first_row = batch.first().map(|row| row.row_no).unwrap_or(0);
                let last_row = batch.last().map(|row| row.row_no).unwrap_or(0);
                if mode == ImportMode::Strict {
                    let message = format!(
                        "第 {} 批（行 {first_row}–{last_row}）入库失败，前 {imported} 行已入库；请按已入库范围修正后重导（原因：{error}）",
                        batch_index + 1
                    );
                    model::update_import_result(
                        &task.id,
                        "FAILED",
                        imported as i32,
                        Some(&message),
                    )
                    .await
                    .map_err(|db_error| utils::db_error(db_error, "回写入库结果"))?;
                    return Err(ApiError::new(422, error_code::IMPORT_BATCH_FAILED, message));
                }
                tracing::warn!(
                    task_id = %task.id,
                    batch = batch_index + 1,
                    error = %error,
                    "PARTIAL 模式：跳过失败批次"
                );
            }
        }
    }

    let status = if mode == ImportMode::Partial && outcome.report.error > 0 {
        "PARTIAL"
    } else {
        "SUCCESS"
    };
    let message = format!(
        "共 {total_batches} 批，入库 {imported} 行（校验失败 {} 行已跳过）",
        outcome.report.error
    );
    model::update_import_result(&task.id, status, imported as i32, Some(&message))
        .await
        .map_err(|error| utils::db_error(error, "回写入库结果"))?;

    Ok(Res::ok(ConfirmRes {
        task_id: task.id.to_string(),
        status: status.to_string(),
        imported_rows: imported as i32,
        message,
    }))
}

// ==================== 同步导出 ====================

/// 同步导出（≤ `sync_max_rows`）。
///
/// # 参数
///
/// - `query`：`biz_type` + 业务过滤条件。
///
/// # 返回值
///
/// xlsx 文件流。
///
/// # 异常情况
///
/// - 404 + `60011`：定义未注册；
/// - 409 + `60015`：命中行数超过同步上限（异步导出属 A4）；
/// - 422：命中 0 行（E-EXP-10）。
#[alun::permission(
    path = "/business/impexp/export/sync",
    method = "GET",
    permission = "business:impexp:export:create"
)]
#[alun::get("/business/impexp/export/sync")]
pub async fn export_sync(Query(query): Query<ExportSyncQuery>) -> Response {
    match export_sync_response(&query).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 同步导出响应构造。
async fn export_sync_response(query: &ExportSyncQuery) -> std::result::Result<Response, ApiError> {
    let definition = global_registry()
        .export_definition(query.biz_type.trim())
        .ok_or_else(|| def_not_found(&query.biz_type))?;
    let filter = ExportFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        status: query.status,
    };
    let total = definition
        .count(&filter)
        .await
        .map_err(|error| utils::db_error(error, "统计导出行数"))?;
    if total == 0 {
        return Err(ApiError::new(
            422,
            error_code::PARAM_INVALID,
            "当前筛选条件下没有数据可导出",
        ));
    }
    if total > definition.sync_max_rows() as i64 {
        return Err(ApiError::new(
            409,
            error_code::EXPORT_NOT_READY,
            format!(
                "数据量较大（{} 行），超过同步导出上限 {} 行，将转为后台导出（异步导出随 A4 提供）",
                total,
                definition.sync_max_rows()
            ),
        ));
    }

    // keyset 分页：每页 2000 行；页大小与「内存峰值」挂钩，10 万行走异步（A4）
    const PAGE: u32 = 2_000;
    let mut rows: Vec<HashMap<&'static str, String>> = Vec::with_capacity(total as usize);
    let mut after_id: Option<String> = None;
    loop {
        let page = definition
            .fetch(&filter, after_id.as_deref(), PAGE)
            .await
            .map_err(|error| utils::db_error(error, "查询导出数据"))?;
        if page.is_empty() {
            break;
        }
        after_id = page.last().map(|(id, _)| id.clone());
        rows.extend(page.into_iter().map(|(_, row)| row));
        if rows.len() >= total as usize {
            break;
        }
    }

    // 字典翻译（导出把码值翻回中文）
    let mut dicts = HashMap::new();
    for column in definition.columns() {
        if let crate::business::impexp::definition::ColumnKind::Dict(code) = column.kind
            && !dicts.contains_key(code)
        {
            let translation = global_dicts()
                .get(code)
                .await
                .map_err(|error| utils::db_error(error, "装载字典"))?;
            dicts.insert(code.to_string(), translation.as_ref().clone());
        }
    }

    let headers = definition
        .columns()
        .iter()
        .map(|column| column.header)
        .collect::<Vec<_>>();
    let formatted =
        crate::business::impexp::engine::format_export_rows(definition.columns(), &rows, &dicts);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &formatted)?;
    Ok(xlsx_response(
        bytes,
        &format!(
            "{}_{}.xlsx",
            definition.biz_name(),
            chrono::Local::now().format("%Y%m%d%H%M%S")
        ),
    ))
}

// ==================== 公共辅助 ====================

/// 定义未注册错误（60011）。
fn def_not_found(biz_type: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::IMPORT_DEF_NOT_FOUND,
        format!("导入导出功能「{biz_type}」未注册或已停用，请联系管理员"),
    )
}

/// 按 ID 装载任务。
///
/// # 异常情况
///
/// 400：ID 格式非法；404：任务不存在。
async fn load_task(raw_id: &str) -> std::result::Result<model::ImportTask, ApiError> {
    let id = utils::require_uuid(raw_id, "id")?;
    model::find_import_task(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询导入任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "导入任务不存在"))
}

/// 从 multipart 中读取字段名为 `file` 的文件。
///
/// # 返回值
///
/// `(原始文件名, 字节)`。
///
/// # 异常情况
///
/// 400：表单解析失败 / 没有 `file` 字段。
async fn read_upload_file(
    mut multipart: Multipart,
) -> std::result::Result<(String, Vec<u8>), ApiError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::new(400, error_code::PAYLOAD_INVALID, "上传表单解析失败"))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let file_name = field.file_name().unwrap_or("import.xlsx").to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::new(400, error_code::PAYLOAD_INVALID, "上传文件内容读取失败"))?;
        return Ok((file_name, bytes.to_vec()));
    }
    Err(ApiError::new(
        400,
        error_code::PARAM_INVALID,
        "缺少文件字段 file",
    ))
}

/// 把上传文件写到 `{upload_path}/impexp/{task_id}.xlsx`。
///
/// # 返回值
///
/// 落盘绝对/相对路径（与框架 `upload_path()` 一致，重启后仍可用）。
///
/// # 异常情况
///
/// 500：目录创建或写入失败。
async fn persist_upload(
    task_id: uuid::Uuid,
    bytes: &[u8],
) -> std::result::Result<String, ApiError> {
    let dir = std::path::Path::new(alun::upload_path()).join("impexp");
    tokio::fs::create_dir_all(&dir).await.map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("创建导入目录失败：{error}"),
        )
    })?;
    let path = dir.join(format!("{task_id}.xlsx"));
    tokio::fs::write(&path, bytes).await.map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("保存导入文件失败：{error}"),
        )
    })?;
    Ok(path.to_string_lossy().to_string())
}

/// 计算 SHA-256（小写十六进制）。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 错误明细 → 摘要 JSON（前 [`SUMMARY_LIMIT`] 条）。
fn summary_rows(errors: &[RowIssue]) -> Vec<serde_json::Value> {
    errors
        .iter()
        .take(SUMMARY_LIMIT)
        .map(|issue| {
            serde_json::json!({
                "row_no": issue.row_no,
                "column_key": issue.column_key,
                "column_header": issue.column_header,
                "error_code": issue.kind.as_str(),
                "message": issue.message,
            })
        })
        .collect()
}

/// xlsx 下载响应（RFC 5987 中文文件名）。
fn xlsx_response(bytes: Vec<u8>, file_name: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(XLSX_MIME));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(file_name))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    (headers, bytes).into_response()
}

/// 构造 `Content-Disposition`（ASCII 回退名 + RFC 5987 编码名）。
fn content_disposition(file_name: &str) -> String {
    let encoded = percent_encode(file_name);
    let ascii = file_name
        .chars()
        .map(|value| {
            if value.is_ascii() && value != '"' {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// URL 百分号编码（只保留 RFC 3986 非保留字符）。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // 空串的 SHA-256
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn content_disposition_encodes_chinese_name() {
        let value = content_disposition("字典项导入模板_v1.xlsx");
        assert!(value.starts_with("attachment;"));
        assert!(value.contains("filename*=UTF-8''"));
        // 中文必须被百分号编码，ASCII 部分保留
        assert!(value.contains("%E5%AD%97"));
        assert!(value.contains("_v1.xlsx"));
    }

    #[test]
    fn percent_encode_keeps_unreserved_characters() {
        assert_eq!(percent_encode("aZ09-._~"), "aZ09-._~");
        assert_eq!(percent_encode("中"), "%E4%B8%AD");
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    #[test]
    fn summary_keeps_only_first_hundred_rows() {
        let errors = (0..150)
            .map(|index| {
                RowIssue::row(
                    index,
                    crate::business::impexp::definition::ErrorKind::Rule,
                    "x",
                )
            })
            .collect::<Vec<_>>();
        let summary = summary_rows(&errors);
        assert_eq!(summary.len(), SUMMARY_LIMIT);
        assert_eq!(summary[0]["row_no"], serde_json::json!(0));
    }

    #[test]
    fn unknown_definition_returns_60011() {
        let error = def_not_found("nope.nope");
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::IMPORT_DEF_NOT_FOUND);
    }
}
