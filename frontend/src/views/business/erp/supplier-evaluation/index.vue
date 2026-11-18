<template>
  <div class="sev-page">
    <!-- 没有取数码：页面能进（有页面显形码）但接口会 403 —— 说清缺什么，不摆一堆点不动的按钮 -->
    <t-alert
      v-if="!canList"
      theme="warning"
      class="sev-alert"
      data-testid="sev-no-perm"
      :message="`当前账号没有供应商评估的取数权限（${PERM.list}），请让管理员在角色授权里勾上「查看台账排名」那一行`"
    />

    <t-tabs v-else v-model="activeTab" data-testid="sev-tabs">
      <!-- ==================== Tab 1：评估台账 ==================== -->
      <t-tab-panel value="ledger" label="评估台账">
        <ListPage
          ref="listRef"
          title="供应商评估"
          testid="sev-list"
          table-testid="sev-list-table"
          meta-unit="份"
          :columns="columns"
          :filters="filters"
          :filter-state="filter"
          :fetcher="fetcher"
          :query-builder="queryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #toolbar>
            <t-tooltip
              :content="
                canSave
                  ? '给一家供应商在一个评估期间打分：四维（质量 / 交期 / 价格 / 服务）加权总分由后端算，等级按阈值 A≥90 / B≥80 / C≥70 / D<70 自动判'
                  : `当前账号没有 ${PERM.save} 权限`
              "
              placement="top-right"
            >
              <span class="gate-wrap">
                <t-button theme="primary" :disabled="!canSave" data-testid="sev-new" @click="openCreate">
                  <template #icon><t-icon name="add" /></template>
                  评估录入
                </t-button>
              </span>
            </t-tooltip>
          </template>

          <template #evaluation_no="{ row }">
            <span class="mono">{{ row.evaluation_no }}</span>
          </template>

          <template #supplier="{ row }">
            <div class="cell-stack">
              <span class="cell-strong">{{ row.supplier_display_name || row.supplier_name || '（供应商已删）' }}</span>
              <!-- 快照与现名不同（供应商改过名）时提示一句：评估结论针对「当时那家」 -->
              <span v-if="row.supplier_name && row.supplier_name !== row.supplier_display_name" class="cell-sub">
                当时：{{ row.supplier_name }}
              </span>
            </div>
          </template>

          <template #period="{ row }">
            <div class="cell-stack">
              <span>{{ periodText(row) }}</span>
              <span class="cell-sub mono">{{ row.period_start }} ~ {{ row.period_end }}</span>
            </div>
          </template>

          <template #scores="{ row }">
            <div class="cell-stack">
              <span class="mono">
                质 {{ row.quality_score }} · 交 {{ row.delivery_score }} · 价 {{ row.price_score }} · 服
                {{ row.service_score }}
              </span>
              <span class="cell-sub">权重 {{ weightsText(row) }}</span>
            </div>
          </template>

          <template #total_score="{ row }">
            <div class="cell-stack">
              <span class="cell-total mono">{{ row.total_score }}</span>
              <t-tag :theme="gradeTheme(row.grade)" variant="light" size="small" data-testid="sev-row-grade">
                {{ row.grade_label || '未评级' }}
              </t-tag>
            </div>
          </template>

          <template #evaluator="{ row }">
            <div class="cell-stack">
              <span>{{ row.evaluator_name || '—' }}</span>
              <span class="cell-sub mono">{{ row.eval_date }}</span>
            </div>
          </template>

          <template #op="{ row }">
            <t-space size="small">
              <t-link theme="primary" data-testid="sev-row-detail" @click="openDetail(row)">详情</t-link>
              <t-tooltip
                :content="canSave ? '按这一行的分数与权重重打一次（同期间覆盖，单号不变）' : `当前账号没有 ${PERM.save} 权限`"
                placement="top"
              >
                <span class="gate-wrap">
                  <t-link
                    theme="primary"
                    data-testid="sev-row-regrade"
                    :disabled="!canSave"
                    @click="openFromRow(row)"
                  >
                    重新打分
                  </t-link>
                </span>
              </t-tooltip>
            </t-space>
          </template>
        </ListPage>
      </t-tab-panel>

      <!-- ==================== Tab 2：本期排名 ==================== -->
      <t-tab-panel value="rank" label="本期排名" lazy>
        <t-card :bordered="false" class="sev-card">
          <div class="rank-bar">
            <t-space size="small" break-line>
              <t-button
                v-for="preset in PERIOD_PRESETS"
                :key="preset.key"
                size="small"
                variant="outline"
                data-testid="sev-rank-preset"
                @click="applyPreset(preset.key)"
              >
                {{ preset.label }}
              </t-button>
            </t-space>
            <t-space size="small" break-line>
              <t-date-range-picker
                v-model="rankRange"
                placeholder="评估期间（起 ~ 止）"
                clearable
                data-testid="sev-rank-range"
                @change="loadRank"
              />
              <t-button theme="primary" data-testid="sev-rank-search" :loading="rankLoading" @click="loadRank">
                查询排名
              </t-button>
            </t-space>
          </div>

          <div class="rank-head">
            <span class="rank-head__title" data-testid="sev-rank-title">
              {{ rankMeta.periodLabel || '—' }} · 共 {{ rankMeta.total }} 家参与排名
            </span>
            <span class="rank-head__hint">
              名次按加权总分降序（总分相同按单号先后，名次不并列）；总分与等级都是后端按库层生成列与阈值算的
            </span>
          </div>

          <t-alert v-if="rankError" theme="error" :message="rankError" class="sev-alert" data-testid="sev-rank-error" />

          <t-loading :loading="rankLoading">
            <t-table
              row-key="id"
              :data="rankRows"
              :columns="rankColumns"
              :hover="true"
              size="small"
              data-testid="sev-rank-table"
            >
              <template #rank="{ row }">
                <t-tag :theme="rankTheme(row.rank)" variant="light" size="small">第 {{ row.rank }} 名</t-tag>
              </template>
              <template #supplier="{ row }">
                <span class="cell-strong">{{ row.supplier_display_name || row.supplier_name || '（供应商已删）' }}</span>
              </template>
              <template #total_score="{ row }">
                <span class="cell-total mono">{{ row.total_score }}</span>
              </template>
              <template #scores="{ row }">
                <span class="mono">
                  质 {{ row.quality_score }} · 交 {{ row.delivery_score }} · 价 {{ row.price_score }} · 服
                  {{ row.service_score }}
                </span>
              </template>
              <template #grade="{ row }">
                <t-tag :theme="gradeTheme(row.grade)" variant="light" size="small">{{ row.grade_label || '未评级' }}</t-tag>
              </template>
              <template #evaluator="{ row }">
                <span>{{ row.evaluator_name || '—' }}</span>
              </template>
              <template #op="{ row }">
                <t-link theme="primary" data-testid="sev-rank-detail" @click="openDetail(row)">详情</t-link>
              </template>
              <template #empty>
                <div class="rank-empty" data-testid="sev-rank-empty">
                  该期间还没有评估结论：先在「评估台账」里录入，或换一个期间
                </div>
              </template>
            </t-table>
          </t-loading>
        </t-card>
      </t-tab-panel>
    </t-tabs>

    <!-- ==================== 打分抽屉 ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      :header="formHeader"
      size="720px"
      :footer="false"
      destroy-on-close
      data-testid="sev-form"
    >
      <t-form label-width="120px" class="sev-form">
        <t-form-item label="供应商" required-mark>
          <t-select
            v-model="form.supplier_id"
            filterable
            clearable
            :loading="supplierLoading"
            :options="supplierOptions"
            placeholder="输入编码或名称搜索（含已停用的供应商）"
            data-testid="sev-form-supplier"
            @search="searchSuppliers"
          />
        </t-form-item>

        <t-form-item label="评估期间" required-mark>
          <t-space size="small" break-line>
            <t-date-picker
              v-model="form.period_start"
              value-type="YYYY-MM-DD"
              placeholder="期间起"
              data-testid="sev-form-period-start"
              @change="onPeriodChange"
            />
            <span class="sep">~</span>
            <t-date-picker
              v-model="form.period_end"
              value-type="YYYY-MM-DD"
              placeholder="期间止"
              data-testid="sev-form-period-end"
              @change="onPeriodChange"
            />
            <t-button size="small" variant="outline" data-testid="sev-form-period-this-quarter" @click="applyFormPeriod('quarter')">
              本季度
            </t-button>
            <t-button size="small" variant="outline" data-testid="sev-form-period-this-month" @click="applyFormPeriod('month')">
              本月
            </t-button>
            <t-button size="small" variant="outline" data-testid="sev-form-period-this-year" @click="applyFormPeriod('year')">
              本年
            </t-button>
          </t-space>
        </t-form-item>

        <t-form-item label="评估日">
          <t-date-picker
            v-model="form.eval_date"
            value-type="YYYY-MM-DD"
            placeholder="空 = 今天（不许早于期间起点）"
            clearable
            data-testid="sev-form-eval-date"
          />
        </t-form-item>

        <t-form-item label="四维权重">
          <div class="form-block">
            <t-space size="small" break-line>
              <t-button
                v-for="preset in WEIGHT_PRESETS"
                :key="preset.key"
                size="small"
                variant="outline"
                :title="preset.hint"
                data-testid="sev-form-weight-preset"
                @click="applyWeights(preset.key)"
              >
                {{ preset.label }}
              </t-button>
            </t-space>
            <div class="weight-grid">
              <div v-for="dim in DIMENSIONS" :key="dim" class="weight-cell">
                <span class="weight-label">{{ DIMENSION_LABELS[dim] }}</span>
                <t-input
                  v-model="form.weights[dim]"
                  placeholder="0–1"
                  :data-testid="`sev-form-weight-${dim}`"
                />
              </div>
            </div>
            <div class="weight-sum" :class="{ 'is-bad': !weights.isOne }">
              权重和 <span class="mono">{{ weights.sumText }}</span>
              <span v-if="weights.isOne">（= 1，可以保存）</span>
              <span v-else>（必须**精确**等于 1：四舍五入到 0.9999 也会被后端拒绝）</span>
            </div>
          </div>
        </t-form-item>

        <t-form-item label="四维得分" required-mark>
          <div class="form-block">
            <div class="score-grid">
              <div v-for="dim in DIMENSIONS" :key="dim" class="score-cell">
                <span class="weight-label">{{ DIMENSION_LABELS[dim] }}</span>
                <t-input
                  v-model="form.scores[dim]"
                  placeholder="0–100"
                  :data-testid="`sev-form-score-${dim}`"
                />
              </div>
            </div>
            <div class="score-hint">
              交期可以先取参考分（按真实入库单算准时率），其余三维没有客观数据源，必须人工打分
            </div>
            <t-space size="small" break-line>
              <t-button
                size="small"
                theme="primary"
                variant="outline"
                :loading="suggestionLoading"
                :disabled="!canFetchSuggestion"
                data-testid="sev-form-suggestion"
                @click="loadSuggestion"
              >
                取交期参考分
              </t-button>
              <t-button
                v-if="suggestion?.suggested_score"
                size="small"
                variant="outline"
                data-testid="sev-form-suggestion-fill"
                @click="fillSuggestion"
              >
                填入 {{ suggestion.suggested_score }} 分
              </t-button>
            </t-space>
            <t-alert
              v-if="suggestion"
              theme="info"
              class="sev-alert"
              data-testid="sev-form-suggestion-text"
              :message="suggestionText(suggestion)"
            />
          </div>
        </t-form-item>

        <t-form-item label="备注">
          <t-textarea
            v-model="form.remark"
            :autosize="{ minRows: 2, maxRows: 5 }"
            placeholder="扣分事由 / 改进要求 / 复审约定（选填）"
            data-testid="sev-form-remark"
          />
        </t-form-item>

        <t-form-item label="预览">
          <div class="preview" data-testid="sev-form-preview">
            <span>加权总分（预览）</span>
            <span class="cell-total mono">{{ previewTotalText ?? '—' }}</span>
            <t-tag v-if="previewGradeValue" :theme="gradeTheme(previewGradeValue)" variant="light" size="small">
              {{ previewGradeValue }}
            </t-tag>
            <span class="cell-sub">
              只是前端预览：总分是库层生成列、等级由后端按阈值写，**保存后以后端返回为准**
            </span>
          </div>
        </t-form-item>

        <t-alert
          v-if="formErrorList.length"
          theme="error"
          class="sev-alert"
          data-testid="sev-form-errors"
          :message="formErrorList.join('；')"
        />

        <div class="form-footer">
          <t-space>
            <t-button variant="outline" data-testid="sev-form-cancel" @click="formVisible = false">取消</t-button>
            <t-button
              theme="primary"
              :loading="saving"
              :disabled="!canSave || formErrorList.length > 0"
              data-testid="sev-form-save"
              @click="handleSave"
            >
              保存评估
            </t-button>
          </t-space>
        </div>
      </t-form>
    </t-drawer>

    <!-- ==================== 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="detail ? `评估单 ${detail.evaluation_no}` : '评估单详情'"
      size="640px"
      :footer="false"
      data-testid="sev-detail"
    >
      <t-loading :loading="detailLoading">
        <t-alert v-if="detailError" theme="error" :message="detailError" class="sev-alert" data-testid="sev-detail-error" />
        <template v-if="detail">
          <t-descriptions :column="2" size="small" bordered>
            <t-descriptions-item label="单号">
              <span class="mono">{{ detail.evaluation_no }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="期间">{{ periodText(detail) }}</t-descriptions-item>
            <t-descriptions-item label="供应商">
              <div class="cell-stack">
                <span>{{ detail.supplier_display_name }}</span>
                <span v-if="detail.supplier_name && detail.supplier_name !== detail.supplier_display_name" class="cell-sub">
                  快照：{{ detail.supplier_name }}
                </span>
              </div>
            </t-descriptions-item>
            <t-descriptions-item label="评估日">
              <span class="mono">{{ detail.eval_date }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="评估人">{{ detail.evaluator_name || '—' }}</t-descriptions-item>
            <t-descriptions-item label="创建时间">
              <span class="mono">{{ timeText(detail.created_at) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="加权总分">
              <span class="cell-total mono">{{ detail.total_score }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="等级">
              <t-tag :theme="gradeTheme(detail.grade)" variant="light" size="small">
                {{ detail.grade_label || '未评级' }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="权重" :span="2">
              <span class="mono">{{ weightsText(detail) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="质量 / 交期" :span="2">
              <span class="mono">{{ detail.quality_score }} / {{ detail.delivery_score }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="价格 / 服务" :span="2">
              <span class="mono">{{ detail.price_score }} / {{ detail.service_score }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="备注" :span="2">
              <div class="cell-pre">{{ detail.remark?.trim() ? detail.remark : '—' }}</div>
            </t-descriptions-item>
          </t-descriptions>

          <div class="detail-footer">
            <t-tooltip :content="canSave ? '按这一行的分数与权重重打一次（同期间覆盖）' : `当前账号没有 ${PERM.save} 权限`">
              <span class="gate-wrap">
                <t-button
                  theme="primary"
                  variant="outline"
                  :disabled="!canSave"
                  data-testid="sev-detail-regrade"
                  @click="openFromRow(detail)"
                >
                  以此为模板重新打分
                </t-button>
              </span>
            </t-tooltip>
          </div>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 供应商评估页面（台账 + 本期排名 + 打分录入）。
 *
 * # 三条口径（照抄后端，别在页面里重新发明）
 *
 * | 谁 | 判什么 |
 * |----|--------|
 * | 后端 `grating::save_evaluation` | 总分（库层生成列）+ 等级（`GRADE_THRESHOLDS`）|
 * | 后端 `SaveEvaluationReq::parse` | 权重和 = 1 / 分数 0–100 / 期间与评估日 |
 * | 本页 | 只做「同样的判据先拦一遍」+ 预览；**不**自己算总分入库、**不**自己写等级 |
 *
 * # 一个必须记住的业务事实
 *
 * 同一供应商同一期间**只有一份评估**：再点一次「评估录入」并保存 = 覆盖（单号不变）。
 * 所以页面文案写「重新打分」而不是「新建」；后端返回 `created=false` 时提示也是覆盖。
 */
import { computed, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListPageData } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { fetchSupplierPage, type Supplier } from '@/api/business/erp';
import {
  fetchDeliverySuggestion,
  fetchEvaluationDetail,
  fetchEvaluationPage,
  fetchEvaluationRank,
  saveEvaluation,
  type DeliverySuggestion,
  type EvaluationQuery,
  type EvaluationRow,
  type RankRow,
} from '@/api/business/erp/supplier-evaluation';
import {
  DIMENSIONS,
  DIMENSION_LABELS,
  GRADE_OPTIONS,
  PAGE_SIZE,
  PERIOD_PRESETS,
  WEIGHT_PRESETS,
  buildEvaluationQuery,
  buildSavePayload,
  currentMonthRange,
  currentQuarterRange,
  currentYearRange,
  emptyFilter,
  emptyForm,
  formErrors,
  formFromRow,
  gradeTheme,
  keywordWarning,
  periodText,
  previewGrade,
  previewTotal,
  rankTheme,
  suggestionText,
  timeText,
  weightsState,
  weightsText,
  type EvaluationFilterState,
  type EvaluationFormState,
  type PeriodRange,
} from './supplier-evaluation-logic';

/** 本页用到的权限码（与 `V0457` 的 `sys_menu.auth_code` 逐字一致） */
const PERM = {
  /** 页面显形（**只在路由 meta 用**，本文件不拿它判断任何东西） */
  page: 'business:erp:supplier-evaluation:page',
  /** 台账 / 排名 / 详情（按钮行 …4496） */
  list: 'business:erp:supplier-evaluation:list',
  /** 打分录入 + 交期参考分（按钮行 …4497） */
  save: 'business:erp:supplier-evaluation:save',
} as const;

const userStore = useUserStore();
const canList = computed(() => userStore.hasPermission(PERM.list));
const canSave = computed(() => userStore.hasPermission(PERM.save));

const activeTab = ref<'ledger' | 'rank'>('ledger');

/** 错误文案归一（后端 400 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error) {
    return error.message || fallback;
  }
  return fallback;
}

// ==================== 台账列表 ====================

const listRef = ref<InstanceType<typeof ListPage> | null>(null);
const filter = reactive<EvaluationFilterState>(emptyFilter());

const filters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '单号 / 供应商名 / 备注',
    width: 320,
    testid: 'sev-filter-keyword',
  },
  {
    key: 'grade',
    type: 'select',
    options: GRADE_OPTIONS,
    placeholder: '全部等级',
    width: 160,
    searchOnChange: true,
    testid: 'sev-filter-grade',
  },
];

const queryBuilder = (state: EvaluationFilterState, page: number, pageSize: number) =>
  buildEvaluationQuery(state, page, pageSize);

const columns: ColumnDef[] = [
  { colKey: 'evaluation_no', title: '评估单号', width: 170, fixed: 'left' },
  { colKey: 'supplier', title: '供应商', minWidth: 180 },
  { colKey: 'period', title: '评估期间', width: 180 },
  { colKey: 'scores', title: '四维得分 / 权重', minWidth: 260 },
  { colKey: 'total_score', title: '总分 / 等级', width: 130 },
  { colKey: 'evaluator', title: '评估人 / 评估日', width: 150 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

const fetcher: ListFetcher<EvaluationRow> = async (query): Promise<ListPageData<EvaluationRow>> => {
  const warn = keywordWarning(filter.keyword);
  if (warn) {
    // 后端会 400；先给一条本地提示，再把请求照发 —— 让用户看到后端的原文案
    MessagePlugin.warning(warn);
  }
  try {
    return await fetchEvaluationPage(query as EvaluationQuery);
  } catch (error) {
    throw new Error(errorText(error, '取供应商评估台账失败'));
  }
};

async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

// ==================== 供应商下拉（懒加载 + 远端搜索） ====================

/**
 * 供应商候选。**不按 status 过滤**：停用的供应商也可能要评估（评估是「过去这个期间表现如何」，
 * 与现在能不能下单无关），过滤掉启用态会让被拉黑的供应商永远打不了分。
 */
const supplierOptions = ref<{ label: string; value: string }[]>([]);
const supplierLoading = ref(false);
let supplierLoaded = false;

async function loadSuppliers(keyword = ''): Promise<void> {
  supplierLoading.value = true;
  try {
    const page = await fetchSupplierPage({ keyword: keyword.trim() || undefined, page: 1, page_size: 50 });
    supplierOptions.value = (page.list ?? []).map((item: Supplier) => ({
      label: `${item.supplier_code} ${item.supplier_name}${item.status === 1 ? '' : '（停用）'}`,
      value: item.id,
    }));
    supplierLoaded = true;
  } catch (error) {
    if (!supplierLoaded) {
      MessagePlugin.warning(errorText(error, '供应商下拉加载失败：可能缺 business:erp:supplier:list 权限'));
    }
  } finally {
    supplierLoading.value = false;
  }
}

function searchSuppliers(keyword: string): void {
  void loadSuppliers(keyword);
}

/** 回填时把当前供应商塞进候选（否则远端搜索结果里没有它，选择框显示空白） */
function ensureSupplierOption(row: Pick<EvaluationRow, 'supplier_id' | 'supplier_display_name'>): void {
  if (supplierOptions.value.some((item) => item.value === row.supplier_id)) {
    return;
  }
  supplierOptions.value = [
    { label: row.supplier_display_name || row.supplier_id, value: row.supplier_id },
    ...supplierOptions.value,
  ];
}

// ==================== 打分表单 ====================

const formVisible = ref(false);
const saving = ref(false);
const form = reactive<EvaluationFormState>(emptyForm());

const formHeader = computed(() =>
  form.supplier_id && form.period_start && form.period_end
    ? '评估录入（同一家供应商同一期间只有一份结论：已存在就覆盖，单号不变）'
    : '评估录入',
);

const weights = computed(() => weightsState(form.weights));
const previewTotalText = computed(() => previewTotal(form.weights, form.scores));
const previewGradeValue = computed(() => previewGrade(previewTotalText.value));
const formErrorList = computed(() => formErrors(form));

function openCreate(): void {
  Object.assign(form, emptyForm());
  suggestion.value = null;
  if (!supplierLoaded) {
    void loadSuppliers();
  }
  formVisible.value = true;
}

function openFromRow(row: EvaluationRow): void {
  Object.assign(form, formFromRow(row));
  suggestion.value = null;
  ensureSupplierOption(row);
  if (!supplierLoaded) {
    void loadSuppliers();
  }
  formVisible.value = true;
}

function applyWeights(key: string): void {
  const preset = WEIGHT_PRESETS.find((item) => item.key === key);
  if (!preset) {
    return;
  }
  Object.assign(form.weights, preset.weights);
}

function applyFormPeriod(kind: 'month' | 'quarter' | 'year'): void {
  const range: PeriodRange =
    kind === 'month' ? currentMonthRange() : kind === 'quarter' ? currentQuarterRange() : currentYearRange();
  form.period_start = range.start;
  form.period_end = range.end;
  suggestion.value = null;
}

function onPeriodChange(): void {
  // 期间变了，之前取的参考分就不再对应这段期间 —— 清掉，避免「拿上季度的准时率填这一季」
  suggestion.value = null;
}

const handleSave = async (): Promise<void> => {
  if (formErrorList.value.length) {
    MessagePlugin.warning(formErrorList.value[0]);
    return;
  }
  saving.value = true;
  try {
    const result = await saveEvaluation(buildSavePayload(form));
    MessagePlugin.success(
      result.created
        ? `已新建评估 ${result.row.evaluation_no}：总分 ${result.row.total_score}，等级 ${result.row.grade_label}`
        : `已覆盖评估 ${result.row.evaluation_no}（同期间重新打分）：总分 ${result.row.total_score}，等级 ${result.row.grade_label}`,
    );
    formVisible.value = false;
    await reloadList();
    if (activeTab.value === 'rank') {
      await loadRank();
    }
  } catch (err) {
    MessagePlugin.error(errorText(err, '保存评估失败'));
  } finally {
    saving.value = false;
  }
};

// ==================== 交期参考分 ====================

const suggestion = ref<DeliverySuggestion | null>(null);
const suggestionLoading = ref(false);

const canFetchSuggestion = computed(
  () => Boolean(form.supplier_id && form.period_start && form.period_end && form.period_start <= form.period_end),
);

async function loadSuggestion(): Promise<void> {
  if (!canFetchSuggestion.value) {
    MessagePlugin.warning('先选供应商与评估期间，再取参考分');
    return;
  }
  suggestionLoading.value = true;
  try {
    suggestion.value = await fetchDeliverySuggestion({
      supplier_id: form.supplier_id,
      period_start: form.period_start,
      period_end: form.period_end,
    });
  } catch (error) {
    MessagePlugin.error(errorText(error, '取交期参考分失败'));
  } finally {
    suggestionLoading.value = false;
  }
}

function fillSuggestion(): void {
  const score = suggestion.value?.suggested_score;
  if (!score) {
    return;
  }
  form.scores.delivery = score;
}

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<EvaluationRow | null>(null);

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  try {
    detail.value = await fetchEvaluationDetail(id);
  } catch (error) {
    detail.value = null;
    detailError.value = errorText(error, '加载评估单详情失败');
  } finally {
    detailLoading.value = false;
  }
}

function openDetail(row: EvaluationRow): void {
  detail.value = row;
  detailVisible.value = true;
  void loadDetail(row.id);
}

// ==================== 本期排名 ====================

const rankRange = ref<string[]>((() => {
  const range = currentQuarterRange();
  return [range.start, range.end];
})());
const rankRows = ref<RankRow[]>([]);
const rankLoading = ref(false);
const rankError = ref('');
const rankMeta = reactive({ total: 0, periodLabel: '' });

const rankColumns: ColumnDef[] = [
  { colKey: 'rank', title: '名次', width: 100, fixed: 'left' },
  { colKey: 'supplier', title: '供应商', minWidth: 200 },
  { colKey: 'total_score', title: '加权总分', width: 110 },
  { colKey: 'scores', title: '四维得分', minWidth: 240 },
  { colKey: 'grade', title: '等级', width: 110 },
  { colKey: 'evaluator', title: '评估人', width: 120 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

function applyPreset(key: string): void {
  const preset = PERIOD_PRESETS.find((item) => item.key === key);
  if (!preset) {
    return;
  }
  const range = preset.range();
  rankRange.value = [range.start, range.end];
  void loadRank();
}

async function loadRank(): Promise<void> {
  rankError.value = '';
  const [start, end] = rankRange.value ?? [];
  if (!start || !end) {
    rankError.value = '请选择评估期间（排名是同一期间同一口径下的比较，不能空着查）';
    rankRows.value = [];
    rankMeta.total = 0;
    rankMeta.periodLabel = '';
    return;
  }
  rankLoading.value = true;
  try {
    const result = await fetchEvaluationRank({ period_start: start, period_end: end, limit: 200 });
    rankRows.value = result.rows ?? [];
    rankMeta.total = result.total ?? 0;
    rankMeta.periodLabel = result.period_label ?? `${result.period_start} ~ ${result.period_end}`;
  } catch (error) {
    rankRows.value = [];
    rankMeta.total = 0;
    rankMeta.periodLabel = '';
    rankError.value = errorText(error, '加载本期排名失败');
  } finally {
    rankLoading.value = false;
  }
}

/** 首屏就把排名拉一次（默认本季度），切 tab 时不必再点一次「查询」 */
void loadRank();
</script>

<style scoped>
.sev-page {
  padding: 0;
}

.sev-alert {
  margin-bottom: 12px;
}

.sev-card {
  background: transparent;
}

.rank-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.rank-head {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 8px;
}

.rank-head__title {
  font-weight: 600;
}

.rank-head__hint,
.cell-sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.rank-empty {
  padding: 24px 0;
  color: var(--td-text-color-placeholder);
}

.gate-wrap {
  display: inline-block;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.cell-strong {
  font-weight: 500;
}

.cell-total {
  font-size: 16px;
  font-weight: 600;
}

.cell-pre {
  white-space: pre-wrap;
  word-break: break-all;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

.sep {
  color: var(--td-text-color-placeholder);
}

.sev-form {
  padding-right: 8px;
}

.form-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}

.weight-grid,
.score-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px;
}

.weight-cell,
.score-cell {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.weight-label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.weight-sum {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.weight-sum.is-bad {
  color: var(--td-error-color);
}

.score-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.preview {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.form-footer,
.detail-footer {
  display: flex;
  justify-content: flex-end;
  padding-top: 12px;
}
</style>
