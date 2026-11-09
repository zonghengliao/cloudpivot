<template>
  <div class="stocktake-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-stocktake-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建盘点单
        </t-button>
        <t-button variant="outline" data-testid="btn-stocktake-refresh" @click="refresh">
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
          data-testid="stocktake-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.take_type"
          class="tool-row__type"
          :options="typeFilterOptions"
          placeholder="全部类型"
          clearable
          data-testid="stocktake-filter-type"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.warehouse_id"
          class="tool-row__warehouse"
          :options="warehouseOptions"
          placeholder="全部仓库"
          clearable
          filterable
          data-testid="stocktake-filter-warehouse"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="盘点单号（模糊）"
          clearable
          data-testid="stocktake-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-stocktake-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-stocktake-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">盘点单</span>
          <span class="panel-head__meta" data-testid="stocktake-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          过账时按差异正负分别走入库 / 出库，并生成「盘点差异凭证」（盘盈 借 1405 贷 1901；盘亏 借 1901 贷 1405）
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="stocktake-load-error"
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
        data-testid="stocktake-table"
        @page-change="handlePageChange"
      >
        <template #take_no="{ row }">
          <span class="mono" :data-testid="`stocktake-no-${row.take_no}`">{{ row.take_no }}</span>
        </template>

        <template #take_type_label="{ row }">
          <t-tag size="small" variant="outline" :theme="typeTheme(row.take_type)">
            {{ row.take_type_label || takeTypeLabel(row.take_type) }}
          </t-tag>
        </template>

        <template #warehouse_id="{ row }">{{ warehouseNameOf(row.warehouse_id) }}</template>

        <template #take_date="{ row }">
          <span class="muted">{{ shortDate(row.take_date) }}</span>
        </template>

        <template #qty_pair="{ row }">
          <span class="num" :data-testid="`stocktake-book-${row.take_no}`">{{ formatQty(row.total_book_qty) }}</span>
          <span class="muted"> / </span>
          <span class="num" :data-testid="`stocktake-actual-${row.take_no}`">{{ formatQty(row.total_actual_qty) }}</span>
        </template>

        <!-- ★ 差异金额：草稿 / 待过账恒为 0（过账才按账上真实差额写），列标题已标口径 -->
        <template #total_diff_amount="{ row }">
          <span
            class="money"
            :class="{ 'money--pending': row.status === 'DRAFT' || row.status === 'PENDING_POST' }"
            :data-testid="`stocktake-diff-${row.take_no}`"
          >
            {{
              row.status === 'POSTED' || row.status === 'CANCELLED'
                ? formatMoney(row.total_diff_amount)
                : '过账后才有值'
            }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="stockTakeStatusTheme(row.status)" :data-testid="`stocktake-status-${row.take_no}`">
            {{ row.status_label || stockTakeStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <!-- 草稿的主要动作是「填实盘数」，其它状态是「查看」——同一个抽屉，
                 不摆两个指向同一处的链接（列表里少一个链接，扫读一行更快） -->
            <t-link theme="primary" :data-testid="`btn-view-${row.take_no}`" @click="openDetail(row)">
              {{ canEdit(row) ? '填实盘数' : '查看' }}
            </t-link>
            <t-link
              v-if="canDelete(row)"
              v-privilege="[PERM.remove]"
              theme="danger"
              :data-testid="`btn-remove-${row.take_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="stocktake-empty">
            <t-icon name="inbox" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的盘点单' : '还没有盘点单' }}</p>
            <p class="panel-empty__desc">
              {{
                filtered
                  ? '换个条件再查，或点「重置」回全量'
                  : '点左上角「新建盘点单」录入第一张（全盘 / 抽盘都会自动快照账面数）'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建抽屉（12 列栅格） ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      header="新建盘点单"
      size="900px"
      :footer="false"
      data-testid="stocktake-form-drawer"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" data-testid="stocktake-submit-error" />

      <t-alert theme="info" class="dlg-alert" data-testid="stocktake-snapshot-note">
        <template #message>
          账面数由服务端在建单时从<strong>即时库存</strong>快照（页面不填、也不许改）；
          差异 = 实盘 − 账面，由数据库生成列算，同一份口径不靠代码自觉。
        </template>
      </t-alert>

      <!--
        E2E01-FIX-1：新建盘点单还没有 id（查不了 `/attachment/check`），但规则确定
        （`STOCK_TAKE` + `CONFIRM`，V0347：盘点表或盈亏说明必传）—— 先提示，
        建单后在详情「附件」上传，传完「提交审批」按钮才会变亮。
      -->
      <t-alert
        theme="warning"
        class="dlg-alert"
        data-testid="stocktake-create-attach-notice"
        message="提交审批前必须上传盘点表或盈亏说明（pdf/jpg/png）：建单后在详情「附件」上传，传完「提交审批」按钮才会变亮。"
      />

      <t-form :data="form" label-width="96px" class="form">
        <t-row :gutter="16">
          <t-col :span="6">
            <t-form-item
              label="盘点类型"
              name="take_type"
              :status="formErrors.take_type ? 'error' : undefined"
              :tips="formErrors.take_type"
            >
              <t-radio-group :value="form.take_type" data-testid="stocktake-form-type" @change="handleTypeChange">
                <t-radio v-for="option in TAKE_TYPE_OPTIONS" :key="option.value" :value="option.value">
                  {{ option.label }}
                </t-radio>
              </t-radio-group>
            </t-form-item>
          </t-col>

          <t-col :span="6">
            <t-form-item
              label="盘点仓库"
              name="warehouse_id"
              :status="formErrors.warehouse_id ? 'error' : undefined"
              :tips="formErrors.warehouse_id"
            >
              <t-select
                v-model="form.warehouse_id"
                :options="warehouseOptions"
                placeholder="选择盘点仓库"
                filterable
                data-testid="stocktake-form-warehouse"
                @change="handleWarehouseChange"
              />
            </t-form-item>
          </t-col>

          <t-col :span="6">
            <t-form-item
              label="盘点日期"
              name="take_date"
              :status="formErrors.take_date ? 'error' : undefined"
              :tips="formErrors.take_date"
            >
              <t-date-picker
                v-model="form.take_date"
                format="YYYY-MM-DD"
                placeholder="选择盘点日期"
                data-testid="stocktake-form-date"
              />
            </t-form-item>
          </t-col>

          <t-col :span="6">
            <t-form-item label="类型说明">
              <span class="field-hint" data-testid="stocktake-type-hint">{{ typeHint }}</span>
            </t-form-item>
          </t-col>

          <t-col :span="12">
            <t-form-item label="备注" name="remark" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark">
              <t-textarea
                v-model="form.remark"
                :maxlength="STOCK_TAKE_MAX_REMARK"
                :autosize="{ minRows: 2, maxRows: 4 }"
                placeholder="选填（≤ 500 字）：盘点范围 / 组织说明"
                data-testid="stocktake-form-remark"
              />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <!-- 全盘：范围由服务端按仓库库存桶快照，不需要用户填 -->
      <t-alert v-if="form.take_type === 'FULL'" theme="info" class="dlg-alert" data-testid="stocktake-full-note">
        <template #message>
          全盘会按所选仓库<strong>现有全部库存桶</strong>自动生成本次盘点明细（一条不少）；
          仓库没有库存行时后端会拒绝建单并提示改用抽盘。
        </template>
      </t-alert>

      <!-- 抽盘：逐行指定 物料 × 库位 -->
      <template v-else>
        <div class="section-title">
          盘点范围（{{ form.items.length }} 行）
          <span class="section-title__hint">只填「盘谁」，账面数由服务端快照</span>
        </div>
        <t-alert v-if="formErrors.items" theme="error" :message="formErrors.items" class="items-alert" data-testid="stocktake-items-error" />

        <div v-if="form.items.length > 0" class="items" data-testid="stocktake-form-items">
          <div class="items__head">
            <span class="items__col items__col--material">物料</span>
            <span class="items__col items__col--location">库位账行（可空 = 未分配桶）</span>
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
                :data-testid="`stocktake-item-material-${index}`"
                @change="() => loadLocationRows(item)"
              />
              <span v-if="itemError(index, 'material_id')" class="items__error" :data-testid="`stocktake-item-material-error-${index}`">
                {{ itemError(index, 'material_id') }}
              </span>
            </div>

            <div class="items__col items__col--location">
              <t-select
                v-model="item.location_id"
                :options="locationOptionsOf(item)"
                :placeholder="locationPlaceholderOf(item)"
                clearable
                :disabled="!item.material_id || !form.warehouse_id"
                :data-testid="`stocktake-item-location-${index}`"
              />
            </div>

            <div class="items__col items__col--op">
              <t-link theme="danger" :data-testid="`btn-stocktake-item-remove-${index}`" @click="removeItem(index)">
                删除
              </t-link>
            </div>
          </div>
        </div>

        <t-button
          variant="outline"
          size="small"
          class="items__add"
          :disabled="form.items.length >= STOCK_TAKE_MAX_LINES"
          data-testid="btn-stocktake-item-add"
          @click="addItem"
        >
          <template #icon><t-icon name="add" /></template>
          添加盘点物料
        </t-button>
      </template>

      <div class="drawer-footer">
        <t-button
          theme="primary"
          :loading="submitting"
          :disabled="submitting"
          data-testid="btn-stocktake-save"
          @click="handleSave"
        >
          保存草稿
        </t-button>
        <t-button variant="outline" data-testid="btn-stocktake-form-cancel" @click="formVisible = false">取消</t-button>
      </div>
    </t-drawer>

    <!-- ==================== ④ 详情 / 填实盘数抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="盘点单详情"
      size="900px"
      :footer="false"
      data-testid="stocktake-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading"><t-loading size="small" /> 加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />
      <t-alert v-else-if="detailWarn" theme="warning" :message="detailWarn" class="dlg-alert" data-testid="stocktake-detail-warn" />

      <div v-if="!detailLoading && detail" class="detail">
        <div class="detail__head">
          <span class="detail__no mono" data-testid="stocktake-detail-no">{{ detail.take_no }}</span>
          <t-tag size="small" :theme="stockTakeStatusTheme(detail.status)" data-testid="stocktake-detail-status">
            {{ detail.status_label || stockTakeStatusLabel(detail.status) }}
          </t-tag>
          <t-tag size="small" variant="outline" :theme="typeTheme(detail.take_type)" data-testid="stocktake-detail-type">
            {{ detail.take_type_label || takeTypeLabel(detail.take_type) }}
          </t-tag>
          <span v-if="detail.status === 'DRAFT'" class="detail__hint" data-testid="stocktake-detail-edit-hint">
            草稿：可直接改「实盘数」，改完点右侧「保存实盘数」
          </span>
          <span class="detail__spacer" />
          <!-- PDF-08：下载 PDF（盘点单） -->
          <DocPdfButton
            :path="`/api/business/erp/stock-take/${detail.id}/pdf`"
            :filename="`盘点单-${detail.take_no}.pdf`"
            testid="stocktake-detail-pdf"
          />
        </div>

        <!-- 动作失败：报文原样展示（409 没配流程 / 边不合法都靠它） -->
        <t-alert
          v-if="actionError"
          theme="error"
          class="detail__alert"
          data-testid="stocktake-action-error"
        >
          <template #message>
            <span class="detail__alert-text">{{ actionError }}</span>
          </template>
          <template v-if="flowNotConfigured" #operation>
            <t-button size="small" theme="warning" variant="outline" data-testid="btn-stocktake-open-skip" @click="openSkipDialog">
              显式跳过审批…
            </t-button>
          </template>
        </t-alert>

        <div class="detail__grid">
          <div class="detail-field">
            <span class="detail-field__label">盘点仓库</span>
            <span class="detail-field__value" data-testid="stocktake-detail-warehouse">{{ warehouseNameOf(detail.warehouse_id) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">盘点日期</span>
            <span class="detail-field__value">{{ detail.take_date }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">账面数量合计</span>
            <span class="detail-field__value num">{{ formatQty(detail.total_book_qty) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">实盘数量合计</span>
            <span class="detail-field__value num" data-testid="stocktake-detail-actual">{{ formatQty(detail.total_actual_qty) }}</span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">盘盈 / 盘亏</span>
            <span class="detail-field__value">
              <span class="num gain" data-testid="stocktake-detail-gain">{{ gainLoss.gain }}</span>
              <span class="muted"> / </span>
              <span class="num loss" data-testid="stocktake-detail-loss">{{ gainLoss.loss }}</span>
              <span v-if="!gainLoss.fromBackend" class="detail-field__note" data-testid="stocktake-gainloss-note">
                按明细差异汇总（过账后以后端合计为准）
              </span>
            </span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">差异金额</span>
            <span class="detail-field__value money" data-testid="stocktake-detail-diff">
              {{
                detail.status === 'POSTED' || detail.status === 'CANCELLED'
                  ? formatMoney(detail.total_diff_amount)
                  : '过账后才有值'
              }}
            </span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">差异凭证</span>
            <span class="detail-field__value mono" data-testid="stocktake-detail-voucher">
              <t-link
                v-if="detail.voucher_no"
                theme="primary"
                :data-testid="`stocktake-voucher-link-${detail.voucher_no}`"
                @click.stop="openVoucher(detail.voucher_no)"
              >
                {{ detail.voucher_no }}
              </t-link>
              <span v-else-if="detail.voucher_id" class="muted" data-testid="stocktake-detail-voucher-missing">
                {{ `凭证 id ${shortId(detail.voucher_id)}（凭证号查不到，可能已删除）` }}
              </span>
              <span v-else class="muted">—（未过账 / 零差异无凭证）</span>
            </span>
          </div>
          <div v-if="detail.reverse_voucher_id" class="detail-field">
            <span class="detail-field__label">红冲凭证</span>
            <span class="detail-field__value mono" data-testid="stocktake-detail-reverse-voucher">
              <t-link
                v-if="detail.reverse_voucher_no"
                theme="primary"
                :data-testid="`stocktake-reverse-voucher-link-${detail.reverse_voucher_no}`"
                @click.stop="openVoucher(detail.reverse_voucher_no)"
              >
                {{ detail.reverse_voucher_no }}
              </t-link>
              <span v-else class="muted" data-testid="stocktake-detail-reverse-voucher-missing">
                {{ `凭证 id ${shortId(detail.reverse_voucher_id)}（凭证号查不到，可能已删除）` }}
              </span>
            </span>
          </div>
          <div class="detail-field">
            <span class="detail-field__label">审批实例</span>
            <span class="detail-field__value mono">{{ detail.approval_instance_id ? shortId(detail.approval_instance_id) : '—' }}</span>
          </div>
          <div class="detail-field detail-field--wide">
            <span class="detail-field__label">备注</span>
            <span class="detail-field__value">{{ dash(detail.remark) }}</span>
          </div>
          <div v-if="detail.cancel_reason" class="detail-field detail-field--wide">
            <span class="detail-field__label">作废原因</span>
            <span class="detail-field__value" data-testid="stocktake-detail-cancel-reason">{{ detail.cancel_reason }}</span>
          </div>
        </div>

        <!-- 状态历史：只为真实发生过的里程碑打点 -->
        <div class="section-title">状态历史</div>
        <ul class="history" data-testid="stocktake-history">
          <li v-for="entry in history" :key="entry.key" class="history__item">
            <span class="history__dot" />
            <span class="history__label">{{ entry.label }}</span>
            <span class="history__time">{{ formatDateTime(entry.at) }}</span>
            <span v-if="entry.note" class="history__note">{{ entry.note }}</span>
          </li>
        </ul>

        <div class="section-title">
          明细（{{ detail.items.length }} 条）
          <span class="section-title__hint">
            账面数 / 差异只读（账面 = 建单快照，差异 = PG 生成列）；单价与差异金额「过账后才有值」
          </span>
        </div>

        <t-table row-key="id" size="small" :data="detail.items" :columns="detailColumns" data-testid="stocktake-detail-items">
          <template #material="{ row }">
            <span class="mono">{{ row.material_code || shortId(row.material_id) }}</span>
            <span class="muted"> {{ row.material_name || '' }}</span>
          </template>
          <template #book_qty="{ row }">
            <span class="num">{{ formatQty(row.book_qty) }}</span>
          </template>
          <template #actual_qty="{ row }">
            <!-- 草稿可编辑；其它状态一律只读（过账后差异不许再动） -->
            <t-input
              v-if="editable"
              v-model="actuals[row.id]"
              class="actual-input"
              placeholder="实盘数"
              :data-testid="`stocktake-actual-input-${row.line_no}`"
            />
            <span v-else class="num" :data-testid="`stocktake-actual-text-${row.line_no}`">{{ formatQty(row.actual_qty) }}</span>
          </template>
          <template #diff_qty="{ row }">
            <span
              class="num"
              :class="{
                gain: diffDirection(row.diff_qty) === 'gain',
                loss: diffDirection(row.diff_qty) === 'loss',
              }"
              :data-testid="`stocktake-diff-qty-${row.line_no}`"
            >
              {{ formatDiff(row.diff_qty) }}
            </span>
          </template>
          <template #unit_cost="{ row }">
            <span class="money" :class="{ 'money--pending': Number(row.unit_cost) === 0 }">
              {{ Number(row.unit_cost) === 0 ? '—' : formatMoney(row.unit_cost) }}
            </span>
          </template>
          <template #diff_amount="{ row }">
            <span class="money" :class="{ 'money--pending': Number(row.diff_amount) === 0 }">
              {{ Number(row.diff_amount) === 0 ? '—' : formatMoney(row.diff_amount) }}
            </span>
          </template>
          <template #location_id="{ row }">{{ locationLabelOf(row.location_id) }}</template>
        </t-table>

        <t-alert v-if="actualError" theme="error" :message="actualError" class="dlg-alert" data-testid="stocktake-actual-error" />

        <!--
          ATT-09：盘点表 / 盈亏说明的上传入口 —— 提交审批前必须已有附件
          （后端 ATT-06 闸门，规则 `STOCK_TAKE` + `CONFIRM`）。
        -->
        <div class="section-title">附件（盘点表 / 盈亏说明）</div>
        <DocAttachmentPanel
          :doc-type="DOC_KIND.STOCK_TAKE"
          :doc-id="detail.id"
          :default-role="DEFAULT_ROLE_BY_DOC.STOCK_TAKE"
          testid-prefix="stocktake-attach"
          @changed="refreshAttachGate()"
        />

        <div v-if="editable" class="detail__save-row">
          <t-button
            v-privilege="[PERM.update]"
            theme="primary"
            variant="outline"
            :loading="savingActuals"
            data-testid="btn-stocktake-save-actuals"
            @click="handleSaveActuals"
          >
            保存实盘数
          </t-button>
          <span class="detail__save-hint">保存只写草稿，不动任何账；差异合计会在保存后重算</span>
        </div>

        <!-- 动作按钮：只由后端 next_statuses 驱动（前端不写第二份状态机） -->
        <div class="detail__actions" data-testid="stocktake-detail-actions">
          <template v-for="action in detailActions" :key="action.status">
            <!--
              ATT-09：只有「提交审批」（DRAFT → PENDING_POST）受附件闸门约束
              （规则 `STOCK_TAKE` + `CONFIRM`：盘点表 / 盈亏说明必传）。
              其它动作（过账 / 撤回 / 作废）与附件无关，不加遮罩也不加 tooltip。
            -->
            <t-tooltip :content="actionTip(action)" :disabled="!actionTip(action)" placement="top-left">
              <span class="op-wrap">
                <t-button
                  v-privilege="[PERM.status]"
                  :theme="action.theme === 'default' ? 'default' : action.theme"
                  :variant="action.theme === 'primary' ? 'base' : 'outline'"
                  :disabled="!hasStatusPermission || actionGateBlocked(action)"
                  :data-testid="action.testid"
                  @click="startAction(action)"
                >
                  {{ action.label }}
                </t-button>
              </span>
            </t-tooltip>
          </template>
          <AttachmentGateHint :hint="attachGate.hint" testid="stocktake-detail-gate-hint" />
          <span v-if="detailActions.length === 0" class="detail__actions-hint" data-testid="stocktake-no-actions">
            {{ detail.status === 'CANCELLED' ? '已作废：单据不可再流转' : '当前状态没有可执行动作' }}
          </span>
          <t-button variant="outline" data-testid="btn-stocktake-drawer-close" @click="detailVisible = false">关闭</t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ⑤ 提交审批 / 过账确认弹窗 ==================== -->
    <t-dialog
      v-model:visible="confirmVisible"
      :header="confirmHeader"
      width="580px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stocktake-confirm-dialog"
    >
      <p class="dialog-text" data-testid="stocktake-confirm-text">{{ confirmText }}</p>
      <p v-if="confirmError" class="dialog-error" data-testid="stocktake-confirm-error">{{ confirmError }}</p>
      <div class="dialog-footer">
        <t-button
          theme="primary"
          :loading="acting"
          :data-testid="confirmTarget === 'POSTED' ? 'btn-stocktake-post-confirm' : 'btn-stocktake-submit-confirm'"
          @click="confirmTransition"
        >
          {{ confirmTarget === 'POSTED' ? '确认过账' : '提交审批' }}
        </t-button>
        <t-button variant="outline" data-testid="btn-stocktake-confirm-cancel" @click="confirmVisible = false">取消</t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（原因必填） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废盘点单"
      width="580px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stocktake-cancel-dialog"
    >
      <p class="dialog-text">
        作废 <strong class="mono">{{ detail?.take_no }}</strong>
        <template v-if="detail?.status === 'POSTED'">
          ：三本账按明细快照回滚、补一条冲销流水，并<strong>红冲</strong>盘点差异凭证。
        </template>
        <template v-else>：草稿 / 待过账单据作废不动任何账。</template>
        作废原因会进状态日志，已过账的还会进红字凭证摘要，<strong>必填</strong>。
      </p>
      <t-textarea
        v-model="cancelReason"
        :maxlength="STOCK_TAKE_MAX_REASON"
        :autosize="{ minRows: 3, maxRows: 5 }"
        placeholder="例如：仓库选错，重新建一张"
        data-testid="stocktake-cancel-reason"
      />
      <p v-if="cancelError" class="dialog-error" data-testid="stocktake-cancel-error">{{ cancelError }}</p>
      <div class="dialog-footer">
        <t-button theme="danger" :loading="acting" data-testid="btn-stocktake-cancel-confirm" @click="confirmCancel">
          确认作废
        </t-button>
        <t-button variant="outline" data-testid="btn-stocktake-cancel-dismiss" @click="cancelVisible = false">取消</t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑦ 显式跳过审批弹窗（409 66033 的唯一出口） ==================== -->
    <t-dialog
      v-model:visible="skipVisible"
      header="显式跳过审批"
      width="620px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="stocktake-skip-dialog"
    >
      <t-alert theme="warning" class="dlg-alert" data-testid="stocktake-skip-note">
        <template #message>
          没有已发布的审批流程 <strong>stock_take_post</strong>，系统默认<strong>拒绝</strong>提交（fail-closed）。
          跳过审批属内控例外：需要权限码 <strong>{{ PERM_SKIP_APPROVAL }}</strong>，
          且理由 ≥ {{ MIN_SKIP_REASON_CHARS }} 字 —— 理由会写进单据状态日志，审计据此追溯。
        </template>
      </t-alert>
      <t-textarea
        v-model="skipReason"
        :maxlength="STOCK_TAKE_MAX_REASON"
        :autosize="{ minRows: 3, maxRows: 5 }"
        placeholder="例如：年度盘点点位已由仓管与财务双人现场复核，本次免流程"
        data-testid="stocktake-skip-reason"
      />
      <p v-if="skipError" class="dialog-error" data-testid="stocktake-skip-error">{{ skipError }}</p>
      <div class="dialog-footer">
        <t-button theme="warning" :loading="acting" data-testid="btn-stocktake-skip-confirm" @click="confirmSkip">
          带理由提交审批
        </t-button>
        <t-button variant="outline" data-testid="btn-stocktake-skip-dismiss" @click="skipVisible = false">取消</t-button>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { DEFAULT_ROLE_BY_DOC, type DocType } from '@/api/business/erp/attachment';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { useUserStore } from '@/stores/user';
import AttachmentGateHint from '@/views/business/erp/shared/AttachmentGateHint.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import DocPdfButton from '@/components/DocPdfButton.vue';
import { gateButtonTip } from '@/views/business/erp/shared/attachment-gate-pure';
import { useAttachmentGate } from '@/views/business/erp/shared/use-attachment-gate';
import {
  fetchInventoryLocationPage,
  fetchLocationList,
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import {
  createStockTake,
  deleteStockTake,
  fetchStockTakeDetail,
  fetchStockTakePage,
  updateStockTake,
  updateStockTakeStatus,
  type StockTakeDetail,
  type StockTakeRow,
} from '@/api/business/erp/stock-take';
import {
  MIN_SKIP_REASON_CHARS,
  PERM_SKIP_APPROVAL,
  STOCK_TAKE_MAX_LINES,
  STOCK_TAKE_MAX_REASON,
  STOCK_TAKE_MAX_REMARK,
  STOCK_TAKE_PAGE_SIZE,
  STOCK_TAKE_STATUS_FILTER_OPTIONS,
  TAKE_TYPE_OPTIONS,
  aggregateDiff,
  buildActualPayload,
  buildCancelPayload,
  buildSkipSubmitPayload,
  buildStockTakePayload,
  buildStockTakeQuery,
  canDelete,
  canEdit,
  dash,
  detailToActuals,
  diffDirection,
  emptyScopeItem,
  emptyStockTakeFilter,
  emptyStockTakeForm,
  errorText,
  formatDateTime,
  formatDiff,
  formatMoney,
  formatQty,
  gainLossDisplay,
  isFlowNotConfigured,
  isStockTakeFiltered,
  shortDate,
  shortId,
  statusActions,
  statusHistory,
  stockTakeStatusLabel,
  stockTakeStatusTheme,
  takeTypeLabel,
  validateActuals,
  validateCancelReason,
  validateSkipReason,
  validateStockTakeForm,
  type StockTakeActionSpec,
  type StockTakeActualState,
  type StockTakeFilterState,
  type StockTakeFormErrors,
  type StockTakeFormState,
  type StockTakeScopeItemForm,
} from './stock-take-logic';

/** 本页权限码（与 V0211 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:stock-take:list',
  detail: 'business:erp:stock-take:detail',
  create: 'business:erp:stock-take:create',
  update: 'business:erp:stock-take:update',
  remove: 'business:erp:stock-take:delete',
  status: 'business:erp:stock-take:status',
} as const;

/** 主数据下拉一次拉多少（与出库 / 收货页同口径） */
const OPTION_LIMIT = 200;

/** 路由（「差异凭证 / 红冲凭证」点号跳凭证列表核账） */
const router = useRouter();

/** 表格列（宽度合计 940px：实测 1280 视口、侧栏展开时表体可视宽 968px，
 *  留 28px 余量 —— 不溢出才不会让固定「操作」列盖住「状态」列） */
const columns: PrimaryTableCol[] = [
  { colKey: 'take_no', title: '盘点单号', width: 170 },
  { colKey: 'take_type_label', title: '类型', width: 70 },
  { colKey: 'warehouse_id', title: '仓库', minWidth: 100, ellipsis: true },
  { colKey: 'take_date', title: '盘点日期', width: 90 },
  // 账面 / 实盘合成一列：「0 / 5」比两列各 110px 更好扫，也省掉一次横向滚动
  { colKey: 'qty_pair', title: '账面 / 实盘', width: 130, align: 'right' },
  // ★ 差异金额：单元格里写「过账后才有值」，所以表头不必再堆注解（省 3 行换行）
  { colKey: 'total_diff_amount', title: '差异金额', width: 120, align: 'right' },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right' },
];

/** 详情抽屉里的明细列（宽度合计 ≈ 800px：900px 抽屉内不横向滚动，
 *  否则「未分配库位」这类尾列会被裁掉一半） */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 46 },
  { colKey: 'material', title: '物料', minWidth: 150 },
  { colKey: 'book_qty', title: '账面数', width: 90, align: 'right' },
  { colKey: 'actual_qty', title: '实盘数', width: 120, align: 'right' },
  { colKey: 'diff_qty', title: '差异', width: 80, align: 'right' },
  { colKey: 'unit_cost', title: '单价', width: 90, align: 'right' },
  { colKey: 'diff_amount', title: '差异金额', width: 100, align: 'right' },
  { colKey: 'location_id', title: '库位账行', width: 120, ellipsis: true },
];

const statusFilterOptions = STOCK_TAKE_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
const typeFilterOptions = TAKE_TYPE_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================
const filter = reactive<StockTakeFilterState>(emptyStockTakeFilter());
const rows = ref<StockTakeRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = STOCK_TAKE_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const filtered = computed(() => isStockTakeFiltered(filter));

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
/** 库位主数据（id → 编码 / 名称），按仓库懒加载 */
const locations = ref<Array<{ id: string; location_code: string; location_name: string | null }>>([]);
/** 库位账行缓存（key = `${warehouse_id}|${material_id}`） */
const locationRowCache = new Map<string, Array<{ label: string; value: string }>>();

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_code} ${item.warehouse_name}`,
    value: item.id,
  })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}`,
    value: item.id,
  })),
);

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

/** 盘点类型标签主题（抽盘 = primary，全盘 = 默认） */
function typeTheme(takeType: string): 'default' | 'primary' | 'warning' {
  switch (takeType) {
    case 'FULL':
      return 'default';
    case 'PARTIAL':
      return 'primary';
    default:
      return 'warning';
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
    const data = await fetchStockTakePage(buildStockTakeQuery(filter, page, pageSize));
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
  Object.assign(filter, emptyStockTakeFilter());
  void loadOrders(1);
}

function handlePageChange(pageInfo: { current: number }): void {
  void loadOrders(pageInfo.current);
}

// ==================== 建单表单 ====================
const formVisible = ref(false);
const form = reactive<StockTakeFormState>(emptyStockTakeForm());
const formErrors = ref<StockTakeFormErrors>({});
const submitting = ref(false);
const submitError = ref('');
let lineSeed = 0;

/** 类型说明（全盘 / 抽盘的行为差别讲人话） */
const typeHint = computed(() =>
  form.take_type === 'FULL'
    ? '全盘：该仓所有库存桶一次性盘完'
    : '抽盘：只盘下面列出的「物料 × 库位」',
);

function resetForm(): void {
  Object.assign(form, emptyStockTakeForm());
  formErrors.value = {};
  submitError.value = '';
  lineSeed = 0;
  locationRowCache.clear();
}

function openCreate(): void {
  resetForm();
  formVisible.value = true;
}

function addItem(): void {
  lineSeed += 1;
  form.items.push(emptyScopeItem(`line-${lineSeed}`));
}

function removeItem(index: number): void {
  form.items.splice(index, 1);
  // 索引型错误键会错位，删行后整体重算一遍
  formErrors.value = validateStockTakeForm(form);
}

/** 行内错误（`items.<index>.<field>`） */
function itemError(index: number, field: 'material_id'): string {
  return formErrors.value[`items.${index}.${field}`] ?? '';
}

/** 切换类型：抽盘 → 自动补一行；全盘 → 清空范围（后端对「FULL + items」直接 400） */
function handleTypeChange(value: string | number | boolean): void {
  const next = String(value) === 'PARTIAL' ? 'PARTIAL' : 'FULL';
  form.take_type = next;
  if (next === 'FULL') {
    form.items = [];
  } else if (form.items.length === 0) {
    addItem();
  }
  formErrors.value = {};
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
function locationPlaceholderOf(item: StockTakeScopeItemForm): string {
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
 * 要的是 `biz_inventory_location.id`（**库位账行**，不是库位主数据的 id）——
 * 后端建单时按「物料 × 仓库」校验它归不归这一行，传错就是 400。
 */
async function loadLocationRows(item: StockTakeScopeItemForm): Promise<void> {
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
        return { label: `${name}（账面 ${formatQty(line.qty)}）`, value: line.id };
      }),
    );
  } catch {
    locationRowCache.set(key, []);
  }
}

function locationOptionsOf(item: StockTakeScopeItemForm): Array<{ label: string; value: string }> {
  if (!item.material_id || !form.warehouse_id) {
    return [];
  }
  return locationRowCache.get(`${form.warehouse_id}|${item.material_id}`) ?? [];
}

/** 保存草稿（建单 → `POST`；建完直接打开详情让用户填实盘数） */
async function handleSave(): Promise<void> {
  submitError.value = '';
  const errors = validateStockTakeForm(form);
  formErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    submitError.value = '还有 ' + Object.keys(errors).length + ' 处没填对，请看标红的字段';
    return;
  }
  submitting.value = true;
  try {
    const saved = await createStockTake(buildStockTakePayload(form));
    formVisible.value = false;
    MessagePlugin.success(`已建单 ${saved.take_no}（账面数已快照，共 ${saved.items.length} 行）`);
    await loadOrders(1);
    await loadDetail(saved.id);
  } catch (error) {
    submitError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

/** 列表删除（仅草稿；后端软删） */
function confirmRemove(row: StockTakeRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除盘点单',
    body: `确认删除草稿 ${row.take_no}？删除后不可恢复（仅草稿可删，已过账请走作废）。`,
    theme: 'warning',
    confirmBtn: '确认删除',
    onConfirm: async () => {
      try {
        await deleteStockTake(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadOrders(current.value);
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

// ==================== 详情 / 填实盘数 ====================
const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailWarn = ref('');
const detail = ref<StockTakeDetail | null>(null);
/** 动作失败（409 没配流程 / 边不合法就靠它原样展示） */
const actionError = ref('');
/** 实盘数保存失败 */
const actualError = ref('');
const acting = ref(false);
const savingActuals = ref(false);

/** 实盘数编辑态（明细 id → 输入框文本） */
const actuals = reactive<StockTakeActualState>({});

/** 只有草稿能改实盘数 */
const editable = computed(() => detail.value?.status === 'DRAFT');

/** 状态历史（真实发生过的里程碑） */
const history = computed(() => (detail.value ? statusHistory(detail.value) : []));

/**
 * 盘盈 / 盘亏展示口径。
 *
 * 草稿阶段后端只重算账面 / 实盘合计（`total_gain_qty` 恒 0，等过账才写），
 * 所以这里按明细 `diff_qty` 汇总，并明确标注「按明细差异汇总」—— 不让用户把 0 当真值。
 */
const gainLoss = computed(() =>
  detail.value
    ? gainLossDisplay(detail.value)
    : { gain: '0', loss: '0', fromBackend: false },
);

/** 动作按钮：**完全由后端 `next_statuses` 派生** */
const detailActions = computed<StockTakeActionSpec[]>(() =>
  detail.value ? statusActions(detail.value.status, detail.value.next_statuses) : [],
);

// ==================== 附件必传闸门（ATT-09） ====================

/**
 * 盘点单「提交审批」= `DRAFT → PENDING_POST`（后端的 `CONFIRM` 动作），
 * 此时盘点表 / 盈亏说明必传（规则 `STOCK_TAKE` + `CONFIRM`）。
 *
 * 只有草稿要查：已待过账 / 已过账 / 已作废不会再走这条边。
 */
const attachDocType = ref<DocType>('STOCK_TAKE');
const attachDocId = computed(() => (detail.value?.status === 'DRAFT' ? detail.value.id : null));

const { gate: attachGate, refresh: refreshAttachGate } = useAttachmentGate({
  docType: attachDocType,
  docId: attachDocId,
  at: 'CONFIRM',
});

const userStore = useUserStore();
const hasStatusPermission = computed(() => userStore.hasPermission(PERM.status));
const statusTip = computed(() => (hasStatusPermission.value ? '' : `缺少权限 ${PERM.status}`));

/** 这个动作是否被附件闸门拦住（只有「提交审批」那一条边受约束，见模板注释） */
function actionGateBlocked(action: StockTakeActionSpec): boolean {
  return action.status === 'PENDING_POST' && !attachGate.value.canProceed;
}

/**
 * 动作按钮的 tooltip：只有受闸门约束的动作才有文案（权限优先，其次附件缺口）。
 * 其它动作返回空串 → `:disabled="!tip"` 不挂空 tooltip。
 */
function actionTip(action: StockTakeActionSpec): string {
  if (action.status !== 'PENDING_POST') {
    return '';
  }
  return gateButtonTip(attachGate.value, statusTip.value);
}

/** 上一次动作失败是不是「没配已发布审批流程」（决定要不要摆出「显式跳过」这条出路） */
const errorFlowFlag = ref(false);
const flowNotConfigured = computed(() => errorFlowFlag.value);

async function openDetail(row: StockTakeRow): Promise<void> {
  await loadDetail(row.id);
}

/**
 * 跳到凭证列表并按凭证号过滤（核账链路）。
 *
 * 财务拿「差异凭证」是去凭证列表核账：列表按 `voucher_no` 检索（`?voucher_no=PZ-…` 是
 * 凭证页自己支持的深链，会预填筛选并查一次），所以这里只把号带过去，不复制凭证详情。
 */
function openVoucher(voucherNo: string | null): void {
  if (!voucherNo) {
    return;
  }
  void router.push({ path: '/business/erp/voucher', query: { voucher_no: voucherNo } });
}

function syncActuals(target: StockTakeDetail): void {
  const next = detailToActuals(target);
  for (const key of Object.keys(actuals)) {
    delete actuals[key];
  }
  Object.assign(actuals, next);
}

async function loadDetail(id: string, keepError = false): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detailWarn.value = '';
  if (!keepError) {
    actionError.value = '';
    actualError.value = '';
    errorFlowFlag.value = false;
  }
  try {
    const loaded = await fetchStockTakeDetail(id);
    detail.value = loaded;
    syncActuals(loaded);
    await Promise.all([loadLocationsFor(loaded), loadLocationRowsForDetail(loaded)]);
  } catch (error) {
    detailError.value = errorText(error);
  } finally {
    detailLoading.value = false;
  }
}

async function loadLocationsFor(target: StockTakeDetail): Promise<void> {
  if (!target.warehouse_id) {
    return;
  }
  try {
    locations.value = await fetchLocationList(target.warehouse_id, true);
  } catch {
    /* 拉不到不影响主流程，id 会以短 id 展示 */
  }
}

/** 详情里每行的库位账行也拉一遍，把 id 译成「库位编码（账面 N）」 */
async function loadLocationRowsForDetail(target: StockTakeDetail): Promise<void> {
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
            return { label: `${name}（账面 ${formatQty(row.qty)}）`, value: row.id };
          }),
        );
      } catch {
        locationRowCache.set(key, []);
      }
    }),
  );
}

/** 保存实盘数（`PUT`，仅草稿） */
async function handleSaveActuals(): Promise<void> {
  if (!detail.value) {
    return;
  }
  actualError.value = '';
  const errors = validateActuals(detail.value.items, actuals);
  if (Object.keys(errors).length > 0) {
    actualError.value = Object.values(errors)[0] ?? '实盘数没填对';
    return;
  }
  savingActuals.value = true;
  try {
    const updated = await updateStockTake(detail.value.id, buildActualPayload(detail.value, actuals));
    detail.value = updated;
    syncActuals(updated);
    const agg = aggregateDiff(updated.items);
    MessagePlugin.success(
      agg.diffLines > 0
        ? `实盘数已保存（按明细：盘盈 ${formatQty(agg.gainQty)} / 盘亏 ${formatQty(agg.lossQty)}，共 ${agg.diffLines} 行有差异）`
        : '实盘数已保存（账实相符，无差异）',
    );
    await loadOrders(current.value);
  } catch (error) {
    actualError.value = errorText(error);
  } finally {
    savingActuals.value = false;
  }
}

// ==================== 状态流转 ====================
const confirmVisible = ref(false);
const confirmTarget = ref<'PENDING_POST' | 'POSTED'>('PENDING_POST');
const confirmError = ref('');
const cancelVisible = ref(false);
const cancelReason = ref('');
const cancelError = ref('');

const confirmHeader = computed(() => (confirmTarget.value === 'POSTED' ? '确认过账' : '提交盘点审批'));
const confirmText = computed(() => {
  if (confirmTarget.value === 'POSTED') {
    return `过账后会把 ${detail.value?.take_no ?? ''} 的差异按正负分别走入库 / 出库（三本账一起调平），并生成盘点差异凭证。过账不可撤销，要走回头路只能「作废」（按快照回滚 + 红冲）。`;
  }
  return `提交后 ${detail.value?.take_no ?? ''} 进入审批（流程 stock_take_post）：差异金额 < 10000 走仓管主管一级，≥ 10000 走仓管主管 → 总经理两级。审批通过后还需由持过账权限的人单独过账。`;
});

/** 点动作按钮：作废先要原因，提交 / 过账先确认，撤回直接走 */
function startAction(action: StockTakeActionSpec): void {
  actionError.value = '';
  if (action.status === 'CANCELLED') {
    cancelReason.value = '';
    cancelError.value = '';
    cancelVisible.value = true;
    return;
  }
  if (action.needConfirm && (action.status === 'PENDING_POST' || action.status === 'POSTED')) {
    confirmTarget.value = action.status;
    confirmError.value = '';
    confirmVisible.value = true;
    return;
  }
  void runStatus(action.status, null);
}

function confirmTransition(): void {
  void runStatus(confirmTarget.value, null, true);
}

async function confirmCancel(): Promise<void> {
  const message = validateCancelReason(cancelReason.value, detail.value?.status ?? 'DRAFT');
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
 * - `409 + 66033`：没配已发布流程 → 出「显式跳过审批…」按钮；
 * - `409 + 66001`：边不合法 → 报文里列了当前状态与合法路径；
 * - `403 + 10003`：没有 `business:erp:skip-approval`。
 */
async function runStatus(
  status: 'POSTED' | 'PENDING_POST' | 'DRAFT' | 'CANCELLED',
  reason: string | null,
  fromConfirmDialog = false,
  fromCancelDialog = false,
  extra: { skip_approval?: boolean; skip_reason?: string | null } = {},
): Promise<void> {
  if (!detail.value) {
    return;
  }
  acting.value = true;
  const targetId = detail.value.id;
  try {
    const payload =
      status === 'CANCELLED'
        ? buildCancelPayload(reason ?? '')
        : { status, ...extra };
    const updated = await updateStockTakeStatus(targetId, payload);
    detail.value = updated;
    syncActuals(updated);
    confirmVisible.value = false;
    cancelVisible.value = false;
    skipVisible.value = false;
    errorFlowFlag.value = false;
    // 上一次失败的红条必须清掉：动作成功后还挂着「职责分离 65018」会让人以为这次也失败了
    actionError.value = '';
    confirmError.value = '';
    cancelError.value = '';
    MessagePlugin.success(
      status === 'POSTED'
        ? `已过账，差异金额 ${formatMoney(updated.total_diff_amount)}`
        : status === 'CANCELLED'
          ? '已作废'
          : status === 'PENDING_POST'
            ? '已提交审批（待过账）'
            : '已撤回为草稿',
    );
    await loadOrders(current.value);
  } catch (error) {
    const text = errorText(error);
    errorFlowFlag.value = isFlowNotConfigured(error);
    if (fromConfirmDialog) {
      confirmError.value = text;
    }
    if (fromCancelDialog) {
      cancelError.value = text;
    }
    // 抽屉里的常驻提示：弹窗关掉后用户也能看到为什么失败
    actionError.value = text;
  } finally {
    acting.value = false;
  }
}

// ==================== 显式跳过审批 ====================
const skipVisible = ref(false);
const skipReason = ref('');
const skipError = ref('');

function openSkipDialog(): void {
  skipReason.value = '';
  skipError.value = '';
  skipVisible.value = true;
}

async function confirmSkip(): Promise<void> {
  const message = validateSkipReason(skipReason.value);
  if (message) {
    skipError.value = message;
    return;
  }
  const payload = buildSkipSubmitPayload(skipReason.value);
  await runStatus('PENDING_POST', null, false, false, {
    skip_approval: payload.skip_approval,
    skip_reason: payload.skip_reason,
  });
  if (skipVisible.value && actionError.value) {
    skipError.value = actionError.value;
  }
}

onMounted(() => {
  void loadBasics();
  void loadOrders(1);
});
</script>

<style scoped lang="less">
.stocktake-page {
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

.tool-row__keyword {
  width: 200px;
}

/* ==================== 列表 ==================== */

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert {
  margin-bottom: 12px;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  white-space: nowrap;
}

.op-cell > * {
  white-space: nowrap;
  flex: 0 0 auto;
}

.panel-empty {
  padding: 40px 0;
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

.dlg-alert {
  margin-bottom: 12px;
}

.form {
  margin-bottom: 8px;
}

.field-hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.section-title {
  display: flex;
  align-items: baseline;
  gap: 8px;
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

.items-alert {
  margin-bottom: 8px;
}

.items {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.items__head,
.items__row {
  display: grid;
  grid-template-columns: minmax(220px, 2fr) minmax(200px, 1.4fr) 64px;
  gap: 12px;
  align-items: center;
}

.items__head {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  padding-left: 2px;
}

.items__error {
  display: block;
  margin-top: 2px;
  font-size: 12px;
  color: var(--td-error-color);
}

.items__col--op {
  text-align: right;
}

.items__add {
  margin-top: 10px;
}

.drawer-footer,
.dialog-footer {
  display: flex;
  gap: 12px;
  margin-top: 20px;
}

/* ==================== 详情 ==================== */

.drawer-loading {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-secondary);
}

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.detail__no {
  font-size: 16px;
  font-weight: 600;
}

.detail__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* PDF-08：把「下载 PDF」推到右上角 */
.detail__spacer {
  margin-left: auto;
}

.detail__alert {
  margin-bottom: 12px;
}

.detail__alert-text {
  word-break: break-word;
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 24px;
  padding: 12px 0;
  border-top: 1px solid var(--td-component-stroke);
  border-bottom: 1px solid var(--td-component-stroke);
}

.detail-field {
  display: flex;
  gap: 8px;
  font-size: 13px;
  min-width: 0;
}

.detail-field--wide {
  grid-column: 1 / -1;
}

.detail-field__label {
  flex: 0 0 92px;
  color: var(--td-text-color-secondary);
}

.detail-field__value {
  flex: 1 1 auto;
  color: var(--td-text-color-primary);
  word-break: break-word;
}

.detail-field__note {
  margin-left: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.history {
  margin: 0;
  padding: 0;
  list-style: none;
}

.history__item {
  display: flex;
  align-items: baseline;
  gap: 8px;
  font-size: 13px;
  padding: 4px 0;
}

.history__dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--td-brand-color);
  flex: 0 0 auto;
}

.history__label {
  color: var(--td-text-color-primary);
}

.history__time {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.history__note {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.actual-input {
  width: 120px;
  margin-left: auto;
}

.detail__save-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 12px;
}

.detail__save-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.detail__actions {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-top: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-stroke);
}

.detail__actions-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ATT-09：disabled 的 t-button 收不到 hover —— tooltip 必须包一层普通元素才能对灰按钮生效 */
.op-wrap {
  display: inline-flex;
}

/* ==================== 通用小件 ==================== */

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-secondary);
}

.num {
  font-variant-numeric: tabular-nums;
}

.money {
  font-variant-numeric: tabular-nums;
}

.money--pending {
  color: var(--td-text-color-placeholder);
}

.gain {
  color: var(--td-success-color);
}

.loss {
  color: var(--td-error-color);
}

.dialog-text {
  margin: 0 0 12px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--td-text-color-primary);
}

.dialog-error {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
  word-break: break-word;
}
</style>
