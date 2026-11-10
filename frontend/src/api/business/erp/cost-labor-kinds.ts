/**
 * 工时台账的词表与契约类型（**纯声明 / 纯常量，不 import 任何请求层**）。
 *
 * # 为什么单独成文件
 *
 * 与 `cost-object-kinds.ts` / `expense-kinds.ts` 同款：`api/request.ts` 顶层读
 * `import.meta.env`，node 下 `tsx --test` 直接崩。词表切出来后，
 * `cost-labor-logic.ts` 只依赖本文件，纯逻辑单测零依赖可跑。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 状态 | `status` ∈ `DRAFT / CONFIRMED` | `ck_biz_cost_labor_status` / `cost_labor_dto::STATUSES` |
 * | 来源 | `source_type` ∈ `MANUAL / IMPORT / MES` | `ck_biz_cost_labor_src` |
 * | 工时 / 机器工时 | `hours` / `machine_hours`（**字符串**） | `CostLaborListRes` |
 * | 费率 / 金额 | `rate` / `amount`（**字符串**；金额 = round(工时×费率, 4)） | 同上 |
 * | 对象号 | `cost_object_no`（列表 join 拿到的，认这个不认 uuid） | 同上 |
 * | 费率取数来源 | `rate_scope` / `rate_scope_label` | 同上（**只有建单回单有值**，列表恒 `null`） |
 *
 * # 三条口径
 *
 * 1. **金额一律服务端算**（`round(hours × rate, 4)`）。页面上的"预计金额"只做提示，
 *    不作为入参提交（`amount` 只是可选的核对值，给了就必须对上，否则 `400 + 66058`）；
 * 2. **工资率留空 = 后端自动取数**（对象 > 车间 > 公司，B2-3）；三级全空 ⇒ `422 + 66059`，
 *    后端**不会**按 0 计 —— 页面必须把这条说清楚，否则用户以为是页面卡了；
 * 3. **只有 `DRAFT` 能改 / 删 / 确认前可改**；`CONFIRMED` 是归集与结算的依据，只读。
 */

// ==================== 词表 ====================

/** 状态值域（`ck_biz_cost_labor_status`） */
export const COST_LABOR_STATUSES = ['DRAFT', 'CONFIRMED'] as const;

/** 状态 */
export type CostLaborStatus = (typeof COST_LABOR_STATUSES)[number];

/** 状态中文（页面只认这一份译名） */
export const COST_LABOR_STATUS_LABELS: Record<CostLaborStatus, string> = {
  DRAFT: '草稿',
  CONFIRMED: '已确认',
};

/** 来源值域（`ck_biz_cost_labor_src`） */
export const COST_LABOR_SOURCES = ['MANUAL', 'IMPORT', 'MES'] as const;

/** 来源 */
export type CostLaborSource = (typeof COST_LABOR_SOURCES)[number];

/** 来源中文 */
export const COST_LABOR_SOURCE_LABELS: Record<CostLaborSource, string> = {
  MANUAL: '手工录入',
  IMPORT: 'Excel 导入',
  MES: 'MES 报工',
};

/** 列精度（与后端 `HOURS_SCALE` / `RATE_SCALE` / `AMOUNT_SCALE` 一致） */
export const HOURS_SCALE = 2;
/** 费率精度（`rate NUMERIC(18,6)`） */
export const RATE_SCALE = 6;
/** 金额精度（`amount NUMERIC(18,4)`，CHECK 里的 `round(…, 4)`） */
export const AMOUNT_SCALE = 4;

/** 金额与「工时×费率」不符（`error_code::ERP_COST_LABOR_AMOUNT_MISMATCH`） */
export const COST_LABOR_AMOUNT_CODE = 66058;
/** 三级费率全空（`error_code::ERP_COST_RATE_MISSING`） */
export const COST_RATE_MISSING_CODE = 66059;

/** 权限码（V0378 种子逐一对应；`confirm` 复用 `update`，设计与后端一致） */
export const COST_LABOR_PERMISSIONS = {
  list: 'business:erp:cost-labor:list',
  create: 'business:erp:cost-labor:create',
  update: 'business:erp:cost-labor:update',
  delete: 'business:erp:cost-labor:delete',
  import: 'business:erp:cost-labor:import',
} as const;

/** 列表每页条数（后端默认 20、上限 200） */
export const PAGE_SIZE = 20;
/** 下拉候选上限（一次拉够用） */
export const OPTION_LIMIT = 200;

// ==================== ① 列表 ====================

/** 列表行（逐字抄 `CostLaborListRes`） */
export interface CostLaborRow {
  /** 主键 */
  id: string;
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 工作日期 `YYYY-MM-DD` */
  work_date: string;
  /** 成本对象 id */
  cost_object_id: string;
  /** 对象号 `CB-yyyyMMdd-NNNNNN`（join；对象已删时为空） */
  cost_object_no: string | null;
  /** 成本对象状态（`OPEN / CLOSED / CANCELLED`） */
  cost_object_status: string | null;
  /** 产出品 id */
  product_id: string | null;
  /** 产出品名称 */
  product_name: string | null;
  /** 员工 id（按班组计时为空） */
  employee_id: string | null;
  /** 员工姓名（**姓名快照**，审计口径） */
  employee_name: string;
  /** 工作中心 id */
  dept_id: string;
  /** 工作中心名（`sys_dept` 实时名优先，落空回落快照） */
  dept_name: string | null;
  /** 人工工时（字符串） */
  hours: string;
  /** 机器工时（字符串） */
  machine_hours: string;
  /** 工资率（字符串） */
  rate: string;
  /** 费率取数来源（**只有建单回单有值**，列表恒 `null`） */
  rate_scope: string | null;
  /** 取数来源人话（同上） */
  rate_scope_label: string | null;
  /** 金额（字符串；= round(工时×费率, 4)） */
  amount: string;
  /** 来源类型 */
  source_type: string;
  /** 状态 */
  status: string;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/** 列表筛选（`CostLaborPageQuery`） */
export interface CostLaborPageQuery {
  /** 会计期间 `yyyy-MM` */
  period_code?: string;
  /** 成本对象 id */
  cost_object_id?: string;
  /** 工作中心 id */
  dept_id?: string;
  /** 员工 id */
  employee_id?: string;
  /** 状态 */
  status?: CostLaborStatus | '';
  /** 关键字：员工姓名 / 对象号 / 产出品名称（模糊） */
  keyword?: string;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

// ==================== ② 新建 / ③ 编辑 ====================

/**
 * 新建工时（`CostLaborCreateReq`）。
 *
 * 必填（后端 `spec_from_create` 会 400）：`period_code` / `work_date`（必须落在期间内）/
 * `cost_object_id`（且必须是 `OPEN`）/ `employee_name` / `dept_id` / `hours`（> 0）。
 *
 * `rate` 留空 ⇒ 后端按「成本对象 > 车间 > 公司」自动取数；给错了数字仍按显式费率走校验。
 */
export interface CostLaborCreatePayload {
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 工作日期 `YYYY-MM-DD` */
  work_date: string;
  /** 成本对象 */
  cost_object_id: string;
  /** 员工（可空 = 按班组计） */
  employee_id?: string | null;
  /** 员工姓名（必填，姓名快照） */
  employee_name: string;
  /** 工作中心 */
  dept_id: string;
  /** 人工工时（> 0，≤ 2 位小数；字符串原样传，后端 Decimal 收） */
  hours: string;
  /** 机器工时（可空，缺省 0，≤ 2 位小数） */
  machine_hours?: string;
  /** 工资率（留空 = 自动取数） */
  rate?: string | null;
  /** 金额核对值（可选；给了必须等于 round(工时×费率,4)） */
  amount?: string | null;
  /** 备注 */
  remark?: string | null;
}

/**
 * 编辑工时（`CostLaborUpdateReq`）：**部分更新** —— 缺失 = 不改。
 *
 * ⚠️ 编辑**不会**自动重新取费率（后端口径：留空 = 沿用当前快照）。换车间后想按新车间口径算，
 * 必须显式给 `rate`。页面在车间变化时会提示这一点。
 */
export type CostLaborUpdatePayload = Partial<CostLaborCreatePayload>;

// ==================== ④ 导入 ====================

/** 逐行错误（`ImportIssue`：行号是 **Excel 真实行号**，表头是第 1 行） */
export interface ImportIssue {
  /** Excel 行号（第 2 行起是数据） */
  line: number;
  /** 出错列（列名与模板表头逐字一致） */
  column: string;
  /** 原因（含"怎么修"） */
  message: string;
}

/** 导入回执（`CostLaborImportRes`：**全有或全无**） */
export interface CostLaborImportResult {
  /** 成功入库行数（要么 0，要么等于文件数据行数） */
  success: number;
  /** 有错的行数（按行去重） */
  failed: number;
  /** 逐行错误（成功时为空） */
  rows: ImportIssue[];
}
