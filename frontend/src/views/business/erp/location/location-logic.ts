/**
 * 库位管理页（`/business/erp/location`）的**纯逻辑**（不 import vue，`tsx --test` 可直接单测）。
 *
 * # 为什么单独拆一个文件
 *
 * 「筛选行 → 请求参数」「后端枚举 → 中文」「启用位 → 文案/配色」「warehouse_id → 仓库路径」
 * 这些判断一旦漂移，`vue-tsc` 抓不到、e2e 也只看得见「界面长什么样」。放这里，
 * `tests/unit/erp-location.test.ts` 先红，页面只负责渲染。
 *
 * # 字段口径（逐字抄后端，不许猜）
 *
 * | 语义 | 字段 | 出处 |
 * |------|------|------|
 * | 库位编码 | `location_code` | `dto.rs::LocationRes` |
 * | 库位名称 | `location_name` | 同上（可空） |
 * | 所属仓库 | `warehouse_id` | 同上 —— **响应里没有 `warehouse_name`**，名称只能前端用 `warehouse/tree` 翻译 |
 * | 库位类型 | `location_type` | `dto.rs::LOCATION_TYPES`：STORAGE/PICKING/RECEIVING/SHIPPING/QUARANTINE/SCRAP |
 * | 是否默认 | `is_default` | `dto.rs::LocationRes::is_default`（`bool`，库列是 `smallint` 1/0） |
 * | 容量 | `capacity` | `numeric(18,4)`，**`null` = 不限** |
 * | 启用状态 | `status` | `smallint`：1 启用 / 0 停用（**不是** 布尔、也不是 2 值枚举） |
 *
 * 分页接口：`GET /business/erp/location/page?warehouse_id=&keyword=&location_type=&status=&page=&page_size=`
 *（`handler.rs::location_page` + `model.rs::location_page`），**服务端分页**、排序固定
 * `sort_order ASC, location_code ASC`（所以前端不提供排序，别做一个后端不认的排序按钮）。
 *
 * # 与仓库页的关系
 *
 * 单仓维护（新建 / 编辑 / 删除）在 W4a 的仓库页右侧已有；本页是**跨仓库总览**：
 * 服务端分页 + 仓库/关键词/类型/启用位筛选 + 详情抽屉。所以类型映射这类口径**不另抄一份**，
 * 直接复用 `../warehouse/warehouse-tree` 的常量与标签函数（单一来源，后端改枚举时只改一处）。
 */
import type { Location, LocationType, WarehouseTreeNode } from '@/api/business/erp';
import {
  LOCATION_TYPE_OPTIONS,
  findWarehouse,
  flattenWarehouses,
  locationTypeLabel,
  warehousePathText,
} from '../warehouse/warehouse-tree';

/** 库位类型下拉项 / 类型中文（复用仓库页的单一来源，不重复抄枚举） */
export { LOCATION_TYPE_OPTIONS, locationTypeLabel };

/** 每页条数（与后端 `DEFAULT_PAGE_SIZE` 一致；切页走后端，不在前端切片） */
export const LOCATION_PAGE_SIZE = 20;

/** 库位筛选行状态（`''` = 不限；`status` 用字符串是为了贴合 `t-select` 的 value） */
export interface LocationFilterState {
  /** 所属仓库 id（`''` = 全部仓库，跨仓库总览的默认态） */
  warehouse_id: string;
  /** 关键词（编码 / 名称模糊，后端 `LIKE` 两侧匹配） */
  keyword: string;
  /** 库位类型（`''` = 全部类型） */
  location_type: LocationType | '';
  /** 启用状态（`''` = 全部 / `'1'` 启用 / `'0'` 停用） */
  status: '' | '1' | '0';
}

/** 空筛选（重置用；每次返回新对象，避免共享引用被就地改） */
export function emptyLocationFilter(): LocationFilterState {
  return { warehouse_id: '', keyword: '', location_type: '', status: '' };
}

/**
 * 筛选行 → `GET /erp/location/page` 的参数。
 *
 * 三条口径：
 * 1. `''` / 空串一律**整键缺席**（不是发空串）—— 后端把 `?keyword=` 当「等于空串」过滤，发了就查出 0 行；
 * 2. `warehouse_id` 只在真的选了仓库时才带上（不选 = 跨仓库总览）；
 * 3. `status` **用 `!== ''` 判定，不能用真假值** —— `'0'`（停用）是真值、`0`（number）是假值，
 *    写成 `if (filter.status)` 再 `Number(...)` 会在某个环节把「只看停用」静默变成「全部」，这是最容易漏的坑。
 */
export function buildLocationQuery(
  filter: LocationFilterState,
  page: number,
  pageSize: number = LOCATION_PAGE_SIZE,
): Record<string, unknown> {
  const query: Record<string, unknown> = { page, page_size: pageSize };
  if (filter.warehouse_id) {
    query.warehouse_id = filter.warehouse_id;
  }
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.location_type) {
    query.location_type = filter.location_type;
  }
  if (filter.status !== '') {
    query.status = Number(filter.status);
  }
  return query;
}

/** 是否处于筛选态（空态文案要分「还没有库位」和「没有匹配」） */
export function isLocationFiltered(filter: LocationFilterState): boolean {
  return (
    Boolean(filter.warehouse_id)
    || Boolean(filter.keyword.trim())
    || Boolean(filter.location_type)
    || filter.status !== ''
  );
}

// ==================== 启用状态 / 类型配色 ====================

/** 启用状态下拉（`value` 是字符串：`'0'` 是**有效选项**，不能被当真值过滤掉） */
export const LOCATION_STATUS_OPTIONS: Array<{ value: '1' | '0'; label: string }> = [
  { value: '1', label: '启用' },
  { value: '0', label: '停用' },
];

/**
 * 启用状态 → 中文（`smallint` 1/0）。
 *
 * 未知值不静默显示空白：回 `启用状态未知（N）`，脏数据在界面上是可见的。
 */
export function locationStatusLabel(status: number): string {
  if (status === 1) {
    return '启用';
  }
  if (status === 0) {
    return '停用';
  }
  return `未知（${status}）`;
}

/** 启用状态 → `t-tag` 主题（停用走 `default` 灰，别用 danger：停用是正常业务态，不是错误） */
export function locationStatusTheme(status: number): 'success' | 'default' {
  return status === 1 ? 'success' : 'default';
}

/** 库位类型 → `t-tag` 主题（只取 TDesign 主题名，不写裸 hex） */
export function locationTypeTheme(
  type: string,
): 'primary' | 'success' | 'warning' | 'danger' | 'default' {
  switch (type) {
    case 'PICKING':
      return 'primary';
    case 'RECEIVING':
      return 'success';
    case 'SHIPPING':
      return 'warning';
    case 'QUARANTINE':
      return 'warning';
    case 'SCRAP':
      return 'danger';
    case 'STORAGE':
    default:
      return 'default';
  }
}

// ==================== 仓库维度 ====================

/**
 * 「仓库」筛选下拉的选项：**全部仓库**（不只是叶子）。
 *
 * 为什么筛选放开到非叶子：库位按口径只能挂在叶子仓库下，但历史数据 / 手工 SQL 里可能有挂在
 * 非叶子仓库的库位行；筛选器如果只列叶子，这些行就**永远筛不出来**，而列表又明明显示着它们 ——
 * 用户会以为是筛选坏了。新建 / 编辑弹窗里仍然只列叶子（那是写入口径，后端 66005 兜底）。
 *
 * 文案带完整路径（`总仓 / 分仓（WH02）`）：同名仓库平铺之后也得能分清是哪一层。
 */
export interface WarehouseFilterOption {
  /** 值：仓库 id */
  value: string;
  /** 文案：完整路径（code 兜底） */
  label: string;
  /** 是否叶子（下拉里给个「可建库位」的标记用；筛选本身不排除非叶子） */
  is_leaf: boolean;
}

/** 仓库筛选下拉选项（全部仓库，带层级路径） */
export function warehouseFilterOptions(nodes: WarehouseTreeNode[]): WarehouseFilterOption[] {
  return flattenWarehouses(nodes).map(({ node }) => ({
    value: node.id,
    label: `${warehousePathText(nodes, node.id) || node.warehouse_name}（${node.warehouse_code}）`,
    is_leaf: node.is_leaf,
  }));
}

/**
 * `warehouse_id` → 可读仓库名。
 *
 * 列表接口**不下发** `warehouse_name`，只能拿 `warehouse/tree` 反查；查不到（树还没加载、
 * 或该仓库已被删）就回 uuid 前 8 位 —— 显示空白会让人以为「这行没仓库」。
 */
export function warehouseNameOf(nodes: WarehouseTreeNode[], warehouseId: string | null): string {
  const id = (warehouseId ?? '').trim();
  if (!id) {
    return '—';
  }
  const hit = findWarehouse(nodes, id);
  if (hit) {
    return warehousePathText(nodes, id) || hit.warehouse_name;
  }
  return id.length > 8 ? id.slice(0, 8) : id;
}

// ==================== 行视图 ====================

/** 列表行视图（`Location` 原字段不动，纯附加渲染用的派生值） */
export interface LocationRowView extends Location {
  /** 所属仓库路径文案（列表接口没有 `warehouse_name`，这里已翻译好） */
  warehouse_text: string;
  /** 库位类型中文 */
  type_label: string;
  /** 启用状态中文 */
  status_label: string;
  /** 容量文案（`null` = 不限） */
  capacity_text: string;
}

/** 容量文案：`numeric(18,4)` 可能是 `"0.0000"` / `0` / `null`，统一在这里收口 */
export function capacityText(capacity: string | number | null | undefined): string {
  if (capacity === null || capacity === undefined || capacity === '') {
    return '不限';
  }
  return String(capacity);
}

/** 列表行 → 视图行（翻译仓库名 / 类型 / 状态） */
export function toLocationRow(row: Location, nodes: WarehouseTreeNode[]): LocationRowView {
  return {
    ...row,
    warehouse_text: warehouseNameOf(nodes, row.warehouse_id),
    type_label: locationTypeLabel(row.location_type),
    status_label: locationStatusLabel(row.status),
    capacity_text: capacityText(row.capacity),
  };
}

/** 空值统一显示 `—`（表格里不允许出现 `null` 字面量） */
export function dash(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text === '' ? '—' : text;
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（详情抽屉用；纯本地时间，不做时区换算） */
export function formatTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return '—';
  }
  const pad = (n: number): string => String(n).padStart(2, '0');
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
    + ` ${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}
