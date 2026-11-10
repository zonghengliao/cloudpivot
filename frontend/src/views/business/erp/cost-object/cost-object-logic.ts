/**
 * 成本对象页的纯逻辑（ARCH-9 §6.2.1 / §4.1）。
 *
 * 抽成纯函数的理由与 `asset-logic.ts` / `period-logic.ts` 一致：**判据要能单测**。
 * 「哪个状态能编辑」「必填项缺哪个」「编辑只带真改过的字段」「关批要不要二次确认」
 * 这些一旦漂移，`vue-tsc` 抓不到、`.vue` 也不好测 —— 写成函数就能一条一条断言。
 *
 * 本文件**不 import 任何请求层**：`api/request.ts` 顶层读 `import.meta.env`，
 * 在 `tsx --test` 下会崩；错误对象按结构取 `code` / `message`（与 `ApiError` 同形）。
 *
 * # 校验口径（镜像后端 `cost_object_dto::spec_from_create` 的 400 文案）
 *
 * 后端会拦的，前端先拦一道（省一次往返，且能就地标红）：
 * `product_id` 必填、`target_warehouse_id` 必填、`plan_qty > 0`、
 * `workshop_dept_id` 必填、`start_date` 必填且 `due_date >= start_date`。
 * `material_input_mode='AS_PROGRESS'` 后端直接 400（一期不支持），页面置灰。
 */

// 只依赖词表文件（不 import 请求层）：`cost-object.ts` 顶层引 `@/api/request`，
// 而 `request.ts` 读 `import.meta.env` —— 在 `tsx --test` 下会崩。
import {
  CLOSE_BLOCK_CODES,
  CLOSE_BLOCK_HINTS,
  COST_OBJECT_CLOSE_BLOCKED_CODE,
  COST_OBJECT_STATUS_LABELS,
  MATERIAL_INPUT_MODE_LABELS,
  type CloseBlockCode,
  type CostObjectCreatePayload,
  type CostObjectDetail,
  type CostObjectRow,
  type CostObjectStatus,
  type CostObjectUpdatePayload,
  type MaterialInputMode,
} from '@/api/business/erp/cost-object-kinds';
import { todayString } from '@/views/business/erp/shared/money';

// ==================== 权限码（V0378 种子，逐字一致） ====================

/** 本页权限码 */
export const COST_OBJECT_PERMISSIONS = {
  /** 列表 / 详情 */
  list: 'business:erp:cost-object:list',
  /** 新建 */
  create: 'business:erp:cost-object:create',
  /** 编辑 */
  update: 'business:erp:cost-object:update',
  /** 关批 */
  close: 'business:erp:cost-object:close',
} as const;

/** 列表默认每页条数（与后端 `DEFAULT_PAGE_SIZE` 同口径） */
export const PAGE_SIZE = 20;
/** 下拉一次拉多少（物料 / 客户 / 仓库） */
export const OPTION_LIMIT = 200;

// ==================== 展示小件 ====================

/** 状态 → 中文（后端不返回 `status_label`，成本对象这层译名只在前端） */
export function statusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return COST_OBJECT_STATUS_LABELS[status as CostObjectStatus] ?? status;
}

/** 状态 → TDesign Tag 主题 */
export function statusTheme(status: string | null | undefined): 'success' | 'default' | 'danger' {
  switch (status) {
    case 'OPEN':
      return 'success';
    case 'CLOSED':
      return 'default';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 投料方式 → 中文 */
export function inputModeLabel(mode: string | null | undefined): string {
  if (!mode) {
    return '—';
  }
  return MATERIAL_INPUT_MODE_LABELS[mode as MaterialInputMode] ?? mode;
}

/**
 * 数量格式化：去掉无意义的尾随 0（`"100.0000"` → `"100"`，`"12.5000"` → `"12.5"`）。
 *
 * 数量是 `NUMERIC(18,4)`，后端出参固定在 4 位小数；直接显示 `100.0000` 在
 * 「计划 / 完工 / 在制」三列里非常吵。**金额不要走这里**（金额要走 `formatMoney`，
 * 两位小数 + 千分位）。
 */
export function formatQty(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '0';
  }
  const text = String(value).trim();
  if (!/^-?\d+(\.\d+)?$/.test(text)) {
    return text;
  }
  const trimmed = text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text;
  return trimmed === '-0' ? '0' : trimmed;
}

/** 数量是否 > 0（字符串安全，不走浮点累加） */
export function isPositiveQty(value: string | number | null | undefined): boolean {
  const num = Number(value ?? 0);
  return Number.isFinite(num) && num > 0;
}

/** 数量是否可视为 0（四位小数口径内，容差 0.00005） */
export function isZeroQty(value: string | number | null | undefined): boolean {
  const num = Number(value ?? 0);
  return !Number.isFinite(num) || Math.abs(num) < 0.00005;
}

// ==================== 期间筛选：月份 → 开工日期区间 ====================

/**
 * 把「开工期间 `yyyy-MM`」折算成列表接口认的 `start_from` / `start_to`。
 *
 * 后端 `CostObjectPageQuery` **没有** `period_code`，只有开工日期区间
 * （§5.2 #1 的 `start_from` / `start_to`）—— 页面上的月份筛选就落在这两个参数上。
 *
 * 非法月份返回空对象（= 不带日期条件），不抛错：筛选框清空后不该让页面炸。
 */
export function monthRange(month: string | null | undefined): {
  start_from?: string;
  start_to?: string;
} {
  const text = (month ?? '').trim();
  const matched = /^(\d{4})-(0[1-9]|1[0-2])$/.exec(text);
  if (!matched) {
    return {};
  }
  const year = Number(matched[1]);
  const monthNo = Number(matched[2]);
  // 下个月 1 号减一天 = 本月最后一天；用 Date.UTC 避免本地时区把 1 号推成上个月末
  const lastDay = new Date(Date.UTC(year, monthNo, 0)).getUTCDate();
  return {
    start_from: `${matched[1]}-${matched[2]}-01`,
    start_to: `${matched[1]}-${matched[2]}-${String(lastDay).padStart(2, '0')}`,
  };
}

// ==================== 表单状态 ====================

/** 建 / 改共用表单状态（全字符串，v-model 直接绑） */
export interface CostObjectFormState {
  /** 产出品 */
  product_id: string;
  /** 目标入库仓 */
  target_warehouse_id: string;
  /** 目标库位（可空，跟仓库联动） */
  target_location_id: string;
  /** 计划数量 */
  plan_qty: string;
  /** 生产车间 */
  workshop_dept_id: string;
  /** 开工日期 `YYYY-MM-DD` */
  start_date: string;
  /** 计划完工日期（可空） */
  due_date: string;
  /** 上游订单号（可空） */
  source_doc_no: string;
  /** 客户（可空） */
  customer_id: string;
  /** 产出批号（可空） */
  lot_no: string;
  /** 投料方式（一期只有 `ONE_TIME`） */
  material_input_mode: MaterialInputMode;
  /** 备注（可空） */
  remark: string;
}

/** 表单错误（字段 → 人话；空对象 = 通过） */
export type CostObjectFormErrors = Partial<Record<keyof CostObjectFormState, string>>;

/** 空表单（开工日期默认北京时间今天；投料方式默认一次投入） */
export function emptyCostObjectForm(today: string = todayString()): CostObjectFormState {
  return {
    product_id: '',
    target_warehouse_id: '',
    target_location_id: '',
    plan_qty: '',
    workshop_dept_id: '',
    start_date: today,
    due_date: '',
    source_doc_no: '',
    customer_id: '',
    lot_no: '',
    material_input_mode: 'ONE_TIME',
    remark: '',
  };
}

/** 详情 → 表单（编辑回填） */
export function formFromDetail(detail: CostObjectDetail): CostObjectFormState {
  return {
    product_id: detail.product_id,
    target_warehouse_id: detail.target_warehouse_id,
    target_location_id: detail.target_location_id ?? '',
    plan_qty: formatQty(detail.plan_qty),
    workshop_dept_id: detail.workshop_dept_id,
    start_date: detail.start_date,
    due_date: detail.due_date ?? '',
    source_doc_no: detail.source_doc_no ?? '',
    customer_id: detail.customer_id ?? '',
    lot_no: detail.lot_no ?? '',
    material_input_mode: detail.material_input_mode,
    remark: detail.remark ?? '',
  };
}

/** `YYYY-MM-DD` 形状 + 真实日期（与后端 `parse_date` 同口径） */
export function isDateString(value: string | null | undefined): boolean {
  const text = (value ?? '').trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(text)) {
    return false;
  }
  const [year, month, day] = text.split('-').map(Number);
  const probe = new Date(Date.UTC(year, month - 1, day));
  return (
    probe.getUTCFullYear() === year &&
    probe.getUTCMonth() === month - 1 &&
    probe.getUTCDate() === day
  );
}

/**
 * 表单校验（必填 + 数量 + 日期先后）。
 *
 * `collected=true`（该对象已有归集行）时 `product_id` 只读，页面传 `false` 让校验跳过它 ——
 * 不是「放宽」，而是后端已有归集就拒绝改产出品（409），前端不该把只读字段判成错误。
 */
export function validateCostObjectForm(
  form: CostObjectFormState,
  options: { collected?: boolean } = {},
): CostObjectFormErrors {
  const errors: CostObjectFormErrors = {};
  if (!form.product_id) {
    errors.product_id = '产出品必填：完工入库按这个物料记账';
  }
  if (!form.workshop_dept_id) {
    errors.workshop_dept_id = '生产车间必填：料工费按车间归集与分摊';
  }
  if (!form.target_warehouse_id) {
    errors.target_warehouse_id = '目标仓必填：结算时完工入库要落到这个仓';
  }
  const qty = form.plan_qty.trim();
  if (!qty) {
    errors.plan_qty = '计划数量必填';
  } else if (!/^\d+(\.\d{1,4})?$/.test(qty)) {
    errors.plan_qty = '计划数量最多 4 位小数，且不能为负';
  } else if (!isPositiveQty(qty)) {
    errors.plan_qty = '计划数量必须大于 0';
  }
  if (!form.start_date.trim()) {
    errors.start_date = '开工日期必填：它决定成本归属期间';
  } else if (!isDateString(form.start_date)) {
    errors.start_date = '开工日期格式必须是 YYYY-MM-DD';
  }
  if (form.due_date.trim()) {
    if (!isDateString(form.due_date)) {
      errors.due_date = '计划完工日期格式必须是 YYYY-MM-DD';
    } else if (isDateString(form.start_date) && form.due_date < form.start_date) {
      errors.due_date = '计划完工日期不能早于开工日期';
    }
  }
  if (form.remark.length > 2000) {
    errors.remark = '备注最长 2000 字符';
  }
  if (options.collected) {
    delete errors.product_id;
  }
  return errors;
}

/** 错误数（0 = 通过；页面用它决定要不要发请求） */
export function errorCount(errors: CostObjectFormErrors): number {
  return Object.keys(errors).length;
}

/** 空串 → `undefined`（后端把 `""` 当值，别把空串写进可空列） */
function optional(value: string): string | undefined {
  const text = value.trim();
  return text ? text : undefined;
}

/** 表单 → 新建 payload（必填字段已过 `validateCostObjectForm`） */
export function toCreatePayload(form: CostObjectFormState): CostObjectCreatePayload {
  const payload: CostObjectCreatePayload = {
    product_id: form.product_id,
    target_warehouse_id: form.target_warehouse_id,
    plan_qty: form.plan_qty.trim(),
    workshop_dept_id: form.workshop_dept_id,
    start_date: form.start_date.trim(),
    material_input_mode: form.material_input_mode,
  };
  // 可空字段：空串一律不发（后端把 `""` 当值，会把可空列写成空字符串）
  const location = optional(form.target_location_id);
  if (location) {
    payload.target_location_id = location;
  }
  const due = optional(form.due_date);
  if (due) {
    payload.due_date = due;
  }
  const sourceDocNo = optional(form.source_doc_no);
  if (sourceDocNo) {
    payload.source_doc_no = sourceDocNo;
  }
  const customerId = optional(form.customer_id);
  if (customerId) {
    payload.customer_id = customerId;
  }
  const lotNo = optional(form.lot_no);
  if (lotNo) {
    payload.lot_no = lotNo;
  }
  const remark = optional(form.remark);
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

/**
 * 表单 → **编辑** payload：只带**真改过**的字段（后端 PUT 是部分更新 + 三态）。
 *
 * 三类处理，一个都不能错：
 * 1. 字段没改 ⇒ **不出现在 payload 里**（缺失 = 不改）；
 * 2. 可空字段原来是空的、现在还是空 ⇒ 也不带（省一次无意义写入）；
 * 3. 可空字段原来是**有值**、现在被清空 ⇒ 显式 `null`（清空）。传 `undefined` /
 *    空串都是「不改」，用户会以为清掉了而库里还在。
 *
 * 返回空对象表示「没有任何改动」，页面据此提示并不发请求。
 */
export function toUpdatePayload(
  origin: CostObjectDetail,
  form: CostObjectFormState,
): CostObjectUpdatePayload {
  const payload: CostObjectUpdatePayload = {};

  if (form.product_id && form.product_id !== origin.product_id) {
    payload.product_id = form.product_id;
  }
  if (form.target_warehouse_id && form.target_warehouse_id !== origin.target_warehouse_id) {
    payload.target_warehouse_id = form.target_warehouse_id;
  }
  if (form.workshop_dept_id && form.workshop_dept_id !== origin.workshop_dept_id) {
    payload.workshop_dept_id = form.workshop_dept_id;
  }
  if (form.plan_qty.trim() && formatQty(form.plan_qty) !== formatQty(origin.plan_qty)) {
    payload.plan_qty = form.plan_qty.trim();
  }
  if (form.start_date.trim() && form.start_date.trim() !== origin.start_date) {
    payload.start_date = form.start_date.trim();
  }
  if (form.material_input_mode !== origin.material_input_mode) {
    payload.material_input_mode = form.material_input_mode;
  }

  // 可空字段：三态（缺失 / null 清空 / 有值）
  const nextLocation = optional(form.target_location_id) ?? null;
  if (nextLocation !== (origin.target_location_id ?? null)) {
    payload.target_location_id = nextLocation;
  }
  const nextDue = optional(form.due_date) ?? null;
  if (nextDue !== (origin.due_date ?? null)) {
    payload.due_date = nextDue;
  }
  const nextSourceDoc = optional(form.source_doc_no) ?? null;
  if (nextSourceDoc !== (origin.source_doc_no ?? null)) {
    payload.source_doc_no = nextSourceDoc;
  }
  const nextCustomer = optional(form.customer_id) ?? null;
  if (nextCustomer !== (origin.customer_id ?? null)) {
    payload.customer_id = nextCustomer;
  }
  const nextLot = optional(form.lot_no) ?? null;
  if (nextLot !== (origin.lot_no ?? null)) {
    payload.lot_no = nextLot;
  }
  const nextRemark = optional(form.remark) ?? null;
  if (nextRemark !== (origin.remark ?? null)) {
    payload.remark = nextRemark;
  }
  return payload;
}

/** payload 是否有字段（页面据此提示「没有改动」） */
export function isEmptyPayload(payload: object): boolean {
  return Object.keys(payload).length === 0;
}

// ==================== 按钮态（§6.2.1） ====================

/**
 * 能否编辑：只有 `OPEN` 能改（`CLOSED` / `CANCELLED` 只读）。
 *
 * `CANCELLED` 是后端状态机的终态之一，改它没有任何意义（结算是按 `OPEN` 的对象算的）。
 */
export function canEditObject(status: string | null | undefined): boolean {
  return status === 'OPEN';
}

/** 能否关批：只有 `OPEN`，且后端有效边里确实有 `OPEN → CLOSED` */
export function canCloseObject(
  detail: Pick<CostObjectDetail, 'status' | 'next_statuses'> | null | undefined,
): boolean {
  if (!detail || detail.status !== 'OPEN') {
    return false;
  }
  const next = detail.next_statuses ?? [];
  // 有效边被管理端禁用时 next_statuses 里就没有 CLOSED —— 别硬给按钮（点了必 409）
  return next.length === 0 ? true : next.includes('CLOSED');
}

/** 行级：列表行没有 `next_statuses`，按状态判断（与后端 §4.1 三条边一致） */
export function canCloseRow(row: Pick<CostObjectRow, 'status'> | null | undefined): boolean {
  return row?.status === 'OPEN';
}

/** 是否有归集（`collect_summary.total != 0` 即认为有；数量与金额都用字符串比） */
export function hasCollection(detail: Pick<CostObjectDetail, 'collect_summary'> | null): boolean {
  if (!detail) {
    return false;
  }
  return !isZeroQty(detail.collect_summary?.total ?? '0');
}

// ==================== 关批二次确认 ====================

/**
 * 是否必须「带在制关批」（§4.1 前置 ⑤）：期末在制数量 > 0。
 *
 * `true` 时页面必须先让用户勾「我确认带在制关批」，请求才带 `force_wip: true`；
 * 不勾就发请求 → 后端 409 + `66055`（`[WIP_NOT_ZERO]`）。
 */
export function requiresForceWip(detail: Pick<CostObjectDetail, 'wip_qty'> | null): boolean {
  if (!detail) {
    return false;
  }
  return !isZeroQty(detail.wip_qty);
}

/** 关批确认框里逐条列的事实（用户拿它对账，不是装饰） */
export interface CloseSummary {
  /** 对象号 */
  object_no: string;
  /** 产出品 + 批号 */
  product: string;
  /** 归集总成本（字符串，原样展示） */
  collect_total: string;
  /** 期末在制数量 */
  wip_qty: string;
  /** 是否要显式确认带在制 */
  need_force_wip: boolean;
  /** 关批后不可逆的提示行 */
  irreversible_hint: string;
}

/** 组装关批确认摘要（纯函数，单测直接断言） */
export function closeSummaryOf(detail: CostObjectDetail): CloseSummary {
  const product = [detail.product_name ?? detail.product_id, detail.lot_no]
    .filter((part): part is string => Boolean(part))
    .join(' / ');
  return {
    object_no: detail.object_no,
    product: product || '—',
    collect_total: detail.collect_summary?.total ?? '0',
    wip_qty: formatQty(detail.wip_qty),
    need_force_wip: requiresForceWip(detail),
    irreversible_hint:
      '关批后不可回「在产」（§4.1：CLOSED 不可逆，要复工请新建批），也不再领料 / 报工。',
  };
}

/** 关批请求体（勾了带在制才带 `force_wip`，否则不发这个字段） */
export function toClosePayload(
  options: { forceWip?: boolean; remark?: string } = {},
): { force_wip?: boolean; remark?: string } {
  const payload: { force_wip?: boolean; remark?: string } = {};
  if (options.forceWip === true) {
    payload.force_wip = true;
  }
  const remark = (options.remark ?? '').trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

// ==================== 错误文案 ====================

/** 结构化的错误（`ApiError` 同形，不 import 请求层） */
interface ErrorLike {
  code?: unknown;
  message?: unknown;
  status?: unknown;
}

/**
 * 错误 → 人话。
 *
 * * 业务码 `66055`（关批被拦）：`msg` 已经逐条列出 5 条前置里不过的那几条，
 *   这里**只追加「去哪修」的指引**，不解析中文；
 * * 其它错误：原样返回 `msg`（后端文案本身已带「怎么修」）。
 */
export function closeErrorText(error: unknown): string {
  if (typeof error === 'string') {
    return error.trim();
  }
  if (!error || typeof error !== 'object') {
    return '关批失败，请重试';
  }
  const like = error as ErrorLike;
  const message = typeof like.message === 'string' ? like.message.trim() : '';
  const code = typeof like.code === 'number' ? like.code : 0;
  if (code === COST_OBJECT_CLOSE_BLOCKED_CODE) {
    const missing = CLOSE_BLOCK_CODES.filter((item) => message.includes(`[${item}]`));
    const hints = missing.map((item) => CLOSE_BLOCK_HINTS[item]).join('；');
    return hints ? `${message}（怎么修：${hints}）` : message;
  }
  return message || '关批失败，请重试';
}

/** 拦截原因 → 指引（页面在确认框里逐条展示时用） */
export function blockHint(code: CloseBlockCode): string {
  return CLOSE_BLOCK_HINTS[code];
}
