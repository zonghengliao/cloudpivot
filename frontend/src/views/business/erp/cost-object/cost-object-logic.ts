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
  COST_OBJECT_PERIOD_LOCKED_CODE,
  COST_OBJECT_STATUS_LABELS,
  MATERIAL_INPUT_MODE_LABELS,
  SOURCE_DOC_STATUS_LABELS,
  SOURCE_DOC_TYPE_LABELS,
  type CloseBlockCode,
  type CostObjectCreatePayload,
  type CostObjectDetail,
  type CostObjectOutputPayload,
  type CostObjectRow,
  type CostObjectSourceCheck,
  type CostObjectSourceDoc,
  type CostObjectSourceTotals,
  type CostObjectSources,
  type CostObjectStatus,
  type CostObjectUpdatePayload,
  type MaterialInputMode,
  type SourceDocType,
} from '@/api/business/erp/cost-object-kinds';
import { resolveSourceRoute } from '@/views/business/erp/shared/source-route';
import { formatMoney, todayString } from '@/views/business/erp/shared/money';

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
  /**
   * 一键领料（B4-3）：跳到出库单新建页预填。
   *
   * 刻意用**出库单的建单权限**而不是成本对象的权限：这个按钮的最终动作是
   * `POST /business/erp/stock-out`，没有建单权限的人点进去只会在保存时 403。
   * 预填接口 `/stock-out/cost-object-issue-prefill` 也是按同一个码授权的。
   */
  issue: 'business:erp:stock-out:create',
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

/**
 * 错误数（0 = 通过；页面用它决定要不要发请求）。
 *
 * 参数放宽成「字符串键 → 可能为空」的任意记录：建 / 改表单（`CostObjectFormErrors`）
 * 与产量面板（`OutputFormErrors`）共用同一个计数口径，不必写两遍。
 */
export function errorCount(errors: Record<string, string | undefined>): number {
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

/**
 * 能否一键领料（B4-3）：只有 `OPEN`（在制）的对象能领料。
 *
 * 依据：§4.1 状态表「`OPEN` → 可领料」；`CLOSED` 的成本已结转到完工入库，
 * 再领料会与在制余额对不上（后端建单侧 409 + `66057` 还会再拦一次，这里只是
 * 不给按钮 —— 点了必然失败的事不该让用户点）。
 */
export function canIssueMaterial(
  object: Pick<CostObjectDetail, 'status'> | Pick<CostObjectRow, 'status'> | null | undefined,
): boolean {
  return object?.status === 'OPEN';
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

// ==================== 本期产量与在制（B2-14 · §4.5.1 产量/进度行） ====================
//
// 为什么这三个数单独一块（不进建 / 改表单）：
// * 语义是「本期实际干出来多少」，跨月要反复改，而主数据基本不动；
// * 后端守卫更严（该对象参与过的期间**已结算**就只读），前端要把这条说清楚；
// * 设计 §8.3 R-4 要求留「填写人 + 时间」—— 面板上要能显示上次是谁填的。

/** 面板表单状态（三个数都是字符串，`v-model` 直接绑） */
export interface OutputFormState {
  /** 累计完工数量（不是本期增量） */
  completed_qty: string;
  /** 期末在制数量 */
  wip_qty: string;
  /** 期末在制完工进度 0–100 */
  completion_pct: string;
}

/** 字段级错误（面板就地标红用） */
export type OutputFormErrors = Partial<Record<keyof OutputFormState, string>>;

/** 数量输入形态：非负、最多 4 位小数（列是 `NUMERIC(18,4)`） */
const QTY_INPUT = /^\d+(\.\d{1,4})?$/;
/** 进度输入形态：最多 2 位小数（列是 `NUMERIC(5,2)`）；上限 100 另判 */
const PCT_INPUT = /^\d+(\.\d{1,2})?$/;

/** 空面板（三个 0，与库默认一致） */
export function emptyOutputForm(): OutputFormState {
  return { completed_qty: '0', wip_qty: '0', completion_pct: '0' };
}

/**
 * 详情 → 面板初值。
 *
 * 走 `formatQty` 去尾零（后端出参是 `"80.0000"`，直接塞输入框很吵，且用户看不出
 * 该不该补 `0`）。**不做四舍五入**：值本来就是 4 位以内。
 */
export function outputFormFromDetail(
  detail:
    | Pick<CostObjectDetail, 'completed_qty' | 'wip_qty' | 'completion_pct'>
    | null
    | undefined,
): OutputFormState {
  if (!detail) {
    return emptyOutputForm();
  }
  return {
    completed_qty: formatQty(detail.completed_qty),
    wip_qty: formatQty(detail.wip_qty),
    completion_pct: formatQty(detail.completion_pct),
  };
}

/**
 * 面板校验（镜像后端 `spec_from_output` 的 400 文案）。
 *
 * 与后端**逐条同口径**，多一条都不加（前端拦了后端放行 = 用户被自己页面卡死）：
 *
 * 1. 累计完工 / 期末在制 必须 ≥ 0 且最多 4 位小数；
 * 2. 完工进度 0–100 且最多 2 位小数；
 * 3. `wip_qty > 0` ⇒ 进度必须 > 0（calc 第 6 段 `66067` 的同一判据）。
 */
export function validateOutputForm(form: OutputFormState): OutputFormErrors {
  const errors: OutputFormErrors = {};
  const completed = form.completed_qty.trim();
  if (!QTY_INPUT.test(completed)) {
    errors.completed_qty = '累计完工要 ≥ 0，最多 4 位小数';
  }
  const wip = form.wip_qty.trim();
  if (!QTY_INPUT.test(wip)) {
    errors.wip_qty = '期末在制要 ≥ 0，最多 4 位小数';
  }
  const pct = form.completion_pct.trim();
  if (!PCT_INPUT.test(pct) || Number(pct) > 100) {
    errors.completion_pct = '完工进度要在 0–100 之间，最多 2 位小数';
  } else if (!errors.wip_qty && isPositiveQty(wip) && isZeroQty(pct)) {
    // 与后端 66067 同一口径：不然用户填完、跑期间计算时才会被拒
    errors.completion_pct = '有期末在制就必须填完工进度（> 0），否则跑期间计算会被 66067 拦下';
  }
  return errors;
}

/**
 * 面板 → 请求体：**三个数全传**。
 *
 * 后端「字段缺失 = 不改」的语义更灵活，但面板上三个数都显示、用户看到的就是全量，
 * 全传最不容易误会（不会出现「我以为清空了在制，其实没传这个字段」）。
 */
export function toOutputPayload(form: OutputFormState): CostObjectOutputPayload {
  return {
    completed_qty: form.completed_qty.trim(),
    wip_qty: form.wip_qty.trim(),
    completion_pct: form.completion_pct.trim(),
  };
}

/**
 * 面板保存的错误 → 人话。
 *
 * * `66070`（期间已结算 ⇒ 产量只读）：`msg` 已点名期间，这里只追加「去哪修」；
 * * 其它（400 值域 / 409 已关批）：原样返回 `msg`。
 */
export function outputErrorText(error: unknown): string {
  if (typeof error === 'string') {
    const text = error.trim();
    return text || '保存失败，请重试';
  }
  if (!error || typeof error !== 'object') {
    return '保存失败，请重试';
  }
  const like = error as ErrorLike;
  const message = typeof like.message === 'string' ? like.message.trim() : '';
  const code = typeof like.code === 'number' ? like.code : 0;
  if (code === COST_OBJECT_PERIOD_LOCKED_CODE) {
    const hint = '到「成本核算」把该期间的成本批次红冲（或撤回）后再回来改产量';
    return message ? `${message}（怎么修：${hint}）` : `期间已结算，产量只读（怎么修：${hint}）`;
  }
  return message || '保存失败，请重试';
}

// ==================== ⑧ 料从哪来 / 货去哪了（B4-2） ====================

/**
 * 反查行的单据状态 → 中文。
 *
 * 出库单与入库单是两套状态词，撞在同一个 `status` 字段上；不认识的值原样返回
 * （不编「未知」—— 后端加了新状态，页面至少还能看出那个英文词）。
 */
export function sourceStatusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return SOURCE_DOC_STATUS_LABELS[status] ?? status;
}

/** 反查行的类型 → 中文。 */
export function sourceDocTypeLabel(docType: string | null | undefined): string {
  if (!docType) {
    return '—';
  }
  return SOURCE_DOC_TYPE_LABELS[docType as SourceDocType] ?? docType;
}

/**
 * 一条领料单的「归集情况」文案（页面上就是这一句话解释「这笔料进没进成本」）。
 *
 * 四种情形（与后端 `to_sources_res` 的判定同口径，**不另算一套**）：
 *
 * * `POSTED` + `collected` ⇒ 「已归集 · 2026-11」；
 * * `POSTED` + 未归集 ⇒ 「未归集（已归集 6 / 10）」—— 差额摆出来，用户知道缺多少；
 * * 非 `POSTED` ⇒ 「{状态}，未过账不计入成本」；
 * * 入库单（`collected=true` 表示有效） ⇒ 「有效」/「已作废」。
 */
export function sourceDocCollectedText(row: CostObjectSourceDoc): string {
  if (row.doc_type === 'STOCK_IN') {
    return row.collected ? '已入账' : sourceStatusLabel(row.status);
  }
  if (row.status !== 'POSTED') {
    return `${sourceStatusLabel(row.status)}，不计入成本`;
  }
  if (row.collected) {
    const periods = row.period_codes.length ? ` · ${row.period_codes.join(' / ')}` : '';
    return `已归集${periods}`;
  }
  return `未归集（已归集 ${formatQty(row.collected_qty)} / ${formatQty(row.qty)}）`;
}

/** 归集情况对应的 TDesign 状态色。 */
export function sourceDocCollectedTheme(
  row: CostObjectSourceDoc,
): 'success' | 'warning' | 'default' | 'danger' {
  if (row.doc_type === 'STOCK_IN') {
    return row.collected ? 'success' : 'danger';
  }
  if (row.status !== 'POSTED') {
    return 'default';
  }
  return row.collected ? 'success' : 'warning';
}

/**
 * 一行 → 原单据的跳转目标（**用 `shared/source-route` 的映射表，不自己拼路径**）。
 *
 * 两个来源键早就在表里：
 * * `STOCK_OUT` → 出库页 `?out_no=`（半深链：按单号筛到那一行）；
 * * `STOCK_IN` → 收货入库页 `?in_id=`（真深链：直接开详情）。
 *
 * 返回 `null` = 这一行这次跳不了（映射缺失 / 参数缺失），页面渲染纯文本，
 * **不给 404 假链接**（表的设计原则，见 `source-route.ts` 头注释）。
 */
export function sourceDocRoute(
  row: Pick<CostObjectSourceDoc, 'doc_type' | 'id' | 'doc_no'>,
): { path: string; query: Record<string, string> } | null {
  const route = resolveSourceRoute(row.doc_type, row.id, row.doc_no);
  return route ? { path: route.path, query: route.query } : null;
}

/** 一组单据合计 → 一行文案（「3 张 · 5 行」+ 数量 / 金额另列，这里只给张数行数）。 */
export function sourceTotalsCountText(totals: CostObjectSourceTotals | null | undefined): string {
  // 反查还没回来（`sources === null`）时标题也在渲染 ⇒ 允许空值，给「—」而不是「undefined 张」
  if (!totals) {
    return '—';
  }
  return `${totals.doc_count} 张 · ${totals.line_count} 行`;
}

/**
 * 判据汇总：`{ failed, text }`。
 *
 * 全绿 ⇒ 「5 条判据全部对得上」；有红 ⇒ 「2 条对不上：领料金额 = 归集直接材料、…」，
 * 直接点名是哪几条，用户不用一条条找。
 */
export function sourceChecksSummary(checks: CostObjectSourceCheck[]): {
  total: number;
  failed: number;
  allOk: boolean;
  text: string;
} {
  const failedChecks = checks.filter((check) => !check.ok);
  const total = checks.length;
  if (!total) {
    return { total: 0, failed: 0, allOk: false, text: '没有判据' };
  }
  if (!failedChecks.length) {
    return { total, failed: 0, allOk: true, text: `${total} 条判据全部对得上` };
  }
  return {
    total,
    failed: failedChecks.length,
    allOk: false,
    text: `${failedChecks.length} 条对不上：${failedChecks.map((check) => check.label).join('、')}`,
  };
}

/**
 * 「这个批的料从哪来、货去哪了」的总述（详情页标题旁边的一句）。
 *
 * 例：`领料 3 张 120 件 / 1 200.00 元 · 完工入库 1 张 80 件 / 2 000.00 元`
 * 用后端给的合计（已过账口径），不在这里重算。
 */
export function sourcesHeadline(sources: CostObjectSources): string {
  const issue = `领料 ${sources.issue_posted_totals.doc_count} 张 ${formatQty(
    sources.issue_posted_totals.qty,
  )} / ${formatMoney(sources.issue_posted_totals.amount)}`;
  const receipt = `完工入库 ${sources.receipt_totals.doc_count} 张 ${formatQty(
    sources.receipt_totals.qty,
  )} / ${formatMoney(sources.receipt_totals.amount)}`;
  return `${issue} · ${receipt}`;
}
