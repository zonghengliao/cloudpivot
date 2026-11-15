<template>
  <div class="rate-page">
    <!-- ==================== 页面头 + 筛选 ==================== -->
    <!--
      ①② 主列表（服务端分页）交给 ListPage（P3-12 收口）。
      「按月批量录入」的行内编辑表格与新建 / 编辑表单仍是本页自己的弹窗（录入表单不是列表）。
    -->
    <ListPage
      ref="listRef"
      title="汇率管理"
      testid="rate"
      table-testid="rate-table"
      btn-prefix="btn-rate"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      @reset="handleReset"
    >
      <template #head-extra>
        <span class="page-head__tip">
          <t-tag v-if="baseCode" size="small" variant="light" theme="success">
            目标币种 {{ baseCode }}
          </t-tag>
          <t-tag
            v-if="fxSettings"
            size="small"
            variant="light"
            :theme="fxSettings.problem ? 'danger' : 'primary'"
            data-testid="rate-fx-account-tag"
          >
            汇兑损益 {{ fxCurrentLabel }}
          </t-tag>
          方向固定 <b>1 原币 = 汇率 × 目标币种</b>（只存「外币 → 本位币」，反向由 1/汇率 推导，不落重复行）。
          取数口径：<code>effective_date ≤ 单据日</code> 取最近一条（含当日），
          <b>建单那一刻锁进单据行</b>，之后改汇率不影响已建单据（SO-40）。
          月末重估（SO-41）读的是 <code>MONTH_END</code> 类型。
        </span>
      </template>

      <!-- 生效日区间：拆成 date_from / date_to 两个入参键，声明式控件表达不了 -->
      <template #filter-date_range>
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
      </template>

      <template #toolbar>
        <t-tooltip
          :content="fxHint || '指定汇兑差额记在哪个科目（期末调汇 / 应收重估都写它）'"
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="default"
              variant="outline"
              data-testid="btn-rate-fx-settings"
              @click="openFxSettings"
            >
              <template #icon><t-icon name="currency-exchange" /></template>
              汇兑损益科目
            </t-button>
          </span>
        </t-tooltip>
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
      </template>

      <!-- 币种下拉加载失败的提示（列表本身失败由 ListPage 自己的错误条兜） -->
      <template #filter-footer>
        <t-alert
          v-if="pageError"
          theme="error"
          :message="pageError"
          class="page-alert"
          data-testid="rate-error"
        />
      </template>

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

      <!-- 空态文案分两态：筛过 / 还没录过（原文案逐字保留） -->
      <template #empty>
        <div class="panel-empty" data-testid="rate-empty">
          <t-icon class="panel-empty__icon" name="money" />
          <p class="panel-empty__title">{{ hasFilter ? '没有符合条件的汇率' : '还没有汇率' }}</p>
          <p class="panel-empty__desc">
            {{
              hasFilter
                ? '换个币种 / 类型 / 日期试试，或点「重置」'
                : '点右上角「按月批量录入」一次录一批，或「新建汇率」单条录入'
            }}
          </p>
        </div>
      </template>
    </ListPage>

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

    <!-- ==================== 汇兑损益科目（M1-06） ==================== -->
    <t-dialog
      v-model:visible="fxVisible"
      header="汇兑损益科目"
      width="560px"
      :confirm-btn="{ content: '保存', loading: fxSaving, disabled: !canSaveFx }"
      :close-on-overlay-click="false"
      data-testid="rate-fx-settings-dialog"
      @confirm="submitFxSettings"
    >
      <t-alert v-if="fxError" theme="error" :message="fxError" class="page-alert" data-testid="rate-fx-settings-error" />
      <t-alert
        v-if="fxSettings?.problem"
        theme="warning"
        :message="`当前配置不可用：${fxSettings.problem}`"
        class="page-alert"
        data-testid="rate-fx-settings-problem"
      />
      <t-form label-width="112px" class="rate-form">
        <t-form-item label="当前科目">
          <span class="mono" data-testid="rate-fx-settings-current">{{ fxCurrentLabel }}</span>
          <t-tag size="small" variant="light" :theme="fxSettings?.source === 'CONFIG' ? 'primary' : 'default'">
            {{ fxSourceText }}
          </t-tag>
        </t-form-item>
        <t-form-item label="改为">
          <t-select
            v-model="fxSelected"
            :options="fxCandidateOpts"
            filterable
            placeholder="选择末级损益类科目（如 5603 财务费用）"
            data-testid="rate-fx-settings-select"
          />
          <template #help>
            只能选<b>启用中的末级损益类</b>科目，且该科目不能标记「外币核算」
            （标了就会自己被期末调汇扫进去，差额越滚越大）。
          </template>
        </t-form-item>
        <t-form-item label="影响面">
          <span class="muted" data-testid="rate-fx-settings-hint">{{ fxHint }}</span>
        </t-form-item>
        <t-form-item v-if="fxBlockedReason" label=" ">
          <span class="muted" data-testid="rate-fx-settings-blocked">{{ fxBlockedReason }}</span>
        </t-form-item>
      </t-form>
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
import { ApiError } from '@/api/request';
import {
  RATE_TYPE_OPTIONS,
  batchMonthRates,
  createRate,
  deleteRate,
  fetchCurrencyOptions,
  fetchFxSettings,
  fetchRatePage,
  updateFxSettings,
  updateRate,
  type CurrencyOption,
  type ExchangeRateFilter,
  type ExchangeRateRow,
  type FxSettings,
  type RateType,
} from '@/api/business/erp/currency';
import {
  PERM_FX_SETTINGS_UPDATE,
  fxAccountLabel,
  fxCandidateOptions,
  fxErrorText,
  fxSaveBlockedReason,
  fxSettingsHint,
  fxSourceLabel,
} from './fx-settings-logic';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';

/** 本页权限码（与 `V0316__currency_menu.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:fx-rate:list',
  create: 'business:erp:fx-rate:create',
  update: 'business:erp:fx-rate:update',
  remove: 'business:erp:fx-rate:delete',
  batch: 'business:erp:fx-rate:batch',
  quote: 'business:erp:fx-rate:quote',
  /** 汇兑损益科目配置（M1-06 · V0411）—— 写权限，读复用 list */
  fxSettings: PERM_FX_SETTINGS_UPDATE,
} as const;

const PAGE_SIZE = 20;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canBatch = computed(() => userStore.hasPermission(PERM.batch));

const rateTypeOptions = RATE_TYPE_OPTIONS;

const columns: ColumnDef[] = [
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

// ==================== 列表（筛选 / 分页 / 行操作交给 ListPage） ====================

const filter = reactive<ExchangeRateFilter>({
  from_currency: undefined,
  rate_type: undefined,
  date_from: undefined,
  date_to: undefined,
});
// ⚠️ 初值必须是 `[]` 而不是 `undefined`：`t-date-range-picker` 的 `value` 校验要求 Array，
// 传 undefined 会在控制台刷 `typeof value: undefined must be Array!`（功能不坏但脏）
const dateRange = ref<Array<string | number>>([]);
/** 页面级错误（目前只有币种下拉加载失败；列表加载失败由 ListPage 自己的错误条兜） */
const pageError = ref('');

/** ListPage 实例（保存 / 删除 / 批量录入后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 是否处于筛选态（空态文案分「没匹配」「还没录过」） */
const hasFilter = computed(() => Boolean(filter.from_currency || filter.rate_type || filter.date_from));

/**
 * 筛选项声明（顺序即渲染顺序）。
 *
 * 前两个是声明式下拉；生效日区间是 `custom` —— 它要拆成 `date_from` / `date_to`
 * 两个入参键，`t-date-range-picker` 的取值/清空语义也由页面自己接。
 * 三个控件改造前都是「改完点查询」，所以不开 `searchOnChange`（手感不变）。
 */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'from_currency',
    type: 'select',
    options: currencyCodeOptions.value,
    placeholder: '全部原币',
    width: 190,
    filterable: true,
    testid: 'rate-filter-from',
  },
  {
    key: 'rate_type',
    type: 'select',
    options: rateTypeOptions,
    placeholder: '全部类型',
    width: 190,
    testid: 'rate-filter-type',
  },
  { key: 'date_range', type: 'custom', testid: 'rate-filter-dates' },
]);

/** 拉一页 */
const fetcher: ListFetcher<ExchangeRateRow> = (query) => fetchRatePage(query as ExchangeRateFilter);

/** 入参拼装（与改造前 `loadRows` 里那段逐字一致：空值由 api 层的 `get` 整键丢掉） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as ExchangeRateFilter;
  return {
    from_currency: value.from_currency,
    rate_type: value.rate_type,
    date_from: value.date_from,
    date_to: value.date_to,
    page,
    page_size: pageSize,
  };
};

/** 重拉当前页（保存 / 删除 / 批量录入后） */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

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

/** 「重置」：ListPage 会清声明里的键，日期区间拆出来的两个键要页面自己清 */
function handleReset(): void {
  filter.date_from = undefined;
  filter.date_to = undefined;
  dateRange.value = [];
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
    await reloadList();
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
        await reloadList();
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
    await reloadList();
  } catch (error) {
    batchError.value = messageOf(error);
  } finally {
    batchSubmitting.value = false;
  }
}

// ==================== 汇兑损益科目配置（M1-06 · V0411） ====================
//
// 这块是**账务口径**：改的是「汇兑差额记哪个科目」，不是牌价。三条交互约定：
//   ① 只读也能看（`fx-rate:list` 就能看），写按钮另判 `fx-settings:update`；
//   ② 配置坏了（科目停用 / 被删）读接口仍 200，页面用 `problem` 画出原因并让用户就地改；
//   ③ 保存成功后要刷新页面上的「当前配置」提示（同一个响应体就是最新配置，不用再拉一次）。

/** 当前配置（读接口复用 `fx-rate:list`；读失败时保持 null，页面不显示这块信息） */
const fxSettings = ref<FxSettings | null>(null);
const fxVisible = ref(false);
const fxError = ref('');
const fxSaving = ref(false);
const fxSelected = ref('');

const canUpdateFxSettings = computed(() => userStore.hasPermission(PERM_FX_SETTINGS_UPDATE));
const fxCandidateOpts = computed(() => (fxSettings.value ? fxCandidateOptions(fxSettings.value) : []));
const fxCurrentLabel = computed(() => (fxSettings.value ? fxAccountLabel(fxSettings.value) : '—'));
const fxSourceText = computed(() => (fxSettings.value ? fxSourceLabel(fxSettings.value) : '—'));
const fxHint = computed(() => (fxSettings.value ? fxSettingsHint(fxSettings.value) : ''));
const fxBlockedReason = computed(() =>
  fxSaveBlockedReason(fxSelected.value, fxSettings.value, canUpdateFxSettings.value),
);
const canSaveFx = computed(() => fxBlockedReason.value === null);

async function loadFxSettings(): Promise<void> {
  try {
    fxSettings.value = await fetchFxSettings();
  } catch (error) {
    // 读配置失败不该让汇率列表报错（列表本身仍可用）
    fxSettings.value = null;
    fxError.value = `汇兑损益科目读取失败：${messageOf(error)}`;
  }
}

function openFxSettings(): void {
  fxError.value = '';
  fxSelected.value = fxSettings.value?.account_code ?? '';
  fxVisible.value = true;
  if (!fxSettings.value) {
    void loadFxSettings();
  }
}

async function submitFxSettings(): Promise<void> {
  if (!canSaveFx.value) {
    return;
  }
  fxSaving.value = true;
  fxError.value = '';
  try {
    fxSettings.value = await updateFxSettings(fxSelected.value);
    fxSelected.value = fxSettings.value.account_code;
    fxVisible.value = false;
    MessagePlugin.success(`汇兑损益科目已改为 ${fxAccountLabel(fxSettings.value)}`);
  } catch (error) {
    const code = error instanceof ApiError ? error.code : undefined;
    fxError.value = fxErrorText(code, messageOf(error));
  } finally {
    fxSaving.value = false;
  }
}

onMounted(async () => {
  // 主列表由 ListPage 自己拉首屏；这里只补币种主数据（下拉 / 目标币种 / 批量录入行）
  // 与汇兑损益科目配置（页面顶部要显示「现在记在哪个科目」）
  await loadCurrencies();
  await loadFxSettings();
});
</script>

<style scoped>
.rate-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 页头附注（#head-extra）：方向 / 取数口径说明 */
.page-head__tip {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  line-height: 1.7;
}

/* 生效日区间（custom 筛选插槽自绘） */
.filter-row__dates {
  width: 280px;
}

/* 空态（#empty） */
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
