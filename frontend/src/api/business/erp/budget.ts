/**
 * 预算 API 层（FIN-1 后端 `backend/src/business/erp/budget_{dto,handler,model,guard}.rs`）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `stock-out.ts` / `stock-take.ts` 同一个理由：`index.ts` 已是 3000+ 行的仓储域总契约，
 * 而预算是独立后端模块（自己的 handler / DTO / 迁移 `V0227__erp_budget_tables.up.sql`）。
 * 按模块边界单独成文件，页面只 `import '@/api/business/erp/budget'`，不会和别人正在改的
 * `index.ts` 互相踩。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `budget_no` | `budget_dto.rs::BudgetRes::budget_no` |
 * | 状态 | `status` + `status_label` | 后端给中文名，前端不维护第二份译名 |
 * | 年度额度 | `total_amount`（**字符串**，`Decimal` 出 JSON 就是字符串） | 同上 |
 * | 版本号 | `version_no`（i32） | 同上 |
 * | 编制/调整 | `change_type` ∈ `ORIGINAL` / `ADJUST` | 同上 |
 * | 月度分解 | `periods[]` 的 `period_code` / `period_month` / `amount` / `used` / `available` | `PeriodRes` |
 * | 占用记录 | `usages[]` 的 `source_no` / `over_budget` / `over_amount` | `UsageRes` |
 * | 执行报表 | `occupied` / `consumed` / `used` / `available` / `exec_ratio` / `over` | `ReportRes` |
 * | 映射 | `map_type` / `ref_id` / `ref_code` / `account_*` / `dept_id` / `priority` | `MappingRes` |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML ——
 *    AGENTS.md 第 9 条）；前缀统一 `const BASE = '/api/business/erp/budget'`；
 * 2. **详情是「包一层」的**：`GET /budget/{id}` 返回 `BudgetDetailRes`
 *    = `{ budget, periods, usages, year_used, year_available, versions, next_statuses }`，
 *    **不是**把 `BudgetRes` 摊进顶层（与出库单 / 盘点单的 `#[serde(flatten)]` 不同）；
 * 3. **`GET /budget/report` 返回数组、不分页**（`Vec<ReportRes>`），**`GET /budget/mapping/page`
 *    才分页**；两条都是静态段，别和 `/budget/{id}` 混成一条；
 * 4. **映射列表的类型筛选走 `status` 这个参数名**：`budget_model::map_page` 从
 *    `q.status` 里读 `map_type`（`if let Some(map_type) = q.status.as_deref()`，
 *    见 `budget_model.rs` 的 `map_page`）。这是后端复用 `BudgetFilter` 的既成事实，
 *    前端不改名也不许「顺手纠正」——改了后端就不筛了。所以本文件的
 *    `fetchBudgetMappingPage` 把 `map_type` 翻译成 `status` 发出去。
 *
 * # 金额口径
 *
 * 后端一律 `Decimal`，JSON 里是字符串（`"360000.0000"`）。本层**不**转 number，
 * 保持字符串原样透传；比较 / 累加走 `@/views/business/erp/shared/money` 的整数分。
 */

import { del, get, post, put, type PageData } from '@/api/request';
import { fetchAccountPage, type AccountRow } from '@/api/business/erp/index';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp/budget';

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

// ==================== 枚举（逐字抄后端，一个大写都不改） ====================

/**
 * 预算状态（与 `budget_dto::STATUSES` / `ck_biz_budget_status` 逐字一致）。
 *
 * ```text
 * DRAFT ──提交审批──→ SUBMITTED ──审批通过──→ ACTIVE ──被新版本取代──→ SUPERSEDED
 *   │                    │                     └──年结批量关──→ CLOSED
 *   └──作废──→ CANCELLED  └──作废──→ CANCELLED
 * ```
 *
 * ⚠️ **没有 `ACTIVE → DRAFT`**：已生效的预算不能退回草稿改，只能再调一版（ARCH-6 硬规则 4）。
 * 所以编制页对 `ACTIVE` 行给的是「调整」而不是「编辑」。
 */
export type BudgetStatus = 'DRAFT' | 'SUBMITTED' | 'ACTIVE' | 'SUPERSEDED' | 'CLOSED' | 'CANCELLED';

/** 编制 / 调整（与 `budget_model::CHANGE_ORIGINAL / CHANGE_ADJUST` 一致） */
export type BudgetChangeType = 'ORIGINAL' | 'ADJUST';

/** 控制策略（与 `budget_guard::ControlMode` 的 `as_str()` 一致；`null` = 继承全局） */
export type BudgetControlMode = 'OFF' | 'WARN' | 'BLOCK';

/** 映射类型（与 `budget_dto::validate_mapping` 的白名单逐字一致） */
export type BudgetMapType = 'MATERIAL' | 'MATERIAL_TYPE' | 'WAREHOUSE' | 'DEFAULT';

// ==================== 出参 ====================

/** 列表行 / 版本行（逐字抄 `budget_dto::BudgetRes`） */
export interface BudgetRow {
  /** 主键 */
  id: string;
  /** 单号 `BDG-2026-000012` */
  budget_no: string;
  /** 年度 */
  fiscal_year: number;
  /** 部门 id */
  dept_id: string;
  /** 部门名（建单时快照，部门改名不影响历史单） */
  dept_name: string;
  /** 预算科目 id（必须末级） */
  account_id: string;
  /** 科目编码（快照） */
  account_code: string;
  /** 科目名（快照） */
  account_name: string;
  /** 年度额度（Decimal 字符串） */
  total_amount: string;
  /** 槽级控制策略；`null` = 继承 `sys_config erp.budget.control_mode` */
  control_mode: BudgetControlMode | null;
  /** 版本号（1 起；>1 只可能由「调整」产生） */
  version_no: number;
  /** ORIGINAL / ADJUST */
  change_type: BudgetChangeType;
  /** 状态 */
  status: BudgetStatus;
  /** 状态中文（后端给） */
  status_label: string;
  /** 审批实例 id（提交后才有） */
  approval_instance_id: string | null;
  /** 提交时间 */
  submitted_at: string | null;
  /** 通过时间 */
  approved_at: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
}

/** 月度分解行（逐字抄 `budget_dto::PeriodRes`） */
export interface BudgetPeriodLine {
  /** 期间 `yyyy-MM` */
  period_code: string;
  /** 月份 1..12 */
  period_month: number;
  /** 月度额度 */
  amount: string;
  /** 已用（现算：OCCUPIED + CONSUMED） */
  used: string;
  /** 可用（额度 − 已用；负数 = 该月超支） */
  available: string;
  /** 备注 */
  remark: string | null;
}

/** 占用 / 执行记录（逐字抄 `budget_dto::UsageRes`） */
export interface BudgetUsageRow {
  /** 主键 */
  id: string;
  /** 期间 */
  period_code: string;
  /** 金额 */
  amount: string;
  /** OCCUPIED / CONSUMED / RELEASED */
  status: string;
  /** 状态中文 */
  status_label: string;
  /** 来源类型（本期只有 `PURCHASE`） */
  source_type: string;
  /** 来源单号 */
  source_no: string;
  /** 提交时是否超预算（**提交那一刻的快照**，额度后来调高也不会变） */
  over_budget: boolean;
  /** 提交时超支额 */
  over_amount: string;
  /** 释放原因 */
  release_reason: string | null;
}

/** 详情（逐字抄 `budget_dto::BudgetDetailRes` —— 注意是**包一层**的） */
export interface BudgetDetail {
  /** 主表 */
  budget: BudgetRow;
  /** 12 个月分解（含当月已用） */
  periods: BudgetPeriodLine[];
  /** 占用 / 执行记录 */
  usages: BudgetUsageRow[];
  /** 年度已用 */
  year_used: string;
  /** 年度可用（可为负） */
  year_available: string;
  /** 同槽版本链（含自己） */
  versions: BudgetRow[];
  /** 下一步可流转状态（状态机给的，前端不硬编码） */
  next_statuses: string[];
}

/** 报表分月明细（逐字抄 `budget_dto::MonthRes`） */
export interface BudgetReportMonth {
  /** 期间 */
  period_code: string;
  /** 额度 */
  amount: string;
  /** 已用 */
  used: string;
  /** 可用 */
  available: string;
}

/** 执行报表行（逐字抄 `budget_dto::ReportRes`） */
export interface BudgetReportRow {
  /** 预算版本 id */
  budget_id: string;
  /** 年度 */
  fiscal_year: number;
  /** 部门 id */
  dept_id: string;
  /** 部门名 */
  dept_name: string;
  /** 科目 id */
  account_id: string;
  /** 科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string;
  /** 年度额度 */
  total_amount: string;
  /** 占用中（已提交未执行） */
  occupied: string;
  /** 已执行 */
  consumed: string;
  /** 已用合计 = occupied + consumed */
  used: string;
  /** 可用（**负数 = 超支**） */
  available: string;
  /** 执行率（百分比两位小数的 Decimal 字符串，如 `"85.25"`） */
  exec_ratio: string;
  /** 是否超支（后端算好，前端不重算） */
  over: boolean;
  /** 分月明细 */
  months: BudgetReportMonth[];
}

/** 映射行（逐字抄 `budget_dto::MappingRes`） */
export interface BudgetMappingRow {
  /** 主键 */
  id: string;
  /** MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT */
  map_type: BudgetMapType;
  /** 目标 id（DEFAULT 为空） */
  ref_id: string | null;
  /** 目标编码快照 */
  ref_code: string | null;
  /** 预算科目 id */
  account_id: string;
  /** 科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string;
  /** 部门覆盖（`null` = 全公司） */
  dept_id: string | null;
  /** 优先级（同分比类型精细度） */
  priority: number;
  /** 1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
}

// ==================== 入参 ====================

/** 分页 / 报表筛选（逐字抄 `budget_dto::BudgetFilter`） */
export interface BudgetFilterQuery {
  /** 页码 */
  page?: number;
  /** 每页条数（后端上限 100） */
  page_size?: number;
  /** 年度 */
  fiscal_year?: number;
  /** 部门 id */
  dept_id?: string;
  /** 科目 id */
  account_id?: string;
  /** 状态（列表用） */
  status?: string;
  /** ORIGINAL / ADJUST */
  change_type?: string;
  /** 单号 / 科目 / 部门模糊 */
  keyword?: string;
}

/** 一条月度分解（入参；`month` 与 `period_code` 二选一，都默认给出 `month`） */
export interface BudgetPeriodReq {
  /** 期间 `yyyy-MM`（不给则按 `month` + 年度拼） */
  period_code?: string;
  /** 月份 1..12 */
  month?: number;
  /** 额度（字符串定点；后端 `Decimal` 也吃 JSON 数字） */
  amount?: string | number;
  /** 备注 */
  remark?: string;
}

/** 新建（逐字抄 `budget_dto::CreateBudgetReq`） */
export interface CreateBudgetReq {
  /** 年度 */
  fiscal_year: number;
  /** 部门 id */
  dept_id: string;
  /** 预算科目 id（必须末级） */
  account_id: string;
  /** 年度额度 */
  total_amount: string | number;
  /** 槽级控制策略（`null` = 继承全局） */
  control_mode?: string | null;
  /** 12 个月分解（空数组 / 不传 = 后端平均分摊） */
  periods?: BudgetPeriodReq[];
  /** 备注 */
  remark?: string | null;
}

/** 改草稿（逐字抄 `budget_dto::UpdateBudgetReq` —— 部门与科目不可改） */
export interface UpdateBudgetReq {
  /** 年度额度 */
  total_amount: string | number;
  /** 槽级控制策略 */
  control_mode?: string | null;
  /** 12 个月分解 */
  periods?: BudgetPeriodReq[];
  /** 备注 */
  remark?: string | null;
}

/** 调整（逐字抄 `budget_dto::AdjustBudgetReq` —— 原因必填） */
export interface AdjustBudgetReq {
  /** 新的年度额度 */
  total_amount: string | number;
  /** 槽级控制策略 */
  control_mode?: string | null;
  /** 12 个月分解 */
  periods?: BudgetPeriodReq[];
  /** **调整原因（必填）** */
  change_reason: string;
  /** 备注 */
  remark?: string | null;
}

/** 映射入参（逐字抄 `budget_dto::MappingReq`） */
export interface BudgetMappingReq {
  /** 映射类型 */
  map_type: BudgetMapType;
  /** 目标 id（DEFAULT 必须为空；其余类型后端校验必给） */
  ref_id?: string | null;
  /** 目标编码（MATERIAL_TYPE 用物料类型编码，解析只认它） */
  ref_code?: string | null;
  /** 预算科目 id */
  account_id: string;
  /** 部门覆盖（空 = 全公司） */
  dept_id?: string | null;
  /** 优先级（缺省 0） */
  priority?: number;
  /** 状态（0/1，缺省 1） */
  status?: number;
  /** 备注 */
  remark?: string | null;
}

// ==================== 接口 ====================

/** 预算分页（`GET /business/erp/budget/page`，权限 `budget:list`） */
export function fetchBudgetPage(query: BudgetFilterQuery): Promise<PageData<BudgetRow>> {
  return get<PageData<BudgetRow>>(`${BASE}/page`, asParams(query));
}

/**
 * 预算详情（`GET /business/erp/budget/{id}`，权限 `budget:list`）。
 *
 * 返回 12 个月分解 + 当月已用 + 占用记录 + 同槽版本链 + 状态机给的 `next_statuses`。
 */
export function fetchBudgetDetail(id: string): Promise<BudgetDetail> {
  return get<BudgetDetail>(`${BASE}/${id}`);
}

/** 新建预算（`POST /business/erp/budget`，权限 `budget:create`；返回详情） */
export function createBudget(payload: CreateBudgetReq): Promise<BudgetDetail> {
  return post<BudgetDetail>(BASE, payload);
}

/** 改草稿（`PUT /business/erp/budget/{id}`，权限 `budget:update`；仅 `DRAFT`） */
export function updateBudget(id: string, payload: UpdateBudgetReq): Promise<BudgetDetail> {
  return put<BudgetDetail>(`${BASE}/${id}`, payload);
}

/** 删草稿（`DELETE /business/erp/budget/{id}`，权限 `budget:delete`；软删） */
export function deleteBudget(id: string): Promise<void> {
  return del<void>(`${BASE}/${id}`);
}

/** 提交审批（`POST /business/erp/budget/{id}/submit`，权限 `budget:submit`） */
export function submitBudget(id: string): Promise<BudgetDetail> {
  return post<BudgetDetail>(`${BASE}/${id}/submit`);
}

/**
 * 调整 = 新增版本（`POST /business/erp/budget/{id}/adjust`，权限 `budget:adjust`）。
 *
 * 后端内部做两件事：建 `version_no + 1` 的草稿 → **立即送审**，所以返回的是**新版本**的详情。
 */
export function adjustBudget(id: string, payload: AdjustBudgetReq): Promise<BudgetDetail> {
  return post<BudgetDetail>(`${BASE}/${id}/adjust`, payload);
}

/** 执行报表（`GET /business/erp/budget/report`，权限 `budget:report`；**不分页**） */
export function fetchBudgetReport(query: BudgetFilterQuery): Promise<BudgetReportRow[]> {
  return get<BudgetReportRow[]>(`${BASE}/report`, asParams(query));
}

/**
 * 映射分页（`GET /business/erp/budget/mapping/page`，权限 `budget:mapping`）。
 *
 * ⚠️ 后端的类型筛选参数名是 **`status`**（`budget_model::map_page` 从 `q.status` 读
 * `map_type`）。本函数把语义化的 `map_type` 翻译过去，页面不必知道这个历史包袱。
 */
export function fetchBudgetMappingPage(query: {
  map_type?: string;
  dept_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<BudgetMappingRow>> {
  const { map_type, ...rest } = query;
  return get<PageData<BudgetMappingRow>>(
    `${BASE}/mapping/page`,
    asParams({ ...rest, status: map_type }),
  );
}

/** 新建映射（`POST /business/erp/budget/mapping`，权限 `budget:mapping`） */
export function createBudgetMapping(payload: BudgetMappingReq): Promise<{ id: string }> {
  return post<{ id: string }>(`${BASE}/mapping`, payload);
}

/** 修改映射（`PUT /business/erp/budget/mapping/{id}`，权限 `budget:mapping`） */
export function updateBudgetMapping(
  id: string,
  payload: BudgetMappingReq,
): Promise<{ id: string; updated: boolean }> {
  return put<{ id: string; updated: boolean }>(`${BASE}/mapping/${id}`, payload);
}

/** 删映射（`DELETE /business/erp/budget/mapping/{id}`，权限 `budget:mapping`；软删） */
export function deleteBudgetMapping(id: string): Promise<{ id: string; deleted: boolean }> {
  return del<{ id: string; deleted: boolean }>(`${BASE}/mapping/${id}`);
}

// ==================== 下拉候选 ====================

/** 预算科目候选（`AccountRow` 原样 + 已过滤末级 / 启用） */
export type BudgetAccountOption = Pick<AccountRow, 'id' | 'account_code' | 'account_name' | 'is_leaf'>;

/**
 * 一次拉够预算可用的科目：**只要末级 + 启用**。
 *
 * 为什么必须 `is_leaf=true`：后端 `account_snapshot` 对非末级科目直接 400
 * （ARCH-6 红线 3 —— 父科目与子科目各占一次额度等于算两遍）。与其让用户选完再吃 400，
 * 不如下拉里就不出现。`status=1` 同理（停用科目后端也 400）。
 *
 * ⚠️ `fetchAccountPage` 的 `is_leaf` 是后端的 `Option<bool>`，`true` 会发
 * `is_leaf=true`（`asParams` 保留布尔值）；`false` 也别写成 `''`。
 */
export async function fetchBudgetAccountOptions(limit = 500): Promise<BudgetAccountOption[]> {
  const data = await fetchAccountPage({ is_leaf: true, status: 1, page: 1, page_size: limit });
  return (data.list ?? []).map((row) => ({
    id: row.id,
    account_code: row.account_code,
    account_name: row.account_name,
    is_leaf: row.is_leaf,
  }));
}
