/**
 * 员工档案「Excel 导入」的前端契约与纯逻辑（卡 `t_b0808afe`）。
 *
 * 纯逻辑文件：**不 import 请求层、不 import Vue**，`tsx --test` 里可以直接 import 做单测
 * （`tests/unit/hr-employee-import.test.ts` 会读 Rust 源码把业务编码 / 权限码 / 行数上限对拍）。
 * 请求函数在 `./employee.ts`，页面只认 `@/api/business/hr/employee` 一个入口。
 *
 * # 三段链路各自打在哪
 *
 * | 段 | 接口 | 权限码 |
 * |---|---|---|
 * | 下载模板 | `GET /business/hr/employee/import-template` | `business:hr:employee:import` |
 * | 上传 + 预检 | `POST /business/impexp/import/upload?biz_type=hr.employee&mode=PARTIAL&duplicate=SKIP` | `business:impexp:import:upload` |
 * | 确认入库 | `POST /business/impexp/import/{id}/confirm` | `business:impexp:import:confirm` |
 * | 错误行 xlsx | `GET /business/impexp/import/{id}/errors/file` | `business:impexp:import:download` |
 *
 * 模板下载走 HR 自己的路由（人事专员通常没有 impexp 的权限码），上传 / 确认 / 错误行
 * 复用 impexp 的通用三段 —— 页面上「导入员工」按钮因此按**两组**权限一起门禁
 * （见 {@link canUseImport}）。
 *
 * # 为什么上传必须显式传 `PARTIAL` + `SKIP`
 *
 * - `PARTIAL`：花名册里几行写错是常态（部门名抄错一个、身份证少一位）。`STRICT` 下
 *   只要有一行错，确认入库会被后端整批拒绝（框架语义），用户会以为「导入不可用」。
 * - `SKIP`：同一份花名册导第二遍必须**不产生第二个人**。`SKIP` 下后端对「身份证 /
 *   手机号与库里已有相同」的行直接跳过，并在结果里体现为「入库数 < 预检通过数」。
 *
 * # 跳过行数怎么来的（不要自己再猜一遍）
 *
 * 后端 `persist` 返回的是**真正写入**的行数（`ON CONFLICT DO NOTHING` 跳过的行不算），
 * 所以：
 *
 * ```
 * 重复跳过 = 预检通过行数（report.valid） - 确认回执里的 imported_rows
 * ```
 *
 * 见 {@link skippedDuplicateCount}。这个差值是**唯一**可信来源：不要在前端用
 * 「文件里出现过的身份证」自行去重 —— 那样既拿不到跨批次的历史，也会把
 * 「UPSERT 更新」误算成跳过。
 */

import type { ImportConfirmRes, ImportErrorRes, PrecheckReport } from '@/api/business/impexp';

/** 导入定义业务编码（后端 `hr.employee`，`EmployeeImport::biz_type`）。 */
export const EMPLOYEE_IMPORT_BIZ_TYPE = 'hr.employee';

/** 模板下载路径（后端注解不含 `/api`，前端必须写全）。 */
export const EMPLOYEE_IMPORT_TEMPLATE_PATH = '/api/business/hr/employee/import-template';

/** 模板文件名（与后端 `Content-Disposition` 的 `filename*` 同源：`员工档案导入模板_v1.xlsx`）。 */
export const EMPLOYEE_IMPORT_TEMPLATE_FILE = '员工档案导入模板.xlsx';

/** 错误行 xlsx 文件名。 */
export const EMPLOYEE_IMPORT_ERRORS_FILE = '员工档案导入错误行.xlsx';

/** 单次导入行数上限（后端 `EmployeeImport::MAX_ROWS`，卡面：1000 行以内）。 */
export const EMPLOYEE_IMPORT_MAX_ROWS = 1000;

/** 模板列数（卡面 A..AA 27 列 + 末尾追加的「工号」列 = 28）。 */
export const EMPLOYEE_IMPORT_COLUMN_COUNT = 28;

/** 本卡新增的三个权限码（模板下载 / 上传 / 确认 + 错误行下载）。 */
export const EMPLOYEE_IMPORT_PERMS = {
  /** 模板下载 + 页面入口（`V0387` 的「导入员工」按钮码） */
  template: 'business:hr:employee:import',
  /** 上传 + 预检（impexp 通用接口） */
  upload: 'business:impexp:import:upload',
  /** 确认入库 */
  confirm: 'business:impexp:import:confirm',
  /** 下载错误行 xlsx */
  errors: 'business:impexp:import:download',
} as const;

/**
 * 页面上「导入员工」入口是否可用。
 *
 * 模板下载只要 HR 权限码；真正导入要 impexp 的上传 / 确认码，缺一个就会在中途 403，
 * 所以入口按钮按「两组都有」门禁 —— 没有权限时按钮不渲染，而不是点了才报错。
 *
 * @param hasPermission 用户权限判定函数（`userStore.hasPermission`）
 */
export function canUseImport(hasPermission: (code: string) => boolean): boolean {
  return (
    hasPermission(EMPLOYEE_IMPORT_PERMS.template) &&
    hasPermission(EMPLOYEE_IMPORT_PERMS.upload) &&
    hasPermission(EMPLOYEE_IMPORT_PERMS.confirm)
  );
}

/** 错误明细表格列（行号 / 列 / 原因；与 impexp 错误行 xlsx 同口径）。 */
export const EMPLOYEE_IMPORT_ERROR_COLUMNS: {
  colKey: string;
  title: string;
  width?: number;
  minWidth?: number;
  ellipsis?: boolean;
}[] = [
  { colKey: 'row_no', title: 'Excel 行号', width: 96 },
  { colKey: 'column', title: '列', width: 120 },
  { colKey: 'message', title: '原因', minWidth: 260 },
  { colKey: 'raw_value', title: '单元格原值', width: 160, ellipsis: true },
];

/** 预检报告里「可导入 / 有错误」的摘要（预览页顶部一行）。 */
export interface ImportSummary {
  /** 数据行总数（不含示例行与空行） */
  total: number;
  /** 预检通过、可入库的行数 */
  valid: number;
  /** 预检失败的行数 */
  error: number;
  /** 跳过的示例行数 */
  skippedExample: number;
  /** 跳过的空行数 */
  skippedBlank: number;
  /** 摘要文案（给 `t-alert` / `data-testid` 断言用） */
  text: string;
}

/**
 * 预检报告 → 摘要。
 *
 * @param report 上传接口返回的 `report`（可能为空：还没上传）
 */
export function summarizeImport(report: PrecheckReport | null | undefined): ImportSummary {
  const total = report?.total ?? 0;
  const valid = report?.valid ?? 0;
  const error = report?.error ?? 0;
  const skippedExample = report?.skipped_example ?? 0;
  const skippedBlank = report?.skipped_blank ?? 0;
  const parts = [`共 ${total} 行`, `可导入 ${valid} 行`];
  if (error > 0) {
    parts.push(`有错误 ${error} 行`);
  }
  if (skippedExample > 0) {
    parts.push(`跳过示例行 ${skippedExample}`);
  }
  if (skippedBlank > 0) {
    parts.push(`跳过空行 ${skippedBlank}`);
  }
  return { total, valid, error, skippedExample, skippedBlank, text: parts.join(' · ') };
}

/**
 * 「确认导入」按钮能不能点。
 *
 * 三个条件缺一不可：预检回执已拿到、有可入库的行、没有请求在飞。
 * **有错误行也允许确认**（`PARTIAL` 语义：错行不拦合法行）—— 这是与考勤页
 * （`STRICT`）不同的地方，别照抄。
 */
export function canConfirmImport(report: PrecheckReport | null | undefined, busy = false): boolean {
  if (busy) {
    return false;
  }
  return (report?.valid ?? 0) > 0;
}

/**
 * 本次确认真正「按重复跳过」的行数。
 *
 * = 预检通过数 − 实际入库数（后端 `persist` 只把真正写入的行计入返回值）。
 *
 * @param valid 预检通过行数（`report.valid`）
 * @param imported 确认回执里的 `imported_rows`
 */
export function skippedDuplicateCount(valid: number, imported: number): number {
  const skip = valid - imported;
  return skip > 0 ? skip : 0;
}

/** 导入结果（结果页展示用）。 */
export interface ImportOutcome {
  /** 实际入库行数 */
  imported: number;
  /** 因库内重复被跳过的行数 */
  skipped: number;
  /** 预检失败（行内有错，未尝试入库）的行数 */
  failed: number;
  /** 给用户看的一句话（成功 / 部分成功 / 全跳过 三种口吻） */
  text: string;
}

/**
 * 生成结果页文案。
 *
 * @param confirm 确认入库回执（`confirmImport` 的返回值）
 * @param valid 预检通过行数（用来算重复跳过）
 * @param failed 预检失败行数
 */
export function importOutcomeText(confirm: ImportConfirmRes, valid: number, failed: number): ImportOutcome {
  const imported = confirm.imported_rows ?? 0;
  const skipped = skippedDuplicateCount(valid, imported);
  const parts = [`成功 ${imported} 行`];
  if (skipped > 0) {
    // 卡面要求「不报错但明确提示已跳过 N 行（重复）」，所以这句话单独成段
    parts.push(`已跳过 ${skipped} 行（重复）`);
  }
  if (failed > 0) {
    parts.push(`失败 ${failed} 行`);
  }
  return { imported, skipped, failed, text: parts.join(' · ') };
}

/**
 * 结果页「下载错误行」是否可用（有错误行才给按钮）。
 *
 * @param failed 预检失败行数
 */
export function canDownloadErrors(failed: number): boolean {
  return failed > 0;
}

/**
 * 选中文件的即时校验（大小 / 扩展名），返回错误文案；通过返回空串。
 *
 * 5MB 与后端的 `[upload] max_size_mb` 同口径（后端也有一道，这里只是让用户
 * 少等一次往返）。
 *
 * @param file 用户选中的文件
 */
export function validateImportFile(file: File | null | undefined): string {
  if (!file) {
    return '';
  }
  const name = file.name.toLowerCase();
  if (!name.endsWith('.xlsx') && !name.endsWith('.xls')) {
    return '只支持 .xlsx / .xls 文件，请用「下载导入模板」得到的模板填写';
  }
  const limit = 5 * 1024 * 1024;
  if (file.size > limit) {
    return `文件超过 5MB（当前 ${(file.size / 1024 / 1024).toFixed(1)}MB），请拆分后分批导入`;
  }
  if (file.size === 0) {
    return '文件是空的，请确认已保存内容';
  }
  return '';
}

/** 错误明细的列名（后端 `column_header` 优先，缺了回退 `column_key`）。 */
export function errorColumnLabel(issue: ImportErrorRes): string {
  return issue.column_header || issue.column_key || '整行';
}

/** 错误明细的原因（后端 `message` 优先，回退 `error_message`）。 */
export function errorMessageText(issue: ImportErrorRes): string {
  return issue.message || issue.error_message || '-';
}
