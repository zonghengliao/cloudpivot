/**
 * 标准成本维护页纯逻辑（M5-09）。
 *
 * 这里只放**不碰 DOM、不碰请求层**的东西，`pnpm test:unit`（`tsx --test`）直接钉住。
 * 页面（`index.vue`）只做「取数 + 渲染 + 把错误显示出来」。
 *
 * # 钉住的六件事（都是「算错就要么白干、要么把标准写错」的地方）
 *
 * ① **金额一律「整数微元」算**：料 = Σ round(用量 × 单价, 6)，工 = round(工时 × 费率, 6)。
 *    用 `number` 直接乘会得 `17.166665999999998` 这种值，而库里有
 *    `ck_biz_cost_std_sum`（总 = 料 + 工 + 费）—— 前端预览与库对不上，
 *    用户会以为「保存后金额被改了」。所以全程 `BigInt` 定点（6 位小数），
 *    只在显示前转回字符串。
 * ② **留空 ≠ 0**：卡头三要素留空是合法口径（= 0），明细的用量 / 单价留空是**错**
 *    （填了耗用物料就必须给，按 0 补会把材料成本算少且没有红灯）；
 * ③ **入库字符串不做数值化**：后端 `Decimal` 收字符串，前端把用户输入 trim 后原样提交
 *    （`2.50` 不会被改成 `2.5`，避免「我没改它却变了」的审计疑问）；
 * ④ **已生效版本锁改**：`effective_from < 今天` 只能改停止日 / 状态 / 备注，
 *    所以 `toUpdatePayload` 在这种情况下**只发这三样**（不发数字、不发明细 ——
 *    后端 66109 会拒，而且「重算金额必须等于库金额」那道闸很严）；
 * ⑤ **期间 ⇄ 日期**：新建用整期写法（`period_code`），自定义窗口才发 `effective_from/to`；
 *    两者同时发会被后端判「矛盾」400；
 * ⑥ **导入结果**：「全有或全无」必须一句话说清楚 —— 失败时不能只说「失败」，
 *    要说「一条都没入库」+ 逐行原因。
 */

import type {
  ImportIssue,
  StandardDetailRes,
  StandardImportResult,
  StandardItemPayload,
  StandardRow,
} from '@/api/business/erp/cost-standard-kinds';
import { STANDARD_STATUS_ACTIVE } from '@/api/business/erp/cost-standard-kinds';

// ==================== 期间 / 日期 ====================

/** `yyyy-MM` 形状（年份 2000–2099，月份 01–12 —— 与后端 `period_window` 同口径）。 */
const PERIOD_PATTERN = /^(20\d{2})-(0[1-9]|1[0-2])$/;

/** `yyyy-MM-dd` 形状 + 真实日期（`2026-02-30` 不算）。 */
export function isDateText(text: string): boolean {
  const value = text.trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return false;
  }
  const [year, month, day] = value.split('-').map((part) => Number(part));
  const date = new Date(Date.UTC(year, month - 1, day));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

/** 期间形状校验（`yyyy-MM`）。 */
export function isPeriodText(text: string): boolean {
  return PERIOD_PATTERN.test(text.trim());
}

/** 今天（`yyyy-MM-dd`，本地时区 —— 与页面展示一致）。 */
export function todayText(now: Date = new Date()): string {
  const year = now.getFullYear();
  const month = `${now.getMonth() + 1}`.padStart(2, '0');
  const day = `${now.getDate()}`.padStart(2, '0');
  return `${year}-${month}-${day}`;
}

/** 当前期间（`yyyy-MM`）。 */
export function currentPeriod(now: Date = new Date()): string {
  return todayText(now).slice(0, 7);
}

/** 期间 → `(期初, 期末)`（与后端 `period_window` 同口径：期末 = 下月 1 日 − 1 天）。 */
export function periodWindow(period: string): { start: string; end: string } | null {
  if (!isPeriodText(period)) {
    return null;
  }
  const [year, month] = period.trim().split('-').map((part) => Number(part));
  const lastDay = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return {
    start: `${period.trim()}-01`,
    end: `${period.trim()}-${`${lastDay}`.padStart(2, '0')}`,
  };
}

/** 生效区间文本：`2026-11-01 ~ 至今` / `2026-11-01 ~ 2026-11-30`。 */
export function windowText(from: string | null | undefined, to: string | null | undefined): string {
  if (!from) {
    return '—';
  }
  return `${from} ~ ${to ? to : '至今'}`;
}

// ==================== 定点数值（6 位小数） ====================

/** 金额 / 单价 / 费率的小数位（与库列一致）。 */
export const AMOUNT_SCALE = 6;
/** 工时的数字位（与库列一致：4 位）。 */
export const HOURS_SCALE = 4;
/** BOM 行数上限（后端 `dto::ITEM_MAX`）。 */
export const ITEM_MAX = 200;
/** 备注长度上限（后端 `dto::REMARK_MAX`）。 */
export const REMARK_MAX = 200;

/**
 * 文本 → 定点整数（`scale` 位小数）。
 *
 * # 返回值
 *
 * `null` = 不是数字 / 负数 / 小数位超过 `scale`（与后端 `ensure_decimal` 的判据一致：
 * 超精度**不静默四舍五入**，因为库里那列就是这个精度）。
 */
export function toScaled(text: string | null | undefined, scale: number): bigint | null {
  const value = (text ?? '').trim();
  if (!value) {
    return null;
  }
  if (!/^\d+(\.\d+)?$/.test(value)) {
    return null;
  }
  const [intPart, fracPart = ''] = value.split('.');
  if (fracPart.length > scale) {
    return null;
  }
  const padded = `${intPart}${fracPart.padEnd(scale, '0')}`;
  try {
    return BigInt(padded);
  } catch {
    return null;
  }
}

/** 定点整数 → 文本（固定 `scale` 位小数，与后端出参同形：`"17.166666"`）。 */
export function scaledText(value: bigint, scale: number): string {
  const negative = value < 0n;
  const abs = negative ? -value : value;
  const text = abs.toString().padStart(scale + 1, '0');
  const cut = text.length - scale;
  return `${negative ? '-' : ''}${text.slice(0, cut)}.${text.slice(cut)}`;
}

/**
 * 定点乘法 + 半远离零舍入：`a`（`aScale` 位）× `b`（`bScale` 位）→ `outScale` 位。
 *
 * 与 PG `round(numeric, n)` / Rust `RoundingStrategy::MidpointAwayFromZero` 同口径 ——
 * 库里的 CHECK（`stock_amount = round(qty*price,6)`）就是按这个算的。
 */
export function mulRound(a: bigint, aScale: number, b: bigint, bScale: number, outScale: number): bigint {
  const product = a * b;
  const productScale = aScale + bScale;
  if (productScale === outScale) {
    return product;
  }
  if (productScale < outScale) {
    return product * 10n ** BigInt(outScale - productScale);
  }
  const divisor = 10n ** BigInt(productScale - outScale);
  const negative = product < 0n;
  const abs = negative ? -product : product;
  const rounded = (abs + divisor / 2n) / divisor;
  return negative ? -rounded : rounded;
}

/** 三要素预览（单位标准成本的料 / 工 / 费 / 总，全部是 `"0.000000"` 形状的文本）。 */
export interface AmountPreview {
  material: string;
  labor: string;
  overhead: string;
  total: string;
  /** 明细里有没有算不出来的行（用量 / 单价非法）—— 有就整块标红，别让用户以为数是准的 */
  valid: boolean;
}

/**
 * 三要素预览：料 = Σ round(用量 × 单价, 6)、工 = round(工时 × 费率, 6)、总 = 料 + 工 + 费。
 *
 * 与服务端 `StandardSpec::amounts` 同一个算式；**服务端仍是最终口径**（页面只做提前告知）。
 */
export function previewAmounts(
  items: ReadonlyArray<{ std_qty: string; std_price: string }>,
  hours: string,
  rate: string,
  overhead: string,
): AmountPreview {
  let material = 0n;
  let valid = true;
  for (const item of items) {
    const qty = toScaled(item.std_qty, AMOUNT_SCALE);
    const price = toScaled(item.std_price, AMOUNT_SCALE);
    if (qty === null || price === null) {
      valid = false;
      continue;
    }
    material += mulRound(qty, AMOUNT_SCALE, price, AMOUNT_SCALE, AMOUNT_SCALE);
  }
  const hoursValue = hours.trim() ? toScaled(hours, HOURS_SCALE) : 0n;
  const rateValue = rate.trim() ? toScaled(rate, AMOUNT_SCALE) : 0n;
  const overheadValue = overhead.trim() ? toScaled(overhead, AMOUNT_SCALE) : 0n;
  if (hoursValue === null || rateValue === null || overheadValue === null) {
    valid = false;
  }
  const labor = mulRound(hoursValue ?? 0n, HOURS_SCALE, rateValue ?? 0n, AMOUNT_SCALE, AMOUNT_SCALE);
  const overheadCost = overheadValue ?? 0n;
  return {
    material: scaledText(material, AMOUNT_SCALE),
    labor: scaledText(labor, AMOUNT_SCALE),
    overhead: scaledText(overheadCost, AMOUNT_SCALE),
    total: scaledText(material + labor + overheadCost, AMOUNT_SCALE),
    valid,
  };
}

// ==================== 表单 ====================

/** 表单里的一行标准 BOM（`key` 只给 `v-for` 用，不提交）。 */
export interface StandardFormItem {
  key: string;
  component_material_id: string;
  std_qty: string;
  std_price: string;
}

/** 表单状态（新建 / 编辑共用）。 */
export interface StandardFormState {
  material_id: string;
  /** 整期写法（新建必填；编辑未生效版本也可用它换窗口） */
  period_code: string;
  /** `true` = 自定义生效窗口（用下面两个日期，不用 `period_code`） */
  custom_window: boolean;
  effective_from: string;
  effective_to: string;
  /** `true` = 把停止日清成「至今有效」 */
  clear_effective_to: boolean;
  std_labor_hours: string;
  std_labor_rate: string;
  std_overhead_cost: string;
  status: number;
  remark: string;
  items: StandardFormItem[];
}

/** 表单校验结果（键 = 字段名，值 = 人话）。 */
export type StandardFormErrors = Partial<
  Record<
    'material_id' | 'period_code' | 'effective_from' | 'effective_to' | 'std_labor_hours' | 'std_labor_rate' | 'std_overhead_cost' | 'remark' | 'items',
    string
  >
>;

let itemSeq = 0;

/** 造一个空行（`key` 进程内递增，避免同物料多行时 `v-for` 复用错行）。 */
export function emptyItem(): StandardFormItem {
  itemSeq += 1;
  return { key: `it-${itemSeq}`, component_material_id: '', std_qty: '', std_price: '' };
}

/** 空表单（新建用）：默认期间 = 本月。 */
export function emptyForm(now: Date = new Date()): StandardFormState {
  return {
    material_id: '',
    period_code: currentPeriod(now),
    custom_window: false,
    effective_from: '',
    effective_to: '',
    clear_effective_to: false,
    std_labor_hours: '',
    std_labor_rate: '',
    std_overhead_cost: '',
    status: STANDARD_STATUS_ACTIVE,
    remark: '',
    items: [],
  };
}

/** 详情 → 表单（编辑用）。 */
export function formFromDetail(detail: StandardDetailRes): StandardFormState {
  const head = detail.head;
  const period = head.effective_from.slice(0, 7);
  const window = periodWindow(period);
  // 生效区间正好是整期 ⇒ 按整期回填（这样「只改数字」不会顺手把窗口改掉）
  const isWholePeriod = window !== null && window.start === head.effective_from && window.end === head.effective_to;
  itemSeq += detail.items.length;
  return {
    material_id: head.material_id,
    period_code: period,
    custom_window: !isWholePeriod,
    effective_from: head.effective_from,
    effective_to: head.effective_to ?? '',
    clear_effective_to: head.effective_to === null,
    std_labor_hours: trimZero(head.std_labor_hours),
    std_labor_rate: trimZero(head.std_labor_rate),
    std_overhead_cost: trimZero(head.std_overhead_cost),
    status: head.status,
    remark: head.remark ?? '',
    items: detail.items.map((item, index) => ({
      key: `it-detail-${item.id}-${index}`,
      component_material_id: item.component_material_id,
      std_qty: trimZero(item.std_qty),
      std_price: trimZero(item.std_price),
    })),
  };
}

/** `"2.5000"` → `"2.5"`（回填给输入框，别让人以为自己填了一串 0）。 */
export function trimZero(text: string | null | undefined): string {
  const value = (text ?? '').trim();
  if (!value.includes('.')) {
    return value;
  }
  const trimmed = value.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' ? '0' : trimmed;
}

/**
 * 编辑的是「已生效过的版本」⇒ 口径字段全部锁定（后端 409/66109）。
 *
 * 判据与后端逐字一致：`effective_from < 今天`。**等于今天不算锁**（今天才开始生效的
 * 版本还能改，历史还没发生）。
 */
export function isLockedEffective(row: Pick<StandardRow, 'effective_from'>, today: string): boolean {
  return row.effective_from < today;
}

/** 校验表单（**纯函数**：页面只负责把结果画出来）。 */
export function validateForm(form: StandardFormState, creating: boolean): StandardFormErrors {
  const errors: StandardFormErrors = {};

  if (!form.material_id.trim()) {
    errors.material_id = '请选择成品 / 半成品物料';
  }

  if (form.custom_window || !creating) {
    if (form.custom_window) {
      if (!isDateText(form.effective_from)) {
        errors.effective_from = '生效起始日必须是 yyyy-MM-dd';
      }
      if (form.effective_to.trim()) {
        if (!isDateText(form.effective_to)) {
          errors.effective_to = '停止日必须是 yyyy-MM-dd（留空 = 至今有效）';
        } else if (isDateText(form.effective_from) && form.effective_to.trim() < form.effective_from.trim()) {
          errors.effective_to = '停止日不能早于生效起始日';
        }
      }
    } else if (!isPeriodText(form.period_code)) {
      errors.period_code = '期间必须是 yyyy-MM（如 2026-11）';
    }
  } else if (!isPeriodText(form.period_code)) {
    errors.period_code = '期间必须是 yyyy-MM（如 2026-11）';
  }

  const hourError = checkNumber(form.std_labor_hours, HOURS_SCALE, '标准工时');
  if (hourError) {
    errors.std_labor_hours = hourError;
  }
  const rateError = checkNumber(form.std_labor_rate, AMOUNT_SCALE, '标准工资率');
  if (rateError) {
    errors.std_labor_rate = rateError;
  }
  const overheadError = checkNumber(form.std_overhead_cost, AMOUNT_SCALE, '标准制造费用');
  if (overheadError) {
    errors.std_overhead_cost = overheadError;
  }

  if (form.remark.trim().length > REMARK_MAX) {
    errors.remark = `备注最长 ${REMARK_MAX} 个字符`;
  }

  if (form.items.length > ITEM_MAX) {
    errors.items = `标准 BOM 最多 ${ITEM_MAX} 行（当前 ${form.items.length} 行）`;
  } else {
    const seen = new Set<string>();
    for (const [index, item] of form.items.entries()) {
      const line = index + 1;
      if (!item.component_material_id) {
        errors.items = `第 ${line} 行：请选择耗用物料`;
        break;
      }
      if (seen.has(item.component_material_id)) {
        errors.items = `第 ${line} 行：同一耗用物料只能出现一次（要拆用量请合并到同一行）`;
        break;
      }
      seen.add(item.component_material_id);
      if (toScaled(item.std_qty, AMOUNT_SCALE) === null || (toScaled(item.std_qty, AMOUNT_SCALE) ?? 0n) <= 0n) {
        errors.items = `第 ${line} 行：标准用量必须 > 0（最多 ${AMOUNT_SCALE} 位小数）`;
        break;
      }
      if (toScaled(item.std_price, AMOUNT_SCALE) === null) {
        errors.items = `第 ${line} 行：标准单价必须 ≥ 0（最多 ${AMOUNT_SCALE} 位小数）`;
        break;
      }
    }
  }

  return errors;
}

/** 可空数值列的校验（留空合法 = 0；非空必须非负且不超精度）。 */
function checkNumber(text: string, scale: number, label: string): string | null {
  const value = text.trim();
  if (!value) {
    return null;
  }
  if (toScaled(value, scale) === null) {
    return `${label}必须是非负数字，最多 ${scale} 位小数（收到「${value}」）`;
  }
  return null;
}

/** 表单有错吗。 */
export function isValid(errors: StandardFormErrors): boolean {
  return Object.keys(errors).length === 0;
}

/** 表单里真的改过东西吗（用于「没有改动」的提示，避免一次无意义提交）。 */
export function isUnchanged(form: StandardFormState, detail: StandardDetailRes): boolean {
  const baseline = formFromDetail(detail);
  return (
    form.material_id === baseline.material_id &&
    form.period_code === baseline.period_code &&
    form.custom_window === baseline.custom_window &&
    form.effective_from === baseline.effective_from &&
    form.effective_to === baseline.effective_to &&
    form.clear_effective_to === baseline.clear_effective_to &&
    trimNumberText(form.std_labor_hours) === trimNumberText(baseline.std_labor_hours) &&
    trimNumberText(form.std_labor_rate) === trimNumberText(baseline.std_labor_rate) &&
    trimNumberText(form.std_overhead_cost) === trimNumberText(baseline.std_overhead_cost) &&
    form.status === baseline.status &&
    form.remark.trim() === baseline.remark.trim() &&
    form.items.length === baseline.items.length &&
    form.items.every(
      (item, index) =>
        item.component_material_id === baseline.items[index]?.component_material_id &&
        trimNumberText(item.std_qty) === trimNumberText(baseline.items[index]?.std_qty ?? '') &&
        trimNumberText(item.std_price) === trimNumberText(baseline.items[index]?.std_price ?? ''),
    )
  );
}

/** 数值文本归一（`2.50` 与 `2.5` 视为同一个数；空 → `0`）。 */
export function trimNumberText(text: string): string {
  const value = text.trim();
  if (!value) {
    return '0';
  }
  return trimZero(value);
}

/** 明细 → 提交用数组（**保留用户原文**，只 trim）。 */
export function toItemPayloads(items: ReadonlyArray<StandardFormItem>): StandardItemPayload[] {
  return items.map((item) => ({
    component_material_id: item.component_material_id,
    std_qty: item.std_qty.trim(),
    std_price: item.std_price.trim(),
  }));
}

/** 卡头三要素（留空的键**不发**：后端 `None` = 0）。 */
function headPayload(form: StandardFormState): {
  std_labor_hours?: string;
  std_labor_rate?: string;
  std_overhead_cost?: string;
} {
  const out: { std_labor_hours?: string; std_labor_rate?: string; std_overhead_cost?: string } = {};
  if (form.std_labor_hours.trim()) {
    out.std_labor_hours = form.std_labor_hours.trim();
  }
  if (form.std_labor_rate.trim()) {
    out.std_labor_rate = form.std_labor_rate.trim();
  }
  if (form.std_overhead_cost.trim()) {
    out.std_overhead_cost = form.std_overhead_cost.trim();
  }
  return out;
}

/**
 * 新建入参。
 *
 * 整期写法发 `period_code`（后端据此落「期初 ~ 期末」），自定义窗口才发两个日期 ——
 * 两组同时发会被后端判「矛盾」400（`window_from_req`）。
 */
export function toCreatePayload(form: StandardFormState): {
  material_id: string;
  period_code?: string;
  effective_from?: string;
  effective_to?: string;
  std_labor_hours?: string;
  std_labor_rate?: string;
  std_overhead_cost?: string;
  items: StandardItemPayload[];
  status?: number;
  remark?: string;
} {
  const payload: {
    material_id: string;
    period_code?: string;
    effective_from?: string;
    effective_to?: string;
    std_labor_hours?: string;
    std_labor_rate?: string;
    std_overhead_cost?: string;
    items: StandardItemPayload[];
    status?: number;
    remark?: string;
  } = {
    material_id: form.material_id.trim(),
    items: toItemPayloads(form.items),
    status: form.status,
    ...headPayload(form),
  };
  if (form.custom_window) {
    payload.effective_from = form.effective_from.trim();
    if (form.effective_to.trim()) {
      payload.effective_to = form.effective_to.trim();
    }
  } else {
    payload.period_code = form.period_code.trim();
  }
  if (form.remark.trim()) {
    payload.remark = form.remark.trim();
  }
  return payload;
}

/**
 * 修改入参（**已生效版本只发三样**：停止日 / 状态 / 备注）。
 *
 * 为什么锁改时不发数字与明细：后端 66109 除了拦「改口径字段」，还会校验
 * 「按明细重算的金额 == 库里金额」—— 发一份前端 trim 过的数字，一旦小数位不同
 * （`2.50` vs `2.5` 无碍，但 `12.345678` 被输入框截成 `12.35` 就有碍）就会被判成
 * 「静默重算历史金额」而整单拒绝。少发即安全。
 */
export function toUpdatePayload(
  form: StandardFormState,
  options: { locked: boolean; originalEffectiveTo: string | null },
): {
  material_id?: string;
  period_code?: string;
  effective_from?: string;
  effective_to?: string;
  clear_effective_to?: boolean;
  std_labor_hours?: string;
  std_labor_rate?: string;
  std_overhead_cost?: string;
  items?: StandardItemPayload[];
  status?: number;
  remark?: string;
} {
  const stopChanged = (form.effective_to.trim() || null) !== (options.originalEffectiveTo ?? null);
  const payload: {
    material_id?: string;
    period_code?: string;
    effective_from?: string;
    effective_to?: string;
    clear_effective_to?: boolean;
    std_labor_hours?: string;
    std_labor_rate?: string;
    std_overhead_cost?: string;
    items?: StandardItemPayload[];
    status?: number;
    remark?: string;
  } = {
    status: form.status,
    remark: form.remark.trim(),
  };

  if (form.custom_window) {
    if (stopChanged) {
      if (form.effective_to.trim()) {
        payload.effective_to = form.effective_to.trim();
      } else {
        payload.clear_effective_to = true;
      }
    }
  } else if (stopChanged) {
    if (form.effective_to.trim()) {
      payload.effective_to = form.effective_to.trim();
    } else {
      payload.clear_effective_to = true;
    }
  }

  if (options.locked) {
    return payload;
  }

  payload.material_id = form.material_id.trim();
  Object.assign(payload, headPayload(form));
  payload.items = toItemPayloads(form.items);
  if (form.custom_window) {
    payload.effective_from = form.effective_from.trim();
  } else {
    payload.period_code = form.period_code.trim();
  }
  return payload;
}

// ==================== 展示 ====================

/** 物料标称：`FG-1001 成品A`（编码缺失时退化成 id 前 8 位）。 */
export function materialLabel(row: {
  material_code?: string | null;
  material_name?: string | null;
  material_id?: string;
}): string {
  const code = row.material_code ?? (row.material_id ? row.material_id.slice(0, 8) : '—');
  return row.material_name ? `${code} ${row.material_name}` : code;
}

/** 物料类型中文（与后端 `MATERIAL_TYPES` 同口径）。 */
export function materialTypeLabel(type: string | null | undefined): string {
  switch (type) {
    case 'RAW':
      return '原材料';
    case 'SEMI':
      return '半成品';
    case 'FINISHED':
      return '成品';
    case 'SERVICE':
      return '服务';
    default:
      return type ?? '—';
  }
}

/** 行的「料 / 工 / 费 / 总」四列展示（后端已算好，这里只做形状统一）。 */
export function amountsOf(row: StandardRow): {
  material: string;
  labor: string;
  overhead: string;
  total: string;
} {
  return {
    material: row.std_material_cost,
    labor: row.std_labor_cost,
    overhead: row.std_overhead_cost,
    total: row.std_total_cost,
  };
}

/** 删除按钮能不能点（未生效过才可删；与后端 66110 同判据）。 */
export function canDelete(row: Pick<StandardRow, 'effective_from'>, today: string): boolean {
  return !isLockedEffective(row, today);
}

// ==================== 发布轴（M5-10「审核后生效」） ====================

/**
 * 发布动作闸（纯逻辑，与后端 `publish_guard` 同判据）。
 *
 * | 当前发布状态 | 提交发布 | 撤回 | 说明 |
 * |---|:--:|:--:|---|
 * | `DRAFT` | ✅ | ❌ | 草稿：还没提交 / 被驳回 / 撤回后的状态 |
 * | `PENDING` | ❌ | ✅ | 审批中：只能等，或发起人撤回 |
 * | `PUBLISHED` | ❌ | ❌ | 已发布：要换口径请新增一版 |
 *
 * **只判发布状态，不判 `status`**：`status` 是「参与期间取数」那条轴，
 * 草稿卡的 `status` 也可能是 1（占住生效窗口等审批），不能拿它当「已发布」。
 */
export interface PublishGate {
  /** 能不能提交发布审批 */
  canSubmit: boolean;
  /** 能不能撤回发布审批 */
  canWithdraw: boolean;
  /** 当前发布状态（归一成大写；未知值原样返回） */
  state: string;
  /** 提交按钮的 tooltip（不能点时说明原因与出路） */
  submitHint: string;
  /** 撤回按钮的 tooltip */
  withdrawHint: string;
}

/** 发布状态归一（大小写 / 空白都容忍；与后端 `is_publish_state` 同口径）。 */
export function normalizePublishState(state: string | null | undefined): string {
  return (state ?? '').trim().toUpperCase();
}

/** 计算一行的发布动作闸。 */
export function publishGate(row: Pick<StandardRow, 'publish_state'>): PublishGate {
  const state = normalizePublishState(row.publish_state);
  const canSubmit = state === 'DRAFT';
  const canWithdraw = state === 'PENDING';
  let submitHint = '提交发布审批（财务经理审批通过后这一版才参与差异分析取数）';
  if (state === 'PENDING') {
    submitHint = '已经在审批中：等审批结束，或先「撤回」再改';
  } else if (state === 'PUBLISHED') {
    submitHint = '已发布：要换口径请新增一版（历史版本是差异分析的依据，不许回头改）';
  } else if (!canSubmit) {
    submitHint = `发布状态「${state || '未知'}」不接受提交`;
  }
  const withdrawHint = canWithdraw
    ? '撤回发布申请（只有发起人本人能撤；撤回后回草稿，可改完重提）'
    : '只有「审批中」才能撤回';
  return { canSubmit, canWithdraw, state, submitHint, withdrawHint };
}

/** 「失效」这件事的后果说明（后端会在 `warnings` 里再补一遍）。 */
export const INACTIVE_CONSEQUENCE =
  '失效后这一版不再参与期间取数：该期间若没有别的启用版本，差异分析会「取不到标准」而跳过';

/** 「生效」这件事的前置说明。 */
export const ACTIVE_CONSEQUENCE = '生效要求同物料已有启用版本不与它的生效期重叠（重叠会被后端 409 拒绝）';

/** 已生效版本被锁的提示文案（表单里显示，说明为什么这些控件是灰的）。 */
export const LOCKED_HINT =
  '这一版已经生效过（生效起始日早于今天）：只能改「停止日 / 状态 / 备注」。要换口径请新增一版，或先把这一版的停止日收口到切换日前一天。';

// ==================== 导入 ====================

/** 导入结果摘要（「全有或全无」必须一句话说清）。 */
export function importSummaryText(result: StandardImportResult): string {
  if (result.success > 0 || result.failed === 0) {
    return `导入成功：${result.success} 行 → 新建 ${result.versions_created} 版标准成本`;
  }
  const lines = new Set(result.rows.map((issue) => issue.line)).size;
  return `整批未入库（全有或全无）：${lines} 行有 ${result.rows.length} 处错误，一条都没写入，改完再传`;
}

/** 导入结果的主题（成功绿 / 拒绝红，不用「警告黄」—— 铁律是「一条都没入库」）。 */
export function importSummaryTheme(result: StandardImportResult): 'success' | 'error' {
  return result.success > 0 || result.failed === 0 ? 'success' : 'error';
}

/** 导入错误表的数据（行号排序 + 列名兜底）。 */
export function importIssueRows(result: StandardImportResult): Array<ImportIssue & { key: string }> {
  return [...result.rows]
    .sort((left, right) => left.line - right.line)
    .map((issue, index) => ({ ...issue, key: `${issue.line}-${issue.column}-${index}` }));
}

/** 导入对话框的填写须知（与后端 9 列模板的「填写须知」sheet 同口径）。 */
export const IMPORT_GUIDE = [
  '第 1 行是表头，第 2 行起是数据；模板第 2 行是示例，导入前请删除（系统也会自动跳过与示例完全一致的行）。',
  '一组「物料编码 + 期间」= 一版标准成本；组内一行 = 一条标准 BOM 明细。',
  '组内各行的「标准工时 / 标准工资率 / 标准制造费用 / 备注」必须一致，不一致整批拒绝。',
  '「耗用物料编码 / 标准用量 / 标准单价」要么全空（无材料耗用），要么全填。',
  '导入只**新建**版本：同物料该期间已有启用版本会报错（先失效旧版或在维护页改）。',
  '任何一行有错 ⇒ 整批一条都不入库，错误按「Excel 行号 + 列名 + 原因」返回。',
] as const;
