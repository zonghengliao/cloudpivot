/**
 * 收货入库页纯逻辑（P2-E1 + 后端 P2-B1）—— 状态按钮显隐 / 过账确认口径 / 表单校验 / 请求体。
 *
 * # 为什么单独一个 `.ts`
 *
 * 与 `purchase-logic.ts` 同理：用例跑在没有 DOM 的 node 里，`.vue` 的 `setup()` 起不来。
 * 而这里有两件**错了会出事**的事：
 *
 * 1. **过账不可撤销**（只能作废红冲），按钮该不该出现、弹窗默认勾哪个选项，必须可断言；
 * 2. 「发票未到 → 暂估入账」本批**不开放**，必须置灰且不可提交（PRD §3.3.2）。
 *
 * # 金额口径：收货入库只有一种
 *
 * 后端 `stockin_dto::StockInItemReq` 只收 `price_excl`（不含税单价），入库成本也只用不含税金额
 * （`total_amount` / `item.amount`）。所以收货单**没有口径徽标** —— 不是漏了，
 * 而是成本基础只能是不含税，给了「含税录入」反而会算错成本。
 * 价税合计由 `amount + tax_amount` 得出，与库里 `ck_biz_si_item_tax` 的精确恒等一致。
 *
 * # 状态机（`stockin_model`）
 *
 * ```text
 * DRAFT ──过账──→ POSTED ──作废（必须填原因，且不冲回库存）──→ CANCELLED
 *   └────作废（必须填原因）────→ CANCELLED
 * ```
 */
import type {
  CreateStockInReq,
  StockInItemReq,
  StockInItemLine,
  StockInRow,
  StockInStatus,
  StockInStatusReq,
} from '@/api/business/erp';

/** 每页条数 */
export const STOCKIN_PAGE_SIZE = 20;

/** 备注上限（与采购单同口径） */
export const STOCKIN_MAX_REMARK = 500;

/**
 * 金额列标题：PRD 硬要求带「(价税合计)」。
 *
 * 与 `purchase-logic.ts::AMOUNT_COLUMN_TITLE` 同字面 —— 两张单都对着同一份 PRD，
 * 措辞不能各写各的（用户会在两个页面之间对照数字）。
 */
export const AMOUNT_COLUMN_TITLE = '金额(价税合计)';

/** 明细行上限（后端 `MAX_LINES`） */
export const STOCKIN_MAX_LINES = 200;

/** 税率候选（0 = 免税；13 / 9 / 6 / 3 为现行常用档） */
export const TAX_RATE_OPTIONS: ReadonlyArray<{ label: string; value: number }> = [
  { label: '13%', value: 13 },
  { label: '9%', value: 9 },
  { label: '6%', value: 6 },
  { label: '3%', value: 3 },
  { label: '0%（免税）', value: 0 },
];

/** 一行三金额 */
export interface StockInLineAmounts {
  /** 不含税金额 */
  amount: number;
  /** 税额 */
  tax: number;
  /** 价税合计 */
  total: number;
}

/** 表单一行的可编辑状态 */
export interface StockInItemForm {
  /** 行内稳定 key */
  key: string;
  /** 物料 id */
  material_id: string;
  /** 数量（> 0） */
  qty: number;
  /** 不含税单价 */
  price_excl: number;
  /** 税率 0–100 */
  tax_rate: number;
}

/** 表单状态 */
export interface StockInFormState {
  /** 单据 id（有值 = 编辑） */
  id: string;
  /** 供应商 id */
  supplier_id: string;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 入库日期 `YYYY-MM-DD` */
  in_date: string;
  /** ★ 发票已随货到达（默认 `true`） */
  invoice_received: boolean;
  /** 备注 */
  remark: string;
  /** 明细 */
  items: StockInItemForm[];
}

/** 表单校验结果 */
export type StockInFormErrors = Record<string, string>;

/** 筛选行 */
export interface StockInFilterState {
  /** 状态（`''` = 全部） */
  status: StockInStatus | '';
  /** 供应商 id */
  supplier_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 单号 / 备注关键词 */
  keyword: string;
}

/** 行级按钮显隐 */
export interface StockInRowActions {
  /** 编辑（仅 `DRAFT`） */
  edit: boolean;
  /** 过账（仅 `DRAFT`） */
  post: boolean;
  /** 作废（`DRAFT` / `POSTED`） */
  cancel: boolean;
  /** 删除（仅 `DRAFT`） */
  remove: boolean;
  /** 查看（永远可点） */
  view: boolean;
}

// ==================== 数值 ====================

/** NUMERIC 可能是字符串，统一过这里（`"226.0000"` 直接用 `+` 会变成字符串拼接） */
export function toNumber(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/** 舍入（half away from zero，与 Rust `Decimal::round_dp` 同向） */
export function roundTo(value: number, dp = 2): number {
  const factor = 10 ** dp;
  const scaled = value * factor;
  if (!Number.isFinite(scaled)) {
    return 0;
  }
  const nudge = 1e-9 * (Math.abs(scaled) + 1);
  const adjusted = scaled >= 0 ? scaled + nudge : scaled - nudge;
  return Math.round(adjusted) / factor;
}

/** 金额 2 位 */
export function round2(value: number): number {
  return roundTo(value, 2);
}

/** 单价 6 位（列 `NUMERIC(18,6)`） */
export function round6(value: number): number {
  return roundTo(value, 6);
}

/**
 * 单行三金额（收货只走「不含税录入」这一种，与后端 `stockin_dto` 一致）。
 *
 * ```text
 * amount = round2(qty × price_excl)
 * tax    = round2(amount × rate / 100)
 * total  = amount + tax
 * ```
 */
export function lineAmounts(qty: number, priceExcl: number, taxRate: number): StockInLineAmounts {
  const safeQty = Number.isFinite(qty) ? qty : 0;
  const safePrice = Number.isFinite(priceExcl) ? priceExcl : 0;
  const safeRate = Number.isFinite(taxRate) ? taxRate : 0;
  const amount = round2(safeQty * safePrice);
  const tax = round2((amount * safeRate) / 100);
  return { amount, tax, total: round2(amount + tax) };
}

/** 表单一行 → 三金额 */
export function lineAmountsOfItem(item: StockInItemForm): StockInLineAmounts {
  return lineAmounts(toNumber(item.qty), toNumber(item.price_excl), toNumber(item.tax_rate));
}

/** 整单合计（逐行舍入后相加） */
export function computeTotals(
  items: ReadonlyArray<StockInItemForm>,
): StockInLineAmounts & { qty: number } {
  let qty = 0;
  let amount = 0;
  let tax = 0;
  let total = 0;
  for (const item of items) {
    const line = lineAmountsOfItem(item);
    qty = round6(qty + toNumber(item.qty));
    amount = round2(amount + line.amount);
    tax = round2(tax + line.tax);
    total = round2(total + line.total);
  }
  return { qty, amount, tax, total };
}

// ==================== 状态 / 按钮显隐 ====================

/** 状态 → 中文（与 `stockin_model::status_label` 同文案） */
export function stockInStatusLabel(status: StockInStatus | string): string {
  switch (status) {
    case 'DRAFT':
      return '草稿';
    case 'POSTED':
      return '已过账';
    case 'CANCELLED':
      return '已作废';
    default:
      return String(status);
  }
}

/** 状态 → TDesign tag 主题 */
export function stockInStatusTheme(
  status: StockInStatus | string,
): 'default' | 'primary' | 'success' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'POSTED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 状态下拉候选 */
export const STOCKIN_STATUS_FILTER_OPTIONS: ReadonlyArray<{
  label: string;
  value: '' | StockInStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿', value: 'DRAFT' },
  { label: '已过账', value: 'POSTED' },
  { label: '已作废', value: 'CANCELLED' },
];

/**
 * 行级按钮显隐（卡面 §四 + 后端 `LEGAL_EDGES`）。
 *
 * | 状态 | 编辑 | 过账 | 作废 | 删除 | 查看 |
 * |---|---|---|---|---|---|
 * | `DRAFT` | ✅ | ✅ | ✅ | ✅ | ✅ |
 * | `POSTED` | — | — | ✅ | — | ✅ |
 * | `CANCELLED` | — | — | — | — | ✅ |
 *
 * ⚠️ `POSTED` 的「作废」**不冲回库存**（P2-B1 只改状态，红冲在 P2-C1），
 * 提示文案见 {@link cancelConfirmMessage}。
 */
export function availableStockInActions(status: StockInStatus | string): StockInRowActions {
  const isDraft = status === 'DRAFT';
  const isPosted = status === 'POSTED';
  return {
    edit: isDraft,
    post: isDraft,
    cancel: isDraft || isPosted,
    remove: isDraft,
    view: true,
  };
}

// ==================== 过账确认弹窗（PRD §3.3.2） ====================

/** 「发票未到」选项的禁用提示（本批不开放暂估入账） */
export const ESTIMATE_DISABLED_TIP = '暂估入库将在第 2 批（含发票管理）开放';

/**
 * 发票选项（弹窗里两个必选项）。
 *
 * - `RECEIVED`：**默认勾选**，过账按可抵扣进项税入账；
 * - `ESTIMATE`：**置灰不可选**（`disabled: true`），tooltip 说明开放批次。
 *
 * 后端 `invoice_received` 默认 `true`、传 `false` 也照收（本批不拦），
 * 所以置灰是**纯前端行为**（卡面硬红线 3），与后端不冲突。
 */
export const INVOICE_OPTIONS: ReadonlyArray<{
  value: 'RECEIVED' | 'ESTIMATE';
  label: string;
  desc: string;
  disabled: boolean;
  tip?: string;
}> = [
  {
    value: 'RECEIVED',
    label: '发票已随货到达',
    desc: '本次入库按可抵扣进项税入账',
    disabled: false,
  },
  {
    value: 'ESTIMATE',
    label: '发票未到',
    desc: '按暂估入账，不计进项税',
    disabled: true,
    tip: ESTIMATE_DISABLED_TIP,
  },
];

/** 过账弹窗默认选中的发票选项（永远是「已随货到达」） */
export function defaultInvoiceChoice(): 'RECEIVED' | 'ESTIMATE' {
  return 'RECEIVED';
}

/** 发票选项 → 建单 / 过账用的布尔值 */
export function invoiceReceivedOf(choice: 'RECEIVED' | 'ESTIMATE'): boolean {
  return choice === 'RECEIVED';
}

/** 过账弹窗「过账后将……」的三条后果（PRD §3.3.2 原文） */
export const POST_CONSEQUENCES: ReadonlyArray<string> = [
  '增加库存（移动加权成本）',
  '生成应付单和记账凭证',
  '不可撤销，只能作废红冲',
];

/** 过账确认文案（弹窗标题下的副标题） */
export function postConfirmMessage(inNo: string, totalWithTax: number | string): string {
  return `收货单 ${inNo} 过账后不可撤销：价税合计 ${formatMoney(totalWithTax)}，将增加库存并生成应付单与记账凭证。`;
}

/** 作废确认文案（`POSTED` 时提醒不冲回库存） */
export function cancelConfirmMessage(status: StockInStatus | string): string {
  return status === 'POSTED'
    ? '作废后本批库存不会自动冲回（红冲在下一批开放），只是把单据标记为已作废。确认作废？'
    : '作废后草稿将不可再编辑，确认作废？';
}

// ==================== 格式化 ====================

/** 金额格式化（千分位 + 2 位） */
export function formatMoney(value: number | string | null | undefined): string {
  const num = toNumber(value);
  const negative = num < 0;
  const fixed = Math.abs(round2(num)).toFixed(2);
  const [intPart, decPart] = fixed.split('.');
  const grouped = (intPart ?? '0').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${negative ? '-' : ''}${grouped}.${decPart ?? '00'}`;
}

/** 数量格式化 */
export function formatQty(value: number | string | null | undefined): string {
  const num = toNumber(value);
  return Number.isInteger(num) ? String(num) : String(round6(num));
}

/** 日期 → `MM-DD` */
export function shortDate(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(value);
  return match ? `${match[2]}-${match[3]}` : value;
}

/** 空值占位 */
export function dash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? '—' : value;
}

/** 短 id */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/** 北京时间今天（与后端 `today_cst()` 同口径） */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 10);
}

// ==================== 表单 ====================

/** 空筛选 */
export function emptyStockInFilter(): StockInFilterState {
  return { status: '', supplier_id: '', warehouse_id: '', keyword: '' };
}

/** 单行空明细（税率默认 13%，与采购单同口径） */
export function emptyStockInItem(key: string): StockInItemForm {
  return { key, material_id: '', qty: 1, price_excl: 0, tax_rate: 13 };
}

/** 空表单（`invoice_received` 默认 `true`） */
export function emptyStockInForm(): StockInFormState {
  return {
    id: '',
    supplier_id: '',
    warehouse_id: '',
    in_date: todayString(),
    invoice_received: true,
    remark: '',
    items: [emptyStockInItem('row-1')],
  };
}

/**
 * 详情 → 表单（编辑回填）。
 *
 * 明细接口给的是 `price`（不含税单价）与 `price_incl`，这里用 `price` —— 收货口径只有一个。
 */
export function detailToForm(
  header: StockInRow,
  items: ReadonlyArray<StockInItemLine>,
): StockInFormState {
  return {
    id: header.id,
    supplier_id: header.supplier_id ?? '',
    warehouse_id: header.warehouse_id ?? '',
    in_date: header.in_date ?? todayString(),
    invoice_received: header.invoice_received !== false,
    remark: header.remark ?? '',
    items:
      items.length === 0
        ? [emptyStockInItem('row-1')]
        : items.map((line, index) => ({
            key: `row-${index + 1}`,
            material_id: line.material_id,
            qty: toNumber(line.qty),
            price_excl: round6(toNumber(line.price)),
            tax_rate: toNumber(line.tax_rate),
          })),
  };
}

/** 表单校验（与后端 `stockin_dto` 同规则：qty > 0、price_excl ≥ 0、税率 0–100） */
export function validateStockInForm(form: StockInFormState): StockInFormErrors {
  const errors: StockInFormErrors = {};

  if (!form.supplier_id) {
    errors.supplier_id = '请选择供应商';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选择收货仓库';
  }
  if (!form.in_date) {
    errors.in_date = '请选择入库日期';
  }
  if (form.remark.length > STOCKIN_MAX_REMARK) {
    errors.remark = `备注最多 ${STOCKIN_MAX_REMARK} 字（当前 ${form.remark.length} 字）`;
  }
  if (form.items.length === 0) {
    errors.items = '至少要有一行明细';
  }
  if (form.items.length > STOCKIN_MAX_LINES) {
    errors.items = `明细最多 ${STOCKIN_MAX_LINES} 行（当前 ${form.items.length} 行）`;
  }

  for (const [index, item] of form.items.entries()) {
    const prefix = `items.${index}`;
    if (!item.material_id) {
      errors[`${prefix}.material_id`] = '请选择物料';
    }
    const qty = toNumber(item.qty);
    if (!(qty > 0)) {
      // 后端：ck_biz_si_item_qty 要求 qty > 0，0 数量直接 400
      errors[`${prefix}.qty`] = `第 ${index + 1} 行数量必须大于 0`;
    } else if (roundTo(qty, 4) !== qty) {
      errors[`${prefix}.qty`] = `第 ${index + 1} 行数量最多 4 位小数`;
    }
    if (toNumber(item.price_excl) < 0) {
      errors[`${prefix}.price_excl`] = `第 ${index + 1} 行单价不能为负`;
    } else if (roundTo(toNumber(item.price_excl), 6) !== toNumber(item.price_excl)) {
      // 单价列 NUMERIC(18,6)
      errors[`${prefix}.price_excl`] = `第 ${index + 1} 行单价最多 6 位小数`;
    }
    const rate = toNumber(item.tax_rate);
    if (rate < 0 || rate > 100) {
      errors[`${prefix}.tax_rate`] = `第 ${index + 1} 行税率必须在 0–100 之间`;
    }
  }

  return errors;
}

/** 校验是否通过 */
export function isStockInFormValid(form: StockInFormState): boolean {
  return Object.keys(validateStockInForm(form)).length === 0;
}

/**
 * 表单 → 建单 / 改单请求体（**不传金额**，服务端算）。
 *
 * 无源直收（手工建单）：`items` 每行 `material_id` + `qty` + `price_excl` + `tax_rate`。
 * 序列号本批不做（一物一码物料在过账时由系统自动取号；卡面未要求手工录号）。
 */
export function buildStockInPayload(form: StockInFormState): CreateStockInReq {
  const items: StockInItemReq[] = form.items.map((item) => ({
    material_id: item.material_id,
    qty: toNumber(item.qty),
    price_excl: round6(toNumber(item.price_excl)),
    tax_rate: toNumber(item.tax_rate),
  }));

  return {
    supplier_id: form.supplier_id,
    warehouse_id: form.warehouse_id,
    in_date: form.in_date,
    invoice_received: form.invoice_received,
    remark: form.remark.trim() || null,
    items,
  };
}

/**
 * 过账请求体。
 *
 * `serials` 只在手工指定序列号时传；本页不传（系统在过账事务里自动取号，
 * 见 P2-B1 的 `stockin_handler::post_in_tx` 第 ④ 步）。
 */
export function buildPostPayload(): StockInStatusReq {
  return { status: 'POSTED' };
}

/** 作废请求体（`POSTED → CANCELLED` 必须带 `reason`，否则 400） */
export function buildCancelPayload(reason: string): StockInStatusReq {
  return { status: 'CANCELLED', reason: reason.trim() };
}

/** 筛选 → 查询参数（空串不发） */
export function buildStockInQuery(
  filter: StockInFilterState,
  page: number,
  pageSize: number = STOCKIN_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.supplier_id) {
    query.supplier_id = filter.supplier_id;
  }
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/** 是否处于筛选态 */
export function isStockInFiltered(filter: StockInFilterState): boolean {
  return Boolean(filter.status || filter.supplier_id || filter.warehouse_id || filter.keyword.trim());
}

/** 过账按钮是否可用（无明细的草稿不该过账 —— 后端也会拦） */
export function canPost(row: StockInRow): boolean {
  return row.status === 'DRAFT' && toNumber(row.total_qty) > 0;
}
