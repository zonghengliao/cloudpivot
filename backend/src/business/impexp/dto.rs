//! 导入导出的请求 / 响应 DTO。
//!
//! 约定（与底座一致）：请求 DTO 后缀 `Req` / 查询后缀 `Query`，响应 DTO 后缀 `Res`；
//! 响应里只暴露前端需要的字段，模型内部字段（如 `file_path`）不外泄。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::impexp::model::{ExportTask, ImportTask, ImportTaskError};
use crate::business::impexp::validate::PrecheckReport;

/// 模板下载查询。
#[derive(Debug, Deserialize)]
pub struct TemplateQuery {
    /// 业务编码。
    pub biz_type: String,
}

/// 上传预检查询。
#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// 业务编码。
    pub biz_type: String,
    /// 导入模式：`STRICT`（默认）/ `PARTIAL`。
    pub mode: Option<String>,
    /// 重复数据策略：`UPSERT`（默认，按编码更新）/ `SKIP`（已存在则跳过）。
    ///
    /// 快照进 `sys_import_task.options.duplicate_strategy`，由业务定义的 `persist` 读取
    /// （见 `model::import_task_options`）。
    pub duplicate: Option<String>,
}

/// 同步导出查询。
#[derive(Debug, Deserialize)]
pub struct ExportSyncQuery {
    /// 业务编码。
    pub biz_type: String,
    /// 关键词（业务自定义解释）。
    pub keyword: Option<String>,
    /// 状态过滤（1 启用 / 0 停用）。
    pub status: Option<i16>,
}

/// 导入任务分页查询。
#[derive(Debug, Deserialize)]
pub struct ImportPageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（任务号 / 文件名 / 业务名）。
    pub keyword: Option<String>,
    /// 状态过滤。
    pub status: Option<String>,
    /// 业务编码过滤。
    pub biz_type: Option<String>,
    /// 只看自己的任务（默认 `true`，PRD D9：导入任务按创建人隔离）。
    pub only_mine: Option<bool>,
}

/// 错误明细分页查询。
#[derive(Debug, Deserialize)]
pub struct ErrorPageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 上传 + 预检结果。
#[derive(Debug, Serialize)]
pub struct UploadRes {
    /// 任务 ID。
    pub task_id: String,
    /// 任务号。
    pub task_no: String,
    /// 任务状态（`VALIDATED`）。
    pub status: String,
    /// 预检报告（总计 / 通过 / 失败 / 明细 / 外键 SQL 次数 / 耗时）。
    pub report: PrecheckReport,
}

/// 确认入库结果。
#[derive(Debug, Serialize)]
pub struct ConfirmRes {
    /// 任务 ID。
    pub task_id: String,
    /// 终态（`SUCCESS` / `PARTIAL`）。
    pub status: String,
    /// 实际入库行数。
    pub imported_rows: i32,
    /// 说明。
    pub message: String,
}

/// 导入任务详情。
#[derive(Debug, Serialize)]
pub struct ImportTaskRes {
    /// 任务 ID。
    pub id: String,
    /// 任务号。
    pub task_no: String,
    /// 业务编码。
    pub biz_type: String,
    /// 业务名称。
    pub biz_name: String,
    /// 原始文件名。
    pub file_name: Option<String>,
    /// 文件字节数。
    pub file_size: Option<i64>,
    /// 导入模式。
    pub mode: String,
    /// 状态。
    pub status: String,
    /// 数据行总数。
    pub total_rows: i32,
    /// 通过校验行数。
    pub valid_rows: i32,
    /// 失败行数。
    pub error_rows: i32,
    /// 已入库行数。
    pub imported_rows: i32,
    /// 进度。
    pub progress: i16,
    /// 说明。
    pub message: Option<String>,
    /// 错误摘要（前 100 条，JSON）。
    pub error_summary: Option<serde_json::Value>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 完成时间。
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ImportTaskRes {
    /// 由模型转换（`file_path` / `file_sha256` 等内部字段不外泄）。
    ///
    /// # 参数
    ///
    /// - `task`：任务模型。
    pub fn from_model(task: ImportTask) -> Self {
        let error_summary = task
            .error_summary
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        Self {
            id: task.id.to_string(),
            task_no: task.task_no,
            biz_type: task.biz_type,
            biz_name: task.biz_name,
            file_name: task.file_name,
            file_size: task.file_size,
            mode: task.mode,
            status: task.status,
            total_rows: task.total_rows,
            valid_rows: task.valid_rows,
            error_rows: task.error_rows,
            imported_rows: task.imported_rows,
            progress: task.progress,
            message: task.message,
            error_summary,
            created_at: task.created_at,
            finished_at: task.finished_at,
        }
    }
}

/// 错误明细行。
#[derive(Debug, Serialize)]
pub struct ImportErrorRes {
    /// Excel 真实行号。
    pub row_no: i32,
    /// 列键。
    pub column_key: Option<String>,
    /// 列中文名。
    pub column_header: Option<String>,
    /// 单元格原文。
    pub raw_value: Option<String>,
    /// 错误码（REQUIRED / TYPE / UNIQUE / FK_NOT_FOUND …）。
    pub error_code: String,
    /// 错误原因。
    pub error_message: String,
}

impl ImportErrorRes {
    /// 由模型转换。
    ///
    /// # 参数
    ///
    /// - `error`：明细模型。
    pub fn from_model(error: ImportTaskError) -> Self {
        Self {
            row_no: error.row_no,
            column_key: error.column_key,
            column_header: error.column_header,
            raw_value: error.raw_value,
            error_code: error.error_code,
            error_message: error.error_message,
        }
    }
}

// ==================== 异步导出 / 任务操作（A4） ====================

/// 创建异步导出任务。
#[derive(Debug, Deserialize)]
pub struct ExportCreateReq {
    /// 导出定义编码。
    pub biz_type: String,
    /// 关键词（业务自定义解释）。
    pub keyword: Option<String>,
    /// 状态过滤（1 启用 / 0 停用）。
    pub status: Option<i16>,
    /// 格式（`XLSX` / `CSV`；缺省按行数自动选：≤ 20 万行 XLSX，超过 CSV）。
    pub format: Option<String>,
    /// 导出文件名（不含扩展名；缺省 `业务名_时间戳`）。
    pub export_name: Option<String>,
}

/// 导出任务分页查询。
#[derive(Debug, Deserialize)]
pub struct ExportPageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（任务号 / 文件名 / 业务名）。
    pub keyword: Option<String>,
    /// 状态过滤。
    pub status: Option<String>,
    /// 业务编码过滤。
    pub biz_type: Option<String>,
    /// 只看自己的任务（默认 `true`）。
    pub only_mine: Option<bool>,
}

/// 导出任务详情 / 列表项。
#[derive(Debug, Serialize)]
pub struct ExportTaskRes {
    /// 任务 ID。
    pub id: String,
    /// 任务号。
    pub task_no: String,
    /// 业务编码。
    pub biz_type: String,
    /// 业务名称。
    pub biz_name: String,
    /// 导出文件名（不含扩展名）。
    pub export_name: String,
    /// 格式。
    pub format: String,
    /// 状态。
    pub status: String,
    /// 预计总行数。
    pub total_rows: i64,
    /// 已导出行数。
    pub exported_rows: i64,
    /// 进度百分比（**单调递增**，100 表示产物已落盘）。
    pub progress: i16,
    /// 产物文件名。
    pub file_name: Option<String>,
    /// 产物字节数。
    pub file_size: Option<i64>,
    /// 产物 SHA-256（下载后可比对）。
    pub file_sha256: Option<String>,
    /// 下载过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 已尝试次数。
    pub attempt_count: i16,
    /// 最大尝试次数。
    pub max_attempts: i16,
    /// 失败原因 / 排队说明。
    pub message: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 开始时间。
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间。
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ExportTaskRes {
    /// 由模型转换（`file_path` / `query_params` 等内部字段不外泄）。
    ///
    /// # 参数
    ///
    /// - `task`：导出任务模型。
    pub fn from_model(task: ExportTask) -> Self {
        Self {
            id: task.id.to_string(),
            task_no: task.task_no,
            biz_type: task.biz_type,
            biz_name: task.biz_name,
            export_name: task.export_name,
            format: task.format,
            status: task.status,
            total_rows: task.total_rows,
            exported_rows: task.exported_rows,
            progress: task.progress,
            file_name: task.file_name,
            file_size: task.file_size,
            file_sha256: task.file_sha256,
            expire_at: task.expire_at,
            attempt_count: task.attempt_count,
            max_attempts: task.max_attempts,
            message: task.message,
            created_at: task.created_at,
            started_at: task.started_at,
            finished_at: task.finished_at,
        }
    }
}

/// 任务操作结果（取消 / 重试 / 删除）。
#[derive(Debug, Serialize)]
pub struct TaskActionRes {
    /// 任务 ID。
    pub task_id: String,
    /// 操作后的状态。
    pub status: String,
    /// 说明（如「已打上取消标记，worker 将在批次边界中止」）。
    pub message: String,
}
