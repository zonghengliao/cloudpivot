<!--
  费用类型页（FIN-3-FE / ARCH-6 §4.10 / §4.2 `biz_expense_type`）。

  这一页是报销单的「口径台账」：每条类型决定
  ① 借哪个费用科目（必须末级 + 借方）、
  ② 进项税能不能抵扣（招待费 / 福利费 → false，前端税额恒 0）、
  ③ 是否受预算控制（决定提交时是否占预算）、
  ④ 是否必填发票号、⑤ 单笔金额上限（0 = 不限）。

  # 为什么「删除」要给 409 兜底

  被报销单引用过的类型后端会拒绝 DELETE（409），只能停用 —— 硬删会让历史单据
  的科目快照失去来源。所以这里删失败时直接提示「改停用」，并保留启停按钮。

  # 科目下拉只列末级

  `fetchBudgetAccountOptions` 已经是 `is_leaf=true&status=1`：后端 `account_snapshot`
  对非末级科目直接 400，与其让用户选完再吃报错，不如根本不出现在下拉里。
-->
<template>
  <div class="type-page">
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="expense-type-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建费用类型
        </t-button>
        <t-button variant="outline" data-testid="expense-type-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="expense-type-filter-status"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="编码 / 名称（模糊）"
          clearable
          data-testid="expense-type-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="expense-type-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="expense-type-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">费用类型</span>
          <span class="panel-head__meta" data-testid="expense-type-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          编码留空保存时自动生成（ET-01 形态）；「不可抵扣」的类型在报销单里税额恒为 0
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="expense-type-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="expense-type-table"
        @page-change="handlePageChange"
      >
        <template #type_code="{ row }">
          <span class="mono" :data-testid="`expense-type-code-${row.type_code}`">{{ row.type_code }}</span>
        </template>
        <template #type_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.type_name }}</span>
            <span v-if="row.remark" class="name-cell__sub">{{ row.remark }}</span>
          </div>
        </template>
        <template #account="{ row }">
          <div class="name-cell">
            <span class="mono">{{ row.account_code }}</span>
            <span class="name-cell__sub">{{ row.account_name }}</span>
          </div>
        </template>
        <template #tax_deductible="{ row }">
          <t-tag size="small" variant="light" :theme="row.tax_deductible ? 'success' : 'warning'">
            {{ row.tax_deductible ? '可抵扣' : '不可抵扣' }}
          </t-tag>
        </template>
        <template #budget_controlled="{ row }">
          <span class="muted">{{ row.budget_controlled ? '受预算' : '不占预算' }}</span>
        </template>
        <template #need_invoice_no="{ row }">
          <span class="muted">{{ row.need_invoice_no ? '必填' : '可选' }}</span>
        </template>
        <template #amount_limit="{ row }">
          <span class="money">{{ limitText(row.amount_limit) }}</span>
        </template>
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="row.status === 1 ? 'success' : 'default'">
            {{ row.status === 1 ? '启用' : '停用' }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-button
              v-privilege="[PERM.update]"
              variant="text"
              size="small"
              data-testid="expense-type-op-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-button>
            <t-button
              v-privilege="[PERM.update]"
              variant="text"
              size="small"
              data-testid="expense-type-op-toggle"
              @click="toggleStatus(row)"
            >
              {{ row.status === 1 ? '停用' : '启用' }}
            </t-button>
            <t-button
              v-privilege="[PERM.remove]"
              variant="text"
              size="small"
              theme="danger"
              data-testid="expense-type-op-delete"
              @click="handleDelete(row)"
            >
              删除
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== 新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="editingId ? `编辑费用类型 ${editingCode}` : '新建费用类型'"
      width="720px"
      :confirm-btn="{ content: '保存', theme: 'primary', loading: saving }"
      data-testid="expense-type-dialog"
      @confirm="handleSave"
    >
      <t-form label-width="112px" :data="form" class="form">
        <t-row :gutter="[16, 8]">
          <t-col :span="12">
            <t-form-item label="编码" :status="codeError ? 'error' : undefined">
              <!-- CODE-07 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
              <div class="dlg-inline">
                <div class="dlg-inline__input">
                  <t-input
                    v-model="form.type_code"
                    :maxlength="40"
                    placeholder="留空自动生成，如 ET-01"
                    :disabled="!!editingId"
                    data-testid="expense-type-form-code"
                  />
                </div>
                <t-button
                  variant="outline"
                  theme="primary"
                  :disabled="!!editingId"
                  :loading="genCodeLoading"
                  data-testid="expense-type-gen-code"
                  @click="handleGenCode"
                >
                  生成编码
                </t-button>
              </div>
              <template #help>
                <span class="dlg-help">
                  {{
                    codeError ||
                    (editingId
                      ? '编码是历史报销单的科目快照口径，不支持修改'
                      : '不点「生成编码」则保存时自动生成；生成后仍可手改（企业自有编码如 ADMIN-FEE 也可直接填）')
                  }}
                </span>
              </template>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="名称" :status="nameError ? 'error' : undefined" :help="nameError || undefined">
              <t-input v-model="form.type_name" placeholder="如 差旅费" data-testid="expense-type-form-name" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item
              label="借方科目"
              :status="accountError ? 'error' : undefined"
              :help="accountError || '只列末级科目（父科目入账会被后端 400）'"
            >
              <t-select
                v-model="form.account_id"
                :options="accountOptions"
                placeholder="选择末级费用科目"
                filterable
                data-testid="expense-type-form-account"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="进项税可抵扣">
              <div class="switch-cell">
                <t-switch
                  v-model="form.tax_deductible"
                  data-testid="expense-type-form-deductible"
                />
                <span class="switch-cell__tip">
                  {{ form.tax_deductible ? '按含税拆价税' : '税额恒 0（招待费 / 福利费）' }}
                </span>
              </div>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="受预算控制">
              <div class="switch-cell">
                <t-switch
                  v-model="form.budget_controlled"
                  data-testid="expense-type-form-budget"
                />
                <span class="switch-cell__tip">{{ form.budget_controlled ? '提交时占用预算' : '不占预算' }}</span>
              </div>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="必填发票号">
              <div class="switch-cell">
                <t-switch v-model="form.need_invoice_no" data-testid="expense-type-form-invoice" />
                <span class="switch-cell__tip">{{ form.need_invoice_no ? '每行必须填发票号' : '发票号可选' }}</span>
              </div>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="单笔上限"
              :status="limitError ? 'error' : undefined"
              :help="limitError || '0 表示不限'"
            >
              <t-input v-model="form.amount_limit" placeholder="0 = 不限" data-testid="expense-type-form-limit" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="排序">
              <t-input-number v-model="form.sort_order" :min="0" theme="normal" data-testid="expense-type-form-sort" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="状态">
              <t-select v-model="form.status" :options="statusOptions" data-testid="expense-type-form-status" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="备注">
              <t-input v-model="form.remark" :maxlength="200" placeholder="可选" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  createExpenseType,
  deleteExpenseType,
  fetchExpenseTypeNextCode,
  fetchExpenseTypePage,
  updateExpenseType,
  type ExpenseType,
} from '@/api/business/erp/expense';
import { fetchBudgetAccountOptions, type BudgetAccountOption } from '@/api/business/erp/budget';
import { ApiError } from '@/api/request';
import { formatMoney } from '@/views/business/erp/shared/money';
import { masterCodeError } from '@/views/business/erp/shared/master-code';
import {
  emptyTypeForm,
  typeFormFromRow,
  toTypePayload,
  validateTypeForm,
  type ExpenseTypeForm,
} from '../expense/expense-logic';

/** 本页权限码（V0242 的 4 个，一个不多） */
const PERM = {
  list: 'business:erp:expense-type:list',
  create: 'business:erp:expense-type:create',
  update: 'business:erp:expense-type:update',
  remove: 'business:erp:expense-type:delete',
} as const;

const PAGE_SIZE = 20;

const rows = ref<ExpenseType[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<{ keyword: string; status: number | undefined }>({ keyword: '', status: undefined });

const statusOptions = [
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
];

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

const columns: PrimaryTableCol[] = [
  { colKey: 'type_code', title: '编码', width: 110 },
  { colKey: 'type_name', title: '名称 / 备注', width: 140, ellipsis: true },
  { colKey: 'account', title: '借方科目', width: 150, ellipsis: true },
  { colKey: 'tax_deductible', title: '进项税', width: 80 },
  { colKey: 'budget_controlled', title: '预算', width: 76 },
  { colKey: 'need_invoice_no', title: '发票号', width: 70 },
  { colKey: 'amount_limit', title: '单笔上限', width: 96, align: 'right' },
  { colKey: 'status', title: '状态', width: 70 },
  { colKey: 'op', title: '操作', width: 160, fixed: 'right' },
];

/** 单笔上限文案（0 = 不限，别显示成「0.00」） */
function limitText(raw: string | null | undefined): string {
  const cents = Number(raw ?? 0) * 100;
  if (!Number.isFinite(cents) || cents <= 0) {
    return '不限';
  }
  return formatMoney(raw);
}

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchExpenseTypePage({
      page: current.value,
      page_size: PAGE_SIZE,
      keyword: filter.keyword.trim(),
      status: filter.status,
    });
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    loadError.value = errorText(error);
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadRows();
}

function handleReset(): void {
  filter.keyword = '';
  filter.status = undefined;
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 表单 ====================

const dialogVisible = ref(false);
const editingId = ref<string | null>(null);
const editingCode = ref('');
const saving = ref(false);
const form = reactive<ExpenseTypeForm>(emptyTypeForm());
const accounts = ref<BudgetAccountOption[]>([]);

const accountOptions = computed(() =>
  accounts.value.map((item) => ({
    label: `${item.account_code} ${item.account_name}`,
    value: item.id,
  })),
);

/**
 * 校验只在**点过保存之后**才标红。
 *
 * 一打开弹窗就满屏「名称必填 / 请选择科目」是噪音：用户还没输入就被判错。
 * `attempted` 由 `handleSave` 置位，新建 / 编辑时清零。
 */
const attempted = ref(false);

function codeProblem(): string {
  // CODE-07：编码可留空（后端自动取号 ET-01），只校验「填了的话」的字符集。
  return masterCodeError(form.type_code) ?? '';
}

const codeError = computed(() => (attempted.value ? codeProblem() : ''));
const nameError = computed(() => (attempted.value && !form.type_name.trim() ? '名称必填' : ''));
const accountError = computed(() => (attempted.value && !form.account_id ? '请选择末级借方科目' : ''));
const limitError = computed(() =>
  Number(form.amount_limit) < 0 ? '不能为负（0 = 不限）' : '',
);

function openCreate(): void {
  Object.assign(form, emptyTypeForm());
  editingId.value = null;
  editingCode.value = '';
  attempted.value = false;
  dialogVisible.value = true;
}

function openEdit(row: ExpenseType): void {
  Object.assign(form, typeFormFromRow(row));
  editingId.value = row.id;
  editingCode.value = row.type_code;
  attempted.value = false;
  dialogVisible.value = true;
}

// ==================== 生成编码（CODE-07 交互方式 B） ====================

/** 「生成编码」按钮加载中 */
const genCodeLoading = ref(false);

/**
 * 取下一个可用编码填入编码框（**不占号**，可手改）。
 *
 * 后端 `GET /business/erp/expense-type/next-code` 只读 `biz_seq`（方案 B）：
 * - 点了又取消不会浪费流水；
 * - 并发下两人可能看到同一个号 ⇒ 保存时被抢走的人是 `409 + 66001`，
 *   错误面板会把后端原文（点名编码）弹出来，点一次「生成编码」换个号即可。
 */
async function handleGenCode(): Promise<void> {
  if (editingId.value) {
    return;
  }
  genCodeLoading.value = true;
  try {
    const preview = await fetchExpenseTypeNextCode();
    form.type_code = preview.code;
    // 之前「格式不合法」的红字已不成立，清掉免得跟着新号走
    attempted.value = false;
  } catch (error) {
    MessagePlugin.error(`生成编码失败：${errorText(error)}`);
  } finally {
    genCodeLoading.value = false;
  }
}

async function handleSave(): Promise<void> {
  attempted.value = true;
  const error = validateTypeForm(form);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  saving.value = true;
  try {
    const payload = toTypePayload(form);
    if (editingId.value) {
      await updateExpenseType(editingId.value, payload);
      MessagePlugin.success('已保存');
    } else {
      const created = await createExpenseType(payload);
      // 没点「生成编码」直接保存时编码是后端取的号 —— 回填到表单并在提示里报出来，
      // 用户当场就能看到这条类型到底叫 ET-0 几。
      form.type_code = created.type_code;
      MessagePlugin.success(`已新建费用类型 · 编码 ${created.type_code}`);
    }
    dialogVisible.value = false;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    saving.value = false;
  }
}

// ==================== 启停 / 删除 ====================

async function toggleStatus(row: ExpenseType): Promise<void> {
  const nextStatus = row.status === 1 ? 0 : 1;
  const nextForm = typeFormFromRow(row);
  nextForm.status = nextStatus;
  try {
    await updateExpenseType(row.id, toTypePayload(nextForm));
    MessagePlugin.success(nextStatus === 1 ? '已启用' : '已停用（历史单据不受影响）');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  }
}

function handleDelete(row: ExpenseType): void {
  const dialog = DialogPlugin.confirm({
    header: '删除费用类型',
    body: `确认删除「${row.type_code} ${row.type_name}」？被报销单引用过的类型删不掉（只能停用）。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteExpenseType(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadRows();
      } catch (error) {
        // 409 = 已被报销单引用：这不是故障，是业务约束，给可执行建议
        if (error instanceof ApiError && error.status === 409) {
          MessagePlugin.warning('该类型已被报销单引用，不能删除；请改用「停用」');
          dialog.hide();
        } else {
          MessagePlugin.error(errorText(error));
        }
      }
    },
  });
}

// ==================== 初始化 ====================

async function loadAccounts(): Promise<void> {
  try {
    accounts.value = await fetchBudgetAccountOptions();
  } catch (error) {
    MessagePlugin.warning(`科目下拉加载失败：${errorText(error)}`);
  }
}

function errorText(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
}

onMounted(async () => {
  await Promise.all([loadAccounts(), loadRows()]);
});
</script>

<style scoped>
.type-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__status {
  width: 140px;
}

.tool-row__keyword {
  width: 220px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta {
  margin-left: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.list-alert {
  margin-bottom: 12px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;
}

.name-cell__main {
  color: var(--td-text-color-primary);
}

.name-cell__sub {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 2px;
}

.money {
  font-variant-numeric: tabular-nums;
  font-family: var(--td-font-family-number, monospace);
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.muted {
  color: var(--td-text-color-secondary);
}

.switch-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 32px;
}

.switch-cell__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

/* CODE-07：「编码框 + 生成编码」一行铺满（输入框吃掉剩余宽度，按钮不缩） */
.dlg-inline {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  flex-wrap: nowrap;
}

.dlg-inline__input {
  flex: 1 1 auto;
  min-width: 0;
}

.dlg-help {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.form {
  margin-top: 4px;
}

/* 下拉 / 数字输入框撑满列宽，避免同一行左右宽窄不一 */
.form :deep(.t-select__wrap),
.form :deep(.t-input-number),
.form :deep(.t-input) {
  width: 100%;
}
</style>
