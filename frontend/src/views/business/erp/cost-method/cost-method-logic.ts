/**
 * 成本方法配置页纯逻辑（M5-01 · 品种法 / 分批法 / 标准成本）。
 *
 * 只放判定：表单缺省、校验、payload 拼装（部分更新只带真改过的字段）、
 * 展示文案（方法 / 范围 / 状态 / 生效期 / 是否锁口径）。`.vue` 只负责画。
 *
 * # 三条口径（后端逐字同源，别在页面里弱化）
 *
 * 1. **同范围生效期不许重叠**：一重叠，「这期按什么算」就没答案（后端 `409 + 66102`）。
 *    页面在客户端先做一次「同范围 + 区间相交」预检，提示语与后端一致；
 * 2. **已生效过（`effective_from < 今天`）的版本只允许改 停止日 / 状态 / 备注**
 *    （后端 `409 + 66105`）—— 页面把「方法 / 范围 / 对象 / 生效日」四个控件置灰并给提示，
 *    不让用户白填一遍才被拒；
 * 3. **一期只有分批法算得出来**：选品种法 / 标准成本时，表单上直接挂红字说明
 *    「这期点计算会被 66101 拒绝」，而不是等用户配置完再报错。
 */

import {
  COST_METHODS,
  COST_METHOD_DEFAULT,
  COST_METHOD_IMPLEMENTED,
  COST_METHOD_LABELS,
  COST_METHOD_NOTES,
  COST_METHOD_STATUS_LABELS,
  COST_SCOPE_LABELS,
  COST_SCOPES,
  EFFECTIVE_SOURCE_LABELS,
  type CostMethodCreatePayload,
  type CostMethodRow,
  type CostMethodUpdatePayload,
  type CostScope,
} from '@/api/business/erp/cost-method-kinds';

/** 日期串形状（`YYYY-MM-DD`） */
export function isDateString(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value.trim());
}

/** 方法中文（未知值原样返回，不吞） */
export function methodLabel(method: string): string {
  return COST_METHOD_LABELS[method as keyof typeof COST_METHOD_LABELS] ?? method;
}

/** 方法说明 */
export function methodNote(method: string): string {
  return COST_METHOD_NOTES[method as keyof typeof COST_METHOD_NOTES] ?? '';
}

/** 该方法一期能不能算（未知值按「不能算」处理：不确定就不要放行） */
export function isImplemented(method: string): boolean {
  return COST_METHOD_IMPLEMENTED[method as keyof typeof COST_METHOD_IMPLEMENTED] === true;
}

/** 范围中文 */
export function scopeLabel(scope: string): string {
  return COST_SCOPE_LABELS[scope as CostScope] ?? scope;
}

/** 状态中文 */
export function statusLabel(status: number): string {
  return COST_METHOD_STATUS_LABELS[status] ?? String(status);
}

/** 判定来源中文（`DEFAULT` 等不在词表里时原样返回） */
export function sourceLabel(source: string): string {
  return EFFECTIVE_SOURCE_LABELS[source] ?? source;
}

/** 方法下拉（未实现的挂后缀，选之前就知道算不出来） */
export const methodOptions = COST_METHODS.map((method) => ({
  value: method,
  label: `${COST_METHOD_LABELS[method]}${COST_METHOD_IMPLEMENTED[method] ? '' : '（一期未实现，只可配置）'}`,
  note: COST_METHOD_NOTES[method],
}));

/** 范围下拉 */
export const scopeOptions = COST_SCOPES.map((scope) => ({
  value: scope,
  label: COST_SCOPE_LABELS[scope],
  note:
    scope === 'GLOBAL'
      ? '全公司没有更具体配置时按它算（一条开区间版本就够）'
      : '给某个成本对象单独指定方法（优先于全公司默认）',
}));

/** 状态下拉 */
export const statusOptions = [
  { value: 1, label: COST_METHOD_STATUS_LABELS[1] },
  { value: 0, label: COST_METHOD_STATUS_LABELS[0] },
];

/** 生效期文案：`2026-01-01 ~ 长期` */
export function effectiveRangeText(row: Pick<CostMethodRow, 'effective_from' | 'effective_to'>): string {
  return `${row.effective_from} ~ ${row.effective_to ?? '长期'}`;
}

/**
 * 是否「已生效过」（口径锁定）。
 *
 * 判据与后端一致：`effective_from < 今天`（当天生效的当天还能改 —— 它还没覆盖任何
 * 一个过去的核算期间）。字符串日期比较在 `YYYY-MM-DD` 下就是字典序 = 时间序。
 */
export function isLocked(row: Pick<CostMethodRow, 'effective_from'>, today: string): boolean {
  return row.effective_from < today;
}

/** 行标签：全公司显示中文，对象级优先对象号（对象被删时回落 id 前 8 位） */
export function scopeDisplay(row: Pick<CostMethodRow, 'scope' | 'scope_id' | 'scope_object_no'>): string {
  if (row.scope === 'GLOBAL') {
    return COST_SCOPE_LABELS.GLOBAL;
  }
  if (row.scope_object_no) {
    return row.scope_object_no;
  }
  const id = row.scope_id ?? '';
  return id ? `${id.slice(0, 8)}…` : '—';
}

// ==================== 表单 ====================

/** 表单状态（字符串/数字，全部可空：`t-select` 与 `t-date-picker` 的 v-model 友好） */
export interface MethodFormState {
  method: string;
  scope: string;
  scope_id: string;
  effective_from: string;
  effective_to: string;
  status: number;
  remark: string;
}

/**
 * 新建缺省值。
 *
 * 范围缺省给 `COST_OBJECT`（卡面「按成本对象选方法」正是主用法）；生效日缺省今天
 * （用户要排到未来时会自己改）；状态缺省启用。
 */
export function emptyForm(today: string): MethodFormState {
  return {
    method: COST_METHOD_DEFAULT,
    scope: 'COST_OBJECT',
    scope_id: '',
    effective_from: today,
    effective_to: '',
    status: 1,
    remark: '',
  };
}

/** 行 → 表单（编辑用） */
export function formFromRow(row: CostMethodRow): MethodFormState {
  return {
    method: row.method,
    scope: row.scope,
    scope_id: row.scope_id ?? '',
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
  };
}

/** 表单校验结果（key 与控件对齐，便于行内红字） */
export interface MethodFormErrors {
  method?: string;
  scope?: string;
  scope_id?: string;
  effective_from?: string;
  effective_to?: string;
}

/**
 * 校验表单。
 *
 * 只查「形状与成对」；「与既有版本重叠」由 [`findOverlapHint`] 单独算 ——
 * 它需要列表数据，且提示语要说清是跟谁重叠。
 */
export function validateForm(form: MethodFormState): MethodFormErrors {
  const errors: MethodFormErrors = {};
  if (!form.method) {
    errors.method = '请选择核算方法';
  }
  if (!form.scope) {
    errors.scope = '请选择配置范围';
  }
  if (form.scope === 'COST_OBJECT' && !form.scope_id.trim()) {
    errors.scope_id = '指定成本对象时必须选择对象（要配全公司默认请把范围改成「全公司默认」）';
  }
  if (form.scope === 'GLOBAL' && form.scope_id.trim()) {
    errors.scope_id = '全公司默认不能带成本对象';
  }
  if (!isDateString(form.effective_from)) {
    errors.effective_from = '生效日必填（YYYY-MM-DD）';
  }
  if (form.effective_to.trim()) {
    if (!isDateString(form.effective_to)) {
      errors.effective_to = '停止日必须是 YYYY-MM-DD（留空 = 长期有效）';
    } else if (isDateString(form.effective_from) && form.effective_to < form.effective_from) {
      errors.effective_to = '停止日不能早于生效日';
    }
  }
  return errors;
}

/** 校验是否通过 */
export function isValid(errors: MethodFormErrors): boolean {
  return Object.keys(errors).length === 0;
}

/**
 * 客户端重叠预检：同范围、启用中、区间相交的既有版本。
 *
 * `excludeId` = 编辑时排除自己。判据与后端 `find_overlap` 逐字一致（停用行不参与）。
 */
export function findOverlapHint(
  rows: CostMethodRow[],
  form: MethodFormState,
  excludeId?: string,
): CostMethodRow | null {
  const from = form.effective_from;
  const to = form.effective_to.trim() || null;
  if (!isDateString(from)) {
    return null;
  }
  const wanted = {
    scope: form.scope,
    scope_id: form.scope === 'COST_OBJECT' ? form.scope_id : null,
  };
  for (const row of rows) {
    if (excludeId && row.id === excludeId) {
      continue;
    }
    if (row.status !== 1) {
      continue;
    }
    if (row.scope !== wanted.scope) {
      continue;
    }
    const rowScopeId = row.scope === 'COST_OBJECT' ? row.scope_id : null;
    if (rowScopeId !== wanted.scope_id) {
      continue;
    }
    const rowEnd = row.effective_to;
    const aEndOk = rowEnd === null || rowEnd >= from;
    const bEndOk = to === null || to >= row.effective_from;
    if (aEndOk && bEndOk) {
      return row;
    }
  }
  return null;
}

/** 新建 payload */
export function toCreatePayload(form: MethodFormState): CostMethodCreatePayload {
  return {
    method: form.method,
    scope: form.scope,
    scope_id: form.scope === 'COST_OBJECT' ? form.scope_id.trim() : null,
    effective_from: form.effective_from.trim(),
    effective_to: form.effective_to.trim() || null,
    status: form.status,
    remark: form.remark.trim(),
  };
}

/**
 * 修改 payload：**只带真改过的字段**（缺省 = 不改）。
 *
 * `effective_to` / `remark` 清空时显式传空串（后端把空串解释成「清空」；传 `null` 或不传
 * 是「不改」—— `serde` 的 `Option<String>` 分不出这两者，所以前端必须用空串表达清空）。
 */
export function toUpdatePayload(form: MethodFormState, row: CostMethodRow): CostMethodUpdatePayload {
  const payload: CostMethodUpdatePayload = {};
  if (form.method !== row.method) {
    payload.method = form.method;
  }
  if (form.scope !== row.scope) {
    payload.scope = form.scope;
  }
  const formScopeId = form.scope === 'COST_OBJECT' ? form.scope_id.trim() : null;
  const rowScopeId = row.scope === 'COST_OBJECT' ? (row.scope_id ?? null) : null;
  if (formScopeId !== rowScopeId) {
    payload.scope_id = formScopeId;
  }
  if (form.effective_from !== row.effective_from) {
    payload.effective_from = form.effective_from;
  }
  if ((form.effective_to.trim() || null) !== (row.effective_to ?? null)) {
    payload.effective_to = form.effective_to.trim();
  }
  if (form.status !== row.status) {
    payload.status = form.status;
  }
  if ((form.remark.trim() || null) !== (row.remark ?? null)) {
    payload.remark = form.remark.trim();
  }
  return payload;
}

/** 修改 payload 是否为空（空 = 什么都没改，不该发请求） */
export function isEmptyPayload(payload: CostMethodUpdatePayload): boolean {
  return Object.keys(payload).length === 0;
}
