//! 附件必传规则管理 · 数据访问（**ATT-09**）。
//!
//! 本文件只做一件事：`biz_attachment_rule` 的读写 SQL。校验在 [`super::dto`]，
//! 编排 / 权限 / 审计在 [`super::handler`]。
//!
//! # 为什么读侧不复用 `rule::load_rules`
//!
//! [`super::super::rule::load_rules`] 是**闸门视角**：只读启用中的规则、只读闸门用得到的
//! 列，未知 `doc_type` 直接报错（闸门读到脏行必须响）。管理页面要的正好相反 ——
//! **禁用行也要列出来**（否则「我配过什么、什么时候关的」不可追），还要 `id` /
//! `enabled` / `created_at` / `updated_at`。两边字段集与语义都不同，硬合并会让
//! 闸门被迫读管理页需要的列（或反之），得不偿失。
//!
//! # 唯一键与 409
//!
//! `V0347` 建了表达式部分唯一索引 `uk_biz_attachment_rule_key`
//! （`doc_type + required_at + COALESCE(file_role,'') + COALESCE(condition_sql,'')`
//! 且 `deleted_at IS NULL`）。本文件的 [`find_conflict`] 用**逐字相同的 COALESCE 口径**
//! 先查一次，让用户拿到 409 + 「撞了哪条规则」；并发下仍可能撞索引，[`unique_conflict`]
//! 把 `DbError::UniqueViolation` 也收成同一条 409（判据用变体，不匹配文案）。

use chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;
use crate::utils;

use super::super::model::DocKind;
use super::super::rule::RequiredAt;
use super::dto::RuleDraft;

/// 规则表名（与 `V0347` 的 `CREATE TABLE` 逐字一致；本文件只碰这一张表）。
pub const TABLE: &str = "biz_attachment_rule";

/// 列表 / 详情要读的列（唯一来源，改列名只改这里）。
const COLUMNS: &str = "id, doc_type, file_role, required_at, min_count, max_count, \
                       allow_exts, condition_sql, remark, enabled, created_at, updated_at";

/// 一条规则的行投影（管理面视角：**含禁用行**、带审计时间）。
#[derive(Debug, Clone)]
pub struct RuleRow {
    /// 规则主键。
    pub id: Uuid,
    /// 单据类型。
    pub doc_type: DocKind,
    /// 指定文件角色（原文大写；`None` = 任意角色都算）。
    pub file_role: Option<String>,
    /// 触发动作。
    pub required_at: RequiredAt,
    /// 至少要几个命中白名单的附件。
    pub min_count: i32,
    /// 最多几个（`None` = 不限）。
    pub max_count: Option<i32>,
    /// 扩展名白名单原文（小写逗号分隔；`None` = 不限格式）。
    pub allow_exts: Option<String>,
    /// 附加条件片段（对单据表当前行求值；`None` = 恒适用）。
    pub condition_sql: Option<String>,
    /// 人话名（直接进错误文案）。
    pub remark: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后更新时间。
    pub updated_at: DateTime<Utc>,
}

impl RuleRow {
    /// 行 → 结构体。
    ///
    /// # 异常情况
    ///
    /// `doc_type` / `required_at` 不在词表内 → [`DbError::InvalidValue`]：
    /// 库里有 CHECK 兜着，真出现说明有人绕过 CHECK 改了库（或用老版本写入），
    /// 管理页面必须显式报错而不是把该行静默藏掉（藏掉 = 界面上「少了条规则」查不出原因）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let doc_type_raw: String = row.get_typed("doc_type")?;
        let doc_type = DocKind::parse(&doc_type_raw).ok_or_else(|| {
            DbError::InvalidValue(format!(
                "规则 doc_type「{doc_type_raw}」不在 DocKind 词表内"
            ))
        })?;
        let required_at_raw: String = row.get_typed("required_at")?;
        let required_at = RequiredAt::parse(&required_at_raw).ok_or_else(|| {
            DbError::InvalidValue(format!(
                "规则 required_at「{required_at_raw}」不在 SUBMIT / CONFIRM / ACTIVATE 内"
            ))
        })?;
        Ok(Self {
            id: row.get_typed("id")?,
            doc_type,
            file_role: row.get_typed("file_role")?,
            required_at,
            min_count: row.get_typed("min_count")?,
            max_count: row.get_typed("max_count")?,
            allow_exts: row.get_typed("allow_exts")?,
            condition_sql: row.get_typed("condition_sql")?,
            remark: row.get_typed("remark")?,
            enabled: row.get_typed("enabled")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 当前用户 UUID → 可写进 `uuid` 列的 [`Value`]（未登录 / 系统调用写 `NULL`）。
///
/// 与 `erp::handler::operator_of` 同一口径：审计列只收真 UUID，绝不塞空串
/// （`NULLIF(?::text,'')::uuid` 与它配套使用）。
fn operator_of(claims: &TokenClaims) -> Value {
    utils::current_user_uuid(claims).map_or(Value::Null, |id| Value::from(&id))
}

/// 全部规则（**含已禁用**，不含已软删），按「单据 → 动作 → 角色」稳定排序。
///
/// # 排序口径
///
/// `doc_type, required_at, file_role NULLS FIRST, id`：同单据同动作下，通用规则
/// （`file_role IS NULL` = 任意角色）排在具体角色规则前面 —— 与闸门逐条判的顺序一致，
/// 运维在页面上看到的就是校验发生的顺序。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（管理页读失败必须如实报错，不能显示「0 条规则」）。
pub async fn list_all() -> DbResult<Vec<RuleRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE deleted_at IS NULL \
                  ORDER BY doc_type, required_at, file_role NULLS FIRST, id"
            ),
            vec![],
        )
        .await?;
    rows.iter().map(RuleRow::from_row).collect()
}

/// 按主键读一条（不含已软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find(id: &Uuid) -> DbResult<Option<RuleRow>> {
    let rows = db()
        .find_with_params(
            &format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(id)],
        )
        .await?;
    rows.first().map(RuleRow::from_row).transpose()
}

/// 查同键的存活规则（唯一索引口径，`exclude` 用于编辑时排除自己）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_conflict(draft: &RuleDraft, exclude: Option<&Uuid>) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_attachment_rule \
              WHERE deleted_at IS NULL \
                AND doc_type = ? \
                AND required_at = ? \
                AND COALESCE(file_role, '') = COALESCE(?::text, '') \
                AND COALESCE(condition_sql, '') = COALESCE(?::text, '') \
                AND (?::uuid IS NULL OR id <> ?::uuid)",
            vec![
                Value::Text(draft.doc_type.as_str().to_string()),
                Value::Text(draft.required_at.as_str().to_string()),
                draft
                    .file_role
                    .as_deref()
                    .map_or(Value::Null, |role| Value::Text(role.to_string())),
                draft
                    .condition_sql
                    .as_deref()
                    .map_or(Value::Null, |text| Value::Text(text.to_string())),
                exclude.map_or(Value::Null, Value::from),
                exclude.map_or(Value::Null, Value::from),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed::<Uuid>("id"))
        .transpose()
}

/// 新建一条规则，返回主键。
///
/// # 异常情况
///
/// - 撞 `uk_biz_attachment_rule_key` → [`DbError::UniqueViolation`]（handler 转 409）；
/// - 数据库异常 → [`DbError`]。
pub async fn insert(claims: &TokenClaims, draft: &RuleDraft) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let rows = db()
        .find_with_params(
            "INSERT INTO biz_attachment_rule \
                 (id, doc_type, file_role, required_at, min_count, max_count, \
                  allow_exts, condition_sql, remark, enabled, \
                  created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?::text, ?, ?, ?, ?::text, ?::text, ?::text, ?, ?, ?, \
                     NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) \
             RETURNING id",
            vec![
                Value::from(&id),
                Value::Text(draft.doc_type.as_str().to_string()),
                draft
                    .file_role
                    .as_deref()
                    .map_or(Value::Null, |role| Value::Text(role.to_string())),
                Value::Text(draft.required_at.as_str().to_string()),
                Value::I32(draft.min_count),
                draft.max_count.map_or(Value::Null, Value::I32),
                draft
                    .allow_exts
                    .as_deref()
                    .map_or(Value::Null, |text| Value::Text(text.to_string())),
                draft
                    .condition_sql
                    .as_deref()
                    .map_or(Value::Null, |text| Value::Text(text.to_string())),
                draft
                    .remark
                    .as_deref()
                    .map_or(Value::Null, |text| Value::Text(text.to_string())),
                Value::from(draft.enabled),
                Value::from(now),
                Value::from(now),
                Value::Text(
                    utils::current_user_uuid(claims)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(
                    utils::current_user_uuid(claims)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed::<Uuid>("id"))
        .transpose()?
        .ok_or_else(|| DbError::InvalidValue("插入附件规则没有返回主键".to_string()))
}

/// 全量编辑一条规则（`PUT`：可空字段传 `None` 即置空），返回受影响行数。
///
/// # 异常情况
///
/// - 撞 `uk_biz_attachment_rule_key` → [`DbError::UniqueViolation`]（handler 转 409）；
/// - 数据库异常 → [`DbError`]。
pub async fn update(claims: &TokenClaims, id: &Uuid, draft: &RuleDraft) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_attachment_rule \
                SET doc_type = ?, \
                    file_role = ?::text, \
                    required_at = ?, \
                    min_count = ?, \
                    max_count = ?, \
                    allow_exts = ?::text, \
                    condition_sql = ?::text, \
                    remark = ?::text, \
                    enabled = ?, \
                    updated_at = ?, \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Text(draft.doc_type.as_str().to_string()),
            draft
                .file_role
                .as_deref()
                .map_or(Value::Null, |role| Value::Text(role.to_string())),
            Value::Text(draft.required_at.as_str().to_string()),
            Value::I32(draft.min_count),
            draft.max_count.map_or(Value::Null, Value::I32),
            draft
                .allow_exts
                .as_deref()
                .map_or(Value::Null, |text| Value::Text(text.to_string())),
            draft
                .condition_sql
                .as_deref()
                .map_or(Value::Null, |text| Value::Text(text.to_string())),
            draft
                .remark
                .as_deref()
                .map_or(Value::Null, |text| Value::Text(text.to_string())),
            Value::from(draft.enabled),
            Value::from(Utc::now()),
            Value::Text(
                utils::current_user_uuid(claims)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(id),
        ],
    )
    .await
}

/// 启用 / 禁用一条规则，返回受影响行数。
///
/// 只改 `enabled`：**不碰** 其它字段（启停是高频操作，整行回传会让并发编辑互相覆盖）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn set_enabled(claims: &TokenClaims, id: &Uuid, enabled: bool) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_attachment_rule \
                SET enabled = ?, updated_at = ?, update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(enabled),
            Value::from(Utc::now()),
            Value::Text(
                utils::current_user_uuid(claims)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(id),
        ],
    )
    .await
}

/// 软删一条规则，返回受影响行数（`0` = 已被别人删了 / 不存在）。
///
/// 软删而不是物理删：`sys_menu` / 审计 / 排障都要能回答「这条规则什么时候没的」。
/// 软删后同一键可以重新建（唯一索引带 `WHERE deleted_at IS NULL`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete(claims: &TokenClaims, id: &Uuid) -> DbResult<u64> {
    let now = Utc::now();
    db().execute_with_params(
        "UPDATE biz_attachment_rule \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(
                utils::current_user_uuid(claims)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(id),
        ],
    )
    .await
}

/// 写路径的数据库错误收口：唯一冲突 → `409`，CHECK 违反 → `400`，其余 → `500`。
///
/// # 为什么判据用变体不用文案
///
/// `DbError` 的 `Display` 是 `unique constraint violated constraint=uk_biz_xxx`，
/// 不含 `duplicate key`（`erp::handler::write_error` 的注释里记着同一条教训）。
///
/// # 参数
///
/// - `error`：数据库错误；
/// - `ctx`：动作名（`新建` / `编辑` / `启用` …），只用于 500 的文案；
/// - `conflict`：唯一冲突时要回给用户的那条 409（闭包生成，避免无冲突时白算）。
pub fn write_error(error: DbError, ctx: &str, conflict: impl FnOnce() -> ApiError) -> ApiError {
    match &error {
        DbError::UniqueViolation { .. } => conflict(),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "{ctx}附件规则不满足数据库约束{}：请检查数量区间 / 扩展名白名单 / 条件片段",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        _ => ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("{ctx}附件规则失败：{error}"),
        ),
    }
}

/// 审计用的 `operator`（未登录写 `NULL`，不写空串）。
pub fn operator_column(claims: &TokenClaims) -> Value {
    operator_of(claims)
}
