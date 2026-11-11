<template>
  <div class="stockout-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-stockout-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建出库单
        </t-button>
        <t-button variant="outline" data-testid="btn-stockout-refresh" @click="refresh">
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
          data-testid="stockout-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.out_type"
          class="tool-row__type"
          :options="typeFilterOptions"
          placeholder="全部类型"
          clearable
          data-testid="stockout-filter-type"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.warehouse_id"
          class="tool-row__warehouse"
          :options="warehouseOptions"
          placeholder="全部仓库"
          clearable
          filterable
          data-testid="stockout-filter-warehouse"
          @change="handleSearch"
        />
        <t-date-range-picker
          v-model="filter.date_range"
          class="tool-row__range"
          placeholder="出库日期区间"
          format="YYYY-MM-DD"
          clearable
          data-testid="stockout-filter-date"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="单号 / 事由 / 备注（模糊）"
          clearable
          data-testid="stockout-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-stockout-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-stockout-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">出库单</span>
          <span class="panel-head__meta" data-testid="stockout-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          出库过账会减三本账并生成成本结转凭证（借 6401 / 1901，贷 1405）；作废按明细快照回滚
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="stockout-load-error"
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
        hover
        data-testid="stockout-table"
        @page-change="handlePageChange"
      >
        <template #out_no="{ row }">
          <span class="mono" :data-testid="`stockout-no-${row.out_no}`">{{ row.out_no }}</span>
        </template>

        <template #out_type_label="{ row }">
          <t-tag size="small" variant="outline" :theme="typeTheme(row.out_type)">
            {{ row.out_type_label || outTypeLabel(row.out_type) }}
          </t-tag>
        </template>

        <template #warehouse_id="{ row }">{{ warehouseNameOf(row.warehouse_id) }}</template>

        <template #out_date="{ row }">
          <span class="muted">{{ shortDate(row.out_date) }}</span>
        </template>

        <template #total_qty="{ row }">
          <span class="num" :data-testid="`stockout-qty-${row.out_no}`">{{ formatQty(row.total_qty) }}</span>
        </template>

        <!-- ★ 成本金额：草稿恒为 0，过账后才是账上真实差额（列标题已标口径） -->
        <template #total_amount="{ row }">
          <span
            class="money"
            :class="{ 'money--pending': row.status === 'DRAFT' || row.status === 'PENDING_POST' }"
            :data-testid="`stockout-amount-${row.out_no}`"
          >
            {{ row.status === 'POSTED' || row.status === 'CANCELLED' ? formatMoney(row.total_amount) : '过账后才有值' }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="stockOutStatusTheme(row.status)" :data-testid="`stockout-status-${row.out_no}`">
            {{ row.status_label || stockOutStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-view-${row.out_no}`" @click="openDetail(row)">查看</t-link>
            <t-link
              v-if="canEdit(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`btn-edit-${row.out_no}`"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="canDelete(row)"
              v-privilege="[PERM.remove]"
              theme="danger"
              :data-testid="`btn-remove-${row.out_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="stockout-empty">
            <t-icon name="inbox" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的出库单' : '还没有出库单' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个条件再查，或点「重置」回全量' : '点左上角「新建出库单」录入第一张（领用 / 报废 / 盘亏）' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑抽屉 ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      :header="form.id ? '编辑出库单' : '新建出库单'"
      size="900px"
      :footer="false"
      data-testid="stockout-form-drawer"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" data-testid="stockout-submit-error" />

      <t-alert theme="info" class="dlg-alert" data-testid="stockout-cost-note">
        <template #message>
          出库成本由服务端按<strong>账上移动加权单价</strong>计算（明细不填单价 / 金额 / 单位）；
          过账时才减库存并生成凭证，保存下来只是<strong>草稿</strong>。
        </template>
      </t-alert>

      <!-- 一键领料（B4-3）：从成本对象详情页「领料」跳进来时显示来源 -->
      <t-alert
        v-if="prefillBanner"
        theme="success"
        class="dlg-alert"
        data-testid="stockout-prefill-banner"
      >
        <template #message>
          来自成本对象 <strong>{{ prefillBanner.object_no }}</strong> ·
          生产车间 <strong>{{ prefillBanner.workshop_dept_name }}</strong>。
          <template v-if="prefillBanner.material_source === 'HISTORY'">
            已按上次领料预填 {{ prefillBanner.material_count }} 种物料，数量请按本次实际改。
          </template>
          <template v-else>
            这一批还没领过料，物料请手动选（一期没有 BOM 定额）。
          </template>
          保存后自动挂回该成本对象，过账借方 <strong>4001.01 生产成本-直接材料</strong>。
        </template>
      </t-alert>

      <t-form :data="form" label-width="96px" class="form">
        <t-form-item
          label="出库类型"
          name="out_type"
          :status="formErrors.out_type ? 'error' : undefined"
          :tips="formErrors.out_type"
        >
          <t-radio-group :value="form.out_type" data-testid="stockout-form-type" @change="handleTypeChange">
            <t-radio v-for="option in OUT_TYPE_OPTIONS" :key="option.value" :value="option.value">
              {{ option.label }}
            </t-radio>
          </t-radio-group>
        </t-form-item>

        <div class="form-row">
          <t-form-item
            label="出库仓库"
            name="warehouse_id"
            class="form-row__col"
            :status="formErrors.warehouse_id ? 'error' : undefined"
            :tips="formErrors.warehouse_id"
          >
            <t-select
              v-model="form.warehouse_id"
              :options="warehouseOptions"
              placeholder="选择出库仓库"
              filterable
              data-testid="stockout-form-warehouse"
              @change="handleWarehouseChange"
            />
          </t-form-item>

          <t-form-item
            label="出库日期"
            name="out_date"
            class="form-row__col"
            :status="formErrors.out_date ? 'error' : undefined"
            :tips="formErrors.out_date"
          >
            <t-date-picker
              v-model="form.out_date"
              format="YYYY-MM-DD"
              placeholder="选择出库日期"
              data-testid="stockout-form-date"
            />
          </t-form-item>
        </div>

        <t-form-item
          label="出库事由"
          name="reason"
          :status="formErrors.reason ? 'error' : undefined"
          :tips="formErrors.reason"
        >
          <div class="reason-field">
            <t-input
              v-model="form.reason"
              :placeholder="reasonPlaceholder"
              :maxlength="STOCK_OUT_MAX_REASON"
              data-testid="stockout-form-reason"
            />
            <span class="reason-field__hint" data-testid="stockout-reason-hint">
              {{ reasonHint }}
            </span>
          </div>
        </t-form-item>

        <t-form-item label="成本对象" name="cost_object_id">
          <div class="cost-object-field">
            <t-select
              v-if="allowsCostObject(form.out_type)"
              v-model="form.cost_object_id"
              :options="costObjectSelectOptions"
              placeholder="可选：把这次领料归到某个生产批（只列在制的）"
              filterable
              clearable
              data-testid="stockout-form-cost-object"
            />
            <span class="cost-object-field__hint" data-testid="stockout-debit-hint">
              <template v-if="allowsCostObject(form.out_type)">
                过账借方：<strong>{{ debitPreview.code }} {{ debitPreview.name }}</strong>
                <template v-if="form.cost_object_id">（挂了成本对象，料费归集到 4001.01）</template>
                <template v-else>（不挂对象走 6401；想按生产批归集材料成本请选一个对象）</template>
              </template>
              <template v-else>
                报废 / 盘亏不挂成本对象，过账借方恒为
                <strong>{{ debitPreview.code }} {{ debitPreview.name }}</strong>
              </template>
            </span>
          </div>
        </t-form-item>

        <t-form-item v-if="prefillBanner" label="生产车间">
          <div class="cost-object-field">
            <t-input :value="prefillBanner.workshop_dept_name" readonly data-testid="stockout-form-workshop" />
            <span class="cost-object-field__hint" data-testid="stockout-workshop-hint">
              来自成本对象 <strong>{{ prefillBanner.object_no }}</strong>。出库单没有车间列，
              保存时这个车间已写进出库事由（凭证摘要会带上）
            </span>
          </div>
        </t-form-item>

        <t-form-item label="备注" name="remark">
          <t-textarea
            v-model="form.remark"
            :maxlength="STOCK_OUT_MAX_REMARK"
            :autosize="{ minRows: 2, maxRows: 4 }"
            placeholder="选填（≤ 500 字）"
            data-testid="stockout-form-remark"
          />
        </t-form-item>
      </t-form>

      <div class="section-title">
        明细（{{ form.items.length }} 行）
        <span class="section-title__hint">一物一码物料不能走出库单，请到序列号管理里做出库 / 报废</span>
      </div>
      <t-alert v-if="formErrors.items" theme="error" :message="formErrors.items" class="items-alert" />

      <div class="items" data-testid="stockout-form-items">
        <div class="items__head">
          <span class="items__col items__col--material">物料</span>
          <span class="items__col items__col--qty">数量</span>
          <span class="items__col items__col--location">库位账行（可选）</span>
          <span class="items__col items__col--op">操作</span>
        </div>

        <div v-for="(item, index) in form.items" :key="item.key" class="items__row">
          <div class="items__col items__col--material">
            <t-select
              v-model="item.material_id"
              :options="materialOptions"
              placeholder="搜索物料编码 / 名称"
              filterable
              clearable
              :status="itemError(index, 'material_id') ? 'error' : undefined"
              :data-testid="`stockout-item-material-${index}`"
              @change="() => loadLocationRows(item)"
            />
            <span v-if="itemError(index, 'material_id')" class="items__error" :data-testid="`stockout-item-material-error-${index}`">
              {{ itemError(index, 'material_id') }}
            </span>
          </div>

          <div class="items__col items__col--qty">
            <t-input-number
              v-model="item.qty"
              :min="0"
              :max="9999999"
              theme="normal"
              placeholder="数量"
              :status="itemError(index, 'qty') ? 'error' : undefined"
              :data-testid="`stockout-item-qty-${index}`"
            />
            <span v-if="itemError(index, 'qty')" class="items__error" :data-testid="`stockout-item-qty-error-${index}`">
              {{ itemError(index, 'qty') }}
            </span>
          </div>

          <div class="items__col items__col--location">
            <t-select
              v-model="item.location_id"
              :options="locationOptionsOf(item)"
              :placeholder="locationPlaceholderOf(item)"
              clearable
              :disabled="!item.material_id || !form.warehouse_id"
              :data-testid="`stockout-item-location-${index}`"
            />
          </div>

          <div class="items__col items__col--op">
            <t-link
              theme="danger"
              :disabled="form.items.length <= 1"
              :data-testid="`btn-stockout-item-remove-${index}`"
              @click="removeItem(index)"
            >
              删除
            </t-link>
          </div>
        </div>
      </div>

      <t-button
        variant="outline"
        size="small"
        class="items__add"
        :disabled="form.items.length >= STOCK_OUT_MAX_LINES"
        data-testid="btn-stockout-item-add"
        @click="addItem"
      >
        <template #icon><t-icon name="add" /></template>
        添加明细
      </t-button>

      <div class="drawer-footer">
        <t-button
          theme="primary"
          :loading="submitting"
          :disabled="submitting"
          data-testid="btn-stockout-save"
          @click="handleSave"
        >
          {{ form.id ? '保存修改' : '保存草稿' }}
        </t-button>
        <t-button variant="outline" data-testid="btn-stockout-form-cancel" @click="formVisible = false">取消</t-button>
      </div>
    </t-drawer>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="出库单详情"
      size="820px"
      :footer="false"
      data-testid="stockout-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail">
        <div class="detail__head">
          <span class="detail__no mono" data-testid="stockout-detail-no">{{ detail.out_no }}</span>
          <t-tag size="small" :theme="stockOutStatusTheme(detail.status)" data-testid="stockout-detail-status">
            {{ detail.status_label || stockOutStatusLabel(detail.status) }}
          </t-tag>
          <t-tag size="small" variant="outline" :theme="typeTheme(detail.out_type)" data-testid="stockout-detail-type">
            {{ detail.out_type_label || outTypeLabel(detail.out_type) }}
          </t-tag>
          <span class="detail__spacer" />
          <!-- VTRACE-4：本单产生的凭证（`STOCK_OUT`）；无凭证 / 无权限 / 查询失败一律不渲染 -->
          <VoucherTraceButton :sources="voucherSources" testid="stockout-detail-vouchers" />
          <!-- PDF-08：下载 PDF（出库单） -->
          <DocPdfButton
            :path="`/api/business/erp/stock-out/${detail.id}/pdf`"
            :filename="`出库单-${detail.out_no}.pdf`"
            testid="stockout-detail-pdf"
          />
        </div>

        <!-- 动作失败（尤其是 409 负库存）：报文原样展示，包含「可用量」 -->
        <t-alert
          v-if="actionError"
          theme="error"
          class="detail__alert"
          data-testid="stockout-action-error"
        >
          <template #message>
            <span class="detail__alert-text">{{ actionError }}</span>
          </template>
        </t-alert>

        <div class="detail__grid">
          <div class="detail-field"><span class="detail-field__label">出库仓库</span><span class="detail-field__value" data-testid="stockout-detail-warehouse">{{ warehouseNameOf(detail.warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">出库日期</span><span class="detail-field__value">{{ detail.out_date }}</span></div>
          <div class="detail-field"><span class="detail-field__label">合计数量</span><span class="detail-field__value">{{ formatQty(detail.total_qty) }}</span></div>
          <div class="detail-field">
            <span class="detail-field__label">成本金额</span>
            <span class="detail-field__value money" data-testid="stockout-detail-amount">
              {{ detail.status === 'POSTED' || detail.status === 'CANCELLED' ? formatMoney(detail.total_amount) : '过账后才有值' }}
            </span>
          </div>
          <div class="detail-field"><span class="detail-field__label">出库事由</span><span class="detail-field__value" data-testid="stockout-detail-reason">{{ dash(detail.reason) }}</span></div>
          <div class="detail-field">
            <span class="detail-field__label">成本对象</span>
            <span class="detail-field__value mono" data-testid="stockout-detail-cost-object">
              {{ detail.cost_object_no || '—（未挂对象：不进生产成本归集）' }}
            </span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">过账借方</span>
            <span class="detail-field__value mono" data-testid="stockout-detail-debit">
              {{ detail.debit_account_code || debitAccountHint(detail.out_type, detail.cost_object_id).code }}
              {{ detail.debit_account_name || debitAccountHint(detail.out_type, detail.cost_object_id).name }}
            </span>
          </div>
          <div class="detail-field"><span class="detail-field__label">凭证</span><span class="detail-field__value mono">{{ detail.voucher_id ? shortId(detail.voucher_id) : '—（未过账 / 零成本无凭证）' }}</span></div>
          <div class="detail-field detail-field--wide"><span class="detail-field__label">备注</span><span class="detail-field__value">{{ dash(detail.remark) }}</span></div>
          <div v-if="detail.cancel_reason" class="detail-field detail-field--wide">
            <span class="detail-field__label">作废原因</span>
            <span class="detail-field__value" data-testid="stockout-detail-cancel-reason">{{ detail.cancel_reason }}</span>
          </div>
        </div>

        <!-- 状态历史：只为真实发生过的里程碑打点（后端没给单独的历史接口） -->
        <div class="section-title">状态历史</div>
        <ul class="history" data-testid="stockout-history">
          <li v-for="entry in history" :key="entry.key" class="history__item">
            <span class="history__dot" />
            <span class="history__label">{{ entry.label }}</span>
            <span class="history__time">{{ formatDateTime(entry.at) }}</span>
            <span v-if="entry.note" class="history__note">{{ entry.note }}</span>
          </li>
        </ul>

        <div class="section-title">
          明细（{{ detail.items.length }} 条）
          <span class="section-title__hint">单价 / 金额两列「过账后才有值」（出库成本 = 账上移动加权单价的真实扣减额）</span>
        </div>
        <t-table row-key="id" size="small" :data="detail.items" :columns="detailColumns" data-testid="stockout-detail-items">
          <template #material="{ row }">
            <span class="mono">{{ row.material_code || shortId(row.material_id) }}</span>
            <span class="muted"> {{ row.material_name || '' }}</span>
          </template>
          <template #qty="{ row }">{{ formatQty(row.qty) }}</template>
          <template #unit_cost="{ row }">
            <span class="money" :class="{ 'money--pending': Number(row.unit_cost) === 0 }">
              {{ Number(row.unit_cost) === 0 ? '—' : formatMoney(row.unit_cost) }}
            </span>
          </template>
          <template #amount="{ row }">
            <span class="money" :class="{ 'money--pending': Number(row.amount) === 0 }">
              {{ Number(row.amount) === 0 ? '—' : formatMoney(row.amount) }}
            </span>
          </template>
          <template #location_id="{ row }">{{ locationLabelOf(row.location_id) }}</template>
        </t-table>

        <!-- 动作按钮：只由后端 next_statuses 驱动（前端不写第二份状态机） -->
        <div class="detail__actions" data-testid="stockout-detail-actions">
          <t-button
            v-for="action in detailActions"
            :key="action.status"
            :theme="action.theme === 'default' ? 'default' : action.theme"
            :variant="action.theme === 'primary' ? 'base' : 'outline'"
            :data-testid="action.testid"
            @click="startAction(action)"
          >
            {{ action.label }}
          </t-button>
          <span v-if="detailActions.length === 0" class="detail__actions-hint" data-testid="stockout-no-actions">
            {{ detail.status === 'CANCELLED' ? '已作废：单据不可再流转' : '当前状态没有可执行动作' }}
          </span>
          <t-button variant="outline" data-testid="btn-stockout-drawer-close" @click="detailVisible = false">关闭</t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ⑤ 过账确认弹窗 ==================== -->
    <t-dialog
      v-model:visible="postVisible"
      header="确认过账"
      width="560px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockout-post-dialog"
    >
      <p class="dialog-text">
        过账后会把 <strong>{{ detail?.out_no }}</strong> 的明细从三本账（成本账 / 库位账 / 流水）里扣减，
        并按账上移动加权单价生成<strong>成本结转凭证</strong>（借
        {{ detail?.debit_account_code || debitAccountHint(detail?.out_type, detail?.cost_object_id).code }}，贷 1405）。
        过账<strong>不可撤销</strong>，要走回头路只能「作废」（按明细快照回滚 + 红冲凭证）。
      </p>
      <p v-if="postError" class="dialog-error" data-testid="stockout-post-error">{{ postError }}</p>
      <div class="dialog-footer">
        <t-button theme="primary" :loading="acting" data-testid="btn-stockout-post-confirm" @click="confirmPost">确认过账</t-button>
        <t-button variant="outline" data-testid="btn-stockout-post-cancel" @click="postVisible = false">取消</t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（原因必填） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废出库单"
      width="560px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stockout-cancel-dialog"
    >
      <p class="dialog-text">
        作废 <strong class="mono">{{ detail?.out_no }}</strong>
        <template v-if="detail?.status === 'POSTED'">
          ：三本账按明细金额快照回滚、补一条 IN 冲销流水，并红冲成本结转凭证。
        </template>
        <template v-else>：草稿 / 待过账单据作废不动任何账。</template>
        作废原因会进状态日志与红字凭证摘要，<strong>必填</strong>。
      </p>
      <t-textarea
        v-model="cancelReason"
        :maxlength="STOCK_OUT_MAX_REASON"
        :autosize="{ minRows: 3, maxRows: 5 }"
        placeholder="例如：领错料，整单退回仓库"
        data-testid="stockout-cancel-reason"
      />
      <p v-if="cancelError" class="dialog-error" data-testid="stockout-cancel-error">{{ cancelError }}</p>
      <div class="dialog-footer">
        <t-button theme="danger" :loading="acting" data-testid="btn-stockout-cancel-confirm" @click="confirmCancel">
          确认作废
        </t-button>
        <t-button variant="outline" data-testid="btn-stockout-cancel-dismiss" @click="cancelVisible = false">取消</t-button>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  fetchInventoryLocationPage,
  fetchLocationList,
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import DocPdfButton from '@/components/DocPdfButton.vue';
import VoucherTraceButton from '@/views/business/erp/shared/VoucherTraceButton.vue';
import { stockOutSources } from '@/views/business/erp/shared/voucher-trace';
import {
  createStockOut,
  deleteStockOut,
  fetchStockOutCostObjectOptions,
  fetchStockOutDetail,
  fetchStockOutIssuePrefill,
  fetchStockOutPage,
  updateStockOut,
  updateStockOutStatus,
  type StockOutCostObjectOption,
  type StockOutDetail,
  type StockOutRow,
} from '@/api/business/erp/stock-out';
import {
  OUT_TYPE_OPTIONS,
  STOCK_OUT_MAX_LINES,
  STOCK_OUT_MAX_REASON,
  STOCK_OUT_MAX_REMARK,
  STOCK_OUT_PAGE_SIZE,
  STOCK_OUT_STATUS_FILTER_OPTIONS,
  allowsCostObject,
  applyIssuePrefill,
  buildCancelPayload,
  buildStockOutPayload,
  buildStockOutQuery,
  canDelete,
  canEdit,
  dash,
  debitAccountHint,
  detailToForm,
  emptyStockOutFilter,
  emptyStockOutForm,
  emptyStockOutItem,
  errorText,
  formatDateTime,
  formatMoney,
  formatQty,
  isNegativeStockError,
  isStockOutFiltered,
  outTypeLabel,
  requiresReason,
  shortDate,
  shortId,
  statusActions,
  statusHistory,
  stockOutStatusLabel,
  stockOutStatusTheme,
  validateCancelReason,
  validateStockOutForm,
  type IssuePrefillBanner,
  type StockOutActionSpec,
  type StockOutFilterState,
  type StockOutFormErrors,
  type StockOutFormState,
  type StockOutItemForm,
} from './stock-out-logic';

/** 本页权限码（与 V0203 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:stock-out:list',
  detail: 'business:erp:stock-out:detail',
  create: 'business:erp:stock-out:create',
  update: 'business:erp:stock-out:update',
  remove: 'business:erp:stock-out:delete',
  status: 'business:erp:stock-out:status',
} as const;

/** 主数据下拉一次拉多少（与收货页同口径） */
const OPTION_LIMIT = 200;

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'out_no', title: '出库单号', width: 210 },
  { colKey: 'out_type_label', title: '类型', width: 100 },
  { colKey: 'warehouse_id', title: '仓库', minWidth: 150, ellipsis: true },
  { colKey: 'out_date', title: '出库日期', width: 100 },
  { colKey: 'total_qty', title: '数量', width: 100, align: 'right' },
  // ★ 成本金额：草稿 / 待过账显示「过账后才有值」，不让 0 被当成真金额看
  { colKey: 'total_amount', title: '成本金额(过账后)', width: 160, align: 'right' },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

/** 详情抽屉里的明细列 */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 46 },
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'qty', title: '数量', width: 90, align: 'right' },
  { colKey: 'unit_cost', title: '单价', width: 110, align: 'right' },
  { colKey: 'amount', title: '金额', width: 120, align: 'right' },
  { colKey: 'location_id', title: '库位账行', width: 140, ellipsis: true },
];

const statusFilterOptions = STOCK_OUT_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
const typeFilterOptions = OUT_TYPE_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================
const filter = reactive<StockOutFilterState>(emptyStockOutFilter());
const rows = ref<StockOutRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = STOCK_OUT_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const filtered = computed(() => isStockOutFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// ==================== 主数据 ====================
const warehouses = ref<Warehouse[]>([]);
const materials = ref<MaterialRow[]>([]);
/** 库位主数据（id → 名称），按仓库懒加载 */
const locations = ref<Array<{ id: string; location_code: string; location_name: string | null }>>([]);
/** 库位账行缓存（key = `${warehouse_id}|${material_id}`） */
const locationRowCache = new Map<string, Array<{ label: string; value: string }>>();
/** 一物一码物料 id（出库单禁止：后端 400 指路序列号管理） */
const serialMaterialIds = computed(() =>
  materials.value.filter((item) => item.serial_required).map((item) => item.id),
);

// ---- 一键领料预填（B4-3：从成本对象详情页跳进来） ----
/** 预填横幅（对象号 / 车间 / 物料范围来源），`null` = 不是从成本对象跳进来的 */
const prefillBanner = ref<IssuePrefillBanner | null>(null);
/** 预填物料的补充下拉项（历史物料不在前 200 条主数据里时补上，别显示裸 id） */
const prefillMaterialOptions = ref<Array<{ label: string; value: string }>>([]);
/** 预填的成本对象（可能不在 OPEN 下拉的前 200 条里 → 补一条，否则回显是裸 id） */
const prefillObject = ref<StockOutCostObjectOption | null>(null);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_code} ${item.warehouse_name}`,
    value: item.id,
  })),
);

/** 物料下拉：**提前过滤掉一物一码物料**（别让用户填完一行才吃 400） */
const materialOptions = computed(() => {
  const options = materials.value
    .filter((item) => !item.serial_required)
    .map((item) => ({
      label: `${item.material_code} ${item.material_name}`,
      value: item.id,
    }));
  // 一键领料预填的物料可能不在「前 200 条」主数据里（老物料 / 历史领过但列表没拉到）
  // → 补进下拉，否则选择器会把 id 裸着显示
  for (const extra of prefillMaterialOptions.value) {
    if (!options.some((item) => item.value === extra.value)) {
      options.push(extra);
    }
  }
  return options;
});

/** 仓库 id → 名称 */
function warehouseNameOf(warehouseId: string | null | undefined): string {
  if (!warehouseId) {
    return '—';
  }
  const hit = warehouses.value.find((item) => item.id === warehouseId);
  return hit ? hit.warehouse_name : shortId(warehouseId);
}

/** 库位账行 id → 可读名（库位主数据 id → 编码） */
function locationLabelOf(locationRowId: string | null | undefined): string {
  if (!locationRowId) {
    return '未分配库位';
  }
  for (const options of locationRowCache.values()) {
    const hit = options.find((item) => item.value === locationRowId);
    if (hit) {
      return hit.label;
    }
  }
  return shortId(locationRowId);
}

/** 出库类型标签主题 */
function typeTheme(outType: string): 'default' | 'primary' | 'warning' | 'danger' {
  switch (outType) {
    case 'ISSUE':
      return 'primary';
    case 'SCRAP':
      return 'danger';
    case 'LOSS':
      return 'warning';
    default:
      return 'default';
  }
}

async function loadBasics(): Promise<void> {
  try {
    const [warehousePage, materialPage] = await Promise.all([
      fetchWarehousePage({ page: 1, page_size: OPTION_LIMIT }),
      fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT }),
    ]);
    warehouses.value = warehousePage.list;
    materials.value = materialPage.list;
  } catch {
    // 主数据拉不到不挡列表：下拉会空，但列表 / 详情照常（错误各自有提示）
  }
}

/** 拉列表 */
async function loadOrders(page = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchStockOutPage(buildStockOutQuery(filter, page, pageSize));
    rows.value = data.list;
    total.value = data.total;
    current.value = page;
  } catch (error) {
    loadError.value = errorText(error);
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function refresh(): void {
  void loadOrders(current.value);
}

function handleSearch(): void {
  void loadOrders(1);
}

function handleReset(): void {
  Object.assign(filter, emptyStockOutFilter());
  void loadOrders(1);
}

function handlePageChange(pageInfo: { current: number }): void {
  void loadOrders(pageInfo.current);
}

// ==================== 建单 / 编辑表单 ====================
const formVisible = ref(false);
const form = reactive<StockOutFormState>(emptyStockOutForm());
const formErrors = ref<StockOutFormErrors>({});
const submitting = ref(false);
const submitError = ref('');
let lineSeed = 1;

// ==================== 成本对象（ARCH-9 B1-10 / B1-12） ====================
/** 可选成本对象（**只有 OPEN**；数据源是 `/stock-out/cost-object-options`） */
const costObjectOptions = ref<StockOutCostObjectOption[]>([]);
/** 编辑旧单时那个「已经不在下拉里」的对象（草稿期间被关批 / 作废） */
const staleCostObject = ref<StockOutCostObjectOption | null>(null);

/** 下拉项：`对象号 · 产品名`；旧单的对象不在 OPEN 列表里时补一条并标注原因 */
const costObjectSelectOptions = computed(() => {
  const options = costObjectOptions.value.map((item) => ({
    label: item.product_name ? `${item.object_no} · ${item.product_name}` : item.object_no,
    value: item.id,
  }));
  const stale = staleCostObject.value;
  if (stale && !options.some((item) => item.value === stale.id)) {
    options.unshift({
      label: `${stale.object_no}（已关批 / 已作废，不可再选）`,
      value: stale.id,
    });
  }
  // 一键领料预填的对象（下拉前 200 条里可能没有）→ 补一条正常可选的项
  const prefill = prefillObject.value;
  if (prefill && !options.some((item) => item.value === prefill.id)) {
    options.unshift({
      label: prefill.product_name ? `${prefill.object_no} · ${prefill.product_name}` : prefill.object_no,
      value: prefill.id,
    });
  }
  return options;
});

/** 借方科目**预览**（选完成本对象这里会变；过账口径以详情下发的为准） */
const debitPreview = computed(() => debitAccountHint(form.out_type, form.cost_object_id));

/** 拉可选成本对象（拉不到不挡建单：下拉空，用户仍可保存不挂对象的单） */
async function loadCostObjects(): Promise<void> {
  try {
    costObjectOptions.value = await fetchStockOutCostObjectOptions();
  } catch {
    costObjectOptions.value = [];
  }
}

/** 事由框的占位 / 提示随类型变（报废 / 盘亏必填） */
const reasonPlaceholder = computed(() =>
  requiresReason(form.out_type) ? '必填：报废 / 盘亏要挂 1901，凭证摘要靠它说明原因' : '选填：领用用途（车间领用 / 项目用料 …）',
);
const reasonHint = computed(() =>
  requiresReason(form.out_type)
    ? '「' + outTypeLabel(form.out_type) + '」必须填事由，否则后端 400 且无法挂 1901 待处理财产损溢'
    : '领用 / 其他不是必填，但建议写清用途（凭证摘要会用）',
);

function resetForm(next: StockOutFormState): void {
  Object.assign(form, next);
  formErrors.value = {};
  submitError.value = '';
  lineSeed = form.items.length + 1;
  locationRowCache.clear();
  staleCostObject.value = null;
}

/** 清掉「一键领料」的一切痕迹（手点「新建」或预填失败时） */
function clearIssuePrefill(): void {
  prefillBanner.value = null;
  prefillMaterialOptions.value = [];
  prefillObject.value = null;
}

function openCreate(): void {
  resetForm(emptyStockOutForm());
  clearIssuePrefill();
  void loadCostObjects();
  formVisible.value = true;
}

const route = useRoute();
const router = useRouter();

/**
 * 一键领料入口（B4-3）：成本对象详情页「领料」→ `?cost_object_id=…`。
 *
 * 顺序上先 `router.replace` 把 query 摘掉再去请求：
 * ① 用户关掉抽屉 / 刷新页面时不该再弹一次同样的预填（那是「幽灵重开」）；
 * ② 预填失败（对象已关批）也不该留着一个会反复触发的参数。
 *
 * 失败路径只弹提示、**不打开表单**：对象不是 OPEN 时表单里的成本对象下拉也选不到它，
 * 强行开一张空单只会让用户白填一遍再吃 409。
 */
async function applyRouteIssuePrefill(): Promise<void> {
  const raw = route.query.cost_object_id;
  const costObjectId = (Array.isArray(raw) ? raw[0] : raw) ?? '';
  if (typeof costObjectId !== 'string' || !costObjectId.trim()) {
    return;
  }
  await router.replace({ path: route.path });
  try {
    const prefill = await fetchStockOutIssuePrefill(costObjectId.trim());
    const applied = applyIssuePrefill(prefill);
    resetForm(applied.form);
    prefillBanner.value = applied.banner;
    prefillMaterialOptions.value = prefill.materials.map((line) => ({
      label:
        `${line.material_code ?? ''} ${line.material_name ?? ''}`.trim() || shortId(line.material_id),
      value: line.material_id,
    }));
    prefillObject.value = {
      id: prefill.cost_object_id,
      object_no: prefill.object_no,
      product_id: prefill.product_id,
      product_code: prefill.product_code,
      product_name: prefill.product_name,
      plan_qty: '0',
      start_date: null,
    };
    void loadCostObjects();
    if (form.warehouse_id) {
      void loadLocations();
    }
    formVisible.value = true;
    MessagePlugin.success(
      prefill.has_history
        ? `已按成本对象 ${prefill.object_no} 预填 ${prefill.materials.length} 种物料，请核对数量`
        : `已带入成本对象 ${prefill.object_no}：这一批还没领过料，请选物料`,
    );
  } catch (error) {
    MessagePlugin.error(errorText(error));
  }
}

async function openEdit(row: StockOutRow): Promise<void> {
  try {
    const detail = await fetchStockOutDetail(row.id);
    if (detail.status !== 'DRAFT') {
      MessagePlugin.warning('只有草稿能编辑，请刷新列表再试');
      return;
    }
    resetForm(detailToForm(detail));
    clearIssuePrefill();
    // 旧单挂的对象可能已经关批 / 作废（不在 OPEN 下拉里）——补一条并标注，别让回显变成一串 id
    if (detail.cost_object_id && detail.cost_object_no) {
      staleCostObject.value = {
        id: detail.cost_object_id,
        object_no: detail.cost_object_no,
        product_id: '',
        product_code: null,
        product_name: null,
        plan_qty: '0',
        start_date: null,
      };
    }
    void loadCostObjects();
    // 编辑态：把每行的库位账行下拉先补上，否则回显是短 id
    await Promise.all(form.items.map((item) => loadLocationRows(item)));
    formVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error));
  }
}

function addItem(): void {
  lineSeed += 1;
  form.items.push(emptyStockOutItem(`line-${lineSeed}`));
}

function removeItem(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
  // 索引型错误键会错位，删行后整体重算一遍
  formErrors.value = validateStockOutForm(form, serialMaterialIds.value);
}

/** 行内错误（`items.<index>.<field>`） */
function itemError(index: number, field: 'material_id' | 'qty'): string {
  return formErrors.value[`items.${index}.${field}`] ?? '';
}

function handleTypeChange(value: string | number | boolean): void {
  form.out_type = String(value) as StockOutFormState['out_type'];
  // 只有领用能挂成本对象：类型改走就清掉残留选择（后端对非 ISSUE 挂对象 400）
  if (!allowsCostObject(form.out_type)) {
    form.cost_object_id = '';
  }
}

function handleWarehouseChange(): void {
  // 换仓库 → 库位账行全部失效
  locationRowCache.clear();
  form.items.forEach((item) => {
    item.location_id = undefined;
  });
  void loadLocations();
}

/** 拉当前仓库的库位主数据（库位账行的 `location_id` 指向它，译名要用） */
async function loadLocations(): Promise<void> {
  if (!form.warehouse_id) {
    locations.value = [];
    return;
  }
  try {
    locations.value = await fetchLocationList(form.warehouse_id, false);
  } catch {
    locations.value = [];
  }
}

/** 库位账行下拉的占位文案 */
function locationPlaceholderOf(item: StockOutItemForm): string {
  if (!form.warehouse_id) {
    return '先选仓库';
  }
  if (!item.material_id) {
    return '先选物料';
  }
  return '不指定（默认库位桶）';
}

/**
 * 拉某行的库位账行下拉。
 *
 * 缺的是 `biz_inventory_location.id`（**库位账行**，不是库位主数据的 id）——
 * 后端建单时按「物料 × 仓库」校验它归不归这一行，传错就是 400。
 * 拿不到库位主数据时用「库位账行 <短id> · 数量」兜底。
 */
async function loadLocationRows(item: StockOutItemForm): Promise<void> {
  if (!item.material_id || !form.warehouse_id) {
    return;
  }
  const key = `${form.warehouse_id}|${item.material_id}`;
  if (locationRowCache.has(key)) {
    return;
  }
  try {
    const data = await fetchInventoryLocationPage({
      material_id: item.material_id,
      warehouse_id: form.warehouse_id,
      page: 1,
      page_size: OPTION_LIMIT,
    });
    locationRowCache.set(
      key,
      data.list.map((line) => {
        const master = locations.value.find((loc) => loc.id === line.location_id);
        const name = master
          ? `${master.location_code}${master.location_name ? ' ' + master.location_name : ''}`
          : line.location_id
            ? `库位 ${shortId(line.location_id)}`
            : '未分配库位';
        return { label: `${name}（可用 ${formatQty(line.qty)}）`, value: line.id };
      }),
    );
  } catch {
    locationRowCache.set(key, []);
  }
}

function locationOptionsOf(item: StockOutItemForm): Array<{ label: string; value: string }> {
  if (!item.material_id || !form.warehouse_id) {
    return [];
  }
  return locationRowCache.get(`${form.warehouse_id}|${item.material_id}`) ?? [];
}

/** 保存（新建 → POST；编辑 → PUT，仅草稿） */
async function handleSave(): Promise<void> {
  submitError.value = '';
  const errors = validateStockOutForm(form, serialMaterialIds.value);
  formErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    submitError.value = '还有 ' + Object.keys(errors).length + ' 处没填对，请看标红的字段';
    return;
  }
  submitting.value = true;
  try {
    const payload = buildStockOutPayload(form);
    const saved = form.id ? await updateStockOut(form.id, payload) : await createStockOut(payload);
    formVisible.value = false;
    MessagePlugin.success(form.id ? '已保存修改' : `已保存草稿 ${saved.out_no}`);
    await loadOrders(current.value);
    await loadDetail(saved.id);
  } catch (error) {
    submitError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

/** 列表删除（仅草稿；后端软删） */
function confirmRemove(row: StockOutRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除出库单',
    body: `确认删除草稿 ${row.out_no}？删除后不可恢复（仅草稿可删，已过账请走作废）。`,
    theme: 'warning',
    confirmBtn: '确认删除',
    onConfirm: async () => {
      try {
        await deleteStockOut(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadOrders(current.value);
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

// ==================== 详情 ====================
const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<StockOutDetail | null>(null);
/**
 * VTRACE-4：本单的凭证来源（`STOCK_OUT`，`source_id = biz_stock_out.id`）。
 *
 * 出库过账才产生凭证（成本结转），草稿 / 待过账 = 没有凭证 = 按钮整只不出现。
 */
const voucherSources = computed(() =>
  detail.value ? stockOutSources(detail.value.id, detail.value.out_no) : [],
);
/** 动作失败（409 负库存的「可用量」就靠它原样展示） */
const actionError = ref('');
const acting = ref(false);

/** 状态历史（真实发生过的里程碑） */
const history = computed(() => (detail.value ? statusHistory(detail.value) : []));

/** 动作按钮：**完全由后端 `next_statuses` 派生** */
const detailActions = computed<StockOutActionSpec[]>(() =>
  detail.value ? statusActions(detail.value.status, detail.value.next_statuses) : [],
);

async function openDetail(row: StockOutRow): Promise<void> {
  await loadDetail(row.id);
}

async function loadDetail(id: string, keepError = false): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  if (!keepError) {
    actionError.value = '';
  }
  try {
    const loaded = await fetchStockOutDetail(id);
    detail.value = loaded;
    // 详情里的库位账行 / 仓库名要能译出来
    await Promise.all([loadLocationsFor(loaded), loadLocationRowsForDetail(loaded)]);
  } catch (error) {
    detailError.value = errorText(error);
  } finally {
    detailLoading.value = false;
  }
}

async function loadLocationsFor(target: StockOutDetail): Promise<void> {
  if (!target.warehouse_id) {
    return;
  }
  try {
    locations.value = await fetchLocationList(target.warehouse_id, true);
  } catch {
    /* 拉不到不影响主流程，id 会以短 id 展示 */
  }
}

/** 详情里每行的库位账行也拉一遍，把 id 译成「库位编码（可用 N）」 */
async function loadLocationRowsForDetail(target: StockOutDetail): Promise<void> {
  await Promise.all(
    target.items.map(async (line) => {
      const key = `${target.warehouse_id}|${line.material_id}`;
      if (locationRowCache.has(key)) {
        return;
      }
      try {
        const data = await fetchInventoryLocationPage({
          material_id: line.material_id,
          warehouse_id: target.warehouse_id,
          page: 1,
          page_size: OPTION_LIMIT,
        });
        locationRowCache.set(
          key,
          data.list.map((row) => {
            const master = locations.value.find((loc) => loc.id === row.location_id);
            const name = master
              ? `${master.location_code}${master.location_name ? ' ' + master.location_name : ''}`
              : row.location_id
                ? `库位 ${shortId(row.location_id)}`
                : '未分配库位';
            return { label: `${name}（可用 ${formatQty(row.qty)}）`, value: row.id };
          }),
        );
      } catch {
        locationRowCache.set(key, []);
      }
    }),
  );
}

// ==================== 状态流转 ====================
const postVisible = ref(false);
const postError = ref('');
const cancelVisible = ref(false);
const cancelReason = ref('');
const cancelError = ref('');

/** 点动作按钮：作废先要原因，过账先确认，其余（提交 / 撤回）直接走 */
function startAction(action: StockOutActionSpec): void {
  actionError.value = '';
  if (action.status === 'CANCELLED') {
    cancelReason.value = '';
    cancelError.value = '';
    cancelVisible.value = true;
    return;
  }
  if (action.status === 'POSTED') {
    postError.value = '';
    postVisible.value = true;
    return;
  }
  void runStatus(action.status, null);
}

async function confirmPost(): Promise<void> {
  await runStatus('POSTED', null, true);
}

async function confirmCancel(): Promise<void> {
  const message = validateCancelReason(cancelReason.value);
  if (message) {
    cancelError.value = message;
    return;
  }
  cancelError.value = '';
  await runStatus('CANCELLED', cancelReason.value.trim(), false, true);
}

/**
 * 执行状态流转。
 *
 * 失败时**把后端报文原样展示**（`errorText` 会带上错误码）：
 * 负库存拦截的报文里有「物料 / 仓库 / 可用量」，用户据此改数量再提交；
 * 只显示「操作失败」等于把唯一有用的信息丢掉。
 */
async function runStatus(
  status: 'POSTED' | 'PENDING_POST' | 'DRAFT' | 'CANCELLED',
  reason: string | null,
  fromPostDialog = false,
  fromCancelDialog = false,
): Promise<void> {
  if (!detail.value) {
    return;
  }
  acting.value = true;
  const targetId = detail.value.id;
  try {
    const payload = status === 'CANCELLED' ? buildCancelPayload(reason ?? '') : { status };
    const updated = await updateStockOutStatus(targetId, payload);
    detail.value = updated;
    postVisible.value = false;
    cancelVisible.value = false;
    MessagePlugin.success(
      status === 'POSTED'
        ? `已过账，成本结转金额 ${formatMoney(updated.total_amount)}`
        : status === 'CANCELLED'
          ? '已作废（三本账已回滚）'
          : status === 'PENDING_POST'
            ? '已提交待过账'
            : '已撤回为草稿',
    );
    await loadOrders(current.value);
    await loadLocationsFor(updated);
    await loadLocationRowsForDetail(updated);
  } catch (error) {
    const text = errorText(error);
    if (fromPostDialog) {
      postError.value = text;
    }
    if (fromCancelDialog) {
      cancelError.value = text;
    }
    // 抽屉里的常驻提示：过账弹窗关掉后用户也能看到为什么失败
    actionError.value = isNegativeStockError(error)
      ? `${text}——请按上面点名物料的「可用量」改小数量或换仓库后重试`
      : text;
  } finally {
    acting.value = false;
  }
}

onMounted(async () => {
  await loadBasics();
  applyOutNoDeepLink();
  void loadOrders(1);
  // 一键领料：成本对象详情页「领料」跳进来时带 ?cost_object_id=…
  await applyRouteIssuePrefill();
});

/**
 * 深链 `?out_no=OUT-…`（COST-B2-12）：「未归集清单」里点「未挂成本对象的领料」跳进来时，
 * 直接把这张单筛出来（本页列表的 `keyword` 支持单号模糊匹配）。
 *
 * 与 `?cost_object_id=` 同一个纪律：只读 query、只在有值时写 `filter.keyword`
 * （不带参数进页面时不能清掉用户手填的筛选），处理完把 query 摘掉防止刷新又筛一次。
 */
function applyOutNoDeepLink(): void {
  const raw = route.query.out_no;
  const outNo = (typeof raw === 'string' ? raw : Array.isArray(raw) ? String(raw[0] ?? '') : '').trim();
  if (!outNo) {
    return;
  }
  filter.keyword = outNo;
  const rest = { ...route.query };
  delete rest.out_no;
  void router.replace({ path: route.path, query: rest });
}
</script>

<style scoped lang="less">
.stockout-page {
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
  width: 130px;
}

.tool-row__type {
  width: 120px;
}

.tool-row__warehouse {
  width: 190px;
}

.tool-row__range {
  width: 240px;
}

.tool-row__keyword {
  width: 220px;
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

/* 草稿 / 待过账的金额是「还没有」，用弱色 + 斜体，避免被当成真 0 */
.money--pending {
  color: var(--td-text-color-placeholder);
  font-style: italic;
  font-family: inherit;
}

.num {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 10px;
}

.panel-empty {
  padding: 36px 0;
  text-align: center;
}

.panel-empty__icon {
  font-size: 40px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 8px 0 4px;
  font-size: 14px;
  color: var(--td-text-color-primary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 表单 ==================== */

.form {
  margin-top: 4px;
}

.form-row {
  display: flex;
  gap: 16px;
}

.form-row__col {
  flex: 1 1 0;
  min-width: 0;
}

.reason-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.reason-field__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* 成本对象选择器 + 借方科目预览（B1-12） */
.cost-object-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.cost-object-field__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.cost-object-field__hint strong {
  color: var(--td-brand-color);
  font-family: var(--td-font-family-mono, monospace);
}

.section-title {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.section-title__hint {
  font-size: 12px;
  font-weight: 400;
  color: var(--td-text-color-placeholder);
}

.dlg-alert {
  margin-bottom: 12px;
}

.items-alert {
  margin-bottom: 8px;
}

.items {
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  overflow: hidden;
}

.items__head,
.items__row {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 8px 12px;
}

.items__head {
  background: var(--td-bg-color-container-hover);
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.items__row + .items__row {
  border-top: 1px solid var(--td-component-stroke);
}

.items__col {
  min-width: 0;
}

.items__col--material {
  flex: 1 1 0;
}

.items__col--qty {
  flex: 0 0 130px;
}

.items__col--location {
  flex: 0 0 210px;
}

.items__col--op {
  flex: 0 0 52px;
  display: flex;
  align-items: center;
  height: 32px;
}

.items__error {
  display: block;
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-error-color);
}

.items__add {
  margin-top: 10px;
}

.drawer-footer {
  display: flex;
  gap: 12px;
  margin-top: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-stroke);
}

/* ==================== 详情抽屉 ==================== */

.drawer-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 24px 0;
  color: var(--td-text-color-secondary);
}

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 12px;
}

/* PDF-08：把「下载 PDF」推到右上角（左侧标签保持左对齐） */
.detail__spacer {
  margin-left: auto;
}

.detail__no {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__alert {
  margin-bottom: 12px;
}

.detail__alert-text {
  white-space: pre-wrap;
  word-break: break-word;
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px 16px;
}

.detail-field {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.detail-field--wide {
  grid-column: span 3;
}

.detail-field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.detail-field__value {
  font-size: 13px;
  color: var(--td-text-color-primary);
  word-break: break-word;
}

.history {
  margin: 0;
  padding: 0;
  list-style: none;
}

.history__item {
  position: relative;
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 4px 0 4px 14px;
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.history__dot {
  position: absolute;
  left: 0;
  top: 11px;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--td-brand-color);
}

.history__time {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.history__note {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.detail__actions {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-stroke);
}

.detail__actions-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 弹窗 ==================== */

.dialog-text {
  margin: 0 0 12px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--td-text-color-primary);
}

.dialog-error {
  margin: 10px 0 0;
  padding: 8px 10px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 13px;
  line-height: 1.6;
  color: var(--td-error-color);
  white-space: pre-wrap;
  word-break: break-word;
}

.dialog-footer {
  display: flex;
  gap: 12px;
  margin-top: 16px;
}
</style>
