/**
 * 会计期间页的**纯逻辑**（P4-C）——不 import vue，可被 `tsx --test` 直接单测。
 *
 * # 与后端对齐的硬口径（`psql \d biz_accounting_period` 佐证）
 *
 * - 状态只有 3 个字面量：`ck_biz_period_status` = `NOT_OPENED | OPEN | CLOSED`
 *   （**没有「已结账未结平」「试算」这类中间态**，结账是 `OPEN → CLOSED` 一步）；
 * - `period_code` 是 `VARCHAR(7)` 的 `yyyy-MM`，同时还有 `period_year` / `period_month` 两列，
 *   三者冗余（后端 `uk_biz_period_ym` 唯一约束保证不打架）—— 页面用 `period_code` 展示、
 *   用 `period_year` 筛选；
 * - `closed_at` / `closed_by` 只在 `CLOSED` 后有值，其它状态恒 `NULL`。
 */

import type { AccountPeriodRow, PeriodStatus } from '@/api/business/erp';

/** 期间状态中文（键与 `ck_biz_period_status` 逐字一致） */
const PERIOD_STATUS_LABELS: Record<string, string> = {
  NOT_OPENED: '未开启',
  OPEN: '已开启',
  CLOSED: '已结账',
};

/** 期间状态 → 中文（未知值原样返回，绝不显示空白） */
export function periodStatusLabel(status: string | null | undefined): string {
  const key = (status ?? '').trim();
  if (!key) {
    return '—';
  }
  return PERIOD_STATUS_LABELS[key] ?? key;
}

/**
 * 期间状态 → `t-tag` 主题色。
 *
 * - 未开启 = 灰（还没开通，不用关注）；
 * - 已开启 = 主色（可记账，当前正在用的那个）；
 * - 已结账 = 成功色（本月账已封，改不了）。视觉上「已完成」用成功色比用灰更符合记账人的预期。
 */
export function periodStatusTheme(status: string | null | undefined): 'default' | 'primary' | 'success' {
  switch ((status ?? '').trim()) {
    case 'OPEN':
      return 'primary';
    case 'CLOSED':
      return 'success';
    default:
      return 'default';
  }
}

/** 状态下拉项（顺序照 `NOT_OPENED → OPEN → CLOSED` 的生命周期，不按字母排） */
export const PERIOD_STATUS_OPTIONS: Array<{ value: PeriodStatus; label: string }> = [
  { value: 'NOT_OPENED', label: '未开启' },
  { value: 'OPEN', label: '已开启' },
  { value: 'CLOSED', label: '已结账' },
];

/** 期间编码 → 中文标题（`2026-10` → `2026 年 10 月`；不合格式的原样返回） */
export function periodTitle(code: string | null | undefined): string {
  const match = /^(\d{4})-(\d{2})$/.exec((code ?? '').trim());
  if (!match) {
    return (code ?? '').trim() || '—';
  }
  return `${match[1]} 年 ${Number(match[2])} 月`;
}

/** 日期区间文案（`2026-10-01 ~ 2026-10-31`） */
export function periodRangeText(row: Pick<AccountPeriodRow, 'start_date' | 'end_date'>): string {
  const start = (row.start_date ?? '').trim();
  const end = (row.end_date ?? '').trim();
  if (!start || !end) {
    return '—';
  }
  return `${start} ~ ${end}`;
}

/**
 * 是否「当前会计期间」。
 *
 * 判定用 `period_code === 本月 yyyy-MM`（不用 `start_date <= today <= end_date`：
 * 那样跨月补账时会同时highlight两个期间）。`now` 显式传入，单测才好钉死。
 */
export function isCurrentPeriod(
  row: Pick<AccountPeriodRow, 'period_code'>,
  now: Date = new Date(),
): boolean {
  const year = now.getFullYear();
  const month = `${now.getMonth() + 1}`.padStart(2, '0');
  return (row.period_code ?? '').trim() === `${year}-${month}`;
}

/** 期间天数（含首尾；`end < start` 时返回 0，脏数据不抛错） */
export function periodDays(row: Pick<AccountPeriodRow, 'start_date' | 'end_date'>): number {
  const start = Date.parse(`${row.start_date}T00:00:00Z`);
  const end = Date.parse(`${row.end_date}T00:00:00Z`);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) {
    return 0;
  }
  return Math.round((end - start) / 86_400_000) + 1;
}

/** 页面筛选状态（`''` / `null` = 全部） */
export interface PeriodFilterState {
  /** 年份（`null` = 全部年份） */
  period_year: number | null;
  /** 状态（`''` = 全部状态） */
  status: PeriodStatus | '';
}

/** 空筛选 */
export function emptyPeriodFilter(): PeriodFilterState {
  return { period_year: null, status: '' };
}

/** 筛选是否有内容 */
export function isPeriodFiltered(filter: PeriodFilterState): boolean {
  return filter.period_year !== null || filter.status !== '';
}

/**
 * 年份下拉项：**数据里出现过的年份 + 当前年**，倒序（最近的在最上面）。
 *
 * 不从 1970 铺到 2100 —— 期间是逐月建的，没建的年份选了也是空列表，
 * 摆一堆选不出东西的选项纯属噪音。
 */
export function periodYearOptions(rows: readonly Pick<AccountPeriodRow, 'period_year'>[], now: Date = new Date()): number[] {
  const years = new Set<number>([now.getFullYear()]);
  for (const row of rows) {
    if (typeof row.period_year === 'number' && Number.isFinite(row.period_year)) {
      years.add(row.period_year);
    }
  }
  return [...years].sort((left, right) => right - left);
}

/**
 * 组装 `/period/page` 的查询入参。
 *
 * 与 `dto.rs::PeriodFilter` 一一对应：只发真给了的值（`null` / `''` 一律不发，
 * 否则后端把 `?status=` 当「状态等于空串」查 0 行）。
 */
export function buildPeriodQuery(
  filter: PeriodFilterState,
  page = 1,
  pageSize = 20,
): { period_year?: number; status?: PeriodStatus; page: number; page_size: number } {
  const query: { period_year?: number; status?: PeriodStatus; page: number; page_size: number } = {
    page,
    page_size: pageSize,
  };
  if (typeof filter.period_year === 'number' && Number.isFinite(filter.period_year)) {
    query.period_year = filter.period_year;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  return query;
}

/**
 * 结账时间文案。
 *
 * `CLOSED` 但 `closed_at` 为空（历史数据 / 后端漏写）时如实说「已结账（无结账时间）」，
 * 不拿 `updated_at` 顶替 —— 结账时间在财务上是证据，不能编。
 */
export function closedAtText(row: Pick<AccountPeriodRow, 'status' | 'closed_at'>): string {
  const at = (row.closed_at ?? '').trim();
  if (at) {
    return at.slice(0, 19).replace('T', ' ');
  }
  return row.status === 'CLOSED' ? '已结账（无结账时间）' : '—';
}
