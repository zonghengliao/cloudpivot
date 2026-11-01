/**
 * 审批详情 / 待办卡片的**主数据译名**（`supplier_id` → 「示例供应商（种子）」）。
 *
 * # 为什么在前端查（方案 A）
 *
 * `form_data` 是发起时**冻结**的 JSON，里面只有 id，没有名称
 * （`purchase_handler::purchase_form_data` 实测：`supplier_id` / `warehouse_id` / `create_by`，无 `*_name`）。
 * 后端补名称要动 `form_data` 的写入口径 + 存量单据回填，那是另一张卡的事（见
 * `docs/P1-FIX-审批详情格式化-证据.md` 的「后续」一节）。本卡先用**一次批量查主数据**把名称补上：
 * 详情页一次 `Promise.allSettled` 拉 4 张主数据表，构建 `id → name` 映射，页面只查内存。
 *
 * # 关键取舍：**权限不够时静默降级**
 *
 * 审批岗位（`DEPT_LEADER` / `PURCHASE_MANAGER` / …）在生产库里**没有任何 `business:erp:*` 权限**
 * （2026-11-01 实测 0 条），这些账号拉主数据会 403。所以这里的每个请求都必须 `allSettled`，
 * 失败 = 该表映射为空 → `form-field-labels.ts` 会兜底显示 `(ID: 01a18def…)`（硬红线 3），
 * 页面不许因此报错、也不许把整块审批内容清空。
 *
 * # 缓存
 *
 * 同一页面（详情页 / 审批中心）只需要一份；用模块级 Promise 缓存，多次调用只发一轮请求
 * （详情页轮询 30s 一次，不缓存会反复打主数据接口）。
 */
import {
  fetchMaterialPage,
  fetchSupplierOptions,
  fetchUnitOptions,
  fetchWarehouseTree,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import { pageUsers } from '@/api/system/user-api';
import { FIELD_NAME_SOURCE, type FormNameLookup, type FormNameSource } from './form-field-labels';

/** 一张主数据表的 `id → 名称` 映射 */
export type NameMap = Map<string, string>;

/** 五张表（+ 用户，用于 `create_by` 制单人） */
export interface MasterNameMaps {
  supplier: NameMap;
  warehouse: NameMap;
  material: NameMap;
  unit: NameMap;
  user: NameMap;
}

/** 空映射表（加载失败 / 未加载时用；查不到就一律降级 `(ID: …)`） */
export function emptyNameMaps(): MasterNameMaps {
  return {
    supplier: new Map(),
    warehouse: new Map(),
    material: new Map(),
    unit: new Map(),
    user: new Map(),
  };
}

/** 合法的译名来源名（`_id` 结尾的键去掉后缀能直接对上） */
const NAME_SOURCES: FormNameSource[] = ['supplier', 'warehouse', 'material', 'unit', 'user'];

/**
 * 译名函数（喂给 `form-field-labels.ts`）。
 *
 * `key` 决定查哪张表：先按 {@link FIELD_NAME_SOURCE} 精确匹配（`from_warehouse_id` 这类前缀不规则的），
 * 再用「去掉 `_id` 后缀」兜底（`supplier_id` → `supplier`），都不匹配返回 `undefined`（调用方兜底显示 ID）。
 */
export function nameLookupOf(maps: MasterNameMaps): FormNameLookup {
  return (key: string, id: string): string | undefined => {
    if (!id) {
      return undefined;
    }
    const bare = key.endsWith('_id') ? key.slice(0, -3) : key;
    const mapped: FormNameSource | undefined = FIELD_NAME_SOURCE[key];
    const fallback = (NAME_SOURCES as string[]).includes(bare)
      ? (bare as FormNameSource)
      : undefined;
    const source = mapped ?? fallback;
    if (!source) {
      return undefined;
    }
    return maps[source].get(id);
  };
}

/** 仓库树 → 扁平 `id → 名称`（树是任意深度，递归收） */
function flattenWarehouses(nodes: WarehouseTreeNode[], out: NameMap): NameMap {
  for (const node of nodes) {
    if (node?.id) {
      out.set(node.id, node.warehouse_name);
    }
    if (Array.isArray(node?.children) && node.children.length > 0) {
      flattenWarehouses(node.children, out);
    }
  }
  return out;
}

/** 保单号：一个数据源失败只是少一类名称，不许影响其它 */
async function safely<T>(task: Promise<T>): Promise<T | null> {
  try {
    return await task;
  } catch {
    // 403（审批岗位没 ERP 权限）/ 404（路由未上线）/ 网络抖动都走这里
    return null;
  }
}

/**
 * 拉全量主数据名称映射。
 *
 * 一次 5 个请求并发（供应商 / 仓库 / 物料 / 单位 / 用户），任一失败静默降级。
 * 物料与用户按「一页 500 条」取：审批详情要看的是**单据上那几个**物料，种子库量级足够；
 * 真到上千条时应该让后端把名称写进 `form_data`（后续卡），这里的 500 只是止损。
 */
export async function loadMasterNameMaps(): Promise<MasterNameMaps> {
  const [suppliers, warehouses, materials, units, users] = await Promise.all([
    safely(fetchSupplierOptions(500)),
    safely(fetchWarehouseTree()),
    safely(fetchMaterialPage({ page: 1, page_size: 500 })),
    safely(fetchUnitOptions()),
    safely(pageUsers({ page: 1, page_size: 500 })),
  ]);

  const maps = emptyNameMaps();
  for (const row of suppliers ?? []) {
    if (row?.id) {
      maps.supplier.set(row.id, row.supplier_name);
    }
  }
  flattenWarehouses(warehouses ?? [], maps.warehouse);
  for (const row of materials?.list ?? []) {
    if (row?.id) {
      maps.material.set(row.id, row.material_name);
    }
  }
  for (const row of units ?? []) {
    if (row?.id) {
      maps.unit.set(row.id, row.unit_name);
    }
  }
  for (const row of users?.list ?? []) {
    if (row?.id) {
      maps.user.set(row.id, row.real_name || row.username);
    }
  }
  return maps;
}

/** 模块级缓存：一次页面会话只拉一轮 */
let cached: Promise<MasterNameMaps> | null = null;

/** 取（或首次拉）名称映射 —— 详情页 / 审批中心共用 */
export function ensureMasterNameMaps(): Promise<MasterNameMaps> {
  if (!cached) {
    cached = loadMasterNameMaps();
  }
  return cached;
}

/** 测试 / 切账号时重置缓存 */
export function resetMasterNameMaps(): void {
  cached = null;
}
