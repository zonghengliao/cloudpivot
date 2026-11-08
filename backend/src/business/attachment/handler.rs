//! 单据附件接口（ARCH-3 §6 R-P1-3 / DC-17）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内写 `/business/attachment/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/attachment/upload` | `business:attachment:upload` | 上传 + 绑定（multipart：`doc_type` / `doc_id` / `file_role` / `remark` / `file`） |
//! | GET | `/attachment/list` | `business:attachment:list` + 单据读码 | 按 `doc_type + doc_id` 查（`sort=time` 缺省 / `manual`） |
//! | PUT | `/attachment/sort` | `business:attachment:upload` + 单据读码 | 手动排序（ATT-03：提交完整 id 顺序，回读 `manual` 列表） |
//! | DELETE | `/attachment/{id}` | `business:attachment:list` + 行级 | 软删绑定（上传人 ∪ `business:attachment:delete`） |
//! | GET | `/attachment/download/{id}` | `business:attachment:list` + 单据读码 | 下载（复用 storage 出流 + 写 `operation_logs`） |
//!
//! 「单据读码」= 该类单据所在模块的读权限码（[`model::DocKind::view_permissions`]），
//! 由 **ATT-02** 补上：附件可见性 = 单据可见性，见 [`dto::can_view_doc`]。
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

use axum::extract::{ConnectInfo, Json, Multipart, Path, Query};
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
use crate::support::log::model::OperationLogModel;
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

/// 按单据查附件（`GET /business/attachment/list?doc_type=&doc_id=&sort=`）。
///
/// # 返回
///
/// `doc_type` / `doc_id` 回显 + `list` + `total` + `max_count` + `sort`；每条带
/// `can_delete`（前端据此决定显示删除按钮，判据与后端删除接口**同一条函数**）。
///
/// # 排序（ATT-03）
///
/// `sort` 缺省 = `time`：**按上传时间倒序**（刚传的在上）。
/// `sort=manual`：按 `sort_order` 升序（用户在面板里上移 / 下移后的顺序）。
/// 两种口径的 ORDER BY 常量只在 [`model::ListOrder::order_by`] 里，本函数不拼 SQL。
///
/// # 校验顺序（ATT-02：先判读权，再判存在）
///
/// 1. `doc_type` / `doc_id` / `sort` 合法性（400）；
/// 2. **单据读码**（403，[`dto::can_view_doc`]）—— 先于存在性判定，
///    否则「有没有这张单」会从 404/200 的差异里漏出去；
/// 3. 单据存在性（404）。
///
/// 列表与下载共用第 2 步：能列出附件却下载不了（或反之）是没有业务含义的组合。
///
/// # 异常情况
///
/// - 400：`doc_type` 不在白名单 / `doc_id` 不是 UUID / `sort` 不在白名单；
/// - 403：缺该模块的读码（`business:erp:payable:*` 等，见 [`model::DocKind::view_permissions`]）；
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
    let order = dto::parse_sort_order(query.sort.as_deref())?;

    if !dto::can_view_doc(&claims, kind) {
        tracing::warn!(
            doc_type = kind.as_str(),
            %doc_id,
            user = ?utils::current_user_uuid(&claims),
            "越权查看单据附件列表被拒（缺模块读码）"
        );
        return Err(dto::doc_forbidden(kind));
    }

    if !model::doc_exists(kind, &doc_id)
        .await
        .map_err(|error| utils::db_error(error, "校验单据存在性"))?
    {
        return Err(dto::doc_not_found(kind));
    }

    list_of(&claims, kind, doc_id, order).await
}

/// 组装列表出参（`list` / `sort` 两个接口共用：同一份投影与同一条 `can_delete` 判据）。
///
/// # 参数
///
/// - `claims`：当前用户（算 `can_delete`）；
/// - `kind` / `doc_id`：单据；
/// - `order`：排序口径（调用方已校验权限与存在性）。
async fn list_of(
    claims: &TokenClaims,
    kind: model::DocKind,
    doc_id: uuid::Uuid,
    order: model::ListOrder,
) -> ResResult<DocAttachmentListRes> {
    let rows = model::list_by_doc(kind, &doc_id, order)
        .await
        .map_err(|error| utils::db_error(error, "查询单据附件"))?;
    let list: Vec<DocAttachmentItemRes> =
        rows.iter().map(|row| dto::to_item(row, claims)).collect();

    Ok(Res::ok(DocAttachmentListRes {
        doc_type: kind.as_str().to_string(),
        doc_id: doc_id.to_string(),
        total: u64::try_from(list.len()).unwrap_or(0),
        list,
        max_count: MAX_ATTACHMENTS_PER_DOC,
        sort: order.as_str().to_string(),
    }))
}

/// 手动排序（`PUT /business/attachment/sort`，ATT-03）。
///
/// # 请求体
///
/// ```json
/// { "doc_type": "CONTRACT", "doc_id": "<uuid>", "ids": ["<附件id>", "<附件id>"] }
/// ```
///
/// `ids` 是**该单据附件的完整顺序**：第 1 个 → `sort_order = 0`，第 2 个 → 1 …
///
/// # 语义
///
/// 1. 校验单据类型 / 主键 / id 列表（400）；
/// 2. **单据读码**（403）—— 能看这张单才谈得上排它的附件；
/// 3. 单据存在性（404）；
/// 4. **成员校验**：`ids` 必须与「该单据当前未删附件」的集合**逐一对上**，
///    多一个 / 少一个都 400（见 [`dto::parse_sort_request`] 的「为什么要求完整列表」）；
/// 5. 一个事务里逐条写 `sort_order`，成功后按 `manual` 口径回读整份列表。
///
/// 返回的是**重排后的列表**（与 `GET /list?sort=manual` 同结构）：前端拿到就能直接替换，
/// 不必再补一次 GET，也避免「写完再读」之间的第二次请求失败导致界面回滚。
///
/// # 权限：为什么挂 `:upload` 而不是 `:list`
///
/// 与 `remove` 同口径（`system::authz_tests::o13`：写接口不许挂读码）：
/// 「能上传附件的人」正是「会去整理附件顺序」的那批人。单据读码另判（第 2 步），
/// 所以「只持 `attachment:upload` 但看不到这张单」的人排不了它的附件。
///
/// # 留痕
///
/// 通过审计总线投递一条 `DOC_ATTACHMENT_SORT`（与上传同口径；总线批量落库）。
/// 删除 / 下载是**直写** `operation_logs` —— 那两条的审计价值高于排序，
/// 不能容忍「进批次丢了就没了」。HTTP 层面还有全局中间件的一条 `PUT` 流水。
///
/// # 异常情况
///
/// - 400：参数非法 / `ids` 与该单据的附件集合不匹配；
/// - 403：缺 `business:attachment:upload`，或缺该单据模块的读码；
/// - 404：单据不存在 / 已删除；
/// - 500：数据库异常（事务整体回滚，不会留下写了一半的序号）。
#[nik::permission(
    path = "/business/attachment/sort",
    method = "PUT",
    permission = "business:attachment:upload"
)]
#[nik::put("/business/attachment/sort")]
pub async fn sort(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::DocAttachmentSortReq>,
) -> ResResult<DocAttachmentListRes> {
    let ticket = dto::parse_sort_request(&req)?;

    if !dto::can_view_doc(&claims, ticket.kind) {
        tracing::warn!(
            doc_type = ticket.kind.as_str(),
            doc_id = %ticket.doc_id,
            user = ?utils::current_user_uuid(&claims),
            "越权重排单据附件被拒（缺模块读码）"
        );
        return Err(dto::doc_forbidden(ticket.kind));
    }

    if !model::doc_exists(ticket.kind, &ticket.doc_id)
        .await
        .map_err(|error| utils::db_error(error, "校验单据存在性"))?
    {
        return Err(dto::doc_not_found(ticket.kind));
    }

    // 幂等前置：同一顺序重复提交不会产生任何变化，直接回读返回（少一次事务）
    let active = model::active_ids(ticket.kind, &ticket.doc_id)
        .await
        .map_err(|error| utils::db_error(error, "查询单据附件 id"))?;
    ensure_same_members(&active, &ticket.ids)?;

    let tx_kind = ticket.kind;
    let tx_doc_id = ticket.doc_id;
    let tx_ids = ticket.ids.clone();
    let updated = db()
        .transaction(
            |tx| async move { model::set_sort_orders(&tx, tx_kind, &tx_doc_id, &tx_ids).await },
        )
        .await
        .map_err(|error| utils::db_error(error, "重排单据附件"))?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            OP_SORT,
            "DocAttachment",
            ticket.doc_id.to_string(),
            None,
            Some(serde_json::json!({
                "doc_type": ticket.kind.as_str(),
                "doc_id": ticket.doc_id.to_string(),
                "count": ticket.ids.len(),
                "ordered_ids": ticket
                    .ids
                    .iter()
                    .take(SORT_LOG_ID_LIMIT)
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>(),
                "updated_rows": updated,
            })),
        ),
    );

    tracing::info!(
        doc_type = ticket.kind.as_str(),
        doc_id = %ticket.doc_id,
        count = ticket.ids.len(),
        updated_rows = updated,
        "单据附件顺序已更新"
    );

    list_of(
        &claims,
        ticket.kind,
        ticket.doc_id,
        model::ListOrder::Manual,
    )
    .await
}

/// 重排留痕的操作码（`operation_logs.operation`）。
pub const OP_SORT: &str = "DOC_ATTACHMENT_SORT";

/// 留痕里最多记录多少个 id（50 个附件的全量顺序写进 detail 会撑大日志表）。
const SORT_LOG_ID_LIMIT: usize = 10;

/// 校验 `ids` 与该单据当前未删附件的集合**逐一对上**。
///
/// # 为什么在库里比对而不是只信入参
///
/// - 入参可能混进别的单据的附件 id（越权改别人的排序）；
/// - 也可能漏掉某个附件（那一条会保留旧序号，与本轮写入的 0..n-1 撞号）；
/// - 还可能包含刚被删掉的附件。
///
/// 三种情况都让「提交的顺序 ≠ 库里的顺序」，一律 400 并说清差在哪。
///
/// # 异常情况
///
/// - 400 + `PARAM_INVALID`：多出的 id / 缺失的 id（消息点名第一个，附计数）。
///
/// # 可见性
///
/// `pub(crate)`：`tests::ensure_same_members_truth_table` 直接断言这条判据（不连库）。
pub(crate) fn ensure_same_members(
    active: &[uuid::Uuid],
    submitted: &[uuid::Uuid],
) -> std::result::Result<(), ApiError> {
    let active_set: std::collections::HashSet<uuid::Uuid> = active.iter().copied().collect();
    let submitted_set: std::collections::HashSet<uuid::Uuid> = submitted.iter().copied().collect();

    if let Some(extra) = submitted.iter().find(|id| !active_set.contains(*id)) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "ids 里有不属于该单据（或已删除）的附件：{extra}（该单据当前 {} 个附件，提交 {} 个）",
                active.len(),
                submitted.len()
            ),
        ));
    }
    if let Some(missing) = active.iter().find(|id| !submitted_set.contains(*id)) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "ids 不完整，缺少附件 {missing}（该单据当前 {} 个附件，提交 {} 个）；重排必须提交完整顺序",
                active.len(),
                submitted.len()
            ),
        ));
    }
    Ok(())
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

    let started = std::time::Instant::now();
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

    record_delete_log(
        &headers,
        &connect_info,
        &claims,
        &row,
        file_deleted,
        started,
    )
    .await;

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

/// 删除留痕的操作码（`operation_logs.operation`，ATT-03）。
pub const OP_DELETE: &str = "DOC_ATTACHMENT_DELETE";

/// 构造一条删除留痕（**纯逻辑**：不连库，单测直接断言字段）。
///
/// # 为什么删除要自己写一行（ATT-03 实测发现）
///
/// 改造前删除走 `log::record_audit(...)` → 日志总线（500 ms 批量）。在 145 隔离实例上
/// 实测：**总线里的审计行始终没落库**，`operation_logs` 里只剩全局中间件写的那条
/// `DELETE /api/business/attachment/{id}`（只有 target_id，没有单据类型 / 文件名）。
/// 而「谁删了哪张单的哪个附件」正是审计要回答的问题。
///
/// 与 [`download_log`] 同一条理由与同一种做法：直写 `operation_logs`，
/// 写库失败只 `warn`（留痕是旁路，不能反压业务）。
///
/// # 与中间件那条 `DELETE /api/business/attachment/{id}` 的关系
///
/// 两条都留：中间件是「HTTP 层面发生了什么」的通用流水，本条是「业务层面删了哪张单据的
/// 哪个附件」的结构化记录（`detail` 带 doc_type / doc_id / file_name / 大小 / 引用是否释放）。
pub(crate) fn delete_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    row: &model::DocAttachmentRow,
    file_deleted: bool,
    duration_ms: i32,
) -> OperationLogModel {
    OperationLogModel {
        id: uuid::Uuid::now_v7(),
        request_id: utils::request_id(headers),
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: OP_DELETE.to_string(),
        target_type: Some("DocAttachment".to_string()),
        target_id: Some(row.id.to_string()),
        detail: Some(serde_json::json!({
            "doc_type": row.doc_type,
            "doc_id": row.doc_id.to_string(),
            "file_id": row.file_id.to_string(),
            "file_role": row.file_role,
            "file_name": row.file_name,
            "file_size": row.file_size,
            "file_deleted": file_deleted,
            "soft_delete": true,
        })),
        ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        duration_ms: Some(duration_ms),
        request_path: Some(format!("/api/business/attachment/{}", row.id)),
        request_method: Some("DELETE".to_string()),
        request_params: None,
        response_data: Some(serde_json::json!({ "deleted": true, "file_deleted": file_deleted })),
        status_code: Some(200),
        // 追加写表关闭时间戳 Hook，created_at 由写入方显式赋值
        created_at: chrono::Utc::now(),
    }
}

/// 把一条删除留痕写进 `operation_logs`（失败只 `warn`，不影响已成功的删除）。
async fn record_delete_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    row: &model::DocAttachmentRow,
    file_deleted: bool,
    started: std::time::Instant,
) {
    let mut entry = delete_log(
        headers,
        connect_info,
        claims,
        row,
        file_deleted,
        utils::elapsed_ms(started),
    );
    if let Err(error) = entry.insert(db()).await {
        tracing::warn!(
            attachment_id = %row.id,
            operation = OP_DELETE,
            error = %error,
            "单据附件删除留痕写库失败（删除已完成，不因此报错）"
        );
    }
}

/// 下载附件（`GET /business/attachment/download/{id}`）。
///
/// # 为什么参数是绑定行 id 而不是 `file_id`
///
/// 绑定行是「这张单据的这份文件」的唯一凭据；直接暴露 `file_id` 就能拿别的单据的文件
/// （UUID v7 时间有序，枚举成本低）。下载前先查绑定行 → 再交给 storage 出流，
/// 与任务附件「先校验任务可见性、再出流」的顺序完全一致。
///
/// # 权限（ATT-02）
///
/// 两道判据，各管一件事：
///
/// 1. **路由码** `business:attachment:list`（`#[nik::permission]`，中间件判）=
///    「能不能用附件功能」；
/// 2. **行级单据读码**（[`dto::can_view_doc`]）= 「能不能看这张单」。
///
/// 第 2 道是 ATT-02 补的：此前只有第 1 道，于是任何持 `business:attachment:list`
/// 的角色（仓库主管、采购、仓管……）都能下载财务单据的发票影像。
///
/// # 判定顺序与「存在性探测面」
///
/// `400 非法 id` → `404 绑定行不存在` → `403 缺单据读码` → 出流。
///
/// 绑定行的 `doc_type` 是判权限的前提，所以「附件是否存在」必须**先**读出来，
/// 于是两者的先后无法反转为「先判权再审存在」。可接受的理由：
///
/// - 403 的消息由单据**类型**推导（不含文件名 / 大小 / 任何行的事实），
///   只有「该 id 是不是一条附件绑定行」这一位信息会从 403/404 的差异里漏出；
/// - 主键是 UUID v7（时间有序但随机位足够），枚举不可行；
/// - 持该模块读码的人本来就能列出这类单据的附件 —— 对他而言存在性不是秘密。
///
/// 越权分支一律 403 + 点名缺失的权限码（不返回 404 假装不存在：那会让
/// 「为什么我打不开」变成需要翻代码的谜题，见 [`dto::doc_forbidden`]）。
///
/// # 留痕
///
/// 成功出流后写一条 `operation_logs`（`DOC_ATTACHMENT_DOWNLOAD`，
/// 目标 = 附件绑定行，`detail` 含单据类型 / 单据 id / 文件 id / 文件名 / 字节数）。
/// 写库失败只记 `warn`，**不影响下载**（留痕是旁路，不能反压业务）。
///
/// # 异常情况
///
/// - 400：id 不是 UUID / 库里的 `doc_type` 不在白名单（数据漂移要响，不能静默放行）；
/// - 403：缺该单据模块的读码；
/// - 404：附件不存在 / 已软删 / 物理对象丢失；
/// - 409 / 502：存储后端不可用（透传 storage 的口径）。
#[nik::permission(
    path = "/business/attachment/download/{id}",
    method = "GET",
    permission = "business:attachment:list"
)]
#[nik::get("/business/attachment/download/{id}")]
pub async fn download(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> Response {
    let started = std::time::Instant::now();
    match download_inner(&id, &claims, &headers, &connect_info, started).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载的实际流程（错误走 [`ApiError`]，便于 `into_response`）。
///
/// # 参数
///
/// - `raw_id`：路径里的附件绑定行 id；
/// - `claims`：当前用户（判单据读码 + 写留痕）；
/// - `headers` / `connect_info` / `started`：留痕用的请求上下文（IP / UA / 耗时）。
async fn download_inner(
    raw_id: &str,
    claims: &TokenClaims,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    started: std::time::Instant,
) -> std::result::Result<Response, ApiError> {
    let attachment_id = utils::require_uuid(raw_id, "id")?;
    let row = model::find_active(&attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询附件"))?
        .ok_or_else(dto::attachment_not_found)?;

    // ATT-02 · 行级：附件可见性 = 单据可见性。
    // 库里出现未知 doc_type 说明 CHECK 约束被绕过 / 代码与 DDL 漂移：报 400 而不是放行。
    let kind = dto::parse_doc_type(&row.doc_type)?;
    if !dto::can_view_doc(claims, kind) {
        tracing::warn!(
            attachment_id = %attachment_id,
            doc_type = kind.as_str(),
            doc_id = %row.doc_id,
            file_id = %row.file_id,
            user = ?utils::current_user_uuid(claims),
            "越权下载单据附件被拒（缺模块读码）"
        );
        return Err(dto::doc_forbidden(kind));
    }

    let response = download_response(&row.file_id.to_string())
        .await
        .inspect_err(|error| {
            tracing::warn!(
                attachment_id = %attachment_id,
                file_id = %row.file_id,
                error_code = error.code,
                error = %error.msg,
                "单据附件下载失败"
            );
        })?;

    record_download_log(headers, connect_info, claims, &row, kind, started).await;
    Ok(response)
}

/// 下载留痕的操作码（`operation_logs.operation`）。
pub const OP_DOWNLOAD: &str = "DOC_ATTACHMENT_DOWNLOAD";

/// 构造一条下载留痕（**纯逻辑**：不连库、不写库，单测直接断言字段）。
///
/// # 为什么自己写一行而不是等全局中间件
///
/// 全局 `operation_log` 中间件只记 [`crate::shared::constants::AUDITED_METHODS`]
/// （POST / PUT / PATCH / DELETE）：下载是 `GET`，不会留下任何痕迹。而「谁把发票影像
/// 拷走了」恰是最需要留痕的读操作。
///
/// # 参数
///
/// - `headers` / `connect_info`：取 IP / UA / `x-request-id`（与其它日志同源）；
/// - `claims`：操作人；
/// - `row`：被下载的附件绑定行（目标对象）；
/// - `kind`：单据类型（`detail.doc_type` 用它，保证与库里的标签一致）；
/// - `duration_ms`：从进入 handler 到出流的耗时。
///
/// # 可见性
///
/// `pub(crate)`：`tests::download_log_*` 直接断言字段（不连库），
/// `tests::db_download_log_lands_in_operation_logs` 用它跑真实写库。
pub(crate) fn download_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    row: &model::DocAttachmentRow,
    kind: model::DocKind,
    duration_ms: i32,
) -> OperationLogModel {
    OperationLogModel {
        id: uuid::Uuid::now_v7(),
        request_id: utils::request_id(headers),
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: OP_DOWNLOAD.to_string(),
        target_type: Some("DocAttachment".to_string()),
        target_id: Some(row.id.to_string()),
        detail: Some(serde_json::json!({
            "doc_type": kind.as_str(),
            "doc_id": row.doc_id.to_string(),
            "file_id": row.file_id.to_string(),
            "file_role": row.file_role,
            "file_name": row.file_name,
            "file_size": row.file_size,
        })),
        ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        duration_ms: Some(duration_ms),
        request_path: Some(format!("/api/business/attachment/download/{}", row.id)),
        request_method: Some("GET".to_string()),
        // 读操作没有请求体 / 响应体快照（文件字节不进日志）
        request_params: None,
        response_data: None,
        status_code: Some(200),
        // 追加写表关闭时间戳 Hook，created_at 由写入方显式赋值
        created_at: chrono::Utc::now(),
    }
}

/// 把一条下载留痕写进 `operation_logs`（失败只 `warn`，不影响已成功的下载）。
///
/// # 为什么直写而不是投递日志总线
///
/// 总线是异步批量（500 ms 一批），而下载是**低频**操作：直写多花的这一次往返
/// （毫秒级）换的是「响应返回时这一行已经在库里」。总线在未初始化的进程
/// （单测 / 定时任务）里会丢事件，审计行静默消失比慢 1 ms 严重得多。
///
/// 先例：`business/flow/self_approval.rs::record` 同样是直写 `operation_logs`。
async fn record_download_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    row: &model::DocAttachmentRow,
    kind: model::DocKind,
    started: std::time::Instant,
) {
    let mut entry = download_log(
        headers,
        connect_info,
        claims,
        row,
        kind,
        utils::elapsed_ms(started),
    );
    if let Err(error) = entry.insert(db()).await {
        tracing::warn!(
            attachment_id = %row.id,
            operation = OP_DOWNLOAD,
            error = %error,
            "单据附件下载留痕写库失败（下载已完成，不因此报错）"
        );
    }
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
