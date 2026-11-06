/**
 * 收款单页纯逻辑（SO-18）—— 所有能测的判定都在这里，`index.vue` 只负责渲染。
 *
 * # 口径逐字抄后端，不猜
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 状态域 4 值 | `receipt::STATUSES` / `ck_biz_receipt_status`（V0292） |
 * | 状态中文 | `receipt::status_label`（前端只做兜底） |
 * | 收款方式 4 值 | `receipt::METHODS` / `ck_biz_receipt_method` |
 * | 方式中文 | `receipt::method_label`（`None` = 「未指定」） |
 * | 状态机边 | `state_machine::RECEIPT_EDGES`（3 条）经 `receipt::LEGAL_EDGES` 导出 |
 * | 作废原因长度 | `receipt::REASON_MIN_CHARS` = 4 / `REASON_MAX_CHARS` = 500 |
 * | 可核销状态 | `receipt::is_allocatable` = `DRAFT` \| `CONFIRMED` |
 * | 过账前置 | `change_status`：`allocated_amount == amount` 才允许 `POSTED`（否则 409） |
 * | 核销上限 | `allocate`：单张 ≤ 应收可核销余额，合计 ≤ 收款金额 |
 *
 * # 为什么状态域要在这里再钉一次
 *
 * 收款单的 `DRAFT/CONFIRMED/POSTED/CANCELLED` 与应收的 `OPEN/PARTIAL/SETTLED/...`
 * 是**两套值**。把应收的值传给收款接口会 400（`ReceiptPageQuery::validate` 白名单），
 * 反过来也一样。两端在 TS 里都只是 `string`，抄错了 `vue-tsc` 抓不到 ——
 * 所以 `RECEIPT_STATUSES` 与 `tests/unit/erp-receipt.test.ts` 一起把这条钉住。
 *
 * # 金额为什么一律走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"1000.0000"`），前端 `+` 会变成字符串拼接。
 * 核销面板的「实时剩余」是**会算错钱**的地方，累加 / 比较全部过 `toCents`，
 * 只在显示前换回来（见 `views/business/erp/shared/money.ts`）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  Receipt,
  ReceiptAllocation,
  ReceiptMethod,
  ReceiptQuery,
  ReceiptStatus,
} from '@/api/business/erp';
import { formatCents, toCents, toNumber } from '../shared/money';

/** 每页条数（与销售订单 / 发货单 / 应收台账同口径；后端上限 200） */
export const RECEIPT_PAGE_SIZE = 20;

/** 核销面板一次拉多少张未收应收（种子规模够用；后端上限 200） */
export const ALLOCATION_FETCH_LIMIT = 200;

/** 作废原因长度域（`receipt::REASON_MIN_CHARS` / `REASON_MAX_CHARS`，按**字符**数） */
export const RECEIPT_REASON_MIN = 4;
export const RECEIPT_REASON_MAX = 500;

/** 收款的 5 个权限码（`V0293` 种的 `sys_menu.auth_code`，逐字一致） */
export const RECEIPT_PERMS = {
  list: 'business:erp:receipt:list',
  detail: 'business:erp:receipt:detail',
  create: 'business:erp:receipt:create',
  allocate: 'business:erp:receipt:allocate',
  status: 'business:erp:receipt:status',
} as const;

/** 状态域（`receipt::STATUSES`，顺序与后端一致） */
export const RECEIPT_STATUSES: ReceiptStatus[] = ['DRAFT', 'CONFIRMED', 'POSTED', 'CANCELLED'];

/**
 * 状态机边（`state_machine::RECEIPT_EDGES`）。
 *
 * 只有三条，**故意**没有 `CONFIRMED→CANCELLED` 与 `POSTED→*`：
 * 确认过的收款要作废，得先反核销、走红冲单，不能一步抹掉。
 */
export const RECEIPT_EDGES: [ReceiptStatus, ReceiptStatus][] = [
  ['DRAFT', 'CONFIRMED'],
  ['CONFIRMED', 'POSTED'],
  ['DRAFT', 'CANCELLED'],
];

/** 需要原因才能走的边（`RECEIPT_REASON_EDGES`） */
export const RECEIPT_REASON_EDGES: [ReceiptStatus, ReceiptStatus][] = [['DRAFT', 'CANCELLED']];

/** 入账态（`RECEIPT_LOCKED`）：金额与核销被凭证锁死 */
export const RECEIPT_LOCKED: ReceiptStatus[] = ['POSTED', 'CANCELLED'];

/** 状态中文兜底（正常用后端 `status_label`；`status_label` 缺失时回落这里） */
export const RECEIPT_STATUS_LABELS: Record<string, string> = {
  DRAFT: '草稿',
  CONFIRMED: '已确认',
  POSTED: '已过账',
  CANCELLED: '已作废',
};

/** 状态配色：草稿灰蓝、已确认蓝、已过账绿、已作废灰 */
export const RECEIPT_STATUS_THEMES: Record<
  string,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  CONFIRMED: 'primary',
  POSTED: 'success',
  CANCELLED: 'default',
};

/** 状态下拉（值 = 后端域值，标签 = 后端中文） */
export const RECEIPT_STATUS_OPTIONS: { label: string; value: ReceiptStatus }[] = RECEIPT_STATUSES.map(
  (status) => ({ label: RECEIPT_STATUS_LABELS[status] ?? status, value: status }),
);

/** 收款方式域（`receipt::METHODS`） */
export const RECEIPT_METHODS: ReceiptMethod[] = ['BANK_TRANSFER', 'CASH', 'CHECK', 'OTHER'];

/** 收款方式中文（`receipt::method_label`；空 = 未指定） */
export const RECEIPT_METHOD_LABELS: Record<string, string> = {
  BANK_TRANSFER: '银行转账',
  CASH: '现金',
  CHECK: '支票',
  OTHER: '其它',
};

/** 方式下拉（不提供「空」项：`clearable` 自己给空态，空 = 未指定） */
export const RECEIPT_METHOD_OPTIONS: { label: string; value: ReceiptMethod }[] =
  RECEIPT_METHODS.map((method) => ({
    label: RECEIPT_METHOD_LABELS[method] ?? method,
    value: method,
  }));

/**
 * 列表列（order 决定列序；金额列右对齐、窄列定宽）。
 *
 * 宽度合计 **1081px**，与应收台账（`RECEIVABLE_COLUMNS`，1081px）同尺度：1600 视口、侧栏展开时
 * `t-table__content` 实测约 1288px，放得下；1280 视口（968px）会横向滚动，
 * 但 `op` 是 `fixed: 'right'`、`receipt_no` 是 `fixed: 'left'`，被盖住的只会是中间的
 * 「收款方式 / 状态」——所以这两列**不能再宽**，单号也不能折行（20 位 mono）。
 */
export const RECEIPT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'receipt_no', title: '收款单号', width: 156, fixed: 'left' },
  { colKey: 'customer_name', title: '客户', width: 130, ellipsis: true },
  { colKey: 'receipt_date', title: '收款日期', width: 106 },
  { colKey: 'amount', title: '收款金额', width: 105, align: 'right' },
  { colKey: 'allocated_amount', title: '已核销', width: 100, align: 'right' },
  { colKey: 'unallocated_amount', title: '未核销', width: 100, align: 'right' },
  { colKey: 'method', title: '收款方式', width: 96 },
  { colKey: 'status', title: '状态', width: 88 },
  // 操作列最宽的一档是 DRAFT：详情 / 核销 / 确认 / 作废 四个 text 按钮 ≈ 200px，
  // 再窄会折成「查/看」两行（本仓 TDesign 固定列实测）。所以 220 而不是 200。
  { colKey: 'op', title: '操作', width: 220, fixed: 'right' },
];

/** 页面筛选状态（v-model 形状；提交前由 {@link buildReceiptQuery} 收敛） */
export interface ReceiptFilterState {
  status: ReceiptStatus | '';
  customerId: string;
  receiptNo: string;
  method: ReceiptMethod | '';
  /** 收款日期下界 `yyyy-MM-dd`（含） */
  dateFrom: string;
  /** 收款日期上界 `yyyy-MM-dd`（含） */
  dateTo: string;
}

/** 空筛选（首次进入 / 重置） */
export function emptyReceiptFilter(): ReceiptFilterState {
  return { status: '', customerId: '', receiptNo: '', method: '', dateFrom: '', dateTo: '' };
}

/** 是否带了任何筛选（决定列表头上的「已筛选」徽标） */
export function isReceiptFiltered(filter: ReceiptFilterState): boolean {
  return Boolean(
    filter.status ||
      filter.customerId ||
      filter.receiptNo.trim() ||
      filter.method ||
      filter.dateFrom ||
      filter.dateTo,
  );
}

/**
 * 筛选 → 请求参数。
 *
 * **空串一律不发**：后端把 `?status=` 当「等于空串」过滤（查 0 行），
 * `?customer_id=` 会被当成非法 UUID 直接 400（`ReceiptPageQuery::validate`）。
 * `receipt_no` 是模糊匹配单号**或客户名**（`receipt_page` 的 `LIKE` 两列）。
 */
export function buildReceiptQuery(
  filter: ReceiptFilterState,
  page: number,
  pageSize: number = RECEIPT_PAGE_SIZE,
): ReceiptQuery {
  const query: ReceiptQuery = { page: Math.max(1, Math.trunc(page) || 1), page_size: pageSize };
  if (filter.status) query.status = filter.status;
  if (filter.customerId) query.customer_id = filter.customerId;
  const receiptNo = filter.receiptNo.trim();
  if (receiptNo) query.receipt_no = receiptNo;
  if (filter.method) query.method = filter.method;
  if (filter.dateFrom) query.date_from = filter.dateFrom;
  if (filter.dateTo) query.date_to = filter.dateTo;
  return query;
}

/** 状态中文：后端 `status_label` 优先，没给才回落本地表，最后回落编码 */
export function receiptStatusLabel(row: Pick<Receipt, 'status' | 'status_label'>): string {
  return row.status_label || RECEIPT_STATUS_LABELS[row.status] || row.status || '—';
}

/** 状态配色（未知状态给灰，不炸） */
export function receiptStatusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  return RECEIPT_STATUS_THEMES[status] ?? 'default';
}

/** 收款方式中文：后端 `method_label` 优先（空 = 未指定） */
export function receiptMethodLabel(row: Pick<Receipt, 'method' | 'method_label'>): string {
  if (row.method_label) return row.method_label;
  if (!row.method) return '未指定';
  return RECEIPT_METHOD_LABELS[row.method] ?? row.method;
}

/** 是否可核销（`receipt::is_allocatable`：DRAFT / CONFIRMED） */
export function isAllocatable(status: string): boolean {
  return status === 'DRAFT' || status === 'CONFIRMED';
}

/** 状态机允许的去向（前端只做**显隐**；真正的判定在后端 `LEGAL_EDGES`） */
export function allowedTargets(from: string): ReceiptStatus[] {
  return RECEIPT_EDGES.filter(([source]) => source === from).map(([, target]) => target);
}

/** 该边是否要原因（`RECEIPT_REASON_EDGES`） */
export function needsReason(from: string, to: string): boolean {
  return RECEIPT_REASON_EDGES.some(([source, target]) => source === from && target === to);
}

/** 能否确认（`DRAFT → CONFIRMED`） */
export function canConfirm(row: Pick<Receipt, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/**
 * 能否作废（`DRAFT → CANCELLED`）。
 *
 * 已核销的收款**不能**直接作废（后端 409）：先反核销再作废。
 */
export function canCancel(row: Pick<Receipt, 'status' | 'allocated_amount'>): boolean {
  return row.status === 'DRAFT' && toCents(row.allocated_amount) === 0;
}

/** 作废按钮禁用原因（空串 = 可用，给 tooltip 用） */
export function cancelBlockedReason(row: Pick<Receipt, 'status' | 'allocated_amount'>): string {
  if (row.status === 'CONFIRMED') {
    return '已确认的收款不直接作废：先反核销，走红冲单（状态图没有 CONFIRMED→CANCELLED 这条边）';
  }
  if (row.status === 'POSTED') {
    return '已过账的收款已生成凭证，不能作废（要冲销请走红冲）';
  }
  if (row.status === 'CANCELLED') {
    return '已经是终态';
  }
  if (toCents(row.allocated_amount) !== 0) {
    return '这张收款已核销到应收上：先反核销，再作废（否则应收会被凭空还原）';
  }
  return '';
}

/**
 * 能否过账（`CONFIRMED → POSTED`）。
 *
 * 后端的硬前置是「核销满」：`allocated_amount == amount`，否则 409。
 * 前端的按钮禁用**只为少跑一趟**，判定口径与后端逐字一致。
 */
export function canPost(row: Pick<Receipt, 'status' | 'amount' | 'allocated_amount'>): boolean {
  return row.status === 'CONFIRMED' && toCents(row.allocated_amount) === toCents(row.amount);
}

/** 过账按钮禁用原因（空串 = 可用） */
export function postBlockedReason(
  row: Pick<Receipt, 'status' | 'amount' | 'allocated_amount'>,
): string {
  if (row.status === 'DRAFT') {
    return '先确认（DRAFT→CONFIRMED）再过账';
  }
  if (row.status === 'POSTED') {
    return '已过账';
  }
  if (row.status === 'CANCELLED') {
    return '已作废的收款不能过账';
  }
  const remaining = toCents(row.amount) - toCents(row.allocated_amount);
  if (remaining !== 0) {
    return `还有 ${formatCents(remaining)} 未核销到应收：凭证只有「借 1002 / 贷 1122」两行，未核销余额贷记 1122 等于凭空冲减应收（后端会 409）`;
  }
  return '';
}

/** 已核销进度（0–100，整数百分比；金额为 0 时给 0 不除零） */
export function allocatedPercent(row: Pick<Receipt, 'amount' | 'allocated_amount'>): number {
  const amount = toCents(row.amount);
  if (amount <= 0) return 0;
  const allocated = toCents(row.allocated_amount);
  return Math.min(100, Math.max(0, Math.round((allocated / amount) * 100)));
}

/** 未核销金额（整数分） */
export function unallocatedCents(row: Pick<Receipt, 'amount' | 'allocated_amount'>): number {
  return toCents(row.amount) - toCents(row.allocated_amount);
}

// ==================== 核销面板 ====================

/**
 * 核销面板的一行。
 *
 * `allocated` 是**本次**（这张收款）对该应收的核销金额（整数分）。
 * `capCents` 是「可核销余额」= 应收未收 + 本收款此前已核过它的金额
 * —— 后端 `allocate` 的算法逐字一致：本收款之前核的那部分已经计入
 * `received_amount`，不先加回来会把「改大金额」误判成超额。
 */
export interface AllocationRowState {
  receivable_id: string;
  ar_no: string;
  customer_name: string;
  due_date: string;
  /** 应收金额（字符串，后端 Decimal） */
  amount: string | number;
  /** 已收金额 */
  received_amount: string | number;
  /** 未收金额 */
  unsettled_amount: string | number;
  status: string;
  status_label: string;
  /** 本收款此前已核过它的金额（详情 `allocations[]` 回填，整数分） */
  priorCents: number;
  /** 本次填写金额（整数分，0 = 不核销） */
  allocated: number;
  /** 是否勾选 */
  checked: boolean;
}

/** 一张应收的最小入参（`Receivable` 的子集，避免单测造整行） */
export interface AllocatableReceivable {
  id: string;
  ar_no: string;
  customer_name: string;
  due_date: string;
  amount: string | number;
  received_amount: string | number;
  unsettled_amount: string | number;
  status: string;
  status_label: string;
}

/**
 * 可核销余额（整数分）= 未收 + 本收款此前已核过它的金额。
 *
 * 与后端 `allocate` 的 `cap` 逐字一致；`cap < 0` 视为 0（脏数据不放开超额）。
 */
export function allocatableCents(row: Pick<AllocationRowState, 'unsettled_amount' | 'priorCents'>): number {
  const cents = toCents(row.unsettled_amount) + row.priorCents;
  return cents > 0 ? cents : 0;
}

/** 面板行：应收列表 + 详情已有核销明细 → 可编辑状态 */
export function buildAllocationRows(
  receivables: AllocatableReceivable[],
  existing: ReceiptAllocation[] = [],
): AllocationRowState[] {
  const priorById = new Map<string, number>();
  for (const item of existing) {
    priorById.set(item.receivable_id, toCents(item.amount));
  }
  const seen = new Set<string>();
  const rows: AllocationRowState[] = [];
  for (const item of receivables) {
    if (seen.has(item.id)) continue;
    seen.add(item.id);
    const priorCents = priorById.get(item.id) ?? 0;
    rows.push({
      receivable_id: item.id,
      ar_no: item.ar_no,
      customer_name: item.customer_name,
      due_date: item.due_date,
      amount: item.amount,
      received_amount: item.received_amount,
      unsettled_amount: item.unsettled_amount,
      status: item.status,
      status_label: item.status_label,
      priorCents,
      allocated: priorCents,
      checked: priorCents > 0,
    });
  }
  // 详情里有、但「未收应收」查询没带回来的（例如已结清的）：也要留在面板上，
  // 否则保存时那一行会被静默丢掉（用户在面板上看不到，却改了别行金额）。
  for (const item of existing) {
    if (seen.has(item.receivable_id)) continue;
    seen.add(item.receivable_id);
    const priorCents = toCents(item.amount);
    rows.push({
      receivable_id: item.receivable_id,
      ar_no: item.ar_no,
      customer_name: '',
      due_date: '',
      amount: item.ar_amount,
      received_amount: item.ar_received_amount,
      unsettled_amount: item.ar_unsettled_amount,
      status: item.ar_status,
      status_label: item.ar_status_label,
      priorCents,
      allocated: priorCents,
      checked: priorCents > 0,
    });
  }
  return rows;
}

/** 本次核销合计（整数分）= 勾选行的金额之和 */
export function sumAllocated(rows: AllocationRowState[]): number {
  return rows.reduce((sum, row) => (row.checked ? sum + row.allocated : sum), 0);
}

/** 核销后这张收款还剩多少（整数分，可负 = 超额） */
export function remainingAfterAllocation(amount: string | number, rows: AllocationRowState[]): number {
  return toCents(amount) - sumAllocated(rows);
}

/** 单行剩余（整数分）= 可核销余额 − 本次填写 */
export function rowRemainingCents(row: AllocationRowState): number {
  return allocatableCents(row) - row.allocated;
}

/**
 * 已核销行「不可撤销」校验。
 *
 * 后端 `allocate` 只做 UPSERT（`ON CONFLICT (receipt_id, receivable_id) DO UPDATE SET amount`），
 * **不删不在本次 items 里的行**；而单行金额必须 `> 0`（0 会被 400 拒）。
 * 所以「已核销过、这次取消勾选」在前端是**做不到**的：后端会静默保留原行，
 * 头上的 `allocated_amount` 仍含它 —— 用户以为释放了额度，实际没释放，还会让
 * 「合计 ≤ 收款金额」的判定和面板上的数字对不上。
 *
 * 反核销接口（`DELETE /receipt/{id}/allocate/{receivable_id}`）SO-17 未开放，
 * 所以这里**直接把口子堵住**：已经有核销金额的应收，至少保留原来的金额。
 */
export function priorAllocationError(rows: AllocationRowState[]): string | null {
  for (const row of rows) {
    if (row.priorCents <= 0) continue;
    if (!row.checked) {
      return `「${row.ar_no}」已经核销过 ${formatCents(row.priorCents)}：本版本没有反核销接口，不能取消勾选（要冲回请走红冲）`;
    }
    if (row.allocated < row.priorCents) {
      return `「${row.ar_no}」已核销 ${formatCents(row.priorCents)}，本次不能减少到 ${formatCents(row.allocated)}（没有反核销接口，只能改大或保持不变）`;
    }
  }
  return null;
}

/**
 * 面板校验。返回 `null` = 可提交，否则返回**说人话**的第一条错误。
 *
 * 与后端 `allocate` 的四道校验一一对应（顺序也照后端：空 / 非正 → 单张超额 → 合计超额）：
 * 1. 至少一行、金额 > 0；
 * 2. 单张 ≤ 可核销余额；
 * 3. 合计 ≤ 收款金额；
 * 4. 未勾选但填了金额 → 视为「漏勾」，报错而不是静默丢掉；
 * 5. 已核销行不能取消 / 调小（后端不删行，见 {@link priorAllocationError}）。
 */
export function validateAllocation(amount: string | number, rows: AllocationRowState[]): string | null {
  const priorError = priorAllocationError(rows);
  if (priorError) return priorError;
  const filled = rows.filter((row) => row.allocated > 0);
  const dirtyUnchecked = filled.filter((row) => !row.checked);
  if (dirtyUnchecked.length > 0) {
    return `「${dirtyUnchecked[0]?.ar_no ?? ''}」填了金额但没勾选：勾上它，或把金额清成 0`;
  }
  const checked = rows.filter((row) => row.checked);
  if (checked.length === 0) {
    return '至少勾选一张应收';
  }
  for (const row of checked) {
    if (row.allocated <= 0) {
      return `「${row.ar_no}」的核销金额必须大于 0`;
    }
    const cap = allocatableCents(row);
    if (row.allocated > cap) {
      return `本次核销 ${formatCents(row.allocated)} 超过应收 ${row.ar_no} 的可核销余额 ${formatCents(cap)}`;
    }
  }
  const total = sumAllocated(rows);
  if (total > toCents(amount)) {
    return `核销总额 ${formatCents(total)} 超过收款金额 ${formatCents(toCents(amount))}`;
  }
  return null;
}

/**
 * 面板「有变化」的行（`allocated !== priorCents`）。
 *
 * 为什么要跳过没改金额的行：后端 `allocate` 会对**每一条** `items` 先锁应收再校验
 * `receivable::has_balance(&status)`，而 `SETTLED` 是终态 → 409。上一笔核销已把某张应收
 * 核满的场合，面板上那行（`priorCents = 本次金额`）如果照原样再提交一次，就会被
 * 「已收清不能核销」拦下 —— 但库里那一行本来就已经是对的，完全没必要再发。
 * 同理也不发 `allocated === priorCents` 的行：UPSERT 结果一样，只会多跑一趟（还多一次 409）。
 *
 * 后端 `allocate` 的合计校验（`untouched + total`）把「不在本次 items 里的既有核销」
 * 也算进来了，所以跳过未变行不会让「合计 ≤ 收款金额」失真。
 */
export function changedAllocationRows(rows: AllocationRowState[]): AllocationRowState[] {
  return rows.filter((row) => row.checked && row.allocated > 0 && row.allocated !== row.priorCents);
}

/** 面板是否真有改动（没有改动不该发请求：后端会 400「items 不能为空」） */
export function hasAllocationChanges(rows: AllocationRowState[]): boolean {
  return changedAllocationRows(rows).length > 0;
}

/** 面板 → 请求明细（只带勾选、有金额、**且金额变了**的行；见 {@link changedAllocationRows}） */
export function buildAllocateItems(rows: AllocationRowState[]): { receivable_id: string; amount: string }[] {
  return changedAllocationRows(rows)
    .map((row) => ({
      receivable_id: row.receivable_id,
      // 后端 `Decimal` 收字符串：用「元.4 位小数」的定点形式，避免 0.1 之类的二进制尾巴
      amount: (row.allocated / 100).toFixed(4),
    }));
}

/**
 * 一键「按收款金额顺序填满」：从上到下（面板已按到期日排序）把剩余额度填到各应收。
 *
 * 给「财务拿到一笔钱，先冲最早的账」这个最常见动作一个按钮，省得逐行敲数字。
 */
export function fillSequentially(
  amount: string | number,
  rows: AllocationRowState[],
): AllocationRowState[] {
  let left = toCents(amount);
  return rows.map((row) => {
    const cap = allocatableCents(row);
    const take = Math.max(0, Math.min(cap, left));
    left -= take;
    return { ...row, allocated: take, checked: take > 0 };
  });
}

/** 面板合计的三个数字（给底部统计条） */
export function allocationSummary(
  amount: string | number,
  rows: AllocationRowState[],
): { receiptCents: number; allocatedCents: number; remainingCents: number; checkedCount: number } {
  const receiptCents = toCents(amount);
  const allocatedCents = sumAllocated(rows);
  return {
    receiptCents,
    allocatedCents,
    remainingCents: receiptCents - allocatedCents,
    checkedCount: rows.filter((row) => row.checked && row.allocated > 0).length,
  };
}

/** 作废原因是否合法（按字符数，与后端 `validate_reason` 同口径） */
export function receiptReasonError(reason: string): string {
  const text = reason.trim();
  if (text.length < RECEIPT_REASON_MIN) {
    return `原因至少 ${RECEIPT_REASON_MIN} 个字（说明为什么作废这张收款单）`;
  }
  if (text.length > RECEIPT_REASON_MAX) {
    return `原因最多 ${RECEIPT_REASON_MAX} 个字，当前 ${text.length} 个字`;
  }
  return '';
}

/**
 * 登记表单的本地校验（只为少跑一趟；后端逐条再校验一次）。
 * 返回空串 = 通过。
 */
export function createFormError(form: {
  customer_id: string;
  amount: string | number;
  receipt_date?: string | null;
}): string {
  if (!form.customer_id) {
    return '请选择客户';
  }
  const cents = toCents(form.amount);
  if (cents <= 0) {
    return '收款金额必须大于 0';
  }
  if (toNumber(form.amount) < 0) {
    return '收款金额必须大于 0';
  }
  if (form.receipt_date && !/^\d{4}-\d{2}-\d{2}$/.test(form.receipt_date)) {
    return '收款日期形状必须是 yyyy-MM-dd';
  }
  return '';
}

/** 应收状态是否还能被核销（`OPEN` / `PARTIAL`；终态后端 409） */
export function receivableAllocatable(status: string): boolean {
  return status === 'OPEN' || status === 'PARTIAL';
}

/** 应收状态中文兜底（后端也有 `status_label`，这里只兜空） */
export const RECEIVABLE_STATUS_LABELS_FALLBACK: Record<string, string> = {
  OPEN: '未收',
  PARTIAL: '部分已收',
  SETTLED: '已收清',
  WRITTEN_OFF: '坏账核销',
  CANCELLED: '已作废',
};

/**
 * 面板数据源：把两次分页（`status=OPEN` + `status=PARTIAL`）合成「该客户所有未收应收」。
 *
 * 为什么发两次而不是一次：`GET /receivable/page` 的 `status` 只收**单值**（`ReceivableQuery`），
 * 「未收」在后端是两种状态（`receivable::has_balance`），只查 `OPEN` 会漏掉收了一半的单子。
 * 为什么还要在本地再筛一遍：两页各取 200 条后可能重叠（并发收款把 OPEN 推成 PARTIAL），
 * 且 `unsettled_amount === 0` 的脏数据不该出现在面板上。
 *
 * 排序按 **到期日升序**（同日按单号）—— 财务拿到一笔钱的默认动作是「先冲最早的账」，
 * {@link fillSequentially} 依赖这个顺序。
 */
export function mergeAllocatableReceivables(
  rows: AllocatableReceivable[],
): AllocatableReceivable[] {
  const seen = new Set<string>();
  const out: AllocatableReceivable[] = [];
  for (const row of rows) {
    if (!row || !row.id || seen.has(row.id)) continue;
    seen.add(row.id);
    if (!receivableAllocatable(row.status)) continue;
    if (toCents(row.unsettled_amount) <= 0) continue;
    out.push(row);
  }
  return out.sort((a, b) => {
    const dueA = a.due_date || '9999-12-31';
    const dueB = b.due_date || '9999-12-31';
    if (dueA !== dueB) return dueA < dueB ? -1 : 1;
    return a.ar_no < b.ar_no ? -1 : a.ar_no > b.ar_no ? 1 : 0;
  });
}

/** 面板行的应收状态中文 */
export function allocationArStatusLabel(row: Pick<AllocationRowState, 'status' | 'status_label'>): string {
  return (
    row.status_label ||
    RECEIVABLE_STATUS_LABELS_FALLBACK[row.status] ||
    row.status ||
    '—'
  );
}
