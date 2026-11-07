<template>
  <div class="currency-page">
    <!-- ==================== 页面头 + 筛选 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="page-head">
        <div class="page-head__title">
          <span>币种管理</span>
          <t-tag v-if="baseCode" size="small" variant="light" theme="success">
            本位币 {{ baseCode }}
          </t-tag>
        </div>
        <span class="page-head__tip">
          币种码是<b>主键</b>（三位 ISO 4217 大写），建了就不能改；不用了就<b>停用</b>（停用后新单据不可选，
          历史单据与汇率照旧可查）。本位币<b>全库唯一</b>，且必须是「启用」状态 ——
          全仓建单取本位币只认 <code>is_base AND status = 'ACTIVE'</code>。
        </span>
      </div>

      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="币种码 / 名称"
          clearable
          data-testid="currency-filter-keyword"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
        <t-select
          v-model="filter.status"
          class="filter-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="currency-filter-status"
        />
        <t-button theme="primary" data-testid="btn-currency-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-currency-reset" @click="handleReset">重置</t-button>
        <span class="filter-row__spacer" />
        <t-tooltip
          :content="
            canCreate
              ? '新增币种（三位 ISO 4217 大写码，如 JPY / GBP）'
              : '当前账号没有 business:erp:currency:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-currency-new"
              :disabled="!canCreate"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建币种
            </t-button>
          </span>
        </t-tooltip>
      </div>

      <t-alert
        v-if="pageError"
        theme="error"
        :message="pageError"
        class="page-alert"
        data-testid="currency-error"
      />
      <t-alert
        v-if="!hasBase && !pageError"
        theme="warning"
        message="当前账套没有本位币：建单会按 CNY 兜底（日志只有一条 warn）。请把某个启用的币种「设为本位币」。"
        class="page-alert"
        data-testid="currency-no-base-warning"
      />

      <t-table
        row-key="code"
        size="small"
        hover
        :data="rows"
        :columns="columns"
        :loading="loading"
        :empty="emptyText"
        data-testid="currency-table"
      >
        <template #code="{ row }">
          <span class="mono code-cell" data-testid="currency-code">{{ row.code }}</span>
        </template>
        <template #name="{ row }">
          <span>{{ row.name }}</span>
        </template>
        <template #symbol="{ row }">
          <span class="mono">{{ row.symbol }}</span>
        </template>
        <template #decimal_places="{ row }">
          <span class="mono">{{ row.decimal_places }}</span>
        </template>
        <template #is_base="{ row }">
          <t-tag
            v-if="row.is_base"
            size="small"
            variant="light"
            theme="success"
            data-testid="currency-base-tag"
          >
            本位币
          </t-tag>
          <span v-else class="muted">—</span>
        </template>
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="row.status === 'ACTIVE' ? 'success' : 'default'">
            {{ row.status === 'ACTIVE' ? '启用' : '停用' }}
          </t-tag>
        </template>
        <template #rate_count="{ row }">
          <span class="mono">{{ row.rate_count }}</span>
        </template>
        <template #updated_at="{ row }">
          <span class="mono muted">{{ formatTime(row.updated_at) }}</span>
        </template>
        <template #op="{ row }">
          <div class="op-row">
            <t-link
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="btn-currency-edit"
              @click="openEdit(row as CurrencyRow)"
            >
              编辑
            </t-link>
            <t-link
              v-if="!row.is_base"
              v-privilege="[PERM.base]"
              theme="primary"
              data-testid="btn-currency-set-base"
              @click="confirmSetBase(row as CurrencyRow)"
            >
              设为本位币
            </t-link>
            <t-tooltip
              v-if="!row.is_base"
              :content="row.status === 'ACTIVE' ? '停用后新单据不可选，历史数据保留' : '重新启用'"
              placement="top"
            >
              <t-link
                v-privilege="[PERM.update]"
                :theme="row.status === 'ACTIVE' ? 'warning' : 'success'"
                data-testid="btn-currency-toggle"
                @click="toggleStatus(row as CurrencyRow)"
              >
                {{ row.status === 'ACTIVE' ? '停用' : '启用' }}
              </t-link>
            </t-tooltip>
            <t-tooltip v-else content="本位币不能停用：请先把别的币种设为本位币" placement="top">
              <span class="gate-wrap">
                <t-link theme="default" class="disabled-link">停用</t-link>
              </span>
            </t-tooltip>
          </div>
        </template>
      </t-table>

      <div class="pager-row">
        <span class="pager-row__total">共 {{ rows.length }} 个币种（主数据不分页）</span>
      </div>
    </t-card>

    <!-- ==================== 新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingCode ? `编辑币种 ${editingCode}` : '新建币种'"
      width="560px"
      :confirm-btn="{ content: editingCode ? '保存' : '创建', loading: submitting }"
      :close-on-overlay-click="false"
      data-testid="currency-form-dialog"
      @confirm="submitForm"
    >
      <t-alert v-if="formError" theme="error" :message="formError" class="page-alert" />
      <t-form label-width="104px" class="currency-form">
        <t-form-item label="币种码" required-mark>
          <t-input
            v-model="form.code"
            placeholder="三位大写，如 JPY"
            :maxlength="3"
            :disabled="Boolean(editingCode)"
            data-testid="currency-form-code"
          />
          <template #help>
            {{
              editingCode
                ? '币种码是主键，不可修改（历史汇率与单据按码引用；要换码请新建 + 停用旧行）'
                : 'ISO 4217 三位大写（小写会自动转大写）'
            }}
          </template>
        </t-form-item>
        <t-form-item label="名称" required-mark>
          <t-input v-model="form.name" placeholder="如 日元" :maxlength="64" data-testid="currency-form-name" />
        </t-form-item>
        <t-form-item label="符号" required-mark>
          <t-input v-model="form.symbol" placeholder="如 ¥ / $ / €" :maxlength="8" data-testid="currency-form-symbol" />
        </t-form-item>
        <t-form-item label="小数位">
          <t-input-number
            v-model="form.decimal_places"
            theme="normal"
            :min="0"
            :max="4"
            :step="1"
            data-testid="currency-form-decimals"
          />
          <template #help>0 = JPY / KRW；2 = 常见；4 是本仓上限（金额列统一 NUMERIC(18,4)）</template>
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="form.status" data-testid="currency-form-status">
            <t-radio value="ACTIVE">启用</t-radio>
            <t-radio value="INACTIVE">停用</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item v-if="!editingCode" label="本位币">
          <t-checkbox v-model="form.set_base" data-testid="currency-form-set-base">
            创建后立即设为本位币（会接管现有本位币）
          </t-checkbox>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="form.remark" placeholder="选填" :maxlength="200" data-testid="currency-form-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 币种管理页（销售链 · SO-42 / ARCH-7 §15.6.3 ①）
 *
 * # 这张表的口径（写错了会让全仓金额换算错）
 *
 * | 事实 | 后果 |
 * |------|------|
 * | 币种码是**主键**（`^[A-Z]{3}$`） | 建了不可改；改码 = 新建 + 停用旧行 |
 * | `is_base` 全库**唯一**（部分唯一索引） | 「设为本位币」是**接管**，不是"多选一个" |
 * | 本位币**必须 ACTIVE** | 停掉本位币 → 建单取本位币失败，回落 CNY 只有一条 warn |
 * | 停用 ≠ 删除 | 停用后新单据不可选；历史单据 / 汇率按码引用，照旧可查 |
 * | `decimal_places ∈ 0..4` | 本仓金额列 `NUMERIC(18,4)`，> 4 位会静默丢分 |
 *
 * # 权限
 *
 * 四个码各挡一个动作；`v-privilege` 只隐藏**按钮**，接口侧后端会再判一次
 * （前端隐藏不是安全边界）。
 *
 * # 为什么不分页
 *
 * `biz_currency` 是 4~10 行的极小主数据（V0272 只种了 CNY/USD/EUR/HKD），
 * 一次拉全比翻页更好用；下拉（开单页）与列表共用同一个权限码。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  CURRENCY_STATUS_OPTIONS,
  createCurrency,
  fetchCurrencyList,
  setBaseCurrency,
  updateCurrency,
  type CurrencyFilter,
  type CurrencyRow,
  type CurrencyStatus,
} from '@/api/business/erp/currency';

/** 本页权限码（与 `V0316__currency_menu.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:currency:list',
  create: 'business:erp:currency:create',
  update: 'business:erp:currency:update',
  base: 'business:erp:currency:base',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));

const statusOptions = CURRENCY_STATUS_OPTIONS;

/** 表格列（币种码 / 名称 / 符号 / 小数位 / 本位币 / 状态 / 汇率条数 / 更新时间 / 操作） */
const columns = [
  { colKey: 'code', title: '币种码', width: 96 },
  { colKey: 'name', title: '名称', width: 140 },
  { colKey: 'symbol', title: '符号', width: 80 },
  { colKey: 'decimal_places', title: '小数位', width: 80 },
  { colKey: 'is_base', title: '本位币', width: 100 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'rate_count', title: '汇率条数', width: 96 },
  { colKey: 'updated_at', title: '更新时间', width: 160 },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right' as const },
];

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（只做展示，不做时区换算：后端已是 UTC 时间戳） */
function formatTime(raw: string): string {
  if (!raw) {
    return '—';
  }
  return raw.replace('T', ' ').slice(0, 16);
}

// ==================== 列表 ====================

const filter = reactive<CurrencyFilter>({ status: undefined, keyword: '' });
const rows = ref<CurrencyRow[]>([]);
const loading = ref(false);
const pageError = ref('');

const baseCode = computed(() => rows.value.find((row) => row.is_base)?.code ?? '');
const hasBase = computed(() => rows.value.some((row) => row.is_base));

const emptyText = computed(() => {
  if (loading.value) {
    return '加载中…';
  }
  if (filter.keyword || filter.status) {
    return '没有符合条件的币种（换个关键字 / 状态试试，或点「重置」）';
  }
  return '还没有币种 —— 点右上角「新建币种」';
});

async function loadRows(): Promise<void> {
  loading.value = true;
  pageError.value = '';
  try {
    rows.value = await fetchCurrencyList({
      status: filter.status as CurrencyStatus | undefined,
      keyword: filter.keyword?.trim() || undefined,
    });
  } catch (error) {
    rows.value = [];
    pageError.value = `币种加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  void loadRows();
}

function handleReset(): void {
  filter.keyword = '';
  filter.status = undefined;
  void loadRows();
}

// ==================== 新建 / 编辑 ====================

interface CurrencyFormDraft {
  code: string;
  name: string;
  symbol: string;
  decimal_places: number;
  status: CurrencyStatus;
  remark: string;
  set_base: boolean;
}

const formVisible = ref(false);
const submitting = ref(false);
const formError = ref('');
const editingCode = ref('');
const form = reactive<CurrencyFormDraft>(emptyForm());

function emptyForm(): CurrencyFormDraft {
  return {
    code: '',
    name: '',
    symbol: '',
    decimal_places: 2,
    status: 'ACTIVE',
    remark: '',
    set_base: false,
  };
}

function openCreate(): void {
  editingCode.value = '';
  formError.value = '';
  Object.assign(form, emptyForm());
  formVisible.value = true;
}

function openEdit(row: CurrencyRow): void {
  editingCode.value = row.code;
  formError.value = '';
  Object.assign(form, {
    code: row.code,
    name: row.name,
    symbol: row.symbol,
    decimal_places: row.decimal_places,
    status: row.status,
    remark: row.remark ?? '',
    set_base: false,
  });
  formVisible.value = true;
}

/** 前端先挡一遍（后端还会再判；前端挡是为了不白跑一趟网络） */
function validateForm(): string {
  const code = form.code.trim().toUpperCase();
  if (!/^[A-Z]{3}$/.test(code)) {
    return '币种码必须是三位大写字母（如 JPY）';
  }
  if (!form.name.trim()) {
    return '名称不能为空';
  }
  if (!form.symbol.trim()) {
    return '符号不能为空';
  }
  if (form.decimal_places < 0 || form.decimal_places > 4) {
    return '小数位必须在 0~4 之间';
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
    if (editingCode.value) {
      const updated = await updateCurrency(editingCode.value, {
        name: form.name.trim(),
        symbol: form.symbol.trim(),
        decimal_places: form.decimal_places,
        status: form.status,
        remark: form.remark.trim(),
      });
      MessagePlugin.success(`已保存 ${updated.code}`);
    } else {
      const created = await createCurrency({
        code: form.code.trim().toUpperCase(),
        name: form.name.trim(),
        symbol: form.symbol.trim(),
        decimal_places: form.decimal_places,
        status: form.status,
        remark: form.remark.trim() || null,
        set_base: form.set_base,
      });
      MessagePlugin.success(`已创建 ${created.code}${created.is_base ? '（并设为本位币）' : ''}`);
    }
    formVisible.value = false;
    await loadRows();
  } catch (error) {
    // 后端原文（码重复 / 本位币不能停用 / 约束）直接摆到弹窗里
    formError.value = messageOf(error);
  } finally {
    submitting.value = false;
  }
}

// ==================== 立本位币 / 启停 ====================

function confirmSetBase(row: CurrencyRow): void {
  const dialog = DialogPlugin.confirm({
    header: '设为本位币',
    body: `把 ${row.name}（${row.code}）设为账套本位币？当前本位币 ${baseCode.value} 会同时被取消。\n\n本位币全库唯一：已建单据里锁定的汇率不变，但新单据的币种口径从此以后按 ${row.code} 记。`,
    theme: 'warning',
    confirmBtn: '设为本位币',
    onConfirm: async () => {
      try {
        await setBaseCurrency(row.code);
        MessagePlugin.success(`${row.code} 已设为本位币`);
        await loadRows();
      } catch (error) {
        MessagePlugin.error(`设置失败：${messageOf(error)}`);
      } finally {
        dialog.hide();
      }
    },
  });
}

async function toggleStatus(row: CurrencyRow): Promise<void> {
  const next: CurrencyStatus = row.status === 'ACTIVE' ? 'INACTIVE' : 'ACTIVE';
  try {
    await updateCurrency(row.code, { status: next });
    MessagePlugin.success(`${row.code} ${next === 'ACTIVE' ? '已启用' : '已停用'}`);
    await loadRows();
  } catch (error) {
    // 本位币停用会被后端 409 挡住，原文带修复指引
    MessagePlugin.error(messageOf(error));
  }
}

onMounted(() => {
  void loadRows();
});
</script>

<style scoped>
.currency-page {
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

.filter-row__keyword {
  width: 220px;
}

.filter-row__status {
  width: 140px;
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

.code-cell {
  font-weight: 600;
  letter-spacing: 0.04em;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.disabled-link {
  color: var(--td-text-color-disabled);
  cursor: not-allowed;
}

.gate-wrap {
  display: inline-block;
}

.pager-row {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.currency-form {
  padding-top: 4px;
}
</style>
