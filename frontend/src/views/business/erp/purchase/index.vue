<template>
  <div class="purchase-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-purchase-create"
          @click="openCreateDialog"
        >
          <template #icon><t-icon name="add" /></template>
          新建采购单
        </t-button>
        <t-button variant="outline" data-testid="btn-purchase-refresh" @click="refresh">
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
          data-testid="purchase-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.supplier_id"
          class="tool-row__supplier"
          :options="supplierOptions"
          placeholder="全部供应商"
          clearable
          filterable
          data-testid="purchase-filter-supplier"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__no"
          placeholder="采购单号 / 备注（模糊）"
          clearable
          data-testid="purchase-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-purchase-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-purchase-reset" @click="handleReset">重置</t-button>
      </div>
      <!-- O4 列表记忆：恢复筛选是静默行为，必须给用户可感知的提示 + 一键清空 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="purchase-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">采购订单</span>
          <span class="panel-head__meta" data-testid="purchase-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">金额列按「价税合计」展示，单据口径见单号旁的徽标</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="purchase-load-error"
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
        data-testid="purchase-table"
        @page-change="handlePageChange"
      >
        <template #order_no="{ row }">
          <div class="no-cell">
            <span class="mono" :data-testid="`purchase-no-${row.order_no}`">{{ row.order_no }}</span>
            <!-- ★ PRD §3.1.4：口径徽标常驻（列表也标，避免两人对着同一单说不同数） -->
            <t-tag size="small" variant="outline" :theme="priceModeTheme(row.price_mode)">
              {{ priceModeLabel(row.price_mode) }}
            </t-tag>
          </div>
        </template>

        <template #supplier_id="{ row }">{{ supplierNameOf(row.supplier_id) }}</template>

        <template #order_date="{ row }">
          <span class="muted">{{ shortDate(row.order_date) }}</span>
        </template>

        <!-- ★ 金额列必须标「(价税合计)」 -->
        <template #total_with_tax="{ row }">
          <span class="money" :data-testid="`purchase-total-${row.order_no}`">{{ formatMoney(row.total_with_tax) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="purchaseStatusTheme(row.status)" :data-testid="`purchase-status-${row.order_no}`">
            {{ purchaseStatusLabel(row.status) }}
          </t-tag>
          <!-- P-FLOW-3A：审批中点名审批单号（O5：报文/界面都要能直接照着找） -->
          <span v-if="row.status === 'SUBMITTED' && row.approval_instance_no" class="approval-no">
            {{ row.approval_instance_no }}
          </span>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-view-${row.order_no}`" @click="openDetail(row)">查看</t-link>
            <t-link
              v-if="actionsOf(row).edit"
              theme="primary"
              :data-testid="`btn-edit-${row.order_no}`"
              @click="openEditDialog(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="actionsOf(row).submit"
              theme="primary"
              :data-testid="`btn-submit-${row.order_no}`"
              @click="confirmSubmit(row)"
            >
              提交审批
            </t-link>
            <t-link
              v-if="actionsOf(row).close"
              theme="default"
              :data-testid="`btn-close-${row.order_no}`"
              @click="confirmClose(row)"
            >
              关闭
            </t-link>
            <t-link
              v-if="actionsOf(row).cancel"
              theme="danger"
              :data-testid="`btn-cancel-${row.order_no}`"
              @click="confirmCancel(row)"
            >
              取消
            </t-link>
            <t-link
              v-if="actionsOf(row).remove"
              theme="danger"
              :data-testid="`btn-remove-${row.order_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="purchase-empty">
            <t-icon name="file-copy" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的采购单' : '还没有采购单' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个条件再查，或点右侧「重置」回全量' : '点左上角「新建采购单」录入第一张' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      width="960px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="purchase-form-dialog"
    >
      <template #header>
        <div class="dlg-head">
          <span class="dlg-head__title">{{ form.id ? '编辑采购单' : '新建采购单' }}</span>
          <!-- ★★ 口径徽标：单据头常驻，点击切换（PRD §3.1.4 硬要求） -->
          <t-tooltip :content="priceModeTip(form.price_mode)">
            <button
              type="button"
              class="mode-badge"
              :class="`mode-badge--${form.price_mode.toLowerCase()}`"
              data-testid="purchase-mode-badge"
              @click="handleSwitchMode"
            >
              <t-icon name="swap" size="14px" />
              <span class="mode-badge__text" data-testid="purchase-mode-text">{{ priceModeLabel(form.price_mode) }}</span>
            </button>
          </t-tooltip>
        </div>
      </template>

      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <div class="mode-note" data-testid="purchase-mode-note">
        当前按<strong>{{ priceModeLabel(form.price_mode) }}</strong>录入：单价列填{{ priceColumnTitle(form.price_mode) }}，
        金额列「{{ AMOUNT_COLUMN_TITLE }}」由系统按各行税率实时计算（提交后以服务端计算为准）。
      </div>

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
              data-testid="purchase-supplier"
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
              data-testid="purchase-warehouse"
            />
          </t-form-item>
        </div>

        <div class="form-row">
          <t-form-item
            label="下单日期"
            name="order_date"
            class="form-row__col"
            :status="formErrors.order_date ? 'error' : undefined"
            :tips="formErrors.order_date"
          >
            <t-date-picker
              v-model="form.order_date"
              value-type="YYYY-MM-DD"
              placeholder="选择日期"
              data-testid="purchase-date"
            />
          </t-form-item>

          <t-form-item
            label="期望到货"
            name="expect_date"
            class="form-row__col"
            :status="formErrors.expect_date ? 'error' : undefined"
            :tips="formErrors.expect_date"
          >
            <t-date-picker
              v-model="form.expect_date"
              value-type="YYYY-MM-DD"
              placeholder="可空"
              clearable
              data-testid="purchase-expect-date"
            />
          </t-form-item>

          <t-form-item
            label="账期(天)"
            name="credit_days"
            class="form-row__col form-row__col--narrow"
            :status="formErrors.credit_days ? 'error' : undefined"
            :tips="formErrors.credit_days"
          >
            <t-input-number v-model="form.credit_days" :min="0" :max="3650" theme="normal" data-testid="purchase-credit-days" />
          </t-form-item>
        </div>

        <t-form-item label="备注" name="remark" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark">
          <t-input
            v-model="form.remark"
            :maxlength="MAX_REMARK"
            placeholder="可空，如：季度备货"
            data-testid="purchase-remark"
          />
        </t-form-item>

        <!-- ---------------- 明细 ---------------- -->
        <div class="items">
          <div class="items__head">
            <span class="items__title">明细</span>
            <span class="items__hint">{{ lineCountText(form.items.length) }}，单价按「{{ priceColumnTitle(form.price_mode) }}」填写</span>
          </div>

          <div class="items__grid items__grid--head">
            <span>物料</span>
            <span>数量</span>
            <span>{{ priceColumnTitle(form.price_mode) }}</span>
            <span>税率</span>
            <span class="ta-right">{{ AMOUNT_COLUMN_TITLE }}</span>
            <span class="ta-right">其中税额</span>
            <span />
          </div>

          <div
            v-for="(item, index) in form.items"
            :key="item.key"
            class="items__row"
            :data-testid="`purchase-item-${index}`"
          >
            <div class="items__grid">
              <div class="items__cell">
                <t-select
                  v-model="item.material_id"
                  :options="materialOptions"
                  placeholder="选择物料"
                  filterable
                  :data-testid="`purchase-item-material-${index}`"
                />
                <p v-if="rowErrors(index).material_id" class="cell-error">{{ rowErrors(index).material_id }}</p>
              </div>

              <div class="items__cell">
                <t-input-number
                  v-model="item.qty"
                  :min="0"
                  :max="9999999"
                  theme="normal"
                  :data-testid="`purchase-item-qty-${index}`"
                />
                <p v-if="rowErrors(index).qty" class="cell-error">{{ rowErrors(index).qty }}</p>
              </div>

              <div class="items__cell">
                <!-- 不设 `decimal-places`：单价列 NUMERIC(18,6)，但锁死 6 位会让「2」显示成「2.000000」，
                     作业员会以为填错了。自由输入，6 位上限由 validatePurchaseForm 兜 -->
                <t-input-number
                  v-model="item.price"
                  :min="0"
                  theme="normal"
                  :data-testid="`purchase-item-price-${index}`"
                />
                <p v-if="rowErrors(index).price" class="cell-error">{{ rowErrors(index).price }}</p>
              </div>

              <div class="items__cell">
                <t-select
                  v-model="item.tax_rate"
                  :options="taxRateOptions"
                  :data-testid="`purchase-item-tax-${index}`"
                />
                <p v-if="rowErrors(index).tax_rate" class="cell-error">{{ rowErrors(index).tax_rate }}</p>
              </div>

              <div class="items__cell items__cell--num">
                <span class="money" :data-testid="`purchase-item-amount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(form.price_mode, item).total) }}
                </span>
              </div>

              <div class="items__cell items__cell--num">
                <span class="muted" :data-testid="`purchase-item-taxamount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(form.price_mode, item).tax) }}
                </span>
              </div>

              <div class="items__cell items__cell--op">
                <t-tooltip content="删除该行">
                  <t-button
                    variant="text"
                    shape="square"
                    :disabled="form.items.length <= 1"
                    :data-testid="`purchase-item-remove-${index}`"
                    @click="removeItem(index)"
                  >
                    <template #icon><t-icon name="delete" /></template>
                  </t-button>
                </t-tooltip>
              </div>
            </div>
          </div>

          <t-button variant="dashed" block data-testid="btn-purchase-add-item" @click="addItem">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
          <p v-if="formErrors.items" class="cell-error">{{ formErrors.items }}</p>
        </div>

        <!-- ---------------- 合计 ---------------- -->
        <div class="totals" data-testid="purchase-totals">
          <span class="totals__item">
            合计价款 <strong data-testid="purchase-total-amount">{{ formatMoney(totals.amount) }}</strong>
          </span>
          <span class="totals__item">
            税额 <strong data-testid="purchase-total-tax">{{ formatMoney(totals.tax) }}</strong>
          </span>
          <span class="totals__item totals__item--strong">
            价税合计 <strong data-testid="purchase-total-with-tax">{{ formatMoney(totals.total) }}</strong>
          </span>
        </div>

        <!-- O1 表单记忆提示：新建时带出了上次的供应商 / 仓库，说一声并留「重置」出口 -->
        <FormMemoryHint
          v-if="!form.id"
          :labels="memoryLabels"
          testid="purchase-form-memory"
          @reset="handleResetMemory"
        />

        <div class="dlg-footer">
          <t-button variant="outline" data-testid="btn-purchase-cancel" @click="formVisible = false">取消</t-button>
          <t-button
            v-privilege="[form.id ? PERM.update : PERM.create]"
            variant="outline"
            theme="primary"
            :loading="saving"
            data-testid="btn-purchase-save-draft"
            @click="save('draft')"
          >
            保存草稿
          </t-button>
          <t-button
            v-privilege="[form.id ? PERM.update : PERM.create]"
            theme="primary"
            :loading="submitting"
            data-testid="btn-purchase-submit"
            @click="save('submit')"
          >
            提交
          </t-button>
        </div>
      </t-form>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="采购单详情"
      size="720px"
      :footer="false"
      data-testid="purchase-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail">
        <div class="detail__head">
          <span class="detail__no mono">{{ detail.order_no }}</span>
          <t-tag size="small" :theme="purchaseStatusTheme(detail.status)">{{ purchaseStatusLabel(detail.status) }}</t-tag>
          <!-- 详情头同样常驻口径徽标 -->
          <t-tag size="small" variant="outline" :theme="priceModeTheme(detail.price_mode)" data-testid="purchase-detail-mode">
            {{ priceModeLabel(detail.price_mode) }}
          </t-tag>
          <!-- P-FLOW-3A：审批进度（审批中点名单号，点开去审批单详情看时间线） -->
          <t-link
            v-if="detail.approval_instance_id"
            theme="primary"
            data-testid="purchase-detail-approval"
            @click="openApprovalInstance(detail.approval_instance_id)"
          >
            {{ detail.status === 'SUBMITTED' ? '审批中' : '审批单' }}
            {{ detail.approval_instance_no ?? detail.approval_instance_id }}
          </t-link>
          <!-- D4/F1：一键生成收货单（条件 = 状态可收 且 还有待收行，口径全部来自后端） -->
          <span class="detail__spacer" />
          <t-button
            v-if="pending?.can_receive"
            size="small"
            theme="primary"
            data-testid="btn-purchase-gen-stockin"
            @click="goCreateStockIn"
          >
            <template #icon><t-icon name="add" /></template>
            生成收货单
          </t-button>
          <t-tooltip v-else-if="pending && !pending.can_receive" :content="pending.blocked_reason ?? ''">
            <t-button size="small" variant="outline" disabled data-testid="btn-purchase-gen-stockin-disabled">
              生成收货单
            </t-button>
          </t-tooltip>
        </div>

        <div class="detail__grid">
          <div class="detail-field"><span class="detail-field__label">供应商</span><span class="detail-field__value">{{ supplierNameOf(detail.supplier_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">收货仓库</span><span class="detail-field__value">{{ warehouseNameOf(detail.warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">下单日期</span><span class="detail-field__value">{{ detail.order_date }}</span></div>
          <div class="detail-field"><span class="detail-field__label">期望到货</span><span class="detail-field__value">{{ dash(detail.expect_date) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">账期</span><span class="detail-field__value">{{ detail.credit_days }} 天</span></div>
          <div class="detail-field"><span class="detail-field__label">币种</span><span class="detail-field__value">{{ detail.currency_code }}</span></div>
          <div class="detail-field"><span class="detail-field__label">合计数量</span><span class="detail-field__value">{{ formatQty(detail.total_qty) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价款</span><span class="detail-field__value">{{ formatMoney(detail.total_amount) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">税额</span><span class="detail-field__value">{{ formatMoney(detail.total_tax) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价税合计</span><span class="detail-field__value money">{{ formatMoney(detail.total_with_tax) }}</span></div>
          <div class="detail-field detail-field--wide"><span class="detail-field__label">备注</span><span class="detail-field__value">{{ dash(detail.remark) }}</span></div>
        </div>

        <!-- D4/F6：收货进度（已收 X / 订单 Y）+ 已生成的收货单可反查 -->
        <div v-if="pending" class="section-title" data-testid="purchase-receive-progress">
          收货进度：已收 {{ formatQty(pendingTotal.received) }} / 订单 {{ formatQty(pendingTotal.ordered) }}
          <span class="muted">（待收 {{ formatQty(pendingTotal.pending) }}，{{ pending.pending_line_count }} 行未收完）</span>
        </div>
        <div v-if="pending && pending.stock_ins.length > 0" class="po-stockins" data-testid="purchase-generated-stockins">
          <span class="po-stockins__label">已生成收货单：</span>
          <t-link
            v-for="stockIn in pending.stock_ins"
            :key="stockIn.id"
            theme="primary"
            class="po-stockins__item mono"
            :data-testid="`purchase-stockin-link-${stockIn.in_no}`"
            @click="goStockIn(stockIn.id)"
          >
            {{ stockIn.in_no }}
            <span class="muted">（{{ stockIn.status_label }}{{ stockIn.posted ? '' : '·未过账' }}）</span>
          </t-link>
        </div>
        <p v-else-if="pending && pending.blocked_reason" class="muted po-blocked" data-testid="purchase-receive-blocked">
          {{ pending.blocked_reason }}
        </p>

        <div class="section-title">明细（{{ detailItems.length }} 条）</div>
        <t-table row-key="id" size="small" :data="detailItems" :columns="detailColumns" data-testid="purchase-detail-items">
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }">{{ formatQty(row.qty) }}</template>
          <template #price="{ row }">{{ formatMoney(row.price) }}</template>
          <template #tax_rate="{ row }">{{ formatQty(row.tax_rate) }}%</template>
          <template #amount="{ row }">{{ formatMoney(row.amount) }}</template>
          <template #tax_amount="{ row }">{{ formatMoney(row.tax_amount) }}</template>
          <template #with_tax_amount="{ row }">{{ formatMoney(row.with_tax_amount) }}</template>
          <template #received_qty="{ row }">{{ formatQty(row.received_qty) }}</template>
          <!-- D4/F2：待收 = 订单 − 已收（只算已过账收货单），0 显示「已收满」 -->
          <template #pending_qty="{ row }">
            <span :data-testid="`purchase-pending-${row.line_no}`">
              {{ formatQty(pendingQtyOf(row.id)) }}
            </span>
          </template>
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
  createPurchaseOrder,
  deletePurchaseOrder,
  fetchMaterialPage,
  fetchPurchaseOrderDetail,
  fetchPurchaseOrderItems,
  fetchPurchaseOrderPage,
  fetchPurchaseOrderPendingItems,
  fetchSupplierOptions,
  fetchWarehousePage,
  submitPurchaseOrderApproval,
  updatePurchaseOrder,
  updatePurchaseOrderStatus,
  type MaterialRow,
  type PendingReceiveRes,
  type PriceMode,
  type PurchaseOrderItemLine,
  type PurchaseOrderRow,
  type Supplier,
  type Warehouse,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import FormMemoryHint from '@/components/FormMemoryHint.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { pickFields, resolveDefaults } from '@/composables/memory-core';
import { useFormMemory } from '@/composables/useFormMemory';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import {
  AMOUNT_COLUMN_TITLE,
  MAX_LINES,
  MAX_REMARK,
  PURCHASE_PAGE_SIZE,
  PURCHASE_STATUS_FILTER_OPTIONS,
  availablePurchaseActions,
  buildPurchasePayload,
  buildPurchaseQuery,
  buildStatusPayload,
  computeTotals,
  dash,
  detailToForm,
  emptyPurchaseFilter,
  emptyPurchaseForm,
  emptyPurchaseItem,
  formatMoney,
  formatQty,
  isPurchaseFiltered,
  lineAmountsOfItem,
  lineCountText,
  priceColumnTitle,
  priceModeLabel,
  priceModeTheme,
  priceModeTip,
  purchaseStatusLabel,
  purchaseStatusTheme,
  shortDate,
  shortId,
  switchConfirmMessage,
  switchPriceMode,
  toNumber,
  validatePurchaseForm,
  type PurchaseFilterState,
  type PurchaseFormErrors,
  type PurchaseFormState,
  type PurchaseItemForm,
} from './purchase-logic';

/** 本页权限码（与 V0141 / V0146 的权限种子逐一对应；`SUPER_ADMIN` 走短路） */
const PERM = {
  list: 'business:erp:purchase-order:list',
  create: 'business:erp:purchase-order:create',
  update: 'business:erp:purchase-order:update',
  status: 'business:erp:purchase-order:status',
  remove: 'business:erp:purchase-order:delete',
} as const;

/** 主数据下拉一次拉多少（种子规模下够用；上千条应该走远端搜索） */
const OPTION_LIMIT = 200;

/** 表格列（`colKey` 与 `PurchaseOrderRow` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  { colKey: 'order_no', title: '采购单号', width: 250 },
  { colKey: 'supplier_id', title: '供应商', minWidth: 160, ellipsis: true },
  { colKey: 'order_date', title: '日期', width: 100 },
  // ★ 金额列标题带「(价税合计)」—— PRD 硬要求，避免两人对着同一单说不同数
  { colKey: 'total_with_tax', title: AMOUNT_COLUMN_TITLE, width: 150, align: 'right' },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right' },
];

/** 详情抽屉里的明细列（金额列同样标「价税合计」） */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 50, align: 'center' },
  { colKey: 'material_id', title: '物料', minWidth: 150, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 80, align: 'right' },
  { colKey: 'price', title: '不含税单价', width: 110, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 70, align: 'right' },
  { colKey: 'amount', title: '不含税金额', width: 110, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 90, align: 'right' },
  { colKey: 'with_tax_amount', title: AMOUNT_COLUMN_TITLE, width: 130, align: 'right' },
  { colKey: 'received_qty', title: '已收货', width: 90, align: 'right' },
  // D4/F2：待收 = 订单 − 已收（后端按 POSTED 聚合算，前端只渲染）
  { colKey: 'pending_qty', title: '待收', width: 80, align: 'right' },
];

/** 状态下拉（`''` = 全部） */
const statusFilterOptions = PURCHASE_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

/** 税率为 0–100 的常用档位（自由输入后端也收，这里给选择更快） */
const taxRateOptions = [
  { label: '13%', value: 13 },
  { label: '9%', value: 9 },
  { label: '6%', value: 6 },
  { label: '3%', value: 3 },
  { label: '0%（免税）', value: 0 },
];

// ==================== 记忆字段（O1） ====================

/**
 * 表单记忆字段：**只记「下次新建还想用同一套默认值」的选择**。
 *
 * 供应商 / 仓库 / 口径 / 账期是「这一阵子就按这个来」的稳定偏好；
 * 金额、备注、日期都是一次性的，记了只会让下次新建带出错的数字。
 */
const MEMORY_FIELDS: readonly (keyof PurchaseFormState)[] = [
  'supplier_id',
  'warehouse_id',
  'price_mode',
  'credit_days',
];

/** 记忆字段的中文名（提示条「已带出上次的供应商、仓库」） */
const MEMORY_LABELS: Record<string, string> = {
  supplier_id: '供应商',
  warehouse_id: '仓库',
  price_mode: '金额口径',
  credit_days: '账期',
};

/** 明细行记忆：新加的一行沿用上一次填的税率（其余照系统默认） */
const LINE_MEMORY_FIELDS: readonly (keyof PurchaseItemForm)[] = ['tax_rate'];

/** 明细行记忆的中文名 */
const LINE_MEMORY_LABELS: Record<string, string> = { tax_rate: '税率' };

// ==================== 列表 ====================

/** 筛选行 */
const filter = reactive<PurchaseFilterState>(emptyPurchaseFilter());
/** 行数据 */
const rows = ref<PurchaseOrderRow[]>([]);
/** 总条数 */
const total = ref(0);
/** 当前页 */
const current = ref(1);
/** 每页条数 */
const pageSize = PURCHASE_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');

/**
 * 列表记忆（O4）。
 *
 * 构造即恢复（早于 `onMounted` 的首次加载）：筛选条件与页码直接从 sessionStorage 回填，
 * 之后改筛选走 300ms 防抖写回；点提示条上的「清空筛选」→ {@link handleClearListMemory}。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<PurchaseFilterState>({
  listName: 'erp-purchase',
  filter,
  page: current,
  pageSize,
});

/**
 * 表单记忆（O1）：记住上次新建时选的供应商 / 仓库 / 口径 / 账期。
 *
 * 优先级铁律（与 D4「采购 → 收货」带出共用同一套实现）：**源单带出 > 上次记忆 > 系统默认**。
 */
const formMemory = useFormMemory<PurchaseFormState>('erp-purchase', MEMORY_FIELDS, {
  labels: MEMORY_LABELS,
});
/** 明细行记忆：新行的税率沿用上一次（每张单核一遍税率的人有福了） */
const lineMemory = useFormMemory<PurchaseItemForm>('erp-purchase-line', LINE_MEMORY_FIELDS, {
  labels: LINE_MEMORY_LABELS,
});
/** 本次新建实际「带出」的记忆字段中文名（空 = 提示条不显示） */
const memoryLabels = ref<string[]>([]);

/** 主数据 */
const suppliers = ref<Supplier[]>([]);
const warehouses = ref<Warehouse[]>([]);
const materials = ref<MaterialRow[]>([]);

/** 是否处于筛选态 */
const filtered = computed(() => isPurchaseFiltered(filter));

/** 供应商下拉 */
const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: `${item.supplier_name}（${item.supplier_code}）`, value: item.id })),
);

/** 仓库下拉 */
const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

/** 物料下拉 */
const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

/** 分页器（服务端分页） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 供应商名称（查不到显示短 id，别留白） */
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
function actionsOf(row: PurchaseOrderRow): ReturnType<typeof availablePurchaseActions> {
  return availablePurchaseActions(row.status);
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
    const data = await fetchPurchaseOrderPage(buildPurchaseQuery(filter, page, pageSize));
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

/** 主数据（失败不挡列表：下拉为空时页面还能看单） */
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

/** 查询（回第一页） */
function handleSearch(): void {
  void loadOrders(1);
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptyPurchaseFilter());
  void loadOrders(1);
}

/**
 * 列表记忆 · 「清空筛选」（提示条上的出口）。
 *
 * 三件事一起做：条件回全量、页码回 1、**删掉 sessionStorage 里的记忆**
 * —— 只清界面不清记忆的话，下次进页面又会「恢复」回来，用户会觉得按钮没用。
 */
function handleClearListMemory(): void {
  Object.assign(filter, emptyPurchaseFilter());
  current.value = 1;
  forgetListMemory();
  void loadOrders(1);
}

/** 刷新（保留筛选与页码） */
function refresh(): void {
  void loadOrders();
  void loadBasics();
}

/** 翻页 */
function handlePageChange(pageInfo: { current?: number }): void {
  void loadOrders(pageInfo.current ?? 1);
}

// ==================== 表单 ====================

/** 弹窗可见 */
const formVisible = ref(false);
/** 保存中 */
const saving = ref(false);
/** 提交中 */
const submitting = ref(false);
/** 提交错误 */
const submitError = ref('');
/** 表单 */
const form = reactive<PurchaseFormState>(emptyPurchaseForm());
/** 表单校验结果（点过保存才提示） */
const formErrors = ref<PurchaseFormErrors>({});

/** 实时合计（与后端同公式，仅用于弹窗所见即所得） */
const totals = computed(() => computeTotals(form.price_mode, form.items));

/** 取第 index 行的错误 */
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

/** 重置表单到空态 */
function resetForm(): void {
  Object.assign(form, emptyPurchaseForm());
  formErrors.value = {};
  submitError.value = '';
}

/**
 * 新建：先铺系统默认，再按「源单 > 记忆 > 默认」盖一遍。
 *
 * `source` 给 D4 的「采购 → 收货」这类带出路径留着：源单有值的字段永远赢，
 * 源单没带到的字段才轮到上次记忆。
 */
function applyCreateDefaults(source: Partial<PurchaseFormState> = {}): void {
  const resolved = resolveDefaults<PurchaseFormState>(
    emptyPurchaseForm(),
    formMemory.recall(),
    source,
    MEMORY_FIELDS,
  );
  // 只写记忆字段：日期 / 明细 / 备注保持刚 reset 的系统默认
  Object.assign(form, pickFields(resolved, MEMORY_FIELDS));
  memoryLabels.value = formMemory.labelsOf(source);
}

/** 新建 */
function openCreateDialog(): void {
  resetForm();
  applyCreateDefaults();
  formVisible.value = true;
}

/**
 * 表单记忆 · 「重置」：清掉记忆并把记忆字段退回系统默认。
 *
 * 只动记忆字段 —— 用户手填的日期和明细行不该被一起清掉。
 */
function handleResetMemory(): void {
  formMemory.forget();
  memoryLabels.value = [];
  const fresh = emptyPurchaseForm();
  const patch: Record<string, unknown> = {};
  for (const field of MEMORY_FIELDS) {
    patch[String(field)] = fresh[field];
  }
  Object.assign(form, patch);
  MessagePlugin.success('已清空表单记忆，下次新建按系统默认');
}

/** 编辑（拉单头 + 明细，按单据口径回填） */
async function openEditDialog(row: PurchaseOrderRow): Promise<void> {
  if (!actionsOf(row).edit) {
    MessagePlugin.warning('只有草稿可以编辑');
    return;
  }
  resetForm();
  formVisible.value = true;
  loading.value = true;
  try {
    const [order, items] = await Promise.all([
      fetchPurchaseOrderDetail(row.id),
      fetchPurchaseOrderItems(row.id),
    ]);
    Object.assign(form, detailToForm(order, items));
  } catch (error) {
    formVisible.value = false;
    MessagePlugin.error(messageOf(error));
  } finally {
    loading.value = false;
  }
}

/**
 * ★ 口径切换（PRD §3.1.4）：先确认，再换 mode。
 *
 * 切换不换算单价 —— 同一串数字被重新解释（2.00 从「不含税单价」变成「含税单价」），
 * 各行金额按新口径重算（100×2×13%：226.00 → 200.00）。
 */
function handleSwitchMode(): void {
  const next: PriceMode = form.price_mode === 'EXCL' ? 'INCL' : 'EXCL';
  const dialog = DialogPlugin.confirm({
    header: '切换金额口径',
    theme: 'warning',
    body: switchConfirmMessage(next),
    confirmBtn: { content: '确认切换', theme: 'primary' },
    onConfirm: () => {
      form.price_mode = switchPriceMode(form.items, next);
      dialog.hide();
    },
  });
}

/** 加一行 */
function addItem(): void {
  if (form.items.length >= MAX_LINES) {
    MessagePlugin.warning(`明细最多 ${MAX_LINES} 行`);
    return;
  }
  const line = emptyPurchaseItem(`row-${Date.now()}-${form.items.length + 1}`);
  // 明细行记忆：新行沿用上一次填的税率（其他字段走系统默认）
  Object.assign(line, pickFields(lineMemory.recall(), LINE_MEMORY_FIELDS));
  form.items.push(line);
}

/** 删一行（至少留一行） */
function removeItem(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
}

/**
 * 保存。
 *
 * - `draft`：只建单 / 改单（后端建单一律落 `DRAFT`）；
 * - `submit`：先建单 / 改单，再 `PATCH status=SUBMITTED`。
 *
 * 两步失败时文案分开说：「单子已建好但提交失败」不能让用户以为没保存。
 */
async function save(mode: 'draft' | 'submit'): Promise<void> {
  formErrors.value = validatePurchaseForm(form);
  if (Object.keys(formErrors.value).length > 0) {
    MessagePlugin.warning('表单还有 ' + Object.keys(formErrors.value).length + ' 处需要修正');
    return;
  }

  const isEdit = Boolean(form.id);
  if (mode === 'draft') {
    saving.value = true;
  } else {
    submitting.value = true;
  }
  submitError.value = '';
  try {
    const payload = buildPurchasePayload(form);
    const detail = isEdit ? await updatePurchaseOrder(form.id, payload) : await createPurchaseOrder(payload);
    const orderId = detail.order.id;
    const orderNo = detail.order.order_no;

    if (mode === 'submit') {
      try {
        // P-FLOW-3A：提交 = 发起审批（不是直接改状态）。后端按 po_approve 流程分流。
        await submitPurchaseOrderApproval(orderId);
        MessagePlugin.success(`${orderNo} 已提交审批`);
      } catch (error) {
        // 单子已经保存成功，只有提交失败 —— 不能假装整体失败
        MessagePlugin.warning(`${orderNo} 已保存为草稿，但提交审批失败：${messageOf(error)}`);
      }
    } else {
      MessagePlugin.success(`${orderNo} 已保存为草稿`);
    }

    formVisible.value = false;
    // 保存成功才写记忆：用户改了「这次用别的供应商」→ 下次默认就是这个新选择
    formMemory.remember(form);
    lineMemory.remember({ tax_rate: form.items[form.items.length - 1]?.tax_rate ?? 13 });
    memoryLabels.value = [];
    await loadOrders(isEdit ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
    submitting.value = false;
  }
}

// ==================== 状态流转 ====================

/** 打状态接口 + 刷新 */
async function patchStatus(row: PurchaseOrderRow, status: 'SUBMITTED' | 'CLOSED' | 'CANCELLED', reason?: string): Promise<void> {
  try {
    await updatePurchaseOrderStatus(row.id, buildStatusPayload(status, reason));
    MessagePlugin.success(`${row.order_no} 状态已更新`);
    await loadOrders();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

/** 提交审批（DRAFT → SUBMITTED，发起 po_approve 审批流；高危动作要二次确认，O8） */
function confirmSubmit(row: PurchaseOrderRow): void {
  if (!actionsOf(row).submit) {
    MessagePlugin.warning('当前状态不能提交');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '提交审批',
    theme: 'info',
    body: `${row.order_no}（含税 ${formatMoney(row.total_with_tax)}）将提交审批：按金额自动分流到部门主管 / 采购经理 / 采购经理+总经理会签。提交后单据锁定，等审批结论。确认提交？`,
    confirmBtn: { content: '确认提交', theme: 'primary' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await submitPurchaseOrderApproval(row.id);
        MessagePlugin.success(`${row.order_no} 已提交审批`);
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

/** 关闭（SUBMITTED → CLOSED，需确认：关闭后不可再收货） */
function confirmClose(row: PurchaseOrderRow): void {
  const dialog = DialogPlugin.confirm({
    header: '关闭采购单',
    theme: 'warning',
    body: `${row.order_no} 将被关闭：关闭后不能再收货，也不能再编辑。确认关闭？`,
    confirmBtn: { content: '确认关闭', theme: 'primary' },
    onConfirm: async () => {
      dialog.hide();
      await patchStatus(row, 'CLOSED');
    },
  });
}

/** 取消（DRAFT / SUBMITTED → CANCELLED，需填原因） */
function confirmCancel(row: PurchaseOrderRow): void {
  if (!actionsOf(row).cancel) {
    MessagePlugin.warning('当前状态不能取消');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '取消采购单',
    theme: 'danger',
    body: `${row.order_no} 将变为「已取消」，取消后不可恢复（要改数量请走作废红冲流程）。确认取消？`,
    confirmBtn: { content: '确认取消', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      await patchStatus(row, 'CANCELLED', '页面取消');
    },
  });
}

// ==================== 详情 ====================
/**
 * 打开审批单详情（P-FLOW-3A）：审批进度时间线在流程实例页，单据页只给入口。
 *
 * 传 **实例 id**（不是单号）：路由是 `/flow/instance/:id`，用单号拼会 404。
 */
function openApprovalInstance(instanceId: string | null): void {
  if (!instanceId) {
    return;
  }
  void router.push({ path: `/flow/instance/${instanceId}` });
}
/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情加载中 */
const detailLoading = ref(false);
/** 详情错误 */
const detailError = ref('');
/** 单头 */
const detail = ref<PurchaseOrderRow | null>(null);
/** 明细 */
const detailItems = ref<PurchaseOrderItemLine[]>([]);

/** 打开详情抽屉 */
async function openDetail(row: PurchaseOrderRow): Promise<void> {
  await loadDetail(row.id);
}

// ==================== D4：采购单 → 一键生成收货单 ====================

const router = useRouter();
const route = useRoute();

/** 收货入库页路径（`business/erp/stock-in`，别名 `stockin`） */
const STOCK_IN_PATH = '/business/erp/stock-in';

/** 待收明细（「生成收货单」按钮 / 收货进度 / 已生成收货单都读它；加载失败 = `null`，详情不整块失败） */
const pending = ref<PendingReceiveRes | null>(null);

/** 待收汇总（已收 X / 订单 Y / 待收 Z） */
const pendingTotal = computed(() => {
  const total = { ordered: 0, received: 0, pending: 0 };
  for (const item of pending.value?.items ?? []) {
    total.ordered += toNumber(item.ordered_qty);
    total.received += toNumber(item.received_qty);
    total.pending += toNumber(item.pending_qty);
  }
  return total;
});

/** 采购明细行 → 待收数量（拿不到就显示 0，不让表格出现 `NaN`） */
function pendingQtyOf(itemId: string): number {
  const line = pending.value?.items.find((item) => item.item_id === itemId);
  return toNumber(line?.pending_qty);
}

/** 按 id 开详情抽屉（路由带 `po_id` 跳过来时用） */
async function loadDetail(id: string): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  detailItems.value = [];
  pending.value = null;
  try {
    const [order, items] = await Promise.all([
      fetchPurchaseOrderDetail(id),
      fetchPurchaseOrderItems(id),
    ]);
    detail.value = order;
    detailItems.value = items;
  } catch (error) {
    detailError.value = messageOf(error);
    return;
  } finally {
    detailLoading.value = false;
  }
  // 待收明细单独兜底：拉不到（权限 / 网络）只影响「生成收货单」按钮，详情照常看
  try {
    pending.value = await fetchPurchaseOrderPendingItems(id);
  } catch {
    pending.value = null;
  }
}

/**
 * 「生成收货单」：跳到收货页并带上采购单 id（收货页自动带出待收明细）。
 *
 * 生成的**只是草稿**（硬红线 7）：过账必须回收货页由用户确认。
 */
async function goCreateStockIn(): Promise<void> {
  const order = detail.value;
  if (!order || !pending.value?.can_receive) {
    MessagePlugin.warning(pending.value?.blocked_reason ?? '这张采购单没有可收的待收明细');
    return;
  }
  detailVisible.value = false;
  await router.push({ path: STOCK_IN_PATH, query: { from: 'po', po_id: order.id } });
}

/** 「已生成收货单 IN-xxx」点击 → 跳收货页并直开该单详情（F8 双向可跳） */
function goStockIn(stockInId: string): void {
  void router.push({ path: STOCK_IN_PATH, query: { in_id: stockInId } });
}

/**
 * 路由参数：从收货单详情点「来源采购单」跳进来（`?po_id=`）→ 直接开这张采购单的详情抽屉。
 *
 * 消费后立刻 `replace` 掉，刷新不会重复弹。
 */
async function applyRouteQuery(): Promise<void> {
  const poId = typeof route.query.po_id === 'string' ? route.query.po_id : '';
  if (!poId) {
    return;
  }
  await router.replace({ path: route.path, query: {} });
  await loadDetail(poId);
}

/** 删除（DRAFT 软删） */
async function confirmRemove(row: PurchaseOrderRow): Promise<void> {
  const dialog = DialogPlugin.confirm({
    header: '删除采购单',
    theme: 'danger',
    body: `${row.order_no} 将被软删除（列表不再显示）。确认删除？`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deletePurchaseOrder(row.id);
        MessagePlugin.success('已删除');
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}
onMounted(() => {
  void loadBasics();
  // 用 `current.value` 而不是写死 1：列表记忆恢复的页码要在首次加载就生效
  void loadOrders(current.value);
  // D4：从收货单详情跳「来源采购单」（`?po_id=`）→ 直开详情
  void applyRouteQuery();
});
</script>

<style scoped lang="less">
.purchase-page {
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

.tool-row__supplier {
  width: 200px;
}

.tool-row__no {
  width: 220px;
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

.no-cell {
  display: flex;
  align-items: center;
  gap: 8px;
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

/* ==================== 弹窗头 / 口径徽标 ==================== */

.dlg-head {
  display: flex;
  align-items: center;
  gap: 12px;
}

.dlg-head__title {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

/*
 * ★ 口径徽标：常驻、可点、颜色只用 TDesign 变量。
 * 不含税 = 品牌色描边；含税 = 警告色描边（切换是「有后果的操作」，视觉上要跳一下）。
 */
.mode-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 10px;
  border-radius: 12px;
  border: 1px solid var(--td-brand-color);
  background: var(--td-brand-color-light);
  color: var(--td-brand-color);
  font-size: 12px;
  line-height: 20px;
  cursor: pointer;
  transition: background-color 0.2s;

  &:hover {
    background: var(--td-brand-color-focus);
  }

  &--incl {
    border-color: var(--td-warning-color);
    background: var(--td-warning-color-light);
    color: var(--td-warning-color);
  }
}

.mode-note {
  margin: 0 0 12px;
  padding: 8px 12px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 20px;
}

.dlg-alert {
  margin-bottom: 10px;
}

/* ==================== 表单 ==================== */

.form-row {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
}

.form-row__col {
  flex: 1 1 240px;
  min-width: 200px;
}

.form-row__col--narrow {
  flex: 0 0 150px;
  min-width: 130px;
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

/* 明细 7 列：物料 / 数量 / 单价 / 税率 / 金额(价税合计) / 其中税额 / 操作 */
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

/* ==================== 合计 ==================== */

.totals {
  display: flex;
  align-items: baseline;
  justify-content: flex-end;
  gap: 24px;
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

/* D4/F1：「生成收货单」按钮推到详情头右端 */
.detail__spacer {
  flex: 1 1 auto;
}

/* D4/F6：已生成收货单链（采购单 → 收货单） */
.po-stockins {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px 12px;
  margin: 4px 0 10px;
  font-size: 12px;
}

.po-stockins__label {
  color: var(--td-text-color-secondary);
}

.po-stockins__item {
  font-size: 12px;
}

/* D4：不能收货的原因（已收满 / 状态不可收） */
.po-blocked {
  margin: 4px 0 10px;
  font-size: 12px;
  line-height: 1.5;
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

.section-title {
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}
</style>
