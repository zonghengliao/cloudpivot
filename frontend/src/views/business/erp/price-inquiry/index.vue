<template>
  <div class="piq-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="单号 / 采购员 / 物料编码"
          clearable
          data-testid="piq-filter-keyword"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="piq-filter-status"
        />

        <t-date-picker
          v-model="filter.date_from"
          class="filter-row__date"
          value-type="YYYY-MM-DD"
          placeholder="询价日期起"
          clearable
          data-testid="piq-filter-date-from"
        />
        <t-date-picker
          v-model="filter.date_to"
          class="filter-row__date"
          value-type="YYYY-MM-DD"
          placeholder="询价日期止"
          clearable
          data-testid="piq-filter-date-to"
        />

        <t-button theme="primary" data-testid="btn-piq-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-piq-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-piq-refresh" @click="loadList">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="
            canCreate
              ? '新建询价单：一张单可以问多家供应商，定标后按供应商拆采购订单'
              : '当前账号没有 business:erp:price-inquiry:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              :disabled="!canCreate"
              data-testid="btn-piq-new"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建询价单
            </t-button>
          </span>
        </t-tooltip>
      </div>

      <t-alert
        v-if="listError"
        theme="error"
        :message="listError"
        class="page-alert"
        data-testid="piq-list-error"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="table-card">
      <t-table
        row-key="id"
        size="small"
        hover
        :data="rows"
        :columns="columns"
        :loading="loading"
        :empty="emptyText"
        data-testid="piq-table"
      >
        <template #inquiry_no="{ row }">
          <div class="no-cell">
            <b class="mono">{{ row.inquiry_no }}</b>
            <span class="sub">{{ row.inquiry_date }}</span>
          </div>
        </template>

        <template #buyer_name="{ row }">
          <span>{{ row.buyer_name || '—' }}</span>
        </template>

        <template #warehouse_name="{ row }">
          <span>{{ row.warehouse_name || '—' }}</span>
        </template>

        <template #counts="{ row }">
          <span class="mono counts-cell">
            {{ row.item_count }} 行 / {{ row.supplier_count }} 家
          </span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="statusTheme(row.status)">
            {{ row.status_label || statusLabel(row.status) }}
          </t-tag>
        </template>

        <template #award="{ row }">
          <div class="award-cell">
            <t-tag v-if="row.award_mode" size="small" variant="outline" theme="primary">
              {{ row.award_mode_label }}
            </t-tag>
            <span class="sub">{{ row.award_supplier_name || awardSupplierText(row) }}</span>
          </div>
        </template>

        <template #po_refs="{ row }">
          <span class="sub mono">{{ poRefsText(row.po_refs) }}</span>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-button
              variant="text"
              theme="primary"
              size="small"
              data-testid="btn-piq-detail"
              @click="openDetail(row.id)"
            >
              比价 / 详情
            </t-button>
            <t-button
              variant="text"
              theme="primary"
              size="small"
              :disabled="!canQuote || row.status === 'CONVERTED'"
              data-testid="btn-piq-quote"
              @click="openQuoteFor(row.id)"
            >
              录入报价
            </t-button>
            <t-button
              variant="text"
              theme="primary"
              size="small"
              :disabled="!canToPo || row.status !== 'AWARDED'"
              data-testid="btn-piq-to-po"
              @click="openToPoFor(row.id)"
            >
              转采购单
            </t-button>
          </div>
        </template>
      </t-table>

      <div class="pager-row">
        <t-pagination
          :current="page"
          :page-size="PAGE_SIZE"
          :total="total"
          :show-page-size="false"
          show-jumper
          data-testid="piq-pager"
          @current-change="handlePageChange"
        />
      </div>
    </t-card>

    <!-- ==================== ③ 新建询价单 ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="新建询价单"
      :width="1040"
      :confirm-btn="{ content: '创建', loading: creating, disabled: creating }"
      :cancel-btn="{ disabled: creating }"
      :close-on-overlay-click="false"
      data-testid="piq-create-dialog"
      @confirm="submitCreate"
    >
      <div class="form-body">
        <t-form label-width="100px" class="form">
          <div class="form-grid">
            <t-form-item label="询价日期">
              <t-date-picker
                v-model="createForm.inquiry_date"
                value-type="YYYY-MM-DD"
                placeholder="默认今天（北京时间）"
                clearable
                data-testid="piq-create-date"
              />
            </t-form-item>

            <t-form-item label="收货仓库" required-mark>
              <t-select
                v-model="createForm.warehouse_id"
                :options="warehouseOptions"
                filterable
                placeholder="转采购单时用它落收货仓库"
                data-testid="piq-create-warehouse"
              />
            </t-form-item>
          </div>

          <t-form-item label="备注">
            <t-input
              v-model="createForm.remark"
              :maxlength="MAX_REMARK"
              placeholder="可空，例：2026 年 11 月机加工件寻源"
              data-testid="piq-create-remark"
            />
          </t-form-item>
        </t-form>

        <div class="line-editor" data-testid="piq-create-items">
          <div class="line-editor__head">
            <span class="line-editor__no">#</span>
            <span>物料</span>
            <span>数量</span>
            <span>需求日期</span>
            <span>行备注</span>
            <span class="line-editor__op"></span>
          </div>

          <div v-for="(line, index) in createForm.items" :key="line.key" class="line-editor__row">
            <span class="line-editor__no mono">{{ index + 1 }}</span>
            <t-select
              v-model="line.material_id"
              :options="materialOptions"
              filterable
              placeholder="选择物料（单位自动带出）"
              :data-testid="`piq-create-material-${index}`"
            />
            <t-input-number
              v-model="line.qty"
              :min="0"
              :max="9999999"
              theme="normal"
              placeholder="数量"
              :data-testid="`piq-create-qty-${index}`"
            />
            <t-date-picker
              v-model="line.demand_date"
              value-type="YYYY-MM-DD"
              clearable
              placeholder="可空"
              :data-testid="`piq-create-demand-${index}`"
            />
            <t-input v-model="line.remark" placeholder="可空" />
            <t-button
              class="line-editor__op"
              variant="text"
              theme="danger"
              :disabled="createForm.items.length <= 1"
              @click="removeCreateLine(index)"
            >
              删除
            </t-button>
          </div>

          <t-button variant="outline" data-testid="btn-piq-add-line" @click="addCreateLine">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
        </div>

        <t-alert
          v-if="createError"
          theme="error"
          :message="createError"
          data-testid="piq-create-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ④ 录入报价 ==================== -->
    <t-dialog
      v-model:visible="quoteVisible"
      :header="quoteHeader"
      :width="980"
      :confirm-btn="{ content: '提交报价', loading: quoting, disabled: quoting }"
      :cancel-btn="{ disabled: quoting }"
      :close-on-overlay-click="false"
      data-testid="piq-quote-dialog"
      @confirm="submitQuote"
    >
      <div class="form-body">
        <t-form label-width="100px" class="form">
          <t-form-item label="报价供应商" required-mark>
            <t-select
              v-model="quoteForm.supplier_id"
              :options="supplierOptions"
              filterable
              placeholder="一家供应商一次提交，同一供应商可重复提交改价"
              data-testid="piq-quote-supplier"
              @change="onQuoteSupplierChange"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="quoteReplacing"
          theme="warning"
          message="该供应商已经报过这几行：本次提交是「改价」（同一明细 × 供应商唯一，不会新增一行）"
          data-testid="piq-quote-replace-hint"
        />

        <table class="edit-table" data-testid="piq-quote-table">
          <thead>
            <tr>
              <th>#</th>
              <th>物料</th>
              <th>数量</th>
              <th>不含税单价</th>
              <th>税率%</th>
              <th>交期（天）</th>
              <th>备注</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(line, index) in quoteForm.items" :key="line.item_id">
              <td class="mono">{{ line.line_no }}</td>
              <td>
                <div class="material-cell">
                  <span class="mono">{{ line.material_code }}</span>
                  <span>{{ line.material_name }}</span>
                </div>
              </td>
              <td class="mono">{{ decimalText(line.qty) }}</td>
              <td>
                <t-input-number
                  v-model="line.price_excl"
                  :min="0"
                  :decimal-places="6"
                  theme="normal"
                  placeholder="0.000000"
                  :data-testid="`piq-quote-price-${index}`"
                />
              </td>
              <td>
                <t-input-number
                  v-model="line.tax_rate"
                  :min="0"
                  :max="100"
                  :decimal-places="2"
                  theme="normal"
                  :data-testid="`piq-quote-tax-${index}`"
                />
              </td>
              <td>
                <t-input-number
                  v-model="line.delivery_days"
                  :min="0"
                  :max="3650"
                  theme="normal"
                  placeholder="可空"
                  :data-testid="`piq-quote-days-${index}`"
                />
              </td>
              <td>
                <t-input v-model="line.remark" placeholder="可空" />
              </td>
            </tr>
          </tbody>
        </table>

        <p class="hint">
          含税单价不填：后端按 <b>不含税单价 ×（1 + 税率%）</b> 现算并落库（比价表里的含税价与不含税价同源）。
        </p>

        <t-alert
          v-if="quoteError"
          theme="error"
          :message="quoteError"
          data-testid="piq-quote-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 详情 / 比价 / 定标 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="1180px"
      :header="detailHeader"
      :footer="false"
      data-testid="piq-detail-drawer"
    >
      <div v-if="detail" class="detail-body">
        <!-- 单头 -->
        <t-descriptions :column="3" size="small" class="head-desc" data-testid="piq-detail-head">
          <t-descriptions-item label="询价单号">
            <span class="mono">{{ detail.header.inquiry_no }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="询价日期">{{ detail.header.inquiry_date }}</t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag size="small" variant="light" :theme="statusTheme(detail.header.status)">
              {{ detail.header.status_label }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="采购员">{{ detail.header.buyer_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="收货仓库">{{ detail.header.warehouse_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="报价供应商">{{ detail.header.supplier_count }} 家</t-descriptions-item>
          <t-descriptions-item label="定标方式">
            {{ detail.header.award_mode_label || '—' }}
          </t-descriptions-item>
          <t-descriptions-item label="中标供应商">
            {{ detail.header.award_supplier_name || awardSupplierText(detail.header) }}
          </t-descriptions-item>
          <t-descriptions-item label="采购订单">
            <span class="mono">{{ poRefsText(detail.header.po_refs) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="备注" :span="3">{{ detail.header.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <!-- 比价表 -->
        <div class="section-title">
          比价表
          <span class="sub">同物料 × 不同供应商：单价 / 税率 / 交期 横向对比，最低价标绿</span>
        </div>

        <div class="compare-wrap">
          <table class="compare-table" data-testid="piq-compare-table">
            <thead>
              <tr>
                <th class="col-no">#</th>
                <th class="col-material">物料</th>
                <th class="col-qty">数量</th>
                <th class="col-date">需求日期</th>
                <th class="col-bidders">报价家数</th>
                <th v-for="supplier in compareSuppliers" :key="supplier.supplier_id">
                  {{ supplier.supplier_name }}
                </th>
                <th class="col-award">中标供应商</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in detail.comparison" :key="row.item_id">
                <td class="mono">{{ row.line_no }}</td>
                <td>
                  <div class="material-cell">
                    <span class="mono">{{ row.material_code }}</span>
                    <span>{{ row.material_name }}</span>
                  </div>
                </td>
                <td class="mono">{{ decimalText(row.qty) }} {{ row.unit_name }}</td>
                <td>{{ row.demand_date || '—' }}</td>
                <td>
                  <t-tag size="small" variant="light" :theme="row.has_competition ? 'success' : 'warning'">
                    {{ row.bidder_count }} 家
                  </t-tag>
                </td>
                <td
                  v-for="supplier in compareSuppliers"
                  :key="supplier.supplier_id"
                  class="quote-cell"
                  :class="{ 'is-lowest': quoteOf(row, supplier.supplier_id)?.is_lowest }"
                >
                  <template v-if="quoteOf(row, supplier.supplier_id)">
                    <div class="quote-price mono">
                      {{ decimalText(quoteOf(row, supplier.supplier_id)?.price_excl) }}
                      <t-tag
                        v-if="quoteOf(row, supplier.supplier_id)?.is_lowest"
                        size="small"
                        theme="success"
                        variant="light"
                      >
                        最低
                      </t-tag>
                      <t-tag
                        v-if="quoteOf(row, supplier.supplier_id)?.is_awarded"
                        size="small"
                        theme="primary"
                        variant="light"
                      >
                        中标
                      </t-tag>
                    </div>
                    <div class="sub mono">
                      含税 {{ decimalText(quoteOf(row, supplier.supplier_id)?.price_incl) }} ·
                      税 {{ decimalText(quoteOf(row, supplier.supplier_id)?.tax_rate) }}% ·
                      交期 {{ quoteOf(row, supplier.supplier_id)?.delivery_days ?? '—' }} 天
                    </div>
                    <div v-if="quoteOf(row, supplier.supplier_id)?.remark" class="sub">
                      {{ quoteOf(row, supplier.supplier_id)?.remark }}
                    </div>
                  </template>
                  <span v-else class="sub">未报价</span>
                </td>
                <td>
                  <t-select
                    v-model="awardPick[row.item_id]"
                    size="small"
                    :options="quoteOptionsOf(row)"
                    :disabled="!canAward || awardLocked"
                    placeholder="选择中标"
                    :data-testid="`piq-award-select-${row.line_no}`"
                  />
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- 定标 -->
        <div class="section-title">
          定标
          <span class="sub">每条明细选一家中标供应商；不同明细可以给不同供应商 → 转单时自动拆成多张采购单</span>
        </div>

        <div class="award-panel">
          <t-alert
            v-if="singleSourceLines(detail).length > 0"
            theme="warning"
            :message="singleSourceMessage"
            data-testid="piq-single-source-alert"
          />

          <div class="award-row">
            <t-button
              variant="outline"
              :disabled="!canAward || awardLocked"
              data-testid="btn-piq-pick-lowest"
              @click="pickLowest"
            >
              全部选最低价
            </t-button>

            <t-textarea
              v-model="awardReason"
              class="award-reason"
              :maxlength="MAX_REMARK"
              :disabled="!canAward || awardLocked"
              :placeholder="`单源例外理由（不足 2 家报价的明细必须填，至少 ${MIN_SINGLE_SOURCE_REASON} 字）`"
              data-testid="piq-award-reason"
            />
          </div>

          <div v-if="awardSummary.length > 0" class="award-preview">
            <span class="sub">定标预览（按供应商分组 → 采购单）：</span>
            <t-tag
              v-for="group in awardSummary"
              :key="group.supplier_id"
              variant="light"
              theme="primary"
              data-testid="piq-award-preview-tag"
            >
              {{ group.supplier_name }} · 第 {{ group.line_nos.join('、') }} 行
            </t-tag>
          </div>

          <t-alert
            v-if="awardError"
            theme="error"
            :message="awardError"
            data-testid="piq-award-error"
          />

          <div class="award-actions">
            <t-button
              theme="primary"
              :loading="awarding"
              :disabled="!canAward || awardLocked || awarding"
              data-testid="btn-piq-award-submit"
              @click="submitAward"
            >
              定标
            </t-button>
            <t-tooltip :content="toPoState.hint" placement="top">
              <span class="gate-wrap">
                <t-button
                  theme="success"
                  variant="outline"
                  :loading="converting"
                  :disabled="!canToPo || !toPoState.enabled"
                  data-testid="btn-piq-to-po-submit"
                  @click="openToPo"
                >
                  转采购订单
                </t-button>
              </span>
            </t-tooltip>
            <t-button
              variant="outline"
              :disabled="!canQuote || detail.header.status === 'CONVERTED'"
              data-testid="btn-piq-quote-from-detail"
              @click="openQuoteFor(detail.header.id)"
            >
              录入报价
            </t-button>
          </div>
        </div>

        <!-- 明细（只读） -->
        <div class="section-title">询价明细</div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="itemColumns"
          data-testid="piq-item-table"
        />
      </div>
    </t-drawer>

    <!-- ==================== ⑥ 转采购订单 ==================== -->
    <t-dialog
      v-model:visible="toPoVisible"
      header="转采购订单"
      :width="620"
      :confirm-btn="{ content: '生成采购订单', loading: converting, disabled: converting }"
      :cancel-btn="{ disabled: converting }"
      :close-on-overlay-click="false"
      data-testid="piq-to-po-dialog"
      @confirm="submitToPo"
    >
      <div class="form-body">
        <t-alert
          theme="info"
          message="按中标供应商分组：同一供应商的多行合成一张采购订单，单价取该供应商的中标报价（金额由采购模块服务端现算）。"
        />
        <t-form label-width="100px" class="form">
          <t-form-item label="下单日期">
            <t-date-picker
              v-model="toPoForm.order_date"
              value-type="YYYY-MM-DD"
              placeholder="默认今天（北京时间）"
              clearable
              data-testid="piq-to-po-date"
            />
          </t-form-item>
          <t-form-item label="收货仓库">
            <t-select
              v-model="toPoForm.warehouse_id"
              :options="warehouseOptions"
              filterable
              clearable
              :placeholder="`默认用询价单的仓库：${detail?.header.warehouse_name ?? '—'}`"
              data-testid="piq-to-po-warehouse"
            />
          </t-form-item>
          <t-form-item label="备注前缀">
            <t-input
              v-model="toPoForm.remark"
              :maxlength="MAX_REMARK"
              placeholder="可空，例：转自 PIQ-…"
              data-testid="piq-to-po-remark"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="toPoError"
          theme="error"
          :message="toPoError"
          data-testid="piq-to-po-error"
        />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import {
  fetchMaterialPage,
  fetchSupplierOptions,
  fetchWarehousePage,
  type MaterialRow,
  type Supplier,
  type Warehouse,
} from '@/api/business/erp';
import {
  STATUS_OPTIONS,
  awardPriceInquiry,
  convertPriceInquiryToPo,
  createPriceInquiry,
  fetchPriceInquiryDetail,
  fetchPriceInquiryPage,
  statusLabel,
  statusTheme,
  submitPriceQuote,
  type CompareRow,
  type PriceInquiryDetail,
  type PriceInquiryRow,
  type ToPoResult,
} from '@/api/business/erp/price-inquiry';
import {
  MAX_REMARK,
  MIN_SINGLE_SOURCE_REASON,
  PAGE_SIZE,
  awardPickOf,
  awardSummaryOf,
  buildAwardPayload,
  buildCreatePayload,
  buildQuery,
  buildQuotePayload,
  buildToPoPayload,
  compareSuppliersOf,
  dateRangeError,
  decimalText,
  emptyCreateForm,
  emptyFilter,
  emptyListText,
  emptyQuoteForm,
  emptyToPoForm,
  isFiltered,
  lowestPicksOf,
  newCreateLine,
  poRefsText,
  quoteOf,
  quotesOfSupplier,
  singleSourceLines,
  toPoStateOf,
  validateAward,
  validateCreateForm,
  validateQuoteForm,
  type AwardPick,
  type CreateForm,
  type FilterState,
  type QuoteForm,
  type ToPoForm,
} from './price-inquiry-logic';

/** 本页权限码（与 `V0321__price_inquiry.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情 */
  list: 'business:erp:price-inquiry:list',
  /** 新建 */
  create: 'business:erp:price-inquiry:create',
  /** 录入报价 */
  quote: 'business:erp:price-inquiry:quote',
  /** 定标 */
  award: 'business:erp:price-inquiry:award',
  /** 转采购订单 */
  toPo: 'business:erp:price-inquiry:to-po',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canQuote = computed(() => userStore.hasPermission(PERM.quote));
const canAward = computed(() => userStore.hasPermission(PERM.award));
const canToPo = computed(() => userStore.hasPermission(PERM.toPo));

/** 错误文案归一（后端 409 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  return error instanceof Error ? error.message : fallback;
}

// ==================== 列表 ====================

const filter = reactive<FilterState>(emptyFilter());
const rows = ref<PriceInquiryRow[]>([]);
const total = ref(0);
const page = ref(1);
const loading = ref(false);
const listError = ref('');

const statusOptions = STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const emptyText = computed(() => emptyListText(isFiltered(filter)));

const columns: PrimaryTableCol[] = [
  { colKey: 'inquiry_no', title: '询价单号', width: 190 },
  { colKey: 'buyer_name', title: '采购员', width: 100 },
  { colKey: 'warehouse_name', title: '收货仓库', width: 130 },
  { colKey: 'counts', title: '明细 / 报价', width: 120 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'award', title: '定标', minWidth: 190 },
  { colKey: 'po_refs', title: '采购订单', minWidth: 190 },
  { colKey: 'op', title: '操作', width: 230, fixed: 'right' },
];

const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 150 },
  { colKey: 'material_name', title: '物料名称', minWidth: 180 },
  { colKey: 'qty', title: '数量', width: 110 },
  { colKey: 'unit_name', title: '单位', width: 80 },
  { colKey: 'demand_date', title: '需求日期', width: 120 },
  { colKey: 'remark', title: '备注', minWidth: 140 },
];

/** 单一供应商定标时的显示名（多供应商定标时 `award_supplier_name` 为空，靠报价译名） */
function awardSupplierText(row: Pick<PriceInquiryRow, 'award_supplier_id' | 'award_supplier_name' | 'award_mode'>): string {
  if (row.award_supplier_name) {
    return row.award_supplier_name;
  }
  if (row.award_mode === 'SINGLE_SOURCE') {
    return '单源例外（明细见比价表）';
  }
  if (row.award_mode === 'MULTI') {
    return '多家分组定标（明细见比价表）';
  }
  return '—';
}

async function loadList(): Promise<void> {
  const rangeError = dateRangeError(filter);
  if (rangeError) {
    listError.value = rangeError;
    return;
  }
  loading.value = true;
  listError.value = '';
  try {
    const data = await fetchPriceInquiryPage(buildQuery(filter, page.value));
    rows.value = data.list;
    total.value = data.total;
  } catch (error) {
    listError.value = errorText(error, '询价单列表加载失败');
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  page.value = 1;
  void loadList();
}

function handleReset(): void {
  Object.assign(filter, emptyFilter());
  page.value = 1;
  void loadList();
}

function handlePageChange(next: number): void {
  page.value = next;
  void loadList();
}

// ==================== 主数据下拉 ====================

const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
const suppliers = ref<Supplier[]>([]);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}`,
    value: item.id,
  })),
);
const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_name}（${item.warehouse_code}）`,
    value: item.id,
  })),
);
const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({
    label: `${item.supplier_name}（${item.supplier_code}）`,
    value: item.id,
  })),
);

/** 主数据下拉失败不阻塞页面（仍可看列表 / 比价；只是没法建单） */
async function loadOptions(): Promise<void> {
  const [materialPage, warehousePage, supplierList] = await Promise.all([
    fetchMaterialPage({ status: 1, page: 1, page_size: 200 }).catch(() => null),
    fetchWarehousePage({ status: 1, page: 1, page_size: 200 }).catch(() => null),
    fetchSupplierOptions(200).catch(() => [] as Supplier[]),
  ]);
  materials.value = materialPage?.list ?? [];
  warehouses.value = warehousePage?.list ?? [];
  suppliers.value = supplierList;
}

// ==================== 新建 ====================

const createVisible = ref(false);
const createForm = reactive<CreateForm>(emptyCreateForm());
const createError = ref('');
const creating = ref(false);

function openCreate(): void {
  Object.assign(createForm, emptyCreateForm());
  createError.value = '';
  createVisible.value = true;
}

function addCreateLine(): void {
  createForm.items.push(newCreateLine());
}

function removeCreateLine(index: number): void {
  if (createForm.items.length <= 1) {
    return;
  }
  createForm.items.splice(index, 1);
}

async function submitCreate(): Promise<void> {
  const message = validateCreateForm(createForm);
  if (message) {
    createError.value = message;
    return;
  }
  creating.value = true;
  createError.value = '';
  try {
    const detail = await createPriceInquiry(buildCreatePayload(createForm));
    createVisible.value = false;
    MessagePlugin.success(`询价单 ${detail.header.inquiry_no} 已创建（${detail.items.length} 行），下一步录入报价`);
    page.value = 1;
    await loadList();
    // 建完直接进详情，省一次点击（详情里能直接录入报价）
    showDetail(detail);
  } catch (error) {
    createError.value = errorText(error, '建单失败');
  } finally {
    creating.value = false;
  }
}

// ==================== 详情 / 比价 / 定标 ====================

const detailVisible = ref(false);
const detail = ref<PriceInquiryDetail | null>(null);
const awardPick = ref<AwardPick>({});
const awardReason = ref('');
const awardError = ref('');
const awarding = ref(false);

const detailHeader = computed(() =>
  detail.value ? `${detail.value.header.inquiry_no} · ${detail.value.header.status_label}` : '询价单详情',
);
const compareSuppliers = computed(() => (detail.value ? compareSuppliersOf(detail.value) : []));
const awardLocked = computed(() =>
  detail.value ? detail.value.header.status === 'AWARDED' || detail.value.header.status === 'CONVERTED' : true,
);
const awardSummary = computed(() => (detail.value ? awardSummaryOf(detail.value, awardPick.value) : []));
const singleSourceMessage = computed(() => {
  if (!detail.value) {
    return '';
  }
  const lines = singleSourceLines(detail.value).map((row) => row.line_no);
  return `第 ${lines.join('、')} 行不足 2 家报价：单源采购必须填写例外理由（至少 ${MIN_SINGLE_SOURCE_REASON} 字），理由是定标留痕、事后审计的依据`;
});
const toPoState = computed(() =>
  detail.value ? toPoStateOf(detail.value) : { enabled: false, hint: '请先打开一张询价单' },
);

/** 每行可选的供应商（只有报了价的才能选 —— 后端同样会 409 拦没报价的组合） */
function quoteOptionsOf(row: CompareRow): { label: string; value: string }[] {
  return row.quotes.map((quote) => ({
    label: `${quote.supplier_name}（${decimalText(quote.price_excl)}）`,
    value: quote.supplier_id,
  }));
}

function showDetail(next: PriceInquiryDetail): void {
  detail.value = next;
  awardPick.value = awardPickOf(next);
  awardReason.value = '';
  awardError.value = '';
  detailVisible.value = true;
}

async function openDetail(id: string): Promise<void> {
  try {
    showDetail(await fetchPriceInquiryDetail(id));
  } catch (error) {
    MessagePlugin.error(errorText(error, '询价单详情加载失败'));
  }
}

/** 详情重取（任何写操作后用后端返回值刷新，保证页面与库一致） */
async function refreshDetail(id: string): Promise<void> {
  const next = await fetchPriceInquiryDetail(id);
  detail.value = next;
  awardPick.value = { ...awardPickOf(next), ...awardPick.value };
  for (const key of Object.keys(awardPick.value)) {
    if (!next.comparison.some((row) => row.item_id === key)) {
      delete awardPick.value[key];
    }
  }
}

function pickLowest(): void {
  if (!detail.value || awardLocked.value) {
    return;
  }
  awardPick.value = lowestPicksOf(detail.value);
}

async function submitAward(): Promise<void> {
  if (!detail.value) {
    return;
  }
  const message = validateAward(detail.value, awardPick.value, awardReason.value);
  if (message) {
    awardError.value = message;
    return;
  }
  awarding.value = true;
  awardError.value = '';
  const inquiryId = detail.value.header.id;
  try {
    const next = await awardPriceInquiry(
      inquiryId,
      buildAwardPayload(awardPick.value, awardReason.value),
    );
    detail.value = next;
    awardPick.value = awardPickOf(next);
    MessagePlugin.success(
      next.header.award_mode === 'SINGLE_SOURCE'
        ? '已定标（单源例外，理由已留痕），可以转采购订单'
        : '已定标，可以转采购订单',
    );
    await loadList();
  } catch (error) {
    awardError.value = errorText(error, '定标失败');
  } finally {
    awarding.value = false;
  }
}

// ==================== 报价 ====================

const quoteVisible = ref(false);
const quoteForm = reactive<QuoteForm>({ supplier_id: '', items: [] });
const quoteError = ref('');
const quoting = ref(false);
const quoteInquiryId = ref('');

const quoteHeader = computed(() =>
  detail.value ? `录入报价 · ${detail.value.header.inquiry_no}` : '录入报价',
);
const quoteReplacing = computed(() => {
  if (!detail.value || !quoteForm.supplier_id) {
    return false;
  }
  return quotesOfSupplier(detail.value, quoteForm.supplier_id).length > 0;
});

async function openQuoteFor(id: string): Promise<void> {
  try {
    const next = detail.value?.header.id === id ? detail.value : await fetchPriceInquiryDetail(id);
    detail.value = next;
    quoteInquiryId.value = id;
    Object.assign(quoteForm, emptyQuoteForm(next));
    quoteError.value = '';
    quoteVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '询价单详情加载失败'));
  }
}

/** 换供应商时把该供应商已有报价回填（改价场景不用重敲） */
function applyQuoteSupplierBackfill(): void {
  if (!detail.value || !quoteForm.supplier_id) {
    return;
  }
  const existing = quotesOfSupplier(detail.value, quoteForm.supplier_id);
  if (existing.length === 0) {
    return;
  }
  const byItem = new Map(existing.map((quote) => [quote.item_id, quote]));
  for (const line of quoteForm.items) {
    const quote = byItem.get(line.item_id);
    if (!quote) {
      continue;
    }
    line.price_excl = Number(quote.price_excl);
    line.tax_rate = Number(quote.tax_rate);
    line.delivery_days = quote.delivery_days ?? '';
    line.remark = quote.remark ?? '';
  }
}

async function submitQuote(): Promise<void> {
  const message = validateQuoteForm(quoteForm);
  if (message) {
    quoteError.value = message;
    return;
  }
  quoting.value = true;
  quoteError.value = '';
  try {
    const next = await submitPriceQuote(quoteInquiryId.value, buildQuotePayload(quoteForm));
    detail.value = next;
    awardPick.value = { ...awardPickOf(next), ...awardPick.value };
    quoteVisible.value = false;
    MessagePlugin.success(
      quoteReplacing.value ? '报价已更新（同一明细 × 供应商 = 改价）' : '报价已录入',
    );
    await loadList();
  } catch (error) {
    quoteError.value = errorText(error, '报价提交失败');
  } finally {
    quoting.value = false;
  }
}

// ==================== 转采购订单 ====================

const toPoVisible = ref(false);
const toPoForm = reactive<ToPoForm>(emptyToPoForm());
const toPoError = ref('');
const converting = ref(false);

function openToPo(): void {
  Object.assign(toPoForm, emptyToPoForm());
  toPoError.value = '';
  toPoVisible.value = true;
}

/**
 * 列表行上的「转采购单」：先按 id 取详情（列表行没有中标明细），再开弹窗。
 *
 * 不直接复用 `detail.value`（用户可能看的是另一张单）—— id 相同才复用缓存，
 * 否则重新拉，避免把 A 单的仓库/明细带进 B 单的转单请求。
 */
async function openToPoFor(id: string): Promise<void> {
  try {
    const next = detail.value?.header.id === id ? detail.value : await fetchPriceInquiryDetail(id);
    detail.value = next;
    awardPick.value = awardPickOf(next);
    openToPo();
  } catch (error) {
    MessagePlugin.error(errorText(error, '询价单详情加载失败'));
  }
}

async function submitToPo(): Promise<void> {
  if (!detail.value) {
    return;
  }
  const inquiryId = detail.value.header.id;
  converting.value = true;
  toPoError.value = '';
  try {
    const result: ToPoResult = await convertPriceInquiryToPo(inquiryId, buildToPoPayload(toPoForm));
    toPoVisible.value = false;
    const names = result.purchase_orders.map((order) => order.po_no).join('、');
    MessagePlugin.success(`已生成 ${result.purchase_orders.length} 张采购订单：${names}`);
    await refreshDetail(inquiryId);
    await loadList();
  } catch (error) {
    toPoError.value = errorText(error, '转采购订单失败');
  } finally {
    converting.value = false;
  }
}

/**
 * 报价弹窗里换供应商时回填已有报价。
 *
 * `t-select` 的 `@change` 在 v-model 更新前/后执行因版本而异，这里用 `nextTick`
 * 之后的 watcher 更稳 —— 但更简单可靠的是在 change 回调里显式取事件值。
 */
function onQuoteSupplierChange(value: unknown): void {
  quoteForm.supplier_id = typeof value === 'string' ? value : '';
  applyQuoteSupplierBackfill();
}

onMounted(() => {
  void loadOptions();
  void loadList();
});
</script>

<style scoped>
.piq-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card,
.table-card {
  border-radius: 8px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.filter-row__keyword {
  width: 240px;
}

.filter-row__select {
  width: 140px;
}

.filter-row__date {
  width: 170px;
}

.filter-row__spacer {
  flex: 1;
}

.gate-wrap {
  display: inline-block;
}

.page-alert {
  margin-top: 12px;
}

.no-cell,
.award-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.counts-cell {
  white-space: nowrap;
}

.op-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 2px;
}

.pager-row {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}

.form-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 12px;
}

.line-editor {
  border: 1px solid var(--td-component-border);
  border-radius: 6px;
  padding: 8px;
}

.line-editor__head,
.line-editor__row {
  display: grid;
  grid-template-columns: 32px minmax(220px, 1.6fr) 110px 150px minmax(140px, 1fr) 64px;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
}

.line-editor__head {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.line-editor__no {
  text-align: center;
}

.line-editor__op {
  display: flex;
  justify-content: center;
}

.hint {
  margin: 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.detail-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.section-title {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-top: 4px;
  font-weight: 600;
}

.compare-wrap {
  overflow-x: auto;
}

.compare-table,
.edit-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.compare-table th,
.compare-table td,
.edit-table th,
.edit-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 8px;
  text-align: left;
  vertical-align: top;
}

.compare-table th,
.edit-table th {
  background: var(--td-bg-color-page, #f5f7fa);
  font-weight: 600;
  white-space: nowrap;
}

.col-no {
  width: 40px;
}

.col-material {
  min-width: 170px;
}

.col-qty {
  width: 110px;
}

.col-date {
  width: 110px;
}

.col-bidders {
  width: 90px;
}

.col-award {
  width: 190px;
}

.quote-cell.is-lowest {
  background: var(--td-success-color-1, #e8f8f2);
}

.quote-price {
  display: flex;
  align-items: center;
  gap: 4px;
  font-weight: 600;
}

.material-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.award-panel {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.award-row {
  display: flex;
  align-items: flex-start;
  gap: 10px;
}

.award-reason {
  flex: 1;
}

.award-preview {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}

.award-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
</style>
