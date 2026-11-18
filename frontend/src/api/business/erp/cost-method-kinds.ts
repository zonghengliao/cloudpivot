/**
 * 成本方法配置的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * 出处：`backend/app/src/business/erp/cost/cost_method_dto.rs`、表 `biz_cost_method_config`（V0459）、
 * 菜单与权限码 `V0460`。
 *
 * # 三件事要分清
 *
 * | 事情 | 在哪 | 值域 |
 * |---|---|---|
 * | 存货**计价法**（出库按什么价结转） | 物料主数据 `costing_method` | 只有移动加权（V0201 收敛） |
 * | 成本**核算方法**（料工费按什么粒度归集） | 本页 | `JOB` / `VARIETY` / `STANDARD` |
 * | 成本**对象**（归集到哪个筐） | 成本对象页 | 生产批 |
 *
 * # 一期只实现分批法（ARCH-9 D-3）
 *
 * 三种方法都能配，但只有 `JOB`（分批法）能算：品种法是三期（D-16）、标准成本二期只做差异分析
 * 不改计价（D-15）。配了未实现的方法，点「计算成本」会被后端 `409 + 66101` 拒绝 ——
 * 本页在下拉与列表里就把它标出来，不让用户踩到才知道。
 *
 * # 生效期即版本（改方法不溯及历史）
 *
 * 切换方法的正确姿势：把**旧版本**的停止日改到切换日前一天 → 新建一条从切换日生效的版本。
 * 已生效过（`effective_from < 今天`）的版本只允许改 停止日 / 状态 / 备注（后端 `409 + 66105`），
 * 也不允许删除（`409 + 66104`）—— 它是「那段期间按哪种方法算」的凭据。
 */

// ==================== 词表 ====================

/** 核算方法值域（`ck_biz_cmc_method`） */
export const COST_METHODS = ['JOB', 'VARIETY', 'STANDARD'] as const;

/** 核算方法 */
export type CostMethodCode = (typeof COST_METHODS)[number];

/** 方法 → 中文名（与后端 `method_label` 逐字一致） */
export const COST_METHOD_LABELS: Record<CostMethodCode, string> = {
  JOB: '分批法',
  VARIETY: '品种法',
  STANDARD: '标准成本',
};

/** 方法 → 一句话解释（与后端 `method_note` 同义，比后端短一截，列表 tooltip 用） */
export const COST_METHOD_NOTES: Record<CostMethodCode, string> = {
  JOB: '按成本对象（生产批）归集并结转：一期已实现，计算按它执行',
  VARIETY: '按产品归集、不分批次：一期只可配置，计算引擎未实现（三期）',
  STANDARD: '以标准成本卡为基准做差异：一期只可配置，计算引擎未实现（二期）',
};

/** 已实现的方法集合（一期只有分批法；后端 `method_implemented` 的同口径前端版） */
export const COST_METHOD_IMPLEMENTED: Record<CostMethodCode, boolean> = {
  JOB: true,
  VARIETY: false,
  STANDARD: false,
};

/** 代码兜底方法（没有任何配置命中时后端按它算） */
export const COST_METHOD_DEFAULT: CostMethodCode = 'JOB';

/** 范围值域（`ck_biz_cmc_scope`） */
export const COST_SCOPES = ['COST_OBJECT', 'GLOBAL'] as const;

/** 范围 */
export type CostScope = (typeof COST_SCOPES)[number];

/** 范围 → 中文名（与后端 `scope_label` 一致） */
export const COST_SCOPE_LABELS: Record<CostScope, string> = {
  COST_OBJECT: '指定成本对象',
  GLOBAL: '全公司默认',
};

/** 判定来源（与后端 `source` / `source_label` 一致） */
export const EFFECTIVE_SOURCE_LABELS: Record<string, string> = {
  COST_OBJECT: '成本对象级配置',
  GLOBAL: '全公司默认配置',
  DEFAULT: '代码兜底（未配置任何方法，默认分批法）',
};

/** 启用 */
export const COST_METHOD_ACTIVE = 1;
/** 停用 */
export const COST_METHOD_DISABLED = 0;

/** 状态中文（与后端 `status_label` 一致） */
export const COST_METHOD_STATUS_LABELS: Record<number, string> = {
  1: '启用',
  0: '停用',
};

/** 每页条数（与其它成本页一致） */
export const PAGE_SIZE = 20;

/**
 * 本页 4 个权限码（`V0460` 的 4 行菜单逐一对应）。
 *
 * `list` 同时承载「分页」与「生效方法查询」两条只读接口。
 */
export const COST_METHOD_PERMISSIONS = {
  list: 'business:erp:cost-method:list',
  create: 'business:erp:cost-method:create',
  update: 'business:erp:cost-method:update',
  delete: 'business:erp:cost-method:delete',
} as const;

// ==================== 业务码（与后端 `error_code.rs` 一致） ====================

/** 所选方法在引擎里还没实现（计算入口拦下） */
export const COST_METHOD_UNIMPLEMENTED_CODE = 66101;
/** 同范围生效期重叠 */
export const COST_METHOD_OVERLAP_CODE = 66102;
/** 范围参数不合法 */
export const COST_METHOD_SCOPE_INVALID_CODE = 66103;
/** 删除已生效过的配置 */
export const COST_METHOD_DELETE_IN_USE_CODE = 66104;
/** 改写已生效版本的方法 / 范围 / 生效日 */
export const COST_METHOD_LOCKED_EFFECTIVE_CODE = 66105;

// ==================== 契约类型 ====================

/** 列表一行（`CostMethodRes`） */
export interface CostMethodRow {
  id: string;
  method: CostMethodCode | string;
  method_label: string;
  method_implemented: boolean;
  method_note: string;
  scope: CostScope | string;
  scope_label: string;
  scope_id: string | null;
  scope_object_no: string | null;
  scope_object_status: string | null;
  effective_from: string;
  effective_to: string | null;
  status: number;
  status_label: string;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 分页入参 */
export interface CostMethodPageQuery {
  scope?: string;
  method?: string;
  status?: number;
  scope_id?: string;
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 新建入参（`CostMethodCreateReq`） */
export interface CostMethodCreatePayload {
  method: string;
  scope: string;
  scope_id?: string | null;
  effective_from: string;
  effective_to?: string | null;
  status?: number;
  remark?: string;
}

/** 修改入参（`CostMethodUpdateReq`：缺省 = 不改；`effective_to` / `remark` 空串 = 清空） */
export interface CostMethodUpdatePayload {
  method?: string;
  scope?: string;
  scope_id?: string | null;
  effective_from?: string;
  effective_to?: string;
  status?: number;
  remark?: string;
}

/** 生效方法查询入参 */
export interface CostMethodEffectiveQuery {
  cost_object_id?: string;
  on?: string;
}

/** 生效方法查询出参（`CostMethodEffectiveRes`） */
export interface CostMethodEffectiveRes {
  cost_object_id: string | null;
  cost_object_no: string | null;
  on_date: string;
  method: CostMethodCode | string;
  method_label: string;
  method_implemented: boolean;
  method_note: string;
  source: string;
  source_label: string;
  config_id: string | null;
  effective_from: string | null;
  effective_to: string | null;
  engine_hint: string;
}
