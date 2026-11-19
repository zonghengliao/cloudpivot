/**
 * 在制品页纯逻辑（M5-25）。
 *
 * 这里只放**不碰 DOM、不碰请求层、不重算任何业务公式**的东西，
 * `pnpm test:unit`（`tsx --test`）直接钉住；页面（`index.vue`）只做
 * 「取数 + 渲染 + 把错误显示出来」。
 *
 * # 为什么本页几乎没有算数
 *
 * 两条链的算术全在后端，且各有单测钉着：
 *
 * | 链 | 唯一实现 | 本页怎么用 |
 * |---|---|---|
 * | 工序加权综合完工率 | `cost_wip_pct_dto::rollup_of`（贡献 / 占比也在那） | 直接显示 `rollup.pct` / `rows[].contribution_pct` |
 * | 期末余额滚动 | `cost_wip_balance_dto::balance_of` + `carry_of` | 直接显示 `wip_total` / `elements[]` / `carry.state` |
 *
 * 前端**再算一遍**只会制造两份口径：「页面显示 350.01、后端余额 350.00」这种差异查起来要人命。
 * 所以本文件只做五类事：
 *
 * ① **形状统一**：字符串小数 → 界面要的文字（千分位、去尾零、百分比）；
 * ② **口径转达**：`null` 金额说「算不出来」而不是画 `0.00`；`carry.state` 八态判色；
 * ③ **入参拼装**：期间必填（空期间**不发请求**，别让用户看到一整屏 400）；
 * ④ **编辑态**：把后端行转成可编辑行、整表提交、按后端值域做**前置校验**（省一次 400 往返）；
 * ⑤ **话术**：`cost_ready=false`、已红冲、算不出来、没有工序 … 每一处都说清「为什么 + 下一步去哪」。
 */

import type {
  WipBalanceQuery,
  WipBalanceRes,
  WipBalanceRow,
  WipBalanceTotals,
  WipCarry,
  WipElementRoll,
  WipOperationRes,
  WipOperationRowReq,
  WipRollup,
} from '@/api/business/erp/cost-wip-kinds';
import {
  BALANCE_STATE_DEFAULT,
  BALANCE_STATE_FILTERS,
  CARRY_STATES,
  EDIT_STATE,
  MAX_ROWS,
  MAX_SEQ,
  NAME_MAX_CHARS,
  PAGE_SIZE,
  PCT_SCALE,
  REMARK_MAX_CHARS,
  WEIGHT_CEILING,
  WEIGHT_SCALE,
} from '@/api/business/erp/cost-wip-kinds';
import type { CostObjectPageQuery, CostObjectStatus } from '@/api/business/erp/cost-object-kinds';
import { formatMoney } from '@/views/business/erp/shared/money';
import { currentPeriod, isPeriodText, trimZero } from '../cost-standard/cost-standard-logic';

export { PAGE_SIZE };

// ==================== 筛选 / 入参 ====================

/** 完工率录入页签的列表筛选。 */
export interface ObjectFilter {
  /** 关键字：对象号 / 产出批号 / 上游订单号。 */
  keyword: string;
  /** 状态（缺省 `OPEN`：已关批的批改不了完工率，列表里先别混进来）。 */
  status: CostObjectStatus | '';
}

/** 余额页签的筛选（`period_code` 必填）。 */
export interface BalanceFilter {
  /** 会计期间 `yyyy-MM`。 */
  period_code: string;
  /** 对象号 / 批号 / 产出品编码 / 名称关键字。 */
  keyword: string;
  /** 结转状态筛选（缺省 `wip`，与后端一致）。 */
  state: string;
}

/** 默认期间（本月）：录入 / 余额都要锚定某一个月，首屏就给一个能跑的值。 */
export function defaultPeriod(now: Date = new Date()): string {
  return currentPeriod(now);
}

/** 期间是否可用于查询（`yyyy-MM` 形状）。 */
export function hasPeriod(filter: Pick<BalanceFilter, 'period_code'>): boolean {
  return isPeriodText(filter.period_code ?? '');
}

/** 录入页签的筛选初值。 */
export function defaultObjectFilter(): ObjectFilter {
  return { keyword: '', status: 'OPEN' };
}

/** 余额页签的筛选初值。 */
export function defaultBalanceFilter(now: Date = new Date()): BalanceFilter {
  return { period_code: defaultPeriod(now), keyword: '', state: BALANCE_STATE_DEFAULT };
}

/**
 * 录入页签 → 成本对象列表入参（纯函数）。
 *
 * 只做「trim + 去空键」：`status` 为空就是「全部状态」（整键缺席），
 * 不写 `status=''` —— 后端把空串当「等于空串」过滤会查出 0 行。
 */
export function buildObjectQuery(
  filter: ObjectFilter,
  page: number,
  pageSize: number = PAGE_SIZE,
): CostObjectPageQuery {
  const query: CostObjectPageQuery = {
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : PAGE_SIZE,
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  const status = (filter.status ?? '').trim();
  if (status) {
    query.status = status as CostObjectStatus;
  }
  return query;
}

/** 余额页签期间为空时的引导话术（这时**不请求**，直接画这句话）。 */
export const PERIOD_REQUIRED_HINT =
  '先选会计期间：期末在制余额是「截至某一期的存量」，必须锚定某一个月才谈得上余额（报表不猜「上个月」）。';

/**
 * 余额页签 → 接口入参（纯函数，单测钉住）。
 *
 * - 期间非法 ⇒ 返回 `null`（页面**不发请求**，显示 {@link PERIOD_REQUIRED_HINT}）；
 * - 关键字 trim 后为空 ⇒ 整键缺席（backend 把 `keyword=` 当「筛空」）；
 * - `state` 照发（缺省 `wip`）：后端缺省也是 `wip`，显式发回显更直白（出参有 `state`）。
 */
export function buildBalanceQuery(
  filter: BalanceFilter,
  page: number,
  pageSize: number = PAGE_SIZE,
): WipBalanceQuery | null {
  const period = (filter.period_code ?? '').trim();
  if (!isPeriodText(period)) {
    return null;
  }
  const query: WipBalanceQuery = {
    period_code: period,
    state: (filter.state ?? '').trim() || BALANCE_STATE_DEFAULT,
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : PAGE_SIZE,
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

// ==================== 数值 → 文字 ====================

/** 金额 → `12,345.60`（千分位 + 2 位，整数分实现见 `shared/money.ts`）。 */
export function moneyText(value: string | number | null | undefined): string {
  return formatMoney(value);
}

/** 数量 → 去尾零 + 千分位（`40.0000` → `40`，`1234.5000` → `1,234.5`）。 */
export function qtyText(value: string | number | null | undefined): string {
  const text = trimZero(typeof value === 'number' ? String(value) : (value ?? ''));
  if (!/^-?\d+(\.\d+)?$/.test(text)) {
    return text || '—';
  }
  const negative = text.startsWith('-');
  const body = negative ? text.slice(1) : text;
  const [intPart, fracPart] = body.split('.');
  const grouped = (intPart ?? '0').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${negative ? '-' : ''}${grouped}${fracPart ? `.${fracPart}` : ''}`;
}

/**
 * 精确文本（**不丢位**）：`413.1584` → `413.1584`。
 *
 * 展开行与尾差用这个：尾差常是 `0.0001` 级别，套 2 位金额格式会被显示成 `0.00`，
 * 用户会以为「没有尾差」，而它恰恰是「对平用」的那一分钱。
 */
export function exactText(value: string | number | null | undefined): string {
  return qtyText(value);
}

/**
 * 完工率 / 差额 → 百分比文本（`"50"` → `50%`，`"22.2300"` → `22.23%`）。
 *
 * 后端给的是**百分数本身**（不是比值）：`completion_pct: "50.00"` 就是 50%，
 * 所以这里**不乘 100**（与 `cost-variance-logic::rateText` 相反 —— 那是比值）。
 */
export function pctText(value: string | number | null | undefined): string {
  const text = trimZero(typeof value === 'number' ? String(value) : (value ?? ''));
  if (!text) {
    return '—';
  }
  return `${text}%`;
}

/** 人民币金额 + `null` 说人话（`null` 是「算不出来」，不是 0）。 */
export function amountOrDash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? '—' : moneyText(value);
}

/** 数量 + `null` 说人话。 */
export function qtyOrDash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? '—' : qtyText(value);
}

// ==================== 结转八态 ====================

/** 结转状态定义（未知值回落 `blocked` 的形态，但 `label` 原样保留后端给的）。 */
export function carryStateDef(state: string | null | undefined): (typeof CARRY_STATES)[number] {
  const key = (state ?? '').trim();
  return CARRY_STATES.find((item) => item.value === key) ?? CARRY_STATES[CARRY_STATES.length - 1];
}

/** 结转状态 → tag 主题（已结转绿 / 结转不齐红 / 其余需要处理的黄）。 */
export function carryTheme(state: string | null | undefined): 'success' | 'warning' | 'danger' | 'default' | 'primary' {
  return carryStateDef(state).theme;
}

/** 结转状态中文（后端 `label` 优先，缺了按字面值查表）。 */
export function carryText(carry: WipCarry | null | undefined): string {
  if (!carry) {
    return '—';
  }
  return carry.label || carryStateDef(carry.state).label;
}

/** 这一行是否要标红（后端 `needs_attention` 是唯一判据，前端不自己推）。 */
export function needsAttention(carry: WipCarry | null | undefined): boolean {
  return Boolean(carry?.needs_attention);
}

// ==================== 汇总卡片（余额页签） ====================

/** 汇总卡片的一条。 */
export interface SummaryCard {
  /** 卡片 key（同时作 testid 后缀）。 */
  key: string;
  /** 标题。 */
  label: string;
  /** 主值。 */
  value: string;
  /** 副标题（口径 / 计数说明）。 */
  sub: string;
  /** 主值语义色。 */
  tone: 'default' | 'danger' | 'success' | 'primary';
}

/**
 * 整表合计 → 汇总卡片（**直接用后端 `totals`，前端不累加**）。
 *
 * 八张卡：行数 / 期初 / 本期投入 / 完工转出 / 期末余额 / 按要素 / 未入账 / 需处理。
 * 「未入账」单列一档，因为它是这张表里最危险的一类（看着有余额、下一期取不到），
 * 混进「期末余额」里就再也分不出来了。
 */
export function summaryCards(res: WipBalanceRes | null): SummaryCard[] {
  const totals = res?.totals;
  if (!totals) {
    return [];
  }
  const unbooked = Number(totals.unbooked_total ?? '0');
  const attention = attentionCount(totals);
  return [
    {
      key: 'rows',
      label: '行数',
      value: `${totals.row_count}`,
      sub: `有余额 ${totals.wip_row_count} · 已结转 ${totals.carried_count} · 待结转 ${totals.pending_count}`,
      tone: 'default',
    },
    {
      key: 'beg',
      label: '期初在制',
      value: moneyText(totals.beg_total),
      sub: `数量 ${qtyText(totals.beg_qty)}（上期已结算计算单的期末）`,
      tone: 'default',
    },
    {
      key: 'cur',
      label: '本期投入',
      value: moneyText(totals.cur_total),
      sub: '本期归集的料 / 工 / 费（biz_cost_collect）',
      tone: 'default',
    },
    {
      key: 'finished',
      label: '完工转出',
      value: moneyText(totals.finished_total),
      sub: '本期完工部分转出的成本',
      tone: 'default',
    },
    {
      key: 'wip',
      label: '期末在制余额',
      value: moneyText(totals.wip_total),
      sub: `数量 ${qtyText(totals.wip_qty)}（期初 + 投入 − 转出，差额倒挤）`,
      tone: 'primary',
    },
    {
      key: 'elements',
      label: '按要素（料 / 工 / 费）',
      value: moneyText(totals.wip_material),
      sub: `工 ${moneyText(totals.wip_labor)} · 费 ${moneyText(totals.wip_overhead)}（${totals.element_row_count} 行可分要素）`,
      tone: 'default',
    },
    {
      key: 'unbooked',
      label: '未入账余额',
      value: moneyText(totals.unbooked_total),
      sub: `试算有、账面没有：${totals.unsettled_count} 行（下期取不到，必须入账）`,
      tone: unbooked > 0 ? 'danger' : 'default',
    },
    {
      key: 'attention',
      label: '需要处理的行',
      value: `${attention}`,
      sub:
        `结转不齐 ${totals.mismatch_count} · 关批挂账 ${totals.closed_wip_count} · ` +
        `已红冲 ${totals.reversed_count} · 算不出 ${totals.blocked_count}`,
      tone: attention > 0 ? 'danger' : 'success',
    },
  ];
}

/** 需要人去处理的行数（后端五档之和；`cleared` / `carried` / `pending` 不算）。 */
export function attentionCount(totals: WipBalanceTotals | null | undefined): number {
  if (!totals) {
    return 0;
  }
  return (
    totals.mismatch_count +
    totals.unsettled_count +
    totals.reversed_count +
    totals.closed_wip_count +
    totals.blocked_count
  );
}

// ==================== 行内展开（余额页签） ====================

/** 展开行里的一条「标签 + 值」。 */
export interface Fact {
  /** 标签。 */
  label: string;
  /** 显示值（已格式化）。 */
  value: string;
  /** 语义色：需要处理红 / 正常绿（只给关键条目用）。 */
  tone?: 'default' | 'danger' | 'success';
}

/** 一行的三要素滚动（固定料 / 工 / 费三行，缺的补占位而不补 0）。 */
export interface ElementLine {
  /** 要素码（行 key）。 */
  key: string;
  /** 中文名。 */
  label: string;
  /** 期初。 */
  beg: string;
  /** 本期投入。 */
  cur: string;
  /** 完工转出。 */
  finished: string;
  /** 期末余额。 */
  wip: string;
}

/**
 * 三要素 → 展示行（固定顺序，缺的补占位）。
 *
 * 后端已经是料 / 工 / 费顺序，这里仍显式按固定顺序取一次：
 * 万一哪天后端只回两行，页面**不能**因此把「制造费用」吞掉 ——
 * 「费」不见了，三要素加总就和余额对不上。
 */
export function elementLines(elements: ReadonlyArray<WipElementRoll> | null | undefined): ElementLine[] {
  const order: Array<{ key: string; label: string }> = [
    { key: 'MATERIAL', label: '材料' },
    { key: 'LABOR', label: '人工' },
    { key: 'OVERHEAD', label: '制造费用' },
  ];
  const byKey = new Map<string, WipElementRoll>();
  for (const element of elements ?? []) {
    byKey.set((element.element ?? '').toUpperCase(), element);
  }
  return order.map(({ key, label }) => {
    const element = byKey.get(key);
    return {
      key,
      label: element?.label || label,
      beg: amountOrDash(element?.beg ?? null),
      cur: amountOrDash(element?.cur ?? null),
      finished: amountOrDash(element?.finished ?? null),
      wip: amountOrDash(element?.wip ?? null),
    };
  });
}

/** 分要素不可用时的说明（已结算行的试算与账面不一致时，后端把三要素清空）。 */
export const ELEMENTS_UNAVAILABLE_HINT =
  '分要素不可用：该行已结算，但试算与账面不一致（算了之后又改过数量 / 完工率 / 归集）。' +
  '按试算拆出来的三要素与账面余额加不齐，所以后端不给这三列。修法是红冲本期重算（已结算的单不能直接重算）。';

/** 账户快照 / 结转核对 → 展开行的「账面与结转」事实列表。 */
export function ledgerFacts(row: WipBalanceRow): Fact[] {
  const facts: Fact[] = [];
  if (row.ledger) {
    facts.push({
      label: '账面期末',
      value: `数量 ${qtyText(row.ledger.wip_end_qty)} · 成本 ${moneyText(row.ledger.wip_end_cost)}（${row.ledger.status}）`,
    });
    facts.push({
      label: '试算与账面',
      value: row.ledger.agrees ? '一致' : '不一致（分要素已隐藏）',
      tone: row.ledger.agrees ? 'success' : 'danger',
    });
  } else {
    facts.push({ label: '账面期末', value: '本期没有已结算计算单（余额是试算值，未入账）', tone: 'danger' });
  }
  const carry = row.carry;
  facts.push({ label: '结转状态', value: carryText(carry), tone: needsAttention(carry) ? 'danger' : 'success' });
  if (carry.to_period) {
    facts.push({ label: '下期期间', value: carry.to_period });
  }
  if (carry.to_beg_qty !== null || carry.to_beg_cost !== null) {
    facts.push({
      label: '下期取走',
      value: `数量 ${qtyOrDash(carry.to_beg_qty)} · 成本 ${amountOrDash(carry.to_beg_cost)}`,
    });
  }
  if (carry.diff_qty !== null || carry.diff_cost !== null) {
    facts.push({
      label: '差额（下期 − 本期账面）',
      value: `数量 ${qtyOrDash(carry.diff_qty)} · 成本 ${amountOrDash(carry.diff_cost)}`,
      tone: 'danger',
    });
  }
  return facts;
}

/** 一行的数量口径（展开行顶部）。 */
export function quantityFacts(row: WipBalanceRow): Fact[] {
  return [
    { label: '计划数量', value: qtyText(row.plan_qty) },
    { label: '累计完工', value: qtyText(row.cumulative_completed_qty) },
    { label: '本期完工', value: qtyText(row.completed_qty) },
    {
      label: '期初在制',
      value: `${qtyText(row.beg_qty)}${row.beg_period ? `（${row.beg_period}）` : '（首期，无上期）'}`,
    },
    { label: '期末在制', value: qtyText(row.wip_qty) },
    { label: '完工率', value: pctText(row.completion_pct) },
    { label: '口径来源', value: row.source_label || row.source },
  ];
}

/** 算不出来的原因（`issues[]` 是「事实 + 怎么修」，整段展示，不截断）。 */
export function issueTexts(row: WipBalanceRow): string[] {
  return (row.issues ?? []).map((issue) => `${issue.message}（怎么修：${issue.fix}）`);
}

/** 该行的最终提示：先 `carry.hint`，再逐条 `issues`，最后 `warnings`。 */
export function rowHints(row: WipBalanceRow): string[] {
  const hints: string[] = [];
  if (row.carry?.hint) {
    hints.push(row.carry.hint);
  }
  hints.push(...issueTexts(row));
  hints.push(...(row.warnings ?? []));
  return hints;
}

/** 行的金额列颜色：算不出来 / 需要处理 → 红，正常 → 默认。 */
export function wipTone(row: WipBalanceRow): 'default' | 'danger' | 'success' {
  if (row.carry?.state === 'blocked' || row.wip_total === null) {
    return 'danger';
  }
  if (needsAttention(row.carry)) {
    return 'danger';
  }
  return row.carry?.state === 'carried' ? 'success' : 'default';
}

/** 物料 / 产出品列文本（编码 + 名称；主数据删了只剩 id 前 8 位）。 */
export function productText(row: WipBalanceRow): string {
  const code = (row.product_code ?? '').trim();
  const name = (row.product_name ?? '').trim();
  if (code && name) {
    return `${code} ${name}`;
  }
  return code || name || row.product_id.slice(0, 8);
}

// ==================== 话术（余额页签） ====================

/**
 * 顶部引导条（`cost_ready=false` / 已红冲）。
 *
 * 两种都不是「算坏了」，页面必须说清原因与下一步：
 * - 没有活跃批次：本期还没做成本核算，表里的余额是**试算视图**；
 * - 批次已红冲：账面余额作废，所有行按「已红冲」处理，重算后再看。
 */
export function scopeHintOf(res: WipBalanceRes | null): string {
  if (!res) {
    return '';
  }
  if (res.reversed_batch) {
    return (
      `${res.period_code} 的成本批次 ${res.reversed_batch.batch_no} 已红冲：账面余额作废，本表所有行按「已红冲」处理。` +
      '重跑该期间的成本核算后再回来看余额。'
    );
  }
  if (!res.cost_ready) {
    return (
      `${res.period_code} 还没有成本核算批次：余额是试算视图，没有已结算快照，下一期的期初取不到这笔钱。` +
      '先到「成本核算」页建该期间的批次并执行计算（POST /business/erp/cost-period/calc），再回来看余额。'
    );
  }
  return '';
}

/**
 * 口径说明去 markdown 记号。
 *
 * 后端 `note` / `formula_*` 里用 `**…**` 与反引号做强调（markdown 源码风格），
 * 直接塞进 `t-alert` 的纯文本会原样露出记号。这里只删记号，
 * **不动口径原文**（口径是后端定的，前端不改写）。
 */
export function noteText(note: string | null | undefined): string {
  return (note ?? '')
    .replace(/\*\*/g, '')
    .replace(/`/g, '')
    .replace(/[ \t]+/g, ' ')
    .replace(/\s*\n\s*/g, ' ')
    .trim();
}

/** 空态文案（区分「没选期间」「筛选没命中」「这个月什么都没有」）。 */
export function emptyTextOf(res: WipBalanceRes | null, filter: BalanceFilter): string {
  if (!hasPeriod(filter)) {
    return PERIOD_REQUIRED_HINT;
  }
  if (!res) {
    return '还没有取到数据（点「查询」重试，或换一个期间）。';
  }
  const state = (res.state || BALANCE_STATE_DEFAULT).trim();
  if (state !== 'all' && state !== BALANCE_STATE_DEFAULT) {
    const label = stateLabelOf(state);
    return `${res.period_code} 没有「${label}」的行 —— 换成「全部」或清空关键字看看（没有行不代表没有余额，可能是筛选把它藏了）。`;
  }
  return (
    `${res.period_code} 没有在制余额行：或本期没有成本对象投产，或所有批都已全部完工（期末在制为 0）。` +
    '先到「成本对象」页确认本期有在产批次，并到「成本核算」页跑该期间的计算。'
  );
}

/** 状态筛选值 → 中文（**从 `BALANCE_STATE_FILTERS` 派生**：两处各写一份必然漂移）。 */
function stateLabelOf(state: string): string {
  return BALANCE_STATE_FILTERS.find((option) => option.value === state)?.label ?? state;
}

// ==================== 完工率录入（M5-21） ====================

/** 编辑中的一行（全部是字符串：用户输入什么就是什么，提交前统一规整）。 */
export interface EditorRow {
  /** 行内唯一 key（前端用；不进 payload）。 */
  key: string;
  /** 工序序号（字符串，提交前转整数）。 */
  seq: string;
  /** 工序名。 */
  operation_name: string;
  /** 权重（字符串；空 = 用后端缺省 1）。 */
  weight: string;
  /** 本工序完工率（字符串，**必填**）。 */
  completion_pct: string;
  /** 备注。 */
  remark: string;
}

/** 一行的字段级错误（键 = 字段名，值 = 人话）。 */
export interface EditorRowErrors {
  /** 序号错误。 */
  seq?: string;
  /** 工序名错误。 */
  operation_name?: string;
  /** 权重错误。 */
  weight?: string;
  /** 完工率错误。 */
  completion_pct?: string;
  /** 备注错误。 */
  remark?: string;
}

/** 校验结果。 */
export interface EditorValidation {
  /** 是否可提交。 */
  ok: boolean;
  /** 按行索引（0 基）的字段错误。 */
  rows: Record<number, EditorRowErrors>;
  /** 全局错误（行数上限 / 序号重复这类说不出「哪一格」的）。 */
  messages: string[];
}

let editorKeySeq = 0;

/** 生成行 key（仅前端用，避免 `v-for` 用索引导致输入框丢焦点）。 */
function nextKey(): string {
  editorKeySeq += 1;
  return `wip-row-${editorKeySeq}`;
}

/** 后端行 → 编辑行（字符串原样搬，**不改标度**：`"2.0000"` 就是 `"2.0000"`）。 */
export function editorRowsFromRes(res: WipOperationRes | null): EditorRow[] {
  return (res?.rows ?? []).map((row) => ({
    key: nextKey(),
    seq: String(row.seq),
    operation_name: row.operation_name,
    weight: row.weight,
    completion_pct: row.completion_pct,
    remark: row.remark ?? '',
  }));
}

/** 新增一行的缺省序号（现有最大 `seq` + 1，封顶 `MAX_SEQ`）。 */
export function nextSeq(rows: ReadonlyArray<EditorRow>): number {
  let max = 0;
  for (const row of rows) {
    const seq = Number((row.seq ?? '').trim());
    if (Number.isInteger(seq) && seq > max) {
      max = seq;
    }
  }
  return Math.min(max + 1, MAX_SEQ);
}

/** 追加一行（完工率预填 0 并**要求用户改**：后端不接受缺值，预填空串会让人以为「不用填」）。 */
export function appendRow(rows: EditorRow[]): EditorRow {
  const row: EditorRow = {
    key: nextKey(),
    seq: String(nextSeq(rows)),
    operation_name: '',
    weight: '1',
    completion_pct: '0',
    remark: '',
  };
  rows.push(row);
  return row;
}

/** 删一行（整表覆盖：删了就提交时不带它 ⇒ 后端软删）。 */
export function removeRow(rows: EditorRow[], key: string): void {
  const index = rows.findIndex((row) => row.key === key);
  if (index >= 0) {
    rows.splice(index, 1);
  }
}

/** 小数位数（`"1.2300"` → 4；`"2"` → 0；非数字 → `null`）。 */
export function decimalsOf(text: string): number | null {
  const raw = (text ?? '').trim();
  if (!/^-?\d+(\.\d+)?$/.test(raw)) {
    return null;
  }
  const dot = raw.indexOf('.');
  return dot < 0 ? 0 : raw.length - dot - 1;
}

/**
 * 校验编辑中的整表（**前置校验，省一次 400 往返**）。
 *
 * 值域逐条对应后端 `cost_wip_pct_dto::specs_of` 与库层 CHECK；
 * 文案也照后端的意思写（点名哪一行哪一格），但不替代后端 —— 真正的权威仍是后端 400。
 */
export function validateRows(rows: ReadonlyArray<EditorRow>): EditorValidation {
  const errors: Record<number, EditorRowErrors> = {};
  const messages: string[] = [];
  if (rows.length > MAX_ROWS) {
    messages.push(`一次最多 ${MAX_ROWS} 道工序（当前 ${rows.length} 行）：再多就不是工序，是 BOM 明细了`);
  }
  const seen = new Map<number, number>();
  rows.forEach((row, index) => {
    const rowErrors: EditorRowErrors = {};
    const at = index + 1;

    const seqText = (row.seq ?? '').trim();
    const seq = Number(seqText);
    if (!seqText) {
      rowErrors.seq = '必填';
    } else if (!Number.isInteger(seq) || seq < 1 || seq > MAX_SEQ) {
      rowErrors.seq = `必须是 1–${MAX_SEQ} 的整数`;
    } else if (seen.has(seq)) {
      rowErrors.seq = `与第 ${(seen.get(seq) ?? 0) + 1} 行重复（同一批内序号唯一）`;
    } else {
      seen.set(seq, index);
    }

    const name = (row.operation_name ?? '').trim();
    if (!name) {
      rowErrors.operation_name = '必填';
    } else if (name.length > NAME_MAX_CHARS) {
      rowErrors.operation_name = `最多 ${NAME_MAX_CHARS} 字（当前 ${name.length}）`;
    }

    const weightText = (row.weight ?? '').trim();
    if (weightText) {
      const weight = Number(weightText);
      const decimals = decimalsOf(weightText);
      if (!Number.isFinite(weight)) {
        rowErrors.weight = '必须是数字';
      } else if (weight <= 0) {
        rowErrors.weight = '必须 > 0（综合完工率的分母是权重合计）';
      } else if (decimals !== null && decimals > WEIGHT_SCALE) {
        rowErrors.weight = `最多 ${WEIGHT_SCALE} 位小数`;
      } else if (weight > Number(WEIGHT_CEILING)) {
        rowErrors.weight = `不能超过 ${WEIGHT_CEILING}`;
      }
    }

    const pctTextValue = (row.completion_pct ?? '').trim();
    if (!pctTextValue) {
      rowErrors.completion_pct = '必填（0–100，不会当成 0 提交）';
    } else {
      const pct = Number(pctTextValue);
      const decimals = decimalsOf(pctTextValue);
      if (!Number.isFinite(pct)) {
        rowErrors.completion_pct = '必须是数字';
      } else if (pct < 0 || pct > 100) {
        rowErrors.completion_pct = '必须在 0–100 之间';
      } else if (decimals !== null && decimals > PCT_SCALE) {
        rowErrors.completion_pct = `最多 ${PCT_SCALE} 位小数`;
      }
    }

    if ((row.remark ?? '').length > REMARK_MAX_CHARS) {
      rowErrors.remark = `最多 ${REMARK_MAX_CHARS} 字（当前 ${(row.remark ?? '').length}）`;
    }

    if (Object.keys(rowErrors).length > 0) {
      errors[index] = rowErrors;
      messages.push(`第 ${at} 行（seq ${seqText || '空'}）填写有误`);
    }
  });
  return { ok: messages.length === 0, rows: errors, messages };
}

/**
 * 编辑行 → 保存入参（**看得见的每一行都提交**）。
 *
 * 三个细节：
 * 1. `seq` 转整数（后端要 `i32`）；`weight` 空 ⇒ **整键不传**（后端缺省 1）；
 * 2. `remark` 空 ⇒ `null`（后端 `clean_optional` 也这么收，但显式给更清楚）；
 * 3. 顺序无关（后端按 `seq` 重排），这里仍按界面顺序发，报文读起来顺眼。
 *
 * ⚠️ 空表也是合法入参（= 清空该批全部工序，后端软删；对象级完工率回到「沿用最近一次手填」）。
 */
export function toSavePayload(rows: ReadonlyArray<EditorRow>): { rows: WipOperationRowReq[] } {
  return {
    rows: rows.map((row) => {
      const weight = (row.weight ?? '').trim();
      const remark = (row.remark ?? '').trim();
      const req: WipOperationRowReq = {
        seq: Number((row.seq ?? '').trim()),
        operation_name: (row.operation_name ?? '').trim(),
        completion_pct: (row.completion_pct ?? '').trim(),
      };
      if (weight) {
        req.weight = weight;
      }
      if (remark) {
        req.remark = remark;
      } else {
        req.remark = null;
      }
      return req;
    }),
  };
}

/** 当前能不能保存（录入闸由后端给，前端不自己判状态）。 */
export function canSave(res: WipOperationRes | null): boolean {
  return Boolean(res && res.state === EDIT_STATE.EDITABLE && res.object.editable);
}

/** 录入闸锁住时的说明（后端 `blocked_reason` 原文 + 出路）。 */
export function lockedHint(res: WipOperationRes | null): string {
  if (!res) {
    return '';
  }
  if (canSave(res)) {
    return '';
  }
  return res.object.blocked_reason || '该批当前不可录工序完工率（只有「在产」且未结算的批能改）';
}

/** 汇总一句话（抽屉顶部；`rollup` 为 `null` 时说明「没有工序」）。 */
export function rollupText(res: WipOperationRes | null): string {
  if (!res) {
    return '—';
  }
  if (!res.rollup) {
    return '还没有工序：对象级完工率沿用最近一次「填写本期产量与在制」手填的值';
  }
  const rollup: WipRollup = res.rollup;
  return `${rollup.row_count} 道工序 · Σ权重 ${qtyText(rollup.weight_sum)} · 综合完工率 ${pctText(rollup.pct)}`;
}

/** 汇总与对象级是否一致（`false` 时后端会给一条提醒，这里只负责文案与判色）。 */
export function rollupMatched(rollup: WipRollup | null | undefined): boolean {
  return rollup ? rollup.matched : true;
}

/**
 * 「界面有改动、汇总还是老值」的提示。
 *
 * 页面**不预览**未保存的综合完工率：加权公式只在后端（`rollup_of`）实现一次，
 * 前端再算一遍就会出现「预览 50.00%、保存后 50.01%」这种谁都解释不清的差。
 * 所以脏了就明确说「下面的汇总是上次保存的值」。
 */
export const DIRTY_ROLLUP_HINT =
  '表格已改动，下面的汇总是**上次保存**的值。点「保存」后由后端重算并回写对象级完工率（前端不预览，避免两处公式打架）。';

/** 保存成功的一句话（说清插 / 改 / 删了几个）。 */
export function savedText(res: WipOperationRes | null): string {
  if (!res || !res.changed) {
    return '';
  }
  return `已保存：新增 ${res.inserted} 行 · 更新 ${res.updated} 行 · 删除 ${res.deleted} 行`;
}

/** 表单级错误 → 人话（409 = 业务状态不允许，补一句「去哪修」）。 */
export function saveErrorText(error: unknown): string {
  const message = errorTextOf(error);
  if (statusOf(error) === 409) {
    return `${message}（怎么修：已关批的批要复工请新建批；已结算期间要先到「成本核算」把该期间的成本批次红冲）`;
  }
  return message;
}

/** `error.message` / `error.msg` 兜底。 */
function errorTextOf(error: unknown): string {
  if (typeof error === 'string') {
    return error.trim() || '保存失败，请重试';
  }
  if (error && typeof error === 'object') {
    const like = error as { message?: string; msg?: string };
    const message = (like.message ?? like.msg ?? '').trim();
    if (message) {
      return message;
    }
  }
  return '保存失败，请重试';
}

/** 从错误对象里取 HTTP 状态（请求层透传 `status` / `statusCode`，取不到给 0）。 */
function statusOf(error: unknown): number {
  if (error && typeof error === 'object') {
    const like = error as { status?: number; statusCode?: number; code?: number };
    for (const value of [like.status, like.statusCode]) {
      if (typeof value === 'number' && value >= 100) {
        return value;
      }
    }
  }
  return 0;
}

/** 行的贡献 / 占比文本（`share_pct` / `contribution_pct` 都是后端算好的百分点）。 */
export function shareText(row: { share_pct: string }): string {
  return pctText(row.share_pct);
}

/** 行的贡献文本。 */
export function contributionText(row: { contribution_pct: string }): string {
  return pctText(row.contribution_pct);
}

/** 录入页签的空态文案。 */
export function objectEmptyText(filter: ObjectFilter): string {
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    return `没有匹配「${keyword}」的成本对象 —— 换个关键字，或把状态切到「全部」。`;
  }
  if ((filter.status ?? '') === 'OPEN') {
    return '本期没有「在产」的成本对象：先到「成本对象」页建批（或把状态切到「已关批」查看历史批的工序）。';
  }
  return '没有成本对象：先到「成本对象」页建批。';
}
