<template>
  <div class="warehouse-page">
    <!-- 工具条（高频）：主操作 + 仓库搜索 -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <t-space align="center">
          <t-button
            v-privilege="[WAREHOUSE_PERM.warehouseCreate]"
            theme="primary"
            size="small"
            data-testid="btn-new-warehouse"
            @click="openWarehouseCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新建仓库
          </t-button>
          <!-- 非叶子仓库时按钮置灰 + tooltip 说明（库位只能挂叶子仓库，后端 66005 同源） -->
          <t-tooltip :content="locationGate.reason" :disabled="!locationGate.disabled" placement="bottom-left">
            <span class="gate-wrap">
              <t-button
                v-privilege="[WAREHOUSE_PERM.locationCreate]"
                variant="outline"
                size="small"
                :disabled="locationGate.disabled"
                data-testid="btn-new-location"
                @click="openLocationCreate"
              >
                <template #icon><t-icon name="add" /></template>
                新建库位
              </t-button>
            </span>
          </t-tooltip>
          <t-button variant="outline" size="small" :loading="treeLoading" data-testid="btn-refresh" @click="reloadAll">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
          <!-- 批量导入入口：一键下载模板 + 字段说明（不必先去导入向导找） -->
          <ImportEntryButton
            biz-type="business.erp.warehouse"
            biz-label="仓库"
            testid="btn-import-warehouse"
          />
        </t-space>
        <t-input
          v-model="keyword"
          class="toolbar__search"
          placeholder="搜索仓库编码 / 名称"
          clearable
          data-testid="wh-search"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
      </div>
    </t-card>

    <div class="warehouse-body">
      <!-- 左：仓库树（物化路径，逐节点独立折叠） -->
      <t-card :bordered="false" class="tree-card">
        <div class="panel-head">
          <span class="panel-head__title">仓库树</span>
          <span class="panel-head__meta" data-testid="tree-meta">{{ treeMeta }}</span>
        </div>

        <t-alert v-if="treeError" theme="error" :message="treeError" class="tree-alert">
          <template #operation>
            <t-button size="small" variant="outline" @click="loadTree">重试</t-button>
          </template>
        </t-alert>

        <t-loading :loading="treeLoading" size="small" class="tree-loading">
          <t-tree
            v-if="visibleTree.length > 0"
            :data="visibleTree"
            :keys="TREE_KEYS"
            :expanded="expanded"
            :actived="actived"
            :hover="true"
            :line="true"
            :transition="false"
            data-testid="warehouse-tree"
            @expand="handleExpand"
            @click="handleNodeClick"
          >
            <template #label="{ node }">
              <span
                class="wh-node"
                :data-code="node.data.warehouse_code"
                :data-key="nodeKeyOf(node.data)"
                :data-leaf="String(leafOf(node.data))"
              >
                <t-tag size="small" variant="light" :theme="levelTheme(node.data.level)" class="wh-node__level">
                  {{ levelLabel(node.data.level) }}
                </t-tag>
                <span class="wh-node__name">{{ node.data.warehouse_name }}</span>
                <t-tag
                  v-if="node.data.warehouse_type !== 'NORMAL'"
                  size="small"
                  variant="outline"
                  class="wh-node__type"
                >
                  {{ warehouseTypeLabel(node.data.warehouse_type) }}
                </t-tag>
                <span v-if="node.data.status !== 1" class="wh-node__off">已停用</span>
                <span class="wh-node__tail">
                  {{ leafOf(node.data) ? '叶子' : `${childCountOf(node.data)} 个子仓` }}
                </span>
              </span>
            </template>
          </t-tree>

          <div v-else-if="!treeLoading" class="panel-empty" data-testid="tree-empty">
            <t-icon class="panel-empty__icon" name="shop" />
            <p class="panel-empty__title">{{ keyword.trim() ? '没有匹配的仓库' : '还没有仓库' }}</p>
            <p class="panel-empty__desc">
              {{ keyword.trim() ? '换个编码 / 名称关键词试试' : '点工具条上的「新建仓库」建第一个总仓' }}
            </p>
          </div>
        </t-loading>
      </t-card>

      <!-- 右：库位列表（按选中仓库过滤）+ 分页 -->
      <t-card :bordered="false" class="list-card">
        <div class="panel-head">
          <div class="panel-head__main">
            <span class="panel-head__title">库位列表</span>
            <span class="panel-head__sub" data-testid="selected-title">{{ selectedTitle }}</span>
          </div>
          <t-space size="small">
            <t-button
              v-if="selected"
              v-privilege="[WAREHOUSE_PERM.warehouseUpdate]"
              variant="outline"
              size="small"
              data-testid="btn-edit-warehouse"
              @click="openWarehouseEdit"
            >
              <template #icon><t-icon name="edit" /></template>
              编辑仓库
            </t-button>
          </t-space>
        </div>

        <t-alert v-if="locationError" theme="error" :message="locationError" class="list-alert">
          <template #operation>
            <t-button size="small" variant="outline" @click="loadLocations">重试</t-button>
          </template>
        </t-alert>

        <!-- 未选仓库：直接说清为什么没有列表 -->
        <div v-if="!selected" class="panel-empty" data-testid="list-placeholder">
          <t-icon class="panel-empty__icon" name="scan" />
          <p class="panel-empty__title">先在左侧选中一个仓库</p>
          <p class="panel-empty__desc">库位按所属仓库展示；只有叶子仓库才能挂库位</p>
        </div>

        <!-- 选中非叶子仓库：已有的库位照常列（能编辑），只是不许再新建 -->
        <template v-else>
          <t-alert
            v-if="selected && !selected.is_leaf"
            theme="info"
            class="list-alert"
            data-testid="non-leaf-hint"
            :message="locationGate.reason"
          />

          <t-table
            row-key="id"
            :data="pagedLocations"
            :columns="columns"
            :loading="locationLoading"
            :pagination="pagination"
            :hover="true"
            data-testid="location-table"
            @page-change="handlePageChange"
          >
          <template #type="{ row }">
            <t-tag size="small" variant="light">{{ locationTypeLabel(row.location_type) }}</t-tag>
          </template>
          <template #location_name="{ row }">
            {{ row.location_name || '—' }}
          </template>
          <template #status="{ row }">
            <t-tag size="small" :theme="row.status === 1 ? 'success' : 'default'" variant="light">
              {{ row.status === 1 ? '启用' : '停用' }}
            </t-tag>
          </template>
          <template #op="{ row }">
            <t-space size="small">
              <t-link v-privilege="[WAREHOUSE_PERM.locationUpdate]" theme="primary" @click="openLocationEdit(row)">
                编辑
              </t-link>
              <t-link v-privilege="[WAREHOUSE_PERM.locationDelete]" theme="danger" @click="confirmRemoveLocation(row)">
                删除
              </t-link>
            </t-space>
          </template>
          <template #empty>
            <div class="panel-empty" data-testid="locations-empty">
              <t-icon class="panel-empty__icon" name="scan" />
              <p class="panel-empty__title">「{{ selected?.warehouse_name }}」还没有库位</p>
              <p class="panel-empty__desc">点工具条「新建库位」建第一个；编码在同一仓库内唯一</p>
            </div>
          </template>
          </t-table>
        </template>
      </t-card>
    </div>

    <WarehouseFormDialog
      v-model:visible="warehouseDialogVisible"
      :tree="tree"
      :editing="editingWarehouse"
      :default-parent-id="selectedId"
      @saved="handleWarehouseSaved"
    />
    <LocationFormDialog
      v-model:visible="locationDialogVisible"
      :tree="tree"
      :editing="editingLocation"
      :default-warehouse-id="selectedId ?? ''"
      @saved="handleLocationSaved"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol, type TableProps, type TreeOptionData } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import {
  deleteLocation,
  fetchLocationList,
  fetchWarehouseTree,
  type Location,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import WarehouseFormDialog from './components/WarehouseFormDialog.vue';
import LocationFormDialog from './components/LocationFormDialog.vue';
import { WAREHOUSE_PERM } from './permissions';
import {
  allWarehouseIds,
  clampPage,
  filterWarehouseTree,
  findWarehouse,
  flattenWarehouses,
  initialExpandedIds,
  levelLabel,
  levelTheme,
  locationCreateGate,
  locationTypeLabel,
  paginateRows,
  resolveIsLeaf,
  selectWarehouseId,
  toggleExpandedId,
  treeDepth,
  warehouseNodeKey,
  warehousePathText,
  warehouseTypeLabel,
} from './warehouse-tree';

/** 与后端出参字段名一致（t-tree / t-tree-select 共用） */
const TREE_KEYS = { value: 'id', label: 'warehouse_name', children: 'children' } as const;

/** 库位列表每页条数（`location/list` 不服务端分页，这里纯前端切） */
const PAGE_SIZE = 10;

// ==================== 仓库树 ====================

/** 原始树（接口返回） */
const tree = ref<WarehouseTreeNode[]>([]);
/** 树加载中 */
const treeLoading = ref(false);
/** 树错误 */
const treeError = ref('');
/** 逐节点独立展开的 id 数组（t-tree 的 expanded） */
const expanded = ref<string[]>([]);
/** 搜索关键词 */
const keyword = ref('');
/** 当前选中仓库 id */
const selectedId = ref<string | null>(null);

/** 搜索过滤后的树（关键词为空时就是原树） */
const filterResult = computed(() => filterWarehouseTree(tree.value, keyword.value));
/** 渲染用的树 */
const visibleTree = computed(() => filterResult.value.tree);
/** 树当前选中高亮 */
const actived = computed(() => (selectedId.value ? [selectedId.value] : []));

/** 树规模（面板副标题，也让 e2e 能断言「至少 3 层」） */
const treeMeta = computed(() => {
  const total = allWarehouseIds(visibleTree.value).length;
  const depth = treeDepth(visibleTree.value);
  if (keyword.value.trim()) {
    return `${filterResult.value.matchedCount} 个匹配 / 共 ${allWarehouseIds(tree.value).length} 个 · ${treeDepth(tree.value)} 层`;
  }
  return `${total} 个仓库 · ${depth} 层`;
});

/** 当前选中的仓库节点 */
const selected = computed<WarehouseTreeNode | null>(() =>
  selectedId.value ? findWarehouse(tree.value, selectedId.value) : null,
);

/** 「新建库位」按钮的门禁（非叶子禁用 + 原因文案） */
const locationGate = computed(() => locationCreateGate(selected.value));

/** 右面板标题：当前选中：路径（编码） */
const selectedTitle = computed(() => {
  if (!selected.value) {
    return '未选中仓库';
  }
  return `${warehousePathText(tree.value, selected.value.id)}（${selected.value.warehouse_code}）`;
});

/** 子仓库个数（树节点尾巴文案） */
function childCountOf(node: WarehouseTreeNode): number {
  return node.children?.length ?? 0;
}

/** 是否叶子（`children` 优先，见 `warehouse-tree.ts:resolveIsLeaf`） */
function leafOf(node: TreeOptionData): boolean {
  return resolveIsLeaf(node as unknown as WarehouseTreeNode);
}

/** 节点 key（兜底空 id / 重复 code，避免 t-tree 把两个节点渲染成一个） */
function nodeKeyOf(node: TreeOptionData): string {
  return warehouseNodeKey(node as unknown as WarehouseTreeNode);
}

/** 加载整棵树 */
async function loadTree(): Promise<void> {
  treeLoading.value = true;
  treeError.value = '';
  try {
    const data = await fetchWarehouseTree();
    tree.value = data;
    // 首次进入：展开第 1 层（有孩子的），并默认选中第一个仓库
    expanded.value = initialExpandedIds(data);
    if (!selectedId.value || !findWarehouse(data, selectedId.value)) {
      const first = flattenWarehouses(data)[0]?.node ?? null;
      selectedId.value = first ? first.id : null;
    }
  } catch (error) {
    treeError.value = error instanceof ApiError ? error.message : '仓库树加载失败';
  } finally {
    treeLoading.value = false;
  }
}

/** 逐节点独立折叠 / 展开（只动自己，不联级） */
function handleExpand(value: Array<string | number>): void {
  expanded.value = value.map(String);
}

/** 点节点 → 右侧按该仓库过滤（`watch(selectedId)` 负责真正拉数据） */
function handleNodeClick(context: { node: { data: TreeOptionData } }): void {
  const node = context.node.data as unknown as WarehouseTreeNode;
  selectedId.value = selectWarehouseId(node);
}

/** 搜索时自动展开命中的祖先，否则命中的深层节点看不见 */
watch(keyword, () => {
  if (keyword.value.trim()) {
    expanded.value = filterResult.value.expandedIds;
  } else {
    expanded.value = initialExpandedIds(tree.value);
  }
});

/** 折叠开关按钮（工具栏没做，但保留给需要「全展开」的场景 e2e 点） */
function toggleNode(id: string): void {
  expanded.value = toggleExpandedId(expanded.value, id);
}

// ==================== 库位列表 ====================

/** 当前仓库的全部库位（前端分页前） */
const allLocations = ref<Location[]>([]);
/** 库位加载中 */
const locationLoading = ref(false);
/** 库位错误 */
const locationError = ref('');
/** 库位当前页 */
const locationPage = ref(1);

/** 当前页切片 */
const pagedLocations = computed(() =>
  paginateRows(allLocations.value, locationPage.value, PAGE_SIZE),
);

/** 分页器（受控：页码与切片同源，避免两处各算一遍） */
const pagination = computed<TableProps['pagination']>(() => ({
  current: clampPage(locationPage.value, allLocations.value.length, PAGE_SIZE),
  pageSize: PAGE_SIZE,
  total: allLocations.value.length,
  showJumper: allLocations.value.length > PAGE_SIZE,
  showPageSize: false,
}));

/** 表格列（`cell` 走模板 slot，避免 `PrimaryTableCol<Location>` 与表格泛型 `TableRowData` 打架） */
const columns: PrimaryTableCol[] = [
  { colKey: 'location_code', title: '编码', width: 160, ellipsis: true },
  { colKey: 'location_name', title: '名称', ellipsis: true },
  { colKey: 'type', title: '类型', width: 120 },
  { colKey: 'status', title: '是否启用', width: 120 },
  { colKey: 'op', title: '操作', width: 140, align: 'right' },
];

/** 按选中仓库加载库位（非叶子仓库里**已存在**的库位照常显示，只是不许新建） */
async function loadLocations(): Promise<void> {
  const current = selected.value;
  allLocations.value = [];
  locationError.value = '';
  if (!current) {
    return;
  }
  locationLoading.value = true;
  try {
    // include_disabled = true：列表要能看见停用的库位（状态列才有意义）
    allLocations.value = await fetchLocationList(current.id, true);
  } catch (error) {
    locationError.value = error instanceof ApiError ? error.message : '库位列表加载失败';
  } finally {
    locationLoading.value = false;
  }
}

/** 翻页（受控分页：自己改页码，不依赖组件内部状态） */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  locationPage.value = clampPage(pageInfo.current, allLocations.value.length, PAGE_SIZE);
}

// ==================== 弹窗 ====================

/** 仓库弹窗 */
const warehouseDialogVisible = ref(false);
const editingWarehouse = ref<WarehouseTreeNode | null>(null);
/** 库位弹窗 */
const locationDialogVisible = ref(false);
const editingLocation = ref<Location | null>(null);

/** 新建仓库（预选当前节点为父仓库） */
function openWarehouseCreate(): void {
  editingWarehouse.value = null;
  warehouseDialogVisible.value = true;
}

/** 编辑当前选中的仓库 */
function openWarehouseEdit(): void {
  if (!selected.value) {
    return;
  }
  editingWarehouse.value = selected.value;
  warehouseDialogVisible.value = true;
}

/** 新建库位（默认挂在当前选中的叶子仓库下） */
function openLocationCreate(): void {
  if (locationGate.value.disabled) {
    MessagePlugin.warning(locationGate.value.reason);
    return;
  }
  editingLocation.value = null;
  locationDialogVisible.value = true;
}

/** 编辑库位 */
function openLocationEdit(row: Location): void {
  editingLocation.value = row;
  locationDialogVisible.value = true;
}

/** 仓库保存成功：重载树 → 定位/展开到它 */
async function handleWarehouseSaved(payload: { id: string | null; isEdit: boolean; name: string }): Promise<void> {
  const createdId = payload.isEdit ? null : payload.id;
  await loadTree();
  if (createdId) {
    selectedId.value = createdId;
    // 新节点的父级展开（否则它在折叠的父节点里看不见）
    const parentId = findParentId(tree.value, createdId);
    if (parentId) {
      expanded.value = Array.from(new Set([...expanded.value, parentId]));
    }
  }
}

/** 库位保存成功：刷新当前仓库的库位 */
async function handleLocationSaved(): Promise<void> {
  await loadLocations();
  // 新建/改完后页码可能越界，夹一下
  locationPage.value = clampPage(locationPage.value, allLocations.value.length, PAGE_SIZE);
}

/** 找某节点的直接上级 id */
function findParentId(nodes: WarehouseTreeNode[], id: string): string | null {
  for (const node of nodes) {
    if ((node.children ?? []).some((child) => child.id === id)) {
      return node.id;
    }
    const hit = findParentId(node.children ?? [], id);
    if (hit) {
      return hit;
    }
  }
  return null;
}

/** 删除库位（软删，二次确认） */
function confirmRemoveLocation(row: Location): void {
  const dialog = DialogPlugin.confirm({
    header: '删除库位',
    body: `确认删除库位「${row.location_code}${row.location_name ? ` / ${row.location_name}` : ''}」？删除后不可用于新单据，历史库存不动。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteLocation(row.id);
        MessagePlugin.success('库位已删除');
        dialog.hide();
        await handleLocationSaved();
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

/** 工具条「刷新」：树 + 当前仓库的库位一起刷 */
async function reloadAll(): Promise<void> {
  await loadTree();
  await loadLocations();
}

onMounted(async () => {
  await loadTree();
  await loadLocations();
});

// 选中变化时（含首屏）加载库位；`handleNodeClick` 里已处理同值不重复请求
watch(selectedId, (next, prev) => {
  if (next !== prev) {
    locationPage.value = 1;
    void loadLocations();
  }
});

// 供 e2e / 未来「全展开」按钮复用
defineExpose({ toggleNode });
</script>

<style scoped lang="less">
.warehouse-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.toolbar-card :deep(.t-card__body) {
  padding: 12px 16px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.toolbar__search {
  width: 260px;
}

/* 禁用的按钮外面包一层 span，否则 t-tooltip 在 disabled 元素上不触发 */
.gate-wrap {
  display: inline-flex;
}

.warehouse-body {
  display: flex;
  gap: 12px;
  align-items: stretch;
  min-height: 60vh;
}

.tree-card {
  width: 340px;
  flex: 0 0 340px;
}

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
  min-width: 0;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
  white-space: nowrap;
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}

.panel-head__sub {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tree-alert,
.list-alert {
  margin-bottom: 8px;
}

.tree-loading {
  display: block;
  min-height: 240px;
  /* 仓库多时树自己要能滚（面板定高，横向不滚） */
  max-height: calc(100vh - 260px);
  overflow: auto;
  overflow-x: hidden;
}

/* 树节点：层级标签 + 名称 + 类型 + 尾巴（叶子 / 子仓数） */
.wh-node {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  max-width: 100%;
}

.wh-node__level {
  flex: 0 0 auto;
}

.wh-node__name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.wh-node__type {
  flex: 0 0 auto;
}

.wh-node__off {
  flex: 0 0 auto;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  text-decoration: line-through;
}

.wh-node__tail {
  flex: 0 0 auto;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 48px 16px;
  text-align: center;
}

.panel-empty__icon {
  font-size: 32px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  max-width: 420px;
  line-height: 1.6;
}
</style>
