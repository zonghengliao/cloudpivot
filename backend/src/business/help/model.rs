//! 帮助文档（批次 C · C4）的领域模型与数据访问。
//!
//! 依据 `docs/批次C-技术架构.md`：§3.1（三表设计 / 自引用树 / 版本快照）、
//! §3.2（Markdown 渲染 + TOC）、§3.3（检索方案结论：`ILIKE` + 转义，不建 trgm）、
//! §3.4（权限）、§3.6（API 契约）、§5.5.2（H-01~H-14）。
//!
//! # 三条不变量（改这个文件前先读）
//!
//! 1. **`content_html` 只能由 [`crate::business::shared::content`] 生成**：
//!    [`render_content`] 是唯一的渲染入口。任何"顺手用 `format!("<h2>{}</h2>")` 拼一下"
//!    的写法都是 XSS 面（架构 §3.2、红线）；
//! 2. **检索只搜 `content_md`，且关键词必须过 [`escape_like`]**：不转义时用户搜 `%`
//!    会全表命中（H-08），搜 `_` 会变成单字符通配（多命中）；
//! 3. **版本号只增不减**：保存 / 发布 / 回滚都 `version + 1` 并写一条快照，
//!    回滚 = 把旧内容**复制成新版本**（H-06：历史版本一行不少）。
//!
//! # 版本号的生命周期（H-05 / H-06 的口径）
//!
//! | 动作 | `sys_help_doc.version` | `sys_help_doc_version` | 说明 |
//! |------|------------------------|------------------------|------|
//! | 新建文档 | `0` | 无快照 | 0 = "还没有任何可回滚的内容" |
//! | 保存（PUT） | `+1` | 新版本一行 | 「每次保存生成版本快照」——票面要求 |
//! | 发布 | `+1` | 新版本一行 | 连发 3 次 → 版本 1/2/3（H-05） |
//! | 回滚到 vN | `+1` | 新版本一行（内容 = vN） | 历史不动（H-06：回滚到 v1 后 `version = 4`） |
//! | 清理任务 | 不变 | 只保留最近 20 行 | 删最旧（H-07） |
//!
//! 之所以让「保存」也 +1：票面第 4 条要求「每次保存生成版本快照」，而架构 §3.1 的
//! 「发布时留快照」只覆盖发布路径。两者取并集后，"版本号"= "内容变更次数"，
//! 语义一致且不会出现「同一 version 两行不同内容」的冲突。
//!
//! # 与公告的一致性
//!
//! 编辑一条**已发布**的帮助文档会自动退回 `DRAFT`（架构 §3.6.2 的 PUT 行）：
//! 未复核的内容不该继续对外可见。副作用是 `published_at` 必须清空
//! （`ck_sys_help_doc_publish` 是状态与时间的一致性约束），重新发布时重写。

use alun::prelude::*;

use crate::business::shared::content;
use crate::business::shared::scope;
use crate::task::Scheduler;

/// 分类树的最大深度（架构 §3.1：超过 3 层前端树组件会爆炸）。
pub const MAX_CATEGORY_DEPTH: i32 = 3;

/// 每篇文档保留的版本快照数量（架构 §3.1：20 版 × 20 KB ≈ 400 KB/篇）。
pub const KEEP_VERSIONS: i64 = 20;

/// 版本清理任务名（同时是调度名与日志标识，与 `sys_job.invoke_target` 对齐）。
pub const TASK_NAME: &str = "help_version_cleanup";

/// 版本清理 cron：每天 02:40（错开日志清理 03:00 与分区运维 01:20）。
pub const TASK_CRON: &str = "0 40 2 * * *";

/// 检索关键词上限（架构 §3.3.2：多关键词 AND 连接，≤ 5 个词）。
pub const MAX_SEARCH_TERMS: usize = 5;

/// 命中片段的单侧上下文宽度（字符数）：首个命中位置前后各取 40 字。
pub const SNIPPET_CONTEXT_CHARS: usize = 40;

// ==================== 领域错误 ====================

/// 帮助文档的领域错误。
///
/// 模型层不认识 HTTP，只表达"业务上发生了什么"；映射到状态码与业务码的职责在
/// `handler::help_error`（单一出口，避免同一个错误在两个接口返回不同状态码）。
#[derive(Debug)]
pub enum HelpError {
    /// 数据库异常（原样带出，便于 handler 记日志）。
    Db(DbError),
    /// 分类层级将超过 3 层 → 422 + 62010。
    DepthExceeded,
    /// 分类下仍有（未软删的）文档 → 409 + 62011。
    CategoryNotEmpty,
    /// 分类下仍有子分类 → 409 + 10003（RESOURCE_CONFLICT）。
    CategoryHasChildren,
    /// 分类不存在 / 已软删 → 404 + 62014。
    CategoryNotFound,
    /// 分类编码重复 → 409 + 10003。
    CategoryCodeConflict,
    /// 分类树移动形成环（目标父节点在自己的子树里）→ 400 + 10001。
    CategoryMoveCycle,
    /// 文档不存在 / 已软删 → 404 + 62015。
    DocNotFound,
    /// 文档 slug 重复 → 409 + 62016。
    DocSlugConflict,
    /// 文档状态不允许该操作 → 409 + 62017。
    DocStatusConflict,
    /// 文档内容 / 标题 / slug 不合法 → 400 + 62018。
    DocContentInvalid,
    /// 版本快照不存在 → 404 + 62019。
    VersionNotFound,
}

impl From<DbError> for HelpError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl HelpError {
    /// 面向用户的提示文案（`ApiError.msg`）。
    ///
    /// # 返回值
    ///
    /// 中文短句；数据库异常统一返回空串（由 `utils::db_error` 给标准文案）。
    pub fn message(&self) -> &'static str {
        match self {
            Self::Db(_) => "",
            Self::DepthExceeded => "分类层级超过 3 层",
            Self::CategoryNotEmpty => "分类下仍有文档，不可删除",
            Self::CategoryHasChildren => "分类下仍有子分类，不可删除",
            Self::CategoryNotFound => "分类不存在或已删除",
            Self::CategoryCodeConflict => "分类编码已存在",
            Self::CategoryMoveCycle => "不能把分类移动到自己的子树下",
            Self::DocNotFound => "文档不存在或已删除",
            Self::DocSlugConflict => "文档 slug 已存在",
            Self::DocStatusConflict => "当前状态不允许该操作",
            Self::DocContentInvalid => "文档标题 / 正文 / slug 格式或长度不合法",
            Self::VersionNotFound => "指定的历史版本不存在",
        }
    }
}

// ==================== 行结构 ====================

/// 分类行（含已发布文档数）。
#[derive(Debug, Clone)]
pub struct CategoryRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 父分类（`None` = 根）。
    pub parent_id: Option<uuid::Uuid>,
    /// 显示名。
    pub name: String,
    /// 编码（`^[a-z][a-z0-9_-]{1,63}$`）。
    pub code: String,
    /// 图标名。
    pub icon: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 排序号。
    pub sort_order: i32,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 该分类下**已发布**文档数（树节点角标用）。
    pub doc_count: i64,
}

/// 分类写入参数。
#[derive(Debug, Clone)]
pub struct CategoryInput {
    /// 父分类（`None` = 根）。
    pub parent_id: Option<uuid::Uuid>,
    /// 显示名。
    pub name: String,
    /// 编码。
    pub code: String,
    /// 图标。
    pub icon: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 排序号。
    pub sort_order: i32,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
}

/// 文档列表卡片行（不含正文，列表页不需要 20 KB 正文）。
#[derive(Debug, Clone)]
pub struct DocCardRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 标题。
    pub title: String,
    /// 对外 slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类 ID。
    pub category_id: uuid::Uuid,
    /// 分类名（`LEFT JOIN`，分类缺失时为空串）。
    pub category_name: String,
    /// 关键词（逗号分隔，空串表示未填）。
    pub keywords: String,
    /// 当前版本号。
    pub version: i32,
    /// 状态（`DRAFT` / `PUBLISHED` / `OFFLINE`）。
    pub status: String,
    /// 排序号。
    pub sort_order: i32,
    /// 阅读量。
    pub view_count: i32,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 文档详情行（含 `content_md` 原文与 `content_html` 快照）。
#[derive(Debug, Clone)]
pub struct DocDetailRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 标题。
    pub title: String,
    /// 对外 slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类 ID。
    pub category_id: uuid::Uuid,
    /// 分类名。
    pub category_name: String,
    /// Markdown 原文。
    pub content_md: String,
    /// 写入时的净化 HTML 快照（出参前还要过闸 3 再净化）。
    pub content_html: String,
    /// 关键词。
    pub keywords: String,
    /// 当前版本号。
    pub version: i32,
    /// 状态。
    pub status: String,
    /// 排序号。
    pub sort_order: i32,
    /// 阅读量。
    pub view_count: i32,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 版本快照行（列表用，不含正文）。
#[derive(Debug, Clone)]
pub struct VersionRow {
    /// 版本号。
    pub version: i32,
    /// 该版本的标题。
    pub title: String,
    /// 变更说明。
    pub change_note: Option<String>,
    /// 快照时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 操作人。
    pub create_by: Option<uuid::Uuid>,
    /// 该版本正文的字节长度（列表上判断"是不是大版本"，不需要拉全文）。
    pub content_bytes: i64,
}

/// 版本快照详情（回滚用）。
#[derive(Debug, Clone)]
pub struct VersionDetailRow {
    /// 版本号。
    pub version: i32,
    /// 标题。
    pub title: String,
    /// Markdown 原文。
    pub content_md: String,
    /// 当时的 HTML 快照。
    pub content_html: String,
}

/// 检索命中行。
#[derive(Debug, Clone)]
pub struct SearchHitRow {
    /// 主键。
    pub id: uuid::Uuid,
    /// 标题。
    pub title: String,
    /// slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类名。
    pub category_name: String,
    /// 正文原文（用于在应用层生成纯文本 `snippet`）。
    pub content_md: String,
    /// 状态。
    pub status: String,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 文档列表过滤条件。
#[derive(Debug, Clone, Default)]
pub struct DocFilter {
    /// 分类 ID（配合 `include_children` 取子树）。
    pub category_id: Option<uuid::Uuid>,
    /// 是否连同子分类的文档一起取。
    pub include_children: bool,
    /// 标题 / 摘要关键词（已转义、已包 `%`）。
    pub keyword: Option<String>,
    /// 状态过滤（`None` + `published_only=false` = 不过滤；管理员用）。
    pub status: Option<String>,
    /// 强制只看已发布（用户侧）。
    pub published_only: bool,
}

/// 检索过滤条件。
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// 关键词列表（原始词，未转义；空列表 = 不检索直接返回空）。
    pub terms: Vec<String>,
    /// 状态过滤（管理员可选 `DRAFT`）。
    pub status: Option<String>,
    /// 强制只看已发布。
    pub published_only: bool,
    /// 分类过滤（含子树）。
    pub category_id: Option<uuid::Uuid>,
}

// ==================== 纯函数（可单测，不碰库） ====================

/// 转义 `LIKE` / `ILIKE` 的通配符（`\` / `%` / `_`）。
///
/// # 参数
///
/// - `raw`：用户输入的原始片段（不含首尾 `%`）。
///
/// # 返回值
///
/// 可安全放进 `ILIKE ? ESCAPE '\'` 的片段。**不转义时**用户搜 `%` 会全表命中
/// （H-08），搜 `a_b` 会把 `_` 当单字符通配（多命中）。
pub fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 把用户输入切成检索词（按空白切分，去空、去重、最多 [`MAX_SEARCH_TERMS`] 个）。
///
/// # 参数
///
/// - `raw`：原始检索串（`?q=`）。
///
/// # 返回值
///
/// 检索词列表。空输入返回空列表（调用方据此直接返回空页，**不要**拼 `%%` 全表查）。
pub fn split_terms(raw: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for part in raw.split_whitespace() {
        if part.is_empty() {
            continue;
        }
        if terms.iter().any(|existing| existing == part) {
            continue;
        }
        terms.push(part.to_string());
        if terms.len() >= MAX_SEARCH_TERMS {
            break;
        }
    }
    terms
}

/// 把检索词包成 `%…%` 的 `ILIKE` 模式（已转义）。
///
/// # 参数
///
/// - `term`：单个检索词。
///
/// # 返回值
///
/// `"%{escaped}%"`。
pub fn like_pattern(term: &str) -> String {
    format!("%{}%", escape_like(term))
}

/// 在纯文本里定位首个命中并截取片段（H-10：片段必须是**纯文本**）。
///
/// # 参数
///
/// - `plain`：正文纯文本（[`content::to_plain_text`] 的产物）。
/// - `terms`：检索词。
///
/// # 返回值
///
/// `(片段, 命中的词)`。片段去掉了所有 `<` / `>`（避免二次 XSS 面，也让 H-10 的
/// 正则断言可稳定通过），命中位置两侧各留 [`SNIPPET_CONTEXT_CHARS`] 个字符。
pub fn make_snippet(plain: &str, terms: &[String]) -> (String, Vec<String>) {
    let chars: Vec<char> = plain.chars().collect();
    let mut hit_at: Option<usize> = None;
    let mut matched: Vec<String> = Vec::new();
    for term in terms {
        let needle: Vec<char> = term.chars().collect();
        if let Some(index) = find_ci(&chars, &needle) {
            matched.push(term.clone());
            if hit_at.is_none_or(|current| index < current) {
                hit_at = Some(index);
            }
        }
    }

    let start = hit_at.unwrap_or(0).saturating_sub(SNIPPET_CONTEXT_CHARS);
    let end = (start + SNIPPET_CONTEXT_CHARS * 2).min(chars.len());
    let mut body: String = chars[start..end].iter().collect();
    body = body.replace(['<', '>'], " ");
    body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        body.insert(0, '…');
    }
    if end < chars.len() {
        body.push('…');
    }
    (body, matched)
}

/// 在字符切片里做 ASCII 大小写不敏感的子串查找。
///
/// # 参数
///
/// - `hay`：被查文本（已拆字符）。
/// - `needle`：检索词（已拆字符）。
///
/// # 返回值
///
/// 首个命中的字符下标。中文按"字符相等"比较（`eq_ignore_ascii_case` 对非 ASCII
/// 只在完全相等时为真），英文大小写不敏感。
fn find_ci(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&start| {
        hay[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// 摘要缺省值：正文纯文本的前 120 个字符。
///
/// # 参数
///
/// - `raw`：正文字符串（Markdown 或 HTML）。
///
/// # 返回值
///
/// 去标签、折叠空白后的截断文本；输入为空返回空串。
pub fn derive_summary(raw: &str) -> String {
    let plain = content::to_plain_text(raw);
    let collapsed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(120).collect()
}

/// 渲染正文：**唯一**的 `content_html` 生成入口（红线）。
///
/// # 参数
///
/// - `content_md`：Markdown 原文。
///
/// # 返回值
///
/// `Recipe::Help` 配方（保留 heading 锚点 `id`，供 TOC 跳转）净化后的 HTML。
pub fn render_content(content_md: &str) -> String {
    content::render_help(content_md).content_html
}

/// 校验 slug 格式（与 DB 的 `ck_sys_help_doc_slug` 同一条正则）。
///
/// # 参数
///
/// - `slug`：待校验的 slug。
///
/// # 返回值
///
/// 合法返回 `true`。DB 约束是最后一道闸，这里提前拦是为了给出 400 + 62018
/// 而不是让 PG 抛约束错误变成 500（H-03）。
pub fn is_valid_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.len() < 2 || bytes.len() > 120 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// 校验分类编码格式（与 DB 的 `ck_sys_help_category_code` 同一条正则）。
///
/// # 参数
///
/// - `code`：待校验的编码。
///
/// # 返回值
///
/// 合法返回 `true`（`^[a-z][a-z0-9_-]{1,63}$`）。
pub fn is_valid_category_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.len() < 2 || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

// ==================== 分类：查询 ====================

/// 分类行投影（`doc_count` 只数**已发布**文档，用户侧角标口径）。
const CATEGORY_SELECT: &str = "SELECT c.id, c.parent_id, c.name, c.code, c.icon, c.description, \
     c.sort_order, c.status, \
     (SELECT count(*) FROM sys_help_doc d \
       WHERE d.category_id = c.id AND d.deleted_at IS NULL AND d.status = 'PUBLISHED') AS doc_count \
     FROM sys_help_category c";

/// 列出全部分类（未软删）。
///
/// # 参数
///
/// - `only_enabled`：`true` 时只返回 `status = 1`（用户侧）；管理侧传 `false`。
///
/// # 返回值
///
/// 扁平列表，按 `sort_order, name` 排序；组树在 handler 里用 `utils::build_tree`
/// （分类量 ≤ 数百，一次全取比递归 CTE 逐层查更省往返）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_categories(only_enabled: bool) -> DbResult<Vec<CategoryRow>> {
    let mut sql = format!("{CATEGORY_SELECT} WHERE c.deleted_at IS NULL");
    if only_enabled {
        sql.push_str(" AND c.status = 1");
    }
    sql.push_str(" ORDER BY c.sort_order ASC, c.name ASC");
    let rows = db().find(&sql).await?;
    rows.iter().map(category_from_row).collect()
}

/// 按 ID 查分类（未软删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_category(id: &uuid::Uuid) -> DbResult<Option<CategoryRow>> {
    let sql = format!("{CATEGORY_SELECT} WHERE c.deleted_at IS NULL AND c.id = ?::uuid LIMIT 1");
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    rows.first().map(category_from_row).transpose()
}

/// 分类编码是否已被占用（未软删）。
///
/// # 参数
///
/// - `code`：编码。
/// - `exclude`：排除的分类 ID（编辑时排除自己）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn category_code_taken(code: &str, exclude: Option<&uuid::Uuid>) -> DbResult<bool> {
    let count: i64 = match exclude {
        Some(id) => {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_help_category \
                  WHERE code = ?::text AND deleted_at IS NULL AND id <> ?::uuid",
                vec![Value::Text(code.to_string()), Value::from(id)],
            )
            .await?
        }
        None => {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_help_category \
                  WHERE code = ?::text AND deleted_at IS NULL",
                vec![Value::Text(code.to_string())],
            )
            .await?
        }
    };
    Ok(count > 0)
}

/// 分类自身的深度（根 = 1）。
///
/// # 参数
///
/// - `id`：分类 ID。
///
/// # 返回值
///
/// `Some(depth)`；分类不存在 / 已软删返回 `None`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn category_depth(id: &uuid::Uuid) -> DbResult<Option<i32>> {
    let rows = db()
        .find_with_params(
            "WITH RECURSIVE up AS ( \
                 SELECT c.id, c.parent_id, 1 AS depth \
                   FROM sys_help_category c \
                  WHERE c.id = ?::uuid AND c.deleted_at IS NULL \
                 UNION ALL \
                 SELECT p.id, p.parent_id, up.depth + 1 \
                   FROM sys_help_category p \
                   JOIN up ON p.id = up.parent_id \
                  WHERE p.deleted_at IS NULL \
             ) SELECT max(depth) AS depth FROM up",
            vec![Value::from(id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let depth: Option<i32> = row.get_typed("depth")?;
    Ok(depth)
}

/// 分类子树的高度（自身 = 1）。
///
/// # 参数
///
/// - `id`：分类 ID。
///
/// # 返回值
///
/// 子树最深处到自身的层数；分类不存在返回 0。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn subtree_height(id: &uuid::Uuid) -> DbResult<i32> {
    let rows = db()
        .find_with_params(
            "WITH RECURSIVE down AS ( \
                 SELECT c.id, 1 AS h FROM sys_help_category c \
                  WHERE c.id = ?::uuid AND c.deleted_at IS NULL \
                 UNION ALL \
                 SELECT child.id, down.h + 1 \
                   FROM sys_help_category child \
                   JOIN down ON child.parent_id = down.id \
                  WHERE child.deleted_at IS NULL \
             ) SELECT COALESCE(max(h), 0) AS h FROM down",
            vec![Value::from(id)],
        )
        .await?;
    let h: i32 = rows
        .first()
        .map(|row| row.get_typed::<i32>("h"))
        .transpose()?
        .unwrap_or(0);
    Ok(h)
}

/// 分类下（未软删的）文档数，**不论状态**——删除分类的门禁看的是"有没有东西要丢"。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn category_doc_count(id: &uuid::Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_help_doc WHERE category_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(id)],
    )
    .await
}

/// 分类的直接子分类数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn category_child_count(id: &uuid::Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_help_category WHERE parent_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(id)],
    )
    .await
}

// ==================== 分类：写入 ====================

/// 新建分类（写前校验深度与编码唯一）。
///
/// # 参数
///
/// - `input`：分类参数。
///
/// # 返回值
///
/// 落库后的分类行。
///
/// # 异常情况
///
/// - [`HelpError::CategoryNotFound`]：父分类不存在；
/// - [`HelpError::DepthExceeded`]：父分类已在第 3 层（H-01）；
/// - [`HelpError::CategoryCodeConflict`]：编码重复；
/// - [`HelpError::Db`]：数据库异常。
pub async fn create_category(input: &CategoryInput) -> std::result::Result<CategoryRow, HelpError> {
    if let Some(parent_id) = input.parent_id {
        let parent_depth = category_depth(&parent_id)
            .await?
            .ok_or(HelpError::CategoryNotFound)?;
        if parent_depth + 1 > MAX_CATEGORY_DEPTH {
            return Err(HelpError::DepthExceeded);
        }
    }
    if category_code_taken(&input.code, None).await? {
        return Err(HelpError::CategoryCodeConflict);
    }

    let id = uuid::Uuid::now_v7();
    let inserted = db()
        .execute_with_params(
            "INSERT INTO sys_help_category \
                 (id, parent_id, name, code, icon, description, sort_order, status) \
             VALUES (?::uuid, NULLIF(?::text, '')::uuid, ?::text, ?::text, ?::text, ?::text, ?::int, ?::smallint)",
            vec![
                Value::from(&id),
                Value::Text(input.parent_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(input.name.clone()),
                Value::Text(input.code.clone()),
                Value::Text(input.icon.clone().unwrap_or_default()),
                Value::Text(input.description.clone().unwrap_or_default()),
                Value::I32(input.sort_order),
                Value::I16(input.status),
            ],
        )
        .await?;
    if inserted == 0 {
        return Err(HelpError::Db(DbError::InvalidValue(
            "新建分类未写入任何行".to_string(),
        )));
    }
    find_category(&id).await?.ok_or(HelpError::CategoryNotFound)
}

/// 更新分类（改名 / 换父 / 改排序 / 启停）。
///
/// # 参数
///
/// - `id`：分类 ID；
/// - `input`：新参数。
///
/// # 返回值
///
/// 更新后的分类行。
///
/// # 异常情况
///
/// - [`HelpError::CategoryNotFound`]：分类或目标父分类不存在；
/// - [`HelpError::DepthExceeded`]：移动后（自身深度 + 子树高度）超过 3 层（H-01）；
/// - [`HelpError::CategoryMoveCycle`]：目标父分类在自己的子树里；
/// - [`HelpError::CategoryCodeConflict`]：编码重复。
pub async fn update_category(
    id: &uuid::Uuid,
    input: &CategoryInput,
) -> std::result::Result<CategoryRow, HelpError> {
    let current = find_category(id)
        .await?
        .ok_or(HelpError::CategoryNotFound)?;
    if category_code_taken(&input.code, Some(id)).await? {
        return Err(HelpError::CategoryCodeConflict);
    }

    if input.parent_id != current.parent_id
        && let Some(parent_id) = input.parent_id
    {
        if &parent_id == id {
            return Err(HelpError::CategoryMoveCycle);
        }
        let parent_depth = category_depth(&parent_id)
            .await?
            .ok_or(HelpError::CategoryNotFound)?;
        // 目标父节点不能在自己的子树里（否则成环，递归 CTE 会无限展开）
        let in_subtree: i64 = db()
            .scalar_with_params(
                "WITH RECURSIVE down AS ( \
                     SELECT c.id FROM sys_help_category c \
                      WHERE c.id = ?::uuid AND c.deleted_at IS NULL \
                     UNION ALL \
                     SELECT child.id FROM sys_help_category child \
                       JOIN down ON child.parent_id = down.id \
                      WHERE child.deleted_at IS NULL \
                 ) SELECT count(*) FROM down WHERE id = ?::uuid",
                vec![Value::from(id), Value::from(&parent_id)],
            )
            .await?;
        if in_subtree > 0 {
            return Err(HelpError::CategoryMoveCycle);
        }
        // 移动后整体深度 = 父深度 + 子树高度，必须 ≤ 3
        let height = subtree_height(id).await?;
        if parent_depth + height > MAX_CATEGORY_DEPTH {
            return Err(HelpError::DepthExceeded);
        }
    }

    let affected = db()
        .execute_with_params(
            "UPDATE sys_help_category \
                SET parent_id = NULLIF(?::text, '')::uuid, name = ?::text, code = ?::text, \
                    icon = ?::text, description = ?::text, sort_order = ?::int, \
                    status = ?::smallint, updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![
                Value::Text(input.parent_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(input.name.clone()),
                Value::Text(input.code.clone()),
                Value::Text(input.icon.clone().unwrap_or_default()),
                Value::Text(input.description.clone().unwrap_or_default()),
                Value::I32(input.sort_order),
                Value::I16(input.status),
                Value::from(id),
            ],
        )
        .await?;
    if affected == 0 {
        return Err(HelpError::CategoryNotFound);
    }
    find_category(id).await?.ok_or(HelpError::CategoryNotFound)
}

/// 软删分类（有子分类或有文档时拒绝，**不做级联删**）。
///
/// # 异常情况
///
/// - [`HelpError::CategoryNotFound`]；
/// - [`HelpError::CategoryHasChildren`]：还有子分类；
/// - [`HelpError::CategoryNotEmpty`]：还有文档（H-02：409 + 62011 且分类仍在）。
pub async fn delete_category(id: &uuid::Uuid) -> std::result::Result<(), HelpError> {
    find_category(id)
        .await?
        .ok_or(HelpError::CategoryNotFound)?;
    if category_child_count(id).await? > 0 {
        return Err(HelpError::CategoryHasChildren);
    }
    if category_doc_count(id).await? > 0 {
        return Err(HelpError::CategoryNotEmpty);
    }
    db().execute_with_params(
        "UPDATE sys_help_category SET deleted_at = now(), updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(id)],
    )
    .await?;
    Ok(())
}

// ==================== 文档：查询 ====================

/// 文档卡片投影（列表 / 检索共用同一套列，避免两处字段漂移）。
const DOC_CARD_SELECT: &str = "SELECT d.id, d.title, d.slug, d.summary, d.category_id, \
     COALESCE(c.name, '') AS category_name, COALESCE(d.keywords, '') AS keywords, \
     d.version, d.status, d.sort_order, d.view_count, d.published_at, d.updated_at \
     FROM sys_help_doc d \
     LEFT JOIN sys_help_category c ON c.id = d.category_id";

/// 文档详情投影。
const DOC_DETAIL_SELECT: &str = "SELECT d.id, d.title, d.slug, d.summary, d.category_id, \
     COALESCE(c.name, '') AS category_name, d.content_md, d.content_html, \
     COALESCE(d.keywords, '') AS keywords, d.version, d.status, d.sort_order, \
     d.view_count, d.published_at, d.created_at, d.updated_at \
     FROM sys_help_doc d \
     LEFT JOIN sys_help_category c ON c.id = d.category_id";

/// 按 ID 查文档详情（未软删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_doc(id: &uuid::Uuid) -> DbResult<Option<DocDetailRow>> {
    let sql = format!("{DOC_DETAIL_SELECT} WHERE d.deleted_at IS NULL AND d.id = ?::uuid LIMIT 1");
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    rows.first().map(doc_detail_from_row).transpose()
}

/// 按 slug 查文档详情（未软删）——架构 §3.6.1 的对外稳定 URL 口径。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_doc_by_slug(slug: &str) -> DbResult<Option<DocDetailRow>> {
    let sql =
        format!("{DOC_DETAIL_SELECT} WHERE d.deleted_at IS NULL AND d.slug = ?::text LIMIT 1");
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(slug.to_string())])
        .await?;
    rows.first().map(doc_detail_from_row).transpose()
}

/// slug 是否已被占用（未软删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn slug_taken(slug: &str, exclude: Option<&uuid::Uuid>) -> DbResult<bool> {
    let count: i64 = match exclude {
        Some(id) => {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_help_doc \
                  WHERE slug = ?::text AND deleted_at IS NULL AND id <> ?::uuid",
                vec![Value::Text(slug.to_string()), Value::from(id)],
            )
            .await?
        }
        None => {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_help_doc \
                  WHERE slug = ?::text AND deleted_at IS NULL",
                vec![Value::Text(slug.to_string())],
            )
            .await?
        }
    };
    Ok(count > 0)
}

/// 分类子树 ID 集合的递归 CTE 片段（含自身）。
///
/// # 参数
///
/// - `alias`：CTE 名（调用方拼进 SQL 时用同名）。
///
/// # 返回值
///
/// `WITH RECURSIVE {alias} AS (… SELECT id …)` 形式的片段，供 `IN (SELECT id FROM {alias})`。
///
/// # 为什么不用 `path LIKE`
///
/// 与架构 §3.1 同一条论据：`dept_path` 搬迁不重算的坑。递归 CTE 每次以当前数据为准。
pub fn subtree_cte(alias: &str) -> String {
    format!(
        "WITH RECURSIVE {alias} AS ( \
             SELECT c.id FROM sys_help_category c \
              WHERE c.id = ?::uuid AND c.deleted_at IS NULL \
             UNION ALL \
             SELECT child.id FROM sys_help_category child \
               JOIN {alias} ON child.parent_id = {alias}.id \
              WHERE child.deleted_at IS NULL \
         ) "
    )
}

/// 文档列表分页。
///
/// # 参数
///
/// - `filter`：过滤条件（分类 / 关键词 / 状态）；
/// - `page` / `page_size`：分页。
///
/// # 返回值
///
/// `Page<DocCardRow>`。排序：`sort_order` 升序 → 发布时间倒序 → id 倒序。
///
/// # 为什么走 `paginate_unscoped`
///
/// 帮助文档是**全局内容**（架构 §3.5 / S-1），有意不登记 `SCOPED_TABLES`；但闸①
/// 要求列表分页只能走 [`scope::paginate`] / [`scope::paginate_unscoped`]，所以这里
/// 用显式 `unscoped` 锚点，而不是自己拼 `LIMIT/OFFSET`。
pub async fn page_docs(
    filter: &DocFilter,
    page: u64,
    page_size: u64,
) -> DbResult<Page<DocCardRow>> {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();

    let use_subtree = filter.category_id.is_some() && filter.include_children;
    if use_subtree {
        sql.push_str(&subtree_cte("help_subtree"));
        // 安全：use_subtree 为真时 category_id 必为 Some
        if let Some(category_id) = filter.category_id {
            params.push(Value::from(&category_id));
        }
    }
    sql.push_str(DOC_CARD_SELECT);
    sql.push_str(" WHERE d.deleted_at IS NULL");

    match (filter.category_id, filter.include_children) {
        (Some(_), true) => {
            sql.push_str(" AND d.category_id IN (SELECT id FROM help_subtree)");
        }
        (Some(category_id), false) => {
            sql.push_str(" AND d.category_id = ?::uuid");
            params.push(Value::from(&category_id));
        }
        _ => {}
    }
    if filter.published_only {
        sql.push_str(" AND d.status = 'PUBLISHED'");
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND d.status = ?::text");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(keyword) = filter.keyword.as_deref() {
        sql.push_str(
            " AND (d.title ILIKE ?::text ESCAPE '\\' OR d.summary ILIKE ?::text ESCAPE '\\')",
        );
        params.push(Value::Text(keyword.to_string()));
        params.push(Value::Text(keyword.to_string()));
    }

    // SCOPE-EXEMPT: 帮助文档是全局内容（架构 §3.5 / 附录 A S-1），有意不登记
    // SCOPED_TABLES；列表仍走唯一分页入口的显式 unscoped 锚点。
    let result = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_help_doc"),
        &sql,
        params,
        Some("ORDER BY d.sort_order ASC, d.published_at DESC NULLS LAST, d.id DESC"),
        page,
        page_size,
    )
    .await?;

    let mut list = Vec::with_capacity(result.list.len());
    for row in &result.list {
        list.push(doc_card_from_row(row)?);
    }
    Ok(Page {
        list,
        total: result.total,
        page: result.page,
        size: result.size,
        pages: result.pages,
    })
}

/// 检索（标题 + 正文，`ILIKE`，多词 AND）。
///
/// # 参数
///
/// - `filter`：检索词（原始词，本函数负责转义）与状态 / 分类过滤；
/// - `page` / `page_size`：分页。
///
/// # 返回值
///
/// `Page<SearchHitRow>`。`terms` 为空时返回**空页**（不查库）：`%%` 会全表命中，
/// 而"没关键词"的正确语义是"没有结果"，不是"全部结果"。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn search(
    filter: &SearchFilter,
    page: u64,
    page_size: u64,
) -> DbResult<Page<SearchHitRow>> {
    if filter.terms.is_empty() {
        return Ok(Page {
            list: Vec::new(),
            total: 0,
            page,
            size: page_size,
            pages: 0,
        });
    }

    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(category_id) = filter.category_id {
        sql.push_str(&subtree_cte("help_subtree"));
        params.push(Value::from(&category_id));
    }
    sql.push_str(
        "SELECT d.id, d.title, d.slug, d.summary, COALESCE(c.name, '') AS category_name, \
                d.content_md, d.status, d.published_at \
           FROM sys_help_doc d \
           LEFT JOIN sys_help_category c ON c.id = d.category_id \
          WHERE d.deleted_at IS NULL",
    );
    if filter.category_id.is_some() {
        sql.push_str(" AND d.category_id IN (SELECT id FROM help_subtree)");
    }
    if filter.published_only {
        sql.push_str(" AND d.status = 'PUBLISHED'");
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND d.status = ?::text");
        params.push(Value::Text(status.to_string()));
    }
    // 每个词一组 (title OR content_md)，组间 AND
    for term in &filter.terms {
        sql.push_str(
            " AND (d.title ILIKE ?::text ESCAPE '\\' OR d.content_md ILIKE ?::text ESCAPE '\\')",
        );
        let pattern = like_pattern(term);
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }

    // SCOPE-EXEMPT: 同 page_docs（全局内容，显式 unscoped 锚点）
    let result = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_help_doc"),
        &sql,
        params,
        Some("ORDER BY d.published_at DESC NULLS LAST, d.sort_order ASC, d.id DESC"),
        page,
        page_size,
    )
    .await?;

    let mut list = Vec::with_capacity(result.list.len());
    for row in &result.list {
        list.push(SearchHitRow {
            id: row.get_typed("id")?,
            title: row.get_typed("title")?,
            slug: row.get_typed("slug")?,
            summary: row.get_typed("summary")?,
            category_name: row.get_typed("category_name")?,
            content_md: row.get_typed("content_md")?,
            status: row.get_typed("status")?,
            published_at: row.get_typed("published_at")?,
        });
    }
    Ok(Page {
        list,
        total: result.total,
        page: result.page,
        size: result.size,
        pages: result.pages,
    })
}

// ==================== 文档：写入 ====================

/// 文档写入参数。
#[derive(Debug, Clone)]
pub struct DocInput {
    /// 分类 ID。
    pub category_id: uuid::Uuid,
    /// 标题。
    pub title: String,
    /// slug。
    pub slug: String,
    /// 摘要（为空时由正文纯文本前 120 字补齐）。
    pub summary: Option<String>,
    /// Markdown 原文。
    pub content_md: String,
    /// 关键词（逗号分隔）。
    pub keywords: Option<String>,
    /// 排序号。
    pub sort_order: i32,
}

/// 新建文档（初始草稿，`version = 0`，不产生版本快照）。
///
/// # 参数
///
/// - `input`：文档参数；
/// - `operator`：操作人（写 `create_by`）。
///
/// # 返回值
///
/// 落库后的详情行（`content_html` 已由 [`render_content`] 生成）。
///
/// # 异常情况
///
/// - [`HelpError::CategoryNotFound`]：分类不存在；
/// - [`HelpError::DocContentInvalid`]：标题 / 正文 / slug 不合法；
/// - [`HelpError::DocSlugConflict`]：slug 重复（H-03）。
pub async fn create_doc(
    input: &DocInput,
    operator: Option<uuid::Uuid>,
) -> std::result::Result<DocDetailRow, HelpError> {
    validate_doc_input(input)?;
    find_category(&input.category_id)
        .await?
        .ok_or(HelpError::CategoryNotFound)?;
    if slug_taken(&input.slug, None).await? {
        return Err(HelpError::DocSlugConflict);
    }

    let id = uuid::Uuid::now_v7();
    let summary = resolve_summary(input);
    let html = render_content(&input.content_md);
    let affected = db()
        .execute_with_params(
            "INSERT INTO sys_help_doc \
                 (id, category_id, title, slug, summary, content_md, content_html, keywords, \
                  version, status, sort_order, view_count, published_at, create_by, update_by) \
             VALUES (?::uuid, ?::uuid, ?::text, ?::text, ?::text, ?::text, ?::text, ?::text, \
                     0, 'DRAFT', ?::int, 0, NULL, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)",
            vec![
                Value::from(&id),
                Value::from(&input.category_id),
                Value::Text(input.title.clone()),
                Value::Text(input.slug.clone()),
                Value::Text(summary),
                Value::Text(input.content_md.clone()),
                Value::Text(html),
                Value::Text(input.keywords.clone().unwrap_or_default()),
                Value::I32(input.sort_order),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await?;
    if affected == 0 {
        return Err(HelpError::Db(DbError::InvalidValue(
            "新建文档未写入任何行".to_string(),
        )));
    }
    find_doc(&id).await?.ok_or(HelpError::DocNotFound)
}

/// 保存文档（**每次保存生成一条版本快照**，`version + 1`）。
///
/// # 参数
///
/// - `id`：文档 ID；
/// - `input`：新参数；
/// - `operator`：操作人；
/// - `note`：变更说明（缺省 `"保存"`）。
///
/// # 返回值
///
/// 保存后的详情行。
///
/// # 状态机（架构 §3.6.2）
///
/// 编辑一条 `PUBLISHED` 文档会退回 `DRAFT` 并清空 `published_at`
/// （`ck_sys_help_doc_publish` 要求两者一致）；`OFFLINE` 保持 `OFFLINE`。
///
/// # 异常情况
///
/// 同 [`create_doc`]，另加 [`HelpError::DocNotFound`]。
pub async fn save_doc(
    id: &uuid::Uuid,
    input: &DocInput,
    operator: Option<uuid::Uuid>,
    note: Option<&str>,
) -> std::result::Result<DocDetailRow, HelpError> {
    validate_doc_input(input)?;
    find_doc(id).await?.ok_or(HelpError::DocNotFound)?;
    find_category(&input.category_id)
        .await?
        .ok_or(HelpError::CategoryNotFound)?;
    if slug_taken(&input.slug, Some(id)).await? {
        return Err(HelpError::DocSlugConflict);
    }

    let summary = resolve_summary(input);
    let html = render_content(&input.content_md);
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    let note_text = note.unwrap_or("保存").to_string();
    let doc_id = *id;
    let category_id = input.category_id;
    let title = input.title.clone();
    let slug = input.slug.clone();
    let content_md = input.content_md.clone();
    let keywords = input.keywords.clone().unwrap_or_default();
    let sort_order = input.sort_order;

    db().transaction(move |tx| async move {
        tx.execute_with_params(
            "UPDATE sys_help_doc \
                SET category_id = ?::uuid, title = ?::text, slug = ?::text, summary = ?::text, \
                    content_md = ?::text, content_html = ?::text, keywords = ?::text, \
                    sort_order = ?::int, version = version + 1, \
                    status = CASE WHEN status = 'PUBLISHED' THEN 'DRAFT' ELSE status END, \
                    published_at = CASE WHEN status = 'PUBLISHED' THEN NULL ELSE published_at END, \
                    update_by = NULLIF(?::text, '')::uuid, updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![
                Value::from(&category_id),
                Value::Text(title),
                Value::Text(slug),
                Value::Text(summary),
                Value::Text(content_md),
                Value::Text(html),
                Value::Text(keywords),
                Value::I32(sort_order),
                Value::Text(operator_text.clone()),
                Value::from(&doc_id),
            ],
        )
        .await?;
        snapshot_in_tx(&tx, &doc_id, Some(&note_text), &operator_text).await
    })
    .await?;

    find_doc(id).await?.ok_or(HelpError::DocNotFound)
}

/// 发布文档（`version + 1` + 版本快照 + `PUBLISHED`）。
///
/// # 参数
///
/// - `id`：文档 ID；
/// - `operator`：操作人；
/// - `note`：变更说明（缺省 `"发布"`）。
///
/// # 返回值
///
/// 发布后的详情行。
///
/// # 异常情况
///
/// - [`HelpError::DocNotFound`]；
/// - [`HelpError::Db`]。
pub async fn publish_doc(
    id: &uuid::Uuid,
    operator: Option<uuid::Uuid>,
    note: Option<&str>,
) -> std::result::Result<DocDetailRow, HelpError> {
    find_doc(id).await?.ok_or(HelpError::DocNotFound)?;
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    let note_text = note.unwrap_or("发布").to_string();
    let doc_id = *id;

    db().transaction(move |tx| async move {
        tx.execute_with_params(
            "UPDATE sys_help_doc \
                SET status = 'PUBLISHED', published_at = COALESCE(published_at, now()), \
                    version = version + 1, update_by = NULLIF(?::text, '')::uuid, \
                    updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::Text(operator_text.clone()), Value::from(&doc_id)],
        )
        .await?;
        snapshot_in_tx(&tx, &doc_id, Some(&note_text), &operator_text).await
    })
    .await?;

    find_doc(id).await?.ok_or(HelpError::DocNotFound)
}

/// 回滚到指定版本（**用旧内容生成新版本**，历史一行不删）。
///
/// # 参数
///
/// - `id`：文档 ID；
/// - `version`：目标版本号；
/// - `operator`：操作人；
/// - `note`：变更说明（缺省 `"回滚到 v{version}"`）。
///
/// # 返回值
///
/// 回滚后的详情行：内容 = 目标版本内容，`version = 原 version + 1`（H-06）。
///
/// # 异常情况
///
/// - [`HelpError::DocNotFound`]；
/// - [`HelpError::VersionNotFound`]：目标版本不存在；
/// - [`HelpError::Db`]。
pub async fn rollback_doc(
    id: &uuid::Uuid,
    version: i32,
    operator: Option<uuid::Uuid>,
    note: Option<&str>,
) -> std::result::Result<DocDetailRow, HelpError> {
    find_doc(id).await?.ok_or(HelpError::DocNotFound)?;
    let target = find_version(id, version)
        .await?
        .ok_or(HelpError::VersionNotFound)?;

    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    let note_text = note
        .map(str::to_string)
        .unwrap_or_else(|| format!("回滚到 v{version}"));
    let doc_id = *id;
    let title = target.title;
    let content_md = target.content_md;
    let content_html = target.content_html;

    db().transaction(move |tx| async move {
        tx.execute_with_params(
            "UPDATE sys_help_doc \
                SET title = ?::text, content_md = ?::text, content_html = ?::text, \
                    version = version + 1, update_by = NULLIF(?::text, '')::uuid, \
                    updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![
                Value::Text(title),
                Value::Text(content_md),
                Value::Text(content_html),
                Value::Text(operator_text.clone()),
                Value::from(&doc_id),
            ],
        )
        .await?;
        snapshot_in_tx(&tx, &doc_id, Some(&note_text), &operator_text).await
    })
    .await?;

    find_doc(id).await?.ok_or(HelpError::DocNotFound)
}

/// 软删文档。
///
/// # 异常情况
///
/// - [`HelpError::DocNotFound`]；
/// - [`HelpError::Db`]。
pub async fn delete_doc(id: &uuid::Uuid) -> std::result::Result<(), HelpError> {
    find_doc(id).await?.ok_or(HelpError::DocNotFound)?;
    db().execute_with_params(
        "UPDATE sys_help_doc SET deleted_at = now(), updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(id)],
    )
    .await?;
    Ok(())
}

/// 阅读量 +1（去重的判定在 handler，这里只做原子自增）。
///
/// # 返回值
///
/// 自增后的阅读量；文档不存在返回 `None`。
///
/// # 为什么用 `RETURNING`
///
/// `UPDATE … RETURNING` 一条语句同时完成"加"与"读回"，避免两条语句之间插进
/// 别人的自增导致返回旧值。
pub async fn increment_view(id: &uuid::Uuid) -> DbResult<Option<i32>> {
    let rows = db()
        .find_with_params(
            "UPDATE sys_help_doc SET view_count = view_count + 1 \
              WHERE id = ?::uuid AND deleted_at IS NULL \
             RETURNING view_count",
            vec![Value::from(id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed::<i32>("view_count"))
        .transpose()
}

// ==================== 版本快照 ====================

/// 列出某文档的版本快照（新 → 旧）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_versions(doc_id: &uuid::Uuid) -> DbResult<Vec<VersionRow>> {
    let rows = db()
        .find_with_params(
            "SELECT version, title, change_note, created_at, create_by, \
                    octet_length(content_md)::bigint AS content_bytes \
               FROM sys_help_doc_version \
              WHERE doc_id = ?::uuid \
              ORDER BY version DESC",
            vec![Value::from(doc_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(VersionRow {
                version: row.get_typed("version")?,
                title: row.get_typed("title")?,
                change_note: row.get_typed("change_note")?,
                created_at: row.get_typed("created_at")?,
                create_by: row.get_typed("create_by")?,
                content_bytes: row.get_typed("content_bytes")?,
            })
        })
        .collect()
}

/// 取单个版本快照（回滚用）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_version(doc_id: &uuid::Uuid, version: i32) -> DbResult<Option<VersionDetailRow>> {
    let rows = db()
        .find_with_params(
            "SELECT version, title, content_md, content_html \
               FROM sys_help_doc_version \
              WHERE doc_id = ?::uuid AND version = ?::int LIMIT 1",
            vec![Value::from(doc_id), Value::I32(version)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(VersionDetailRow {
                version: row.get_typed("version")?,
                title: row.get_typed("title")?,
                content_md: row.get_typed("content_md")?,
                content_html: row.get_typed("content_html")?,
            })
        })
        .transpose()
}

/// 清理某文档的历史版本，只留最近 `keep` 条。
///
/// # 返回值
///
/// 删除的行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn cleanup_versions(doc_id: &uuid::Uuid, keep: i64) -> DbResult<u64> {
    db().execute_with_params(
        "DELETE FROM sys_help_doc_version v \
          WHERE v.doc_id = ?::uuid \
            AND v.version <= (SELECT max(version) FROM sys_help_doc_version WHERE doc_id = ?::uuid) - ?::bigint",
        vec![
            Value::from(doc_id),
            Value::from(doc_id),
            Value::I64(keep),
        ],
    )
    .await
}

/// 清理**全部**文档的历史版本（定时任务用）。
///
/// # 返回值
///
/// 删除的行数（H-07：25 版 → 只剩 20 版，删的是最旧的 5 条）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn cleanup_all_versions(keep: i64) -> DbResult<u64> {
    db().execute_with_params(
        "DELETE FROM sys_help_doc_version v \
          USING (SELECT doc_id, max(version) AS top \
                   FROM sys_help_doc_version GROUP BY doc_id) t \
          WHERE v.doc_id = t.doc_id AND v.version <= t.top - ?::bigint",
        vec![Value::I64(keep)],
    )
    .await
}

/// 版本清理任务体（供定时任务与手工触发共用）。
///
/// # 返回値
///
/// 删除的版本行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn run_cleanup_task() -> DbResult<u64> {
    cleanup_all_versions(KEEP_VERSIONS).await
}

/// 在调度器上注册 `help_version_cleanup`（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
///
/// # 为什么必须在 `register_builtin` 里注册
///
/// 框架调度器在 `start()` 时对注册表做**一次性快照**，写进 `bootstrap.rs` 会被随后的
/// `reload()` 清空（批次 A 已实测）。C0 占位实现挂的是空任务，C4 在这里换成真实现。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：帮助文档历史版本清理",
        || {
            tokio::spawn(async move {
                match run_cleanup_task().await {
                    Ok(deleted) => {
                        tracing::info!(deleted, "帮助文档历史版本清理完成（保留最近 20 版）")
                    }
                    Err(error) => tracing::error!(error = %error, "帮助文档历史版本清理失败"),
                }
            })
        },
    );
}

// ==================== 内部辅助 ====================

/// 事务内写一条版本快照（`INSERT … SELECT` 保证快照内容 = 刚落库的文档行）。
///
/// # 参数
///
/// - `tx`：事务句柄（必须与紧邻的 `UPDATE` 用同一个事务，否则可能快照到别人的版本）；
/// - `doc_id`：文档 ID；
/// - `note`：变更说明；
/// - `operator`：操作人字符串（空串 = 未知）。
async fn snapshot_in_tx(
    tx: &Transaction,
    doc_id: &uuid::Uuid,
    note: Option<&str>,
    operator: &str,
) -> DbResult<()> {
    tx.execute_with_params(
        "INSERT INTO sys_help_doc_version \
             (doc_id, version, title, content_md, content_html, change_note, create_by) \
         SELECT d.id, d.version, d.title, d.content_md, d.content_html, ?::text, \
                NULLIF(?::text, '')::uuid \
           FROM sys_help_doc d WHERE d.id = ?::uuid \
         ON CONFLICT (doc_id, version) DO NOTHING",
        vec![
            Value::Text(note.unwrap_or("").to_string()),
            Value::Text(operator.to_string()),
            Value::from(doc_id),
        ],
    )
    .await?;
    Ok(())
}

/// 校验文档写入参数（长度 / 格式；与 DB 约束同一口径）。
///
/// # 异常情况
///
/// 不合法返回 [`HelpError::DocContentInvalid`]。
fn validate_doc_input(input: &DocInput) -> std::result::Result<(), HelpError> {
    let title_len = input.title.chars().count();
    if title_len == 0 || title_len > 200 {
        return Err(HelpError::DocContentInvalid);
    }
    if !is_valid_slug(&input.slug) {
        return Err(HelpError::DocContentInvalid);
    }
    let bytes = input.content_md.len();
    if bytes == 0 || bytes > 262_144 {
        return Err(HelpError::DocContentInvalid);
    }
    if let Some(summary) = input.summary.as_deref()
        && summary.chars().count() > 500
    {
        return Err(HelpError::DocContentInvalid);
    }
    if let Some(keywords) = input.keywords.as_deref()
        && keywords.chars().count() > 255
    {
        return Err(HelpError::DocContentInvalid);
    }
    Ok(())
}

/// 摘要：显式给了就用，空白则从正文纯文本派生。
fn resolve_summary(input: &DocInput) -> String {
    match input.summary.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => derive_summary(&input.content_md),
    }
}

fn category_from_row(row: &Row) -> DbResult<CategoryRow> {
    Ok(CategoryRow {
        id: row.get_typed("id")?,
        parent_id: row.get_typed("parent_id")?,
        name: row.get_typed("name")?,
        code: row.get_typed("code")?,
        icon: row.get_typed("icon")?,
        description: row.get_typed("description")?,
        sort_order: row.get_typed("sort_order")?,
        status: row.get_typed("status")?,
        doc_count: row.get_typed("doc_count")?,
    })
}

fn doc_card_from_row(row: &Row) -> DbResult<DocCardRow> {
    Ok(DocCardRow {
        id: row.get_typed("id")?,
        title: row.get_typed("title")?,
        slug: row.get_typed("slug")?,
        summary: row.get_typed("summary")?,
        category_id: row.get_typed("category_id")?,
        category_name: row.get_typed("category_name")?,
        keywords: row.get_typed("keywords")?,
        version: row.get_typed("version")?,
        status: row.get_typed("status")?,
        sort_order: row.get_typed("sort_order")?,
        view_count: row.get_typed("view_count")?,
        published_at: row.get_typed("published_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

fn doc_detail_from_row(row: &Row) -> DbResult<DocDetailRow> {
    Ok(DocDetailRow {
        id: row.get_typed("id")?,
        title: row.get_typed("title")?,
        slug: row.get_typed("slug")?,
        summary: row.get_typed("summary")?,
        category_id: row.get_typed("category_id")?,
        category_name: row.get_typed("category_name")?,
        content_md: row.get_typed("content_md")?,
        content_html: row.get_typed("content_html")?,
        keywords: row.get_typed("keywords")?,
        version: row.get_typed("version")?,
        status: row.get_typed("status")?,
        sort_order: row.get_typed("sort_order")?,
        view_count: row.get_typed("view_count")?,
        published_at: row.get_typed("published_at")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H-08：`%` / `_` / `\` 必须转义，否则 `ILIKE` 语义漂移。
    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(like_pattern("100%"), "%100\\%%");
        assert_eq!(like_pattern("a_b"), "%a\\_b%");
        assert_eq!(like_pattern("c\\d"), "%c\\\\d%");
        // 普通中文不受影响
        assert_eq!(like_pattern("基座"), "%基座%");
    }

    /// H-08 的"不全表命中"另一面：只由通配符组成的检索词，转义后是**字面量匹配**。
    #[test]
    fn escape_like_keeps_percent_literal() {
        let pattern = like_pattern("%");
        assert_eq!(pattern, "%\\%%");
        // 断言这个模式在 SQL 语义下只匹配"含 % 字面量"的行：
        // `%\%` + `%` → 前缀任意 + 字面 % + 后缀任意。这里用字符串形态固化口径。
        assert!(pattern.starts_with("%\\%"));
    }

    /// 多关键词：按空白切分、去重、最多 5 个（架构 §3.3.2）。
    #[test]
    fn split_terms_dedups_and_caps() {
        assert_eq!(split_terms("  基座  公告  "), vec!["基座", "公告"]);
        assert_eq!(split_terms("a a a b"), vec!["a", "b"]);
        assert_eq!(split_terms("1 2 3 4 5 6 7").len(), MAX_SEARCH_TERMS);
        assert!(split_terms("   ").is_empty());
    }

    /// H-10：片段里不允许出现 `<` / `>`（纯文本，不给二次 XSS 面）。
    #[test]
    fn snippet_strips_angle_brackets() {
        let plain = "正文 <script>alert(1)</script> 结尾";
        let terms = vec!["script".to_string()];
        let (snippet, matched) = make_snippet(plain, &terms);
        assert!(!snippet.contains('<'), "{snippet}");
        assert!(!snippet.contains('>'), "{snippet}");
        assert_eq!(matched, vec!["script".to_string()]);
        assert!(snippet.contains("script"), "{snippet}");
    }

    /// 片段围绕首个命中截取，两侧加省略号。
    #[test]
    fn snippet_windows_around_first_hit() {
        let plain = format!("{}关键字{}", "前".repeat(200), "后".repeat(200));
        let (snippet, _) = make_snippet(&plain, &["关键字".to_string()]);
        assert!(snippet.starts_with('…'), "{snippet}");
        assert!(snippet.ends_with('…'), "{snippet}");
        assert!(snippet.contains("关键字"), "{snippet}");
        // 40 + 3 + 40 = 83 个字符（加两侧省略号）
        assert!(snippet.chars().count() <= 2 * SNIPPET_CONTEXT_CHARS + 5);
    }

    /// 英文大小写不敏感（`ILIKE` 的语义），中文按字符相等。
    #[test]
    fn snippet_is_ascii_case_insensitive() {
        let (snippet, matched) = make_snippet("System Backup Guide", &["backup".to_string()]);
        assert!(snippet.contains("Backup"), "{snippet}");
        assert_eq!(matched.len(), 1);
    }

    /// 未命中时退化为开头片段（不 panic、不返回空串）。
    #[test]
    fn snippet_falls_back_to_head_when_missed() {
        let (snippet, matched) = make_snippet("完全无关的正文内容", &["不存在的词".to_string()]);
        assert!(matched.is_empty());
        assert!(!snippet.is_empty());
    }

    /// H-03：slug 正则与 DB 的 `ck_sys_help_doc_slug` 逐字一致。
    #[test]
    fn slug_validation_matches_db_check() {
        for ok in ["ab", "a1", "help-center", "abc-123", &"a".repeat(120)] {
            assert!(is_valid_slug(ok), "应合法：{ok}");
        }
        for bad in [
            "",
            "a",
            "A",
            "My Slug",
            "帮助",
            "-abc",
            "a_b",
            &"a".repeat(121),
        ] {
            assert!(!is_valid_slug(bad), "应非法：{bad}");
        }
    }

    /// 分类编码正则与 `ck_sys_help_category_code` 一致。
    #[test]
    fn category_code_validation_matches_db_check() {
        for ok in ["ab", "help_center", "guide-1", "a_"] {
            assert!(is_valid_category_code(ok), "应合法：{ok}");
        }
        for bad in ["", "a", "A1", "1abc", "帮助", "a b"] {
            assert!(!is_valid_category_code(bad), "应非法：{bad}");
        }
    }

    /// 摘要派生：剥标签、折叠空白、截断 120 字。
    #[test]
    fn derive_summary_from_markdown() {
        let summary = derive_summary("## 标题\n\n正文 **加粗** 内容");
        assert!(!summary.contains('#'));
        assert!(!summary.contains("**"));
        assert!(summary.contains("正文"));
    }

    /// 渲染入口必须走 `Recipe::Help`：保留 heading 锚点 `id`（H-11 的根因）。
    ///
    /// # 层级偏移（与架构 §3.2 示例的差异，实测口径）
    ///
    /// `shared/content`（C1）把正文标题**统一 +1**：页面已有 `<h1>`，`##` 渲染成
    /// `<h3>`（公告 A-13 用的是同一条规则）。因此 `## 维护时间` 的锚点是
    /// `h-3-1` 而不是架构文档里举例的 `h-2-1` —— **id 与 HTML 标签层级一致**才是
    /// 真正的不变量（TOC 跳转依赖这一点），H-11 的断言按实际形态修正。
    #[test]
    fn render_content_keeps_heading_anchor() {
        let html = render_content("## 维护时间\n\n### 步骤\n");
        assert!(html.contains("<h3 id=\"h-3-1\">"), "{html}");
        assert!(html.contains("<h4 id=\"h-4-1\">"), "{html}");
    }

    /// 渲染入口必须净化脚本（红线：content_html 不能自带 `<script>`）。
    #[test]
    fn render_content_strips_script() {
        let html = render_content("<script>alert(1)</script>正文");
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("正文"), "{html}");
    }

    /// 错误文案齐全（漏一个会是空消息）。
    #[test]
    fn every_domain_error_has_message() {
        let errors = [
            HelpError::DepthExceeded,
            HelpError::CategoryNotEmpty,
            HelpError::CategoryHasChildren,
            HelpError::CategoryNotFound,
            HelpError::CategoryCodeConflict,
            HelpError::CategoryMoveCycle,
            HelpError::DocNotFound,
            HelpError::DocSlugConflict,
            HelpError::DocStatusConflict,
            HelpError::DocContentInvalid,
            HelpError::VersionNotFound,
        ];
        for error in errors {
            assert!(!error.message().is_empty(), "{error:?} 缺文案");
        }
    }

    /// 子树 CTE 片段必须带 `?::uuid`（参数按位置绑定，漏了会绑不上）。
    #[test]
    fn subtree_cte_is_parameterised() {
        let cte = subtree_cte("help_subtree");
        assert!(cte.starts_with("WITH RECURSIVE help_subtree"));
        assert_eq!(cte.matches("?::uuid").count(), 1);
    }
}
