/**
 * 制造费用科目（`4101.*`）的判定与下拉候选 —— **费用台账与分摊规则共用**。
 *
 * # 为什么抽到 shared
 *
 * 两个页面都要回答同一个问题："哪些科目算制造费用、能进成本？"分开写两份，
 * 迟早一边接受 `4101.` 另一边拒绝，于是「费用台账能存、分摊规则选不到」这种鬼故事就出来了。
 * 口径逐条抄后端：
 *
 * | 地方 | 规则 |
 * |------|------|
 * | `ck_biz_cost_oh_debit` / `ck_biz_cost_rule_account` | `account_code LIKE '4101.%'` |
 * | `cost_overhead_dto::is_manufacturing_account` | 前缀 `4101.` 之后**还有内容**且每段都是数字 |
 * | `ensure_account`（后端查库） | 账套里**存在 + 末级 + 启用**（否则 `400 + 66060`） |
 *
 * 前置段（前缀 + 分段数字）在前端先拦，库层校验以后端为准；末级/启用只是把
 * 明显不可能的挡在下拉外。
 */

/** 制造费用科目前缀（`ck_biz_cost_oh_debit` / `ck_biz_cost_rule_account`） */
export const MANUFACTURING_ACCOUNT_PREFIX = '4101.';

/**
 * 是否是制造费用科目（`4101.x`，分段全数字）。
 *
 * `4101.` / `4101.abc` / `41011` 都不是 —— 前者没有末级编码，后者前缀不成立。
 */
export function isManufacturingAccount(code: string): boolean {
  const text = code.trim();
  if (!text.startsWith(MANUFACTURING_ACCOUNT_PREFIX)) {
    return false;
  }
  const rest = text.slice(MANUFACTURING_ACCOUNT_PREFIX.length);
  if (!rest) {
    return false;
  }
  return rest.split('.').every((segment) => segment.length > 0 && /^\d+$/.test(segment));
}

/** 科目候选的最小形状（`AccountRow` 满足它） */
export interface AccountLike {
  /** 科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string;
  /** 是否末级 */
  is_leaf: boolean;
  /** 启用状态（1 启用 / 0 停用） */
  status: number;
}

/** 制造费用科目下拉（借方 / 被摊科目）：`4101.*` + 末级 + 启用，按编码排序 */
export function pickManufacturingAccounts<T extends AccountLike>(
  accounts: T[],
): Array<{ label: string; value: string }> {
  return accounts
    .filter((item) => item.is_leaf && item.status === 1 && isManufacturingAccount(item.account_code))
    .map((item) => ({ label: `${item.account_code} ${item.account_name}`, value: item.account_code }))
    .sort((left, right) => left.value.localeCompare(right.value));
}

/** 对方科目下拉：所有启用末级科目，排除制造费用本身（贷方不可能是 4101.*） */
export function pickCreditAccounts<T extends AccountLike>(
  accounts: T[],
): Array<{ label: string; value: string }> {
  return accounts
    .filter((item) => item.is_leaf && item.status === 1 && !isManufacturingAccount(item.account_code))
    .map((item) => ({ label: `${item.account_code} ${item.account_name}`, value: item.account_code }))
    .sort((left, right) => left.value.localeCompare(right.value));
}
