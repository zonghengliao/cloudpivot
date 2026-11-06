<template>
  <div class="commission-page">
    <!-- ==================== ① 筛选 + 列表 ==================== -->
    <t-card :bordered="false" class="panel-card">
      <div class="page-head">
        <div class="page-head__title">
          <span>佣金结算</span>
          <t-tag size="small" variant="light" theme="primary">按已收款计佣 · 期间 × 销售员</t-tag>
        </div>
        <span class="page-head__tip">
          佣金基数 = 应收单<b>已核销（真正收到钱）</b>的金额，不是开单金额；退货 / 退款会落<b>负数基数</b>（冲回）。
          状态单向走：草稿 → 审批通过（出计提凭证）→ 确认发放（出付款凭证），<b>不能回退</b>。
        </span>
      </div>

      <div class="filter-row">
        <t-select
          v-model="filter.period_code"
          class="filter-row__period"
          :options="periods"
          placeholder="全部期间"
          clearable
          data-testid="cmset-filter-period"
        />
        <t-select
          v-model="filter.salesperson_id"
          class="filter-row__select"
          :options="userOptions"
          placeholder="全部销售员"
          clearable
          filterable
          data-testid="cmset-filter-salesperson"
        />
        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="cmset-filter-status"
        />
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="结算单号 / 销售员"
          clearable
          data-testid="cmset-filter-keyword"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-button theme="primary" data-testid="btn-cmset-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-cmset-reset" @click="handleReset">重置</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="
            canCreate
              ? '按期间 + 销售员生成草稿结算单（不选销售员 = 给所有有可计佣差额的人各生成一张）'
              : '当前账号没有 business:erp:commission-settle:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-cmset-settle"
              :disabled="!canCreate"
              @click="openSettle"
            >
              <template #icon><t-icon name="add" /></template>
              生成结算单
            </t-button>
          </span>
        </t-tooltip>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="page-alert"
        data-testid="cmset-list-error"
      />

      <div v-if="rows.length" class="totals-strip" data-testid="cmset-totals">
        <span>本页 {{ totals.count }} 张</span>
        <span class="totals-strip__sep">·</span>
        <span>
          基数合计
          <b class="mono">{{ moneyText(totals.sales) }}</b>
        </span>
        <span class="totals-strip__sep">·</span>
        <span>
          应发合计
          <b class="mono" :class="{ 'money--bad': totals.commission < 0 }">
            {{ moneyText(totals.commission) }}
          </b>
        </span>
        <span class="totals-strip__hint">（只汇总当前页，不是全量）</span>
      </div>

      <t-table
        row-key="id"
        size="small"
        hover
        :data="rows"
        :columns="columns"
        :loading="loading"
        :empty="emptyText"
        :expand-icon="true"
        :expanded-row-keys="expandedKeys"
        data-testid="cmset-table"
        @expand-change="handleExpandChange"
      >
        <template #settlement_no="{ row }">
          <span class="mono">{{ row.settlement_no }}</span>
        </template>
        <template #salesperson_name="{ row }">
          <span>{{ row.salesperson_name || '（账号已删除）' }}</span>
        </template>
        <template #period_code="{ row }">
          <div class="period-cell">
            <span>{{ periodLabel(row.period_code) }}</span>
            <span class="period-cell__code mono">{{ row.period_code }}</span>
          </div>
        </template>
        <template #total_sales="{ row }">
          <span class="mono">{{ moneyText(row.total_sales) }}</span>
        </template>
        <template #total_commission="{ row }">
          <span
            class="mono"
            :class="{ 'money--bad': isClawback(row.total_commission) }"
            data-testid="cmset-commission"
          >
            {{ moneyText(row.total_commission) }}
          </span>
          <t-tag
            v-if="isClawback(row.total_commission)"
            size="small"
            variant="light"
            theme="danger"
            class="clawback-tag"
          >
            冲回
          </t-tag>
        </template>
        <template #item_count="{ row }">
          <span class="mono">{{ row.item_count }}</span>
        </template>
        <template #status="{ row }">
          <div class="status-cell">
            <t-tag
              size="small"
              variant="light"
              :theme="settlementStatusTheme(row.status)"
              data-testid="cmset-status-tag"
            >
              {{ settlementStatusLabel(row.status) }}
            </t-tag>
            <t-tooltip
              v-if="row.voucher_no"
              :content="`计提凭证 ${row.voucher_no}`"
              placement="top"
            >
              <t-tag size="small" variant="outline" theme="default">已计提</t-tag>
            </t-tooltip>
          </div>
        </template>
        <template #op="{ row }">
          <div class="op-row">
            <t-link
              v-if="canStatus && canTransition(row.status, 'APPROVED')"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-cmset-approve"
              @click="openStatus(row as CommissionSettlementRow, 'APPROVED')"
            >
              审批通过
            </t-link>
            <t-link
              v-if="canStatus && canTransition(row.status, 'PAID')"
              v-privilege="[PERM.status]"
              theme="success"
              data-testid="btn-cmset-pay"
              @click="openStatus(row as CommissionSettlementRow, 'PAID')"
            >
              确认发放
            </t-link>
            <t-link
              theme="default"
              data-testid="btn-cmset-items"
              @click="toggleExpand(row as CommissionSettlementRow)"
            >
              {{ expandedKeys.includes(row.id) ? '收起明细' : '明细' }}
            </t-link>
          </div>
        </template>

        <!-- ==================== 明细展开（哪笔订单产生多少佣金） ==================== -->
        <template #expandedRow="{ row }">
          <div class="items-block" :data-testid="`cmset-items-${row.id}`">
            <div class="items-block__head">
              <span class="items-block__title">
                佣金明细（{{ row.item_count }} 行）· 结算单 {{ row.settlement_no }}
              </span>
              <span class="items-block__meta">
                凭证：{{ voucherText(row as CommissionSettlementRow) }} · 发放时间：{{
                  paidAtText(row.paid_at)
                }}
              </span>
            </div>

            <t-alert
              v-if="detailErrors[row.id]"
              theme="error"
              :message="detailErrors[row.id]"
              class="items-block__alert"
              :data-testid="`cmset-items-error-${row.id}`"
            />

            <t-table
              v-else
              row-key="id"
              size="small"
              :data="details[row.id]?.items ?? []"
              :columns="itemColumns"
              :loading="detailLoading[row.id] === true"
              :empty="detailEmptyText(row as CommissionSettlementRow)"
              :data-testid="`cmset-items-table-${row.id}`"
            >
              <template #so_no="{ row: item }">
                <span class="mono">{{ item.so_no || '—' }}</span>
              </template>
              <template #ar_no="{ row: item }">
                <span class="mono">{{ item.ar_no || '—' }}</span>
              </template>
              <template #rule_name="{ row: item }">
                <span>{{ item.rule_name || '—（规则已删除）' }}</span>
              </template>
              <template #rate="{ row: item }">
                <span class="mono">{{ itemRateText(item) }}</span>
              </template>
              <template #base_amount="{ row: item }">
                <span class="mono" :class="{ 'money--bad': isClawback(item.base_amount) }">
                  {{ moneyText(item.base_amount) }}
                </span>
              </template>
              <template #commission="{ row: item }">
                <span class="mono" :class="{ 'money--bad': isClawback(item.commission) }">
                  {{ moneyText(item.commission) }}
                </span>
              </template>
              <template #remark="{ row: item }">
                <span>{{ item.remark || '—' }}</span>
              </template>
            </t-table>

            <div v-if="details[row.id]" class="items-block__foot" :data-testid="`cmset-items-total-${row.id}`">
              明细佣金合计 <b class="mono">{{ moneyText(sumItems(details[row.id]?.items ?? [])) }}</b>
              · 结算单应发 <b class="mono">{{ moneyText(row.total_commission) }}</b>
              <span class="items-block__hint">（两者由后端同事务写入，应相等）</span>
            </div>
          </div>
        </template>
      </t-table>

      <div class="pager-row">
        <span class="pager-row__hint">
          同人同期间只能有一张存活的结算单；草稿可重算覆盖，已审批 / 已发放的拒绝重算
        </span>
        <t-pagination
          :current="page"
          :page-size="SETTLEMENT_PAGE_SIZE"
          :total="total"
          :show-page-size="false"
          show-jumper
          data-testid="cmset-pager"
          @current-change="handlePageChange"
        />
      </div>
    </t-card>

    <!-- ==================== ② 生成结算单 ==================== -->
    <t-dialog
      v-model:visible="settleVisible"
      header="生成佣金结算单"
      :width="620"
      :confirm-btn="{ content: '生成', loading: settling, disabled: settling }"
      :cancel-btn="{ disabled: settling }"
      :close-on-overlay-click="false"
      data-testid="cmset-settle-dialog"
      @confirm="submitSettle"
    >
      <t-form label-width="110px" class="form">
        <t-form-item label="结算期间" required-mark>
          <t-select
            v-model="settleForm.period_code"
            :options="periods"
            placeholder="选择期间（yyyy-MM）"
            data-testid="cmset-settle-period"
          />
        </t-form-item>
        <t-form-item label="销售员">
          <t-select
            v-if="!userOptionsFailed"
            v-model="settleForm.salesperson_id"
            :options="userOptions"
            filterable
            clearable
            placeholder="留空 = 给所有有可计佣差额的销售员各生成一张"
            data-testid="cmset-settle-salesperson"
          />
          <t-input
            v-else
            v-model="settleForm.salesperson_id"
            placeholder="候选列表无权限读取：可留空（全员）或粘贴销售员 UUID"
            data-testid="cmset-settle-salesperson-manual"
          />
        </t-form-item>
        <t-form-item label="备注">
          <t-input
            v-model="settleForm.remark"
            placeholder="选填，写进结算单备注（重算会覆盖）"
            data-testid="cmset-settle-remark"
          />
        </t-form-item>
      </t-form>

      <t-alert
        v-if="settleFormError"
        theme="error"
        :message="settleFormError"
        data-testid="cmset-settle-error"
      />

      <!-- 生成结果（成功 + 跳过都要说；200 不是错误） -->
      <div v-if="settleResult" class="settle-result" data-testid="cmset-settle-result">
        <t-alert theme="success" :message="settleResultText(settleResult)" />
        <ul v-if="settleResult.skipped.length" class="settle-result__skip">
          <li v-for="row in settleResult.skipped" :key="row.salesperson_id">
            {{ skippedText(row) }}
          </li>
        </ul>
      </div>
    </t-dialog>

    <!-- ==================== ③ 审批 / 发放 ==================== -->
    <t-dialog
      v-model:visible="statusVisible"
      :header="statusTarget ? `${statusActionLabel(statusTarget.to)} — ${statusTarget.row.settlement_no}` : '状态流转'"
      :width="600"
      :confirm-btn="{
        content: statusTarget ? statusActionLabel(statusTarget.to) : '确认',
        theme: statusTarget?.to === 'PAID' ? 'success' : 'primary',
        loading: statusSubmitting,
        disabled: statusSubmitting,
      }"
      :cancel-btn="{ disabled: statusSubmitting }"
      :close-on-overlay-click="false"
      data-testid="cmset-status-dialog"
      @confirm="submitStatus"
    >
      <div v-if="statusTarget" class="status-body">
        <div class="status-body__row">
          <span class="status-body__label">销售员 / 期间</span>
          <span>{{ statusTarget.row.salesperson_name }} · {{ periodLabel(statusTarget.row.period_code) }}</span>
        </div>
        <div class="status-body__row">
          <span class="status-body__label">基数 / 应发佣金</span>
          <span class="mono">
            {{ moneyText(statusTarget.row.total_sales) }} /
            <b :class="{ 'money--bad': isClawback(statusTarget.row.total_commission) }">
              {{ moneyText(statusTarget.row.total_commission) }}
            </b>
          </span>
        </div>
        <t-alert theme="warning" :message="statusActionHint(statusTarget.to)" class="status-body__alert" />

        <t-form v-if="statusNeedsReason(statusTarget.to)" label-width="80px" class="form">
          <t-form-item label="发放说明">
            <t-textarea
              v-model="statusReason"
              placeholder="选填，例如「2026-11 工资一并发放」"
              data-testid="cmset-status-reason"
            />
          </t-form-item>
        </t-form>
      </div>

      <t-alert
        v-if="statusError"
        theme="error"
        :message="statusError"
        class="status-body__alert"
        data-testid="cmset-status-error"
      />
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 佣金结算页（销售链 · SO-37 / ARCH-7 §15.4.3，后端 SO-36）
 *
 * # 一句话业务
 *
 * 按「期间 × 销售员」把**已收款**的应收金额汇总成一张结算单，逐笔列出哪张订单 / 应收
 * 产生了多少佣金；审批通过出计提凭证、确认发放出付款凭证。
 *
 * # 三条口径（写错就会重复发钱 / 少记账）
 *
 * 1. **按已收款计佣**：基数是应收的已核销金额，不是开单金额（后端 `received_amount`）；
 * 2. **状态单向**：`DRAFT → APPROVED → PAID`，没有回退边（回退等于改写已入账凭证）；
 *    按钮显隐靠 `canTransition()` 而不是硬编码状态比较；
 * 3. **生成是幂等重算**：同人同期间重复生成会覆盖旧草稿；`APPROVED` / `PAID` 会 409 拒重算，
 *    这在页面上是**正常提示**，不是故障。
 *
 * # 明细是懒加载的
 *
 * 列表接口只带 `item_count`，明细要 `GET /commission/{id}`。展开时才拉（带缓存），
 * 否则一页 20 张 × 每张几十行明细会打出 20 个请求。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  changeCommissionStatus,
  fetchCommissionSettlement,
  fetchCommissionSettlementPage,
  settleCommission,
  type CommissionItemRow,
  type CommissionSettlementDetail,
  type CommissionSettlementFilter,
  type CommissionSettlementRow,
  type CommissionSettlementStatus,
  type SettleCommissionRes,
} from '@/api/business/erp/commission';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import { todayPeriod } from '@/views/business/erp/shared/money';
import {
  ITEM_COLUMNS,
  SETTLEMENT_COLUMNS,
  SETTLEMENT_PAGE_SIZE,
  SETTLEMENT_STATUS_OPTIONS,
  buildSettlePayload,
  canTransition,
  emptySettlementFilter,
  isClawback,
  itemRateText,
  moneyText,
  pageTotals,
  paidAtText,
  periodLabel,
  periodOptions,
  settlementEmptyText,
  settlementStatusLabel,
  settlementStatusTheme,
  settleResultText,
  skippedText,
  statusActionHint,
  statusActionLabel,
  statusNeedsReason,
  validateSettlePeriod,
  voucherText,
} from './commission-logic';

/** 本页权限码（与 `V0310__commission_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:commission-settle:list',
  create: 'business:erp:commission-settle:create',
  status: 'business:erp:commission-settle:status',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canStatus = computed(() => userStore.hasPermission(PERM.status));

const columns = SETTLEMENT_COLUMNS;
const itemColumns = ITEM_COLUMNS;
const statusOptions = SETTLEMENT_STATUS_OPTIONS;
/** 期间候选：本月往前 12 个月（未来月一定没有已收款，选出来只会是空单） */
const periods = periodOptions();

/** 错误 → 人话（`ApiError` 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== 列表 ====================

const filter = reactive<CommissionSettlementFilter>(emptySettlementFilter());
const rows = ref<CommissionSettlementRow[]>([]);
const total = ref(0);
const page = ref(1);
const loading = ref(false);
const loadError = ref('');

const emptyText = computed(() => {
  if (loading.value) {
    return '加载中…';
  }
  return settlementEmptyText(filter);
});

const totals = computed(() => pageTotals(rows.value));

async function loadRows(target: number = page.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchCommissionSettlementPage({
      ...filter,
      page: target,
      page_size: SETTLEMENT_PAGE_SIZE,
    });
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    page.value = data.page ?? target;
    // 行集变了：清掉不再可见行的明细缓存与展开态
    const alive = new Set(rows.value.map((row) => row.id));
    for (const key of Object.keys(details.value)) {
      if (!alive.has(key)) {
        delete details.value[key];
        delete detailErrors.value[key];
        delete detailLoading.value[key];
      }
    }
    expandedKeys.value = expandedKeys.value.filter((key) => alive.has(key));
    const lastPage = Math.max(1, Math.ceil(total.value / SETTLEMENT_PAGE_SIZE));
    if (page.value > lastPage) {
      page.value = lastPage;
      await loadRows(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `结算单加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  page.value = 1;
  void loadRows(1);
}

function handleReset(): void {
  Object.assign(filter, emptySettlementFilter());
  page.value = 1;
  void loadRows(1);
}

function handlePageChange(current: number): void {
  page.value = current;
  void loadRows(current);
}

// ==================== 明细（懒加载 + 缓存） ====================

const expandedKeys = ref<string[]>([]);
const details = ref<Record<string, CommissionSettlementDetail>>({});
const detailLoading = ref<Record<string, boolean>>({});
const detailErrors = ref<Record<string, string>>({});

/** 明细为空时的文案（区分「草稿未算出行」「零明细」） */
function detailEmptyText(row: CommissionSettlementRow): string {
  if (row.item_count > 0) {
    return '明细加载中…';
  }
  return '这张结算单没有明细行（该期间没有新增已收款，也没有退货冲回）';
}

async function loadDetail(id: string): Promise<void> {
  if (details.value[id] || detailLoading.value[id]) {
    return;
  }
  detailLoading.value[id] = true;
  detailErrors.value[id] = '';
  try {
    details.value[id] = await fetchCommissionSettlement(id);
  } catch (error) {
    detailErrors.value[id] = `明细加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value[id] = false;
  }
}

function handleExpandChange(keys: Array<string | number>): void {
  expandedKeys.value = keys.map((key) => String(key));
  for (const id of expandedKeys.value) {
    void loadDetail(id);
  }
}

/** 行尾「明细」链接：等价于点展开箭头（省得用户找不到那个小三角） */
function toggleExpand(row: CommissionSettlementRow): void {
  if (expandedKeys.value.includes(row.id)) {
    expandedKeys.value = expandedKeys.value.filter((key) => key !== row.id);
    return;
  }
  expandedKeys.value = [...expandedKeys.value, row.id];
  void loadDetail(row.id);
}

/** 明细佣金合计（与单头 `total_commission` 应相等；用来发现后端算错） */
function sumItems(items: CommissionItemRow[]): number {
  return items.reduce((acc, item) => {
    const value = Number(item.commission ?? 0);
    return acc + (Number.isFinite(value) ? value : 0);
  }, 0);
}

// ==================== 销售员候选 ====================

const users = ref<UserRes[]>([]);
const userOptionsFailed = ref(false);

const userOptions = computed(() =>
  users.value.map((item) => ({
    label: `${item.real_name || item.username}（${item.username}）`,
    value: item.id,
  })),
);

/** 销售员候选要 `system:user:list`（销售员 / 销售主管都没有）→ 失败就降级成手填 */
async function loadSalespersonOptions(): Promise<void> {
  try {
    const data = await pageUsers({ page: 1, page_size: 200, status: 1 });
    users.value = data.list ?? [];
    userOptionsFailed.value = users.value.length === 0;
  } catch {
    users.value = [];
    userOptionsFailed.value = true;
  }
}

// ==================== 生成结算单 ====================

const settleVisible = ref(false);
const settling = ref(false);
const settleFormError = ref('');
const settleResult = ref<SettleCommissionRes | null>(null);
const settleForm = reactive({
  period_code: todayPeriod(),
  salesperson_id: '',
  remark: '',
});

function openSettle(): void {
  settleForm.period_code = todayPeriod();
  settleForm.salesperson_id = '';
  settleForm.remark = '';
  settleFormError.value = '';
  settleResult.value = null;
  settleVisible.value = true;
}

async function submitSettle(): Promise<void> {
  if (settling.value) {
    return;
  }
  const error = validateSettlePeriod(settleForm.period_code);
  if (error) {
    settleFormError.value = error;
    return;
  }
  settleFormError.value = '';
  settling.value = true;
  try {
    const res = await settleCommission(
      buildSettlePayload(settleForm.period_code, settleForm.salesperson_id || undefined, settleForm.remark),
    );
    settleResult.value = res;
    // 弹窗不自动关：用户要看「生成了几张 / 谁被跳过 / 为什么」
    await loadRows(1);
    if (res.settlements.length > 0) {
      MessagePlugin.success(settleResultText(res));
    } else {
      MessagePlugin.warning(settleResultText(res));
    }
  } catch (err) {
    settleFormError.value = messageOf(err);
  } finally {
    settling.value = false;
  }
}

// ==================== 审批 / 发放 ====================

const statusVisible = ref(false);
const statusSubmitting = ref(false);
const statusError = ref('');
const statusReason = ref('');
const statusTarget = ref<{ row: CommissionSettlementRow; to: CommissionSettlementStatus } | null>(null);

function openStatus(row: CommissionSettlementRow, to: CommissionSettlementStatus): void {
  if (!canTransition(row.status, to)) {
    MessagePlugin.warning(`不能从「${settlementStatusLabel(row.status)}」直接到「${settlementStatusLabel(to)}」`);
    return;
  }
  statusTarget.value = { row, to };
  statusReason.value = '';
  statusError.value = '';
  statusVisible.value = true;
}

async function submitStatus(): Promise<void> {
  const target = statusTarget.value;
  if (!target || statusSubmitting.value) {
    return;
  }
  statusSubmitting.value = true;
  statusError.value = '';
  try {
    const updated = await changeCommissionStatus(target.row.id, {
      status: target.to,
      reason: statusReason.value.trim() || null,
    });
    MessagePlugin.success(`${statusActionLabel(target.to)}成功`);
    statusVisible.value = false;
    // 回包是完整详情：直接刷新该行明细缓存，列表重载拿最新合计与凭证号
    details.value[target.row.id] = updated;
    await loadRows(page.value);
  } catch (err) {
    statusError.value = messageOf(err);
  } finally {
    statusSubmitting.value = false;
  }
}

onMounted(() => {
  void loadRows(1);
  void loadSalespersonOptions();
});
</script>

<style scoped lang="less">
.commission-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.panel-card {
  padding: 16px 20px 8px;
}

.page-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 14px;
}

.page-head__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.page-head__tip {
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.filter-row__period {
  width: 170px;
}

.filter-row__select {
  width: 170px;
}

.filter-row__keyword {
  width: 220px;
}

.filter-row__spacer {
  flex: 1;
}

.gate-wrap {
  display: inline-flex;
}

.page-alert {
  margin-bottom: 12px;
}

.totals-strip {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding: 8px 12px;
  margin-bottom: 10px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
  background: var(--td-bg-color-container-hover);
  border-radius: var(--td-radius-default);
}

.totals-strip__sep {
  color: var(--td-text-color-placeholder);
}

.totals-strip__hint {
  color: var(--td-text-color-placeholder);
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.money--bad {
  color: var(--td-error-color);
  font-weight: 600;
}

.period-cell {
  display: flex;
  flex-direction: column;
  line-height: 18px;
}

.period-cell__code {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.status-cell {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.clawback-tag {
  margin-left: 4px;
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
  white-space: nowrap;
}

.items-block {
  padding: 8px 12px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: var(--td-radius-default);
}

.items-block__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 8px;
}

.items-block__title {
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.items-block__meta {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.items-block__alert {
  margin-bottom: 8px;
}

.items-block__foot {
  margin-top: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.items-block__hint {
  color: var(--td-text-color-placeholder);
}

.pager-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 0 8px;
}

.pager-row__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.pager-row :deep(.t-pagination) {
  margin-left: auto;
}

.form :deep(.t-form__item) {
  margin-bottom: 12px;
}

/* 表单控件铺满一行（`t-select` 默认按内容宽，长销售员名会被截断） */
.form :deep(.t-form__controls-content .t-select) {
  width: 100%;
}

.settle-result {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.settle-result__skip {
  margin: 0;
  padding-left: 20px;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.status-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.status-body__row {
  display: flex;
  gap: 12px;
  font-size: 13px;
}

.status-body__label {
  width: 110px;
  color: var(--td-text-color-secondary);
}

.status-body__alert {
  margin-top: 4px;
}
</style>
