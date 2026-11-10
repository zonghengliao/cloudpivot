/**
 * 分摊规则的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * 出处：`backend/src/business/erp/cost_alloc_rule_dto.rs`、表 `biz_cost_alloc_rule`（V0372）。
 *
 * # 三条口径
 *
 * 1. **规则是版本化的**：`(车间, 科目)` 是"规则族"的键，`effective_from` 更晚的版本赢。
 *    11 月要改口径 ⇒ **新建一条 `effective_from = 2026-11-01` 的规则**，10 月重算仍拿旧版本
 *    （改规则不溯及历史，§4.4.5）；
 * 2. **已参与过分摊的版本只允许改 规则名 / 备注 / 状态**（后端 409）。想改口径就新建版本 ——
 *    这条界面上没法预先判断（列表出参没有"是否参与过"），所以靠 409 报文引导；
 * 3. **试算只算不写**：`POST /preview` 返回逐对象 `basis_value / alloc_amount / rounding_adj`，
 *    守恒 `Σ alloc_amount == pool_amount`（最大余数法，尾差只落一行）。
 *
 * # 四种分配标准的基数
 *
 * | 标准 | 基数 | 来源 |
 * |------|------|------|
 * | `LABOR_HOURS` | 当期人工工时 | `biz_cost_labor`（CONFIRMED） |
 * | `MACHINE_HOURS` | 当期机器工时 | `biz_cost_labor.machine_hours` |
 * | `OUTPUT_QTY` | 当期完工数量 | 成本计算单 / 成本对象 |
 * | `MATERIAL_COST` | 当期材料成本 | `biz_cost_collect`（element=MATERIAL） |
 */

// ==================== 词表 ====================

/** 分配标准值域（`ck_biz_cost_rule_basis`） */
export const ALLOC_BASES = ['LABOR_HOURS', 'MACHINE_HOURS', 'OUTPUT_QTY', 'MATERIAL_COST'] as const;

/** 分配标准 */
export type AllocBasis = (typeof ALLOC_BASES)[number];

/** 分配标准中文（与后端 `basis_label` 逐字一致） */
export const ALLOC_BASIS_LABELS: Record<AllocBasis, string> = {
  LABOR_HOURS: '按人工工时分摊',
  MACHINE_HOURS: '按机器工时分摊',
  OUTPUT_QTY: '按本期完工数量分摊',
  MATERIAL_COST: '按材料成本分摊',
};

/** 分配标准的一句话解释（下拉里跟在名字后面，设计 §6.2.3 要求） */
export const ALLOC_BASIS_HINTS: Record<AllocBasis, string> = {
  LABOR_HOURS: '工时 = 按人工工时占比（不填机器工时的车间最常用）',
  MACHINE_HOURS: '按机器工时占比（设备密集型车间更公平）',
  OUTPUT_QTY: '按本期完工数量占比（工序少、工时不准时用）',
  MATERIAL_COST: '按材料成本占比（材料占大头的产品）',
};

/** 启用状态（`ck_biz_cost_rule_status`：1 启用 / 0 停用） */
export const ALLOC_RULE_STATUSES = [1, 0] as const;
/** 启用 */
export const ALLOC_RULE_ACTIVE = 1;
/** 停用 */
export const ALLOC_RULE_DISABLED = 0;

/** 状态中文（与后端 `status_label` 一致） */
export const ALLOC_RULE_STATUS_LABELS: Record<number, string> = {
  1: '启用',
  0: '停用',
};

/** 制造费用科目前缀（`ck_biz_cost_rule_account`） */
export const ALLOC_ACCOUNT_PREFIX = '4101.';
/** 规则编码长度上限 */
export const RULE_CODE_MAX = 32;
/** 规则名长度上限 */
export const RULE_NAME_MAX = 120;
/** 优先级缺省 / 上下限（`priority INT DEFAULT 10`） */
export const PRIORITY_DEFAULT = 10;
/** 优先级下限 */
export const PRIORITY_MIN = 1;
/** 优先级上限 */
export const PRIORITY_MAX = 999;

/** 科目非法（`error_code::ERP_COST_OVERHEAD_ACCOUNT`，与费用台账共用） */
export const ALLOC_ACCOUNT_CODE = 66060;
/** 池有金额但当期无在产对象（`ERP_COST_ALLOC_NO_OBJECT`） */
export const ALLOC_NO_OBJECT_CODE = 66062;
/** 该标准基数合计为 0（`ERP_COST_ALLOC_BASIS_ZERO`） */
export const ALLOC_BASIS_ZERO_CODE = 66063;

/** 权限码（V0378 种子逐一对应） */
export const COST_ALLOC_RULE_PERMISSIONS = {
  list: 'business:erp:cost-alloc-rule:list',
  create: 'business:erp:cost-alloc-rule:create',
  update: 'business:erp:cost-alloc-rule:update',
  delete: 'business:erp:cost-alloc-rule:delete',
  preview: 'business:erp:cost-alloc-rule:preview',
} as const;

/** 列表每页条数 */
export const PAGE_SIZE = 20;

// ==================== ① 列表 ====================

/** 规则行（逐字抄 `CostAllocRuleListRes`） */
export interface CostAllocRuleRow {
  /** 主键 */
  id: string;
  /** 规则编码（唯一，大写） */
  rule_code: string;
  /** 规则名 */
  rule_name: string;
  /** 业务类型（一期恒 `OVERHEAD`） */
  biz_type: string;
  /** 被摊车间 id */
  source_dept_id: string;
  /** 车间名（实时名优先，落空回落快照） */
  source_dept_name: string | null;
  /** 被摊科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string | null;
  /** 分配标准 */
  basis: string;
  /** 分配标准中文 */
  basis_label: string;
  /** 优先级 */
  priority: number;
  /** 生效开始日 */
  effective_from: string;
  /** 生效结束日（空 = 长期有效） */
  effective_to: string | null;
  /** 状态 1/0 */
  status: number;
  /** 状态中文 */
  status_label: string;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 列表筛选（`CostAllocRulePageQuery`） */
export interface CostAllocRulePageQuery {
  /** 会计期间：只返回生效期覆盖该期间的规则 */
  period_code?: string;
  /** 被摊车间 */
  dept_id?: string;
  /** 被摊科目 */
  account_code?: string;
  /** 分配标准 */
  basis?: AllocBasis | '';
  /** 状态 1/0 */
  status?: number;
  /** 关键字：编码 / 名称 / 车间 / 科目 / 备注 */
  keyword?: string;
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

// ==================== ② 新建 / ③ 修改 ====================

/** 新建规则（`CostAllocRuleCreateReq`） */
export interface CostAllocRuleCreatePayload {
  /** 规则编码（大写字母/数字/_-. ，≤32） */
  rule_code: string;
  /** 规则名 */
  rule_name: string;
  /** 被摊车间 */
  dept_id: string;
  /** 被摊科目（必须 4101.*） */
  account_code: string;
  /** 分配标准 */
  basis: AllocBasis;
  /** 优先级（缺省 10） */
  priority?: number;
  /** 生效开始日 `YYYY-MM-DD` */
  effective_from: string;
  /** 生效结束日（空 = 长期） */
  effective_to?: string | null;
  /** 状态 1/0 */
  status?: number;
  /** 备注 */
  remark?: string | null;
}

/**
 * 修改规则（`CostAllocRuleUpdateReq`）：**缺省 = 不改**。
 *
 * `effective_to` 传**空串** = 清空（改成长期有效）；不传 = 不改。
 * 已参与过分摊的版本只放行 `rule_name` / `remark` / `status`（其它字段后端 409）。
 */
export type CostAllocRuleUpdatePayload = Partial<CostAllocRuleCreatePayload>;

// ==================== ④ 试算（只算不写） ====================

/** 试算入参 */
export interface CostAllocPreviewReq {
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 只算这几条规则（空 = 该期间生效的全部规则） */
  rule_ids?: string[];
}

/** 试算里的一行（= 分摊结果一行的口径，但不落库） */
export interface CostAllocPreviewLine {
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号 */
  cost_object_no: string;
  /** 对象状态 `OPEN / CLOSED` */
  cost_object_status: string;
  /** 本对象在该标准下的基数（字符串） */
  basis_value: string;
  /** 摊到本对象的金额（含尾差） */
  alloc_amount: string;
  /** 尾差调整（**只有一行**非 0） */
  rounding_adj: string;
}

/** 一条规则的试算结果 */
export interface CostAllocPreviewGroup {
  /** 规则 id */
  rule_id: string;
  /** 规则编码 */
  rule_code: string;
  /** 规则名 */
  rule_name: string;
  /** 车间 id */
  source_dept_id: string;
  /** 车间名 */
  source_dept_name: string;
  /** 科目编码 */
  account_code: string;
  /** 科目名 */
  account_name: string | null;
  /** 分配标准 */
  basis: string;
  /** 分配标准中文 */
  basis_label: string;
  /** 待分摊池金额 */
  pool_amount: string;
  /** 基数合计 */
  basis_total: string;
  /** 本组分摊合计（= 池，守恒） */
  alloc_total: string;
  /** 本组尾差合计 */
  rounding_total: string;
  /** 逐对象明细 */
  rows: CostAllocPreviewLine[];
}

/** 有池但没规则能摊（诊断） */
export interface CostAllocPreviewUnallocated {
  /** 车间 id */
  source_dept_id: string | null;
  /** 车间名 */
  source_dept_name: string | null;
  /** 科目编码 */
  account_code: string | null;
  /** 科目名 */
  account_name: string | null;
  /** 未分摊金额 */
  pool_amount: string;
  /** 为什么没摊 */
  reason: string;
}

/** 试算出参 */
export interface CostAllocPreviewRes {
  /** 会计期间 */
  period_code: string;
  /** 逐规则结果 */
  groups: CostAllocPreviewGroup[];
  /** 已摊池合计 */
  total_pool_amount: string;
  /** 已摊金额合计（守恒时等于上一项） */
  total_alloc_amount: string;
  /** 未分摊池 */
  unallocated: CostAllocPreviewUnallocated[];
}
