<!--
  报表项维护页（卡 P1-FIX-8B / 后端 `report_item_admin.rs`）。

  `biz_report_item` 是资产负债表 + 利润表**共用的唯一取数配置**：53 行 = 报表上的 53 个
  项目。这张页之前不存在，报表项只能改库 —— 于是「应收账款取哪些科目」「净利润的公式对不对」
  这类问题只能找研发。本页把它变成业务自己能维护的配置面：列表 / 新增 / 编辑 / 排序 / 启停。

  # 三个不显眼但很关键的交互决定

  1. **改完立即生效，不需要发版也不需要刷新缓存**：报表接口每次编制都现查这张表
     （`report::load_items_of`），页头那句提示就是让使用者敢改。
  2. **编码只读**：`item_code` 是公式的引用键（`NET_PROFIT = TOTAL_PROFIT - INCOME_TAX`），
     改名会让所有引用变成死链，所以后端 400、前端渲染成只读并把理由写在下拉旁边。
  3. **排序用「整表顺序」提交**：公式只能引用排在自己**前面**的项，局部交换无法判断
     「换完之后还有没有公式引用落到后面」，所以 ↑↓ 一次就把整表顺序传给后端，
     后端整表校验通过才写库。

  # 取数模式 / 计算式二选一

  一行要么按科目取数（`account_pattern`）、要么引用其它项（`formula`），不能都填也不能
  都不填 —— 例外是层级 1 的大类标题行（「流动资产」「非流动资产」），金额恒为空。
  表单用单选切换三种形态：科目取数 / 引用其它项 / 纯标题行。
-->
<template>
  <div class="ri-page">
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-radio-group v-model="activeReport" variant="default-filled" data-testid="ri-report-tabs">
          <t-radio-button v-for="report in catalog.reports" :key="report.report_code" :value="report.report_code">
            {{ report.report_name }}
            <span class="ri-tab-count">{{ report.item_count }}</span>
          </t-radio-button>
        </t-radio-group>

        <span class="tool-row__spacer" />

        <span class="ri-total" data-testid="ri-total">共 {{ rows.length }} 项</span>
        <t-button
          v-privilege="[PERM.manage]"
          theme="primary"
          data-testid="ri-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增报表项
        </t-button>
        <t-button variant="outline" data-testid="ri-btn-refresh" @click="refresh">刷新</t-button>
      </div>

      <div class="tool-row tool-row--filter">
        <t-input
          v-model="keyword"
          class="ri-filter-keyword"
          placeholder="编码 / 名称（模糊）"
          clearable
          data-testid="ri-filter-keyword"
        />
        <t-select
          v-model="statusFilter"
          class="ri-filter-status"
          :options="statusOptions"
          placeholder="全部状态"
          data-testid="ri-filter-status"
        />
        <t-checkbox v-model="onlyUnmatched" data-testid="ri-filter-unmatched">
          只看「未命中任何科目」
        </t-checkbox>
        <span class="ri-filter-hint">
          改完即生效：报表每次编制都现读这张配置表，不需要发版
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="ri-alert"
        data-testid="ri-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="refresh">重试</t-button>
        </template>
      </t-alert>
    </t-card>

    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">{{ activeReportName }}</span>
          <span class="panel-head__meta" data-testid="ri-meta">
            共 {{ rows.length }} 项 · 启用 {{ activeCount }} · 停用 {{ rows.length - activeCount }} ·
            未命中科目 {{ unmatchedCount }}
          </span>
        </div>
        <span class="panel-head__tip">
          排序号同时决定公式求值顺序：被引用的项必须排在引用它的项前面
        </span>
      </div>

      <t-table
        row-key="id"
        :data="visibleRows"
        :columns="columns"
        :loading="loading"
        hover
        data-testid="ri-table"
      >
        <template #sort_order="{ row, rowIndex }">
          <div class="ri-sort">
            <span class="ri-sort__no" :data-testid="`ri-sort-${row.item_code}`">{{ row.sort_order }}</span>
            <t-button
              v-privilege="[PERM.manage]"
              class="ri-sort__btn"
              size="small"
              variant="text"
              shape="square"
              :disabled="rowIndex === 0 || sorting"
              data-testid="`ri-move-up-${row.item_code}`"
              @click="move(rowIndex, -1)"
            >
              <t-icon name="chevron-up" />
            </t-button>
            <t-button
              v-privilege="[PERM.manage]"
              class="ri-sort__btn"
              size="small"
              variant="text"
              shape="square"
              :disabled="rowIndex === visibleRows.length - 1 || sorting"
              data-testid="`ri-move-down-${row.item_code}`"
              @click="move(rowIndex, 1)"
            >
              <t-icon name="chevron-down" />
            </t-button>
          </div>
        </template>

        <template #item_code="{ row }">
          <span class="mono" :data-testid="`ri-code-${row.item_code}`">{{ row.item_code }}</span>
        </template>

        <template #item_name="{ row }">
          <div class="ri-name" :style="{ paddingLeft: `${(row.item_level - 1) * 16}px` }">
            <span class="ri-name__main" :class="{ 'ri-name__main--title': row.is_title }">
              {{ row.item_name }}
            </span>
            <t-tag v-if="row.is_deduction" size="small" variant="light" theme="warning">减项</t-tag>
            <t-tag v-if="row.is_title" size="small" variant="light" theme="default">标题行</t-tag>
            <span v-if="row.remark" class="ri-name__sub">{{ row.remark }}</span>
          </div>
        </template>

        <template #item_level="{ row }">
          <span class="muted" :data-testid="`ri-level-${row.item_code}`">L{{ row.item_level }}</span>
        </template>

        <template #side="{ row }">
          <span class="muted">{{ sideLabel(row.side) }}</span>
        </template>

        <template #source="{ row }">
          <div class="ri-source">
            <span v-if="row.account_pattern" class="mono" :data-testid="`ri-pattern-${row.item_code}`">
              {{ row.account_pattern }}
            </span>
            <span v-else-if="row.formula" class="mono" :data-testid="`ri-formula-${row.item_code}`">
              {{ row.formula }}
            </span>
            <span v-else class="muted">— 纯标题，金额为空 —</span>
            <t-tag
              v-if="row.matched_account_count !== null"
              size="small"
              variant="light"
              :theme="row.matched_account_count === 0 ? 'danger' : 'success'"
              :data-testid="`ri-matched-${row.item_code}`"
            >
              命中 {{ row.matched_account_count }} 科
            </t-tag>
            <t-tooltip v-if="row.referenced_by.length" :content="`被 ${row.referenced_by.join(' / ')} 的公式引用`">
              <t-tag size="small" variant="light" theme="primary">被引 {{ row.referenced_by.length }}</t-tag>
            </t-tooltip>
          </div>
        </template>

        <template #is_active="{ row }">
          <t-switch
            :value="row.is_active"
            :disabled="!canManage"
            :data-testid="`ri-toggle-${row.item_code}`"
            @change="(value: unknown) => handleToggle(row, Boolean(value))"
          />
        </template>

        <template #op="{ row }">
          <t-button
            v-privilege="[PERM.manage]"
            size="small"
            variant="outline"
            :data-testid="`ri-edit-${row.item_code}`"
            @click="openEdit(row)"
          >
            编辑
          </t-button>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== 新增 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="editingId ? `编辑报表项 · ${editingCode}` : `新增报表项 · ${activeReportName}`"
      width="780px"
      :confirm-btn="{ content: '保存', loading: saving }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="false"
      data-testid="ri-dialog"
      @confirm="handleSave"
    >
      <t-form label-align="right" :label-width="110">
        <t-row :gutter="[16, 0]">
          <t-col :span="12">
            <t-form-item label="报表项编码" required-mark>
              <t-input
                v-model="form.item_code"
                :disabled="Boolean(editingId)"
                placeholder="大写字母开头，如 OTHER_PAYABLES"
                data-testid="ri-form-code"
              />
              <div v-if="editingId" class="ri-form-tip">编码是公式的引用键，创建后不可修改</div>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="报表项名称" required-mark>
              <t-input v-model="form.item_name" :maxlength="64" data-testid="ri-form-name" />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="层级">
              <t-select
                v-model="form.item_level"
                :options="levelOptions"
                data-testid="ri-form-level"
              />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="所属方">
              <t-select v-model="form.side" :options="sideOptions" data-testid="ri-form-side" />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="排序号">
              <t-input-number
                v-model="form.sort_order"
                :min="0"
                :step="1"
                data-testid="ri-form-sort"
              />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="取数方式">
              <t-radio-group v-model="sourceMode" data-testid="ri-form-mode">
                <t-radio value="PATTERN">按科目取数</t-radio>
                <t-radio value="FORMULA">引用其它报表项（公式）</t-radio>
                <t-radio value="TITLE">纯标题行（金额为空）</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-col>

          <t-col v-if="sourceMode === 'PATTERN'" :span="24">
            <t-form-item label="取数模式">
              <t-input
                v-model="form.account_pattern"
                placeholder="1001,1002 或 2221 或 1* 或 @CATEGORY:PROFIT_LOSS"
                data-testid="ri-form-pattern"
              />
              <div class="ri-form-tips">
                <span v-for="tip in catalog.pattern_tips" :key="tip.syntax" class="ri-form-tip">
                  <b class="mono">{{ tip.syntax }}</b> {{ tip.meaning }}
                </span>
              </div>
              <div class="ri-account-picker">
                <t-select
                  v-model="pickedAccounts"
                  class="ri-account-picker__select"
                  :options="accountOptions"
                  multiple
                  filterable
                  placeholder="从科目表挑选（自动拼成编码清单）"
                  data-testid="ri-form-account-picker"
                  @change="applyPickedAccounts"
                />
              </div>
            </t-form-item>
          </t-col>

          <t-col v-if="sourceMode === 'FORMULA'" :span="24">
            <t-form-item label="计算式">
              <t-input
                v-model="form.formula"
                placeholder="REVENUE - COST_OF_SALES - TAX_SURCHARGE"
                data-testid="ri-form-formula"
              />
              <div class="ri-form-tips">
                <span class="ri-form-tip">只支持空格分隔的 <b class="mono">+</b> / <b class="mono">-</b>；引用的项必须排在本项前面</span>
              </div>
              <div class="ri-formula-picker">
                <span class="ri-formula-picker__label">可引用（排在本项前面）：</span>
                <t-tag
                  v-for="item in formulaCandidates"
                  :key="item.item_code"
                  class="ri-formula-picker__tag"
                  size="small"
                  variant="outline"
                  data-testid="`ri-formula-chip-${item.item_code}`"
                  @click="appendFormula(item.item_code)"
                >
                  {{ item.item_code }}
                </t-tag>
                <span v-if="!formulaCandidates.length" class="muted">本项已是最前面的一项</span>
              </div>
            </t-form-item>
          </t-col>

          <t-col :span="24">
            <t-form-item label="上级报表项">
              <t-select
                v-model="form.parent_code"
                :options="parentOptions"
                clearable
                filterable
                placeholder="可选（仅用于展示分组）"
                data-testid="ri-form-parent"
              />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="减项">
              <t-switch v-model="form.is_deduction" data-testid="ri-form-deduction" />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="启用">
              <t-switch v-model="form.is_active" data-testid="ri-form-active" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="备注">
              <t-input v-model="form.remark" :maxlength="200" placeholder="可选" data-testid="ri-form-remark" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <!-- 取数预览：不落库先看「这条配置会取到哪些科目的钱」 -->
      <t-card :bordered="false" class="ri-preview">
        <div class="ri-preview__head">
          <span class="ri-preview__title">取数预览</span>
          <t-button
            size="small"
            variant="outline"
            :loading="previewing"
            data-testid="ri-preview-btn"
            @click="handlePreview"
          >
            预览这条配置
          </t-button>
        </div>
        <template v-if="preview">
          <div class="ri-preview__body" data-testid="ri-preview-body">
            <span>
              命中科目 <b data-testid="ri-preview-count">{{ preview.matched_count }}</b> 个
            </span>
            <span v-if="preview.matched_accounts.length" class="ri-preview__accounts">
              {{ preview.matched_accounts.map((account) => `${account.account_code} ${account.account_name}`).join('、') }}
            </span>
          </div>
          <t-alert
            v-for="problem in preview.problems"
            :key="`p-${problem}`"
            theme="error"
            :message="problem"
            class="ri-preview__alert"
            data-testid="ri-preview-problem"
          />
          <t-alert
            v-for="warning in preview.warnings"
            :key="`w-${warning}`"
            theme="warning"
            :message="warning"
            class="ri-preview__alert"
            data-testid="ri-preview-warning"
          />
        </template>
      </t-card>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  createReportItem,
  fetchReportItemCatalog,
  fetchReportItems,
  previewReportItem,
  sortReportItems,
  toggleReportItem,
  updateReportItem,
  type ReportItem,
  type ReportItemCatalog,
  type ReportItemPayload,
  type ReportItemPreview,
} from '@/api/business/erp/report-item';
import { fetchAccountOptions } from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

/** 本页权限码（与 V0256 的 sys_menu.auth_code 逐字一致）。 */
const PERM = {
  list: 'business:erp:report-item:list',
  manage: 'business:erp:report-item:manage',
} as const;

type SourceMode = 'PATTERN' | 'FORMULA' | 'TITLE';

const loading = ref(false);
const sorting = ref(false);
const saving = ref(false);
const previewing = ref(false);
const loadError = ref('');

const catalog = ref<ReportItemCatalog>({
  reports: [],
  sides: [],
  levels: [],
  pattern_tips: [],
  total_items: 0,
});
const rows = ref<ReportItem[]>([]);
const activeReport = ref('');

const keyword = ref('');
const statusFilter = ref<'ALL' | 'ON' | 'OFF'>('ALL');
const onlyUnmatched = ref(false);

const statusOptions = [
  { label: '全部状态', value: 'ALL' },
  { label: '仅启用', value: 'ON' },
  { label: '仅停用', value: 'OFF' },
];

/** 权限判断只在「按钮禁用」上用；接口本身由后端注解把关（`v-privilege` 管可见性）。 */
const userStore = useUserStore();
const canManage = computed(() => userStore.hasPermission(PERM.manage));

const activeReportName = computed(
  () => catalog.value.reports.find((report) => report.report_code === activeReport.value)?.report_name ?? '',
);
const activeCount = computed(() => rows.value.filter((row) => row.is_active).length);
const unmatchedCount = computed(
  () => rows.value.filter((row) => row.matched_account_count === 0).length,
);

const visibleRows = computed(() => {
  const text = keyword.value.trim().toLowerCase();
  return rows.value.filter((row) => {
    if (statusFilter.value === 'ON' && !row.is_active) return false;
    if (statusFilter.value === 'OFF' && row.is_active) return false;
    if (onlyUnmatched.value && row.matched_account_count !== 0) return false;
    if (!text) return true;
    return (
      row.item_code.toLowerCase().includes(text) ||
      row.item_name.toLowerCase().includes(text) ||
      (row.account_pattern ?? '').toLowerCase().includes(text) ||
      (row.formula ?? '').toLowerCase().includes(text)
    );
  });
});

const columns: PrimaryTableCol[] = [
  { colKey: 'sort_order', title: '排序', width: 116 },
  { colKey: 'item_code', title: '编码', width: 188 },
  { colKey: 'item_name', title: '名称 / 备注', minWidth: 220 },
  { colKey: 'item_level', title: '层级', width: 64 },
  { colKey: 'side', title: '所属方', width: 96 },
  { colKey: 'source', title: '取数来源 / 命中', minWidth: 260 },
  { colKey: 'is_active', title: '启用', width: 76 },
  { colKey: 'op', title: '操作', width: 88, fixed: 'right' },
];

const sideOptions = computed(() =>
  catalog.value.sides.map((side) => ({ label: `${side.label}（${side.value}）`, value: side.value })),
);
const levelOptions = computed(() =>
  catalog.value.levels.map((level) => ({ label: level.label, value: Number(level.value) })),
);

function sideLabel(value: string): string {
  return catalog.value.sides.find((side) => side.value === value)?.label ?? value;
}

function errorText(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== 载入 ====================

async function loadCatalog(): Promise<boolean> {
  try {
    catalog.value = await fetchReportItemCatalog();
    if (!activeReport.value || !catalog.value.reports.some((report) => report.report_code === activeReport.value)) {
      activeReport.value = catalog.value.reports[0]?.report_code ?? '';
    }
    return true;
  } catch (error) {
    loadError.value = errorText(error);
    return false;
  }
}

async function loadRows(): Promise<void> {
  if (!activeReport.value) {
    rows.value = [];
    return;
  }
  loading.value = true;
  loadError.value = '';
  try {
    rows.value = await fetchReportItems(activeReport.value);
  } catch (error) {
    loadError.value = errorText(error);
    rows.value = [];
  } finally {
    loading.value = false;
  }
}

async function refresh(): Promise<void> {
  if (await loadCatalog()) {
    await loadRows();
  }
}

watch(activeReport, () => {
  void loadRows();
});

onMounted(() => {
  void refresh();
});

// ==================== 启停 ====================

async function handleToggle(row: ReportItem, next: boolean): Promise<void> {
  try {
    const saved = await toggleReportItem(row.id, next);
    MessagePlugin.success(next ? `已启用「${saved.item_name}」` : `已停用「${saved.item_name}」（报表不再显示该行）`);
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  }
}

// ==================== 排序 ====================

/**
 * 上移 / 下移一格。
 *
 * 必须把**整表顺序**发给后端：公式只能引用排在前面的项，局部交换无法判断
 * 「换完之后还有没有公式引用落到后面」，后端会整表校验，不合法直接 400 并说明是哪一项。
 */
async function move(index: number, delta: number): Promise<void> {
  const target = index + delta;
  if (target < 0 || target >= rows.value.length) {
    return;
  }
  const next = rows.value.slice();
  const [moved] = next.splice(index, 1);
  next.splice(target, 0, moved);
  sorting.value = true;
  try {
    rows.value = await sortReportItems(activeReport.value, next.map((row) => row.id));
    MessagePlugin.success(`「${moved.item_name}」已${delta < 0 ? '上移' : '下移'}`);
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    sorting.value = false;
  }
}

// ==================== 表单 ====================

interface ItemForm {
  item_code: string;
  item_name: string;
  item_level: number;
  parent_code: string;
  side: string;
  account_pattern: string;
  formula: string;
  is_deduction: boolean;
  is_active: boolean;
  sort_order: number;
  remark: string;
}

const dialogVisible = ref(false);
const editingId = ref<string | null>(null);
const editingCode = ref('');
const sourceMode = ref<SourceMode>('PATTERN');
const form = reactive<ItemForm>(emptyForm());
const pickedAccounts = ref<string[]>([]);
const preview = ref<ReportItemPreview | null>(null);
const accountOptions = ref<Array<{ label: string; value: string }>>([]);

function emptyForm(): ItemForm {
  return {
    item_code: '',
    item_name: '',
    item_level: 2,
    parent_code: '',
    side: 'ASSET',
    account_pattern: '',
    formula: '',
    is_deduction: false,
    is_active: true,
    sort_order: 0,
    remark: '',
  };
}

const parentOptions = computed(() =>
  rows.value
    .filter((row) => row.item_code !== editingCode.value)
    .map((row) => ({
      label: `${row.item_code} · ${row.item_name}`,
      value: row.item_code,
    })),
);

/** 公式候选：只有排在本项**前面**的项能被引用（求值顺序 = 排序号）。 */
const formulaCandidates = computed(() => {
  const ownOrder = form.sort_order;
  return rows.value
    .filter((row) => row.item_code !== editingCode.value)
    .filter((row) => (editingId.value ? row.sort_order < ownOrder : row.sort_order <= ownOrder))
    .sort((a, b) => a.sort_order - b.sort_order)
    .map((row) => ({ item_code: row.item_code, item_name: row.item_name }));
});

async function loadAccountOptions(): Promise<void> {
  if (accountOptions.value.length) {
    return;
  }
  try {
    const accounts = await fetchAccountOptions(600);
    accountOptions.value = accounts.map((account) => ({
      label: `${account.account_code} ${account.account_name}`,
      value: account.account_code,
    }));
  } catch {
    // 科目表拉不到不影响手写模式：只是少了「挑选」的便利
    accountOptions.value = [];
  }
}

function openCreate(): void {
  Object.assign(form, emptyForm());
  form.sort_order = (rows.value[rows.value.length - 1]?.sort_order ?? 0) + 10;
  form.side = activeReport.value === 'INCOME_STATEMENT' ? 'EQUITY' : 'ASSET';
  sourceMode.value = 'PATTERN';
  editingId.value = null;
  editingCode.value = '';
  preview.value = null;
  pickedAccounts.value = [];
  void loadAccountOptions();
  dialogVisible.value = true;
}

function openEdit(row: ReportItem): void {
  Object.assign(form, {
    item_code: row.item_code,
    item_name: row.item_name,
    item_level: row.item_level,
    parent_code: row.parent_code ?? '',
    side: row.side,
    account_pattern: row.account_pattern ?? '',
    formula: row.formula ?? '',
    is_deduction: row.is_deduction,
    is_active: row.is_active,
    sort_order: row.sort_order,
    remark: row.remark ?? '',
  });
  sourceMode.value = row.formula ? 'FORMULA' : row.account_pattern ? 'PATTERN' : 'TITLE';
  editingId.value = row.id;
  editingCode.value = row.item_code;
  preview.value = null;
  pickedAccounts.value = [];
  void loadAccountOptions();
  dialogVisible.value = true;
}

/** 科目多选 → 拼成逗号清单（保留手写的前缀 / @CATEGORY 之类，只覆盖纯编码段）。 */
function applyPickedAccounts(value: unknown): void {
  const codes = Array.isArray(value) ? value.map(String) : [];
  if (!codes.length) {
    return;
  }
  const kept = form.account_pattern
    .split(',')
    .map((token) => token.trim())
    .filter((token) => token && (token.includes('*') || token.includes('%') || token.startsWith('@')));
  form.account_pattern = [...new Set([...kept, ...codes])].join(',');
}

function appendFormula(code: string): void {
  const current = form.formula.trim();
  if (!current) {
    form.formula = code;
    return;
  }
  form.formula = `${current} ${current.endsWith('+') || current.endsWith('-') ? '' : '+ '}${code}`;
}

function buildPayload(): ReportItemPayload {
  const payload: ReportItemPayload = {
    item_name: form.item_name.trim(),
    item_level: form.item_level,
    parent_code: form.parent_code.trim(),
    side: form.side,
    is_deduction: form.is_deduction,
    is_active: form.is_active,
    sort_order: form.sort_order,
    remark: form.remark.trim(),
    // 「空串 = 清空」：三种形态互斥，把不用的那一个显式清掉
    account_pattern: sourceMode.value === 'PATTERN' ? form.account_pattern.trim() : '',
    formula: sourceMode.value === 'FORMULA' ? form.formula.trim() : '',
  };
  if (!editingId.value) {
    payload.report_code = activeReport.value;
    payload.item_code = form.item_code.trim().toUpperCase();
  }
  return payload;
}

async function handlePreview(): Promise<void> {
  previewing.value = true;
  try {
    preview.value = await previewReportItem({
      report_code: activeReport.value,
      account_pattern: sourceMode.value === 'PATTERN' ? form.account_pattern.trim() : '',
      formula: sourceMode.value === 'FORMULA' ? form.formula.trim() : '',
      exclude_id: editingId.value ?? undefined,
      sort_order: form.sort_order,
    });
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    previewing.value = false;
  }
}

async function handleSave(): Promise<void> {
  const payload = buildPayload();
  if (!payload.item_name) {
    MessagePlugin.warning('报表项名称必填');
    return;
  }
  if (sourceMode.value === 'PATTERN' && !payload.account_pattern) {
    MessagePlugin.warning('按科目取数必须填取数模式');
    return;
  }
  if (sourceMode.value === 'FORMULA' && !payload.formula) {
    MessagePlugin.warning('引用其它报表项必须填计算式');
    return;
  }
  saving.value = true;
  try {
    const saved = editingId.value
      ? await updateReportItem(editingId.value, payload)
      : await createReportItem(payload);
    if (saved.warnings.length) {
      MessagePlugin.warning(saved.warnings.join('；'));
    } else {
      MessagePlugin.success(editingId.value ? '已保存（报表刷新即生效）' : '已新增（报表刷新即生效）');
    }
    dialogVisible.value = false;
    await refresh();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    saving.value = false;
  }
}
</script>

<style scoped>
.ri-page {
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
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row--filter {
  margin-top: 12px;
}

.tool-row__spacer {
  flex: 1;
}

.ri-tab-count {
  margin-left: 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ri-total {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.ri-filter-keyword {
  width: 220px;
}

.ri-filter-status {
  width: 130px;
}

.ri-filter-hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ri-alert {
  margin-top: 12px;
}

.panel-head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta,
.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.ri-sort {
  display: flex;
  align-items: center;
  gap: 2px;
}

.ri-sort__no {
  min-width: 30px;
  font-variant-numeric: tabular-nums;
  color: var(--td-text-color-secondary);
}

.ri-sort__btn {
  min-width: 22px;
  padding: 0 2px;
}

.ri-name {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.ri-name__main--title {
  font-weight: 600;
}

.ri-name__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ri-source {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.mono {
  font-family: var(--td-font-family-mono, 'JetBrains Mono', Menlo, Consolas, monospace);
  font-size: 12px;
}

.muted {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.ri-form-tip,
.ri-form-tips {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ri-form-tips {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 4px;
}

.ri-account-picker {
  margin-top: 8px;
  width: 100%;
}

.ri-account-picker__select {
  width: 100%;
}

.ri-formula-picker {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
  margin-top: 8px;
}

.ri-formula-picker__label {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ri-formula-picker__tag {
  cursor: pointer;
}

.ri-preview {
  margin-top: 8px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.ri-preview__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.ri-preview__title {
  font-size: 13px;
  font-weight: 600;
}

.ri-preview__body {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 8px;
  font-size: 12px;
}

.ri-preview__accounts {
  color: var(--td-text-color-secondary);
}

.ri-preview__alert {
  margin-top: 8px;
}
</style>
