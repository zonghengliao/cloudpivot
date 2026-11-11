/**
 * 成本核算批次的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * 出处：`backend/src/business/erp/cost_period_dto.rs`、表 `biz_cost_period` /
 * `biz_cost_period_item`（V0373）、设计 `ARCH-9 §4.1 / §5.2 #25–#33 / §6.2.2`。
 *
 * # 六条不能想当然的口径
 *
 * 1. **状态叫 `SUBMITTED`，不叫 `PENDING`**：库层 `ck_biz_cost_period_status` 是
 *    `DRAFT|SUBMITTED|APPROVED|SETTLED|CLOSED|CANCELLED|REVERSED` 七值，卡面写的
 *    `PENDING` 是设计草稿期的旧名（后端 `cost_period_dto` 头部已具名偏差）；
 * 2. **金额一律 `string`**（`NUMERIC(18,4)` → JSON 字符串），要显示就 `formatMoney`，
 *    要累加就用 `shared/decimal.ts` 的定点函数，**别 `Number()`**（会丢精度）；
 * 3. **两套合计不能混用**：单头 `total_*` 是**结算时落库**的审计口径（`DRAFT` 阶段恒 0），
 *    详情 `items_summary` 是**按计算单现算**的作业口径 —— 页面上「试算结果」只用后者；
 * 4. **没有 `POST /cost-period/calc`（设计 #27 编排入口尚未落地）**：本卡只做「建批次 +
 *    看已有计算单」，不要在前端伪造一个本地试算（口径在后端，前端算出来的数没人认）；
 * 5. **一个期间同时只有一个活跃批次**（`uk_biz_cost_period`）：`CANCELLED` / `REVERSED`
 *    不占位，所以红冲/作废之后同期间能重建（后端 409 报文会点名已有批次号）；
 * 6. **结算/红冲都是高危动作**：结算要输入「结算」二字、红冲要输入「红冲」且原因 ≥10 字
 *    （与仓库既有高危确认口径一致，见 `views/business/erp/shared` 的同类弹窗）。
 */

// ==================== 状态词表（逐字抄 ck_biz_cost_period_status） ====================

/** 批次状态值域（7 值） */
export const COST_PERIOD_STATUSES = [
  'DRAFT',
  'SUBMITTED',
  'APPROVED',
  'SETTLED',
  'CLOSED',
  'CANCELLED',
  'REVERSED',
] as const;

/** 批次状态 */
export type CostPeriodStatus = (typeof COST_PERIOD_STATUSES)[number];

/** 状态中文名（与后端 `cost_period_dto::status_label` 逐字一致） */
export const COST_PERIOD_STATUS_LABELS: Record<CostPeriodStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '已提交待审',
  APPROVED: '审批通过',
  SETTLED: '已结算',
  CLOSED: '期间已结账',
  CANCELLED: '已作废',
  REVERSED: '已红冲',
};

/** 状态配色（TDesign tag theme） */
export const COST_PERIOD_STATUS_THEMES: Record<
  CostPeriodStatus,
  'default' | 'primary' | 'warning' | 'success' | 'danger'
> = {
  DRAFT: 'default',
  SUBMITTED: 'warning',
  APPROVED: 'primary',
  SETTLED: 'success',
  CLOSED: 'success',
  CANCELLED: 'default',
  REVERSED: 'danger',
};

/** 人工成本来源（`ck_biz_cost_period_labor`） */
export const LABOR_SOURCES = ['MANUAL', 'PAYROLL'] as const;

/** 人工成本来源 */
export type LaborSource = (typeof LABOR_SOURCES)[number];

/** 人工来源中文（PAYROLL 二期随 ARCH9-2 薪资单上线） */
export const LABOR_SOURCE_LABELS: Record<LaborSource, string> = {
  MANUAL: '手工台账',
  PAYROLL: '薪资单',
};

/** 计算单行状态（`ck_biz_cost_item_status`） */
export const COST_ITEM_STATUS_LABELS: Record<string, string> = {
  CALC: '已计算',
  SETTLED: '已结算',
};

// ==================== 权限码（与 V0378 的 sys_menu.auth_code 逐字一致） ====================

/**
 * 本页五个权限码（设计 §5.3；V0378 已种进 `sys_menu`）。
 *
 * 用 `v-privilege="[PERM.settle]"` 挂按钮：**没有权限码的按钮直接不渲染**，
 * 而不是「渲染了、点下去 403」——与其余 ERP 页同口径。
 */
export const COST_PERIOD_PERMISSIONS = {
  list: 'business:erp:cost-period:list',
  calc: 'business:erp:cost-period:calc',
  submit: 'business:erp:cost-period:submit',
  settle: 'business:erp:cost-period:settle',
  reverse: 'business:erp:cost-period:reverse',
} as const;

// ==================== 高危动作口径（与后端 DTO 常量对齐） ====================

/** 结算要求输入的确认词（设计 §6.2.2 ⑤） */
export const SETTLE_KEYWORD = '结算';
/** 红冲要求输入的确认词（本卡口径，与结算同一套高危确认） */
export const REVERSE_KEYWORD = '红冲';
/** 红冲原因最短字数（后端 `cost_period_dto::REVERSE_REASON_MIN`） */
export const REVERSE_REASON_MIN = 10;
/** 作废原因最短字数（后端 `cost_period_dto::CANCEL_REASON_MIN`） */
export const CANCEL_REASON_MIN = 10;

/** 分页条数（与其余 ERP 列表一致） */
export const PAGE_SIZE = 20;

// ==================== 契约类型 ====================

/** 列表一行（`CostPeriodListRes`） */
export interface CostPeriodRow {
  /** 主键 */
  id: string;
  /** 期间 `yyyy-MM` */
  period_code: string;
  /** 批次号 `CS-yyyyMM-0001` */
  batch_no: string;
  /** 状态 */
  status: CostPeriodStatus;
  /** 人工来源 */
  labor_source: LaborSource;
  /** 成本对象数（结算时落库） */
  object_count: number;
  /** 材料合计（落库值） */
  total_material: string;
  /** 人工合计（落库值） */
  total_labor: string;
  /** 制造费用合计（落库值） */
  total_overhead: string;
  /** 投入合计（落库值） */
  total_cost: string;
  /** 完工转出合计（落库值） */
  total_finished: string;
  /** 期末在制合计（落库值） */
  total_wip: string;
  /** 结算时间 */
  settled_at: string | null;
  /** 制单人姓名（实时 join） */
  created_by_name: string | null;
  /** 创建时间 */
  created_at: string;
  /** 备注 */
  remark: string | null;
}

/** 成本计算单一行（详情 `items[]`） */
export interface CostSheetItem {
  /** 计算单主键 */
  id: string;
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号 */
  object_no: string | null;
  /** 产出品名称 */
  product_name: string | null;
  /** 期初在制数量 */
  wip_beg_qty: string;
  /** 期初在制成本 */
  wip_beg_cost: string;
  /** 本期完工数量 */
  completed_qty: string;
  /** 期末在制数量 */
  wip_end_qty: string;
  /** 期末在制完工进度 % */
  completion_pct: string;
  /** 约当产量（材料） */
  equiv_material: string;
  /** 约当产量（转换成本） */
  equiv_conversion: string;
  /** 本期材料投入 */
  mat_cur: string;
  /** 本期人工投入 */
  labor_cur: string;
  /** 本期制造费用投入 */
  oh_cur: string;
  /** 单位材料成本 */
  unit_material: string;
  /** 单位人工成本 */
  unit_labor: string;
  /** 单位制造费用 */
  unit_overhead: string;
  /** 完工转出成本 */
  finished_cost: string;
  /** 期末在制成本 */
  wip_end_cost: string;
  /** 完工入库单价 */
  unit_finished_cost: string;
  /** 完工入库单（结算后回写） */
  stock_in_id: string | null;
  /** 计算单状态 `CALC` / `SETTLED` */
  status: string;
}

/** 明细现算合计（`CostPeriodSummary`） */
export interface CostPeriodSummary {
  /** 成本对象数（有计算单行的对象数） */
  object_count: number;
  /** 材料合计 */
  total_material: string;
  /** 人工合计 */
  total_labor: string;
  /** 制造费用合计 */
  total_overhead: string;
  /** 投入合计 */
  total_cost: string;
  /** 完工转出合计 */
  total_finished: string;
  /** 期末在制合计 */
  total_wip: string;
}

/** 批次详情（`CostPeriodDetailRes`） */
export interface CostPeriodDetail {
  id: string;
  /** 机构（多组织哨兵值） */
  org_id: string;
  period_code: string;
  batch_no: string;
  status: CostPeriodStatus;
  labor_source: LaborSource;
  object_count: number;
  total_material: string;
  total_labor: string;
  total_overhead: string;
  total_cost: string;
  total_finished: string;
  total_wip: string;
  /** 审批实例（`SUBMITTED` 起必须有） */
  approval_instance_id: string | null;
  /** 人工计提凭证（结算生成） */
  labor_voucher_id: string | null;
  /** 制造费用凭证（结算生成） */
  overhead_voucher_id: string | null;
  /** 完工结转凭证（结算生成） */
  finish_voucher_id: string | null;
  /** 结算时间 */
  settled_at: string | null;
  /** 结算人 */
  settled_by: string | null;
  /** 结算人姓名 */
  settled_by_name: string | null;
  /** 红冲时间 */
  reversed_at: string | null;
  /** 红冲原因 */
  reverse_reason: string | null;
  /** 作废原因 */
  cancel_reason: string | null;
  remark: string | null;
  created_by_name: string | null;
  created_at: string;
  updated_at: string;
  /** 成本计算单 */
  items: CostSheetItem[];
  /** 明细现算合计 */
  items_summary: CostPeriodSummary;
  /** 当前态可去的状态（读后端**有效边**，管理端禁用某条边后就不出现） */
  next_statuses: string[];
  /** 有效边表 */
  legal_edges: [string, string][];
}

/** 列表筛选（逐字抄 `CostPeriodPageQuery`） */
export interface CostPeriodPageQuery {
  /** 期间 `yyyy-MM` */
  period_code?: string;
  /** 状态 */
  status?: CostPeriodStatus | '';
  /** 关键字（批次号 / 期间 / 备注） */
  keyword?: string;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

/** 新建批次（`CostPeriodCreateReq`） */
export interface CostPeriodCreatePayload {
  /** 期间 `yyyy-MM`（必填） */
  period_code: string;
  /** 人工来源（缺省 `MANUAL`） */
  labor_source?: LaborSource;
  /** 备注 */
  remark?: string;
}

/** 编辑批次（只放「改数不算账」的两个字段；只有 `DRAFT` 可改） */
export interface CostPeriodUpdatePayload {
  labor_source?: LaborSource;
  remark?: string;
}

/** 带原因的高危动作入参（作废 / 红冲同形） */
export interface CostPeriodReasonPayload {
  reason: string;
}
