/**
 * 薪资单详情页 API 层（ARCH-9 §5.2 #37/#38/#41/#42/#43/#44/#45 · §6.2.5 · HR-43）。
 *
 * 后端：`salary_sheet_handler.rs`（HR-36 / HR-37 / HR-39 / HR-40）与 `salary_export.rs`（HR-42）。
 * 出参契约、状态词表、动作矩阵、凭证卡纯逻辑在 `./salary-kinds.ts`（可单测），本文件
 * `export *` 出来 —— 页面只认 `@/api/business/hr/salary` 一个入口。
 *
 * | # | 方法 | 路径（后端注解不含 `/api`） | 权限码 | 页面用在哪 |
 * |:--:|---|---|---|---|
 * | 37 | GET  | `/business/hr/salary/{id}` | `salary:detail` | 单头卡 + 汇总卡 + 凭证卡 |
 * | 38 | GET  | `/business/hr/salary/{id}/detail/page` | `salary:detail` | 明细表（`with_items=true` 带出展开项） |
 * | 41 | PUT  | `/business/hr/salary/{id}/detail/{detail_id}` | `salary:calc` | 展开行里「改金额」 |
 * | 42 | POST | `/business/hr/salary/{id}/submit` | `salary:submit` | 操作栏「提交审批」 |
 * | 43 | POST | `/business/hr/salary/{id}/post` | `salary:post` | 操作栏「过账」 |
 * | 44 | POST | `/business/hr/salary/{id}/reverse` | `salary:reverse` | 操作栏「红冲」 |
 * | 45 | GET  | `/business/hr/salary/{id}/export` | `salary:export` | 操作栏「导出工资表」 |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」—— 本仓最高频的前端踩坑（`AGENTS.md` 约定 9）。
 *
 * # 三个「不回 JSON 信封」的细节
 *
 * 1. **导出是二进制**：`responseType: 'blob'`，且要判 `content-type` —— 错误时后端回的是
 *    JSON 信封（400/404/500 也走同一个 response），直接存成 `.xlsx` 用户会得到「打不开的
 *    Excel」（与 `attendance.ts::downloadAttendanceExport` 同一份实现口径）。
 * 2. **提交 / 过账没有请求体**：后端只从令牌取操作人（`Operator`），传 `{}` 也能过，
 *    但不要传 `body: undefined` 之外的东西 —— 传了 `dry_run` 之类会被当成越权参数。
 * 3. **改金额的 `items` 是「编码 + 新金额」**：金额传字符串（`"1200.00"`）避免 JS 浮点把
 *    `0.1+0.2` 那类误差带进 `Decimal`；后端 `rust_decimal` 按字符串精确解析。
 */

import service, { get, post, put, type PageData } from '@/api/request';

import type {
  SalaryDetailFilter,
  SalaryDetailRow,
  SalaryOverridePayload,
  SalaryOverrideResult,
  SalarySheetCard,
  SalarySheetSummary,
} from './salary-kinds';
import { buildDetailQuery } from './salary-kinds';

export * from './salary-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/hr';

// ==================== 读 ====================

/**
 * 单头汇总（`GET /salary/{id}`，#37）。
 *
 * 一次拿齐四段里的三块：**单头全列**（含两张凭证号 / 审批实例 id）、
 * **三口径对账**（`consistent` + 逐字段 `mismatches`）、**部门汇总**。
 *
 * 明细分页**不内嵌**（#38 是独立接口）：一页 200 人的明细塞进来，前端要么不用要么两套
 * 分页互相打架（后端模块文档写明了这个取舍）。
 *
 * `consistent=false` **不是请求失败**：回执照常 200，页面把它当诊断信息显著提示，
 * 而不是整页报错 —— 用户得能打开单据看到「哪几个字段漂了」。
 */
export function fetchSalarySheetSummary(id: string): Promise<SalarySheetSummary> {
  return get<SalarySheetSummary>(`${BASE}/salary/${id}`);
}

/**
 * 明细分页（`GET /salary/{id}/detail/page`，#38）：一人一行，`items` 展开后逐项。
 *
 * `with_items` 恒为 `true`（`buildDetailQuery` 里写死）：展开行直接读回执里的 `items`，
 * 不要一人一次请求 —— 那是本仓反复修过的 N+1。
 */
export function fetchSalaryDetailPage(
  id: string,
  page = 1,
  pageSize = 20,
  filter: SalaryDetailFilter = {},
): Promise<PageData<SalaryDetailRow>> {
  return get<PageData<SalaryDetailRow>>(
    `${BASE}/salary/${id}/detail/page`,
    buildDetailQuery(page, pageSize, filter),
  );
}

// ==================== 写 ====================

/**
 * 改明细里的单项金额（`PUT /salary/{id}/detail/{detail_id}`，#41）。
 *
 * 后端在一个事务里做「锁单 → 锁明细 → 越权闸 → 只读闸（状态）→ 覆盖 → 重算汇总 → 写状态
 * 日志」，回执带**重算后的单头与明细行**：页面拿它直接更新单头合计，不用再打一次汇总
 * （两次读之间别人改一笔，界面就会显示旧合计）。
 *
 * `changed=false` 是幂等重试（传的金额与库里一致），不是失败。
 */
export function updateSalaryDetailItem(
  sheetId: string,
  detailId: string,
  payload: SalaryOverridePayload,
): Promise<SalaryOverrideResult> {
  return put<SalaryOverrideResult>(
    `${BASE}/salary/${sheetId}/detail/${detailId}`,
    payload,
  );
}

/**
 * 提交审批（`POST /salary/{id}/submit`，#42）。
 *
 * 后端一步做完：刷考勤口径 → 过审批前置闸 → 起流程实例 → `CALCULATED → SUBMITTED` →
 * CAS 回写。回执是单头卡（`SalarySheetCard`），页面据此直接改状态标签，不必重查。
 *
 * 失败文案全是中文人话（缺附件 / 期间已关 / 状态不对），调用方直接
 * `MessagePlugin.error(messageOf(error))`。
 */
export function submitSalarySheet(id: string): Promise<SalarySheetCard> {
  return post<SalarySheetCard>(`${BASE}/salary/${id}/submit`);
}

/**
 * 过账（`POST /salary/{id}/post`，#43）。
 *
 * 单一事务：锁单 → 状态闸（`APPROVED`）→ 考勤期间 `LOCKED` → 明细恒等式复算 →
 * 两张凭证 → `APPROVED → POSTED`。**成功后凭证号出现在单头上**（回执里带），
 * 所以页面拿回执就能点亮凭证卡。
 */
export function postSalarySheet(id: string): Promise<SalarySheetCard> {
  return post<SalarySheetCard>(`${BASE}/salary/${id}/post`);
}

/**
 * 红冲（`POST /salary/{id}/reverse`，#44）：原因**必填 ≥5 字**（前端先手拦，
 * 见 `reasonError`）。生成两张红字凭证（计提 + 发放）并把单据打到 `REVERSED` 终态。
 */
export function reverseSalarySheet(id: string, reason: string): Promise<SalarySheetCard> {
  return post<SalarySheetCard>(`${BASE}/salary/${id}/reverse`, { reason });
}

// ==================== 导出（xlsx 二进制） ====================

/**
 * 导出工资表 xlsx（`GET /salary/{id}/export`，#45）。
 *
 * # 为什么用 `service.get` + `responseType: 'blob'`
 *
 * 这个接口回的是二进制 xlsx（不是 JSON 信封）。普通 `get()` 会把二进制当 UTF-8 解成乱码，
 * 再拼 Blob 就是坏文件（解压报「文件已损坏」）。
 *
 * # 为什么还要看 `content-type`
 *
 * 400/404/500 也走同一个 response，回的是 JSON 错误信封。非 `spreadsheetml` /
 * `octet-stream` 的响应先读成文本、抠出 `msg` 再抛 —— 用户看到的是
 * 「薪资单 xxx 不存在」而不是一个打不开的 Excel。
 *
 * # 文件名
 *
 * 后端在 `content-disposition` 里给了 `工资表-2026-11-SAL-202611-000001.xlsx`
 * （`salary_export.rs::file_name`）；这里不回传文件名，页面按单头拼一份即可
 * （`salaryExportFileName`）。
 */
export async function downloadSalaryExport(id: string): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/salary/${id}/export`, {
    responseType: 'blob',
  });
  const contentType = String(response.headers?.['content-type'] ?? '');
  if (contentType.includes('spreadsheetml') || contentType.includes('octet-stream')) {
    return response.data;
  }
  const text = await (response.data as unknown as Blob).text();
  let message = '导出失败';
  try {
    const body = JSON.parse(text) as { msg?: string; message?: string };
    message = body.msg || body.message || message;
  } catch {
    // 不是 JSON：多半是 HTML（路径漏了 `/api` 命中 SPA fallback）
    message = text.includes('<html')
      ? '导出接口返回了页面而不是文件：请检查接口路径前缀'
      : '导出失败';
  }
  throw new Error(message);
}

/**
 * 导出文件名（与后端 `salary_export.rs::file_name` 同构：
 * `工资表-<期间>-<单号>.xlsx`）。
 *
 * 页面上用 `a.download` 落盘，后端响应头里的中文名有时被浏览器忽略；
 * 拼一份保证「同一张单导出两次文件名一致」，HR 归档不会出现 `工资表(1).xlsx`。
 */
export function salaryExportFileName(periodCode: string, sheetNo: string): string {
  const period = (periodCode ?? '').trim() || '未知期间';
  const no = (sheetNo ?? '').trim() || '薪资单';
  return `工资表-${period}-${no}.xlsx`;
}
