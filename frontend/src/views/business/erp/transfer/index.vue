<template>
  <div class="transfer-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-transfer-create"
          @click="openCreateDialog"
        >
          <template #icon><t-icon name="add" /></template>
          新建调拨单
        </t-button>
        <t-button variant="outline" data-testid="btn-transfer-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <!-- 超期在途开关（LNK-F12）：打开 = 只看超期在途，同时锁定状态下拉（后端 400 的矛盾组合） -->
        <t-tooltip content="只看「已发货但超过阈值天数仍未收货」的单；打开后状态筛选锁定为在途">
          <t-checkbox
            v-model="filter.in_transit_overdue"
            class="tool-row__overdue"
            data-testid="transfer-filter-overdue"
            @change="handleOverdueChange"
          >
            超期在途
          </t-checkbox>
        </t-tooltip>

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          :placeholder="statusFilterLocked ? '在途（已锁定）' : '全部状态'"
          :disabled="statusFilterLocked"
          clearable
          data-testid="transfer-filter-status"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.transfer_no"
          class="tool-row__no"
          placeholder="调拨单号（模糊）"
          clearable
          data-testid="transfer-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-transfer-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-transfer-reset" @click="handleReset">重置</t-button>
      </div>
      <!-- O4 列表记忆：恢复筛选（含「超期在途」开关）必须让用户看得见 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="transfer-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">调拨单</span>
          <span class="panel-head__meta" data-testid="transfer-meta">共 {{ total }} 条</span>
          <!-- 页头角标：超期在途总量（summary 接口；点一下直接筛超期） -->
          <t-tag
            v-if="overdueHeadText"
            theme="danger"
            variant="light"
            size="small"
            class="panel-head__overdue"
            data-testid="transfer-overdue-badge"
            @click="turnOnOverdueFilter"
          >
            {{ overdueHeadText }}
          </t-tag>
        </div>
        <span class="panel-head__tip">点任意一行看详情与审批进度</span>
      </div>

      <!-- 接口未就绪（W3b 联调前）：不是红错，是「这条路还没通车」 -->
      <t-alert v-if="apiMissing" theme="info" class="list-alert" data-testid="transfer-api-missing">
        <template #message>
          调拨接口尚未上线（{{ apiMissing }}）：页面结构与交互已就绪，等 W3b 后端合并后即可联调。
        </template>
      </t-alert>

      <t-alert v-else-if="loadError" theme="error" :message="loadError" class="list-alert" data-testid="transfer-load-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadTransfers()">重试</t-button>
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
        testid="transfer-batch"
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
        data-testid="transfer-table"
        @select-change="onSelectChange"
        @row-click="handleRowClick"
        @page-change="handlePageChange"
      >
        <template #transfer_no="{ row }">
          <span class="mono" :data-testid="`transfer-no-${row.transfer_no}`">{{ row.transfer_no }}</span>
        </template>

        <template #from_warehouse_id="{ row }">
          {{ warehouseNameOf(row.from_warehouse_id) }}
        </template>

        <template #to_warehouse_id="{ row }">
          {{ warehouseNameOf(row.to_warehouse_id) }}
        </template>

        <template #status="{ row }">
          <div class="status-cell">
            <t-tag
              size="small"
              :theme="transferStatusTheme(row.status)"
              :variant="transferStatusVariant(row.status)"
              :data-testid="`transfer-status-${row.transfer_no}`"
            >
              {{ transferStatusLabel(row.status) }}
            </t-tag>
            <!-- 超期在途红标：status=IN_TRANSIT 且后端判定 in_transit_overdue（LNK-F12） -->
            <t-tooltip v-if="overdueTextOf(row)" :content="overdueTipOf(row)">
              <span
                class="overdue-mark"
                :data-testid="`transfer-overdue-${row.transfer_no}`"
              >
                {{ overdueTextOf(row) }}
              </span>
            </t-tooltip>
          </div>
        </template>

        <template #item_count="{ row }">
          <span class="muted">{{ itemCountText(row) }}</span>
        </template>

        <template #transfer_date="{ row }">
          <span class="muted">{{ formatTime(row.transfer_date) }}</span>
        </template>

        <template #applicant_name="{ row }">
          {{ dash(row.applicant_name) }}
        </template>

        <template #op="{ row }">
          <div class="op-cell" @click.stop>
            <t-link theme="primary" :data-testid="`btn-view-${row.transfer_no}`" @click="openDetail(row)">
              查看
            </t-link>

            <t-link
              v-if="actionsOf(row).edit"
              v-privilege="[PERM.update]"
              theme="primary"
              :data-testid="`btn-edit-${row.transfer_no}`"
              @click="openEditDialog(row)"
            >
              编辑
            </t-link>

            <t-link
              v-if="actionsOf(row).submit"
              v-privilege="[PERM.status]"
              theme="primary"
              :data-testid="`btn-submit-${row.transfer_no}`"
              @click="runAction(row, 'submit')"
            >
              提交审批
            </t-link>

            <t-link
              v-if="actionsOf(row).withdraw"
              v-privilege="[PERM.status]"
              theme="primary"
              :data-testid="`btn-withdraw-${row.transfer_no}`"
              @click="runAction(row, 'withdraw')"
            >
              撤回
            </t-link>

            <t-link
              v-if="actionsOf(row).ship"
              v-privilege="[PERM.status]"
              theme="primary"
              :data-testid="`btn-ship-${row.transfer_no}`"
              @click="openShipDialog(row)"
            >
              发货
            </t-link>

            <t-link
              v-if="actionsOf(row).receive"
              v-privilege="[PERM.status]"
              theme="primary"
              :data-testid="`btn-receive-${row.transfer_no}`"
              @click="runAction(row, 'receive')"
            >
              收货
            </t-link>

            <t-link
              v-if="actionsOf(row).remove"
              v-privilege="[PERM.remove]"
              theme="danger"
              :data-testid="`btn-remove-${row.transfer_no}`"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="transfer-empty">
            <t-icon class="panel-empty__icon" name="swap" />
            <p class="panel-empty__title">
              {{ filter.in_transit_overdue ? '没有超期的在途单' : (filtered ? '没有匹配的调拨单' : '还没有调拨单') }}
            </p>
            <p class="panel-empty__desc">
              {{ filter.in_transit_overdue
                ? '已发货的单都在阈值天数内：发货超过阈值仍未收货才会出现在这里'
                : (filtered ? '换个状态或单号，或点「重置」看全部' : '点左上角「新建调拨单」建第一张：选调出仓 / 调入仓 → 加明细 → 保存草稿') }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="720px"
      header="调拨单详情"
      data-testid="transfer-detail-drawer"
    >
      <template #footer>
        <!-- TDesign 的 drawer 默认只靠遮罩关闭，实测 Esc 不回焦不生效 → 给一个明确的关闭按钮 -->
        <div class="drawer-footer">
          <!-- D9：复制为新单（原单只读，后端建新草稿；权限 = 建单权限） -->
          <t-button
            v-privilege="[PERM.create]"
            theme="primary"
            variant="outline"
            :loading="copying"
            :disabled="copying || !detail"
            data-testid="btn-transfer-copy"
            @click="copyCurrent"
          >
            复制为新单
          </t-button>
          <t-button variant="outline" data-testid="btn-drawer-close" @click="detailVisible = false">关闭</t-button>
        </div>
      </template>

      <div v-if="detailLoading" class="drawer-loading">
        <t-loading size="small" /> 加载中…
      </div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />

      <div v-else-if="detail" class="detail">
        <!--
          R-P1-5：关键动作固定在抽屉顶部（吸顶条，翻到底也看得见）。
          用户原话「操作员操作的时候不能满屏去找按钮」—— 这 4 个动作原来分散在列表行上，
          点进详情就一个也看不到。
        -->
        <DocActionBar title="关键操作" testid="transfer-action-bar">
          <t-button
            size="small"
            theme="primary"
            :disabled="!actionsOf(detail).submit"
            data-testid="btn-transfer-detail-submit"
            @click="runAction(detail, 'submit')"
          >
            <template #icon><t-icon name="send" /></template>
            提交审批
          </t-button>
          <t-button
            size="small"
            theme="primary"
            variant="outline"
            :disabled="!actionsOf(detail).ship"
            data-testid="btn-transfer-detail-ship"
            @click="openShipDialog(detail)"
          >
            <template #icon><t-icon name="vehicle" /></template>
            发货
          </t-button>
          <t-button
            size="small"
            theme="success"
            variant="outline"
            :disabled="!actionsOf(detail).receive"
            data-testid="btn-transfer-detail-receive"
            @click="runAction(detail, 'receive')"
          >
            <template #icon><t-icon name="check-circle" /></template>
            完成（收货）
          </t-button>
          <t-button
            size="small"
            theme="danger"
            variant="outline"
            :disabled="!actionsOf(detail).withdraw"
            data-testid="btn-transfer-detail-cancel"
            @click="openWithdrawDialog(detail)"
          >
            <template #icon><t-icon name="delete" /></template>
            作废
          </t-button>
        </DocActionBar>
        <div class="detail__head">
          <span class="detail__no mono">{{ detail.transfer_no }}</span>
          <t-tag :theme="transferStatusTheme(detail.status)" :variant="transferStatusVariant(detail.status)" size="small">
            {{ transferStatusLabel(detail.status) }}
          </t-tag>
          <t-tooltip v-if="overdueTextOf(detail)" :content="overdueTipOf(detail)">
            <span class="overdue-mark" data-testid="transfer-detail-overdue">{{ overdueTextOf(detail) }}</span>
          </t-tooltip>
        </div>

        <div class="detail__grid">
          <div class="detail-field"><span class="detail-field__label">调出仓</span><span class="detail-field__value">{{ warehouseNameOf(detail.from_warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">调入仓</span><span class="detail-field__value">{{ warehouseNameOf(detail.to_warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">在途仓</span><span class="detail-field__value">{{ warehouseNameOf(detail.transit_warehouse_id) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">调拨日期</span><span class="detail-field__value">{{ formatTime(detail.transfer_date) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">申请人</span><span class="detail-field__value">{{ dash(detail.applicant_name) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">合计数量</span><span class="detail-field__value">{{ formatQty(detail.total_qty) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">合计金额</span><span class="detail-field__value">{{ formatQty(detail.total_amount) }}</span></div>
          <div class="detail-field"><span class="detail-field__label">原因</span><span class="detail-field__value">{{ dash(detail.reason) }}</span></div>
          <div v-if="overdueTextOf(detail)" class="detail-field">
            <span class="detail-field__label">超期</span>
            <span class="detail-field__value overdue-mark--text" data-testid="transfer-detail-overdue-desc">{{ overdueTipOf(detail) }}</span>
          </div>
        </div>

        <!-- 状态时间戳：状态机的痕迹（V0140 用 CHECK 把它们和状态绑死） -->
        <ul class="stamp-list">
          <li v-for="stamp in stampsOf(detail)" :key="stamp.label" class="stamp">
            <span class="stamp__label">{{ stamp.label }}</span>
            <span class="stamp__value">{{ formatTime(stamp.at) }}</span>
          </li>
          <li v-if="detail.cancel_reason" class="stamp">
            <span class="stamp__label">取消原因</span>
            <span class="stamp__value">{{ detail.cancel_reason }}</span>
          </li>
        </ul>

        <div class="section-title">明细（{{ detail.items.length }} 条）</div>
        <t-table
          row-key="id"
          size="small"
          :data="detail.items"
          :columns="itemColumns"
          data-testid="transfer-detail-items"
        >
          <template #material_id="{ row }">{{ materialNameOf(row.material_id) }}</template>
          <template #qty="{ row }">{{ formatQty(row.qty) }}</template>
          <template #serials="{ row }">
            <span v-if="(row.serials ?? []).length === 0" class="muted">—</span>
            <t-tooltip v-else :content="serialSummary(row.serials, 99)">
              <span class="mono">{{ serialSummary(row.serials) }}</span>
            </t-tooltip>
          </template>
          <template #from_location_id="{ row }">
            <span class="mono muted">{{ shortId(row.from_location_id) }}</span>
          </template>
        </t-table>

        <!--
          审批进度（C2：换掉本页自绘时间线，改用全站 `FlowProgress`）。
          `approval_instance_id` 是弱引用，有才查；节点 + 审批人明细（谁 / 时间 / 意见 /
          跳过说明）都来自同一个 `GET /business/flow/instance/{id}`，与采购 / 收货 /
          应付 / 凭证长得一模一样。
          ⚠ 自绘时间线原来还有两条**实例级**信息（提交人 + 提交时间），`FlowProgress`
          只画节点、不含它们，所以单独一行文字补回来（`buildApprovalSteps` 第一步，
          文案与老时间线一致）；「完成 / 驳回」这类终态由 END 节点的状态文案覆盖。
        -->
        <div class="section-title">审批进度</div>
        <div v-if="!detail.approval_instance_id" class="approval-empty" data-testid="transfer-approval-none">
          未提交审批（草稿状态没有审批实例；提交后这里会显示审批进度）
        </div>
        <div v-else-if="approvalLoading" class="drawer-loading"><t-loading size="small" /> 审批进度加载中…</div>
        <t-alert v-else-if="approvalError" theme="warning" :message="approvalError" />
        <template v-else>
          <div v-if="approvalSubmitText" class="approval-submit" data-testid="transfer-approval-submit">
            {{ approvalSubmitText }}
          </div>
          <FlowProgress
            :nodes="approvalProgress.nodes"
            :tasks="approvalProgress.tasks"
            data-testid="transfer-approval-progress"
          />
        </template>

        <!-- ATT-11：调拨审批单（建议传，不校验）—— 已保存单据才有 doc id -->
        <div class="section-title">附件（调拨审批单）</div>
        <DocAttachmentPanel
          :doc-type="DOC_KIND.STOCK_TRANSFER"
          :doc-id="detail.id"
          :default-role="DEFAULT_ROLE_BY_DOC.STOCK_TRANSFER"
          hint="调拨审批单 / 双方签字单；不传也能提交与收发货"
          testid-prefix="transfer-attach"
        />
      </div>
    </t-drawer>

    <!-- ==================== ④ 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="form.id ? '编辑调拨单' : '新建调拨单'"
      width="860px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="transfer-form-dialog"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <!-- D9：复制提示（序列号不复制 —— 实物号码跟着原单走） -->
      <t-alert v-if="copyNotice" theme="warning" class="dlg-alert" data-testid="transfer-copy-notice">
        {{ copyNotice }}
      </t-alert>

      <t-form :data="form" label-width="84px">
        <div class="form-row">
          <t-form-item
            label="调出仓"
            name="from_warehouse_id"
            class="form-row__col"
            :status="formErrors.from_warehouse_id ? 'error' : undefined"
            :tips="formErrors.from_warehouse_id"
          >
            <t-select
              v-model="form.from_warehouse_id"
              :options="warehouseOptions"
              placeholder="选择调出仓"
              filterable
              data-testid="transfer-from-warehouse"
            />
          </t-form-item>

          <t-form-item
            label="调入仓"
            name="to_warehouse_id"
            class="form-row__col"
            :status="formErrors.to_warehouse_id ? 'error' : undefined"
            :tips="formErrors.to_warehouse_id"
          >
            <t-select
              v-model="form.to_warehouse_id"
              :options="toWarehouseOptions"
              placeholder="选择调入仓（不能与调出仓相同）"
              filterable
              data-testid="transfer-to-warehouse"
            />
          </t-form-item>
        </div>

        <div class="form-row">
          <t-form-item
            label="调拨日期"
            name="transfer_date"
            class="form-row__col"
            :status="formErrors.transfer_date ? 'error' : undefined"
            :tips="formErrors.transfer_date"
          >
            <t-date-picker
              v-model="form.transfer_date"
              value-type="YYYY-MM-DD"
              placeholder="选择日期"
              data-testid="transfer-date"
            />
          </t-form-item>

          <t-form-item
            label="原因"
            name="reason"
            class="form-row__col"
            :status="formErrors.reason ? 'error' : undefined"
            :tips="formErrors.reason"
          >
            <t-input v-model="form.reason" :maxlength="255" placeholder="如：分仓补货" data-testid="transfer-reason" />
          </t-form-item>
        </div>

        <!-- 明细：物料 / 数量 / 序列号 / 删除 -->
        <div class="items">
          <div class="items__head">
            <span class="items__title">明细</span>
            <!-- R-P1-5：从库存直接选 —— 作业员不用另开库存页抄物料 / 库位 / 数量 -->
            <t-button
              size="small"
              theme="primary"
              variant="outline"
              class="items__pick"
              data-testid="btn-transfer-pick-stock"
              @click="openStockPicker"
            >
              <template #icon><t-icon name="swap" /></template>
              从库存选择
            </t-button>
            <span class="items__hint">一物一码物料必须逐个选序列号（个数 == 数量）</span>
          </div>

          <div class="items__grid items__grid--head">
            <span>物料</span>
            <span>数量</span>
            <span>序列号</span>
            <span />
          </div>

          <div v-for="(item, index) in form.items" :key="item.key" class="items__row" :data-testid="`transfer-item-${index}`">
            <div class="items__grid">
              <div class="items__cell">
                <t-select
                  v-model="item.material_id"
                  :options="materialOptions"
                  placeholder="选择物料"
                  filterable
                  :data-testid="`item-material-${index}`"
                  @change="(value: unknown) => handleMaterialChange(item, value)"
                />
                <p v-if="rowErrors[item.key]?.material_id" class="cell-error">{{ rowErrors[item.key]?.material_id }}</p>
              </div>

              <div class="items__cell">
                <!-- 小数位只在「一物一码」时锁 0（否则 2 会显示成 2.0000，作业员以为填错了）；
                     非一物一码不设 `decimal-places`，自由输入由 validateTransferForm 兜 4 位上限 -->
                <t-input-number
                  v-model="item.qty"
                  :min="0"
                  :max="9999999"
                  :decimal-places="serialRequiredOf(item.material_id) ? 0 : undefined"
                  theme="normal"
                  :data-testid="`item-qty-${index}`"
                  @change="handleQtyChange(item)"
                />
                <p v-if="rowErrors[item.key]?.qty" class="cell-error">{{ rowErrors[item.key]?.qty }}</p>
              </div>

              <div class="items__cell">
                <div v-if="serialRequiredOf(item.material_id)" class="serial-cell">
                  <t-button
                    size="small"
                    variant="outline"
                    :data-testid="`item-serials-${index}`"
                    @click="openSerialPicker(index)"
                  >
                    选择序列号（{{ item.serial_ids.length }}）
                  </t-button>
                  <t-tag
                    v-if="item.serial_ids.length > 0 && item.serial_ids.length === Number(item.qty)"
                    size="small"
                    theme="success"
                    variant="light"
                  >
                    数量已对齐
                  </t-tag>
                  <t-tag v-else-if="item.serial_ids.length > 0" size="small" theme="warning" variant="light">
                    {{ item.serial_ids.length }}/{{ item.qty }}
                  </t-tag>
                </div>
                <span v-else class="muted">非一物一码，只填数量</span>
                <p v-if="rowErrors[item.key]?.serial_ids" class="cell-error">{{ rowErrors[item.key]?.serial_ids }}</p>
              </div>

              <div class="items__cell items__cell--op">
                <t-link
                  theme="danger"
                  :disabled="form.items.length <= 1"
                  :data-testid="`item-remove-${index}`"
                  @click="removeItem(index)"
                >
                  删除
                </t-link>
              </div>
            </div>
          </div>

          <p v-if="formErrors.items" class="cell-error">{{ formErrors.items }}</p>

          <t-button variant="dashed" class="items__add" data-testid="btn-add-item" @click="addItem">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
        </div>
      </t-form>

      <!-- O1 表单记忆提示：带出了上次的调出仓 / 调入仓就明说 -->
      <FormMemoryHint
        v-if="!form.id"
        :labels="memoryLabels"
        testid="transfer-form-memory"
        @reset="handleResetMemory"
      />

      <div class="dlg-footer">
        <t-button variant="outline" :disabled="formSaving" @click="formVisible = false">取消</t-button>
        <t-button
          variant="outline"
          theme="primary"
          :loading="formSaving && savingMode === 'draft'"
          :disabled="formSaving"
          data-testid="btn-save-draft"
          @click="submitForm('draft')"
        >
          保存草稿
        </t-button>
        <t-button
          theme="primary"
          :loading="formSaving && savingMode === 'submit'"
          :disabled="formSaving"
          data-testid="btn-save-submit"
          @click="submitForm('submit')"
        >
          提交审批
        </t-button>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 序列号选择弹窗 ==================== -->
    <t-dialog
      v-model:visible="serialPickerVisible"
      header="选择序列号"
      width="620px"
      :confirm-btn="{ content: '确定', loading: serialLoading }"
      data-testid="transfer-serial-dialog"
      @confirm="confirmSerialPick"
    >
      <div class="serial-dialog">
        <div class="serial-dialog__bar">
          <span class="serial-dialog__hint">
            来自「{{ warehouseNameOf(form.from_warehouse_id) }}」的在库序列号，需要
            {{ serialPickerNeed }} 个，已选 <b>{{ serialPickerSelected.length }}</b> 个
          </span>
          <span class="tool-row__spacer" />
          <t-button size="small" variant="outline" :disabled="serialOptions.length === 0" @click="selectAllSerials">
            按数量全选
          </t-button>
          <t-button size="small" variant="outline" @click="serialPickerSelected = []">清空</t-button>
        </div>

        <t-alert v-if="!form.from_warehouse_id" theme="warning" message="先选调出仓：序列号按「物料 × 仓库」过滤" />
        <t-alert v-else-if="serialError" theme="error" :message="serialError" />
        <div v-else-if="serialOptions.length === 0 && !serialLoading" class="serial-dialog__empty">
          该物料在「{{ warehouseNameOf(form.from_warehouse_id) }}」没有在库序列号 —— 先去序列号页批量生成
        </div>

        <t-checkbox-group v-else v-model="serialPickerSelected" class="serial-list">
          <t-checkbox v-for="serial in serialOptions" :key="serial.id" :value="serial.id" class="serial-list__item">
            <span class="mono">{{ serial.serial_no }}</span>
          </t-checkbox>
        </t-checkbox-group>

        <p v-if="serialTruncated" class="serial-dialog__warn">
          只列出前 {{ SERIAL_OPTION_LIMIT }} 个在库序列号，数量更大请先在序列号页确认库存
        </p>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 发货弹窗（不可逆，要选在途仓） ==================== -->
    <t-dialog
      v-model:visible="shipVisible"
      header="发货确认"
      theme="warning"
      width="520px"
      :confirm-btn="{ content: '确认发货', loading: shipping, theme: 'warning' }"
      data-testid="transfer-ship-dialog"
      @confirm="confirmShip"
    >
      <p class="ship-text">
        单号 <span class="mono">{{ shipRow?.transfer_no }}</span> 将从「已批准」变为「在途」：货离开调出仓进入在途仓，
        <b>发货后不可撤回</b>，只能等收货。
      </p>
      <t-form label-width="84px">
        <t-form-item label="在途仓">
          <t-select
            v-model="shipTransitWarehouseId"
            :options="transitWarehouseOptions"
            placeholder="默认取系统在途仓"
            clearable
            filterable
            data-testid="transfer-transit-warehouse"
          />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== ⑦ 撤回弹窗（PENDING → CANCELLED；取消原因必填） ==================== -->
    <t-dialog
      v-model:visible="withdrawVisible"
      header="撤回调拨单"
      theme="warning"
      width="520px"
      :confirm-btn="{ content: '确认撤回', loading: withdrawing, theme: 'warning' }"
      data-testid="transfer-withdraw-dialog"
      @confirm="confirmWithdraw"
    >
      <p class="ship-text">
        单号 <span class="mono">{{ withdrawRow?.transfer_no }}</span> 将从「审批中」变为「已取消」：
        后端会把在跑的审批单一并终止，<b>撤回后不可恢复</b>（只能删除，不能再提交）。
      </p>
      <t-form label-width="84px">
        <t-form-item label="撤回原因" required-mark>
          <t-input
            v-model="withdrawReason"
            placeholder="例如：需求取消 / 数量填错"
            :maxlength="255"
            data-testid="transfer-withdraw-reason"
          />
        </t-form-item>
      </t-form>
      <p v-if="withdrawError" class="ship-text" data-testid="transfer-withdraw-error">{{ withdrawError }}</p>
    </t-dialog>

    <!-- ==================== ⑧ 从库存选择（R-P1-5：物料 / 库位 / 可用量 / 成本价 直接带出） ==================== -->
    <StockPickerDialog
      v-model:visible="stockPickerVisible"
      :warehouse-id="form.from_warehouse_id"
      :warehouse-name="warehouseNameOf(form.from_warehouse_id)"
      :materials="materials"
      @confirm="applyStockPicks"
    />
  </div>
</template>

<script setup lang="ts">
import {
  transferBatchStatus,
} from '@/api/business/batch';
import BatchActionBar from '@/components/BatchActionBar.vue';
import { useBatchSelection } from '@/composables/useBatchSelection';
import { useUserStore } from '@/stores/user';
import type { BatchActionSpec, BatchExportTask, BatchResult, BatchRow } from '@/utils/batch-action';
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  createTransfer,
  deleteTransfer,
  copyTransfer,
  fetchMaterialPage,
  fetchSerialPage,
  fetchTransferDetail,
  fetchTransferOverdueSummary,
  fetchTransferPage,
  fetchWarehousePage,
  updateTransfer,
  updateTransferStatus,
  type MaterialRow,
  type SerialRow,
  type TransferDetail,
  type TransferOverdueSummary,
  type TransferRow,
  type TransferStatus,
  type Warehouse,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import FormMemoryHint from '@/components/FormMemoryHint.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { pickFields, resolveDefaults } from '@/composables/memory-core';
import { useFormMemory } from '@/composables/useFormMemory';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import { getFlowInstance, type FlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import {
  EMPTY_APPROVAL_PROGRESS,
  approvalProgressOf,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import DocActionBar from '@/views/business/erp/shared/DocActionBar.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import StockPickerDialog from '@/views/business/erp/shared/StockPickerDialog.vue';
import type { StockPick } from '@/views/business/erp/shared/stock-pick';
import {
  OVERDUE_DEFAULT_THRESHOLD_DAYS,
  SERIAL_OPTION_LIMIT,
  TRANSFER_ACTION_LABEL,
  TRANSFER_PAGE_SIZE,
  TRANSFER_STATUS_FILTER_OPTIONS,
  addTransferItem,
  buildApprovalSteps,
  buildTransferPayload,
  buildTransferQuery,
  dash,
  detailToForm,
  emptyTransferFilter,
  emptyTransferForm,
  formatQty,
  formatTime,
  hasTransferFormError,
  isStatusFilterLocked,
  isTransferFiltered,
  itemCountText,
  mergeStockPicks,
  overdueBadgeText,
  overdueBadgeTooltip,
  overdueSummaryText,
  shortId,
  serialSummary,
  transferActionHint,
  transferCopyNotice,
  transferNoFromQuery,
  transferStatusLabel,
  transferStatusTheme,
  transferStatusVariant,
  availableActions,
  validateTransferForm,
  withdrawReasonError,
  type TransferAction,
  type TransferFilterState,
  type TransferFormErrors,
  type TransferFormState,
  type TransferItemForm,
} from './transfer-logic';

/** 本页权限码（与 transfer 后端权限种子同口径；`SUPER_ADMIN` 走 `*:*:*` 短路） */
const PERM = {
  /** 列表（页面入口） */
  list: 'business:erp:transfer:list',
  /** 新建 */
  create: 'business:erp:transfer:create',
  /** 改单 */
  update: 'business:erp:transfer:update',
  /** 状态流转 */
  status: 'business:erp:transfer:status',
  /** 删除 */
  remove: 'business:erp:transfer:delete',
} as const;

/** 表格列（`colKey` 与 `TransferRow` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  // D7：勾选列（没有任何可用批量动作的行，复选框直接禁用）
  {
    colKey: 'row-select',
    type: 'multiple',
    width: 50,
    disabled: (options: { row: unknown }) => !isSelectable(options.row as TransferRow),
  },
  { colKey: 'transfer_no', title: '调拨单号', width: 190, fixed: 'left' },
  { colKey: 'from_warehouse_id', title: '调出仓', width: 150, ellipsis: true },
  { colKey: 'to_warehouse_id', title: '调入仓', width: 150, ellipsis: true },
  { colKey: 'status', title: '状态', width: 150 },
  { colKey: 'item_count', title: '明细数', width: 88, align: 'right' },
  { colKey: 'transfer_date', title: '调拨日期', width: 120 },
  { colKey: 'applicant_name', title: '申请人', width: 110, ellipsis: true },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right' },
];

/** 详情抽屉里的明细列 */
const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 50, align: 'center' },
  { colKey: 'material_id', title: '物料', minWidth: 170, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 90, align: 'right' },
  { colKey: 'serials', title: '序列号', minWidth: 220, ellipsis: true },
  { colKey: 'from_location_id', title: '调出库位', width: 120 },
];

/** 状态下拉（`''` = 全部） */
const statusFilterOptions = TRANSFER_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

// ==================== 记忆字段（O1） ====================

/**
 * 表单记忆字段：调出仓 / 调入仓。
 *
 * 「调拨」这类操作在仓库间来来回回就那么几条线（WH01→WH02 天天有），
 * 每次重选两遍下拉纯浪费；卡面另提的 `unit_id` / `location_id`
 * 在本仓调拨单明细里不存在（明细只有物料 / 数量 / 序列号），所以不硬造。
 */
const MEMORY_FIELDS: readonly (keyof TransferFormState)[] = ['from_warehouse_id', 'to_warehouse_id'];

/** 记忆字段中文名 */
const MEMORY_LABELS: Record<string, string> = {
  from_warehouse_id: '调出仓',
  to_warehouse_id: '调入仓',
};

// ==================== 列表 / 筛选 ====================

/** 路由（`?transfer_no=` 深链要在首屏列表之后消费） */
const route = useRoute();
const router = useRouter();
/** 筛选行 */
const filter = reactive<TransferFilterState>(emptyTransferFilter());
/** 行数据 */
const rows = ref<TransferRow[]>([]);
/** 总条数 */
const total = ref(0);

// ==================== 批量操作（D7） ====================

/** 当前用户（批量动作的权限判定）。 */
const userStore = useUserStore();

/** 一次「选中全部筛选结果」拉多少主键（后端单页上限 200）。 */
const BATCH_FETCH_PAGE_SIZE = 200;

/**
 * 批量动作声明。
 *
 * `allowedStatus` 与后端状态机**逐字一致**：不符合的行由组件禁用按钮并说明原因，
 * 高危动作（作废 / 删除）走二次确认 + 必填原因。
 */
const BATCH_ACTIONS: BatchActionSpec[] = [
  {
    key: 'submit',
    label: '批量提交审批',
    theme: 'primary',
    allowedStatus: ['DRAFT'],
    targetStatus: 'PENDING',
  },
  {
    key: 'cancel',
    label: '批量作废',
    theme: 'danger',
    allowedStatus: ['DRAFT', 'PENDING'],
    targetStatus: 'CANCELLED',
    needConfirm: true,
    requireReason: true,
  },
];

/** 动作 → 权限码（与单条按钮同一套码；没有码的按钮禁用并说明）。 */
const BATCH_PERMISSIONS: Record<string, string> = {
  submit: 'business:erp:transfer:status',
  cancel: 'business:erp:transfer:status',
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
  action: BatchActionSpec,
  ids: string[],
  payload: { reason?: string },
): Promise<BatchResult | BatchExportTask> {
  return transferBatchStatus({ ids, status: action.targetStatus, reason: payload.reason });
}

/** 「选中全部 N 条筛选结果」：按当前筛选把全部主键拉回来（最多 1 万条）。 */
async function fetchAllRowIds(): Promise<BatchRow[]> {
  const acc: BatchRow[] = [];
  for (let page = 1; page <= 50; page += 1) {
    const data = await fetchTransferPage(buildTransferQuery(filter, page, BATCH_FETCH_PAGE_SIZE));
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
  void loadTransfers(1);
}
/** 当前页 */
const current = ref(1);
/** 每页条数 */
const pageSize = TRANSFER_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');
/** 接口未就绪的说明（W3b 合并前：404 不是故障） */
const apiMissing = ref('');

/**
 * 列表记忆（O4）：筛选（含「超期在途」开关）+ 页码。
 *
 * 构造即恢复：开关状态也一起回来 —— 否则「打开超期在途 → 进详情 → 回来」开关就掉了，
 * 而列表还记得状态筛选，会出现前后不一致。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<TransferFilterState>({
  listName: 'erp-transfer',
  filter,
  page: current,
  pageSize,
});

/** 表单记忆（O1）：调出仓 / 调入仓；优先级 源单 > 记忆 > 默认 */
const formMemory = useFormMemory<TransferFormState>('erp-transfer', MEMORY_FIELDS, {
  labels: MEMORY_LABELS,
});
/** 本次新建带出的记忆字段中文名（空 = 不显示提示条） */
const memoryLabels = ref<string[]>([]);
/** 物料主数据（译 material_id + 下拉） */
const materials = ref<MaterialRow[]>([]);
/** 仓库主数据（译 warehouse_id + 下拉） */
const warehouses = ref<Warehouse[]>([]);

/** 是否处于筛选态（空态文案分「没数据」「没匹配」） */
const filtered = computed(() => isTransferFiltered(filter));

/** 状态下拉是否锁定：超期开关打开时锁死（`in_transit_overdue=true` + `status≠IN_TRANSIT` 后端 400） */
const statusFilterLocked = computed(() => isStatusFilterLocked(filter));

/** 超期在途汇总（页头角标；接口拿不到就是 null，页面照常用） */
const overdueSummary = ref<TransferOverdueSummary | null>(null);

/** 角标文案（`total = 0` / 接口没通 → 空串，红色角标不挂） */
const overdueHeadText = computed(() => overdueSummaryText(overdueSummary.value));

/** tooltip 里用的阈值（角标拿不到就按后端缺省 7 天，同口径） */
const overdueThresholdDays = computed(() => overdueSummary.value?.threshold_days ?? OVERDUE_DEFAULT_THRESHOLD_DAYS);

/** 行内超期红标文案（`''` = 不显示） */
function overdueTextOf(row: Pick<TransferRow, 'status' | 'in_transit_overdue' | 'overdue_days'>): string {
  return overdueBadgeText(row);
}

/** 行内超期红标 hover 文案（告警台账：已于 X 提醒过 N 次） */
function overdueTipOf(row: TransferRow): string {
  return overdueBadgeTooltip(row, overdueThresholdDays.value);
}

/** 物料下拉 */
const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_name}（${item.material_code}）`, value: item.id })),
);

/** 仓库下拉 */
const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

/** 调入仓下拉：剔除已选的调出仓（前端先拦一次，DB 的 CHECK 是最后一道） */
const toWarehouseOptions = computed(() =>
  warehouseOptions.value.filter((option) => option.value !== form.from_warehouse_id),
);

/** 在途仓下拉（默认取 `warehouse_type === 'IN_TRANSIT'` 的种子仓） */
const transitWarehouseOptions = computed(() =>
  warehouses.value
    .filter((item) => item.warehouse_type === 'IN_TRANSIT')
    .map((item) => ({ label: `${item.warehouse_name}（${item.warehouse_code}）`, value: item.id })),
);

/** 分页器（服务端分页） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 仓库名称（查不到显示短 id，别留白） */
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

/** 行的按钮可用性 */
function actionsOf(row: TransferRow): ReturnType<typeof availableActions> {
  return availableActions(row.status);
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 「这条路还没通车」：404（路由不存在）/ 10002（业务资源不存在）。
 *
 * 实测口径（`curl` 打真后端 172.18.10.144）：
 * - 未注册路由 → HTTP 404 + `{"code":404,"msg":"请求的资源不存在"}`，规整成 `ApiError(404, …)`；
 * - 业务 404（如按 id 查不到单据）→ `code = 10002`（`shared::error_code` 的通用码）。
 *
 * 这两种都不该在列表页弹红错：W3b 合并前它是**预期状态**，合并后是「这条单据没了」。
 */
function isNotFound(error: unknown): boolean {
  if (error instanceof ApiError) {
    return error.code === 404 || error.code === 10002 || error.message.includes('404');
  }
  return false;
}

/** 拉列表 */
async function loadTransfers(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchTransferPage(buildTransferQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
    apiMissing.value = '';
    // 列表每次刷新都顺带刷新角标（收货 / 删单会让超期总量变化，角标不能停在旧值）
    void loadOverdueSummary();
  } catch (error) {
    rows.value = [];
    total.value = 0;
    if (isNotFound(error)) {
      // 接口未上线：信息提示 + 空态，不是红错（卡面 §七 的兜底要求）
      apiMissing.value = 'GET /api/business/erp/transfer/page 返回 404';
    } else {
      loadError.value = `调拨单列表加载失败：${messageOf(error)}`;
    }
  } finally {
    loading.value = false;
  }
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadTransfers(1);
}

/**
 * 超期开关切换。
 *
 * 打开时必须把状态筛选清空：后端 `in_transit_overdue=true` + `status≠IN_TRANSIT` 直接 400，
 * 而「超期在途」本身就意味着 `status=IN_TRANSIT`，再叠一个状态筛选既冗余又能造出 400 组合。
 * （`buildTransferQuery` 里还有第二道保险：开关打开时整键丢掉 `status`。）
 */
function handleOverdueChange(): void {
  if (filter.in_transit_overdue) {
    filter.status = '';
  }
  handleSearch();
}

/** 点页头角标 → 直接筛「超期在途」（比自己去工具栏找开关少两步） */
function turnOnOverdueFilter(): void {
  filter.in_transit_overdue = true;
  filter.status = '';
  handleSearch();
}

/**
 * 拉超期汇总（页头角标）。
 *
 * 失败一律静默（`null` → 不渲染角标）：旧版后端没这条路由、或库里一台超期单都没有，
 * 都不该在列表页弹红错 —— 角标是锦上添花，不是列表的前置条件。
 */
async function loadOverdueSummary(): Promise<void> {
  try {
    overdueSummary.value = await fetchTransferOverdueSummary();
  } catch {
    overdueSummary.value = null;
  }
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptyTransferFilter());
  current.value = 1;
  void loadTransfers(1);
}

/** 列表记忆 · 「清空筛选」：条件回全量 + 页码回 1 + 删掉 sessionStorage 记忆 */
function handleClearListMemory(): void {
  Object.assign(filter, emptyTransferFilter());
  current.value = 1;
  forgetListMemory();
  void loadTransfers(1);
}

/** 刷新（保留筛选与页码） */
function refresh(): void {
  void loadTransfers(current.value);
  void loadMasterData();
}

/** 切页 */
function handlePageChange(pageInfo: { current: number }): void {
  current.value = pageInfo.current;
  void loadTransfers(pageInfo.current);
}

/** 主数据（物料 / 仓库）：拉不到不挡列表，只是名称列退化成短 id */
async function loadMasterData(): Promise<void> {
  const [materialList, warehouseList] = await Promise.all([
    fetchMaterialPage({ page: 1, page_size: 200 }).catch(() => ({ list: [] as MaterialRow[] })),
    fetchWarehousePage({ page: 1, page_size: 200 }).catch(() => ({ list: [] as Warehouse[] })),
  ]);
  materials.value = materialList.list ?? [];
  warehouses.value = warehouseList.list ?? [];
}

// ==================== 状态流转（按状态显隐 + 不可逆二次确认） ====================

/** 发货弹窗可见 */
const shipVisible = ref(false);
/** 发货中的单 */
const shipRow = ref<TransferRow | null>(null);
/** 发货选的在途仓 */
const shipTransitWarehouseId = ref('');
/** 发货提交中 */
const shipping = ref(false);

/** 打开发货弹窗（在途仓默认取种子仓，取不到就留给后端） */
function openShipDialog(row: TransferRow): void {
  shipRow.value = row;
  shipTransitWarehouseId.value = transitWarehouseOptions.value[0]?.value ?? '';
  shipVisible.value = true;
}

/** 确认发货：`PATCH /status` → `IN_TRANSIT` */
async function confirmShip(): Promise<void> {
  const row = shipRow.value;
  if (!row) {
    return;
  }
  shipping.value = true;
  try {
    await updateTransferStatus(row.id, {
      status: 'IN_TRANSIT',
      transit_warehouse_id: shipTransitWarehouseId.value || null,
    });
    MessagePlugin.success(`${row.transfer_no} 已发货，货在在途仓`);
    shipVisible.value = false;
    await loadTransfers(current.value);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    shipping.value = false;
  }
}

/**
 * 行级动作（提交审批 / 撤回 / 收货）。
 *
 * 收货不可逆 → `DialogPlugin.confirm`（danger）；撤回 = 取消整单（终态）→ 自己一个弹窗要原因；
 * 提交审批是可逆的中间态 → 直接发。
 * 删除单独走 `confirmRemove`（文案不同：删除是「单据消失」不是「状态变化」）。
 */
function runAction(row: TransferRow, action: Exclude<TransferAction, 'edit' | 'remove' | 'ship'>): void {
  const available = availableActions(row.status);
  if (!available[action]) {
    MessagePlugin.warning(transferActionHint(row.status));
    return;
  }
  if (action === 'receive') {
    const dialog = DialogPlugin.confirm({
      header: '收货确认',
      theme: 'warning',
      body:
        `单号 ${row.transfer_no} 将从「在途」变为「已完成」：货进调入仓，库存以调入仓为准。`
        + '收货后单据终态只读、不可撤销，确认继续？',
      confirmBtn: { content: '确认收货', theme: 'primary' },
      onConfirm: async () => {
        dialog.hide();
        await patchStatus(row, 'DONE', `${row.transfer_no} 已收货完成`);
      },
    });
    return;
  }
  if (action === 'withdraw') {
    openWithdrawDialog(row);
    return;
  }
  void patchStatus(row, 'PENDING', `${row.transfer_no} 已${TRANSFER_ACTION_LABEL.submit}`);
}

// ==================== 撤回（PENDING → CANCELLED，取消原因必填） ====================
//
// 为什么不是「退回草稿」：后端 `transfer_model.rs::LEGAL_EDGES` 里 PENDING 只有两条出边
// （→APPROVED / →CANCELLED），P8 真机实测撤回按钮发 `DRAFT` 得到 409「非法状态流转」。
// 撤回的同时后端会终止在跑的审批单，所以这里必须把原因一起给（`ck_biz_transfer_state` 要求非空白）。

/** 撤回弹窗可见 */
const withdrawVisible = ref(false);
/** 要撤回的单 */
const withdrawRow = ref<TransferRow | null>(null);
/** 撤回原因（必填） */
const withdrawReason = ref('');
/** 前端校验/接口错误文案 */
const withdrawError = ref('');
/** 撤回提交中 */
const withdrawing = ref(false);

/** 打开撤回弹窗（每次清空上一次的残留） */
function openWithdrawDialog(row: TransferRow): void {
  withdrawRow.value = row;
  withdrawReason.value = '';
  withdrawError.value = '';
  withdrawVisible.value = true;
}

/** 确认撤回：`PATCH /status` → `CANCELLED` + `reason` */
async function confirmWithdraw(): Promise<void> {
  const row = withdrawRow.value;
  if (!row) {
    return;
  }
  const error = withdrawReasonError(withdrawReason.value);
  if (error) {
    withdrawError.value = error;
    return;
  }
  withdrawing.value = true;
  try {
    await updateTransferStatus(row.id, { status: 'CANCELLED', reason: withdrawReason.value.trim() });
    MessagePlugin.success(`${row.transfer_no} 已撤回（单据已取消）`);
    withdrawVisible.value = false;
    await loadTransfers(current.value);
    if (detail.value?.id === row.id) {
      await openDetail({ ...row, id: row.id });
    }
  } catch (e) {
    withdrawError.value = messageOf(e);
    MessagePlugin.error(messageOf(e));
  } finally {
    withdrawing.value = false;
  }
}

/** 打状态流转接口 + 刷新 + 同步抽屉（抽屉开着时不能显示旧状态） */
async function patchStatus(row: TransferRow, target: TransferStatus, successText: string): Promise<void> {
  try {
    await updateTransferStatus(row.id, { status: target as 'PENDING' | 'DRAFT' | 'DONE' });
    MessagePlugin.success(successText);
    await loadTransfers(current.value);
    if (detail.value?.id === row.id) {
      await openDetail({ ...row, id: row.id });
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

/** 删除（仅 DRAFT / CANCELLED，软删，二次确认） */
function confirmRemove(row: TransferRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除确认',
    theme: 'danger',
    body: `单号 ${row.transfer_no}（${transferStatusLabel(row.status)}）将被删除（软删，可后台恢复）。确认删除？`,
    confirmBtn: { content: '确认删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTransfer(row.id);
        MessagePlugin.success(`${row.transfer_no} 已删除`);
        dialog.hide();
        if (detail.value?.id === row.id) {
          detailVisible.value = false;
        }
        await loadTransfers(current.value);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 详情抽屉 ====================

/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情数据 */
const detail = ref<TransferDetail | null>(null);
/** 详情加载中 */
const detailLoading = ref(false);
/** 详情错误 */
const detailError = ref('');
/** 审批进度（节点 + 审批人明细；与其它单据同一份装载器） */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });
/** 实例级信息（提交人 / 提交时间 / 单号 —— `FlowProgress` 只画节点，这些不在里面） */
const approvalInstance = ref<FlowInstance | null>(null);
/** 审批加载中 */
const approvalLoading = ref(false);
/** 审批错误 */
const approvalError = ref('');

/**
 * 提交那一行（`提交人 X · 2026-11-01 14:00:00 · FLW-…`）。
 *
 * 换组件前本页自绘时间线的第一行就是「谁 · 提交 · 时间」，`FlowProgress` 不含实例级字段，
 * 这一行补回来（口径直接取 `buildApprovalSteps` 的第一步，文案与审批详情页一致）。
 */
const approvalSubmitText = computed(() => {
  const instance = approvalInstance.value;
  if (!instance) {
    return '';
  }
  const [submit] = buildApprovalSteps({ instance, timeline: [] });
  return `提交人 ${submit.who} · ${formatTime(submit.at)} · ${instance.instance_no}`;
});

/**
 * 点行 → 抽屉（点了操作列的链接不会走到这里：`.op-cell` 上 `@click.stop`）。
 *
 * `t-table` 的回调形参是 `RowEventContext<TableRowData>`（`row` 是宽松索引类型），
 * 这里收窄成 `TransferRow` —— 列表就是 `GET /transfer/page` 的 `TransferRow[]`。
 */
function handleRowClick(context: { row: Record<string, unknown> }): void {
  void openDetail(context.row as unknown as TransferRow);
}

/** 打详情 + 审批进度 */
async function openDetail(row: TransferRow): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  approvalInstance.value = null;
  approvalError.value = '';
  try {
    const data = await fetchTransferDetail(row.id);
    detail.value = data;
    await loadTransferApproval(data.approval_instance_id);
  } catch (error) {
    detail.value = null;
    detailError.value = isNotFound(error)
      ? '详情接口尚未上线（等 W3b 联调）'
      : `详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

/**
 * 拉审批进度（`approval_instance_id` 是弱引用；没有就什么都不发）。
 *
 * 实例详情**只拉一次**：节点 + 审批人明细交给共享的 `approvalProgressOf`
 * （与采购 / 收货 / 应付 / 凭证同一份映射），实例级字段（提交人 / 单号）另存。
 * 拉不到不影响单据本身：给黄条 + 空进度，不把详情整块吞掉。
 */
async function loadTransferApproval(instanceId: string | null): Promise<void> {
  if (!instanceId) {
    approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    approvalInstance.value = null;
    return;
  }
  approvalLoading.value = true;
  approvalError.value = '';
  try {
    const instanceDetail = await getFlowInstance(instanceId);
    approvalInstance.value = instanceDetail.instance;
    approvalProgress.value = approvalProgressOf(instanceDetail);
  } catch (error) {
    approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    approvalInstance.value = null;
    approvalError.value = `审批进度加载失败：${messageOf(error)}`;
  } finally {
    approvalLoading.value = false;
  }
}

/** 状态时间戳（只列有值的，避免一排「—」） */
function stampsOf(row: TransferRow): Array<{ label: string; at: string | null }> {
  const all: Array<{ label: string; at: string | null }> = [
    { label: '提交', at: row.submitted_at },
    { label: '批准', at: row.approved_at },
    { label: '发货', at: row.shipped_at },
    { label: '收货', at: row.received_at },
    { label: '取消', at: row.cancelled_at },
  ];
  return all.filter((item) => Boolean(item.at));
}

// ==================== 新建 / 编辑弹窗 ====================

/** 弹窗可见 */
const formVisible = ref(false);
/** 表单（含被编辑单的 id，空 = 新建） */
const form = reactive<TransferFormState & { id: string }>({ ...emptyTransferForm(), id: '' });
/** 字段错误 */
const formErrors = ref<TransferFormErrors>({});
/** 提交错误 */
const submitError = ref('');
/** 提交中 */
const formSaving = ref(false);
/** 提交模式（决定哪个按钮转圈） */
const savingMode = ref<'draft' | 'submit'>('draft');
/** D9：复制进行中（按钮 loading + disabled，连点只建一张） */
const copying = ref(false);
/** D9：复制提示（序列号不复制；下次打开表单要清掉） */
const copyNotice = ref('');

/** 行级错误（模板里直接取） */
const rowErrors = computed(() => formErrors.value.rows ?? {});

/**
 * 打开新建弹窗。
 *
 * 优先级：**源单带出 > 上次记忆 > 系统默认**（本页暂无源单路径，`source` 参数留给以后）。
 */
function applyCreateDefaults(source: Partial<TransferFormState> = {}): void {
  const resolved = resolveDefaults<TransferFormState>(
    emptyTransferForm(),
    formMemory.recall(),
    source,
    MEMORY_FIELDS,
  );
  Object.assign(form, pickFields(resolved, MEMORY_FIELDS));
  memoryLabels.value = formMemory.labelsOf(source);
}

/** 打开新建弹窗 */
function openCreateDialog(): void {
  Object.assign(form, emptyTransferForm(), { id: '' });
  formErrors.value = {};
  submitError.value = '';
  copyNotice.value = '';
  applyCreateDefaults();
  formVisible.value = true;
}

/**
 * D9：复制为新单（详情抽屉底部的「复制为新单」）。
 *
 * 语义全在后端（`POST /erp/transfer/{id}/copy`）：新草稿 + 新单号 + 两仓 / 原因 / 备注 /
 * 明细全带出，状态与审批关联清空，**序列号不复制**（实物号码跟着原单走）。
 * 前端拿到**新单详情**后填表（`form.id` = 新单 id，所以保存走「改这张草稿」，
 * 连点不会多建单），并把「序列号要重新选」这件事说清楚。
 */
async function copyCurrent(): Promise<void> {
  const origin = detail.value;
  if (!origin || copying.value) {
    return;
  }
  copying.value = true;
  try {
    const created = await copyTransfer(origin.id);
    const serialRequired: Record<string, boolean> = {};
    for (const item of created.items ?? []) {
      const hit = materials.value.find((material) => material.id === item.material_id);
      serialRequired[item.material_id] = hit?.serial_required ?? false;
    }
    detailVisible.value = false;
    Object.assign(form, detailToForm(created, serialRequired), { id: created.id });
    if (form.items.length === 0) {
      form.items = [addTransferItem('row-1')];
    }
    formErrors.value = {};
    submitError.value = '';
    copyNotice.value = transferCopyNotice(origin);
    formVisible.value = true;
    MessagePlugin.success(`已复制为 ${created.transfer_no}（草稿），请核对后提交`);
    await loadTransfers(1);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    copying.value = false;
  }
}

/** 表单记忆 · 「重置」：清记忆 + 调出/调入仓退回系统默认 */
function handleResetMemory(): void {
  formMemory.forget();
  memoryLabels.value = [];
  const fresh = emptyTransferForm();
  const patch: Record<string, unknown> = {};
  for (const field of MEMORY_FIELDS) {
    patch[String(field)] = fresh[field];
  }
  Object.assign(form, patch);
  MessagePlugin.success('已清空表单记忆，下次新建按系统默认');
}

/** 打开编辑弹窗（仅 DRAFT；先拉详情填表） */
async function openEditDialog(row: TransferRow): Promise<void> {
  if (!availableActions(row.status).edit) {
    MessagePlugin.warning(transferActionHint(row.status));
    return;
  }
  formErrors.value = {};
  submitError.value = '';
  copyNotice.value = '';
  formSaving.value = true;
  try {
    const data = await fetchTransferDetail(row.id);
    const serialRequired: Record<string, boolean> = {};
    for (const item of data.items ?? []) {
      const hit = materials.value.find((material) => material.id === item.material_id);
      serialRequired[item.material_id] = hit?.serial_required ?? false;
    }
    Object.assign(form, detailToForm(data, serialRequired), { id: data.id });
    if (form.items.length === 0) {
      form.items = [addTransferItem('row-1')];
    }
    formVisible.value = true;
  } catch (error) {
    MessagePlugin.error(isNotFound(error) ? '详情接口尚未上线（等 W3b 联调）' : messageOf(error));
  } finally {
    formSaving.value = false;
  }
}

/** 加一行明细（key 用「行数+时间戳」保证唯一，删行再加不会撞） */
function addItem(): void {
  form.items.push(addTransferItem(`row-${form.items.length + 1}-${Date.now()}`));
}

// ==================== R-P1-5：从库存选择 ====================

/** 库存选择弹窗可见 */
const stockPickerVisible = ref(false);

/** 打开库存选择：没有调出仓就没有库存口径（弹窗里也会提示，但这里先拦一句更省事） */
function openStockPicker(): void {
  if (!form.from_warehouse_id) {
    MessagePlugin.warning('先选「调出仓」：可调拨的库存是按调出仓算的');
    return;
  }
  stockPickerVisible.value = true;
}

/**
 * 把弹窗选中的库存并进明细。
 *
 * 合并规则在 `mergeStockPicks`（纯函数，单测钉住）：同物料同库位累加、不同库位各占一行、
 * 整行空着的首行先复用 —— 作业员点完不该看到一行空的明细或者两行一模一样的物料。
 */
function applyStockPicks(picks: StockPick[]): void {
  if (picks.length === 0) {
    return;
  }
  const before = form.items.length;
  form.items = mergeStockPicks(
    form.items,
    picks.map((pick) => ({
      material_id: pick.material_id,
      from_location_id: pick.from_location_id,
      qty: pick.qty,
      serial_required: pick.serial_required,
    })),
    (index) => `row-${index + 1}-${Date.now()}`,
  );
  syncSerialRequired();
  formErrors.value = validateTransferForm(form);
  const added = form.items.length - before;
  MessagePlugin.success(
    added > 0
      ? `已从库存带入 ${picks.length} 条（新增 ${added} 行明细）`
      : `已从库存带入 ${picks.length} 条明细（明细共 ${form.items.length} 行：同物料同库位的已累加数量）`,
  );
  // 一物一码物料带出来后必须选序列号，别让作业员以为完事了
  const needSerials = form.items.filter(
    (item) => serialRequiredOf(item.material_id) && item.serial_ids.length !== Number(item.qty),
  );
  if (needSerials.length > 0) {
    MessagePlugin.info(`${needSerials.length} 行是一物一码物料：还要点「选择序列号」把号码对上数量`);
  }
}

/** 删一行明细（至少留一条） */
function removeItem(index: number): void {
  if (form.items.length <= 1) {
    return;
  }
  form.items.splice(index, 1);
}

/**
 * 换物料：重置「一物一码」标记并清掉已选序列号（物料变了序列号必然不匹配）。
 *
 * `value` 是 `t-select` 的 change 出参 —— **必须用它而不是只读 `item.material_id`**：
 * 实测 TDesign 的 `change` 可能先于 `v-model` 回写触发（那一刻 `item.material_id` 还是旧值），
 * 只读 v-model 会把一物一码物料判成「非一物一码」，序列号入口整个不出现。
 */
function handleMaterialChange(item: TransferItemForm, value: unknown): void {
  const materialId = typeof value === 'string' && value ? value : item.material_id;
  item.material_id = materialId;
  const hit = materials.value.find((material) => material.id === materialId);
  item.serial_required = hit?.serial_required ?? false;
  item.serial_ids = [];
}

/** 改数量：一物一码物料数量变小时，已选序列号要跟着裁掉（否则「个数 ≠ 数量」提交必被拦） */
function handleQtyChange(item: TransferItemForm): void {
  if (!serialRequiredOf(item.material_id)) {
    return;
  }
  const qty = Math.max(0, Math.floor(Number(item.qty ?? 0)));
  if (item.serial_ids.length > qty) {
    item.serial_ids = item.serial_ids.slice(0, qty);
  }
}

/**
 * 物料是否一物一码（**从物料主数据现算，不依赖表单行里的缓存字段**）。
 *
 * 为什么不只用 `item.serial_required`：TDesign 的 `t-select` 在 `change` 与 `v-model` 回写
 * 之间没有顺序保证（实测 change 先到，此时 `item.material_id` 还是旧值），缓存字段会漏更新，
 * 「选择序列号」入口就整个不出现。模板一律走这个函数，`item.serial_required` 只在提交前
 * 用 `syncSerialRequired()` 刷一遍给校验用。
 */
function serialRequiredOf(materialId: string): boolean {
  if (!materialId) {
    return false;
  }
  return materials.value.find((material) => material.id === materialId)?.serial_required ?? false;
}

/** 提交前把「一物一码」标记刷成主数据真值（`validateTransferForm` 吃的是表单对象） */
function syncSerialRequired(): void {
  for (const item of form.items) {
    item.serial_required = serialRequiredOf(item.material_id);
  }
}

/** 提交表单（`draft` = 只存；`submit` = 存完再提交流转） */
async function submitForm(mode: 'draft' | 'submit'): Promise<void> {
  // 先把「一物一码」标记刷成主数据真值，再校验（否则下拉刚选完的那一刻校验会漏）
  syncSerialRequired();
  const errors = validateTransferForm(form);
  formErrors.value = errors;
  submitError.value = '';
  if (hasTransferFormError(errors)) {
    return;
  }
  savingMode.value = mode;
  formSaving.value = true;
  try {
    const payload = buildTransferPayload(form);
    const saved = form.id ? await updateTransfer(form.id, payload) : await createTransfer(payload);
    if (mode === 'submit') {
      await updateTransferStatus(saved.id, { status: 'PENDING' });
      MessagePlugin.success(`${saved.transfer_no} 已提交审批`);
    } else {
      MessagePlugin.success(`${saved.transfer_no} 已保存草稿`);
    }
    formVisible.value = false;
    // 保存成功才写记忆（这条仓库线下次还这么调）
    formMemory.remember(form);
    memoryLabels.value = [];
    current.value = 1;
    await loadTransfers(1);
    if (detailVisible.value && detail.value?.id === saved.id) {
      await openDetail(saved);
    }
  } catch (error) {
    submitError.value = isNotFound(error)
      ? '调拨接口尚未上线（等 W3b 联调）：本次提交没有发到后端'
      : messageOf(error);
  } finally {
    formSaving.value = false;
  }
}

// ==================== 序列号选择弹窗 ====================

/** 弹窗可见 */
const serialPickerVisible = ref(false);
/** 正在给第几条明细选 */
const serialPickerIndex = ref(-1);
/** 可选序列号 */
const serialOptions = ref<SerialRow[]>([]);
/** 已选 */
const serialPickerSelected = ref<string[]>([]);
/** 加载中 */
const serialLoading = ref(false);
/** 加载错误 */
const serialError = ref('');
/** 是否被截断（≥ 上限） */
const serialTruncated = ref(false);

/** 这次要选几个 */
const serialPickerNeed = computed(() => {
  const item = form.items[serialPickerIndex.value];
  return item ? Number(item.qty ?? 0) : 0;
});

/** 打开选择器：按「物料 × 调出仓 × 在库」拉可选序列号 */
async function openSerialPicker(index: number): Promise<void> {
  const item = form.items[index];
  if (!item) {
    return;
  }
  serialPickerIndex.value = index;
  serialPickerSelected.value = [...item.serial_ids];
  serialOptions.value = [];
  serialError.value = '';
  serialTruncated.value = false;
  serialPickerVisible.value = true;

  if (!form.from_warehouse_id) {
    serialError.value = '先选调出仓：序列号按「物料 × 仓库」过滤';
    return;
  }
  if (!item.material_id) {
    serialError.value = '先选物料';
    return;
  }
  serialLoading.value = true;
  try {
    const data = await fetchSerialPage({
      material_id: item.material_id,
      warehouse_id: form.from_warehouse_id,
      status: 'IN_STOCK',
      page: 1,
      page_size: SERIAL_OPTION_LIMIT,
    });
    serialOptions.value = data.list ?? [];
    serialTruncated.value = (data.total ?? 0) > serialOptions.value.length;
  } catch (error) {
    serialError.value = `序列号加载失败：${messageOf(error)}`;
  } finally {
    serialLoading.value = false;
  }
}

/** 按住手头数量全选（默认取前 N 个，作业员可再微调） */
function selectAllSerials(): void {
  serialPickerSelected.value = serialOptions.value.slice(0, serialPickerNeed.value).map((serial) => serial.id);
}

/** 确认：回填到当前明细行 */
function confirmSerialPick(): void {
  const item = form.items[serialPickerIndex.value];
  if (!item) {
    serialPickerVisible.value = false;
    return;
  }
  item.serial_ids = [...serialPickerSelected.value];
  // 选完立刻复校验：还能顺手清掉旧错误
  syncSerialRequired();
  formErrors.value = validateTransferForm(form);
  serialPickerVisible.value = false;
}

// ==================== 初始化 ====================

/**
 * 路由参数（跨模块深链）——`?transfer_no=TR-…` 直接打开该调拨单详情。
 *
 * # 为什么需要
 *
 * 结账 / 年结的「挡路单据明细」里，调拨单那行的「去处理」带的是 `transfer_no`
 * （后端 `period_items::route_of` 给的），但本页原来**不消费**这个参数：
 * 点过来只落到一张没筛选的列表上，用户还得自己再搜一遍单号。
 * voucher / purchase / stock-in 三页早就都有 `applyRouteQuery`，这里补齐。
 *
 * # 两条边界
 *
 * 1. 参数消费后立刻 `replace` 掉：刷新页面不该重复弹一次详情；
 * 2. 查不到（单号不存在 / 已删 / 无权限）就**说清**，不留一张静默的空列表。
 */
async function applyRouteQuery(): Promise<void> {
  const transferNo = transferNoFromQuery(route.query as Record<string, unknown>);
  if (!transferNo) {
    return;
  }
  await router.replace({ path: route.path, query: {} });
  filter.transfer_no = transferNo;
  await loadTransfers(1);
  const row = rows.value.find((item) => item.transfer_no === transferNo);
  if (!row) {
    void MessagePlugin.warning(`没有找到调拨单 ${transferNo}（可能已删除，或没有查看权限）`);
    return;
  }
  await openDetail(row);
}

onMounted(async () => {
  await loadMasterData();
  // 列表记忆恢复的页码要在首次加载就生效
  await loadTransfers(current.value);
  // 深链要等首屏列表出来之后再跑（要按单号在列表里定位那一行）
  await applyRouteQuery();
});
</script>

<style scoped lang="less">
.transfer-page {
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

/* 超期在途开关：和状态下拉一个视觉分量，别让文案跟输入框粘在一起 */
.tool-row__overdue {
  margin-right: -2px;
  white-space: nowrap;
}

.tool-row__no {
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

/* 超期角标：可点（点一下直接筛超期），hover 给手型 + 稍微提亮 */
.panel-head__overdue {
  cursor: pointer;
  transition: filter 0.15s ease;

  &:hover {
    filter: brightness(0.96);
  }
}

/* 状态列：状态标签 + 超期红标一行排开，换行也不撑高整行 */
.status-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
  min-width: 0;
}

/*
 * 「超期 N 天」红标。
 *
 * 颜色只用语义变量（`--td-error-color`），不写裸 hex：本仓 22 套色板 × 明暗
 * 全靠语义变量驱动，写死一个红会在别的色板里瞎眼（`pnpm check:palettes` 也会红）。
 * 红底浅色描边 + 等宽数字，比纯红字在深色模式下也读得清。
 */
.overdue-mark {
  flex: 0 0 auto;
  padding: 0 6px;
  border: 1px solid var(--td-error-color);
  border-radius: var(--td-radius-small, 3px);
  color: var(--td-error-color);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  line-height: 18px;
  white-space: nowrap;
}

/* 详情抽屉里同一颗红标当正文用（边框去掉，避免在小字号下显得脏） */
.overdue-mark--text {
  color: var(--td-error-color);
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

/* ==================== 详情抽屉 ==================== */

.drawer-footer {
  display: flex;
  justify-content: flex-end;
}

.drawer-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.detail__head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 12px;
}

.detail__no {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(220px, 1fr));
  gap: 8px 20px;
  padding: 12px 14px;
  border-radius: var(--td-radius-medium);
  background: var(--td-bg-color-container-hover);
}

.detail-field {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
  font-size: 13px;
}

.detail-field__label {
  flex: 0 0 68px;
  color: var(--td-text-color-placeholder);
}

.detail-field__value {
  overflow: hidden;
  color: var(--td-text-color-primary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.stamp-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 20px;
  margin: 12px 0 4px;
  padding: 0;
  list-style: none;
}

.stamp {
  display: flex;
  align-items: baseline;
  gap: 6px;
  font-size: 12px;
}

.stamp__label {
  color: var(--td-text-color-placeholder);
}

.stamp__value {
  color: var(--td-text-color-secondary);
}

.section-title {
  margin: 18px 0 10px;
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.approval-empty {
  padding: 12px 14px;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
  border-radius: var(--td-radius-medium);
  background: var(--td-bg-color-container-hover);
}

/*
 * 审批进度（C2）：自绘时间线已换成全站 `FlowProgress`，这里只留一行实例级说明
 * （提交人 / 提交时间 / 单号 —— 进度条只画节点，不含这些）。
 */
.approval-submit {
  margin: 0 0 6px 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 弹窗表单 ==================== */

.form-row {
  display: flex;
  gap: 16px;
}

.form-row__col {
  flex: 1 1 0;
  min-width: 0;
}

.dlg-alert {
  margin-bottom: 12px;
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

/* 「从库存选择」按钮：贴在明细标题右边，窄屏换行也不挤掉提示 */
.items__pick {
  align-self: center;
}

/* 列宽固定：物料弹性、数量/序列号/操作定宽，行与表头用同一套 grid 才不跑偏 */
.items__grid {
  display: grid;
  grid-template-columns: minmax(200px, 1fr) 130px minmax(220px, 260px) 60px;
  gap: 12px;
  align-items: start;
}

.items__grid--head {
  margin-bottom: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.items__row {
  padding: 8px 10px;
  margin-bottom: 8px;
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-default);
}

.items__cell {
  min-width: 0;
}

.items__cell--op {
  padding-top: 6px;
  text-align: right;
}

.serial-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.cell-error {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
}

.items__add {
  width: 100%;
  margin-top: 4px;
}

.dlg-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 20px;
}

/* ==================== 序列号选择 ==================== */

.serial-dialog__bar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
  flex-wrap: wrap;
}

.serial-dialog__hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.serial-dialog__empty {
  padding: 20px 0;
  font-size: 13px;
  text-align: center;
  color: var(--td-text-color-placeholder);
}

.serial-dialog__warn {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-warning-color);
}

.serial-list {
  display: grid;
  grid-template-columns: repeat(2, minmax(220px, 1fr));
  gap: 4px 12px;
  max-height: 320px;
  overflow-y: auto;
}

.serial-list__item {
  margin: 0;
}

/* ==================== 发货弹窗 ==================== */

.ship-text {
  margin: 0 0 12px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
}
</style>
