// ============================================================================
// task_handler_attachment.rs —— 附件：上传（可见性校验）与单文件读取
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::extract::Path;
use nik::prelude::*;

use super::task_handler_common::*;
use crate::business::storage;
use crate::business::task::dto::TaskUploadRes;
use crate::business::task::hermes_sync;
use crate::business::task::model::{self};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::error_code;
use crate::utils;

/// 上传任务附件（`POST /business/task/{id}/attachment`，`multipart` 字段名 `file`）。
///
/// # 分类（老大 1A 的落点）
///
/// - 人类上传 → `category='task'`（50 MB，允许办公文档 / 压缩包）；
/// - AI 上传 → `category='task_ai'`（100 MB，白名单更严、禁可执行扩展名、禁直传）。
///
/// # 命名（E14）
///
/// AI 上传的文件名若没带本任务号，**自动加** `<任务号>-` 前缀
/// （`report.md` → `TSK-202610-000123-report.md`）；人类上传保持原名。
/// 理由：同一个 AI 处理多个任务时都产出 `report.md`，附件列表里全叫一个名字。
///
/// 两者的策略校验 / 秒传 / 落库都走
/// [`crate::business::storage::handler::store_server_upload`] —— 与存储模块的
/// 通用上传**逐字节同一套**，AI 因此拿不到比人类更宽的入口。
///
/// # 异常情况
///
/// - 401：无身份；403：人类无读权限；404：任务不存在 / 看不见；
/// - 400：没有 `file` 字段 / 扩展名或 MIME 不被策略允许 / **单任务附件已达 50 个**；
/// - 413：超过策略或服务端上限；500/502：写对象或落库失败。
#[nik::no_auth("/business/task/{id}/attachment")]
#[nik::post("/business/task/{id}/attachment")]
pub async fn upload_attachment(
    claims: Option<Extension<AuthClaims>>,
    ai_actor: Option<Extension<AiTaskActor>>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> ResResult<TaskUploadRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let reader = resolve_reader(claims, ai_actor)?;
    let me = reader.principal().await?;
    // E14：AI 的产出文件要按任务号命名，所以这里要把任务号留下（人类上传不用）。
    let task_no = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)?
        .card
        .task_no;

    // T6b：单任务附件上限 50（卡面 §二 规则 2）。**读流之前**判 ——
    // 否则一个 50 MB 的文件会先被完整读进内存再被拒（白烧一次 IO 与内存）。
    let existing = crate::business::task::collab::count_attachments(task_id)
        .await
        .map_err(|error| utils::db_error(error, "统计任务附件"))?;
    if let Err(limit) = crate::business::task::collab::attachment_quota_verdict(existing) {
        return Err(ApiError::new(
            400,
            error_code::TASK_FIELD_INVALID,
            format!("单任务附件最多 {limit} 个，请先删除不再需要的附件"),
        ));
    }

    // 读流之前先用 Content-Length 判一次（框架 DefaultBodyLimit 会中途掐断 body，
    // 掐断时的错误信息是「读取失败」，很容易被误判成策略问题）
    let server_limit = storage::handler::server_upload_limit_bytes();
    if let Some(declared) = storage::handler::declared_content_length(&headers)
        && declared > server_limit
    {
        return Err(storage::handler::server_size_error(server_limit));
    }

    let (raw_name, mime_type, bytes) = read_single_upload(&mut multipart).await?;
    // E14：AI 产出文件名自动带本任务号前缀（`TSK-202610-000123-report.md`）。
    //
    // 只在 AI 侧做：人类附件叫什么叫人自己定，改名反而会破坏人的直觉。
    // 自动加而不是「只打 WARN」的理由见 [`hermes_sync::name_output_file`]。
    let file_name = if reader.is_ai() {
        let prefixed = hermes_sync::name_output_file(&task_no, &raw_name);
        if prefixed != raw_name {
            tracing::info!(
                task_no = %task_no,
                raw_name = %raw_name,
                file_name = %prefixed,
                "AI 产出文件名未带任务号，已自动加前缀"
            );
        }
        prefixed
    } else {
        raw_name
    };
    let category = if reader.is_ai() {
        hermes_sync::AI_UPLOAD_CATEGORY
    } else {
        HUMAN_UPLOAD_CATEGORY
    };
    let operator = reader.user_id();
    let record = storage::handler::store_server_upload(
        category,
        &file_name,
        &mime_type,
        &bytes,
        Some(operator),
    )
    .await?;

    // E14：秒传按「内容 hash」复用 `sys_file` 记录，同名不同内容不受影响；但同一份字节
    // 被第二个任务再传时，落到附件列表的仍是**首次上传**那个名字（记录是共用的）。
    // 只留痕不阻断 —— 改名意味着复制一条 sys_file（多一份存储），不值当。
    if reader.is_ai() && record.original_name.as_deref() != Some(file_name.as_str()) {
        tracing::warn!(
            task_no = %task_no,
            wanted = %file_name,
            kept = ?record.original_name,
            file_id = %record.id,
            "AI 产出命中内容秒传，沿用首次上传的文件名"
        );
    }

    let attachment_id = if reader.is_ai() {
        let profile = match &reader {
            TaskReader::Ai(actor) => actor.profile.clone(),
            TaskReader::Human(_) => None,
        };
        model::bind_ai_attachment(db(), task_id, record.id, profile.as_deref(), operator)
            .await
            .map_err(|error| utils::db_error(error, "绑定 AI 附件"))?
    } else {
        model::bind_human_attachment(db(), task_id, record.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "绑定任务附件"))?
    };

    tracing::info!(
        task_id = %task_id,
        file_id = %record.id,
        file_size = record.file_size,
        category,
        ai = reader.is_ai(),
        "任务附件已上传"
    );

    Ok(Res::ok(TaskUploadRes {
        attachment_id: attachment_id.to_string(),
        file_id: record.id.to_string(),
        file_name: record.original_name.clone().unwrap_or(file_name),
        file_size: record.file_size,
        source: if reader.is_ai() { "AI" } else { "HUMAN" }.to_string(),
        category: category.to_string(),
    }))
}

/// 人类上传任务附件用的分类（`V0107` 已种策略：50 MB）。
pub(super) const HUMAN_UPLOAD_CATEGORY: &str = "task";

/// 从 multipart 里读**第一个**文件字段（表单字段名 `file`，允许其它字段混排）。
///
/// # 为什么自己读而不用存储模块的 `read_single_file`
///
/// 那个函数绑定在存储模块的策略（大小 / 扩展名）上，而任务附件必须先按
/// 「人类 / AI」选分类（`task` / `task_ai`）才能取策略。这里只负责「把字节读出来」，
/// 策略校验留给 `store_server_upload`（唯一实现）。
///
/// # 异常情况
///
/// - 400 + `10006`：没有文件字段 / 读取失败；
/// - 413：单字段超过服务端上限（框架 `DefaultBodyLimit` 兜底，这里提前给白话文案）。
pub(super) async fn read_single_upload(
    multipart: &mut axum::extract::Multipart,
) -> std::result::Result<(String, String, Vec<u8>), ApiError> {
    let limit = storage::handler::server_upload_limit_bytes();
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        ApiError::new(
            400,
            error_code::PAYLOAD_INVALID,
            format!("上传表单解析失败：{error}"),
        )
    })? {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(|value| value.to_string());
        let mime = field
            .content_type()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        if name != "file" && file_name.is_none() {
            // 表单里的普通字段（如 category）直接跳过
            continue;
        }
        let bytes = field.bytes().await.map_err(|error| {
            ApiError::new(
                400,
                error_code::PAYLOAD_INVALID,
                format!("读取上传文件失败：{error}（单文件上限 {limit} 字节）"),
            )
        })?;
        if bytes.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::PAYLOAD_INVALID,
                "上传的文件是空的".to_string(),
            ));
        }
        let file_name = file_name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "upload.bin".to_string());
        return Ok((file_name, mime, bytes.to_vec()));
    }
    Err(ApiError::new(
        400,
        error_code::PAYLOAD_INVALID,
        "上传表单里没有文件字段（字段名应为 file）".to_string(),
    ))
}

// ==================== 内部工具 ====================
