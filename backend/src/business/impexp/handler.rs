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
//! | GET | `/impexp/import/{id}/errors/file` | `business:impexp:import:download` | 下载「错误行」xlsx（推荐路径） |
//! | GET | `/impexp/import/{id}/download` | `business:impexp:import:download` | 同上（**文档别名**，与上一条同实现） |
//! | POST | `/impexp/import/{id}/confirm` | `business:impexp:import:confirm` | 确认入库（批事务） |
//! | GET | `/impexp/export/sync` | `business:impexp:export:create` | 同步导出（`sync_max_rows` 内） |
//! | POST | `/impexp/import/{id}/cancel` | `business:impexp:import:cancel` | 取消（PENDING/VALIDATED 直接取消；IMPORTING 打标记） |
//! | POST | `/impexp/import/{id}/retry` | `business:impexp:import:retry` | 失败重试（新建任务，保留原任务） |
//! | POST | `/impexp/export` | `business:impexp:export:create` | 建异步导出任务 |
//! | GET | `/impexp/export/page` | `business:impexp:export:list` | 导出任务分页 |
//! | GET | `/impexp/export/{id}` | `business:impexp:export:list` | 任务详情（进度 / 产物 / 重试次数） |
//! | GET | `/impexp/export/{id}/download` | `business:impexp:export:download` | 下载产物（**过期返回 410**） |
//! | POST | `/impexp/export/{id}/cancel` | `business:impexp:export:cancel` | 取消（PENDING 直接取消；RUNNING 打标记） |
//! | POST | `/impexp/export/{id}/retry` | `business:impexp:export:retry` | 重试（FAILED/CANCELED/EXPIRED 重排队） |
//! | DELETE | `/impexp/export/{id}` | `business:impexp:export:delete` | 删除任务与产物文件 |
//!
//! 异步导出的**执行**（认领 / 进度 / 重试 / 取消中止）在
//! [`crate::business::impexp::worker`]，过期与僵尸回收在
//! [`crate::business::impexp::cleanup`]；本文件只做接口层的编排与状态校验。
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

use axum::Json;
use axum::body::Body;
use axum::extract::Multipart;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;

use crate::business::impexp::definition::{
    ExportFilter, ImportCtx, ImportDefinition, ImportMode, RowIssue,
};
use crate::business::impexp::dto::{
    ConfirmRes, ErrorPageQuery, ExportCreateReq, ExportPageQuery, ExportSyncQuery, ExportTaskRes,
    ImportErrorRes, ImportPageQuery, ImportTaskRes, TaskActionRes, TemplateQuery, UploadQuery,
    UploadRes,
};
use crate::business::impexp::model::{
    self, ExportTaskFilter, ImportTaskFilter, NewExportTask, NewImportTask,
};
use crate::business::impexp::registry::{DefinitionBrief, ensure_mode_allowed, global_registry};
use crate::business::impexp::template;
use crate::business::impexp::validate::{self, DbFkResolver, global_dicts};
use crate::business::impexp::worker;
use crate::business::shared::principal::{self, ScopedPrincipal};
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
#[nik::permission(
    path = "/business/impexp/def/list",
    method = "GET",
    permission = "business:impexp:def:list"
)]
#[nik::get("/business/impexp/def/list")]
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
#[nik::permission(
    path = "/business/impexp/import/template",
    method = "GET",
    permission = "business:impexp:import:template"
)]
#[nik::get("/business/impexp/import/template")]
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
#[nik::permission(
    path = "/business/impexp/import/upload",
    method = "POST",
    permission = "business:impexp:import:upload"
)]
#[nik::post("/business/impexp/import/upload")]
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

    let outcome = run_precheck(
        &definition,
        task_id,
        &bytes,
        utils::current_user_uuid(&claims),
        mode,
    )
    .await?;

    Ok(Res::ok(UploadRes {
        task_id: task_id.to_string(),
        task_no,
        status: "VALIDATED".to_string(),
        report: outcome,
    }))
}

/// 跑一次预检并把结果落库（上传与「重试」共用）。
///
/// # 参数
///
/// - `definition`：导入定义。
/// - `task_id`：任务主键（错误明细与会话上下文都挂在它上面）。
/// - `bytes`：上传文件内容（**调用方负责**校验 SHA-256，本函数只解析）。
/// - `operator`：操作人。
/// - `mode`：导入模式。
///
/// # 返回值
///
/// 预检报告；任务行被回写为 `VALIDATED`（无错误）或仍 `VALIDATED`（有错误行时由调用方
/// 决定能不能入库）。
///
/// # 异常情况
///
/// - 结构性问题（非 xlsx / 缺列 / 空表）：任务置 `FAILED` 并留痕后返回原错误；
/// - 数据库异常：返回 500。
async fn run_precheck(
    definition: &std::sync::Arc<dyn ImportDefinition>,
    task_id: uuid::Uuid,
    bytes: &[u8],
    operator: Option<uuid::Uuid>,
    mode: ImportMode,
) -> std::result::Result<validate::PrecheckReport, ApiError> {
    let ctx = ImportCtx {
        task_id,
        operator,
        mode,
    };
    let resolver = DbFkResolver::new();
    let outcome =
        match validate::precheck(definition.as_ref(), bytes, global_dicts(), &resolver, &ctx).await
        {
            Ok(report) => report,
            Err(error) => {
                // 结构性问题也要把任务置为失败并留痕，方便运营回看「他到底传了什么」
                let _ = model::update_precheck_result(
                    &task_id,
                    "FAILED",
                    0,
                    0,
                    0,
                    None,
                    Some(&error.msg),
                )
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
    Ok(outcome)
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
#[nik::permission(
    path = "/business/impexp/import/page",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[nik::get("/business/impexp/import/page")]
pub async fn import_page(
    Query(query): Query<ImportPageQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<PageData<Vec<ImportTaskRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    // 批次 B · B5（附录 C #12）：解析一次数据范围，导入任务列表按 create_by 收口。
    let principal = principal::resolve_for_claims(&claims).await?;
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
    let result = model::page_import_tasks(&filter, page, page_size, &principal)
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
#[nik::permission(
    path = "/business/impexp/import/{id}",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[nik::get("/business/impexp/import/{id}")]
pub async fn import_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<ImportTaskRes> {
    // 批次 B · B5：详情与列表同口径（越权读他人任务 → 404，而不是 200）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_task(&id, &principal).await?;
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
#[nik::permission(
    path = "/business/impexp/import/{id}/errors",
    method = "GET",
    permission = "business:impexp:import:list"
)]
#[nik::get("/business/impexp/import/{id}/errors")]
pub async fn import_errors(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<ErrorPageQuery>,
) -> ResResult<PageData<Vec<ImportErrorRes>>> {
    // 批次 B · B5：先按数据范围取父任务（子表按 task_id 收口，见 model::page_task_errors）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_task(&id, &principal).await?;
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
#[nik::permission(
    path = "/business/impexp/import/{id}/errors/file",
    method = "GET",
    permission = "business:impexp:import:download"
)]
#[nik::get("/business/impexp/import/{id}/errors/file")]
pub async fn import_errors_file(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    // 批次 B · B5：错误行文件下载与详情同口径（越权 → 404 响应体）。
    let result = async {
        let principal = principal::resolve_for_claims(&claims).await?;
        errors_file_response(&id, &principal).await
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载导入「结果 / 错误明细」文件（**文档路径**，与 [`import_errors_file`] 同一实现）。
///
/// # 为什么有两个路径
///
/// - `docs/批次A-技术架构.md` §3.8 与 `docs/批次A-导入导出接口.md` 早期把下载写成
///   `/impexp/import/{id}/download`；A3 实现时落成了更精确的 `/import/{id}/errors/file`
///   （明确「下的是错误行 xlsx」）。A7 前端两种写法都出现过，形成「文档写的路径 404」。
/// - 修复口径：**两个都提供**。`/import/{id}/download` 作为文档承诺的对外别名，
///   与 `/errors/file` 指向同一个处理器（同一权限码 `business:impexp:import:download`），
///   文档同步改成「别名 + 推荐路径」两栏，前端可继续用任一。
///
/// 别名优于「只改文档」的理由：文档是**对外契约**，已经按文档接入的调用方不该因为
/// 实现命名更细而被迫改代码；别名只是多一条路由，没有额外逻辑与数据风险。
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
#[nik::permission(
    path = "/business/impexp/import/{id}/download",
    method = "GET",
    permission = "business:impexp:import:download"
)]
#[nik::get("/business/impexp/import/{id}/download")]
pub async fn import_download(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    // 批次 B · B5：下载与详情同口径（越权 → 404 响应体）。
    let result = async {
        let principal = principal::resolve_for_claims(&claims).await?;
        errors_file_response(&id, &principal).await
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 错误行文件响应构造。
async fn errors_file_response(
    id: &str,
    principal: &ScopedPrincipal,
) -> std::result::Result<Response, ApiError> {
    let task = load_task(id, principal).await?;
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
#[nik::permission(
    path = "/business/impexp/import/{id}/confirm",
    method = "POST",
    permission = "business:impexp:import:confirm"
)]
#[nik::post("/business/impexp/import/{id}/confirm")]
pub async fn import_confirm(
    Path(id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<ConfirmRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_task(&id, &principal).await?;
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

    // 取消检查放在**批次边界**（与导出 worker 同一取舍）：一次事务内的批量写入不再打断，
    // 但下一批开始前一定先看取消标记，且如实回写「已入库 m 行」
    if model::import_cancel_requested(&task.id)
        .await
        .map_err(|error| utils::db_error(error, "读取取消标记"))?
        .unwrap_or(false)
    {
        let message = format!("用户取消：已在批次边界中止入库，前 {imported} 行已入库");
        model::finish_import_canceled(&task.id, imported as i32, &message)
            .await
            .map_err(|error| utils::db_error(error, "回写取消状态"))?;
        return Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            message,
        ));
    }

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
/// - `claims`：当前用户（解析数据范围与脱敏规则）；
/// - `query`：`biz_type` + 业务过滤条件。
///
/// # 返回值
///
/// xlsx 文件流（敏感列已按当前用户的生效规则脱敏）。
///
/// # 异常情况
///
/// - 401 + `20005`：令牌缺少用户标识（无法判定脱敏规则，拒绝导出）；
/// - 404 + `60011`：定义未注册；
/// - 409 + `60015`：命中行数超过同步上限（异步导出属 A4）；
/// - 422：命中 0 行（E-EXP-10）；422 + `61008`：敏感列没有生效规则（fail-closed）。
#[nik::permission(
    path = "/business/impexp/export/sync",
    method = "GET",
    permission = "business:impexp:export:create"
)]
#[nik::get("/business/impexp/export/sync")]
pub async fn export_sync(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<ExportSyncQuery>,
) -> Response {
    match export_sync_response(&query, &claims).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 同步导出响应构造。
///
/// 批次 B · B6：导出必须与列表同口径——先解析**发起人**的数据范围与脱敏规则，
/// 取数走 `count_scoped` / `fetch_scoped`，出参走
/// [`crate::business::impexp::engine::format_export_rows`] 的脱敏分支。
async fn export_sync_response(
    query: &ExportSyncQuery,
    claims: &TokenClaims,
) -> std::result::Result<Response, ApiError> {
    let definition = global_registry()
        .export_definition(query.biz_type.trim())
        .ok_or_else(|| def_not_found(&query.biz_type))?;
    let filter = ExportFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        status: query.status,
        ..Default::default()
    };
    // 批次 B · B6：数据范围 + 脱敏规则都必须来自**发起人**。令牌里没有用户标识时
    // 直接拒绝导出——无法判定脱敏规则就等于无法保证不出明文（fail-closed）。
    let principal = principal::resolve_for_claims(claims).await?;
    let mask = crate::business::masking::resolve(Some(&principal), Some(claims))
        .await
        .map_err(|error| utils::db_error(error, "解析当前用户脱敏规则"))?;
    let total = definition
        .count_scoped(&filter, Some(&principal))
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
            .fetch_scoped(&filter, after_id.as_deref(), PAGE, Some(&principal))
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
    let formatted = crate::business::impexp::engine::format_export_rows(
        definition.columns(),
        &rows,
        &dicts,
        &mask,
    )
    .map_err(|error| {
        // 脱敏失败 → 拒绝导出（422），绝不返回明文文件
        ApiError::new(error.http_status(), error.error_code(), error.message())
    })?;
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
async fn load_task(
    raw_id: &str,
    principal: &ScopedPrincipal,
) -> std::result::Result<model::ImportTask, ApiError> {
    // 批次 B · B5：统一入口 —— 看不见的任务一律按 404 处理（不暴露"存在但无权"的差异）。
    let id = utils::require_uuid(raw_id, "id")?;
    model::find_import_task(&id, principal)
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
    let dir = std::path::Path::new(nik::upload_path()).join("impexp");
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
pub(crate) fn xlsx_response(bytes: Vec<u8>, file_name: &str) -> Response {
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

// ==================== 异步导出（A4） ====================
//
// 与同步导出的分工（架构 §3.5）：≤ `sync_max_rows` 走 `GET /export/sync` 直接回文件流，
// 不落任务表；更大的量走 `POST /export` 建任务，由
// [`crate::business::impexp::worker`] 在后台执行，前端轮询 `/export/{id}` 看进度。

/// CSV 的 Content-Type。
const CSV_MIME: &str = "text/csv";

/// 超过该行数时默认改用 CSV（xlsx 写 20 万行以上内存与耗时都不划算，架构 §3.4 第三档）。
const CSV_DEFAULT_THRESHOLD: i64 = 200_000;

/// 解析导出格式：显式指定优先，否则按行数自动选。
///
/// # 参数
///
/// - `raw`：请求里的 `format`（可为空）。
/// - `total`：预计行数。
///
/// # 返回值
///
/// `XLSX` 或 `CSV`（大写）。
///
/// # 异常情况
///
/// 422：格式不在白名单。
fn resolve_export_format(raw: Option<&str>, total: i64) -> std::result::Result<String, ApiError> {
    match utils::trimmed(raw) {
        Some(value) => {
            let upper = value.to_ascii_uppercase();
            if matches!(upper.as_str(), "XLSX" | "CSV") {
                Ok(upper)
            } else {
                Err(ApiError::new(
                    422,
                    error_code::PARAM_INVALID,
                    "format 只能是 XLSX 或 CSV",
                ))
            }
        }
        None => Ok(if total > CSV_DEFAULT_THRESHOLD {
            "CSV".to_string()
        } else {
            "XLSX".to_string()
        }),
    }
}

/// 创建异步导出任务。
///
/// # 参数
///
/// - `claims`：当前用户（任务归属）。
/// - `req`：`biz_type` + 过滤条件 + 可选格式与文件名。
///
/// # 返回值
///
/// 新建任务（状态 `PENDING`，`progress = 0`）；真正的执行由 worker 在数秒内接手。
///
/// # 异常情况
///
/// - 404 + `60011`：定义未注册；
/// - 422：过滤后 0 行 / 格式非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/impexp/export",
    method = "POST",
    permission = "business:impexp:export:create"
)]
#[nik::post("/business/impexp/export")]
pub async fn export_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<ExportCreateReq>,
) -> ResResult<ExportTaskRes> {
    // 批次 B · B5：创建任务是**单条写入**，但仍要先解析范围 —— 下面的读回
    // （`find_export_task`）与列表同口径，看不见的任务不会被当成创建成功返回。
    let principal = principal::resolve_for_claims(&claims).await?;
    let biz_type = req.biz_type.trim().to_string();
    let definition = global_registry()
        .export_definition(&biz_type)
        .ok_or_else(|| def_not_found(&biz_type))?;
    let filter = ExportFilter {
        keyword: utils::trimmed(req.keyword.as_deref()).map(str::to_string),
        status: req.status,
        ..Default::default()
    };
    // 批次 B · B6：建任务时的「命中行数」也必须按**发起人的数据范围**统计（复用上面
    // B5 解析好的 `principal`），否则会出现「列表只看得到 5 个人、导出任务却报 5000 行、
    // 产物却是 5 行」的口径错位（更糟的是：不过滤范围时，这个行数本身就是一次越权侦察）。
    let total = definition
        .count_scoped(&filter, Some(&principal))
        .await
        .map_err(|error| utils::db_error(error, "统计导出行数"))?;
    if total <= 0 {
        return Err(ApiError::new(
            422,
            error_code::PARAM_INVALID,
            "当前筛选条件下没有可导出的数据",
        ));
    }
    let format = resolve_export_format(req.format.as_deref(), total)?;
    let export_name = utils::trimmed(req.export_name.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}_{}",
                definition.biz_name(),
                chrono::Local::now().format("%Y%m%d_%H%M%S")
            )
        });

    let task_id = uuid::Uuid::now_v7();
    let task_no = model::build_export_task_no(&task_id);
    // 过滤条件与列定义都存快照：定义改版后，历史任务的进度/产物语义仍然自洽
    let query_params = serde_json::json!({
        "keyword": filter.keyword,
        "status": filter.status,
    })
    .to_string();
    let columns_snapshot = serde_json::to_string(
        &definition
            .columns()
            .iter()
            .map(|column| {
                serde_json::json!({
                    "key": column.key,
                    "header": column.header,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    model::insert_export_task(&NewExportTask {
        id: task_id,
        task_no: &task_no,
        biz_type: definition.biz_type(),
        biz_name: definition.biz_name(),
        export_name: &export_name,
        format: &format,
        query_params: Some(&query_params),
        columns_snapshot: Some(&columns_snapshot),
        max_attempts: worker::DEFAULT_MAX_ATTEMPTS,
        operator: utils::current_user_uuid(&claims),
    })
    .await
    .map_err(|error| utils::db_error(error, "创建导出任务"))?;

    let task = model::find_export_task(&task_id, &principal)
        .await
        .map_err(|error| utils::db_error(error, "查询导出任务"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "导出任务创建后未能读回，请稍后重试",
            )
        })?;
    tracing::info!(
        task_id = %task.id,
        task_no = %task.task_no,
        total = total,
        format = %format,
        "已创建异步导出任务"
    );
    Ok(Res::ok(ExportTaskRes::from_model(task)))
}

/// 导出任务分页。
///
/// # 参数
///
/// - `query`：分页与过滤（默认只看自己创建的任务）。
///
/// # 返回值
///
/// 按创建时间倒序的分页数据。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/impexp/export/page",
    method = "GET",
    permission = "business:impexp:export:list"
)]
#[nik::get("/business/impexp/export/page")]
pub async fn export_page(
    Query(query): Query<ExportPageQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<PageData<Vec<ExportTaskRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    // 批次 B · B5（附录 C #13）：导出任务列表同样按 create_by 收口（only_mine=false 不再放开全站）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let only_mine = query.only_mine.unwrap_or(true);
    let filter = ExportTaskFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        status: utils::trimmed(query.status.as_deref()).map(str::to_string),
        biz_type: utils::trimmed(query.biz_type.as_deref()).map(str::to_string),
        create_by: if only_mine {
            utils::current_user_uuid(&claims)
        } else {
            None
        },
    };
    let result = model::page_export_tasks(&filter, page, page_size, &principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询导出任务"))?;
    let list = result
        .list
        .into_iter()
        .map(ExportTaskRes::from_model)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 导出任务详情（前端轮询这个接口看进度）。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 任务详情（进度 / 产物信息 / 重试次数 / 失败原因）。
///
/// # 异常情况
///
/// 404：任务不存在；400：ID 格式非法。
#[nik::permission(
    path = "/business/impexp/export/{id}",
    method = "GET",
    permission = "business:impexp:export:list"
)]
#[nik::get("/business/impexp/export/{id}")]
pub async fn export_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<ExportTaskRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_export_task(&id, &principal).await?;
    Ok(Res::ok(ExportTaskRes::from_model(task)))
}

/// 下载导出产物。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 文件流（本地磁盘流式下发，不整文件进内存）；响应头带 `X-File-SHA256` 供客户端校验。
///
/// # 异常情况
///
/// - **410 + `60016`**：产物已过期（`expire_at` 已过或已被清理任务回收）；
/// - 409 + `60015`：任务未完成（`PENDING` / `RUNNING` / `FAILED` / `CANCELED`）；
/// - 404：任务不存在。
#[nik::permission(
    path = "/business/impexp/export/{id}/download",
    method = "GET",
    permission = "business:impexp:export:download"
)]
#[nik::get("/business/impexp/export/{id}/download")]
pub async fn export_download(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let principal = principal::resolve_for_claims(&claims).await?;
        export_download_response(&id, &principal).await
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载响应构造（把 410 / 409 的判定集中在一处，便于单测覆盖）。
async fn export_download_response(
    raw_id: &str,
    principal: &ScopedPrincipal,
) -> std::result::Result<Response, ApiError> {
    let task = load_export_task(raw_id, principal).await?;

    if task.status == "EXPIRED" {
        return Err(expired_error());
    }
    if task.status != "SUCCESS" {
        return Err(ApiError::new(
            409,
            error_code::EXPORT_NOT_READY,
            format!(
                "任务当前状态为 {}，产物尚未就绪（完成后才能下载）",
                task.status
            ),
        ));
    }
    // 懒过期：清理任务是每天 04:40 跑，过期但还没被扫到的行状态仍是 SUCCESS
    if task.is_expired(chrono::Utc::now()) {
        let _ = model::expire_export_task(&task.id).await;
        if let Some(path) = task.file_path.as_deref() {
            let _ = tokio::fs::remove_file(path).await;
            tracing::info!(task_no = %task.task_no, path = path, "下载命中过期产物，已回收文件");
        }
        return Err(expired_error());
    }

    let Some(path) = task.file_path.clone() else {
        // 记录在库但路径被清理过：对用户而言与过期等价
        return Err(ApiError::new(
            410,
            error_code::EXPORT_FILE_MISSING,
            "导出产物文件已不存在（可能已被清理），请重新导出",
        ));
    };
    let file = tokio::fs::File::open(&path).await.map_err(|error| {
        tracing::warn!(path = path, error = %error, "打开导出产物失败");
        ApiError::new(
            410,
            error_code::EXPORT_FILE_MISSING,
            "导出产物文件已不存在（可能已被清理），请重新导出",
        )
    })?;
    let size = file
        .metadata()
        .await
        .map(|meta| meta.len())
        .unwrap_or_default();
    let file_name = task
        .file_name
        .clone()
        .unwrap_or_else(|| format!("{}.xlsx", task.task_no));
    let mime = if task.format.eq_ignore_ascii_case("CSV") {
        CSV_MIME
    } else {
        XLSX_MIME
    };

    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string())
            .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "响应头构造失败"))?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&file_name))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    if let Some(sha) = task.file_sha256.as_deref()
        && let Ok(value) = HeaderValue::from_str(sha)
    {
        headers.insert(header::HeaderName::from_static("x-file-sha256"), value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// 取消导出任务。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// `PENDING` 直接取消（`CANCELED`）；`RUNNING` 只打标记，由 worker 在批次边界中止。
///
/// # 异常情况
///
/// 409 + `60021`：当前状态不允许取消（`SUCCESS` / `FAILED` / `EXPIRED` / 终态）。
#[nik::permission(
    path = "/business/impexp/export/{id}/cancel",
    method = "POST",
    permission = "business:impexp:export:cancel"
)]
#[nik::post("/business/impexp/export/{id}/cancel")]
pub async fn export_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskActionRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_export_task(&id, &principal).await?;
    let outcome = model::cancel_export_task(&task.id)
        .await
        .map_err(|error| utils::db_error(error, "取消导出任务"))?;
    match outcome {
        Some(true) => Ok(Res::ok(TaskActionRes {
            task_id: task.id.to_string(),
            status: "CANCELED".to_string(),
            message: "任务已取消".to_string(),
        })),
        Some(false) => Ok(Res::ok(TaskActionRes {
            task_id: task.id.to_string(),
            status: "RUNNING".to_string(),
            message: "已打上取消标记，后台任务会在当前分块结束后中止".to_string(),
        })),
        None => Err(ApiError::new(
            409,
            error_code::EXPORT_STATUS_INVALID,
            format!("当前状态为 {}，不允许取消", task.status),
        )),
    }
}

/// 重试导出任务：把 `FAILED` / `CANCELED` / `EXPIRED` 重新排队。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 重排后的任务（`PENDING`，`attempt_count` 归零）。
///
/// # 说明
///
/// 导出重试**复用同一条任务行**（任务号不变，前端只维护一个 id）。这与导入重试
/// 「新建一条任务」不同：导入的原任务是「某次上传的文件 + 校验结论」，必须留档；
/// 而导出没有输入文件，同一意图重跑不产生歧义。
///
/// # 异常情况
///
/// 409 + `60021`：当前状态不允许重试（`PENDING` / `RUNNING` / `SUCCESS`）。
#[nik::permission(
    path = "/business/impexp/export/{id}/retry",
    method = "POST",
    permission = "business:impexp:export:retry"
)]
#[nik::post("/business/impexp/export/{id}/retry")]
pub async fn export_retry(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<ExportTaskRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_export_task(&id, &principal).await?;
    let affected = model::requeue_export_task(&task.id)
        .await
        .map_err(|error| utils::db_error(error, "重试导出任务"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::EXPORT_STATUS_INVALID,
            format!(
                "当前状态为 {}，只有失败 / 取消 / 过期的任务才能重试",
                task.status
            ),
        ));
    }
    if let Some(path) = task.file_path.as_deref() {
        let _ = tokio::fs::remove_file(path).await;
    }
    let refreshed = model::find_export_task(&task.id, &principal)
        .await
        .map_err(|error| utils::db_error(error, "查询导出任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "导出任务不存在"))?;
    Ok(Res::ok(ExportTaskRes::from_model(refreshed)))
}

/// 删除导出任务与产物文件（软删任务行）。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// 删除后的状态说明。
///
/// # 异常情况
///
/// 409 + `60021`：任务正在执行（`RUNNING`），不允许删除（先取消）。
#[nik::permission(
    path = "/business/impexp/export/{id}",
    method = "DELETE",
    permission = "business:impexp:export:delete"
)]
#[nik::delete("/business/impexp/export/{id}")]
pub async fn export_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskActionRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_export_task(&id, &principal).await?;
    let affected = model::soft_delete_export_task(&task.id)
        .await
        .map_err(|error| utils::db_error(error, "删除导出任务"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::EXPORT_STATUS_INVALID,
            "任务正在执行（RUNNING），请先取消再删除",
        ));
    }
    if let Some(path) = task.file_path.as_deref() {
        let _ = tokio::fs::remove_file(path).await;
    }
    Ok(Res::ok(TaskActionRes {
        task_id: task.id.to_string(),
        status: "DELETED".to_string(),
        message: "任务与产物已删除".to_string(),
    }))
}

// ==================== 导入任务取消 / 重试（A4） ====================

/// 取消导入任务。
///
/// # 参数
///
/// - `id`：任务 ID。
///
/// # 返回值
///
/// `PENDING` / `PARSING` / `VALIDATED` 直接取消；`IMPORTING` 打标记，
/// `import_confirm` 会在下一个批次前中止并回写已入库行数。
///
/// # 异常情况
///
/// 409 + `60013`：当前状态不允许取消（`SUCCESS` / `PARTIAL` / `FAILED` / 已取消）。
#[nik::permission(
    path = "/business/impexp/import/{id}/cancel",
    method = "POST",
    permission = "business:impexp:import:cancel"
)]
#[nik::post("/business/impexp/import/{id}/cancel")]
pub async fn import_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskActionRes> {
    // 批次 B · B5：写操作同样先按数据范围取任务（看不见的任务不能取消）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_task(&id, &principal).await?;
    let outcome = model::cancel_import_task(&task.id)
        .await
        .map_err(|error| utils::db_error(error, "取消导入任务"))?;
    match outcome {
        Some(true) => Ok(Res::ok(TaskActionRes {
            task_id: task.id.to_string(),
            status: "CANCELED".to_string(),
            message: "任务已取消".to_string(),
        })),
        Some(false) => Ok(Res::ok(TaskActionRes {
            task_id: task.id.to_string(),
            status: "IMPORTING".to_string(),
            message: "已打上取消标记，入库会在当前批次结束后中止".to_string(),
        })),
        None => Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            format!("当前状态为 {}，不允许取消", task.status),
        )),
    }
}

/// 重试导入任务：**新建一条任务**并重跑预检（原任务保留，审计可追溯）。
///
/// # 参数
///
/// - `id`：原任务 ID。
/// - `claims`：当前用户（新任务归属）。
///
/// # 返回值
///
/// 新任务 ID + 完整预检报告（与上传接口同款）。
///
/// # 异常情况
///
/// - 409 + `60013`：当前状态不允许重试（未到失败 / 取消态）；
/// - 404：原任务的上传文件已被回收，需重新上传。
#[nik::permission(
    path = "/business/impexp/import/{id}/retry",
    method = "POST",
    permission = "business:impexp:import:retry"
)]
#[nik::post("/business/impexp/import/{id}/retry")]
pub async fn import_retry(
    Path(id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UploadRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let task = load_task(&id, &principal).await?;
    if !matches!(task.status.as_str(), "FAILED" | "CANCELED" | "PARTIAL") {
        return Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            format!(
                "当前状态为 {}，只有失败 / 取消 / 部分成功的任务才能重试",
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
            "原任务的上传文件已被回收，请重新上传文件",
        )
    })?;
    let bytes = tokio::fs::read(&file_path).await.map_err(|error| {
        ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("读取原上传文件失败（可能已被回收）：{error}"),
        )
    })?;
    if let Some(expected) = task.file_sha256.as_deref()
        && sha256_hex(&bytes) != expected
    {
        return Err(ApiError::new(
            409,
            error_code::IMPORT_STATUS_INVALID,
            "原上传文件内容已变化（SHA-256 校验失败），请重新上传",
        ));
    }

    let mode = ImportMode::parse(&task.mode).unwrap_or(ImportMode::Strict);
    let mode = ensure_mode_allowed(definition.allow_partial(), mode)?;
    let operator = utils::current_user_uuid(&claims);
    let new_id = uuid::Uuid::now_v7();
    let new_task_no = model::build_import_task_no(&new_id);
    let file_name = task
        .file_name
        .clone()
        .unwrap_or_else(|| "import.xlsx".to_string());
    // 来源留档：新任务的 options 里记下「从哪条任务重试而来」，审计可回溯
    let options = serde_json::json!({ "retry_from": task.id.to_string() }).to_string();
    model::insert_import_task(&NewImportTask {
        id: new_id,
        task_no: &new_task_no,
        biz_type: definition.biz_type(),
        biz_name: definition.biz_name(),
        file_path: &file_path,
        file_sha256: task.file_sha256.as_deref().unwrap_or_default(),
        file_name: &file_name,
        file_size: task.file_size.unwrap_or(bytes.len() as i64),
        mode: mode.as_str(),
        operator,
        options: Some(&options),
    })
    .await
    .map_err(|error| utils::db_error(error, "创建重试任务"))?;

    let outcome = run_precheck(&definition, new_id, &bytes, operator, mode).await?;
    tracing::info!(
        source_task = %task.task_no,
        new_task = %new_task_no,
        total = outcome.total,
        error = outcome.error,
        "导入任务重试已新建任务并完成预检"
    );
    Ok(Res::ok(UploadRes {
        task_id: new_id.to_string(),
        task_no: new_task_no,
        status: "VALIDATED".to_string(),
        report: outcome,
    }))
}

/// 按 ID 装载导出任务。
///
/// # 异常情况
///
/// 400：ID 格式非法；404：任务不存在（含已软删）。
async fn load_export_task(
    raw_id: &str,
    principal: &ScopedPrincipal,
) -> std::result::Result<model::ExportTask, ApiError> {
    let id = utils::require_uuid(raw_id, "id")?;
    model::find_export_task(&id, principal)
        .await
        .map_err(|error| utils::db_error(error, "查询导出任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "导出任务不存在"))
}

/// 410：产物已过期。
fn expired_error() -> ApiError {
    ApiError::new(
        410,
        error_code::EXPORT_EXPIRED,
        "导出结果已过期（可下载时间已结束），请重新发起导出",
    )
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
