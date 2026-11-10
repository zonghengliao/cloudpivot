/**
 * 批次主数据契约与词表（ARCH-9 §3.1 ① / §5.2 #7–#9；后端 `lot_{dto,handler,model}.rs`）。
 *
 * # 为什么单独一个「无请求依赖」的文件
 *
 * `@/api/request` 顶层读了 `import.meta.env`（`BASE_URL` / 登录跳转路径），
 * 在 node 里 `tsx --test` 直接 import 会崩。而纯逻辑单测要 import 这些**类型与词表**，
 * 所以这里不 import 请求层；`lot.ts` 原样 `export *` 出去，页面仍然只认一个入口。
 * （与 `cost-object-kinds.ts` 同款做法。）
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 批号 | `lot_no`（物料级唯一，不是全局唯一） | `lot_dto.rs::LotListRes` |
 * | 物料 | `material_id` + `material_code` / `material_name`（实时 join） | 同上 |
 * | 来源 | `source_type` ∈ `PURCHASE/PRODUCTION/OPENING/MANUAL` | `ck_biz_lot_source` |
 * | 质检 | `qc_status` ∈ `NA/PENDING/PASSED/REJECTED` | `ck_biz_lot_qc` |
 * | 状态 | `status` 是 **数字** 1/0（DDL 是 SMALLINT） | `ck_biz_lot_status` |
 * | 日期 | `prod_date` / `expire_date` 是 `YYYY-MM-DD` 字符串 | `lot_dto.rs` |
 *
 * # 三个容易踩的点
 *
 * 1. **`status` 是数字不是布尔**：`0` = 停用（历史保留、下拉里消失），`1` = 启用；
 * 2. **`material_id` 建单后不可改**（业务键 `(material_id, lot_no)` 的一半）；
 * 3. **PUT 是部分更新 + 三态**：字段缺失 = 不改、显式 `null` = 清空（可空字段）。
 *    所以编辑只能带「真改过」的字段（见 `lot-logic.ts::toUpdatePayload`）。
 */

/** 来源类型（与 `ck_biz_lot_source` 逐字一致） */
export type LotSourceType = 'PURCHASE' | 'PRODUCTION' | 'OPENING' | 'MANUAL';

/** 质检状态（与 `ck_biz_lot_qc` 逐字一致） */
export type LotQcStatus = 'NA' | 'PENDING' | 'PASSED' | 'REJECTED';

/** 批次状态：1 启用 / 0 停用（DDL `SMALLINT`） */
export const LOT_STATUS_ENABLED = 1;
/** 批次状态：0 停用 */
export const LOT_STATUS_DISABLED = 0;

/** 批号上限（DDL `VARCHAR(64)`） */
export const LOT_NO_MAX = 64;
/** 来源单号上限（DDL `VARCHAR(32)`） */
export const LOT_SOURCE_DOC_NO_MAX = 32;
/** 每页条数（与其它 ERP 列表同口径） */
export const LOT_PAGE_SIZE = 20;
/** 批号下拉上限（后端 `lot_dto::OPTIONS_LIMIT`） */
export const LOT_OPTIONS_LIMIT = 200;

/** 批号重复的业务码（`shared::error_code::ERP_LOT_NO_DUPLICATE`） */
export const LOT_NO_DUPLICATE_CODE = 66056;

/** 来源类型下拉（中文名由前端维护一份：后端只给枚举值） */
export const SOURCE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: LotSourceType }> = [
  { label: '采购批', value: 'PURCHASE' },
  { label: '生产批', value: 'PRODUCTION' },
  { label: '期初', value: 'OPENING' },
  { label: '手工建', value: 'MANUAL' },
];

/** 质检状态下拉 */
export const QC_STATUS_OPTIONS: ReadonlyArray<{ label: string; value: LotQcStatus }> = [
  { label: '不适用', value: 'NA' },
  { label: '待检', value: 'PENDING' },
  { label: '合格', value: 'PASSED' },
  { label: '不合格', value: 'REJECTED' },
];

/** 状态下拉（`value` 是数字，与 DDL 一致） */
export const LOT_STATUS_OPTIONS: ReadonlyArray<{ label: string; value: number }> = [
  { label: '启用', value: LOT_STATUS_ENABLED },
  { label: '停用', value: LOT_STATUS_DISABLED },
];

/** 本页权限码（与 V0378 的菜单 / 按钮种子一一对应；无 delete —— 后端没这个接口） */
export const LOT_PERMISSIONS = {
  list: 'business:erp:lot:list',
  create: 'business:erp:lot:create',
  update: 'business:erp:lot:update',
} as const;

/** 列表行（`LotListRes`） */
export interface LotRow {
  /** 主键 */
  id: string;
  /** 批号 */
  lot_no: string;
  /** 物料 */
  material_id: string;
  /** 物料名称（实时 join） */
  material_name: string | null;
  /** 物料编码（实时 join） */
  material_code: string | null;
  /** 来源类型 */
  source_type: LotSourceType | string;
  /** 来源单据类型（`STOCK_IN` / `COST_PERIOD` / `COST_OBJECT` / `BACKFILL` 或 null） */
  source_doc_type: string | null;
  /** 来源单据 id */
  source_doc_id: string | null;
  /** 来源单号快照 */
  source_doc_no: string | null;
  /** 供应商（采购批） */
  supplier_id: string | null;
  /** 供应商名称（实时 join） */
  supplier_name: string | null;
  /** 生产 / 到货日期 `YYYY-MM-DD` */
  prod_date: string | null;
  /** 失效日期 `YYYY-MM-DD` */
  expire_date: string | null;
  /** 质检状态 */
  qc_status: LotQcStatus | string;
  /** 状态（1 启用 / 0 停用） */
  status: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/**
 * 下拉选项（`LotOptionRes`；`GET /business/erp/lot/options?material_id=…`）。
 *
 * 只返 **启用** 的批：停用的批不该再被领料 / 建成本对象选中。
 */
export interface LotOption {
  /** 主键 */
  id: string;
  /** 批号 */
  lot_no: string;
  /** 物料 */
  material_id: string;
  /** 物料名称 */
  material_name: string | null;
  /** 来源类型 */
  source_type: LotSourceType | string;
  /** 生产日期 */
  prod_date: string | null;
  /** 失效日期 */
  expire_date: string | null;
  /** 质检状态（前端据此把「不合格」标红） */
  qc_status: LotQcStatus | string;
}

/** 列表查询参数（`LotPageQuery`；空串一律不发，后端把 `?keyword=` 当「等于空串」） */
export interface LotPageQuery {
  /** 批号 / 物料编码 / 物料名称（模糊） */
  keyword?: string;
  /** 物料 */
  material_id?: string;
  /** 来源类型 */
  source_type?: LotSourceType | string;
  /** 质检状态 */
  qc_status?: LotQcStatus | string;
  /** 状态（1 / 0） */
  status?: number;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数 */
  page_size?: number;
}

/** 新建（`LotCreateReq`）：`material_id` 建单后不可改 */
export interface LotCreatePayload {
  /** 批号（必填） */
  lot_no: string;
  /** 物料（必填） */
  material_id: string;
  /** 来源类型（缺省 MANUAL） */
  source_type?: LotSourceType | string;
  /** 来源单据类型（可空） */
  source_doc_type?: string | null;
  /** 来源单据 id（可空） */
  source_doc_id?: string | null;
  /** 来源单号快照（可空） */
  source_doc_no?: string | null;
  /** 供应商（可空） */
  supplier_id?: string | null;
  /** 生产 / 到货日期（可空） */
  prod_date?: string | null;
  /** 失效日期（可空，≥ prod_date） */
  expire_date?: string | null;
  /** 质检状态（缺省 NA） */
  qc_status?: LotQcStatus | string;
  /** 状态（1 / 0，缺省 1） */
  status?: number;
  /** 备注（可空） */
  remark?: string | null;
}

/**
 * 编辑（`LotUpdateReq`）：**部分更新 + 三态**。
 *
 * 缺失 = 不改；显式 `null` = 清空（仅可空字段）；`material_id` 不在入参里。
 * 类型上用 `?: string | null` 表达「可以发 null」，用「键不存在」表达「不改」——
 * 所以拼报文时只能挑真改过的键（`toUpdatePayload`）。
 */
export type LotUpdatePayload = Partial<Omit<LotCreatePayload, 'material_id'>>;
