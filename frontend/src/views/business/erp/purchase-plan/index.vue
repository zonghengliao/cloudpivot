<template>
  <div class="pp-page">
    <!-- 两个取数码都没有：页面能进（有页面显形码）但两个 Tab 全 403 —— 说清缺什么 -->
    <t-alert
      v-if="!canList && !canExec"
      theme="warning"
      class="pp-alert"
      data-testid="pp-no-perm"
      :message="`当前账号没有采购计划的取数权限（${PERM.list} / ${PERM.execution}），请让管理员在角色授权里勾上对应按钮行`"
    />

    <t-tabs v-if="canList || canExec" v-model="activeTab" data-testid="pp-tabs">
      <!-- ==================== Tab 1：采购计划（录入） ==================== -->
      <t-tab-panel v-if="canList" value="plans" label="采购计划">
        <ListPage
          ref="listRef"
          title="采购计划"
          testid="pp"
          table-testid="pp-table"
          meta-unit="张"
          btn-prefix="btn-pp"
          :columns="columns"
          :filters="filters"
          :filter-state="filter"
          :fetcher="fetcher"
          :query-builder="queryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
          @loaded="onLoaded"
        >
          <template #toolbar>
            <t-tooltip
              :content="
                canCreate
                  ? '按年度 / 月度编物料需求：粒度是「物料 × 仓库」，审批通过后可一键转采购申请'
                  : `当前账号没有 ${PERM.create} 权限`
              "
              placement="top-right"
            >
              <span class="gate-wrap">
                <t-button
                  theme="primary"
                  :disabled="!canCreate"
                  data-testid="btn-pp-new"
                  @click="openCreate"
                >
                  <template #icon><t-icon name="add" /></template>
                  新建计划
                </t-button>
              </span>
            </t-tooltip>
          </template>

          <!-- 年份 / 部门自绘：「全部」在 filter 里是 `null` / `''`，t-select 的 v-model 不收 null -->
          <template #filter-plan_year>
            <t-select
              v-model="yearModel"
              :style="{ width: '140px' }"
              :options="yearOptions"
              placeholder="全部年度"
              clearable
              data-testid="pp-filter-year"
              @change="handleSearch"
            />
          </template>
          <template #filter-dept_id>
            <t-select
              v-model="deptModel"
              :style="{ width: '180px' }"
              :options="deptOptions"
              placeholder="全部部门"
              filterable
              clearable
              data-testid="pp-filter-dept"
              @change="handleSearch"
            />
          </template>

          <template #plan_no="{ row }">
            <span class="mono">{{ row.plan_no }}</span>
          </template>
          <template #plan_name="{ row }">
            <div class="cell-stack">
              <span>{{ row.plan_name }}</span>
              <span class="cell-sub">
                {{ planTypeLabel(row.plan_type) }} · {{ periodText(row) }}
                <template v-if="row.parent_plan_no"> · 父 {{ row.parent_plan_no }}</template>
              </span>
            </div>
          </template>
          <template #dept_name="{ row }">
            <div class="cell-stack">
              <span>{{ row.dept_name || '公司级' }}</span>
              <span class="cell-sub">{{ row.planner_name || '—' }}</span>
            </div>
          </template>
          <template #total_planned_qty="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono">{{ decimalText(row.total_planned_qty) }}</span>
              <span class="cell-sub">{{ row.item_count }} 行</span>
            </div>
          </template>
          <template #total_planned_amount="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono">{{ decimalText(row.total_planned_amount) }}</span>
              <span class="cell-sub">已执行 {{ decimalText(row.total_executed_qty) }}</span>
            </div>
          </template>
          <template #status="{ row }">
            <t-tag :theme="statusTheme(row.status)" variant="light" size="small">
              {{ row.status_label }}
            </t-tag>
          </template>

          <template #op="{ row }">
            <t-space size="small">
              <t-link theme="primary" data-testid="btn-pp-detail" @click="openDetail(row.id)">详情</t-link>
              <t-link
                v-if="row.editable && canUpdate"
                theme="primary"
                data-testid="btn-pp-edit"
                @click="openEdit(row.id)"
              >编辑</t-link>
              <t-link
                v-if="row.editable && canSubmit"
                theme="warning"
                data-testid="btn-pp-submit"
                @click="handleSubmit(row)"
              >提交审批</t-link>
              <t-link
                v-if="canConvert(row) && canToRequisition"
                theme="success"
                data-testid="btn-pp-to-req"
                @click="openConvert(row.id)"
              >转采购申请</t-link>
              <t-link
                v-if="row.editable && canDelete"
                theme="danger"
                data-testid="btn-pp-delete"
                @click="handleDelete(row)"
              >删除</t-link>
            </t-space>
          </template>

          <template #empty="{ filtered }">
            <div class="empty-block" data-testid="pp-empty">{{ emptyListText(filtered) }}</div>
          </template>
        </ListPage>
      </t-tab-panel>

      <!-- ==================== Tab 2：执行率（只读） ==================== -->
      <t-tab-panel v-if="canExec" value="execution" label="执行率">
        <ListPage
          ref="execListRef"
          title="采购计划执行率"
          testid="pp-exec"
          table-testid="pp-exec-table"
          meta-unit="张"
          btn-prefix="btn-pp-exec"
          row-key="plan_id"
          :columns="execColumns"
          :filters="execFilters"
          :filter-state="execFilter"
          :fetcher="execFetcher"
          :query-builder="execQueryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #toolbar>
            <t-tooltip
              content="执行率 = 已下单 / 计划量：已下单按申请单头 po_id 指向的「存活且未作废」采购单算（草稿采购单也算，作废 / 软删不算）"
              placement="top-right"
            >
              <span class="hint-inline" data-testid="pp-exec-hint">口径：已下单 ÷ 计划量</span>
            </t-tooltip>
          </template>

          <template #filter-plan_year>
            <t-select
              v-model="execYearModel"
              :style="{ width: '130px' }"
              :options="yearOptions"
              placeholder="全部年度"
              clearable
              data-testid="pp-exec-filter-year"
              @change="handleExecSearch"
            />
          </template>
          <template #filter-plan_month>
            <t-select
              v-model="execMonthModel"
              :style="{ width: '120px' }"
              :options="MONTH_OPTIONS"
              placeholder="全部月份"
              clearable
              data-testid="pp-exec-filter-month"
              @change="handleExecSearch"
            />
          </template>
          <template #filter-min_rate>
            <t-select
              v-model="execMinRateModel"
              :style="{ width: '230px' }"
              :options="MIN_RATE_OPTIONS"
              placeholder="执行率不限"
              clearable
              data-testid="pp-exec-filter-min-rate"
              @change="handleExecSearch"
            />
          </template>
          <template #filter-dept_id>
            <t-select
              v-model="deptModel"
              :style="{ width: '180px' }"
              :options="deptOptions"
              placeholder="全部部门"
              filterable
              clearable
              data-testid="pp-exec-filter-dept"
              @change="handleExecSearch"
            />
          </template>

          <template #plan_no="{ row }">
            <span class="mono">{{ row.plan_no }}</span>
          </template>
          <template #plan_name="{ row }">
            <div class="cell-stack">
              <span>{{ row.plan_name }}</span>
              <span class="cell-sub">
                {{ planTypeLabel(row.plan_type) }} · {{ periodText(row) }}
                <template v-if="row.dept_name"> · {{ row.dept_name }}</template>
              </span>
            </div>
          </template>
          <template #planned_qty="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono">{{ decimalText(row.planned_qty) }}</span>
              <span class="cell-sub">已下单 {{ decimalText(row.ordered_qty) }}</span>
            </div>
          </template>
          <template #exec_rate="{ row }">
            <div class="rate-cell" :data-testid="`pp-exec-rate-${row.plan_id}`">
              <t-progress
                :percentage="progressPercent(row.exec_rate_pct)"
                theme="line"
                :status="progressStatus(row.exec_state)"
                :label="false"
              />
              <span class="rate-cell__text mono">{{ pctText(row.exec_rate_pct) }}</span>
            </div>
          </template>
          <template #exec_state="{ row }">
            <div class="cell-stack">
              <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
                {{ row.exec_state_label }}
              </t-tag>
              <span class="cell-sub">{{ lineCountsText(row) }}</span>
            </div>
          </template>
          <template #op="{ row }">
            <t-link theme="primary" data-testid="btn-pp-exec-drill" @click="openExecution(row.plan_id)">
              下钻
            </t-link>
          </template>

          <template #empty="{ filtered }">
            <div class="empty-block" data-testid="pp-exec-empty">
              {{
                filtered
                  ? '没有符合筛选条件的计划：清一下筛选条件再试'
                  : '还没有可统计的计划：先「录入」一张计划并提交审批，执行率会随申请单 / 采购单自动累计'
              }}
            </div>
          </template>
        </ListPage>
      </t-tab-panel>
    </t-tabs>

    <!-- ==================== 新建 / 编辑计划 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="formHeader"
      width="1180px"
      :confirm-btn="{ content: '保存草稿', loading: saving }"
      :close-on-overlay-click="false"
      data-testid="pp-form-dialog"
      @confirm="handleSave"
    >
      <t-form label-width="96px" class="pp-form">
        <t-row :gutter="[16, 12]">
          <t-col :span="8">
            <t-form-item label="计划名称" required-mark>
              <t-input
                v-model="form.plan_name"
                :maxlength="MAX_NAME"
                placeholder="例：2026 年度原材料采购计划"
                data-testid="pp-form-name"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="计划类型" required-mark>
              <t-radio-group
                :value="form.plan_type"
                data-testid="pp-form-type"
                @change="handleTypeChange"
              >
                <t-radio value="ANNUAL">年度计划</t-radio>
                <t-radio value="MONTHLY">月度计划</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="计划年度" required-mark>
              <t-input-number
                v-model="form.plan_year"
                :min="YEAR_MIN"
                :max="YEAR_MAX"
                :decimal-places="0"
                theme="normal"
                data-testid="pp-form-year"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="计划月份">
              <t-select
                v-if="form.plan_type === 'MONTHLY'"
                :value="form.plan_month ?? undefined"
                :options="MONTH_OPTIONS"
                placeholder="选月份"
                data-testid="pp-form-month"
                @change="(value: SelectValue) => (form.plan_month = typeof value === 'number' ? value : null)"
              />
              <span v-else class="cell-sub" data-testid="pp-form-month-na">年度计划不带月</span>
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="父年度计划">
              <t-select
                v-if="form.plan_type === 'MONTHLY'"
                v-model="form.parent_plan_id"
                :options="parentOptions"
                placeholder="可不挂"
                filterable
                clearable
                data-testid="pp-form-parent"
              />
              <span v-else class="cell-sub">只有月度计划能挂</span>
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="编制部门">
              <t-select
                v-model="form.dept_id"
                :options="deptOptions"
                placeholder="不选 = 公司级"
                filterable
                clearable
                data-testid="pp-form-dept"
              />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="备注">
              <t-textarea
                v-model="form.remark"
                :maxlength="MAX_REMARK"
                placeholder="编制依据 / 说明（审批人会看）"
                data-testid="pp-form-remark"
              />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <t-alert
        v-if="form.lines.length > MAX_LINES"
        theme="warning"
        class="pp-alert"
        :message="`明细最多 ${MAX_LINES} 行（当前 ${form.lines.length} 行），请拆成多张计划单`"
        data-testid="pp-line-limit"
      />
      <div class="lines-head">
        <span class="lines-title">计划明细（粒度 = 物料 × 仓库）</span>
        <t-button size="small" variant="outline" data-testid="btn-pp-add-line" @click="addLine">
          <template #icon><t-icon name="add" /></template>
          加一行
        </t-button>
      </div>

      <t-table row-key="key" :data="form.lines" :columns="lineColumns" size="small" data-testid="pp-lines-table">
        <template #material_id="{ row }">
          <t-select
            v-model="row.material_id"
            :options="materialOptions"
            placeholder="选择物料"
            filterable
            size="small"
            data-testid="pp-line-material"
          />
        </template>
        <template #warehouse_id="{ row }">
          <t-select
            v-model="row.warehouse_id"
            :options="warehouseOptions"
            placeholder="选择仓库"
            filterable
            size="small"
            data-testid="pp-line-warehouse"
          />
        </template>
        <template #planned_qty="{ row }">
          <t-input-number
            v-model="row.planned_qty"
            :min="0.0001"
            :decimal-places="4"
            size="small"
            data-testid="pp-line-qty"
          />
        </template>
        <template #planned_price="{ row }">
          <t-input-number
            v-model="row.planned_price"
            :min="0"
            :decimal-places="6"
            size="small"
            placeholder="不含税"
            data-testid="pp-line-price"
          />
        </template>
        <template #planned_amount="{ row }">
          <span class="mono">{{ lineAmountText(row.planned_qty, row.planned_price) }}</span>
        </template>
        <template #remark="{ row }">
          <t-input v-model="row.remark" size="small" placeholder="可空" data-testid="pp-line-remark" />
        </template>
        <template #ops="{ row }">
          <t-link theme="danger" data-testid="btn-pp-remove-line" @click="removeLine(row.key)">删除</t-link>
        </template>
      </t-table>

      <div class="totals-row" data-testid="pp-form-totals">
        合计 {{ formTotals(form).lines }} 行 · 计划量 {{ formTotals(form).qty }} · 预计金额（不含税）
        {{ formTotals(form).amount }}
        <span class="cell-sub">（金额由服务端按明细重算，这里只是预览）</span>
      </div>
    </t-dialog>

    <!-- ==================== 详情 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="detailHeader"
      size="900px"
      :footer="false"
      data-testid="pp-detail-drawer"
    >
      <template v-if="detail">
        <t-descriptions :column="2" size="small" class="detail-desc">
          <t-descriptions-item label="计划单号">{{ detail.plan.plan_no }}</t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag :theme="statusTheme(detail.plan.status)" variant="light" size="small">
              {{ detail.plan.status_label }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="计划名称">{{ detail.plan.plan_name }}</t-descriptions-item>
          <t-descriptions-item label="类型 / 期间">
            {{ planTypeLabel(detail.plan.plan_type) }} · {{ periodText(detail.plan) }}
          </t-descriptions-item>
          <t-descriptions-item label="父年度计划">{{ detail.plan.parent_plan_no || '—' }}</t-descriptions-item>
          <t-descriptions-item label="编制部门">{{ detail.plan.dept_name || '公司级' }}</t-descriptions-item>
          <t-descriptions-item label="编制人">{{ detail.plan.planner_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="计划量合计">{{ decimalText(detail.plan.total_planned_qty) }}</t-descriptions-item>
          <t-descriptions-item label="计划金额（不含税）">
            {{ decimalText(detail.plan.total_planned_amount) }}
          </t-descriptions-item>
          <t-descriptions-item label="已执行量（已转申请）">
            {{ decimalText(detail.plan.total_executed_qty) }}
          </t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ detail.plan.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <div class="lines-head">
          <span class="lines-title">明细（{{ detail.items.length }} 行）</span>
          <t-space size="small">
            <t-button
              v-if="canToRequisition"
              size="small"
              variant="outline"
              data-testid="btn-pp-detail-exec"
              @click="openExecution(detail.plan.id)"
            >执行率</t-button>
            <t-button
              v-if="canConvert(detail.plan) && canToRequisition"
              size="small"
              theme="primary"
              data-testid="btn-pp-detail-to-req"
              @click="openConvert(detail.plan.id)"
            >转采购申请</t-button>
          </t-space>
        </div>
        <t-table row-key="id" :data="detail.items" :columns="detailItemColumns" size="small" data-testid="pp-detail-items">
          <template #planned_qty="{ row }">
            <span class="mono">{{ decimalText(row.planned_qty) }}</span>
          </template>
          <template #executed_qty="{ row }">
            <span class="mono">{{ decimalText(row.executed_qty) }}</span>
          </template>
          <template #planned_price="{ row }">
            <span class="mono">{{ decimalText(row.planned_price) }}</span>
          </template>
          <template #planned_amount="{ row }">
            <span class="mono">{{ decimalText(row.planned_amount) }}</span>
          </template>
        </t-table>
      </template>
    </t-drawer>

    <!-- ==================== 转采购申请 ==================== -->
    <t-dialog
      v-model:visible="convertVisible"
      :header="convertHeader"
      width="1080px"
      :confirm-btn="{ content: '生成采购申请', loading: converting }"
      :close-on-overlay-click="false"
      data-testid="pp-to-req-dialog"
      @confirm="handleConvert"
    >
      <t-alert
        theme="info"
        class="pp-alert"
        message="按仓库**一仓一单**生成采购申请：N 个仓库 → N 张申请单。缺省转「剩余量 = 计划量 − 已执行量」，可以逐行改小分批转；超过剩余量后端会 400（临时加量请直接新建采购申请单）。"
      />
      <t-form label-width="96px">
        <t-row :gutter="[16, 12]">
          <t-col :span="6">
            <t-form-item label="申请部门" required-mark>
              <t-select
                v-model="convertForm.dept_id"
                :options="deptOptions"
                placeholder="公司级计划必须选"
                filterable
                clearable
                data-testid="pp-to-req-dept"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="税率 %">
              <t-input-number
                v-model="convertForm.tax_rate"
                :min="0"
                :max="100"
                :decimal-places="4"
                theme="normal"
                data-testid="pp-to-req-tax"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="申请日期">
              <t-date-picker
                v-model="convertForm.req_date"
                value-type="YYYY-MM-DD"
                placeholder="缺省 = 今天"
                clearable
                data-testid="pp-to-req-date"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="需求日期">
              <t-date-picker
                v-model="convertForm.demand_date"
                value-type="YYYY-MM-DD"
                placeholder="可空"
                clearable
                data-testid="pp-to-req-demand"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="备注">
              <t-input
                v-model="convertForm.remark"
                :maxlength="MAX_REMARK"
                placeholder="会追加在「由采购计划 … 转入」之后"
                data-testid="pp-to-req-remark"
              />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <div class="lines-head">
        <span class="lines-title">转出明细（勾选要转的行）</span>
        <t-space size="small">
          <t-link theme="primary" data-testid="btn-pp-to-req-all" @click="selectAllConvert(true)">全选</t-link>
          <t-link theme="default" data-testid="btn-pp-to-req-none" @click="selectAllConvert(false)">全不选</t-link>
        </t-space>
      </div>
      <t-table row-key="plan_item_id" :data="convertForm.lines" :columns="convertColumns" size="small" data-testid="pp-to-req-lines">
        <template #selected="{ row }">
          <t-checkbox :checked="row.selected" data-testid="pp-to-req-pick" @change="(v: boolean) => (row.selected = v)" />
        </template>
        <template #planned_qty="{ row }">
          <span class="mono">{{ decimalText(row.planned_qty) }}</span>
        </template>
        <template #executed_qty="{ row }">
          <span class="mono">{{ decimalText(row.executed_qty) }}</span>
        </template>
        <template #remaining="{ row }">
          <span class="mono">{{ row.remaining }}</span>
        </template>
        <template #qty="{ row }">
          <t-input-number
            v-model="row.qty"
            :min="0"
            :decimal-places="4"
            size="small"
            :disabled="!row.selected"
            data-testid="pp-to-req-qty"
          />
        </template>
        <template #est_price="{ row }">
          <t-input-number
            v-model="row.est_price"
            :min="0"
            :decimal-places="6"
            size="small"
            :disabled="!row.selected"
            placeholder="计划单价为 0 时必填"
            data-testid="pp-to-req-price"
          />
        </template>
      </t-table>
      <div class="totals-row" data-testid="pp-to-req-totals">
        已勾选 {{ convertTotals(convertForm).lines }} 行 · 转出数量 {{ convertTotals(convertForm).qty }} ·
        预计金额（不含税） {{ convertTotals(convertForm).amount }}
      </div>
    </t-dialog>

    <!-- ==================== 执行率下钻 ==================== -->
    <t-drawer
      v-model:visible="execVisible"
      :header="execHeader"
      size="980px"
      :footer="false"
      data-testid="pp-exec-drawer"
    >
      <template v-if="execution">
        <div class="exec-kpi" data-testid="pp-exec-kpi">
          <div class="exec-kpi__item">
            <span class="exec-kpi__label">计划量</span>
            <span class="exec-kpi__value mono">{{ decimalText(execution.plan.planned_qty) }}</span>
          </div>
          <div class="exec-kpi__item">
            <span class="exec-kpi__label">已申请（按单据）</span>
            <span class="exec-kpi__value mono">{{ decimalText(execution.plan.req_qty) }}</span>
          </div>
          <div class="exec-kpi__item">
            <span class="exec-kpi__label">已下单</span>
            <span class="exec-kpi__value mono">{{ decimalText(execution.plan.ordered_qty) }}</span>
          </div>
          <div class="exec-kpi__item">
            <span class="exec-kpi__label">剩余可下单</span>
            <span class="exec-kpi__value mono">{{ remainingText(execution.plan.remaining_qty) }}</span>
          </div>
          <div class="exec-kpi__item">
            <span class="exec-kpi__label">执行率</span>
            <span class="exec-kpi__value">
              <t-tag :theme="execStateTheme(execution.plan.exec_state)" variant="light" size="small">
                {{ pctText(execution.plan.exec_rate_pct) }}
              </t-tag>
              <span class="cell-sub">{{ execution.plan.exec_state_label }}</span>
            </span>
          </div>
        </div>
        <div class="cell-sub exec-note" data-testid="pp-exec-note">
          {{ lineCountsText(execution.plan) }} · 执行率 = 已下单 ÷ 计划量；状态按未舍入量判，
          所以可能出现「100.00% 仍为部分下单」
        </div>

        <div class="lines-head"><span class="lines-title">逐行执行率（展开看来源单据）</span></div>
        <t-table
          row-key="plan_item_id"
          :data="execution.items"
          :columns="execItemColumns"
          size="small"
          :expand-icon="true"
          data-testid="pp-exec-items"
        >
          <template #planned_qty="{ row }">
            <span class="mono">{{ decimalText(row.planned_qty) }}</span>
          </template>
          <template #req_qty="{ row }">
            <span class="mono">{{ decimalText(row.req_qty) }}</span>
          </template>
          <template #ordered_qty="{ row }">
            <span class="mono">{{ decimalText(row.ordered_qty) }}</span>
          </template>
          <template #remaining_qty="{ row }">
            <span class="mono">{{ remainingText(row.remaining_qty) }}</span>
          </template>
          <template #exec_rate="{ row }">
            <div class="rate-cell">
              <t-progress
                :percentage="progressPercent(row.exec_rate_pct)"
                theme="line"
                :status="progressStatus(row.exec_state)"
                :label="false"
              />
              <span class="rate-cell__text mono">{{ pctText(row.exec_rate_pct) }}</span>
            </div>
          </template>
          <template #exec_state="{ row }">
            <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
              {{ row.exec_state_label }}
            </t-tag>
          </template>
          <template #sources="{ row }">
            <span class="cell-sub">
              申请 {{ row.req_doc_count }} 张 / 采购 {{ row.po_doc_count }} 张
            </span>
          </template>
          <template #expandedRow="{ row }">
            <div class="src-block" :data-testid="`pp-exec-src-${row.plan_item_id}`">
              <div class="src-block__title">
                来源单据（申请明细 → 采购单）：勾掉的不计入「已下单」，原因见右列
              </div>
              <t-table
                v-if="row.sources.length"
                row-key="req_item_id"
                size="small"
                :data="row.sources"
                :columns="sourceColumns"
              >
                <template #qty="{ row: source }">
                  <span class="mono">{{ decimalText(source.qty) }}</span>
                </template>
                <template #po_no="{ row: source }">
                  <span class="mono">{{ sourcePoText(source) }}</span>
                </template>
                <template #counted="{ row: source }">
                  <t-tag :theme="source.counted ? 'success' : 'default'" variant="light" size="small">
                    {{ source.reason }}
                  </t-tag>
                </template>
              </t-table>
              <div v-else class="cell-sub">这一行还没有转过采购申请</div>
            </div>
          </template>
        </t-table>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 采购计划页面（M4-06）：**录入** + **执行率**。
 *
 * # 两个 Tab 各管一件事（各自一个 ListPage，服务端分页）
 *
 * | Tab | 取数 | 权限码 | 能做什么 |
 * |-----|------|--------|----------|
 * | 采购计划 | `GET purchase-plan/page` + `{id}` | `…:list`（+ create/update/delete/submit/to-requisition 控制按钮） | 建 / 改 / 删 / 提交审批 / 转采购申请 |
 * | 执行率 | `GET purchase-plan/execution/page` + `{id}/execution` | `…:execution` | 只看（下钻到申请单 / 采购单） |
 *
 * # 页面不做的事（都交给服务端，别在这里重算）
 *
 * 1. **金额**：单头 `total_planned_amount` 是派生列，服务端按明细现算；行金额预览只是输入时给人看的。
 * 2. **执行率状态**：读 `exec_state` / `over_plan`，**绝不用 pct 反推**（后端允许 100.00% 仍为 PARTIAL）。
 * 3. **状态机**：用后端给的 `editable`，不自己写「只有 DRAFT 能编辑」这套判断。
 *
 * # 权限（V0447~V0449 的 auth_code，逐字一致）
 *
 * 页面显形码 `…:page` 只在路由 `meta.permission`（AGENTS 约定 14：后端零引用），
 * 这里只用 7 个接口码判按钮与 Tab。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { SelectValue } from 'tdesign-vue-next';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import {
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import {
  convertPlanToRequisition,
  createPlan,
  deletePlan,
  fetchExecutionPage,
  fetchPlanDetail,
  fetchPlanExecution,
  fetchPlanPage,
  submitPlan,
  updatePlan,
  type ExecutionFilter,
  type PlanDetail,
  type PlanExecItem,
  type PlanExecRow,
  type PlanExecution,
  type PlanFilter,
  type PlanRow,
  type PlanStatus,
  type PlanType,
} from '@/api/business/erp/purchase-plan';
import {
  MAX_LINES,
  MAX_NAME,
  MAX_REMARK,
  MIN_RATE_OPTIONS,
  MONTH_OPTIONS,
  PAGE_SIZE,
  PLAN_TYPE_OPTIONS,
  STATUS_OPTIONS,
  YEAR_MAX,
  YEAR_MIN,
  buildConvertPayload,
  buildExecQuery,
  buildPlanPayload,
  buildQuery,
  convertTotals,
  convertedSummaryText,
  currentYear,
  decimalText,
  emptyConvertForm,
  emptyExecFilter,
  emptyFilter,
  emptyListText,
  emptyPlanForm,
  execStateTheme,
  formFromDetail,
  formTotals,
  lineAmountText,
  lineCountsText,
  monthForType,
  newPlanLine,
  parentCandidates,
  pctText,
  periodText,
  planTypeLabel,
  progressPercent,
  progressStatus,
  remainingText,
  statusTheme,
  validateConvert,
  validatePlanForm,
  type ConvertForm,
  type ExecFilterState,
  type FilterState,
  type PlanForm,
} from './purchase-plan-logic';

/** 本页权限码（与 V0447 / V0448 / V0449 的 auth_code 逐字一致） */
const PERM = {
  /** 页面显形（只在路由 meta 用，后端零引用） */
  page: 'business:erp:purchase-plan:page',
  /** 列表 / 详情 */
  list: 'business:erp:purchase-plan:list',
  /** 新建 */
  create: 'business:erp:purchase-plan:create',
  /** 修改 */
  update: 'business:erp:purchase-plan:update',
  /** 删除 */
  delete: 'business:erp:purchase-plan:delete',
  /** 提交审批 */
  submit: 'business:erp:purchase-plan:submit',
  /** 转采购申请 */
  toRequisition: 'business:erp:purchase-plan:to-requisition',
  /** 执行率（两个只读接口共用） */
  execution: 'business:erp:purchase-plan:execution',
} as const;

const userStore = useUserStore();
const canList = computed(() => userStore.hasPermission(PERM.list));
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canUpdate = computed(() => userStore.hasPermission(PERM.update));
const canDelete = computed(() => userStore.hasPermission(PERM.delete));
const canSubmit = computed(() => userStore.hasPermission(PERM.submit));
const canToRequisition = computed(() => userStore.hasPermission(PERM.toRequisition));
const canExec = computed(() => userStore.hasPermission(PERM.execution));

/** 默认落在第一个有权限的 Tab（只有执行率码的人不该看到空白页） */
const activeTab = ref(canList.value ? 'plans' : 'execution');

/** 错误文案归一（后端 409 / 400 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error) {
    return error.message || fallback;
  }
  return fallback;
}

/**
 * 能不能转采购申请：只有**已批准**的计划。
 *
 * 与后端 `convert_plan` 的状态闸同口径（DRAFT 没批、SUBMITTED 还在批都不许转）。
 */
function canConvert(row: { status: string }): boolean {
  return row.status === 'APPROVED';
}

// ==================== 主数据下拉 ====================

const deptOptions = ref<{ label: string; value: string }[]>([]);
const warehouseOptions = ref<{ label: string; value: string }[]>([]);
const materialOptions = ref<{ label: string; value: string }[]>([]);

function flattenDepts(nodes: DeptNode[], out: { label: string; value: string }[] = []) {
  nodes.forEach((node) => {
    out.push({ label: node.dept_name, value: node.id });
    if (node.children?.length) {
      flattenDepts(node.children, out);
    }
  });
  return out;
}

async function loadOptions(): Promise<void> {
  try {
    const [tree, warehouses, materials] = await Promise.all([
      getDeptTree({}),
      fetchWarehousePage({ page: 1, page_size: 200, status: 1 }),
      fetchMaterialPage({ page: 1, page_size: 200, status: 1 }),
    ]);
    deptOptions.value = flattenDepts(tree as DeptNode[]);
    warehouseOptions.value = (warehouses.list ?? []).map((item: Warehouse) => ({
      label: item.warehouse_name,
      value: item.id,
    }));
    materialOptions.value = (materials.list ?? []).map((item: MaterialRow) => ({
      label: `${item.material_code} ${item.material_name}`,
      value: item.id,
    }));
  } catch (error) {
    MessagePlugin.warning(errorText(error, '主数据加载失败（下拉框可能为空）'));
  }
}

/** 年度下拉（当前年 +1 到 -5：计划是「今年编明年」，所以留一年未来） */
const yearOptions = (() => {
  const year = currentYear();
  return Array.from({ length: 7 }, (_, index) => {
    const value = year + 1 - index;
    return { label: `${value} 年`, value };
  });
})();

// ==================== Tab 1：计划列表 ====================

const filter = reactive<FilterState>(emptyFilter());
const rows = ref<PlanRow[]>([]);
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const statusOptions = STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const planTypeOptions = PLAN_TYPE_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

const columns: ColumnDef[] = [
  { colKey: 'plan_no', title: '计划单号', width: 170 },
  { colKey: 'plan_name', title: '计划名称', width: 260 },
  { colKey: 'dept_name', title: '编制部门 / 编制人', width: 170 },
  { colKey: 'total_planned_qty', title: '计划量', width: 130, align: 'right' },
  { colKey: 'total_planned_amount', title: '计划金额', width: 150, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 280, fixed: 'right', exportable: false },
];

const filters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '计划单号 / 名称 / 部门 / 备注',
    width: 300,
    testid: 'pp-filter-keyword',
  },
  { key: 'status', type: 'select', placeholder: '全部状态', width: 140, options: statusOptions, testid: 'pp-filter-status' },
  {
    key: 'plan_type',
    type: 'select',
    placeholder: '全部类型',
    width: 140,
    options: planTypeOptions,
    testid: 'pp-filter-type',
  },
  { key: 'plan_year', type: 'custom', testid: 'pp-filter-year-def' },
  { key: 'dept_id', type: 'custom', testid: 'pp-filter-dept-def' },
];

/** 年份下拉桥（`null` = 全部；t-select 的 v-model 不收 null） */
const yearModel = computed<number | string | undefined>({
  get: () => (filter.plan_year === null ? undefined : filter.plan_year),
  set: (value) => {
    filter.plan_year = typeof value === 'number' ? value : null;
  },
});

/** 部门下拉桥（`''` = 全部） */
const deptModel = computed<string | undefined>({
  get: () => filter.dept_id || undefined,
  set: (value) => {
    filter.dept_id = typeof value === 'string' ? value : '';
  },
});

const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const typed = state as FilterState;
  return buildQuery(
    {
      keyword: typed.keyword ?? '',
      status: (typed.status ?? '') as PlanStatus | '',
      plan_type: (typed.plan_type ?? '') as PlanType | '',
      plan_year: typed.plan_year ?? null,
      dept_id: typed.dept_id ?? '',
    },
    page,
    pageSize,
  );
};

const fetcher: ListFetcher<PlanRow> = (query) => fetchPlanPage(query as PlanFilter);

function onLoaded(payload: { rows: PlanRow[] }): void {
  rows.value = payload.rows;
}

async function reloadList(page?: number): Promise<void> {
  await listRef.value?.load(page ?? listRef.value.current);
}

function handleSearch(): void {
  void reloadList(1);
}

// ==================== Tab 2：执行率列表 ====================

const execFilter = reactive<ExecFilterState>(emptyExecFilter());
const execListRef = ref<InstanceType<typeof ListPage> | null>(null);

const execColumns: ColumnDef[] = [
  { colKey: 'plan_no', title: '计划单号', width: 160 },
  { colKey: 'plan_name', title: '计划名称', width: 250 },
  { colKey: 'planned_qty', title: '计划量 / 已下单', width: 150, align: 'right' },
  { colKey: 'exec_rate', title: '执行率', width: 210 },
  { colKey: 'exec_state', title: '状态', width: 190 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right', exportable: false },
];

const execFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '计划单号 / 名称 / 部门 / 编制人',
    width: 280,
    testid: 'pp-exec-filter-keyword',
  },
  { key: 'status', type: 'select', placeholder: '全部状态', width: 130, options: statusOptions, testid: 'pp-exec-filter-status' },
  {
    key: 'plan_type',
    type: 'select',
    placeholder: '全部类型',
    width: 130,
    options: planTypeOptions,
    testid: 'pp-exec-filter-type',
  },
  { key: 'plan_year', type: 'custom', testid: 'pp-exec-filter-year-def' },
  { key: 'plan_month', type: 'custom', testid: 'pp-exec-filter-month-def' },
  { key: 'min_rate', type: 'custom', testid: 'pp-exec-filter-min-rate-def' },
  { key: 'dept_id', type: 'custom', testid: 'pp-exec-filter-dept-def' },
];

const execYearModel = computed<number | string | undefined>({
  get: () => (execFilter.plan_year === null ? undefined : execFilter.plan_year),
  set: (value) => {
    execFilter.plan_year = typeof value === 'number' ? value : null;
  },
});

const execMonthModel = computed<number | string | undefined>({
  get: () => (execFilter.plan_month === null ? undefined : execFilter.plan_month),
  set: (value) => {
    execFilter.plan_month = typeof value === 'number' ? value : null;
  },
});

const execMinRateModel = computed<number | string | undefined>({
  get: () => (execFilter.min_rate === null ? undefined : execFilter.min_rate),
  set: (value) => {
    execFilter.min_rate = typeof value === 'number' ? value : null;
  },
});

const execQueryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const typed = state as ExecFilterState;
  return buildExecQuery(
    {
      keyword: typed.keyword ?? '',
      status: (typed.status ?? '') as PlanStatus | '',
      plan_type: (typed.plan_type ?? '') as PlanType | '',
      plan_year: typed.plan_year ?? null,
      plan_month: typed.plan_month ?? null,
      dept_id: typed.dept_id ?? '',
      min_rate: typed.min_rate ?? null,
    },
    page,
    pageSize,
  );
};

const execFetcher: ListFetcher<PlanExecRow> = (query) => fetchExecutionPage(query as ExecutionFilter);

async function reloadExecList(page?: number): Promise<void> {
  await execListRef.value?.load(page ?? execListRef.value.current);
}

function handleExecSearch(): void {
  void reloadExecList(1);
}

// ==================== 新建 / 编辑 ====================

const formVisible = ref(false);
const saving = ref(false);
const form = reactive<PlanForm>(emptyPlanForm());
/** 父单候选（同年度年度计划；只在月度计划时加载） */
const annualPlans = ref<PlanRow[]>([]);

const formHeader = computed(() => (form.id ? '编辑采购计划（只有草稿能改）' : '新建采购计划'));

const parentOptions = computed(() =>
  parentCandidates(annualPlans.value, Number(form.plan_year) || 0, form.id).map((plan) => ({
    label: `${plan.plan_no} ${plan.plan_name}`,
    value: plan.id,
  })),
);

const lineColumns: PrimaryTableCol[] = [
  { colKey: 'material_id', title: '物料', width: 280 },
  { colKey: 'warehouse_id', title: '仓库', width: 180 },
  { colKey: 'planned_qty', title: '计划量', width: 150 },
  { colKey: 'planned_price', title: '预估单价', width: 160 },
  { colKey: 'planned_amount', title: '行金额（预览）', width: 150, align: 'right' },
  { colKey: 'remark', title: '行备注', width: 180 },
  { colKey: 'ops', title: '操作', width: 80 },
];

/** 拉父单候选（同年度年度计划） */
async function loadAnnualPlans(): Promise<void> {
  if (!canList.value) {
    return;
  }
  try {
    const data = await fetchPlanPage({
      page: 1,
      page_size: 200,
      plan_type: 'ANNUAL',
      plan_year: Number(form.plan_year) || currentYear(),
    });
    annualPlans.value = data.list ?? [];
  } catch (error) {
    MessagePlugin.warning(errorText(error, '父年度计划下拉加载失败'));
  }
}

function openCreate(): void {
  Object.assign(form, emptyPlanForm());
  annualPlans.value = [];
  formVisible.value = true;
}

async function openEdit(id: string): Promise<void> {
  try {
    const detail = await fetchPlanDetail(id);
    Object.assign(form, formFromDetail(detail));
    if (form.plan_type === 'MONTHLY') {
      await loadAnnualPlans();
    }
    formVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载计划详情失败'));
  }
}

/** 类型切换：月份归位 + 父单候选跟着年度变 */
async function handleTypeChange(value: SelectValue): Promise<void> {
  const next = (value as PlanType) ?? 'ANNUAL';
  form.plan_type = next;
  form.plan_month = monthForType(next, form.plan_month);
  if (next === 'MONTHLY') {
    await loadAnnualPlans();
  } else {
    form.parent_plan_id = '';
  }
}

function addLine(): void {
  form.lines.push(newPlanLine());
}

function removeLine(key: number): void {
  if (form.lines.length <= 1) {
    MessagePlugin.warning('至少保留 1 行明细');
    return;
  }
  form.lines = form.lines.filter((line) => line.key !== key);
}

async function handleSave(): Promise<void> {
  const error = validatePlanForm(form, annualPlans.value.length ? annualPlans.value : undefined);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  saving.value = true;
  try {
    const payload = buildPlanPayload(form);
    const detail = form.id ? await updatePlan(form.id, payload) : await createPlan(payload);
    MessagePlugin.success(`已保存 ${detail.plan.plan_no}（草稿）`);
    formVisible.value = false;
    await reloadList();
  } catch (err) {
    MessagePlugin.error(errorText(err, '保存采购计划失败'));
  } finally {
    saving.value = false;
  }
}

// ==================== 提交 / 删除 ====================

async function handleSubmit(row: PlanRow): Promise<void> {
  try {
    const detail = await submitPlan(row.id, { reason: '采购计划提交审批' });
    MessagePlugin.success(
      `${detail.plan.plan_no} 已提交审批${
        Number(detail.plan.total_planned_amount) >= 500000 ? '（计划金额 ≥ 50 万：已自动加一道财务经理复核）' : ''
      }`,
    );
    await reloadList();
  } catch (error) {
    MessagePlugin.error(errorText(error, '提交审批失败'));
  }
}

async function handleDelete(row: PlanRow): Promise<void> {
  const dialog = DialogPlugin.confirm({
    header: '删除采购计划',
    theme: 'danger',
    body: `确认删除 ${row.plan_no}？只有草稿能删（非草稿后端会 409），删掉后不可恢复。`,
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deletePlan(row.id);
        MessagePlugin.success(`${row.plan_no} 已删除`);
        dialog.hide();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(errorText(error, '删除失败'));
      }
    },
  });
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detail = ref<PlanDetail | null>(null);

const detailHeader = computed(() => (detail.value ? `${detail.value.plan.plan_no} 采购计划` : '采购计划详情'));

const detailItemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 140 },
  { colKey: 'material_name', title: '物料名称', width: 180 },
  { colKey: 'warehouse_name', title: '仓库', width: 130 },
  { colKey: 'unit_name', title: '单位', width: 80 },
  { colKey: 'planned_qty', title: '计划量', width: 120, align: 'right' },
  { colKey: 'executed_qty', title: '已执行量', width: 120, align: 'right' },
  { colKey: 'planned_price', title: '预估单价', width: 130, align: 'right' },
  { colKey: 'planned_amount', title: '金额', width: 140, align: 'right' },
  { colKey: 'remark', title: '行备注', width: 160 },
];

async function openDetail(id: string): Promise<void> {
  try {
    detail.value = await fetchPlanDetail(id);
    detailVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载详情失败'));
  }
}

// ==================== 转采购申请 ====================

const convertVisible = ref(false);
const converting = ref(false);
const convertForm = reactive<ConvertForm>({
  plan_id: '',
  dept_id: '',
  req_date: '',
  demand_date: '',
  tax_rate: 0,
  remark: '',
  lines: [],
});
/** 转申请的来源详情（公司级计划要判「必须选部门」） */
const convertSource = ref<PlanDetail | null>(null);

const convertHeader = computed(() =>
  convertSource.value ? `转采购申请（${convertSource.value.plan.plan_no}）` : '转采购申请',
);

const convertColumns: PrimaryTableCol[] = [
  { colKey: 'selected', title: '转', width: 60 },
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 130 },
  { colKey: 'material_name', title: '物料名称', width: 170 },
  { colKey: 'warehouse_name', title: '仓库', width: 120 },
  { colKey: 'planned_qty', title: '计划量', width: 110, align: 'right' },
  { colKey: 'executed_qty', title: '已执行', width: 110, align: 'right' },
  { colKey: 'remaining', title: '剩余可转', width: 120, align: 'right' },
  { colKey: 'qty', title: '本次转出', width: 150 },
  { colKey: 'est_price', title: '预估单价', width: 160 },
];

async function openConvert(id: string): Promise<void> {
  try {
    const source = await fetchPlanDetail(id);
    if (source.plan.status !== 'APPROVED') {
      MessagePlugin.warning('只有「已批准」的计划可以转采购申请');
      return;
    }
    convertSource.value = source;
    Object.assign(convertForm, emptyConvertForm(source));
    convertVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载计划详情失败'));
  }
}

function selectAllConvert(selected: boolean): void {
  convertForm.lines.forEach((line) => {
    // 剩余 0 的行没有可转量：全选也不该勾上（否则必然撞「转出必须 > 0」）
    line.selected = selected && Number(line.remaining) > 0;
  });
}

async function handleConvert(): Promise<void> {
  const companyLevel = !convertForm.dept_id && !convertSource.value?.plan.dept_id;
  const error = validateConvert(convertForm, companyLevel);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  converting.value = true;
  try {
    const result = await convertPlanToRequisition(convertForm.plan_id, buildConvertPayload(convertForm));
    MessagePlugin.success(
      `已生成 ${result.requisitions.length} 张采购申请：${convertedSummaryText(result.requisitions)}（转出 ${
        result.converted_qty
      }，预计金额 ${result.converted_amount}）`,
    );
    convertVisible.value = false;
    detailVisible.value = false;
    await reloadList();
  } catch (err) {
    MessagePlugin.error(errorText(err, '转采购申请失败'));
  } finally {
    converting.value = false;
  }
}

// ==================== 执行率下钻 ====================

const execVisible = ref(false);
const execution = ref<PlanExecution | null>(null);

const execHeader = computed(() => (execution.value ? `${execution.value.plan.plan_no} 执行率` : '执行率'));

const execItemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 130 },
  { colKey: 'material_name', title: '物料名称', width: 160 },
  { colKey: 'warehouse_name', title: '仓库', width: 110 },
  { colKey: 'planned_qty', title: '计划量', width: 110, align: 'right' },
  { colKey: 'req_qty', title: '已申请', width: 110, align: 'right' },
  { colKey: 'ordered_qty', title: '已下单', width: 110, align: 'right' },
  { colKey: 'remaining_qty', title: '剩余', width: 110, align: 'right' },
  { colKey: 'exec_rate', title: '执行率', width: 190 },
  { colKey: 'exec_state', title: '状态', width: 110 },
  { colKey: 'sources', title: '来源', width: 170 },
];

const sourceColumns: PrimaryTableCol[] = [
  { colKey: 'req_no', title: '申请单号', width: 170 },
  { colKey: 'req_status_label', title: '申请状态', width: 100 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'po_no', title: '采购单号', width: 170 },
  { colKey: 'po_status_label', title: '采购状态', width: 110 },
  { colKey: 'counted', title: '是否计入', width: 140 },
];

/** 反范式化给来源表用：未转单时 `po_no` 是 null，模板里直接显示占位 */
function sourcePoText(row: PlanExecItem['sources'][number]): string {
  return row.po_no ?? '未转采购单';
}

async function openExecution(planId: string): Promise<void> {
  try {
    execution.value = await fetchPlanExecution(planId);
    execVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载执行率失败'));
  }
}

onMounted(async () => {
  // 列表首屏由 ListPage 自己拉（`void load(1)` 在组件 setup 里）；
  // 这里只补主数据下拉，免得重复请求一次列表。
  await loadOptions();
});
</script>

<style scoped>
.pp-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 4px 0 16px;
}

.pp-alert {
  margin-bottom: 8px;
}

.gate-wrap {
  display: inline-flex;
}

.hint-inline {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
}

.cell-right {
  align-items: flex-end;
}

.cell-sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.empty-block {
  padding: 24px 0;
  color: var(--td-text-color-placeholder);
}

.lines-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 16px 0 8px;
}

.lines-title {
  font-weight: 600;
}

.totals-row {
  margin-top: 8px;
  font-size: 13px;
}

.detail-desc {
  margin-bottom: 8px;
}

.rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rate-cell :deep(.t-progress) {
  flex: 1;
}

.rate-cell__text {
  min-width: 56px;
  text-align: right;
}

.exec-kpi {
  display: flex;
  flex-wrap: wrap;
  gap: 24px;
  padding: 8px 0 4px;
}

.exec-kpi__item {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.exec-kpi__label {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.exec-kpi__value {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 16px;
  font-weight: 600;
}

.exec-note {
  padding-bottom: 4px;
}

.src-block {
  padding: 8px 12px;
}

.src-block__title {
  margin-bottom: 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}
</style>
