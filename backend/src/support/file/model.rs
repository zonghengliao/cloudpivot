//! 文件元数据的强类型模型与数据访问。
//!
//! 对应迁移 `V0010__create_sys_file`。v1 仅使用本地存储单后端，因此去掉了旧实现
//! 的存储桶字段；上传时间由 `created_at` 承担。
//!
//! # 分层
//!
//! 需要编写 SQL 的查询（秒传查询、分页）集中在**本文件**；仅调用框架快捷方法
//! （`find_or_none` / `insert` / `delete` 等）的简单读写由 `handler.rs` 直接执行。

use nik::prelude::*;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::shared::constants::EnableStatus;
use crate::utils;

/// `sys_file` 表对应的强类型模型（文件元数据）。
///
/// # 唯一性与软删
///
/// `file_key` 的部分唯一索引带 `WHERE deleted_at IS NULL`；`file_hash` 用于
/// 秒传（同一 SHA-256 直接复用已存对象）。
///
/// # 大小类型
///
/// `file_size` 对应 `BIGINT NOT NULL`，用 `i64` 承载。
#[derive(Debug, Clone, Model)]
#[table("sys_file")]
pub struct SysFileModel {
    /// 主键（UUID v7）。
    pub id: uuid::Uuid,
    /// 存储文件名（唯一标识）。
    pub file_name: String,
    /// 原始文件名（用户上传时的名称）。
    pub original_name: Option<String>,
    /// 对象键，软删表上的部分唯一列。
    pub file_key: String,
    /// 文件大小（字节）。
    pub file_size: i64,
    /// 文件 SHA-256（用于秒传与完整性校验）。
    pub file_hash: Option<String>,
    /// MIME 类型。
    pub mime_type: Option<String>,
    /// 扩展名。
    pub extension: Option<String>,
    /// 存储路径。
    pub storage_path: Option<String>,
    /// 分类（`avatar` / `attachment` / `document`）。
    pub category: Option<String>,
    /// 文件状态（1 正常 / 0 已删除）。
    pub status: EnableStatus,
    /// 上传人用户 ID。
    pub uploaded_by: Option<uuid::Uuid>,
    /// 创建时间（即上传时间），由 `TimestampHook` 维护。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间，由 `TimestampHook` 维护。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 创建人 ID。
    pub create_by: Option<uuid::Uuid>,
    /// 更新人 ID。
    pub update_by: Option<uuid::Uuid>,
    /// 软删标记（`NULL` = 未删除）。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ==================== 上传人显示名（ATT-03） ====================

/// 上传人显示名表达式（join 版，`sys_user` 别名 `u`）。
///
/// 单独抽成常量是为了让「分页 / 详情 join 版」与「单用户查询版」
/// （[`UPLOADER_NAME_EXPR_SELF`]）在单测里能逐字比对 —— 两处口径漂移会直接红。
const UPLOADER_NAME_EXPR_JOINED: &str = "COALESCE(NULLIF(u.real_name, ''), u.username, '')";

/// 上传人显示名表达式（单表版，直接取 `sys_user` 自身列）。
const UPLOADER_NAME_EXPR_SELF: &str = "COALESCE(NULLIF(real_name, ''), username, '')";

/// 分页 / 详情共用的列清单（唯一来源：要改列名只改这里）。
///
/// `uploader_name` 由 `sys_user` 实时 join 得到，**不在 `sys_file` 存快照列**：
/// 上传人属「主数据关联 → join 实时取名」（AGENTS.md 约定 13），用户改名后列表
/// 应显示新名。
///
/// 三处口径细节（与 `business/attachment` 的 join 保持一致）：
///
/// 1. [`UPLOADER_NAME_EXPR_JOINED`] —— 真名优先、空则用户名、再空则空串
///    （`String` 契约恒定非 NULL，前端无需判 null）；
/// 2. **不过滤 `u.deleted_at`** —— 用户被软删后，历史文件仍应显示「当时是谁上传的」，
///    直接 `INNER JOIN`/过滤软删会把它变成空；
/// 3. `uploaded_by` 指向不存在 / 已物理删除的用户（历史脏数据）时 LEFT JOIN 出 NULL，
///    被 `COALESCE` 兜成空串 —— 前端显示 `-`，接口不报错。
pub(crate) fn file_select() -> String {
    format!(
        "SELECT f.*, {UPLOADER_NAME_EXPR_JOINED} AS uploader_name \
         FROM sys_file f LEFT JOIN sys_user u ON u.id = f.uploaded_by"
    )
}

/// 分页 / 详情读出的文件行：文件元数据 + 上传人显示名。
///
/// `SysFileModel` 只映射 `sys_file` 的列，join 出来的 `uploader_name` 放不进模型，
/// 故包一层。`SELECT f.*, …` 多出的列由派生 `Model::from_row` 的「按列名匹配」兜底
/// 路径忽略（列序与模型不一致时自动回退到按名取值）。
#[derive(Debug, Clone)]
pub(crate) struct FileRow {
    /// 文件元数据。
    pub file: SysFileModel,
    /// 上传人显示名（`COALESCE(real_name, username, '')`，空串 = 用户已不存在）。
    pub uploader_name: String,
}

impl FileRow {
    /// 从查询结果行构造。
    ///
    /// # 异常情况
    ///
    /// 列类型不匹配时返回 [`DbError`]（说明 SQL 与行结构漂移了）。
    fn from_row(row: Row) -> std::result::Result<Self, DbError> {
        // 先取 join 列再交出行所有权给 `SysFileModel::from_row`（它按值消费 Row）
        let uploader_name: String = row.get_typed("uploader_name")?;
        Ok(Self {
            file: SysFileModel::from_row(row)?,
            uploader_name,
        })
    }
}

// ==================== 查询条件与访问函数 ====================

/// 文件分页查询条件。
///
/// `original_name` 保持原始关键词，由本层统一转换为 `LIKE` 模式；`category` /
/// `mime_type` 由处理器去空白后传入，`None` 表示不过滤。
pub(crate) struct FileFilter {
    /// 原始文件名（模糊匹配）。
    pub original_name: Option<String>,
    /// 文件分类（精确匹配）。
    pub category: Option<String>,
    /// MIME 类型（精确匹配）。
    pub mime_type: Option<String>,
}

/// 按内容哈希查询可复用的已存文件（秒传）。
///
/// 命中条件：`file_hash` 相同、未软删、状态为启用。
///
/// # 参数
///
/// - `file_hash`：内容 SHA-256 十六进制摘要。
///
/// # 返回值
///
/// 命中返回已存文件模型，未命中返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub(crate) async fn find_reusable_file(file_hash: &str) -> std::result::Result<Option<SysFileModel>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT * FROM sys_file WHERE file_hash = ? AND deleted_at IS NULL AND status = 1 LIMIT 1",
            vec![Value::from(file_hash)],
        )
        .await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(SysFileModel::from_row(row)?)),
        None => Ok(None),
    }
}

/// 构造分页查询的 SQL 与业务参数（纯函数，便于单测断言不含 `ORDER BY` / `LIMIT`
/// 且 `{FILTER}` 出现在业务过滤条件**之前**）。
///
/// # 参数
///
/// - `filter`：过滤条件（原始文件名模糊、分类 / MIME 精确）。
///
/// # 返回值
///
/// `(base_sql, params)`：`base_sql` 已含 [`file_select`] 的 LEFT JOIN 与 `{FILTER}`
/// 标记，`params` 与 `base_sql` 里 `?` 的出现顺序一致。
fn build_page_query(filter: &FileFilter) -> (String, Vec<Value>) {
    let mut sql = format!("{} WHERE f.deleted_at IS NULL {}", file_select(), scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(filter.original_name.as_deref()) {
        sql.push_str(" AND f.original_name LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(category) = filter.category.as_deref() {
        sql.push_str(" AND f.category = ?");
        params.push(Value::from(category));
    }
    if let Some(mime_type) = filter.mime_type.as_deref() {
        sql.push_str(" AND f.mime_type = ?");
        params.push(Value::from(mime_type));
    }
    (sql, params)
}

/// 分页查询文件记录（含上传人显示名）。
///
/// # 参数
///
/// - `filter`：过滤条件（原始文件名模糊、分类 / MIME 精确）。
/// - `page`：页码（从 1 开始）。
/// - `page_size`：每页条数。
///
/// # 返回值
///
/// 已按 `created_at` 倒序的文件分页结果；每行带 `uploader_name`（ATT-03）。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub(crate) async fn page_files(
    filter: &FileFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> std::result::Result<Page<FileRow>, DbError> {
    // 批次 B · B5（附录 C #11）：`sys_file` 已登记进 SCOPED_TABLES，列表必须按数据范围收口。
    //
    // `sys_file` **没有部门列**，部门形态要先 join `sys_user` 取 `dept_id` —— 那是 Q3 决策
    // 的「下一批（P1）」。本批先用归属列 `uploaded_by` 收口：部门类范围在 `filter_for` 里
    // fail-closed 降级为「仅本人」并置 `is_degraded()`（自检接口可见），所以这里关掉的是
    // 「有 `support:file:list` 就能翻全站文件」这个越权口子，不会把不该看的放开。
    //
    // 主体表别名 `f`（与 `ScopeTarget::SYS_FILE` 一致），片段只锚定主体表（§2.6 规则 1）。
    //
    // ATT-03：`sys_user` 的 LEFT JOIN 在 `{FILTER}` **之前**（见 `build_page_query`），
    // 片段里 `f.uploaded_by` 的 `?` 依旧排在业务过滤参数之前，参数顺序契约不变；
    // 且 `u.id = f.uploaded_by` 是一对一（主键 join），不放大行数，`COUNT(*)` 口径不变。
    let (sql, params) = build_page_query(filter);

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate(
        scope::ScopeTarget::SYS_FILE,
        &sql,
        params,
        Some("ORDER BY f.created_at DESC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let list = list
        .into_iter()
        .map(FileRow::from_row)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 按主键查文件详情（含上传人显示名）。
///
/// 与 [`page_files`] 共用 [`file_select`]，保证列表 / 详情两处列口径一致
/// （ATT-03：详情弹窗右侧「上传人」也要显示名字）。
///
/// # 参数
///
/// - `id`：文件主键。
///
/// # 返回值
///
/// 命中返回文件行，未命中（含已软删）返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub(crate) async fn find_file_with_uploader(id: &uuid::Uuid) -> std::result::Result<Option<FileRow>, DbError> {
    let sql = format!("{} WHERE f.id = ? AND f.deleted_at IS NULL LIMIT 1", file_select());
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    rows.into_iter().next().map(FileRow::from_row).transpose()
}

/// 取单个用户的上传人显示名（上传 / 秒传 / 秒传预检路径用）。
///
/// 这几条路径手里只有 `uploaded_by`，拿不到 [`FileRow`]，用同一条
/// `COALESCE(NULLIF(real_name, ''), username, '')` 口径补上，保证**所有**返回
/// [`crate::support::file::dto::FileRes`] 的接口上传人字段契约一致。
///
/// # 参数
///
/// - `user_id`：用户主键；`None`（历史记录没写上传人）直接返回空串。
///
/// # 返回值
///
/// 显示名；用户不存在（历史脏数据 / 已物理删除）返回空串。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub(crate) async fn uploader_display_name(user_id: Option<uuid::Uuid>) -> std::result::Result<String, DbError> {
    let Some(id) = user_id else {
        return Ok(String::new());
    };
    let mut rows = db()
        .find_with_params(
            &format!("SELECT {UPLOADER_NAME_EXPR_SELF} AS uploader_name FROM sys_user WHERE id = ? LIMIT 1"),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.pop() {
        Some(row) => row.get_typed("uploader_name"),
        None => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    //! SQL 片段断言：join / 别名映射 / 分页守卫（不需要数据库）。

    use super::*;

    /// 分页 / 详情都必须走 `sys_user` 的 LEFT JOIN（**不能** INNER JOIN，
    /// 否则指向脏数据 `uploaded_by` 的文件会整行消失）。
    #[test]
    fn file_select_joins_sys_user_by_left_join() {
        let sql = file_select();
        assert!(
            sql.contains("FROM sys_file f LEFT JOIN sys_user u ON u.id = f.uploaded_by"),
            "file_select 必须带 LEFT JOIN sys_user：{sql}"
        );
        // 主体表在前、user 表在 join 侧：`FROM sys_user` 开头说明写反了
        assert!(sql.trim_start().starts_with("SELECT f.*"), "{sql}");
    }

    /// 别名映射：真名优先 → 用户名 → 空串（空串而非 NULL，前端无需判空）。
    #[test]
    fn file_select_maps_uploader_name_with_fallback_chain() {
        let sql = file_select();
        assert!(
            sql.contains(&format!("{UPLOADER_NAME_EXPR_JOINED} AS uploader_name")),
            "uploader_name 必须按 real_name → username → '' 兜底并取别名：{sql}"
        );
        // 不过滤 u.deleted_at：软删用户的历史文件名仍要能显示
        assert!(
            !sql.contains("u.deleted_at"),
            "不允许对 sys_user 加 deleted_at 过滤（历史文件名会丢）：{sql}"
        );
    }

    /// 两处口径同源：join 版表达式去掉 `u.` 前缀后应与单表版逐字相同。
    #[test]
    fn joined_and_single_user_expressions_are_identical() {
        assert_eq!(UPLOADER_NAME_EXPR_JOINED.replace("u.", ""), UPLOADER_NAME_EXPR_SELF);
        assert!(file_select().contains(UPLOADER_NAME_EXPR_JOINED));
    }

    /// 分页 base_sql：带 join、带 `{FILTER}` 且位于业务过滤条件之前，
    /// **不含** ORDER BY / LIMIT（`scope::paginate` 的硬门禁）。
    #[test]
    fn page_query_keeps_filter_marker_before_business_params() {
        let filter = FileFilter {
            original_name: Some("报表".to_string()),
            category: Some("attachment".to_string()),
            mime_type: Some("application/pdf".to_string()),
        };
        let (sql, params) = build_page_query(&filter);

        assert!(sql.contains("LEFT JOIN sys_user u ON u.id = f.uploaded_by"), "{sql}");
        assert!(sql.contains("AS uploader_name"), "{sql}");
        assert_eq!(
            sql.matches(scope::FILTER_MARKER).count(),
            1,
            "{{FILTER}} 必须恰好一次：{sql}"
        );

        // 片段标记必须排在所有业务 `?` 之前（参数顺序契约：片段参数前置）
        let marker_at = sql.find(scope::FILTER_MARKER).expect("marker");
        let first_business_param = sql.find("f.original_name LIKE ?").expect("business filter");
        assert!(
            marker_at < first_business_param,
            "{{FILTER}} 必须在业务过滤条件之前：{sql}"
        );

        // 三个过滤条件 → 三个业务参数，与 SQL 里 `?` 的个数一致
        assert_eq!(params.len(), 3, "业务参数个数应与 SQL 里的 ? 一致");
        assert_eq!(sql.matches('?').count(), 3, "{sql}");

        // `scope::paginate` 禁止 base_sql 自带排序 / 分页
        assert!(!sql.to_uppercase().contains("ORDER BY"), "{sql}");
        assert!(!sql.to_uppercase().contains("LIMIT"), "{sql}");
    }

    /// 无过滤条件时分页 SQL 仍要有 join 与 `{FILTER}`，且参数为空。
    #[test]
    fn page_query_without_filters() {
        let filter = FileFilter {
            original_name: None,
            category: None,
            mime_type: None,
        };
        let (sql, params) = build_page_query(&filter);
        assert!(sql.contains("LEFT JOIN sys_user u"), "{sql}");
        assert!(sql.contains("f.deleted_at IS NULL"), "{sql}");
        assert_eq!(sql.matches(scope::FILTER_MARKER).count(), 1, "{sql}");
        assert_eq!(params.len(), 0);
        assert!(!sql.contains('?'), "无过滤条件时不该有绑定参数：{sql}");
    }

    /// 详情 SQL 与分页共用同一列清单，且限定主键 + 软删。
    #[test]
    fn detail_query_reuses_same_columns() {
        let detail_sql = format!("{} WHERE f.id = ? AND f.deleted_at IS NULL LIMIT 1", file_select());
        assert!(
            detail_sql.starts_with(&file_select()),
            "详情必须复用 file_select()：{detail_sql}"
        );
        assert!(detail_sql.contains("AS uploader_name"), "{detail_sql}");
        assert!(detail_sql.contains("WHERE f.id = ?"), "{detail_sql}");
        assert!(detail_sql.contains("f.deleted_at IS NULL"), "{detail_sql}");
        assert_eq!(detail_sql.matches('?').count(), 1, "{detail_sql}");
    }
}
