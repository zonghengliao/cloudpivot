/**
 * 销售退货单页纯逻辑（SO-31）—— 所有可测的判断都在这里，`.vue` 只负责渲染。
 *
 * # 字段口径
 *
 * 出参 / 入参逐字抄 `backend/src/business/erp/sales_return_handler.rs`
 * （`ReturnRes` / `ReturnItemRes` / `ReturnableItemRes` / `CreateReturnReq` /
 * `ReturnItemReq` / `ReturnStatusReq`），前端类型在 `@/api/business/erp/sales-return`。
 *
 * **别照发货单抄**：发货单是 4 态、没有审批段；退货单是 6 态且**有审批段**
 * （`SUBMITTED → APPROVED`），「提交」和「批准」是两个动作、两份权限判断。
 *
 * # 状态机（唯一真相：`state_machine::SALES_RETURN_EDGES`，7 条边）
 *
 * ```text
 * DRAFT ──提交──▶ SUBMITTED ──批准──▶ APPROVED ──▶ PENDING_POST ──过账──▶ POSTED
 *   │                │                    │
 *   └────────────────┴────────────────────┴──作废──▶ CANCELLED
 * ```
 *
 * - `POSTED` 是终态：过账真动账（回库 + 红字入库凭证 + 冲减应收 + 已开票造红字发票），
 *   **不可撤销**，要退回只能再开一张反向单据；
 * - 只有 `SUBMITTED → CANCELLED` / `APPROVED → CANCELLED` 要 `reason`
 *   （`SALES_RETURN_REASON_EDGES`，后端 400）；`DRAFT → CANCELLED` 不要。
 * - `PENDING_POST → CANCELLED` **不存在**（边表没有）：卡在待过账要作废，
 *   只能让有权限的人过账后退货，别再拼一条假前端边。
 *
 * # 金额
 *
 * 与服务端 `sales_return_model::line_amounts` **同一个公式**：
 * `amount = round(price × qty, 4)`、`tax = round(amount × rate / 100, 4)`。
 * 前端只在弹窗里做预估展示，落库值一律以后端返回的 `total_*` 为准。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  ReturnableItem,
  ReturnStatus,
  ReturnType,
  SalesReturn,
} from '@/api/business/erp/sales-return';

/** 每页条数（与销售订单 / 发货单列表同口径；后端上限 200、`0` → 20） */
export const RETURN_PAGE_SIZE = 20;

/** 备注上限（`V0303` 的 `remark` 是 `VARCHAR(500)`） */
export const RETURN_REMARK_MAX = 500;

/** 明细行数上限（表上没有硬限制，纯防手滑） */
export const RETURN_MAX_LINES = 200;

/** 状态中文名（与后端 `sales_return_model::status_label` 逐字一致；后端也给，这是兜底） */
export const RETURN_STATUS_LABELS: Record<ReturnStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '已提交',
  APPROVED: '已批准',
  PENDING_POST: '待过账',
  POSTED: '已过账',
  // ⚠️ 后端 `status_label` 对 CANCELLED 的中文是「已取消」（不是「已作废」）：
  // 动作叫「作废」、状态叫「已取消」，徽标一律显示后端给的 `status_label`，
  // 这张兜底表也必须逐字对齐，否则同一张单在两个页面上叫两个名字。
  CANCELLED: '已取消',
};

/** 状态标签配色：草稿灰、已提交蓝、已批准青、待过账橙、已过账绿、已取消红 */
export const RETURN_STATUS_THEMES: Record<
  ReturnStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  SUBMITTED: 'primary',
  APPROVED: 'success',
  PENDING_POST: 'warning',
  POSTED: 'success',
  CANCELLED: 'danger',
};

/** 状态域（后端顺序：主链 5 态 + 作废） */
export const RETURN_STATUSES: readonly ReturnStatus[] = [
  'DRAFT',
  'SUBMITTED',
  'APPROVED',
  'PENDING_POST',
  'POSTED',
  'CANCELLED',
];

/** 主链（详情里的状态线用；`CANCELLED` 不在主链上，单独用徽标表达） */
export const RETURN_MAIN_CHAIN: ReadonlyArray<{ label: string; value: ReturnStatus }> = [
  { label: '草稿', value: 'DRAFT' },
  { label: '已提交', value: 'SUBMITTED' },
  { label: '已批准', value: 'APPROVED' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
];

/** 状态下拉候选（顺序 = 状态机走向；`''` = 全部） */
export const RETURN_STATUS_OPTIONS: ReadonlyArray<{
  label: string;
  value: '' | ReturnStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿', value: 'DRAFT' },
  { label: '已提交', value: 'SUBMITTED' },
  { label: '已批准', value: 'APPROVED' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
  { label: '已取消', value: 'CANCELLED' },
];

/** 退货类型中文名（与后端 `return_type_label` 逐字一致） */
export const RETURN_TYPE_LABELS: Record<ReturnType, string> = {
  QUALITY: '质量',
  WRONG_ITEM: '错发',
  OTHER: '其他',
};

/** 退货类型域（后端 `RETURN_TYPES` 顺序） */
export const RETURN_TYPES: readonly ReturnType[] = ['QUALITY', 'WRONG_ITEM', 'OTHER'];

/** 退货类型下拉（建单必填） */
export const RETURN_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: ReturnType }> =
  RETURN_TYPES.map((value) => ({ label: RETURN_TYPE_LABELS[value], value }));

/**
 * 合法边表（逐条抄 `state_machine::SALES_RETURN_EDGES`，顺序也一致）。
 *
 * 前端的按钮显隐 / 目标态判断**只读这一份**，不写第二套状态机。
 * `tests/unit/erp-sales-return.test.ts` 直接读后端源码逐条比对。
 */
export const RETURN_EDGES: ReadonlyArray<readonly [ReturnStatus, ReturnStatus]> = [
  ['DRAFT', 'SUBMITTED'],
  ['SUBMITTED', 'APPROVED'],
  ['APPROVED', 'PENDING_POST'],
  ['PENDING_POST', 'POSTED'],
  ['DRAFT', 'CANCELLED'],
  ['SUBMITTED', 'CANCELLED'],
  ['APPROVED', 'CANCELLED'],
];

/** 需要原因的状态边（逐条抄 `SALES_RETURN_REASON_EDGES`） */
export const RETURN_REASON_EDGES: ReadonlyArray<readonly [ReturnStatus, ReturnStatus]> = [
  ['SUBMITTED', 'CANCELLED'],
  ['APPROVED', 'CANCELLED'],
];

// ==================== 状态判定 ====================

/** 状态中文名：认不出编码时回落编码本身（别显示空白） */
export function returnStatusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return RETURN_STATUS_LABELS[status as ReturnStatus] ?? status;
}

/** 状态标签配色：未知状态回落 `default`（不猜颜色） */
export function returnStatusTheme(
  status: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!status) {
    return 'default';
  }
  return RETURN_STATUS_THEMES[status as ReturnStatus] ?? 'default';
}

/** 退货类型中文名：认不出编码时回落编码本身 */
export function returnTypeLabel(
  type: string | null | undefined,
  backendLabel?: string | null,
): string {
  if (backendLabel) {
    return backendLabel;
  }
  if (!type) {
    return '—';
  }
  return RETURN_TYPE_LABELS[type as ReturnType] ?? type;
}

/** 这条迁移在不在边表里（后端 `model::can_transition` 同口径） */
export function canTransition(
  from: string | null | undefined,
  to: string | null | undefined,
): boolean {
  if (!from || !to) {
    return false;
  }
  return RETURN_EDGES.some(([edgeFrom, edgeTo]) => edgeFrom === from && edgeTo === to);
}

/** 从当前态出发的合法目标态（后端 `allowed_targets` 同口径，顺序按边表） */
export function allowedTargets(status: string | null | undefined): ReturnStatus[] {
  if (!status) {
    return [];
  }
  return RETURN_EDGES.filter(([from]) => from === status).map(([, to]) => to);
}

/** 提交按钮：只有 `DRAFT → SUBMITTED` */
export function canSubmit(status: string | null | undefined): boolean {
  return status === 'DRAFT';
}

/** 批准按钮：只有 `SUBMITTED → APPROVED`（销售主管动作） */
export function canApprove(status: string | null | undefined): boolean {
  return status === 'SUBMITTED';
}

/** 进入待过账按钮：只有 `APPROVED → PENDING_POST` */
export function canEnterPendingPost(status: string | null | undefined): boolean {
  return status === 'APPROVED';
}

/**
 * **过账**按钮：只有 `PENDING_POST → POSTED`。
 *
 * `DRAFT` / `APPROVED` 都不能直接过账 —— 边表里没有那些边，前端一步步走，
 * 不做「提交并过账」的假动作。
 */
export function canPost(status: string | null | undefined): boolean {
  return status === 'PENDING_POST';
}

/** 作废按钮：`{DRAFT, SUBMITTED, APPROVED} → CANCELLED`（后两条必须写原因） */
export function canCancel(status: string | null | undefined): boolean {
  return status === 'DRAFT' || status === 'SUBMITTED' || status === 'APPROVED';
}

/** 终态（已过账 / 已取消）：出了就不再动 */
export function isTerminal(status: string | null | undefined): boolean {
  return status === 'POSTED' || status === 'CANCELLED';
}

/** 这条边要不要原因（与 `SALES_RETURN_REASON_EDGES` 逐条一致） */
export function statusNeedsReason(
  from: string | null | undefined,
  to: ReturnStatus | null | undefined,
): boolean {
  if (!from || !to) {
    return false;
  }
  return RETURN_REASON_EDGES.some(([edgeFrom, edgeTo]) => edgeFrom === from && edgeTo === to);
}

/** 列表行 / 详情操作条的显隐（一次算清，模板里不再写条件） */
export function rowActions(row: {
  status: string | null | undefined;
}): {
  submit: boolean;
  approve: boolean;
  pendingPost: boolean;
  post: boolean;
  cancel: boolean;
} {
  const status = row.status;
  return {
    submit: canSubmit(status),
    approve: canApprove(status),
    pendingPost: canEnterPendingPost(status),
    post: canPost(status),
    cancel: canCancel(status),
  };
}

/**
 * 主链上「已经走到」的步骤（详情状态线用）。
 *
 * 作废单只点亮草稿一步：作废不是主链上的前进，而是从某个中间态跳出。
 */
export function mainChainReached(
  status: string | null | undefined,
  step: ReturnStatus,
): boolean {
  if (!status) {
    return false;
  }
  if (status === 'CANCELLED') {
    return step === 'DRAFT';
  }
  const order = RETURN_MAIN_CHAIN.map((item) => item.value as string);
  return order.indexOf(step) <= order.indexOf(status);
}

// ==================== 列表查询 ====================

/**
 * 列表筛选（`ReturnFilter`：`return_no` 模糊；`status` / `customer_id` /
 * `shipment_id` 精确）。
 */
export interface ReturnFilterState {
  /** 单号（模糊） */
  return_no: string;
  /** 状态（`''` = 全部） */
  status: '' | ReturnStatus;
  /** 客户 id（`''` = 全部） */
  customer_id: string;
}

/** 空筛选（每次重置都造一份新的，别共享引用） */
export function emptyReturnFilter(): ReturnFilterState {
  return { return_no: '', status: '', customer_id: '' };
}

/** 是否处于筛选态（重置按钮 / 「已筛选」提示用） */
export function isReturnFiltered(filter: ReturnFilterState): boolean {
  return Boolean(filter.return_no.trim()) || Boolean(filter.status) || Boolean(filter.customer_id);
}

/** 筛选 + 分页 → 请求参数（空串根本不发，见 `sales-return.ts::asParams`） */
export function buildReturnQuery(
  filter: ReturnFilterState,
  page: number,
  pageSize: number = RETURN_PAGE_SIZE,
): {
  return_no?: string;
  status?: ReturnStatus;
  customer_id?: string;
  page: number;
  page_size: number;
} {
  const query: {
    return_no?: string;
    status?: ReturnStatus;
    customer_id?: string;
    page: number;
    page_size: number;
  } = { page, page_size: pageSize };
  const returnNo = filter.return_no.trim();
  if (returnNo) {
    query.return_no = returnNo;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.customer_id) {
    query.customer_id = filter.customer_id;
  }
  return query;
}

/**
 * 列表列：单号 / 客户 / 退货日期 / 类型 / 金额 / 状态 / 原发货单 / 操作。
 *
 * 卡面点名的六列（`return_no` / 客户 / 退货日期 / 类型 / 金额 / 状态）在前六位，
 * 另加「原发货单」（退货的来源，「从发货单生成」的闭环靠它自证）与操作列。
 * 宽按「1600 视口、侧栏展开时 ≈1288px」预算：合计 1210，留余量，
 * 固定「操作」列不会盖住「状态」列。
 */
export const RETURN_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'return_no', title: '退货单号', width: 175, fixed: 'left' },
  { colKey: 'customer_name', title: '客户', width: 170, ellipsis: true },
  { colKey: 'return_date', title: '退货日期', width: 105 },
  { colKey: 'return_type', title: '类型', width: 80 },
  { colKey: 'total_with_tax', title: '金额(价税合计)', width: 130, align: 'right' },
  { colKey: 'status', title: '状态', width: 95 },
  { colKey: 'shipment_no', title: '原发货单', width: 165 },
  { colKey: 'op', title: '操作', width: 210, fixed: 'right' },
];

/** 详情明细列（行号 / 物料 / 数量 / 单价 / 金额 / 税率 / 税额 / 库位 / 备注） */
export const RETURN_ITEM_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行号', width: 64 },
  { colKey: 'material_id', title: '物料', minWidth: 200, ellipsis: true },
  { colKey: 'qty', title: '退货数量', width: 100, align: 'right' },
  { colKey: 'price', title: '单价', width: 100, align: 'right' },
  { colKey: 'amount', title: '金额', width: 110, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 80, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 100, align: 'right' },
  { colKey: 'location_id', title: '退回库位', width: 130 },
  { colKey: 'remark', title: '备注', minWidth: 110, ellipsis: true },
];

/** 新建弹窗的明细列（多两列「已发 / 可退」，便于核对超退） */
export const RETURN_FORM_ITEM_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '订单行', width: 74 },
  { colKey: 'material_id', title: '物料', minWidth: 210, ellipsis: true },
  { colKey: 'shipped', title: '已发', width: 90, align: 'right' },
  { colKey: 'returned', title: '已退', width: 90, align: 'right' },
  { colKey: 'remaining', title: '可退', width: 90, align: 'right' },
  { colKey: 'qty', title: '本次退货', width: 140 },
  { colKey: 'location_id', title: '退回库位', width: 160 },
  { colKey: 'line_op', title: '操作', width: 70 },
];

// ==================== 从发货单带出明细（表单） ====================

/** 一行「从发货单带出」的明细草稿 */
export interface ReturnLineDraft {
  /** 前端行 id（不是后端主键；只用于 `key`） */
  key: string;
  /** 销售订单行 id（提交时必填） */
  so_item_id: string;
  /** 物料 id（只用于显示，提交不发） */
  material_id: string;
  /** 订单行号 */
  line_no: number;
  /** 已发货量 */
  shipped: string;
  /** 已退量 */
  returned: string;
  /** 可退量（= 本次可退上限） */
  remaining: string;
  /** 单价（不含税，取自订单行） */
  price: string;
  /** 税率百分数 */
  tax_rate: string;
  /** 本次退货数量（`number | string` 两态都收） */
  qty: number | string;
  /** 退回库位（`''` = 整仓） */
  location_id: string;
  /** 行备注 */
  remark: string;
}

let lineSeq = 0;

/** 数字/字符串 → 4 位定标的字符串（后端 `numeric(18,4)` 口径；非法 → `'0'`） */
export function qtyText(value: number | string | null | undefined): string {
  const num = toFinite(value);
  const rounded = Math.round(num * 10000) / 10000;
  return String(rounded);
}

function toFinite(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * 可退明细 → 表单行（**只带 `remaining > 0` 的行**，数量默认 `remaining`）。
 *
 * 与后端「`items` 留空 = 从发货单带出全部可退明细」同一个取数口径
 * （`sales_return_handler::resolve_items` 的带出分支），所以「自动带出」与
 * 「前端显式带出后提交」在建单结果上等价。
 */
export function linesFromReturnable(items: ReadonlyArray<ReturnableItem>): ReturnLineDraft[] {
  return items
    .filter((item) => toFinite(item.remaining) > 0)
    .slice()
    .sort((a, b) => a.line_no - b.line_no)
    .map((item) => {
      lineSeq += 1;
      const remaining = qtyText(item.remaining);
      return {
        key: `line-${Date.now()}-${lineSeq}`,
        so_item_id: item.so_item_id,
        material_id: item.material_id,
        line_no: item.line_no,
        shipped: qtyText(item.shipped),
        returned: qtyText(item.returned),
        remaining,
        price: qtyText(item.price),
        tax_rate: qtyText(item.tax_rate),
        qty: remaining,
        location_id: '',
        remark: '',
      };
    });
}

/** 一行金额拆分（与服务端 `line_amounts` 同公式：金额 = round(price×qty,4)，税 = round(金额×税率/100,4)） */
export function lineAmounts(
  qty: number | string,
  price: number | string,
  taxRate: number | string,
): { amount: number; tax: number } {
  const amount = Math.round(toFinite(qty) * toFinite(price) * 10000) / 10000;
  const tax = Math.round(amount * (toFinite(taxRate) / 100) * 10000) / 10000;
  return { amount, tax };
}

/** 表单预估合计（不含税 / 税额 / 价税合计；落库值以后端返回为准） */
export function totalsOf(lines: ReadonlyArray<ReturnLineDraft>): {
  amount: number;
  tax: number;
  withTax: number;
} {
  let amount = 0;
  let tax = 0;
  for (const line of lines) {
    const split = lineAmounts(line.qty, line.price, line.tax_rate);
    amount += split.amount;
    tax += split.tax;
  }
  amount = Math.round(amount * 10000) / 10000;
  tax = Math.round(tax * 10000) / 10000;
  return { amount, tax, withTax: Math.round((amount + tax) * 10000) / 10000 };
}

/** 新建退货单的表单态 */
export interface ReturnFormDraft {
  /** 原发货单 id（`''` = 还没选） */
  shipment_id: string;
  /** 退货类型（必填；`''` = 还没选） */
  return_type: '' | ReturnType;
  /** 退回入库仓库 id（`''` = 还没选） */
  warehouse_id: string;
  /** 退货日期（`yyyy-MM-dd`） */
  return_date: string;
  /** 备注 */
  remark: string;
  /** 明细（选发货单后从 `returnable` 带出） */
  items: ReturnLineDraft[];
}

/** 空表单（`today` 由调用方给，便于测试与「北京时间今天」口径） */
export function emptyReturnForm(today: string): ReturnFormDraft {
  return {
    shipment_id: '',
    return_type: '',
    warehouse_id: '',
    return_date: today,
    remark: '',
    items: [],
  };
}

/**
 * 表单校验（提交前的前端闸；服务端还会校一遍，前端只是省一次 400）。
 *
 * 规则逐条对齐后端 `create_return` / `resolve_items`：
 * 发货单必选、仓库必选、类型必选、日期 `yyyy-MM-dd`、至少 1 行、
 * 每行数量 > 0 且 ≤ 可退量。
 */
export function validateReturnForm(form: ReturnFormDraft): string | null {
  if (!form.shipment_id) {
    return '请选择原发货单（只有已过账的发货单有可退明细）';
  }
  if (!form.return_type) {
    return '请选择退货类型（质量 / 错发 / 其他）';
  }
  if (!form.warehouse_id) {
    return '请选择退回入库仓库';
  }
  if (!isDateString(form.return_date)) {
    return '退货日期格式应为 yyyy-MM-dd';
  }
  if (form.remark.length > RETURN_REMARK_MAX) {
    return `备注不能超过 ${RETURN_REMARK_MAX} 字`;
  }
  if (form.items.length === 0) {
    return '这张发货单没有可退的明细（可退量都是 0，或已被其他未作废的退货单占满）';
  }
  if (form.items.length > RETURN_MAX_LINES) {
    return `明细最多 ${RETURN_MAX_LINES} 行`;
  }
  for (const [index, line] of form.items.entries()) {
    const at = `第 ${index + 1} 行（订单行 ${line.line_no}）`;
    const qty = toFinite(line.qty);
    if (!Number.isFinite(Number(line.qty)) || qty <= 0) {
      return `${at}：退货数量必须大于 0`;
    }
    const remaining = toFinite(line.remaining);
    if (qty > remaining) {
      return `${at}：退货数量 ${qty} 超过可退量 ${remaining}（超退后端会 400）`;
    }
  }
  if (totalsOf(form.items).withTax < 0) {
    return '退货金额不能为负';
  }
  return null;
}

/**
 * 表单 → 建单入参。
 *
 * - 发货单必带（服务端据此带出客户 / 订单并校验一致）；
 * - `qty` 显式发（用户可能改小 = 部分退）；`price` / `tax_rate` **不发**，
 *   由服务端从订单行取（前端改价会与订单口径打架）；
 * - 物料不发（有 `so_item_id` 时服务端忽略它）。
 */
export function buildCreatePayload(form: ReturnFormDraft): {
  shipment_id: string;
  warehouse_id: string;
  return_type: ReturnType;
  return_date: string;
  remark?: string;
  items: Array<{ so_item_id: string; qty?: string; location_id?: string; remark?: string }>;
} {
  const payload: {
    shipment_id: string;
    warehouse_id: string;
    return_type: ReturnType;
    return_date: string;
    remark?: string;
    items: Array<{ so_item_id: string; qty?: string; location_id?: string; remark?: string }>;
  } = {
    shipment_id: form.shipment_id,
    warehouse_id: form.warehouse_id,
    // 校验已挡住空值；这里给 `OTHER` 只是让类型收敛（服务端仍会校）
    return_type: (form.return_type || 'OTHER') as ReturnType,
    return_date: form.return_date,
    items: [],
  };
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  for (const line of form.items) {
    const item: { so_item_id: string; qty?: string; location_id?: string; remark?: string } = {
      so_item_id: line.so_item_id,
    };
    if (line.qty !== '' && line.qty !== null && line.qty !== undefined) {
      item.qty = qtyText(line.qty);
    }
    if (line.location_id) {
      item.location_id = line.location_id;
    }
    const lineRemark = line.remark.trim();
    if (lineRemark) {
      item.remark = lineRemark;
    }
    payload.items.push(item);
  }
  return payload;
}

/** 表单里是否有「改过数量」的行（提交按钮提示用：全默认 = 整单全退） */
export function isFullReturn(form: ReturnFormDraft): boolean {
  return form.items.every((line) => toFinite(line.qty) === toFinite(line.remaining));
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

/** 列表空态文案（区分「没数据」与「筛出来没有」） */
export function emptyListText(filter: ReturnFilterState): string {
  return isReturnFiltered(filter)
    ? '当前筛选条件下没有退货单，试试放宽条件或重置'
    : '还没有退货单，点右上角「从发货单新建」带出可退明细';
}

// ==================== 详情 / 过账文案 ====================

/**
 * 过账确认弹窗的正文（**只陈述后端真实效果**，见 `sales_return_posting::post_in_tx`）。
 *
 * 五件真实发生的事：生成退货入库单（`in_type=RETURN`）、三本账回库、红字入库凭证
 * （借 1405 / 贷 6401）、冲减原应收（部分退减额 / 全额退且一分没收 → 应收作废）、
 * 已开票则造红字发票 + 收入冲减凭证；外加回写订单已发量与释放信用占用。
 */
export function postDialogText(
  record: Pick<SalesReturn, 'return_no' | 'total_with_tax'> | null,
): string {
  const no = record?.return_no ?? '本单';
  const amount = record ? moneyText(record.total_with_tax) : '';
  return (
    `过账 ${no}（价税合计 ${amount}）会在同一事务里：` +
    '① 生成退货入库单并按原出库成本回库三本账；② 生成红字入库凭证（借 1405 / 贷 6401）；' +
    '③ 冲减原应收（全额退且一分没收 → 该应收作废；退货额 > 未收会 409 并点名先做收款退款单）；' +
    '④ 已开票的自动造红字发票 + 收入冲减凭证。' +
    '过账不可撤销（要退回只能再开一张反向单据），确认继续？'
  );
}

/** 过账后详情里三张回链单的显示文案（未过账 / 已删除分开说） */
export function postingLinks(record: {
  status: string | null | undefined;
  stock_in_id: string | null;
  receivable_id: string | null;
  red_invoice_id: string | null;
}): { stockIn: string; receivable: string; redInvoice: string } {
  const posted = record.status === 'POSTED';
  return {
    stockIn: record.stock_in_id ? '已生成' : posted ? '入库单已删除' : '未过账',
    receivable: record.receivable_id ? '已冲减' : posted ? '未关联应收' : '未过账',
    redInvoice: record.red_invoice_id
      ? '已生成'
      : posted
        ? '未开票（无红票）'
        : '未过账',
  };
}

/** 金额显示：`1921.0000` → `1,921.00`（两位小数；非法 → `0.00`） */
export function moneyText(value: number | string | null | undefined): string {
  const num = toFinite(value);
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}
