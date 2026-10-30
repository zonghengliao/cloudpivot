/**
 * ERP 仓库 / 库位接口（W1c 已上线的 10 个接口里本页要用的那几条）。
 *
 * 路径与后端 `backend/src/business/erp/handler.rs` 的
 * `#[nik::get/post/put/delete("/business/erp/warehouse|location/...")]` 逐条对应
 * （注解不含 `/api`，前端写全路径）。
 *
 * # 三条取值口径（照后端 `dto.rs`，不自己发明）
 *
 * 1. **枚举一律大写字面量**：`warehouse_type` ∈ `NORMAL|RAW|FINISHED|SCRAP|IN_TRANSIT`（5 个），
 *    `location_type` ∈ `STORAGE|PICKING|RECEIVING|SHIPPING|QUARANTINE|SCRAP`（6 个）；
 * 2. **`level` 不由调用方随手指**：顶级仓库必须 1，有上级必须 2 或 3，且最多 3 层
 *    （后端 `CreateWarehouseReq::validate`）；`warehouse_path` 由后端按父路径拼，**请求体里没有这个字段**；
 * 3. **`is_leaf` 是派生值，只读**：建子仓库时后端把上级 `is_leaf` 置 0，删掉最后一个下级再复位 1
 *    （`handler.rs` 建/删事务里各一条 UPDATE）。所以 `Create/UpdateWarehouseReq` 都没有该字段 ——
 *    前端把它当「结果」展示，不能当「开关」提交。
 *
 * # 树接口的出参形状
 *
 * `GET /warehouse/tree` 的每一项是 `WarehouseRes` 平铺 + `children` 递归（后端
 * `WarehouseTreeNode` 用 `#[serde(flatten)]` 摊平，**没有** `node` 这一层）。
 * 树里不带 `child_count`（那是分页接口的派生字段），判断叶子只看 `is_leaf`。
 *
 * # 写接口的「缺席 ≠ 清空」
 *
 * 编辑仓库 / 库位用 `Update*Req`：字段**缺席 = 保持原值**，显式 `null` = 清空。
 * 所以编辑表单里被清空的可空字段（地址 / 负责人）必须显式传 `null`，不能省掉。
 */
import { del, get, post, put, type PageData } from '@/api/request';

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

/** 仓库分页（编码 / 名称关键词 + 类型 / 层级 / 上级 / 启用位） */
export function fetchWarehousePage(query: {
  keyword?: string;
  warehouse_type?: string;
  level?: number;
  parent_id?: string;
  status?: number;
  page?: number;
  page_size?: number;
}): Promise<PageData<Warehouse[]>> {
  return get<PageData<Warehouse[]>>(`${BASE}/warehouse/page`, asParams(query));
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
