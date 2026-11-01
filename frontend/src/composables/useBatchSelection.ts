/**
 * 列表页批量勾选状态（D7）。
 *
 * 每个接入批量的页面都要维护同一套东西：`selected-row-keys`（绑定表格）、
 * 勾选行对象（判状态用）、跨页缓存（「选中全部筛选结果」后其它页的行也要能判状态）。
 * 写八遍必然漂移，所以收在这里；页面只留「动作声明 + 执行器」。
 *
 * # 刷新后不保持勾选（卡面 §三 ⑤）
 *
 * 批量执行完由 `BatchActionBar` 主动 `update:selected: []`，列表刷新也不会把旧 id 带回来 ——
 * 保持勾选的真实风险是「用户以为还要再点一次」，于是第二次点成了重复过账。
 */

import { ref, watch, type Ref } from 'vue';
import {
  isRowSelectable,
  type BatchActionSpec,
  type BatchRow,
} from '@/utils/batch-action';

/** 有主键、可能带状态的行（列表页的行结构都满足）。 */
export interface SelectableRow {
  id: string;
  status?: string | null;
}

/** 批量勾选的返回值。 */
export interface BatchSelection {
  /** 绑给 `t-table` 的 `selected-row-keys`。 */
  selectedIds: Ref<string[]>;
  /** 勾选行对象（状态判定 / 结果回显用）。 */
  selectedRows: Ref<BatchRow[]>;
  /** `t-table` 的 `@select-change`：把 key 列表对齐成行对象。 */
  onSelectChange: (keys: Array<string | number>) => void;
  /** `BatchActionBar` 的 `@update:selected`（清空 / 全选筛选结果）。 */
  onSelectedRows: (rows: BatchRow[]) => void;
  /** 行复选框是否可勾（任何动作都不适用的行禁用）。 */
  isSelectable: (row: SelectableRow) => boolean;
  /** 清空勾选（切筛选 / 换页时调用）。 */
  clearSelection: () => void;
}

/**
 * 创建批量勾选状态。
 *
 * @param actions 本页声明的批量动作（决定哪些行可勾）
 * @param rowsRef 当前页数据（用于把 id 还原成带状态的行）
 */
export function useBatchSelection(
  actions: BatchActionSpec[],
  rowsRef: Ref<SelectableRow[]>
): BatchSelection {
  const selectedIds = ref<string[]>([]);
  const selectedRows = ref<BatchRow[]>([]);
  /** 见过的行（跨页全选后，其它页的行不在 `rowsRef` 里，靠它判状态）。 */
  const cache = new Map<string, BatchRow>();

  watch(
    rowsRef,
    (rows) => {
      for (const row of rows) {
        cache.set(row.id, { id: row.id, status: row.status ?? null });
      }
    },
    { immediate: true, deep: false }
  );

  function toRow(id: string): BatchRow {
    return cache.get(id) ?? { id };
  }

  function onSelectChange(keys: Array<string | number>): void {
    const ids = keys.map(String);
    selectedIds.value = ids;
    selectedRows.value = ids.map(toRow);
  }

  function onSelectedRows(rows: BatchRow[]): void {
    selectedRows.value = rows;
    selectedIds.value = rows.map((row) => row.id);
    for (const row of rows) {
      cache.set(row.id, row);
    }
  }

  function isSelectable(row: SelectableRow): boolean {
    return isRowSelectable(actions, { id: row.id, status: row.status ?? null });
  }

  function clearSelection(): void {
    selectedIds.value = [];
    selectedRows.value = [];
  }

  return { selectedIds, selectedRows, onSelectChange, onSelectedRows, isSelectable, clearSelection };
}
