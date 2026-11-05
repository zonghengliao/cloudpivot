<template>
  <div class="location-page">
    <!-- ① 筛选行：仓库筛选是本页的主轴（库位挂在仓库下，不选就是跨仓总览） -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-select
          v-model="filter.warehouse_id"
          class="filter-row__warehouse"
          :options="warehouseOptions"
          placeholder="全部仓库（跨仓总览）"
          clearable
          filterable
          data-testid="loc-filter-warehouse"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="库位编码 / 名称（模糊）"
          clearable
          data-testid="loc-filter-keyword"
          @enter="handleSearch"
        />
        <t-select
          v-model="filter.location_type"
          class="filter-row__select"
          :options="typeFilterOptions"
          placeholder="全部类型"
          clearable
          data-testid="loc-filter-type"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="filter-row__select--narrow"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="loc-filter-status"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-loc-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-loc-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" :loading="loading" data-testid="btn-loc-refresh" @click="handleRefresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="filter-row__spacer" />

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
      </div>
    </t-card>

    <!-- ② 列表（服务端分页：切页重新打接口，不在前端切片） -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">库位列表</span>
          <span class="panel-head__meta" data-testid="loc-meta">
            共 {{ total }} 个库位{{ filter.warehouse_id ? '（当前仓库）' : '（全部仓库）' }}
          </span>
        </div>
        <span class="panel-head__hint">
          单仓内的新建 / 删除也可在
          <router-link class="panel-head__link" to="/business/erp/warehouse">仓库管理</router-link>
          页操作
        </span>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadLocations(current)">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="tableRows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="loc-table"
        @page-change="handlePageChange"
      >
        <template #location_code="{ row }">
          <span class="mono" :data-testid="`loc-code-${row.location_code}`">{{ row.location_code }}</span>
        </template>

        <template #location_name="{ row }">
          {{ dash(row.location_name) }}
        </template>

        <template #warehouse_id="{ row }">
          <!-- LocationRes 只有 warehouse_id，没有名称：这里用 warehouse/tree 翻译 -->
          <span :data-testid="`loc-warehouse-${row.location_code}`">{{ row.warehouse_text }}</span>
        </template>

        <template #location_type="{ row }">
          <t-tag size="small" :theme="locationTypeTheme(row.location_type)" variant="light">
            {{ row.type_label }}
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
            {{ row.status_label }}
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
      </t-table>
    </t-card>

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
  toLocationRow,
  warehouseFilterOptions,
  warehouseNameOf,
  type LocationFilterState,
  type LocationRowView,
} from './location-logic';

/**
 * 本页权限码（与 `backend/migrations/business/V0141__erp_permissions.up.sql` 逐字一致）。
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

// ==================== 主数据 / 筛选 ====================

/** 筛选行 */
const filter = reactive<LocationFilterState>(emptyLocationFilter());
/** 原始行数据 */
const rows = ref<Location[]>([]);
/** 总条数（后端 `total`） */
const total = ref(0);
/** 当前页 */
const current = ref(1);
/** 每页条数（服务端分页） */
const pageSize = LOCATION_PAGE_SIZE;
/** 列表加载中 */
const loading = ref(false);
/** 列表加载错误 */
const loadError = ref('');
/** 仓库树（仓库下拉 + `warehouse_id → 名称` 翻译） */
const tree = ref<WarehouseTreeNode[]>([]);

/** 是否处于筛选态（空态文案分「没数据」「没匹配」） */
const filtered = computed(() => isLocationFiltered(filter));

/** 仓库筛选下拉（全部仓库：非叶子也要能筛，否则历史数据永远筛不出来） */
const warehouseOptions = computed(() => warehouseFilterOptions(tree.value));

/** 表格数据 = 原始行 + 翻译好的仓库名 / 类型 / 状态 */
const tableRows = computed<LocationRowView[]>(() =>
  rows.value.map((row) => toLocationRow(row, tree.value)),
);

/** 分页器（服务端分页：`total` 用后端回的值，不在前端算） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 拉列表。
 *
 * 页码夹紧放在**后端回参之后**：删光最后一页的行时后端会回一个空页，
 * 直接把 `current` 落回 `maxPage` 会让分页器停在一个空页上（用户以为数据没了）。
 */
async function loadLocations(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchLocationPage(buildLocationQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, data.page ?? page), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `库位列表加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
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

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadLocations(1);
}

/** 重置筛选（回跨仓总览） */
function handleReset(): void {
  Object.assign(filter, emptyLocationFilter());
  current.value = 1;
  void loadLocations(1);
}

/** 刷新（保留当前筛选与页码） */
function handleRefresh(): void {
  void loadTree();
  void loadLocations(current.value);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number }): void {
  current.value = pageInfo.current;
  void loadLocations(pageInfo.current);
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
  const page = payload.isEdit ? current.value : 1;
  current.value = page;
  await loadLocations(page);
}

// ==================== 初始化 ====================

onMounted(async () => {
  await Promise.all([loadTree(), loadLocations(1)]);
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

/* ==================== 列表 ==================== */

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
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
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

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
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
