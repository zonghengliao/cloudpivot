<!--
  标准成本维护页（M5-09【M5·成本】标准成本：维护页 CRUD · 批量导入）。

  一页三件事：**版本列表（物料 × 期间）→ 一版的标准卡头 + 标准 BOM → 批量导入**。

  # 为什么列表和表单都围绕「期间」

  标准成本是**按期间生效的口径**（M5-08 的版本表就是 `物料 × 期间`）：列表默认按期间筛，
  新建默认落本月整期，导入的每组也带期间。这样「这一期这个物料按哪版算」永远只有一个答案。

  # 已生效版本锁改（后端 409/66109、66110）

  生效起始日早于今天的版本，是「那段期间按什么标准算」的凭据：
  **只能改停止日 / 状态 / 备注**，也不能删。页面把其余控件置灰并给出说明，
  而不是等用户改完保存再被拒。

  # 批量导入（9 列一张表，全有或全无）

  一组「物料编码 + 期间」= 一版，组内一行 = 一条标准 BOM 明细。任何一行有错整批不入库，
  错误按「Excel 行号 + 列名 + 原因」逐条列出 —— 半批导入会让标准成本少一版而没人发现。
-->
<template>
  <div class="cost-standard-page">
    <ListPage
      ref="listRef"
      class="tool-row--dense"
      title="标准成本"
      testid="cs"
      table-testid="cs-table"
      meta-testid="cs-meta"
      btn-prefix="cs-btn"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      @loaded="onLoaded"
    >
      <template #filter-period_code>
        <t-date-picker
          v-model="filter.period_code"
          class="tool-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间"
          clearable
          data-testid="cs-filter-period"
          @change="handleSearch"
        />
      </template>

      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="cs-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增版本
        </t-button>
        <t-button
          v-privilege="[PERM.create]"
          variant="outline"
          data-testid="cs-btn-import"
          @click="openImport"
        >
          <template #icon><t-icon name="upload" /></template>
          批量导入
        </t-button>
        <t-button variant="text" data-testid="cs-btn-template" @click="handleDownloadTemplate">
          <template #icon><t-icon name="download" /></template>
          下载模板
        </t-button>
      </template>

      <template #head-extra>
        <span class="cs-tip">
          一版 = 「某物料从哪天到哪天按这套标准算」；同一物料的启用版本生效期不许重叠。
          已生效过的版本只能改停止日 / 状态 / 备注（历史口径是差异分析的凭据）
        </span>
      </template>

      <template #material="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ materialLabel(row) }}</span>
          <span class="name-cell__sub">
            {{ materialTypeLabel(row.material_type) }}
            <template v-if="row.material_spec"> · {{ row.material_spec }}</template>
          </span>
        </div>
      </template>

      <template #version="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">v{{ row.version_no }}</span>
          <span class="name-cell__sub">{{ row.items_count }} 行 BOM</span>
        </div>
      </template>

      <template #window="{ row }">{{ windowText(row.effective_from, row.effective_to) }}</template>

      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="statusTheme(row.status)">
          {{ row.status_label || statusLabel(row.status) }}
        </t-tag>
      </template>

      <template #publish="{ row }">
        <t-tag size="small" variant="light" :theme="publishStateTheme(row.publish_state)">
          {{ row.publish_state_label || publishStateLabel(row.publish_state) }}
        </t-tag>
      </template>

      <template #std_material_cost="{ row }">
        <span class="num">{{ amountsOf(row).material }}</span>
      </template>
      <template #std_labor_cost="{ row }">
        <span class="num">{{ amountsOf(row).labor }}</span>
      </template>
      <template #std_overhead_cost="{ row }">
        <span class="num">{{ amountsOf(row).overhead }}</span>
      </template>
      <template #std_total_cost="{ row }">
        <span class="num num--strong">{{ amountsOf(row).total }}</span>
      </template>

      <template #standard="{ row }">
        <div class="name-cell">
          <span class="name-cell__main num">{{ trimZero(row.std_labor_hours) }} h</span>
          <span class="name-cell__sub num">@ {{ trimZero(row.std_labor_rate) }} /h</span>
        </div>
      </template>

      <template #remark="{ row }">{{ row.remark || '—' }}</template>

      <template #op="{ row }">
        <div class="op-cell">
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            :disabled="!publishGate(row).canSubmit"
            :title="publishGate(row).submitHint"
            data-testid="cs-btn-publish"
            @click.stop="submitPublish(row)"
          >
            提交发布
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            theme="warning"
            :disabled="!publishGate(row).canWithdraw"
            :title="publishGate(row).withdrawHint"
            data-testid="cs-btn-withdraw"
            @click.stop="withdrawPublish(row)"
          >
            撤回
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="cs-btn-edit"
            @click.stop="openEdit(row)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="cs-btn-status"
            @click.stop="openStatus(row)"
          >
            {{ row.status === 1 ? '失效' : '生效' }}
          </t-link>
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            :disabled="!canDelete(row, today)"
            data-testid="cs-btn-delete"
            @click.stop="removeRow(row)"
          >
            删除
          </t-link>
        </div>
      </template>
    </ListPage>

    <!-- ==================== 新建 / 编辑：卡头 + 标准 BOM ==================== -->
    <t-dialog
      v-model:visible="dialog.visible"
      :header="dialog.editing ? `编辑标准成本 v${dialog.editing.version_no}` : '新增标准成本版本'"
      width="920px"
      :confirm-btn="{ content: '保存', loading: dialog.saving }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="false"
      @confirm="submitForm"
    >
      <t-form label-width="110px" :data="form" class="cs-form">
        <div class="cs-form__row">
          <t-form-item label="物料" :error="errors.material_id">
            <t-select
              v-model="form.material_id"
              :options="productOptions"
              :loading="optionsLoading"
              :disabled="lockedEditing"
              filterable
              clearable
              placeholder="成品 / 半成品"
              data-testid="cs-form-material"
            >
              <template #empty>
                <div class="form-note">
                  没有可选物料：要么还没建成品 / 半成品，要么当前账号缺物料查看权限。
                </div>
              </template>
            </t-select>
          </t-form-item>

          <t-form-item label="版本状态">
            <t-radio-group v-model="form.status" data-testid="cs-form-status">
              <t-radio :value="1">生效</t-radio>
              <t-radio :value="0">失效</t-radio>
            </t-radio-group>
          </t-form-item>
        </div>

        <t-form-item label="生效窗口">
          <t-radio-group
            v-model="form.custom_window"
            :disabled="lockedEditing"
            data-testid="cs-form-window-mode"
            @change="handleWindowModeChange"
          >
            <t-radio :value="false">整期（按会计期间）</t-radio>
            <t-radio :value="true">自定义日期</t-radio>
          </t-radio-group>
        </t-form-item>

        <div class="cs-form__row">
          <t-form-item v-if="!form.custom_window" label="期间" :error="errors.period_code">
            <t-date-picker
              v-model="form.period_code"
              mode="month"
              value-type="YYYY-MM"
              format="YYYY-MM"
              :disabled="lockedEditing"
              placeholder="如 2026-11"
              data-testid="cs-form-period"
            />
            <div class="form-note">整期 = 期初 ~ 期末：{{ periodHint }}</div>
          </t-form-item>
          <template v-else>
            <t-form-item label="生效起始日" :error="errors.effective_from">
              <t-date-picker
                v-model="form.effective_from"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                :disabled="lockedEditing"
                placeholder="含当天"
                data-testid="cs-form-from"
              />
            </t-form-item>
            <t-form-item label="停止日" :error="errors.effective_to">
              <t-date-picker
                v-model="form.effective_to"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                clearable
                placeholder="留空 = 至今有效"
                data-testid="cs-form-to"
              />
            </t-form-item>
          </template>
        </div>

        <div class="cs-form__row">
          <t-form-item label="标准工时" :error="errors.std_labor_hours">
            <t-input
              v-model="form.std_labor_hours"
              :disabled="lockedEditing"
              placeholder="可空 = 0，最多 4 位小数"
              data-testid="cs-form-hours"
            />
          </t-form-item>
          <t-form-item label="标准工资率" :error="errors.std_labor_rate">
            <t-input
              v-model="form.std_labor_rate"
              :disabled="lockedEditing"
              placeholder="可空 = 0，最多 6 位小数"
              data-testid="cs-form-rate"
            />
          </t-form-item>
          <t-form-item label="制造费用" :error="errors.std_overhead_cost">
            <t-input
              v-model="form.std_overhead_cost"
              :disabled="lockedEditing"
              placeholder="可空 = 0，最多 6 位小数"
              data-testid="cs-form-overhead"
            />
          </t-form-item>
        </div>

        <t-alert
          v-if="lockedEditing"
          theme="warning"
          class="form-alert"
          :message="LOCKED_HINT"
          data-testid="cs-form-locked"
        />

        <!-- ---------- 标准 BOM ---------- -->
        <div class="cs-sub">
          <div class="cs-sub__head">
            <span class="cs-sub__title">标准 BOM（耗用物料 × 单位用量 × 标准单价）</span>
            <t-button
              v-if="!lockedEditing"
              size="small"
              variant="outline"
              data-testid="cs-form-add-item"
              @click="addItem"
            >
              <template #icon><t-icon name="add" /></template>
              添加明细
            </t-button>
          </div>

          <t-alert
            v-if="errors.items"
            theme="error"
            class="form-alert"
            :message="errors.items"
            data-testid="cs-form-items-error"
          />

          <t-table
            row-key="key"
            size="small"
            :data="form.items"
            :columns="itemColumns"
            :bordered="true"
            data-testid="cs-form-items"
          >
            <template #component_material_id="{ row }">
              <t-select
                v-model="row.component_material_id"
                :options="componentOptions"
                :disabled="lockedEditing"
                filterable
                clearable
                placeholder="耗用物料"
                data-testid="cs-form-item-material"
              />
            </template>
            <template #std_qty="{ row }">
              <t-input
                v-model="row.std_qty"
                :disabled="lockedEditing"
                placeholder="> 0"
                data-testid="cs-form-item-qty"
              />
            </template>
            <template #std_price="{ row }">
              <t-input
                v-model="row.std_price"
                :disabled="lockedEditing"
                placeholder="≥ 0"
                data-testid="cs-form-item-price"
              />
            </template>
            <template #amount="{ row }">
              <span class="num">{{ itemAmountText(row) }}</span>
            </template>
            <template #item_op="{ row }">
              <t-link
                v-if="!lockedEditing"
                theme="danger"
                data-testid="cs-form-item-remove"
                @click="removeItem(row.key)"
              >
                删除
              </t-link>
              <span v-else>—</span>
            </template>
          </t-table>

          <div class="cs-amounts" data-testid="cs-form-preview">
            <span class="cs-amounts__title">单位标准成本预览</span>
            <span>料 <b class="num">{{ preview.material }}</b></span>
            <span>工 <b class="num">{{ preview.labor }}</b></span>
            <span>费 <b class="num">{{ preview.overhead }}</b></span>
            <span class="cs-amounts__total">总 <b class="num">{{ preview.total }}</b></span>
            <span class="cs-amounts__hint">
              ＝ 料 Σ round(用量×单价,6) ＋ round(工时×费率,6) ＋ 制造费用；<b>服务端为准</b>
            </span>
            <span v-if="!preview.valid" class="cs-amounts__warn">有明细的用量 / 单价填得不对，预览不可信</span>
          </div>
        </div>

        <t-form-item label="备注" :error="errors.remark">
          <t-textarea
            v-model="form.remark"
            :maxlength="REMARK_MAX"
            placeholder="版本说明 / 审批单号（可空）"
            data-testid="cs-form-remark"
          />
        </t-form-item>

        <t-alert
          v-if="overlapHint"
          theme="error"
          class="form-alert"
          :message="overlapHint"
          data-testid="cs-form-overlap"
        />
      </t-form>
    </t-dialog>

    <!-- ==================== 生效 / 失效 ==================== -->
    <t-dialog
      v-model:visible="statusDialog.visible"
      :header="statusDialog.row?.status === 1 ? '失效这一版' : '生效这一版'"
      width="560px"
      :confirm-btn="{ content: statusDialog.row?.status === 1 ? '确认失效' : '确认生效', loading: statusDialog.saving, theme: statusDialog.row?.status === 1 ? 'warning' : 'primary' }"
      :cancel-btn="{ content: '取消' }"
      @confirm="submitStatus"
    >
      <p class="cs-status__who">
        {{ statusDialog.row ? materialLabel(statusDialog.row) : '' }} · v{{ statusDialog.row?.version_no }} ·
        {{ windowText(statusDialog.row?.effective_from, statusDialog.row?.effective_to) }}
      </p>
      <t-alert
        theme="info"
        class="form-alert"
        :message="statusDialog.row?.status === 1 ? INACTIVE_CONSEQUENCE : ACTIVE_CONSEQUENCE"
      />
      <t-form label-width="90px">
        <t-form-item label="变更原因">
          <t-textarea
            v-model="statusDialog.reason"
            :maxlength="100"
            placeholder="可空；填了会追加到备注留痕（[生效|失效 yyyy-MM-dd] …）"
            data-testid="cs-status-reason"
          />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== 批量导入（9 列，全有或全无） ==================== -->
    <t-dialog
      v-model:visible="importDialog.visible"
      header="批量导入标准成本"
      width="820px"
      :footer="false"
      :close-on-overlay-click="false"
      data-testid="cs-import-dialog"
    >
      <t-alert theme="info" class="form-alert">
        模板 9 列：物料编码 / 期间 / 标准工时 / 标准工资率 / 标准制造费用 / 耗用物料编码 / 标准用量 /
        标准单价 / 备注。<b>一组「物料编码 + 期间」= 一版</b>；<b>任何一行有错，整批都不入库</b>。
      </t-alert>
      <ul class="cs-guide">
        <li v-for="(line, index) in IMPORT_GUIDE" :key="index">{{ line }}</li>
      </ul>

      <div class="cs-import__row">
        <t-button variant="outline" data-testid="cs-import-template" @click="handleDownloadTemplate">
          <template #icon><t-icon name="download" /></template>
          下载导入模板
        </t-button>
        <input
          ref="fileInput"
          type="file"
          accept=".xlsx"
          class="hidden-input"
          data-testid="cs-import-file"
          @change="handleFileChange"
        />
        <t-button variant="outline" data-testid="cs-import-pick" @click="pickFile">
          <template #icon><t-icon name="folder" /></template>
          选择 xlsx 文件
        </t-button>
        <span class="cs-import__file" data-testid="cs-import-filename">
          {{ importDialog.file ? importDialog.file.name : '未选择文件' }}
        </span>
        <t-button
          theme="primary"
          :loading="importDialog.uploading"
          :disabled="!importDialog.file"
          data-testid="cs-import-submit"
          @click="submitImport"
        >
          上传并校验
        </t-button>
      </div>

      <t-alert
        v-if="importDialog.error"
        theme="error"
        :message="importDialog.error"
        class="form-alert"
        data-testid="cs-import-error"
      />

      <template v-if="importDialog.result">
        <t-alert
          :theme="importSummaryTheme(importDialog.result)"
          :message="importSummaryText(importDialog.result)"
          class="form-alert"
          data-testid="cs-import-summary"
        />
        <t-table
          v-if="importIssueRows(importDialog.result).length"
          row-key="key"
          size="small"
          max-height="320"
          :data="importIssueRows(importDialog.result)"
          :columns="issueColumns"
          data-testid="cs-import-issues"
        >
          <template #line="{ row }"><span class="mono">第 {{ row.line }} 行</span></template>
          <template #column="{ row }">{{ row.column }}</template>
          <template #message="{ row }">{{ row.message }}</template>
        </t-table>
      </template>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  COST_STANDARD_DELETE_CODE,
  COST_STANDARD_LOCKED_CODE,
  COST_STANDARD_OVERLAP_CODE,
  COST_STANDARD_PERMISSIONS,
  COST_STANDARD_PUBLISH_GUARD_CODE,
  OPTION_LIMIT,
  PAGE_SIZE,
  PUBLISH_STATE_OPTIONS,
  STANDARD_STATUS_OPTIONS,
  createStandard,
  deleteStandard,
  downloadStandardTemplate,
  fetchStandardDetail,
  fetchStandardPage,
  importStandards,
  publishStateLabel,
  publishStateTheme,
  saveBlob,
  setStandardStatus,
  statusLabel,
  statusTheme,
  submitStandardPublish,
  updateStandard,
  withdrawStandardPublish,
  type StandardImportResult,
  type StandardRow,
} from '@/api/business/erp/cost-standard';
import { fetchMaterialPage, type MaterialRow } from '@/api/business/erp';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { todayString } from '@/views/business/erp/shared/money';
import { errorCodeOf, errorText } from '@/views/business/erp/shared/error-text';
import {
  ACTIVE_CONSEQUENCE,
  IMPORT_GUIDE,
  INACTIVE_CONSEQUENCE,
  ITEM_MAX,
  LOCKED_HINT,
  REMARK_MAX,
  amountsOf,
  canDelete,
  emptyForm,
  emptyItem,
  formFromDetail,
  importIssueRows,
  importSummaryText,
  importSummaryTheme,
  isLockedEffective,
  isUnchanged,
  isValid,
  materialLabel,
  materialTypeLabel,
  periodWindow,
  previewAmounts,
  publishGate,
  toCreatePayload,
  toScaled,
  toUpdatePayload,
  todayText,
  trimZero,
  validateForm,
  windowText,
  AMOUNT_SCALE,
  type StandardFormErrors,
  type StandardFormItem,
  type StandardFormState,
} from './cost-standard-logic';

/** 本页权限码（复用 V0460 的四个 `cost-method:*`，零新增） */
const PERM = COST_STANDARD_PERMISSIONS;

/** 今天（北京时间，与后端 `today_cst()` 同口径） */
const today = todayString();

// ==================== 列表 ====================

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const filter = reactive<{
  period_code: string;
  status: number | '';
  publish_state: string;
  keyword: string;
}>({
  period_code: '',
  status: '',
  publish_state: '',
  keyword: '',
});

const columns: ColumnDef[] = [
  { colKey: 'material', title: '物料', width: 220, ellipsis: true },
  { colKey: 'version', title: '版本', width: 100 },
  { colKey: 'window', title: '生效期', width: 200 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'publish', title: '发布', width: 90 },
  { colKey: 'std_material_cost', title: '标准材料', width: 120 },
  { colKey: 'std_labor_cost', title: '标准人工', width: 120 },
  { colKey: 'std_overhead_cost', title: '制造费用', width: 120 },
  { colKey: 'std_total_cost', title: '标准总成本', width: 130 },
  { colKey: 'standard', title: '工时 / 工资率', width: 120 },
  { colKey: 'remark', title: '备注', width: 200, ellipsis: true },
  { colKey: 'op', title: '操作', width: 300, fixed: 'right', exportable: false },
];

const filters = computed<FilterDef[]>(() => [
  {
    // `t-date-picker mode="month"` 不是声明式控件，走 custom 插槽自绘
    key: 'period_code',
    type: 'custom',
    placeholder: '会计期间',
    width: 150,
    testid: 'cs-filter-period',
  },
  {
    key: 'status',
    type: 'select',
    options: STANDARD_STATUS_OPTIONS.map((option) => ({ label: option.label, value: option.value })),
    searchOnChange: true,
    placeholder: '全部状态',
    width: 110,
    testid: 'cs-filter-status',
  },
  {
    key: 'publish_state',
    type: 'select',
    options: PUBLISH_STATE_OPTIONS.map((option) => ({ label: option.label, value: option.value })),
    searchOnChange: true,
    placeholder: '全部发布状态',
    width: 130,
    testid: 'cs-filter-publish',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '物料编码 / 名称 / 备注',
    width: 220,
    testid: 'cs-filter-keyword',
  },
]);

const fetcher: ListFetcher<StandardRow> = (query) =>
  fetchStandardPage(query as Parameters<typeof fetchStandardPage>[0]);

const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as { period_code: string; status: number | ''; publish_state: string; keyword: string };
  return {
    page,
    page_size: pageSize,
    period_code: value.period_code,
    status: value.status === '' ? undefined : value.status,
    publish_state: value.publish_state || undefined,
    keyword: value.keyword.trim(),
  };
};

/** 当前页数据（客户端重叠预检用；服务端仍是最终判据） */
const rows = ref<StandardRow[]>([]);

function onLoaded(payload?: { rows?: StandardRow[] }): void {
  if (payload?.rows) {
    rows.value = payload.rows;
  }
}

async function reload(): Promise<void> {
  await listRef.value?.reload();
}

function handleSearch(): void {
  void listRef.value?.search();
}

// ==================== 物料下拉（成品 / 半成品） ====================

const products = ref<MaterialRow[]>([]);
const optionsLoading = ref(false);

const productOptions = computed(() =>
  products.value.map((row) => ({
    value: row.id,
    label: `${row.material_code} ${row.material_name}`,
  })),
);

/** 耗用物料可选范围更宽（原材料 + 半成品 + 成品：标准 BOM 允许用任何在用物料） */
const componentOptions = computed(() =>
  products.value.map((row) => ({
    value: row.id,
    label: `${row.material_code} ${row.material_name}（${materialTypeLabel(row.material_type)}）`,
  })),
);

/**
 * 载一次物料（失败不阻塞页面）。
 *
 * 物料列表接口一次只认一个 `material_type`，所以按 3 类各发一次再合并；
 * 服务类（SERVICE）不进下拉 —— 后端也会拒（标准 BOM 里挂服务类没有意义）。
 */
async function loadProducts(): Promise<void> {
  optionsLoading.value = true;
  try {
    const types = ['FINISHED', 'SEMI', 'RAW'] as const;
    const pages = await Promise.all(
      types.map((type) =>
        fetchMaterialPage({ material_type: type, status: 1, page: 1, page_size: OPTION_LIMIT }),
      ),
    );
    products.value = pages.flatMap((page) => page.list ?? []);
  } catch (error) {
    MessagePlugin.warning(errorText(error, '加载物料下拉失败（可继续用列表与导入）'));
  } finally {
    optionsLoading.value = false;
  }
}

// ==================== 表单（新建 / 编辑） ====================

const dialog = reactive<{ visible: boolean; editing: StandardRow | null; saving: boolean }>({
  visible: false,
  editing: null,
  saving: false,
});

const form = reactive<StandardFormState>(emptyForm());
const errors = ref<StandardFormErrors>({});

/** 标准 BOM 子表列（编辑行用插槽，金额列只读） */
const itemColumns: PrimaryTableCol[] = [
  { colKey: 'component_material_id', title: '耗用物料', width: 260 },
  { colKey: 'std_qty', title: '标准用量', width: 130 },
  { colKey: 'std_price', title: '标准单价', width: 140 },
  { colKey: 'amount', title: '金额（用量×单价）', width: 160 },
  { colKey: 'item_op', title: '操作', width: 80 },
];

/** 编辑一条已生效过的版本：口径字段全部锁定 */
const lockedEditing = computed(() => (dialog.editing ? isLockedEffective(dialog.editing, today) : false));

/** 整期写法时给个窗口提示（让用户看到「这一期到底覆盖到哪天」） */
const periodHint = computed(() => {
  if (form.custom_window) {
    return '按自定义日期';
  }
  const window = periodWindow(form.period_code);
  return window ? `${window.start} ~ ${window.end}` : '期间格式 yyyy-MM';
});

/** 三要素预览（服务端仍是最终口径） */
const preview = computed(() =>
  previewAmounts(form.items, form.std_labor_hours, form.std_labor_rate, form.std_overhead_cost),
);

/** 客户端重叠提示（与后端 66108 同判据；只对已加载的行生效） */
const overlapHint = computed<string>(() => {
  if (!dialog.visible || lockedEditing.value || !form.material_id) {
    return '';
  }
  const window = formWindow();
  if (!window) {
    return '';
  }
  const hit = rows.value.find(
    (row) =>
      row.id !== dialog.editing?.id &&
      row.material_id === form.material_id &&
      row.status === 1 &&
      row.effective_from <= window.end &&
      (row.effective_to === null || row.effective_to >= window.start),
  );
  if (!hit) {
    return '';
  }
  return `该物料已有启用版本 v${hit.version_no}（${windowText(hit.effective_from, hit.effective_to)}）与本窗口重叠：同一期间只能有一套启用标准。要换口径请先把旧版停止日收口到新版生效日前一天，或先失效旧版。`;
});

/** 表单当前窗口（整期 → 期间首末日；自定义 → 原样） */
function formWindow(): { start: string; end: string } | null {
  if (form.custom_window) {
    const start = form.effective_from.trim();
    const end = form.effective_to.trim();
    if (!start) {
      return null;
    }
    return { start, end: end || '9999-12-31' };
  }
  const window = periodWindow(form.period_code);
  return window ? { start: window.start, end: window.end } : null;
}

function resetForm(state: StandardFormState): void {
  Object.assign(form, state);
  errors.value = {};
}

function openCreate(): void {
  dialog.editing = null;
  resetForm(emptyForm());
  dialog.visible = true;
}

async function openEdit(row: StandardRow): Promise<void> {
  try {
    const detail = await fetchStandardDetail(row.id);
    dialog.editing = detail.head;
    resetForm(formFromDetail(detail));
    dialog.visible = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载版本详情失败'));
  }
}

/** 切换「整期 / 自定义」时把当前窗口带过去，避免用户重填 */
function handleWindowModeChange(value: boolean | string | number): void {
  const custom = value === true || value === 'true';
  if (custom && !form.effective_from) {
    const window = periodWindow(form.period_code);
    form.effective_from = window?.start ?? todayText();
    form.effective_to = window?.end ?? '';
  } else if (!custom && form.effective_from) {
    form.period_code = form.effective_from.slice(0, 7);
  }
}

function addItem(): void {
  if (form.items.length >= ITEM_MAX) {
    MessagePlugin.warning(`标准 BOM 最多 ${ITEM_MAX} 行`);
    return;
  }
  form.items.push(emptyItem());
}

function removeItem(key: string): void {
  form.items = form.items.filter((item) => item.key !== key);
}

/** 一行明细的金额预览（四舍五入到 6 位，与库 CHECK 同口径） */
function itemAmountText(item: StandardFormItem): string {
  const qty = toScaled(item.std_qty, AMOUNT_SCALE);
  const price = toScaled(item.std_price, AMOUNT_SCALE);
  if (qty === null || price === null) {
    return '—';
  }
  return previewAmounts([{ std_qty: item.std_qty, std_price: item.std_price }], '', '', '').material;
}

async function submitForm(): Promise<void> {
  const found = validateForm(form, dialog.editing === null);
  errors.value = found;
  if (!isValid(found)) {
    return;
  }
  if (overlapHint.value) {
    MessagePlugin.warning('生效窗口与既有启用版本重叠，请先修改窗口或先失效旧版');
    return;
  }
  dialog.saving = true;
  try {
    if (dialog.editing) {
      const detail = await fetchStandardDetail(dialog.editing.id);
      if (isUnchanged(form, detail)) {
        MessagePlugin.info('没有改动');
        dialog.visible = false;
        return;
      }
      const payload = toUpdatePayload(form, {
        locked: lockedEditing.value,
        originalEffectiveTo: dialog.editing.effective_to,
      });
      await updateStandard(dialog.editing.id, payload);
      MessagePlugin.success('已保存');
    } else {
      await createStandard(toCreatePayload(form));
      MessagePlugin.success('已新增标准成本版本');
    }
    dialog.visible = false;
    await reload();
  } catch (error) {
    MessagePlugin.error(explainWriteError(error, '保存失败'));
  } finally {
    dialog.saving = false;
  }
}

/** 把三个版本护栏业务码翻成「怎么改」，其余原样展示后端人话。 */
function explainWriteError(error: unknown, fallback: string): string {
  const code = errorCodeOf(error);
  if (code === COST_STANDARD_OVERLAP_CODE) {
    return errorText(error, '生效期与同物料其它启用版本重叠');
  }
  if (code === COST_STANDARD_LOCKED_CODE) {
    return errorText(error, '这一版已生效过：只能改停止日 / 状态 / 备注');
  }
  if (code === COST_STANDARD_DELETE_CODE) {
    return errorText(error, '这一版已生效过，不能删除（请改成「失效」）');
  }
  if (code === COST_STANDARD_PUBLISH_GUARD_CODE) {
    // 66111 的报文后端已经写清「当前是什么状态 + 出路是什么」，原样给人看
    return errorText(error, '发布状态不接受这个动作（请刷新后按报文里的出路处理）');
  }
  return errorText(error, fallback);
}

// ==================== 发布审批（M5-10「审核后生效」） ====================

/**
 * 提交发布审批。
 *
 * 界面**只负责发起**：通过与否在审批中心（财务经理）。按钮的可用性由
 * [`publishGate`] 统一判（与后端 `publish_guard` 同判据），不在模板里散着写。
 */
function submitPublish(row: StandardRow): void {
  const gate = publishGate(row);
  if (!gate.canSubmit) {
    MessagePlugin.warning(gate.submitHint);
    return;
  }
  const dialogInstance = DialogPlugin.confirm({
    header: '提交发布审批',
    theme: 'info',
    body: `确认把 ${materialLabel(row)} 的 v${row.version_no}（${windowText(row.effective_from, row.effective_to)}）提交发布审批？审批通过后这一版才参与差异分析取数；审批期间不能改口径。`,
    confirmBtn: { content: '提交', theme: 'primary' },
    onConfirm: async () => {
      dialogInstance.hide();
      try {
        await submitStandardPublish(row.id);
        MessagePlugin.success('已提交发布审批');
        await reload();
      } catch (error) {
        MessagePlugin.error(explainWriteError(error, '提交发布失败'));
      }
    },
  });
}

/** 撤回发布审批（只有发起人本人能撤；后端带「实例已终态」自愈分支）。 */
function withdrawPublish(row: StandardRow): void {
  const gate = publishGate(row);
  if (!gate.canWithdraw) {
    MessagePlugin.warning(gate.withdrawHint);
    return;
  }
  const dialogInstance = DialogPlugin.confirm({
    header: '撤回发布申请',
    theme: 'warning',
    body: `确认撤回 ${materialLabel(row)} 的 v${row.version_no} 的发布申请？撤回后这一版回到草稿，可以改口径后重新提交。`,
    confirmBtn: { content: '撤回', theme: 'warning' },
    onConfirm: async () => {
      dialogInstance.hide();
      try {
        await withdrawStandardPublish(row.id);
        MessagePlugin.success('已撤回，卡回到草稿');
        await reload();
      } catch (error) {
        MessagePlugin.error(explainWriteError(error, '撤回失败'));
      }
    },
  });
}

// ==================== 生效 / 失效 ====================

const statusDialog = reactive<{
  visible: boolean;
  row: StandardRow | null;
  reason: string;
  saving: boolean;
}>({ visible: false, row: null, reason: '', saving: false });

function openStatus(row: StandardRow): void {
  // M5-10：未发布的卡不许手工置生效（后端 66111 也会拦，这里提前拦住并说清出路）
  if (row.status !== 1 && publishGate(row).state !== 'PUBLISHED') {
    MessagePlugin.warning(
      `这一版还没发布（当前「${row.publish_state_label || publishStateLabel(row.publish_state)}」）：\n请先「提交发布」，财务经理审批通过后自动生效。`,
    );
    return;
  }
  statusDialog.row = row;
  statusDialog.reason = '';
  statusDialog.visible = true;
}

async function submitStatus(): Promise<void> {
  const row = statusDialog.row;
  if (!row) {
    return;
  }
  statusDialog.saving = true;
  try {
    const target = row.status === 1 ? 0 : 1;
    const result = await setStandardStatus(row.id, {
      status: target,
      reason: statusDialog.reason.trim() || undefined,
    });
    MessagePlugin.success(`${target === 1 ? '已生效' : '已失效'}${result.message ? `：${result.message}` : ''}`);
    if (result.warnings.length) {
      MessagePlugin.warning(result.warnings.join('；'));
    }
    statusDialog.visible = false;
    await reload();
  } catch (error) {
    MessagePlugin.error(explainWriteError(error, targetText(row) + '失败'));
  } finally {
    statusDialog.saving = false;
  }
}

function targetText(row: StandardRow): string {
  return row.status === 1 ? '失效' : '生效';
}

// ==================== 删除 ====================

function removeRow(row: StandardRow): void {
  if (!canDelete(row, today)) {
    MessagePlugin.warning('已生效过的版本不能删除（它是那段期间口径的凭据），请改成「失效」');
    return;
  }
  const dialogInstance = DialogPlugin.confirm({
    header: '删除标准成本版本',
    theme: 'warning',
    body: `确认删除 ${materialLabel(row)} 的 v${row.version_no}（${windowText(row.effective_from, row.effective_to)}）？删除后不可恢复；已生效过的版本后端会拒绝删除。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialogInstance.hide();
      try {
        await deleteStandard(row.id);
        MessagePlugin.success('已删除');
        await reload();
      } catch (error) {
        MessagePlugin.error(explainWriteError(error, '删除失败'));
      }
    },
  });
}

// ==================== 批量导入 ====================

const importDialog = reactive<{
  visible: boolean;
  file: File | null;
  uploading: boolean;
  error: string;
  result: StandardImportResult | null;
}>({ visible: false, file: null, uploading: false, error: '', result: null });

const fileInput = ref<HTMLInputElement | null>(null);

const issueColumns: PrimaryTableCol[] = [
  { colKey: 'line', title: 'Excel 行号', width: 110 },
  { colKey: 'column', title: '列', width: 140 },
  { colKey: 'message', title: '原因', ellipsis: false },
];

function openImport(): void {
  importDialog.file = null;
  importDialog.error = '';
  importDialog.result = null;
  if (fileInput.value) {
    fileInput.value.value = '';
  }
  importDialog.visible = true;
}

function pickFile(): void {
  fileInput.value?.click();
}

function handleFileChange(event: Event): void {
  const target = event.target as HTMLInputElement;
  const file = target.files?.[0] ?? null;
  importDialog.file = file;
  importDialog.error = '';
  importDialog.result = null;
  if (file && !file.name.toLowerCase().endsWith('.xlsx')) {
    importDialog.error = '只支持 .xlsx（旧版 .xls 请另存为 xlsx 再上传）';
    importDialog.file = null;
  }
}

async function submitImport(): Promise<void> {
  if (!importDialog.file) {
    return;
  }
  importDialog.uploading = true;
  importDialog.error = '';
  importDialog.result = null;
  try {
    const result = await importStandards(importDialog.file);
    importDialog.result = result;
    if (result.success > 0 || result.failed === 0) {
      MessagePlugin.success(importSummaryText(result));
      await reload();
    } else {
      MessagePlugin.error(importSummaryText(result));
    }
  } catch (error) {
    importDialog.error = errorText(error, '导入失败');
  } finally {
    importDialog.uploading = false;
  }
}

async function handleDownloadTemplate(): Promise<void> {
  try {
    const blob = await downloadStandardTemplate();
    saveBlob(blob, '标准成本导入模板_v1.xlsx');
  } catch (error) {
    MessagePlugin.error(errorText(error, '下载模板失败'));
  }
}

onMounted(async () => {
  await loadProducts();
});
</script>

<style scoped>
.cost-standard-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.cs-tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
}

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.name-cell__main {
  display: flex;
  align-items: center;
  gap: 6px;
}

.name-cell__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.num {
  font-variant-numeric: tabular-nums;
}

.num--strong {
  font-weight: 600;
}

.op-cell {
  display: flex;
  gap: 12px;
}

.cs-form__row {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}

.cs-form__row > :deep(.t-form__item) {
  min-width: 240px;
}

.form-note {
  margin-top: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.6;
}

.form-alert {
  margin: 8px 0;
}

.cs-sub {
  margin: 8px 0 16px;
  border: 1px solid var(--td-component-border);
  border-radius: 6px;
  padding: 12px;
}

.cs-sub__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}

.cs-sub__title {
  font-weight: 600;
}

.cs-amounts {
  display: flex;
  align-items: center;
  gap: 14px;
  flex-wrap: wrap;
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 4px;
  font-size: 13px;
}

.cs-amounts__title {
  font-weight: 600;
}

.cs-amounts__total {
  font-weight: 600;
}

.cs-amounts__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.cs-amounts__warn {
  color: var(--td-error-color);
  font-size: 12px;
}

.cs-status__who {
  margin: 0 0 8px;
  font-weight: 600;
}

.cs-guide {
  margin: 8px 0 12px;
  padding-left: 20px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.cs-import__row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.cs-import__file {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.hidden-input {
  display: none;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}
</style>
