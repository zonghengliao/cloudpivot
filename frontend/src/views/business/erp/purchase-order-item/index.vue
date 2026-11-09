<template>
  <div class="item-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-item-refresh" @click="loadItems()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="搜索单号 / 物料编码 / 物料名称"
          clearable
          data-testid="item-filter-keyword"
          @enter="handleSearch"
          @clear="handleKeywordClear"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
        <t-select
          v-model="orderModel"
          class="tool-row__order"
          :options="orderOptions"
          placeholder="全部采购单"
          filterable
          clearable
          data-testid="item-filter-order"
          @change="handleSearch"
        />
        <t-select
          v-model="materialModel"
          class="tool-row__material"
          :options="materialOptions"
          placeholder="全部物料"
          filterable
          clearable
          data-testid="item-filter-material"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-item-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-item-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 明细列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">采购明细</span>
          <span class="panel-head__meta" data-testid="item-meta">共 {{ total }} 行</span>
        </div>
        <!-- 明细行只能由采购单的建单 / 收货流程写入，本页没有写入口 -->
        <span class="panel-head__tip" data-testid="item-readonly-tip">
          只读：明细随采购单建单 / 收货过账自动生成
        </span>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="采购明细接口尚未上线"
        class="list-alert"
        data-testid="item-api-missing"
      >
        <template #description>
          <span class="mono">GET /api/business/erp/purchase-order-item/page</span> 返回 404。接口上线后本页无需改动。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="item-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadItems()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        v-if="rows.length > 0 || loading"
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="item-table"
        @page-change="handlePageChange"
      >
        <template #order_id="{ row }">
          <span class="mono order-no" :data-testid="`item-order-${row.id}`">
            {{ orderText(row.order_id) }}
          </span>
        </template>

        <template #line_no="{ row }">
          <span class="muted">{{ row.line_no }}</span>
        </template>

        <template #material_id="{ row }">
          <span :data-testid="`item-material-${row.id}`">{{ materialText(row.material_id) }}</span>
        </template>

        <template #unit_id="{ row }">
          <span class="muted">{{ unitText(row.unit_id) }}</span>
        </template>

        <template #qty="{ row }">
          <span class="num">{{ decimalText(row.qty) }}</span>
        </template>

        <template #price="{ row }">
          <span class="num">{{ formatMoney(row.price) }}</span>
        </template>

        <template #tax_rate="{ row }">
          <span class="muted">{{ taxRateText(row.tax_rate) }}</span>
        </template>

        <template #amount="{ row }">
          <span class="num">{{ formatMoney(row.amount) }}</span>
        </template>

        <template #tax_amount="{ row }">
          <span class="num muted">{{ formatMoney(row.tax_amount) }}</span>
        </template>

        <template #with_tax_amount="{ row }">
          <span class="num strong" :data-testid="`item-with-tax-${row.id}`">
            {{ formatMoney(row.with_tax_amount) }}
          </span>
        </template>

        <template #received_qty="{ row }">
          <span class="received" :class="{ 'received--done': isFullyReceived(row) }">
            {{ receivedText(row) }}
          </span>
          <t-tag v-if="isFullyReceived(row)" size="small" variant="light" theme="success" class="done-tag">
            已收满
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-link theme="primary" hover="color" data-testid="btn-item-detail" @click="openDetail(row)">
            详情
          </t-link>
        </template>
      </t-table>

      <div v-else class="panel-empty" data-testid="item-empty">
        <t-icon name="order-list" class="panel-empty__icon" />
        <p class="panel-empty__title">{{ filtered ? '没有匹配的采购明细' : '暂无采购明细' }}</p>
        <p class="panel-empty__desc">
          {{ filtered ? '换个关键词、采购单或物料再试' : '采购单建单后，明细行会自动出现在这里' }}
        </p>
      </div>
    </t-card>

    <!-- ==================== ③ 明细抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="560px"
      :footer="false"
      :header="false"
      data-testid="item-drawer"
    >
      <div v-if="detail" class="drawer" data-testid="item-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ orderText(detail.order_id) }}</span>
            <t-tag size="small" variant="light" theme="primary">第 {{ detail.line_no }} 行</t-tag>
            <t-tag v-if="isFullyReceived(detail)" size="small" variant="light" theme="success">已收满</t-tag>
          </div>
          <t-button
            size="small"
            variant="outline"
            data-testid="btn-item-drawer-close"
            @click="detailVisible = false"
          >
            关闭
          </t-button>
        </div>

        <div class="drawer-body">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">物料</span>
              <span class="field__value">{{ materialText(detail.material_id) }}</span>
            </div>
            <div class="field">
              <span class="field__label">计量单位</span>
              <span class="field__value">{{ unitText(detail.unit_id) }}</span>
            </div>
            <div class="field">
              <span class="field__label">数量</span>
              <span class="field__value num">{{ decimalText(detail.qty) }}</span>
            </div>
            <div class="field">
              <span class="field__label">已收货</span>
              <span class="field__value num">{{ decimalText(detail.received_qty) }}</span>
            </div>
            <div class="field">
              <span class="field__label">不含税单价</span>
              <span class="field__value num">{{ formatMoney(detail.price) }}</span>
            </div>
            <div class="field">
              <span class="field__label">税率</span>
              <span class="field__value">{{ taxRateText(detail.tax_rate) }}</span>
            </div>
            <div class="field">
              <span class="field__label">不含税金额</span>
              <span class="field__value num">{{ formatMoney(detail.amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">税额</span>
              <span class="field__value num">{{ formatMoney(detail.tax_amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">价税合计</span>
              <span class="field__value num strong">{{ formatMoney(detail.with_tax_amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">更新时间</span>
              <span class="field__value muted">{{ timeText(detail.updated_at) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">行备注</span>
              <span class="field__value">{{ detail.remark || '—' }}</span>
            </div>
          </div>

          <p class="drawer-hint">
            单价是<b>不含税单价</b>（列表接口不返回含税单价）；含税口径请以「价税合计」为准，
            不要在页面上用「单价 ×(1+税率)」反算 —— 与库里的金额会有分位差。
          </p>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 采购明细页（卡 t_6f023cee / BUG-SYS-01）—— **只读台账**。
 *
 * # 为什么需要这一页
 *
 * `sys_menu` 里 `/business/erp/purchase-order-item`（menu_type=2，is_visible=true）
 * 一直都在，但 `src/router/index.ts` 没注册路由、`views/` 下也没有组件文件 ——
 * 用户点侧栏必然 404（测试C 的 n08 用例钉住了这条不变量）。
 * 后端接口（`/purchase-order-item/page` + `/{id}`）自 V0141 起就齐全，缺的只是页面。
 *
 * # 为什么没有「新建 / 编辑」
 *
 * 明细行由采购单的建单 / 改单整体覆盖（`PUT /purchase-order/{id}` 只认 DRAFT），
 * 以及收货过账回写 `received_qty`。单独改一行明细会绕过订单的金额一致性校验
 * （硬红线：金额一律服务端算），所以本页只读，页头写清责任归属。
 *
 * # 主数据只拉 200 条
 *
 * 采购单 / 物料 / 单位都是**弱引用**（只存 id）。这里按 200 条拉一次建 Map，
 * 拉不到就退回短 id 显示 —— 名称列退化，但列表绝不给空白（见 `labelOf`）。
 *
 * 关键词搜索（卡 E2E01-FIX-2）会命中**全表**，命中行可能落在「最新 200 条主数据」
 * 之外 —— 那种行的单号 / 物料名会显示成短 id。这是已知的显示上限（后端列表 DTO 不返
 * 单号与物料名），不是筛选错了：筛选结果本身是全的，可点「详情」核对金额 / 行号。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchMaterialPage,
  fetchPurchaseOrderItemDetail,
  fetchPurchaseOrderItemPage,
  fetchPurchaseOrderPage,
  fetchUnitPage,
  type MaterialRow,
  type PurchaseOrderItemLine,
  type PurchaseOrderRow,
  type Unit,
} from '@/api/business/erp';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  buildItemQuery,
  decimalText,
  emptyItemFilter,
  isFullyReceived,
  isItemFiltered,
  labelOf,
  receivedText,
  taxRateText,
  type ItemFilterState,
} from './purchase-order-item-logic';

/** 每页条数 */
const PAGE_SIZE = 20;
/** 主数据一次拉取条数（后端 page_size 上限 200） */
const OPTION_LIMIT = 200;

/** 表格列（顺序 = 采购人读一行明细的顺序：单号 → 哪一行 → 什么货 → 多少 → 多少钱） */
const columns: PrimaryTableCol[] = [
  { colKey: 'order_id', title: '采购单', width: 210, fixed: 'left' },
  { colKey: 'line_no', title: '行', width: 60 },
  { colKey: 'material_id', title: '物料', minWidth: 180, ellipsis: true },
  { colKey: 'unit_id', title: '单位', width: 90 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'price', title: '单价(不含税)', width: 140, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 90, align: 'right' },
  { colKey: 'amount', title: '不含税金额', width: 150, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 130, align: 'right' },
  { colKey: 'with_tax_amount', title: '价税合计', width: 150, align: 'right' },
  { colKey: 'received_qty', title: '已收货', width: 180 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

const filter = reactive<ItemFilterState>(emptyItemFilter());

/** `t-select` 的 v-model 不收 null：清空 → `''`（= 全部） */
const orderModel = computed<string | undefined>({
  get: () => filter.order_id || undefined,
  set: (value) => {
    filter.order_id = typeof value === 'string' ? value : '';
  },
});

const materialModel = computed<string | undefined>({
  get: () => filter.material_id || undefined,
  set: (value) => {
    filter.material_id = typeof value === 'string' ? value : '';
  },
});

/** 主数据（拉不到就空 Map，名称列退化成短 id） */
const orders = ref<PurchaseOrderRow[]>([]);
const materials = ref<MaterialRow[]>([]);
const units = ref<Unit[]>([]);

const orderNoMap = computed(() => new Map(orders.value.map((item) => [item.id, item.order_no])));
const materialNameMap = computed(
  () =>
    new Map(
      materials.value.map((item) => [
        item.id,
        item.material_code ? `${item.material_name}（${item.material_code}）` : item.material_name,
      ]),
    ),
);
const unitNameMap = computed(() => new Map(units.value.map((item) => [item.id, item.unit_name])));

const orderOptions = computed(() =>
  orders.value.map((item) => ({ label: item.order_no, value: item.id })),
);
const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_name}（${item.material_code}）`,
    value: item.id,
  })),
);

const rows = ref<PurchaseOrderItemLine[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

/** 抽屉 */
const detailVisible = ref(false);
const detail = ref<PurchaseOrderItemLine | null>(null);

const filtered = computed(() => isItemFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// ==================== 行为 ====================

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 未注册路由 = HTTP 404；业务 404 = `10002` */
function isApiMissing(error: unknown): boolean {
  if (!(error instanceof ApiError)) {
    return false;
  }
  if (error.code === 404) {
    return true;
  }
  return error.message.includes('404') || error.message.includes('请求的资源不存在');
}

/** 采购单号（主数据缺这条时显示短 id，不留白） */
function orderText(orderId: string): string {
  return labelOf(orderNoMap.value, orderId);
}

/** 物料名 + 编码 */
function materialText(materialId: string): string {
  return labelOf(materialNameMap.value, materialId);
}

/** 单位名 */
function unitText(unitId: string): string {
  return labelOf(unitNameMap.value, unitId);
}

/** `2026-10-31T03:12:00Z` → `2026-10-31 03:12:00`（列表/抽屉统一口径，不做时区换算） */
function timeText(value: string | null | undefined): string {
  const raw = (value ?? '').trim();
  if (!raw) {
    return '—';
  }
  return raw.slice(0, 19).replace('T', ' ');
}

/** 拉列表 */
async function loadItems(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchPurchaseOrderItemPage(buildItemQuery(filter, page, PAGE_SIZE));
    apiMissing.value = false;
    rows.value = data.list ?? [];
    total.value = data.total ?? rows.value.length;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / PAGE_SIZE));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    apiMissing.value = isApiMissing(error);
    loadError.value = apiMissing.value ? '' : messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 拉主数据（任一失败都不挡列表） */
async function loadRefs(): Promise<void> {
  const [orderPage, materialPage, unitPage] = await Promise.all([
    fetchPurchaseOrderPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchUnitPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
  ]);
  orders.value = orderPage?.list ?? [];
  materials.value = materialPage?.list ?? [];
  units.value = unitPage?.list ?? [];
}

/** 查询：条件变了回第 1 页 */
function handleSearch(): void {
  void loadItems(1);
}

/**
 * 清空关键词 → 恢复全量。
 *
 * 不能直接复用 `handleSearch`：`clear` 事件与 `update:modelValue` 的先后顺序由组件
 * 内部决定，掐不清时可能拿旧关键词再查一次（界面清空了、结果还是过滤后的）。
 * 这里显式把 keyword 归零再查，口径不依赖事件顺序。
 */
function handleKeywordClear(): void {
  filter.keyword = '';
  void loadItems(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptyItemFilter());
  void loadItems(1);
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadItems(pageInfo.current);
}

/**
 * 打开详情：先用列表行渲染（零等待），再拉一次详情覆盖。
 *
 * 列表元素与详情同构（`PurchaseOrderItemRes`），但详情可能是别的卡刚改过的行 ——
 * 先渲染再刷新，既有即时反馈又不会展示过期数据。
 */
async function openDetail(row: PurchaseOrderItemLine): Promise<void> {
  detail.value = row;
  detailVisible.value = true;
  try {
    detail.value = await fetchPurchaseOrderItemDetail(row.id);
  } catch {
    // 详情拉不到就保留列表行（不弹错：抽屉里已有可用数据）
  }
}

onMounted(() => {
  void loadRefs();
  void loadItems(1);
});
</script>

<style scoped lang="less">
.item-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 工具栏 ==================== */

.tool-card {
  flex: 0 0 auto;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__order {
  width: 220px;
}

.tool-row__keyword {
  width: 260px;
}

.tool-row__material {
  width: 240px;
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
  flex-wrap: wrap;
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

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
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

.num {
  font-variant-numeric: tabular-nums;
}

.strong {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.order-no {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.received {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.received--done {
  color: var(--td-success-color);
}

.done-tag {
  margin-left: 6px;
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

/* ==================== 抽屉 ==================== */

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.drawer-head__title {
  font-size: 15px;
  font-weight: 600;
}

.drawer-body {
  padding-top: 14px;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 16px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.field--wide {
  grid-column: 1 / -1;
}

.field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.field__value {
  font-size: 13px;
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.drawer-hint {
  margin: 16px 0 0;
  padding: 10px 12px;
  border-radius: var(--td-radius-medium);
  background: var(--td-bg-color-container-hover);
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
}
</style>
