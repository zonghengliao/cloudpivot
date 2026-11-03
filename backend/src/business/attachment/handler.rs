//! 单据附件接口（ARCH-3 §6 R-P1-3 / DC-17）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内写 `/business/attachment/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/attachment/upload` | `business:attachment:upload` | 上传 + 绑定（multipart：`doc_type` / `doc_id` / `file_role` / `remark` / `file`） |
//! | GET | `/attachment/list` | `business:attachment:list` | 按 `doc_type + doc_id` 查 |
//! | DELETE | `/attachment/{id}` | `business:attachment:list` + 行级 | 软删绑定（上传人 ∪ `business:attachment:delete`） |
//! | GET | `/attachment/download/{id}` | `business:attachment:list` | 下载（复用 storage 出流） |
//!
//! # 三个「不重造」的红线（卡面「不要做的事」）
//!
//! 1. **上传**走 [`crate::business::storage::handler::store_server_upload`] —— 与人上传、
//!    任务附件、AI 产出回传**逐字节同一套**策略校验 / 秒传 / 选后端 / 落 `sys_file`；
//! 2. **下载**走 [`crate::business::storage::handler::download_response`] —— `LOCAL` 流式、
//!    `S3` 302 预签名、`nosniff`、RFC 5987 文件名全在同一份实现里；
//! 3. **文件回收**靠 storage 的清理任务（软删 7 天后回收物理对象），本模块不另造通道。
//!
//! # 删除的级联口径
//!
//! 先软删**绑定行**（列表立刻消失、可审计），再判「这份文件还有没有别的单据在用」：
//! 没有才顺带软删 `sys_file`。**不能无脑删文件记录** —— 秒传（内容 hash 命中）会让
//! 第二个客户端的同一份 PDF 复用同一条 `sys_file`，顺手删掉会让别人的附件 404。
//!
//! # 错误出口
//!
//! 本文件是 handler 层，统一 `ResResult<T>`；数据库错误直接 `?`（`DbError → ApiError`
//! 已有 `From`），**不写多余的 `map_err(ApiError::internal(..))`**。

use axum::extract::{ConnectInfo, Multipart, Path, Query};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use std::net::SocketAddr;

use crate::business::storage;
use crate::business::storage::handler::{
    declared_content_length, download_response, server_size_error, server_upload_limit_bytes,
    store_server_upload,
};
use crate::shared::error_code;
use crate::support::log;
use crate::utils;

use super::dto::{
    self, DocAttachmentDeleteRes, DocAttachmentItemRes, DocAttachmentListRes, DocAttachmentQuery,
    DocAttachmentUploadRes,
};
use super::model::{self, MAX_ATTACHMENTS_PER_DOC};

/// 上传分类（复用 storage 已种的 `attachment` 策略：100 MB、办公文档 / 图片 / 压缩包白名单）。
///
/// 不新开分类的理由：卡面红线是「复用 storage 模块」；`attachment` 的白名单本来就覆盖
/// 合同扫描件（pdf / jpg / png）、签收单（jpg / pdf）与发票影像（pdf / ofd 以外都能放）。
const UPLOAD_CATEGORY: &str = "attachment";

/// 上传并绑定附件（`POST /business/attachment/upload`）。
///
/// # 表单字段
///
/// | 字段 | 必填 | 说明 |
/// |------|------|------|
/// | `doc_type` | 是 | `CONTRACT` / `PURCHASE_ORDER` / `STOCK_IN` / `PAYABLE` / `PAYMENT` |
/// | `doc_id` | 是 | 单据主键（UUID） |
/// | `file_role` | 否 | `CONTRACT_SCAN` / `SIGN_RECEIPT` / `INVOICE` / `OTHER`（缺省 `OTHER`） |
/// | `remark` | 否 | ≤ 200 字符 |
/// | `file` | 是 | 文件本体（字段名**必须**是 `file`） |
///
/// # 校验顺序（每一步都先于「更贵的那一步」）
///
/// 1. `Content-Length` 与服务端硬顶（**读流之前**，否则大文件会先读进内存再被拒）；
/// 2. 文本字段白名单（`doc_type` / `doc_id` / `file_role` / `remark`）；
/// 3. 单据存在性（404 —— 不允许给不存在的单据挂号）；
/// 4. 单张单据附件数上限（50）；
/// 5. 读文件 + 走 storage 策略（大小 / 扩展名 / MIME）。
///
/// # 异常情况
///
/// - 400：字段缺失 / 类型非法 / 备注超长 / 附件数已达上限 / 扩展名或 MIME 不被策略允许；
/// - 403：无 `business:attachment:upload`；
/// - 404：单据不存在或已删除；
/// - 422：超过策略或服务端大小上限（`60003`）；
/// - 500 / 502：写对象或落库失败（对象失败时不插绑定行）。
#[nik::permission(
    path = "/business/attachment/upload",
    method = "POST",
    permission = "business:attachment:upload"
)]
#[nik::post("/business/attachment/upload")]
pub async fn upload(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    multipart: Multipart,
) -> ResResult<DocAttachmentUploadRes> {
    let server_limit = server_upload_limit_bytes();
    if let Some(declared) = declared_content_length(&headers)
        && server_limit > 0
        && declared > server_limit
    {
        return Err(server_size_error(server_limit));
    }

    let (fields, file) = read_multipart(multipart, server_limit).await?;
    let meta = dto::parse_upload_meta(
        fields.doc_type.as_deref(),
        fields.doc_id.as_deref(),
        fields.file_role.as_deref(),
        fields.remark.as_deref(),
    )?;

    if !model::doc_exists(meta.doc_type, &meta.doc_id)
        .await
        .map_err(|error| utils::db_error(error, "校验单据存在性"))?
    {
        return Err(dto::doc_not_found(meta.doc_type));
    }

    let existing = model::count_by_doc(meta.doc_type, &meta.doc_id)
        .await
        .map_err(|error| utils::db_error(error, "统计单据附件"))?;
    if existing >= MAX_ATTACHMENTS_PER_DOC {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("单张单据最多 {MAX_ATTACHMENTS_PER_DOC} 个附件，请先删除不再需要的附件"),
        ));
    }

    let operator = utils::current_user_uuid(&claims);
    // 秒传标记：命中内容 hash 时 storage 会复用已有 `sys_file`（不写对象、不插行）
    let hash = storage::sigv4::hex_sha256(&file.bytes);
    let duplicated = storage::model::find_reusable_file(&hash)
        .await
        .map_err(|error| utils::db_error(error, "查询可复用文件"))?
        .is_some();

    let record = store_server_upload(
        UPLOAD_CATEGORY,
        &file.name,
        &file.mime,
        &file.bytes,
        operator,
    )
    .await?;

    let binding_id = model::insert(
        meta.doc_type,
        &meta.doc_id,
        &record.id,
        meta.file_role,
        meta.remark.as_deref(),
        operator,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入附件绑定"))?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "DOC_ATTACHMENT_UPLOAD",
            "DocAttachment",
            binding_id.to_string(),
            None,
            Some(serde_json::json!({
                "doc_type": meta.doc_type.as_str(),
                "doc_id": meta.doc_id.to_string(),
                "file_id": record.id.to_string(),
                "file_role": meta.file_role.as_str(),
                "original_name": file.name,
                "file_size": record.file_size,
                "storage_code": record.storage_code,
                "duplicated": duplicated,
            })),
        ),
    );

    tracing::info!(
        doc_type = meta.doc_type.as_str(),
        doc_id = %meta.doc_id,
        file_id = %record.id,
        file_role = meta.file_role.as_str(),
        file_size = record.file_size,
        duplicated,
        "单据附件已上传"
    );

    Ok(Res::ok(DocAttachmentUploadRes {
        id: binding_id.to_string(),
        file_id: record.id.to_string(),
        file_name: record
            .original_name
            .clone()
            .unwrap_or_else(|| file.name.clone()),
        file_size: record.file_size,
        file_role: meta.file_role.as_str().to_string(),
        doc_type: meta.doc_type.as_str().to_string(),
        doc_id: meta.doc_id.to_string(),
        duplicated,
    }))
}

/// 按单据查附件（`GET /business/attachment/list?doc_type=&doc_id=`）。
///
/// # 返回
///
/// `doc_type` / `doc_id` 回显 + `list` + `total` + `max_count`；每条带
/// `can_delete`（前端据此决定显示删除按钮，判据与后端删除接口**同一条函数**）。
///
/// # 异常情况
///
/// - 400：`doc_type` 不在白名单 / `doc_id` 不是 UUID；
/// - 404：单据不存在或已删除（**不给已删单据挂的附件留后门**）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/attachment/list",
    method = "GET",
    permission = "business:attachment:list"
)]
#[nik::get("/business/attachment/list")]
pub async fn list(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<DocAttachmentQuery>,
) -> ResResult<DocAttachmentListRes> {
    let kind = dto::parse_doc_type(&query.doc_type)?;
    let doc_id = utils::require_uuid(&query.doc_id, "doc_id")?;

    if !model::doc_exists(kind, &doc_id)
        .await
        .map_err(|error| utils::db_error(error, "校验单据存在性"))?
    {
        return Err(dto::doc_not_found(kind));
    }

    let rows = model::list_by_doc(kind, &doc_id)
        .await
        .map_err(|error| utils::db_error(error, "查询单据附件"))?;
    let list: Vec<DocAttachmentItemRes> =
        rows.iter().map(|row| dto::to_item(row, &claims)).collect();

    Ok(Res::ok(DocAttachmentListRes {
        doc_type: kind.as_str().to_string(),
        doc_id: doc_id.to_string(),
        total: u64::try_from(list.len()).unwrap_or(0),
        list,
        max_count: MAX_ATTACHMENTS_PER_DOC,
    }))
}

/// 删除附件（`DELETE /business/attachment/{id}`，软删）。
///
/// # 语义
///
/// 1. 绑定行软删（`biz_doc_attachment.deleted_at`）—— 列表立刻消失、行仍在库里可审；
/// 2. 这份文件**没有别的单据在用**时，顺带软删 `sys_file`（物理对象由 storage 的清理
///    任务在 7 天后回收）；还有别的单据在用则**只删绑定**；
/// 3. 第 2 步失败不影响第 1 步（只记 warn）—— 否则「附件已经从列表消失、用户却收到 500」
///    会让人反复重试。
///
/// # 权限：为什么路由层挂 `:upload` 而不是 `:list`
///
/// 两层判据，各管一件事：
///
/// - **路由层**用写侧码 `business:attachment:upload`：`system::authz_tests::o13` 明令写接口
///   不许挂 `:list` / `:detail`（「有权查看即可改数据」是明确的漏洞），而「能上传的人」
///   正是「应该能撤销自己上传」的那批人 —— 挂 `:upload` 让上传人不必再等一次授权；
/// - **行级**用 `business:attachment:delete` 判「删**别人**的」：见 [`super::dto::can_delete`]。
///
/// # 异常情况
///
/// - 404：附件不存在 / 已删除；
/// - 403：既不是上传人，也没有升权码；或没有 `business:attachment:upload`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/attachment/{id}",
    method = "DELETE",
    permission = "business:attachment:upload"
)]
#[nik::delete("/business/attachment/{id}")]
pub async fn remove(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<DocAttachmentDeleteRes> {
    let attachment_id = utils::require_uuid(&id, "id")?;
    let row = model::find_active(&attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询附件"))?
        .ok_or_else(dto::attachment_not_found)?;

    if !dto::can_delete(&claims, row.uploaded_by) {
        return Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            format!(
                "只能删除自己上传的附件，或持有 {} 权限",
                super::dto::PERM_DELETE
            ),
        ));
    }

    let affected = model::soft_delete(db(), &attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "删除附件绑定"))?;

    let operator = utils::current_user_uuid(&claims);
    let mut file_deleted = false;
    match model::count_by_file(&row.file_id).await {
        Ok(0) => match storage::model::soft_delete_file(&row.file_id, operator).await {
            Ok(_) => file_deleted = true,
            Err(error) => tracing::warn!(
                file_id = %row.file_id,
                error = %error,
                "附件绑定已删，但 sys_file 标记失败（等待清理任务兜底）"
            ),
        },
        Ok(remaining) => tracing::info!(
            file_id = %row.file_id,
            remaining,
            "该文件仍被其它单据引用，仅删除绑定、保留文件记录"
        ),
        Err(error) => tracing::warn!(
            file_id = %row.file_id,
            error = %error,
            "查询文件引用计数失败，跳过 sys_file 标记"
        ),
    }

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "DOC_ATTACHMENT_DELETE",
            "DocAttachment",
            attachment_id.to_string(),
            Some(serde_json::json!({
                "doc_type": row.doc_type,
                "doc_id": row.doc_id.to_string(),
                "file_id": row.file_id.to_string(),
                "file_role": row.file_role,
                "file_name": row.file_name,
            })),
            Some(serde_json::json!({ "deleted": true, "file_deleted": file_deleted })),
        ),
    );

    tracing::info!(
        attachment_id = %attachment_id,
        file_id = %row.file_id,
        file_deleted,
        "单据附件已删除"
    );

    Ok(Res::ok(DocAttachmentDeleteRes {
        id: attachment_id.to_string(),
        file_id: row.file_id.to_string(),
        deleted: affected > 0,
        file_deleted,
    }))
}

/// 下载附件（`GET /business/attachment/download/{id}`）。
///
/// # 为什么参数是绑定行 id 而不是 `file_id`
///
/// 绑定行是「这张单据的这份文件」的唯一凭据；直接暴露 `file_id` 就能拿别的单据的文件
/// （UUID v7 时间有序，枚举成本低）。下载前先查绑定行 → 再交给 storage 出流，
/// 与任务附件「先校验任务可见性、再出流」的顺序完全一致。
///
/// # 异常情况
///
/// - 400：id 不是 UUID；
/// - 404：附件不存在 / 已软删 / 物理对象丢失；
/// - 409 / 502：存储后端不可用（透传 storage 的口径）。
#[nik::permission(
    path = "/business/attachment/download/{id}",
    method = "GET",
    permission = "business:attachment:list"
)]
#[nik::get("/business/attachment/download/{id}")]
pub async fn download(Path(id): Path<String>) -> Response {
    match download_inner(&id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载的实际流程（错误走 [`ApiError`]，便于 `into_response`）。
async fn download_inner(raw_id: &str) -> std::result::Result<Response, ApiError> {
    let attachment_id = utils::require_uuid(raw_id, "id")?;
    let row = model::find_active(&attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询附件"))?
        .ok_or_else(dto::attachment_not_found)?;
    download_response(&row.file_id.to_string())
        .await
        .inspect_err(|error| {
            tracing::warn!(
                attachment_id = %attachment_id,
                file_id = %row.file_id,
                error_code = error.code,
                error = %error.msg,
                "单据附件下载失败"
            );
        })
}

// ==================== 公共辅助 ====================

/// multipart 里的文本字段（`doc_type` / `doc_id` / `file_role` / `remark`）。
#[derive(Debug, Default)]
struct RawFields {
    /// 单据类型原文。
    doc_type: Option<String>,
    /// 单据主键原文。
    doc_id: Option<String>,
    /// 文件角色原文。
    file_role: Option<String>,
    /// 备注原文。
    remark: Option<String>,
}

/// 读上来的单个文件。
#[derive(Debug)]
struct UploadedFile {
    /// 原始文件名（multipart 的 `filename`）。
    name: String,
    /// MIME（客户端声明；策略 `allowed_mimes` 为空时不校验）。
    mime: String,
    /// 文件内容。
    bytes: Vec<u8>,
}

/// 读 multipart：文本字段进 [`RawFields`]，`file` 字段读成 [`UploadedFile`]。
///
/// # 参数
///
/// - `multipart`：axum 表单提取器；
/// - `server_limit`：服务端单文件硬顶（字节，0 = 不限），读流被掐断时用于给出正确文案。
///
/// # 异常情况
///
/// - 400 + `PAYLOAD_INVALID`：表单解析失败 / 缺 `file` 字段 / 文件名为空；
/// - 422 + `60003`：body 超过服务端硬顶（框架在中途掐断）。
async fn read_multipart(
    mut multipart: Multipart,
    server_limit: u64,
) -> std::result::Result<(RawFields, UploadedFile), ApiError> {
    let mut fields = RawFields::default();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_error(error.status(), server_limit))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let file_name = field.file_name().unwrap_or("").trim().to_string();
            let mime = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|error| multipart_error(error.status(), server_limit))?
                .to_vec();
            if file_name.is_empty() {
                return Err(ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    "缺少文件名（multipart 的 filename）",
                ));
            }
            file = Some(UploadedFile {
                name: file_name,
                mime,
                bytes,
            });
            continue;
        }

        let value = field
            .text()
            .await
            .map_err(|error| multipart_error(error.status(), server_limit))?;
        match name.as_str() {
            "doc_type" => fields.doc_type = Some(value),
            "doc_id" => fields.doc_id = Some(value),
            "file_role" => fields.file_role = Some(value),
            "remark" => fields.remark = Some(value),
            // 未知字段忽略（前端可能带 `category` 一类历史字段）
            _ => {}
        }
    }

    let Some(file) = file else {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "缺少文件字段 file",
        ));
    };
    Ok((fields, file))
}

/// 读 multipart 失败 → 对外错误。
///
/// # 判定
///
/// `413 PAYLOAD_TOO_LARGE` 说明撞了框架的 `DefaultBodyLimit`（服务端中转硬顶）→
/// 翻译成 `422 + 60003`「超过服务端单文件上限」，并引导走预签名直传；其余失败仍是
/// `400 + 10006`（不掩盖真正的表单错误）。
fn multipart_error(status: axum::http::StatusCode, server_limit: u64) -> ApiError {
    if status == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        return server_size_error(server_limit);
    }
    ApiError::new(
        400,
        error_code::PAYLOAD_INVALID,
        format!("上传内容读取失败（HTTP {}）", status.as_u16()),
    )
}

/// 供测试与文档使用：本模块的权限码清单（与迁移 `V0212` 逐字对应）。
pub const PERMISSIONS: [&str; 3] = [dto::PERM_UPLOAD, dto::PERM_LIST, dto::PERM_DELETE];
