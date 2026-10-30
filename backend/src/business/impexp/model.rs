//! 导入 / 导出任务的数据访问（对应迁移 `V0041` / `V0042` / `V0050`）。
//!
//! # 分层
//!
//! 需要写 SQL 的查询集中在本文件；handler 只做参数校验与编排，不出现 SQL。
//!
//! # JSONB 列的读写
//!
//! `error_summary` / `options` 是 JSONB：**写入时**传字符串并 `?::jsonb`（不让驱动猜类型），
//! **读取时**在 SELECT 里 `::text`，再在应用层反序列化——这样不依赖驱动的 JSONB 参数绑定
//! 细节，也避免把「列里存的是什么」耦合到 `TypeMapper` 上。
//!
//! # 明细批量写入
//!
//! 错误明细一次最多 2000 条，用「一个 JSONB 参数 + `jsonb_to_recordset`」一条语句写完
//! （POC-6 T13c 已实测 1000 条批量写正常），不做逐条 INSERT。

use nik::prelude::*;

use crate::business::impexp::definition::RowIssue;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::utils;

/// `sys_import_task` 的强类型视图。
#[derive(Debug, Clone)]
pub struct ImportTask {
    /// 主键。
    pub id: uuid::Uuid,
    /// 任务号（展示用）。
    pub task_no: String,
    /// 业务编码。
    pub biz_type: String,
    /// 业务名称。
    pub biz_name: String,
    /// 上传文件的落盘路径（A4 的异步 worker 也读这里）。
    pub file_path: Option<String>,
    /// 上传文件 SHA-256（完整性核对）。
    pub file_sha256: Option<String>,
    /// 原始文件名。
    pub file_name: Option<String>,
    /// 文件字节数。
    pub file_size: Option<i64>,
    /// 导入模式。
    pub mode: String,
    /// 任务状态。
    pub status: String,
    /// 数据行总数。
    pub total_rows: i32,
    /// 通过校验行数。
    pub valid_rows: i32,
    /// 校验失败行数。
    pub error_rows: i32,
    /// 已入库行数。
    pub imported_rows: i32,
    /// 进度百分比。
    pub progress: i16,
    /// 错误摘要（JSON 文本）。
    pub error_summary: Option<String>,
    /// 消息（失败原因 / 分批说明）。
    pub message: Option<String>,
    /// 创建人。
    pub create_by: Option<uuid::Uuid>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 完成时间。
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 软删标记。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `sys_import_task_error` 的一行。
#[derive(Debug, Clone)]
pub struct ImportTaskError {
    /// 主键。
    pub id: uuid::Uuid,
    /// Excel 真实行号。
    pub row_no: i32,
    /// 列键。
    pub column_key: Option<String>,
    /// 列中文名。
    pub column_header: Option<String>,
    /// 单元格原文。
    pub raw_value: Option<String>,
    /// 错误码。
    pub error_code: String,
    /// 错误文案。
    pub error_message: String,
}

/// 新建导入任务的入参。
pub struct NewImportTask<'a> {
    /// 任务主键。
    pub id: uuid::Uuid,
    /// 任务号。
    pub task_no: &'a str,
    /// 业务编码。
    pub biz_type: &'a str,
    /// 业务名称。
    pub biz_name: &'a str,
    /// 文件落盘路径。
    pub file_path: &'a str,
    /// 文件 SHA-256。
    pub file_sha256: &'a str,
    /// 原始文件名。
    pub file_name: &'a str,
    /// 文件字节数。
    pub file_size: i64,
    /// 导入模式。
    pub mode: &'a str,
    /// 操作人。
    pub operator: Option<uuid::Uuid>,
    /// 选项快照（JSON 文本）。
    pub options: Option<&'a str>,
}

const IMPORT_TASK_COLUMNS: &str = "id, task_no, biz_type, biz_name, file_path, file_sha256, \
     file_name, file_size, mode, status, total_rows, valid_rows, error_rows, imported_rows, \
     progress, error_summary::text AS error_summary, message, create_by, created_at, \
     finished_at, deleted_at";

/// 写一条导入任务（状态 `PENDING`）。
///
/// # 参数
///
/// - `task`：任务字段。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn insert_import_task(task: &NewImportTask<'_>) -> std::result::Result<(), DbError> {
    // 有操作人 / 无操作人两条**常量 SQL**：不能把 `Value::Null` 绑到 `?::uuid`（驱动按
    // Unknown 类型编码会失败：「error serializing parameter」），所以无操作人时直接用 SQL 里的 NULL。
    const SQL_WITH_OPERATOR: &str = "INSERT INTO sys_import_task \
         (id, task_no, biz_type, biz_name, module_code, file_path, file_sha256, file_name, \
          file_size, mode, status, progress, create_by, update_by, options) \
         VALUES (?::uuid, ?, ?, ?, 'business', ?, ?, ?, ?, ?, 'PENDING', 0, ?::uuid, ?::uuid, \
                 ?::jsonb)";
    const SQL_WITHOUT_OPERATOR: &str = "INSERT INTO sys_import_task \
         (id, task_no, biz_type, biz_name, module_code, file_path, file_sha256, file_name, \
          file_size, mode, status, progress, create_by, update_by, options) \
         VALUES (?::uuid, ?, ?, ?, 'business', ?, ?, ?, ?, ?, 'PENDING', 0, NULL, NULL, ?::jsonb)";

    let options = task.options.unwrap_or("{}");
    let mut params = vec![
        Value::from(task.id),
        Value::from(task.task_no),
        Value::from(task.biz_type),
        Value::from(task.biz_name),
        Value::from(task.file_path),
        Value::from(task.file_sha256),
        Value::from(task.file_name),
        Value::from(task.file_size),
        Value::from(task.mode),
    ];
    let sql = match task.operator {
        Some(operator) => {
            params.push(Value::from(operator));
            params.push(Value::from(operator));
            params.push(Value::Json(options.to_string()));
            SQL_WITH_OPERATOR
        }
        None => {
            params.push(Value::Json(options.to_string()));
            SQL_WITHOUT_OPERATOR
        }
    };
    db().execute_with_params(sql, params).await?;
    Ok(())
}

/// 回写预检结果。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `status`：目标状态（`VALIDATED` / `FAILED`）。
/// - `total` / `valid` / `error`：行数统计。
/// - `summary`：错误摘要 JSON（前 100 条）。
/// - `message`：附加说明。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_precheck_result(
    id: &uuid::Uuid,
    status: &str,
    total: i32,
    valid: i32,
    error: i32,
    summary: Option<&str>,
    message: Option<&str>,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_import_task SET status = ?, total_rows = ?, valid_rows = ?, error_rows = ?, \
         progress = 100, error_summary = NULLIF(?::text, '')::jsonb, \
         message = NULLIF(?::text, ''), \
         finished_at = CASE WHEN ? IN ('FAILED') THEN CURRENT_TIMESTAMP ELSE finished_at END, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            Value::from(total),
            Value::from(valid),
            Value::from(error),
            Value::from(summary.unwrap_or("")),
            Value::from(message.unwrap_or("")),
            Value::from(status),
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

/// 更新任务状态。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `status`：目标状态。
/// - `message`：说明（`None` 表示不改）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_status(
    id: &uuid::Uuid,
    status: &str,
    message: Option<&str>,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_import_task SET status = ?, \
         message = COALESCE(NULLIF(?::text, ''), message), updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            Value::from(message.unwrap_or("")),
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

/// 回写入库结果。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `status`：`SUCCESS` / `PARTIAL` / `FAILED`。
/// - `imported`：已入库行数。
/// - `message`：说明（分批失败时写明批次与行号范围）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_import_result(
    id: &uuid::Uuid,
    status: &str,
    imported: i32,
    message: Option<&str>,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_import_task SET status = ?, imported_rows = ?, progress = 100, \
         message = COALESCE(NULLIF(?::text, ''), message), finished_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            Value::from(imported),
            Value::from(message.unwrap_or("")),
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

/// 取消导入任务：`PENDING` / `PARSING` / `VALIDATED` 直接置 `CANCELED`，
/// `IMPORTING` 只打标记（由 `import_confirm` 在批次边界检查后中止）。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// `Some(true)` 立即取消；`Some(false)` 已打取消标记；`None` 当前状态不允许取消。
///
/// # 为什么 `VALIDATED` 可以直接取消
///
/// `VALIDATED` 是「预检完成、等用户点确认」的停顿态，此刻没有任何后台动作在跑，
/// 直接取消不会留下半成品；而 `IMPORTING` 可能正抱着一个批次事务，
/// 强改状态会与正在写入的事务打架，因此只打标记、由批次边界自行收尾。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn cancel_import_task(id: &uuid::Uuid) -> std::result::Result<Option<bool>, DbError> {
    let affected = db()
        .execute_with_params(
            "UPDATE sys_import_task \
             SET status = 'CANCELED', message = '用户取消', \
                 finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND deleted_at IS NULL \
               AND status IN ('PENDING', 'PARSING', 'VALIDATED')",
            vec![Value::from(*id)],
        )
        .await?;
    if affected > 0 {
        return Ok(Some(true));
    }
    let flagged = db()
        .execute_with_params(
            "UPDATE sys_import_task SET cancel_requested = TRUE, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND status = 'IMPORTING' AND deleted_at IS NULL",
            vec![Value::from(*id)],
        )
        .await?;
    if flagged > 0 {
        Ok(Some(false))
    } else {
        Ok(None)
    }
}

/// 读导入任务的取消标记（`import_confirm` 在每个批次前调一次）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn import_cancel_requested(
    id: &uuid::Uuid,
) -> std::result::Result<Option<bool>, DbError> {
    let row = db()
        .find_first_with_params(
            "SELECT cancel_requested FROM sys_import_task WHERE id = ?::uuid",
            vec![Value::from(*id)],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed("cancel_requested")?)),
        None => Ok(None),
    }
}

/// 取消收尾：置 `CANCELED` 并如实记录已入库行数（**进度不写成 100**）。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `imported`：已入库行数。
/// - `message`：说明（写明中止位置）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn finish_import_canceled(
    id: &uuid::Uuid,
    imported: i32,
    message: &str,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_import_task \
         SET status = 'CANCELED', imported_rows = ?, message = ?, \
             finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status IN ('IMPORTING', 'VALIDATED', 'PENDING')",
        vec![
            Value::from(imported),
            Value::from(message),
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

/// 按主键查任务。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// 命中返回任务，未命中或已删除返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn find_import_task(
    id: &uuid::Uuid,
    principal: &ScopedPrincipal,
) -> std::result::Result<Option<ImportTask>, DbError> {
    // 批次 B · B5：单条读取与列表同口径（附录 C #12）。片段追加在业务条件**之后**，
    // 因此参数顺序是「id, 片段参数」（与 SQL 里 `?` 的出现顺序一致）。
    let mut sql = format!(
        "SELECT {IMPORT_TASK_COLUMNS} FROM sys_import_task t WHERE t.id = ?::uuid AND t.deleted_at IS NULL"
    );
    let filter = scope::filter_for(principal, scope::ScopeTarget::SYS_IMPORT_TASK);
    sql.push_str(filter.sql());
    let mut params = vec![Value::from(*id)];
    params.extend(filter.params().iter().cloned());
    let rows = db().find_with_params(&sql, params).await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(map_import_task(&row)?)),
        None => Ok(None),
    }
}

/// 导入任务分页过滤。
#[derive(Debug, Default)]
pub struct ImportTaskFilter {
    /// 关键词（任务号 / 文件名 / 业务名称模糊匹配）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 业务编码。
    pub biz_type: Option<String>,
    /// 只看自己的任务（`None` 表示不限）。
    pub create_by: Option<uuid::Uuid>,
}

/// 导入任务分页。
///
/// # 参数
///
/// - `filter`：过滤条件。
/// - `page` / `page_size`：分页。
///
/// # 返回值
///
/// 按创建时间倒序的分页结果。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn page_import_tasks(
    filter: &ImportTaskFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> std::result::Result<Page<ImportTask>, DbError> {
    // 批次 B · B5（附录 C #12）：`sys_import_task` 登记进数据范围。此前只有 `only_mine`
    // 开关，不开启时任何持 `business:impexp:list` 的人都能看到**全站**导入任务
    // （含他人上传的文件名、行数、错误摘要）—— 这里按 `create_by` 收口。
    // `{FILTER}` 必须在业务条件之前（片段参数恒前置，§2.6）；片段只锚定主体表 `t`。
    let mut sql =
        format!("SELECT {IMPORT_TASK_COLUMNS} FROM sys_import_task t WHERE t.deleted_at IS NULL");
    sql.push_str(scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (t.task_no LIKE ? OR t.file_name LIKE ? OR t.biz_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::from(status));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        sql.push_str(" AND t.biz_type = ?");
        params.push(Value::from(biz_type));
    }
    if let Some(operator) = filter.create_by {
        sql.push_str(" AND t.create_by = ?::uuid");
        params.push(Value::from(operator));
    }

    let result = scope::paginate(
        scope::ScopeTarget::SYS_IMPORT_TASK,
        &sql,
        params,
        Some("ORDER BY t.created_at DESC, t.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let list = result
        .list
        .iter()
        .map(map_import_task)
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok(Page {
        list,
        total: result.total,
        page: result.page,
        size: result.size,
        pages: result.pages,
    })
}

/// 批量写入错误明细（一条语句，JSONB recordset）。
///
/// # 参数
///
/// - `task_id`：任务主键。
/// - `issues`：错误明细（超过 1000 条由调用方分批）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn insert_task_errors(
    task_id: &uuid::Uuid,
    issues: &[RowIssue],
) -> std::result::Result<(), DbError> {
    if issues.is_empty() {
        return Ok(());
    }
    let payload = serde_json::to_string(
        &issues
            .iter()
            .map(|issue| {
                serde_json::json!({
                    "row_no": issue.row_no,
                    "column_key": issue.column_key,
                    "column_header": issue.column_header,
                    "raw_value": issue.raw_value,
                    "error_code": issue.kind.as_str(),
                    "error_message": issue.message,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| DbError::InvalidValue(format!("错误明细序列化失败：{error}")))?;
    db().execute_with_params(
        "INSERT INTO sys_import_task_error \
         (task_id, row_no, column_key, column_header, raw_value, error_code, error_message) \
         SELECT ?::uuid, v.row_no, v.column_key, v.column_header, v.raw_value, v.error_code, \
                v.error_message \
         FROM jsonb_to_recordset(?::jsonb) AS v(row_no int, column_key text, column_header text, \
                raw_value text, error_code text, error_message text)",
        vec![Value::from(*task_id), Value::Json(payload)],
    )
    .await?;
    Ok(())
}

/// 删除某任务的错误明细（重新预检前清空，保证幂等）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn delete_task_errors(task_id: &uuid::Uuid) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "DELETE FROM sys_import_task_error WHERE task_id = ?::uuid",
        vec![Value::from(*task_id)],
    )
    .await?;
    Ok(())
}

/// 错误明细分页（按行号升序）。
///
/// # 参数
///
/// - `task_id`：任务主键。
/// - `page` / `page_size`：分页。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn page_task_errors(
    task_id: &uuid::Uuid,
    page: u64,
    page_size: u64,
) -> std::result::Result<Page<ImportTaskError>, DbError> {
    // `sys_import_task_error` 是导入任务的**子表**（无部门列、无 create_by），它的可见性
    // 完全由父任务的可见性决定：调用方 `handler::import_errors` 先用带 scope 的
    // `load_task` 取任务（越权 → 404），再按 `task_id` 取错误明细。
    // 因此这里只分页、不注入（`unscoped` 显式声明）。SCOPE-EXEMPT: 子表按 task_id 收口，
    // 父任务可见性已由 load_task 的 scope::filter_for(SYS_IMPORT_TASK) 判定。
    let sql = "SELECT id, row_no, column_key, column_header, raw_value, error_code, error_message \
               FROM sys_import_task_error WHERE task_id = ?::uuid";
    let result = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_import_task_error"),
        sql,
        vec![Value::from(*task_id)],
        Some("ORDER BY row_no ASC, id ASC"),
        page,
        page_size,
    )
    .await?;
    let list = result
        .list
        .iter()
        .map(map_task_error)
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok(Page {
        list,
        total: result.total,
        page: result.page,
        size: result.size,
        pages: result.pages,
    })
}

/// 取全部错误明细（「下载错误行」用，上限由调用方控制）。
///
/// # 参数
///
/// - `task_id`：任务主键。
/// - `limit`：最多取多少条。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn list_task_errors(
    task_id: &uuid::Uuid,
    limit: i64,
) -> std::result::Result<Vec<ImportTaskError>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT id, row_no, column_key, column_header, raw_value, error_code, error_message \
             FROM sys_import_task_error WHERE task_id = ?::uuid \
             ORDER BY row_no ASC, id ASC LIMIT ?",
            vec![Value::from(*task_id), Value::from(limit)],
        )
        .await?;
    rows.iter().map(map_task_error).collect()
}

/// 行 → [`ImportTask`]。
fn map_import_task(row: &nik::prelude::Row) -> std::result::Result<ImportTask, DbError> {
    Ok(ImportTask {
        id: row.get_typed("id")?,
        task_no: row.get_typed("task_no")?,
        biz_type: row.get_typed("biz_type")?,
        biz_name: row.get_typed("biz_name")?,
        file_path: row.get_typed("file_path")?,
        file_sha256: row.get_typed("file_sha256")?,
        file_name: row.get_typed("file_name")?,
        file_size: row.get_typed("file_size")?,
        mode: row.get_typed("mode")?,
        status: row.get_typed("status")?,
        total_rows: row.get_typed("total_rows")?,
        valid_rows: row.get_typed("valid_rows")?,
        error_rows: row.get_typed("error_rows")?,
        imported_rows: row.get_typed("imported_rows")?,
        progress: row.get_typed("progress")?,
        error_summary: row.get_typed("error_summary")?,
        message: row.get_typed("message")?,
        create_by: row.get_typed("create_by")?,
        created_at: row.get_typed("created_at")?,
        finished_at: row.get_typed("finished_at")?,
        deleted_at: row.get_typed("deleted_at")?,
    })
}

/// 行 → [`ImportTaskError`]。
fn map_task_error(row: &nik::prelude::Row) -> std::result::Result<ImportTaskError, DbError> {
    Ok(ImportTaskError {
        id: row.get_typed("id")?,
        row_no: row.get_typed("row_no")?,
        column_key: row.get_typed("column_key")?,
        column_header: row.get_typed("column_header")?,
        raw_value: row.get_typed("raw_value")?,
        error_code: row.get_typed("error_code")?,
        error_message: row.get_typed("error_message")?,
    })
}

/// 生成任务号（`IMP` + 日期 + 4 位短码）。
///
/// # 参数
///
/// - `id`：任务主键（取末 4 位十六进制做短码，避免同一秒内重复）。
///
/// # 返回值
///
/// 形如 `IMP20261028-3F2A`。
pub fn build_import_task_no(id: &uuid::Uuid) -> String {
    let simple = id.simple().to_string();
    let suffix = simple
        .get(simple.len().saturating_sub(4)..)
        .unwrap_or("0000")
        .to_ascii_uppercase();
    format!("IMP{}-{suffix}", chrono::Utc::now().format("%Y%m%d"))
}

// ==================== 导出任务（A4） ====================
//
// 与导入任务同样的分层：**所有 SQL 都在这半段**，worker / cleanup / handler 只做编排。
// A3 只建了 `sys_export_task` 表，本卡补齐它的读写与状态机（认领 / 进度 / 完成 / 失败退避 /
// 取消 / 重排队），运行期字段的迁移见 `V0050`。

/// `sys_export_task` 的强类型视图（含 A4 追加的运行期字段）。
#[derive(Debug, Clone)]
pub struct ExportTask {
    /// 主键。
    pub id: uuid::Uuid,
    /// 任务号（展示用）。
    pub task_no: String,
    /// 导出定义编码。
    pub biz_type: String,
    /// 业务名称快照。
    pub biz_name: String,
    /// 导出文件名（不含扩展名）。
    pub export_name: String,
    /// 格式（`XLSX` / `CSV`）。
    pub format: String,
    /// 过滤条件快照（JSON 文本）。
    pub query_params: Option<String>,
    /// 列定义快照（JSON 文本）。
    pub columns_snapshot: Option<String>,
    /// 状态。
    pub status: String,
    /// 总行数。
    pub total_rows: i64,
    /// 已导出行数。
    pub exported_rows: i64,
    /// 进度百分比。
    pub progress: i16,
    /// 产物落盘路径。
    pub file_path: Option<String>,
    /// 产物文件名。
    pub file_name: Option<String>,
    /// 产物字节数。
    pub file_size: Option<i64>,
    /// 产物 SHA-256。
    pub file_sha256: Option<String>,
    /// 下载过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 已尝试次数。
    pub attempt_count: i16,
    /// 最大尝试次数快照。
    pub max_attempts: i16,
    /// 最早可认领时间（退避）。
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 取消请求标记。
    pub cancel_requested: bool,
    /// 认领者（worker 标识）。
    pub locked_by: Option<String>,
    /// 最近心跳。
    pub heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 失败原因 / 分批说明。
    pub message: Option<String>,
    /// 创建人。
    pub create_by: Option<uuid::Uuid>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 开始时间。
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间。
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 软删标记。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ExportTask {
    /// 产物是否已过期（`expire_at <= now`）。
    ///
    /// # 参数
    ///
    /// - `now`：判定时刻（由调用方给，单测可注入固定时间）。
    ///
    /// # 返回值
    ///
    /// 有 `expire_at` 且已到点返回 `true`；`expire_at` 为空（未完成的任务）返回 `false`。
    ///
    /// # 为什么下载接口要用它做「懒判定」
    ///
    /// 清理任务是每天 04:40 跑一次，`SUCCESS` 且过期但还没被扫到的任务行状态仍是 `SUCCESS`。
    /// 下载时先按 `expire_at` 判一次，过期就回 **410** 并顺手置 `EXPIRED`——
    /// 用户不会因为「还没到清理时间」而下载到一个本该过期的文件。
    pub fn is_expired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        matches!(self.expire_at, Some(expire_at) if expire_at <= now)
    }
}

/// 生成 `sys_export_task` 的列清单。
///
/// # 参数
///
/// - `prefix`：列前缀，空串（单表查询）或 `"t."`（`UPDATE ... FROM` 的 `RETURNING`，
///   此时 `pickup` 里也有 `id`，不加前缀会歧义）。
///
/// # 返回值
///
/// 逗号分隔的列清单，JSONB 列统一 `::text` 输出（见模块头「JSONB 列的读写」）。
fn export_task_columns(prefix: &str) -> String {
    format!(
        "{prefix}id, {prefix}task_no, {prefix}biz_type, {prefix}biz_name, {prefix}export_name, \
         {prefix}format, {prefix}query_params::text AS query_params, \
         {prefix}columns_snapshot::text AS columns_snapshot, {prefix}status, {prefix}total_rows, \
         {prefix}exported_rows, {prefix}progress, {prefix}file_path, {prefix}file_name, \
         {prefix}file_size, {prefix}file_sha256, {prefix}expire_at, {prefix}attempt_count, \
         {prefix}max_attempts, {prefix}next_run_at, {prefix}cancel_requested, {prefix}locked_by, \
         {prefix}heartbeat_at, {prefix}message, {prefix}create_by, {prefix}created_at, \
         {prefix}started_at, {prefix}finished_at, {prefix}deleted_at"
    )
}

/// 新建导出任务的入参。
pub struct NewExportTask<'a> {
    /// 任务主键。
    pub id: uuid::Uuid,
    /// 任务号。
    pub task_no: &'a str,
    /// 导出定义编码。
    pub biz_type: &'a str,
    /// 业务名称快照。
    pub biz_name: &'a str,
    /// 导出文件名（不含扩展名）。
    pub export_name: &'a str,
    /// 格式（`XLSX` / `CSV`）。
    pub format: &'a str,
    /// 过滤条件快照（JSON 文本）。
    pub query_params: Option<&'a str>,
    /// 列定义快照（JSON 文本）。
    pub columns_snapshot: Option<&'a str>,
    /// 最大尝试次数（写入任务行做快照）。
    pub max_attempts: i16,
    /// 操作人。
    pub operator: Option<uuid::Uuid>,
}

/// 写一条导出任务（状态 `PENDING`，`next_run_at` 为空表示立即可认领）。
///
/// # 参数
///
/// - `task`：任务字段。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（任务号重复会命中 `uk_sys_export_task_no`）。
pub async fn insert_export_task(task: &NewExportTask<'_>) -> std::result::Result<(), DbError> {
    // 与导入任务同理：`Value::Null` 不能绑到 `?::uuid`，有/无操作人各一条常量 SQL。
    const SQL_WITH_OPERATOR: &str = "INSERT INTO sys_export_task \
         (id, task_no, biz_type, biz_name, module_code, export_name, format, query_params, \
          columns_snapshot, status, progress, max_attempts, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, 'business', ?, ?, ?::jsonb, ?::jsonb, 'PENDING', 0, ?, ?::uuid, ?::uuid)";
    const SQL_WITHOUT_OPERATOR: &str = "INSERT INTO sys_export_task \
         (id, task_no, biz_type, biz_name, module_code, export_name, format, query_params, \
          columns_snapshot, status, progress, max_attempts, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, 'business', ?, ?, ?::jsonb, ?::jsonb, 'PENDING', 0, ?, NULL, NULL)";

    let query_params = task.query_params.unwrap_or("{}");
    let columns_snapshot = task.columns_snapshot.unwrap_or("[]");
    let mut params = vec![
        Value::from(task.id),
        Value::from(task.task_no),
        Value::from(task.biz_type),
        Value::from(task.biz_name),
        Value::from(task.export_name),
        Value::from(task.format),
        Value::Json(query_params.to_string()),
        Value::Json(columns_snapshot.to_string()),
        Value::from(i32::from(task.max_attempts)),
    ];
    let sql = match task.operator {
        Some(operator) => {
            params.push(Value::from(operator));
            params.push(Value::from(operator));
            SQL_WITH_OPERATOR
        }
        None => SQL_WITHOUT_OPERATOR,
    };
    db().execute_with_params(sql, params).await?;
    Ok(())
}

/// 按主键查导出任务。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// 命中返回任务，未命中或已删除返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn find_export_task(
    id: &uuid::Uuid,
    principal: &ScopedPrincipal,
) -> std::result::Result<Option<ExportTask>, DbError> {
    // 批次 B · B5：单条读取与列表同口径（附录 C #13）。
    let mut sql = format!(
        "SELECT {} FROM sys_export_task t WHERE t.id = ?::uuid AND t.deleted_at IS NULL",
        export_task_columns("t.")
    );
    let filter = scope::filter_for(principal, scope::ScopeTarget::SYS_EXPORT_TASK);
    sql.push_str(filter.sql());
    let mut params = vec![Value::from(*id)];
    params.extend(filter.params().iter().cloned());
    let rows = db().find_with_params(&sql, params).await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(map_export_task(&row)?)),
        None => Ok(None),
    }
}

/// 按 ID 装载导出任务，**不做数据范围过滤**（仅供后台 worker 与集成测试）。
///
/// # 为什么允许存在
///
/// 后台任务（`worker` 的调度循环）与集成测试都没有「请求者」这个概念：worker 要处理
/// 全站排队的任务，测试要断言任务被回收后的状态。它们传不进 `ScopedPrincipal`
/// （该类型没有公开构造器，这是闸 1 的一部分），所以只能走这个显式命名的出口。
///
/// **任何面向用户的 handler 都不得调用本函数** —— 用错一次就是越权读；
/// 用户侧一律走 [`find_export_task`]（带 `principal`）。
pub async fn find_export_task_unscoped(
    id: &uuid::Uuid,
) -> std::result::Result<Option<ExportTask>, DbError> {
    let sql = format!(
        "SELECT {} FROM sys_export_task WHERE id = ?::uuid AND deleted_at IS NULL",
        export_task_columns("")
    );
    let rows = db().find_with_params(&sql, vec![Value::from(*id)]).await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(map_export_task(&row)?)),
        None => Ok(None),
    }
}

/// 导出任务分页过滤。
#[derive(Debug, Default)]
pub struct ExportTaskFilter {
    /// 关键词（任务号 / 文件名 / 业务名称模糊匹配）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 业务编码。
    pub biz_type: Option<String>,
    /// 只看自己的任务（`None` 表示不限）。
    pub create_by: Option<uuid::Uuid>,
}

/// 导出任务分页。
///
/// # 参数
///
/// - `filter`：过滤条件。
/// - `page` / `page_size`：分页。
///
/// # 返回值
///
/// 按创建时间倒序的分页结果。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn page_export_tasks(
    filter: &ExportTaskFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> std::result::Result<Page<ExportTask>, DbError> {
    // 批次 B · B5（附录 C #13）：`sys_export_task` 与导入任务同口径，按 `create_by` 收口；
    // 否则 `only_mine=false` 时任何持 `business:impexp:list` 的人都能看到全站导出任务。
    let mut sql = format!(
        "SELECT {} FROM sys_export_task t WHERE t.deleted_at IS NULL",
        export_task_columns("t.")
    );
    sql.push_str(scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (t.task_no LIKE ? OR t.file_name LIKE ? OR t.biz_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::from(status));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        sql.push_str(" AND t.biz_type = ?");
        params.push(Value::from(biz_type));
    }
    if let Some(operator) = filter.create_by {
        sql.push_str(" AND t.create_by = ?::uuid");
        params.push(Value::from(operator));
    }

    let result = scope::paginate(
        scope::ScopeTarget::SYS_EXPORT_TASK,
        &sql,
        params,
        Some("ORDER BY t.created_at DESC, t.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let list = result
        .list
        .iter()
        .map(map_export_task)
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok(Page {
        list,
        total: result.total,
        page: result.page,
        size: result.size,
        pages: result.pages,
    })
}

/// 认领语句（独立成函数，便于单测断言「`SKIP LOCKED` 没被谁顺手删掉」）。
///
/// # 返回值
///
/// 认领导出任务的完整 SQL（`?` 依次为 `worker_id`）。
pub fn claim_export_task_sql() -> String {
    format!(
        "WITH pickup AS ( \
             SELECT id FROM sys_export_task \
             WHERE status = 'PENDING' AND deleted_at IS NULL \
               AND (next_run_at IS NULL OR next_run_at <= CURRENT_TIMESTAMP) \
             ORDER BY created_at ASC, id ASC \
             LIMIT 1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE sys_export_task t \
         SET status = 'RUNNING', \
             started_at = COALESCE(t.started_at, CURRENT_TIMESTAMP), \
             locked_by = ?, \
             heartbeat_at = CURRENT_TIMESTAMP, \
             progress = 0, \
             exported_rows = 0, \
             cancel_requested = FALSE, \
             message = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         FROM pickup \
         WHERE t.id = pickup.id \
         RETURNING {}",
        export_task_columns("t.")
    )
}

/// 认领一条待执行的导出任务（多 worker 并发安全的唯一入口）。
///
/// # 参数
///
/// - `worker_id`：认领者标识（写进 `locked_by`，便于排查「这条任务归谁」）。
///
/// # 返回值
///
/// 认领成功返回该任务（状态已置 `RUNNING`），无任务可认领返回 `None`。
///
/// # 为什么是 `FOR UPDATE SKIP LOCKED`
///
/// 两个 worker 同时执行这条语句时：A 在子查询里锁住候选行，B 的 `SKIP LOCKED` 直接跳过
/// **已加锁的行**（不是等锁），因此 B 拿到的是「下一条」或空集——**同一条任务只会被认领一次**。
/// 单条语句在自动提交事务里执行，语句结束即提交，不会长事务持锁。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn claim_export_task(
    worker_id: &str,
) -> std::result::Result<Option<ExportTask>, DbError> {
    let sql = claim_export_task_sql();
    let rows = db()
        .find_with_params(&sql, vec![Value::from(worker_id)])
        .await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(map_export_task(&row)?)),
        None => Ok(None),
    }
}

/// 回写总行数（认领后统计一次，供进度百分比与前端展示）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn set_export_total(id: &uuid::Uuid, total: i64) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task SET total_rows = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![Value::from(total), Value::from(*id)],
    )
    .await?;
    Ok(())
}

/// 回写进度与心跳（**进度只增不减**）。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `exported`：已导出行数。
/// - `progress`：进度百分比（0–100）。
///
/// # 返回值
///
/// 影响行数：`0` 表示任务已不是 `RUNNING`（被僵尸回收 / 取消），调用方应停止写入。
///
/// # 为什么用 `GREATEST`
///
/// 进度**单调递增**是前端轮询的前提。即使调用方算错、或两个更新的提交顺序颠倒，
/// `GREATEST(progress, ?)` 也保证库里不会出现「从 60% 退回 40%」。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn touch_export_progress(
    id: &uuid::Uuid,
    exported: i64,
    progress: i16,
) -> std::result::Result<u64, DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task \
         SET exported_rows = ?, progress = GREATEST(progress, ?), \
             heartbeat_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![
            Value::from(exported),
            Value::from(i32::from(progress)),
            Value::from(*id),
        ],
    )
    .await
}

/// 只打心跳（不改进度；用于长时间不产出数据块的阶段，如字典装载）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn heartbeat_export_task(id: &uuid::Uuid) -> std::result::Result<u64, DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task SET heartbeat_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![Value::from(*id)],
    )
    .await
}

/// 读取消标记。
///
/// # 返回值
///
/// `Some(true)` 表示收到取消请求；`None` 表示任务行已不存在。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn export_cancel_requested(
    id: &uuid::Uuid,
) -> std::result::Result<Option<bool>, DbError> {
    let row = db()
        .find_first_with_params(
            "SELECT cancel_requested FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(*id)],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed("cancel_requested")?)),
        None => Ok(None),
    }
}

/// 导出成功收尾：写产物、置 `SUCCESS`、算过期时间。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `file_path` / `file_name` / `file_size` / `file_sha256`：产物信息。
/// - `ttl_seconds`：下载有效期（秒）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn finish_export_success(
    id: &uuid::Uuid,
    file_path: &str,
    file_name: &str,
    file_size: i64,
    file_sha256: &str,
    mask_applied: bool,
    ttl_seconds: i64,
) -> std::result::Result<(), DbError> {
    // 过期时刻在应用侧算好再作为 timestamptz 绑定：
    // 不用 `? * INTERVAL '1 second'`——那样 PG 会把参数推断成 double precision，
    // 与驱动的 int 编码对不上（A4 实测踩过，见 worker 失败日志）。
    let expire_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds.max(1));
    db().execute_with_params(
        "UPDATE sys_export_task \
         SET status = 'SUCCESS', progress = 100, file_path = ?, file_name = ?, file_size = ?, \
             file_sha256 = ?, expire_at = ?, mask_applied = ?, \
             locked_by = NULL, heartbeat_at = NULL, message = NULL, \
             finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![
            Value::from(file_path),
            Value::from(file_name),
            Value::from(file_size),
            Value::from(file_sha256),
            Value::from(expire_at),
            Value::Bool(mask_applied),
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

/// 失败收尾：`attempt_count` 自增，未达上限则重新排队（带退避），达上限置 `FAILED`。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `message`：失败原因（写进 `message`）。
/// - `backoff_seconds`：本次退避时长（秒），仅未达上限时生效。
///
/// # 返回值
///
/// `(新状态, 已尝试次数, 最大尝试次数)`；影响行数为 0（任务已不是 `RUNNING`）时返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn fail_export_task(
    id: &uuid::Uuid,
    message: &str,
    backoff_seconds: i64,
) -> std::result::Result<Option<(String, i16, i16)>, DbError> {
    // 同一条语句里 SET 的各表达式都读**旧值**，因此 `attempt_count + 1` 在四处含义一致。
    // 下次可执行时刻同样在应用侧算好再绑定（原因见 `finish_export_success` 的注释）。
    let next_run_at = chrono::Utc::now() + chrono::Duration::seconds(backoff_seconds.max(1));
    let row = db()
        .find_first_with_params(
            "UPDATE sys_export_task \
             SET attempt_count = attempt_count + 1, \
                 status = CASE WHEN attempt_count + 1 >= max_attempts THEN 'FAILED' ELSE 'PENDING' END, \
                 next_run_at = CASE WHEN attempt_count + 1 >= max_attempts THEN NULL ELSE ?::timestamptz END, \
                 finished_at = CASE WHEN attempt_count + 1 >= max_attempts THEN CURRENT_TIMESTAMP ELSE NULL END, \
                 progress = 0, exported_rows = 0, locked_by = NULL, heartbeat_at = NULL, \
                 message = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND status = 'RUNNING' \
             RETURNING status, attempt_count, max_attempts",
            vec![
                Value::from(next_run_at),
                Value::from(message),
                Value::from(*id),
            ],
        )
        .await?;
    match row {
        Some(row) => Ok(Some((
            row.get_typed("status")?,
            row.get_typed("attempt_count")?,
            row.get_typed("max_attempts")?,
        ))),
        None => Ok(None),
    }
}

/// 终态失败收尾：**不重试**，直接把 `attempt_count` 顶到上限并置 `FAILED`。
///
/// # 参数
///
/// - `id`：任务主键。
/// - `message`：失败原因（写进 `message`，运维看得到「为什么不再试」）。
///
/// # 什么时候用
///
/// 重试没有意义的情形：定义已被注销、命中 0 行、超过单任务行数上限。这类原因下一次
/// 执行会**一模一样地失败**，让它走退避重试只是把同样的日志再刷三遍。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn fail_export_task_terminal(
    id: &uuid::Uuid,
    message: &str,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task \
         SET status = 'FAILED', \
             attempt_count = GREATEST(attempt_count, max_attempts), \
             progress = 0, exported_rows = 0, locked_by = NULL, heartbeat_at = NULL, \
             next_run_at = NULL, message = ?, finished_at = CURRENT_TIMESTAMP, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![Value::from(message), Value::from(*id)],
    )
    .await?;
    Ok(())
}

/// worker 侧主动中止（收到取消标记后清收尾）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn mark_export_canceled(
    id: &uuid::Uuid,
    message: &str,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task \
         SET status = 'CANCELED', message = ?, locked_by = NULL, heartbeat_at = NULL, \
             finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'RUNNING'",
        vec![Value::from(message), Value::from(*id)],
    )
    .await?;
    Ok(())
}

/// 请求取消：`PENDING` 直接置 `CANCELED`，`RUNNING` 只打标记。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// `Some(true)` 表示立即取消成功（原地置 `CANCELED`），
/// `Some(false)` 表示已打上取消标记（等待 worker 在批次边界中止），
/// `None` 表示当前状态不允许取消（终态 / 行不存在）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn cancel_export_task(id: &uuid::Uuid) -> std::result::Result<Option<bool>, DbError> {
    let affected = db()
        .execute_with_params(
            "UPDATE sys_export_task \
             SET status = 'CANCELED', message = '用户取消', locked_by = NULL, heartbeat_at = NULL, \
                 finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL",
            vec![Value::from(*id)],
        )
        .await?;
    if affected > 0 {
        return Ok(Some(true));
    }
    let flagged = db()
        .execute_with_params(
            "UPDATE sys_export_task SET cancel_requested = TRUE, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND status = 'RUNNING' AND deleted_at IS NULL",
            vec![Value::from(*id)],
        )
        .await?;
    if flagged > 0 {
        Ok(Some(false))
    } else {
        Ok(None)
    }
}

/// 重新排队（人工「重试」）：把 `FAILED` / `CANCELED` / `EXPIRED` 重置回 `PENDING`。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// 影响行数；`0` 表示当前状态不允许重试（`PENDING` / `RUNNING` / `SUCCESS`）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn requeue_export_task(id: &uuid::Uuid) -> std::result::Result<u64, DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task \
         SET status = 'PENDING', progress = 0, exported_rows = 0, total_rows = 0, \
             attempt_count = 0, next_run_at = NULL, cancel_requested = FALSE, \
             locked_by = NULL, heartbeat_at = NULL, message = NULL, \
             started_at = NULL, finished_at = NULL, file_path = NULL, file_name = NULL, \
             file_size = NULL, file_sha256 = NULL, expire_at = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND deleted_at IS NULL \
           AND status IN ('FAILED', 'CANCELED', 'EXPIRED')",
        vec![Value::from(*id)],
    )
    .await
}

/// 置为过期（下载时懒判定 + 清理任务共用）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn expire_export_task(id: &uuid::Uuid) -> std::result::Result<u64, DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'EXPIRED', file_path = NULL, \
             message = '产物已过期并回收', updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND status = 'SUCCESS'",
        vec![Value::from(*id)],
    )
    .await
}

/// 软删导出任务（物理文件由调用方删除）。
///
/// # 返回值
///
/// 影响行数；`0` 表示任务不存在或正在执行（`RUNNING` 不允许删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn soft_delete_export_task(id: &uuid::Uuid) -> std::result::Result<u64, DbError> {
    db().execute_with_params(
        "UPDATE sys_export_task SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status <> 'RUNNING'",
        vec![Value::from(*id)],
    )
    .await
}

/// 行 → [`ExportTask`]。
fn map_export_task(row: &nik::prelude::Row) -> std::result::Result<ExportTask, DbError> {
    Ok(ExportTask {
        id: row.get_typed("id")?,
        task_no: row.get_typed("task_no")?,
        biz_type: row.get_typed("biz_type")?,
        biz_name: row.get_typed("biz_name")?,
        export_name: row.get_typed("export_name")?,
        format: row.get_typed("format")?,
        query_params: row.get_typed("query_params")?,
        columns_snapshot: row.get_typed("columns_snapshot")?,
        status: row.get_typed("status")?,
        total_rows: row.get_typed("total_rows")?,
        exported_rows: row.get_typed("exported_rows")?,
        progress: row.get_typed("progress")?,
        file_path: row.get_typed("file_path")?,
        file_name: row.get_typed("file_name")?,
        file_size: row.get_typed("file_size")?,
        file_sha256: row.get_typed("file_sha256")?,
        expire_at: row.get_typed("expire_at")?,
        attempt_count: row.get_typed("attempt_count")?,
        max_attempts: row.get_typed("max_attempts")?,
        next_run_at: row.get_typed("next_run_at")?,
        cancel_requested: row.get_typed("cancel_requested")?,
        locked_by: row.get_typed("locked_by")?,
        heartbeat_at: row.get_typed("heartbeat_at")?,
        message: row.get_typed("message")?,
        create_by: row.get_typed("create_by")?,
        created_at: row.get_typed("created_at")?,
        started_at: row.get_typed("started_at")?,
        finished_at: row.get_typed("finished_at")?,
        deleted_at: row.get_typed("deleted_at")?,
    })
}

/// 生成任务号（`EXP` + 日期 + 4 位短码，与导入任务号同一规则）。
///
/// # 参数
///
/// - `id`：任务主键。
///
/// # 返回值
///
/// 形如 `EXP20261028-9C1D`。
pub fn build_export_task_no(id: &uuid::Uuid) -> String {
    let simple = id.simple().to_string();
    let suffix = simple
        .get(simple.len().saturating_sub(4)..)
        .unwrap_or("0000")
        .to_ascii_uppercase();
    format!("EXP{}-{suffix}", chrono::Utc::now().format("%Y%m%d"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_no_has_prefix_and_suffix() {
        let id = uuid::Uuid::now_v7();
        let task_no = build_import_task_no(&id);
        assert!(task_no.starts_with("IMP"));
        assert_eq!(task_no.len(), 3 + 8 + 1 + 4);
        let simple = id.simple().to_string();
        assert!(task_no.ends_with(&simple[simple.len() - 4..].to_ascii_uppercase()));
    }

    /// 构造一个只关心 `expire_at` 的导出任务（其余字段取占位值）。
    fn sample_export_task(expire_at: Option<chrono::DateTime<chrono::Utc>>) -> ExportTask {
        ExportTask {
            id: uuid::Uuid::now_v7(),
            task_no: build_export_task_no(&uuid::Uuid::now_v7()),
            biz_type: "system.dict_item".to_string(),
            biz_name: "字典项".to_string(),
            export_name: "字典项导出".to_string(),
            format: "XLSX".to_string(),
            query_params: None,
            columns_snapshot: None,
            status: "SUCCESS".to_string(),
            total_rows: 0,
            exported_rows: 0,
            progress: 0,
            file_path: None,
            file_name: None,
            file_size: None,
            file_sha256: None,
            expire_at,
            attempt_count: 0,
            max_attempts: 3,
            next_run_at: None,
            cancel_requested: false,
            locked_by: None,
            heartbeat_at: None,
            message: None,
            create_by: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            finished_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn export_task_no_has_exp_prefix() {
        let id = uuid::Uuid::now_v7();
        let task_no = build_export_task_no(&id);
        assert!(task_no.starts_with("EXP"), "{task_no}");
        assert_eq!(task_no.len(), 3 + 8 + 1 + 4);
    }

    #[test]
    fn expired_artifact_is_detected_at_or_after_expire_at() {
        let now = chrono::Utc::now();
        let expired = sample_export_task(Some(now - chrono::Duration::seconds(1)));
        assert!(expired.is_expired(now));
        // 边界：正好等于 expire_at 也算过期（下载不能多给一秒）
        let on_the_edge = sample_export_task(Some(now));
        assert!(on_the_edge.is_expired(now));
        let alive = sample_export_task(Some(now + chrono::Duration::seconds(60)));
        assert!(!alive.is_expired(now));
    }

    #[test]
    fn unfinished_task_is_never_expired() {
        // 未完成的任务没有 expire_at，不能被判成 410
        let task = sample_export_task(None);
        assert!(!task.is_expired(chrono::Utc::now()));
    }

    #[test]
    fn claim_sql_uses_skip_locked_and_guards_state() {
        // 这条用例守的是「并发只认领一次」的地基：SKIP LOCKED 一旦被改成普通 FOR UPDATE
        // 或被人顺手删掉，两个 worker 就会串行阻塞/重复认领，而单测会立刻红
        let sql = claim_export_task_sql();
        assert!(sql.contains("FOR UPDATE SKIP LOCKED"), "{sql}");
        assert!(sql.contains("status = 'PENDING'"), "{sql}");
        assert!(
            sql.contains("next_run_at IS NULL OR next_run_at <= CURRENT_TIMESTAMP"),
            "{sql}"
        );
        assert!(sql.contains("SET status = 'RUNNING'"), "{sql}");
        assert!(sql.contains("RETURNING t.id"), "{sql}");
        // 认领时必须重置上一轮的取消标记与进度，否则「取消过的任务重试」会立刻自杀
        assert!(sql.contains("cancel_requested = FALSE"), "{sql}");
        assert!(sql.contains("progress = 0"), "{sql}");
    }

    #[test]
    fn export_columns_prefix_qualifies_every_column() {
        // UPDATE ... FROM pickup 的 RETURNING 必须带表前缀，否则 id 与 pickup.id 歧义
        let prefixed = export_task_columns("t.");
        for column in ["t.id", "t.task_no", "t.status", "t.heartbeat_at"] {
            assert!(prefixed.contains(column), "缺 {column}: {prefixed}");
        }
        assert!(!prefixed.contains("pickup"), "{prefixed}");
    }
}
