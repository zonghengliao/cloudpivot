<template>
  <div class="sales-target-page">
    <!-- ==================== 页面头 ==================== -->
    <t-card :bordered="false" class="page-card">
      <div class="page-head">
        <div class="page-head__title">
          <span>销售目标与预测</span>
          <t-tag size="small" variant="light" theme="primary">{{ periodCode || '未选期间' }}</t-tag>
        </div>
        <span class="page-head__tip">
          目标按「范围 + 期间」唯一（全公司 / 销售员 / 部门 / 物料）；看板<b>两个口径</b>一起算：
          <code>按订单金额</code>（APPROVED 及之后的订单含税额）与
          <code>按已收款金额</code>（期间内确认/过账的收款核销额 − 退款）。达成率 = 实际 ÷ 目标 × 100，
          缺口 = 目标 − 实际（<b>负数表示超额</b>）。
        </span>
      </div>

      <t-tabs v-model="activeTab" data-testid="sales-target-tabs">
        <!-- ==================== ① 达成率看板 ==================== -->
        <t-tab-panel value="board" label="达成率看板">
          <div class="filter-row">
            <t-input
              v-model="periodCode"
              class="filter-row__period"
              placeholder="期间：2026-11 或 2026-Q4"
              data-testid="board-period"
              @enter="loadBoard"
            />
            <t-select
              v-model="boardScopeType"
              class="filter-row__scope"
              :options="scopeOptions"
              data-testid="board-scope"
            />
            <t-radio-group v-model="basis" variant="default-filled" data-testid="board-basis">
              <t-radio-button v-for="item in basisOptions" :key="item.value" :value="item.value">
                {{ item.label }}
              </t-radio-button>
            </t-radio-group>
            <t-button theme="primary" data-testid="btn-board-search" @click="loadBoard">刷新</t-button>
            <span class="filter-row__spacer" />
            <t-tag variant="light" theme="default" data-testid="board-basis-tip">
              当前展示：{{ basis === 'RECEIPT' ? '按已收款金额' : '按订单金额' }}
            </t-tag>
          </div>

          <t-alert
            v-if="boardError"
            theme="error"
            :message="boardError"
            class="page-alert"
            data-testid="board-error"
          />

          <!-- 汇总卡 -->
          <div class="summary-row">
            <t-card :bordered="true" class="summary-card">
              <div class="summary-card__label">目标合计</div>
              <div class="summary-card__value mono" data-testid="summary-target">
                {{ formatMoney(boardSummary.target) }}
              </div>
            </t-card>
            <t-card :bordered="true" class="summary-card">
              <div class="summary-card__label">实际合计（当前口径）</div>
              <div class="summary-card__value mono" data-testid="summary-actual">
                {{ formatMoney(boardSummary.actual) }}
              </div>
            </t-card>
            <t-card :bordered="true" class="summary-card">
              <div class="summary-card__label">综合达成率</div>
              <div class="summary-card__value mono" data-testid="summary-rate">
                {{ formatRate(boardSummary.rate) }}
              </div>
            </t-card>
            <t-card :bordered="true" class="summary-card">
              <div class="summary-card__label">缺口合计（负数 = 超额）</div>
              <div class="summary-card__value mono" data-testid="summary-gap">
                {{ formatMoney(boardSummary.gap) }}
              </div>
            </t-card>
          </div>

          <t-table
            row-key="target_no"
            size="small"
            hover
            :data="boardRows"
            :columns="boardColumns"
            :loading="boardLoading"
            :empty="boardEmptyText"
            data-testid="board-table"
          >
            <template #scope="{ row }">
              <div class="scope-cell">
                <span class="scope-cell__name" data-testid="board-scope-name">{{ row.scope }}</span>
                <t-tag size="small" variant="outline" theme="default">
                  {{ scopeLabelOf(row.scope_type) }}
                </t-tag>
              </div>
            </template>
            <template #target_amount="{ row }">
              <span class="mono">{{ formatMoney(row.target_amount) }}</span>
            </template>
            <template #actual_amount="{ row }">
              <span class="mono">{{ formatMoney(row.actual_amount) }}</span>
            </template>
            <template #achievement="{ row }">
              <div class="rate-cell">
                <t-progress
                  theme="line"
                  :percentage="progressPercent(row.achievement_rate)"
                  :label="false"
                  :color="rateColor(row.achievement_rate)"
                  data-testid="board-progress"
                />
                <span class="mono rate-cell__text" data-testid="board-rate">
                  {{ formatRate(row.achievement_rate) }}
                </span>
              </div>
            </template>
            <template #gap="{ row }">
              <span
                class="mono"
                :class="Number(row.gap) > 0 ? 'gap-short' : 'gap-over'"
                data-testid="board-gap"
              >
                {{ formatMoney(row.gap) }}
              </span>
            </template>
            <template #other_basis="{ row }">
              <span class="mono muted" data-testid="board-other-basis">
                {{ formatMoney(otherBasisOf(row).actual_amount) }}
                （{{ formatRate(otherBasisOf(row).achievement_rate) }}）
              </span>
            </template>
          </t-table>
        </t-tab-panel>

        <!-- ==================== ② 目标维护 ==================== -->
        <t-tab-panel value="target" label="目标维护">
          <div class="filter-row">
            <t-input
              v-model="targetFilter.period_code"
              class="filter-row__period"
              placeholder="期间：2026-11 / 2026-Q4"
              clearable
              data-testid="target-filter-period"
              @enter="loadTargets"
            />
            <t-select
              v-model="targetFilter.scope_type"
              class="filter-row__scope"
              :options="scopeOptionsWithAll"
              placeholder="全部范围"
              clearable
              data-testid="target-filter-scope"
            />
            <t-select
              v-model="targetFilter.status"
              class="filter-row__status"
              :options="statusOptionsWithAll"
              placeholder="全部状态"
              clearable
              data-testid="target-filter-status"
            />
            <t-input
              v-model="targetFilter.keyword"
              class="filter-row__keyword"
              placeholder="编号 / 范围名 / 备注"
              clearable
              data-testid="target-filter-keyword"
              @enter="loadTargets"
            />
            <t-button theme="primary" data-testid="btn-target-search" @click="loadTargets">查询</t-button>
            <t-button variant="outline" data-testid="btn-target-reset" @click="resetTargetFilter">
              重置
            </t-button>
            <span class="filter-row__spacer" />
            <t-button theme="primary" data-testid="btn-target-new" @click="openTargetCreate">
              <template #icon><t-icon name="add" /></template>
              新建目标
            </t-button>
          </div>

          <t-alert
            v-if="targetError"
            theme="error"
            :message="targetError"
            class="page-alert"
            data-testid="target-error"
          />

          <t-table
            row-key="id"
            size="small"
            hover
            :data="targetRows"
            :columns="targetColumns"
            :loading="targetLoading"
            :empty="targetEmptyText"
            data-testid="target-table"
          >
            <template #target_no="{ row }">
              <span class="mono">{{ row.target_no }}</span>
            </template>
            <template #scope="{ row }">
              <div class="scope-cell">
                <span>{{ row.scope_name || '全公司' }}</span>
                <t-tag size="small" variant="outline" theme="default">
                  {{ scopeLabelOf(row.scope_type) }}
                </t-tag>
              </div>
            </template>
            <template #period_code="{ row }">
              <span class="mono">{{ row.period_code }}</span>
            </template>
            <template #target_amount="{ row }">
              <span class="mono">{{ formatMoney(row.target_amount) }}</span>
            </template>
            <template #target_qty="{ row }">
              <span class="mono">{{ Number(row.target_qty) > 0 ? row.target_qty : '—' }}</span>
            </template>
            <template #status="{ row }">
              <t-tag size="small" variant="light" :theme="row.status === 'ACTIVE' ? 'success' : 'default'">
                {{ row.status === 'ACTIVE' ? '启用' : '停用' }}
              </t-tag>
            </template>
            <template #updated_at="{ row }">
              <span class="mono muted">{{ formatTime(row.updated_at) }}</span>
            </template>
            <template #op="{ row }">
              <div class="op-row">
                <t-link v-privilege="[PERM.update]" theme="primary" data-testid="btn-target-edit" @click="openTargetEdit(row)">
                  编辑
                </t-link>
                <t-link
                  v-privilege="[PERM.update]"
                  :theme="row.status === 'ACTIVE' ? 'warning' : 'success'"
                  data-testid="btn-target-toggle"
                  @click="toggleTarget(row)"
                >
                  {{ row.status === 'ACTIVE' ? '停用' : '启用' }}
                </t-link>
                <t-link v-privilege="[PERM.delete]" theme="danger" data-testid="btn-target-delete" @click="confirmDeleteTarget(row)">
                  删除
                </t-link>
              </div>
            </template>
          </t-table>

          <div class="pager-row">
            <t-pagination
              v-model="targetFilter.page"
              v-model:page-size="targetFilter.page_size"
              :total="targetTotal"
              :page-size-options="[10, 20, 50]"
              data-testid="target-pager"
              @change="loadTargets"
            />
          </div>
        </t-tab-panel>

        <!-- ==================== ③ 销售预测 ==================== -->
        <t-tab-panel value="forecast" label="销售预测">
          <div class="filter-row">
            <t-input
              v-model="forecastFilter.period_code"
              class="filter-row__period"
              placeholder="期间：2026-11 / 2026-Q4"
              clearable
              data-testid="forecast-filter-period"
              @enter="loadForecasts"
            />
            <t-select
              v-model="forecastFilter.confidence"
              class="filter-row__status"
              :options="confidenceOptionsWithAll"
              placeholder="全部信心度"
              clearable
              data-testid="forecast-filter-confidence"
            />
            <t-button theme="primary" data-testid="btn-forecast-search" @click="loadForecasts">
              查询
            </t-button>
            <span class="filter-row__spacer" />
            <t-button theme="primary" data-testid="btn-forecast-new" @click="openForecastCreate">
              <template #icon><t-icon name="add" /></template>
              新建预测
            </t-button>
          </div>

          <t-alert
            v-if="forecastError"
            theme="error"
            :message="forecastError"
            class="page-alert"
            data-testid="forecast-error"
          />

          <t-table
            row-key="id"
            size="small"
            hover
            :data="forecastRows"
            :columns="forecastColumns"
            :loading="forecastLoading"
            :empty="forecastEmptyText"
            data-testid="forecast-table"
          >
            <template #forecast_no="{ row }">
              <span class="mono">{{ row.forecast_no }}</span>
            </template>
            <template #dimension="{ row }">
              <span>{{ forecastDimension(row) }}</span>
            </template>
            <template #period_code="{ row }">
              <span class="mono">{{ row.period_code }}</span>
            </template>
            <template #forecast_amount="{ row }">
              <span class="mono">{{ formatMoney(row.forecast_amount) }}</span>
            </template>
            <template #confidence="{ row }">
              <t-tag size="small" variant="light" :theme="confidenceTheme(row.confidence)">
                {{ confidenceLabel(row.confidence) }}
              </t-tag>
            </template>
            <template #version_no="{ row }">
              <span class="mono">v{{ row.version_no }}</span>
            </template>
            <template #op="{ row }">
              <div class="op-row">
                <t-link
                  v-privilege="[PERM.forecastUpdate]"
                  theme="primary"
                  data-testid="btn-forecast-edit"
                  @click="openForecastEdit(row)"
                >
                  编辑
                </t-link>
                <t-link
                  v-privilege="[PERM.forecastDelete]"
                  theme="danger"
                  data-testid="btn-forecast-delete"
                  @click="confirmDeleteForecast(row)"
                >
                  删除
                </t-link>
              </div>
            </template>
          </t-table>

          <div class="pager-row">
            <t-pagination
              v-model="forecastFilter.page"
              v-model:page-size="forecastFilter.page_size"
              :total="forecastTotal"
              :page-size-options="[10, 20, 50]"
              data-testid="forecast-pager"
              @change="loadForecasts"
            />
          </div>
        </t-tab-panel>
      </t-tabs>
    </t-card>

    <!-- ==================== 目标弹窗 ==================== -->
    <t-dialog
      v-model:visible="targetFormVisible"
      :header="editingTargetId ? '编辑销售目标' : '新建销售目标'"
      width="600px"
      :confirm-btn="{ content: editingTargetId ? '保存' : '创建', loading: targetSubmitting }"
      :close-on-overlay-click="false"
      data-testid="target-form-dialog"
      @confirm="submitTargetForm"
    >
      <t-alert v-if="targetFormError" theme="error" :message="targetFormError" class="page-alert" />
      <t-form label-width="110px" class="form-body">
        <t-form-item label="范围类型" required-mark>
          <t-select v-model="targetForm.scope_type" :options="scopeOptions" @change="onScopeTypeChange" />
        </t-form-item>
        <t-form-item v-if="targetForm.scope_type !== 'ALL'" label="范围对象" required-mark>
          <t-select
            v-if="!scopeOptionsFailed"
            v-model="targetForm.scope_id"
            :options="scopeObjectOptions"
            filterable
            placeholder="选择对象"
            data-testid="target-form-scope-id"
          />
          <t-input v-else v-model="targetForm.scope_id" placeholder="粘贴对象 UUID" />
          <span v-if="scopeOptionsFailed" class="form-hint">
            候选拉不到（缺 system:user:list / dept / material 查看权限）：请直接粘贴 UUID。
          </span>
        </t-form-item>
        <t-form-item v-else label="范围对象">
          <span class="form-hint">全公司目标不绑定对象（后端要求 scope_id 为空）</span>
        </t-form-item>
        <t-form-item label="期间码" required-mark>
          <t-input v-model="targetForm.period_code" placeholder="2026-11 或 2026-Q4" data-testid="target-form-period" />
        </t-form-item>
        <t-form-item label="目标金额" required-mark>
          <t-input-number v-model="targetForm.target_amount" :min="0" :decimal-places="2" theme="normal" data-testid="target-form-amount" />
        </t-form-item>
        <t-form-item label="目标数量">
          <t-input-number v-model="targetForm.target_qty" :min="0" :decimal-places="4" theme="normal" data-testid="target-form-qty" />
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="targetForm.status">
            <t-radio value="ACTIVE">启用</t-radio>
            <t-radio value="INACTIVE">停用</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="targetForm.remark" :maxlength="200" placeholder="选填" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== 预测弹窗 ==================== -->
    <t-dialog
      v-model:visible="forecastFormVisible"
      :header="editingForecastId ? '编辑销售预测' : '新建销售预测'"
      width="600px"
      :confirm-btn="{ content: editingForecastId ? '保存' : '创建', loading: forecastSubmitting }"
      :close-on-overlay-click="false"
      data-testid="forecast-form-dialog"
      @confirm="submitForecastForm"
    >
      <t-alert v-if="forecastFormError" theme="error" :message="forecastFormError" class="page-alert" />
      <t-form label-width="110px" class="form-body">
        <t-form-item label="客户">
          <t-select
            v-model="forecastForm.customer_id"
            :options="customerOptions"
            filterable
            clearable
            placeholder="可空（与物料同时为空 = 全局预测）"
          />
        </t-form-item>
        <t-form-item label="物料">
          <t-select
            v-model="forecastForm.material_id"
            :options="materialOptions"
            filterable
            clearable
            placeholder="可空"
          />
        </t-form-item>
        <t-form-item label="期间码" required-mark>
          <t-input v-model="forecastForm.period_code" placeholder="2026-11 或 2026-Q4" data-testid="forecast-form-period" />
        </t-form-item>
        <t-form-item label="预测金额" required-mark>
          <t-input-number v-model="forecastForm.forecast_amount" :min="0" :decimal-places="2" theme="normal" data-testid="forecast-form-amount" />
        </t-form-item>
        <t-form-item label="信心度">
          <t-radio-group v-model="forecastForm.confidence">
            <t-radio v-for="item in confidenceOptions" :key="item.value" :value="item.value">
              {{ item.label }}
            </t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="版本号">
          <t-input-number v-model="forecastForm.version_no" :min="1" :decimal-places="0" theme="normal" />
          <span class="form-hint">同一维度同一期间的新一轮预测请用新版本号（v2、v3…）</span>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="forecastForm.remark" :maxlength="200" placeholder="选填" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 销售目标与预测（SO-44 / ARCH-7 §15.8）
 *
 * # 三个页签
 *
 * | 页签 | 干什么 | 关键点 |
 * |------|--------|--------|
 * | 达成率看板 | 目标 vs 实际（进度条 + 缺口） | **两个口径**（订单 / 已收款）一次请求都拿到，切口径不重发 |
 * | 目标维护 | 建 / 改 / 停用 / 软删目标 | `ALL` 不带 scope_id；「范围 + 期间」唯一（409 原文回显） |
 * | 销售预测 | 滚动预测（客户 / 物料 / 全局 × 版本） | 同维度同期间同版本唯一 |
 *
 * # 看板数字怎么读
 *
 * ```text
 * 达成率 = 实际 ÷ 目标 × 100     （后端算，前端只展示；可能 > 100 = 超额）
 * 缺口   = 目标 − 实际           （正 = 还差这么多，负 = 超额这么多）
 * ```
 *
 * 进度条夹到 100%（超额时条满但数字照实）；缺口颜色：正红（差）/ 负绿（超）。
 *
 * # 权限
 *
 * 6 个操作按钮走 `v-privilege`（仅隐藏按钮，接口侧后端再判一次）；
 * 看板与目标列表共用 `business:erp:sales-target:list`。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { fetchCustomerPage, fetchMaterialPage, type MaterialRow } from '@/api/business/erp';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import {
  BASIS_OPTIONS,
  CONFIDENCE_OPTIONS,
  SCOPE_OPTIONS,
  TARGET_STATUS_OPTIONS,
  createForecast,
  createTarget,
  deleteForecast,
  deleteTarget,
  fetchAchievement,
  fetchForecastPage,
  fetchTargetPage,
  formatMoney,
  formatRate,
  progressPercent,
  updateForecast,
  updateTarget,
  type AchievementBasisKey,
  type AchievementBasisResult,
  type AchievementRow,
  type Confidence,
  type ForecastRow,
  type ScopeType,
  type TargetRow,
  type TargetStatus,
} from '@/api/business/erp/sales-target';

/** 本页权限码（与 `V0318__sales_target_forecast.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:sales-target:list',
  create: 'business:erp:sales-target:create',
  update: 'business:erp:sales-target:update',
  delete: 'business:erp:sales-target:delete',
  forecastList: 'business:erp:sales-forecast:list',
  forecastCreate: 'business:erp:sales-forecast:create',
  forecastUpdate: 'business:erp:sales-forecast:update',
  forecastDelete: 'business:erp:sales-forecast:delete',
} as const;

const scopeOptions = SCOPE_OPTIONS;
const scopeOptionsWithAll = SCOPE_OPTIONS;
const basisOptions = BASIS_OPTIONS;
const confidenceOptions = CONFIDENCE_OPTIONS;
const statusOptionsWithAll = TARGET_STATUS_OPTIONS;
const confidenceOptionsWithAll = CONFIDENCE_OPTIONS;

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（只展示，不换算时区） */
function formatTime(raw: string): string {
  if (!raw) {
    return '—';
  }
  return raw.replace('T', ' ').slice(0, 16);
}

/** 默认期间：本月（北京时间口径，够用即可） */
function currentPeriod(): string {
  const now = new Date();
  const month = `${now.getMonth() + 1}`.padStart(2, '0');
  return `${now.getFullYear()}-${month}`;
}

function scopeLabelOf(value: ScopeType): string {
  return scopeOptions.find((item) => item.value === value)?.label ?? value;
}

function confidenceLabel(value: Confidence): string {
  return confidenceOptions.find((item) => item.value === value)?.label ?? value;
}

function confidenceTheme(value: Confidence): 'success' | 'warning' | 'danger' {
  if (value === 'HIGH') {
    return 'success';
  }
  return value === 'MEDIUM' ? 'warning' : 'danger';
}

function rateColor(value: unknown): string {
  const num = Number(value ?? 0);
  if (num >= 100) {
    return 'var(--td-success-color)';
  }
  if (num >= 60) {
    return 'var(--td-warning-color)';
  }
  return 'var(--td-error-color)';
}

// ==================== 页签 ====================
const activeTab = ref<'board' | 'target' | 'forecast'>('board');

// ==================== 看板 ====================
const periodCode = ref(currentPeriod());
const boardScopeType = ref<ScopeType>('SALESPERSON');
const basis = ref<AchievementBasisKey>('ORDER');
const boardRows = ref<AchievementRow[]>([]);
const boardLoading = ref(false);
const boardError = ref('');

const boardColumns = [
  { colKey: 'scope', title: '范围', width: 200 },
  { colKey: 'target_amount', title: '目标金额', width: 150, align: 'right' as const },
  { colKey: 'actual_amount', title: '实际金额（当前口径）', width: 170, align: 'right' as const },
  { colKey: 'achievement', title: '达成率', minWidth: 220 },
  { colKey: 'gap', title: '缺口（负数=超额）', width: 160, align: 'right' as const },
  { colKey: 'other_basis', title: '另一口径', minWidth: 180 },
];

const boardEmptyText = computed(() => {
  if (boardLoading.value) {
    return '加载中…';
  }
  if (!periodCode.value) {
    return '请先填期间（如 2026-11 或 2026-Q4）';
  }
  return `期间 ${periodCode.value} 下没有「${scopeLabelOf(boardScopeType.value)}」的启用目标 —— 去「目标维护」页签建一条`;
});

const boardSummary = computed(() => {
  let target = 0;
  let actual = 0;
  let gap = 0;
  for (const row of boardRows.value) {
    target += Number(row.target_amount ?? 0);
    actual += Number(row.actual_amount ?? 0);
    gap += Number(row.gap ?? 0);
  }
  return {
    target,
    actual,
    gap,
    rate: target > 0 ? (actual / target) * 100 : 0,
  };
});

/** 当前口径的另一面（切到订单就看收款，反之亦然） */
function otherBasisOf(row: AchievementRow): AchievementBasisResult {
  return basis.value === 'RECEIPT' ? row.by_order : row.by_receipt;
}

async function loadBoard(): Promise<void> {
  const period = periodCode.value.trim();
  if (!period) {
    boardError.value = '期间码必填（yyyy-MM 或 yyyy-QN）';
    boardRows.value = [];
    return;
  }
  boardLoading.value = true;
  boardError.value = '';
  try {
    boardRows.value = await fetchAchievement({
      period_code: period,
      scope_type: boardScopeType.value,
      basis: basis.value,
    });
  } catch (error) {
    boardRows.value = [];
    boardError.value = `达成率加载失败：${messageOf(error)}`;
  } finally {
    boardLoading.value = false;
  }
}

// ==================== 目标维护 ====================
const targetFilter = reactive({
  page: 1,
  page_size: 20,
  period_code: '',
  scope_type: '' as ScopeType | '',
  status: '' as TargetStatus | '',
  keyword: '',
});
const targetRows = ref<TargetRow[]>([]);
const targetTotal = ref(0);
const targetLoading = ref(false);
const targetError = ref('');

const targetColumns = [
  { colKey: 'target_no', title: '目标编号', width: 170 },
  { colKey: 'scope', title: '范围', width: 200 },
  { colKey: 'period_code', title: '期间', width: 100 },
  { colKey: 'target_amount', title: '目标金额', width: 150, align: 'right' as const },
  { colKey: 'target_qty', title: '目标数量', width: 120, align: 'right' as const },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'updated_at', title: '更新时间', width: 160 },
  { colKey: 'op', title: '操作', width: 190, fixed: 'right' as const },
];

const targetEmptyText = computed(() => {
  if (targetLoading.value) {
    return '加载中…';
  }
  return '还没有销售目标 —— 点右上角「新建目标」';
});

async function loadTargets(): Promise<void> {
  targetLoading.value = true;
  targetError.value = '';
  try {
    const page = await fetchTargetPage({
      page: targetFilter.page,
      page_size: targetFilter.page_size,
      period_code: targetFilter.period_code.trim() || undefined,
      scope_type: targetFilter.scope_type || undefined,
      status: targetFilter.status || undefined,
      keyword: targetFilter.keyword.trim() || undefined,
    });
    targetRows.value = page.list ?? [];
    targetTotal.value = page.total ?? 0;
  } catch (error) {
    targetRows.value = [];
    targetTotal.value = 0;
    targetError.value = `目标加载失败：${messageOf(error)}`;
  } finally {
    targetLoading.value = false;
  }
}

function resetTargetFilter(): void {
  targetFilter.page = 1;
  targetFilter.period_code = '';
  targetFilter.scope_type = '';
  targetFilter.status = '';
  targetFilter.keyword = '';
  void loadTargets();
}

// ---------- 目标弹窗 ----------
interface TargetFormDraft {
  scope_type: ScopeType;
  scope_id: string;
  period_code: string;
  target_amount: number;
  target_qty: number;
  status: TargetStatus;
  remark: string;
}

const targetFormVisible = ref(false);
const targetSubmitting = ref(false);
const targetFormError = ref('');
const editingTargetId = ref('');
const targetForm = reactive<TargetFormDraft>(emptyTargetForm());

function emptyTargetForm(): TargetFormDraft {
  return {
    scope_type: 'SALESPERSON',
    scope_id: '',
    period_code: periodCode.value || currentPeriod(),
    target_amount: 0,
    target_qty: 0,
    status: 'ACTIVE',
    remark: '',
  };
}

function openTargetCreate(): void {
  editingTargetId.value = '';
  targetFormError.value = '';
  Object.assign(targetForm, emptyTargetForm());
  targetFormVisible.value = true;
}

function openTargetEdit(row: TargetRow): void {
  editingTargetId.value = row.id;
  targetFormError.value = '';
  Object.assign(targetForm, {
    scope_type: row.scope_type,
    scope_id: row.scope_id ?? '',
    period_code: row.period_code,
    target_amount: Number(row.target_amount),
    target_qty: Number(row.target_qty),
    status: row.status,
    remark: row.remark ?? '',
  });
  targetFormVisible.value = true;
}

/** 范围类型变化：`ALL` 必须清掉 scope_id，否则库层 CHECK 会 400 */
function onScopeTypeChange(): void {
  if (targetForm.scope_type === 'ALL') {
    targetForm.scope_id = '';
  }
}

function validateTargetForm(): string {
  if (targetForm.scope_type !== 'ALL' && !targetForm.scope_id.trim()) {
    return `范围类型「${scopeLabelOf(targetForm.scope_type)}」必须选一个范围对象`;
  }
  if (!/^\d{4}-(0[1-9]|1[0-2]|[Qq][1-4])$/.test(targetForm.period_code.trim())) {
    return '期间码只认 yyyy-MM 或 yyyy-QN（如 2026-11 / 2026-Q4）';
  }
  if (!(targetForm.target_amount > 0)) {
    return '目标金额必须大于 0';
  }
  return '';
}

async function submitTargetForm(): Promise<void> {
  const invalid = validateTargetForm();
  if (invalid) {
    targetFormError.value = invalid;
    return;
  }
  targetSubmitting.value = true;
  targetFormError.value = '';
  const scopeId = targetForm.scope_type === 'ALL' ? null : targetForm.scope_id.trim();
  const payload = {
    scope_type: targetForm.scope_type,
    scope_id: scopeId,
    period_code: targetForm.period_code.trim().toUpperCase(),
    target_amount: targetForm.target_amount.toFixed(2),
    target_qty: targetForm.target_qty.toFixed(4),
    status: targetForm.status,
    remark: targetForm.remark.trim() || null,
  };
  try {
    if (editingTargetId.value) {
      const updated = await updateTarget(editingTargetId.value, payload);
      MessagePlugin.success(`已保存 ${updated.target_no}`);
    } else {
      const created = await createTarget(payload);
      MessagePlugin.success(`已创建 ${created.target_no}`);
    }
    targetFormVisible.value = false;
    await Promise.all([loadTargets(), loadBoard()]);
  } catch (error) {
    // 唯一索引冲突（同范围同期间已有目标）后端 409 原文摆进弹窗
    targetFormError.value = messageOf(error);
  } finally {
    targetSubmitting.value = false;
  }
}

async function toggleTarget(row: TargetRow): Promise<void> {
  const next: TargetStatus = row.status === 'ACTIVE' ? 'INACTIVE' : 'ACTIVE';
  try {
    await updateTarget(row.id, { status: next });
    MessagePlugin.success(`${row.target_no} ${next === 'ACTIVE' ? '已启用' : '已停用'}`);
    await Promise.all([loadTargets(), loadBoard()]);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

function confirmDeleteTarget(row: TargetRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除销售目标',
    body: `软删目标「${row.target_no}」（${scopeLabelOf(row.scope_type)} / ${row.period_code}）？\n\n只是不再使用的话建议「停用」：删除后看板不再统计它，历史无从追溯。`,
    theme: 'warning',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteTarget(row.id);
        MessagePlugin.success('已删除');
        await Promise.all([loadTargets(), loadBoard()]);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      } finally {
        dialog.hide();
      }
    },
  });
}

// ==================== 预测维护 ====================
const forecastFilter = reactive({
  page: 1,
  page_size: 20,
  period_code: '',
  confidence: '' as Confidence | '',
});
const forecastRows = ref<ForecastRow[]>([]);
const forecastTotal = ref(0);
const forecastLoading = ref(false);
const forecastError = ref('');

const forecastColumns = [
  { colKey: 'forecast_no', title: '预测编号', width: 170 },
  { colKey: 'dimension', title: '维度', minWidth: 220 },
  { colKey: 'period_code', title: '期间', width: 100 },
  { colKey: 'forecast_amount', title: '预测金额', width: 150, align: 'right' as const },
  { colKey: 'confidence', title: '信心度', width: 90 },
  { colKey: 'version_no', title: '版本', width: 80 },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' as const },
];

const forecastEmptyText = computed(() => {
  if (forecastLoading.value) {
    return '加载中…';
  }
  return '还没有销售预测 —— 点右上角「新建预测」';
});

function forecastDimension(row: ForecastRow): string {
  const parts: string[] = [];
  if (row.customer_name) {
    parts.push(`客户：${row.customer_name}`);
  }
  if (row.material_name) {
    parts.push(`物料：${row.material_name}`);
  }
  return parts.length > 0 ? parts.join(' / ') : '全局预测';
}

async function loadForecasts(): Promise<void> {
  forecastLoading.value = true;
  forecastError.value = '';
  try {
    const page = await fetchForecastPage({
      page: forecastFilter.page,
      page_size: forecastFilter.page_size,
      period_code: forecastFilter.period_code.trim() || undefined,
      confidence: forecastFilter.confidence || undefined,
    });
    forecastRows.value = page.list ?? [];
    forecastTotal.value = page.total ?? 0;
  } catch (error) {
    forecastRows.value = [];
    forecastTotal.value = 0;
    forecastError.value = `预测加载失败：${messageOf(error)}`;
  } finally {
    forecastLoading.value = false;
  }
}

interface ForecastFormDraft {
  customer_id: string;
  material_id: string;
  period_code: string;
  forecast_amount: number;
  confidence: Confidence;
  version_no: number;
  remark: string;
}

const forecastFormVisible = ref(false);
const forecastSubmitting = ref(false);
const forecastFormError = ref('');
const editingForecastId = ref('');
const forecastForm = reactive<ForecastFormDraft>(emptyForecastForm());

function emptyForecastForm(): ForecastFormDraft {
  return {
    customer_id: '',
    material_id: '',
    period_code: periodCode.value || currentPeriod(),
    forecast_amount: 0,
    confidence: 'MEDIUM',
    version_no: 1,
    remark: '',
  };
}

function openForecastCreate(): void {
  editingForecastId.value = '';
  forecastFormError.value = '';
  Object.assign(forecastForm, emptyForecastForm());
  forecastFormVisible.value = true;
}

function openForecastEdit(row: ForecastRow): void {
  editingForecastId.value = row.id;
  forecastFormError.value = '';
  Object.assign(forecastForm, {
    customer_id: row.customer_id ?? '',
    material_id: row.material_id ?? '',
    period_code: row.period_code,
    forecast_amount: Number(row.forecast_amount),
    confidence: row.confidence,
    version_no: row.version_no,
    remark: row.remark ?? '',
  });
  forecastFormVisible.value = true;
}

async function submitForecastForm(): Promise<void> {
  if (!/^\d{4}-(0[1-9]|1[0-2]|[Qq][1-4])$/.test(forecastForm.period_code.trim())) {
    forecastFormError.value = '期间码只认 yyyy-MM 或 yyyy-QN（如 2026-11 / 2026-Q4）';
    return;
  }
  if (!(forecastForm.forecast_amount >= 0)) {
    forecastFormError.value = '预测金额不能为负';
    return;
  }
  forecastSubmitting.value = true;
  forecastFormError.value = '';
  const payload = {
    customer_id: forecastForm.customer_id || null,
    material_id: forecastForm.material_id || null,
    period_code: forecastForm.period_code.trim().toUpperCase(),
    forecast_amount: forecastForm.forecast_amount.toFixed(2),
    confidence: forecastForm.confidence,
    version_no: forecastForm.version_no,
    remark: forecastForm.remark.trim() || null,
  };
  try {
    if (editingForecastId.value) {
      const updated = await updateForecast(editingForecastId.value, payload);
      MessagePlugin.success(`已保存 ${updated.forecast_no}`);
    } else {
      const created = await createForecast(payload);
      MessagePlugin.success(`已创建 ${created.forecast_no}`);
    }
    forecastFormVisible.value = false;
    await loadForecasts();
  } catch (error) {
    forecastFormError.value = messageOf(error);
  } finally {
    forecastSubmitting.value = false;
  }
}

function confirmDeleteForecast(row: ForecastRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除销售预测',
    body: `软删预测「${row.forecast_no}」？删除后该版本不再出现在列表里。`,
    theme: 'warning',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteForecast(row.id);
        MessagePlugin.success('已删除');
        await loadForecasts();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      } finally {
        dialog.hide();
      }
    },
  });
}

// ==================== 候选下拉（销售员 / 部门 / 物料 / 客户） ====================
const users = ref<UserRes[]>([]);
const materials = ref<MaterialRow[]>([]);
const depts = ref<DeptNode[]>([]);
const customers = ref<{ id: string; name: string }[]>([]);
const scopeOptionsFailed = ref(false);

const userOptions = computed(() =>
  users.value.map((item) => ({
    label: `${item.real_name || item.username}（${item.username}）`,
    value: item.id,
  })),
);

function flattenDepts(nodes: DeptNode[], out: { label: string; value: string }[] = []) {
  for (const node of nodes) {
    out.push({ label: node.dept_name, value: node.id });
    if (node.children?.length) {
      flattenDepts(node.children, out);
    }
  }
  return out;
}

const deptOptions = computed(() => flattenDepts(depts.value));

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}`,
    value: item.id,
  })),
);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: item.name, value: item.id })),
);

const scopeObjectOptions = computed(() => {
  if (targetForm.scope_type === 'SALESPERSON') {
    return userOptions.value;
  }
  if (targetForm.scope_type === 'DEPT') {
    return deptOptions.value;
  }
  return materialOptions.value;
});

/** 拉候选：四处互不依赖，单边失败只置降级标记，不影响主流程 */
async function loadOptions(): Promise<void> {
  const [userResult, deptResult, materialResult] = await Promise.allSettled([
    pageUsers({ page: 1, page_size: 200, status: 1 }),
    getDeptTree({}),
    fetchMaterialPage({ page: 1, page_size: 200, status: 1 }),
  ]);
  if (userResult.status === 'fulfilled') {
    users.value = userResult.value.list ?? [];
  } else {
    users.value = [];
  }
  if (deptResult.status === 'fulfilled') {
    depts.value = deptResult.value ?? [];
  } else {
    depts.value = [];
  }
  if (materialResult.status === 'fulfilled') {
    materials.value = materialResult.value.list ?? [];
  } else {
    materials.value = [];
  }
  // 客户候选走同一套 ERP 接口（失败就留空，预测弹窗允许用手输 / 留空）
  try {
    const customerPage = await fetchTargetCustomerOptions();
    customers.value = customerPage;
  } catch {
    customers.value = [];
  }
  scopeOptionsFailed.value =
    userOptions.value.length === 0 && deptOptions.value.length === 0 && materialOptions.value.length === 0;
}

/**
 * 客户候选：复用客户档案分页（`GET /api/business/erp/customer/page`）。
 * 拿不到就返回空数组（预测的客户可以空 = 全局预测），不阻断页面。
 */
async function fetchTargetCustomerOptions(): Promise<{ id: string; name: string }[]> {
  const page = await fetchCustomerPage({ page: 1, page_size: 200 });
  return (page.list ?? []).map((item) => ({ id: item.id, name: `${item.code} ${item.name}` }));
}

// ==================== 生命周期 ====================
// 切口径 / 换范围必须重新取数：服务端把「选中口径」放进 `actual_amount`，
// 只改本地状态的话会出现「标签写按已收款、数字还是按订单」的假看板（真机走查踩过）。
watch([basis, boardScopeType], () => {
  void loadBoard();
});

onMounted(() => {
  void loadBoard();
  void loadTargets();
  void loadForecasts();
  void loadOptions();
});
</script>

<style scoped>
.sales-target-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.page-card {
  padding: 16px;
}

.page-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 12px;
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

.filter-row__period {
  width: 200px;
}

.filter-row__scope {
  width: 150px;
}

.filter-row__status {
  width: 130px;
}

.filter-row__keyword {
  width: 200px;
}

.filter-row__spacer {
  flex: 1;
}

.page-alert {
  margin-bottom: 12px;
}

.summary-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-bottom: 12px;
}

.summary-card {
  flex: 1 1 180px;
}

.summary-card__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  margin-bottom: 6px;
}

.summary-card__value {
  font-size: 18px;
  font-weight: 600;
}

.scope-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.scope-cell__name {
  font-weight: 500;
}

.rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rate-cell__text {
  min-width: 64px;
}

.gap-short {
  color: var(--td-error-color);
}

.gap-over {
  color: var(--td-success-color);
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.pager-row {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}

.form-body {
  padding-top: 4px;
}

.form-hint {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}
</style>
