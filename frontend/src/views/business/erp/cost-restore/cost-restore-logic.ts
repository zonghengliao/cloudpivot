/**
 * 成本还原页纯逻辑（M5-30）。
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
 * | 逐层卷积（半成品 → 料工费） | `cost_restore_rollup_dto::restore_of`（M5-27） | 直接显示 `after` / `delta` / `flows[]` / `semis[]` |
 * | 分组汇总 + 料工费占比 | `cost_restore_report_dto::aggregate` / `group_view`（M5-28） | 直接显示 `restored.*_share` / `shift.*_pp` / `share_sum` |
 *
 * 前端**再算一遍**只会制造两份口径：「页面显示 62.51%、后端 62.50%」这种差异查起来要人命。
 * 所以本文件只做五类事：
 *
 * ① **形状统一**：字符串小数 → 界面要的文字（千分位、去尾零、百分比、带符号百分点）；
 * ② **口径转达**：三档状态判色；`null` 占比说「分母为 0」而不是画 `0.00%`；
 *    `item = null` 说「还没算成本」而不是画 `0`；
 * ③ **入参拼装**：期间必填（空期间**不发请求**，别让用户看到一整屏 400）；层数夹到 1–12；
 * ④ **自检展示**：逐层台账守恒、报表 `share_sum ≈ 100.00`、`check = 0` ——
 *    这些都是**后端已经给的值**，前端只做「跟目标比一下」的判色，不重算金额；
 * ⑤ **话术**：`cost_ready=false`、还原不彻底、层数被夹、占比舍入差 … 每一处都说清
 *    「为什么 + 下一步去哪」。
 *
 * # 三件本页**不做**的事（做了就是两处口径）
 *
 * * 不把 `flows[]` 的 `imputed` 相加（第 2 层起是上一层往下拨的钱，不是新增投入）；
 * * 不硬判 `share_sum === "100"`（M5-29 的四层链夹具上真实出现 `100.01`，见
 *   {@link shareSumTone} 的容差）；
 * * 不从金额反推行状态（`restored` / `partial` / `no_semi` 由后端 `row_state` 判）。
 */

import type {
  RestoreFlow,
  RestoreGroupRow,
  RestoreItemBrief,
  RestoreReportQuery,
  RestoreReportRes,
  RestoreRollupQuery,
  RestoreRollupRes,
  RestoreRow,
  RestoreSemi,
  RestoreStateFilter,
  RestoreTotals,
} from '@/api/business/erp/cost-restore-kinds';
import {
  ELEMENT_KEYS,
  ELEMENT_LABELS,
  MAX_DEPTH_DEFAULT,
  MAX_DEPTH_LIMIT,
  MAX_DEPTH_MIN,
  PAGE_SIZE,
  REPORT_PAGE_SIZE,
  RESTORE_GROUP_BYS,
  RESTORE_GROUP_BY_DEFAULT,
  RESTORE_STATES,
  RESTORE_STATE_DEFAULT,
  SHARE_SUM_TARGET,
  SHARE_SUM_TOLERANCE,
} from '@/api/business/erp/cost-restore-kinds';
import { formatMoney, toCents } from '@/views/business/erp/shared/money';
import { currentPeriod, isPeriodText, trimZero } from '../cost-standard/cost-standard-logic';

export { PAGE_SIZE, REPORT_PAGE_SIZE };

/** 空占位（表格里所有"没有值"的位置统一用它，不画 0）。 */
const DASH = '—';

// ==================== 筛选 / 入参 ====================

/** 本页筛选（两个页签共用一份，切页签不丢期间）。 */
export interface RestoreFilter {
  /** 会计期间 `yyyy-MM`（**必填**：还原是某一期投料的快照）。 */
  period_code: string;
  /** 对象号 / 生产订单号 / 产出批号 / 产出品编码 / 名称关键字。 */
  keyword: string;
  /** 状态筛选（`all` / `restored` / `partial` / `no_semi`）。 */
  state: RestoreStateFilter;
  /** 下钻层数（数字：界面用 `t-input-number`，提交前夹到 1–12）。 */
  max_depth: number;
  /** 报表汇总维度（`dept` / `product` / `object` / `all`），明细表不用它。 */
  group_by: string;
}

/** 默认期间（本月）：两个页签都要锚定某一个月，首屏就给一个能跑的值。 */
export function defaultPeriod(now: Date = new Date()): string {
  return currentPeriod(now);
}

/** 期间是否可用于查询（`yyyy-MM` 形状）。 */
export function hasPeriod(filter: Pick<RestoreFilter, 'period_code'>): boolean {
  return isPeriodText(filter.period_code ?? '');
}

/** 筛选初值。 */
export function defaultRestoreFilter(now: Date = new Date()): RestoreFilter {
  return {
    period_code: defaultPeriod(now),
    keyword: '',
    state: RESTORE_STATE_DEFAULT,
    max_depth: MAX_DEPTH_DEFAULT,
    group_by: RESTORE_GROUP_BY_DEFAULT,
  };
}

/** 期间为空时的引导话术（这时**不请求**，直接画这句话）。 */
export const PERIOD_REQUIRED_HINT =
  '先选会计期间：还原是「某一期投进去的钱最终变成了什么」，必须锚定某一个月才谈得上还原（报表不猜「上个月」）。';

/**
 * 下钻层数归一化（纯函数，单测钉住）。
 *
 * 后端 `filter_of` 的口径：`0 / 缺省 ⇒ 6`、超过 12 夹到 12。页面提前夹一次，
 * 让输入框里的值和真正发出去的值一致（输入 99 却按 12 查，界面会骗人）。
 */
export function depthValue(text: string | number | null | undefined): number {
  const raw = typeof text === 'number' ? text : Number((text ?? '').toString().trim());
  if (!Number.isFinite(raw) || raw <= 0) {
    return MAX_DEPTH_DEFAULT;
  }
  return Math.min(Math.max(Math.trunc(raw), MAX_DEPTH_MIN), MAX_DEPTH_LIMIT);
}

/**
 * 明细表 → 接口入参（纯函数）。
 *
 * - 期间非法 ⇒ 返回 `null`（页面**不发请求**，显示 {@link PERIOD_REQUIRED_HINT}）；
 * - 关键字 trim 后为空 ⇒ 整键缺席（后端把 `keyword=` 当「筛空」）；
 * - `state` 照发（缺省 `all`）：出参有 `state` 回显，显式发更直白；
 * - `max_depth` 走 {@link depthValue}（夹到 1–12）。
 */
export function buildRollupQuery(
  filter: RestoreFilter,
  page: number,
  pageSize: number = PAGE_SIZE,
): RestoreRollupQuery | null {
  const period = (filter.period_code ?? '').trim();
  if (!isPeriodText(period)) {
    return null;
  }
  const query: RestoreRollupQuery = {
    period_code: period,
    state: (filter.state ?? '').trim() || RESTORE_STATE_DEFAULT,
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : PAGE_SIZE,
    max_depth: depthValue(filter.max_depth),
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/**
 * 报表页签 → 接口入参（纯函数）。
 *
 * 比明细表多一个 `group_by`：拼错后端直接 400（`GroupBy::parse` **不静默回落**），
 * 所以这里只发词表里那四个值，空值回落缺省 `dept`。
 */
export function buildReportQuery(
  filter: RestoreFilter,
  page: number,
  pageSize: number = REPORT_PAGE_SIZE,
): RestoreReportQuery | null {
  const period = (filter.period_code ?? '').trim();
  if (!isPeriodText(period)) {
    return null;
  }
  const group = (filter.group_by ?? '').trim();
  const known = RESTORE_GROUP_BYS.some((option) => option.value === group);
  const query: RestoreReportQuery = {
    period_code: period,
    group_by: known ? group : RESTORE_GROUP_BY_DEFAULT,
    state: (filter.state ?? '').trim() || RESTORE_STATE_DEFAULT,
    page: page > 0 ? page : 1,
    page_size: pageSize > 0 ? pageSize : REPORT_PAGE_SIZE,
    max_depth: depthValue(filter.max_depth),
  };
  const keyword = (filter.keyword ?? '').trim();
  if (keyword) {
    query.keyword = keyword;
  }
  return query;
}

/** 维度下拉项（从 `RESTORE_GROUP_BYS` 派生：两处各写一份必然漂移）。 */
export function groupSelectOptions(): Array<{ label: string; value: string }> {
  return RESTORE_GROUP_BYS.map((option) => ({ label: option.label, value: option.value }));
}

/** 状态下拉项（从 `RESTORE_STATES` 派生，含「全部」）。 */
export function stateSelectOptions(): Array<{ label: string; value: string }> {
  return RESTORE_STATES.map((option) => ({ label: option.label, value: option.value }));
}

// ==================== 数值 → 文字 ====================

/** 金额 → `12,345.60`（千分位 + 2 位，整数分实现见 `shared/money.ts`）。 */
export function moneyText(value: string | number | null | undefined): string {
  return formatMoney(value);
}

/**
 * 精确文本（**不丢位**）：`413.1584` → `413.1584`。
 *
 * 逐层台账与半成品明细用这个：拨下来的钱常是 4 位小数，套 2 位金额格式会把
 * `0.0001` 显示成 `0.00`，用户会以为「没有尾差」，而它恰恰是「对平用」的那一分。
 */
export function exactText(value: string | number | null | undefined): string {
  const text = trimZero(typeof value === 'number' ? String(value) : (value ?? ''));
  if (!/^-?\d+(\.\d+)?$/.test(text)) {
    return text || DASH;
  }
  const negative = text.startsWith('-');
  const body = negative ? text.slice(1) : text;
  const [intPart, fracPart] = body.split('.');
  const grouped = (intPart ?? '0').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${negative ? '-' : ''}${grouped}${fracPart ? `.${fracPart}` : ''}`;
}

/** 数量 → 去尾零 + 千分位（`40.0000` → `40`，`1234.5000` → `1,234.5`）。 */
export function qtyText(value: string | number | null | undefined): string {
  return exactText(value);
}

/**
 * 百分比文本（`"62.5"` → `62.5%`）。
 *
 * 后端给的是**百分数本身**（不是比值）：`after_material_share: "62.5"` 就是 62.5%，
 * 所以这里**不乘 100** —— 与 `cost-variance-logic::rateText`（那是比值）相反。
 * `null` ⇒「—」：分母为 0 时没有占比，画 `0.00%` 会被读成「全是 0」。
 */
export function pctText(value: string | number | null | undefined): string {
  const text = trimZero(typeof value === 'number' ? String(value) : (value ?? ''));
  if (!text) {
    return DASH;
  }
  return `${text}%`;
}

/**
 * 占比变化 → `+13.71pp` / `-40.00pp`（带符号，单位百分点）。
 *
 * 「材料 −40pp、人工 +13.71pp、费 +26.29pp」这种话**必须带符号**才读得懂；
 * 后端给的是纯数字（可能带 `-`），这里只补 `+` 与 `pp` 后缀。
 */
export function signedPctText(value: string | number | null | undefined): string {
  const text = trimZero(typeof value === 'number' ? String(value) : (value ?? ''));
  if (!text) {
    return DASH;
  }
  const negative = text.startsWith('-');
  return `${negative ? '' : '+'}${text}pp`;
}

/** 金额 + `null` 说人话（`null` 是「算不出来」，不是 0）。 */
export function amountOrDash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? DASH : moneyText(value);
}

/** 精确金额 + `null` 说人话。 */
export function exactOrDash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? DASH : exactText(value);
}

// ==================== 行状态 ====================

/** 状态词条（未知值回落成 `default` 主题，但 `label` 用后端给的）。 */
export function stateDef(state: string | null | undefined) {
  const key = (state ?? '').trim();
  return RESTORE_STATES.find((option) => option.value === key) ?? RESTORE_STATES[0];
}

/** 状态 → tag 主题（已还原绿 / 不彻底红 / 其余灰）。 */
export function stateTheme(state: string | null | undefined): 'success' | 'danger' | 'default' | 'warning' | 'primary' {
  return stateDef(state).theme;
}

/** 状态中文（后端 `state_label` 优先，缺了按字面值查表）。 */
export function stateLabelOf(row: Pick<RestoreRow, 'state' | 'state_label'>): string {
  const label = (row.state_label ?? '').trim();
  return label || stateDef(row.state).label;
}

/** 要素中文名。 */
export function elementLabel(key: string): string {
  return ELEMENT_LABELS[key as keyof typeof ELEMENT_LABELS] ?? key;
}

/** 成本对象类型中文（后端 `OBJECT_TYPES = [BATCH, ORDER, PRODUCT]`）。 */
export function objectTypeLabel(objectType: string | null | undefined): string {
  const key = (objectType ?? '').trim().toUpperCase();
  if (key === 'BATCH') {
    return '批次';
  }
  if (key === 'ORDER') {
    return '订单';
  }
  if (key === 'PRODUCT') {
    return '产品';
  }
  return key || DASH;
}

/** 产出品文本（`编码 名称`；都缺 ⇒ 短 id 兜底，不留白）。 */
export function productText(row: Pick<RestoreRow, 'product_code' | 'product_name' | 'product_id'>): string {
  const code = (row.product_code ?? '').trim();
  const name = (row.product_name ?? '').trim();
  if (code && name) {
    return `${code} ${name}`;
  }
  if (code || name) {
    return code || name;
  }
  const id = (row.product_id ?? '').trim();
  return id ? id.slice(0, 8) : DASH;
}

// ==================== 自检判色（只比目标，不重算金额） ====================

/**
 * 占比合计判色。
 *
 * **不许**判 `=== 100`：四项各自由 4 位金额四舍五入到 2 位，合计允许 ±0.02 的舍入差，
 * M5-29 的四层链夹具上实测真实出现 `100.01`（37.83 + 31.09 + 31.09 + 0.00）。
 * 容差 {@link SHARE_SUM_TOLERANCE} 用**整数分**比（`toCents`），不用浮点。
 *
 * 返回：`ok`（在容差内）/ `off`（超出容差，真有问题：金额被改过或后端回归）/
 * `unknown`（`null`：分母为 0，没有占比）。
 */
export function shareSumTone(value: string | null | undefined): 'ok' | 'off' | 'unknown' {
  if (value === null || value === undefined || (value ?? '').trim() === '') {
    return 'unknown';
  }
  const cents = Math.abs(toCents(value) - toCents(SHARE_SUM_TARGET));
  return cents <= toCents(SHARE_SUM_TOLERANCE) ? 'ok' : 'off';
}

/** 占比合计的一句话（把实际值写出来，别只说「正常」）。 */
export function shareSumText(value: string | null | undefined): string {
  const tone = shareSumTone(value);
  if (tone === 'unknown') {
    return '占比合计 —（分母为 0：这一组没有金额）';
  }
  if (tone === 'off') {
    return `占比合计 ${pctText(value)}：偏离 100% 超过 ±${SHARE_SUM_TOLERANCE}，四项金额或占比有一处不对，先把这张表报给财务核对`;
  }
  return `占比合计 ${pctText(value)}（四项各自舍入，允许 ±${SHARE_SUM_TOLERANCE} 舍入差：这是正常值）`;
}

/**
 * 金额文本 → 万分之一（4 位）整数，用于**精确**比较与守恒自检。
 *
 * 为什么不用 `toCents`（2 位）：逐层台账的尾差常是 `0.0001` 量级，
 * 换成整数分就被抹成 0，`flowLines` 的守恒自检会假绿。这里按库列精度
 * `NUMERIC(18,4)` 取 4 位（`0.0001 × 10000 = 1`），与后端 `Flow::balanced` 同精度。
 */
function toScaled4(value: string | number | null | undefined): number {
  const num = Number((value ?? '').toString().trim());
  if (!Number.isFinite(num)) {
    return 0;
  }
  return Math.round(num * 10000);
}

/**
 * 守恒自检判色：`还原后合计 + 还原不彻底 − 还原前合计` 应为 0。
 *
 * 后端每行 / 每组合计都算好这个数（`check`），前端只判它是不是 0
 * （**4 位精度**，`0.0001` 也算不平 —— 那是真差额，不是浮点噪声）。
 */
export function checkTone(value: string | null | undefined): 'ok' | 'off' | 'unknown' {
  if (value === null || value === undefined || (value ?? '').trim() === '') {
    return 'unknown';
  }
  return toScaled4(value) === 0 ? 'ok' : 'off';
}

/** 守恒自检的一句话。 */
export function checkText(value: string | null | undefined): string {
  const tone = checkTone(value);
  if (tone === 'unknown') {
    return '守恒自检 —';
  }
  if (tone === 'off') {
    return `守恒自检 ${exactText(value)}：还原后合计 + 还原不彻底 ≠ 还原前合计（差额即此值），这说明取数或卷积出了问题，别拿这张表结账`;
  }
  return '守恒自检 0：还原后合计 + 还原不彻底 = 还原前合计（还原只是把钱换了个形态，总额一分不动）';
}

// ==================== 汇总卡片 ====================

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
 * 明细表整表合计 → 汇总卡片（**直接用后端 `totals`，前端不累加**）。
 *
 * 八张卡：行数 / 还原前合计 / 还原后外购材料 / 人工 / 制造费用 / 还原不彻底 /
 * 材料里的半成品 / 守恒自检。
 *
 * 「还原不彻底」单列一档：它是这张表里最容易被忽略、又最需要盯的数
 * （看着"还原完了"，其实一部分钱还停在半成品形态）。混进「还原后合计」里就再也分不出来。
 */
export function summaryCards(res: RestoreRollupRes | null): SummaryCard[] {
  const totals = res?.totals;
  if (!totals) {
    return [];
  }
  const partial = totals.partial_row_count;
  const check = totals.check;
  return [
    {
      key: 'rows',
      label: '成本对象',
      value: `${totals.row_count}`,
      sub: `已还原 ${totals.restored_row_count} · 不彻底 ${totals.partial_row_count} · 无需还原 ${totals.no_semi_row_count}（最深 ${totals.max_depth} 层 / ${totals.node_count} 节点）`,
      tone: 'default',
    },
    {
      key: 'before',
      label: '还原前合计',
      value: moneyText(totals.before_total),
      sub: `料 ${moneyText(totals.before_material)}（含半成品 ${moneyText(totals.semi_in_material)}）· 工 ${moneyText(totals.before_labor)} · 费 ${moneyText(totals.before_overhead)}`,
      tone: 'default',
    },
    {
      key: 'after_material',
      label: '还原后外购材料',
      value: moneyText(totals.after_material),
      sub: `占还原后合计 ${pctText(totals.after_material_share)}`,
      tone: 'default',
    },
    {
      key: 'after_labor',
      label: '还原后人工',
      value: moneyText(totals.after_labor),
      sub: `占还原后合计 ${pctText(totals.after_labor_share)}（含从材料里拆出来的）`,
      tone: 'default',
    },
    {
      key: 'after_overhead',
      label: '还原后制造费用',
      value: moneyText(totals.after_overhead),
      sub: `占还原后合计 ${pctText(totals.after_overhead_share)}（含从材料里拆出来的）`,
      tone: 'default',
    },
    {
      key: 'unresolved',
      label: '还原不彻底',
      value: moneyText(totals.unresolved),
      sub:
        partial > 0
          ? `${partial} 行停在半成品形态：产出批次没算 / 已红冲 / 结构闭环 / 到层数上限，展开行看卡在第几层`
          : '没有钱停在半成品形态（全部拆到外购料 / 人工 / 制造费用）',
      tone: partial > 0 ? 'danger' : 'success',
    },
    {
      key: 'semi',
      label: '材料里的自制半成品',
      value: moneyText(totals.semi_in_material),
      sub: '还原的源头：这部分金额被逐层拆成本层料 / 工 / 费（不是新增投入）',
      tone: 'primary',
    },
    {
      key: 'check',
      label: '守恒自检',
      value: exactText(check),
      sub: '还原后合计 + 还原不彻底 − 还原前合计（应为 0）',
      tone: checkTone(check) === 'ok' ? 'success' : 'danger',
    },
  ];
}

/**
 * 报表整表合计 → 汇总卡片。
 *
 * 与明细表的区别：这里的占比分母统一取**还原前合计**（= 还原后三要素 + 不彻底），
 * 所以四项占比合计约 100.00%（允许 ±0.02 舍入差）；明细表行级 `after_*_share`
 * 的分母是「还原后三要素合计」，两者**口径不同**（见 M5-28 模块头第 1 条）。
 */
export function reportCards(res: RestoreReportRes | null): SummaryCard[] {
  const totals = res?.totals;
  if (!totals) {
    return [];
  }
  const shareSum = totals.share_sum;
  return [
    {
      key: 'groups',
      label: '成本对象',
      value: `${totals.object_count}`,
      sub: `已还原 ${totals.restored_rows} · 不彻底 ${totals.partial_rows} · 无需还原 ${totals.no_semi_rows}`,
      tone: 'default',
    },
    {
      key: 'before',
      label: '还原前合计',
      value: moneyText(totals.before.total),
      sub: `料 ${moneyText(totals.before.material)}（含半成品 ${moneyText(totals.semi_in_material)}）· 工 ${moneyText(totals.before.labor)} · 费 ${moneyText(totals.before.overhead)}`,
      tone: 'default',
    },
    {
      key: 'material',
      label: '还原后材料占比',
      value: pctText(totals.restored.material_share),
      sub: `还原前 ${pctText(totals.before.material_share)} · 变化 ${signedPctText(totals.shift.material_pp)}`,
      tone: 'default',
    },
    {
      key: 'labor',
      label: '还原后人工占比',
      value: pctText(totals.restored.labor_share),
      sub: `还原前 ${pctText(totals.before.labor_share)} · 变化 ${signedPctText(totals.shift.labor_pp)}`,
      tone: 'default',
    },
    {
      key: 'overhead',
      label: '还原后制造费用占比',
      value: pctText(totals.restored.overhead_share),
      sub: `还原前 ${pctText(totals.before.overhead_share)} · 变化 ${signedPctText(totals.shift.overhead_pp)}`,
      tone: 'default',
    },
    {
      key: 'unresolved',
      label: '还原不彻底占比',
      value: pctText(totals.restored.unresolved_share),
      sub: `金额 ${moneyText(totals.restored.unresolved)}：既不算外购料，也不丢掉（分母是还原前合计）`,
      tone: (totals.partial_rows ?? 0) > 0 ? 'danger' : 'success',
    },
    {
      key: 'share_sum',
      label: '占比合计（自检）',
      value: pctText(shareSum),
      sub: `材料 + 人工 + 制造费用 + 还原不彻底 应约 100.00%（±${SHARE_SUM_TOLERANCE} 舍入差）`,
      tone: shareSumTone(shareSum) === 'off' ? 'danger' : shareSumTone(shareSum) === 'ok' ? 'success' : 'default',
    },
    {
      key: 'check',
      label: '守恒自检',
      value: exactText(totals.check),
      sub: '还原后合计 + 还原不彻底 − 还原前合计（应为 0）',
      tone: checkTone(totals.check) === 'ok' ? 'success' : 'danger',
    },
  ];
}

// ==================== 展开行：三要素对照 ====================

/** 展开行里的一行要素（还原前 → 还原后 + 差额）。 */
export interface ElementLine {
  /** 要素键。 */
  key: string;
  /** 中文名。 */
  label: string;
  /** 还原前。 */
  before: string;
  /** 还原后。 */
  after: string;
  /** 差额（后端 `delta`，不是前端相减）。 */
  delta: string;
}

/**
 * 三要素对照（固定「材料 / 人工 / 制造费用 / 合计 / 还原不彻底」五行）。
 *
 * 顺序固定取自 {@link ELEMENT_KEYS}：万一后端少给一行，页面**不能**把「制造费用」吞掉 ——
 * 那一行不见了，三要素就跟合计对不上。差额直接用后端 `delta`（`total` 恒为 0）。
 */
export function elementLines(row: RestoreRow): ElementLine[] {
  const lines: ElementLine[] = ELEMENT_KEYS.map((key) => ({
    key,
    label: ELEMENT_LABELS[key],
    before: moneyText(row.before[key]),
    after: moneyText(row.after[key]),
    delta: signedAmount(row.delta[key]),
  }));
  lines.push({
    key: 'total',
    label: '合计',
    before: moneyText(row.before.total),
    after: moneyText(row.after.total),
    delta: signedAmount(row.delta.total),
  });
  lines.push({
    key: 'unresolved',
    label: '还原不彻底',
    before: DASH,
    after: moneyText(row.unresolved),
    delta: DASH,
  });
  return lines;
}

/**
 * 差额金额 → 带符号文本（`+320` / `-680` / `0`）。
 *
 * 差额是**后端算好的**（`delta.*`），这里只补符号：还原后材料必然减少、
 * 人工与费用必然增加，没有符号读不出方向。
 */
export function signedAmount(value: string | null | undefined): string {
  const text = trimZero((value ?? '').toString());
  if (!text || text === '0') {
    return '0';
  }
  const negative = text.startsWith('-');
  return `${negative ? '' : '+'}${exactText(text)}`;
}

// ==================== 展开行：逐层台账 ====================

/** 逐层台账的展示行。 */
export interface FlowLine {
  /** 行 key（层号）。 */
  key: string;
  /** 层级文本（`第 1 层`）。 */
  levelText: string;
  /** 本层节点数。 */
  nodes: string;
  /** 流入本层的金额。 */
  imputed: string;
  /** 本层确认外购料。 */
  purchased: string;
  /** 本层确认人工。 */
  labor: string;
  /** 本层确认制造费用。 */
  overhead: string;
  /** 继续往下拨的钱。 */
  carriedDown: string;
  /** 本层不彻底。 */
  unresolved: string;
  /** 本层守恒（流入 − 五项之和，应为 0）。 */
  balance: string;
  /** 本层是否守恒。 */
  balanced: boolean;
  /** 口径说明（后端文案）。 */
  note: string;
}

/**
 * 逐层台账 → 展示行（**各层不相加**：第 2 层起是上一层「材料部分」往下拨的钱）。
 *
 * 每行做一个守恒自检：`imputed − (purchased + labor + overhead + carried_down + unresolved)`
 * 应为 0（后端 `Flow::balanced` 同一条等式）。用整数分比，避免浮点假报警。
 */
export function flowLines(flows: ReadonlyArray<RestoreFlow> | null | undefined): FlowLine[] {
  return (flows ?? []).map((flow) => {
    const balanceMicros =
      toScaled4(flow.imputed) -
      (toScaled4(flow.purchased) + toScaled4(flow.labor) + toScaled4(flow.overhead) + toScaled4(flow.carried_down) + toScaled4(flow.unresolved));
    return {
      key: `flow-${flow.level}`,
      levelText: `第 ${flow.level} 层`,
      nodes: `${flow.node_count}`,
      imputed: exactText(flow.imputed),
      purchased: exactText(flow.purchased),
      labor: exactText(flow.labor),
      overhead: exactText(flow.overhead),
      carriedDown: exactText(flow.carried_down),
      unresolved: exactText(flow.unresolved),
      balance: balanceMicros === 0 ? '0' : exactText(balanceMicros / 10000),
      balanced: balanceMicros === 0,
      note: flow.note ?? '',
    };
  });
}

// ==================== 展开行：半成品还原明细 ====================

/** 半成品还原明细的展示行。 */
export interface SemiLine {
  /** 行 key（层 + 物料 + 批号）。 */
  key: string;
  /** 层级文本。 */
  levelText: string;
  /** 物料（编码 + 名称；缺编码用名称，都缺用短 id）。 */
  material: string;
  /** 领料批号。 */
  lotNo: string;
  /** 被卷积的金额（父层拨下来的份额）。 */
  imputed: string;
  /** 还原后外购料。 */
  materialCost: string;
  /** 还原后人工。 */
  labor: string;
  /** 还原后制造费用。 */
  overhead: string;
  /** 还原不彻底。 */
  unresolved: string;
  /** 结构来源。 */
  source: string;
  /** 产出它的成本对象号。 */
  producer: string;
  /** 是否停在这一层。 */
  stopped: boolean;
  /** 「停在这一层」的原因话术。 */
  stoppedText: string;
}

/** 半成品明细 → 展示行（一行一个被卷积掉的自制半成品节点）。 */
export function semiLines(semis: ReadonlyArray<RestoreSemi> | null | undefined): SemiLine[] {
  return (semis ?? []).map((semi, index) => {
    const code = (semi.material_code ?? '').trim();
    const name = (semi.material_name ?? '').trim();
    const material = code && name ? `${code} ${name}` : code || name || semi.material_id.slice(0, 8);
    return {
      key: `semi-${semi.level}-${semi.material_id}-${semi.lot_no ?? ''}-${index}`,
      levelText: `第 ${semi.level} 层`,
      material,
      lotNo: (semi.lot_no ?? '').trim() || DASH,
      imputed: exactText(semi.imputed),
      materialCost: exactText(semi.elements.material),
      labor: exactText(semi.elements.labor),
      overhead: exactText(semi.elements.overhead),
      unresolved: exactText(semi.unresolved),
      source: (semi.source_label ?? '').trim() || (semi.source ?? '').trim() || DASH,
      producer: (semi.producer_nos ?? '').trim() || DASH,
      stopped: semi.stopped,
      stoppedText: semi.stopped
        ? '停在这一层（取不到结构 / 到层数上限 / 结构闭环）：材料部分记入「还原不彻底」'
        : '已继续往下拆（材料里还有半成品就再拆一层）',
    };
  });
}

// ==================== 展开行：事实与话术 ====================

/** 展开行里的一条「标签 + 值」。 */
export interface Fact {
  /** 标签。 */
  label: string;
  /** 显示值（已格式化）。 */
  value: string;
  /** 语义色：需要处理红 / 正常绿（只给关键条目用）。 */
  tone?: 'default' | 'danger' | 'success';
}

/** 一行的事实列表（对象 / 计算单 / 半成品 / 层数）。 */
export function rowFacts(row: RestoreRow): Fact[] {
  const facts: Fact[] = [
    { label: '对象类型', value: objectTypeLabel(row.object_type) },
    { label: '产出品', value: productText(row) },
    { label: '生产车间', value: (row.workshop_dept_name ?? '').trim() || '未指定车间' },
    { label: '产出批号', value: (row.lot_no ?? '').trim() || DASH },
  ];
  facts.push(itemFact(row.item));
  facts.push({ label: '材料里的半成品', value: `${moneyText(row.semi_in_material)}（占材料投入 ${pctText(row.semi_share)}）` });
  facts.push({ label: '材料里的外购料', value: moneyText(row.purchased_in_material) });
  facts.push({
    label: '还原出来的工 / 费',
    value: `工 ${moneyText(row.restored_labor)} · 费 ${moneyText(row.restored_overhead)}（从材料里的半成品拆出来）`,
  });
  facts.push({ label: '还原层数', value: `${row.depth} 层 / ${row.node_count} 节点` });
  facts.push({ label: '还原后占比（分母 = 还原后合计）', value: `料 ${pctText(row.after_material_share)} · 工 ${pctText(row.after_labor_share)} · 费 ${pctText(row.after_overhead_share)}` });
  return facts;
}

/** 本期计算单一条事实（没算过 ⇒ 明说「还没算」，不画 0）。 */
function itemFact(item: RestoreItemBrief | null): Fact {
  if (!item) {
    return {
      label: '本期计算单',
      value: '本期还没算过这个成本对象（不是成本为 0）：还原只依赖归集行与半成品产出批次的计算单，照做没问题',
      tone: 'default',
    };
  }
  return {
    label: '本期计算单',
    value: `${item.status} · 完工 ${qtyText(item.completed_qty)} · 完工转出 ${moneyText(item.finished_cost)} · 单位 ${exactText(item.unit_finished_cost)}`,
    tone: 'default',
  };
}

/** 还原不彻底的口径话术（页面常驻一条，展开行再逐行给原因）。 */
export const PARTIAL_HINT =
  '「还原不彻底」= 停在半成品形态、无法继续拆的金额（产出批次没计算 / 已红冲 / 结构闭环 / 到层数上限 / 没有下一层投入）。' +
  '它既不算进外购材料，也不丢掉：还原后合计 + 还原不彻底 = 还原前合计（守恒自检恒为 0）。';

/** 层数提示（明细表 `state = partial` 时最常见的原因之一）。 */
export function depthHint(res: RestoreRollupRes | null): string {
  if (!res) {
    return '';
  }
  const depth = res.max_depth ?? MAX_DEPTH_DEFAULT;
  if (depth >= MAX_DEPTH_LIMIT) {
    return `下钻层数已到上限 ${MAX_DEPTH_LIMIT} 层：还有不彻底就是结构本身的问题（缺结构 / 闭环），不是层数不够。`;
  }
  return `当前下钻 ${depth} 层（上限 ${MAX_DEPTH_LIMIT}）：如果有行不彻底，先把层数调大再试 —— 截断只会把「材料部分」挡在上一层，已拆出的工 / 费不动。`;
}

/** 该行的提示：先逐条 `issues`（事实 + 怎么修），再 `warnings`，最后按状态补一句。 */
export function hintsOf(row: RestoreRow): string[] {
  const hints: string[] = [];
  for (const issue of row.issues ?? []) {
    const message = (issue.message ?? '').trim();
    const fix = (issue.fix ?? '').trim();
    if (message && fix) {
      hints.push(`${message}（怎么修：${fix}）`);
    } else if (message || fix) {
      hints.push(message || fix);
    }
  }
  hints.push(...(row.warnings ?? []));
  if (row.state === 'partial' && hints.length === 0) {
    hints.push(
      '这一行有金额停在半成品形态但没有给出原因码：多为半成品领料批号对不上产出批次，' +
        '到「成本对象」页核对产出批号与半成品的入库批号。',
    );
  }
  return hints;
}

// ==================== 报表分组行 ====================

/** 报表一组的展示行。 */
export interface GroupLine {
  /** 分组键。 */
  key: string;
  /** 分组名。 */
  label: string;
  /** 副标。 */
  hint: string;
  /** 成本对象个数。 */
  objectCount: number;
  /** 还原前金额。 */
  beforeTotal: string;
  /** 还原前材料 / 人工 / 费用。 */
  beforeElements: string;
  /** 还原后金额（三要素合计，不含不彻底）。 */
  afterTotal: string;
  /** 还原后材料 / 人工 / 费用。 */
  afterElements: string;
  /** 还原后材料占比。 */
  materialShare: string;
  /** 还原后人工占比。 */
  laborShare: string;
  /** 还原后费用占比。 */
  overheadShare: string;
  /** 还原不彻底占比。 */
  unresolvedShare: string;
  /** 材料占比变化。 */
  materialPp: string;
  /** 人工占比变化。 */
  laborPp: string;
  /** 费用占比变化。 */
  overheadPp: string;
  /** 占比合计（自检）。 */
  shareSum: string;
  /** 占比合计判色。 */
  shareSumTone: 'ok' | 'off' | 'unknown';
  /** 守恒自检。 */
  check: string;
  /** 守恒判色。 */
  checkTone: 'ok' | 'off' | 'unknown';
  /** 后端原始 `share_sum`（tooltip 显示原文与容差判断用）。 */
  shareRaw: string | null;
  /** 后端原始 `check`（tooltip 用）。 */
  checkRaw: string;
  /** 主导要素中文名。 */
  dominant: string;
  /** 半成品占材料投入。 */
  semiShare: string;
  /** 行状态计数。 */
  rowCounts: string;
}

/** 报表分组 → 展示行（一页 50 组，行序由后端定：还原后合计降序 + key 升序）。 */
export function groupLines(res: RestoreReportRes | null): GroupLine[] {
  return (res?.groups ?? []).map((group) => groupLine(group));
}

/** 单个分组 → 展示行（合计行也走这条，页面用 `groupLine(res.totals)`）。 */
export function groupLine(group: RestoreGroupRow): GroupLine {
  return {
    key: group.key,
    label: group.label,
    hint: (group.hint ?? '').trim(),
    objectCount: group.object_count,
    beforeTotal: moneyText(group.before.total),
    beforeElements: `料 ${moneyText(group.before.material)} · 工 ${moneyText(group.before.labor)} · 费 ${moneyText(group.before.overhead)}`,
    afterTotal: moneyText(group.restored.total),
    afterElements: `料 ${moneyText(group.restored.material)} · 工 ${moneyText(group.restored.labor)} · 费 ${moneyText(group.restored.overhead)}`,
    materialShare: pctText(group.restored.material_share),
    laborShare: pctText(group.restored.labor_share),
    overheadShare: pctText(group.restored.overhead_share),
    unresolvedShare: pctText(group.restored.unresolved_share),
    materialPp: signedPctText(group.shift.material_pp),
    laborPp: signedPctText(group.shift.labor_pp),
    overheadPp: signedPctText(group.shift.overhead_pp),
    shareSum: pctText(group.share_sum),
    shareSumTone: shareSumTone(group.share_sum),
    check: exactText(group.check),
    checkTone: checkTone(group.check),
    shareRaw: group.share_sum,
    checkRaw: group.check,
    dominant: (group.dominant_label ?? '').trim() || DASH,
    semiShare: pctText(group.semi_share),
    rowCounts: `已还原 ${group.restored_rows} · 不彻底 ${group.partial_rows} · 无需还原 ${group.no_semi_rows}`,
  };
}

// ==================== 话术（页头 / 空态 / 口径） ====================

/**
 * 顶部引导条（`cost_ready=false`）。
 *
 * 不是「算坏了」：没有成本核算批次时，明细表的「本期完工成本」是空的，但**还原照做** ——
 * 还原只依赖归集行与半成品产出批次的计算单。页面必须说清这一点，否则人以为整张表不可信。
 */
export function scopeHintOf(res: RestoreRollupRes | RestoreReportRes | null): string {
  if (!res) {
    return '';
  }
  if (res.cost_ready) {
    return '';
  }
  return (
    `${res.period_code} 还没有成本核算批次：「本期完工成本」这一列是空的，但还原照做 —— ` +
    '还原只依赖本期归集行与半成品产出批次的计算单。要到「成本核算」页建该期间批次并执行计算后，完工成本才会有数。'
  );
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

/** 明细表空态文案（区分「没选期间」「筛选没命中」「这个月什么都没有」）。 */
export function emptyTextOf(res: RestoreRollupRes | null, filter: RestoreFilter): string {
  if (!hasPeriod(filter)) {
    return PERIOD_REQUIRED_HINT;
  }
  if (!res) {
    return '还没有取到数据（点「查询」重试，或换一个期间）。';
  }
  const state = (filter.state ?? '').trim();
  if (state && state !== 'all') {
    const label = RESTORE_STATES.find((option) => option.value === state)?.label ?? state;
    return `${res.period_code} 没有「${label}」的行 —— 换成「全部」或清空关键字看看（没有行不代表没投产，可能是筛选把它藏了）。`;
  }
  return (
    `${res.period_code} 没有可还原的成本对象：或本期没有投产批次，或所有批本期没有材料投入。` +
    '先到「成本对象」页确认本期有投产批次，并到「成本核算」页跑该期间的归集与计算。'
  );
}

/** 报表空态文案。 */
export function reportEmptyText(res: RestoreReportRes | null, filter: RestoreFilter): string {
  if (!hasPeriod(filter)) {
    return PERIOD_REQUIRED_HINT;
  }
  if (!res) {
    return '还没有取到数据（点「查询」重试，或换一个期间）。';
  }
  return (
    `${res.period_code} 没有可汇总的还原结果（维度：${res.group_by_label || filter.group_by}）：` +
    '明细表这一期本身就是空的，报表自然没有行。先看明细页签确认本期有可还原的成本对象。'
  );
}

/** 报表页脚口径（分母 / 占比公式 / 结构变化公式，三段合一）。 */
export function reportBasis(res: RestoreReportRes | null): string {
  if (!res) {
    return '';
  }
  return [noteText(res.basis), noteText(res.formula_share), noteText(res.formula_shift)].filter(Boolean).join(' ');
}

/** 明细表页脚口径（卷积 + 不彻底 + 页脚说明）。 */
export function rollupBasis(res: RestoreRollupRes | null): string {
  if (!res) {
    return '';
  }
  return [noteText(res.formula_restore), noteText(res.formula_unresolved), noteText(res.note)].filter(Boolean).join(' ');
}

/** 报表一行是否要标红（不彻底占比 > 0 且行数 > 0）。 */
export function groupNeedsAttention(group: RestoreGroupRow): boolean {
  return group.partial_rows > 0;
}

/** 合计行（报表的 `totals` 也走 {@link groupLine}，页面别自己拼一遍）。 */
export function reportTotalsLine(res: RestoreReportRes | null): GroupLine | null {
  return res ? groupLine(res.totals) : null;
}

/** 明细表一行的金额语义色（不彻底 > 0 ⇒ 红，否则默认）。 */
export function unresolvedTone(row: Pick<RestoreRow, 'state'>): 'default' | 'danger' {
  return row.state === 'partial' ? 'danger' : 'default';
}

/** `RestoreTotals` 的不彻底金额（展开行/卡片共用，避免各处 `.unresolved` 写错层级）。 */
export function unresolvedOfTotals(totals: RestoreTotals | null | undefined): string {
  return moneyText(totals?.unresolved);
}
