/**
 * 销售订单页纯逻辑（SO-07）—— 所有可测的判断都在这里，`.vue` 只负责渲染。
 *
 * # 字段口径
 *
 * 出参与入参逐字抄 `backend/src/business/erp/sales_order_handler.rs`
 * （`SalesOrderRes` / `CreateSalesOrderReq` / `UpdateSalesOrderReq` / `SalesOrderItemReq`），
 * 前端类型在 `@/api/business/erp` 的「销售订单」段。**别照采购单抄**：采购是
 * `order_no` / `supplier_id` / `price_mode`，销售是 `so_no` / `customer_id`，没有
 * 含税价口径（单价恒为不含税，税额单算）。
 *
 * # 金额谁算
 *
 * **后端算，前端只做预览**：
 * - 提交只发 `qty` / `price` / `tax_rate`（`SalesOrderItemReq` 里没有金额字段）；
 * - 列表 / 详情的 `amount` / `tax_amount` / 单头三项一律显示服务端值；
 * - 表单里那三个金额是 {@link computeLine} 现算的**预览**，口径尽量贴后端
 *   （逐行先 4 位定标再相加，见 `sales_order_model::Totals::of`），但差异以服务端为准。
 *
 * # 状态与按钮
 *
 * 状态域 7 值（`sales_order_model::STATUSES`），合法边 11 条
 * （`state_machine::SALES_ORDER_EDGES`）。按钮显隐**只看状态**，不看「我是不是制单人」：
 * - 提交：`DRAFT → SUBMITTED`（走 `POST /{id}/submit-approval`）；
 * - 撤回审批：`SUBMITTED → DRAFT`（**走审批引擎**
 *   `POST /business/flow/instance/{id}/withdraw`，不是 PATCH 状态）；
 * - 作废：`{DRAFT, SUBMITTED, APPROVED} → CANCELLED`（需要原因）；
 * - 关闭：`{APPROVED, PARTIAL_OUT} → CLOSED`（需要原因）。
 *
 * `APPROVED` / `DRAFT` 这两个目标态**不许**走 `PATCH /status`（后端 400）——
 * 审批结论只由审批中心写，见 SO-06 的硬口径。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  SalesOrder,
  SalesOrderItem,
  SalesOrderItemReq,
  SalesOrderStatus,
  CreateSalesOrderReq,
  UpdateSalesOrderReq,
} from '@/api/business/erp';
import type { PriceResolveRes, PriceSource } from '@/api/business/erp/customer-price';
import { priceSourceLabel, priceSourceTheme } from '@/views/business/erp/shared/price-source';

/** 每页条数（与采购 / 收货列表同口径；后端上限 200、`0` → 20） */
export const SALES_ORDER_PAGE_SIZE = 20;

/** 备注上限（`V0264` 的 `remark` 是 `VARCHAR(500)`） */
export const SALES_ORDER_REMARK_MAX = 500;

/** 明细行数上限（表上没有硬限制，纯防手滑；后端也接受更多） */
export const SALES_ORDER_MAX_LINES = 200;

/** 状态中文名（与后端 `sales_model::status_label` 逐字一致；后端也给 `status_label`，这是兜底） */
export const SALES_ORDER_STATUS_LABELS: Record<SalesOrderStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '审批中',
  APPROVED: '已审批',
  PARTIAL_OUT: '部分出库',
  OUTBOUND: '已出库',
  CLOSED: '已关闭',
  CANCELLED: '已作废',
};

/**
 * 状态标签配色（**对齐现有规范**：采购单 `purchaseStatusTheme` 同一套语义）。
 *
 * `DRAFT` 灰、审批中 / 已审批 品牌蓝、部分出库 橙、完成 绿、作废 红。
 */
export const SALES_ORDER_STATUS_THEMES: Record<
  SalesOrderStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  SUBMITTED: 'primary',
  APPROVED: 'primary',
  PARTIAL_OUT: 'warning',
  OUTBOUND: 'success',
  CLOSED: 'success',
  CANCELLED: 'danger',
};

/** 状态下拉候选（顺序 = 状态机走向；`''` = 全部，排第一） */
export const SALES_ORDER_STATUS_OPTIONS: ReadonlyArray<{
  label: string;
  value: '' | SalesOrderStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿', value: 'DRAFT' },
  { label: '审批中', value: 'SUBMITTED' },
  { label: '已审批', value: 'APPROVED' },
  { label: '部分出库', value: 'PARTIAL_OUT' },
  { label: '已出库', value: 'OUTBOUND' },
  { label: '已关闭', value: 'CLOSED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 所有状态编码（后端顺序，用于「未知值」兜底判断） */
export const SALES_ORDER_STATUSES: readonly SalesOrderStatus[] = [
  'DRAFT',
  'SUBMITTED',
  'APPROVED',
  'PARTIAL_OUT',
  'OUTBOUND',
  'CLOSED',
  'CANCELLED',
];

// ==================== 状态判定 ====================

/** 状态中文名：后端 `status_label` 优先，认不出编码时回落编码本身（别显示空白） */
export function salesOrderStatusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return (
    SALES_ORDER_STATUS_LABELS[status as SalesOrderStatus] ?? status
  );
}

/** 状态标签配色：未知状态回落 `default`（不猜颜色） */
export function salesOrderStatusTheme(
  status: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!status) {
    return 'default';
  }
  return SALES_ORDER_STATUS_THEMES[status as SalesOrderStatus] ?? 'default';
}

/** 只有草稿能改（其余状态后端 409，前端先别让人点） */
export function isEditable(status: string | null | undefined): boolean {
  return status === 'DRAFT';
}

/** 提交按钮：`DRAFT`（走 `POST /{id}/submit-approval`） */
export function canSubmit(status: string | null | undefined): boolean {
  return status === 'DRAFT';
}

/**
 * 撤回审批：`SUBMITTED`。
 *
 * 走**审批引擎**（`POST /business/flow/instance/{id}/withdraw` → 实例 WITHDRAWN →
 * 回调把单据打回 `DRAFT`），**不是** `PATCH /status`。
 */
export function canWithdraw(status: string | null | undefined): boolean {
  return status === 'SUBMITTED';
}

/** 作废：`{DRAFT, SUBMITTED, APPROVED} → CANCELLED`（11 条合法边里的三条） */
export function canCancel(status: string | null | undefined): boolean {
  return status === 'DRAFT' || status === 'SUBMITTED' || status === 'APPROVED';
}

/** 关闭：`{APPROVED, PARTIAL_OUT} → CLOSED` */
export function canClose(status: string | null | undefined): boolean {
  return status === 'APPROVED' || status === 'PARTIAL_OUT';
}

/**
 * 需要原因的状态边（与 `state_machine::SALES_ORDER_BASE` 的 `requires_reason` 逐条一致）：
 *
 * | 边 | 原因 |
 * |----|------|
 * | `SUBMITTED → CANCELLED` | 审批中作废（要撤审批实例） |
 * | `APPROVED → CANCELLED` | 已审批作废 |
 * | `APPROVED → CLOSED` | 关闭（剩余量作废） |
 * | `PARTIAL_OUT → CLOSED` | 关闭（剩余量作废） |
 *
 * `DRAFT → CANCELLED` **不需要**原因（草稿从没占用额度，也没发过货）。
 */
export function statusNeedsReason(from: string | null | undefined, to: SalesOrderStatus): boolean {
  if (to === 'CLOSED') {
    return from === 'APPROVED' || from === 'PARTIAL_OUT';
  }
  if (to === 'CANCELLED') {
    return from === 'SUBMITTED' || from === 'APPROVED';
  }
  return false;
}

/**
 * 列表行操作条的显隐（一次算清，模板里不再写条件）。
 *
 * ⚠️ **撤回不看 `approval_instance_id`**：列表行的实例 id 恒为 `null`（后端只在
 * 详情补这条），真要点撤回时页面再拉详情拿 id —— 见 `index.vue::withdrawOrder`。
 */
export function rowActions(row: {
  status: string | null | undefined;
}): {
  edit: boolean;
  submit: boolean;
  withdraw: boolean;
  cancel: boolean;
  close: boolean;
} {
  const status = row.status;
  return {
    edit: isEditable(status),
    submit: canSubmit(status),
    withdraw: canWithdraw(status),
    cancel: canCancel(status),
    close: canClose(status),
  };
}

// ==================== 列表查询 ====================

/** 列表筛选（`SalesOrderFilter`：`so_no` 模糊，`status` / `customer_id` 精确） */
export interface SalesOrderFilter {
  /** 单号（模糊） */
  so_no: string;
  /** 状态（`''` = 全部） */
  status: '' | SalesOrderStatus;
  /** 客户 id（`''` = 全部） */
  customer_id: string;
}

/** 空筛选（每次重置都造一份新的，别共享引用） */
export function emptySalesOrderFilter(): SalesOrderFilter {
  return { so_no: '', status: '', customer_id: '' };
}

/** 是否处于筛选态（重置按钮 / "已筛选" 提示用） */
export function isSalesOrderFiltered(filter: SalesOrderFilter): boolean {
  return Boolean(filter.so_no.trim()) || Boolean(filter.status) || Boolean(filter.customer_id);
}

/**
 * 筛选 + 分页 → 请求参数。
 *
 * 空串**根本不发**：后端把 `?so_no=` 当「`so_no` = 空串」过滤，会查出 0 行
 * （本仓所有列表页同一个坑）。
 */
export function buildSalesOrderQuery(
  filter: SalesOrderFilter,
  page: number,
  pageSize: number = SALES_ORDER_PAGE_SIZE,
): {
  so_no?: string;
  status?: SalesOrderStatus;
  customer_id?: string;
  page: number;
  page_size: number;
} {
  const query: {
    so_no?: string;
    status?: SalesOrderStatus;
    customer_id?: string;
    page: number;
    page_size: number;
  } = { page, page_size: pageSize };
  const soNo = filter.so_no.trim();
  if (soNo) {
    query.so_no = soNo;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.customer_id) {
    query.customer_id = filter.customer_id;
  }
  return query;
}

/** 列表列（订单日期 / 客户 / 金额 / 状态；明细与操作列由页面插槽渲染） */
export const SALES_ORDER_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'so_no', title: '订单编号', width: 190, fixed: 'left' },
  { colKey: 'customer_name', title: '客户', minWidth: 200, ellipsis: true },
  { colKey: 'order_date', title: '订单日期', width: 120 },
  { colKey: 'delivery_date', title: '要求交期', width: 120 },
  { colKey: 'total_with_tax', title: '价税合计', width: 150, align: 'right' },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'op', title: '操作', width: 220, fixed: 'right' },
];

/** 详情 / 表单里的明细列（行号 / 物料 / 数量 / 单价 / 税率 / 金额 / 税额） */
export const SALES_ORDER_ITEM_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行号', width: 70 },
  { colKey: 'material_id', title: '物料', minWidth: 220, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'price', title: '不含税单价', width: 130, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 90, align: 'right' },
  { colKey: 'amount', title: '金额', width: 130, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 120, align: 'right' },
];

// ==================== 金额（预览口径） ====================

/** 四舍五入到 `dp` 位（后端 `Decimal::round_dp` 的 half-away-from-zero，正数下等价） */
function roundTo(value: number, dp: number): number {
  const factor = 10 ** dp;
  return Math.round(Math.abs(value) * factor + 1e-9) / factor * (value < 0 ? -1 : 1);
}

/** 一行明细的金额预览（与后端 `line_amounts` 同口径：先 4 位定标，税额基于定标后的金额） */
export interface LineAmounts {
  /** 行不含税金额 */
  amount: number;
  /** 行税额 */
  tax_amount: number;
  /** 行价税合计 */
  with_tax: number;
}

/**
 * 算一行金额（**预览**；服务端会重算一遍并以它为准）。
 *
 * 后端口径（`sales_order_model::line_amounts`）：
 * `amount = round(price × qty, 4)`、`tax_amount = round(amount × tax_rate / 100, 4)`。
 * 非法输入（NaN / 负数 / 非数字）一律按 0 算 —— 表单校验负责报错，预览不抛异常。
 */
export function computeLine(
  qty: number | string | null | undefined,
  price: number | string | null | undefined,
  taxRate: number | string | null | undefined,
): LineAmounts {
  const q = toFinite(qty);
  const p = toFinite(price);
  const rate = toFinite(taxRate);
  if (q <= 0 || p < 0 || rate < 0) {
    return { amount: 0, tax_amount: 0, with_tax: 0 };
  }
  const amount = roundTo(p * q, 4);
  const tax = roundTo((amount * rate) / 100, 4);
  return { amount, tax_amount: tax, with_tax: roundTo(amount + tax, 4) };
}

/** 数字化：空 / 非数字 → 0（后端出参可能是字符串 `"12.0000"`） */
function toFinite(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/** 单头汇总（逐行 4 位定标后相加，与后端 `Totals::of` 同口径） */
export interface OrderTotals {
  /** 不含税合计 */
  amount: number;
  /** 税额合计 */
  tax: number;
  /** 价税合计 */
  with_tax: number;
}

/** 一批明细行的汇总（**预览**；服务端回填的 `total_*` 才作数） */
export function computeTotals(
  lines: ReadonlyArray<{
    qty: number | string | null | undefined;
    price: number | string | null | undefined;
    tax_rate: number | string | null | undefined;
  }>,
): OrderTotals {
  let amount = 0;
  let tax = 0;
  for (const line of lines) {
    const computed = computeLine(line.qty, line.price, line.tax_rate);
    amount += computed.amount;
    tax += computed.tax_amount;
  }
  const amount4 = roundTo(amount, 4);
  const tax4 = roundTo(tax, 4);
  return { amount: amount4, tax: tax4, with_tax: roundTo(amount4 + tax4, 4) };
}

// ==================== 表单 ====================

/** 一行明细草稿（表单态；金额由 {@link computeLine} 现算，不存） */
export interface SalesOrderLineDraft {
  /** 前端行 id（不是后端主键；只用于 `key`） */
  key: string;
  /** 物料 id（`''` = 还没选） */
  material_id: string;
  /**
   * 数量（`number | string` 两种态都收：`t-input-number` 清空时给数字，
   * 用户手输中途是字符串；统一在 {@link computeLine} / {@link toItemReq} 归一）
   */
  qty: number | string;
  /** 不含税单价（同上，两种态都收） */
  price: number | string;
  /** 税率百分数（同上，两种态都收） */
  tax_rate: number | string;
  /**
   * 取价来源（SO-34 自动带价的**结果**，只在前端显示，不提交）。
   *
   * `''` = 还没取价（没选客户/物料，或接口还没回）；其余见 `price_dto::SOURCES`。
   * 后端订单只收 `qty/price/tax_rate`，来源是"这一行为什么是这个价"的审计线索，
   * 不进单据 —— 真要留痕，后端按 `source_id` 能回查到那一行价格配置。
   */
  price_source?: PriceSource | '';
  /**
   * 销售员手动改过价（SO-34）。
   *
   * `true` 时**不再被自动取价覆盖**（改过的价是人工决策，重算冲掉就是"卖亏了没人知道"）；
   * 想看规则价就点行尾「重取价」。展示上打「手动价」标签盖住来源。
   */
  price_manual?: boolean;
}

/** 单头草稿（`customer_id` / `contract_id` / 日期 / 备注） */
export interface SalesOrderFormDraft {
  /** 客户 id（`''` = 还没选） */
  customer_id: string;
  /**
   * 销售合同 id（`''` = 不挂合同，SO-27）。
   *
   * 只放「SALES 类型 + 可用状态」的合同；填了由后端校验乙方 = 本单客户与合同额度。
   */
  contract_id: string;
  /**
   * 销售员 id（`''` = 不指定，SO-35 / ARCH-7 §15.4）。
   *
   * 佣金按「订单的销售员」归属，**留空不等于建单人**：后端不默认，前台也不猜。
   * 填了后端会校验「存在 + 未软删 + 启用」，停用账号 400。
   */
  salesperson_id: string;
  /**
   * 币种（SO-40 / V0314；`''` = 本位币）。
   *
   * 只放 `ACTIVE` 的币种；外币单提交时后端按 `order_date` 取最近 SPOT 汇率并锁进单据行。
   * **汇率永远只读**：`exchange_rate` 是建单那一刻锁定的，前端不给编辑入口。
   */
  currency_code: string;
  /** 下单日期（`yyyy-MM-dd`） */
  order_date: string;
  /** 要求交期（`''` = 未约定） */
  delivery_date: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  items: SalesOrderLineDraft[];
}

let lineSeq = 0;

/** 新行（默认数量 1、税率 13%——与 `biz_material.tax_rate` 默认口径一致，用户可改） */
export function newSalesOrderLine(
  defaults: Partial<Omit<SalesOrderLineDraft, 'key'>> = {},
): SalesOrderLineDraft {
  lineSeq += 1;
  return {
    key: `line-${Date.now()}-${lineSeq}`,
    material_id: '',
    qty: '1',
    price: '',
    tax_rate: '13',
    price_source: '',
    price_manual: false,
    ...defaults,
  };
}

// ==================== 自动带价（SO-34 / ARCH-7 §15.3.3） ====================

/**
 * 取价结果 → 行（自动带价）。
 *
 * 三条口径：
 * 1. **来源总是更新**：即使销售手动改过价，也把"规则命中了哪一级"记下来
 *    （页面用「手动价」标签盖住它展示，但 `重取价` 时要用）；
 * 2. **手动价不被覆盖**：`price_manual = true` 时价格保持人工值 —— 自动重算冲掉
 *    销售特意让的价，是最容易"卖亏了还查不出来"的一类事故；
 * 3. **`NONE` 不清空已填价**：没价是业务事实（让销售手填），把用户手填的数字抹掉更糟。
 *
 * @returns `true` = 价格被自动带出；`false` = 只是记录了来源（手动价 / 无价）
 */
export function applyResolvedPrice(line: SalesOrderLineDraft, res: PriceResolveRes): boolean {
  line.price_source = res.source;
  if (line.price_manual) {
    return false;
  }
  if (res.price === null || res.price === undefined || res.price === '') {
    return false;
  }
  const num = Number(res.price);
  if (!Number.isFinite(num)) {
    return false;
  }
  line.price = String(Number(num.toFixed(4)));
  return true;
}

/**
 * 销售员改价 → 标记「手动价」。
 *
 * 调用点在单价输入框的 `@change`，**且只在该行已有来源（`price_source`）时才标**
 * ——新建行还没取过价时手填一个值是正常流程，不该一上来就顶着「手动价」标签；
 * 一旦有来源，用户改动就是覆盖系统价，必须留痕。
 */
export function markPriceManual(line: SalesOrderLineDraft): void {
  if (line.price_source) {
    line.price_manual = true;
  }
}

/** 行尾「重取价」：清掉手动标记，让下一次取价覆盖价格 */
export function clearManualPrice(line: SalesOrderLineDraft): void {
  line.price_manual = false;
}

/** 行的取价徽标：手动价 > 来源；都没取过价返回 `null`（不渲染标签） */
export function linePriceBadge(line: SalesOrderLineDraft): {
  label: string;
  theme: 'success' | 'primary' | 'default' | 'warning';
} | null {
  if (line.price_manual) {
    return { label: '手动价', theme: 'warning' };
  }
  if (!line.price_source) {
    return null;
  }
  return {
    label: priceSourceLabel(line.price_source),
    theme: priceSourceTheme(line.price_source),
  };
}

/** 行的取价徽标文案（无徽标 → 空串，模板里配合 `v-if="linePriceBadge(row)"`） */
export function linePriceBadgeLabel(line: SalesOrderLineDraft): string {
  return linePriceBadge(line)?.label ?? '';
}

/** 行的取价徽标配色（无徽标 → `default`，TDesign 不会为此报错） */
export function linePriceBadgeTheme(
  line: SalesOrderLineDraft,
): 'success' | 'primary' | 'default' | 'warning' {
  return linePriceBadge(line)?.theme ?? 'default';
}

/** 空表单（新建用；`today` 由调用方给，便于测试与「北京时间今天」口径） */
export function emptySalesOrderForm(today: string): SalesOrderFormDraft {
  return {
    customer_id: '',
    contract_id: '',
    salesperson_id: '',
    // '' = 本位币（后端缺省取账套本位币并锁 rate = 1）；下拉用 currency/options 填
    currency_code: '',
    order_date: today,
    delivery_date: '',
    remark: '',
    items: [newSalesOrderLine()],
  };
}

/**
 * 详情 → 表单（编辑回填）。
 *
 * 金额**不回填**（表单里现算），`delivery_date` / `remark` 的 `null` → `''`
 * （输入框只认字符串）。明细按 `line_no` 升序还原，行 key 重新发。
 */
export function formFromDetail(order: SalesOrder): SalesOrderFormDraft {
  return {
    customer_id: order.customer_id,
    contract_id: order.contract_id ?? '',
    // 销售员可能为 null（SO-35 之前的老单 / 建单时没指定）→ 折成空串，输入框只认字符串
    salesperson_id: order.salesperson_id ?? '',
    // SO-42：老单（V0314 之前）没有 currency_code → 折成空串 = 本位币，编辑时不换币种
    currency_code: order.currency_code ?? '',
    order_date: order.order_date,
    delivery_date: order.delivery_date ?? '',
    remark: order.remark ?? '',
    items: [...order.items]
      .sort((a, b) => a.line_no - b.line_no)
      .map((item: SalesOrderItem) =>
        newSalesOrderLine({
          material_id: item.material_id,
          qty: String(toFinite(item.qty)),
          price: String(toFinite(item.price)),
          tax_rate: String(toFinite(item.tax_rate)),
          // 历史单据没有取价来源（SO-34 之前的单没有这条线索）：留空 = 不显示标签，
          // 编单时也不会被"自动带价"悄悄改价（price_manual=true，人工价优先）
          price_source: '',
          price_manual: true,
        }),
      ),
  };
}

/**
 * 表单校验（提交前的前端闸；服务端还会校一遍，前端只是省一次 400）。
 *
 * 规则逐条对齐后端 `build_order` / `build_item`：
 * 客户必填、日期 `yyyy-MM-dd`、交期不早于下单日、至少 1 行、
 * 每行物料必填 / 数量 > 0 / 单价 ≥ 0 / 税率 0–100。
 *
 * @returns 第一条错误文案（`null` = 通过）
 */
export function validateSalesOrderForm(form: SalesOrderFormDraft): string | null {
  if (!form.customer_id) {
    return '请选择客户（客户必须存在且未冻结）';
  }
  if (!isDateString(form.order_date)) {
    return '下单日期格式应为 yyyy-MM-dd';
  }
  if (form.delivery_date) {
    if (!isDateString(form.delivery_date)) {
      return '要求交期格式应为 yyyy-MM-dd';
    }
    if (form.delivery_date < form.order_date) {
      return '要求交期不能早于下单日期';
    }
  }
  if (form.remark.length > SALES_ORDER_REMARK_MAX) {
    return `备注不能超过 ${SALES_ORDER_REMARK_MAX} 字`;
  }
  if (form.items.length === 0) {
    return '至少要有 1 行明细';
  }
  if (form.items.length > SALES_ORDER_MAX_LINES) {
    return `明细最多 ${SALES_ORDER_MAX_LINES} 行`;
  }
  for (const [index, line] of form.items.entries()) {
    const at = `第 ${index + 1} 行`;
    if (!line.material_id) {
      return `${at}：请选择物料`;
    }
    const qty = toFinite(line.qty);
    if (!Number.isFinite(Number(line.qty)) || Number(line.qty) <= 0) {
      return `${at}：数量必须大于 0`;
    }
    const price = line.price === '' ? 0 : Number(line.price);
    if (!Number.isFinite(price) || price < 0) {
      return `${at}：单价不能为负`;
    }
    const rate = line.tax_rate === '' ? 0 : Number(line.tax_rate);
    if (!Number.isFinite(rate) || rate < 0 || rate > 100) {
      return `${at}：税率必须在 0–100 之间（百分数）`;
    }
    if (qty === 0) {
      return `${at}：数量必须大于 0`;
    }
  }
  return null;
}

/** `yyyy-MM-dd` 且是真日期（`2026-13-01` / `2026-02-30` 都要拒） */
export function isDateString(value: string | null | undefined): boolean {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return false;
  }
  const [year, month, day] = value.split('-').map(Number) as [number, number, number];
  if (month < 1 || month > 12 || day < 1) {
    return false;
  }
  const daysInMonth = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return day <= daysInMonth;
}

/** 表单明细 → 入参明细（金额不发；空单价 / 空税率按 0，与后端 `unwrap_or(ZERO)` 同口径） */
export function toItemReq(line: SalesOrderLineDraft): SalesOrderItemReq {
  const req: SalesOrderItemReq = {
    material_id: line.material_id,
    qty: String(toFinite(line.qty)),
  };
  const price = line.price === '' ? '' : String(toFinite(line.price));
  if (price !== '') {
    req.price = price;
  }
  const rate = line.tax_rate === '' ? '' : String(toFinite(line.tax_rate));
  if (rate !== '') {
    req.tax_rate = rate;
  }
  return req;
}

/** 表单 → 新建入参 */
export function buildCreatePayload(form: SalesOrderFormDraft): CreateSalesOrderReq {
  return {
    customer_id: form.customer_id,
    contract_id: form.contract_id || null,
    // 空串 = 不指定销售员（后端不默认成建单人：佣金归属要人明确指定）
    salesperson_id: form.salesperson_id || null,
    // 空串 = 本位币（后端缺省取账套本位币，rate = 1）；外币由后端按单据日锁定汇率
    currency_code: form.currency_code || null,
    order_date: form.order_date,
    delivery_date: form.delivery_date || null,
    remark: form.remark.trim() || null,
    items: form.items.map(toItemReq),
  };
}

/**
 * 表单 → 编辑入参（**整单覆盖式**：明细全传，后端全量替换）。
 *
 * 交期 / 备注用 `''` 表示清空（后端把 `Some("")` 折成 `None`）。
 */
export function buildUpdatePayload(form: SalesOrderFormDraft): UpdateSalesOrderReq {
  return {
    customer_id: form.customer_id,
    // 空串 = 解绑合同（后端 `parse_contract_id` 把 Some("") 折成 None）；
    // 不能省略：省略表示「不改」，用户就永远解不掉已挂的合同
    contract_id: form.contract_id,
    // 空串 = 解绑销售员（后端把 `Some("")` 折成 None）；不能省略，否则永远解不掉
    salesperson_id: form.salesperson_id,
    // SO-42：不传 = 不改汇率；传了就按（可能改过的）order_date 重新锁定。
    // 空串 = 回到本位币（后端把 Some("") 折成 None → 沿用/本位币），不能省略，
    // 否则用户永远换不回本位币。
    currency_code: form.currency_code,
    order_date: form.order_date,
    delivery_date: form.delivery_date || '',
    remark: form.remark.trim(),
    items: form.items.map(toItemReq),
  };
}

/** 能否再插一行（超过 {@link SALES_ORDER_MAX_LINES} 就别让点了） */
export function canAddLine(form: SalesOrderFormDraft): boolean {
  return form.items.length < SALES_ORDER_MAX_LINES;
}

/** 删一行：只剩一行时**不许删**（后端要求至少 1 行，留着空行让人填比让人重开更省事） */
export function canRemoveLine(form: SalesOrderFormDraft): boolean {
  return form.items.length > 1;
}

/** 列表空态文案（区分「没数据」与「筛出来没有」） */
export function emptyListText(filter: SalesOrderFilter): string {
  return isSalesOrderFiltered(filter)
    ? '当前筛选条件下没有销售订单，试试放宽条件或重置'
    : '还没有销售订单，点右上角「新建销售订单」开单';
}
