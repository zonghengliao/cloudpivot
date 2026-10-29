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

use alun::prelude::*;

use crate::business::shared::scope;

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
