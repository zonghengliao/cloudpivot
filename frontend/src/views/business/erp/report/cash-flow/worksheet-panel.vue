<!--
  现金流底稿录入面板（M1-18 · 后端 `cashflow_worksheet.rs` · 迁移 V0414 / V0415）。

  这是「现金流量表」页 `现金流底稿` tab 的主体：**逐行编制底稿**（期初余额 + 三大活动的
  各项目发生额 + 期末余额），并当场看到后端算好的小计与勾稽结论。

  # 四个不显眼但很关键的交互决定

  1. **保存允许暂时不平**：底稿是工作稿，会计经常先录流入、期末余额最后才对得上。
     页面**不**拿 `check.balanced` 拦保存（那会逼人先编个假数把单存进去）；只有「出表」
     才要求闭合（后端 `…/statement/export` 未闭合 400 + 66087）。
  2. **只发改动过的行**：后端 `items` 是非空即 upsert，把 12 个从没编过的配置项全发一遍
     会把它们写成「已录 0」，`entered` 位被污染 ⇒「本期没编这一行」这个信息就没了。
     提交体由 `buildSavePayload`（`worksheet-logic.ts`，有单测）挑 diff。
  3. **试算不写库**：`试算校验` 打 `POST …/worksheet/check`（一个字节都不落库），把草稿
     叠加到已存底稿上算「期初 + 流入 − 流出 = 期末」；结果标「草稿试算」，与「已存底稿」
     的勾稽结论分开显示，不让人把草稿数当已存数。
  4. **金额是十进制字符串**：输入框原文直接交后端（`Decimal` 解析），页面**不做金额计算**；
     小计 / 净额 / 差额全部来自后端。千分位格式化走 `formatAmount`（不经过 `Number()`）。

  # 期间闸

  只有 `OPEN` 的期间可写。`CLOSED`（账已定稿）/ `NOT_OPENED`（窗口没开）时输入框置灰、
  保存按钮禁用，但底稿照常显示 —— 历史底稿、已结账期间都要能看（后端 GET 不受限）。
-->
<template>
  <div class="cfw-panel">
    <div class="tool-row">
      <span class="tool-row__label">会计期间</span>
      <t-select
        v-model="period"
        class="tool-row__period"
        :options="periods"
        :loading="periodLoading"
        placeholder="选择会计期间"
        data-testid="cfw-period"
        @change="load()"
      />
      <t-button theme="primary" :loading="loading" data-testid="cfw-reload" @click="load()">查询</t-button>
      <t-button
        variant="outline"
        :loading="checking"
        :disabled="!sheet"
        data-testid="cfw-check"
        @click="runCheck()"
      >
        <template #icon><t-icon name="check-circle" /></template>
        试算校验
      </t-button>
      <t-button
        v-privilege="[PERM_WORKSHEET_SAVE]"
        theme="primary"
        :loading="saving"
        :disabled="!canEdit || !dirty"
        data-testid="cfw-save"
        @click="save()"
      >
        <template #icon><t-icon name="save" /></template>
        保存底稿
      </t-button>
      <t-button
        variant="outline"
        :disabled="!sheet"
        data-testid="cfw-reset"
        @click="reset()"
      >
        还原
      </t-button>
      <t-button variant="outline" :disabled="!sheet" data-testid="cfw-preview" @click="preview()">
        <template #icon><t-icon name="browse" /></template>
        报表预览
      </t-button>
      <span v-if="dirty" class="cfw-dirty" data-testid="cfw-dirty">有未保存的修改</span>
    </div>

    <t-alert v-if="error" theme="error" :message="error" class="cfw-alert" data-testid="cfw-error">
      <template #operation>
        <t-button size="small" variant="outline" @click="load()">重试</t-button>
      </template>
    </t-alert>

    <t-alert
      v-if="sheet && !sheet.editable"
      theme="warning"
      class="cfw-alert"
      data-testid="cfw-locked"
      :message="`期间 ${sheet.period} 当前为「${periodStatusText(sheet.period_status)}」，底稿只读：要改请先到「会计期间」页开账 / 撤销结账`"
    />

    <t-alert
      v-if="!canSave && sheet"
      theme="info"
      class="cfw-alert"
      data-testid="cfw-no-save-perm"
      message="当前账号没有「底稿录入」权限（business:erp:cashflow-worksheet:save），只能查看"
    />

    <t-loading :loading="loading" size="small">
      <div v-if="sheet" class="cfw-sheet">
        <div class="cfw-head">
          <h2 class="cfw-head__title">现金流底稿</h2>
          <div class="cfw-head__meta">
            <span data-testid="cfw-period-name">期间：{{ sheet.period }}（{{ periodStatusText(sheet.period_status) }}）</span>
            <span data-testid="cfw-entered">
              已录 {{ enteredRows }} / {{ sheet.rows.length }} 个明细项
            </span>
            <span data-testid="cfw-editable">{{ sheet.editable ? '可编辑' : '只读' }}</span>
          </div>
          <div class="cfw-head__note">
            直接法勾稽式：<b>期初余额 + 流入合计 − 流出合计 = 期末余额</b>（精确到 4 位小数，差一分钱都不算闭合）。
            保存允许暂时不平，<b>出表</b>前必须闭合。
          </div>
        </div>

        <div class="cfw-table-wrap">
          <table class="cfw-table" data-testid="cfw-table">
            <thead>
              <tr>
                <th class="cfw-col-item">项目</th>
                <th class="cfw-col-dir">方向</th>
                <th class="cfw-col-amount">本期发生额</th>
                <th class="cfw-col-remark">编制说明</th>
              </tr>
            </thead>
            <tbody>
              <!-- ============ 期初余额（行在明细之前，与纸面报表同序） ============ -->
              <tr class="cfw-row--balance" data-testid="cfw-opening-row">
                <td class="cfw-col-item" data-testid="cfw-opening-name">
                  {{ sheet.opening?.item_name ?? OPENING_NAME_FALLBACK }}
                </td>
                <td class="cfw-col-dir">余额</td>
                <td class="cfw-col-amount">
                  <t-input
                    v-model="draft.opening"
                    size="small"
                    :disabled="!canEdit"
                    :status="openingStatus"
                    :tip="openingTip"
                    placeholder="未录入（可负：透支）"
                    data-testid="cfw-opening-amount"
                    @blur="refreshBlur('opening')"
                  />
                </td>
                <td class="cfw-col-remark">
                  <t-input
                    v-model="draft.openingRemark"
                    size="small"
                    :disabled="!canEdit"
                    placeholder="可选"
                    data-testid="cfw-opening-remark"
                  />
                </td>
              </tr>

              <!-- ============ 三大活动：分组标题 + 流入明细 + 流入小计 + 流出明细 + 流出小计 + 净额 ============ -->
              <template v-for="section in sections" :key="section.activity">
                <tr class="cfw-row--activity" :data-testid="`cfw-section-${section.activity}`">
                  <td colspan="4">{{ section.name }}</td>
                </tr>

                <tr
                  v-for="row in section.inflow"
                  :key="`in-${row.item_code}`"
                  class="cfw-row--line"
                  :data-testid="`cfw-row-${row.item_code}`"
                >
                  <td class="cfw-col-item">
                    <span class="cfw-item-name">{{ row.item_name }}</span>
                    <span class="cfw-item-code">{{ row.item_code }}</span>
                    <t-tag v-if="!row.item_active" size="small" variant="light" theme="default" class="cfw-tag">已停用</t-tag>
                    <t-tag
                      v-else-if="!row.entered"
                      size="small"
                      variant="light"
                      theme="warning"
                      class="cfw-tag"
                      :data-testid="`cfw-unentered-${row.item_code}`"
                    >
                      未录
                    </t-tag>
                  </td>
                  <td class="cfw-col-dir">流入</td>
                  <td class="cfw-col-amount">
                    <t-input
                      v-model="draft.rows[row.item_code].amount"
                      size="small"
                      :disabled="!canEdit"
                      :status="rowStatus(row)"
                      :tip="rowTip(row)"
                      :placeholder="row.entered ? '0' : '未录入'"
                      :data-testid="`cfw-amount-${row.item_code}`"
                      @blur="refreshBlur(row.item_code)"
                    />
                  </td>
                  <td class="cfw-col-remark">
                    <t-input
                      v-model="draft.rows[row.item_code].remark"
                      size="small"
                      :disabled="!canEdit"
                      placeholder="可选"
                      :data-testid="`cfw-remark-${row.item_code}`"
                    />
                  </td>
                </tr>
                <tr class="cfw-row--subtotal" :data-testid="`cfw-inflow-total-${section.activity}`">
                  <td class="cfw-col-item"><span class="cfw-indent">现金流入小计</span></td>
                  <td class="cfw-col-dir" />
                  <td class="cfw-col-amount">{{ formatAmount(section.block?.inflow) }}</td>
                  <td class="cfw-col-remark cfw-subtotal__meta">
                    {{ section.inflow.length }} 个流入项 · 已录 {{ enteredCount(section.inflow) }}
                  </td>
                </tr>

                <tr
                  v-for="row in section.outflow"
                  :key="`out-${row.item_code}`"
                  class="cfw-row--line"
                  :data-testid="`cfw-row-${row.item_code}`"
                >
                  <td class="cfw-col-item">
                    <span class="cfw-item-name">{{ row.item_name }}</span>
                    <span class="cfw-item-code">{{ row.item_code }}</span>
                    <t-tag v-if="!row.item_active" size="small" variant="light" theme="default" class="cfw-tag">已停用</t-tag>
                    <t-tag
                      v-else-if="!row.entered"
                      size="small"
                      variant="light"
                      theme="warning"
                      class="cfw-tag"
                      :data-testid="`cfw-unentered-${row.item_code}`"
                    >
                      未录
                    </t-tag>
                  </td>
                  <td class="cfw-col-dir">流出</td>
                  <td class="cfw-col-amount">
                    <t-input
                      v-model="draft.rows[row.item_code].amount"
                      size="small"
                      :disabled="!canEdit"
                      :status="rowStatus(row)"
                      :tip="rowTip(row)"
                      :placeholder="row.entered ? '0' : '未录入'"
                      :data-testid="`cfw-amount-${row.item_code}`"
                      @blur="refreshBlur(row.item_code)"
                    />
                  </td>
                  <td class="cfw-col-remark">
                    <t-input
                      v-model="draft.rows[row.item_code].remark"
                      size="small"
                      :disabled="!canEdit"
                      placeholder="可选"
                      :data-testid="`cfw-remark-${row.item_code}`"
                    />
                  </td>
                </tr>
                <tr class="cfw-row--subtotal" :data-testid="`cfw-outflow-total-${section.activity}`">
                  <td class="cfw-col-item"><span class="cfw-indent">现金流出小计</span></td>
                  <td class="cfw-col-dir" />
                  <td class="cfw-col-amount">{{ formatAmount(section.block?.outflow) }}</td>
                  <td class="cfw-col-remark cfw-subtotal__meta">
                    {{ section.outflow.length }} 个流出项 · 已录 {{ enteredCount(section.outflow) }}
                  </td>
                </tr>
                <tr class="cfw-row--net" :data-testid="`cfw-net-${section.activity}`">
                  <td class="cfw-col-item">
                    {{ ACTIVITY_LABELS[section.activity] ?? section.name }}产生的现金流量净额
                  </td>
                  <td class="cfw-col-dir" />
                  <td class="cfw-col-amount" :data-testid="`cfw-net-amount-${section.activity}`">
                    {{ formatAmount(section.block?.net) }}
                  </td>
                  <td class="cfw-col-remark cfw-subtotal__meta">
                    {{ section.block ? `${section.block.entered_rows} 行已录` : '—' }}
                  </td>
                </tr>
              </template>

              <!-- ============ 期末余额 ============ -->
              <tr class="cfw-row--balance" data-testid="cfw-closing-row">
                <td class="cfw-col-item" data-testid="cfw-closing-name">
                  {{ sheet.closing?.item_name ?? CLOSING_NAME_FALLBACK }}
                </td>
                <td class="cfw-col-dir">余额</td>
                <td class="cfw-col-amount">
                  <t-input
                    v-model="draft.closing"
                    size="small"
                    :disabled="!canEdit"
                    :status="closingStatus"
                    :tip="closingTip"
                    placeholder="未录入（= 期初 + 流入 − 流出）"
                    data-testid="cfw-closing-amount"
                    @blur="refreshBlur('closing')"
                  />
                </td>
                <td class="cfw-col-remark">
                  <t-input
                    v-model="draft.closingRemark"
                    size="small"
                    :disabled="!canEdit"
                    placeholder="可选"
                    data-testid="cfw-closing-remark"
                  />
                </td>
              </tr>

              <!-- ============ 合计区（直接法三行，后端算好的） ============ -->
              <tr class="cfw-row--total" data-testid="cfw-total-inflow">
                <td class="cfw-col-item">现金流入合计（三大活动）</td>
                <td class="cfw-col-dir" />
                <td class="cfw-col-amount">{{ formatAmount(sheet.totals.inflow) }}</td>
                <td class="cfw-col-remark cfw-subtotal__meta">已存底稿口径</td>
              </tr>
              <tr class="cfw-row--total" data-testid="cfw-total-outflow">
                <td class="cfw-col-item">现金流出合计（三大活动）</td>
                <td class="cfw-col-dir" />
                <td class="cfw-col-amount">{{ formatAmount(sheet.totals.outflow) }}</td>
                <td class="cfw-col-remark cfw-subtotal__meta">已存底稿口径</td>
              </tr>
              <tr class="cfw-row--total" data-testid="cfw-total-net">
                <td class="cfw-col-item">现金及现金等价物净增加额</td>
                <td class="cfw-col-dir" />
                <td class="cfw-col-amount">{{ formatAmount(sheet.totals.net) }}</td>
                <td class="cfw-col-remark cfw-subtotal__meta">
                  流入 − 流出{{ dirty ? '（小计不含未保存的修改，点「试算校验」看草稿结论）' : '' }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- ============ 勾稽结论 ============ -->
        <div
          :class="['cfw-check', `is-check--${tone}`]"
          data-testid="cfw-check-panel"
        >
          <div class="cfw-check__line">
            <span class="cfw-check__icon">{{ tone === 'ok' ? '✅' : '⚠️' }}</span>
            <span data-testid="cfw-check-message">{{ displayedCheck.message }}</span>
            <t-tag v-if="draftCheck" size="small" theme="primary" variant="light" class="cfw-check__tag" data-testid="cfw-check-draft">
              草稿试算（未落库）
            </t-tag>
            <t-tag v-else size="small" theme="default" variant="light" class="cfw-check__tag" data-testid="cfw-check-stored">
              已存底稿
            </t-tag>
          </div>
          <div class="cfw-check__numbers">
            <span>期初余额 <b data-testid="cfw-check-opening">{{ amountOrPlaceholder(displayedCheck.opening) }}</b></span>
            <span>流入合计 <b data-testid="cfw-check-inflow">{{ formatAmount(displayedCheck.inflow) }}</b></span>
            <span>流出合计 <b data-testid="cfw-check-outflow">{{ formatAmount(displayedCheck.outflow) }}</b></span>
            <span>应为期末 <b data-testid="cfw-check-expected">{{ amountOrPlaceholder(displayedCheck.expected_closing) }}</b></span>
            <span>实录期末 <b data-testid="cfw-check-closing">{{ amountOrPlaceholder(displayedCheck.closing) }}</b></span>
            <span>差额 <b data-testid="cfw-check-diff">{{ diffText(displayedCheck.diff) }}</b></span>
          </div>
          <ul v-if="displayedCheck.problems.length" class="cfw-check__problems" data-testid="cfw-problems">
            <li v-for="problem in displayedCheck.problems" :key="problem.code" :data-testid="`cfw-problem-${problem.code}`">
              <b>{{ problemLabel(problem.code) }}</b>：{{ problemText(problem) }}
            </li>
          </ul>
        </div>

        <div class="cfw-foot">
          保存只写改动过的行（期初 / 期末 / 明细各自独立）；勾稽不闭合也能保存，但出表会被拦。
          期间已结账时写入会被后端拒（66085）—— 要改历史请先撤销结账。
        </div>
      </div>

      <div v-else-if="!loading && !error" class="cfw-empty" data-testid="cfw-empty">
        选择会计期间后点「查询」编制现金流底稿
      </div>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
/**
 * 现金流底稿录入面板。
 *
 * 页面**不做任何金额计算**：小计 / 净额 / 应为期末 / 差额全部来自后端（GET 与 POST check）。
 * 前端只做三件事：① 金额格式校验（最多 4 位小数、明细行不许为负）② 挑出改动过的行
 * ③ 把后端的结论按「已存 / 草稿试算」分开显示。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import { fetchPeriodPage, type AccountPeriodRow } from '@/api/business/erp';
import {
  checkCashflowWorksheet,
  fetchCashflowWorksheet,
  saveCashflowWorksheet,
  type WorksheetCheck,
  type WorksheetRow,
  type WorksheetSheet,
} from '@/api/business/erp/cashflow-worksheet';
import { useUserStore } from '@/stores/user';
import {
  ACTIVITY_LABELS,
  CLOSING_NAME_FALLBACK,
  OPENING_NAME_FALLBACK,
  PERM_WORKSHEET_SAVE,
  buildCheckPayload,
  buildDraft,
  buildSavePayload,
  checkTone,
  diffText,
  formatAmount,
  amountOrPlaceholder,
  groupWorksheetRows,
  isDraftDirty,
  normalizeAmount,
  periodOptions,
  periodStatusText,
  problemLabel,
  problemText,
  type DraftState,
} from '@/views/business/erp/report/cash-flow/worksheet-logic';

const emit = defineEmits<{ (event: 'preview', period: string): void }>();

const userStore = useUserStore();

const period = ref('');
const periods = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const loading = ref(false);
const saving = ref(false);
const checking = ref(false);
const error = ref('');

const sheet = ref<WorksheetSheet | null>(null);
const draft = ref<DraftState>({ opening: '', openingRemark: '', closing: '', closingRemark: '', rows: {} });
/** 草稿试算结果（`POST …/check`，不落库）；`null` = 没试算过，显示已存底稿的结论 */
const draftCheck = ref<WorksheetCheck | null>(null);
/** 输入框校验错误（键 = `item_code` / `opening` / `closing`） */
const inputErrors = ref<Record<string, string>>({});

const canSave = computed(() => userStore.hasPermission(PERM_WORKSHEET_SAVE));
const canEdit = computed(() => Boolean(sheet.value?.editable) && canSave.value);
const sections = computed(() => (sheet.value ? groupWorksheetRows(sheet.value) : []));
const dirty = computed(() => (sheet.value ? isDraftDirty(draft.value, sheet.value) : false));
const enteredRows = computed(() => (sheet.value ? sheet.value.rows.filter((row) => row.entered).length : 0));
const displayedCheck = computed<WorksheetCheck>(() => draftCheck.value ?? sheet.value!.check);
const tone = computed(() => checkTone(displayedCheck.value));
const openingStatus = computed(() => (inputErrors.value.opening ? 'error' : 'default'));
const openingTip = computed(() => inputErrors.value.opening);
const closingStatus = computed(() => (inputErrors.value.closing ? 'error' : 'default'));
const closingTip = computed(() => inputErrors.value.closing);

function refreshBlur(key: string) {
  const allowNegative = key === 'opening' || key === 'closing';
  const text = key === 'opening' ? draft.value.opening : key === 'closing' ? draft.value.closing : draft.value.rows[key]?.amount;
  const parsed = normalizeAmount(text ?? '', allowNegative);
  if (parsed.kind === 'error') {
    inputErrors.value = { ...inputErrors.value, [key]: parsed.message };
  } else {
    const next = { ...inputErrors.value };
    delete next[key];
    inputErrors.value = next;
  }
}

function rowStatus(row: WorksheetRow): 'error' | 'default' {
  return inputErrors.value[row.item_code] ? 'error' : 'default';
}

function rowTip(row: WorksheetRow): string | undefined {
  return inputErrors.value[row.item_code];
}

function enteredCount(rows: WorksheetRow[]): number {
  return rows.filter((row) => row.entered).length;
}

async function loadPeriods() {
  periodLoading.value = true;
  try {
    const data = await fetchPeriodPage({ page: 1, page_size: 100 });
    periods.value = periodOptions(data.list as AccountPeriodRow[]);
    if (!period.value && periods.value.length) {
      period.value = periods.value[0].value;
    }
  } catch (err) {
    error.value = `期间列表加载失败：${(err as Error).message ?? err}`;
  } finally {
    periodLoading.value = false;
  }
}

async function load() {
  loading.value = true;
  error.value = '';
  draftCheck.value = null;
  inputErrors.value = {};
  try {
    const data = await fetchCashflowWorksheet(period.value || undefined);
    sheet.value = data;
    period.value = data.period;
    draft.value = buildDraft(data);
  } catch (err) {
    sheet.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

function reset() {
  if (!sheet.value) return;
  draft.value = buildDraft(sheet.value);
  draftCheck.value = null;
  inputErrors.value = {};
  MessagePlugin.info('已还原为已存底稿');
}

async function runCheck() {
  if (!sheet.value) return;
  const built = buildCheckPayload(sheet.value.period, draft.value, sheet.value);
  if ('error' in built) {
    MessagePlugin.error(built.error);
    return;
  }
  checking.value = true;
  try {
    draftCheck.value = await checkCashflowWorksheet(built.payload);
    const result = draftCheck.value;
    if (result.balanced) {
      MessagePlugin.success('草稿试算：勾稽闭合');
    } else {
      MessagePlugin.warning(`草稿试算：${result.message}`);
    }
  } catch (err) {
    MessagePlugin.error(`试算失败：${(err as Error).message ?? err}`);
  } finally {
    checking.value = false;
  }
}

async function save() {
  if (!sheet.value) return;
  const built = buildSavePayload(sheet.value.period, draft.value, sheet.value);
  if (built === null) {
    MessagePlugin.info('没有改动，无需保存');
    return;
  }
  if ('error' in built) {
    MessagePlugin.error(built.error);
    return;
  }
  saving.value = true;
  try {
    const next = await saveCashflowWorksheet(built.payload);
    sheet.value = next;
    draft.value = buildDraft(next);
    draftCheck.value = null;
    inputErrors.value = {};
    if (next.check.balanced) {
      MessagePlugin.success('底稿已保存：勾稽闭合，可以出表');
    } else {
      MessagePlugin.warning(`底稿已保存（工作稿暂未闭合）：${next.check.message}`);
    }
  } catch (err) {
    MessagePlugin.error(`保存失败：${(err as Error).message ?? err}`);
  } finally {
    saving.value = false;
  }
}

function preview() {
  if (!sheet.value) return;
  emit('preview', sheet.value.period);
}

onMounted(async () => {
  await loadPeriods();
  if (period.value) {
    await load();
  }
});

defineExpose({ load });
</script>

<style scoped>
.cfw-alert {
  margin-bottom: 12px;
}

.cfw-dirty {
  font-size: 12px;
  color: var(--td-warning-color-7, #b25e00);
}

.cfw-head {
  margin: 4px 0 10px;
}

.cfw-head__title {
  margin: 0 0 6px;
  font-size: 18px;
  font-weight: 600;
}

.cfw-head__meta {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cfw-head__note {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  line-height: 1.6;
}

.cfw-table-wrap {
  overflow-x: auto;
}

.cfw-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.cfw-table th,
.cfw-table td {
  border: 1px solid var(--td-component-border);
  padding: 4px 8px;
  line-height: 1.6;
  vertical-align: middle;
}

.cfw-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.cfw-col-item {
  width: 44%;
  text-align: left;
}

.cfw-col-dir {
  width: 8%;
  text-align: center;
  color: var(--td-text-color-secondary);
  white-space: nowrap;
}

.cfw-col-amount {
  width: 22%;
  text-align: right;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.cfw-col-remark {
  width: 26%;
}

.cfw-item-name {
  margin-right: 6px;
}

.cfw-item-code {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.cfw-tag {
  margin-left: 6px;
}

.cfw-indent {
  padding-left: 16px;
}

.cfw-subtotal__meta {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.cfw-row--activity td {
  font-weight: 600;
  background: var(--td-bg-color-secondarycontainer);
}

.cfw-row--balance td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

.cfw-row--subtotal td {
  font-weight: 600;
}

.cfw-row--net td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

.cfw-row--total td {
  font-weight: 600;
  background: var(--td-bg-color-container-active);
}

.cfw-check {
  margin-top: 14px;
  padding: 12px 14px;
  border-radius: 6px;
  font-size: 13px;
}

.is-check--ok {
  background: var(--td-success-color-1);
  border: 1px solid var(--td-success-color-2);
  color: var(--td-success-color-7, #2ba471);
}

.is-check--bad {
  background: var(--td-warning-color-1);
  border: 1px solid var(--td-warning-color-2);
  color: var(--td-warning-color-7, #b25e00);
}

.cfw-check__line {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 6px;
}

.cfw-check__icon {
  margin-right: 6px;
}

.cfw-check__tag {
  margin-left: 8px;
  vertical-align: middle;
}

.cfw-check__numbers {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-variant-numeric: tabular-nums;
}

.cfw-check__problems {
  margin: 8px 0 0;
  padding-left: 20px;
  line-height: 1.7;
}

.cfw-foot {
  margin-top: 10px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  line-height: 1.6;
}

.cfw-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}
</style>
