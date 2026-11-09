<template>
  <div class="contract-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-contract-create"
          @click="openCreateDialog"
        >
          <template #icon><t-icon name="add" /></template>
          新建合同
        </t-button>
        <t-button variant="outline" data-testid="btn-contract-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.contract_no"
          class="tool-row__no"
          placeholder="合同号（模糊）"
          clearable
          data-testid="contract-filter-no"
          @enter="handleSearch"
        />
        <t-input
          v-model="filter.name"
          class="tool-row__name"
          placeholder="合同名称（模糊）"
          clearable
          data-testid="contract-filter-name"
          @enter="handleSearch"
        />
        <t-select
          v-model="filter.contract_type"
          class="tool-row__type"
          :options="typeFilterOptions"
          placeholder="全部类型"
          clearable
          data-testid="contract-filter-type"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="contract-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.party_b_id"
          class="tool-row__supplier"
          :options="supplierOptions"
          placeholder="全部乙方"
          clearable
          filterable
          data-testid="contract-filter-supplier"
          @change="handleSearch"
        />
      </div>

      <div class="tool-row tool-row--second">
        <span class="tool-row__label">签订日期</span>
        <t-date-range-picker
          v-model="filter.sign_range"
          class="tool-row__range"
          placeholder="签订日期区间"
          format="YYYY-MM-DD"
          clearable
          data-testid="contract-filter-sign-range"
        />
        <span class="tool-row__label">到期日期</span>
        <t-date-range-picker
          v-model="filter.expire_range"
          class="tool-row__range"
          placeholder="到期日期区间"
          format="YYYY-MM-DD"
          clearable
          data-testid="contract-filter-expire-range"
        />
        <t-button theme="primary" data-testid="btn-contract-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-contract-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">合同管理</span>
          <span class="panel-head__meta" data-testid="contract-meta">{{ listState.totalText }}</span>
        </div>
        <span class="panel-head__tip">金额列为含税总额；「已签订」的合同才能被采购单引用</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="contract-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadContracts()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="listState.rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="contract-table"
        @page-change="handlePageChange"
      >
        <template #contract_no="{ row }">
          <t-link theme="primary" class="mono" :data-testid="`contract-no-${row.contract_no}`" @click="openDetail(row)">
            {{ row.contract_no }}
          </t-link>
        </template>

        <template #contract_type="{ row }">{{ contractTypeLabel(row.contract_type) }}</template>

        <template #party_b_name="{ row }">
          <span :data-testid="`contract-party-${row.contract_no}`">{{ contractPartyBName(row) }}</span>
        </template>

        <template #amount="{ row }">
          <span class="money" :data-testid="`contract-amount-${row.contract_no}`">{{ contractAmountText(row.amount) }}</span>
        </template>

        <template #sign_date="{ row }">
          <span class="muted">{{ row.sign_date || '—' }}</span>
        </template>

        <template #period="{ row }">
          <span class="muted">{{ contractPeriodText(row.start_date, row.end_date) }}</span>
        </template>

        <template #status="{ row }">
          <StatusBadge
            domain="erp-contract"
            :status="row.status"
            :data-testid="`contract-status-${row.contract_no}`"
          />
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-contract-view-${row.contract_no}`" @click="openDetail(row)">
              查看
            </t-link>
            <!-- 只有草稿能改（后端 PUT 非 DRAFT 一律 409 + 66016） -->
            <t-link
              v-if="canEditContract(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`btn-contract-edit-${row.contract_no}`"
              @click="openEditDialog(row)"
            >
              编辑
            </t-link>
            <!-- 卡面：删除只给 DRAFT（且被采购单引用时后端 409 + 66018） -->
            <t-link
              v-if="canDeleteContract(row)"
              v-privilege="[PERM.remove]"
              theme="danger"
              :data-testid="`btn-contract-remove-${row.contract_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
            <!-- 合法边全部来自状态机（见 contract-logic.ts::CONTRACT_EDGES） -->
            <t-link
              v-for="action in contractTransitions(row.status)"
              :key="action.target"
              v-privilege="[PERM.status]"
              :theme="action.theme === 'default' ? 'default' : action.theme"
              :data-testid="`btn-contract-to-${action.target}-${row.contract_no}`"
              @click="openStatusDialog(row, action)"
            >
              {{ action.label }}
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="contract-empty">
            <t-icon name="file" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ emptyState.title }}</p>
            <p class="panel-empty__desc">{{ emptyState.desc }}</p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      width="760px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="contract-form-dialog"
    >
      <template #header>
        <div class="dlg-head">
          <span class="dlg-head__title">{{ form.id ? '编辑合同' : '新建合同' }}</span>
          <span v-if="form.id" class="dlg-head__no mono" data-testid="contract-form-no">{{ form.contract_no }}</span>
          <t-tag v-else size="small" variant="outline">保存后自动生成合同号</t-tag>
        </div>
      </template>

      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <t-form label-width="96px" class="dlg-form">
        <div class="form-grid">
          <t-form-item
            label="合同名称"
            name="name"
            :status="formErrors.name ? 'error' : undefined"
            :tips="formErrors.name"
            class="form-grid__wide"
          >
            <t-input
              v-model="form.name"
              placeholder="例如：2026 年度原材料采购框架合同"
              :maxlength="MAX_CONTRACT_NAME"
              data-testid="contract-form-name"
            />
          </t-form-item>

          <t-form-item
            name="contract_type"
            :status="formErrors.contract_type ? 'error' : undefined"
            :tips="formErrors.contract_type"
          >
            <template #label>
              <span class="label-with-tip">
                合同类型
                <t-tooltip :content="TYPE_GUIDE_TIP" placement="top-right">
                  <t-icon name="help-circle" class="label-with-tip__icon" data-testid="contract-form-type-tip" />
                </t-tooltip>
              </span>
            </template>
            <!-- 四类全开（CTR-02-F1）：选「服务 / 租赁」不挂订单，下方引导行讲清判定口径 -->
            <t-select
              v-model="form.contract_type"
              :options="formTypeOptions"
              placeholder="请选择合同类型"
              data-testid="contract-form-type"
              @change="handleTypeChange"
            />
          </t-form-item>

          <t-form-item
            :label="partyBLabel"
            name="party_b_id"
            :status="formErrors.party_b_id ? 'error' : undefined"
            :tips="formErrors.party_b_id"
          >
            <!-- 乙方随类型切换数据源：SALES → 客户；PURCHASE / SERVICE / LEASE → 供应商 -->
            <div class="party-b-field">
              <t-select
                v-model="form.party_b_id"
                class="party-b-field__select"
                :options="partyBOptions"
                :placeholder="`选择${partyBLabel}`"
                filterable
                clearable
                data-testid="contract-form-party-b"
              />
              <span
                v-if="partyBTip"
                class="form-hint"
                data-testid="contract-form-party-b-tip"
              >{{ partyBTip }}</span>
            </div>
          </t-form-item>

          <!-- 类型选择引导（四行判定表）：类型已签冻结，选错只能终止重签 -->
          <div class="type-guide form-grid__wide" data-testid="contract-type-guide">
            <div class="type-guide__head">
              合同类型怎么选
              <span class="type-guide__warn">签订后类型冻结，选错只能终止重签</span>
            </div>
            <ul class="type-guide__list">
              <li
                v-for="guide in TYPE_GUIDE"
                :key="guide.type"
                class="type-guide__row"
                :data-testid="`contract-type-guide-${guide.type}`"
              >
                <b class="type-guide__type">{{ guide.label }}</b>
                <span class="type-guide__when">{{ guide.when }}</span>
              </li>
            </ul>
          </div>

          <t-form-item
            label="合同金额"
            name="amount"
            :status="formErrors.amount ? 'error' : undefined"
            :tips="formErrors.amount"
          >
            <t-input-number
              v-model="form.amount"
              theme="normal"
              :min="0"
              :decimal-places="2"
              placeholder="含税总额"
              data-testid="contract-form-amount"
            />
            <span class="form-suffix">元（含税）</span>
          </t-form-item>

          <t-form-item
            label="签订日期"
            name="sign_date"
            :status="formErrors.sign_date ? 'error' : undefined"
            :tips="formErrors.sign_date"
          >
            <t-date-picker
              v-model="form.sign_date"
              format="YYYY-MM-DD"
              placeholder="签订日期"
              data-testid="contract-form-sign-date"
            />
          </t-form-item>

          <t-form-item
            label="有效期起"
            name="start_date"
            :status="formErrors.start_date ? 'error' : undefined"
            :tips="formErrors.start_date"
          >
            <t-date-picker
              v-model="form.start_date"
              format="YYYY-MM-DD"
              placeholder="生效日"
              data-testid="contract-form-start-date"
            />
          </t-form-item>

          <t-form-item
            label="有效期止"
            name="end_date"
            :status="formErrors.end_date ? 'error' : undefined"
            :tips="formErrors.end_date"
          >
            <t-date-picker
              v-model="form.end_date"
              format="YYYY-MM-DD"
              placeholder="到期日"
              data-testid="contract-form-end-date"
            />
          </t-form-item>

          <t-form-item label="备注" name="remark" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark" class="form-grid__wide">
            <t-textarea
              v-model="form.remark"
              :autosize="{ minRows: 2, maxRows: 4 }"
              :maxlength="MAX_CONTRACT_REMARK"
              placeholder="付款条件、验收口径等补充说明"
              data-testid="contract-form-remark"
            />
          </t-form-item>
        </div>
      </t-form>

      <div class="dlg-foot-note" data-testid="contract-form-note">
        校验口径与后端一致：名称必填、乙方必选、金额 &gt; 0、有效期止 ≥ 起、签订日期 ≤ 有效期起。
        <span class="muted">（合同表没有税率 / 付款条件列，金额按含税总额录入）</span>
      </div>

      <div class="dlg-footer">
        <t-button variant="outline" data-testid="btn-contract-cancel" @click="formVisible = false">取消</t-button>
        <t-button
          v-privilege="[form.id ? PERM.update : PERM.create]"
          theme="primary"
          :loading="saving"
          data-testid="btn-contract-save"
          @click="save"
        >
          保存
        </t-button>
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="合同详情"
      size="720px"
      :footer="false"
      data-testid="contract-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail" data-testid="contract-detail">
        <div class="detail__head">
          <span class="detail__no mono" data-testid="contract-detail-no">{{ detail.contract_no }}</span>
          <StatusBadge domain="erp-contract" :status="detail.status" data-testid="contract-detail-status" />
          <!-- 「可被采购单引用」只对挂订单的两类有意义：服务 / 租赁按口径不挂订单，
               这里不能挂一个「可被采购单引用」的绿标（会误导） -->
          <template v-if="!isNoOrderContract">
            <t-tag v-if="detail.usable_for_order" size="small" variant="outline" theme="success">可被采购单引用</t-tag>
            <t-tag v-else size="small" variant="outline">不可被采购单引用</t-tag>
          </template>
          <span class="detail__spacer" />
          <t-button
            v-if="canEditContract(detail)"
            v-privilege="[PERM.update]"
            size="small"
            variant="outline"
            data-testid="btn-contract-detail-edit"
            @click="openEditDialog(detail)"
          >
            编辑
          </t-button>
          <t-button size="small" variant="outline" data-testid="btn-contract-detail-close" @click="detailVisible = false">
            关闭
          </t-button>
          <!-- PDF-08：下载 PDF（合同条款 + 签章位 + 附件清单） -->
          <DocPdfButton
            :path="`/api/business/erp/contract/${detail.id}/pdf`"
            :filename="`合同-${detail.contract_no}.pdf`"
            testid="contract-detail-pdf"
          />
        </div>

        <div class="detail__grid">
          <div class="detail-field">
            <span class="detail-field__label">合同名称</span>
            <span class="detail-field__value" data-testid="contract-detail-name">{{ detail.name }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">合同类型</span>
            <span class="detail-field__value">{{ contractTypeLabel(detail.contract_type) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">{{ contractPartyBLabel(detail.contract_type) }}</span>
            <span class="detail-field__value">{{ contractPartyBName(detail) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">签约时乙方快照</span>
            <span class="detail-field__value muted">{{ detail.party_b_name || '—' }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">合同金额（含税）</span>
            <span class="detail-field__value money" data-testid="contract-detail-amount">{{ contractAmountText(detail.amount) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">币种</span>
            <span class="detail-field__value">{{ detail.currency_code }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">签订日期</span>
            <span class="detail-field__value">{{ detail.sign_date || '—' }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">有效期</span>
            <span class="detail-field__value">{{ contractPeriodText(detail.start_date, detail.end_date) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">甲方</span>
            <span class="detail-field__value">{{ detail.party_a_name || '本组织' }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">负责人</span>
            <!-- AUD-02：原来 `shortId(owner_id)` 显示截断 UUID，现在用后端 join 的姓名 -->
            <span class="detail-field__value" data-testid="contract-detail-owner">{{
              detail.owner_name || '—'
            }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">创建时间</span>
            <span class="detail-field__value muted">{{ formatDateTime(detail.created_at) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">最后更新</span>
            <span class="detail-field__value muted">{{ formatDateTime(detail.updated_at) }}</span>
          </div>
          <div class="detail-field detail-field--wide">
            <span class="detail-field__label">备注</span>
            <span class="detail-field__value">{{ detail.remark || '—' }}</span>
          </div>
        </div>

        <!-- 状态流转：按钮由 `next_statuses`（后端状态机下发）驱动 -->
        <div class="section-title">状态流转</div>
        <div v-if="detailTransitions.length > 0" class="detail__actions" data-testid="contract-detail-actions">
          <t-button
            v-for="action in detailTransitions"
            :key="action.target"
            v-privilege="[PERM.status]"
            :theme="action.theme === 'default' ? 'primary' : action.theme"
            variant="outline"
            size="small"
            :data-testid="`btn-contract-detail-to-${action.target}`"
            @click="openStatusDialog(detail, action)"
          >
            {{ action.label }}
          </t-button>
        </div>
        <p v-else class="muted" data-testid="contract-detail-actions-empty">
          当前状态是「{{ contractStatusLabel(detail.status) }}」，状态机没有它可走的边（终态）。
        </p>

        <!-- 履约情况（CTR-02-F1）：口径由后端 `commitment_scope` 决定，前端不自己算 -->
        <template v-if="commitmentVisible">
          <div class="section-title">履约情况</div>
          <!-- 挂订单的两类：已执行 / 剩余 / 未关闭单据都是后端真值 -->
          <div
            v-if="!commitmentNotApplicable"
            class="commit"
            data-testid="contract-detail-commitment"
          >
            <div class="commit__item">
              <span class="commit__label">已执行金额</span>
              <span class="commit__value money" data-testid="contract-commitment-committed">{{ committedText }}</span>
            </div>
            <div class="commit__item">
              <span class="commit__label">剩余额度</span>
              <span
                class="commit__value money"
                :class="{ 'commit__value--neg': remainingNegative }"
                data-testid="contract-commitment-remaining"
              >{{ remainingText }}</span>
            </div>
            <div class="commit__item">
              <span class="commit__label">未关闭单据</span>
              <span class="commit__value" data-testid="contract-commitment-open-count">{{ openOrderCount }} 张</span>
            </div>
            <p
              v-if="openOrderNosText"
              class="commit__nos"
              data-testid="contract-commitment-open-nos"
            >
              单号：{{ openOrderNosText }}
            </p>
            <p class="commit__note muted">
              已执行金额 = 这张合同关联的采购单 / 销售单价税合计（后端现算），与「能不能再下单」的额度守卫同一口径。
            </p>
          </div>
          <!-- 服务 / 租赁：不显示零值金额（会被误读成「一分没执行」） -->
          <p
            v-else
            class="muted commit-na"
            data-testid="contract-detail-commitment-na"
          >{{ COMMITMENT_NOT_APPLICABLE_TEXT }}</p>
        </template>

        <!-- 关联采购单：反查 `GET /erp/purchase-order/page?contract_id=`。
             服务 / 租赁不挂任何订单，这块整块不渲染（否则「暂无采购单引用」会被读成「还没下单」） -->
        <template v-if="!isNoOrderContract">
          <div class="section-title">
            关联采购单<span v-if="linkedOrders.length > 0">（{{ linkedOrders.length }} 张）</span>
          </div>
          <div v-if="linkedLoading" class="muted">加载中…</div>
          <t-table
            v-else-if="linkedOrders.length > 0"
            row-key="id"
            size="small"
            :data="linkedOrders"
            :columns="linkedColumns"
            data-testid="contract-detail-linked"
          >
            <template #order_no="{ row }">{{ row.order_no }}</template>
            <template #status="{ row }">{{ row.status }}</template>
            <template #total_with_tax="{ row }">{{ contractAmountText(row.total_with_tax) }}</template>
          </t-table>
          <p v-else class="muted" data-testid="contract-detail-linked-empty">
            暂无采购单引用这张合同（合同需先走完审批、变成「已签订 / 已生效」才能被采购单引用）。
          </p>
        </template>

        <!-- 附件：合同扫描件 / 补充协议（ARCH-3 DC-17）—— 组件与另外三个单据详情页共用 -->
        <div class="section-title">附件</div>
        <DocAttachmentPanel
          :doc-type="DOC_KIND.CONTRACT"
          :doc-id="detail.id"
          :default-role="DEFAULT_ROLE_BY_DOC.CONTRACT"
          hint="合同扫描件（含签章页）/ 补充协议；不传也能保存与提交"
          testid-prefix="contract-attach"
        />
      </div>
    </t-drawer>

    <!-- ==================== ⑤ 状态流转确认弹窗 ==================== -->
    <t-dialog
      v-model:visible="statusVisible"
      width="520px"
      :close-on-overlay-click="false"
      data-testid="contract-status-dialog"
      :header="statusAction ? `${statusAction.label}：${statusRow?.contract_no ?? ''}` : '状态流转'"
      :confirm-btn="{ content: '确认', theme: 'primary', loading: statusSaving }"
      @confirm="submitStatus"
    >
      <t-alert v-if="statusError" theme="error" :message="statusError" class="dlg-alert" />
      <t-alert
        v-if="statusOpenOrderWarning"
        theme="warning"
        :message="statusOpenOrderWarning"
        class="dlg-alert"
        data-testid="contract-archive-open-orders"
      />
      <p class="status-confirm__text" data-testid="contract-status-confirm-text">
        {{ statusConfirmText }}
      </p>
      <t-form v-if="statusAction?.needReason" label-width="72px">
        <t-form-item
          label="终止原因"
          :status="statusReasonError ? 'error' : undefined"
          :tips="statusReasonError"
        >
          <t-textarea
            v-model="statusReason"
            :autosize="{ minRows: 3, maxRows: 5 }"
            :maxlength="500"
            placeholder="必填：终止会写进状态日志与合同备注"
            data-testid="contract-status-reason"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 合同管理（A5）：列表 + 新建/编辑 + 状态流转 + 详情抽屉。
 *
 * # 口径从哪来
 *
 * - 字段与会话契约：`backend/src/business/erp/contract_dto.rs`（A2）；
 * - 状态与合法边：`backend/src/business/shared/state_machine.rs::CONTRACT_EDGES`；
 * - 列表行的 `next_statuses` / `can_edit` 由后端详情下发，**有就不用前端再判一遍**。
 *
 * # 卡面 → 实际的四处出入（以已落库的 DDL/代码为准，卡里已报告）
 *
 * | 卡面 | 实际 |
 * |---|---|
 * | 5 态含 `CANCELLED`、DRAFT → [生效][取消] | 无 `CANCELLED`；R-P1-1 后是 **6 态**，DRAFT → PENDING_APPROVAL（提交审批）→ 审批通过才 SIGNED；草稿作废走删除 |
 * | 金额 ≥ 0 | `amount > 0`（`ck_biz_contract_amount`），0 元合同后端 400 |
 * | 税率 / 付款条件字段 | DDL 无 `tax_rate` / `payment_terms` 列，表单不提供 |
 * | `meta.auth` | 本仓 `meta.permission`（`router/index.ts` 的 RouteMeta 只有 `permission`） |
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  changeContractStatus,
  createContract,
  deleteContract,
  fetchContractDetail,
  fetchContractPage,
  fetchContractPurchaseOrders,
  fetchCustomerPage,
  fetchSupplierOptions,
  updateContract,
  type ContractDetail,
  type ContractRow,
  type Customer,
  type PurchaseOrderRow,
  type Supplier,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import DocPdfButton from '@/components/DocPdfButton.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import { todayString } from '@/views/business/erp/shared/money';
import {
  COMMITMENT_NOT_APPLICABLE_TEXT,
  CONTRACT_PAGE_SIZE,
  CONTRACT_STATUS_FILTER_OPTIONS,
  CONTRACT_TYPE_GUIDE as TYPE_GUIDE,
  CONTRACT_TYPE_GUIDE_TIP as TYPE_GUIDE_TIP,
  MAX_CONTRACT_NAME,
  MAX_CONTRACT_REMARK,
  buildContractQuery,
  buildCreatePayload,
  buildStatusPayload,
  buildUpdatePayload,
  canDeleteContract,
  canEditContract,
  commitmentViewOf,
  contractAmountText,
  contractEmptyState,
  contractFormFromDetail,
  contractListState,
  contractPartyBKind,
  contractPartyBLabel,
  contractPartyBName,
  contractPartyBTip,
  contractPeriodText,
  contractStatusLabel,
  contractTransitions,
  contractTypeLabel,
  contractTypeOptions,
  emptyContractFilter,
  emptyContractForm,
  formErrorMessage,
  isContractFiltered,
  isNoOrderContractType,
  openOrderWarning,
  transitionConfirmText,
  validateContractForm,
  type ContractFilterState,
  type ContractFormErrors,
  type ContractFormState,
  type ContractTransition,
} from './contract-logic';

/** 本页权限码（P2-4 已统一到 `business:erp:contract:*`，与 ERP 其余码同族） */
const PERM = {
  list: 'business:erp:contract:list',
  create: 'business:erp:contract:create',
  update: 'business:erp:contract:update',
  remove: 'business:erp:contract:delete',
  status: 'business:erp:contract:status',
} as const;

/** 主数据下拉一次拉多少（种子规模下够用） */
const OPTION_LIMIT = 200;

/** 表格列（`colKey` 与 `ContractRow` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  { colKey: 'contract_no', title: '合同号', width: 170 },
  { colKey: 'name', title: '合同名称', minWidth: 160, ellipsis: true },
  { colKey: 'contract_type', title: '类型', width: 90 },
  { colKey: 'party_b_name', title: '乙方', width: 150, ellipsis: true },
  { colKey: 'amount', title: '合同金额（含税）', width: 140, align: 'right' },
  { colKey: 'sign_date', title: '签订日期', width: 104 },
  { colKey: 'period', title: '有效期', width: 180 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 230, fixed: 'right' },
];

/** 详情抽屉里「关联采购单」的列 */
const linkedColumns: PrimaryTableCol[] = [
  { colKey: 'order_no', title: '采购单号', minWidth: 170 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'total_with_tax', title: '价税合计', width: 130, align: 'right' },
];

/** 状态下拉（`''` = 全部） */
const statusFilterOptions = CONTRACT_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

/** 类型下拉（筛选区给全部 4 类，后端 `CONTRACT_TYPES` 过滤本来就支持四类） */
const typeFilterOptions = contractTypeOptions().map((item) => ({
  label: item.label,
  value: item.value,
}));

/** 表单里的类型下拉（四类全可选，CTR-02-F1） */
const formTypeOptions = contractTypeOptions();

// ==================== 列表 ====================

/** 筛选行 */
const filter = reactive<ContractFilterState>(emptyContractFilter());
/** 当前页 */
const current = ref(1);
/** 每页条数 */
const pageSize = CONTRACT_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');
/** 服务端响应（行 + 总数） */
const pageData = ref<{ list: ContractRow[]; total: number }>({ list: [], total: 0 });

/** 列表区状态（行数 / 总数 / 空态判定，逻辑在 contract-logic.ts，单测覆盖） */
const listState = computed(() => contractListState(pageData.value));

/** 是否处于筛选态（决定空态文案） */
const filtered = computed(() => isContractFiltered(filter));

/** 空态文案 */
const emptyState = computed(() => contractEmptyState(filtered.value));

/** 分页器（服务端分页） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: listState.value.total,
  showJumper: true,
  showPageSize: false,
}));

/** 乙方下拉：供应商（PURCHASE）与客户（SALES）两套数据源，按表单类型切换 */
const suppliers = ref<Supplier[]>([]);
const customers = ref<Customer[]>([]);
const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: `${item.supplier_name}（${item.supplier_code}）`, value: item.id })),
);
const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id })),
);
/** 当前表单类型对应的乙方候选（SALES → 客户；PURCHASE / SERVICE / LEASE → 供应商） */
const partyBOptions = computed(() =>
  contractPartyBKind(form.contract_type) === 'CUSTOMER' ? customerOptions.value : supplierOptions.value,
);
/** 乙方称谓（「乙方供应商 / 乙方客户 / 乙方服务商 / 乙方出租方」）—— label / placeholder / 校验文案同源 */
const partyBLabel = computed(() => contractPartyBLabel(form.contract_type));
/** 服务 / 租赁才显示「服务商 / 出租方在系统中登记为供应商」这行说明 */
const partyBTip = computed(() => contractPartyBTip(form.contract_type));

/**
 * 切换合同类型时清空乙方：供应商 id 与客户 id 是两个 id 空间，
 * 留着上一个类型选中的 id 会让「乙方」看起来选好了、实际指向另一张主档（提交才 400）。
 */
function handleTypeChange(): void {
  form.party_b_id = '';
  formErrors.value.party_b_id = undefined;
}
/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表 */
async function loadContracts(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchContractPage(buildContractQuery(filter, page, pageSize));
    const rows = data?.list ?? [];
    const total = data?.total ?? 0;
    pageData.value = { list: rows, total };
    const rawPage = data?.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    pageData.value = { list: [], total: 0 };
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 主数据（失败不挡列表：下拉为空时页面还能看单） */
async function loadBasics(): Promise<void> {
  // 两套乙方主档一起拉：切换类型时不用等一次网络往返
  const [supplierResult, customerResult] = await Promise.allSettled([
    fetchSupplierOptions(OPTION_LIMIT),
    fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: OPTION_LIMIT }),
  ]);
  suppliers.value = supplierResult.status === 'fulfilled' ? supplierResult.value : [];
  customers.value = customerResult.status === 'fulfilled' ? (customerResult.value.list ?? []) : [];
}

/** 查询（回第一页） */
function handleSearch(): void {
  void loadContracts(1);
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptyContractFilter());
  void loadContracts(1);
}

/** 刷新（保留筛选与页码） */
function refresh(): void {
  void loadContracts();
  void loadBasics();
}

/** 翻页 */
function handlePageChange(pageInfo: { current?: number }): void {
  void loadContracts(pageInfo.current ?? 1);
}

// ==================== 表单 ====================

/** 弹窗可见 */
const formVisible = ref(false);
/** 保存中 */
const saving = ref(false);
/** 提交错误（后端文案） */
const submitError = ref('');
/** 表单 */
const form = reactive<ContractFormState>(emptyContractForm());
/** 校验结果（点过保存才显示） */
const formErrors = ref<ContractFormErrors>({});

/** 默认日期：签订 = 生效 = 今天，到期 = 一年后（草稿也能先建后补，但别让用户每次手点） */
function applyDefaultDates(): void {
  const today = todayString();
  const next = new Date(`${today}T00:00:00Z`);
  next.setUTCFullYear(next.getUTCFullYear() + 1);
  form.sign_date = today;
  form.start_date = today;
  form.end_date = next.toISOString().slice(0, 10);
}

/** 新建 */
function openCreateDialog(): void {
  Object.assign(form, emptyContractForm());
  applyDefaultDates();
  formErrors.value = {};
  submitError.value = '';
  formVisible.value = true;
}

/** 编辑（拉最新详情；非 DRAFT 直接拦） */
async function openEditDialog(row: { id: string; status: string }): Promise<void> {
  if (!canEditContract(row)) {
    MessagePlugin.warning('只有草稿可以编辑');
    return;
  }
  formErrors.value = {};
  submitError.value = '';
  formVisible.value = true;
  try {
    const detail = await fetchContractDetail(row.id);
    Object.assign(form, contractFormFromDetail(detail));
  } catch (error) {
    formVisible.value = false;
    MessagePlugin.error(messageOf(error));
  }
}

/** 保存（新建 / 编辑；只落草稿，状态流转单独走） */
async function save(): Promise<void> {
  formErrors.value = validateContractForm(form);
  const message = formErrorMessage(formErrors.value);
  if (message) {
    MessagePlugin.warning(message);
    return;
  }

  const isEdit = Boolean(form.id);
  saving.value = true;
  submitError.value = '';
  try {
    if (isEdit) {
      const detail = await updateContract(form.id, buildUpdatePayload(form));
      MessagePlugin.success(`${detail.contract_no} 已保存`);
    } else {
      const detail = await createContract(buildCreatePayload(form));
      MessagePlugin.success(`${detail.contract_no} 已创建为草稿`);
    }
    formVisible.value = false;
    await loadContracts(isEdit ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 详情 ====================

/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情加载中 */
const detailLoading = ref(false);
/** 详情错误 */
const detailError = ref('');
/** 详情 */
const detail = ref<ContractDetail | null>(null);
/** 关联采购单 */
const linkedOrders = ref<PurchaseOrderRow[]>([]);
/** 关联采购单加载中 */
const linkedLoading = ref(false);

/** 详情里的可用流转（**优先用后端 `next_statuses`**） */
const detailTransitions = computed<ContractTransition[]>(() =>
  detail.value ? contractTransitions(detail.value.status, detail.value.next_statuses) : [],
);

/**
 * 履约区（CTR-02-F1）：
 * - `commitmentVisible=false` → 字段缺失 / 未知口径，整块不渲染（降级，不抛错）；
 * - `commitmentNotApplicable=true` → 服务 / 租赁，只渲染一行灰字，**不显示零值金额**。
 *
 * 取值全部经 `commitmentViewOf` 收口（单测覆盖），模板里不写字符串比较。
 */
const commitmentView = computed(() => commitmentViewOf(detail.value));
const commitmentVisible = computed(() => commitmentView.value.kind !== 'HIDDEN');
const commitmentNotApplicable = computed(() => commitmentView.value.kind === 'NOT_APPLICABLE');
const committedText = computed(() =>
  commitmentView.value.kind === 'ORDER_LINKED' ? commitmentView.value.committedText : '',
);
const remainingText = computed(() =>
  commitmentView.value.kind === 'ORDER_LINKED' ? commitmentView.value.remainingText : '',
);
const remainingNegative = computed(() =>
  commitmentView.value.kind === 'ORDER_LINKED' ? commitmentView.value.remainingNegative : false,
);
const openOrderCount = computed(() =>
  commitmentView.value.kind === 'ORDER_LINKED' ? commitmentView.value.openOrderCount : 0,
);
const openOrderNosText = computed(() =>
  commitmentView.value.kind === 'ORDER_LINKED' ? commitmentView.value.openOrderNos.join('、') : '',
);

/** 服务 / 租赁：不挂订单（「关联采购单」区块与「可被采购单引用」标记都不该出现） */
const isNoOrderContract = computed(() => isNoOrderContractType(detail.value?.contract_type));

/** 反查引用了这张合同的采购单（A3 给采购单加了 `contract_id` 筛选） */
async function loadLinkedOrders(contractId: string): Promise<void> {
  linkedLoading.value = true;
  try {
    const data = await fetchContractPurchaseOrders(contractId);
    linkedOrders.value = data?.list ?? [];
  } catch {
    // 反查只是附加信息：没有采购单权限 / 拉不到时给空态，不把详情打成错误
    linkedOrders.value = [];
  } finally {
    linkedLoading.value = false;
  }
}

/** 打开详情 */
async function openDetail(row: { id: string }): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  linkedOrders.value = [];
  try {
    const loaded = await fetchContractDetail(row.id);
    detail.value = loaded;
    // 服务 / 租赁不挂采购单：不发这条反查（块本身也不渲染）
    if (!isNoOrderContractType(loaded.contract_type)) {
      void loadLinkedOrders(row.id);
    }
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

/** 删除（仅 DRAFT；被引用时后端 409 + 66018，文案会点名采购单） */
function confirmRemove(row: ContractRow): void {
  if (!canDeleteContract(row)) {
    MessagePlugin.warning('只有草稿可以删除');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除合同',
    theme: 'warning',
    body: `确定删除 ${row.contract_no}（${row.name}）吗？删除是软删，合同号不再复用。`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteContract(row.id);
        MessagePlugin.success(`${row.contract_no} 已删除`);
        await loadContracts();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 状态流转 ====================

/** 流转弹窗可见 */
const statusVisible = ref(false);
/** 流转中的合同（带 `contract_type`：确认文案要按「挂不挂订单」换一套说法） */
const statusRow = ref<{ id: string; contract_no: string; status: string; contract_type?: string; open_order_count?: number | null } | null>(null);
/** 本次动作 */
const statusAction = ref<ContractTransition | null>(null);
/**
 * 本次归档（`ACTIVE → COMPLETED`）的未关闭单据数（E2E07-FIX-4 ④）。
 *
 * 列表行没有 `open_order_count`（那是详情字段），所以点「归档」时按需拉一次详情回填；
 * 拉不到就按 0 处理（提示只是附加信息，不能把流转挡死）。
 */
const statusOpenOrderCount = ref(0);
/** 归档前提示文案（空串 = 不显示警示条） */
const statusOpenOrderWarning = computed(() =>
  statusVisible.value && statusAction.value
    ? openOrderWarning(statusOpenOrderCount.value, statusAction.value.target)
    : '',
);
/** 终止原因 */
const statusReason = ref('');
/** 原因校验错误 */
const statusReasonError = ref('');
/** 提交中 */
const statusSaving = ref(false);
/** 后端错误 */
const statusError = ref('');

/** 确认文案（服务 / 租赁换一套不挂订单的说法；归档带上未关闭单据数） */
const statusConfirmText = computed(() =>
  statusRow.value && statusAction.value
    ? transitionConfirmText(
        statusRow.value.contract_no,
        statusRow.value.status,
        statusAction.value,
        statusRow.value.contract_type,
        statusOpenOrderCount.value,
      )
    : '',
);

/** 打开流转弹窗 */
async function openStatusDialog(
  row: { id: string; contract_no: string; status: string; contract_type?: string; open_order_count?: number | null },
  action: ContractTransition,
): Promise<void> {
  statusRow.value = row;
  statusAction.value = action;
  statusReason.value = '';
  statusReasonError.value = '';
  statusError.value = '';
  statusOpenOrderCount.value = 0;
  statusVisible.value = true;
  // 归档前提示（E2E07-FIX-4 ④）：`ACTIVE → COMPLETED` 不硬拦在途单据（设计决策 E4），
  // 但要把「还有几张没关」摆在确认弹窗上。列表行没有这个字段 → 按需拉一次详情。
  if (action.target === 'COMPLETED' && typeof row.open_order_count !== 'number') {
    try {
      const loaded = await fetchContractDetail(row.id);
      statusOpenOrderCount.value = Number(loaded?.open_order_count ?? 0) || 0;
    } catch {
      // 拉不到只是提示缺失：不清空弹窗、不拦流转（口径同 loadLinkedOrders）
      statusOpenOrderCount.value = 0;
    }
  } else {
    statusOpenOrderCount.value = Number(row.open_order_count ?? 0) || 0;
  }
}

/** 提交流转（需要原因的两条终止边先在本地拦一道） */
async function submitStatus(): Promise<void> {
  const row = statusRow.value;
  const action = statusAction.value;
  if (!row || !action) {
    return;
  }
  const reason = statusReason.value.trim();
  if (action.needReason && !reason) {
    statusReasonError.value = '终止必须填写原因（后端 CONTRACT_REASON_EDGES 要求）';
    return;
  }
  statusReasonError.value = '';
  statusSaving.value = true;
  statusError.value = '';
  try {
    const detailAfter = await changeContractStatus(
      row.id,
      buildStatusPayload(row.status, action.target, reason),
    );
    MessagePlugin.success(`${row.contract_no} 已流转为「${contractStatusLabel(detailAfter.status)}」`);
    statusVisible.value = false;
    // 抽屉开着就同步刷新抽屉里的详情
    if (detail.value?.id === row.id) {
      detail.value = detailAfter;
    }
    await loadContracts();
  } catch (error) {
    statusError.value = messageOf(error);
  } finally {
    statusSaving.value = false;
  }
}

// ==================== 杂项 ====================

/** 时间戳 → 北京时间 `MM-DD HH:mm`（后端下发 UTC，详情里按 CST 展示，与全站口径一致） */
function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return String(value);
  }
  const cst = new Date(parsed.getTime() + 8 * 60 * 60 * 1000).toISOString();
  return `${cst.slice(5, 10)} ${cst.slice(11, 16)}`;
}

onMounted(() => {
  void loadBasics();
  void loadContracts(1);
  void applyRouteQuery();
});

/**
 * 跨页深链（A6 加的，**只加这一个入口，不动 A5 已有行为**）：
 * 采购单详情点「合同号」→ `/business/erp/contract?contract_id=<id>` → 直开这张合同的详情抽屉。
 *
 * 参数消费后立刻 `replace` 掉：刷新不该重复弹（与采购/收货页同一套约定）。
 */
async function applyRouteQuery(): Promise<void> {
  const router = useRouter();
  const route = useRoute();
  const contractId = typeof route.query.contract_id === 'string' ? route.query.contract_id : '';
  if (!contractId) {
    return;
  }
  await router.replace({ path: route.path, query: {} });
  await openDetail({ id: contractId });
}
</script>

<style scoped lang="less">
.contract-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.list-card {
  padding: 4px 0;
}

.tool-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;

  & + & {
    margin-top: 10px;
  }

  &__spacer {
    flex: 1 1 auto;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 13px;
    white-space: nowrap;
  }

  &__no {
    width: 180px;
  }

  &__name {
    width: 200px;
  }

  &__type {
    width: 150px;
  }

  &__status {
    width: 130px;
  }

  &__supplier {
    width: 220px;
  }

  &__range {
    width: 250px;
  }
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;

  &__main {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__meta {
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }

  &__tip {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.list-alert {
  margin-bottom: 12px;
}

.panel-empty {
  padding: 36px 0;
  text-align: center;

  &__icon {
    font-size: 28px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 8px 0 4px;
    font-weight: 600;
  }

  &__desc {
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }
}

.op-cell {
  display: flex;
  flex-wrap: nowrap;
  gap: 12px;
  align-items: center;
  white-space: nowrap;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-secondary);
}

.money {
  font-variant-numeric: tabular-nums;
}

.dlg-head {
  display: flex;
  align-items: center;
  gap: 10px;

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__no {
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }
}

.dlg-alert {
  margin-bottom: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px 16px;

  &__wide {
    grid-column: span 2;
  }
}

.form-suffix {
  margin-left: 8px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.label-with-tip {
  display: inline-flex;
  align-items: center;
  gap: 4px;

  &__icon {
    color: var(--td-text-color-placeholder);
    font-size: 14px;
    cursor: help;
  }
}

/* 乙方选择器 + 服务/租赁的来源说明（说明放在控件下方，不跟控件挤一行） */
.party-b-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;

  &__select {
    width: 100%;
  }
}

.form-hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.4;
}

/* 类型选择引导（四行判定表） */
.type-guide {
  padding: 10px 12px;
  margin: 2px 0 6px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;

  &__head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 6px;
    color: var(--td-text-color-primary);
    font-size: 13px;
    font-weight: 600;
  }

  &__warn {
    color: var(--td-warning-color);
    font-size: 12px;
    font-weight: 400;
  }

  &__list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  &__row {
    display: flex;
    gap: 8px;
    align-items: baseline;
    line-height: 1.7;
    font-size: 12px;
  }

  &__type {
    flex: 0 0 auto;
    color: var(--td-text-color-primary);
    font-weight: 600;
  }

  &__when {
    color: var(--td-text-color-secondary);
  }
}

/* 详情：履约情况 */
.commit {
  display: flex;
  flex-wrap: wrap;
  gap: 12px 32px;

  &__item {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__value {
    font-size: 15px;
    font-weight: 600;

    &--neg {
      color: var(--td-error-color);
    }
  }

  &__nos {
    flex: 0 0 100%;
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    word-break: break-all;
  }

  &__note {
    flex: 0 0 100%;
    margin: 0;
    font-size: 12px;
  }
}

.commit-na {
  margin: 0;
  font-size: 13px;
}

.dlg-foot-note {
  margin-top: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.dlg-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 16px;
}

.drawer-loading {
  padding: 24px 0;
  color: var(--td-text-color-secondary);
}

.detail {
  &__head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 16px;
  }

  &__no {
    font-size: 15px;
    font-weight: 600;
  }

  &__spacer {
    flex: 1 1 auto;
  }

  &__grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px 20px;
  }

  &__actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
}

.detail-field {
  display: flex;
  flex-direction: column;
  gap: 2px;

  &--wide {
    grid-column: span 2;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__value {
    font-size: 14px;
    word-break: break-all;
  }
}

.section-title {
  margin: 20px 0 10px;
  font-weight: 600;
}

.status-confirm__text {
  margin: 0 0 12px;
  line-height: 1.6;
}
</style>
