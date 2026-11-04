/**
 * 资产分类页纯逻辑（页面只渲染）。
 *
 * 分类是折旧参数的**唯一来源**：使用月数、净残值率、三个科目都从这里带到卡片上。
 * 所以新建 / 编辑表单的校验口径必须与后端 `asset_dto::validate_category` 一致 ——
 * 具体规则在 `../asset/asset-logic.ts` 的 {@link validateCategoryForm}，本文件只补
 * 「列表展示怎么排、状态怎么翻、科目怎么选」这类页面层小事。
 */

import type { AssetCategory } from '@/api/business/erp/asset';
import { DEPRECIATION_METHOD_LABELS } from '@/api/business/erp/asset-kinds';
import type { AccountRow } from '@/api/business/erp';

/** 分类状态（1 启用 / 0 停用，与 `ck_biz_asset_cat_status` 一致）。 */
export const CATEGORY_STATUS_OPTIONS = [
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
] as const;

/** 列表筛选状态（含「全部」）。 */
export const CATEGORY_STATUS_FILTER_OPTIONS = [
  { label: '全部状态', value: '' },
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
] as const;

/** 分类状态 tag 主题。 */
export function categoryStatusTheme(status: number): 'success' | 'default' {
  return status === 1 ? 'success' : 'default';
}

/** 状态中文。 */
export function categoryStatusLabel(status: number): string {
  return status === 1 ? '启用' : '停用';
}

/** 折旧方法中文（本期只有平均年限法；后端加了新方法这里会原样显示英文码）。 */
export function methodLabel(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  return (DEPRECIATION_METHOD_LABELS as Record<string, string>)[raw] ?? String(raw);
}

/** 科目下拉选项（只列末级科目：非末级科目不能记账）。 */
export function accountOptions(accounts: AccountRow[]): Array<{ label: string; value: string }> {
  return accounts
    .filter((account) => account.is_leaf && account.status === 1)
    .map((account) => ({
      label: `${account.account_code} ${account.account_name}`,
      value: account.id,
    }));
}

/** 科目 id → 显示文案（详情 / 列表用；查不到时原样显示 id 前 8 位）。 */
export function accountLabel(accounts: AccountRow[], id: string | null | undefined): string {
  if (!id) {
    return '—';
  }
  const hit = accounts.find((account) => account.id === id);
  return hit ? `${hit.account_code} ${hit.account_name}` : id.slice(0, 8);
}

/** 表单回填：分类行 → 表单值（编辑用）。 */
export function categoryToForm(row: AssetCategory): {
  category_code: string;
  category_name: string;
  useful_life_months: number;
  salvage_rate: string;
  asset_account_id: string;
  accum_account_id: string;
  expense_account_id: string;
  status: number;
  sort_order: number;
  remark: string;
} {
  return {
    category_code: row.category_code,
    category_name: row.category_name,
    useful_life_months: row.useful_life_months,
    salvage_rate: row.salvage_rate,
    asset_account_id: row.asset_account_id,
    accum_account_id: row.accum_account_id,
    expense_account_id: row.expense_account_id,
    status: row.status,
    sort_order: row.sort_order,
    remark: row.remark ?? '',
  };
}

/** 分类 → 建卡下拉的一句说明（年限 + 残值率，用户最关心的两个参数）。 */
export function categorySummary(row: AssetCategory): string {
  return `${row.useful_life_months} 个月 · 残值率 ${row.salvage_rate}`;
}
