/**
 * 销售返利「政策配置」页纯逻辑（M3-19 / 后端 M3-15）—— 可测的判断都在这里，`.vue` 只渲染。
 *
 * # 口径（全抄后端与迁移，前端不另立一套）
 *
 * | 项 | 真口径 | 出处 |
 * |----|--------|------|
 * | 按量 `BY_QTY` | 恰好 **1 档**，`tier_from = 0`、`tier_to` 空；`rate_unit` 固定 `PER_UNIT` | `rebate_dto::checked_rules` |
 * | 按额 `BY_AMOUNT` | 恰好 **1 档**，区间同上；`rate_unit` 固定 `PERCENT` | 同上 |
 * | 阶梯 `TIERED` | **2~20 档**；首档从 0 起；逐档首尾相接；末档 `tier_to` 空 | 同上 |
 * | 比率 | `rate > 0`；`PERCENT` 时 ≤ 100；最多 6 位小数 | `check_rate` |
 * | 档位值 | `tier_from ≥ 0`；`tier_to > tier_from`；最多 4 位小数 | `check_tier_value` |
 * | 有效期 | `effective_to ≥ effective_from`（`null` = 长期；**两端都含**） | V0438 `ck_biz_srbp_effective` |
 * | 计价单位 | 按量必 元/件、按额必 百分比、阶梯两者皆可（必须显式） | `checked_unit` |
 * | 结算粒度 | `MONTH` / `QUARTER`（与期间码形状互相钉死） | `checked_settle_period` |
 *
 * # 为什么前端也要校验一遍
 *
 * 库层 / 后端的 400 文案是「第 N 档的 rate 必须大于 0」这类有一定可读性的句子，但错误要
 * 一次一个来回才能看到；前端先拦一道能给「第 2 档的上限要和第 3 档的下限接上」这种人话
 * （后端仍会再判一次 —— 前端不是安全边界）。
 *
 * # 两条容易踩的
 *
 * 1. **编辑是整单覆盖**：`PUT` 会按提交的 `rules` 整单替换档位，所以编辑弹窗必须持有全部档位
 *    （本文件 `policyFormFrom` 负责回填）；只发变化的那一档 = 把别的档删掉。
 * 2. **`status` 是数字**（`1` / `0`），不是 `ENABLED` 这种字符串 —— 抄错会让后端 400。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { formatMoney } from '@/views/business/erp/shared/money';
import type {
  DecimalText,
  RebatePolicyFilter,
  RebatePolicyRow,
  RebatePolicyStatus,
  RebatePolicyWriteReq,
  RebateRateUnit,
  RebateRuleInput,
  RebateRuleRow,
  RebateRuleType,
  RebateSettlePeriod,
} from '@/api/business/erp/rebate';

/** 每页条数（与仓内列表页同口径；后端上限 200） */
export const POLICY_PAGE_SIZE = 20;

/** 政策名称上限（`V0438` 的 `policy_name VARCHAR(200)`） */
export const POLICY_NAME_MAX = 200;

/** 备注上限（`remark TEXT`，纯防手滑） */
export const POLICY_REMARK_MAX = 500;

/** 档数上下限（`rebate_dto::MAX_TIERS` / `MIN_TIER_TIERS`） */
export const MAX_TIERS = 20;
export const MIN_TIER_TIERS = 2;

/** 域值（与 `rebate_dto` 常量逐字一致；单测钉住） */
export const REBATE_RULE_TYPES: readonly RebateRuleType[] = ['BY_QTY', 'BY_AMOUNT', 'TIERED'];
export const REBATE_RATE_UNITS: readonly RebateRateUnit[] = ['PER_UNIT', 'PERCENT'];
export const REBATE_SETTLE_PERIODS: readonly RebateSettlePeriod[] = ['MONTH', 'QUARTER'];

/** 规则类型候选（顺序 = 后端 `RULE_TYPES`） */
export const RULE_TYPE_OPTIONS: Array<{ label: string; value: RebateRuleType; hint: string }> = [
  {
    label: '按量返利',
    value: 'BY_QTY',
    hint: '净销量 × 每件返多少元（平铺单档，例如每件 0.5 元）',
  },
  {
    label: '按额返利',
    value: 'BY_AMOUNT',
    hint: '净销售额 × 百分比（平铺单档，例如 2% ⇒ 填 2）',
  },
  {
    label: '阶梯返利',
    value: 'TIERED',
    hint: '按销量或销售额分档，落哪档按哪档费率乘（不做累进）',
  },
];

/** 规则类型中文名 */
export const RULE_TYPE_LABELS: Record<RebateRuleType, string> = {
  BY_QTY: '按量',
  BY_AMOUNT: '按额',
  TIERED: '阶梯',
};

/** 计价单位候选（阶梯时二选一；按量/按额由类型钉死） */
export const RATE_UNIT_OPTIONS: Array<{ label: string; value: RebateRateUnit }> = [
  { label: '元/件（按销量分档）', value: 'PER_UNIT' },
  { label: '销售额百分比（按销售额分档）', value: 'PERCENT' },
];

/** 计价单位中文名 */
export const RATE_UNIT_LABELS: Record<RebateRateUnit, string> = {
  PER_UNIT: '元/件',
  PERCENT: '销售额百分比',
};

/** 结算粒度候选 */
export const SETTLE_PERIOD_OPTIONS: Array<{ label: string; value: RebateSettlePeriod }> = [
  { label: '月度（期间码 yyyy-MM）', value: 'MONTH' },
  { label: '季度（期间码 yyyy-QN）', value: 'QUARTER' },
];

/** 结算粒度中文名 */
export const SETTLE_PERIOD_LABELS: Record<RebateSettlePeriod, string> = {
  MONTH: '月度',
  QUARTER: '季度',
};

/** 政策状态中文名（后端字段是 `1` / `0`） */
export const POLICY_STATUS_LABELS: Record<RebatePolicyStatus, string> = {
  1: '启用',
  0: '停用',
};

/** 规则类型筛选候选（`''` = 全部） */
export const RULE_TYPE_FILTER_OPTIONS: Array<{ label: string; value: '' | RebateRuleType }> = [
  { label: '全部类型', value: '' },
  ...REBATE_RULE_TYPES.map((value) => ({ label: RULE_TYPE_LABELS[value], value })),
];

/** 结算粒度筛选候选（`''` = 全部） */
export const SETTLE_PERIOD_FILTER_OPTIONS: Array<{
  label: string;
  value: '' | RebateSettlePeriod;
}> = [
  { label: '全部粒度', value: '' },
  ...REBATE_SETTLE_PERIODS.map((value) => ({ label: SETTLE_PERIOD_LABELS[value], value })),
];

/** 状态筛选候选（`''` = 全部） */
export const POLICY_STATUS_FILTER_OPTIONS: Array<{
  label: string;
  value: '' | RebatePolicyStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
];

/** 政策列表列（`op` 由页面用 `#op` 插槽渲染） */
export const POLICY_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'policy_no', title: '政策编号', width: 175, fixed: 'left' },
  { colKey: 'policy_name', title: '政策名称', minWidth: 190, ellipsis: true },
  { colKey: 'customer', title: '客户', minWidth: 170, ellipsis: true },
  { colKey: 'rule_type', title: '规则类型', width: 90 },
  { colKey: 'rule_detail', title: '规则 / 档位', minWidth: 230, ellipsis: true },
  { colKey: 'settle_period', title: '结算粒度', width: 95 },
  { colKey: 'validity', title: '有效期', width: 195 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 130, fixed: 'right' },
];

// ==================== 展示 ====================

/** 规则类型中文名（未知值回落编码本身，别显示空白） */
export function ruleTypeLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return RULE_TYPE_LABELS[value as RebateRuleType] ?? value;
}

/** 计价单位中文名 */
export function rateUnitLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return RATE_UNIT_LABELS[value as RebateRateUnit] ?? value;
}

/** 结算粒度中文名 */
export function settlePeriodLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return SETTLE_PERIOD_LABELS[value as RebateSettlePeriod] ?? value;
}

/** 政策状态中文名（`1` 启用 / `0` 停用；未知值按停用灰显示，不显示空白） */
export function policyStatusLabel(value: number | null | undefined): string {
  if (value === 1) {
    return '启用';
  }
  if (value === 0) {
    return '停用';
  }
  return '—';
}

/** 政策状态配色：启用 = 成功色，停用 = 灰 */
export function policyStatusTheme(
  value: number | null | undefined,
): 'success' | 'default' | 'warning' {
  if (value === 1) {
    return 'success';
  }
  if (value === 0) {
    return 'default';
  }
  return 'warning';
}

/** 客户展示：优先编码 + 名称；客户已软删时后端给 `null`（历史政策仍要能读） */
export function customerText(
  row: Pick<RebatePolicyRow, 'customer_code' | 'customer_name' | 'customer_id'>,
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

/**
 * 比率展示：`PERCENT` 带 `%`、`PER_UNIT` 带「元/件」。
 *
 * 只写数值会让人以为是另一种量纲（`5` 是 5% 还是 5 元/件？）—— 单位必须显式。
 */
export function rateText(rate: DecimalText | null | undefined, unit: RebateRateUnit): string {
  const raw = rate === null || rate === undefined || rate === '' ? '0' : String(rate);
  const num = Number(raw);
  if (!Number.isFinite(num)) {
    return `${raw}${unit === 'PERCENT' ? '%' : ' 元/件'}`;
  }
  // 去掉 `0.500000` 的尾随 0（后端 `NUMERIC(18,6)` 发字符串）
  const text = String(Number(num.toFixed(6)));
  return unit === 'PERCENT' ? `${text}%` : `${text} 元/件`;
}

/**
 * 档位区间展示：`[0, 1000) + [1000, +∞)`。
 *
 * 左闭右开是后端落档口径（`pick_tier`），区间边界值落**下一档** —— 展示时必须体现，
 * 否则「正好 1000 件算哪档」会各说各话。
 */
export function tierRangeText(
  from: DecimalText | null | undefined,
  to: DecimalText | null | undefined,
): string {
  const start = from === null || from === undefined || from === '' ? '0' : String(Number(from));
  const end = to === null || to === undefined || to === '' ? '+∞' : String(Number(to));
  return `${start} ~ ${end}`;
}

/**
 * 「规则 / 档位」列文案。
 *
 * - 按量 / 按额：`平铺 0.5 元/件`
 * - 阶梯：`3 档：0~1000 / 1000~5000 / 5000~+∞`（档数多时只写档数与首末档，避免撑行）
 */
export function policyRuleSummary(
  row: Pick<RebatePolicyRow, 'rule_type' | 'rate_unit' | 'rules'>,
): string {
  const rules = row.rules ?? [];
  if (rules.length === 0) {
    return '—（无档位）';
  }
  if (row.rule_type !== 'TIERED') {
    const first = rules[0];
    return `平铺 ${rateText(first.rate, row.rate_unit)}`;
  }
  const head = `${rules.length} 档`;
  const ranges = rules
    .slice(0, 3)
    .map((rule) => tierRangeText(rule.tier_from, rule.tier_to))
    .join(' / ');
  const tail = rules.length > 3 ? ` … ${tierRangeText(rules[rules.length - 1].tier_from, rules[rules.length - 1].tier_to)}` : '';
  return `${head}：${ranges}${tail}`;
}

/** 单档费率明细（编辑弹窗下方的档位预览用） */
export function ruleLineText(
  rule: Pick<RebateRuleRow, 'seq' | 'tier_from' | 'tier_to' | 'rate'>,
  unit: RebateRateUnit,
): string {
  return `第 ${rule.seq} 档 ${tierRangeText(rule.tier_from, rule.tier_to)} ⇒ ${rateText(rule.rate, unit)}`;
}

/** 有效期文案（两端都含；`effective_to` 空 = 长期有效） */
export function validityText(
  from: string | null | undefined,
  to: string | null | undefined,
): string {
  return `${from || '—'} ~ ${to || '长期'}`;
}

// ==================== 筛选 ====================

/** 空筛选 */
export function emptyPolicyFilter(): RebatePolicyFilter {
  return {
    keyword: '',
    rule_type: '',
    settle_period: '',
    status: '',
    active_on: '',
    page: 1,
    page_size: POLICY_PAGE_SIZE,
  };
}

/** 是否带了筛选条件（决定空态文案） */
export function isPolicyFiltered(filter: RebatePolicyFilter): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() ||
      filter.rule_type ||
      filter.settle_period ||
      filter.status !== '' && filter.status !== undefined ||
      (filter.active_on ?? '').trim(),
  );
}

/** 列表空态文案 */
export function policyEmptyText(filter: RebatePolicyFilter): string {
  return isPolicyFiltered(filter)
    ? '没有匹配的返利政策：换个客户 / 类型 / 粒度再查'
    : '还没有返利政策：先给要返利的客户建一条（按量 / 按额 / 阶梯）';
}

// ==================== 表单 ====================

/** 一档的表单草稿（数值允许 `t-input-number` 回写的数字；提交前统一转字符串） */
export interface TierDraft {
  /** 本档下限（含）；按量/按额恒为 `0` */
  tier_from: number | string;
  /** 本档上限（不含）；空串 = 无上限（只有末档允许） */
  tier_to: number | string;
  /** 比率 */
  rate: number | string;
  remark: string;
}

/** 政策表单草稿 */
export interface PolicyFormDraft {
  policy_name: string;
  /** 客户 id */
  customer_id: string;
  /** 客户显示名（只给人看，不提交；编辑回填时用 `customer_code + customer_name`） */
  customer_display: string;
  rule_type: RebateRuleType;
  rate_unit: RebateRateUnit;
  settle_period: RebateSettlePeriod;
  effective_from: string;
  /** 空串 = 长期有效 */
  effective_to: string;
  status: RebatePolicyStatus;
  remark: string;
  rules: TierDraft[];
}

/** 类型对应的固定计价单位（`TIERED` 为 `null`：必须显式选） */
export function defaultUnitOf(ruleType: RebateRuleType): RebateRateUnit | null {
  if (ruleType === 'BY_QTY') {
    return 'PER_UNIT';
  }
  if (ruleType === 'BY_AMOUNT') {
    return 'PERCENT';
  }
  return null;
}

/** 平铺政策的唯一一档（`tier_from = 0`、无上限） */
export function flatTier(rate: number | string = ''): TierDraft {
  return { tier_from: '0', tier_to: '', rate, remark: '' };
}

/**
 * 阶梯政策的初始两档：`0~1000` + `1000~+∞`。
 *
 * 给一个**首尾相接、末档开口**的模板，是为了让「首档不从 0 起 / 中间断档 / 末档封顶」
 * 这三个最常见错误在默认状态下就不存在（用户只需填费率与边界数字）。
 */
export function defaultTieredTiers(rate: number | string = ''): TierDraft[] {
  return [
    { tier_from: '0', tier_to: '1000', rate, remark: '' },
    { tier_from: '1000', tier_to: '', rate: '', remark: '' },
  ];
}

/** 空表单（新建用；`today` 由调用方给，便于测试与「北京时间今天」口径） */
export function emptyPolicyForm(today: string): PolicyFormDraft {
  return {
    policy_name: '',
    customer_id: '',
    customer_display: '',
    rule_type: 'BY_QTY',
    rate_unit: 'PER_UNIT',
    settle_period: 'MONTH',
    effective_from: today,
    effective_to: '',
    status: 1,
    remark: '',
    rules: [flatTier()],
  };
}

/** `0.500000` → `0.5`（回填输入框时别让用户看到 6 位多余 0） */
export function toPlain(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '';
  }
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return String(Number(num.toFixed(6)));
}

/** 列表行 → 表单（编辑回填；**全量档位**，因为编辑是整单替换） */
export function policyFormFrom(row: RebatePolicyRow): PolicyFormDraft {
  const rules = (row.rules ?? [])
    .slice()
    .sort((left, right) => Number(left.tier_from) - Number(right.tier_from))
    .map((rule) => ({
      tier_from: toPlain(rule.tier_from),
      tier_to: rule.tier_to === null || rule.tier_to === undefined ? '' : toPlain(rule.tier_to),
      rate: toPlain(rule.rate),
      remark: rule.remark ?? '',
    }));
  return {
    policy_name: row.policy_name,
    customer_id: row.customer_id,
    customer_display: customerText(row),
    rule_type: row.rule_type,
    rate_unit: row.rate_unit,
    settle_period: row.settle_period,
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
    rules: rules.length > 0 ? rules : [flatTier()],
  };
}

/**
 * 切换规则类型时的表单整形。
 *
 * - 按量 / 按额：把档位压成**唯一一档平铺**（保留第一档的费率，别让人重填）；
 * - 阶梯：不足两档时补成两档模板；
 * - `rate_unit`：按量/按额由类型钉死；阶梯保留用户当前选择（默认 元/件）。
 *
 * 为什么必须整形：后端 `checked_rules` 对「按量却给了 3 档」直接 400 + 66089，
 * 而用户只是点了下类型 —— 前端当场把形状调对，比事后报错体验好得多。
 */
export function onRuleTypeChange(form: PolicyFormDraft, next: RebateRuleType): void {
  const firstRate = form.rules[0]?.rate ?? '';
  form.rule_type = next;
  const fixed = defaultUnitOf(next);
  if (fixed) {
    form.rate_unit = fixed;
    form.rules = [flatTier(firstRate)];
    return;
  }
  // TIERED：必须显式单位；档位不足两档就补模板（保留第一档费率与备注）
  if (form.rules.length < MIN_TIER_TIERS) {
    const template = defaultTieredTiers(firstRate);
    if (form.rules.length === 1) {
      template[1].remark = form.rules[0].remark ?? '';
    }
    form.rules = template;
  }
}

/** 加一档（只有末档可开口，所以新档的下限 = 上一档的上限） */
export function addTier(form: PolicyFormDraft): { ok: boolean; reason?: string } {
  if (form.rule_type !== 'TIERED') {
    return { ok: false, reason: '只有阶梯返利可以加档（按量/按额是平铺单档）' };
  }
  if (form.rules.length >= MAX_TIERS) {
    return { ok: false, reason: `档位最多 ${MAX_TIERS} 档` };
  }
  const last = form.rules[form.rules.length - 1];
  const lastTo = String(last?.tier_to ?? '').trim();
  if (!lastTo) {
    return { ok: false, reason: '请先给当前最后一档填上限：新档的下限就是它的上限（阶梯必须首尾相接）' };
  }
  form.rules.push({ tier_from: lastTo, tier_to: '', rate: '', remark: '' });
  return { ok: true };
}

/**
 * 删档：**只允许删最后一档**（且至少留 2 档）。
 *
 * 删中间档会立刻断开「上一档的上限 = 下一档的下限」，那种数据后端一定拒；
 * 与其让用户删完再看一条报错，不如只留一条安全路径。
 */
export function removeTier(form: PolicyFormDraft, index: number): { ok: boolean; reason?: string } {
  if (form.rule_type !== 'TIERED') {
    return { ok: false, reason: '按量/按额是平铺单档，不能删档' };
  }
  if (form.rules.length <= MIN_TIER_TIERS) {
    return { ok: false, reason: `阶梯返利至少要 ${MIN_TIER_TIERS} 档（只有一档请改用按量 / 按额）` };
  }
  if (index !== form.rules.length - 1) {
    return { ok: false, reason: '只能删最后一档：删中间档会让上一档与下一档接不上' };
  }
  form.rules.pop();
  // 删完之后新的末档必须开口（无条件清空，避免「末档封顶」这种算不出来返利的形状）
  const last = form.rules[form.rules.length - 1];
  if (last) {
    last.tier_to = '';
  }
  return { ok: true };
}

/** 改了第 `index` 档的上限 → 同步下一档的下限（保持首尾相接） */
export function syncTierBound(form: PolicyFormDraft, index: number): void {
  const current = form.rules[index];
  const next = form.rules[index + 1];
  if (!current || !next) {
    return;
  }
  const to = String(current.tier_to ?? '').trim();
  if (to) {
    next.tier_from = to;
  }
}

/** 小数位判定（`1.2345` ⇒ 4 位；`1` ⇒ 0 位） */
function decimalPlaces(value: number): number {
  if (!Number.isFinite(value)) {
    return 0;
  }
  const text = String(value);
  const dot = text.indexOf('.');
  if (dot < 0) {
    return 0;
  }
  return text.length - dot - 1;
}

/** `yyyy-MM-dd` 形状 */
function isDateString(text: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(text);
}

/**
 * 档位校验（返回第一条人话错误；`null` = 通过）。
 *
 * 与后端 `checked_rules` 同序：先逐档基础校验（谁错点名谁），再判类型形状。
 */
export function validateRules(
  ruleType: RebateRuleType,
  rateUnit: RebateRateUnit,
  drafts: TierDraft[],
): string | null {
  if (drafts.length === 0) {
    return '至少要有 1 档规则：没有比率的政策算不出返利';
  }
  if (drafts.length > MAX_TIERS) {
    return `档位最多 ${MAX_TIERS} 档（当前 ${drafts.length} 档）`;
  }
  for (let index = 0; index < drafts.length; index += 1) {
    const label = `第 ${index + 1} 档`;
    const fromText = String(drafts[index].tier_from ?? '').trim();
    const from = Number(fromText === '' ? 0 : fromText);
    if (!Number.isFinite(from) || from < 0) {
      return `${label}的下限必须是不小于 0 的数字`;
    }
    if (decimalPlaces(from) > 4) {
      return `${label}的下限最多 4 位小数（库层 NUMERIC(18,4)，多的位会被静默四舍五入）`;
    }
    const toText = String(drafts[index].tier_to ?? '').trim();
    if (toText !== '') {
      const to = Number(toText);
      if (!Number.isFinite(to) || to < 0) {
        return `${label}的上限必须是不小于 0 的数字（留空 = 无上限）`;
      }
      if (decimalPlaces(to) > 4) {
        return `${label}的上限最多 4 位小数`;
      }
      if (to <= from) {
        return `${label}的区间不成立：上限（${to}）必须大于下限（${from}）—— 相等就是空区间`;
      }
    }
    const rateText_ = String(drafts[index].rate ?? '').trim();
    const rate = Number(rateText_);
    if (rateText_ === '' || !Number.isFinite(rate)) {
      return `${label}缺少比率（按量填每件返多少元，按额/百分比阶梯填百分比）`;
    }
    if (rate <= 0) {
      return `${label}的比率必须大于 0：0 元档会让人以为「这一档不返」`;
    }
    if (decimalPlaces(rate) > 6) {
      return `${label}的比率最多 6 位小数（库层 NUMERIC(18,6)）`;
    }
    if (rateUnit === 'PERCENT' && rate > 100) {
      return `${label}是百分比，不能超过 100（填 2 表示 2%，不是 0.02）`;
    }
  }

  const flat = ruleType === 'BY_QTY' || ruleType === 'BY_AMOUNT';
  if (flat) {
    if (drafts.length !== 1) {
      return `${ruleTypeLabel(ruleType)}返利只能有 1 档（当前 ${drafts.length} 档）：要按区间分档请改用阶梯`;
    }
    const only = drafts[0];
    const from = Number(String(only.tier_from ?? '').trim() || 0);
    const to = String(only.tier_to ?? '').trim();
    if (from !== 0 || to !== '') {
      return `${ruleTypeLabel(ruleType)}返利是平铺费率：唯一一档必须从 0 起且上限留空（当前 ${tierRangeText(only.tier_from, only.tier_to)}）—— 要设区间请改用阶梯`;
    }
    return null;
  }

  // TIERED：按 tier_from 升序后判形状（与后端一致：入参顺序不参与判定）
  const sorted = drafts
    .map((draft) => ({
      from: Number(String(draft.tier_from ?? '').trim() || 0),
      to: String(draft.tier_to ?? '').trim(),
    }))
    .sort((left, right) => left.from - right.from);

  if (sorted.length < MIN_TIER_TIERS) {
    return `阶梯返利至少要 ${MIN_TIER_TIERS} 档（当前 ${sorted.length} 档）：只有一档请改用按量 / 按额`;
  }
  for (let index = 1; index < sorted.length; index += 1) {
    if (sorted[index].from === sorted[index - 1].from) {
      return `阶梯档位下限重复（${sorted[index].from} 出现两次）：同一档位区间只能有一行`;
    }
  }
  if (sorted[0].from !== 0) {
    return `阶梯的第一档必须从 0 起（当前 ${sorted[0].from}）：0 ~ ${sorted[0].from} 这段会没有费率，结算时被静默算成 0 返利`;
  }
  for (let index = 0; index < sorted.length - 1; index += 1) {
    const current = sorted[index];
    const next = sorted[index + 1];
    if (current.to === '') {
      return `第 ${index + 1} 档（${current.from} 起）没写上限，但它不是最后一档：中间档必须写上上限`;
    }
    if (Number(current.to) !== next.from) {
      return `第 ${index + 1} 档的上限（${current.to}）与第 ${index + 2} 档的下限（${next.from}）接不上：阶梯必须首尾相接`;
    }
  }
  if (sorted[sorted.length - 1].to !== '') {
    return '最后一档必须留空上限（空 = 无上限）：封顶的话超过上限的部分会落在所有档位之外，返利算成 0';
  }
  return null;
}

/**
 * 表单校验（返回第一条人话错误；`null` = 通过）。
 *
 * 顺序刻意如此：先判「必填」、再判「语义」、最后判档位形状 —— 用户改起来最短。
 */
export function validatePolicyForm(form: PolicyFormDraft): string | null {
  const name = form.policy_name.trim();
  if (!name) {
    return '请填写政策名称';
  }
  if (name.length > POLICY_NAME_MAX) {
    return `政策名称最多 ${POLICY_NAME_MAX} 字`;
  }
  if (!form.customer_id.trim()) {
    return '请选择客户（返利政策是「某客户 × 某粒度」的条款）';
  }
  if (!REBATE_SETTLE_PERIODS.includes(form.settle_period)) {
    return '结算粒度只能是月度（MONTH）或季度（QUARTER）';
  }
  if (!REBATE_RULE_TYPES.includes(form.rule_type)) {
    return '规则类型只能是 按量 / 按额 / 阶梯';
  }
  if (form.rule_type === 'TIERED' && !REBATE_RATE_UNITS.includes(form.rate_unit)) {
    return '阶梯返利必须选计价单位：元/件（按销量分档）或 销售额百分比（按销售额分档）';
  }
  if (!isDateString(form.effective_from)) {
    return '请选择生效日（yyyy-MM-dd）';
  }
  if (form.effective_to) {
    if (!isDateString(form.effective_to)) {
      return '失效日格式必须是 yyyy-MM-dd';
    }
    if (form.effective_to < form.effective_from) {
      return '失效日不能早于生效日';
    }
  }
  if (form.remark.length > POLICY_REMARK_MAX) {
    return `备注最多 ${POLICY_REMARK_MAX} 字`;
  }
  const unit = defaultUnitOf(form.rule_type) ?? form.rate_unit;
  return validateRules(form.rule_type, unit, form.rules);
}

/**
 * 表单 → 接口入参。
 *
 * - `rules` 里数值统一转字符串（后端是 `Decimal`，浮点直传会带 `0.30000000000000004`）；
 * - 空上限发 `null`（不是空串：`Option<Decimal>` 收到 `""` 会 400）；
 * - `rate_unit` **总是显式发**（阶梯必填；按量/按额显式发也不会错，省一次「省略」的分支）；
 * - `status` 是数字。
 */
export function buildPolicyPayload(form: PolicyFormDraft): RebatePolicyWriteReq {
  const unit = defaultUnitOf(form.rule_type) ?? form.rate_unit;
  const rules: RebateRuleInput[] = form.rules.map((draft) => {
    const to = String(draft.tier_to ?? '').trim();
    return {
      tier_from: String(Number(String(draft.tier_from ?? '').trim() || 0)),
      tier_to: to === '' ? null : String(Number(to)),
      rate: String(Number(String(draft.rate ?? '').trim() || 0)),
      remark: draft.remark.trim() || null,
    };
  });
  return {
    policy_name: form.policy_name.trim(),
    customer_id: form.customer_id.trim(),
    rule_type: form.rule_type,
    rate_unit: unit,
    settle_period: form.settle_period,
    effective_from: form.effective_from,
    effective_to: form.effective_to || null,
    status: form.status,
    remark: form.remark.trim() || null,
    rules,
  };
}

/** 档位速览（弹窗底部提示用）：把表单里的档位按当前单位念一遍 */
export function tierPreviewText(form: PolicyFormDraft): string[] {
  const unit = defaultUnitOf(form.rule_type) ?? form.rate_unit;
  return form.rules.map((draft, index) => {
    const rate = Number(String(draft.rate ?? '').trim());
    const rateLabel = Number.isFinite(rate) && rate > 0 ? rateText(rate, unit) : '（未填费率）';
    return `第 ${index + 1} 档 ${tierRangeText(draft.tier_from, draft.tier_to)} ⇒ ${rateLabel}`;
  });
}

/** 费率输入后缀（`PER_UNIT` = 元/件，`PERCENT` = %） */
export function rateSuffix(unit: RebateRateUnit): string {
  return unit === 'PERCENT' ? '%' : '元/件';
}

/** 费率输入提示（防「把 2% 填成 0.02」） */
export function ratePlaceholder(unit: RebateRateUnit): string {
  return unit === 'PERCENT' ? '百分数，例如 2 表示 2%' : '每件返多少元，例如 0.5';
}

/** 金额展示（档位备注里可能出现的金额；与仓内其它列表同口径） */
export const moneyText = formatMoney;
