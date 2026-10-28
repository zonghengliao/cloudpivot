/**
 * 导入导出接口（批次 A · A3/A4）。
 *
 * 路径与后端 `backend/src/business/impexp/handler.rs` 的 `#[alun::get/post]` 逐条对应；
 * 状态机取值见 `validate.rs` / `model.rs`（字符串大写枚举）。
 *
 * # 导入状态机
 *
 * `PENDING`（已建任务）→ `VALIDATING`（后台校验）→ `VALIDATED`（待确认入库）
 *   → `IMPORTING`（入库中）→ `SUCCESS` / `PARTIAL`；失败分支 `FAILED`，人工取消 `CANCELLED`。
 *
 * # 注意
 *
 * - 模板下载与错误明细下载都返回二进制，必须用 `responseType: 'blob'` 且自行触发保存；
 * - 「下载错误行」返回的是**服务端重新生成的 xlsx**，不是前端拼的 CSV。
 */
import service, { del, get, post, type PageData } from '@/api/request';

/** 导入模式：`STRICT` 严格（默认）/ `PARTIAL` 允许部分导入 */
export type ImportMode = 'STRICT' | 'PARTIAL';

/** 导入任务状态 */
export type ImportTaskStatus =
  | 'PENDING'
  | 'VALIDATING'
  | 'VALIDATED'
  | 'IMPORTING'
  | 'SUCCESS'
  | 'PARTIAL'
  | 'FAILED'
  | 'CANCELLED';

/** 导出任务状态 */
export type ExportTaskStatus = 'PENDING' | 'RUNNING' | 'SUCCESS' | 'FAILED' | 'CANCELLED';

/** 导入 / 导出定义（对应后端 `DefinitionBrief`） */
export interface DefinitionBrief {
  /** 业务编码（如 `support.dict_item`） */
  biz_type: string;
  /** 业务名称（中文） */
  biz_name: string;
  /** 是否支持导入 */
  supports_import: boolean;
  /** 是否支持导出 */
  supports_export: boolean;
  /** 模板版本 */
  template_version: number;
  /** 单次导入行数上限 */
  max_rows: number;
  /** 是否允许部分导入 */
  allow_partial: boolean;
}

/** 结构级警告（对应后端 `ImportWarning`，如 E-IMP-04 / E-IMP-05） */
export interface ImportWarning {
  /** 警告码 */
  code: string;
  /** 面向用户的文案 */
  message: string;
}

/** 单行校验问题（对应后端 `RowIssue` / `ImportErrorRes`） */
export interface ImportErrorRes {
  /** 原始 Excel 行号（含表头，从 1 开始） */
  row_no: number;
  /** 列 key */
  column_key?: string | null;
  /** 列中文表头 */
  column_header?: string | null;
  /** 原始单元格值 */
  raw_value?: string | null;
  /** 错误码（如 `REQUIRED` / `TYPE` / `UNIQUE`） */
  error_code: string;
  /** 失败原因（E-IMP-08 ~ E-IMP-17 文案） */
  error_message: string;
}

/** 预检报告（对应后端 `PrecheckReport`） */
export interface PrecheckReport {
  /** 数据行总数（不含表头、示例行与空行） */
  total: number;
  /** 校验通过行数 */
  valid: number;
  /** 校验失败行数 */
  error: number;
  /** 自动跳过的示例行数 */
  skipped_example: number;
  /** 跳过的空行数 */
  skipped_blank: number;
  /** 结构级警告 */
  warnings: ImportWarning[];
  /** 错误明细（服务端最多返回前 N 条） */
  errors: ImportErrorRes[];
  /** 明细是否被截断 */
  truncated: boolean;
  /** 外键批量校验实际执行的 SQL 条数 */
  fk_sql_statements: number;
  /** 校验耗时（毫秒） */
  elapsed_ms: number;
}

/** 上传并建任务响应（对应后端 `UploadRes`） */
export interface ImportUploadRes {
  /** 任务 id */
  task_id: string;
  /** 任务编号（人可读） */
  task_no: string;
  /** 任务状态 */
  status: string;
  /** 预检报告 */
  report: PrecheckReport;
}

/** 确认入库响应（对应后端 `ConfirmRes`） */
export interface ImportConfirmRes {
  /** 任务 id */
  task_id: string;
  /** 任务状态 */
  status: string;
  /** 已入库行数 */
  imported_rows: number;
  /** 结果说明（「成功 120，失败 0」这类汇总） */
  message: string;
}

/** 导入任务记录（对应后端 `ImportTaskRes`） */
export interface ImportTaskRes {
  /** 任务 id */
  id: string;
  /** 任务编号 */
  task_no: string;
  /** 业务编码 */
  biz_type: string;
  /** 业务名称 */
  biz_name: string;
  /** 原始文件名 */
  file_name?: string | null;
  /** 文件大小（字节） */
  file_size?: number | null;
  /** 导入模式 */
  mode: string;
  /** 任务状态 */
  status: ImportTaskStatus;
  /** 总行数 */
  total_rows: number;
  /** 校验通过行数 */
  valid_rows: number;
  /** 校验失败行数 */
  error_rows: number;
  /** 已入库行数 */
  imported_rows: number;
  /** 进度（0~100） */
  progress: number;
  /** 结果说明 */
  message?: string | null;
  /** 错误摘要（结构由后端决定，前端只做条数展示） */
  error_summary?: unknown;
  /** 创建时间 */
  created_at: string;
  /** 完成时间 */
  finished_at?: string | null;
}

/** 导出任务记录（对应架构文档 §3.8 的导出任务字段） */
export interface ExportTaskRes {
  /** 任务 id */
  id: string;
  /** 任务编号 */
  task_no: string;
  /** 业务编码 */
  biz_type: string;
  /** 业务名称 */
  biz_name: string;
  /** 任务状态 */
  status: ExportTaskStatus;
  /** 总行数 */
  total_rows: number;
  /** 已导出行数 */
  exported_rows: number;
  /** 进度（0~100） */
  progress: number;
  /** 产物文件名 */
  file_name?: string | null;
  /** 产物大小（字节） */
  file_size?: number | null;
  /** 下载链接过期时间（过期后下载返回 410 / E-EXP-05） */
  expire_at?: string | null;
  /** 结果说明 / 失败原因 */
  message?: string | null;
  /** 创建时间 */
  created_at: string;
  /** 完成时间 */
  finished_at?: string | null;
}

/** 导入任务分页查询参数 */
export type ImportPageQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 任务号 / 文件名模糊 */
  keyword?: string;
  /** 状态 */
  status?: string;
  /** 业务编码 */
  biz_type?: string;
  /** 只看我的 */
  only_mine?: boolean;
};

/** 导出任务分页查询参数 */
export type ExportPageQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 任务号 / 文件名模糊 */
  keyword?: string;
  /** 状态 */
  status?: string;
  /** 业务编码 */
  biz_type?: string;
};

/** 查询已注册的导入 / 导出定义 */
export function listDefinitions(): Promise<DefinitionBrief[]> {
  return get<DefinitionBrief[]>('/api/business/impexp/def/list');
}

/** 下载导入模板（xlsx 二进制） */
export async function downloadImportTemplate(bizType: string): Promise<Blob> {
  const response = await service.get<Blob>('/api/business/impexp/import/template', {
    params: { biz_type: bizType },
    responseType: 'blob',
  });
  return response.data;
}

/** 上传 Excel 并建导入任务（返回预检报告） */
export function uploadImportFile(
  file: File,
  bizType: string,
  mode: ImportMode = 'STRICT',
): Promise<ImportUploadRes> {
  const formData = new FormData();
  formData.append('file', file);
  return service
    .post<{ code: number; msg: string; data: ImportUploadRes }>(
      `/api/business/impexp/import/upload?biz_type=${encodeURIComponent(bizType)}&mode=${mode}`,
      formData,
    )
    .then((response) => {
      const body = response.data;
      if (!body || body.code !== 0) {
        throw new Error(body?.msg || '上传失败');
      }
      return body.data;
    });
}

/** 分页查询导入任务 */
export function pageImportTasks(query: ImportPageQuery): Promise<PageData<ImportTaskRes>> {
  return get<PageData<ImportTaskRes>>('/api/business/impexp/import/page', query);
}

/** 查询导入任务详情 */
export function getImportTask(id: string): Promise<ImportTaskRes> {
  return get<ImportTaskRes>(`/api/business/impexp/import/${id}`);
}

/** 分页查询导入错误明细（行号 / 列 / 原因） */
export function pageImportErrors(id: string, page: number, pageSize: number): Promise<PageData<ImportErrorRes>> {
  return get<PageData<ImportErrorRes>>(`/api/business/impexp/import/${id}/errors`, { page, page_size: pageSize });
}

/** 下载「错误行」xlsx */
export async function downloadImportErrors(id: string): Promise<Blob> {
  const response = await service.get<Blob>(`/api/business/impexp/import/${id}/errors/file`, { responseType: 'blob' });
  return response.data;
}

/** 确认入库（`VALIDATED → IMPORTING`） */
export function confirmImport(id: string): Promise<ImportConfirmRes> {
  return post<ImportConfirmRes>(`/api/business/impexp/import/${id}/confirm`);
}

/** 取消导入（仅 `PENDING` / `VALIDATED`） */
export function cancelImport(id: string): Promise<null> {
  return post<null>(`/api/business/impexp/import/${id}/cancel`);
}

/** 失败重试（新建任务，保留原任务） */
export function retryImport(id: string): Promise<ImportTaskRes> {
  return post<ImportTaskRes>(`/api/business/impexp/import/${id}/retry`);
}

/** 同步导出（≤ 5 000 行，直接返回 xlsx） */
export async function exportSync(bizType: string, params?: Record<string, unknown>): Promise<Blob> {
  const response = await service.get<Blob>('/api/business/impexp/export/sync', {
    params: { biz_type: bizType, ...params },
    responseType: 'blob',
  });
  return response.data;
}

/** 建异步导出任务 */
export function createExport(data: {
  biz_type: string;
  format?: string;
  columns?: string[];
  keyword?: string;
  status?: number;
}): Promise<ExportTaskRes> {
  return post<ExportTaskRes>('/api/business/impexp/export', data);
}

/** 分页查询导出任务 */
export function pageExportTasks(query: ExportPageQuery): Promise<PageData<ExportTaskRes>> {
  return get<PageData<ExportTaskRes>>('/api/business/impexp/export/page', query);
}

/** 查询导出任务详情 */
export function getExportTask(id: string): Promise<ExportTaskRes> {
  return get<ExportTaskRes>(`/api/business/impexp/export/${id}`);
}

/** 下载导出产物（过期返回 410） */
export async function downloadExport(id: string): Promise<Blob> {
  const response = await service.get<Blob>(`/api/business/impexp/export/${id}/download`, { responseType: 'blob' });
  return response.data;
}

/** 取消导出（仅 `PENDING`） */
export function cancelExport(id: string): Promise<null> {
  return post<null>(`/api/business/impexp/export/${id}/cancel`);
}

/** 删除导出任务与产物 */
export function deleteExport(id: string): Promise<null> {
  return del<null>(`/api/business/impexp/export/${id}`);
}

/** 导入导出模块接口集合 */
export const impexpApi = {
  listDefinitions,
  downloadImportTemplate,
  uploadImportFile,
  pageImportTasks,
  getImportTask,
  pageImportErrors,
  downloadImportErrors,
  confirmImport,
  cancelImport,
  retryImport,
  exportSync,
  createExport,
  pageExportTasks,
  getExportTask,
  downloadExport,
  cancelExport,
  deleteExport,
};
