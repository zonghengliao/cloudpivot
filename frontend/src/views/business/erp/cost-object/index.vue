<!--
  成本对象（生产批）页（COST-B1-11 / ARCH-9 §6.2.1 · §5.2 #1–#5）。

  一页三件事：**列表（分页 + 状态/期间/车间筛选）→ 表单（建 / 改，必填校验）→ 关批（二次确认 + 错误提示）**。

  # 三个交互口径（都有卡面验收，别弱化）

  ① **列表筛选**：状态 / 开工期间（月 → `start_from`+`start_to`）/ 车间 / 关键字。
     后端列表接口**没有** `period_code`，期间就是开工日期区间（见 `cost-object-logic.ts::monthRange`）；
  ② **表单校验**：必填项（产出品 / 车间 / 目标仓 / 计划数量 / 开工日期）在页面先拦一道，
     文案与后端 400 对齐；编辑只提交**真改过**的字段（后端 PUT 是部分更新 + 三态，
     整表回传会把「清空」语义误发）；
  ③ **关批**：按钮 → 摘要确认框（列归集成本 / 在制数量，在制≠0 要勾「带在制关批」）
     → 第二段 `DialogPlugin.confirm` 终确认 → 接口。被 5 条前置拦下时展示后端逐条原因
     + 后端业务码 66055，并补「去哪修」的指引。

  # 没有的东西（别以为是漏了）

  * **删除按钮**：后端 B1-8 没做 `DELETE`（只有权限码，无路由）；
  * **作废按钮**：设计 §4.1 的 `OPEN→CANCELLED` 边已注册，但作废接口属另一张卡；
  * **「领料」快捷入口**：要等出库单页支持 `cost_object_id`（B1-12），本页不造死链。
-->
<template>
  <div class="cost-object-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="co-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建成本对象
        </t-button>
        <t-button variant="outline" data-testid="co-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="co-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.workshop_dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部车间"
          clearable
          filterable
          data-testid="co-filter-dept"
          @change="handleSearch"
        />
        <t-date-picker
          v-model="filter.month"
          class="tool-row__month"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="开工期间"
          clearable
          data-testid="co-filter-month"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="对象号 / 批号 / 订单号（模糊）"
          clearable
          data-testid="co-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="co-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="co-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">成本对象</span>
          <span class="panel-head__meta" data-testid="co-meta">共 {{ total }} 个</span>
        </div>
        <span class="panel-head__tip">
          一个生产批一个对象；料工费按批归集，关批后不可回「在产」（要复工请新建批）
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="co-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="co-table"
        @page-change="handlePageChange"
      >
        <template #object_no="{ row }">
          <span class="mono" :data-testid="`co-no-${row.object_no}`">{{ row.object_no }}</span>
        </template>
        <template #product_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.product_name || '—' }}</span>
            <span class="name-cell__sub">{{ lotOrOrderText(row) }}</span>
          </div>
        </template>
        <template #plan_qty="{ row }">
          <span class="num">{{ formatQty(row.plan_qty) }}</span>
        </template>
        <template #completed_qty="{ row }">
          <span class="num">{{ formatQty(row.completed_qty) }}</span>
        </template>
        <template #wip_qty="{ row }">
          <span class="num" :class="{ 'num--warn': !isZeroQty(row.wip_qty) }">
            {{ formatQty(row.wip_qty) }}
          </span>
        </template>
        <template #cost_total="{ row }">
          <span class="money">{{ formatMoney(row.cost_total) }}</span>
        </template>
        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="statusTheme(row.status)"
            :data-testid="`co-status-${row.object_no}`"
          >
            {{ statusLabel(row.status) }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-link
              theme="primary"
              :data-testid="`co-btn-detail-${row.object_no}`"
              @click="openDetail(row)"
            >
              详情
            </t-link>
            <t-link
              v-if="canEditObject(row.status)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`co-btn-edit-${row.object_no}`"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="canCloseRow(row)"
              v-privilege="[PERM.close]"
              theme="warning"
              :data-testid="`co-btn-close-${row.object_no}`"
              @click="openClose(row)"
            >
              关批
            </t-link>
            <span v-if="!canEditObject(row.status)" class="op-cell__hint">已结项</span>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 建 / 改 表单 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑成本对象' : '新建成本对象'"
      width="980px"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      data-testid="co-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-alert
        v-if="formError"
        theme="error"
        :message="formError"
        class="form-alert"
        data-testid="co-form-error"
      />
      <t-form :data="form" label-width="110px" colon>
        <t-row :gutter="[16, 0]">
          <t-col :span="12">
            <t-form-item
              label="产出品"
              :help="formErrors.product_id || productLockedHint"
              :status="formStatus('product_id')"
            >
              <t-select
                v-model="form.product_id"
                :options="productOptions"
                :disabled="productLocked"
                :loading="optionsLoading"
                placeholder="选择产出品（仅成品 / 半成品）"
                filterable
                data-testid="co-form-product"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="生产车间"
              :help="formErrors.workshop_dept_id"
              :status="formStatus('workshop_dept_id')"
            >
              <DeptTreeSelect
                v-model="form.workshop_dept_id"
                placeholder="选择生产车间"
                data-testid="co-form-dept"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="计划数量"
              :help="formErrors.plan_qty"
              :status="formStatus('plan_qty')"
            >
              <t-input
                v-model="form.plan_qty"
                placeholder="> 0，最多 4 位小数"
                data-testid="co-form-plan-qty"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="投料方式" help="一期只支持「一次投入」；「随进度投入」属三期能力">
              <t-radio-group v-model="form.material_input_mode" data-testid="co-form-mode">
                <t-radio value="ONE_TIME">一次投入</t-radio>
                <t-radio value="AS_PROGRESS" disabled>随进度投入（三期）</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="目标仓"
              :help="formErrors.target_warehouse_id"
              :status="formStatus('target_warehouse_id')"
            >
              <t-select
                v-model="form.target_warehouse_id"
                :options="warehouseOptions"
                :loading="optionsLoading"
                placeholder="完工入库仓"
                filterable
                data-testid="co-form-warehouse"
                @change="handleWarehouseChange"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="目标库位" help="可空；跟仓库联动">
              <t-select
                v-model="form.target_location_id"
                :options="locationOptions"
                :loading="locationLoading"
                :disabled="!form.target_warehouse_id"
                placeholder="选择库位（可空）"
                filterable
                clearable
                data-testid="co-form-location"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="开工日期"
              :help="formErrors.start_date"
              :status="formStatus('start_date')"
            >
              <t-date-picker
                v-model="form.start_date"
                format="YYYY-MM-DD"
                value-type="YYYY-MM-DD"
                clearable
                data-testid="co-form-start"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item
              label="计划完工"
              :help="formErrors.due_date"
              :status="formStatus('due_date')"
            >
              <t-date-picker
                v-model="form.due_date"
                format="YYYY-MM-DD"
                value-type="YYYY-MM-DD"
                clearable
                placeholder="可空"
                data-testid="co-form-due"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="产出批号" help="填了自动建批次主数据（来源=生产）">
              <t-input
                v-model="form.lot_no"
                placeholder="可空，如 LOT-20261101-01"
                data-testid="co-form-lot-no"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="上游订单号" help="按订单生产时填，便于按订单追溯成本">
              <t-input
                v-model="form.source_doc_no"
                placeholder="可空，如 SO-20261101-001"
                data-testid="co-form-source-doc"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="客户" help="按订单生产时的买方（可空）">
              <t-select
                v-model="form.customer_id"
                :options="customerOptions"
                :loading="optionsLoading"
                placeholder="可选"
                filterable
                clearable
                data-testid="co-form-customer"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="备注">
              <t-input
                v-model="form.remark"
                placeholder="可空"
                :maxlength="2000"
                data-testid="co-form-remark"
              />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
      <p class="form-footnote">
        必填：产出品 / 生产车间 / 目标仓 / 计划数量 / 开工日期。
        保存后对象号由服务端取号（<code>CB-yyyyMMdd-NNNNNN</code>）。
      </p>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="900px"
      :header="detail ? `${detail.object_no} · ${statusLabel(detail.status)}` : '成本对象详情'"
      :footer="false"
      data-testid="co-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <template v-if="detail">
          <h4 class="section-title">基本信息</h4>
          <t-descriptions :column="2" size="small" bordered>
            <t-descriptions-item label="对象号">{{ detail.object_no }}</t-descriptions-item>
            <t-descriptions-item label="状态">{{ statusLabel(detail.status) }}</t-descriptions-item>
            <t-descriptions-item label="投料方式">
              {{ inputModeLabel(detail.material_input_mode) }}
            </t-descriptions-item>
            <t-descriptions-item label="产出品">
              {{ detail.product_code ? `${detail.product_code} ` : '' }}{{ detail.product_name || detail.product_id }}
            </t-descriptions-item>
            <t-descriptions-item label="产出批号">{{ detail.lot_no || '—' }}</t-descriptions-item>
            <t-descriptions-item label="生产车间">
              {{ detail.workshop_dept_name || detail.workshop_dept_id }}
            </t-descriptions-item>
            <t-descriptions-item label="计划数量">{{ formatQty(detail.plan_qty) }}</t-descriptions-item>
            <t-descriptions-item label="累计完工">{{ formatQty(detail.completed_qty) }}</t-descriptions-item>
            <t-descriptions-item label="期末在制">
              {{ formatQty(detail.wip_qty) }}
              <span class="muted">（进度 {{ detail.completion_pct }}%）</span>
            </t-descriptions-item>
            <t-descriptions-item label="目标仓">
              {{ detail.target_warehouse_name || detail.target_warehouse_id }}
            </t-descriptions-item>
            <t-descriptions-item label="目标库位">
              {{ detail.target_location_name || '—' }}
            </t-descriptions-item>
            <t-descriptions-item label="客户">{{ detail.customer_name || '—' }}</t-descriptions-item>
            <t-descriptions-item label="开工日期">{{ detail.start_date }}</t-descriptions-item>
            <t-descriptions-item label="计划完工">{{ detail.due_date || '—' }}</t-descriptions-item>
            <t-descriptions-item label="上游订单">{{ detail.source_doc_no || '—' }}</t-descriptions-item>
            <t-descriptions-item label="关批时间">{{ detail.closed_at || '—' }}</t-descriptions-item>
            <t-descriptions-item label="备注" :span="2">{{ detail.remark || '—' }}</t-descriptions-item>
          </t-descriptions>

          <h4 class="section-title">归集汇总</h4>
          <div class="summary-grid" data-testid="co-detail-summary">
            <div class="summary-cell">
              <span class="summary-cell__label">直接材料</span>
              <span class="summary-cell__value">{{ formatMoney(detail.collect_summary.mat) }}</span>
            </div>
            <div class="summary-cell">
              <span class="summary-cell__label">直接人工</span>
              <span class="summary-cell__value">{{ formatMoney(detail.collect_summary.labor) }}</span>
            </div>
            <div class="summary-cell">
              <span class="summary-cell__label">制造费用</span>
              <span class="summary-cell__value">{{ formatMoney(detail.collect_summary.oh) }}</span>
            </div>
            <div class="summary-cell summary-cell--total">
              <span class="summary-cell__label">合计</span>
              <span class="summary-cell__value">{{ formatMoney(detail.collect_summary.total) }}</span>
            </div>
          </div>

          <h4 class="section-title">成本计算单</h4>
          <t-alert
            v-if="!detail.period_items.length"
            theme="info"
            message="还没有结算过：到「成本核算」里跑期间计算后，这里会按期间列出约当产量、单位成本与完工转出。"
            data-testid="co-detail-no-items"
          />
          <t-table
            v-else
            row-key="period_code"
            size="small"
            :data="detail.period_items"
            :columns="periodColumns"
            data-testid="co-detail-items"
          >
            <template #wip_end_qty="{ row }">{{ formatQty(row.wip_end_qty) }}</template>
            <template #unit_finished_cost="{ row }">
              <span class="money">{{ formatMoney(row.unit_finished_cost) }}</span>
            </template>
            <template #finished_cost="{ row }">
              <span class="money">{{ formatMoney(row.finished_cost) }}</span>
            </template>
          </t-table>

          <div v-if="canCloseObject(detail)" class="detail-actions">
            <t-button
              v-privilege="[PERM.close]"
              theme="warning"
              variant="outline"
              data-testid="co-detail-btn-close"
              @click="openCloseFromDetail()"
            >
              关批
            </t-button>
            <span class="muted">关批前会校验 5 条前置（草稿工时 / 未归集领料 / 期间锁定 / 未过账领料 / 在制数量）</span>
          </div>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ==================== ⑤ 关批（二次确认） ==================== -->
    <t-dialog
      v-model:visible="closeVisible"
      header="关批成本对象"
      width="640px"
      :confirm-btn="{ content: '确认关批', loading: closeSubmitting, theme: 'warning' }"
      :cancel-btn="{ content: '取消' }"
      data-testid="co-close-dialog"
      @confirm="submitClose"
      @close="closeVisible = false"
    >
      <template v-if="closeTarget">
        <t-alert
          theme="warning"
          :message="closeSummary?.irreversible_hint"
          class="form-alert"
          data-testid="co-close-irreversible"
        />
        <t-alert
          v-if="closeError"
          theme="error"
          :message="closeError"
          class="form-alert"
          data-testid="co-close-error"
        />
        <dl class="close-facts" data-testid="co-close-facts">
          <div>
            <dt>对象号</dt>
            <dd class="mono">{{ closeSummary?.object_no }}</dd>
          </div>
          <div>
            <dt>产出品 / 批号</dt>
            <dd>{{ closeSummary?.product }}</dd>
          </div>
          <div>
            <dt>已归集成本</dt>
            <dd class="money">{{ formatMoney(closeSummary?.collect_total ?? '0') }}</dd>
          </div>
          <div>
            <dt>期末在制数量</dt>
            <dd :class="{ 'num--warn': closeSummary?.need_force_wip }">
              {{ closeSummary?.wip_qty }}
            </dd>
          </div>
        </dl>

        <t-checkbox v-model="closeConfirmed" data-testid="co-close-confirm-check">
          我已确认：关批后不可回「在产」，也不再领料 / 报工
        </t-checkbox>

        <t-checkbox
          v-if="closeSummary?.need_force_wip"
          v-model="forceWipChecked"
          class="close-force"
          data-testid="co-close-force-wip"
        >
          我确认带在制关批：在制 {{ closeSummary?.wip_qty }} 的成本将永久挂在已关闭的对象上
        </t-checkbox>

        <t-form label-width="80px" class="close-remark">
          <t-form-item label="关批备注">
            <t-input
              v-model="closeRemark"
              placeholder="可空；会写进状态日志"
              :maxlength="200"
              data-testid="co-close-remark"
            />
          </t-form-item>
        </t-form>
        <p class="muted close-tip">
          点「确认关批」后还会再弹一次终确认。被前置拦下时，这里会逐条列出原因与「去哪修」。
        </p>
      </template>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  closeCostObject,
  createCostObject,
  fetchCostObjectDetail,
  fetchCostObjectPage,
  updateCostObject,
  COST_OBJECT_STATUSES,
  type CostObjectDetail,
  type CostObjectRow,
  type CostObjectStatus,
} from '@/api/business/erp/cost-object';
import {
  fetchCustomerPage,
  fetchLocationList,
  fetchMaterialPage,
  fetchWarehouseTree,
  type Customer,
  type Location,
  type MaterialRow,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { formatMoney, todayString } from '@/views/business/erp/shared/money';
import {
  COST_OBJECT_PERMISSIONS,
  PAGE_SIZE,
  OPTION_LIMIT,
  canCloseObject,
  canCloseRow,
  canEditObject,
  closeErrorText,
  closeSummaryOf,
  emptyCostObjectForm,
  errorCount,
  formFromDetail,
  formatQty,
  hasCollection,
  inputModeLabel,
  isEmptyPayload,
  isZeroQty,
  monthRange,
  statusLabel,
  statusTheme,
  toClosePayload,
  toCreatePayload,
  toUpdatePayload,
  validateCostObjectForm,
  type CloseSummary,
  type CostObjectFormErrors,
  type CostObjectFormState,
} from './cost-object-logic';

/** 本页权限码（V0378 种子逐一对应；`delete` 后端无路由，故不列） */
const PERM = COST_OBJECT_PERMISSIONS;

// ==================== 列表 ====================

const rows = ref<CostObjectRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<{
  keyword: string;
  status: CostObjectStatus | '';
  workshop_dept_id: string;
  month: string;
}>({ keyword: '', status: '', workshop_dept_id: '', month: '' });

const statusFilterOptions = COST_OBJECT_STATUSES.map((value) => ({
  label: statusLabel(value),
  value,
}));

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽合计 ≈ 940px：1280 视口（侧栏展开）不会被固定「操作」列压住状态列
const columns: PrimaryTableCol[] = [
  { colKey: 'object_no', title: '对象号', width: 156 },
  { colKey: 'product_name', title: '产出品 / 批号 · 订单', width: 190, ellipsis: true },
  { colKey: 'workshop_dept_name', title: '车间', width: 90, ellipsis: true },
  { colKey: 'plan_qty', title: '计划', width: 76, align: 'right' },
  { colKey: 'completed_qty', title: '完工', width: 76, align: 'right' },
  { colKey: 'wip_qty', title: '在制', width: 76, align: 'right' },
  { colKey: 'cost_total', title: '归集成本', width: 110, align: 'right' },
  { colKey: 'status', title: '状态', width: 80 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

const periodColumns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 84 },
  { colKey: 'period_status', title: '批次状态', width: 100 },
  { colKey: 'wip_beg_qty', title: '期初在制', width: 96, align: 'right' },
  { colKey: 'completed_qty', title: '本期完工', width: 96, align: 'right' },
  { colKey: 'wip_end_qty', title: '期末在制', width: 96, align: 'right' },
  { colKey: 'equiv_conversion', title: '约当产量', width: 100, align: 'right' },
  { colKey: 'unit_finished_cost', title: '单位完工成本', width: 130, align: 'right' },
  { colKey: 'finished_cost', title: '完工转出', width: 120, align: 'right' },
];

/** 批号 / 订单合一行显示（列表没有开工日期列，靠这行给业务线索） */
function lotOrOrderText(row: CostObjectRow): string {
  const parts: string[] = [];
  if (row.lot_no) {
    parts.push(`批号 ${row.lot_no}`);
  }
  if (row.source_doc_no) {
    parts.push(`订单 ${row.source_doc_no}`);
  }
  return parts.length ? parts.join(' · ') : '无批号 / 订单';
}

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const range = monthRange(filter.month);
    const page = await fetchCostObjectPage({
      page: current.value,
      page_size: PAGE_SIZE,
      keyword: filter.keyword.trim(),
      status: filter.status,
      workshop_dept_id: filter.workshop_dept_id,
      start_from: range.start_from,
      start_to: range.start_to,
    });
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    loadError.value = errorText(error);
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadRows();
}

function handleReset(): void {
  filter.keyword = '';
  filter.status = '';
  filter.workshop_dept_id = '';
  filter.month = '';
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 下拉主数据 ====================

const products = ref<MaterialRow[]>([]);
const warehouses = ref<WarehouseTreeNode[]>([]);
const locations = ref<Location[]>([]);
const customers = ref<Customer[]>([]);
const optionsLoading = ref(false);
const locationLoading = ref(false);

const productOptions = computed(() =>
  products.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}${item.spec ? ` / ${item.spec}` : ''}`,
    value: item.id,
  })),
);

/** 仓库树拍平（缩进表示层级） */
const warehouseOptions = computed(() => {
  const out: Array<{ label: string; value: string }> = [];
  const walk = (nodes: WarehouseTreeNode[], depth: number): void => {
    for (const node of nodes) {
      if (node.status !== 1) {
        continue;
      }
      out.push({ label: `${'　'.repeat(depth)}${node.warehouse_name}`, value: node.id });
      if (node.children?.length) {
        walk(node.children, depth + 1);
      }
    }
  };
  walk(warehouses.value, 0);
  return out;
});

const locationOptions = computed(() =>
  locations.value.map((item) => ({
    label: `${item.location_code}${item.location_name ? ` ${item.location_name}` : ''}`,
    value: item.id,
  })),
);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id })),
);

/** 车间下拉（树拍平；表单用 DeptTreeSelect，筛选器用这个） */
const deptTree = ref<DeptNode[]>([]);

const deptOptions = computed(() => flattenDeptTree(deptTree.value));

function flattenDeptTree(nodes: DeptNode[], depth = 0): Array<{ label: string; value: string }> {
  const out: Array<{ label: string; value: string }> = [];
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
    const children = node.children ?? [];
    if (children.length) {
      out.push(...flattenDeptTree(children, depth + 1));
    }
  }
  return out;
}

/**
 * 产出品：只列 `FINISHED` / `SEMI`（设计 §6.2.1）。
 *
 * 物料列表接口一次只认一个 `material_type`，所以发两次再合并；
 * 各自 `page_size=200`（下拉够用），超了就让用户在下拉里搜编码。
 */
async function loadProducts(): Promise<void> {
  const types = ['FINISHED', 'SEMI'] as const;
  const pages = await Promise.all(
    types.map((type) =>
      fetchMaterialPage({ material_type: type, status: 1, page: 1, page_size: OPTION_LIMIT }),
    ),
  );
  products.value = pages.flatMap((page) => page.list ?? []);
}

async function loadWarehouses(): Promise<void> {
  warehouses.value = await fetchWarehouseTree();
}

async function loadCustomers(): Promise<void> {
  const page = await fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: OPTION_LIMIT });
  customers.value = page.list ?? [];
}

async function loadDepts(): Promise<void> {
  deptTree.value = await getDeptTree({ status: 1 });
}

/** 仓 → 库位联动（选仓后重新拉；清空仓则清空库位） */
async function handleWarehouseChange(): Promise<void> {
  form.target_location_id = '';
  locations.value = [];
  if (!form.target_warehouse_id) {
    return;
  }
  locationLoading.value = true;
  try {
    locations.value = await fetchLocationList(form.target_warehouse_id, false);
  } catch (error) {
    MessagePlugin.warning(`库位加载失败：${errorText(error)}`);
  } finally {
    locationLoading.value = false;
  }
}

async function loadOptions(): Promise<void> {
  optionsLoading.value = true;
  try {
    await Promise.all([loadProducts(), loadWarehouses(), loadCustomers(), loadDepts()]);
  } catch (error) {
    MessagePlugin.warning(`下拉主数据加载失败：${errorText(error)}`);
  } finally {
    optionsLoading.value = false;
  }
}

// ==================== 表单（建 / 改） ====================

const formVisible = ref(false);
const submitting = ref(false);
const formError = ref('');
const formErrors = ref<CostObjectFormErrors>({});
const editingId = ref('');
const editingOrigin = ref<CostObjectDetail | null>(null);
const form = reactive<CostObjectFormState>(emptyCostObjectForm());

/** 产出品锁定：已有归集就必须锁（后端 409「产出品不能再改」） */
const productLocked = computed(() => Boolean(editingId.value) && hasCollection(editingOrigin.value));
const productLockedHint = computed(() =>
  productLocked.value ? '已有归集，产出品不可改（要改请作废重建）' : '',
);

function formStatus(field: keyof CostObjectFormState): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

function resetFormState(): void {
  Object.assign(form, emptyCostObjectForm(todayString()));
  formErrors.value = {};
  formError.value = '';
  editingOrigin.value = null;
}

function openCreate(): void {
  editingId.value = '';
  resetFormState();
  locations.value = [];
  formVisible.value = true;
}

async function openEdit(row: CostObjectRow): Promise<void> {
  editingId.value = row.id;
  resetFormState();
  formVisible.value = true;
  try {
    const detail = await fetchCostObjectDetail(row.id);
    editingOrigin.value = detail;
    Object.assign(form, formFromDetail(detail));
    if (detail.target_warehouse_id) {
      locations.value = await fetchLocationList(detail.target_warehouse_id, false);
    }
  } catch (error) {
    formError.value = errorText(error);
  }
}

function closeForm(): void {
  formVisible.value = false;
  resetFormState();
  editingId.value = '';
}

async function submitForm(): Promise<void> {
  formError.value = '';
  const errors = validateCostObjectForm(form, { collected: productLocked.value });
  formErrors.value = errors;
  if (errorCount(errors) > 0) {
    formError.value = '还有必填项没填对，请看下方标红项';
    return;
  }
  submitting.value = true;
  try {
    if (editingId.value) {
      const origin = editingOrigin.value;
      if (!origin) {
        formError.value = '详情没加载成功，请关闭后重试';
        return;
      }
      const payload = toUpdatePayload(origin, form);
      if (isEmptyPayload(payload)) {
        MessagePlugin.info('没有改动');
        return;
      }
      const detail = await updateCostObject(editingId.value, payload);
      MessagePlugin.success(`已保存 ${detail.object_no}`);
    } else {
      const detail = await createCostObject(toCreatePayload(form));
      MessagePlugin.success(`已创建 ${detail.object_no}`);
    }
    formVisible.value = false;
    resetFormState();
    editingId.value = '';
    await loadRows();
  } catch (error) {
    formError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<CostObjectDetail | null>(null);

async function openDetail(row: CostObjectRow): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detail.value = null;
  try {
    detail.value = await fetchCostObjectDetail(row.id);
  } catch (error) {
    MessagePlugin.error(`详情加载失败：${errorText(error)}`);
    detailVisible.value = false;
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 关批 ====================

const closeVisible = ref(false);
const closeSubmitting = ref(false);
const closeTarget = ref<CostObjectDetail | null>(null);
const closeSummary = ref<CloseSummary | null>(null);
const closeConfirmed = ref(false);
const forceWipChecked = ref(false);
const closeRemark = ref('');
const closeError = ref('');

/** 行上点「关批」：先取详情（关批摘要要归集成本与在制数量，列表行没有） */
async function openClose(row: CostObjectRow): Promise<void> {
  closeError.value = '';
  closeConfirmed.value = false;
  forceWipChecked.value = false;
  closeRemark.value = '';
  closeVisible.value = true;
  closeTarget.value = null;
  closeSummary.value = null;
  try {
    const loaded = await fetchCostObjectDetail(row.id);
    closeTarget.value = loaded;
    closeSummary.value = closeSummaryOf(loaded);
  } catch (error) {
    closeError.value = errorText(error);
  }
}

/** 详情抽屉里点「关批」：详情已在手，直接用 */
function openCloseFromDetail(): void {
  if (!detail.value) {
    return;
  }
  closeError.value = '';
  closeConfirmed.value = false;
  forceWipChecked.value = false;
  closeRemark.value = '';
  closeTarget.value = detail.value;
  closeSummary.value = closeSummaryOf(detail.value);
  closeVisible.value = true;
}

async function submitClose(): Promise<void> {
  const target = closeTarget.value;
  const summary = closeSummary.value;
  if (!target || !summary) {
    closeError.value = '详情没加载成功，请关闭后重试';
    return;
  }
  closeError.value = '';
  if (!closeConfirmed.value) {
    closeError.value = '请先勾选确认「关批后不可回在产」';
    return;
  }
  if (summary.need_force_wip && !forceWipChecked.value) {
    closeError.value = `期末在制 ${summary.wip_qty} 不为 0：请勾选「带在制关批」，或先结算掉在制数量`;
    return;
  }
  // 第二段确认（高危动作：关批不可逆，且影响成本归集基线）
  const confirmed = await new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: '最后确认',
      body:
        `即将关批 ${summary.object_no}（${summary.product}），已归集成本 ${formatMoney(summary.collect_total)}。` +
        (summary.need_force_wip ? `带在制 ${summary.wip_qty} 关批。` : '') +
        '关批后不可回「在产」。',
      theme: 'warning',
      confirmBtn: '确认关批',
      onConfirm: () => {
        dialog.hide();
        resolve(true);
      },
      onClose: () => resolve(false),
      onCancel: () => resolve(false),
    });
  });
  if (!confirmed) {
    return;
  }
  closeSubmitting.value = true;
  try {
    const payload = toClosePayload({
      forceWip: summary.need_force_wip ? forceWipChecked.value : false,
      remark: closeRemark.value,
    });
    const closed = await closeCostObject(target.id, payload);
    MessagePlugin.success(`已关批 ${closed.object_no}`);
    closeVisible.value = false;
    if (detail.value?.id === closed.id) {
      detail.value = closed;
    }
    await loadRows();
  } catch (error) {
    // 5 条前置拦下 → 409 + 66055，msg 逐条列原因；这里补「去哪修」
    closeError.value = closeErrorText(error);
  } finally {
    closeSubmitting.value = false;
  }
}

// ==================== 错误文案 ====================

/** 结构化错误 → 人话（`ApiError.message` 已带请求 ID，原样用） */
function errorText(error: unknown): string {
  if (typeof error === 'string') {
    return error.trim();
  }
  if (error && typeof error === 'object') {
    const like = error as { message?: string; code?: number };
    const message = (like.message ?? '').trim();
    if (message) {
      return message;
    }
    if (typeof like.code === 'number') {
      return `请求失败（业务码 ${like.code}）`;
    }
  }
  return '请求失败，请重试';
}

onMounted(() => {
  void loadOptions();
  void loadRows();
});
</script>

<style scoped>
.cost-object-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.list-card {
  border-radius: 8px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__status {
  width: 118px;
}

.tool-row__dept {
  width: 150px;
}

.tool-row__month {
  width: 132px;
}

.tool-row__keyword {
  width: 220px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.panel-head__meta {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.list-alert,
.form-alert {
  margin-bottom: 8px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.name-cell__main {
  font-weight: 500;
}

.name-cell__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.num {
  font-variant-numeric: tabular-nums;
}

.num--warn {
  color: var(--td-warning-color);
  font-weight: 600;
}

.money {
  font-variant-numeric: tabular-nums;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.op-cell__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.form-footnote {
  margin: 8px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.section-title {
  margin: 16px 0 8px;
  font-size: 14px;
  font-weight: 600;
}

.summary-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px;
}

.summary-cell {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
}

.summary-cell--total {
  background: var(--td-bg-color-container-hover);
}

.summary-cell__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.summary-cell__value {
  font-size: 16px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.detail-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 16px;
}

.muted {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.close-facts {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 16px;
  margin: 0 0 12px;
  padding: 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
}

.close-facts dt {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.close-facts dd {
  margin: 2px 0 0;
  font-size: 14px;
}

.close-force {
  display: block;
  margin-top: 8px;
}

.close-remark {
  margin-top: 12px;
}

.close-tip {
  margin: 8px 0 0;
}
</style>
