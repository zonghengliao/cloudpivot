<!--
  请假单页（ARCH-9 §6.1 页面 #3 · §6.2 · HR-28）。

  ```
  两个 tab：全部请假（按 dept 数据权限） / 我的请假（mine=true，只显示自己）
  列表（分页 + 筛选） → 新建/编辑抽屉（表单） → 详情抽屉（单据 + 审批轨迹 + 动作栏）
  ```

  # 数据来源（三个接口，全部走同一份契约）

  | 块 | 接口 | 说明 |
  |---|---|---|
  | 列表 | `GET /api/business/hr/leave/page` | 「全部」不带 `mine`；「我的」带 `mine=true`（设计 §6.1） |
  | 详情 | `GET /api/business/hr/leave/{id}` | `LeaveRes` 平铺 + `approval`（实例 + 时间线） |
  | 写 | `POST/PUT /leave` · `POST /{id}/submit` · `POST /{id}/cancel` | 建单 / 改草稿 / 提交 / 删除·撤回·销假 |

  `approval.timeline` 已经带 `node_name` / `assignee_name` / `status_label` / 意见 / 时间，
  **不再调 flow 模块的接口**：详情接口就是审批轨迹的唯一来源，多调一次只会多一处
  「两个来源不一致」的风险（同 `hr/employee` 详情不单调合同/薪资的口径）。

  # 四条与「想当然」不一样的交互（每一条都有真库/真接口依据）

  1. **「我的请假」不是另一个接口**：同一个 `page` 带 `mine=true`。tab 切换必须重查，
     且不能复用上一个 tab 的数组（否则会把别人的单显示在自己的 tab 里）。
  2. **作废接口一条路做三件事**：草稿→删除、审批中→撤回、已批准→销假；**三条都要
     填原因 ≥5 字**（后端 `cancel_of` 在状态分流前统一校验）。所以按钮文案按状态变，
     弹窗里的原因标签也跟着变（删除原因 / 撤回原因 / 销假原因）。
  3. **`duration_days` 是字符串**（`Decimal`）：显示走去尾零的 `durationText`，
     表单里按字符串传，不做浮点加减。
  4. **改草稿只能改非员工字段**：后端 `LeaveUpdateReq` 不收 `employee_id`（换人要作废
     重开），表单里员工只读展示。

  # 本页不做的（已上报）

  - 员工自助的独立入口：`/leave/my` 随 **HR-41**；本页「我的请假」tab 已按设计 §6.1 落地。
  - 附件（病假证明）：`DocAttachmentPanel` 的 `doc_type` 与必传规则属附件轴卡，本卡不接。
-->
<template>
  <div class="hr-leave-page">
    <!-- ==================== ① tab + 筛选 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <t-tabs v-model="activeTab" class="page-tabs" data-testid="hr-leave-tabs" @change="handleTabChange">
        <t-tab-panel v-if="canListAll" value="all" label="全部请假" data-testid="hr-leave-tab-all" />
        <t-tab-panel value="mine" label="我的请假" data-testid="hr-leave-tab-mine" />
      </t-tabs>

      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="单号 / 工号 / 姓名"
          clearable
          data-testid="hr-leave-keyword"
          @enter="handleSearch"
          @clear="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <DeptTreeSelect
          v-if="!mineTab"
          v-model="filter.dept_id"
          class="filter-row__dept"
          placeholder="全部部门"
          data-testid="hr-leave-dept"
        />

        <t-select
          v-model="filter.leave_type"
          class="filter-row__select"
          :options="leaveTypeOptions"
          placeholder="全部假别"
          multiple
          clearable
          :min-collapsed-num="1"
          data-testid="hr-leave-type"
        />

        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          multiple
          clearable
          :min-collapsed-num="1"
          data-testid="hr-leave-status"
        />

        <t-date-range-picker
          v-model="dateRange"
          class="filter-row__range"
          clearable
          allow-input
          data-testid="hr-leave-dates"
        />

        <t-button theme="primary" data-testid="hr-leave-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="hr-leave-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="hr-leave-refresh" @click="loadRows">刷新</t-button>
      </div>

      <ListMemoryBar
        :restored="memory.restored.value"
        :text="memory.restoredText.value"
        testid="hr-leave-memory"
        @clear="handleClearMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">{{ mineTab ? '我的请假' : '请假单' }}</span>
          <span class="panel-head__meta" data-testid="hr-leave-meta">共 {{ total }} 张</span>
        </div>
        <div class="panel-head__side">
          <t-button v-if="canCreate" theme="primary" data-testid="hr-leave-create" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建请假
          </t-button>
        </div>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert" data-testid="hr-leave-list-error">
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
        data-testid="hr-leave-table"
        @page-change="handlePageChange"
      >
        <template #leave_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="hr-leave-row-open" @click="openDetail(row)">
            {{ row.leave_no }}
          </t-link>
        </template>

        <template #employee="{ row }">
          <span class="emp-name">{{ row.employee_name }}</span>
          <span class="emp-no mono">{{ row.employee_no }}</span>
        </template>

        <template #dept_name="{ row }">
          <span :class="{ muted: !row.dept_name }">{{ textOr(row.dept_name) }}</span>
        </template>

        <template #leave_type="{ row }">
          <t-tag size="small" variant="light" theme="default" data-testid="hr-leave-type-tag">
            {{ row.leave_type_label || leaveTypeLabel(row.leave_type) }}
          </t-tag>
        </template>

        <template #range="{ row }">
          <span class="mono range-text">{{ rangeText(row) }}</span>
        </template>

        <template #duration_days="{ row }">
          <span class="mono">{{ durationText(row.duration_days) }}</span>
        </template>

        <template #paid="{ row }">
          <t-tag size="small" variant="light" :theme="row.paid === PAID_NO ? 'warning' : 'success'">
            {{ row.paid_label || paidLabel(row.paid) }}
          </t-tag>
        </template>

        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="leaveStatusTheme(row.status)" data-testid="hr-leave-status-tag">
            {{ row.status_label || leaveStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="hr-leave-row-detail" @click="openDetail(row)">详情</t-link>
            <template v-if="!row.locked">
              <t-link
                v-if="canUpdate && leaveActionsOf(row.status).edit"
                theme="primary"
                data-testid="hr-leave-row-edit"
                @click="openEdit(row)"
              >
                编辑
              </t-link>
              <t-link
                v-if="canSubmit && leaveActionsOf(row.status).submit"
                theme="primary"
                data-testid="hr-leave-row-submit"
                @click="handleSubmit(row)"
              >
                提交
              </t-link>
            </template>
            <t-link
              v-if="canCancel && leaveActionsOf(row.status).cancel"
              theme="danger"
              data-testid="hr-leave-row-cancel"
              @click="openCancel(row)"
            >
              {{ leaveActionsOf(row.status).cancelLabel }}
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="hr-leave-empty">
            <t-icon class="panel-empty__icon" name="time" />
            <p class="panel-empty__title">{{ isLeaveFiltered(filter) ? '没有匹配的请假单' : '还没有请假单' }}</p>
            <p class="panel-empty__desc">
              {{
                isLeaveFiltered(filter)
                  ? '换个关键词 / 假别 / 状态 / 日期试试，或点「重置」看全部'
                  : mineTab
                    ? '你还没有请过假；点右上「新建请假」提一张（审批通过后才进考勤与薪资扣款）'
                    : '点右上「新建请假」替员工建单；员工也能在「我的请假」里自助提交'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑抽屉 ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      size="640px"
      :footer="false"
      :header="false"
      destroy-on-close
      data-testid="hr-leave-form-drawer"
    >
      <div class="drawer">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title">{{ editingId ? '编辑请假单' : '新建请假单' }}</span>
            <span v-if="editingNo" class="mono drawer-head__no">{{ editingNo }}</span>
            <t-tag size="small" theme="default" variant="light">草稿</t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="hr-leave-form-close" @click="formVisible = false">
              取消
            </t-button>
          </div>
        </div>

        <t-form :data="form" label-width="96px" class="leave-form" data-testid="hr-leave-form">
          <t-form-item label="员工" required-mark>
            <template v-if="canPickEmployee">
              <t-select
                v-model="form.employee_id"
                class="w-full"
                filterable
                clearable
                :loading="employeeLoading"
                :options="employeeOptions"
                placeholder="输入工号 / 姓名搜索"
                data-testid="hr-leave-employee"
                @filter="searchEmployees"
              />
            </template>
            <template v-else>
              <span class="self-employee" data-testid="hr-leave-employee-self">
                {{ selfEmployeeLabel || '未关联员工档案' }}
              </span>
              <div class="field-hint">自助建单只能给自己请假（员工档案由人事维护）</div>
            </template>
            <div v-if="formErrors.employee_id" class="field-error">{{ formErrors.employee_id }}</div>
          </t-form-item>

          <t-form-item label="假别" required-mark>
            <t-select
              v-model="form.leave_type"
              class="w-full"
              :options="leaveTypeOptions"
              placeholder="请选择假别"
              data-testid="hr-leave-type-select"
            />
            <div v-if="formErrors.leave_type" class="field-error">{{ formErrors.leave_type }}</div>
          </t-form-item>

          <t-form-item label="开始时间" required-mark>
            <t-date-picker
              v-model="form.start_time"
              class="w-full"
              enable-time-picker
              allow-input
              clearable
              format="YYYY-MM-DD HH:mm"
              placeholder="半天假用 12:00 切（上午到 12:00 / 下午从 12:00）"
              data-testid="hr-leave-start"
              @change="syncDuration"
            />
            <div v-if="formErrors.start_time" class="field-error">{{ formErrors.start_time }}</div>
          </t-form-item>

          <t-form-item label="结束时间" required-mark>
            <t-date-picker
              v-model="form.end_time"
              class="w-full"
              enable-time-picker
              allow-input
              clearable
              format="YYYY-MM-DD HH:mm"
              placeholder="必须晚于开始时间"
              data-testid="hr-leave-end"
              @change="syncDuration"
            />
            <div v-if="formErrors.end_time" class="field-error">{{ formErrors.end_time }}</div>
          </t-form-item>

          <t-form-item label="请假天数" required-mark>
            <t-input-number
              v-model="form.duration_days"
              class="w-full"
              theme="normal"
              :step="0.5"
              :min="0.5"
              :max="365"
              :decimal-places="1"
              placeholder="按 0.5 天为步长；按起止自动建议，可改"
              data-testid="hr-leave-duration"
            />
            <div v-if="formErrors.duration_days" class="field-error">{{ formErrors.duration_days }}</div>
            <div v-else-if="suggestedDuration && suggestedDuration !== textOf(form.duration_days)" class="field-hint">
              按起止算应为 {{ suggestedDuration }} 天。请假天数直接进薪资扣款（无薪假），必须与起止对得上。
            </div>
          </t-form-item>

          <t-form-item label="带薪">
            <t-radio-group v-model="form.paid" data-testid="hr-leave-paid">
              <t-radio :value="PAID_YES">带薪</t-radio>
              <t-radio :value="PAID_NO">无薪（进薪资扣款）</t-radio>
            </t-radio-group>
            <div class="field-hint">不按假别自动推导：事假也可能带薪，以这里选的为准。</div>
          </t-form-item>

          <t-form-item label="请假事由">
            <t-textarea
              v-model="form.reason"
              class="w-full"
              :maxlength="LEAVE_MAX_REASON"
              placeholder="如：家中有事 / 医院复查（最多 500 字）"
              :autosize="{ minRows: 2, maxRows: 4 }"
              data-testid="hr-leave-reason"
            />
            <div v-if="formErrors.reason" class="field-error">{{ formErrors.reason }}</div>
          </t-form-item>

          <t-form-item label="备注">
            <t-textarea
              v-model="form.remark"
              class="w-full"
              :maxlength="LEAVE_MAX_REMARK"
              placeholder="内部备注（最多 1000 字）"
              :autosize="{ minRows: 2, maxRows: 4 }"
              data-testid="hr-leave-remark"
            />
            <div v-if="formErrors.remark" class="field-error">{{ formErrors.remark }}</div>
          </t-form-item>
        </t-form>

        <div class="form-foot">
          <t-button variant="outline" data-testid="hr-leave-form-cancel" @click="formVisible = false">取消</t-button>
          <t-button
            theme="default"
            variant="outline"
            :loading="saving"
            data-testid="hr-leave-form-save"
            @click="handleSave(false)"
          >
            保存草稿
          </t-button>
          <t-button
            v-if="canSubmit"
            theme="primary"
            :loading="saving"
            data-testid="hr-leave-form-save-submit"
            @click="handleSave(true)"
          >
            保存并提交
          </t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="760px"
      :footer="false"
      :header="false"
      destroy-on-close
      data-testid="hr-leave-detail-drawer"
    >
      <div v-if="detailLoading" class="drawer drawer--loading" data-testid="hr-leave-detail-loading">
        <t-loading size="large" />
        <span class="muted">加载请假单…</span>
      </div>

      <div v-else-if="detailError" class="drawer">
        <t-alert theme="error" :message="detailError" data-testid="hr-leave-detail-error">
          <template #operation>
            <t-button size="small" variant="outline" @click="reloadDetail">重试</t-button>
          </template>
        </t-alert>
      </div>

      <div v-else-if="detail" class="drawer" data-testid="hr-leave-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="mono drawer-head__no">{{ detail.leave_no }}</span>
            <t-tag size="small" variant="light" :theme="leaveStatusTheme(detail.status)" data-testid="hr-leave-detail-status">
              {{ detail.status_label || leaveStatusLabel(detail.status) }}
            </t-tag>
            <span class="drawer-head__meta">{{ detail.employee_name }} · {{ textOr(detail.dept_name) }}</span>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="hr-leave-detail-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <DocActionBar title="关键操作" testid="hr-leave-action-bar">
          <t-button
            v-if="canUpdate && detailActions.edit"
            size="small"
            variant="outline"
            data-testid="hr-leave-action-edit"
            @click="openEdit(detail)"
          >
            编辑
          </t-button>
          <t-button
            v-if="canSubmit && detailActions.submit"
            size="small"
            theme="primary"
            data-testid="hr-leave-action-submit"
            @click="handleSubmit(detail)"
          >
            提交审批
          </t-button>
          <t-button
            v-if="canCancel && detailActions.cancel"
            size="small"
            theme="danger"
            variant="outline"
            data-testid="hr-leave-action-cancel"
            @click="openCancel(detail)"
          >
            {{ detailActions.cancelLabel }}
          </t-button>
        </DocActionBar>

        <t-alert
          v-if="detail.status === 'CANCELLED' && detail.cancel_reason"
          theme="warning"
          class="cancel-note"
          data-testid="hr-leave-cancel-note"
        >
          <template #title>已作废（销假）</template>
          <span>
            {{ detail.cancel_reason }}
            <span class="muted">
              —— {{ detail.cancelled_by_name || '未知操作人' }} {{ datetimeText(detail.cancelled_at) }}
            </span>
          </span>
        </t-alert>

        <t-descriptions :column="2" size="small" bordered class="detail-desc">
          <t-descriptions-item label="员工">
            {{ detail.employee_name }} <span class="mono muted">{{ detail.employee_no }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="部门">{{ textOr(detail.dept_name) }}</t-descriptions-item>
          <t-descriptions-item label="假别">{{ detail.leave_type_label || leaveTypeLabel(detail.leave_type) }}</t-descriptions-item>
          <t-descriptions-item label="带薪">
            <t-tag size="small" variant="light" :theme="detail.paid === PAID_NO ? 'warning' : 'success'">
              {{ detail.paid_label || paidLabel(detail.paid) }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="请假区间" :span="2">
            <span class="mono">{{ rangeText(detail) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="请假天数">
            <span class="mono">{{ durationText(detail.duration_days) }}</span> 天
          </t-descriptions-item>
          <t-descriptions-item label="提交">
            <template v-if="detail.submitted_at">
              {{ datetimeText(detail.submitted_at) }}
              <span class="muted">· {{ detail.submitted_by_name || '未知' }}</span>
            </template>
            <span v-else class="muted">未提交</span>
          </t-descriptions-item>
          <t-descriptions-item label="请假事由" :span="2">{{ textOr(detail.reason) }}</t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ textOr(detail.remark) }}</t-descriptions-item>
          <t-descriptions-item label="创建 / 修改" :span="2">
            <span class="mono">{{ datetimeText(detail.created_at) }}</span>
            <span class="muted"> / </span>
            <span class="mono">{{ datetimeText(detail.updated_at) }}</span>
          </t-descriptions-item>
        </t-descriptions>

        <div class="section-head">
          <span class="section-head__title">审批轨迹</span>
          <span v-if="detail.approval_history > 1" class="section-head__meta">
            历史上提交过 {{ detail.approval_history }} 次（撤回重提）
          </span>
        </div>

        <template v-if="detail.approval">
          <div class="approval-head">
            <span class="mono">{{ detail.approval.instance_no }}</span>
            <t-tag size="small" variant="light" theme="primary">{{ detail.approval.status_label }}</t-tag>
            <span class="muted">
              {{ detail.approval.flow_name }} · 发起人 {{ detail.approval.initiator_name }} ·
              {{ datetimeText(detail.approval.started_at) }}
            </span>
          </div>
          <t-timeline class="approval-timeline" data-testid="hr-leave-approval-timeline">
            <t-timeline-item
              v-for="(task, index) in detail.approval.timeline"
              :key="`${task.node_key}-${task.round}-${index}`"
              :label="datetimeText(task.completed_at || task.created_at)"
              :theme="approvalTaskTheme(task.status)"
            >
              <div class="task-line">
                <span class="task-line__node">{{ task.node_name || task.node_key }}</span>
                <span class="task-line__assignee">{{ task.assignee_name || '未指派' }}</span>
                <t-tag size="small" variant="light" :theme="approvalTaskTheme(task.status)">
                  {{ task.status_label || approvalTaskLabel(task.status) }}
                </t-tag>
                <t-tag v-if="task.is_self_approval" size="small" variant="light" theme="warning">自审</t-tag>
                <t-tag v-if="task.is_auto" size="small" variant="light" theme="default">系统代点</t-tag>
              </div>
              <div v-if="task.comment" class="task-comment">意见：{{ task.comment }}</div>
            </t-timeline-item>
          </t-timeline>
          <t-alert v-if="detail.approval.result_comment" theme="info" class="approval-result">
            {{ detail.approval.result_comment }}
          </t-alert>
        </template>
        <div v-else class="empty-block" data-testid="hr-leave-no-approval">
          <t-icon name="time" />
          <span>还没有审批记录（草稿未提交，或审批实例已被清理）</span>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ⑤ 作废 / 撤回 / 删除（一条接口三件事） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      :header="cancelDialogHeader"
      :confirm-btn="{ content: cancelConfirmText, theme: 'danger', loading: cancelling }"
      data-testid="hr-leave-cancel-dialog"
      @confirm="handleCancelConfirm"
    >
      <div class="cancel-body">
        <p class="cancel-body__lead" data-testid="hr-leave-cancel-lead">{{ cancelLead }}</p>
        <t-textarea
          v-model="cancelReason"
          :maxlength="LEAVE_MAX_CANCEL_REASON"
          :autosize="{ minRows: 3, maxRows: 5 }"
          :placeholder="`${cancelReasonLabel(cancelTarget?.status)}：至少 ${LEAVE_MIN_CANCEL_REASON} 个字`"
          data-testid="hr-leave-cancel-reason"
        />
        <div v-if="cancelError" class="field-error" data-testid="hr-leave-cancel-error">{{ cancelError }}</div>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 请假单页（HR-28）。
 *
 * 全部口径走 `leave-kinds.ts` 的纯函数（可单测）：tab / 筛选序列化 / 状态动作 /
 * 表单校验 / 天数与区间文案 / 审批节点配色。页面只做「拉数据 + 摆界面 + 弹提示」。
 *
 * 权限码（`V0387__hr_menu_perms` 逐字一致）：
 * `business:hr:leave:{list,detail,create,update,submit,cancel}`。
 * 页面入口由 `router/index.ts` 的 `meta.permission` 拦；页内按钮由 `hasPermission` 拦
 * （无权限时按钮不出现，而不是点了 403）。
 *
 * 自助角色（只有 `leave:create`，没有 `employee:list`）：
 * 新建抽屉里的员工下拉换成就地展示「我自己」，`employee_id` 从自己的请假记录推出来 ——
 * `/employee/my` 还没有（HR-41 / HR-47 的决策面），所以这里不假装能查到。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import DocActionBar from '@/views/business/erp/shared/DocActionBar.vue';
import { ApiError } from '@/api/request';
import {
  LEAVE_COLUMNS,
  LEAVE_MAX_CANCEL_REASON,
  LEAVE_MAX_REASON,
  LEAVE_MAX_REMARK,
  LEAVE_MIN_CANCEL_REASON,
  LEAVE_PAGE_SIZE,
  LEAVE_PERMS,
  LEAVE_STATUS_OPTIONS,
  LEAVE_TYPE_OPTIONS,
  PAID_NO,
  PAID_YES,
  approvalTaskLabel,
  approvalTaskTheme,
  buildLeaveQuery,
  cancelActionLabel,
  cancelReasonLabel,
  datetimeText,
  defaultLeaveTab,
  durationText,
  emptyLeaveFilter,
  emptyLeaveForm,
  fetchLeaveDetail,
  fetchLeavePage,
  formFromRow,
  isLeaveFiltered,
  leaveActionsOf,
  leaveStatusLabel,
  leaveStatusTheme,
  leaveTypeLabel,
  paidLabel,
  rangeText,
  suggestDuration,
  tabIsMine,
  textOf,
  textOr,
  toCreatePayload,
  toUpdatePayload,
  validateCancelReason,
  validateLeaveForm,
  createLeave,
  submitLeave,
  updateLeave,
  cancelLeave,
  type LeaveDetail,
  type LeaveForm,
  type LeaveListRow,
  type LeaveTab,
} from '@/api/business/hr/leave';
import { fetchEmployeePage, type EmployeeListRow } from '@/api/business/hr/employee';

const userStore = useUserStore();

/** 本页权限（逐条对应 `V0387` 的 6 个码） */
const canListAll = computed(() => userStore.hasPermission(LEAVE_PERMS.list));
const canCreate = computed(() => userStore.hasPermission(LEAVE_PERMS.create));
const canUpdate = computed(() => userStore.hasPermission(LEAVE_PERMS.update));
const canSubmit = computed(() => userStore.hasPermission(LEAVE_PERMS.submit));
const canCancel = computed(() => userStore.hasPermission(LEAVE_PERMS.cancel));
/** 有人事档案查询权限才能替别人建单（自助角色只能给自己） */
const canPickEmployee = computed(() => userStore.hasPermission('business:hr:employee:list'));

/** 表格列（`colKey` 与后端出参同名；`employee` / `range` / `op` 是插槽列） */
const columns = LEAVE_COLUMNS;
const leaveTypeOptions = LEAVE_TYPE_OPTIONS.map((item) => ({ ...item }));
const statusOptions = LEAVE_STATUS_OPTIONS.map((item) => ({ ...item }));

// ==================== tab ====================

/** 当前 tab（无 `leave:list` 的人只能看「我的请假」） */
const activeTab = ref<LeaveTab>(defaultLeaveTab(userStore.hasPermission(LEAVE_PERMS.list)));
const mineTab = computed(() => tabIsMine(activeTab.value));

// ==================== 列表状态 ====================

const filter = reactive(emptyLeaveFilter());
/** 日期区间（`t-date-range-picker` 用数组；筛选行里存两个字符串，双向同步） */
const dateRange = ref<string[]>([]);

const currentPage = ref(1);
const pageSize = ref(LEAVE_PAGE_SIZE);

/** 列表记忆：进页面恢复筛选 + tab + 页码 */
const memory = useListFilterMemory({
  listName: 'hr-leave',
  filter,
  page: currentPage,
  pageSize,
  tab: activeTab,
  defaultTab: defaultLeaveTab(userStore.hasPermission(LEAVE_PERMS.list)),
  allowedTabs: canListAll.value ? ['all', 'mine'] : ['mine'],
});

// 记忆已在 setup 阶段就地恢复筛选 → 日期区间跟着回填（`t-date-range-picker` 要数组）
if (filter.date_from && filter.date_to) {
  dateRange.value = [filter.date_from, filter.date_to];
}
// picker → 筛选行（只在这一侧赋值，避免与 filter→picker 形成回环）
watch(dateRange, (value) => {
  filter.date_from = value && value[0] ? String(value[0]) : '';
  filter.date_to = value && value[1] ? String(value[1]) : '';
});

const rows = ref<LeaveListRow[]>([]);
const total = ref(0);
const loading = ref(false);
const loadError = ref('');

const pagination = computed(() => ({
  current: currentPage.value,
  pageSize: pageSize.value,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 统一错误文案（`ApiError` 带后端中文报文） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    // 409 + 68012 / 66033 这类都是后端已经写好的人话，直接显示
    return error.message;
  }
  return error instanceof Error ? error.message : '请求失败';
}

/** 拉列表（`mine` 由 tab 决定 —— 两个 tab 共用这一条路径，不可能漏传） */
async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const query = buildLeaveQuery(filter, currentPage.value, pageSize.value, mineTab.value);
    // 「我的请假」不带部门筛选（自己的假与所选部门无关，带了反而会把结果筛空）
    if (mineTab.value) {
      delete query.dept_id;
    }
    const data = await fetchLeavePage(query);
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** tab 切换：换 tab 必须重查，且不能沿用上一个 tab 的数组 */
function handleTabChange(): void {
  currentPage.value = 1;
  rows.value = [];
  total.value = 0;
  void loadRows();
}

function handleSearch(): void {
  currentPage.value = 1;
  void loadRows();
}

function handleReset(): void {
  const tab = activeTab.value;
  Object.assign(filter, emptyLeaveFilter());
  activeTab.value = tab;
  dateRange.value = [];
  currentPage.value = 1;
  memory.forget();
  void loadRows();
}

function handleClearMemory(): void {
  handleReset();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  currentPage.value = pageInfo.current;
  if (pageInfo.pageSize && pageInfo.pageSize !== pageSize.value) {
    pageSize.value = pageInfo.pageSize;
  }
  void loadRows();
}

// ==================== 新建 / 编辑表单 ====================

const formVisible = ref(false);
const saving = ref(false);
/** 正在编辑的单 id（空 = 新建） */
const editingId = ref('');
/** 正在编辑的单号（抽屉标题旁展示） */
const editingNo = ref('');
const form = reactive<LeaveForm>(emptyLeaveForm());
const formErrors = ref<Record<string, string>>({});
/** 用户是否手动改过天数（改过就不再按起止覆盖） */
const durationTouched = ref(false);
/** 自助建单时「我自己」的展示文案（无 `employee:list` 时用） */
const selfEmployeeLabel = ref('');

/** 员工下拉（远程搜索；只有 `employee:list` 权限才加载） */
const employeeOptions = ref<{ label: string; value: string }[]>([]);
const employeeLoading = ref(false);
let employeeSearchTimer: ReturnType<typeof setTimeout> | null = null;

/** 按起止建议的天数（仅提示用） */
const suggestedDuration = computed(() => suggestDuration(form.start_time, form.end_time));

/** 起止变了 → 没手动改过天数就跟着建议值走 */
function syncDuration(): void {
  if (durationTouched.value) {
    return;
  }
  const suggested = suggestDuration(form.start_time, form.end_time);
  if (suggested) {
    form.duration_days = suggested;
  }
}

/** 员工下拉：拉一页（默认在职三态，关键工人事只关心在职/试用） */
async function loadEmployees(keyword = ''): Promise<void> {
  if (!canPickEmployee.value) {
    return;
  }
  employeeLoading.value = true;
  try {
    const data = await fetchEmployeePage({
      keyword: keyword.trim() || undefined,
      page: 1,
      page_size: 20,
      status: ['PROBATION', 'ACTIVE', 'SUSPENDED'],
    });
    employeeOptions.value = (data.list ?? []).map((row: EmployeeListRow) => ({
      value: row.id,
      label: `${row.name}（${row.emp_no}）${row.dept_name ? ` · ${row.dept_name}` : ''}`,
    }));
  } catch {
    // 搜索失败不清空已有选项：用户还能选上次拉到的人
  } finally {
    employeeLoading.value = false;
  }
}

/** 输入即搜（250ms 防抖，避免每个字符打一次接口） */
function searchEmployees(keyword: string): void {
  if (employeeSearchTimer) {
    clearTimeout(employeeSearchTimer);
  }
  employeeSearchTimer = setTimeout(() => {
    void loadEmployees(keyword);
  }, 250);
}

/**
 * 自助建单的 `employee_id`：从自己的请假记录里推。
 *
 * 没有 `/employee/my`（HR-41）时这是唯一诚实的取法：`mine=true` 的列表行带
 * `employee_id` + 姓名工号。一条都没有 → 无法建单，直接给出可执行的提示。
 */
async function resolveSelfEmployee(): Promise<boolean> {
  if (canPickEmployee.value) {
    return true;
  }
  try {
    const data = await fetchLeavePage({ mine: true, page: 1, page_size: 1 });
    const own = (data.list ?? [])[0];
    if (!own) {
      MessagePlugin.warning('当前账号还没有请假记录，也没有员工档案可关联：请让人事在「请假单」里替你建第一张单');
      return false;
    }
    form.employee_id = own.employee_id;
    selfEmployeeLabel.value = `${own.employee_name}（${own.employee_no}）`;
    return true;
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    return false;
  }
}

/** 打开新建 */
async function openCreate(): Promise<void> {
  editingId.value = '';
  editingNo.value = '';
  Object.assign(form, emptyLeaveForm());
  formErrors.value = {};
  durationTouched.value = false;
  selfEmployeeLabel.value = '';
  formVisible.value = true;
  // 自助口径：先把「我自己」解析出来（拿不到就别让人白填一屏）
  if (!canPickEmployee.value) {
    const ok = await resolveSelfEmployee();
    if (!ok) {
      formVisible.value = false;
    }
    return;
  }
  if (employeeOptions.value.length === 0) {
    void loadEmployees();
  }
  // 「我的请假」tab 里替自己建单：预填当前登录人对应的员工（能查到就用第一条）
  if (mineTab.value) {
    try {
      const data = await fetchLeavePage({ mine: true, page: 1, page_size: 1 });
      const own = (data.list ?? [])[0];
      if (own && !form.employee_id) {
        form.employee_id = own.employee_id;
        employeeOptions.value = [{ value: own.employee_id, label: `${own.employee_name}（${own.employee_no}）` }, ...employeeOptions.value];
      }
    } catch {
      // 预填失败不影响手选
    }
  }
}

/** 打开编辑（只能改草稿；`employee_id` 不可改） */
function openEdit(row: LeaveListRow): void {
  editingId.value = row.id;
  editingNo.value = row.leave_no;
  Object.assign(form, formFromRow(row));
  formErrors.value = {};
  durationTouched.value = false;
  selfEmployeeLabel.value = `${row.employee_name}（${row.employee_no}）`;
  // 编辑时把当前员工塞进选项，否则下拉显示空
  employeeOptions.value = [{ value: row.employee_id, label: `${row.employee_name}（${row.employee_no}）` }];
  formVisible.value = true;
}

/** 表单校验 → 错误映射（后端也会校验一遍，这里只为即时反馈） */
function collectFormErrors(requireEmployee: boolean): boolean {
  const errors = validateLeaveForm(form, requireEmployee);
  const mapped: Record<string, string> = {};
  for (const item of errors) {
    mapped[item.field] = mapped[item.field] ?? item.message;
  }
  formErrors.value = mapped;
  if (errors.length > 0) {
    MessagePlugin.error(errors[0].message);
    return false;
  }
  return true;
}

/**
 * 保存（`andSubmit=true` → 存完立刻提交审批）。
 *
 * 「保存并提交」在同一个按钮里做两步：先建/改草稿，再 submit。第二步失败时草稿已经
 * 落库（可重试），所以提示要说清「草稿已保存，提交失败原因：…」—— 不给死单。
 */
async function handleSave(andSubmit: boolean): Promise<void> {
  if (saving.value) {
    return;
  }
  if (!collectFormErrors(!editingId.value)) {
    return;
  }
  saving.value = true;
  try {
    let savedId = editingId.value;
    if (editingId.value) {
      await updateLeave(editingId.value, toUpdatePayload(form));
    } else {
      const created = await createLeave(toCreatePayload(form));
      savedId = created.id;
    }
    if (andSubmit && savedId) {
      try {
        await submitLeave(savedId, {});
        MessagePlugin.success('已保存并提交审批');
      } catch (error) {
        MessagePlugin.warning(`草稿已保存，但提交失败：${messageOf(error)}`);
      }
    } else {
      MessagePlugin.success(editingId.value ? '草稿已更新' : '草稿已保存');
    }
    formVisible.value = false;
    await loadRows();
    if (detailVisible.value && savedId) {
      void loadDetail(savedId);
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    saving.value = false;
  }
}

// 天数被手改 → 不再自动覆盖（`t-input-number` 写回 number，比较前先归一成字符串）
watch(
  () => form.duration_days,
  (value, previous) => {
    const suggested = suggestDuration(form.start_time, form.end_time);
    if (textOf(value) !== textOf(previous) && textOf(value) !== suggested) {
      durationTouched.value = true;
    }
  },
);

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<LeaveDetail | null>(null);
/** 详情上的动作态（由后端状态算，前端不自己 match） */
const detailActions = computed(() => leaveActionsOf(detail.value?.status));

function openDetail(row: LeaveListRow): void {
  detailVisible.value = true;
  detail.value = null;
  detailError.value = '';
  void loadDetail(row.id);
}

function reloadDetail(): void {
  const id = detail.value?.id;
  if (id) {
    void loadDetail(id);
  } else {
    detailVisible.value = false;
  }
}

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  try {
    detail.value = await fetchLeaveDetail(id);
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 提交 / 作废 ====================

/** 提交审批（二次确认：提交后单据锁编辑，要改得先撤回） */
async function handleSubmit(row: LeaveListRow): Promise<void> {
  const confirmed = await confirmDialog(
    `提交「${row.leave_no}」审批？提交后单据锁定编辑，审批中如需修改要先「撤回」。`,
    '提交审批',
  );
  if (!confirmed) {
    return;
  }
  try {
    await submitLeave(row.id, {});
    MessagePlugin.success('已提交审批');
    await loadRows();
    if (detailVisible.value && detail.value?.id === row.id) {
      void loadDetail(row.id);
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

const cancelVisible = ref(false);
const cancelling = ref(false);
const cancelTarget = ref<LeaveListRow | null>(null);
const cancelReason = ref('');
const cancelError = ref('');

/** 弹窗标题 / 说明 / 确认按钮：随「这条单走哪条作废路」变 */
const cancelDialogHeader = computed(() => `${cancelActionLabel(cancelTarget.value?.status)}请假单`);
const cancelConfirmText = computed(() => cancelActionLabel(cancelTarget.value?.status) || '确定');
const cancelLead = computed(() => {
  const row = cancelTarget.value;
  if (!row) {
    return '';
  }
  switch (cancelActionOfSafe(row.status)) {
    case 'DELETED':
      return `删除草稿「${row.leave_no}」：软删除后不再出现在列表里（草稿从未提交，不影响考勤）。`;
    case 'WITHDRAWN':
      return `撤回「${row.leave_no}」：会终止正在跑的审批并退回草稿，之后可修改再提交。`;
    case 'CANCELLED':
      return `销假 / 作废「${row.leave_no}」：这张单已批准并可能已进考勤，作废后考勤口径要人工核对（请写清原因）。`;
    default:
      return '';
  }
});

function cancelActionOfSafe(status: string): string | null {
  return leaveActionsOf(status).cancelAction;
}

function openCancel(row: LeaveListRow): void {
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelError.value = '';
  cancelVisible.value = true;
}

async function handleCancelConfirm(): Promise<void> {
  const row = cancelTarget.value;
  if (!row || cancelling.value) {
    return;
  }
  const error = validateCancelReason(cancelReason.value);
  if (error) {
    cancelError.value = error;
    return;
  }
  cancelling.value = true;
  try {
    const result = await cancelLeave(row.id, cancelReason.value.trim());
    MessagePlugin.success(result.action_label || `${cancelActionLabel(row.status)}成功`);
    cancelVisible.value = false;
    if (detailVisible.value && detail.value?.id === row.id) {
      if (result.action === 'DELETED') {
        detailVisible.value = false;
      } else {
        void loadDetail(row.id);
      }
    }
    await loadRows();
  } catch (error) {
    cancelError.value = messageOf(error);
  } finally {
    cancelling.value = false;
  }
}

/** 二次确认（TDesign 的 DialogPlugin 是命令式，返回 Promise） */
async function confirmDialog(content: string, title: string): Promise<boolean> {
  const { DialogPlugin } = await import('tdesign-vue-next');
  return new Promise((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: title,
      body: content,
      theme: 'warning',
      onConfirm: () => {
        dialog.hide();
        resolve(true);
      },
      onClose: () => {
        dialog.hide();
        resolve(false);
      },
    });
  });
}

// ==================== 首次加载 ====================

onMounted(() => {
  // `useListFilterMemory` 在 setup 阶段已恢复 tab / 筛选 / 页码 —— 这里用恢复后的
  // `activeTab` 与 `currentPage`，否则「记住我的 tab / 第 2 页」永远回到默认。
  void loadRows();
});
</script>

<style scoped lang="less">
.hr-leave-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  flex: 0 0 auto;
}

.page-tabs {
  margin-bottom: 8px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
}

.filter-row__keyword {
  width: 220px;
}

.filter-row__dept {
  width: 190px;
}

.filter-row__select {
  width: 165px;
}

.filter-row__range {
  width: 260px;
}

.filter-row__spacer {
  flex: 1 1 auto;
}

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.panel-head__meta {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 13px;
}

.list-alert {
  margin-bottom: 12px;
}

.emp-name {
  margin-right: 6px;
}

.emp-no {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.range-text {
  font-size: 12px;
}

.op-row {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, 'SFMono-Regular', Consolas, monospace);
}

.muted {
  color: var(--td-text-color-placeholder, #0000004d);
}

.panel-empty {
  padding: 40px 0;
  text-align: center;
}

.panel-empty__icon {
  font-size: 32px;
  color: var(--td-text-color-placeholder, #0000004d);
}

.panel-empty__title {
  margin: 8px 0 4px;
  font-weight: 600;
}

.panel-empty__desc {
  margin: 0;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 13px;
}

.drawer {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 4px 2px 24px;
}

.drawer--loading {
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 80px 0;
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.drawer-head__title {
  font-size: 16px;
  font-weight: 600;
}

.drawer-head__no {
  color: var(--td-text-color-secondary, #00000099);
}

.drawer-head__meta {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 13px;
}

.leave-form {
  :deep(.t-form__item) {
    margin-bottom: 14px;
  }
}

.w-full {
  width: 100%;
}

.field-hint {
  margin-top: 4px;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
  line-height: 1.5;
}

.field-error {
  margin-top: 4px;
  color: var(--td-error-color, #d54941);
  font-size: 12px;
}

.self-employee {
  font-weight: 600;
}

.form-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding-top: 8px;
  border-top: 1px solid var(--td-component-stroke, #e7e7e7);
}

.detail-desc {
  margin-top: 4px;
}

.cancel-note {
  margin: 4px 0;
}

.section-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-top: 8px;
}

.section-head__title {
  font-weight: 600;
}

.section-head__meta {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.approval-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.approval-timeline {
  padding-left: 4px;
}

.task-line {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.task-line__node {
  font-weight: 600;
}

.task-line__assignee {
  color: var(--td-text-color-secondary, #00000099);
}

.task-comment {
  margin-top: 4px;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.approval-result {
  margin-top: 8px;
}

.empty-block {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 20px 0;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 13px;
}

.cancel-body__lead {
  margin: 0 0 10px;
  line-height: 1.6;
}
</style>
