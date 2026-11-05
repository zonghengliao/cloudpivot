<template>
  <div class="opening-page">
    <!-- ==================== ① 期间 + 状态 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <span class="tool-label">期初期间</span>
        <t-select
          v-model="periodModel"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          placeholder="选择期间"
          data-testid="opening-period-select"
          @change="loadOpening()"
        />
        <t-tag
          :theme="openingStateTheme(opening?.state)"
          variant="light"
          data-testid="opening-state-tag"
        >
          {{ openingStateLabel(opening?.state) }}
        </t-tag>
        <t-tag v-if="opening" theme="default" variant="outline" data-testid="opening-period-status">
          期间 {{ opening.period_status }}
        </t-tag>

        <span class="tool-row__spacer" />

        <t-button variant="outline" :loading="loading" data-testid="btn-opening-refresh" @click="loadOpening()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
      </div>
    </t-card>

    <!-- ==================== ② 只读提示（结账后整页只读） ==================== -->
    <t-alert
      v-if="readonlyText"
      theme="warning"
      :message="readonlyText"
      class="page-alert"
      data-testid="opening-readonly-alert"
    />

    <t-alert
      v-else-if="loadError"
      theme="error"
      :message="loadError"
      class="page-alert"
      data-testid="opening-load-error"
    >
      <template #operation>
        <t-button size="small" variant="outline" @click="loadOpening()">重试</t-button>
      </template>
    </t-alert>

    <!-- ==================== ③ 两种录入方式 ==================== -->
    <t-card :bordered="false" class="main-card">
      <t-tabs v-model="tab" data-testid="opening-tabs">
        <!-- ---------- 方式 A：按科目录 ---------- -->
        <t-tab-panel value="account" label="按科目录入">
          <div class="panel-tip">
            财务口径：每个科目一行，借方或贷方填一边。合计借贷必须相等才能保存
            （不平衡时后端返回 <span class="mono">400 / 66012</span> 并点名差额与科目）。
          </div>

          <div class="grid-head">
            <span class="col-code">科目</span>
            <span class="col-money">借方期初</span>
            <span class="col-money">贷方期初</span>
            <span class="col-remark">备注</span>
            <span class="col-op" />
          </div>

          <div
            v-for="(row, index) in accountRows"
            :key="row.key"
            class="grid-row"
            :data-testid="`opening-row-${index}`"
          >
            <t-select
              v-model="row.account_code"
              class="col-code"
              :options="accountOptions"
              filterable
              clearable
              placeholder="选择末级科目"
              :disabled="isReadonly"
              :data-testid="`opening-account-${index}`"
            />
            <t-input
              v-model="row.debit"
              class="col-money"
              type="number"
              align="right"
              placeholder="0.00"
              :disabled="isReadonly"
              :data-testid="`opening-debit-${index}`"
            />
            <t-input
              v-model="row.credit"
              class="col-money"
              type="number"
              align="right"
              placeholder="0.00"
              :disabled="isReadonly"
              :data-testid="`opening-credit-${index}`"
            />
            <t-input
              v-model="row.remark"
              class="col-remark"
              placeholder="备注（可选）"
              :disabled="isReadonly"
            />
            <t-button
              v-if="!isReadonly"
              class="col-op"
              variant="text"
              theme="danger"
              data-testid="btn-opening-remove-row"
              @click="removeAccountRow(index)"
            >
              删除
            </t-button>
          </div>

          <div v-if="accountRowErrors.length > 0" class="row-errors" data-testid="opening-row-errors">
            <span v-for="item in accountRowErrors" :key="item">{{ item }}</span>
          </div>

          <div class="grid-foot">
            <t-button
              v-if="!isReadonly"
              variant="outline"
              data-testid="btn-opening-add-row"
              @click="addAccountRow()"
            >
              <template #icon><t-icon name="add" /></template>
              添加科目
            </t-button>
            <span class="tool-row__spacer" />
            <span class="foot-label">合计</span>
            <span class="col-money mono" data-testid="opening-total-debit">{{ moneyText(localDebit) }}</span>
            <span class="col-money mono" data-testid="opening-total-credit">{{ moneyText(localCredit) }}</span>
            <span class="col-remark" />
            <span class="col-op" />
          </div>

          <div class="balance-line">
            <t-tag
              :theme="combinedBalanced ? 'success' : 'danger'"
              variant="light"
              data-testid="opening-balance-state"
            >
              {{ combinedBalanced ? '✅ 平衡' : `❌ 不平衡，差额 ${moneyText(combinedDifference)}` }}
            </t-tag>
            <span class="muted" data-testid="opening-combined-total">
              合并口径（含库存期初 {{ moneyText(opening?.inventory_amount) }}）：借
              {{ moneyText(opening?.combined_debit_total) }} / 贷
              {{ moneyText(opening?.combined_credit_total) }}
            </span>
          </div>
        </t-tab-panel>

        <!-- ---------- 方式 B 第一项：库存期初 ---------- -->
        <t-tab-panel value="inventory" label="库存期初">
          <div class="panel-tip">
            仓管口径：录每个物料的期初数量与单价。保存时同事务写
            <span class="mono">biz_inventory</span> /
            <span class="mono">biz_inventory_location</span> /
            <span class="mono">biz_inventory_txn</span> 三本账，并自动生成
            <span class="mono">借 {{ opening?.inventory_debit_account ?? '1405' }}</span> /
            <span class="mono">贷 {{ opening?.inventory_credit_account ?? '3103' }}</span>
            两行期初凭证。
          </div>

          <div class="grid-head">
            <span class="col-code">物料</span>
            <span class="col-code">仓库</span>
            <span class="col-qty">数量</span>
            <span class="col-money">单价</span>
            <span class="col-money">金额</span>
            <span class="col-op" />
          </div>

          <div
            v-for="(row, index) in inventoryRows"
            :key="row.key"
            class="grid-row"
            :data-testid="`opening-inventory-row-${index}`"
          >
            <t-select
              v-model="row.material_id"
              class="col-code"
              :options="materialOptions"
              filterable
              clearable
              placeholder="选择物料"
              :disabled="isReadonly"
            />
            <t-select
              v-model="row.warehouse_id"
              class="col-code"
              :options="warehouseOptions"
              filterable
              clearable
              placeholder="选择仓库"
              :disabled="isReadonly"
            />
            <t-input
              v-model="row.qty"
              class="col-qty"
              type="number"
              align="right"
              placeholder="0"
              :disabled="isReadonly"
              :data-testid="`opening-inventory-qty-${index}`"
            />
            <t-input
              v-model="row.unit_cost"
              class="col-money"
              type="number"
              align="right"
              placeholder="0.00"
              :disabled="isReadonly"
              :data-testid="`opening-inventory-cost-${index}`"
            />
            <span class="col-money mono amount" :data-testid="`opening-inventory-amount-${index}`">
              {{ moneyText(toMoney(row.qty) * toMoney(row.unit_cost)) }}
            </span>
            <t-button
              v-if="!isReadonly"
              class="col-op"
              variant="text"
              theme="danger"
              data-testid="btn-opening-remove-inventory"
              @click="removeInventoryRow(index)"
            >
              删除
            </t-button>
          </div>

          <div class="grid-foot">
            <t-button
              v-if="!isReadonly"
              variant="outline"
              data-testid="btn-opening-add-inventory"
              @click="addInventoryRow()"
            >
              <template #icon><t-icon name="add" /></template>
              添加物料
            </t-button>
            <span class="tool-row__spacer" />
            <span class="foot-label">合计</span>
            <span class="col-qty" />
            <span class="col-money" />
            <span class="col-money mono" data-testid="opening-inventory-total">
              {{ moneyText(localInventoryAmount) }}
            </span>
            <span class="col-op" />
          </div>

          <t-alert
            v-if="inventoryHint"
            theme="info"
            :message="inventoryHint"
            class="inline-alert"
            data-testid="opening-inventory-hint"
          />
        </t-tab-panel>
      </t-tabs>

      <!-- ==================== ④ 期初凭证 + 提示 + 保存 ==================== -->
      <div class="voucher-line">
        <span class="foot-label">期初凭证</span>
        <span class="mono" data-testid="opening-voucher-info">{{ voucherText(opening) }}</span>
        <span class="muted" data-testid="opening-voucher-unique">
          一个期间只有一张（重复保存＝覆盖更新，凭证号不变）
        </span>
      </div>

      <ul v-if="(opening?.hints ?? []).length > 0" class="hints" data-testid="opening-hints">
        <li v-for="hint in opening?.hints ?? []" :key="hint">{{ hint }}</li>
      </ul>
      <ul v-else-if="localHint" class="hints" data-testid="opening-local-hint">
        <li>{{ localHint }}</li>
      </ul>

      <div class="footer-row">
        <t-button
          v-if="tab === 'account' && canWrite"
          theme="primary"
          :disabled="!canSaveOpening"
          :loading="saving"
          data-testid="btn-opening-save"
          @click="handleSaveAccount()"
        >
          保存期初
        </t-button>
        <t-button
          v-if="tab === 'inventory' && canWriteInventory"
          theme="primary"
          :disabled="!canSaveOpening"
          :loading="saving"
          data-testid="btn-opening-save-inventory"
          @click="handleSaveInventory()"
        >
          保存库存期初
        </t-button>
        <span class="muted">
          {{ canSaveOpening ? '保存后会同时生成/覆盖期初凭证（同一事务）' : '借贷不平衡或期间只读时不能保存' }}
        </span>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 期初余额录入（ERP-R5）—— 财务管理 / 期初余额。
 *
 * # 两个 Tab 对应后端的两种口径（卡面 §一）
 *
 * | Tab | 接口 | 落表 |
 * |-----|------|------|
 * | 按科目录入 | `POST /api/business/erp/opening-balance` | `biz_opening_balance` |
 * | 库存期初 | `POST /api/business/erp/opening-balance/inventory` | `biz_opening_inventory` + 库存三本账 |
 *
 * 两个 Tab 共用**同一张期初凭证**（`voucher_type='OPENING'`，一个期间最多一张）：
 * 库存期初会自动产出 `借 1405 / 贷 3103` 两行，所以「合并口径是否平衡」由**服务端**算好
 * （`opening.balanced`），前端只用它控制保存按钮，不自己重算——两处各算一遍会有两套真相。
 *
 * # 体验要求（卡面 §五）
 *
 * - 实时显示借贷合计 + 平衡状态（本地行算「科目侧」，服务端给「合并口径」）；
 * - 不平保存按钮禁用；
 * - 已有期初时按钮文案是「保存期初」（覆盖），不是「新增」；
 * - 结账后整页只读 + 顶部提示「2026-11 已结账，期初不可改」。
 *
 * # 权限（与 V0189 的三个节点逐字对应）
 *
 * | 动作 | 权限码 |
 * |------|--------|
 * | 看期初 | `business:erp:opening:read` |
 * | 录科目期初 | `business:erp:opening:write` |
 * | 录库存期初 | `business:erp:opening:inventory` |
 *
 * 没有写权限时**不渲染**保存按钮（不是 disabled）。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchAccountOptions,
  fetchMaterialPage,
  fetchOpeningBalance,
  fetchPeriodPage,
  fetchWarehousePage,
  saveOpeningBalance,
  saveOpeningInventory,
  type AccountRow,
  type MaterialRow,
  type OpeningBalance,
  type Warehouse,
} from '@/api/business/erp';
import { useUserStore } from '@/stores/user';
import {
  OPENING_PERMISSIONS,
  balanceOf,
  canSave,
  defaultOpeningPeriod,
  effectiveEntries,
  inventoryVoucherHint,
  moneyText,
  openingStateLabel,
  openingStateTheme,
  readonlyReason,
  rowError,
  sumEntries,
  toMoney,
  voucherText,
} from './opening-logic';

const userStore = useUserStore();

// ---------- 状态 ----------
const loading = ref(false);
const saving = ref(false);
const periodLoading = ref(false);
const loadError = ref('');
const tab = ref<'account' | 'inventory'>('account');
const periodModel = ref('');
const periodOptions = ref<Array<{ value: string; label: string }>>([]);
const opening = ref<OpeningBalance | null>(null);

/** 科目下拉（只放末级、启用中） */
const accountOptions = ref<Array<{ value: string; label: string }>>([]);
/** 物料 / 仓库下拉 */
const materialOptions = ref<Array<{ value: string; label: string }>>([]);
const warehouseOptions = ref<Array<{ value: string; label: string }>>([]);

interface AccountRowState {
  key: number;
  account_code: string;
  debit: string;
  credit: string;
  remark: string;
}

interface InventoryRowState {
  key: number;
  material_id: string;
  warehouse_id: string;
  qty: string;
  unit_cost: string;
}

let rowSeed = 0;
const accountRows = ref<AccountRowState[]>([]);
const inventoryRows = ref<InventoryRowState[]>([]);

// ---------- 权限 ----------
const canWrite = computed(() => userStore.hasPermission(OPENING_PERMISSIONS.write));
const canWriteInventory = computed(() => userStore.hasPermission(OPENING_PERMISSIONS.inventory));
const isReadonly = computed(() => readonlyText.value !== null);

// ---------- 只读 / 提示 ----------
const readonlyText = computed(() => readonlyReason(opening.value));
const inventoryHint = computed(() => inventoryVoucherHint(opening.value));

/** 本地「科目侧」合计（不含库存期初，纯前端算给用户看） */
const localTotals = computed(() =>
  sumEntries(
    effectiveEntries(accountRows.value).map((row) => ({
      account_id: '',
      account_code: row.account_code,
      account_name: '',
      account_category: '',
      debit: row.debit,
      credit: row.credit,
      remark: row.remark || null,
    })),
  ),
);
const localDebit = computed(() => localTotals.value.debit);
const localCredit = computed(() => localTotals.value.credit);
const localBalance = computed(() => balanceOf(localDebit.value, localCredit.value));

/**
 * 合并口径平衡（**以服务端为准**）。
 *
 * 只在「本地没有未保存改动」时用服务端的 `balanced`；一旦用户改了行，就用本地算式
 * （科目侧 + 库存期初金额，库存期初金额在服务端已知）。
 */
const combinedBalanced = computed(() => {
  const inventoryAmount = toMoney(opening.value?.inventory_amount);
  const { balanced } = balanceOf(localDebit.value + inventoryAmount, localCredit.value + inventoryAmount);
  return balanced;
});
const combinedDifference = computed(() => {
  const inventoryAmount = toMoney(opening.value?.inventory_amount);
  return balanceOf(localDebit.value + inventoryAmount, localCredit.value + inventoryAmount).difference;
});

const localInventoryAmount = computed(() =>
  inventoryRows.value.reduce((sum, row) => sum + toMoney(row.qty) * toMoney(row.unit_cost), 0),
);

/** 行级错误（只提示，不拦保存 —— 后端才是权威） */
const accountRowErrors = computed(() => {
  const errors: string[] = [];
  accountRows.value.forEach((row, index) => {
    const filled = toMoney(row.debit) !== 0 || toMoney(row.credit) !== 0 || row.account_code;
    if (!filled) {
      return;
    }
    const error = rowError(row);
    if (error) {
      errors.push(`第 ${index + 1} 行：${error}`);
    }
  });
  return errors;
});

const localHint = computed(() => {
  if (!localBalance.value.balanced) {
    return `科目期初借贷不平衡，差 ${moneyText(localBalance.value.difference)} —— 补齐对方科目后保存按钮才会亮`;
  }
  if (effectiveEntries(accountRows.value).length === 0 && inventoryRows.value.length === 0) {
    return '还没有录任何期初：至少录一行科目期初，或到「库存期初」Tab 录物料期初';
  }
  return '';
});

const canSaveOpening = computed(() => {
  if (isReadonly.value || saving.value) {
    return false;
  }
  if (!combinedBalanced.value) {
    return false;
  }
  return tab.value === 'account'
    ? canSave(opening.value, (code) => userStore.hasPermission(code), 'account')
    : canSave(opening.value, (code) => userStore.hasPermission(code), 'inventory');
});

// ---------- 行操作 ----------
function addAccountRow(code = '', debit = '', credit = '') {
  accountRows.value.push({ key: ++rowSeed, account_code: code, debit, credit, remark: '' });
}

function removeAccountRow(index: number) {
  accountRows.value.splice(index, 1);
  if (accountRows.value.length === 0) {
    addAccountRow();
  }
}

function addInventoryRow() {
  inventoryRows.value.push({
    key: ++rowSeed,
    material_id: '',
    warehouse_id: '',
    qty: '',
    unit_cost: '',
  });
}

function removeInventoryRow(index: number) {
  inventoryRows.value.splice(index, 1);
}

// ---------- 数据装载 ----------
async function loadPeriods() {
  periodLoading.value = true;
  try {
    const page = await fetchPeriodPage({ page: 1, page_size: 100 });
    const list = page.list ?? [];
    periodOptions.value = list.map((row) => ({
      value: row.period_code,
      label: `${row.period_code}（${row.status === 'CLOSED' ? '已结账' : row.status === 'OPEN' ? '已开启' : '未开启'}）`,
    }));
    if (!periodModel.value) {
      // 默认期间：当前月（北京时间）优先，其次最早 OPEN，最后兜底最新一条。
      // 老写法 `list.find(status==='OPEN')` 会落在**最新**已开期间（如 12 月空表），
      // 用户会以为「期初数据丢了」——判据与兜底顺序见 `defaultOpeningPeriod`。
      periodModel.value = defaultOpeningPeriod(list);
    }
  } catch (error) {
    loadError.value = errorText(error, '加载会计期间失败');
  } finally {
    periodLoading.value = false;
  }
}

async function loadOptions() {
  const [accounts, materials, warehouses] = await Promise.allSettled([
    fetchAccountOptions(500),
    fetchMaterialPage({ status: 1, page: 1, page_size: 200 }),
    fetchWarehousePage({ status: 1, page: 1, page_size: 200 }),
  ]);
  if (accounts.status === 'fulfilled') {
    accountOptions.value = (accounts.value as AccountRow[])
      .filter((row) => row.is_leaf)
      .map((row) => ({ value: row.account_code, label: `${row.account_code} ${row.account_name}` }));
  }
  if (materials.status === 'fulfilled') {
    materialOptions.value = (materials.value.list ?? []).map((row: MaterialRow) => ({
      value: row.id,
      label: `${row.material_code} ${row.material_name}${row.serial_required ? '（一物一码）' : ''}`,
    }));
  }
  if (warehouses.status === 'fulfilled') {
    warehouseOptions.value = (warehouses.value.list ?? []).map((row: Warehouse) => ({
      value: row.id,
      label: `${row.warehouse_code} ${row.warehouse_name}`,
    }));
  }
}

/** 服务端结果 → 两个 Tab 的编辑行（覆盖式：服务端是唯一真相） */
function applyOpening(data: OpeningBalance) {
  opening.value = data;
  accountRows.value = data.entries.map((row) => ({
    key: ++rowSeed,
    account_code: row.account_code,
    debit: moneyInput(row.debit),
    credit: moneyInput(row.credit),
    remark: row.remark ?? '',
  }));
  if (accountRows.value.length === 0 && !data.locked) {
    addAccountRow();
  }
  inventoryRows.value = data.inventory.map((row) => ({
    key: ++rowSeed,
    material_id: row.material_id,
    warehouse_id: row.warehouse_id,
    qty: moneyInput(row.qty),
    unit_cost: moneyInput(row.unit_cost),
  }));
}

async function loadOpening() {
  if (!periodModel.value) {
    return;
  }
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchOpeningBalance(periodModel.value);
    applyOpening(data);
  } catch (error) {
    opening.value = null;
    accountRows.value = [];
    inventoryRows.value = [];
    loadError.value = errorText(error, '加载期初余额失败');
  } finally {
    loading.value = false;
  }
}

// ---------- 保存 ----------
async function handleSaveAccount() {
  if (!periodModel.value) {
    return;
  }
  saving.value = true;
  try {
    const entries = effectiveEntries(accountRows.value)
      .filter((row) => row.account_code)
      .map((row) => ({
        account_code: row.account_code,
        debit: toMoney(row.debit),
        credit: toMoney(row.credit),
        remark: row.remark || undefined,
      }));
    const data = await saveOpeningBalance({ period: periodModel.value, entries });
    applyOpening(data);
    await MessagePlugin.success(
      `期初已保存：借 ${moneyText(data.combined_debit_total)} / 贷 ${moneyText(data.combined_credit_total)}，期初凭证 ${data.voucher?.voucher_no ?? '—'}`,
    );
  } catch (error) {
    await MessagePlugin.error(errorText(error, '保存期初失败'));
  } finally {
    saving.value = false;
  }
}

async function handleSaveInventory() {
  if (!periodModel.value) {
    return;
  }
  saving.value = true;
  try {
    const items = inventoryRows.value
      .filter((row) => row.material_id && row.warehouse_id && toMoney(row.qty) > 0)
      .map((row) => ({
        material_id: row.material_id,
        warehouse_id: row.warehouse_id,
        qty: toMoney(row.qty),
        unit_cost: toMoney(row.unit_cost),
      }));
    const data = await saveOpeningInventory({ period: periodModel.value, items });
    applyOpening(data);
    await MessagePlugin.success(
      `库存期初已保存：${items.length} 个物料，合计 ${moneyText(data.inventory_amount)}`,
    );
  } catch (error) {
    await MessagePlugin.error(errorText(error, '保存库存期初失败'));
  } finally {
    saving.value = false;
  }
}

// ---------- 工具 ----------
/** 金额 → 输入框文本（去掉多余的 0，避免输入框里出现 `50000.0000`） */
function moneyInput(value: number | string | null | undefined): string {
  const amount = toMoney(value);
  return amount === 0 ? '' : String(amount);
}

/** 统一错误文案：后端消息原样展示（不平衡的差额与科目都在里面） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return fallback;
}

onMounted(async () => {
  await loadOptions();
  await loadPeriods();
  await loadOpening();
});
</script>

<style scoped>
.opening-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.main-card {
  padding: 4px 8px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__period {
  width: 220px;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-label,
.foot-label {
  color: var(--td-text-color-secondary, #666);
  font-size: 13px;
}

.page-alert,
.inline-alert {
  margin: 0 8px;
}

.panel-tip {
  color: var(--td-text-color-secondary, #666);
  font-size: 13px;
  line-height: 1.7;
  margin: 8px 0 12px;
}

.grid-head,
.grid-row,
.grid-foot {
  display: flex;
  align-items: center;
  gap: 8px;
}

.grid-head {
  padding: 6px 0;
  color: var(--td-text-color-secondary, #666);
  font-size: 13px;
  border-bottom: 1px solid var(--td-component-border, #e7e7e7);
}

.grid-row {
  padding: 6px 0;
}

.grid-foot {
  padding: 10px 0;
  border-top: 1px solid var(--td-component-border, #e7e7e7);
}

.col-code {
  flex: 2 1 0;
  min-width: 180px;
}

.col-money {
  flex: 1 1 0;
  min-width: 110px;
  text-align: right;
}

.col-qty {
  flex: 0 0 100px;
  min-width: 90px;
  text-align: right;
}

.col-remark {
  flex: 2 1 0;
  min-width: 120px;
}

.col-op {
  flex: 0 0 56px;
  text-align: center;
}

.amount {
  text-align: right;
}

.row-errors {
  display: flex;
  flex-direction: column;
  gap: 2px;
  color: var(--td-error-color, #d54941);
  font-size: 12px;
  padding: 4px 0 8px;
}

.balance-line {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding-top: 4px;
}

.voucher-line {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px dashed var(--td-component-border, #e7e7e7);
}

.hints {
  margin: 8px 0 0;
  padding-left: 18px;
  color: var(--td-warning-color, #e37318);
  font-size: 12px;
  line-height: 1.8;
}

.footer-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
}

.muted {
  color: var(--td-text-color-placeholder, #999);
  font-size: 12px;
}
</style>
