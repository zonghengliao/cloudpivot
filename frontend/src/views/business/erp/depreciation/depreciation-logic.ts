/**
 * 折旧计提页纯逻辑（页面只渲染）。
 *
 * 三行汇总的算法不在页面里 —— 在 `../asset/asset-logic.ts` 的
 * {@link summarizeDepreciation}（那里同时解释了「为什么应提要按月序数推、
 * 而不是读 `last_depreciation_period`」，以及差异非 0 时该怎么读）。
 * 本文件只管：批次能不能红冲、差异行怎么措辞、K4 合计自检、期间下拉。
 */

import type { DepBatch } from '@/api/business/erp/depreciation';
import {
  DEP_BATCH_STATUS_LABELS,
  DEP_BATCH_STATUS_THEMES,
  type DepBatchStatus,
} from '@/api/business/erp/asset-kinds';
import {
  addMonthsToPeriod,
  cmpMoney,
  currentPeriod,
  formatMoney,
  previousPeriod,
  type DepreciationSummary,
} from '../asset/asset-logic';

/** 批次状态 tag 主题。 */
export function depBatchTheme(
  raw: string | null | undefined,
): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  if (!raw) {
    return 'default';
  }
  return DEP_BATCH_STATUS_THEMES[raw as DepBatchStatus] ?? 'default';
}

/** 批次状态中文。 */
export function depBatchLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return DEP_BATCH_STATUS_LABELS[raw as DepBatchStatus] ?? String(raw);
}

/** 只有已过账（POSTED）的批次可以整批红冲（后端 66039 的镜像）。 */
export function canReverseBatch(batch: Pick<DepBatch, 'status'>): boolean {
  return batch.status === 'POSTED';
}

/** 期间下拉：上 12 个月 + 当月（按时间倒序）。 */
export function periodOptions(today?: string): Array<{ label: string; value: string }> {
  const now = currentPeriod(today);
  const out: Array<{ label: string; value: string }> = [];
  for (let i = 0; i < 13; i += 1) {
    const period = addMonthsToPeriod(now, -i);
    out.push({ label: periodLabel(period), value: period });
  }
  return out;
}

/** 默认期间 = 上一个会计月（与后端 `run` 不传期间、定时任务同口径）。 */
export function defaultPeriod(today?: string): string {
  return previousPeriod(today);
}

/** `"2026-09"` → `"2026 年 09 月"`。 */
export function periodLabel(period: string): string {
  if (!period || period.length < 7) {
    return period || '—';
  }
  return `${period.slice(0, 4)} 年 ${period.slice(5, 7)} 月`;
}

/** 差异行的措辞与语气（三行汇总的第三行）。 */
export function diffDisplay(summary: DepreciationSummary): {
  label: string;
  value: string;
  hint: string;
  theme: 'success' | 'warning' | 'danger';
} {
  if (summary.diff_level === 'OK') {
    return {
      label: '差异',
      value: '0.00',
      hint: summary.has_posted_batch ? '账实相符：该期间已按卡片口径提足' : '该期间尚未计提',
      theme: 'success',
    };
  }
  if (summary.diff_level === 'UNDER') {
    return {
      label: '差异（少提）',
      value: formatMoney(summary.diff_amount),
      hint: '应提大于已提：可能本期尚未计提，或有资产期初导入 / 红冲后需重跑',
      theme: 'warning',
    };
  }
  return {
    label: '差异（多提）',
    value: formatMoney(summary.diff_amount),
    hint: '已提大于应提：可能处置后未及时停提，或存在重复计提批次，请核对明细',
    theme: 'danger',
  };
}

/**
 * K4 自检：批次合计必须等于明细合计。
 *
 * 后端的巡检 K4 钉的是库里那两列，这里在页面上再比一次 —— 明细表就摆在旁边，
 * 对不上时给一条黄条，比让财务自己拿计算器加 30 行靠谱。
 */
export function detailTotalMatches(batch: Pick<DepBatch, 'total_amount'>, detailTotal: string): boolean {
  return cmpMoney(batch.total_amount, detailTotal) === 0;
}

/** 计提结果提示（toast 文案）。 */
export function runResultText(outcome: string, message: string, amount: string): string {
  const prefix: Record<string, string> = {
    POSTED: '计提成功',
    EXISTS: '本期已计提（幂等，不重提）',
    SKIPPED: '本期无应提折旧',
    REVERSED_RERUN: '红冲后重提成功',
  };
  const head = prefix[outcome] ?? outcome;
  const body = message ? `：${message}` : '';
  const money = outcome === 'POSTED' || outcome === 'REVERSED_RERUN' ? `（本次 ${formatMoney(amount)}）` : '';
  return `${head}${money}${body}`;
}
