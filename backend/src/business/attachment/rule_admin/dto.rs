//! 附件必传规则管理 · 接口契约与**纯逻辑校验**（ATT-09）。
//!
//! 本文件只有两种东西：**出参 / 入参结构体**与**不碰数据库的校验函数**。
//! handler 负责编排（查库 → 校验 → 写库 → 审计），model 负责 SQL，校验规则集中在这里
//! 是为了让「界面上存不进去的配置」与「库里 CHECK 约束」能被同一批单测钉死
//! （`biz_attachment_rule` 的 5 条 CHECK 见 `V0347`）。
//!
//! # 校验口径与 DDL 的对应关系（改 DDL 要同步改这里，反之亦然）
//!
//! | 入参 | 规则 | DDL / 闸门侧 |
//! |------|------|--------------|
//! | `doc_type` | 必须是 [`DocKind::ALL`] 之一（大小写不敏感） | `ck_biz_attachment_rule_doc_type` |
//! | `required_at` | 必须是 `SUBMIT` / `CONFIRM` / `ACTIVATE` | `ck_biz_attachment_rule_required_at` |
//! | `file_role` | 空 → `NULL`（任意角色都算）；非空必须是 `FileRole` 词表值 | `file_role varchar(40)` |
//! | `min_count` | ≥ 1（缺省 1） | `ck_biz_attachment_rule_min` |
//! | `max_count` | 空 → 不限；非空必须 ≥ `min_count`（且 ≥ 1） | `ck_biz_attachment_rule_max` |
//! | `allow_exts` | 逗号分隔；每个扩展名 `[a-z0-9]{1,10}`；归一后非空且 ≤ 200 字符 | `ck_biz_attachment_rule_exts` + [`super::super::rule::parse_exts`] |
//! | `condition_sql` | 不允许 `;` `--` `/*` `*/` `?`；≤ 300 字符 | `ck_biz_attachment_rule_condition` + [`super::super::rule::condition_sql_is_safe`] |
//! | `remark` | ≤ 200 字符（列宽） | `remark varchar(200)` |
//!
//! 「界面上存不进去的配置」指的不只是错值，还包括**看起来对、实际让规则永不成立**的
//! 组合：`min_count = 0`（空规则，闸门永远通过却让人以为配了）、`max_count < min_count`
//! （恒不成立，单据永远提交不了）、`allow_exts = ','`（解析成「什么都不许」）。
//! 这三类在库里也有 CHECK，但让用户看到 500 而不是「哪一项写错了」是不可接受的。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;
use crate::shared::error_code;

use super::super::model::{DocKind, FileRole};
use super::super::rule::{self as att_rule, AttachmentRule, RequiredAt};

/// 查看规则列表的权限码（页面的 `meta.permission` 也用它）。
pub const PERM_LIST: &str = "support:attachment-rule:list";
/// 新建规则的权限码。
pub const PERM_CREATE: &str = "support:attachment-rule:create";
/// 编辑 / 启停规则的权限码。
pub const PERM_UPDATE: &str = "support:attachment-rule:update";
/// 删除（软删）规则的权限码。
pub const PERM_DELETE: &str = "support:attachment-rule:delete";

/// `remark` 列宽（`varchar(200)`）。
pub const REMARK_MAX_CHARS: usize = 200;
/// `condition_sql` 列宽（`varchar(300)`）。
pub const CONDITION_MAX_CHARS: usize = 300;
/// `allow_exts` 列宽（`varchar(200)`），归一后的字符串长度上限。
pub const ALLOW_EXTS_MAX_CHARS: usize = 200;
/// 单个扩展名的最大长度（`docx` / `jpeg` 这类都够；更长的不是扩展名）。
pub const EXT_TOKEN_MAX_CHARS: usize = 10;
/// 一条规则里最多写多少个扩展名（`allow_exts` 列宽 200 已经够，这里给一个可读的上限）。
pub const EXT_TOKEN_MAX_COUNT: usize = 20;

/// 一个下拉选项（`{ value, label }`）。
///
/// 前端不自己维护第二份词表：`doc_type` 来自 [`DocKind::ALL`]，`required_at` 来自
/// [`RequiredAt::ALL`]，`file_role` 来自 [`FileRole::GROUPS`] —— 三条都只有后端一份。
#[derive(Debug, Clone, Serialize)]
pub struct RuleOptionRes {
    /// 落库字面值（`INVOICE` / `SUBMIT` / `INVOICE_SCAN` …）。
    pub value: String,
    /// 中文标签（`发票` / `提交` / `发票影像` …）。
    pub label: String,
}

/// 下拉数据源（`GET /support/attachment-rule/options`）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleOptionsRes {
    /// 单据类型（12 个，顺序与 `DocKind::ALL` 一致）。
    pub doc_types: Vec<RuleOptionRes>,
    /// 触发动作（3 个：提交 / 确认 / 生效）。
    pub required_ats: Vec<RuleOptionRes>,
    /// 文件角色（6 个固定分组；外加一句「留空 = 任意角色都算」由前端文案说明）。
    pub file_roles: Vec<RuleOptionRes>,
}

/// 下拉数据源（纯函数：三份词表都来自枚举，前端不再抄第二份）。
///
/// 顺序与 `DocKind::ALL` / `RequiredAt::ALL` / `FileRole::ALL` 逐字一致，
/// 单测 `options_come_from_enums` 把「下拉里能选 = 后端认得」钉死。
pub fn options() -> RuleOptionsRes {
    RuleOptionsRes {
        doc_types: DocKind::ALL
            .iter()
            .map(|kind| RuleOptionRes {
                value: kind.as_str().to_string(),
                label: kind.label().to_string(),
            })
            .collect(),
        required_ats: RequiredAt::ALL
            .iter()
            .map(|at| RuleOptionRes {
                value: at.as_str().to_string(),
                label: at.label().to_string(),
            })
            .collect(),
        file_roles: FileRole::ALL
            .iter()
            .map(|role| RuleOptionRes {
                value: role.as_str().to_string(),
                label: role.label().to_string(),
            })
            .collect(),
    }
}

/// 一条规则（`GET /support/attachment-rule/list` 的元素）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleItemRes {
    /// 规则主键。
    pub id: String,
    /// 单据类型字面值。
    pub doc_type: String,
    /// 单据类型中文名。
    pub doc_type_label: String,
    /// 指定文件角色（`None` = 任意角色都算）。
    pub file_role: Option<String>,
    /// 文件角色中文名（`None` = 任意）。
    pub file_role_label: Option<String>,
    /// 触发动作字面值。
    pub required_at: String,
    /// 触发动作中文名。
    pub required_at_label: String,
    /// 至少要几个命中白名单的附件。
    pub min_count: i32,
    /// 最多允许几个（`None` = 不限）。
    pub max_count: Option<i32>,
    /// 扩展名白名单原文（归一后、逗号分隔；`None` = 不限格式）。
    pub allow_exts: Option<String>,
    /// 扩展名白名单拆分后的数组（前端画 tag 用，省一次 split）。
    pub allow_exts_list: Vec<String>,
    /// 附加条件片段（对单据表当前行求值；`None` = 恒适用）。
    pub condition_sql: Option<String>,
    /// 人话名（直接进错误文案）。
    pub remark: Option<String>,
    /// 是否启用（禁用后该规则不参与校验，**即时生效**）。
    pub enabled: bool,
    /// 完整要求文案（与单据提交被拦时 400 报文的第一段**同源**）：
    /// `发票必须上传发票影像（pdf/ofd/xml）`。
    ///
    /// 由 [`AttachmentRule::requirement`] 拼，管理员改 `remark` 就能在页面上立刻看到
    /// 下次提交会提示什么 —— 这是「不写代码、不发版」最直接的反馈。
    pub requirement: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最后更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 规则列表（`GET /support/attachment-rule/list`）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleListRes {
    /// 规则（含已禁用；不含已软删）。
    pub list: Vec<RuleItemRes>,
    /// 条数。
    pub total: u64,
}

/// 新建 / 编辑规则的请求体（`POST /support/attachment-rule`、`PUT /support/attachment-rule/{id}`）。
///
/// # PUT 是**全量替换**
///
/// 每个可空字段传 `null` = 置空（例：`condition_sql: null` = 无条件）。刻意不做
/// 「不传就是保持原值」：那种语义要求前端区分「没填」与「填了空」，而管理页面本来就
/// 会把整行读出来再提交。混着来最容易出现「我明明清空了条件，怎么还生效」。
#[derive(Debug, Clone, Deserialize)]
pub struct RuleSaveReq {
    /// 单据类型（`INVOICE` / `EXPENSE_REPORT` …）。
    pub doc_type: String,
    /// 文件角色（可空 = 任意角色都算）。
    #[serde(default)]
    pub file_role: Option<String>,
    /// 触发动作（`SUBMIT` / `CONFIRM` / `ACTIVATE`）。
    pub required_at: String,
    /// 至少要几个（缺省 1）。
    #[serde(default)]
    pub min_count: Option<i32>,
    /// 最多几个（可空 = 不限）。
    #[serde(default)]
    pub max_count: Option<i32>,
    /// 扩展名白名单（逗号分隔，可空 = 不限格式）。
    #[serde(default)]
    pub allow_exts: Option<String>,
    /// 附加条件片段（可空 = 恒适用）。
    #[serde(default)]
    pub condition_sql: Option<String>,
    /// 人话名（可空 = 回落角色中文名 / 「附件」）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 是否启用（缺省 `true`）。
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 启停请求体（`PUT /support/attachment-rule/{id}/enabled`）。
#[derive(Debug, Clone, Deserialize)]
pub struct RuleToggleReq {
    /// 目标状态：`false` = 该规则不参与校验（立即生效）。
    pub enabled: bool,
}

/// 删除结果（软删）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleDeleteRes {
    /// 规则主键。
    pub id: String,
    /// 是否真的删掉了（`false` = 已经被别人删了，按幂等处理）。
    pub deleted: bool,
}

/// 校验通过的规则（可直接落库）。
#[derive(Debug, Clone)]
pub struct RuleDraft {
    /// 单据类型。
    pub doc_type: DocKind,
    /// 文件角色（已归一为规范字面值；`None` = 任意）。
    pub file_role: Option<String>,
    /// 触发动作。
    pub required_at: RequiredAt,
    /// 至少要几个。
    pub min_count: i32,
    /// 最多几个。
    pub max_count: Option<i32>,
    /// 扩展名白名单（已小写、去重、逗号连接；`None` = 不限）。
    pub allow_exts: Option<String>,
    /// 附加条件片段（已去空白；`None` = 恒适用）。
    pub condition_sql: Option<String>,
    /// 人话名（已去空白；空串 → `None`）。
    pub remark: Option<String>,
    /// 是否启用。
    pub enabled: bool,
}

/// 超管判定（**只有超管能改**，卡面「权限」一节）。
///
/// # 判据
///
/// 角色里含 [`SUPER_ADMIN_ROLE_CODE`] **或** 权限里含通配 `*` / `*:*:*`。
///
/// 为什么不直接用 `claims.is_super_admin()`：框架那个函数只判**通配权限**，不看角色。
/// 而登录时超管的权限列表里确实带 `*:*:*`（`system/auth/model.rs` 的短路），所以正常
/// 情况下两者等价；但测试夹具 / 手工造的 claims 只带角色不带通配时，框架函数会把
/// 真正的超管判成「不是超管」。两个都认才是「超管」的本意，也是
/// `flow::linkage::ensure_config_admin` 的既有口径（同一类内控配置，判据不该分叉）。
///
/// # 参数
///
/// - `claims`：当前用户（来自中间件 `AuthClaims`，**不**取自请求参数）；
/// - `action`：动作名（`新建` / `编辑` / `启用 / 禁用` / `删除`），只用于文案。
///
/// # 异常情况
///
/// 403 + `PERMISSION_DENIED`：不是超管。
pub fn ensure_super_admin(claims: &TokenClaims, action: &str) -> std::result::Result<(), ApiError> {
    let is_super = claims.roles.iter().any(|role| role == SUPER_ADMIN_ROLE_CODE)
        || claims.permissions.iter().any(|code| code == "*" || code == "*:*:*");
    if is_super {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "{action}附件必传规则需要超级管理员（{SUPER_ADMIN_ROLE_CODE}）：\
             规则决定「哪些单据在哪个动作提交不了」，属于内控配置，只能由超管维护"
        ),
    ))
}

/// 解析并校验新建 / 编辑入参（**纯函数**，不连库）。
///
/// # 参数
///
/// - `req`：请求体（见 [`RuleSaveReq`]）。
///
/// # 返回值
///
/// [`RuleDraft`]（所有字段已归一）。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：任何一项不符合 DDl 约束或「看起来对但让规则永不成立」的组合，
/// 消息里点名是哪一项、为什么不行（不返回 500 —— 那是「用户看不出哪里错了」的最差体验）。
pub fn validate(req: &RuleSaveReq) -> std::result::Result<RuleDraft, ApiError> {
    let doc_type = DocKind::parse(&req.doc_type).ok_or_else(|| {
        let allowed = DocKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        param_error(format!("单据类型 doc_type 非法：{}（可选 {allowed}）", req.doc_type))
    })?;

    let required_at = RequiredAt::parse(&req.required_at).ok_or_else(|| {
        let allowed = RequiredAt::ALL
            .iter()
            .map(|at| at.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        param_error(format!(
            "触发动作 required_at 非法：{}（可选 {allowed}）",
            req.required_at
        ))
    })?;

    let file_role = parse_file_role(req.file_role.as_deref())?;
    let min_count = req.min_count.unwrap_or(1);
    if min_count < 1 {
        return Err(param_error(format!(
            "min_count 至少要 1（当前 {min_count}）：0 等于「不校验」，\
             会让管理员以为配了闸而实际全部放行"
        )));
    }
    if let Some(max) = req.max_count
        && max < 1
    {
        return Err(param_error(format!(
            "max_count 要么留空（不限），要么至少 1（当前 {max}）"
        )));
    }
    if let Some(max) = req.max_count
        && max < min_count
    {
        return Err(param_error(format!(
            "max_count（{max}）不能小于 min_count（{min_count}）：\
             这样的规则永远不可能成立，单据会永久提交不了"
        )));
    }

    let allow_exts = parse_allow_exts(req.allow_exts.as_deref())?;
    let condition_sql = parse_condition(req.condition_sql.as_deref())?;
    let remark = normalize_opt(req.remark.as_deref());
    if let Some(text) = remark.as_deref()
        && text.chars().count() > REMARK_MAX_CHARS
    {
        return Err(param_error(format!(
            "remark 最多 {REMARK_MAX_CHARS} 个字符（当前 {} 个）",
            text.chars().count()
        )));
    }

    Ok(RuleDraft {
        doc_type,
        file_role,
        required_at,
        min_count,
        max_count: req.max_count,
        allow_exts,
        condition_sql,
        remark,
        enabled: req.enabled.unwrap_or(true),
    })
}

/// 解析文件角色：空 → `None`（任意角色都算），非空必须是词表值（老值归一）。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：不在 `FileRole::ALL` 内。
pub fn parse_file_role(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    FileRole::parse(Some(text))
        .map(|role| Some(role.as_str().to_string()))
        .ok_or_else(|| {
            let allowed = FileRole::ALL
                .iter()
                .map(|role| role.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            param_error(format!(
                "文件角色 file_role 非法：{text}（可选 {allowed}；留空 = 任意角色都算）"
            ))
        })
}

/// 解析扩展名白名单：逗号分隔 → 小写、去空白、去前导点、去重（保序）→ 逗号连接。
///
/// # 规则
///
/// - 空白 → `Ok(None)`（不限格式）；
/// - 每个 token 必须匹配 `[a-z0-9]{1,10}`（**不接受通配符 `*`**：闸门按字符串逐字比对，
///   `*` 不匹配任何扩展名，写进去就是一条永不满足的规则）；
/// - 最多 [`EXT_TOKEN_MAX_COUNT`] 个，归一后总长 ≤ [`ALLOW_EXTS_MAX_CHARS`]。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：非法 token / 数量或长度超限（消息点名那个 token）。
pub fn parse_allow_exts(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for part in text.split([',', '，']) {
        let token = part.trim().trim_start_matches('.').to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        if token.len() > EXT_TOKEN_MAX_CHARS || !token.chars().all(|c| c.is_ascii_alphanumeric() && c.is_ascii()) {
            return Err(param_error(format!(
                "扩展名「{token}」不合法：只允许字母 / 数字，长度 1–{EXT_TOKEN_MAX_CHARS}\
                 （不要写点号前缀与通配符）"
            )));
        }
        if !out.contains(&token) {
            out.push(token);
        }
    }
    if out.is_empty() {
        return Err(param_error(
            "allow_exts 只写了分隔符（例如 \",\"）：要么留空表示不限格式，要么写出具体扩展名",
        ));
    }
    if out.len() > EXT_TOKEN_MAX_COUNT {
        return Err(param_error(format!(
            "allow_exts 最多 {EXT_TOKEN_MAX_COUNT} 个扩展名（当前 {} 个）",
            out.len()
        )));
    }
    let joined = out.join(",");
    if joined.chars().count() > ALLOW_EXTS_MAX_CHARS {
        return Err(param_error(format!(
            "allow_exts 归一后长度 {}/{ALLOW_EXTS_MAX_CHARS} 字符，太长",
            joined.chars().count()
        )));
    }
    Ok(Some(joined))
}

/// 解析附加条件片段：空白 → `None`；其余必须过 [`att_rule::condition_sql_is_safe`]。
///
/// # 为什么长度也要卡
///
/// 列是 `varchar(300)`，超长会让 PG 报 500（用户看到「服务器错误」，实际是字段太长）。
///
/// # 异常情况
///
/// 400 + `PARAM_INVALID`：含 `;` / `--` / `/*` / `*/` / `?`，或超长。
pub fn parse_condition(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let safe = att_rule::condition_sql_is_safe(raw).map_err(param_error)?;
    if let Some(text) = safe.as_deref()
        && text.chars().count() > CONDITION_MAX_CHARS
    {
        return Err(param_error(format!(
            "condition_sql 最多 {CONDITION_MAX_CHARS} 个字符（当前 {} 个）",
            text.chars().count()
        )));
    }
    Ok(safe)
}

/// 去空白 + 空串归一为 `None`。
fn normalize_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 400 + `PARAM_INVALID` 的统一构造（本文件所有校验失败都走它）。
fn param_error(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 规则不存在（`404`）。
pub fn rule_not_found(id: &Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("附件必传规则 {id} 不存在或已删除"),
    )
}

/// 同一条规则键已存在（`409`）。
///
/// # 为什么算冲突而不是「覆盖」
///
/// 同一 `(doc_type, required_at, file_role, condition_sql)` 若存在两条存活规则，
/// 闸门会把两条**都**判一遍并给出两段可能互相矛盾的文案，而管理员在界面上看不出
/// 哪条生效（`V0347` 用表达式部分唯一索引正是为了杜绝这件事）。
/// 返回 409 + 直接说清撞了哪条规则，比抛 500（唯一索引报错）有用得多。
pub fn rule_key_conflict(draft: &RuleDraft, existing_id: &Uuid) -> ApiError {
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!(
            "已存在同样的规则（单据 {} / 动作 {} / 角色 {} / 条件 {}）：规则 {existing_id}。\
             请直接编辑那条，或改掉角色 / 条件后再保存",
            draft.doc_type.as_str(),
            draft.required_at.as_str(),
            draft.file_role.as_deref().unwrap_or("任意"),
            draft.condition_sql.as_deref().unwrap_or("无条件"),
        ),
    )
}

/// 行 → 出参（**纯函数**：不连库，标签全部来自枚举）。
///
/// `requirement` 借用 [`AttachmentRule::requirement`]，保证页面显示的文案与
/// 「提交被拦时的 400 报文」逐字同源（ATT-08 的红字 / 报文同源口径的延续）。
pub fn to_item(row: &super::model::RuleRow) -> RuleItemRes {
    let doc_type_label = row.doc_type.label().to_string();
    let allow_exts_list = att_rule::parse_exts(row.allow_exts.as_deref()).unwrap_or_default();
    let requirement = AttachmentRule {
        id: row.id,
        doc_type: row.doc_type,
        file_role: row.file_role.clone(),
        required_at: row.required_at,
        min_count: row.min_count,
        max_count: row.max_count,
        allow_exts: att_rule::parse_exts(row.allow_exts.as_deref()),
        condition_sql: row.condition_sql.clone(),
        remark: row.remark.clone(),
    }
    .requirement();
    RuleItemRes {
        id: row.id.to_string(),
        doc_type: row.doc_type.as_str().to_string(),
        doc_type_label,
        file_role: row.file_role.clone(),
        file_role_label: row
            .file_role
            .as_deref()
            .and_then(|role| FileRole::parse(Some(role)))
            .map(|role| role.label().to_string()),
        required_at: row.required_at.as_str().to_string(),
        required_at_label: row.required_at.label().to_string(),
        min_count: row.min_count,
        max_count: row.max_count,
        allow_exts: row.allow_exts.clone(),
        allow_exts_list,
        condition_sql: row.condition_sql.clone(),
        remark: row.remark.clone(),
        enabled: row.enabled,
        requirement,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}
