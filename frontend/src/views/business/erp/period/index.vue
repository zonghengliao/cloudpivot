<template>
  <div class="period-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-period-refresh" @click="loadPeriods()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <!--
          ★ ERP-P1：开账 / 批量开账 / 行内结账与反结账**按权限码渲染**。
          无权限 = 不渲染（不是 disabled）：这三个动作本来就不该出现在没权限的人眼前。
        -->
        <template v-if="canOpen">
          <t-button theme="primary" data-testid="btn-period-open" @click="openOpenDialog">
            <template #icon><t-icon name="add" /></template>
            开账
          </t-button>
          <t-button variant="outline" data-testid="btn-period-batch-open" @click="openBatchDialog">
            批量开账
          </t-button>
        </template>

        <span class="tool-row__spacer" />

        <t-select
          v-model="yearModel"
          class="tool-row__year"
          :options="yearOptions"
          placeholder="全部年份"
          clearable
          data-testid="period-filter-year"
          @change="handleSearch"
        />
        <t-select
          v-model="statusModel"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="period-filter-status"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-period-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-period-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 期间列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">会计期间</span>
          <span class="panel-head__meta" data-testid="period-meta">共 {{ total }} 个期间</span>
        </div>
        <span class="panel-head__tip" data-testid="period-action-tip">
          {{ actionTip }}
        </span>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="会计期间接口尚未上线"
        class="list-alert"
        data-testid="period-api-missing"
      >
        <template #description>
          <span class="mono">GET /api/business/erp/period/page</span> 返回 404。接口上线后本页无需改动。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="period-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadPeriods()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        v-if="rows.length > 0 || loading"
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :row-class-name="rowClassName"
        hover
        data-testid="period-table"
        @page-change="handlePageChange"
      >
        <template #period_code="{ row }">
          <span class="mono period-code" :data-testid="`period-code-${row.period_code}`">
            {{ row.period_code }}
          </span>
          <t-tag
            v-if="isCurrentPeriodRow(row)"
            size="small"
            variant="light"
            theme="primary"
            class="current-tag"
            data-testid="period-current-mark"
          >
            当月
          </t-tag>
        </template>

        <template #range="{ row }">
          <span class="muted" :data-testid="`period-range-${row.period_code}`">{{ periodRangeText(row) }}</span>
          <span class="muted days">（{{ periodDays(row) }} 天）</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="periodStatusTheme(row.status)"
            :data-testid="`period-status-${row.period_code}`"
          >
            {{ periodStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #closed_at="{ row }">
          <span class="muted" :data-testid="`period-closed-${row.period_code}`">{{ closedAtText(row) }}</span>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link
              v-if="rowAction(row) === 'open' && canOpen"
              theme="primary"
              :data-testid="`btn-period-open-${row.period_code}`"
              @click="runOpenOne(row)"
            >
              开账
            </t-link>
            <t-link
              v-if="rowAction(row) === 'close' && canClose"
              theme="danger"
              :data-testid="`btn-period-close-${row.period_code}`"
              @click="openCloseDialog(row)"
            >
              结账
            </t-link>
            <t-link
              v-if="rowAction(row) === 'reopen' && canReopen"
              theme="warning"
              :data-testid="`btn-period-reopen-${row.period_code}`"
              @click="openReopenDialog(row)"
            >
              反结账
            </t-link>
            <span v-if="!hasRowAction(row)" class="muted">—</span>
          </div>
        </template>
      </t-table>

      <div v-else class="panel-empty" data-testid="period-empty">
        <t-icon name="calendar" class="panel-empty__icon" />
        <p class="panel-empty__title">{{ filtered ? '没有匹配的会计期间' : '暂无会计期间' }}</p>
        <p class="panel-empty__desc">
          {{ filtered ? '换个年份或状态再试' : emptyHint }}
        </p>
      </div>
    </t-card>

    <!-- ==================== ③ 开账弹窗 ==================== -->
    <t-dialog
      v-model:visible="openDialog.visible"
      header="开账（新建会计期间）"
      :confirm-btn="{ content: '开账', loading: openDialog.saving }"
      :cancel-btn="{ content: '取消' }"
      data-testid="period-open-dialog"
      @confirm="submitOpen"
    >
      <div class="form-grid">
        <div class="form-row">
          <label class="form-label">会计年度</label>
          <t-input-number
            v-model="openDialog.year"
            theme="normal"
            :min="2000"
            :max="2999"
            :step="1"
            data-testid="period-open-year"
          />
        </div>
        <div class="form-row">
          <label class="form-label">会计月份</label>
          <t-select
            v-model="openDialog.month"
            :options="monthOptions"
            data-testid="period-open-month"
          />
        </div>
        <div class="form-row">
          <label class="form-label">备注（可选）</label>
          <t-input v-model="openDialog.remark" placeholder="如：2026 年 11 月" data-testid="period-open-remark" />
        </div>
      </div>
      <p class="form-hint">
        起止日由后端按自然月自动算（<span class="mono">{{ openDialogPreview }}</span>）；期间建好后状态是「已开启」。
      </p>
    </t-dialog>

    <!-- ==================== ④ 批量开账弹窗 ==================== -->
    <t-dialog
      v-model:visible="batchDialog.visible"
      header="批量开账（一次开多个月）"
      :confirm-btn="{ content: '批量开账', loading: batchDialog.saving }"
      :cancel-btn="{ content: '取消' }"
      data-testid="period-batch-dialog"
      @confirm="submitBatch"
    >
      <div class="form-grid">
        <div class="form-row">
          <label class="form-label">起始期间</label>
          <t-input v-model="batchDialog.from" placeholder="2026-11" data-testid="period-batch-from" />
        </div>
        <div class="form-row">
          <label class="form-label">结束期间</label>
          <t-input v-model="batchDialog.to" placeholder="2027-12" data-testid="period-batch-to" />
        </div>
      </div>
      <p class="form-hint">
        格式 <span class="mono">YYYY-MM</span>，含两端；已存在的期间自动跳过（幂等），一次最多 120 个月。
      </p>
    </t-dialog>

    <!-- ==================== ⑤ 结账弹窗（先预检） ==================== -->
    <t-dialog
      v-model:visible="closeDialog.visible"
      :header="`结账 ${closeDialog.code}`"
      :confirm-btn="{
        content: '确认结账',
        theme: 'danger',
        loading: closeDialog.saving,
        disabled: !closeDialog.check?.can_close,
      }"
      :cancel-btn="{ content: '取消' }"
      width="620px"
      data-testid="period-close-dialog"
      @confirm="submitClose"
    >
      <div v-if="closeDialog.loadingCheck" class="check-loading" data-testid="period-close-check-loading">
        正在做结账预检…
      </div>
      <template v-else>
        <t-alert
          v-if="closeDialog.check?.can_close"
          theme="success"
          :message="closeCheckSummary(closeDialog.check ?? emptyCheck)"
          data-testid="period-close-check-ok"
        />
        <t-alert
          v-else
          theme="warning"
          :message="closeCheckSummary(closeDialog.check ?? emptyCheck)"
          data-testid="period-close-check-blocked"
        >
          <template #description>
            <ul class="blocker-list" data-testid="period-close-blockers">
              <li v-for="(blocker, index) in closeDialog.check?.blockers ?? []" :key="`${blocker.type}-${index}`">
                <span class="blocker-type mono">{{ blocker.type }}</span>
                <span>{{ blocker.message }}</span>
                <a v-if="blocker.link" class="blocker-link" :href="blocker.link" target="_blank" rel="noopener">
                  去处理
                </a>
                <span v-if="blocker.samples && blocker.samples.length" class="blocker-samples mono">
                  （{{ blocker.samples.join(' / ') }}）
                </span>
              </li>
            </ul>
          </template>
        </t-alert>

        <div class="form-row close-reason-row">
          <label class="form-label">结账备注（可选）</label>
          <t-input
            v-model="closeDialog.reason"
            placeholder="如：2026 年 10 月结账"
            data-testid="period-close-reason"
          />
        </div>
        <p class="form-hint">
          结账后本期间只读：不能再生成凭证、单据不能过账、红冲也会被拒。如需再动账，只能「反结账」（要填原因）。
        </p>
      </template>
    </t-dialog>

    <!-- ==================== ⑥ 反结账弹窗 ==================== -->
    <t-dialog
      v-model:visible="reopenDialog.visible"
      :header="`反结账 ${reopenDialog.code}`"
      :confirm-btn="{ content: '确认反结账', theme: 'warning', loading: reopenDialog.saving }"
      :cancel-btn="{ content: '取消' }"
      width="560px"
      data-testid="period-reopen-dialog"
      @confirm="submitReopen"
    >
      <t-alert
        theme="warning"
        message="反结账会让已封账的月份重新可写：谁都能在这个月补凭证 / 改账。"
        class="list-alert"
      >
        <template #description>
          原因会写进状态日志（审计证据），至少 {{ MIN_REOPEN_REASON_CHARS }} 个字。
        </template>
      </t-alert>
      <div class="form-row">
        <label class="form-label">反结账原因（必填）</label>
        <t-textarea
          v-model="reopenDialog.reason"
          :maxlength="200"
          placeholder="如：2026 年 10 月漏记一笔运费，需要补一张凭证"
          data-testid="period-reopen-reason"
        />
      </div>
      <p v-if="reopenDialog.error" class="form-error" data-testid="period-reopen-error">
        {{ reopenDialog.error }}
      </p>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 会计期间页（P4-C 列表 + **ERP-P1 开账 / 结账 / 反结账**）。
 *
 * # 这一版改了什么
 *
 * P4-C 把本页做成**只读**并留了一句「只读：开账 / 结账由财务主管操作」——
 * 设计意图对（防误操作），但**没有提供那个入口**，于是老板问「会计期间为什么不能
 * 添加新的期间」。ERP-P1 补的就是执行者要用的按钮，并把权限收在三个独立权限码上。
 *
 * # 权限（卡面硬红线 5）
 *
 * | 动作 | 权限码 | 无权限时 |
 * |------|--------|----------|
 * | 开账 / 批量开账 | `business:erp:period:open` | **不渲染**按钮 |
 * | 结账 | `business:erp:period:close` | **不渲染**行内「结账」 |
 * | 反结账 | `business:erp:period:reopen` | **不渲染**行内「反结账」 |
 *
 * 用 `v-if` 而不是 `:disabled`：置灰会在没权限的人页面上永久留一个点不动的按钮。
 *
 * # 结账是两步（卡面 O5 / O8）
 *
 * 点「结账」→ 先 `GET /period/{id}/close-check` 把阻塞项摆出来
 * （「还有 3 张草稿凭证未过账」「借贷不平，差 20 元」）→ 全绿才允许点「确认结账」。
 * 高危动作二次确认 + 可选备注，反结账另有**必填原因（≥ 10 字）**。
 *
 * # 字段来源（以库列 / DTO 为准）
 *
 * `biz_accounting_period`：`period_year / period_month / period_code(VARCHAR 7) /
 * start_date / end_date / status(NOT_OPENED|OPEN|CLOSED) / closed_at / closed_by`；
 * 出参 `dto.rs::PeriodRes` 同名 + `id / remark / created_at / updated_at`。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  batchOpenPeriods,
  closePeriod,
  fetchPeriodCloseCheck,
  fetchPeriodPage,
  openPeriod,
  reopenPeriod,
  type AccountPeriodRow,
  type PeriodCloseCheck,
} from '@/api/business/erp';
import { useUserStore } from '@/stores/user';
import {
  MIN_REOPEN_REASON_CHARS,
  PERIOD_ACTION_PERMISSIONS,
  PERIOD_STATUS_OPTIONS,
  batchOpenError,
  buildPeriodQuery,
  canRunPeriodAction,
  closeCheckSummary,
  closeReasonError,
  closedAtText,
  emptyPeriodFilter,
  isCurrentPeriod,
  isPeriodFiltered,
  periodActionOf,
  periodDays,
  periodRangeText,
  periodStatusLabel,
  periodStatusTheme,
  periodYearOptions,
  reopenReasonError,
  type PeriodAction,
  type PeriodFilterState,
} from './period-logic';

/** 每页条数（一年 12 条 → 一页看不完也不至于翻页太频繁，取 20） */
const PAGE_SIZE = 20;

/** 表格列（`op` 是本页唯一非后端列） */
const columns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 160 },
  { colKey: 'range', title: '起止日期', minWidth: 240 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'closed_at', title: '结账时间', width: 190 },
  { colKey: 'op', title: '操作', width: 120 },
];

const statusOptions = PERIOD_STATUS_OPTIONS.map((item) => ({ ...item }));

/** 月份下拉 1–12 */
const monthOptions = Array.from({ length: 12 }, (_, index) => ({
  value: index + 1,
  label: `${index + 1} 月`,
}));

const filter = reactive<PeriodFilterState>(emptyPeriodFilter());

/**
 * `t-select` 的 `v-model` 不收 `null`，而纯逻辑里「全部」用 `null` / `''`。
 * 两个双向 computed 做桥：清空 → `null` / `''`，选中 → 原值。
 */
const yearModel = computed<number | string | undefined>({
  get: () => (filter.period_year === null ? undefined : filter.period_year),
  set: (value) => {
    filter.period_year = typeof value === 'number' ? value : null;
  },
});

/** 状态下拉的桥（同上：`''` = 全部） */
const statusModel = computed<string | undefined>({
  get: () => filter.status || undefined,
  set: (value) => {
    filter.status = (typeof value === 'string' ? value : '') as PeriodFilterState['status'];
  },
});

const rows = ref<AccountPeriodRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

/** 年份下拉：数据里出现过的年份 + 当前年，倒序 */
const yearOptions = computed(() =>
  periodYearOptions(rows.value).map((year) => ({ label: `${year} 年`, value: year })),
);

const filtered = computed(() => isPeriodFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// ==================== 权限 ====================

const userStore = useUserStore();
/** 有开账权限（开账 / 批量开账按钮） */
const canOpen = computed(() => userStore.hasPermission(PERIOD_ACTION_PERMISSIONS.open));
/** 有结账权限（行内「结账」） */
const canClose = computed(() => userStore.hasPermission(PERIOD_ACTION_PERMISSIONS.close));
/** 有反结账权限（行内「反结账」） */
const canReopen = computed(() => userStore.hasPermission(PERIOD_ACTION_PERMISSIONS.reopen));

/** 预检结果为空时的占位（模板里 `?? emptyCheck`，避免把 `null` 传进纯函数） */
const emptyCheck = { can_close: false, blockers: [] as PeriodCloseCheck['blockers'] };

/** 页头那句提示：按当前账号的权限说清楚「能做什么」 */
const actionTip = computed(() => {
  const actions: string[] = [];
  if (canOpen.value) actions.push('开账');
  if (canClose.value) actions.push('结账');
  if (canReopen.value) actions.push('反结账');
  if (actions.length === 0) {
    return '只读：开账 / 结账 / 反结账需要财务权限';
  }
  return `当前账号可操作：${actions.join(' / ')}`;
});

/** 空态提示：有开账权限就提示点「开账」，没有就说去找财务 */
const emptyHint = computed(() =>
  canOpen.value ? '还没有期间：点左上角「开账」建第一个月，或用「批量开账」一次开一年' : '期间由财务主管按月开账后才有数据',
);

// ==================== 弹窗状态 ====================

/** 开账弹窗 */
const openDialog = reactive({
  visible: false,
  saving: false,
  year: new Date().getFullYear(),
  month: new Date().getMonth() + 1,
  remark: '',
});

/** 批量开账弹窗 */
const batchDialog = reactive({
  visible: false,
  saving: false,
  from: '',
  to: '',
});

/** 结账弹窗（含预检结果） */
const closeDialog = reactive({
  visible: false,
  saving: false,
  loadingCheck: false,
  id: '',
  code: '',
  reason: '',
  check: null as PeriodCloseCheck | null,
});

/** 反结账弹窗 */
const reopenDialog = reactive({
  visible: false,
  saving: false,
  id: '',
  code: '',
  reason: '',
  error: '',
});

/** 开账弹窗里那句「起止日大概是多少」的预览（真实值以后端返回为准） */
const openDialogPreview = computed(() => {
  const year = openDialog.year;
  const month = openDialog.month;
  if (!Number.isFinite(year) || !Number.isFinite(month)) {
    return '—';
  }
  const last = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return `${year}-${String(month).padStart(2, '0')}-01 ~ ${year}-${String(month).padStart(2, '0')}-${last}`;
});

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

/** 当前会计期间（页面高亮 + 「当月」标记） */
function isCurrentPeriodRow(row: AccountPeriodRow): boolean {
  return isCurrentPeriod(row);
}

/** 当前期间行加浅底（让「这个月」在长列表里一眼可见） */
function rowClassName({ row }: { row: AccountPeriodRow }): string {
  return isCurrentPeriodRow(row) ? 'period-row--current' : '';
}

/** 这一行该出哪个动作（纯逻辑，单测覆盖） */
function rowAction(row: AccountPeriodRow): PeriodAction | null {
  return periodActionOf(row.status);
}

/** 这一行是否**显示**了可点动作（无权限时行内只有「—」） */
function hasRowAction(row: AccountPeriodRow): boolean {
  return canRunPeriodAction(row.status, (code) => userStore.hasPermission(code));
}

/** 拉列表 */
async function loadPeriods(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchPeriodPage(buildPeriodQuery(filter, page, PAGE_SIZE));
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

/** 查询：条件变了回第 1 页（否则在第 3 页筛选出 1 条会看到空表） */
function handleSearch(): void {
  void loadPeriods(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptyPeriodFilter());
  void loadPeriods(1);
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadPeriods(pageInfo.current);
}

// ---------- 开账 ----------

/** 打开「开账」弹窗（默认填当前年月） */
function openOpenDialog(): void {
  const now = new Date();
  openDialog.year = now.getFullYear();
  openDialog.month = now.getMonth() + 1;
  openDialog.remark = '';
  openDialog.visible = true;
}

/** 提交开账 */
async function submitOpen(): Promise<void> {
  if (openDialog.saving) {
    return;
  }
  openDialog.saving = true;
  try {
    const row = await openPeriod({
      period_year: openDialog.year,
      period_month: openDialog.month,
      remark: openDialog.remark.trim() || undefined,
    });
    MessagePlugin.success(`会计期间 ${row.period_code} 已开账（${row.start_date} ~ ${row.end_date}）`);
    openDialog.visible = false;
    await loadPeriods(1);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    openDialog.saving = false;
  }
}

/**
 * 行内「开账」（状态为 `NOT_OPENED` 的历史期间）。
 *
 * 与弹窗开账走同一个接口：后端对 `NOT_OPENED` 的既有行是「置为 OPEN」而不是新建，
 * 所以这里不需要额外的「更新状态」接口。
 */
async function runOpenOne(row: AccountPeriodRow): Promise<void> {
  if (!canOpen.value) {
    return;
  }
  try {
    const updated = await openPeriod({ period_year: row.period_year, period_month: row.period_month });
    MessagePlugin.success(`会计期间 ${updated.period_code} 已开账`);
    await loadPeriods();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

// ---------- 批量开账 ----------

/** 打开「批量开账」弹窗（默认从下个月起开一年） */
function openBatchDialog(): void {
  const now = new Date();
  const fromYear = now.getFullYear();
  const fromMonth = now.getMonth() + 1;
  const toKey = fromYear * 12 + fromMonth + 11;
  const toYear = Math.floor((toKey - 1) / 12);
  const toMonth = ((toKey - 1) % 12) + 1;
  batchDialog.from = `${fromYear}-${String(fromMonth).padStart(2, '0')}`;
  batchDialog.to = `${toYear}-${String(toMonth).padStart(2, '0')}`;
  batchDialog.visible = true;
}

/** 提交批量开账 */
async function submitBatch(): Promise<void> {
  if (batchDialog.saving) {
    return;
  }
  const error = batchOpenError(batchDialog.from, batchDialog.to);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  batchDialog.saving = true;
  try {
    const result = await batchOpenPeriods({
      from: batchDialog.from.trim(),
      to: batchDialog.to.trim(),
    });
    const skipped = result.skipped_count > 0 ? `，跳过 ${result.skipped_count} 个已存在的期间` : '';
    MessagePlugin.success(`已开账 ${result.created_count} 个期间${skipped}`);
    batchDialog.visible = false;
    await loadPeriods(1);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    batchDialog.saving = false;
  }
}

// ---------- 结账 ----------

/** 打开结账弹窗：先预检，把阻塞项摆出来（卡面 O5） */
async function openCloseDialog(row: AccountPeriodRow): Promise<void> {
  if (!canClose.value) {
    return;
  }
  closeDialog.visible = true;
  closeDialog.id = row.id;
  closeDialog.code = row.period_code;
  closeDialog.reason = '';
  closeDialog.check = null;
  closeDialog.loadingCheck = true;
  try {
    closeDialog.check = await fetchPeriodCloseCheck(row.id);
  } catch (error) {
    MessagePlugin.error(`结账预检失败：${messageOf(error)}`);
    closeDialog.visible = false;
  } finally {
    closeDialog.loadingCheck = false;
  }
}

/** 提交结账（预检未通过时按钮是 disabled，不会走到这里） */
async function submitClose(): Promise<void> {
  if (closeDialog.saving || !closeDialog.check?.can_close) {
    return;
  }
  const reasonError = closeReasonError(closeDialog.reason);
  if (reasonError) {
    MessagePlugin.warning(reasonError);
    return;
  }
  closeDialog.saving = true;
  try {
    const row = await closePeriod(closeDialog.id, closeDialog.reason.trim() || undefined);
    MessagePlugin.success(`会计期间 ${row.period_code} 已结账`);
    closeDialog.visible = false;
    await loadPeriods();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    closeDialog.saving = false;
  }
}

// ---------- 反结账 ----------

/** 打开反结账弹窗（必填原因） */
function openReopenDialog(row: AccountPeriodRow): void {
  if (!canReopen.value) {
    return;
  }
  reopenDialog.id = row.id;
  reopenDialog.code = row.period_code;
  reopenDialog.reason = '';
  reopenDialog.error = '';
  reopenDialog.visible = true;
}

/** 提交反结账 */
async function submitReopen(): Promise<void> {
  if (reopenDialog.saving) {
    return;
  }
  const error = reopenReasonError(reopenDialog.reason);
  if (error) {
    reopenDialog.error = error;
    return;
  }
  reopenDialog.error = '';
  reopenDialog.saving = true;
  try {
    const row = await reopenPeriod(reopenDialog.id, reopenDialog.reason.trim());
    MessagePlugin.success(`会计期间 ${row.period_code} 已反结账，回到「已开启」`);
    reopenDialog.visible = false;
    await loadPeriods();
  } catch (error) {
    reopenDialog.error = messageOf(error);
    MessagePlugin.error(messageOf(error));
  } finally {
    reopenDialog.saving = false;
  }
}

onMounted(() => {
  void loadPeriods(1);
});
</script>

<style scoped lang="less">
.period-page {
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

.tool-row__year {
  width: 140px;
}

.tool-row__status {
  width: 140px;
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

.period-code {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.current-tag {
  margin-left: 6px;
}

.days {
  margin-left: 4px;
}

.op-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

/* 当月行：极浅主色底（只做「一眼找到」，不改变行的可读性） */
:deep(.period-row--current) {
  background-color: var(--td-bg-color-container-hover);
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

/* ==================== 弹窗 ==================== */

.form-grid {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.form-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.form-label {
  flex: 0 0 92px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.form-hint {
  margin: 12px 0 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.form-error {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
}

.check-loading {
  padding: 16px 0;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.blocker-list {
  margin: 8px 0 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.9;
}

.blocker-type {
  margin-right: 6px;
  color: var(--td-text-color-placeholder);
}

.blocker-link {
  margin-left: 8px;
  color: var(--td-brand-color);
}

.blocker-samples {
  margin-left: 6px;
  color: var(--td-text-color-placeholder);
}

.close-reason-row {
  margin-top: 14px;
}
</style>
