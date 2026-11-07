/**
 * 销售目标 / 预测 API 层（后端 `backend/src/business/erp/sales_target_handler.rs`，SO-44）。
 *
 * # 三块接口
 *
 * | 页面区 | 接口 |
 * |--------|------|
 * | 达成率看板 | `GET /api/business/erp/sales-target/achievement?period_code=&scope_type=&basis=` |
 * | 目标维护 | `GET /api/business/erp/sales-target/page` + `POST` / `PUT {id}` / `DELETE {id}` |
 * | 销售预测 | `GET /api/business/erp/sales-forecast/page` + `POST` / `PUT {id}` / `DELETE {id}` |
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 范围类型 | `scope_type`：`SALESPERSON` / `DEPT` / `MATERIAL_CATEGORY` / `ALL` | `ck_biz_st_target_scope` |
 * | 范围对象 | `scope_id`（`ALL` 必须为 `null`，其余必须给） | `ck_biz_st_target_scope_id` |
 * | 期间码 | `period_code`：`yyyy-MM` 或 `yyyy-QN`（**不是** `yyyyMM`） | `ck_biz_st_target_period` |
 * | 目标金额 / 数量 | `target_amount`（2 位小数）/ `target_qty`（4 位，0 = 没定） | `TargetRes` |
 * | 达成率 | `achievement_rate`（**百分数**，`85.50` 就是 85.5%） | `AchievementRow` |
 * | 缺口 | `gap` = 目标 − 实际（**负数 = 超额**） | 同上 |
 * | 两个口径 | `by_order` / `by_receipt` 各有 `actual_amount / achievement_rate / gap` | 同上 |
 * | 信心度 | `confidence`：`HIGH` / `MEDIUM` / `LOW` | `ck_biz_sf_confidence` |
 * | 版本 | `version_no`（同维度同期间同版本唯一） | `uk_biz_sales_forecast_dim` |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **金额 / 达成率在 JSON 里是字符串**（后端 `Decimal`），`t-input-number` 给数字 ——
 *    提交前统一 `String(value)`，展示统一 `formatMoney()`；
 * 3. **`achievement_rate` 可能 > 100**（超额）：进度条要夹到 100，数字照实显示；
 * 4. **`scope_id` 三态**：新建时 `ALL` 必须不带（传 `null`），其余必须给 uuid；
 *    编辑时「不传」= 不改，「传 null」= 清空（只有 `ALL` 成立）。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 金额：后端 `NUMERIC` 在 JSON 里是字符串，但 `t-input-number` 给数字 */
export type DecimalText = string | number;

// ==================== 域值 ====================

/** 目标范围类型 */
export type ScopeType = 'SALESPERSON' | 'DEPT' | 'MATERIAL_CATEGORY' | 'ALL';

/** 达成率口径：按订单金额 / 按已收款金额 */
export type AchievementBasisKey = 'ORDER' | 'RECEIPT';

/** 目标状态 */
export type TargetStatus = 'ACTIVE' | 'INACTIVE';

/** 预测信心度 */
export type Confidence = 'HIGH' | 'MEDIUM' | 'LOW';

/** 范围下拉（顺序与后端 SCOPE_TYPES 一致） */
export const SCOPE_OPTIONS: { value: ScopeType; label: string }[] = [
  { value: 'SALESPERSON', label: '销售员' },
  { value: 'DEPT', label: '部门' },
  { value: 'MATERIAL_CATEGORY', label: '物料' },
  { value: 'ALL', label: '全公司' },
];

/** 口径下拉 */
export const BASIS_OPTIONS: { value: AchievementBasisKey; label: string }[] = [
  { value: 'ORDER', label: '按订单金额' },
  { value: 'RECEIPT', label: '按已收款金额' },
];

/** 状态下拉 */
export const TARGET_STATUS_OPTIONS: { value: TargetStatus; label: string }[] = [
  { value: 'ACTIVE', label: '启用' },
  { value: 'INACTIVE', label: '停用' },
];

/** 信心度下拉 */
export const CONFIDENCE_OPTIONS: { value: Confidence; label: string }[] = [
  { value: 'HIGH', label: '高' },
  { value: 'MEDIUM', label: '中' },
  { value: 'LOW', label: '低' },
];

// ==================== 目标 ====================

/** 目标一行 */
export interface TargetRow {
  id: string;
  /** `STG-yyyyMMdd-000001` */
  target_no: string;
  scope_type: ScopeType;
  scope_id: string | null;
  /** 范围显示名（销售员姓名 / 部门名 / 物料名；`ALL` 为 null） */
  scope_name: string | null;
  /** `yyyy-MM` 或 `yyyy-QN` */
  period_code: string;
  target_amount: DecimalText;
  target_qty: DecimalText;
  status: TargetStatus;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 目标分页筛选 */
export interface TargetFilter {
  /** axios params 需要索引签名 */
  [key: string]: unknown;
  page?: number;
  page_size?: number;
  period_code?: string;
  scope_type?: ScopeType | '';
  status?: TargetStatus | '';
  keyword?: string;
}

/** 新建目标入参 */
export interface TargetCreatePayload {
  scope_type: ScopeType;
  scope_id?: string | null;
  period_code: string;
  target_amount: string;
  target_qty?: string;
  status?: TargetStatus;
  remark?: string | null;
}

/** 编辑目标入参（部分更新；不传的字段不改） */
export interface TargetUpdatePayload {
  scope_type?: ScopeType;
  scope_id?: string | null;
  period_code?: string;
  target_amount?: string;
  target_qty?: string;
  status?: TargetStatus;
  remark?: string | null;
}

/** 目标分页 */
export function fetchTargetPage(params: TargetFilter): Promise<PageData<TargetRow>> {
  return get<PageData<TargetRow>>(`${BASE}/sales-target/page`, params);
}

/** 新建目标 */
export function createTarget(payload: TargetCreatePayload): Promise<TargetRow> {
  return post<TargetRow>(`${BASE}/sales-target`, payload);
}

/** 编辑 / 停用目标 */
export function updateTarget(id: string, payload: TargetUpdatePayload): Promise<TargetRow> {
  return put<TargetRow>(`${BASE}/sales-target/${id}`, payload);
}

/** 软删目标（录错了才用；不再使用请停用） */
export function deleteTarget(id: string): Promise<void> {
  return del<void>(`${BASE}/sales-target/${id}`);
}

// ==================== 达成率看板 ====================

/** 单口径结果 */
export interface AchievementBasisResult {
  actual_amount: DecimalText;
  /** 百分数（`85.50` = 85.5%），可能 > 100 */
  achievement_rate: DecimalText;
  /** 目标 − 实际（负数 = 超额） */
  gap: DecimalText;
}

/** 看板一行（两个口径都在） */
export interface AchievementRow {
  /** 范围显示名（销售员姓名 / 部门名 / 物料名 / 全公司） */
  scope: string;
  scope_type: ScopeType;
  scope_id: string | null;
  target_no: string;
  period_code: string;
  target_amount: DecimalText;
  target_qty: DecimalText;
  /** 按请求 `basis` 选中的口径 */
  actual_amount: DecimalText;
  achievement_rate: DecimalText;
  gap: DecimalText;
  by_order: AchievementBasisResult;
  by_receipt: AchievementBasisResult;
}

/** 看板筛选 */
export interface AchievementQuery {
  /** axios params 需要索引签名 */
  [key: string]: unknown;
  /** **必填**：`yyyy-MM` 或 `yyyy-QN` */
  period_code: string;
  /** 缺省 `ALL`（后端口径） */
  scope_type?: ScopeType;
  /** 缺省 `ORDER` */
  basis?: AchievementBasisKey;
}

/** 达成率看板（一次给两个口径） */
export function fetchAchievement(params: AchievementQuery): Promise<AchievementRow[]> {
  return get<AchievementRow[]>(`${BASE}/sales-target/achievement`, params);
}

// ==================== 销售预测 ====================

/** 预测一行 */
export interface ForecastRow {
  id: string;
  /** `SFC-yyyyMMdd-000001` */
  forecast_no: string;
  customer_id: string | null;
  customer_name: string | null;
  material_id: string | null;
  material_name: string | null;
  period_code: string;
  forecast_amount: DecimalText;
  confidence: Confidence;
  version_no: number;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 预测分页筛选 */
export interface ForecastFilter {
  /** axios params 需要索引签名 */
  [key: string]: unknown;
  page?: number;
  page_size?: number;
  period_code?: string;
  customer_id?: string;
  material_id?: string;
  confidence?: Confidence | '';
}

/** 新建预测入参（客户与物料都空 = 全局滚动预测） */
export interface ForecastCreatePayload {
  customer_id?: string | null;
  material_id?: string | null;
  period_code: string;
  forecast_amount: string;
  confidence?: Confidence;
  version_no?: number;
  remark?: string | null;
}

/** 编辑预测入参 */
export interface ForecastUpdatePayload {
  customer_id?: string | null;
  material_id?: string | null;
  period_code?: string;
  forecast_amount?: string;
  confidence?: Confidence;
  version_no?: number;
  remark?: string | null;
}

/** 预测分页 */
export function fetchForecastPage(params: ForecastFilter): Promise<PageData<ForecastRow>> {
  return get<PageData<ForecastRow>>(`${BASE}/sales-forecast/page`, params);
}

/** 新建预测 */
export function createForecast(payload: ForecastCreatePayload): Promise<ForecastRow> {
  return post<ForecastRow>(`${BASE}/sales-forecast`, payload);
}

/** 编辑预测 */
export function updateForecast(id: string, payload: ForecastUpdatePayload): Promise<ForecastRow> {
  return put<ForecastRow>(`${BASE}/sales-forecast/${id}`, payload);
}

/** 软删预测 */
export function deleteForecast(id: string): Promise<void> {
  return del<void>(`${BASE}/sales-forecast/${id}`);
}

// ==================== 展示辅助（纯函数，页面直接用） ====================

/** 金额 → `12,000.00`（字符串 / 数字都收；非法值原样回显，不吞错） */
export function formatMoney(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 达成率 → `85.50%` */
export function formatRate(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return `${num.toFixed(2)}%`;
}

/** 进度条百分比：夹到 [0, 100]（达成率可能 > 100，进度条不能溢出） */
export function progressPercent(value: DecimalText | null | undefined): number {
  const num = typeof value === 'number' ? value : Number(value ?? 0);
  if (!Number.isFinite(num) || num <= 0) {
    return 0;
  }
  return Math.min(100, Math.round(num * 100) / 100);
}
