/**
 * 出库单页纯逻辑（DC-13 后端 / 前端页面）—— 状态按钮 / 表单校验 / 请求体 / 展示口径。
 *
 * # 为什么单独一个 `.ts`
 *
 * 与 `stockin-logic.ts` / `purchase-logic.ts` 同理：用例跑在没有 DOM 的 node 里，
 * `.vue` 的 `setup()` 起不来。而这里有四件**错了会出事**的事：
 *
 * 1. **按钮只能由后端的 `next_statuses` 驱动** —— 前端写第二份状态机，等后端收紧边
 *    （比如以后不允许 `DRAFT → POSTED` 直过）时页面会把用户送进 409；
 * 2. **作废必须填原因**（`ck_biz_so_cancel` 是 NOT NULL 口径），空理由提交就是一次白跑；
 * 3. **报废 / 盘亏必须填事由** —— 后端要拿它挂 1901 待处理财产损溢，没理由直接 400；
 * 4. **明细绝不能带单价 / 金额 / 单位** —— 出库成本取账上移动加权单价的真实差额，
 *    带上那些字段等于给客户端一个「自己定价」的入口（B2 BLOCKER 的经典成因）。
 */

import type {
  CreateStockOutReq,
  StockOutDetail,
  StockOutItemReq,
  StockOutRow,
  StockOutStatus,
  StockOutStatusReq,
  StockOutType,
} from '@/api/business/erp/stock-out';

/** 每页条数（与收货单列表同口径） */
export const STOCK_OUT_PAGE_SIZE = 20;

/** 备注上限（后端 `MAX_REMARK_LEN`） */
export const STOCK_OUT_MAX_REMARK = 500;

/** 事由 / 作废原因上限（后端 `MAX_REASON_LEN`，列 `VARCHAR(255)`） */
export const STOCK_OUT_MAX_REASON = 255;

/** 明细行上限（后端 `MAX_LINES`） */
export const STOCK_OUT_MAX_LINES = 200;

/**
 * 出库类型下拉（中文名与后端 `out_type_label` 逐字一致）。
 *
 * 顺序按业务常用度：领用 > 报废 > 盘亏 > 其他（领用占 90% 的量）。
 */
export const OUT_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: StockOutType }> = [
  { label: '领用', value: 'ISSUE' },
  { label: '报废', value: 'SCRAP' },
  { label: '盘亏', value: 'LOSS' },
  { label: '其他', value: 'OTHER' },
];

/**
 * 必须填事由的类型（后端 `STOCK_OUT_REASON_TYPES`）。
 *
 * 这两个类型过账时借 **1901 待处理财产损溢**，凭证摘要要写清「为什么报废 / 盘亏」，
 * 没有理由的 1901 过不了审计 —— 所以后端 400 兜底，前端提前拦一道。
 */
export const REQUIRES_REASON_TYPES: ReadonlyArray<StockOutType> = ['SCRAP', 'LOSS'];

/** 状态筛选下拉（`''` = 全部；TDesign 把空串当未选，所以首项是「全部」但 value 为空由 clearable 承担） */
export const STOCK_OUT_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: StockOutStatus }> = [
  { label: '草稿', value: 'DRAFT' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 出库类型 → 中文名（后端没给 label 时的兜底译名） */
export function outTypeLabel(value: StockOutType | string | null | undefined): string {
  const hit = OUT_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 状态 → 中文名（后端没给 label 时的兜底译名） */
export function stockOutStatusLabel(status: StockOutStatus | string | null | undefined): string {
  switch (status) {
    case 'DRAFT':
      return '草稿';
    case 'PENDING_POST':
      return '待过账';
    case 'POSTED':
      return '已过账';
    case 'CANCELLED':
      return '已作废';
    default:
      return String(status ?? '—');
  }
}

/**
 * 状态 → TDesign 标签主题。
 *
 * `PENDING_POST` 用 `warning`：它不是错误，但「等人过账」在列表里必须显眼
 * （这是 DC-13 的不做实降级交接位，卡住就没人减库存）。
 */
export function stockOutStatusTheme(
  status: StockOutStatus | string | null | undefined,
): 'default' | 'primary' | 'warning' | 'success' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'PENDING_POST':
      return 'warning';
    case 'POSTED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 该出库类型是否必须填事由 */
export function requiresReason(outType: StockOutType | string | null | undefined): boolean {
  return REQUIRES_REASON_TYPES.includes(outType as StockOutType);
}

/** 列表筛选状态 */
export interface StockOutFilterState {
  /** 单号 / 事由 / 备注（模糊） */
  keyword: string;
  /** 状态（空 = 全部） */
  status: string;
  /** 出库类型（空 = 全部） */
  out_type: string;
  /** 仓库（空 = 全部） */
  warehouse_id: string;
  /** 出库日期区间 `[起, 止]`（TDesign t-date-range-picker 的 v-model 形状） */
  date_range: string[];
}

/** 空筛选条件 */
export function emptyStockOutFilter(): StockOutFilterState {
  return { keyword: '', status: '', out_type: '', warehouse_id: '', date_range: [] };
}

/** 表单里的一行明细（key 只用于 v-for，不提交） */
export interface StockOutItemForm {
  /** 行内稳定 key */
  key: string;
  /** 物料 id */
  material_id: string;
  /** 数量（`undefined` = 还没填；TDesign `t-input-number` 的未选态就是 undefined） */
  qty?: number;
  /** 库位账行 id（可空；`undefined` = 未指定库位桶） */
  location_id?: string;
}

/** 建单表单 */
export interface StockOutFormState {
  /** 编辑态的单据 id（`null` = 新建） */
  id: string | null;
  /** 出库类型 */
  out_type: StockOutType;
  /** 出库仓库 */
  warehouse_id: string;
  /** 出库日期 `YYYY-MM-DD` */
  out_date: string;
  /** 事由（报废 / 盘亏必填） */
  reason: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  items: StockOutItemForm[];
}

/** 校验错误（key = 字段名 / `items.<index>.<field>`） */
export type StockOutFormErrors = Record<string, string>;

/** 今天（CST，`YYYY-MM-DD`）—— 出库日期默认值 */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 3600 * 1000);
  return shifted.toISOString().slice(0, 10);
}

/** 一行空明细 */
export function emptyStockOutItem(key: string): StockOutItemForm {
  return { key, material_id: '', qty: undefined, location_id: undefined };
}

/** 空表单（默认领用 + 今天 + 一行空明细） */
export function emptyStockOutForm(now: Date = new Date()): StockOutFormState {
  return {
    id: null,
    out_type: 'ISSUE',
    warehouse_id: '',
    out_date: todayString(now),
    reason: '',
    remark: '',
    items: [emptyStockOutItem('line-1')],
  };
}

/** 详情 → 编辑表单（仅草稿可编辑） */
export function detailToForm(detail: StockOutDetail): StockOutFormState {
  return {
    id: detail.id,
    out_type: (detail.out_type as StockOutType) || 'ISSUE',
    warehouse_id: detail.warehouse_id,
    out_date: detail.out_date,
    reason: detail.reason ?? '',
    remark: detail.remark ?? '',
    items:
      detail.items.length > 0
        ? detail.items.map((item, index) => ({
            key: `line-${index + 1}`,
            material_id: item.material_id,
            qty: Number(item.qty),
            location_id: item.location_id ?? undefined,
          }))
        : [emptyStockOutItem('line-1')],
  };
}

/**
 * 表单校验（纯逻辑，不查库）。
 *
 * `serialMaterialIds` 传入「一物一码物料」的 id 集合时，会额外拦下这类物料 ——
 * 后端对它们直接 400（账面数量必须等于在库序列号件数，那条件数只能由序列号模块维护），
 * 与其让用户填完一行再吃 400，不如选物料时就告诉他不让选（本函数是第二道闸）。
 */
export function validateStockOutForm(
  form: StockOutFormState,
  serialMaterialIds: ReadonlyArray<string> = [],
): StockOutFormErrors {
  const errors: StockOutFormErrors = {};

  if (!OUT_TYPE_OPTIONS.some((item) => item.value === form.out_type)) {
    errors.out_type = '请选择出库类型（领用 / 报废 / 盘亏 / 其他）';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选择出库仓库';
  }
  if (!form.out_date) {
    errors.out_date = '请选择出库日期';
  }
  if (requiresReason(form.out_type) && !form.reason.trim()) {
    errors.reason = `「${outTypeLabel(form.out_type)}」必须填事由：过账时要挂 1901 待处理财产损溢，凭证摘要靠它`;
  }
  if (form.reason.length > STOCK_OUT_MAX_REASON) {
    errors.reason = `事由最长 ${STOCK_OUT_MAX_REASON} 字（当前 ${form.reason.length} 字）`;
  }
  if (form.remark.length > STOCK_OUT_MAX_REMARK) {
    errors.remark = `备注最长 ${STOCK_OUT_MAX_REMARK} 字（当前 ${form.remark.length} 字）`;
  }

  if (form.items.length === 0) {
    errors.items = '至少要有一行明细';
    return errors;
  }
  if (form.items.length > STOCK_OUT_MAX_LINES) {
    errors.items = `明细最多 ${STOCK_OUT_MAX_LINES} 行（当前 ${form.items.length} 行）`;
    return errors;
  }

  const seen = new Set<string>();
  form.items.forEach((item, index) => {
    const prefix = `items.${index}`;
    if (!item.material_id) {
      errors[`${prefix}.material_id`] = '请选择物料';
    } else if (serialMaterialIds.includes(item.material_id)) {
      errors[`${prefix}.material_id`] = '一物一码物料不能走出库单：请到序列号管理里做出库 / 报废';
    } else if (seen.has(item.material_id)) {
      // 同物料多行不是错的（不同库位桶各自出库是正常业务），只提示不拦 —— 这里放行。
    }
    if (item.material_id) {
      seen.add(item.material_id);
    }
    const qty = Number(item.qty ?? 0);
    if (!Number.isFinite(qty) || qty <= 0) {
      errors[`${prefix}.qty`] = '数量必须 > 0（0 数量出库没有业务含义）';
    }
  });

  return errors;
}

/** 表单是否可提交 */
export function isStockOutFormValid(form: StockOutFormState, serialMaterialIds: string[] = []): boolean {
  return Object.keys(validateStockOutForm(form, serialMaterialIds)).length === 0;
}

/** 数字 → 定长字符串（去掉浮点尾巴；后端收 `Decimal`，字符串最稳） */
export function qtyText(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0';
  }
  return String(Number(num.toFixed(4)));
}

/**
 * 表单 → 建单请求体。
 *
 * **只挑后端认的字段**：明细里没有单价 / 金额 / 单位（出库成本由服务端按账上
 * 移动加权单价的真实差额决定）。空串一律转 `null`，不留 `""` 让后端猜。
 */
export function buildStockOutPayload(form: StockOutFormState): CreateStockOutReq {
  const items: StockOutItemReq[] = form.items.map((item) => ({
    material_id: item.material_id,
    qty: qtyText(item.qty),
    location_id: item.location_id || null,
  }));
  return {
    out_type: form.out_type,
    warehouse_id: form.warehouse_id,
    out_date: form.out_date,
    reason: form.reason.trim() ? form.reason.trim() : null,
    remark: form.remark.trim() ? form.remark.trim() : null,
    items,
  };
}

/** 筛选条件 → 查询参数（空串一律不发；日期区间拆成 `date_from` / `date_to`） */
export function buildStockOutQuery(
  filter: StockOutFilterState,
  page: number,
  pageSize: number = STOCK_OUT_PAGE_SIZE,
): Record<string, string | number> {
  const query: Record<string, string | number> = { page, page_size: pageSize };
  if (filter.keyword.trim()) {
    query.keyword = filter.keyword.trim();
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.out_type) {
    query.out_type = filter.out_type;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  const [from, to] = filter.date_range ?? [];
  if (from) {
    query.date_from = from;
  }
  if (to) {
    query.date_to = to;
  }
  return query;
}

/** 筛选是否生效（空态文案区分「还没有单」和「没有匹配」） */
export function isStockOutFiltered(filter: StockOutFilterState): boolean {
  return Boolean(
    filter.keyword.trim() || filter.status || filter.out_type || filter.warehouse_id || filter.date_range?.length,
  );
}

/** 详情抽屉里由 `next_statuses` 驱动的一个按钮 */
export interface StockOutActionSpec {
  /** 目标状态 */
  status: StockOutStatus;
  /** 按钮文案 */
  label: string;
  /** 按钮主题 */
  theme: 'primary' | 'danger' | 'default';
  /** 点下去要不要先弹「填原因」的弹窗 */
  needReason: boolean;
  /** 稳定的 data-testid（真机验收按它点） */
  testid: string;
}

/**
 * 动作按钮的文案与语义（以 `(当前状态, 目标状态)` 为准，不看状态机的绝对表）。
 *
 * 之所以要「当前状态」参与：目标 `DRAFT` 在 `PENDING_POST` 下是**撤回**，
 * 换个来源状态含义就不同 —— 只看目标会写出误导用户的文案。
 */
function actionOf(current: string, target: StockOutStatus): StockOutActionSpec | null {
  switch (target) {
    case 'POSTED':
      return { status: target, label: '过账', theme: 'primary', needReason: false, testid: 'btn-stockout-post' };
    case 'PENDING_POST':
      return {
        status: target,
        label: '提交待过账',
        theme: 'primary',
        needReason: false,
        testid: 'btn-stockout-submit',
      };
    case 'DRAFT':
      return {
        status: target,
        label: current === 'PENDING_POST' ? '撤回（回草稿）' : '退回草稿',
        theme: 'default',
        needReason: false,
        testid: 'btn-stockout-withdraw',
      };
    case 'CANCELLED':
      return { status: target, label: '作废', theme: 'danger', needReason: true, testid: 'btn-stockout-cancel' };
    default:
      return null;
  }
}

/** 后端下发顺序里认识的 4 个状态，未知值一律丢弃（不让页面渲出一个点不了 / 看不懂的按钮） */
const KNOWN_TARGETS: ReadonlyArray<StockOutStatus> = ['POSTED', 'PENDING_POST', 'DRAFT', 'CANCELLED'];

/**
 * **详情抽屉的按钮列表 = 后端 `next_statuses` 的投影**。
 *
 * 前端不写第二份状态机：`next_statuses` 里没有的目标，这里一个按钮都不会出；
 * 后端改了边，页面立刻跟着变（单测按「只喂两个目标 → 只有两个按钮」断言）。
 */
export function statusActions(current: string, nextStatuses: ReadonlyArray<string>): StockOutActionSpec[] {
  const actions: StockOutActionSpec[] = [];
  for (const raw of nextStatuses ?? []) {
    if (!KNOWN_TARGETS.includes(raw as StockOutStatus)) {
      continue;
    }
    const target = raw as StockOutStatus;
    if (actions.some((item) => item.status === target)) {
      continue;
    }
    const spec = actionOf(current, target);
    if (spec) {
      actions.push(spec);
    }
  }
  return actions;
}

/** 只有草稿能改（后端 `ensure_draft` 同口径：其它状态 409） */
export function canEdit(row: Pick<StockOutRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 只有草稿能删（后端软删同样只认 DRAFT） */
export function canDelete(row: Pick<StockOutRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 作废原因校验（弹窗里 `t-textarea` 的必填；空理由连请求都不发） */
export function validateCancelReason(reason: string): string {
  const text = reason.trim();
  if (!text) {
    return '作废必须填原因：它会进状态日志与红字凭证摘要，事后要凭它追溯';
  }
  if (text.length > STOCK_OUT_MAX_REASON) {
    return `作废原因最长 ${STOCK_OUT_MAX_REASON} 字（当前 ${text.length} 字）`;
  }
  return '';
}

/** 作废请求体 */
export function buildCancelPayload(reason: string): StockOutStatusReq {
  return { status: 'CANCELLED', reason: reason.trim() };
}

/** 状态历史的一条 */
export interface StockOutHistoryEntry {
  /** 稳定 key */
  key: string;
  /** 里程碑名 */
  label: string;
  /** 时间（RFC3339；`null` = 后端没下发该时间） */
  at: string | null;
  /** 补充说明（作废原因 / 待过账提示） */
  note: string;
}

/**
 * 状态历史（由详情里能拿到的时间戳拼出来）。
 *
 * 后端没有单独的「状态流转日志」接口，但详情下发了 `created_at` / `posted_at` /
 * `cancelled_at` / `cancel_reason`，足够画出四个里程碑。**只为真实发生过的事打点**：
 * 草稿单据的「已过账」那一行不会出现（不猜、不补假时间）。
 */
export function statusHistory(
  detail: Pick<StockOutDetail, 'created_at' | 'status' | 'posted_at' | 'cancelled_at' | 'cancel_reason'>,
): StockOutHistoryEntry[] {
  const entries: StockOutHistoryEntry[] = [
    { key: 'created', label: '创建（草稿）', at: detail.created_at, note: '' },
  ];
  if (detail.status === 'PENDING_POST') {
    entries.push({
      key: 'pending',
      label: '提交待过账',
      at: null,
      note: '等待他人过账（制单与过账分权：同一人不该既提单又过账）',
    });
  }
  if (detail.posted_at) {
    entries.push({ key: 'posted', label: '过账（三本账已负向 + 凭证已生成）', at: detail.posted_at, note: '' });
  }
  if (detail.cancelled_at) {
    entries.push({
      key: 'cancelled',
      label: '作废（三本账已按快照回滚 + 红冲凭证）',
      at: detail.cancelled_at,
      note: detail.cancel_reason ?? '',
    });
  }
  return entries;
}

/** 金额展示（两位小数 + 千分位） */
export function formatMoney(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0.00';
  }
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 数量展示（最多 4 位小数，去掉无意义的 0） */
export function formatQty(value: number | string | null | undefined): string {
  const num = Number(value ?? 0);
  if (!Number.isFinite(num)) {
    return '0';
  }
  return String(Number(num.toFixed(4)));
}

/** 日期只留 `MM-DD`（列表列宽有限；详情里给完整日期） */
export function shortDate(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length >= 10 ? value.slice(5, 10) : value;
}

/** 空值占位 */
export function dash(value: string | null | undefined): string {
  return value && String(value).trim() ? String(value) : '—';
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（浏览器本地时区，也就是用户看到的北京时间） */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  const pad = (num: number) => String(num).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 短 id（物料 / 仓库这种还没译名的兜底展示） */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/** 接口错误的最小形状（不 import axios：单测里能直接喂对象） */
export interface ErrorLike {
  /** 业务码（`66034` 负库存 / `66035` 作废缺流水 / `10001` 参数 …） */
  code?: number;
  /** 报文（**原样展示**：负库存的「可用量」就在这句里） */
  message?: string;
}

/**
 * 错误 → 页面文案。
 *
 * 关键口径：**原样带上后端 `msg`**。负库存拦截的报文长这样：
 * 「负库存拦截：物料 M-001（螺栓）在仓库 WH-01 / 库位 A-01 可用量 3，本次出库 5」——
 * 只写「操作失败」等于把用户唯一的自救信息（可用量）丢掉。
 */
export function errorText(error: unknown): string {
  if (error && typeof error === 'object') {
    const like = error as ErrorLike;
    const message = (like.message ?? '').trim();
    if (message) {
      return typeof like.code === 'number' && like.code > 0 ? `${message}（错误码 ${like.code}）` : message;
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error;
  }
  return '操作失败：接口没有返回说明，请刷新后重试';
}

/** 报错是否属于「负库存拦截」（页面要额外强调「可用量不足」） */
export function isNegativeStockError(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && (error as ErrorLike).code === 66034);
}

/** 报错是否属于「作废时过账流水缺行」 */
export function isCancelBlockedError(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && (error as ErrorLike).code === 66035);
}
