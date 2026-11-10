<!--
  手工费用台账页（COST-B2-13 / ARCH-9 §6.1 #3 · §4.3 · §5.2 #17–#19）。

  一页三件事：**列表（期间/车间/费用项目/关键字）→ 新增（池 or 直接计入）→ 删除**。

  # 最重要的一条语义：**池行 vs 直接行**

  `cost_object_id` 空 = 车间费用池（当期汇总后按分摊规则摊给各对象）；
  非空 = 直接计入该批（不进池）。界面上用「车间池」标签 + 本页汇总（池金额 / 直入金额）
  把两者分开，避免财务把"直接计入"看成"还没摊的池"。

  # 没有的东西（别以为是漏了）

  * **编辑**：设计 §5.2 只给了 list / create / delete（`biz_cost_overhead` 也没有状态列），
    录错请删掉重录；已被归集引用时后端 409（报文点名期间）；
  * **导入**：设计 §5.4 明确「费用台账二期再开导入，一期表单够用」。
-->
<template>
  <div class="cost-overhead-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="co-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增费用
        </t-button>
        <t-button variant="outline" data-testid="co-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-date-picker
          v-model="filter.period_code"
          class="tool-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间"
          clearable
          data-testid="co-filter-period"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部车间"
          clearable
          filterable
          data-testid="co-filter-dept"
          @change="handleSearch"
        />
        <DictSelect
          v-model="filter.expense_item"
          class="tool-row__item"
          :dict-code="EXPENSE_ITEM_DICT_CODE"
          placeholder="全部费用项目"
          data-testid="co-filter-item"
          @update:model-value="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="科目 / 车间 / 备注（模糊）"
          clearable
          data-testid="co-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="co-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="co-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">手工费用台账</span>
          <span class="panel-head__meta" data-testid="co-meta">共 {{ total }} 行</span>
        </div>
        <span class="panel-head__tip">
          折旧由固定资产模块自动进来、报销由费用报销进来；这里只录「手工台账」，
          借方必须是 4101.* 制造费用科目
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="co-load-error"
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
        data-testid="co-table"
        @page-change="handlePageChange"
      >
        <template #period_code="{ row }">
          <span class="mono">{{ row.period_code }}</span>
        </template>
        <template #cost_object_id="{ row }">
          <span v-if="isPoolRow(row)" class="pool-tag" data-testid="co-pool-tag">
            <t-tag size="small" variant="light" theme="warning">车间池</t-tag>
            <span class="muted">待分摊</span>
          </span>
          <span v-else class="mono">{{ row.cost_object_no || shortId(row.cost_object_id) }}</span>
        </template>
        <template #dept_name="{ row }">{{ row.dept_name || shortId(row.dept_id) }}</template>
        <template #expense_item="{ row }">
          {{ expenseItemLabel(row.expense_item, row.expense_item_label) }}
        </template>
        <template #debit_account_code="{ row }">
          <span class="mono">{{ row.debit_account_code }}</span>
          <span class="muted">{{ row.debit_account_name || '' }}</span>
        </template>
        <template #credit_account_code="{ row }">
          <span class="mono">{{ row.credit_account_code }}</span>
          <span class="muted">{{ row.credit_account_name || '' }}</span>
        </template>
        <template #amount="{ row }">
          <span class="money" :data-testid="`co-amount-${row.id}`">{{ formatMoney(row.amount) }}</span>
        </template>
        <template #remark="{ row }">{{ row.remark || '—' }}</template>
        <template #op="{ row }">
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            data-testid="co-btn-delete"
            @click="handleDelete(row)"
          >
            删除
          </t-link>
        </template>
      </t-table>

      <div class="summary-row" data-testid="co-summary">
        <span>
          本页 {{ summary.row_count }} 行：车间池
          <b class="money">{{ formatMoney(summary.pool_amount) }}</b>
          （{{ summary.pool_count }} 行）· 直接计入
          <b class="money">{{ formatMoney(summary.direct_amount) }}</b>
          （{{ summary.direct_count }} 行）· 合计
          <b class="money">{{ formatMoney(summary.total_amount) }}</b>
        </span>
        <span class="muted">合计只统计「当前页」（列表是服务端分页）</span>
      </div>
    </t-card>

    <!-- ==================== ③ 新增 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      header="新增费用台账"
      width="720px"
      :confirm-btn="{ content: '提交', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      data-testid="co-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-alert
        v-if="formError"
        theme="error"
        :message="formError"
        class="form-alert"
        data-testid="co-form-error"
      />

      <t-form label-width="112px" :data="form" data-testid="co-form">
        <div class="form-grid">
          <t-form-item label="会计期间" :status="formStatus('period_code')" :help="formErrors.period_code">
            <t-date-picker
              v-model="form.period_code"
              mode="month"
              value-type="YYYY-MM"
              format="YYYY-MM"
              placeholder="yyyy-MM"
              data-testid="co-form-period"
            />
          </t-form-item>
          <t-form-item label="车间" :status="formStatus('dept_id')" :help="formErrors.dept_id">
            <DeptTreeSelect v-model="form.dept_id" data-testid="co-form-dept" />
          </t-form-item>
          <t-form-item label="费用项目" :status="formStatus('expense_item')" :help="formErrors.expense_item">
            <DictSelect
              v-model="form.expense_item"
              :dict-code="EXPENSE_ITEM_DICT_CODE"
              data-testid="co-form-item"
            />
          </t-form-item>
          <t-form-item label="成本对象" :help="'空 = 车间费用池（等分摊）；选了 = 直接计入该批'">
            <t-select
              v-model="form.cost_object_id"
              :options="objectOptions"
              :loading="optionsLoading"
              placeholder="不选 = 车间费用池"
              clearable
              filterable
              data-testid="co-form-object"
            />
          </t-form-item>
          <t-form-item
            label="借方科目"
            :status="formStatus('debit_account_code')"
            :help="formErrors.debit_account_code"
          >
            <t-select
              v-model="form.debit_account_code"
              :options="debitOptions"
              :loading="optionsLoading"
              placeholder="4101.* 启用末级"
              filterable
              data-testid="co-form-debit"
            />
          </t-form-item>
          <t-form-item
            label="对方科目"
            :status="formStatus('credit_account_code')"
            :help="formErrors.credit_account_code"
          >
            <t-select
              v-model="form.credit_account_code"
              :options="creditOptions"
              :loading="optionsLoading"
              placeholder="银行 / 应付 / 累计折旧…"
              filterable
              data-testid="co-form-credit"
            />
          </t-form-item>
          <t-form-item label="金额" :status="formStatus('amount')" :help="formErrors.amount">
            <t-input v-model="form.amount" placeholder="> 0，最多 4 位小数" data-testid="co-form-amount" />
          </t-form-item>
        </div>

        <t-form-item label="备注" :status="formStatus('remark')" :help="formErrors.remark">
          <t-input v-model="form.remark" :maxlength="255" placeholder="可空" data-testid="co-form-remark" />
        </t-form-item>

        <p class="field-hint" data-testid="co-form-hint">
          借方科目必须是 <b>4101.*</b> 制造费用科目，且是账套里<b>存在 + 末级 + 启用</b>的；
          不填按缺省 <b>{{ DEFAULT_DEBIT_ACCOUNT }}</b>（制造费用-其他）。科目不对后端会 400 + 66060 并点名账套现状。
        </p>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  createCostOverhead,
  deleteCostOverhead,
  fetchCostOverheadPage,
  COST_OVERHEAD_PERMISSIONS,
  DEFAULT_DEBIT_ACCOUNT,
  PAGE_SIZE,
  type CostOverheadPageQuery,
  type CostOverheadRow,
} from '@/api/business/erp/cost-overhead';
import { fetchCostObjectPage } from '@/api/business/erp/cost-object';
import { fetchAccountOptions } from '@/api/business/erp';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import DictSelect from '@/components/DictSelect.vue';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { formatMoney, shortId, todayPeriod } from '@/views/business/erp/shared/money';
import { errorText } from '@/views/business/erp/shared/error-text';
import {
  EXPENSE_ITEM_DICT_CODE,
  emptyOverheadForm,
  errorCount,
  expenseItemLabel,
  isPoolRow,
  pickCreditAccounts,
  pickManufacturingAccounts,
  summarizeRows,
  toCreatePayload,
  validateOverheadForm,
  type OverheadFormErrors,
  type OverheadFormState,
} from './cost-overhead-logic';

/** 本页权限码（V0378 种子逐一对应；**没有 update**：设计只给了 3 个接口） */
const PERM = COST_OVERHEAD_PERMISSIONS;

// ==================== 列表 ====================

const rows = ref<CostOverheadRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<{
  period_code: string;
  dept_id: string;
  expense_item: string;
  keyword: string;
}>({ period_code: '', dept_id: '', expense_item: '', keyword: '' });

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽合计 ≈ 1000px：1280 视口（侧栏展开）不会被固定「操作」列压住金额列
const columns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 84 },
  { colKey: 'dept_name', title: '车间', width: 104, ellipsis: true },
  { colKey: 'expense_item', title: '费用项目', width: 100, ellipsis: true },
  { colKey: 'cost_object_id', title: '归属', width: 150, ellipsis: true },
  { colKey: 'debit_account_code', title: '借方科目', width: 168, ellipsis: true },
  { colKey: 'credit_account_code', title: '对方科目', width: 168, ellipsis: true },
  { colKey: 'amount', title: '金额', width: 120, align: 'right' },
  { colKey: 'remark', title: '备注', ellipsis: true },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right' },
];

const summary = computed(() => summarizeRows(rows.value));

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const query: CostOverheadPageQuery = {
      page: current.value,
      page_size: PAGE_SIZE,
      period_code: filter.period_code,
      dept_id: filter.dept_id,
      expense_item: filter.expense_item,
      keyword: filter.keyword.trim(),
    };
    const page = await fetchCostOverheadPage(query);
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    loadError.value = errorText(error, '加载失败');
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
  filter.period_code = '';
  filter.dept_id = '';
  filter.expense_item = '';
  filter.keyword = '';
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 下拉主数据 ====================

const deptTree = ref<DeptNode[]>([]);
const objectRows = ref<Array<{ id: string; object_no: string; product_name: string | null }>>([]);
const accounts = ref<Array<{ account_code: string; account_name: string; is_leaf: boolean; status: number }>>(
  [],
);
const optionsLoading = ref(false);

const deptOptions = computed(() => flattenDeptTree(deptTree.value));
const debitOptions = computed(() => pickManufacturingAccounts(accounts.value));
const creditOptions = computed(() => pickCreditAccounts(accounts.value));
const objectOptions = computed(() =>
  objectRows.value.map((row) => ({
    label: `${row.object_no} · ${row.product_name ?? '产出品未知'}`,
    value: row.id,
  })),
);

function flattenDeptTree(nodes: DeptNode[], depth = 0): Array<{ label: string; value: string }> {
  const out: Array<{ label: string; value: string }> = [];
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
    const children = node.children ?? [];
    if (children.length) {
      out.push(...flattenDeptTree(children, depth + 1));
    }
  }
  return out;
}

async function loadOptions(): Promise<void> {
  optionsLoading.value = true;
  try {
    const [tree, objectPage, accountList] = await Promise.all([
      getDeptTree({ status: 1 }),
      // 直接计入只能挂「在产」对象（已关批的对象不再接受新的成本归集）
      fetchCostObjectPage({ status: 'OPEN', page: 1, page_size: 200 }),
      fetchAccountOptions(500),
    ]);
    deptTree.value = tree;
    objectRows.value = objectPage.list ?? [];
    accounts.value = accountList;
  } catch (error) {
    MessagePlugin.warning(errorText(error, '下拉主数据加载失败'));
  } finally {
    optionsLoading.value = false;
  }
}

// ==================== 表单 ====================

const formVisible = ref(false);
const submitting = ref(false);
const formError = ref('');
const formErrors = ref<OverheadFormErrors>({});
const form = reactive<OverheadFormState>(emptyOverheadForm(todayPeriod()));

function formStatus(field: keyof OverheadFormState): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

function openCreate(): void {
  Object.assign(form, emptyOverheadForm(filter.period_code || todayPeriod()));
  formErrors.value = {};
  formError.value = '';
  formVisible.value = true;
}

function closeForm(): void {
  formVisible.value = false;
  formErrors.value = {};
  formError.value = '';
}

async function submitForm(): Promise<void> {
  formError.value = '';
  const errors = validateOverheadForm(form);
  formErrors.value = errors;
  if (errorCount(errors) > 0) {
    formError.value = '还有必填项没填对，请看下方标红项';
    return;
  }
  submitting.value = true;
  try {
    const created = await createCostOverhead(toCreatePayload(form));
    MessagePlugin.success(
      isPoolRow(created)
        ? `已录入 ${created.amount} 元到车间池（${created.dept_name ?? ''}），当期分摊时按规则摊出去`
        : `已录入 ${created.amount} 元，直接计入 ${created.cost_object_no ?? '该成本对象'}`,
    );
    formVisible.value = false;
    closeForm();
    await loadRows();
  } catch (error) {
    // 66060（科目不合法）/ 66061（金额不合法）的报文里已点名账套现状与"怎么修"
    formError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

// ==================== 删除 ====================

async function handleDelete(row: CostOverheadRow): Promise<void> {
  const target = isPoolRow(row) ? '车间费用池' : (row.cost_object_no ?? '该成本对象');
  const confirmed = await new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: '删除费用台账行',
      body: `删除 ${row.period_code} ${row.dept_name ?? ''} ${expenseItemLabel(row.expense_item, row.expense_item_label)} ${formatMoney(row.amount)} 元（归属：${target}）？已被当期成本归集引用时会拒绝删除。`,
      theme: 'warning',
      confirmBtn: '删除',
      onConfirm: () => {
        dialog.hide();
        resolve(true);
      },
      onClose: () => resolve(false),
      onCancel: () => resolve(false),
    });
  });
  if (!confirmed) {
    return;
  }
  try {
    await deleteCostOverhead(row.id);
    MessagePlugin.success('已删除');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error, '删除失败'));
  }
}

onMounted(() => {
  void loadOptions();
  void loadRows();
});
</script>

<style scoped>
.cost-overhead-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.list-card {
  border-radius: 8px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__period {
  width: 132px;
}

.tool-row__dept {
  width: 150px;
}

.tool-row__item {
  width: 150px;
}

.tool-row__keyword {
  width: 210px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.panel-head__meta {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.list-alert,
.form-alert {
  margin-bottom: 8px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}

.num,
.money {
  font-variant-numeric: tabular-nums;
}

.muted {
  margin-left: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.pool-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.pool-tag .muted {
  margin-left: 0;
}

.summary-row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-top: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 12px;
}

.field-hint {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.5;
}
</style>
