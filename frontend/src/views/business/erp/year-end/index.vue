<template>
  <div class="year-end-page">
    <!-- ==================== ① 年度 + 状态 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <span class="tool-label">会计年度</span>
        <t-select
          v-model="yearModel"
          class="tool-row__year"
          :options="yearOptions"
          :loading="yearLoading"
          placeholder="选择年度"
          data-testid="yearend-year-select"
          @change="loadCheck()"
        />
        <t-tag :theme="statusTheme(check?.state)" variant="light" data-testid="yearend-status-tag">
          {{ statusLabel(check?.state) }}
        </t-tag>
        <t-tag v-if="check" theme="default" variant="outline" data-testid="yearend-periods-closed">
          本年已结账 {{ check.state.periods_closed }}/12 月
        </t-tag>
        <t-tag
          v-if="check"
          :theme="check.state.next_year_periods_ready ? 'success' : 'warning'"
          variant="outline"
          data-testid="yearend-next-year"
        >
          {{ check.year + 1 }} 年已开账 {{ check.state.next_year_open_count }}/12 月
        </t-tag>

        <span class="tool-row__spacer" />

        <t-button
          variant="outline"
          :loading="loading"
          data-testid="btn-yearend-refresh"
          @click="loadCheck()"
        >
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
      </div>
    </t-card>

    <!-- ==================== ② 读失败 ==================== -->
    <t-alert
      v-if="loadError"
      theme="error"
      :message="loadError"
      class="page-alert"
      data-testid="yearend-load-error"
    >
      <template #operation>
        <t-button size="small" variant="outline" @click="loadCheck()">重试</t-button>
      </template>
    </t-alert>

    <!-- ==================== ③ 前置检查 ==================== -->
    <t-card :bordered="false" class="main-card" title="前置检查">
      <template #actions>
        <span class="card-hint">五项全过才能年结（任一不过后端 409 并逐条点名）</span>
      </template>

      <div v-if="loading" class="check-loading" data-testid="yearend-check-loading">正在检查…</div>

      <ul v-else class="check-list" data-testid="yearend-check-list">
        <li
          v-for="line in lines"
          :key="line.type"
          class="check-item"
          :class="line.ok ? 'is-ok' : 'is-bad'"
          :data-testid="`yearend-check-${line.type}`"
        >
          <span class="check-icon">{{ line.ok ? '✅' : '❌' }}</span>
          <span class="check-label">{{ line.label }}</span>
          <span class="check-detail">{{ line.ok ? '' : line.detail }}</span>
          <router-link v-if="!line.ok && line.link" :to="line.link" class="check-link">
            去处理 →
          </router-link>
        </li>
      </ul>

      <div v-if="extras.length > 0" class="extra-blockers" data-testid="yearend-extra-blockers">
        <t-alert
          v-for="item in extras"
          :key="item.type"
          theme="warning"
          :message="item.message"
          class="page-alert"
        />
      </div>
    </t-card>

    <!-- ==================== ④ 结转结果 / 操作 ==================== -->
    <t-card :bordered="false" class="main-card" title="年度结转">
      <div class="summary-grid">
        <div class="summary-item">
          <span class="summary-label">本年净利润</span>
          <span class="summary-value mono" data-testid="yearend-net-profit">
            {{ moneyText(check?.state.net_profit) }}
          </span>
        </div>
        <div class="summary-item">
          <span class="summary-label">结转凭证</span>
          <span class="summary-value mono" data-testid="yearend-voucher">
            {{ voucherText(check?.state) }}
          </span>
        </div>
        <div class="summary-item">
          <span class="summary-label">下年期初</span>
          <span class="summary-value" data-testid="yearend-opening">
            {{ openingText(check?.state) }}
          </span>
        </div>
      </div>

      <div class="action-row">
        <t-button
          v-if="showCloseButton"
          theme="primary"
          :disabled="!closeEnabled"
          :title="closeBlockedReasonText ?? ''"
          data-testid="btn-yearend-close"
          @click="openCloseDialog"
        >
          年度结转
        </t-button>
        <span v-if="showCloseButton && closeBlockedReasonText" class="blocked-hint" data-testid="yearend-close-blocked">
          {{ closeBlockedReasonText }}
        </span>

        <t-button
          v-if="showReopenButton"
          theme="danger"
          variant="outline"
          data-testid="btn-yearend-reopen"
          @click="openReopenDialog"
        >
          反年结
        </t-button>
        <span v-if="showReopenButton" class="blocked-hint">
          反年结会删掉结转凭证、清空 {{ (check?.year ?? 0) + 1 }}-01 期初并解封 12 月
        </span>
      </div>
    </t-card>

    <!-- ==================== ⑤ 年结确认弹窗（高危二次确认） ==================== -->
    <t-dialog
      v-model:visible="closeDialog.visible"
      :header="`年度结转 ${check?.year ?? ''}`"
      width="640px"
      :confirm-btn="{ content: '确认结转', theme: 'primary', loading: closeDialog.saving }"
      :cancel-btn="{ content: '取消' }"
      data-testid="yearend-close-dialog"
      @confirm="submitClose"
    >
      <div class="dialog-body">
        <t-alert theme="warning" message="年结会生成损益结转凭证并写入下年期初，动作不可撤销（只能反年结）" />
        <ul class="dialog-list">
          <li>生成一张 <span class="mono">YEAR_END</span> 凭证：把 5xxx / 6xxx 结平，本年利润转入 3104</li>
          <li>写入 <span class="mono">{{ (check?.year ?? 0) + 1 }}-01</span> 期初余额（损益类归零）</li>
          <li>{{ check?.year ?? '' }} 年 12 月保持「已结账」</li>
        </ul>
        <div class="dialog-field">
          <span class="dialog-label">备注（可选）</span>
          <t-input v-model="closeDialog.reason" placeholder="如：2026 年度结账" data-testid="yearend-close-reason" />
        </div>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 反年结弹窗（必填原因 ≥ 10 字） ==================== -->
    <t-dialog
      v-model:visible="reopenDialog.visible"
      :header="`反年结 ${check?.year ?? ''}`"
      width="640px"
      :confirm-btn="{ content: '确认反年结', theme: 'danger', loading: reopenDialog.saving }"
      :cancel-btn="{ content: '取消' }"
      :confirm-tip="reopenDialog.error || undefined"
      data-testid="yearend-reopen-dialog"
      @confirm="submitReopen"
    >
      <div class="dialog-body">
        <t-alert
          theme="error"
          message="反年结会删除结转凭证、清空下年期初、解封 12 月 —— 下一年度已经有凭证时会被拒绝"
        />
        <div class="dialog-field">
          <span class="dialog-label">原因（必填，≥ 10 字）</span>
          <t-input
            v-model="reopenDialog.reason"
            placeholder="如：成本结转口径有误，需重做 2026 年度结转"
            data-testid="yearend-reopen-reason"
          />
        </div>
        <div v-if="reopenDialog.error" class="dialog-error" data-testid="yearend-reopen-error">
          {{ reopenDialog.error }}
        </div>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 年度结转（ERP-R4）—— 财务管理 / 年度结转。
 *
 * # 页面要回答的三件事
 *
 * 1. **能不能结转**：五项前置校验逐条 ✅ / ❌（点名的月份 / 张数 / 差额直接展示），
 *    全过才允许点「年度结转」；不过时按钮**置灰 + 说明原因**（不是隐藏）。
 * 2. **结转完了吗**：状态 tag「2026 年度已结转」+ 结转凭证号 + 下年期初行数。
 * 3. **怎么回滚**：反年结必须填原因（≥ 10 字），且下一年度不能有凭证。
 *
 * # 权限（与 V0190 的三个节点逐字对应）
 *
 * | 动作 | 权限码 | 无权限时 |
 * |------|--------|----------|
 * | 看状态 / 前置检查 | `business:erp:yearend:read` | 整页 403（路由 meta 也拦一次） |
 * | 年结 | `business:erp:yearend:close` | **不渲染**「年度结转」 |
 * | 反年结 | `business:erp:yearend:reopen` | **不渲染**「反年结」 |
 *
 * 用 `v-if` 而不是 `:disabled`：置灰会在没权限的人页面上永久留一个点不动的按钮；
 * 而「有权限但前置没过」用 `:disabled` + 文案，两者语义不同（只读 vs 无权限）。
 *
 * # 为什么下年期初只显示、不生成期初凭证
 *
 * 年结把期初写进 `biz_opening_balance(YYYY+1-01, source='YEAR_END')`，**不**生成
 * 2027-01 的 OPENING 凭证（报表的期初 = 历史累计，再生成一张就双计）。
 * 页面照实说「期初已生成 N 行」，别让人以为漏了凭证。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  closeYearEnd,
  fetchPeriodPage,
  fetchYearEndCheck,
  reopenYearEnd,
  type YearEndCheck,
} from '@/api/business/erp';
import { useUserStore } from '@/stores/user';
import {
  YEAR_END_PERMISSIONS,
  canClose,
  canReopen,
  checkLines,
  closeBlockedReason,
  closeResultText,
  extraBlockers,
  moneyText,
  openingText,
  reasonError,
  statusLabel,
  statusTheme,
  voucherText,
} from './year-end-logic';

const userStore = useUserStore();

// ---------- 状态 ----------
const loading = ref(false);
const yearLoading = ref(false);
const loadError = ref('');
const yearModel = ref<number | undefined>(undefined);
const yearOptions = ref<Array<{ value: number; label: string }>>([]);
const check = ref<YearEndCheck | null>(null);

const closeDialog = reactive({ visible: false, saving: false, reason: '' });
const reopenDialog = reactive({ visible: false, saving: false, reason: '', error: '' });

// ---------- 权限 ----------
const hasPermission = (code: string): boolean => userStore.hasPermission(code);

// ---------- 派生 ----------
const lines = computed(() => checkLines(check.value));
const extras = computed(() => extraBlockers(check.value));
const closeEnabled = computed(() => canClose(check.value, hasPermission));
const closeBlockedReasonText = computed(() => closeBlockedReason(check.value, hasPermission));
const showCloseButton = computed(() => hasPermission(YEAR_END_PERMISSIONS.close));
const showReopenButton = computed(
  () => hasPermission(YEAR_END_PERMISSIONS.reopen) && canReopen(check.value, hasPermission),
);

// ---------- 读 ----------
async function loadYearOptions(): Promise<void> {
  yearLoading.value = true;
  try {
    const page = await fetchPeriodPage({ page: 1, page_size: 200 });
    const years = Array.from(new Set((page.list ?? []).map((row) => row.period_year))).sort(
      (a, b) => b - a,
    );
    yearOptions.value = years.map((year) => ({ value: year, label: `${year} 年` }));
    if (yearModel.value === undefined && years.length > 0) {
      yearModel.value = years[0];
    }
  } catch (error) {
    loadError.value = error instanceof ApiError ? error.message : '读取会计期间失败';
  } finally {
    yearLoading.value = false;
  }
}

async function loadCheck(): Promise<void> {
  if (yearModel.value === undefined) {
    return;
  }
  loading.value = true;
  loadError.value = '';
  try {
    check.value = await fetchYearEndCheck(yearModel.value);
  } catch (error) {
    check.value = null;
    loadError.value = error instanceof ApiError ? error.message : '读取年度结转状态失败';
  } finally {
    loading.value = false;
  }
}

// ---------- 年结 ----------
function openCloseDialog(): void {
  if (!closeEnabled.value) {
    return;
  }
  closeDialog.reason = '';
  closeDialog.visible = true;
}

async function submitClose(): Promise<void> {
  if (yearModel.value === undefined) {
    return;
  }
  closeDialog.saving = true;
  try {
    const result = await closeYearEnd(yearModel.value, closeDialog.reason.trim() || undefined);
    closeDialog.visible = false;
    await MessagePlugin.success(closeResultText(result));
    await loadCheck();
  } catch (error) {
    await MessagePlugin.error(error instanceof ApiError ? error.message : '年结失败');
  } finally {
    closeDialog.saving = false;
  }
}

// ---------- 反年结 ----------
function openReopenDialog(): void {
  reopenDialog.reason = '';
  reopenDialog.error = '';
  reopenDialog.visible = true;
}

async function submitReopen(): Promise<void> {
  if (yearModel.value === undefined) {
    return;
  }
  const problem = reasonError(reopenDialog.reason);
  if (problem) {
    reopenDialog.error = problem;
    return;
  }
  reopenDialog.saving = true;
  reopenDialog.error = '';
  try {
    const result = await reopenYearEnd(yearModel.value, reopenDialog.reason.trim());
    reopenDialog.visible = false;
    await MessagePlugin.success(
      `${result.year} 年度已反结转：删除凭证 ${result.deleted_voucher_no ?? '—'}、清空期初 ${result.deleted_opening_rows} 行${
        result.december_reopened ? '、12 月已解封' : ''
      }`,
    );
    await loadCheck();
  } catch (error) {
    reopenDialog.error = error instanceof ApiError ? error.message : '反年结失败';
  } finally {
    reopenDialog.saving = false;
  }
}

onMounted(async () => {
  await loadYearOptions();
  await loadCheck();
});
</script>

<style scoped>
.year-end-page {
  padding: 16px;
}
.tool-card,
.main-card {
  margin-bottom: 12px;
}
.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.tool-row__year {
  width: 160px;
}
.tool-row__spacer {
  flex: 1;
}
.tool-label {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}
.page-alert {
  margin-bottom: 12px;
}
.card-hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}
.check-loading {
  color: var(--td-text-color-placeholder);
  padding: 12px 0;
}
.check-list {
  list-style: none;
  margin: 0;
  padding: 0;
}
.check-item {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 8px 0;
  border-bottom: 1px dashed var(--td-component-stroke);
  font-size: 14px;
}
.check-item:last-child {
  border-bottom: none;
}
.check-item.is-bad .check-label {
  color: var(--td-error-color);
}
.check-icon {
  width: 18px;
}
.check-label {
  min-width: 160px;
  font-weight: 500;
}
.check-detail {
  flex: 1;
  color: var(--td-text-color-secondary);
  font-size: 13px;
  line-height: 1.6;
}
.check-link {
  color: var(--td-brand-color);
  font-size: 13px;
  white-space: nowrap;
}
.summary-grid {
  display: flex;
  gap: 32px;
  flex-wrap: wrap;
  padding-bottom: 12px;
}
.summary-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 220px;
}
.summary-label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}
.summary-value {
  font-size: 14px;
}
.action-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding-top: 8px;
  border-top: 1px solid var(--td-component-stroke);
}
.blocked-hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}
.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.dialog-list {
  margin: 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
  line-height: 1.8;
}
.dialog-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dialog-label {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}
.dialog-error {
  color: var(--td-error-color);
  font-size: 13px;
}
.mono {
  font-family: var(--td-font-family-mono, monospace);
}
</style>
