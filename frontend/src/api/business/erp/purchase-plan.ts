/**
 * 采购计划 API 层（后端 `backend/app/src/business/erp/purchase/purchase_plan*`，M4-02/03/04）。
 *
 * # 接口（8 条：6 写读 + 2 只读执行率）
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 分页 | `GET /api/business/erp/purchase-plan/page` | `business:erp:purchase-plan:list` |
 * | 详情（单头 + 明细） | `GET /api/business/erp/purchase-plan/{id}` | 同上 |
 * | 建计划 | `POST /api/business/erp/purchase-plan` | `…:create` |
 * | 改计划（**整单替换**） | `PUT /api/business/erp/purchase-plan/{id}` | `…:update` |
 * | 删计划 | `DELETE /api/business/erp/purchase-plan/{id}` | `…:delete` |
 * | 提交审批 | `POST /api/business/erp/purchase-plan/{id}/submit` | `…:submit` |
 * | 转采购申请（一仓一单） | `POST /api/business/erp/purchase-plan/{id}/to-requisition` | `…:to-requisition` |
 * | 执行率总览 | `GET /api/business/erp/purchase-plan/execution/page` | `…:execution` |
 * | 执行率详情（含下钻） | `GET /api/business/erp/purchase-plan/{id}/execution` | 同上 |
 *
 * # 六个容易踩的点（全部来自后端契约注释，别在页面里重新猜）
 *
 * 1. **路径必须写全 `/api`**（后端 `#[nik::get]` 不含前缀；漏了会命中 SPA fallback
 *    返回 HTML，报「响应体格式不符合」）；
 * 2. **金额 / 数量是字符串**（后端 `Decimal`）：提交前 `String(value)`，展示用
 *    `decimalText()`；前端**不做**金额算术，`line_amount` 只在输入时给个预览值；
 * 3. **单头三个 `total_*` 是派生列**：请求体里带了一律被忽略，金额由
 *    `Σ(planned_qty × planned_price)` 现算 —— 所以 payload 里干脆不送；
 * 4. **粒度 = 物料 × 仓库**：同一张计划里 `(material_id, warehouse_id)` 重复，
 *    后端在**请求层** 400 并点出「第 3 行与第 1 行」，前端先拦一次（同口径文案）；
 * 5. **月度计划的父单必须是同年度计划**（`parent_plan_id` → `ANNUAL`，同年）；
 * 6. **执行率状态不要自己推**：后端给 `exec_state` / `exec_state_label` / `over_plan`，
 *    而且**允许 `exec_rate_pct = 100.00` 但状态 `PARTIAL`**（率舍到 2 位、状态用未舍入量判）
 *    —— 用 pct 反推状态会在 99.995% 那一格翻错。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

/**
 * 计划状态（与 V0446 的 `ck_biz_pp_status` 逐字一致）
 *
 * ⚠ 本文件是**纯契约 + 取数**层（会被页面在运行时 import 进来，进而拉起 `@/api/request`）。
 * 中文名 / 主题色 / 金额展示这类**运行时展示件**一律放在
 * `views/business/erp/purchase-plan/purchase-plan-logic.ts` —— 那边能被 Node 单测直接加载，
 * 也不会因为 `import.meta.env` 让 `pnpm test:unit` 炸掉。
 */
export type PlanStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED';

/** 计划类型（`ck_biz_pp_type`：年度 / 月度） */
export type PlanType = 'ANNUAL' | 'MONTHLY';

/** 执行率行状态（与 V0449 视图 `exec_state` 的 CASE 逐字一致） */
export type ExecState = 'NOT_STARTED' | 'PARTIAL' | 'DONE' | 'OVER';

/** 采购计划列表行 / 单头（后端 `PlanRowRes`） */
export interface PlanRow {
  /** 主键 */
  id: string;
  /** 计划单号 PPL-yyyyMM-NNNN */
  plan_no: string;
  /** 计划名称 */
  plan_name: string;
  /** 类型 */
  plan_type: PlanType;
  /** 类型中文 */
  plan_type_label: string;
  /** 计划年度 */
  plan_year: number;
  /** 月份（年度计划为 null） */
  plan_month: number | null;
  /** 父年度计划 id */
  parent_plan_id: string | null;
  /** 父年度计划单号（实时查） */
  parent_plan_no: string | null;
  /** 编制部门 id */
  dept_id: string | null;
  /** 编制部门名 */
  dept_name: string | null;
  /** 编制人 id */
  planner_id: string | null;
  /** 编制人姓名（快照） */
  planner_name: string | null;
  /** 状态 */
  status: PlanStatus;
  /** 状态中文 */
  status_label: string;
  /** 审批实例 id */
  approval_instance_id: string | null;
  /** 计划量合计 */
  total_planned_qty: DecimalText;
  /** 已执行量合计（转申请回写列的合计） */
  total_executed_qty: DecimalText;
  /** 计划金额合计（不含税） */
  total_planned_amount: DecimalText;
  /** 备注 */
  remark: string | null;
  /** 明细行数 */
  item_count: number;
  /** 创建时间 */
  created_at: string;
  /** 创建人 */
  create_by: string | null;
  /**
   * 能否编辑 / 删除 / 提交（只有 `DRAFT` 为 true）。
   *
   * 前端**不必自己推状态机**：以它为准，状态机若调整，页面不用改。
   */
  editable: boolean;
}

/** 计划明细行（后端 `PlanItemRes`） */
export interface PlanItem {
  /** 主键 */
  id: string;
  /** 行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库名 */
  warehouse_name: string;
  /** 单位 id */
  unit_id: string;
  /** 单位名 */
  unit_name: string;
  /** 计划量 */
  planned_qty: DecimalText;
  /** 已执行量（转申请回写列） */
  executed_qty: DecimalText;
  /** 预估不含税单价 */
  planned_price: DecimalText;
  /** 金额（服务端算） */
  planned_amount: DecimalText;
  /** 行备注 */
  remark: string | null;
}

/** 详情 = 单头 + 明细 */
export interface PlanDetail {
  /** 单头 */
  plan: PlanRow;
  /** 明细 */
  items: PlanItem[];
}

/** 建单 / 改单明细入参（单位由物料带出，不传） */
export interface PlanItemPayload {
  /** 物料 */
  material_id: string;
  /** 仓库（粒度轴之一） */
  warehouse_id: string;
  /** 计划量（> 0） */
  planned_qty: string;
  /** 预估不含税单价（≥ 0，缺省 0） */
  planned_price?: string;
  /** 行备注 */
  remark?: string | null;
}

/**
 * 建单 / 改单入参（后端 `CreatePlanReq`）。
 *
 * 单头 `total_*` 三个派生列**不在这里**：后端一律忽略请求里的值，只认明细现算。
 */
export interface PlanPayload {
  /** 计划名称（必填，≤128 字） */
  plan_name: string;
  /** 类型 */
  plan_type: PlanType;
  /** 年度（1990–2200） */
  plan_year: number;
  /** 月份（月度必填 1–12；年度**不许**传） */
  plan_month?: number | null;
  /** 父年度计划（月度可挂） */
  parent_plan_id?: string | null;
  /** 编制部门（可空 = 公司级） */
  dept_id?: string | null;
  /** 编制人（缺省 = 当前登录人） */
  planner_id?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 明细（不能为空） */
  items: PlanItemPayload[];
}

/** 分页查询条件（后端 `PlanPageQuery`） */
export interface PlanFilter {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 关键字：单号 / 名称 / 部门名 / 备注 */
  keyword?: string;
  /** 状态 */
  status?: PlanStatus | '';
  /** 类型 */
  plan_type?: PlanType | '';
  /** 年度 */
  plan_year?: number | null;
  /** 编制部门 */
  dept_id?: string | null;
}

/** 提交审批入参（`skip_*` 只在没配已发布流程时用得上） */
export interface SubmitPlanPayload {
  /** 说明（≤200 字） */
  reason?: string | null;
  /** 显式跳过审批（需 `business:erp:skip-approval` + 理由 ≥10 字） */
  skip_approval?: boolean;
  /** 跳过理由 */
  skip_reason?: string | null;
}

/** 转采购申请的单行入参（后端 `ConvertLineReq`） */
export interface ConvertLinePayload {
  /** 计划明细行 id */
  plan_item_id: string;
  /** 本次转出数量（缺省 = 剩余量） */
  qty?: string;
  /** 覆盖预估单价（计划行单价为 0 时**必填**） */
  est_price?: string;
}

/**
 * 转采购申请入参（后端 `ToRequisitionReq`）。
 *
 * `items` 留空 = 把**全部剩余量**转成申请；给了就按给的行转（可分批）。
 */
export interface ToRequisitionPayload {
  /** 申请部门（缺省取计划编制部门；公司级计划必须显式给） */
  dept_id?: string | null;
  /** 申请人（缺省 = 当前登录人） */
  applicant_id?: string | null;
  /** 申请日期（缺省 = 今天） */
  req_date?: string | null;
  /** 需求日期 */
  demand_date?: string | null;
  /** 税率（0–100，缺省 0；计划不记税率，整单统一取它） */
  tax_rate?: string;
  /** 备注（后端会追加「由采购计划 PPL-… 转入」） */
  remark?: string | null;
  /** 只转这些行；留空 = 全部剩余量 */
  items: ConvertLinePayload[];
}

/** 转换出来的申请明细行（后端 `ConvertedLineRes`） */
export interface ConvertedLine {
  /** 来源计划行 id */
  plan_item_id: string;
  /** 来源计划行号 */
  plan_line_no: number;
  /** 物料 */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 本次转出数量 */
  qty: DecimalText;
  /** 预估不含税单价 */
  est_price: DecimalText;
  /** 行金额 */
  amount: DecimalText;
  /** 转换后计划行剩余量 */
  remaining_after: DecimalText;
}

/** 转换出来的一张申请单（后端 `ConvertedRequisitionRes`；**一仓一张**） */
export interface ConvertedRequisition {
  /** 申请单 id */
  requisition_id: string;
  /** 申请单号 PRQ-yyyyMMdd-NNNNNN */
  req_no: string;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 收货仓库名 */
  warehouse_name: string;
  /** 申请部门 */
  dept_id: string;
  /** 申请部门名 */
  dept_name: string | null;
  /** 申请日期 */
  req_date: string;
  /** 预估金额合计（不含税） */
  est_amount: DecimalText;
  /** 明细行数 */
  line_count: number;
  /** 明细 */
  lines: ConvertedLine[];
}

/** 转申请结果（后端 `ToRequisitionRes`） */
export interface ToRequisitionResult {
  /** 计划的最新状态（已执行量已是回写后的值） */
  plan: PlanRow;
  /** 生成的申请单（按仓库首次出现顺序；N 个仓库 N 张） */
  requisitions: ConvertedRequisition[];
  /** 共转出多少行 */
  converted_lines: number;
  /** 共转出多少数量 */
  converted_qty: DecimalText;
  /** 共转出多少金额 */
  converted_amount: DecimalText;
}

// ==================== 执行率（M4-04 · 只读） ====================

/** 执行率总览的筛选（后端 `ExecutionPageQuery`） */
export interface ExecutionFilter {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 关键字：计划单号 / 名称 / 部门名 / 编制人 */
  keyword?: string;
  /** 计划状态 */
  status?: PlanStatus | '';
  /** 计划类型 */
  plan_type?: PlanType | '';
  /** 计划年度 */
  plan_year?: number | null;
  /** 计划月份（1–12） */
  plan_month?: number | null;
  /** 编制部门 */
  dept_id?: string | null;
  /** 执行率下限（**百分比** 0–1000：100 = 只看已下满 / 超计划的） */
  min_rate?: number | null;
}

/** 计划执行率（后端 `PlanExecRowRes`；总览一行 / 详情单头） */
export interface PlanExecRow {
  /** 计划 id（注意：不是 `id` —— 这是执行率视图的出参） */
  plan_id: string;
  /** 计划单号 */
  plan_no: string;
  /** 计划名称 */
  plan_name: string;
  /** 类型 */
  plan_type: PlanType;
  /** 类型中文 */
  plan_type_label: string;
  /** 年度 */
  plan_year: number;
  /** 月份（年度计划为 null） */
  plan_month: number | null;
  /** 编制部门 id */
  dept_id: string | null;
  /** 编制部门名 */
  dept_name: string | null;
  /** 编制人姓名（快照） */
  planner_name: string | null;
  /** 计划状态 */
  status: PlanStatus;
  /** 计划状态中文 */
  status_label: string;
  /** 计划金额合计（不含税） */
  total_planned_amount: DecimalText;
  /** 明细行数 */
  line_count: number;
  /** 计划量合计 */
  planned_qty: DecimalText;
  /** 回写列快照：已申请量 */
  booked_qty: DecimalText;
  /** 已申请量（按单据现算） */
  req_qty: DecimalText;
  /** 已下单量合计 */
  ordered_qty: DecimalText;
  /** 剩余可下单合计（**可为负**：超计划） */
  remaining_qty: DecimalText;
  /** 还能再转成采购申请的量（Σ计划量 − Σ回写已申请量，负数按 0） */
  convertible_qty: DecimalText;
  /** 整单执行率（0–1，6 位；计划量 0 时 null） */
  exec_rate: DecimalText | null;
  /** 整单执行率百分比（0–100，2 位） */
  exec_rate_pct: DecimalText | null;
  /** 整单已申请率（0–1，6 位） */
  req_rate: DecimalText | null;
  /** 整单已申请率百分比（0–100，2 位） */
  req_rate_pct: DecimalText | null;
  /** 整单状态 */
  exec_state: ExecState;
  /** 整单状态中文 */
  exec_state_label: string;
  /** 未开工行数 */
  not_started_lines: number;
  /** 部分下单行数 */
  partial_lines: number;
  /** 已下满行数 */
  done_lines: number;
  /** 超计划行数 */
  over_lines: number;
  /** 创建时间 */
  created_at: string;
}

/** 来源单据（下钻：这一格计划量被哪些申请单 / 采购单消耗了） */
export interface ExecSource {
  /** 申请明细 id */
  req_item_id: string;
  /** 申请单 id */
  req_id: string;
  /** 申请单号 */
  req_no: string;
  /** 申请单状态 */
  req_status: string;
  /** 申请单状态中文 */
  req_status_label: string;
  /** 申请数量（这一行计划量拨给它的部分） */
  qty: DecimalText;
  /** 采购单 id（未转单为 null） */
  po_id: string | null;
  /** 采购单号（未转单为 null） */
  po_no: string | null;
  /** 采购单状态 */
  po_status: string | null;
  /** 采购单状态中文 */
  po_status_label: string | null;
  /** 是否计入「已下单」 */
  counted: boolean;
  /** 计入 / 不计入的原因（人话，原样展示） */
  reason: string;
}

/** 计划明细的执行率一行（后端 `PlanExecItemRes`） */
export interface PlanExecItem {
  /** 计划明细 id（**不是** `id`） */
  plan_item_id: string;
  /** 行号 */
  line_no: number;
  /** 物料 */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库名 */
  warehouse_name: string;
  /** 单位 id */
  unit_id: string;
  /** 单位名 */
  unit_name: string;
  /** 计划量 */
  planned_qty: DecimalText;
  /** 计划单价 */
  planned_price: DecimalText;
  /** 计划金额 */
  planned_amount: DecimalText;
  /** 回写列快照：已申请量 */
  booked_qty: DecimalText;
  /** 已申请量（按单据现算） */
  req_qty: DecimalText;
  /** 已下单量 */
  ordered_qty: DecimalText;
  /** 剩余可下单（可为负） */
  remaining_qty: DecimalText;
  /** 还能再转成采购申请的量 */
  convertible_qty: DecimalText;
  /** 执行率（0–1，6 位） */
  exec_rate: DecimalText | null;
  /** 执行率百分比（0–100，2 位） */
  exec_rate_pct: DecimalText | null;
  /** 已申请率 */
  req_rate: DecimalText | null;
  /** 已申请率百分比 */
  req_rate_pct: DecimalText | null;
  /** 行状态 */
  exec_state: ExecState;
  /** 行状态中文 */
  exec_state_label: string;
  /** 是否超计划（前端标红直接读它，不用自己比大小） */
  over_plan: boolean;
  /** 申请明细行数 */
  req_count: number;
  /** 涉及申请单张数 */
  req_doc_count: number;
  /** 涉及采购单张数（存活且未作废） */
  po_doc_count: number;
  /** 来源单据 */
  sources: ExecSource[];
}

/** 单计划执行率详情（后端 `PlanExecutionRes`） */
export interface PlanExecution {
  /** 单头（含汇总与行状态计数） */
  plan: PlanExecRow;
  /** 明细（按行号升序，每行带来源单据） */
  items: PlanExecItem[];
}

/** 去掉空串 / null 参数（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  Object.entries(query).forEach(([key, value]) => {
    if (value === undefined || value === null || value === '') {
      return;
    }
    out[key] = value;
  });
  return out;
}

/** 分页查询采购计划 */
export function fetchPlanPage(query: PlanFilter): Promise<PageData<PlanRow>> {
  return get<PageData<PlanRow>>(`${BASE}/purchase-plan/page`, asParams(query));
}

/** 采购计划详情（单头 + 明细） */
export function fetchPlanDetail(id: string): Promise<PlanDetail> {
  return get<PlanDetail>(`${BASE}/purchase-plan/${id}`);
}

/** 新建采购计划（保存为草稿） */
export function createPlan(payload: PlanPayload): Promise<PlanDetail> {
  return post<PlanDetail>(`${BASE}/purchase-plan`, payload);
}

/** 修改采购计划（**整单替换**：单头 + 全量明细；只有草稿能改） */
export function updatePlan(id: string, payload: PlanPayload): Promise<PlanDetail> {
  return put<PlanDetail>(`${BASE}/purchase-plan/${id}`, payload);
}

/** 删除采购计划（只有草稿能删；软删） */
export function deletePlan(id: string): Promise<void> {
  return del<void>(`${BASE}/purchase-plan/${id}`);
}

/** 提交审批（走 `pp_approve` 流程；≥50 万自动多一道财务经理复核） */
export function submitPlan(id: string, payload: SubmitPlanPayload = {}): Promise<PlanDetail> {
  return post<PlanDetail>(`${BASE}/purchase-plan/${id}/submit`, payload);
}

/** 转采购申请（按仓库**一仓一单**；已执行量在同一事务里回写） */
export function convertPlanToRequisition(
  id: string,
  payload: ToRequisitionPayload,
): Promise<ToRequisitionResult> {
  return post<ToRequisitionResult>(`${BASE}/purchase-plan/${id}/to-requisition`, payload);
}

/** 执行率总览（跨计划；只读） */
export function fetchExecutionPage(query: ExecutionFilter): Promise<PageData<PlanExecRow>> {
  return get<PageData<PlanExecRow>>(`${BASE}/purchase-plan/execution/page`, asParams(query));
}

/** 单计划执行率详情（单头 + 逐行 + 每行来源单据；只读） */
export function fetchPlanExecution(id: string): Promise<PlanExecution> {
  return get<PlanExecution>(`${BASE}/purchase-plan/${id}/execution`);
}
