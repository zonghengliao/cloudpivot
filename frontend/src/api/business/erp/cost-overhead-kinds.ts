/**
 * 手工费用台账的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * # 字段名逐字抄后端
 *
 * 出处：`backend/src/business/erp/cost_overhead_dto.rs`
 * （`CostOverheadListRes` / `CostOverheadCreateReq`）、表 `biz_cost_overhead`（V0371）。
 *
 * # 三条口径
 *
 * 1. **`cost_object_id` 可空**：空 = 车间费用池（等分摊，§4.3），非空 = 直接计入该批。
 *    这是费用台账最重要的一条语义 —— 界面上必须一眼能分清「池」与「直接」；
 * 2. **借方科目必须 `4101.*`**（`ck_biz_cost_oh_debit`），缺省 `4101.09` 制造费用-其他；
 *    且必须是**账套里存在 + 末级 + 启用**（后端查库校验，`400 + 66060`）；
 * 3. **金额 > 0 且 ≤ 4 位小数**，不静默舍入（`NUMERIC(18,4)` 会把 8.5555 舍成 8.5556，
 *    金额就错了且没人会发现）。
 *
 * # 没有的东西
 *
 * * **状态列**：`biz_cost_overhead` 没有 status —— 台账行要么在（未删）要么被软删；
 * * **编辑接口**：设计 §5.2 只给了 list / create / delete 三个（没有 PUT），
 *   所以页面不提供"编辑"，录错请删掉重录（已被归集引用时后端 409）。
 */

// ==================== 词表 ====================

/** 费用项目值域（`ck_biz_cost_oh_item`，与字典 `cost_expense_item` 的 7 项一一对应） */
export const EXPENSE_ITEMS = [
  'DEPRECIATION',
  'UTILITY',
  'RENT',
  'REPAIR',
  'WORKSHOP_ADMIN',
  'CONSUMABLE',
  'OTHER',
] as const;

/** 费用项目 */
export type ExpenseItem = (typeof EXPENSE_ITEMS)[number];

/**
 * 费用项目中文（**兜底**：字典 `cost_expense_item` 才是唯一真源）。
 *
 * 列表出参里已有 `expense_item_label`（字典实时名），字典行被删时才用这份。
 */
export const EXPENSE_ITEM_LABELS: Record<ExpenseItem, string> = {
  DEPRECIATION: '折旧费',
  UTILITY: '水电费',
  RENT: '租金',
  REPAIR: '修理费',
  WORKSHOP_ADMIN: '车间管理费',
  CONSUMABLE: '机物料消耗',
  OTHER: '其他',
};

/** 字典编码（费用项目下拉走它） */
export const EXPENSE_ITEM_DICT = 'cost_expense_item';

/** 借方科目前缀（`ck_biz_cost_oh_debit`） */
export const MANUFACTURING_ACCOUNT_PREFIX = '4101.';

/** 借方科目缺省值（DDL 默认） */
export const DEFAULT_DEBIT_ACCOUNT = '4101.09';

/** 金额精度（`amount NUMERIC(18,4)`） */
export const AMOUNT_SCALE = 4;

/** 金额非法（`error_code::ERP_COST_OVERHEAD_AMOUNT`：> 0 且最多 4 位小数） */
export const OVERHEAD_AMOUNT_CODE = 66061;
/** 科目非法（`error_code::ERP_COST_OVERHEAD_ACCOUNT`，与分摊规则共用） */
export const OVERHEAD_ACCOUNT_CODE = 66060;

/** 权限码（V0378 种子逐一对应；**没有 update** —— 设计 §5.2 只给了 3 个） */
export const COST_OVERHEAD_PERMISSIONS = {
  list: 'business:erp:cost-overhead:list',
  create: 'business:erp:cost-overhead:create',
  delete: 'business:erp:cost-overhead:delete',
} as const;

/** 列表每页条数 */
export const PAGE_SIZE = 20;
/** 下拉候选上限 */
export const OPTION_LIMIT = 400;

// ==================== ① 列表 ====================

/** 列表行（逐字抄 `CostOverheadListRes`） */
export interface CostOverheadRow {
  /** 主键 */
  id: string;
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 成本对象（空 = 车间费用池） */
  cost_object_id: string | null;
  /** 对象号（空 = 池行） */
  cost_object_no: string | null;
  /** 成本对象状态 */
  cost_object_status: string | null;
  /** 车间 id */
  dept_id: string;
  /** 车间名（实时名优先，落空回落快照） */
  dept_name: string | null;
  /** 费用项目编码 */
  expense_item: string;
  /** 费用项目名（字典实时名，字典删了回落内置文案） */
  expense_item_label: string | null;
  /** 借方科目编码 */
  debit_account_code: string;
  /** 借方科目名 */
  debit_account_name: string | null;
  /** 对方科目编码 */
  credit_account_code: string;
  /** 对方科目名 */
  credit_account_name: string | null;
  /** 金额（字符串，4 位小数） */
  amount: string;
  /** 来源 `MANUAL` / `IMPORT` */
  source_type: string;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/** 列表筛选（`CostOverheadPageQuery`） */
export interface CostOverheadPageQuery {
  /** 会计期间 */
  period_code?: string;
  /** 成本对象 id */
  cost_object_id?: string;
  /** 车间 id */
  dept_id?: string;
  /** 费用项目 */
  expense_item?: string;
  /** 关键字：科目编码/名称、车间名、备注 */
  keyword?: string;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

// ==================== ② 新建 ====================

/**
 * 新建费用台账行（`CostOverheadCreateReq`）。
 *
 * 必填：`period_code` / `dept_id` / `expense_item` / `credit_account_code` / `amount`（> 0）。
 * `debit_account_code` 可空（缺省 `4101.09`）；`cost_object_id` 可空（空 = 车间池）。
 */
export interface CostOverheadCreatePayload {
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 成本对象（空 = 车间费用池；填 = 直接计入该批） */
  cost_object_id?: string | null;
  /** 车间（必填；池的归属车间，也是分摊规则的匹配键） */
  dept_id: string;
  /** 费用项目 */
  expense_item: string;
  /** 借方科目（可空，缺省 4101.09；必须 4101.* 且启用末级） */
  debit_account_code?: string | null;
  /** 对方科目（必填，如 1002 / 2241.01 / 1801） */
  credit_account_code: string;
  /** 金额（> 0，≤ 4 位小数；字符串原样传） */
  amount: string;
  /** 备注 */
  remark?: string | null;
}
