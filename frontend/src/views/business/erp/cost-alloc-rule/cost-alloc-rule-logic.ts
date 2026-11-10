/**
 * 分摊规则页纯逻辑（COST-B2-13 / ARCH-9 §6.2.3 · §4.4 · §5.2 #20–#24）。
 *
 * 只放判定：表单校验、payload 拼装（部分更新只带真改过的字段）、试算守恒判定与
 * 展示用的百分比、生效期文案。`.vue` 只负责画。
 *
 * # 三条口径（卡面验收项，别弱化）
 *
 * 1. **规则编码**：trim + 大写归一，只允许大写字母 / 数字 / `_ - .`，≤ 32（后端同名口径）。
 *    页面**先归一再提交**，避免用户打小写得到"编码已存在"的假冲突；
 * 2. **试算守恒**：`Σ alloc_amount == pool_amount` 是硬断言。页面在每组表尾打勾，
 *    并用 `decimalEquals` 比较（后端固定 4 位小数，字面比较会因为位数不同而抖）；
 * 3. **两条硬错误不吞**：池有金额但当期无在产对象（`66062`）/ 该标准基数合计 0（`66063`）
 *    —— 报文已点名车间、科目、金额，页面原样展示并说明"怎么修"。
 */

import {
  ALLOC_BASES,
  ALLOC_BASIS_HINTS,
  ALLOC_BASIS_LABELS,
  ALLOC_RULE_STATUS_LABELS,
  PRIORITY_DEFAULT,
  PRIORITY_MAX,
  PRIORITY_MIN,
  RULE_CODE_MAX,
  RULE_NAME_MAX,
  type AllocBasis,
  type CostAllocPreviewGroup,
  type CostAllocPreviewRes,
  type CostAllocRuleCreatePayload,
  type CostAllocRuleRow,
  type CostAllocRuleUpdatePayload,
} from '@/api/business/erp/cost-alloc-rule-kinds';
import { decimalEquals, ratio, scaledInt } from '@/views/business/erp/shared/decimal';
import { isMonthKey } from '@/views/business/erp/shared/money';

/** 日期串（`YYYY-MM-DD`） */
export function isDateString(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value.trim());
}

/** 期间串（`yyyy-MM`；与后端 `is_month_key` 同口径） */
export function isPeriodKey(value: string): boolean {
  return isMonthKey(value);
}

/** 规则编码归一（trim + 大写；后端 `normalize_rule_code` 同口径） */
export function normalizeRuleCode(raw: string): string {
  return raw.trim().toUpperCase();
}

/** 规则编码字符合法（大写字母 / 数字 / `_ - .`） */
export function isRuleCodeShape(value: string): boolean {
  return /^[A-Z0-9_.-]+$/.test(value);
}

/** 分配标准中文（未知值原样返回，不吞） */
export function basisLabel(basis: string): string {
  return ALLOC_BASIS_LABELS[basis as AllocBasis] ?? basis;
}

/** 分配标准的一句话解释 */
export function basisHint(basis: string): string {
  return ALLOC_BASIS_HINTS[basis as AllocBasis] ?? '（未知分配标准，请核对后端值域）';
}

/** 状态中文 */
export function statusLabel(status: number): string {
  return ALLOC_RULE_STATUS_LABELS[status] ?? String(status);
}

/** 分配标准下拉（名字 + 解释拼在一行，设计 §6.2.3 要求"带一句解释"） */
export function basisOptions(): Array<{ label: string; value: string }> {
  return ALLOC_BASES.map((value) => ({
    label: `${ALLOC_BASIS_LABELS[value]} —— ${ALLOC_BASIS_HINTS[value]}`,
    value,
  }));
}

/** 生效期文案（`2026-11-01 起` / `2026-11-01 ~ 2026-12-31`） */
export function effectiveRangeText(row: Pick<CostAllocRuleRow, 'effective_from' | 'effective_to'>): string {
  if (!row.effective_to) {
    return `${row.effective_from} 起（长期）`;
  }
  return `${row.effective_from} ~ ${row.effective_to}`;
}

// ==================== 表单 ====================

/** 表单状态 */
export interface RuleFormState {
  /** 规则编码 */
  rule_code: string;
  /** 规则名 */
  rule_name: string;
  /** 车间 */
  dept_id: string;
  /** 科目 */
  account_code: string;
  /** 分配标准 */
  basis: string;
  /** 优先级 */
  priority: string;
  /** 生效开始日 */
  effective_from: string;
  /** 生效结束日（空 = 长期） */
  effective_to: string;
  /** 状态 1/0 */
  status: number;
  /** 备注 */
  remark: string;
}

/** 表单错误 */
export type RuleFormErrors = Partial<Record<keyof RuleFormState, string>>;

/** 空表单（生效期默认当月 1 日起、无终止日，设计 §6.2.3） */
export function emptyRuleForm(today: string, deptId = ''): RuleFormState {
  const monthStart = `${today.slice(0, 7)}-01`;
  return {
    rule_code: '',
    rule_name: '',
    dept_id: deptId,
    account_code: '',
    basis: 'LABOR_HOURS',
    priority: String(PRIORITY_DEFAULT),
    effective_from: monthStart,
    effective_to: '',
    status: 1,
    remark: '',
  };
}

/** 列表行 → 表单（编辑用） */
export function formFromRow(row: CostAllocRuleRow): RuleFormState {
  return {
    rule_code: row.rule_code,
    rule_name: row.rule_name,
    dept_id: row.source_dept_id,
    account_code: row.account_code,
    basis: row.basis,
    priority: String(row.priority),
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
  };
}

/** 表单校验（文案与后端 400 对齐） */
export function validateRuleForm(form: RuleFormState): RuleFormErrors {
  const errors: RuleFormErrors = {};
  const code = normalizeRuleCode(form.rule_code);
  if (!code) {
    errors.rule_code = '规则编码必须填（唯一，大写字母 / 数字 / _ - .）';
  } else if (code.length > RULE_CODE_MAX) {
    errors.rule_code = `规则编码最长 ${RULE_CODE_MAX} 个字符（当前 ${code.length} 个）`;
  } else if (!isRuleCodeShape(code)) {
    errors.rule_code = '规则编码只允许大写字母、数字与 _ - . （小写会自动转大写）';
  }
  if (!form.rule_name.trim()) {
    errors.rule_name = '规则名必须填（给财务看的名字，如「一车间制造费用按工时分摊」）';
  } else if (form.rule_name.trim().length > RULE_NAME_MAX) {
    errors.rule_name = `规则名最长 ${RULE_NAME_MAX} 个字符`;
  }
  if (!form.dept_id) {
    errors.dept_id = '被摊车间必须选';
  }
  if (!form.account_code.trim()) {
    errors.account_code = '被摊科目必须选（只列 4101.* 启用末级）';
  }
  if (!ALLOC_BASES.includes(form.basis as AllocBasis)) {
    errors.basis = '分配标准必须四选一';
  }
  const priority = Number(form.priority);
  if (!Number.isInteger(priority) || priority < PRIORITY_MIN || priority > PRIORITY_MAX) {
    errors.priority = `优先级是 ${PRIORITY_MIN}~${PRIORITY_MAX} 的整数（缺省 ${PRIORITY_DEFAULT}；数字小的先算，本期一期只影响展示顺序）`;
  }
  if (!isDateString(form.effective_from)) {
    errors.effective_from = '生效开始日必须选（YYYY-MM-DD）';
  }
  const to = form.effective_to.trim();
  if (to && !isDateString(to)) {
    errors.effective_to = '生效结束日可空；填就是 YYYY-MM-DD';
  } else if (to && isDateString(form.effective_from) && to < form.effective_from) {
    errors.effective_to = `生效结束日（${to}）不能早于开始日（${form.effective_from}）`;
  }
  if (form.remark.length > 255) {
    errors.remark = '备注最长 255 个字符';
  }
  return errors;
}

/** 错误条数 */
export function errorCount(errors: RuleFormErrors): number {
  return Object.keys(errors).length;
}

/** 表单 → 新建入参 */
export function toCreatePayload(form: RuleFormState): CostAllocRuleCreatePayload {
  const payload: CostAllocRuleCreatePayload = {
    rule_code: normalizeRuleCode(form.rule_code),
    rule_name: form.rule_name.trim(),
    dept_id: form.dept_id,
    account_code: form.account_code.trim(),
    basis: form.basis as AllocBasis,
    priority: Number(form.priority),
    effective_from: form.effective_from.trim(),
  };
  const to = form.effective_to.trim();
  if (to) {
    payload.effective_to = to;
  }
  if (form.status === 0) {
    payload.status = 0;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/**
 * 表单 → 修改入参（**只带真改过的字段**）。
 *
 * `effective_to` 传空串 = 清空（改成长期有效）；不传 = 不改。这里是两者唯一的差别，
 * 所以"原来是有限期、现在清空"必须显式发 `''`。
 */
export function toUpdatePayload(origin: CostAllocRuleRow, form: RuleFormState): CostAllocRuleUpdatePayload {
  const payload: CostAllocRuleUpdatePayload = {};
  if (normalizeRuleCode(form.rule_code) !== origin.rule_code) {
    payload.rule_code = normalizeRuleCode(form.rule_code);
  }
  if (form.rule_name.trim() !== origin.rule_name) {
    payload.rule_name = form.rule_name.trim();
  }
  if (form.dept_id !== origin.source_dept_id) {
    payload.dept_id = form.dept_id;
  }
  if (form.account_code.trim() !== origin.account_code) {
    payload.account_code = form.account_code.trim();
  }
  if (form.basis !== origin.basis) {
    payload.basis = form.basis as AllocBasis;
  }
  if (Number(form.priority) !== origin.priority) {
    payload.priority = Number(form.priority);
  }
  if (form.effective_from.trim() !== origin.effective_from) {
    payload.effective_from = form.effective_from.trim();
  }
  const to = form.effective_to.trim();
  if (to !== (origin.effective_to ?? '')) {
    payload.effective_to = to;
  }
  if (form.status !== origin.status) {
    payload.status = form.status;
  }
  if (form.remark.trim() !== (origin.remark ?? '')) {
    payload.remark = form.remark.trim();
  }
  return payload;
}

/** 有没有真改动 */
export function isEmptyPayload(payload: CostAllocRuleUpdatePayload): boolean {
  return Object.keys(payload).length === 0;
}

// ==================== 试算展示 ====================

/** 一组试算的守恒判定（Σ 分摊额 == 池金额） */
export function groupConserved(group: CostAllocPreviewGroup): boolean {
  return decimalEquals(group.alloc_total, group.pool_amount);
}

/** 整体守恒判定（已摊池 == 已摊金额；未分摊池单独列，不参与） */
export function previewConserved(preview: CostAllocPreviewRes): boolean {
  return decimalEquals(preview.total_alloc_amount, preview.total_pool_amount);
}

/** 本对象占基数合计的百分比（展示用，保留 2 位） */
export function basisSharePercent(line: { basis_value: string }, group: CostAllocPreviewGroup): string {
  return `${(ratio(line.basis_value, group.basis_total) * 100).toFixed(2)}%`;
}

/** 有尾差调整的行（最大余数法**只有一行**非 0；页面要把它标出来） */
export function roundingRows(group: CostAllocPreviewGroup): Array<{ cost_object_no: string; rounding_adj: string }> {
  return group.rows
    .filter((line) => scaledInt(line.rounding_adj, 4) !== 0)
    .map((line) => ({ cost_object_no: line.cost_object_no, rounding_adj: line.rounding_adj }));
}

/** 试算有没有可用结果（没有组、没有诊断 = 该期间既没钱也没规则） */
export function previewEmpty(preview: CostAllocPreviewRes | null): boolean {
  if (!preview) {
    return true;
  }
  return preview.groups.length === 0 && preview.unallocated.length === 0;
}

/** 试算摘要（对话框顶部一行：池 / 摊 / 未摊） */
export function previewSummaryText(preview: CostAllocPreviewRes): string {
  const unallocated = preview.unallocated.length;
  const conserved = previewConserved(preview) ? '守恒 ✓' : '不守恒 ✗（请把结果发给开发，这是 bug）';
  return `期间 ${preview.period_code}：池 ${preview.total_pool_amount} / 摊 ${preview.total_alloc_amount} · ${conserved} · 未分摊 ${unallocated} 组`;
}

/** 规则"已参与分摊不能改口径"的引导（后端 409 时报文之外再补一句） */
export const RULE_LOCKED_HINT =
  '已参与过分摊的版本只允许改「规则名 / 备注 / 状态」。要改分配标准或科目，请新建一条生效期更晚的版本（如 12 月 1 日起），旧版本继续为历史期间服务 —— 这是"改规则不溯及历史"的落地方式。';
