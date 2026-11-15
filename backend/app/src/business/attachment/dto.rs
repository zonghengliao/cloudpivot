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
//! | `file_role` | 缺省 / 空串 → `DEFAULT`；老值 `INVOICE`/`OTHER` 归一；非白名单值 | 400 + `PARAM_INVALID` |
//! | `remark` | ≤ 200 字符（列 `VARCHAR(255)`，留余量） | 400 + `PARAM_INVALID` |
//! | 单文件大小 | ≤ [`MAX_FILE_BYTES`]（20 MB，ATT-05） | 400 + `PARAM_INVALID`（[`FILE_TOO_LARGE_MSG`]） |
//! | 单据存在性 | 由 handler 查库（`model::doc_exists`） | 404 + `RESOURCE_NOT_FOUND` |
//! | 单据读码（上传 / 重排 / 删他人附件） | [`preflight_upload`] / [`can_delete`]（ATT-04） | 403 + `PERMISSION_DENIED` |
//! | 附件个数 | ≤ [`model::MAX_ATTACHMENTS_PER_DOC`] | 400 + `PARAM_INVALID` |
//! | `sort`（列表） | 缺省 / `time` → 时间倒序；`manual` → 手动序号 | 400 + `PARAM_INVALID`（ATT-03） |
//! | `ids`（重排） | 非空、全部 UUID、无重复 | 400 + `PARAM_INVALID`（ATT-03） |
//! | 删除他人附件 | 上传人本人 ∪ `business:attachment:delete` | 403 + `PERMISSION_DENIED` |

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

use super::model::{DocAttachmentRow, DocKind, FileRole, ListOrder};

/// 上传单据附件的权限码。
pub const PERM_UPLOAD: &str = "business:attachment:upload";
/// 查看 / 下载单据附件的权限码（列表与下载共用）。
pub const PERM_LIST: &str = "business:attachment:list";
/// 删除**他人**附件的行级升权码（上传人永远能删自己的）。
pub const PERM_DELETE: &str = "business:attachment:delete";

/// 备注列的字符上限（列是 `VARCHAR(255)`，留 55 字符余量给后续加工）。
pub const REMARK_MAX_CHARS: usize = 200;

/// 文件名快照列的字符上限（`biz_doc_attachment.file_name VARCHAR(255)`，E2E07-FIX-3）。
pub const FILE_NAME_MAX_CHARS: usize = 255;

/// 归一 multipart 里的 `filename` → **本次上传的文件名快照**（E2E07-FIX-3）。
///
/// # 规则（顺序即实现顺序）
///
/// 1. **只取最后一段路径**：浏览器只会给 basename，但客户端可以伪造
///    `../../etc/passwd` 或 `C:\Users\x\a.pdf`。这个值会进列表、会回填进
///    `Content-Disposition` 之类的地方，留着目录段只会制造歧义；
/// 2. **去掉控制字符**（`\r` `\n` `\t` 之类）：防止有人借文件名做头注入 /
///    在列表里伪造多行；
/// 3. **去首尾空白**；空白结果 → 400（与 `read_multipart` 的「缺少文件名」同码）；
/// 4. **≤ [`FILE_NAME_MAX_CHARS`] 字符**（按 `char` 数，不是字节：中文名一个字
///    占 3 字节但列宽是 255 **字符**）。超长不静默截断 —— 截断会让用户拿到一个
///    「差几个字」的名字去猜，直接 400 让他改名更诚实。
///
/// # 返回值
///
/// 归一后的文件名；调用方拿它**同时**去 `sys_file`（扩展名 → 策略）与
/// `biz_doc_attachment.file_name`（快照），保证「策略校验的名字」与「看到的字」一致。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：空名 / 超长。
pub fn normalize_file_name(raw: &str) -> std::result::Result<String, ApiError> {
    // ① 最后一段路径（`/` 与 `\` 都算分隔符）
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    // ② 去掉控制字符 + ③ 去首尾空白
    let cleaned: String = base.chars().filter(|ch| !ch.is_control()).collect();
    let name = cleaned.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "缺少文件名（multipart 的 filename）",
        ));
    }
    // ④ 长度（按字符数）
    let chars = name.chars().count();
    if chars > FILE_NAME_MAX_CHARS {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("文件名过长（{chars} 字符，最多 {FILE_NAME_MAX_CHARS} 字符），请重命名后再上传"),
        ));
    }
    Ok(name.to_string())
}

/// 单个附件的大小上限：**20 MB**（ATT-05 用户拍板）。
///
/// 这是**业务口径**，比 storage 的 `attachment` 策略（100 MB）更严：单据附件是
/// 合同扫描件 / 发票影像这类「单页或多页文档」，超过 20 MB 基本是没压缩的扫描件，
/// 传上去列表页也打不开、下载也慢。所以在本模块先拦一道，storage 策略继续做
/// 「扩展名 / MIME / 秒传」的第二道闸。
///
/// ⚠ 与前端 [`MAX_FILE_MB`] / `frontend/src/components/attachment-upload-pure.ts`
/// 的 `MAX_ATTACHMENT_FILE_BYTES` 是**同一个数字**，单测
/// `file_size_limit_matches_frontend` 逐字比对（两边漂了直接红）。
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// 大小上限的 MB 数值（拼文案用，避免 `20` 在代码里出现两次）。
pub const MAX_FILE_MB: u64 = MAX_FILE_BYTES / (1024 * 1024);

/// 单文件超限的提示文案（与前端 `FILE_TOO_LARGE_MESSAGE` 逐字一致）。
pub const FILE_TOO_LARGE_MSG: &str = "文件超过 20MB，请压缩或分卷上传";

/// 单文件超限错误：**400 + `PARAM_INVALID`**。
///
/// # 为什么不是 413 / 422
///
/// `413` 是「网关 / 框架把请求体掐断」，`422 + 60003` 是 storage 策略上限；
/// 而 20 MB 是**本模块的业务规则**，属于「入参不合法」，前端要按 400 弹业务文案
/// （卡面验收④：绕过前端直接 curl 25 MB → 后端 400）。
pub fn file_too_large() -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, FILE_TOO_LARGE_MSG)
}

/// 把一块 multipart 分片并进缓冲区，**先判上限再拷贝**。
///
/// # 参数
///
/// - `buffer`：已读到的文件字节（原地追加）；
/// - `chunk`：本次读到的分片。
///
/// # 返回值
///
/// 追加成功 → `Ok(())`。
///
/// # 异常情况
///
/// - 400 + `PARAM_INVALID`（[`file_too_large`]）：加上本片后超过 [`MAX_FILE_BYTES`]。
///   **超限时不写入 `buffer`** —— 调用方立刻返回错误，内存里不会留下半份超限文件。
///
/// # 为什么要流式判
///
/// 先 `field.bytes()` 把整个文件读进内存再判大小，等于让任意大小的文件先把进程内存
/// 打满（25 MB 无所谓，500 MB 就是事故）。按分片累加 + 提前返回，才能做到
/// 「超限的字节一个都不留」。
pub fn extend_within_file_limit(buffer: &mut Vec<u8>, chunk: &[u8]) -> std::result::Result<(), ApiError> {
    let next = buffer.len() as u64 + chunk.len() as u64;
    if next > MAX_FILE_BYTES {
        return Err(file_too_large());
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

/// 列表查询参数（`GET /business/attachment/list?doc_type=&doc_id=&sort=`）。
#[derive(Debug, Clone, Deserialize)]
pub struct DocAttachmentQuery {
    /// 单据类型（`CONTRACT` / `PURCHASE_ORDER` / `STOCK_IN` / `PAYABLE` / `PAYMENT` …）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 排序口径（ATT-03）：缺省 / `time` = 上传时间倒序；`manual` = 手动序号升序。
    #[serde(default)]
    pub sort: Option<String>,
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
    /// 手动排序序号（ATT-03：手动模式下按它升序，前端上移/下移据此与后端对齐）。
    pub sort_order: i32,
    /// 上传人。
    pub uploaded_by: Option<String>,
    /// 上传人姓名。
    pub uploader_name: String,
    /// 上传时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 当前用户能否删除（上传人本人 ∪ `business:attachment:delete`）。
    pub can_delete: bool,
}

/// 一个文件角色分组（ATT-04 面板分栏）。
///
/// # 为什么由后端下发分组清单，而不是前端自己遍历 items 分组
///
/// 卡面验收②要求「**空分组**显示『暂无 XX，点击上传』引导」。只遍历 `list` 的话，
/// 空分组恰好不在数据里 —— 前端就得自己维护一份角色词表，两份词表必然漂移
/// （后端加一个角色 → 前端不认识 → 那个分组永远不出现）。
///
/// 所以后端直接给「该显示的全部角色 + 各自条数」，前端只负责按这个顺序画：
///
/// - 该单据类型的分组清单（[`FileRole::catalog`]，`count = 0` 也要出现）；
/// - 外加数据里真实存在、但不在清单里的角色（老值 `SIGN_RECEIPT`、或接口直传的越界角色）。
///
/// 不变式：`sum(count) == total`（每条附件恰好落进一个分组；未知值进 `DEFAULT`）。
#[derive(Debug, Clone, Serialize)]
pub struct DocAttachmentRoleRes {
    /// 角色字面值（`CONTRACT_SCAN` / `SUPPLEMENT` / …）。
    pub code: String,
    /// 中文分组标题（`合同扫描件` / `补充协议` / …）。
    pub label: String,
    /// 该角色下的附件条数（0 = 空分组，前端显示上传引导）。
    pub count: u64,
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
    /// 本次响应用到的排序口径（`time` / `manual`；ATT-03）。
    ///
    /// 回显而不是让前端记：`PUT /attachment/sort` 固定用 `manual` 回读，
    /// 前端拿同一份结构直接替换列表，不必自己猜「这份是哪个口径的」。
    pub sort: String,
    /// 分组口径（ATT-04 / ATT-11）：该单据类型的分组清单（含空分组）+ 数据里存在的其它角色。
    ///
    /// 前端按这个数组的顺序渲染分组（不再自己维护词表，见 [`DocAttachmentRoleRes`]）。
    pub roles: Vec<DocAttachmentRoleRes>,
}

/// 附件必传校验查询（`GET /business/attachment/check`，**ATT-08**）。
///
/// # 为什么是 GET 而不是 POST
///
/// 它只回答「现在这张单在本动作下能不能过闸」，不写任何东西：详情页每次打开、
/// 每次上传 / 删除附件后都要重算一次，用 GET 才能被浏览器 / 调试工具按缓存语义对待。
#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentCheckQuery {
    /// 单据类型（`CONTRACT` / `INVOICE` / … 共 12 类）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 触发动作：`SUBMIT`（缺省）/ `CONFIRM` / `ACTIVATE`。
    #[serde(default)]
    pub at: Option<String>,
}

/// 一条缺口（对应前端「按钮旁红字」要画的一行）。
///
/// 卡面契约只要求 `role / need / have / min` 四个字段，这里**补齐**了判断所需的
/// 其余事实：`total`（该规则范围内有多少附件）、`actual_exts`（现有格式是什么）、
/// `shortfall`（还差几个）。前端因此不需要自己再算一遍 —— 算一遍就有第二套判据。
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentCheckMissingRes {
    /// 规则主键（运维按它去 `biz_attachment_rule` 改配置）。
    pub rule_id: String,
    /// 规则指定的文件角色（`None` = 任意角色都算）。
    pub role: Option<String>,
    /// 需求的人话名（`remark` → 角色中文名 → 「附件」）。
    pub role_label: String,
    /// 要求的扩展名（`pdf/ofd/xml`）；`None` = 不限格式。
    pub need: Option<String>,
    /// 命中白名单的附件数。
    pub have: i32,
    /// 至少要几个。
    pub min: i32,
    /// 该规则范围内的附件总数（不分格式）。
    pub total: i32,
    /// 还差几个。
    pub shortfall: i32,
    /// 现有附件的扩展名（去重升序；都是白名单外的）。
    pub actual_exts: Vec<String>,
    /// **按钮旁的一行红字**（后端拼，前端原样显示）。
    pub hint: String,
    /// 完整人话（与绕过前端提交时 400 报文逐字一致）。
    pub message: String,
}

/// 附件必传校验结果（`GET /business/attachment/check`）。
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentCheckRes {
    /// 单据类型（回显）。
    pub doc_type: String,
    /// 单据主键（回显）。
    pub doc_id: String,
    /// 触发动作字面值。
    pub at: String,
    /// 触发动作中文（`提交` / `确认` / `生效`）。
    pub at_label: String,
    /// **能不能放行**：`true` = 亮按钮；`false` = 禁用按钮 + 红字。
    pub can_proceed: bool,
    /// 缺口清单（`can_proceed = true` 时为空）。
    pub missing: Vec<AttachmentCheckMissingRes>,
    /// 合并后的红字（多条缺口用「；」连接，前端一行显示不完就折行）。
    pub hint: Option<String>,
    /// 合并后的完整报文（多条用换行，与后端 400 的 `msg` 逐字一致；tooltip 用）。
    pub message: Option<String>,
}

/// 解析触发动作（缺省 / 空串 → `SUBMIT`）。
///
/// # 为什么默认 `SUBMIT`
///
/// 卡面的接口示例就是 `at=SUBMIT`，而且「提交」是绝大多数单据的闸口；
/// 让前端每次都显式传，是更好的实践（前端确实每次都传），缺省值只是为了
/// 调试时 `curl` 少一个参数也能用 —— 它**不会**让错误动作静默通过：
/// 未知值（如 `CONFIRMED`）仍返回 400。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`（消息列出三个可选值）。
pub fn parse_required_at(raw: Option<&str>) -> std::result::Result<super::rule::RequiredAt, ApiError> {
    let text = raw.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("SUBMIT");
    super::rule::RequiredAt::parse(text).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("触发动作 at 非法：{text}（可选 SUBMIT / CONFIRM / ACTIVATE）"),
        )
    })
}

/// 违例 → 一条缺口出参（**纯函数**，不连库）。
pub fn to_check_missing(violation: &super::rule::RuleViolation) -> AttachmentCheckMissingRes {
    AttachmentCheckMissingRes {
        rule_id: violation.rule_id.to_string(),
        role: violation.file_role.clone(),
        role_label: violation.requirement_name(),
        need: match violation.allowed_exts.is_empty() {
            true => None,
            false => Some(violation.allowed_exts.join("/")),
        },
        have: violation.matched,
        min: violation.min_count,
        total: violation.total,
        shortfall: violation.shortfall(),
        actual_exts: violation.actual_exts.clone(),
        hint: violation.hint(),
        message: violation.message(),
    }
}

/// 违例清单 → 校验结果（**纯函数**；handler 只补单据类型 / 主键 / 动作）。
///
/// `can_proceed` 与 `missing` 是同一条判据的两个投影（`can_proceed == missing.is_empty()`），
/// 单测 `can_proceed_iff_no_missing` 钉住这一点：前端如果只读一个字段也不会判错。
pub fn to_check_res(
    kind: DocKind,
    doc_id: Uuid,
    at: super::rule::RequiredAt,
    violations: &[super::rule::RuleViolation],
) -> AttachmentCheckRes {
    let missing: Vec<AttachmentCheckMissingRes> = violations.iter().map(to_check_missing).collect();
    let hint = if missing.is_empty() {
        None
    } else {
        Some(
            missing
                .iter()
                .map(|item| item.hint.clone())
                .collect::<Vec<_>>()
                .join("；"),
        )
    };
    let message = if missing.is_empty() {
        None
    } else {
        Some(
            missing
                .iter()
                .map(|item| item.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    AttachmentCheckRes {
        doc_type: kind.as_str().to_string(),
        doc_id: doc_id.to_string(),
        at: at.as_str().to_string(),
        at_label: at.label().to_string(),
        can_proceed: missing.is_empty(),
        missing,
        hint,
        message,
    }
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
        return Err(ApiError::new(400, error_code::PARAM_INVALID, "缺少单据主键 doc_id"));
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

/// 上传的**前置判定链**（纯逻辑，不连库）：字段合法性（400）→ 单据读码（403）。
///
/// # 为什么要有这一步（ATT-04）
///
/// ATT-02 把 `list` / `download` 收到「附件可见性 = 单据可见性」，但写路径没动：
/// 只持 `business:attachment:upload`、没有该模块读码的人，仍能往**自己看不见的单据**
/// 上盲传文件 —— 那张单据的附件列表里随即出现他的上传物（跨部门投毒面），
/// 而且 `404 单据不存在` / `400 字段非法` 的差异让他能枚举「某个 payable / contract id
/// 是否存在」。上传必须与读同一把尺子：看不见的单，不许挂东西。
///
/// # 顺序（卡面 ATT-04）
///
/// ```text
///   400 字段非法（不含 doc_type 白名单外的值） → 403 缺单据读码 → 〔handler〕404 单据不存在
/// ```
///
/// 存在性判定要查库，留在 handler；本函数只做「不查库就能判的」两步，
/// 从而保证**无权用户无论拿什么 id（存在的、不存在的、随机的）都拿到同一份 403**，
/// 探测面在权限判定处被彻底截断。
///
/// # 异常情况
///
/// - 400 + `PARAM_INVALID`：[`parse_upload_meta`] 的失败；
/// - 403 + `PERMISSION_DENIED`：缺该单据模块的读码（[`doc_upload_forbidden`]）。
///
/// # 可见性
///
/// `pub`：handler 用，`tests::upload_preflight_*` 直接断言顺序（不连库）。
pub fn preflight_upload(
    claims: &TokenClaims,
    doc_type: Option<&str>,
    doc_id: Option<&str>,
    file_role: Option<&str>,
    remark: Option<&str>,
) -> std::result::Result<UploadMeta, ApiError> {
    let meta = parse_upload_meta(doc_type, doc_id, file_role, remark)?;
    if !can_view_doc(claims, meta.doc_type) {
        return Err(doc_upload_forbidden(meta.doc_type));
    }
    Ok(meta)
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
            format!("文件角色 file_role 非法：{}（可选 {allowed}）", raw.unwrap_or_default()),
        )
    })
}

/// 解析列表排序口径（ATT-03）。
///
/// # 参数
///
/// - `raw`：请求里的 `sort`（可缺省）。缺省 / 空串 → [`ListOrder::TimeDesc`]（时间倒序）。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：值不在 `time` / `manual` 白名单内（**不静默回落成默认**：
/// 「参数写错却拿到默认顺序」会让人以为排序生效了，是最难查的一类“假成功”）。
pub fn parse_sort_order(raw: Option<&str>) -> std::result::Result<ListOrder, ApiError> {
    ListOrder::parse(raw).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "排序口径 sort 非法：{}（可选 {} / {}）",
                raw.unwrap_or_default(),
                ListOrder::TimeDesc.as_str(),
                ListOrder::Manual.as_str()
            ),
        )
    })
}

/// 解析 UUID 字段（纯逻辑；错误消息带上原文，便于定位是哪一个 id 写错了）。
///
/// # 参数
///
/// - `raw`：请求里的字符串（去空白后解析）；
/// - `field`：字段名（`doc_id` / `ids[]`），只用于错误消息。
fn parse_uuid_field(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| ApiError::new(400, error_code::PARAM_INVALID, format!("{field} 不是合法 UUID：{raw}")))
}

/// 重排入参（`PUT /business/attachment/sort`）。
#[derive(Debug, Clone, Deserialize)]
pub struct DocAttachmentSortReq {
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 目标顺序（附件绑定行 id 的完整列表，第 1 个 → `sort_order = 0`）。
    pub ids: Vec<String>,
}

/// 重排入参解析结果（已校验：类型 / 单据主键 / 每个附件 id 都是 UUID 且不重复）。
#[derive(Debug, Clone)]
pub struct SortTicket {
    /// 单据类型。
    pub kind: DocKind,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 目标顺序（已按入参顺序）。
    pub ids: Vec<Uuid>,
}

/// 解析重排入参（**纯逻辑**，不连库）。
///
/// # 规则
///
/// | 输入 | 失败 |
/// |------|------|
/// | `doc_type` 不在白名单 | 400（列出可选值） |
/// | `doc_id` 不是 UUID | 400 |
/// | `ids` 为空 | 400（「必须给出完整的附件顺序」） |
/// | 某个 id 不是 UUID | 400（消息带上那个值） |
/// | `ids` 里重复 | 400（消息带上重复值） |
///
/// # 为什么要求「完整列表」而不是增量
///
/// 序号是**相对顺序**：只提交一部分 id 时，剩下的行保留旧序号，会与本轮写入的
/// 0..n-1 撞号，最终顺序取决于 `created_at` 兜底 —— 用户看到的和提交的不是一回事。
/// 完整列表让「提交了什么 = 库里存的什么」，重排接口的语义才可验证。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`（见上表）。
pub fn parse_sort_request(req: &DocAttachmentSortReq) -> std::result::Result<SortTicket, ApiError> {
    let kind = parse_doc_type(&req.doc_type)?;
    let doc_id = parse_uuid_field(&req.doc_id, "doc_id")?;

    if req.ids.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "ids 不能为空：重排必须提交该单据附件的完整顺序",
        ));
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(req.ids.len());
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for raw in &req.ids {
        let id = parse_uuid_field(raw, "ids[]")?;
        if !seen.insert(id) {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("ids 里有重复的附件 id：{raw}"),
            ));
        }
        ids.push(id);
    }

    Ok(SortTicket { kind, doc_id, ids })
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

/// 行级删除判据：上传人本人 ∪ 持 `business:attachment:delete`（ATT-04 补单据读码）。
///
/// # 参数
///
/// - `claims`：当前用户；
/// - `uploaded_by`：附件上传人（历史行可能为 `NULL` —— 无属主行按「只有升权码能删」处理）；
/// - `can_view_doc`：当前用户能否查看**该附件所属单据**（[`can_view_doc`] 的结果，
///   由调用方算好传进来，避免本函数再持有 `DocKind` 概念）。
///
/// # 两个分支的语义（ATT-04 拍板）
///
/// | 分支 | 判据 | 为什么 |
/// |------|------|--------|
/// | 上传人本人 | 只认「我是上传人」，**不看单据读码** | 删除是撤回自己的行为：「传错了要能撤」是上传人自己的事。换岗 / 调部门后仍能删当年挂上去的附件 —— **明确接受**（否则那条附件就再也删不掉，只能找管理员改库） |
/// | 持 `:delete` 的人 | 要 delete 码 **且** 能看见该单据 | 升权删**别人**的附件，是「对这张单据的处置」；看不见这张单还谈得上处置它吗？与 ATT-02「附件可见性 = 单据可见性」同一把尺子 |
///
/// 无属主的历史行（`uploaded_by = NULL`）落进第二行分支：同样要 delete 码 + 看得见。
///
/// # 与前端 `can_delete` 的一致性
///
/// 列表出参的 `can_delete` 直接由本函数（经 [`to_item`]）算出，接口与按钮永远是同一条判据 ——
/// 「按钮亮着点了 403」这种事不会发生。
pub fn can_delete(claims: &TokenClaims, uploaded_by: Option<Uuid>, can_view_doc: bool) -> bool {
    if claims.has_permission(PERM_DELETE) {
        return can_view_doc;
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
    denied(kind, &format!("查看该附件所属的{}", kind.label()))
}

/// 无权向该单据上传附件时的错误（403 + 明确提示，**ATT-04**）。
///
/// # 与 [`doc_forbidden`] 的关系
///
/// 同一条 403 + `30001` 出口，只把动作短语换成「上传」：对上传的人来说，
/// 「无权查看该附件所属的应付单」是句答非所问的话（他本来就没在查看附件）。
/// 权限码清单仍然由 [`DocKind::view_permissions`] 推导，两份消息不会漂移。
///
/// # 不泄露什么（探测面）
///
/// 消息只含**单据类型**与所需权限码，与「这个 doc_id 存不存在」完全无关 ——
/// 这正是 ATT-04 要把 403 放在 404 之前的目的：无权用户拿任何 id 都得到这一句。
pub fn doc_upload_forbidden(kind: DocKind) -> ApiError {
    denied(kind, &format!("向该{}上传附件", kind.label()))
}

/// 403 `PERMISSION_DENIED` 的统一构造：`无权{动作}（需要以下任一权限：a / b）`。
///
/// # 参数
///
/// - `kind`：单据类型（决定单据中文名与所需读码，**不接受调用方自拼权限码**）；
/// - `action`：动作短语（`查看该附件所属的应付单` / `向该应付单上传附件`）。
fn denied(kind: DocKind, action: &str) -> ApiError {
    ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "无权{action}（需要以下任一权限：{}）",
            kind.view_permissions().join(" / ")
        ),
    )
}

/// 行 → 出参。
///
/// # 参数
///
/// - `row`：数据层的附件行；
/// - `claims`：当前用户（算 `can_delete`）；
/// - `can_view_doc`：当前用户能否查看该附件所属单据（ATT-04：由调用方用
///   [`can_view_doc`] 算好传进来 —— 列表接口已经在进门前判过，这里再算一遍不查库，几乎零成本）。
pub fn to_item(row: &DocAttachmentRow, claims: &TokenClaims, can_view_doc: bool) -> DocAttachmentItemRes {
    DocAttachmentItemRes {
        id: row.id.to_string(),
        doc_type: row.doc_type.clone(),
        doc_id: row.doc_id.to_string(),
        file_id: row.file_id.to_string(),
        file_role: row.file_role.clone(),
        file_role_label: FileRole::parse(Some(&row.file_role))
            .unwrap_or(FileRole::Default)
            .label()
            .to_string(),
        remark: row.remark.clone(),
        file_name: row.file_name.clone(),
        file_size: row.file_size,
        mime_type: row.mime_type.clone(),
        sort_order: row.sort_order,
        uploaded_by: row.uploaded_by.map(|id| id.to_string()),
        uploader_name: row.uploader_name.clone(),
        created_at: row.created_at,
        can_delete: can_delete(claims, row.uploaded_by, can_view_doc),
    }
}

/// 按 `file_role` 汇总分组（ATT-04 面板分栏 / ATT-11 按单据给角色，**纯逻辑**，不连库）。
///
/// # 参数
///
/// - `kind`：单据类型（决定画哪些分组，见 [`FileRole::catalog`]）；
/// - `items`：出参列表（`to_item` 的结果，顺序即当前排序口径下的顺序）；
///
/// # 返回值
///
/// 分组清单，顺序固定为：
///
/// 1. [`FileRole::catalog`] 给的清单（**空分组也在**，`count = 0` —— 卡面验收②
///    要靠它显示「暂无 XX，点击上传」）；
/// 2. 数据里真实存在、但不在清单里的角色（老值 `SIGN_RECEIPT`，或接口直传的越界角色），
///    按 [`FileRole::ALL`] 的顺序追加（**没有数据就不出现**）。
///
/// # 为什么要按单据类型过滤（ATT-11）
///
/// 卡面把 14 个角色按业务分给了不同单据（供应商 = 营业执照 / 开户许可 / 资质证书…）。
/// 若每张单据都把 14 个分组画出来，一张合同详情下面会挂 13 个用不上的空分组 ——
/// 「空分组也要显示」这条规则的目的是**提示缺什么**，挂 13 个无关分组就变成噪声。
///
/// # 归一与不变式
///
/// 每条附件的 `file_role` 先过 [`FileRole::parse`]（老值 `INVOICE` / `OTHER` 归一，
/// 未知值兜底 `DEFAULT`），因此：
///
/// ```text
///   sum(roles[].count) == items.len()      // 每条附件恰好落进一个分组
/// ```
///
/// 未知值兜底 `DEFAULT` 而不是丢弃：`count` 与 `total` 对不上时，前端「已上传 N 个」
/// 与分组条数会互相矛盾，用户会以为有附件丢了。
///
/// # 为什么 `label` 从 [`FileRole::label`] 取
///
/// 中文名只有一份（后端枚举）；前端 `FILE_ROLE_OPTIONS` 作为上传下拉的词表由单测比对，
/// 但**分组标题直接用后端下发的 `label`**，不做第二份映射。
pub fn role_catalog(kind: DocKind, items: &[DocAttachmentItemRes]) -> Vec<DocAttachmentRoleRes> {
    use std::collections::HashMap;

    let mut counts: HashMap<FileRole, u64> = HashMap::new();
    for item in items {
        let role = FileRole::parse(Some(&item.file_role)).unwrap_or(FileRole::Default);
        *counts.entry(role).or_insert(0) += 1;
    }

    let planned = FileRole::catalog(kind);
    let mut catalog: Vec<DocAttachmentRoleRes> = planned
        .iter()
        .map(|role| DocAttachmentRoleRes {
            code: role.as_str().to_string(),
            label: role.label().to_string(),
            count: counts.get(role).copied().unwrap_or(0),
        })
        .collect();

    for role in FileRole::ALL {
        if planned.contains(&role) {
            continue;
        }
        let count = counts.get(&role).copied().unwrap_or(0);
        if count > 0 {
            catalog.push(DocAttachmentRoleRes {
                code: role.as_str().to_string(),
                label: role.label().to_string(),
                count,
            });
        }
    }

    catalog
}
