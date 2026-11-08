<template>
  <div class="prt-page">
    <t-card :bordered="false">
      <!-- 筛选 -->
      <t-form :data="query" layout="inline" label-width="80px" class="prt-filter">
        <t-form-item label="状态">
          <t-select v-model="query.status" :options="statusFilters" clearable placeholder="全部" />
        </t-form-item>
        <t-form-item label="关键字">
          <t-input v-model="query.keyword" placeholder="退货单号 / 入库单号 / 供应商 / 原因" clearable />
        </t-form-item>
        <t-form-item label="退货日期">
          <t-date-range-picker v-model="dateRange" clearable @change="onDateChange" />
        </t-form-item>
        <t-form-item>
          <t-space>
            <t-button theme="primary" data-testid="btn-prt-search" @click="load(1)">查询</t-button>
            <t-button variant="outline" data-testid="btn-prt-reset" @click="reset">重置</t-button>
            <t-button theme="success" data-testid="btn-prt-create" @click="openCreate">新建退货单</t-button>
          </t-space>
        </t-form-item>
      </t-form>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        data-testid="table-purchase-return"
        @page-change="onPageChange"
      >
        <template #status="{ row }">
          <t-tag :theme="statusTheme(row.status)" variant="light">{{ row.status_label }}</t-tag>
        </template>
        <template #total_with_tax="{ row }">{{ decimalText(row.total_with_tax) }}</template>
        <template #links="{ row }">
          <t-space size="small">
            <span v-if="row.payable_no">应付 {{ row.payable_no }}</span>
            <span v-if="row.supplier_receivable_no">应退 {{ row.supplier_receivable_no }}</span>
            <span v-if="row.voucher_no">凭证 {{ row.voucher_no }}</span>
          </t-space>
        </template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link theme="primary" data-testid="btn-prt-detail" @click="openDetail(row.id)">详情</t-link>
            <t-link
              v-if="row.status === 'DRAFT'"
              theme="primary"
              data-testid="btn-prt-submit"
              @click="onSubmit(row)"
            >提交审批</t-link>
            <t-link
              v-if="row.status === 'APPROVED'"
              theme="success"
              data-testid="btn-prt-confirm"
              @click="onConfirm(row)"
            >确认退货</t-link>
            <t-link
              v-if="row.status === 'DRAFT' || row.status === 'APPROVED'"
              theme="danger"
              data-testid="btn-prt-cancel"
              @click="onCancel(row)"
            >作废</t-link>
          </t-space>
        </template>
      </t-table>
    </t-card>

    <!-- 详情 -->
    <t-drawer v-model:visible="detailVisible" header="采购退货详情" size="900px" :footer="false">
      <template v-if="detail">
        <t-descriptions :column="3" bordered size="small">
          <t-descriptions-item label="退货单号">{{ detail.purchase_return.return_no }}</t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag :theme="statusTheme(detail.purchase_return.status)" variant="light">
              {{ detail.purchase_return.status_label }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="退货日期">{{ detail.purchase_return.return_date }}</t-descriptions-item>
          <t-descriptions-item label="来源入库单">{{ detail.purchase_return.stock_in_no }}</t-descriptions-item>
          <t-descriptions-item label="采购订单">{{ detail.purchase_return.purchase_order_no || '-' }}</t-descriptions-item>
          <t-descriptions-item label="供应商">{{ detail.purchase_return.supplier_name || '-' }}</t-descriptions-item>
          <t-descriptions-item label="数量合计">{{ detail.purchase_return.total_qty }}</t-descriptions-item>
          <t-descriptions-item label="不含税">{{ decimalText(detail.purchase_return.total_amount) }}</t-descriptions-item>
          <t-descriptions-item label="价税合计">{{ decimalText(detail.purchase_return.total_with_tax) }}</t-descriptions-item>
          <t-descriptions-item label="出库单">{{ detail.purchase_return.stock_out_no || '-' }}</t-descriptions-item>
          <t-descriptions-item label="应付单">{{ detail.purchase_return.payable_no || '-' }}</t-descriptions-item>
          <t-descriptions-item label="凭证">{{ detail.purchase_return.voucher_no || '-' }}</t-descriptions-item>
          <t-descriptions-item label="供应商应退">{{ detail.purchase_return.supplier_receivable_no || '-' }}</t-descriptions-item>
          <t-descriptions-item label="退货原因" :span="2">{{ detail.purchase_return.reason }}</t-descriptions-item>
          <t-descriptions-item label="备注" :span="3">{{ detail.purchase_return.remark || '-' }}</t-descriptions-item>
        </t-descriptions>

        <h4 class="prt-section">退货明细</h4>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="itemColumns"
          data-testid="table-prt-items"
        >
          <template #qty="{ row }">{{ row.qty }}</template>
          <template #price="{ row }">{{ decimalText(row.price, 4) }}</template>
          <template #price_incl="{ row }">{{ decimalText(row.price_incl, 4) }}</template>
          <template #with_tax_amount="{ row }">{{ decimalText(row.with_tax_amount) }}</template>
        </t-table>

        <!--
          ATT-09：退货单/签收凭证的上传入口 —— 提交审批前必须已有附件（后端 ATT-06 闸门，
          规则 `PURCHASE_RETURN` + `SUBMIT`）。没有上传入口的页面，必传规则永远无法满足。
        -->
        <h4 class="prt-section">附件（退货单 / 签收凭证）</h4>
        <DocAttachmentPanel
          :doc-type="DOC_KIND.PURCHASE_RETURN"
          :doc-id="detail.purchase_return.id"
          :default-role="DEFAULT_ROLE_BY_DOC.PURCHASE_RETURN"
          testid-prefix="prt-attach"
          @changed="refreshAttachGate()"
        />

        <div class="prt-actions" data-testid="prt-detail-actions">
          <template v-if="detail.purchase_return.status === 'DRAFT'">
            <!-- 缺附件时按钮 disabled，外面套 tooltip（disabled 的 t-button 收不到 hover） -->
            <t-tooltip :content="submitGateTip" :disabled="!submitGateTip" placement="top-left">
              <span class="op-wrap">
                <t-button
                  theme="primary"
                  :loading="submitting"
                  :disabled="!hasSubmitPermission || !attachGate.canProceed"
                  data-testid="prt-detail-submit"
                  @click="submitFromDetail"
                >
                  提交审批
                </t-button>
              </span>
            </t-tooltip>
            <AttachmentGateHint :hint="attachGate.hint" testid="prt-detail-gate-hint" />
          </template>
          <t-button variant="outline" data-testid="prt-detail-close" @click="detailVisible = false">关闭</t-button>
        </div>
      </template>
    </t-drawer>

    <!-- 新建 -->
    <t-dialog
      v-model:visible="createVisible"
      header="新建采购退货单"
      width="980px"
      :confirm-btn="{ content: '保存草稿', loading: saving }"
      @confirm="saveCreate"
    >
      <t-form label-width="100px">
        <t-form-item label="来源入库单" required>
          <t-select
            v-model="form.stock_in_id"
            :options="stockInOptions"
            :loading="stockInLoading"
            filterable
            placeholder="选一张已过账（POSTED）的采购入库单"
            data-testid="select-prt-stock-in"
            @change="(value: unknown) => loadSource(String(value))"
          />
        </t-form-item>
        <t-form-item label="退货日期">
          <t-date-picker v-model="form.return_date" clearable data-testid="input-prt-date" />
        </t-form-item>
        <t-form-item label="退货原因" required>
          <t-input v-model="form.reason" placeholder="质量 / 短交 / 错发 / 价格 —— 审计要能回答为什么退" data-testid="input-prt-reason" />
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="form.remark" placeholder="可选" />
        </t-form-item>
      </t-form>

      <template v-if="source">
        <t-alert theme="info" :message="`入库单 ${source.stock_in_no}：供应商 ${source.supplier_name || '-'}；单价与税率取原入库快照（不许改）`" />
        <t-table
          row-key="stock_in_item_id"
          size="small"
          :data="source.items"
          :columns="sourceColumns"
          data-testid="table-prt-source"
          class="prt-source"
        >
          <template #received_qty="{ row }">{{ row.received_qty }}</template>
          <template #returned_qty="{ row }">{{ row.returned_qty }}</template>
          <template #returnable_qty="{ row }">{{ row.returnable_qty }}</template>
          <template #price_incl="{ row }">{{ decimalText(row.price_incl, 4) }}</template>
          <template #input="{ row }">
            <t-input-number
              v-model="qtyMap[row.stock_in_item_id]"
              :min="0"
              :max="Number(row.returnable_qty)"
              :decimal-places="4"
              theme="normal"
              size="small"
            />
          </template>
        </t-table>
        <div class="prt-total">
          合计价税金额（后端按原入库单价计算）：<b>{{ decimalText(estimatedTotal) }}</b>
        </div>
      </template>
    </t-dialog>

    <!-- 需要填原因的动作（确认 / 作废） -->
    <t-dialog
      v-model:visible="askVisible"
      :header="askTitle"
      width="520px"
      :confirm-btn="{ content: '确定' }"
      cancel-btn="取消"
      @confirm="confirmAsk"
      @cancel="settleAsk(null)"
      @close="settleAsk(null)"
    >
      <p class="prt-ask-hint">{{ askHint }}</p>
      <t-textarea
        v-model="askText"
        :placeholder="askRequired ? '必填：为什么' : '可选：补充说明'"
        :autosize="{ minRows: 2, maxRows: 4 }"
        data-testid="input-prt-ask-reason"
      />
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 采购退货（P2-03）：列表 + 新建（选入库单带明细）+ 提交审批 + 确认退货 + 作废。
 *
 * 与后端 `purchase_return_handler.rs` 一一对应；金额一律用 `decimalText()` 展示
 *（后端 `Decimal` 在 JSON 里是字符串），建单只提交数量，单价由后端从入库快照取。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { DateRangeValue } from 'tdesign-vue-next';
import { fetchStockInPage } from '@/api/business/erp';
import { checkDocAttachments, DEFAULT_ROLE_BY_DOC, type DocType } from '@/api/business/erp/attachment';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { useUserStore } from '@/stores/user';
import AttachmentGateHint from '@/views/business/erp/shared/AttachmentGateHint.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import { gateButtonTip } from '@/views/business/erp/shared/attachment-gate-pure';
import { useAttachmentGate } from '@/views/business/erp/shared/use-attachment-gate';
import {
  cancelPurchaseReturn,
  confirmPurchaseReturn,
  createPurchaseReturn,
  decimalText,
  getPurchaseReturn,
  getReturnableSource,
  pagePurchaseReturn,
  STATUS_OPTIONS,
  statusTheme,
  submitPurchaseReturn,
  type ReturnDetail,
  type ReturnPageQuery,
  type ReturnRow,
  type ReturnableSource,
} from '@/api/business/erp/purchase-return';

const loading = ref(false);
const rows = ref<ReturnRow[]>([]);
const total = ref(0);
const query = reactive<ReturnPageQuery>({ page: 1, page_size: 20, status: '', keyword: '' });
const dateRange = ref<DateRangeValue>();

const pagination = computed(() => ({
  current: query.page,
  pageSize: query.page_size,
  total: total.value,
  showJumper: true,
  pageSizeOptions: [10, 20, 50],
}));

const statusFilters = [{ value: '', label: '全部' }, ...STATUS_OPTIONS];

const columns = [
  { colKey: 'return_no', title: '退货单号', width: 190 },
  { colKey: 'stock_in_no', title: '来源入库单', width: 175 },
  { colKey: 'supplier_name', title: '供应商', width: 150, ellipsis: true },
  { colKey: 'return_date', title: '退货日期', width: 115 },
  { colKey: 'total_qty', title: '数量', width: 100, align: 'right' as const },
  { colKey: 'total_with_tax', title: '价税合计', width: 120, align: 'right' as const },
  { colKey: 'links', title: '账务单据', width: 260, ellipsis: true },
  { colKey: 'status', title: '状态', width: 95 },
  { colKey: 'op', title: '操作', width: 200, fixed: 'right' as const },
];

const itemColumns = [
  { colKey: 'line_no', title: '行号', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 130 },
  { colKey: 'material_name', title: '物料名称', ellipsis: true },
  { colKey: 'unit_name', title: '单位', width: 70 },
  { colKey: 'qty', title: '退货数量', width: 100, align: 'right' as const },
  { colKey: 'price', title: '不含税单价', width: 115, align: 'right' as const },
  { colKey: 'price_incl', title: '含税单价', width: 110, align: 'right' as const },
  { colKey: 'with_tax_amount', title: '价税金额', width: 115, align: 'right' as const },
  { colKey: 'received_qty', title: '原入库', width: 95, align: 'right' as const },
  { colKey: 'returned_qty', title: '已退(含本单)', width: 110, align: 'right' as const },
];

const sourceColumns = [
  { colKey: 'line_no', title: '行号', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 130 },
  { colKey: 'material_name', title: '物料名称', ellipsis: true },
  { colKey: 'unit_name', title: '单位', width: 70 },
  { colKey: 'received_qty', title: '入库数量', width: 100, align: 'right' as const },
  { colKey: 'returned_qty', title: '已退', width: 90, align: 'right' as const },
  { colKey: 'returnable_qty', title: '可退', width: 90, align: 'right' as const },
  { colKey: 'price_incl', title: '含税单价', width: 110, align: 'right' as const },
  { colKey: 'input', title: '本次退货', width: 140 },
];

// ==================== 列表 ====================

async function load(page = query.page ?? 1) {
  loading.value = true;
  try {
    query.page = page;
    const data = await pagePurchaseReturn({ ...query });
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
  } catch (error) {
    MessagePlugin.error(String(error));
  } finally {
    loading.value = false;
  }
}

function reset() {
  query.status = '';
  query.keyword = '';
  query.date_from = undefined;
  query.date_to = undefined;
  dateRange.value = undefined;
  load(1);
}

function onDateChange(value: unknown) {
  const range = Array.isArray(value) ? (value as Array<string | number | Date>) : [];
  query.date_from = range[0] ? String(range[0]) : undefined;
  query.date_to = range[1] ? String(range[1]) : undefined;
}

function onPageChange(info: { current: number; pageSize: number }) {
  query.page_size = info.pageSize;
  load(info.current);
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detail = ref<ReturnDetail | null>(null);

async function openDetail(id: string) {
  try {
    detail.value = await getPurchaseReturn(id);
    detailVisible.value = true;
  } catch (error) {
    MessagePlugin.error(String(error));
  }
}

// ==================== 附件必传闸门（ATT-09） ====================

/**
 * 采购退货提交审批前必须已有退货单 / 签收凭证
 * （规则表 `PURCHASE_RETURN` + `SUBMIT`，后端 `purchase_return_handler::submit` 同一条闸门）。
 *
 * 只有草稿要查：已提交 / 已批准 / 已确认 / 已作废不会再走 `→ SUBMITTED`。
 */
const attachDocType = ref<DocType>('PURCHASE_RETURN');
const attachDocId = computed(() =>
  detail.value?.purchase_return.status === 'DRAFT' ? detail.value.purchase_return.id : null,
);

const { gate: attachGate, refresh: refreshAttachGate } = useAttachmentGate({
  docType: attachDocType,
  docId: attachDocId,
  at: 'SUBMIT',
});

/** 提交权限码（后端 `purchase_return_handler::submit` 的 `#[nik::permission]`） */
const SUBMIT_PERMISSION = 'business:erp:purchase-return:submit';
const userStore = useUserStore();
const hasSubmitPermission = computed(() => userStore.hasPermission(SUBMIT_PERMISSION));
const submitTip = computed(() => (hasSubmitPermission.value ? '' : `缺少权限 ${SUBMIT_PERMISSION}`));

/** 提交按钮 tooltip：权限优先，其次附件缺口（定序在 `gateButtonTip` 里） */
const submitGateTip = computed(() => gateButtonTip(attachGate.value, submitTip.value));

// ==================== 新建 ====================

const createVisible = ref(false);
const saving = ref(false);
const stockInLoading = ref(false);
const stockInOptions = ref<{ value: string; label: string }[]>([]);
const source = ref<ReturnableSource | null>(null);
const qtyMap = reactive<Record<string, number>>({});
const form = reactive<{
  stock_in_id: string;
  return_date: string;
  reason: string;
  remark: string;
}>({ stock_in_id: '', return_date: '', reason: '', remark: '' });

async function openCreate() {
  form.stock_in_id = '';
  form.return_date = new Date().toISOString().slice(0, 10);
  form.reason = '';
  form.remark = '';
  source.value = null;
  Object.keys(qtyMap).forEach((key) => delete qtyMap[key]);
  createVisible.value = true;
  stockInLoading.value = true;
  try {
    // 只列已过账的采购入库（草稿 / 作废的没有可退数量）
    const data = await fetchStockInPage({ status: 'POSTED', page: 1, page_size: 100 });
    stockInOptions.value = (data.list ?? [])
      .filter((row) => row.in_type === 'PURCHASE_IN' || row.in_type === 'PURCHASE')
      .map((row) => ({
        value: row.id,
        label: `${row.in_no}${row.supplier_name ? ` / ${row.supplier_name}` : ''}`,
      }));
  } catch (error) {
    MessagePlugin.error(String(error));
  } finally {
    stockInLoading.value = false;
  }
}

async function loadSource(stockInId: string) {
  if (!stockInId) return;
  try {
    source.value = await getReturnableSource(stockInId);
    Object.keys(qtyMap).forEach((key) => delete qtyMap[key]);
    for (const item of source.value.items) {
      qtyMap[item.stock_in_item_id] = 0;
    }
  } catch (error) {
    MessagePlugin.error(String(error));
    source.value = null;
  }
}

const estimatedTotal = computed(() => {
  if (!source.value) return 0;
  return source.value.items.reduce((sum, item) => {
    const qty = Number(qtyMap[item.stock_in_item_id] ?? 0);
    return sum + qty * Number(item.price_incl);
  }, 0);
});

async function saveCreate() {
  if (!form.stock_in_id) {
    MessagePlugin.warning('请先选择来源入库单');
    return;
  }
  if (!form.reason.trim()) {
    MessagePlugin.warning('退货原因必填');
    return;
  }
  const items = Object.entries(qtyMap)
    .filter(([, qty]) => Number(qty) > 0)
    .map(([stock_in_item_id, qty]) => ({ stock_in_item_id, qty: Number(qty) }));
  if (items.length === 0) {
    MessagePlugin.warning('至少填一行的退货数量');
    return;
  }
  saving.value = true;
  try {
    await createPurchaseReturn({
      stock_in_id: form.stock_in_id,
      return_date: form.return_date || undefined,
      reason: form.reason,
      remark: form.remark || undefined,
      items,
    });
    MessagePlugin.success('退货单已保存（草稿），请提交审批');
    createVisible.value = false;
    load(1);
  } catch (error) {
    MessagePlugin.error(String(error));
  } finally {
    saving.value = false;
  }
}

// ==================== 动作（需要填原因的动作走同一个弹窗） ====================

const askVisible = ref(false);
const askTitle = ref('');
const askHint = ref('');
const askRequired = ref(false);
const askText = ref('');
let pendingResolve: ((value: string | null) => void) | null = null;

function askReason(title: string, hint: string, required: boolean): Promise<string | null> {
  askTitle.value = title;
  askHint.value = hint;
  askRequired.value = required;
  askText.value = '';
  askVisible.value = true;
  return new Promise((resolve) => {
    pendingResolve = resolve;
  });
}

function settleAsk(value: string | null) {
  askVisible.value = false;
  if (pendingResolve) {
    pendingResolve(value);
    pendingResolve = null;
  }
}

function confirmAsk() {
  if (askRequired.value && !askText.value.trim()) {
    MessagePlugin.warning('这里必须填写原因');
    return;
  }
  // 空串 = 「确认但没写原因」（原因可选的动作允许）；null = 弹窗被关掉（不执行）
  settleAsk(askText.value.trim());
}

const submitting = ref(false);

/**
 * 提交审批前的附件闸门（ATT-09）。
 *
 * 列表行没有「按钮旁红字」的位置，所以缺什么就直说、并**打开详情**（详情里有上传面板
 * 和那个按钮的红字，验收①②都落在那里）。查询失败不阻断（同 `use-attachment-gate`
 * 的降级口径），交给后端权威判定。
 */
async function passAttachmentGate(docId: string): Promise<boolean> {
  try {
    const check = await checkDocAttachments('PURCHASE_RETURN', docId, 'SUBMIT');
    if (check.can_proceed) {
      return true;
    }
    MessagePlugin.warning(
      `${check.at_label || '提交'}前需要补齐：${check.hint ?? '缺少必传附件（退货单或签收凭证）'}`,
    );
    await openDetail(docId);
    return false;
  } catch (error) {
    console.warn('[purchase-return] 提交前必传校验查询失败，降级为直接提交（后端仍会拦）', error);
    return true;
  }
}

/** 提交审批（列表行）：先查闸门再提交 */
async function onSubmit(row: ReturnRow): Promise<void> {
  if (!(await passAttachmentGate(row.id))) {
    return;
  }
  await runSubmit(row.id);
}

/** 提交审批（详情抽屉）：按钮已受权限 ∪ 闸门约束，这里仍兜一次底 */
async function submitFromDetail(): Promise<void> {
  const target = detail.value?.purchase_return;
  if (!target) {
    return;
  }
  if (!hasSubmitPermission.value) {
    MessagePlugin.warning(submitTip.value);
    return;
  }
  if (!attachGate.value.canProceed) {
    MessagePlugin.warning(submitGateTip.value || '还缺必传附件，请先在上方附件面板上传');
    return;
  }
  await runSubmit(target.id);
}

/** 真正提交（列表 / 详情共用一条路径），成功后列表与（打开着的）详情都刷新 */
async function runSubmit(id: string): Promise<void> {
  if (submitting.value) {
    return;
  }
  submitting.value = true;
  try {
    await submitPurchaseReturn(id);
    MessagePlugin.success('已提交审批');
    await load();
    if (detailVisible.value) {
      detail.value = await getPurchaseReturn(id);
    }
  } catch (error) {
    MessagePlugin.error(String(error));
  } finally {
    submitting.value = false;
  }
}

async function onConfirm(row: ReturnRow) {
  const reason = await askReason(
    `确认退货 ${row.return_no}`,
    '确认后：出库回冲 + 冲减应付（已付款部分转供应商应退款）+ 生成冲销凭证，账务不可撤销。',
    false,
  );
  if (reason === null) return;
  try {
    const result = await confirmPurchaseReturn(row.id, reason || undefined);
    MessagePlugin.success(
      `已确认：出库单 ${result.purchase_return.stock_out_no ?? ''} / 凭证 ${result.purchase_return.voucher_no ?? ''}`,
    );
    load();
  } catch (error) {
    MessagePlugin.error(String(error));
  }
}

async function onCancel(row: ReturnRow) {
  const required = row.status === 'APPROVED';
  const reason = await askReason(
    `作废 ${row.return_no}`,
    required ? '已批准的单据作废必须写原因（财务已按它准备冲账）。' : '草稿作废不需要原因。',
    required,
  );
  if (reason === null) return;
  try {
    await cancelPurchaseReturn(row.id, reason || undefined);
    MessagePlugin.success('已作废');
    load();
  } catch (error) {
    MessagePlugin.error(String(error));
  }
}

onMounted(() => load(1));
</script>

<style scoped>
.prt-page {
  padding: 16px;
}
.prt-filter {
  margin-bottom: 12px;
}
.prt-section {
  margin: 18px 0 8px;
}
.prt-source {
  margin-top: 12px;
}
.prt-total {
  margin-top: 10px;
  text-align: right;
  color: var(--td-text-color-secondary);
}
/* ATT-09：详情底部动作条（提交审批 + 红字 + 关闭） */
.prt-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-border);
}
/* disabled 的 t-button 收不到 hover：tooltip 必须包一层普通元素才能对灰按钮生效 */
.op-wrap {
  display: inline-flex;
}
</style>
