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
  /**
   * 来源采购明细行 id（D4）。
   *
   * 带了它，后端才能把本次收货量回写到**那一行**的 `received_qty`（按行匹配，不按物料），
   * 也才能在过账时判「超收」。从采购单带出时自动填；手工建单时为空。
   */
  purchase_order_item_id?: string;
  /**
   * 来源采购行的待收快照（D4 提示用：`订单 100 / 已收 30 / 待收 70`）。
   *
   * 只用于**展示与前端预校验**，不是权威口径 —— 权威口径在后端（保存时按库里的
   * POSTED 聚合再算一遍，超了 400）。
   */
  pending?: StockInItemPending;
  /** 物料 id */
  material_id: string;
  /** 数量（> 0） */
  qty: number;
  /** 不含税单价 */
  price_excl: number;
  /** 税率 0–100 */
  tax_rate: number;
}

/** 一行「待收」快照（来自 `GET /purchase-order/{id}/pending-items`） */
export interface StockInItemPending {
  /** 订单数量 */
  ordered_qty: number;
  /** 已收数量（只算已过账的收货单） */
  received_qty: number;
  /** 待收数量 = 订单 − 已收（本单最多能收这么多） */
  pending_qty: number;
}

/** 表单状态 */
export interface StockInFormState {
  /** 单据 id（有值 = 编辑） */
  id: string;
  /** 来源采购单 id（D4；空串 = 无源直收） */
  purchase_order_id: string;
  /** 来源采购单号（只用于界面显示，不落库 —— 后端从 id 反查） */
  purchase_order_no: string;
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
  /** 来源采购单号（模糊；卡面 O3） */
  purchase_order_no: string;
  /** 单号 / 备注关键词 */
  keyword: string;
}

/** 行级按钮显隐 */
export interface StockInRowActions {
  /** 编辑（仅 `DRAFT`） */
  edit: boolean;
  /** 过账（仅 `DRAFT`） */
  post: boolean;
  /** 作废（`DRAFT` 直接作废 / `POSTED` 发起作废审批） */
  cancel: boolean;
  /** 撤回作废申请（仅 `PENDING_CANCEL`，且只有发起人能撤——后端兜底校验） */
  withdrawCancel: boolean;
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
    case 'PENDING_CANCEL':
      return '作废审批中';
    case 'CANCELLED':
      return '已作废';
    default:
      return String(status);
  }
}

/**
 * 状态 → TDesign tag 主题。
 *
 * `PENDING_CANCEL` 用 `warning`（橙色）：它既不是「正常」也不是「终态」，
 * 而是**等人复核**的中间态 —— 用绿色会被误读成没事，用红色会被误读成已作废。
 */
export function stockInStatusTheme(
  status: StockInStatus | string,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'POSTED':
      return 'success';
    case 'PENDING_CANCEL':
      return 'warning';
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
  { label: '作废审批中', value: 'PENDING_CANCEL' },
  { label: '已作废', value: 'CANCELLED' },
];

/**
 * 行级按钮显隐（卡面 §四 + 后端 `LEGAL_EDGES`）。
 *
 * | 状态 | 编辑 | 过账 | 作废 | 撤回作废 | 删除 | 查看 |
 * |---|---|---|---|---|---|---|
 * | `DRAFT` | ✅ | ✅ | ✅（直接作废） | — | ✅ | ✅ |
 * | `POSTED` | — | — | ✅（**发起审批**） | — | — | ✅ |
 * | `PENDING_CANCEL` | — | — | — | ✅ | — | ✅ |
 * | `CANCELLED` | — | — | — | — | — | ✅ |
 *
 * ⚠️ P16 起 `POSTED` 的「作废」**不立即动账**：它只发起作废审批（单据锁定、状态变
 * 「作废审批中」），六本账在**复核通过**那一刻才回滚。所以这一行不再有「作废即红冲」的错觉，
 * 提示文案见 {@link cancelConfirmMessage}。
 */
export function availableStockInActions(status: StockInStatus | string): StockInRowActions {
  const isDraft = status === 'DRAFT';
  const isPosted = status === 'POSTED';
  const isPendingCancel = status === 'PENDING_CANCEL';
  return {
    edit: isDraft,
    post: isDraft,
    cancel: isDraft || isPosted,
    withdrawCancel: isPendingCancel,
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

/**
 * 作废确认文案（P16：已过账的作废是**发起审批**，不是立即红冲）。
 *
 * `POSTED` 时说的是真实后果：单据进入「作废审批中」并**锁定**，六本账等复核通过才回滚。
 * 这句文案必须与后端行为逐字对齐 —— 旧文案「不会自动冲回库存（红冲在下一批开放）」
 * 早在 P9 就不成立了（P9 起作废会回滚六本账）。
 */
export function cancelConfirmMessage(status: StockInStatus | string): string {
  if (status === 'POSTED') {
    return (
      '提交后将进入「作废审批中」：单据在此期间被锁定（不能改数量 / 金额 / 明细），' +
      '六本账（库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号）在**审批通过**后才回滚；' +
      '审批驳回或撤回则单据回到已过账，账一点不动。确认发起作废审批？'
    );
  }
  if (status === 'PENDING_CANCEL') {
    return '该单的作废申请正在审批中：要取消请用「撤回」。';
  }
  return '作废后草稿将不可再编辑，确认作废？';
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
  return { status: '', supplier_id: '', warehouse_id: '', purchase_order_no: '', keyword: '' };
}

/** 单行空明细（税率默认 13%，与采购单同口径） */
export function emptyStockInItem(key: string): StockInItemForm {
  return { key, purchase_order_item_id: '', material_id: '', qty: 1, price_excl: 0, tax_rate: 13 };
}

/** 空表单（`invoice_received` 默认 `true`） */
export function emptyStockInForm(): StockInFormState {
  return {
    id: '',
    purchase_order_id: '',
    purchase_order_no: '',
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
    purchase_order_id: header.source_order_id ?? '',
    purchase_order_no: header.source_order_no ?? '',
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
            purchase_order_item_id: line.source_item_id ?? '',
            material_id: line.material_id,
            qty: toNumber(line.qty),
            price_excl: round6(toNumber(line.price)),
            tax_rate: toNumber(line.tax_rate),
          })),
  };
}

/**
 * 采购单待收明细 → 收货单草稿表单（D4 的「一键带出」）。
 *
 * 口径（卡面 F1 / F2 / O1）：
 * - 供应商 / 仓库 / 备注 / 来源单号**全部从源单带**，不让用户重填；
 * - 只带 `pending_qty > 0` 的行（已收满的行带出来只会让人误收），
 *   数量默认 = `pending_qty`（用户可以改小 = 部分收货，也可以删行）；
 * - 每行记 `purchase_order_item_id` + 待收快照，界面据此显示「订单 / 已收 / 待收」上限。
 *
 * @returns `form`（可直接 `Object.assign` 到页面表单）+ `lineCount`（带出几行，用于提示文案）
 */
export function formFromPending(
  pending: PendingReceiveResLike,
): { form: StockInFormState; lineCount: number } {
  const lines = pending.items.filter((item) => toNumber(item.pending_qty) > 0);
  const items: StockInItemForm[] = lines.map((item, index) => ({
    key: `po-${item.item_id}-${index + 1}`,
    purchase_order_item_id: item.item_id,
    material_id: item.material_id,
    // 默认数量 = 待收数量（本单最多能收这么多）；用户可以改小
    qty: toNumber(item.pending_qty),
    price_excl: round6(toNumber(item.price_excl)),
    tax_rate: toNumber(item.tax_rate),
    pending: {
      ordered_qty: toNumber(item.ordered_qty),
      received_qty: toNumber(item.received_qty),
      pending_qty: toNumber(item.pending_qty),
    },
  }));

  return {
    form: {
      id: '',
      purchase_order_id: pending.order.id,
      purchase_order_no: pending.order.order_no,
      supplier_id: pending.order.supplier_id,
      warehouse_id: pending.order.warehouse_id,
      in_date: todayString(),
      invoice_received: true,
      remark: '',
      // 一行待收都没有时留一个空行（后端会拦，但界面不该是零行鬼样子）
      items: items.length > 0 ? items : [emptyStockInItem('row-1')],
    },
    lineCount: items.length,
  };
}

/** `formFromPending` 只依赖这几个字段（用最小形状，便于单测直接构造） */
export interface PendingReceiveResLike {
  order: {
    id: string;
    order_no: string;
    supplier_id: string;
    warehouse_id: string;
  };
  items: ReadonlyArray<{
    item_id: string;
    material_id: string;
    ordered_qty: number | string;
    received_qty: number | string;
    pending_qty: number | string;
    price_excl: number | string;
    tax_rate: number | string;
  }>;
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
    } else if (item.pending && qty > item.pending.pending_qty) {
      // D4 防超收：前端先拦一次（后端还会按库里的 POSTED 聚合再判一次，超了 400）
      errors[`${prefix}.qty`] =
        `第 ${index + 1} 行本次最多可收 ${formatQty(item.pending.pending_qty)}` +
        `（订单 ${formatQty(item.pending.ordered_qty)} / 已收 ${formatQty(item.pending.received_qty)}）`;
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
    // D4：来源行 id 必须原样带回（后端按行回写已收数量、判超收）
    purchase_order_item_id: item.purchase_order_item_id || null,
    material_id: item.material_id,
    qty: toNumber(item.qty),
    price_excl: round6(toNumber(item.price_excl)),
    tax_rate: toNumber(item.tax_rate),
  }));

  return {
    // 带来源采购单时用业务别名 `purchase_order_id`（与 `source_order_id` 同义，只能给一个）
    purchase_order_id: form.purchase_order_id || null,
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
  // D4/O3：来源采购单号（后端模糊匹配）
  const orderNo = filter.purchase_order_no.trim();
  if (orderNo) {
    query.purchase_order_no = orderNo;
  }
  return query;
}

/** 是否处于筛选态 */
export function isStockInFiltered(filter: StockInFilterState): boolean {
  return Boolean(
    filter.status ||
      filter.supplier_id ||
      filter.warehouse_id ||
      filter.purchase_order_no.trim() ||
      filter.keyword.trim(),
  );
}

/** 过账按钮是否可用（无明细的草稿不该过账 —— 后端也会拦） */
export function canPost(row: StockInRow): boolean {
  return row.status === 'DRAFT' && toNumber(row.total_qty) > 0;
}
