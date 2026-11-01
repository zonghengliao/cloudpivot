/**
 * 年度结转页的**纯逻辑**（ERP-R4）—— 不 import vue，可被 `tsx --test` 直接单测。
 *
 * # 与后端对齐的硬口径（逐字抄 `backend/src/business/erp/year_end.rs`）
 *
 * | 语义 | 真值 | 出处 |
 * |------|------|------|
 * | 三个权限码 | `business:erp:yearend:{read,close,reopen}` | `year_end.rs::PERM_*` / V0190 `sys_menu` |
 * | 年度状态 | `CLOSED` 已结转 / `NOT_CLOSED` 未结转 | `year_end.rs::STATUS_*` |
 * | 五项前置校验 | 见 `BLOCKER_LABELS` | `year_end.rs::collect_blockers` |
 * | 反年结原因最小字数 | 10 | `year_end.rs::MIN_REOPEN_REASON_CHARS` |
 * | 本年利润 / 利润分配 | `3103` / `3104` | `year_end.rs::ACCOUNT_*` |
 * | 下年期初期间 | `(year + 1)-01` | `year_end.rs::next_year_opening_period` |
 *
 * # 为什么金额一律走 `toMoney`
 *
 * 后端是 `Decimal`，经 JSON 回来可能是 `"1234.0000"` 也可能是 `1234`；
 * JS 的 `+` 对字符串是拼接，直接相加会得到 `"1234.00001000"` 这种事故。
 */

import type { YearEndBlocker, YearEndCheck, YearEndState } from '@/api/business/erp';

/** 三个权限码（与 V0190 种的节点逐字一致） */
export const YEAR_END_PERMISSIONS = {
  read: 'business:erp:yearend:read',
  close: 'business:erp:yearend:close',
  reopen: 'business:erp:yearend:reopen',
} as const;

/** 反年结原因最小字数（后端 400 的判据，前端先提示） */
export const MIN_REOPEN_REASON_CHARS = 10;

/** 阻塞项类型 → 页面上的标题（未知类型原样显示，绝不空白） */
export const BLOCKER_LABELS: Record<string, string> = {
  PERIOD_NOT_CLOSED: '12 个月全部结账',
  DOC_UNFINISHED: '无未完成单据',
  UNBALANCED: '试算平衡',
  INVARIANT: '不变量全绿',
  NEXT_YEAR_NOT_OPENED: '下一年度已开账',
  NEXT_YEAR_OPENING_EXISTS: '下一年度期初未被占用',
  ALREADY_CLOSED: '本年度未结转',
  NOT_CLOSED: '本年度已结转',
};

/** 五项前置校验的固定顺序（页面按它逐条渲染 ✅ / ❌） */
export const CHECK_ORDER = [
  'PERIOD_NOT_CLOSED',
  'DOC_UNFINISHED',
  'UNBALANCED',
  'INVARIANT',
  'NEXT_YEAR_NOT_OPENED',
] as const;

/** 金额入参（字符串 / 数字 / 空）→ number（非法一律 0，不抛错） */
export function toMoney(value: number | string | null | undefined): number {
  if (value === null || value === undefined) {
    return 0;
  }
  if (typeof value === 'number') {
    return Number.isFinite(value) ? value : 0;
  }
  const text = value.trim();
  if (!text) {
    return 0;
  }
  const parsed = Number(text);
  return Number.isFinite(parsed) ? parsed : 0;
}

/** 金额 → 展示文本（两位小数 + 千分位） */
export function moneyText(value: number | string | null | undefined): string {
  return toMoney(value).toLocaleString('zh-CN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

/** 年度状态 → 中文 */
export function statusLabel(state: YearEndState | null | undefined): string {
  if (!state) {
    return '—';
  }
  return state.closed ? `${state.year} 年度已结转` : `${state.year} 年度未结转`;
}

/** 年度状态 → `t-tag` theme */
export function statusTheme(state: YearEndState | null | undefined): 'success' | 'warning' {
  return state?.closed ? 'success' : 'warning';
}

/** 某类型阻塞项（没有就是 undefined） */
export function blockerOf(check: YearEndCheck | null, type: string): YearEndBlocker | undefined {
  return check?.blockers.find((item) => item.type === type);
}

/** 一条前置校验的展示行 */
export interface CheckLine {
  type: string;
  label: string;
  ok: boolean;
  /** ❌ 时的一句话（点名月份 / 张数 / 差额） */
  detail: string;
  link: string | null;
}

/**
 * 五项前置校验的渲染行（**固定五行**：通过的也渲染 ✅，页面不用自己对齐顺序）。
 *
 * 为什么固定五行而不是「只渲染 blockers」：财务看的是「五项都过了吗」，
 * 只渲染失败的项会让人数不清还差几项。
 */
export function checkLines(check: YearEndCheck | null): CheckLine[] {
  return CHECK_ORDER.map((type) => {
    const blocker = blockerOf(check, type);
    return {
      type,
      label: BLOCKER_LABELS[type] ?? type,
      ok: !blocker,
      detail: blocker?.message ?? '',
      link: blocker?.link ?? null,
    };
  });
}

/** 前置校验之外、但同样会拦住年结的两项（已结转 / 下年期初被占用） */
export function extraBlockers(check: YearEndCheck | null): YearEndBlocker[] {
  const fixed = new Set<string>(CHECK_ORDER);
  return (check?.blockers ?? []).filter((item) => !fixed.has(item.type));
}

/**
 * 「年度结转」按钮能不能点。
 *
 * 三条同时满足：① 服务端说 can_close；② 有 close 权限；③ 当前不是「已结转」。
 */
export function canClose(
  check: YearEndCheck | null,
  permission: (code: string) => boolean,
): boolean {
  if (!check || !check.can_close) {
    return false;
  }
  if (check.state?.closed) {
    return false;
  }
  return permission(YEAR_END_PERMISSIONS.close);
}

/** 「反年结」按钮能不能点（已结转 + 有 reopen 权限） */
export function canReopen(
  check: YearEndCheck | null,
  permission: (code: string) => boolean,
): boolean {
  if (!check?.state?.closed) {
    return false;
  }
  return permission(YEAR_END_PERMISSIONS.reopen);
}

/** 年结按钮的禁用原因（`null` = 可点）—— 按钮禁用时必须说清为什么 */
export function closeBlockedReason(
  check: YearEndCheck | null,
  permission: (code: string) => boolean,
): string | null {
  if (!check) {
    return '还没读到年度状态';
  }
  if (!permission(YEAR_END_PERMISSIONS.close)) {
    return '没有年结权限（business:erp:yearend:close）';
  }
  if (check.state?.closed) {
    return `${check.year} 年度已结转：要重做请先反年结`;
  }
  if (!check.can_close) {
    return `还有 ${check.blockers.length} 项前置校验未通过`;
  }
  return null;
}

/** 反年结原因校验（`null` = 通过；与后端同判据：必填 + ≥ 10 字） */
export function reasonError(raw: string | null | undefined): string | null {
  const text = (raw ?? '').trim();
  if (!text) {
    return `反年结必须填写原因（至少 ${MIN_REOPEN_REASON_CHARS} 个字）`;
  }
  const chars = Array.from(text).length;
  if (chars < MIN_REOPEN_REASON_CHARS) {
    return `反年结原因至少 ${MIN_REOPEN_REASON_CHARS} 个字（当前 ${chars} 字）`;
  }
  return null;
}

/** 结转凭证摘要（空年度没有凭证，要说清「不是失败」） */
export function voucherText(state: YearEndState | null | undefined): string {
  if (!state) {
    return '—';
  }
  if (state.carry_forward_voucher_no) {
    return `${state.carry_forward_voucher_no} · ${state.carry_forward_entries} 条分录 · 借 ${moneyText(
      state.carry_forward_total,
    )}`;
  }
  if (state.closed) {
    return '本年度没有损益余额，已跳过结转凭证（空年度不是失败）';
  }
  return '尚未生成结转凭证';
}

/** 下年期初摘要 */
export function openingText(state: YearEndState | null | undefined): string {
  if (!state) {
    return '—';
  }
  if (!state.opening_balance_created) {
    return `${state.opening_period} 期初尚未生成`;
  }
  return `${state.opening_period} 期初已生成（${state.opening_rows} 行，借贷已平）`;
}

/**
 * 年结完成后的提示文案（`carry_forward_voucher_no === null` 时不能吓人）。
 */
export function closeResultText(result: {
  year: number;
  carry_forward_voucher_no: string | null;
  opening_rows: number;
  opening_period?: string;
}): string {
  const opening = result.opening_period ?? `${result.year + 1}-01`;
  if (!result.carry_forward_voucher_no) {
    return `${result.year} 年度已结转：没有损益余额，跳过结转凭证；${opening} 期初已生成 ${result.opening_rows} 行`;
  }
  return `${result.year} 年度已结转：凭证 ${result.carry_forward_voucher_no}，${opening} 期初已生成 ${result.opening_rows} 行`;
}
