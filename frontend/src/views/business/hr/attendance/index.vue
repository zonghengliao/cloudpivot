<!--
  考勤管理页（ARCH-9 §6.1 页面 #2 · §6.2.3 · HR-29）。

  ```
  两个 tab：考勤期间（批次列表 + 新建 / 提交 / 锁定 / 解锁 / 重算）
           考勤台账（期间切换 + 一人一行 + 单元格编辑 + 导入）
  ```

  # 数据来源（全部走 `@/api/business/hr/attendance`）

  | 块 | 接口 | 说明 |
  |---|---|---|
  | 期间列表 | `GET /attendance/period/page` | 期间 tab 与台账下拉共用一份数据 |
  | 期间维护 | `POST/PUT/DELETE /attendance/period[...]` + `POST .../recalc` | HR-19 补的三条（改 / 删 / 重算） |
  | 状态流转 | `POST /attendance/period/{id}/status` | 提交 / 锁定 / 解锁（解锁 ≥5 字原因） |
  | 台账 | `GET /attendance/record/page` | `period_id` 必填，一人一行 |
  | 单元格编辑 | `PUT /attendance/record/{id}` | **部分更新**：一次只发一列 |
  | 导入 | impexp 通用三段（`biz_type='hr.attendance'`） | 见 `attendance.ts` 文件头：HR 专属路由尚未落地 |

  # 五条与「想当然」不一样的交互（每条都有真接口依据）

  1. **单元格是「失焦保存」，不是「点保存按钮」**（后端 DTO 就是为它设计的：一次只发一个
     字段、值没变回 `changed=false` 不写库）。所以每个输入框自带 loading，失败时把值
     回滚成库里的数，不让人以为改成功了。
  2. **能不能改看 `row.editable`，不看 `row.period_status`**：后端按
     `ATTENDANCE_PERIOD_LOCKED` 算好下发（`LOCKED` 才 false）。前端再 `match` 一遍状态串
     就是第二份真相。
  3. **锁定要先把期间提交**（状态机三条边：`DRAFT→SUBMITTED→LOCKED`，`LOCKED→DRAFT`）：
     草稿行上不出现「锁定」按钮，而不是点了 400。
  4. **解锁必须填 ≥5 字原因**（后端 `parse_status` 在状态分流前校验）：对话框里没有原因
     就禁用确认按钮。
  5. **导入的考勤月来自 Excel 行，不是页面参数**：Excel「考勤月」列与所选期间不一致时，
     后端预检会带 Excel 行号拦下 —— 上传对话框里写明这一点，省掉一次白传。

  # 本页不做的（已上报）

  - **导出**（`GET /attendance/export`，设计 §5.2 #24）随 **HR-30**，本页不画导出按钮；
  - **「我的考勤」tab**（`GET /attendance/my`，设计 §6.1）随 **HR-41**，本页不做；
  - **HR 专属导入路由** `POST /attendance/import`（设计 §5.2 #23）未注册，见 `attendance.ts`。
-->
<template>
  <div class="hr-attendance-page">
    <!-- ==================== ⓞ tab ==================== -->
    <t-card :bordered="false" class="tabs-card">
      <t-tabs v-model="activeTab" data-testid="hr-att-tabs" @change="handleTabChange">
        <t-tab-panel value="period" label="考勤期间" data-testid="hr-att-tab-period" />
        <t-tab-panel value="ledger" label="考勤台账" data-testid="hr-att-tab-ledger" />
      </t-tabs>
    </t-card>

    <!-- ==================== ① 考勤期间 ==================== -->
    <template v-if="activeTab === 'period'">
      <t-card :bordered="false" class="filter-card">
        <div class="filter-row">
          <t-input
            v-model="periodFilter.keyword"
            class="filter-row__keyword"
            placeholder="考勤月（如 2026-11）"
            clearable
            data-testid="hr-att-period-keyword"
            @enter="handlePeriodSearch"
            @clear="handlePeriodSearch"
          >
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>

          <t-select
            v-model="periodFilter.status"
            class="filter-row__select"
            :options="statusFilterOptions"
            placeholder="全部状态"
            multiple
            clearable
            :min-collapsed-num="1"
            data-testid="hr-att-period-status"
          />

          <t-input
            v-model="periodFilter.year"
            class="filter-row__year"
            placeholder="年份（如 2026）"
            clearable
            data-testid="hr-att-period-year"
            @enter="handlePeriodSearch"
          />

          <t-button theme="primary" data-testid="hr-att-period-search" @click="handlePeriodSearch">查询</t-button>
          <t-button variant="outline" data-testid="hr-att-period-reset" @click="handlePeriodReset">重置</t-button>
          <t-button variant="outline" data-testid="hr-att-period-refresh" @click="loadPeriods">刷新</t-button>
          <span class="filter-row__spacer" />
          <t-button
            v-if="canPeriod"
            theme="primary"
            data-testid="hr-att-period-create"
            @click="openPeriodCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新建期间
          </t-button>
        </div>
      </t-card>

      <t-card :bordered="false" class="list-card">
        <div class="panel-head">
          <div class="panel-head__main">
            <span class="panel-head__title">考勤期间</span>
            <span class="panel-head__meta" data-testid="hr-att-period-meta">共 {{ periodTotal }} 期</span>
          </div>
        </div>

        <t-alert
          v-if="periodError"
          theme="error"
          :message="periodError"
          class="list-alert"
          data-testid="hr-att-period-error"
        >
          <template #operation>
            <t-button size="small" variant="outline" @click="loadPeriods">重试</t-button>
          </template>
        </t-alert>

        <t-table
          row-key="id"
          :data="periods"
          :columns="ATTENDANCE_PERIOD_COLUMNS"
          :loading="periodLoading"
          :pagination="periodPagination"
          hover
          data-testid="hr-att-period-table"
          @page-change="handlePeriodPageChange"
        >
          <template #period_code="{ row }">
            <t-link theme="primary" class="mono" data-testid="hr-att-period-open" @click="openLedger(row)">
              {{ row.period_code }}
            </t-link>
          </template>

          <template #range="{ row }">
            <span class="mono range-text">{{ periodRangeText(row) }}</span>
          </template>

          <template #status="{ row }">
            <t-tag
              size="small"
              variant="light"
              :theme="periodStatusTheme(row.status)"
              data-testid="hr-att-period-status-tag"
            >
              {{ row.status_label || periodStatusLabel(row.status) }}
            </t-tag>
          </template>

          <template #employee_count="{ row }">
            <span class="mono">{{ row.employee_count }}</span>
          </template>

          <template #should_days_total="{ row }">
            <span class="mono">{{ decimalText(row.should_days_total) }}</span>
          </template>

          <template #actual_days_total="{ row }">
            <span class="mono">{{ decimalText(row.actual_days_total) }}</span>
          </template>

          <template #ot_hours_total="{ row }">
            <span class="mono">{{ decimalText(row.ot_hours_total) }}</span>
          </template>

          <template #audit="{ row }">
            <span class="audit-text">{{ periodAuditText(row) }}</span>
          </template>

          <template #remark="{ row }">
            <span :class="{ muted: !row.remark }" :title="row.remark || ''">{{ textOr(row.remark) }}</span>
          </template>

          <template #op="{ row }">
            <div class="op-row">
              <t-link theme="primary" data-testid="hr-att-period-ledger" @click="openLedger(row)">台账</t-link>
              <t-link
                v-if="canPeriod && periodActionsOf(row.status).edit"
                theme="primary"
                data-testid="hr-att-period-edit"
                @click="openPeriodEdit(row)"
              >
                编辑
              </t-link>
              <t-link
                v-if="canLock && periodActionsOf(row.status).submit"
                theme="primary"
                data-testid="hr-att-period-submit"
                @click="openStatusDialog(row, 'SUBMITTED')"
              >
                提交
              </t-link>
              <t-link
                v-if="canLock && periodActionsOf(row.status).lock"
                theme="primary"
                data-testid="hr-att-period-lock"
                @click="openStatusDialog(row, 'LOCKED')"
              >
                锁定
              </t-link>
              <t-link
                v-if="canLock && periodActionsOf(row.status).unlock"
                theme="warning"
                data-testid="hr-att-period-unlock"
                @click="openStatusDialog(row, 'DRAFT')"
              >
                解锁
              </t-link>
              <t-link
                v-if="canPeriod && periodActionsOf(row.status).recalc"
                theme="default"
                data-testid="hr-att-period-recalc"
                @click="handleRecalc(row)"
              >
                重算
              </t-link>
              <t-popconfirm
                v-if="canPeriod && periodActionsOf(row.status).remove"
                theme="danger"
                :content="`删除考勤期间 ${row.period_code}？只有「期间下没有考勤记录」的草稿能删（软删，可恢复）。`"
                @confirm="handleDeletePeriod(row)"
              >
                <t-link theme="danger" data-testid="hr-att-period-delete">删除</t-link>
              </t-popconfirm>
            </div>
          </template>

          <template #empty>
            <div class="panel-empty" data-testid="hr-att-period-empty">
              <t-icon class="panel-empty__icon" name="calendar" />
              <p class="panel-empty__title">
                {{ isPeriodFiltered(periodFilter) ? '没有匹配的考勤期间' : '还没有考勤期间' }}
              </p>
              <p class="panel-empty__desc">
                {{
                  isPeriodFiltered(periodFilter)
                    ? '换个考勤月 / 状态 / 年份试试，或点「重置」看全部'
                    : '点右上「新建期间」建本月批次（考勤月 yyyy-MM + 计薪周期），建好就能进「考勤台账」导入或录数'
                }}
              </p>
            </div>
          </template>
        </t-table>
      </t-card>
    </template>

    <!-- ==================== ② 考勤台账 ==================== -->
    <template v-else>
      <t-card :bordered="false" class="filter-card">
        <div class="filter-row">
          <t-select
            v-model="recordFilter.period_id"
            class="filter-row__period"
            :options="ledgerPeriodOptions"
            :loading="ledgerPeriodLoading"
            placeholder="选择考勤期间"
            filterable
            data-testid="hr-att-ledger-period"
            @change="handlePeriodSwitch"
          />

          <t-tag
            v-if="selectedPeriod"
            size="small"
            variant="light"
            :theme="periodStatusTheme(selectedPeriod.status)"
            data-testid="hr-att-ledger-status"
          >
            {{ selectedPeriod.status_label || periodStatusLabel(selectedPeriod.status) }}
          </t-tag>

          <t-checkbox v-model="includeLocked" data-testid="hr-att-ledger-include-locked" @change="loadLedgerPeriods">
            含已锁定
          </t-checkbox>

          <DeptTreeSelect
            v-model="recordFilter.dept_id"
            class="filter-row__dept"
            placeholder="全部部门"
            data-testid="hr-att-ledger-dept"
          />

          <t-input
            v-model="recordFilter.keyword"
            class="filter-row__keyword"
            placeholder="工号 / 姓名"
            clearable
            data-testid="hr-att-ledger-keyword"
            @enter="handleRecordSearch"
            @clear="handleRecordSearch"
          >
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>

          <t-button theme="primary" data-testid="hr-att-ledger-search" @click="handleRecordSearch">查询</t-button>
          <t-button variant="outline" data-testid="hr-att-ledger-reset" @click="handleRecordReset">重置</t-button>
        </div>

        <div class="ledger-actions">
          <ImportEntryButton
            v-if="canImport"
            :biz-type="ATTENDANCE_BIZ_TYPE"
            :biz-label="ATTENDANCE_BIZ_LABEL"
            testid="hr-att-import-entry"
          />
          <t-button
            v-if="canImport"
            theme="primary"
            variant="outline"
            :disabled="!recordFilter.period_id"
            data-testid="hr-att-import-upload"
            @click="openImportDialog"
          >
            <template #icon><t-icon name="upload" /></template>
            上传考勤表
          </t-button>

          <t-button
            v-if="canPeriod && selectedPeriod && periodActionsOf(selectedPeriod.status).recalc"
            variant="outline"
            :loading="recalculating"
            data-testid="hr-att-ledger-recalc"
            @click="handleRecalc(selectedPeriod)"
          >
            重算汇总
          </t-button>

          <t-button
            v-if="canLock && selectedPeriod && periodActionsOf(selectedPeriod.status).lock"
            theme="primary"
            data-testid="hr-att-ledger-lock"
            @click="openStatusDialog(selectedPeriod, 'LOCKED')"
          >
            锁定
          </t-button>
          <t-button
            v-if="canLock && selectedPeriod && periodActionsOf(selectedPeriod.status).unlock"
            theme="warning"
            variant="outline"
            data-testid="hr-att-ledger-unlock"
            @click="openStatusDialog(selectedPeriod, 'DRAFT')"
          >
            解锁
          </t-button>

          <span class="ledger-actions__totals mono" data-testid="hr-att-ledger-totals">{{ totalsLine }}</span>
        </div>

        <t-alert
          v-if="selectedPeriod && selectedPeriod.locked"
          theme="warning"
          class="list-alert"
          data-testid="hr-att-ledger-locked-tip"
          message="本期间已锁定：台账只读（锁定期间是薪资试算的基线）。要改数据请先「解锁」（需要填写原因）。"
        />

        <t-alert
          v-if="recordError"
          theme="error"
          :message="recordError"
          class="list-alert"
          data-testid="hr-att-ledger-error"
        >
          <template #operation>
            <t-button size="small" variant="outline" @click="loadRecords">重试</t-button>
          </template>
        </t-alert>

        <t-table
          v-if="recordFilter.period_id"
          row-key="id"
          :data="records"
          :columns="ATTENDANCE_RECORD_COLUMNS"
          :loading="recordLoading"
          :pagination="recordPagination"
          hover
          data-testid="hr-att-record-table"
          @page-change="handleRecordPageChange"
        >
          <template #employee_no="{ row }">
            <span class="mono">{{ row.employee_no }}</span>
          </template>

          <template #employee_name="{ row }">
            <span class="emp-name">{{ row.employee_name }}</span>
          </template>

          <template #dept_name="{ row }">
            <span :class="{ muted: !row.dept_name }">{{ textOr(row.dept_name) }}</span>
          </template>

          <!-- 11 个可编辑数值列：失焦保存（一次只发这一列） -->
          <template v-for="field in NUMBER_FIELDS" :key="field.key" #[field.key]="{ row }">
            <div class="cell-num" :class="{ 'cell-num--locked': !row.editable }">
              <t-input-number
                v-model="cellDraft[cellKey(row, field.key)]"
                class="cell-num__input"
                theme="normal"
                size="small"
                align="right"
                :min="0"
                :max="field.max"
                :step="field.step"
                :decimal-places="field.decimals"
                :disabled="!row.editable || !canRecord"
                :title="`${field.label}（${field.unit}，最多 ${field.max}）`"
                :data-testid="`hr-att-cell-${field.key}`"
                @blur="handleCellBlur(row, field)"
              />
            </div>
          </template>

          <template #ot_hours_total="{ row }">
            <span class="mono">{{ decimalText(row.ot_hours_total) }}</span>
          </template>

          <template #source="{ row }">
            <t-tag size="small" variant="light" theme="default">
              {{ row.source_label || sourceLabel(row.source) }}
            </t-tag>
          </template>

          <template #updated="{ row }">
            <span class="audit-text">
              {{ datetimeText(row.updated_at) }}
              <template v-if="row.updated_by_name"> · {{ row.updated_by_name }}</template>
            </span>
          </template>

          <template #empty>
            <div class="panel-empty" data-testid="hr-att-record-empty">
              <t-icon class="panel-empty__icon" name="user-business" />
              <p class="panel-empty__title">
                {{ isRecordFiltered(recordFilter) ? '没有匹配的考勤记录' : '这一期还没有考勤记录' }}
              </p>
              <p class="panel-empty__desc">
                {{
                  isRecordFiltered(recordFilter)
                    ? '换个部门 / 关键字试试，或点「重置」看全部'
                    : '点上方「上传考勤表」按模板导入（考勤月列填本期月份），导入后一人一行，可直接改数'
                }}
              </p>
            </div>
          </template>
        </t-table>

        <div v-else class="panel-empty" data-testid="hr-att-record-no-period">
          <t-icon class="panel-empty__icon" name="calendar" />
          <p class="panel-empty__title">先选一期考勤期间</p>
          <p class="panel-empty__desc">
            台账按期间出数（一人一行）。还没有期间就去「考勤期间」tab 点「新建期间」，建好回来自动可选。
          </p>
        </div>

        <t-alert
          v-for="(warning, index) in recordWarnings"
          :key="index"
          theme="warning"
          class="list-alert"
          data-testid="hr-att-record-warning"
          :message="warning"
        />
      </t-card>
    </template>

    <!-- ==================== ③ 期间表单抽屉 ==================== -->
    <t-drawer
      v-model:visible="periodFormVisible"
      size="520px"
      :footer="false"
      :header="false"
      destroy-on-close
      data-testid="hr-att-period-form-drawer"
    >
      <div class="drawer">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title">{{ editingPeriodId ? '编辑考勤期间' : '新建考勤期间' }}</span>
            <t-tag v-if="editingPeriodId" size="small" variant="light" theme="default">
              {{ periodForm.period_code }}
            </t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button
              size="small"
              variant="outline"
              data-testid="hr-att-period-form-close"
              @click="periodFormVisible = false"
            >
              取消
            </t-button>
          </div>
        </div>

        <t-form :data="periodForm" label-width="96px" data-testid="hr-att-period-form">
          <t-form-item label="考勤月" required-mark>
            <t-input
              v-model="periodForm.period_code"
              class="w-full mono"
              :disabled="Boolean(editingPeriodId)"
              placeholder="yyyy-MM（如 2026-11）"
              :maxlength="PERIOD_CODE_LEN"
              data-testid="hr-att-period-form-code"
            />
            <div v-if="periodErrors.period_code" class="field-error">{{ periodErrors.period_code }}</div>
            <div v-else-if="editingPeriodId" class="field-hint">
              考勤月是期间的人工标识（报表 / 薪资 / 巡检按它对齐），建好后不能改；错了请删掉重建。
            </div>
          </t-form-item>

          <t-form-item label="起始日期" required-mark>
            <t-date-picker
              v-model="periodForm.period_start"
              class="w-full"
              allow-input
              clearable
              format="YYYY-MM-DD"
              data-testid="hr-att-period-form-start"
            />
            <div v-if="periodErrors.period_start" class="field-error">{{ periodErrors.period_start }}</div>
          </t-form-item>

          <t-form-item label="截止日期" required-mark>
            <t-date-picker
              v-model="periodForm.period_end"
              class="w-full"
              allow-input
              clearable
              format="YYYY-MM-DD"
              data-testid="hr-att-period-form-end"
            />
            <div v-if="periodErrors.period_end" class="field-error">{{ periodErrors.period_end }}</div>
            <div v-else class="field-hint">
              支持「26 日 ~ 次月 25 日」的计薪周期；跨度最长 {{ MAX_SPAN_DAYS }} 天。
            </div>
          </t-form-item>

          <t-form-item label="备注">
            <t-textarea
              v-model="periodForm.remark"
              class="w-full"
              :maxlength="PERIOD_MAX_REMARK"
              placeholder="计薪口径 / 开期说明（最多 500 字）"
              :autosize="{ minRows: 2, maxRows: 4 }"
              data-testid="hr-att-period-form-remark"
            />
            <div v-if="periodErrors.remark" class="field-error">{{ periodErrors.remark }}</div>
          </t-form-item>
        </t-form>

        <div class="form-foot">
          <t-button variant="outline" data-testid="hr-att-period-form-cancel" @click="periodFormVisible = false">
            取消
          </t-button>
          <t-button
            theme="primary"
            :loading="periodSaving"
            data-testid="hr-att-period-form-save"
            @click="handlePeriodSave"
          >
            {{ editingPeriodId ? '保存' : '创建' }}
          </t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ④ 状态流转对话框 ==================== -->
    <t-dialog
      v-model:visible="statusVisible"
      :header="statusDialogTitle"
      :confirm-btn="{
        content: statusConfirmText,
        theme: statusTarget === 'DRAFT' ? 'warning' : 'primary',
        loading: statusSaving,
        disabled: isStatusConfirmDisabled,
      }"
      data-testid="hr-att-status-dialog"
      @confirm="handleStatusConfirm"
    >
      <div class="status-body">
        <p class="status-body__lead" data-testid="hr-att-status-lead">{{ statusLead }}</p>
        <template v-if="statusTarget === 'DRAFT'">
          <t-textarea
            v-model="statusReason"
            :autosize="{ minRows: 3, maxRows: 5 }"
            :placeholder="`解锁原因：至少 ${MIN_UNLOCK_REASON} 个字（会写进状态日志）`"
            data-testid="hr-att-status-reason"
          />
          <div v-if="statusReason && validateUnlockReason(statusReason)" class="field-error">
            {{ validateUnlockReason(statusReason) }}
          </div>
        </template>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 导入对话框 ==================== -->
    <t-dialog
      v-model:visible="importVisible"
      header="上传考勤表"
      width="880px"
      :footer="false"
      data-testid="hr-att-import-dialog"
    >
      <div class="import-body">
        <t-alert theme="info" class="import-body__tip">
          模板 15 列，<b>「考勤月」列必须填本页选中的期间（{{ selectedPeriod?.period_code || 'yyyy-MM' }}）</b>，
          「工号」必须在员工档案里存在且在职；一人一月一行（同一人重复行按「更新」处理）。
          上传后先预检、再确认入库 —— 预检有错行时不会写库。
        </t-alert>

        <!-- 步骤一：选文件 -->
        <template v-if="!importUpload">
          <div class="import-body__actions">
            <t-button variant="outline" :loading="templateLoading" data-testid="hr-att-import-template" @click="handleDownloadTemplate">
              <template #icon><t-icon name="download" /></template>
              下载导入模板
            </t-button>
            <input
              ref="fileInput"
              type="file"
              accept=".xlsx,.xls"
              class="import-body__file"
              data-testid="hr-att-import-file"
              @change="handleFilePicked"
            />
            <t-button variant="outline" data-testid="hr-att-import-pick" @click="pickFile">选择文件</t-button>
            <span class="import-body__file-name">
              {{ importFile ? importFile.name : '未选择文件（.xlsx / .xls）' }}
            </span>
            <t-button
              theme="primary"
              :disabled="!importFile"
              :loading="uploading"
              data-testid="hr-att-import-upload-submit"
              @click="handleUpload"
            >
              上传并预检
            </t-button>
          </div>
          <t-alert v-if="importError" theme="error" :message="importError" class="import-body__tip" data-testid="hr-att-import-error" />
        </template>

        <!-- 步骤二：预检报告 -->
        <template v-else>
          <div class="import-report" data-testid="hr-att-import-report">
            <span class="import-report__item">任务 <b class="mono">{{ importUpload.task_no }}</b></span>
            <span class="import-report__item">数据行 <b>{{ importReport?.total ?? 0 }}</b></span>
            <span class="import-report__item import-report__item--ok">通过 <b>{{ importReport?.valid ?? 0 }}</b></span>
            <span class="import-report__item" :class="{ 'import-report__item--bad': (importReport?.error ?? 0) > 0 }">
              失败 <b>{{ importReport?.error ?? 0 }}</b>
            </span>
            <span v-if="importReport?.skipped_example" class="import-report__item">
              跳过示例行 <b>{{ importReport.skipped_example }}</b>
            </span>
            <span v-if="importReport?.skipped_blank" class="import-report__item">
              跳过空行 <b>{{ importReport.skipped_blank }}</b>
            </span>
          </div>

          <t-alert
            v-for="(warning, index) in importReport?.warnings ?? []"
            :key="`w-${index}`"
            theme="warning"
            class="import-body__tip"
            :message="warning.message"
          />

          <div v-if="(importReport?.errors?.length ?? 0) > 0" class="import-errors">
            <div class="import-errors__title">
              错误明细（前 {{ importReport?.errors?.length ?? 0 }} 条{{ importReport?.truncated ? '，已截断' : '' }}）
            </div>
            <t-table
              row-key="row_no"
              size="small"
              :data="importReport?.errors ?? []"
              :columns="IMPORT_ERROR_COLUMNS"
              max-height="280"
              data-testid="hr-att-import-errors"
            >
              <template #column="{ row }">
                <span>{{ row.column_header || row.column_key || '-' }}</span>
              </template>
              <template #message="{ row }">
                <span>{{ row.message || row.error_message || '-' }}</span>
              </template>
            </t-table>
          </div>

          <div class="import-body__actions">
            <t-button
              variant="outline"
              :loading="templateLoading"
              data-testid="hr-att-import-template-again"
              @click="handleDownloadTemplate"
            >
              下载模板
            </t-button>
            <t-button variant="outline" data-testid="hr-att-import-reset" @click="resetImport">重新选文件</t-button>
            <t-button
              theme="primary"
              :disabled="(importReport?.valid ?? 0) === 0"
              :loading="confirming"
              data-testid="hr-att-import-confirm"
              @click="handleConfirmImport"
            >
              确认导入（{{ importReport?.valid ?? 0 }} 行）
            </t-button>
          </div>
          <t-alert v-if="importError" theme="error" :message="importError" class="import-body__tip" />
          <t-alert
            v-if="importDone"
            theme="success"
            :message="importDone"
            class="import-body__tip"
            data-testid="hr-att-import-done"
          />
        </template>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 考勤管理页（HR-29）。
 *
 * 全部口径走 `attendance-kinds.ts` 的纯函数（可单测）：状态词表 / 动作态 / 查询序列化 /
 * 表单校验 / 数值格式化。页面只做「拉数据 + 摆界面 + 弹提示」。
 *
 * 权限码（`V0387__hr_menu_perms` 逐字一致）：
 * `business:hr:attendance:{list,period,record,import,export,lock}`。
 * 页面入口由 `router/index.ts` 的 `meta.permission` 拦；页内按钮由 `hasPermission` 拦
 * （无权限时按钮不出现，而不是点了 403）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import { ApiError } from '@/api/request';
import { saveBlob } from '@/utils/download';
import type { PrecheckReport } from '@/api/business/impexp';
import {
  ATTENDANCE_BIZ_LABEL,
  ATTENDANCE_BIZ_TYPE,
  ATTENDANCE_MAX_PAGE_SIZE,
  ATTENDANCE_PAGE_SIZE,
  ATTENDANCE_PERIOD_COLUMNS,
  ATTENDANCE_PERMS,
  ATTENDANCE_RECORD_COLUMNS,
  LEDGER_PERIOD_STATUSES,
  MAX_SPAN_DAYS,
  MIN_UNLOCK_REASON,
  NUMBER_FIELDS,
  PERIOD_CODE_LEN,
  PERIOD_MAX_REMARK,
  PERIOD_STATUS_OPTIONS,
  buildPeriodQuery,
  buildRecordQuery,
  decimalText,
  decimalValue,
  datetimeText,
  emptyPeriodFilter,
  emptyPeriodForm,
  emptyRecordFilter,
  emptyTotals,
  isPeriodFiltered,
  isRecordFiltered,
  periodActionsOf,
  periodAuditText,
  periodFormFromRow,
  periodOptionLabel,
  periodRangeText,
  periodStatusLabel,
  periodStatusTheme,
  recordPatchOf,
  sourceLabel,
  statusChangePayload,
  textOr,
  toPeriodCreatePayload,
  toPeriodUpdatePayload,
  totalsOfPeriod,
  totalsText,
  validatePeriodForm,
  validateUnlockReason,
  changePeriodStatus,
  confirmAttendanceImport,
  createPeriod,
  deletePeriod,
  downloadAttendanceTemplate,
  fetchPeriodPage,
  fetchRecordPage,
  patchRecord,
  recalcPeriod,
  updatePeriod,
  uploadAttendanceImport,
  type AttendancePeriodRow,
  type AttendanceRecordRow,
  type NumberFieldMeta,
  type PeriodForm,
  type PeriodFilter,
  type PeriodStatus,
  type PeriodTotals,
  type RecordFilter,
} from '@/api/business/hr/attendance';

const userStore = useUserStore();

/** 本页权限 */
const canPeriod = computed(() => userStore.hasPermission(ATTENDANCE_PERMS.period));
const canRecord = computed(() => userStore.hasPermission(ATTENDANCE_PERMS.record));
const canImport = computed(() => userStore.hasPermission(ATTENDANCE_PERMS.import));
const canLock = computed(() => userStore.hasPermission(ATTENDANCE_PERMS.lock));

/** 状态筛选下拉 */
const statusFilterOptions = PERIOD_STATUS_OPTIONS.map((item) => ({ ...item }));

/** 导入错误明细的列（行号 / 列 / 原因） */
const IMPORT_ERROR_COLUMNS = [
  { colKey: 'row_no', title: 'Excel 行号', width: 100 },
  { colKey: 'column', title: '列', width: 140 },
  { colKey: 'raw_value', title: '原值', width: 140, ellipsis: true },
  { colKey: 'message', title: '失败原因', width: 360, ellipsis: true },
];

/** 统一错误文案（`ApiError` 带后端中文报文，直接显示） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : '请求失败';
}

// ==================== tab ====================

const activeTab = ref<'period' | 'ledger'>('period');

function handleTabChange(): void {
  if (activeTab.value === 'ledger') {
    void loadLedgerPeriods().then(() => {
      if (!recordFilter.period_id) {
        return;
      }
      void loadRecords();
    });
  } else {
    void loadPeriods();
  }
}

// ==================== 期间列表 ====================

const periodFilter = reactive<PeriodFilter>(emptyPeriodFilter());
const periods = ref<AttendancePeriodRow[]>([]);
const periodTotal = ref(0);
const periodLoading = ref(false);
const periodError = ref('');
const periodPage = ref(1);
const periodPageSize = ref(ATTENDANCE_PAGE_SIZE);

const periodPagination = computed(() => ({
  current: periodPage.value,
  pageSize: periodPageSize.value,
  total: periodTotal.value,
  showJumper: true,
  showPageSize: false,
}));

async function loadPeriods(): Promise<void> {
  periodLoading.value = true;
  periodError.value = '';
  try {
    const data = await fetchPeriodPage(buildPeriodQuery(periodFilter, periodPage.value, periodPageSize.value));
    periods.value = data.list ?? [];
    periodTotal.value = data.total ?? 0;
  } catch (error) {
    periods.value = [];
    periodTotal.value = 0;
    periodError.value = messageOf(error);
  } finally {
    periodLoading.value = false;
  }
}

function handlePeriodSearch(): void {
  periodPage.value = 1;
  void loadPeriods();
}

function handlePeriodReset(): void {
  Object.assign(periodFilter, emptyPeriodFilter());
  periodPage.value = 1;
  void loadPeriods();
}

function handlePeriodPageChange(pageInfo: { current: number; pageSize: number }): void {
  periodPage.value = pageInfo.current;
  if (pageInfo.pageSize && pageInfo.pageSize !== periodPageSize.value) {
    periodPageSize.value = pageInfo.pageSize;
  }
  void loadPeriods();
}

// ==================== 期间表单 ====================

const periodFormVisible = ref(false);
const periodSaving = ref(false);
const editingPeriodId = ref('');
const editingPeriod = ref<AttendancePeriodRow | null>(null);
const periodForm = reactive<PeriodForm>(emptyPeriodForm());
const periodErrors = ref<Record<string, string>>({});

function openPeriodCreate(): void {
  editingPeriodId.value = '';
  editingPeriod.value = null;
  Object.assign(periodForm, emptyPeriodForm());
  periodErrors.value = {};
  periodFormVisible.value = true;
}

function openPeriodEdit(row: AttendancePeriodRow): void {
  editingPeriodId.value = row.id;
  editingPeriod.value = row;
  Object.assign(periodForm, periodFormFromRow(row));
  periodErrors.value = {};
  periodFormVisible.value = true;
}

function collectPeriodErrors(): boolean {
  const errors = validatePeriodForm(periodForm);
  const mapped: Record<string, string> = {};
  for (const item of errors) {
    mapped[item.field] = mapped[item.field] ?? item.message;
  }
  periodErrors.value = mapped;
  if (errors.length > 0) {
    MessagePlugin.error(errors[0].message);
    return false;
  }
  return true;
}

async function handlePeriodSave(): Promise<void> {
  if (periodSaving.value) {
    return;
  }
  if (!collectPeriodErrors()) {
    return;
  }
  periodSaving.value = true;
  try {
    if (editingPeriodId.value) {
      const updated = await updatePeriod(editingPeriodId.value, toPeriodUpdatePayload(periodForm));
      MessagePlugin.success(`考勤期间 ${updated.period_code} 已保存`);
    } else {
      const created = await createPeriod(toPeriodCreatePayload(periodForm));
      MessagePlugin.success(`考勤期间 ${created.period_code} 已创建（草稿）`);
    }
    periodFormVisible.value = false;
    await loadPeriods();
    if (activeTab.value === 'ledger') {
      await loadLedgerPeriods();
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    periodSaving.value = false;
  }
}

// ==================== 状态流转 ====================

const statusVisible = ref(false);
const statusSaving = ref(false);
const statusTarget = ref<PeriodStatus>('SUBMITTED');
const statusReason = ref('');
const statusPeriod = ref<AttendancePeriodRow | null>(null);

const statusDialogTitle = computed(() => {
  if (statusTarget.value === 'SUBMITTED') {
    return '提交考勤期间';
  }
  if (statusTarget.value === 'LOCKED') {
    return '锁定考勤期间';
  }
  return '解锁考勤期间';
});

const statusConfirmText = computed(() => {
  if (statusTarget.value === 'SUBMITTED') {
    return '提交';
  }
  if (statusTarget.value === 'LOCKED') {
    return '确认锁定';
  }
  return '确认解锁';
});

const statusLead = computed(() => {
  const code = statusPeriod.value?.period_code ?? '';
  if (statusTarget.value === 'SUBMITTED') {
    return `提交期间 ${code}？提交后仍可继续导入 / 改数（客户第一轮导入常有错行），下一步才是「锁定」。`;
  }
  if (statusTarget.value === 'LOCKED') {
    return `锁定期间 ${code}？锁定后考勤记录不许增删改；薪资单按锁定版本计算。要再改必须先解锁。`;
  }
  return `解锁期间 ${code}？锁定期间是薪资试算的基线，解开它会让已经算过的工资与考勤对不上，请写清原因（≥ ${MIN_UNLOCK_REASON} 字）。`;
});

/** 解锁原因不满足时禁用「确认解锁」（后端还会再校验一次，这里只为不让用户白点） */
const isStatusConfirmDisabled = computed(
  () => statusTarget.value === 'DRAFT' && Boolean(validateUnlockReason(statusReason.value)),
);

function openStatusDialog(row: AttendancePeriodRow, target: PeriodStatus): void {
  statusPeriod.value = row;
  statusTarget.value = target;
  statusReason.value = '';
  statusVisible.value = true;
}

async function handleStatusConfirm(): Promise<void> {
  const row = statusPeriod.value;
  if (!row || statusSaving.value) {
    return;
  }
  if (statusTarget.value === 'DRAFT') {
    const invalid = validateUnlockReason(statusReason.value);
    if (invalid) {
      MessagePlugin.error(invalid);
      return;
    }
  }
  statusSaving.value = true;
  try {
    const updated = await changePeriodStatus(row.id, statusChangePayload(statusTarget.value, statusReason.value));
    MessagePlugin.success(`期间 ${updated.period_code} 已${periodStatusLabel(updated.status)}`);
    statusVisible.value = false;
    await loadPeriods();
    if (activeTab.value === 'ledger') {
      await loadLedgerPeriods();
      syncSelectedPeriod();
      if (recordFilter.period_id) {
        await loadRecords();
      }
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    statusSaving.value = false;
  }
}

// ==================== 删除 / 重算 ====================

async function handleDeletePeriod(row: AttendancePeriodRow): Promise<void> {
  try {
    await deletePeriod(row.id);
    MessagePlugin.success(`考勤期间 ${row.period_code} 已删除`);
    if (recordFilter.period_id === row.id) {
      recordFilter.period_id = '';
      records.value = [];
    }
    await loadPeriods();
    if (activeTab.value === 'ledger') {
      await loadLedgerPeriods();
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

const recalculating = ref(false);

async function handleRecalc(row: AttendancePeriodRow): Promise<void> {
  if (recalculating.value) {
    return;
  }
  recalculating.value = true;
  try {
    const result = await recalcPeriod(row.id);
    totalsCache[row.id] = result.after;
    if (result.changed) {
      MessagePlugin.success(
        `汇总已重算：${result.before.employee_count} → ${result.after.employee_count} 人、应出勤 ${decimalText(
          result.before.should_days_total,
        )} → ${decimalText(result.after.should_days_total)} 天（扫描 ${result.scanned} 行）`,
      );
    } else {
      MessagePlugin.info(`汇总重算完成，数字未变（幂等：扫描 ${result.scanned} 行）`);
    }
    await loadPeriods();
    if (activeTab.value === 'ledger') {
      await loadLedgerPeriods();
      syncSelectedPeriod();
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    recalculating.value = false;
  }
}

// ==================== 台账：期间下拉 ====================

const ledgerPeriods = ref<AttendancePeriodRow[]>([]);
const ledgerPeriodLoading = ref(false);
/** 是否把已锁定期间也列进下拉（默认按设计 §6.2.3 只列草稿 / 已提交） */
const includeLocked = ref(false);

const ledgerPeriodOptions = computed(() => ledgerPeriods.value.map((row) => ({ label: periodOptionLabel(row), value: row.id })));

/** 当前选中的期间行（状态标签、锁定 / 解锁按钮、合计都看它） */
const selectedPeriod = computed<AttendancePeriodRow | null>(
  () => ledgerPeriods.value.find((row) => row.id === recordFilter.period_id) ?? null,
);

async function loadLedgerPeriods(): Promise<void> {
  ledgerPeriodLoading.value = true;
  try {
    const statuses = includeLocked.value ? [...LEDGER_PERIOD_STATUSES, 'LOCKED'] : [...LEDGER_PERIOD_STATUSES];
    const data = await fetchPeriodPage({ status: statuses.join(','), page: 1, page_size: ATTENDANCE_MAX_PAGE_SIZE });
    ledgerPeriods.value = data.list ?? [];
    // 选中项还在就保留；不在了（被删 / 被排除）就退回第一期的第一行
    if (recordFilter.period_id && !ledgerPeriods.value.some((row) => row.id === recordFilter.period_id)) {
      recordFilter.period_id = '';
    }
    if (!recordFilter.period_id && ledgerPeriods.value.length > 0) {
      recordFilter.period_id = ledgerPeriods.value[0].id;
      recordPage.value = 1;
      await loadRecords();
    }
  } catch (error) {
    ledgerPeriods.value = [];
    MessagePlugin.error(messageOf(error));
  } finally {
    ledgerPeriodLoading.value = false;
  }
}

/** 期间数据刷新后，把选中期间的最新状态同步回来（锁定 / 解锁 / 重算之后） */
function syncSelectedPeriod(): void {
  if (!recordFilter.period_id || ledgerPeriods.value.length === 0) {
    return;
  }
  if (!ledgerPeriods.value.some((row) => row.id === recordFilter.period_id)) {
    recordFilter.period_id = ledgerPeriods.value[0]?.id ?? '';
  }
}

// ==================== 台账：列表 + 单元格编辑 ====================

const recordFilter = reactive<RecordFilter>(emptyRecordFilter());
const records = ref<AttendanceRecordRow[]>([]);
const recordTotal = ref(0);
const recordLoading = ref(false);
const recordError = ref('');
const recordPage = ref(1);
const recordPageSize = ref(ATTENDANCE_PAGE_SIZE);
/** 底栏合计（期间头上的四个快照列；单行修正后由回执的 `period_totals` 刷新） */
const currentTotals = ref<PeriodTotals>(emptyTotals());
/**
 * 期间汇总缓存（`periodId → 四个快照列`）。
 *
 * 为什么不只读 `ledgerPeriods` 里的期间行：那份数据是**进页面时**拉的，而单行修正会在
 * 回执里带**同事务重算**后的 `period_totals`（比列表里的行新）。只信列表行就会出现
 * 「改完一格、切走再切回来，合计又变回旧数字」。所以修正 / 重算 / 导入后的新值都写进这份
 * 缓存，`loadRecords` 优先取缓存，没有才回落到期间行。
 */
const totalsCache = reactive<Record<string, PeriodTotals>>({});
/** 单元格输入框的草稿值（`${rowId}:${field}` → number） */
const cellDraft = reactive<Record<string, number>>({});
/** 正在保存的单元格（`${rowId}:${field}`） */
const cellSaving = ref('');
/** 上一次修正的一致性警告（设计 §4.9 W1：只报不拦） */
const recordWarnings = ref<string[]>([]);

const recordPagination = computed(() => ({
  current: recordPage.value,
  pageSize: recordPageSize.value,
  total: recordTotal.value,
  showJumper: true,
  showPageSize: false,
}));

const totalsLine = computed(() => totalsText(currentTotals.value));

/** 单元格 key（模板里 `v-model` 与保存都用它） */
function cellKey(row: AttendanceRecordRow, key: string): string {
  return `${row.id}:${key}`;
}

/** 把一行的 11 个数值灌进草稿（表格首次渲染 / 保存成功后刷新用） */
function fillDraft(row: AttendanceRecordRow): void {
  for (const field of NUMBER_FIELDS) {
    cellDraft[cellKey(row, field.key)] = decimalValue(row[field.key], field.decimals);
  }
}

async function loadRecords(): Promise<void> {
  const query = buildRecordQuery(recordFilter, recordPage.value, recordPageSize.value);
  if (!query) {
    records.value = [];
    recordTotal.value = 0;
    return;
  }
  recordLoading.value = true;
  recordError.value = '';
  recordWarnings.value = [];
  try {
    const data = await fetchRecordPage(query);
    records.value = data.list ?? [];
    recordTotal.value = data.total ?? 0;
    for (const row of records.value) {
      fillDraft(row);
    }
    // 合计优先取「修正 / 重算 / 导入」回执带回来的那份（同事务重算，比列表里的期间行新），
    // 没有缓存才回落到期间行上的四个快照列
    const periodId = recordFilter.period_id;
    const cached = totalsCache[periodId];
    if (cached) {
      currentTotals.value = cached;
    } else {
      const period = ledgerPeriods.value.find((item) => item.id === periodId);
      currentTotals.value = period ? totalsOfPeriod(period) : emptyTotals();
    }
  } catch (error) {
    records.value = [];
    recordTotal.value = 0;
    recordError.value = messageOf(error);
  } finally {
    recordLoading.value = false;
  }
}

/** 期间切换：页码归零 + 重查（不能复用上一期的数组） */
function handlePeriodSwitch(): void {
  recordPage.value = 1;
  records.value = [];
  recordTotal.value = 0;
  recordWarnings.value = [];
  void loadRecords();
}

function handleRecordSearch(): void {
  recordPage.value = 1;
  void loadRecords();
}

function handleRecordReset(): void {
  const periodId = recordFilter.period_id;
  Object.assign(recordFilter, emptyRecordFilter());
  recordFilter.period_id = periodId;
  recordPage.value = 1;
  void loadRecords();
}

function handleRecordPageChange(pageInfo: { current: number; pageSize: number }): void {
  recordPage.value = pageInfo.current;
  if (pageInfo.pageSize && pageInfo.pageSize !== recordPageSize.value) {
    recordPageSize.value = pageInfo.pageSize;
  }
  void loadRecords();
}

/**
 * 单元格失焦 → 只发这一列的修正。
 *
 * 三条硬要求：
 * 1. 值没变（`recordPatchOf` 回 null）→ 不发请求（后端也会回 changed=false，但白闪一次 loading）；
 * 2. 失败 → **把输入框回滚成库里的数**（否则用户以为改成功了，而库里没动）；
 * 3. 成功 → 用回执里的行覆盖本地（含 `ot_hours_total` / `updated_at`），并用回执里的
 *    `period_totals` 刷合计（后端是同事务重算的，读到的数一定自洽）。
 */
async function handleCellBlur(row: AttendanceRecordRow, field: NumberFieldMeta): Promise<void> {
  if (!row.editable || !canRecord.value) {
    return;
  }
  const key = cellKey(row, field.key);
  const payload = recordPatchOf(row, field.key, cellDraft[key]);
  if (!payload) {
    return;
  }
  if (cellSaving.value) {
    // 已有单元格在存：把这一次退回原值，让用户重试（避免两个并发的整行改写互相覆盖）
    cellDraft[key] = decimalValue(row[field.key], field.decimals);
    MessagePlugin.warning('上一个单元格还在保存，请稍后再改这一格');
    return;
  }
  cellSaving.value = key;
  try {
    const result = await patchRecord(row.id, payload);
    const index = records.value.findIndex((item) => item.id === row.id);
    if (index >= 0) {
      records.value[index] = result.record;
      fillDraft(result.record);
    }
    currentTotals.value = result.period_totals;
    totalsCache[result.record.period_id] = result.period_totals;
    recordWarnings.value = result.warnings ?? [];
    if (result.changed) {
      MessagePlugin.success('已保存');
    }
  } catch (error) {
    cellDraft[key] = decimalValue(row[field.key], field.decimals);
    MessagePlugin.error(messageOf(error));
  } finally {
    cellSaving.value = '';
  }
}

/** 期间行 → 台账 tab 并选中它（期间列表第一列与「台账」动作都走这里） */
function openLedger(row: AttendancePeriodRow): void {
  activeTab.value = 'ledger';
  if (row.status === 'LOCKED' && !includeLocked.value) {
    includeLocked.value = true;
  }
  void loadLedgerPeriods().then(() => {
    recordFilter.period_id = row.id;
    recordPage.value = 1;
    void loadRecords();
  });
}

// ==================== 导入 ====================

const importVisible = ref(false);
const importFile = ref<File | null>(null);
const fileInput = ref<HTMLInputElement>();
const uploading = ref(false);
const confirming = ref(false);
const templateLoading = ref(false);
const importError = ref('');
const importDone = ref('');
const importUpload = ref<{ task_id: string; task_no: string; status: string } | null>(null);
const importReport = ref<PrecheckReport | null>(null);

function openImportDialog(): void {
  resetImport();
  importVisible.value = true;
}

function resetImport(): void {
  importFile.value = null;
  importUpload.value = null;
  importReport.value = null;
  importError.value = '';
  importDone.value = '';
  if (fileInput.value) {
    fileInput.value.value = '';
  }
}

function pickFile(): void {
  fileInput.value?.click();
}

function handleFilePicked(event: Event): void {
  const target = event.target as HTMLInputElement;
  const file = target.files?.[0] ?? null;
  if (file) {
    importFile.value = file;
    importError.value = '';
  }
}

async function handleDownloadTemplate(): Promise<void> {
  templateLoading.value = true;
  try {
    const blob = await downloadAttendanceTemplate();
    saveBlob(blob, '考勤台账导入模板.xlsx');
    MessagePlugin.success('模板已开始下载');
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    templateLoading.value = false;
  }
}

async function handleUpload(): Promise<void> {
  if (!importFile.value || uploading.value) {
    return;
  }
  uploading.value = true;
  importError.value = '';
  try {
    const result = await uploadAttendanceImport(importFile.value);
    importUpload.value = { task_id: result.task_id, task_no: result.task_no, status: result.status };
    importReport.value = result.report;
  } catch (error) {
    importError.value = messageOf(error);
  } finally {
    uploading.value = false;
  }
}

async function handleConfirmImport(): Promise<void> {
  const task = importUpload.value;
  if (!task || confirming.value) {
    return;
  }
  confirming.value = true;
  importError.value = '';
  try {
    const result = await confirmAttendanceImport(task.task_id);
    importDone.value = `${result.message}（已入库 ${result.imported_rows} 行）`;
    MessagePlugin.success(result.message || '导入完成');
    // 导入走的是 impexp 通用三段（HR 专属导入路由尚未注册），落库后**期间头的四个汇总列
    // 不会自动重算** —— 不补这一下，台账上会出现「2 行明细、合计 0 人」的自相矛盾。
    // recalc 是幂等接口（HR-19），失败只提示不打断导入结果。
    const periodId = recordFilter.period_id;
    if (periodId) {
      try {
        const recalc = await recalcPeriod(periodId);
        totalsCache[periodId] = recalc.after;
      } catch (error) {
        MessagePlugin.warning(`导入已入库，但汇总重算失败：${messageOf(error)}（可点「重算汇总」重试）`);
      }
    }
    await loadLedgerPeriods();
    syncSelectedPeriod();
    if (periodId) {
      await loadRecords();
    }
    await loadPeriods();
  } catch (error) {
    importError.value = messageOf(error);
  } finally {
    confirming.value = false;
  }
}

// ==================== 首次加载 ====================

onMounted(() => {
  void loadPeriods();
  void loadLedgerPeriods();
});
</script>

<style scoped lang="less">
.hr-attendance-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tabs-card :deep(.t-card__body) {
  padding-bottom: 0;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;

  &__keyword {
    width: 200px;
  }

  &__select {
    width: 180px;
  }

  &__year {
    width: 140px;
  }

  &__period {
    width: 280px;
  }

  &__dept {
    width: 180px;
  }

  &__spacer {
    flex: 1;
  }
}

.ledger-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 12px;

  &__totals {
    margin-left: auto;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;

  &__main {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }

  &__title {
    font-size: 15px;
    font-weight: 600;
  }

  &__meta {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.list-alert {
  margin-bottom: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.range-text {
  font-size: 12px;
}

.audit-text {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.emp-name {
  font-weight: 500;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.op-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.cell-num {
  display: flex;
  justify-content: flex-end;

  &__input {
    width: 100%;
    min-width: 72px;
  }

  &--locked :deep(.t-input-number) {
    background: var(--td-bg-color-container-hover);
  }
}

.panel-empty {
  padding: 28px 0;
  text-align: center;

  &__icon {
    font-size: 32px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 8px 0 4px;
    font-weight: 600;
  }

  &__desc {
    margin: 0;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.drawer {
  display: flex;
  flex-direction: column;
  gap: 16px;
  height: 100%;
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;

  &__main {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  &__title {
    font-size: 16px;
    font-weight: 600;
  }
}

.form-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: auto;
  padding-top: 12px;
}

.field-error {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-error-color);
}

.field-hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.w-full {
  width: 100%;
}

.status-body {
  display: flex;
  flex-direction: column;
  gap: 10px;

  &__lead {
    margin: 0;
    font-size: 13px;
  }
}

.import-body {
  display: flex;
  flex-direction: column;
  gap: 12px;

  &__tip {
    margin: 0;
  }

  &__actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  &__file {
    display: none;
  }

  &__file-name {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.import-report {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  font-size: 13px;

  &__item--ok b {
    color: var(--td-success-color);
  }

  &__item--bad b {
    color: var(--td-error-color);
  }
}

.import-errors {
  display: flex;
  flex-direction: column;
  gap: 6px;

  &__title {
    font-size: 13px;
    font-weight: 600;
  }
}
</style>
