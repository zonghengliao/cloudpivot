<!--
  成本核算页（COST-B3-11 / ARCH-9 §6.1 #5 · §6.2.2 五步向导）。

  一页两件事：**批次列表** + **五步核算向导**（抽屉）。

  ```
  ① 选期间 → ② 料工费确认（缺口自查）→ ③ 试算（成本计算单）→ ④ 提交审批 → ⑤ 结算
  ```

  # 为什么是「列表 + 抽屉向导」而不是纯向导

  一个期间同时只有一个活跃批次（`uk_biz_cost_period`），但历史上每个期间都会留一条：
  已结算 / 已红冲 / 已作废的批次是审计凭据，必须能翻回去看（凭证 id、红冲原因、结算人）。
  所以列表是「过去」，抽屉向导是「这一批现在走到哪」。

  # 每一步的数据来源（只有一条数据流）

  | 步 | 数据 | 说明 |
  |---|---|---|
  | ① | `GET /erp/period/page` | 会计期间状态：`CLOSED` 的期间置灰（设计 §6.2.2 ①：需先反结账） |
  | ② | `GET /cost-period/uncollected?period_code=` | 四块缺口全由后端给（`UncollectedPanel`） |
  | ③ | `GET /cost-period/{id}` 的 `items` | 成本计算单；`DRAFT` 阶段单头 `total_*` 恒 0，合计用现算值 |
  | ④ | `POST /cost-period/{id}/submit` → `GET /business/flow/instance/{id}` | 提交同事务发起审批实例，进度条只读引擎节点行 |
  | ⑤ | `POST /cost-period/{id}/settle` / `.../reverse` | 结算与红冲都走「关键字确认」高危框 |

  # 没有「重算」按钮能点（已报备的缺口）

  设计 §5.2 #27 `POST /business/erp/cost-period/calc`（六段串联的计算编排）**尚未落地**，
  所以步③ 的「重算」按钮是**禁用 + tooltip 说明**，不是「点了没反应」也不是前端本地算一份
  （口径在后端，前端算的数没人认）。等编排卡上线，只需在 `recalcCostPeriod()` 里接一个
  POST 并在 `handleRecalc` 里调用，其它不用改。
-->
<template>
  <div class="cost-period-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.calc]"
          theme="primary"
          data-testid="cp-btn-new"
          @click="openWizard(null)"
        >
          <template #icon><t-icon name="add" /></template>
          新建核算批次
        </t-button>
        <t-button variant="outline" data-testid="cp-btn-refresh" @click="refreshAll">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.period_code"
          class="tool-row__period"
          :options="periodFilterOptions"
          placeholder="全部期间"
          clearable
          filterable
          data-testid="cp-filter-period"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="cp-filter-status"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="批次号 / 备注"
          clearable
          data-testid="cp-filter-keyword"
          @enter="handleSearch"
          @clear="handleSearch"
        />
        <t-button variant="outline" data-testid="cp-btn-search" @click="handleSearch">查询</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 批次列表 ==================== -->
    <t-card :bordered="false" class="table-card">
      <t-alert v-if="loadError" theme="error" class="table-alert" data-testid="cp-list-error">
        <template #message>{{ loadError }}</template>
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
        data-testid="cp-table"
        @page-change="handlePageChange"
      >
        <template #batch_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="cp-row-open" @click="openWizard(row)">
            {{ row.batch_no }}
          </t-link>
        </template>
        <template #period_code="{ row }"><span class="mono">{{ row.period_code }}</span></template>
        <template #status="{ row }">
          <t-tag size="small" :theme="statusTheme(row.status)" variant="light">
            {{ statusLabel(row.status) }}
          </t-tag>
        </template>
        <template #object_count="{ row }"><span class="num">{{ row.object_count }}</span></template>
        <template #total_cost="{ row }">
          <span class="money">{{ formatMoney(row.total_cost) }}</span>
        </template>
        <template #total_finished="{ row }">
          <span class="money">{{ formatMoney(row.total_finished) }}</span>
        </template>
        <template #total_wip="{ row }"><span class="money">{{ formatMoney(row.total_wip) }}</span></template>
        <template #settled_at="{ row }">
          <span class="mono">{{ dash(row.settled_at ? shortDate(row.settled_at) : '') }}</span>
        </template>
        <template #created_by_name="{ row }">
          {{ row.created_by_name || '—' }}
        </template>
        <template #ops="{ row }">
          <div class="row-ops">
            <t-link theme="primary" @click="openWizard(row)">继续核算</t-link>
            <t-link
              v-if="canReverse(row.status)"
              v-privilege="[PERM.reverse]"
              theme="danger"
              data-testid="cp-row-reverse"
              @click="openReverse(row.id)"
            >
              红冲
            </t-link>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 五步核算向导 ==================== -->
    <t-drawer
      v-model:visible="wizardVisible"
      :header="wizardTitleText"
      size="1080px"
      :footer="false"
      :close-on-overlay-click="false"
      destroy-on-close
      data-testid="cp-wizard"
    >
      <div class="wizard">
        <t-steps :current="step" class="wizard__steps" data-testid="cp-steps">
          <t-step-item
            v-for="(item, index) in WIZARD_STEPS"
            :key="item.key"
            :title="item.title"
            :content="step === index ? item.hint : undefined"
          />
        </t-steps>

        <t-alert v-if="wizardError" theme="error" class="wizard__alert" data-testid="cp-wizard-error">
          <template #message><span class="pre">{{ wizardError }}</span></template>
        </t-alert>

        <!-- ---------------- 步 ① 选期间 ---------------- -->
        <section v-if="step === 0" class="pane" data-testid="cp-pane-period">
          <div class="pane__field">
            <div class="pane__label">
              会计期间 <span class="pane__req">*</span>
              <span class="pane__hint">只列未结账期间；已结账期间要先反结账才能核算</span>
            </div>
            <t-select
              v-model="wizardPeriod"
              class="pane__period"
              :options="periodOptions"
              placeholder="选择期间（yyyy-MM）"
              filterable
              data-testid="cp-period-select"
              @change="handlePeriodChange"
            />
            <p class="pane__foot">
              一个期间同时只有一个活跃批次（作废 / 红冲的不占位）；
              期间状态来自会计期间表，不是成本模块自己的状态。
            </p>
          </div>

          <t-alert
            v-if="periodClosed"
            theme="warning"
            class="pane__alert"
            data-testid="cp-period-closed"
          >
            会计期间 {{ wizardPeriod }} 已结账，不能核算：请先反结账（财务总监），再回来选这个期间
          </t-alert>

          <t-alert
            v-if="existingBatch"
            theme="info"
            class="pane__alert"
            data-testid="cp-period-existing"
          >
            期间 {{ wizardPeriod }} 已有批次
            <b class="mono">{{ existingBatch.batch_no }}</b>
            （{{ statusLabel(existingBatch.status) }}）：
            <t-link theme="primary" @click="openWizard(existingBatch)">打开这个批次继续</t-link>
            <span v-if="isFinalStatus(existingBatch.status)">；该批次已终结，可以新建下一批</span>
          </t-alert>

          <div class="pane__field">
            <div class="pane__label">
              人工成本来源
              <span class="pane__hint">
                MANUAL = 手工台账（一期）；PAYROLL = 薪资单（ARCH9-2 上线后切换，两者同计就是双计提）
              </span>
            </div>
            <t-radio-group v-model="form.labor_source" variant="default-filled" data-testid="cp-labor-source">
              <t-radio-button value="MANUAL">手工台账</t-radio-button>
              <t-radio-button value="PAYROLL">薪资单</t-radio-button>
            </t-radio-group>
          </div>

          <div class="pane__field">
            <div class="pane__label">备注</div>
            <t-textarea
              v-model="form.remark"
              :maxlength="500"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="选填：本期核算的口径说明（红冲/重算时会保留）"
              data-testid="cp-remark"
            />
          </div>

          <div v-if="batch && canEdit(batch.status)" class="pane__row-ops">
            <t-button
              v-privilege="[PERM.calc]"
              variant="outline"
              theme="primary"
              :loading="acting"
              data-testid="cp-btn-save-draft"
              @click="handleSaveDraft"
            >
              保存人工来源 / 备注
            </t-button>
            <span class="pane__meta">只有草稿能改这两个字段（提交后数据冻结）</span>
          </div>
        </section>

        <!-- ---------------- 步 ② 料工费确认（缺口自查） ---------------- -->
        <section v-else-if="step === 1" class="pane" data-testid="cp-pane-check">
          <p class="pane__lead">
            自动检查「没挂成本对象的领料 / 没工时或没费率的在产对象 / 没摊完的费用池」。
            <b>不阻断</b>：可以带着缺口算，但结算前每一块都要能解释。
          </p>
          <UncollectedPanel :period-code="wizardPeriod" show-recalc @recalc="goStep(2)" />
        </section>

        <!-- ---------------- 步 ③ 试算（成本计算单） ---------------- -->
        <section v-else-if="step === 2" class="pane" data-testid="cp-pane-calc">
          <div class="pane__row-ops">
            <t-button
              v-privilege="[PERM.calc]"
              theme="primary"
              variant="outline"
              :disabled="!canRecalc(batch?.status ?? null)"
              data-testid="cp-btn-recalc"
              @click="handleRecalc"
            >
              <template #icon><t-icon name="refresh" /></template>
              重算
            </t-button>
            <t-tooltip
              content="重算会清空本期归集重算（只有草稿能重算）。计算编排入口（设计 §5.2 #27）尚未上线：按钮已就位，接口落地后即可点。"
            >
              <t-icon name="help-circle" class="pane__help" />
            </t-tooltip>
            <!-- 导出（设计 §5.2 #32）：与「看计算单」同权限码；批次还没建时无处可导 -->
            <t-button
              v-privilege="[PERM.list]"
              variant="outline"
              :disabled="!batch"
              :loading="exporting"
              data-testid="cp-btn-export"
              @click="handleExportSheet"
            >
              <template #icon><t-icon name="download" /></template>
              导出 xlsx
            </t-button>
            <span class="pane__spacer" />
            <span v-if="batch" class="pane__meta" data-testid="cp-batch-meta">
              批次 <b class="mono">{{ batch.batch_no }}</b> · {{ statusLabel(batch.status) }} ·
              人工来源 {{ laborSourceLabel(batch.labor_source) }} · 制单 {{ batch.created_by_name || '—' }}
            </span>
          </div>

          <CalcSheetPanel
            :items="batch?.items ?? []"
            :summary="batch?.items_summary ?? null"
            :loading="detailLoading"
            :show-create="!batch"
            :creating="acting"
            :notice="calcNotice"
            @create="handleCreateAndCalc"
          />
        </section>

        <!-- ---------------- 步 ④ 提交审批 ---------------- -->
        <section v-else-if="step === 3" class="pane" data-testid="cp-pane-approve">
          <t-alert
            v-if="batch && canSubmit(batch.status)"
            theme="info"
            class="pane__alert"
            data-testid="cp-submit-ready"
          >
            当前是草稿，可以提交审批：提交后批次数据冻结（不能改数），并进入「财务经理 → 财务总监」两级顺序审批。
          </t-alert>

          <!-- 审批进度：只来自引擎写的节点行 -->
          <template v-if="hasApprovalProgress(approvalProgress)">
            <div class="pane__section-title">{{ APPROVAL_PROGRESS_TITLE }}</div>
            <FlowProgress
              :nodes="approvalProgress.nodes"
              :tasks="approvalProgress.tasks"
              data-testid="cp-approval-progress"
            />
          </template>
          <t-alert
            v-else-if="fallback"
            theme="info"
            class="pane__alert"
            data-testid="cp-approval-fallback"
          >
            {{ fallback.label }} <span class="mono">{{ fallback.no }}</span>
            ：进度节点拉不到（权限 / 实例已归档），可到「审批中心」按实例号查看。
          </t-alert>
          <t-alert
            v-else-if="batch"
            class="pane__alert"
            data-testid="cp-approval-none"
          >
            还没有审批实例：草稿批次要提交后才有审批进度。
          </t-alert>

          <div class="pane__row-ops pane__row-ops--top">
            <t-button
              v-if="batch && canSubmit(batch.status)"
              v-privilege="[PERM.submit]"
              theme="primary"
              :loading="acting"
              data-testid="cp-btn-submit-open"
              @click="submitPreviewVisible = true"
            >
              提交审批
            </t-button>
            <t-button
              v-if="batch && canWithdraw(batch.status)"
              v-privilege="[PERM.submit]"
              theme="warning"
              variant="outline"
              :loading="acting"
              data-testid="cp-btn-withdraw"
              @click="handleWithdraw"
            >
              撤回审批
            </t-button>
            <t-button
              v-if="batch && canCancel(batch.status)"
              v-privilege="[PERM.submit]"
              variant="outline"
              theme="danger"
              :loading="acting"
              data-testid="cp-btn-cancel"
              @click="openCancelDialog"
            >
              作废批次
            </t-button>
            <span class="pane__spacer" />
            <t-button variant="outline" data-testid="cp-btn-goto-check" @click="goStep(1)">
              回看料工费
            </t-button>
          </div>
        </section>

        <!-- ---------------- 步 ⑤ 结算 / 红冲 ---------------- -->
        <section v-else class="pane" data-testid="cp-pane-settle">
          <t-alert
            v-if="batch"
            :theme="batch.status === 'SETTLED' || batch.status === 'CLOSED' ? 'success' : 'info'"
            class="pane__alert"
            data-testid="cp-settle-plan"
          >
            <template #message>
              <span>{{ settlePlanLine }}</span>
            </template>
          </t-alert>

          <div v-if="batch" class="settle-facts" data-testid="cp-settle-facts">
            <div class="settle-facts__item">
              <span class="settle-facts__label">成本对象</span>
              <span class="settle-facts__value num">{{ batch.items_summary.object_count }}</span>
            </div>
            <div class="settle-facts__item">
              <span class="settle-facts__label">投入合计</span>
              <span class="settle-facts__value money">{{ formatMoney(batch.items_summary.total_cost) }}</span>
            </div>
            <div class="settle-facts__item">
              <span class="settle-facts__label">完工转出</span>
              <span class="settle-facts__value money">{{ formatMoney(batch.items_summary.total_finished) }}</span>
            </div>
            <div class="settle-facts__item">
              <span class="settle-facts__label">期末在制</span>
              <span class="settle-facts__value money">{{ formatMoney(batch.items_summary.total_wip) }}</span>
            </div>
            <div class="settle-facts__item">
              <span class="settle-facts__label">结算时间</span>
              <span class="settle-facts__value mono">{{ batch.settled_at ? shortDate(batch.settled_at) : '—' }}</span>
            </div>
            <div class="settle-facts__item">
              <span class="settle-facts__label">结算人</span>
              <span class="settle-facts__value">{{ batch.settled_by_name || '—' }}</span>
            </div>
          </div>

          <div v-if="vouchers.length > 0" class="settle-vouchers" data-testid="cp-settle-vouchers">
            <span class="settle-vouchers__title">结算生成的凭证</span>
            <span v-for="voucher in vouchers" :key="voucher.id" class="settle-vouchers__item">
              {{ voucher.label }} <span class="mono">{{ shortId(voucher.id) }}</span>
            </span>
          </div>

          <t-alert
            v-if="batch && batch.status === 'REVERSED'"
            theme="error"
            class="pane__alert"
            data-testid="cp-reversed-info"
          >
            已于 {{ shortDate(batch.reversed_at || '') }} 红冲：{{ batch.reverse_reason }}。
            同期间可以新建下一批（红冲批次不占位）。
          </t-alert>

          <div class="pane__row-ops">
            <t-button
              v-if="batch"
              v-privilege="[PERM.settle]"
              theme="primary"
              :disabled="!canSettle(batch.status)"
              :loading="acting"
              data-testid="cp-btn-settle"
              @click="openSettleDialog(batch.id)"
            >
              结算（入库 + 凭证）
            </t-button>
            <t-button
              v-if="batch && canReverse(batch.status)"
              v-privilege="[PERM.reverse]"
              theme="danger"
              variant="outline"
              :loading="acting"
              data-testid="cp-btn-reverse"
              @click="openReverse(batch.id)"
            >
              红冲结算
            </t-button>
            <span class="pane__spacer" />
            <span v-if="batch" class="pane__meta" data-testid="cp-settle-hint">
              {{ settleBlockedHint(batch.status) }}
            </span>
          </div>
        </section>

        <!-- ---------------- 抽屉页脚 ---------------- -->
        <div class="wizard__foot">
          <t-button variant="outline" :disabled="step === 0" data-testid="cp-btn-prev" @click="goStep(step - 1)">
            上一步
          </t-button>
          <t-button
            v-if="step < LAST_STEP"
            theme="primary"
            :disabled="!canNext"
            data-testid="cp-btn-next"
            @click="goStep(step + 1)"
          >
            下一步
          </t-button>
          <span class="wizard__spacer" />
          <span class="wizard__foot-hint" data-testid="cp-foot-hint">{{ footHint }}</span>
          <t-button variant="outline" data-testid="cp-btn-close" @click="wizardVisible = false">关闭</t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ④ 提交审批：流程预览 ==================== -->
    <t-dialog
      v-model:visible="submitPreviewVisible"
      header="提交审批确认"
      :confirm-btn="{ content: '提交审批', theme: 'primary', loading: acting }"
      :cancel-btn="{ content: '再看看', variant: 'outline' }"
      width="520px"
      data-testid="cp-submit-preview"
      @confirm="handleSubmit"
    >
      <div class="preview">
        <p class="preview__lead">
          批次 <b class="mono">{{ batch?.batch_no }}</b>（期间 {{ batch?.period_code }}）提交后：
        </p>
        <ol class="preview__list">
          <li v-for="line in FLOW_PREVIEW_LINES" :key="line">{{ line }}</li>
        </ol>
        <p class="preview__foot">
          没有已发布的「成本结算审批」流程时，后端会**拒绝提交**并提示先去「审批流程」发布
          （不静默直过）。
        </p>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 结算 / 红冲：高危确认 ==================== -->
    <RiskConfirmDialog
      v-model:visible="settleDialogVisible"
      mode="settle"
      :plan="settlePlanLine"
      :loading="acting"
      :error="settleError"
      @confirm="handleSettle"
    />
    <RiskConfirmDialog
      v-model:visible="reverseDialogVisible"
      mode="reverse"
      :plan="reversePlanLine"
      :loading="acting"
      :error="reverseError"
      @confirm="handleReverse"
    />

    <!-- ==================== 作废批次（原因 ≥10 字） ==================== -->
    <t-dialog
      v-model:visible="cancelDialogVisible"
      header="作废核算批次"
      :confirm-btn="{ content: '确认作废', theme: 'danger', loading: acting, disabled: !cancelReasonOk }"
      :cancel-btn="{ content: '取消', variant: 'outline' }"
      width="480px"
      data-testid="cp-cancel-dialog"
      @confirm="handleCancel"
    >
      <p class="preview__lead">
        作废后批次置「已作废」、不再占位（同期间可重开）。作废不生成任何凭证，也**不会**撤销已生成的单据。
      </p>
      <t-textarea
        v-model="cancelReason"
        :maxlength="255"
        :autosize="{ minRows: 2, maxRows: 4 }"
        placeholder="作废原因（至少 10 个字，写进状态日志）"
        data-testid="cp-cancel-reason"
      />
      <p v-if="cancelReasonHint" class="preview__error" data-testid="cp-cancel-reason-error">
        {{ cancelReasonHint }}
      </p>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 成本核算页（COST-B3-11）。
 *
 * 口径与手势：
 * - **按钮按状态 + 权限两层收敛**：状态决定「现在能不能」（`cost-period-logic` 的
 *   `canSubmit/canSettle/...`），权限决定「这个人能不能」（`v-privilege`）——
 *   两层都过才渲染；
 * - **动作后一律重取详情**（不信本地拼），并刷新列表（状态/金额都变了）；
 * - **错误原样展示 + 补一句怎么修**：后端的 409/422 报文已点名批次号与差额
 *   （`utils::db_error` / `cost_period_dto` 的文案），前端不再改写措辞。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute } from 'vue-router';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  cancelCostPeriod,
  createCostPeriod,
  exportCostSheet,
  fetchCostPeriodDetail,
  fetchCostPeriodPage,
  reverseCostPeriod,
  settleCostPeriod,
  submitCostPeriod,
  updateCostPeriod,
  withdrawCostPeriod,
  COST_PERIOD_PERMISSIONS,
  COST_PERIOD_STATUSES,
  PAGE_SIZE,
  type CostPeriodDetail,
  type CostPeriodRow,
  type CostPeriodStatus,
  type LaborSource,
} from '@/api/business/erp/cost-period';
import { fetchPeriodPage, type AccountPeriodRow } from '@/api/business/erp';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import { formatMoney, dash, shortDate, shortId, todayPeriod } from '@/views/business/erp/shared/money';
import { errorText } from '@/views/business/erp/shared/error-text';
import { saveBlob } from '@/utils/download';
import {
  EMPTY_APPROVAL_PROGRESS,
  approvalFallbackOf,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import {
  APPROVAL_PROGRESS_TITLE,
  FLOW_PREVIEW_LINES,
  LAST_STEP,
  WIZARD_STEPS,
  canCancel,
  canEdit,
  canNextFrom,
  canRecalc,
  canReverse,
  canSettle,
  canSubmit,
  canWithdraw,
  costSheetFileName,
  isFinalStatus,
  laborSourceLabel,
  reasonHint,
  reversePlanText,
  settleBlockedHint,
  settleSummaryText,
  statusLabel,
  statusTheme,
  stepOfStatus,
  wizardTitle,
} from './cost-period-logic';
import UncollectedPanel from './steps/UncollectedPanel.vue';
import CalcSheetPanel from './steps/CalcSheetPanel.vue';
import RiskConfirmDialog from './steps/RiskConfirmDialog.vue';

/** 本页权限码（V0378 种子逐一对应） */
const PERM = COST_PERIOD_PERMISSIONS;

/** 路由（读深链 `?batch_no=`） */
const route = useRoute();

// ==================== 列表 ====================

const filter = reactive<{ period_code: string; status: CostPeriodStatus | ''; keyword: string }>({
  period_code: '',
  status: '',
  keyword: '',
});

const rows = ref<CostPeriodRow[]>([]);
const loading = ref(false);
const loadError = ref('');
const total = ref(0);
const page = ref(1);

/** 会计期间（用于 ① 期间下拉：只列未结账；已结账置灰并说明） */
const periods = ref<AccountPeriodRow[]>([]);

const pagination = computed(() => ({
  current: page.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  pageSizeOptions: [PAGE_SIZE],
}));

const statusFilterOptions = COST_PERIOD_STATUSES.map((status) => ({
  label: statusLabel(status),
  value: status,
}));

/** 列表筛选的期间下拉：用本页出现过的期间 + 全部会计期间（去重，倒序） */
const periodFilterOptions = computed(() => {
  const set = new Set<string>(periods.value.map((item) => item.period_code));
  for (const row of rows.value) {
    set.add(row.period_code);
  }
  return [...set]
    .sort((left, right) => (left < right ? 1 : -1))
    .map((code) => ({ label: periodLabel(code), value: code }));
});

/** 期间下拉选项（步①：未结账可选，已结账置灰） */
const periodOptions = computed(() =>
  periods.value
    .slice()
    .sort((left, right) => (left.period_code < right.period_code ? 1 : -1))
    .map((item) => ({
      label: periodLabel(item.period_code, item.status === 'CLOSED'),
      value: item.period_code,
      disabled: item.status === 'CLOSED',
    })),
);

function periodLabel(code: string, closed = false): string {
  return closed ? `${code}（已结账，需先反结账）` : code;
}

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchCostPeriodPage({
      ...filter,
      page: page.value,
      page_size: PAGE_SIZE,
    });
    rows.value = data.list;
    total.value = data.total;
  } catch (error) {
    loadError.value = errorText(error, '加载成本核算批次失败');
  } finally {
    loading.value = false;
  }
}

async function loadPeriods(): Promise<void> {
  try {
    const data = await fetchPeriodPage({ page: 1, page_size: 100 });
    periods.value = data.list;
  } catch {
    // 期间拉不到不影响核算（只是步①下拉为空）：不弹窗、不阻断，用户仍可从列表进批次
    periods.value = [];
  }
}

function handleSearch(): void {
  page.value = 1;
  void loadRows();
}

function handlePageChange(info: { current: number }): void {
  page.value = info.current;
  void loadRows();
}

function refreshAll(): void {
  void loadRows();
  void loadPeriods();
  if (wizardVisible.value && batch.value) {
    void loadDetail(batch.value.id);
  }
}

// ==================== 向导 ====================

const wizardVisible = ref(false);
const step = ref(0);
const wizardPeriod = ref('');
const batch = ref<CostPeriodDetail | null>(null);
const detailLoading = ref(false);
const acting = ref(false);
const wizardError = ref('');

/** 步① 表单（建批次时提交；`DRAFT` 下改它走 PUT） */
const form = reactive<{ labor_source: LaborSource; remark: string }>({
  labor_source: 'MANUAL',
  remark: '',
});

/** 当前期间已存在的活跃批次（步① 提示用） */
const existingBatch = ref<CostPeriodRow | null>(null);

/** 期间是否已结账 */
const periodClosed = computed<boolean>(() =>
  periods.value.some((item) => item.period_code === wizardPeriod.value && item.status === 'CLOSED'),
);

const wizardTitleText = computed<string>(() =>
  wizardTitle(wizardPeriod.value, batch.value?.batch_no ?? null),
);

/** 步③ 的真实现状说明（计算编排未上线） */
const calcNotice = computed<string>(() =>
  batch.value
    ? '计算结果由后端「计算」流程写入；设计 §5.2 #27 的计算编排入口尚未上线，所以「重算」按钮暂不可点。'
    : '',
);

/** 步⑤ 顶部文案（未结算「将生成」/ 已结算「已生成」） */
const settlePlanLine = computed<string>(() => settleSummaryText(batch.value));

/** 红冲确认框要展示的批次（列表里直接点红冲时，batch 可能还没加载） */
const reverseTarget = ref<CostPeriodDetail | null>(null);
const reversePlanLine = computed<string>(() => reversePlanText(reverseTarget.value ?? batch.value));

/** 结算后生成的凭证（空 id 不显示） */
const vouchers = computed(() =>
  batch.value
    ? [
        { label: '人工计提', id: batch.value.labor_voucher_id },
        { label: '制造费用', id: batch.value.overhead_voucher_id },
        { label: '完工结转', id: batch.value.finish_voucher_id },
      ].filter((item): item is { label: string; id: string } => Boolean(item.id))
    : [],
);

/** 审批进度（只来自引擎节点行） */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

const fallback = computed(() =>
  approvalFallbackOf({
    status: batch.value?.status ?? null,
    approval_instance_id: batch.value?.approval_instance_id ?? null,
    approval_instance_no: null,
  }),
);

/** 打开向导：`row` 为空 = 新建（步①） */
async function openWizard(row: CostPeriodRow | null): Promise<void> {
  wizardVisible.value = true;
  wizardError.value = '';
  wizardPeriod.value = row?.period_code ?? todayPeriod();
  form.labor_source = row?.labor_source ?? 'MANUAL';
  form.remark = row?.remark ?? '';
  existingBatch.value = null;
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };

  if (row) {
    batch.value = null;
    const detail = await loadDetail(row.id);
    step.value = stepOfStatus(detail?.status ?? row.status);
  } else {
    batch.value = null;
    step.value = 0;
    await checkExistingBatch();
  }
}

/** 步①：这个期间有没有批次（只提示，不阻断 —— 用户可点「打开这个批次」） */
async function checkExistingBatch(): Promise<void> {
  existingBatch.value = null;
  if (!wizardPeriod.value) {
    return;
  }
  try {
    const data = await fetchCostPeriodPage({ period_code: wizardPeriod.value, page: 1, page_size: 3 });
    existingBatch.value =
      data.list.find((item) => !isFinalStatus(item.status)) ?? data.list[0] ?? null;
  } catch {
    existingBatch.value = null;
  }
}

/** 取批次详情（单头 + 计算单 + 审批进度）；返回详情供调用方决定停在哪一步 */
async function loadDetail(id: string): Promise<CostPeriodDetail | null> {
  detailLoading.value = true;
  try {
    const detail = await fetchCostPeriodDetail(id);
    batch.value = detail;
    wizardPeriod.value = detail.period_code;
    form.labor_source = detail.labor_source;
    form.remark = detail.remark ?? '';
    await loadProgress(detail.approval_instance_id);
    return detail;
  } catch (error) {
    wizardError.value = errorText(error, '加载批次详情失败');
    return null;
  } finally {
    detailLoading.value = false;
  }
}

/** 审批进度：拉不到就退化成「审批单号」，不让整块失败 */
async function loadProgress(instanceId: string | null): Promise<void> {
  approvalProgress.value = await loadApprovalProgress(instanceId, getFlowInstance);
}

/** 步间跳转（① 必须有合法期间；④ 之后必须有批次） */
function goStep(target: number): void {
  if (target < 0 || target > LAST_STEP) {
    return;
  }
  // 第 ③ 步正是「建批次 + 看计算单」的地方，所以只有 ④ 起才强制要求已有批次
  if (target >= 3 && !batch.value) {
    MessagePlugin.warning('先在第 ③ 步建批次：提交审批是对着一张核算批次提交的');
    return;
  }
  wizardError.value = '';
  step.value = target;
}

/** 下一步是否可点 */
const canNext = computed<boolean>(() => canNextFrom(step.value, {
  period: wizardPeriod.value,
  periodClosed: periodClosed.value,
  hasBatch: Boolean(batch.value),
}));

/** 页脚提示（说明为什么「下一步」点不了） */
const footHint = computed<string>(() => {
  if (step.value === 0) {
    if (!wizardPeriod.value) {
      return '先选会计期间';
    }
    if (periodClosed.value) {
      return '期间已结账，需先反结账';
    }
    if (existingBatch.value && !isFinalStatus(existingBatch.value.status)) {
      return `期间已有活跃批次 ${existingBatch.value.batch_no}：点上面「打开这个批次继续」`;
    }
    return '';
  }
  if (step.value === 2 && !batch.value) {
    return '还没有批次：点「建批次」开始核算';
  }
  return '';
});

async function handlePeriodChange(): Promise<void> {
  await checkExistingBatch();
}

// ==================== 动作 ====================

/** 建批次（`POST /cost-period`，权限 `cost-period:calc`） */
async function handleCreateAndCalc(): Promise<void> {
  if (!wizardPeriod.value) {
    wizardError.value = '先选会计期间';
    return;
  }
  acting.value = true;
  wizardError.value = '';
  try {
    const detail = await createCostPeriod({
      period_code: wizardPeriod.value,
      labor_source: form.labor_source,
      remark: form.remark.trim() || undefined,
    });
    batch.value = detail;
    step.value = 2;
    MessagePlugin.success(`已建批次 ${detail.batch_no}（草稿）`);
    await loadRows();
  } catch (error) {
    wizardError.value = errorText(error, '新建核算批次失败');
  } finally {
    acting.value = false;
  }
}

/**
 * 保存草稿的人工来源 / 备注（`PUT /cost-period/{id}`，只有 `DRAFT` 可改）。
 *
 * 与「建批次」分开：建批次是**一次性动作**（会占掉期间的活跃批次位），
 * 改数应该走 PUT，不能靠再建一次（那会 409）。
 */
async function handleSaveDraft(): Promise<void> {
  if (!batch.value) {
    return;
  }
  acting.value = true;
  wizardError.value = '';
  try {
    const detail = await updateCostPeriod(batch.value.id, {
      labor_source: form.labor_source,
      remark: form.remark.trim(),
    });
    batch.value = detail;
    MessagePlugin.success('已保存');
    await loadRows();
  } catch (error) {
    wizardError.value = errorText(error, '保存失败');
  } finally {
    acting.value = false;
  }
}

/**
 * 重算（设计 §5.2 #27）：**接口尚未上线**，按钮置灰 + tooltip 说明。
 * 这里保留调用位，编排卡落地后把实现换成 `POST /cost-period/calc` 即可。
 */
function handleRecalc(): void {
  if (!canRecalc(batch.value?.status ?? null)) {
    MessagePlugin.warning('只有草稿能重算（提交后数据冻结）');
    return;
  }
  MessagePlugin.info(
    '计算编排入口（设计 §5.2 #27 POST /cost-period/calc）尚未上线，暂不能在页面上重算；' +
      '当前展示的是后端已写入的计算单。',
  );
}

/** 导出中（按钮 loading；避免连点导出多份） */
const exporting = ref(false);

/**
 * 导出成本计算单 xlsx（设计 §5.2 #32）。
 *
 * 后端按**批次 id** 一次拉整张计算单（对象 × 料工费 × 分摊 × 约当 × 单位成本 × 完工/在制），
 * 数字与本页步③ 同源（后端 `detail_of`），所以导出件与屏幕上的表逐格一致。
 * 失败报文由 api 层的 `blobError` 读回原文，这里不再改写措辞。
 */
async function handleExportSheet(): Promise<void> {
  if (!batch.value) {
    MessagePlugin.warning('先建批次再导出');
    return;
  }
  exporting.value = true;
  try {
    const blob = await exportCostSheet(batch.value.id);
    saveBlob(blob, costSheetFileName(batch.value.batch_no));
    MessagePlugin.success(`已导出 ${costSheetFileName(batch.value.batch_no)}`);
  } catch (error) {
    MessagePlugin.error(errorText(error, '导出成本计算单失败'));
  } finally {
    exporting.value = false;
  }
}

/** 提交审批（`DRAFT → SUBMITTED`，同事务发起两级审批实例） */
async function handleSubmit(): Promise<void> {
  if (!batch.value) {
    return;
  }
  acting.value = true;
  wizardError.value = '';
  try {
    const detail = await submitCostPeriod(batch.value.id);
    batch.value = detail;
    await loadProgress(detail.approval_instance_id);
    step.value = 3;
    submitPreviewVisible.value = false;
    MessagePlugin.success(`批次 ${detail.batch_no} 已提交审批（财务经理 → 财务总监）`);
    await loadRows();
  } catch (error) {
    // 常见：没有已发布流程 409 / 没有可用审批人 422 —— 报文已点名怎么修
    wizardError.value = errorText(error, '提交审批失败');
    submitPreviewVisible.value = false;
  } finally {
    acting.value = false;
  }
}

/** 撤回（终止在跑实例 + 回草稿） */
async function handleWithdraw(): Promise<void> {
  if (!batch.value) {
    return;
  }
  acting.value = true;
  wizardError.value = '';
  try {
    const detail = await withdrawCostPeriod(batch.value.id);
    batch.value = detail;
    approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    step.value = stepOfStatus(detail.status);
    MessagePlugin.success('已撤回：批次回到草稿，可改数后重新提交');
    await loadRows();
  } catch (error) {
    wizardError.value = errorText(error, '撤回审批失败');
  } finally {
    acting.value = false;
  }
}

/** 结算（`APPROVED → SETTLED`：入库 + 三张凭证 + 6 条断言） */
const settleDialogVisible = ref(false);
const settleError = ref('');
const settleTargetId = ref('');

function openSettleDialog(id: string): void {
  settleTargetId.value = id;
  settleError.value = '';
  settleDialogVisible.value = true;
}

async function handleSettle(): Promise<void> {
  if (!settleTargetId.value) {
    return;
  }
  acting.value = true;
  settleError.value = '';
  try {
    const detail = await settleCostPeriod(settleTargetId.value);
    batch.value = detail;
    settleDialogVisible.value = false;
    step.value = LAST_STEP;
    MessagePlugin.success(`批次 ${detail.batch_no} 已结算：入库 + 凭证已生成`);
    await loadRows();
  } catch (error) {
    // 断言不平 409 / 库存推不动 409：整笔回滚，批次仍 APPROVED，修数据后可重试
    settleError.value = errorText(error, '结算失败');
  } finally {
    acting.value = false;
  }
}

/** 红冲（`SETTLED → REVERSED`：红字凭证 + 库存回冲 + 入库单作废） */
const reverseDialogVisible = ref(false);
const reverseError = ref('');
const reverseTargetId = ref('');

async function openReverse(id: string): Promise<void> {
  reverseTargetId.value = id;
  reverseError.value = '';
  reverseTarget.value = null;
  try {
    reverseTarget.value = await fetchCostPeriodDetail(id);
  } catch {
    // 拉不到详情不阻断：确认框用列表口径的兜底文案
    reverseTarget.value = null;
  }
  reverseDialogVisible.value = true;
}

async function handleReverse(reason: string): Promise<void> {
  if (!reverseTargetId.value) {
    return;
  }
  acting.value = true;
  reverseError.value = '';
  try {
    const detail = await reverseCostPeriod(reverseTargetId.value, { reason });
    batch.value = detail;
    reverseDialogVisible.value = false;
    step.value = LAST_STEP;
    MessagePlugin.success(`批次 ${detail.batch_no} 已红冲：红字凭证 + 库存回冲 + 入库单作废`);
    await loadRows();
  } catch (error) {
    // 库存退不回 409 + 66078：报文里给了「走销售退货/盘亏」的修法
    reverseError.value = errorText(error, '红冲失败');
  } finally {
    acting.value = false;
  }
}

/** 作废（`DRAFT → CANCELLED`，原因 ≥10 字） */
const cancelDialogVisible = ref(false);
const cancelReason = ref('');
const cancelReasonHint = computed<string | null>(() => reasonHint(cancelReason.value));
const cancelReasonOk = computed<boolean>(() => cancelReasonHint.value === null);

function openCancelDialog(): void {
  cancelReason.value = '';
  cancelDialogVisible.value = true;
}

async function handleCancel(): Promise<void> {
  if (!batch.value || !cancelReasonOk.value) {
    return;
  }
  acting.value = true;
  wizardError.value = '';
  try {
    const detail = await cancelCostPeriod(batch.value.id, { reason: cancelReason.value.trim() });
    batch.value = detail;
    cancelDialogVisible.value = false;
    step.value = 0;
    MessagePlugin.success(`批次 ${detail.batch_no} 已作废（同期间可重开）`);
    await loadRows();
  } catch (error) {
    wizardError.value = errorText(error, '作废批次失败');
    cancelDialogVisible.value = false;
  } finally {
    acting.value = false;
  }
}

/** 提交审批的流程预览 */
const submitPreviewVisible = ref(false);

// ==================== 列 ====================

/**
 * 列表列（1440 视口，内容区约 1128px）：批次号 150 / 期间 80 / 状态 96 /
 * 对象 60 / 投入 104 / 完工 104 / 在制 104 / 结算时间 88 / 制单 72 / 操作 120。
 */
const columns: PrimaryTableCol[] = [
  { colKey: 'batch_no', title: '批次号', width: 150, fixed: 'left' },
  { colKey: 'period_code', title: '期间', width: 80 },
  { colKey: 'status', title: '状态', width: 96 },
  { colKey: 'object_count', title: '对象', width: 60, align: 'right' },
  { colKey: 'total_cost', title: '投入', width: 104, align: 'right' },
  { colKey: 'total_finished', title: '完工转出', width: 104, align: 'right' },
  { colKey: 'total_wip', title: '期末在制', width: 104, align: 'right' },
  { colKey: 'settled_at', title: '结算时间', width: 88 },
  { colKey: 'created_by_name', title: '制单', width: 72, ellipsis: true },
  { colKey: 'ops', title: '操作', width: 120, fixed: 'right' },
];

// ==================== 生命周期 ====================

onMounted(async () => {
  await loadRows();
  void loadPeriods();
  // 深链：凭证来源「成本核算批次」带 `?batch_no=CS-yyyyMM-NNNN` 进来（VTRACE 的凭证明细
  // 点批次号），直接打开那一批的五步向导。找不到就留在列表（不弹错：来源可能是别的组织 / 已删）。
  const batchNo = String(route.query.batch_no ?? '').trim();
  if (batchNo) {
    await openByBatchNo(batchNo);
  }
});

/** 按批次号打开向导（深链入口；找不到就静默留在列表） */
async function openByBatchNo(batchNo: string): Promise<void> {
  const hit = rows.value.find((item) => item.batch_no === batchNo);
  if (hit) {
    await openWizard(hit);
    return;
  }
  try {
    const data = await fetchCostPeriodPage({ keyword: batchNo, page: 1, page_size: PAGE_SIZE });
    const found = data.list.find((item) => item.batch_no === batchNo);
    if (found) {
      await openWizard(found);
    }
  } catch {
    // 深链失败不影响列表可用
  }
}
</script>

<style scoped>
.cost-period-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__period {
  width: 200px;
}

.tool-row__status {
  width: 140px;
}

.tool-row__keyword {
  width: 200px;
}

.table-alert {
  margin-bottom: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 12px;
}

.num,
.money {
  font-variant-numeric: tabular-nums;
}

.row-ops {
  display: flex;
  gap: 10px;
}

/* ---------- 向导 ---------- */
.wizard {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding-bottom: 8px;
}

.wizard__steps {
  padding: 4px 0 8px;
}

.wizard__alert {
  margin: 0;
}

.pane {
  display: flex;
  flex-direction: column;
  gap: 14px;
  min-height: 320px;
}

.pane__lead {
  margin: 0;
  font-size: 13px;
  line-height: 1.6;
  color: var(--td-text-color-secondary);
}

.pane__field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.pane__label {
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.pane__req {
  color: var(--td-error-color);
}

.pane__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.pane__period {
  width: 280px;
}

.pane__foot {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.pane__alert {
  margin: 0;
}

.pane__row-ops {
  display: flex;
  align-items: center;
  gap: 10px;
}

.pane__row-ops--top {
  margin-top: 4px;
}

.pane__spacer {
  flex: 1;
}

.pane__help {
  color: var(--td-text-color-placeholder);
  cursor: help;
}

.pane__meta {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.pane__section-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.settle-facts {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 8px 16px;
  padding: 12px;
  background: var(--td-bg-color-secondarycontainer);
  border-radius: 4px;
}

.settle-facts__item {
  display: flex;
  justify-content: space-between;
  font-size: 13px;
}

.settle-facts__label {
  color: var(--td-text-color-secondary);
}

.settle-vouchers {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  font-size: 13px;
}

.settle-vouchers__title {
  font-weight: 600;
}

.wizard__foot {
  display: flex;
  align-items: center;
  gap: 10px;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);
}

.wizard__spacer {
  flex: 1;
}

.wizard__foot-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ---------- 弹窗 ---------- */
.preview {
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 13px;
  line-height: 1.6;
}

.preview__lead,
.preview__foot {
  margin: 0;
  color: var(--td-text-color-secondary);
}

.preview__list {
  margin: 0;
  padding-left: 18px;
  color: var(--td-text-color-primary);
}

.preview__error {
  margin: 0;
  font-size: 12px;
  color: var(--td-error-color);
}

.pre {
  white-space: pre-wrap;
}
</style>
