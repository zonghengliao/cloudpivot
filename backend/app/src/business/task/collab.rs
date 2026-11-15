//! 工作任务 · 协作三件套：评论 / 附件 / 工时（批次 C+ · **T6b**）。
//!
//! 依据：卡面 t_3fe560e1 三段规则 + `docs/批次C+-工作任务管理-核心设计.md`
//!       §八（通知分级）、§九（附件走 storage）、§十三（工时汇总）
//!
//! # 本模块存在的理由
//!
//! T1 建了 7 张表，但只交付了 `biz_task` / `biz_task_member` / `biz_task_dependency`
//! 的接口。`biz_task_comment` / `biz_task_attachment` / `biz_task_worklog` 三张表
//! **建了却没有读写入口**（「有表没功能」）。T5b 只补了「写」的最低限度
//! （`POST /comment`、`POST /attachment`）—— 列表、删除、下载、工时全缺，
//! 详情页因此只能看到一个死附件清单。本模块把三张表补齐成可用的协作面。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/task/{id}/comments` | `business:task:read` | 评论列表（时间正序，分页） |
//! | DELETE | `/business/task/{id}/comments/{comment_id}` | `business:task:read` | 软删（作者 / `business:task:comment:delete`） |
//! | GET | `/business/task/{id}/attachments` | `business:task:read` | 附件列表（含来源 HUMAN/AI） |
//! | DELETE | `/business/task/{id}/attachments/{att_id}` | `business:task:read` | 软删（上传者 / `business:task:attachment:delete`） |
//! | GET | `/business/task/{id}/attachments/{att_id}/download` | `business:task:read` | 下载（**先校验任务可见性**） |
//! | GET | `/business/task/{id}/worklogs` | `business:task:read` | 工时列表 + 合计 |
//! | POST | `/business/task/{id}/worklogs` | `business:task:read` | 登记工时（服务端汇总 `actual_hours`） |
//! | DELETE | `/business/task/{id}/worklogs/{wl_id}` | `business:task:read` | 删除（本人 / `business:task:worklog`） |
//!
//! 「写」的两个入口（`POST /comment`、`POST /attachment`）留在 [`super::handler`]：
//! 它们要同时接受 **AI 任务令牌**（走 `#[nik::no_auth]` 白名单），与这里的
//! 「人用 JWT」是两套身份解析；搬过来只会让 `no_auth` 白名单与令牌守卫的边界变模糊。
//!
//! # 四条硬约束（本卡红线，逐条对应卡面）
//!
//! 1. **附件下载必须先判任务可见性**：只凭 `att_id` 就能下载 = 任何登录用户
//!    遍历 UUID 就能拖走全公司的任务附件。所以 [`download_attachment`] 的顺序是
//!    「可见任务 → 附件属于该任务 → 文件未软删 → 交给 storage 出流」，
//!    任何一步不成立都是 **404**（不是 403：403 会告诉调用者「这个附件存在」）。
//! 2. **@ 提及是强制通知**（修正 8）：`force = true`，不尊重收件人的偏好开关。
//!    实现在 [`notify_mentions`]，被 [`super::handler::add_comment`] 调用。
//! 3. **系统生成的评论不可删**：`STATUS_CHANGE` / `ASSIGN` / `PROGRESS_AUTO` /
//!    `AI_PROGRESS` 一律 **403**（连超管也不行）—— 它们是任务的时间线事实，
//!    删掉等于篡改「这个任务发生过什么」。判定只有一处：[`comment_delete_verdict`]。
//! 4. **`actual_hours` 只由服务端汇总**：登记 / 删除工时的同一个事务里跑
//!    [`sync_actual_hours`]（`SUM(hours)`），接口**不接受**任何 `actual_hours` 入参。
//!
//! # 与 T2/D4 的关系（父任务进度不受工时影响）
//!
//! 父任务进度由 `state::aggregate_progress` 按**子任务 `progress` × `estimate_hours`**
//! 加权算出（D4/D5），与本卡登记的工时**无关**；工时只回写任务自己的
//! `actual_hours`。因此「有子任务的任务登记工时」不会触发父任务进度重算 ——
//! 这不是漏实现，而是两条口径本来就不同（预估工时决定权重，实际工时只做成本统计）。
//! 父任务的 `actual_hours` 也不汇总子任务（否则一个 20 层任务树会被重复累加）。
//!
//! SCOPE-EXEMPT: 本文件所有子表查询都以「父任务可见性已判定」为前提
//! （handler 先走 `model::find_detail`，那是 scope ∪ 参与关系过滤的唯一实现），
//! 子表只按 `task_id` 收口。再注入一次 scope 会造成「父任务可见、子行被裁掉」
//! —— 比漏权限更难发现（用户会以为评论/附件丢了），故显式豁免。

use axum::Json;
use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Response};
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::storage;
use crate::business::task::dto::{
    self, PERM_ATTACHMENT_DELETE, PERM_COMMENT_DELETE, PERM_WORKLOG, TaskAttachmentDeleteRes, TaskAttachmentItemRes,
    TaskAttachmentsRes, TaskCommentDeleteRes, TaskCommentItemRes, TaskCommentPageQuery, TaskCommentsRes,
    TaskWorklogCreateReq, TaskWorklogDeleteRes, TaskWorklogItemRes, TaskWorklogSaveRes, TaskWorklogsRes,
};
use crate::business::task::handler::{not_found, parse_uuid_field};
use crate::business::task::model::{self, TaskDetailRow};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（上限 / 值域，唯一来源） ====================

/// 单任务附件上限（卡面 §二 规则 2）。
pub const MAX_TASK_ATTACHMENTS: i64 = 50;

/// 单任务工时记录上限（卡面 §三 规则 5，防刷）。
pub const MAX_TASK_WORKLOGS: i64 = 500;

/// 单条工时的下限（15 分钟）。
pub const MIN_WORKLOG_HOURS: f64 = 0.25;

/// 单条工时的上限（一个工作日）。
pub const MAX_WORKLOG_HOURS: f64 = 24.0;

/// 工时说明的最大字符数（与 `biz_task_worklog.description` 的 TEXT 相比留余量）。
pub const MAX_WORKLOG_NOTE_CHARS: usize = 500;

/// **只读**（系统生成）的评论类型：一律不可编辑 / 删除，连超管也不行。
///
/// 卡面 §一.6 点名三种（`STATUS_CHANGE` / `ASSIGN` / `PROGRESS_AUTO`），
/// 外加 T5b 的 `AI_PROGRESS`（AI 进度回写，同属「机器生成的时间线」）。
/// `AI_REPORT` **不在**这个集合里：它是 AI 的执行报告，属「某个作者写下的一段话」，
/// 管理员可以按普通评论的规则清理（作者是 ROBOT，自己删不了）。
pub const READONLY_COMMENT_TYPES: [&str; 4] = ["STATUS_CHANGE", "ASSIGN", "PROGRESS_AUTO", "AI_PROGRESS"];

/// @ 提及通知里评论摘要的字符上限。
pub const MENTION_EXCERPT_CHARS: usize = 120;

// ==================== 纯判定（无 IO，单测直接覆盖） ====================

/// 删除某条评论为什么被拒（文案与状态码的唯一来源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteDeny {
    /// 系统生成的只读评论。
    Readonly,
    /// 既不是作者、也没有删除权限。
    NotAllowed,
}

/// 评论能否被当前用户删除（卡面 §一 规则 2 / 7）。
///
/// | 评论类型 | 作者本人 | 有 `business:task:comment:delete` |
/// |---|---|---|
/// | 只读类型（[`READONLY_COMMENT_TYPES`]） | ❌ | ❌ |
/// | 其它（`PLAIN` / `AI_REPORT`） | ✅ | ✅ |
///
/// 顺序很重要：**先判只读**。否则「作者本人」会把系统评论放过去
/// （系统评论的作者往往是操作人本人 —— 转派评论的 `user_id` 就是分派人）。
pub fn comment_delete_verdict(
    comment_type: &str,
    is_author: bool,
    has_permission: bool,
) -> std::result::Result<(), DeleteDeny> {
    if comment_is_readonly(comment_type) {
        return Err(DeleteDeny::Readonly);
    }
    if is_author || has_permission {
        return Ok(());
    }
    Err(DeleteDeny::NotAllowed)
}

/// 是不是系统生成的只读评论类型。
pub fn comment_is_readonly(comment_type: &str) -> bool {
    READONLY_COMMENT_TYPES
        .iter()
        .any(|kind| kind.eq_ignore_ascii_case(comment_type))
}

/// 工时值的校验与归一（卡面 §三 规则 2）。
///
/// # 行为
///
/// - `NaN` / `±∞` → 拒（JSON 里 `1e400` 会解析成 `inf`，DB 侧报 500 就晚了）；
/// - `< 0.25` / `> 24` → 拒；
/// - 其余按 `NUMERIC(5,1)` 的口径**四舍五入到 1 位小数**（与落库值一致，
///   回执里就不会出现「提交 1.25、回执 1.3」这种自相矛盾）。
pub fn normalize_worklog_hours(raw: f64) -> std::result::Result<f64, String> {
    if !raw.is_finite() {
        return Err("工时必须是有限数字".to_string());
    }
    if raw < MIN_WORKLOG_HOURS {
        return Err(format!("单条工时不能小于 {MIN_WORKLOG_HOURS} 小时（15 分钟）"));
    }
    if raw > MAX_WORKLOG_HOURS {
        return Err(format!("单条工时不能超过 {MAX_WORKLOG_HOURS} 小时（一个工作日）"));
    }
    Ok((raw * 10.0).round() / 10.0)
}

/// 工时日期不得晚于今天（卡面 §三 规则 2）。比较口径 = **CST 的今天**。
pub fn worklog_date_verdict(date: NaiveDate, today: NaiveDate) -> std::result::Result<(), String> {
    if date > today {
        return Err(format!("工时日期不能晚于今天（{today}）"));
    }
    Ok(())
}

/// 附件数量配额：`current` 是**当前未删附件数**，返回 `Err(上限)` 表示不能再传。
pub fn attachment_quota_verdict(current: i64) -> std::result::Result<(), i64> {
    if current >= MAX_TASK_ATTACHMENTS {
        return Err(MAX_TASK_ATTACHMENTS);
    }
    Ok(())
}

/// 工时记录数量配额（防刷）。
pub fn worklog_quota_verdict(current: i64) -> std::result::Result<(), i64> {
    if current >= MAX_TASK_WORKLOGS {
        return Err(MAX_TASK_WORKLOGS);
    }
    Ok(())
}

/// @ 提及的收件人：去重 + 剔除作者自己 + 保持入参顺序（前端按顺序展示）。
pub fn mention_targets(author: Uuid, mentions: &[Uuid]) -> Vec<Uuid> {
    let mut list: Vec<Uuid> = Vec::with_capacity(mentions.len());
    for user_id in mentions {
        if *user_id == author || list.contains(user_id) {
            continue;
        }
        list.push(*user_id);
    }
    list
}

/// 评论摘要（通知正文用）：按**字符**截断，折叠换行。
///
/// 按字节截断会在中文上切出半个字（通知正文直接显示乱码），所以用
/// [`dto::truncate_chars`] 并先把换行压成空格。
pub fn comment_excerpt(content: &str) -> String {
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    dto::truncate_chars(&single_line, MENTION_EXCERPT_CHARS)
}

/// 解析 `work_date`（`YYYY-MM-DD`）。
pub fn parse_work_date(raw: &str) -> std::result::Result<NaiveDate, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("工时日期不能为空（格式 YYYY-MM-DD）".to_string());
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map_err(|_| format!("工时日期格式非法（应为 YYYY-MM-DD）：{raw}"))
}

// ==================== 数据访问（子表，按 task_id 收口） ====================

/// 评论行（列表 / 单条读取共用）。
#[derive(Debug, Clone)]
pub struct CommentRow {
    /// 评论 ID。
    pub id: Uuid,
    /// 评论类型。
    pub comment_type: String,
    /// 评论人。
    pub user_id: Uuid,
    /// 评论人姓名。
    pub user_name: String,
    /// 正文（Markdown 原文）。
    pub content: String,
    /// 被 @ 的用户 ID（jsonb 解析后的结果）。
    pub mentions: Vec<Uuid>,
    /// 回复目标。
    pub parent_comment_id: Option<Uuid>,
    /// 发表时间。
    pub created_at: chrono::DateTime<Utc>,
}

/// 评论列表要读的列（唯一来源，避免两处 SELECT 漂移）。
const COMMENT_COLUMNS: &str = "c.id, c.comment_type, c.user_id, \
     COALESCE(NULLIF(u.real_name, ''), u.username, '') AS user_name, \
     c.content, c.mentions::text AS mentions, c.parent_comment_id, c.created_at";

impl CommentRow {
    /// 从查询行构造（`mentions` 是 jsonb 的文本形态）。
    fn from_row(row: &Row) -> DbResult<Self> {
        let raw_mentions: Option<String> = row.get_typed("mentions")?;
        Ok(Self {
            id: row.get_typed("id")?,
            comment_type: row.get_typed("comment_type")?,
            user_id: row.get_typed("user_id")?,
            user_name: row.get_typed("user_name")?,
            content: row.get_typed("content")?,
            mentions: parse_mentions(raw_mentions.as_deref()),
            parent_comment_id: row.get_typed("parent_comment_id")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 把 `mentions` 的 jsonb 文本解析成 UUID 列表（脏值一律跳过而不是 500）。
fn parse_mentions(raw: Option<&str>) -> Vec<Uuid> {
    let Some(text) = raw else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter_map(|item| Uuid::parse_str(item).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// 评论分页（时间正序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_comments(task_id: Uuid, page: u64, page_size: u64) -> DbResult<(Vec<CommentRow>, u64)> {
    let offset = (page.saturating_sub(1)) * page_size;
    let sql = format!(
        "SELECT {COMMENT_COLUMNS} \
           FROM biz_task_comment c LEFT JOIN sys_user u ON u.id = c.user_id \
          WHERE c.task_id = ?::uuid AND c.deleted_at IS NULL \
          ORDER BY c.created_at ASC, c.id ASC \
          LIMIT ?::int8 OFFSET ?::int8"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![
                Value::from(&task_id),
                Value::I64(i64::try_from(page_size).unwrap_or(50)),
                Value::I64(i64::try_from(offset).unwrap_or(0)),
            ],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(CommentRow::from_row(row)?);
    }
    let total: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_task_comment WHERE task_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&task_id)],
        )
        .await?;
    Ok((list, u64::try_from(total).unwrap_or(0)))
}

/// 取单条未删评论（删除前的所有权 / 类型判定）。
pub async fn find_comment(task_id: Uuid, comment_id: Uuid) -> DbResult<Option<CommentRow>> {
    let sql = format!(
        "SELECT {COMMENT_COLUMNS} \
           FROM biz_task_comment c LEFT JOIN sys_user u ON u.id = c.user_id \
          WHERE c.id = ?::uuid AND c.task_id = ?::uuid AND c.deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&comment_id), Value::from(&task_id)])
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(CommentRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 软删评论（只写 `deleted_at`；**不级联删附件** —— 附件是任务级资源，
/// 卡面 §一 规则 3 明确要求保留审计）。
pub async fn soft_delete_comment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    comment_id: Uuid,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task_comment \
            SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND task_id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&operator), Value::from(&comment_id), Value::from(&task_id)],
    )
    .await
}

/// 附件行（列表 / 单条读取共用）。
#[derive(Debug, Clone)]
pub struct AttachmentRow {
    /// 绑定行 ID。
    pub id: Uuid,
    /// 文件 ID。
    pub file_id: Uuid,
    /// 原始文件名。
    pub file_name: String,
    /// 文件大小（字节）。
    pub file_size: i64,
    /// MIME。
    pub mime_type: String,
    /// 来源 `HUMAN` / `AI`。
    pub source: String,
    /// 产出该附件的 AI profile。
    pub ai_profile: Option<String>,
    /// 上传人。
    pub uploader_id: Option<Uuid>,
    /// 上传人姓名。
    pub uploader_name: String,
    /// 上传时间。
    pub created_at: chrono::DateTime<Utc>,
}

/// 附件要读的列（唯一来源）。
///
/// **`original_name` / `file_name` 必须分开取原始值**：显示名由
/// [`model::display_file_name`] 在 `from_row` 里算（优先原始名，回退 basename）。
/// 这里若再写 `COALESCE(f.original_name, f.file_name)`，存储路径
/// （`2026/10/29/….md`）就会漏进 `file_name` 下发到前端当下载保存名（E12）。
///
/// # `uploader_name` 的三段回落（AUD-01）
///
/// ```text
///   ① a.uploader_name            上传时落库的姓名快照 —— 优先（改名后仍显示当时的名字）
///   ② u.real_name / u.username   实时 join —— 只给 V0352 之前的历史行兜底（回填未跑）
///   ③ ''                          两者都没有 → 空串，前端显示 '-'（**永不回落 UUID**）
/// ```
///
/// 上传人 id 在 `create_by`（行级删除判据 `uploader_id == me` 用它），姓名快照单独一列 ——
/// 两者一起更新（见 `task_model_ai::bind_*_attachment` 的「复活」分支），
/// 否则会出现「界面显示 B、B 却删不掉」的错位。
const ATTACHMENT_COLUMNS: &str = "a.id, a.file_id, \
     COALESCE(f.original_name, '') AS original_name, \
     COALESCE(f.file_name, '') AS file_name, \
     COALESCE(f.file_size, 0) AS file_size, \
     COALESCE(f.mime_type, '') AS mime_type, \
     a.source, a.ai_profile, a.create_by, a.created_at, \
     COALESCE(NULLIF(a.uploader_name, ''), NULLIF(u.real_name, ''), u.username, '') AS uploader_name";

impl AttachmentRow {
    /// 从查询行构造。
    fn from_row(row: &Row) -> DbResult<Self> {
        let stored: String = row.get_typed("file_name")?;
        let original: Option<String> = row.get_typed("original_name")?;
        Ok(Self {
            id: row.get_typed("id")?,
            file_id: row.get_typed("file_id")?,
            file_name: model::display_file_name(original.as_deref(), &stored),
            file_size: row.get_typed("file_size")?,
            mime_type: row.get_typed("mime_type")?,
            source: row.get_typed("source")?,
            ai_profile: row.get_typed("ai_profile")?,
            uploader_id: row.get_typed("create_by")?,
            uploader_name: row.get_typed("uploader_name")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 附件列表（未软删，按 `sort_order` 升序）。
pub async fn list_attachment_rows(task_id: Uuid) -> DbResult<Vec<AttachmentRow>> {
    let sql = format!(
        "SELECT {ATTACHMENT_COLUMNS} \
           FROM biz_task_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.create_by \
          WHERE a.task_id = ?::uuid AND a.deleted_at IS NULL \
          ORDER BY a.sort_order ASC, a.created_at ASC"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&task_id)]).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(AttachmentRow::from_row(row)?);
    }
    Ok(list)
}

/// 取单条未删附件（下载 / 删除前判定「这个附件属于这个任务」）。
pub async fn find_attachment(task_id: Uuid, attachment_id: Uuid) -> DbResult<Option<AttachmentRow>> {
    let sql = format!(
        "SELECT {ATTACHMENT_COLUMNS} \
           FROM biz_task_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.create_by \
          WHERE a.id = ?::uuid AND a.task_id = ?::uuid AND a.deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&attachment_id), Value::from(&task_id)])
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(AttachmentRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 当前未删附件数（配额判定；与 `list_attachment_rows` 同口径）。
pub async fn count_attachments(task_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_task_attachment \
          WHERE task_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&task_id)],
    )
    .await
}

/// 软删附件绑定行。
///
/// 只写 `deleted_at`：`biz_task_attachment` 没有 `updated_at` / `update_by` 列
/// （绑定关系是一次性事实），所以这里不需要操作人参数 —— 谁删的由操作日志记录。
pub async fn soft_delete_attachment(ex: &impl SqlExecutor, attachment_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task_attachment SET deleted_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&attachment_id)],
    )
    .await
}

/// 工时行。
#[derive(Debug, Clone)]
pub struct WorklogRow {
    /// 记录 ID。
    pub id: Uuid,
    /// 填报人。
    pub user_id: Uuid,
    /// 填报人姓名。
    pub user_name: String,
    /// 工时日期。
    pub work_date: NaiveDate,
    /// 工时（小时）。
    pub hours: f64,
    /// 工作说明。
    pub description: Option<String>,
    /// 记录时间。
    pub created_at: chrono::DateTime<Utc>,
}

/// 工时列表要读的列（唯一来源）。
const WORKLOG_COLUMNS: &str = "w.id, w.user_id, \
     COALESCE(NULLIF(u.real_name, ''), u.username, '') AS user_name, \
     w.work_date, w.hours::float8 AS hours, w.description, w.created_at";

impl WorklogRow {
    /// 从查询行构造。
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            user_id: row.get_typed("user_id")?,
            user_name: row.get_typed("user_name")?,
            work_date: row.get_typed("work_date")?,
            hours: row.get_typed("hours")?,
            description: row.get_typed("description")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 工时列表（日期倒序）。
///
/// 名字带 `_rows`：同名 handler [`list_worklogs`] 与它只差一个「人看的是接口、
/// 库看的是行」的语义，同名会让两处调用在阅读时分不清谁在查库。
pub async fn worklog_rows(task_id: Uuid) -> DbResult<Vec<WorklogRow>> {
    let sql = format!(
        "SELECT {WORKLOG_COLUMNS} \
           FROM biz_task_worklog w LEFT JOIN sys_user u ON u.id = w.user_id \
          WHERE w.task_id = ?::uuid \
          ORDER BY w.work_date DESC, w.created_at DESC"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&task_id)]).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(WorklogRow::from_row(row)?);
    }
    Ok(list)
}

/// 单任务工时记录数（配额判定）。
pub async fn count_worklogs(task_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_task_worklog WHERE task_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await
}

/// 取单条工时（删除前的所有权判定）。
pub async fn find_worklog(task_id: Uuid, worklog_id: Uuid) -> DbResult<Option<WorklogRow>> {
    let sql = format!(
        "SELECT {WORKLOG_COLUMNS} \
           FROM biz_task_worklog w LEFT JOIN sys_user u ON u.id = w.user_id \
          WHERE w.id = ?::uuid AND w.task_id = ?::uuid"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&worklog_id), Value::from(&task_id)])
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(WorklogRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 插入一条工时记录。
///
/// 日期一律按 `YYYY-MM-DD` 文本绑定，且写成 `?::text::date`：直接 `?::date`
/// 时驱动看到目标类型是 `date`、拿到的是 `Text`，会报
/// `error serializing parameter N`（本仓库 A5 / 公告卡的实测坑）。
pub async fn insert_worklog(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    user_id: Uuid,
    work_date: NaiveDate,
    hours: f64,
    description: Option<&str>,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let date_text = work_date.format("%Y-%m-%d").to_string();
    ex.exec_sql(
        "INSERT INTO biz_task_worklog \
             (id, task_id, user_id, work_date, hours, description, created_at, create_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::text::date, ?::numeric, ?, now(), ?::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(date_text),
            Value::from(hours),
            match description {
                Some(text) => Value::Text(text.to_string()),
                None => Value::Null,
            },
            Value::from(&user_id),
        ],
    )
    .await?;
    Ok(id)
}

/// 删除一条工时记录（硬删：工时是日志类数据，表本身没有 `deleted_at`）。
///
/// 名字带 `_row`：与同名 handler [`delete_worklog`] 区分（同模块同名函数直接编不过）。
pub async fn delete_worklog_row(ex: &impl SqlExecutor, task_id: Uuid, worklog_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_worklog WHERE id = ?::uuid AND task_id = ?::uuid".to_string(),
        vec![Value::from(&worklog_id), Value::from(&task_id)],
    )
    .await
}

/// 服务端汇总：`biz_task.actual_hours = SUM(未删工时)`。
///
/// # 为什么必须是「服务端算」
///
/// 卡面红线：「工时 `actual_hours` 必须服务端汇总（不许前端传）」。
/// 前端传 = 任何人可以把「我干了 40 小时」写进任务卡而列表上看不出；
/// 而 `actual_hours` 会进统计与成本口径，一旦可写就等于成本数据可造假。
///
/// # 语义
///
/// - 没有工时记录 → `actual_hours = NULL`（**不是 0**：「还没登记」与「登记了 0」
///   是两件事，详情页靠这个区分显示 `-` 还是 `0h`）；
/// - 有记录 → `SUM(hours)`（`NUMERIC(6,1)` 精度）。
pub async fn sync_actual_hours(ex: &impl SqlExecutor, task_id: Uuid, operator: Uuid) -> DbResult<()> {
    ex.exec_sql(
        "UPDATE biz_task t \
            SET actual_hours = (SELECT sum(w.hours)::numeric \
                                  FROM biz_task_worklog w WHERE w.task_id = t.id), \
                updated_at = now(), \
                update_by = ?::uuid \
          WHERE t.id = ?::uuid"
            .to_string(),
        vec![Value::from(&operator), Value::from(&task_id)],
    )
    .await?;
    Ok(())
}

/// 当前任务的 `actual_hours`（回执用）。
pub async fn actual_hours_of(task_id: Uuid) -> DbResult<Option<f64>> {
    let rows = db()
        .find_with_params(
            "SELECT actual_hours::float8 AS actual_hours FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("actual_hours"),
        None => Ok(None),
    }
}

/// 工时合计（列表回执的 `sum_hours`）。
pub async fn sum_worklog_hours(task_id: Uuid) -> DbResult<f64> {
    db().scalar_with_params(
        "SELECT COALESCE(sum(hours), 0)::float8 FROM biz_task_worklog WHERE task_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await
}

// ==================== 内部工具 ====================

/// 可见性闸：解析数据范围 + 单条任务可见性。
///
/// 看不见 / 不存在 / 已软删一律 **404**（不暴露 ID 是否存在）。
pub(crate) async fn visible_task(
    claims: &TokenClaims,
    task_id: Uuid,
) -> std::result::Result<(ScopedPrincipal, TaskDetailRow), ApiError> {
    let scope_principal = principal::resolve_for_claims(claims).await?;
    let row = model::find_detail(&scope_principal, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)?;
    Ok((scope_principal, row))
}

/// 当前用户 UUID（`sub`）。
fn me_of(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 404（评论 / 附件 / 工时不存在，或不属于该任务）。
fn child_not_found(label: &str) -> ApiError {
    ApiError::new(404, error_code::TASK_NOT_FOUND, format!("{label}不存在或已删除"))
}

/// 403（本卡的三类拒绝共用一处文案出口）。
fn forbidden(message: &str) -> ApiError {
    ApiError::new(403, error_code::TASK_FORBIDDEN, message.to_string())
}

// ==================== 评论 ====================

/// 评论列表（`GET /business/task/{id}/comments`）。
///
/// # 规则
///
/// - 能看见任务的人都能看评论（卡面 §一 规则 1）；
/// - 时间**正序**（时间线的阅读方向），分页；
/// - `readonly` / `can_delete` 由服务端算好下发：前端不许自己推导
///   （推导逻辑一旦与后端不一致，就会出现「按钮能点但接口 403」的交互死路）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 看不见；400 + `10001`：id 不是 UUID；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/comments",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/comments")]
pub async fn list_comments(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<TaskCommentPageQuery>,
) -> ResResult<TaskCommentsRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let (rows, total) = page_comments(task_id, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "查询任务评论"))?;
    let has_delete_permission = claims.has_permission(PERM_COMMENT_DELETE);
    let list = rows
        .iter()
        .map(|row| TaskCommentItemRes {
            id: row.id.to_string(),
            comment_type: row.comment_type.clone(),
            user_id: row.user_id.to_string(),
            user_name: row.user_name.clone(),
            content: row.content.clone(),
            mentions: row.mentions.iter().map(Uuid::to_string).collect(),
            parent_comment_id: row.parent_comment_id.map(|parent| parent.to_string()),
            created_at: row.created_at,
            readonly: comment_is_readonly(&row.comment_type),
            can_delete: comment_delete_verdict(&row.comment_type, row.user_id == me, has_delete_permission).is_ok(),
        })
        .collect();

    Ok(Res::ok(TaskCommentsRes {
        list,
        total,
        page,
        page_size,
    }))
}

/// 删除评论（`DELETE /business/task/{id}/comments/{comment_id}`）。
///
/// # 权限矩阵（卡面 §一 规则 2 / 7）
///
/// | 类型 | 作者本人 | `business:task:comment:delete` |
/// |---|---|---|
/// | `STATUS_CHANGE` / `ASSIGN` / `PROGRESS_AUTO` / `AI_PROGRESS` | ❌ 403 | ❌ 403 |
/// | `PLAIN` / `AI_REPORT` | ✅ | ✅ |
///
/// 软删（`deleted_at`）：列表不再返回，但**不级联删附件**（保留审计）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务看不见 / 评论不存在 / 已删；
/// - 403 + `63002`：只读类型，或既非作者也无删除权限。
#[nik::permission(
    path = "/business/task/{id}/comments/{comment_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/comments/{comment_id}")]
pub async fn delete_comment(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, comment_id)): Path<(String, String)>,
) -> ResResult<TaskCommentDeleteRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let comment_uuid = parse_uuid_field(&comment_id, "评论")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let row = find_comment(task_id, comment_uuid)
        .await
        .map_err(|error| utils::db_error(error, "查询评论"))?
        .ok_or_else(|| child_not_found("评论"))?;

    match comment_delete_verdict(
        &row.comment_type,
        row.user_id == me,
        claims.has_permission(PERM_COMMENT_DELETE),
    ) {
        Ok(()) => {}
        Err(DeleteDeny::Readonly) => {
            return Err(forbidden(
                "系统生成的评论（状态变更 / 分派 / 进度汇总 / AI 进度）不可删除",
            ));
        }
        Err(DeleteDeny::NotAllowed) => {
            return Err(forbidden(
                "只能删除自己的评论，或持有 business:task:comment:delete 权限",
            ));
        }
    }

    let affected = soft_delete_comment(db(), task_id, comment_uuid, me)
        .await
        .map_err(|error| utils::db_error(error, "删除评论"))?;
    tracing::info!(task_id = %task_id, comment_id = %comment_uuid, operator = %me, "任务评论已软删");
    Ok(Res::ok(TaskCommentDeleteRes {
        id: comment_uuid.to_string(),
        deleted: affected > 0,
    }))
}

// ==================== 附件 ====================

/// 附件列表（`GET /business/task/{id}/attachments`）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 看不见；500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/attachments",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/attachments")]
pub async fn list_attachments(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskAttachmentsRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let rows = list_attachment_rows(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务附件"))?;
    let has_delete_permission = claims.has_permission(PERM_ATTACHMENT_DELETE);
    let list = rows
        .iter()
        .map(|row| TaskAttachmentItemRes {
            id: row.id.to_string(),
            file_id: row.file_id.to_string(),
            file_name: row.file_name.clone(),
            file_size: row.file_size,
            mime_type: row.mime_type.clone(),
            source: row.source.clone(),
            ai_profile: row.ai_profile.clone(),
            uploader_id: row.uploader_id.map(|user| user.to_string()),
            uploader_name: row.uploader_name.clone(),
            created_at: row.created_at,
            can_delete: has_delete_permission || row.uploader_id == Some(me),
        })
        .collect();

    Ok(Res::ok(TaskAttachmentsRes {
        list,
        total: u64::try_from(rows.len()).unwrap_or(0),
        max_count: MAX_TASK_ATTACHMENTS,
    }))
}

/// 删除附件（`DELETE /business/task/{id}/attachments/{att_id}`）。
///
/// # 语义（卡面 §二 规则 5）
///
/// 1. 绑定行软删（`biz_task_attachment.deleted_at`）—— 列表立刻消失，可审计；
/// 2. 文件记录软删（`sys_file.deleted_at`）—— 物理对象由 storage 的清理任务
///    在 7 天后回收（A 批次既有策略，本卡**不另造回收通道**）。
///
/// 第 2 步失败不影响第 1 步的结果（文件回收是后台补偿的活）：只记 warn，
/// 否则「附件已经在列表里消失、用户却收到 500」会让人反复重试。
///
/// # 异常情况
///
/// - 404：任务看不见 / 附件不存在 / 已删；
/// - 403：既不是上传者，也没有 `business:task:attachment:delete`。
#[nik::permission(
    path = "/business/task/{id}/attachments/{att_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/attachments/{att_id}")]
pub async fn delete_attachment(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, att_id)): Path<(String, String)>,
) -> ResResult<TaskAttachmentDeleteRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let attachment_uuid = parse_uuid_field(&att_id, "附件")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let row = find_attachment(task_id, attachment_uuid)
        .await
        .map_err(|error| utils::db_error(error, "查询任务附件"))?
        .ok_or_else(|| child_not_found("附件"))?;

    if row.uploader_id != Some(me) && !claims.has_permission(PERM_ATTACHMENT_DELETE) {
        return Err(forbidden(
            "只能删除自己上传的附件，或持有 business:task:attachment:delete 权限",
        ));
    }

    let affected = soft_delete_attachment(db(), attachment_uuid)
        .await
        .map_err(|error| utils::db_error(error, "删除任务附件"))?;

    // 文件记录软删（物理对象延迟回收）。失败只记 warn：绑定行已经删掉，
    // 抛 500 会让用户以为删除失败而反复重试。
    match storage::model::soft_delete_file(&row.file_id, Some(me)).await {
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                task_id = %task_id,
                file_id = %row.file_id,
                error = %error,
                "任务附件已软删，但 sys_file 标记失败（等待清理任务兜底）"
            );
        }
    }

    tracing::info!(
        task_id = %task_id,
        attachment_id = %attachment_uuid,
        file_id = %row.file_id,
        operator = %me,
        "任务附件已删除"
    );
    Ok(Res::ok(TaskAttachmentDeleteRes {
        id: attachment_uuid.to_string(),
        file_id: row.file_id.to_string(),
        deleted: affected > 0,
    }))
}

/// 下载任务附件（`GET /business/task/{id}/attachments/{att_id}/download`）。
///
/// # 红线：**先校验任务可见性**
///
/// 顺序固定为「任务可见 → 附件属于该任务 → 文件未软删 → 交给 storage 出流」。
/// 只凭 `att_id` 就出流 = 任何登录用户遍历 UUID 就能拖走全公司的任务附件
/// （UUID v7 是时间有序的，枚举成本比随机 UUID 低得多）。
///
/// 出流复用 A 批次的 [`storage::handler::download_response`]：`LOCAL` 后端流式、
/// `S3` 后端 302 到现签 URL —— **不另造上传/下载通道**（卡面红线）。
///
/// # 异常情况
///
/// - 404：任务看不见 / 附件不存在 / 文件已软删 / 物理对象丢失（**一律 404**，
///   不区分「不存在」与「无权限」，避免把 UUID 变成可枚举资源）；
/// - 409 / 502：存储后端不可用 / 远端访问失败（透传 storage 的错误）。
#[nik::permission(
    path = "/business/task/{id}/attachments/{att_id}/download",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/attachments/{att_id}/download")]
pub async fn download_attachment(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, att_id)): Path<(String, String)>,
) -> Response {
    match download_attachment_inner(&claims, &id, &att_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载的实际流程（错误走 [`ApiError`]，便于 `into_response`）。
async fn download_attachment_inner(
    claims: &TokenClaims,
    raw_task_id: &str,
    raw_attachment_id: &str,
) -> std::result::Result<Response, ApiError> {
    let task_id = parse_uuid_field(raw_task_id, "任务")?;
    let attachment_uuid = parse_uuid_field(raw_attachment_id, "附件")?;
    let (_scope, _task) = visible_task(claims, task_id).await?;

    let row = find_attachment(task_id, attachment_uuid)
        .await
        .map_err(|error| utils::db_error(error, "查询任务附件"))?
        .ok_or_else(|| child_not_found("附件"))?;

    storage::handler::download_response(&row.file_id.to_string())
        .await
        .inspect_err(|error| {
            // storage 的 404（文件不存在 / 已软删）在这里统一表述成「附件不可下载」，
            // 但状态码与业务码保持 storage 的口径（前端已按它做提示）。
            tracing::warn!(
                task_id = %task_id,
                attachment_id = %attachment_uuid,
                file_id = %row.file_id,
                error_code = error.code,
                error = %error.msg,
                "任务附件下载失败"
            );
        })
}

// ==================== 工时 ====================

/// 工时列表（`GET /business/task/{id}/worklogs`）。
///
/// 返回 `list` + `total` + `sum_hours`（明细合计）+ `actual_hours`
/// （任务卡上的冗余汇总，两者在正常情况下**必然相等** —— 不相等就说明
/// 有人绕过接口改了库，界面上会让验收人自己发现）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务看不见；500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/worklogs",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/worklogs")]
pub async fn list_worklogs(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskWorklogsRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let rows = worklog_rows(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务工时"))?;
    let sum_hours = sum_worklog_hours(task_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总任务工时"))?;
    let actual_hours = actual_hours_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询实际工时"))?;
    let has_permission = claims.has_permission(PERM_WORKLOG);

    let list = rows
        .iter()
        .map(|row| TaskWorklogItemRes {
            id: row.id.to_string(),
            user_id: row.user_id.to_string(),
            user_name: row.user_name.clone(),
            work_date: row.work_date,
            hours: row.hours,
            description: row.description.clone(),
            created_at: row.created_at,
            can_delete: row.user_id == me || has_permission,
        })
        .collect();

    Ok(Res::ok(TaskWorklogsRes {
        total: u64::try_from(rows.len()).unwrap_or(0),
        list,
        sum_hours,
        actual_hours,
        max_records: MAX_TASK_WORKLOGS,
    }))
}

/// 登记工时（`POST /business/task/{id}/worklogs`）。
///
/// # 谁能登记（卡面 §三 规则 1）
///
/// 负责人 / 协作人 / 持有 `business:task:worklog` 的人。创建人不额外放行 ——
/// 创建人通常就是负责人；如果他不是，那「他没在这条任务上干活」才是常态。
///
/// # 校验顺序（越靠前越便宜，且失败语义不受后者影响）
///
/// 1. 可见性（404）；
/// 2. 登记资格（403）；
/// 3. 数量配额（400）；
/// 4. 工时区间 / 日期（400）；
/// 5. 落库 + **同一事务内**重算 `actual_hours`。
///
/// # 异常情况
///
/// - 400 + `63009`：工时区间 / 日期非法、超过 500 条上限；
/// - 403 + `63002`：不是负责人 / 协作人，也没有 `business:task:worklog`；
/// - 404 + `63001`：任务看不见。
#[nik::permission(
    path = "/business/task/{id}/worklogs",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/worklogs")]
pub async fn add_worklog(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskWorklogCreateReq>,
) -> ResResult<TaskWorklogSaveRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = me_of(&claims)?;
    let (_scope, task) = visible_task(&claims, task_id).await?;

    ensure_worklog_allowed(&claims, &task, task_id, me).await?;

    let current = count_worklogs(task_id)
        .await
        .map_err(|error| utils::db_error(error, "统计任务工时"))?;
    if worklog_quota_verdict(current).is_err() {
        return Err(ApiError::new(
            400,
            error_code::TASK_FIELD_INVALID,
            format!("单任务工时记录最多 {MAX_TASK_WORKLOGS} 条，请先清理历史记录"),
        ));
    }

    let hours = normalize_worklog_hours(req.hours)
        .map_err(|message| ApiError::new(400, error_code::TASK_FIELD_INVALID, message))?;
    let work_date = parse_work_date(&req.work_date)
        .map_err(|message| ApiError::new(400, error_code::TASK_FIELD_INVALID, message))?;
    worklog_date_verdict(work_date, model::today_cst())
        .map_err(|message| ApiError::new(400, error_code::TASK_FIELD_INVALID, message))?;

    let note = req
        .note
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| dto::truncate_chars(text, MAX_WORKLOG_NOTE_CHARS));

    let worklog_id = db()
        .transaction(|tx| async move {
            let id = insert_worklog(&tx, task_id, me, work_date, hours, note.as_deref()).await?;
            sync_actual_hours(&tx, task_id, me).await?;
            Ok(id)
        })
        .await
        .map_err(|error| utils::db_error(error, "登记工时"))?;

    let sum_hours = sum_worklog_hours(task_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总任务工时"))?;
    let actual_hours = actual_hours_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询实际工时"))?;

    tracing::info!(
        task_id = %task_id,
        worklog_id = %worklog_id,
        hours,
        %work_date,
        actual_hours = actual_hours.unwrap_or(0.0),
        operator = %me,
        "任务工时已登记"
    );

    Ok(Res::ok(TaskWorklogSaveRes {
        id: worklog_id.to_string(),
        work_date,
        hours,
        sum_hours,
        actual_hours,
    }))
}

/// 删除工时（`DELETE /business/task/{id}/worklogs/{wl_id}`）。
///
/// 本人 / `business:task:worklog`。硬删（日志类表没有 `deleted_at`），
/// 删完在同一事务里重算 `actual_hours` —— 否则任务卡会一直挂着已删除的工时。
///
/// # 异常情况
///
/// - 404：任务看不见 / 工时记录不存在；
/// - 403：既不是本人，也没有 `business:task:worklog`。
#[nik::permission(
    path = "/business/task/{id}/worklogs/{wl_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/worklogs/{wl_id}")]
pub async fn delete_worklog(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, wl_id)): Path<(String, String)>,
) -> ResResult<TaskWorklogDeleteRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let worklog_id = parse_uuid_field(&wl_id, "工时记录")?;
    let me = me_of(&claims)?;
    let (_scope, _task) = visible_task(&claims, task_id).await?;

    let row = find_worklog(task_id, worklog_id)
        .await
        .map_err(|error| utils::db_error(error, "查询工时记录"))?
        .ok_or_else(|| child_not_found("工时记录"))?;

    if row.user_id != me && !claims.has_permission(PERM_WORKLOG) {
        return Err(forbidden("只能删除自己的工时记录，或持有 business:task:worklog 权限"));
    }

    let affected = db()
        .transaction(|tx| async move {
            let affected = delete_worklog_row(&tx, task_id, worklog_id).await?;
            sync_actual_hours(&tx, task_id, me).await?;
            Ok(affected)
        })
        .await
        .map_err(|error| utils::db_error(error, "删除工时记录"))?;

    let sum_hours = sum_worklog_hours(task_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总任务工时"))?;
    let actual_hours = actual_hours_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询实际工时"))?;

    tracing::info!(
        task_id = %task_id,
        worklog_id = %worklog_id,
        actual_hours = actual_hours.unwrap_or(0.0),
        operator = %me,
        "任务工时已删除并重算汇总"
    );
    Ok(Res::ok(TaskWorklogDeleteRes {
        id: worklog_id.to_string(),
        deleted: affected > 0,
        sum_hours,
        actual_hours,
    }))
}

/// 工时登记资格：负责人 / 协作人 / `business:task:worklog`。
async fn ensure_worklog_allowed(
    claims: &TokenClaims,
    task: &TaskDetailRow,
    task_id: Uuid,
    me: Uuid,
) -> std::result::Result<(), ApiError> {
    if claims.has_permission(PERM_WORKLOG) || task.card.owner_id == me {
        return Ok(());
    }
    let members = model::member_ids(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    if members.contains(&me) {
        return Ok(());
    }
    Err(forbidden(
        "只有任务负责人、协作人，或持有 business:task:worklog 权限的人可以登记工时",
    ))
}

// ==================== @ 提及通知（被 handler::add_comment 调用） ====================

/// 给「被 @ 的人」发**强制**通知（修正 8：被 @ = 强制，不看收件人偏好）。
///
/// # 参数
///
/// - `task`：评论所属任务（标题 / 编号 / 链接都取自它）；
/// - `content`：评论正文（内部按 [`MENTION_EXCERPT_CHARS`] 截断成摘要）；
/// - `mentions`：被 @ 的用户 ID；
/// - `author` / `author_name`：评论人（作者自己也出现在 @ 列表里时**跳过**，
///   自己 @ 自己不该产生一条通知）。
///
/// # 返回值
///
/// 实际收到通知的用户 ID（去重、剔除作者、剔除不存在 / 已软删的账号）。
///
/// # 为什么失败不影响评论
///
/// 通知是「评论已写入」之后的副作用：`notice::dispatch` 失败只记 warn。
/// 反过来（通知失败就回滚评论）会让用户在通知服务抖动时**写不进评论**。
pub async fn notify_mentions(
    task: &TaskDetailRow,
    content: &str,
    mentions: &[Uuid],
    author: Uuid,
    author_name: &str,
) -> Vec<Uuid> {
    let candidates = mention_targets(author, mentions);
    if candidates.is_empty() {
        return Vec::new();
    }
    let valid = match model::valid_users(&candidates).await {
        Ok(list) => list,
        Err(error) => {
            tracing::warn!(error = %error, "查询被 @ 用户失败，跳过提及通知");
            return Vec::new();
        }
    };
    let targets: Vec<Uuid> = candidates
        .into_iter()
        .filter(|user_id| valid.contains(user_id))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }

    let title = format!("{author_name} 在任务「{}」里提到了你", task.card.title);
    let content = format!(
        "任务：{} {}\n评论：{}",
        task.card.task_no,
        task.card.title,
        comment_excerpt(content)
    );
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        &title,
        &content,
        NoticeTarget::User(targets.clone()),
    );
    draft.link_url = Some(format!("/task/list?id={}", task.card.id));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(task.card.id.to_string());
    draft.sender_id = Some(author);
    draft.sender_name = Some(author_name.to_string());
    // 修正 8：被 @ = 强制通知，不尊重收件人的偏好开关。
    draft.force = true;

    match notice::dispatch(draft).await {
        Ok(result) => {
            tracing::info!(
                task_id = %task.card.id,
                notice_id = %result.notice_id,
                receivers = result.receiver_count,
                "评论提及通知已发送（强制）"
            );
            targets
        }
        Err(error) => {
            tracing::warn!(error = %error, "评论提及通知发送失败（不影响评论）");
            Vec::new()
        }
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("合法日期")
    }

    /// 评论权限矩阵：只读类型连作者本人都删不了（系统评论的作者常是操作人本人）。
    #[test]
    fn comment_delete_matrix() {
        // 普通评论：作者本人可删、管理员可删、旁人不可删
        assert_eq!(comment_delete_verdict("PLAIN", true, false), Ok(()));
        assert_eq!(comment_delete_verdict("PLAIN", false, true), Ok(()));
        assert_eq!(
            comment_delete_verdict("PLAIN", false, false),
            Err(DeleteDeny::NotAllowed)
        );
        // AI 报告：作者是 ROBOT，靠管理员清理
        assert_eq!(comment_delete_verdict("AI_REPORT", false, true), Ok(()));
        // 只读类型：作者 + 管理员都不行
        for kind in READONLY_COMMENT_TYPES {
            assert_eq!(
                comment_delete_verdict(kind, true, true),
                Err(DeleteDeny::Readonly),
                "{kind} 必须只读"
            );
        }
        // 大小写不敏感（库里一律大写，但入参可能来自旧数据）
        assert!(comment_is_readonly("status_change"));
    }

    /// @ 提及：去重、剔除作者、保序。
    #[test]
    fn mention_targets_dedup_and_skip_author() {
        let author = Uuid::now_v7();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let targets = mention_targets(author, &[b, a, author, b]);
        assert_eq!(targets, vec![b, a]);
        assert!(mention_targets(author, &[author]).is_empty());
    }

    /// 评论摘要：压成单行 + 按字符截断（不能把中文切成半个字）。
    #[test]
    fn comment_excerpt_is_char_safe() {
        assert_eq!(comment_excerpt("  hello\n\nworld\t!"), "hello world !");
        let long = "中".repeat(MENTION_EXCERPT_CHARS + 50);
        let excerpt = comment_excerpt(&long);
        assert_eq!(excerpt.chars().count(), MENTION_EXCERPT_CHARS);
        assert!(excerpt.is_char_boundary(excerpt.len()));
    }

    /// 工时值域：0.25–24，且按落库精度归一。
    #[test]
    fn worklog_hours_bounds() {
        assert_eq!(normalize_worklog_hours(0.25), Ok(0.3)); // NUMERIC(5,1) 四舍五入
        assert_eq!(normalize_worklog_hours(2.0), Ok(2.0));
        assert_eq!(normalize_worklog_hours(24.0), Ok(24.0));
        assert_eq!(normalize_worklog_hours(3.5), Ok(3.5));
        // 边界外：拒绝
        assert!(normalize_worklog_hours(0.24).is_err());
        assert!(normalize_worklog_hours(24.5).is_err());
        assert!(normalize_worklog_hours(26.0).is_err());
        assert!(normalize_worklog_hours(0.0).is_err());
        assert!(normalize_worklog_hours(-1.0).is_err());
        // 非有限值（JSON 的 1e400 会解析成 inf）
        assert!(normalize_worklog_hours(f64::INFINITY).is_err());
        assert!(normalize_worklog_hours(f64::NAN).is_err());
    }

    /// 工时日期：今天可以、过去可以、明天拒绝。
    #[test]
    fn worklog_date_not_in_future() {
        let today = date(2026, 10, 29);
        assert_eq!(worklog_date_verdict(today, today), Ok(()));
        assert_eq!(worklog_date_verdict(date(2026, 10, 1), today), Ok(()));
        assert!(worklog_date_verdict(date(2026, 10, 30), today).is_err());
    }

    /// 日期解析：ISO 格式接受，垃圾输入给出可读原因。
    #[test]
    fn work_date_parsing() {
        assert_eq!(parse_work_date("2026-10-29"), Ok(date(2026, 10, 29)));
        assert_eq!(parse_work_date(" 2026-10-29 "), Ok(date(2026, 10, 29)));
        assert!(parse_work_date("2026/10/29").is_err());
        assert!(parse_work_date("").is_err());
        assert!(parse_work_date("2026-02-30").is_err());
    }

    /// 配额：第 50 个附件之后拒绝，第 500 条工时之后拒绝。
    #[test]
    fn quota_verdicts() {
        assert_eq!(attachment_quota_verdict(0), Ok(()));
        assert_eq!(attachment_quota_verdict(49), Ok(()));
        assert_eq!(attachment_quota_verdict(50), Err(MAX_TASK_ATTACHMENTS));
        assert_eq!(attachment_quota_verdict(51), Err(MAX_TASK_ATTACHMENTS));
        assert_eq!(worklog_quota_verdict(499), Ok(()));
        assert_eq!(worklog_quota_verdict(500), Err(MAX_TASK_WORKLOGS));
    }

    /// `mentions` 的 jsonb 解析必须是「脏值跳过」而不是整体失败。
    #[test]
    fn parse_mentions_tolerates_dirty_values() {
        let id = Uuid::now_v7();
        let raw = format!("[\"{id}\", \"not-a-uuid\", 42, null]");
        assert_eq!(parse_mentions(Some(&raw)), vec![id]);
        assert!(parse_mentions(Some("[]")).is_empty());
        assert!(parse_mentions(Some("not json")).is_empty());
        assert!(parse_mentions(None).is_empty());
    }
}

/// E12：附件列清单的结构回归 —— 显示名必须在 Rust 侧算，SQL 不许再「合名」。
#[cfg(test)]
mod attachment_columns_tests {
    use super::ATTACHMENT_COLUMNS;

    #[test]
    fn attachment_columns_expose_original_and_stored_name() {
        assert!(ATTACHMENT_COLUMNS.contains("AS original_name"));
        assert!(ATTACHMENT_COLUMNS.contains("AS file_name"));
        // 反向断言：不允许把两个名字在 SQL 里合起来（那样存储路径会漏进下载文件名）
        assert!(!ATTACHMENT_COLUMNS.contains("f.original_name, f.file_name"));
    }
}
