/**
 * 安全库存水位设置 API 层（后端 `backend/app/src/business/erp/stock_policy/`，
 * M2-13 单格 upsert / M2-17 批量设置 + 导入；M2-17 页面消费这三个面）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `inventory-aging.ts` / `inventory-transit.ts` 同一口径：水位是独立后端模块
 * （自己的 `dto/model/handler` + 迁移 V0428/V0429/V0431），按模块边界单独成文件。
 *
 * # 四个面（字段名逐字抄后端，不是猜的）
 *
 * | 语义 | 路径 | 权限码 | 出参 |
 * |------|------|--------|------|
 * | 列表（物料 × 仓库 一格一行） | `GET …/stock-policy/page` | `…:stock-policy:list` | `PageData<StockPolicyRow>` |
 * | 设置一格（upsert） | `PUT …/stock-policy` | `…:stock-policy:save` | `StockPolicyRow` |
 * | 批量设置（笛卡尔积） | `PUT …/stock-policy/batch` | `…:stock-policy:save` | `StockPolicyBatchResult` |
 * | 删除一格（软删） | `DELETE …/stock-policy/{id}` | `…:stock-policy:delete` | `void` |
 *
 * 另有两条**不在本文件**：
 *
 * - 水位检查（`POST …/stock-policy/check`，M2-14）—— 它是「跑一次巡检」，页面不挂它的按钮；
 *   入口在定时任务页 / 运维脚本；
 * - Excel 导入 —— 走通用导入向导（`biz_type=business.erp.stock-policy`），
 *   由 `components/ImportEntryButton.vue` 负责，本文件不重复实现上传。
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **数量是 `Decimal`，出参可能是字符串**（`"100.0000"`）。一律过
 *    `stock-policy-logic.ts::formatQty` / `toNumber`，别直接 `+`；
 * 3. **`status` 是数字 `1` / `0`**（不是字符串）：后端字段是 `i16`，传 `"1"` 会被
 *    serde 拒成 400。筛选参数才是字符串（`?status=1`）。
 */
import { del, get, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 一格水位设置（逐字抄 `PolicyRes`；名字由后端 LEFT JOIN 主数据实时取，软删后为 `null`） */
export interface StockPolicyRow {
  /** 设置行 id（编辑 / 删除都用它） */
  id: string;
  /** 物料 id */
  material_id: string;
  /** 物料编码（物料软删后 `null`） */
  material_code: string | null;
  /** 物料名称（物料软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 最低水位（= 该格的安全库存阈值；`0` = 本格明示不预警） */
  min_qty: number | string;
  /** 最高水位（`null` = 未设 = 不判超储） */
  max_qty: number | string | null;
  /** 状态：`1` 启用 / `0` 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 列表入参（逐字抄 `PolicyFilter`；四个条件都可留空） */
export interface StockPolicyQuery {
  /** 物料 id（精确 UUID） */
  material_id?: string;
  /** 仓库 id（精确 UUID） */
  warehouse_id?: string;
  /** 状态筛选：`'1'` / `'0'`（**字符串**，非法值后端 400） */
  status?: string;
  /** 关键字（物料编码 / 名称 / 仓库编码 / 名称） */
  keyword?: string;
  /** 页码（从 1 起） */
  page?: number;
  /** 每页条数（0 → 默认 20，上限 200） */
  page_size?: number;
}

/** 设置一格（逐字抄 `SavePolicyReq`；新建与编辑是同一个动作） */
export interface StockPolicySavePayload {
  /** 物料 id（**键**，编辑时不可换格） */
  material_id: string;
  /** 仓库 id（**键**） */
  warehouse_id: string;
  /** 最低水位（`>= 0`） */
  min_qty: number | string;
  /** 最高水位（`null` / 不传 = 未设；设了必须 > `min_qty`） */
  max_qty?: number | string | null;
  /** 状态（`1` 启用 / `0` 停用；不传 = 启用） */
  status?: number;
  /** 备注 */
  remark?: string | null;
}

/** 批量设置（逐字抄 `BatchSavePolicyReq`） */
export interface StockPolicyBatchPayload {
  /** 物料 id 列表（至少 1 个） */
  material_ids: string[];
  /** 仓库 id 列表（至少 1 个） */
  warehouse_ids: string[];
  /** 最低水位 */
  min_qty: number | string;
  /** 最高水位（`null` / 不传 = 未设） */
  max_qty?: number | string | null;
  /** 状态 */
  status?: number;
  /** 备注（整批一份） */
  remark?: string | null;
}

/** 批量设置回执（逐字抄 `BatchSavePolicyRes`） */
export interface StockPolicyBatchResult {
  /** 本次铺开的格数（去重后） */
  cells: number;
  /** 其中新建的行数 */
  created: number;
  /** 其中覆盖的行数 */
  updated: number;
  /** 入参里被去掉的重复物料数 */
  duplicate_materials: number;
  /** 入参里被去掉的重复仓库数 */
  duplicate_warehouses: number;
  /** 一句给人看的结论（直接弹 Message 用） */
  message: string;
}

/** 列表（`GET …/stock-policy/page`，权限 `business:erp:stock-policy:list`） */
export function fetchStockPolicyPage(query: StockPolicyQuery): Promise<PageData<StockPolicyRow>> {
  return get<PageData<StockPolicyRow>>(`${BASE}/stock-policy/page`, asParams(query));
}

/** 设置一格（`PUT …/stock-policy`，权限 `business:erp:stock-policy:save`） */
export function saveStockPolicy(payload: StockPolicySavePayload): Promise<StockPolicyRow> {
  return put<StockPolicyRow>(`${BASE}/stock-policy`, payload);
}

/** 批量设置（`PUT …/stock-policy/batch`，权限 `business:erp:stock-policy:save`） */
export function saveStockPolicyBatch(payload: StockPolicyBatchPayload): Promise<StockPolicyBatchResult> {
  return put<StockPolicyBatchResult>(`${BASE}/stock-policy/batch`, payload);
}

/** 删除一格（`DELETE …/stock-policy/{id}`，权限 `business:erp:stock-policy:delete`） */
export function deleteStockPolicy(id: string): Promise<void> {
  return del<void>(`${BASE}/stock-policy/${id}`);
}

/** 丢掉空串 / null / undefined（后端把 `?material_id=` 当非法 UUID → 400，必须根本不发） */
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
