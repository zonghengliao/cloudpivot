/**
 * 仓库树 / 库位页的**纯逻辑**（不 import vue，可被 `tsx --test` 直接单测）。
 *
 * 拆出来的理由：页面里所有「树怎么折、叶子怎么判、请求体长什么样」的判断都在这里，
 * `.vue` 只负责渲染和调接口。后端改枚举 / 改字段名时，`tests/unit/warehouse-tree.test.ts` 先红。
 *
 * # 三条与后端对齐的硬口径
 *
 * 1. **最多 3 层**（`handler.rs` `ERP_WAREHOUSE_LEVEL_EXCEEDED` = 66004）：`level` 只能是 1/2/3；
 * 2. **库位只能挂叶子仓库**（`ERP_WAREHOUSE_NOT_LEAF` = 66005）：`is_leaf = 0` 的仓库不能建库位；
 * 3. **`is_leaf` 是派生只读**：它由「有没有下级仓库」决定（建子仓库置 0 / 删完下级复位 1），
 *    后端 `Create/UpdateWarehouseReq` 都没有这个字段 —— 所以弹窗里只展示、不提交。
 */
import type {
  CreateLocationReq,
  CreateWarehouseReq,
  Location,
  LocationType,
  UpdateLocationReq,
  UpdateWarehouseReq,
  Warehouse,
  WarehouseTreeNode,
  WarehouseType,
} from '@/api/business/erp';

/** 仓库层级上限（后端 66004 的上限） */
export const MAX_WAREHOUSE_LEVEL = 3;

/** 层级中文（树节点上的层级标签） */
const LEVEL_LABELS: Record<number, string> = {
  1: '总仓',
  2: '分仓',
  3: '库房',
};

/** 层级标签 */
export function levelLabel(level: number): string {
  return LEVEL_LABELS[level] ?? `第 ${level} 层`;
}

/** 层级标签配色（给 `t-tag` 用；只用 TDesign 主题名，不写裸 hex） */
export function levelTheme(level: number): 'primary' | 'success' | 'warning' | 'default' {
  if (level === 1) return 'primary';
  if (level === 2) return 'success';
  if (level === 3) return 'warning';
  return 'default';
}

/** 仓库类型中文（与后端 `WAREHOUSE_TYPES` 的 5 个字面量一一对应） */
const WAREHOUSE_TYPE_LABELS: Record<WarehouseType, string> = {
  NORMAL: '普通仓',
  RAW: '原料仓',
  FINISHED: '成品仓',
  SCRAP: '报废仓',
  IN_TRANSIT: '在途仓',
};

/** 仓库类型标签 */
export function warehouseTypeLabel(type: string): string {
  return WAREHOUSE_TYPE_LABELS[type as WarehouseType] ?? type;
}

/** 仓库类型下拉项（顺序与后端常量一致） */
export const WAREHOUSE_TYPE_OPTIONS: Array<{ value: WarehouseType; label: string }> = (
  Object.keys(WAREHOUSE_TYPE_LABELS) as WarehouseType[]
).map((value) => ({ value, label: WAREHOUSE_TYPE_LABELS[value] }));

/** 库位类型中文（与后端 `LOCATION_TYPES` 的 6 个字面量一一对应） */
const LOCATION_TYPE_LABELS: Record<LocationType, string> = {
  STORAGE: '存储位',
  PICKING: '拣货位',
  RECEIVING: '收货位',
  SHIPPING: '发货位',
  QUARANTINE: '待检位',
  SCRAP: '报废位',
};

/** 库位类型标签 */
export function locationTypeLabel(type: string): string {
  return LOCATION_TYPE_LABELS[type as LocationType] ?? type;
}

/** 库位类型下拉项（顺序与后端常量一致） */
export const LOCATION_TYPE_OPTIONS: Array<{ value: LocationType; label: string }> = (
  Object.keys(LOCATION_TYPE_LABELS) as LocationType[]
).map((value) => ({ value, label: LOCATION_TYPE_LABELS[value] }));

// ==================== 树：展平 / 折叠 / 搜索 ====================

/** 展平后的一行（带深度，方便断言「至少 3 层」） */
export interface FlatWarehouse {
  /** 节点本体 */
  node: WarehouseTreeNode;
  /** 深度（1 起） */
  depth: number;
}

/** 深度优先展平（父在前、子的顺序照后端的 sort_order） */
export function flattenWarehouses(nodes: WarehouseTreeNode[], depth = 1): FlatWarehouse[] {
  const out: FlatWarehouse[] = [];
  for (const node of nodes) {
    out.push({ node, depth });
    if (node.children?.length) {
      out.push(...flattenWarehouses(node.children, depth + 1));
    }
  }
  return out;
}

/** 树的最大深度（验收②「至少 3 层」直接量这个数） */
export function treeDepth(nodes: WarehouseTreeNode[]): number {
  return flattenWarehouses(nodes).reduce((max, row) => Math.max(max, row.depth), 0);
}

/** 全部节点 id（按展平顺序） */
export function allWarehouseIds(nodes: WarehouseTreeNode[]): string[] {
  return flattenWarehouses(nodes).map((row) => row.node.id);
}

/**
 * 首屏默认展开：只展开「第 1 层里有孩子」的节点。
 *
 * 不 `expandAll`：仓库多的时候全展开又长又慢，而展开第 2 层（分仓）已经能一眼看到库区。
 */
export function initialExpandedIds(nodes: WarehouseTreeNode[]): string[] {
  return nodes.filter((node) => node.children?.length > 0).map((node) => node.id);
}

/**
 * 逐节点独立折叠：`expanded` 里没有它 → 收起；有它 → 展开。
 *
 * **只动自己这一个 id**，不联级（联级展开/收起是 `expandMutex` / `expandAll` 的行为，本页不要）。
 */
export function toggleExpandedId(expanded: string[], id: string): string[] {
  return expanded.includes(id) ? expanded.filter((item) => item !== id) : [...expanded, id];
}

/**
 * 节点唯一 key（`t-tree` 的 `value` 字段就用 `id`；这里是给 `data-*` / `:key` 兜底的）。
 *
 * 三档兜底，任何一档都不许返回空串、也不许返回 `undefined`：
 * 1. `id`（正常路径，后端 UUID）；
 * 2. `warehouse_path`（物化路径，含 id）；
 * 3. `warehouse_code` / 空 code → `wh-<序号>`。
 *
 * 为什么要兜底：草稿 / 新增未落库 / e2e mock 数据里 `id` 或 `code` 可能是空串，
 * 直接拿来当 key 会得到 `undefined` 或重复 key，`t-tree` 会把两个节点渲染成一个。
 */
export function warehouseNodeKey(
  node: {
    id?: string | null;
    warehouse_code?: string | null;
    warehouse_path?: string | null;
  },
  index = 0,
): string {
  const id = (node.id ?? '').trim();
  if (id) {
    return `wh:${id}`;
  }
  const path = (node.warehouse_path ?? '').trim();
  if (path) {
    return `path:${path}`;
  }
  const code = (node.warehouse_code ?? '').trim();
  if (code) {
    return `code:${code}`;
  }
  return `wh:#${index}`;
}

/**
 * 整棵树的 key 列表（展平后逐节点生成），**保证互不相同**。
 *
 * 重复 key 的成因就两种：id 缺失（都退到空 code）或同一 code 挂了两次（数据脏）。
 * 这里不抛错也不静默覆盖 —— 给后来者补 `~<序号>` 后缀，第一个保持原样，
 * 这样「本来正常的节点」key 不变，只有重复的那几个被区分开。
 */
export function warehouseTreeKeys(nodes: WarehouseTreeNode[]): string[] {
  const seen = new Map<string, number>();
  const out: string[] = [];
  flattenWarehouses(nodes).forEach(({ node }, index) => {
    const base = warehouseNodeKey(node, index);
    const hit = seen.get(base) ?? 0;
    seen.set(base, hit + 1);
    out.push(hit === 0 ? base : `${base}~${hit}`);
  });
  return out;
}

/**
 * 叶子判定（页面展示 + 建库位闸门共用）。
 *
 * 口径：**`children` 非空就是非叶子**，否则看后端给的 `is_leaf`。
 * 为什么以 `children` 优先：`is_leaf` 是派生字段，后端建/删子仓库时才更新那一行，
 * 若某次事务漏了更新，`children` 才是真的（树接口一次把下级全带回来了）。
 * 两者都没有的节点（列表接口的行）按 `is_leaf` 走，不再二次猜。
 */
export function resolveIsLeaf(
  node: Pick<Warehouse, 'is_leaf'> & { children?: WarehouseTreeNode[] | null },
): boolean {
  if (node.children && node.children.length > 0) {
    return false;
  }
  return node.is_leaf;
}

/**
 * 点击节点 → 选中仓库 id。
 *
 * 传 `null` / 空 id 时返回 `null`（= 取消选中，右侧回空态），不抛错：
 * 树在刷新中被点掉（node 变 undefined）是常态，页面不该因此白屏。
 */
export function selectWarehouseId(
  node: { id?: string | null } | null | undefined,
): string | null {
  const id = (node?.id ?? '').trim();
  return id ? id : null;
}

/** 按 id 找节点（树里任意一层） */
export function findWarehouse(nodes: WarehouseTreeNode[], id: string): WarehouseTreeNode | null {
  for (const node of nodes) {
    if (node.id === id) {
      return node;
    }
    const hit = findWarehouse(node.children ?? [], id);
    if (hit) {
      return hit;
    }
  }
  return null;
}

/** 搜索结果的树 + 需要自动展开的 id 列表 */
export interface WarehouseFilterResult {
  /** 过滤后的树（命中节点 + 它们的祖先） */
  tree: WarehouseTreeNode[];
  /** 需要自动展开的 id（命中的节点本身不展开，只展开它们的祖先） */
  expandedIds: string[];
  /** 命中个数（0 时页面显示空态） */
  matchedCount: number;
}

/**
 * 按关键词过滤仓库树（编码 / 名称，忽略大小写）。
 *
 * 保留「命中的节点 + 它的全部祖先」——否则命中一个第 3 层库区时，父节点被剪掉，
 * 整个子树会静默消失（后端 `assemble_tree` 注释里记的同一个坑）。
 */
export function filterWarehouseTree(nodes: WarehouseTreeNode[], keyword: string): WarehouseFilterResult {
  const needle = keyword.trim().toLowerCase();
  if (!needle) {
    return { tree: nodes, expandedIds: [], matchedCount: 0 };
  }
  const expandedIds: string[] = [];
  let matchedCount = 0;

  const walk = (list: WarehouseTreeNode[]): WarehouseTreeNode[] => {
    const kept: WarehouseTreeNode[] = [];
    for (const node of list) {
      const children = walk(node.children ?? []);
      const self =
        node.warehouse_name.toLowerCase().includes(needle) ||
        node.warehouse_code.toLowerCase().includes(needle);
      if (!self && children.length === 0) {
        continue;
      }
      if (self) {
        matchedCount += 1;
      }
      kept.push({ ...node, children });
    }
    return kept;
  };

  const tree = walk(nodes);
  // 展开 id 用**前序**收集（祖先在前），顺序稳定便于断言；展开本身与顺序无关
  const collectExpandable = (list: WarehouseTreeNode[]): void => {
    for (const node of list) {
      if ((node.children ?? []).length > 0) {
        expandedIds.push(node.id);
        collectExpandable(node.children);
      }
    }
  };
  collectExpandable(tree);

  return { tree, expandedIds, matchedCount };
}

/** 「当前选中」的路径文字（`总仓 / 华东分仓 / A区`），面包屑感的副标题 */
export function warehousePathText(nodes: WarehouseTreeNode[], id: string): string {
  const walk = (list: WarehouseTreeNode[], trail: string[]): string[] | null => {
    for (const node of list) {
      const next = [...trail, node.warehouse_name];
      if (node.id === id) {
        return next;
      }
      const hit = walk(node.children ?? [], next);
      if (hit) {
        return hit;
      }
    }
    return null;
  };
  return (walk(nodes, []) ?? []).join(' / ');
}

// ==================== 层级 / 叶子约束 ====================
/**
 * 「父仓库」树选择器的数据：**剪掉第 3 层节点**（它们不能再有下级）。
 *
 * 为什么剪枝而不是把 disabled 传进去：`t-tree-select` 的 `treeProps.disabled` 只吃布尔，
 * 想按节点禁用得走 `valueType` / 自定义渲染那套（还容易被误选）。
 * 层级 ≤2 的节点天然构成连通子树，剪掉第 3 层不会让任何父节点失去挂载点。
 */
export function parentPickableTree(nodes: WarehouseTreeNode[]): WarehouseTreeNode[] {
  return nodes
    .filter((node) => canHaveChildren(node))
    .map((node) => ({ ...node, children: parentPickableTree(node.children ?? []) }));
}

/** 扁平下拉选项（`t-select` 用） */
export interface FlatOption {
  /** 值（仓库 id） */
  value: string;
  /** 文案（带层级路径，避免同名仓库分不清） */
  label: string;
}

/**
 * 「所属仓库」下拉选项：**只给叶子仓库**（库位只能挂叶子，66005）。
 *
 * 用扁平下拉而不是树选择器：非叶子节点在树里既不能选又不能删（它们撑着结构），
 * 摆在用户面前只会制造「点不动」的困惑。文案带上完整路径（`总仓 / 华东分仓 / A区`），
 * 平铺之后也分得清是哪个。
 */
export function leafWarehouseOptions(nodes: WarehouseTreeNode[]): FlatOption[] {
  return flattenWarehouses(nodes)
    .filter((row) => row.node.is_leaf)
    .map((row) => ({
      value: row.node.id,
      label: `${warehousePathText(nodes, row.node.id) || row.node.warehouse_name}（${row.node.warehouse_code}）`,
    }));
}


/**
 * 新建仓库的层级：由所选上级推导（无上级 = 1）。
 *
 * 后端 `CreateWarehouseReq::validate` 要求 `(parent_id 为空) = (level = 1)`，
 * 所以这个值**不能**让用户手填，只能跟着上级变。
 */
export function deriveWarehouseLevel(parent: Warehouse | null): number {
  return parent ? parent.level + 1 : 1;
}

/** 该仓库还能不能有下级（第 3 层到底了 → 不能再建子仓库） */
export function canHaveChildren(warehouse: Warehouse): boolean {
  return warehouse.level < MAX_WAREHOUSE_LEVEL;
}

/** 「新建库位」按钮的可用性 + 禁用时的说明文案 */
export interface LocationCreateGate {
  /** 是否禁用 */
  disabled: boolean;
  /** 禁用原因（`disabled = false` 时为空串） */
  reason: string;
}

/**
 * **本页的硬约束**:库位只能挂叶子仓库。
 *
 * 后端 `location_create` 同样会拦（`400 + 66005`），这里只是把话提前说到脸上：
 * 非叶子仓库时按钮置灰 + tooltip，省掉一次「填完表单才报错」。
 *
 * 注意：`is_leaf` 是派生的（有下级就是非叶子），**没法在弹窗里勾成叶子** ——
 * 想让它变成叶子只能删掉它的全部下级仓库，文案如实说这一点，不承诺「设为叶子」这种不存在的开关。
 */
export function locationCreateGate(warehouse: Warehouse | null | undefined): LocationCreateGate {
  if (!warehouse) {
    return { disabled: true, reason: '先在左侧选中一个仓库' };
  }
  if (!warehouse.is_leaf) {
    return {
      disabled: true,
      reason: `「${warehouse.warehouse_name}」还有下级仓库，库位只能挂在叶子仓库下：请选中它下面的叶子仓库，或删掉它的全部下级仓库（它会自动变回叶子）`,
    };
  }
  return { disabled: false, reason: '' };
}

// ==================== 仓库表单 ====================

/** 仓库弹窗表单状态 */
export interface WarehouseFormState {
  /** 编码（新建必填，编辑只读） */
  warehouse_code: string;
  /** 名称 */
  warehouse_name: string;
  /** 类型 */
  warehouse_type: WarehouseType;
  /** 上级仓库 id（`null` = 顶级） */
  parent_id: string | null;
  /** 负责人 */
  manager: string;
  /** 地址 */
  address: string;
  /** 启用状态 1/0 */
  status: number;
  /** 备注 */
  remark: string;
  /** **只读展示**：是否叶子（派生值，不提交） */
  is_leaf: boolean;
}

/** 空表单（新建用） */
export function emptyWarehouseForm(parent?: Warehouse | null): WarehouseFormState {
  return {
    warehouse_code: '',
    warehouse_name: '',
    warehouse_type: 'NORMAL',
    parent_id: parent?.id ?? null,
    manager: '',
    address: '',
    status: 1,
    remark: '',
    is_leaf: true,
  };
}

/** 由详情回填表单（编辑用） */
export function warehouseFormFromModel(warehouse: Warehouse): WarehouseFormState {
  return {
    warehouse_code: warehouse.warehouse_code,
    warehouse_name: warehouse.warehouse_name,
    warehouse_type: warehouse.warehouse_type,
    parent_id: warehouse.parent_id,
    manager: warehouse.manager ?? '',
    address: warehouse.address ?? '',
    status: warehouse.status,
    remark: warehouse.remark ?? '',
    is_leaf: warehouse.is_leaf,
  };
}

/** 表单错误（键 = 字段名，值 = 提示） */
export type FieldErrors = Record<string, string>;

/**
 * 校验仓库表单（长度上限照后端 `require_text` / `check_optional_text`）。
 *
 * @param form 表单
 * @param parent 所选上级（用于层级判定；`null` = 顶级）
 */
export function validateWarehouseForm(form: WarehouseFormState, parent: Warehouse | null): FieldErrors {
  const errors: FieldErrors = {};
  const code = form.warehouse_code.trim();
  const name = form.warehouse_name.trim();
  if (!code) {
    errors.warehouse_code = '请填仓库编码';
  } else if (code.length > 40) {
    errors.warehouse_code = '编码最多 40 个字符';
  }
  if (!name) {
    errors.warehouse_name = '请填仓库名称';
  } else if (name.length > 120) {
    errors.warehouse_name = '名称最多 120 个字符';
  }
  if (form.manager.trim().length > 50) {
    errors.manager = '负责人最多 50 个字符';
  }
  if (form.address.trim().length > 255) {
    errors.address = '地址最多 255 个字符';
  }
  if (parent && !canHaveChildren(parent)) {
    errors.parent_id = `「${parent.warehouse_name}」已经是第 ${MAX_WAREHOUSE_LEVEL} 层，最多 ${MAX_WAREHOUSE_LEVEL} 层`;
  }
  return errors;
}

/**
 * 组装新建仓库请求体。
 *
 * - `level` 由上级推导，保证与后端 `(parent_id 为空) = (level = 1)` 自洽；
 * - **不带 `is_leaf`**（后端没有这个入参，带了也会被 `deny unknown fields` 之外的静默忽略，
 *   但契约上没有就不该发）；
 * - 空串一律转 `null`，别让后端把 `''` 当有效值存进 `VARCHAR`。
 */
export function buildWarehouseCreatePayload(
  form: WarehouseFormState,
  parent: Warehouse | null,
): CreateWarehouseReq {
  return {
    warehouse_code: form.warehouse_code.trim(),
    warehouse_name: form.warehouse_name.trim(),
    warehouse_type: form.warehouse_type,
    parent_id: parent?.id ?? null,
    level: deriveWarehouseLevel(parent),
    manager: form.manager.trim() || null,
    address: form.address.trim() || null,
    status: form.status,
    remark: form.remark.trim() || null,
  };
}

/**
 * 组装编辑仓库请求体：**只带真改过的字段**（缺席 = 不改）。
 *
 * 可空字段被清空时必须显式 `null`（否则后端按「缺席」保持原值，用户会发现清不掉地址）。
 */
export function buildWarehouseUpdatePayload(
  form: WarehouseFormState,
  origin: Warehouse,
): UpdateWarehouseReq {
  const payload: UpdateWarehouseReq = {};
  const name = form.warehouse_name.trim();
  if (name !== origin.warehouse_name) {
    payload.warehouse_name = name;
  }
  if (form.warehouse_type !== origin.warehouse_type) {
    payload.warehouse_type = form.warehouse_type;
  }
  const manager = form.manager.trim() || null;
  if (manager !== (origin.manager ?? null)) {
    payload.manager = manager;
  }
  const address = form.address.trim() || null;
  if (address !== (origin.address ?? null)) {
    payload.address = address;
  }
  const remark = form.remark.trim() || null;
  if (remark !== (origin.remark ?? null)) {
    payload.remark = remark;
  }
  if (form.status !== origin.status) {
    payload.status = form.status;
  }
  return payload;
}

/** 编辑请求体是否为空（一个字段都没改 → 直接跳过 PUT，省一次写库） */
export function isEmptyUpdate(payload: UpdateWarehouseReq | UpdateLocationReq): boolean {
  return Object.keys(payload).length === 0;
}

// ==================== 库位表单 ====================

/** 库位弹窗表单状态 */
export interface LocationFormState {
  /** 编码（编辑只读） */
  location_code: string;
  /** 名称 */
  location_name: string;
  /** 所属仓库（新建时必填，编辑时只读 —— 后端不允许换仓库） */
  warehouse_id: string;
  /** 类型 */
  location_type: LocationType;
  /** 启用状态 1/0 */
  status: number;
}

/** 空表单（新建用，默认挂在当前选中的仓库下） */
export function emptyLocationForm(warehouseId = ''): LocationFormState {
  return {
    location_code: '',
    location_name: '',
    warehouse_id: warehouseId,
    location_type: 'STORAGE',
    status: 1,
  };
}

/** 由详情回填表单（编辑用） */
export function locationFormFromModel(location: Location): LocationFormState {
  return {
    location_code: location.location_code,
    location_name: location.location_name ?? '',
    warehouse_id: location.warehouse_id,
    location_type: location.location_type,
    status: location.status,
  };
}

/**
 * 校验库位表单。
 *
 * @param form 表单
 * @param warehouse 所选所属仓库（非叶子时给 66005 同样的提示）
 */
export function validateLocationForm(form: LocationFormState, warehouse: Warehouse | null): FieldErrors {
  const errors: FieldErrors = {};
  const code = form.location_code.trim();
  if (!code) {
    errors.location_code = '请填库位编码';
  } else if (code.length > 40) {
    errors.location_code = '编码最多 40 个字符';
  }
  if (!form.warehouse_id) {
    errors.warehouse_id = '请选所属仓库';
  } else if (warehouse && !warehouse.is_leaf) {
    errors.warehouse_id = locationCreateGate(warehouse).reason;
  }
  if (form.location_name.trim().length > 120) {
    errors.location_name = '名称最多 120 个字符';
  }
  return errors;
}

/** 组装新建库位请求体 */
export function buildLocationCreatePayload(form: LocationFormState): CreateLocationReq {
  return {
    location_code: form.location_code.trim(),
    warehouse_id: form.warehouse_id,
    location_name: form.location_name.trim() || null,
    location_type: form.location_type,
    status: form.status,
  };
}

/** 组装编辑库位请求体（只带真改过的字段） */
export function buildLocationUpdatePayload(
  form: LocationFormState,
  origin: Location,
): UpdateLocationReq {
  const payload: UpdateLocationReq = {};
  const name = form.location_name.trim() || null;
  if (name !== (origin.location_name ?? null)) {
    payload.location_name = name;
  }
  if (form.location_type !== origin.location_type) {
    payload.location_type = form.location_type;
  }
  if (form.status !== origin.status) {
    payload.status = form.status;
  }
  return payload;
}

// ==================== 库位列表（前端分页） ====================

/**
 * 前端分页切片。
 *
 * `location/list` 这接口**不分页**（后端一次返回该仓库全部库位），所以分页只能在前端做：
 * 页码越界时夹回最后一页，避免「在第 3 页删光了之后页面空白」。
 */
export function paginateRows<T>(rows: T[], page: number, pageSize: number): T[] {
  if (pageSize <= 0) {
    return rows;
  }
  const totalPages = Math.max(1, Math.ceil(rows.length / pageSize));
  const safePage = Math.min(Math.max(1, page), totalPages);
  const start = (safePage - 1) * pageSize;
  return rows.slice(start, start + pageSize);
}

/** 夹紧页码（配合 {@link paginateRows}，让 `t-pagination` 的高亮页与切片一致） */
export function clampPage(page: number, total: number, pageSize: number): number {
  const totalPages = Math.max(1, Math.ceil(total / Math.max(1, pageSize)));
  return Math.min(Math.max(1, page), totalPages);
}
