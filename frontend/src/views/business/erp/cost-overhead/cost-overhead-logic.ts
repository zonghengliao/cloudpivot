/**
 * 费用台账页纯逻辑（COST-B2-13 / ARCH-9 §6.1 #3 · §4.3 · §5.2 #17–#19）。
 *
 * 只放判定：表单校验、payload 拼装、池行 / 直接行判定、科目候选过滤、本页汇总。
 * `.vue` 只负责画。
 *
 * # 三条口径（与后端逐字对齐）
 *
 * | 事情 | 后端 | 本文件 |
 * |------|------|--------|
 * | 借方科目 | `ck_biz_cost_oh_debit`：`LIKE '4101.%'` + 账套里存在/末级/启用（`66060`） | {@link isManufacturingAccount} 先拦前缀与分段数字 |
 * | 金额 | `> 0` 且 ≤ 4 位小数（`66061`） | {@link validateOverheadForm} |
 * | 池 vs 直接 | `cost_object_id IS NULL` = 车间池；非空 = 直接计入该批 | {@link isPoolRow} |
 *
 * # 数值口径
 *
 * 本页汇总走整数定点（4 位小数）：`"1000.0000" + "300.0000"` 直接相加会变字符串拼接。
 */

import {
  AMOUNT_SCALE,
  DEFAULT_DEBIT_ACCOUNT,
  EXPENSE_ITEM_DICT,
  EXPENSE_ITEM_LABELS,
  EXPENSE_ITEMS,
  type CostOverheadCreatePayload,
  type CostOverheadRow,
  type ExpenseItem,
} from '@/api/business/erp/cost-overhead-kinds';
import { formatScaled, isDecimalWithinScale, scaledInt } from '@/views/business/erp/shared/decimal';
import { isManufacturingAccount } from '@/views/business/erp/shared/manufacturing-account';

/** 期间串（`yyyy-MM`） */
export function isPeriodKey(value: string): boolean {
  return /^\d{4}-(0[1-9]|1[0-2])$/.test(value.trim());
}

export { isDecimalWithinScale };

/**
 * 是否是制造费用科目（`4101.x`）—— 实现与口径见 `shared/manufacturing-account.ts`。
 *
 * 与 `cost_overhead_dto::is_manufacturing_account` 逐条一致：前缀 `4101.` 之后
 * **必须还有内容**，且每段都是数字（挡掉 `4101.` / `4101.abc` 这种"前缀对但账套里
 * 不可能存在"的输入，免得白跑一次查库）。
 */
export {
  MANUFACTURING_ACCOUNT_PREFIX,
  isManufacturingAccount,
  pickCreditAccounts,
  pickManufacturingAccounts,
  type AccountLike,
} from '@/views/business/erp/shared/manufacturing-account';

/** 费用项目中文（优先用后端给的字典实时名，落空才用内置兜底） */
export function expenseItemLabel(code: string, fromServer?: string | null): string {
  const label = (fromServer ?? '').trim();
  if (label) {
    return label;
  }
  return EXPENSE_ITEM_LABELS[code as ExpenseItem] ?? code;
}

/** 是不是车间费用池行（`cost_object_id IS NULL`，等分摊） */
export function isPoolRow(row: Pick<CostOverheadRow, 'cost_object_id'>): boolean {
  return !row.cost_object_id;
}

/** 费用项目选项（字典加载失败时的兜底；正常路径用 `DictSelect`） */
export function expenseItemOptions(): Array<{ label: string; value: string }> {
  return EXPENSE_ITEMS.map((value) => ({ label: EXPENSE_ITEM_LABELS[value], value }));
}

/** 字典编码（页面 `DictSelect` 用） */
export const EXPENSE_ITEM_DICT_CODE = EXPENSE_ITEM_DICT;

// ==================== 表单 ====================

/** 表单状态（字符串原样收） */
export interface OverheadFormState {
  /** 会计期间 */
  period_code: string;
  /** 成本对象（空 = 车间池） */
  cost_object_id: string;
  /** 车间 */
  dept_id: string;
  /** 费用项目 */
  expense_item: string;
  /** 借方科目 */
  debit_account_code: string;
  /** 对方科目 */
  credit_account_code: string;
  /** 金额 */
  amount: string;
  /** 备注 */
  remark: string;
}

/** 表单错误 */
export type OverheadFormErrors = Partial<Record<keyof OverheadFormState, string>>;

/** 空表单（借方科目预填缺省 4101.09，与 DDL 默认一致） */
export function emptyOverheadForm(period: string): OverheadFormState {
  return {
    period_code: period,
    cost_object_id: '',
    dept_id: '',
    expense_item: 'DEPRECIATION',
    debit_account_code: DEFAULT_DEBIT_ACCOUNT,
    credit_account_code: '',
    amount: '',
    remark: '',
  };
}

/** 表单校验（文案与后端 400 对齐） */
export function validateOverheadForm(form: OverheadFormState): OverheadFormErrors {
  const errors: OverheadFormErrors = {};
  if (!isPeriodKey(form.period_code)) {
    errors.period_code = '会计期间格式是 yyyy-MM（如 2026-11）';
  }
  if (!form.dept_id) {
    errors.dept_id = '车间必须选（池的归属车间，也是分摊规则的匹配键）';
  }
  if (!form.expense_item) {
    errors.expense_item = '费用项目必须选';
  }
  const debit = form.debit_account_code.trim();
  if (!debit) {
    errors.debit_account_code = `借方科目可空，缺省 ${DEFAULT_DEBIT_ACCOUNT}（制造费用-其他）`;
  } else if (!isManufacturingAccount(debit)) {
    errors.debit_account_code = `借方科目必须是制造费用科目（4101.*，如 4101.01），收到「${debit}」`;
  }
  if (!form.credit_account_code.trim()) {
    errors.credit_account_code = '对方科目必须选（银行 1002 / 应付 2241.01 / 累计折旧 1801 …）';
  }
  const amount = form.amount.trim();
  if (!amount) {
    errors.amount = '金额必须填';
  } else if (!isDecimalWithinScale(amount, AMOUNT_SCALE)) {
    errors.amount = `金额最多 ${AMOUNT_SCALE} 位小数（库列是 NUMERIC(18,4)，超精度会被拒而不是四舍五入）`;
  } else if (!(Number(amount) > 0)) {
    errors.amount = '金额必须大于 0（0 元的费用行不进成本，别录）';
  }
  if (form.remark.length > 255) {
    errors.remark = '备注最长 255 个字符';
  }
  return errors;
}

/** 错误条数 */
export function errorCount(errors: OverheadFormErrors): number {
  return Object.keys(errors).length;
}

/** 表单 → 新建入参（空的可选字段一律不发，避免后端把 `""` 当值） */
export function toCreatePayload(form: OverheadFormState): CostOverheadCreatePayload {
  const payload: CostOverheadCreatePayload = {
    period_code: form.period_code.trim(),
    dept_id: form.dept_id,
    expense_item: form.expense_item,
    credit_account_code: form.credit_account_code.trim(),
    amount: form.amount.trim(),
  };
  const objectId = form.cost_object_id.trim();
  if (objectId) {
    payload.cost_object_id = objectId;
  }
  const debit = form.debit_account_code.trim();
  if (debit && debit !== DEFAULT_DEBIT_ACCOUNT) {
    payload.debit_account_code = debit;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

// ==================== 科目候选（实现见 shared/manufacturing-account.ts，上面已 re-export） ====================

// ==================== 本页汇总 ====================

/** 本页汇总（池 / 直接分开：这是费用台账最要紧的一条语义） */
export interface OverheadSummary {
  /** 行数 */
  row_count: number;
  /** 池行数 */
  pool_count: number;
  /** 池金额合计 */
  pool_amount: string;
  /** 直接行数 */
  direct_count: number;
  /** 直接金额合计 */
  direct_amount: string;
  /** 金额合计 */
  total_amount: string;
}

/** 汇总（当前页；分页是服务端做的，页面必须写明"本页"） */
export function summarizeRows(rows: CostOverheadRow[]): OverheadSummary {
  const pool = rows.filter((row) => isPoolRow(row));
  const direct = rows.filter((row) => !isPoolRow(row));
  const sum = (list: CostOverheadRow[]): string =>
    formatScaled(
      list.reduce((acc, row) => acc + scaledInt(row.amount, AMOUNT_SCALE), 0),
      AMOUNT_SCALE,
    );
  return {
    row_count: rows.length,
    pool_count: pool.length,
    pool_amount: sum(pool),
    direct_count: direct.length,
    direct_amount: sum(direct),
    total_amount: sum(rows),
  };
}
