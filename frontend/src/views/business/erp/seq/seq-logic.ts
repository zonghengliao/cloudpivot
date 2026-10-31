/**
 * 单号序列页的**纯逻辑**（卡 t_6f023cee / BUG-SYS-01）—— 不 import vue，
 * 可被 `tsx --test` 直接单测。
 *
 * # 这张表到底是什么（别当成「已发出的单号」）
 *
 * `biz_seq` = **取号规则字典**：`seq_type`（`PO` / `IN` / `TR` / `VC` / `SN`…）唯一，
 * `period_key` + `current_no` 是当前周期计数器的状态，`step` 是每次取号的步长。
 * 取号走 `UPDATE … RETURNING`（见 `purchase_model.rs::bump_seq`），本页只读。
 *
 * ⚠️ 真正拼成单号的模板由**各业务模块自己**决定：
 * `purchase_model.rs::format_order_no` 写死 `PO-{period_key}-{no:06}`，
 * 根本不读本表的 `prefix` / `separator`。所以本页**不给「下一个单号」的完整预览**
 * —— 那会是一个看起来很像、但和实际发出的号不一致的字符串（比不显示更坏）。
 * 只展示「下次取号的序号 = current_no + step」这个确定的事实。
 *
 * # 字段口径
 *
 * `dto.rs::SeqRes`：`seq_type / seq_name / prefix / date_pattern / period_key /
 *  current_no / step / separator / status / remark / created_at / updated_at`。
 * `status` 是 `i16`（1 启用 / 0 停用），不是布尔。
 */

import type { SeqRow } from '@/api/business/erp';

/** 页面筛选状态（`''` / `null` = 全部） */
export interface SeqFilterState {
  /** 关键词（后端匹配 `seq_type` / `seq_name`） */
  keyword: string;
  /** 状态（`null` = 全部） */
  status: number | null;
}

/** 空筛选 */
export function emptySeqFilter(): SeqFilterState {
  return { keyword: '', status: null };
}

/** 筛选是否有内容 */
export function isSeqFiltered(filter: SeqFilterState): boolean {
  return Boolean(filter.keyword.trim()) || filter.status !== null;
}

/** 组装 `/seq/page` 的查询入参（空值不发，见 `dto.rs::SeqFilter`） */
export function buildSeqQuery(
  filter: SeqFilterState,
  page = 1,
  pageSize = 20,
): { keyword?: string; status?: number; page: number; page_size: number } {
  const query: { keyword?: string; status?: number; page: number; page_size: number } = {
    page,
    page_size: pageSize,
  };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status !== null) {
    query.status = filter.status;
  }
  return query;
}

/** 状态下拉（`1` 启用 / `0` 停用，与 `status` 列的 `i16` 同口径） */
export const SEQ_STATUS_OPTIONS: Array<{ value: number; label: string }> = [
  { value: 1, label: '启用' },
  { value: 0, label: '停用' },
];

/** 状态 → 中文（未知值原样返回，绝不空白） */
export function seqStatusLabel(status: number | string | null | undefined): string {
  if (status === null || status === undefined || status === '') {
    return '—';
  }
  const num = Number(status);
  if (num === 1) {
    return '启用';
  }
  if (num === 0) {
    return '停用';
  }
  return String(status);
}

/** 状态 → `t-tag` 主题（停用用灰色：不是错误，只是这个取号器不再发新号） */
export function seqStatusTheme(status: number | string | null | undefined): 'primary' | 'default' {
  return Number(status) === 1 ? 'primary' : 'default';
}

/**
 * 日期段格式 → 中文说明。
 *
 * 后端只认三种（`serial_model.rs::period_key_of` 的取值域）：
 * `yyyyMMdd`（按日归零）/ `yyyyMM`（按月归零）/ 其它（不按周期归零，种子写 `none`）。
 */
export function datePatternText(pattern: string | null | undefined): string {
  const key = (pattern ?? '').trim();
  switch (key) {
    case 'yyyyMMdd':
      return '按日（yyyyMMdd）';
    case 'yyyyMM':
      return '按月（yyyyMM）';
    case '':
    case 'none':
      return '不按周期';
    default:
      return key;
  }
}

/** 当前周期键文案（`none` 口径下 `period_key` 是空串 → 「—」，不是「无」） */
export function periodKeyText(row: Pick<SeqRow, 'period_key' | 'date_pattern'>): string {
  const key = (row.period_key ?? '').trim();
  if (key) {
    return key;
  }
  return datePatternText(row.date_pattern) === '不按周期' ? '—' : '（尚未取号）';
}

/**
 * 下次取号的序号 = `current_no + step`。
 *
 * 只在同一周期内成立；跨周期时后端会先归零再加步长（所以真正发出的号可能更小）。
 * 文案里点明「同周期」，避免财务拿这个数去对账。
 */
export function nextNoOf(row: Pick<SeqRow, 'current_no' | 'step'>): number {
  const current = Number(row.current_no);
  const step = Number(row.step);
  const safeCurrent = Number.isFinite(current) ? current : 0;
  const safeStep = Number.isFinite(step) && step > 0 ? step : 1;
  return safeCurrent + safeStep;
}

/** 分隔符文案（空串 = 无分隔符，显示「无」而不是留白） */
export function separatorText(value: string | null | undefined): string {
  const raw = (value ?? '').trim();
  return raw ? raw : '无';
}
