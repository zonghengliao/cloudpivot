<template>
  <div class="seq-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-seq-refresh" @click="loadSeqs()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="序列类型 / 名称（如 PO、采购订单）"
          clearable
          data-testid="seq-filter-keyword"
          @enter="handleSearch"
        />
        <t-select
          v-model="statusModel"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="seq-filter-status"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-seq-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-seq-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 序列列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">单号序列</span>
          <span class="panel-head__meta" data-testid="seq-meta">共 {{ total }} 个取号器</span>
        </div>
        <!-- 取号器是各业务模块的号段状态，改它等于改「下一个号从几开始」 -->
        <span class="panel-head__tip" data-testid="seq-readonly-tip">
          只读：号段由业务单据取号时自动推进
        </span>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="单号序列接口尚未上线"
        class="list-alert"
        data-testid="seq-api-missing"
      >
        <template #description>
          <span class="mono">GET /api/business/erp/seq/page</span> 返回 404。接口上线后本页无需改动。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="seq-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadSeqs()">重试</t-button>
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
        data-testid="seq-table"
        @page-change="handlePageChange"
      >
        <template #seq_type="{ row }">
          <span class="mono seq-type" :data-testid="`seq-type-${row.seq_type}`">{{ row.seq_type }}</span>
        </template>

        <template #seq_name="{ row }">
          <span :data-testid="`seq-name-${row.seq_type}`">{{ row.seq_name }}</span>
        </template>

        <template #prefix="{ row }">
          <span class="mono">{{ row.prefix || '—' }}</span>
        </template>

        <template #date_pattern="{ row }">
          <span class="muted">{{ datePatternText(row.date_pattern) }}</span>
        </template>

        <template #period_key="{ row }">
          <span class="mono" :class="{ muted: !row.period_key }">{{ periodKeyText(row) }}</span>
        </template>

        <template #current_no="{ row }">
          <span class="num strong" :data-testid="`seq-current-${row.seq_type}`">{{ row.current_no }}</span>
        </template>

        <template #step="{ row }">
          <span class="num muted">{{ row.step }}</span>
        </template>

        <template #next_no="{ row }">
          <span class="num" :data-testid="`seq-next-${row.seq_type}`">{{ nextNoOf(row) }}</span>
        </template>

        <template #separator="{ row }">
          <span class="mono muted">{{ separatorText(row.separator) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="seqStatusTheme(row.status)"
            :data-testid="`seq-status-${row.seq_type}`"
          >
            {{ seqStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #remark="{ row }">
          <span class="muted ellipsis">{{ row.remark || '—' }}</span>
        </template>

        <template #op="{ row }">
          <t-link theme="primary" hover="color" data-testid="btn-seq-detail" @click="openDetail(row)">
            详情
          </t-link>
        </template>
      </t-table>

      <div v-else class="panel-empty" data-testid="seq-empty">
        <t-icon name="automatic-numbering" class="panel-empty__icon" />
        <p class="panel-empty__title">{{ filtered ? '没有匹配的取号器' : '暂无取号器' }}</p>
        <p class="panel-empty__desc">
          {{ filtered ? '换个关键词或状态再试' : '取号器由各业务模块的种子迁移写入（如 PO / IN / TR）' }}
        </p>
      </div>

      <p class="panel-foot" data-testid="seq-foot-tip">
        「下次序号」= 当前序号 + 步长，<b>只在同一周期内成立</b>：跨周期（按日 / 按月）后端会先把序号归零再取号，
        实际发出的号可能更小。完整的单号模板由各业务模块决定（如采购单是
        <span class="mono">PO-yyyyMMdd-000001</span>），不由本表的前缀 / 分隔符驱动。
      </p>
    </t-card>

    <!-- ==================== ③ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="520px"
      :footer="false"
      :header="false"
      data-testid="seq-drawer"
    >
      <div v-if="detail" class="drawer" data-testid="seq-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ detail.seq_type }}</span>
            <span class="drawer-head__name">{{ detail.seq_name }}</span>
            <t-tag size="small" variant="light" :theme="seqStatusTheme(detail.status)">
              {{ seqStatusLabel(detail.status) }}
            </t-tag>
          </div>
          <t-button
            size="small"
            variant="outline"
            data-testid="btn-seq-drawer-close"
            @click="detailVisible = false"
          >
            关闭
          </t-button>
        </div>

        <div class="drawer-body">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">序列类型</span>
              <span class="field__value mono">{{ detail.seq_type }}</span>
            </div>
            <div class="field">
              <span class="field__label">名称</span>
              <span class="field__value">{{ detail.seq_name }}</span>
            </div>
            <div class="field">
              <span class="field__label">前缀</span>
              <span class="field__value mono">{{ detail.prefix || '—' }}</span>
            </div>
            <div class="field">
              <span class="field__label">分隔符</span>
              <span class="field__value mono">{{ separatorText(detail.separator) }}</span>
            </div>
            <div class="field">
              <span class="field__label">日期段格式</span>
              <span class="field__value">{{ datePatternText(detail.date_pattern) }}</span>
            </div>
            <div class="field">
              <span class="field__label">当前周期</span>
              <span class="field__value mono">{{ periodKeyText(detail) }}</span>
            </div>
            <div class="field">
              <span class="field__label">当前序号</span>
              <span class="field__value num strong">{{ detail.current_no }}</span>
            </div>
            <div class="field">
              <span class="field__label">步长</span>
              <span class="field__value num">{{ detail.step }}</span>
            </div>
            <div class="field">
              <span class="field__label">下次序号（同周期）</span>
              <span class="field__value num">{{ nextNoOf(detail) }}</span>
            </div>
            <div class="field">
              <span class="field__label">更新时间</span>
              <span class="field__value muted">{{ timeText(detail.updated_at) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ detail.remark || '—' }}</span>
            </div>
          </div>

          <p class="drawer-hint">
            这张表是<b>取号规则字典</b>：`current_no` 是当前周期已经发到第几号，不是「已发出的单号清单」。
            要查某张单的号，去对应的业务页面（采购订单 / 收货入库 / 调拨单 …）。
          </p>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 单号序列页（卡 t_6f023cee / BUG-SYS-01）—— **只读**，展示 `biz_seq` 取号器状态。
 *
 * # 为什么需要这一页
 *
 * `/business/erp/seq`（menu_type=2，is_visible=true）在 `sys_menu` 里一直可见，
 * 但前端既没注册路由也没有组件 → 点侧栏 404。后端只有 `GET /seq/page` 与
 * `GET /seq/{id}`（没有写接口），所以这里交只读页，不做「新建取号器」的假按钮。
 *
 * # 为什么不做「下一个单号」的完整预览
 *
 * 单号模板写死在**各业务模块**里（`purchase_model.rs::format_order_no` 是
 * `PO-{period_key}-{no:06}`，根本不读本表的 prefix/separator）。凭本表字段拼一个
 * 看起来很像的字符串，只会让人拿它去对账然后发现对不上 —— 所以只给「下次序号」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { fetchSeqDetail, fetchSeqPage, type SeqRow } from '@/api/business/erp';
import {
  SEQ_STATUS_OPTIONS,
  buildSeqQuery,
  datePatternText,
  emptySeqFilter,
  isSeqFiltered,
  nextNoOf,
  periodKeyText,
  seqStatusLabel,
  seqStatusTheme,
  separatorText,
  type SeqFilterState,
} from './seq-logic';

/** 每页条数（取号器是「一类单号一行」，一共十几行，20 足够一页看完） */
const PAGE_SIZE = 20;

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'seq_type', title: '类型', width: 90, fixed: 'left' },
  { colKey: 'seq_name', title: '名称', minWidth: 160, ellipsis: true },
  { colKey: 'prefix', title: '前缀', width: 90 },
  { colKey: 'date_pattern', title: '日期段', width: 150 },
  { colKey: 'period_key', title: '当前周期', width: 130 },
  { colKey: 'current_no', title: '当前序号', width: 110, align: 'right' },
  { colKey: 'step', title: '步长', width: 80, align: 'right' },
  { colKey: 'next_no', title: '下次序号', width: 110, align: 'right' },
  { colKey: 'separator', title: '分隔符', width: 90 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'remark', title: '备注', minWidth: 160, ellipsis: true },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

const statusOptions = SEQ_STATUS_OPTIONS.map((item) => ({ ...item }));

const filter = reactive<SeqFilterState>(emptySeqFilter());

/** `t-select` 的 v-model 不收 null：清空 → `null`（= 全部） */
const statusModel = computed<number | undefined>({
  get: () => (filter.status === null ? undefined : filter.status),
  set: (value) => {
    filter.status = typeof value === 'number' ? value : null;
  },
});

const rows = ref<SeqRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

const detailVisible = ref(false);
const detail = ref<SeqRow | null>(null);

const filtered = computed(() => isSeqFiltered(filter));

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

/** `2026-10-31T03:12:00Z` → `2026-10-31 03:12:00` */
function timeText(value: string | null | undefined): string {
  const raw = (value ?? '').trim();
  if (!raw) {
    return '—';
  }
  return raw.slice(0, 19).replace('T', ' ');
}

/** 拉列表 */
async function loadSeqs(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchSeqPage(buildSeqQuery(filter, page, PAGE_SIZE));
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

/** 查询：条件变了回第 1 页 */
function handleSearch(): void {
  void loadSeqs(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptySeqFilter());
  void loadSeqs(1);
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadSeqs(pageInfo.current);
}

/** 打开详情：先渲染列表行，再拉详情覆盖（同采购明细页的取舍） */
async function openDetail(row: SeqRow): Promise<void> {
  detail.value = row;
  detailVisible.value = true;
  try {
    detail.value = await fetchSeqDetail(row.id);
  } catch {
    // 拉不到就保留列表行
  }
}

onMounted(() => {
  void loadSeqs(1);
});
</script>

<style scoped lang="less">
.seq-page {
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

.tool-row__keyword {
  width: 260px;
}

.tool-row__status {
  width: 130px;
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

.seq-type {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.ellipsis {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.panel-foot {
  margin: 12px 0 0;
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-placeholder);
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
  font-size: 16px;
  font-weight: 600;
}

.drawer-head__name {
  font-size: 13px;
  color: var(--td-text-color-secondary);
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
