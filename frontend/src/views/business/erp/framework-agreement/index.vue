<template>
  <div class="fwa-page">
    <!-- 两个取数码都没有：页面能进（有页面显形码）但两个 Tab 的接口全 403 —— 说清缺什么 -->
    <t-alert
      v-if="!canList && !canExec"
      theme="warning"
      class="fwa-alert"
      data-testid="fwa-no-perm"
      :message="`当前账号没有框架协议的取数权限（${PERM.list} / ${PERM.execution}），请让管理员在角色授权里勾上对应按钮行`"
    />

    <t-tabs v-if="canList || canExec" v-model="activeTab" data-testid="fwa-tabs">
      <!-- ==================== Tab 1：协议台账（维护视角：建 / 改 / 删 / 提交审批） ==================== -->
      <t-tab-panel v-if="canList" value="ledger" label="协议台账">
        <ListPage
          ref="ledgerRef"
          title="框架协议"
          testid="fwa-ledger"
          table-testid="fwa-ledger-table"
          meta-unit="份"
          :columns="ledgerColumns"
          :filters="ledgerFilters"
          :filter-state="ledgerFilter"
          :fetcher="fetcher"
          :query-builder="ledgerQueryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #toolbar>
            <t-tooltip
              :content="
                canCreate
                  ? '签一份框架协议：供应商 + 有效期 + 价格条款 + 物料范围。审批通过（采购经理 → 财务经理）后才能被采购单引用'
                  : `当前账号没有 ${PERM.create} 权限`
              "
              placement="top-right"
            >
              <span class="gate-wrap">
                <t-button theme="primary" :disabled="!canCreate" data-testid="fwa-new" @click="openCreate">
                  <template #icon><t-icon name="add" /></template>
                  新建协议
                </t-button>
              </span>
            </t-tooltip>
          </template>

          <template #op="{ row }">
            <t-space size="small">
              <t-link theme="primary" data-testid="fwa-ledger-detail" @click="openDetail(row, 'ledger')">
                详情
              </t-link>
              <t-link theme="default" data-testid="fwa-ledger-exec" @click="openDetail(row, 'execution')">
                执行量
              </t-link>
              <!-- 三个写入口逐条 gate：只有草稿能改 / 删 / 提（判据来自后端 editable，不自己写状态机） -->
              <t-link
                v-if="canUpdate"
                theme="primary"
                data-testid="fwa-ledger-edit"
                :disabled="row.status !== 'DRAFT'"
                @click="openEdit(row)"
              >
                编辑
              </t-link>
              <t-link
                v-if="canSubmit"
                theme="primary"
                data-testid="fwa-ledger-submit"
                :disabled="row.status !== 'DRAFT'"
                @click="handleSubmit(row)"
              >
                提交
              </t-link>
              <t-link
                v-if="canDelete"
                theme="danger"
                data-testid="fwa-ledger-delete"
                :disabled="row.status !== 'DRAFT'"
                @click="handleDelete(row)"
              >
                删除
              </t-link>
            </t-space>
          </template>

          <template #agreement_no="{ row }">
            <span class="mono">{{ row.agreement_no }}</span>
          </template>
          <template #agreement_name="{ row }">
            <div class="cell-stack">
              <span class="cell-strong">{{ row.agreement_name }}</span>
              <span class="cell-sub">
                {{ priceModeLabel(row.price_mode) }} · {{ priceTaxText(row.price_includes_tax) }} ·
                {{ row.price_currency }}
              </span>
            </div>
          </template>
          <template #supplier="{ row }">
            <div class="cell-stack">
              <span>{{ supplierText(row) }}</span>
              <span class="cell-sub">{{ ownerText(row) }}</span>
            </div>
          </template>
          <template #period="{ row }">
            <div class="cell-stack">
              <span class="mono">{{ periodText(row) }}</span>
              <span class="cell-sub">{{ validityText(row) }}</span>
            </div>
          </template>
          <template #cap_qty="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.cap_qty) }">
                {{ capText(row.cap_qty) }}
              </span>
              <span class="cell-sub">单头 {{ capText(row.total_max_qty) }}</span>
            </div>
          </template>
          <template #cap_amount="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.cap_amount) }">
                {{ capMoneyText(row.cap_amount) }}
              </span>
              <span class="cell-sub">已用 {{ moneyText(row.executed_amount_capped) }}</span>
            </div>
          </template>
          <template #item_count="{ row }">
            <div class="cell-stack">
              <span>{{ itemSummaryText(row) }}</span>
              <span class="cell-sub">{{ docCountText(row) }}</span>
            </div>
          </template>
          <template #status="{ row }">
            <div class="cell-stack">
              <t-tag :theme="statusTheme(row.status)" variant="light" size="small">
                {{ row.status_label || agreementStatusLabel(row.status) }}
              </t-tag>
              <t-tag :theme="validityTheme(row.validity_state)" variant="outline" size="small">
                {{ row.validity_state_label || validityLabel(row.validity_state) }}
              </t-tag>
            </div>
          </template>

          <template #footer>
            <div class="fwa-note" data-testid="fwa-ledger-note">
              台账支持新建 / 编辑 / 删除 / 提交审批（均需对应权限码，且**只有草稿**能改删提）：
              提交后走 <span class="mono">fw_agreement</span> 审批流（采购经理 → 财务经理），
              通过后协议才生效、才能被采购单引用。改单**不许**把物料上限改到小于已执行量，
              也不许删掉已有执行量的物料行（否则下单准入与执行量报表会互相矛盾）。
              用量与来源单据请看「执行量」Tab。
            </div>
          </template>

          <template #empty>
            <div class="cell-sub">{{ emptyListText(ledgerFilter, 'ledger') }}</div>
          </template>
        </ListPage>
      </t-tab-panel>

      <!-- ==================== Tab 2：执行量（执行视角 + 下钻） ==================== -->
      <t-tab-panel v-if="canExec" value="execution" label="执行量">
        <ListPage
          ref="execRef"
          title="协议执行量"
          testid="fwa-exec"
          table-testid="fwa-exec-table"
          meta-unit="份"
          :columns="execColumns"
          :filters="execFilters"
          :filter-state="execFilter"
          :fetcher="execFetcher"
          :query-builder="execQueryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #filter-over_only="{ value, set }">
            <t-checkbox
              :model-value="value === true"
              data-testid="fwa-filter-over"
              @change="(checked) => toggleOverOnly(Boolean(checked), set)"
            >
              只看超限
            </t-checkbox>
          </template>

          <template #op="{ row }">
            <t-link theme="primary" data-testid="fwa-exec-detail" @click="openDetail(row, 'execution')">
              下钻
            </t-link>
          </template>

          <template #agreement_no="{ row }">
            <span class="mono">{{ row.agreement_no }}</span>
          </template>
          <template #agreement_name="{ row }">
            <div class="cell-stack">
              <span class="cell-strong">{{ row.agreement_name }}</span>
              <span class="cell-sub">
                {{ priceModeLabel(row.price_mode) }} · {{ priceTaxText(row.price_includes_tax) }}
              </span>
            </div>
          </template>
          <template #supplier="{ row }">
            <span>{{ supplierText(row) }}</span>
          </template>
          <template #validity="{ row }">
            <div class="cell-stack">
              <t-tag :theme="validityTheme(row.validity_state)" variant="light" size="small">
                {{ row.validity_state_label || validityLabel(row.validity_state) }}
              </t-tag>
              <span v-if="validityLeftText(row)" class="cell-sub">{{ validityLeftText(row) }}</span>
            </div>
          </template>
          <template #used="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono">{{ decimalText(row.executed_qty) }}</span>
              <span class="cell-sub">封顶 {{ capText(row.cap_qty) }}</span>
            </div>
          </template>
          <template #remaining="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.remaining_qty), 'is-over': isNegative(row.remaining_qty) }">
                {{ remainingText(row.remaining_qty) }}
              </span>
              <span class="cell-sub">{{ usedAmountLabel(row.price_includes_tax) }} {{ usedAmountOf(row) }}</span>
            </div>
          </template>
          <template #exec_rate="{ row }">
            <div class="rate-cell">
              <t-progress
                :percentage="progressPercent(row.exec_qty_rate_pct)"
                theme="line"
                :status="progressStatus(row.exec_state)"
                :label="false"
              />
              <span class="rate-cell__text mono">{{ pctText(row.exec_qty_rate_pct) }}</span>
            </div>
          </template>
          <template #exec_state="{ row }">
            <div class="cell-stack">
              <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
                {{ row.exec_state_label || execStateLabel(row.exec_state) }}
              </t-tag>
              <span class="cell-sub">{{ lineCountsBrief(row) }}</span>
            </div>
          </template>
          <template #docs="{ row }">
            <span class="cell-sub-inline">{{ docCountText(row) }}</span>
          </template>

          <template #empty>
            <div class="cell-sub">{{ emptyListText(execFilter, 'execution') }}</div>
          </template>
        </ListPage>
      </t-tab-panel>
    </t-tabs>

    <!-- ==================== 新建 / 编辑协议（只有草稿能改） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="formHeader"
      width="1180px"
      :confirm-btn="{ content: '保存草稿', loading: saving }"
      :close-on-overlay-click="false"
      data-testid="fwa-form-dialog"
      @confirm="handleSave"
    >
      <t-form label-width="104px" class="fwa-form">
        <t-row :gutter="[16, 12]">
          <t-col :span="8">
            <t-form-item label="协议名称" required-mark>
              <t-input v-model="form.agreement_name" :maxlength="MAX_NAME" placeholder="例：2026 年度钢材框架采购协议" data-testid="fwa-form-name" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="供应商" required-mark>
              <t-select
                v-model="form.supplier_id"
                :options="supplierOptions"
                placeholder="选择供应商"
                filterable
                data-testid="fwa-form-supplier"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="有效期起" required-mark>
              <t-date-picker v-model="form.start_date" value-type="YYYY-MM-DD" placeholder="含当日" data-testid="fwa-form-start" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="有效期止" required-mark>
              <t-date-picker v-model="form.end_date" value-type="YYYY-MM-DD" placeholder="含当日" data-testid="fwa-form-end" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="签订日期">
              <t-date-picker v-model="form.sign_date" value-type="YYYY-MM-DD" placeholder="可与生效日不同" data-testid="fwa-form-sign" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="定价方式">
              <t-radio-group :value="form.price_mode" data-testid="fwa-form-mode" @change="handleModeChange">
                <t-radio value="FIXED">固定价</t-radio>
                <t-radio value="FLOATING">随行就市</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="协议价口径">
              <t-switch v-model="form.price_includes_tax" data-testid="fwa-form-tax" />
              <span class="cell-sub">{{ form.price_includes_tax ? '含税价' : '不含税价' }}</span>
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="币种">
              <t-input v-model="form.price_currency" :maxlength="3" placeholder="CNY" data-testid="fwa-form-currency" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="允许高出">
              <t-input-number
                v-model="form.price_tolerance_pct"
                :min="0"
                :max="100"
                :decimal-places="2"
                theme="normal"
                suffix="%"
                data-testid="fwa-form-tolerance"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="经办部门">
              <t-select
                v-model="form.dept_id"
                :options="deptOptions"
                placeholder="不选 = 公司级"
                filterable
                clearable
                data-testid="fwa-form-dept"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="总量上限">
              <t-input-number
                v-model="form.total_max_qty"
                :min="0"
                :decimal-places="4"
                theme="normal"
                placeholder="空 = 不限"
                data-testid="fwa-form-total-qty"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="总额上限">
              <t-input-number
                v-model="form.total_max_amount"
                :min="0"
                :decimal-places="4"
                theme="normal"
                placeholder="空 = 不限"
                data-testid="fwa-form-total-amount"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="调价规则">
              <t-textarea
                v-model="form.price_adjust_rule"
                :maxlength="MAX_RULE"
                :autosize="{ minRows: 1, maxRows: 3 }"
                :placeholder="form.price_mode === 'FLOATING' ? '随行就市必填：如「随钢材指数月度均价上浮不超过 3%」' : '固定价可留空'"
                data-testid="fwa-form-rule"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="备注">
              <t-textarea v-model="form.remark" :maxlength="MAX_REMARK" :autosize="{ minRows: 1, maxRows: 3 }" placeholder="签约背景 / 说明（审批人会看）" data-testid="fwa-form-remark" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <div class="lines-head">
        <span class="lines-title">物料范围（一行 = 一个物料：协议价 + 税率 + 数量上限）</span>
        <t-button size="small" variant="outline" data-testid="fwa-add-line" @click="addLine">
          <template #icon><t-icon name="add" /></template>
          加一行
        </t-button>
      </div>

      <t-table row-key="key" :data="form.lines" :columns="lineColumns" size="small" data-testid="fwa-lines-table">
        <template #material_id="{ row }">
          <t-select
            v-model="row.material_id"
            :options="materialOptions"
            placeholder="选择物料"
            filterable
            size="small"
            data-testid="fwa-line-material"
          />
        </template>
        <template #agreed_price="{ row }">
          <t-input-number
            v-model="row.agreed_price"
            :min="0"
            :decimal-places="6"
            size="small"
            placeholder="0 = 赠品"
            data-testid="fwa-line-price"
          />
        </template>
        <template #tax_rate="{ row }">
          <t-input-number
            v-model="row.tax_rate_pct"
            :min="0"
            :max="100"
            :decimal-places="2"
            size="small"
            suffix="%"
            data-testid="fwa-line-tax"
          />
        </template>
        <template #max_qty="{ row }">
          <t-input-number
            v-model="row.max_qty"
            :min="0"
            :decimal-places="4"
            size="small"
            placeholder="空 = 不限"
            data-testid="fwa-line-max-qty"
          />
        </template>
        <template #executed_qty="{ row }">
          <span class="mono" :class="{ 'is-over': hasUsed(row) }">{{ decimalText(row.executed_qty ?? 0) }}</span>
        </template>
        <template #remark="{ row }">
          <t-input v-model="row.remark" size="small" placeholder="可空" data-testid="fwa-line-remark" />
        </template>
        <template #ops="{ row }">
          <t-link theme="danger" data-testid="fwa-remove-line" @click="removeLine(row.key)">删除</t-link>
        </template>
      </t-table>
      <div class="cell-sub fwa-form-hint">
        单位由物料基础单位带出（与采购单同口径）；「已用」列 = 下单累计，新上限**不能**小于它。
        空物料范围可以存草稿，但**提交审批前必须有物料**。
      </div>
    </t-dialog>

    <!-- ==================== 详情 / 下钻抽屉（两个 Tab 共用一份取数） ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="drawerHeader"
      size="1080px"
      :footer="false"
      data-testid="fwa-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="fwa-detail-error" />

        <template v-if="head">
          <!-- KPI 排：已用 / 剩余 / 执行率 / 金额 -->
          <div class="kpi-row" data-testid="fwa-kpi">
            <div class="kpi-cell">
              <span class="kpi-label">已用量</span>
              <span class="kpi-value mono">{{ decimalText(head.executed_qty) }}</span>
              <span class="kpi-sub">{{ docCountText(head) }}计入</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">剩余可下单</span>
              <span
                class="kpi-value mono"
                :class="{ 'is-unlimited': isUnlimited(head.remaining_qty), 'is-over': isNegative(head.remaining_qty) }"
                data-testid="fwa-kpi-remaining"
              >
                {{ remainingText(head.remaining_qty) }}
              </span>
              <span class="kpi-sub">封顶 {{ capText(head.cap_qty) }}</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">数量执行率</span>
              <span class="kpi-value mono" data-testid="fwa-kpi-rate">{{ pctText(head.exec_qty_rate_pct) }}</span>
              <span class="kpi-sub">{{ head.exec_state_label || execStateLabel(head.exec_state) }}</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">{{ usedAmountLabel(head.price_includes_tax) }}</span>
              <span class="kpi-value mono">{{ moneyText(head.executed_amount_capped) }}</span>
              <span class="kpi-sub">金额封顶 {{ capMoneyText(head.cap_amount) }}</span>
            </div>
          </div>
          <div class="kpi-foot cell-sub" data-testid="fwa-kpi-foot">
            {{ lineCountsText(head) }} · 执行率 = 已用量 ÷ 封顶（不限量时没有分母，显示「不限」）
          </div>

          <!-- 单头信息 -->
          <t-descriptions :column="3" size="small" bordered class="fwa-desc">
            <t-descriptions-item label="协议号">
              <span class="mono">{{ head.agreement_no }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="协议名称">{{ head.agreement_name }}</t-descriptions-item>
            <t-descriptions-item label="供应商">{{ supplierText(head) }}</t-descriptions-item>
            <t-descriptions-item label="经办">{{ ownerText(head) }}</t-descriptions-item>
            <t-descriptions-item label="签订日期">{{ head.sign_date || '—' }}</t-descriptions-item>
            <t-descriptions-item label="有效期">
              <div class="cell-stack">
                <span class="mono">{{ periodText(head) }}</span>
                <span class="cell-sub">{{ validityText(head) }}</span>
              </div>
            </t-descriptions-item>
            <t-descriptions-item label="协议状态">
              <t-tag :theme="statusTheme(head.status)" variant="light" size="small">
                {{ head.status_label || agreementStatusLabel(head.status) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="有效期状态">
              <t-tag :theme="validityTheme(head.validity_state)" variant="light" size="small">
                {{ head.validity_state_label || validityLabel(head.validity_state) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="今天是否生效">
              <t-tag :theme="head.is_effective ? 'success' : 'default'" variant="light" size="small">
                {{ head.is_effective ? '生效（可下单）' : '不生效（下单会被拦）' }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="定价方式">
              {{ head.price_mode_label || priceModeLabel(head.price_mode) }}
            </t-descriptions-item>
            <t-descriptions-item label="协议价口径">{{ priceTaxText(head.price_includes_tax) }}</t-descriptions-item>
            <t-descriptions-item label="币种">{{ head.price_currency }}</t-descriptions-item>
            <t-descriptions-item label="允许高于协议价">{{ toleranceText(head.price_tolerance) }}</t-descriptions-item>
            <t-descriptions-item label="单头数量上限">{{ capText(head.total_max_qty) }}</t-descriptions-item>
            <t-descriptions-item label="单头金额上限">{{ capMoneyText(head.total_max_amount) }}</t-descriptions-item>
            <t-descriptions-item label="有效数量封顶">
              <span class="mono">{{ capText(head.cap_qty) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="明细上限合计">
              <span class="mono">{{ capText(head.item_max_qty_sum) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="最近下单日">{{ head.last_order_on || '未下过单' }}</t-descriptions-item>
            <t-descriptions-item label="创建时间">{{ timeText(head.created_at) }}</t-descriptions-item>
          </t-descriptions>

          <!-- 物料范围与逐行执行量 -->
          <div class="section-head">
            <span class="section-title">物料范围与执行量</span>
            <span class="cell-sub">
              封顶 <span class="mono">{{ capText(head.cap_qty) }}</span> ·
              明细全部设上限时单头封顶取明细合计，有一行不限则整体不限
            </span>
          </div>
          <t-table
            row-key="agreement_item_id"
            :data="detail?.items ?? []"
            :columns="itemColumns"
            size="small"
            data-testid="fwa-detail-items"
          >
            <template #material="{ row }">
              <div class="cell-stack">
                <span>{{ materialText(row) }}</span>
                <span class="cell-sub">{{ row.unit_name || '—' }}</span>
              </div>
            </template>
            <template #agreed_price="{ row }">
              <div class="cell-stack cell-right">
                <span class="mono">{{ decimalText(row.agreed_price) }}</span>
                <span class="cell-sub">{{ taxRateText(row.tax_rate) }}</span>
              </div>
            </template>
            <template #used_qty="{ row }">
              <div class="cell-stack cell-right">
                <span class="mono">{{ decimalText(row.executed_qty) }}</span>
                <span v-if="hasDrift(row)" class="cell-sub cell-drift" :title="driftText(row)">
                  {{ driftText(row) }}
                </span>
                <span v-else class="cell-sub">
                  上限 <span :class="{ 'is-unlimited': isUnlimited(row.max_qty) }">{{ capText(row.max_qty) }}</span>
                  · {{ row.order_doc_count }} 张单
                </span>
              </div>
            </template>
            <template #remaining_qty="{ row }">
              <span
                class="mono"
                :class="{ 'is-unlimited': isUnlimited(row.remaining_qty), 'is-over': isNegative(row.remaining_qty) }"
              >
                {{ remainingText(row.remaining_qty) }}
              </span>
            </template>
            <template #exec_rate="{ row }">
              <div class="rate-cell">
                <t-progress
                  :percentage="progressPercent(row.exec_qty_rate_pct)"
                  theme="line"
                  :status="progressStatus(row.exec_state)"
                  :label="false"
                />
                <span class="rate-cell__text mono">{{ pctText(row.exec_qty_rate_pct) }}</span>
              </div>
            </template>
            <template #exec_state="{ row }">
              <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
                {{ row.exec_state_label || execStateLabel(row.exec_state) }}
              </t-tag>
            </template>
            <template #empty>
              <div class="cell-sub" data-testid="fwa-detail-items-empty">
                这份协议没有物料范围（后端 200 + 空列表，不是 404）—— 它不会产生执行量，
                也不会被下单引用。
              </div>
            </template>
          </t-table>

          <!-- 下钻：来源采购单 -->
          <div class="section-head">
            <span class="section-title">来源采购单（下钻）</span>
            <span class="cell-sub" data-testid="fwa-sources-summary">
              {{ sourcesSummaryText(detail?.sources ?? []) }} ·
              作废 / 软删的单也列在这里，但不计入已用量
            </span>
          </div>
          <t-table
            row-key="sourceKey"
            :data="sourcesRows"
            :columns="sourceColumns"
            size="small"
            data-testid="fwa-detail-sources"
          >
            <template #order_no="{ row }">
              <span class="mono">{{ sourceOrderText(row) }}</span>
            </template>
            <template #qty="{ row }">
              <span class="mono">{{ decimalText(row.qty) }}</span>
            </template>
            <template #amount="{ row }">
              <span class="mono">{{ moneyText(row.amount) }}</span>
            </template>
            <template #counted="{ row }">
              <t-tag :theme="sourceVerdictTheme(row)" variant="light" size="small">{{ row.verdict }}</t-tag>
            </template>
            <template #empty>
              <div class="cell-sub" data-testid="fwa-detail-sources-empty">
                还没有采购单引用这份协议 —— 已用量为 0 是正常的。
              </div>
            </template>
          </t-table>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 框架协议页面：**协议台账**（维护：建 / 改 / 删 / 提交审批）+ **执行量**（只读报表）。
 *
 * # 两套接口码，各管一个 Tab（AGENTS 约定 14）
 *
 * | Tab | 接口 | 权限码 |
 * |-----|------|--------|
 * | 协议台账 | `GET /framework-agreement/page`、`GET /{id}`（+ 写接口） | `…:list`（写按钮另有 create/update/delete/submit） |
 * | 执行量 | `GET /framework-agreement/execution/page`、`GET /{id}/execution` | `…:execution` |
 *
 * 页面显形码 `business:erp:framework-agreement:page` **只在路由 `meta.permission`**
 * （后端零引用，本文件也不拿它判任何东西）。
 *
 * # 谁负责什么判断
 *
 * * **能不能改 / 删 / 提** 读列表行的 `status === 'DRAFT'`（后端 `editable` 同一判据），
 *   不在这里自己推状态机；越权请求后端会 409，报文原样弹给用户。
 * * **额度能不能收紧** 由后端判（`check_cap_shrink` / `check_missing_material`）；
 *   页面的「已用」列只是把后端的数显示出来，不做本地拦截 —— 两处各写一套必然分叉。
 *
 * # 页面**不做**的算术（全部交给后端，见 `framework_agreement_execution_dto.rs`）
 *
 * 1. 剩余 = 封顶 − 已用 **不由前端算**（后端给 `remaining_qty`，且**可为负**）；
 * 2. 执行率状态**不用 pct 反推**（读 `exec_state`：`OVER` 可能 pct 舍成 100.00%）；
 * 3. 有效期是否生效**不按日期自己算**（读 `is_effective` / `validity_state`，判据在库里）；
 * 4. 金额**不做跨行加总**（单头 `executed_amount_capped` 已是协议价口径的合计）。
 */
import { computed, reactive, ref } from 'vue';
import type { PrimaryTableCol, SelectValue } from 'tdesign-vue-next';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import {
  fetchMaterialPage,
  fetchSupplierOptions,
  type MaterialRow,
  type Supplier,
} from '@/api/business/erp';
import {
  createAgreement,
  deleteAgreement,
  fetchAgreementDetail,
  fetchAgreementExecution,
  fetchAgreementPage,
  fetchExecutionPage,
  submitAgreement,
  updateAgreement,
  type AgreementDetail,
  type AgreementExecRow,
  type AgreementExecution,
  type AgreementPayload,
  type ExecSource,
  type ExecutionQuery,
  type PriceMode,
} from '@/api/business/erp/framework-agreement';
import {
  PAGE_SIZE,
  STATUS_OPTIONS,
  VALIDITY_OPTIONS,
  agreementStatusLabel,
  buildExecQuery,
  buildLedgerQuery,
  capMoneyText,
  capText,
  decimalText,
  detailTitle,
  docCountText,
  driftText,
  emptyExecFilter,
  emptyLedgerFilter,
  emptyListText,
  execStateLabel,
  execStateTheme,
  hasDrift,
  itemSummaryText,
  lineCountsBrief,
  lineCountsText,
  materialText,
  moneyText,
  ownerText,
  pctText,
  periodText,
  priceModeLabel,
  priceTaxText,
  progressPercent,
  progressStatus,
  remainingText,
  sourceKey,
  sourceOrderText,
  sourcesSummaryText,
  sourceVerdictTheme,
  statusTheme,
  supplierText,
  taxRateText,
  timeText,
  toNumberOrNull,
  toleranceText,
  usedAmountLabel,
  usedAmountOf,
  validityLabel,
  validityLeftText,
  validityText,
  validityTheme,
  type ExecFilterState,
  type LedgerFilterState,
} from './framework-agreement-logic';

/** 本页用到的权限码（与 V0452 / V0456 的 `sys_menu.auth_code` 逐字一致） */
const PERM = {
  /** 页面显形（**只在路由 meta 用**，本文件不拿它判断任何东西） */
  page: 'business:erp:framework-agreement:page',
  /** 台账列表 / 详情（M4-11b 的按钮行，sort 20） */
  list: 'business:erp:framework-agreement:list',
  /** 新建 */
  create: 'business:erp:framework-agreement:create',
  /** 修改 */
  update: 'business:erp:framework-agreement:update',
  /** 删除 */
  delete: 'business:erp:framework-agreement:delete',
  /** 提交审批 */
  submit: 'business:erp:framework-agreement:submit',
  /** 执行量两条只读接口（M4-09） */
  execution: 'business:erp:framework-agreement:execution',
} as const;

/** 表单上限（与后端 dto 的常量一致） */
const MAX_NAME = 128;
const MAX_REMARK = 500;
const MAX_RULE = 2000;

const userStore = useUserStore();
const canList = computed(() => userStore.hasPermission(PERM.list));
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canUpdate = computed(() => userStore.hasPermission(PERM.update));
const canDelete = computed(() => userStore.hasPermission(PERM.delete));
const canSubmit = computed(() => userStore.hasPermission(PERM.submit));
const canExec = computed(() => userStore.hasPermission(PERM.execution));

/** 默认落在第一个有权限的 Tab（只有执行量码的人不该看到空白页） */
const activeTab = ref(canList.value ? 'ledger' : 'execution');

const ledgerRef = ref<InstanceType<typeof ListPage> | null>(null);
const execRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 错误文案归一（后端 400 / 409 的 message 就是给人看的，原样展示） */
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
 * 台账列表取数。
 *
 * M4-11b 起台账 Tab 走 `GET /framework-agreement/page`（码 `…:list`）——
 * 与执行量总览同一份行结构，但**权限码是另一个**（只有执行量码的人不该能看维护台账）。
 */
const fetcher: ListFetcher<AgreementExecRow> = async (query) => {
  try {
    return await fetchAgreementPage(query as ExecutionQuery);
  } catch (error) {
    throw new Error(errorText(error, '取框架协议列表失败'));
  }
};

/** 执行量 Tab 的取数（M4-09 的老接口，码 `…:execution`） */
const execFetcher: ListFetcher<AgreementExecRow> = async (query) => {
  try {
    return await fetchExecutionPage(query as ExecutionQuery);
  } catch (error) {
    throw new Error(errorText(error, '取协议执行量失败'));
  }
};

async function reloadLedger(): Promise<void> {
  await ledgerRef.value?.load(ledgerRef.value.current);
}

async function reloadExec(): Promise<void> {
  await execRef.value?.load(execRef.value.current);
}

// ==================== Tab 1：协议台账 ====================

const ledgerFilter = ref<LedgerFilterState>(emptyLedgerFilter());

const ledgerFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '协议号 / 协议名称 / 供应商 / 经办人',
    width: 300,
    testid: 'fwa-filter-keyword',
  },
  { key: 'status', type: 'select', options: STATUS_OPTIONS, placeholder: '全部状态', width: 140, testid: 'fwa-filter-status' },
  {
    key: 'validity_state',
    type: 'select',
    options: VALIDITY_OPTIONS,
    placeholder: '全部有效期状态',
    width: 160,
    testid: 'fwa-filter-validity',
  },
];

const ledgerQueryBuilder = (filter: LedgerFilterState, page: number, pageSize: number) =>
  buildLedgerQuery(filter, page, pageSize);

const ledgerColumns: ColumnDef[] = [
  { colKey: 'agreement_no', title: '协议号', width: 170, fixed: 'left' },
  { colKey: 'agreement_name', title: '协议名称', minWidth: 200 },
  { colKey: 'supplier', title: '供应商 / 经办', minWidth: 170 },
  { colKey: 'period', title: '有效期', width: 200 },
  { colKey: 'cap_qty', title: '数量封顶', width: 130, align: 'right' },
  { colKey: 'cap_amount', title: '金额封顶', width: 150, align: 'right' },
  { colKey: 'item_count', title: '物料范围', width: 140 },
  { colKey: 'status', title: '状态', width: 120 },
  { colKey: 'op', title: '操作', width: 110, fixed: 'right' },
];

// ==================== Tab 2：执行量 ====================

const execFilter = ref<ExecFilterState>(emptyExecFilter());

const execFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '协议号 / 协议名称 / 供应商 / 经办人',
    width: 300,
    testid: 'fwa-exec-filter-keyword',
  },
  {
    key: 'validity_state',
    type: 'select',
    options: VALIDITY_OPTIONS,
    placeholder: '全部有效期状态',
    width: 160,
    searchOnChange: true,
    testid: 'fwa-exec-filter-validity',
  },
  {
    key: 'over_only',
    type: 'custom',
    defaultValue: false,
    testid: 'fwa-exec-filter-over',
  },
];

const execQueryBuilder = (filter: ExecFilterState, page: number, pageSize: number) =>
  buildExecQuery(filter, page, pageSize);

/** 「只看超限」勾选后立即查询（自定义筛选控件不会自己触发查询） */
function toggleOverOnly(checked: boolean, set: (value: unknown) => void): void {
  set(checked);
  execRef.value?.search();
}

const execColumns: ColumnDef[] = [
  { colKey: 'agreement_no', title: '协议号', width: 168, fixed: 'left' },
  { colKey: 'agreement_name', title: '协议名称', minWidth: 140 },
  { colKey: 'supplier', title: '供应商', width: 112 },
  { colKey: 'validity', title: '有效期', width: 112 },
  { colKey: 'used', title: '已用量 / 封顶', width: 112, align: 'right' },
  { colKey: 'remaining', title: '剩余', width: 124, align: 'right' },
  { colKey: 'exec_rate', title: '数量执行率', width: 132 },
  { colKey: 'exec_state', title: '行状态', width: 156 },
  { colKey: 'docs', title: '单数', width: 92 },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right' },
];

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailMode = ref<'ledger' | 'execution'>('ledger');
/** 抽屉标题先用列表行（详情还没回来时也有标题，不会空白一瞬） */
const detailRow = ref<AgreementExecRow | null>(null);
const detail = ref<AgreementExecution | null>(null);

/** 单头（详情回来后以详情为准，回来后列表行同源） */
const head = computed<AgreementExecRow | null>(() => detail.value?.agreement ?? detailRow.value);

const drawerHeader = computed(() => {
  const title = detailTitle(head.value);
  return detailMode.value === 'execution' ? `${title} · 执行量` : title;
});

/** 来源单据表加一个稳定的行主键（同一张单多行引用同一协议行） */
const sourcesRows = computed(() =>
  (detail.value?.sources ?? []).map((source: ExecSource) => ({ ...source, sourceKey: sourceKey(source) })),
);

/** 打开抽屉并取详情（404 = 协议不存在 / 已软删，报文原样展示） */
async function openDetail(row: AgreementExecRow, mode: 'ledger' | 'execution'): Promise<void> {
  detailMode.value = mode;
  detailRow.value = row;
  detail.value = null;
  detailError.value = '';
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await fetchAgreementExecution(row.agreement_id);
  } catch (error) {
    detailError.value = errorText(error, '取协议执行量详情失败');
  } finally {
    detailLoading.value = false;
  }
}

/** 协议行表（物料范围 + 逐行执行量，两个 Tab 共用；列宽刻意压到抽屉宽度内，免得要横向滚） */
const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行号', width: 60 },
  { colKey: 'material', title: '物料 / 单位', minWidth: 170 },
  { colKey: 'agreed_price', title: '协议价 / 税率', width: 120, align: 'right' },
  { colKey: 'used_qty', title: '已用量 / 上限', width: 150, align: 'right' },
  { colKey: 'remaining_qty', title: '剩余', width: 96, align: 'right' },
  { colKey: 'exec_rate', title: '执行率', width: 150 },
  { colKey: 'exec_state', title: '状态', width: 96 },
];

/** 来源采购单表 */
const sourceColumns: PrimaryTableCol[] = [
  { colKey: 'order_no', title: '采购单号', width: 160 },
  { colKey: 'order_status_label', title: '单据状态', width: 96 },
  { colKey: 'order_date', title: '下单日', width: 104 },
  { colKey: 'qty', title: '数量', width: 84, align: 'right' },
  { colKey: 'amount', title: '金额（不含税）', width: 120, align: 'right' },
  { colKey: 'counted', title: '是否计入', width: 120 },
];

// ==================== 新建 / 编辑（M4-11b） ====================

/** 表单里的一行物料范围 */
type FormLine = {
  /** 本地行 key（t-table 的 row-key，不落库） */
  key: number;
  /** 物料 id */
  material_id: string;
  /** 协议单价（0 = 赠品 / 试用料） */
  agreed_price: number | undefined;
  /** 进项税率（**百分比**显示：13 表示 13%，提交时 ÷100） */
  tax_rate_pct: number | undefined;
  /** 该物料数量上限（空 = 不限；不能小于 `executed_qty`） */
  max_qty: number | undefined;
  /** 已用量（后端回填，只读展示；新建时为空） */
  executed_qty: string;
  /** 行备注 */
  remark: string;
};

/** 建 / 改协议的表单状态 */
type FormState = {
  /** 编辑时的协议 id（空 = 新建） */
  id: string;
  /** 协议号（编辑时展示用） */
  agreement_no: string;
  agreement_name: string;
  supplier_id: string;
  dept_id: string;
  start_date: string;
  end_date: string;
  sign_date: string;
  price_mode: PriceMode;
  price_includes_tax: boolean;
  price_currency: string;
  /** 允许高出协议价的比例（**百分比**显示：5 表示 5%） */
  price_tolerance_pct: number | undefined;
  price_adjust_rule: string;
  total_max_qty: number | undefined;
  total_max_amount: number | undefined;
  remark: string;
  lines: FormLine[];
};

let lineSeed = 1;

function newLine(): FormLine {
  lineSeed += 1;
  return {
    key: lineSeed,
    material_id: '',
    agreed_price: 0,
    tax_rate_pct: 13,
    max_qty: undefined,
    executed_qty: '0',
    remark: '',
  };
}

/** 今天（本地日期，`YYYY-MM-DD`） */
function todayText(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** 默认有效期 = 今天 → 今年 12-31（框架协议通常按年签） */
function emptyForm(): FormState {
  const today = todayText();
  const year = today.slice(0, 4);
  return {
    id: '',
    agreement_no: '',
    agreement_name: '',
    supplier_id: '',
    dept_id: '',
    start_date: today,
    end_date: `${year}-12-31`,
    sign_date: '',
    price_mode: 'FIXED',
    price_includes_tax: false,
    price_currency: 'CNY',
    price_tolerance_pct: 0,
    price_adjust_rule: '',
    total_max_qty: undefined,
    total_max_amount: undefined,
    remark: '',
    lines: [newLine()],
  };
}

const formVisible = ref(false);
const saving = ref(false);
const form = reactive<FormState>(emptyForm());

const formHeader = computed(() => (form.id ? `编辑框架协议 ${form.agreement_no}（只有草稿能改）` : '新建框架协议'));

const lineColumns: PrimaryTableCol[] = [
  { colKey: 'material_id', title: '物料', width: 260 },
  { colKey: 'agreed_price', title: '协议价', width: 150 },
  { colKey: 'tax_rate', title: '税率', width: 120 },
  { colKey: 'max_qty', title: '数量上限', width: 150 },
  { colKey: 'executed_qty', title: '已用', width: 110, align: 'right' },
  { colKey: 'remark', title: '行备注', width: 180 },
  { colKey: 'ops', title: '操作', width: 80 },
];

// ---------- 主数据下拉（懒加载：只读用户进来不该多打三个接口） ----------

const supplierOptions = ref<{ label: string; value: string }[]>([]);
const deptOptions = ref<{ label: string; value: string }[]>([]);
const materialOptions = ref<{ label: string; value: string }[]>([]);
let optionsLoaded = false;

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
  if (optionsLoaded) {
    return;
  }
  try {
    const [tree, suppliers, materials] = await Promise.all([
      getDeptTree({}),
      fetchSupplierOptions(200),
      fetchMaterialPage({ page: 1, page_size: 200, status: 1 }),
    ]);
    deptOptions.value = flattenDepts(tree as DeptNode[]);
    supplierOptions.value = (suppliers ?? []).map((item: Supplier) => ({
      label: `${item.supplier_code} ${item.supplier_name}`,
      value: item.id,
    }));
    materialOptions.value = (materials.list ?? []).map((item: MaterialRow) => ({
      label: `${item.material_code} ${item.material_name}`,
      value: item.id,
    }));
    optionsLoaded = true;
  } catch (error) {
    MessagePlugin.warning(errorText(error, '主数据加载失败（下拉框可能为空）'));
  }
}

// ---------- 打开表单 ----------

function openCreate(): void {
  Object.assign(form, emptyForm());
  lineSeed += 1;
  form.lines = [newLine()];
  void loadOptions();
  formVisible.value = true;
}

/**
 * 编辑：先取**维护口径的详情**（`GET /{id}`，含行备注与已用量）。
 *
 * 不用执行量详情（`GET /{id}/execution`）回填：那份 DTO 没有行备注，
 * 整单替换提交时会把备注抹成空（一次「只改个价」就丢掉全部行备注）。
 */
async function openEdit(row: AgreementExecRow): Promise<void> {
  try {
    const detail: AgreementDetail = await fetchAgreementDetail(row.agreement_id);
    const head = detail.agreement;
    Object.assign(form, {
      id: head.id,
      agreement_no: head.agreement_no,
      agreement_name: head.agreement_name,
      supplier_id: head.supplier_id,
      dept_id: head.dept_id ?? '',
      start_date: head.start_date,
      end_date: head.end_date,
      sign_date: head.sign_date ?? '',
      price_mode: head.price_mode,
      price_includes_tax: head.price_includes_tax,
      price_currency: head.price_currency,
      price_tolerance_pct: Number(head.price_tolerance) * 100,
      price_adjust_rule: head.price_adjust_rule ?? '',
      total_max_qty: toNumberOrNull(head.total_max_qty as number | string | null) ?? undefined,
      total_max_amount: toNumberOrNull(head.total_max_amount as number | string | null) ?? undefined,
      remark: head.remark ?? '',
      lines: detail.items.map((item) => {
        lineSeed += 1;
        return {
          key: lineSeed,
          material_id: item.material_id,
          agreed_price: Number(item.agreed_price),
          tax_rate_pct: Number(item.tax_rate) * 100,
          max_qty: toNumberOrNull(item.max_qty as number | string | null) ?? undefined,
          executed_qty: String(item.executed_qty),
          remark: item.remark ?? '',
        };
      }),
    });
    if (form.lines.length === 0) {
      form.lines = [newLine()];
    }
    await loadOptions();
    formVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载协议详情失败'));
  }
}

function handleModeChange(value: SelectValue): void {
  form.price_mode = (value as PriceMode) ?? 'FIXED';
}

function addLine(): void {
  form.lines.push(newLine());
}

function removeLine(key: number): void {
  if (form.lines.length <= 1) {
    MessagePlugin.warning('至少保留 1 行物料（要空着就整行都不填，提交前再补）');
    return;
  }
  form.lines = form.lines.filter((line) => line.key !== key);
}

/** 「已用」非 0 → 这一行的上限被锁住了（改小会被后端 409，这里只做视觉提示） */
function hasUsed(row: FormLine): boolean {
  return Number(row.executed_qty) > 0;
}

// ---------- 校验 / 提交 ----------

/**
 * 本地校验（**只拦「明显填错了」**，业务判据一律留给后端）。
 *
 * 与后端同一套口径里挑出三条「不用连库就能判」的：名称非空、有效期同序、
 * 浮动价必须有调价规则；其余（上限 > 0、同物料唯一、收紧额度）由后端判并回原文。
 */
function validateForm(state: FormState): string {
  if (!state.agreement_name.trim()) {
    return '协议名称不能为空';
  }
  if (!state.supplier_id) {
    return '请选择供应商';
  }
  if (!state.start_date || !state.end_date) {
    return '有效期起止都要填';
  }
  if (state.start_date > state.end_date) {
    return '有效期起不能晚于有效期止（起止日都含当日，写反了这份协议一天都不会生效）';
  }
  if (state.price_mode === 'FLOATING' && !state.price_adjust_rule.trim()) {
    return '定价方式是「随行就市」时必须写调价规则（没有规则的浮动价 = 没有约定）';
  }
  const filled = state.lines.filter((line) => line.material_id);
  const seen = new Set<string>();
  for (const line of filled) {
    if (seen.has(line.material_id)) {
      return '同一份协议里一个物料只能有一行（两行就是两个协议价）';
    }
    seen.add(line.material_id);
  }
  return '';
}

/** 表单 → 入参（百分比折成 0–1 分数；空上限发 null = 不限，绝不发 0） */
function buildPayload(state: FormState): AgreementPayload {
  const ratio = (pct: number | undefined): string | null =>
    pct === undefined || pct === null ? null : (pct / 100).toFixed(4);
  return {
    agreement_name: state.agreement_name.trim(),
    supplier_id: state.supplier_id,
    dept_id: state.dept_id || null,
    owner_id: null,
    start_date: state.start_date,
    end_date: state.end_date,
    sign_date: state.sign_date || null,
    price_mode: state.price_mode,
    price_includes_tax: state.price_includes_tax,
    price_currency: state.price_currency.trim().toUpperCase() || 'CNY',
    price_tolerance: ratio(state.price_tolerance_pct) ?? '0',
    price_adjust_rule: state.price_adjust_rule.trim() || null,
    total_max_qty: state.total_max_qty === undefined || state.total_max_qty === 0 ? null : String(state.total_max_qty),
    total_max_amount:
      state.total_max_amount === undefined || state.total_max_amount === 0 ? null : String(state.total_max_amount),
    remark: state.remark.trim() || null,
    items: state.lines
      .filter((line) => line.material_id)
      .map((line) => ({
        material_id: line.material_id,
        agreed_price: String(line.agreed_price ?? 0),
        tax_rate: ratio(line.tax_rate_pct) ?? '0',
        max_qty: line.max_qty === undefined || line.max_qty === 0 ? null : String(line.max_qty),
        remark: line.remark.trim() || null,
      })),
  };
}

async function handleSave(): Promise<void> {
  const error = validateForm(form);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  saving.value = true;
  try {
    const payload = buildPayload(form);
    const detail = form.id ? await updateAgreement(form.id, payload) : await createAgreement(payload);
    MessagePlugin.success(`已保存 ${detail.agreement.agreement_no}（草稿）`);
    formVisible.value = false;
    await reloadLedger();
  } catch (err) {
    MessagePlugin.error(errorText(err, '保存框架协议失败'));
  } finally {
    saving.value = false;
  }
}

// ---------- 提交审批 / 删除 ----------

async function handleSubmit(row: AgreementExecRow): Promise<void> {
  try {
    const detail = await submitAgreement(row.agreement_id, { reason: '框架协议提交审批' });
    MessagePlugin.success(
      `${detail.agreement.agreement_no} 已提交审批（采购经理 → 财务经理；通过后协议才生效）`,
    );
    await reloadLedger();
    if (canExec.value) {
      await reloadExec();
    }
  } catch (error) {
    MessagePlugin.error(errorText(error, '提交审批失败'));
  }
}

async function handleDelete(row: AgreementExecRow): Promise<void> {
  const dialog = DialogPlugin.confirm({
    header: '删除框架协议',
    theme: 'danger',
    body: `确认删除 ${row.agreement_no}？只有草稿能删（非草稿后端会 409），删掉后不可恢复。`,
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteAgreement(row.agreement_id);
        MessagePlugin.success(`${row.agreement_no} 已删除`);
        dialog.hide();
        await reloadLedger();
      } catch (error) {
        MessagePlugin.error(errorText(error, '删除失败'));
      }
    },
  });
}

// ==================== 展示小工具（模板内联太啰嗦的两个） ====================
/** 封顶 / 剩余为空 = 不限量（模板按它加「不限」的灰字样式） */
function isUnlimited(value: unknown): boolean {
  return toNumberOrNull(value as number | string | null | undefined) === null;
}

/** 负数（超上限）—— 模板按它标红，**不截断** */
function isNegative(value: unknown): boolean {
  const parsed = toNumberOrNull(value as number | string | null | undefined);
  return parsed !== null && parsed < 0;
}

</script>

<style scoped>
.fwa-page {
  padding: 16px;
}

.fwa-alert {
  margin-bottom: 12px;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.35;
}

.cell-right {
  align-items: flex-end;
}

.cell-strong {
  font-weight: 500;
}

.cell-sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.cell-drift {
  color: var(--td-warning-color);
}

.cell-sub-inline {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  white-space: nowrap;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-variant-numeric: tabular-nums;
}

/* 不限量：灰字 + 斜体，与「0」在视觉上分开 */
.is-unlimited {
  color: var(--td-text-color-placeholder);
  font-style: italic;
}

/* 超上限（负数剩余）：染红 + 加粗，报表必须让 OVER 露头 */
.is-over {
  color: var(--td-error-color);
  font-weight: 600;
}

.rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rate-cell__text {
  flex: 0 0 auto;
  font-size: 12px;
}

.fwa-note {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
  padding-top: 8px;
}

/* 新建按钮的 gate 包裹（t-tooltip 需要一个能接受鼠标事件的元素） */
.gate-wrap {
  display: inline-block;
}

.fwa-form-hint {
  padding-top: 8px;
  line-height: 1.6;
}

.lines-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 14px 0 8px;
}

.lines-title {
  font-size: 14px;
  font-weight: 600;
}

.kpi-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
  margin-bottom: 8px;
}

.kpi-cell {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f5f6f7);
}

.kpi-label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.kpi-value {
  font-size: 18px;
  font-weight: 600;
}

.kpi-sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.kpi-foot {
  margin-bottom: 12px;
}

.fwa-desc {
  margin-bottom: 16px;
}

.section-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin: 16px 0 8px;
}

.section-title {
  font-size: 14px;
  font-weight: 600;
}
</style>
