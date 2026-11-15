<template>
  <div class="commission-page">
    <!-- 筛选 / 分页 / 行操作交给 ListPage（P3-12 收口） -->
    <ListPage
      ref="listRef"
      title="佣金结算"
      meta-unit="张"
      testid="cmset"
      table-testid="cmset-table"
      meta-testid="cmset-meta"
      btn-prefix="btn-cmset"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="SETTLEMENT_PAGE_SIZE"
      @loaded="onLoaded"
    >
      <!-- 页头后缀：「按已收款计佣」是这一页最容易被误读的口径 -->
      <template #head-extra>
        <t-tag size="small" variant="light" theme="primary">按已收款计佣 · 期间 × 销售员</t-tag>
      </template>

      <!-- 口径说明（页面级信息条，整宽，不属于按钮行） -->
      <template #filter-footer>
        <p class="page-tip">
          佣金基数 = 应收单<b>已核销（真正收到钱）</b>的金额，不是开单金额；退货 / 退款会落<b>负数基数</b>（冲回）。
          状态单向走：草稿 → 审批通过（出计提凭证）→ 确认发放（出付款凭证），<b>不能回退</b>。
        </p>
      </template>

      <template #toolbar>
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
      </template>

      <!-- 页脚合计条：只汇总当前页（拿不到全量，绝不假装有） -->
      <template #footer>
        <div v-if="totals.count" class="totals-strip" data-testid="cmset-totals">
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
      </template>

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
          <t-tooltip v-if="row.voucher_no" :content="`计提凭证 ${row.voucher_no}`" placement="top">
            <t-tag size="small" variant="outline" theme="default">已计提</t-tag>
          </t-tooltip>
        </div>
      </template>

      <!-- 行操作：显隐取决于「状态能否迁到目标态 + 权限」，声明式表达不了，页面自绘 -->
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
          <t-link theme="default" data-testid="btn-cmset-items" @click="openItems(row as CommissionSettlementRow)">
            明细
          </t-link>
        </div>
      </template>

      <template #empty="{ filtered }">
        <div class="panel-empty" data-testid="cmset-empty">
          <t-icon class="panel-empty__icon" name="file" />
          <p class="panel-empty__title">{{ settlementEmptyText(filter) }}</p>
          <p v-if="filtered" class="panel-empty__desc">换个期间 / 状态再查，或点「重置」看全部</p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 佣金明细（哪笔订单产生多少佣金） ==================== -->
    <t-drawer
      v-model:visible="itemsVisible"
      :header="itemsTitle"
      size="880px"
      :footer="false"
      data-testid="cmset-items-drawer"
    >
      <div v-if="itemsRow" class="items-block" :data-testid="`cmset-items-${itemsRow.id}`">
        <div class="items-block__head">
          <span class="items-block__title">
            佣金明细（{{ itemsRow.item_count }} 行）· 结算单 {{ itemsRow.settlement_no }}
          </span>
          <span class="items-block__meta">
            凭证：{{ voucherText(itemsRow) }} · 发放时间：{{ paidAtText(itemsRow.paid_at) }}
          </span>
        </div>

        <t-alert
          v-if="detailErrors[itemsRow.id]"
          theme="error"
          :message="detailErrors[itemsRow.id]"
          class="items-block__alert"
          :data-testid="`cmset-items-error-${itemsRow.id}`"
        />

        <t-table
          v-else
          row-key="id"
          size="small"
          :data="details[itemsRow.id]?.items ?? []"
          :columns="itemColumns"
          :loading="detailLoading[itemsRow.id] === true"
          :empty="detailEmptyText(itemsRow)"
          :data-testid="`cmset-items-table-${itemsRow.id}`"
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

        <div v-if="details[itemsRow.id]" class="items-block__foot" :data-testid="`cmset-items-total-${itemsRow.id}`">
          明细佣金合计 <b class="mono">{{ moneyText(sumItems(details[itemsRow.id]?.items ?? [])) }}</b>
          · 结算单应发 <b class="mono">{{ moneyText(itemsRow.total_commission) }}</b>
          <span class="items-block__hint">（两者由后端同事务写入，应相等）</span>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ③ 生成结算单 ==================== -->
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

    <!-- ==================== ④ 审批 / 发放 ==================== -->
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
 * 列表接口只带 `item_count`，明细要 `GET /commission/{id}`。点「明细」才拉（带缓存），
 * 否则一页 20 张 × 每张几十行明细会打出 20 个请求。
 *
 * 明细**从展开行改成了抽屉**：ListPage 只透传 `expandedRow` 插槽，不转发 t-table 的
 * `expand-change`、也不支持受控 `expandedRowKeys`，页面拿不到「这一行刚被展开」的时机，
 * 懒加载就无从触发（一次性把 20 张的明细拉回来正是这条注释要避免的）。抽屉保留原来的
 * 全部文案与 `data-testid`（`cmset-items-*`）。
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
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
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
/** 状态下拉（`''` = 全部，交给 placeholder，不在选项里放空串） */
const statusOptions = SETTLEMENT_STATUS_OPTIONS.filter((item) => item.value !== '').map((item) => ({ ...item }));
/** 期间候选：本月往前 12 个月（未来月一定没有已收款，选出来只会是空单） */
const periods = periodOptions();

/** 错误 → 人话（`ApiError` 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== 列表（筛选 / 分页 / 行操作交给 ListPage） ====================

/** 筛选状态（`filterState` 就地读写） */
const filter = reactive<CommissionSettlementFilter>(emptySettlementFilter());
/** 当前页（保存 / 状态流转后回页用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** 本页行（页脚合计用；`#footer` 拿得 到 rows，这里存一份给 computed） */
const pageRows = ref<CommissionSettlementRow[]>([]);
/** ListPage 实例（生成 / 状态流转后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 本页合计（前端只汇总**当前页**，不是全量） */
const totals = computed(() => pageTotals(pageRows.value));

/**
 * 筛选项声明（关键词走回车，下拉一律不「值变即查」，与改造前手感一致）。
 *
 * `computed` 是因为销售员下拉的候选是异步拉回来的（拉不到就是空下拉，与改造前一致）。
 */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'period_code',
    type: 'select',
    placeholder: '全部期间',
    width: 170,
    options: periods,
    testid: 'cmset-filter-period',
  },
  {
    key: 'salesperson_id',
    type: 'select',
    placeholder: '全部销售员',
    width: 170,
    options: userOptions.value,
    filterable: true,
    testid: 'cmset-filter-salesperson',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 170,
    options: statusOptions,
    testid: 'cmset-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '结算单号 / 销售员',
    width: 220,
    testid: 'cmset-filter-keyword',
  },
]);

/** 拉一页 */
const fetcher: ListFetcher<CommissionSettlementRow> = (query) =>
  fetchCommissionSettlementPage(query as Parameters<typeof fetchCommissionSettlementPage>[0]);

/** 入参拼装（`{...filter, page, page_size}`，与改造前逐字同口径） */
const queryBuilder: ListQueryBuilder = (state, page, size) => ({
  ...(state as CommissionSettlementFilter),
  page,
  page_size: size,
});

/** 一页拉完：同步页码，并把不再可见行的明细缓存 / 错误清掉（省内存，也避免旧行残留） */
function onLoaded(payload: { rows: CommissionSettlementRow[]; page: number }): void {
  current.value = payload.page;
  pageRows.value = payload.rows;
  const alive = new Set(payload.rows.map((row) => row.id));
  for (const key of Object.keys(details.value)) {
    if (!alive.has(key)) {
      delete details.value[key];
      delete detailErrors.value[key];
      delete detailLoading.value[key];
    }
  }
}

// ==================== 明细（懒加载 + 缓存） ====================

/** 明细抽屉可见 */
const itemsVisible = ref(false);
/** 抽屉里这张结算单 */
const itemsRow = ref<CommissionSettlementRow | null>(null);
/** 抽屉标题 */
const itemsTitle = computed(() => (itemsRow.value ? `佣金明细 · ${itemsRow.value.settlement_no}` : '佣金明细'));

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

/** 行尾「明细」：打开抽屉并拉明细（有缓存就不再请求） */
async function openItems(row: CommissionSettlementRow): Promise<void> {
  itemsRow.value = row;
  itemsVisible.value = true;
  await loadDetail(row.id);
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
    await listRef.value?.load(1);
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
    await listRef.value?.load(current.value);
  } catch (err) {
    statusError.value = messageOf(err);
  } finally {
    statusSubmitting.value = false;
  }
}

onMounted(() => {
  // 列表由 ListPage 自己在 setup 里拉首屏；这里只补销售员候选
  void loadSalespersonOptions();
});
</script>

<style scoped lang="less">
.commission-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.page-tip {
  margin: 8px 0 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.gate-wrap {
  display: inline-flex;
}

.totals-strip {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding: 8px 12px;
  margin-top: 10px;
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

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 明细（抽屉） ==================== */

.items-block {
  padding: 4px 0 12px;
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
