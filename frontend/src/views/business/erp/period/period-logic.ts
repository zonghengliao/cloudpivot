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

// ==================== 开账 / 结账 / 反结账（ERP-P1） ====================

/**
 * 期间三个动作的权限码（逐字抄 `V0188__erp_period_close.up.sql` 与
 * `period.rs::{PERM_OPEN,PERM_CLOSE,PERM_REOPEN}`）。
 *
 * 三个码**必须分开**：能看期间列表（`:list`）不等于能封账（`:close`），
 * 能封账不等于能撤封账（`:reopen`，通常只给财务总监 / 超管）。
 */
export const PERIOD_ACTION_PERMISSIONS = {
  open: 'business:erp:period:open',
  close: 'business:erp:period:close',
  reopen: 'business:erp:period:reopen',
  /** 催办（PERIOD-CLOSE）：能给处理人发催办 ≠ 有资格替他把单过账，所以另立一码 */
  remind: 'business:erp:period:remind',
} as const;

/** 期间行上可用的动作 */
export type PeriodAction = 'open' | 'close' | 'reopen';

/**
 * 某一行**当前可以点哪个动作**（纯函数，页面按它 v-if 出按钮）。
 *
 * | 状态 | 动作 |
 * |------|------|
 * | `NOT_OPENED` | 开账 |
 * | `OPEN` | 结账 |
 * | `CLOSED` | 反结账 |
 *
 * 未知状态一律不给动作（宁可少一个按钮，也不要把「结账」挂到一个看不懂的状态上）。
 */
export function periodActionOf(status: string | null | undefined): PeriodAction | null {
  switch ((status ?? '').trim()) {
    case 'NOT_OPENED':
      return 'open';
    case 'OPEN':
      return 'close';
    case 'CLOSED':
      return 'reopen';
    default:
      return null;
  }
}

/** 动作的中文名（按钮 / 弹窗标题 / toast 共用） */
export const PERIOD_ACTION_LABELS: Record<PeriodAction, string> = {
  open: '开账',
  close: '结账',
  reopen: '反结账',
};

/**
 * 该行的按钮**是否应该出现**：既要状态允许，也要有对应权限码。
 *
 * **无权限 → 按钮不渲染**（卡面硬红线 5），不是置灰：置灰等于在没权限的人页面上
 * 永久留一个点不动的按钮，还暗示「你可以申请一下」。
 */
export function canRunPeriodAction(
  status: string | null | undefined,
  hasPermission: (code: string) => boolean,
): boolean {
  const action = periodActionOf(status);
  if (!action) {
    return false;
  }
  return hasPermission(PERIOD_ACTION_PERMISSIONS[action]);
}

/** 反结账原因的最小字数（与后端 `MIN_REOPEN_REASON_CHARS` 一致） */
export const MIN_REOPEN_REASON_CHARS = 10;

/**
 * 反结账原因的前端校验（返回 null = 通过，否则是错误文案）。
 *
 * 后端一定会再校验一次 —— 这里先拦是为了「不用往返一次才知道自己少写了两个字」，
 * 文案与后端逐字对齐，用户不会看到两套说法。
 */
export function reopenReasonError(reason: string | null | undefined): string | null {
  const text = (reason ?? '').trim();
  if (!text) {
    return '反结账必须填写原因：撤销已封账的月份会让人在已结账期间改账，原因要能让审计看懂「为什么」';
  }
  const chars = [...text].length;
  if (chars < MIN_REOPEN_REASON_CHARS) {
    return `反结账原因至少 ${MIN_REOPEN_REASON_CHARS} 个字（当前 ${chars} 字）`;
  }
  return null;
}

/** 结账备注上限（与后端 `MAX_CLOSE_REASON_CHARS` 一致） */
export const MAX_CLOSE_REASON_CHARS = 200;

/** 结账备注校验（可选；只在超长时报错） */
export function closeReasonError(reason: string | null | undefined): string | null {
  const chars = [...((reason ?? '').trim())].length;
  if (chars > MAX_CLOSE_REASON_CHARS) {
    return `结账备注最多 ${MAX_CLOSE_REASON_CHARS} 字（当前 ${chars} 字）`;
  }
  return null;
}

/**
 * 结账弹窗的一句话摘要（预检结果 → 人能一眼判断的文字）。
 *
 * - 全部通过 → 「前置校验全部通过，可以结账」
 * - 有阻塞 → 「还有 N 项阻塞：<第一条文案>」+ 后续条数
 */
export function closeCheckSummary(check: {
  can_close: boolean;
  blockers: ReadonlyArray<{ message: string }>;
}): string {
  if (check.can_close || check.blockers.length === 0) {
    return '前置校验全部通过，可以结账';
  }
  const first = check.blockers[0]?.message ?? '未知阻塞项';
  const rest = check.blockers.length - 1;
  return rest > 0 ? `${first}（另有 ${rest} 项）` : first;
}

/**
 * 批量开账的入参校验（`YYYY-MM` 严格、from ≤ to、跨度 ≤ 120 个月）。
 *
 * 与后端 `parse_year_month` / `MAX_BATCH_MONTHS` 同口径：前端先拦一次，
 * 少一次「提交后才知道填错」的往返。
 */
export function batchOpenError(from: string, to: string): string | null {
  const pattern = /^\d{4}-(0[1-9]|1[0-2])$/;
  const fromText = (from ?? '').trim();
  const toText = (to ?? '').trim();
  if (!pattern.test(fromText)) {
    return `起始期间必须是 YYYY-MM（如 2026-11），当前是「${fromText || '空'}」`;
  }
  if (!pattern.test(toText)) {
    return `结束期间必须是 YYYY-MM（如 2027-12），当前是「${toText || '空'}」`;
  }
  const fromKey = Number(fromText.slice(0, 4)) * 12 + Number(fromText.slice(5, 7));
  const toKey = Number(toText.slice(0, 4)) * 12 + Number(toText.slice(5, 7));
  if (fromKey > toKey) {
    return `起始期间 ${fromText} 晚于结束期间 ${toText}`;
  }
  const months = toKey - fromKey + 1;
  if (months > 120) {
    return `一次最多开 120 个期间（当前 ${months} 个）：期间行建出来不可删除`;
  }
  return null;
}

// ==================== 结账 / 年结共用的「挡路单据」展示逻辑 ====================
//
// 这些函数（单据类型中文 / 角色中文 / 金额 / 已等 / 可催办判定 / 可催办明细 ……）
// 与「会计期间」没有半点关系，年结检查清单也要用同一套口径，所以搬到了
// `../shared/blocker-logic.ts`，这里原样再导出 —— 老调用方（`CloseCheckPanel.vue`、
// `tests/unit/erp-period-close-items.test.ts`）的 import 路径不用改。
export * from '../shared/blocker-logic';
