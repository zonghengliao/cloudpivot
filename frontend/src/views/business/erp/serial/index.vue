<template>
  <div class="serial-page">
    <!-- ① 扫码行：仓库作业的第一入口，条码枪 = 快速输入 + 回车，所以放最上面且 autofocus -->
    <t-card :bordered="false" class="scan-card">
      <div class="scan-row">
        <span class="scan-row__label">
          <t-icon name="barcode" class="scan-row__icon" />
          扫码
        </span>
        <t-input
          ref="scanInputRef"
          v-model="scanInput"
          class="scan-row__input"
          placeholder="扫描或输入序列号，如 SN-20261030-000001"
          clearable
          autofocus
          :loading="scanning"
          data-testid="serial-scan-input"
          @enter="handleScan"
          @clear="handleScanClear"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
        <t-button
          v-privilege="[PERM.serialView]"
          theme="primary"
          :loading="scanning"
          data-testid="btn-scan"
          @click="handleScan"
        >
          查询
        </t-button>
        <span class="scan-row__hint">条码枪扫完自动回车，回车即查</span>
      </div>

      <!-- 未命中：红字（不是弹窗/红条 —— 扫错标签是日常，不是故障） -->
      <p v-if="scanError" class="scan-error" data-testid="serial-scan-error">
        <t-icon name="error-circle" />
        {{ scanError }}
      </p>

      <!-- 命中：信息卡（物料 / 仓库 / 库位 / 状态 / 入库），名称只有扫码出参才有 -->
      <div v-if="scanResult" class="hit-card" data-testid="serial-scan-hit">
        <div class="hit-card__head">
          <span class="hit-card__no">{{ scanResult.serial_no }}</span>
          <t-tag size="small" :theme="serialStatusTheme(scanResult.status)" variant="light">
            {{ serialStatusLabel(scanResult.status) }}
          </t-tag>
        </div>
        <div class="hit-card__grid">
          <div class="hit-field">
            <span class="hit-field__label">物料</span>
            <span class="hit-field__value">{{ scanResult.material_name }}（{{ scanResult.material_code }}）</span>
          </div>
          <div class="hit-field">
            <span class="hit-field__label">仓库</span>
            <span class="hit-field__value">{{ dash(scanResult.warehouse_name) }}</span>
          </div>
          <div class="hit-field">
            <span class="hit-field__label">库位</span>
            <span class="hit-field__value">{{ dash(scanResult.location_name) }}</span>
          </div>
          <div class="hit-field">
            <span class="hit-field__label">状态</span>
            <span class="hit-field__value">{{ serialStatusLabel(scanResult.status) }}</span>
          </div>
          <div class="hit-field">
            <span class="hit-field__label">入库</span>
            <span class="hit-field__value">{{ formatTime(scanResult.inbound_at) }}</span>
          </div>
          <div class="hit-field">
            <span class="hit-field__label">出库/报废</span>
            <span class="hit-field__value">{{ formatTime(scanResult.outbound_at) }}</span>
          </div>
        </div>
      </div>
    </t-card>

    <!-- ② 筛选行 -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-select
          v-model="filter.material_id"
          class="filter-row__select"
          :options="materialOptions"
          placeholder="全部物料"
          clearable
          filterable
          data-testid="serial-filter-material"
        />
        <t-select
          v-model="filter.warehouse_id"
          class="filter-row__select"
          :options="warehouseOptions"
          placeholder="全部仓库"
          clearable
          filterable
          data-testid="serial-filter-warehouse"
        />
        <t-select
          v-model="filter.status"
          class="filter-row__select filter-row__select--narrow"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="serial-filter-status"
        />
        <t-input
          v-model="filter.serial_no"
          class="filter-row__no"
          placeholder="序列号（模糊）"
          clearable
          data-testid="serial-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-serial-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-serial-reset" @click="handleReset">重置</t-button>

        <span class="filter-row__spacer" />

        <t-button
          v-privilege="[PERM.serialGenerate]"
          theme="primary"
          data-testid="btn-batch-generate"
          @click="openBatchDialog"
        >
          <template #icon><t-icon name="add" /></template>
          批量生成
        </t-button>
      </div>
    </t-card>

    <!-- ③ 列表 -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">序列号列表</span>
          <span class="panel-head__meta" data-testid="serial-meta">共 {{ total }} 条</span>
        </div>
        <span v-if="scannedId && !scanHitOnPage" class="panel-head__warn" data-testid="serial-scan-offpage">
          扫码命中的行不在当前页，已按该号筛选
        </span>
      </div>

      <t-alert
        v-if="batchResultText"
        theme="success"
        :message="batchResultText"
        close
        class="list-alert"
        data-testid="batch-result-hint"
        @close="batchResultText = ''"
      />

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadSerials()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="tableRows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :row-class-name="rowClassName"
        :hover="true"
        data-testid="serial-table"
        @page-change="handlePageChange"
      >
        <template #serial_no="{ row }">
          <span class="mono" :data-testid="`serial-no-${row.serial_no}`">{{ row.serial_no }}</span>
          <t-tag v-if="row.scanned" size="small" theme="warning" variant="light" class="hit-flag" data-testid="serial-hit-flag">
            扫码命中
          </t-tag>
        </template>

        <template #material_id="{ row }">
          {{ materialNameOf(row.material_id) }}
        </template>

        <template #warehouse_id="{ row }">
          {{ warehouseNameOf(row.warehouse_id) }}
        </template>

        <template #location_id="{ row }">
          <!-- SerialRes 只给 location_id（指向 biz_inventory_location.id），没有名称 -->
          <t-tooltip v-if="row.location_id" :content="row.location_id">
            <span class="mono muted">{{ shortId(row.location_id) }}</span>
          </t-tooltip>
          <span v-else class="muted">—</span>
        </template>

        <template #batch_no="{ row }">
          {{ dash(row.batch_no) }}
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="serialStatusTheme(row.status)" variant="light" :data-testid="`status-${row.serial_no}`">
            {{ serialStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #inbound_at="{ row }">
          <span class="muted">{{ formatTime(row.inbound_at) }}</span>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-tooltip :content="actionsOf(row).outbound ? '出库：在库 → 已出库（不可逆）' : serialActionHint(row.status)">
              <t-link
                v-privilege="[PERM.serialStatus]"
                theme="primary"
                :disabled="!actionsOf(row).outbound"
                :class="{ 'op-disabled': !actionsOf(row).outbound }"
                :data-testid="`btn-outbound-${row.serial_no}`"
                @click="confirmTransition(row, 'OUTBOUND')"
              >
                出库
              </t-link>
            </t-tooltip>
            <t-tooltip :content="actionsOf(row).scrap ? '报废：不可逆，报废后不能再出库' : serialActionHint(row.status)">
              <t-link
                v-privilege="[PERM.serialStatus]"
                theme="danger"
                :disabled="!actionsOf(row).scrap"
                :class="{ 'op-disabled': !actionsOf(row).scrap }"
                :data-testid="`btn-scrap-${row.serial_no}`"
                @click="confirmTransition(row, 'SCRAPPED')"
              >
                报废
              </t-link>
            </t-tooltip>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="serial-empty">
            <t-icon class="panel-empty__icon" name="barcode" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的序列号' : '还没有序列号' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个物料 / 仓库 / 状态，或点「重置」看全部' : '点右上角「批量生成」为一物一码物料生成号段' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ④ 批量生成弹窗 -->
    <t-dialog
      v-model:visible="batchVisible"
      header="批量生成序列号"
      :confirm-btn="{ content: '生成', loading: batchSaving }"
      :close-on-overlay-click="false"
      width="560px"
      data-testid="serial-batch-dialog"
      @confirm="submitBatch"
    >
      <t-alert v-if="batchSubmitError" theme="error" :message="batchSubmitError" class="dlg-alert" />

      <t-form :data="batchForm" label-width="96px">
        <t-form-item
          label="物料"
          name="material_id"
          :status="batchErrors.material_id ? 'error' : undefined"
          :tips="batchErrors.material_id"
        >
          <t-select
            v-model="batchForm.material_id"
            :options="serialMaterialOptions"
            placeholder="只列一物一码物料"
            filterable
            data-testid="batch-material"
          />
        </t-form-item>

        <t-form-item
          label="仓库"
          name="warehouse_id"
          :status="batchErrors.warehouse_id ? 'error' : undefined"
          :tips="batchErrors.warehouse_id"
        >
          <t-select
            v-model="batchForm.warehouse_id"
            :options="warehouseOptions"
            placeholder="必选：账面数量按 物料×仓库 派生"
            filterable
            data-testid="batch-warehouse"
          />
        </t-form-item>

        <t-form-item
          label="数量"
          name="count"
          :status="batchErrors.count ? 'error' : undefined"
          :tips="batchErrors.count"
        >
          <t-input-number
            v-model="batchForm.count"
            :min="1"
            :max="MAX_BATCH_COUNT"
            theme="normal"
            data-testid="batch-count"
          />
          <template #help>
            <span class="dlg-help">单次最多 {{ MAX_BATCH_COUNT }} 个</span>
          </template>
        </t-form-item>

        <!-- 成本单价：批量生成 = 无来源入库（期初/其他入库），没有采购单可挂，成本只能操作人录 -->
        <t-form-item
          label="成本单价"
          name="unit_cost"
          required
          :status="batchErrors.unit_cost ? 'error' : undefined"
          :tips="batchErrors.unit_cost"
        >
          <t-input
            v-model="batchForm.unit_cost"
            placeholder="必填，不含税，如 12.34"
            data-testid="batch-unit-cost"
          />
          <template #help>
            <span class="dlg-help">
              不含税单价，必填（> 0，最多 6 位小数）：系统按「件数 × 单价」记入库存金额并生成凭证
            </span>
          </template>
        </t-form-item>

        <!-- 金额预览：提交前让操作人核一眼「这次入账多少钱」（4 位小数，与账面 amount 同精度） -->
        <t-form-item label="入库金额">
          <div class="dlg-amount">
            <span class="dlg-amount__value" data-testid="batch-amount-preview">
              预计入库金额：{{ batchPreviewAmount }} 元
            </span>
            <span class="dlg-help">按「数量 × 单价」4 位小数，与账面 amount 一致</span>
          </div>
        </t-form-item>

        <t-form-item label="来源类型" name="source_type">
          <t-select
            v-model="batchForm.source_type"
            :options="sourceTypeOptions"
            placeholder="选填"
            clearable
            data-testid="batch-source-type"
          />
        </t-form-item>

        <t-form-item
          label="批次号"
          name="batch_no"
          :status="batchErrors.batch_no ? 'error' : undefined"
          :tips="batchErrors.batch_no"
        >
          <t-input v-model="batchForm.batch_no" :maxlength="64" placeholder="选填，如 B001" data-testid="batch-no" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  batchGenerateSerials,
  fetchMaterialPage,
  fetchSerialPage,
  fetchWarehousePage,
  scanSerial,
  updateSerialStatus,
  type MaterialRow,
  type SerialRow,
  type SerialScanRow,
  type SerialStatus,
  type Warehouse,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import {
  MAX_BATCH_COUNT,
  SERIAL_PAGE_SIZE,
  SERIAL_STATUS_FILTER_OPTIONS,
  SOURCE_TYPE_OPTIONS,
  applyScanHighlight,
  availableActions,
  batchGenerateErrorText,
  buildBatchPayload,
  buildSerialQuery,
  dash,
  emptyBatchForm,
  emptySerialFilter,
  formatBatchAmount,
  formatBatchResultText,
  formatTime,
  hasBatchFormError,
  isSerialFiltered,
  isSerialNotFound,
  serialActionHint,
  serialStatusLabel,
  serialStatusTheme,
  validateBatchForm,
  type BatchFormErrors,
  type BatchGenerateFormState,
  type SerialFilterState,
  type SerialRowWithScan,
} from './serial-logic';

/** 本页权限码（与 `backend/migrations/business/V0142__erp_serial_permissions.up.sql` 逐字一致） */
const PERM = {
  /** 列表（页面入口） */
  serialList: 'business:erp:serial:list',
  /** 扫码 */
  serialView: 'business:erp:serial:view',
  /** 批量生成 */
  serialGenerate: 'business:erp:serial:generate',
  /** 状态流转 */
  serialStatus: 'business:erp:serial:status',
} as const;

/** 表格列（`colKey` 与 `SerialRes` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  { colKey: 'serial_no', title: '序列号', width: 250, fixed: 'left' },
  { colKey: 'material_id', title: '物料', minWidth: 160, ellipsis: true },
  { colKey: 'warehouse_id', title: '仓库', width: 140, ellipsis: true },
  { colKey: 'location_id', title: '库位', width: 130 },
  { colKey: 'batch_no', title: '批次', width: 120, ellipsis: true },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'inbound_at', title: '入库', width: 150 },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

/** 状态下拉（`''` = 全部） */
const statusFilterOptions = SERIAL_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
/** 来源类型下拉 */
const sourceTypeOptions = SOURCE_TYPE_OPTIONS.map((item) => ({ ...item }));

// ==================== 扫码 ====================

/** 扫码输入框 DOM（回车后强制回焦，条码枪连扫才顺） */
const scanInputRef = ref<{ focus?: () => void } | null>(null);
/** 扫码框内容 */
const scanInput = ref('');
/** 扫码中 */
const scanning = ref(false);
/** 扫码红字提示 */
const scanError = ref('');
/** 扫码命中（信息卡数据源） */
const scanResult = ref<SerialScanRow | null>(null);
/** 命中行 id（表格高亮依据） */
const scannedId = ref<string | null>(null);
/** 命中行是否落在当前页（不在就提示「已按该号筛选」） */
const scanHitOnPage = ref(false);

/** 输入框回焦（扫完立刻能扫下一个） */
function focusScanInput(): void {
  void nextTick(() => scanInputRef.value?.focus?.());
}

/** 清空扫码态（点输入框的 × 时） */
function handleScanClear(): void {
  scanInput.value = '';
  scanError.value = '';
  scanResult.value = null;
  scannedId.value = null;
  scanHitOnPage.value = false;
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 扫码：`GET /serial/scan`。
 *
 * 命中后做三件事：
 * 1. 出信息卡（物料/仓库/库位/状态/入库 —— 名称只有扫码出参才有）；
 * 2. 表格高亮命中行；
 * 3. 命中行不在当前页时，把筛选改成这个号再查一次 —— 否则用户只会看到「什么都没发生」。
 */
async function handleScan(): Promise<void> {
  const serialNo = scanInput.value.trim();
  if (!serialNo) {
    scanError.value = '请先输入或扫描序列号';
    return;
  }
  scanning.value = true;
  scanError.value = '';
  try {
    const hit = await scanSerial(serialNo);
    scanResult.value = hit;
    scannedId.value = hit.id;
    if (!rows.value.some((row) => row.id === hit.id)) {
      Object.assign(filter, { ...emptySerialFilter(), serial_no: hit.serial_no });
      current.value = 1;
      await loadSerials(1);
    }
    scanHitOnPage.value = tableRows.value.some((row) => row.id === hit.id && row.scanned);
    await scrollToScannedRow(hit.id);
  } catch (error) {
    scanResult.value = null;
    scannedId.value = null;
    scanHitOnPage.value = false;
    // 404 / 10002 = 这个号不存在，红字提示；其余才是真错误
    scanError.value = isSerialNotFound(error)
      ? `未找到该序列号：${serialNo}`
      : `扫码查询失败：${messageOf(error)}`;
  } finally {
    scanning.value = false;
    focusScanInput();
  }
}

/** 把命中行滚到视野中间（`row-key="id"` 会落到 `data-row-key`） */
async function scrollToScannedRow(id: string): Promise<void> {
  await nextTick();
  const row = document.querySelector(`tr[data-row-key="${id}"]`);
  if (row instanceof HTMLElement) {
    row.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }
}

// ==================== 列表 / 筛选 ====================

/** 筛选行 */
const filter = reactive<SerialFilterState>(emptySerialFilter());
/** 原始行数据 */
const rows = ref<SerialRow[]>([]);
/** 总条数 */
const total = ref(0);
/** 当前页 */
const current = ref(1);
/** 每页条数 */
const pageSize = SERIAL_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');
/** 物料主数据（译 material_id → 名称 + 筛选下拉） */
const materials = ref<MaterialRow[]>([]);
/** 仓库主数据（译 warehouse_id → 名称 + 下拉） */
const warehouses = ref<Warehouse[]>([]);

/** 是否处于筛选态（空态文案分「没数据」「没匹配」） */
const filtered = computed(() => isSerialFiltered(filter));

/** 物料下拉（筛选：全部物料） */
const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

/** 物料下拉（批量生成：只列一物一码，后端会再判一次） */
const serialMaterialOptions = computed(() =>
  materials.value
    .filter((item) => item.serial_required)
    .map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

/** 仓库下拉 */
const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

/** 表格数据 = 原始行 + 扫码命中标记 */
const tableRows = computed<SerialRowWithScan[]>(() => applyScanHighlight(rows.value, scannedId.value).list);

/** 分页器（服务端分页） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 物料名称（查不到就显示 id 短码，别显示空白） */
function materialNameOf(materialId: string): string {
  const hit = materials.value.find((item) => item.id === materialId);
  return hit ? `${hit.material_name}（${hit.material_code}）` : shortId(materialId);
}

/** 仓库名称 */
function warehouseNameOf(warehouseId: string | null): string {
  if (!warehouseId) {
    return '—';
  }
  const hit = warehouses.value.find((item) => item.id === warehouseId);
  return hit ? hit.warehouse_name : shortId(warehouseId);
}

/** uuid 前 8 位（表里放全串撑爆列宽） */
function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}

/** 行动作可用性 */
function actionsOf(row: SerialRow): ReturnType<typeof availableActions> {
  return availableActions(row.status);
}

/** 扫码命中行的 class */
function rowClassName({ row }: { row: SerialRow }): string {
  return row.id === scannedId.value ? 'serial-row--hit' : '';
}

/** 拉列表 */
async function loadSerials(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchSerialPage(buildSerialQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
    scanHitOnPage.value = scannedId.value
      ? rows.value.some((row) => row.id === scannedId.value)
      : false;
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `序列号列表加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadSerials(1);
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptySerialFilter());
  current.value = 1;
  void loadSerials(1);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number }): void {
  current.value = pageInfo.current;
  void loadSerials(pageInfo.current);
}

// ==================== 状态流转（不可逆，二次确认） ====================

/** 出库 / 报废：都不可逆，先确认再打接口 */
function confirmTransition(row: SerialRow, target: Exclude<SerialStatus, 'IN_STOCK'>): void {
  if (!availableActions(row.status)[target === 'OUTBOUND' ? 'outbound' : 'scrap']) {
    MessagePlugin.warning(serialActionHint(row.status));
    return;
  }
  // 动作名（出库/报废）与目标状态名（已出库/已报废）不是一回事，别混用：
  // 标题要「出库确认」，正文里才说「将变更为『已出库』」。
  const action = target === 'OUTBOUND' ? '出库' : '报废';
  const label = serialStatusLabel(target);
  // 正文是纯文本（TDesign 的 dialog body 不渲染 markdown，别写 **加粗**）
  const consequence =
    target === 'OUTBOUND'
      ? '出库后不能再出库，只能报废。'
      : '报废是终态，之后没有任何可执行的流转。';
  const dialog = DialogPlugin.confirm({
    header: `${action}确认`,
    theme: target === 'SCRAPPED' ? 'danger' : 'warning',
    body:
      `序列号 ${row.serial_no}（当前：${serialStatusLabel(row.status)}）将变更为「${label}」。`
      + `状态流转不可逆：${consequence}确认继续？`,
    confirmBtn: { content: `确认${action}`, theme: target === 'SCRAPPED' ? 'danger' : 'primary' },
    onConfirm: async () => {
      try {
        await updateSerialStatus(row.id, { status: target });
        MessagePlugin.success(`${row.serial_no} 已${action}`);
        dialog.hide();
        await loadSerials(current.value);
        // 状态变了，扫码信息卡里的状态也得跟着变（重新点查一次，别让卡片说谎）
        if (scanResult.value?.id === row.id) {
          await handleScan();
        }
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 批量生成 ====================

/** 弹窗可见 */
const batchVisible = ref(false);
/** 表单 */
const batchForm = reactive<BatchGenerateFormState>(emptyBatchForm());
/** 字段错误 */
const batchErrors = ref<BatchFormErrors>({});
/** 提交错误（后端 400 的 msg 原样透出，不改写） */
const batchSubmitError = ref('');
/** 提交中 */
const batchSaving = ref(false);
/** 上一次成功的业务摘要（列表上方常驻一条，刷新/关提示后消失） */
const batchResultText = ref('');

/** 入库金额预览（`数量 × 单价`，4 位小数；非法输入显示 `—`） */
const batchPreviewAmount = computed(() => formatBatchAmount(batchForm));

/** 打开弹窗（表单重置，避免上一次的残留） */
function openBatchDialog(): void {
  Object.assign(batchForm, emptyBatchForm());
  batchErrors.value = {};
  batchSubmitError.value = '';
  batchVisible.value = true;
}

/** 提交：先本地校验，再 POST；成功提示「生成了 N 个序列号 + 本次入库金额」并刷新列表 */
async function submitBatch(): Promise<void> {
  const errors = validateBatchForm(batchForm);
  batchErrors.value = errors;
  if (hasBatchFormError(errors)) {
    return;
  }
  batchSaving.value = true;
  batchSubmitError.value = '';
  try {
    const result = await batchGenerateSerials(buildBatchPayload(batchForm));
    // 摘要里直接用后端回显的字符串（amount/unit_cost/qty），不在前端重算金额
    batchResultText.value = formatBatchResultText(result);
    MessagePlugin.success(batchResultText.value);
    batchVisible.value = false;
    current.value = 1;
    await loadSerials(1);
  } catch (error) {
    // 400（code 10001）的 msg 已写清原因（缺单价 / ≤0 / 非数字），直出；只有拿不到 msg 才兜底
    batchSubmitError.value = batchGenerateErrorText(error);
  } finally {
    batchSaving.value = false;
  }
}

// ==================== 初始化 ====================

onMounted(async () => {
  focusScanInput();
  // 主数据拉不到不挡列表（只是名称列退化成长 id）
  const [materialList, warehouseList] = await Promise.all([
    fetchMaterialPage({ page: 1, page_size: 200 }).catch(() => ({ list: [] as MaterialRow[] })),
    fetchWarehousePage({ page: 1, page_size: 200 }).catch(() => ({ list: [] as Warehouse[] })),
  ]);
  materials.value = materialList.list ?? [];
  warehouses.value = warehouseList.list ?? [];
  await loadSerials(1);
});
</script>

<style scoped lang="less">
.serial-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 扫码行 ==================== */

.scan-card {
  flex: 0 0 auto;
}

.scan-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.scan-row__label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.scan-row__icon {
  font-size: 18px;
  color: var(--td-brand-color);
}

/* 扫码框是整页最该被看见的东西：加宽 + 大字号（条码枪输错一位的概率和字号无关，但人眼核对需要） */
.scan-row__input {
  width: 380px;
  font-size: 15px;
}

.scan-row__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.scan-error {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 10px 0 0;
  font-size: 13px;
  color: var(--td-error-color);
}

/* ==================== 命中信息卡 ==================== */

.hit-card {
  margin-top: 12px;
  padding: 12px 14px;
  border-radius: var(--td-radius-medium);
  background: var(--td-bg-color-container-hover);
  border-left: 3px solid var(--td-brand-color);
}

.hit-card__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
}

.hit-card__no {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.hit-card__grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(180px, 1fr));
  gap: 8px 16px;
}

.hit-field {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
  font-size: 13px;
}

.hit-field__label {
  flex: 0 0 auto;
  color: var(--td-text-color-placeholder);
}

.hit-field__value {
  overflow: hidden;
  color: var(--td-text-color-primary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ==================== 筛选 / 列表 ==================== */

.filter-card {
  flex: 0 0 auto;
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.filter-row__select {
  width: 190px;
}

.filter-row__select--narrow {
  width: 130px;
}

.filter-row__no {
  width: 220px;
}

.filter-row__spacer {
  flex: 1 1 auto;
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

.panel-head__warn {
  font-size: 12px;
  color: var(--td-warning-color);
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

/* 扫码命中的行：品牌色淡底 + 左侧色条，扫一眼能定位（不写死 hex，跟色板走）
 *
 * ⚠️ 必须落在 `td` 上：TDesign 表格把背景色画在单元格（`td`）上，给 `tr` 设
 * `background-color` / `box-shadow` 会被 td 的实色盖掉（实测 tr 底色 246,247,247 ≈ 普通行，
 * 等于没高亮）。 */
:deep(.serial-row--hit) td {
  background-color: var(--td-brand-color-light);
}

:deep(.serial-row--hit) td:first-child {
  box-shadow: inset 3px 0 0 0 var(--td-brand-color);
}

.hit-flag {
  margin-left: 8px;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 10px;
}

.op-disabled {
  opacity: 0.45;
  cursor: not-allowed;
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

.dlg-alert {
  margin-bottom: 12px;
}

.dlg-help {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* 入库金额预览：数字比说明文字重一档，扫一眼就能核对（颜色仍走 TDesign 语义变量，不写裸 hex） */
.dlg-amount {
  display: flex;
  align-items: baseline;
  gap: 8px;
  flex-wrap: wrap;
}

.dlg-amount__value {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}
</style>
