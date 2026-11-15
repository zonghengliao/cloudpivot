<!--
  分摊规则页（COST-B2-13 / ARCH-9 §6.2.3 · §4.4 · §5.2 #20–#24）。

  一页三件事：**列表（期间/车间/标准/状态/关键字）→ 规则表单（版本化）→ 试算弹窗**。

  # 试算弹窗（本卡验收项）

  选期间 → `POST /cost-alloc-rule/preview` → 逐规则逐对象显示
  「待分摊池 / 各对象基数 / 分摊额 / 尾差」，**不落库**。
  每组表尾打勾表示 **Σ分摊额 == 池金额**（守恒可视化，设计 §6.2.3 原文）。
  未分摊池（有池但没规则覆盖 / 反查不到科目）单独列在底部，**不静默吞**。

  # 两条硬错误不吞

  池里有钱但当期没有任何在产对象 ⇒ `409 + 66062`；该标准基数合计为 0 ⇒ `409 + 66063`。
  报文已点名车间、科目、金额，页面原样展示（再补一句"怎么修"）。这两种情况
  如果只显示"试算完成、摊 0"，用户会以为算完了，结算时 4101 余额清不掉。

  # 版本化（改规则不溯及历史）

  规则是 `(车间, 科目)` 的版本序列，`effective_from` 更晚的赢。11 月改口径 =
  **新建一条 11-01 生效的规则**。已参与过分摊的版本只允许改名称/备注/状态（后端 409）。
-->
<template>
  <div class="cost-alloc-rule-page">
    <!-- ==================== ①② 筛选 / 分页 / 勾选 / 行操作交给 ListPage（P3-09） ==================== -->
    <ListPage
      ref="listRef"
      title="分摊规则"
      testid="car"
      table-testid="car-table"
      meta-testid="car-meta"
      btn-prefix="car-btn"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      selectable
      @loaded="onLoaded"
      @selection-change="onSelectionChange"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="car-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增规则
        </t-button>
        <t-button
          v-privilege="[PERM.preview]"
          theme="warning"
          variant="outline"
          data-testid="car-btn-preview"
          @click="openPreview"
        >
          <template #icon><t-icon name="calculate" /></template>
          试算
        </t-button>
      </template>

      <template #head-extra>
        <span class="panel-head__tip">
          一条规则 = 「某车间的某制造费用科目，按某标准摊给该车间当期在产的对象」；
          改口径请新建生效期更晚的版本（不溯及历史）
        </span>
      </template>

      <!-- 生效覆盖期间：`t-date-picker mode="month"` 走 custom 插槽自绘 -->
      <template #filter-period_code>
        <t-date-picker
          v-model="filter.period_code"
          class="tool-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="生效覆盖期间"
          clearable
          data-testid="car-filter-period"
          @change="handleSearch"
        />
      </template>

      <template #rule_code="{ row }">
        <span class="mono" :data-testid="`car-code-${row.rule_code}`">{{ row.rule_code }}</span>
      </template>
      <template #rule_name="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ row.rule_name }}</span>
          <span class="name-cell__sub">{{ row.remark || '—' }}</span>
        </div>
      </template>
      <template #source_dept_name="{ row }">
        {{ row.source_dept_name || shortId(row.source_dept_id) }}
      </template>
      <template #account_code="{ row }">
        <span class="mono">{{ row.account_code }}</span> {{ row.account_name || '' }}
      </template>
      <template #basis="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ row.basis_label || basisLabel(row.basis) }}</span>
          <span class="name-cell__sub">{{ row.basis }}</span>
        </div>
      </template>
      <template #effective="{ row }">{{ effectiveRangeText(row) }}</template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="row.status === 1 ? 'success' : 'default'">
          {{ row.status_label || statusLabel(row.status) }}
        </t-tag>
      </template>
      <template #op="{ row }">
        <div class="op-cell">
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="car-btn-edit"
            @click="openEdit(row)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="car-btn-toggle"
            @click="toggleStatus(row)"
          >
            {{ row.status === 1 ? '停用' : '启用' }}
          </t-link>
          <t-link
            v-privilege="[PERM.preview]"
            theme="primary"
            data-testid="car-btn-preview-row"
            @click="openPreviewFor(row)"
          >
            试算
          </t-link>
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            data-testid="car-btn-delete"
            @click="handleDelete(row)"
          >
            删除
          </t-link>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ③ 规则表单 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑分摊规则' : '新增分摊规则'"
      width="760px"
      :confirm-btn="{ content: editingId ? '保存' : '提交', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      data-testid="car-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-alert
        v-if="formError"
        theme="error"
        :message="formError"
        class="form-alert"
        data-testid="car-form-error"
      />
      <t-alert
        v-if="editingId"
        theme="info"
        message="已参与过分摊的版本只允许改「规则名 / 备注 / 状态」；要改分配标准或科目，请新建一条生效期更晚的版本。"
        class="form-alert"
        data-testid="car-form-version-tip"
      />

      <t-form label-width="112px" :data="form" data-testid="car-form">
        <div class="form-grid">
          <t-form-item label="规则编码" :status="formStatus('rule_code')" :help="formErrors.rule_code">
            <t-input
              v-model="form.rule_code"
              placeholder="如 DEPT1_LABOR_20261101（大写）"
              :maxlength="32"
              data-testid="car-form-code"
            />
          </t-form-item>
          <t-form-item label="规则名" :status="formStatus('rule_name')" :help="formErrors.rule_name">
            <t-input
              v-model="form.rule_name"
              placeholder="如 一车间制造费用按工时分摊"
              :maxlength="120"
              data-testid="car-form-name"
            />
          </t-form-item>
          <t-form-item label="被摊车间" :status="formStatus('dept_id')" :help="formErrors.dept_id">
            <DeptTreeSelect v-model="form.dept_id" data-testid="car-form-dept" />
          </t-form-item>
          <t-form-item label="被摊科目" :status="formStatus('account_code')" :help="formErrors.account_code">
            <t-select
              v-model="form.account_code"
              :options="accountOptions"
              :loading="optionsLoading"
              placeholder="只列 4101.* 启用末级"
              filterable
              data-testid="car-form-account"
            />
          </t-form-item>
          <t-form-item label="分配标准" :status="formStatus('basis')" :help="formErrors.basis">
            <t-select v-model="form.basis" :options="basisSelectOptions" data-testid="car-form-basis" />
          </t-form-item>
          <t-form-item label="优先级" :status="formStatus('priority')" :help="formErrors.priority">
            <t-input v-model="form.priority" placeholder="1~999，缺省 10" data-testid="car-form-priority" />
          </t-form-item>
          <t-form-item
            label="生效开始日"
            :status="formStatus('effective_from')"
            :help="formErrors.effective_from"
          >
            <t-date-picker
              v-model="form.effective_from"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              placeholder="默认当月 1 日"
              data-testid="car-form-from"
            />
          </t-form-item>
          <t-form-item label="生效结束日" :status="formStatus('effective_to')" :help="formErrors.effective_to">
            <t-date-picker
              v-model="form.effective_to"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              placeholder="不填 = 长期有效"
              clearable
              data-testid="car-form-to"
            />
          </t-form-item>
          <t-form-item label="状态">
            <t-switch
              :value="form.status === 1"
              data-testid="car-form-status"
              @change="(value: unknown) => (form.status = value ? 1 : 0)"
            />
            <span class="muted">{{ form.status === 1 ? '启用' : '停用（不参与试算与分摊）' }}</span>
          </t-form-item>
        </div>

        <t-form-item label="备注" :status="formStatus('remark')" :help="formErrors.remark">
          <t-input v-model="form.remark" :maxlength="255" placeholder="可空" data-testid="car-form-remark" />
        </t-form-item>

        <p class="field-hint" data-testid="car-form-hint">
          同 (车间, 科目, 生效日) 只能有一条版本；11 月改口径就新建一条 11-01 生效的规则，
          10 月重算仍拿旧版本 —— 这是「改规则不溯及历史」的落地方式。
        </p>
      </t-form>
    </t-dialog>

    <!-- ==================== ④ 试算弹窗 ==================== -->
    <t-dialog
      v-model:visible="previewVisible"
      header="分摊试算（只算不写）"
      width="960px"
      :footer="false"
      :close-on-overlay-click="false"
      data-testid="car-preview-dialog"
    >
      <div class="preview-toolbar">
        <t-date-picker
          v-model="previewPeriod"
          class="preview-toolbar__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="试算期间"
          data-testid="car-preview-period"
        />
        <t-checkbox
          v-if="!previewRuleId"
          v-model="previewSelectedOnly"
          :disabled="!selectedIds.length"
          data-testid="car-preview-selected"
        >
          只算本页勾选的 {{ selectedIds.length }} 条规则
        </t-checkbox>
        <span v-else class="muted" data-testid="car-preview-row-scope">
          只算 {{ previewRuleCode }} 这一条规则
        </span>
        <t-button
          theme="primary"
          :loading="previewLoading"
          data-testid="car-preview-run"
          @click="runPreview"
        >
          开始试算
        </t-button>
        <span class="muted">不落库，可以反复点</span>
      </div>

      <t-alert
        v-if="previewError"
        theme="error"
        :message="previewError"
        class="form-alert"
        data-testid="car-preview-error"
      />

      <template v-if="preview">
        <t-alert
          :theme="previewConserved(preview) ? 'success' : 'error'"
          :message="previewSummaryText(preview)"
          class="form-alert"
          data-testid="car-preview-summary"
        />

        <t-alert
          v-if="previewEmpty(preview)"
          theme="info"
          message="该期间没有可分摊的池、也没有生效规则：要么当期还没归集制造费用，要么规则都停用了。"
          data-testid="car-preview-empty"
        />

        <div
          v-for="group in preview.groups"
          :key="group.rule_id"
          class="preview-group"
          :data-testid="`car-preview-group-${group.rule_code}`"
        >
          <div class="preview-group__head">
            <div class="preview-group__title">
              <span class="mono">{{ group.rule_code }}</span>
              <span>{{ group.rule_name }}</span>
              <t-tag size="small" variant="light" theme="primary">{{ group.basis_label }}</t-tag>
            </div>
            <div class="preview-group__meta">
              {{ group.source_dept_name }} · 科目
              <span class="mono">{{ group.account_code }}</span>
              {{ group.account_name || '' }}
            </div>
          </div>

          <t-table
            row-key="cost_object_id"
            size="small"
            :data="group.rows"
            :columns="previewColumns"
            data-testid="car-preview-rows"
          >
            <template #cost_object_no="{ row }">
              <span class="mono">{{ row.cost_object_no }}</span>
            </template>
            <template #cost_object_status="{ row }">
              <span class="muted">{{ row.cost_object_status === 'OPEN' ? '在产' : row.cost_object_status }}</span>
            </template>
            <template #basis_value="{ row }"><span class="num">{{ row.basis_value }}</span></template>
            <template #share="{ row }">{{ basisSharePercent(row, group) }}</template>
            <template #alloc_amount="{ row }">
              <span class="money">{{ formatMoney(row.alloc_amount) }}</span>
            </template>
            <template #rounding_adj="{ row }">
              <span :class="{ 'num--warn': row.rounding_adj !== '0.0000' && row.rounding_adj !== '0' }">
                {{ row.rounding_adj }}
              </span>
            </template>
          </t-table>

          <div class="preview-group__foot" :data-testid="`car-preview-foot-${group.rule_code}`">
            <span>
              池 <b class="money">{{ formatMoney(group.pool_amount) }}</b>
              · 基数合计 <b class="num">{{ group.basis_total }}</b>
              · 分摊合计 <b class="money">{{ formatMoney(group.alloc_total) }}</b>
              · 尾差 <b class="num">{{ group.rounding_total }}</b>
            </span>
            <span class="conserved" :class="{ 'conserved--bad': !groupConserved(group) }">
              <t-icon :name="groupConserved(group) ? 'check-circle-filled' : 'close-circle-filled'" />
              {{ groupConserved(group) ? '合计 = 池（守恒）' : '不守恒，请把结果发给开发' }}
            </span>
          </div>
          <p v-if="roundingRows(group).length" class="preview-group__rounding">
            尾差落在：
            <span v-for="item in roundingRows(group)" :key="item.cost_object_no" class="mono">
              {{ item.cost_object_no }}（{{ item.rounding_adj }}）
            </span>
            —— 最大余数法保证「余额不丢」，尾差只落一行，别手工改数
          </p>
        </div>

        <div v-if="preview.unallocated.length" class="preview-unallocated" data-testid="car-preview-unallocated">
          <h4>未分摊池（{{ preview.unallocated.length }} 组）</h4>
          <t-table
            row-key="source_dept_id"
            size="small"
            :data="preview.unallocated"
            :columns="unallocatedColumns"
          >
            <template #amount="{ row }">
              <span class="money">{{ formatMoney(row.pool_amount) }}</span>
            </template>
          </t-table>
        </div>
      </template>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  createCostAllocRule,
  deleteCostAllocRule,
  fetchCostAllocRulePage,
  previewCostAllocation,
  updateCostAllocRule,
  ALLOC_BASES,
  ALLOC_RULE_DISABLED,
  ALLOC_RULE_ACTIVE,
  COST_ALLOC_RULE_PERMISSIONS,
  PAGE_SIZE,
  type AllocBasis,
  type CostAllocPreviewRes,
  type CostAllocRuleRow,
} from '@/api/business/erp/cost-alloc-rule';
import { fetchAccountOptions } from '@/api/business/erp';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQueryBuilder,
} from '@/components/ListPage/types';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { formatMoney, shortId, todayString, todayPeriod } from '@/views/business/erp/shared/money';
import { errorCodeOf, errorText } from '@/views/business/erp/shared/error-text';
import { pickManufacturingAccounts } from '@/views/business/erp/shared/manufacturing-account';
import {
  RULE_LOCKED_HINT,
  basisLabel,
  basisOptions,
  basisSharePercent,
  effectiveRangeText,
  emptyRuleForm,
  errorCount,
  formFromRow,
  groupConserved,
  isEmptyPayload,
  previewConserved,
  previewEmpty,
  previewSummaryText,
  roundingRows,
  statusLabel,
  toCreatePayload,
  toUpdatePayload,
  validateRuleForm,
  type RuleFormErrors,
  type RuleFormState,
} from './cost-alloc-rule-logic';

/** 本页权限码（V0378 种子逐一对应） */
const PERM = COST_ALLOC_RULE_PERMISSIONS;

// ==================== 列表（列 / 筛选 / 勾选 / 拉页交给 ListPage，P3-09） ====================

const filter = reactive<{
  period_code: string;
  dept_id: string;
  basis: AllocBasis | '';
  status: number | '';
  keyword: string;
}>({ period_code: '', dept_id: '', basis: '', status: '', keyword: '' });

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const statusFilterOptions = [
  { label: statusLabel(ALLOC_RULE_ACTIVE), value: ALLOC_RULE_ACTIVE },
  { label: statusLabel(ALLOC_RULE_DISABLED), value: ALLOC_RULE_DISABLED },
];

const basisFilterOptions = ALLOC_BASES.map((value) => ({ label: basisLabel(value), value }));
const basisSelectOptions = basisOptions();

// 列宽合计 ≈ 1136px：1440 视口（内容区约 1170px）不横向溢出（第一版 1292px 会把操作列挤出去）
// 勾选列由 ListPage 补（`selectable`），这里不再手声明 `row-select`
const columns: ColumnDef[] = [
  { colKey: 'rule_code', title: '编码', width: 140, ellipsis: true },
  { colKey: 'rule_name', title: '规则名 / 备注', width: 170, ellipsis: true },
  { colKey: 'source_dept_name', title: '车间', width: 90, ellipsis: true },
  { colKey: 'account_code', title: '被摊科目', width: 150, ellipsis: true },
  { colKey: 'basis', title: '分配标准', width: 150, ellipsis: true },
  { colKey: 'effective', title: '生效期', width: 150 },
  { colKey: 'status', title: '状态', width: 70 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right', exportable: false },
];

const previewColumns: PrimaryTableCol[] = [
  { colKey: 'cost_object_no', title: '成本对象', width: 160 },
  { colKey: 'cost_object_status', title: '状态', width: 70 },
  { colKey: 'basis_value', title: '基数', width: 110, align: 'right' },
  { colKey: 'share', title: '占比', width: 80, align: 'right' },
  { colKey: 'alloc_amount', title: '分摊额', width: 130, align: 'right' },
  { colKey: 'rounding_adj', title: '尾差', width: 90, align: 'right' },
];

const unallocatedColumns: PrimaryTableCol[] = [
  { colKey: 'source_dept_name', title: '车间', width: 140 },
  { colKey: 'account_code', title: '科目', width: 180 },
  { colKey: 'amount', title: '未分摊金额', width: 140, align: 'right' },
  { colKey: 'reason', title: '为什么没摊' },
];

/** 本页勾选（试算弹窗「只算勾选的 N 条」用；清勾选由 ListPage 在换页/查询时抛空数组） */
const selectedIds = ref<string[]>([]);

function onSelectionChange(ids: string[]): void {
  selectedIds.value = ids;
}

/**
 * 声明式筛选（顺序即渲染顺序）。
 *
 * 「生效覆盖期间」是 `custom`：`t-date-picker mode="month"` 不是声明式控件。
 */
const filters = computed<FilterDef[]>(() => [
  { key: 'period_code', type: 'custom', testid: 'car-filter-period' },
  {
    key: 'dept_id',
    type: 'select',
    options: deptOptions.value,
    filterable: true,
    searchOnChange: true,
    placeholder: '全部车间',
    width: 140,
    testid: 'car-filter-dept',
  },
  {
    key: 'basis',
    type: 'select',
    options: basisFilterOptions,
    searchOnChange: true,
    placeholder: '全部分配标准',
    width: 160,
    testid: 'car-filter-basis',
  },
  {
    key: 'status',
    type: 'select',
    options: statusFilterOptions,
    searchOnChange: true,
    placeholder: '全部状态',
    width: 110,
    testid: 'car-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '编码 / 名称 / 科目 / 备注',
    width: 200,
    testid: 'car-filter-keyword',
  },
]);

/** 拉一页 */
const fetcher: ListFetcher<CostAllocRuleRow> = (query) =>
  fetchCostAllocRulePage(query as Parameters<typeof fetchCostAllocRulePage>[0]);

/** 入参拼装（与改造前 `loadRows` 里那段逐字一致） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as {
    period_code: string;
    dept_id: string;
    basis: AllocBasis | '';
    status: number | '';
    keyword: string;
  };
  return {
    page,
    page_size: pageSize,
    period_code: value.period_code,
    dept_id: value.dept_id,
    basis: value.basis as '' | undefined,
    status: value.status === '' ? undefined : value.status,
    keyword: value.keyword.trim(),
  };
};

/**
 * 一页拉完：清勾选。
 *
 * 改造前 `loadRows` 每次都会 `selectedIds = []`（换页/改状态后不会带着旧 id 试算），
 * 这里保持同一口径：ListPage 只管渲染，清勾选的动作仍在页面里做。
 */
function onLoaded(): void {
  listRef.value?.clearSelection();
  selectedIds.value = [];
}

/** 重拉列表（不传 = 当前页）；名字保持不变，页面里 4 处调用点不用改 */
async function loadRows(page?: number): Promise<void> {
  if (page === undefined) {
    await listRef.value?.reload();
    return;
  }
  await listRef.value?.load(page);
}

/** 筛选项值一变就查（原来 `@change="handleSearch"` 的手感：回第 1 页） */
function handleSearch(): void {
  void listRef.value?.search();
}

// ==================== 下拉主数据 ====================

const deptTree = ref<DeptNode[]>([]);
const accounts = ref<Array<{ account_code: string; account_name: string; is_leaf: boolean; status: number }>>(
  [],
);
const optionsLoading = ref(false);

const deptOptions = computed(() => flattenDeptTree(deptTree.value));
const accountOptions = computed(() => pickManufacturingAccounts(accounts.value));

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
    const [tree, accountList] = await Promise.all([getDeptTree({ status: 1 }), fetchAccountOptions(500)]);
    deptTree.value = tree;
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
const formErrors = ref<RuleFormErrors>({});
const editingId = ref('');
const editingOrigin = ref<CostAllocRuleRow | null>(null);
const form = reactive<RuleFormState>(emptyRuleForm(todayString()));

function formStatus(field: keyof RuleFormState): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

function resetFormState(): void {
  Object.assign(form, emptyRuleForm(todayString(), filter.dept_id));
  formErrors.value = {};
  formError.value = '';
  editingOrigin.value = null;
}

function openCreate(): void {
  editingId.value = '';
  resetFormState();
  formVisible.value = true;
}

function openEdit(row: CostAllocRuleRow): void {
  editingId.value = row.id;
  resetFormState();
  editingOrigin.value = row;
  Object.assign(form, formFromRow(row));
  formVisible.value = true;
}

function closeForm(): void {
  formVisible.value = false;
  resetFormState();
  editingId.value = '';
}

async function submitForm(): Promise<void> {
  formError.value = '';
  const errors = validateRuleForm(form);
  formErrors.value = errors;
  if (errorCount(errors) > 0) {
    formError.value = '还有必填项没填对，请看下方标红项';
    return;
  }
  submitting.value = true;
  try {
    if (editingId.value) {
      const origin = editingOrigin.value;
      if (!origin) {
        formError.value = '原规则没加载成功，请关闭后重试';
        return;
      }
      const payload = toUpdatePayload(origin, form);
      if (isEmptyPayload(payload)) {
        MessagePlugin.info('没有改动');
        return;
      }
      const saved = await updateCostAllocRule(editingId.value, payload);
      MessagePlugin.success(`已保存 ${saved.rule_code}`);
    } else {
      const created = await createCostAllocRule(toCreatePayload(form));
      MessagePlugin.success(`已新建 ${created.rule_code}（${created.effective_from} 起生效）`);
    }
    formVisible.value = false;
    resetFormState();
    editingId.value = '';
    await loadRows();
  } catch (error) {
    // 409：已参与分摊的版本改了口径 → 报文之外再补"新建版本"的引导
    const code = errorCodeOf(error);
    formError.value = code === 409 ? `${errorText(error)}\n\n${RULE_LOCKED_HINT}` : errorText(error);
  } finally {
    submitting.value = false;
  }
}

// ==================== 停用 / 启用 ====================

async function toggleStatus(row: CostAllocRuleRow): Promise<void> {
  const target = row.status === ALLOC_RULE_ACTIVE ? ALLOC_RULE_DISABLED : ALLOC_RULE_ACTIVE;
  try {
    const saved = await updateCostAllocRule(row.id, { status: target });
    MessagePlugin.success(`${saved.rule_code} 已${target === ALLOC_RULE_ACTIVE ? '启用' : '停用'}`);
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error, '改状态失败'));
  }
}

// ==================== 删除 ====================

async function handleDelete(row: CostAllocRuleRow): Promise<void> {
  const confirmed = await new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: '删除分摊规则',
      body: `删除 ${row.rule_code}（${row.rule_name}）？已参与过分摊的版本会被拒绝删除 —— 那样会让历史期间的分摊结果找不到来源。只停用不删除是更安全的选择。`,
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
    await deleteCostAllocRule(row.id);
    MessagePlugin.success('已删除');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error, '删除失败'));
  }
}

// ==================== 试算 ====================

const previewVisible = ref(false);
const previewLoading = ref(false);
const previewError = ref('');
const preview = ref<CostAllocPreviewRes | null>(null);
const previewPeriod = ref(todayPeriod());
const previewSelectedOnly = ref(false);
/**
 * 行上点「试算」时的「只算这一条」范围。
 *
 * 单独一个 ref 而不是写 `selectedIds = [row.id]`：勾选状态现在归 ListPage 管，
 * 页面改本地数组不会让表格的勾选框跟着变 —— 那会出现「弹窗说只算 1 条、
 * 表格勾着 3 条」的矛盾。行上的意图是「就这一条」，所以直接按 id 算，不动勾选。
 */
const previewRuleId = ref('');
const previewRuleCode = ref('');

function openPreview(): void {
  previewVisible.value = true;
  previewError.value = '';
  preview.value = null;
  previewPeriod.value = filter.period_code || todayPeriod();
  previewSelectedOnly.value = false;
  previewRuleId.value = '';
  previewRuleCode.value = '';
}

/** 行上点「试算」：直接把期间定为该规则生效月，并只算这一条 */
function openPreviewFor(row: CostAllocRuleRow): void {
  previewVisible.value = true;
  previewError.value = '';
  preview.value = null;
  previewPeriod.value = row.effective_from.slice(0, 7);
  previewRuleId.value = row.id;
  previewRuleCode.value = row.rule_code;
  previewSelectedOnly.value = true;
}

async function runPreview(): Promise<void> {
  previewError.value = '';
  previewLoading.value = true;
  try {
    const req: { period_code: string; rule_ids?: string[] } = { period_code: previewPeriod.value };
    // 行上进来 ⇒ 只算那一条；工具栏进来 ⇒ 看「只算勾选」勾没勾
    const onlyIds = previewRuleId.value ? [previewRuleId.value] : selectedIds.value;
    if (previewSelectedOnly.value && onlyIds.length) {
      req.rule_ids = [...onlyIds];
    }
    preview.value = await previewCostAllocation(req);
  } catch (error) {
    preview.value = null;
    const code = errorCodeOf(error);
    const hint =
      code === 66062
        ? '当期确实没有在产成本对象：请先建成本对象 / 录领料与工时，再回来试算。'
        : code === 66063
          ? '这个分配标准的基数是 0（例如「按机器工时」但当期没人填机器工时）：换标准，或补录基数数据。'
          : code === 66060
            ? '被摊科目不合法（必须 4101.* 且账套里存在 + 末级 + 启用）。'
            : '';
    previewError.value = hint ? `${errorText(error)}\n\n${hint}` : errorText(error);
  } finally {
    previewLoading.value = false;
  }
}

onMounted(() => {
  void loadOptions();
  void loadRows();
});
</script>

<style scoped>
.cost-alloc-rule-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row__period {
  width: 140px;
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

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.name-cell__main {
  font-weight: 500;
}

.name-cell__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.num,
.money {
  font-variant-numeric: tabular-nums;
}

.num--warn {
  color: var(--td-warning-color);
  font-weight: 600;
}

.muted {
  margin-left: 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 8px;
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

.preview-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 8px;
}

.preview-toolbar__period {
  width: 150px;
}

.preview-group {
  margin-bottom: 16px;
  padding: 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
}

.preview-group__head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

.preview-group__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
}

.preview-group__meta {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.preview-group__foot {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-top: 8px;
  font-size: 12px;
}

.conserved {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--td-success-color);
  font-weight: 600;
}

.conserved--bad {
  color: var(--td-error-color);
}

.preview-group__rounding {
  margin: 6px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.preview-unallocated h4 {
  margin: 8px 0;
  font-size: 14px;
}
</style>
