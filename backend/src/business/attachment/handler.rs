//! 单据附件接口（ARCH-3 §6 R-P1-3 / DC-17）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内写 `/business/attachment/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/attachment/upload` | `business:attachment:upload` + 单据读码 | 上传 + 绑定（multipart：`doc_type` / `doc_id` / `file_role` / `remark` / `file`） |
//! | GET | `/attachment/list` | `business:attachment:list` + 单据读码 | 按 `doc_type + doc_id` 查（`sort=time` 缺省 / `manual`） |
//! | PUT | `/attachment/sort` | `business:attachment:upload` + 单据读码 | 手动排序（ATT-03：提交完整 id 顺序，回读 `manual` 列表） |
//! | DELETE | `/attachment/{id}` | `business:attachment:upload` + 行级 | 软删绑定（上传人本人；删别人的要 `business:attachment:delete` **且**看得见该单据，ATT-04） |
//! | GET | `/attachment/download/{id}` | `business:attachment:list` + 单据读码 | 下载（复用 storage 出流 + 写 `operation_logs`） |
//! | GET | `/attachment/check` | `business:attachment:list` + 单据读码 | **ATT-08**：本单据在本动作下缺什么附件（`can_proceed` / `missing[].hint`） |
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

use crate::business::shared::user_name;
use crate::business::storage;
use crate::business::storage::handler::{
    declared_content_length, download_response, server_size_error, server_upload_limit_bytes, store_server_upload,
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
/// | `file_role` | 否 | `CONTRACT_SCAN` / `SUPPLEMENT` / `ACCEPTANCE` / `INVOICE_SCAN` / `BANK_RECEIPT` / `DEFAULT`（缺省 `DEFAULT`；老值 `SIGN_RECEIPT` / `INVOICE` / `OTHER` 仍接受，后两者归一） |
/// | `remark` | 否 | ≤ 200 字符 |
/// | `file` | 是 | 文件本体（字段名**必须**是 `file`） |
///
/// # 校验顺序（每一步都先于「更贵的那一步」）
///
/// 1. `Content-Length` 与服务端硬顶（**读流之前**，否则大文件会先读进内存再被拒）；
/// 2. 单文件 ≤ **20 MB**（ATT-05，按分片累加，超限立刻掐断 —— 见 [`dto::MAX_FILE_BYTES`]）；
/// 3. 文本字段白名单（`doc_type` / `doc_id` / `file_role` / `remark`）；
/// 4. **单据读码**（403，[`dto::can_view_doc`]，ATT-04）—— 与 `list` / `download` 同一把尺子：
///    看不见这张单，就别往上挂东西（否则会往别人的单据里投毒，并能用 404/400 差异探测
///    单据是否存在）。第 3、4 步合在 [`dto::preflight_upload`] 里，**不查库**；
/// 5. 单据存在性（404 —— 不允许给不存在的单据挂号）：排在 403 **之后**，
///    无读码的人拿任何 id 都只能拿到 403，探测面在此截断；
/// 6. 单张单据附件数上限（50）；
/// 7. 读文件 + 走 storage 策略（大小 / 扩展名 / MIME）。
///
/// # 留痕（AUD-02）
///
/// 绑定成功且**响应发出之前**写一条 `operation_logs`（`DOC_ATTACHMENT_UPLOAD`，
/// 目标 = 附件绑定行，`detail` 带单据类型 / 单据 id / 文件 id / 角色 / 文件名 /
/// 原始文件名 / 字节数 / 秒传标记 / 存储后端），直写库见 [`record_upload_log`]。
/// 写库失败只记 `warn`，**不影响上传**（留痕是旁路，不能反压业务）。
///
/// 被 403 / 400 / 404 拦下的请求不会留这条结构化行（没有绑定行可指），
/// 它们出现在全局中间件的通用流水里。
///
/// # 异常情况
///
/// - 400：字段缺失 / 类型非法 / 备注超长 / 附件数已达上限 / **文件超过 20 MB** /
///   文件名过长（> 255 字符）/ 扩展名或 MIME 不被策略允许；
/// - 403：无 `business:attachment:upload`；或**缺该单据模块的读码**
///   （[`dto::doc_upload_forbidden`]，点名所需权限码）；
/// - 404：单据不存在或已删除（**只有过了读码这一关才可能看到 404**）；
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
    let started = std::time::Instant::now();
    let server_limit = server_upload_limit_bytes();
    if let Some(declared) = declared_content_length(&headers)
        && server_limit > 0
        && declared > server_limit
    {
        return Err(server_size_error(server_limit));
    }

    let (fields, file) = read_multipart(multipart, server_limit).await?;
    // E2E07-FIX-3：**本次上传的文件名快照**。归一（去目录段 / 控制字符 / 超长 400）
    // 之后同时用于 ① storage 的扩展名白名单与对象键、② `biz_doc_attachment.file_name`。
    // 秒传复用同一个 `sys_file` 时，这条快照保证每个附件各显其名
    // （读不到 `sys_file.original_name` 那个「首个上传者的名字」）。
    let submitted_name = dto::normalize_file_name(&file.name)?;
    // ATT-04：字段合法性（400）→ 单据读码（403）。两步都不查库，保证「无权用户拿任何
    // doc_id 都拿到同一份 403」，不给出「这个 id 存不存在」的任何信息。
    let meta = dto::preflight_upload(
        &claims,
        fields.doc_type.as_deref(),
        fields.doc_id.as_deref(),
        fields.file_role.as_deref(),
        fields.remark.as_deref(),
    )
    .inspect_err(|error| {
        if error.status == 403 {
            tracing::warn!(
                doc_type = fields.doc_type.as_deref().unwrap_or_default(),
                doc_id = fields.doc_id.as_deref().unwrap_or_default(),
                user = ?utils::current_user_uuid(&claims),
                "越权上传单据附件被拒（缺模块读码）"
            );
        }
    })?;

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
    // AUD-01：上传人姓名**快照**。写库后永不回改 —— 用户改名 / 销号后，
    // 回看这条附件仍然显示「上传当时是谁」。追溯不到账号时由
    // `user_name::snapshot_name` 给占位串（`(已删除用户)`），不留空、不落 UUID。
    let uploader_name = match operator {
        Some(user_id) => Some(
            user_name::snapshot_name(user_id)
                .await
                .map_err(|error| utils::db_error(error, "查询上传人姓名"))?,
        ),
        None => None,
    };
    // 秒传标记：命中内容 hash 时 storage 会复用已有 `sys_file`（不写对象、不插行）
    let hash = storage::sigv4::hex_sha256(&file.bytes);
    let duplicated = storage::model::find_reusable_file(&hash)
        .await
        .map_err(|error| utils::db_error(error, "查询可复用文件"))?
        .is_some();

    let record = store_server_upload(UPLOAD_CATEGORY, &submitted_name, &file.mime, &file.bytes, operator).await?;

    // 绑定行带上**本次上传的文件名快照**。返回值里的 `file_name` 是**库里那条行**的值：
    // 同一张单据重复上传同一份字节时命中幂等分支、沿用既有行的名字（不回写），
    // 响应必须跟它走 —— 否则「上传返回 file2.pdf、列表显示 file1.pdf」。
    let binding = model::insert_with_uploader(
        meta.doc_type,
        &meta.doc_id,
        &record.id,
        meta.file_role,
        meta.remark.as_deref(),
        operator,
        uploader_name.as_deref(),
        Some(&submitted_name),
    )
    .await
    .map_err(|error| utils::db_error(error, "写入附件绑定"))?;
    let binding_id = binding.id;

    // 显示名口径：**库里那条绑定行的文件名快照**（= 本次上传的名字；幂等命中时是既有行的）。
    // 与列表 / 下载 / 删除留痕读的是同一列（`biz_doc_attachment.file_name`），
    // 三处同源，不会出现「界面看到的」≠「审计记下的」。
    let file_name = binding.file_name.unwrap_or_else(|| submitted_name.clone());

    // AUD-02：直写 `operation_logs`（不走日志总线）。总线是 500 ms 批量，未初始化
    // 的进程会丢事件 —— 145 上实测上传 8 次、`DOC_ATTACHMENT_UPLOAD` 一行没有。
    // 与 [`download_log`] / [`delete_log`] 同一条做法：写库失败只 warn，不反压上传。
    record_upload_log(
        &headers,
        &connect_info,
        &claims,
        &UploadLogFacts {
            binding_id,
            doc_type: meta.doc_type,
            doc_id: meta.doc_id,
            file_id: record.id,
            file_role: meta.file_role,
            file_name: &file_name,
            submitted_name: &submitted_name,
            file_size: record.file_size,
            storage_code: record.storage_code.as_deref(),
            duplicated,
        },
        started,
    )
    .await;

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
        file_name,
        file_size: record.file_size,
        file_role: meta.file_role.as_str().to_string(),
        doc_type: meta.doc_type.as_str().to_string(),
        doc_id: meta.doc_id.to_string(),
        duplicated,
    }))
}

// ==================== 上传留痕（AUD-02） ====================

/// 上传留痕的操作码（`operation_logs.operation`，AUD-02）。
pub const OP_UPLOAD: &str = "DOC_ATTACHMENT_UPLOAD";

/// 一次上传留痕要用到的事实（**纯数据**：不连库、不依赖 storage 类型，单测直接构造）。
///
/// # 为什么需要这个结构
///
/// `upload` 里能拿到的东西来自三处（`dto::UploadMeta` 的解析结果、`sys_file` 记录、
/// multipart 的表单文件名），直接摊成 10 个位置参数既难读又容易错位。收成一个
/// 结构后，[`upload_log`] 的签名只剩「请求上下文 + 事实 + 耗时」三段。
#[derive(Debug, Clone)]
pub(crate) struct UploadLogFacts<'a> {
    /// 附件绑定行 id（`target_id`：审计要能回指到具体这张单据上的这一行）。
    pub binding_id: uuid::Uuid,
    /// 单据类型。
    pub doc_type: model::DocKind,
    /// 单据主键。
    pub doc_id: uuid::Uuid,
    /// `sys_file.id`。
    pub file_id: uuid::Uuid,
    /// 文件角色。
    pub file_role: model::FileRole,
    /// 落库的显示名（**库里那条绑定行的 `file_name` 快照**，与列表 / 下载 / 删除留痕的
    /// `file_name` 同一口径。E2E07-FIX-3 之前读的是 `sys_file.original_name`，
    /// 秒传时那会是**别人**的文件名）。
    pub file_name: &'a str,
    /// 本次请求 multipart 里提交的文件名。秒传复用时它与 `file_name` **相同**
    /// （快照记的就是本次的名字）；只有在「同一张单据重复上传同一份字节、
    /// 沿用既有行名字」时才可能不同 —— 这个差异本身有审计价值。
    pub submitted_name: &'a str,
    /// 字节数（`sys_file.file_size`）。
    pub file_size: i64,
    /// 文件所在后端编码（`NULL` = A1 之前的本地默认目录；秒传时是旧记录的编码）。
    pub storage_code: Option<&'a str>,
    /// 是否命中秒传（复用已有 `sys_file`，本次未写物理对象）。
    pub duplicated: bool,
}

/// 构造一条上传留痕（**纯逻辑**：不连库、不写库，单测直接断言字段）。
///
/// # 为什么上传也要自己写一行（AUD-02 实测发现）
///
/// 改造前上传走 `log::record_audit(...)` → 日志总线（500 ms 批量）。145 用真 token
/// 上传 8 次（4 成功 / 2×403 / 1×400 / 1×404）后查 `operation_logs`：
///
/// ```sql
/// select operation, count(*) from operation_logs
///  where created_at > now() - interval '45 minutes' and operation like 'DOC_ATTACHMENT%'
///  group by 1;
/// -- DOC_ATTACHMENT_DELETE | 3     ← 删除是直写，落了
/// -- （无 DOC_ATTACHMENT_UPLOAD）
/// ```
///
/// 只剩全局中间件那条 `POST /api/business/attachment/upload`（没有单据类型 / 文件名 /
/// 大小）。这与 ATT-03 修删除时的现象**逐字相同**，做法也照抄：直写 `operation_logs`，
/// 写库失败只 `warn`（留痕是旁路，不能反压上传）。
///
/// # `detail` 的键名（ATT-04 清理脚本踩过的坑）
///
/// 旧实现只有 `original_name`，于是按 `detail->>'file_name'` 检索**一条也查不到**。
/// 现在两个键都给：`file_name`（显示名）+ `original_name`（本次提交的文件名）。
/// 两个键都能查到这一行，且语义分明。
///
/// # 与中间件那条 `POST /api/business/attachment/upload` 的关系
///
/// 两条都留：中间件是「HTTP 层面发生了什么」的通用流水，本条是「业务层面谁往哪张
/// 单据上挂了哪个附件」的结构化记录。
pub(crate) fn upload_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    facts: &UploadLogFacts<'_>,
    duration_ms: i32,
) -> OperationLogModel {
    OperationLogModel {
        id: uuid::Uuid::now_v7(),
        request_id: utils::request_id(headers),
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: OP_UPLOAD.to_string(),
        target_type: Some("DocAttachment".to_string()),
        target_id: Some(facts.binding_id.to_string()),
        detail: Some(serde_json::json!({
            "doc_type": facts.doc_type.as_str(),
            "doc_id": facts.doc_id.to_string(),
            "file_id": facts.file_id.to_string(),
            "file_role": facts.file_role.as_str(),
            // 两个键都要：ATT-04 清理脚本按 file_name 检索时踩过「只有 original_name」的坑
            "file_name": facts.file_name,
            "original_name": facts.submitted_name,
            "file_size": facts.file_size,
            "storage_code": facts.storage_code,
            "duplicated": facts.duplicated,
        })),
        ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        duration_ms: Some(duration_ms),
        request_path: Some("/api/business/attachment/upload".to_string()),
        request_method: Some("POST".to_string()),
        // multipart 正文（含文件字节）不进日志；结构化事实都在 detail 里
        request_params: None,
        response_data: Some(serde_json::json!({
            "id": facts.binding_id.to_string(),
            "file_id": facts.file_id.to_string(),
            "file_size": facts.file_size,
            "duplicated": facts.duplicated,
        })),
        status_code: Some(200),
        // 追加写表关闭时间戳 Hook，created_at 由写入方显式赋值
        created_at: chrono::Utc::now(),
    }
}

/// 把一条上传留痕写进 `operation_logs`（失败只 `warn`，不影响已成功的上传）。
///
/// # 为什么直写而不是投递日志总线
///
/// 与 [`record_download_log`] 同一条理由：总线在未初始化的进程里会丢事件
/// （AUD-02 在 145 实测：8 次上传，结构化审计行 0 条），而「谁往哪张单挂了什么附件」
/// 正是审计要回答的问题。多花的这一次往返（毫秒级）换「响应返回时这行已在库里」。
async fn record_upload_log(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    facts: &UploadLogFacts<'_>,
    started: std::time::Instant,
) {
    let mut entry = upload_log(headers, connect_info, claims, facts, utils::elapsed_ms(started));
    if let Err(error) = entry.insert(db()).await {
        tracing::warn!(
            attachment_id = %facts.binding_id,
            operation = OP_UPLOAD,
            error = %error,
            "单据附件上传留痕写库失败（上传已完成，不因此报错）"
        );
    }
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
    // ATT-04：`can_delete` 的「删别人」分支要求看得见这张单；能走到这里说明读码已过，
    // 但判据仍按 `can_view_doc` 计算（同一条函数，权限码若日后细分也不会失配）。
    let can_view = dto::can_view_doc(claims, kind);
    let list: Vec<DocAttachmentItemRes> = rows.iter().map(|row| dto::to_item(row, claims, can_view)).collect();
    // ATT-04 / ATT-11：分组口径随列表一起下发（空分组也要能画出来 → 不能只靠遍历 list）
    let roles = dto::role_catalog(kind, &list);

    Ok(Res::ok(DocAttachmentListRes {
        doc_type: kind.as_str().to_string(),
        doc_id: doc_id.to_string(),
        total: u64::try_from(list.len()).unwrap_or(0),
        list,
        max_count: MAX_ATTACHMENTS_PER_DOC,
        sort: order.as_str().to_string(),
        roles,
    }))
}

/// 查「本单据在本动作下缺什么附件」（`GET /business/attachment/check`，**ATT-08**）。
///
/// # 用途
///
/// 前端在单据详情页加载 / 每次上传或删除附件后调用它，把结果直接映射成三个 UI 事实：
///
/// - `can_proceed = false` → 「提交 / 开票 / 确认」按钮 **disabled**；
/// - `missing[].hint` → 按钮旁**红字**（`缺少发票影像（需 pdf/ofd/xml 格式）`）；
/// - `missing[].message` → 点 disabled 按钮的 **tooltip**（完整人话，与后端 400 同源）。
///
/// 闸门本身仍在各单据的 submit / confirm / activate 分支里
/// （[`super::rule::ensure_attachments`]）—— 本接口是**提前告知**，不是替代：
/// 绕过前端直接 curl，后端照样 400（卡面验收④的双保险）。
///
/// # 与 `list` 完全同一条准入链
///
/// 1. `doc_type` / `doc_id` / `at` 合法性（400）；
/// 2. **单据读码**（403）—— 能看这张单才谈得上它缺什么附件；
/// 3. 单据存在性（404）；
/// 4. 规则表 + 附件事实（`rule::check_attachments`，与闸门同一函数）。
///
/// # 读库失败的口径
///
/// 规则表读不出来 → **500**（`附件必传规则校验失败：…`）。刻意不返回
/// `can_proceed = true`：那等于把「不知道」说成「可以通过」。前端对 500 的处置是
/// **不禁用按钮、也不画红字**（见 `use-attachment-gate.ts`）—— 用户点了之后
/// 仍会被后端 400 拦住，比「因为一次数据库抖动就把所有人锁死」好。
///
/// # 异常情况
///
/// - 400：`doc_type` 不在白名单 / `doc_id` 不是 UUID / `at` 不在
///   `SUBMIT` / `CONFIRM` / `ACTIVATE`；
/// - 403：缺该单据模块的读码（[`dto::doc_forbidden`]）；
/// - 404：单据不存在或已删除；
/// - 500：规则表 / 附件表读不出来，或规则里出现词表外的值。
#[nik::permission(
    path = "/business/attachment/check",
    method = "GET",
    permission = "business:attachment:list"
)]
#[nik::get("/business/attachment/check")]
pub async fn check(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::AttachmentCheckQuery>,
) -> ResResult<dto::AttachmentCheckRes> {
    let kind = dto::parse_doc_type(&query.doc_type)?;
    let doc_id = utils::require_uuid(&query.doc_id, "doc_id")?;
    let at = dto::parse_required_at(query.at.as_deref())?;

    if !dto::can_view_doc(&claims, kind) {
        tracing::warn!(
            doc_type = kind.as_str(),
            %doc_id,
            user = ?utils::current_user_uuid(&claims),
            "越权查询单据附件必传状态被拒（缺模块读码）"
        );
        return Err(dto::doc_forbidden(kind));
    }

    if !model::doc_exists(kind, &doc_id)
        .await
        .map_err(|error| utils::db_error(error, "校验单据存在性"))?
    {
        return Err(dto::doc_not_found(kind));
    }

    let violations = super::rule::check_attachments(kind, doc_id, at)
        .await
        .map_err(super::rule::AttachRuleError::into_api_error)?;
    Ok(Res::ok(dto::to_check_res(kind, doc_id, at, &violations)))
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
        .transaction(|tx| async move { model::set_sort_orders(&tx, tx_kind, &tx_doc_id, &tx_ids).await })
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

    list_of(&claims, ticket.kind, ticket.doc_id, model::ListOrder::Manual).await
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
/// # ATT-04：行级补「单据读码」
///
/// | 分支 | 判据 | 卡面口径 |
/// |------|------|----------|
/// | 上传人删自己的 | 只认上传人本人，**不看单据读码** | 明确可接受（「换岗后仍能删当年挂的附件」，反过来则那条附件只能改库删） |
/// | 持 `:delete` 删别人的 | delete 码 **且** 看得见该单据 | ATT-04 补上（升权处置一张看不见的单没有业务含义，与 ATT-02 同一把尺子） |
///
/// # 异常情况
///
/// - 400：`id` 不是 UUID；库里的 `doc_type` 不在白名单（数据漂移要响，不能静默放行）；
/// - 403：既不是上传人、也没有升权码；持升权码但看不见该单据（[`dto::doc_forbidden`]）；
///   或没有 `business:attachment:upload`；
/// - 404：附件不存在 / 已删除；
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

    // ATT-04 · 行级：库里的 doc_type 决定「看得见这张单吗」。
    // 数据漂移（未知 doc_type）报 400 而不是放行 —— 与 download 一致。
    let kind = dto::parse_doc_type(&row.doc_type)?;
    let can_view = dto::can_view_doc(&claims, kind);
    if !dto::can_delete(&claims, row.uploaded_by, can_view) {
        // 两种拒绝原因分开说：缺单据读码 vs 既非上传人又无升权码
        if claims.has_permission(dto::PERM_DELETE) {
            tracing::warn!(
                attachment_id = %attachment_id,
                doc_type = kind.as_str(),
                doc_id = %row.doc_id,
                user = ?utils::current_user_uuid(&claims),
                "越权删除单据附件被拒（持 delete 码但缺模块读码）"
            );
            return Err(dto::doc_forbidden(kind));
        }
        return Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            format!("只能删除自己上传的附件，或持有 {} 权限", super::dto::PERM_DELETE),
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

    record_delete_log(&headers, &connect_info, &claims, &row, file_deleted, started).await;

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

    let response = download_response(&row.file_id.to_string()).await.inspect_err(|error| {
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
    let mut entry = download_log(headers, connect_info, claims, row, kind, utils::elapsed_ms(started));
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
/// - 400 + `PARAM_INVALID`：**文件本体超过 [`dto::MAX_FILE_BYTES`]**（ATT-05：20 MB）——
///   按分片累加判断，超限后**只排水不入内存**，读完整个请求体再回 400；
/// - 422 + `60003`：body 超过服务端硬顶（框架在中途掐断）。
///
/// # 为什么「超限之后还要把请求体读完」
///
/// 直接 `return Err(...)` 会让 hyper 在客户端还在发 body 时关掉连接：curl 通常还能读到
/// 那一份 400（它边发边收），但「一次性写完再读」的客户端（Python urllib、部分 SDK）
/// 会先撞 `ECONNRESET`、拿不到业务文案 —— 用户看到的是「上传失败」，不是
/// 「文件超过 20MB，请压缩或分卷上传」。
///
/// 所以超限后继续 `chunk()` 把剩余字节**读完并丢弃**（不进 `bytes`、不进内存），
/// 再在外层统一返回 400。读到多少有上限：框架的 `DefaultBodyLimit`
/// （`[upload].max_size_mb`）会在超顶时自己掐断并报 413 → [`multipart_error`]。
async fn read_multipart(
    mut multipart: Multipart,
    server_limit: u64,
) -> std::result::Result<(RawFields, UploadedFile), ApiError> {
    let mut fields = RawFields::default();
    let mut file: Option<UploadedFile> = None;
    // 文件本体已超 20 MB：后续字节全部丢弃，读完再统一报错
    let mut too_large = false;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_error(error.status(), server_limit))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let file_name = field.file_name().unwrap_or("").trim().to_string();
            let mime = field.content_type().unwrap_or("application/octet-stream").to_string();
            // ATT-05：逐片累加 + 超限后只排水（不先把整份 `bytes()` 读进内存）
            let mut bytes: Vec<u8> = Vec::new();
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|error| multipart_error(error.status(), server_limit))?
            {
                if too_large {
                    continue;
                }
                if dto::extend_within_file_limit(&mut bytes, &chunk).is_err() {
                    too_large = true;
                    // 已经读到的半份数据也没有保留价值（要报错），立刻释放
                    bytes = Vec::new();
                }
            }
            if too_large {
                continue;
            }
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
        if too_large {
            // 已经要报超限了：剩余文本字段只用于把 body 读完，不再解析
            continue;
        }
        match name.as_str() {
            "doc_type" => fields.doc_type = Some(value),
            "doc_id" => fields.doc_id = Some(value),
            "file_role" => fields.file_role = Some(value),
            "remark" => fields.remark = Some(value),
            // 未知字段忽略（前端可能带 `category` 一类历史字段）
            _ => {}
        }
    }

    // 排水完毕再报错：客户端能完整拿到 400 + 业务文案（见函数文档）
    if too_large {
        return Err(dto::file_too_large());
    }

    let Some(file) = file else {
        return Err(ApiError::new(400, error_code::PARAM_INVALID, "缺少文件字段 file"));
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
