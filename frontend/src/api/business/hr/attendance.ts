/**
 * 考勤页 API 层（ARCH-9 §5.2 #18~#22 + HR-19 补的 #DK · HR-29 前端）。
 *
 * 后端：`attendance_period_handler.rs`（期间 · HR-19）/ `attendance_record_handler.rs`
 * （台账 · HR-20）。出参契约与词表在 `./attendance-kinds.ts`（纯逻辑、可单测），
 * 本文件 `export *` 出来 —— 页面只认 `@/api/business/hr/attendance` 一个入口。
 *
 * | # | 方法 | 路径 | 权限码 | 页面用在哪 |
 * |:--:|---|---|---|---|
 * | 18 | GET    | `/attendance/period/page` | `attendance:list` | 期间 tab 列表 + 台账期间下拉 |
 * | 19 | POST   | `/attendance/period` | `attendance:period` | 新建期间 |
 * | 20 | POST   | `/attendance/period/{id}/status` | `attendance:lock` | 提交 / 锁定 / 解锁 |
 * | 21 | GET    | `/attendance/record/page` | `attendance:record` | 台账列表（按期间） |
 * | 22 | PUT    | `/attendance/record/{id}` | `attendance:record` | 单元格失焦保存（单行修正） |
 * | DK | PUT    | `/attendance/period/{id}` | `attendance:period` | 编辑期间的起止 / 备注 |
 * | DK | DELETE | `/attendance/period/{id}` | `attendance:period` | 删草稿期间（软删） |
 * | DK | POST   | `/attendance/period/{id}/recalc` | `attendance:period` | 汇总重算（幂等可观测） |
 * | 23 | POST   | `/attendance/import` | `attendance:import` | 上传考勤表（走 impexp 通用三段） |
 * | 24 | GET    | `/attendance/export` | `attendance:export` | 导出考勤台账 xlsx（HR-30，含锁定状态列） |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」—— 本仓最高频的前端踩坑。
 *
 * # 导入为什么走 impexp 通用上传
 *
 * `POST /business/hr/attendance/import`（设计 §5.2 #23）尚未注册：父卡 t_8c17e9f5（HR-21）
 * 只交了 `business/impexp/builtin/hr.rs` 的导入定义（`biz_type='hr.attendance'`），
 * 明确把路由与导入后处理留给后续卡。本卡因此复用 impexp 的通用上传 / 预检 / 确认三段
 * （`POST /business/impexp/import/upload` → `POST /business/impexp/import/{id}/confirm`），
 * 差别只是 `biz_type` 固定为 `hr.attendance`。已在卡内上报，等 HR 专属路由落地后
 * 只需换这一个函数体，页面不用动。
 *
 * # 确认入库即重算期间头 —— 页面不要再补 recalc
 *
 * 通用路径**会**重算：`{id}/confirm` 落库的动作就是后端
 * `builtin/hr.rs::AttendanceImport::persist`，它在**同一个事务**里刷期间头的四个汇总列
 * （t_de6e8d41 复核结论；证据 `docs/evidence/2026-11-12-hr22b-import-recalc/` 实测：
 * 空期间 → 上传 2 行 → confirm → 不调任何 recalc 直接读 `period/page`
 * 即得 `employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`）。
 *
 * 所以「确认导入」成功后调用方**只该看到 upload + confirm 两个请求**。HR-29 曾在这里补过
 * 一次 `POST /attendance/period/{id}/recalc`（担心「2 行明细、合计 0 人」），HR-33 已撤掉：
 * 后端没有这个缺口，那次补偿只是多一次请求 + 一段会误导后来人的注释。
 * `/attendance/period/{id}/recalc`（本文件 `recalcPeriod`）保留，但只给期间 tab 的
 * 「重算」按钮做**人工**补偿入口。
 *
 * 回执里**没有** `period_totals`（impexp `ImportConfirmRes` 只有 `imported_rows` / `message`），
 * 导入后的合计一律以重新拉的期间行（`period/page`）为准 —— 页面侧记得把旧缓存作废。
 */

import service, { del, get, post, put, type PageData } from '@/api/request';
import {
  confirmImport,
  downloadImportTemplate,
  pageImportErrors,
  uploadImportFile,
  type DuplicateStrategy,
  type ImportConfirmRes,
  type ImportErrorRes,
  type ImportMode,
  type ImportUploadRes,
} from '@/api/business/impexp';

import {
  ATTENDANCE_BIZ_TYPE,
  type AttendancePeriodRow,
  type AttendanceRecordRow,
  type PeriodRecalcRes,
  type RecordPatchPayload,
  type RecordPatchRes,
} from './attendance-kinds';

export * from './attendance-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/hr';

// ==================== 期间（#18 / #19 / #20 + DK 三条） ====================

/** 期间分页（`GET /attendance/period/page`，权限 `attendance:list`） */
export function fetchPeriodPage(query: Record<string, unknown> = {}): Promise<PageData<AttendancePeriodRow>> {
  return get<PageData<AttendancePeriodRow>>(`${BASE}/attendance/period/page`, query);
}

/** 建期间（`POST /attendance/period`，权限 `attendance:period`；恒为 `DRAFT`） */
export function createPeriod(payload: Record<string, unknown>): Promise<AttendancePeriodRow> {
  return post<AttendancePeriodRow>(`${BASE}/attendance/period`, payload);
}

/** 改期间的起止 / 备注（部分更新；已锁定回 409 + 68011 的人话） */
export function updatePeriod(id: string, payload: Record<string, unknown>): Promise<AttendancePeriodRow> {
  return put<AttendancePeriodRow>(`${BASE}/attendance/period/${id}`, payload);
}

/** 删草稿期间（软删；非草稿或已有记录回 409） */
export function deletePeriod(id: string): Promise<AttendancePeriodRow> {
  return del<AttendancePeriodRow>(`${BASE}/attendance/period/${id}`);
}

/**
 * 状态流转（`POST /attendance/period/{id}/status`，权限 `attendance:lock`）。
 *
 * 三条边：`DRAFT→SUBMITTED` / `SUBMITTED→LOCKED` / `LOCKED→DRAFT`（解锁要 ≥5 字原因）。
 */
export function changePeriodStatus(id: string, payload: Record<string, unknown>): Promise<AttendancePeriodRow> {
  return post<AttendancePeriodRow>(`${BASE}/attendance/period/${id}/status`, payload);
}

/** 汇总重算（幂等；回执带 `before` / `after` / `changed`，界面能看见幂等） */
export function recalcPeriod(id: string): Promise<PeriodRecalcRes> {
  return post<PeriodRecalcRes>(`${BASE}/attendance/period/${id}/recalc`);
}

// ==================== 台账（#21 / #22） ====================

/** 台账分页（`GET /attendance/record/page`；`period_id` 必填，一人一行） */
export function fetchRecordPage(query: Record<string, unknown>): Promise<PageData<AttendanceRecordRow>> {
  return get<PageData<AttendanceRecordRow>>(`${BASE}/attendance/record/page`, query);
}

/**
 * 单行修正（`PUT /attendance/record/{id}`）：**部分更新**，单元格失焦时只发一个字段。
 *
 * 回执含 `changed`（值没变时后端不写库不留痕）、`warnings`（一致性警告，只报不拦）、
 * 以及**同事务重算后的期间汇总**（台账底部合计直接拿这份，不用再打 recalc）。
 */
export function patchRecord(id: string, payload: RecordPatchPayload): Promise<RecordPatchRes> {
  return put<RecordPatchRes>(`${BASE}/attendance/record/${id}`, payload);
}

/**
 * 导出考勤台账 xlsx（`GET /business/hr/attendance/export`，权限 `attendance:export`，HR-30）。
 *
 * 导出的是**屏幕上那张表**：`period_id` 必填，`dept_id` / `keyword` 跟列表页同口径。
 * 后端按数据范围（部门）裁，部门范围的人导不到别部门的行。
 *
 * # 为什么用 `responseType: 'blob'` 而不是 `get()`
 *
 * 这个接口回的是二进制 xlsx（不是 JSON 信封）。用普通 `get()` 会把二进制当 UTF-8 解成
 * 乱码字符串，再拼成 Blob 就是坏文件（解压报「文件已损坏」）。
 *
 * # 为什么还要看一眼 `content-type`
 *
 * 接口在 400/404/500 时回的是 **JSON 错误信封**（`{code,msg}`），也走同一个 `response`。
 * 如果直接把它当 blob 存成 `.xlsx`，用户得到的是一个「打不开的 Excel」而不是
 * 「考勤期间不存在」。所以非 `spreadsheet` 的响应先读成文本、抠出 `msg` 再抛错。
 */
export async function downloadAttendanceExport(
  periodId: string,
  filters: { dept_id?: string; keyword?: string } = {},
): Promise<Blob> {
  const params: Record<string, unknown> = { period_id: periodId };
  if (filters.dept_id) {
    params.dept_id = filters.dept_id;
  }
  if (filters.keyword) {
    params.keyword = filters.keyword;
  }
  const response = await service.get<Blob>(`${BASE}/attendance/export`, {
    params,
    responseType: 'blob',
  });
  const contentType = String(response.headers?.['content-type'] ?? '');
  if (contentType.includes('spreadsheetml') || contentType.includes('octet-stream')) {
    return response.data;
  }
  // 走到这里说明后端回的是错误信封（或命中了 SPA fallback 的 HTML）
  const text = await (response.data as unknown as Blob).text();
  let message = '导出失败';
  try {
    const body = JSON.parse(text) as { msg?: string; message?: string };
    message = body.msg || body.message || message;
  } catch {
    // 不是 JSON：多半是 HTML（路径漏了 `/api` 命中 SPA fallback）
    message = text.includes('<html') ? '导出接口返回了页面而不是文件：请检查接口路径前缀' : '导出失败';
  }
  throw new Error(message);
}

// ==================== 导入（走 impexp 通用三段，见文件头） ====================

/**
 * 上传考勤 Excel（`biz_type='hr.attendance'`）。
 *
 * 上传即同步预检：回执带 `report`（总行数 / 通过 / 失败 / 警告 / 错误行），
 * 页面据此决定「确认入库」还是「先去改 Excel」。
 */
export function uploadAttendanceImport(
  file: File,
  mode: ImportMode = 'STRICT',
  duplicate: DuplicateStrategy = 'UPSERT',
): Promise<ImportUploadRes> {
  return uploadImportFile(file, ATTENDANCE_BIZ_TYPE, mode, duplicate);
}

/**
 * 确认入库（预检通过的行落库；回执带已入库行数与汇总说明）。
 *
 * **入库与期间头重算在同一个事务里**（见文件头），调用方不需要也不应该再补一次
 * `recalcPeriod`。回执不带 `period_totals`，合计以重新拉的期间行为准。
 */
export function confirmAttendanceImport(taskId: string): Promise<ImportConfirmRes> {
  return confirmImport(taskId);
}

/** 下载考勤导入模板（xlsx，含「字段说明」sheet） */
export function downloadAttendanceTemplate(): Promise<Blob> {
  return downloadImportTemplate(ATTENDANCE_BIZ_TYPE);
}

/** 某一批导入的错误明细（行号 / 列 / 原因） */
export function fetchAttendanceImportErrors(
  taskId: string,
  page = 1,
  pageSize = 50,
): Promise<PageData<ImportErrorRes>> {
  return pageImportErrors(taskId, page, pageSize);
}
