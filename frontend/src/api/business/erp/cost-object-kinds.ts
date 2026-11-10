/**
 * 成本对象的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * # 为什么单独成文件
 *
 * 与 `expense-kinds.ts` / `asset-kinds.ts` / `doc-kinds.ts` 同款理由：词表要能被
 * **纯逻辑单测**引用。`cost-object.ts` 里有 `import { get, post, put } from '@/api/request'`，
 * 而 `request.ts` 顶层读 `import.meta.env` —— 在 node 里跑 `tsx --test` 会直接崩。
 * 把词表和类型切出来后，`cost-object-logic.ts` 只依赖这个文件，单测零依赖可跑。
 *
 * # 取值来源（改这里之前先改后端）
 *
 * - 状态：`backend/src/business/erp/cost_object_dto.rs::STATUS_*`
 *   + 迁移 `V0370` 的 `ck_biz_cost_object_status`
 * - 对象类型 / 投料方式：同文件 `OBJECT_TYPES` / `MODES` + `ck_biz_cost_object_type` / `_input`
 * - 关批拦截原因：同文件 `BLOCK_*`；业务码 `66055` 见 `backend/src/shared/error_code.rs`
 * - 权限码：迁移 `V0378` 的 `sys_menu.auth_code` 种子
 *
 * 加值会同时踩后端的状态机 / CHECK 约束巡检，前端不要自行发明取值。
 */

// ==================== 词表 ====================

/** 成本对象状态（`ck_biz_cost_object_status`） */
export const COST_OBJECT_STATUSES = ['OPEN', 'CLOSED', 'CANCELLED'] as const;

/** 成本对象状态 */
export type CostObjectStatus = (typeof COST_OBJECT_STATUSES)[number];

/** 状态中文（页面只认这一份译名） */
export const COST_OBJECT_STATUS_LABELS: Record<CostObjectStatus, string> = {
  OPEN: '在产',
  CLOSED: '已关批',
  CANCELLED: '已作废',
};

/** 投料方式（`ck_biz_cost_object_input`；一期只支持 `ONE_TIME`） */
export const MATERIAL_INPUT_MODES = ['ONE_TIME', 'AS_PROGRESS'] as const;

/** 投料方式 */
export type MaterialInputMode = (typeof MATERIAL_INPUT_MODES)[number];

/** 投料方式中文 */
export const MATERIAL_INPUT_MODE_LABELS: Record<MaterialInputMode, string> = {
  ONE_TIME: '一次投入',
  AS_PROGRESS: '随进度投入',
};

/** 对象类型（一期只写 `BATCH`，`ORDER` / `PRODUCT` 值域预留） */
export const COST_OBJECT_TYPES = ['BATCH', 'ORDER', 'PRODUCT'] as const;

/** 对象类型 */
export type CostObjectType = (typeof COST_OBJECT_TYPES)[number];

/** 关批被拦的业务码（`error_code::ERP_COST_OBJECT_CLOSE_BLOCKED`） */
export const COST_OBJECT_CLOSE_BLOCKED_CODE = 66055;

/** 关批拦截原因分类（后端 `BLOCK_*` 常量，逐字一致） */
export const CLOSE_BLOCK_CODES = [
  'DRAFT_LABOR',
  'UNCOLLECTED_STOCK_OUT',
  'PERIOD_LOCKED',
  'UNPOSTED_STOCK_OUT',
  'WIP_NOT_ZERO',
] as const;

/** 关批拦截原因分类 */
export type CloseBlockCode = (typeof CLOSE_BLOCK_CODES)[number];

/** 拦截原因的人话指引（`msg` 已带详细文案，这里只补「去哪修」） */
export const CLOSE_BLOCK_HINTS: Record<CloseBlockCode, string> = {
  DRAFT_LABOR: '去「工时台账」把这些行确认或删除',
  UNCOLLECTED_STOCK_OUT: '去「出库单」把领料归集到本对象（或重跑成本计算）',
  PERIOD_LOCKED: '等该期间成本批次回到草稿，或红冲已结算批次',
  UNPOSTED_STOCK_OUT: '去「出库单」过账或作废这些领料单',
  WIP_NOT_ZERO: '确认带在制关批（勾选确认框）或先把在制数量结算掉',
};

// ==================== ① 列表 ====================

/**
 * 列表行（逐字抄 `CostObjectListRes`，一个字段不多一个不少）。
 *
 * ⚠️ 没有 `start_date` / `due_date` / `completion_pct` —— 想看日期去详情接口。
 */
export interface CostObjectRow {
  /** 主键 */
  id: string;
  /** 对象号 `CB-yyyyMMdd-NNNNNN` */
  object_no: string;
  /** 对象类型（一期恒 `BATCH`） */
  object_type: CostObjectType;
  /** 产出品 id */
  product_id: string;
  /** 产出品名称 */
  product_name: string | null;
  /** 生产车间 id */
  workshop_dept_id: string;
  /** 生产车间名称（实时 join 优先、建单快照兜底） */
  workshop_dept_name: string | null;
  /** 计划数量（字符串） */
  plan_qty: string;
  /** 累计完工数量（字符串） */
  completed_qty: string;
  /** 期末在制数量（字符串） */
  wip_qty: string;
  /** 状态 */
  status: CostObjectStatus;
  /** 产出批号 */
  lot_no: string | null;
  /** 上游订单号 */
  source_doc_no: string | null;
  /** 归集总成本（字符串；无归集时 `"0"`，有归集时 `"100.0000"`） */
  cost_total: string;
}

/**
 * 列表筛选（`CostObjectPageQuery`）。
 *
 * `start_from` / `start_to` 是**开工日期区间**（含边界，`YYYY-MM-DD`）——
 * 列表接口没有 `period_code` 概念，页面上的「开工期间」月份筛选就是折算成这两个参数。
 */
export interface CostObjectPageQuery {
  /** 关键字：对象号 / 产出批号 / 上游订单号（模糊） */
  keyword?: string;
  /** 状态 */
  status?: CostObjectStatus | '';
  /** 产出品 id */
  product_id?: string;
  /** 生产车间 id */
  workshop_dept_id?: string;
  /** 开工日期 ≥（含，`YYYY-MM-DD`） */
  start_from?: string;
  /** 开工日期 ≤（含，`YYYY-MM-DD`） */
  start_to?: string;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（0 = 默认 20，上限 200） */
  page_size?: number;
}

// ==================== ② 详情 ====================

/** 归集汇总（`CollectSummary`） */
export interface CollectSummary {
  /** 直接材料 */
  mat: string;
  /** 直接人工 */
  labor: string;
  /** 制造费用 */
  oh: string;
  /** 合计（后端由三项相加，不另查库） */
  total: string;
}

/** 成本计算单行（`CostPeriodItemRes`，来自 `biz_cost_period_item`） */
export interface CostPeriodItem {
  /** 期间 `yyyy-MM` */
  period_code: string;
  /** 批次状态 `DRAFT / SUBMITTED / APPROVED / SETTLED / …` */
  period_status: string;
  /** 计算单行状态 `CALC / SETTLED` */
  status: string;
  /** 期初在制数量 */
  wip_beg_qty: string;
  /** 期初在制成本 */
  wip_beg_cost: string;
  /** 本期完工数量 */
  completed_qty: string;
  /** 期末在制数量 */
  wip_end_qty: string;
  /** 完工进度% */
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
  /** 单位完工成本（→ 入库单价） */
  unit_finished_cost: string;
  /** 完工入库单（结算后才有） */
  stock_in_id: string | null;
}

/** 详情（`CostObjectDetailRes`；建 / 改 / 关批成功后返回的也是它） */
export interface CostObjectDetail {
  /** 主键 */
  id: string;
  /** 对象号 */
  object_no: string;
  /** 对象类型 */
  object_type: CostObjectType;
  /** 产出品 id */
  product_id: string;
  /** 产出品名称 */
  product_name: string | null;
  /** 产出品编码（显示「MAT-0001 钢轴」用） */
  product_code: string | null;
  /** 产出批号 */
  lot_no: string | null;
  /** 目标入库仓 id */
  target_warehouse_id: string;
  /** 目标仓名称 */
  target_warehouse_name: string | null;
  /** 目标库位 id */
  target_location_id: string | null;
  /** 目标库位名称 */
  target_location_name: string | null;
  /** 计划数量 */
  plan_qty: string;
  /** 累计完工数量 */
  completed_qty: string;
  /** 期末在制数量 */
  wip_qty: string;
  /** 期末在制完工进度% */
  completion_pct: string;
  /** 投料方式 */
  material_input_mode: MaterialInputMode;
  /** 上游订单号 */
  source_doc_no: string | null;
  /** 客户 id */
  customer_id: string | null;
  /** 客户名称 */
  customer_name: string | null;
  /** 生产车间 id */
  workshop_dept_id: string;
  /** 生产车间名称 */
  workshop_dept_name: string | null;
  /** 开工日期 `YYYY-MM-DD` */
  start_date: string;
  /** 计划完工日期 */
  due_date: string | null;
  /** 状态 */
  status: CostObjectStatus;
  /** 关批时间（RFC3339） */
  closed_at: string | null;
  /** 关批人 */
  closed_by: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
  /** 归集汇总 */
  collect_summary: CollectSummary;
  /** 成本计算单（按期倒序） */
  period_items: CostPeriodItem[];
  /** 当前态可去的状态（读有效边：管理端禁用某条边后这里就不再出现它） */
  next_statuses: CostObjectStatus[];
  /** 有效边表（与 `next_statuses` 同源） */
  legal_edges: Array<[string, string]>;
}

// ==================== ③ 新建 / ④ 编辑 / ⑤ 关批入参 ====================

/**
 * 新建请求（`CostObjectCreateReq`，字段全 `Option`）。
 *
 * 必填（后端 `spec_from_create` 会 400）：`product_id` / `target_warehouse_id` /
 * `plan_qty`（> 0）/ `workshop_dept_id` / `start_date`（`YYYY-MM-DD`）。
 */
export interface CostObjectCreatePayload {
  /** 产出品（物料，只允许 `FINISHED` / `SEMI`） */
  product_id: string;
  /** 目标入库仓 */
  target_warehouse_id: string;
  /** 目标库位（可空） */
  target_location_id?: string | null;
  /** 计划数量（> 0；字符串原样传，后端 `Decimal` 收） */
  plan_qty: string;
  /** 生产车间 */
  workshop_dept_id: string;
  /** 开工日期 `YYYY-MM-DD` */
  start_date: string;
  /** 计划完工日期（≥ 开工日期） */
  due_date?: string | null;
  /** 上游订单号 */
  source_doc_no?: string | null;
  /** 客户 */
  customer_id?: string | null;
  /** 产出批号（填了自动建 `biz_lot`，`source_type=PRODUCTION`） */
  lot_no?: string | null;
  /** 投料方式（缺省 `ONE_TIME`；一期传 `AS_PROGRESS` 直接 400） */
  material_input_mode?: MaterialInputMode;
  /** 备注 */
  remark?: string | null;
}

/**
 * 编辑请求（`CostObjectUpdateReq`）：**部分更新 + 三态**。
 *
 * * 字段**缺失** ⇒ 不改；
 * * 可空字段显式 `null` ⇒ 清空；
 * * 有值 ⇒ 写入。
 *
 * 只有 `OPEN` 的对象能改；`product_id` 在已有归集行时后端 409
 * （「已有 N 行归集，产出品不能再改」）。
 */
export type CostObjectUpdatePayload = Partial<CostObjectCreatePayload>;

/** 关批请求（`CostObjectCloseReq`） */
export interface CostObjectClosePayload {
  /**
   * 带在制关批（§4.1 前置 ⑤ 的显式确认）。
   *
   * `wip_qty > 0` 且不带 `true` ⇒ 409 + `66055`（`[WIP_NOT_ZERO]`）。
   */
  force_wip?: boolean;
  /** 关批备注（落 `remark` 与状态日志 reason） */
  remark?: string | null;
}
