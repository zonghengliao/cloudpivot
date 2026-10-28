//! 导入 / 导出任务的数据访问（对应迁移 `V0041` / `V0042`）。
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

use alun::prelude::*;

use crate::business::impexp::definition::RowIssue;
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

/// `sys_export_task` 的强类型视图（A3 只建表，异步 worker 属 A4）。
#[derive(Debug, Clone)]
pub struct ExportTask {
    /// 主键。
    pub id: uuid::Uuid,
    /// 任务号。
    pub task_no: String,
    /// 业务编码。
    pub biz_type: String,
    /// 业务名称。
    pub biz_name: String,
    /// 导出文件名。
    pub export_name: String,
    /// 格式（XLSX / CSV）。
    pub format: String,
    /// 状态。
    pub status: String,
    /// 总行数。
    pub total_rows: i64,
    /// 已导出行数。
    pub exported_rows: i64,
    /// 进度百分比。
    pub progress: i16,
    /// 产物文件 ID。
    pub file_id: Option<uuid::Uuid>,
    /// 下载过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建人。
    pub create_by: Option<uuid::Uuid>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
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
    db().execute_with_params(
        "INSERT INTO sys_import_task \
         (id, task_no, biz_type, biz_name, module_code, file_path, file_sha256, file_name, \
          file_size, mode, status, progress, create_by, update_by, options) \
         VALUES (?::uuid, ?, ?, ?, 'business', ?, ?, ?, ?, ?, 'PENDING', 0, ?::uuid, ?::uuid, \
                 ?::jsonb)",
        vec![
            Value::from(task.id),
            Value::from(task.task_no),
            Value::from(task.biz_type),
            Value::from(task.biz_name),
            Value::from(task.file_path),
            Value::from(task.file_sha256),
            Value::from(task.file_name),
            Value::from(task.file_size),
            Value::from(task.mode),
            match task.operator {
                Some(id) => Value::from(id),
                None => Value::Null,
            },
            match task.operator {
                Some(id) => Value::from(id),
                None => Value::Null,
            },
            match task.options {
                Some(raw) => Value::from(raw),
                None => Value::Null,
            },
        ],
    )
    .await?;
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
         progress = 100, error_summary = ?::jsonb, message = ?, \
         finished_at = CASE WHEN ? IN ('FAILED') THEN CURRENT_TIMESTAMP ELSE finished_at END, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            Value::from(total),
            Value::from(valid),
            Value::from(error),
            match summary {
                Some(raw) => Value::from(raw),
                None => Value::Null,
            },
            match message {
                Some(raw) => Value::from(raw),
                None => Value::Null,
            },
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
         message = COALESCE(?, message), updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            match message {
                Some(raw) => Value::from(raw),
                None => Value::Null,
            },
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
         message = COALESCE(?, message), finished_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(status),
            Value::from(imported),
            match message {
                Some(raw) => Value::from(raw),
                None => Value::Null,
            },
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
pub async fn find_import_task(id: &uuid::Uuid) -> std::result::Result<Option<ImportTask>, DbError> {
    let sql = format!(
        "SELECT {IMPORT_TASK_COLUMNS} FROM sys_import_task WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(*id)]).await?;
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
) -> std::result::Result<Page<ImportTask>, DbError> {
    let mut sql =
        format!("SELECT {IMPORT_TASK_COLUMNS} FROM sys_import_task WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (task_no LIKE ? OR file_name LIKE ? OR biz_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        sql.push_str(" AND biz_type = ?");
        params.push(Value::from(biz_type));
    }
    if let Some(operator) = filter.create_by {
        sql.push_str(" AND create_by = ?::uuid");
        params.push(Value::from(operator));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC");

    let result = db()
        .paginate_with_params(&sql, params, page, page_size)
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
        vec![Value::from(*task_id), Value::from(payload)],
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
    let sql = "SELECT id, row_no, column_key, column_header, raw_value, error_code, error_message \
               FROM sys_import_task_error WHERE task_id = ?::uuid ORDER BY row_no ASC, id ASC";
    let result = db()
        .paginate_with_params(sql, vec![Value::from(*task_id)], page, page_size)
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
fn map_import_task(row: &alun::prelude::Row) -> std::result::Result<ImportTask, DbError> {
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
fn map_task_error(row: &alun::prelude::Row) -> std::result::Result<ImportTaskError, DbError> {
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
}
