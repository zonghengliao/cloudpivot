<!--
  销售报价单（M3-06 前端；后端 M3-02/03/04）。

  # 这一页做四件事

  1. **列表**：按 单号 / 状态 / 客户 / 有效期 筛，服务端分页（`GET /api/business/erp/quotation/page`）；
  2. **详情**：单头 + 明细 + 合计 + 审批摘要 + 转单回链（`GET /quotation/{id}`）；
  3. **转单**：ACCEPTED 一键转销售订单（带行，幂等；`POST /quotation/{id}/convert`）；
  4. **建单/改单/状态流转/送审/删单**：把 5 个权限码对应的动作都落到实处。

  # 三条口径（写错就是错账或必然 409）

  - **发出（DRAFT→SENT）不是改状态**：它挂在审批闸后面，走 `submit-approval`；
    直接 `PATCH status=SENT` 后端一律 409 + `66088`。所以列表/详情里没有「发出」按钮，
    只有「提交审批」与「审批通过后发出」两条路，判断依据是详情返回的 `approval.status`；
  - **审批不是状态**：`status` 仍是 `DRAFT` 时也可能在审批中 —— 待批徽标只读 `approval`；
  - **过期是现算事实**：`expired` 由后端按 CST 今天算，不是状态值，列表要单独打标记。

  # 权限（V0433 / V0434 的 5 个码，逐字对应 `quotation_handler::QUOTATION_PERMISSIONS`）

  `list`（列表/详情）· `create` · `update`（只有 DRAFT）· `delete`（DRAFT/CANCELLED）·
  `status`（状态流转 + 送审）· `convert`（转销售订单）。
-->
<template>
  <div class="qt-page">
    <!-- ==================== ① 列表 ==================== -->
    <ListPage
      ref="listRef"
      title="报价单"
      testid="qt"
      table-testid="qt-table"
      btn-prefix="btn-qt"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      :actions="actions"
    >
      <template #toolbar>
        <t-tooltip
          :content="
            canCreate
              ? '新建报价单：取号 QT-yyyyMMdd-000001；客户须 ACTIVE、至少 1 行明细、有效期是必填条款'
              : '当前账号没有 business:erp:quotation:create 权限'
          "
          placement="top-right"
        >
          <span class="qt-gate">
            <t-button theme="primary" :disabled="!canCreate" data-testid="btn-qt-new" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新建报价单
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #quote_no="{ row }">
        <div class="qt-no-cell">
          <b class="qt-mono">{{ row.quote_no }}</b>
          <span class="qt-sub">报价 {{ row.quote_date }}</span>
        </div>
      </template>

      <template #customer="{ row }">
        <div class="qt-no-cell">
          <span>{{ row.customer_name || '客户已删除' }}</span>
          <span class="qt-sub qt-mono">{{ row.customer_code || '—' }}</span>
        </div>
      </template>

      <template #valid_until="{ row }">
        <div class="qt-no-cell">
          <span>{{ row.valid_until }}</span>
          <t-tag v-if="row.expired" size="small" theme="danger" variant="light" data-testid="qt-expired-tag">
            已过期
          </t-tag>
        </div>
      </template>

      <template #total_with_tax="{ row }">
        <span class="qt-mono qt-amount">{{ formatMoney(row.total_with_tax) }}</span>
      </template>

      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="statusTheme(row.status)">
          {{ row.status_label || statusLabel(row.status) }}
        </t-tag>
      </template>

      <template #convert="{ row }">
        <div class="qt-no-cell">
          <t-tag v-if="row.converted_so_id" size="small" variant="light" theme="success">已转单</t-tag>
          <span v-else-if="row.status === 'ACCEPTED'" class="qt-sub">可转单</span>
          <span v-else class="qt-sub">—</span>
          <span v-if="row.converted_so_id" class="qt-sub qt-mono">
            {{ String(row.converted_so_id).slice(0, 8) }}…
          </span>
        </div>
      </template>

      <template #empty="{ filtered }">
        <div class="qt-empty" data-testid="qt-empty">
          <t-icon name="search" class="qt-empty__icon" />
          <p class="qt-empty__title">{{ emptyListText(filtered) }}</p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="1180px"
      :header="detailHeader"
      :footer="false"
      data-testid="qt-detail-drawer"
    >
      <div v-if="detail" class="qt-detail">
        <t-descriptions :column="3" size="small" data-testid="qt-detail-head">
          <t-descriptions-item label="报价单号">
            <span class="qt-mono">{{ detail.quote_no }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="客户">
            {{ detail.customer_name || '（客户已删除）' }}
            <span class="qt-sub qt-mono">{{ detail.customer_code || '' }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag size="small" variant="light" :theme="statusTheme(detail.status)">
              {{ detail.status_label }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="报价日期">{{ detail.quote_date }}</t-descriptions-item>
          <t-descriptions-item label="有效期至">
            {{ detail.valid_until }}
            <t-tag v-if="detail.expired" size="small" theme="danger" variant="light">已过期</t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="价税合计">
            <span class="qt-mono">{{ formatMoney(detail.total_with_tax) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="审批">
            <t-tag v-if="approvalBadge" size="small" variant="light" :theme="approvalBadge.theme" data-testid="qt-approval-tag">
              {{ approvalBadge.text }}
            </t-tag>
            <span v-else class="qt-sub">从未提交审批</span>
          </t-descriptions-item>
          <t-descriptions-item label="销售订单">
            <span class="qt-mono">{{ convertedText }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="更新时间">{{ shortTime(detail.updated_at) }}</t-descriptions-item>
          <t-descriptions-item label="备注" :span="3">{{ detail.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <!-- 审批已通过但还没发出：这是唯一「可以点发出」的时机 -->
        <t-alert
          v-if="detail.status === 'DRAFT' && detail.approval?.status === 'APPROVED'"
          theme="success"
          :message="issueHintOf(detail)"
          data-testid="qt-issue-ready"
        />

        <!-- ==================== 动作行（按钮不是常驻乱摆：不可用就置灰 + tooltip 说明） -->
        <div class="qt-actions">
          <t-tooltip
            :content="detail.editable ? '改草稿（明细整单替换）' : '只有草稿能改：已发出的报价要改价请开新单'"
            placement="top"
          >
            <span class="qt-gate">
              <t-button
                variant="outline"
                :disabled="!canUpdate || !detail.editable"
                data-testid="btn-qt-edit"
                @click="openEdit(detail)"
              >
                编辑
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip :content="submitState.hint" placement="top">
            <span class="qt-gate">
              <t-button
                theme="primary"
                variant="outline"
                :loading="submittingApproval"
                :disabled="!canStatus || !submitState.enabled || submittingApproval"
                data-testid="btn-qt-submit-approval"
                @click="submitApproval"
              >
                提交审批
              </t-button>
            </span>
          </t-tooltip>

          <!-- 审批通过后的「发出」：这一步不需要再审，直接走 PATCH status=SENT -->
          <t-tooltip v-if="detail.approval?.status === 'APPROVED' && detail.status === 'DRAFT'" :content="issueHintOf(detail)" placement="top">
            <span class="qt-gate">
              <t-button
                theme="primary"
                :loading="issuing"
                :disabled="!canStatus || issuing"
                data-testid="btn-qt-issue"
                @click="issueQuotation"
              >
                发出
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip v-for="action in statusActions" :key="action.target" :content="statusActionTip(action)" placement="top">
            <span class="qt-gate">
              <t-button
                variant="outline"
                :theme="action.theme === 'default' ? undefined : action.theme"
                :disabled="!canStatus"
                :data-testid="`btn-qt-status-${action.target}`"
                @click="openStatusDialog(action)"
              >
                {{ action.label }}
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip :content="convertState.hint" placement="top">
            <span class="qt-gate">
              <t-button
                theme="success"
                :loading="converting"
                :disabled="!canConvert || !convertState.enabled || converting"
                data-testid="btn-qt-convert"
                @click="openConvert(detail)"
              >
                转销售订单
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            :content="detail.deletable ? '软删（DRAFT / CANCELLED 可删）' : '只有草稿 / 已作废能删：发出去的报价要留痕，请走作废'"
            placement="top"
          >
            <span class="qt-gate">
              <t-button
                theme="danger"
                variant="outline"
                :disabled="!canDelete || !detail.deletable"
                data-testid="btn-qt-delete"
                @click="confirmDelete(detail)"
              >
                删除
              </t-button>
            </span>
          </t-tooltip>

          <!-- M3-07：报价单 PDF（复用 DocPdfButton：带 token 取 blob、用后端给的中文名存盘） -->
          <DocPdfButton
            :path="`/api/business/erp/quotation/${detail.id}/pdf`"
            :filename="`报价单-${detail.quote_no}.pdf`"
            testid="qt-detail-pdf"
          />
        </div>

        <!-- ==================== 明细 + 合计 ==================== -->
        <div class="qt-section-title">
          报价明细
          <span class="qt-sub">金额由后端按 单价 × 数量、税额按 金额 × 税率 现算（前端不提交金额）</span>
        </div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="itemColumns"
          data-testid="qt-item-table"
        />
        <div class="qt-totals" data-testid="qt-detail-totals">
          <span>不含税合计 <b class="qt-mono">{{ formatMoney(detail.total_amount) }}</b></span>
          <span>税额合计 <b class="qt-mono">{{ formatMoney(detail.total_tax) }}</b></span>
          <span>价税合计 <b class="qt-mono">{{ formatMoney(detail.total_with_tax) }}</b></span>
        </div>
      </div>
      <t-loading v-else :loading="detailLoading" size="small" />
    </t-drawer>

    <!-- ==================== ③ 新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? `编辑报价单 ${formQuoteNo}` : '新建报价单'"
      :width="1080"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: formSaving, disabled: formSaving }"
      :cancel-btn="{ disabled: formSaving }"
      :close-on-overlay-click="false"
      data-testid="qt-form-dialog"
      @confirm="submitForm"
    >
      <div class="qt-form-body">
        <t-form label-width="100px" class="qt-form">
          <div class="qt-form-grid">
            <t-form-item label="客户" required-mark>
              <t-select
                v-model="form.customer_id"
                :options="customerOptions"
                filterable
                placeholder="只列 ACTIVE 客户（冻结客户不能开新报价）"
                data-testid="qt-form-customer"
              />
            </t-form-item>
            <t-form-item label="报价日期">
              <t-date-picker
                v-model="form.quote_date"
                value-type="YYYY-MM-DD"
                placeholder="默认今天（北京时间）"
                clearable
                data-testid="qt-form-quote-date"
              />
            </t-form-item>
            <t-form-item label="有效期至" required-mark>
              <t-date-picker
                v-model="form.valid_until"
                value-type="YYYY-MM-DD"
                placeholder="必填：报价给多少天有效是商业条款"
                clearable
                data-testid="qt-form-valid-until"
              />
            </t-form-item>
          </div>
          <t-form-item label="备注">
            <t-input
              v-model="form.remark"
              :maxlength="MAX_REMARK"
              placeholder="可空，例：付款条件 30 天、含税价、运费另计"
              data-testid="qt-form-remark"
            />
          </t-form-item>
        </t-form>

        <div class="qt-line-editor" data-testid="qt-form-items">
          <div class="qt-line-head">
            <span>#</span>
            <span>物料</span>
            <span>数量</span>
            <span>不含税单价</span>
            <span>税率%</span>
            <span>行金额</span>
            <span>备注</span>
            <span></span>
          </div>
          <div v-for="(line, index) in form.items" :key="line.key" class="qt-line-row">
            <span class="qt-mono qt-line-no">{{ index + 1 }}</span>
            <t-select
              v-model="line.material_id"
              :options="materialOptions"
              filterable
              placeholder="选择物料"
              :data-testid="`qt-form-material-${index}`"
            />
            <t-input-number
              v-model="line.qty"
              :min="0"
              :decimal-places="4"
              theme="normal"
              placeholder="数量"
              :data-testid="`qt-form-qty-${index}`"
            />
            <t-input-number
              v-model="line.price"
              :min="0"
              :decimal-places="6"
              theme="normal"
              placeholder="0.000000"
              :data-testid="`qt-form-price-${index}`"
            />
            <t-input-number
              v-model="line.tax_rate"
              :min="0"
              :max="100"
              :decimal-places="2"
              theme="normal"
              placeholder="0"
              :data-testid="`qt-form-tax-${index}`"
            />
            <span class="qt-mono qt-line-amount" :data-testid="`qt-form-amount-${index}`">
              {{ formatMoney(lineAmountsOf(line).withTax) }}
            </span>
            <t-input v-model="line.remark" :maxlength="MAX_LINE_REMARK" placeholder="可空" />
            <t-button
              variant="text"
              theme="danger"
              :disabled="form.items.length <= 1"
              data-testid="btn-qt-remove-line"
              @click="removeLine(index)"
            >
              删除
            </t-button>
          </div>
          <t-button variant="outline" data-testid="btn-qt-add-line" @click="addLine">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
        </div>

        <div class="qt-totals" data-testid="qt-form-totals">
          <span>不含税合计 <b class="qt-mono">{{ formatMoney(formTotals.amount) }}</b></span>
          <span>税额合计 <b class="qt-mono">{{ formatMoney(formTotals.tax) }}</b></span>
          <span>价税合计 <b class="qt-mono">{{ formatMoney(formTotals.withTax) }}</b></span>
          <span class="qt-sub">{{ form.items.length }} 行 / 最多 {{ MAX_LINES }} 行</span>
        </div>

        <t-alert v-if="formError" theme="error" :message="formError" data-testid="qt-form-error" />
      </div>
    </t-dialog>

    <!-- ==================== ④ 状态流转（原因弹窗） ==================== -->
    <t-dialog
      v-model:visible="statusVisible"
      :header="statusDialogHeader"
      :width="560"
      :confirm-btn="{ content: '确认', loading: statusSaving, disabled: statusSaving }"
      :cancel-btn="{ disabled: statusSaving }"
      :close-on-overlay-click="false"
      data-testid="qt-status-dialog"
      @confirm="submitStatus"
    >
      <div class="qt-form-body">
        <t-alert
          v-if="statusTarget"
          theme="info"
          :message="`${detail?.status_label ?? ''} → ${statusLabel(statusTarget)}；这条边会写一条状态日志（谁、什么时候、为什么）`"
        />
        <t-form label-width="80px" class="qt-form">
          <t-form-item :label="statusNeedsReason ? '原因' : '备注'" :required-mark="statusNeedsReason">
            <t-textarea
              v-model="statusReason"
              :maxlength="MAX_REMARK"
              :placeholder="
                statusNeedsReason
                  ? '必填：客户拒绝 / 已发出后作废，事后要能回答「为什么」'
                  : '可空（草稿作废这类边不强制原因）'
              "
              data-testid="qt-status-reason"
            />
          </t-form-item>
        </t-form>
        <t-alert v-if="statusError" theme="error" :message="statusError" data-testid="qt-status-error" />
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 转销售订单 ==================== -->
    <t-dialog
      v-model:visible="convertVisible"
      :header="convertHeader"
      :width="640"
      :confirm-btn="{ content: '生成销售订单', loading: converting, disabled: converting }"
      :cancel-btn="{ disabled: converting }"
      :close-on-overlay-click="false"
      data-testid="qt-convert-dialog"
      @confirm="submitConvert"
    >
      <div class="qt-form-body">
        <t-alert
          theme="info"
          :message="`明细逐行整列搬（物料 / 数量 / 单价 / 税率 / 行金额），金额不重算；新订单状态是草稿。${CONVERT_SOURCE_HINT}`"
        />
        <t-form label-width="100px" class="qt-form">
          <t-form-item label="下单日期">
            <t-date-picker
              v-model="convertForm.order_date"
              value-type="YYYY-MM-DD"
              placeholder="默认今天（不是报价日期）"
              clearable
              data-testid="qt-convert-order-date"
            />
          </t-form-item>
          <t-form-item label="要求交期">
            <t-date-picker
              v-model="convertForm.delivery_date"
              value-type="YYYY-MM-DD"
              placeholder="可空，不能早于下单日期"
              clearable
              data-testid="qt-convert-delivery-date"
            />
          </t-form-item>
          <t-form-item label="销售员">
            <t-select
              v-model="convertForm.salesperson_id"
              :options="salespersonOptions"
              filterable
              clearable
              :placeholder="salespersonOptions.length === 0 ? '拉不到账号列表（需 system:user:list），可留空' : '可空，佣金归属人必须是人明确指定'"
              data-testid="qt-convert-salesperson"
            />
          </t-form-item>
          <t-form-item label="备注">
            <t-input
              v-model="convertForm.remark"
              :maxlength="MAX_REMARK"
              placeholder="可空：来源「由报价单 QT-… 转换」不会被覆盖"
              data-testid="qt-convert-remark"
            />
          </t-form-item>
        </t-form>
        <t-alert v-if="convertError" theme="error" :message="convertError" data-testid="qt-convert-error" />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import DocPdfButton from '@/components/DocPdfButton.vue';
import type { ColumnDef, FilterDef, ListActions, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import {
  createQuotation,
  convertQuotation,
  deleteQuotation,
  fetchQuotationDetail,
  fetchQuotationPage,
  changeQuotationStatus,
  submitQuotationApproval,
  updateQuotation,
  EXPIRED_OPTIONS,
  STATUS_OPTIONS,
  statusLabel,
  statusTheme,
  type ConvertQuotationRes,
  type Quotation,
  type QuotationStatus,
} from '@/api/business/erp/quotation';
import { fetchCustomerPage, fetchMaterialPage, fetchSalesOrderDetail, type Customer, type MaterialRow } from '@/api/business/erp';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  CONVERT_SOURCE_HINT,
  MAX_LINES,
  MAX_LINE_REMARK,
  MAX_REMARK,
  PAGE_SIZE,
  approvalBadgeOf,
  buildConvertPayload,
  buildCreatePayload,
  buildUpdatePayload,
  buildQuery,
  canSubmitApproval,
  convertResultText,
  convertStateOf,
  convertedRefText,
  emptyConvertForm,
  emptyFilter,
  emptyForm,
  emptyListText,
  formFrom,
  issueHintOf,
  lineAmountsOf,
  newLine,
  statusActionsOf,
  totalsOf,
  validateConvertForm,
  validateForm,
  type ConvertFormDraft,
  type FilterState,
  type FormDraft,
  type StatusAction,
} from './quotation-logic';

/** 本页权限码（与 V0433 / V0434 的 `sys_menu.auth_code` 逐字一致） */
const PERM = {
  /** 列表 / 详情 */
  list: 'business:erp:quotation:list',
  /** 新建 */
  create: 'business:erp:quotation:create',
  /** 编辑（只有 DRAFT） */
  update: 'business:erp:quotation:update',
  /** 删除（DRAFT / CANCELLED） */
  delete: 'business:erp:quotation:delete',
  /** 状态流转 + 提交审批 */
  status: 'business:erp:quotation:status',
  /** 转销售订单 */
  convert: 'business:erp:quotation:convert',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canUpdate = computed(() => userStore.hasPermission(PERM.update));
const canDelete = computed(() => userStore.hasPermission(PERM.delete));
const canStatus = computed(() => userStore.hasPermission(PERM.status));
const canConvert = computed(() => userStore.hasPermission(PERM.convert));

/** 错误文案归一（后端 409 / 400 的 msg 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  return error instanceof Error ? error.message : fallback;
}

/** RFC3339 → `yyyy-MM-dd HH:mm`（展示用；非法值原样返回） */
function shortTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const match = /^(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2})/.exec(value);
  return match ? `${match[1]} ${match[2]}` : value;
}

// ==================== 列表 ====================

const filter = reactive<FilterState>(emptyFilter());
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const statusOptions = STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const expiredOptions = EXPIRED_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

const columns: ColumnDef[] = [
  { colKey: 'quote_no', title: '报价单号', width: 190 },
  { colKey: 'customer', title: '客户', minWidth: 200 },
  { colKey: 'valid_until', title: '有效期至', width: 150 },
  { colKey: 'total_with_tax', title: '价税合计', width: 150 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'convert', title: '销售订单', width: 150 },
  { colKey: 'op', title: '操作', width: 250, fixed: 'right' },
];

const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 150 },
  { colKey: 'material_name', title: '物料名称', minWidth: 180 },
  { colKey: 'spec', title: '规格', width: 140 },
  { colKey: 'qty', title: '数量', width: 110 },
  { colKey: 'unit_name', title: '单位', width: 80 },
  { colKey: 'price', title: '不含税单价', width: 130 },
  { colKey: 'amount', title: '不含税金额', width: 140 },
  { colKey: 'tax_rate', title: '税率%', width: 90 },
  { colKey: 'tax_amount', title: '税额', width: 120 },
  { colKey: 'with_tax', title: '含税金额', width: 140 },
  { colKey: 'remark', title: '行备注', minWidth: 160 },
];

/** 筛选项：单号 / 状态 / 客户 / 有效期（后两个是「精确 + 三态」，空值不发） */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'quote_no',
    type: 'keyword',
    placeholder: '报价单号（如 QT-20261117-000001）',
    width: 280,
    testid: 'qt-filter-quote-no',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    testid: 'qt-filter-status',
  },
  {
    key: 'customer_id',
    type: 'select',
    placeholder: '全部客户',
    width: 220,
    filterable: true,
    options: customerOptions.value,
    testid: 'qt-filter-customer',
  },
  {
    key: 'expired',
    type: 'select',
    placeholder: '全部有效期',
    width: 150,
    // 「全部」用 ALL 这个显式值：TDesign 把 `''` 当未选，会显示成 placeholder 里那句「全部有效期」，
    // 看起来一样但重置 / 拼参的行为不同（ALL 才是不传 expired）
    options: expiredOptions,
    searchOnChange: true,
    testid: 'qt-filter-expired',
  },
]);

/** 入参拼装（空值整键缺席、`ALL` 不传 expired） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildQuery(state as FilterState, page, pageSize);

/** 拉一页（「起止」类筛选在这里没有，错误直接由 ListPage 画红条） */
const fetcher: ListFetcher<Quotation> = async (query) => {
  return fetchQuotationPage(query as Parameters<typeof fetchQuotationPage>[0]);
};

/** 行操作：详情 / 编辑 / 转单 / 删除（状态流转在详情里做 —— 那里才有审批摘要可判） */
const actions: ListActions<Quotation> = {
  row: [
    {
      key: 'detail',
      label: '详情',
      testid: 'btn-qt-detail',
      handler: (row) => openDetail(row.id),
    },
    {
      key: 'edit',
      label: '编辑',
      testid: 'btn-qt-edit-row',
      permission: PERM.update,
      whenDenied: 'disable',
      disabled: (row) => !row.editable,
      tip: () => '只有草稿能改：已发出的报价要改价请开新单',
      handler: (row) => openEdit(row),
    },
    {
      key: 'convert',
      label: '转销售订单',
      theme: 'success',
      testid: 'btn-qt-convert-row',
      permission: PERM.convert,
      whenDenied: 'disable',
      disabled: (row) => !convertStateOf(row).enabled,
      tip: (row) => convertStateOf(row).hint,
      handler: (row) => openConvert(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      testid: 'btn-qt-delete-row',
      permission: PERM.delete,
      whenDenied: 'disable',
      disabled: (row) => !row.deletable,
      tip: () => '只有草稿 / 已作废能删：发出去的报价要留痕，请走作废',
      handler: (row) => confirmDelete(row),
    },
  ],
};

/** 刷新列表（默认回当前页；删单后回到第 1 页） */
async function reloadList(page?: number): Promise<void> {
  await listRef.value?.load(page);
}

// ==================== 主数据下拉 ====================

const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);
const salespersons = ref<UserRes[]>([]);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id })),
);
const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_code} ${item.material_name}`, value: item.id })),
);
const salespersonOptions = computed(() =>
  salespersons.value.map((item) => ({ label: `${item.real_name || item.username}（${item.username}）`, value: item.id })),
);

/**
 * 主数据下拉失败不阻塞页面（仍可看列表 / 详情 / 转单；只是没法建单）。
 *
 * 客户只列 `ACTIVE`：冻结客户开新单后端 400（`require_active_customer`）。
 */
async function loadOptions(): Promise<void> {
  const [customerPage, materialPage, userPage] = await Promise.all([
    fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: 200 }).catch(() => null),
    fetchMaterialPage({ status: 1, page: 1, page_size: 200 }).catch(() => null),
    pageUsers({ page: 1, page_size: 200, status: 1 }).catch(() => null),
  ]);
  customers.value = customerPage?.list ?? [];
  materials.value = materialPage?.list ?? [];
  salespersons.value = userPage?.list ?? [];
}

// ==================== 建单 / 改单 ====================

const formVisible = ref(false);
const editingId = ref<string | null>(null);
const formQuoteNo = ref('');
const form = reactive<FormDraft>(emptyForm());
const formError = ref('');
const formSaving = ref(false);

const formTotals = computed(() => totalsOf(form.items));

function openCreate(): void {
  editingId.value = null;
  formQuoteNo.value = '';
  Object.assign(form, emptyForm());
  formError.value = '';
  formVisible.value = true;
}

/** 详情 / 列表行 → 表单（只有 DRAFT 允许；列表行没有明细，缺明细时按 id 重拉详情） */
async function openEdit(row: Quotation): Promise<void> {
  try {
    const body = row.items && row.items.length > 0 ? row : await fetchQuotationDetail(row.id);
    if (!body.editable) {
      MessagePlugin.warning(`${body.quote_no} 当前是「${body.status_label}」：只有草稿能改`);
      return;
    }
    editingId.value = body.id;
    formQuoteNo.value = body.quote_no;
    Object.assign(form, formFrom(body));
    formError.value = '';
    formVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '报价单详情加载失败'));
  }
}

function addLine(): void {
  form.items.push(newLine());
}

function removeLine(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
}

async function submitForm(): Promise<void> {
  const message = validateForm(form);
  if (message) {
    formError.value = message;
    return;
  }
  formSaving.value = true;
  formError.value = '';
  try {
    if (editingId.value) {
      const updated = await updateQuotation(editingId.value, buildUpdatePayload(form));
      formVisible.value = false;
      MessagePlugin.success(`报价单 ${updated.quote_no} 已保存`);
      await reloadList();
      syncDetail(updated);
    } else {
      const created = await createQuotation(buildCreatePayload(form));
      formVisible.value = false;
      MessagePlugin.success(`报价单 ${created.quote_no} 已创建（${created.items.length} 行），下一步提交审批`);
      await reloadList(1);
      showDetail(created);
    }
  } catch (error) {
    formError.value = errorText(error, editingId.value ? '保存失败' : '建单失败');
  } finally {
    formSaving.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<Quotation | null>(null);
/** 转单回链的订单号（详情里只有订单 id，订单号要单独查；查不到就退回短 id） */
const convertedSoNo = ref<string | null>(null);

const detailHeader = computed(() =>
  detail.value ? `${detail.value.quote_no} · ${detail.value.status_label}` : '报价单详情',
);
const approvalBadge = computed(() => approvalBadgeOf(detail.value));
const submitState = computed(() =>
  detail.value ? canSubmitApproval(detail.value) : { enabled: false, hint: '请先打开一张报价单' },
);
const statusActions = computed<StatusAction[]>(() =>
  detail.value ? statusActionsOf(detail.value.status, detail.value.allowed_targets) : [],
);
const convertState = computed(() => convertStateOf(detail.value));
const convertedText = computed(() =>
  detail.value ? convertedRefText(detail.value, convertedSoNo.value) : '—',
);

function showDetail(body: Quotation): void {
  detail.value = body;
  detailVisible.value = true;
  convertedSoNo.value = null;
  void loadConvertedSoNo(body);
}

/** 列表行 → 详情（拉全量：明细 + 审批摘要） */
async function openDetail(id: string): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detail.value = null;
  try {
    showDetail(await fetchQuotationDetail(id));
  } catch (error) {
    detailVisible.value = false;
    MessagePlugin.error(errorText(error, '报价单详情加载失败'));
  } finally {
    detailLoading.value = false;
  }
}

/** 取转单的订单号（只为展示；失败 = 空 = 退回短 id，不影响任何按钮） */
async function loadConvertedSoNo(body: Quotation): Promise<void> {
  if (!body.converted_so_id) {
    return;
  }
  try {
    const order = await fetchSalesOrderDetail(body.converted_so_id);
    if (detail.value?.id === body.id) {
      convertedSoNo.value = order.so_no;
    }
  } catch {
    // 订单可能被删 / 无权看：详情里已经有 id 前 8 位兜底，这里静默
  }
}

/** 抽屉开着就同步刷新那一张（写操作后不靠猜，重取读接口） */
function syncDetail(body: Quotation): void {
  if (!detailVisible.value || detail.value?.id !== body.id) {
    return;
  }
  showDetail(body);
}

// ==================== 提交审批 / 发出 ====================

const submittingApproval = ref(false);
const issuing = ref(false);

async function submitApproval(): Promise<void> {
  if (!detail.value) {
    return;
  }
  const body = detail.value;
  submittingApproval.value = true;
  try {
    const updated = await submitQuotationApproval(body.id);
    MessagePlugin.success(
      updated.approval
        ? `${updated.quote_no} 已送审（${updated.approval.instance_no}），审批通过后自动发出`
        : `${updated.quote_no} 已发出（没配 quotation_approve 流程，按跳过审批处理，理由已留痕）`,
    );
    showDetail(updated);
    await reloadList();
  } catch (error) {
    MessagePlugin.error(errorText(error, '提交审批失败'));
  } finally {
    submittingApproval.value = false;
  }
}

/** 审批通过后的「发出」：唯一的非审批状态推进（后端 `IssueGate::RequireApproval` 放行） */
async function issueQuotation(): Promise<void> {
  if (!detail.value) {
    return;
  }
  const body = detail.value;
  issuing.value = true;
  try {
    const updated = await changeQuotationStatus(body.id, { target_status: 'SENT' });
    MessagePlugin.success(`${updated.quote_no} 已发出，可以等客户回复了`);
    showDetail(updated);
    await reloadList();
  } catch (error) {
    MessagePlugin.error(errorText(error, '发出失败'));
  } finally {
    issuing.value = false;
  }
}

// ==================== 状态流转（原因弹窗） ====================

const statusVisible = ref(false);
const statusTarget = ref<QuotationStatus | null>(null);
const statusNeedsReason = ref(false);
const statusReason = ref('');
const statusError = ref('');
const statusSaving = ref(false);

const statusDialogHeader = computed(() => {
  if (!detail.value || !statusTarget.value) {
    return '状态流转';
  }
  return `${detail.value.quote_no}：${detail.value.status_label} → ${statusLabel(statusTarget.value)}`;
});

function statusActionTip(action: StatusAction): string {
  return action.needsReason
    ? `${action.label}：这条边要写原因（对外推翻过承诺，事后要能回答「为什么」）`
    : `${action.label}：不需要原因，会留一条状态日志`;
}

function openStatusDialog(action: StatusAction): void {
  if (!detail.value) {
    return;
  }
  statusTarget.value = action.target;
  statusNeedsReason.value = action.needsReason;
  statusReason.value = '';
  statusError.value = '';
  statusVisible.value = true;
}

async function submitStatus(): Promise<void> {
  const body = detail.value;
  const target = statusTarget.value;
  if (!body || !target) {
    statusVisible.value = false;
    return;
  }
  const reason = statusReason.value.trim();
  if (statusNeedsReason.value && !reason) {
    statusError.value = '这条状态迁移必须填写原因（客户拒绝 / 已发出后作废，是审计依据）';
    return;
  }
  statusSaving.value = true;
  statusError.value = '';
  try {
    const updated = await changeQuotationStatus(body.id, {
      target_status: target,
      reason: reason || undefined,
    });
    statusVisible.value = false;
    MessagePlugin.success(`${updated.quote_no} 已变为「${updated.status_label}」`);
    showDetail(updated);
    await reloadList();
  } catch (error) {
    statusError.value = errorText(error, '状态流转失败');
  } finally {
    statusSaving.value = false;
  }
}

// ==================== 删除 ====================

function confirmDelete(row: Pick<Quotation, 'id' | 'quote_no' | 'deletable'>): void {
  if (!row.deletable) {
    void MessagePlugin.warning('只有草稿 / 已作废能删：发出去的报价要留痕，请走作废');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除报价单',
    theme: 'warning',
    body: `确认删除 ${row.quote_no}？软删（可从历史里恢复审计），删除后该单号不再占用；已发出的报价请走「作废」而不是删除。`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    cancelBtn: '再想想',
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteQuotation(row.id);
        MessagePlugin.success(`${row.quote_no} 已删除`);
        if (detail.value?.id === row.id) {
          detailVisible.value = false;
        }
        await reloadList(1);
      } catch (error) {
        MessagePlugin.error(errorText(error, '删除失败'));
      }
    },
  });
}

// ==================== 转销售订单 ====================

const convertVisible = ref(false);
const convertTarget = ref<Pick<Quotation, 'id' | 'quote_no'> | null>(null);
const convertForm = reactive<ConvertFormDraft>(emptyConvertForm());
const convertError = ref('');
const converting = ref(false);

const convertHeader = computed(() =>
  convertTarget.value ? `转销售订单 · ${convertTarget.value.quote_no}` : '转销售订单',
);

/** 列表行 / 详情都能进来（列表行缺明细，转单只按报价单 id 走后端，不需要明细） */
function openConvert(row: Pick<Quotation, 'id' | 'quote_no' | 'status' | 'converted_so_id' | 'expired'>): void {
  const state = convertStateOf(row);
  if (!state.enabled) {
    void MessagePlugin.warning(state.hint);
    return;
  }
  convertTarget.value = row;
  Object.assign(convertForm, emptyConvertForm());
  convertError.value = '';
  convertVisible.value = true;
}

async function submitConvert(): Promise<void> {
  const target = convertTarget.value;
  if (!target) {
    convertVisible.value = false;
    return;
  }
  const message = validateConvertForm(convertForm);
  if (message) {
    convertError.value = message;
    return;
  }
  converting.value = true;
  convertError.value = '';
  try {
    const result: ConvertQuotationRes = await convertQuotation(target.id, buildConvertPayload(convertForm));
    convertVisible.value = false;
    MessagePlugin.success(convertResultText(result));
    await reloadList();
    // 详情抽屉开着且就是这张单 → 重取（状态列/回链都要更新）
    if (detail.value?.id === target.id) {
      try {
        showDetail(await fetchQuotationDetail(target.id));
      } catch {
        // 重取失败不影响「已转单」这个事实，用户手动刷新即可
      }
    }
  } catch (error) {
    convertError.value = errorText(error, '转销售订单失败');
  } finally {
    converting.value = false;
  }
}

// ==================== 生命周期 ====================

onMounted(() => {
  void loadOptions();
});
</script>

<style scoped>
.qt-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.qt-gate {
  display: inline-block;
}

.qt-sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.qt-mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.qt-amount {
  font-weight: 600;
}

.qt-no-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.qt-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 24px 0;
  color: var(--td-text-color-secondary);
}

.qt-empty__icon {
  font-size: 28px;
}

.qt-empty__title {
  margin: 0;
}

.qt-detail {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.qt-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.qt-section-title {
  display: flex;
  align-items: baseline;
  gap: 8px;
  font-weight: 600;
}

.qt-totals {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 20px;
}

.qt-form-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.qt-form-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 0 12px;
}

.qt-line-editor {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px;
  border: 1px solid var(--td-component-border);
  border-radius: 6px;
}

.qt-line-head,
.qt-line-row {
  display: grid;
  grid-template-columns: 32px minmax(200px, 1.5fr) 110px 130px 90px 130px minmax(120px, 1fr) 64px;
  align-items: center;
  gap: 8px;
}

.qt-line-head {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.qt-line-no {
  text-align: center;
}

.qt-line-amount {
  text-align: right;
  font-weight: 600;
}
</style>
