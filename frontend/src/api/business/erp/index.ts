/**
 * ERP 仓储域前端 API 层（W4a-1）。
 *
 * **本文件是唯一契约来源**：`warehouse-api.ts` 已降级为 `export * from './index'` 的兼容壳，
 * 页面 / 纯逻辑 / 弹窗一律 import '@/api/business/erp'。
 *
 * # 字段名为什么不叫 `code` / `name` / `path`
 *
 * 卡面草稿写的是 `code / name / path`，那是**猜的**。真口径逐条抄自
 * `backend/src/business/erp/dto.rs`（`WarehouseRes` / `WarehouseTreeNode` / `LocationRes`）
 * 与 `backend/migrations/business/V0137__erp_warehouse_location.up.sql`（`biz_warehouse` 列名）：
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 编码 | `warehouse_code` | `dto.rs` `WarehouseRes::warehouse_code` |
 * | 名称 | `warehouse_name` | `dto.rs` `WarehouseRes::warehouse_name` |
 * | 物化路径 | `warehouse_path` | `dto.rs` `WarehouseRes::warehouse_path`（`/uuid/uuid/`） |
 * | 上级 | `parent_id` | `dto.rs` `WarehouseRes::parent_id` |
 * | 层级 | `level` | `dto.rs` `WarehouseRes::level`（1 总仓 / 2 分仓 / 3 库房） |
 * | 叶子 | `is_leaf` | 派生只读：建子仓库置 0、删完下级复位 1 |
 * | 树节点 | `WarehouseRes` 平铺 + `children` | `handler.rs` `WarehouseTreeNode` 用 `#[serde(flatten)]` |
 *
 * 猜字段名的代价是运行时 `undefined`（树渲染成空行、表单回填空白），编译期抓不到 ——
 * 所以这一层宁可长，也不缩写。
 *
 * # 路径
 *
 * 后端注解不含 `/api`，前端写全路径：`/api/business/erp/...`
 * （`handler.rs`：`/warehouse/page`、`/warehouse/tree`、`/warehouse/{id}`、`/location/list`、`/location/page`）。
 *
 * # 入参口径
 *
 * - 空串 / null / undefined 一律**不发**（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行）；
 * - `GET /warehouse/tree?root_id=` 可选，不传返回全部顶级仓库及下级；
 * - `GET /location/list?warehouse_id=&include_disabled=` 中 `warehouse_id` 必填；
 * - `GET /warehouse/page` 的 list 元素是 `WarehouseRes`（**不是** `WarehouseCard`，没有 `child_count`）；
 * - 枚举一律大写字面量：`warehouse_type` ∈ `NORMAL|RAW|FINISHED|SCRAP|IN_TRANSIT`。
 */

import { del, get, patch, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 仓库类型（与后端 `dto::WAREHOUSE_TYPES` 逐字一致） */
export type WarehouseType = 'NORMAL' | 'RAW' | 'FINISHED' | 'SCRAP' | 'IN_TRANSIT';

/** 库位类型（与后端 `dto::LOCATION_TYPES` 逐字一致） */
export type LocationType = 'STORAGE' | 'PICKING' | 'RECEIVING' | 'SHIPPING' | 'QUARANTINE' | 'SCRAP';

/** 仓库（详情 / 树节点本体的形状，对应后端 `WarehouseRes`） */
export interface Warehouse {
  /** 主键 */
  id: string;
  /** 编码（未删除内唯一） */
  warehouse_code: string;
  /** 名称 */
  warehouse_name: string;
  /** 类型 */
  warehouse_type: WarehouseType;
  /** 负责人 */
  manager: string | null;
  /** 是否允许负库存 */
  allow_negative: boolean;
  /** 地址 */
  address: string | null;
  /** 启用状态：1 启用 / 0 停用 */
  status: number;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string | null;
  /** 上级仓库（`null` = 顶级） */
  parent_id: string | null;
  /** 层级（1 总仓 / 2 分仓 / 3 库房） */
  level: number;
  /** 物化路径 `/uuid/uuid/`，后端算，前端只读 */
  warehouse_path: string;
  /** 是否叶子（派生：有无下级仓库） */
  is_leaf: boolean;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/** 仓库树节点（`WarehouseRes` + 递归 children） */
export interface WarehouseTreeNode extends Warehouse {
  /** 下级仓库（后端已按 sort_order / 路径排好） */
  children: WarehouseTreeNode[];
}

/**
 * 卡面里叫 `WarehouseNode` 的就是 `WarehouseTreeNode`（同一形状，别名只为对齐卡面命名）。
 *
 * 注意字段名：**不是** `code/name/path`，而是 `warehouse_code/warehouse_name/warehouse_path`
 * —— 见模块头注释里的对照表。
 */
export type WarehouseNode = WarehouseTreeNode;

/** 新建仓库请求体（对应后端 `CreateWarehouseReq`） */
export interface CreateWarehouseReq {
  warehouse_code: string;
  warehouse_name: string;
  warehouse_type: WarehouseType;
  manager?: string | null;
  address?: string | null;
  status?: number;
  sort_order?: number;
  remark?: string | null;
  parent_id?: string | null;
  level: number;
}

/**
 * 编辑仓库请求体（对应后端 `UpdateWarehouseReq`）。
 *
 * 可空字段用 `null` 表示「清空」，缺席表示「不改」—— 页面只在用户真改过时才带字段。
 */
export interface UpdateWarehouseReq {
  warehouse_name?: string;
  warehouse_type?: WarehouseType;
  manager?: string | null;
  address?: string | null;
  status?: number;
  sort_order?: number;
  remark?: string | null;
}

/** 库位（对应后端 `LocationRes`） */
export interface Location {
  /** 主键 */
  id: string;
  /** 库位编码（同一仓库内唯一） */
  location_code: string;
  /** 名称 */
  location_name: string | null;
  /** 所属仓库 */
  warehouse_id: string;
  /** 类型 */
  location_type: LocationType;
  /** 是否仓库默认库位 */
  is_default: boolean;
  /** 容量 */
  capacity: string | number | null;
  /** 启用状态 */
  status: number;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 新建库位请求体（对应后端 `CreateLocationReq`） */
export interface CreateLocationReq {
  location_code: string;
  warehouse_id: string;
  location_name?: string | null;
  location_type: LocationType;
  is_default?: boolean;
  capacity?: string | number | null;
  status?: number;
  sort_order?: number;
  remark?: string | null;
}

/** 编辑库位请求体（**不允许换仓库**，后端 `UpdateLocationReq` 没有 `warehouse_id`） */
export interface UpdateLocationReq {
  location_name?: string | null;
  location_type?: LocationType;
  is_default?: boolean;
  capacity?: string | number | null;
  status?: number;
  sort_order?: number;
  remark?: string | null;
}

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，必须根本不发） */
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

// ==================== 仓库 ====================

/**
 * 仓库树（物化路径前缀查询）。
 *
 * @param rootId 子树根；不传则返回全部顶级仓库及下级
 */
export function fetchWarehouseTree(rootId?: string): Promise<WarehouseTreeNode[]> {
  return get<WarehouseTreeNode[]>(`${BASE}/warehouse/tree`, asParams({ root_id: rootId }));
}

/**
 * 仓库分页（编码 / 名称关键词 + 类型 / 层级 / 上级 / 启用位）。
 *
 * ⚠️ W4b-2 修正：这里原来写的是 `PageData<Warehouse[]>`（list 变成二维数组），
 * 与真接口出参 `{ list: [ {...仓库...} ], total, page, page_size }` 不符 ——
 * `data.list.map(...)` 在类型上就编不过，运行时倒是能跑（TS 只是标注）。
 * 序列号页要用它拉仓库下拉，顺手按真出参改正（本函数此前无其它调用点）。
 */
export function fetchWarehousePage(query: {
  keyword?: string;
  warehouse_type?: string;
  level?: number;
  parent_id?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Warehouse>> {
  return get<PageData<Warehouse>>(`${BASE}/warehouse/page`, asParams(query));
}

/** 仓库详情 */
export function fetchWarehouseDetail(id: string): Promise<Warehouse> {
  return get<Warehouse>(`${BASE}/warehouse/${id}`);
}

/** 新建仓库（`level` 必须与上级层级自洽，见模块头注释） */
export function createWarehouse(payload: CreateWarehouseReq): Promise<Warehouse> {
  return post<Warehouse>(`${BASE}/warehouse`, payload);
}

/** 编辑仓库（缺席 = 不改，`null` = 清空） */
export function updateWarehouse(id: string, payload: UpdateWarehouseReq): Promise<Warehouse> {
  return put<Warehouse>(`${BASE}/warehouse/${id}`, payload);
}

/** 删除仓库（软删；有下级或已被库位引用时后端 409） */
export function deleteWarehouse(id: string): Promise<void> {
  return del<void>(`${BASE}/warehouse/${id}`);
}

// ==================== 库位 ====================

/**
 * 按仓库取库位（本页库位列表的数据源）。
 *
 * @param warehouseId 所属仓库（必填，后端 `warehouse_id` 非法 UUID 会 400+66006）
 * @param includeDisabled 是否连停用库位一起返回（编辑回填用；列表默认只要启用的）
 */
export function fetchLocationList(warehouseId: string, includeDisabled = true): Promise<Location[]> {
  return get<Location[]>(
    `${BASE}/location/list`,
    asParams({ warehouse_id: warehouseId, include_disabled: includeDisabled }),
  );
}

/** 库位分页（关键词 / 仓库 / 类型 / 启用位） */
export function fetchLocationPage(query: {
  keyword?: string;
  warehouse_id?: string;
  location_type?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Location[]>> {
  return get<PageData<Location[]>>(`${BASE}/location/page`, asParams(query));
}

/** 库位详情 */
export function fetchLocationDetail(id: string): Promise<Location> {
  return get<Location>(`${BASE}/location/${id}`);
}

/**
 * 新建库位。
 *
 * **只能挂在叶子仓库下**：后端 `location_create` 对 `is_leaf = 0` 的仓库直接
 * `400 + 66005`（`ERP_WAREHOUSE_NOT_LEAF`），不是只靠前端拦。
 */
export function createLocation(payload: CreateLocationReq): Promise<Location> {
  return post<Location>(`${BASE}/location`, payload);
}

/** 编辑库位（缺席 = 不改，`null` = 清空） */
export function updateLocation(id: string, payload: UpdateLocationReq): Promise<Location> {
  return put<Location>(`${BASE}/location/${id}`, payload);
}

/** 删除库位（软删） */
export function deleteLocation(id: string): Promise<void> {
  return del<void>(`${BASE}/location/${id}`);
}

// ==================== 物料 ====================

/**
 * 物料类型（与后端 `dto::MATERIAL_TYPES` 逐字一致，`ck_biz_material_type` 兜底）。
 *
 * 卡面草图写的是「分类」（`category_id` / `category_name`）—— 那是**猜的**：
 * `dto.rs::MaterialRes` 里**没有**这两个字段，`biz_material` 表也没有 `category_id` 列；
 * 后端只种了 `business:erp:category:*` 权限码，`handler.rs` 里**没有** `/erp/category` 路由
 * （`grep -n "erp/category" handler.rs` 零命中）。物料真正分的类就是 `material_type` 四值，
 * 所以页面上的「分类」列 / 下拉 = `material_type`。
 */
export type MaterialType = 'RAW' | 'SEMI' | 'FINISHED' | 'SERVICE';

/**
 * 物料（对应后端 `dto.rs::MaterialRes`，字段名逐条抄自 serde 出参）。
 *
 * | 卡面草稿 | 真字段 | 说明 |
 * |---|---|---|
 * | `category_id` / `category_name` | —— | 不存在，见 `MaterialType` 注释 |
 * | `unit_name` | `unit_id` | 出参只有 id；名称由 `/erp/unit/page` 本地映射 |
 * | `is_active` | `status` | `i16`：1 启用 / 0 停用（**不是**布尔） |
 * | `external_sn` | —— | 列在表里（V0138）但 `MaterialRes` 没吐，前端拿不到 |
 *
 * `safety_stock` / `tax_rate` 是 `Decimal`，serde 出参是**字符串**（`"10.0000"`），
 * 回写时原样字符串也能被后端 `Decimal` 反序列化。
 */
export interface MaterialRow {
  /** 主键 */
  id: string;
  /** 物料编码（未删除内唯一，`VARCHAR(40)`） */
  material_code: string;
  /** 物料名称（`VARCHAR(120)`） */
  material_name: string;
  /** 规格型号 */
  spec: string | null;
  /** 基础计量单位 id */
  unit_id: string;
  /** 物料类型（= 页面上的「分类」） */
  material_type: MaterialType;
  /** 默认仓库 */
  default_warehouse_id: string | null;
  /** 安全库存 */
  safety_stock: string | number;
  /** 计价方式 */
  costing_method: string;
  /** 存货科目 */
  inventory_account_id: string | null;
  /** 税率（0–1） */
  tax_rate: string | number;
  /** 启用状态：1 启用 / 0 停用 */
  status: number;
  /** ★ 是否一物一码（V0138 的列；W2b 补进接口的开关） */
  serial_required: boolean;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/** 新建物料请求体（对应后端 `CreateMaterialReq`） */
export interface CreateMaterialReq {
  /** 编码（未删除内唯一） */
  material_code: string;
  /** 名称 */
  material_name: string;
  /** 基础单位（必填 UUID） */
  unit_id: string;
  /** 类型（缺省 RAW） */
  material_type?: MaterialType;
  /** 规格 */
  spec?: string | null;
  /** 默认仓库 */
  default_warehouse_id?: string | null;
  /** 安全库存（缺省 0） */
  safety_stock?: string | number | null;
  /** 计价方式（缺省 MOVING_AVG） */
  costing_method?: string;
  /** 存货科目 */
  inventory_account_id?: string | null;
  /** 税率（缺省 0） */
  tax_rate?: string | number | null;
  /** 启用状态（缺省 1） */
  status?: number;
  /** ★ 是否一物一码（缺省 false） */
  serial_required?: boolean;
  /** 备注 */
  remark?: string | null;
}

/**
 * 编辑物料请求体（对应后端 `UpdateMaterialReq`）。
 *
 * **没有 `material_code`** —— 编码是身份，后端不收。缺席 = 不改，`null` = 清空。
 */
export interface UpdateMaterialReq {
  /** 名称 */
  material_name?: string;
  /** 规格（`null` = 清空） */
  spec?: string | null;
  /** 基础单位 */
  unit_id?: string;
  /** 类型 */
  material_type?: MaterialType;
  /** 默认仓库（`null` = 清空） */
  default_warehouse_id?: string | null;
  /** 安全库存 */
  safety_stock?: string | number;
  /** 计价方式 */
  costing_method?: string;
  /** 存货科目（`null` = 清空） */
  inventory_account_id?: string | null;
  /** 税率 */
  tax_rate?: string | number;
  /** 启用状态 */
  status?: number;
  /** ★ 一物一码（缺席 = 不改；页面编辑态置灰，故正常不发） */
  serial_required?: boolean;
  /** 备注（`null` = 清空） */
  remark?: string | null;
}

/** 计量单位（对应后端 `UnitRes`；本页只用来把 `unit_id` 译成名字） */
export interface UnitRow {
  /** 主键 */
  id: string;
  /** 单位编码（PCS / KG / BOX） */
  unit_code: string;
  /** 单位名称（个 / 千克 / 箱） */
  unit_name: string;
  /** 是否基础单位 */
  is_base: boolean;
  /** 启用状态 */
  status: number;
}

/**
 * 物料分页（服务端分页，`GET /erp/material/page`）。
 *
 * `serial_required` 是**三态**：`true` / `false` 过滤，`undefined` 不带该参数（= 全部）。
 * 空串一律不发（`asParams` 兜住）—— 后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行。
 */
export function fetchMaterialPage(query: {
  keyword?: string;
  material_type?: MaterialType | '';
  serial_required?: boolean;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<MaterialRow>> {
  return get<PageData<MaterialRow>>(`${BASE}/material/page`, asParams(query));
}

/** 物料详情 */
export function fetchMaterialDetail(id: string): Promise<MaterialRow> {
  return get<MaterialRow>(`${BASE}/material/${id}`);
}

/**
 * 新建物料。
 *
 * 后端返回的是整个 `MaterialRes`（不是只有 id），所以这里返回 `MaterialRow` ——
 * 卡面写 `Promise<{ id: string }>` 是保守估计，`row.id` 就是那个 id。
 */
export function createMaterial(payload: CreateMaterialReq): Promise<MaterialRow> {
  return post<MaterialRow>(`${BASE}/material`, payload);
}

/** 编辑物料（缺席 = 不改，`null` = 清空） */
export function updateMaterial(id: string, payload: UpdateMaterialReq): Promise<MaterialRow> {
  return put<MaterialRow>(`${BASE}/material/${id}`, payload);
}

/**
 * 计量单位列表（`GET /erp/unit/page`，取前 200 条够用：种子只有 3 个）。
 *
 * 物料出参只给 `unit_id`，列表的「单位」列与弹窗的单位下拉都得靠它译名。
 * 权限码 `business:erp:unit:list`（V0141 已种）。
 */
export function fetchUnitOptions(): Promise<UnitRow[]> {
  return get<PageData<UnitRow>>(`${BASE}/unit/page`, asParams({ page: 1, page_size: 200 })).then(
    (page) => page.list,
  );
}

// ==================== 序列号（W4b-2 / 后端 W2b） ====================

/**
 * 序列号状态（与 `serial_model::ALL_STATUSES` 逐字一致，`V0138` 的 `ck_biz_material_serial_status` 兜底）。
 *
 * 状态机（`serial_model::can_transition`，文档注释里就是这张表）：
 *
 * | 从 \ 到 | `OUTBOUND` | `SCRAPPED` |
 * |---|---|---|
 * | `IN_STOCK` | ✅ | ✅ |
 * | `OUTBOUND` | — | ✅ |
 * | `SCRAPPED` | ❌ | ❌ |
 *
 * `IN_STOCK` **不是**可写的目标状态：新增只能走批量生成（`WRITABLE_TARGETS` 只有两个）。
 */
export type SerialStatus = 'IN_STOCK' | 'OUTBOUND' | 'SCRAPPED';

/** 来源单据类型（与 `serial_model::SOURCE_TYPES` 逐字一致，写错的类型会让「按来源追溯」查不到东西） */
export type SerialSourceType = 'PURCHASE' | 'TRANSFER' | 'PRODUCE' | 'RETURN';

/**
 * 序列号列表 / 详情行（对应后端 `serial_dto::SerialRes`，字段名逐条抄 serde 出参）。
 *
 * | 卡面草图 | 真字段 | 说明 |
 * |---|---|---|
 * | 物料名 / 仓库名 | —— | **列表行里没有**：只有 `material_id` / `warehouse_id`，名称由页面本地译 |
 * | 库位 | `location_id` | 指向 `biz_inventory_location.id`（**不是** `biz_location.id`） |
 * | 入库 | `inbound_at` | 首次入库时间；转 OUTBOUND / SCRAPPED 都写 `outbound_at` |
 *
 * 名称只在**扫码**出参 `SerialScanRes` 里有（`material_name` / `warehouse_name` / `location_name`）。
 */
export interface SerialRow {
  /** 主键 */
  id: string;
  /** 系统内码 `SN-yyyyMMdd-XXXXXX`（未删除内全局唯一） */
  serial_no: string;
  /** 物料 id */
  material_id: string;
  /** 当前 / 最后所在仓库 id */
  warehouse_id: string | null;
  /** 当前所在库位库存行 id（`biz_inventory_location.id`） */
  location_id: string | null;
  /** 批次号 */
  batch_no: string | null;
  /** 状态 */
  status: SerialStatus;
  /** 首次入库时间（RFC3339） */
  inbound_at: string | null;
  /** 出库 / 报废时间（RFC3339） */
  outbound_at: string | null;
  /** 来源单据类型 */
  source_type: SerialSourceType | null;
  /** 来源单据 id */
  source_id: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 扫码出参（对应后端 `serial_dto::SerialScanRes`）。
 *
 * 比 `SerialRow` 多四个**译名**字段（`material_code` / `material_name` / `warehouse_name` /
 * `location_name`），少 `source_type` / `source_id` / `created_at` / `updated_at` ——
 * 卡片上要显示「物料 成品D」「仓库 分仓」就靠它，不要拿 `SerialRow` 自己拼。
 */
export interface SerialScanRow {
  /** 主键 */
  id: string;
  /** 序列号 */
  serial_no: string;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 仓库 id */
  warehouse_id: string | null;
  /** 仓库名称 */
  warehouse_name: string | null;
  /** 库位库存行 id */
  location_id: string | null;
  /** 库位名称 */
  location_name: string | null;
  /** 批次号 */
  batch_no: string | null;
  /** 状态 */
  status: SerialStatus;
  /** 入库时间 */
  inbound_at: string | null;
  /** 出库 / 报废时间 */
  outbound_at: string | null;
}

/** 批量生成请求体（对应后端 `serial_dto::BatchGenerateReq`） */
export interface BatchGenerateSerialReq {
  /** 物料 id（**必须** `serial_required = TRUE`，否则 400） */
  material_id: string;
  /**
   * 仓库 id。
   *
   * 后端 W2c-1 的不变式①：一物一码物料的账面数量按「物料×仓库」由序列号派生，
   * 所以这里**实际必填** —— 不传直接 `400 + 66005`（文案：「必须指定 warehouse_id」）。
   */
  warehouse_id: string;
  /** 库位库存行 id（可空；必须是同仓库的行） */
  location_id?: string | null;
  /** 批次号（可空，≤64） */
  batch_no?: string | null;
  /** 生成条数（1..=1000，`serial_model::MAX_BATCH_COUNT`） */
  count: number;
  /** 来源类型 */
  source_type?: SerialSourceType | null;
  /** 来源单据 id（可空） */
  source_id?: string | null;
}

/**
 * 批量生成出参（对应 `SerialBatchRes`）。
 *
 * `serial_nos` 给扫码枪 / 贴标机直接用；`list` 是回读的整行（带 `id`，列表才点得动状态流转）。
 */
export interface SerialBatchRes {
  /** 实际生成条数 */
  count: number;
  /** 本次号段所属周期键（`yyyyMMdd`，北京时间） */
  period_key: string;
  /** 号列表（升序，可直接打印） */
  serial_nos: string[];
  /** 回读的整行（与 `serial_nos` 一一对应） */
  list: SerialRow[];
}

/**
 * 序列号分页（`GET /erp/serial/page`，服务端分页）。
 *
 * 参数照 `serial_dto::SerialFilter`：`material_id` / `warehouse_id` / `location_id` /
 * `status` / `serial_no`（模糊）/ `page` / `page_size`。
 * 空串一律不发（`asParams` 兜住）—— 后端把 `?serial_no=` 当「等于空串」过滤，会查出 0 行。
 */
export function fetchSerialPage(query: {
  material_id?: string;
  warehouse_id?: string;
  location_id?: string;
  status?: SerialStatus | '';
  serial_no?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<SerialRow>> {
  return get<PageData<SerialRow>>(`${BASE}/serial/page`, asParams(query));
}

/**
 * 扫码定位（`GET /erp/serial/scan?serial_no=`，走 `uk_biz_material_serial_no` 唯一索引点查）。
 *
 * 查不到时后端返回 **HTTP 404 + `code = 10002`**（`10002` 是通用「资源不存在」），
 * 不是 `code = 0` 带空对象 —— 页面要把它译成「未找到该序列号」，不能当成普通报错弹红条。
 * 号为空则 `400`（`ERP_FILTER_INVALID`）：条码枪漏扫时前端应先自己拦一次。
 */
export function scanSerial(serialNo: string): Promise<SerialScanRow> {
  return get<SerialScanRow>(`${BASE}/serial/scan`, { serial_no: serialNo.trim() });
}

/**
 * 批量生成（`POST /erp/serial/batch-generate`，权限 `business:erp:serial:generate`）。
 *
 * 前置闸门（后端硬校验，前端只是提前拦）：
 * - 物料必须 `serial_required = TRUE`，否则 400；
 * - `warehouse_id` 必填（不变式①）；
 * - `count` ∈ 1..1000。
 */
export function batchGenerateSerials(payload: BatchGenerateSerialReq): Promise<SerialBatchRes> {
  return post<SerialBatchRes>(`${BASE}/serial/batch-generate`, payload);
}

/**
 * 状态流转（`PATCH /erp/serial/{id}/status`，**不可逆**）。
 *
 * `status` 只收 `OUTBOUND` / `SCRAPPED`；`reason` 最长 200 字，只进日志与流水备注
 * （`V0138` 的序列号表没有该列，红线⑤不许改表）。
 */
export function updateSerialStatus(
  id: string,
  payload: { status: Exclude<SerialStatus, 'IN_STOCK'>; reason?: string | null },
): Promise<SerialRow> {
  return patch<SerialRow>(`${BASE}/serial/${id}/status`, payload);
}

/** 序列号详情（`GET /erp/serial/{id}`） */
export function fetchSerialDetail(id: string): Promise<SerialRow> {
  return get<SerialRow>(`${BASE}/serial/${id}`);
}

// ==================== 调拨单（W4c / 后端 W3b） ====================

/**
 * 调拨单状态（与 `V0140__erp_stock_transfer.up.sql` 的 `ck_transfer_status` 逐字一致）。
 *
 * 状态机（`ck_biz_transfer_state` 把状态和时间戳绑死，越级流转在库里就写不进去）：
 *
 * | 从 \ 到 | `PENDING` | `APPROVED` | `IN_TRANSIT` | `DONE` | `CANCELLED` |
 * |---|---|---|---|---|---|
 * | `DRAFT` | ✅ 提交审批 | — | — | — | — |
 * | `PENDING` | — | ✅ 审批通过 | — | — | — |
 * | `APPROVED` | — | — | ✅ 发货 | — | — |
 * | `IN_TRANSIT` | — | — | — | ✅ 收货 | — |
 * | `DONE` / `CANCELLED` | 终态 | | | | |
 *
 * ⚠️ W3b 的 `dto.rs` 落地前，本段类型**以 V0140 的列名 + 本卡契约文字为准**；
 * 联调时只需要核对字段名，不需要重写页面（页面只 import 这里的类型）。
 */
export type TransferStatus =
  | 'DRAFT'
  | 'PENDING'
  | 'APPROVED'
  | 'IN_TRANSIT'
  | 'DONE'
  | 'CANCELLED';

/**
 * 调拨单主表行（对应 `biz_stock_transfer` 的列；列表与详情共用）。
 *
 * 字段名逐条抄 V0140 §1 的列名（**不造 `code` / `name` 这种简称**）：
 * `transfer_no` / `from_warehouse_id` / `to_warehouse_id` / `transfer_date` / `reason` /
 * `total_qty` / `total_amount` / `submitted_at` / `approved_at` / `shipped_at` /
 * `received_at` / `cancelled_at` / `cancel_reason` / `approval_instance_id`。
 *
 * # `item_count` 是**列表页必须由后端下发**的派生字段
 *
 * 卡面列表要显示「明细数」，而主表没有这一列。两种取法：
 * ① 后端在 `TransferRes` 里 join 出 `item_count`（本卡按这个写，W3b 加一个
 *    `LEFT JOIN (SELECT transfer_id, count(*) …)` 即可）；
 * ② 前端逐行查详情 —— 一页 20 行 = 21 个请求，否决。
 * 所以字段声明成**可选**：后端没下发时页面显示 `—`，绝不假装有值。
 */
export interface TransferRow {
  /** 主键 */
  id: string;
  /** 调拨单号 `TR-yyyyMMdd-000001`（软删后可复用，见 V0140 §1 ⑧） */
  transfer_no: string;
  /** 调出仓库 id */
  from_warehouse_id: string;
  /** 调入仓库 id（**必须** ≠ `from_warehouse_id`，`ck_transfer_diff_wh` 兜底） */
  to_warehouse_id: string;
  /** 在途仓 id（`IN_TRANSIT` / `DONE` 状态必须非空，V0139 种子 `WH-TRANSIT`） */
  transit_warehouse_id: string | null;
  /** 状态 */
  status: TransferStatus;
  /** 调拨日期（`YYYY-MM-DD`，DATE 列不是时间戳） */
  transfer_date: string;
  /** 调拨原因 / 说明（≤255，V0140 取卡面的 `reason` 而非 W0 的 `transfer_reason`） */
  reason: string | null;
  /** 申请人 id（弱引用 `sys_user.id`） */
  applicant_id: string | null;
  /** 申请人姓名快照（单据留痕，不随改名变） */
  applicant_name: string | null;
  /** 审批人 id（弱引用） */
  approver_id: string | null;
  /** 审批实例 id（弱引用 `biz_flow_instance.id`，`biz_type='STOCK_TRANSFER'`） */
  approval_instance_id: string | null;
  /** 合计数量（明细 `qty` 汇总快照） */
  total_qty: number | string;
  /** 合计金额（原价转移的成本快照汇总） */
  total_amount: number | string;
  /** 提交审批时间 */
  submitted_at: string | null;
  /** 审批通过时间 */
  approved_at: string | null;
  /** 发运时间 */
  shipped_at: string | null;
  /** 签收时间 */
  received_at: string | null;
  /** 取消时间 */
  cancelled_at: string | null;
  /** 取消原因（`CANCELLED` 必填非空） */
  cancel_reason: string | null;
  /** 备注 */
  remark: string | null;
  /** 明细数（**后端派生字段**，见类型注释；没有就显示 `—`） */
  item_count?: number;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 明细的序列号（对应 `biz_stock_transfer_item_serial`，**子表不是 JSONB**）。
 *
 * `serial_no` 是后端 join `biz_material_serial` 出来的译名（卡面详情要显示「SN-…」，
 * 只给 `serial_id` 的话页面得再查一轮；W3b 若只返回 id，页面会退化成短 id + tooltip）。
 */
export interface TransferItemSerial {
  /** 子表主键 */
  id: string;
  /** 所属明细行 id */
  item_id: string;
  /** 单品序列号 id（`biz_material_serial.id`，硬外键） */
  serial_id: string;
  /** 序列号字符串（后端 join 译名；缺失时页面显示短 id） */
  serial_no?: string;
}

/**
 * 调拨明细（对应 `biz_stock_transfer_item`）。
 *
 * `qty` 恒正 —— 方向由主表的 `from_warehouse_id` / `to_warehouse_id` 表达（V0140 §2）。
 * NUMERIC 列在后端 serde 里可能出 JSON number 也可能出字符串，故类型写 `number | string`，
 * 页面统一走 `toFixedQty()` 归一。
 */
export interface TransferItem {
  /** 主键 */
  id: string;
  /** 所属调拨单 */
  transfer_id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 调出库位（`biz_inventory_location.id`，可空 = 未指定） */
  from_location_id: string | null;
  /** 调入库位（同上） */
  to_location_id: string | null;
  /** 调拨数量（恒 > 0） */
  qty: number | string;
  /** 已发运数量（部分发运预留，本批整单发运） */
  shipped_qty: number | string;
  /** 已签收数量（本批整单签收） */
  received_qty: number | string;
  /** 单位成本快照 */
  unit_cost: number | string;
  /** 金额 = qty × unit_cost */
  amount: number | string;
  /** 行备注 */
  remark: string | null;
  /** 序列号（一物一码物料必填；非一物一码为空数组） */
  serials: TransferItemSerial[];
}

/** 调拨单详情（`GET /erp/transfer/{id}`：主表 + 明细 + 每条明细的序列号） */
export interface TransferDetail extends TransferRow {
  /** 明细（含每条明细的 `serials`） */
  items: TransferItem[];
}

/** 建单 / 改单的一条明细入参（`CreateTransferItemReq`） */
export interface TransferItemReq {
  /** 物料 id */
  material_id: string;
  /** 数量（恒 > 0；`NUMERIC(18,4)`，允许 4 位小数） */
  qty: number;
  /** 调出库位（可空） */
  from_location_id?: string | null;
  /** 调入库位（可空） */
  to_location_id?: string | null;
  /** 行备注 */
  remark?: string | null;
  /**
   * 序列号 id 列表（一物一码物料**必填且条数 == qty**）。
   *
   * 注意 V0140 文件头 ③：序列号子表的唯一键是 `(item_id, serial_id)`，
   * **同一条货挂在同一张单的两条明细下 DB 拦不住** → 跨明细去重必须由前端先拦、
   * 后端再拦一次（`collectDuplicateSerialIds()`）。
   */
  serial_ids?: string[];
}

/** 新建调拨单请求体（`POST /erp/transfer`） */
export interface CreateTransferReq {
  /** 调出仓 id */
  from_warehouse_id: string;
  /** 调入仓 id（≠ `from_warehouse_id`） */
  to_warehouse_id: string;
  /** 调拨日期 `YYYY-MM-DD` */
  transfer_date: string;
  /** 原因（≤255） */
  reason?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 明细（至少 1 条） */
  items: TransferItemReq[];
}

/**
 * 改单请求体（`PUT /erp/transfer/{id}`，**仅 DRAFT**）。
 *
 * 与建单同形：改单是整单替换明细（子表逐行重写），不是增量合并。
 */
export type UpdateTransferReq = CreateTransferReq;

/**
 * 状态流转请求体（`PATCH /erp/transfer/{id}/status`）。
 *
 * - `PENDING`（提交审批）/ `DRAFT`（撤回）/ `IN_TRANSIT`（发货）/ `DONE`（收货）；
 * - `CANCELLED` 必须带 `cancel_reason`（非空，`ck_biz_transfer_state` 兜底）；
 * - 发货时 `transit_warehouse_id` 会被写进主表（`IN_TRANSIT` 要求非空）。
 */
export interface TransferStatusReq {
  /** 目标状态 */
  status: Exclude<TransferStatus, 'APPROVED'>;
  /** 取消原因（仅 `CANCELLED` 必填） */
  cancel_reason?: string | null;
  /** 在途仓 id（仅 `IN_TRANSIT` 用；不传则由后端取种子 `WH-TRANSIT`） */
  transit_warehouse_id?: string | null;
}

/**
 * 调拨单分页（`GET /erp/transfer/page`，服务端分页）。
 *
 * 参数照卡面契约：`status` / `from_warehouse_id` / `to_warehouse_id` / `transfer_no`（模糊）。
 * 空串一律不发（`asParams` 兜住）—— 后端把 `?transfer_no=` 当「等于空串」过滤会查出 0 行。
 */
export function fetchTransferPage(query: {
  status?: TransferStatus | '';
  from_warehouse_id?: string;
  to_warehouse_id?: string;
  transfer_no?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<TransferRow>> {
  return get<PageData<TransferRow>>(`${BASE}/transfer/page`, asParams(query));
}

/** 调拨单详情（`GET /erp/transfer/{id}`，含 `items[].serials`） */
export function fetchTransferDetail(id: string): Promise<TransferDetail> {
  return get<TransferDetail>(`${BASE}/transfer/${id}`);
}

/** 建单（`POST /erp/transfer`，权限 `business:erp:transfer:create`） */
export function createTransfer(payload: CreateTransferReq): Promise<TransferDetail> {
  return post<TransferDetail>(`${BASE}/transfer`, payload);
}

/** 改单（`PUT /erp/transfer/{id}`，**仅 DRAFT**，整单替换明细） */
export function updateTransfer(id: string, payload: UpdateTransferReq): Promise<TransferDetail> {
  return put<TransferDetail>(`${BASE}/transfer/${id}`, payload);
}

/**
 * 状态流转（`PATCH /erp/transfer/{id}/status`）。
 *
 * 目标状态 = `PENDING`（提交审批）→ `APPROVED`（审批引擎写，前端**不调**）→
 * `IN_TRANSIT`（发货）→ `DONE`（收货）；`DRAFT` = 撤回，`CANCELLED` = 取消（需原因）。
 */
export function updateTransferStatus(id: string, payload: TransferStatusReq): Promise<TransferRow> {
  return patch<TransferRow>(`${BASE}/transfer/${id}/status`, payload);
}

/** 删除（`DELETE /erp/transfer/{id}`，**仅 DRAFT / CANCELLED**，软删） */
export function deleteTransfer(id: string): Promise<void> {
  return del<void>(`${BASE}/transfer/${id}`);
}

/**
 * 某张调拨单的序列号列表（`GET /erp/transfer/{id}/serials`）。
 *
 * 详情接口已经带 `items[].serials`，这个接口留给「只看序列号」的导出 / 打印场景；
 * 页面主体不依赖它，但契约要落地（W3b 已定）。
 */
export function fetchTransferSerials(id: string): Promise<TransferItemSerial[]> {
  return get<TransferItemSerial[]>(`${BASE}/transfer/${id}/serials`);
}
