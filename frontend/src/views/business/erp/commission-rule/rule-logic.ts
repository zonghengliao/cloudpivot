/**
 * 佣金规则页纯逻辑（SO-37 / ARCH-7 §15.4.2 ②）—— 可测的判断都在这里，`.vue` 只渲染。
 *
 * # 口径（写错会静默算错钱，全部抄库层 CHECK）
 *
 * | 项 | 真口径 | 出处 |
 * |----|--------|------|
 * | 范围 ↔ 对象 | `ALL ⇒ scope_id = null`；`SALESPERSON / MATERIAL_CATEGORY ⇒ scope_id 必填` | `ck_biz_cmrule_scope_id` |
 * | 百分比 | `0 ≤ rate ≤ 100`（**百分数**，5 就是 5%） | `ck_biz_cmrule_rate` |
 * | 固定额 | `rate ≥ 0`（与基数无关，每笔给这么多） | 同上 |
 * | 金额区间 | `min_amount ≥ 0` 且 `max_amount ≥ min_amount`（null = 不封顶） | `ck_biz_cmrule_amount` |
 * | 有效期 | `effective_to ≥ effective_from`（null = 长期有效；**两端都含**） | `ck_biz_cmrule_effective` |
 * | 命中优先级 | 销售员 > 物料 > 全局；同级 生效日新 → 创建新 → id 小 | `commission_handler::pick_rule` |
 *
 * # 为什么前端也要校验一遍
 *
 * 库层 CHECK 的 400 文案是「参数校验失败」，用户看不出是哪个字段错了。前端先拦一道
 * 能给「封顶不能小于起算金额」这种人话；后端仍会再判一次（前端不是安全边界）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { formatMoney, shortId } from '@/views/business/erp/shared/money';
import type {
  CommissionCalcType,
  CommissionRuleFilter,
  CommissionRuleRow,
  CommissionRuleStatus,
  CommissionScopeType,
  CreateCommissionRuleReq,
  DecimalText,
} from '@/api/business/erp/commission';

/** 每页条数（与仓内列表页同口径；后端上限 200、`0` → 20） */
export const RULE_PAGE_SIZE = 20;

/** 规则名上限（`V0309` 的 `rule_name VARCHAR(100)`） */
export const RULE_NAME_MAX = 100;

/** 备注上限（`remark TEXT`，纯防手滑；后端不截断） */
export const RULE_REMARK_MAX = 500;

/** 范围候选（顺序 = 匹配优先级从高到低，用户一眼看出「哪个更优先」） */
export const SCOPE_OPTIONS: Array<{ label: string; value: CommissionScopeType }> = [
  { label: '按销售员（最高优先级）', value: 'SALESPERSON' },
  { label: '按物料（中优先级）', value: 'MATERIAL_CATEGORY' },
  { label: '全局（兜底）', value: 'ALL' },
];

/** 计算方式候选 */
export const CALC_OPTIONS: Array<{ label: string; value: CommissionCalcType }> = [
  { label: '百分比（按佣金基数 × 比率）', value: 'PERCENT' },
  { label: '固定额（每笔固定金额）', value: 'FIXED' },
];

/** 规则状态候选（`''` = 全部，排第一） */
export const RULE_STATUS_OPTIONS: Array<{
  label: string;
  value: '' | CommissionRuleStatus;
}> = [
  { label: '全部状态', value: '' },
  { label: '启用', value: 'ACTIVE' },
  { label: '停用', value: 'INACTIVE' },
];

/** 范围筛选候选（`''` = 全部） */
export const SCOPE_FILTER_OPTIONS: Array<{
  label: string;
  value: '' | CommissionScopeType;
}> = [
  { label: '全部范围', value: '' },
  ...SCOPE_OPTIONS.map((item) => ({ label: item.label, value: item.value })),
];

/** 范围中文名（后端字段是英文枚举，页面不直接显示它） */
export const SCOPE_LABELS: Record<CommissionScopeType, string> = {
  SALESPERSON: '按销售员',
  MATERIAL_CATEGORY: '按物料',
  ALL: '全局',
};

/** 计算方式中文名 */
export const CALC_LABELS: Record<CommissionCalcType, string> = {
  PERCENT: '百分比',
  FIXED: '固定额',
};

/** 规则状态中文名 */
export const RULE_STATUS_LABELS: Record<CommissionRuleStatus, string> = {
  ACTIVE: '启用',
  INACTIVE: '停用',
};

/** 规则列表列（`op` 由页面用 `#op` 插槽渲染） */
export const RULE_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'rule_name', title: '规则名', minWidth: 180, ellipsis: true },
  { colKey: 'scope_type', title: '适用范围', width: 110 },
  { colKey: 'scope_display', title: '范围对象', minWidth: 160, ellipsis: true },
  { colKey: 'calc_type', title: '计算方式', width: 100 },
  { colKey: 'rate', title: '费率 / 金额', width: 130, align: 'right' },
  { colKey: 'amount_range', title: '起算 / 封顶', width: 170, align: 'right' },
  { colKey: 'validity', title: '有效期', width: 190 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 130, fixed: 'right' },
];

// ==================== 文案 / 格式化 ====================

/** 范围中文名（未知值回落编码本身，别显示空白） */
export function scopeTypeLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return SCOPE_LABELS[value as CommissionScopeType] ?? value;
}

/** 计算方式中文名 */
export function calcTypeLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return CALC_LABELS[value as CommissionCalcType] ?? value;
}

/** 规则状态中文名 */
export function ruleStatusLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return RULE_STATUS_LABELS[value as CommissionRuleStatus] ?? value;
}

/** 规则状态配色（启用 = 成功色，停用 = 灰） */
export function ruleStatusTheme(
  value: string | null | undefined,
): 'success' | 'default' | 'warning' | 'danger' {
  if (value === 'ACTIVE') {
    return 'success';
  }
  if (value === 'INACTIVE') {
    return 'default';
  }
  return 'warning';
}

/**
 * 数字文本化（保留小数、去掉尾随 0 之外不做四舍五入）。
 *
 * 金额列是 `NUMERIC(18,4)`、比率是 `NUMERIC(8,4)`，后端统一发字符串。页面只做
 * 「按 2 位（金额）/ 4 位（比率）展示」，**不自己算钱**。
 */
export function decimalText(value: DecimalText | null | undefined, places = 4): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return num.toFixed(places);
}

/** 金额展示：走 `shared/money.ts` 的整数分实现（冲回行的负号必须留得住） */
export const moneyText = formatMoney;

/**
 * 费率 / 金额列文案。
 *
 * `PERCENT` 必须带 `%` —— 只写 `5.0000` 会让人以为是 5 元；`FIXED` 必须带「元/单」，
 * 否则会被当成百分比。
 */
export function rateText(row: Pick<CommissionRuleRow, 'calc_type' | 'rate'>): string {
  if (row.calc_type === 'PERCENT') {
    return `${decimalText(row.rate, 4)}%`;
  }
  return `${moneyText(row.rate)} 元/单`;
}

/** 起算 / 封顶文案（封顶为 null 时写「不封顶」，别留空） */
export function amountRangeText(
  row: Pick<CommissionRuleRow, 'min_amount' | 'max_amount'>,
): string {
  const min = moneyText(row.min_amount);
  const max =
    row.max_amount === null || row.max_amount === undefined
      ? '不封顶'
      : moneyText(row.max_amount);
  return `${min} / ${max}`;
}

/** 有效期文案（两端都含；`effective_to` 空 = 长期有效） */
export function validityText(
  from: string | null | undefined,
  to: string | null | undefined,
): string {
  const start = from || '—';
  return `${start} ~ ${to || '长期'}`;
}

/**
 * 范围对象展示：优先用后端补的 `scope_name`；没有就用 `scope_id` 前 8 位。
 *
 * 为什么不隐藏：`scope_id` 是 UUID，前 8 位足够在列表里区分（点编辑能看到全量），
 * 比显示「—」让用户以为规则没绑对象要好。
 */
export function scopeDisplay(row: CommissionRuleRow): string {
  if (row.scope_type === 'ALL') {
    return '全部（无范围限制）';
  }
  const name = (row.scope_name ?? '').trim();
  if (name) {
    return name;
  }
  if (!row.scope_id) {
    return '—（未绑定范围对象）';
  }
  return `${row.scope_type === 'SALESPERSON' ? '销售员' : '物料'} ${shortId(row.scope_id)}`;
}

// ==================== 表单 ====================

/** 表单草稿（金额 / 比率允许 `t-input-number` 回写的数字；提交前统一转字符串，不做算术） */
export interface RuleFormDraft {
  rule_name: string;
  scope_type: CommissionScopeType;
  scope_id: string;
  calc_type: CommissionCalcType;
  /** `PERCENT` ⇒ 百分数；`FIXED` ⇒ 金额 */
  rate: number | string;
  min_amount: number | string;
  /** 空串 = 不封顶 */
  max_amount: number | string;
  effective_from: string;
  /** 空串 = 长期有效 */
  effective_to: string;
  status: CommissionRuleStatus;
  remark: string;
}

/** 空表单（新建用；`today` 由调用方给，便于测试与「北京时间今天」口径） */
export function emptyRuleForm(today: string): RuleFormDraft {
  return {
    rule_name: '',
    scope_type: 'SALESPERSON',
    scope_id: '',
    calc_type: 'PERCENT',
    rate: '',
    min_amount: '0',
    max_amount: '',
    effective_from: today,
    effective_to: '',
    status: 'ACTIVE',
    remark: '',
  };
}

/** 列表行 → 表单（编辑回填；`null` 一律折成空串，输入框只认字符串） */
export function ruleFormFrom(row: CommissionRuleRow): RuleFormDraft {
  return {
    rule_name: row.rule_name,
    scope_type: row.scope_type,
    scope_id: row.scope_id ?? '',
    calc_type: row.calc_type,
    rate: toPlain(row.rate),
    min_amount: toPlain(row.min_amount),
    max_amount: row.max_amount === null || row.max_amount === undefined ? '' : toPlain(row.max_amount),
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
  };
}

/** `1.0000` → `1`（回填输入框时别让用户看到 4 位多余 0） */
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

/** 日期形状（`yyyy-MM-dd`，只判形状 —— 真日历由 `t-date-picker` 保证） */
function isDateString(text: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(text);
}

/**
 * 表单校验（返回第一条人话错误；`null` = 通过）。
 *
 * 顺序刻意如此：先判「必填」，再判「语义」，用户改起来最短。
 */
export function validateRuleForm(form: RuleFormDraft): string | null {
  const name = form.rule_name.trim();
  if (!name) {
    return '请填写规则名';
  }
  if (name.length > RULE_NAME_MAX) {
    return `规则名最多 ${RULE_NAME_MAX} 字`;
  }
  if (form.scope_type === 'ALL') {
    if (form.scope_id) {
      return '全局规则的适用范围对象必须为空（范围对象只有「按销售员 / 按物料」才填）';
    }
  } else if (!form.scope_id) {
    return form.scope_type === 'SALESPERSON'
      ? '请选择销售员（按销售员的规则必须绑定一个人）'
      : '请选择物料（按物料的规则必须绑定一个物料）';
  }
  const rate = Number(form.rate);
  if (form.rate === '' || !Number.isFinite(rate)) {
    return form.calc_type === 'PERCENT' ? '请填写费率（百分数，例如 5）' : '请填写固定金额';
  }
  if (rate < 0) {
    return '费率 / 金额不能为负';
  }
  if (form.calc_type === 'PERCENT' && rate > 100) {
    return '百分比不能超过 100（5 表示 5%，不是 0.05）';
  }
  const min = Number(form.min_amount === '' ? 0 : form.min_amount);
  if (!Number.isFinite(min) || min < 0) {
    return '起算金额必须是不小于 0 的数字';
  }
  if (form.max_amount !== '') {
    const max = Number(form.max_amount);
    if (!Number.isFinite(max)) {
      return '封顶金额必须是数字（留空 = 不封顶）';
    }
    if (max < min) {
      return '封顶金额不能小于起算金额';
    }
  }
  if (!isDateString(form.effective_from)) {
    return '请选择生效起始日';
  }
  if (form.effective_to) {
    if (!isDateString(form.effective_to)) {
      return '生效截止日格式必须是 yyyy-MM-dd';
    }
    if (form.effective_to < form.effective_from) {
      return '生效截止日不能早于生效起始日';
    }
  }
  if (form.remark.length > RULE_REMARK_MAX) {
    return `备注最多 ${RULE_REMARK_MAX} 字`;
  }
  return null;
}

/** 表单 → 接口入参（`ALL` 的 `scope_id` 必须发 `null`，否则库层 CHECK 拒） */
export function buildRulePayload(form: RuleFormDraft): CreateCommissionRuleReq {
  return {
    rule_name: form.rule_name.trim(),
    scope_type: form.scope_type,
    scope_id: form.scope_type === 'ALL' ? null : form.scope_id,
    calc_type: form.calc_type,
    rate: form.rate === '' ? '0' : String(Number(form.rate)),
    min_amount: form.min_amount === '' ? '0' : String(Number(form.min_amount)),
    max_amount: form.max_amount === '' ? null : String(Number(form.max_amount)),
    effective_from: form.effective_from,
    effective_to: form.effective_to || null,
    status: form.status,
    remark: form.remark.trim() || null,
  };
}

/**
 * 切换范围类型时清 `scope_id`。
 *
 * `ALL` 不允许带 `scope_id`（库层 CHECK），而「改了范围但对象还留着」是最常见的 400。
 */
export function onScopeTypeChange(form: RuleFormDraft, next: CommissionScopeType): void {
  form.scope_type = next;
  if (next === 'ALL') {
    form.scope_id = '';
  }
}

/**
 * 切换计算方式时清 `rate`。
 *
 * `PERCENT` 的 5 是「5%」，`FIXED` 的 5 是「5 元」—— 留着旧值等于把费率悄悄变成金额
 * （5% 变 5 元/单，金额差一个数量级），必须清空让人重填。
 */
export function onCalcTypeChange(form: RuleFormDraft, next: CommissionCalcType): void {
  if (form.calc_type !== next) {
    form.rate = '';
  }
  form.calc_type = next;
}

/** 费率输入提示（随计算方式变，避免用户把 5% 填成 0.05） */
export function ratePlaceholder(calc: CommissionCalcType): string {
  return calc === 'PERCENT' ? '百分数，例如 5 表示 5%' : '每笔固定金额，例如 50';
}

/** 费率单位后缀 */
export function rateSuffix(calc: CommissionCalcType): string {
  return calc === 'PERCENT' ? '%' : '元/单';
}

// ==================== 筛选 ====================

/** 空筛选 */
export function emptyRuleFilter(): CommissionRuleFilter {
  return { keyword: '', scope_type: '', status: '', page: 1, page_size: RULE_PAGE_SIZE };
}

/** 是否带了筛选条件（决定空态文案是「暂无数据」还是「没有匹配的规则」） */
export function isRuleFiltered(filter: CommissionRuleFilter): boolean {
  return Boolean(
    (filter.keyword ?? '').trim() || filter.scope_type || filter.status,
  );
}

/** 列表空态文案 */
export function ruleEmptyText(filter: CommissionRuleFilter): string {
  return isRuleFiltered(filter)
    ? '没有匹配的佣金规则：试试放宽筛选，或新建一条'
    : '还没有佣金规则：先建一条「全局」兜底规则，再按销售员 / 物料细化';
}
