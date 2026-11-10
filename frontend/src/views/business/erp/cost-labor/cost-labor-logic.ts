/**
 * 工时台账页纯逻辑（COST-B2-13 / ARCH-9 §6.2.4 · §4.5.3 · §5.2 #10–#16）。
 *
 * 这里只放**不碰 DOM、不碰请求**的判定：表单校验、payload 拼装（编辑只带真改过的字段）、
 * 行级按钮态、金额提示、按成本对象分组汇总、导入回执文案。`.vue` 只负责画。
 *
 * # 为什么必须把这些写成函数
 *
 * 卡面验收里的「工时台账页 + 导入按钮」看着是画页面，实际风险全在口径：
 * * 金额是 `round(hours × rate, 4)`，前端**不能**当入参提交（给错了后端 `400 + 66058`）；
 * * `rate` 留空 = 后端自动取数，三级全空 ⇒ `422 + 66059`（**不按 0 计**）；
 * * 编辑是**部分更新**，整表回传会把 `null` 当"清空"写掉；
 * * 只有 `DRAFT` 能改 / 删 / 确认。
 *
 * 这些判据一旦漂移（把 `v-if` 改成 `:disabled`、PUT 整表回传），`vue-tsc` 抓不到。
 *
 * # 数值口径
 *
 * 所有累加走**整数定点**（{@link sumDecimal}）：`hours` 是 2 位、`amount` 是 4 位，
 * 字符串直接相加会得到 `"8.00" + "2.00" = "8.002.00"`，`Number` 相加又会在
 * `0.1 + 0.2` 处露馅。分组汇总的"合计"必须与后端分页的口径一致。
 */

import {
  AMOUNT_SCALE,
  COST_LABOR_STATUS_LABELS,
  COST_LABOR_SOURCE_LABELS,
  type CostLaborCreatePayload,
  type CostLaborRow,
  type CostLaborStatus,
  type CostLaborUpdatePayload,
  type ImportIssue,
} from '@/api/business/erp/cost-labor-kinds';

/** 期间串（`yyyy-MM`） */
export function isPeriodKey(value: string): boolean {
  return /^\d{4}-(0[1-9]|1[0-2])$/.test(value.trim());
}

/** 日期串（`YYYY-MM-DD`） */
export function isDateString(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value.trim());
}

/** 日期是否落在期间内（后端 `ensure_date_in_period` 同口径：按 `yyyy-MM` 前缀） */
export function isDateInPeriod(date: string, period: string): boolean {
  return isDateString(date) && isPeriodKey(period) && date.slice(0, 7) === period;
}

/**
 * 小数文本是否合法且不超过 `scale` 位小数（实现与说明见 `shared/decimal.ts`）。
 */
import { formatScaled, isDecimalWithinScale, scaledInt, sumDecimal } from '@/views/business/erp/shared/decimal';

export {
  decimalEquals,
  formatScaled,
  isDecimalWithinScale,
  ratio,
  scaledInt,
  sumDecimal,
} from '@/views/business/erp/shared/decimal';

/** 正数（> 0）且精度合法 */
export function isPositiveDecimal(value: string, scale: number): boolean {
  if (!isDecimalWithinScale(value, scale)) {
    return false;
  }
  return Number(value) > 0;
}

/** 非负数（≥ 0）且精度合法 */
export function isNonNegativeDecimal(value: string, scale: number): boolean {
  if (!isDecimalWithinScale(value, scale)) {
    return false;
  }
  return Number(value) >= 0;
}

/**
 * 预计金额（**只做提示，不提交**）。
 *
 * 口径与后端 `expected_amount` 完全一致：`round(hours × rate, 4)`，半数远离零。
 * 用整数定点算，避免 `8 × 33.333333` 在 double 上落到 `266.666664`。
 */
export function expectedAmount(hours: string, rate: string): string {
  const hoursInt = scaledInt(hours, 4);
  const rateInt = scaledInt(rate, 6);
  // hours(4dp) × rate(6dp) → 定点 10 位；再收到 4 位（半数远离零，与后端一致）
  const product = hoursInt * rateInt;
  if (!Number.isFinite(product)) {
    return formatScaled(0, AMOUNT_SCALE);
  }
  const sign = product < 0 ? -1 : 1;
  const scaled4 = Math.round(Math.abs(product) / 1e6);
  return formatScaled(sign * scaled4, AMOUNT_SCALE);
}

// ==================== 表单 ====================

/** 表单状态（字符串原样收，提交前不转 number） */
export interface LaborFormState {
  /** 会计期间 */
  period_code: string;
  /** 工作日期 */
  work_date: string;
  /** 成本对象 */
  cost_object_id: string;
  /** 员工姓名 */
  employee_name: string;
  /** 工作中心 */
  dept_id: string;
  /** 人工工时 */
  hours: string;
  /** 机器工时 */
  machine_hours: string;
  /** 工资率（留空 = 自动取数） */
  rate: string;
  /** 备注 */
  remark: string;
}

/** 表单错误（字段 → 文案；空对象 = 没问题） */
export type LaborFormErrors = Partial<Record<keyof LaborFormState, string>>;

/** 空表单（`today` = `YYYY-MM-DD`，期间默认取它的前 7 位） */
export function emptyLaborForm(today: string): LaborFormState {
  return {
    period_code: today.slice(0, 7),
    work_date: today,
    cost_object_id: '',
    employee_name: '',
    dept_id: '',
    hours: '',
    machine_hours: '',
    rate: '',
    remark: '',
  };
}

/** 列表行 → 表单（编辑用） */
export function formFromRow(row: CostLaborRow): LaborFormState {
  return {
    period_code: row.period_code,
    work_date: row.work_date,
    cost_object_id: row.cost_object_id,
    employee_name: row.employee_name,
    dept_id: row.dept_id,
    hours: row.hours,
    machine_hours: row.machine_hours,
    rate: row.rate,
    remark: row.remark ?? '',
  };
}

/** 表单校验（文案与后端 400 对齐，尽量说清"怎么修"） */
export function validateLaborForm(form: LaborFormState): LaborFormErrors {
  const errors: LaborFormErrors = {};
  if (!isPeriodKey(form.period_code)) {
    errors.period_code = '会计期间格式是 yyyy-MM（如 2026-11）';
  }
  if (!isDateString(form.work_date)) {
    errors.work_date = '工作日期必须选（YYYY-MM-DD）';
  } else if (isPeriodKey(form.period_code) && !isDateInPeriod(form.work_date, form.period_code)) {
    errors.work_date = `工作日期 ${form.work_date} 不在会计期间 ${form.period_code} 内（期间归属由日期决定，跨月请分两行录）`;
  }
  if (!form.cost_object_id) {
    errors.cost_object_id = '成本对象必须选（只有「在产」的对象能报工）';
  }
  if (!form.employee_name.trim()) {
    errors.employee_name = '员工姓名必须填（姓名快照，删号后也能回溯是谁的工时）';
  }
  if (!form.dept_id) {
    errors.dept_id = '工作中心（部门）必须选';
  }
  if (!form.hours.trim()) {
    errors.hours = '人工工时必须填';
  } else if (!isPositiveDecimal(form.hours, 2)) {
    errors.hours = '人工工时必须是大于 0 的数字，最多 2 位小数（库列是 NUMERIC(10,2)，超精度会被拒）';
  }
  if (form.machine_hours.trim() && !isNonNegativeDecimal(form.machine_hours, 2)) {
    errors.machine_hours = '机器工时可空，填就是 ≥ 0 的数字，最多 2 位小数';
  }
  if (form.rate.trim() && !isNonNegativeDecimal(form.rate, 6)) {
    errors.rate = '工资率可空（留空按 对象 > 车间 > 公司 自动取数）；填就是 ≥ 0 的数字，最多 6 位小数';
  }
  if (form.remark.length > 255) {
    errors.remark = '备注最长 255 个字符';
  }
  return errors;
}

/** 错误条数（页面 `errorCount(errors) > 0` 才拦提交） */
export function errorCount(errors: LaborFormErrors): number {
  return Object.keys(errors).length;
}

/** 表单 → 新建入参（**不带 `amount`**：金额一律服务端算） */
export function toCreatePayload(form: LaborFormState): CostLaborCreatePayload {
  const payload: CostLaborCreatePayload = {
    period_code: form.period_code.trim(),
    work_date: form.work_date.trim(),
    cost_object_id: form.cost_object_id,
    employee_name: form.employee_name.trim(),
    dept_id: form.dept_id,
    hours: form.hours.trim(),
  };
  const machineHours = form.machine_hours.trim();
  if (machineHours) {
    payload.machine_hours = machineHours;
  }
  const rate = form.rate.trim();
  if (rate) {
    payload.rate = rate;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/**
 * 表单 → 编辑入参（**只带真改过的字段**）。
 *
 * 后端的 PUT 是部分更新：字段缺失 = 不改、显式 `null` = 清空。整表回传会把用户
 * 没动过的字段重写一遍（`rate` 尤其危险：编辑不自动重取，回传空串会被当"清空"）。
 */
export function toUpdatePayload(origin: CostLaborRow, form: LaborFormState): CostLaborUpdatePayload {
  const payload: CostLaborUpdatePayload = {};
  if (form.period_code.trim() !== origin.period_code) {
    payload.period_code = form.period_code.trim();
  }
  if (form.work_date.trim() !== origin.work_date) {
    payload.work_date = form.work_date.trim();
  }
  if (form.cost_object_id !== origin.cost_object_id) {
    payload.cost_object_id = form.cost_object_id;
  }
  if (form.employee_name.trim() !== origin.employee_name) {
    payload.employee_name = form.employee_name.trim();
  }
  if (form.dept_id !== origin.dept_id) {
    payload.dept_id = form.dept_id;
  }
  if (form.hours.trim() !== origin.hours) {
    payload.hours = form.hours.trim();
  }
  if (form.machine_hours.trim() !== origin.machine_hours) {
    payload.machine_hours = form.machine_hours.trim();
  }
  if (form.rate.trim() !== origin.rate) {
    payload.rate = form.rate.trim();
  }
  if (form.remark.trim() !== (origin.remark ?? '')) {
    payload.remark = form.remark.trim();
  }
  return payload;
}

/** 有没有真改动（PATCH 语义为空 ⇒ 页面提示"没有改动"，不发请求） */
export function isEmptyPayload(payload: CostLaborUpdatePayload): boolean {
  return Object.keys(payload).length === 0;
}

// ==================== 行级按钮态 ====================

/** 状态中文（未知值原样返回，不吞） */
export function statusLabel(status: string): string {
  return COST_LABOR_STATUS_LABELS[status as CostLaborStatus] ?? status;
}

/** 状态主题（TDesign tag theme） */
export function statusTheme(status: string): 'default' | 'success' | 'warning' {
  if (status === 'CONFIRMED') {
    return 'success';
  }
  if (status === 'DRAFT') {
    return 'warning';
  }
  return 'default';
}

/** 来源中文 */
export function sourceLabel(source: string): string {
  return COST_LABOR_SOURCE_LABELS[source as keyof typeof COST_LABOR_SOURCE_LABELS] ?? source;
}

/** 能不能改（只有 DRAFT；后端 409 同口径） */
export function canEditRow(row: Pick<CostLaborRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 能不能删（只有 DRAFT） */
export function canDeleteRow(row: Pick<CostLaborRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 能不能确认（只有 DRAFT；已确认再点也是幂等成功，但按钮该置灰） */
export function canConfirmRow(row: Pick<CostLaborRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 对象是否已关批（页面要标出来：已关批仍可看历史，但通常不该再报工） */
export function objectClosed(row: Pick<CostLaborRow, 'cost_object_status'>): boolean {
  return row.cost_object_status === 'CLOSED' || row.cost_object_status === 'CANCELLED';
}

// ==================== 按成本对象分组（§6.2.4 第一条） ====================

/** 分组汇总行（车间统计员按批核对用） */
export interface LaborGroup {
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号（为空时显示 id 前 8 位） */
  cost_object_no: string;
  /** 产出品名称 */
  product_name: string | null;
  /** 参与的行数 */
  row_count: number;
  /** 员工人数（姓名去重） */
  people_count: number;
  /** 人工工时合计 */
  hours_total: string;
  /** 机器工时合计 */
  machine_hours_total: string;
  /** 金额合计 */
  amount_total: string;
  /** 已确认行数 */
  confirmed_count: number;
  /** 草稿行数 */
  draft_count: number;
}

/**
 * 按成本对象分组汇总（**本页数据**，不是全量）。
 *
 * 列表分页是后端做的，所以这里的合计只代表当前页 —— 页面必须把这点写给用户看
 * （否则「合计 120 小时」会被当成当期全量）。
 */
export function groupByObject(rows: CostLaborRow[]): LaborGroup[] {
  const map = new Map<string, CostLaborRow[]>();
  for (const row of rows) {
    const list = map.get(row.cost_object_id);
    if (list) {
      list.push(row);
    } else {
      map.set(row.cost_object_id, [row]);
    }
  }
  const groups: LaborGroup[] = [];
  for (const [costObjectId, list] of map) {
    const names = new Set(list.map((row) => row.employee_name));
    groups.push({
      cost_object_id: costObjectId,
      cost_object_no: list[0].cost_object_no ?? `${costObjectId.slice(0, 8)}…`,
      product_name: list[0].product_name,
      row_count: list.length,
      people_count: names.size,
      hours_total: sumDecimal(list.map((row) => row.hours), 2),
      machine_hours_total: sumDecimal(list.map((row) => row.machine_hours), 2),
      amount_total: sumDecimal(list.map((row) => row.amount), AMOUNT_SCALE),
      confirmed_count: list.filter((row) => row.status === 'CONFIRMED').length,
      draft_count: list.filter((row) => row.status === 'DRAFT').length,
    });
  }
  groups.sort((left, right) => left.cost_object_no.localeCompare(right.cost_object_no));
  return groups;
}

// ==================== 导入 ====================

/**
 * 导入回执 → 人话摘要。
 *
 * 全有或全无：`success > 0` ⇒ `failed` 必为 0。**不要**把 `success=0, failed=3`
 * 说成"部分成功"——那一条都没入库。
 */
export function importSummaryText(result: { success: number; failed: number }): string {
  if (result.success > 0 && result.failed === 0) {
    return `导入成功：${result.success} 行已入库（状态草稿，复核后请批量确认）`;
  }
  if (result.success === 0 && result.failed === 0) {
    return '文件里没有数据行（第 1 行是表头，第 2 行起才是数据）';
  }
  return `整批拒绝：${result.failed} 行有问题，一条都没入库 —— 改完重新上传（半批入库会让当期人工成本少一块而没人发现）`;
}

/** 逐行错误 → 表格行（行号 + 列 + 原因，直接渲染） */
export function importIssueRows(issues: ImportIssue[]): ImportIssue[] {
  return [...issues].sort((left, right) => left.line - right.line || left.column.localeCompare(right.column));
}

/** 三级费率全空时的提示（`66059` 的"怎么修"） */
export const RATE_MISSING_HINT =
  '工资率留空时后端按「成本对象 > 车间 > 公司」自动取数；三级都没配 ⇒ 422 + 66059，' +
  '**不会按 0 计**（编造的人工成本比算不出来更危险）。请到费率维护里给该对象 / 车间 / 公司配一条生效中的工资率，或在本表单直接填费率。';

/** 费率自动取数说明（表单字段旁的常驻提示） */
export const RATE_AUTO_HINT = '留空 = 后端自动取数（成本对象 > 车间 > 公司），回单会把「取到哪一级」显示出来';
