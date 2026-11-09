/**
 * 凭证页纯逻辑（P2-E2）—— 状态映射 / **借贷平衡判断** / 筛选参数。
 *
 * # ★ 借贷平衡为什么要前端再算一遍
 *
 * 后端已经有三道闸：`biz_voucher` 的 `CHECK (total_debit = total_credit)`、
 * 详情接口里「分录合计 vs 单头合计」不一致直接 `500 + 66009`、
 * 生成凭证时 `voucher.rs::post` 里 `assert balanced`。
 *
 * 但**用户看到的平衡必须是他看得懂的那种**：合计行写出借方总和与贷方总和、
 * 相等打绿勾、不等红字警告 —— 这不是把后端结论照抄一遍，而是把
 * 「钱对不对」变成屏幕上两个数字的对比。所以详情抽屉里：
 *
 * ```text
 * 合计   Σ借（前端逐行加的分）   Σ贷（前端逐行加的分）   ✅ 借贷平衡 / ⚠ 借贷不平，差额 X
 * ```
 *
 * 前端这一份若与后端 `entry_total_debit/credit` 不一致，界面会把两个数并排标出来
 * （见 {@link balanceCheck}），因为那说明**响应本身自相矛盾**，比账不平更该报警。
 *
 * # 金额一律整数分
 *
 * 见 `../shared/money.ts` 文件头：`226.00000000000003` 这种浮点残留会让
 * 「平」变「不平」，而这是财务页面最不能接受的一类假警报。
 */
import type { VoucherEntryLine, VoucherRow, VoucherStatus, VoucherType } from '@/api/business/erp';
import type { TagTheme } from '../payable/payable-logic';
import { dash, toCents } from '../shared/money';

/** 每页条数（与其它 ERP 列表页同口径） */
export const VOUCHER_PAGE_SIZE = 20;

/** 状态 → 中文（与 `voucher_query::status_label` 逐字一致；后端也给了 `status_label`） */
export function voucherStatusLabel(status: VoucherStatus | string): string {
  switch (status) {
    case 'POSTED':
      return '已记账';
    case 'DRAFT':
      return '草稿';
    case 'AUDITED':
      return '已审核';
    case 'CANCELLED':
      return '已作废';
    default:
      return String(status);
  }
}

/**
 * 状态 → tag 主题。
 *
 * `POSTED`（唯一进总账的状态）用 success 绿；
 * `DRAFT` 灰（还没记账，不是错）；`AUDITED` 蓝（过了审核但未记账）；`CANCELLED` 红。
 */
export function voucherStatusTheme(status: VoucherStatus | string): TagTheme {
  switch (status) {
    case 'POSTED':
      return 'success';
    case 'DRAFT':
      return 'default';
    case 'AUDITED':
      return 'primary';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 状态下拉候选（`''` = 全部；凭证列表**默认全状态**，草稿也要看得见） */
export const VOUCHER_STATUS_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: '' | VoucherStatus }> = [
  { label: '全部状态', value: '' },
  { label: '已记账', value: 'POSTED' },
  { label: '草稿', value: 'DRAFT' },
  { label: '已审核', value: 'AUDITED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 凭证类型 → 中文（与 `voucher_query::voucher_type_label` 一致） */
export function voucherTypeLabel(voucherType: VoucherType | string): string {
  switch (voucherType) {
    case 'AUTO':
      return '自动';
    case 'MANUAL':
      return '手工';
    default:
      return String(voucherType);
  }
}

/** 凭证类型下拉候选（`''` = 全部） */
export const VOUCHER_TYPE_FILTER_OPTIONS: ReadonlyArray<{ label: string; value: '' | VoucherType }> = [
  { label: '全部来源类型', value: '' },
  { label: '自动（过账生成）', value: 'AUTO' },
  { label: '手工录入', value: 'MANUAL' },
];

/**
 * 来源单据类型 → 中文。
 *
 * ⚠️ 卡面 §四 的「来源」列写的是「采购入库」，对应 **`source_type = STOCK_IN`**；
 * `biz_voucher.source_type` 是自由文本（V0149 无 CHECK），所以未知值原样显示而不是吞掉。
 */
export function sourceTypeLabel(sourceType: string | null | undefined): string {
  switch (sourceType) {
    case 'STOCK_IN':
      return '采购入库';
    case 'PURCHASE_IN_REVERSE':
      return '采购入库（红冲）';
    case 'MANUAL':
      return '手工';
    default:
      return dash(sourceType);
  }
}

// ==================== ★ 借贷平衡 ====================

/** 借贷合计（整数分，避免浮点） */
export interface BalanceTotals {
  /** 借方合计（分） */
  debitCents: number;
  /** 贷方合计（分） */
  creditCents: number;
  /** 差额 = 借 − 贷（分；0 = 平） */
  diffCents: number;
  /** 借贷是否相等（**精确到分**） */
  balanced: boolean;
}

/** 逐行累加借方 / 贷方（整数分），返回合计与平衡结论 */
export function sumEntries(entries: ReadonlyArray<Pick<VoucherEntryLine, 'debit_amount' | 'credit_amount'>>): BalanceTotals {
  let debitCents = 0;
  let creditCents = 0;
  for (const entry of entries) {
    debitCents += toCents(entry.debit_amount);
    creditCents += toCents(entry.credit_amount);
  }
  const diffCents = debitCents - creditCents;
  return { debitCents, creditCents, diffCents, balanced: diffCents === 0 };
}

/** 只判平不平（单测与列表页用；详情页用 {@link sumEntries} 拿到两个合计数） */
export function isBalanced(
  entries: ReadonlyArray<Pick<VoucherEntryLine, 'debit_amount' | 'credit_amount'>>,
): boolean {
  return sumEntries(entries).balanced;
}

/** 平衡提示文案（相等 → 绿勾；不等 → 红字 + 差额） */
export function balanceText(balanced: boolean, diffCents = 0): string {
  if (balanced) {
    return '借贷平衡';
  }
  return `借贷不平，差额 ${(Math.abs(diffCents) / 100).toFixed(2)}`;
}

/** 平衡图标（TDesign 图标名；绿勾 / 红警告） */
export function balanceIcon(balanced: boolean): string {
  return balanced ? 'check-circle-filled' : 'error-circle-filled';
}

/** 平衡提示的主题（绿 / 红） */
export function balanceTheme(balanced: boolean): TagTheme {
  return balanced ? 'success' : 'danger';
}

/**
 * 详情的三向校验：**前端逐行加** vs **后端给的分录合计** vs **后端给的结论**。
 *
 * ```text
 * frontend   = Σ(debit) / Σ(credit)                    ← 屏幕上合计行显示的数
 * backend    = entry_total_debit / entry_total_credit  ← 详情接口给的数
 * header     = total_debit / total_credit              ← 单头合计
 * ```
 *
 * 三者相等才算「真的平」；`mismatch` 非空说明**响应自相矛盾**
 * （后端 66009 的闸没兜住），UI 要显眼地说出来，而不是只信 `balanced` 字段。
 */
export interface BalanceCheck extends BalanceTotals {
  /** 后端给的分录借方合计（分） */
  backendDebitCents: number;
  /** 后端给的分录贷方合计（分） */
  backendCreditCents: number;
  /** 单头借方合计（分） */
  headerDebitCents: number;
  /** 单头贷方合计（分） */
  headerCreditCents: number;
  /** 前端算的与后端给的三个数是否完全一致 */
  consistent: boolean;
  /** 不一致时的人话描述（一致为空串） */
  mismatch: string;
}

/** 对一张凭证详情做完整平衡校验 */
export function balanceCheck(
  detail: Pick<
    VoucherDetailLike,
    'entries' | 'entry_total_debit' | 'entry_total_credit' | 'total_debit' | 'total_credit'
  >,
): BalanceCheck {
  const totals = sumEntries(detail.entries);
  const backendDebitCents = toCents(detail.entry_total_debit);
  const backendCreditCents = toCents(detail.entry_total_credit);
  const headerDebitCents = toCents(detail.total_debit);
  const headerCreditCents = toCents(detail.total_credit);
  const consistent =
    backendDebitCents === totals.debitCents &&
    backendCreditCents === totals.creditCents &&
    headerDebitCents === totals.debitCents &&
    headerCreditCents === totals.creditCents;
  return {
    ...totals,
    backendDebitCents,
    backendCreditCents,
    headerDebitCents,
    headerCreditCents,
    consistent,
    mismatch: consistent
      ? ''
      : `分录逐行合计 ${(totals.debitCents / 100).toFixed(2)} / ${(totals.creditCents / 100).toFixed(2)}` +
        ` 与后端合计 ${(backendDebitCents / 100).toFixed(2)} / ${(backendCreditCents / 100).toFixed(2)} 不一致`,
  };
}

/** {@link balanceCheck} 需要的最小形状（避免 import 整个 `VoucherDetail` 造成循环） */
interface VoucherDetailLike {
  entries: VoucherEntryLine[];
  entry_total_debit: number | string;
  entry_total_credit: number | string;
  total_debit: number | string;
  total_credit: number | string;
}

// ==================== 筛选 ====================

/** 筛选行 */
export interface VoucherFilterState {
  /** 会计期间 `yyyy-MM`（`''` = 全部） */
  period: string;
  /** 状态（`''` = 全部） */
  status: VoucherStatus | '';
  /** 凭证类型（`''` = 全部） */
  voucher_type: VoucherType | '';
  /** 来源单据类型（`''` = 全部） */
  source_type: string;
  /** 业务单据号（精确；退货单号等「一张业务单 → 多张凭证」的查询键） */
  source_doc_no: string;
  /** 凭证号（后端模糊 `LIKE %x%`） */
  voucher_no: string;
}

/** 空筛选 */
export function emptyVoucherFilter(): VoucherFilterState {
  return {
    period: '',
    status: '',
    voucher_type: '',
    source_type: '',
    source_doc_no: '',
    voucher_no: '',
  };
}

/**
 * 筛选 → 查询参数（空串一律不发）。
 *
 * ⚠️ 这里**没有** `page_size` 之外的默认值注入：`period` 不填就是「全部期间」，
 * 不能默认成当月 —— 会计核对历史凭证时不希望还要先清一次筛选。
 */
export function buildVoucherQuery(
  filter: VoucherFilterState,
  page: number,
  pageSize: number = VOUCHER_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  const period = filter.period.trim();
  if (period) {
    query.period = period;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.voucher_type) {
    query.voucher_type = filter.voucher_type;
  }
  if (filter.source_type) {
    query.source_type = filter.source_type;
  }
  // 业务单据号（精确）：后端 `AND source_doc_no = ?`，退货单号 → 两张凭证
  const docNo = filter.source_doc_no.trim();
  if (docNo) {
    query.source_doc_no = docNo;
  }
  const no = filter.voucher_no.trim();
  if (no) {
    query.voucher_no = no;
  }
  return query;
}

/** 是否处于筛选态 */
export function isVoucherFiltered(filter: VoucherFilterState): boolean {
  return Boolean(
    filter.period.trim() ||
      filter.status ||
      filter.voucher_type ||
      filter.source_type ||
      filter.source_doc_no.trim() ||
      filter.voucher_no.trim(),
  );
}

/** 列表行的平衡结论（列表只显示单头两列；红字凭证照常参与比较） */
export function rowBalanced(row: Pick<VoucherRow, 'total_debit' | 'total_credit'>): boolean {
  return toCents(row.total_debit) === toCents(row.total_credit);
}

/** 凭证头一行摘要：`记 PZ-20261031-000001`（凭证字 + 号） */
export function voucherTitleOf(row: Pick<VoucherRow, 'voucher_word' | 'voucher_no'>): string {
  return `${row.voucher_word || '记'} ${row.voucher_no}`;
}

// ==================== P-FLOW-3B：红冲审批中 / 撤回 ====================

/**
 * 「红冲审批中」徽标文案（卡面 §一 ② 原话）。
 *
 * ⚠️ 凭证**没有** `PENDING_REVERSE` 状态（硬红线 3：所有报表 / 账簿取数都写死
 * `WHERE status = 'POSTED'`，加状态会让已入账凭证从四大报表里消失）。
 * 「审批中」只是 `biz_voucher.pending_reverse_instance_id` 有值，`status` 仍是 `POSTED`。
 * 所以徽标必须与状态标签**并列显示**，不能顶替状态。
 */
export const VOUCHER_PENDING_REVERSE_LABEL = '红冲审批中';

/** 判断红冲按钮 / 徽标所需的凭证字段（列表行与详情都满足）。 */
export interface VoucherReverseState {
  /** 状态（红冲只认 `POSTED`） */
  status?: string | null;
  /** 是否红字凭证 */
  is_red?: boolean | null;
  /** 被红冲的原凭证 id（红字凭证才有） */
  reverse_of?: string | null;
  /** 红冲它的红字凭证 id（被冲过才有） */
  reversed_by?: string | null;
  /** 红冲它的红字凭证号（列表给的人话版本） */
  reversed_by_no?: string | null;
  /** 红冲审批实例 id（有值 = 红冲审批中） */
  pending_reverse_instance_id?: string | null;
}

/**
 * 是否「红冲审批中」：`pending_reverse_instance_id` 有值。
 *
 * 只看这个字段，不看 `status` —— 因为红冲审批中 `status` **仍然是 `POSTED`**
 * （这是后端设计，不是数据没刷新）。
 */
export function isPendingReverse(row: Pick<VoucherReverseState, 'pending_reverse_instance_id'> | null | undefined): boolean {
  const instanceId = row?.pending_reverse_instance_id;
  return typeof instanceId === 'string' && instanceId.trim() !== '';
}

/**
 * 可红冲：已记账（POSTED）、不是红字凭证、没被红冲过、**不在红冲审批中**。
 *
 * 最后一条是 P-FLOW-3B 新增的：审批中的凭证再点红冲，后端 409
 * 「已在红冲审批中（FLW-…）」，与其让人点了才看见红字，不如直接换成「撤回红冲审批」。
 */
export function canReverseVoucher(row: VoucherReverseState | null | undefined): boolean {
  if (!row) {
    return false;
  }
  if (row.status !== 'POSTED') {
    return false;
  }
  if (row.is_red === true) {
    return false;
  }
  if (row.reversed_by || row.reversed_by_no) {
    return false;
  }
  return !isPendingReverse(row);
}

/**
 * 可撤回红冲审批：状态 `POSTED`、非红字、没被红冲过、**确实在审批中**、且有撤回权限。
 *
 * 权限码 `business:erp:voucher:withdraw-reverse`（V0191 目前只授 `SUPER_ADMIN`）：
 * 没权限的人不显示入口，并在详情里说明原因（{@link withdrawReverseBlockedReason}）。
 */
export function canWithdrawReverse(
  row: VoucherReverseState | null | undefined,
  hasWithdrawPermission = true,
): boolean {
  if (!row || !hasWithdrawPermission) {
    return false;
  }
  if (row.status !== 'POSTED' || row.is_red === true || row.reversed_by || row.reversed_by_no) {
    return false;
  }
  return isPendingReverse(row);
}

/** 没有撤回权限时的一句解释（有权限 / 不在审批中返回空串）。 */
export function withdrawReverseBlockedReason(
  row: VoucherReverseState | null | undefined,
  hasWithdrawPermission: boolean,
): string {
  if (!isPendingReverse(row) || hasWithdrawPermission) {
    return '';
  }
  return '当前账号没有「撤回红冲审批」的权限（business:erp:voucher:withdraw-reverse），请联系有权限的发起人或系统管理员';
}

/*
 * P-FLOW-3B 补漏：原先这里还有一层「从 `GET /business/flow/instance/page` 挑出红冲类 PENDING
 * 实例、再按 `biz_id` 反查凭证」的兜底映射，已**整层删除**。它有两个毛病：
 * ① 每次列表多打一次实例分页；② 实例分页有**数据可见性过滤**（不是本人发起 / 参与就查不到），
 * 于是「能看凭证列表但没参与过红冲审批的账号」看不到徽标。
 * 后端补漏后列表 / 详情 / 红冲回执三处都下发该字段（字段恒在、不审批时为 `null`），
 * 前端直接读字段即可 —— 红冲的流程业务类型常量也随之成了死代码，一并删掉。
 */
