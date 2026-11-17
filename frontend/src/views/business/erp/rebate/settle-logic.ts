/**
 * 销售返利「结算单」页纯逻辑（M3-19 / 后端 M3-16、M3-17）—— 可测的判断都在这里，`.vue` 只渲染。
 *
 * # 口径（全抄后端，前端不重算一分钱）
 *
 * | 项 | 真口径 | 出处 |
 * |----|--------|------|
 * | 期间码形状 | 月度 `yyyy-MM`（01–12）、季度 `yyyy-QN`（N ∈ 1–4）；**与粒度互相钉死** | `PeriodArgs::resolve` / `ck_biz_srs_period_shape` |
 * | 基数 | 销售额 = 订单（`APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED`）− 已过账退货；销量同口径 | `V0439` 头注释 |
 * | 落档 | 净额 / 净量 ≤ 0 时**按 0 落档**（负数不产生负返利） | `calc_rebate` |
 * | 应返金额 | 2 位小数，恒 ≥ 0 | `calc_rebate` |
 * | 状态机 | `DRAFT`（可重算 / 可作废）→ `SETTLED`（已开红字发票，M3-17 置位）；`CANCELLED` 保留行 | `rebate_settle_dto::STATUSES` |
 * | 一期间一单 | 同客户同粒度同期间**未作废**的行唯一（作废后可重开） | `uk_biz_srs_period` |
 *
 * # 三条会让人误判的
 *
 * 1. **「没有政策」不是错误**：期间首日无启用政策、或政策没覆盖整期 → 后端 `409 + 66091`
 *    并在报文里写清缺哪段日期。页面要原样展示这句，不要翻成「系统错误」。
 * 2. **跑批逐客户独立**：`CREATED` / `EXISTS`（已有结算单跳过）/ `FAILED`（带原因）
 *    三种结果都要显示 —— 只报「成功 0 张」会让用户以为接口挂了。
 * 3. **作废只对 `DRAFT`**：`SETTLED` 想改只能红冲（M3-17 的红字发票），后端 409，页面别渲染按钮。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { formatMoney, toNumber } from '@/views/business/erp/shared/money';
import type {
  DecimalText,
  RebateBaseNumbers,
  RebatePreview,
  RebateRunItem,
  RebateRunReq,
  RebateRunRes,
  RebateSettlementFilter,
  RebateSettlementRow,
  RebateSettlementStatus,
  RebateSettlePeriod,
  RebateSettleReq,
} from '@/api/business/erp/rebate';

/** 每页条数（与仓内列表页同口径；后端上限 200） */
export const SETTLE_PAGE_SIZE = 20;

/** 备注上限（`rebate_settle_dto::MAX_REMARK_LEN`） */
export const SETTLE_REMARK_MAX = 500;

/**
 * 状态域（与 `rebate_settle_dto::STATUSES` 逐字一致）。
 *
 * 定义在这里而不是 `api/rebate.ts`：api 模块顶层 import 了 `@/api/request`
 * （读 `import.meta.env`），而单测跑在 node 下（`import.meta.env === undefined`）
 * —— 页面逻辑只要从 api 模块取**运行时值**，`pnpm test:unit` 就整条崩（本仓既有约束）。
 */
export const REBATE_SETTLEMENT_STATUSES: readonly RebateSettlementStatus[] = [
  'DRAFT',
  'SETTLED',
  'CANCELLED',
];

/** 结算粒度（顺序 = 下拉顺序） */
export const REBATE_SETTLE_PERIODS: readonly RebateSettlePeriod[] = ['MONTH', 'QUARTER'];

/** 状态中文名 */
export const SETTLE_STATUS_LABELS: Record<RebateSettlementStatus, string> = {
  DRAFT: '草稿',
  SETTLED: '已结算',
  CANCELLED: '已作废',
};

/** 状态配色：草稿灰（可改）、已结算成功色（钱已冲）、已作废线框 */
export const SETTLE_STATUS_THEMES: Record<
  RebateSettlementStatus,
  'default' | 'primary' | 'success' | 'warning' | 'danger'
> = {
  DRAFT: 'default',
  SETTLED: 'success',
  CANCELLED: 'danger',
};

/** 状态筛选候选（`''` = 全部） */
export const SETTLE_STATUS_OPTIONS: Array<{
  label: string;
  value: '' | RebateSettlementStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '草稿（可重算 / 可作废）', value: 'DRAFT' },
  { label: '已结算（已开红字发票）', value: 'SETTLED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 结算粒度筛选候选（`''` = 全部） */
export const SETTLE_PERIOD_FILTER_OPTIONS: Array<{
  label: string;
  value: '' | RebateSettlePeriod;
}> = [
  { label: '全部粒度', value: '' },
  { label: '月度', value: 'MONTH' },
  { label: '季度', value: 'QUARTER' },
];

/** 结算单列表列（`op` 由页面用 `#op` 插槽渲染） */
export const SETTLEMENT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'settlement_no', title: '结算单号', width: 160, fixed: 'left' },
  { colKey: 'customer', title: '客户', minWidth: 150, ellipsis: true },
  { colKey: 'period', title: '期间', width: 150 },
  { colKey: 'policy_no', title: '适用政策', width: 150, ellipsis: true },
  { colKey: 'base', title: '净销售额 / 净销量', minWidth: 160, align: 'right' },
  { colKey: 'tier', title: '命中档 / 费率', minWidth: 175, ellipsis: true },
  { colKey: 'rebate_amount', title: '应返金额', width: 120, align: 'right' },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'invoice', title: '红字发票', width: 130, ellipsis: true },
  { colKey: 'op', title: '操作', width: 145, fixed: 'right' },
];

// ==================== 状态 ====================

/** 状态中文名（未知值回落编码本身，别显示空白） */
export function settleStatusLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return SETTLE_STATUS_LABELS[value as RebateSettlementStatus] ?? value;
}

/** 状态配色 */
export function settleStatusTheme(
  value: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!value) {
    return 'default';
  }
  return SETTLE_STATUS_THEMES[value as RebateSettlementStatus] ?? 'default';
}

/** 能否重算 / 作废（只有 `DRAFT`；页面据此渲染按钮，后端仍会再判一次） */
export function isEditable(row: Pick<RebateSettlementRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

// ==================== 期间 ====================

/** 月度期间码形状：`yyyy-MM`，月份 01–12 */
const MONTH_CODE = /^\d{4}-(0[1-9]|1[0-2])$/;
/** 季度期间码形状：`yyyy-QN`，N 1–4（`q` 也认，展示前归一化） */
const QUARTER_CODE = /^\d{4}-Q([1-4])$/i;

/**
 * 归一化期间码：去空白、季度季号大写成 `Q`。
 *
 * 与后端 `Period::from_code` 同口径（后端接受 `2026-q1` 并归一化成 `2026-Q1`）。
 */
export function normalizePeriodCode(raw: string | null | undefined): string {
  const text = (raw ?? '').trim();
  if (!text) {
    return '';
  }
  return text.toUpperCase();
}

/**
 * 期间码校验（返回人话错误；`null` = 通过）。
 *
 * **粒度与形状必须互配**（后端 `PeriodArgs::resolve` 与库层 `ck_biz_srs_period_shape`
 * 是同一句话）：月度配 `yyyy-Q1` 会被 400，季度配 `yyyy-MM` 同样。
 */
export function validatePeriodCode(
  settlePeriod: RebateSettlePeriod,
  rawCode: string,
): string | null {
  const code = normalizePeriodCode(rawCode);
  if (!code) {
    return '请选择结算期间';
  }
  if (settlePeriod === 'MONTH') {
    if (!MONTH_CODE.test(code)) {
      return `月度期间码必须是 yyyy-MM（例如 2026-11），收到「${code}」`;
    }
    return null;
  }
  if (settlePeriod === 'QUARTER') {
    if (!QUARTER_CODE.test(code)) {
      return `季度期间码必须是 yyyy-QN（N 为 1-4，例如 2026-Q1），收到「${code}」`;
    }
    return null;
  }
  return '结算粒度只能是月度（MONTH）或季度（QUARTER）';
}

/** 期间码中文（`2026-11` → `2026 年 11 月`；`2026-Q1` → `2026 年第 1 季度`） */
export function periodCodeLabel(code: string | null | undefined): string {
  const text = normalizePeriodCode(code);
  if (!text) {
    return '—';
  }
  const month = /^(\d{4})-(0[1-9]|1[0-2])$/.exec(text);
  if (month) {
    return `${month[1]} 年 ${Number(month[2])} 月`;
  }
  const quarter = QUARTER_CODE.exec(text);
  if (quarter) {
    return `${text.slice(0, 4)} 年第 ${Number(quarter[1])} 季度`;
  }
  return text;
}

/** 期间区间文案（左闭右开的后端口径 → 给人看的「含末日」） */
export function periodRangeText(
  from: string | null | undefined,
  lastDay: string | null | undefined,
): string {
  if (!from || !lastDay) {
    return '—';
  }
  return `${from} ~ ${lastDay}`;
}

/**
 * 可结算的**月度**期间候选：本月往前 `months - 1` 个月（新的排前面）。
 *
 * 不含未来月：未来期间一定没有已确认订单，跑出来只会是零额空单，
 * 白占「同客户同期间唯一结算单」的额度（作废后才好重开）。
 */
export function monthPeriodOptions(
  now: Date = new Date(),
  months = 13,
): Array<{ label: string; value: string }> {
  let year = now.getFullYear();
  let month = now.getMonth() + 1;
  const out: Array<{ label: string; value: string }> = [];
  for (let index = 0; index < months; index += 1) {
    const value = `${year}-${String(month).padStart(2, '0')}`;
    out.push({ label: periodCodeLabel(value), value });
    month -= 1;
    if (month === 0) {
      month = 12;
      year -= 1;
    }
  }
  return out;
}

/**
 * 可结算的**季度**期间候选：本季往前 `quarters - 1` 个季度（新的排前面）。
 *
 * 本季不排除：季中结算（部分月份已开票）是真实场景，返回的基数就是「季度至今」。
 */
export function quarterPeriodOptions(
  now: Date = new Date(),
  quarters = 8,
): Array<{ label: string; value: string }> {
  let year = now.getFullYear();
  let quarter = Math.floor(now.getMonth() / 3) + 1;
  const out: Array<{ label: string; value: string }> = [];
  for (let index = 0; index < quarters; index += 1) {
    const value = `${year}-Q${quarter}`;
    out.push({ label: periodCodeLabel(value), value });
    quarter -= 1;
    if (quarter === 0) {
      quarter = 4;
      year -= 1;
    }
  }
  return out;
}

/** 期间候选（按粒度选一套） */
export function periodOptionsOf(
  settlePeriod: RebateSettlePeriod,
  now: Date = new Date(),
): Array<{ label: string; value: string }> {
  return settlePeriod === 'QUARTER' ? quarterPeriodOptions(now) : monthPeriodOptions(now);
}

// ==================== 展示 ====================

/** 金额展示（2 位千分位；负数原样带负号 = 净额被退货冲成负） */
export const moneyText = formatMoney;

/** 数量展示（去掉 `4.0000` 的尾随 0） */
export function qtyText(value: DecimalText | null | undefined): string {
  const num = toNumber(value);
  return String(Number(num.toFixed(4)));
}

/**
 * 客户展示：优先编码 + 名称；客户已软删时后端给 `null`（历史结算单仍要能读）。
 */
export function customerText(
  row: Pick<RebateSettlementRow, 'customer_code' | 'customer_name' | 'customer_id'>,
): string {
  const code = (row.customer_code ?? '').trim();
  const name = (row.customer_name ?? '').trim();
  if (code && name) {
    return `${code} ${name}`;
  }
  if (name) {
    return name;
  }
  if (code) {
    return code;
  }
  return row.customer_id ? `客户 ${row.customer_id.slice(0, 8)}…` : '—';
}

/** 「净销售额 / 净销量」列文案 */
export function baseCellText(base: RebateBaseNumbers | null | undefined): string {
  if (!base) {
    return '—';
  }
  return `${moneyText(base.net_amount)} / ${qtyText(base.net_qty)}`;
}

/**
 * 基数全量文案（详情用）：订单 − 退货 = 净额。
 *
 * 明细必须摊开：财务对账时要能自己验「净额是不是订单减退货」，只给一个净额没法验。
 */
export function baseDetailText(base: RebateBaseNumbers | null | undefined): string {
  if (!base) {
    return '—';
  }
  return `订单 ${base.order_count} 张 / ${moneyText(base.order_amount)}、${qtyText(base.order_qty)} 件 − 退货 ${base.return_count} 张 / ${moneyText(base.return_amount)}、${qtyText(base.return_qty)} 件 = 净额 ${moneyText(base.net_amount)}、净量 ${qtyText(base.net_qty)}`;
}

/** 净基数是否被退货冲成负（负数不产生负返利，后端按 0 落档） */
export function isNegativeBase(base: RebateBaseNumbers | null | undefined): boolean {
  if (!base) {
    return false;
  }
  return toNumber(base.net_amount) < 0 || toNumber(base.net_qty) < 0;
}

/** 费率文案（百分比带 `%`、元/件带单位） */
export function rateText(rate: DecimalText | null | undefined, unit: string | null | undefined): string {
  const raw = rate === null || rate === undefined || rate === '' ? '0' : String(rate);
  const num = Number(raw);
  const text = Number.isFinite(num) ? String(Number(num.toFixed(6))) : raw;
  return unit === 'PERCENT' ? `${text}%` : `${text} 元/件`;
}

/**
 * 「命中档 / 费率」列文案。
 *
 * - 命中：`第 2 档 1000 ~ 5000 ⇒ 2%`
 * - 未命中（`tier_seq` 为空，只可能发生在政策被绕过接口改歪时）：`未命中档位`
 */
export function tierHitText(
  row: Pick<RebateSettlementRow, 'tier_seq' | 'tier_from' | 'tier_to' | 'rate' | 'rate_unit'>,
): string {
  if (row.tier_seq === null || row.tier_seq === undefined) {
    return '未命中档位';
  }
  const from = qtyText(row.tier_from);
  const to = row.tier_to === null || row.tier_to === undefined ? '+∞' : qtyText(row.tier_to);
  return `第 ${row.tier_seq} 档 ${from} ~ ${to} ⇒ ${rateText(row.rate, row.rate_unit)}`;
}

/** 红字发票列文案（M3-17 写入；未开票时留一句「未开票」而不是空白） */
export function invoiceText(
  row: Pick<RebateSettlementRow, 'invoice_no' | 'invoice_id' | 'status'>,
): string {
  if (row.invoice_no) {
    return row.invoice_no;
  }
  if (row.invoice_id) {
    return `发票 ${row.invoice_id.slice(0, 8)}…`;
  }
  return row.status === 'DRAFT' ? '未开票（草稿）' : '—';
}

/** 转结时间文案（只显示到日） */
export function settledAtText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.slice(0, 10);
}

// ==================== 入参 ====================

/** 试算 / 计算 / 重算入参（`remark` 空则不填） */
export function buildSettleReq(
  customerId: string,
  settlePeriod: RebateSettlePeriod,
  periodCode: string,
  remark?: string,
): RebateSettleReq {
  const payload: RebateSettleReq = {
    customer_id: customerId.trim(),
    settle_period: settlePeriod,
    period_code: normalizePeriodCode(periodCode),
  };
  const trimmed = (remark ?? '').trim();
  if (trimmed) {
    payload.remark = trimmed;
  }
  return payload;
}

/** 整期跑批入参（`customer_ids` 空数组 = 后端按「该期间有政策的全部客户」跑） */
export function buildRunReq(
  settlePeriod: RebateSettlePeriod,
  periodCode: string,
  customerIds: string[] = [],
  remark?: string,
): RebateRunReq {
  const payload: RebateRunReq = {
    settle_period: settlePeriod,
    period_code: normalizePeriodCode(periodCode),
  };
  const ids = customerIds.map((id) => id.trim()).filter((id) => id.length > 0);
  if (ids.length > 0) {
    payload.customer_ids = ids;
  }
  const trimmed = (remark ?? '').trim();
  if (trimmed) {
    payload.remark = trimmed;
  }
  return payload;
}

// ==================== 试算结果 ====================

/**
 * 试算结论一句话（**先报钱，再报前提**）。
 *
 * 无政策时不要说「应返 0 元」就完 —— 那会让人以为「这期真的一分不返」，
 * 而真相是「这期没有政策可算」，去补政策才有钱。
 */
export function previewHeadline(preview: RebatePreview): string {
  const period = periodCodeLabel(preview.period_code);
  if (!preview.policy) {
    return `${period}：该期间没有覆盖整期的启用政策，无法算出返利（补政策后重算）`;
  }
  return `${period}：应返 ${moneyText(preview.rebate_amount)}（政策 ${preview.policy.policy_no}）`;
}

/** 试算提示逐条（后端 `warnings` 原文；没有就返回空数组） */
export function previewWarnings(preview: RebatePreview): string[] {
  return (preview.warnings ?? []).filter((item) => item.trim().length > 0);
}

/** 试算里政策档位快照的速览（弹窗里展开给人看「按什么规则算的」） */
export function previewTierLines(preview: RebatePreview): string[] {
  const tiers = preview.policy?.tiers ?? [];
  return tiers.map((tier) => {
    const to = tier.tier_to === null || tier.tier_to === undefined ? '+∞' : qtyText(tier.tier_to);
    return `第 ${tier.seq} 档 ${qtyText(tier.tier_from)} ~ ${to} ⇒ ${rateText(tier.rate, preview.policy?.rate_unit ?? '')}`;
  });
}

// ==================== 跑批结果 ====================

/** 跑批汇总文案（**成功 / 跳过 / 失败都要说**，只报成功会像接口挂了） */
export function runResultText(res: RebateRunRes): string {
  const period = periodCodeLabel(res.period_code);
  const parts = [`${period} 共 ${res.total} 家客户`];
  parts.push(`新建 ${res.created} 张`);
  if (res.skipped > 0) {
    parts.push(`已有跳过 ${res.skipped} 张`);
  }
  if (res.failed > 0) {
    parts.push(`失败 ${res.failed} 家`);
  }
  return parts.join('；');
}

/** 单客户结果文案（后端原文拼上，不自己编原因） */
export function runItemText(item: RebateRunItem): string {
  const who = item.customer_name
    ? `${item.customer_code ? `${item.customer_code} ` : ''}${item.customer_name}`
    : item.customer_id.slice(0, 8);
  if (item.outcome === 'CREATED') {
    return `${who}：新建 ${item.settlement_no ?? ''}，应返 ${moneyText(item.rebate_amount)}`;
  }
  if (item.outcome === 'EXISTS') {
    return `${who}：已有结算单 ${item.settlement_no ?? ''}（${item.status ?? ''}），跳过`;
  }
  return `${who}：失败 —— ${item.message ?? '后端未给原因'}`;
}

/** 跑批结果分组（失败的排前面：跑完第一眼要看的是「哪家挂了」） */
export function runResultGroups(res: RebateRunRes): {
  failed: RebateRunItem[];
  created: RebateRunItem[];
  skipped: RebateRunItem[];
} {
  const items = res.items ?? [];
  return {
    failed: items.filter((item) => item.outcome === 'FAILED'),
    created: items.filter((item) => item.outcome === 'CREATED'),
    skipped: items.filter((item) => item.outcome === 'EXISTS'),
  };
}

// ==================== 筛选 ====================

/** 空筛选 */
export function emptySettlementFilter(): RebateSettlementFilter {
  return {
    keyword: '',
    settle_period: '',
    period_code: '',
    status: '',
    page: 1,
    page_size: SETTLE_PAGE_SIZE,
  };
}

/** 是否带了筛选条件（决定空态文案） */
export function isSettlementFiltered(filter: RebateSettlementFilter): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() ||
      filter.settle_period ||
      (filter.period_code ?? '').trim() ||
      filter.status,
  );
}

/** 列表空态文案 */
export function settlementEmptyText(filter: RebateSettlementFilter): string {
  return isSettlementFiltered(filter)
    ? '没有匹配的结算单：换个期间 / 状态再查'
    : '还没有结算单：点右上角「整期跑批」按期间批量算，或「试算计算」针对单个客户';
}

/**
 * 当前页合计（**只汇总当前页，不是全量**）。
 *
 * 页面上必须写「本页合计」—— 把当前页当全量是最常见的一类误读（财务会拿它对账）。
 */
export function pageTotals(rows: RebateSettlementRow[]): {
  netAmount: number;
  rebateAmount: number;
  count: number;
} {
  let netAmount = 0;
  let rebateAmount = 0;
  for (const row of rows) {
    netAmount += toNumber(row.base?.net_amount);
    rebateAmount += toNumber(row.rebate_amount);
  }
  return { netAmount, rebateAmount, count: rows.length };
}
