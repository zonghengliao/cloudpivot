/**
 * 期初余额页的**纯逻辑**（ERP-R5）—— 不 import vue，可被 `tsx --test` 直接单测。
 *
 * # 与后端对齐的硬口径（逐字抄 `backend/src/business/erp/opening.rs`）
 *
 * | 语义 | 真值 | 出处 |
 * |------|------|------|
 * | 三个权限码 | `business:erp:opening:{read,write,inventory}` | `opening.rs::PERM_*` / V0189 `sys_menu` |
 * | 状态 | `NONE` 未录 / `ENTERED` 已录 / `LOCKED` 已锁定 | `opening.rs::STATE_*` |
 * | 期间状态 | `OPEN` / `CLOSED` / `NOT_OPENED` | `ck_biz_period_status` |
 * | 库存期初借方科目 | `1405` | `opening.rs::ACCOUNT_INVENTORY` |
 * | 库存期初贷方科目 | `3103` | `opening.rs::ACCOUNT_INVENTORY_OFFSET` |
 * | 不平衡业务码 | `66012` | `shared/error_code.rs::ERP_OPENING_UNBALANCED` |
 *
 * # 为什么金额一律走 `toMoney`
 *
 * 后端是 `Decimal`，经 JSON 回来可能是 `"50000.0000"`（字符串）也可能是 `50000`（数字），
 * 取决于序列化路径。直接 `a + b` 会得到 `"50000.0000" + 1000 === "50000.00001000"` 这种
 * 事故（JS 的 `+` 对字符串是拼接）——所以**所有金额先过 `toMoney` 再算**。
 */

import type { OpeningBalance, OpeningEntry, OpeningInventoryLine } from '@/api/business/erp';
import { todayPeriod } from '../shared/money';

/** 三个权限码（与 V0189 种的节点逐字一致） */
export const OPENING_PERMISSIONS = {
  read: 'business:erp:opening:read',
  write: 'business:erp:opening:write',
  inventory: 'business:erp:opening:inventory',
} as const;

/** 期初状态的样式（`t-tag` theme） */
export function openingStateTheme(state: string | null | undefined): 'default' | 'primary' | 'success' | 'warning' {
  switch ((state ?? '').trim()) {
    case 'ENTERED':
      return 'primary';
    case 'LOCKED':
      return 'success';
    default:
      return 'warning';
  }
}

/** 期初状态 → 中文（未知值原样返回，绝不显示空白） */
export function openingStateLabel(state: string | null | undefined): string {
  switch ((state ?? '').trim()) {
    case 'ENTERED':
      return '期初已录';
    case 'LOCKED':
      return '已锁定';
    case 'NONE':
      return '期初未录';
    default: {
      const raw = (state ?? '').trim();
      return raw || '—';
    }
  }
}

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

/** 金额 → 展示文本（两位小数 + 千分位；空值显示 `0.00`） */
export function moneyText(value: number | string | null | undefined): string {
  const amount = toMoney(value);
  return amount.toLocaleString('zh-CN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

/** 合计「科目期初」的借贷两侧（**不含**库存期初） */
export function sumEntries(entries: OpeningEntry[]): { debit: number; credit: number } {
  let debit = 0;
  let credit = 0;
  for (const row of entries) {
    debit += toMoney(row.debit);
    credit += toMoney(row.credit);
  }
  return { debit, credit };
}

/** 借贷平衡判定（差额保留两位以避免浮点噪声） */
export function balanceOf(debit: number, credit: number): { balanced: boolean; difference: number } {
  const difference = Math.round((debit - credit) * 100) / 100;
  return { balanced: difference === 0, difference };
}

/**
 * 页面顶部的只读原因（`null` = 可编辑）。
 *
 * - 期间已结账 → 整页只读（后端也会 409，但前端要**先说**）；
 * - 期间没开账 → 不能录（后端 400 + 66010）。
 */
export function readonlyReason(opening: OpeningBalance | null): string | null {
  if (!opening) {
    return null;
  }
  if (opening.locked || opening.period_status === 'CLOSED') {
    return `${opening.period} 已结账，期初不可改`;
  }
  if (opening.period_status === 'NOT_OPENED') {
    return `${opening.period} 还没开账：请先在「会计期间」页开账，再录期初`;
  }
  return null;
}

/**
 * 保存按钮可用性。
 *
 * 卡面「不平保存按钮禁用」——判据用**服务端算好的合并口径**（`opening.balanced`），
 * 不是前端自己再算一遍：库存期初会自动生成 1405 借方，前端算不全。
 */
export function canSave(
  opening: OpeningBalance | null,
  permission: (code: string) => boolean,
  kind: 'account' | 'inventory',
): boolean {
  if (!opening || readonlyReason(opening) !== null) {
    return false;
  }
  if (!permission(kind === 'account' ? OPENING_PERMISSIONS.write : OPENING_PERMISSIONS.inventory)) {
    return false;
  }
  return opening.balanced;
}

/** 一条录入行的即时校验（`null` = 通过） */
export function rowError(row: { account_code?: string; debit?: number | string; credit?: number | string }): string | null {
  const code = (row.account_code ?? '').trim();
  if (!code) {
    return '请选择科目';
  }
  const debit = toMoney(row.debit);
  const credit = toMoney(row.credit);
  if (debit < 0 || credit < 0) {
    return '期初金额不能为负';
  }
  if (debit > 0 && credit > 0) {
    return '借方与贷方不能同时填';
  }
  return null;
}

/** 有内容的行（两列都 0 的行后端会直接丢掉，前端也不该拦着保存） */
export function effectiveEntries<T extends { debit?: number | string; credit?: number | string }>(rows: T[]): T[] {
  return rows.filter((row) => toMoney(row.debit) !== 0 || toMoney(row.credit) !== 0);
}

/** 期初凭证摘要（`PZ-202611-000001 · 2026-11-01 · 4 条分录`） */
export function voucherText(opening: OpeningBalance | null): string {
  if (!opening?.voucher) {
    return '尚未生成期初凭证';
  }
  const voucher = opening.voucher;
  return `${voucher.voucher_no} · ${voucher.voucher_date} · ${voucher.entry_count} 条分录 · 借 ${moneyText(voucher.total_debit)}`;
}

/** 库存期初合计（金额 / 数量两种口径，列表脚注用） */
export function sumInventory(lines: OpeningInventoryLine[]): { qty: number; amount: number } {
  let qty = 0;
  let amount = 0;
  for (const row of lines) {
    qty += toMoney(row.qty);
    amount += toMoney(row.amount);
  }
  return { qty, amount };
}

/**
 * 期初凭证的「对方科目」说明（库存期初自动生成的 1405 / 3103 两行）。
 *
 * 为什么要在页面上说清楚：财务看到凭证里凭空多出两行会以为是系统 bug，
 * 而它其实是库存期初的自动产物（卡面 §四 ④）。
 */
export function inventoryVoucherHint(opening: OpeningBalance | null): string | null {
  if (!opening) {
    return null;
  }
  const amount = toMoney(opening.inventory_amount);
  if (amount === 0) {
    return null;
  }
  return `库存期初 ${moneyText(amount)} 会自动生成 借 ${opening.inventory_debit_account} / 贷 ${opening.inventory_credit_account} 两行分录`;
}

// ==================== 首次进入的默认期间 ====================

/** 期间行里本逻辑需要的两列（`AccountPeriodRow` 的子集，便于单测直接造） */
export interface OpeningPeriodOptionSource {
  /** 期间编码 `yyyy-MM` */
  period_code: string;
  /** 期间状态 `OPEN` / `CLOSED` / `NOT_OPENED` */
  status: string;
}

/**
 * 首次进入期初页该落哪个期间（`periodModel` 为空时调用）。
 *
 * # 为什么不能「取第一个 OPEN」
 *
 * `fetchPeriodPage` 按 `period_code DESC` 返回 ⇒ 「第一个 OPEN」是**最新**的已开期间。
 * 2026-11-06 进页面就落到 2026-12（全 0.00、状态「未录」），用户会以为「期初数据丢了」，
 * 而实际数据在 2026-11 —— 这是 P1-FIX-9 衍生卡 F3 的原始缺陷。
 *
 * # 选期顺序（每一档都要有兜底，绝不返回空串把页面打成「未选期间」）
 *
 * 1. **当前月**（北京时间 `todayPeriod()`）且它是 `OPEN` → 用它（绝大多数场景：当期做账）；
 * 2. 否则取**最早**的 `OPEN` 期间（倒序列表里的最后一个；期间码是 `yyyy-MM`，字典序=时间序）。
 *    「最早未录」里的「未录」判定要逐期间查期初状态（`GET /opening-balance` 一次一个期间），
 *    首屏为它发 N 个请求不划算 ⇒ 退一档用「最早 OPEN」，用户看到的仍是最可能已录的那个月；
 * 3. 一个 `OPEN` 都没有（全结账 / 尚未开账）→ 取**最新**一条（后端第一条），只用来展示状态与只读提示。
 *
 * @param rows 期间清单（`fetchPeriodPage` 的 `list`，顺序不敏感，内部自己排）
 * @param now  当前时间（默认 `new Date()`，单测注入固定时刻）
 */
export function defaultOpeningPeriod(
  rows: readonly OpeningPeriodOptionSource[],
  now: Date = new Date(),
): string {
  if (rows.length === 0) {
    return '';
  }
  const byCodeAsc = [...rows].sort((left, right) => left.period_code.localeCompare(right.period_code));
  const opens = byCodeAsc.filter((row) => row.status === 'OPEN');
  if (opens.length === 0) {
    const latest = byCodeAsc[byCodeAsc.length - 1];
    return latest?.period_code ?? '';
  }
  const currentPeriod = todayPeriod(now);
  const hit = opens.find((row) => row.period_code === currentPeriod);
  return hit ? hit.period_code : (opens[0]?.period_code ?? '');
}
