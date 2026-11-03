/**
 * stock-pick.ts —— 调拨单「从库存选择」的纯逻辑（ARCH-3 §6 R-P1-5）
 *
 * 抽成纯函数是为了**能单测**：`qty − locked_qty` 的可用量口径、成本价按「物料 × 仓库」
 * 取整仓汇总行的 `avg_cost`、零可用量行的排序 —— 这些都不该埋在 `.vue` 的 `script setup` 里
 * （SFC 里既不好测、也不能 `export` 类型）。
 *
 * # 数据来源（三个只读接口）
 *
 * | 行数据 | 接口 | 用到什么 |
 * |---|---|---|
 * | 库位库存 | `GET /erp/inventory-location/page?warehouse_id=` | `id`（= 明细的 `from_location_id`）/ `location_id` / `qty` / `locked_qty` |
 * | 即时库存 | `GET /erp/inventory/page?warehouse_id=&only_nonzero=true` | `avg_cost`（移动加权成本） |
 * | 物料主数据 | 父页面已加载 | 名称 / 编码 / 是否一物一码 |
 *
 * # 两个同名 id 反着指（W1c 记下的坑）
 *
 * * `InventoryLocationRow.id` → **库位库存行**（`biz_inventory_location.id`）——明细要这个；
 * * `InventoryLocationRow.location_id` → **库位主数据**（`biz_location.id`）——译名用这个。
 */

import type { InventoryLocationRow, InventoryRow, MaterialRow } from '@/api/business/erp';

/** 弹窗里的一行可选库存 */
export interface PickerRow {
  /** 稳定 key（= 库位库存行 id） */
  key: string;
  /** 库位库存行 id —— 明细的 `from_location_id` 就取它 */
  inventory_location_id: string;
  /** 物料 id */
  material_id: string;
  /** 库位主数据 id（`null` = 该仓未分配库位） */
  location_id: string | null;
  /** 可用量 = `qty − locked_qty`（可为负：锁定量超过账面时如实显示，不悄悄抹成 0） */
  available_qty: number;
  /** 移动加权成本（取自即时库存同物料汇总行；没有成本价时是 0） */
  unit_cost: number;
  /** 是否真有成本价（`false` → 界面显示「—（无成本价）」） */
  has_cost: boolean;
  /** 物料是否一物一码（带入后必须选序列号） */
  serial_required: boolean;
  /** 本次调拨数量（默认 = 可用量，可改小） */
  pick_qty: number;
}

/** 父页面拿到的选择结果（并入表单明细用） */
export interface StockPick {
  /** 物料 id */
  material_id: string;
  /** 库位库存行 id（`null` = 该仓未分配库位） */
  from_location_id: string | null;
  /** 本次调拨数量 */
  qty: number;
  /** 成本价（只用于页面计算金额展示，不随明细提交 —— 后端自己算快照） */
  unit_cost: number;
  /** 一物一码 */
  serial_required: boolean;
}

/** 成本价取「同物料在**本仓**的汇总行」：`avg_cost` 与库位无关（同仓同物料一个成本）。 */
function buildCostMap(stockRows: InventoryRow[]): Map<string, { cost: number; has: boolean }> {
  const costOf = new Map<string, { cost: number; has: boolean }>();
  for (const row of stockRows) {
    const raw = Number(row.avg_cost ?? 0);
    const cost = Number.isFinite(raw) ? raw : 0;
    const current = costOf.get(row.material_id);
    // 同一物料可能有「整仓汇总行 + 各库位行」，取第一个有成本的（零成本的汇总行不算数）
    if (!current || (!current.has && cost > 0)) {
      costOf.set(row.material_id, { cost, has: cost > 0 });
    }
  }
  return costOf;
}

/**
 * 把三个接口的结果拼成弹窗行（**纯函数**：无 IO、无 Vue 依赖，单测直接喂数组）。
 *
 * 顺序：可用量降序（有货的在最上面），同可用量按物料编码稳定排序（弹窗不会每次抖动）。
 */
export function buildPickerRows(
  locationRows: InventoryLocationRow[],
  stockRows: InventoryRow[],
  materials: MaterialRow[],
): PickerRow[] {
  const costOf = buildCostMap(stockRows);
  const materialOf = new Map(materials.map((item) => [item.id, item]));

  return locationRows
    .map((row) => {
      const available = Number(row.qty ?? 0) - Number(row.locked_qty ?? 0);
      const cost = costOf.get(row.material_id) ?? { cost: 0, has: false };
      return {
        key: row.id,
        inventory_location_id: row.id,
        material_id: row.material_id,
        location_id: row.location_id,
        available_qty: Number.isFinite(available) ? available : 0,
        unit_cost: cost.cost,
        has_cost: cost.has,
        serial_required: materialOf.get(row.material_id)?.serial_required ?? false,
        pick_qty: available > 0 ? available : 0,
      } satisfies PickerRow;
    })
    .sort((left, right) => {
      if (right.available_qty !== left.available_qty) {
        return right.available_qty - left.available_qty;
      }
      const leftCode = materialOf.get(left.material_id)?.material_code ?? '';
      const rightCode = materialOf.get(right.material_id)?.material_code ?? '';
      return leftCode.localeCompare(rightCode);
    });
}

/**
 * 弹窗行 → 带进明细的选择结果。
 *
 * 只取勾选了、且「本次调拨」> 0 的行；数量上限在界面上已经夹住（≤ 可用量），
 * 这里再兜一次（手输/脚本改值的路径不该把 0 或负数带进明细）。
 */
export function toStockPicks(rows: PickerRow[], selectedKeys: string[]): StockPick[] {
  const selected = new Set(selectedKeys);
  return rows
    .filter((row) => selected.has(row.key) && Number(row.pick_qty) > 0)
    .map((row) => ({
      material_id: row.material_id,
      from_location_id: row.inventory_location_id,
      qty: Math.min(Number(row.pick_qty), Math.max(row.available_qty, 0)),
      unit_cost: row.unit_cost,
      serial_required: row.serial_required,
    }));
}
