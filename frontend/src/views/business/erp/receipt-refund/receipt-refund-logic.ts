/**
 * 收款退款单页的**纯逻辑**（NEAR-FIX-1 / 卡 t_67dcb9cb）。
 *
 * # 为什么判据全部搬到这里
 *
 * 页面上最容易出错的不是渲染，而是「哪个按钮该亮」和「这笔钱能不能这么退」：
 *
 * | 判据 | 写错的后果 |
 * |------|-----------|
 * | `canSubmitApproval` | 未核销满就送审 → 审批通过后 `post_after_approval` 过账被 409 打回 → 单据卡在 CONFIRMED 且「已通过」的实例，会计看不出下一步该干嘛 |
 * | `canWithdraw` | 用 `status` 判「审批中」→ 撤回按钮永远不出现（审批不是状态，见 receipt_refund.rs 模块文档） |
 * | `validateAllocations` | 冲减总额超退款金额 → 后端 400，用户填完一屏才被打回 |
 *
 * 所以口径全部抽成纯函数，`pnpm test:unit` 直接打；`.vue` 里只剩 `v-if` 与 `MessagePlugin`。
 *
 * # 口径来源（逐字抄后端，不猜）
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 状态 4 值 | `receipt_refund::STATUSES` = DRAFT / CONFIRMED / POSTED / CANCELLED |
 * | 合法边 3 条 | `state_machine::RECEIPT_REFUND_EDGES`（DRAFT→CONFIRMED / CONFIRMED→POSTED / DRAFT→CANCELLED）|
 * | 退款原因 ≥ 4 字 | `receipt_refund::REASON_MIN_CHARS`（库层 `ck_biz_receipt_refund_reason` 双闸）|
 * | 过账要求核销满 | `receipt_refund::change_status` 的 POSTED 分支（未核销余额 > 0 → 409）|
 * | 审批不是状态 | 模块文档「审批不是一个状态 … 前端判断已提交待批要读详情里的 approval 段」|
 *
 * # 金额一律走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"100.0000"`）。冲减金额是要**累加和比较**的，
 * 全走 `shared/money.ts` 的 `toCents`，只在显示前用 `formatCents` 换回来。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';

import type { ReceiptRefund, ReceiptRefundDetail } from '@/api/business/erp/receipt-refund';
import type { Receivable } from '@/api/business/erp';

import { toCents } from '../shared/money';

/** 权限码（与 V0295 的 `sys_menu.auth_code` 逐字一致，差一个字 = 按钮点了 403） */
export const REFUND_PERMS = {
  list: 'business:erp:receipt-refund:list',
  detail: 'business:erp:receipt-refund:detail',
  create: 'business:erp:receipt-refund:create',
  allocate: 'business:erp:receipt-refund:allocate',
  status: 'business:erp:receipt-refund:status',
} as const;

/** 退款原因最短长度（`receipt_refund::REASON_MIN_CHARS`） */
export const REFUND_REASON_MIN = 4;
/** 退款原因最长长度（`receipt_refund::REASON_MAX_CHARS`） */
export const REFUND_REASON_MAX = 500;
/** 分页大小（与收款单页同口径） */
export const REFUND_PAGE_SIZE = 20;
/** 冲减候选应收一次最多拉多少（后端 page_size 上限内，够财务用） */
export const RECEIVABLE_FETCH_LIMIT = 200;

/** 状态筛选下拉（顺序 = 单据生命周期顺序，不是字母序） */
export const REFUND_STATUS_OPTIONS = [
  { label: '草稿', value: 'DRAFT' },
  { label: '已确认', value: 'CONFIRMED' },
  { label: '已过账', value: 'POSTED' },
  { label: '已作废', value: 'CANCELLED' },
] as const;

/** 退款方式下拉（`receipt_refund::METHODS` 4 值） */
export const REFUND_METHOD_OPTIONS = [
  { label: '银行转账', value: 'BANK_TRANSFER' },
  { label: '现金', value: 'CASH' },
  { label: '支票', value: 'CHECK' },
  { label: '其它', value: 'OTHER' },
] as const;

/** 列表列定义（`t-table` 的 columns；`PrimaryTableCol` 与收款单页同一套写法） */
export const REFUND_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'refund_no', title: '退款单号', width: 176, fixed: 'left' },
  { colKey: 'source_receipt_no', title: '来源收款单', width: 158 },
  { colKey: 'customer_name', title: '客户', width: 140, ellipsis: true },
  { colKey: 'refund_date', title: '退款日期', width: 106 },
  { colKey: 'amount', title: '退款金额', width: 110, align: 'right' },
  { colKey: 'allocated_amount', title: '已冲减', width: 106, align: 'right' },
  { colKey: 'method_label', title: '退款方式', width: 96 },
  { colKey: 'status', title: '状态', width: 88 },
  { colKey: 'approval_status', title: '审批', width: 92 },
  // 操作列最宽的一档是 CONFIRMED：详情 / 核销 / 提交审批 / 撤回 / 过账 ≈ 240px，
  // 再窄会折行（本仓 TDesign 固定列实测）。
  { colKey: 'op', title: '操作', width: 260, fixed: 'right' },
];

/** 状态 → tag 主题（与收款单页 `receiptStatusTheme` 同一套配色） */
export function refundStatusTheme(status: string | null | undefined): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'CONFIRMED':
      return 'primary';
    case 'POSTED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 审批状态 → tag 主题（`null` = 从未提交，走 default） */
export function approvalTheme(status: string | null | undefined): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'PENDING':
      return 'warning';
    case 'APPROVED':
      return 'success';
    case 'REJECTED':
      return 'danger';
    case 'WITHDRAWN':
      return 'default';
    case 'CANCELLED':
      return 'default';
    default:
      return 'default';
  }
}

/**
 * 审批状态显示文案。
 *
 * **`null` 必须显示「未送审」**，而不是空白 —— 「没提过」和「提了但被撤回」
 * 是两件事，界面上必须能分开（撤回之后可以再提，从没提过也能提）。
 */
export function approvalLabel(status: string | null | undefined): string {
  switch (status) {
    case 'PENDING':
      return '审批中';
    case 'APPROVED':
      return '已通过';
    case 'REJECTED':
      return '已驳回';
    case 'WITHDRAWN':
      return '已撤回';
    case 'CANCELLED':
      return '已终止';
    default:
      return '未送审';
  }
}

/** 列表筛选状态（页面 `reactive` 的形状） */
export interface RefundFilter {
  status: string;
  customerId: string;
  method: string;
  refundNo: string;
  dateFrom: string;
  dateTo: string;
}

/** 空筛选（重置用；每次返回新对象，避免共享引用被改） */
export function emptyRefundFilter(): RefundFilter {
  return { status: '', customerId: '', method: '', refundNo: '', dateFrom: '', dateTo: '' };
}

/** 筛选 → 分页入参（空串交给 `asParams` 兜住，但这里也主动剔掉，便于单测断言） */
export function buildRefundQuery(filter: RefundFilter, page: number, pageSize = REFUND_PAGE_SIZE) {
  const query: Record<string, string | number> = { page, page_size: pageSize };
  if (filter.status.trim()) {
    query.status = filter.status.trim();
  }
  if (filter.customerId.trim()) {
    query.customer_id = filter.customerId.trim();
  }
  if (filter.method.trim()) {
    query.method = filter.method.trim();
  }
  if (filter.refundNo.trim()) {
    query.refund_no = filter.refundNo.trim();
  }
  if (filter.dateFrom.trim()) {
    query.date_from = filter.dateFrom.trim();
  }
  if (filter.dateTo.trim()) {
    query.date_to = filter.dateTo.trim();
  }
  return query;
}

/** 是否有任何筛选生效（列表右上角「已筛选」标记） */
export function isRefundFiltered(filter: RefundFilter): boolean {
  return Boolean(
    filter.status.trim() ||
      filter.customerId.trim() ||
      filter.method.trim() ||
      filter.refundNo.trim() ||
      filter.dateFrom.trim() ||
      filter.dateTo.trim(),
  );
}

/**
 * 退款原因校验（建单必填、作废必填，均 ≥ 4 字）。
 *
 * 按**字符数**算（`Array.from` 拆代理对），与后端 `chars().count()` 同口径 ——
 * 用 `String.length` 的话一个 emoji 会被算成 2，前后端口径就不一致了。
 */
export function validateRefundReason(raw: string): string | null {
  const text = (raw ?? '').trim();
  const count = Array.from(text).length;
  if (count === 0) {
    return '退款原因必填：退款是钱往外走，没有依据无法追溯';
  }
  if (count < REFUND_REASON_MIN) {
    return `退款原因至少 ${REFUND_REASON_MIN} 个字（例如「客户退货」「重复收款」），当前 ${count} 个字`;
  }
  if (count > REFUND_REASON_MAX) {
    return `退款原因最多 ${REFUND_REASON_MAX} 个字，当前 ${count} 个字`;
  }
  return null;
}

/**
 * 单张应收当前的**可退金额（分）**。
 *
 * 后端口径（`receipt_refund::validate_allocate_amount` 的注释）：
 * `可退 = 该应收 received_amount + 本退款此前已冲减它的金额`。
 * 已冲减的部分已经从 `received_amount` 里扣掉了，不先加回来会误拒「改金额」。
 *
 * @param receivedCents 应收当前的 `received_amount`（分）
 * @param alreadyCents  本退款单此前对这一张应收已冲减的金额（分；新单传 0）
 */
export function refundableCents(receivedCents: number, alreadyCents = 0): number {
  const total = receivedCents + alreadyCents;
  return total > 0 ? total : 0;
}

/** 一张待冲减的候选行（核销面板的 `rows`） */
export interface AllocationRow {
  receivableId: string;
  arNo: string;
  arAmountCents: number;
  receivedCents: number;
  /** 本退款单此前已冲减（编辑既有单时才有） */
  alreadyCents: number;
  /** 本次填的金额（元，`t-input-number` 的 v-model 直接绑） */
  inputAmount: number | null;
  status: string;
}

/**
 * 应收分页行 → 冲减候选行。
 *
 * 只保留「**收过钱**」的应收（`received_amount > 0` 或本单已冲减过）：
 * 一分没收的应收没有可退的钱，列出来只会让人误填。
 */
export function buildAllocationRows(
  receivables: readonly Receivable[],
  priorAllocations: readonly { receivable_id: string; amount: number | string }[] = [],
): AllocationRow[] {
  const priorMap = new Map<string, number>();
  for (const item of priorAllocations) {
    priorMap.set(item.receivable_id, toCents(item.amount));
  }
  const rows: AllocationRow[] = [];
  for (const ar of receivables) {
    const receivedCents = toCents(ar.received_amount);
    const alreadyCents = priorMap.get(ar.id) ?? 0;
    if (receivedCents <= 0 && alreadyCents <= 0) {
      continue;
    }
    rows.push({
      receivableId: ar.id,
      arNo: ar.ar_no,
      arAmountCents: toCents(ar.amount),
      receivedCents,
      alreadyCents,
      inputAmount: alreadyCents > 0 ? alreadyCents / 100 : null,
      status: ar.status,
    });
  }
  // 先冲最早的账是财务的默认动作（与收款单核销面板同口径：按单号升序 ≈ 按挂账先后）
  rows.sort((a, b) => a.arNo.localeCompare(b.arNo));
  return rows;
}

/** 冲减面板的校验结果 */
export interface AllocationCheck {
  /** 合计（分） */
  totalCents: number;
  /** 有金额的行数 */
  filledCount: number;
  /** 第一条错误（null = 通过） */
  error: string | null;
}

/**
 * 冲减面板校验。
 *
 * 三条硬口径（与后端 `validate_allocate_amount` / `validate_allocate_total` 一一对应）：
 * ① 单行金额 > 0 且 ≤ 该应收可退金额（`received + 本单已冲减`）；
 * ② 合计 ≤ 退款金额（`receipt_refund::validate_allocate_total`）；
 * ③ 至少要填一行（`allocate` 的 items 不能为空）。
 *
 * 为什么前端也要拦一遍：后端是最后一道闸，但用户在面板里填完一屏再吃 400
 * 体验很差，且错误文案只在接口响应里出现一次。
 */
export function validateAllocations(
  rows: readonly AllocationRow[],
  refundAmountCents: number,
): AllocationCheck {
  let totalCents = 0;
  let filledCount = 0;
  for (const row of rows) {
    const cents = row.inputAmount === null ? 0 : Math.round(Math.abs(row.inputAmount) * 100);
    if (cents === 0) {
      continue;
    }
    if (cents < 0) {
      return { totalCents, filledCount, error: `应收 ${row.arNo} 的冲减金额不能为负` };
    }
    filledCount += 1;
    const cap = refundableCents(row.receivedCents, row.alreadyCents);
    if (cents > cap) {
      return {
        totalCents,
        filledCount,
        error: `应收 ${row.arNo} 本次冲减 ${(cents / 100).toFixed(2)} 超过可退 ${(cap / 100).toFixed(2)}（该应收已收 + 本单此前已冲减）`,
      };
    }
    totalCents += cents;
  }
  if (filledCount === 0) {
    return { totalCents, filledCount, error: '至少填一张应收的冲减金额（退款必须冲已收到的钱）' };
  }
  if (totalCents > refundAmountCents) {
    return {
      totalCents,
      filledCount,
      error: `冲减合计 ${(totalCents / 100).toFixed(2)} 超过退款金额 ${(refundAmountCents / 100).toFixed(2)}`,
    };
  }
  return { totalCents, filledCount, error: null };
}

/** 冲减面板 → 接口入参（只发有金额的行） */
export function buildAllocationPayload(
  rows: readonly AllocationRow[],
): { receivable_id: string; amount: number }[] {
  return rows
    .filter((row) => row.inputAmount !== null && Number(row.inputAmount) !== 0)
    .map((row) => ({
      receivable_id: row.receivableId,
      amount: Number((Number(row.inputAmount)).toFixed(2)),
    }));
}

/** 未冲减余额（分） */
export function unallocatedCents(detail: ReceiptRefund | null | undefined): number {
  if (!detail) {
    return 0;
  }
  return toCents(detail.amount) - toCents(detail.allocated_amount);
}

/** 草稿可作废：只有 DRAFT 有 CANCELLED 这条边，且必须一分钱都没冲减（库层 CHECK 兜底） */
export function canCancel(row: ReceiptRefund | null | undefined): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'DRAFT' && toCents(row.allocated_amount) === 0;
}

/** 作废按钮的阻塞原因（null = 可点） */
export function cancelBlockedReason(row: ReceiptRefund | null | undefined): string | null {
  if (!row) {
    return null;
  }
  if (row.status !== 'DRAFT') {
    return row.status === 'POSTED'
      ? '已过账：钱已付出且凭证已记账，退款单不能作废（要抹掉只能由客户重新回一张收款单，或在凭证层红冲）'
      : '只有草稿能作废（状态图里没有「已确认 → 已作废」这条边）';
  }
  if (toCents(row.allocated_amount) > 0) {
    return '已发生冲减（应收已被冲减），不能作废：先把冲减改回 0 或整单重登';
  }
  return null;
}

/** 可确认：DRAFT → CONFIRMED */
export function canConfirm(row: ReceiptRefund | null | undefined): boolean {
  return Boolean(row && row.status === 'DRAFT');
}

/** 可过账：CONFIRMED 且核销满（`allocated === amount`） */
export function canPost(row: ReceiptRefund | null | undefined): boolean {
  return Boolean(row && row.status === 'CONFIRMED' && unallocatedCents(row) === 0);
}

/** 过账按钮的阻塞原因（null = 可点） */
export function postBlockedReason(row: ReceiptRefund | null | undefined): string | null {
  if (!row) {
    return null;
  }
  if (row.status !== 'CONFIRMED') {
    return row.status === 'DRAFT'
      ? '草稿不能直接过账：先「确认」，再核销到应收'
      : null;
  }
  if (unallocatedCents(row) > 0) {
    return `还有未冲减余额 ${(unallocatedCents(row) / 100).toFixed(2)}：凭证要「借 1122 应收 / 贷 1002 银行」各等于退款金额，未冲减的部分借记 1122 就是凭空多记一笔应收 —— 先核销满（或作废重登）`;
  }
  return null;
}

/**
 * 可提交审批：`CONFIRMED` + 核销满 + 当前没有在跑的审批实例。
 *
 * **「已在审批中」只能看 `approval_status === 'PENDING'`**：审批不是一个单据状态
 * （`status` 仍是 CONFIRMED），用 status 判会写出「审批中还能再提一次」的漏洞。
 */
export function canSubmitApproval(row: ReceiptRefund | null | undefined): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'CONFIRMED' && unallocatedCents(row) === 0 && row.approval_status !== 'PENDING';
}

/** 提交审批按钮的阻塞原因（null = 可点） */
export function submitBlockedReason(row: ReceiptRefund | null | undefined): string | null {
  if (!row) {
    return null;
  }
  if (row.status === 'DRAFT') {
    return '草稿不能送审：先「确认」（DRAFT → CONFIRMED），再核销到应收';
  }
  if (row.status === 'POSTED') {
    return '已过账（终态），无需再送审';
  }
  if (row.status === 'CANCELLED') {
    return '已作废的单据不能再送审';
  }
  if (unallocatedCents(row) > 0) {
    return `还有未冲减余额 ${(unallocatedCents(row) / 100).toFixed(2)}：审批通过后会直接过账，未核销满必然 409，先把核销做满`;
  }
  if (row.approval_status === 'PENDING') {
    return `已在审批中（${row.approval_instance_no || '审批单号缺失'}）：等审批人处理，或先「撤回」再改`;
  }
  return null;
}

/**
 * 可撤回审批：最近一条实例是 `PENDING` 才有得撤。
 *
 * 撤回走流程引擎（`POST /business/flow/instance/{id}/withdraw`），
 * 只允许发起人且**零人已审**，否则 409 + 65023 —— 前端不预判「是不是发起人」
 * （当前用户 id 的比较会漏掉代提场景），交给后端判，前端只负责把按钮放对位置。
 */
export function canWithdraw(row: ReceiptRefund | null | undefined): boolean {
  return Boolean(row && row.approval_status === 'PENDING' && row.approval_instance_id);
}

/** 退款单是否还能编辑核销（DRAFT / CONFIRMED 都能改，过账后锁死） */
export function isAllocatable(row: ReceiptRefund | null | undefined): boolean {
  if (!row) {
    return false;
  }
  if (row.status !== 'DRAFT' && row.status !== 'CONFIRMED') {
    return false;
  }
  // 审批在跑的期间不许改核销（钱数变了审批批的就不是这笔了）
  return row.approval_status !== 'PENDING';
}

/**
 * 新建弹窗的金额上限校验。
 *
 * @param amountCents 本次退款金额（分）
 * @param capCents    来源收款单的「可退上限」（分）；`null` = 未选收款单 / 还不知道
 * @returns 错误文案（null = 通过）
 *
 * 为什么「未选来源收款单」时不拦：后端 `create` 不要求来源收款单
 * （`source_receipt_id` 可空），没有上限可算；真正的上限在 `allocate` 那一步
 * （逐张应收的可退金额）由后端把关。
 */
export function validateRefundAmount(amountCents: number, capCents: number | null): string | null {
  if (!Number.isFinite(amountCents) || amountCents <= 0) {
    return '退款金额必须大于 0（零元 / 负数退款没有业务含义）';
  }
  if (capCents !== null && amountCents > capCents) {
    return `退款金额 ${(amountCents / 100).toFixed(2)} 超过该收款单可退金额 ${(capCents / 100).toFixed(2)}（只能退已经收到的钱）`;
  }
  return null;
}

/**
 * 来源收款单的可退上限（分）= 该收款单核销过的应收里，`received_amount` 之和。
 *
 * 注意这是**粗上限**：真正的可退金额按「每一张应收」单独算（`refundableCents`），
 * 因为多笔收款可能核销到同一张应收，逐张可退之和会小于（或等于）这里。
 * 这里只用于新建弹窗的即时提示，避免用户填一个明显超额的数。
 *
 * @param allocations 收款单的核销明细（`ReceiptDetail.allocations`）
 * @param receivedByReceivable receivable_id → 当前 received_amount（分）
 */
export function receiptRefundableCap(
  allocations: readonly { receivable_id: string }[],
  receivedByReceivable: ReadonlyMap<string, number>,
): number {
  let total = 0;
  const seen = new Set<string>();
  for (const item of allocations) {
    if (seen.has(item.receivable_id)) {
      continue;
    }
    seen.add(item.receivable_id);
    total += receivedByReceivable.get(item.receivable_id) ?? 0;
  }
  return total;
}

/** 详情抽屉里「审批进度」要不要渲染（有实例 id 才拉节点，与 shared/approval-progress 同口径） */
export function hasApprovalInstance(detail: ReceiptRefundDetail | null | undefined): boolean {
  return Boolean(detail?.approval?.instance_id || detail?.approval_instance_id);
}
