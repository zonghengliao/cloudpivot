<template>
  <div class="stockin-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-stockin-create"
          @click="openCreateDialog"
        >
          <template #icon><t-icon name="add" /></template>
          新建收货单
        </t-button>
        <t-button variant="outline" data-testid="btn-stockin-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="stockin-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.supplier_id"
          class="tool-row__supplier"
          :options="supplierOptions"
          placeholder="全部供应商"
          clearable
          filterable
          data-testid="stockin-filter-supplier"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.warehouse_id"
          class="tool-row__warehouse"
          :options="warehouseOptions"
          placeholder="全部仓库"
          clearable
          filterable
          data-testid="stockin-filter-warehouse"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__no"
          placeholder="收货单号 / 备注（模糊）"
          clearable
          data-testid="stockin-filter-no"
          @enter="handleSearch"
        />
        <!-- D4/O3：按来源采购单号筛（从采购单跳过来的人最常用这一条） -->
        <t-input
          v-model="filter.purchase_order_no"
          class="tool-row__po"
          placeholder="来源采购单号（模糊）"
          clearable
          data-testid="stockin-filter-po-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-stockin-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-stockin-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">收货入库</span>
          <span class="panel-head__meta" data-testid="stockin-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">草稿可编辑 / 过账；过账后作废需复核（作废审批中锁定单据，审批通过才回滚六本账）</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="stockin-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadOrders()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="stockin-table"
        @page-change="handlePageChange"
      >
        <template #in_no="{ row }">
          <span class="mono" :data-testid="`stockin-no-${row.in_no}`">{{ row.in_no }}</span>
        </template>

        <!-- D4/F3：来源采购单号可点，跳回采购单详情（没有来源就是「—」） -->
        <template #source_order_no="{ row }">
          <t-link
            v-if="row.source_order_no"
            theme="primary"
            class="mono"
            :data-testid="`stockin-po-link-${row.in_no}`"
            @click="goPurchaseOrder(row)"
          >
            {{ row.source_order_no }}
          </t-link>
          <span v-else class="muted">—</span>
        </template>

        <template #supplier_id="{ row }">{{ supplierNameOf(row.supplier_id) }}</template>

        <template #warehouse_id="{ row }">{{ warehouseNameOf(row.warehouse_id) }}</template>

        <template #in_date="{ row }">
          <span class="muted">{{ shortDate(row.in_date) }}</span>
        </template>

        <template #total_qty="{ row }">
          <span class="num" :data-testid="`stockin-qty-${row.in_no}`">{{ formatQty(row.total_qty) }}</span>
        </template>

        <!-- ★ 金额列同样标「(价税合计)」 -->
        <template #total_with_tax="{ row }">
          <span class="money" :data-testid="`stockin-total-${row.in_no}`">{{ formatMoney(row.total_with_tax) }}</span>
        </template>

        <template #status="{ row }">
          <div class="status-cell">
            <t-tag size="small" :theme="stockInStatusTheme(row.status)" :data-testid="`stockin-status-${row.in_no}`">
              {{ stockInStatusLabel(row.status) }}
            </t-tag>
            <!-- 过账后能看出「按可抵扣进项税入账」还是事后作废 -->
            <t-tag v-if="row.status === 'POSTED' && row.invoice_received" size="small" variant="outline" theme="success">
              发票已到
            </t-tag>
          </div>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-view-${row.in_no}`" @click="openDetail(row)">查看</t-link>
            <t-link
              v-if="actionsOf(row).edit"
              theme="primary"
              :data-testid="`btn-edit-${row.in_no}`"
              @click="openEditDialog(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="actionsOf(row).post"
              theme="primary"
              :disabled="!canPost(row)"
              :data-testid="`btn-post-${row.in_no}`"
              @click="openPostDialog(row)"
            >
              过账
            </t-link>
            <t-link
              v-if="actionsOf(row).cancel"
              theme="danger"
              :data-testid="`btn-cancel-${row.in_no}`"
              @click="confirmCancel(row)"
            >
              作废
            </t-link>
            <t-link
              v-if="actionsOf(row).withdrawCancel"
              theme="primary"
              :data-testid="`btn-withdraw-cancel-${row.in_no}`"
              @click="confirmWithdrawCancel(row)"
            >
              撤回作废申请
            </t-link>
            <t-link
              v-if="actionsOf(row).remove"
              theme="danger"
              :data-testid="`btn-remove-${row.in_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="stockin-empty">
            <t-icon name="inbox" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的收货单' : '还没有收货单' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个条件再查，或点右侧「重置」回全量' : '点左上角「新建收货单」录入第一张' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="form.id ? '编辑收货单' : '新建收货单'"
      width="900px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockin-form-dialog"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <!-- D4/F1：从采购单带出时把「源单 + 条数 + 草稿不自动过账」说清楚 -->
      <t-alert v-if="form.purchase_order_id" theme="info" class="dlg-alert" data-testid="stockin-source-po">
        <template #message>
          已从采购单 <strong class="mono">{{ form.purchase_order_no || form.purchase_order_id }}</strong>
          带出 <strong>{{ form.items.length }}</strong> 行待收明细：数量默认 = <strong>待收数量</strong>，
          可以改小（部分收货）或删掉这次不收的行。保存后是<strong>草稿</strong>，核对无误再点「过账」。
        </template>
      </t-alert>

      <t-alert theme="info" class="dlg-alert" data-testid="stockin-cost-note">
        <template #message>
          入库成本只按<strong>不含税单价</strong>入账（移动加权），价税合计由系统按税率计算；过账时才写库存。
        </template>
      </t-alert>

      <t-form :data="form" label-width="84px">
        <div class="form-row">
          <t-form-item
            label="供应商"
            name="supplier_id"
            class="form-row__col"
            :status="formErrors.supplier_id ? 'error' : undefined"
            :tips="formErrors.supplier_id"
          >
            <t-select
              v-model="form.supplier_id"
              :options="supplierOptions"
              placeholder="选择供应商"
              filterable
              data-testid="stockin-supplier"
            />
          </t-form-item>

          <t-form-item
            label="收货仓库"
            name="warehouse_id"
            class="form-row__col"
            :status="formErrors.warehouse_id ? 'error' : undefined"
            :tips="formErrors.warehouse_id"
          >
            <t-select
              v-model="form.warehouse_id"
              :options="warehouseOptions"
              placeholder="选择收货仓库"
              filterable
              data-testid="stockin-warehouse"
            />
          </t-form-item>
        </div>

        <div class="form-row">
          <t-form-item
            label="入库日期"
            name="in_date"
            class="form-row__col"
            :status="formErrors.in_date ? 'error' : undefined"
            :tips="formErrors.in_date"
          >
            <t-date-picker
              v-model="form.in_date"
              value-type="YYYY-MM-DD"
              placeholder="选择日期"
              data-testid="stockin-date"
            />
          </t-form-item>

          <t-form-item label="发票" name="invoice_received" class="form-row__col">
            <t-radio-group v-model="form.invoice_received" :disabled="true" data-testid="stockin-invoice">
              <t-radio :value="true">发票已随货到达</t-radio>
              <t-tooltip :content="ESTIMATE_DISABLED_TIP">
                <span class="disabled-radio"><t-radio :value="false" :disabled="true">发票未到（暂估）</t-radio></span>
              </t-tooltip>
            </t-radio-group>
          </t-form-item>
        </div>

        <t-form-item label="备注" name="remark" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark">
          <t-input
            v-model="form.remark"
            :maxlength="STOCKIN_MAX_REMARK"
            placeholder="可空，如：到货验收合格"
            data-testid="stockin-remark"
          />
        </t-form-item>

        <!-- ---------------- 明细 ---------------- -->
        <div class="items">
          <div class="items__head">
            <span class="items__title">明细</span>
            <span class="items__hint">数量必须 > 0；单价填不含税单价</span>
          </div>

          <div class="items__grid items__grid--head">
            <span>物料</span>
            <span>数量</span>
            <span>不含税单价</span>
            <span>税率</span>
            <span class="ta-right">{{ AMOUNT_COLUMN_TITLE }}</span>
            <span class="ta-right">其中税额</span>
            <span />
          </div>

          <div
            v-for="(item, index) in form.items"
            :key="item.key"
            class="items__row"
            :data-testid="`stockin-item-${index}`"
          >
            <div class="items__grid">
              <div class="items__cell">
                <t-select
                  v-model="item.material_id"
                  :options="materialOptions"
                  placeholder="选择物料"
                  filterable
                  :data-testid="`stockin-item-material-${index}`"
                />
                <p v-if="rowErrors(index).material_id" class="cell-error">{{ rowErrors(index).material_id }}</p>
              </div>

              <div class="items__cell">
                <t-input-number
                  v-model="item.qty"
                  :min="0"
                  :max="item.pending ? item.pending.pending_qty : 9999999"
                  theme="normal"
                  :data-testid="`stockin-item-qty-${index}`"
                />
                <!-- D4/F2：把上限摆在数量下面（订单 / 已收 / 待收），改数量时不用翻采购单 -->
                <p v-if="item.pending" class="cell-hint" :data-testid="`stockin-item-pending-${index}`">
                  订单 {{ formatQty(item.pending.ordered_qty) }} /
                  已收 {{ formatQty(item.pending.received_qty) }} /
                  待收 <strong>{{ formatQty(item.pending.pending_qty) }}</strong>
                </p>
                <p v-if="rowErrors(index).qty" class="cell-error">{{ rowErrors(index).qty }}</p>
              </div>

              <div class="items__cell">
                <!-- 同采购页：不锁 6 位小数，避免「2」显示成「2.000000」 -->
                <t-input-number
                  v-model="item.price_excl"
                  :min="0"
                  theme="normal"
                  :data-testid="`stockin-item-price-${index}`"
                />
                <p v-if="rowErrors(index).price_excl" class="cell-error">{{ rowErrors(index).price_excl }}</p>
              </div>

              <div class="items__cell">
                <t-select
                  v-model="item.tax_rate"
                  :options="taxRateOptions"
                  :data-testid="`stockin-item-tax-${index}`"
                />
                <p v-if="rowErrors(index).tax_rate" class="cell-error">{{ rowErrors(index).tax_rate }}</p>
              </div>

              <div class="items__cell items__cell--num">
                <span class="money" :data-testid="`stockin-item-amount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(item).total) }}
                </span>
              </div>

              <div class="items__cell items__cell--num">
                <span class="muted" :data-testid="`stockin-item-taxamount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(item).tax) }}
                </span>
              </div>

              <div class="items__cell items__cell--op">
                <t-tooltip content="删除该行">
                  <t-button
                    variant="text"
                    shape="square"
                    :disabled="form.items.length <= 1"
                    :data-testid="`stockin-item-remove-${index}`"
                    @click="removeItem(index)"
                  >
                    <template #icon><t-icon name="delete" /></template>
                  </t-button>
                </t-tooltip>
              </div>
            </div>
          </div>

          <t-button variant="dashed" block data-testid="btn-stockin-add-item" @click="addItem">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
          <p v-if="formErrors.items" class="cell-error">{{ formErrors.items }}</p>
        </div>

        <div class="totals" data-testid="stockin-totals">
          <span class="totals__item">
            合计数量 <strong data-testid="stockin-total-qty">{{ formatQty(totals.qty) }}</strong>
          </span>
          <span class="totals__item">
            价款 <strong data-testid="stockin-total-amount">{{ formatMoney(totals.amount) }}</strong>
          </span>
          <span class="totals__item">
            税额 <strong data-testid="stockin-total-tax">{{ formatMoney(totals.tax) }}</strong>
          </span>
          <span class="totals__item totals__item--strong">
            {{ AMOUNT_COLUMN_TITLE }} <strong data-testid="stockin-total-with-tax">{{ formatMoney(totals.total) }}</strong>
          </span>
        </div>

        <div class="dlg-footer">
          <t-button variant="outline" data-testid="btn-stockin-cancel" @click="formVisible = false">取消</t-button>
          <t-button
            v-privilege="[form.id ? PERM.update : PERM.create]"
            theme="primary"
            :loading="saving"
            data-testid="btn-stockin-save"
            @click="save"
          >
            保存草稿
          </t-button>
        </div>
      </t-form>
    </t-dialog>

    <!-- ==================== ④ ★ 过账确认弹窗（PRD §3.3.2） ==================== -->
    <t-dialog
      v-model:visible="postVisible"
      header="确认过账"
      width="560px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockin-post-dialog"
    >
      <div class="post-body">
        <div class="post-kv">
          <span class="post-kv__label">收货单</span>
          <span class="post-kv__value mono" data-testid="stockin-post-no">{{ postTarget?.in_no ?? '—' }}</span>
        </div>
        <div class="post-kv">
          <span class="post-kv__label">金额</span>
          <span class="post-kv__value money" data-testid="stockin-post-total">
            价税合计 {{ formatMoney(postTarget?.total_with_tax) }}
          </span>
        </div>

        <!-- 两个发票选项：已随货到达默认勾选；「发票未到」置灰 + tooltip -->
        <div class="post-invoice" data-testid="stockin-post-invoice">
          <div
            v-for="option in INVOICE_OPTIONS"
            :key="option.value"
            class="post-invoice__row"
            :class="{ 'post-invoice__row--disabled': option.disabled }"
          >
            <t-tooltip :content="option.tip" :disabled="!option.disabled">
              <span class="post-invoice__inner">
                <t-checkbox
                  :checked="invoiceChoice === option.value"
                  :disabled="option.disabled"
                  :data-testid="`stockin-invoice-${option.value.toLowerCase()}`"
                  @change="handleInvoiceChoice(option.value, option.disabled)"
                >
                  <span class="post-invoice__label">{{ option.label }}</span>
                </t-checkbox>
                <span class="post-invoice__desc">（{{ option.desc }}）</span>
                <t-tag v-if="option.disabled" size="small" variant="outline" theme="default">本批不可选</t-tag>
              </span>
            </t-tooltip>
          </div>
        </div>

        <t-alert theme="warning" class="post-warn" data-testid="stockin-post-consequences">
          <template #message>
            <div class="post-warn__title"><t-icon name="error-circle" /> 过账后将：</div>
            <ul class="post-warn__list">
              <li v-for="line in POST_CONSEQUENCES" :key="line">{{ line }}</li>
            </ul>
          </template>
        </t-alert>
      </div>

      <div class="dlg-footer">
        <t-button variant="outline" data-testid="btn-stockin-post-cancel" @click="postVisible = false">取消</t-button>
        <t-button
          v-privilege="[PERM.status]"
          theme="primary"
          :loading="posting"
          data-testid="btn-stockin-post-confirm"
          @click="confirmPost"
        >
          确认过账
        </t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="收货单详情"
      size="720px"
      :footer="false"
      data-testid="stockin-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail">
        <div class="detail__head">
          <span class="detail__no mono">{{ detail.header.in_no }}</span>
          <t-tag size="small" :theme="stockInStatusTheme(detail.header.status)">
            {{ detail.header.status_label || stockInStatusLabel(detail.header.status) }}
          </t-tag>
          <t-tag size="small" variant="outline" :theme="detail.header.invoice_received ? 'success' : 'warning'">
            {{ detail.header.invoice_received ? '发票已到' : '发票未到（暂估）' }}
          </t-tag>
        </div>

        <div class="detail__grid">
          <div class="detail-field"><span class="detail-field__label">供应商</span><span class="detail-field__value">{{ supplierNameOf(detail.header.supplier_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">收货仓库</span><span class="detail-field__value">{{ warehouseNameOf(detail.header.warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">入库日期</span><span class="detail-field__value">{{ detail.header.in_date }}</span></div>
          <div class="detail-field"><span class="detail-field__label">入库类型</span><span class="detail-field__value">{{ detail.header.in_type }}</span></div>
          <!-- D4/F3：来源采购单可点，跳回采购单详情 -->
          <div class="detail-field">
            <span class="detail-field__label">来源采购单</span>
            <span class="detail-field__value">
              <t-link
                v-if="detail.purchase_order_no || detail.purchase_order_id"
                theme="primary"
                class="mono"
                data-testid="stockin-detail-po-link"
                @click="goPurchaseOrderById(detail.purchase_order_id, detail.purchase_order_no)"
              >
                {{ detail.purchase_order_no || shortId(detail.purchase_order_id) }}
              </t-link>
              <span v-else class="muted">—（无源直收）</span>
            </span>
          </div>
          <div class="detail-field"><span class="detail-field__label">合计数量</span><span class="detail-field__value">{{ formatQty(detail.header.total_qty) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">序列号件数</span><span class="detail-field__value">{{ detail.serial_count }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价款</span><span class="detail-field__value">{{ formatMoney(detail.header.total_amount) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">税额</span><span class="detail-field__value">{{ formatMoney(detail.header.total_tax) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价税合计</span><span class="detail-field__value money">{{ formatMoney(detail.header.total_with_tax) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">过账时间</span><span class="detail-field__value">{{ dash(detail.header.posted_at) }}</span></div>
          <div class="detail-field detail-field--wide"><span class="detail-field__label">备注</span><span class="detail-field__value">{{ dash(detail.header.remark) }}</span></div>
          <div v-if="detail.header.cancel_reason" class="detail-field detail-field--wide">
            <span class="detail-field__label">作废原因 / 申请理由</span><span class="detail-field__value">{{ detail.header.cancel_reason }}</span>
          </div>
        </div>

        <p v-if="detail.summary" class="detail__summary">{{ detail.summary }}</p>

        <!-- P16：作废审批进度（复用审批中心的 FlowProgress，进度只来自引擎写的节点行） -->
        <template v-if="approvalNodes.length > 0">
          <div class="section-title">作废审批进度</div>
          <FlowProgress :nodes="approvalNodes" data-testid="stockin-cancel-progress" />
        </template>

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table row-key="id" size="small" :data="detail.items" :columns="detailColumns" data-testid="stockin-detail-items">
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }">{{ formatQty(row.qty) }}</template>
          <template #price="{ row }">{{ formatMoney(row.price) }}</template>
          <template #tax_rate="{ row }">{{ formatQty(row.tax_rate) }}%</template>
          <template #amount="{ row }">{{ formatMoney(row.amount) }}</template>
          <template #tax_amount="{ row }">{{ formatMoney(row.tax_amount) }}</template>
          <template #with_tax_amount="{ row }">{{ formatMoney(row.with_tax_amount) }}</template>
          <template #cost_price="{ row }">{{ formatMoney(row.cost_price) }}</template>
        </t-table>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  createStockIn,
  deleteStockIn,
  fetchMaterialPage,
  fetchPurchaseOrderPendingItems,
  fetchStockInDetail,
  fetchStockInPage,
  fetchSupplierOptions,
  fetchWarehousePage,
  updateStockIn,
  updateStockInStatus,
  withdrawStockInCancel,
  type MaterialRow,
  type StockInDetail,
  type StockInRow,
  type Supplier,
  type Warehouse,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress, {
  type FlowProgressNode,
} from '@/views/business/flow/components/FlowProgress.vue';
import {
  AMOUNT_COLUMN_TITLE,
  ESTIMATE_DISABLED_TIP,
  INVOICE_OPTIONS,
  POST_CONSEQUENCES,
  STOCKIN_MAX_REMARK,
  STOCKIN_MAX_LINES,
  STOCKIN_PAGE_SIZE,
  STOCKIN_STATUS_FILTER_OPTIONS,
  availableStockInActions,
  buildCancelPayload,
  buildPostPayload,
  buildStockInPayload,
  buildStockInQuery,
  canPost,
  cancelConfirmMessage,
  computeTotals,
  dash,
  defaultInvoiceChoice,
  detailToForm,
  emptyStockInFilter,
  emptyStockInForm,
  emptyStockInItem,
  formatMoney,
  formatQty,
  formFromPending,
  isStockInFiltered,
  lineAmountsOfItem,
  shortDate,
  shortId,
  stockInStatusLabel,
  stockInStatusTheme,
  validateStockInForm,
  type StockInFilterState,
  type StockInFormErrors,
  type StockInFormState,
} from './stockin-logic';

/** 本页权限码（与 V0147 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:stock-in:list',
  create: 'business:erp:stock-in:create',
  update: 'business:erp:stock-in:update',
  status: 'business:erp:stock-in:status',
  remove: 'business:erp:stock-in:delete',
} as const;

/** 主数据下拉一次拉多少 */
const OPTION_LIMIT = 200;

/** 表格列（卡面 §四：in_no / 供应商 / 仓库 / 入库日期 / 数量 / 金额 / 状态 / 操作） */
const columns: PrimaryTableCol[] = [
  { colKey: 'in_no', title: '收货单号', width: 210 },
  // D4/F3：来源采购单（列上直接给单号，不用点进详情才知道这批货是给哪张单收的）
  { colKey: 'source_order_no', title: '来源采购单', width: 190, ellipsis: true },
  { colKey: 'supplier_id', title: '供应商', minWidth: 150, ellipsis: true },
  { colKey: 'warehouse_id', title: '仓库', width: 140, ellipsis: true },
  { colKey: 'in_date', title: '入库日期', width: 100 },
  { colKey: 'total_qty', title: '数量', width: 90, align: 'right' },
  // ★ 金额列标题带「(价税合计)」
  { colKey: 'total_with_tax', title: AMOUNT_COLUMN_TITLE, width: 150, align: 'right' },
  { colKey: 'status', title: '状态', width: 150 },
  { colKey: 'op', title: '操作', width: 210, fixed: 'right' },
];

/** 详情抽屉里的明细列 */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 50, align: 'center' },
  { colKey: 'material_id', title: '物料', minWidth: 150, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 80, align: 'right' },
  { colKey: 'price', title: '不含税单价', width: 110, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 70, align: 'right' },
  { colKey: 'amount', title: '不含税金额', width: 110, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 90, align: 'right' },
  { colKey: 'with_tax_amount', title: AMOUNT_COLUMN_TITLE, width: 130, align: 'right' },
  { colKey: 'cost_price', title: '成本单价', width: 110, align: 'right' },
];

/** 状态下拉 */
const statusFilterOptions = STOCKIN_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

/** 税率下拉 */
const taxRateOptions = [
  { label: '13%', value: 13 },
  { label: '9%', value: 9 },
  { label: '6%', value: 6 },
  { label: '3%', value: 3 },
  { label: '0%（免税）', value: 0 },
];

// ==================== 列表 ====================

const filter = reactive<StockInFilterState>(emptyStockInFilter());
const rows = ref<StockInRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = STOCKIN_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const suppliers = ref<Supplier[]>([]);
const warehouses = ref<Warehouse[]>([]);
const materials = ref<MaterialRow[]>([]);

const filtered = computed(() => isStockInFiltered(filter));

const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: `${item.supplier_name}（${item.supplier_code}）`, value: item.id })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 供应商名称 */
function supplierNameOf(supplierId: string | null | undefined): string {
  if (!supplierId) {
    return '—';
  }
  const hit = suppliers.value.find((item) => item.id === supplierId);
  return hit ? hit.supplier_name : shortId(supplierId);
}

/** 仓库名称 */
function warehouseNameOf(warehouseId: string | null | undefined): string {
  if (!warehouseId) {
    return '—';
  }
  const hit = warehouses.value.find((item) => item.id === warehouseId);
  return hit ? hit.warehouse_name : shortId(warehouseId);
}

/** 物料名称 */
function materialNameOf(materialId: string): string {
  const hit = materials.value.find((item) => item.id === materialId);
  return hit ? `${hit.material_name}（${hit.material_code}）` : shortId(materialId);
}

/** 行按钮显隐 */
function actionsOf(row: StockInRow): ReturnType<typeof availableStockInActions> {
  return availableStockInActions(row.status);
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表 */
async function loadOrders(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchStockInPage(buildStockInQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 主数据 */
async function loadBasics(): Promise<void> {
  const [supplierList, warehousePage, materialPage] = await Promise.all([
    fetchSupplierOptions(OPTION_LIMIT).catch(() => [] as Supplier[]),
    fetchWarehousePage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
  ]);
  suppliers.value = supplierList;
  warehouses.value = warehousePage?.list ?? [];
  materials.value = materialPage?.list ?? [];
}

function handleSearch(): void {
  void loadOrders(1);
}

function handleReset(): void {
  Object.assign(filter, emptyStockInFilter());
  void loadOrders(1);
}

function refresh(): void {
  void loadOrders();
  void loadBasics();
}

function handlePageChange(pageInfo: { current?: number }): void {
  void loadOrders(pageInfo.current ?? 1);
}

// ==================== 表单 ====================

const formVisible = ref(false);
const saving = ref(false);
const submitError = ref('');
const form = reactive<StockInFormState>(emptyStockInForm());
const formErrors = ref<StockInFormErrors>({});

/** 实时合计（仅用于所见即所得；落库以服务端为准） */
const totals = computed(() => computeTotals(form.items));

/** 第 index 行的字段错误 */
function rowErrors(index: number): Record<string, string> {
  const prefix = `items.${index}`;
  const out: Record<string, string> = {};
  for (const [key, text] of Object.entries(formErrors.value)) {
    if (key.startsWith(`${prefix}.`)) {
      out[key.slice(prefix.length + 1)] = text;
    }
  }
  return out;
}

function resetForm(): void {
  Object.assign(form, emptyStockInForm());
  formErrors.value = {};
  submitError.value = '';
}
function openCreateDialog(): void {
  resetForm();
  formVisible.value = true;
}

// ==================== D4：采购订单 → 一键生成收货单 ====================

const router = useRouter();
const route = useRoute();

/** 采购订单页的路由路径（`business/erp/purchase-order`，别名 `purchase`） */
const PURCHASE_PATH = '/business/erp/purchase-order';

/** 列表 / 详情里的「来源采购单号」点击 → 跳回采购单（带 `po_id` 让对端直开详情） */
function goPurchaseOrderById(id: string | null | undefined, no: string | null | undefined): void {
  if (!id) {
    return;
  }
  void router.push({ path: PURCHASE_PATH, query: { po_id: id, ...(no ? { po_no: no } : {}) } });
}

/** 列表行的「来源采购单」点击 */
function goPurchaseOrder(row: StockInRow): void {
  goPurchaseOrderById(row.source_order_id, row.source_order_no);
}

/**
 * 从采购单带出草稿（卡面 F1 / F2 / O1）。
 *
 * 流程：查待收明细 → 单头（供应商 / 仓库 / 来源单号）从源单填 → 明细只带 `pending_qty > 0` 的行、
 * 数量默认 = 待收数量 → **仍是草稿**（硬红线 7：不自动过账）。
 */
async function openFromPurchaseOrder(poId: string): Promise<void> {
  resetForm();
  loading.value = true;
  try {
    const pending = await fetchPurchaseOrderPendingItems(poId);
    const { form: filled, lineCount } = formFromPending(pending);
    if (lineCount === 0) {
      // 一行都不剩：不打开空草稿，直接把后端给的原因说出来
      MessagePlugin.warning(pending.blocked_reason ?? `采购单 ${pending.order.order_no} 没有可收的待收明细`);
      openCreateDialog();
      return;
    }
    Object.assign(form, filled);
    formErrors.value = {};
    submitError.value = '';
    formVisible.value = true;
    MessagePlugin.success(
      `已从采购单 ${pending.order.order_no} 带出 ${lineCount} 行待收明细，请核对数量`,
    );
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    openCreateDialog();
  } finally {
    loading.value = false;
  }
}

/** 编辑（拉详情：注意 `header` 包了一层） */
async function openEditDialog(row: StockInRow): Promise<void> {
  if (!actionsOf(row).edit) {
    MessagePlugin.warning('只有草稿可以编辑');
    return;
  }
  resetForm();
  formVisible.value = true;
  loading.value = true;
  try {
    const detail = await fetchStockInDetail(row.id);
    Object.assign(form, detailToForm(detail.header, detail.items));
  } catch (error) {
    formVisible.value = false;
    MessagePlugin.error(messageOf(error));
  } finally {
    loading.value = false;
  }
}

function addItem(): void {
  if (form.items.length >= STOCKIN_MAX_LINES) {
    MessagePlugin.warning(`明细最多 ${STOCKIN_MAX_LINES} 行`);
    return;
  }
  form.items.push(emptyStockInItem(`row-${Date.now()}-${form.items.length + 1}`));
}

function removeItem(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
}

/** 保存草稿（建单 / 改单） */
async function save(): Promise<void> {
  formErrors.value = validateStockInForm(form);
  if (Object.keys(formErrors.value).length > 0) {
    MessagePlugin.warning('表单还有 ' + Object.keys(formErrors.value).length + ' 处需要修正');
    return;
  }
  const isEdit = Boolean(form.id);
  saving.value = true;
  submitError.value = '';
  try {
    const payload = buildStockInPayload(form);
    const detail = isEdit ? await updateStockIn(form.id, payload) : await createStockIn(payload);
    MessagePlugin.success(`${detail.header.in_no} 已保存为草稿`);
    formVisible.value = false;
    await loadOrders(isEdit ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== ★ 过账 ====================

const postVisible = ref(false);
const posting = ref(false);
const postTarget = ref<StockInRow | null>(null);
/** 发票选项（默认「已随货到达」；「发票未到」置灰不可选） */
const invoiceChoice = ref<'RECEIVED' | 'ESTIMATE'>(defaultInvoiceChoice());

/** 打开过账确认弹窗（每次都重置为默认勾选） */
function openPostDialog(row: StockInRow): void {
  if (!actionsOf(row).post) {
    MessagePlugin.warning('只有草稿可以过账');
    return;
  }
  if (!canPost(row)) {
    MessagePlugin.warning('这张收货单没有数量，不能过账');
    return;
  }
  postTarget.value = row;
  invoiceChoice.value = defaultInvoiceChoice();
  postVisible.value = true;
}

/** 发票选项点击：置灰项直接忽略（双保险，UI 已 disabled） */
function handleInvoiceChoice(value: 'RECEIVED' | 'ESTIMATE', disabled: boolean): void {
  if (disabled) {
    MessagePlugin.warning(ESTIMATE_DISABLED_TIP);
    return;
  }
  invoiceChoice.value = value;
}

/** 确认过账（不可撤销，成功后刷列表） */
async function confirmPost(): Promise<void> {
  const target = postTarget.value;
  if (!target) {
    return;
  }
  posting.value = true;
  try {
    await updateStockInStatus(target.id, buildPostPayload());
    MessagePlugin.success(`${target.in_no} 已过账，库存与流水已写入`);
    postVisible.value = false;
    postTarget.value = null;
    await loadOrders();
  } catch (error) {
    // 重复过账会拿到 409 + 10003（文案含「已过账」），原样透出
    MessagePlugin.error(messageOf(error));
  } finally {
    posting.value = false;
  }
}

/**
 * 发起作废（P16：已过账的单据走**审批**，不再直接动账）。
 *
 * - `DRAFT`：直接作废（从未入账，无需复核）；
 * - `POSTED`：只发起作废审批 → 状态变「作废审批中」，六本账等复核通过才回滚；
 * - 文案逐字说明后果（`cancelConfirmMessage`），不让人以为点一下就红冲了。
 */
function confirmCancel(row: StockInRow): void {
  if (!actionsOf(row).cancel) {
    MessagePlugin.warning('当前状态不能作废');
    return;
  }
  const isApproval = row.status === 'POSTED';
  const dialog = DialogPlugin.confirm({
    header: isApproval ? '发起作废审批' : '作废收货单',
    theme: 'danger',
    body: cancelConfirmMessage(row.status),
    confirmBtn: {
      content: isApproval ? '提交审批' : '确认作废',
      theme: 'danger',
    },
    onConfirm: async () => {
      dialog.hide();
      try {
        const updated = await updateStockInStatus(row.id, buildCancelPayload('页面作废'));
        if (isApproval) {
          MessagePlugin.success(
            `${row.in_no} 已提交作废审批（作废审批中）：审批通过后才回滚六本账`,
          );
        } else {
          MessagePlugin.success(`${row.in_no} 已作废`);
        }
        // 详情抽屉开着时同步刷新（状态 / 审批实例 id 都变了）
        if (detailVisible.value && updated?.header?.id === row.id) {
          detail.value = updated;
        }
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

/**
 * 撤回作废申请（仅 `PENDING_CANCEL`；后端校验「只有发起人能撤」）。
 *
 * 撤回后单据回到「已过账」、审批单落 `WITHDRAWN`、审批中心待办同时消失 —— 账一点没动。
 */
function confirmWithdrawCancel(row: StockInRow): void {
  if (!actionsOf(row).withdrawCancel) {
    MessagePlugin.warning('只有「作废审批中」的单据能撤回作废申请');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '撤回作废申请',
    body: `撤回后 ${row.in_no} 回到「已过账」，库存等六本账不受影响。确认撤回？`,
    confirmBtn: { content: '确认撤回', theme: 'primary' },
    onConfirm: async () => {
      dialog.hide();
      try {
        const updated = await withdrawStockInCancel(row.id);
        MessagePlugin.success(`${row.in_no} 已撤回作废申请（回到已过账）`);
        if (detailVisible.value && updated?.header?.id === row.id) {
          detail.value = updated;
        }
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

/**
 * 删除（仅 DRAFT，软删）。
 *
 * 与「作废」的区别：作废是**留痕**的状态变化（POSTED 也能作废，会写 cancelled_* 三列）；
 * 删除是「这张单从列表里消失」，只对草稿开放（后端 403/409 兜底）。
 */
function confirmRemove(row: StockInRow): void {
  if (!actionsOf(row).remove) {
    MessagePlugin.warning('只有草稿可以删除');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除收货单',
    theme: 'danger',
    body: `${row.in_no} 将被软删除（列表不再显示，历史台账不受影响）。确认删除？`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteStockIn(row.id);
        MessagePlugin.success(`${row.in_no} 已删除`);
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 详情 ====================
const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<StockInDetail | null>(null);
/** 作废审批进度（P16；空 = 没挂审批单 / 拉不到） */
const approvalNodes = ref<FlowProgressNode[]>([]);

async function openDetail(row: StockInRow): Promise<void> {
  await loadDetail(row.id);
}

/** 按 id 开详情抽屉（路由带 `in_id` 跳过来时用，手里没有整行） */
async function loadDetail(id: string): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  approvalNodes.value = [];
  try {
    detail.value = await fetchStockInDetail(id);
    await loadApprovalProgress(detail.value?.header?.approval_instance_id ?? null);
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

/**
 * 拉审批进度（P16：只有在「作废审批中」的单据才挂 `approval_instance_id`）。
 *
 * 直接复用审批中心的 `getFlowInstance` + `FlowProgress`：
 * **进度只来自引擎写的节点行**，前端不自己猜「走到第几步」。
 * 拉不到（权限 / 实例已归档）就退化为不显示，不让详情页整块失败。
 */
async function loadApprovalProgress(instanceId: string | null): Promise<void> {
  if (!instanceId) {
    approvalNodes.value = [];
    return;
  }
  try {
    const instance = await getFlowInstance(instanceId);
    approvalNodes.value = (instance.nodes ?? []).map((node) => ({
      node_key: node.node_key,
      name: node.node_name || node.node_key,
      type: node.node_type,
      round: node.round,
      status: node.status,
      approver_count: node.approver_count ?? 0,
    }));
  } catch {
    approvalNodes.value = [];
  }
}

onMounted(() => {
  void loadBasics();
  void loadOrders(1);
  void applyRouteQuery();
});

/**
 * 路由参数（D4 跨模块跳转）。
 *
 * - `?from=po&po_id=<id>`：从采购单详情点「生成收货单」跳进来 → 自动带出草稿；
 * - `?in_id=<id>`：从采购单详情点「已生成收货单 IN-xxx」跳进来 → 直接开详情抽屉。
 *
 * 参数消费后立刻 `replace` 掉：刷新页面不该重复弹一次带出 / 详情
 * （用户手动刷新往往就是想看干净的列表）。
 */
async function applyRouteQuery(): Promise<void> {
  const from = typeof route.query.from === 'string' ? route.query.from : '';
  const poId = typeof route.query.po_id === 'string' ? route.query.po_id : '';
  const inId = typeof route.query.in_id === 'string' ? route.query.in_id : '';
  if (!poId && !inId) {
    return;
  }
  await router.replace({ path: route.path, query: {} });
  if (from === 'po' && poId) {
    await openFromPurchaseOrder(poId);
    return;
  }
  if (inId) {
    await loadDetail(inId);
  }
}
</script>

<style scoped lang="less">
.stockin-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 工具栏 ==================== */

.tool-card {
  flex: 0 0 auto;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__status {
  width: 140px;
}

.tool-row__supplier,
.tool-row__warehouse {
  width: 180px;
}

.tool-row__no {
  width: 210px;
}

/* D4/O3：来源采购单号筛选框 */
.tool-row__po {
  width: 190px;
}

/* ==================== 列表 ==================== */

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert {
  margin-bottom: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.money {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  color: var(--td-text-color-primary);
}

.num {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.status-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
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

/* ==================== 表单 ==================== */

.dlg-alert {
  margin-bottom: 10px;
}

.form-row {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
}

.form-row__col {
  flex: 1 1 240px;
  min-width: 200px;
}

.disabled-radio {
  display: inline-flex;
  color: var(--td-text-color-disabled);
  cursor: not-allowed;
}

.items {
  margin-top: 4px;
}

.items__head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 8px;
}

.items__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.items__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.items__grid {
  display: grid;
  grid-template-columns: minmax(160px, 2fr) 96px 120px 96px 130px 110px 40px;
  gap: 8px;
  align-items: start;
}

.items__grid--head {
  padding: 0 0 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.items__row {
  padding: 8px 0;
  border-top: 1px solid var(--td-component-stroke);
}

.items__cell {
  min-width: 0;
}

.items__cell--num {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  height: 32px;
  font-size: 13px;
}

.items__cell--op {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 32px;
}

.ta-right {
  text-align: right;
}

.cell-error {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
}

/* D4/F2：数量下方的待收上限提示（订单 / 已收 / 待收） */
.cell-hint {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.4;
  color: var(--td-text-color-placeholder);
}

.totals {
  display: flex;
  align-items: baseline;

  justify-content: flex-end;
  gap: 20px;
  flex-wrap: wrap;
  margin-top: 12px;
  padding: 12px 16px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.totals__item strong {
  margin-left: 6px;
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  font-size: 15px;
  color: var(--td-text-color-primary);
}

.totals__item--strong strong {
  color: var(--td-brand-color);
}

.dlg-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 16px;
}

/* ==================== 过账弹窗 ==================== */

.post-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.post-kv {
  display: flex;
  align-items: baseline;
  gap: 10px;
  font-size: 13px;
}

.post-kv__label {
  flex: 0 0 56px;
  color: var(--td-text-color-placeholder);
}

.post-kv__value {
  color: var(--td-text-color-primary);
}

.post-invoice {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 4px;
  border: 1px solid var(--td-component-stroke);
}

.post-invoice__row--disabled {
  color: var(--td-text-color-disabled);
}

.post-invoice__inner {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}

.post-invoice__desc {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.post-warn {
  border-radius: 4px;
}

.post-warn__title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
}

.post-warn__list {
  margin: 6px 0 0;
  padding-left: 20px;
  font-size: 13px;
  line-height: 22px;
}

/* ==================== 详情抽屉 ==================== */

.drawer-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 0;
  color: var(--td-text-color-placeholder);
}

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 12px;
}

.detail__no {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 20px;
  padding: 12px 16px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
}

.detail-field {
  display: flex;
  gap: 8px;
  font-size: 13px;
}

.detail-field--wide {
  grid-column: 1 / -1;
}

.detail-field__label {
  flex: 0 0 72px;
  color: var(--td-text-color-placeholder);
}

.detail-field__value {
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.detail__summary {
  margin: 10px 0 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.section-title {
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}
</style>
