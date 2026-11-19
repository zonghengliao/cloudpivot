/**
 * 差异分析页纯逻辑（M5-19）。
 *
 * 这里只放**不碰 DOM、不碰请求层、不重算任何公式**的东西，`pnpm test:unit`（`tsx --test`）直接钉住。
 * 页面（`index.vue`）只做「取数 + 渲染 + 把错误显示出来」。
 *
 * # 为什么本页几乎没有算数
 *
 * 差异的算数（标准应发生额 = 单位标准 × 完工数量、差异率、尾差对平、三要素拆分、整表合计）
 * 全在后端 `cost_standard_compare_dto::calc_of / totals_of` 里，并且有 43 条后端单测 + 真库用例
 * 钉着（M5-11/M5-17）。前端**再算一遍**只会制造两份口径：
 * 「页面显示 3,299.99、后端合计 3,300.00」这种差异查起来要人命。
 * 所以本文件只做四类事：
 *
 * ① **形状统一**：后端 4/6 位小数字符串 → 界面要的文字（千分位、去尾零、百分比）；
 * ② **口径转达**：`null` 差异率要说「标准额为 0，不计算」而不是画个 0%；
 * ③ **入参拼装**：期间必填（空期间**不发请求**，别让用户看到一整屏 400）；
 * ④ **话术**：`cost_ready=false`、合计为 0、行提示为空 … 每一处都要说清「为什么 + 下一步去哪」。
 *
 * # 复用 M5-18 的既有纯函数（同一套口径，别抄第二份）
 *
 * `isPeriodText` / `currentPeriod` / `windowText` / `materialLabel` / `trimZero` 来自
 * `../cost-standard/cost-standard-logic`；金额格式化走 `shared/money` 的整数分实现。
 */

import type {
  CostVarianceElement,
  CostVarianceQuery,
  CostVarianceRes,
  CostVarianceRow,
  CostVarianceStandardSide,
  CostVarianceTotals,
  CostVarianceVariance,
} from '@/api/business/erp/cost-variance-kinds';
import {
  DIRECTION,
  ELEMENT_ORDER,
  PAGE_SIZE,
  ROW_STATE,
  directionLabel,
  directionTheme,
  elementLabel,
  rowStateLabel,
} from '@/api/business/erp/cost-variance-kinds';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  currentPeriod,
  isPeriodText,
  materialLabel,
  trimZero,
  windowText,
} from '../cost-standard/cost-standard-logic';

export { PAGE_SIZE };

/** 本页筛选状态（`period_code` 必填；键名与接口入参 / ListPage 插槽一致）。 */
export interface VarianceFilter {
  /** 会计期间 `yyyy-MM`。 */
  period_code: string;
  /** 物料编码 / 名称关键字。 */
  keyword: string;
  /** 状态筛选 `all` / `comparable` / `no_standard` / `no_actual`。 */
  state: string;
}

/** 展开明细里的一条「标签 + 值」。 */
export interface Fact {
  /** 标签。 */
  label: string;
  /** 显示值（已格式化）。 */
  value: string;
  /** 语义色：不利红 / 有利绿（只给差异类条目用）。 */
  tone?: 'default' | 'danger' | 'success';
}

// ==================== 期间 ====================

/** 默认期间（本月）。差异分析必须锚定一个月，首屏就给它一个能跑的值。 */
export function defaultPeriod(now: Date = new Date()): string {
  return currentPeriod(now);
}

/** 期间是否可用于查询（`yyyy-MM` 形状）。 */
export function hasPeriod(filter: Pick<VarianceFilter, 'period_code'>): boolean {
  return isPeriodText(filter.period_code ?? '');
}

/** 期间为空时的引导话术（这时**不请求**，直接画这句话）。 */
export const PERIOD_REQUIRED_HINT =
  '先选会计期间：差异 = 实际 − 标准，必须锚定某一个月才谈得上对比（报表不猜「上个月」）。';

// ==================== 入参 ====================

/**
 * 筛选状态 → 接口入参（纯函数，单测钉住）。
 *
 * - 期间非法 ⇒ 返回 `null`（页面**不发请求**，显示 {@link PERIOD_REQUIRED_HINT}）；
 * - 关键字 trim 后为空 ⇒ 整键缺席（别发 `keyword=`：后端把它当「筛空」）；
 * - `state` 为空 / `all` 时**照发** `all`：后端缺省也是 `all`，显式发回显更直白（出参有 `state_filter`）。
 */
export function buildQuery(
  filter: VarianceFilter,
  page: number,
  pageSize: number = PAGE_SIZE,
): CostVarianceQuery | null {
  const period = (filter.period_code ?? '').trim();
  if (!isPeriodText(period)) {
    return null;
  }
  const query: CostVarianceQuery = {
    period_code: period,
    state: (filter.state ?? '').trim() || 'all',
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

/**
 * 金额 → `12,345.60`（千分位 + 2 位）。
 *
 * 后端金额是 4 位小数字符串；列表 / 汇总只给人看两位（与全仓 ERP 页面同口径，
 * 整数分实现见 `shared/money.ts`，避免浮点累加误差）。**要看原始 4 位就展开行**
 * （展开用 {@link exactText}）。
 */
export function moneyText(value: string | number | null | undefined): string {
  return formatMoney(value);
}

/** 数量 / 工时 → 去尾零 + 千分位（`800.0000` → `800`，`1234.5000` → `1,234.5`）。 */
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

/** 单价 / 单位成本 → 去尾零（`12.000000` → `12`，`17.166666` 原样）。 */
export function unitText(value: string | number | null | undefined): string {
  return trimZero(typeof value === 'number' ? String(value) : (value ?? '')) || '—';
}

/**
 * 差异率（**比值**字符串）→ 百分比文本。
 *
 * 后端 `rate` 是 `差异额 / 标准应发生额` 的原值（`"0.150000"` = 15%），
 * `null` = 标准额为 0、数学上不成立 —— 这时显示「—」并在提示里说明，
 * **不能**显示 `0.00%`（那会被读成「没有差异」）。
 */
export function rateText(rate: string | null | undefined): string {
  const raw = (rate ?? '').trim();
  if (!raw) {
    return '—';
  }
  const value = Number(raw);
  if (!Number.isFinite(value)) {
    return '—';
  }
  return `${(value * 100).toFixed(2)}%`;
}

/** 差异率不可计算时的说明（列表列 tooltip / 合计脚注用）。 */
export const RATE_UNDEFINED_HINT = '标准应发生额为 0，差异率不计算';

/**
 * 精确文本（**不丢位**）：`12345.6000` → `12,345.6`。
 *
 * 展开行与尾差用这个：尾差常是 `0.0001` 级别，套 2 位金额格式会被显示成 `0.00`，
 * 用户会以为「没有尾差」，而它恰恰是「对平用」的那一分钱。
 */
export function exactText(value: string | number | null | undefined): string {
  return qtyText(value);
}

// ==================== 要素（三类差异） ====================

/** 要素行（页面表格 / 展开行共用）。 */
export interface ElementRow {
  /** 要素字面值（行 key）。 */
  key: string;
  /** 要素中文。 */
  label: string;
  /** 单位标准（行内展开用；合计块为空串）。 */
  stdUnit: string;
  /** 单位实际（行内展开用；合计块为空串）。 */
  actUnit: string;
  /** 单位差异（行内展开用；合计块为空串）。 */
  unitAmount: string;
  /** 标准应发生额。 */
  stdAmount: string;
  /** 实际完工成本。 */
  actAmount: string;
  /** 差异额。 */
  amount: string;
  /** 差异率文本（不可算 ⇒ `—`）。 */
  rate: string;
  /** 方向字面值。 */
  direction: string;
  /** 方向中文。 */
  directionLabel: string;
  /** 语义色。 */
  tone: 'default' | 'danger' | 'success';
}

/**
 * 三要素 → 展示行（**固定按料 / 工 / 费排序**）。
 *
 * 后端顺序已经是料工费，但这里显式按 {@link ELEMENT_ORDER} 排一次并补上缺失要素：
 * 万一哪天后端只回两行，页面**不能**因此把「制造费用」那一行吞掉 ——
 * 「费」不见了，差异合计和页面上的三行就对不上。
 */
export function elementRows(elements: ReadonlyArray<CostVarianceElement> | null | undefined): ElementRow[] {
  const byKey = new Map<string, CostVarianceElement>();
  for (const element of elements ?? []) {
    byKey.set((element.element ?? '').toUpperCase(), element);
  }
  const keys = [...ELEMENT_ORDER];
  for (const key of byKey.keys()) {
    if (!keys.includes(key)) {
      keys.push(key);
    }
  }
  return keys.map((key) => {
    const element = byKey.get(key);
    // 后端没回这一要素 ⇒ 金额一律「—」，**不**补 0：补 0 会让「漏了一行」看起来像「没有差异」
    const missing = !element;
    return {
      key,
      label: element?.element_label || elementLabel(key),
      stdUnit: unitText(element?.std_unit),
      actUnit: unitText(element?.act_unit),
      unitAmount: element?.unit_amount ? exactText(element.unit_amount) : '—',
      stdAmount: missing ? '—' : moneyText(element.std_amount),
      actAmount: missing ? '—' : moneyText(element.act_amount),
      amount: missing ? '—' : moneyText(element.amount),
      rate: missing ? '—' : rateText(element.rate),
      direction: element?.direction ?? DIRECTION.EVEN,
      directionLabel: element?.direction_label || directionLabel(element?.direction),
      tone: directionTheme(element?.direction),
    };
  });
}

// ==================== 汇总（整表合计） ====================

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
 * 为什么强调「不累加」：报表是服务端分页的，前端手里只有当前页；
 * 拿当前页去加总出来的「合计」会在翻页时变，而财务会拿它跟凭证对。
 * 后端 `totals` 是**按筛选后全集**聚合的，翻页不变。
 */
export function summaryCards(res: CostVarianceRes | null): SummaryCard[] {
  const totals = res?.totals;
  if (!totals) {
    return [];
  }
  const tone = directionTheme(totals.direction);
  return [
    {
      key: 'rows',
      label: '物料行数',
      value: `${totals.rows}`,
      sub: `可比 ${totals.comparable_rows} · 缺标准 ${totals.no_standard_rows} · 缺实际 ${totals.no_actual_rows}`,
      tone: 'default',
    },
    {
      key: 'std',
      label: '标准应发生额',
      value: moneyText(totals.std_amount),
      sub: '单位标准成本 × 完工数量（可比行）',
      tone: 'default',
    },
    {
      key: 'act',
      label: '实际完工成本',
      value: moneyText(totals.act_amount),
      sub: '成本计算单的完工成本（可比行）',
      tone: 'default',
    },
    {
      key: 'amount',
      label: '差异额合计',
      value: moneyText(totals.amount),
      sub: `${totals.direction_label || directionLabel(totals.direction)}：正数 = 不利（超支）、负数 = 有利（节约）`,
      tone,
    },
    {
      key: 'rate',
      label: '差异率合计',
      value: rateText(totals.rate),
      sub: totals.rate ? '差异额合计 / 标准应发生额合计' : RATE_UNDEFINED_HINT,
      tone,
    },
    {
      key: 'gap',
      label: '不可比部分',
      value: moneyText(totals.no_standard_amount),
      sub: '缺标准行的实际完工成本（参考，不混进差异额）',
      tone: 'default',
    },
  ];
}

// ==================== 话术 ====================

/**
 * 实际侧未就绪时的引导（`cost_ready=false`）。
 *
 * 后端此时仍会列出标准侧（全部「缺实际」），页面**必须**说清原因与下一步，
 * 否则用户会以为「差异全是 0」是系统算坏了。
 */
export function scopeHintOf(res: CostVarianceRes | null): string {
  if (!res) {
    return '';
  }
  if (res.cost_ready) {
    return '';
  }
  return (
    `${res.period_code} 还没有成本核算批次：实际一侧为空，所有行都会显示「缺实际」，` +
    '差异自然也算不出来。先到「成本核算」页建该期间的批次并执行计算（POST /business/erp/cost-period/calc），再回来看差异。'
  );
}

/** 标准侧取数口径（后端固定文案 + 基准日，页面照抄，不改写）。 */
export function basisTextOf(res: CostVarianceRes | null): string {
  if (!res) {
    return '';
  }
  return `标准取「${res.standard_basis}」，本期基准日 ${res.period_end}（期末那一天覆盖的版本）`;
}

/**
 * 口径说明去 markdown 记号。
 *
 * 后端 `note` 里用 `**…**` 做强调（markdown 源码风格），直接塞进 `t-alert` 的纯文本 message
 * 会原样露出星号。这里只删强调记号，**不动口径原文**（口径是后端定的，前端不改写）。
 */
export function noteText(note: string | null | undefined): string {
  return (note ?? '')
    .replace(/\*\*/g, '')
    .replace(/[ \t]+/g, ' ')
    .replace(/\s*\n\s*/g, ' ')
    .trim();
}

/** 空态文案（区分「没选期间」与「选了但没有行」）。 */
export function emptyTextOf(res: CostVarianceRes | null, filter: VarianceFilter): string {
  if (!hasPeriod(filter)) {
    return PERIOD_REQUIRED_HINT;
  }
  if (!res) {
    return '还没有取到数据（点「查询」重试，或换一个期间）。';
  }
  const state = (res.state_filter || 'all').trim();
  if (state !== 'all') {
    return `${res.period_code} 没有符合「${
      state === 'comparable' ? '可比且有差异' : state === 'no_standard' ? '缺标准' : state === 'no_actual' ? '缺实际' : state
    }」的行 —— 换个筛选或清空关键字看看。`;
  }
  return (
    `${res.period_code} 既没有已发布的标准成本，也没有成本计算单：` +
    '先去「标准成本」页建一版并提交发布，同时到「成本核算」页跑该期间的计算。'
  );
}

// ==================== 行内展开 ====================

/** 物料列文本（编码 + 名称；物料被删只剩 id 前 8 位）。 */
export function rowMaterialText(row: CostVarianceRow): string {
  return materialLabel(row);
}

/** 规格（空 ⇒ `—`）。 */
export function specText(row: CostVarianceRow): string {
  const spec = (row.material_spec ?? '').trim();
  return spec || '—';
}

/** 标准版本一句话（`v2 · 2026-11-01 ~ 至今`；期内换版追加标记）。 */
export function versionText(standard: CostVarianceStandardSide): string {
  const base = `v${standard.version_no} · ${windowText(standard.effective_from, standard.effective_to)}`;
  return standard.switch_in_period ? `${base}（期内换版，按期末那版对比）` : base;
}

/**
 * 标准一侧的明细（展开行左半）。
 *
 * 三个要素的单位成本照抄后端 `std_*_cost`（**不**用行内 `variance.elements[].std_unit` 反推：
 * 缺实际的行没有 `variance`，但标准仍在，用户同样要能看见口径）。
 */
export function standardFacts(row: CostVarianceRow): Fact[] {
  const standard = row.standard;
  if (!standard) {
    return [];
  }
  return [
    { label: '版本', value: versionText(standard) },
    { label: '单位材料', value: unitText(standard.std_material_cost) },
    { label: '单位人工', value: unitText(standard.std_labor_cost) },
    { label: '单位制造费用', value: unitText(standard.std_overhead_cost) },
    { label: '单位标准总成本', value: unitText(standard.std_total_cost) },
    {
      label: '单位工时 / 工资率',
      value: `${qtyText(standard.std_labor_hours)} h @ ${unitText(standard.std_labor_rate)} /h`,
    },
  ];
}

/** 实际一侧的明细（展开行右半）。 */
export function actualFacts(row: CostVarianceRow): Fact[] {
  const actual = row.actual;
  if (!actual) {
    return [];
  }
  return [
    { label: '计算单行数', value: `${actual.object_count} 行` },
    { label: '完工数量', value: qtyText(actual.completed_qty) },
    { label: '账面完工成本', value: exactText(actual.book_amount) },
    { label: '实际材料', value: exactText(actual.amount_material) },
    { label: '实际人工', value: exactText(actual.amount_labor) },
    { label: '实际制造费用', value: exactText(actual.amount_overhead) },
    { label: '单位实际总成本', value: unitText(actual.unit_total) },
    { label: '本期投入（参考）', value: exactText(actual.period_input) },
    { label: '期末在制（参考）', value: exactText(actual.wip_end_cost) },
  ];
}

/**
 * 差异块的单位口径（展开行底部）。
 *
 * 尾差单独一条并写「对平用，不是差异」：它 = 账面完工成本 − Σ(单位成本 × 完工数量)，
 * 是四位小数取舍的产物，混进「差异」里会让人以为差了一分钱。
 */
export function varianceFacts(variance: CostVarianceVariance | null): Fact[] {
  if (!variance) {
    return [];
  }
  const tone = directionTheme(variance.direction);
  return [
    { label: '单位标准', value: unitText(variance.unit_std_amount) },
    { label: '单位实际', value: unitText(variance.unit_act_amount) },
    { label: '单位差异', value: exactText(variance.unit_amount), tone },
    { label: '尾差（对平用，不是差异）', value: exactText(variance.rounding_diff) },
  ];
}

/** 行的可展开内容是否存在（不可比的行也有标准 / 实际一侧，照样可展开）。 */
export function hasExpandableDetail(row: CostVarianceRow): boolean {
  return Boolean(row.standard || row.actual || row.variance);
}

/** 可比行的三要素（不可比 ⇒ 空数组，页面画「没法比」的原因）。 */
export function rowElements(row: CostVarianceRow): ElementRow[] {
  return row.variance ? elementRows(row.variance.elements) : [];
}

/** 行状态是否可比（页面据此决定要不要画差异列的三要素）。 */
export function isComparable(row: CostVarianceRow): boolean {
  return (row.state ?? '').trim() === ROW_STATE.BOTH;
}

/** 合计里有几行不可比（页面提醒「差异额只覆盖可比行」）。 */
export function incomparableCount(totals: CostVarianceTotals | null | undefined): number {
  if (!totals) {
    return 0;
  }
  return totals.no_standard_rows + totals.no_actual_rows;
}

/** 「共 N 行」后缀（把不可比行数说在同一个元素里，e2e 按 meta 断言时不会漏）。 */
export function metaSuffixOf(totals: CostVarianceTotals | null | undefined): string {
  const gap = incomparableCount(totals);
  if (!totals || totals.rows === 0) {
    return '';
  }
  return gap > 0 ? `（其中 ${gap} 行不可比，不进差异合计）` : '';
}

/** 状态文案（行状态中文；后端 `state_label` 优先）。 */
export function rowStateText(row: CostVarianceRow): string {
  return row.state_label || rowStateLabel(row.state);
}

// ==================== 列表列（紧凑形态） ====================

/** 三要素的短名（列表那种窄列写的：料 / 工 / 费）。 */
export const ELEMENT_SHORT: Readonly<Record<string, string>> = {
  MATERIAL: '料',
  LABOR: '工',
  OVERHEAD: '费',
};

/**
 * 行内三要素紧凑文本（列表「三类差异」列）：`料 600.00 · 工 -180.00 · 费 0.00`。
 *
 * 不可比行为 `—`（`variance` 为 `null`）：这时三要素**没有**意义，
 * 写 0 会让人以为「差异为零」，而真相是「缺一侧、根本没比」。
 */
export function elementsCompactText(row: CostVarianceRow): string {
  if (!row.variance) {
    return '—';
  }
  const rows = elementRows(row.variance.elements);
  if (rows.length === 0) {
    return '—';
  }
  return rows.map((row_) => `${ELEMENT_SHORT[row_.key] ?? row_.label} ${row_.amount}`).join(' · ');
}

/**
 * 完工数量（比较基数）：可比行取 `variance.completed_qty`，缺标准行取实际侧的
 * （表格里要能看见「这个料本期做了多少」，没有实际 ⇒ `—`）。
 */
export function completedQtyText(row: CostVarianceRow): string {
  const text = row.variance?.completed_qty ?? row.actual?.completed_qty ?? '';
  return qtyText(text);
}
