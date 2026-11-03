<!--
  StockPickerDialog.vue —— 调拨单「从库存选择物料」（ARCH-3 §6 R-P1-5）

  # 用户原话

  「操作便捷」——建调拨单时**不该**让作业员另外开一页库存列表、把物料号 / 库位 / 数量抄过来。

  # 弹窗里有什么

  | 列 | 来源 |
  |---|---|
  | 物料 | `biz_material`（父页面已加载的主数据，译名 + 是否一物一码） |
  | 仓库 | 弹窗入参（调出仓） |
  | 库位 | `GET /erp/location/list?warehouse_id=…`（`biz_location` 主数据） |
  | 可用量 | `GET /erp/inventory-location/page?warehouse_id=…` 的 `qty − locked_qty` |
  | 成本价 | `GET /erp/inventory/page?warehouse_id=…&only_nonzero=true` 的 `avg_cost`（移动加权） |
  | 本次调拨 | 默认 = 可用量，可改（上限 = 可用量，前端先拦一次，后端过账时还会二次校验） |

  勾选 → [带入明细] → 父页面把选中的行并进明细（同物料同库位合并数量）。

  # 关键口径（踩过的坑）

  * 明细的 `from_location_id` 指的是 **`biz_inventory_location.id`（库位库存行）**，
    不是 `biz_location.id`（库位主数据）。`InventoryLocationRow` 两个 id 都在：
    `row.id` = 库存行（**用它**），`row.location_id` = 库位主数据（译名用）。
  * 即时库存（`biz_inventory`）的唯一键含 `COALESCE(location_id, 全零)`，
    同一「物料 × 仓库」可能有多行（整仓汇总行 + 各库位行）。本弹窗**只列库位行**
    （`inventory-location`），成本取整仓汇总行的 `avg_cost` —— 同仓同物料成本只有一个值。
  * 没有成本价就没有金额：`avg_cost` 缺失时显示「—」并把成本按 0 带出，
    作业员能看见（不会悄悄按 0 记账还以为有成本）。
-->
<template>
  <t-dialog
    :visible="visible"
    :header="headerText"
    width="900px"
    placement="center"
    :close-on-overlay-click="false"
    :confirm-btn="null"
    :cancel-btn="null"
    data-testid="stock-picker-dialog"
    @close="close"
  >
    <t-alert
      v-if="!warehouseId"
      theme="warning"
      message="先选「调出仓」：库存是按仓库算的，没仓库就没有可选的库存"
      data-testid="stock-picker-no-warehouse"
    />

    <template v-else>
      <div class="sp__toolbar">
        <t-input
          v-model="keyword"
          class="sp__search"
          placeholder="按物料编码 / 名称过滤"
          clearable
          data-testid="stock-picker-search"
        />
        <t-checkbox v-model="onlyAvailable" data-testid="stock-picker-only-available">
          只看有可用量
        </t-checkbox>
        <span class="sp__meta" data-testid="stock-picker-meta">
          可选 {{ visibleRows.length }} 条 · 已选 {{ selected.length }} 条
        </span>
      </div>

      <div v-if="loading" class="sp__loading"><t-loading size="small" /> 正在读库存…</div>
      <t-alert v-else-if="error" theme="error" :message="error" data-testid="stock-picker-error" />

      <template v-else>
        <t-table
          v-if="visibleRows.length > 0"
          row-key="key"
          size="small"
          max-height="360px"
          :data="visibleRows"
          :columns="columns"
          :selected-row-keys="selectedKeys"
          data-testid="stock-picker-table"
          @select-change="onSelectChange"
        >
          <template #material="{ row }">
            <span class="sp__material">{{ materialLabel(row.material_id) }}</span>
            <t-tag v-if="row.serial_required" size="small" variant="outline" theme="warning">一物一码</t-tag>
          </template>
          <template #location="{ row }">{{ locationLabel(row.location_id) }}</template>
          <template #qty="{ row }">
            <span :class="['mono', row.available_qty <= 0 ? 'sp__zero' : '']">{{ formatQty(row.available_qty) }}</span>
          </template>
          <template #cost="{ row }">
            <span class="mono">{{ row.has_cost ? formatMoney(row.unit_cost) : '—（无成本价）' }}</span>
          </template>
          <template #pick="{ row }">
            <t-input-number
              v-model="row.pick_qty"
              :min="0"
              :max="row.available_qty > 0 ? row.available_qty : 0"
              :decimal-places="row.serial_required ? 0 : undefined"
              theme="normal"
              size="small"
              :disabled="row.available_qty <= 0"
              :data-testid="`stock-pick-qty-${row.key}`"
            />
          </template>
        </t-table>

        <div v-else class="sp__empty" data-testid="stock-picker-empty">
          <template v-if="rows.length > 0">
            没有匹配的库存行（换个关键词，或取消「只看有可用量」）
          </template>
          <template v-else>
            这个仓库没有可选的库存（换个仓库，或先做一张收货入库把货收进来）
          </template>
        </div>

        <p class="sp__hint">
          勾选后「本次调拨」默认等于可用量，可以改小；一物一码物料带入明细后还要逐个选序列号。
        </p>
      </template>
    </template>

    <template #footer>
      <div class="sp__footer">
        <t-button variant="outline" :disabled="submitting" data-testid="stock-picker-dismiss" @click="close">
          取消
        </t-button>
        <t-button
          theme="primary"
          :disabled="!warehouseId || submitting"
          data-testid="stock-picker-confirm"
          @click="confirm"
        >
          带入明细（{{ selected.length }} 条）
        </t-button>
      </div>
    </template>
  </t-dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  fetchInventoryLocationPage,
  fetchInventoryPage,
  fetchLocationList,
  type Location,
  type MaterialRow,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { buildPickerRows, toStockPicks, type PickerRow, type StockPick } from './stock-pick';

/** 一次最多读多少条库存行（一个仓库不会有几百种物料，够用且不至于卡） */
const PAGE_SIZE = 500;

const props = withDefaults(
  defineProps<{
    /** 弹窗可见（v-model:visible） */
    visible: boolean;
    /** 调出仓 id（空 = 还没选仓库，弹窗给提示） */
    warehouseId: string;
    /** 调出仓名称（标题用） */
    warehouseName?: string;
    /** 物料主数据（译名 + 一物一码标记） */
    materials?: MaterialRow[];
  }>(),
  { warehouseName: '', materials: () => [] },
);

const emit = defineEmits<{
  (event: 'update:visible', value: boolean): void;
  (event: 'confirm', picks: StockPick[]): void;
}>();

const loading = ref(false);
const submitting = ref(false);
const error = ref('');
const keyword = ref('');
const onlyAvailable = ref(true);
const rows = ref<PickerRow[]>([]);
const selectedKeys = ref<string[]>([]);
const locations = ref<Location[]>([]);

const headerText = computed(() =>
  props.warehouseName ? `从库存选择 · ${props.warehouseName}` : '从库存选择',
);

const columns = [
  { colKey: 'row-select', type: 'multiple' as const, width: 48 },
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'location', title: '库位', width: 150 },
  { colKey: 'qty', title: '可用量', width: 110, align: 'right' as const },
  { colKey: 'cost', title: '成本价', width: 130, align: 'right' as const },
  { colKey: 'pick', title: '本次调拨', width: 140 },
];

/** 被过滤后的可见行（勾选状态跨过滤保留） */
const visibleRows = computed(() => {
  const word = keyword.value.trim().toLowerCase();
  return rows.value.filter((row) => {
    if (onlyAvailable.value && row.available_qty <= 0) return false;
    if (!word) return true;
    return materialLabel(row.material_id).toLowerCase().includes(word);
  });
});

const selected = computed(() => rows.value.filter((row) => selectedKeys.value.includes(row.key)));

function materialOf(id: string): MaterialRow | undefined {
  return props.materials.find((item) => item.id === id);
}

function materialLabel(id: string): string {
  const hit = materialOf(id);
  if (!hit) return id.slice(0, 8);
  return `${hit.material_name}（${hit.material_code}）`;
}

function locationLabel(id: string | null): string {
  if (!id) return '—（未分配库位）';
  const hit = locations.value.find((item) => item.id === id);
  if (!hit) return id.slice(0, 8);
  return `${hit.location_name ?? hit.location_code}（${hit.location_code}）`;
}

function formatQty(raw: number | string | null | undefined): string {
  const value = Number(raw ?? 0);
  if (!Number.isFinite(value)) return String(raw ?? '0');
  return value.toFixed(4).replace(/\.?0+$/, '') || '0';
}

function formatMoney(raw: number): string {
  if (!Number.isFinite(raw)) return '—';
  return `¥${raw.toLocaleString('zh-CN', { minimumFractionDigits: 4, maximumFractionDigits: 4 })}`;
}

/** 勾选变化：TDesign 的 select-change 出参是 (keys, { selectedRowData }) */
function onSelectChange(keys: Array<string | number>): void {
  selectedKeys.value = keys.map(String);
}

function close(): void {
  emit('update:visible', false);
}

function confirm(): void {
  const picks: StockPick[] = toStockPicks(rows.value, selectedKeys.value);
  if (picks.length === 0) {
    void MessagePlugin.warning('还没选数量：勾选一行并填「本次调拨」大于 0');
    return;
  }
  submitting.value = true;
  emit('confirm', picks);
  submitting.value = false;
  close();
}

async function load(): Promise<void> {
  selectedKeys.value = [];
  rows.value = [];
  locations.value = [];
  error.value = '';
  if (!props.warehouseId) return;
  loading.value = true;
  try {
    // 三个只读接口并行：库位库存（数量 + 库位）/ 即时库存（成本价）/ 库位主数据（译名）
    const [locationPage, stockPage, locationList] = await Promise.all([
      fetchInventoryLocationPage({ warehouse_id: props.warehouseId, page: 1, page_size: PAGE_SIZE }),
      fetchInventoryPage({ warehouse_id: props.warehouseId, only_nonzero: true, page: 1, page_size: PAGE_SIZE }),
      fetchLocationList(props.warehouseId, true).catch(() => [] as Location[]),
    ]);
    locations.value = locationList ?? [];
    // 拼行 + 成本价 + 排序全在纯函数里（`stock-pick.ts`，单测钉住）
    rows.value = buildPickerRows(locationPage.list ?? [], stockPage.list ?? [], props.materials);
  } catch (err) {
    error.value = err instanceof ApiError ? err.message : `库存读取失败：${(err as Error).message}`;
  } finally {
    loading.value = false;
  }
}

watch(
  () => [props.visible, props.warehouseId] as const,
  ([visible]) => {
    if (visible) {
      keyword.value = '';
      onlyAvailable.value = true;
      void load();
    }
  },
  { immediate: true },
);
</script>

<style scoped>
.sp__toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  margin-bottom: 10px;
}

.sp__search {
  width: 240px;
}

.sp__meta {
  margin-left: auto;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.sp__loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 32px 0;
}

.sp__empty {
  padding: 32px 0;
  text-align: center;
  color: var(--td-text-color-placeholder, #0000004d);
}

.sp__material {
  margin-right: 6px;
}

.sp__zero {
  color: var(--td-text-color-placeholder, #0000004d);
}

.sp__hint {
  margin: 10px 0 0;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.sp__footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
