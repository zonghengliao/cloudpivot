<!--
  薪资单详情页（ARCH-9 §6.1 / §6.2.5 · HR-43）。

  ```
  ① 单头卡：期间 / 状态 / 人数 / 应发 / 代扣 / 实发 / 计提  +  操作栏（吸顶）
  ② 汇总卡：按部门小计 + 合计行 + 明细项按类型合计 + 三口径对账
  ③ 明细表：一人一行，展开 = 该人的全部薪资项（金额 + 算法 + 说明），可改单项金额
  ④ 审批轨迹：FlowProgress（只来自 GET /business/flow/instance/{id}）
  ⑤ 凭证卡：计提 / 发放 / 红冲三张，有凭证号才给深链
  ⑥ 附件：签字工资表扫描件（DocAttachmentPanel）
  ```

  # 为什么是独立页而不是抽屉

  四段加操作栏塞不进 720px 抽屉（设计 §6.1 明写）。路由 `/business/hr/salary/:id` 是
  **页面级路由**：`sys_menu` 里没有这一行，靠列表页点单号跳进来。

  # 按钮态由状态算，页面不写 if

  `salaryActionsOf(status)` 只返回**真有后端路由**的动作（见 `salary-kinds.ts` 模块文档 ②）：
  设计 §6.2.5 的「试算 / 删除 / 作废」在 §5.2 的接口表里没有对应路由，
  状态机也没有 `CANCELLED` 这个值 —— 画出来就是点了必 404 的假按钮，所以不画
  （已在卡内上报）。「导出」不按状态拦（后端 `export_bundle` 不看状态）。

  # 三处兜底（都不报错、不弹窗）

  1. **单头与明细不一致**（`consistent=false`）：页面顶部红条 + 逐字段列出
     「单头 X / 明细 Y」—— 用户要能打开单据看哪几个字段漂了，不能整页 500；
  2. **部门汇总合计 ≠ 单头**：汇总卡下给黄条（这是界面侧的 A7 巡检，与后端三口径是两件事）；
  3. **审批进度拉不到**：降级成「审批单 FLW-xxx」链接，详情照常看（`approval-progress.ts` 口径）。

  # 改单项金额（#41）为什么放在展开行里

  HR 校对工资时说的是「这个人这笔绩效该给 1200」—— 展开行里点那一项改，比先选人再选项少两步；
  保存后**用回执里的单头**刷新合计（`SalaryOverrideResult.sheet`），不再打一次汇总
  （两次读之间别人改一笔，界面就会显示旧合计）。
-->
<template>
  <div class="hr-salary-detail" data-testid="hr-salary-detail-page">
    <!-- ==================== ① 单头卡 ==================== -->
    <t-card :bordered="false" class="head-card">
      <div class="head-row">
        <div class="head-main">
          <t-button variant="text" class="back-btn" data-testid="hr-salary-back" @click="goBack">
            <template #icon><t-icon name="chevron-left" /></template>
            返回
          </t-button>
          <span class="head-title mono" data-testid="hr-salary-sheet-no">
            {{ sheet?.sheet_no || '薪资单' }}
          </span>
          <t-tag
            size="small"
            variant="light"
            :theme="statusTheme"
            data-testid="hr-salary-status-tag"
          >
            {{ statusLabel }}
          </t-tag>
          <span class="head-period">{{ periodLabel }}</span>
          <span v-if="statusHint" class="head-hint" data-testid="hr-salary-status-hint">
            {{ statusHint }}
          </span>
        </div>
        <div class="head-side">
          <t-button size="small" variant="outline" :loading="loading" @click="loadAll">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </div>
      </div>

      <!-- 操作栏：吸顶，往下翻明细 / 审批 / 凭证时一直在（DocActionBar 的设计意图） -->
      <DocActionBar title="关键动作" testid="hr-salary-action-bar">
        <t-button
          v-for="action in actions"
          :key="action.key"
          size="small"
          :theme="action.theme"
          :title="action.title"
          :loading="pendingAction === action.key"
          :disabled="Boolean(pendingAction) && pendingAction !== action.key"
          :data-testid="`hr-salary-action-${action.key}`"
          @click="handleAction(action.key)"
        >
          {{ action.label }}
        </t-button>
        <span v-if="!actions.length" class="muted action-empty" data-testid="hr-salary-action-empty">
          当前状态（{{ statusLabel }}）没有可执行动作
        </span>
      </DocActionBar>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="head-alert"
        data-testid="hr-salary-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadAll">重试</t-button>
        </template>
      </t-alert>

      <!-- 单头指标（金额右对齐、千分位；空值给 —） -->
      <div v-if="sheet" class="metrics">
        <div class="metric">
          <span class="metric__label">薪资期间</span>
          <span class="metric__value mono">{{ periodText(sheet.period_code, sheet.period_start, sheet.period_end) }}</span>
        </div>
        <div class="metric">
          <span class="metric__label">人数</span>
          <span class="metric__value mono" data-testid="hr-salary-employee-count">
            {{ sheet.totals.employee_count }} 人
          </span>
        </div>
        <div class="metric">
          <span class="metric__label">应发合计</span>
          <span class="metric__value money" data-testid="hr-salary-gross">
            {{ moneyText(sheet.totals.gross_total) }}
          </span>
        </div>
        <div class="metric">
          <span class="metric__label">代扣合计</span>
          <span class="metric__value money">{{ moneyText(sheet.totals.withhold_total) }}</span>
        </div>
        <div class="metric metric--strong">
          <span class="metric__label">实发合计</span>
          <span class="metric__value money" data-testid="hr-salary-net">
            {{ moneyText(sheet.totals.net_total) }}
          </span>
        </div>
        <div class="metric">
          <span class="metric__label">计提合计</span>
          <span class="metric__value money" data-testid="hr-salary-accrual">
            {{ moneyText(sheet.totals.accrual_total) }}
          </span>
        </div>
        <div class="metric">
          <span class="metric__label">单位社保 / 公积金</span>
          <span class="metric__value money">
            {{ moneyText(sheet.totals.si_company_total) }} / {{ moneyText(sheet.totals.hf_company_total) }}
          </span>
        </div>
        <div class="metric">
          <span class="metric__label">计算时间</span>
          <span class="metric__value mono">{{ datetimeText(sheet.calculated_at) }}</span>
        </div>
        <div class="metric">
          <span class="metric__label">过账时间</span>
          <span class="metric__value mono" data-testid="hr-salary-posted-at">
            {{ datetimeText(sheet.posted_at) }}
          </span>
        </div>
        <div v-if="sheet.reverse_reason" class="metric metric--wide">
          <span class="metric__label">红冲原因</span>
          <span class="metric__value">{{ sheet.reverse_reason }}</span>
        </div>
        <div v-if="sheet.remark" class="metric metric--wide">
          <span class="metric__label">备注</span>
          <span class="metric__value">{{ sheet.remark }}</span>
        </div>
      </div>

      <!-- 三口径对账：不一致是**诊断信息**，红条列字段，不整页报错 -->
      <t-alert
        v-if="summary && !summary.consistent"
        theme="error"
        class="head-alert"
        data-testid="hr-salary-inconsistent"
      >
        <template #title>
          单头与明细不一致（{{ summary.mismatches.length }} 处）—— 这张单的数不能直接发
        </template>
        <ul class="mismatch-list">
          <li v-for="item in summary.mismatches" :key="item.field">
            <span class="mismatch-list__label">{{ item.label }}</span>
            <span class="mono">单头 {{ item.header }} · 明细 {{ item.detail }}</span>
          </li>
        </ul>
      </t-alert>
    </t-card>

    <!-- ==================== ② 汇总卡 ==================== -->
    <t-card :bordered="false" class="section-card summary-card">
      <div class="section-head">
        <span class="section-title">按部门汇总</span>
        <span class="section-meta" data-testid="hr-salary-dept-meta">
          {{ deptRows.length }} 个部门 · {{ deptTotals.employee_count }} 人
        </span>
      </div>

      <t-table
        row-key="dept_key"
        :data="deptTableRows"
        :columns="deptColumns"
        size="small"
        :loading="loading"
        :row-class-name="deptRowClass"
        data-testid="hr-salary-dept-table"
      >
        <template #dept_name="{ row }">
          <span v-if="row.is_total" class="dept-total-label">合计</span>
          <span v-else :class="{ muted: !row.dept_id }">{{ row.dept_name }}</span>
        </template>
        <template #employee_count="{ row }">
          <span class="mono">{{ row.employee_count }}</span>
        </template>
        <template #gross_total="{ row }">
          <span class="money">{{ moneyText(row.gross_total) }}</span>
        </template>
        <template #si_company_total="{ row }">
          <span class="money">{{ moneyText(row.si_company_total) }}</span>
        </template>
        <template #hf_company_total="{ row }">
          <span class="money">{{ moneyText(row.hf_company_total) }}</span>
        </template>
        <template #net_total="{ row }">
          <span class="money">{{ moneyText(row.net_total) }}</span>
        </template>
        <template #empty>
          <div class="panel-empty">这张单还没有明细行（引擎没算过人，或明细被清空）</div>
        </template>
      </t-table>

      <t-alert
        v-if="sheet && deptRows.length && !deptMatches"
        theme="warning"
        class="head-alert"
        data-testid="hr-salary-dept-mismatch"
      >
        部门汇总合计与单头对不上（巡检查 A7）：单头 {{ moneyText(sheet.totals.gross_total) }} /
        部门合计 {{ moneyText(deptTotals.gross_total) }} —— 明细里可能有部门为空的行走丢了，请核对。
      </t-alert>

      <div v-if="summary" class="type-totals" data-testid="hr-salary-item-totals">
        <span class="type-totals__title">明细项按类型</span>
        <span class="type-totals__item">
          Σ 收入 <b class="money">{{ moneyText(summary.item_totals.earning_total) }}</b>
        </span>
        <span class="type-totals__item">
          Σ 应发前扣减 <b class="money">{{ moneyText(summary.item_totals.pre_deduct_total) }}</b>
        </span>
        <span class="type-totals__item">
          Σ 代扣 <b class="money">{{ moneyText(summary.item_totals.withhold_total) }}</b>
        </span>
        <span class="type-totals__item">
          Σ 单位成本 <b class="money">{{ moneyText(summary.item_totals.employer_total) }}</b>
        </span>
        <span class="type-totals__item">
          明细项 {{ summary.item_totals.line_count }} 条
        </span>
      </div>
    </t-card>

    <!-- ==================== ③ 明细表（可展开） ==================== -->
    <t-card :bordered="false" class="section-card detail-card">
      <div class="section-head">
        <span class="section-title">员工明细</span>
        <span class="section-meta" data-testid="hr-salary-detail-meta">
          共 {{ detailTotal }} 人 · 点左侧箭头展开看 {{ itemHint }}
        </span>
        <div class="section-side">
          <DeptTreeSelect
            v-model="detailFilter.dept_id"
            class="filter-dept"
            placeholder="全部部门"
            data-testid="hr-salary-detail-dept"
            @update:model-value="handleDetailSearch"
          />
          <t-input
            v-model="detailFilter.keyword"
            class="filter-keyword"
            placeholder="工号 / 姓名"
            clearable
            data-testid="hr-salary-detail-keyword"
            @enter="handleDetailSearch"
            @clear="handleDetailSearch"
          >
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>
          <t-button variant="outline" size="small" data-testid="hr-salary-detail-search" @click="handleDetailSearch">
            查询
          </t-button>
          <t-button
            variant="outline"
            size="small"
            data-testid="hr-salary-detail-expand-all"
            @click="toggleExpandAll"
          >
            {{ allExpanded ? '收起全部' : '展开全部' }}
          </t-button>
        </div>
      </div>

      <t-alert
        v-if="detailError"
        theme="error"
        :message="detailError"
        class="head-alert"
        data-testid="hr-salary-detail-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="reloadDetail">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="detailRows"
        :columns="detailColumns"
        :loading="detailLoading"
        :pagination="detailPagination"
        :expand-icon="true"
        :expanded-row-keys="expandedKeys"
        hover
        data-testid="hr-salary-detail-table"
        @page-change="handlePageChange"
        @expand-change="handleExpandChange"
      >
        <template #employee="{ row }">
          <div class="emp-cell">
            <span class="emp-cell__name">{{ row.employee_name }}</span>
            <span class="emp-cell__no mono">{{ row.employee_no }}</span>
          </div>
        </template>
        <template #dept_name="{ row }">
          <span :class="{ muted: !row.dept_name }">{{ row.dept_name || '—' }}</span>
        </template>
        <template #attendance="{ row }">
          <span class="muted mono attendance-text">{{ attendanceText(row) }}</span>
        </template>
        <template #earning_amount="{ row }">
          <span class="money">{{ moneyText(row.earning_amount) }}</span>
        </template>
        <template #gross_amount="{ row }">
          <span class="money">{{ moneyText(row.gross_amount) }}</span>
        </template>
        <template #withhold_amount="{ row }">
          <span class="money">{{ moneyText(row.withhold_amount) }}</span>
        </template>
        <template #net_amount="{ row }">
          <span class="money money--strong">{{ moneyText(row.net_amount) }}</span>
        </template>
        <template #item_count="{ row }">
          <span class="mono muted" :data-testid="`hr-salary-row-items-${row.id}`">{{ itemCountText(row) }}</span>
        </template>

        <!-- 展开行 = 这个人的全部薪资项（金额 + 算法 + 引擎写的代入说明） -->
        <template #expandedRow="{ row }">
          <div class="items-wrap" :data-testid="`hr-salary-items-${row.id}`">
            <div class="items-head">
              <span class="items-head__title">{{ row.employee_name }}（{{ row.employee_no }}）的薪资项</span>
              <span class="items-head__meta">
                应发 {{ moneyText(row.gross_amount) }} · 代扣 {{ moneyText(row.withhold_amount) }} ·
                实发 {{ moneyText(row.net_amount) }}
              </span>
              <span v-if="row.remark" class="items-head__remark">备注：{{ row.remark }}</span>
            </div>

            <t-table
              v-if="row.items.length"
              row-key="item_code"
              :data="row.items"
              :columns="itemColumns"
              size="small"
              data-testid="hr-salary-items-table"
            >
              <template #item_type="{ row: item }">
                <t-tag size="small" variant="light" :theme="itemTypeTheme(item.item_type)">
                  {{ item.item_type_label || item.item_type }}
                </t-tag>
              </template>
              <template #calc_type="{ row: item }">
                <span class="muted">{{ item.calc_type_label || item.calc_type }}</span>
              </template>
              <template #amount="{ row: item }">
                <span class="money">{{ moneyText(item.amount) }}</span>
              </template>
              <template #calc_note="{ row: item }">
                <span class="calc-note mono" :title="item.expr_snapshot || ''">
                  {{ itemExplainText(item) }}
                </span>
              </template>
              <template #op="{ row: item }">
                <t-link
                  v-if="editable"
                  theme="primary"
                  :data-testid="`hr-salary-item-edit-${item.item_code}`"
                  @click="openOverride(row, item)"
                >
                  改金额
                </t-link>
                <span v-else class="muted">—</span>
              </template>
            </t-table>
            <div v-else class="panel-empty panel-empty--inline">
              这一行没有明细项（`with_items=false` 或引擎未落项）
            </div>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="hr-salary-detail-empty">
            <t-icon class="panel-empty__icon" name="user-business" />
            <p class="panel-empty__title">
              {{ isDetailFiltered(detailFilter) ? '没有匹配的员工' : '这张单没有明细' }}
            </p>
            <p class="panel-empty__desc">
              {{
                isDetailFiltered(detailFilter)
                  ? '换个部门 / 工号 / 姓名试试'
                  : '薪资单要先「试算 / 建单」才会落明细；建单在薪资列表页'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ④ 审批轨迹 ==================== -->
    <t-card :bordered="false" class="section-card">
      <div class="section-head">
        <span class="section-title">{{ APPROVAL_PROGRESS_TITLE }}</span>
        <span class="section-meta">{{ approvalMeta }}</span>
      </div>
      <FlowProgress
        v-if="hasApprovalProgress(approvalProgress)"
        :nodes="approvalProgress.nodes"
        :tasks="approvalProgress.tasks"
        data-testid="hr-salary-approval-progress"
      />
      <div
        v-else-if="approvalFallback"
        class="approval-fallback"
        data-testid="hr-salary-approval-fallback"
      >
        <span class="muted">审批进度暂时拉不到，</span>
        <t-link theme="primary" @click="openApprovalInstance(sheet?.approval_instance_id)">
          {{ approvalFallback.no }}
        </t-link>
      </div>
      <div v-else class="panel-empty panel-empty--inline" data-testid="hr-salary-approval-empty">
        未发起审批，暂无审批过程
      </div>
    </t-card>

    <!-- ==================== ⑤ 凭证卡 ==================== -->
    <t-card :bordered="false" class="section-card">
      <div class="section-head">
        <span class="section-title">凭证</span>
        <span class="section-meta">
          过账生成计提 + 发放两张；红冲再生成两张红字（登录凭证页按来源 {{ sheet?.sheet_no || '本单' }} 可反查）
        </span>
      </div>
      <div class="voucher-grid" data-testid="hr-salary-voucher-cards">
        <div
          v-for="card in voucherCards"
          :key="card.role"
          class="voucher-card"
          :class="{ 'voucher-card--empty': !card.no }"
          :data-testid="`hr-salary-voucher-${card.role}`"
        >
          <div class="voucher-card__head">
            <t-icon class="voucher-card__icon" name="file-copy" />
            <span class="voucher-card__label">{{ card.label }}</span>
            <t-tag size="small" variant="light" :theme="card.no ? 'success' : 'default'">
              {{ card.no ? '已生成' : '未生成' }}
            </t-tag>
          </div>
          <div class="voucher-card__no">
            <t-link
              v-if="card.path"
              theme="primary"
              class="mono"
              :data-testid="`hr-salary-voucher-link-${card.role}`"
              @click="openVoucher(card.path)"
            >
              {{ card.no }}
            </t-link>
            <span v-else-if="card.no" class="mono">{{ card.no }}</span>
            <span v-else class="muted">{{ card.emptyText }}</span>
          </div>
          <div class="voucher-card__amount money">{{ moneyText(card.amount) }}</div>
          <div class="voucher-card__meta muted">{{ card.metaText }}</div>
        </div>
      </div>
    </t-card>

    <!-- ==================== ⑥ 附件 ==================== -->
    <t-card :bordered="false" class="section-card">
      <div class="section-head">
        <span class="section-title">附件</span>
        <span class="section-meta">签字工资表 / 银行回单（过账前留档）</span>
      </div>
      <DocAttachmentPanel
        v-if="sheet"
        :doc-type="DOC_KIND.SALARY_SHEET"
        :doc-id="sheet.id"
        :default-role="DEFAULT_ROLE_BY_DOC.SALARY_SHEET"
        hint="建议上传签字工资表扫描件（不拦过账，但审计要）"
        testid-prefix="hr-salary-attach"
      />
    </t-card>

    <!-- ==================== 红冲弹窗 ==================== -->
    <t-dialog
      v-model:visible="reverseVisible"
      attach="body"
      header="红冲薪资单"
      width="560px"
      :confirm-btn="{ content: '确认红冲', theme: 'danger', loading: pendingAction === 'reverse' }"
      :close-on-overlay-click="false"
      data-testid="hr-salary-reverse-dialog"
      @confirm="submitReverse"
    >
      <div class="dialog-body">
        <p class="dialog-tip">
          红冲会为 <b>{{ sheet?.sheet_no }}</b> 生成<b>两张红字凭证</b>（计提 + 发放各冲回一次），
          单据进「已红冲」终态 —— 红冲不可撤销，要重发请新开一张。
        </p>
        <ul class="dialog-list">
          <li>计提凭证：{{ sheet?.accrual_voucher_no || '（本单没有计提凭证号）' }}</li>
          <li>发放凭证：{{ sheet?.payment_voucher_no || '（本单没有发放凭证号）' }}</li>
        </ul>
        <t-form label-width="90px">
          <t-form-item label="红冲原因" required-mark>
            <t-textarea
              v-model="reverseReason"
              :maxlength="MAX_REVERSE_REASON"
              :autosize="{ minRows: 3, maxRows: 6 }"
              placeholder="例：11 月绩效录入错误，需重算后重发（至少 5 个字）"
              data-testid="hr-salary-reverse-reason"
            />
            <div v-if="reverseError" class="field-error" data-testid="hr-salary-reverse-error">
              {{ reverseError }}
            </div>
          </t-form-item>
        </t-form>
      </div>
    </t-dialog>

    <!-- ==================== 改单项金额弹窗 ==================== -->
    <t-dialog
      v-model:visible="overrideVisible"
      attach="body"
      :header="overrideHeader"
      width="560px"
      :confirm-btn="{ content: '保存', theme: 'primary', loading: overriding }"
      :close-on-overlay-click="false"
      data-testid="hr-salary-override-dialog"
      @confirm="submitOverride"
    >
      <div class="dialog-body">
        <p class="dialog-tip">
          改的是<b>这一项的金额</b>，保存后后端会重算这个人的应发 / 代扣 / 实发与单头合计，
          并把「原值 → 新值 + 原因」写进状态日志。
        </p>
        <t-form label-width="90px">
          <t-form-item label="员工">
            <span data-testid="hr-salary-override-employee">
              {{ overrideTarget?.row?.employee_name }}（{{ overrideTarget?.row?.employee_no }}）
            </span>
          </t-form-item>
          <t-form-item label="薪资项">
            <span data-testid="hr-salary-override-item">
              {{ overrideTarget?.item?.item_name }}
              <span class="mono muted">{{ overrideTarget?.item?.item_code }}</span>
            </span>
          </t-form-item>
          <t-form-item label="原金额">
            <span class="money">{{ moneyText(overrideTarget?.item?.amount) }}</span>
          </t-form-item>
          <t-form-item label="新金额" required-mark>
            <t-input-number
              v-model="overrideAmount"
              class="w-full"
              theme="normal"
              :decimal-places="2"
              :step="100"
              data-testid="hr-salary-override-amount"
            />
            <div v-if="overrideAmountError" class="field-error">{{ overrideAmountError }}</div>
          </t-form-item>
          <t-form-item label="原因" required-mark>
            <t-textarea
              v-model="overrideReason"
              :maxlength="MAX_OVERRIDE_REASON"
              :autosize="{ minRows: 2, maxRows: 5 }"
              placeholder="例：客户口头确认 11 月绩效补 300（至少 5 个字）"
              data-testid="hr-salary-override-reason"
            />
            <div v-if="overrideReasonError" class="field-error" data-testid="hr-salary-override-error">
              {{ overrideReasonError }}
            </div>
          </t-form-item>
        </t-form>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 薪资单详情页（HR-43）。取数 / 词表 / 动作矩阵都在 `@/api/business/hr/salary`
 * （纯逻辑在 `salary-kinds.ts`，单测见 `tests/unit/hr-salary-detail.test.ts`），
 * 本文件只做**接线与渲染**。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';

import { ApiError } from '@/api/request';
import {
  DEPT_SUMMARY_COLUMNS,
  DETAIL_COLUMNS,
  ITEM_COLUMNS,
  MAX_OVERRIDE_REASON,
  MAX_REVERSE_REASON,
  MIN_OVERRIDE_REASON,
  MIN_REVERSE_REASON,
  SALARY_DETAIL_PAGE_SIZE,
  SALARY_PERMS,
  attendanceText,
  datetimeText,
  deptTotalsMatchHeader,
  emptyDetailFilter,
  isDetailFiltered,
  isSalaryDetailEditable,
  itemCountText,
  itemExplainText,
  itemTypeTheme,
  moneyText,
  periodText,
  reasonError,
  salaryActionsOf,
  salaryStatusHint,
  salaryStatusLabel,
  salaryStatusTheme,
  sumDeptTotals,
  voucherCardsOf,
  type SalaryActionKey,
  type SalaryDetailFilter,
  type SalaryDetailRow,
  type SalaryItemLine,
  type SalarySheetSummary,
  type SalaryVoucherCard,
} from '@/api/business/hr/salary';
import {
  downloadSalaryExport,
  fetchSalaryDetailPage,
  fetchSalarySheetSummary,
  postSalarySheet,
  reverseSalarySheet,
  salaryExportFileName,
  submitSalarySheet,
  updateSalaryDetailItem,
} from '@/api/business/hr/salary';
import { getFlowInstance } from '@/api/business/flow-api';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { useUserStore } from '@/stores/user';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import DocActionBar from '@/views/business/erp/shared/DocActionBar.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import {
  APPROVAL_PROGRESS_TITLE,
  EMPTY_APPROVAL_PROGRESS,
  approvalFallbackOf,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';

const route = useRoute();
const router = useRouter();
const userStore = useUserStore();

/** 单 id（路由参数；空串 = 路由配错，页面显示空态而不是打 undefined 的请求） */
const sheetId = computed(() => String(route.params.id ?? '').trim());

// ==================== 状态 ====================

const summary = ref<SalarySheetSummary | null>(null);
const loading = ref(false);
const loadError = ref('');

/** 三段表格的列（表头字典集中在 `salary-kinds.ts`，页面不手抄） */
const deptColumns = DEPT_SUMMARY_COLUMNS;
const detailColumns = DETAIL_COLUMNS;
const itemColumns = ITEM_COLUMNS;

const detailRows = ref<SalaryDetailRow[]>([]);
const detailTotal = ref(0);
const detailPage = ref(1);
const detailPageSize = ref(SALARY_DETAIL_PAGE_SIZE);
const detailLoading = ref(false);
const detailError = ref('');
const detailFilter = reactive<SalaryDetailFilter>(emptyDetailFilter());
const expandedKeys = ref<string[]>([]);

/** 审批进度（拉不到就是空 = 降级成单号链接，不报错） */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

/** 正在跑的动作（同一个时刻只允许一个，避免连点两次过账） */
const pendingAction = ref<SalaryActionKey | ''>('');

// ==================== 派生 ====================

const sheet = computed(() => summary.value?.sheet ?? null);
const status = computed(() => (sheet.value?.status ?? '').trim());
const statusLabel = computed(() => sheet.value?.status_label || salaryStatusLabel(status.value));
const statusTheme = computed(() => salaryStatusTheme(status.value));
const statusHint = computed(() => salaryStatusHint(status.value));

const periodLabel = computed(() =>
  sheet.value
    ? periodText(sheet.value.period_code, sheet.value.period_start, sheet.value.period_end)
    : '',
);

/** 当前状态能点的动作（再按权限过滤；没权限的按钮一个都不出现） */
const actions = computed(() =>
  salaryActionsOf(status.value).filter((action) => userStore.hasPermission(action.permission)),
);

/** 明细项能不能改：状态（后端 `ensure_writable`）+ `salary:calc` 权限 */
const editable = computed(
  () => isSalaryDetailEditable(status.value) && userStore.hasPermission(SALARY_PERMS.calc),
);

const deptRows = computed(() =>
  (summary.value?.dept_summary ?? []).map((row) => ({
    ...row,
    /** 表格 row-key：部门为空的行也要唯一（`(未分配部门)` 合并成一行） */
    dept_key: row.dept_id ?? '__no_dept__',
  })),
);

const deptTotals = computed(() => sumDeptTotals(summary.value?.dept_summary));
const deptMatches = computed(() => deptTotalsMatchHeader(deptTotals.value, sheet.value?.totals));

/**
 * 部门表的数据 = 部门行 + **合计行**。
 *
 * 合计为什么放进表格而不是在表下另画一行：另画的 div 列宽得手抄一份，表头一旦自适应
 * （1440 屏会把固定列宽撑开）合计的「应发合计」就永远偏在「人数」下面 —— 实测第一版
 * 就是这样，差半列。放进表格由 t-table 自己算列宽，天然对齐。
 */
const deptTableRows = computed(() => {
  const rows = deptRows.value;
  if (!rows.length) {
    return rows;
  }
  const totals = deptTotals.value;
  return [
    ...rows,
    {
      dept_key: '__total__',
      dept_id: null,
      dept_name: '合计',
      is_total: true,
      employee_count: totals.employee_count,
      earning_total: totals.earning_total,
      pre_deduct_total: totals.pre_deduct_total,
      gross_total: totals.gross_total,
      withhold_total: totals.withhold_total,
      net_total: totals.net_total,
      si_company_total: totals.si_company_total,
      hf_company_total: totals.hf_company_total,
      accrual_total: totals.accrual_total,
    },
  ];
});

/** 合计行的高亮类（`row-class-name`） */
function deptRowClass(params: { row: { dept_key?: string } }): string {
  return params.row?.dept_key === '__total__' ? 'dept-total-row' : '';
}

const detailPagination = computed(() => ({
  current: detailPage.value,
  pageSize: detailPageSize.value,
  total: detailTotal.value,
  showJumper: true,
  showPageSize: false,
}));

const itemHint = computed(() => {
  const first = detailRows.value[0];
  return first ? `${Math.max(0, Number(first.item_count) || 0)} 个薪资项` : '薪资项';
});

const allExpanded = computed(
  () => detailRows.value.length > 0 && expandedKeys.value.length >= detailRows.value.length,
);

const approvalFallback = computed(() =>
  approvalFallbackOf({
    status: status.value,
    approval_instance_id: sheet.value?.approval_instance_id ?? null,
  }),
);

/** 审批卡右侧的一句口径说明（没有实例就不说话） */
const approvalMeta = computed(() => {
  if (!sheet.value?.approval_instance_id) {
    return '未发起审批';
  }
  return status.value === 'SUBMITTED' ? '审批中（明细已冻结）' : '已审批完成或已撤回';
});

const voucherCards = computed<SalaryVoucherCard[]>(() => voucherCardsOf(sheet.value));

// ==================== 取数 ====================

/** 错误 → 文案（后端 `msg` 已经是中文人话，直接显示） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 单头汇总（含部门汇总 / 三口径对账）*/
async function loadSummary(): Promise<void> {
  if (!sheetId.value) {
    loadError.value = '路由里没有薪资单 id';
    return;
  }
  loading.value = true;
  loadError.value = '';
  try {
    summary.value = await fetchSalarySheetSummary(sheetId.value);
  } catch (error) {
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 明细分页（`with_items=true`：展开行直接读回执，不再一人一请求） */
async function loadDetail(page = detailPage.value): Promise<void> {
  if (!sheetId.value) {
    return;
  }
  detailLoading.value = true;
  detailError.value = '';
  try {
    const data = await fetchSalaryDetailPage(
      sheetId.value,
      page,
      detailPageSize.value,
      detailFilter,
    );
    detailRows.value = data.list ?? [];
    detailTotal.value = Number(data.total) || 0;
    detailPage.value = Number(data.page) || page;
    detailPageSize.value = Number(data.page_size) || detailPageSize.value;
    // 翻页 / 筛选后旧的展开键指向已不在列表里的行 —— 清掉，避免表格显示空展开块
    expandedKeys.value = expandedKeys.value.filter((key) =>
      detailRows.value.some((row) => row.id === key),
    );
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

/** 表格上的「重试」按钮（`@click` 会塞一个 MouseEvent，不能直接绑 `loadDetail()`） */
function reloadDetail(): void {
  void loadDetail();
}

/** 审批进度（永不抛：拉不到就是空，页面降级） */
async function loadApproval(): Promise<void> {
  approvalProgress.value = await loadApprovalProgress(
    sheet.value?.approval_instance_id ?? null,
    getFlowInstance,
  );
}

/** 整页刷新（单头 → 明细 → 审批，顺序不能反：审批要读单头上的实例 id） */
async function loadAll(): Promise<void> {
  await loadSummary();
  await Promise.all([loadDetail(1), loadApproval()]);
}

// ==================== 交互 ====================

function goBack(): void {
  // 详情页是列表页点进来的：优先回上一页。直接开链接（无历史）时回工作台 ——
  // 薪资**列表页**目前还没有前端卡片（已上报），所以这里不指向一个 404 的路径。
  if (window.history.length > 1) {
    router.back();
    return;
  }
  void router.push({ path: '/' });
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  detailPage.value = pageInfo.current;
  if (pageInfo.pageSize && pageInfo.pageSize !== detailPageSize.value) {
    detailPageSize.value = pageInfo.pageSize;
  }
  void loadDetail(pageInfo.current);
}

function handleDetailSearch(): void {
  expandedKeys.value = [];
  void loadDetail(1);
}

function handleExpandChange(keys: Array<string | number>): void {
  expandedKeys.value = keys.map((key) => String(key));
}

function toggleExpandAll(): void {
  expandedKeys.value = allExpanded.value ? [] : detailRows.value.map((row) => row.id);
}

function openVoucher(path: string): void {
  void router.push(path);
}

function openApprovalInstance(instanceId: string | null | undefined): void {
  const id = (instanceId ?? '').trim();
  if (id) {
    void router.push({ path: `/flow/instance/${id}` });
  }
}

// ==================== 动作（提交 / 过账 / 红冲 / 导出） ====================

function handleAction(key: SalaryActionKey): void {
  switch (key) {
    case 'submit':
      confirmSubmit();
      return;
    case 'post':
      confirmPost();
      return;
    case 'reverse':
      openReverse();
      return;
    case 'export':
      void doExport();
      return;
    default:
      return;
  }
}

/** 提交审批：先确认（提交后明细冻结，撤回要去审批中心） */
function confirmSubmit(): void {
  const target = sheet.value;
  if (!target) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '提交审批',
    body: `提交后 ${target.sheet_no} 的明细会冻结，审批中谁都不能改数；撤回 / 驳回请到审批中心。`,
    confirmBtn: '提交',
    onConfirm: () => {
      dialog.hide();
      void doSubmit();
    },
    onClose: () => dialog.hide(),
  });
}

async function doSubmit(): Promise<void> {
  if (!sheet.value || pendingAction.value) {
    return;
  }
  pendingAction.value = 'submit';
  try {
    const card = await submitSalarySheet(sheet.value.id);
    MessagePlugin.success(`${card.sheet_no} 已提交审批（${card.status_label}）`);
    await loadAll();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    pendingAction.value = '';
  }
}

/** 过账：确认框里写清「会生成哪两张凭证」—— 这是财务最怕点错的一步 */
function confirmPost(): void {
  const target = sheet.value;
  if (!target) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '过账',
    body:
      `过账会为 ${target.sheet_no} 生成计提 + 发放两张凭证并进账：` +
      `计提 ${moneyText(target.totals.accrual_total)}（含单位社保 / 公积金）、` +
      `发放 ${moneyText(target.totals.net_total)}（实发）。过账后要改只能红冲。`,
    confirmBtn: '过账',
    onConfirm: () => {
      dialog.hide();
      void doPost();
    },
    onClose: () => dialog.hide(),
  });
}

async function doPost(): Promise<void> {
  if (!sheet.value || pendingAction.value) {
    return;
  }
  pendingAction.value = 'post';
  try {
    const card = await postSalarySheet(sheet.value.id);
    MessagePlugin.success(
      `${card.sheet_no} 已过账（${card.accrual_voucher_no || '计提凭证已生成'} / ${card.payment_voucher_no || '发放凭证已生成'}）`,
    );
    await loadAll();
  } catch (error) {
    // 过账失败最常见的是「期间未开 / 考勤未锁定」——后端报文已点名，直接弹
    MessagePlugin.error(messageOf(error));
  } finally {
    pendingAction.value = '';
  }
}

// ==================== 红冲 ====================

const reverseVisible = ref(false);
const reverseReason = ref('');
const reverseError = ref('');

function openReverse(): void {
  reverseReason.value = '';
  reverseError.value = '';
  reverseVisible.value = true;
}

async function submitReverse(): Promise<void> {
  if (!sheet.value || pendingAction.value) {
    return;
  }
  const invalid = reasonError(reverseReason.value, {
    min: MIN_REVERSE_REASON,
    max: MAX_REVERSE_REASON,
    label: '红冲原因',
  });
  if (invalid) {
    reverseError.value = invalid;
    return;
  }
  reverseError.value = '';
  pendingAction.value = 'reverse';
  try {
    const card = await reverseSalarySheet(sheet.value.id, reverseReason.value.trim());
    MessagePlugin.success(`${card.sheet_no} 已红冲（两张红字凭证已生成）`);
    reverseVisible.value = false;
    await loadAll();
  } catch (error) {
    reverseError.value = messageOf(error);
  } finally {
    pendingAction.value = '';
  }
}

// ==================== 改单项金额（#41） ====================

const overrideVisible = ref(false);
const overriding = ref(false);
const overrideTarget = ref<{ row: SalaryDetailRow; item: SalaryItemLine } | null>(null);
const overrideAmount = ref<number>(0);
const overrideReason = ref('');
const overrideReasonError = ref('');
const overrideAmountError = ref('');

const overrideHeader = computed(() => {
  const target = overrideTarget.value;
  return target ? `改金额：${target.item.item_name}` : '改金额';
});

function openOverride(row: SalaryDetailRow, item: SalaryItemLine): void {
  overrideTarget.value = { row, item };
  overrideAmount.value = Number(item.amount) || 0;
  overrideReason.value = '';
  overrideReasonError.value = '';
  overrideAmountError.value = '';
  overrideVisible.value = true;
}

async function submitOverride(): Promise<void> {
  const target = overrideTarget.value;
  if (!target || overriding.value) {
    return;
  }
  const amount = Number(overrideAmount.value);
  overrideAmountError.value = Number.isFinite(amount) ? '' : '新金额必须是数字';
  const reasonInvalid = reasonError(overrideReason.value, {
    min: MIN_OVERRIDE_REASON,
    max: MAX_OVERRIDE_REASON,
    label: '原因',
  });
  overrideReasonError.value = reasonInvalid ?? '';
  if (overrideAmountError.value || reasonInvalid) {
    return;
  }
  overriding.value = true;
  try {
    const result = await updateSalaryDetailItem(target.row.sheet_id, target.row.id, {
      reason: overrideReason.value.trim(),
      // 金额传字符串：`toFixed(2)` 保证不把 JS 浮点尾巴（1200.0000000000002）带进 Decimal
      items: [{ item_code: target.item.item_code, amount: amount.toFixed(2) }],
    });
    if (result.changed) {
      const change = result.changes[0];
      MessagePlugin.success(
        change
          ? `${result.employee_name} 的 ${change.item_name}：${moneyText(change.before)} → ${moneyText(change.after)}`
          : `${result.employee_name} 的金额已改`,
      );
    } else {
      MessagePlugin.info('金额与库里一致，没有改动（也没有留痕）');
    }
    overrideVisible.value = false;
    // 用回执刷新：单头（合计变了）+ 该明细行（项金额 / item_count 变了），不再打一次汇总
    if (summary.value) {
      summary.value = { ...summary.value, sheet: result.sheet };
    }
    detailRows.value = detailRows.value.map((row) =>
      row.id === result.detail.id ? result.detail : row,
    );
  } catch (error) {
    overrideReasonError.value = messageOf(error);
  } finally {
    overriding.value = false;
  }
}

// ==================== 导出 ====================

async function doExport(): Promise<void> {
  const target = sheet.value;
  if (!target || pendingAction.value) {
    return;
  }
  pendingAction.value = 'export';
  try {
    const blob = await downloadSalaryExport(target.id);
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = salaryExportFileName(target.period_code, target.sheet_no);
    anchor.click();
    URL.revokeObjectURL(url);
    MessagePlugin.success(`已导出 ${salaryExportFileName(target.period_code, target.sheet_no)}`);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    pendingAction.value = '';
  }
}

// ==================== 生命周期 ====================

onMounted(() => {
  void loadAll();
});

// 同一个页面组件被复用到另一个 id（路由参数变化）时必须重查，否则会显示上一张单的数
watch(sheetId, (next, prev) => {
  if (next && next !== prev) {
    expandedKeys.value = [];
    detailPage.value = 1;
    void loadAll();
  }
});
</script>

<style scoped>
.hr-salary-detail {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 12px;
}

/* ==================== 单头卡 ==================== */

.head-card,
.section-card {
  border-radius: 8px;
}

.head-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.head-main {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-right: auto;
}

.back-btn {
  margin-left: -8px;
}

.head-title {
  font-size: 16px;
  font-weight: 600;
}

.head-period {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 13px;
}

.head-hint {
  flex: 1 1 100%;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
  line-height: 18px;
}

.metrics {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
  gap: 12px;
  margin-top: 12px;
}

.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 12px;
  background: var(--td-bg-color-container-hover, #f5f5f5);
  border-radius: 6px;
}

.metric--strong {
  background: var(--td-brand-color-light, #e3f2fd);
}

.metric--wide {
  grid-column: 1 / -1;
}

.metric__label {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.metric__value {
  font-size: 14px;
  font-weight: 500;
}

.head-alert {
  margin-top: 12px;
}

.action-empty {
  font-size: 12px;
}

.mismatch-list {
  margin: 4px 0 0;
  padding-left: 18px;
  font-size: 12px;
}

.mismatch-list__label {
  display: inline-block;
  min-width: 96px;
  font-weight: 600;
}

/* ==================== 区块通用 ==================== */

.section-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.section-title {
  font-size: 14px;
  font-weight: 600;
}

.section-title::before {
  content: '';
  display: inline-block;
  width: 3px;
  height: 14px;
  margin-right: 6px;
  vertical-align: -2px;
  background: var(--td-brand-color, #0052d9);
  border-radius: 2px;
}

.section-meta {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.section-side {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}

.filter-dept {
  width: 180px;
}

.filter-keyword {
  width: 180px;
}

/* ==================== 部门汇总 ==================== */

.dept-total-label {
  font-weight: 600;
}

:deep(.dept-total-row) {
  font-weight: 600;
  background: var(--td-bg-color-container-hover, #f5f5f5);
}

.type-totals {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 16px;
  margin-top: 12px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.type-totals__title {
  font-weight: 600;
  color: var(--td-text-color-primary, #000000e6);
}

/* ==================== 明细 ==================== */

.emp-cell {
  display: flex;
  flex-direction: column;
  line-height: 18px;
}

.emp-cell__name {
  font-weight: 500;
}

.emp-cell__no {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.attendance-text {
  font-size: 12px;
}

.items-wrap {
  padding: 8px 12px 12px;
  background: var(--td-bg-color-container-hover, #f5f5f5);
  border-radius: 6px;
}

.items-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 8px;
}

.items-head__title {
  font-weight: 600;
  font-size: 13px;
}

.items-head__meta,
.items-head__remark {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.calc-note {
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.panel-empty {
  padding: 32px 12px;
  text-align: center;
  color: var(--td-text-color-secondary, #00000099);
}

.panel-empty--inline {
  padding: 16px 12px;
  font-size: 13px;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder, #0000004d);
}

.panel-empty__title {
  margin: 8px 0 4px;
  font-size: 14px;
  color: var(--td-text-color-primary, #000000e6);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
}

/* ==================== 审批 ==================== */

.approval-fallback {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
}

/* ==================== 凭证卡 ==================== */

.voucher-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 12px;
}

.voucher-card {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  border-radius: 8px;
}

.voucher-card--empty {
  border-style: dashed;
  background: var(--td-bg-color-container-hover, #fafafa);
}

.voucher-card__head {
  display: flex;
  align-items: center;
  gap: 6px;
}

.voucher-card__icon {
  color: var(--td-brand-color, #0052d9);
}

.voucher-card__label {
  font-weight: 600;
  margin-right: auto;
}

.voucher-card__no {
  font-size: 13px;
  min-height: 20px;
}

.voucher-card__amount {
  font-size: 18px;
  font-weight: 600;
}

.voucher-card__meta {
  font-size: 12px;
  line-height: 18px;
}

/* ==================== 弹窗 ==================== */

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.dialog-tip {
  margin: 0;
  font-size: 13px;
  line-height: 20px;
}

.dialog-list {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.field-error {
  margin-top: 4px;
  color: var(--td-error-color, #d54941);
  font-size: 12px;
}

.w-full {
  width: 100%;
}

/* ==================== 共用原子 ==================== */

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.money {
  font-variant-numeric: tabular-nums;
}

.money--strong {
  font-weight: 600;
}

.muted {
  color: var(--td-text-color-placeholder, #0000004d);
}
</style>
