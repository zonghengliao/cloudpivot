<template>
  <div class="location-page">
    <!-- ①② 筛选 / 分页 / 行操作 / 导出交给 ListPage（P3-07） -->
    <ListPage
      ref="listRef"
      title="库位列表"
      testid="loc"
      table-testid="loc-table"
      meta-testid="loc-meta"
      btn-prefix="btn-loc"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="LOCATION_PAGE_SIZE"
      exportable
      @loaded="onLoaded"
      @refresh="handleRefresh"
    >
      <!-- 口径后缀写进「共 N 条」同一个元素（e2e 按 loc-meta 断言「全部仓库 / 当前仓库」） -->
      <template #meta-extra>
        {{ filter.warehouse_id ? '（当前仓库）' : '（全部仓库）' }}
      </template>

      <template #head-extra>
        <span class="panel-head__hint">
          单仓内的新建 / 删除也可在
          <router-link class="panel-head__link" to="/business/erp/warehouse">仓库管理</router-link>
          页操作
        </span>
      </template>

      <template #toolbar>
        <ImportEntryButton biz-type="business.erp.location" biz-label="库位" testid="btn-import-location" />

        <t-button
          v-privilege="[PERM.locationCreate]"
          theme="primary"
          data-testid="btn-loc-new"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建库位
        </t-button>
      </template>

      <template #location_code="{ row }">
        <span class="mono" :data-testid="`loc-code-${row.location_code}`">{{ row.location_code }}</span>
      </template>

      <template #location_name="{ row }">
        {{ dash(row.location_name) }}
      </template>

      <template #warehouse_id="{ row }">
        <!-- LocationRes 只有 warehouse_id，没有名称：这里用 warehouse/tree 翻译 -->
        <span :data-testid="`loc-warehouse-${row.location_code}`">{{ warehouseNameOf(tree, row.warehouse_id) }}</span>
      </template>

      <template #location_type="{ row }">
        <t-tag size="small" :theme="locationTypeTheme(row.location_type)" variant="light">
          {{ locationTypeLabel(row.location_type) }}
        </t-tag>
      </template>

      <template #is_default="{ row }">
        <t-tag v-if="row.is_default" size="small" theme="warning" variant="light">默认收货位</t-tag>
        <span v-else class="muted">—</span>
      </template>

      <template #status="{ row }">
        <t-tag
          size="small"
          :theme="locationStatusTheme(row.status)"
          variant="light"
          :data-testid="`loc-status-${row.location_code}`"
        >
          {{ locationStatusLabel(row.status) }}
        </t-tag>
      </template>

      <template #op="{ row }">
        <div class="op-cell">
          <t-link theme="primary" data-testid="btn-loc-view" @click="openDetail(row)">查看</t-link>
          <t-link
            v-privilege="[PERM.locationUpdate]"
            theme="primary"
            data-testid="btn-loc-edit"
            @click="openEdit(row)"
          >
            编辑
          </t-link>
        </div>
      </template>

      <template #empty>
        <div class="panel-empty" data-testid="loc-empty">
          <t-icon class="panel-empty__icon" name="location" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的库位' : '还没有库位' }}</p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个仓库 / 类型 / 关键词，或点「重置」看全部'
                : '库位挂在叶子仓库下：点右上角「新建库位」建第一个'
            }}
          </p>
        </div>
      </template>
    </ListPage>

    <!-- ③ 详情抽屉（只读；编辑入口在行操作里） -->
    <t-drawer
      v-model:visible="detailVisible"
      header="库位详情"
      size="440px"
      :footer="false"
      data-testid="loc-detail-drawer"
    >
      <t-alert v-if="detailError" theme="error" :message="detailError" class="dlg-alert" />
      <t-loading :loading="detailLoading" size="small">
        <div v-if="detail" class="detail" data-testid="loc-detail-body">
          <div class="detail__head">
            <span class="detail__code mono">{{ detail.location_code }}</span>
            <t-tag size="small" :theme="locationStatusTheme(detail.status)" variant="light">
              {{ locationStatusLabel(detail.status) }}
            </t-tag>
            <t-tag v-if="detail.is_default" size="small" theme="warning" variant="light">默认收货位</t-tag>
          </div>

          <dl class="detail__grid">
            <dt>库位名称</dt>
            <dd>{{ dash(detail.location_name) }}</dd>
            <dt>所属仓库</dt>
            <dd>{{ warehouseNameOf(tree, detail.warehouse_id) }}</dd>
            <dt>库位类型</dt>
            <dd>{{ locationTypeLabel(detail.location_type) }}</dd>
            <dt>容量上限</dt>
            <dd>{{ capacityText(detail.capacity) }}</dd>
            <dt>排序值</dt>
            <dd>{{ detail.sort_order }}</dd>
            <dt>备注</dt>
            <dd>{{ dash(detail.remark) }}</dd>
            <dt>创建时间</dt>
            <dd>{{ formatTime(detail.created_at) }}</dd>
            <dt>更新时间</dt>
            <dd>{{ formatTime(detail.updated_at) }}</dd>
            <dt>库位 id</dt>
            <dd class="mono detail__id">{{ detail.id }}</dd>
          </dl>
        </div>
      </t-loading>
    </t-drawer>

    <!-- ④ 新建 / 编辑：**复用**仓库页的库位弹窗（同一套字段校验 + 叶子仓库闸门，不重复实现） -->
    <LocationFormDialog
      v-model:visible="formVisible"
      :tree="tree"
      :editing="editing"
      :default-warehouse-id="filter.warehouse_id"
      @saved="handleSaved"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import {
  fetchLocationDetail,
  fetchLocationPage,
  fetchWarehouseTree,
  type Location,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import LocationFormDialog from '../warehouse/components/LocationFormDialog.vue';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  LOCATION_PAGE_SIZE,
  LOCATION_STATUS_OPTIONS,
  LOCATION_TYPE_OPTIONS,
  buildLocationQuery,
  capacityText,
  dash,
  emptyLocationFilter,
  formatTime,
  isLocationFiltered,
  locationStatusLabel,
  locationStatusTheme,
  locationTypeLabel,
  locationTypeTheme,
  warehouseFilterOptions,
  warehouseNameOf,
  type LocationFilterState,
} from './location-logic';

/**
 * 本页权限码（与 `backend/app/migrations/business/V0141__erp_permissions.up.sql` 逐字一致）。
 *
 * 就近放在页面里（不改 `src/constants/permissions.ts`）：那个文件是全站单点，
 * ERP 这一批多张卡会同时往它里面加码，谁后提交谁撞冲突。
 */
const PERM = {
  /** 列表（页面入口，与 `sys_menu.auth_code` 一致） */
  locationList: 'business:erp:location:list',
  /** 新建库位 */
  locationCreate: 'business:erp:location:create',
  /** 编辑库位 */
  locationUpdate: 'business:erp:location:update',
} as const;

/** 表格列（`colKey` 与 `LocationRes` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  { colKey: 'location_code', title: '库位编码', width: 180, fixed: 'left' },
  { colKey: 'location_name', title: '库位名称', minWidth: 180, ellipsis: true },
  { colKey: 'warehouse_id', title: '所属仓库', minWidth: 220, ellipsis: true },
  { colKey: 'location_type', title: '库位类型', width: 120 },
  { colKey: 'is_default', title: '默认位', width: 120 },
  { colKey: 'status', title: '是否启用', width: 110 },
  { colKey: 'op', title: '操作', width: 130, fixed: 'right' },
];

/** 类型下拉（`''` = 全部） */
const typeFilterOptions = LOCATION_TYPE_OPTIONS.map((item) => ({ ...item }));
/** 状态下拉（`'0'` 停用是有效选项，别当真值丢掉） */
const statusFilterOptions = LOCATION_STATUS_OPTIONS.map((item) => ({ ...item }));

// ==================== 筛选 / 列表（交给 ListPage） ====================

/** 筛选行（`filterState` 就地读写；`buildLocationQuery` / `isLocationFiltered` 引用的是同一个对象） */
const filter = reactive<LocationFilterState>(emptyLocationFilter());
/** 当前页（保存后回页用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** ListPage 实例（新建 / 编辑保存后重拉列表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);
/** 仓库树（仓库下拉 + `warehouse_id → 名称` 翻译） */
const tree = ref<WarehouseTreeNode[]>([]);

/** 是否处于筛选态（空态文案分「没数据」「没匹配」） */
const filtered = computed(() => isLocationFiltered(filter));

/** 仓库筛选下拉（全部仓库：非叶子也要能筛，否则历史数据永远筛不出来） */
const warehouseOptions = computed(() => warehouseFilterOptions(tree.value));

/**
 * 筛选项声明。
 *
 * 四个控件改造前**都**是「值一变就查」（`@change="handleSearch"`），关键词走回车 ——
 * 手感与改造前一致。`computed` 是因为仓库下拉依赖 `tree`（异步拉回来的）。
 */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '全部仓库（跨仓总览）',
    width: 280,
    options: warehouseOptions.value,
    filterable: true,
    searchOnChange: true,
    testid: 'loc-filter-warehouse',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '库位编码 / 名称（模糊）',
    width: 220,
    testid: 'loc-filter-keyword',
  },
  {
    key: 'location_type',
    type: 'select',
    placeholder: '全部类型',
    width: 150,
    options: typeFilterOptions,
    searchOnChange: true,
    testid: 'loc-filter-type',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 130,
    options: statusFilterOptions,
    searchOnChange: true,
    testid: 'loc-filter-status',
  },
]);

/** 拉一页（行保持原始 `Location`，仓库名 / 类型 / 状态在单元格里现译，树后到也能跟上） */
const fetcher: ListFetcher<Location> = (query) =>
  fetchLocationPage(query as Parameters<typeof fetchLocationPage>[0]);

/** 入参拼装（页面自己的 `buildLocationQuery`，不换接口契约） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildLocationQuery(state as LocationFilterState, page, pageSize);

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 一页拉完：同步页码（保存后回页要用） */
function onLoaded(payload: { page: number }): void {
  current.value = payload.page;
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 仓库树（失败不阻塞列表：仓库名退化成 uuid 短码，页面仍可用） */
async function loadTree(): Promise<void> {
  try {
    tree.value = await fetchWarehouseTree();
  } catch (error) {
    tree.value = [];
    MessagePlugin.warning(`仓库树加载失败，所属仓库暂时显示 id：${messageOf(error)}`);
  }
}

/** 「刷新」按钮：仓库树顺带重拉（组件的「刷新」只刷列表，树要页面自己跟） */
function handleRefresh(): void {
  void loadTree();
}

// ==================== 详情抽屉 ====================

/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情数据 */
const detail = ref<Location | null>(null);
/** 详情加载中 */
const detailLoading = ref(false);
/** 详情错误 */
const detailError = ref('');

/**
 * 打开详情。
 *
 * 列表接口本来就下发全字段，但详情仍走 `/location/{id}`：抽屉是「这一行的当前真相」，
 * 而列表可能是几分钟前拉的（别人刚改过状态，列表不会自己刷新）。
 */
async function openDetail(row: Location): Promise<void> {
  detailVisible.value = true;
  detail.value = row;
  detailLoading.value = true;
  detailError.value = '';
  try {
    detail.value = await fetchLocationDetail(row.id);
  } catch (error) {
    detailError.value = `库位详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 新建 / 编辑（复用仓库页弹窗） ====================

/** 弹窗可见 */
const formVisible = ref(false);
/** 编辑对象（`null` = 新建） */
const editing = ref<Location | null>(null);

/** 新建（所属仓库预选 = 当前筛选的仓库；没筛就让人在弹窗里选叶子仓库） */
function openCreate(): void {
  editing.value = null;
  formVisible.value = true;
}

/** 编辑（仓库不可换：后端 `UpdateLocationReq` 没有 `warehouse_id`） */
function openEdit(row: Location): void {
  editing.value = row;
  formVisible.value = true;
}

/** 保存成功：刷新列表（新建回第 1 页，让新行按 sort_order/code 出现在视野里） */
async function handleSaved(payload: { id: string; isEdit: boolean }): Promise<void> {
  await reloadList(payload.isEdit ? current.value : 1);
}

// ==================== 初始化 ====================

onMounted(async () => {
  // 列表由 ListPage 自己在 setup 里拉首屏；这里只补仓库树（拉不到只是仓库名显示短 id）
  await loadTree();
});
</script>

<style scoped lang="less">
.location-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 筛选行 ==================== */

.filter-card {
  flex: 0 0 auto;
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

/* 仓库是本页主轴，给最宽 */
.filter-row__warehouse {
  width: 280px;
}

.filter-row__keyword {
  width: 220px;
}

.filter-row__select {
  width: 150px;
}

.filter-row__select--narrow {
  width: 130px;
}

.filter-row__spacer {
  flex: 1 1 auto;
}

.panel-head__link {
  color: var(--td-brand-color);
  text-decoration: none;
}

.panel-head__link:hover {
  text-decoration: underline;
}

.list-alert {
  margin-bottom: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 12px;
}
/* ==================== 详情抽屉 ==================== */

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 16px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.detail__code {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__grid {
  display: grid;
  grid-template-columns: 84px minmax(0, 1fr);
  gap: 10px 12px;
  margin: 0;
  font-size: 13px;
}

.detail__grid dt {
  color: var(--td-text-color-placeholder);
}

.detail__grid dd {
  margin: 0;
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.detail__id {
  color: var(--td-text-color-secondary);
}

.dlg-alert {
  margin-bottom: 12px;
}
</style>
