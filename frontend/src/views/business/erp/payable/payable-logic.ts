/**
 * 应付单页纯逻辑（P2-E2）—— 状态映射 / 筛选参数 / 到期日提示。
 *
 * # 为什么单独一个 `.ts`
 *
 * 用例跑在没有 DOM 的 node 里（`tsx --test`），`.vue` 的 `setup()` 跑不起来。
 * 而「OPEN 是橙还是蓝」「到期日过了要不要标红」「空串到底发不发」这类地方
 * 漂移了，`vue-tsc` 抓不到、e2e 也只看得出「界面长什么样」——
 * 所以全部搬到这里，`.vue` 只负责渲染与发请求。
 *
 * # 状态色（卡面 §三 的硬要求）
 *
 * ```text
 * OPEN         未付      橙 ⇒ TDesign warning
 * PARTIAL_PAID 部分付款  蓝 ⇒ TDesign primary
 * SETTLED      已结清    绿 ⇒ TDesign success
 * CANCELLED    已作废    灰 ⇒ TDesign default
 * ```
 *
 * 颜色只用 TDesign 的语义主题（`<t-tag :theme>`），不写任何裸 hex —— 22 套配色
 * （`pnpm check:palettes`）靠主题变量自己适配明暗。
 */
import type { PayableRow, PayableStatus } from '@/api/business/erp';
import { dash, shortId, toCents, toNumber, todayString } from '../shared/money';

/** 每页条数（与其它 ERP 列表页同口径） */
export const PAYABLE_PAGE_SIZE = 20;

/** TDesign tag 主题（本仓只允许这五种 + `light` 变体，这里只用基础五种） */
export type TagTheme = 'default' | 'primary' | 'success' | 'warning' | 'danger';

/**
 * 「作废审批中」状态字面量（P-FLOW-3B）。
 *
 * 后端 `payable::STATUS_PENDING_CANCEL`；V0191 把它补进了状态机（
 * `OPEN → PENDING_CANCEL → CANCELLED / OPEN`）并加了库约束
 * `ck_biz_payable_pending_cancel`。抽成常量是因为它在 6 处出现
 * （文案 / 配色 / 筛选 / 两个按钮判据 / 付款闸），散写字面量必然漂移一处。
 */
export const PAYABLE_STATUS_PENDING_CANCEL = 'PENDING_CANCEL';

/** 状态 → 中文（与 `payable.rs` 的 `STATUS_*` / `payable_query.rs::status_label` 逐字对应） */
export function payableStatusLabel(status: PayableStatus | string): string {
  switch (status) {
    case 'OPEN':
      return '未付';
    case 'PARTIAL_PAID':
      return '部分付款';
    case 'SETTLED':
      return '已结清';
    case 'CANCELLED':
      return '已作废';
    case PAYABLE_STATUS_PENDING_CANCEL:
      // P-FLOW-3B：与后端 `payable_query::status_label` 的「作废审批中」逐字一致
      return '作废审批中';
    default:
      return String(status);
  }
}

/** 状态 → tag 主题（卡面 §三：橙 / 蓝 / 绿 / 灰；作废审批中用橙 = 「还没定」而不是「已作废」） */
export function payableStatusTheme(status: PayableStatus | string): TagTheme {
  switch (status) {
    case 'OPEN':
      return 'warning';
    case 'PARTIAL_PAID':
      return 'primary';
    case 'SETTLED':
      return 'success';
    case PAYABLE_STATUS_PENDING_CANCEL:
      return 'warning';
    case 'CANCELLED':
    default:
      return 'default';
  }
}

/**
 * 状态下拉候选（`''` = 全部，排第一；`t-select` 把空串当未选，所以 `''` 只是「整键缺席」）。
 *
 * `PENDING_CANCEL` 必须能被筛出来：财务每天要盯「有哪些单卡在作废审批里」，
 * 后端 `payable_page` 直接按 `p.status = ?` 过滤，没有额外口径。
 */
export const PAYABLE_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: '' | PayableStatus }> = [
  { label: '全部状态', value: '' },
  { label: '未付', value: 'OPEN' },
  { label: '部分付款', value: 'PARTIAL_PAID' },
  { label: '已结清', value: 'SETTLED' },
  { label: '作废审批中', value: PAYABLE_STATUS_PENDING_CANCEL },
  { label: '已作废', value: 'CANCELLED' },
];

/** 筛选行 */
export interface PayableFilterState {
  /** 状态（`''` = 全部） */
  status: PayableStatus | '';
  /** 供应商 id（`''` = 全部） */
  supplier_id: string;
  /** 应付单号（后端模糊 `LIKE %x%`） */
  payable_no: string;
  /** 到期日起（可空） */
  due_date_from: string;
  /** 到期日止（可空） */
  due_date_to: string;
}

/** 空筛选 */
export function emptyPayableFilter(): PayableFilterState {
  return { status: '', supplier_id: '', payable_no: '', due_date_from: '', due_date_to: '' };
}

/**
 * 筛选 → 查询参数。
 *
 * 空串一律不发（后端把 `?keyword=` 当「等于空串」过滤会查出 0 行），
 * 与 `api/business/erp/index.ts::asParams` 同一约定；这里先去掉是为了单测能断言。
 */
export function buildPayableQuery(
  filter: PayableFilterState,
  page: number,
  pageSize: number = PAYABLE_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.supplier_id) {
    query.supplier_id = filter.supplier_id;
  }
  const no = filter.payable_no.trim();
  if (no) {
    query.payable_no = no;
  }
  if (filter.due_date_from) {
    query.due_date_from = filter.due_date_from;
  }
  if (filter.due_date_to) {
    query.due_date_to = filter.due_date_to;
  }
  return query;
}

/** 是否处于筛选态（空态文案分「没数据」与「没匹配」） */
export function isPayableFiltered(filter: PayableFilterState): boolean {
  return Boolean(
    filter.status ||
      filter.supplier_id ||
      filter.payable_no.trim() ||
      filter.due_date_from ||
      filter.due_date_to,
  );
}

/**
 * 付款进度（0–100，整数）。
 *
 * 用**整数分**算，避免 `paid/amount` 的浮点误差；`amount <= 0` 时返回 0（库里有
 * `ck_biz_payable_amount` 兜底，正常不会出现，但脏数据不许让进度条 NaN）。
 */
export function paidPercent(paid: number | string, amount: number | string): number {
  const total = toCents(amount);
  if (total <= 0) {
    return 0;
  }
  const paidCents = toCents(paid);
  const percent = Math.round((paidCents / total) * 100);
  return Math.min(100, Math.max(0, percent));
}

/** 是否逾期（未付 / 部分付款且到期日早于今天；已结清与已作废不标） */
export function isOverdue(row: Pick<PayableRow, 'status' | 'due_date'>, today: string = todayString()): boolean {
  if (row.status !== 'OPEN' && row.status !== 'PARTIAL_PAID') {
    return false;
  }
  if (!row.due_date) {
    return false;
  }
  return row.due_date < today;
}

/** 逾期文案（表格里的小字；不逾期返回空串，调用方 `v-if` 决定要不要渲染） */
export function overdueText(row: Pick<PayableRow, 'status' | 'due_date'>, today: string = todayString()): string {
  return isOverdue(row, today) ? '已逾期' : '';
}

/**
 * 未付金额：**优先读库列** `unpaid_amount`，缺了才用 `amount − paid` 兜底。
 *
 * 为什么不是「自己算一遍更保险」：库列是 P2-C1 过账时写的，`ck_biz_payable_unpaid`
 * 保证它与 `amount − paid_amount` 恒等；前端再算一遍只在脏数据时才有差别，
 * 那种情况应当**显示后端给的数**（对账以库为准），兜底只是防 `null`。
 */
export function unpaidOf(row: Pick<PayableRow, 'amount' | 'paid_amount' | 'unpaid_amount'>): number {
  if (row.unpaid_amount !== null && row.unpaid_amount !== undefined && row.unpaid_amount !== '') {
    return toNumber(row.unpaid_amount);
  }
  return toNumber(row.amount) - toNumber(row.paid_amount);
}

/** 来源单据展示文案（`STOCK_IN` → 收货入库） */
export function sourceTypeLabel(sourceType: string | null | undefined): string {
  switch (sourceType) {
    case 'STOCK_IN':
      return '收货入库';
    case 'MANUAL':
      return '手工';
    default:
      return dash(sourceType);
  }
}

/** 行摘要：`来源 收货单号`（表格「来源」列与详情抽屉共用） */
export function sourceText(row: Pick<PayableRow, 'source_type' | 'source_no'>): string {
  const label = sourceTypeLabel(row.source_type);
  return row.source_no ? `${label} ${row.source_no}` : label;
}

/** 供应商名称（主数据没加载完时退化成短 id，别留白） */
export function supplierNameOf(
  supplierId: string | null | undefined,
  supplierNameMap: ReadonlyMap<string, string>,
): string {
  if (!supplierId) {
    return '—';
  }
  return supplierNameMap.get(supplierId) ?? shortId(supplierId);
}

// ==================== D5：应付单 → 发起付款 ====================

/** 可付信息里本文件用得到的字段（避免把整个 api 类型塞进纯逻辑） */
export interface PayableInfoLike {
  /** 后端给的「能不能付」结论 */
  can_pay: boolean;
  /** 不能付的原因 */
  blocked_reason: string | null;
  /** 默认付款金额（= 未付） */
  default_amount: number | string;
  /** 未付金额 */
  unpaid_amount: number | string;
  /** 应付总额 */
  amount: number | string;
}

/**
 * 付款弹窗的**预填金额**（F2 / O1：带未付，不带总额）。
 *
 * 直接取后端 `default_amount`，**不在前端重算**：
 * ① 未付金额的口径只有库列说了算（`ck_biz_payable_unpaid` 恒等）；
 * ② 后端在不能付款时把 `default_amount` 归 0，前端不需要再判一次状态。
 *
 * 兜底：老版本后端没有 `default_amount`（灰度期）时退回 `unpaid_amount`，
 * 再不行按 `amount`（至少不会填出 NaN）。
 */
export function payPrefillAmount(info: PayableInfoLike | null | undefined): number {
  if (!info) {
    return 0;
  }
  const fallback = toNumber(info.unpaid_amount ?? info.amount);
  const amount = info.default_amount === null || info.default_amount === undefined
    ? fallback
    : toNumber(info.default_amount);
  return Number.isFinite(amount) && amount > 0 ? amount : 0;
}

/**
 * 详情抽屉「发起付款」按钮是否显示。
 *
 * 三个条件同时成立：后端说能付（`can_pay`）、有付款权限、预填金额 > 0。
 * 卡面 L9：已结清 / 已作废的单**不显示**按钮（不是置灰 —— 置灰会让人一直点）。
 *
 * # P-FLOW-3B 补的第 4 个条件：`status` 不能是 `PENDING_CANCEL`
 *
 * 后端 `payable_payment.rs::payment_capability` **还没拦** `PENDING_CANCEL`
 * （它的分支只有 CANCELLED / SETTLED / unpaid<=0 / 有审批中的付款），
 * 于是「作废审批中」的单 `can_pay` 仍是 `true` —— 前端照抄就会在作废审批中显示「发起付款」，
 * 用户点下去必然被 `validate_payment_target`（只认 OPEN / PARTIAL_PAID）409 打回。
 * 卡面 §一 ① 明确要求「该状态下付款按钮禁用」，所以这里补一道前端闸，
 * 并把「后端 can_pay 口径漏了 PENDING_CANCEL」写进子卡让后端修（见
 * `docs/evidence/20261102-pflow3b-fe/README.md`）。后端补上后这道闸自动变成冗余（不会误伤）。
 *
 * `status` 省略时按老口径判（老调用方 / 老用例不用改）。
 */
export function canStartPay(
  info: PayableInfoLike | null | undefined,
  hasPayPermission: boolean,
  status?: PayableStatus | string | null,
): boolean {
  if (!info || !hasPayPermission) {
    return false;
  }
  if (status === PAYABLE_STATUS_PENDING_CANCEL) {
    return false;
  }
  return info.can_pay === true && payPrefillAmount(info) > 0;
}

// ==================== P-FLOW-3B：作废审批中 / 撤回 ====================

/** 行 / 详情里这三个判据只需要状态与已付金额（列表行与详情结构都满足）。 */
export interface PayableActionState {
  /** 状态 */
  status: PayableStatus | string;
  /** 已付金额（`numeric` 出来是字符串） */
  paid_amount: number | string;
}

/**
 * 可付款：未付 / 部分已付（后端同样按这个口径判 409）。
 *
 * `PENDING_CANCEL` **不在**这里：作废审批中的单不能再付款（卡面 §一 ①「付款按钮禁用」）。
 * 这不是新增判据 —— 原来的白名单本来就不含它，只是 P-FLOW-3B 之前没有这个状态。
 */
export function canPayPayable(row: PayableActionState | null | undefined): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'OPEN' || row.status === 'PARTIAL_PAID';
}

/** 可作废：未付（OPEN）且一分钱都没付（后端同样按这个口径判 409）。 */
export function canCancelPayable(row: PayableActionState | null | undefined): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'OPEN' && toNumber(row.paid_amount) === 0;
}

/** 是否「作废审批中」（列表徽标 / 详情提示 / 按钮切换的唯一判据）。 */
export function isPendingCancel(row: Pick<PayableActionState, 'status'> | null | undefined): boolean {
  return row?.status === PAYABLE_STATUS_PENDING_CANCEL;
}

/**
 * 可撤回作废审批：**状态是 `PENDING_CANCEL`** 且有撤回权限。
 *
 * 为什么还判权限：后端撤回走 `POST /payable/{id}/withdraw-cancel`，
 * 权限码 `business:erp:payable:withdraw-cancel`；V0191 目前只把它授给了 `SUPER_ADMIN`
 * （老码 `business:erp:payable:cancel` 才是 FINANCE / ACCOUNTANT 都有的），
 * 没权限的人点了只会 403 —— 与其让人点出错，不如直接不显示撤回入口并说明原因
 * （`withdrawCancelBlockedReason`）。
 */
export function canWithdrawCancel(
  row: Pick<PayableActionState, 'status'> | null | undefined,
  hasWithdrawPermission = true,
): boolean {
  return isPendingCancel(row) && hasWithdrawPermission;
}

/** 没有撤回权限时给用户的一句解释（有权限返回空串）。 */
export function withdrawCancelBlockedReason(
  row: Pick<PayableActionState, 'status'> | null | undefined,
  hasWithdrawPermission: boolean,
): string {
  if (!isPendingCancel(row) || hasWithdrawPermission) {
    return '';
  }
  return '当前账号没有「撤回作废审批」的权限（business:erp:payable:withdraw-cancel），请联系有权限的发起人或系统管理员';
}

/**
 * 详情抽屉该展示哪一条审批进度。
 *
 * P-FLOW-3B 起同一块位置要能显示两种进度：付款审批（P17）与**作废审批**。
 * 判据来源不同，必须一起决定，否则会出现「抽屉里显示的是付款审批、单据其实卡在作废审批」的误导：
 *
 * - 单据在作废审批中（`PENDING_CANCEL`）→ 用 **`cancel_instance_id`**（`PayableRes` 列表 + 详情
 *   都下发的**新字段**）。补漏之前只能用「送审那一刻」响应里的 `approval_instance_id`，
 *   重新打开抽屉进度区就消失了；
 * - 否则 → 用那笔还在审批中的付款实例（付款记录上的 `approval_instance_id`）。
 *
 * 抽成纯函数是为了能单测：写错分支不会报错，只会「进度区空着」或「标题与内容不一致」。
 */
export function approvalTargetOf(
  row: { status?: string | null; cancel_instance_id?: string | null } | null | undefined,
  paymentInstanceId?: string | null,
): { instanceId: string | null; title: string } {
  const cancelInstanceId = (row?.cancel_instance_id ?? '').trim();
  if (row?.status === PAYABLE_STATUS_PENDING_CANCEL && cancelInstanceId) {
    return { instanceId: cancelInstanceId, title: '作废审批进度' };
  }
  return { instanceId: (paymentInstanceId ?? '').trim() || null, title: '付款审批进度' };
}

/**
 * 抽屉里的「作废原因」一行（没有原因返回空串 → 模板 `v-if` 不渲染）。
 *
 * 原因走 `PayableRes::cancel_reason`（新字段），不是「送审那一刻」的临时状态，
 * 所以刷新 / 重开抽屉后仍在。
 */
export function cancelReasonText(row: { cancel_reason?: string | null } | null | undefined): string {
  return (row?.cancel_reason ?? '').trim();
}

/**
 * 来源单据的跳转路径（`STOCK_IN` → 收货单页并打开详情）。
 *
 * 目前只有收货单页支持 `?in_id=`（见 `views/business/erp/stockin/index.vue`）；
 * 其它来源类型返回 `null`，前端渲染成纯文本（不给假链接）。
 */
export function sourceDocPath(sourceType: string | null | undefined, sourceId: string | null | undefined): string | null {
  if (!sourceId) {
    return null;
  }
  if (sourceType === 'STOCK_IN') {
    return `/business/erp/stock-in?in_id=${encodeURIComponent(sourceId)}`;
  }
  return null;
}

/** 付款凭证的跳转路径（凭证页支持 `?voucher_no=` 预填筛选）；空号返回 `null`。 */
export function voucherDocPath(voucherNo: string | null | undefined): string | null {
  const no = (voucherNo ?? '').trim();
  return no ? `/business/erp/voucher?voucher_no=${encodeURIComponent(no)}` : null;
}

/** 应付单的跳转路径（付款记录上的「应付单号」点回；应付页支持 `?payable_no=` 深链）。 */
export function payableDocPath(payableNo: string | null | undefined): string | null {
  const no = (payableNo ?? '').trim();
  return no ? `/business/erp/payable?payable_no=${encodeURIComponent(no)}` : null;
}
