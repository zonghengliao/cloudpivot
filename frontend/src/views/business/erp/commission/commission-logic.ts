/**
 * 佣金结算页纯逻辑（SO-37 / ARCH-7 §15.4.3）—— 可测的判断都在这里，`.vue` 只渲染。
 *
 * # 口径（全抄后端，前端不重算）
 *
 * | 项 | 真口径 | 出处 |
 * |----|--------|------|
 * | 状态机 | `DRAFT → APPROVED → PAID`，**无回退边** | `commission_dto::LEGAL_EDGES` |
 * | 审批副作用 | `APPROVED` 出**计提凭证**（借 5601 / 贷 2241.02） | `commission_handler::post_commission_voucher` |
 * | 发放副作用 | `PAID` 出**付款凭证**（借 2241.02 / 贷 1002） | 同上 |
 * | 期间 | `yyyy-MM`（`ck_biz_cmset_period`；月份 01–12） | `validate_period_code` |
 * | 基数 | 已**收款**口径（不是开单金额），**有符号**：正计提 / 负冲回 | `commission_dto.rs` 头注释 |
 * | 合计 | `total_sales` 按 0 兜底（纯冲回期间记 0），`total_commission` **可为负** | `ck_biz_cmset_total_sales` |
 *
 * # 为什么「合计」让用户看到负数
 *
 * 纯退货 / 退款月份，佣金是负数（本期该收回的钱）。前端把负数藏成 0 会让财务以为
 * 「这个月不用发也没得扣」，下个月才发现账上多一笔应付款 —— 负数必须显式显示成
 * 红色并标「冲回」。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { formatMoney, isMonthKey, todayPeriod } from '@/views/business/erp/shared/money';
import type {
  CommissionItemRow,
  CommissionSettlementFilter,
  CommissionSettlementRow,
  CommissionSettlementStatus,
  CommissionSkippedRow,
  SettleCommissionReq,
  SettleCommissionRes,
} from '@/api/business/erp/commission';

/**
 * 结算状态域（与 `commission_dto::STATUSES` 逐字一致，顺序 = 状态机走向）。
 *
 * **必须定义在这里而不是 `api/commission.ts`**：api 模块顶层 import 了
 * `@/api/request`（读 `import.meta.env`），而单测跑在 node 下
 * （`import.meta.env === undefined` → 加载即 TypeError）。页面逻辑只要从 api 模块
 * 取**运行时值**，`pnpm test:unit` 就整条崩（本仓既有约束）。类型仍从 api 模块来。
 */
export const COMMISSION_SETTLEMENT_STATUSES: readonly CommissionSettlementStatus[] = [
  'DRAFT',
  'APPROVED',
  'PAID',
];

/** 合法状态迁移边（`commission_dto::LEGAL_EDGES` 逐字一致；无回退边） */
export const COMMISSION_STATUS_EDGES: ReadonlyArray<
  readonly [CommissionSettlementStatus, CommissionSettlementStatus]
> = [
  ['DRAFT', 'APPROVED'],
  ['APPROVED', 'PAID'],
];

/** 每页条数（与仓内列表页同口径） */
export const SETTLEMENT_PAGE_SIZE = 20;

/** 状态中文名（后端 `status_label` 不给结算单，前端维护这一份） */
export const SETTLEMENT_STATUS_LABELS: Record<CommissionSettlementStatus, string> = {
  DRAFT: '草稿',
  APPROVED: '已审批',
  PAID: '已发放',
};

/** 状态配色：草稿灰、已审批（已入账、待付钱）警告色、已发放成功色 */
export const SETTLEMENT_STATUS_THEMES: Record<
  CommissionSettlementStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  APPROVED: 'warning',
  PAID: 'success',
};

/** 状态筛选候选（`''` = 全部） */
export const SETTLEMENT_STATUS_OPTIONS: Array<{
  label: string;
  value: '' | CommissionSettlementStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿（可重算）', value: 'DRAFT' },
  { label: '已审批（已计提）', value: 'APPROVED' },
  { label: '已发放（已付款）', value: 'PAID' },
];

/** 结算单列表列（`op` 由页面用 `#op` 插槽渲染） */
export const SETTLEMENT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'settlement_no', title: '结算单号', width: 180, fixed: 'left' },
  { colKey: 'salesperson_name', title: '销售员', minWidth: 120, ellipsis: true },
  { colKey: 'period_code', title: '期间', width: 110 },
  { colKey: 'total_sales', title: '佣金基数', width: 140, align: 'right' },
  { colKey: 'total_commission', title: '应发佣金', width: 140, align: 'right' },
  { colKey: 'item_count', title: '明细', width: 80, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right' },
];

/** 明细列（哪笔订单 / 应收产生多少佣金） */
export const ITEM_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'so_no', title: '销售订单', width: 180 },
  { colKey: 'ar_no', title: '应收单号', width: 180 },
  { colKey: 'rule_name', title: '命中规则', minWidth: 160, ellipsis: true },
  { colKey: 'rate', title: '比率快照', width: 110, align: 'right' },
  { colKey: 'base_amount', title: '佣金基数', width: 140, align: 'right' },
  { colKey: 'commission', title: '本行佣金', width: 130, align: 'right' },
  { colKey: 'remark', title: '备注', minWidth: 160, ellipsis: true },
];

// ==================== 状态机 ====================

/** 状态中文名（未知值回落编码本身） */
export function settlementStatusLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return SETTLEMENT_STATUS_LABELS[value as CommissionSettlementStatus] ?? value;
}

/** 状态配色 */
export function settlementStatusTheme(
  value: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!value) {
    return 'default';
  }
  return SETTLEMENT_STATUS_THEMES[value as CommissionSettlementStatus] ?? 'default';
}

/** 该状态允许迁到的目标态（无回退边；`PAID` 是终态） */
export function nextStatuses(
  from: string | null | undefined,
): CommissionSettlementStatus[] {
  if (!from) {
    return [];
  }
  return COMMISSION_STATUS_EDGES.filter(([edgeFrom]) => edgeFrom === from).map(
    ([, to]) => to,
  );
}

/** 边是否合法（跳步 / 回退一律 `false`，页面据此不渲染按钮） */
export function canTransition(
  from: string | null | undefined,
  to: CommissionSettlementStatus,
): boolean {
  return nextStatuses(from).includes(to);
}

/** 动作文案（按**边**取，不按目标态：`APPROVED` 只能来自草稿） */
export function statusActionLabel(to: CommissionSettlementStatus): string {
  if (to === 'APPROVED') {
    return '审批通过';
  }
  if (to === 'PAID') {
    return '确认发放';
  }
  return '转草稿';
}

/** 动作后果说明（点之前让人知道会生成什么凭证） */
export function statusActionHint(to: CommissionSettlementStatus): string {
  if (to === 'APPROVED') {
    return '审批通过会生成佣金计提凭证（借 5601 销售费用 / 贷 2241.02 其他应付款-佣金），生成后不能重算，也不能撤回。';
  }
  if (to === 'PAID') {
    return '确认发放会生成付款凭证（借 2241.02 / 贷 1002 银行存款），这是钱真正出账的一步。';
  }
  return '';
}

/** 是否需要「备注」（发放说明建议填，但不强制：后端 `reason` 可选） */
export function statusNeedsReason(to: CommissionSettlementStatus): boolean {
  return to === 'PAID';
}

// ==================== 期间 ====================

/** 期间文案（`2026-11` → `2026 年 11 月`） */
export function periodLabel(period: string | null | undefined): string {
  if (!period) {
    return '—';
  }
  const match = /^(\d{4})-(\d{2})$/.exec(period);
  if (!match) {
    return period;
  }
  return `${match[1]} 年 ${Number(match[2])} 月`;
}

/**
 * 可结算期间候选：**本月往前 12 个月**（共 13 个），新的排前面。
 *
 * 为什么不含未来月：佣金按「已收款」计，未来期间一定没有已核销金额，
 * 选出来只会得到一张零明细空单，白白占掉「同人同期间唯一结算单」的额度。
 */
export function periodOptions(
  now: Date = new Date(),
  months = 13,
): Array<{ label: string; value: string }> {
  const current = todayPeriod(now);
  const [yearText, monthText] = current.split('-');
  let year = Number(yearText);
  let month = Number(monthText);
  const out: Array<{ label: string; value: string }> = [];
  for (let i = 0; i < months; i += 1) {
    const value = `${year}-${String(month).padStart(2, '0')}`;
    out.push({ label: periodLabel(value), value });
    month -= 1;
    if (month === 0) {
      month = 12;
      year -= 1;
    }
  }
  return out;
}

/** 期间校验（返回人话错误；`null` = 通过） */
export function validateSettlePeriod(period: string): string | null {
  if (!period) {
    return '请选择结算期间';
  }
  if (!isMonthKey(period)) {
    return '结算期间格式必须是 yyyy-MM（例如 2026-11）';
  }
  return null;
}

/** 生成结算单入参（空销售员 = 不限，后端按「有可计佣差额的人」各生成一张） */
export function buildSettlePayload(period: string, salespersonId?: string, remark?: string): SettleCommissionReq {
  const payload: SettleCommissionReq = { period_code: period };
  if (salespersonId) {
    payload.salesperson_id = salespersonId;
  }
  const trimmed = (remark ?? '').trim();
  if (trimmed) {
    payload.remark = trimmed;
  }
  return payload;
}

/**
 * 生成结果汇总文案（**成功 + 跳过都要说**）。
 *
 * 后端把「没有可计佣差额 / 已有审批中的单」放进 `skipped`（200 不是错误）；
 * 只报「成功 0 张」会让用户以为接口挂了。
 */
export function settleResultText(res: SettleCommissionRes): string {
  const created = res.settlements.length;
  const skipped = res.skipped.length;
  const period = periodLabel(res.period_code);
  const parts: string[] = [];
  if (created > 0) {
    parts.push(`已生成 ${created} 张草稿结算单（${period}）`);
  } else {
    parts.push(`${period} 没有生成结算单`);
  }
  if (skipped > 0) {
    parts.push(`${skipped} 位销售员被跳过`);
  }
  return parts.join('；');
}

/** 跳过原因逐条文案（销售员 + 后端原文，别自己编原因） */
export function skippedText(row: CommissionSkippedRow): string {
  return `${row.salesperson_name || '（未知销售员）'}：${row.reason}`;
}

// ==================== 展示 ====================

/** 金额展示（2 位千分位；负数原样带负号 = 冲回） */
export const moneyText = formatMoney;

/** 是否冲回（佣金为负） */
export function isClawback(value: number | string | null | undefined): boolean {
  const num = Number(value ?? 0);
  return Number.isFinite(num) && num < 0;
}

/** 明细行是否冲回（负基数或负佣金） */
export function isItemClawback(row: Pick<CommissionItemRow, 'base_amount' | 'commission'>): boolean {
  return isClawback(row.commission) || isClawback(row.base_amount);
}

/** 发放时间文案（只显示日期部分，`null` → `—`） */
export function paidAtText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.slice(0, 10);
}

/** 凭证文案（没审批时 `—`，有凭证号用凭证号） */
export function voucherText(row: Pick<CommissionSettlementRow, 'voucher_no' | 'voucher_id'>): string {
  if (row.voucher_no) {
    return row.voucher_no;
  }
  if (row.voucher_id) {
    return `凭证 ${row.voucher_id.slice(0, 8)}…`;
  }
  return '—（未计提）';
}

/**
 * 明细行比率文案（`PERCENT` 带 `%`、`FIXED` 带「元/单」）。
 *
 * 明细只存 `rate` 快照，**没有** `calc_type`；判据用「≤ 100 且非整数金额」太脆。
 * 后端写明细时 `rate` 就是规则快照值，百分比一定是 0–100 之间的小数，
 * 固定额通常 ≥ 100 —— 但这不可靠，所以这里**不加单位**，只显示数值，
 * 单位由「命中规则」列后面括号里的规则名 / 页面说明承担。
 */
export function itemRateText(row: Pick<CommissionItemRow, 'rate'>): string {
  const num = Number(row.rate ?? 0);
  if (!Number.isFinite(num)) {
    return String(row.rate ?? '—');
  }
  return num.toFixed(4).replace(/0+$/, '').replace(/\.$/, '');
}

// ==================== 筛选 ====================

/** 空筛选 */
export function emptySettlementFilter(): CommissionSettlementFilter {
  return {
    keyword: '',
    period_code: '',
    salesperson_id: '',
    status: '',
    page: 1,
    page_size: SETTLEMENT_PAGE_SIZE,
  };
}

/** 是否带筛选条件 */
export function isSettlementFiltered(filter: CommissionSettlementFilter): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() || filter.period_code || filter.salesperson_id || filter.status,
  );
}

/** 列表空态文案 */
export function settlementEmptyText(filter: CommissionSettlementFilter): string {
  return isSettlementFiltered(filter)
    ? '没有匹配的结算单：换个期间 / 状态再查'
    : '还没有结算单：点右上角「生成结算单」，按期间 + 销售员出一张草稿';
}

/**
 * 结算单合计行（前端只汇总**当前页**，不是全量）。
 *
 * 页面上必须写「本页合计」—— 把当前页当全量是最常见的一类误读（财务会拿它去对账）。
 */
export function pageTotals(rows: CommissionSettlementRow[]): {
  sales: number;
  commission: number;
  count: number;
} {
  let sales = 0;
  let commission = 0;
  for (const row of rows) {
    const s = Number(row.total_sales ?? 0);
    const c = Number(row.total_commission ?? 0);
    if (Number.isFinite(s)) {
      sales += s;
    }
    if (Number.isFinite(c)) {
      commission += c;
    }
  }
  return { sales, commission, count: rows.length };
}
