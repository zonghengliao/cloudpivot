/**
 * 供应商评估页面纯逻辑（M4-16 后端接口 + 本卡前端）。
 *
 * 与 `.vue` 分家的理由：权重和 / 预览总分 / 期间预设 / 表单校验这几处是**最容易被浮点和
 * 「差不多等于 1」坑到**的地方，单测要能直接打（SFC 里的东西 `node --test` 加载不到）。
 *
 * # 口径来源（不许在页面里重新发明）
 *
 * | 口径 | 出处 |
 * |------|------|
 * | 等级阈值 A≥90 / B≥80 / C≥70 / D<70（含下界） | `supplier_evaluation_grading::GRADE_THRESHOLDS` |
 * | 总分 = round2(Σ 权重 × 分数)，四舍五入（中点远离零） | 库层生成列 + `weighted_total` |
 * | 权重各 0–1 且**之和精确 = 1** | `Weights::is_valid` = 库层 `ck_biz_sev_weights` |
 * | 分数各 0–100 | `DimensionScores::is_valid` = 库层 `ck_biz_sev_scores` |
 * | 期间起 ≤ 止、评估日 ≥ 期间起 | `ck_biz_sev_period` / `ck_biz_sev_eval_date` |
 * | 四等分默认权重 0.25 | `Weights::equal` = 库层列默认值 |
 *
 * # 一个必须记住的事实
 *
 * **总分与等级是后端算的**（总分是库层生成列，等级按阈值写在库里）。本文件里的
 * `previewTotal()` / `previewGrade()` 只用于「边填边看」，页面必须标成「预览」，
 * 保存后一律以后端返回的 `total_score` / `grade` 为准 —— 两处若不一致，是后端/库层漂移，
 * 不是前端该去「修正」的东西。
 */

/** 分数 / 权重的定点精度（两者都用 4 位小数参与运算，够覆盖「权重 0.3333」这类录入） */
const SCALE_DIGITS = 4;
/** 10^SCALE_DIGITS */
const SCALE = 10 ** SCALE_DIGITS;

/** 默认每页条数（与后端 `EvaluationPageQuery::filter` 的默认值一致） */
export const PAGE_SIZE = 20;

/** 关键字长度上限（后端 `dto::MAX_KEYWORD`：超了 400，页面先提示不截断） */
export const MAX_KEYWORD = 64;

/** 四维（顺序与后端 `DIMENSIONS` 一致：质量 / 交期 / 价格 / 服务） */
export const DIMENSIONS = ['quality', 'delivery', 'price', 'service'] as const;

/** 维度类型 */
export type Dimension = (typeof DIMENSIONS)[number];

/** 维度中文名 */
export const DIMENSION_LABELS: Record<Dimension, string> = {
  quality: '质量',
  delivery: '交期',
  price: '价格',
  service: '服务',
};

/** 等级闭集（与后端 `GRADES` 逐字一致） */
export type EvaluationGrade = 'A' | 'B' | 'C' | 'D';

/** 等级下拉（值里**不放** `''` 当「全部」：TDesign 把空串当未选，会显示成 placeholder） */
export const GRADE_OPTIONS: { value: EvaluationGrade; label: string }[] = [
  { value: 'A', label: 'A（优秀）' },
  { value: 'B', label: 'B（良好）' },
  { value: 'C', label: 'C（合格需改进）' },
  { value: 'D', label: 'D（不合格）' },
];

/**
 * 等级阈值（与后端 `GRADE_THRESHOLDS` **逐字一致**，只用于预览）。
 *
 * 一旦后端调阈值（改一行常量 + 跑一次 regrade），这里必须同步 —— 不一致的后果是
 * 「预览说 B、保存后是 C」，比不预览更糟。所以 `previewGrade` 只用于表单提示，
 * 且文案写明「以后端为准」。
 */
export const GRADE_THRESHOLDS: { grade: EvaluationGrade; min: number }[] = [
  { grade: 'A', min: 90 },
  { grade: 'B', min: 80 },
  { grade: 'C', min: 70 },
  { grade: 'D', min: 0 },
];

/** 权重预设（四者之和都**精确**等于 1，与「权重和必须是 1」同口径） */
export const WEIGHT_PRESETS: { key: string; label: string; hint: string; weights: Record<Dimension, string> }[] = [
  {
    key: 'equal',
    label: '四等分',
    hint: '四个维度各 0.25（与库层列默认值一致）',
    weights: { quality: '0.25', delivery: '0.25', price: '0.25', service: '0.25' },
  },
  {
    key: 'quality',
    label: '质量优先',
    hint: '质量 0.4 / 交期 0.3 / 价格 0.2 / 服务 0.1',
    weights: { quality: '0.4', delivery: '0.3', price: '0.2', service: '0.1' },
  },
  {
    key: 'delivery',
    label: '交期优先',
    hint: '交期 0.4 / 质量 0.3 / 价格 0.2 / 服务 0.1',
    weights: { quality: '0.3', delivery: '0.4', price: '0.2', service: '0.1' },
  },
  {
    key: 'price',
    label: '价格优先',
    hint: '价格 0.4 / 质量 0.3 / 交期 0.2 / 服务 0.1',
    weights: { quality: '0.3', delivery: '0.2', price: '0.4', service: '0.1' },
  },
];

// ==================== 定点小数（不碰浮点） ====================

/**
 * 定点字符串 → 整数（`digits` 位小数）。
 *
 * 返回 `null` 表示「不是合法定点数」或「小数位超过精度」——**不静默截断**
 * （截断等于替用户改数字，权重和可能因此从 1 变成 0.9999）。
 */
export function toScaled(text: string, digits = SCALE_DIGITS): number | null {
  const raw = String(text ?? '').trim();
  if (!/^-?\d+(\.\d+)?$/.test(raw)) {
    return null;
  }
  const negative = raw.startsWith('-');
  const body = negative ? raw.slice(1) : raw;
  const [intPart, fracPart = ''] = body.split('.');
  if (fracPart.length > digits) {
    return null;
  }
  const padded = (fracPart + '0'.repeat(digits)).slice(0, digits);
  const value = Number(intPart) * 10 ** digits + Number(padded || '0');
  return negative ? -value : value;
}

/** 整数（`digits` 位小数）→ 定点字符串，至少保留 `keep` 位（默认 2）。 */
export function fromScaled(value: number, digits = SCALE_DIGITS, keep = 2): string {
  const negative = value < 0;
  const abs = Math.abs(value);
  const intPart = Math.trunc(abs / 10 ** digits);
  const frac = String(abs % 10 ** digits).padStart(digits, '0').slice(0, Math.max(keep, 0));
  return `${negative ? '-' : ''}${intPart}${frac ? `.${frac}` : ''}`;
}

/**
 * 四维权重之和（定点加法，返回 `keep` 位小数的字符串）。
 *
 * 空 / 非法项**不计入**（表单里允许边填边看），所以「和」可能不是 4 项之和 ——
 * 页面要同时显示「已填几项」，见 {@link weightsState}。
 */
export function weightsSum(weights: Partial<Record<Dimension, string>>, keep = 4): string {
  let total = 0;
  for (const dim of DIMENSIONS) {
    const scaled = toScaled(weights[dim] ?? '');
    if (scaled !== null) {
      total += scaled;
    }
  }
  return fromScaled(total, SCALE_DIGITS, keep);
}

/** 权重状态（页面「权重和 0.9999 ≠ 1」的提示用） */
export type WeightsState = {
  /** 和（字符串） */
  sumText: string;
  /** 精确等于 1？ */
  isOne: boolean;
  /** 四项都填了？ */
  filled: number;
  /** 四项都在 0–1？（空项不算越界） */
  inRange: boolean;
};

/** 权重状态（判据与后端 `Weights::is_valid` 一致：各 0–1 且之和**精确** = 1）。 */
export function weightsState(weights: Partial<Record<Dimension, string>>): WeightsState {
  let total = 0;
  let filled = 0;
  let inRange = true;
  for (const dim of DIMENSIONS) {
    const scaled = toScaled(weights[dim] ?? '');
    if (scaled === null) {
      continue;
    }
    filled += 1;
    total += scaled;
    if (scaled < 0 || scaled > SCALE) {
      inRange = false;
    }
  }
  return {
    sumText: fromScaled(total, SCALE_DIGITS, 4),
    isOne: filled === 4 && total === SCALE && inRange,
    filled,
    inRange,
  };
}

/**
 * 预览加权总分：`round2(Σ 权重 × 分数)`，**四舍五入（中点远离零）**，与库层生成列同公式。
 *
 * 实现是定点整数运算（权重与分数各 4 位小数 → 乘积 8 位 → 和 → 一次性舍到 2 位），
 * 不用 `Number` 浮点相乘：`0.25 × 99.99` 在浮点下是 24.997500000000002，
 * 攒四项再舍入就可能差 0.01，而库层是精确 numeric。
 *
 * 任一项非法（空 / 超精度）→ `null`（页面显示「—」，不显示一个错的数）。
 */
export function previewTotal(
  weights: Partial<Record<Dimension, string>>,
  scores: Partial<Record<Dimension, string>>,
): string | null {
  let sum = 0;
  for (const dim of DIMENSIONS) {
    const weight = toScaled(weights[dim] ?? '');
    const score = toScaled(scores[dim] ?? '');
    if (weight === null || score === null) {
      return null;
    }
    sum += weight * score;
  }
  // 从 10^8 一次性舍到 10^2：q = 整数部分，rem 是丢弃的小数位
  const unit = 10 ** (SCALE_DIGITS * 2 - 2);
  const quotient = Math.trunc(sum / unit);
  const remainder = sum % unit;
  const rounded = remainder * 2 >= unit ? quotient + 1 : quotient;
  return fromScaled(rounded, 2, 2);
}

/** 预览等级（阈值表逐字复述；越界总分返回 `null` —— 那是算错，不是 D 级）。 */
export function previewGrade(totalText: string | null): EvaluationGrade | null {
  if (totalText === null) {
    return null;
  }
  const scaled = toScaled(totalText, 2);
  if (scaled === null || scaled < 0 || scaled > 100 * 10 ** 2) {
    return null;
  }
  for (const item of GRADE_THRESHOLDS) {
    if (scaled >= item.min * 10 ** 2) {
      return item.grade;
    }
  }
  return null;
}

// ==================== 期间预设 ====================

/** 期间（起止都含，`YYYY-MM-DD`） */
export type PeriodRange = { start: string; end: string };

/** Date → `YYYY-MM-DD`（用本地年月日，不用 `toISOString()` —— 那是 UTC，北京早上 8 点前会少一天）。 */
export function formatDate(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${year}-${month}-${day}`;
}

/** 某年某月的天数（下个月 0 号 = 本月最后一天，闰年由 Date 自己算）。 */
function daysInMonth(year: number, monthIndex: number): number {
  return new Date(year, monthIndex + 1, 0).getDate();
}

/** 某月的整月期间 */
export function monthRange(year: number, monthIndex: number): PeriodRange {
  const last = daysInMonth(year, monthIndex);
  return {
    start: formatDate(new Date(year, monthIndex, 1)),
    end: formatDate(new Date(year, monthIndex, last)),
  };
}

/** 某季度的整季期间（`quarter` 取 1–4） */
export function quarterRange(year: number, quarter: number): PeriodRange {
  const firstMonth = (quarter - 1) * 3;
  const lastMonth = firstMonth + 2;
  return {
    start: formatDate(new Date(year, firstMonth, 1)),
    end: formatDate(new Date(year, lastMonth, daysInMonth(year, lastMonth))),
  };
}

/** 某年的整年期间 */
export function yearRange(year: number): PeriodRange {
  return { start: `${year}-01-01`, end: `${year}-12-31` };
}

/** 当前月 / 当前季 / 当前年（`now` 可注入，单测用） */
export function currentMonthRange(now = new Date()): PeriodRange {
  return monthRange(now.getFullYear(), now.getMonth());
}

/** 当前季度 */
export function currentQuarterRange(now = new Date()): PeriodRange {
  const quarter = Math.floor(now.getMonth() / 3) + 1;
  return quarterRange(now.getFullYear(), quarter);
}

/** 当前年度 */
export function currentYearRange(now = new Date()): PeriodRange {
  return yearRange(now.getFullYear());
}

/** 上一季度（跨年自动退到去年 Q4） */
export function lastQuarterRange(now = new Date()): PeriodRange {
  const current = Math.floor(now.getMonth() / 3) + 1;
  if (current === 1) {
    return quarterRange(now.getFullYear() - 1, 4);
  }
  return quarterRange(now.getFullYear(), current - 1);
}

/** 排名页的期间快选 */
export const PERIOD_PRESETS: { key: string; label: string; range: (now?: Date) => PeriodRange }[] = [
  { key: 'this-quarter', label: '本季度', range: currentQuarterRange },
  { key: 'last-quarter', label: '上季度', range: lastQuarterRange },
  { key: 'this-month', label: '本月', range: currentMonthRange },
  { key: 'this-year', label: '本年', range: currentYearRange },
];

// ==================== 列表筛选 ====================

/** 台账筛选状态（键 = `FilterDef.key`） */
export type EvaluationFilterState = {
  /** 关键字：单号 / 供应商名 / 备注 */
  keyword: string;
  /** 等级（`''` = 全部） */
  grade: EvaluationGrade | '';
  /** 期间起（与 `period_end` 成对；空 = 不按期间筛） */
  period_start: string;
  /** 期间止 */
  period_end: string;
};

/** 空筛选（重置 / 首屏都用它，别在页面里写字面量） */
export function emptyFilter(): EvaluationFilterState {
  return { keyword: '', grade: '', period_start: '', period_end: '' };
}

/**
 * 拼装分页入参。
 *
 * 三条口径（踩过的坑）：
 * 1. 空串 / 纯空格**整键缺席**（后端把 `?keyword=` 当「等于空串」过滤，回 0 行）；
 * 2. 期间两个键**成对出现**（后端允许只给一个，但页面语义是「整段期间」，半截区间查不到东西）；
 * 3. `page` / `page_size` 永远发数字（后端按 u64 解析）。
 */
export function buildEvaluationQuery(
  state: EvaluationFilterState,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  const keyword = state.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (state.grade) {
    query.grade = state.grade;
  }
  const start = state.period_start.trim();
  const end = state.period_end.trim();
  if (start && end) {
    query.period_start = start;
    query.period_end = end;
  }
  return query;
}

/** 关键字超长提示（不截断，让后端原文案兜底） */
export function keywordWarning(keyword: string): string | null {
  const length = keyword.trim().length;
  if (length <= MAX_KEYWORD) {
    return null;
  }
  return `关键字 ${length} 个字符，超过后端上限 ${MAX_KEYWORD}：会被拒绝，请缩短后再查`;
}

// ==================== 打分表单 ====================

/** 打分表单状态 */
export type EvaluationFormState = {
  /** 供应商 id */
  supplier_id: string;
  /** 期间起 */
  period_start: string;
  /** 期间止 */
  period_end: string;
  /** 评估日（空 = 当天） */
  eval_date: string;
  /** 四维权重（字符串，定点） */
  weights: Record<Dimension, string>;
  /** 四维分数（字符串，定点） */
  scores: Record<Dimension, string>;
  /** 备注 */
  remark: string;
};

/** 空表单（默认四等分 + 期间用本月） */
export function emptyForm(now = new Date()): EvaluationFormState {
  const period = currentMonthRange(now);
  return {
    supplier_id: '',
    period_start: period.start,
    period_end: period.end,
    eval_date: '',
    weights: { ...WEIGHT_PRESETS[0].weights },
    scores: { quality: '', delivery: '', price: '', service: '' },
    remark: '',
  };
}

/** 从一行回填表单（重新打分 / 详情页「以此为模板」） */
export function formFromRow(row: {
  supplier_id: string;
  period_start: string;
  period_end: string;
  eval_date: string;
  quality_weight: string;
  delivery_weight: string;
  price_weight: string;
  service_weight: string;
  quality_score: string;
  delivery_score: string;
  price_score: string;
  service_score: string;
  remark: string | null;
}): EvaluationFormState {
  return {
    supplier_id: row.supplier_id,
    period_start: row.period_start,
    period_end: row.period_end,
    eval_date: row.eval_date,
    weights: {
      quality: row.quality_weight,
      delivery: row.delivery_weight,
      price: row.price_weight,
      service: row.service_weight,
    },
    scores: {
      quality: row.quality_score,
      delivery: row.delivery_score,
      price: row.price_score,
      service: row.service_score,
    },
    remark: row.remark ?? '',
  };
}

/**
 * 表单校验（返回**人话**错误列表；空数组 = 可提交）。
 *
 * 判据与后端 `SaveEvaluationReq::parse` 逐条对应 —— 这里先拦一遍是为了少一次往返，
 * **不是**替代服务端（后端仍是唯一闸门，报文原样展示）。
 */
export function formErrors(state: EvaluationFormState): string[] {
  const errors: string[] = [];
  if (!state.supplier_id.trim()) {
    errors.push('请选择供应商');
  }
  const start = state.period_start.trim();
  const end = state.period_end.trim();
  if (!start || !end) {
    errors.push('评估期间起止都要填');
  } else if (start > end) {
    errors.push(`评估期间起 ${start} 晚于止 ${end}`);
  }
  const evalDate = state.eval_date.trim();
  if (evalDate && start && evalDate < start) {
    errors.push(`评估日 ${evalDate} 早于评估期间起点 ${start}`);
  }
  const weights = weightsState(state.weights);
  if (weights.filled !== 4) {
    errors.push('四维权重四个都要填（或用预设一键填入）');
  } else if (!weights.inRange) {
    errors.push('四维权重必须各在 0–1 之间');
  } else if (!weights.isOne) {
    errors.push(`四维权重之和必须精确等于 1（当前 ${weights.sumText}）`);
  }
  for (const dim of DIMENSIONS) {
    const raw = (state.scores[dim] ?? '').trim();
    if (!raw) {
      errors.push(`${DIMENSION_LABELS[dim]}得分必填`);
      continue;
    }
    const scaled = toScaled(raw);
    if (scaled === null) {
      errors.push(`${DIMENSION_LABELS[dim]}得分不是合法数值（最多 4 位小数）`);
      continue;
    }
    if (scaled < 0 || scaled > 100 * SCALE) {
      errors.push(`${DIMENSION_LABELS[dim]}得分必须在 0–100 之间`);
    }
  }
  return errors;
}

/**
 * 表单 → 后端入参。
 *
 * 只发后端认的键：**没有** `total_score` / `grade`（总分是生成列，等级后端按阈值写）。
 * 空评估日不发（后端按北京时间当天填）。
 */
export function buildSavePayload(state: EvaluationFormState): {
  supplier_id: string;
  period_start: string;
  period_end: string;
  eval_date?: string;
  quality_weight: string;
  delivery_weight: string;
  price_weight: string;
  service_weight: string;
  quality_score: string;
  delivery_score: string;
  price_score: string;
  service_score: string;
  remark?: string;
} {
  const payload: ReturnType<typeof buildSavePayload> = {
    supplier_id: state.supplier_id.trim(),
    period_start: state.period_start.trim(),
    period_end: state.period_end.trim(),
    quality_weight: state.weights.quality.trim(),
    delivery_weight: state.weights.delivery.trim(),
    price_weight: state.weights.price.trim(),
    service_weight: state.weights.service.trim(),
    quality_score: state.scores.quality.trim(),
    delivery_score: state.scores.delivery.trim(),
    price_score: state.scores.price.trim(),
    service_score: state.scores.service.trim(),
  };
  const evalDate = state.eval_date.trim();
  if (evalDate) {
    payload.eval_date = evalDate;
  }
  const remark = state.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

// ==================== 展示小工具 ====================

/** 等级颜色（A 绿 / B 蓝 / C 橙 / D 红 / 未评级灰） */
export function gradeTheme(grade: EvaluationGrade | null): 'success' | 'primary' | 'warning' | 'danger' | 'default' {
  switch (grade) {
    case 'A':
      return 'success';
    case 'B':
      return 'primary';
    case 'C':
      return 'warning';
    case 'D':
      return 'danger';
    default:
      return 'default';
  }
}

/** 名次颜色（前三名高亮，其余中性） */
export function rankTheme(rank: number): 'success' | 'warning' | 'primary' | 'default' {
  if (rank === 1) {
    return 'success';
  }
  if (rank === 2) {
    return 'primary';
  }
  if (rank === 3) {
    return 'warning';
  }
  return 'default';
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（本地时区；非法值原样返回） */
export function timeText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(
    date.getMinutes(),
  )}`;
}

/** 权重展示（`0.4 / 0.3 / 0.2 / 0.1`，四维同序） */
export function weightsText(row: {
  quality_weight: string;
  delivery_weight: string;
  price_weight: string;
  service_weight: string;
}): string {
  return [row.quality_weight, row.delivery_weight, row.price_weight, row.service_weight].join(' / ');
}

/** 期间文案（后端给了 `period_label` 就用它，缺了退回 `起 ~ 止`） */
export function periodText(row: { period_label?: string | null; period_start: string; period_end: string }): string {
  const label = (row.period_label ?? '').trim();
  return label || `${row.period_start} ~ ${row.period_end}`;
}

/** 交期参考分文案（**无数据不许显示成 0 分**） */
export function suggestionText(suggestion: {
  docs: number;
  late_docs: number;
  total_qty: string;
  on_time_rate: string | null;
  suggested_score: string | null;
  note: string;
}): string {
  if (suggestion.suggested_score === null || suggestion.on_time_rate === null) {
    return suggestion.note;
  }
  return `准时率 ${suggestion.on_time_rate}%（${suggestion.docs} 张入库单里 ${suggestion.late_docs} 张含逾期行，共 ${suggestion.total_qty} 件）；建议的交期得分 ${suggestion.suggested_score}`;
}
