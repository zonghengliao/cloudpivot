<!--
  资产分类页（FIN-2 / ARCH-6 §5.10）。

  分类是折旧参数的唯一来源：使用月数、净残值率、原值 / 累计折旧 / 折旧费用三个科目。
  改分类不会改动已建卡的参数（卡片在建卡时快照），所以编辑表单只影响**后续**建卡。
-->
<template>
  <div class="category-page">
    <!-- ①② 筛选 / 分页 / 行操作交给 ListPage（P3-12 收口） -->
    <ListPage
      ref="listRef"
      title="资产分类"
      testid="category"
      table-testid="category-table"
      meta-testid="category-meta"
      btn-prefix="category-btn"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="category-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建分类
        </t-button>
      </template>

      <!-- 页头附注：折旧方法口径（原来是列表卡右上角的说明） -->
      <template #head-extra>
        <span class="panel-head__tip">
          折旧方法本期只有平均年限法；月折旧额 = round((原值 × (1 − 残值率)) / 月数, 4)
        </span>
      </template>

      <template #category_code="{ row }">
        <span class="mono" :data-testid="`category-code-${row.category_code}`">{{ row.category_code }}</span>
      </template>
      <template #depreciation_method="{ row }">
        <t-tag size="small" variant="light">{{ methodLabel(row.depreciation_method) }}</t-tag>
      </template>
      <template #useful_life_months="{ row }">
        <span class="num">{{ row.useful_life_months }} 个月</span>
      </template>
      <template #salvage_rate="{ row }">
        <span class="num">{{ formatRate(row.salvage_rate) }}</span>
      </template>
      <template #accounts="{ row }">
        <div class="accounts-cell">
          <span>{{ accountLabel(accounts, row.asset_account_id) }}</span>
          <span class="muted">/ {{ accountLabel(accounts, row.accum_account_id) }}</span>
          <span class="muted">/ {{ accountLabel(accounts, row.expense_account_id) }}</span>
        </div>
      </template>
      <template #status="{ row }">
        <t-tag size="small" :theme="categoryStatusTheme(row.status)" variant="light">
          {{ categoryStatusLabel(row.status) }}
        </t-tag>
      </template>
      <template #op="{ row }">
        <div class="op-cell">
          <t-link v-privilege="[PERM.update]" theme="primary" @click="openEdit(row)">编辑</t-link>
          <t-link v-privilege="[PERM.remove]" theme="danger" @click="confirmDelete(row)">删除</t-link>
        </div>
      </template>
    </ListPage>

    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑资产分类' : '新建资产分类'"
      width="720px"
      :confirm-btn="{ content: '保存', loading: submitting }"
      @confirm="submitForm"
    >
      <t-form :data="form" label-width="110px">
        <t-row :gutter="[16, 0]">
          <t-col :span="12">
            <t-form-item label="分类编码" :status="statusOf('category_code')">
              <!-- CODE-07 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
              <!-- CODE-08：系统参数关闭「允许手工指定编码」后编码栏只读、按钮隐藏 -->
              <div class="dlg-inline dlg-inline--grow">
                <t-input
                  v-model="form.category_code"
                  :maxlength="40"
                  placeholder="留空自动生成，如 AC-01"
                  :disabled="manualCodeDisabled(!!editingId, allowManualCode)"
                  class="dlg-inline__grow"
                  data-testid="category-form-code"
                />
                <t-button
                  v-if="allowManualCode"
                  variant="outline"
                  theme="primary"
                  :disabled="!!editingId"
                  :loading="genCodeLoading"
                  data-testid="category-gen-code"
                  @click="handleGenCode"
                >
                  生成编码
                </t-button>
              </div>
              <template #help>
                <span class="dlg-help">
                  {{
                    errors.category_code ||
                    (editingId
                      ? '编码是分类身份，不支持修改'
                      : manualCodeHelp(allowManualCode, '不点「生成编码」则保存时自动生成；生成后仍可手改（企业自有编码如 CAT-BUILDING 也可直接填）'))
                  }}
                </span>
              </template>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="分类名称" :help="errors.category_name" :status="statusOf('category_name')">
              <t-input v-model="form.category_name" placeholder="如 电子设备" data-testid="category-form-name" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="使用月数"
              :help="errors.useful_life_months"
              :status="statusOf('useful_life_months')"
            >
              <t-input-number
                v-model="form.useful_life_months"
                :min="1"
                :max="600"
                theme="normal"
                placeholder="1–600"
                data-testid="category-form-months"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="净残值率" :help="errors.salvage_rate" :status="statusOf('salvage_rate')">
              <t-input v-model="form.salvage_rate" placeholder="0.05 = 5%" data-testid="category-form-salvage" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="原值科目" :help="errors.asset_account_id" :status="statusOf('asset_account_id')">
              <t-select
                v-model="form.asset_account_id"
                :options="optionsOfAccount"
                placeholder="默认 1601 固定资产"
                filterable
                data-testid="category-form-asset-account"
              />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="累计折旧科目" :help="errors.accum_account_id" :status="statusOf('accum_account_id')">
              <t-select
                v-model="form.accum_account_id"
                :options="optionsOfAccount"
                placeholder="默认 1602 累计折旧"
                filterable
                data-testid="category-form-accum-account"
              />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="折旧费用科目" :help="errors.expense_account_id" :status="statusOf('expense_account_id')">
              <t-select
                v-model="form.expense_account_id"
                :options="optionsOfAccount"
                placeholder="默认 5602.01 管理费用-折旧费"
                filterable
                data-testid="category-form-expense-account"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="状态">
              <t-select v-model="form.status" :options="statusOptions" data-testid="category-form-status" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="排序">
              <t-input-number v-model="form.sort_order" :min="0" theme="normal" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="备注">
              <t-textarea v-model="form.remark" :maxlength="2000" placeholder="可选" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
      <p class="dialog-tip">
        分类的年限与残值率只在**建卡时**快照到卡片上；改分类不会影响已建卡的折旧口径（这是会计上的要求）。
      </p>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import { fetchAccountOptions, type AccountRow } from '@/api/business/erp';
import {
  createAssetCategory,
  deleteAssetCategory,
  fetchAssetCategoryNextCode,
  fetchAssetCategoryPage,
  updateAssetCategory,
  type AssetCategory,
  type AssetCategoryPayload,
  type AssetCategoryQuery,
} from '@/api/business/erp/asset';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';

import {
  CATEGORY_STATUS_FILTER_OPTIONS,
  CATEGORY_STATUS_OPTIONS,
  accountLabel,
  accountOptions,
  categoryStatusLabel,
  categoryStatusTheme,
  categoryToForm,
  methodLabel,
} from './category-logic';
import { toMasterCodeInput } from '../shared/master-code';
import { loadAllowManualCode, manualCodeDisabled, manualCodeHelp } from '../shared/code-policy';
import {
  emptyCategoryForm,
  errorText,
  formatRate,
  validateCategoryForm,
  type CategoryFormState,
} from '../asset/asset-logic';

/** 本页权限码（与 V0229 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:asset-category:list',
  create: 'business:erp:asset-category:create',
  update: 'business:erp:asset-category:update',
  remove: 'business:erp:asset-category:delete',
} as const;

const PAGE_SIZE = 20;

const accounts = ref<AccountRow[]>([]);

/** 筛选行（`filterState` 就地读写；`queryBuilder` 引用的是同一个对象） */
const filter = reactive<{ keyword: string; status: number | '' }>({ keyword: '', status: '' });
const statusFilterOptions = CATEGORY_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
const statusOptions = CATEGORY_STATUS_OPTIONS.map((item) => ({ ...item }));
const optionsOfAccount = computed(() => accountOptions(accounts.value));

/** ListPage 实例（保存 / 删除后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/**
 * 筛选项声明（顺序即渲染顺序）。
 *
 * 两个控件改造前都是「状态一变就查」（`@change="handleSearch"`），关键词走回车 ——
 * 手感与改造前一致。
 */
const filters: FilterDef[] = [
  {
    key: 'status',
    type: 'select',
    options: statusFilterOptions,
    placeholder: '全部状态',
    width: 130,
    searchOnChange: true,
    testid: 'category-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '编码 / 名称（模糊）',
    width: 220,
    testid: 'category-filter-keyword',
  },
];

/** 拉一页 */
const fetcher: ListFetcher<AssetCategory> = (query) =>
  fetchAssetCategoryPage(query as AssetCategoryQuery);

/** 入参拼装（与改造前 `loadRows` 里那段一致：关键词 trim、状态原样） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as { keyword: string; status: number | '' };
  return {
    page,
    page_size: pageSize,
    keyword: value.keyword.trim(),
    status: value.status,
  };
};

const columns: ColumnDef[] = [
  { colKey: 'category_code', title: '分类编码', width: 130 },
  { colKey: 'category_name', title: '分类名称', width: 140 },
  { colKey: 'depreciation_method', title: '折旧方法', width: 120 },
  { colKey: 'useful_life_months', title: '使用月数', width: 100 },
  { colKey: 'salvage_rate', title: '净残值率', width: 100, align: 'right' },
  { colKey: 'accounts', title: '科目（原值 / 累计折旧 / 费用）', width: 330, ellipsis: true },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

/** 重拉当前页（保存 / 删除后；新建时回第 1 页让新行进视野） */
async function reloadList(page?: number): Promise<void> {
  if (page === undefined) {
    await listRef.value?.reload();
    return;
  }
  await listRef.value?.load(page);
}

async function loadAccounts(): Promise<void> {
  try {
    accounts.value = await fetchAccountOptions(500);
  } catch (error) {
    MessagePlugin.warning(`科目表加载失败：${errorText(error)}`);
  }
}

// ==================== 表单 ====================

const formVisible = ref(false);
const editingId = ref('');
const form = reactive<CategoryFormState>(emptyCategoryForm());
const errors = ref<Partial<Record<keyof CategoryFormState, string>>>({});
const submitting = ref(false);

function statusOf(field: keyof CategoryFormState): 'error' | undefined {
  return errors.value[field] ? 'error' : undefined;
}

function openCreate(): void {
  editingId.value = '';
  Object.assign(form, emptyCategoryForm());
  errors.value = {};
  formVisible.value = true;
}

function openEdit(row: AssetCategory): void {
  editingId.value = row.id;
  Object.assign(form, categoryToForm(row));
  errors.value = {};
  formVisible.value = true;
}

function toPayload(): AssetCategoryPayload {
  return {
    // 留空发 null：后端据此自动取号（AC-01 形态）；填了原样保留（**大小写不改**，六类同一口径）。
    category_code: toMasterCodeInput(form.category_code),
    category_name: form.category_name.trim(),
    useful_life_months: Number(form.useful_life_months ?? 0),
    salvage_rate: form.salvage_rate.trim() || '0',
    asset_account_id: form.asset_account_id,
    accum_account_id: form.accum_account_id,
    expense_account_id: form.expense_account_id,
    status: form.status,
    sort_order: Number(form.sort_order ?? 0),
    remark: form.remark,
  };
}

// ==================== 生成编码（CODE-07 交互方式 B） ====================

/** 「生成编码」按钮加载中 */
const genCodeLoading = ref(false);

/**
 * 是否允许手工指定编码（CODE-08，系统参数 `erp.master_data.allow_manual_code`）。
 *
 * `false` ⇒ 编码栏只读 + 「生成编码」按钮隐藏 + 提示「编码由系统自动生成」。
 */
const allowManualCode = ref(true);

/**
 * 取下一个可用编码填入编码框（**不占号**，可手改）。
 *
 * 后端 `GET /business/erp/asset-category/next-code` 只读 `biz_seq`（方案 B）：
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
    const preview = await fetchAssetCategoryNextCode();
    form.category_code = preview.code;
    // 之前「格式不合法」之类的提示已不成立，清掉免得红框跟着新号走
    delete errors.value.category_code;
  } catch (error) {
    MessagePlugin.error(`生成编码失败：${errorText(error)}`);
  } finally {
    genCodeLoading.value = false;
  }
}

async function submitForm(): Promise<void> {
  const found = validateCategoryForm(form);
  errors.value = found;
  if (Object.keys(found).length > 0) {
    MessagePlugin.warning('请先修正表单里的红字项');
    return;
  }
  submitting.value = true;
  try {
    if (editingId.value) {
      await updateAssetCategory(editingId.value, toPayload());
      MessagePlugin.success('分类已保存（已建卡的参数不受影响）');
    } else {
      const created = await createAssetCategory(toPayload());
      // 没点「生成编码」直接保存时编码是后端取的号 —— 回填到表单并在提示里报出来，
      // 用户当场就能看到自己这条分类到底叫 AC-0 几。
      form.category_code = created.category_code;
      MessagePlugin.success(`分类已创建 · 编码 ${created.category_code}`);
    }
    formVisible.value = false;
    // 新建回第 1 页（新行按 sort_order/code 排在前面），编辑留在当前页
    await reloadList(editingId.value ? undefined : 1);
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

function confirmDelete(row: AssetCategory): void {
  const dialog = DialogPlugin.confirm({
    header: '删除资产分类',
    body: `确认删除「${row.category_code} ${row.category_name}」？分类下已有资产卡片时后端会拒绝（409）。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteAssetCategory(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

onMounted(async () => {
  // 列表由 ListPage 自己在 setup 里拉首屏；这里只补表单依赖：科目表 + 编码策略
  await loadAccounts();
  // CODE-08：编码策略单独取（失败按「允许手填」兜底，不打断列表）
  allowManualCode.value = await loadAllowManualCode();
});
</script>

<style scoped lang="less">
.category-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.accounts-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  font-size: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 13px;
}

.num {
  font-variant-numeric: tabular-nums;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-cell {
  display: flex;
  gap: 10px;
}

/* CODE-07：「编码框 + 生成编码」一行铺满（输入框吃掉剩余宽度，按钮不缩） */
.dlg-inline {
  display: flex;
  align-items: center;
  gap: 8px;
}

.dlg-inline--grow {
  width: 100%;
  flex-wrap: nowrap;
}

.dlg-inline__grow {
  flex: 1 1 auto;
  min-width: 0;
}

.dlg-help {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dialog-tip {
  margin: 8px 0 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-placeholder);
}
</style>
