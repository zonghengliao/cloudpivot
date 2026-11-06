<template>
  <div class="shipment-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.ship_no"
          class="filter-row__keyword"
          placeholder="发货单号（模糊）"
          clearable
          data-testid="shipment-filter-no"
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
          data-testid="shipment-filter-status"
        />

        <t-select
          v-model="filter.customer_id"
          class="filter-row__customer"
          :options="customerOptions"
          placeholder="全部客户"
          clearable
          filterable
          data-testid="shipment-filter-customer"
        />

        <t-button theme="primary" data-testid="btn-shipment-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-shipment-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-shipment-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="
            canWrite
              ? '选一张已审批 / 部分出库的销售订单，自动带出未发完的明细'
              : '当前账号没有 business:erp:shipment:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-shipment-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建发货单
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">发货单</span>
          <span class="panel-head__meta" data-testid="shipment-meta">共 {{ total }} 条</span>
          <t-tag v-if="isFiltered" size="small" variant="outline" data-testid="shipment-filtered-tag">
            已筛选
          </t-tag>
        </div>
        <span class="panel-head__hint">
          状态机 DRAFT → 待过账 → 已过账（过账同事务生成销售出库单、扣三本账、回写订单已发量）；
          已过账不可作废，走红冲
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="shipment-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadShipments()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="shipment-table"
        @page-change="handlePageChange"
      >
        <template #ship_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="shipment-no-link" @click="openDetail(row)">
            {{ row.ship_no }}
          </t-link>
        </template>

        <template #so_no="{ row }">
          <!--
            ⚠️ 后端缺口（SO-11 实测，已开缺陷卡）：`page_shipment` 只批量补了 customer_* /
            warehouse_*，**没补 `so_no`**（`ShipmentRes::of` 里恒 `None`），所以列表里这一列
            目前只能显示兜底文案；详情接口是有的。别把 `null` 当成「订单已删除」——
            真实原因（取消/红冲）也不会体现在这里。
          -->
          <span class="mono" :class="{ muted: !row.so_no }" :title="row.so_no || '列表接口未下发订单号（详情里有）'">
            {{ row.so_no || '见详情' }}
          </span>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="customerText(row)">
            {{ customerText(row) }}
          </span>
        </template>

        <template #warehouse_name="{ row }">
          <span :class="{ muted: !row.warehouse_name }" :title="warehouseText(row)">
            {{ warehouseText(row) }}
          </span>
        </template>

        <template #ship_date="{ row }">
          <span class="mono">{{ row.ship_date }}</span>
        </template>

        <template #total_qty="{ row }">
          <span class="mono">{{ formatQty(row.total_qty) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="shipmentStatusTheme(row.status)"
            :data-testid="`shipment-status-${row.ship_no}`"
          >
            {{ row.status_label || shipmentStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #stock_out_no="{ row }">
          <!--
            ⚠️ 列表接口不下发 `stock_out_no`（后端缺口，同 `so_no`）：已过账的行在这里只能
            提示「见详情」。别再按 `null` 判「出库单已删除」—— 那会把每张已过账单都说成坏数据。
          -->
          <span
            v-if="row.stock_out_no"
            class="mono"
            :data-testid="`shipment-stock-out-${row.ship_no}`"
          >
            {{ row.stock_out_no }}
          </span>
          <span
            v-else
            class="muted"
            :data-testid="`shipment-stock-out-${row.ship_no}`"
            :title="row.status === 'POSTED' ? '列表接口未下发出库单号（详情里有）' : undefined"
          >
            {{ row.status === 'POSTED' ? '见详情' : '未过账' }}
          </span>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="btn-shipment-detail" @click="openDetail(row)">
              详情
            </t-link>
            <t-link
              v-if="rowActions(row).submit"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-shipment-submit"
              @click="submitShipment(row)"
            >
              提交
            </t-link>
            <t-link
              v-if="rowActions(row).post"
              v-privilege="[PERM.status]"
              theme="success"
              data-testid="btn-shipment-post"
              @click="openPostDialog(row)"
            >
              过账
            </t-link>
            <t-link
              v-if="rowActions(row).cancel"
              v-privilege="[PERM.status]"
              theme="danger"
              data-testid="btn-shipment-cancel"
              @click="openCancelDialog(row)"
            >
              作废
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="empty-block" data-testid="shipment-empty">{{ emptyListText(filter) }}</div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建（从销售订单带出未发完明细） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      header="新建发货单"
      :width="1120"
      :confirm-btn="{ content: '创建发货单', loading: submitting, disabled: submitting }"
      :cancel-btn="{ disabled: submitting }"
      :close-on-overlay-click="false"
      data-testid="shipment-form-dialog"
      @confirm="submitForm"
    >
      <div class="form-body">
        <t-form label-width="96px" class="form-head">
          <t-row :gutter="[16, 0]">
            <t-col :span="6">
              <t-form-item label="销售订单" required-mark>
                <t-select
                  v-model="form.so_id"
                  :options="orderOptions"
                  filterable
                  :loading="orderLoading"
                  placeholder="只列已审批 / 部分出库的订单"
                  data-testid="shipment-form-so"
                  @change="handleOrderChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="发货仓库" required-mark>
                <t-select
                  v-model="form.warehouse_id"
                  :options="warehouseOptions"
                  filterable
                  placeholder="选择发货仓库"
                  data-testid="shipment-form-warehouse"
                  @change="handleWarehouseChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="发货日期" required-mark>
                <t-date-picker
                  v-model="form.ship_date"
                  value-type="YYYY-MM-DD"
                  format="YYYY-MM-DD"
                  :clearable="false"
                  data-testid="shipment-form-date"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="备注">
                <t-input
                  v-model="form.remark"
                  :maxlength="SHIPMENT_REMARK_MAX"
                  placeholder="发货备注（选填）"
                  data-testid="shipment-form-remark"
                />
              </t-form-item>
            </t-col>
          </t-row>
        </t-form>

        <div class="section-head">
          <span class="section-head__title">明细（{{ form.items.length }} 行）</span>
          <span class="section-head__tip">
            选定订单后自动带出<strong>未发完</strong>的行，数量默认「未发量」；改小即部分发货，删行即本次不发
          </span>
          <span class="section-head__spacer" />
          <span class="section-head__total" data-testid="shipment-form-total">
            本次合计 <b class="mono">{{ formatQty(formTotal) }}</b>
          </span>
        </div>

        <t-alert
          v-if="shippableError"
          theme="error"
          :message="shippableError"
          class="form-alert"
          data-testid="shipment-shippable-error"
        />

        <t-loading :loading="shippableLoading" size="small" show-overlay>
          <t-table
            row-key="key"
            size="small"
            :data="form.items"
            :columns="formItemColumns"
            :empty="emptyShippableText"
            data-testid="shipment-form-items"
          >
            <template #line_no="{ row }">
              <span class="mono">{{ row.line_no }}</span>
            </template>
            <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
            <template #ordered="{ row }"><span class="mono">{{ formatQty(row.ordered) }}</span></template>
            <template #delivered="{ row }"><span class="mono">{{ formatQty(row.delivered) }}</span></template>
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
                data-testid="shipment-form-qty"
              />
            </template>
            <template #location_id="{ row }">
              <t-select
                v-model="row.location_id"
                class="location-select"
                :options="locationOptions"
                clearable
                placeholder="整仓 / FIFO"
                data-testid="shipment-form-location"
              />
            </template>
            <template #line_op="{ row }">
              <t-link
                theme="danger"
                :disabled="form.items.length <= 1"
                data-testid="btn-shipment-remove-line"
                @click="removeLine(row.key)"
              >
                不发
              </t-link>
            </template>
          </t-table>
        </t-loading>

        <t-alert v-if="formError" theme="error" :message="formError" class="form-alert" data-testid="shipment-form-error" />
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="发货单详情"
      size="920px"
      :footer="false"
      data-testid="shipment-detail-drawer"
    >
      <div v-if="detailLoading" class="detail-loading"><t-loading :loading="true" text="加载中…" /></div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" data-testid="shipment-detail-error" />

      <template v-else-if="detail">
        <div class="detail-head">
          <span class="detail-head__no mono" data-testid="shipment-detail-no">{{ detail.ship_no }}</span>
          <t-tag size="small" variant="light" :theme="shipmentStatusTheme(detail.status)" data-testid="shipment-detail-status">
            {{ detail.status_label || shipmentStatusLabel(detail.status) }}
          </t-tag>
          <span class="detail-head__spacer" />
          <t-button
            v-if="detailActions.submit"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-shipment-detail-submit"
            @click="submitShipment(detail)"
          >
            提交待过账
          </t-button>
          <t-button
            v-if="detailActions.post"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-shipment-detail-post"
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
            data-testid="btn-shipment-detail-cancel"
            @click="openCancelDialog(detail)"
          >
            作废
          </t-button>
          <t-button variant="outline" size="small" data-testid="btn-shipment-drawer-close" @click="detailVisible = false">
            关闭
          </t-button>
        </div>

        <t-alert
          v-if="detailActionError"
          theme="error"
          class="detail-alert"
          data-testid="shipment-action-error"
          :message="detailActionError"
        />

        <div class="status-line" data-testid="shipment-status-line">
          <span
            v-for="(step, index) in STATUS_STEPS"
            :key="step.value"
            class="status-line__step"
            :class="{
              'status-line__step--done': stepReached(step.value),
              'status-line__step--current': detail.status === step.value,
            }"
          >
            <span class="status-line__dot" />
            {{ step.label }}
            <span v-if="index < STATUS_STEPS.length - 1" class="status-line__arrow">→</span>
          </span>
        </div>

        <t-descriptions :column="2" size="small" class="detail-desc" data-testid="shipment-detail-desc">
          <t-descriptions-item label="来源销售订单">
            <span class="mono">{{ detail.so_no || '订单已删除' }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="客户">{{ customerText(detail) }}</t-descriptions-item>
          <t-descriptions-item label="发货仓库">{{ warehouseText(detail) }}</t-descriptions-item>
          <t-descriptions-item label="发货日期">{{ detail.ship_date }}</t-descriptions-item>
          <t-descriptions-item label="数量合计">{{ formatQty(detail.total_qty) }}</t-descriptions-item>
          <t-descriptions-item label="关联出库单号">
            <span
              class="mono"
              :class="{ muted: !detail.stock_out_no && detail.status !== 'POSTED' }"
              data-testid="shipment-detail-stock-out-no"
            >
              {{ stockOutText(detail) }}
            </span>
          </t-descriptions-item>
          <t-descriptions-item label="创建时间">{{ formatTime(detail.created_at) }}</t-descriptions-item>
          <t-descriptions-item label="更新时间">{{ formatTime(detail.updated_at) }}</t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ detail.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <t-alert
          v-if="detail.status === 'POSTED'"
          theme="success"
          class="detail-alert"
          data-testid="shipment-posted-hint"
          :message="`已过账：三本账已按明细扣减，成本结转凭证已生成；关联销售出库单 ${detail.stock_out_no || '（已删除）'}`"
        />
        <t-alert
          v-else-if="detail.status === 'CANCELLED'"
          theme="warning"
          class="detail-alert"
          data-testid="shipment-cancelled-hint"
          :message="'已取消（作废）：本单未过账，没有动过库存与凭证'"
        />

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="SHIPMENT_ITEM_COLUMNS"
          data-testid="shipment-detail-items"
        >
          <template #line_no="{ row }">
            <span class="mono">{{ row.line_no }}</span>
          </template>
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }"><span class="mono">{{ formatQty(row.qty) }}</span></template>
          <template #location_id="{ row }">
            <span :class="{ muted: !row.location_id }">{{ locationNameOf(row.location_id) }}</span>
          </template>
          <template #serial_nos="{ row }">
            <span v-if="row.serial_nos.length" class="mono">{{ row.serial_nos.join('、') }}</span>
            <span v-else class="muted">—</span>
          </template>
          <template #remark="{ row }">{{ row.remark || '—' }}</template>
        </t-table>
      </template>
    </t-drawer>

    <!-- ==================== ⑤ 过账确认弹窗 ==================== -->
    <t-dialog
      v-model:visible="postVisible"
      header="确认过账"
      width="580px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="shipment-post-dialog"
    >
      <p class="dialog-text" data-testid="shipment-post-text">{{ postDialogText(postTarget) }}</p>
      <t-alert
        v-if="postError"
        theme="error"
        class="dialog-error"
        data-testid="shipment-post-error"
        :message="postError"
      />
      <div class="dialog-footer">
        <t-button theme="primary" :loading="acting" data-testid="btn-shipment-post-confirm" @click="confirmPost">
          确认过账
        </t-button>
        <t-button variant="outline" data-testid="btn-shipment-post-cancel" @click="postVisible = false">取消</t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（PENDING_POST 必须写原因） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废发货单"
      width="520px"
      :confirm-btn="{ content: '作废', theme: 'danger', loading: acting }"
      :cancel-btn="{ disabled: acting }"
      data-testid="shipment-cancel-dialog"
      @confirm="confirmCancel"
    >
      <div class="reason-body">
        <p class="reason-tip">
          {{
            cancelNeedsReason
              ? '待过账的单作废会释放本单占用的未发量，必须写清原因（进状态日志与对账依据）。'
              : '草稿作废不动账、也不占用未发量，原因可选。'
          }}
        </p>
        <t-textarea
          v-model="cancelReason"
          :autosize="{ minRows: 3, maxRows: 5 }"
          :maxlength="200"
          placeholder="原因（例如：客户改单 / 仓库选错）"
          data-testid="shipment-cancel-reason"
        />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 发货单页（SO-11）。
 *
 * # 接口（全部真接口，路径逐字对应 `backend/src/business/erp/shipment_handler.rs`）
 *
 * | 动作 | 请求 | 权限 |
 * |------|------|------|
 * | 列表 | `GET /api/business/erp/shipment/page` | `:list` |
 * | 详情（含 `stock_out_no`） | `GET /api/business/erp/shipment/{id}` | `:list` |
 * | 未发完明细（从订单带出） | `GET /api/business/erp/shipment/shippable/{so_id}` | `:list` |
 * | 新建 | `POST /api/business/erp/shipment`（`items` 省略 = 服务端带出全部未发完行） | `:create` |
 * | 提交 / 过账 / 作废 | `PATCH /api/business/erp/shipment/{id}/status` | `:status` |
 *
 * # 三条硬口径
 *
 * 1. **过账不是状态标记**：`PENDING_POST → POSTED` 会在同一事务里生成销售出库单
 *    （`source_type=SALES`）、扣三本账、生成成本结转凭证、回写订单 `delivered_qty`
 *    与订单状态 —— 前端只说这些，不减少一处也不多加一处；
 * 2. **`DRAFT` 不能直接过账**：边表里只有 `PENDING_POST → POSTED`，所以列表里两个按钮
 *    分开（提交 / 过账），不要拼成一个「提交并过账」的假动作；
 * 3. **发货单没有审批流**（`ShipmentRes` 没有 `approval_instance_id`），所以版本里的
 *    `FlowProgress` 在这里**用不上**：审批链在销售订单（`so_approve`）那一层，
 *    这里画的是单据自己的 4 态状态线（`STATUS_STEPS`）。
 *
 * # 关联出库单号
 *
 * `stock_out_no` 只有**详情**补（列表行为 `null`），过账成功后详情里直接显示
 * `OUT-yyyyMMdd-000001`；列表列显示「未过账 / 出库单已删除」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  fetchCustomerPage,
  fetchLocationList,
  fetchMaterialPage,
  fetchSalesOrderPage,
  fetchWarehousePage,
  type Customer,
  type Location,
  type MaterialRow,
  type SalesOrder,
  type Warehouse,
} from '@/api/business/erp';
import {
  changeShipmentStatus,
  createShipment,
  fetchShippableItems,
  fetchShipmentDetail,
  fetchShipmentPage,
  type Shipment,
  type ShippableItem,
} from '@/api/business/erp/shipment';
import {
  SHIPMENT_ITEM_COLUMNS,
  SHIPMENT_COLUMNS,
  SHIPMENT_PAGE_SIZE,
  SHIPMENT_REMARK_MAX,
  SHIPMENT_STATUS_OPTIONS,
  buildCreatePayload,
  buildShipmentQuery,
  emptyListText,
  emptyShipmentFilter,
  emptyShipmentForm,
  isShipmentFiltered,
  linesFromShippable,
  postDialogText,
  rowActions,
  shipmentStatusLabel,
  shipmentStatusTheme,
  statusNeedsReason,
  stockOutText,
  totalQtyOf,
  validateShipmentForm,
  type ShipmentFilter,
  type ShipmentFormDraft,
  type ShipmentLineDraft,
} from './shipment-query';

/** 本页权限码（与 `V0284__sales_shipment_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情 / 可发明细（页面入口） */
  list: 'business:erp:shipment:list',
  /** 新建 */
  create: 'business:erp:shipment:create',
  /** 状态流转（提交 / 过账 / 作废） */
  status: 'business:erp:shipment:status',
} as const;

const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

/** 表格列（colKey 与后端出参同名） */
const columns = SHIPMENT_COLUMNS;
/** 状态下拉（`''` = 全部，靠 `clearable` 清） */
const statusOptions = SHIPMENT_STATUS_OPTIONS.map((item) => ({ ...item }));

/** 详情里的状态线（4 态，顺序即状态机走向；`CANCELLED` 不在主链上，单独用 tag 表达） */
const STATUS_STEPS = [
  { label: '草稿', value: 'DRAFT' },
  { label: '待过账', value: 'PENDING_POST' },
  { label: '已过账', value: 'POSTED' },
] as const;

/** 主链上已经走过的步骤（作废单只点亮草稿那一步） */
function stepReached(step: string): boolean {
  const current = detail.value?.status;
  if (!current) {
    return false;
  }
  if (current === 'CANCELLED') {
    return step === 'DRAFT';
  }
  const order = STATUS_STEPS.map((item) => item.value as string);
  return order.indexOf(step) <= order.indexOf(current);
}

/** 新建弹窗的明细列（多两列「订单量 / 已发 / 未发」，便于核对超发） */
const formItemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '订单行', width: 80 },
  { colKey: 'material_id', title: '物料', minWidth: 220, ellipsis: true },
  { colKey: 'ordered', title: '订单量', width: 100, align: 'right' },
  { colKey: 'delivered', title: '已发', width: 90, align: 'right' },
  { colKey: 'remaining', title: '未发量', width: 100, align: 'right' },
  { colKey: 'qty', title: '本次发货', width: 150 },
  { colKey: 'location_id', title: '发货库位', width: 170 },
  { colKey: 'line_op', title: '操作', width: 80 },
];

// ==================== 列表 ====================

const filter = reactive<ShipmentFilter>(emptyShipmentFilter());
const rows = ref<Shipment[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = SHIPMENT_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const isFiltered = computed(() => isShipmentFiltered(filter));
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

async function loadShipments(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchShipmentPage(buildShipmentQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      await loadShipments(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `发货单加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadShipments(1);
}

function handleReset(): void {
  Object.assign(filter, emptyShipmentFilter());
  current.value = 1;
  void loadShipments(1);
}

function handleRefresh(): void {
  void loadShipments(current.value);
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadShipments(pageInfo.current);
}

// ==================== 主数据下拉 ====================

const warehouses = ref<Warehouse[]>([]);
const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);
/** 表单里当前仓库的库位（`biz_location.id`，过账时按它指定库位；空 = 整仓 FIFO） */
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
    return '整仓 / FIFO';
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

// ==================== 新建（从订单带出） ====================

const formVisible = ref(false);
const form = reactive<ShipmentFormDraft>(emptyShipmentForm(todayCst()));
const formError = ref('');
const submitting = ref(false);
const shippableLoading = ref(false);
const shippableError = ref('');
/** 可发货订单（`APPROVED` + `PARTIAL_OUT` 两页合并） */
const shippableOrders = ref<SalesOrder[]>([]);
const orderLoading = ref(false);

const orderOptions = computed(() =>
  shippableOrders.value.map((item) => ({
    label: `${item.so_no} · ${item.customer_name || item.customer_code || '客户已删除'}`,
    value: item.id,
  })),
);

const formTotal = computed(() => totalQtyOf(form.items));

const emptyShippableText = computed(() =>
  form.so_id
    ? '这张订单没有未发完的行（已全部发货，或未发量被其他未过账的发货单占满）'
    : '先选销售订单，页面会自动带出未发完的明细',
);

/** 订单下拉：只列已审批 / 部分出库（后端 `SHIPPABLE_SO_STATUSES`） */
async function loadShippableOrders(): Promise<void> {
  orderLoading.value = true;
  try {
    const [approved, partial] = await Promise.all([
      fetchSalesOrderPage({ status: 'APPROVED', page: 1, page_size: 200 }),
      fetchSalesOrderPage({ status: 'PARTIAL_OUT', page: 1, page_size: 200 }),
    ]);
    shippableOrders.value = [...(approved.list ?? []), ...(partial.list ?? [])];
  } catch {
    // 下拉拉不到不挡列表：退化成空选项，用户可刷新页面重试
    shippableOrders.value = [];
  } finally {
    orderLoading.value = false;
  }
}

function openCreate(): void {
  formError.value = '';
  shippableError.value = '';
  Object.assign(form, emptyShipmentForm(todayCst()));
  locations.value = [];
  formVisible.value = true;
  if (shippableOrders.value.length === 0) {
    void loadShippableOrders();
  }
}

/**
 * 选定订单 → 拉「未发完的行」。
 *
 * ⚠️ 用 `@change` 的**事件出参**取值，不读 `form.so_id`：TDesign 先 emit change
 * 再回写 v-model，读当前值会拿到上一张订单（踩过）。
 */
async function handleOrderChange(value: unknown): Promise<void> {
  const soId = typeof value === 'string' ? value : String(value ?? '');
  form.so_id = soId;
  form.items = [];
  shippableError.value = '';
  if (!soId) {
    return;
  }
  shippableLoading.value = true;
  try {
    const items = await fetchShippableItems(soId);
    applyShippable(items);
  } catch (error) {
    shippableError.value = `带出订单明细失败：${messageOf(error)}`;
  } finally {
    shippableLoading.value = false;
  }
}

/** 可发明细 → 表单行（只保留 `remaining > 0`；口径与后端自动带出同源） */
function applyShippable(items: ReadonlyArray<ShippableItem>): void {
  form.items = linesFromShippable(items);
  if (form.items.length === 0) {
    shippableError.value = '这张订单没有未发完的明细，换一张订单或先去订单页确认发货进度';
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
    MessagePlugin.warning('至少保留 1 行；整单都不发就取消弹窗');
    return;
  }
  const index = form.items.findIndex((line: ShipmentLineDraft) => line.key === key);
  if (index >= 0) {
    form.items.splice(index, 1);
  }
}

async function submitForm(): Promise<void> {
  const error = validateShipmentForm(form);
  if (error) {
    formError.value = error;
    return;
  }
  formError.value = '';
  submitting.value = true;
  try {
    const created = await createShipment(buildCreatePayload(form));
    MessagePlugin.success(`已创建 ${created.ship_no}（草稿，${formatQty(created.total_qty)} 件）`);
    formVisible.value = false;
    await loadShipments(1);
    await openDetail(created);
  } catch (submitError) {
    // 后端原文（超发 400 / 订单状态已变 409 / 无未发量 409）直接摆在弹窗里
    formError.value = messageOf(submitError);
  } finally {
    submitting.value = false;
  }
}

// ==================== 状态流转（提交 / 过账 / 作废） ====================

const acting = ref(false);

/** 提交待过账：`DRAFT → PENDING_POST`（不动库存） */
async function submitShipment(row: Shipment): Promise<void> {
  try {
    const updated = await changeShipmentStatus(row.id, { target_status: 'PENDING_POST' });
    MessagePlugin.success(`${updated.ship_no} 已提交待过账`);
    await refreshAfterMutation(updated.id);
  } catch (error) {
    MessagePlugin.error(`提交失败：${messageOf(error)}`);
    await loadShipments(current.value);
  }
}

/** 过账确认弹窗 */
const postVisible = ref(false);
const postError = ref('');
const postTarget = ref<Shipment | null>(null);

function openPostDialog(row: Shipment): void {
  postTarget.value = row;
  postError.value = '';
  postVisible.value = true;
}

/**
 * 确认过账：`PENDING_POST → POSTED`。
 *
 * 后端同一事务：生成销售出库单 → 扣三本账 → 成本结转凭证 → 回填 `stock_out_id`
 * → 订单行 `delivered_qty` → 订单 `PARTIAL_OUT/OUTBOUND`。
 * 库存不足 / 并发超发是 409（报文点名物料与数量），**原样显示**，不在前端编安慰话。
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
    const updated = await changeShipmentStatus(row.id, { target_status: 'POSTED' });
    postVisible.value = false;
    MessagePlugin.success(
      `${updated.ship_no} 已过账，生成销售出库单 ${updated.stock_out_no || ''}`.trim(),
    );
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
const cancelTarget = ref<Shipment | null>(null);

const cancelNeedsReason = computed(() =>
  cancelTarget.value ? statusNeedsReason(cancelTarget.value.status, 'CANCELLED') : false,
);

function openCancelDialog(row: Shipment): void {
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
    MessagePlugin.warning('待过账的单作废必须填写原因');
    return;
  }
  acting.value = true;
  try {
    const updated = await changeShipmentStatus(row.id, {
      target_status: 'CANCELLED',
      reason: cancelReason.value.trim() || undefined,
    });
    MessagePlugin.success(`${updated.ship_no} 已作废（状态：已取消）`);
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
  await loadShipments(current.value);
  if (detailVisible.value && detail.value?.id === id) {
    await loadDetail(id);
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailActionError = ref('');
const detail = ref<Shipment | null>(null);

const detailActions = computed(() => rowActions(detail.value ?? { status: null }));

async function openDetail(row: Shipment): Promise<void> {
  detailVisible.value = true;
  await loadDetail(row.id);
}

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  detailActionError.value = '';
  detail.value = null;
  try {
    detail.value = await fetchShipmentDetail(id);
    // 详情的库位下拉要用：仓库换过就按详情里的仓库重拉一次
    if (detail.value.warehouse_id && detail.value.warehouse_id !== form.warehouse_id) {
      locations.value = await fetchLocationList(detail.value.warehouse_id, true).catch(() => []);
    }
  } catch (error) {
    detailError.value = `详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

onMounted(async () => {
  await Promise.all([loadShipments(1), loadOptions()]);
});
</script>

<style scoped lang="less">
.shipment-page {
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
