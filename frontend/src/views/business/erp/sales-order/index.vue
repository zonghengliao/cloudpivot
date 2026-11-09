<template>
  <div class="sales-order-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.so_no"
          class="filter-row__keyword"
          placeholder="订单编号（模糊）"
          clearable
          data-testid="so-filter-no"
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
          data-testid="so-filter-status"
        />

        <t-select
          v-model="filter.customer_id"
          class="filter-row__customer"
          :options="customerOptions"
          placeholder="全部客户"
          clearable
          filterable
          data-testid="so-filter-customer"
        />

        <t-button theme="primary" data-testid="btn-so-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-so-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-so-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="canWrite ? '新建一张销售订单（SO-yyyyMMdd-000001）' : '当前账号没有 business:erp:sale-order:create 权限'"
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-so-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建销售订单
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">销售订单</span>
          <span class="panel-head__meta" data-testid="so-meta">共 {{ total }} 条</span>
          <t-tag v-if="isFiltered" size="small" variant="outline" data-testid="so-filtered-tag">已筛选</t-tag>
        </div>
        <span class="panel-head__hint">
          提交后走审批中心（<code>so_approve</code>：销售主管 → 金额 ≥1 万加财务经理 → ≥10 万加总经理）；
          已审批 / 部分出库可关闭，草稿 / 审批中 / 已审批可作废
        </span>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert" data-testid="so-load-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadOrders()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="so-table"
        @page-change="handlePageChange"
      >
        <template #so_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="so-no-link" @click="openDetail(row)">
            {{ row.so_no }}
          </t-link>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="customerText(row)">
            {{ customerText(row) }}
          </span>
        </template>

        <template #order_date="{ row }">
          <span class="mono">{{ row.order_date }}</span>
        </template>

        <template #delivery_date="{ row }">
          <span class="mono" :class="{ muted: !row.delivery_date }">{{ row.delivery_date || '—' }}</span>
        </template>

        <template #total_with_tax="{ row }">
          <span class="mono">{{ formatMoney(row.total_with_tax) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="salesOrderStatusTheme(row.status)"
            :data-testid="`so-status-${row.so_no}`"
          >
            {{ row.status_label || salesOrderStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="btn-so-detail" @click="openDetail(row)">详情</t-link>
            <t-link
              v-if="rowActions(row).edit"
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="btn-so-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="rowActions(row).submit"
              v-privilege="[PERM.status]"
              theme="primary"
              data-testid="btn-so-submit"
              @click="submitOrder(row)"
            >
              提交
            </t-link>
            <t-link
              v-if="rowActions(row).withdraw"
              v-privilege="[PERM.status]"
              theme="warning"
              data-testid="btn-so-withdraw"
              @click="withdrawOrder(row)"
            >
              撤回
            </t-link>
            <t-link
              v-if="rowActions(row).close"
              v-privilege="[PERM.status]"
              theme="success"
              data-testid="btn-so-close"
              @click="openReasonDialog(row, 'CLOSED')"
            >
              关闭
            </t-link>
            <t-link
              v-if="rowActions(row).cancel"
              v-privilege="[PERM.status]"
              theme="danger"
              data-testid="btn-so-cancel"
              @click="openReasonDialog(row, 'CANCELLED')"
            >
              作废
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="empty-block" data-testid="so-empty">{{ emptyListText(filter) }}</div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑销售订单' : '新建销售订单'"
      :width="1100"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: submitting, disabled: submitting }"
      :cancel-btn="{ disabled: submitting }"
      :close-on-overlay-click="false"
      data-testid="so-form-dialog"
      @confirm="submitForm"
    >
      <div class="form-body">
        <t-form label-width="96px" class="form-head">
          <t-row :gutter="[16, 0]">
            <t-col :span="6">
              <t-form-item label="客户" required-mark>
                <t-select
                  v-model="form.customer_id"
                  :options="customerOptions"
                  filterable
                  placeholder="选择客户（只列正常状态）"
                  data-testid="so-form-customer"
                  @change="handleFormCustomerChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="销售合同">
                <!-- 只列 SALES 类型 + ACTIVE 的合同（卡面口径）；选了合同自动带出客户 -->
                <t-select
                  v-model="form.contract_id"
                  :options="contractOptions"
                  filterable
                  clearable
                  placeholder="不挂合同（可选）"
                  data-testid="so-form-contract"
                  @change="handleContractChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="销售员">
                <!-- SO-37：佣金归属人。留空 = 不指定（后端不默认成建单人）；
                     候选要 system:user:list，销售员账号没有 → 降级成提示 + 手填 -->
                <t-select
                  v-if="!salespersonOptionsFailed"
                  v-model="form.salesperson_id"
                  :options="salespersonOptions"
                  filterable
                  clearable
                  placeholder="不指定（留空则这笔订单不产生佣金）"
                  data-testid="so-form-salesperson"
                />
                <t-input
                  v-else
                  v-model="form.salesperson_id"
                  placeholder="候选无权限读取：粘贴销售员 UUID 或留空"
                  data-testid="so-form-salesperson-manual"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="订单日期" required-mark>
                <t-date-picker
                  v-model="form.order_date"
                  value-type="YYYY-MM-DD"
                  format="YYYY-MM-DD"
                  :clearable="false"
                  data-testid="so-form-order-date"
                  @change="handleOrderDateChange"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="币种">
                <!-- SO-42：只列 ACTIVE 币种；'' = 本位币（后端缺省取账套本位币，rate = 1）。
                     建单后币种随汇率一起锁进单据行，非草稿单不可改（表单本身就不可编辑）。 -->
                <t-select
                  v-model="form.currency_code"
                  :options="currencyOptions"
                  filterable
                  clearable
                  placeholder="默认本位币"
                  data-testid="so-form-currency"
                  @change="refreshQuote"
                />
                <template #help>
                  本位币（{{ baseCurrencyCode || 'CNY' }}）不需要汇率；外币按<b>订单日期</b>取最近一条即时汇率
                </template>
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="汇率">
                <!-- 只读：汇率由后端在保存那一刻锁定，前端不提供编辑入口（锁定后不可改） -->
                <div class="rate-readonly" data-testid="so-form-rate">
                  <template v-if="quoteLoading">
                    <t-loading size="small" />
                    <span class="rate-readonly__hint">查询汇率…</span>
                  </template>
                  <template v-else-if="quote && quote.source === 'MISSING'">
                    <span class="rate-readonly__value rate-readonly__value--bad mono">未录汇率</span>
                    <span class="rate-readonly__hint rate-readonly__hint--bad">
                      {{ quote.currency_code }} 在 {{ form.order_date }}（含）前没有即时汇率，保存会被拒绝 ——
                      请先到「汇率管理」补录
                    </span>
                  </template>
                  <template v-else-if="quote">
                    <span class="rate-readonly__value mono">{{ rateText(quote.exchange_rate) }}</span>
                    <span class="rate-readonly__hint">
                      {{
                        quote.source === 'BASE'
                          ? `本位币 ${quote.currency_code}，恒 1`
                          : `1 ${quote.currency_code} = ${rateText(quote.exchange_rate)} ${quote.base_currency}` +
                            (quote.effective_date ? `（牌价生效日 ${quote.effective_date}）` : '')
                      }}
                    </span>
                  </template>
                  <span v-else class="rate-readonly__hint">选定币种后自动带出</span>
                </div>
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="要求交期">
                <t-date-picker
                  v-model="form.delivery_date"
                  value-type="YYYY-MM-DD"
                  format="YYYY-MM-DD"
                  placeholder="未约定"
                  clearable
                  data-testid="so-form-delivery-date"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="备注">
                <t-input
                  v-model="form.remark"
                  :maxlength="SALES_ORDER_REMARK_MAX"
                  placeholder="订单备注（选填）"
                  data-testid="so-form-remark"
                />
              </t-form-item>
            </t-col>
          </t-row>
        </t-form>

        <div class="section-head">
          <span class="section-head__title">明细（{{ form.items.length }} 行）</span>
          <span class="section-head__tip">金额 = 数量 × 单价（不含税），税额 = 金额 × 税率；提交后由服务端重算</span>
          <span class="section-head__spacer" />
          <t-button
            size="small"
            variant="outline"
            :disabled="!canAddLine(form)"
            data-testid="btn-so-add-line"
            @click="addLine"
          >
            <template #icon><t-icon name="add" /></template>
            加一行
          </t-button>
        </div>

        <t-table
          row-key="key"
          size="small"
          :data="form.items"
          :columns="formItemColumns"
          data-testid="so-form-items"
        >
          <template #line_index="{ rowIndex }">
            <span class="mono">{{ rowIndex + 1 }}</span>
          </template>

          <template #material_id="{ row }">
            <t-select
              v-model="row.material_id"
              class="material-select"
              :options="materialOptions"
              filterable
              :filter="filterMaterial"
              placeholder="物料编码 / 名称"
              data-testid="so-form-material"
              @change="onLineMaterialChange(row)"
            />
          </template>

          <template #qty="{ row }">
            <t-input-number
              v-model="row.qty"
              theme="normal"
              :decimal-places="4"
              :min="0"
              align="right"
              data-testid="so-form-qty"
              @change="onLineQtyChange(row)"
            />
          </template>

          <template #price="{ row }">
            <div class="price-cell">
              <t-input-number
                v-model="row.price"
                theme="normal"
                :decimal-places="4"
                :min="0"
                align="right"
                placeholder="0.0000"
                data-testid="so-form-price"
                @change="onLinePriceChange(row)"
              />
              <!-- 含税口径提示（E2E-FIX-4 ②）：填的是不含税价，原协议价是含税价 -->
              <span v-if="linePriceInclHint(row)" class="price-cell__hint" data-testid="so-form-price-incl-hint">
                {{ linePriceInclHint(row) }}
              </span>
            </div>
          </template>

          <template #price_source="{ row }">
            <div class="source-cell">
              <t-tag
                v-if="linePriceBadge(row)"
                size="small"
                variant="light"
                :theme="linePriceBadgeTheme(row)"
                data-testid="so-form-price-source"
              >
                {{ linePriceBadgeLabel(row) }}
              </t-tag>
              <span v-else class="source-cell__empty">—</span>
              <t-link
                v-if="row.price_source || row.price_manual"
                theme="primary"
                size="small"
                data-testid="btn-so-refetch-price"
                @click="refetchLinePrice(row, true)"
              >
                重取价
              </t-link>
            </div>
          </template>

          <template #tax_rate="{ row }">
            <t-input-number
              v-model="row.tax_rate"
              theme="normal"
              :decimal-places="2"
              :min="0"
              :max="100"
              align="right"
              data-testid="so-form-tax-rate"
            />
          </template>

          <template #amount="{ row }">
            <span class="mono" data-testid="so-form-amount">{{ formatMoney(computeLine(row.qty, row.price, row.tax_rate).amount) }}</span>
          </template>

          <template #tax_amount="{ row }">
            <span class="mono">{{ formatMoney(computeLine(row.qty, row.price, row.tax_rate).tax_amount) }}</span>
          </template>

          <template #line_op="{ row }">
            <t-link
              theme="danger"
              :disabled="!canRemoveLine(form)"
              data-testid="btn-so-remove-line"
              @click="removeLine(row.key)"
            >
              删除
            </t-link>
          </template>
        </t-table>

        <div class="totals" data-testid="so-form-totals">
          <span>不含税合计 <b class="mono">{{ formatMoney(formTotals.amount) }}</b></span>
          <span>税额合计 <b class="mono">{{ formatMoney(formTotals.tax) }}</b></span>
          <span>价税合计 <b class="mono totals__strong">{{ formatMoney(formTotals.with_tax) }}</b></span>
        </div>

        <t-alert v-if="formError" theme="error" :message="formError" class="form-alert" data-testid="so-form-error" />
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉（含审批进度） ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="销售订单详情"
      size="920px"
      :footer="false"
      data-testid="so-detail-drawer"
    >
      <div v-if="detailLoading" class="detail-loading"><t-loading :loading="true" text="加载中…" /></div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" data-testid="so-detail-error" />

      <template v-else-if="detail">
        <div class="detail-head">
          <span class="detail-head__no mono" data-testid="so-detail-no">{{ detail.so_no }}</span>
          <t-tag size="small" variant="light" :theme="salesOrderStatusTheme(detail.status)">
            {{ detail.status_label || salesOrderStatusLabel(detail.status) }}
          </t-tag>
          <span class="detail-head__spacer" />
          <!-- 详情里的操作与列表同源（状态判定都在 sales-order-query.ts） -->
          <t-button
            v-if="detailActions.submit"
            v-privilege="[PERM.status]"
            size="small"
            theme="primary"
            data-testid="btn-so-detail-submit"
            @click="submitOrder(detail)"
          >
            提交审批
          </t-button>
          <t-button
            v-if="detailActions.withdraw"
            v-privilege="[PERM.status]"
            size="small"
            variant="outline"
            data-testid="btn-so-detail-withdraw"
            @click="withdrawOrder(detail)"
          >
            撤回审批
          </t-button>
          <t-button
            v-if="detailActions.close"
            v-privilege="[PERM.status]"
            size="small"
            variant="outline"
            data-testid="btn-so-detail-close"
            @click="openReasonDialog(detail, 'CLOSED')"
          >
            关闭
          </t-button>
          <t-button
            v-if="detailActions.cancel"
            v-privilege="[PERM.status]"
            size="small"
            theme="danger"
            variant="outline"
            data-testid="btn-so-detail-cancel"
            @click="openReasonDialog(detail, 'CANCELLED')"
          >
            作废
          </t-button>
          <t-button
            v-if="detailActions.edit"
            v-privilege="[PERM.update]"
            size="small"
            variant="outline"
            data-testid="btn-so-detail-edit"
            @click="openEdit(detail)"
          >
            编辑
          </t-button>
          <!-- PDF-08：下载 PDF -->
          <DocPdfButton
            :path="`/api/business/erp/sales-order/${detail.id}/pdf`"
            :filename="`销售订单-${detail.so_no}.pdf`"
            testid="so-detail-pdf"
          />
        </div>

        <t-descriptions :column="2" size="small" class="detail-desc" data-testid="so-detail-desc">
          <t-descriptions-item label="客户">{{ customerText(detail) }}</t-descriptions-item>
          <t-descriptions-item label="销售合同">{{ detail.contract_no || '—' }}</t-descriptions-item>
          <t-descriptions-item label="销售员">{{ detail.salesperson_name || '未指定（不产生佣金）' }}</t-descriptions-item>
          <t-descriptions-item label="订单日期">{{ detail.order_date }}</t-descriptions-item>
          <t-descriptions-item label="要求交期">{{ detail.delivery_date || '—' }}</t-descriptions-item>
          <t-descriptions-item label="币种">
            <span class="mono">{{ detail.currency_code || '—' }}</span>
            <t-tag v-if="isBaseCurrency(detail.currency_code)" size="small" variant="light" theme="success" class="detail-tag">
              本位币
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="锁定汇率">
            <!-- 只读：这是建单那一刻锁进单据行的汇率，之后汇率表怎么变都不回写 -->
            <span class="mono" data-testid="so-detail-rate">{{ rateText(detail.exchange_rate) }}</span>
            <span class="detail-hint">
              （{{ isBaseCurrency(detail.currency_code) ? '本位币恒 1' : `1 ${detail.currency_code} = 上述本位币，建单时锁定，不可改` }}）
            </span>
          </t-descriptions-item>
          <t-descriptions-item label="价税合计">
            <b class="mono">{{ formatMoney(detail.total_with_tax) }}</b>
          </t-descriptions-item>
          <t-descriptions-item label="不含税合计">{{ formatMoney(detail.total_amount) }}</t-descriptions-item>
          <t-descriptions-item label="税额合计">{{ formatMoney(detail.total_tax) }}</t-descriptions-item>
          <t-descriptions-item v-if="!isBaseCurrency(detail.currency_code)" label="本位币价税合计">
            <b class="mono" data-testid="so-detail-base-total">{{ formatMoney(detail.base_total_with_tax) }}</b>
          </t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ detail.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <!--
          审批进度：复用审批中心的 FlowProgress（与采购 / 应付 / 凭证 / 收货 / 调拨同一口径）。
          节点行只来自 `GET /business/flow/instance/{id}`，前端不自己推「走到第几步」；
          没有实例 id（草稿 / 已驳回打回）就整块不渲染，拉不到节点则降级成单号。
        -->
        <template v-if="hasApprovalProgress(approvalProgress)">
          <div class="section-title">审批进度</div>
          <FlowProgress
            :nodes="approvalProgress.nodes"
            :tasks="approvalProgress.tasks"
            data-testid="so-approval-progress"
          />
        </template>
        <div v-else-if="approvalFallback" class="approval-fallback" data-testid="so-approval-fallback">
          {{ approvalFallback.label }} {{ approvalFallback.no }}（节点进度暂不可见，请到审批中心查看）
        </div>

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="detailItemColumns"
          data-testid="so-detail-items"
        >
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }"><span class="mono">{{ formatQty(row.qty) }}</span></template>
          <template #price="{ row }"><span class="mono">{{ formatMoney(row.price) }}</span></template>
          <template #tax_rate="{ row }"><span class="mono">{{ formatQty(row.tax_rate) }}%</span></template>
          <template #amount="{ row }"><span class="mono">{{ formatMoney(row.amount) }}</span></template>
          <template #tax_amount="{ row }"><span class="mono">{{ formatMoney(row.tax_amount) }}</span></template>
          <template #delivered_qty="{ row }"><span class="mono">{{ formatQty(row.delivered_qty) }}</span></template>
          <template #open_qty="{ row }"><span class="mono">{{ formatQty(row.open_qty) }}</span></template>
        </t-table>
      </template>
    </t-drawer>

    <!-- ==================== ⑤ 作废 / 关闭（要原因） ==================== -->
    <t-dialog
      v-model:visible="reasonVisible"
      :header="reasonTarget === 'CLOSED' ? '关闭销售订单' : '作废销售订单'"
      :width="520"
      :confirm-btn="{ content: reasonTarget === 'CLOSED' ? '关闭' : '作废', theme: 'danger', loading: reasonSubmitting }"
      :cancel-btn="{ disabled: reasonSubmitting }"
      data-testid="so-reason-dialog"
      @confirm="confirmReason"
    >
      <div class="reason-body">
        <p class="reason-tip">
          {{ reasonNeedsInput ? '这条状态迁移会影响客户额度占用 / 未发数量，必须写清原因（进状态日志与对账依据）。' : '草稿作废不动账，原因可选。' }}
        </p>
        <t-textarea
          v-model="reasonText"
          :autosize="{ minRows: 3, maxRows: 5 }"
          :maxlength="200"
          placeholder="原因（例如：客户取消订单 / 重复开单）"
          data-testid="so-reason-input"
        />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 销售订单页（SO-07）。
 *
 * # 接口（全部真接口，路径逐字对应 `backend/src/business/erp/sales_order_handler.rs`）
 *
 * | 动作 | 请求 | 权限 |
 * |------|------|------|
 * | 列表 | `GET /api/business/erp/sales-order/page` | `:list` |
 * | 详情 | `GET /api/business/erp/sales-order/{id}` | `:list` |
 * | 新建 | `POST /api/business/erp/sales-order` | `:create` |
 * | 编辑 | `PUT /api/business/erp/sales-order/{id}`（只有 DRAFT） | `:update` |
 * | 提交审批 | `POST /api/business/erp/sales-order/{id}/submit-approval` | `:status` |
 * | 作废 / 关闭 | `PATCH /api/business/erp/sales-order/{id}/status` | `:status` |
 * | 撤回审批 | `POST /api/business/flow/instance/{id}/withdraw`（**审批引擎**，不是 PATCH） | `business:flow:instance:read` 侧 |
 *
 * # 三条硬口径（SO-06 交接）
 *
 * 1. **审批结论不许人工写**：`APPROVED` / `DRAFT` 这两个目标态永远不发给 `PATCH /status`
 *    （后端 400）。撤回 = 撤审批实例，通过 / 驳回 = 在审批中心点。
 * 2. **撤回要先拿实例 id**：列表行的 `approval_instance_id` 恒为 `null`（后端只在详情补），
 *    所以撤回按钮点下去先拉一次详情。
 * 3. **列表不画审批进度**：实例 id 只有详情有，进度块（`FlowProgress`）只在详情抽屉里。
 *
 * # 金额
 *
 * 表单里的金额是 {@link computeLine} 的**预览**，提交只发 `qty` / `price` / `tax_rate`；
 * 列表 / 详情的金额一律是服务端返回值（`formatMoney` 只做显示格式）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  createSalesOrder,
  changeSalesOrderStatus,
  fetchContractPage,
  fetchCustomerPage,
  fetchMaterialPage,
  fetchSalesOrderDetail,
  fetchSalesOrderPage,
  submitSalesOrderApproval,
  updateSalesOrder,
  type ContractRow,
  type Customer,
  type MaterialRow,
  type SalesOrder,
  type SalesOrderStatus,
} from '@/api/business/erp';
import { getFlowInstance, withdrawFlowInstance } from '@/api/business/flow-api';
import { quoteRate, fetchCurrencyOptions, type CurrencyOption, type RateQuote } from '@/api/business/erp/currency';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import DocPdfButton from '@/components/DocPdfButton.vue';
import {
  EMPTY_APPROVAL_PROGRESS,
  approvalFallbackOf,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  SALES_ORDER_COLUMNS,
  SALES_ORDER_ITEM_COLUMNS,
  SALES_ORDER_MAX_LINES,
  SALES_ORDER_PAGE_SIZE,
  SALES_ORDER_REMARK_MAX,
  SALES_ORDER_STATUS_OPTIONS,
  applyResolvedPrice,
  buildCreatePayload,
  buildSalesOrderQuery,
  buildUpdatePayload,
  canAddLine,
  canRemoveLine,
  clearManualPrice,
  computeLine,
  computeTotals,
  emptyListText,
  emptySalesOrderFilter,
  emptySalesOrderForm,
  formFromDetail,
  isSalesOrderFiltered,
  linePriceBadge,
  linePriceBadgeLabel,
  linePriceBadgeTheme,
  linePriceInclHint,
  markPriceManual,
  newSalesOrderLine,
  rowActions,
  salesOrderStatusLabel,
  salesOrderStatusTheme,
  statusNeedsReason,
  validateSalesOrderForm,
  type SalesOrderFilter,
  type SalesOrderFormDraft,
  type SalesOrderLineDraft,
} from './sales-order-query';
import { resolvePrice, type PriceResolveRes } from '@/api/business/erp/customer-price';

/** 本页权限码（与 `V0265__sales_order_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情（页面入口） */
  list: 'business:erp:sale-order:list',
  /** 新建 */
  create: 'business:erp:sale-order:create',
  /** 编辑（只有 DRAFT） */
  update: 'business:erp:sale-order:update',
  /** 状态流转（提交 / 撤回 / 作废 / 关闭） */
  status: 'business:erp:sale-order:status',
} as const;

const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

/** 表格列（colKey 与后端出参同名） */
const columns = SALES_ORDER_COLUMNS;
/** 表单明细列（行号 / 物料 / 数量 / 单价 / 取价来源 / 税率 / 金额 / 税额 / 删除） */
const formItemColumns: PrimaryTableCol[] = [
  { colKey: 'line_index', title: '#', width: 48 },
  { colKey: 'material_id', title: '物料', minWidth: 220 },
  { colKey: 'qty', title: '数量', width: 110 },
  { colKey: 'price', title: '不含税单价', width: 130 },
  // SO-34：取价来源（协议价 / 阶梯价 / 标准价 / 未找到 / 手动价）
  { colKey: 'price_source', title: '取价来源', width: 150 },
  { colKey: 'tax_rate', title: '税率%', width: 90 },
  { colKey: 'amount', title: '金额', width: 120, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 110, align: 'right' },
  { colKey: 'line_op', title: '操作', width: 80 },
];
/** 详情明细列（多两列发货进度） */
const detailItemColumns: PrimaryTableCol[] = [
  ...SALES_ORDER_ITEM_COLUMNS,
  { colKey: 'delivered_qty', title: '已发', width: 100, align: 'right' },
  { colKey: 'open_qty', title: '未发', width: 100, align: 'right' },
];
/** 状态下拉（`''` = 全部，靠 `clearable` 清） */
const statusOptions = SALES_ORDER_STATUS_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================

const filter = reactive<SalesOrderFilter>(emptySalesOrderFilter());
const rows = ref<SalesOrder[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = SALES_ORDER_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const isFiltered = computed(() => isSalesOrderFiltered(filter));
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

async function loadOrders(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchSalesOrderPage(buildSalesOrderQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      await loadOrders(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `销售订单加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadOrders(1);
}

function handleReset(): void {
  Object.assign(filter, emptySalesOrderFilter());
  current.value = 1;
  void loadOrders(1);
}

function handleRefresh(): void {
  void loadOrders(current.value);
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadOrders(pageInfo.current);
}

// ==================== 主数据下拉（客户 / 物料 / 销售合同） ====================

const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);
/** 可挂的销售合同（`contract_type=SALES` + `status=ACTIVE`，SO-27） */
const contracts = ref<ContractRow[]>([]);
/** 销售员候选（SO-37；`sys_user.page` 里 `status=1` 的启用账号） */
const salespersons = ref<UserRes[]>([]);
/** 销售员候选拉不到（销售员 / 销售主管没有 `system:user:list`）→ 表单里给降级提示 */
const salespersonOptionsFailed = ref(false);

/** 客户下拉：只列 `ACTIVE`（冻结客户开不了新单，后端 400） */
const customerOptions = computed(() =>
  customers.value.map((item) => ({
    label: `${item.code} ${item.name}`,
    value: item.id,
  })),
);

/**
 * 合同下拉：**只列 SALES 类型 + ACTIVE 的合同**（卡面口径）。
 *
 * 后端 `USABLE_FOR_ORDER` 还放行 `SIGNED`（与采购单同一常量），API 调用方可以挂
 * 已签署合同；页面按卡面只列 ACTIVE，避免用户在不该下单的合同上点出单。
 * 选中后由 `handleContractChange` 自动带出客户（后端要求合同乙方 = 订单客户）。
 */
const contractOptions = computed(() =>
  contracts.value.map((item) => ({
    label: `${item.contract_no} ${item.contract_name}`,
    value: item.id,
  })),
);

/** 销售员下拉（`real_name（username）`；账号名兜底，别显示裸 UUID） */
const salespersonOptions = computed(() =>
  salespersons.value.map((item) => ({
    label: `${item.real_name || item.username}（${item.username}）`,
    value: item.id,
  })),
);

/**
 * 选合同 → 自动带出客户。
 *
 * 为什么自动带：后端强制「合同乙方 = 订单客户」，让用户手选两次必然对不上；
 * 清空合同时**不动**客户（用户可能先选了客户再挂合同，回头解绑不该丢客户）。
 */
function handleContractChange(value: unknown): void {
  const id = typeof value === 'string' ? value : '';
  const picked = contracts.value.find((item) => item.id === id);
  if (picked) {
    form.customer_id = picked.party_b_id;
  }
}

/** 物料下拉：只列启用（`status = 1`），标签带编码 + 规格 */
const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: materialLabel(item),
    value: item.id,
  })),
);

function materialLabel(material: MaterialRow): string {
  const spec = material.spec ? ` / ${material.spec}` : '';
  return `${material.material_code} ${material.material_name}${spec}`;
}

/** 本地过滤（一次拉 200 条够用；超出时让用户去物料页查） */
function filterMaterial(keyword: string, option: { label?: unknown }): boolean {
  if (!keyword) {
    return true;
  }
  return String(option.label ?? '').toLowerCase().includes(keyword.toLowerCase());
}

/** 详情里 `material_id` → 名称（下拉可能没覆盖到，回落短 id） */
function materialNameOf(id: string): string {
  const found = materials.value.find((item) => item.id === id);
  if (found) {
    return materialLabel(found);
  }
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

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

/** 客户显示：名称（编码），客户软删则两列都是 null */
function customerText(row: { customer_name: string | null; customer_code: string | null }): string {
  if (row.customer_name && row.customer_code) {
    return `${row.customer_name}（${row.customer_code}）`;
  }
  if (row.customer_name) {
    return row.customer_name;
  }
  return '客户已删除';
}

async function loadOptions(): Promise<void> {
  // 四个下拉互不依赖，一起发；任何一个失败都不该让页面整体失败 —— 只在控制台留痕
  const [customerResult, materialResult, contractResult, userResult] = await Promise.allSettled([
    fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: 200 }),
    fetchMaterialPage({ status: 1, page: 1, page_size: 200 }),
    // SO-27：只列 SALES + ACTIVE 的销售合同（卡面口径）
    fetchContractPage({ contract_type: 'SALES', status: 'ACTIVE', page: 1, page_size: 200 }),
    // SO-37：销售员候选（佣金归属人）。销售员 / 销售主管**没有** system:user:list，
    // 拉不到时只是下拉为空 —— 建单本身照旧（后端允许销售员留空）
    pageUsers({ page: 1, page_size: 200, status: 1 }),
  ]);
  if (customerResult.status === 'fulfilled') {
    customers.value = customerResult.value.list ?? [];
  }
  if (materialResult.status === 'fulfilled') {
    materials.value = materialResult.value.list ?? [];
  }
  if (contractResult.status === 'fulfilled') {
    contracts.value = contractResult.value.list ?? [];
  }
  if (userResult.status === 'fulfilled') {
    salespersons.value = userResult.value.list ?? [];
    salespersonOptionsFailed.value = salespersons.value.length === 0;
  } else {
    salespersonOptionsFailed.value = true;
  }
}

// ==================== 新建 / 编辑 ====================

const formVisible = ref(false);
const editingId = ref<string | null>(null);
const form = reactive<SalesOrderFormDraft>(emptySalesOrderForm(todayCst()));
const formError = ref('');
const submitting = ref(false);

/** 北京时间今天（与后端 `today_cst()` 同口径） */
function todayCst(): string {
  return new Date(Date.now() + 8 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

// ==================== 币种 + 汇率（SO-42 / ARCH-7 §15.6） ====================

/**
 * 币种下拉（只 `ACTIVE`，来自「币种管理」主数据）。
 *
 * 读不到就**留空**：`''` 表示本位币，后端缺省取账套本位币并锁 `rate = 1`，
 * 所以下拉挂了也不影响开本位币单（只是不能开外币单）。
 */
const currencyOptions = ref<{ label: string; value: string }[]>([]);
const baseCurrencyCode = ref('');

async function loadCurrencies(): Promise<void> {
  try {
    const list: CurrencyOption[] = await fetchCurrencyOptions();
    baseCurrencyCode.value = list.find((item) => item.is_base)?.code ?? '';
    currencyOptions.value = list.map((item) => ({ label: item.label, value: item.code }));
  } catch {
    // 权限缺失 / 接口挂了：外币开不了，但本位币单照旧（不要在开单页弹红条吓人）
    currencyOptions.value = [];
  }
}

/** 汇率预览（`source = 'MISSING'` 是合法结果：外币没录汇率，保存会被后端拒绝） */
const quote = ref<RateQuote | null>(null);
const quoteLoading = ref(false);

/** 汇率展示：8 位定标（`NUMERIC(18,8)`），与汇率管理页同一格式 */
function rateText(raw: string | number | null | undefined): string {
  if (raw === null || raw === undefined || raw === '') {
    return '—';
  }
  const value = typeof raw === 'number' ? raw : Number(raw);
  return Number.isFinite(value) ? value.toFixed(8) : String(raw);
}

/** 是否本位币单（空码 = V0314 之前的老单，按本位币处理） */
function isBaseCurrency(code: string | null | undefined): boolean {
  if (!code) {
    return true;
  }
  // 币种主数据没读到时**不臆断**（不把外币单标成本位币）
  return baseCurrencyCode.value ? code === baseCurrencyCode.value : false;
}

/**
 * 拉一次汇率预览。
 *
 * 与建单锁定**同一把尺**（后端 `currency::lock`）：页面上看到的就是保存时落进
 * `exchange_rate` 的值，不会「预览 7.1、落库 7.0」。
 */
async function refreshQuote(): Promise<void> {
  if (!formVisible.value) {
    return;
  }
  quoteLoading.value = true;
  try {
    quote.value = await quoteRate(form.currency_code || undefined, form.order_date || undefined);
  } catch {
    // 预览失败不挡提交（后端仍会自己锁一次并给出准确错误）
    quote.value = null;
  } finally {
    quoteLoading.value = false;
  }
}

/** 换订单日期：汇率按单据日取，日期一变就要重算预览 */
function handleOrderDateChange(): void {
  void refreshQuote();
}

/** 表单合计（预览；提交后以服务端 `total_*` 为准） */
const formTotals = computed(() =>
  computeTotals(
    form.items.map((line) => ({ qty: line.qty, price: line.price, tax_rate: line.tax_rate })),
  ),
);

function openCreate(): void {
  editingId.value = null;
  formError.value = '';
  Object.assign(form, emptySalesOrderForm(todayCst()));
  quote.value = null;
  formVisible.value = true;
  // 默认本位币的汇率恒 1（后端缺省），打开弹窗就把预览拉出来，别让用户盯着空白
  void refreshQuote();
}

/** 编辑：先拉详情（列表行没有明细），再用 `formFromDetail` 回填 */
async function openEdit(row: SalesOrder): Promise<void> {
  try {
    const detail = row.items.length > 0 ? row : await fetchSalesOrderDetail(row.id);
    editingId.value = detail.id;
    formError.value = '';
    Object.assign(form, formFromDetail(detail));
    quote.value = null;
    formVisible.value = true;
    // 编辑态预览的是「按当前币种 + 订单日期**现在**会锁到的汇率」；
    // 单据行上已锁的 exchange_rate 以详情为准（详情页展示 `detail.exchange_rate`）
    void refreshQuote();
  } catch (error) {
    MessagePlugin.error(`加载订单详情失败：${messageOf(error)}`);
  }
}

function addLine(): void {
  if (!canAddLine(form)) {
    MessagePlugin.warning(`明细最多 ${SALES_ORDER_MAX_LINES} 行`);
    return;
  }
  form.items.push(newSalesOrderLine());
}

function removeLine(key: string): void {
  if (!canRemoveLine(form)) {
    MessagePlugin.warning('至少保留 1 行明细');
    return;
  }
  const index = form.items.findIndex((line: SalesOrderLineDraft) => line.key === key);
  if (index >= 0) {
    form.items.splice(index, 1);
  }
}

// ==================== 自动带价（SO-34 / ARCH-7 §15.3.3） ====================

/**
 * 每行的取价请求序号。
 *
 * `qty` 每敲一次都会发一次请求，慢的那个后回来会把新价覆盖成旧价（经典乱序 bug）。
 * 每次发请求自增序号，回来时序号不是最新就**丢弃**——这是前端唯一能保证"最后一次输入赢"的手段。
 */
const priceSeq = new Map<string, number>();

/** 行在表单里的序号（1 起，报错文案用） */
function lineNoOf(line: SalesOrderLineDraft): number {
  return form.items.findIndex((item) => item.key === line.key) + 1;
}

/**
 * 取价并回填一行。
 *
 * @param line   目标行
 * @param force  `true` = 用户点「重取价」：先清掉手动标记，再按规则覆盖
 */
async function refetchLinePrice(line: SalesOrderLineDraft, force = false): Promise<void> {
  const materialId = line.material_id;
  if (!materialId) {
    line.price_source = '';
    return;
  }
  const qty = Number(line.qty);
  if (!Number.isFinite(qty) || qty <= 0) {
    // qty 必填（SO-33）：数量还没填好就取价会静默命中 min_qty=0 的低价档，宁可不取
    line.price_source = '';
    return;
  }
  if (force) {
    clearManualPrice(line);
  }
  const token = (priceSeq.get(line.key) ?? 0) + 1;
  priceSeq.set(line.key, token);
  try {
    const res: PriceResolveRes = await resolvePrice({
      material_id: materialId,
      qty: String(qty),
      customer_id: form.customer_id || undefined,
      date: form.order_date || undefined,
    });
    // 过期响应 / 行已被删 / 物料已被换掉 → 丢弃（别把别的行的价写进来）
    if (priceSeq.get(line.key) !== token || line.material_id !== materialId) {
      return;
    }
    applyResolvedPrice(line, res);
    if (res.source === 'NONE' && !line.price_manual) {
      // 无价是合法业务结果（200）：提示手填，不是报错弹窗
      MessagePlugin.warning(`第 ${lineNoOf(line)} 行未找到协议价 / 阶梯价 / 标准价，请手填不含税单价`);
    }
  } catch (error) {
    if (priceSeq.get(line.key) === token) {
      // 取价失败不该拦住开单：告诉用户、让他手填
      MessagePlugin.warning(`第 ${lineNoOf(line)} 行取价失败（可手填单价）：${messageOf(error)}`);
    }
  }
}

/**
 * 换物料：**先清掉旧单价**再取价。
 *
 * 不清的话，取价失败（网络 / 404）时旧物料的价会留在新物料行上 —— 这正是
 * "卖错价"最典型的一条路径。
 */
function onLineMaterialChange(row: SalesOrderLineDraft): void {
  row.price = '';
  row.price_source = '';
  row.price_manual = false;
  row.price_incl = '';
  row.price_tax_rate = '';
  void refetchLinePrice(row);
}

/** 改数量：档位随量变，重新取价（手动价不动，只更新来源） */
function onLineQtyChange(row: SalesOrderLineDraft): void {
  void refetchLinePrice(row);
}

/** 改单价：有来源时标「手动价」（销售覆盖了系统价，必须留痕） */
function onLinePriceChange(row: SalesOrderLineDraft): void {
  markPriceManual(row);
}

/** 换客户：所有已选物料的行重新取价（协议价 / 客户专属阶梯都跟着客户变） */
function handleFormCustomerChange(): void {
  for (const line of form.items) {
    if (line.material_id) {
      void refetchLinePrice(line);
    }
  }
}

async function submitForm(): Promise<void> {
  const error = validateSalesOrderForm(form);
  if (error) {
    formError.value = error;
    return;
  }
  formError.value = '';
  submitting.value = true;
  try {
    const created = editingId.value
      ? await updateSalesOrder(editingId.value, buildUpdatePayload(form))
      : await createSalesOrder(buildCreatePayload(form));
    MessagePlugin.success(editingId.value ? `已保存 ${created.so_no}` : `已创建 ${created.so_no}`);
    formVisible.value = false;
    await loadOrders(editingId.value ? current.value : 1);
  } catch (submitError) {
    // 后端原文（客户冻结 / 状态已变 / 明细校验）直接摆到弹窗里，别只说「保存失败」
    formError.value = messageOf(submitError);
  } finally {
    submitting.value = false;
  }
}

// ==================== 提交 / 撤回 / 作废 / 关闭 ====================

/**
 * 提交审批。
 *
 * 走 `POST /{id}/submit-approval`：配了 `so_approve` 就发起实例（单据转 `SUBMITTED`），
 * 没配流程后端 409（`APPROVAL_FLOW_NOT_CONFIGURED`）—— 这里原样显示后端文案，
 * 不在前端「假装提交成功」。
 */
async function submitOrder(row: SalesOrder): Promise<void> {
  try {
    const updated = await submitSalesOrderApproval(row.id);
    MessagePlugin.success(`${updated.so_no} 已提交审批`);
    await refreshAfterMutation(updated.id);
  } catch (error) {
    MessagePlugin.error(`提交失败：${messageOf(error)}`);
    await loadOrders(current.value);
  }
}

/**
 * 撤回审批。
 *
 * 列表行没有实例 id（后端只在详情补），所以先拉详情；撤回走**审批引擎**
 * `POST /business/flow/instance/{id}/withdraw`，引擎回调再把单据打回 `DRAFT`。
 */
async function withdrawOrder(row: SalesOrder): Promise<void> {
  let instanceId = row.approval_instance_id ?? '';
  try {
    if (!instanceId) {
      const detail = await fetchSalesOrderDetail(row.id);
      instanceId = detail.approval_instance_id ?? '';
    }
    if (!instanceId) {
      MessagePlugin.warning('这张单当前没有在跑的审批实例（可能已结束），请刷新后查看状态');
      await loadOrders(current.value);
      return;
    }
    await withdrawFlowInstance(instanceId, '制单人撤回审批');
    MessagePlugin.success(`${row.so_no} 已撤回，单据回到草稿`);
    await refreshAfterMutation(row.id);
  } catch (error) {
    MessagePlugin.error(`撤回失败：${messageOf(error)}`);
    await loadOrders(current.value);
  }
}

/** 作废 / 关闭的原因弹窗 */
const reasonVisible = ref(false);
const reasonTarget = ref<SalesOrderStatus>('CANCELLED');
const reasonOrder = ref<SalesOrder | null>(null);
const reasonText = ref('');
const reasonSubmitting = ref(false);

/** 这条边要不要原因（与后端 `reason_edges` 同源） */
const reasonNeedsInput = computed(() =>
  reasonOrder.value ? statusNeedsReason(reasonOrder.value.status, reasonTarget.value) : false,
);

function openReasonDialog(row: SalesOrder, target: SalesOrderStatus): void {
  reasonOrder.value = row;
  reasonTarget.value = target;
  reasonText.value = '';
  reasonVisible.value = true;
}

async function confirmReason(): Promise<void> {
  const order = reasonOrder.value;
  if (!order) {
    reasonVisible.value = false;
    return;
  }
  if (reasonNeedsInput.value && !reasonText.value.trim()) {
    MessagePlugin.warning('这条状态迁移必须填写原因');
    return;
  }
  reasonSubmitting.value = true;
  try {
    const updated = await changeSalesOrderStatus(order.id, {
      target_status: reasonTarget.value,
      reason: reasonText.value.trim() || undefined,
    });
    MessagePlugin.success(
      `${updated.so_no} 已${reasonTarget.value === 'CLOSED' ? '关闭' : '作废'}`,
    );
    reasonVisible.value = false;
    await refreshAfterMutation(order.id);
  } catch (error) {
    MessagePlugin.error(`操作失败：${messageOf(error)}`);
  } finally {
    reasonSubmitting.value = false;
  }
}

/** 变更后刷新列表；详情抽屉开着就同步刷新那一张 */
async function refreshAfterMutation(id: string): Promise<void> {
  await loadOrders(current.value);
  if (detailVisible.value && detail.value?.id === id) {
    await loadDetail(id);
  }
}

// ==================== 详情（含审批进度） ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<SalesOrder | null>(null);
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

const detailActions = computed(() => rowActions(detail.value ?? { status: null }));
/** 拉不到节点时的降级文案（「审批中 FLW-xxx」） */
const approvalFallback = computed(() =>
  approvalFallbackOf(
    detail.value
      ? {
          status: detail.value.status,
          approval_instance_id: detail.value.approval_instance_id,
        }
      : null,
  ),
);

async function openDetail(row: SalesOrder): Promise<void> {
  detailVisible.value = true;
  await loadDetail(row.id);
}

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    const data = await fetchSalesOrderDetail(id);
    detail.value = data;
    // 进度永不让详情页失败：没实例 → 空；拉不到 → 空（降级成单号）
    approvalProgress.value = await loadApprovalProgress(data.approval_instance_id, getFlowInstance);
  } catch (error) {
    detailError.value = `详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

onMounted(async () => {
  // 币种下拉与订单/主数据互不依赖：一起发，币种读不到也不挡开单项
  await Promise.all([loadOrders(1), loadOptions(), loadCurrencies()]);
});
</script>

<style scoped lang="less">
.sales-order-page {
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
    width: 220px;
  }

  &__select {
    width: 150px;
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
}

.material-select {
  width: 100%;
}

.totals {
  display: flex;
  justify-content: flex-end;
  gap: 24px;
  padding: 8px 4px;
  font-size: 13px;

  &__strong {
    color: var(--td-brand-color);
    font-size: 15px;
  }
}

.form-alert {
  margin-top: 4px;
}

// SO-42：币种汇率只读展示（汇率本身没有输入框 —— 锁定后不可改）
.rate-readonly {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.5;

  &__value {
    font-size: 14px;
    font-weight: 600;
    color: var(--td-brand-color);

    &--bad {
      color: var(--td-error-color);
    }
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    white-space: normal;

    &--bad {
      color: var(--td-error-color);
    }
  }
}

.detail-tag {
  margin-left: 6px;
}

.detail-hint {
  margin-left: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.detail-loading {
  padding: 32px 0;
  text-align: center;
}

.detail-head {
  display: flex;
  align-items: center;
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

.detail-desc {
  margin-bottom: 8px;
}

.section-title {
  margin: 16px 0 8px;
  font-weight: 600;
}

.approval-fallback {
  margin: 8px 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
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

/* SO-34：取价来源单元格（标签 + 重取价链接，窄列里也不换行） */
.source-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}

.source-cell__empty {
  color: var(--td-text-color-placeholder);
}

/* 单价 + 含税口径提示（E2E-FIX-4 ②）：提示要小、要贴着输入框，不能挤掉明细行高 */
.price-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.price-cell__hint {
  font-size: 12px;
  line-height: 16px;
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}
</style>
