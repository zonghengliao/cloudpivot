/**
 * 采购订单页纯逻辑（P2-E1）—— 口径计算 / 合计 / 校验 / 请求体 / 状态按钮显隐。
 *
 * # 为什么单独一个 `.ts`
 *
 * 用例跑在**没有 DOM** 的 node 里（`tsx --test`），`.vue` 里的 `setup()` 跑不起来。
 * 而「100×2, 13% 到底算出 200/26/226 还是 176.99/23.01/200」「这一行的按钮该不该亮」
 * 恰恰是会算错钱、会误操作的地方 —— 所以全部搬到这里，`.vue` 只负责渲染与发请求。
 *
 * # 公式口径：与后端逐字一致
 *
 * 真源是 `backend/src/business/erp/purchase_model.rs::line_amounts`（P2-A2）：
 *
 * ```text
 * 口径 A（EXCL 不含税录入）
 *   amount   = round2(qty × price)
 *   tax      = round2(amount × rate / 100)
 *   with_tax = amount + tax
 *
 * 口径 B（INCL 含税录入）
 *   with_tax = round2(qty × price)
 *   amount   = round2(with_tax / (1 + rate/100))
 *   tax      = with_tax − amount        // ★ 减法倒挤，不能 round2(with_tax × rate/(100+rate))
 * ```
 *
 * ⚠️ **口径 B 的税额必须是减法倒挤**：再乘一次税率会出现
 * `amount + tax ≠ with_tax` 的分钱差，而库里 `ck_biz_po_item_tax` / `ck_biz_si_item_tax`
 * 是**精确恒等**（`amount + tax_amount = with_tax_amount`），差一分钱就插不进库。
 *
 * ⚠️ 合计是**逐行舍入后再相加**（PRD §3.1.5「禁止先加总再舍入」），不是 `sum(qty×price)` 再 round。
 */
import type {
  ContractRow,
  PriceMode,
  PurchaseOrderItemLine,
  PurchaseOrderRow,
  PurchaseOrderStatus,
  PurchaseType,
  CreatePurchaseOrderReq,
  PurchaseOrderItemReq,
  PurchaseOrderStatusReq,
} from '@/api/business/erp';

/** 不含税录入（默认口径，`biz_purchase_order.price_mode` 的默认值） */
export const PRICE_MODE_EXCL: PriceMode = 'EXCL';

/** 含税录入 */
export const PRICE_MODE_INCL: PriceMode = 'INCL';

/** 合同采购（A3：供应商从合同乙方派生，只读） */
export const PURCHASE_TYPE_CONTRACT: PurchaseType = 'CONTRACT';

/** 现货采购（供应商手选，历史口径） */
export const PURCHASE_TYPE_SPOT: PurchaseType = 'SPOT';

/**
 * 采购类型下拉选项 —— **顺序即语义**：先选类型，供应商字段的形态跟着变。
 *
 * 卡面明确 `CONTRACT` 是默认值（合同驱动是主干流程，现货是例外）。
 */
export const PURCHASE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: PurchaseType }> = [
  { label: '合同采购', value: PURCHASE_TYPE_CONTRACT },
  { label: '现货采购', value: PURCHASE_TYPE_SPOT },
];

/**
 * 能不能被采购单引用的合同状态（**与后端 `contract_model::USABLE_FOR_ORDER` 逐字一致**）。
 *
 * ⚠️ 卡面写「只列出 status = ACTIVE」，但后端 `resolve_contract` 实际接受 `SIGNED` / `ACTIVE`
 * （ARCH-2 §5.1 V-22 与设计 §6.2 都是「已签署 / 生效」）。这里跟着后端走 ——
 * 否则用户会看到「合同可用 = true」却选不出来、下不了单。
 */
export const ORDERABLE_CONTRACT_STATUSES: ReadonlyArray<string> = ['SIGNED', 'ACTIVE'];

/** 每页条数（与其它 ERP 列表页同口径） */
export const PURCHASE_PAGE_SIZE = 20;

/** 明细行上限（后端 `MAX_LINES`，超了 400） */
export const MAX_LINES = 200;

/** 备注上限（后端 `MAX_REMARK_LEN`） */
export const MAX_REMARK = 500;

/** 一行算出来的三个金额（都不含货币符号，单位：元） */
export interface LineAmounts {
  /** 不含税金额 */
  amount: number;
  /** 税额 */
  tax: number;
  /** 价税合计 */
  total: number;
}

/** 单据合计 */
export interface PurchaseTotals extends LineAmounts {
  /** 合计数量 */
  qty: number;
}

/** 表单一行的可编辑状态（`price` 的含义由 `mode` 决定，见 {@link lineAmounts}） */
export interface PurchaseItemForm {
  /** 行内稳定 key（Vue `:key`，与 `line_no` 无关） */
  key: string;
  /** 物料 id */
  material_id: string;
  /** 数量 */
  qty: number;
  /** 录入单价：`EXCL` = 不含税单价，`INCL` = 含税单价 */
  price: number;
  /** 税率（0–100，不是 0–1） */
  tax_rate: number;
}

/** 表单状态 */
export interface PurchaseFormState {
  /** 单据 id（有值 = 编辑） */
  id: string;
  /**
   * 采购类型（A3）。**建单后不可改**（后端 409），编辑态只读回传。
   *
   * 它决定供应商从哪来：`CONTRACT` = 合同乙方（只读）/ `SPOT` = 用户手选。
   */
  purchase_type: PurchaseType;
  /** 合同 id（`CONTRACT` 必填；`SPOT` 必须为空） */
  contract_id: string;
  /** 合同号（**快照**，选中合同时写入，用于表单上「让操作员确认选对了」） */
  contract_no: string;
  /** 合同乙方名称（= 供应商显示值；供应商下拉数据可能还没加载完，这里留一份兜底） */
  contract_supplier_name: string;
  /** 供应商 id */
  supplier_id: string;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 下单日期 `YYYY-MM-DD` */
  order_date: string;
  /** 期望到货日（可空） */
  expect_date: string;
  /** 账期天数 */
  credit_days: number;
  /** 备注 */
  remark: string;
  /** ★ 口径（徽标读它，常驻） */
  price_mode: PriceMode;
  /** 明细 */
  items: PurchaseItemForm[];
}

/** 表单校验结果（字段 → 文案；空对象 = 通过） */
export type PurchaseFormErrors = Record<string, string>;

/** 筛选行 */
export interface PurchaseFilterState {
  /** 状态（`''` = 全部） */
  status: PurchaseOrderStatus | '';
  /** 供应商 id（`''` = 全部） */
  supplier_id: string;
  /** 单号 / 备注关键词 */
  keyword: string;
}

/** 行级按钮显隐 */
export interface PurchaseRowActions {
  /** 编辑（仅 `DRAFT`） */
  edit: boolean;
  /** 提交（仅 `DRAFT`） */
  submit: boolean;
  /** 关闭（仅 `SUBMITTED`） */
  close: boolean;
  /** 取消（`DRAFT` / `SUBMITTED`） */
  cancel: boolean;
  /** 删除（仅 `DRAFT`） */
  remove: boolean;
  /** 收货（`SUBMITTED` 及之后但未收完 → 跳收货单新建） */
  receive: boolean;
  /** 查看（永远可点） */
  view: boolean;
}

// ==================== 数值归一 ====================

/**
 * 后端 NUMERIC 出 JSON 可能是 number 也可能是字符串（`"226.0000"`）。
 *
 * ⚠️ 直接用 `+` 会得到 `"226.0000"` 字符串拼接（`"" + 1` 那种坑），所以统一过这里。
 */
export function toNumber(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * 舍入到 `dp` 位（默认 2），**四舍五入（half away from zero）**，与 Rust `Decimal::round_dp` 同向。
 *
 * 为什么带 `1e-9 * |scaled|` 的微扰：`0.145` 在 IEEE754 里是 `0.14499999999999999`，
 * 直接 `Math.round(0.145*100)` 得 14（错），加一个相对量级的微扰后才能得像 Rust 一样的 15。
 * 微扰只在「离中点 1e-11 以内」时起作用，正常金额不会落在那里。
 */
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

/** 金额统一 2 位（`money()`） */
export function round2(value: number): number {
  return roundTo(value, 2);
}

/** 单价 6 位（`unit_price()`，列 `NUMERIC(18,6)`） */
export function round6(value: number): number {
  return roundTo(value, 6);
}

// ==================== 口径计算 ====================

/**
 * 单行三金额 —— 与后端 `line_amounts(mode, qty, price, rate)` 逐行等价。
 *
 * @param mode     口径（`EXCL` 时 `price` 是不含税单价；`INCL` 时是含税单价）
 * @param qty      数量
 * @param price    录入单价（**口径决定含义**，不要自己先换算）
 * @param taxRate  税率 0–100
 */
export function lineAmounts(
  mode: PriceMode,
  qty: number,
  price: number,
  taxRate: number,
): LineAmounts {
  const safeQty = Number.isFinite(qty) ? qty : 0;
  const safePrice = Number.isFinite(price) ? price : 0;
  const safeRate = Number.isFinite(taxRate) ? taxRate : 0;

  if (mode === PRICE_MODE_INCL) {
    const total = round2(safeQty * safePrice);
    const divisor = 1 + safeRate / 100;
    const amount = divisor === 0 ? total : round2(total / divisor);
    // ★ 减法倒挤：保证 amount + tax === total（库 CHECK 精确恒等）
    return { amount, tax: round2(total - amount), total };
  }

  const amount = round2(safeQty * safePrice);
  const tax = round2((amount * safeRate) / 100);
  return { amount, tax, total: round2(amount + tax) };
}

/** 表单一行 → 三金额（缺字段按 0 处理，输入框清空时也会走到这里） */
export function lineAmountsOfItem(mode: PriceMode, item: PurchaseItemForm): LineAmounts {
  return lineAmounts(mode, toNumber(item.qty), toNumber(item.price), toNumber(item.tax_rate));
}

/**
 * 整单合计：**逐行舍入后相加**（PRD §3.1.5）。
 *
 * 先 `sum(qty × price)` 再舍入会与后端差钱（后端 `sum_lines` 明确逐行加），
 * 也会出现「弹窗合计 226.00、保存后列表 225.99」这种最难解释的 bug。
 */
export function computeTotals(
  mode: PriceMode,
  items: ReadonlyArray<PurchaseItemForm>,
): PurchaseTotals {
  let qty = 0;
  let amount = 0;
  let tax = 0;
  let total = 0;
  for (const item of items) {
    const line = lineAmountsOfItem(mode, item);
    qty = round6(qty + toNumber(item.qty));
    amount = round2(amount + line.amount);
    tax = round2(tax + line.tax);
    total = round2(total + line.total);
  }
  return { qty, amount, tax, total };
}

// ==================== 口径徽标（PRD §3.1.4） ====================

/** 徽标文案：`[不含税]` / `[含税]`（**常驻，不许省略**） */
export function priceModeLabel(mode: PriceMode): string {
  return mode === PRICE_MODE_INCL ? '含税' : '不含税';
}

/** 徽标 tooltip */
export function priceModeTip(mode: PriceMode): string {
  return mode === PRICE_MODE_INCL
    ? '含税录入：单价填价税合计，系统按各行税率反算不含税金额与税额'
    : '不含税录入：单价填不含税价，系统按各行税率加算税额';
}

/** 徽标配色（只用 TDesign 语义色变量语义，不写死色值） */
export function priceModeTheme(mode: PriceMode): 'primary' | 'warning' {
  return mode === PRICE_MODE_INCL ? 'warning' : 'primary';
}

/** 单价列标题 —— 切换口径后**列名跟着变**，否则用户会按旧含义填数 */
export function priceColumnTitle(mode: PriceMode): string {
  return mode === PRICE_MODE_INCL ? '含税单价' : '不含税单价';
}

/** 金额列标题：PRD 硬要求带「(价税合计)」，避免两人对着同一单说不同数 */
export const AMOUNT_COLUMN_TITLE = '金额(价税合计)';

/** 口径切换的确认文案（PRD §3.1.4 原文） */
export function switchConfirmMessage(to: PriceMode): string {
  return `切换为${priceModeLabel(to)}录入后，所有行将按各行税率重新计算金额，是否继续？`;
}

/**
 * 切换口径：**保留录入的单价数值，重新解释它的含义**。
 *
 * 这是 PRD 的行为：`100 × 2, 13%` 在不含税口径下合计 226.00，切到含税口径后
 * 同一串数字被当成含税单价 → 价税合计 200.00（不含税 176.99 + 税 23.01）。
 *
 * 另一种做法（把 226 保价换算成含税单价 2.26）会让「合计金额」整单漂移，
 * PRD 明确要的是「重新计算金额」，故这里**不动 price**，只换 mode。
 */
export function switchPriceMode(items: ReadonlyArray<PurchaseItemForm>, to: PriceMode): PriceMode {
  // 入参 items 保持不变是有意的：切换后金额由 computeTotals(newMode, items) 重新算。
  // 这里返回目标口径，方便调用方一行写完：`form.price_mode = switchPriceMode(form.items, 'INCL')`
  void items;
  return to;
}

/**
 * 反算某行的「当前口径录入单价」。
 *
 * 详情/明细接口（`/purchase-order-item/page`）**只给不含税 `price`**，没给 `price_incl`。
 * 含税口径回填时用 `with_tax_amount / qty` 反算 —— 这个值再乘数量会**精确还原**价税合计；
 * 而 `price × (1 + rate/100)` 在 6 位小数上会与库里 `price_incl` 差分钱。
 */
export function priceOfLineInMode(mode: PriceMode, line: PurchaseOrderItemLine): number {
  const qty = toNumber(line.qty);
  if (mode === PRICE_MODE_INCL) {
    if (qty <= 0) {
      return round6(toNumber(line.price));
    }
    return round6(toNumber(line.with_tax_amount) / qty);
  }
  return round6(toNumber(line.price));
}

// ==================== 格式化 ====================

/** 金额格式化：`226.00` / `12,345.60`（三位一组，固定 2 位） */
export function formatMoney(value: number | string | null | undefined): string {
  const num = toNumber(value);
  const negative = num < 0;
  const fixed = Math.abs(round2(num)).toFixed(2);
  const [intPart, decPart] = fixed.split('.');
  const grouped = (intPart ?? '0').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${negative ? '-' : ''}${grouped}.${decPart ?? '00'}`;
}

/** 数量格式化：整数不补小数，4 位以内保留（`100` / `2.5`） */
export function formatQty(value: number | string | null | undefined): string {
  const num = toNumber(value);
  if (Number.isInteger(num)) {
    return String(num);
  }
  return String(round6(num));
}

/** 日期 `YYYY-MM-DD` → `MM-DD`（列表窄列用；解析失败原样返回） */
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

/**
 * 主数据名 Map 取值 —— **空串当作没命中**。
 *
 * 库里 `supplier_name` 是 `NOT NULL`，但接口/前端缓存里出现过 `''`（导入脏数据），
 * 若直接当命中，列表上会出现一格空白而不是回落到快照名。
 */
function nameFromMap(id: string | null | undefined, map: ReadonlyMap<string, string>): string | null {
  if (!id) {
    return null;
  }
  const name = map.get(id);
  return typeof name === 'string' && name.trim() !== '' ? name : null;
}

/**
 * 供应商显示名（列表 / 报表 / 导出口径）—— **主档现值 → 单据快照 → `—`**。
 *
 * 口径真源：`docs/编码规范.md` §1.10（S2）+ `docs/architecture/P2-7-供应商名口径盘点.md` §3。
 *
 * # 为什么是「现值优先」而不是「快照优先」
 *
 * 列表/报表是**看现在**的地方：供应商改名后，列表必须跟着变（否则同一家供应商在两张单上
 * 出现两个名字，对账的人会以为是两家）。审计「当时叫什么」由详情页并列的风控快照栏承担（S3）。
 *
 * # 为什么**不能**回落 uuid（本函数存在的理由）
 *
 * 旧实现是 `supplierNameMap.get(id) ?? shortId(id)`：主档被软删 / 主数据清单还没拉回来时，
 * 列表上会显示 `0f8a3c1d…` 这种 uuid 短号 —— 用户既看不懂，也**丢掉了后端已经下发的快照名**
 * （`PurchaseOrderRes.supplier_name` = `supplier_name_snapshot`，存量 NULL 时后端已用现值兜底）。
 * 现在这条兜底被**彻底删掉**：查不到就回落到快照，快照也没有才是 `—`。
 */
export function supplierDisplayName(
  supplierId: string | null | undefined,
  snapshotName: string | null | undefined,
  supplierNameMap: ReadonlyMap<string, string>,
): string {
  const current = nameFromMap(supplierId, supplierNameMap);
  if (current) {
    return current;
  }
  if (typeof snapshotName === 'string' && snapshotName.trim() !== '') {
    return snapshotName;
  }
  return '—';
}

/** 供应商现名（详情页「供应商（现名）」/ 表单只读字段；只认主档现值，查不到给 `—`，**不落 uuid**） */
export function supplierCurrentNameOf(
  supplierId: string | null | undefined,
  supplierNameMap: ReadonlyMap<string, string>,
): string {
  return nameFromMap(supplierId, supplierNameMap) ?? '—';
}

/** 短 id（主数据没加载完时兜底，别留白） */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/** 北京时间的今天（`YYYY-MM-DD`）—— 与后端 `today_cst()` 同口径 */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 10);
}

// ==================== 状态 ====================

/** 状态 → 中文（与 `purchase_model::status_label` 同文案；`SUBMITTED` 在本卡语义是「审批中」） */
export function purchaseStatusLabel(status: PurchaseOrderStatus | string): string {
  switch (status) {
    case 'DRAFT':
      return '草稿';
    case 'SUBMITTED':
      return '审批中';
    case 'APPROVED':
      return '已审批';
    case 'PARTIAL_RECEIVED':
      return '部分收货';
    case 'RECEIVED':
      return '已收货';
    case 'CLOSED':
      return '已关闭';
    case 'CANCELLED':
      return '已取消';
    default:
      return String(status);
  }
}

/** 状态 → TDesign tag 主题 */
export function purchaseStatusTheme(
  status: PurchaseOrderStatus | string,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SUBMITTED':
      return 'primary';
    case 'APPROVED':
      return 'primary';
    case 'PARTIAL_RECEIVED':
      return 'warning';
    case 'RECEIVED':
      return 'success';
    case 'CLOSED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 状态下拉候选（`''` = 全部，排第一） */
export const PURCHASE_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: '' | PurchaseOrderStatus }> = [
  { label: '全部状态', value: '' },
  { label: '草稿', value: 'DRAFT' },
  { label: '审批中', value: 'SUBMITTED' },
  { label: '已审批', value: 'APPROVED' },
  { label: '部分收货', value: 'PARTIAL_RECEIVED' },
  { label: '已收货', value: 'RECEIVED' },
  { label: '已关闭', value: 'CLOSED' },
  { label: '已取消', value: 'CANCELLED' },
];

/**
 * 行级按钮显隐 —— 与后端 `LEGAL_EDGES` 对齐（只亮**真能成功**的按钮）。
 *
 * | 状态 | 编辑 | 提交 | 关闭 | 取消 | 删除 | 收货 | 查看 |
 * |---|---|---|---|---|---|---|---|
 * | `DRAFT` | ✅ | ✅ | — | ✅ | ✅ | — | ✅ |
 * | `SUBMITTED` | — | — | ✅ | ✅ | — | ✅ | ✅ |
 * | `APPROVED` / `PARTIAL_RECEIVED` | — | — | — | — | — | ✅ | ✅ |
 * | `RECEIVED` / `CLOSED` / `CANCELLED` | — | — | — | — | — | — | ✅ |
 *
 * 后端服务端还会再判一次（409），这里只是不让用户点了才被告知不行。
 */
export function availablePurchaseActions(status: PurchaseOrderStatus | string): PurchaseRowActions {
  const isDraft = status === 'DRAFT';
  const isSubmitted = status === 'SUBMITTED';
  const canReceive = isSubmitted || status === 'APPROVED' || status === 'PARTIAL_RECEIVED';
  return {
    edit: isDraft,
    submit: isDraft,
    close: isSubmitted,
    cancel: isDraft || isSubmitted,
    remove: isDraft,
    receive: canReceive,
    view: true,
  };
}

// ==================== 采购类型 / 合同（A3） ====================

/** 采购类型中文名（列表列、详情、徽标共用一处，别在模板里写三元） */
export function purchaseTypeLabel(type: PurchaseType | string | null | undefined): string {
  return type === PURCHASE_TYPE_CONTRACT ? '合同采购' : '现货采购';
}

/** 采购类型徽标配色：合同采购用主色（主干流程），现货采购用中性色 */
export function purchaseTypeTheme(type: PurchaseType | string | null | undefined): 'primary' | 'default' {
  return type === PURCHASE_TYPE_CONTRACT ? 'primary' : 'default';
}

/** 是否合同采购（模板里判断字段形态的唯一入口） */
export function isContractPurchase(form: Pick<PurchaseFormState, 'purchase_type'>): boolean {
  return form.purchase_type === PURCHASE_TYPE_CONTRACT;
}

/**
 * 采购类型是否只读 —— **编辑态一律只读**。
 *
 * 后端改单时 `purchase_type` 与库里不同会 409；前端让它从一开始就点不动，
 * 比点完再弹「已锁定」体面。
 */
export function isPurchaseTypeLocked(form: Pick<PurchaseFormState, 'id'>): boolean {
  return Boolean(form.id);
}

/** 合同选择器是否只读：编辑态 + 合同采购 → 不让换合同（避免洗掉关联） */
export function isContractLocked(form: Pick<PurchaseFormState, 'id' | 'purchase_type'>): boolean {
  return Boolean(form.id) && isContractPurchase(form);
}

/**
 * 切换采购类型时要把「另一条路」的残留清掉，否则会带着不该存在的字段提交：
 *
 * - → `CONTRACT`：供应商改由合同带，`supplier_id` 清空（等选合同），合同字段就位；
 * - → `SPOT`：`contract_id` / 合同号 / 乙方名清空（后端 SPOT 带 contract_id 直接 400）。
 *
 * @returns 同一个 form（就地改，返回便于单测断言）
 */
export function applyPurchaseType<F extends PurchaseFormState>(form: F, next: PurchaseType): F {
  if (isPurchaseTypeLocked(form)) {
    return form; // 建单后不可改：后端 409，这里直接不动
  }
  form.purchase_type = next;
  if (next === PURCHASE_TYPE_CONTRACT) {
    form.supplier_id = '';
    form.contract_id = '';
    form.contract_no = '';
    form.contract_supplier_name = '';
  } else {
    form.contract_id = '';
    form.contract_no = '';
    form.contract_supplier_name = '';
  }
  return form;
}

/** 合同下拉只列「采购类 + 可下单状态」（`SIGNED` / `ACTIVE`，与后端 `USABLE_FOR_ORDER` 同步） */
export function selectableContracts(rows: ReadonlyArray<ContractRow>): ContractRow[] {
  return rows.filter(
    (row) => row.contract_type === 'PURCHASE' && ORDERABLE_CONTRACT_STATUSES.includes(row.status),
  );
}

/**
 * 合同下拉的一项文案：**合同号 + 名称 + 乙方**（操作员要靠这三样确认没选错）。
 *
 * 卡面还要求「剩余可用金额」，但 `GET /erp/contract/page` 只给合同总额、没有已下单额 /
 * 剩余额（A5 已报告该接口无聚合字段），所以这里只在有金额时附上**合同总额**并标注清楚，
 * 不假装是剩余额。
 */
export function contractOptionLabel(contract: Pick<ContractRow, 'contract_no' | 'name' | 'party_b_name' | 'amount'>): string {
  const amount = toNumber(contract.amount);
  const amountText = amount > 0 ? ` · 合同总额 ${formatMoney(amount)}` : '';
  return `${contract.contract_no} · ${contract.name} · 乙方：${contract.party_b_name}${amountText}`;
}

/**
 * 选中合同 → 把乙方灌进表单（**供应商字段的唯一写入点**）。
 *
 * 供应商 id 与名称同时写：id 用于提交（合同采购时后端其实不看，但不留空能防呆），
 * 名称用于「供应商下拉还没加载完」时的只读展示兜底。
 *
 * @returns 同一个 form（就地改；返回便于单测断言）
 */
export function applyContractToForm<F extends PurchaseFormState>(
  form: F,
  contract: Pick<ContractRow, 'id' | 'contract_no' | 'party_b_id' | 'party_b_name' | 'party_b_current_name'>,
): F {
  form.contract_id = contract.id;
  form.contract_no = contract.contract_no;
  form.contract_supplier_name = contract.party_b_current_name || contract.party_b_name;
  form.supplier_id = contract.party_b_id;
  return form;
}

/** 供应商字段下面那行说明（合同采购才有；空串 = 不显示） */
export function supplierHintOf(form: Pick<PurchaseFormState, 'purchase_type'>): string {
  return isContractPurchase(form) ? '供应商来自合同乙方，如需更换请改合同' : '';
}

/** 合同号展示（空 = 未选合同） */
export function contractNoText(form: Pick<PurchaseFormState, 'contract_id' | 'contract_no'>): string {
  return form.contract_no || (form.contract_id ? '（合同号缺失）' : '');
}

/** 空筛选 */
export function emptyPurchaseFilter(): PurchaseFilterState {
  return { status: '', supplier_id: '', keyword: '' };
}

/** 单行空明细 */
export function emptyPurchaseItem(key: string): PurchaseItemForm {
  return { key, material_id: '', qty: 1, price: 0, tax_rate: 13 };
}

/** 空表单（默认不含税口径 —— 徽标开局就是「不含税」；采购类型默认合同采购，卡面口径） */
export function emptyPurchaseForm(): PurchaseFormState {
  return {
    id: '',
    purchase_type: PURCHASE_TYPE_CONTRACT,
    contract_id: '',
    contract_no: '',
    contract_supplier_name: '',
    supplier_id: '',
    warehouse_id: '',
    order_date: todayString(),
    expect_date: '',
    credit_days: 0,
    remark: '',
    price_mode: PRICE_MODE_EXCL,
    items: [emptyPurchaseItem('row-1')],
  };
}

/**
 * 已有单据 → 表单（编辑回填）。
 *
 * `items` 传 `PurchaseOrderItemLine[]`（明细分页接口的形状），
 * 录入单价按单据口径反算（见 {@link priceOfLineInMode}）。
 */
export function detailToForm(
  order: PurchaseOrderRow,
  items: ReadonlyArray<PurchaseOrderItemLine>,
): PurchaseFormState {
  const mode: PriceMode = order.price_mode === PRICE_MODE_INCL ? PRICE_MODE_INCL : PRICE_MODE_EXCL;
  return {
    id: order.id,
    // 存量单后端已回填 `SPOT`，这里只在明确 `CONTRACT` 时才认合同采购（缺字段也别翻成合同）
    purchase_type: order.purchase_type === PURCHASE_TYPE_CONTRACT ? PURCHASE_TYPE_CONTRACT : PURCHASE_TYPE_SPOT,
    contract_id: order.contract_id ?? '',
    contract_no: order.contract_no ?? order.contract_no_snapshot ?? '',
    contract_supplier_name: order.supplier_name ?? '',
    supplier_id: order.supplier_id ?? '',
    warehouse_id: order.warehouse_id ?? '',
    order_date: order.order_date ?? todayString(),
    expect_date: order.expect_date ?? '',
    credit_days: toNumber(order.credit_days),
    remark: order.remark ?? '',
    price_mode: mode,
    items:
      items.length === 0
        ? [emptyPurchaseItem('row-1')]
        : items.map((line, index) => ({
            key: `row-${index + 1}`,
            material_id: line.material_id,
            qty: toNumber(line.qty),
            price: priceOfLineInMode(mode, line),
            tax_rate: toNumber(line.tax_rate),
          })),
  };
}

/**
 * 表单校验（与后端 `purchase_dto::prepare` 同规则）。
 *
 * 后端会再判一次（400），但等到点「保存」才知道第 3 行数量是 0 的体验太差；
 * 这里返回 `{ 字段路径: 文案 }`，`.vue` 直接挂到 `t-form-item` 的 `tips` 上。
 */
export function validatePurchaseForm(form: PurchaseFormState): PurchaseFormErrors {
  const errors: PurchaseFormErrors = {};

  // 合同采购：供应商由合同乙方派生 → 拦「没选合同」，不拦「供应商为空」（那是结果不是原因）
  if (isContractPurchase(form)) {
    if (!form.contract_id) {
      errors.contract_id = '合同采购必须选择合同（供应商由合同乙方带出）';
    }
  } else if (!form.supplier_id) {
    errors.supplier_id = '请选择供应商';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选择收货仓库';
  }
  if (!form.order_date) {
    errors.order_date = '请选择下单日期';
  }
  if (form.expect_date && form.order_date && form.expect_date < form.order_date) {
    errors.expect_date = '期望到货日不能早于下单日期';
  }
  if (toNumber(form.credit_days) < 0) {
    errors.credit_days = '账期天数不能为负';
  }
  if (form.remark.length > MAX_REMARK) {
    errors.remark = `备注最多 ${MAX_REMARK} 字（当前 ${form.remark.length} 字）`;
  }
  if (form.items.length === 0) {
    errors.items = '至少要有一行明细';
  }
  if (form.items.length > MAX_LINES) {
    errors.items = `明细最多 ${MAX_LINES} 行（当前 ${form.items.length} 行）`;
  }

  for (const [index, item] of form.items.entries()) {
    const prefix = `items.${index}`;
    if (!item.material_id) {
      errors[`${prefix}.material_id`] = '请选择物料';
    }
    const qty = toNumber(item.qty);
    if (!(qty > 0)) {
      // 后端文案：ck_biz_po_item_qty 要求 qty > 0，草稿也不许 0
      errors[`${prefix}.qty`] = `第 ${index + 1} 行数量必须大于 0（草稿同样不允许 0 数量）`;
    } else if (roundTo(qty, 4) !== qty) {
      errors[`${prefix}.qty`] = `第 ${index + 1} 行数量最多 4 位小数`;
    }
    const price = toNumber(item.price);
    if (price < 0) {
      errors[`${prefix}.price`] = `第 ${index + 1} 行单价不能为负（退货请走作废红冲）`;
    } else if (roundTo(price, 6) !== price) {
      // 单价列 NUMERIC(18,6)：超过 6 位小数后端会静默截断，先在这里拦
      errors[`${prefix}.price`] = `第 ${index + 1} 行单价最多 6 位小数`;
    }
    const rate = toNumber(item.tax_rate);
    if (rate < 0 || rate > 100) {
      errors[`${prefix}.tax_rate`] = `第 ${index + 1} 行税率必须在 0–100 之间`;
    }
  }

  return errors;
}

/** 校验是否通过 */
export function isPurchaseFormValid(form: PurchaseFormState): boolean {
  return Object.keys(validatePurchaseForm(form)).length === 0;
}

/**
 * 表单 → 建单 / 改单请求体。
 *
 * ⚠️ **不传金额**：三金额一律服务端算（硬红线），前端算的只用于弹窗上那行小字。
 * 口径决定单价塞进哪个字段：`EXCL → price_excl`、`INCL → price_incl`。
 */
export function buildPurchasePayload(form: PurchaseFormState): CreatePurchaseOrderReq {
  const mode: PriceMode = form.price_mode === PRICE_MODE_INCL ? PRICE_MODE_INCL : PRICE_MODE_EXCL;
  const items: PurchaseOrderItemReq[] = form.items.map((item) => {
    const price = round6(toNumber(item.price));
    const line: PurchaseOrderItemReq = {
      material_id: item.material_id,
      qty: toNumber(item.qty),
      tax_rate: toNumber(item.tax_rate),
    };
    if (mode === PRICE_MODE_INCL) {
      line.price_incl = price;
    } else {
      line.price_excl = price;
    }
    return line;
  });

  const isContract = form.purchase_type === PURCHASE_TYPE_CONTRACT;
  // 合同采购只发 contract_id（供应商由服务端从合同乙方派生，别把派生值再发回去）：
  // 前端传了不一致的 supplier_id 会被 400 `66025` 顶回来，传一致的也只是幂等噪声；
  // 现货采购反之 —— 必须发 supplier_id，且 contract_id 一个字段都不能带（DTO 会 400）。
  const head = isContract
    ? { purchase_type: form.purchase_type, contract_id: form.contract_id || null }
    : { purchase_type: form.purchase_type, contract_id: null, supplier_id: form.supplier_id };

  return {
    ...head,
    warehouse_id: form.warehouse_id,
    order_date: form.order_date,
    expect_date: form.expect_date || null,
    price_mode: mode,
    credit_days: toNumber(form.credit_days),
    remark: form.remark.trim() || null,
    items,
  };
}

/** 状态流转请求体（`reason` 去掉首尾空白，空串发 `null`） */
export function buildStatusPayload(
  status: PurchaseOrderStatusReq['status'],
  reason?: string,
): PurchaseOrderStatusReq {
  const text = (reason ?? '').trim();
  return text ? { status, reason: text } : { status };
}

/**
 * 筛选 → 查询参数。
 *
 * 空串一律不发（后端把 `?keyword=` 当「等于空串」过滤会查出 0 行），
 * 与 `api/business/erp/index.ts::asParams` 同一约定；这里先去掉是为了单测能断言。
 */
export function buildPurchaseQuery(
  filter: PurchaseFilterState,
  page: number,
  pageSize: number = PURCHASE_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.supplier_id) {
    query.supplier_id = filter.supplier_id;
  }
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/** 是否处于筛选态（空态文案分「没数据」与「没匹配」） */
export function isPurchaseFiltered(filter: PurchaseFilterState): boolean {
  return Boolean(filter.status || filter.supplier_id || filter.keyword.trim());
}

/** 明细行数文案 */
export function lineCountText(total: number): string {
  return `${total} 行明细`;
}

// ==================== 信用额度（P2-2 ①） ====================

/**
 * 信用额度评估的展示口径（三项 + 是否已评估）。
 *
 * 三种状态**不能混**（混了就会把「没查过」显示成「没问题」）：
 *
 * | `known` | `over` | 显示 | 含义 |
 * |---|---|---|---|
 * | `false` | `false` | 未评估 | 草稿 / 从未提交过（`credit_exposure IS NULL`） |
 * | `true` | `true` | 超信用额度 | 提交时点累计 > 额度，已强制走审批 |
 * | `true` | `false` | 额度内 / 额度不限 | 提交时点查过，没超 |
 */
export interface CreditVerdictView {
  /** 是否评估过（`credit_exposure` 非空） */
  known: boolean;
  /** 是否超限 */
  over: boolean;
  /** 短标签（标签/徽标用） */
  label: string;
  /** 完整说明（tooltip 用：累计、额度、差额） */
  tip: string;
}

/**
 * 由单据三列算展示口径。
 *
 * 金额一律走 {@link formatMoney}（后端 `Decimal` 出参是字符串，直接拼会出现 `20000.0000`）。
 * 额度为 `0` 时显示「额度不限」—— 这是 `biz_supplier.credit_limit` 的既有语义（V0137），
 * 不要显示成「额度 0 / 已超 0」。
 */
export function creditVerdictOf(
  row: Pick<PurchaseOrderRow, 'over_credit_limit' | 'credit_exposure' | 'credit_limit_snapshot'>,
): CreditVerdictView {
  if (row.credit_exposure === null || row.credit_exposure === undefined) {
    return {
      known: false,
      over: false,
      label: '未评估',
      tip: '草稿还没做过信用评估：提交时才现算该供应商的累计未结采购额',
    };
  }
  const exposure = toNumber(row.credit_exposure);
  const limit = toNumber(row.credit_limit_snapshot);
  if (row.over_credit_limit) {
    return {
      known: true,
      over: true,
      label: '超信用额度',
      tip: `提交时累计未结采购额 ${formatMoney(exposure)}，超出供应商信用额度 ${formatMoney(limit)}（必须走审批）`,
    };
  }
  if (limit <= 0) {
    return {
      known: true,
      over: false,
      label: '额度不限',
      tip: `提交时累计未结采购额 ${formatMoney(exposure)}；该供应商信用额度为 0（不限）`,
    };
  }
  return {
    known: true,
    over: false,
    label: '额度内',
    tip: `提交时累计未结采购额 ${formatMoney(exposure)} / 额度 ${formatMoney(limit)}，剩余 ${formatMoney(limit - exposure)}`,
  };
}
