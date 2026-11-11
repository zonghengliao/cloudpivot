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

import service, { del, get, patch, post, put, type PageData } from '@/api/request';

import type { DocMeta } from './doc-meta';

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
  /**
   * 仓库编码（≤40，业务上唯一）。
   *
   * **留空（`null` / `''` / 纯空白）⇒ 后端自动取号 `WH-01` 形态**（CODE-03：
   * `biz_seq` 的 WH 号段，`pad_width = 2` 纯流水、不计日期、永不重置，第 100 个起 `WH-100`）；
   * 有值则原样保留（企业自有编码体系如 `CK-001` 要能对接）。
   */
  warehouse_code: string | null;
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

/**
 * 仓库编码预览（`GET /business/erp/warehouse/next-code`，权限 `business:erp:warehouse:create`）。
 *
 * **只读、不占号**（交互方式 B 的方案 B）：连点两次拿到同一个号，点了又取消不会浪费流水；
 * 并发下两个人可能看到同一个号 —— 保存时被抢走后端回 `409 + 66001`，提示重新生成即可。
 *
 * 号段：`WH-01` 形态（2 位纯流水，`biz_seq.pad_width = 2`），不计日期、永不重置。
 */
export function fetchWarehouseNextCode(): Promise<NextCodePreview> {
  return get<NextCodePreview>(`${BASE}/warehouse/next-code`);
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

/**
 * 库位分页（关键词 / 仓库 / 类型 / 启用位）。
 *
 * ⚠️ P4-B 修正：这里原来写的是 `PageData<Location[]>`，而 `PageData<T>` 的 `list` 已经是 `T[]`
 * —— 于是 `data.list` 的类型变成 `Location[][]`（二维）。之前没人调用过这个函数，`vue-tsc`
 * 也就没报；库位管理页（`views/business/erp/location`）一用就露馅。
 * 与 `fetchWarehousePage`（W4b-2 修过同一处）口径对齐：`PageData<Location>`。
 *
 * 真出参：`{ list: [ {...LocationRes} ], total, page, page_size }`（`handler.rs::location_page`）。
 */
export function fetchLocationPage(query: {
  keyword?: string;
  warehouse_id?: string;
  location_type?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Location>> {
  return get<PageData<Location>>(`${BASE}/location/page`, asParams(query));
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
 * 物料类型 / 物料分类（与后端 `dto::MATERIAL_TYPES` 逐字一致，`ck_biz_material_type` 兜底）。
 *
 * ERP-S1（V0187）把枚举由 4 值扩到 7 值：新增 `PACKAGING` 包装物 / `CONSUMABLE` 低值易耗品 /
 * `OFFICE` 办公用品。后端常量与 DB CHECK 同步改过，前端只改这一个联合类型 + 选项表。
 *
 * 卡面草图写的是「分类」（`category_id` / `category_name`）—— 那是**猜的**：
 * `dto.rs::MaterialRes` 里**没有**这两个字段，`biz_material` 表也没有 `category_id` 列；
 * 后端只种了 `business:erp:category:*` 权限码，`handler.rs` 里**没有** `/erp/category` 路由
 * （`grep -n "erp/category" handler.rs` 零命中）。物料真正分的类就是 `material_type`，
 * 所以页面上的「分类」列 / 下拉 = `material_type`。
 */
export type MaterialType =
  | 'RAW'
  | 'SEMI'
  | 'FINISHED'
  | 'PACKAGING'
  | 'CONSUMABLE'
  | 'OFFICE'
  | 'SERVICE';

/**
 * 物料（对应后端 `dto.rs::MaterialRes`，字段名逐条抄自 serde 出参）。
 *
 * | 卡面草稿 | 真字段 | 说明 |
 * |---|---|---|
 * | `category_id` / `category_name` | —— | 不存在，见 `MaterialType` 注释 |
 * | `unit_name` | `unit_id` + `unit_name` | 后端 `MaterialRes` 已 join 取名（约定 13）；本页仍保留 `/erp/unit/page` 本地映射作兜底 |
 * | `warehouse_name` | `default_warehouse_id` + `warehouse_name` | 同上（约定 13，后端 LEFT JOIN `biz_warehouse`） |
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
  /** 基础计量单位名称（后端 join 取名，约定 13；单位被软删时也为 `null`） */
  unit_name?: string | null;
  /** 物料类型（= 页面上的「分类」） */
  material_type: MaterialType;
  /** 默认仓库 */
  default_warehouse_id: string | null;
  /** 默认仓库名称（后端 join 取名，约定 13；没设默认仓库时为 `null`） */
  warehouse_name?: string | null;
  /** 安全库存 */
  safety_stock: string | number;
  /** 计价方式 */
  costing_method: string;
  /** 存货科目 */
  inventory_account_id: string | null;
  /** 税率（0–1） */
  tax_rate: string | number;
  /**
   * 标准售价（**不含税**，`null` = 未维护）。
   *
   * 取价链第 ④ 级（`STANDARD`）：协议价 / 阶梯价都没命中时用它。
   * 与订单行 `price` 同口径（不含税）；协议价 / 阶梯价是含税价，
   * `GET /price/resolve` 会折算成 `price_excl` 再给前端填单。
   */
  standard_price: string | number | null;
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
  /**
   * 物料编码（未删除内唯一）。
   *
   * **留空（`null` / `''` / 纯空白）⇒ 后端自动取号 `MAT-0001` 形态**（CODE-03：
   * `biz_seq` 的 MAT 号段，纯流水 4 位、不计日期、永不重置）；
   * 有值则原样保留（物料常按分类自编号，`YB-001-02` 也要能用）。
   */
  material_code: string | null;
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
  /**
   * 标准售价（**不含税**，取价链第 ④ 级；留空 = 不维护）。
   *
   * 必须 `> 0`（后端 `ck_biz_material_standard_price` 同口径：0 元不表示免费）。
   */
  standard_price?: string | number | null;
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
  /** 标准售价（**不含税**；`null` = 清空标准售价，缺席 = 不改） */
  standard_price?: string | number | null;
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
  /** 是否系统预置（ERP-S1 V0187）：预置值不能删，只能停用 */
  is_system?: boolean;
  /** 数量小数位数（0–6，ERP-S1 V0187） */
  precision?: number;
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
 * 物料编码预览（`GET /business/erp/material/next-code`，权限 `business:erp:material:create`）。
 *
 * **只读、不占号**（交互方式 B 的方案 B）：连点两次拿到同一个号；并发撞号由保存时的
 * `409 + 66001` 兜底。号段：`MAT-0001` 形态（4 位纯流水），不计日期、永不重置。
 */
export function fetchMaterialNextCode(): Promise<NextCodePreview> {
  return get<NextCodePreview>(`${BASE}/material/next-code`);
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

/**
 * 来源单据类型（与 `serial_model::SOURCE_TYPES` 逐字一致，写错的类型会让「按来源追溯」查不到东西）。
 *
 * P2-5 词表收敛：改为从 `doc-kinds.ts`（= 后端 `SerialSourceType`）派生，
 * 不再在契约文件里手写一遍字面量 —— 两边漂了就编译不过 / 单测红。
 */
export type SerialSourceType = import('./doc-kinds').SerialSourceType;

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
  /**
   * 入库库位（可空；指向 `biz_location.id`，即用户在「入库库位」下拉里选的那个库位字典）。
   *
   * E2E-FIX-6 新增，与收货入库 `location_dict_id` 同口径：
   * 传 → 成本账 / 库位账 / 序列号都落该库位的账行（按 (物料, 仓库, 库位) find-or-create）；
   * 不传 → 后端用该仓库的**默认库位**兜底；
   * 仓库连默认库位都没有 → 落「未分配」桶（`location_id IS NULL`，与收货一致）。
   *
   * 不要与 `location_id` 同时发（后端 400）：前者是库位**账行**，后者是库位**字典**。
   */
  location_dict_id?: string | null;
  /** 批次号（可空，≤64） */
  batch_no?: string | null;
  /** 生成条数（1..=1000，`serial_model::MAX_BATCH_COUNT`） */
  count: number;
  /**
   * 成本单价（**必填**，不含税，> 0，最多 6 位小数，且 < 100000000000）。
   *
   * 「批量生成」= **无来源入库（期初 / 其他入库）**：没有采购单，成本由操作人负责，
   * 所以必须录单价；后端按「件数 × 单价」写 `biz_inventory.amount`（4 位小数）与库位账，
   * 并同事务生成凭证（借 1405 / 贷 1901），见 `docs/adr-20261031-erp-serial-batch-generate-semantics.md`。
   *
   * `number` 与字符串都收（后端 `OptionalDecimal` 是 untagged），但**不走 `0` 缺省**：
   * 缺字段 / `≤ 0` / 非数字一律 `400 + code 10001`（零落库），老客户端会被直接拒。
   */
  unit_cost: number | string;
  /** 来源类型 */
  source_type?: SerialSourceType | null;
  /** 来源单据 id（可空） */
  source_id?: string | null;
}

/** 记账后该桶（物料 × 仓库 × 库位行）的成本账快照（`serial_dto::BatchInventoryRes`） */
export interface BatchInventoryRes {
  /** 账面数量（`numeric(18,4)` → 字符串） */
  qty: string;
  /** 账面金额（`numeric(18,4)` → 字符串） */
  amount: string;
  /** 移动加权均价（`numeric(18,6)` → 字符串） */
  avg_cost: string;
}

/**
 * 批量生成出参（对应 `SerialBatchRes`）。
 *
 * `serial_nos` 给扫码枪 / 贴标机直接用；`list` 是回读的整行（带 `id`，列表才点得动状态流转）。
 *
 * 后端的 `Decimal` 一律序列化成**字符串**（精度不能交给 JS 的 double），所以这 3 个新增字段
 * 都声明成 `string`；页面只做展示与对账，不要 `Number()` 之后四舍五入。
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
  /** 本次成本单价（不含税，6 位小数） */
  unit_cost: string;
  /** 本次入账金额 = 该桶 `biz_inventory.amount` 的实际增量（4 位小数） */
  amount: string;
  /** 记账后该桶的成本账快照 */
  inventory: BatchInventoryRes;
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

  // ---------- 超期在途（LNK-F12 / 迁移 V0171，字段名逐字抄 `TransferRes`） ----------

  /**
   * 是否「超期在途」：`IN_TRANSIT` 且 `shipped_at` 早于「现在 − 阈值天数」。
   *
   * 判定口径唯一真源是后端（`transfer_model::is_in_transit_overdue` + 同一分界点的 SQL），
   * 前端**不自己算**：阈值走 `sys_config`（默认 7 天，运维可改），前端算必然漂移。
   * 声明成可选是因为「后端还没上这一版」时列表照常可用（页面一律 `=== true` 判）。
   */
  in_transit_overdue?: boolean;
  /** 已压天数（`floor((now − shipped_at) / 1 天)`，**仅超期行有值**） */
  overdue_days?: number | null;
  /** 最近一次超期告警时间（`null` = 扫描任务还没提醒过） */
  overdue_alerted_at?: string | null;
  /** 累计告警次数（`> 0` 说明真的推过站内信） */
  overdue_alert_count?: number;
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
 * 状态流转请求体（`PATCH /erp/transfer/{id}/status`，字段逐条抄 `transfer_dto::TransferStatusReq`）。
 *
 * - `PENDING`（提交审批）/ `CANCELLED`（撤回=取消整单）/ `IN_TRANSIT`（发货）/ `DONE`（收货）；
 * - `→ CANCELLED` 必须带 `reason`（**字段名就是 `reason`**，不是 `cancel_reason`；
 *   后端 `TransferStatusReq` 只有 `reason`，传 `cancel_reason` 会被 serde 丢掉 → 400「必须给 reason」）；
 * - 发货时 `transit_warehouse_id` 会被写进主表（`IN_TRANSIT` 要求非空）。
 */
export interface TransferStatusReq {
  /** 目标状态 */
  status: Exclude<TransferStatus, 'APPROVED'>;
  /** 取消原因（仅 `CANCELLED` 必填，最长 255 字，空白视为没给） */
  reason?: string | null;
  /** 在途仓 id（仅 `IN_TRANSIT` 用；不传则由后端取种子 `WH-TRANSIT`） */
  transit_warehouse_id?: string | null;
}

/**
 * 调拨单分页（`GET /erp/transfer/page`，服务端分页）。
 *
 * 参数照卡面契约：`status` / `from_warehouse_id` / `to_warehouse_id` / `transfer_no`（模糊），
 * 以及 LNK-F12 的 `in_transit_overdue`（`true` = 只看超期在途 / `false` = 排除超期 / 缺省 = 不筛）。
 *
 * ⚠️ 矛盾组合后端直接 **400**（不是空列表）：
 * `in_transit_overdue=true` + `status=!=IN_TRANSIT`、`in_transit_overdue=false` + `status=IN_TRANSIT`。
 * 页面走 `buildTransferQuery()`，它在超期开关打开时把 `status` 整键丢掉，不会拼出矛盾组合。
 *
 * 空串一律不发（`asParams` 兜住）—— 后端把 `?transfer_no=` 当「等于空串」过滤会查出 0 行。
 */
export function fetchTransferPage(query: {
  status?: TransferStatus | '';
  from_warehouse_id?: string;
  to_warehouse_id?: string;
  transfer_no?: string;
  in_transit_overdue?: boolean;
  page?: number;
  page_size?: number;
}): Promise<PageData<TransferRow>> {
  return get<PageData<TransferRow>>(`${BASE}/transfer/page`, asParams(query));
}

/**
 * 超期在途汇总（`GET /erp/transfer/overdue/summary`，LNK-F12 新增）。
 *
 * 给页头角标用：「现在有几张超期单、阈值是多少天」。权限码复用列表的
 * `business:erp:transfer:list`，不需要额外授权。
 *
 * 后端对「阈值读不到」的处理是回退 7 天（不是不告警），所以 `threshold_days` 一定有值；
 * 这里仍写成 number 并让页面按缺省 7 兜底，避免旧版后端（没有这个接口）时角标文案变成 `undefined`。
 */
export interface TransferOverdueSummary {
  /** 超期阈值（天），来自 `sys_config.erp.transfer.in_transit_overdue_days` */
  threshold_days: number;
  /** 当前超期在途单总数（含所有页） */
  total: number;
  /** 最早一张超期单的发货时间（`null` = 没有超期单） */
  oldest_shipped_at: string | null;
}

/** 超期在途汇总（`GET /erp/transfer/overdue/summary`） */
export function fetchTransferOverdueSummary(): Promise<TransferOverdueSummary> {
  return get<TransferOverdueSummary>(`${BASE}/transfer/overdue/summary`);
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
 * 复制为新单（D9：`POST /erp/transfer/{id}/copy`，权限 = 建单权限）。
 *
 * 后端语义：新单号 + `DRAFT`，带出两仓 / 原因 / 备注 / 明细（物料·数量·库位），
 * 状态时间戳与审批关联全清，**序列号不复制**（实物号码跟着原单走，复制单要重新选）。
 */
export function copyTransfer(id: string): Promise<TransferDetail> {
  return post<TransferDetail>(`${BASE}/transfer/${id}/copy`, {});
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

// ==================== 供应商（W1c 只读接口；P2-E1 采购 / 收货页的下拉与译名） ====================

/**
 * 供应商（逐字抄 `dto.rs` `SupplierRes`）。
 *
 * # 为什么不叫 `code` / `name`
 *
 * 真列名是 `supplier_code` / `supplier_name`（`V0137__erp_master_data_and_warehouse`）。
 * 采购单 / 收货单只存 `supplier_id`，列表要显示「供应商A」就必须靠这张表译名 ——
 * 猜成 `name` 的代价是整列空白，编译期抓不到。
 */
export interface Supplier {
  /** 主键 */
  id: string;
  /** 编码（未删除内唯一） */
  supplier_code: string;
  /** 名称 */
  supplier_name: string;
  /** 简称 */
  short_name: string | null;
  /** 联系人 */
  contact_person: string | null;
  /** 联系电话 */
  contact_phone: string | null;
  /** 邮箱 */
  email: string | null;
  /** 纳税人识别号 */
  tax_no: string | null;
  /** 地址 */
  address: string | null;
  /** 账期天数（采购单的 `credit_days` 默认值来源） */
  credit_days: number;
  /** 信用额度 */
  credit_limit: number | string;
  /** 应付科目 */
  payable_account_id: string | null;
  /** 开票抬头 */
  invoice_title: string | null;
  /** 开户行 */
  invoice_bank_name: string | null;
  /** 银行账号 */
  invoice_bank_account: string | null;
  /** 启用状态：1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 供应商下拉候选（只取启用的，够填满下拉即可）。
 *
 * 分页接口 `GET /erp/supplier/page` 的 list 元素就是 `SupplierRes`，这里只翻一页，
 * 不提供「全量拉取」——主数据上千条时下拉应该走远端搜索，那是后续卡的事。
 */
export function fetchSupplierOptions(limit = 200): Promise<Supplier[]> {
  return get<PageData<Supplier>>(
    `${BASE}/supplier/page`,
    asParams({ status: 1, page: 1, page_size: limit }),
  ).then((page) => page.list ?? []);
}

// ==================== 采购订单（P2-A2 / P2-E1） ====================

/**
 * 金额口径（`biz_purchase_order.price_mode` + `ck_biz_po_price_mode`）。
 *
 * | 值 | 含义 | 单价列含义 | 公式（`purchase_model::line_amounts`） |
 * |---|---|---|---|
 * | `EXCL` | 不含税录入 | `price_excl` | `amount=qty*price` / `tax=amount*rate%` / `with_tax=amount+tax` |
 * | `INCL` | 含税录入 | `price_incl` | `with_tax=qty*price` / `amount=with_tax/(1+rate%)` / `tax=with_tax-amount`（**减法倒挤**） |
 *
 * ⚠️ 口径 B 的税额必须是**减法倒挤**：再乘一次税率会出现 `amount+tax ≠ with_tax` 的分钱差，
 * 而库里 `ck_biz_po_item_tax` / `ck_biz_si_item_tax` 是**精确恒等**，写不进去。
 */
export type PriceMode = 'EXCL' | 'INCL';

/**
 * 采购订单状态（`purchase_model::STATUS_*` + `ck_biz_po_status`）。
 *
 * P2-A2 的 4 条合法边（`LEGAL_EDGES`）：
 * `DRAFT→SUBMITTED` / `DRAFT→CANCELLED` / `SUBMITTED→CLOSED` / `SUBMITTED→CANCELLED`。
 *
 * `APPROVED` / `PARTIAL_RECEIVED` / `RECEIVED` 由审批与收货流程维护（P2-B1 的过账会推进），
 * **前端不主动发起**，但列表要能显示（`status_label` 后端已给中文）。
 */
export type PurchaseOrderStatus =
  | 'DRAFT'
  | 'SUBMITTED'
  | 'APPROVED'
  | 'PARTIAL_RECEIVED'
  | 'RECEIVED'
  | 'CLOSED'
  | 'CANCELLED';

/**
 * 采购类型（A3/V0198 的 `biz_purchase_order.purchase_type`）。
 *
 * - `CONTRACT` 合同采购：供应商**从合同乙方派生**（只读），必须带 `contract_id`；
 * - `SPOT` 现货采购：供应商手选（历史行为），存量单全是这个值。
 *
 * ⚠️ 建单后**不允许改**（后端 409 `locked_change`）：改类型等于换掉供应商来源，
 * 与已生成的收货单 / 应付单口径冲突。
 */
export type PurchaseType = 'CONTRACT' | 'SPOT';

/**
 * 采购订单主表行（逐字抄 `dto.rs::PurchaseOrderRes`，列表与详情共用同一形状）。
 *
 * 列表接口 `GET /erp/purchase-order/page` 的 list 元素就是它（**不是** `PurchaseOrderCard`：
 * 那个精简版没有 `price_mode`，页面拿不到口径徽标就没法在列表上标注）。
 *
 * NUMERIC 列经 serde 出 JSON 时可能是 number 也可能是字符串，故金额写 `number | string`，
 * 页面统一走 `toNumber()` / `formatMoney()` 归一。
 */
export interface PurchaseOrderRow {
  /** 主键 */
  id: string;
  /** 采购单号 `PO-yyyyMMdd-000001` */
  order_no: string;
  /** 供应商 id（弱引用 `biz_supplier.id`） */
  supplier_id: string;
  /** 下单日期（DATE，`YYYY-MM-DD`） */
  order_date: string;
  /** 期望到货日 */
  expect_date: string | null;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 状态 */
  status: PurchaseOrderStatus;
  /** 币种 */
  currency_code: string;
  /** 合计数量 */
  total_qty: number | string;
  /** 不含税金额（`total_amount`，**不是** `amount_excl`） */
  total_amount: number | string;
  /** 税额（`total_tax`） */
  total_tax: number | string;
  /** 价税合计（`total_with_tax`）—— 列表「金额(价税合计)」列读它 */
  total_with_tax: number | string;
  /** 账期天数 */
  credit_days: number;
  /** 备注 */
  remark: string | null;
  /** 金额口径（PRD §3.1.4 的口径徽标读它） */
  price_mode: PriceMode;
  /** 采购类型（A3/V0198：`CONTRACT` 合同采购 / `SPOT` 现货采购；存量单全为 `SPOT`） */
  purchase_type: PurchaseType;
  /** 合同 id（只有 `CONTRACT` 单有；详情「合同号」点进去跳合同详情用它） */
  contract_id: string | null;
  /** 合同号（**快照**，与下面 `contract_no_snapshot` 同源；`SPOT` 单为 `null`） */
  contract_no: string | null;
  /**
   * 合同号快照列原文（`biz_purchase_order.contract_no_snapshot`）。
   *
   * 列表页**只读它**（R3-a / AP-6：不为显示单号去实时 JOIN 合同表，否则合同改名后历史单会「跟着变」）。
   */
  contract_no_snapshot: string | null;
  /** 供应商名称（显示用：快照优先，存量行由后端用主档现值兜底） */
  supplier_name: string | null;
  /** 供应商名称快照列原文（存量行 `null`，那时名称不可知） */
  supplier_name_snapshot: string | null;
  /**
   * 收货仓库名称（P2-9 / DC-23；**列表读快照、详情读现值**）。
   *
   * - 列表：`warehouse_name_snapshot`（仓库改名后仍显示下单当时的名字），老单
   *   （快照为 `null`）由后端批量现查兜底，所以列表里这一列**基本总有值**；
   * - 详情：关联查的现名（点进单据看到「现在是哪个仓」），仓库被软删时回落快照。
   *
   * 前端**不要**再拿 `warehouse_id` 去 `warehouses` 列表里找名字（那是第二份真相，
   * 且找不到时会退化成 uuid 短号）；只在这个字段为空时兜底。
   */
  warehouse_name: string | null;
  /** 收货仓库名称快照列原文（P2-9 之前的老单为 `null`） */
  warehouse_name_snapshot: string | null;
  /**
   * 所属项目 id（V0237 / P2-8）。`null` = **未归集项目**（存量 18 张单全是它）。
   *
   * ⚠️ 与 `supplier_id` 不同：这是**弱引用 + 无外键**，项目被软删后本字段仍然保留原 uuid
   * （历史单据不能因为项目没了就丢掉归集线索），而下面的 `project_name` 会变成 `null`。
   * 所以「有 id 没名字」= 项目已删，界面显示「（已删除项目）」——**永远不显示 uuid**。
   */
  project_id: string | null;
  /**
   * 项目**现名**（后端实时关联查 `biz_project`，不是快照）。
   *
   * 口径与供应商名相反：项目改名后旧单必须跟着显示新名（否则按项目汇总会因改名裂成两行）；
   * 项目已软删 / 查不到时为 `null`，此时用 {@link project_id} 判「已删除项目」。
   */
  project_name: string | null;
  /** 提交时间 */
  submitted_at: string | null;
  /** 提交人 */
  submitted_by: string | null;
  /** 审批时间 */
  approved_at: string | null;
  /** 审批人 */
  approved_by: string | null;
  /** 关闭时间 */
  closed_at: string | null;
  /** 关闭人 */
  closed_by: string | null;
  /** 审批实例 id（P-FLOW-3A；null = 没走审批 / 已驳回打回） */
  approval_instance_id: string | null;
  /** 审批实例单号 `FLW-…`（列表显示「审批中（FLW-xxx）」用，后端批量回填） */
  approval_instance_no: string | null;
  /**
   * 提交时点是否超供应商信用额度（P2-2 ① / V0215）。
   *
   * ⚠️ `false` **不等于**「查过、没超」：从未提交过的草稿也是 `false`。
   * 要区分请同时看 {@link credit_exposure}：`null` = 没查过（未评估）。
   */
  over_credit_limit: boolean;
  /**
   * 提交时点「该供应商累计未结采购额（含本单）」快照；`null` = 未评估。
   *
   * 口径 = `SUM(total_with_tax)` WHERE 同供应商 AND `status <> 'CANCELLED'` AND 未删。
   * 只做展示/审计，**不是**实时值（实时值只在提交那一刻现算，见后端 `purchase_credit`）。
   */
  credit_exposure: number | string | null;
  /** 提交时点供应商信用额度快照（`0` = 不限） */
  credit_limit_snapshot: number | string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
  /**
   * 预算尚未生效的提示（FIX-2 ①）。
   *
   * **只在详情接口回填**（列表不带这个字段，后端 `skip_serializing_if` 空数组）。
   * 含义：该科目存在 DRAFT / 审批中 / 未到生效日的预算 → 本期的超支**不受它控制**。
   * 只提示、不阻断（提交照常放行）。
   */
  budget_warnings?: BudgetWarningRow[];
}

/** 预算未生效提示（`budget_dto::BudgetWarningRes`，报销单 / 采购单共用）。 */
export interface BudgetWarningRow {
  /** 提示码（固定 `BUDGET_NOT_EFFECTIVE`） */
  code: string;
  /** 预算科目 id */
  account_id: string;
  /** 预算科目编码 */
  account_code: string;
  /** 预算单号 */
  budget_no: string;
  /** DRAFT / SUBMITTED / ACTIVE */
  status: string;
  /** 状态中文 */
  status_label: string;
  /** 一句话说明（直接展示） */
  message: string;
}

/**
 * 采购明细（`GET /erp/purchase-order-item/page` 的 list 元素，= `dto.rs::PurchaseOrderItemRes`）。
 *
 * ⚠️ 三个金额字段是 `amount` / `tax_amount` / `with_tax_amount`，
 * **不是**卡面草稿写的 `line_amount` / `line_tax` / `line_total`（那是架构文档的别名）。
 *
 * ⚠️ 本接口**只给 `price`（不含税单价）**，没有 `price_incl`。含税口径的用户单价要用
 * `with_tax_amount / qty` 反算（`purchase-logic.ts::priceOfLineInMode`），
 * 别改用 `price * (1+rate)` —— 后者在大额/多位小数上会与库里的 `price_incl` 差分钱。
 */
export interface PurchaseOrderItemLine {
  /** 主键 */
  id: string;
  /** 所属订单 */
  order_id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 计量单位 id */
  unit_id: string;
  /** 数量 */
  qty: number | string;
  /** 不含税单价 */
  price: number | string;
  /** 税率（0–100，不是 0–1） */
  tax_rate: number | string;
  /** 不含税金额 */
  amount: number | string;
  /** 税额 */
  tax_amount: number | string;
  /** 价税合计 */
  with_tax_amount: number | string;
  /** 已收货数量（收货过账回写） */
  received_qty: number | string;
  /** 行备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 建单 / 改单返回的明细行（`purchase_dto.rs::PurchaseOrderLineRes`）。
 *
 * 与 {@link PurchaseOrderItemLine} 的差别只有两处：本形状**同时给 `price_excl` 与 `price_incl`**，
 * 且没有 `order_id` / `created_at`；建单成功后直接拿它回填表单即可，不用再查一次明细。
 */
export interface PurchaseOrderLine {
  /** 主键 */
  id: string;
  /** 行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 计量单位 id */
  unit_id: string;
  /** 数量 */
  qty: number | string;
  /** 不含税单价 */
  price_excl: number | string;
  /** 含税单价 */
  price_incl: number | string;
  /** 税率（0–100） */
  tax_rate: number | string;
  /** 不含税金额 */
  amount: number | string;
  /** 税额 */
  tax_amount: number | string;
  /** 价税合计 */
  with_tax_amount: number | string;
  /** 已收货数量 */
  received_qty: number | string;
  /** 行备注 */
  remark: string | null;
}

/** 建单 / 改单 / 流转的返回（`purchase_dto.rs::PurchaseOrderDetailRes`：单头 + 明细） */
export interface PurchaseOrderDetail {
  /**
   * 单头（统一三段式口径 `data.header`，E2E01-FIX-4）
   *
   * 前端一律读这个字段；`order` 是兼容期保留的旧名，不要再用。
   */
  header: PurchaseOrderRow;
  /** 明细 */
  items: PurchaseOrderLine[];
  /** 附属信息（`data.meta`）：审批实例号 / 实例状态；没走审批时是 `{}` */
  meta: DocMeta;
  /**
   * @deprecated 与 `header` 同值，兼容期保留。新代码读 `header`。
   */
  order: PurchaseOrderRow;
}

/**
 * 建单 / 改单的一条明细入参（`purchase_dto.rs::PurchaseOrderItemReq`）。
 *
 * ⚠️ **没有金额字段**：金额一律服务端算（硬红线 2）。前端算的合计数只用于「所见即所得」，
 * 落库以响应里的 `order.total_*` 为准；两边公式相同（见 {@link PriceMode}），
 * 不一致只可能是舍入实现差异，页面显示服务端回值。
 */
export interface PurchaseOrderItemReq {
  /** 物料 id */
  material_id: string;
  /** 数量（> 0；最多 4 位小数，`ck_biz_po_item_qty`） */
  qty: number;
  /** 不含税单价（`EXCL` 口径必填） */
  price_excl?: number | null;
  /** 含税单价（`INCL` 口径必填；缺省时后端回退读 `price_excl`） */
  price_incl?: number | null;
  /** 税率（0–100） */
  tax_rate: number;
  /** 行备注 */
  remark?: string | null;
}

/** 建单请求体（`POST /erp/purchase-order`） */
export interface CreatePurchaseOrderReq {
  /**
   * 供应商 id。
   *
   * - `SPOT`（现货采购）：**必填**；
   * - `CONTRACT`（合同采购）：**可以不传** —— 服务端从合同乙方派生；传了必须等于乙方
   *   （不等 → 400 `66025`，不静默覆盖）。前端在合同采购时不传，避免「填了个和自己不一样的」。
   */
  supplier_id?: string;
  /**
   * 采购类型（省缺 = 沿用当前类型；新建省缺 = `SPOT`）。
   *
   * ⚠️ 改单时**不能与库里不同**（后端 409），所以编辑态只读回传原值。
   */
  purchase_type?: PurchaseType;
  /** 合同 id（`CONTRACT` 必填；`SPOT` 必须为空，DTO 会拦） */
  contract_id?: string | null;
  /**
   * 所属项目 id（**可空**；V0237 / P2-8）。
   *
   * - `null` / 不传 = 未归集项目；
   * - 非空必须是「存在且未软删」的项目，否则 `400 + 66002`；非法 UUID → `400 + 66006`；
   * - **归档项目允许挂**（补录当期采购是合法业务，状态过滤交前端下拉）。
   *
   * ⚠️ 改单是**整单覆盖**：不传 `project_id` 就等于清空项目归属。所以表单提交必须
   * 始终带 `project_id: form.project_id || null`（见 `purchase-logic.ts::buildPurchasePayload`）。
   */
  project_id?: string | null;
  /** 收货仓库 id（必填） */
  warehouse_id: string;
  /** 下单日期 `YYYY-MM-DD`（不传 = 服务端取北京今天） */
  order_date?: string;
  /** 期望到货日（不得早于 `order_date`） */
  expect_date?: string | null;
  /** 金额口径（不传 = `EXCL`） */
  price_mode?: PriceMode;
  /** 账期天数（≥ 0） */
  credit_days?: number;
  /** 备注（≤ 500 字） */
  remark?: string | null;
  /** 明细（至少 1 行，至多 200 行） */
  items: PurchaseOrderItemReq[];
}

/** 改单请求体（`PUT /erp/purchase-order/{id}`，**仅 DRAFT**，整单覆盖明细） */
export type UpdatePurchaseOrderReq = CreatePurchaseOrderReq;

/**
 * 状态流转请求体（`PATCH /erp/purchase-order/{id}/status`）。
 *
 * 只允许 `LEGAL_EDGES` 的 4 条边，其余 409；`CANCELLED` 建议带 `reason`（写进备注/审计）。
 */
export interface PurchaseOrderStatusReq {
  /** 目标状态 */
  status: 'SUBMITTED' | 'CLOSED' | 'CANCELLED';
  /** 原因（取消 / 关闭时填） */
  reason?: string | null;
}

/** 采购订单分页（`GET /erp/purchase-order/page`，服务端分页） */
export function fetchPurchaseOrderPage(query: {
  keyword?: string;
  status?: PurchaseOrderStatus | '';
  supplier_id?: string;
  warehouse_id?: string;
  /** 所属项目筛选（V0237 `idx_biz_po_project`；本卡不做筛选条 UI，只留参数） */
  project_id?: string;
  date_from?: string;
  date_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<PurchaseOrderRow>> {
  return get<PageData<PurchaseOrderRow>>(`${BASE}/purchase-order/page`, asParams(query));
}

/** 采购订单详情（`GET /erp/purchase-order/{id}`，**只有单头**，明细走 {@link fetchPurchaseOrderItems}） */
export function fetchPurchaseOrderDetail(id: string): Promise<PurchaseOrderRow> {
  return get<PurchaseOrderRow>(`${BASE}/purchase-order/${id}`);
}

/**
 * 某张采购单的明细（`GET /erp/purchase-order-item/page?order_id=`）。
 *
 * 详情接口不返回明细，编辑回填必须走这条路。`page_size` 取 200（后端建单上限 200 行）。
 */
export function fetchPurchaseOrderItems(orderId: string): Promise<PurchaseOrderItemLine[]> {
  return get<PageData<PurchaseOrderItemLine>>(
    `${BASE}/purchase-order-item/page`,
    asParams({ order_id: orderId, page: 1, page_size: 200 }),
  ).then((page) => page.list ?? []);
}

/** 建单（`POST /erp/purchase-order`，权限 `business:erp:purchase-order:create`） */
export function createPurchaseOrder(payload: CreatePurchaseOrderReq): Promise<PurchaseOrderDetail> {
  return post<PurchaseOrderDetail>(`${BASE}/purchase-order`, payload);
}

/** 改单（`PUT /erp/purchase-order/{id}`，**仅 DRAFT**，整单覆盖明细） */
export function updatePurchaseOrder(
  id: string,
  payload: UpdatePurchaseOrderReq,
): Promise<PurchaseOrderDetail> {
  return put<PurchaseOrderDetail>(`${BASE}/purchase-order/${id}`, payload);
}

/** 状态流转（`PATCH /erp/purchase-order/{id}/status`，其余 409） */
export function updatePurchaseOrderStatus(
  id: string,
  payload: PurchaseOrderStatusReq,
): Promise<PurchaseOrderDetail> {
  return patch<PurchaseOrderDetail>(`${BASE}/purchase-order/${id}/status`, payload);
}

/**
 * 提交采购订单审批（`POST /erp/purchase-order/{id}/submit-approval`，P-FLOW-3A）。
 *
 * 后端按已发布的 `po_approve` 流程发起审批（金额三档：< 1 万部门主管 / 1–10 万采购经理 /
 * ≥ 10 万采购经理+总经理会签），单据 `DRAFT → SUBMITTED`（列表显示「审批中」）。
 * 没配流程时后端**降级**为直接提交（并留 warn），前端无感。
 *
 * 错误：409 已在审批中（报文带 `FLW-xxx`）/ 不是草稿；400 无明细或金额为 0。
 */
export function submitPurchaseOrderApproval(id: string): Promise<PurchaseOrderDetail> {
  return post<PurchaseOrderDetail>(`${BASE}/purchase-order/${id}/submit-approval`, {});
}

/** 删除（`DELETE /erp/purchase-order/{id}`，**仅 DRAFT**，软删） */
export function deletePurchaseOrder(id: string): Promise<void> {
  return del<void>(`${BASE}/purchase-order/${id}`);
}

// ==================== 采购单待收明细（D4：一键生成收货单） ====================

/**
 * 采购单单头摘要（`purchase_dto.rs::PendingOrderRes`）。
 *
 * 供应商 / 仓库**名称**由后端一并给出：收货页要把它回填进下拉框，名称给了就不用再查一次。
 */
export interface PendingReceiveOrder {
  id: string;
  order_no: string;
  status: string;
  status_label: string;
  supplier_id: string;
  supplier_name: string;
  warehouse_id: string;
  warehouse_name: string;
  total_qty: number | string;
  order_date: string;
  remark: string | null;
}

/**
 * 待收明细行（`purchase_dto.rs::PendingItemRes`）。
 *
 * 数量口径（卡面 F2 / 硬红线 4）：
 * - `received_qty` **只算已过账（POSTED）的收货单**，草稿 / 作废都不算；
 * - `pending_qty = ordered_qty - received_qty`（负数按 0）。
 *
 * 前端带出时用 `pending_qty` 当默认数量，并在行上显示「订单 / 已收 / 待收」让用户看得见上限。
 */
export interface PendingReceiveItem {
  /** 采购明细行 id（建收货单时落 `purchase_order_item_id`） */
  item_id: string;
  line_no: number;
  material_id: string;
  material_code: string;
  material_name: string;
  unit_id: string;
  unit_name: string;
  ordered_qty: number | string;
  received_qty: number | string;
  pending_qty: number | string;
  price_excl: number | string;
  price_incl: number | string;
  tax_rate: number | string;
  cost_price: number | string;
}

/** 已生成收货单（采购单详情反查用；`posted` = 已过账，草稿不计入已收数量） */
export interface PendingReceiveStockIn {
  id: string;
  in_no: string;
  status: string;
  status_label: string;
  in_date: string;
  total_qty: number | string;
  posted: boolean;
}

/** 待收明细响应（`GET /erp/purchase-order/{id}/pending-items`） */
export interface PendingReceiveRes {
  order: PendingReceiveOrder;
  /** **全部行**都返回（`pending_qty = 0` 的也在，前端据此显示「已收满」） */
  items: PendingReceiveItem[];
  /** 这张采购单生成过的收货单（未软删，新单在前） */
  stock_ins: PendingReceiveStockIn[];
  /** 还有几行没收完 */
  pending_line_count: number;
  /** 是否还能再收（状态可收 **且** 至少一行 `pending_qty > 0`）——「生成收货单」按钮的显示条件 */
  can_receive: boolean;
  /** 不能收的原因（后端拼好，前端直接展示） */
  blocked_reason: string | null;
}

/**
 * 查采购单待收明细（`GET /erp/purchase-order/{id}/pending-items`，权限同采购单查看）。
 *
 * 采购单详情页据此决定「生成收货单」按钮显不显示；收货页据此一键带出单头 + 明细。
 */
export function fetchPurchaseOrderPendingItems(id: string): Promise<PendingReceiveRes> {
  return get<PendingReceiveRes>(`${BASE}/purchase-order/${id}/pending-items`);
}

// ==================== 收货入库（P2-B1 / P2-E1） ====================

/**
 * 收货单状态（`stockin_model::STATUS_*`）。
 *
 * P16 起作废是**两步**：`POSTED → PENDING_CANCEL`（发起作废审批）→ 审批通过 → `CANCELLED`；
 * 驳回 / 撤回则由 `PENDING_CANCEL` 回到 `POSTED`。草稿仍可直接 `DRAFT → CANCELLED`。
 */
export type StockInStatus = 'DRAFT' | 'POSTED' | 'PENDING_POST' | 'PENDING_CANCEL' | 'CANCELLED';

/**
 * 收货入库单主表（逐字抄 `stockin_dto.rs::StockInRes`）。
 *
 * ⚠️ 列表接口的 list 元素是**扁平**的 `StockInRes`；详情接口 `GET /erp/stock-in/{id}`
 * 返回的是 {@link StockInDetail}（`header` 包了一层 + `items`）—— 两种形状别搞混。
 */
export interface StockInRow {
  /** 主键 */
  id: string;
  /** 收货单号 `IN-yyyyMMdd-000001` */
  in_no: string;
  /** 入库类型（`PURCHASE_IN` 等） */
  in_type: string;
  /** 来源采购单 id（可空 = 无源直收） */
  source_order_id: string | null;
  /**
   * 来源采购单**单号**（`PO-yyyyMMdd-000001`；无来源 / 采购单已删 = `null`）。
   *
   * 后端列表接口批量补齐（`stockin_handler::stock_in_page` 走一次 `IN (...)`），
   * 前端不该为了一列单号再发 N 次请求。列上「来源采购单」直接渲染它。
   */
  source_order_no: string | null;
  /** 供应商 id */
  supplier_id: string | null;
  /**
   * 供应商名称（后端按 `supplier_id` 快照读主数据，**不 JOIN 采购单** —— R3-a）。
   * 列表/详情直接渲染它，别在前端自己拼。
   */
  supplier_name?: string | null;
  /**
   * 无源单据标记（A4/V0199：`true` = 期初 / 复制 / 其他，没有来源采购单）。
   *
   * 它**不是**「`source_order_id` 为空」的同义反复：后端要求建单时**显式**声明
   * （`is_sourceless = true` + 备注），否则 400 `66027` —— 无源要可枚举、能进巡检。
   */
  is_sourceless?: boolean;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 入库日期 `YYYY-MM-DD` */
  in_date: string;
  /** 状态 */
  status: StockInStatus;
  /** 状态中文（后端给：草稿 / 已过账 / 已作废） */
  status_label: string;
  /** 币种 */
  currency_code: string;
  /** 合计数量 */
  total_qty: number | string;
  /** 不含税金额 */
  total_amount: number | string;
  /** 税额 */
  total_tax: number | string;
  /** 价税合计（列表「金额(价税合计)」列读它） */
  total_with_tax: number | string;
  /** ★ 发票已随货到达（过账弹窗的必选项；`false` = 暂估，本批不可选） */
  invoice_received: boolean;
  /** 应付单 id（过账后生成，P2-C1） */
  voucher_id: string | null;
  /** 记账凭证 id（过账后生成，P2-C1） */
  payable_id: string | null;
  /** 备注 */
  remark: string | null;
  /** 过账时间 */
  posted_at: string | null;
  /** 过账人 */
  posted_by: string | null;
  /** 作废时间 */
  cancelled_at: string | null;
  /** 作废人 */
  cancelled_by: string | null;
  /** 作废原因（P16：`PENDING_CANCEL` 时是**申请理由**，审批人据此判断） */
  cancel_reason: string | null;
  /** 作废审批实例 id（P16；只有「作废审批中」非空，详情抽屉据此画审批进度） */
  approval_instance_id: string | null;
  /** 创建人（P16：内控口径「复核人 ≠ 制单人」，前端据此提示） */
  create_by: string | null;
  /** 制单人姓名（AUD-02 起后端批量 join 下发；不再把 id 当显示名） */
  create_by_name?: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 收货明细（`stockin_dto.rs::StockInItemRes`）。
 *
 * ⚠️ 行金额同样是 `amount` / `tax_amount` / `with_tax_amount`；
 * `price` 是**不含税**单价（入库成本口径，硬红线：成本只用不含税金额）。
 */
export interface StockInItemLine {
  /** 主键 */
  id: string;
  /** 行号 */
  line_no: number;
  /** 来源采购明细 id */
  source_item_id: string | null;
  /** 物料 id */
  material_id: string;
  /** 计量单位 id（`NOT NULL`，建单时由后端查物料补） */
  unit_id: string;
  /** 数量（> 0） */
  qty: number | string;
  /** 不含税单价 */
  price: number | string;
  /** 含税单价 */
  price_incl: number | string;
  /** 税率（0–100） */
  tax_rate: number | string;
  /** 不含税金额 */
  amount: number | string;
  /** 税额 */
  tax_amount: number | string;
  /** 价税合计 */
  with_tax_amount: number | string;
  /** 入库成本单价快照（过账时的移动加权后单价） */
  cost_price: number | string;
  /** 库位账行 id（`biz_inventory_location.id`；过账后才落定，可空 = 未分配桶） */
  location_id: string | null;
  /**
   * 用户选的库位（`biz_location.id`，P2-4）。
   *
   * 为空 = 建单时没指定 → 过账用仓库默认库位，没有默认则进「未分配」桶
   * （此时详情会带 `location_warnings` 提示）。
   */
  location_dict_id?: string | null;
  /** 库位编码（后端按 `location_dict_id` 译名，如 `A-01-03`） */
  location_code?: string | null;
  /** 库位名称 */
  location_name?: string | null;
  /** 批次号 */
  lot_no: string | null;
  /** 行备注 */
  remark: string | null;
}

/** 收货单详情（`GET /erp/stock-in/{id}`：**header 包了一层** + items + 序列号件数） */
export interface StockInDetail {
  /** 单头 */
  header: StockInRow;
  /** 明细 */
  items: StockInItemLine[];
  /** 序列号件数 */
  serial_count: number;
  /** 口径说明（后端生成，用于页面底部小字） */
  summary: string;
  /** 来源采购单 id（= `header.source_order_id`，卡面 D4/F3 的字段名） */
  purchase_order_id: string | null;
  /** 来源采购单号（详情页「来源采购单：PO-xxx」+ 可点回采购单） */
  purchase_order_no: string | null;
  /**
   * 库位提示（P2-4）：未指定库位会进「未分配」桶 / 库位超容量 —— **只提示不拦截**。
   *
   * 服务端按当前数据现算（草稿态已把本单数量算进来），前端显示成告警条即可。
   */
  location_warnings?: string[];
  /**
   * 关联审批实例的当前状态（BUG-02）：`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED`。
   *
   * `PENDING_POST` 一个状态分不清「审批还在跑」和「审批已通过但过账失败」—— 后者要看这里。
   */
  approval_instance_status?: string | null;
  /**
   * 能不能点「重新过账」（BUG-02）：服务端算好的 `status === 'PENDING_POST' && 实例 === 'APPROVED'`。
   *
   * 前端只认这个字段，不自己拼条件（口径只有一份，避免两边不一致）。
   */
  repost_available?: boolean;
}

/**
 * 建单 / 改单的一条明细（`stockin_dto.rs::StockInItemReq`）。
 *
 * - `price_excl` 是**必填口径**：收货入库只认不含税单价（成本基础）；
 * - `serial_ids`：一物一码物料在**建单期只做条数 / 去重 / 长度校验**，
 *   真正落号在过账（`serials` 参数）；
 * - 带 `source_order_id` 时 `purchase_order_item_id` 必填，否则后端无法回写已收数量（400）。
 */
export interface StockInItemReq {
  /** 来源采购明细 id（带 `source_order_id` 时必填） */
  purchase_order_item_id?: string | null;
  /** 物料 id */
  material_id: string;
  /** 数量（> 0，0 数量 400） */
  qty: number;
  /** 不含税单价（≥ 0） */
  price_excl: number;
  /** 税率（0–100） */
  tax_rate: number;
  /** 库位 id（可空） */
  location_id?: string | null;
  /**
   * 用户选的库位（`biz_location.id`，P2-4，**推荐用这个**）。
   *
   * 后端校验：必须属于本单收货仓库、未软删、已启用；不传 → 仓库默认库位，仍没有则进「未分配」桶。
   */
  location_dict_id?: string | null;
  /** 序列号（一物一码物料；长度为 0 表示过账时自动取号） */
  serial_ids?: string[];
  /** 行备注 */
  remark?: string | null;
}

/** 建单请求体（`POST /erp/stock-in`） */
export interface CreateStockInReq {
  /** 来源采购单 id（`items` 为空**当且仅当**带它：后端按未收数量自动带出） */
  source_order_id?: string | null;
  /**
   * 来源采购单 id 的**业务别名**（卡面 D4 的字段名，与 `source_order_id` 是同一列）。
   *
   * 两个都给时后端要求它们相等（不等 400），本页只给这一个 —— 「采购订单 → 生成收货单」
   * 这条链路读起来更顺。带它时每行必须带 `purchase_order_item_id`，否则后端无法回写已收数量。
   */
  purchase_order_id?: string | null;
  /**
   * 供应商 id（A4 起**不再是用户输入**）。
   *
   * - 带来源采购单：**不要传** —— 服务端从 `biz_purchase_order.supplier_id` 派生；
   *   传了且与采购单不一致 → 400（不静默覆盖）；
   * - 无来源（期初 / 复制 / 其他）：**必填**，且必须同时 `is_sourceless = true`。
   */
  supplier_id?: string;
  /**
   * 无源单据标记（A4/AP-5）。无来源采购单时**必须显式 `true` + 填 `remark`**，
   * 否则后端 400 `66027`（无源要可枚举、能进巡检，不能靠空值隐式表达）。
   */
  is_sourceless?: boolean;
  /** 收货仓库 id（带源单时必须与采购单收货仓库一致） */
  warehouse_id: string;
  /** 入库日期 `YYYY-MM-DD` */
  in_date: string;
  /** 发票已随货到达（默认 `true`） */
  invoice_received?: boolean;
  /** 备注 */
  remark?: string | null;
  /** 明细 */
  items: StockInItemReq[];
}

/** 改单请求体（`PUT /erp/stock-in/{id}`，**仅 DRAFT**，整单覆盖明细） */
export type UpdateStockInReq = CreateStockInReq;

/** 过账时补序列号（`stockin_dto.rs::StockInLineSerials`） */
export interface StockInLineSerials {
  /** 行号（1 起） */
  line_no: number;
  /** 该行的序列号字符串 */
  serial_ids: string[];
}

/**
 * 状态流转请求体（`PATCH /erp/stock-in/{id}/status`）。
 *
 * - `POSTED`（过账）：同一事务里 锁单头 → CAS → 库存加法 + 流水 → 序列号 → 回写采购已收 → 推进 PO；
 *   重复过账 `409 + 10003`；
 * - `CANCELLED`：`POSTED → CANCELLED` **必须带 `reason`**，且**本批不冲回库存**（红冲属 P2-C1）。
 */
export interface StockInStatusReq {
  /** 目标状态 */
  status: 'POSTED' | 'CANCELLED';
  /** 作废原因（`POSTED → CANCELLED` 必填） */
  reason?: string | null;
  /** 一物一码的序列号（`serials[{line_no, serial_ids}]`；可省 = 系统自动取号） */
  serials?: StockInLineSerials[];
}

/** 收货入库分页（`GET /erp/stock-in/page`，服务端分页，list 元素是扁平 `StockInRow`） */
export function fetchStockInPage(query: {
  keyword?: string;
  status?: StockInStatus | '';
  supplier_id?: string;
  warehouse_id?: string;
  /** 来源采购单号（模糊匹配；卡面 O3「收货单列表按采购单号筛选」） */
  purchase_order_no?: string;
  date_from?: string;
  date_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<StockInRow>> {
  return get<PageData<StockInRow>>(`${BASE}/stock-in/page`, asParams(query));
}

/** 收货单详情（`GET /erp/stock-in/{id}`，注意 `header` 包了一层） */
export function fetchStockInDetail(id: string): Promise<StockInDetail> {
  return get<StockInDetail>(`${BASE}/stock-in/${id}`);
}

/** 建单（`POST /erp/stock-in`，金额只由服务端算，传了也会被忽略） */
export function createStockIn(payload: CreateStockInReq): Promise<StockInDetail> {
  return post<StockInDetail>(`${BASE}/stock-in`, payload);
}

/** 改单（`PUT /erp/stock-in/{id}`，**仅 DRAFT**，整单覆盖明细） */
export function updateStockIn(id: string, payload: UpdateStockInReq): Promise<StockInDetail> {
  return put<StockInDetail>(`${BASE}/stock-in/${id}`, payload);
}

/**
 * 复制为新单（D9：`POST /erp/stock-in/{id}/copy`，权限 = 建单权限）。
 *
 * 后端语义（别在前端重复实现）：新单号 + `DRAFT`，带出供应商 / 仓库 / 库位 / 明细 / 备注，
 * **采购单关联清空**（防待收额度被二次占用）、入库日期取今天、凭证/应付/审批关联全空。
 * 返回的是**新单的完整详情**，可直接喂给 `detailToForm` 打开编辑弹窗。
 */
export function copyStockIn(id: string): Promise<StockInDetail> {
  return post<StockInDetail>(`${BASE}/stock-in/${id}/copy`, {});
}

/** 过账 / 作废（`PATCH /erp/stock-in/{id}/status`） */
export function updateStockInStatus(
  id: string,
  payload: StockInStatusReq,
): Promise<StockInDetail> {
  return patch<StockInDetail>(`${BASE}/stock-in/${id}/status`, payload);
}

/**
 * 提交收货过账（`POST /erp/stock-in/{id}/submit-post`，P-FLOW-3A）。
 *
 * 配了已发布的 `stock_in_post` 流程时：`DRAFT → PENDING_POST`，走审批
 * （含税 < 1000 免审直接过账 + 知会仓管主管；≥ 1000 仓管主管审批），审批通过才真正过账。
 * 没配流程时后端降级为直接过账（与 P-FLOW-3A 之前一致）。
 *
 * 与 `PATCH /status` 走**同一个后端入口**：换 URL 绕不过审批。
 */
export function submitStockInPost(
  id: string,
  payload: Partial<StockInStatusReq> = {},
): Promise<StockInDetail> {
  return post<StockInDetail>(`${BASE}/stock-in/${id}/submit-post`, {
    status: 'POSTED',
    ...payload,
  });
}

/**
 * 重新过账（`POST /erp/stock-in/{id}/repost`，BUG-02）。
 *
 * 只对「审批已通过、但上次过账失败」的死单开放：单据 `PENDING_POST` + 审批实例 `APPROVED`。
 * 后端重跑**全部**过账闸（供应商复核 / 制单≠过账 / 单位复检 / 采购单状态与待收额度），
 * 成功 → `POSTED` 且六本账落库；失败 → 状态仍是 `PENDING_POST`（改完数据可以再点一次）。
 *
 * 注意：`PATCH /status` 走不通这条路（后端明确拒绝 `PENDING_POST → POSTED`，防绕过审批），
 * 这就是为什么要单独一个接口。
 */
export function repostStockIn(id: string): Promise<StockInDetail> {
  return post<StockInDetail>(`${BASE}/stock-in/${id}/repost`, {});
}

/**
 * 撤回收货单的作废申请（`POST /erp/stock-in/{id}/cancel-withdraw`，P16）。
 *
 * 只有**发起人**能撤（后端校验，其余人 403 + 65004）；单据 `PENDING_CANCEL → POSTED`，
 * 六本账一动没动。撤回成功后审批中心的待办同时被终止（后端同一事务里做）。
 */
export function withdrawStockInCancel(id: string): Promise<StockInDetail> {
  return post<StockInDetail>(`${BASE}/stock-in/${id}/cancel-withdraw`, {});
}

/** 删除（`DELETE /erp/stock-in/{id}`，**仅 DRAFT**，软删） */
export function deleteStockIn(id: string): Promise<void> {
  return del<void>(`${BASE}/stock-in/${id}`);
}

// ==================== 应付单（P2-C1 数据层 / P2-E2 前端） ====================

/**
 * 应付单状态（与 `backend/src/business/erp/payable.rs` 的 `STATUS_*` 逐字一致）。
 *
 * `PENDING_CANCEL`（作废审批中）是 P-FLOW-3B 新增的**真实状态**（V0191 补了三条边 +
 * 库约束 `ck_biz_payable_pending_cancel`）：`OPEN → PENDING_CANCEL`（送审）→
 * `CANCELLED`（通过）/ `OPEN`（驳回或撤回）。此状态下欠款一分没动，只是不能再付款 / 再作废。
 */
export type PayableStatus = 'OPEN' | 'PARTIAL_PAID' | 'SETTLED' | 'CANCELLED' | 'PENDING_CANCEL';

/**
 * 应付单行（字段逐字抄 `backend/src/business/erp/payable.rs` 的
 * `COLUMNS` 常量与 `PayableRow` 结构，列名以 V0148 为准）。
 *
 * # ⚠️ 查询接口尚未落地（P2-C1 的缺口，已写到看板评论）
 *
 * P2-C1 只交了**数据层**：收货过账时在同一个事务里写 `biz_payable`
 * （`payable::create_for_stock_in`），但**没有** HTTP 查询接口 ——
 * 实测 `grep -rn 'erp/payable' backend/src/` **0 命中**，
 * `backend/migrations/business/V0148__erp_payable.up.sql` 也**没有**种 `business:erp:payable:*` 权限码。
 *
 * 本层按架构文档 `docs/ERP-批次1-技术架构.md` 第 1888 行的契约
 * （`GET /business/erp/payable/page`，权限码 `business:erp:payable:list`）先落地，
 * 字段名照**库列**（不是文档里的示意），后端补接口即通，前端不用改。
 *
 * # 金额为什么是 `number | string`
 *
 * 后端金额是 `rust_decimal::Decimal`，serde 默认序列化成**字符串**（`"226.0000"`）；
 * 直接 `+` 会变成字符串拼接 —— 统一走 `payable-logic.ts::toNumber`。
 */
export interface PayableRow {
  /** 主键 */
  id: string;
  /** 应付单号 `AP-yyyyMMdd-000001`（`payable::NO_PREFIX`） */
  payable_no: string;
  /** 供应商 id（列表要译名，靠 `fetchSupplierOptions` 本地映射） */
  supplier_id: string;
  /** 来源单据类型（`STOCK_IN` …） */
  source_type: string;
  /** 来源单据 id */
  source_id: string;
  /** 来源单号（收货单号） */
  source_no: string;
  /** 发票号（批次 2 回填，本批恒 null） */
  invoice_no: string | null;
  /** 发票日期 */
  invoice_date: string | null;
  /** 币种 */
  currency_code: string;
  /** 应付金额（含税） */
  amount: number | string;
  /** 已付金额（本批只读，恒 0） */
  paid_amount: number | string;
  /** 未付金额（= amount − paid_amount，库列冗余） */
  unpaid_amount: number | string;
  /** 账期天数快照 */
  credit_days: number;
  /** 到期日 `YYYY-MM-DD` */
  due_date: string;
  /** 所属会计期间 `yyyy-MM` */
  period_code: string;
  /** 状态 */
  status: PayableStatus;
  /** 结清时间 */
  settled_at: string | null;
  /** 关联凭证 id（P2-C1 回写） */
  voucher_id: string | null;
  /** 备注 */
  remark: string | null;
  /**
   * 作废审批实例 id（P-FLOW-3B 补漏后**列表 + 详情都下发**）。
   *
   * `PENDING_CANCEL` 时必有值（库侧 `ck_biz_payable_pending_cancel`），撤回 / 审批结束后后端清成
   * `null`。详情抽屉据此显示「作废审批进度」——在此之前只能在**送审那一刻**用响应里的
   * `approval_instance_id` 显示，重新打开抽屉进度区就没了。
   */
  cancel_instance_id: string | null;
  /** 作废原因（P-FLOW-3B 补漏后**列表 + 详情都下发**）：送审时填的那段文字，不审批时是 `null`。 */
  cancel_reason: string | null;
  /** 创建时间 */
  created_at: string | null;
  /** 更新时间 */
  updated_at: string | null;
}

/**
 * 应付单分页（`GET /erp/payable/page`，服务端分页）。
 *
 * 入参照架构文档 1888 行：「供应商 / 状态 / 到期日区间 / 单号」。
 * 空串一律不发（`asParams` 已滤），`payable_no` 后端用 `LIKE %x%` 模糊匹配。
 */
export function fetchPayablePage(query: {
  status?: PayableStatus | '';
  supplier_id?: string;
  payable_no?: string;
  due_date_from?: string;
  due_date_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<PayableRow>> {
  return get<PageData<PayableRow>>(`${BASE}/payable/page`, asParams(query));
}

// ==================== 凭证（P2-D1 只读接口） ====================

/** 凭证状态白名单（`voucher_query::STATUSES` + `ck_biz_voucher_status`） */
export type VoucherStatus = 'POSTED' | 'DRAFT' | 'AUDITED' | 'CANCELLED';

/** 凭证类型（`voucher_query::VOUCHER_TYPES`）：AUTO = 过账自动生成，MANUAL = 手工录 */
export type VoucherType = 'AUTO' | 'MANUAL';

/**
 * 凭证（列表行，逐字抄 `voucher_query.rs::VoucherRes`）。
 *
 * `status_label` / `voucher_type_label` 后端已给中文，前端本地映射只用于
 * 单测与颜色（后端标签改了，前端不至于显示空白）。
 */
export interface VoucherRow {
  /** 主键 */
  id: string;
  /** 凭证字（默认「记」） */
  voucher_word: string;
  /** 凭证号 `PZ-yyyyMMdd-000001` */
  voucher_no: string;
  /** 会计期间 `yyyy-MM` */
  period: string;
  /** 凭证日期 `YYYY-MM-DD` */
  voucher_date: string;
  /** 凭证类型 */
  voucher_type: VoucherType;
  /** 凭证类型中文（后端给：自动 / 手工） */
  voucher_type_label: string;
  /** 来源单据类型（`STOCK_IN` / `MANUAL` …） */
  source_type: string;
  /** 来源单据 id */
  source_id: string | null;
  /** 来源单据号（记账依据 / 子单据号） */
  source_no: string | null;
  /**
   * 业务单据号（`biz_voucher.source_doc_no`）。
   *
   * 退货凭证这里是**退货单号**（`source_no` 则是入库单 / 红字发票号）：
   * 「一张业务单 → 多张凭证」的查询键，绝大多数单据两列同值。
   */
  source_doc_no: string | null;
  /** 摘要 */
  summary: string;
  /** 借方合计 */
  total_debit: number | string;
  /** 贷方合计 */
  total_credit: number | string;
  /** 分录行数 */
  entry_count: number;
  /** 状态 */
  status: VoucherStatus;
  /** 状态中文（后端给：已记账 / 草稿 / 已审核 / 已作废） */
  status_label: string;
  /** 是否红字凭证（P2-C1：借贷互换 + `is_red=1`） */
  is_red: boolean;
  /** 被红冲的原凭证 id */
  reverse_of: string | null;
  /** 红冲它的凭证 id */
  reversed_by: string | null;
  /** 记账时间 */
  posted_at: string | null;
  /** 记账人 */
  posted_by: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
}

/**
 * 凭证分录（逐字抄 `voucher_query.rs::VoucherEntryRes`）。
 *
 * `account_code` / `account_name` 是**分录行上的快照**（`biz_voucher_entry` 两列 NOT NULL），
 * 直接展示，不要再去 JOIN 科目表（科目改名后历史凭证仍要显示当时的名字）。
 */
export interface VoucherEntryLine {
  /** 主键 */
  id: string;
  /** 行号（详情按它升序） */
  line_no: number;
  /** 摘要 */
  summary: string;
  /** 科目 id */
  account_id: string;
  /** 科目编码（快照） */
  account_code: string;
  /** 科目名称（快照） */
  account_name: string;
  /** 借方金额 */
  debit_amount: number | string;
  /** 贷方金额 */
  credit_amount: number | string;
  /** 辅助核算：供应商 */
  aux_supplier_id: string | null;
  /** 辅助核算：物料 */
  aux_material_id: string | null;
  /** 辅助核算：仓库 */
  aux_warehouse_id: string | null;
  /** 辅助核算：部门 */
  aux_dept_id: string | null;
  /** 辅助核算：数量 */
  aux_qty: number | string | null;
}

/**
 * 凭证详情（`GET /erp/voucher/{id}`，`VoucherDetailRes`：单头 flatten + 全部分录）。
 *
 * `balanced` 恒为 `true` —— 分录合计与单头不等时后端直接 `500 + 66009`，
 * 根本走不到前端（所以界面上它是「后端结论」，而借贷平衡的**可视化**仍由
 * `voucher-logic.ts::sumEntries` 在前端重算一遍，两边对不上要能看出来）。
 */
export interface VoucherDetail extends VoucherRow {
  /** 全部分录（`line_no` 升序） */
  entries: VoucherEntryLine[];
  /** 分录借方合计 */
  entry_total_debit: number | string;
  /** 分录贷方合计 */
  entry_total_credit: number | string;
  /** 单头与分录是否一致（后端结论） */
  balanced: boolean;
}

/** 凭证分页（`GET /erp/voucher/page`，默认**全部状态**，列表是账务核对入口） */
export function fetchVoucherPage(query: {
  period?: string;
  status?: VoucherStatus | '';
  voucher_type?: VoucherType | '';
  source_type?: string;
  /** 业务单据号（**精确**）：退货单号等「一张业务单 → 多张凭证」的查询键 */
  source_doc_no?: string;
  voucher_no?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<VoucherRow>> {
  return get<PageData<VoucherRow>>(`${BASE}/voucher/page`, asParams(query));
}

/** 凭证详情（`GET /erp/voucher/{id}`，含全部分录） */
export function fetchVoucherDetail(id: string): Promise<VoucherDetail> {
  return get<VoucherDetail>(`${BASE}/voucher/${id}`);
}

/**
 * 按来源单据反查凭证（`GET /erp/voucher/by-source?source_type=&source_id=`，VTRACE-3）。
 *
 * 回答的是**「这张业务单据产生过哪些凭证」** —— 单据详情页「查看凭证 (N)」按钮的唯一读数。
 *
 * # 后端口径（`voucher_query.rs::voucher_by_source_handler`，逐条对上）
 *
 * - 返回**该单据产生的全部凭证**，红字凭证（`is_red = true`）也在内，按
 *   `posted_at DESC NULLS LAST, created_at DESC, voucher_no DESC` 排（最新一笔在最前）；
 * - **空结果返回 `[]`，不是 404**：没生成过凭证是正常业务状态（未过账 / 只建单），
 *   前端据此决定按钮显示与否；
 * - `source_type` 必须在 `VoucherSourceType` 值域内（非法值 **400 + 10001**），
 *   `source_id` 非 uuid → **400 + 66006**；
 * - 权限 `business:erp:voucher:list`（与凭证列表同一个码）。
 *
 * @param sourceType `biz_voucher.source_type`（`STOCK_IN` / `STOCK_OUT` / `EXPENSE_REPORT` …）
 * @param sourceId   `biz_voucher.source_id`（对应业务单据主键）
 */
export function fetchVouchersBySource(
  sourceType: string,
  sourceId: string,
): Promise<VoucherRow[]> {
  return get<VoucherRow[]>(
    `${BASE}/voucher/by-source`,
    asParams({ source_type: sourceType, source_id: sourceId }),
  );
}

// ==================== 总账 / 科目余额表（P2-D1 只读接口） ====================

/**
 * 会计科目（逐字抄 `dto.rs::AccountRes`）。
 *
 * 总账 Tab1 的「科目」下拉与 Tab2 的译名都读它；`level` / `is_leaf` 决定下拉是否缩进。
 */
export interface AccountRow {
  /** 主键 */
  id: string;
  /** 科目编码（`1405` / `2221.01`） */
  account_code: string;
  /** 科目名称 */
  account_name: string;
  /** 上级科目 */
  parent_id: string | null;
  /** 级次 */
  level: number;
  /** 类别（`ASSET` / `LIABILITY` …） */
  account_category: string;
  /** 余额方向（`DEBIT` / `CREDIT`） */
  balance_direction: string;
  /** 是否末级 */
  is_leaf: boolean;
  /** 辅助核算维度 */
  aux_dimensions: string | null;
  /** 启用状态：1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 科目下拉候选（一次拉够用；上千条应走远端搜索，那是后续卡的事） */
export function fetchAccountOptions(limit = 500): Promise<AccountRow[]> {
  return get<PageData<AccountRow>>(
    `${BASE}/account/page`,
    asParams({ status: 1, page: 1, page_size: limit }),
  ).then((page) => page.list ?? []);
}

/**
 * 总账流水行（逐字抄 `ledger.rs::LedgerRowRes`）。
 *
 * ⚠️ `running_balance` 是**本页内**的逐行余额（不含期初）：
 * 第一行 = 本页之前所有行的净额合计 + 本行；要看含期初的余额请用科目余额表的
 * `opening_*` / `closing_*`。
 */
export interface LedgerRow {
  /** 凭证 id */
  voucher_id: string;
  /** 凭证日期 `YYYY-MM-DD` */
  voucher_date: string;
  /** 凭证字 */
  voucher_word: string;
  /** 凭证号 */
  voucher_no: string;
  /** 分录行号 */
  line_no: number;
  /** 摘要（分录摘要） */
  summary: string;
  /** 科目编码（分录快照） */
  account_code: string;
  /** 科目名称（分录快照） */
  account_name: string;
  /** 借方发生额 */
  debit_amount: number | string;
  /** 贷方发生额 */
  credit_amount: number | string;
  /** 逐行余额（净额累加结果） */
  running_balance: number | string;
  /** 是否红字凭证 */
  is_red: boolean;
}

/**
 * 科目流水分页（`GET /erp/ledger/page`）。
 *
 * `account_id` / `account_code` **必须给一个**（两个都不给 → 400 + 10001）；
 * 期间既可以是 `yyyy-MM`（起始取月首、结束取月末），也可以是 `yyyy-MM-dd`。
 */
export function fetchLedgerPage(query: {
  account_id?: string;
  account_code?: string;
  period_from?: string;
  period_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<LedgerRow>> {
  return get<PageData<LedgerRow>>(`${BASE}/ledger/page`, asParams(query));
}

/** 科目余额表的一行（逐字抄 `ledger.rs::TrialBalanceRow`） */
export interface TrialBalanceRow {
  /** 科目 id */
  account_id: string;
  /** 科目编码 */
  account_code: string;
  /** 科目名称 */
  account_name: string;
  /** 科目类别 */
  account_category: string;
  /** 余额方向 */
  balance_direction: string;
  /** 期初借方余额 */
  opening_debit: number | string;
  /** 期初贷方余额 */
  opening_credit: number | string;
  /** 本期借方发生额 */
  period_debit: number | string;
  /** 本期贷方发生额 */
  period_credit: number | string;
  /** 期末借方余额 */
  closing_debit: number | string;
  /** 期末贷方余额 */
  closing_credit: number | string;
}

/**
 * 科目余额表（`GET /erp/ledger/trial-balance`，`TrialBalanceRes`）。
 *
 * 后端已把 Σ借 / Σ贷 算好并在**不平的时候 500**（`66009`），
 * 所以正常响应里 `balanced` 恒为 `true`；前端仍会把 `rows` 重新加一遍，
 * 两套数字对不上要能在界面上看出来（防「后端一笔算错、汇总却自洽」）。
 */
export interface TrialBalance {
  /** 会计期间 `yyyy-MM` */
  period: string;
  /** 期间起始日 */
  period_start: string;
  /** 期间结束日 */
  period_end: string;
  /** 明细行（按科目编码升序，**含全部未删除科目**，没发生额的是全 0 行） */
  rows: TrialBalanceRow[];
  /** 期初借方合计 */
  total_opening_debit: number | string;
  /** 期初贷方合计 */
  total_opening_credit: number | string;
  /** 本期借方合计 */
  total_period_debit: number | string;
  /** 本期贷方合计 */
  total_period_credit: number | string;
  /** 期末借方合计 */
  total_closing_debit: number | string;
  /** 期末贷方合计 */
  total_closing_credit: number | string;
  /** 借贷是否平衡（后端结论） */
  balanced: boolean;
}

/** 科目余额表（`GET /erp/ledger/trial-balance`，`period` 必填 `yyyy-MM`） */
export function fetchTrialBalance(period: string): Promise<TrialBalance> {
  return get<TrialBalance>(`${BASE}/ledger/trial-balance`, asParams({ period }));
}

// ==================== ERP 主数据 CRUD（P4-A：计量单位 / 供应商） ====================

/**
 * 计量单位（完整 `UnitRes`，逐字抄 `dto.rs::UnitRes`，实打实 curl 过线上接口核对）。
 *
 * # 与 `UnitRow` 的关系
 *
 * `UnitRow` 是物料页只要「id + 名字」时的**子集**，字段名逐字一致；
 * 本页要显示折算率 / 折算基准 / 备注，所以在它上面扩成完整形状，不另起一套字段名
 * （另起一套 = 同一张表两个类型，早晚漂移）。
 *
 * # 「小数位数」与「系统预置」（ERP-S1 / V0187 补的）
 *
 * V0187 给 `biz_unit` 加了两列：`precision`（数量小数位数 0–6）与 `is_system`
 * （系统预置标记）。卡面草稿当初写的「小数位数」这一段历史注释已作废：
 * 现在后端**确实**有这个字段，列表照字段显示，不许再造假列。 */
export interface Unit extends UnitRow {
  /** 折算基准单位（`null` = 基础单位） */
  base_unit_id: string | null;
  /** 折算率：1 个本单位 = `rate` 个基准单位（`Decimal` 以字符串下发，如 `"12.000000"`） */
  rate: number | string;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string | null;
  /** 是否系统预置（预置值不能删，只能停用） */
  is_system: boolean;
  /** 数量小数位数（0–6） */
  precision: number;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 新建计量单位（`dto::CreateUnitReq`；`is_base=false` 时 `base_unit_id` 与 `rate` 必填） */
export interface CreateUnitReq {
  /** 单位编码（≤20，未删除内唯一） */
  unit_code: string;
  /** 单位名称（≤50） */
  unit_name: string;
  /** 是否基础单位（后端缺省 true） */
  is_base?: boolean;
  /** 折算基准单位（`is_base=true` 时**不能**填，否则 400） */
  base_unit_id?: string | null;
  /** 折算率（`is_base=true` 时只能是 1） */
  rate?: number | string | null;
  /** 启用状态：1 / 0 */
  status?: number;
  /** 排序值 */
  sort_order?: number;
  /** 备注 */
  remark?: string | null;
  /** 数量小数位数（0–6，缺省 0；ERP-S1 V0187） */
  precision?: number;
}

/**
 * 编辑计量单位（`dto::UpdateUnitReq`）。
 *
 * 缺席 = 不改；`base_unit_id: null` = **清空折算基准 → 变成基础单位**（后端会把 rate 归 1），
 * 这是 `double_option` 语义，页面只在用户真改了才带。
 */
export interface UpdateUnitReq {
  /** 单位名称 */
  unit_name?: string;
  /** 折算基准单位（`null` = 清空 → 基础单位） */
  base_unit_id?: string | null;
  /** 折算率 */
  rate?: number | string;
  /** 启用状态 */
  status?: number;
  /** 排序值 */
  sort_order?: number;
  /** 备注（`null` = 清空） */
  remark?: string | null;
  /** 数量小数位数（0–6；缺席 = 不改；ERP-S1 V0187） */
  precision?: number;
}

/**
 * 计量单位分页（`GET /erp/unit/page`，服务端分页）。
 *
 * 权限码 `business:erp:unit:list`；list 元素是完整 `UnitRes`（**不是** `UnitCard`，
 * 后端 `unit_page` 映射的是 `UnitRes::from`）。
 */
export function fetchUnitPage(query: {
  keyword?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Unit>> {
  return get<PageData<Unit>>(`${BASE}/unit/page`, asParams(query));
}

/** 计量单位详情（`GET /erp/unit/{id}`，也是 `:list` 权限） */
export function fetchUnitDetail(id: string): Promise<Unit> {
  return get<Unit>(`${BASE}/unit/${id}`);
}

/** 新建计量单位（`POST /erp/unit`，权限 `business:erp:unit:create`，回参是整个 `UnitRes`） */
export function createUnit(payload: CreateUnitReq): Promise<Unit> {
  return post<Unit>(`${BASE}/unit`, payload);
}

/** 编辑计量单位（`PUT /erp/unit/{id}`，权限 `business:erp:unit:update`） */
export function updateUnit(id: string, payload: UpdateUnitReq): Promise<Unit> {
  return put<Unit>(`${BASE}/unit/${id}`, payload);
}

/**
 * 删除计量单位（`DELETE /erp/unit/{id}`，软删，权限同样是 `:update`）。
 *
 * 被物料引用时后端返回 **409 + 60005**（「还有 N 个物料在用该单位，不能删除」），
 * 页面要把 `ApiError.message` 原样弹给用户 —— 别自己编「删除失败」。
 */
export function deleteUnit(id: string): Promise<void> {
  return del<void>(`${BASE}/unit/${id}`);
}

/**
 * 编码预览响应（`GET /business/erp/{supplier,customer}/next-code`）。
 *
 * 后端 `preview_*_code` 只读号段表（不占号），所以同一个号可以反复出现在这里；
 * 真到保存时号被别人抢走 → `409 + 66001`，提示用户重新生成即可。
 */
export interface NextCodePreview {
  /** 下一个可用编码（如 `SUP-0003` / `CUS-0005`） */
  code: string;
}

/**
 * 主数据编码策略（`GET /business/erp/master-data/code-policy`，CODE-08）。
 *
 * 系统参数 `erp.master_data.allow_manual_code` 的对外投影：由它决定 6 个主数据表单的
 * 编码栏是「可填 + 有生成编码按钮」还是「只读 + 没有按钮」。
 *
 * 后端在**建单入口**也按同一条参数把关（关闭时手工编码 `400 + 66054`），所以前端拿不到
 * 这个接口时按 `default_allow_manual_code`（= 允许）渲染不影响正确性 —— 最坏情况是用户
 * 看到输入框却提交不了，错误信息会说明原因。
 */
export interface MasterDataCodePolicy {
  /** 生效值：`true` = 可手填企业自有编码；`false` = 编码完全由系统生成 */
  allow_manual_code: boolean;
  /** 参数键（排查用，如 `erp.master_data.allow_manual_code`） */
  config_key: string;
  /** 代码默认值（参数缺失 / 接口不可用时前端的兜底口径） */
  default_allow_manual_code: boolean;
}

/**
 * 读主数据编码策略（登录即可，不挂权限码）。
 *
 * 六个主数据表单共用一条策略，所以调用方一律经
 * `@/views/business/erp/shared/code-policy` 的 `loadAllowManualCode()`（带进程内缓存），
 * 不要在页面里直接调本函数。
 */
export function fetchMasterDataCodePolicy(): Promise<MasterDataCodePolicy> {
  return get<MasterDataCodePolicy>(`${BASE}/master-data/code-policy`);
}

/** 新建供应商（`dto::CreateSupplierReq`，字段与 `biz_supplier` 列逐字一致） */
export interface CreateSupplierReq {
  /**
   * 编码（≤40，未删除内唯一）。
   *
   * **留空（`null` / `''` / 纯空白）⇒ 后端自动取号 `SUP-0001` 形态**（CODE-02）；
   * 有值则原样保留（企业自有编码体系如 `GYS-001` 要能对接）。
   */
  supplier_code: string | null;
  /** 名称（≤200） */
  supplier_name: string;
  /** 简称（≤100） */
  short_name?: string | null;
  /** 联系人（≤50） */
  contact_person?: string | null;
  /** 联系电话（≤30） */
  contact_phone?: string | null;
  /** 邮箱（≤120） */
  email?: string | null;
  /** 纳税人识别号（≤50） */
  tax_no?: string | null;
  /** 地址（≤255） */
  address?: string | null;
  /** 账期天数（≥0，缺省 0） */
  credit_days?: number;
  /** 信用额度（≥0，缺省 0） */
  credit_limit?: number | string | null;
  /** 应付科目（会计科目 id） */
  payable_account_id?: string | null;
  /** 开票抬头（≤200） */
  invoice_title?: string | null;
  /** 开户行（≤200） */
  invoice_bank_name?: string | null;
  /** 银行账号（≤64） */
  invoice_bank_account?: string | null;
  /** 启用状态：1 / 0 */
  status?: number;
  /** 备注 */
  remark?: string | null;
}

/** 编辑供应商（`dto::UpdateSupplierReq`；缺席 = 不改，`null` = 清空） */
export interface UpdateSupplierReq {
  /** 名称 */
  supplier_name?: string;
  /** 简称 */
  short_name?: string | null;
  /** 联系人 */
  contact_person?: string | null;
  /** 联系电话 */
  contact_phone?: string | null;
  /** 邮箱 */
  email?: string | null;
  /** 纳税人识别号 */
  tax_no?: string | null;
  /** 地址 */
  address?: string | null;
  /** 账期天数 */
  credit_days?: number;
  /** 信用额度 */
  credit_limit?: number | string;
  /** 应付科目（`null` = 清空） */
  payable_account_id?: string | null;
  /** 开票抬头 */
  invoice_title?: string | null;
  /** 开户行 */
  invoice_bank_name?: string | null;
  /** 银行账号 */
  invoice_bank_account?: string | null;
  /** 启用状态 */
  status?: number;
  /** 备注（`null` = 清空） */
  remark?: string | null;
}

/**
 * 供应商分页（`GET /erp/supplier/page`，服务端分页）。
 *
 * 权限码 `business:erp:supplier:list`；list 元素是完整 `SupplierRes`（含 `tax_no` / `email` 等，
 * 不是只读下拉用的 `SupplierCard` 子集），所以列表页能直接出「税号」列。
 */
export function fetchSupplierPage(query: {
  keyword?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Supplier>> {
  return get<PageData<Supplier>>(`${BASE}/supplier/page`, asParams(query));
}

/** 供应商详情（`GET /erp/supplier/{id}`，也是 `:list` 权限） */
export function fetchSupplierDetail(id: string): Promise<Supplier> {
  return get<Supplier>(`${BASE}/supplier/${id}`);
}

/**
 * 供应商编码预览（`GET /business/erp/supplier/next-code`，权限 `business:erp:supplier:create`）。
 *
 * **只读、不占号**（交互方式 B 的方案 B）：连点两次拿到同一个号，点了又取消不会浪费流水；
 * 并发下两个人可能看到同一个号 —— 保存时被抢走后端回 `409 + 66001`，提示重新生成即可。
 *
 * 注意路径写全 `/api`（`${BASE}` 已含），漏前缀会命中 SPA fallback 返回 HTML。
 */
export function fetchSupplierNextCode(): Promise<NextCodePreview> {
  return get<NextCodePreview>(`${BASE}/supplier/next-code`);
}

/** 新建供应商（`POST /erp/supplier`，权限 `business:erp:supplier:create`） */
export function createSupplier(payload: CreateSupplierReq): Promise<Supplier> {
  return post<Supplier>(`${BASE}/supplier`, payload);
}

/** 编辑供应商（`PUT /erp/supplier/{id}`，权限 `business:erp:supplier:update`） */
export function updateSupplier(id: string, payload: UpdateSupplierReq): Promise<Supplier> {
  return put<Supplier>(`${BASE}/supplier/${id}`, payload);
}

/**
 * 删除供应商（`DELETE /erp/supplier/{id}`，软删，权限同样是 `:update`）。
 *
 * 被采购单引用时后端返回 **409**，`ApiError.message` 原样弹给用户。
 */
export function deleteSupplier(id: string): Promise<void> {
  return del<void>(`${BASE}/supplier/${id}`);
}

// ==================== 财务基础页：会计科目 / 会计期间（P4-C，只读） ====================

/**
 * 会计科目类别（逐字抄 `backend/migrations/business/V0141` 的
 * `ck_biz_account_category` CHECK 约束：5 个字面量，**损益是 `PROFIT_LOSS` 不是 `PROFIT`**）。
 */
export type AccountCategory = 'ASSET' | 'LIABILITY' | 'EQUITY' | 'COST' | 'PROFIT_LOSS';

/** 余额方向（`ck_biz_account_direction`：`DEBIT|CREDIT`） */
export type BalanceDirection = 'DEBIT' | 'CREDIT';

/**
 * 会计科目分页（`GET /erp/account/page`，服务端分页）。
 *
 * 入参逐字抄 `dto.rs::AccountFilter`：`keyword`（编码/名称 LIKE）、`account_category`、
 * `is_leaf`（`Some(true)` 只看末级）、`parent_id`、`status`（1 启用 / 0 停用）。
 * 空串一律不发（`asParams` 已滤；后端把 `?keyword=` 当「等于空串」查 0 行）。
 *
 * 树形页面一次把整棵树拉下来（`page_size` 给大值）再在前端按 `parent_id` 拼树 ——
 * 后端没有「树接口」，也不该为 4 条科目造一个。
 */
export function fetchAccountPage(query: {
  keyword?: string;
  account_category?: AccountCategory | '';
  is_leaf?: boolean;
  parent_id?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<AccountRow>> {
  return get<PageData<AccountRow>>(`${BASE}/account/page`, asParams(query));
}

/**
 * 会计期间状态（逐字抄 `ck_biz_period_status`：`NOT_OPENED|OPEN|CLOSED`）。
 *
 * 注意：**没有** `未结账` 这种中间态，结账走 `OPEN → CLOSED` 一步；
 * `NOT_OPENED` 是「尚未开启」，不是「已关闭」。
 */
export type PeriodStatus = 'NOT_OPENED' | 'OPEN' | 'CLOSED';

/**
 * 报表侧会出现第 4 个状态：**`DERIVED`**（库里没有期间行、但该月有已记账凭证时，
 * 后端按自然月推算并在 `check.hints` 里要求尽快开账）。它不是 `biz_accounting_period`
 * 里存得下的值（库里有 `ck_biz_period_status`），只出现在报表响应里。
 */
export type ReportPeriodStatus = PeriodStatus | 'DERIVED';

/** 会计期间（逐字抄 `dto.rs::PeriodRes`） */
export interface AccountPeriodRow {
  /** 主键 */
  id: string;
  /** 年份（如 2026） */
  period_year: number;
  /** 月份（1..12） */
  period_month: number;
  /** 期间编码 `yyyy-MM`（如 `2026-10`，库列 VARCHAR(7)） */
  period_code: string;
  /** 开始日 `YYYY-MM-DD` */
  start_date: string;
  /** 结束日 `YYYY-MM-DD` */
  end_date: string;
  /** 状态 */
  status: PeriodStatus;
  /** 结账时间（`CLOSED` 后才有值） */
  closed_at: string | null;
  /** 结账人 */
  closed_by: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 会计期间分页（`GET /erp/period/page`，服务端分页）。
 *
 * 入参逐字抄 `dto.rs::PeriodFilter`：`period_year` + `status`，**没有关键词**（期间编码就是年月）。
 */
export function fetchPeriodPage(query: {
  period_year?: number;
  status?: PeriodStatus | '';
  page?: number;
  page_size?: number;
}): Promise<PageData<AccountPeriodRow>> {
  return get<PageData<AccountPeriodRow>>(`${BASE}/period/page`, asParams(query));
}

// ==================== 会计期间：开账 / 结账 / 反结账（ERP-P1） ====================
//
// 三个权限码（`business:erp:period:{open,close,reopen}`）与页面常量放在
// `views/business/erp/period/period-logic.ts`（纯逻辑、可被 `tsx --test` 单测），
// 这里不重复一份 —— 两个地方各写一遍，早晚会漂。

/** 开账入参（逐字抄 `period.rs::OpenPeriodReq`） */
export interface OpenPeriodPayload {
  /** 会计年度（2000–2999） */
  period_year: number;
  /** 会计月份（1–12） */
  period_month: number;
  /** 备注（可选，落 `biz_accounting_period.remark`） */
  remark?: string;
}

/** 批量开账入参（逐字抄 `period.rs::BatchOpenReq`；`from` / `to` 均 `YYYY-MM`，含两端） */
export interface BatchOpenPeriodPayload {
  /** 起始期间，如 `2026-11` */
  from: string;
  /** 结束期间，如 `2027-12` */
  to: string;
  /** 备注（可选） */
  remark?: string;
}

/** 批量开账结果（逐字抄 `period.rs::BatchOpenRes`） */
export interface BatchOpenPeriodResult {
  /** 本次新建的期间 */
  created: AccountPeriodRow[];
  /** 已存在而跳过的期间编码（幂等：重复批量开账不会 409） */
  skipped: string[];
  /** 新建条数 */
  created_count: number;
  /** 跳过条数 */
  skipped_count: number;
}

/**
 * 结账阻塞项（逐字抄 `period.rs::Blocker`）。
 *
 * `type` 决定前端图标与跳转：
 * - `DRAFT_VOUCHER`：有草稿凭证（`link` 指向凭证列表，带 `status=DRAFT`）
 * - `DOC_UNFINISHED`：有未完成单据（采购单 / 收货单 / 调拨单）
 * - `UNBALANCED`：借贷不平（`diff` 是差额，定点字符串）
 * - `INVARIANT`：不变量未通过（账实 / 账账不符）
 * - `PERIOD_STATUS`：期间自身状态不允许（已结账 / 未开账）
 */
export interface PeriodCloseBlocker {
  type: 'DRAFT_VOUCHER' | 'DOC_UNFINISHED' | 'UNBALANCED' | 'INVARIANT' | 'PERIOD_STATUS';
  /** 条数（凭证数 / 单据数 / 不变量条数） */
  count?: number;
  /** 差额（仅 `UNBALANCED`） */
  diff?: string;
  /** 给人看的一句话（后端已点名张数 / 差额） */
  message: string;
  /** 去处理的页面 */
  link?: string;
  /** 定位线索（凭证号 / 单号，最多 3 个） */
  samples?: string[];
  /**
   * ★ 挡路单据明细（PERIOD-CLOSE，后端 `period_items::BlockingItem`，最多 50 条）。
   *
   * 空数组 / 缺省 = 这一类没有单据级明细（例如期间自身状态不对），
   * 前端退回「一句话 + 去看看」的老样式（backward compat）。
   */
  items?: PeriodBlockingItem[];
}

/**
 * 一条挡路单据的明细（逐字抄 `period_items::BlockingItem`）。
 *
 * `doc_id` 是单据主键（催办接口按它选中「催哪几张」）；`route` 是带 id 的前端路由，
 * 「去处理」直接跳过去；`owner_role` 是**角色码**（前端译成中文），
 * `owner_name` 只在「该角色恰好一个启用用户」时有值。
 */
export interface PeriodBlockingItem {
  /** 单据类型：`VOUCHER` / `STOCK_IN` / `PURCHASE_ORDER` / `STOCK_TRANSFER` / `INVENTORY` */
  doc_type: string;
  /** 单据主键（uuid 字符串） */
  doc_id: string;
  /** 单据号（`PZ-…` / `IN-…` / `PO-…` / `TR-…`） */
  doc_no: string;
  /** 当前状态（中文标签，后端已翻译） */
  status_label: string;
  /** 金额（定点字符串；不是所有单据都有） */
  amount?: string | number | null;
  /** 制单人（只作参考，不作处理人） */
  created_by_name?: string | null;
  /** 下一步该谁处理（角色码；缺省 = 待分配） */
  owner_role?: string | null;
  /** 具体处理人（只在唯一用户时有值） */
  owner_name?: string | null;
  /** 前端动作文案：「去过账」/「去审批」/「去收货」 */
  action_label: string;
  /** 前端路由（带 id 直达） */
  route: string;
  /** 卡了多久的起点（ISO 时间戳） */
  waiting_since?: string | null;
}

/** 催办入参（逐字抄 `period_remind::RemindReq`） */
export interface PeriodRemindPayload {
  /** 催哪一类（缺省 / 空 = 该期间全部有处理人的明细） */
  blocker_kind?: string | null;
  /** 催哪几张（缺省 / 空 = 该类全部） */
  item_ids?: string[];
}

/** 催办结果（逐字抄 `period_remind::RemindRes`） */
export interface PeriodRemindResult {
  /** 发出去的条数 */
  sent: number;
  /** 发送失败条数（可重试） */
  failed: number;
  /** 没有处理人而跳过 */
  skipped_no_owner: number;
  /** 24 小时内已催过而跳过 */
  skipped_recent: number;
  /** 命中的明细条数 */
  total: number;
  /** 一句给人看的结论 */
  message: string;
}

/** 结账预检响应（逐字抄 `period.rs::CloseCheckRes`） */
export interface PeriodCloseCheck {
  /** 能不能结账（= `blockers` 为空） */
  can_close: boolean;
  /** 期间本身 */
  period: AccountPeriodRow;
  /** 阻塞项（空 = 可以结账） */
  blockers: PeriodCloseBlocker[];
  /**
   * 全库巡检告警（B4 库存流水累计 ≠ 当前库存 / C1 C2 负库存）。
   *
   * **不拦结账**：这些是「整本账」的时点状态，任何一处库存漂移都会封死所有期间的
   * 结账，所以后端把它们与 `blockers` 分开（见 `period.rs::global_health_warnings`）。
   * 前端只做灰字提示，让人封账时心里有数。
   */
  warnings?: PeriodCloseBlocker[];
}

/**
 * 开账（单月）。
 *
 * `POST /erp/period`，权限 `business:erp:period:open`。
 * 已存在且状态为 `OPEN` / `CLOSED` → 409（文案点名期间）。
 */
export function openPeriod(payload: OpenPeriodPayload): Promise<AccountPeriodRow> {
  return post<AccountPeriodRow>(`${BASE}/period`, payload);
}

/**
 * 批量开账（`from` ~ `to` 逐月，幂等：已存在的跳过而不是 409）。
 *
 * `POST /erp/period/batch-open`，权限 `business:erp:period:open`。
 * 一次最多 120 个月（后端护栏），超了 400。
 */
export function batchOpenPeriods(payload: BatchOpenPeriodPayload): Promise<BatchOpenPeriodResult> {
  return post<BatchOpenPeriodResult>(`${BASE}/period/batch-open`, payload);
}

/**
 * 结账预检（只读）。
 *
 * `GET /erp/period/{id}/close-check`，权限 `business:erp:period:close`。
 * 结账弹窗先调它，把「3 张草稿凭证 / 借贷差 0.01 元」摆出来，**再**让人点确认。
 */
export function fetchPeriodCloseCheck(id: string): Promise<PeriodCloseCheck> {
  return get<PeriodCloseCheck>(`${BASE}/period/${encodeURIComponent(id)}/close-check`);
}

/**
 * 结账。
 *
 * `POST /erp/period/{id}/close`，权限 `business:erp:period:close`。
 * 四项前置校验任一不过 → 409（文案点名张数 / 差额），期间状态不变。
 */
export function closePeriod(id: string, reason?: string): Promise<AccountPeriodRow> {
  return post<AccountPeriodRow>(`${BASE}/period/${encodeURIComponent(id)}/close`, {
    reason: reason ?? null,
  });
}

/**
 * 反结账（`CLOSED → OPEN`）。
 *
 * `POST /erp/period/{id}/reopen`，权限 `business:erp:period:reopen`。
 * `reason` **必填且 ≥ 10 字**（后端硬校验，前端也先拦一次，少一次无用的往返）。
 */
export function reopenPeriod(id: string, reason: string): Promise<AccountPeriodRow> {
  return post<AccountPeriodRow>(`${BASE}/period/${encodeURIComponent(id)}/reopen`, { reason });
}

/**
 * 催办：把挡路单据的待办发给处理人（PERIOD-CLOSE）。
 *
 * `POST /erp/period/{id}/remind`，权限 `business:erp:period:remind`。
 *
 * 收件人**在服务端现算**（不信任前端传的收件人）：请求只带「催哪一类 + 催哪几张」，
 * 后端重新跑一遍结账预检再按明细的 owner 发通知；同一张单 24 小时内不重复催。
 *
 * 返回 `{ sent, failed, skipped_no_owner, skipped_recent, total, message }`，
 * `message` 是可以直接 toast 的一句话。
 */
export function remindPeriod(id: string, payload: PeriodRemindPayload): Promise<PeriodRemindResult> {
  return post<PeriodRemindResult>(`${BASE}/period/${encodeURIComponent(id)}/remind`, payload);
}

// ==================== 即时库存 / 库位库存 / 库存流水（P4-D 前端） ====================

/**
 * 即时库存行（`GET /erp/inventory/page`，逐字抄 `dto.rs::InventoryRes`）。
 *
 * # 字段口径（硬红线：列名是 `qty`，不是 `quantity`）
 *
 * | 卡面草稿 | 真字段 | 说明 |
 * |---|---|---|
 * | 数量 | `qty` | 账面数量 `NUMERIC(18,4)`，**含**锁定量 |
 * | 锁定 | `locked_qty` | **预留字段**（本版本无占用流程，恒为 0；页面已不显示该列） |
 * | 可用 | —— | 接口**不给**，前端算 `qty − locked_qty`（见 `inventory-logic.ts`） |
 * | 均价 | `avg_cost` | 移动加权 `NUMERIC(18,6)` |
 * | 金额 | `amount` | 账面金额 `NUMERIC(18,4)` |
 * | 物料 / 仓库 | `material_id` / `warehouse_id` | 出参**只有 id**，名称靠主数据本地映射 |
 * | 库位 | —— | `biz_inventory.location_id` 有列但 `InventoryRes` **不吐**；库位维度看 Tab2 |
 *
 * 唯一键是 `(material_id, warehouse_id, COALESCE(location_id, 全零uuid))`（V0139），
 * 所以同一「物料 × 仓库」可能出现多行（整仓汇总行 + 各库位行），页面按后端原样展示。
 */
export interface InventoryRow {
  /** 主键 */
  id: string;
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 账面数量（含锁定） */
  qty: number | string;
  /** 锁定量（**预留字段**：本版本无占用流程，恒为 0） */
  locked_qty: number | string;
  /** 移动加权平均成本 */
  avg_cost: number | string;
  /** 账面金额 */
  amount: number | string;
  /** 最近一次流水时间（RFC3339，可空） */
  last_txn_at: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/**
 * 库位库存行（`GET /erp/inventory-location/page`，逐字抄 `dto.rs::InventoryLocationRes`）。
 *
 * ⚠️ `location_id` 指向 **`biz_location.id`**（库位主数据），不是 `biz_inventory_location.id`
 * —— 两者同名反着指：`biz_inventory.location_id` / `biz_material_serial.location_id`
 * 指库位库存行，本行的 `location_id` 才是真库位。译名走 `fetchLocationPage` 的本地映射。
 */
export interface InventoryLocationRow {
  /** 主键（库位库存行 id） */
  id: string;
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 库位 id（`null` = 该仓库未分配到具体库位的数量） */
  location_id: string | null;
  /** 数量 */
  qty: number | string;
  /** 锁定量 */
  locked_qty: number | string;
  /** 最近一次流水时间 */
  last_txn_at: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 出入方向（`dto::TXN_TYPES` + `ck_biz_inv_txn_type`，只有两个值） */
export type InventoryTxnType = 'IN' | 'OUT';

/**
 * 库存流水行（`GET /erp/inventory-txn/page`，逐字抄 `dto.rs::InventoryTxnRes`）。
 *
 * `qty` 恒为正（`ck_biz_inv_txn_qty: qty > 0`），方向由 `txn_type` 表达 ——
 * 界面上「+200 / −200」是**前端加符号**，不要拿 `qty` 的正负当方向。
 *
 * `before_* / after_*` 六列是 W1c 在事务内算好的快照，详情抽屉里用来对账
 * （`before_qty + 方向数量 = after_qty`）。
 */
export interface InventoryTxnRow {
  /** 主键 */
  id: string;
  /** 物料 id */
  material_id: string;
  /** 仓库 id */
  warehouse_id: string;
  /** 方向（`IN` / `OUT`） */
  txn_type: InventoryTxnType | string;
  /** 业务类型（`PURCHASE_IN` / `TRANSFER_IN` / `TRANSFER_OUT` / `SERIAL_IN` / `SERIAL_OUT` …） */
  biz_type: string;
  /** 单据类型（`STOCK_IN` / `STOCK_TRANSFER` / `MATERIAL_SERIAL` …） */
  doc_type: string;
  /** 单据主键 */
  doc_id: string;
  /** 单据号 */
  doc_no: string;
  /** 单据行号 */
  line_no: number;
  /** 数量（恒正） */
  qty: number | string;
  /** 单位成本 */
  unit_cost: number | string;
  /** 金额 */
  amount: number | string;
  /** 变动前数量 */
  before_qty: number | string;
  /** 变动前金额 */
  before_amount: number | string;
  /** 变动前均价 */
  before_cost: number | string;
  /** 变动后数量 */
  after_qty: number | string;
  /** 变动后金额 */
  after_amount: number | string;
  /** 变动后均价 */
  after_cost: number | string;
  /** 会计期间 `yyyy-MM` */
  period_code: string;
  /** 操作人 */
  operator_id: string | null;
  /** 操作人姓名（AUD-02 起后端批量 join 下发） */
  operator_name?: string | null;
  /** 备注 */
  remark: string | null;
  /** 记账时间（流水按它倒序） */
  created_at: string;
}

/**
 * 即时库存分页（`GET /erp/inventory/page`）。
 *
 * 后端只认 `material_id` / `warehouse_id` / `only_nonzero` 三个筛选
 * （`dto.rs::InventoryFilter`，没有 `location_id` 也没有关键词）。
 * `only_nonzero` 是**服务端**过滤（`AND qty <> 0`），所以「隐藏零库存」开关
 * 翻页也是准的，不会像前端滤当前页那样把总数算错。
 */
export function fetchInventoryPage(query: {
  material_id?: string;
  warehouse_id?: string;
  only_nonzero?: boolean;
  page?: number;
  page_size?: number;
}): Promise<PageData<InventoryRow>> {
  return get<PageData<InventoryRow>>(`${BASE}/inventory/page`, asParams(query));
}

/** 即时库存详情（`GET /erp/inventory/{id}`） */
export function fetchInventoryDetail(id: string): Promise<InventoryRow> {
  return get<InventoryRow>(`${BASE}/inventory/${id}`);
}

/** 库位库存分页（`GET /erp/inventory-location/page`，筛选：物料 / 仓库 / 库位） */
export function fetchInventoryLocationPage(query: {
  material_id?: string;
  warehouse_id?: string;
  location_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<InventoryLocationRow>> {
  return get<PageData<InventoryLocationRow>>(`${BASE}/inventory-location/page`, asParams(query));
}

/** 库位库存详情（`GET /erp/inventory-location/{id}`） */
export function fetchInventoryLocationDetail(id: string): Promise<InventoryLocationRow> {
  return get<InventoryLocationRow>(`${BASE}/inventory-location/${id}`);
}

/**
 * 库存流水分页（`GET /erp/inventory-txn/page`）。
 *
 * 后端支持的筛选（`dto.rs::InventoryTxnFilter`，全部可选）：
 * `material_id` / `warehouse_id` / `doc_no`（**精确**匹配，不是模糊）/ `doc_type` /
 * `txn_type`（`IN`/`OUT`，别的值 400）/ `period_code` / `date_from` / `date_to`（按
 * `created_at` 的 UTC 日界，右开区间）。
 */
export function fetchInventoryTxnPage(query: {
  material_id?: string;
  warehouse_id?: string;
  doc_no?: string;
  doc_type?: string;
  txn_type?: InventoryTxnType | '';
  period_code?: string;
  date_from?: string;
  date_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<InventoryTxnRow>> {
  return get<PageData<InventoryTxnRow>>(`${BASE}/inventory-txn/page`, asParams(query));
}

/** 库存流水详情（`GET /erp/inventory-txn/{id}`，含 `before_* / after_*` 六列快照） */
export function fetchInventoryTxnDetail(id: string): Promise<InventoryTxnRow> {
  return get<InventoryTxnRow>(`${BASE}/inventory-txn/${id}`);
}

// ==================== 库存巡检（不变式③：账面 = 在库序列号数） ====================

/**
 * 巡检明细行（逐字抄 `inventory_audit.rs::AuditItem`）。
 *
 * 一行 = 一个 `(物料, 仓库)` 组合。`warehouse_id` 为 `null` 表示「有在库序列号但没有仓库归属」，
 * 这种行 `fixable = false`（写不进 `biz_inventory`）。
 */
export interface InventoryAuditItem {
  /** 物料主键 */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 仓库主键（`null` = 序列号没有仓库归属） */
  warehouse_id: string | null;
  /** 仓库名称（与 `warehouse_id` 同生共死） */
  warehouse_name: string | null;
  /**
   * 该 `(物料, 仓库)` 在成本账里有多少个桶（`biz_inventory` 行数）。
   *
   * `1` = 单桶（可自动修）；`0` = 一行没建账（可建档）；`≥2` = 多桶并存（拒绝自动修）。
   */
  bucket_count: number;
  /** 账面数量（该 `(物料, 仓库)` **所有桶之和**；行不存在按 0 报） */
  book_qty: number | string;
  /** 实际在库件数（`status='IN_STOCK'` 的序列号条数） */
  actual_qty: number | string;
  /** 账面 − 实际（正 = 账多，负 = 账少） */
  diff: number | string;
  /** 能否被 `dry_run=false` 修正（缺仓库 / 多桶都不能） */
  fixable: boolean;
  /** 不能自动修的原因：`NO_WAREHOUSE` / `MULTI_BUCKET`；`null` = 可修 */
  unfixable_reason: string | null;
}

/**
 * 巡检报告（逐字抄 `inventory_audit.rs::AuditReport`）。
 *
 * `checked` 是**检查了多少个组合**（含「有序列号没库存行」那类），`mismatched = items.length`。
 */
export interface InventoryAuditReport {
  /** 本次是否只报不修（缺省 `true`） */
  dry_run: boolean;
  /** 检查的 `(物料, 仓库)` 组合数 */
  checked: number;
  /** 不一致组合数 */
  mismatched: number;
  /** 不一致明细 */
  items: InventoryAuditItem[];
  /** 实际修好的行数（`dry_run=true` 恒为 0） */
  fixed: number;
  /** 报出来但修不了的行数 */
  unfixable: number;
}

/**
 * 库存巡检（`GET /erp/inventory/audit?dry_run=true`，权限 `business:erp:inventory:audit`）。
 *
 * ⚠️ **默认只读**（`dry_run` 缺省 `true`，由后端 `parse_dry_run` 兜底）。前端页面只调只读口径，
 * 不在页面里提供「修正」入口 —— 一键改账不该是个页面按钮（红线③：防误修）。
 */
export function fetchInventoryAudit(dryRun = true): Promise<InventoryAuditReport> {
  return get<InventoryAuditReport>(`${BASE}/inventory/audit`, asParams({ dry_run: dryRun }));
}

/**
 * 采购明细分页（`GET /erp/purchase-order-item/page`，权限
 * `business:erp:purchase-order-item:list`）。
 *
 * 后端筛选（`dto.rs::PurchaseOrderItemFilter`）：
 * - `order_id` / `material_id`：**精确**匹配；
 * - `keyword`（卡 E2E01-FIX-2）：`ILIKE '%…%'` 模糊匹配，命中**任一**即算
 *   —— 采购单号（`biz_purchase_order.order_no`）、物料编码（`biz_material.material_code`）、
 *   物料名称（`biz_material.material_name`）；不传 / 空白 = 不加条件。
 *
 * 没有日期区间，也没有排序参数（排序由后端固定：带 keyword 时按单号 + 行号，
 * 不带时按行号）。
 *
 * 与 {@link fetchPurchaseOrderItems} 的区别：
 * - 本函数给的是**跨订单**的分页列表（「采购明细」菜单页要看的全量台账）；
 * - 后者是「某一张单的明细」的便捷封装（编辑回填用，page_size 固定 200）。
 */
export function fetchPurchaseOrderItemPage(query: {
  order_id?: string;
  material_id?: string;
  keyword?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<PurchaseOrderItemLine>> {
  return get<PageData<PurchaseOrderItemLine>>(`${BASE}/purchase-order-item/page`, asParams(query));
}

/** 采购明细详情（`GET /erp/purchase-order-item/{id}`，与列表元素同构） */
export function fetchPurchaseOrderItemDetail(id: string): Promise<PurchaseOrderItemLine> {
  return get<PurchaseOrderItemLine>(`${BASE}/purchase-order-item/${id}`);
}

/**
 * 单号序列（`GET /erp/seq/page` 的 list 元素，= `dto.rs::SeqRes`）。
 *
 * ⚠️ `biz_seq` 是**取号规则字典**，不是「已经发出的单号」：`period_key` + `current_no`
 * 是当前周期的计数器状态，`step` 是每次取号的步长。真正拼成单号的模板
 * （`PO-yyyyMMdd-000001`）由各业务模块自己的 `format_*_no()` 决定（purchase_model /
 * stockin_model / transfer_model / voucher / payable …），**不由本表驱动** ——
 * 所以本页不能凭 prefix/separator 反推「下一个单号」的完整字符串，只能展示计数器事实。
 */
export interface SeqRow {
  /** 主键 */
  id: string;
  /** 序列类型（未删除内唯一，如 `PO` / `IN` / `SN`） */
  seq_type: string;
  /** 序列名称 */
  seq_name: string;
  /** 前缀（如 `PO`） */
  prefix: string;
  /** 日期段格式：`yyyyMMdd` / `yyyyMM` / `none` */
  date_pattern: string;
  /** 当前周期键（`yyyyMMdd` / `yyyyMM` / 空串） */
  period_key: string;
  /** 当前序号（`>= 0`） */
  current_no: number;
  /** 步长（`> 0`；下次取号的序号 = `current_no + step`） */
  step: number;
  /** 分隔符 */
  separator: string;
  /** 启用状态：1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 单号序列分页（`GET /erp/seq/page`，权限 `business:erp:seq:list`；`keyword` 匹配类型/名称） */
export function fetchSeqPage(query: {
  keyword?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<SeqRow>> {
  return get<PageData<SeqRow>>(`${BASE}/seq/page`, asParams(query));
}

/** 单号序列详情（`GET /erp/seq/{id}`，也是 `:list` 权限） */
export function fetchSeqDetail(id: string): Promise<SeqRow> {
  return get<SeqRow>(`${BASE}/seq/${id}`);
}

// ==================== 资产负债表（ERP-R1） ====================

/**
 * 资产负债表的一行（对应后端 `report::ReportLineRes`）。
 *
 * **金额是字符串**：后端 `numeric(20,4)` 走 `rust_decimal`，序列化出来是
 * `"16500.0000"` 这样的**定点字符串**（不是 float，避免 0.1+0.2 那种误差）。
 * 显示前用 `formatAmount` 转千分位 2 位，**不要** `Number(x)` 再乘算。
 */
export interface ReportLine {
  /** 报表项编码（`TOTAL_ASSETS` / `MONETARY_FUNDS` …） */
  item_code: string;
  /** 报表项名称（中文） */
  item_name: string;
  /** 层级：1 大类 / 2 小类 / 3 明细（用于缩进） */
  level: number;
  /** 上级编码 */
  parent_code: string | null;
  /** 期末金额；`null` = 纯标题行（如「流动资产」） */
  amount: string | null;
  /** 上期金额；`null` = 没有上期期间 */
  previous_amount: string | null;
  /** 是否减项（累计折旧 / 累计摊销） */
  is_deduction: boolean;
  /** 是否小计 / 合计行（公式项，加粗） */
  is_total: boolean;
  /** 取数说明（科目模式或公式原文） */
  detail: string | null;
  /** 实际命中的科目编码（可审计） */
  accounts: string[];
}

/** 有余额但没有匹配任何报表项的科目（`check.unmapped`） */
export interface UnmappedAccount {
  account_code: string;
  account_name: string;
  account_category: string;
  amount: string;
}

/** 分录合计与单头不符的凭证（`check.unbalanced_vouchers`，不平的直接证据） */
export interface UnbalancedVoucher {
  voucher_no: string;
  voucher_date: string;
  entry_debit: string;
  entry_credit: string;
  header_debit: string;
  header_credit: string;
}

/** 勾稽校验（对应后端 `report::BalanceCheckRes`） */
export interface BalanceCheck {
  /** 资产总计 ≡ 负债+权益总计 */
  balanced: boolean;
  assets_total: string;
  liability_equity_total: string;
  /** 差额（不做舍入） */
  diff: string;
  /** 差额人话（`0.00` / `2000.00` / `0.0001`） */
  diff_text: string;
  /** 整句结论（直接显示） */
  message: string;
  ledger_debit_total: string;
  ledger_credit_total: string;
  unmapped: UnmappedAccount[];
  unbalanced_vouchers: UnbalancedVoucher[];
  /** 成因提示（不平 / 有未归类科目时才非空） */
  hints: string[];
}

/** 资产负债表（对应后端 `report::BalanceSheetRes`） */
export interface BalanceSheet {
  report_code: string;
  report_name: string;
  /** 编制单位 */
  company_name: string;
  /** 金额单位（元） */
  unit: string;
  /** 会计期间 `yyyy-MM` */
  period: string;
  /** 期间中文（`2026年10月`） */
  period_name: string;
  /** 期间状态：`OPEN` / `CLOSED`；`DERIVED` = 库里没期间行、按自然月推算（要先开账） */
  period_status: ReportPeriodStatus;
  period_start: string;
  period_end: string;
  /** 编制时间（ISO8601） */
  generated_at: string;
  /** 上期期间（`yyyy-MM`；没有则为 null） */
  previous_period: string | null;
  assets: ReportLine[];
  liabilities: ReportLine[];
  equity: ReportLine[];
  check: BalanceCheck;
}

/**
 * 编制资产负债表（`GET /erp/report/balance-sheet?period=yyyy-MM`）。
 *
 * # 异常情况
 * - 400 + `10001`：`period` 缺失 / 不是 `yyyy-MM`；
 * - 400 + `66010`：该期间**没在「会计期间」里建立**（报表不替人推导不存在的报告期）；
 * - 500 + `66011`：`biz_report_item` 配置坏了（公式引用了不存在的报表项）。
 */
export function fetchBalanceSheet(period: string): Promise<BalanceSheet> {
  return get<BalanceSheet>(`${BASE}/report/balance-sheet`, asParams({ period }));
}

/**
 * 导出资产负债表 xlsx（`GET /erp/report/balance-sheet/export`）。
 *
 * 返回的是**文件字节流**（不走 `Res<T>` 统一响应体），必须 `responseType: 'blob'`。
 */
export async function exportBalanceSheet(period: string): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/report/balance-sheet/export`, {
    params: asParams({ period }),
    responseType: 'blob',
  });
  return response.data;
}

// ==================== 利润表（ERP-R2） ====================

/**
 * 利润表行（对应后端 `income_statement::IncomeLineRes`）。
 *
 * 两列金额都是**字符串**（`numeric(20,4)` → `rust_decimal` → JSON 字符串），
 * 前端只做千分位格式化，**不做任何加减**：合计、小计、净利润全部由后端算好。
 */
export interface IncomeLine {
  /** 报表项编码（`REVENUE` / `NET_PROFIT` …；`UNMAPPED_PROFIT_LOSS` 是运行时补出来的未归类行） */
  item_code: string;
  /** 报表项名称（`减：营业成本` 这种前缀已经在名字里，前端**不要**再拼） */
  item_name: string;
  /** 层级：1 大类 / 2 明细（用于缩进） */
  level: number;
  /** 上级编码 */
  parent_code: string | null;
  /** 本期金额 */
  current: string;
  /** 本年累计金额 */
  ytd: string;
  /** 是否合计 / 小计行（营业利润 / 利润总额 / 净利润，加粗） */
  is_total: boolean;
  /** 取数说明（科目模式或公式原文） */
  detail: string | null;
  /** 实际命中的科目编码（可审计） */
  accounts: string[];
}

/** 有发生额但没有任何利润表行匹配的损益类科目（`check.unmapped`） */
export interface UnmappedProfitLoss {
  account_code: string;
  account_name: string;
  /** 本期发生额（正常方向） */
  current: string;
  /** 本年累计发生额（正常方向） */
  ytd: string;
  /** 对本期利润的影响（收入为正、费用为负） */
  effect_current: string;
  /** 对累计利润的影响 */
  effect_ytd: string;
}

/** 利润表勾稽（净利润本年累计 ≡ 资产负债表「未分配利润」本年变动） */
export interface IncomeCheck {
  balanced: boolean;
  /** 净利润（本年累计） */
  net_profit_ytd: string;
  /** 资产负债表「未分配利润」本年变动 */
  equity_change: string;
  /** 差额（不做舍入） */
  diff: string;
  /** 差额人话（`0.00` / `-400.00` / `0.0001`） */
  diff_text: string;
  /** 整句结论（直接显示） */
  message: string;
  undistributed_profit_end: string;
  undistributed_profit_year_start: string;
  unmapped_profit_loss_ytd: string;
  unmapped: UnmappedProfitLoss[];
  ledger_debit_total: string;
  ledger_credit_total: string;
  unbalanced_vouchers: UnbalancedVoucher[];
  hints: string[];
}

/** 利润表（对应后端 `income_statement::IncomeStatementRes`） */
export interface IncomeStatement {
  report_code: string;
  report_name: string;
  company_name: string;
  unit: string;
  /** 会计期间 `yyyy-MM` */
  period: string;
  period_name: string;
  /** `OPEN` / `CLOSED`；`DERIVED` = 库里没期间行、按自然月推算（要先开账） */
  period_status: ReportPeriodStatus;
  period_start: string;
  period_end: string;
  /** 本年累计起算日（1 月 1 日） */
  ytd_start: string;
  /** 本期列名（后端给，前端不写死） */
  current_label: string;
  /** 本年累计列名 */
  ytd_label: string;
  generated_at: string;
  items: IncomeLine[];
  check: IncomeCheck;
}

/**
 * 编制利润表（`GET /erp/report/income-statement?period=yyyy-MM`）。
 *
 * # 异常情况
 * - 400 + `10001`：`period` 缺失 / 不是 `yyyy-MM`；
 * - 400 + `66010`：该期间**没在「会计期间」里建立**且该月没有任何已记账凭证；
 * - 500 + `66011`：`biz_report_item` 配置坏了（公式引用了不存在的报表项）。
 */
export function fetchIncomeStatement(period: string): Promise<IncomeStatement> {
  return get<IncomeStatement>(`${BASE}/report/income-statement`, asParams({ period }));
}

/**
 * 导出利润表 xlsx（`GET /erp/report/income-statement/export`）。
 *
 * 返回文件字节流（不走 `Res<T>` 统一响应体），必须 `responseType: 'blob'`。
 */
export async function exportIncomeStatement(period: string): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/report/income-statement/export`, {
    params: asParams({ period }),
    responseType: 'blob',
  });
  return response.data;
}

/*
 * 报表 PDF 的路径不在这里单独封一个 axios 函数（PDF-08）：三张报表页统一用
 * `DocPdfButton`（`@/components/DocPdfButton.vue`）直接 fetch 带 token 取 blob ——
 * 这样 loading / 403 提示 / `Content-Disposition` 中文名 / `revokeObjectURL` 只有一份实现。
 * 路径口径：`GET /api/business/erp/report/{type}/pdf?period=yyyy-MM`。
 */

// ==================== 现金流量表（ERP-R3，简化版） ====================

/** 现金流量表的一行（对应后端 `cash_flow::CashLineRes`） */
export interface CashFlowLine {
  /** 明细行编码（`OPER_SALES_RECEIPTS` …；`OTHER_*` 是运行时补出来的未分类行） */
  item_code: string;
  /** 明细行名称（纸面报表原文） */
  item_name: string;
  /** 本期金额 */
  current: string;
  /** 本年累计金额 */
  ytd: string;
  /** 实际命中的对方科目编码（可审计） */
  accounts: string[];
  /** 取数说明（对方科目 pattern 原文） */
  detail: string | null;
}

/** 金额对（本期 / 本年累计）—— 后端 `cash_flow::AmountPair` */
export interface CashFlowAmountPair {
  current: string;
  ytd: string;
}

/** 一个活动分区（经营 / 投资 / 筹资 / 其他） */
export interface CashFlowSection {
  /** `OPERATING` / `INVESTING` / `FINANCING` / `OTHER` */
  activity: string;
  /** 标题（含「一、」这类前缀，前端不要再拼） */
  activity_name: string;
  inflow: CashFlowLine[];
  inflow_total: CashFlowAmountPair;
  outflow: CashFlowLine[];
  outflow_total: CashFlowAmountPair;
  /** 净额 = 流入小计 − 流出小计 */
  net: CashFlowAmountPair;
}

/** 现金流量表勾稽（期末现金余额 ≡ 资产负债表「货币资金」） */
export interface CashFlowCheck {
  balanced: boolean;
  /** 本表算出来的期末现金余额 */
  closing_balance: string;
  /** 资产负债表「货币资金」 */
  balance_sheet_cash: string;
  /** 差额（不做舍入） */
  diff: string;
  /** 差额人话（`0.00` / `-250.00` / `0.0001`） */
  diff_text: string;
  /** 整句结论（直接显示） */
  message: string;
  /** 总账现金类科目期末余额（第三方视角） */
  closing_by_ledger: string;
  /** 本期现金分录笔数 */
  cash_entry_count: number;
  /** 未能分类的笔数（已归「其他」） */
  unclassified_count: number;
  unclassified_inflow: string;
  unclassified_outflow: string;
  unbalanced_vouchers: UnbalancedVoucher[];
  hints: string[];
}

/** 现金流量表（对应后端 `cash_flow::CashFlowRes`） */
export interface CashFlow {
  report_code: string;
  report_name: string;
  company_name: string;
  unit: string;
  /** 会计期间 `yyyy-MM` */
  period: string;
  period_name: string;
  /** `OPEN` / `CLOSED`；`DERIVED` = 库里没期间行、按自然月推算（要先开账） */
  period_status: ReportPeriodStatus;
  period_start: string;
  period_end: string;
  /** 本年累计起算日（1 月 1 日） */
  ytd_start: string;
  current_label: string;
  ytd_label: string;
  generated_at: string;
  /** 恒 true：本表是简化版（口径见 simplified_note） */
  simplified: boolean;
  /** 简化口径说明（原样展示） */
  simplified_note: string;
  operating: CashFlowSection;
  investing: CashFlowSection;
  financing: CashFlowSection;
  /** 其他（未分类，含分录不平差额） */
  other: CashFlowSection;
  net_increase: CashFlowAmountPair;
  opening_balance: CashFlowAmountPair;
  closing_balance: CashFlowAmountPair;
  check: CashFlowCheck;
  warnings: string[];
}

/**
 * 编制现金流量表（`GET /erp/report/cash-flow?period=yyyy-MM`）。
 *
 * # 异常情况
 * - 400 + `10001`：`period` 缺失 / 不是 `yyyy-MM`；
 * - 400 + `66010`：该期间**没在「会计期间」里建立**且该月没有任何已记账凭证；
 * - 500 + `66011`：分类配置为空 / 坏（`biz_cashflow_item`）。
 */
export function fetchCashFlow(period: string): Promise<CashFlow> {
  return get<CashFlow>(`${BASE}/report/cash-flow`, asParams({ period }));
}

/**
 * 导出现金流量表 xlsx（`GET /erp/report/cash-flow/export`）。
 *
 * 返回文件字节流（不走 `Res<T>` 统一响应体），必须 `responseType: 'blob'`。
 */
export async function exportCashFlow(period: string): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/report/cash-flow/export`, {
    params: asParams({ period }),
    responseType: 'blob',
  });
  return response.data;
}

// ==================== 期初余额（ERP-R5）====================
//
// 三个接口（后端 `opening.rs`，注解不含 `/api`，前端写全路径）：
//
// | 方法 | 路径 | 权限码 |
// |------|------|--------|
// | GET  | `/api/business/erp/opening-balance?period=2026-11` | `business:erp:opening:read` |
// | POST | `/api/business/erp/opening-balance` | `business:erp:opening:write` |
// | POST | `/api/business/erp/opening-balance/inventory` | `business:erp:opening:inventory` |
//
// # 字段口径（逐字抄 `opening.rs`，不猜）
//
// - 金额是 `Decimal`，经 JSON 回来是**字符串或数字**（`"50000.0000"` / `50000`），
//   所以类型写成 `number | string`，比较一律走 `toMoney()`（见 `opening-logic.ts`）；
// - `state` 三值：`NONE` 未录 / `ENTERED` 已录 / `LOCKED` 已锁定（期间已结账）；
// - `period_status` 三值：`OPEN` / `CLOSED` / `NOT_OPENED`（与 `ck_biz_period_status` 一致）；
// - `balanced` / `difference` 是**服务端算好的合并口径**（科目期初 + 库存期初自动生成的
//   1405 借方），前端不要自己再拼一遍——两处各算一遍就会有两套真相。

/** 期初状态（`opening.rs::STATE_*`） */
export type OpeningState = 'NONE' | 'ENTERED' | 'LOCKED';

/** 一条科目期初（`opening.rs::OpeningEntryRes`） */
export interface OpeningEntry {
  account_id: string;
  account_code: string;
  account_name: string;
  /** `ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS` */
  account_category: string;
  debit: number | string;
  credit: number | string;
  remark: string | null;
}

/** 一条库存期初（`opening.rs::OpeningInventoryRes`） */
export interface OpeningInventoryLine {
  material_id: string;
  material_code: string;
  material_name: string;
  warehouse_id: string;
  warehouse_code: string;
  warehouse_name: string;
  qty: number | string;
  unit_cost: number | string;
  amount: number | string;
}

/** 期初凭证（`opening.rs::OpeningVoucherRes`，一个期间最多一张） */
export interface OpeningVoucher {
  id: string;
  /** `PZ-yyyyMM-000001` */
  voucher_no: string;
  /** 期间第一天 */
  voucher_date: string;
  status: string;
  total_debit: number | string;
  total_credit: number | string;
  entry_count: number;
}

/** 期初余额总览（`opening.rs::OpeningBalanceRes`，两个保存接口也返回它） */
export interface OpeningBalance {
  period: string;
  period_status: 'OPEN' | 'CLOSED' | 'NOT_OPENED';
  state: OpeningState;
  /** = 期间已结账（整页只读） */
  locked: boolean;
  entries: OpeningEntry[];
  /** 科目期初借方合计（不含库存期初） */
  debit_total: number | string;
  /** 科目期初贷方合计（不含库存期初） */
  credit_total: number | string;
  inventory: OpeningInventoryLine[];
  /** 库存期初金额合计（= 期初凭证的 1405 借方） */
  inventory_amount: number | string;
  /** 库存期初的借方科目（`1405`） */
  inventory_debit_account: string;
  /** 库存期初的贷方科目（`3103`） */
  inventory_credit_account: string;
  /** 合并口径借方合计（科目 + 库存） */
  combined_debit_total: number | string;
  /** 合并口径贷方合计 */
  combined_credit_total: number | string;
  balanced: boolean;
  /** 借 − 贷 */
  difference: number | string;
  voucher: OpeningVoucher | null;
  hints: string[];
}

/** 保存科目期初的一条入参（`opening.rs::OpeningEntryReq`） */
export interface OpeningEntryInput {
  account_code: string;
  debit?: number | string;
  credit?: number | string;
  remark?: string;
}

/** 保存科目期初（方式 A）：**整体覆盖** —— 没提到的科目视为没有期初 */
export interface SaveOpeningPayload {
  period: string;
  entries: OpeningEntryInput[];
}

/** 保存库存期初的一条入参（`opening.rs::OpeningInventoryItemReq`） */
export interface OpeningInventoryInput {
  material_id: string;
  warehouse_id: string;
  qty: number | string;
  unit_cost?: number | string;
  remark?: string;
}

/** 保存库存期初（方式 B 的第一项）：**整体覆盖** */
export interface SaveOpeningInventoryPayload {
  period: string;
  items: OpeningInventoryInput[];
}

/**
 * 读期初余额总览（合并两种口径 + 状态 + 期初凭证）。
 *
 * 失败口径（后端 `opening.rs`）：
 * - 400 + `10001`：`period` 缺失 / 不是 `yyyy-MM`；
 * - 400 + `66010`：该期间**没开账**（先去「会计期间」页开账）；
 * - 403：没有 `business:erp:opening:read`。
 */
export function fetchOpeningBalance(period: string): Promise<OpeningBalance> {
  return get<OpeningBalance>(`${BASE}/opening-balance`, asParams({ period }));
}

/**
 * 保存科目期初（方式 A），同事务生成/覆盖唯一的期初凭证。
 *
 * 失败口径：
 * - 400 + `66012`：**借贷不平衡**，消息里带差额与两边科目（前端直接展示 `msg`）；
 * - 409 + `10003`：期间**已结账**，期初只读；
 * - 400 + `10001`：科目不存在 / 非末级 / 借贷同时非零 / 与库存期初重复（1405 / 3103）。
 */
export function saveOpeningBalance(payload: SaveOpeningPayload): Promise<OpeningBalance> {
  return post<OpeningBalance>(`${BASE}/opening-balance`, payload);
}

/**
 * 保存库存期初（方式 B），同事务写库存三本账（成本账 / 库位账 / 流水）与期初凭证。
 *
 * 失败口径：
 * - 409 + `10003`：期间已结账；
 * - 400 + `10001`：物料/仓库不存在、一物一码物料、与手工 1405/3103 期初重复。
 */
export function saveOpeningInventory(
  payload: SaveOpeningInventoryPayload,
): Promise<OpeningBalance> {
  return post<OpeningBalance>(`${BASE}/opening-balance/inventory`, payload);
}

// ==================== 年度结转（ERP-R4）====================
//
// 三个接口（后端 `year_end.rs`，注解不含 `/api`，前端写全路径）：
//
// | 方法 | 路径 | 权限码 |
// |------|------|--------|
// | GET  | `/api/business/erp/year-end/check?year=2026` | `business:erp:yearend:read` |
// | POST | `/api/business/erp/year-end/close` | `business:erp:yearend:close` |
// | POST | `/api/business/erp/year-end/reopen` | `business:erp:yearend:reopen` |
//
// # 字段口径（逐字抄 `year_end.rs`，不猜）
//
// - `state.status` 两值：`NOT_CLOSED` 未结转 / `CLOSED` 已结转；
// - `blockers[].type` 六种：`PERIOD_NOT_CLOSED` / `DOC_UNFINISHED` / `UNBALANCED` /
//   `INVARIANT` / `NEXT_YEAR_NOT_OPENED` / `NEXT_YEAR_OPENING_EXISTS`，
//   另有 `ALREADY_CLOSED`（已结转时 check 必带）；
// - 金额是 `Decimal`，经 JSON 回来是**字符串或数字**，比较一律走 `toMoney()`；
// - `carry_forward_voucher_no` 为 `null` = 空年度跳过结转凭证（不是失败）。

/** 年结阻塞项（`year_end.rs::YearEndBlocker`） */
export interface YearEndBlocker {
  type:
    | 'PERIOD_NOT_CLOSED'
    | 'DOC_UNFINISHED'
    | 'UNBALANCED'
    | 'INVARIANT'
    | 'NEXT_YEAR_NOT_OPENED'
    | 'NEXT_YEAR_OPENING_EXISTS'
    | 'ALREADY_CLOSED'
    | 'NOT_CLOSED'
    | string;
  /** 涉及的期间（仅期间类阻塞项） */
  periods?: string[];
  /** 条数（单据 / 不变量项数 / 手工期初行数） */
  count?: number;
  /** 差额（仅 UNBALANCED） */
  diff?: string;
  message: string;
  link?: string;
  samples?: string[];
  /**
   * ★ 挡路单据明细（YEAR-END，后端 `period_items::BlockingItem`，最多 50 条）。
   *
   * 与 `PeriodCloseBlocker.items` **完全同构**，所以前端复用同一张表
   * （`shared/BlockerCheckPanel.vue`）：单号 / 类型 / 金额 / 制单人 / 该谁处理 / 已等 /
   * 去处理 / 催办。
   *
   * 空数组 / 缺省 = 这一类没有单据级明细（期间未结 / 借贷不平 / 下年未开账），
   * 页面只显示 `message`。
   */
  items?: PeriodBlockingItem[];
}

/** 年度结转状态（`year_end.rs::YearEndState`） */
export interface YearEndState {
  year: number;
  closed: boolean;
  status: 'CLOSED' | 'NOT_CLOSED' | string;
  closed_at: string | null;
  closed_by: string | null;
  reason: string | null;
  carry_forward_voucher_id: string | null;
  carry_forward_voucher_no: string | null;
  carry_forward_total: number | string;
  carry_forward_entries: number;
  net_profit: number | string;
  periods_closed: number;
  opening_period: string;
  opening_balance_created: boolean;
  opening_rows: number;
  next_year_periods_ready: boolean;
  next_year_open_count: number;
}

/** 前置检查响应 */
export interface YearEndCheck {
  year: number;
  can_close: boolean;
  blockers: YearEndBlocker[];
  state: YearEndState;
}

/** 年结响应 */
export interface YearEndCloseResult {
  year: number;
  carry_forward_voucher_no: string | null;
  opening_balance_created: boolean;
  opening_rows: number;
  periods_closed: number;
  next_year_periods_ready: boolean;
  net_profit: number | string;
  entry_count: number;
  state: YearEndState;
}

/** 反年结响应 */
export interface YearEndReopenResult {
  year: number;
  reopened: boolean;
  reason: string;
  deleted_voucher_no: string | null;
  deleted_entries: number;
  deleted_opening_rows: number;
  december_reopened: boolean;
  state: YearEndState;
}

/**
 * 读年度结转状态 + 五项前置检查（只读，不落任何东西）。
 *
 * 失败口径：400 + `10001`（year 缺失 / 越界）；403（没有 read 权限）。
 */
export function fetchYearEndCheck(year: number): Promise<YearEndCheck> {
  return get<YearEndCheck>(`${BASE}/year-end/check`, asParams({ year }));
}

/**
 * 年结：损益结转 + 生成下年期初（同事务）。
 *
 * 失败口径：
 * - 409 + `10003`：五项前置校验未过（消息里**逐条点名**：哪几个月没结账 / 几张单据 /
 *   差多少钱 / 哪一年没开账），或本年度已结转；
 * - 403：没有 `business:erp:yearend:close`。
 */
export function closeYearEnd(year: number, reason?: string): Promise<YearEndCloseResult> {
  return post<YearEndCloseResult>(`${BASE}/year-end/close`, { year, reason });
}

/**
 * 反年结：删结转凭证 + 删下年期初 + 12 月解封（同事务，**必填原因 ≥ 10 字**）。
 *
 * 失败口径：
 * - 400 + `10001`：原因没填 / 少于 10 字；
 * - 409 + `10003`：本年度没结转 / 下一年度已经有凭证（消息点名凭证号）；
 * - 403：没有 `business:erp:yearend:reopen`。
 */
export function reopenYearEnd(year: number, reason: string): Promise<YearEndReopenResult> {
  return post<YearEndReopenResult>(`${BASE}/year-end/reopen`, { year, reason });
}

/** 年结催办入参（后端 `year_end_remind::YearEndRemindReq`，`year` 必填） */
export interface YearEndRemindPayload {
  /** 催哪一类（`DOC_UNFINISHED` / `INVARIANT`）；缺省 / 空 = 本次预检里全部有处理人的明细 */
  blocker_kind?: string | null;
  /** 催哪几张（明细里的 `doc_id`）；缺省 / 空 = 该类全部 */
  item_ids?: string[];
}

/**
 * 年结催办：把挡路单据的待办发给处理人（**服务端现算收件人**，同一张单 24h 不重复）。
 *
 * - 后端：`POST /business/erp/year-end/remind`，权限 `business:erp:year-end:remind`；
 * - 返回与期间催办同一个结构（`PeriodRemindResult`：sent / failed / skipped_no_owner /
 *   skipped_recent / total / message），所以 toast 逻辑可以直接复用 `remindToast()`；
 * - 失败口径：400（年度越界 / 勾选超过 50 张 / 非法 UUID）、403（没有催办权限）。
 */
export function remindYearEnd(
  year: number,
  payload: YearEndRemindPayload,
): Promise<PeriodRemindResult> {
  return post<PeriodRemindResult>(`${BASE}/year-end/remind`, { year, ...payload });
}

// ==================== 合同（A5 · 契约来自 A2 `contract_dto.rs`） ====================

/**
 * 合同类型（`contract_dto::CONTRACT_TYPES` + `ck_biz_contract_type` 逐字一致）。
 *
 * 本期后端放行 `PURCHASE` + `SALES`（`SUPPORTED_TYPES`，SO-26 起销售合同可用）：
 * `SALES` 的乙方是**客户**（`biz_customer`，表单里乙方选择器切换成客户选择器）；
 * `SERVICE` / `LEASE` → 400 + `10001`，表单里这两个选项置灰。
 */
export type ContractType = 'PURCHASE' | 'SALES' | 'SERVICE' | 'LEASE';

/**
 * 合同状态（`contract_model::STATUSES` + `ck_biz_contract_status` 逐字一致、顺序一致）。
 *
 * ⚠️ **没有 `CANCELLED`**：草稿作废走软删除（`DELETE`），「终止」只用于已签合同。
 * A5 卡面写的 5 态含 `CANCELLED`，与 DDL / ARCH-2 不符 —— 以 DDL 为准（A2 已在交付
 * 说明里报告过同一处出入）。
 *
 * R-P1-1（合同审批流）之后是 **6 态**：多了 `PENDING_APPROVAL`（审批中，
 * `approval_instance_id` 非空、业务字段冻结），且 `DRAFT → SIGNED` 这条边被删除
 * （没人审过的合同不能直接变成可用于采购付款的已签状态）。
 */
export type ContractStatus =
  | 'DRAFT'
  | 'PENDING_APPROVAL'
  | 'SIGNED'
  | 'ACTIVE'
  | 'COMPLETED'
  | 'TERMINATED';

/**
 * 列表行（`contract_dto::ContractRes`）。
 *
 * `name` 与 `contract_name` 后端**同值双写**（卡面字段名 / DDL 列名），页面只读 `name`。
 * `party_b_name` 是**签约快照**，`party_b_current_name` 是主档现值（供应商改名后两者不同）。
 */
export interface ContractRow {
  /** 主键 */
  id: string;
  /** 合同号 `HT-yyyyMMdd-000001`（只用于显示 / 模糊查） */
  contract_no: string;
  /** 合同名称（= `contract_name`） */
  name: string;
  /** 合同名称（DDL 列名，与 `name` 同值） */
  contract_name: string;
  /** 合同类型 */
  contract_type: ContractType;
  /** 乙方类型（本期恒 `SUPPLIER`） */
  party_b_type: string;
  /** 乙方 id（本期 = 供应商 id） */
  party_b_id: string;
  /** 乙方名称快照（签约时） */
  party_b_name: string;
  /** 乙方名称现值（主档已删时为 `null`） */
  party_b_current_name: string | null;
  /** 合同金额（> 0；后端 `Decimal` 出 JSON 是字符串） */
  amount: number | string;
  /** 币种 */
  currency_code: string;
  /** 签订日期 */
  sign_date: string | null;
  /** 生效日 */
  start_date: string | null;
  /** 到期日 */
  end_date: string | null;
  /** 状态 */
  status: ContractStatus;
  /** 状态中文名（后端给，页面用本地映射保证配色一致） */
  status_label: string;
  /** 最后更新时间 */
  updated_at: string;
}

/**
 * 详情（`contract_dto::ContractDetailRes` = 列表行 + DDL 其余可读列 + 三个派生字段）。
 *
 * ⚠️ DDL（`V0195`）**没有** `terms` / `attachment` / `tax_rate` / `payment_terms` 列，
 * 所以详情里也没有 —— 卡面提到的「税率」「付款条件」本期无处落库（已在交付说明里报告）。
 */
export interface ContractDetail extends ContractRow {
  /** 甲方 id（`null` = 本组织） */
  party_a_id: string | null;
  /** 甲方名称快照 */
  party_a_name: string | null;
  /** 负责人 */
  owner_id: string | null;
  /** 负责人姓名（AUD-02：后端实时取现值；`null` = 负责人为空 / 账号已不存在，显示 `—`） */
  owner_name?: string | null;
  /** 归属部门 */
  dept_id: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
  /** 创建人 */
  create_by: string | null;
  /** 创建人姓名（AUD-02：实时取值，不是快照） */
  create_by_name?: string | null;
  /** 最后修改人 */
  update_by: string | null;
  /** 最后修改人姓名（AUD-02：实时取值） */
  update_by_name?: string | null;
  /** 能否编辑（= 状态 `DRAFT`）—— 后端给的唯一判据，前端别自己判状态 */
  can_edit: boolean;
  /** 能否被采购单引用（`SIGNED` / `ACTIVE`） */
  usable_for_order: boolean;
  /** 当前状态的合法去向（**状态机给的**，前端别硬编码边） */
  next_statuses: string[];
  /**
   * **履约口径**（CTR-02-F1，`contract_dto::ContractDetailRes::commitment_scope`）。
   *
   * - `ORDER_LINKED` —— `PURCHASE` / `SALES`：下面四个履约字段是**真值**；
   * - `NOT_APPLICABLE` —— `SERVICE` / `LEASE`：按口径不挂采购单 / 销售单，
   *   `committed_amount = 0` 是**正确值**，前端**不得**显示 `0.00` / 0%；
   * - 预留 `PAYMENT_PLAN`（需求池 RP-2026-005）。
   *
   * 声明成可选是为了「字段缺失 / 老后端 / 未知取值」时**降级为不渲染履约区**，
   * 判断统一走 `contract-logic.ts::commitmentViewOf`（别在模板里散写字符串比较）。
   */
  commitment_scope?: string | null;
  /** 已执行金额（关联采购单 + 销售单价税合计，后端现算） */
  committed_amount?: number | string | null;
  /** 剩余额度 = `amount - committed_amount`（**允许为负** = 超签，不夹到 0） */
  remaining_amount?: number | string | null;
  /** 未关闭下游单据数（`COMMITTED` 不等于履约完成，只用于归档前提示） */
  open_order_count?: number | null;
  /** 未关闭下游单据单号（后端限量、按单号升序） */
  open_order_nos?: string[] | null;
}

/** 分页筛选（`contract_dto::ContractFilter`，空值一律不发）。 */
export interface ContractPageQuery {
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（后端上限 100，默认 20） */
  page_size?: number;
  /** 合同号模糊 */
  contract_no?: string;
  /** 合同名称模糊（后端 `name` 优先，`contract_name` 次之） */
  name?: string;
  /** 合同类型 */
  contract_type?: string;
  /** 状态 */
  status?: string;
  /** 乙方 id */
  party_b_id?: string;
  /** 签订日期 ≥（`YYYY-MM-DD`） */
  sign_date_from?: string;
  /** 签订日期 ≤ */
  sign_date_to?: string;
  /** 到期日期 ≥（落 `end_date`） */
  expire_date_from?: string;
  /** 到期日期 ≤ */
  expire_date_to?: string;
  /** 合同号或名称关键词 */
  keyword?: string;
}

/**
 * 新建请求（`contract_dto::CreateContractReq`）。
 *
 * **没有** `status` / `contract_no`：状态恒 `DRAFT`、单号由后端 `biz_seq(CT)` 取；
 * 乙方名称也由后端从 `biz_supplier` 读快照，客户端传了不算数。
 */
export interface CreateContractPayload {
  /** 合同名称（必填，≤ 200 字） */
  name: string;
  /** 合同类型（`PURCHASE` / `SALES`；`SALES` 时 `party_b_id` 是客户 id） */
  contract_type: string;
  /** 乙方 id（`PURCHASE` → 供应商 id；`SALES` → 客户 id） */
  party_b_id: string;
  /** 合同金额（后端 `CHECK (amount > 0)`） */
  amount: number | string;
  /** 币种（缺省 `CNY`） */
  currency_code?: string;
  /** 签订日期 */
  sign_date?: string | null;
  /** 生效日 */
  start_date?: string | null;
  /** 到期日 */
  end_date?: string | null;
  /** 备注 */
  remark?: string | null;
}

/**
 * 改单请求（`contract_dto::UpdateContractReq`，**仅 `DRAFT`**）。
 *
 * 嵌套 `Option` 语义：字段不传 = 保持原值；显式传 `null` = 清空（日期 / 备注）。
 * `contract_no` / `status` 不在契约里 —— 传了会被 serde 丢掉。
 */
export interface UpdateContractPayload {
  /** 合同名称 */
  name?: string;
  /** 乙方 id */
  party_b_id?: string;
  /** 合同金额 */
  amount?: number | string;
  /** 签订日期（`null` = 清空） */
  sign_date?: string | null;
  /** 生效日（`null` = 清空） */
  start_date?: string | null;
  /** 到期日（`null` = 清空） */
  end_date?: string | null;
  /** 备注（`null` = 清空） */
  remark?: string | null;
}

/** 状态流转请求（`contract_dto::ContractStatusReq`）。 */
export interface ContractStatusPayload {
  /** 目标状态 */
  target_status: string;
  /** 原因（两条**终止**边必填，后端 `CONTRACT_REASON_EDGES`） */
  reason?: string;
}

/** 合同分页（`GET /business/erp/contract/page`，权限 `business:erp:contract:list`）。 */
export function fetchContractPage(query: ContractPageQuery): Promise<PageData<ContractRow>> {
  return get<PageData<ContractRow>>(`${BASE}/contract/page`, asParams(query));
}

/** 合同详情（`GET /business/erp/contract/{id}`，含 `can_edit` / `next_statuses`）。 */
export function fetchContractDetail(id: string): Promise<ContractDetail> {
  return get<ContractDetail>(`${BASE}/contract/${id}`);
}

/** 新建合同（`POST /business/erp/contract`，权限 `business:erp:contract:create`，落 `DRAFT`）。 */
export function createContract(payload: CreateContractPayload): Promise<ContractDetail> {
  return post<ContractDetail>(`${BASE}/contract`, payload);
}

/** 改单（`PUT /business/erp/contract/{id}`，**仅 `DRAFT`**，非草稿 409 + `66016`）。 */
export function updateContract(id: string, payload: UpdateContractPayload): Promise<ContractDetail> {
  return put<ContractDetail>(`${BASE}/contract/${id}`, payload);
}

/**
 * 状态流转（`PATCH /business/erp/contract/{id}/status`）。
 *
 * 非法边 409 + `66015`（文案由状态机生成，含「当前态允许去哪」）；
 * 两条 `→TERMINATED` 边缺 `reason` 会 400。
 */
export function changeContractStatus(id: string, payload: ContractStatusPayload): Promise<ContractDetail> {
  return patch<ContractDetail>(`${BASE}/contract/${id}/status`, payload);
}

/** 软删（`DELETE /business/erp/contract/{id}`，被未关闭采购单引用时 409 + `66018`）。 */
export function deleteContract(id: string): Promise<{ id: string; contract_no: string; deleted: boolean }> {
  return del<{ id: string; contract_no: string; deleted: boolean }>(`${BASE}/contract/${id}`);
}

/**
 * 反查「这张合同被哪些采购单引用」（详情抽屉用）。
 *
 * 后端**没有**专门的 `/contract/{id}/purchase-orders` 接口，但 A3 给
 * `GET /erp/purchase-order/page` 加了 `contract_id` 筛选（走部分索引 `idx_biz_po_contract`），
 * 所以前端直接复用现成接口 —— 不需要为 A5 改后端。
 */
export function fetchContractPurchaseOrders(contractId: string, pageSize = 50): Promise<PageData<PurchaseOrderRow>> {
  return get<PageData<PurchaseOrderRow>>(
    `${BASE}/purchase-order/page`,
    asParams({ contract_id: contractId, page: 1, page_size: pageSize }),
  );
}

// ==================== 应付账龄分析（P1-FIX-8A / PRD E4-03 P0） ====================

/**
 * 账龄档位定义（对应后端 `payable_aging::AgingBucketDef`）。
 *
 * **表头一律按 `buckets` 渲染，前端不写死档位**：后端加/改档位（例如加「180 天以上」）
 * 这一页自动跟上，不会两边各说一套。`max_days = null` 表示无上界。
 */
export interface AgingBucketDef {
  key: string;
  label: string;
  min_days: number;
  max_days: number | null;
}

/** 某一档的金额（汇总行 / 供应商行的单元格）。金额是字符串（`numeric` → `rust_decimal`）。 */
export interface AgingBucketAmount {
  key: string;
  label: string;
  /** 未付金额合计 */
  unpaid: string;
  /** 单据数 */
  doc_count: number;
}

/** 供应商汇总行（后端 `payable_aging::SupplierAgingRes`）。 */
export interface AgingSupplier {
  supplier_id: string;
  supplier_name: string;
  doc_count: number;
  total_unpaid: string;
  /** 与 `buckets` 同序、同长度（缺档补 0） */
  buckets: AgingBucketAmount[];
  /** 最长逾期天数（未逾期记 0） */
  max_overdue_days: number;
}

/** 明细行（后端 `payable_aging::AgingDetailRes`）。 */
export interface AgingDetail {
  payable_id: string;
  payable_no: string;
  supplier_id: string;
  supplier_name: string;
  source_no: string;
  invoice_no: string | null;
  period_code: string;
  credit_days: number;
  due_date: string;
  /** 逾期天数：基准日 − 到期日（正数 = 已逾期；0 = 到期当天；负数 = 未到期） */
  overdue_days: number;
  bucket: string;
  bucket_label: string;
  amount: string;
  paid_amount: string;
  unpaid_amount: string;
  status: string;
}

/** 应付账龄分析表（后端 `payable_aging::PayableAgingRes`）。 */
export interface PayableAging {
  report_code: string;
  report_name: string;
  company_name: string;
  unit: string;
  /** 基准日 `yyyy-MM-dd` */
  as_of: string;
  /** 基准日来源：`QUERY`（手填）/ `PERIOD`（期间末日）/ `TODAY`（今天） */
  as_of_source: 'QUERY' | 'PERIOD' | 'TODAY';
  period: string | null;
  period_name: string | null;
  /** 期间状态；期间行不存在时为 null（账龄表不因未开账 400） */
  period_status: ReportPeriodStatus | null;
  generated_at: string;
  buckets: AgingBucketDef[];
  summary: AgingBucketAmount[];
  total_unpaid: string;
  /** 逾期合计 = 除「未到期」以外各档之和 */
  overdue_unpaid: string;
  /** 逾期占比（0–100，两位小数） */
  overdue_ratio: string;
  total_amount: string;
  total_paid: string;
  doc_count: number;
  supplier_count: number;
  suppliers: AgingSupplier[];
  details: AgingDetail[];
  /** 明细总条数（截断前） */
  detail_count: number;
  detail_truncated: boolean;
  hints: string[];
}

/**
 * 编应付账龄分析表（`GET /erp/report/payable-aging`）。
 *
 * - `as_of` 优先（`yyyy-MM-dd`）；只传 `period`（`yyyy-MM`）时用**该期间末日**当基准日；
 *   两个都不传 → 今天（CST）。响应里的 `as_of_source` 会回显这个来源。
 * - 400 + `10001`：`as_of` / `period` 格式非法。
 * - **不会**因为「该月没开账」而 400（账龄与期间状态无关，`period_status` 只是回显）。
 */
export function fetchPayableAging(query: { period?: string; as_of?: string }): Promise<PayableAging> {
  return get<PayableAging>(`${BASE}/report/payable-aging`, asParams(query));
}

/**
 * 导出应付账龄分析 xlsx（`GET /erp/report/payable-aging/export`）。
 *
 * 权限码复用报表导出码 `business:erp:report:export`（与资产负债表 / 利润表 / 现金流量表同一个开关）。
 */
export async function exportPayableAging(query: { period?: string; as_of?: string }): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/report/payable-aging/export`, {
    params: asParams(query),
    responseType: 'blob',
  });
  return response.data;
}

// ==================== 销售链：客户档案（SO-03） ====================

/**
 * 客户状态（逐字抄后端 `customer_model::STATUS_ACTIVE` / `STATUS_FROZEN`）。
 *
 * ⚠️ 不是 1/0：客户档案的状态域是**字符串**，`ACTIVE`（正常）/ `FROZEN`（冻结）。
 * 冻结只影响「开新单」，历史单据与引用不受影响（后端 `V0260` 的
 * `ck_biz_customer_status` CHECK 只认这两个字面量，传 `'1'` 回 400 + 10001）。
 */
export type CustomerStatus = 'ACTIVE' | 'FROZEN';

/**
 * 客户档案（逐字抄 `customer_handler::CustomerRes`，= `biz_customer` 的存活列）。
 *
 * 字段名**没有** `customer_` 前缀：本表列就叫 `code` / `name`（与供应商的
 * `supplier_code` / `supplier_name` 不同），猜成后者 = 整列空白且编译期抓不到。
 */
export interface Customer {
  /** 主键（UUID） */
  id: string;
  /** 客户编码（`VARCHAR(40)`，未删除内唯一；软删后编码可复用） */
  code: string;
  /** 客户全称（`VARCHAR(200)`） */
  name: string;
  /** 纳税人识别号 */
  tax_no: string | null;
  /** 默认联系人姓名 */
  contact_name: string | null;
  /** 默认联系人电话 */
  contact_phone: string | null;
  /** 地址 */
  address: string | null;
  /**
   * 信用额度：`Decimal` 后端以**字符串**下发（如 `"200000.5000"`），
   * 不要 `parseFloat` 后再格式化 —— 会丢 4 位小数口径（与供应商页同规则）。
   */
  credit_limit: string;
  /**
   * 账期天数（`INT NOT NULL DEFAULT 0`，`V0289`）：**0 = 现结**（到期日 = 出库过账日），
   * `> 0` 时应收到期日 = 过账日 + 该天数。与 `biz_supplier.credit_days` 同口径。
   */
  credit_days: number;
  /** 状态：`ACTIVE` / `FROZEN` */
  status: CustomerStatus;
  /** 备注 */
  remark: string | null;
  /** 创建时间（ISO8601） */
  created_at: string;
  /** 更新时间（ISO8601） */
  updated_at: string;
}

/**
 * 新建客户（`customer_handler::CreateCustomerReq`）。
 *
 * - `name` 必填；**`code` 留空（`null` / `''` / 纯空白）⇒ 后端自动取号 `CUS-0001` 形态**（CODE-02）；
 * - `credit_limit >= 0`（`0` = 不控额度）；
 * - `status` 留空 → 后端默认 `ACTIVE`；
 * - 文本字段传 `null` 或省略等价（后端 `clean_optional` 会把空白折成 `None`）。
 */
export interface CreateCustomerReq {
  /** 客户编码（≤40；`null` / 空串 = 让后端自动取号） */
  code: string | null;
  /** 客户全称（必填，≤200） */
  name: string;
  /** 纳税人识别号（≤50） */
  tax_no?: string | null;
  /** 联系人（≤50） */
  contact_name?: string | null;
  /** 联系电话（≤30） */
  contact_phone?: string | null;
  /** 地址（≤255） */
  address?: string | null;
  /** 信用额度（`>= 0`，字符串定点更稳：`"1000.5"`） */
  credit_limit?: string | number | null;
  /** 账期天数（`>= 0` 整数，`0` = 现结；不传 = 后端默认 `0`） */
  credit_days?: number | null;
  /** 状态（不传 = `ACTIVE`） */
  status?: CustomerStatus;
  /** 备注（≤2000） */
  remark?: string | null;
}

/**
 * 编辑客户（`customer_handler::UpdateCustomerReq`，**部分更新**）。
 *
 * 缺席 = 不改；可选文本传 `''`（或 `null`）= 清空。所以「只想切状态」的请求体
 * 就是 `{ status: 'FROZEN' }` —— 不要整表单回写，会把别人刚改的字段覆盖回旧值。
 */
export interface UpdateCustomerReq {
  /** 客户编码（不传 = 不改） */
  code?: string;
  /** 客户全称（不传 = 不改） */
  name?: string;
  /** 纳税人识别号（`''` = 清空） */
  tax_no?: string | null;
  /** 联系人（`''` = 清空） */
  contact_name?: string | null;
  /** 联系电话（`''` = 清空） */
  contact_phone?: string | null;
  /** 地址（`''` = 清空） */
  address?: string | null;
  /** 信用额度（不传 = 不改） */
  credit_limit?: string | number | null;
  /** 账期天数（不传 = 不改；`0` = 现结，`>= 0` 整数） */
  credit_days?: number | null;
  /** 状态（不传 = 不改） */
  status?: CustomerStatus;
  /** 备注（`''` = 清空） */
  remark?: string | null;
}

/**
 * 客户分页（`GET /business/erp/customer/page`，权限 `business:erp:customer:list`）。
 *
 * 入参逐字抄 `customer_handler::CustomerFilter`：`code` / `name` **模糊**，
 * `status` **精确**（非法值后端 400，不静默忽略）。空串一律不发
 * （后端把 `?code=` 当「等于空串」过滤，会查出 0 行）。
 */
export function fetchCustomerPage(query: {
  code?: string;
  name?: string;
  status?: CustomerStatus | '';
  page?: number;
  page_size?: number;
}): Promise<PageData<Customer>> {
  return get<PageData<Customer>>(`${BASE}/customer/page`, asParams(query));
}

/** 客户详情（`GET /business/erp/customer/{id}`，同样是 `:list` 权限；软删行 404） */
export function fetchCustomerDetail(id: string): Promise<Customer> {
  return get<Customer>(`${BASE}/customer/${id}`);
}

/**
 * 客户编码预览（`GET /business/erp/customer/next-code`，权限 `business:erp:customer:create`）。
 *
 * 与供应商同一口径：**只读、不占号**（交互方式 B 的方案 B），撞号由保存时的
 * `409 + 66001` 兜底，前端提示重新生成即可。
 */
export function fetchCustomerNextCode(): Promise<NextCodePreview> {
  return get<NextCodePreview>(`${BASE}/customer/next-code`);
}

/** 新建客户（`POST /business/erp/customer`，权限 `business:erp:customer:create`） */
export function createCustomer(payload: CreateCustomerReq): Promise<Customer> {
  return post<Customer>(`${BASE}/customer`, payload);
}

/** 编辑客户（`PUT /business/erp/customer/{id}`，权限 `business:erp:customer:update`） */
export function updateCustomer(id: string, payload: UpdateCustomerReq): Promise<Customer> {
  return put<Customer>(`${BASE}/customer/${id}`, payload);
}

/**
 * 删除客户（`DELETE /business/erp/customer/{id}`，**软删**，权限 `business:erp:customer:delete`）。
 *
 * 软删后**同一编码可以再建**（`uk_biz_customer_code` 是 `WHERE deleted_at IS NULL` 的部分唯一索引），
 * 所以删除确认文案里要把这件事写出来，别让用户以为编码永久作废。
 */
export function deleteCustomer(id: string): Promise<void> {
  return del<void>(`${BASE}/customer/${id}`);
}

// ==================== 销售订单（SO-07 · 销售链） ====================

/**
 * 销售订单状态域（7 值，逐字抄 `sales_model::STATUSES` / `V0264` 的 `ck_sales_order_status`）。
 *
 * | 编码 | 中文 | 谁写 |
 * |------|------|------|
 * | `DRAFT` | 草稿 | 建单 |
 * | `SUBMITTED` | 审批中 | 提交审批（`POST /{id}/submit-approval`） |
 * | `APPROVED` | 已审批 | **只由审批回调写**（人工 PATCH 一律 400） |
 * | `PARTIAL_OUT` | 部分出库 | 出库过账回写 |
 * | `OUTBOUND` | 已出库 | 出库过账回写 |
 * | `CLOSED` | 已关闭 | 人工关闭（需原因） |
 * | `CANCELLED` | 已作废 | 人工作废（需原因） |
 */
export type SalesOrderStatus =
  | 'DRAFT'
  | 'SUBMITTED'
  | 'APPROVED'
  | 'PARTIAL_OUT'
  | 'OUTBOUND'
  | 'CLOSED'
  | 'CANCELLED';

/**
 * 销售订单明细行（逐字抄 `sales_order_handler::SalesOrderItemRes`）。
 *
 * 金额三项（`amount` / `tax_amount` / 单头汇总）都是**服务端算的**：前端提交只给
 * `qty` / `price` / `tax_rate`，回填时直接显示服务端返回值（前端自己算一遍只用于
 * 表单实时预览，永远不参与提交）。
 */
export interface SalesOrderItem {
  /** 主键 */
  id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id（弱引用 `biz_material.id`） */
  material_id: string;
  /** 数量（> 0） */
  qty: number | string;
  /** 不含税单价 */
  price: number | string;
  /** 行不含税金额（= round(price × qty, 4)） */
  amount: number | string;
  /** 税率百分数（0–100） */
  tax_rate: number | string;
  /** 行税额 */
  tax_amount: number | string;
  /** 已发货数量 */
  delivered_qty: number | string;
  /** 未发货数量（`qty - delivered_qty`） */
  open_qty: number | string;
}

/**
 * 销售订单（逐字抄 `sales_order_handler::SalesOrderRes`；列表行与详情同构）。
 *
 * - **列表行** `items` 恒为空数组（明细只在详情返回），`approval_instance_id` 恒 `null`；
 * - `customer_code` / `customer_name` 在客户软删后为 `null`（列表已批量补齐，详情也补）；
 * - `status_label` 由后端给中文，前端**不要**自己再译一遍（两处口径迟早漂移）。
 */
export interface SalesOrder {
  /** 主键 */
  id: string;
  /** 单号（`SO-yyyyMMdd-000001`） */
  so_no: string;
  /** 客户 id */
  customer_id: string;
  /** 客户编码（客户软删后 `null`） */
  customer_code: string | null;
  /** 客户名称（客户软删后 `null`） */
  customer_name: string | null;
  /** 销售合同 id（未挂合同 `null`，SO-27） */
  contract_id: string | null;
  /** 销售合同号（`HT-yyyyMMdd-000001`；**只有详情填**，列表恒 `null`） */
  contract_no: string | null;
  /** 销售员 id（未指定 `null`；V0309 / SO-35 —— 佣金归属人） */
  salesperson_id: string | null;
  /** 销售员姓名（列表批量补齐 / 详情单查；账号软删后 `null`） */
  salesperson_name: string | null;
  /** 下单日期（`yyyy-MM-dd`） */
  order_date: string;
  /** 要求交期（未约定 `null`） */
  delivery_date: string | null;
  /** 状态编码 */
  status: SalesOrderStatus;
  /** 状态中文名（后端给） */
  status_label: string;
  /** 不含税合计 */
  total_amount: number | string;
  /** 税额合计 */
  total_tax: number | string;
  /** 价税合计（= 上两项之和，精确恒等） */
  total_with_tax: number | string;
  /** 备注 */
  remark: string | null;
  /** 币种（SO-40 / V0314；本位币单也带码，存量单回填成 `CNY`） */
  currency_code: string;
  /**
   * **建单时锁定的**汇率（`1 原币 = exchange_rate 本位币`；本位币单恒 1）。
   *
   * 从单据行读，**不随汇率表变动回写** —— 展示它就是展示这张单的终身汇率。
   */
  exchange_rate: number | string;
  /** 本位币不含税金额（原币 × 锁定汇率，尾差落含税列） */
  base_total_amount: number | string;
  /** 本位币税额 */
  base_total_tax: number | string;
  /** 本位币价税合计（= 上两项之和，精确恒等） */
  base_total_with_tax: number | string;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
  /** 明细（列表恒为空） */
  items: SalesOrderItem[];
  /** 审批实例 id（**只有详情填**；草稿 / 驳回打回为 `null`） */
  approval_instance_id: string | null;
}

/** 一行明细入参（金额不给：由服务端算） */
export interface SalesOrderItemReq {
  /** 物料 id（必填 UUID） */
  material_id: string;
  /** 数量（> 0，必填） */
  qty: string;
  /** 不含税单价（≥ 0，不给 = 0） */
  price?: string;
  /** 税率百分数（0–100，不给 = 0） */
  tax_rate?: string;
}

/** 新建销售订单（`sales_order_handler::CreateSalesOrderReq`）：至少 1 行明细 */
export interface CreateSalesOrderReq {
  /** 客户 id（必须存在且 `ACTIVE`，否则 400） */
  customer_id: string;
  /**
   * 销售合同 id（可空，SO-27）。
   *
   * 填了就由后端校验：类型必须 `SALES`、状态 `SIGNED`/`ACTIVE`、乙方 = 本单客户
   * （否则 400），且「已下单 + 本单」不得超合同金额（超出 409 `66019`）。
   */
  contract_id?: string | null;
  /**
   * 销售员 id（`sys_user.id`，可空 = 不指定；SO-35 / V0309）。
   *
   * 填了就必须是「存在 + 未软删 + 启用」的账号（停用账号不允许再接新单计佣）；
   * **留空不默认成建单人** —— 佣金归属必须是人明确指定的。
   */
  salesperson_id?: string | null;
  /** 下单日期（`yyyy-MM-dd`，不给 = 今天） */
  order_date?: string;
  /** 要求交期（不能早于下单日） */
  delivery_date?: string | null;
  /** 备注 */
  remark?: string | null;
  /**
   * 币种（SO-40 / V0314；不传 = 本位币）。
   *
   * 外币单建单时后端按 `order_date` 取最近一条 SPOT 汇率并**锁进单据行**
   * （`exchange_rate` + 三项 `base_*`）；取不到汇率一律 400，**绝不默认 1**。
   */
  currency_code?: string | null;
  /** 明细（至少 1 行） */
  items: SalesOrderItemReq[];
}

/**
 * 编辑销售订单（`UpdateSalesOrderReq`，**整单覆盖式**，只有 `DRAFT` 能改）。
 *
 * `items` 传了就**整单替换**；不传 = 明细不动。
 */
export interface UpdateSalesOrderReq {
  /** 客户 id（不传 = 不改） */
  customer_id?: string;
  /** 销售合同 id（**不传 = 不改**；传 `''` = 解绑；传 id = 换合同，SO-27） */
  contract_id?: string | null;
  /**
   * 销售员 id（**不传 = 不改**；传 `''` = 解绑；传 id = 换人；SO-35）。
   *
   * 换人同样要过「存在 + 未软删 + 启用」（停用账号不允许再接新单计佣）。
   */
  salesperson_id?: string | null;
  /** 下单日期（不传 = 不改） */
  order_date?: string;
  /** 交期（传 `''` = 清空） */
  delivery_date?: string | null;
  /** 备注（传 `''` = 清空） */
  remark?: string | null;
  /**
   * 币种（SO-40；**不传 = 不改**，传了就按新的 `order_date` 重新锁定汇率）。
   *
   * 草稿阶段换币种是合法的（未过账、无凭证）；已锁定的 `exchange_rate` 不会被前端修改，
   * 后端只认自己按单据日取到的那一条。
   */
  currency_code?: string | null;
  /** 明细（传了 = 整单替换） */
  items?: SalesOrderItemReq[];
}

/** 状态流转（`SalesOrderStatusReq`）：`target_status` 必须是 `LEGAL_EDGES` 里的边 */
export interface SalesOrderStatusReq {
  /** 目标状态 */
  target_status: SalesOrderStatus;
  /** 原因（作废等需要原因） */
  reason?: string;
}

/**
 * 销售订单分页（`GET /business/erp/sales-order/page`，权限 `business:erp:sale-order:list`）。
 *
 * `so_no` 模糊，`status` / `customer_id` 精确；空串一律不发（后端把 `?status=`
 * 当「等于空串」过滤，会查出 0 行）。
 */
export function fetchSalesOrderPage(query: {
  so_no?: string;
  status?: SalesOrderStatus | '';
  customer_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<SalesOrder>> {
  return get<PageData<SalesOrder>>(`${BASE}/sales-order/page`, asParams(query));
}

/** 销售订单详情（`GET /business/erp/sales-order/{id}`，`:list` 权限；软删行 404） */
export function fetchSalesOrderDetail(id: string): Promise<SalesOrder> {
  return get<SalesOrder>(`${BASE}/sales-order/${id}`);
}

/** 新建销售订单（`POST /business/erp/sales-order`，权限 `:create`） */
export function createSalesOrder(payload: CreateSalesOrderReq): Promise<SalesOrder> {
  return post<SalesOrder>(`${BASE}/sales-order`, payload);
}

/** 编辑销售订单（`PUT /business/erp/sales-order/{id}`，权限 `:update`，只有 `DRAFT`） */
export function updateSalesOrder(id: string, payload: UpdateSalesOrderReq): Promise<SalesOrder> {
  return put<SalesOrder>(`${BASE}/sales-order/${id}`, payload);
}

/**
 * 状态流转（`PATCH /business/erp/sales-order/{id}/status`，权限 `:status`）。
 *
 * ⚠️ **`APPROVED` / `DRAFT` 不要往这里传**：审批结论只由审批中心写，人工 PATCH
 * 一律 400（SO-06 硬口径）。撤回审批走 `POST /business/flow/instance/{id}/withdraw`，
 * 驳回 / 通过走 `POST /business/flow/task/{id}/{reject,approve}`。
 */
export function changeSalesOrderStatus(
  id: string,
  payload: SalesOrderStatusReq,
): Promise<SalesOrder> {
  return patch<SalesOrder>(`${BASE}/sales-order/${id}/status`, payload);
}

/**
 * 提交审批（`POST /business/erp/sales-order/{id}/submit-approval`，权限 `:status`）。
 *
 * 与 `PATCH /status` 的 `target_status: 'SUBMITTED'` 是**同一条路**；本端点额外支持
 * `skip_approval` / `skip_reason`（没配 `so_approve` 时的内控例外，需要
 * `business:erp:skip-approval` 且理由 ≥10 字）。
 *
 * 失败口径（SO-06 实测）：
 * - `409` + `APPROVAL_FLOW_NOT_CONFIGURED`：没配流程；
 * - `409` + 审批中：已有在跑实例（报文带审批单号）；
 * - `422`：唯一可审批人 = 制单人（内控）。
 */
export function submitSalesOrderApproval(
  id: string,
  payload: { reason?: string; skip_approval?: boolean; skip_reason?: string } = {},
): Promise<SalesOrder> {
  return post<SalesOrder>(`${BASE}/sales-order/${id}/submit-approval`, payload);
}

// ==================== 应收账款（销售链 · 后端 SO-12/13/14 → 前端 SO-15） ====================

/**
 * 应收单状态域（逐字抄后端 `receivable::STATUSES` / `ck_biz_ar_status`）。
 *
 * ⚠ **不是**应付的那 4 个值：`PARTIAL_PAID` / `PENDING_CANCEL` 是 `biz_payable` 的域，
 * 传给应收接口会 **400**（`receivable_query::ReceivablePageQuery::validate` 白名单拒绝，
 * 绝不静默忽略）。应收对应的是 `PARTIAL`（部分已收）与 `CANCELLED`（出库作废冲销，V0290）。
 */
export type ReceivableStatus = 'OPEN' | 'PARTIAL' | 'SETTLED' | 'WRITTEN_OFF' | 'CANCELLED';

/**
 * 来源单据类型。
 *
 * - `SALES_OUT`：销售出库过账自动挂账（SO-14，`source_no` = 出库单号）；
 * - `MANUAL`：手工挂账（`source_no` 固定 `MANUAL`，也是 `uk_biz_ar_source` 的唯一例外）。
 */
export type ReceivableSourceType = 'MANUAL' | 'SALES_OUT';

/**
 * 应收单（分页行 / 详情单头，后端 `receivable_query::ReceivableRes`）。
 *
 * 金额字段是 `numeric(18,2)` → `rust_decimal`，JSON 里出**字符串**（`"1130.0000"`）；
 * 前端一律过 `money.ts` 的 `formatMoney`（先转整数分再格式化），不要 `parseFloat` 累加。
 */
export interface Receivable {
  id: string;
  /** 应收单号 `AR-yyyyMMdd-000001`（`receivable::NO_PREFIX`） */
  ar_no: string;
  customer_id: string;
  /** 客户名称（后端 LEFT JOIN `biz_customer`，客户被软删时是空串） */
  customer_name: string;
  source_type: ReceivableSourceType | string;
  source_id: string;
  /** 来源单号（出库单号 / `MANUAL`） */
  source_no: string;
  invoice_no: string | null;
  invoice_date: string | null;
  currency_code: string;
  /** 应收金额 */
  amount: number | string;
  /** 已收金额（收款单核销累加；SO-17 落地前恒 0） */
  received_amount: number | string;
  /** 未收金额（= amount − received_amount，库列冗余且有 CHECK 恒等） */
  unsettled_amount: number | string;
  /** 账期天数快照（0 = 现结，挂账时冻结） */
  credit_days: number;
  /** 到期日 `yyyy-MM-dd` */
  due_date: string;
  /** 所属会计期间 `yyyy-MM` */
  period_code: string;
  status: ReceivableStatus | string;
  /** 状态中文（后端 `receivable::status_label`，前端只做兜底不做翻译） */
  status_label: string;
  /**
   * 是否逾期（后端判据：`due_date < 今天` **且** 仍有未收 **且** 状态是 OPEN/PARTIAL）。
   * 到期当天不算逾期，与账龄口径一致。
   */
  overdue: boolean;
  settled_at: string | null;
  /** 收入确认凭证 id（SO-22 前自动挂账的单恒 null） */
  voucher_id: string | null;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 应收详情（`ReceivableRes` 展开 + `voucher_no`）。 */
export interface ReceivableDetail extends Receivable {
  /** 关联凭证号（凭证没生成 / 已删时 null） */
  voucher_no: string | null;
}

/**
 * 应收分页入参（逐字抄 `receivable_query::ReceivablePageQuery`）。
 *
 * `ar_no` 是**模糊**匹配（同时匹配来源单号），**不是** `keyword` ——
 * 传 `keyword` 会被后端静默忽略并返回全表（SO-14 的 curl 脚本踩过这个坑）。
 */
export interface ReceivableQuery {
  page?: number;
  page_size?: number;
  status?: ReceivableStatus | '';
  customer_id?: string;
  ar_no?: string;
  source_type?: ReceivableSourceType | '';
  /** 到期日下界（含）`yyyy-MM-dd` */
  due_date_from?: string;
  /** 到期日上界（含）`yyyy-MM-dd` */
  due_date_to?: string;
}

/**
 * 应收分页（`GET /business/erp/receivable/page`，权限 `business:erp:receivable:list`）。
 *
 * 后端默认**返回全部状态**（核销 / 作废单也要看得见，财务要对账），要只看看未收就自己传 `status`。
 */
export function fetchReceivablePage(query: ReceivableQuery = {}): Promise<PageData<Receivable>> {
  return get<PageData<Receivable>>(`${BASE}/receivable/page`, asParams(query));
}

/** 应收详情（`GET /business/erp/receivable/{id}`，权限 `:detail`）。 */
export function fetchReceivableDetail(id: string): Promise<ReceivableDetail> {
  return get<ReceivableDetail>(`${BASE}/receivable/${id}`);
}

/**
 * 手工挂账（`POST /business/erp/receivable`，权限 `business:erp:receivable:create`）。
 *
 * - `amount` 必填且 `> 0`（后端 400 不静默）；
 * - `due_date` 显式给时**优先于** `credit_days` 推算；
 * - 客户 `FROZEN` → 409；期间已结账 → 400。
 */
export interface CreateReceivableReq {
  customer_id: string;
  amount: string | number;
  /** 账期天数（默认 0 ⇒ 到期日 = 挂账日） */
  credit_days?: number | null;
  /** 到期日 `yyyy-MM-dd`（给了就不按 credit_days 算） */
  due_date?: string | null;
  /** 所属会计期间 `yyyy-MM`（默认挂账日所属期间） */
  period_code?: string | null;
  invoice_no?: string | null;
  invoice_date?: string | null;
  remark?: string | null;
}

/** 手工挂账回执（后端 `receivable::CreateReceivableRes`）。 */
export interface CreateReceivableRes {
  id: string;
  ar_no: string;
  customer_id: string;
  amount: number | string;
  received_amount: number | string;
  unsettled_amount: number | string;
  credit_days: number;
  due_date: string;
  period_code: string;
  status: ReceivableStatus;
  status_label: string;
}

/** 手工挂账。 */
export function createReceivable(payload: CreateReceivableReq): Promise<CreateReceivableRes> {
  return post<CreateReceivableRes>(`${BASE}/receivable`, payload);
}

/**
 * 坏账核销（`PATCH /business/erp/receivable/{id}/write-off`，权限 `business:erp:receivable:write-off`）。
 *
 * 只允许 `OPEN` / `PARTIAL` → `WRITTEN_OFF`；`SETTLED` / 已核销 / 已作废 **409**。
 * `reason` 必填（后端 `REASON_MIN_CHARS` 字起）—— 坏账是会计估计变更，没原因无法审计。
 * 返回**回读后的完整详情**（客户名 + 凭证号），前端可就地刷新卡片。
 */
export function writeOffReceivable(id: string, reason: string): Promise<ReceivableDetail> {
  return patch<ReceivableDetail>(`${BASE}/receivable/${id}/write-off`, { reason });
}

/**
 * 账龄一格（后端共用 `payable_aging::AgingBucketAmountRes`，字段名因此是 **`unpaid`**
 * 而不是 `unsettled` —— 应收侧照抄同一个结构，别按语义改名，改了就是运行时 `undefined`）。
 */
export interface ReceivableAgingCell {
  key: string;
  label: string;
  /** 该档未收金额合计（字段名沿用应付的 `unpaid`） */
  unpaid: string;
  doc_count: number;
}

/** 客户账龄一行（后端 `receivable_aging::CustomerAgingRes`）。 */
export interface ReceivableAgingCustomer {
  customer_id: string;
  customer_name: string;
  doc_count: number;
  /** 未收合计 */
  total_unsettled: string;
  /** 与响应 `buckets` 同序同长（缺档补 0） */
  buckets: ReceivableAgingCell[];
  /** 最长逾期天数（未逾期记 0） */
  max_overdue_days: number;
}

/** 账龄明细一行（后端 `receivable_aging::ReceivableAgingDetailRes`）。 */
export interface ReceivableAgingDetailRow {
  receivable_id: string;
  ar_no: string;
  customer_id: string;
  customer_name: string;
  source_no: string;
  source_type: string;
  invoice_no: string | null;
  period_code: string;
  credit_days: number;
  due_date: string;
  /** 基准日 − 到期日（正数 = 逾期；0 = 到期当天；负数 = 未到期） */
  overdue_days: number;
  bucket: string;
  bucket_label: string;
  amount: string;
  received_amount: string;
  unsettled_amount: string;
  status: string;
}

/** 应收账龄分析表（后端 `receivable_aging::ReceivableAgingRes`）。 */
export interface ReceivableAging {
  report_code: string;
  report_name: string;
  company_name: string;
  unit: string;
  as_of: string;
  as_of_source: 'QUERY' | 'PERIOD' | 'TODAY';
  period: string | null;
  period_name: string | null;
  period_status: ReportPeriodStatus | null;
  generated_at: string;
  /** 档位定义：**表头按它渲染**，前端不写死 30/60/90 */
  buckets: AgingBucketDef[];
  summary: ReceivableAgingCell[];
  total_unsettled: string;
  overdue_unsettled: string;
  overdue_ratio: string;
  total_amount: string;
  total_received: string;
  doc_count: number;
  customer_count: number;
  customers: ReceivableAgingCustomer[];
  details: ReceivableAgingDetailRow[];
  detail_count: number;
  detail_truncated: boolean;
  hints: string[];
}

/**
 * 编应收账龄分析表（`GET /business/erp/receivable/aging`，权限 `business:erp:receivable:aging`）。
 *
 * 入参与应付账龄**同一套语义**：`as_of` 优先；只传 `period` 用该期间末日；都空 = 今天（CST）。
 * 分档与应付**同源**（同一份 `BUCKETS` / `bucket_of`），不含 `WRITTEN_OFF` / `CANCELLED`
 * —— 坏账核销掉的、作废冲销的都不该再算作应收资产。
 */
export function fetchReceivableAging(query: { period?: string; as_of?: string } = {}): Promise<ReceivableAging> {
  return get<ReceivableAging>(`${BASE}/receivable/aging`, asParams(query));
}

// ==================== 收款单（SO-17 后端 / SO-18 前端） ====================

/**
 * 收款单状态域（逐字抄 `receipt::STATUSES` / `ck_biz_receipt_status`）。
 *
 * ⚠️ 与应收的 `OPEN/PARTIAL/SETTLED/…` **不是一套值**：把 `OPEN` 传给收款接口会 400，
 * 把 `DRAFT` 传给应收接口同样 400 —— 两个台账各有自己的 CHECK。
 */
export type ReceiptStatus = 'DRAFT' | 'CONFIRMED' | 'POSTED' | 'CANCELLED';

/** 收款方式域（`receipt::METHODS`，与 `ck_biz_receipt_method` 逐字一致）。 */
export type ReceiptMethod = 'BANK_TRANSFER' | 'CASH' | 'CHECK' | 'OTHER';

/**
 * 收款单行（后端 `receipt::ReceiptRes`，列表 / 详情共用）。
 *
 * 金额是 `NUMERIC` 出的**字符串**（`"1000.0000"`），比较与累加一律走
 * `views/business/erp/shared/money.ts` 的整数分，不要直接 `+`。
 */
export interface Receipt {
  id: string;
  /** 收款单号 `RC-yyyyMMdd-000001`（`receipt::NO_PREFIX`） */
  receipt_no: string;
  customer_id: string;
  /** 客户名称（LEFT JOIN `biz_customer`，客户软删时是空串） */
  customer_name: string;
  /** 收款日期 `yyyy-MM-dd` */
  receipt_date: string;
  amount: number | string;
  /** 已核销金额（= 库内 SUM(核销明细)） */
  allocated_amount: number | string;
  /** 未核销余额（= amount − allocated_amount，后端算好） */
  unallocated_amount: number | string;
  method: ReceiptMethod | string | null;
  /** 收款方式中文（后端 `receipt::method_label`，未指定时「未指定」） */
  method_label: string;
  bank_account: string | null;
  status: ReceiptStatus | string;
  /** 状态中文（后端 `receipt::status_label`） */
  status_label: string;
  /** 过账后生成的收款凭证 id */
  voucher_id: string | null;
  remark: string | null;
  created_at: string | null;
  updated_at: string | null;
}

/** 核销明细（后端 `receipt::AllocationRes`，详情抽屉 / 核销面板用）。 */
export interface ReceiptAllocation {
  id: string;
  receivable_id: string;
  ar_no: string;
  ar_amount: number | string;
  ar_received_amount: number | string;
  ar_unsettled_amount: number | string;
  ar_status: string;
  ar_status_label: string;
  /** 本次（本收款）对这张应收的核销金额 */
  amount: number | string;
  remark: string | null;
  created_at: string | null;
}

/** 收款单详情（`ReceiptRes` 展开 + 凭证号 + 核销明细）。 */
export interface ReceiptDetail extends Receipt {
  voucher_no: string | null;
  allocations: ReceiptAllocation[];
}

/**
 * 收款分页入参（逐字抄 `receipt::ReceiptPageQuery`）。
 *
 * `receipt_no` 是**模糊**匹配（单号 + 客户名），不是 `keyword`；空串一律不发。
 */
export interface ReceiptQuery {
  page?: number;
  page_size?: number;
  status?: ReceiptStatus | '';
  customer_id?: string;
  receipt_no?: string;
  method?: ReceiptMethod | '';
  /** 收款日期下界（含）`yyyy-MM-dd` */
  date_from?: string;
  /** 收款日期上界（含）`yyyy-MM-dd` */
  date_to?: string;
}

/** 收款分页（`GET /business/erp/receipt/page`，权限 `business:erp:receipt:list`）。 */
export function fetchReceiptPage(query: ReceiptQuery = {}): Promise<PageData<Receipt>> {
  return get<PageData<Receipt>>(`${BASE}/receipt/page`, asParams(query));
}

/** 收款详情（`GET /business/erp/receipt/{id}`，权限 `:detail`，含核销明细）。 */
export function fetchReceiptDetail(id: string): Promise<ReceiptDetail> {
  return get<ReceiptDetail>(`${BASE}/receipt/${id}`);
}

/** 登记收款请求（`receipt::CreateReceiptReq`）。 */
export interface CreateReceiptReq {
  customer_id: string;
  /** 缺省 = 今天（CST） */
  receipt_date?: string | null;
  /** 收款金额，必须 > 0 */
  amount: string | number;
  method?: ReceiptMethod | null;
  bank_account?: string | null;
  remark?: string | null;
}

/** 登记收款（`POST /business/erp/receipt`，权限 `business:erp:receipt:create`）。 */
export function createReceipt(payload: CreateReceiptReq): Promise<ReceiptDetail> {
  return post<ReceiptDetail>(`${BASE}/receipt`, payload);
}

/** 一条核销明细（`receipt::AllocateItemReq`）。 */
export interface AllocateReceiptItemReq {
  receivable_id: string;
  /** 本次核销金额，> 0 且 ≤ 该应收「可核销余额」 */
  amount: string | number;
  remark?: string | null;
}

/**
 * 核销到应收（`POST /business/erp/receipt/{id}/allocate`，权限 `:allocate`）。
 *
 * 一次多张应收；同一张应收在**同一张收款**下只有一行（重复提交 = 改金额）。
 * 后端校验：核销总额 ≤ 收款金额、单张 ≤ 应收可核销余额、客户必须一致、
 * 应收必须 OPEN/PARTIAL、收款必须 DRAFT/CONFIRMED。返回回读后的完整详情。
 */
export function allocateReceipt(
  id: string,
  items: AllocateReceiptItemReq[],
): Promise<ReceiptDetail> {
  return post<ReceiptDetail>(`${BASE}/receipt/${id}/allocate`, { items });
}

/**
 * 状态迁移（`PATCH /business/erp/receipt/{id}/status`，权限 `:status`）。
 *
 * 合法边只有三条：`DRAFT→CONFIRMED`、`CONFIRMED→POSTED`、`DRAFT→CANCELLED`。
 * - `POSTED` 要求核销满（`allocated_amount == amount`），否则 409（凭证只有 借1002/贷1122 两行）；
 * - `CANCELLED` 要求已核销为 0 且给原因（≥4 字）。
 */
export function changeReceiptStatus(
  id: string,
  status: ReceiptStatus,
  reason?: string,
): Promise<ReceiptDetail> {
  return patch<ReceiptDetail>(`${BASE}/receipt/${id}/status`, { status, reason });
}

// ==================== 销售发票（SO-20 后端 / SO-21 前端） ====================

/**
 * 发票状态域（逐字抄 `invoice::STATUSES` / `ck_biz_invoice_status`）。
 *
 * ⚠️ 三个域值都**不能**和别的台账混用：
 * - `VOID` 是发票的作废（收款单 / 应收用 `CANCELLED`，传它给发票接口 400）；
 * - `ISSUED` 是「已开票」（= 收入凭证已生成 + 应收已回写），`POSTED` 只是归档；
 * - 没有任何边回到 `DRAFT`（发票一旦取号不能原地重开）。
 */
export type InvoiceStatus = 'DRAFT' | 'ISSUED' | 'POSTED' | 'VOID';

/**
 * 票种（`invoice::TYPES` / `ck_biz_invoice_type`）。
 *
 * `OUT_RED` 是销项红字票（金额为负），由退货流程生成 ——
 * `POST /invoice` 传 `OUT_RED` 会被 400 拒（`invoice::validate_type`，文案指路退货流程）。
 */
export type InvoiceType = 'OUT' | 'OUT_RED';

/** 来源类型（`invoice::SOURCE_TYPES` / `ck_biz_invoice_source`）。 */
export type InvoiceSourceType = 'SALES_ORDER' | 'SHIPMENT' | 'SALES_OUT' | 'MANUAL';

/**
 * 发票（列表 / 详情单头，后端 `invoice::InvoiceRes`）。
 *
 * 金额是 `numeric(18,2)` → `rust_decimal`，JSON 里出**字符串**（`"1130.0000"`）；
 * 比较 / 累加一律走 `views/business/erp/shared/money.ts` 的整数分，不要 `+`。
 */
export interface Invoice {
  id: string;
  /** 发票号 `IV-yyyyMMdd-000001`（`invoice::NO_PREFIX`）或手填税控号 */
  invoice_no: string;
  invoice_type: InvoiceType | string;
  /** 票种中文（后端 `invoice::type_label`，前端只做兜底） */
  invoice_type_label: string;
  /** 发票代码（可空） */
  invoice_code: string | null;
  /**
   * 票面形式（`biz_invoice.is_electronic`，V0348 / ATT-07）。
   *
   * `true` = 电子票（数电票）：开票时发票影像只收 pdf / ofd / xml；
   * `false` = 纸质票：只收 jpg / png / pdf 扫描件。传错格式后端 400。
   */
  is_electronic: boolean;
  customer_id: string;
  /** 客户名称（LEFT JOIN `biz_customer`，客户软删时是空串） */
  customer_name: string;
  /** 开票日期 `yyyy-MM-dd` */
  invoice_date: string;
  /** 不含税金额 */
  amount: number | string;
  /** 税额 */
  tax_amount: number | string;
  /** 价税合计（= amount + tax_amount，库层 CHECK 恒等） */
  total_with_tax: number | string;
  /** 税率（0.13 = 13%） */
  tax_rate: number | string;
  status: InvoiceStatus | string;
  /** 状态中文（后端 `invoice::status_label`） */
  status_label: string;
  /** 关联应收（手工开票为 null） */
  receivable_id: string | null;
  source_type: InvoiceSourceType | string | null;
  /** 来源单号（从应收开票时 = 应收的来源单号，如出库单号 / MANUAL） */
  source_no: string | null;
  /** 收入凭证（ISSUED 时生成，DRAFT / 手工票可能为 null） */
  voucher_id: string | null;
  remark: string | null;
  created_at: string | null;
  updated_at: string | null;
}

/**
 * 发票详情（`invoice::InvoiceDetailRes` = `InvoiceRes` 展平 + 6 个关联字段）。
 *
 * `receivable_headroom` 是**不算本张**的剩余可开票额度
 * （应收金额 − 其它存活发票的价税合计），前端据此提示「这张还能开多少」。
 */
export interface InvoiceDetail extends Invoice {
  /** 收入 / 红冲凭证号（无凭证或凭证已删时 null） */
  voucher_no: string | null;
  /** 关联应收单号 `AR-yyyyMMdd-000001` */
  receivable_no: string | null;
  /** 关联应收金额 */
  receivable_amount: number | string | null;
  receivable_status: string | null;
  receivable_status_label: string | null;
  receivable_headroom: number | string | null;
}

/**
 * 发票分页入参（逐字抄 `invoice::InvoicePageQuery`）。
 *
 * `invoice_no` 是**模糊**匹配（发票号 + 客户名两列），**不是** `keyword`；
 * 状态 / 票种是白名单（传应收的 `OPEN` 或收款的 `CANCELLED` 直接 400）；
 * 空串一律不发（`?customer_id=` 会被当非法 UUID 拒）。
 */
export interface InvoiceQuery {
  page?: number;
  page_size?: number;
  status?: InvoiceStatus | '';
  invoice_type?: InvoiceType | '';
  customer_id?: string;
  receivable_id?: string;
  invoice_no?: string;
  /** 开票日期下界（含）`yyyy-MM-dd` */
  date_from?: string;
  /** 开票日期上界（含）`yyyy-MM-dd` */
  date_to?: string;
}

/** 发票分页（`GET /business/erp/invoice/page`，权限 `business:erp:invoice:list`）。 */
export function fetchInvoicePage(query: InvoiceQuery = {}): Promise<PageData<Invoice>> {
  return get<PageData<Invoice>>(`${BASE}/invoice/page`, asParams(query));
}

/** 发票详情（`GET /business/erp/invoice/{id}`，权限 `:detail`，含应收 / 凭证号 / 剩余额度）。 */
export function fetchInvoiceDetail(id: string): Promise<InvoiceDetail> {
  return get<InvoiceDetail>(`${BASE}/invoice/${id}`);
}

/**
 * 开票请求（`invoice::CreateInvoiceReq`）。
 *
 * - **从应收开票**：只给 `receivable_id` 即可 —— 客户 / 来源单号自动带出，
 *   `total_with_tax` 缺省 = 应收金额全额（后端 `create` 的 `resolve_amounts`）；
 * - **手工开票**：给 `customer_id` + 金额（`total_with_tax` 或 `amount` + `tax_amount`），
 *   `receivable_id` 留空，不受「不超过应收」约束；
 * - `invoice_no` 留空才自动取 `IV-yyyyMMdd-000001`，手填税控号原样保留（重复 409）。
 */
export interface CreateInvoiceReq {
  receivable_id?: string | null;
  customer_id?: string | null;
  /** 缺省 = 今天（CST） */
  invoice_date?: string | null;
  invoice_no?: string | null;
  invoice_code?: string | null;
  /**
   * 票面形式：`true` 电子票（缺省）/ `false` 纸质票。
   *
   * 决定「开票」动作时发票影像的格式白名单（电子票 pdf/ofd/xml、纸质票 jpg/png/pdf），
   * 后端落 `biz_invoice.is_electronic`（V0348）。**不从 `invoice_code` 推断**。
   */
  is_electronic?: boolean | null;
  /** 只认 `OUT`（红字票走退货流程） */
  invoice_type?: InvoiceType | null;
  amount?: string | number | null;
  tax_amount?: string | number | null;
  /** 从应收开票时缺省 = 应收金额 */
  total_with_tax?: string | number | null;
  /** 缺省 0.13 */
  tax_rate?: number | null;
  source_type?: string | null;
  source_no?: string | null;
  remark?: string | null;
}

/** 开票（`POST /business/erp/invoice`，权限 `business:erp:invoice:create`），落 `DRAFT`。 */
export function createInvoice(payload: CreateInvoiceReq): Promise<InvoiceDetail> {
  return post<InvoiceDetail>(`${BASE}/invoice`, payload);
}

/**
 * 状态迁移（`PATCH /business/erp/invoice/{id}/status`，权限 `:status`）。
 *
 * 合法边只有四条（`state_machine::INVOICE_EDGES`）：
 * `DRAFT→ISSUED`（生成收入凭证 + 回写应收）、`ISSUED→POSTED`（归档）、
 * `DRAFT→VOID` / `ISSUED→VOID`（作废，`reason` ≥4 字，ISSUED 来的必红冲）。
 * `POSTED` 是终态（无作废边）；传 `DRAFT` 一律 400（没有回草稿的边）。
 */
export function changeInvoiceStatus(
  id: string,
  status: InvoiceStatus,
  reason?: string,
): Promise<InvoiceDetail> {
  return patch<InvoiceDetail>(`${BASE}/invoice/${id}/status`, { status, reason });
}
