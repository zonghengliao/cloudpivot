<template>
  <div class="sales-return-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.return_no"
          class="filter-row__keyword"
          placeholder="退货单号（模糊）"
          clearable
          data-testid="return-filter-no"
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
          data-testid="return-filter-status"
        />

        <t-select
          v-model="filter.customer_id"
          class="filter-row__customer"
          :options="customerOptions"
          placeholder="全部客户"
          clearable
          filterable
          data-testid="return-filter-customer"
        />

        <t-button theme="primary" data-testid="btn-return-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-return-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-return-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="
            canWrite
              ? '选一张已过账的发货单，自动带出该单的可退明细（已发 − 已退）'
              : '当前账号没有 business:erp:sales-return:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-return-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="rollback" /></template>
              从发货单新建
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">销售退货单</span>
          <span class="panel-head__meta" data-testid="return-meta">共 {{ total }} 条</span>
          <t-tag v-if="isFiltered" size="small" variant="outline" data-testid="return-filtered-tag">
            已筛选
          </t-tag>
        </div>
        <span class="panel-head__hint">
          状态机 草稿 → 已提交 → 已批准 → 待过账 → 已过账；过账同事务回库 + 冲减应收 + 红字凭证，不可撤销
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="return-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadReturns()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="return-table"
        @page-change="handlePageChange"
      >
        <template #return_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="return-no-link" @click="openDetail(row)">
            {{ row.return_no }}
          </t-link>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="customerText(row)">
            {{ customerText(row) }}
          </span>
        </template>

        <template #return_date="{ row }">
          <span class="mono">{{ row.return_date }}</span>
        </template>

        <template #return_type="{ row }">
          <t-tag size="small" variant="outline" data-testid="return-type-tag">
            {{ returnTypeLabel(row.return_type, row.return_type_label) }}
          </t-tag>
        </template>

        <template #total_with_tax="{ row }">
          <span class="mono" :data-testid="`return-amount-${row.return_no}`">
            {{ moneyText(row.total_with_tax) }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="returnStatusTheme(row.status)"
            :data-testid="`return-status-${row.return_no}`"
          >
            {{ row.status_label || returnStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #shipment_no="{ row }">
          <span v-if="row.shipment_no" class="mono">{{ row.shipment_no }}</span>
          <span v-else class="muted" title="无原发货单（其他类型退货 / 发货单已删除）">
            {{ row.shipment_id ? '发货单已删除' : '无原单' }}
          </span>
        </template>

        <!--
          操作列：**全部按状态显隐**（决定权在 `rowActions`，与后端 SALES_RETURN_EDGES 同源）。
          链路是单向的：提交(DRAFT) → 批准(SUBMITTED) → 待过账(APPROVED) → 过账(PENDING_POST)，
          作废口只有 DRAFT / SUBMITTED / APPROVED 三个。
        -->
        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="btn-return-detail" @click="openDetail(row)">
              详情
            </t-link>
            <t-link
              v-if="rowActions(row).submit"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-return-submit"
              @click="submitReturn(row)"
            >
              提交
            </t-link>
            <t-link
              v-if="rowActions(row).approve"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-return-approve"
              @click="approveReturn(row)"
            >
              批准
            </t-link>
            <t-link
              v-if="rowActions(row).pendingPost"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-return-pending-post"
              @click="enterPendingPost(row)"
            >
              待过账
            </t-link>
            <t-link
              v-if="rowActions(row).post"
              v-privilege="[PERM.status]"
              theme="success"
              data-testid="btn-return-post"
              @click="openPostDialog(row)"
            >
              过账
            </t-link>
            <t-link
              v-if="rowActions(row).cancel"
              v-privilege="[PERM.status]"
              theme="danger"
              data-testid="btn-return-cancel"
              @click="openCancelDialog(row)"
            >
              作废
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="empty-block" data-testid="return-empty">{{ emptyListText(filter) }}</div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建（从原发货单带出可退明细） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      header="从发货单新建退货单"
      :width="1180"
      :confirm-btn="{ content: '创建退货单', loading: submitting, disabled: submitting }"
      :cancel-btn="{ disabled: submitting }"
      :close-on-overlay-click="false"
      data-testid="return-form-dialog"
      @confirm="submitForm"
    >
      <div class="form-body">
        <t-form label-width="96px" class="form-head">
          <t-row :gutter="[16, 0]">
            <t-col :span="6">
              <t-form-item label="原发货单" required-mark>
                <t-select
                  v-model="form.shipment_id"
                  :options="shipmentOptions"
                  filterable
                  :loading="shipmentLoading"
                  placeholder="只列已过账的发货单"
                  data-testid="return-form-shipment"
                  @change="handleShipmentChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="退货类型" required-mark>
                <t-select
                  v-model="form.return_type"
                  :options="typeOptions"
                  placeholder="质量 / 错发 / 其他"
                  data-testid="return-form-type"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="退回仓库" required-mark>
                <t-select
                  v-model="form.warehouse_id"
                  :options="warehouseOptions"
                  filterable
                  placeholder="退货回哪个仓库"
                  data-testid="return-form-warehouse"
                  @change="handleWarehouseChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="退货日期" required-mark>
                <t-date-picker
                  v-model="form.return_date"
                  value-type="YYYY-MM-DD"
                  format="YYYY-MM-DD"
                  :clearable="false"
                  data-testid="return-form-date"
                />
              </t-form-item>
            </t-col>
            <t-col :span="12">
              <t-form-item label="备注">
                <t-input
                  v-model="form.remark"
                  :maxlength="RETURN_REMARK_MAX"
                  placeholder="退货原因说明（选填，会写进单据）"
                  data-testid="return-form-remark"
                />
              </t-form-item>
            </t-col>
          </t-row>
        </t-form>

        <div class="section-head">
          <span class="section-head__title">可退明细（{{ form.items.length }} 行）</span>
          <span class="section-head__tip">
            选定发货单后自动带出<strong>可退（已发 − 已退）</strong>的行，数量默认「可退量」；
            改小即部分退货，删行即本次不退
          </span>
          <span class="section-head__spacer" />
          <span class="section-head__total" data-testid="return-form-total">
            预估价税合计 <b class="mono">{{ moneyText(formTotals.withTax) }}</b>
            <span class="muted">（不含税 {{ moneyText(formTotals.amount) }} + 税 {{ moneyText(formTotals.tax) }}）</span>
          </span>
        </div>

        <t-alert
          v-if="returnableError"
          theme="error"
          :message="returnableError"
          class="form-alert"
          data-testid="return-returnable-error"
        />

        <t-loading :loading="returnableLoading" size="small" show-overlay>
          <t-table
            row-key="key"
            size="small"
            :data="form.items"
            :columns="formItemColumns"
            :empty="emptyReturnableText"
            data-testid="return-form-items"
          >
            <template #line_no="{ row }">
              <span class="mono">{{ row.line_no }}</span>
            </template>
            <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
            <template #shipped="{ row }"><span class="mono">{{ formatQty(row.shipped) }}</span></template>
            <template #returned="{ row }"><span class="mono">{{ formatQty(row.returned) }}</span></template>
            <template #remaining="{ row }">
              <span class="mono">{{ formatQty(row.remaining) }}</span>
            </template>
            <template #qty="{ row }">
              <t-input-number
                v-model="row.qty"
                theme="normal"
                :min="0"
                :max="Number(row.remaining)"
                align="right"
                data-testid="return-form-qty"
              />
            </template>
            <template #location_id="{ row }">
              <t-select
                v-model="row.location_id"
                class="location-select"
                :options="locationOptions"
                clearable
                placeholder="整仓"
                data-testid="return-form-location"
              />
            </template>
            <template #line_op="{ row }">
              <t-link
                theme="danger"
                :disabled="form.items.length <= 1"
                data-testid="btn-return-remove-line"
                @click="removeLine(row.key)"
              >
                不退
              </t-link>
            </template>
          </t-table>
        </t-loading>

        <t-alert v-if="formError" theme="error" :message="formError" class="form-alert" data-testid="return-form-error" />
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="退货单详情"
      size="960px"
      :footer="false"
      data-testid="return-detail-drawer"
    >
      <div v-if="detailLoading" class="detail-loading"><t-loading :loading="true" text="加载中…" /></div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" data-testid="return-detail-error" />

      <template v-else-if="detail">
        <div class="detail-head">
          <span class="detail-head__no mono" data-testid="return-detail-no">{{ detail.return_no }}</span>
          <t-tag
            size="small"
            variant="light"
            :theme="returnStatusTheme(detail.status)"
            data-testid="return-detail-status"
          >
            {{ detail.status_label || returnStatusLabel(detail.status) }}
          </t-tag>
          <span class="detail-head__spacer" />
          <t-button
            v-if="detailActions.submit"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-return-detail-submit"
            @click="submitReturn(detail)"
          >
            提交
          </t-button>
          <t-button
            v-if="detailActions.approve"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-return-detail-approve"
            @click="approveReturn(detail)"
          >
            批准
          </t-button>
          <t-button
            v-if="detailActions.pendingPost"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-return-detail-pending-post"
            @click="enterPendingPost(detail)"
          >
            进入待过账
          </t-button>
          <t-button
            v-if="detailActions.post"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-return-detail-post"
            @click="openPostDialog(detail)"
          >
            过账
          </t-button>
          <t-button
            v-if="detailActions.cancel"
            v-privilege="[PERM.status]"
            size="small"
            theme="danger"
            variant="outline"
            data-testid="btn-return-detail-cancel"
            @click="openCancelDialog(detail)"
          >
            作废
          </t-button>
          <t-button variant="outline" size="small" data-testid="btn-return-drawer-close" @click="detailVisible = false">
            关闭
          </t-button>
        </div>

        <t-alert
          v-if="detailActionError"
          theme="error"
          class="detail-alert"
          data-testid="return-action-error"
          :message="detailActionError"
        />

        <!-- 单据自己的状态线（与列表徽标同源；作废单只点亮草稿） -->
        <div class="status-line" data-testid="return-status-line">
          <span
            v-for="(step, index) in RETURN_MAIN_CHAIN"
            :key="step.value"
            class="status-line__step"
            :class="{
              'status-line__step--done': mainChainReached(detail.status, step.value),
              'status-line__step--current': detail.status === step.value,
            }"
          >
            <span class="status-line__dot" />
            {{ step.label }}
            <span v-if="index < RETURN_MAIN_CHAIN.length - 1" class="status-line__arrow">→</span>
          </span>
        </div>

        <t-descriptions :column="2" size="small" class="detail-desc" data-testid="return-detail-desc">
          <t-descriptions-item label="客户">{{ customerText(detail) }}</t-descriptions-item>
          <t-descriptions-item label="退货类型">
            {{ returnTypeLabel(detail.return_type, detail.return_type_label) }}
          </t-descriptions-item>
          <t-descriptions-item label="原发货单">
            <span class="mono" :class="{ muted: !detail.shipment_no }">
              {{ detail.shipment_no || (detail.shipment_id ? '发货单已删除' : '无原单') }}
            </span>
          </t-descriptions-item>
          <t-descriptions-item label="原销售订单">
            <span class="mono" :class="{ muted: !detail.so_no }">
              {{ detail.so_no || (detail.so_id ? '订单已删除' : '无原单') }}
            </span>
          </t-descriptions-item>
          <t-descriptions-item label="退回仓库">{{ warehouseText(detail) }}</t-descriptions-item>
          <t-descriptions-item label="退货日期">
            <span class="mono">{{ detail.return_date }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="不含税金额">
            <span class="mono">{{ moneyText(detail.total_amount) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="税额">
            <span class="mono">{{ moneyText(detail.total_tax) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="价税合计">
            <span class="mono strong" data-testid="return-detail-total-with-tax">
              {{ moneyText(detail.total_with_tax) }}
            </span>
          </t-descriptions-item>
          <t-descriptions-item label="创建时间">{{ formatTime(detail.created_at) }}</t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ detail.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <t-alert
          v-if="detail.status === 'POSTED'"
          theme="success"
          class="detail-alert"
          data-testid="return-posted-hint"
          :message="`已过账：三本账已回库、红字入库凭证已生成，原应收已冲减`"
        >
          <template #description>
            <ul class="post-links">
              <li>退货入库单：{{ links.stockIn }}</li>
              <li>原应收冲减：{{ links.receivable }}</li>
              <li>红字发票：{{ links.redInvoice }}</li>
            </ul>
          </template>
        </t-alert>
        <t-alert
          v-else-if="detail.status === 'CANCELLED'"
          theme="warning"
          class="detail-alert"
          data-testid="return-cancelled-hint"
          :message="'已取消（作废）：本单未过账，没有动过库存、应收与凭证'"
        />

        <!--
          审批进度：复用审批中心的 `FlowProgress`（进度只来自引擎写的节点行）。
          ⚠️ 现状：退货单尚未接入流程引擎（`approval_instance_id` 恒 `null`），
          `loadApprovalProgress` 按共享口径返回空 → 本区块不渲染。接上引擎后自动显示，
          前端无需再改（见 `views/business/erp/shared/approval-progress.ts` 的三条硬口径）。
        -->
        <template v-if="hasApprovalProgress(approvalProgress)">
          <div class="section-title">审批进度</div>
          <FlowProgress
            :nodes="approvalProgress.nodes"
            :tasks="approvalProgress.tasks"
            data-testid="return-approval-progress"
          />
        </template>

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="RETURN_ITEM_COLUMNS"
          data-testid="return-detail-items"
        >
          <template #line_no="{ row }">
            <span class="mono">{{ row.line_no }}</span>
          </template>
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }"><span class="mono">{{ formatQty(row.qty) }}</span></template>
          <template #price="{ row }"><span class="mono">{{ moneyText(row.price) }}</span></template>
          <template #amount="{ row }"><span class="mono">{{ moneyText(row.amount) }}</span></template>
          <template #tax_rate="{ row }"><span class="mono">{{ formatQty(row.tax_rate) }}%</span></template>
          <template #tax_amount="{ row }"><span class="mono">{{ moneyText(row.tax_amount) }}</span></template>
          <template #location_id="{ row }">
            <span :class="{ muted: !row.location_id }">{{ locationNameOf(row.location_id) }}</span>
          </template>
          <template #remark="{ row }">{{ row.remark || '—' }}</template>
        </t-table>
      </template>
    </t-drawer>

    <!-- ==================== ⑤ 过账确认弹窗 ==================== -->
    <t-dialog
      v-model:visible="postVisible"
      header="确认过账"
      width="620px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="return-post-dialog"
    >
      <p class="dialog-text" data-testid="return-post-text">{{ postDialogText(postTarget) }}</p>
      <t-alert
        v-if="postError"
        theme="error"
        class="dialog-error"
        data-testid="return-post-error"
        :message="postError"
      />
      <div class="dialog-footer">
        <t-button theme="primary" :loading="acting" data-testid="btn-return-post-confirm" @click="confirmPost">
          确认过账
        </t-button>
        <t-button variant="outline" data-testid="btn-return-post-cancel" @click="postVisible = false">取消</t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（SUBMITTED / APPROVED 必须写原因） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废退货单"
      width="520px"
      :confirm-btn="{ content: '作废', theme: 'danger', loading: acting }"
      :cancel-btn="{ disabled: acting }"
      data-testid="return-cancel-dialog"
      @confirm="confirmCancel"
    >
      <div class="reason-body">
        <p class="reason-tip">
          {{
            cancelNeedsReason
              ? '已提交 / 已批准的单作废会释放本单占用的可退量，必须写清原因（进状态日志与对账依据）。'
              : '草稿作废不动账、也不占用可退量，原因可选。'
          }}
        </p>
        <t-textarea
          v-model="cancelReason"
          :autosize="{ minRows: 3, maxRows: 5 }"
          :maxlength="200"
          placeholder="原因（例如：客户改单 / 退回数量填错）"
          data-testid="return-cancel-reason"
        />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 销售退货单页（SO-31）。
 *
 * # 接口（全部真接口，路径逐字对应 `backend/src/business/erp/sales_return_handler.rs`）
 *
 * | 动作 | 请求 | 权限 |
 * |------|------|------|
 * | 列表 | `GET /api/business/erp/sales-return/page` | `:list` |
 * | 详情（含明细） | `GET /api/business/erp/sales-return/{id}` | `:list` |
 * | 可退明细（从发货单带出） | `GET /api/business/erp/sales-return/returnable/{shipment_id}` | `:list` |
 * | 新建 | `POST /api/business/erp/sales-return` | `:create` |
 * | 提交 / 批准 / 待过账 / 过账 / 作废 | `PATCH /api/business/erp/sales-return/{id}/status` | `:status` |
 *
 * # 四条硬口径
 *
 * 1. **过账不是状态标记**（SO-30）：`PENDING_POST → POSTED` 在同一事务里生成退货入库单、
 *    三本账回库、红字入库凭证、冲减原应收、已开票则造红字发票 + 收入冲减凭证，
 *    并回写订单已发量与释放信用占用；页面只陈述这些，不多说一处；
 * 2. **DRAFT 不能直接过账**：边表是单向链，五个动作分开做（提交 / 批准 / 待过账 / 过账），
 *    不拼「提交并过账」的假动作；
 * 3. **作废也要按边表**：只有 `DRAFT / SUBMITTED / APPROVED → CANCELLED`；
 *    卡在待过账要作废只能先过账（边表没有 `PENDING_POST → CANCELLED`）；
 * 4. **可退量只信服务端**：`returnable/{shipment_id}` 的 `remaining` 已扣掉同一发货单上
 *    其他未作废退货单的占用，前端只在提交前做上限校验，不自己算第二遍。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  fetchCustomerPage,
  fetchLocationList,
  fetchMaterialPage,
  fetchWarehousePage,
  type Customer,
  type Location,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import { fetchShipmentPage, type Shipment } from '@/api/business/erp/shipment';
import {
  changeReturnStatus,
  createReturn,
  fetchReturnDetail,
  fetchReturnPage,
  fetchReturnableItems,
  type ReturnableItem,
  type SalesReturn,
} from '@/api/business/erp/sales-return';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import {
  EMPTY_APPROVAL_PROGRESS,
  loadApprovalProgress,
  hasApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import {
  RETURN_COLUMNS,
  RETURN_FORM_ITEM_COLUMNS,
  RETURN_ITEM_COLUMNS,
  RETURN_MAIN_CHAIN,
  RETURN_PAGE_SIZE,
  RETURN_REMARK_MAX,
  RETURN_STATUS_OPTIONS,
  RETURN_TYPE_OPTIONS,
  buildCreatePayload,
  buildReturnQuery,
  emptyListText,
  emptyReturnFilter,
  emptyReturnForm,
  isReturnFiltered,
  linesFromReturnable,
  mainChainReached,
  moneyText,
  postDialogText,
  postingLinks,
  returnStatusLabel,
  returnStatusTheme,
  returnTypeLabel,
  rowActions,
  statusNeedsReason,
  totalsOf,
  validateReturnForm,
  type ReturnFilterState,
  type ReturnFormDraft,
  type ReturnLineDraft,
} from './sales-return-query';

/** 本页权限码（与 `V0304__sales_return_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情 / 可退明细（页面入口） */
  list: 'business:erp:sales-return:list',
  /** 新建 */
  create: 'business:erp:sales-return:create',
  /** 状态流转（提交 / 批准 / 待过账 / 过账 / 作废） */
  status: 'business:erp:sales-return:status',
} as const;

const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

/** 表格列（colKey 与后端出参同名） */
const columns = RETURN_COLUMNS;
/** 状态下拉（`''` = 全部，靠 `clearable` 清） */
const statusOptions = RETURN_STATUS_OPTIONS.map((item) => ({ ...item }));
/** 退货类型下拉 */
const typeOptions = RETURN_TYPE_OPTIONS.map((item) => ({ ...item }));
/** 新建弹窗明细列 */
const formItemColumns = RETURN_FORM_ITEM_COLUMNS;

// ==================== 列表 ====================

const filter = reactive<ReturnFilterState>(emptyReturnFilter());
const rows = ref<SalesReturn[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = RETURN_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const isFiltered = computed(() => isReturnFiltered(filter));
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

async function loadReturns(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchReturnPage(buildReturnQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      await loadReturns(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `退货单加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadReturns(1);
}

function handleReset(): void {
  Object.assign(filter, emptyReturnFilter());
  current.value = 1;
  void loadReturns(1);
}

function handleRefresh(): void {
  void loadReturns(current.value);
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadReturns(pageInfo.current);
}

// ==================== 主数据下拉 ====================

const warehouses = ref<Warehouse[]>([]);
const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);
/** 表单里当前仓库的库位（`biz_location.id`；空 = 整仓） */
const locations = ref<Location[]>([]);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_code} ${item.warehouse_name}`,
    value: item.id,
  })),
);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id })),
);

const locationOptions = computed(() =>
  locations.value.map((item) => ({
    label: `${item.location_code}${item.location_name ? ` ${item.location_name}` : ''}`,
    value: item.id,
  })),
);

/** 物料下拉 / 详情里的物料名（一次拉 200 条够用） */
function materialLabel(material: MaterialRow): string {
  const spec = material.spec ? ` / ${material.spec}` : '';
  return `${material.material_code} ${material.material_name}${spec}`;
}

function materialNameOf(id: string | null | undefined): string {
  if (!id) {
    return '—';
  }
  const found = materials.value.find((item) => item.id === id);
  if (found) {
    return materialLabel(found);
  }
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

function locationNameOf(id: string | null | undefined): string {
  if (!id) {
    return '整仓';
  }
  const found = locations.value.find((item) => item.id === id);
  if (found) {
    return `${found.location_code}${found.location_name ? ` ${found.location_name}` : ''}`;
  }
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

function customerText(row: { customer_name: string | null; customer_code: string | null }): string {
  if (row.customer_name && row.customer_code) {
    return `${row.customer_name}（${row.customer_code}）`;
  }
  if (row.customer_name) {
    return row.customer_name;
  }
  return '客户已删除';
}

function warehouseText(row: { warehouse_name: string | null; warehouse_code: string | null }): string {
  if (row.warehouse_name && row.warehouse_code) {
    return `${row.warehouse_name}（${row.warehouse_code}）`;
  }
  if (row.warehouse_name) {
    return row.warehouse_name;
  }
  return '仓库已删除';
}

/** 数量显示：`12.0000` → `12`（保留 4 位小数但不显示无意义的 0） */
function formatQty(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '0';
  }
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num)) {
    return '0';
  }
  return String(Number(num.toFixed(4)));
}

/** 时间显示：RFC3339 → `yyyy-MM-dd HH:mm`（北京时间） */
function formatTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  const shifted = new Date(parsed.getTime() + 8 * 60 * 60 * 1000);
  return `${shifted.toISOString().slice(0, 10)} ${shifted.toISOString().slice(11, 16)}`;
}

/** 北京时间今天（与后端 `today_cst()` 同口径） */
function todayCst(): string {
  return new Date(Date.now() + 8 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

async function loadOptions(): Promise<void> {
  // 三个主数据互不依赖，一起发；任何一个失败都不该让页面整体失败
  const [warehouseResult, customerResult, materialResult] = await Promise.allSettled([
    fetchWarehousePage({ status: 1, page: 1, page_size: 200 }),
    fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: 200 }),
    fetchMaterialPage({ status: 1, page: 1, page_size: 200 }),
  ]);
  if (warehouseResult.status === 'fulfilled') {
    warehouses.value = warehouseResult.value.list ?? [];
  }
  if (customerResult.status === 'fulfilled') {
    customers.value = customerResult.value.list ?? [];
  }
  if (materialResult.status === 'fulfilled') {
    materials.value = materialResult.value.list ?? [];
  }
}

// ==================== 新建（从发货单带出可退明细） ====================

const formVisible = ref(false);
const form = reactive<ReturnFormDraft>(emptyReturnForm(todayCst()));
const formError = ref('');
const submitting = ref(false);
const returnableLoading = ref(false);
const returnableError = ref('');
/** 可退发货单（只列已过账，后端 `RETURNABLE_SHIPMENT_STATUSES`） */
const returnableShipments = ref<Shipment[]>([]);
const shipmentLoading = ref(false);

const shipmentOptions = computed(() => {
  const options = returnableShipments.value.map((item) => ({
    label: `${item.ship_no} · ${item.customer_name || item.customer_code || '客户已删除'}`,
    value: item.id,
  }));
  // 新建成功后可能把这张单的可退量退光了（列表不再含它），但详情/再次打开时保留可读
  if (form.shipment_id && !options.some((item) => item.value === form.shipment_id)) {
    options.unshift({ label: '当前已选发货单', value: form.shipment_id });
  }
  return options;
});

const formTotals = computed(() => totalsOf(form.items));

const emptyReturnableText = computed(() =>
  form.shipment_id
    ? '这张发货单没有可退的明细（已全部退完，或可退量被其他未作废的退货单占满）'
    : '先选原发货单，页面会自动带出可退（已发 − 已退）的明细',
);

/** 发货单下拉：只列已过账（未过账 / 已作废没有可退的货） */
async function loadReturnableShipments(): Promise<void> {
  shipmentLoading.value = true;
  try {
    const data = await fetchShipmentPage({ status: 'POSTED', page: 1, page_size: 200 });
    returnableShipments.value = data.list ?? [];
  } catch {
    // 下拉拉不到不挡列表：退化成空选项，用户可刷新页面重试
    returnableShipments.value = [];
  } finally {
    shipmentLoading.value = false;
  }
}

function openCreate(): void {
  formError.value = '';
  returnableError.value = '';
  Object.assign(form, emptyReturnForm(todayCst()));
  locations.value = [];
  formVisible.value = true;
  if (returnableShipments.value.length === 0) {
    void loadReturnableShipments();
  }
}

/**
 * 选定发货单 → 拉「可退明细」。
 *
 * ⚠️ 用 `@change` 的**事件出参**取值，不读 `form.shipment_id`：TDesign 先 emit change
 * 再回写 v-model，读当前值会拿到上一张发货单（踩过）。
 *
 * 仓库默认带成该发货单的发货仓库（退货回原仓是最常见口径，用户仍可改）。
 */
async function handleShipmentChange(value: unknown): Promise<void> {
  const shipmentId = typeof value === 'string' ? value : String(value ?? '');
  form.shipment_id = shipmentId;
  form.items = [];
  returnableError.value = '';
  if (!shipmentId) {
    return;
  }
  const picked = returnableShipments.value.find((item) => item.id === shipmentId);
  if (picked && !form.warehouse_id) {
    const changed = await handleWarehouseChange(picked.warehouse_id);
    void changed;
  }
  returnableLoading.value = true;
  try {
    const items = await fetchReturnableItems(shipmentId);
    applyReturnable(items);
  } catch (error) {
    returnableError.value = `带出可退明细失败：${messageOf(error)}`;
  } finally {
    returnableLoading.value = false;
  }
}

/** 可退明细 → 表单行（只保留 `remaining > 0`；口径与后端自动带出同源） */
function applyReturnable(items: ReadonlyArray<ReturnableItem>): void {
  form.items = linesFromReturnable(items);
  if (form.items.length === 0) {
    returnableError.value = '这张发货单没有可退的明细，换一张发货单或先去发货单页确认过账状态';
  }
}

/** 换仓库 → 重新拉库位（库位只属于一个仓库；已选的库位必须清掉，否则串仓） */
async function handleWarehouseChange(value: unknown): Promise<void> {
  const warehouseId = typeof value === 'string' ? value : String(value ?? '');
  form.warehouse_id = warehouseId;
  locations.value = [];
  for (const line of form.items) {
    line.location_id = '';
  }
  if (!warehouseId) {
    return;
  }
  try {
    locations.value = await fetchLocationList(warehouseId, false);
  } catch {
    locations.value = [];
  }
}

function removeLine(key: string): void {
  if (form.items.length <= 1) {
    MessagePlugin.warning('至少保留 1 行；整单都不退就取消弹窗');
    return;
  }
  const index = form.items.findIndex((line: ReturnLineDraft) => line.key === key);
  if (index >= 0) {
    form.items.splice(index, 1);
  }
}

async function submitForm(): Promise<void> {
  const error = validateReturnForm(form);
  if (error) {
    formError.value = error;
    return;
  }
  formError.value = '';
  submitting.value = true;
  try {
    const created = await createReturn(buildCreatePayload(form));
    MessagePlugin.success(`已创建 ${created.return_no}（草稿，价税合计 ${moneyText(created.total_with_tax)}）`);
    formVisible.value = false;
    await loadReturns(1);
    await openDetail(created);
  } catch (submitError) {
    // 后端原文（超退 400 / 发货单未过账 409 / 客户不一致 400）直接摆在弹窗里
    formError.value = messageOf(submitError);
  } finally {
    submitting.value = false;
  }
}

// ==================== 状态流转（提交 / 批准 / 待过账 / 过账 / 作废） ====================

const acting = ref(false);

/** 通用单步流转（提交 / 批准 / 待过账）：成功提示里说清「现在到哪一步」 */
async function advance(
  row: SalesReturn,
  target: 'SUBMITTED' | 'APPROVED' | 'PENDING_POST',
): Promise<void> {
  const verb = target === 'SUBMITTED' ? '提交' : target === 'APPROVED' ? '批准' : '进入待过账';
  try {
    const updated = await changeReturnStatus(row.id, { target_status: target });
    MessagePlugin.success(`${updated.return_no} 已${verb}（状态：${updated.status_label || returnStatusLabel(updated.status)}）`);
    await refreshAfterMutation(updated.id);
  } catch (error) {
    MessagePlugin.error(`${verb}失败：${messageOf(error)}`);
    await loadReturns(current.value);
  }
}

/** 提交：`DRAFT → SUBMITTED`（同事务重跑超退闸） */
async function submitReturn(row: SalesReturn): Promise<void> {
  await advance(row, 'SUBMITTED');
}

/** 批准：`SUBMITTED → APPROVED` */
async function approveReturn(row: SalesReturn): Promise<void> {
  await advance(row, 'APPROVED');
}

/** 进入待过账：`APPROVED → PENDING_POST` */
async function enterPendingPost(row: SalesReturn): Promise<void> {
  await advance(row, 'PENDING_POST');
}

/** 过账确认弹窗 */
const postVisible = ref(false);
const postError = ref('');
const postTarget = ref<SalesReturn | null>(null);

function openPostDialog(row: SalesReturn): void {
  postTarget.value = row;
  postError.value = '';
  postVisible.value = true;
}

/**
 * 确认过账：`PENDING_POST → POSTED`。
 *
 * 后端同一事务（`sales_return_posting::post_in_tx`）：退货入库单 → 三本账回库 →
 * 红字入库凭证 → 冲减原应收 → 已开票造红字发票 + 收入冲减凭证 → 回写 `delivered_qty`
 * → 抵减信用占用。库存 / 应收相关的 409 报文点名物料与金额，**原样显示**。
 */
async function confirmPost(): Promise<void> {
  const row = postTarget.value;
  if (!row) {
    postVisible.value = false;
    return;
  }
  acting.value = true;
  postError.value = '';
  try {
    const updated = await changeReturnStatus(row.id, { target_status: 'POSTED' });
    postVisible.value = false;
    MessagePlugin.success(`${updated.return_no} 已过账：库存已回库、应收已冲减`);
    await refreshAfterMutation(updated.id);
  } catch (error) {
    postError.value = messageOf(error);
    // 抽屉里的常驻错误也写一份，弹窗关掉后用户还能看到为什么失败
    detailActionError.value = messageOf(error);
  } finally {
    acting.value = false;
  }
}

/** 作废弹窗 */
const cancelVisible = ref(false);
const cancelReason = ref('');
const cancelTarget = ref<SalesReturn | null>(null);

const cancelNeedsReason = computed(() =>
  cancelTarget.value ? statusNeedsReason(cancelTarget.value.status, 'CANCELLED') : false,
);

function openCancelDialog(row: SalesReturn): void {
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelVisible.value = true;
}

async function confirmCancel(): Promise<void> {
  const row = cancelTarget.value;
  if (!row) {
    cancelVisible.value = false;
    return;
  }
  if (cancelNeedsReason.value && !cancelReason.value.trim()) {
    MessagePlugin.warning('已提交 / 已批准的单作废必须填写原因');
    return;
  }
  acting.value = true;
  try {
    const updated = await changeReturnStatus(row.id, {
      target_status: 'CANCELLED',
      reason: cancelReason.value.trim() || undefined,
    });
    MessagePlugin.success(`${updated.return_no} 已作废（状态：已取消）`);
    cancelVisible.value = false;
    await refreshAfterMutation(updated.id);
  } catch (error) {
    MessagePlugin.error(`作废失败：${messageOf(error)}`);
  } finally {
    acting.value = false;
  }
}

/** 变更后刷新列表；详情抽屉开着就同步刷新那一张 */
async function refreshAfterMutation(id: string): Promise<void> {
  await loadReturns(current.value);
  if (detailVisible.value && detail.value?.id === id) {
    await loadDetail(id);
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailActionError = ref('');
const detail = ref<SalesReturn | null>(null);
/** 审批进度（节点 + 审批人明细；退货单未接引擎时恒空，区块不渲染） */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

const detailActions = computed(() => rowActions(detail.value ?? { status: null }));
const links = computed(() =>
  postingLinks(
    detail.value ?? { status: null, stock_in_id: null, receivable_id: null, red_invoice_id: null },
  ),
);

async function openDetail(row: SalesReturn): Promise<void> {
  detailVisible.value = true;
  await loadDetail(row.id);
}

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  detailActionError.value = '';
  detail.value = null;
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    detail.value = await fetchReturnDetail(id);
    // 明细里的库位名要用：按详情里的仓库重拉一次库位字典（失败不挡详情）
    if (detail.value.warehouse_id) {
      locations.value = await fetchLocationList(detail.value.warehouse_id, true).catch(() => []);
    }
    approvalProgress.value = await loadApprovalProgress(detail.value.approval_instance_id, getFlowInstance);
  } catch (error) {
    detailError.value = `详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

onMounted(async () => {
  await Promise.all([loadReturns(1), loadOptions()]);
});
</script>

<style scoped lang="less">
.sales-return-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px;
}

.filter-card,
.list-card {
  border-radius: 8px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;

  &__keyword {
    width: 200px;
  }

  &__select {
    width: 140px;
  }

  &__customer {
    width: 220px;
  }

  &__spacer {
    flex: 1;
  }
}

.gate-wrap {
  display: inline-flex;
}

.panel-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 8px;

  &__main {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__meta {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }

  &__hint {
    margin-left: auto;
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.list-alert {
  margin-bottom: 8px;
}

.op-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
}

.strong {
  font-weight: 600;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.empty-block {
  padding: 24px 0;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.form-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.section-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 4px 0;

  &__title {
    font-weight: 600;
  }

  &__tip {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }

  &__spacer {
    flex: 1;
  }

  &__total {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}

.location-select {
  width: 100%;
}

.form-alert {
  margin-top: 4px;
}

.detail-loading {
  padding: 32px 0;
  text-align: center;
}

.detail-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 12px;

  &__no {
    font-size: 15px;
    font-weight: 600;
  }

  &__spacer {
    flex: 1;
  }
}

.status-line {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 8px 12px;
  margin-bottom: 8px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover);
  font-size: 12px;

  &__step {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--td-text-color-placeholder);

    &--done {
      color: var(--td-text-color-secondary);
    }

    &--current {
      color: var(--td-brand-color);
      font-weight: 600;
    }
  }

  &__dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentcolor;
  }

  &__arrow {
    margin: 0 2px 0 6px;
    color: var(--td-text-color-placeholder);
  }
}

.detail-desc {
  margin-bottom: 8px;
}

.detail-alert {
  margin: 8px 0;
}

.post-links {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.7;
}

.section-title {
  margin: 16px 0 8px;
  font-weight: 600;
}

.dialog-text {
  margin: 0 0 12px;
  line-height: 1.7;
  color: var(--td-text-color-primary);
}

.dialog-error {
  margin-bottom: 12px;
}

.dialog-footer {
  display: flex;
  gap: 8px;
}

.reason-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.reason-tip {
  margin: 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}
</style>
