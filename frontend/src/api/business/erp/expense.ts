/**
 * 费用报销 API 层（后端 `backend/src/business/erp/expense_{dto,handler,model,compute}.rs`，
 * ARCH-6 §4.10）。
 *
 * # 为什么单独成文件
 *
 * 与 `asset.ts` / `stock-take.ts` 同款理由：`erp/index.ts` 已经是几千行的仓储域总契约，
 * 而报销是独立后端模块（自己的 handler / DTO / 迁移 V0242–V0244）。按模块边界切开，
 * 页面只 `import '@/api/business/erp/expense'`，不会和正在改 `index.ts` 的人互相踩。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `report_no`（`BX-yyyyMMdd-000001`） | `expense_dto.rs::ReportRes` |
 * | 三件套 | `total_amount` / `total_tax` / `total_with_tax` | 同上（Σ 明细，后端算好） |
 * | 付款进度 | `paid_amount` / `unpaid_amount` / `pay_status` | 同上 |
 * | 计提凭证 | `voucher_id` / `voucher_no` | 同上 |
 * | 状态 | `status` + `status_label`（后端给中文，前端不维护第二份译名） | 同上 |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **金额一律字符串**（numeric 四位小数 `"3180.0000"`）。类型上就声明成 `string`，
 *    不要写成 `number` —— `Number()` 之后做等值比较会在 `0.1 + 0.2` 这类地方翻车
 *    （页面上的比较全部走 `expense-logic.ts` 的整数分工具）；
 * 3. **详情是包了一层的**：`ReportDetailRes { report, items, payments, budget_usages, tier_label }`。
 *    单头在 `detail.report` 里，不要照抄盘点单那种 `#[serde(flatten)]` 扁平结构；
 * 4. **明细只录含税**（`with_tax_amount`）：价税由后端 `expense_compute::split` 拆，
 *    前端 `splitPreview` 只是**预览**，入账金额永远以接口返回的 `items[].amount/tax_amount` 为准。
 *
 * # 校验口径（镜像后端的 400 文案，页面上先拦一道）
 *
 * - 建单：事由必填（≤200）、明细 ≥1 条、每条含税金额 > 0、税率 0~1（6% 填 `0.06`）、
 *   行摘要必填、发票类型 ∈ `VAT_SPECIAL / VAT_NORMAL / E_INVOICE / NONE`；
 * - 作废：原因 ≥ 10 字（后端 <10 直接 400）；
 * - 付款：金额 > 0 且 ≤ 未付金额；付款方式 ∈ `BANK_TRANSFER / CASH / OFFSET`。
 */

import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

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

// ==================== 词表（与后端常量逐字一致） ====================
//
// 词表本体在 `expense-kinds.ts`：那里不 import 任何请求层，node 单测可以直接引用
// （`request.ts` 顶层读 `import.meta.env`，在 tsx 下会崩）。这里原样再导出，
// 页面仍然只认 `@/api/business/erp/expense` 一个入口。
export * from './expense-kinds';

// 局部也要用到这两个联合类型（`ExpenseReport` 的字段声明），单独 type import
import type { ExpensePayStatus, ExpenseStatus } from './expense-kinds';

// ==================== ① 费用类型 ====================

/** 费用类型（逐字抄 `TypeRes`）。 */
export interface ExpenseType {
  /** 主键 */
  id: string;
  /** 编码（大写字母开头，如 `TRAVEL`） */
  type_code: string;
  /** 名称 */
  type_name: string;
  /** 借方费用科目 id（末级） */
  account_id: string;
  /** 借方科目编码 */
  account_code: string;
  /** 借方科目名称 */
  account_name: string;
  /** 进项税是否可抵扣（false = 招待费 / 福利费这类，税额恒 0） */
  tax_deductible: boolean;
  /** 是否受预算控制 */
  budget_controlled: boolean;
  /** 是否必填发票号 */
  need_invoice_no: boolean;
  /** 单笔金额上限（`"0.0000"` = 不限） */
  amount_limit: string;
  /** 排序 */
  sort_order: number;
  /** 状态：1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
}

/** 费用类型分页筛选（`TypeFilter`）。 */
export interface ExpenseTypeQuery {
  /** 关键字（编码 / 名称） */
  keyword?: string;
  /** 状态：1 启用 / 0 停用 */
  status?: number | '';
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

/** 新建 / 编辑费用类型入参（`TypeReq`，字段全 `Option`）。 */
export interface ExpenseTypePayload {
  /** 编码 */
  type_code: string;
  /** 名称 */
  type_name: string;
  /** 借方科目 id */
  account_id: string;
  /** 可抵扣 */
  tax_deductible: boolean;
  /** 受预算控制 */
  budget_controlled: boolean;
  /** 必填发票号 */
  need_invoice_no: boolean;
  /** 单笔上限（0 = 不限） */
  amount_limit: string;
  /** 排序 */
  sort_order: number;
  /** 状态 */
  status: number;
  /** 备注 */
  remark?: string;
}

// ==================== ② 报销单与明细 ====================

/** 一条明细（`ItemRes`）。 */
export interface ExpenseItem {
  /** 主键 */
  id: string;
  /** 行号 */
  line_no: number;
  /** 费用发生日 */
  expense_date: string;
  /** 费用类型 id */
  expense_type_id: string;
  /** 类型编码快照 */
  type_code: string;
  /** 类型名快照 */
  type_name: string;
  /** 借方科目 id 快照 */
  account_id: string;
  /** 借方科目编码快照 */
  account_code: string;
  /** 借方科目名快照 */
  account_name: string;
  /** 不含税金额 */
  amount: string;
  /** 税率（`"0.0600"`） */
  tax_rate: string;
  /** 税额 */
  tax_amount: string;
  /** 价税合计（= 用户录入的含税金额） */
  with_tax_amount: string;
  /** 发票号 */
  invoice_no: string | null;
  /** 开票日期 */
  invoice_date: string | null;
  /** 发票类型 */
  invoice_type: string | null;
  /** 行摘要 */
  summary: string;
  /** 行备注 */
  remark: string | null;
}

/** 付款记录（`PaymentRes`）。 */
export interface ExpensePayment {
  /** 主键 */
  id: string;
  /** 付款单号 */
  payment_no: string;
  /** 金额 */
  amount: string;
  /** 付款日期 */
  pay_date: string;
  /** 付款方式 */
  pay_method: string | null;
  /** 付款账户科目编码 */
  pay_account_code: string | null;
  /** 凭证 id */
  voucher_id: string | null;
  /** 凭证号 */
  voucher_no: string | null;
  /** 会计期间 */
  period_code: string;
  /** 状态 */
  status: string;
  /** 备注 */
  remark: string | null;
}

/** 预算占用（`UsageRes`，详情页「预算占用」区块，§4.10 细节 4）。 */
export interface ExpenseBudgetUsage {
  /** 占用 id */
  id: string;
  /** 部门 id */
  dept_id: string;
  /** 科目 id */
  account_id: string;
  /** 科目编码 */
  account_code: string;
  /** 会计年度 */
  fiscal_year: number;
  /** 期间（`yyyy-MM`） */
  period_code: string;
  /** 占用金额（含税） */
  amount: string;
  /** 状态 */
  status: string;
  /** 超预算标记 */
  over_budget: boolean;
  /** 超支额 */
  over_amount: string;
  /** 释放原因 */
  release_reason: string | null;
}

/** 报销单列表行 / 卡片（`ReportRes`）。 */
export interface ExpenseReport {
  /** 主键 */
  id: string;
  /** 单号 */
  report_no: string;
  /** 报销人 id */
  reporter_id: string;
  /** 报销人姓名 */
  reporter_name: string;
  /** 部门 id */
  dept_id: string;
  /** 部门名 */
  dept_name: string;
  /** 事由 */
  title: string;
  /** 费用发生日 */
  expense_date: string;
  /** 会计期间 */
  period_code: string;
  /** 不含税合计 */
  total_amount: string;
  /** 税额合计 */
  total_tax: string;
  /** 价税合计 */
  total_with_tax: string;
  /** 明细条数 */
  item_count: number;
  /** 状态 */
  status: ExpenseStatus | string;
  /** 状态中文 */
  status_label: string;
  /** 付款状态 */
  pay_status: ExpensePayStatus | string;
  /** 付款状态中文 */
  pay_status_label: string;
  /** 已付金额 */
  paid_amount: string;
  /** 未付金额 */
  unpaid_amount: string;
  /** 最近付款日 */
  last_pay_date: string | null;
  /** 超预算标记 */
  over_budget: boolean;
  /** 提交时点的超支额 */
  over_amount: string;
  /**
   * 提交时**实际生效的槽级控制策略**快照（`OFF` / `WARN` / `BLOCK`；`null` = 从未提交过）。
   *
   * FIX-2：槽级 BLOCK、全局 WARN 时这里记的是 `BLOCK` —— 审计要能看出这单被哪条策略管过。
   */
  budget_mode: string | null;
  /** 审批实例 id */
  approval_instance_id: string | null;
  /** 计提凭证 id */
  voucher_id: string | null;
  /** 计提凭证号 */
  voucher_no: string | null;
  /** 提交时间 */
  submitted_at: string | null;
  /** 审批通过时间 */
  approved_at: string | null;
  /** 作废原因 */
  cancel_reason: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
}

/** 报销单详情（`ReportDetailRes`：**包了一层**，不是扁平）。 */
export interface ExpenseDetail {
  /** 单头 */
  report: ExpenseReport;
  /** 明细 */
  items: ExpenseItem[];
  /** 付款记录 */
  payments: ExpensePayment[];
  /** 预算占用 */
  budget_usages: ExpenseBudgetUsage[];
  /** 预算尚未生效的提示（FIX-2：DRAFT / 审批中 / 未到生效日；**只提示不阻断**） */
  budget_warnings: ExpenseBudgetWarning[];
  /** 金额分级文案（「≤1000 元一级审批」这类） */
  tier_label: string;
}

/** 预算未生效提示（逐字抄 `budget_dto::BudgetWarningRes`）。 */
export interface ExpenseBudgetWarning {
  /** 提示码（固定 `BUDGET_NOT_EFFECTIVE`） */
  code: string;
  /** 预算科目 id */
  account_id: string;
  /** 预算科目编码 */
  account_code: string;
  /** 预算单号 */
  budget_no: string;
  /** DRAFT / SUBMITTED / ACTIVE */
  status: string;
  /** 状态中文 */
  status_label: string;
  /** 一句话说明（直接展示） */
  message: string;
}

/** 报销单筛选（`ReportFilter`）。 */
export interface ExpenseReportQuery {
  /** 关键字（单号 / 事由 / 报销人 / 部门） */
  keyword?: string;
  /** 状态 */
  status?: string;
  /** 付款状态 */
  pay_status?: string;
  /** 报销人（「我的报销」传自己） */
  reporter_id?: string;
  /** 部门 */
  dept_id?: string;
  /** 会计期间 `yyyy-MM` */
  period_code?: string;
  /** 费用发生日起 */
  date_from?: string;
  /** 费用发生日止 */
  date_to?: string;
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

/** 一条明细的入参（`ItemReq`，**含税录入**）。 */
export interface ExpenseItemPayload {
  /** 费用发生日（缺省取单头） */
  expense_date?: string;
  /** 费用类型 id */
  expense_type_id: string;
  /** **含税金额** */
  with_tax_amount: string;
  /** 税率（0~1，6% 填 `0.06`） */
  tax_rate: string;
  /** 发票号 */
  invoice_no?: string;
  /** 开票日期 */
  invoice_date?: string;
  /** 发票类型 */
  invoice_type?: string;
  /** 行摘要（必填） */
  summary: string;
  /** 行备注 */
  remark?: string;
}

/** 新建 / 编辑报销单入参（`ReportReq`）。 */
export interface ExpenseReportPayload {
  /** 事由 */
  title: string;
  /** 费用发生日 */
  expense_date?: string;
  /** 报销人（缺省 = 当前用户） */
  reporter_id?: string;
  /** 部门 */
  dept_id?: string;
  /** 备注 */
  remark?: string;
  /** 明细（≥1） */
  items: ExpenseItemPayload[];
}

/** 付款登记入参（`PayReq`）。 */
export interface ExpensePayPayload {
  /** 本次付款金额 */
  amount: string;
  /** 付款日期（缺省今天） */
  pay_date?: string;
  /** 付款方式 */
  pay_method?: string;
  /** 付款账户科目编码（缺省 1002） */
  pay_account_code?: string;
  /** 备注 */
  remark?: string;
}

// ==================== ③ 接口 ====================

/** 报销单分页（`GET /api/business/erp/expense/page`）。 */
export function fetchExpensePage(query: ExpenseReportQuery = {}): Promise<PageData<ExpenseReport>> {
  return get<PageData<ExpenseReport>>(`${BASE}/expense/page`, asParams(query));
}

/**
 * 待付款分页（`GET /api/business/erp/expense/pay/page`）。
 *
 * 后端只认 `page` / `page_size`（`report_payable_page` 的口径是「已审批且未付清」），
 * 其余筛选字段传了也会被忽略 —— 页面上不要摆一排不起作用的筛选框。
 */
export function fetchExpensePayPage(page: number, pageSize: number): Promise<PageData<ExpenseReport>> {
  return get<PageData<ExpenseReport>>(`${BASE}/expense/pay/page`, {
    page,
    page_size: pageSize,
  });
}

/** 报销单详情（`GET /api/business/erp/expense/{id}`）。 */
export function fetchExpenseDetail(id: string): Promise<ExpenseDetail> {
  return get<ExpenseDetail>(`${BASE}/expense/${id}`);
}

/** 新建报销单（返回详情，前端不用再拉一次）。 */
export function createExpense(payload: ExpenseReportPayload): Promise<ExpenseDetail> {
  return post<ExpenseDetail>(`${BASE}/expense`, payload);
}

/** 编辑草稿（返回详情）。 */
export function updateExpense(id: string, payload: ExpenseReportPayload): Promise<ExpenseDetail> {
  return put<ExpenseDetail>(`${BASE}/expense/${id}`, payload);
}

/** 删除草稿（非草稿 → 409）。 */
export function deleteExpense(id: string): Promise<{ id: string }> {
  return del<{ id: string }>(`${BASE}/expense/${id}`);
}

/** 提交审批（返回详情）。 */
export function submitExpense(id: string): Promise<ExpenseDetail> {
  return post<ExpenseDetail>(`${BASE}/expense/${id}/submit`);
}

/** 撤回（`SUBMITTED → DRAFT`，返回详情）。 */
export function withdrawExpense(id: string): Promise<ExpenseDetail> {
  return post<ExpenseDetail>(`${BASE}/expense/${id}/withdraw`);
}

/** 作废（`reason` ≥ 10 字，返回详情）。 */
export function cancelExpense(id: string, reason: string): Promise<ExpenseDetail> {
  return post<ExpenseDetail>(`${BASE}/expense/${id}/cancel`, { reason });
}

/** 付款登记（返回详情）。 */
export function payExpense(id: string, payload: ExpensePayPayload): Promise<ExpenseDetail> {
  return post<ExpenseDetail>(`${BASE}/expense/${id}/pay`, payload);
}

/** 费用类型分页（`GET /api/business/erp/expense-type/page`）。 */
export function fetchExpenseTypePage(
  query: ExpenseTypeQuery = {},
): Promise<PageData<ExpenseType>> {
  return get<PageData<ExpenseType>>(`${BASE}/expense-type/page`, asParams(query));
}

/** 启用中的费用类型全量（「新建报销单」下拉用，`GET .../expense-type/list`）。 */
export function fetchExpenseTypeList(): Promise<ExpenseType[]> {
  return get<ExpenseType[]>(`${BASE}/expense-type/list`);
}

/** 新建费用类型。 */
export function createExpenseType(payload: ExpenseTypePayload): Promise<ExpenseType> {
  return post<ExpenseType>(`${BASE}/expense-type`, payload);
}

/** 编辑费用类型。 */
export function updateExpenseType(id: string, payload: ExpenseTypePayload): Promise<ExpenseType> {
  return put<ExpenseType>(`${BASE}/expense-type/${id}`, payload);
}

/** 删除费用类型（被报销单引用 → 409，只能停用）。 */
export function deleteExpenseType(id: string): Promise<{ id: string }> {
  return del<{ id: string }>(`${BASE}/expense-type/${id}`);
}
