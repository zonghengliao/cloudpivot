/**
 * 报表项维护 API（后端 `report_item_admin.rs` / `report_item_model.rs`，卡 P1-FIX-8B）。
 *
 * `biz_report_item` 是**资产负债表 + 利润表共用的唯一取数配置**：一行 = 报表上的一个项目
 * 要么按科目取数（`account_pattern`）、要么引用其它项算出来（`formula`），
 * 再加上层级 / 排序 / 启停。报表每次编制都现查这张表，所以**改完刷新报表立即生效**。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 报表编码 | `report_code`（`BALANCE_SHEET` / `INCOME_STATEMENT`） | `ItemRes` |
 * | 项编码 | `item_code`（同报表内唯一，公式靠它引用） | 同上 |
 * | 取数模式 | `account_pattern` | 同上 |
 * | 计算式 | `formula` | 同上 |
 * | 层级 / 排序 | `item_level` / `sort_order` | 同上 |
 * | 引用者 | `referenced_by`（停用前必须为空） | 同上 |
 * | 命中科目数 | `matched_account_count`（`null` = 公式项 / 标题行） | 同上 |
 *
 * # 三个容易踩的点
 *
 * 1. **路径写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **编码不可改**：`item_code` 是公式的引用键，编辑时后端 400，页面把它渲染成只读；
 * 3. **保存返回 `warnings`**：模式命中 0 个科目这类「不阻断但会算错」的情况不进 400，
 *    由页面用 `MessagePlugin.warning` 明确告警 —— 不要吞掉。
 */

import { get, post, put } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 报表项（逐字抄后端 `ItemRes`）。 */
export interface ReportItem {
  id: string;
  report_code: string;
  report_name: string;
  item_code: string;
  item_name: string;
  item_level: number;
  parent_code: string | null;
  side: string;
  account_pattern: string | null;
  formula: string | null;
  is_deduction: boolean;
  is_active: boolean;
  sort_order: number;
  remark: string | null;
  updated_at: string | null;
  /** 纯标题行（层级 1 且无取数来源 ⇒ 金额恒为空） */
  is_title: boolean;
  /** 引用本项的其它报表项编码（非空时不能停用） */
  referenced_by: string[];
  /** 取数模式当前命中的科目数（`null` = 公式项 / 标题行） */
  matched_account_count: number | null;
}

/** 保存结果（后端 `ItemSaveRes`）。 */
export interface ReportItemSaveResult {
  item: ReportItem;
  /** 非阻断告警（模式命中 0 个科目等），页面必须显示 */
  warnings: string[];
}

/** 报表字典（后端 `CatalogRes`）。 */
export interface ReportItemCatalog {
  reports: Array<{ report_code: string; report_name: string; item_count: number }>;
  sides: Array<{ value: string; label: string }>;
  levels: Array<{ value: string; label: string }>;
  pattern_tips: Array<{ syntax: string; meaning: string; example: string }>;
  total_items: number;
}

/** 取数预览结果（后端 `PreviewRes`）。 */
export interface ReportItemPreview {
  matched_count: number;
  matched_accounts: Array<{ account_code: string; account_name: string; account_category: string }>;
  /** 阻断性问题（保存会被拒） */
  problems: string[];
  /** 非阻断告警 */
  warnings: string[];
}

/** 新增 / 编辑请求体（后端 `ItemUpsertReq`；空串 = 清空该字段）。 */
export interface ReportItemPayload {
  report_code?: string;
  item_code?: string;
  item_name?: string;
  item_level?: number;
  parent_code?: string;
  side?: string;
  account_pattern?: string;
  formula?: string;
  is_deduction?: boolean;
  is_active?: boolean;
  sort_order?: number;
  remark?: string;
}

/** 报表项列表（含停用行，按 `sort_order` 升序）。 */
export function fetchReportItems(reportCode?: string): Promise<ReportItem[]> {
  return get<ReportItem[]>(`${BASE}/report-item/list`, reportCode ? { report_code: reportCode } : {});
}

/** 字典：报表 / 所属方 / 层级 / 取数模式语法。 */
export function fetchReportItemCatalog(): Promise<ReportItemCatalog> {
  return get<ReportItemCatalog>(`${BASE}/report-item/catalog`);
}

/** 新增报表项。 */
export function createReportItem(payload: ReportItemPayload): Promise<ReportItemSaveResult> {
  return post<ReportItemSaveResult>(`${BASE}/report-item`, payload);
}

/** 编辑报表项（`report_code` / `item_code` 不可变）。 */
export function updateReportItem(
  id: string,
  payload: ReportItemPayload,
): Promise<ReportItemSaveResult> {
  return put<ReportItemSaveResult>(`${BASE}/report-item/${id}`, payload);
}

/** 启停报表项（被公式引用时后端 409）。 */
export function toggleReportItem(id: string, isActive: boolean): Promise<ReportItem> {
  return put<ReportItem>(`${BASE}/report-item/${id}/toggle`, { is_active: isActive });
}

/** 整表重排：`ids` 必须是该报表全部报表项的一个排列。 */
export function sortReportItems(reportCode: string, ids: string[]): Promise<ReportItem[]> {
  return put<ReportItem[]>(`${BASE}/report-item/sort`, { report_code: reportCode, ids });
}

/** 取数预览：这条模式会命中哪些科目 / 公式引用是否合法。 */
export function previewReportItem(payload: {
  report_code: string;
  account_pattern?: string;
  formula?: string;
  exclude_id?: string;
  sort_order?: number;
}): Promise<ReportItemPreview> {
  return post<ReportItemPreview>(`${BASE}/report-item/preview`, payload);
}
