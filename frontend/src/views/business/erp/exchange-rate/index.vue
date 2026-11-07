<template>
  <div class="rate-page">
    <!-- ==================== 页面头 + 筛选 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="page-head">
        <div class="page-head__title">
          <span>汇率管理</span>
          <t-tag v-if="baseCode" size="small" variant="light" theme="success">
            目标币种 {{ baseCode }}
          </t-tag>
        </div>
        <span class="page-head__tip">
          方向固定 <b>1 原币 = 汇率 × 目标币种</b>（只存「外币 → 本位币」，反向由 1/汇率 推导，不落重复行）。
          取数口径：<code>effective_date ≤ 单据日</code> 取最近一条（含当日），
          <b>建单那一刻锁进单据行</b>，之后改汇率不影响已建单据（SO-40）。
          月末重估（SO-41）读的是 <code>MONTH_END</code> 类型。
        </span>
      </div>

      <div class="filter-row">
        <t-select
          v-model="filter.from_currency"
          class="filter-row__code"
          :options="currencyCodeOptions"
          placeholder="全部原币"
          clearable
          filterable
          data-testid="rate-filter-from"
        />
        <t-select
          v-model="filter.rate_type"
          class="filter-row__type"
          :options="rateTypeOptions"
          placeholder="全部类型"
          clearable
          data-testid="rate-filter-type"
        />
        <t-date-range-picker
          v-model="dateRange"
          class="filter-row__dates"
          value-type="YYYY-MM-DD"
          format="YYYY-MM-DD"
          placeholder="生效日区间（含两端）"
          clearable
          data-testid="rate-filter-dates"
          @change="handleDateChange"
        />
        <t-button theme="primary" data-testid="btn-rate-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-rate-reset" @click="handleReset">重置</t-button>
        <span class="filter-row__spacer" />
        <t-tooltip
          :content="
            canBatch
              ? '选一个月份 + 多个币种，一次性 upsert 到该月月末（同键已存在则覆盖）'
              : '当前账号没有 business:erp:fx-rate:batch 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="warning"
              variant="outline"
              data-testid="btn-rate-batch"
              :disabled="!canBatch"
              @click="openBatch"
            >
              <template #icon><t-icon name="calendar" /></template>
              按月批量录入
            </t-button>
          </span>
        </t-tooltip>
        <t-tooltip
          :content="
            canCreate
              ? '新增一条汇率（同币对 + 类型 + 生效日 只能一条）'
              : '当前账号没有 business:erp:fx-rate:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-rate-new"
              :disabled="!canCreate"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建汇率
            </t-button>
          </span>
        </t-tooltip>
      </div>

      <t-alert
        v-if="pageError"
        theme="error"
        :message="pageError"
        class="page-alert"
        data-testid="rate-error"
      />

      <t-table
        row-key="id"
        size="small"
        hover
        :data="rows"
        :columns="columns"
        :loading="loading"
        :empty="emptyText"
        data-testid="rate-table"
      >
        <template #pair="{ row }">
          <span class="mono pair-cell" data-testid="rate-pair">
            {{ row.from_currency }} → {{ row.to_currency }}
          </span>
        </template>
        <template #rate="{ row }">
          <span class="mono rate-cell" data-testid="rate-value">{{ rateText(row.rate) }}</span>
        </template>
        <template #rate_type="{ row }">
          <t-tag size="small" variant="light" :theme="row.rate_type === 'SPOT' ? 'primary' : 'warning'">
            {{ row.rate_type === 'SPOT' ? '即时' : '月末' }}
          </t-tag>
        </template>
        <template #effective_date="{ row }">
          <span class="mono">{{ row.effective_date }}</span>
        </template>
        <template #remark="{ row }">
          <span class="muted">{{ row.remark || '—' }}</span>
        </template>
        <template #op="{ row }">
          <div class="op-row">
            <t-link
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="btn-rate-edit"
              @click="openEdit(row as ExchangeRateRow)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="[PERM.remove]"
              theme="danger"
              data-testid="btn-rate-delete"
              @click="confirmDelete(row as ExchangeRateRow)"
            >
              删除
            </t-link>
          </div>
        </template>
      </t-table>

      <div class="pager-row">
        <span class="pager-row__total">共 {{ total }} 条</span>
        <t-pagination
          :current="page"
          :page-size="PAGE_SIZE"
          :total="total"
          :show-page-size="false"
          show-jumper
          data-testid="rate-pager"
          @current-change="handlePageChange"
        />
      </div>
    </t-card>

    <!-- ==================== 新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑汇率' : '新建汇率'"
      width="560px"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: submitting }"
      :close-on-overlay-click="false"
      data-testid="rate-form-dialog"
      @confirm="submitForm"
    >
      <t-alert v-if="formError" theme="error" :message="formError" class="page-alert" />
      <t-form label-width="104px" class="rate-form">
        <t-form-item label="原币" required-mark>
          <t-select
            v-model="form.from_currency"
            :options="fromCurrencyOptions"
            filterable
            placeholder="外币（如 USD）"
            data-testid="rate-form-from"
          />
          <template #help>只录「外币 → 本位币」；本位币自己不需要汇率（恒 1）</template>
        </t-form-item>
        <t-form-item label="目标币种">
          <t-input :model-value="form.to_currency || baseCode || '（账套本位币）'" disabled />
          <template #help>固定为本位币（后端缺省取账套本位币，不用手填）</template>
        </t-form-item>
        <t-form-item label="汇率" required-mark>
          <t-input-number
            v-model="form.rate"
            theme="normal"
            :min="0"
            :decimal-places="8"
            :step="0.01"
            placeholder="1 原币 = ? 本位币"
            data-testid="rate-form-rate"
          />
          <template #help>必须 &gt; 0，最多 8 位小数（0 会把整单折成 0 元）</template>
        </t-form-item>
        <t-form-item label="类型">
          <t-select
            v-model="form.rate_type"
            :options="rateTypeOptions"
            data-testid="rate-form-type"
          />
        </t-form-item>
        <t-form-item label="生效日" required-mark>
          <t-date-picker
            v-model="form.effective_date"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            :clearable="false"
            data-testid="rate-form-date"
          />
          <template #help>含当日；建单按「≤ 单据日的最近一条」取值</template>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="form.remark" placeholder="来源，如 中国银行 / 央行中间价" :maxlength="200" data-testid="rate-form-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== 按月批量录入 ==================== -->
    <t-dialog
      v-model:visible="batchVisible"
      header="按月批量录入汇率"
      width="720px"
      :confirm-btn="{ content: '批量写入', loading: batchSubmitting, disabled: batchRows.length === 0 }"
      :close-on-overlay-click="false"
      data-testid="rate-batch-dialog"
      @confirm="submitBatch"
    >
      <t-alert v-if="batchError" theme="error" :message="batchError" class="page-alert" />
      <div class="batch-head">
        <div class="batch-head__item">
          <span class="batch-head__label">期间</span>
          <t-date-picker
            v-model="batchForm.month"
            mode="month"
            value-type="YYYY-MM"
            format="YYYY-MM"
            :clearable="false"
            data-testid="rate-batch-month"
            @change="handleBatchMonthChange"
          />
        </div>
        <div class="batch-head__item">
          <span class="batch-head__label">类型</span>
          <t-select
            v-model="batchForm.rate_type"
            :options="rateTypeOptions"
            class="batch-head__select"
            data-testid="rate-batch-type"
            @change="handleBatchTypeChange"
          />
        </div>
        <div class="batch-head__item">
          <span class="batch-head__label">生效日</span>
          <span class="mono" data-testid="rate-batch-effective">{{ batchEffectiveDate }}</span>
          <span class="muted">（该月月末，自动算）</span>
        </div>
      </div>
      <div class="batch-head__item batch-head__item--wide">
        <span class="batch-head__label">备注</span>
        <t-input
          v-model="batchForm.remark"
          placeholder="来源，如 中国银行月末中间价（整批共用）"
          :maxlength="200"
          data-testid="rate-batch-remark"
        />
      </div>

      <t-alert
        v-if="batchLoading"
        theme="info"
        message="正在读取该月已录汇率…"
        class="page-alert"
      />

      <t-table
        row-key="code"
        size="small"
        :data="batchRows"
        :columns="batchColumns"
        data-testid="rate-batch-table"
      >
        <template #code="{ row }">
          <span class="mono">{{ row.code }}</span>
        </template>
        <template #label="{ row }">
          <span>{{ row.label }}</span>
        </template>
        <template #existing="{ row }">
          <span v-if="row.existing === null" class="muted">未录</span>
          <span v-else class="mono muted">{{ rateText(row.existing) }}</span>
        </template>
        <template #rate="{ row }">
          <t-input-number
            v-model="row.rate"
            theme="normal"
            :min="0"
            :decimal-places="8"
            :step="0.01"
            placeholder="留空 = 跳过"
            :data-testid="`rate-batch-input-${row.code}`"
          />
        </template>
      </t-table>
      <div class="batch-tip">
        留空的币种不写库；已录过的币种会<b>覆盖</b>该月同类型同生效日的汇率（upsert）。
        本位币不出现在列表里（自反汇率恒 1，库层会拒）。
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 汇率管理页（销售链 · SO-42 / ARCH-7 §15.6.3 ②）
 *
 * # 这张表的口径（写错了会计上直接错账）
 *
 * | 事实 | 后果 |
 * |------|------|
 * | 方向 `1 from = rate to`，只存「外币 → 本位币」 | 反向要 1/rate，不落重复行（两行互为倒数必然对不上） |
 * | `rate > 0` | 0 会把整单折成 0 元，负数造出反向应收 |
 * | `rate_type`：`SPOT` 建单锁定 / `MONTH_END` 期末重估 | 月末重估（SO-41）读 MONTH_END，别录错类型 |
 * | `effective_date` **必填、无默认** | 补录历史汇率不许静默按今天生效 |
 * | 唯一键 `(原币, 目标币, 类型, 生效日)` | 同键只能一条；「按月批量录入」走 upsert 覆盖 |
 * | **物理删**（表无软删列） | 删了就是删了，`quote` 立刻取不到 → 外币单建单 400 |
 *
 * # 按月批量录入为什么是 upsert 而不是纯插入
 *
 * 每月重录同一批币种是常态（牌价每月更新），纯插入会让用户每次先删一遍；
 * upsert 命中唯一索引后**改值不改行**，月份 + 币种 + 类型 就是天然的自然键。
 *
 * # 权限
 *
 * 五个码各挡一个动作；`v-privilege` 只隐藏按钮，接口侧后端会再判一次。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError, type PageData } from '@/api/request';
import {
  RATE_TYPE_OPTIONS,
  batchMonthRates,
  createRate,
  deleteRate,
  fetchCurrencyOptions,
  fetchRatePage,
  updateRate,
  type CurrencyOption,
  type ExchangeRateFilter,
  type ExchangeRateRow,
  type RateType,
} from '@/api/business/erp/currency';

/** 本页权限码（与 `V0316__currency_menu.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:fx-rate:list',
  create: 'business:erp:fx-rate:create',
  update: 'business:erp:fx-rate:update',
  remove: 'business:erp:fx-rate:delete',
  batch: 'business:erp:fx-rate:batch',
  quote: 'business:erp:fx-rate:quote',
} as const;

const PAGE_SIZE = 20;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canBatch = computed(() => userStore.hasPermission(PERM.batch));

const rateTypeOptions = RATE_TYPE_OPTIONS;

const columns = [
  { colKey: 'pair', title: '币种对', width: 180 },
  { colKey: 'rate', title: '汇率', width: 160 },
  { colKey: 'rate_type', title: '类型', width: 100 },
  { colKey: 'effective_date', title: '生效日期', width: 130 },
  { colKey: 'remark', title: '备注', ellipsis: true },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' as const },
];

/** 错误 → 人话（ApiError 带后端 msg） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 北京时间今天（与后端 `today_cst()` 同口径） */
function todayCst(): string {
  return new Date(Date.now() + 8 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

/** 汇率展示：`7.1` → `7.10000000`（8 位，与 `NUMERIC(18,8)` 同精度） */
function rateText(raw: string | number | null | undefined): string {
  if (raw === null || raw === undefined || raw === '') {
    return '—';
  }
  const value = typeof raw === 'number' ? raw : Number(raw);
  if (!Number.isFinite(value)) {
    return String(raw);
  }
  return value.toFixed(8);
}

/** `YYYY-MM-DD` + 任意天数（用 UTC 计算，避开本地时区） */
function shiftMonthEnd(month: string): string {
  const [yearText, monthText] = month.split('-');
  const year = Number(yearText);
  const monthNumber = Number(monthText);
  if (!Number.isFinite(year) || !Number.isFinite(monthNumber)) {
    return '';
  }
  // 下个月 1 号减 1 天 = 本月月末（闰年自动正确）
  const next = monthNumber === 12 ? Date.UTC(year + 1, 0, 1) : Date.UTC(year, monthNumber, 1);
  return new Date(next - 86_400_000).toISOString().slice(0, 10);
}

// ==================== 币种主数据（下拉 / 目标币种 / 批量行） ====================

const currencies = ref<CurrencyOption[]>([]);
const baseCode = computed(() => currencies.value.find((item) => item.is_base)?.code ?? '');
/** 原币下拉：排除本位币（自反汇率恒 1，库层会拒） */
const fromCurrencyOptions = computed(() =>
  currencies.value
    .filter((item) => !item.is_base)
    .map((item) => ({ label: item.label, value: item.code })),
);
const currencyCodeOptions = computed(() =>
  currencies.value.map((item) => ({ label: item.label, value: item.code })),
);

async function loadCurrencies(): Promise<void> {
  try {
    currencies.value = await fetchCurrencyOptions();
  } catch (error) {
    // 币种读不到不该让汇率列表整体失败（列表本身仍可用，只是下拉空）
    currencies.value = [];
    pageError.value = `币种下拉加载失败：${messageOf(error)}`;
  }
}

// ==================== 列表 ====================

const filter = reactive<ExchangeRateFilter>({
  from_currency: undefined,
  rate_type: undefined,
  date_from: undefined,
  date_to: undefined,
});
// ⚠️ 初值必须是 `[]` 而不是 `undefined`：`t-date-range-picker` 的 `value` 校验要求 Array，
// 传 undefined 会在控制台刷 `typeof value: undefined must be Array!`（功能不坏但脏）
const dateRange = ref<Array<string | number>>([]);
const rows = ref<ExchangeRateRow[]>([]);
const total = ref(0);
const page = ref(1);
const loading = ref(false);
const pageError = ref('');

const emptyText = computed(() => {
  if (loading.value) {
    return '加载中…';
  }
  if (filter.from_currency || filter.rate_type || filter.date_from) {
    return '没有符合条件的汇率（换个币种 / 类型 / 日期试试，或点「重置」）';
  }
  return '还没有汇率 —— 点右上角「按月批量录入」一次录一批，或「新建汇率」单条录入';
});

function handleDateChange(value: unknown): void {
  // `value-type="YYYY-MM-DD"` ⇒ 实际给的是 `[string, string]`；类型上 TDesign 声明得比这宽
  const pair = Array.isArray(value) ? value : [];
  if (pair.length !== 2 || !pair[0] || !pair[1]) {
    filter.date_from = undefined;
    filter.date_to = undefined;
    return;
  }
  filter.date_from = String(pair[0]);
  filter.date_to = String(pair[1]);
}

async function loadRows(target: number = page.value): Promise<void> {
  loading.value = true;
  pageError.value = '';
  try {
    const data: PageData<ExchangeRateRow> = await fetchRatePage({
      ...filter,
      page: target,
      page_size: PAGE_SIZE,
    });
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    page.value = data.page ?? target;
    const lastPage = Math.max(1, Math.ceil(total.value / PAGE_SIZE));
    if (page.value > lastPage) {
      page.value = lastPage;
      await loadRows(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    pageError.value = `汇率加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  page.value = 1;
  void loadRows(1);
}

function handleReset(): void {
  filter.from_currency = undefined;
  filter.rate_type = undefined;
  filter.date_from = undefined;
  filter.date_to = undefined;
  dateRange.value = [];
  page.value = 1;
  void loadRows(1);
}

function handlePageChange(current: number): void {
  page.value = current;
  void loadRows(current);
}

// ==================== 新建 / 编辑 ====================

interface RateFormDraft {
  from_currency: string;
  to_currency: string;
  rate: number | undefined;
  rate_type: RateType;
  effective_date: string;
  remark: string;
}

const formVisible = ref(false);
const formError = ref('');
const submitting = ref(false);
const editingId = ref('');
const form = reactive<RateFormDraft>(emptyRateForm());

function emptyRateForm(): RateFormDraft {
  return {
    from_currency: '',
    to_currency: '',
    rate: undefined,
    rate_type: 'SPOT',
    effective_date: todayCst(),
    remark: '',
  };
}

function openCreate(): void {
  editingId.value = '';
  formError.value = '';
  Object.assign(form, emptyRateForm(), { to_currency: baseCode.value });
  formVisible.value = true;
}

function openEdit(row: ExchangeRateRow): void {
  editingId.value = row.id;
  formError.value = '';
  Object.assign(form, {
    from_currency: row.from_currency,
    to_currency: row.to_currency,
    rate: Number(row.rate),
    rate_type: row.rate_type,
    effective_date: row.effective_date,
    remark: row.remark ?? '',
  });
  formVisible.value = true;
}

function validateForm(): string {
  if (!form.from_currency) {
    return '请选择原币';
  }
  if (form.from_currency === (form.to_currency || baseCode.value)) {
    return '币种对不能自反（原币不能等于目标币种/本位币）';
  }
  if (form.rate === undefined || form.rate === null || Number.isNaN(Number(form.rate))) {
    return '请填写汇率';
  }
  if (Number(form.rate) <= 0) {
    return '汇率必须大于 0（0 会把整单折成 0 元）';
  }
  if (!form.effective_date) {
    return '请选择生效日（该列无默认值：补录历史汇率必须显式给）';
  }
  return '';
}

async function submitForm(): Promise<void> {
  const invalid = validateForm();
  if (invalid) {
    formError.value = invalid;
    return;
  }
  submitting.value = true;
  formError.value = '';
  try {
    if (editingId.value) {
      await updateRate(editingId.value, {
        from_currency: form.from_currency,
        to_currency: form.to_currency || baseCode.value,
        rate: String(form.rate),
        rate_type: form.rate_type,
        effective_date: form.effective_date,
        remark: form.remark.trim(),
      });
      MessagePlugin.success('汇率已保存');
    } else {
      await createRate({
        from_currency: form.from_currency,
        to_currency: form.to_currency || undefined,
        rate: String(form.rate),
        rate_type: form.rate_type,
        effective_date: form.effective_date,
        remark: form.remark.trim() || null,
      });
      MessagePlugin.success('汇率已创建');
    }
    formVisible.value = false;
    await loadRows();
  } catch (error) {
    // 后端原文（同键重复 409 / 汇率非法 / 币种不存在）直接摆到弹窗里
    formError.value = messageOf(error);
  } finally {
    submitting.value = false;
  }
}

function confirmDelete(row: ExchangeRateRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除汇率',
    body: `确认删除 ${row.from_currency} → ${row.to_currency}（${row.rate_type}，生效日 ${row.effective_date}）这条汇率？\n\n汇率是物理删除、不可恢复；删掉后该币种在此时段内如果取不到别的即时汇率，新建外币单据会被拒绝。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteRate(row.id);
        MessagePlugin.success('已删除');
        await loadRows();
      } catch (error) {
        MessagePlugin.error(`删除失败：${messageOf(error)}`);
      } finally {
        dialog.hide();
      }
    },
  });
}

// ==================== 按月批量录入 ====================

interface BatchRowDraft {
  code: string;
  label: string;
  /** 该月已录的汇率（`null` = 未录） */
  existing: string | null;
  /** 待写入的汇率（`undefined` / `null` = 跳过） */
  rate: number | undefined;
}

const batchVisible = ref(false);
const batchError = ref('');
const batchSubmitting = ref(false);
const batchLoading = ref(false);
const batchRows = ref<BatchRowDraft[]>([]);
const batchForm = reactive<{ month: string; rate_type: RateType; remark: string }>({
  month: todayCst().slice(0, 7),
  rate_type: 'MONTH_END',
  remark: '',
});

const batchEffectiveDate = computed(() => shiftMonthEnd(batchForm.month) || '—');

const batchColumns = [
  { colKey: 'code', title: '币种码', width: 100 },
  { colKey: 'label', title: '名称', width: 160 },
  { colKey: 'existing', title: '该月已录', width: 150 },
  { colKey: 'rate', title: '本次汇率', width: 200 },
];

function openBatch(): void {
  batchError.value = '';
  batchForm.month = todayCst().slice(0, 7);
  batchForm.rate_type = 'MONTH_END';
  batchForm.remark = '';
  batchVisible.value = true;
  void reloadBatchRows();
}

/** 该月已有汇率回填（换月份 / 换类型都要重拉：别拿上个月的值当本月默认） */
async function reloadBatchRows(): Promise<void> {
  const monthEnd = shiftMonthEnd(batchForm.month);
  const monthStart = `${batchForm.month}-01`;
  batchLoading.value = true;
  batchError.value = '';
  try {
    const existing = await fetchRatePage({
      rate_type: batchForm.rate_type,
      date_from: monthStart,
      date_to: monthEnd,
      page: 1,
      page_size: 200,
    });
    const byCode = new Map<string, string>();
    for (const row of existing.list ?? []) {
      byCode.set(row.from_currency, String(row.rate));
    }
    batchRows.value = currencies.value
      .filter((item) => !item.is_base)
      .map((item) => {
        const hit = byCode.get(item.code) ?? null;
        return {
          code: item.code,
          label: item.label,
          existing: hit,
          // 回填成「输入框里的默认值」：用户不改就是覆盖成同一个值（幂等）
          rate: hit === null ? undefined : Number(hit),
        };
      });
  } catch (error) {
    batchError.value = `读取该月已有汇率失败：${messageOf(error)}`;
    batchRows.value = currencies.value
      .filter((item) => !item.is_base)
      .map((item) => ({ code: item.code, label: item.label, existing: null, rate: undefined }));
  } finally {
    batchLoading.value = false;
  }
}

function handleBatchMonthChange(): void {
  void reloadBatchRows();
}

function handleBatchTypeChange(): void {
  void reloadBatchRows();
}

async function submitBatch(): Promise<void> {
  const rates = batchRows.value
    .filter((row) => row.rate !== undefined && row.rate !== null && Number(row.rate) > 0)
    .map((row) => ({
      from_currency: row.code,
      rate: String(row.rate),
      remark: batchForm.remark.trim() || null,
    }));
  if (rates.length === 0) {
    batchError.value = '至少给一个币种填写汇率（留空的行不会写库）';
    return;
  }
  batchSubmitting.value = true;
  batchError.value = '';
  try {
    const result = await batchMonthRates({
      month: batchForm.month,
      rate_type: batchForm.rate_type,
      remark: batchForm.remark.trim() || null,
      rates,
    });
    MessagePlugin.success(
      `${result.month} 月末（${result.effective_date}）已写入：新增 ${result.created_count} 条，覆盖 ${result.updated_count} 条`,
    );
    batchVisible.value = false;
    await loadRows();
  } catch (error) {
    batchError.value = messageOf(error);
  } finally {
    batchSubmitting.value = false;
  }
}

onMounted(async () => {
  await loadCurrencies();
  await loadRows();
});
</script>

<style scoped>
.rate-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  padding: 16px;
}

.page-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 14px;
}

.page-head__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
}

.page-head__tip {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  line-height: 1.7;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.filter-row__code {
  width: 190px;
}

.filter-row__type {
  width: 190px;
}

.filter-row__dates {
  width: 280px;
}

.filter-row__spacer {
  flex: 1;
}

.page-alert {
  margin-bottom: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.pair-cell {
  font-weight: 600;
}

.rate-cell {
  font-weight: 600;
  color: var(--td-brand-color);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.gate-wrap {
  display: inline-block;
}

.pager-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 12px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.rate-form {
  padding-top: 4px;
}

.batch-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 20px;
  margin-bottom: 12px;
}

.batch-head__item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.batch-head__item--wide {
  margin-bottom: 12px;
}

.batch-head__label {
  color: var(--td-text-color-secondary);
}

.batch-head__select {
  width: 190px;
}

.batch-tip {
  margin-top: 10px;
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
}
</style>
