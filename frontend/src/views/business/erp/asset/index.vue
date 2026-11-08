<!--
  资产卡片页（FIN-2 / ARCH-6 §5.10）。

  这一页承载四个交互细节里的三个：
  ① 建卡表单**实时**显示「月折旧额 / 预计提完日期」（右侧预览卡，随原值 / 分类 / 购入日变化）；
  ② 从收货单建卡时金额预填并**只读**（改金额要回收货单作废重开）；
  ④ 处置弹窗必须显示「净值 / 已提月数 / 剩余可提」（决定「现在卖亏多少」）。

  页面只做展示与编排：金额比较用 `asset-logic.ts` 的四位定点工具，
  预览值不参与任何入账；建卡/变更后一律重读接口返回的数值。
-->
<template>
  <div class="asset-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="asset-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建资产
        </t-button>
        <t-button variant="outline" data-testid="asset-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="asset-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.category_id"
          class="tool-row__category"
          :options="categoryOptions"
          placeholder="全部分类"
          clearable
          filterable
          data-testid="asset-filter-category"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.use_dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部部门"
          clearable
          filterable
          data-testid="asset-filter-dept"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="编号 / 名称 / 规格（模糊）"
          clearable
          data-testid="asset-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="asset-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="asset-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 台账 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">资产卡片</span>
          <span class="panel-head__meta" data-testid="asset-meta">共 {{ total }} 张</span>
        </div>
        <span class="panel-head__tip">
          一台设备一张卡；月折旧额与净值由服务端按分类参数维护（当月增加当月不提，起提日为购入次月 1 日）
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="asset-load-error"
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
        data-testid="asset-table"
        @page-change="handlePageChange"
      >
        <template #asset_no="{ row }">
          <span class="mono" :data-testid="`asset-no-${row.asset_no}`">{{ row.asset_no }}</span>
        </template>
        <template #asset_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.asset_name }}</span>
            <span class="name-cell__sub">
              {{ row.spec || '—' }} · {{ row.category_code }}
            </span>
          </div>
        </template>
        <template #original_value="{ row }">
          <span class="money">{{ formatMoney(row.original_value) }}</span>
        </template>
        <template #monthly_depreciation="{ row }">
          <span class="money">{{ formatMoney(row.monthly_depreciation) }}</span>
        </template>
        <template #accumulated_depreciation="{ row }">
          <span class="money">{{ formatMoney(row.accumulated_depreciation) }}</span>
        </template>
        <template #net_value="{ row }">
          <span class="money money--strong">{{ formatMoney(row.net_value) }}</span>
        </template>
        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="assetStatusTheme(row.status)"
            :data-testid="`asset-status-${row.asset_no}`"
          >
            {{ row.status_label || assetStatusLabel(row.status) }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`asset-btn-detail-${row.asset_no}`" @click="openDetail(row)">
              详情
            </t-link>
            <t-link
              v-if="canEditAsset(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`asset-btn-edit-${row.asset_no}`"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="canTransferAsset(row)"
              v-privilege="[PERM.transfer]"
              theme="primary"
              :data-testid="`asset-btn-transfer-${row.asset_no}`"
              @click="openTransfer(row)"
            >
              转移
            </t-link>
            <t-link
              v-if="canTransferAsset(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`asset-btn-custodian-${row.asset_no}`"
              @click="openCustodian(row)"
            >
              换人
            </t-link>
            <t-link
              v-if="canIdleAsset(row)"
              v-privilege="[PERM.update]"
              theme="warning"
              :data-testid="`asset-btn-idle-${row.asset_no}`"
              @click="openStatusChange(row, 'IDLE')"
            >
              停用
            </t-link>
            <t-link
              v-if="canEnableAsset(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`asset-btn-enable-${row.asset_no}`"
              @click="openStatusChange(row, 'IN_USE')"
            >
              启用
            </t-link>
            <t-link
              v-if="canTransferAsset(row)"
              v-privilege="[PERM.dispose]"
              theme="warning"
              :data-testid="`asset-btn-dispose-${row.asset_no}`"
              @click="openDispose(row)"
            >
              处置
            </t-link>
            <t-link
              v-if="canCancelDispose(row)"
              v-privilege="[PERM.dispose]"
              theme="warning"
              :data-testid="`asset-btn-cancel-dispose-${row.asset_no}`"
              @click="openCancelDispose(row)"
            >
              作废处置
            </t-link>
            <t-link
              v-if="canDeleteAsset(row)"
              v-privilege="[PERM.remove]"
              theme="danger"
              :data-testid="`asset-btn-delete-${row.asset_no}`"
              @click="confirmDelete(row)"
            >
              删除
            </t-link>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 建卡 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑资产卡片' : '新建资产卡片'"
      width="1000px"
      :confirm-btn="{ content: editingId ? '保存' : '建卡并生成购入凭证', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      data-testid="asset-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <div class="form-layout">
        <div class="form-layout__main">
          <t-form :data="form" label-width="110px" colon>
            <t-row :gutter="[16, 0]">
              <t-col :span="12">
                <t-form-item label="资产名称" :help="formErrors.asset_name" :status="formStatus('asset_name')">
                  <t-input
                    v-model="form.asset_name"
                    placeholder="如：会议室投影仪"
                    :maxlength="200"
                    data-testid="asset-form-name"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="资产分类" :help="formErrors.category_id" :status="formStatus('category_id')">
                  <t-select
                    v-model="form.category_id"
                    :options="categoryOptions"
                    placeholder="选择分类（带出年限与残值率）"
                    filterable
                    :disabled="!!editingId"
                    data-testid="asset-form-category"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="使用部门" :help="formErrors.use_dept_id" :status="formStatus('use_dept_id')">
                  <t-select
                    v-model="form.use_dept_id"
                    :options="deptOptions"
                    placeholder="选择使用部门"
                    filterable
                    :disabled="!!editingId"
                    data-testid="asset-form-dept"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="使用人">
                  <t-select
                    v-model="form.custodian_id"
                    :options="userOptions"
                    placeholder="可选（保管人）"
                    filterable
                    clearable
                    :disabled="!!editingId"
                    data-testid="asset-form-custodian"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="规格型号">
                  <t-input v-model="form.spec" placeholder="可选" :disabled="!!editingId" />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="存放地点">
                  <t-input v-model="form.location" placeholder="可选" />
                </t-form-item>
              </t-col>

              <t-col v-if="!editingId" :span="12">
                <t-form-item label="来源">
                  <t-radio-group v-model="form.source_type" data-testid="asset-form-source" @change="handleSourceSwitch">
                    <t-radio value="MANUAL">手工录入</t-radio>
                    <t-radio value="STOCK_IN">收货单转入</t-radio>
                  </t-radio-group>
                </t-form-item>
              </t-col>
              <t-col v-if="!editingId && form.source_type === 'STOCK_IN'" :span="12">
                <t-form-item label="收货单" :help="formErrors.source_id" :status="formStatus('source_id')">
                  <t-select
                    v-model="form.source_id"
                    :options="stockInOptions"
                    :loading="stockInLoading"
                    placeholder="选择已过账收货单（金额将预填并锁定）"
                    filterable
                    data-testid="asset-form-stockin"
                    @change="handleStockInChange"
                  />
                </t-form-item>
              </t-col>

              <t-col :span="12">
                <t-form-item
                  label="原值"
                  :help="formErrors.original_value || amountLockedHint"
                  :status="formStatus('original_value')"
                >
                  <t-input
                    v-model="form.original_value"
                    placeholder="可抵扣时填不含税价"
                    :disabled="amountLocked"
                    data-testid="asset-form-original"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item
                  label="可抵扣进项税"
                  :help="formErrors.tax_amount"
                  :status="formStatus('tax_amount')"
                >
                  <t-input
                    v-model="form.tax_amount"
                    placeholder="只用于凭证拆分，不减少原值"
                    :disabled="amountLocked"
                    data-testid="asset-form-tax"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item
                  label="购入日期"
                  :help="formErrors.purchase_date"
                  :status="formStatus('purchase_date')"
                >
                  <t-date-picker
                    v-model="form.purchase_date"
                    format="YYYY-MM-DD"
                    clearable
                    :disabled="!!editingId"
                    data-testid="asset-form-purchase-date"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="付款方式">
                  <t-select
                    v-model="form.pay_mode"
                    :options="payModeOptions"
                    :disabled="!!editingId"
                    data-testid="asset-form-pay-mode"
                  />
                </t-form-item>
              </t-col>
              <t-col v-if="!editingId && form.source_type === 'MANUAL'" :span="12">
                <t-form-item label="供应商">
                  <t-select
                    v-model="form.supplier_id"
                    :options="supplierOptions"
                    placeholder="可选"
                    filterable
                    clearable
                    data-testid="asset-form-supplier"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="24">
                <t-form-item label="备注">
                  <t-textarea v-model="form.remark" :maxlength="2000" placeholder="可选" />
                </t-form-item>
              </t-col>
            </t-row>
          </t-form>
        </div>

        <!-- ★ 交互细节①：实时月折旧 / 预计提完日期 -->
        <aside class="preview" data-testid="asset-preview">
          <div class="preview__title">
            <t-icon name="chart" />
            折旧预览
          </div>
          <template v-if="preview">
            <div class="preview__hero">
              <div class="preview__hero-label">月折旧额</div>
              <div class="preview__hero-value" data-testid="asset-preview-monthly">
                {{ formatMoney(preview.monthly_depreciation) }}
              </div>
              <div class="preview__hero-sub">
                共 {{ preview.months }} 个月，尾差月 {{ formatMoney(preview.last_month_amount) }}
              </div>
            </div>
            <dl class="preview__list">
              <div>
                <dt>起提日</dt>
                <dd data-testid="asset-preview-start">{{ preview.start_date }}</dd>
              </div>
              <div>
                <dt>预计提完</dt>
                <dd class="preview__finish" data-testid="asset-preview-finish">{{ preview.finish_date }}</dd>
              </div>
              <div>
                <dt>预计净残值</dt>
                <dd>{{ formatMoney(preview.residual_value) }}</dd>
              </div>
              <div>
                <dt>可折旧总额</dt>
                <dd>{{ formatMoney(preview.depreciable_base) }}</dd>
              </div>
              <div>
                <dt>最后计提期间</dt>
                <dd>{{ preview.last_period }}</dd>
              </div>
            </dl>
            <p class="preview__note">
              口径：round((原值 × (1 − 残值率)) / 月数, 4)；购入次月 1 日起提，当月增加当月不提。
              入账金额以建卡后接口返回的月折旧额为准。
            </p>
          </template>
          <div v-else class="preview__empty" data-testid="asset-preview-empty">
            选择分类、填入原值与购入日期后，这里实时显示月折旧额与预计提完日期。
          </div>
        </aside>
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="920px"
      :header="detail ? `${detail.asset.asset_no} ${detail.asset.asset_name}` : '资产详情'"
      :footer="false"
      data-testid="asset-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <template v-if="detail">
          <t-alert
            v-if="detail.pending_disposal"
            theme="warning"
            message="该资产有在跑的处置审批，处置结果出来前不能再提交新的处置"
            class="detail-alert"
            data-testid="asset-detail-pending"
          />

          <h4 class="section-title">基本信息</h4>
          <t-descriptions :column="3" size="small" bordered>
            <t-descriptions-item label="资产编号">{{ detail.asset.asset_no }}</t-descriptions-item>
            <t-descriptions-item label="分类">{{ detail.asset.category_code }}</t-descriptions-item>
            <t-descriptions-item label="状态">
              <t-tag size="small" :theme="assetStatusTheme(detail.asset.status)">
                {{ detail.asset.status_label || assetStatusLabel(detail.asset.status) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="使用部门">{{ detail.asset.use_dept_name }}</t-descriptions-item>
            <t-descriptions-item label="使用人">{{ detail.asset.custodian_name || '—' }}</t-descriptions-item>
            <t-descriptions-item label="存放地点">{{ detail.asset.location || '—' }}</t-descriptions-item>
            <t-descriptions-item label="原值">{{ formatMoney(detail.asset.original_value) }}</t-descriptions-item>
            <t-descriptions-item label="累计折旧">
              {{ formatMoney(detail.asset.accumulated_depreciation) }}
            </t-descriptions-item>
            <t-descriptions-item label="净值">
              {{ formatMoney(detail.asset.net_value) }}
            </t-descriptions-item>
            <t-descriptions-item label="月折旧额">
              {{ formatMoney(detail.asset.monthly_depreciation) }}
            </t-descriptions-item>
            <t-descriptions-item label="起提日">{{ detail.asset.start_depreciation_date }}</t-descriptions-item>
            <t-descriptions-item label="已提 / 总月数">
              {{ detail.asset.depreciation_months_done }} / {{ detail.asset.useful_life_months }}
            </t-descriptions-item>
            <t-descriptions-item label="购入日期">{{ detail.asset.purchase_date }}</t-descriptions-item>
            <t-descriptions-item label="来源">
              {{ sourceLabel(detail.asset.source_type) }}{{ detail.asset.source_no ? ` ${detail.asset.source_no}` : '' }}
            </t-descriptions-item>
            <t-descriptions-item label="备注">{{ detail.asset.remark || '—' }}</t-descriptions-item>
          </t-descriptions>

          <h4 class="section-title">处置决策参考</h4>
          <div class="summary-grid">
            <div class="summary-cell">
              <span class="summary-cell__label">净值</span>
              <span class="summary-cell__value" data-testid="asset-detail-net">{{ formatMoney(detail.asset.net_value) }}</span>
            </div>
            <div class="summary-cell">
              <span class="summary-cell__label">已提月数</span>
              <span class="summary-cell__value" data-testid="asset-detail-months">
                {{ detail.asset.depreciation_months_done }} / {{ detail.asset.useful_life_months }}
              </span>
            </div>
            <div class="summary-cell">
              <span class="summary-cell__label">剩余可提</span>
              <span class="summary-cell__value" data-testid="asset-detail-left">
                {{ detail.months_left }} 个月 · {{ formatMoney(detail.amount_left) }}
              </span>
            </div>
          </div>

          <t-tabs v-model="detailTab" class="detail-tabs">
            <t-tab-panel value="changes" label="变动时间线">
              <t-timeline v-if="detail.changes.length > 0" class="timeline">
                <t-timeline-item
                  v-for="change in detail.changes"
                  :key="change.id"
                  :content="change.reason"
                  :time="`${change.change_date} · ${change.operator_name || '系统'}`"
                >
                  <template #dot>
                    <span class="timeline__dot" />
                  </template>
                  <div class="timeline__title">{{ change.change_label || changeLabel(change.change_type) }}</div>
                  <div class="timeline__meta">{{ changeSummary(change) }}</div>
                </t-timeline-item>
              </t-timeline>
              <div v-else class="empty-line">暂无变动记录（建卡即为起点）</div>
            </t-tab-panel>

            <t-tab-panel value="depreciations" label="折旧明细">
              <t-table
                row-key="id"
                :data="detail.depreciations"
                :columns="depColumns"
                size="small"
                :pagination="{ pageSize: 12, total: detail.depreciations.length }"
                data-testid="asset-detail-dep-table"
              >
                <template #amount="{ row }">
                  <span class="money">{{ formatMoney(row.amount) }}</span>
                </template>
                <template #net_value_after="{ row }">
                  <span class="money">{{ formatMoney(row.net_value_after) }}</span>
                </template>
                <template #is_last_month="{ row }">
                  <t-tag v-if="row.is_last_month" size="small" theme="warning" variant="light">尾差补齐</t-tag>
                  <span v-else class="muted">—</span>
                </template>
                <template #status="{ row }">
                  <t-tag size="small" :theme="depBatchStatusTheme(row.status)" variant="light">
                    {{ depBatchStatusLabel(row.status) }}
                  </t-tag>
                </template>
              </t-table>
            </t-tab-panel>

            <t-tab-panel value="vouchers" label="凭证">
              <div class="voucher-list">
                <div class="voucher-row">
                  <span>购入凭证</span>
                  <span class="mono">{{ detail.asset.voucher_id || '—' }}</span>
                </div>
                <div class="voucher-row">
                  <span>处置凭证</span>
                  <span class="mono">{{ detail.asset.disposal_voucher_id || '—' }}</span>
                </div>
              </div>
            </t-tab-panel>

            <!--
              ATT-09：附件页签 —— 「启用（入账）」前必须有发票或验收单
              （规则 `FIXED_ASSET` + `ACTIVATE`）。没有上传入口，必传规则永远无法满足。
            -->
            <t-tab-panel value="attachments" label="附件">
              <p class="muted attach-rule">
                启用（入账）前必须上传发票或验收单（pdf / jpg / png）；停用不受此约束。
              </p>
              <DocAttachmentPanel
                :doc-type="DOC_KIND.FIXED_ASSET"
                :doc-id="detail.asset.id"
                :default-role="DEFAULT_ROLE_BY_DOC.FIXED_ASSET"
                testid-prefix="asset-attach"
                @changed="refreshAssetGate()"
              />
            </t-tab-panel>
          </t-tabs>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ==================== ⑤ 部门转移 ==================== -->
    <t-dialog
      v-model:visible="transferVisible"
      header="部门转移"
      width="520px"
      :confirm-btn="{ content: '提交转移', loading: submitting }"
      @confirm="submitTransfer"
    >
      <t-form label-width="92px">
        <t-form-item label="目标部门" :help="transferError" :status="transferError ? 'error' : undefined">
          <t-select
            v-model="transferForm.to_dept_id"
            :options="deptOptions"
            placeholder="选择目标部门"
            filterable
            data-testid="asset-transfer-dept"
          />
        </t-form-item>
        <t-form-item label="变动日期">
          <t-date-picker v-model="transferForm.change_date" format="YYYY-MM-DD" />
        </t-form-item>
        <t-form-item label="原因">
          <t-textarea v-model="transferForm.reason" :maxlength="255" placeholder="必填：为什么要转移" />
        </t-form-item>
      </t-form>
      <p class="dialog-tip">部门转移不产生凭证，只在变动时间线留痕；折旧费用科目带部门辅助，下月起按新部门归集。</p>
    </t-dialog>

    <!-- ==================== ⑥ 换使用人 ==================== -->
    <t-dialog
      v-model:visible="custodianVisible"
      header="更换使用人"
      width="520px"
      :confirm-btn="{ content: '提交变更', loading: submitting }"
      @confirm="submitCustodian"
    >
      <t-form label-width="92px">
        <t-form-item label="新使用人" :help="custodianError" :status="custodianError ? 'error' : undefined">
          <t-select
            v-model="custodianForm.to_user_id"
            :options="userOptions"
            placeholder="选择新使用人"
            filterable
            data-testid="asset-custodian-user"
          />
        </t-form-item>
        <t-form-item label="变动日期">
          <t-date-picker v-model="custodianForm.change_date" format="YYYY-MM-DD" />
        </t-form-item>
        <t-form-item label="原因">
          <t-textarea v-model="custodianForm.reason" :maxlength="255" placeholder="必填：为什么要换人" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== ⑦ 处置（★ 交互细节④） ==================== -->
    <t-dialog
      v-model:visible="disposeVisible"
      header="提交处置审批"
      width="680px"
      :confirm-btn="{ content: '提交审批', loading: submitting }"
      @confirm="submitDispose"
    >
      <t-alert
        v-if="disposeSummary"
        theme="info"
        class="dispose-summary"
        data-testid="asset-dispose-summary"
      >
        <template #message>
          <div class="dispose-summary__grid">
            <div>
              <span class="dispose-summary__label">净值</span>
              <strong data-testid="asset-dispose-net">{{ formatMoney(disposeSummary.net_value) }}</strong>
            </div>
            <div>
              <span class="dispose-summary__label">已提月数</span>
              <strong>{{ disposeSummary.months_done }} / {{ disposeSummary.useful_life_months }}</strong>
            </div>
            <div>
              <span class="dispose-summary__label">剩余可提</span>
              <strong data-testid="asset-dispose-left">
                {{ disposeSummary.months_left }} 个月 · {{ formatMoney(disposeSummary.amount_left) }}
              </strong>
            </div>
          </div>
        </template>
      </t-alert>

      <t-form label-width="92px">
        <t-form-item label="处置类型">
          <t-radio-group v-model="disposeForm.disposal_type" data-testid="asset-dispose-type">
            <t-radio value="SALE">出售</t-radio>
            <t-radio value="SCRAP">报废</t-radio>
            <t-radio value="LOSS">盘亏</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="处置日期" :help="disposeErrors.disposal_date" :status="disposeStatus('disposal_date')">
          <t-date-picker v-model="disposeForm.disposal_date" format="YYYY-MM-DD" />
        </t-form-item>
        <template v-if="disposeForm.disposal_type === 'SALE'">
          <t-form-item label="出售价" :help="disposeErrors.sale_price" :status="disposeStatus('sale_price')">
            <t-input v-model="disposeForm.sale_price" placeholder="不含税售价" data-testid="asset-dispose-price" />
          </t-form-item>
          <t-form-item label="销项税" :help="disposeErrors.output_tax" :status="disposeStatus('output_tax')">
            <t-input v-model="disposeForm.output_tax" placeholder="不填 = 0" data-testid="asset-dispose-tax" />
          </t-form-item>
          <t-form-item label="预计损益">
            <span class="gain-preview" data-testid="asset-dispose-gain">
              {{ formatMoney(disposalGainPreview(disposeSummary?.net_value ?? '0', disposeForm.sale_price)) }}
            </span>
            <span class="muted"> 不含税售价 − 净值（仅供决策参考，入账以凭证为准）</span>
          </t-form-item>
        </template>
        <t-form-item label="原因" :help="disposeErrors.reason" :status="disposeStatus('reason')">
          <t-textarea
            v-model="disposeForm.reason"
            :maxlength="255"
            placeholder="必填：为什么处置（内控留痕）"
            data-testid="asset-dispose-reason"
          />
        </t-form-item>
      </t-form>
      <p class="dialog-tip">
        提交后走「资产处置审批」流程（3 节点）：发起人不能审批自己提交的单据；审批通过才生成处置凭证并转为「已处置」。
      </p>
    </t-dialog>

    <!-- ==================== ⑧ 作废处置 ==================== -->
    <t-dialog
      v-model:visible="cancelDisposeVisible"
      header="作废处置"
      width="520px"
      :confirm-btn="{ content: '确认作废', loading: submitting, theme: 'danger' }"
      @confirm="submitCancelDispose"
    >
      <t-alert theme="warning" message="作废处置会生成红字凭证回滚原处置分录，并把资产恢复为「在役」。请填写原因。" />
      <t-form label-width="92px" class="cancel-form">
        <t-form-item label="原因" :help="cancelError" :status="cancelError ? 'error' : undefined">
          <t-textarea v-model="cancelReason" :maxlength="255" placeholder="必填" data-testid="asset-cancel-reason" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== ⑨ 停用 / 启用（一个弹窗两种语义） ==================== -->
    <t-dialog
      v-model:visible="statusChangeVisible"
      :header="statusChangeKind === 'IDLE' ? '停用资产' : '启用资产'"
      width="520px"
      :confirm-btn="{
        content: statusChangeKind === 'IDLE' ? '确认停用' : '确认启用',
        loading: submitting,
        theme: statusChangeKind === 'IDLE' ? 'warning' : 'primary',
        disabled: statusChangeKind === 'IN_USE' && !assetGate.canProceed,
      }"
      data-testid="asset-status-dialog"
      @confirm="submitStatusChange"
    >
      <t-alert
        v-if="statusChangeKind === 'IDLE'"
        theme="warning"
        message="停用后资产变为「闲置」：仍按月计提折旧（口径不变），仍可转移 / 处置；恢复使用点「启用」。"
      />
      <t-alert
        v-else
        theme="info"
        message="启用后资产回到「在役」，继续按月计提折旧。"
      />
      <!--
        ATT-09：启用 = 资产的「入账 / 生效」动作，发票或验收单必传
        （规则 `FIXED_ASSET` + `ACTIVATE`，后端 `asset_status_change` 同一条闸门）。
        缺附件时「确认启用」置灰并在这里点名缺什么；上传入口在「资产详情 → 附件」页签。
      -->
      <AttachmentGateHint
        v-if="statusChangeKind === 'IN_USE'"
        class="status-gate-hint"
        :hint="assetGate.hint"
        testid="asset-enable-gate-hint"
      />
      <t-link
        v-if="statusChangeKind === 'IN_USE' && !assetGate.canProceed && statusChangeTarget"
        theme="primary"
        class="status-gate-link"
        data-testid="asset-enable-upload-link"
        @click="openStatusUpload"
      >
        去「资产详情 → 附件」上传发票 / 验收单
      </t-link>
      <t-form label-width="92px" class="cancel-form">
        <t-form-item
          label="原因"
          :help="statusChangeError"
          :status="statusChangeError ? 'error' : undefined"
        >
          <t-textarea
            v-model="statusChangeForm.reason"
            :maxlength="255"
            placeholder="必填：为什么停用 / 启用（内控留痕）"
            data-testid="asset-status-reason"
          />
        </t-form-item>
        <t-form-item label="变动日期" help="仅留痕，不参与折旧计算">
          <t-date-picker
            v-model="statusChangeForm.change_date"
            format="YYYY-MM-DD"
            clearable
            data-testid="asset-status-date"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import { fetchStockInPage, fetchSupplierOptions, type StockInRow, type Supplier } from '@/api/business/erp';
import { DEFAULT_ROLE_BY_DOC, type DocType } from '@/api/business/erp/attachment';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import AttachmentGateHint from '@/views/business/erp/shared/AttachmentGateHint.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import { useAttachmentGate } from '@/views/business/erp/shared/use-attachment-gate';
import {
  cancelAssetDispose,
  changeAssetStatus,
  createAsset,
  custodianAsset,
  deleteAsset,
  disposeAsset,
  fetchAssetCategoryList,
  fetchAssetDetail,
  fetchAssetPage,
  transferAsset,
  updateAsset,
  ASSET_STATUSES,
  ASSET_STATUS_LABELS,
  PAY_MODES,
  PAY_MODE_LABELS,
  type AssetCategory,
  type AssetDetail,
  type AssetRow,
  type AssetStatus,
} from '@/api/business/erp/asset';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { pageUsers } from '@/api/system/user-api';
import {
  applyStockInSource,
  assetStatusLabel,
  assetStatusTheme,
  canCancelDispose,
  canDeleteAsset,
  canDisposeAsset,
  canEditAsset,
  canEnableAsset,
  canIdleAsset,
  canTransferAsset,
  changeLabel,
  changeSummary,
  clearSource,
  depBatchStatusLabel,
  depBatchStatusTheme,
  disposalGainPreview,
  disposeSummaryOf,
  emptyAssetForm,
  emptyDisposeForm,
  errorText,
  formatMoney,
  formatRate,
  isAmountLocked,
  previewFromCategory,
  requireReasonError,
  toCreatePayload,
  toDisposePayload,
  todayString,
  validateAssetForm,
  validateDisposeForm,
  type AssetFormState,
  type DepreciationPreview,
  type DisposeFormState,
  type DisposeSummary,
} from './asset-logic';

/** 本页权限码（与 V0229 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:asset:list',
  detail: 'business:erp:asset:detail',
  create: 'business:erp:asset:create',
  update: 'business:erp:asset:update',
  remove: 'business:erp:asset:delete',
  transfer: 'business:erp:asset:transfer',
  dispose: 'business:erp:asset:dispose',
} as const;

/** 每页条数（与后端 `DEFAULT_PAGE_SIZE` 同口径） */
const PAGE_SIZE = 20;
/** 下拉一次拉多少 */
const OPTION_LIMIT = 200;

const rows = ref<AssetRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<{
  keyword: string;
  status: AssetStatus | '';
  category_id: string;
  use_dept_id: string;
}>({ keyword: '', status: '', category_id: '', use_dept_id: '' });

const statusFilterOptions = ASSET_STATUSES.map((value) => ({
  label: ASSET_STATUS_LABELS[value],
  value,
}));
const payModeOptions = PAY_MODES.map((value) => ({ label: PAY_MODE_LABELS[value], value }));

const categories = ref<AssetCategory[]>([]);
const depts = ref<DeptNode[]>([]);
const users = ref<Array<{ id: string; label: string }>>([]);
const suppliers = ref<Supplier[]>([]);
const stockIns = ref<StockInRow[]>([]);
const stockInLoading = ref(false);

const categoryOptions = computed(() =>
  categories.value.map((item) => ({
    label: `${item.category_name}（${item.useful_life_months} 个月 / 残值 ${formatRateText(item.salvage_rate)}）`,
    value: item.id,
  })),
);

/** 部门树拍平成下拉（缩进表示层级） */
const deptOptions = computed(() => {
  const out: Array<{ label: string; value: string }> = [];
  const walk = (nodes: DeptNode[], depth: number): void => {
    for (const node of nodes) {
      out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
      if (node.children?.length) {
        walk(node.children, depth + 1);
      }
    }
  };
  walk(depts.value, 0);
  return out;
});

const userOptions = computed(() => users.value.map((item) => ({ label: item.label, value: item.id })));
const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: item.supplier_name ?? item.id, value: item.id })),
);
const stockInOptions = computed(() =>
  stockIns.value.map((item) => ({
    label: `${item.in_no} · ${formatMoney(item.total_amount as string)}`,
    value: item.id,
  })),
);

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽总和 = 948px：1280 视口（侧栏展开时表格可视宽实测 968px）也不会被固定「操作」列遮挡。
// 上一版合计 1020px，在 1280 下溢出 52px，固定列正好压住「状态」列（实测 statusRight 1112 > opLeft 1060）。
const columns: PrimaryTableCol[] = [
  { colKey: 'asset_no', title: '资产编号', width: 148 },
  { colKey: 'asset_name', title: '名称 / 规格 · 分类', width: 160, ellipsis: true },
  { colKey: 'use_dept_name', title: '使用部门', width: 96, ellipsis: true },
  { colKey: 'original_value', title: '原值', width: 100, align: 'right' },
  { colKey: 'monthly_depreciation', title: '月折旧', width: 92, align: 'right' },
  { colKey: 'net_value', title: '净值', width: 100, align: 'right' },
  { colKey: 'status', title: '状态', width: 68 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right' },
];

const depColumns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 88 },
  { colKey: 'amount', title: '本月折旧', width: 120, align: 'right' },
  { colKey: 'accumulated_after', title: '计提后累计', width: 130, align: 'right' },
  { colKey: 'net_value_after', title: '计提后净值', width: 130, align: 'right' },
  { colKey: 'is_last_month', title: '尾差', width: 92 },
  { colKey: 'status', title: '状态', width: 92 },
];

/** 残值率文案（下拉 label 用） */
function formatRateText(raw: string): string {
  return formatRate(raw);
}

function sourceLabel(raw: string | null | undefined): string {
  switch (raw) {
    case 'MANUAL':
      return '手工录入';
    case 'PURCHASE_ORDER':
      return '采购单';
    case 'STOCK_IN':
      return '收货单';
    default:
      return raw ? String(raw) : '—';
  }
}

// ==================== 列表 ====================

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchAssetPage({
      page: current.value,
      page_size: PAGE_SIZE,
      keyword: filter.keyword.trim(),
      status: filter.status,
      category_id: filter.category_id,
      use_dept_id: filter.use_dept_id,
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
  filter.category_id = '';
  filter.use_dept_id = '';
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 主数据 ====================

async function loadCategories(): Promise<void> {
  try {
    categories.value = await fetchAssetCategoryList();
  } catch (error) {
    MessagePlugin.warning(`资产分类加载失败：${errorText(error)}`);
  }
}

async function loadDepts(): Promise<void> {
  try {
    depts.value = await getDeptTree({ status: 1 });
  } catch (error) {
    MessagePlugin.warning(`部门树加载失败：${errorText(error)}`);
  }
}

async function loadUsers(): Promise<void> {
  try {
    const page = await pageUsers({ page: 1, page_size: OPTION_LIMIT, status: 1 });
    users.value = (page.list ?? []).map((user) => ({
      id: user.id,
      label: `${user.real_name || user.username}（${user.username}）`,
    }));
  } catch {
    users.value = [];
  }
}

async function loadSuppliers(): Promise<void> {
  try {
    suppliers.value = await fetchSupplierOptions(OPTION_LIMIT);
  } catch {
    suppliers.value = [];
  }
}

async function loadStockIns(): Promise<void> {
  if (stockIns.value.length > 0 || stockInLoading.value) {
    return;
  }
  stockInLoading.value = true;
  try {
    // 只有**已过账**的收货单能转固（草稿没有成本，作废的要重开）
    const page = await fetchStockInPage({ status: 'POSTED', page: 1, page_size: OPTION_LIMIT });
    stockIns.value = page.list ?? [];
  } catch (error) {
    MessagePlugin.warning(`收货单列表加载失败：${errorText(error)}`);
  } finally {
    stockInLoading.value = false;
  }
}

// ==================== 建卡 / 编辑 ====================

const formVisible = ref(false);
const editingId = ref('');
const form = reactive<AssetFormState>(emptyAssetForm());
const formErrors = ref<Partial<Record<keyof AssetFormState, string>>>({});
const submitting = ref(false);
const editingRow = ref<AssetRow | null>(null);

const amountLocked = computed(() => !editingId.value && isAmountLocked(form));
const amountLockedHint = computed(() =>
  amountLocked.value ? '金额来自来源单，已锁定：要改金额请回收货单作废后重开' : '',
);

const selectedCategory = computed(
  () => categories.value.find((item) => item.id === form.category_id) ?? null,
);

const preview = computed<DepreciationPreview | null>(() => {
  if (editingId.value) {
    return null;
  }
  return previewFromCategory(form, selectedCategory.value);
});

function formStatus(field: keyof AssetFormState): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

function assignForm(next: AssetFormState): void {
  Object.assign(form, next);
}

function openCreate(): void {
  editingId.value = '';
  editingRow.value = null;
  assignForm(emptyAssetForm());
  formErrors.value = {};
  void loadStockIns();
  formVisible.value = true;
}

function openEdit(row: AssetRow): void {
  editingId.value = row.id;
  editingRow.value = row;
  assignForm({
    ...emptyAssetForm(),
    asset_name: row.asset_name,
    category_id: row.category_id,
    use_dept_id: row.use_dept_id,
    custodian_id: row.custodian_id ?? '',
    spec: row.spec ?? '',
    location: row.location ?? '',
    original_value: row.original_value,
    tax_amount: row.tax_amount,
    purchase_date: row.purchase_date,
    source_type: 'MANUAL',
    pay_mode: 'CASH',
    remark: row.remark ?? '',
  });
  formErrors.value = {};
  formVisible.value = true;
}

function closeForm(): void {
  formVisible.value = false;
  submitting.value = false;
}

function handleStockInChange(value: unknown): void {
  const id = String(value ?? '');
  const picked = stockIns.value.find((item) => item.id === id);
  if (!picked) {
    return;
  }
  const next = applyStockInSource(form, {
    id: picked.id,
    in_no: picked.in_no,
    total_amount: picked.total_amount as string,
  });
  Object.assign(form, next.form);
  formErrors.value = {};
}

function handleSourceSwitch(value: unknown): void {
  if (String(value) === 'MANUAL') {
    assignForm(clearSource(form));
    formErrors.value = {};
  }
}

async function submitForm(): Promise<void> {
  if (editingId.value) {
    const error = requireReasonError(form.asset_name, '资产名称');
    if (error) {
      formErrors.value = { asset_name: error };
      return;
    }
    submitting.value = true;
    try {
      await updateAsset(editingId.value, {
        asset_name: form.asset_name.trim(),
        spec: form.spec.trim(),
        location: form.location.trim(),
        remark: form.remark,
      });
      MessagePlugin.success('已保存（金额与折旧口径不可改）');
      formVisible.value = false;
      await loadRows();
    } catch (error) {
      MessagePlugin.error(errorText(error));
    } finally {
      submitting.value = false;
    }
    return;
  }

  const errors = validateAssetForm(form);
  formErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    MessagePlugin.warning('请先修正表单里的红字项');
    return;
  }
  submitting.value = true;
  try {
    const result = await createAsset(toCreatePayload(form));
    MessagePlugin.success(`建卡成功：${result.no}${result.note ? `（${result.note}）` : ''}`);
    formVisible.value = false;
    current.value = 1;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detail = ref<AssetDetail | null>(null);
const detailLoading = ref(false);
const detailTab = ref<'changes' | 'depreciations' | 'vouchers' | 'attachments'>('changes');

async function openDetail(row: AssetRow): Promise<void> {
  detailVisible.value = true;
  detailTab.value = 'changes';
  await loadDetail(row.id);
}

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  try {
    detail.value = await fetchAssetDetail(id);
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    detailLoading.value = false;
  }
}

async function ensureDetail(row: AssetRow): Promise<AssetDetail | null> {
  if (detail.value?.asset.id === row.id) {
    return detail.value;
  }
  detailLoading.value = true;
  try {
    detail.value = await fetchAssetDetail(row.id);
    return detail.value;
  } catch (error) {
    MessagePlugin.error(errorText(error));
    return null;
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 转移 ====================

const transferVisible = ref(false);
const transferForm = reactive({ to_dept_id: '', change_date: '', reason: '' });
const transferError = ref('');
const transferTarget = ref<AssetRow | null>(null);

function openTransfer(row: AssetRow): void {
  transferTarget.value = row;
  transferForm.to_dept_id = '';
  transferForm.change_date = new Date().toISOString().slice(0, 10);
  transferForm.reason = '';
  transferError.value = '';
  transferVisible.value = true;
}

async function submitTransfer(): Promise<void> {
  const target = transferTarget.value;
  if (!target) {
    return;
  }
  if (!transferForm.to_dept_id) {
    transferError.value = '目标部门必填';
    return;
  }
  const reasonError = requireReasonError(transferForm.reason, '转移原因');
  if (reasonError) {
    transferError.value = reasonError;
    return;
  }
  submitting.value = true;
  try {
    detail.value = await transferAsset(target.id, {
      to_dept_id: transferForm.to_dept_id,
      change_date: transferForm.change_date || undefined,
      reason: transferForm.reason.trim(),
    });
    transferVisible.value = false;
    MessagePlugin.success('已转移（不产生凭证，已留痕）');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 换使用人 ====================

const custodianVisible = ref(false);
const custodianForm = reactive({ to_user_id: '', change_date: '', reason: '' });
const custodianError = ref('');
const custodianTarget = ref<AssetRow | null>(null);

function openCustodian(row: AssetRow): void {
  custodianTarget.value = row;
  custodianForm.to_user_id = '';
  custodianForm.change_date = new Date().toISOString().slice(0, 10);
  custodianForm.reason = '';
  custodianError.value = '';
  custodianVisible.value = true;
}

async function submitCustodian(): Promise<void> {
  const target = custodianTarget.value;
  if (!target) {
    return;
  }
  if (!custodianForm.to_user_id) {
    custodianError.value = '新使用人必填';
    return;
  }
  const reasonError = requireReasonError(custodianForm.reason, '变更原因');
  if (reasonError) {
    custodianError.value = reasonError;
    return;
  }
  submitting.value = true;
  try {
    detail.value = await custodianAsset(target.id, {
      to_user_id: custodianForm.to_user_id,
      change_date: custodianForm.change_date || undefined,
      reason: custodianForm.reason.trim(),
    });
    custodianVisible.value = false;
    MessagePlugin.success('使用人已变更（已留痕）');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 处置 ====================

const disposeVisible = ref(false);
const disposeForm = reactive<DisposeFormState>(emptyDisposeForm());
const disposeErrors = ref<Partial<Record<keyof DisposeFormState, string>>>({});
const disposeTarget = ref<AssetRow | null>(null);
const disposeSummary = ref<DisposeSummary | null>(null);

function disposeStatus(field: keyof DisposeFormState): 'error' | undefined {
  return disposeErrors.value[field] ? 'error' : undefined;
}

async function openDispose(row: AssetRow): Promise<void> {
  const loaded = await ensureDetail(row);
  if (!loaded) {
    return;
  }
  if (!canDisposeAsset(loaded.asset, loaded.pending_disposal)) {
    MessagePlugin.warning(
      loaded.pending_disposal ? '该资产已有在跑的处置审批' : '当前状态不允许提交处置',
    );
    return;
  }
  disposeTarget.value = row;
  disposeSummary.value = disposeSummaryOf(loaded);
  Object.assign(disposeForm, emptyDisposeForm());
  disposeErrors.value = {};
  disposeVisible.value = true;
}

async function submitDispose(): Promise<void> {
  const target = disposeTarget.value;
  if (!target) {
    return;
  }
  const errors = validateDisposeForm(disposeForm);
  disposeErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    MessagePlugin.warning('请先修正处置表单里的红字项');
    return;
  }
  submitting.value = true;
  try {
    detail.value = await disposeAsset(target.id, toDisposePayload(disposeForm));
    disposeVisible.value = false;
    MessagePlugin.success('处置申请已提交审批（发起人不能审批自己提交的单据）');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 作废处置 ====================

const cancelDisposeVisible = ref(false);
const cancelReason = ref('');
const cancelError = ref('');
const cancelTarget = ref<AssetRow | null>(null);

function openCancelDispose(row: AssetRow): void {
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelError.value = '';
  cancelDisposeVisible.value = true;
}

async function submitCancelDispose(): Promise<void> {
  const target = cancelTarget.value;
  if (!target) {
    return;
  }
  const error = requireReasonError(cancelReason.value, '作废原因');
  if (error) {
    cancelError.value = error;
    return;
  }
  submitting.value = true;
  try {
    detail.value = await cancelAssetDispose(target.id, cancelReason.value.trim());
    cancelDisposeVisible.value = false;
    MessagePlugin.success('已作废处置：红字回滚完成，资产恢复在役');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 停用 / 启用（IN_USE ⇄ IDLE） ====================

/** 打开的是哪一个动作：`IDLE` = 停用、`IN_USE` = 启用（一个弹窗两种语义）。 */
const statusChangeKind = ref<'IDLE' | 'IN_USE'>('IDLE');
const statusChangeVisible = ref(false);
const statusChangeForm = reactive({ reason: '', change_date: '' });
const statusChangeError = ref('');
const statusChangeTarget = ref<AssetRow | null>(null);

// ==================== 附件必传闸门（ATT-09） ====================

/**
 * 启用（`IDLE → IN_USE`）= 资产的「入账 / 生效」动作，此时发票或验收单必传
 * （规则 `FIXED_ASSET` + `ACTIVATE`，后端 `asset_status_change` 里同一条闸门）。
 *
 * 只在**启用**弹窗里查（停用与附件无关）：弹窗打开时把 `docId` 指向目标资产，
 * 关闭 / 切成停用就置 `null`（组合式函数回到初始态，不发请求、不禁用、不画红字）。
 */
const assetGateDocType = ref<DocType>('FIXED_ASSET');
const assetGateDocId = computed(() =>
  statusChangeKind.value === 'IN_USE' ? statusChangeTarget.value?.id ?? null : null,
);

const { gate: assetGate, refresh: refreshAssetGate } = useAttachmentGate({
  docType: assetGateDocType,
  docId: assetGateDocId,
  at: 'ACTIVATE',
});

/**
 * 从「启用被闸门拦住」的弹窗跳去上传：关掉弹窗 → 打开该资产详情 → 直接切到附件页签。
 * （不这么做的话，红字只说「缺发票或验收单」，用户找不到上传入口。）
 */
function openStatusUpload(): void {
  const target = statusChangeTarget.value;
  statusChangeVisible.value = false;
  if (!target) {
    return;
  }
  void openDetail(target).then(() => {
    detailTab.value = 'attachments';
  });
}


function openStatusChange(row: AssetRow, target: 'IDLE' | 'IN_USE'): void {
  statusChangeTarget.value = row;
  statusChangeKind.value = target;
  statusChangeForm.reason = '';
  statusChangeForm.change_date = todayString();
  statusChangeError.value = '';
  statusChangeVisible.value = true;
}

async function submitStatusChange(): Promise<void> {
  const target = statusChangeTarget.value;
  if (!target) {
    return;
  }
  const idling = statusChangeKind.value === 'IDLE';
  // ATT-09：启用前必须有发票或验收单（弹窗按钮已置灰，这里再兜一次底）
  if (!idling && !assetGate.value.canProceed) {
    statusChangeError.value =
      assetGate.value.hint ?? '还缺必传附件：请先在「资产详情 → 附件」上传发票或验收单';
    return;
  }
  const reasonError = requireReasonError(statusChangeForm.reason, idling ? '停用原因' : '启用原因');
  if (reasonError) {
    statusChangeError.value = reasonError;
    return;
  }
  submitting.value = true;
  try {
    const res = await changeAssetStatus(target.id, {
      target_status: statusChangeKind.value,
      reason: statusChangeForm.reason.trim(),
      change_date: statusChangeForm.change_date || undefined,
    });
    statusChangeVisible.value = false;
    detail.value = res;
    MessagePlugin.success(
      idling ? '已停用：资产转为「闲置」（折旧照提）' : '已启用：资产恢复「在役」',
    );
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 删除 ====================

function confirmDelete(row: AssetRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除资产卡片',
    body: `确认删除「${row.asset_no} ${row.asset_name}」？软删后不再出现在台账，已计提的折旧明细保留。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteAsset(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadRows();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

onMounted(async () => {
  await Promise.all([loadCategories(), loadDepts(), loadUsers(), loadSuppliers(), loadRows()]);
});
</script>

<style scoped lang="less">
.asset-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;

  &__spacer {
    flex: 1;
  }

  &__status {
    width: 120px;
  }

  &__category,
  &__dept {
    width: 168px;
  }

  &__keyword {
    width: 220px;
  }
}

.list-alert {
  margin-bottom: 12px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 12px;

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

  &__tip {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;

  &__main {
    font-weight: 500;
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.op-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 13px;
}

.money {
  font-variant-numeric: tabular-nums;

  &--strong {
    font-weight: 600;
  }
}

.muted {
  color: var(--td-text-color-placeholder);
}

.form-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 280px;
  gap: 16px;

  &__main {
    min-width: 0;
  }
}

.preview {
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  padding: 14px;
  background: var(--td-bg-color-container);
  align-self: start;
  position: sticky;
  top: 0;

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    margin-bottom: 10px;
  }

  &__hero {
    border-radius: 8px;
    padding: 12px;
    background: var(--td-brand-color-light);

    &-label {
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }

    &-value {
      font-size: 26px;
      font-weight: 700;
      line-height: 1.3;
      font-variant-numeric: tabular-nums;
    }

    &-sub {
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }
  }

  &__list {
    margin: 12px 0 0;
    display: flex;
    flex-direction: column;
    gap: 6px;

    > div {
      display: flex;
      justify-content: space-between;
      gap: 8px;
      font-size: 13px;
    }

    dt {
      color: var(--td-text-color-secondary);
    }

    dd {
      margin: 0;
      font-variant-numeric: tabular-nums;
    }
  }

  &__finish {
    font-weight: 600;
    color: var(--td-brand-color);
  }

  &__note {
    margin: 12px 0 0;
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-text-color-placeholder);
  }

  &__empty {
    font-size: 13px;
    line-height: 1.7;
    color: var(--td-text-color-placeholder);
  }
}

.section-title {
  margin: 16px 0 8px;
  font-size: 14px;
  font-weight: 600;
}

.detail-alert {
  margin-bottom: 8px;
}

.summary-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
}

.summary-cell {
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;

  &__label {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__value {
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
}

.detail-tabs {
  margin-top: 8px;
}

.timeline {
  padding-top: 8px;

  &__dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--td-brand-color);
  }

  &__title {
    font-weight: 600;
  }

  &__meta {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.empty-line {
  padding: 24px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}

.voucher-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.voucher-row {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
}

.dialog-tip {
  margin: 8px 0 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-placeholder);
}

.dispose-summary {
  margin-bottom: 12px;

  &__grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
  }

  &__label {
    display: block;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.gain-preview {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.cancel-form {
  margin-top: 12px;
}

/* ATT-09：启用弹窗里的必传红字 / 去上传的入口 / 附件页签的规则说明 */
.status-gate-hint {
  display: block;
  margin: 8px 0 0;
}

.status-gate-link {
  display: inline-block;
  margin-top: 4px;
  font-size: 12px;
}

.attach-rule {
  margin: 0 0 8px;
  font-size: 12px;
}
</style>
