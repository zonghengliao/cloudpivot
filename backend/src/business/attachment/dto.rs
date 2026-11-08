//! 单据附件接口契约（ARCH-3 DC-17）。
//!
//! 约定与 `erp/*_dto.rs` 一致：主键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 枚举一律大写下划线字符串且与 DDL 的 `CHECK` 逐字一致、时间 `DateTime<Utc>`。
//!
//! # 校验只在这里做（handler 只负责编排）
//!
//! | 输入 | 规则 | 失败 |
//! |------|------|------|
//! | `doc_type` | 必须是 [`DocKind::ALL`] 之一（大小写不敏感） | 400 + `PARAM_INVALID`（消息列出可选值） |
//! | `doc_id` | 必须是 UUID | 400 + `PARAM_INVALID` |
//! | `file_role` | 缺省 / 空串 → `OTHER`；非白名单值 | 400 + `PARAM_INVALID` |
//! | `remark` | ≤ 200 字符（列 `VARCHAR(255)`，留余量） | 400 + `PARAM_INVALID` |
//! | 单据存在性 | 由 handler 查库（`model::doc_exists`） | 404 + `RESOURCE_NOT_FOUND` |
//! | 附件个数 | ≤ [`model::MAX_ATTACHMENTS_PER_DOC`] | 400 + `PARAM_INVALID` |
//! | 删除他人附件 | 上传人本人 ∪ `business:attachment:delete` | 403 + `PERMISSION_DENIED` |

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

use super::model::{DocAttachmentRow, DocKind, FileRole};

/// 上传单据附件的权限码。
pub const PERM_UPLOAD: &str = "business:attachment:upload";
/// 查看 / 下载单据附件的权限码（列表与下载共用）。
pub const PERM_LIST: &str = "business:attachment:list";
/// 删除**他人**附件的行级升权码（上传人永远能删自己的）。
pub const PERM_DELETE: &str = "business:attachment:delete";

/// 备注列的字符上限（列是 `VARCHAR(255)`，留 55 字符余量给后续加工）。
pub const REMARK_MAX_CHARS: usize = 200;

/// 列表查询参数（`GET /business/attachment/list?doc_type=&doc_id=`）。
#[derive(Debug, Clone, Deserialize)]
pub struct DocAttachmentQuery {
    /// 单据类型（`CONTRACT` / `PURCHASE_ORDER` / `STOCK_IN` / `PAYABLE` / `PAYMENT`）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
}

/// 上传时从 multipart 文本字段解析出的元信息。
#[derive(Debug, Clone)]
pub struct UploadMeta {
    /// 单据类型。
    pub doc_type: DocKind,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 文件角色。
    pub file_role: FileRole,
    /// 备注（已去空白，空串归一为 `None`）。
    pub remark: Option<String>,
}

/// 一条附件（对应前端 `DocAttachmentItem`）。
#[derive(Debug, Clone, Serialize)]
pub struct DocAttachmentItemRes {
    /// 绑定行主键（删除 / 下载用）。
    pub id: String,
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// `sys_file.id`。
    pub file_id: String,
    /// 文件角色。
    pub file_role: String,
    /// 文件角色中文名（前端 tag 直接用，避免前端再维护一份词表）。
    pub file_role_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 显示用文件名。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME。
    pub mime_type: Option<String>,
    /// 上传人。
    pub uploaded_by: Option<String>,
    /// 上传人姓名。
    pub uploader_name: String,
    /// 上传时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 当前用户能否删除（上传人本人 ∪ `business:attachment:delete`）。
    pub can_delete: bool,
}

/// 附件列表（`GET /business/attachment/list`）。
#[derive(Debug, Clone, Serialize)]
pub struct DocAttachmentListRes {
    /// 单据类型（回显，便于前端确认请求被正确解析）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 附件列表。
    pub list: Vec<DocAttachmentItemRes>,
    /// 条数。
    pub total: u64,
    /// 单张单据的附件上限（前端据此禁用上传按钮）。
    pub max_count: i64,
}

/// 上传结果。
#[derive(Debug, Clone, Serialize)]
pub struct DocAttachmentUploadRes {
    /// 绑定行主键。
    pub id: String,
    /// `sys_file.id`。
    pub file_id: String,
    /// 显示用文件名。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// 文件角色。
    pub file_role: String,
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 是否命中内容秒传（同一份字节此前已上传过 → 复用 `sys_file` 记录）。
    pub duplicated: bool,
}

/// 删除结果。
#[derive(Debug, Clone, Serialize)]
pub struct DocAttachmentDeleteRes {
    /// 绑定行主键。
    pub id: String,
    /// `sys_file.id`。
    pub file_id: String,
    /// 绑定行是否被软删。
    pub deleted: bool,
    /// `sys_file` 是否同时被软删（还有别的单据在用这份文件时为 `false`）。
    pub file_deleted: bool,
}

/// 解析上传元信息（**纯逻辑**，不连库）。
///
/// # 参数
///
/// - `doc_type`：multipart 文本字段；
/// - `doc_id`：multipart 文本字段；
/// - `file_role`：multipart 文本字段（可缺省）；
/// - `remark`：multipart 文本字段（可缺省）。
///
/// # 返回值
///
/// [`UploadMeta`]。
///
/// # 异常情况
///
/// - 400 + `PARAM_INVALID`：类型不是白名单 / 主键不是 UUID / 角色非法 / 备注超长。
pub fn parse_upload_meta(
    doc_type: Option<&str>,
    doc_id: Option<&str>,
    file_role: Option<&str>,
    remark: Option<&str>,
) -> std::result::Result<UploadMeta, ApiError> {
    let kind = parse_doc_type(doc_type.unwrap_or_default())?;
    let raw_id = doc_id.map(str::trim).unwrap_or_default();
    if raw_id.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "缺少单据主键 doc_id",
        ));
    }
    let doc_id = Uuid::parse_str(raw_id).map_err(|_| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("单据主键 doc_id 不是合法 UUID：{raw_id}"),
        )
    })?;
    let role = parse_file_role(file_role)?;
    let remark = remark
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    if let Some(text) = remark.as_deref()
        && text.chars().count() > REMARK_MAX_CHARS
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("备注最多 {REMARK_MAX_CHARS} 个字符"),
        ));
    }
    Ok(UploadMeta {
        doc_type: kind,
        doc_id,
        file_role: role,
        remark,
    })
}

/// 解析单据类型。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`，消息里列出五个可选值（前端下拉写错时能直接看到正确值）。
pub fn parse_doc_type(raw: &str) -> std::result::Result<DocKind, ApiError> {
    DocKind::parse(raw).ok_or_else(|| {
        let allowed = DocKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("单据类型 doc_type 非法：{raw}（可选 {allowed}）"),
        )
    })
}

/// 解析文件角色（缺省 / 空串 → `OTHER`）。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`，消息里列出四个可选值。
pub fn parse_file_role(raw: Option<&str>) -> std::result::Result<FileRole, ApiError> {
    FileRole::parse(raw).ok_or_else(|| {
        let allowed = FileRole::ALL
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "文件角色 file_role 非法：{}（可选 {allowed}）",
                raw.unwrap_or_default()
            ),
        )
    })
}

/// 单据不存在时的错误（**统一口径**：404 + `RESOURCE_NOT_FOUND`）。
///
/// # 参数
///
/// - `kind`：单据类型（消息里说清是哪一类单据）。
pub fn doc_not_found(kind: DocKind) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{}不存在或已删除，无法挂附件", kind.label()),
    )
}

/// 附件不存在 / 已删除时的错误。
pub fn attachment_not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "附件不存在或已删除")
}

/// 行级删除判据：上传人本人 ∪ 持 `business:attachment:delete`。
///
/// # 参数
///
/// - `claims`：当前用户；
/// - `uploaded_by`：附件上传人（历史行可能为 `NULL` —— 无属主行按「只有升权码能删」处理）。
pub fn can_delete(claims: &TokenClaims, uploaded_by: Option<Uuid>) -> bool {
    if claims.has_permission(PERM_DELETE) {
        return true;
    }
    match (uploaded_by, crate::utils::current_user_uuid(claims)) {
        (Some(owner), Some(me)) => owner == me,
        _ => false,
    }
}

/// 行级读判据（**ATT-02**）：当前用户能否查看「该附件所属的单据」。
///
/// # 为什么这条判据必须存在
///
/// 路由码 `business:attachment:list` 回答的是「能不能用附件功能」，不是
/// 「能不能看这一张单的附件」。只靠它，仓库主管（`WAREHOUSE_MANAGER` 持
/// `business:attachment:list`，但没有 `business:erp:payable:*`）可以拿着一条
/// 应付单的附件 id 把发票影像下载走 —— 越权读财务凭证。
///
/// # 判据
///
/// [`DocKind::view_permissions`] 里任一码命中即可（列表码 ∪ 详情码）。
/// `SUPER_ADMIN` 由框架的 `has_any_permission` 短路（`*` / `*:*:*`），不必另写分支。
///
/// # 参数
///
/// - `claims`：当前用户（来自中间件的 `AuthClaims`，**不**取自请求参数）；
/// - `kind`：附件所属单据的类型。
pub fn can_view_doc(claims: &TokenClaims, kind: DocKind) -> bool {
    claims.has_any_permission(kind.view_permissions())
}

/// 无权查看附件所属单据时的错误（403 + 明确提示）。
///
/// # 提示里为什么点名权限码
///
/// 与本模块其它 4xx 同口径（`doc_not_found` 点名单据类型、`parse_doc_type` 列出可选值）：
/// 运维 / 使用者能直接看出「缺哪一个授权」，不必翻代码。
///
/// # 不泄露什么
///
/// 只说「无权查看该附件所属的应付单」，**不**说文件名 / 大小 / 是否存在 ——
/// 消息由单据**类型**推导，不含任何一条附件行的事实。
pub fn doc_forbidden(kind: DocKind) -> ApiError {
    ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "无权查看该附件所属的{}（需要以下任一权限：{}）",
            kind.label(),
            kind.view_permissions().join(" / ")
        ),
    )
}

/// 行 → 出参。
///
/// # 参数
///
/// - `row`：数据层的附件行；
/// - `claims`：当前用户（算 `can_delete`）。
pub fn to_item(row: &DocAttachmentRow, claims: &TokenClaims) -> DocAttachmentItemRes {
    DocAttachmentItemRes {
        id: row.id.to_string(),
        doc_type: row.doc_type.clone(),
        doc_id: row.doc_id.to_string(),
        file_id: row.file_id.to_string(),
        file_role: row.file_role.clone(),
        file_role_label: FileRole::parse(Some(&row.file_role))
            .unwrap_or(FileRole::Other)
            .label()
            .to_string(),
        remark: row.remark.clone(),
        file_name: row.file_name.clone(),
        file_size: row.file_size,
        mime_type: row.mime_type.clone(),
        uploaded_by: row.uploaded_by.map(|id| id.to_string()),
        uploader_name: row.uploader_name.clone(),
        created_at: row.created_at,
        can_delete: can_delete(claims, row.uploaded_by),
    }
}
