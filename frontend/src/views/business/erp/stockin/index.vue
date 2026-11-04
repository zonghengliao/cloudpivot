<template>
  <div class="stockin-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-stockin-create"
          @click="openCreateDialog"
        >
          <template #icon><t-icon name="add" /></template>
          新建收货单
        </t-button>
        <t-button variant="outline" data-testid="btn-stockin-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="stockin-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.supplier_id"
          class="tool-row__supplier"
          :options="supplierOptions"
          placeholder="全部供应商"
          clearable
          filterable
          data-testid="stockin-filter-supplier"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.warehouse_id"
          class="tool-row__warehouse"
          :options="warehouseOptions"
          placeholder="全部仓库"
          clearable
          filterable
          data-testid="stockin-filter-warehouse"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__no"
          placeholder="收货单号 / 备注（模糊）"
          clearable
          data-testid="stockin-filter-no"
          @enter="handleSearch"
        />
        <!-- D4/O3：按来源采购单号筛（从采购单跳过来的人最常用这一条） -->
        <t-input
          v-model="filter.purchase_order_no"
          class="tool-row__po"
          placeholder="来源采购单号（模糊）"
          clearable
          data-testid="stockin-filter-po-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-stockin-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-stockin-reset" @click="handleReset">重置</t-button>
      </div>
      <!-- O4 列表记忆：恢复筛选必须让用户看得见，并能一键清掉 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="stockin-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- ==================== ② 批量操作（D7） ==================== -->
    <BatchActionBar
      :selected="selectedRows"
      :actions="BATCH_ACTIONS"
      :filtered-total="total"
      :runner="runBatchAction"
      :fetch-all-ids="fetchAllRowIds"
      :can="canBatchAction"
      testid="stockin-batch"
      @update:selected="onSelectedRows"
      @done="handleBatchDone"
    />

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">收货入库</span>
          <span class="panel-head__meta" data-testid="stockin-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">草稿可编辑 / 过账；过账后作废需复核（作废审批中锁定单据，审批通过才回滚六本账）</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="stockin-load-error"
      >
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
        :selected-row-keys="selectedIds"
        hover
        data-testid="stockin-table"
        @select-change="onSelectChange"
        @page-change="handlePageChange"
      >
        <template #in_no="{ row }">
          <span class="mono" :data-testid="`stockin-no-${row.in_no}`">{{ row.in_no }}</span>
        </template>

        <!-- D4/F3：来源采购单号可点，跳回采购单详情（没有来源就是「—」） -->
        <template #source_order_no="{ row }">
          <t-link
            v-if="row.source_order_no"
            theme="primary"
            class="mono"
            :data-testid="`stockin-po-link-${row.in_no}`"
            @click="goPurchaseOrder(row)"
          >
            {{ row.source_order_no }}
          </t-link>
          <!-- A6：无源单据（A4 的显式标记）在列表上要看得见，不能只是一个「—」 -->
          <t-tag
            v-else-if="row.is_sourceless"
            size="small"
            theme="warning"
            variant="outline"
            :data-testid="`stockin-sourceless-${row.in_no}`"
          >无源单据</t-tag>
          <span v-else class="muted">—</span>
        </template>

        <template #supplier_id="{ row }">{{ supplierNameOf(row.supplier_id) }}</template>

        <template #warehouse_id="{ row }">{{ warehouseNameOf(row.warehouse_id) }}</template>

        <template #in_date="{ row }">
          <span class="muted">{{ shortDate(row.in_date) }}</span>
        </template>

        <template #total_qty="{ row }">
          <span class="num" :data-testid="`stockin-qty-${row.in_no}`">{{ formatQty(row.total_qty) }}</span>
        </template>

        <!-- ★ 金额列同样标「(价税合计)」 -->
        <template #total_with_tax="{ row }">
          <span class="money" :data-testid="`stockin-total-${row.in_no}`">{{ formatMoney(row.total_with_tax) }}</span>
        </template>

        <template #status="{ row }">
          <div class="status-cell">
            <t-tag size="small" :theme="stockInStatusTheme(row.status)" :data-testid="`stockin-status-${row.in_no}`">
              {{ stockInStatusLabel(row.status) }}
            </t-tag>
            <!-- 过账后能看出「按可抵扣进项税入账」还是事后作废 -->
            <t-tag v-if="row.status === 'POSTED' && row.invoice_received" size="small" variant="outline" theme="success">
              发票已到
            </t-tag>
          </div>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-view-${row.in_no}`" @click="openDetail(row)">查看</t-link>
            <t-link
              v-if="actionsOf(row).edit"
              theme="primary"
              :data-testid="`btn-edit-${row.in_no}`"
              @click="openEditDialog(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="actionsOf(row).post"
              theme="primary"
              :disabled="!canPost(row)"
              :data-testid="`btn-post-${row.in_no}`"
              @click="openPostDialog(row)"
            >
              提交过账
            </t-link>
            <t-link
              v-if="actionsOf(row).cancel"
              theme="danger"
              :data-testid="`btn-cancel-${row.in_no}`"
              @click="confirmCancel(row)"
            >
              作废
            </t-link>
            <t-link
              v-if="actionsOf(row).withdrawCancel"
              theme="primary"
              :data-testid="`btn-withdraw-cancel-${row.in_no}`"
              @click="confirmWithdrawCancel(row)"
            >
              撤回作废申请
            </t-link>
            <t-link
              v-if="actionsOf(row).remove"
              theme="danger"
              :data-testid="`btn-remove-${row.in_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="stockin-empty">
            <t-icon name="inbox" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的收货单' : '还没有收货单' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个条件再查，或点右侧「重置」回全量' : '点左上角「新建收货单」录入第一张' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="form.id ? '编辑收货单' : '新建收货单'"
      width="900px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockin-form-dialog"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <!-- D4/F1：从采购单带出时把「源单 + 条数 + 草稿不自动过账」说清楚 -->
      <t-alert v-if="form.purchase_order_id" theme="info" class="dlg-alert" data-testid="stockin-source-po">
        <template #message>
          已从采购单 <strong class="mono">{{ form.purchase_order_no || form.purchase_order_id }}</strong>
          带出 <strong>{{ form.items.length }}</strong> 行待收明细：数量默认 = <strong>待收数量</strong>，
          可以改小（部分收货）或删掉这次不收的行。保存后是<strong>草稿</strong>，核对无误再点「过账」。
        </template>
      </t-alert>

      <!-- D9：复制带出的采购关联提示（后端决策 B：复制清空采购单，防待收额度二次占用） -->
      <t-alert v-if="copyNotice" theme="warning" class="dlg-alert" data-testid="stockin-copy-notice">
        {{ copyNotice }}
      </t-alert>

      <t-alert theme="info" class="dlg-alert" data-testid="stockin-cost-note">
        <template #message>
          入库成本只按<strong>不含税单价</strong>入账（移动加权），价税合计由系统按税率计算；过账时才写库存。
        </template>
      </t-alert>

      <t-form :data="form" label-width="84px">
        <!--
          A6：单据来源（采购入库 / 无源单据）。
          采购入库必须绑采购单（供应商由后端从采购单派生）；无源是 A4 的显式例外
          （期初 / 复制 / 其他），要自己指定供应商 + 备注写原因，后端 400 兜底。
        -->
        <div class="form-row">
          <t-form-item
            label="单据来源"
            name="sourceless"
            class="form-row__col"
            :help="Boolean(form.id) ? '编辑态不能改单据来源（否则已收数量与供应商口径会打架）' : ''"
          >
            <div class="source-mode">
              <t-radio-group
                :value="stockInSourceMode(form)"
                :disabled="Boolean(form.id)"
                data-testid="stockin-source-mode"
                @change="handleSourceModeChange"
              >
                <t-radio value="PURCHASE" data-testid="stockin-source-purchase">采购入库</t-radio>
                <t-radio value="SOURCELESS" data-testid="stockin-source-sourceless">无源单据</t-radio>
              </t-radio-group>
              <!-- 「无源单据」标记（A4 口径：无源必须可枚举，界面上要能一眼看出来） -->
              <t-tag
                v-if="showsSourcelessMark(form)"
                size="small"
                theme="warning"
                variant="outline"
                data-testid="stockin-sourceless-mark"
              >无源单据</t-tag>
            </div>
          </t-form-item>
        </div>

        <div class="form-row">
          <!-- 采购单：PURCHASE 型必选；选中后带出供应商 / 仓库 / 待收明细 -->
          <t-form-item
            v-if="!form.sourceless"
            label="采购单"
            name="purchase_order_id"
            class="form-row__col"
            :status="formErrors.purchase_order_id ? 'error' : undefined"
            :tips="formErrors.purchase_order_id"
          >
            <div class="readonly-field">
              <t-select
                v-model="form.purchase_order_id"
                :options="purchaseOrderOptions"
                :disabled="Boolean(form.id)"
                placeholder="选择采购单（只列已审批 / 部分收货的）"
                filterable
                data-testid="stockin-purchase-order"
                @change="handlePurchaseOrderChange"
              />
              <p v-if="form.purchase_order_no" class="field-note" data-testid="stockin-purchase-order-no">
                采购单号：<span class="mono">{{ form.purchase_order_no }}</span>
              </p>
            </div>
          </t-form-item>

          <t-form-item
            label="供应商"
            name="supplier_id"
            class="form-row__col"
            :status="formErrors.supplier_id ? 'error' : undefined"
            :tips="formErrors.supplier_id"
          >
            <!-- A4：采购入库的供应商只读（值来自采购单，后端已派生；前端不再传 supplier_id） -->
            <div v-if="isSupplierReadonly(form)" class="readonly-field">
              <t-input
                :value="supplierText"
                readonly
                :placeholder="'请先选择采购单'"
                data-testid="stockin-supplier-readonly"
              />
              <p class="field-note" data-testid="stockin-supplier-hint">{{ supplierHintText(form) }}</p>
            </div>
            <!-- 无源单据：没有采购单可带出，后端要求自己指定（AP-4 的例外路径） -->
            <t-select
              v-else
              v-model="form.supplier_id"
              :options="supplierOptions"
              placeholder="选择供应商"
              filterable
              data-testid="stockin-supplier"
            />
          </t-form-item>

          <t-form-item
            label="收货仓库"
            name="warehouse_id"
            class="form-row__col"
            :status="formErrors.warehouse_id ? 'error' : undefined"
            :tips="formErrors.warehouse_id"
          >
            <div class="readonly-field">
              <t-select
                v-model="form.warehouse_id"
                :options="warehouseOptions"
                :disabled="Boolean(form.purchase_order_id)"
                placeholder="选择收货仓库"
                filterable
                data-testid="stockin-warehouse"
                @change="handleWarehouseChange"
              />
              <!-- A6：带源单时仓库由采购单带出（后端要求两者一致，改了就是 400） -->
              <p v-if="form.purchase_order_id" class="field-note" data-testid="stockin-warehouse-hint">
                仓库来自采购单，不可修改
              </p>
            </div>
          </t-form-item>
        </div>

        <div class="form-row">
          <t-form-item
            label="入库日期"
            name="in_date"
            class="form-row__col"
            :status="formErrors.in_date ? 'error' : undefined"
            :tips="formErrors.in_date"
          >
            <t-date-picker
              v-model="form.in_date"
              value-type="YYYY-MM-DD"
              placeholder="选择日期"
              data-testid="stockin-date"
            />
          </t-form-item>

          <t-form-item label="发票" name="invoice_received" class="form-row__col">
            <t-radio-group v-model="form.invoice_received" :disabled="true" data-testid="stockin-invoice">
              <t-radio :value="true">发票已随货到达</t-radio>
              <t-tooltip :content="ESTIMATE_DISABLED_TIP">
                <span class="disabled-radio"><t-radio :value="false" :disabled="true">发票未到（暂估）</t-radio></span>
              </t-tooltip>
            </t-radio-group>
          </t-form-item>
        </div>

        <t-form-item label="备注" name="remark" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark">
          <t-input
            v-model="form.remark"
            :maxlength="STOCKIN_MAX_REMARK"
            placeholder="可空，如：到货验收合格"
            data-testid="stockin-remark"
          />
        </t-form-item>

        <!-- ---------------- 明细 ---------------- -->
        <div class="items">
          <div class="items__head">
            <span class="items__title">明细</span>
            <span class="items__hint">数量必须 > 0；单价填不含税单价</span>
            <!-- ★ P2-4：批量设置库位（一次给多行设同一个） -->
            <div class="items__batch" data-testid="stockin-batch-location">
              <span class="items__batch-label">批量设库位</span>
              <t-select
                v-model="batchLocationId"
                :options="locationOptions"
                :disabled="!form.warehouse_id"
                :placeholder="form.warehouse_id ? '选择库位' : '先选收货仓库'"
                size="small"
                clearable
                filterable
                class="items__batch-select"
                data-testid="stockin-batch-location-select"
              />
              <t-button
                size="small"
                variant="outline"
                :disabled="!batchLocationId"
                data-testid="btn-stockin-batch-location"
                @click="handleBatchSetLocation"
              >
                应用到全部行
              </t-button>
            </div>
          </div>

          <div class="items__grid items__grid--head">
            <span>物料</span>
            <span>数量</span>
            <span>不含税单价</span>
            <span>税率</span>
            <span>库位</span>
            <span class="ta-right">{{ AMOUNT_COLUMN_TITLE }}</span>
            <span class="ta-right">其中税额</span>
            <span />
          </div>

          <div
            v-for="(item, index) in form.items"
            :key="item.key"
            class="items__row"
            :data-testid="`stockin-item-${index}`"
          >
            <div class="items__grid">
              <div class="items__cell">
                <t-select
                  v-model="item.material_id"
                  :options="materialOptions"
                  placeholder="选择物料"
                  filterable
                  :data-testid="`stockin-item-material-${index}`"
                />
                <p v-if="rowErrors(index).material_id" class="cell-error">{{ rowErrors(index).material_id }}</p>
              </div>

              <div class="items__cell">
                <t-input-number
                  v-model="item.qty"
                  :min="0"
                  :max="item.pending ? item.pending.pending_qty : 9999999"
                  theme="normal"
                  :data-testid="`stockin-item-qty-${index}`"
                />
                <!-- D4/F2：把上限摆在数量下面（订单 / 已收 / 待收），改数量时不用翻采购单 -->
                <p v-if="item.pending" class="cell-hint" :data-testid="`stockin-item-pending-${index}`">
                  订单 {{ formatQty(item.pending.ordered_qty) }} /
                  已收 {{ formatQty(item.pending.received_qty) }} /
                  待收 <strong>{{ formatQty(item.pending.pending_qty) }}</strong>
                </p>
                <p v-if="rowErrors(index).qty" class="cell-error">{{ rowErrors(index).qty }}</p>
              </div>

              <div class="items__cell">
                <!-- 同采购页：不锁 6 位小数，避免「2」显示成「2.000000」 -->
                <t-input-number
                  v-model="item.price_excl"
                  :min="0"
                  theme="normal"
                  :data-testid="`stockin-item-price-${index}`"
                />
                <p v-if="rowErrors(index).price_excl" class="cell-error">{{ rowErrors(index).price_excl }}</p>
              </div>

              <div class="items__cell">
                <t-select
                  v-model="item.tax_rate"
                  :options="taxRateOptions"
                  :data-testid="`stockin-item-tax-${index}`"
                />
                <p v-if="rowErrors(index).tax_rate" class="cell-error">{{ rowErrors(index).tax_rate }}</p>
              </div>

              <!-- ★ P2-4：库位（按当前收货仓库过滤；留空 = 用仓库默认库位 / 未分配桶） -->
              <div class="items__cell">
                <t-select
                  v-model="item.location_dict_id"
                  :options="locationOptions"
                  :disabled="!form.warehouse_id"
                  :placeholder="form.warehouse_id ? '默认库位' : '先选仓库'"
                  clearable
                  filterable
                  :data-testid="`stockin-item-location-${index}`"
                />
                <p v-if="form.warehouse_id && locationOptions.length === 0" class="cell-hint">
                  该仓库还没有库位
                </p>
              </div>

              <div class="items__cell items__cell--num">
                <span class="money" :data-testid="`stockin-item-amount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(item).total) }}
                </span>
              </div>

              <div class="items__cell items__cell--num">
                <span class="muted" :data-testid="`stockin-item-taxamount-${index}`">
                  {{ formatMoney(lineAmountsOfItem(item).tax) }}
                </span>
              </div>

              <div class="items__cell items__cell--op">
                <t-tooltip content="删除该行">
                  <t-button
                    variant="text"
                    shape="square"
                    :disabled="form.items.length <= 1"
                    :data-testid="`stockin-item-remove-${index}`"
                    @click="removeItem(index)"
                  >
                    <template #icon><t-icon name="delete" /></template>
                  </t-button>
                </t-tooltip>
              </div>
            </div>
          </div>

          <t-button variant="dashed" block data-testid="btn-stockin-add-item" @click="addItem">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
          <p v-if="formErrors.items" class="cell-error">{{ formErrors.items }}</p>
        </div>

        <div class="totals" data-testid="stockin-totals">
          <span class="totals__item">
            合计数量 <strong data-testid="stockin-total-qty">{{ formatQty(totals.qty) }}</strong>
          </span>
          <span class="totals__item">
            价款 <strong data-testid="stockin-total-amount">{{ formatMoney(totals.amount) }}</strong>
          </span>
          <span class="totals__item">
            税额 <strong data-testid="stockin-total-tax">{{ formatMoney(totals.tax) }}</strong>
          </span>
          <span class="totals__item totals__item--strong">
            {{ AMOUNT_COLUMN_TITLE }} <strong data-testid="stockin-total-with-tax">{{ formatMoney(totals.total) }}</strong>
          </span>
        </div>

        <!-- O1 表单记忆提示：带出了上次的供应商 / 仓库就明说，并留「重置」出口 -->
        <FormMemoryHint
          v-if="!form.id"
          :labels="memoryLabels"
          testid="stockin-form-memory"
          @reset="handleResetMemory"
        />

        <div class="dlg-footer">
          <t-button variant="outline" data-testid="btn-stockin-cancel" @click="formVisible = false">取消</t-button>
          <t-button
            v-privilege="[form.id ? PERM.update : PERM.create]"
            theme="primary"
            :loading="saving"
            data-testid="btn-stockin-save"
            @click="save"
          >
            保存草稿
          </t-button>
        </div>
      </t-form>
    </t-dialog>

    <!-- ==================== ④ ★ 过账确认弹窗（PRD §3.3.2） ==================== -->
    <t-dialog
      v-model:visible="postVisible"
      header="确认过账"
      width="560px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockin-post-dialog"
    >
      <div class="post-body">
        <div class="post-kv">
          <span class="post-kv__label">收货单</span>
          <span class="post-kv__value mono" data-testid="stockin-post-no">{{ postTarget?.in_no ?? '—' }}</span>
        </div>
        <div class="post-kv">
          <span class="post-kv__label">金额</span>
          <span class="post-kv__value money" data-testid="stockin-post-total">
            价税合计 {{ formatMoney(postTarget?.total_with_tax) }}
          </span>
        </div>

        <!-- 两个发票选项：已随货到达默认勾选；「发票未到」置灰 + tooltip -->
        <div class="post-invoice" data-testid="stockin-post-invoice">
          <div
            v-for="option in INVOICE_OPTIONS"
            :key="option.value"
            class="post-invoice__row"
            :class="{ 'post-invoice__row--disabled': option.disabled }"
          >
            <t-tooltip :content="option.tip" :disabled="!option.disabled">
              <span class="post-invoice__inner">
                <t-checkbox
                  :checked="invoiceChoice === option.value"
                  :disabled="option.disabled"
                  :data-testid="`stockin-invoice-${option.value.toLowerCase()}`"
                  @change="handleInvoiceChoice(option.value, option.disabled)"
                >
                  <span class="post-invoice__label">{{ option.label }}</span>
                </t-checkbox>
                <span class="post-invoice__desc">（{{ option.desc }}）</span>
                <t-tag v-if="option.disabled" size="small" variant="outline" theme="default">本批不可选</t-tag>
              </span>
            </t-tooltip>
          </div>
        </div>

        <t-alert theme="warning" class="post-warn" data-testid="stockin-post-consequences">
          <template #message>
            <div class="post-warn__title"><t-icon name="error-circle" /> 过账后将：</div>
            <ul class="post-warn__list">
              <li v-for="line in POST_CONSEQUENCES" :key="line">{{ line }}</li>
            </ul>
          </template>
        </t-alert>
      </div>

      <div class="dlg-footer">
        <t-button variant="outline" data-testid="btn-stockin-post-cancel" @click="postVisible = false">取消</t-button>
        <t-button
          v-privilege="[PERM.status]"
          theme="primary"
          :loading="posting"
          data-testid="btn-stockin-post-confirm"
          @click="confirmPost"
        >
          确认过账
        </t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="收货单详情"
      size="720px"
      :footer="false"
      data-testid="stockin-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail">
        <!--
          R-P1-5：关键动作吸顶（用户原话「不能满屏去找按钮」）。
          过账 / 作废原来只长在列表行上，点进详情一个都看不见。
        -->
        <DocActionBar title="关键操作" testid="stockin-action-bar">
          <t-button
            size="small"
            theme="primary"
            :disabled="!actionsOf(detail.header).post || !canPost(detail.header)"
            data-testid="btn-stockin-detail-post"
            @click="openPostDialog(detail.header)"
          >
            <template #icon><t-icon name="file-paste" /></template>
            过账
          </t-button>
          <t-button
            size="small"
            theme="danger"
            variant="outline"
            :disabled="!actionsOf(detail.header).cancel"
            data-testid="btn-stockin-detail-cancel"
            @click="confirmCancel(detail.header)"
          >
            <template #icon><t-icon name="delete" /></template>
            作废
          </t-button>
          <t-button
            size="small"
            theme="warning"
            variant="outline"
            :disabled="!actionsOf(detail.header).withdrawCancel"
            data-testid="btn-stockin-detail-withdraw-cancel"
            @click="confirmWithdrawCancel(detail.header)"
          >
            撤回作废申请
          </t-button>
        </DocActionBar>
        <div class="detail__head">
          <span class="detail__no mono">{{ detail.header.in_no }}</span>
          <t-tag size="small" :theme="stockInStatusTheme(detail.header.status)">
            {{ detail.header.status_label || stockInStatusLabel(detail.header.status) }}
          </t-tag>
          <t-tag size="small" variant="outline" :theme="detail.header.invoice_received ? 'success' : 'warning'">
            {{ detail.header.invoice_received ? '发票已到' : '发票未到（暂估）' }}
          </t-tag>
        </div>

        <div class="detail__grid">
          <!-- A6：单据来源（采购入库 / 无源单据）—— 无源是 A4 的显式标记，详情里要说出来 -->
          <div class="detail-field">
            <span class="detail-field__label">单据来源</span>
            <span class="detail-field__value" data-testid="stockin-detail-source">
              <t-tag
                v-if="detail.header.is_sourceless"
                size="small"
                theme="warning"
                variant="outline"
                data-testid="stockin-detail-sourceless-mark"
              >无源单据</t-tag>
              <span v-else>采购入库</span>
            </span>
          </div>
          <div class="detail-field"><span class="detail-field__label">供应商</span><span class="detail-field__value">{{ supplierNameOf(detail.header.supplier_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">收货仓库</span><span class="detail-field__value">{{ warehouseNameOf(detail.header.warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">入库日期</span><span class="detail-field__value">{{ detail.header.in_date }}</span></div>
          <div class="detail-field"><span class="detail-field__label">入库类型</span><span class="detail-field__value">{{ detail.header.in_type }}</span></div>
          <!-- D4/F3：来源采购单可点，跳回采购单详情 -->
          <div class="detail-field">
            <span class="detail-field__label">来源采购单</span>
            <span class="detail-field__value">
              <t-link
                v-if="detail.purchase_order_no || detail.purchase_order_id"
                theme="primary"
                class="mono"
                data-testid="stockin-detail-po-link"
                @click="goPurchaseOrderById(detail.purchase_order_id, detail.purchase_order_no)"
              >
                {{ detail.purchase_order_no || shortId(detail.purchase_order_id) }}
              </t-link>
              <span v-else class="muted">—（无源直收）</span>
            </span>
          </div>
          <div class="detail-field"><span class="detail-field__label">合计数量</span><span class="detail-field__value">{{ formatQty(detail.header.total_qty) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">序列号件数</span><span class="detail-field__value">{{ detail.serial_count }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价款</span><span class="detail-field__value">{{ formatMoney(detail.header.total_amount) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">税额</span><span class="detail-field__value">{{ formatMoney(detail.header.total_tax) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">价税合计</span><span class="detail-field__value money">{{ formatMoney(detail.header.total_with_tax) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">过账时间</span><span class="detail-field__value">{{ dash(detail.header.posted_at) }}</span></div>
          <div class="detail-field detail-field--wide"><span class="detail-field__label">备注</span><span class="detail-field__value">{{ dash(detail.header.remark) }}</span></div>
          <div v-if="detail.header.cancel_reason" class="detail-field detail-field--wide">
            <span class="detail-field__label">作废原因 / 申请理由</span><span class="detail-field__value">{{ detail.header.cancel_reason }}</span>
          </div>
        </div>

        <p v-if="detail.summary" class="detail__summary">{{ detail.summary }}</p>

        <!-- ★ P2-4：库位提示（未分配 / 超容量）—— 服务端现算，只提示不拦截 -->
        <t-alert
          v-if="detail.location_warnings?.length"
          theme="warning"
          :message="`库位提示（${detail.location_warnings.length} 条）`"
          data-testid="stockin-detail-location-warnings"
        >
          <template #description>
            <ul class="location-warnings">
              <li v-for="(text, index) in detail.location_warnings" :key="index">{{ text }}</li>
            </ul>
          </template>
        </t-alert>

        <!--
          P16：作废审批进度（复用审批中心的 FlowProgress，进度只来自引擎写的节点行）。
          C2：与采购 / 应付 / 凭证 / 调拨统一口径 —— 节点进度 + 审批人明细（谁 / 时间 / 意见 / 跳过说明）
          都来自同一个 `GET /business/flow/instance/{id}`，`tasks` 就是它的 `timeline[]`。
        -->
        <template v-if="hasApprovalProgress(approvalProgress)">
          <div class="section-title">作废审批进度</div>
          <FlowProgress
            :nodes="approvalProgress.nodes"
            :tasks="approvalProgress.tasks"
            data-testid="stockin-cancel-progress"
          />
        </template>

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table row-key="id" size="small" :data="detail.items" :columns="detailColumns" data-testid="stockin-detail-items">
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }">{{ formatQty(row.qty) }}</template>
          <template #price="{ row }">{{ formatMoney(row.price) }}</template>
          <template #tax_rate="{ row }">{{ formatQty(row.tax_rate) }}%</template>
          <template #amount="{ row }">{{ formatMoney(row.amount) }}</template>
          <template #tax_amount="{ row }">{{ formatMoney(row.tax_amount) }}</template>
          <template #with_tax_amount="{ row }">{{ formatMoney(row.with_tax_amount) }}</template>
          <template #cost_price="{ row }">{{ formatMoney(row.cost_price) }}</template>
          <template #location_code="{ row }">
            <span :data-testid="`stockin-detail-location-${row.line_no}`">
              {{ row.location_code || '未分配' }}
            </span>
          </template>
        </t-table>

        <!-- D9：复制为新单（原单只读，后端建新草稿；权限 = 建单权限） -->
        <div class="detail__actions" data-testid="stockin-drawer-actions">
          <t-button
            v-privilege="[PERM.create]"
            theme="primary"
            variant="outline"
            :loading="copying"
            :disabled="copying"
            data-testid="btn-stockin-copy"
            @click="copyCurrent"
          >
            复制为新单
          </t-button>
          <span class="detail__actions-hint">
            复制会新建一张<strong>草稿</strong>：明细全带出，单号重新取，采购单关联清空
          </span>
        </div>

        <!-- 附件：签收单 / 验收报告 / 发票影像（ARCH-3 DC-17）—— 四个单据详情页共用同一组件 -->
        <div class="section-title">附件</div>
        <DocAttachmentPanel
          doc-type="STOCK_IN"
          :doc-id="detail.header.id"
          default-role="SIGN_RECEIPT"
          testid-prefix="stockin-attach"
        />
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  copyStockIn,
  createStockIn,
  deleteStockIn,
  fetchMaterialPage,
  fetchLocationList,
  fetchPurchaseOrderPage,
  fetchPurchaseOrderPendingItems,
  fetchStockInDetail,
  fetchStockInPage,
  fetchSupplierOptions,
  fetchWarehousePage,
  submitStockInPost,
  updateStockIn,
  updateStockInStatus,
  withdrawStockInCancel,
  type MaterialRow,
  type Location,
  type PurchaseOrderRow,
  type StockInDetail,
  type StockInRow,
  type Supplier,
  type Warehouse,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import {
  stockInBatchDelete,
  stockInBatchExport,
  stockInBatchStatus,
} from '@/api/business/batch';
import BatchActionBar from '@/components/BatchActionBar.vue';
import FormMemoryHint from '@/components/FormMemoryHint.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { useBatchSelection } from '@/composables/useBatchSelection';
import { useUserStore } from '@/stores/user';
import type {
  BatchActionSpec,
  BatchExportTask,
  BatchResult,
  BatchRow,
} from '@/utils/batch-action';
import { pickFields, resolveDefaults } from '@/composables/memory-core';
import { useFormMemory } from '@/composables/useFormMemory';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import DocActionBar from '@/views/business/erp/shared/DocActionBar.vue';
import {
  EMPTY_APPROVAL_PROGRESS,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import {
  AMOUNT_COLUMN_TITLE,
  ESTIMATE_DISABLED_TIP,
  INVOICE_OPTIONS,
  POST_CONSEQUENCES,
  STOCKIN_MAX_REMARK,
  STOCKIN_MAX_LINES,
  STOCKIN_PAGE_SIZE,
  STOCKIN_STATUS_FILTER_OPTIONS,
  applySourceMode,
  applyLocationToAllLines,
  availableStockInActions,
  buildCancelPayload,
  buildPostPayload,
  buildStockInPayload,
  buildStockInQuery,
  canPost,
  cancelConfirmMessage,
  computeTotals,
  dash,
  defaultInvoiceChoice,
  detailToForm,
  defaultLocationIdOf,
  emptyStockInFilter,
  emptyStockInForm,
  emptyStockInItem,
  fillDefaultLocation,
  fillFromPending,
  formatMoney,
  formatQty,
  formFromPending,
  isStockInFiltered,
  isSupplierReadonly,
  lineAmountsOfItem,
  locationSelectOptions,
  purchaseOrderOptionLabel,
  selectablePurchaseOrders,
  shortDate,
  shortId,
  showsSourcelessMark,
  stockInSourceMode,
  stockInStatusLabel,
  stockinCopyNotice,
  stockInStatusTheme,
  supplierDisplayText,
  supplierHintText,
  validateStockInForm,
  type StockInFilterState,
  type StockInFormErrors,
  type StockInFormState,
  type StockInItemForm,
} from './stockin-logic';

/** 本页权限码（与 V0147 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:stock-in:list',
  create: 'business:erp:stock-in:create',
  update: 'business:erp:stock-in:update',
  status: 'business:erp:stock-in:status',
  remove: 'business:erp:stock-in:delete',
} as const;

/** 当前用户（批量动作的权限判定用它，与单条按钮同一套码）。 */
const userStore = useUserStore();

/** 主数据下拉一次拉多少 */
const OPTION_LIMIT = 200;

/** 表格列（卡面 §四：in_no / 供应商 / 仓库 / 入库日期 / 数量 / 金额 / 状态 / 操作） */
const columns: PrimaryTableCol[] = [
  // D7：勾选列。「没有任何可用批量动作」的行复选框直接禁用（已过账不能过账，但仍可作废）
  {
    colKey: 'row-select',
    type: 'multiple',
    width: 50,
    disabled: (options: { row: unknown }) => !isSelectable(options.row as StockInRow),
  },
  { colKey: 'in_no', title: '收货单号', width: 210 },
  // D4/F3：来源采购单（列上直接给单号，不用点进详情才知道这批货是给哪张单收的）
  { colKey: 'source_order_no', title: '来源采购单', width: 190, ellipsis: true },
  { colKey: 'supplier_id', title: '供应商', minWidth: 150, ellipsis: true },
  { colKey: 'warehouse_id', title: '仓库', width: 140, ellipsis: true },
  { colKey: 'in_date', title: '入库日期', width: 100 },
  { colKey: 'total_qty', title: '数量', width: 90, align: 'right' },
  // ★ 金额列标题带「(价税合计)」
  { colKey: 'total_with_tax', title: AMOUNT_COLUMN_TITLE, width: 150, align: 'right' },
  { colKey: 'status', title: '状态', width: 150 },
  { colKey: 'op', title: '操作', width: 210, fixed: 'right' },
];

/** 详情抽屉里的明细列 */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 50, align: 'center' },
  { colKey: 'material_id', title: '物料', minWidth: 150, ellipsis: true },
  // ★ P2-4：库位放在物料右边（抽屉宽度有限，放最后一列会被横向滚出视野 ——
  //   而「这批货进了哪个库位」正是用户点开详情最想看的两件事之一）
  { colKey: 'location_code', title: '库位', width: 120, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 80, align: 'right' },
  { colKey: 'price', title: '不含税单价', width: 110, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 70, align: 'right' },
  { colKey: 'amount', title: '不含税金额', width: 110, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 90, align: 'right' },
  { colKey: 'with_tax_amount', title: AMOUNT_COLUMN_TITLE, width: 130, align: 'right' },
  { colKey: 'cost_price', title: '成本单价', width: 110, align: 'right' },
];

/** 状态下拉 */
const statusFilterOptions = STOCKIN_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

/** 税率下拉 */
const taxRateOptions = [
  { label: '13%', value: 13 },
  { label: '9%', value: 9 },
  { label: '6%', value: 6 },
  { label: '3%', value: 3 },
  { label: '0%（免税）', value: 0 },
];

// ==================== 记忆字段（O1） ====================

/**
 * 表单记忆字段。
 *
 * 卡面写的 `in_type` 在本仓的收货单里**不存在**（收货类型由入库单自身语义决定），
 * 等价的「这一步常选一次就一直是它」的字段是 `invoice_received`（发票是否随货到），
 * 所以这里记它 —— 而不是硬造一个后端不认的 `in_type`。
 */
const MEMORY_FIELDS: readonly (keyof StockInFormState)[] = [
  'supplier_id',
  'warehouse_id',
  'invoice_received',
];

/** 记忆字段中文名（提示条文案） */
const MEMORY_LABELS: Record<string, string> = {
  supplier_id: '供应商',
  warehouse_id: '仓库',
  invoice_received: '发票随货选项',
};

/** 明细行记忆：新行沿用上一次的税率 */
const LINE_MEMORY_FIELDS: readonly (keyof StockInItemForm)[] = ['tax_rate'];

/** 明细行记忆中文名 */
const LINE_MEMORY_LABELS: Record<string, string> = { tax_rate: '税率' };

// ==================== 列表 ====================

const filter = reactive<StockInFilterState>(emptyStockInFilter());
const rows = ref<StockInRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = STOCKIN_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

// ==================== 批量操作（D7） ====================

/** 一次「选中全部筛选结果」拉多少主键（后端单页上限 200）。 */
const BATCH_FETCH_PAGE_SIZE = 200;

/**
 * 批量动作声明。
 *
 * `allowedStatus` 与后端状态机**逐字一致**：过账只认 `DRAFT`，作废认 `DRAFT` / `POSTED`，
 * 删除只认草稿与过账审批中（`PENDING_POST`）。状态不符合的按钮由组件自动禁用并说明原因。
 */
const BATCH_ACTIONS: BatchActionSpec[] = [
  {
    key: 'post',
    label: '批量过账',
    theme: 'primary',
    allowedStatus: ['DRAFT'],
    targetStatus: 'POSTED',
  },
  {
    key: 'cancel',
    label: '批量作废',
    theme: 'danger',
    allowedStatus: ['DRAFT', 'POSTED'],
    targetStatus: 'CANCELLED',
    needConfirm: true,
    requireReason: true,
  },
  {
    key: 'delete',
    label: '批量删除',
    theme: 'danger',
    allowedStatus: ['DRAFT', 'PENDING_POST'],
    needConfirm: true,
  },
  { key: 'export', label: '批量导出', theme: 'default', exportKind: true },
];

/** 动作 → 权限码（没有码的按钮禁用 + tooltip 说明，与单条按钮同一套码）。 */
const BATCH_PERMISSIONS: Record<string, string> = {
  post: PERM.status,
  cancel: PERM.status,
  delete: PERM.remove,
  // 批量导出 = 建异步导出任务（写操作）：后端的路由码是 impexp 的 create 码，
  // 导出的数据范围仍受「列表可见性」约束（后端函数内再判 stock-in:list）
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
 * 后端**逐条独立事务**，所以一批里某条失败不影响同批其它条，失败原因逐条回传。
 */
async function runBatchAction(
  action: BatchActionSpec,
  ids: string[],
  payload: { reason?: string },
): Promise<BatchResult | BatchExportTask> {
  if (action.key === 'export') {
    return stockInBatchExport({ ids, keyword: filter.keyword || undefined });
  }
  if (action.key === 'delete') {
    return stockInBatchDelete(ids);
  }
  return stockInBatchStatus({ ids, status: action.targetStatus, reason: payload.reason });
}

/** 「选中全部 N 条筛选结果」：按当前筛选把全部主键拉回来（最多 1 万条，够点一次活）。 */
async function fetchAllRowIds(): Promise<BatchRow[]> {
  const acc: BatchRow[] = [];
  for (let page = 1; page <= 50; page += 1) {
    const data = await fetchStockInPage(buildStockInQuery(filter, page, BATCH_FETCH_PAGE_SIZE));
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
  void loadOrders(1);
}

/**
 * 列表记忆（O4）：进页面恢复筛选 + 页码，改了防抖 300ms 写 sessionStorage。
 * 构造即恢复，早于 `onMounted` 的首次加载。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<StockInFilterState>({
  listName: 'erp-stockin',
  filter,
  page: current,
  pageSize,
});

/** 表单记忆（O1）：供应商 / 仓库 / 发票随货，优先级 源单 > 记忆 > 默认 */
const formMemory = useFormMemory<StockInFormState>('erp-stockin', MEMORY_FIELDS, {
  labels: MEMORY_LABELS,
});
/** 明细行记忆：税率 */
const lineMemory = useFormMemory<StockInItemForm>('erp-stockin-line', LINE_MEMORY_FIELDS, {
  labels: LINE_MEMORY_LABELS,
});
/** 本次新建带出的记忆字段中文名（空 = 不显示提示条） */
const memoryLabels = ref<string[]>([]);

const suppliers = ref<Supplier[]>([]);
const warehouses = ref<Warehouse[]>([]);
const materials = ref<MaterialRow[]>([]);

// ==================== P2-4：库位（按当前收货仓库过滤） ====================

/** 当前收货仓库下的库位（`fetchLocationList(warehouse_id, false)`：只取启用的） */
const locations = ref<Location[]>([]);
/** 批量设置库位用的选择值（空串 = 未选） */
const batchLocationId = ref('');

/** 库位下拉选项（只列启用的；默认库位带「（默认）」标记） */
const locationOptions = computed(() => locationSelectOptions(locations.value));

/** 该仓默认库位 id（没有 → 空串）：新增行 / 换仓库时自动带出 */
const defaultLocationId = computed(() => defaultLocationIdOf(locations.value));

/**
 * 拉当前仓库的库位。
 *
 * 换仓库 / 打开表单 / 从采购单带出时都要重拉：库位是**仓库内唯一**的，
 * 拿旧仓库的库位去提交必然 400「库位属于仓库 X，与收货仓库 Y 不一致」。
 *
 * @param fillRows 是否把「还没选库位」的行补成默认库位（编辑已有草稿时**不要**补，
 *                 否则会把用户保存过的选择洗成默认值）
 */
async function loadLocations(fillRows = false): Promise<void> {
  const warehouseId = form.warehouse_id;
  if (!warehouseId) {
    locations.value = [];
    batchLocationId.value = '';
    return;
  }
  try {
    locations.value = await fetchLocationList(warehouseId, false);
  } catch {
    // 主数据拉不到不该挡住建单：下拉空着，用户仍可提交（后端按默认库位兜底）
    locations.value = [];
  }
  if (fillRows) {
    fillDefaultLocation(form, defaultLocationId.value);
  }
}

/** 仓库变化 → 重拉库位 + 给空行补默认库位（用户已选的行不动） */
function handleWarehouseChange(): void {
  batchLocationId.value = '';
  void loadLocations(true);
}

/** 批量设置库位（一次给所有明细行设同一个） */
function handleBatchSetLocation(): void {
  if (!batchLocationId.value) {
    MessagePlugin.warning('请先选择要批量设置的库位');
    return;
  }
  applyLocationToAllLines(form, batchLocationId.value);
  MessagePlugin.success(`已把 ${form.items.length} 行的库位设为同一库位`);
}

/**
 * 可收货的采购单（A6：直接建收货单时选源单）。
 *
 * 只取一页（`OPTION_LIMIT`）再本地筛状态，筛选口径在 `selectablePurchaseOrders`
 * （`APPROVED` / `PARTIAL_RECEIVED`，卡面口径）；有没有待收行由后端 `pending-items` 最终裁定。
 */
const purchaseOrders = ref<PurchaseOrderRow[]>([]);

const filtered = computed(() => isStockInFiltered(filter));

const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: `${item.supplier_name}（${item.supplier_code}）`, value: item.id })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

/** 采购单下拉（A6）：只列已审批 / 部分收货的单，文案 = 单号 + 供应商 + 状态 */
const purchaseOrderOptions = computed(() =>
  selectablePurchaseOrders(purchaseOrders.value).map((row) => ({
    label: purchaseOrderOptionLabel(row),
    value: row.id,
  })),
);

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 供应商名称 */
function supplierNameOf(supplierId: string | null | undefined): string {
  if (!supplierId) {
    return '—';
  }
  const hit = suppliers.value.find((item) => item.id === supplierId);
  return hit ? hit.supplier_name : shortId(supplierId);
}

/** 仓库名称 */
function warehouseNameOf(warehouseId: string | null | undefined): string {
  if (!warehouseId) {
    return '—';
  }
  const hit = warehouses.value.find((item) => item.id === warehouseId);
  return hit ? hit.warehouse_name : shortId(warehouseId);
}

/** 物料名称 */
function materialNameOf(materialId: string): string {
  const hit = materials.value.find((item) => item.id === materialId);
  return hit ? `${hit.material_name}（${hit.material_code}）` : shortId(materialId);
}

/** 行按钮显隐 */
function actionsOf(row: StockInRow): ReturnType<typeof availableStockInActions> {
  return availableStockInActions(row.status);
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表 */
async function loadOrders(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchStockInPage(buildStockInQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
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

/** 主数据 */
async function loadBasics(): Promise<void> {
  const [supplierList, warehousePage, materialPage, poPage] = await Promise.all([
    fetchSupplierOptions(OPTION_LIMIT).catch(() => [] as Supplier[]),
    fetchWarehousePage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    // A6：采购单下拉（状态在前端筛；没权限 / 失败只让下拉为空，不影响收货页其它功能）
    fetchPurchaseOrderPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
  ]);
  suppliers.value = supplierList;
  warehouses.value = warehousePage?.list ?? [];
  materials.value = materialPage?.list ?? [];
  purchaseOrders.value = poPage?.list ?? [];
}

function handleSearch(): void {
  void loadOrders(1);
}

function handleReset(): void {
  Object.assign(filter, emptyStockInFilter());
  void loadOrders(1);
}

/** 列表记忆 · 「清空筛选」：条件回全量 + 页码回 1 + 删掉 sessionStorage 记忆 */
function handleClearListMemory(): void {
  Object.assign(filter, emptyStockInFilter());
  current.value = 1;
  forgetListMemory();
  void loadOrders(1);
}

function refresh(): void {
  void loadOrders();
  void loadBasics();
}

function handlePageChange(pageInfo: { current?: number }): void {
  void loadOrders(pageInfo.current ?? 1);
}

// ==================== 表单 ====================

const formVisible = ref(false);
const saving = ref(false);
const submitError = ref('');
const form = reactive<StockInFormState>(emptyStockInForm());
const formErrors = ref<StockInFormErrors>({});

/** 供应商只读展示值：采购单带出 → 名称；还没选采购单 → 「请先选择采购单」 */
const supplierText = computed(() => supplierDisplayText(form, (id) => supplierNameOf(id)));

/**
 * 切换单据来源（A6）：采购入库 ⇄ 无源单据。
 *
 * 切到无源 → 清来源单、供应商转手选；切回采购入库 → 清供应商等采购单带出。
 * 不清就会「无源单带着 source_order_id」或「采购单还挂着上次手选的供应商」。
 */
function handleSourceModeChange(value: unknown): void {
  applySourceMode(form, value === 'SOURCELESS');
  delete formErrors.value.supplier_id;
  delete formErrors.value.purchase_order_id;
  delete formErrors.value.remark;
}

/**
 * 在表单里选采购单（A6）：拉待收明细 → 供应商 / 仓库 / 明细行一次带出。
 *
 * 与「从采购单详情点生成收货单」走同一个后端接口，差别只是**不重置单据 id**
 * （编辑草稿时换源单，改的还是这张草稿）。
 */
async function handlePurchaseOrderChange(value: unknown): Promise<void> {
  const poId = typeof value === 'string' ? value : '';
  delete formErrors.value.purchase_order_id;
  if (!poId) {
    form.purchase_order_id = '';
    form.purchase_order_no = '';
    form.supplier_id = '';
    form.supplier_name = '';
    return;
  }
  loading.value = true;
  try {
    const pending = await fetchPurchaseOrderPendingItems(poId);
    fillFromPending(form, pending);
    const lineCount = form.items.filter((item) => item.purchase_order_item_id).length;
    if (lineCount === 0) {
      MessagePlugin.warning(pending.blocked_reason ?? `采购单 ${pending.order.order_no} 没有可收的待收明细`);
    } else {
      MessagePlugin.success(`已从采购单 ${pending.order.order_no} 带出 ${lineCount} 行待收明细`);
    }
  } catch (error) {
    form.purchase_order_id = '';
    form.purchase_order_no = '';
    MessagePlugin.error(messageOf(error));
  } finally {
    loading.value = false;
  }
}

/** 实时合计（仅用于所见即所得；落库以服务端为准） */
const totals = computed(() => computeTotals(form.items));

/** 第 index 行的字段错误 */
function rowErrors(index: number): Record<string, string> {
  const prefix = `items.${index}`;
  const out: Record<string, string> = {};
  for (const [key, text] of Object.entries(formErrors.value)) {
    if (key.startsWith(`${prefix}.`)) {
      out[key.slice(prefix.length + 1)] = text;
    }
  }
  return out;
}

function resetForm(): void {
  Object.assign(form, emptyStockInForm());
  formErrors.value = {};
  submitError.value = '';
  // D9：复制提示只属于「刚复制出来的那张单」，任何一次重置都要清掉
  copyNotice.value = '';
}

/**
 * 新建：系统默认 → 记忆 → 源单（`source` 给「采购 → 收货」这类带出路径留着）。
 * 优先级：**源单带出 > 上次记忆 > 系统默认**。
 */
function applyCreateDefaults(source: Partial<StockInFormState> = {}): void {
  const resolved = resolveDefaults<StockInFormState>(
    emptyStockInForm(),
    formMemory.recall(),
    source,
    MEMORY_FIELDS,
  );
  Object.assign(form, pickFields(resolved, MEMORY_FIELDS));
  memoryLabels.value = formMemory.labelsOf(source);
}

function openCreateDialog(): void {
  resetForm();
  applyCreateDefaults();
  formVisible.value = true;
  // 打开就拉库位：有默认库位的话，第一行立刻带出来（用户不用手选）
  void loadLocations(true);
}

// ==================== D4：采购订单 → 一键生成收货单 ====================

const router = useRouter();
const route = useRoute();

/** 采购订单页的路由路径（`business/erp/purchase-order`，别名 `purchase`） */
const PURCHASE_PATH = '/business/erp/purchase-order';

/** 列表 / 详情里的「来源采购单号」点击 → 跳回采购单（带 `po_id` 让对端直开详情） */
function goPurchaseOrderById(id: string | null | undefined, no: string | null | undefined): void {
  if (!id) {
    return;
  }
  void router.push({ path: PURCHASE_PATH, query: { po_id: id, ...(no ? { po_no: no } : {}) } });
}

/** 列表行的「来源采购单」点击 */
function goPurchaseOrder(row: StockInRow): void {
  goPurchaseOrderById(row.source_order_id, row.source_order_no);
}

/**
 * 从采购单带出草稿（卡面 F1 / F2 / O1）。
 *
 * 流程：查待收明细 → 单头（供应商 / 仓库 / 来源单号）从源单填 → 明细只带 `pending_qty > 0` 的行、
 * 数量默认 = 待收数量 → **仍是草稿**（硬红线 7：不自动过账）。
 */
async function openFromPurchaseOrder(poId: string): Promise<void> {
  resetForm();
  loading.value = true;
  try {
    const pending = await fetchPurchaseOrderPendingItems(poId);
    const { form: filled, lineCount } = formFromPending(pending);
    if (lineCount === 0) {
      // 一行都不剩：不打开空草稿，直接把后端给的原因说出来
      MessagePlugin.warning(pending.blocked_reason ?? `采购单 ${pending.order.order_no} 没有可收的待收明细`);
      openCreateDialog();
      return;
    }
    Object.assign(form, filled);
    formErrors.value = {};
    submitError.value = '';
    void loadLocations(true);
    // 源单带出的字段压过「上次记忆」——提示条只报源单带出的条数
    memoryLabels.value = [];
    formVisible.value = true;
    MessagePlugin.success(
      `已从采购单 ${pending.order.order_no} 带出 ${lineCount} 行待收明细，请核对数量`,
    );
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    openCreateDialog();
  } finally {
    loading.value = false;
  }
}

/** 表单记忆 · 「重置」：清记忆 + 记忆字段退回系统默认（不动用户已填的明细和日期） */
function handleResetMemory(): void {
  formMemory.forget();
  memoryLabels.value = [];
  const fresh = emptyStockInForm();
  const patch: Record<string, unknown> = {};
  for (const field of MEMORY_FIELDS) {
    patch[String(field)] = fresh[field];
  }
  Object.assign(form, patch);
  MessagePlugin.success('已清空表单记忆，下次新建按系统默认');
}

/** 编辑（拉详情：注意 `header` 包了一层） */
async function openEditDialog(row: StockInRow): Promise<void> {
  if (!actionsOf(row).edit) {
    MessagePlugin.warning('只有草稿可以编辑');
    return;
  }
  resetForm();
  formVisible.value = true;
  loading.value = true;
  try {
    const detail = await fetchStockInDetail(row.id);
    Object.assign(form, detailToForm(detail.header, detail.items));
    // 编辑已有草稿：**不**补默认库位（尊重用户保存过的选择）
    void loadLocations(false);
  } catch (error) {
    formVisible.value = false;
    MessagePlugin.error(messageOf(error));
  } finally {
    loading.value = false;
  }
}

/**
 * D9：复制为新单（详情抽屉底部的「复制为新单」）。
 *
 * 语义全在后端（`POST /erp/stock-in/{id}/copy`）：新草稿 + 新单号 + 明细全带出 +
 * **采购单关联清空** + 入库日期取今天 + 凭证/应付/审批关联清空。前端只做三件事：
 *
 * 1. 调接口拿**新单详情**（响应与建单一致），把表单填成新单（`form.id = 新单 id`，
 *    所以后面点保存走的是「改这张草稿」而不是又建一张 —— 连点也不会多出单子）；
 * 2. 原单带过采购单时提示「请重新关联采购单」（后端已清空，这是设计而非丢失）；
 * 3. 刷新列表，让新草稿立刻出现在列表里。
 */
async function copyCurrent(): Promise<void> {
  const origin = detail.value?.header ?? null;
  if (!origin || copying.value) {
    return;
  }
  copying.value = true;
  try {
    const created = await copyStockIn(origin.id);
    detailVisible.value = false;
    resetForm();
    Object.assign(form, detailToForm(created.header, created.items));
    memoryLabels.value = [];
    copyNotice.value = stockinCopyNotice(origin);
    void loadLocations(false);
    formErrors.value = {};
    submitError.value = '';
    formVisible.value = true;
    MessagePlugin.success(`已复制为 ${created.header.in_no}（草稿），请核对后提交`);
    await loadOrders(1);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    copying.value = false;
  }
}

function addItem(): void {
  if (form.items.length >= STOCKIN_MAX_LINES) {
    MessagePlugin.warning(`明细最多 ${STOCKIN_MAX_LINES} 行`);
    return;
  }
  const line = emptyStockInItem(`row-${Date.now()}-${form.items.length + 1}`);
  // 明细行记忆：新行沿用上一次填的税率
  Object.assign(line, pickFields(lineMemory.recall(), LINE_MEMORY_FIELDS));
  // ★ P2-4：新行自动带出该仓默认库位（没有默认就留空，由后端兜底进未分配桶 + 提示）
  line.location_dict_id = defaultLocationId.value;
  form.items.push(line);
}

function removeItem(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
}

/** 保存草稿（建单 / 改单） */
async function save(): Promise<void> {
  formErrors.value = validateStockInForm(form);
  if (Object.keys(formErrors.value).length > 0) {
    MessagePlugin.warning('表单还有 ' + Object.keys(formErrors.value).length + ' 处需要修正');
    return;
  }
  const isEdit = Boolean(form.id);
  saving.value = true;
  submitError.value = '';
  try {
    const payload = buildStockInPayload(form);
    const detail = isEdit ? await updateStockIn(form.id, payload) : await createStockIn(payload);
    MessagePlugin.success(`${detail.header.in_no} 已保存为草稿`);
    // ★ P2-4：后端会把「未指定库位 / 库位超容量」这类提醒放在详情里 —— 保存后当场说出来
    if (detail.location_warnings?.length) {
      MessagePlugin.warning(detail.location_warnings[0]);
    }
    formVisible.value = false;
    // 保存成功才写记忆（用户这次的供应商 / 仓库 / 发票选项就是下次的默认）
    formMemory.remember(form);
    lineMemory.remember({ tax_rate: form.items[form.items.length - 1]?.tax_rate ?? 13 });
    memoryLabels.value = [];
    await loadOrders(isEdit ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== ★ 过账 ====================

const postVisible = ref(false);
const posting = ref(false);
const postTarget = ref<StockInRow | null>(null);
/** 发票选项（默认「已随货到达」；「发票未到」置灰不可选） */
const invoiceChoice = ref<'RECEIVED' | 'ESTIMATE'>(defaultInvoiceChoice());

/** 打开过账确认弹窗（每次都重置为默认勾选） */
function openPostDialog(row: StockInRow): void {
  if (!actionsOf(row).post) {
    MessagePlugin.warning('只有草稿可以过账');
    return;
  }
  if (!canPost(row)) {
    MessagePlugin.warning('这张收货单没有数量，不能过账');
    return;
  }
  postTarget.value = row;
  invoiceChoice.value = defaultInvoiceChoice();
  postVisible.value = true;
}

/** 发票选项点击：置灰项直接忽略（双保险，UI 已 disabled） */
function handleInvoiceChoice(value: 'RECEIVED' | 'ESTIMATE', disabled: boolean): void {
  if (disabled) {
    MessagePlugin.warning(ESTIMATE_DISABLED_TIP);
    return;
  }
  invoiceChoice.value = value;
}

/** 确认过账（P-FLOW-3A：走「提交过账」入口 —— 配了审批流就先送审，免审/无流程则直接过账） */
async function confirmPost(): Promise<void> {
  const target = postTarget.value;
  if (!target) {
    return;
  }
  posting.value = true;
  try {
    const detail = await submitStockInPost(target.id, buildPostPayload());
    // 后端把「免审直过」和「审批中」都回在同一个接口里 —— 按返回的真实状态说话，
    // 不要一口咬定「已过账」（否则用户以为库存已经动了，其实还在等审批）。
    if (detail.header.status === 'PENDING_POST') {
      MessagePlugin.success(`${target.in_no} 已提交过账审批：审批通过后才写库存与凭证`);
    } else {
      MessagePlugin.success(`${target.in_no} 已过账，库存与流水已写入`);
    }
    postVisible.value = false;
    postTarget.value = null;
    await loadOrders();
  } catch (error) {
    // 重复过账会拿到 409 + 10003（文案含「已过账」），原样透出
    MessagePlugin.error(messageOf(error));
  } finally {
    posting.value = false;
  }
}

/**
 * 发起作废（P16：已过账的单据走**审批**，不再直接动账）。
 *
 * - `DRAFT`：直接作废（从未入账，无需复核）；
 * - `POSTED`：只发起作废审批 → 状态变「作废审批中」，六本账等复核通过才回滚；
 * - 文案逐字说明后果（`cancelConfirmMessage`），不让人以为点一下就红冲了。
 */
function confirmCancel(row: StockInRow): void {
  if (!actionsOf(row).cancel) {
    MessagePlugin.warning('当前状态不能作废');
    return;
  }
  const isApproval = row.status === 'POSTED';
  const dialog = DialogPlugin.confirm({
    header: isApproval ? '发起作废审批' : '作废收货单',
    theme: 'danger',
    body: cancelConfirmMessage(row.status),
    confirmBtn: {
      content: isApproval ? '提交审批' : '确认作废',
      theme: 'danger',
    },
    onConfirm: async () => {
      dialog.hide();
      try {
        const updated = await updateStockInStatus(row.id, buildCancelPayload('页面作废'));
        if (isApproval) {
          MessagePlugin.success(
            `${row.in_no} 已提交作废审批（作废审批中）：审批通过后才回滚六本账`,
          );
        } else {
          MessagePlugin.success(`${row.in_no} 已作废`);
        }
        // 详情抽屉开着时同步刷新（状态 / 审批实例 id 都变了）
        if (detailVisible.value && updated?.header?.id === row.id) {
          detail.value = updated;
        }
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

/**
 * 撤回作废申请（仅 `PENDING_CANCEL`；后端校验「只有发起人能撤」）。
 *
 * 撤回后单据回到「已过账」、审批单落 `WITHDRAWN`、审批中心待办同时消失 —— 账一点没动。
 */
function confirmWithdrawCancel(row: StockInRow): void {
  if (!actionsOf(row).withdrawCancel) {
    MessagePlugin.warning('只有「作废审批中」的单据能撤回作废申请');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '撤回作废申请',
    body: `撤回后 ${row.in_no} 回到「已过账」，库存等六本账不受影响。确认撤回？`,
    confirmBtn: { content: '确认撤回', theme: 'primary' },
    onConfirm: async () => {
      dialog.hide();
      try {
        const updated = await withdrawStockInCancel(row.id);
        MessagePlugin.success(`${row.in_no} 已撤回作废申请（回到已过账）`);
        if (detailVisible.value && updated?.header?.id === row.id) {
          detail.value = updated;
        }
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

/**
 * 删除（仅 DRAFT，软删）。
 *
 * 与「作废」的区别：作废是**留痕**的状态变化（POSTED 也能作废，会写 cancelled_* 三列）；
 * 删除是「这张单从列表里消失」，只对草稿开放（后端 403/409 兜底）。
 */
function confirmRemove(row: StockInRow): void {
  if (!actionsOf(row).remove) {
    MessagePlugin.warning('只有草稿可以删除');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除收货单',
    theme: 'danger',
    body: `${row.in_no} 将被软删除（列表不再显示，历史台账不受影响）。确认删除？`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteStockIn(row.id);
        MessagePlugin.success(`${row.in_no} 已删除`);
        await loadOrders();
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 详情 ====================
const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<StockInDetail | null>(null);
/** 作废审批进度（P16；空 = 没挂审批单 / 拉不到）。C2 起连审批人明细（tasks）一起带 */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });
/** D9：复制进行中（按钮同时 loading + disabled —— 连点只建一张） */
const copying = ref(false);
/** D9：复制提示（来源采购单被清空时显示在表单顶部；下次打开表单要清掉） */
const copyNotice = ref('');

async function openDetail(row: StockInRow): Promise<void> {
  await loadDetail(row.id);
}

/** 按 id 开详情抽屉（路由带 `in_id` 跳过来时用，手里没有整行） */
async function loadDetail(id: string): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    const loaded = await fetchStockInDetail(id);
    detail.value = loaded;
    // P16 → C2：只有在「作废审批中」的单据才挂 `approval_instance_id`；
    // 装载走 `shared/approval-progress`（与采购 / 应付 / 凭证 / 调拨同一个函数）。
    approvalProgress.value = await loadApprovalProgress(
      loaded?.header?.approval_instance_id ?? null,
      getFlowInstance,
    );
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

onMounted(() => {
  void loadBasics();
  // 恢复的页码（列表记忆）要在首次加载就生效
  void loadOrders(current.value);
  void applyRouteQuery();
});

/**
 * 路由参数（D4 跨模块跳转）。
 *
 * - `?from=po&po_id=<id>`：从采购单详情点「生成收货单」跳进来 → 自动带出草稿；
 * - `?in_id=<id>`：从采购单详情点「已生成收货单 IN-xxx」跳进来 → 直接开详情抽屉。
 *
 * 参数消费后立刻 `replace` 掉：刷新页面不该重复弹一次带出 / 详情
 * （用户手动刷新往往就是想看干净的列表）。
 */
async function applyRouteQuery(): Promise<void> {
  const from = typeof route.query.from === 'string' ? route.query.from : '';
  const poId = typeof route.query.po_id === 'string' ? route.query.po_id : '';
  const inId = typeof route.query.in_id === 'string' ? route.query.in_id : '';
  if (!poId && !inId) {
    return;
  }
  await router.replace({ path: route.path, query: {} });
  if (from === 'po' && poId) {
    await openFromPurchaseOrder(poId);
    return;
  }
  if (inId) {
    await loadDetail(inId);
  }
}
</script>

<style scoped lang="less">
.stockin-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
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

.tool-row__status {
  width: 140px;
}

.tool-row__supplier,
.tool-row__warehouse {
  width: 180px;
}

.tool-row__no {
  width: 210px;
}

/* D4/O3：来源采购单号筛选框 */
.tool-row__po {
  width: 190px;
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

.num {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.status-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
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

/* ==================== 表单 ==================== */

.dlg-alert {
  margin-bottom: 10px;
}

.form-row {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
}

.form-row__col {
  flex: 1 1 240px;
  min-width: 200px;
}

.disabled-radio {
  display: inline-flex;
  color: var(--td-text-color-disabled);
  cursor: not-allowed;
}

.items {
  margin-top: 4px;
}

.items__head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 8px;
}

.items__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.items__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* P2-4：明细头部的「批量设库位」控件 */
.items__batch {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}

.items__batch-label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.items__batch-select {
  width: 200px;
}

/* P2-4：详情里的库位提示列表 */
.location-warnings {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.6;
}

.items__grid {
  display: grid;
  grid-template-columns: minmax(160px, 2fr) 96px 120px 96px 168px 130px 110px 40px;
  gap: 8px;
  align-items: start;
}

.items__grid--head {
  padding: 0 0 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.items__row {
  padding: 8px 0;
  border-top: 1px solid var(--td-component-stroke);
}

.items__cell {
  min-width: 0;
}

.items__cell--num {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  height: 32px;
  font-size: 13px;
}

.items__cell--op {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 32px;
}

.ta-right {
  text-align: right;
}

.cell-error {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
}

/* D4/F2：数量下方的待收上限提示（订单 / 已收 / 待收） */
.cell-hint {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.4;
  color: var(--td-text-color-placeholder);
}

.totals {
  display: flex;
  align-items: baseline;

  justify-content: flex-end;
  gap: 20px;
  flex-wrap: wrap;
  margin-top: 12px;
  padding: 12px 16px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.totals__item strong {
  margin-left: 6px;
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  font-size: 15px;
  color: var(--td-text-color-primary);
}

.totals__item--strong strong {
  color: var(--td-brand-color);
}

.dlg-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 16px;
}

/* ==================== 过账弹窗 ==================== */

.post-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.post-kv {
  display: flex;
  align-items: baseline;
  gap: 10px;
  font-size: 13px;
}

.post-kv__label {
  flex: 0 0 56px;
  color: var(--td-text-color-placeholder);
}

.post-kv__value {
  color: var(--td-text-color-primary);
}

.post-invoice {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 4px;
  border: 1px solid var(--td-component-stroke);
}

.post-invoice__row--disabled {
  color: var(--td-text-color-disabled);
}

.post-invoice__inner {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}

.post-invoice__desc {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.post-warn {
  border-radius: 4px;
}

.post-warn__title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
}

.post-warn__list {
  margin: 6px 0 0;
  padding-left: 20px;
  font-size: 13px;
  line-height: 22px;
}

/* ==================== 详情抽屉 ==================== */

.drawer-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 0;
  color: var(--td-text-color-placeholder);
}

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 12px;
}

.detail__no {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 20px;
  padding: 12px 16px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
}

.detail-field {
  display: flex;
  gap: 8px;
  font-size: 13px;
}

.detail-field--wide {
  grid-column: 1 / -1;
}

.detail-field__label {
  flex: 0 0 72px;
  color: var(--td-text-color-placeholder);
}

.detail-field__value {
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.detail__summary {
  margin: 10px 0 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* D9：复制为新单（抽屉底部动作条） */
.detail__actions {
  margin-top: 16px;
  padding-top: 12px;
  border-top: 1px dashed var(--td-component-stroke);
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.detail__actions-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.section-title {
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

/* ==================== A6 单据来源 / 只读字段 ==================== */

/* 只读字段下面那行说明（「供应商来自采购单，不可修改」） */
.field-note {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.4;
  color: var(--td-text-color-placeholder);
}

.readonly-field {
  width: 100%;

  /* 只读字段视觉上也要「灰」：TDesign 的 readonly `<t-input>` 默认白底，会被当成可编辑框 */
  :deep(.t-input),
  :deep(.t-select__wrap) {
    background-color: var(--td-bg-color-container-hover);
  }

  :deep(input) {
    cursor: default;
  }
}

/* 「单据来源」一行：单选 + 无源标记并排（标记不能挤到下一行去） */
.source-mode {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
</style>
