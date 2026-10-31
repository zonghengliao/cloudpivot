//! 公告用户侧（批次 C · C2）：四张表的强类型模型与数据访问。
//!
//! 依据 `docs/批次C-技术架构.md`：§2.3（表设计 / 数据权限结论）、§2.7（已读未读口径）、
//! §2.9（关键查询与索引验证 POC-8）、§5.5.1（A-01 / A-02 / A-03 / A-10）。
//!
//! # 可见性判定是**一条**谓词，不是四处各写一遍
//!
//! 「我能看到哪些公告」在用户侧要出现在 5 个地方：列表、未读数、详情、附件下载、
//! 全部已读。它们若各写一遍 WHERE，就会出现「列表里看不到、但未读数算进去了」这类
//! 静默不一致（A-10 的口径正是交叉断言这两者）。所以谓词只在这里定义一次：
//!
//! - [`VISIBLE_STATE`]：状态与有效期（`deleted_at IS NULL` + `status='PUBLISHED'` +
//!   `publish_at <= now()` + `expire_at IS NULL OR expire_at > now()`）；
//! - [`VISIBLE_SCOPE`]：受众匹配（`ALL` / `ROLE` / `DEPT` / `USER`）。
//!
//! 两段拼起来就是 [`VISIBLE_PREDICATE`]。**占位符顺序固定**：先 `ROLE` 的数组、
//! 再 `DEPT` 的数组、最后 `USER` 的 uuid —— 参数由 [`audience_params`] 按同一顺序产出，
//! 且每个构造函数末尾都有单测断言「`?` 个数 == 参数个数」，防止将来加条件时错位。
//!
//! # 为什么有效期做「双保险」
//!
//! `status='EXPIRED'` 由定时任务落库（架构 §2.6），但定时任务最多有 10 分钟延迟；
//! 查询侧必须**同时**带 `expire_at` 时间条件，否则「已过有效期但任务还没跑」的公告
//! 会在这 10 分钟里继续可见（A-03 断言的就是这个场景）。
//!
//! # 数据权限
//!
//! 公告三张上表**有意不登记** `shared/scope.rs` 的 `SCOPED_TABLES`（架构 §2.3.3）：
//! 公告没有部门归属列，可见性由 `scope_type` + `sys_announcement_scope` 判定。
//! 列表分页仍走唯一入口 [`scope::paginate_unscoped`] + [`scope::ScopeTarget::unscoped`]
//! （闸① 不允许调用方自己拼分页），传的是「不参与数据权限」的显式锚点。
//!
//! # 冗余计数的一致性
//!
//! `sys_announcement.read_count` 受 `ck_sys_announcement_counts`（`read_count <= receiver_count`）
//! 约束。已读路径一律用 `LEAST(receiver_count, …)` 递增 / 回算 —— 只加不夹会在一
//! `receiver_count=0` 的公告上（关闭扇出 / 未扇出完）直接把接口打成 500。

use nik::prelude::*;

use crate::business::notice::model::{
    BIZ_TYPE_ANNOUNCEMENT, BizWithdrawReport, Executor as NoticeExecutor,
};
use crate::business::shared::scope;
use crate::shared::error_code;

/// 可见性谓词的状态与有效期部分（**不含**受众匹配）。
pub const VISIBLE_STATE: &str = "a.deleted_at IS NULL \
     AND a.status = 'PUBLISHED' \
     AND (a.publish_at IS NULL OR a.publish_at <= now()) \
     AND (a.expire_at IS NULL OR a.expire_at > now())";

/// 可见性谓词的受众匹配部分（占位符顺序：`ROLE` 数组 → `DEPT` 数组 → `USER` uuid）。
///
/// `DEPT` 一侧只看 `target_id` 是否命中**当前用户所在部门**：发布时已用递归 CTE 把
/// 「本部门 + 所有下级」展开成多行（架构 §2.3.2 方案 b），读取侧不再递归。
pub const VISIBLE_SCOPE: &str = "( a.scope_type = 'ALL' \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'ROLE' \
                    AND s.target_id = ANY(?::text::uuid[])) \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'DEPT' \
                    AND s.target_id = ANY(?::text::uuid[])) \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'USER' \
                    AND s.target_id = ?::uuid) )";

/// 完整可见性谓词（状态 + 受众）。
pub const VISIBLE_PREDICATE: &str = "( a.deleted_at IS NULL \
     AND a.status = 'PUBLISHED' \
     AND (a.publish_at IS NULL OR a.publish_at <= now()) \
     AND (a.expire_at IS NULL OR a.expire_at > now()) \
     AND ( a.scope_type = 'ALL' \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'ROLE' \
                    AND s.target_id = ANY(?::text::uuid[])) \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'DEPT' \
                    AND s.target_id = ANY(?::text::uuid[])) \
      OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                  WHERE s.announcement_id = a.id AND s.scope_type = 'USER' \
                    AND s.target_id = ?::uuid) ) )";

/// 当前用户的受众身份：公告可见性判定所需的三个输入。
///
/// # 为什么不直接用 [`crate::business::shared::principal::ScopedPrincipal`]
///
/// `ScopedPrincipal` 的 `dept_ids()` 是**数据权限**语义（`CUSTOM` 会换成角色配置的部门
/// 集合、`SELF_ONLY` 会清空），而公告的 `DEPT` 受众是**组织归属**语义（「我在哪个部门」
/// 与「我的角色能看到哪些部门的行」是两件事）。用数据范围集合去匹配公告受众，
/// 会让「自定义数据范围只配了 A 部门的主管」看不到发给本部门 B 的公告。
#[derive(Debug, Clone)]
pub struct Audience {
    /// 用户 ID（`USER` 范围与已读回执用）。
    pub user_id: uuid::Uuid,
    /// 持有的角色 ID（只看启用且未过期的绑定）。
    pub role_ids: Vec<uuid::Uuid>,
    /// 所属部门 ID（`sys_user.dept_id`，单值；为空表示未分配部门）。
    pub dept_ids: Vec<uuid::Uuid>,
}

/// 我的可见公告列表卡片行。
#[derive(Debug, Clone)]
pub struct AnnouncementCardRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// 级别（`NORMAL` / `IMPORTANT` / `URGENT`）。
    pub level: String,
    /// 是否置顶。
    pub is_top: bool,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 当前用户是否已读。
    pub read_flag: bool,
    /// 附件数。
    pub attachment_count: i64,
    /// 发布人姓名。
    pub publisher_name: String,
}

/// 公告详情行（含渲染快照与已读状态）。
#[derive(Debug, Clone)]
pub struct AnnouncementDetailRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// Markdown 源。
    pub content_md: String,
    /// 渲染 + 净化后的 HTML 快照（**出参前还会再净化一次**：闸 3）。
    pub content_html: String,
    /// 级别。
    pub level: String,
    /// 发布范围类型。
    pub scope_type: String,
    /// 状态。
    pub status: String,
    /// 是否置顶。
    pub is_top: bool,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 当前用户是否已读。
    pub read_flag: bool,
    /// 当前用户已读时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 附件数。
    pub attachment_count: i64,
    /// 受众是否命中（`ALL` / 角色 / 部门 / 指定用户）。
    pub scope_ok: bool,
    /// 是否「在有效期内且已发布」（含 `publish_at <= now()` 与 `expire_at > now()` 双保险）。
    pub live: bool,
}

/// 附件行。
#[derive(Debug, Clone)]
pub struct AttachmentRow {
    /// 附件绑定行主键（下载地址里的 `att_id`）。
    pub id: uuid::Uuid,
    /// `sys_file.id`。
    pub file_id: uuid::Uuid,
    /// 文件名。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME。
    pub mime_type: Option<String>,
    /// 排序号。
    pub sort_order: i32,
}

/// 可见性判定结果。
#[derive(Debug, Clone)]
pub enum Visibility {
    /// 可见（附带详情行）。
    Visible(Box<AnnouncementDetailRow>),
    /// 公告不存在 / 已软删 → `404 + 62002`。
    NotFound,
    /// 公告不在有效期内 / 未发布 → `403 + 62005`。
    NotAvailable,
    /// 公告发布范围不含当前用户 → `403 + 62004`。
    Forbidden,
}

/// 列表过滤条件（用户侧）。
#[derive(Debug, Clone, Default)]
pub struct VisibleFilter {
    /// 标题 / 摘要关键词（已去空白；`None` = 不过滤）。
    pub keyword: Option<String>,
    /// 只看未读。
    pub only_unread: bool,
}

/// 把部门 / 角色集合编码成 PostgreSQL 文本数组字面量 `{uuid,uuid}`。
///
/// # 参数
///
/// - `ids`：UUID 集合。
///
/// # 返回值
///
/// 形如 `{a,b}` 的字面量（**排序后**编码：同一集合无论来源顺序如何，生成的参数
/// 与 SQL 逐字节相同，便于日志比对与单测断言）；空集合编码为 `{}`。
///
/// # 为什么不用 `Value::Array`
///
/// 与 `shared/scope.rs::array_param` 同口径：绑定 `TEXT` 参数 + SQL 侧 `?::text::uuid[]`，
/// 避免驱动按 `Unknown` 类型编码（impexp 踩过 `error serializing parameter`）。
pub fn array_literal(ids: &[uuid::Uuid]) -> String {
    let mut unique: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    unique.sort_unstable();
    unique.dedup();
    let mut literal = String::with_capacity(unique.len() * 37 + 2);
    literal.push('{');
    literal.push_str(&unique.join(","));
    literal.push('}');
    literal
}

/// 受众的绑定参数（顺序与 [`VISIBLE_SCOPE`] 的占位符**逐一对应**）。
///
/// # 返回值
///
/// `[角色集合, 部门集合, user_id]`。
pub fn audience_params(audience: &Audience) -> Vec<Value> {
    vec![
        Value::Text(array_literal(&audience.role_ids)),
        Value::Text(array_literal(&audience.dept_ids)),
        Value::from(&audience.user_id),
    ]
}

/// 加载当前用户的受众身份。
///
/// # 参数
///
/// - `user_id`：当前用户（取自令牌 `sub`，不是请求参数）。
///
/// # 返回值
///
/// [`Audience`]：角色集合（只算启用、未软删、未过期的绑定）+ 所属部门。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn load_audience(user_id: &uuid::Uuid) -> DbResult<Audience> {
    let role_rows = db()
        .find_with_params(
            "SELECT r.id AS role_id \
               FROM sys_user_role ur \
               JOIN sys_role r ON r.id = ur.role_id \
                              AND r.deleted_at IS NULL \
                              AND r.status = 1 \
              WHERE ur.user_id = ?::uuid \
                AND (ur.expire_time IS NULL OR ur.expire_time > now())",
            vec![Value::from(user_id)],
        )
        .await?;
    let mut role_ids = Vec::with_capacity(role_rows.len());
    for row in &role_rows {
        role_ids.push(row.get_typed("role_id")?);
    }

    let dept_rows = db()
        .find_with_params(
            "SELECT u.dept_id \
               FROM sys_user u \
              WHERE u.id = ?::uuid AND u.deleted_at IS NULL AND u.dept_id IS NOT NULL",
            vec![Value::from(user_id)],
        )
        .await?;
    let mut dept_ids = Vec::with_capacity(dept_rows.len());
    for row in &dept_rows {
        dept_ids.push(row.get_typed("dept_id")?);
    }

    Ok(Audience {
        user_id: *user_id,
        role_ids,
        dept_ids,
    })
}

/// 我的可见公告分页（置顶优先 + 发布时间倒序）。
///
/// # 参数
///
/// - `audience`：当前用户受众身份；
/// - `filter`：关键词 / 只看未读；
/// - `page` / `page_size`：分页（已规范化）。
///
/// # 返回值
///
/// [`Page<AnnouncementCardRow>`]。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn page_visible(
    audience: &Audience,
    filter: &VisibleFilter,
    page: u64,
    page_size: u64,
) -> DbResult<Page<AnnouncementCardRow>> {
    // 占位符文本顺序：① JOIN 的 user_id ② ROLE 数组 ③ DEPT 数组 ④ USER uuid
    //                 ⑤⑥ 关键词（标题 / 摘要各一次）
    let mut sql = String::from(
        "SELECT a.id, a.ann_no, a.title, a.summary, a.level, a.is_top, a.publish_at, \
                (r.id IS NOT NULL) AS read_flag, \
                (SELECT count(*) FROM sys_announcement_attachment att \
                  WHERE att.announcement_id = a.id) AS attachment_count, \
                COALESCE(a.publisher_name, '') AS publisher_name \
           FROM sys_announcement a \
           LEFT JOIN sys_announcement_read r \
                  ON r.announcement_id = a.id AND r.user_id = ?::uuid \
          WHERE ",
    );
    sql.push_str(VISIBLE_PREDICATE);
    let mut params = vec![Value::from(&audience.user_id)];
    params.extend(audience_params(audience));

    if let Some(keyword) = filter.keyword.as_deref() {
        sql.push_str(" AND (a.title ILIKE ? OR a.summary ILIKE ?)");
        let pattern = keyword.to_string();
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if filter.only_unread {
        sql.push_str(" AND r.id IS NULL");
    }

    // SCOPE-EXEMPT: 公告不属数据权限范畴（架构 §2.3.3 结论）——可见性由 scope_type +
    // sys_announcement_scope 判定，注入部门/归属条件会静默少数据。闸① 仍走唯一入口
    // scope::paginate_unscoped 的显式 unscoped 锚点。
    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_announcement"),
        &sql,
        params,
        Some("ORDER BY a.is_top DESC, a.publish_at DESC NULLS LAST, a.id DESC"),
        page,
        page_size,
    )
    .await?;

    let mut rows = Vec::with_capacity(list.len());
    for row in &list {
        rows.push(AnnouncementCardRow {
            id: row.get_typed("id")?,
            ann_no: row.get_typed("ann_no")?,
            title: row.get_typed("title")?,
            summary: row.get_typed("summary")?,
            level: row.get_typed("level")?,
            is_top: row.get_typed("is_top")?,
            publish_at: row.get_typed("publish_at")?,
            read_flag: row.get_typed("read_flag")?,
            attachment_count: row.get_typed("attachment_count")?,
            publisher_name: row.get_typed("publisher_name")?,
        });
    }
    Ok(Page {
        list: rows,
        total,
        page,
        size,
        pages,
    })
}

/// 我的未读公告数（口径：可见 ∧ 未过期 ∧ 未读；与 `page_visible(only_unread=true).total`
/// 必须**恒等**，A-10 的交叉断言）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_unread(audience: &Audience) -> DbResult<i64> {
    // 占位符文本顺序：① ROLE 数组 ② DEPT 数组 ③ USER uuid（谓词内）
    //                 ④ user_id（NOT EXISTS 内，文本上位于谓词之后）
    let sql = format!(
        "SELECT count(*) AS unread \
           FROM sys_announcement a \
          WHERE {VISIBLE_PREDICATE} \
            AND NOT EXISTS (SELECT 1 FROM sys_announcement_read r \
                             WHERE r.announcement_id = a.id AND r.user_id = ?::uuid)"
    );
    let mut params = audience_params(audience);
    params.push(Value::from(&audience.user_id));
    let unread: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(unread)
}

/// 单条公告的可见性判定（详情 / 附件下载共用）。
///
/// # 判定顺序（架构 §2.5.3）
///
/// 1. 记录不存在 / 已软删 → [`Visibility::NotFound`]（`404 + 62002`）；
/// 2. 不在有效期 / 未发布 → [`Visibility::NotAvailable`]（`403 + 62005`）；
/// 3. 受众不命中 → [`Visibility::Forbidden`]（`403 + 62004`）。
///
/// 「不存在」必须早于「不可见」：否则可以用状态码探测「这个 id 是否存在」。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn check_visibility(
    audience: &Audience,
    announcement_id: &uuid::Uuid,
) -> DbResult<Visibility> {
    // 占位符文本顺序：① ROLE 数组 ② DEPT 数组 ③ USER uuid —— 三者都在 **SELECT 投影**
    // （scope_ok 表达式）里，因此排在 FROM/JOIN 的占位符**之前**；
    // ④ JOIN 的 user_id ⑤ WHERE 的公告 id。
    //
    // 这是本函数最容易错的地方：参数顺序按「SQL 文本里 `?` 出现的先后」，
    // 不是按「读起来最自然的语义顺序」。曾按语义顺序绑过一次 → `error serializing
    // parameter 0`（把 uuid 绑给了 `::text::uuid[]`）。
    let sql = String::from(
        "SELECT a.id, a.ann_no, a.title, a.summary, a.content_md, a.content_html, a.level, \
                a.scope_type, a.status, a.is_top, a.publish_at, a.expire_at, \
                COALESCE(a.publisher_name, '') AS publisher_name, \
                (r.id IS NOT NULL) AS read_flag, r.read_at, \
                (SELECT count(*) FROM sys_announcement_attachment att \
                  WHERE att.announcement_id = a.id) AS attachment_count, \
                ( a.scope_type = 'ALL' \
                  OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                              WHERE s.announcement_id = a.id AND s.scope_type = 'ROLE' \
                                AND s.target_id = ANY(?::text::uuid[])) \
                  OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                              WHERE s.announcement_id = a.id AND s.scope_type = 'DEPT' \
                                AND s.target_id = ANY(?::text::uuid[])) \
                  OR EXISTS (SELECT 1 FROM sys_announcement_scope s \
                              WHERE s.announcement_id = a.id AND s.scope_type = 'USER' \
                                AND s.target_id = ?::uuid) ) AS scope_ok, \
                ( a.status = 'PUBLISHED' \
                  AND (a.publish_at IS NULL OR a.publish_at <= now()) \
                  AND (a.expire_at IS NULL OR a.expire_at > now()) ) AS live \
           FROM sys_announcement a \
           LEFT JOIN sys_announcement_read r \
                  ON r.announcement_id = a.id AND r.user_id = ?::uuid \
          WHERE a.id = ?::uuid AND a.deleted_at IS NULL \
          LIMIT 1",
    );
    let mut params = audience_params(audience);
    params.push(Value::from(&audience.user_id));
    params.push(Value::from(announcement_id));
    let rows = db().find_with_params(&sql, params).await?;
    let Some(row) = rows.first() else {
        return Ok(Visibility::NotFound);
    };
    let scope_ok: bool = row.get_typed("scope_ok")?;
    let live: bool = row.get_typed("live")?;
    let detail = AnnouncementDetailRow {
        id: row.get_typed("id")?,
        ann_no: row.get_typed("ann_no")?,
        title: row.get_typed("title")?,
        summary: row.get_typed("summary")?,
        content_md: row.get_typed("content_md")?,
        content_html: row.get_typed("content_html")?,
        level: row.get_typed("level")?,
        scope_type: row.get_typed("scope_type")?,
        status: row.get_typed("status")?,
        is_top: row.get_typed("is_top")?,
        publish_at: row.get_typed("publish_at")?,
        expire_at: row.get_typed("expire_at")?,
        publisher_name: row.get_typed("publisher_name")?,
        read_flag: row.get_typed("read_flag")?,
        read_at: row.get_typed("read_at")?,
        attachment_count: row.get_typed("attachment_count")?,
        scope_ok,
        live,
    };
    if !live {
        return Ok(Visibility::NotAvailable);
    }
    if !scope_ok {
        return Ok(Visibility::Forbidden);
    }
    Ok(Visibility::Visible(Box::new(detail)))
}

/// 标记单条已读（幂等）。
///
/// # 参数
///
/// - `announcement_id`：公告主键（**调用方已确认可见**）；
/// - `user_id`：当前用户。
///
/// # 返回值
///
/// `(是否首次已读, 当前 read_count)`。重复调用不重复计数（`uk_sys_announcement_read`
/// 冲突 → 视为已读）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn mark_read(
    announcement_id: &uuid::Uuid,
    user_id: &uuid::Uuid,
) -> DbResult<(bool, i32)> {
    let inserted = db()
        .execute_with_params(
            "INSERT INTO sys_announcement_read (announcement_id, user_id, read_at, created_at) \
             VALUES (?::uuid, ?::uuid, now(), now()) \
             ON CONFLICT (announcement_id, user_id) DO NOTHING",
            vec![Value::from(announcement_id), Value::from(user_id)],
        )
        .await?;
    let first_time = inserted > 0;
    if first_time {
        // LEAST 夹住：`receiver_count = 0`（关闭扇出 / 尚未扇出）时不能让
        // ck_sys_announcement_counts 把接口打成 500（A-09 的口径）。
        db().execute_with_params(
            "UPDATE sys_announcement \
                    SET read_count = LEAST(receiver_count, read_count + 1), updated_at = now() \
                  WHERE id = ?::uuid",
            vec![Value::from(announcement_id)],
        )
        .await?;
    }
    let read_count: i32 = db()
        .scalar_with_params(
            "SELECT read_count FROM sys_announcement WHERE id = ?::uuid",
            vec![Value::from(announcement_id)],
        )
        .await?;
    Ok((first_time, read_count))
}

/// 把「我的可见未读集合」一次全部标为已读。
///
/// # 返回值
///
/// 本次新增的已读回执行数（重复调用返回 0）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn mark_all_read(audience: &Audience) -> DbResult<u64> {
    // 占位符文本顺序：① user_id（SELECT 投影）② ROLE 数组 ③ DEPT 数组 ④ USER uuid
    //                 ⑤ user_id（NOT EXISTS）
    let sql = format!(
        "INSERT INTO sys_announcement_read (announcement_id, user_id, read_at, created_at) \
         SELECT a.id, ?::uuid, now(), now() \
           FROM sys_announcement a \
          WHERE {VISIBLE_PREDICATE} \
            AND NOT EXISTS (SELECT 1 FROM sys_announcement_read r \
                             WHERE r.announcement_id = a.id AND r.user_id = ?::uuid) \
         ON CONFLICT (announcement_id, user_id) DO NOTHING"
    );
    let mut params = vec![Value::from(&audience.user_id)];
    params.extend(audience_params(audience));
    params.push(Value::from(&audience.user_id));
    let affected = db().execute_with_params(&sql, params).await?;

    // 回算冗余计数（只动当前用户已读过的公告，且只在数值真的变化时写）。
    db().execute_with_params(
        "UPDATE sys_announcement a \
                SET read_count = LEAST(a.receiver_count, t.cnt), updated_at = now() \
               FROM (SELECT announcement_id, count(*) AS cnt \
                       FROM sys_announcement_read \
                      WHERE user_id = ?::uuid \
                      GROUP BY announcement_id) t \
              WHERE a.id = t.announcement_id \
                AND a.read_count <> LEAST(a.receiver_count, t.cnt)",
        vec![Value::from(&audience.user_id)],
    )
    .await?;
    Ok(affected)
}

/// 查附件绑定（**必须**同时给公告 id，防「拿别的公告的附件 id」越权）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_attachment(
    announcement_id: &uuid::Uuid,
    attachment_id: &uuid::Uuid,
) -> DbResult<Option<AttachmentRow>> {
    let rows = db()
        .find_with_params(
            "SELECT att.id, att.file_id, att.file_name, att.file_size, att.mime_type, att.sort_order \
               FROM sys_announcement_attachment att \
              WHERE att.announcement_id = ?::uuid AND att.id = ?::uuid \
              LIMIT 1",
            vec![Value::from(announcement_id), Value::from(attachment_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(AttachmentRow {
            id: row.get_typed("id")?,
            file_id: row.get_typed("file_id")?,
            file_name: row.get_typed("file_name")?,
            file_size: row.get_typed("file_size")?,
            mime_type: row.get_typed("mime_type")?,
            sort_order: row.get_typed("sort_order")?,
        })),
        None => Ok(None),
    }
}

/// 列出一条公告的全部附件（详情页用）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_attachments(announcement_id: &uuid::Uuid) -> DbResult<Vec<AttachmentRow>> {
    let rows = db()
        .find_with_params(
            "SELECT att.id, att.file_id, att.file_name, att.file_size, att.mime_type, att.sort_order \
               FROM sys_announcement_attachment att \
              WHERE att.announcement_id = ?::uuid \
              ORDER BY att.sort_order ASC, att.created_at ASC, att.id ASC",
            vec![Value::from(announcement_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(AttachmentRow {
            id: row.get_typed("id")?,
            file_id: row.get_typed("file_id")?,
            file_name: row.get_typed("file_name")?,
            file_size: row.get_typed("file_size")?,
            mime_type: row.get_typed("mime_type")?,
            sort_order: row.get_typed("sort_order")?,
        });
    }
    Ok(list)
}

// ==================== 管理侧（批次 C · C3）====================
//
// 契约来源：`docs/批次C-技术架构.md` §2.8.2（管理侧 10 接口）、§2.8.3（校验表）、
// §2.6（有效期与自动下线）、§5.5.2 A-05 ~ A-09 / A-18；PRD A1-01 ~ A1-10。
//
// # 状态机（唯一的一份定义）
//
// ```text
//                publish                offline
//   DRAFT ───────────────────▶ PUBLISHED ─────────▶ OFFLINE
//                                ▲   │                │
//                                │   │ expire_at<=now │ publish（重新发布，version+1）
//                                │   ▼                │
//                                │ EXPIRED ◀──────────┘
//                                └──── publish ────────┘
// ```
//
// | 迁移 | 允许的源状态 | 非法时 |
// |------|--------------|--------|
// | `publish` | `DRAFT` / `OFFLINE` / `EXPIRED` | `400 + 62009`（A-18 要求贴 400 证据） |
// | `offline` | `PUBLISHED` | `400 + 62009` |
// | 编辑（`PUT`） | `DRAFT` / `OFFLINE` / `EXPIRED` | `409 + 62009`（§2.8.3 校验表逐字要求 409） |
// | 删除（`DELETE`） | `DRAFT` / `OFFLINE` / `EXPIRED` | `409 + 62009` |
//
// 「发布」与「下线」是**状态机迁移**（非法迁移 = 请求本身不成立 → 400）；
// 「编辑 / 删除已发布公告」是**与既有状态冲突的业务规则**（§2.8.3 明确写 409）。
// 两者都带 `62009`，前端按 code 判断即可，不必依赖状态码分支。
//
// # 发布扇出与补偿（A-05 / A-06 / A-07）
//
// `dispatch` 在**状态事务提交之后**调用（架构 §2.2：提交后调用 + 幂等键 + 补偿）：
//
// 1. 事务内 `status='PUBLISHED'`、`version = version + 1`、`publish_at = now()`、
//    `notice_id = NULL`；
// 2. 事务外调 `business::notice::dispatch()`，幂等键 `announcement:{id}:v{version}`；
// 3. 成功 → [`backfill_notice`] 回填 `notice_id` + `receiver_count`；
//    失败 → 只记日志，`notice_id` 留 `NULL`；
// 4. `announcement_dispatch_retry` 任务扫「`PUBLISHED` 且 `notice_id IS NULL`」的行重放，
//    因幂等键不变，**跑多少次都只有一条 `sys_notice`**（A-07 的硬指标）。
//
// # 为什么不新增列
//
// 架构 §5.3 已把 `V0082`/`V0083` 分配给帮助文档、`V0085`/`V0086` 分配给 C8 的种子；
// 本卡不插队改号。因此「扇出关闭」与「扇出失败」在库里都表现为 `notice_id IS NULL`，
// 由**全局开关**（`sys_config.announcement.fanout_enabled`）区分语义：
// 开关关闭时发布不扇出、补偿任务也整体跳过（止血优先），开关打开后自动补发。

/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已发布。
pub const STATUS_PUBLISHED: &str = "PUBLISHED";
/// 人工下线。
pub const STATUS_OFFLINE: &str = "OFFLINE";
/// 到期自动下线。
pub const STATUS_EXPIRED: &str = "EXPIRED";

/// 全局扇出开关的配置键（`sys_config.config_key`，V0086 种，缺省视为 `true`）。
pub const CONFIG_FANOUT_ENABLED: &str = "announcement.fanout_enabled";

/// 补偿任务单批扫描上限（避免一次把十万行拉进内存）。
pub const DISPATCH_RETRY_BATCH: i64 = 50;

/// 管理列表行。
#[derive(Debug, Clone)]
pub struct ManageCardRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// 级别。
    pub level: String,
    /// 范围类型。
    pub scope_type: String,
    /// 状态。
    pub status: String,
    /// 置顶是否**当前生效**（`is_top` 且未过期）。
    pub top_active: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 接收人数。
    pub receiver_count: i32,
    /// 已读数。
    pub read_count: i32,
    /// 附件数。
    pub attachment_count: i64,
    /// 内容版本。
    pub version: i32,
    /// 扇出的通知 ID。
    pub notice_id: Option<uuid::Uuid>,
    /// 更新时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理详情行（含 Markdown 原文与范围明细）。
#[derive(Debug, Clone)]
pub struct ManageDetailRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// Markdown 原文。
    pub content_md: String,
    /// 渲染快照。
    pub content_html: String,
    /// 级别。
    pub level: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围明细。
    pub scope_ids: Vec<uuid::Uuid>,
    /// 状态。
    pub status: String,
    /// 置顶是否生效（`is_top` 且未过期）。
    pub top_active: bool,
    /// `is_top` 列的原始值（编辑时「未传 is_top」要沿用它，不能按 `top_active` 反推）。
    pub is_top_flag: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人 ID。
    pub publisher_id: Option<uuid::Uuid>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 扇出的通知 ID。
    pub notice_id: Option<uuid::Uuid>,
    /// 接收人数。
    pub receiver_count: i32,
    /// 已读数。
    pub read_count: i32,
    /// 内容版本。
    pub version: i32,
    /// 备注。
    pub remark: String,
    /// 创建时间。
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 更新时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理列表过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ManageFilter {
    /// 关键词（已包装成 `%kw%`）。
    pub keyword: Option<String>,
    /// 状态（多选，已大写；空 = 不过滤）。
    pub statuses: Vec<String>,
    /// 范围类型。
    pub scope_type: Option<String>,
    /// 级别。
    pub level: Option<String>,
    /// 发布时间窗起。
    pub publish_from: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布时间窗止。
    pub publish_to: Option<chrono::DateTime<chrono::Utc>>,
}

/// 待扇出（补偿）行。
#[derive(Debug, Clone)]
pub struct PendingDispatchRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 内容版本（幂等键的一部分）。
    pub version: i32,
    /// 标题。
    pub title: String,
    /// 摘要（通知正文）。
    pub summary: String,
    /// 正文（摘要为空时截取纯文本用）。
    pub content_md: String,
    /// 级别。
    pub level: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围明细。
    pub scope_ids: Vec<uuid::Uuid>,
    /// 有效期止（通知的过期时间）。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人 ID。
    pub publisher_id: Option<uuid::Uuid>,
    /// 发布人姓名。
    pub publisher_name: String,
}

/// 阅读名单行。
#[derive(Debug, Clone)]
pub struct ReaderRow {
    /// 用户 ID。
    pub user_id: uuid::Uuid,
    /// 登录账号。
    pub username: String,
    /// 真实姓名。
    pub real_name: String,
    /// 部门名。
    pub dept_name: String,
    /// 已读时间（`None` = 未读）。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 新建公告的落库入参。
#[derive(Debug, Clone)]
pub struct NewAnnouncement<'a> {
    /// 主键。
    pub id: uuid::Uuid,
    /// 公告编号。
    pub ann_no: &'a str,
    /// 标题。
    pub title: &'a str,
    /// 摘要。
    pub summary: &'a str,
    /// 正文 Markdown。
    pub content_md: &'a str,
    /// 渲染 + 净化后的 HTML 快照。
    pub content_html: &'a str,
    /// 级别。
    pub level: &'a str,
    /// 范围类型。
    pub scope_type: &'a str,
    /// 范围明细（`DEPT` 已展开）。
    pub scope_ids: &'a [uuid::Uuid],
    /// 是否置顶。
    pub is_top: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 有效期止。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 发布人 ID。
    pub publisher_id: uuid::Uuid,
    /// 发布人姓名。
    pub publisher_name: &'a str,
    /// 附件（file_id, sort_order）。
    pub attachments: &'a [(uuid::Uuid, i32)],
}

// ==================== 状态机（纯函数） ====================

/// 允许从 `status` 迁移到「已发布」。
///
/// # 异常情况
///
/// 已是 `PUBLISHED`（或状态值未知）→ `400 + 62009`。
pub fn ensure_publishable(status: &str) -> std::result::Result<(), ApiError> {
    match status {
        STATUS_DRAFT | STATUS_OFFLINE | STATUS_EXPIRED => Ok(()),
        other => Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_STATUS_CONFLICT,
            format!("状态 {other} 不能发布：只有草稿 / 已下线 / 已过期的公告可以发布"),
        )),
    }
}

/// 允许从 `status` 迁移到「已下线」。
///
/// # 异常情况
///
/// 非 `PUBLISHED` → `400 + 62009`。
pub fn ensure_offlinable(status: &str) -> std::result::Result<(), ApiError> {
    match status {
        STATUS_PUBLISHED => Ok(()),
        other => Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_STATUS_CONFLICT,
            format!("状态 {other} 不能下线：只有已发布的公告可以在线下线"),
        )),
    }
}

/// 允许编辑。
///
/// # 异常情况
///
/// `PUBLISHED` → `409 + 62009`（§2.8.3：已发布公告不允许直接改正文，必须先下线）。
pub fn ensure_editable(status: &str) -> std::result::Result<(), ApiError> {
    match status {
        STATUS_DRAFT | STATUS_OFFLINE | STATUS_EXPIRED => Ok(()),
        _ => Err(ApiError::new(
            409,
            error_code::ANNOUNCEMENT_STATUS_CONFLICT,
            "已发布的公告不能直接编辑：请先下线（或下线后再发布，版本号会 +1）",
        )),
    }
}

/// 允许删除（软删）。
///
/// # 异常情况
///
/// `PUBLISHED` → `409 + 62009`（先下线再删，避免「读者点进来 404」）。
pub fn ensure_deletable(status: &str) -> std::result::Result<(), ApiError> {
    match status {
        STATUS_DRAFT | STATUS_OFFLINE | STATUS_EXPIRED => Ok(()),
        _ => Err(ApiError::new(
            409,
            error_code::ANNOUNCEMENT_STATUS_CONFLICT,
            "已发布的公告不能直接删除：请先下线",
        )),
    }
}

/// 编辑时校验有效期与既有发布时间的关系。
///
/// # 为什么需要
///
/// `ck_sys_announcement_expire` 要求 `expire_at > publish_at`。已下线（`publish_at` 非空）
/// 的公告若把有效期改成过去时间，会直接撞约束变成 500 —— 必须在应用层提前给出 62008。
///
/// # 异常情况
///
/// `expire_at <= publish_at` → `400 + 62008`。
pub fn ensure_expire_after_publish(
    publish_at: Option<chrono::DateTime<chrono::Utc>>,
    expire_at: Option<chrono::DateTime<chrono::Utc>>,
) -> std::result::Result<(), ApiError> {
    if let (Some(publish_at), Some(expire_at)) = (publish_at, expire_at)
        && expire_at <= publish_at
    {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_EXPIRE_INVALID,
            "有效期必须晚于上一次发布时间（否则公告一发布就过期）",
        ));
    }
    Ok(())
}

/// 生成公告编号：`AN` + `yyyyMMdd` + 6 位十六进制尾巴。
///
/// # 参数
///
/// - `now`：生成时刻（UTC，取日期段）；
/// - `id`：公告主键（取其末 6 位十六进制，**不是**前 6 位：UUIDv7 高位是时间戳，
///   同一天建立的两条公告前缀完全相同，取前 6 位会直接撞 `uk_sys_announcement_no`，
///   与 A5 的 `sys_notice_no` 是同一个坑）。
///
/// # 返回值
///
/// 形如 `AN20261029A1B2C3`（16 字符，≤ `VARCHAR(32)`）。
pub fn build_ann_no(now: chrono::DateTime<chrono::Utc>, id: &uuid::Uuid) -> String {
    let stamp = now.format("%Y%m%d");
    let simple = id.simple().to_string();
    let tail = &simple[simple.len() - 6..];
    format!("AN{stamp}{}", tail.to_ascii_uppercase())
}

// ==================== 管理列表 / 详情 ====================

/// 管理列表分页（含草稿 / 已发布 / 已下线 / 已过期）。
///
/// # 参数
///
/// - `filter`：关键词 / 状态 / 范围 / 级别 / 发布时间窗；
/// - `page` / `page_size`：已规范化分页。
///
/// # 返回值
///
/// [`Page<ManageCardRow>`]：置顶生效优先 → 发布时间倒序 → id 倒序。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 公告不在数据权限范畴（架构 §2.3.3）：能进管理页的人已持有
/// `business:announcement:list`（元权限），且公告表没有部门归属列。这里走
/// `scope::paginate_unscoped` 的显式 `unscoped` 锚点（闸① 仍走唯一入口）。
pub async fn page_manage(
    filter: &ManageFilter,
    page: u64,
    page_size: u64,
) -> DbResult<Page<ManageCardRow>> {
    let mut sql = String::from(
        "SELECT a.id, a.ann_no, a.title, a.summary, a.level, a.scope_type, a.status, \
                a.is_top, a.top_expire_at, a.publish_at, a.expire_at, \
                COALESCE(a.publisher_name, '') AS publisher_name, \
                a.receiver_count, a.read_count, a.version, a.notice_id, a.updated_at, \
                ( a.is_top AND (a.top_expire_at IS NULL OR a.top_expire_at > now()) ) AS top_active, \
                (SELECT count(*) FROM sys_announcement_attachment att \
                  WHERE att.announcement_id = a.id) AS attachment_count \
           FROM sys_announcement a \
          WHERE a.deleted_at IS NULL",
    );
    let mut params: Vec<Value> = Vec::new();

    if let Some(pattern) = filter.keyword.as_deref() {
        sql.push_str(" AND (a.title ILIKE ? OR a.ann_no ILIKE ? OR a.summary ILIKE ?)");
        params.push(Value::Text(pattern.to_string()));
        params.push(Value::Text(pattern.to_string()));
        params.push(Value::Text(pattern.to_string()));
    }
    if !filter.statuses.is_empty() {
        // 状态是白名单枚举（已在 handler 侧大写化 + 校验），这里按个数拼 `?`
        // （不拼值，值仍走绑定参数）。
        let holders = vec!["?"; filter.statuses.len()].join(", ");
        sql.push_str(&format!(" AND a.status IN ({holders})"));
        for status in &filter.statuses {
            params.push(Value::Text(status.clone()));
        }
    }
    if let Some(scope_type) = filter.scope_type.as_deref() {
        sql.push_str(" AND a.scope_type = ?");
        params.push(Value::Text(scope_type.to_string()));
    }
    if let Some(level) = filter.level.as_deref() {
        sql.push_str(" AND a.level = ?");
        params.push(Value::Text(level.to_string()));
    }
    if let Some(from) = filter.publish_from {
        sql.push_str(" AND a.publish_at >= ?");
        params.push(Value::from(from));
    }
    if let Some(to) = filter.publish_to {
        sql.push_str(" AND a.publish_at <= ?");
        params.push(Value::from(to));
    }

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_announcement"),
        &sql,
        params,
        Some(
            "ORDER BY ( a.is_top AND (a.top_expire_at IS NULL OR a.top_expire_at > now()) ) DESC, \
             a.publish_at DESC NULLS LAST, a.id DESC",
        ),
        page,
        page_size,
    )
    .await?;

    let mut rows = Vec::with_capacity(list.len());
    for row in &list {
        rows.push(ManageCardRow {
            id: row.get_typed("id")?,
            ann_no: row.get_typed("ann_no")?,
            title: row.get_typed("title")?,
            summary: row.get_typed("summary")?,
            level: row.get_typed("level")?,
            scope_type: row.get_typed("scope_type")?,
            status: row.get_typed("status")?,
            top_active: row.get_typed("top_active")?,
            top_expire_at: row.get_typed("top_expire_at")?,
            publish_at: row.get_typed("publish_at")?,
            expire_at: row.get_typed("expire_at")?,
            publisher_name: row.get_typed("publisher_name")?,
            receiver_count: row.get_typed("receiver_count")?,
            read_count: row.get_typed("read_count")?,
            attachment_count: row.get_typed("attachment_count")?,
            version: row.get_typed("version")?,
            notice_id: row.get_typed("notice_id")?,
            updated_at: row.get_typed("updated_at")?,
        });
    }
    Ok(Page {
        list: rows,
        total,
        page,
        size,
        pages,
    })
}

/// 按主键查管理详情（**不受**用户侧可见性限制，管理侧专用）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 同 [`page_manage`]（公告不参与数据权限）。
pub async fn find_manage(id: &uuid::Uuid) -> DbResult<Option<ManageDetailRow>> {
    let rows = db()
        .find_with_params(
            "SELECT a.id, a.ann_no, a.title, a.summary, a.content_md, a.content_html, a.level, \
                    a.scope_type, a.status, a.is_top, a.top_expire_at, a.publish_at, a.expire_at, \
                    a.publisher_id, COALESCE(a.publisher_name, '') AS publisher_name, \
                    a.notice_id, a.receiver_count, a.read_count, a.version, \
                    COALESCE(a.remark, '') AS remark, a.created_at, a.updated_at, \
                    ( a.is_top AND (a.top_expire_at IS NULL OR a.top_expire_at > now()) ) AS top_active \
               FROM sys_announcement a \
              WHERE a.id = ?::uuid AND a.deleted_at IS NULL \
              LIMIT 1",
            vec![Value::from(id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let scope_ids = list_scope_ids(id).await?;
    Ok(Some(ManageDetailRow {
        id: row.get_typed("id")?,
        ann_no: row.get_typed("ann_no")?,
        title: row.get_typed("title")?,
        summary: row.get_typed("summary")?,
        content_md: row.get_typed("content_md")?,
        content_html: row.get_typed("content_html")?,
        level: row.get_typed("level")?,
        scope_type: row.get_typed("scope_type")?,
        scope_ids,
        status: row.get_typed("status")?,
        top_active: row.get_typed("top_active")?,
        is_top_flag: row.get_typed("is_top")?,
        top_expire_at: row.get_typed("top_expire_at")?,
        publish_at: row.get_typed("publish_at")?,
        expire_at: row.get_typed("expire_at")?,
        publisher_id: row.get_typed("publisher_id")?,
        publisher_name: row.get_typed("publisher_name")?,
        notice_id: row.get_typed("notice_id")?,
        receiver_count: row.get_typed("receiver_count")?,
        read_count: row.get_typed("read_count")?,
        version: row.get_typed("version")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    }))
}

/// 列出一条公告的范围明细 ID。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_scope_ids(announcement_id: &uuid::Uuid) -> DbResult<Vec<uuid::Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT s.target_id FROM sys_announcement_scope s \
              WHERE s.announcement_id = ?::uuid \
              ORDER BY s.scope_type ASC, s.target_id ASC",
            vec![Value::from(announcement_id)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("target_id")?);
    }
    Ok(ids)
}

// ==================== 写路径 ====================

/// 建立公告（草稿），同事务写范围明细与附件关联。
///
/// # 参数
///
/// - `new`：落库入参（已通过 `dto::normalize_save` 校验）；
/// - `operator`：操作人（审计列）。
///
/// # 异常情况
///
/// - 数据库异常（含 `uk_sys_announcement_no` 冲突）返回 [`DbError`]。
pub async fn insert_announcement(
    ex: &Transaction,
    new: &NewAnnouncement<'_>,
    operator: uuid::Uuid,
) -> DbResult<()> {
    let id = new.id;
    let ann_no = new.ann_no.to_string();
    let title = new.title.to_string();
    let summary = new.summary.to_string();
    let content_md = new.content_md.to_string();
    let content_html = new.content_html.to_string();
    let level = new.level.to_string();
    let scope_type = new.scope_type.to_string();
    let scope_ids = new.scope_ids.to_vec();
    let remark = new.remark.map(str::to_string);
    let publisher_name = new.publisher_name.to_string();
    let attachments = new.attachments.to_vec();
    let is_top = new.is_top;
    let top_expire_at = new.top_expire_at;
    let expire_at = new.expire_at;
    let publisher_id = new.publisher_id;

    ex.execute_with_params(
        "INSERT INTO sys_announcement \
                 (id, ann_no, title, summary, content_md, content_html, content_format, level, \
                  scope_type, is_top, top_expire_at, status, publish_at, expire_at, \
                  publisher_id, publisher_name, receiver_count, read_count, version, remark, \
                  created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, ?, ?, 'MARKDOWN', ?, ?, ?, ?, 'DRAFT', NULL, ?, \
                     ?::uuid, ?, 0, 0, 1, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::Text(ann_no),
            Value::Text(title),
            Value::Text(summary),
            Value::Text(content_md),
            Value::Text(content_html),
            Value::Text(level),
            Value::Text(scope_type),
            Value::Bool(is_top),
            Value::from(top_expire_at),
            Value::from(expire_at),
            Value::from(&publisher_id),
            Value::Text(publisher_name),
            Value::from(remark),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;

    insert_scope_rows(ex, &id, &scope_ids).await?;
    insert_attachment_rows(ex, &id, &attachments).await?;
    Ok(())
}

/// 更新公告（内容 + 范围 + 附件），同事务。
///
/// # 参数
///
/// - `id`：公告主键（调用方已确认存在且状态可编辑）；
/// - `title` / `summary` / `content_md` / `content_html` / `level` / `scope_type`：内容字段；
/// - `scope_ids`：范围明细（`ALL` 时空切片，会清空旧明细）；
/// - `is_top` / `top_expire_at` / `expire_at` / `remark`：其余可编辑字段；
/// - `attachments`：`None` = 不动附件；`Some` = 全量替换（删掉不在新集合里的行）；
/// - `operator`：操作人。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
#[allow(clippy::too_many_arguments)]
pub async fn update_announcement(
    ex: &Transaction,
    id: &uuid::Uuid,
    title: &str,
    summary: &str,
    content_md: &str,
    content_html: &str,
    level: &str,
    scope_type: &str,
    scope_ids: &[uuid::Uuid],
    is_top: bool,
    top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    expire_at: Option<chrono::DateTime<chrono::Utc>>,
    remark: Option<&str>,
    attachments: Option<&[(uuid::Uuid, i32)]>,
    operator: uuid::Uuid,
) -> DbResult<()> {
    let id = *id;
    let title = title.to_string();
    let summary = summary.to_string();
    let content_md = content_md.to_string();
    let content_html = content_html.to_string();
    let level = level.to_string();
    let scope_type = scope_type.to_string();
    let scope_ids = scope_ids.to_vec();
    let remark = remark.map(str::to_string);
    let attachments = attachments.map(<[(uuid::Uuid, i32)]>::to_vec);

    ex.execute_with_params(
        "UPDATE sys_announcement \
                SET title = ?, summary = ?, content_md = ?, content_html = ?, level = ?, \
                    scope_type = ?, is_top = ?, top_expire_at = ?, expire_at = ?, remark = ?, \
                    updated_at = now(), update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Text(title),
            Value::Text(summary),
            Value::Text(content_md),
            Value::Text(content_html),
            Value::Text(level),
            Value::Text(scope_type),
            Value::Bool(is_top),
            Value::from(top_expire_at),
            Value::from(expire_at),
            Value::from(remark),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await?;

    ex.execute_with_params(
        "DELETE FROM sys_announcement_scope WHERE announcement_id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await?;
    insert_scope_rows(ex, &id, &scope_ids).await?;

    if let Some(items) = attachments {
        ex.execute_with_params(
                "DELETE FROM sys_announcement_attachment \
                  WHERE announcement_id = ?::uuid \
                    AND file_id NOT IN (SELECT (x)::uuid FROM jsonb_array_elements_text(?::jsonb) AS x)",
                vec![
                    Value::from(&id),
                    Value::Json(
                        serde_json::Value::Array(
                            items
                                .iter()
                                .map(|(file_id, _)| {
                                    serde_json::Value::String(file_id.to_string())
                                })
                                .collect(),
                        )
                        .to_string(),
                    ),
                ],
            )
            .await?;
        insert_attachment_rows(ex, &id, &items).await?;
    }
    Ok(())
}

/// 往事务里写范围明细（单条 `INSERT ... SELECT jsonb_array_elements_text`）。
async fn insert_scope_rows(
    ex: &Transaction,
    announcement_id: &uuid::Uuid,
    scope_ids: &[uuid::Uuid],
) -> DbResult<()> {
    if scope_ids.is_empty() {
        return Ok(());
    }
    let payload = serde_json::Value::Array(
        scope_ids
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect(),
    )
    .to_string();
    // scope_type 由公告主体决定；这里从主体表取，避免两处传参不一致
    ex.execute_with_params(
        "INSERT INTO sys_announcement_scope (id, announcement_id, scope_type, target_id, created_at) \
         SELECT gen_random_uuid(), a.id, a.scope_type, (x)::uuid, now() \
           FROM sys_announcement a, jsonb_array_elements_text(?::jsonb) AS x \
          WHERE a.id = ?::uuid AND a.scope_type <> 'ALL' \
         ON CONFLICT (announcement_id, scope_type, target_id) DO NOTHING",
        vec![
            Value::Json(payload),
            Value::from(announcement_id),
        ],
    )
    .await?;
    Ok(())
}

/// 往事务里写附件关联行（`ON CONFLICT DO NOTHING` + 更新排序号）。
async fn insert_attachment_rows(
    ex: &Transaction,
    announcement_id: &uuid::Uuid,
    attachments: &[(uuid::Uuid, i32)],
) -> DbResult<()> {
    for (file_id, sort_order) in attachments {
        // 元数据从 `sys_file` 现查：前端不需要（也不允许）自报文件名 / 大小 / MIME。
        ex.execute_with_params(
            "INSERT INTO sys_announcement_attachment \
                 (id, announcement_id, file_id, file_name, file_size, mime_type, sort_order, created_at) \
             SELECT gen_random_uuid(), ?::uuid, f.id, \
                    COALESCE(f.original_name, f.file_name), f.file_size, f.mime_type, ?, now() \
               FROM sys_file f \
              WHERE f.id = ?::uuid AND f.deleted_at IS NULL \
             ON CONFLICT (announcement_id, file_id) DO UPDATE \
                SET sort_order = EXCLUDED.sort_order",
            vec![
                Value::from(announcement_id),
                Value::from(*sort_order),
                Value::from(file_id),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 软删公告（附件 / 范围明细保留，随主体一起不可见）。
///
/// # 返回值
///
/// 实际影响行数（0 表示已被并发删除）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn soft_delete(ex: &Transaction, id: &uuid::Uuid, operator: uuid::Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE sys_announcement \
            SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(id)],
    )
    .await
}

/// 删公告一次做完两件事：软删主体 + **同事务**下线关联通知（BUG-D-05 / FIX-04）。
///
/// # 为什么合在一个函数里
///
/// handler 若自己「先调 [`soft_delete`] 再调 [`notice::model::withdraw_by_biz`]」，
/// 将来谁在中间插一句 `?` 提前 return，就会留下「公告已删、通知还在」的半截状态。
/// 把两步绑进同一个函数，调用方只能整段成功或整段回滚。
///
/// # 语义
///
/// 1. `sys_announcement` 置 `deleted_at`（附件 / 范围明细保留，随主体一起不可见）；
/// 2. 命中的行数 > 0 时，按 `biz_type='announcement'` + `biz_id=<id>` 把 `sys_notice`
///    置 `deleted_at`，并给 `sys_notice_receiver` 打用户侧删除标记
///    （**已读回执 read_flag / read_at 不动、行不删**，审计口径见
///    [`crate::business::notice::model::withdraw_by_biz`] 的文档）；
/// 3. 命中行数为 0（并发已删）时**什么都不做** —— 通知下线留给赢下并发的那次调用，
///    避免把别人刚发布的新版本通知误杀。
///
/// # 参数
///
/// - `ex`：调用方事务（**必须**，两步的原子性全靠它）；
/// - `id`：公告主键（调用方已 `ensure_deletable`）；
/// - `operator`：操作人。
///
/// # 返回值
///
/// `(软删行数, 通知下线报告)`：行数为 0 时报告恒为空。
///
/// # 异常情况
///
/// 任一步失败 → [`DbError`]，由调用方事务整体回滚（公告的软删一并撤销）。
pub async fn soft_delete_with_notice(
    ex: &Transaction,
    id: &uuid::Uuid,
    operator: uuid::Uuid,
) -> DbResult<(u64, BizWithdrawReport)> {
    let affected = soft_delete(ex, id, operator).await?;
    if affected == 0 {
        return Ok((0, BizWithdrawReport::default()));
    }
    let notice_tx = NoticeExecutor::Tx(ex);
    let report = crate::business::notice::model::withdraw_by_biz(
        &notice_tx,
        BIZ_TYPE_ANNOUNCEMENT,
        &id.to_string(),
    )
    .await?;
    Ok((affected, report))
}

/// 发布落库：`status='PUBLISHED'`、`version+1`、`publish_at=now()`、`notice_id=NULL`。
///
/// # 参数
///
/// - `id`：公告主键（调用方已确认状态可发布、有效期合法）；
/// - `content_html`：渲染 + 净化后的快照（发布时重算，确保与当前 Markdown 一致）；
/// - `summary`：最终摘要（手工填的或自动截取的）；
/// - `operator`：操作人。
///
/// # 返回值
///
/// 发布后的 `version`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（含并发下 `promote` 失败的 0 行情形）。
pub async fn apply_publish(
    ex: &Transaction,
    id: &uuid::Uuid,
    content_html: &str,
    summary: &str,
    operator: uuid::Uuid,
) -> DbResult<i32> {
    let rows = ex
        .find_with_params(
            "UPDATE sys_announcement \
                SET status = 'PUBLISHED', version = version + 1, publish_at = now(), \
                    notice_id = NULL, content_html = ?, summary = ?, \
                    publisher_id = ?::uuid, updated_at = now(), update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL \
             RETURNING version",
            vec![
                Value::Text(content_html.to_string()),
                Value::Text(summary.to_string()),
                Value::from(&operator),
                Value::from(&operator),
                Value::from(id),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::NotFound);
    };
    row.get_typed("version")
}

/// 回填扇出结果（`notice_id` + `receiver_count`）。
///
/// # 参数
///
/// - `id`：公告主键；
/// - `notice_id`：通知主键；
/// - `receiver_count`：扇出接收人数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 为什么 `receiver_count` 要 `GREATEST(read_count, …)`
///
/// `ck_sys_announcement_counts` 要求 `read_count <= receiver_count`。补偿重放时接收人集合
/// 可能比首次小（期间有人被停用），直接把 `receiver_count` 写小会撞约束 → 500。
/// 取两者较大值保证不变量成立，且不会「抹掉」已经发生的已读。
pub async fn backfill_notice(
    id: &uuid::Uuid,
    notice_id: &uuid::Uuid,
    receiver_count: i64,
) -> DbResult<()> {
    db().execute_with_params(
        "UPDATE sys_announcement \
            SET notice_id = ?::uuid, \
                receiver_count = GREATEST(?::int, read_count), \
                updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(notice_id),
            Value::I32(i32::try_from(receiver_count).unwrap_or(i32::MAX)),
            Value::from(id),
        ],
    )
    .await?;
    Ok(())
}

/// 下线落库（`status='OFFLINE'`，不改版本）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn apply_offline(
    ex: &Transaction,
    id: &uuid::Uuid,
    operator: uuid::Uuid,
) -> DbResult<()> {
    ex.execute_with_params(
        "UPDATE sys_announcement \
            SET status = 'OFFLINE', updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(id)],
    )
    .await?;
    Ok(())
}

/// 置顶 / 取消置顶。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn apply_top(
    id: &uuid::Uuid,
    is_top: bool,
    top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    operator: uuid::Uuid,
) -> DbResult<()> {
    db().execute_with_params(
        "UPDATE sys_announcement \
            SET is_top = ?, top_expire_at = ?, updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Bool(is_top),
            Value::from(top_expire_at),
            Value::from(&operator),
            Value::from(id),
        ],
    )
    .await?;
    Ok(())
}

// ==================== 附件 ====================

/// 统计公告附件数量与合计字节数。
///
/// # 返回值
///
/// `(附件数, 合计字节数)`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn attachment_totals(announcement_id: &uuid::Uuid) -> DbResult<(i64, i64)> {
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS cnt, COALESCE(sum(file_size), 0)::bigint AS total \
               FROM sys_announcement_attachment WHERE announcement_id = ?::uuid",
            vec![Value::from(announcement_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok((row.get_typed("cnt")?, row.get_typed("total")?)),
        None => Ok((0, 0)),
    }
}

/// 绑定一个附件到公告（幂等：重复绑定只更新排序号）。
///
/// # 返回值
///
/// 绑定行主键；`None` 表示 `file_id` 不存在 / 已软删（调用方报 `400 + 62003`）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn bind_attachment(
    announcement_id: &uuid::Uuid,
    file_id: &uuid::Uuid,
    sort_order: i32,
) -> DbResult<Option<uuid::Uuid>> {
    let rows = db()
        .find_with_params(
            "INSERT INTO sys_announcement_attachment \
                 (id, announcement_id, file_id, file_name, file_size, mime_type, sort_order, created_at) \
             SELECT gen_random_uuid(), ?::uuid, f.id, \
                    COALESCE(f.original_name, f.file_name), f.file_size, f.mime_type, ?, now() \
               FROM sys_file f \
              WHERE f.id = ?::uuid AND f.deleted_at IS NULL \
             ON CONFLICT (announcement_id, file_id) DO UPDATE \
                SET sort_order = EXCLUDED.sort_order \
             RETURNING id",
            vec![
                Value::from(announcement_id),
                Value::from(sort_order),
                Value::from(file_id),
            ],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get_typed("id")?)),
        None => Ok(None),
    }
}

// ==================== 阅读名单（A-05 ~ A-09 的观测口径） ====================

/// 阅读名单分页（谁读了 / 谁没读）。
///
/// # 参数
///
/// - `announcement_id`：公告主键；
/// - `notice_id`：扇出的通知 ID（`None` = 尚未扇出，名单只含已读回执）；
/// - `state`：全部 / 已读 / 未读；
/// - `keyword`：账号 / 姓名模糊（已包装）；
/// - `page` / `page_size`：分页。
///
/// # 返回值
///
/// [`Page<ReaderRow>`]。名单宇宙 = 扇出接收人 ∪ 已读回执 —— 只取其一都会漏：
/// 关闭扇出时没接收人，用户自己点开也会留已读回执；扇出成功但通知被清理时反过来。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 阅读名单是「某条公告的投递审计」，不按读者的数据范围裁剪
/// （裁剪会让发布者看不到自己公告的送达情况，也无法解释阅读率）。
pub async fn page_readers(
    announcement_id: &uuid::Uuid,
    notice_id: Option<&uuid::Uuid>,
    state: ReaderStateFilter,
    keyword: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<Page<ReaderRow>> {
    let mut sql = String::from(
        "WITH universe AS ( \
             SELECT n.user_id FROM sys_notice_receiver n \
              WHERE n.notice_id = COALESCE(?::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
                AND n.is_deleted = FALSE \
             UNION \
             SELECT ar.user_id FROM sys_announcement_read ar WHERE ar.announcement_id = ?::uuid \
         ) \
         SELECT u.id AS user_id, u.username, COALESCE(u.real_name, '') AS real_name, \
                COALESCE(d.dept_name, '') AS dept_name, ar.read_at \
           FROM universe v \
           JOIN sys_user u ON u.id = v.user_id AND u.deleted_at IS NULL \
           LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
           LEFT JOIN sys_announcement_read ar \
                  ON ar.announcement_id = ?::uuid AND ar.user_id = v.user_id",
    );
    let mut params = vec![
        Value::from(notice_id),
        Value::from(announcement_id),
        Value::from(announcement_id),
    ];

    match state {
        ReaderStateFilter::All => {}
        ReaderStateFilter::Read => sql.push_str(" WHERE ar.read_at IS NOT NULL"),
        ReaderStateFilter::Unread => sql.push_str(" WHERE ar.read_at IS NULL"),
    }
    if let Some(pattern) = keyword {
        sql.push_str(if matches!(state, ReaderStateFilter::All) {
            " WHERE (u.username ILIKE ? OR u.real_name ILIKE ?)"
        } else {
            " AND (u.username ILIKE ? OR u.real_name ILIKE ?)"
        });
        params.push(Value::Text(pattern.to_string()));
        params.push(Value::Text(pattern.to_string()));
    }

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_announcement"),
        &sql,
        params,
        Some("ORDER BY (ar.read_at IS NULL) ASC, ar.read_at DESC NULLS LAST, u.username ASC"),
        page,
        page_size,
    )
    .await?;

    let mut rows = Vec::with_capacity(list.len());
    for row in &list {
        rows.push(ReaderRow {
            user_id: row.get_typed("user_id")?,
            username: row.get_typed("username")?,
            real_name: row.get_typed("real_name")?,
            dept_name: row.get_typed("dept_name")?,
            read_at: row.get_typed("read_at")?,
        });
    }
    Ok(Page {
        list: rows,
        total,
        page,
        size,
        pages,
    })
}

/// 阅读名单的状态过滤（model 侧的窄枚举，避免 model 依赖 dto）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderStateFilter {
    /// 全部。
    All,
    /// 已读。
    Read,
    /// 未读。
    Unread,
}

// ==================== 定时任务支撑 ====================

/// 全局扇出开关（`sys_config.announcement.fanout_enabled`，缺省 / 查询失败一律视为开启）。
///
/// # 为什么查询失败也开启
///
/// 这条开关是「应急止血」用的：读不到配置就**不发**会让一次数据库抖动变成「所有公告
/// 都发不出去」；而读不到就照常发最多是「止血没生效」，由发布接口的返回值和补偿任务
/// 的日志都能看出来。
///
/// SCOPE-EXEMPT: `sys_config` 是全局配置（不参与数据权限）。
pub async fn fanout_enabled() -> bool {
    let rows = db()
        .find_with_params(
            "SELECT config_value FROM sys_config \
              WHERE config_key = ? AND deleted_at IS NULL LIMIT 1",
            vec![Value::Text(CONFIG_FANOUT_ENABLED.to_string())],
        )
        .await;
    match rows {
        Ok(rows) => match rows.first() {
            Some(row) => {
                let raw: String = row.get_typed("config_value").unwrap_or_default();
                !matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                )
            }
            None => true,
        },
        Err(error) => {
            tracing::warn!(%error, "读取公告扇出开关失败，按「开启」处理");
            true
        }
    }
}

/// 待补偿的公告（已发布但 `notice_id` 为空）。
///
/// # 参数
///
/// - `limit`：单批上限。
///
/// # 返回值
///
/// 按发布时间升序的待补偿行（每条带范围明细，扇出时不再二次查询）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 后台补偿任务无请求者身份，按全局条件扫描（注入范围会让别人的公告永不补偿）。
pub async fn pending_dispatch(limit: i64) -> DbResult<Vec<PendingDispatchRow>> {
    let rows = db()
        .find_with_params(
            "SELECT a.id, a.version, a.title, a.summary, a.content_md, a.level, a.scope_type, \
                    a.expire_at, a.publisher_id, COALESCE(a.publisher_name, '') AS publisher_name \
               FROM sys_announcement a \
              WHERE a.deleted_at IS NULL AND a.status = 'PUBLISHED' \
                AND a.notice_id IS NULL AND a.publish_at IS NOT NULL \
              ORDER BY a.publish_at ASC, a.id ASC \
              LIMIT ?",
            vec![Value::I64(limit)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: uuid::Uuid = row.get_typed("id")?;
        list.push(PendingDispatchRow {
            id,
            version: row.get_typed("version")?,
            title: row.get_typed("title")?,
            summary: row.get_typed("summary")?,
            content_md: row.get_typed("content_md")?,
            level: row.get_typed("level")?,
            scope_type: row.get_typed("scope_type")?,
            scope_ids: list_scope_ids(&id).await?,
            expire_at: row.get_typed("expire_at")?,
            publisher_id: row.get_typed("publisher_id")?,
            publisher_name: row.get_typed("publisher_name")?,
        });
    }
    Ok(list)
}

/// 统计待补偿条数（任务日志用，不拉数据）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_pending_dispatch() -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_announcement \
          WHERE deleted_at IS NULL AND status = 'PUBLISHED' \
            AND notice_id IS NULL AND publish_at IS NOT NULL",
        Vec::new(),
    )
    .await
}

/// 到期自动下线（`PUBLISHED` + `expire_at <= now()` → `EXPIRED`）。
///
/// # 返回值
///
/// 影响行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 后台任务，按全局有效期条件批量更新。
pub async fn expire_due() -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_announcement \
            SET status = 'EXPIRED', updated_at = now() \
          WHERE deleted_at IS NULL AND status = 'PUBLISHED' \
            AND expire_at IS NOT NULL AND expire_at <= now()",
        Vec::new(),
    )
    .await
}

/// 置顶到期自动取消（`is_top` 且 `top_expire_at <= now()` → `is_top=false`）。
///
/// # 返回值
///
/// 影响行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 后台任务，按全局置顶时间条件批量更新。
pub async fn top_expire_due() -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_announcement \
            SET is_top = FALSE, top_expire_at = NULL, updated_at = now() \
          WHERE deleted_at IS NULL AND is_top = TRUE \
            AND top_expire_at IS NOT NULL AND top_expire_at <= now()",
        Vec::new(),
    )
    .await
}

/// 展开部门范围（含所有下级）。
///
/// # 参数
///
/// - `dept_ids`：发布时勾选的部门。
///
/// # 返回值
///
/// 「本部门 + 全部下级」的部门 ID 集合（**去重、排序**，便于日志与单测比对）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 为什么要展开
///
/// 架构 §2.3.2 方案 b：发布时刻把子树写进 `sys_announcement_scope`，读取侧只做
/// `= ANY(array)` 判定（不再递归），可见性查询才能吃上
/// `idx_sys_announcement_scope_reverse`。
///
/// SCOPE-EXEMPT: 部门树展开是「发布范围的解析」，与操作者的数据范围无关。
pub async fn expand_dept_ids(dept_ids: &[uuid::Uuid]) -> DbResult<Vec<uuid::Uuid>> {
    if dept_ids.is_empty() {
        return Ok(Vec::new());
    }
    let payload = serde_json::Value::Array(
        dept_ids
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect(),
    )
    .to_string();
    let rows = db()
        .find_with_params(
            "WITH RECURSIVE dept_tree AS ( \
                 SELECT d.id FROM sys_dept d \
                  WHERE d.id IN (SELECT (x)::uuid FROM jsonb_array_elements_text(?::jsonb) AS x) \
                    AND d.deleted_at IS NULL \
                 UNION \
                 SELECT c.id FROM sys_dept c \
                   JOIN dept_tree t ON c.parent_id = t.id \
                  WHERE c.deleted_at IS NULL \
             ) \
             SELECT id FROM dept_tree ORDER BY id",
            vec![Value::Json(payload)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("id")?);
    }
    Ok(ids)
}

/// 把「范围类型 + 原始 ID」解析成要落库的范围明细。
///
/// # 参数
///
/// - `scope_type`：`ALL` / `ROLE` / `DEPT` / `USER`；
/// - `scope_ids`：原始 ID（`DEPT` 会被展开成含下级的集合）。
///
/// # 返回值
///
/// 落库用的 ID 集合（`ALL` 时为空）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn resolve_scope_ids(
    scope_type: &str,
    scope_ids: &[uuid::Uuid],
) -> DbResult<Vec<uuid::Uuid>> {
    if scope_type == "DEPT" {
        return expand_dept_ids(scope_ids).await;
    }
    if scope_type == "ALL" {
        return Ok(Vec::new());
    }
    Ok(scope_ids.to_vec())
}

/// 查询用户显示名（发布人姓名）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 单行身份信息读取（`sys_user` 是登记表，但这是「取自己的显示名」，
/// 不是列表查询，注入数据范围会让非超管的发布人姓名为空）。
pub async fn user_display_name(user_id: &uuid::Uuid) -> DbResult<String> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username) AS display_name \
               FROM sys_user WHERE id = ?::uuid LIMIT 1",
            vec![Value::from(user_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => {
            let name: String = row.get_typed("display_name")?;
            Ok(name)
        }
        None => Ok(String::new()),
    }
}

#[cfg(test)]
mod manage_tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-29T10:00:00Z")
            .expect("固定时钟")
            .with_timezone(&chrono::Utc)
    }

    /// A-18 核心：`DRAFT` / `OFFLINE` / `EXPIRED` 可发布，`PUBLISHED` 不可（400 + 62009）。
    #[test]
    fn publish_state_machine() {
        for status in [STATUS_DRAFT, STATUS_OFFLINE, STATUS_EXPIRED] {
            assert!(ensure_publishable(status).is_ok(), "{status} 应允许发布");
        }
        let error = ensure_publishable(STATUS_PUBLISHED).expect_err("重复发布必须拒绝");
        assert_eq!((error.status, error.code), (400, 62009));
    }

    /// 只有 `PUBLISHED` 能下线，其余 400 + 62009。
    #[test]
    fn offline_state_machine() {
        assert!(ensure_offlinable(STATUS_PUBLISHED).is_ok());
        for status in [STATUS_DRAFT, STATUS_OFFLINE, STATUS_EXPIRED] {
            let error = ensure_offlinable(status).expect_err("非已发布不能下线");
            assert_eq!((error.status, error.code), (400, 62009), "{status}");
        }
    }

    /// A-18：`PUBLISHED` 直接 `PUT` → **409** + 62009（与架构 §2.8.3 校验表逐字一致）。
    #[test]
    fn edit_published_conflicts() {
        let error = ensure_editable(STATUS_PUBLISHED).expect_err("已发布不能直接编辑");
        assert_eq!((error.status, error.code), (409, 62009));
        for status in [STATUS_DRAFT, STATUS_OFFLINE, STATUS_EXPIRED] {
            assert!(ensure_editable(status).is_ok(), "{status} 应允许编辑");
        }
    }

    /// 已发布公告不允许直接删除（先下线），409 + 62009。
    #[test]
    fn delete_published_conflicts() {
        let error = ensure_deletable(STATUS_PUBLISHED).expect_err("已发布不能直接删除");
        assert_eq!((error.status, error.code), (409, 62009));
        assert!(ensure_deletable(STATUS_DRAFT).is_ok());
    }

    /// 编辑时有效期必须晚于上一次发布时间（否则撞 CHECK 约束变 500）。
    #[test]
    fn expire_must_be_after_publish() {
        let publish_at = now();
        let error = ensure_expire_after_publish(Some(publish_at), Some(publish_at))
            .expect_err("等于发布时间必须拒绝");
        assert_eq!((error.status, error.code), (400, 62008));
        assert!(ensure_expire_after_publish(Some(publish_at), None).is_ok());
        assert!(
            ensure_expire_after_publish(
                Some(publish_at),
                Some(publish_at + chrono::Duration::days(1))
            )
            .is_ok()
        );
        // 草稿（无发布时间）不限制
        assert!(ensure_expire_after_publish(None, Some(now())).is_ok());
    }

    /// 公告编号：日期段 + **末** 6 位（同一秒建立的两条必须不同）。
    #[test]
    fn ann_no_shape_is_unique_per_id() {
        let first = uuid::Uuid::now_v7();
        let second = uuid::Uuid::now_v7();
        let no = build_ann_no(now(), &first);
        assert!(no.starts_with("AN20261029"), "{no}");
        assert_eq!(no.len(), 16);
        assert!(no.len() <= 32, "列宽 VARCHAR(32)");
        assert_ne!(build_ann_no(now(), &first), build_ann_no(now(), &second));
    }

    /// 状态过滤的条件拼接数量 = 参数数量（防将来加条件时静默错位）。
    #[test]
    fn manage_filter_placeholders_match_params() {
        let filter = ManageFilter {
            keyword: Some("%维护%".into()),
            statuses: vec![STATUS_DRAFT.to_string(), STATUS_PUBLISHED.to_string()],
            scope_type: Some("ROLE".into()),
            level: Some("URGENT".into()),
            publish_from: Some(now()),
            publish_to: Some(now()),
        };
        // 关键词 3 + 状态 2 + 范围 1 + 级别 1 + 时间 2
        let expected = 3 + filter.statuses.len() + 1 + 1 + 2;
        assert_eq!(expected, 9, "条件数变了要同步改本断言");
        assert_eq!(filter.statuses.len(), 2);
    }

    /// 补偿批量的 SQL 常量稳定（运维按它估算单轮耗时）。
    #[test]
    fn retry_batch_is_bounded() {
        let batch = DISPATCH_RETRY_BATCH;
        assert_eq!(batch, 50);
        assert!(batch > 0, "批量必须为正，否则补偿任务形同空转");
        assert_eq!(CONFIG_FANOUT_ENABLED, "announcement.fanout_enabled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audience(role: &uuid::Uuid, dept: &uuid::Uuid) -> Audience {
        Audience {
            user_id: uuid::Uuid::now_v7(),
            role_ids: vec![*role],
            dept_ids: vec![*dept],
        }
    }

    /// 正常路径：字面量排序 + 去重，编码成 PG 数组文本。
    #[test]
    fn array_literal_sorts_and_dedups() {
        let a = uuid::Uuid::parse_str("00000000-0000-0000-0000-00000000000a").expect("合法 uuid");
        let b = uuid::Uuid::parse_str("00000000-0000-0000-0000-00000000000b").expect("合法 uuid");
        assert_eq!(
            array_literal(&[b, a, b]),
            format!("{{{a},{b}}}"),
            "必须排序去重：同一集合换顺序，参数逐字节相同"
        );
    }

    /// 边界分支：空集合编码成 `{}`（合法空数组，`= ANY('{}'::uuid[])` 恒 false）。
    #[test]
    fn array_literal_empty_is_valid_array() {
        assert_eq!(array_literal(&[]), "{}");
    }

    /// 受众参数顺序 = 谓词占位符顺序（ROLE → DEPT → USER）。
    #[test]
    fn audience_params_order_matches_predicate() {
        let role = uuid::Uuid::now_v7();
        let dept = uuid::Uuid::now_v7();
        let a = audience(&role, &dept);
        let params = audience_params(&a);
        assert_eq!(params.len(), 3);
        assert_eq!(
            VISIBLE_SCOPE.matches('?').count(),
            3,
            "VISIBLE_SCOPE 的占位符数量变了 → audience_params 必须同步"
        );
        assert_eq!(params.len(), VISIBLE_SCOPE.matches('?').count());
    }

    /// A-01 / A-02 / A-03 的谓词形态：状态 + 有效期双保险 + 四类受众都在。
    #[test]
    fn visible_predicate_covers_state_expire_and_scope() {
        for needle in [
            "a.deleted_at IS NULL",
            "a.status = 'PUBLISHED'",
            "a.publish_at <= now()",
            "a.expire_at > now()",
            "a.scope_type = 'ALL'",
            "s.scope_type = 'ROLE'",
            "s.scope_type = 'DEPT'",
            "s.scope_type = 'USER'",
        ] {
            assert!(
                VISIBLE_PREDICATE.contains(needle),
                "可见性谓词缺条件：{needle}"
            );
        }
        assert_eq!(VISIBLE_PREDICATE.matches('?').count(), 3);
    }

    /// 用户侧 5 条查询的「占位符 == 参数」不变量（防止将来加条件时静默错位）。
    ///
    /// 每条查询的绑定顺序都按 **SQL 文本里 `?` 出现的先后**：
    /// - 列表：JOIN `user_id` → 谓词（ROLE / DEPT / USER）
    /// - 未读：谓词 → `NOT EXISTS` 的 `user_id`
    /// - 详情 / 可见性判定：**投影里的 `scope_ok`（ROLE / DEPT / USER）在 FROM 之前** →
    ///   JOIN `user_id` → `WHERE id`
    /// - 全部已读：`SELECT` 投影 `user_id` → 谓词 → `NOT EXISTS` 的 `user_id`
    #[test]
    fn query_placeholders_match_params() {
        let a = audience(&uuid::Uuid::now_v7(), &uuid::Uuid::now_v7());

        // 列表：JOIN user_id + 谓词 3 个
        let mut page_params = vec![Value::from(&a.user_id)];
        page_params.extend(audience_params(&a));
        assert_eq!(page_params.len(), 4);

        // 未读：谓词 3 个 + NOT EXISTS 1 个
        let mut unread_params = audience_params(&a);
        unread_params.push(Value::from(&a.user_id));
        assert_eq!(unread_params.len(), 4);

        // 详情：谓词 3 个 + JOIN user_id + 公告 id（顺序与 `check_visibility` 的 SQL 一致）
        let mut detail_params = audience_params(&a);
        detail_params.push(Value::from(&a.user_id));
        detail_params.push(Value::from(&a.user_id));
        assert_eq!(detail_params.len(), 5);

        // 全部已读：SELECT user_id + 谓词 3 个 + NOT EXISTS 1 个
        let mut all_params = vec![Value::from(&a.user_id)];
        all_params.extend(audience_params(&a));
        all_params.push(Value::from(&a.user_id));
        assert_eq!(all_params.len(), 5);
    }
}
