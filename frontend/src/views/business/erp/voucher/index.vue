<template>
  <div class="voucher-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-voucher-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
        <t-button theme="primary" data-testid="btn-voucher-create" @click="openCreate">
          <template #icon><t-icon name="add" /></template>
          新增凭证
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.period"
          class="tool-row__period"
          placeholder="期间 yyyy-MM"
          clearable
          data-testid="voucher-filter-period"
          @enter="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="voucher-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.voucher_type"
          class="tool-row__type"
          :options="typeFilterOptions"
          placeholder="全部来源类型"
          clearable
          data-testid="voucher-filter-type"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.voucher_no"
          class="tool-row__no"
          placeholder="凭证号（模糊）"
          clearable
          data-testid="voucher-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-voucher-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-voucher-reset" @click="handleReset">重置</t-button>
      </div>
      <!-- O4 列表记忆：凭证按「期间 + 类型 + 状态」反复查，恢复后必须让用户看得见 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="voucher-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">记账凭证</span>
          <span class="panel-head__meta" data-testid="voucher-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">凭证由过账自动生成；点行看分录与借贷平衡</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="voucher-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadVouchers()">重试</t-button>
        </template>
      </t-alert>

      <!-- D7：批量操作工具栏（勾选 → 进度 → 逐条结果） -->
      <BatchActionBar
        :selected="selectedRows"
        :actions="BATCH_ACTIONS"
        :filtered-total="total"
        :runner="runBatchAction"
        :can="canBatchAction"
      :fetch-all-ids="fetchAllRowIds"
        testid="voucher-batch"
        @update:selected="onSelectedRows"
        @done="handleBatchDone"
      />

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        :selected-row-keys="selectedIds"
        data-testid="voucher-table"
        @select-change="onSelectChange"
        @page-change="handlePageChange"
        @row-click="handleRowClick"
      >
        <template #voucher_no="{ row }">
          <div class="no-cell">
            <span class="mono" :data-testid="`voucher-no-${row.voucher_no}`">
              {{ row.voucher_word }} {{ row.voucher_no }}
            </span>
            <!-- 红字凭证：借贷互换 + is_red=1，金额仍是正数（库 CHECK 禁负数），必须标出来 -->
            <t-tag v-if="row.is_red" size="small" variant="light" theme="danger">红字</t-tag>
            <!-- ★ P7：红字凭证要说清它冲的是哪张（否则列表里两张同额凭证谁也分不清） -->
            <span
              v-if="row.is_red && row.reverse_of_no"
              class="linked muted"
              :data-testid="`voucher-reverse-of-${row.voucher_no}`"
            >
              红冲 {{ row.reverse_of_no }}
            </span>
            <!-- ★ P7：原凭证被红冲过 → 指向红字凭证 -->
            <span
              v-if="!row.is_red && row.reversed_by_no"
              class="linked muted"
              :data-testid="`voucher-reversed-by-${row.voucher_no}`"
            >
              已红冲 → {{ row.reversed_by_no }}
            </span>
            <!-- ★ P-FLOW-3B：红冲审批中（凭证仍「已记账」，账一分没动） -->
            <t-tag
              v-if="isPendingReverse(row)"
              size="small"
              variant="light"
              theme="warning"
              :data-testid="`voucher-pending-reverse-${row.voucher_no}`"
            >
              {{ VOUCHER_PENDING_REVERSE_LABEL }}
            </t-tag>
          </div>
        </template>

        <template #voucher_date="{ row }">
          <span class="muted">{{ shortDate(row.voucher_date) }}</span>
        </template>

        <template #period="{ row }">
          <span class="mono muted">{{ row.period }}</span>
        </template>

        <template #summary="{ row }">
          <span :data-testid="`voucher-summary-${row.voucher_no}`">{{ dash(row.summary) }}</span>
        </template>

        <template #source_type="{ row }">
          <span class="muted">{{ sourceTypeLabel(row.source_type) }}</span>
        </template>

        <template #entry_count="{ row }">
          <span class="muted">{{ row.entry_count }} 行</span>
        </template>

        <template #total_debit="{ row }">
          <span class="money" :data-testid="`voucher-debit-${row.voucher_no}`">
            {{ formatMoney(row.total_debit) }}
          </span>
        </template>

        <template #total_credit="{ row }">
          <span class="money" :data-testid="`voucher-credit-${row.voucher_no}`">
            {{ formatMoney(row.total_credit) }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            :theme="voucherStatusTheme(row.status)"
            :data-testid="`voucher-status-${row.voucher_no}`"
          >
            {{ voucherStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-voucher-view-${row.voucher_no}`" @click.stop="openDetail(row)">
              查看
            </t-link>
            <!--
              ★ P0-FIX-4B：审核链三动作（DRAFT→AUDITED→POSTED）。
              按钮清单由 `voucher-audit-logic.ts::voucherActionButtons` 给：状态先筛一道
              （草稿只有「审核」、已审核才有「反审核 / 记账」、已记账一个都没有），
              再叠权限 + 自审。置灰的用 <span> 兜住 title —— TDesign 的 disabled 链接
              不派发鼠标事件，tooltip 永远不出现（见 SKILL「t-tooltip 包 disabled」）。
            -->
            <template v-for="btn in auditButtons(row)" :key="btn.key">
              <span
                v-if="btn.disabled"
                class="op-dead"
                :title="btn.reason"
                :data-testid="`btn-voucher-${btn.key}-${row.voucher_no}`"
              >
                {{ btn.label }}
              </span>
              <t-link
                v-else
                :theme="btn.theme"
                :data-testid="`btn-voucher-${btn.key}-${row.voucher_no}`"
                @click.stop="onAuditAction(btn, row)"
              >
                {{ btn.label }}
              </t-link>
            </template>
            <t-link
              v-if="canReverse(row)"
              theme="danger"
              :data-testid="`btn-voucher-reverse-${row.voucher_no}`"
              @click.stop="openReverse(row)"
            >
              红冲
            </t-link>
            <!-- ★ P-FLOW-3B：红冲审批中 → 「红冲」消失，换「撤回红冲」（撤回后凭证仍是「已记账」） -->
            <t-link
              v-if="canWithdrawReverse(row)"
              theme="warning"
              :data-testid="`btn-voucher-withdraw-${row.voucher_no}`"
              @click.stop="openWithdrawReverse(row)"
            >
              撤回红冲
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="voucher-empty">
            <t-icon name="bill" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的凭证' : '暂无凭证' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个期间 / 状态 / 凭证号再试' : '收货单过账后会自动生成凭证（同一事务）' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 详情抽屉（分录表 + ★ 借贷平衡） ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="760px"
      :footer="false"
      :header="false"
      data-testid="voucher-drawer"
    >
      <div v-if="detail" class="drawer" data-testid="voucher-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono" data-testid="voucher-detail-title">
              {{ voucherTitleOf(detail) }}
            </span>
            <t-tag size="small" :theme="voucherStatusTheme(detail.status)" data-testid="voucher-detail-status">
              {{ voucherStatusLabel(detail.status) }}
            </t-tag>
            <t-tag v-if="detail.is_red" size="small" variant="light" theme="danger">红字凭证</t-tag>
            <!-- ★ P-FLOW-3B：红冲审批中（与状态标并列，不顶替「已记账」） -->
            <t-tag
              v-if="isPendingReverse(detail)"
              size="small"
              variant="light"
              theme="warning"
              data-testid="voucher-detail-pending-reverse"
            >
              {{ VOUCHER_PENDING_REVERSE_LABEL }}
            </t-tag>
            <t-tag size="small" variant="outline" theme="default">
              来源：{{ sourceTypeLabel(detail.source_type) }}
            </t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button
              v-if="canWithdrawReverse(detail)"
              size="small"
              variant="outline"
              theme="warning"
              data-testid="btn-voucher-drawer-withdraw"
              @click="openWithdrawReverse(detail)"
            >
              撤回红冲审批
            </t-button>
            <t-button size="small" variant="outline" data-testid="btn-voucher-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <!--
          ★ P0-FIX-4B：审核链动作条（详情里直接操作，不必回列表点）。
          状态先筛（草稿只有「审核」/ 已审核才有「反审核 · 记账」/ 已记账没有），
          没权限或自审的置灰并在右侧写出原因 —— 「为什么点不了」比「按钮凭空消失」诚实。
        -->
        <div class="audit-bar" data-testid="voucher-detail-audit-actions">
          <t-button
            v-for="btn in detailAuditButtons"
            :key="btn.key"
            size="small"
            :theme="btn.theme"
            :variant="btn.disabled ? 'outline' : 'base'"
            :disabled="btn.disabled"
            :data-testid="`btn-voucher-detail-${btn.key}`"
            @click="onAuditAction(btn, detail)"
          >
            {{ btn.label }}
          </t-button>
          <span v-if="auditBlockedReasons.length === 0 && detailAuditButtons.length === 0" class="audit-bar__why">
            没有可执行的审核动作
          </span>
          <span v-else-if="auditBlockedReasons.length > 0" class="audit-bar__why">
            {{ auditBlockedReasons.join('；') }}
          </span>
          <span class="audit-bar__spacer" />
          <span class="audit-bar__note" data-testid="voucher-audit-note">{{ voucherAuditNote(detail.status) }}</span>
        </div>

        <t-alert
          v-if="auditError"
          theme="error"
          :message="auditError"
          data-testid="voucher-audit-error"
          class="audit-alert"
        />

        <!-- ★ P-FLOW-3B：红冲审批中 → 说清「账没动」+ 撤回入口 -->
        <t-alert
          v-if="isPendingReverse(detail)"
          theme="warning"
          message="红冲审批中：凭证仍是「已记账」，账一分没动"
          :description="pendingReverseDescription(detail)"
          data-testid="voucher-pending-reverse-alert"
        >
          <template #operation>
            <t-button
              v-if="canWithdrawReverse(detail)"
              size="small"
              variant="outline"
              theme="warning"
              data-testid="btn-voucher-drawer-withdraw-inline"
              @click="openWithdrawReverse(detail)"
            >
              撤回红冲审批
            </t-button>
          </template>
        </t-alert>

        <t-loading :loading="detailLoading" size="small">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">凭证日期</span>
              <span class="field__value">{{ dash(detail.voucher_date) }}</span>
            </div>
            <div class="field">
              <span class="field__label">会计期间</span>
              <span class="field__value mono">{{ dash(detail.period) }}</span>
            </div>
            <div class="field">
              <span class="field__label">摘要</span>
              <span class="field__value">{{ dash(detail.summary) }}</span>
            </div>
            <div class="field">
              <span class="field__label">来源单据</span>
              <span class="field__value mono">{{ dash(detail.source_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">记账时间</span>
              <span class="field__value">{{ dash(detail.posted_at) }}</span>
            </div>
            <div class="field">
              <span class="field__label">分录行数</span>
              <span class="field__value">{{ detail.entries.length }} 行</span>
            </div>
            <!-- ★ P7：红字凭证 / 被红冲的反向指针 -->
            <div v-if="detail.is_red" class="field">
              <span class="field__label">红冲的原凭证</span>
              <span class="field__value mono" data-testid="voucher-detail-reverse-of">
                {{ dash(detail.reverse_of_no) }}
              </span>
            </div>
            <div v-if="detail.reversed_by_no" class="field">
              <span class="field__label">已被红冲（红字凭证）</span>
              <t-link
                theme="primary"
                class="mono"
                data-testid="voucher-detail-reversed-by"
                @click="openById(detail.reversed_by)"
              >
                {{ detail.reversed_by_no }}
              </t-link>
            </div>
          </div>

          <t-table
            class="entry-table"
            row-key="id"
            :data="detail.entries"
            :columns="entryColumns"
            size="small"
            bordered
            data-testid="voucher-entry-table"
          >
            <template #line_no="{ row }">
              <span class="mono">{{ row.line_no }}</span>
            </template>
            <template #summary="{ row }">{{ dash(row.summary) }}</template>
            <template #account="{ row }">
              <span class="account-cell" :data-testid="`voucher-entry-account-${row.line_no}`">
                <span class="mono">{{ row.account_code }}</span>
                <span>{{ row.account_name }}</span>
              </span>
            </template>
            <template #debit_amount="{ row }">
              <span class="money" :data-testid="`voucher-entry-debit-${row.line_no}`">
                {{ toCents(row.debit_amount) === 0 ? '' : formatMoney(row.debit_amount) }}
              </span>
            </template>
            <template #credit_amount="{ row }">
              <span class="money" :data-testid="`voucher-entry-credit-${row.line_no}`">
                {{ toCents(row.credit_amount) === 0 ? '' : formatMoney(row.credit_amount) }}
              </span>
            </template>
          </t-table>

          <!-- ★★ 借贷平衡可视化：合计行 + 绿勾 / 红字警告 ★★ -->
          <div class="balance" :class="balanceClass" data-testid="voucher-balance">
            <div class="balance__label">合计</div>
            <div class="balance__debit money" data-testid="voucher-balance-debit">
              {{ formatCents(balance.debitCents) }}
            </div>
            <div class="balance__credit money" data-testid="voucher-balance-credit">
              {{ formatCents(balance.creditCents) }}
            </div>
            <div class="balance__verdict" :class="balanceClass">
              <t-icon :name="balanceIcon(balance.balanced)" />
              <span data-testid="voucher-balance-text">
                {{ balanceText(balance.balanced, balance.diffCents) }}
              </span>
            </div>
          </div>

          <!-- 响应自相矛盾（前端逐行加 ≠ 后端合计）时单独报警：这比账不平更该查 -->
          <t-alert
            v-if="!balance.consistent"
            theme="warning"
            message="响应自相矛盾：分录逐行合计与后端合计不一致"
            :description="balance.mismatch"
            class="balance-alert"
            data-testid="voucher-balance-mismatch"
          />

          <!--
            ★ C2：审批进度（凭证只在「红冲审批中」时挂实例 `pending_reverse_instance_id`）。
            与采购 / 收货 / 应付 / 调拨同一个 `FlowProgress` + 同一个装载器：
            节点进度 + 审批人明细（谁 / 什么时间 / 意见或跳过说明）都来自
            `GET /business/flow/instance/{id}`，前端不自己推「走到第几步」。
          -->
          <template v-if="hasApprovalProgress(approvalProgress)">
            <div class="section-title">{{ APPROVAL_PROGRESS_TITLE }}</div>
            <FlowProgress
              :nodes="approvalProgress.nodes"
              :tasks="approvalProgress.tasks"
              data-testid="voucher-approval-progress"
            />
          </template>
        </t-loading>
      </div>
    </t-drawer>

    <!--
      ==================== ④ 反审核弹窗（原因必填） ====================
      AUDITED → DRAFT，PRD E4-09。原因写进凭证状态日志，所以**必填**：
      确认按钮在理由为空时置灰，且点确认再校验一次（两条路都堵，单测覆盖校验函数）。
    -->
    <t-dialog
      v-model:visible="unAuditVisible"
      header="反审核（打回草稿）"
      theme="warning"
      width="520px"
      :confirm-btn="{
        content: '确认反审核',
        theme: 'warning',
        loading: auditSubmitting,
        disabled: !unAuditReasonValid,
      }"
      :close-on-overlay-click="!auditSubmitting"
      data-testid="voucher-unaudit-dialog"
      @confirm="submitUnAudit"
    >
      <div v-if="unAuditTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ unAuditTarget.voucher_no }}</span>
          <span class="muted">
            {{ voucherStatusLabel(unAuditTarget.status) }} → 草稿
          </span>
        </div>
        <t-form label-align="top">
          <t-form-item label="反审核原因" required-mark>
            <t-textarea
              v-model="unAuditForm.reason"
              :maxlength="200"
              :autosize="{ minRows: 3, maxRows: 5 }"
              placeholder="例如：科目选错，需重做 / 金额录错，已与原始单据核对"
              data-testid="voucher-unaudit-reason"
            />
          </t-form-item>
        </t-form>
        <p class="form-tip">
          原因<strong>必填</strong>，会写进凭证状态日志（审计要能查到「为什么把已审核打回草稿」）。
          反审核后凭证回到「草稿」，改完需要重新审核才能记账。
        </p>
        <t-alert
          v-if="unAuditError"
          theme="error"
          :message="unAuditError"
          data-testid="voucher-unaudit-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ④ 红冲弹窗（原因必填 → P-FLOW-3B 起走审批） ==================== -->
    <t-dialog
      v-model:visible="reverseVisible"
      header="提交凭证红冲"
      theme="warning"
      width="520px"
      :confirm-btn="{ content: '提交红冲', loading: reverseSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!reverseSubmitting"
      data-testid="voucher-reverse-dialog"
      @confirm="submitReverse"
    >
      <div v-if="reverseTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ reverseTarget.voucher_no }}</span>
          <span class="muted">
            借 {{ formatMoney(reverseTarget.total_debit) }} / 贷 {{ formatMoney(reverseTarget.total_credit) }}
          </span>
        </div>
        <t-form label-align="top">
          <t-form-item label="红冲原因" required-mark>
            <t-input
              v-model="reverseForm.reason"
              placeholder="例如：录错科目 / 重复记账"
              data-testid="voucher-reverse-reason"
            />
          </t-form-item>
        </t-form>
        <p class="form-tip">
          配了「凭证红冲审批」流程时，提交后只是<strong>送审</strong>（凭证仍是「已记账」、报表照旧），
          列表会给它打「红冲审批中」；财务经理复核通过才生成红字凭证（借贷互换、金额仍为正、独立号段 PZR），
          原凭证状态保持「已记账」，只记录它被哪张红字凭证冲掉。没配流程则立即生效。
        </p>
        <t-alert v-if="reverseError" theme="error" :message="reverseError" data-testid="voucher-reverse-error" />
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 新增凭证（分录编辑器 + 实时平衡校验） ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="新增手工凭证"
      width="880px"
      :confirm-btn="{
        content: '保存并记账',
        loading: createSubmitting,
        theme: 'primary',
        disabled: !createBalanced,
      }"
      :close-on-overlay-click="!createSubmitting"
      data-testid="voucher-create-dialog"
      @confirm="submitCreate"
    >
      <div class="dialog-body">
        <t-form label-align="top">
          <div class="form-row">
            <t-form-item label="凭证日期">
              <t-input v-model="createForm.voucher_date" placeholder="YYYY-MM-DD（留空 = 今天）" data-testid="voucher-create-date" />
            </t-form-item>
            <t-form-item label="摘要" required-mark class="form-row__grow">
              <t-input v-model="createForm.summary" placeholder="例如：计提本月折旧" data-testid="voucher-create-summary" />
            </t-form-item>
          </div>
        </t-form>

        <t-table
          class="entry-editor"
          row-key="key"
          :data="createForm.entries"
          :columns="editorColumns"
          size="small"
          bordered
          data-testid="voucher-create-entries"
        >
          <template #account_id="{ row }">
            <t-select
              v-model="row.account_id"
              :options="accountOptions"
              filterable
              placeholder="选科目（末级）"
              :data-testid="`voucher-create-account-${row.key}`"
            />
          </template>
          <template #debit="{ row }">
            <t-input-number
              v-model="row.debit"
              theme="normal"
              :min="0"
              :decimal-places="2"
              :step="0.01"
              placeholder="借方"
              :data-testid="`voucher-create-debit-${row.key}`"
              @change="row.credit = 0"
            />
          </template>
          <template #credit="{ row }">
            <t-input-number
              v-model="row.credit"
              theme="normal"
              :min="0"
              :decimal-places="2"
              :step="0.01"
              placeholder="贷方"
              :data-testid="`voucher-create-credit-${row.key}`"
              @change="row.debit = 0"
            />
          </template>
          <template #summary="{ row }">
            <t-input v-model="row.summary" placeholder="留空继承单头摘要" />
          </template>
          <template #op="{ row }">
            <t-link
              theme="danger"
              :disabled="createForm.entries.length <= 2"
              :data-testid="`voucher-create-remove-${row.key}`"
              @click="removeEntry(row.key)"
            >
              删除
            </t-link>
          </template>
        </t-table>

        <div class="editor-foot">
          <t-button variant="outline" size="small" data-testid="voucher-create-add-row" @click="addEntry">
            加一行
          </t-button>
          <span class="editor-foot__spacer" />
          <span class="muted">借方合计</span>
          <span class="money" data-testid="voucher-create-total-debit">{{ formatCents(createTotals.debitCents) }}</span>
          <span class="muted">贷方合计</span>
          <span class="money" data-testid="voucher-create-total-credit">{{ formatCents(createTotals.creditCents) }}</span>
          <t-tag :theme="createBalanced ? 'success' : 'danger'" size="small" data-testid="voucher-create-balance">
            {{ createBalanced ? '借贷平衡' : `差额 ${formatCents(createTotals.diffCents)}` }}
          </t-tag>
        </div>

        <p class="form-tip">
          手工凭证保存即记账（status = POSTED，与自动凭证同口径）；期间必须是「已开启」，
          借贷不平衡时保存按钮不可用（后端同样会 400 并点名差额）。
        </p>
        <t-alert v-if="createError" theme="error" :message="createError" data-testid="voucher-create-error" />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import {
  voucherBatchExport,
} from '@/api/business/batch';
import BatchActionBar from '@/components/BatchActionBar.vue';
import { useBatchSelection } from '@/composables/useBatchSelection';
import { useUserStore } from '@/stores/user';
import type { BatchActionSpec, BatchExportTask, BatchResult, BatchRow } from '@/utils/batch-action';
/**
 * 凭证页（P2-E2）—— 列表 + 详情抽屉（分录表 + **借贷平衡可视化**）。
 *
 * # 字段来源（硬红线 1）
 *
 * 契约在 `@/api/business/erp` 的 `VoucherRow` / `VoucherDetail`，逐字抄
 * `backend/src/business/erp/voucher_query.rs` 的 `VoucherRes` / `VoucherEntryRes` /
 * `VoucherDetailRes`（`voucher_no` / `voucher_date` / `period` / `voucher_type` /
 * `status` / `total_debit` / `total_credit` / `entries[].line_no` …）。
 *
 * # ★ 借贷平衡为什么要前端重算（卡面 §四 硬要求）
 *
 * 后端有三道闸（CHECK + 详情一致性闸 + 生成时断言），正常响应必然平。
 * 但「用户看到的平衡」必须是他看得懂的两个数字对比：合计行写出 Σ借 / Σ贷，
 * 相等打绿勾、不等红字 + 差额。所以 `voucher-logic.ts::sumEntries` 在**前端逐行加一遍**，
 * 并与后端给的 `entry_total_*` / 单头 `total_*` 三方比对（{@link balanceCheck}）：
 * 三个数不一致说明响应自相矛盾，界面单独报警（比账不平更严重）。
 *
 * # 金额
 *
 * 一律 `toCents` 换整数分再比较 / 累加（见 `../shared/money.ts` 文件头），
 * 用浮点比 `226.00000000000003 === 226` 会假报「不平」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import {
  fetchAccountOptions,
  fetchVoucherDetail,
  fetchVoucherPage,
  type AccountRow,
  type VoucherEntryLine,
  type VoucherRow,
} from '@/api/business/erp';
import {
  createVoucher,
  submitReverseVoucher,
  withdrawReverseVoucher,
  type CreateVoucherReq,
  type VoucherDetailEx,
  type VoucherRowEx,
} from './voucher-write-api';
import {
  VOUCHER_PAGE_SIZE,
  VOUCHER_PENDING_REVERSE_LABEL,
  VOUCHER_STATUS_FILTER_OPTIONS,
  VOUCHER_TYPE_FILTER_OPTIONS,
  balanceCheck,
  balanceIcon,
  balanceText,
  buildVoucherQuery,
  canReverseVoucher as canReverse,
  canWithdrawReverse as canWithdrawReverseVoucher,
  emptyVoucherFilter,
  isPendingReverse,
  isVoucherFiltered,
  sourceTypeLabel,
  voucherStatusLabel,
  voucherStatusTheme,
  voucherTitleOf,
  withdrawReverseBlockedReason,
  type VoucherFilterState,
  type VoucherReverseState,
} from './voucher-logic';
import { submitFeedback, withdrawFeedback } from '../shared/approval-flow';
import { dash, formatCents, formatMoney, shortDate, toCents } from '../shared/money';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import { getFlowInstance } from '@/api/business/flow-api';
import {
  APPROVAL_PROGRESS_TITLE,
  EMPTY_APPROVAL_PROGRESS,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
/**
 * ★ P0-FIX-4B：审核 / 反审核 / 记账
 *
 * - `voucher-audit-logic.ts` 是「哪个状态能点哪个按钮、为什么点不了」的**唯一真相**
 *   （纯函数，`tests/unit/p0-fix-4b-voucher-audit.test.ts` 直接打状态矩阵）；
 * - `voucher-audit-api.ts` 是三个写接口的契约（路径 / 入参 / 权限码写在文件头）。
 *
 * 页面这一层只做三件事：把 `hasPermission` 喂进去、弹确认、把回执贴回列表和抽屉。
 */
import {
  unAuditReasonError,
  voucherActionButtons,
  voucherActionConfirmText,
  voucherActionDoneText,
  voucherApprovalInstanceId,
  voucherAuditNote,
  type VoucherActionButton,
  type VoucherAuditAction,
} from './voucher-audit-logic';
import { auditVoucher, postVoucher, unAuditVoucher } from './voucher-audit-api';

/** 表格列（`colKey` 与 `VoucherRow` 字段同名；`source_type` / `entry_count` 不是列字段，走插槽） */
const columns: PrimaryTableCol[] = [
  // D7：勾选列（没有任何可用批量动作的行，复选框直接禁用）
  {
    colKey: 'row-select',
    type: 'multiple',
    width: 50,
    disabled: (options: { row: unknown }) => !isSelectable(options.row as VoucherRowEx),
  },
  { colKey: 'voucher_no', title: '凭证号', width: 230 },
  { colKey: 'voucher_date', title: '日期', width: 90 },
  { colKey: 'period', title: '期间', width: 100 },
  { colKey: 'summary', title: '摘要', minWidth: 180, ellipsis: true },
  { colKey: 'source_type', title: '来源', width: 130 },
  { colKey: 'entry_count', title: '分录', width: 80, align: 'right' },
  { colKey: 'total_debit', title: '借方', width: 130, align: 'right' },
  { colKey: 'total_credit', title: '贷方', width: 130, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  // P0-FIX-4B：操作列从「查看 / 红冲」扩成「查看 + 审核链三动作 + 红冲」，80px 放不下
  { colKey: 'op', title: '操作', width: 220, fixed: 'right' },
];

/** 分录列（★ 借方 / 贷方两列并排，空的那侧不写 0，与纸质凭证同排版） */
const entryColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行', width: 56, align: 'center' },
  { colKey: 'summary', title: '摘要', minWidth: 140, ellipsis: true },
  { colKey: 'account', title: '科目', minWidth: 200 },
  { colKey: 'debit_amount', title: '借方', width: 130, align: 'right' },
  { colKey: 'credit_amount', title: '贷方', width: 130, align: 'right' },
];

const statusFilterOptions = VOUCHER_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
const typeFilterOptions = VOUCHER_TYPE_FILTER_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================

const router = useRouter();
const route = useRoute();

const filter = reactive<VoucherFilterState>(emptyVoucherFilter());
const rows = ref<VoucherRowEx[]>([]);
const total = ref(0);

// ==================== 批量操作（D7） ====================

/** 当前用户（批量动作的权限判定）。 */
const userStore = useUserStore();

/**
 * 撤回红冲审批的权限码（P-FLOW-3B；V0191 目前只授给 `SUPER_ADMIN`，见子卡）。
 * 没码的人看得到「红冲审批中」徽标，但点不到撤回入口，详情里会说明原因。
 */
const WITHDRAW_REVERSE_PERMISSION = 'business:erp:voucher:withdraw-reverse';

/** 当前账号能不能撤回红冲审批 */
const hasWithdrawPermission = computed(() => userStore.hasPermission(WITHDRAW_REVERSE_PERMISSION));

/** 模板用的撤回判据（把权限并进纯函数，`.vue` 里不留第二个判据）。 */
function canWithdrawReverse(row: VoucherReverseState | null): boolean {
  return canWithdrawReverseVoucher(row, hasWithdrawPermission.value);
}

/** 详情抽屉里「红冲审批中」那句说明。 */
function pendingReverseDescription(row: VoucherReverseState | null): string {
  const blocked = withdrawReverseBlockedReason(row, hasWithdrawPermission.value);
  if (blocked) {
    return blocked;
  }
  return '审批通过后才会生成红字凭证（原凭证仍「已记账」，只记录被哪张红字凭证冲掉）；撤回则什么都不会发生，可以重新提交红冲。';
}

/** 一次「选中全部筛选结果」拉多少主键（后端单页上限 200）。 */
const BATCH_FETCH_PAGE_SIZE = 200;

/**
 * 批量动作声明。
 *
 * `allowedStatus` 与后端状态机**逐字一致**：不符合的行由组件禁用按钮并说明原因，
 * 高危动作（作废 / 删除）走二次确认 + 必填原因。
 */
const BATCH_ACTIONS: BatchActionSpec[] = [
  { key: 'export', label: '批量导出', theme: 'default', exportKind: true },
];

/** 动作 → 权限码（与单条按钮同一套码；没有码的按钮禁用并说明）。 */
const BATCH_PERMISSIONS: Record<string, string> = {
  export: 'business:impexp:export:create',
};

const { selectedIds, selectedRows, onSelectChange, onSelectedRows, isSelectable } =
  useBatchSelection(BATCH_ACTIONS, rows);

/** 当前用户能不能点这个批量动作。 */
function canBatchAction(action: BatchActionSpec): boolean {
  const code = BATCH_PERMISSIONS[action.key];
  return !code || userStore.hasPermission(code);
}

/**
 * 执行器：组件按 20 条一批回调这里（一批一个请求）。
 *
 * 后端逐条独立事务：一批里某条失败不影响同批其它条，失败原因逐条回传。
 */
async function runBatchAction(
  _action: BatchActionSpec,
  ids: string[],
  _payload: { reason?: string },
): Promise<BatchResult | BatchExportTask> {
  // 凭证列表按凭证号 / 期间 / 类型筛，导出沿用凭证号口径
  return voucherBatchExport({ ids, keyword: filter.voucher_no || undefined });
}

/** 「选中全部 N 条筛选结果」：按当前筛选把全部主键拉回来（最多 1 万条）。 */
async function fetchAllRowIds(): Promise<BatchRow[]> {
  const acc: BatchRow[] = [];
  for (let page = 1; page <= 50; page += 1) {
    const data = await fetchVoucherPage(buildVoucherQuery(filter, page, BATCH_FETCH_PAGE_SIZE));
    const list = data.list ?? [];
    acc.push(...list.map((row) => ({ id: row.id, status: row.status })));
    if (list.length === 0 || acc.length >= (data.total ?? 0)) {
      break;
    }
  }
  return acc;
}

/** 批量跑完：回到第一页刷新（勾选已由组件清空，避免误点第二次）。 */
function handleBatchDone(): void {
  void loadVouchers(1);
}
const current = ref(1);
const pageSize = VOUCHER_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

/**
 * 列表记忆（O4）：期间 / 状态 / 类型 / 凭证号 + 页码。
 * 构造即恢复，早于 `onMounted` 的首次加载。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<VoucherFilterState>({
  listName: 'erp-voucher',
  filter,
  page: current,
  pageSize,
});

const filtered = computed(() => isVoucherFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 拉列表。
 *
 * 「红冲审批中」直接读后端下发的 `pending_reverse_instance_id`（P-FLOW-3B 补漏后
 * `voucher_query.rs::VoucherRes` 的**列表 / 详情 / 红冲回执三处都下发**，不审批时是
 * `null`、字段恒在），所以前端不再自己倒查审批实例。
 *
 * 早期那层「从审批引擎实例分页补 pending」的兜底已整层删除（P-FLOW-3B 补漏）：
 * 它要多打一次 `GET /business/flow/instance/page`，而且实例分页**有数据可见性过滤**
 * （不是本人发起 / 参与的就查不到）—— 于是「能看凭证列表但没参与过红冲审批」的账号
 * 看不到徽标。换成后端字段后，只要能看到凭证列表就能看到徽标，且一次请求都不多发。
 */
async function loadVouchers(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchVoucherPage(buildVoucherQuery(filter, page, pageSize));
    // 后端多给 `reverse_of_no` / `reversed_by_no`（P7）、`pending_reverse_instance_id` /
    // `reverse_reason`（P-FLOW-3B）；api 层的 VoucherRow 还没声明（那个文件是全 ERP 共用契约，
    // 本卡不动），这里按扩展类型收。
    rows.value = (data.list ?? []) as VoucherRowEx[];
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

function handleSearch(): void {
  void loadVouchers(1);
}

function handleReset(): void {
  Object.assign(filter, emptyVoucherFilter());
  void loadVouchers(1);
}

/** 列表记忆 · 「清空筛选」：条件回全量 + 页码回 1 + 删掉 sessionStorage 记忆 */
function handleClearListMemory(): void {
  Object.assign(filter, emptyVoucherFilter());
  current.value = 1;
  forgetListMemory();
  void loadVouchers(1);
}

function refresh(): void {
  void loadVouchers();
}

/** 翻页 */
function handlePageChange(pageInfo: { current?: number }): void {
  void loadVouchers(pageInfo.current ?? 1);
}

/** 行点击 → 详情（TDesign 给的是 `RowEventContext`，不是 row 本体） */
function handleRowClick(context: { row: unknown }): void {
  void openDetail(context.row as VoucherRow);
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<VoucherDetailEx | null>(null);

/**
 * 审批进度（C2 + P1-FIX-11）。
 *
 * 凭证的审批实例有两处来源，取先有值的那个（判据在
 * `voucher-audit-logic.ts::voucherApprovalInstanceId`，单测覆盖四种组合）：
 * - `pending_reverse_instance_id`：红冲审批中（P-FLOW-3B，列表 / 详情 / 红冲回执三处都下发）；
 * - `approval_instance_id`：后端 4A 若给审核链也挂实例，就用它。
 *
 * 装载器与采购 / 收货 / 应付 / 调拨同一个（`shared/approval-progress`），
 * `tasks` 就是同一接口的 `timeline[]`（谁 / 时间 / 意见 / 跳过说明）。
 * 拉不到（权限 / 实例已归档）→ 空结果，只少这一块，不影响凭证本身。
 */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

/** 按凭证当前挂的审批实例 id 重拉进度（没有实例 id → 直接清空，一次请求都不发）。 */
async function loadVoucherApprovalProgress(row: VoucherDetailEx | null): Promise<void> {
  approvalProgress.value = await loadApprovalProgress(voucherApprovalInstanceId(row), getFlowInstance);
}

/** 空凭证（打开抽屉的占位，避免下一帧读到上一张凭证的合计） */
function emptyDetail(row: VoucherRow): VoucherDetailEx {
  const extended = row as VoucherRowEx;
  return {
    ...extended,
    reverse_of_no: extended.reverse_of_no ?? null,
    reversed_by_no: extended.reversed_by_no ?? null,
    entries: [] as VoucherEntryLine[],
    entry_total_debit: 0,
    entry_total_credit: 0,
    balanced: true,
  };
}

/** 详情接口：api 层类型还没声明 `reverse_of_no` / `reversed_by_no`（共用契约文件本卡不动），按扩展类型收。 */
async function loadDetailEx(id: string): Promise<VoucherDetailEx> {
  return (await fetchVoucherDetail(id)) as VoucherDetailEx;
}

/**
 * 打开详情。
 *
 * 先用列表行占位（单头字段列表已全给），再拉详情补 `entries` ——
 * 抽屉立刻能显示凭证号 / 状态 / 来源，不会先白一下。
 *
 * ⚠️ P-FLOW-3B：`pending_reverse_instance_id` / `reverse_reason` 详情接口**也恒下发**
 * （补漏后端已透出），所以拉回详情后直接用它，不再从列表行 / 审批实例分页补。
 */
async function openDetail(row: VoucherRow): Promise<void> {
  detail.value = emptyDetail(row);
  detailVisible.value = true;
  detailLoading.value = true;
  // 先清进度：上一张凭证的红冲审批节点不能漏到这一张上
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    const loaded = await loadDetailEx(row.id);
    detail.value = normalizePendingReverse(loaded);
    await loadVoucherApprovalProgress(detail.value);
  } catch (error) {
    // 详情失败不关抽屉：单头仍可看，分录位置给出原因
    void MessagePlugin.error(messageOf(error));
  } finally {
    detailLoading.value = false;
  }
}

/**
 * 详情字段归一：后端**恒定下发** `pending_reverse_instance_id` / `reverse_reason`
 * （不审批时是 `null`），这里只把 `undefined` 归成 `null`，让模板判据只有一种形态。
 */
function normalizePendingReverse(detailRow: VoucherDetailEx): VoucherDetailEx {
  return {
    ...detailRow,
    pending_reverse_instance_id: detailRow.pending_reverse_instance_id ?? null,
    reverse_reason: detailRow.reverse_reason ?? null,
  };
}

/** 按 id 打开另一张凭证（详情里「已红冲 → PZR-xxx」的跳转）。 */
async function openById(id: string | null): Promise<void> {
  if (!id) {
    return;
  }
  detailLoading.value = true;
  try {
    detail.value = normalizePendingReverse(await loadDetailEx(id));
    await loadVoucherApprovalProgress(detail.value);
  } catch (error) {
    void MessagePlugin.error(messageOf(error));
  } finally {
    detailLoading.value = false;
  }
}

/** 借贷平衡三方校验（前端逐行加 / 后端分录合计 / 单头合计） */
const balance = computed(() =>
  detail.value
    ? balanceCheck(detail.value)
    : { debitCents: 0, creditCents: 0, diffCents: 0, balanced: true, consistent: true, mismatch: '' } as ReturnType<
        typeof balanceCheck
      >,
);

/** 合计行样式类（平 = 绿，不平 = 红） */
const balanceClass = computed(() => (balance.value.balanced ? 'balance--ok' : 'balance--bad'));

// ==================== P0-FIX-4B：审核链（审核 / 反审核 / 记账） ====================

/**
 * 审核链动作的操作对象。
 *
 * 列表行（`VoucherRowEx`）与详情（`VoucherDetailEx`）都满足：审核动作只需要这 4 个字段。
 * `create_by` 后端**没有**下发给列表 / 详情（`voucher_query.rs::VoucherRes` 里没这个字段），
 * 所以声明成可选：拿不到就不做自审判定（宁可不拦，也不误拦），真正的自审闸在后端 403。
 */
interface VoucherAuditable {
  id: string;
  voucher_no: string;
  status: string;
  create_by?: string | null;
}

/** 写操作进行中（列表 / 详情 / 弹窗共用一把，避免连点打出两次状态流转） */
const auditSubmitting = ref(false);
/** 审核链最后一次失败的原因（详情抽屉里常驻显示，关掉弹窗也还看得见） */
const auditError = ref('');

/** 反审核弹窗 */
const unAuditVisible = ref(false);
const unAuditTarget = ref<VoucherAuditable | null>(null);
const unAuditForm = reactive({ reason: '' });
const unAuditError = ref('');

/** 原因必填：非空白才允许点「确认反审核」（判据在 `voucher-audit-logic.ts`，单测直接打） */
const unAuditReasonValid = computed(() => unAuditReasonError(unAuditForm.reason) === '');

/** 判定按钮可用性需要的上下文（权限 / 当前用户 / 制单人） */
function auditContextOf(target: VoucherAuditable): {
  hasPermission: (code: string) => boolean;
  currentUserId: string | null;
  createdBy: string | null;
} {
  return {
    hasPermission: (code: string) => userStore.hasPermission(code),
    currentUserId: userStore.userInfo?.id ?? null,
    createdBy: target.create_by ?? null,
  };
}

/** 列表某行的审核按钮（状态 → 权限 → 自审三道筛，纯逻辑在 `voucher-audit-logic.ts`） */
function auditButtons(target: VoucherAuditable): VoucherActionButton[] {
  return voucherActionButtons(target.status, auditContextOf(target));
}

/** 详情抽屉的审核按钮（同上；`detail` 为空时没有按钮） */
const detailAuditButtons = computed<VoucherActionButton[]>(() =>
  detail.value ? voucherActionButtons(detail.value.status, auditContextOf(detail.value)) : [],
);

/** 详情里「为什么点不了」的原因清单（空数组 = 没有置灰按钮） */
const auditBlockedReasons = computed<string[]>(() =>
  detailAuditButtons.value.filter((btn) => btn.disabled).map((btn) => btn.reason),
);

/**
 * 点了审核链按钮。
 *
 * - 反审核 → 开弹窗（原因必填，不能用一个 confirm 糊过去）；
 * - 审核 / 记账 → 二次确认（记账动总账且不可撤销，必须让用户看清后果）。
 */
function onAuditAction(btn: VoucherActionButton | null, target: VoucherAuditable | null): void {
  if (!btn || !target || btn.disabled || auditSubmitting.value) {
    return;
  }
  if (btn.key === 'un-audit') {
    openUnAudit(target);
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: btn.key === 'audit' ? '审核凭证' : '记账',
    theme: btn.key === 'post' ? 'warning' : 'default',
    body: voucherActionConfirmText(btn.key, target.voucher_no),
    confirmBtn: {
      content: btn.key === 'audit' ? '确认审核' : '确认记账',
      theme: btn.theme,
    },
    cancelBtn: '再想想',
    onConfirm: async () => {
      dialog.hide();
      await runAuditAction(btn.key, target);
    },
  });
}

/**
 * 执行审核 / 记账（都是写操作，回执是凭证详情：直接用它刷抽屉，不再多发一次 GET）。
 *
 * 失败原因落两处：`auditError`（详情里常驻）+ toast（列表页操作时用户不在详情里）。
 */
async function runAuditAction(action: VoucherAuditAction, target: VoucherAuditable): Promise<void> {
  if (auditSubmitting.value || action === 'un-audit') {
    return;
  }
  auditSubmitting.value = true;
  auditError.value = '';
  try {
    const updated = action === 'audit' ? await auditVoucher(target.id) : await postVoucher(target.id);
    applyAuditResult(action, target, updated);
  } catch (error) {
    auditError.value = messageOf(error);
    MessagePlugin.error(auditError.value);
  } finally {
    auditSubmitting.value = false;
  }
}

/** 打开反审核弹窗（每次清空上一张的原因与错误） */
function openUnAudit(target: VoucherAuditable): void {
  unAuditTarget.value = target;
  unAuditForm.reason = '';
  unAuditError.value = '';
  unAuditVisible.value = true;
}

/**
 * 提交反审核：**原因必填**，空原因在弹窗这一层就拦住（一次请求都不发），
 * 同时原因 ≥ 1 字由 `unAuditReasonError` 判定（后端 4A 若要求更长，它会 400 并点名）。
 */
async function submitUnAudit(): Promise<void> {
  const target = unAuditTarget.value;
  if (!target || auditSubmitting.value) {
    return;
  }
  const reasonError = unAuditReasonError(unAuditForm.reason);
  if (reasonError) {
    unAuditError.value = reasonError;
    return;
  }
  auditSubmitting.value = true;
  unAuditError.value = '';
  auditError.value = '';
  try {
    const updated = await unAuditVoucher(target.id, unAuditForm.reason.trim());
    unAuditVisible.value = false;
    applyAuditResult('un-audit', target, updated);
  } catch (error) {
    unAuditError.value = messageOf(error);
  } finally {
    auditSubmitting.value = false;
  }
}

/**
 * 动作成功后的收口：提示 + （详情开着同一张时）用回执刷新详情 + 列表重拉。
 *
 * 回执是 `VoucherDetailRes`（含全部分录），所以详情可以整块替换；
 * 列表必须重拉 —— 状态列颜色 / 按钮显隐 / 红冲入口都跟着状态变。
 */
function applyAuditResult(action: VoucherAuditAction, target: VoucherAuditable, updated: VoucherDetailEx): void {
  MessagePlugin.success(voucherActionDoneText(action, updated.voucher_no || target.voucher_no));
  if (detail.value?.id === updated.id) {
    detail.value = normalizePendingReverse(updated);
  }
  void loadVouchers();
}

// ==================== 红冲（P7 → P-FLOW-3B 走审批） ====================

/**
 * 可红冲 / 可撤回的判据都在 `voucher-logic.ts`（纯函数，单测直接打）:
 * `canReverse` 已排除「红冲审批中」的凭证（否则点了必 409），
 * `canWithdrawReverse` 再叠一道权限（撤回入口显不显示）。
 */

const reverseVisible = ref(false);
const reverseSubmitting = ref(false);
const reverseError = ref('');
const reverseTarget = ref<VoucherRowEx | null>(null);
const reverseForm = reactive({ reason: '' });

/**
 * 打开红冲弹窗。
 *
 * P-FLOW-3B 起这个弹窗是**送审**：配了流程时提交只生成审批单（凭证仍是「已记账」），
 * 所以按钮叫「提交红冲」而不是「确认红冲」（文案见弹窗里的 form-tip）。
 */
function openReverse(row: VoucherRow): void {
  reverseTarget.value = row as VoucherRowEx;
  reverseForm.reason = '';
  reverseError.value = '';
  reverseVisible.value = true;
}

/**
 * 提交红冲（走审批，三种结局三种提示，口径见 `shared/approval-flow.ts`）。
 *
 * - `PENDING_APPROVAL` → info「已提交审批，等待财务经理复核（PZ-…）」+ 列表打「红冲审批中」；
 * - `REVERSED` → success「已红冲：生成红字凭证 PZR-…」（未配流程，降级直做）；
 * - 其它 → 后端 `message` 兜底。
 *
 * ⚠️ 送审**绝不能**提示「已红冲」：红字凭证还不存在，账一分没动。
 */
async function submitReverse(): Promise<void> {
  const target = reverseTarget.value;
  if (!target || reverseSubmitting.value) {
    return;
  }
  const reason = reverseForm.reason.trim();
  if (!reason) {
    reverseError.value = '红冲原因必填（红字凭证的分录摘要会带这段文字）';
    return;
  }
  reverseSubmitting.value = true;
  reverseError.value = '';
  try {
    const result = await submitReverseVoucher(target.id, reason);
    reverseVisible.value = false;
    const feedback = submitFeedback('voucher-reverse', result, result.voucher_no || target.voucher_no, result.voucher_no);
    MessagePlugin[feedback.level](feedback.text);
    if (result.outcome === 'PENDING_APPROVAL') {
      // 送审：`ReverseSubmitRes`（P-FLOW-3B 补漏）**也下发** `pending_reverse_instance_id`，
      // 抽屉立刻显示「红冲审批中」，不用等列表刷新。
      const instanceId = result.pending_reverse_instance_id ?? result.approval_instance_id ?? null;
      detail.value = normalizePendingReverse({ ...result, pending_reverse_instance_id: instanceId });
      detailVisible.value = true;
      // C2：送审那一刻就把审批进度拉出来（看到「谁是第一个审批人 / 现在停在哪个节点」）
      await loadVoucherApprovalProgress(detail.value);
    } else {
      // 直接红冲：抽屉切到红字凭证，用户能立刻看到借贷互换后的分录
      detail.value = result;
      detailVisible.value = true;
      approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    }
    void loadVouchers();
  } catch (error) {
    reverseError.value = messageOf(error);
  } finally {
    reverseSubmitting.value = false;
  }
}

// ==================== P-FLOW-3B：撤回红冲审批 ====================

/** 撤回中（防连点：撤回是不可逆的写操作，双击会拿到 409）。 */
const withdrawingReverse = ref(false);

/**
 * 撤回红冲审批：二次确认 → 调接口 → 列表 / 抽屉回到「已记账 + 无 pending」。
 *
 * 撤回**什么都不改**（凭证一直是 `POSTED`，红字凭证也从未生成），所以提示里说的是
 * 「仍是「已记账」」而不是「已恢复」。
 */
function openWithdrawReverse(row: VoucherReverseState & { id: string; voucher_no: string }): void {
  if (!canWithdrawReverse(row) || withdrawingReverse.value) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '撤回红冲审批',
    theme: 'warning',
    body: `撤回 ${row.voucher_no} 的红冲审批：审批单作废，凭证仍是「已记账」，不会生成红字凭证。撤回后可以重新提交红冲。`,
    confirmBtn: { content: '撤回审批', theme: 'warning' },
    cancelBtn: '再想想',
    onConfirm: async () => {
      dialog.hide();
      await doWithdrawReverse(row);
    },
  });
}

/** 真正执行撤回。 */
async function doWithdrawReverse(row: { id: string; voucher_no: string }): Promise<void> {
  if (withdrawingReverse.value) {
    return;
  }
  withdrawingReverse.value = true;
  try {
    const updated = await withdrawReverseVoucher(row.id, '页面撤回');
    const feedback = withdrawFeedback('voucher-reverse', updated.voucher_no || row.voucher_no, voucherStatusLabel(updated.status));
    MessagePlugin[feedback.level](feedback.text);
    // 撤回后后端已把 `pending_reverse_instance_id` 置空（响应里就是 null），抽屉直接用它
    if (detail.value?.id === updated.id) {
      detail.value = normalizePendingReverse({ ...updated, pending_reverse_instance_id: null });
      // C2：审批单没了 → 进度区跟着消失（不留上一张审批单的节点）
      approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    }
    void loadVouchers();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    withdrawingReverse.value = false;
  }
}

// ==================== 新增手工凭证（P7） ====================

/** 分录编辑行（`key` 只用于表格 row-key，不进请求体）。 */
interface EditorRow {
  key: number;
  account_id: string;
  debit: number;
  credit: number;
  summary: string;
}

let editorSeq = 0;

function newEditorRow(): EditorRow {
  editorSeq += 1;
  return { key: editorSeq, account_id: '', debit: 0, credit: 0, summary: '' };
}

const createVisible = ref(false);
const createSubmitting = ref(false);
const createError = ref('');
const createForm = reactive({
  voucher_date: '',
  summary: '',
  entries: [newEditorRow(), newEditorRow()] as EditorRow[],
});

const accounts = ref<AccountRow[]>([]);
/** 科目下拉：只给启用中的**末级**科目（非末级记账不合法，后端也会按 id 取快照但语义上不该选）。 */
const accountOptions = computed(() =>
  accounts.value
    .filter((item) => item.status === 1 && item.is_leaf)
    .map((item) => ({ label: `${item.account_code} ${item.account_name}`, value: item.id })),
);

const editorColumns: PrimaryTableCol[] = [
  { colKey: 'account_id', title: '科目', minWidth: 240 },
  { colKey: 'debit', title: '借方金额', width: 150 },
  { colKey: 'credit', title: '贷方金额', width: 150 },
  { colKey: 'summary', title: '分录摘要', minWidth: 180 },
  { colKey: 'op', title: '操作', width: 80, align: 'center' },
];

/** 实时合计（整数分，避免浮点误差把「平」算成「不平」）。 */
const createTotals = computed(() => {
  let debitCents = 0;
  let creditCents = 0;
  for (const row of createForm.entries) {
    debitCents += toCents(row.debit);
    creditCents += toCents(row.credit);
  }
  return {
    debitCents,
    creditCents,
    diffCents: debitCents - creditCents,
  };
});

/** 借贷平衡且非零：不平衡时「保存并记账」按钮禁用（卡面 §二 ③）。 */
const createBalanced = computed(
  () => createTotals.value.diffCents === 0 && createTotals.value.debitCents > 0,
);

async function openCreate(): Promise<void> {
  createError.value = '';
  createForm.voucher_date = '';
  createForm.summary = '';
  createForm.entries = [newEditorRow(), newEditorRow()];
  createVisible.value = true;
  if (accounts.value.length === 0) {
    accounts.value = await fetchAccountOptions().catch(() => [] as AccountRow[]);
  }
}

function addEntry(): void {
  createForm.entries.push(newEditorRow());
}

function removeEntry(key: number): void {
  if (createForm.entries.length <= 2) {
    return;
  }
  createForm.entries = createForm.entries.filter((row) => row.key !== key);
}

/** 提交手工凭证。 */
async function submitCreate(): Promise<void> {
  if (createSubmitting.value) {
    return;
  }
  const summary = createForm.summary.trim();
  if (!summary) {
    createError.value = '摘要必填';
    return;
  }
  if (!createBalanced.value) {
    createError.value = `借贷不平衡（差额 ${formatCents(createTotals.value.diffCents)}），不能保存`;
    return;
  }
  const missing = createForm.entries.findIndex((row) => !row.account_id);
  if (missing >= 0) {
    createError.value = `第 ${missing + 1} 行还没选科目`;
    return;
  }
  const dateText = createForm.voucher_date.trim();
  if (dateText && !/^\d{4}-\d{2}-\d{2}$/.test(dateText)) {
    createError.value = '凭证日期必须是 YYYY-MM-DD（或留空 = 今天）';
    return;
  }

  const body: CreateVoucherReq = {
    voucher_date: dateText || null,
    summary,
    entries: createForm.entries.map((row) => ({
      account_id: row.account_id,
      debit: (toCents(row.debit) / 100).toFixed(4),
      credit: (toCents(row.credit) / 100).toFixed(4),
      summary: row.summary.trim() || null,
    })),
    remark: null,
  };

  createSubmitting.value = true;
  createError.value = '';
  try {
    const created = await createVoucher(body);
    createVisible.value = false;
    void MessagePlugin.success(`已生成凭证 ${created.voucher_no}`);
    void loadVouchers(1);
    detail.value = created;
    detailVisible.value = true;
  } catch (error) {
    createError.value = messageOf(error);
  } finally {
    createSubmitting.value = false;
  }
}

/**
 * 深链：`/business/erp/voucher?voucher_no=PZ-…`（D5 F3：应付单付款记录里的「凭证 PZ-xxx」点过来）。
 *
 * 只预填「凭证号」这一个筛选项（后端 `LIKE %x%`，付款凭证号的唯一前缀已经足够定位），
 * 打开后把 query 清掉，免得刷新页面又把筛选项改回去。
 */
async function openByQuery(): Promise<void> {
  const no = typeof route.query.voucher_no === 'string' ? route.query.voucher_no.trim() : '';
  if (!no) {
    return;
  }
  filter.voucher_no = no;
  await loadVouchers(1);
  const rest = { ...route.query };
  delete rest.voucher_no;
  void router.replace({ path: route.path, query: rest });
}

onMounted(() => {
  // 列表记忆恢复的页码要在首次加载就生效
  void loadVouchers(current.value).then(() => openByQuery());
});
</script>

<style scoped lang="less">
.voucher-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/*
 * 区块标题（C2 新增「审批进度」用）。
 * 与采购 / 收货 / 应付 / 调拨同一组值（16px 上边距 + 13px 600），四个单据看起来一样。
 */
.section-title {
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
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

.tool-row__period {
  width: 130px;
}

.tool-row__status {
  width: 130px;
}

.tool-row__type {
  width: 170px;
}

.tool-row__no {
  width: 200px;
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

.account-cell {
  display: inline-flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
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

/* ==================== 抽屉 ==================== */

.drawer {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 4px 4px 0;
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.drawer-head__title {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 20px;
  margin-bottom: 16px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.field__value {
  font-size: 14px;
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.entry-table {
  margin-bottom: 0;
}

/*
 * ★ 借贷平衡合计行：与分录表的「借方 / 贷方」两列对齐（同样的右对齐宽度），
 * 让人一眼把合计和分录对上。颜色只用 TDesign 语义变量（success / danger），
 * 22 套配色下明暗都自动适配。
 */
.balance {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 10px 16px;
  border: 1px solid var(--td-component-stroke);
  border-top: none;
  border-radius: 0 0 6px 6px;
  background: var(--td-bg-color-container-hover);
  font-size: 14px;
}

.balance__label {
  min-width: 56px;
  color: var(--td-text-color-secondary);
}

.balance__debit,
.balance__credit {
  min-width: 110px;
  text-align: right;
  font-weight: 600;
}

.balance__verdict {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  font-weight: 600;
}

.balance--ok .balance__verdict {
  color: var(--td-success-color);
}

.balance--bad {
  border-color: var(--td-error-color);
  background: var(--td-error-color-light);
}

.balance--bad .balance__verdict {
  color: var(--td-error-color);
}

.balance-alert {
  margin-top: 12px;
}

/* ==================== 操作列 / 关联凭证 / 弹窗（P7） ==================== */

.op-cell {
  display: flex;
  align-items: center;
  gap: 10px;
}

/**
 * ★ P0-FIX-4B：置灰的审核动作。
 *
 * TDesign 的 `t-link disabled` 不派发鼠标事件 → 包一层 <span> 接住 hover，
 * `title` 才出得来（用户看得到「为什么点不了」，而不是一个点不动的灰字）。
 */
.op-dead {
  display: inline-flex;
  align-items: center;
  color: var(--td-text-color-disabled, #c4c7cc);
  cursor: not-allowed;
  font-size: 14px;
}

/* ★ P0-FIX-4B：详情抽屉里的审核动作条 */
.audit-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  margin-bottom: 12px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer, #f3f3f3);
}

.audit-bar__why {
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.audit-bar__spacer {
  flex: 1 1 auto;
}

.audit-bar__note {
  font-size: 12px;
  color: var(--td-text-color-placeholder, #999);
}

.audit-alert {
  margin-bottom: 12px;
}

.linked {
  font-size: 12px;
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.dialog-meta {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 8px 12px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 13px;
}

.form-row {
  display: flex;
  align-items: flex-start;
  gap: 12px;
}

.form-row__grow {
  flex: 1 1 auto;
}

.form-tip {
  margin: 6px 0 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-placeholder);
}

.entry-editor {
  margin-top: 4px;
}

.editor-foot {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 4px 0;
}

.editor-foot__spacer {
  flex: 1 1 auto;
}
</style>
