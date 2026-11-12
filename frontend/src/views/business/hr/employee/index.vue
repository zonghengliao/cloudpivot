<!--
  员工档案页（ARCH-9 §6.1 #1 · §6.2.1 / §6.2.2 · HR-18）。

  ```
  列表（分页 + 筛选） → 详情抽屉（基本信息 / 劳动合同时间线 / 薪资标准 / 附件面板）
  ```

  # 数据只有两个来源

  | 块 | 接口 | 说明 |
  |---|---|---|
  | 列表 | `GET /api/business/hr/employee/page` | 分页 / 关键字 / 部门 / 状态多选 / 用工形式 / 含离职 |
  | 详情 | `GET /api/business/hr/employee/{id}` | 一次给全：49 个字段 + `contracts[]` + `salary_items[]` |

  合同与薪资**不再单独请求**：详情出参已经带了两个子表（设计 §5.2 #2），
  再调 `#16` 只多一次请求、多一处「两个来源不一致」的风险。

  # 三条与「想当然」不一样的口径（都在 `employee-kinds.ts` 的纯函数里）

  1. **合同到期天数是服务端算的**（`contract_expire_in_days`，按 `+08:00` 业务日）。
     前端不用本机时钟重算：跨时区 / 用户改系统时间都会让「还有几天」飘。
  2. **敏感字段只出打码值**（`sensitive_masked.*_masked`）；明文必须用户主动点
     「查看明文」（`GET /employee/{id}/sensitive`，权限 `business:hr:employee:sensitive`），
     后端每次调用都写 `audit_logs` —— 所以**绝不在加载详情时预取**。
  3. **金额是字符串**（`Decimal` → `"8000.0000"`）；合计走整数分（`sumSalary`），
     不做浮点累加。

  # 本页能做什么（2026-11-13 补齐写入口）

  - **新建**：工具栏「新建员工」→ 分组表单（基本信息 / 联系方式 / 教育背景 / 紧急联系人 / 银行信息）；
    工号留空由后端自动取号（`EMP-0001`，取号与插入同事务，不留空号）。
  - **编辑**：详情抽屉底部「编辑」→ 同一表单预填；**身份证 / 工资卡号留空 = 不改**
    （详情只有打码值，无脑回填会把打码值写进库 —— 口径见 `employee-form.ts` 头注）。
  - **删除**：详情抽屉底部「删除」→ 输姓名二次确认 → 走后端软删。
  - 三个按钮各挂自己的权限码（`create` / `update` / `delete`），**无权限时按钮不渲染**。

  # 仍在本页之外的能力

  - 调岗 / 调薪 / 离职 / 绑定账号的入口在各自页面（离职必须走离职单：状态接口对 `RESIGNED` 直接 409）；
  - 已离职档案在本页是**只读**的（后端 `update_employee` 对 `RESIGNED` 409：历史薪资单快照对不上）。
-->
<template>
  <div class="hr-employee-page">
    <!-- ==================== ① 筛选行 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="工号 / 姓名 / 手机"
          clearable
          data-testid="hr-emp-keyword"
          @enter="handleSearch"
          @clear="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <DeptTreeSelect
          v-model="filter.dept_id"
          class="filter-row__dept"
          placeholder="全部部门"
          data-testid="hr-emp-dept"
        />

        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          multiple
          clearable
          :min-collapsed-num="1"
          data-testid="hr-emp-status"
        />

        <t-select
          v-model="filter.employment_type"
          class="filter-row__select"
          :options="employmentTypeOptions"
          placeholder="全部用工形式"
          clearable
          data-testid="hr-emp-employment"
        />

        <label class="filter-row__switch" data-testid="hr-emp-resigned-wrap">
          <t-switch v-model="filter.include_resigned" size="small" data-testid="hr-emp-resigned" />
          <span class="filter-row__switch-text">含离职</span>
        </label>

        <t-button theme="primary" data-testid="hr-emp-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="hr-emp-reset" @click="handleReset">重置</t-button>

        <span class="filter-row__spacer" />

        <span class="filter-row__hint">默认不含离职（勾「含离职」才查得到）</span>
        <t-button variant="outline" data-testid="hr-emp-refresh" @click="loadRows">刷新</t-button>
      </div>

      <ListMemoryBar
        :restored="memory.restored.value"
        :text="memory.restoredText.value"
        testid="hr-emp-memory"
        @clear="handleClearMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">员工档案</span>
          <span class="panel-head__meta" data-testid="hr-emp-meta">共 {{ total }} 人</span>
        </div>
        <div class="panel-head__actions">
          <span
            v-if="!canCreate"
            class="panel-head__hint"
            title="所需权限码：business:hr:employee:create"
            data-testid="hr-emp-create-denied"
          >
            无建档权限，需要建档请联系人事管理员
          </span>
          <t-button
            v-if="canCreate"
            theme="primary"
            data-testid="hr-emp-create"
            @click="openCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新建员工
          </t-button>
        </div>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert" data-testid="hr-emp-list-error">
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
        data-testid="hr-emp-table"
        @page-change="handlePageChange"
      >
        <template #emp_no="{ row }">
          <t-link theme="primary" class="mono" data-testid="hr-emp-row-open" @click="openDetail(row)">
            {{ row.emp_no }}
          </t-link>
        </template>

        <template #name="{ row }">
          <span class="emp-name">{{ row.name }}</span>
          <t-tooltip v-if="row.username" :content="`已绑定账号：${row.username}`" placement="top-left">
            <t-icon name="user" size="14px" class="emp-name__link-icon" data-testid="hr-emp-bound" />
          </t-tooltip>
          <t-tooltip v-else content="未绑定登录账号（此人看不到自己的档案与薪资）" placement="top-left">
            <span class="emp-name__unbound" data-testid="hr-emp-unbound">未绑</span>
          </t-tooltip>
        </template>

        <template #dept_name="{ row }">
          <span>{{ textOr(row.dept_name) }}</span>
        </template>

        <template #position_name="{ row }">
          <span :class="{ muted: !row.position_name }">{{ textOr(row.position_name) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="statusTheme(row.status)" variant="light" data-testid="hr-emp-status-tag">
            {{ row.status_label || statusLabel(row.status) }}
          </t-tag>
        </template>

        <template #hire_date="{ row }">
          <span class="mono">{{ row.hire_date }}</span>
        </template>

        <template #contract_end_date="{ row }">
          <template v-if="row.contract_end_date">
            <span class="mono" :class="contractExpireClass(row.contract_expire_in_days)">{{ row.contract_end_date }}</span>
            <span
              v-if="row.contract_expire_in_days !== null"
              class="expire-note"
              :class="contractExpireClass(row.contract_expire_in_days)"
            >
              {{ contractExpireText(row.contract_expire_in_days) }}
            </span>
          </template>
          <span v-else class="muted" data-testid="hr-emp-no-contract">无合同</span>
        </template>

        <template #mobile="{ row }">
          <span class="mono" :class="{ muted: !row.mobile }">{{ textOr(row.mobile) }}</span>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="hr-emp-row-detail" @click="openDetail(row)">详情</t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="hr-emp-empty">
            <t-icon class="panel-empty__icon" name="user-business" />
            <p class="panel-empty__title">{{ isEmployeeFiltered(filter) ? '没有匹配的员工' : '还没有员工档案' }}</p>
            <p class="panel-empty__desc">
              {{
                isEmployeeFiltered(filter)
                  ? '换个关键词 / 部门 / 状态试试，或点「重置」看全部'
                  : '员工档案由人事在「员工档案」里建档；已有登录账号的可从系统用户批量生成（seed-from-users）'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="900px"
      :footer="false"
      :header="false"
      destroy-on-close
      data-testid="hr-emp-drawer"
    >
      <div v-if="detailLoading" class="drawer drawer--loading" data-testid="hr-emp-detail-loading">
        <t-loading size="large" />
        <span class="muted">加载员工档案…</span>
      </div>

      <div v-else-if="detailError" class="drawer">
        <t-alert theme="error" :message="detailError" data-testid="hr-emp-detail-error">
          <template #operation>
            <t-button size="small" variant="outline" @click="reloadDetail">重试</t-button>
          </template>
        </t-alert>
      </div>

      <div v-else-if="detail" class="drawer" data-testid="hr-emp-detail">
        <!-- 头部：姓名 + 工号 + 状态 + 部门/岗位 -->
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title">{{ detail.name }}</span>
            <span class="mono drawer-head__no">{{ detail.emp_no }}</span>
            <t-tag size="small" :theme="statusTheme(detail.status)" data-testid="hr-emp-detail-status">
              {{ detail.status_label || statusLabel(detail.status) }}
            </t-tag>
            <span class="drawer-head__meta">
              {{ textOr(detail.dept_name) }} · {{ textOr(detail.position_name) }} · {{ textOr(detail.employment_type_label) }}
            </span>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="hr-emp-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <t-tabs v-model="activeTab" class="detail-tabs">
          <!-- ---------- ① 基本信息 ---------- -->
          <t-tab-panel value="basic" label="基本信息">
            <div class="section-title">身份与联系</div>
            <div class="field-grid">
              <div class="field">
                <span class="field__label">工号</span>
                <span class="field__value mono">{{ detail.emp_no }}</span>
              </div>
              <div class="field">
                <span class="field__label">姓名</span>
                <span class="field__value">{{ detail.name }}</span>
              </div>
              <div class="field">
                <span class="field__label">性别</span>
                <span class="field__value">{{ textOr(detail.gender_label) }}</span>
              </div>
              <div class="field">
                <span class="field__label">出生日期</span>
                <span class="field__value mono">{{ textOr(detail.birth_date) }}</span>
              </div>
              <div class="field">
                <span class="field__label">证件类型</span>
                <span class="field__value">{{ textOr(detail.id_card_type_label) }}</span>
              </div>
              <div class="field">
                <span class="field__label">手机</span>
                <span class="field__value mono">{{ textOr(detail.mobile) }}</span>
              </div>
              <div class="field">
                <span class="field__label">邮箱</span>
                <span class="field__value">{{ textOr(detail.email) }}</span>
              </div>
              <div class="field">
                <span class="field__label">登录账号</span>
                <span class="field__value">
                  <template v-if="detail.user_id">{{ textOr(detail.username) }}</template>
                  <span v-else class="muted">未绑定（此人看不到自己的档案与薪资）</span>
                </span>
              </div>
            </div>

            <!-- 敏感字段：只显示打码值；明文要显式点（写审计日志） -->
            <div class="section-title">
              敏感信息
              <span class="section-title__hint">只显示打码值；查看明文会记入审计日志</span>
            </div>
            <div class="field-grid">
              <div class="field">
                <span class="field__label">身份证号</span>
                <div class="field__value field__value--with-action">
                  <span class="mono" data-testid="hr-emp-id-card">
                    {{ plain.id_card ?? textOr(detail.sensitive_masked.id_card_masked, '未登记') }}
                  </span>
                  <t-button
                    v-if="detail.sensitive_masked.id_card_masked && canViewPlain"
                    size="small"
                    variant="text"
                    theme="primary"
                    :loading="plainLoading === 'id_card'"
                    data-testid="hr-emp-id-card-plain"
                    @click="loadPlain('id_card')"
                  >
                    查看明文
                  </t-button>
                </div>
              </div>
              <div class="field">
                <span class="field__label">工资卡号</span>
                <div class="field__value field__value--with-action">
                  <span class="mono" data-testid="hr-emp-bank-account">
                    {{ plain.bank_account ?? textOr(detail.sensitive_masked.bank_account_masked, '未登记') }}
                  </span>
                  <t-button
                    v-if="detail.sensitive_masked.bank_account_masked && canViewPlain"
                    size="small"
                    variant="text"
                    theme="primary"
                    :loading="plainLoading === 'bank_account'"
                    data-testid="hr-emp-bank-account-plain"
                    @click="loadPlain('bank_account')"
                  >
                    查看明文
                  </t-button>
                </div>
              </div>
            </div>

            <div class="section-title">任职</div>
            <div class="field-grid">
              <div class="field">
                <span class="field__label">部门</span>
                <span class="field__value">{{ textOr(detail.dept_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">岗位</span>
                <span class="field__value">{{ textOr(detail.position_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">职级</span>
                <span class="field__value">{{ textOr(detail.job_level_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">用工形式</span>
                <span class="field__value">{{ textOr(detail.employment_type_label) }}</span>
              </div>
              <div class="field">
                <span class="field__label">入职日期</span>
                <span class="field__value mono">{{ detail.hire_date }}</span>
              </div>
              <div class="field">
                <span class="field__label">试用期截止</span>
                <span class="field__value mono">{{ textOr(detail.probation_end_date) }}</span>
              </div>
              <template v-if="detail.leave_date">
                <div class="field">
                  <span class="field__label">离职日期</span>
                  <span class="field__value mono">{{ detail.leave_date }}</span>
                </div>
                <div class="field">
                  <span class="field__label">离职类型</span>
                  <span class="field__value">{{ textOr(detail.leave_type_label) }}</span>
                </div>
              </template>
            </div>

            <div class="section-title">教育与紧急联系人</div>
            <div class="field-grid">
              <div class="field">
                <span class="field__label">学历</span>
                <span class="field__value">{{ textOr(detail.education_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">毕业院校</span>
                <span class="field__value">{{ textOr(detail.school) }}</span>
              </div>
              <div class="field">
                <span class="field__label">专业</span>
                <span class="field__value">{{ textOr(detail.major) }}</span>
              </div>
              <div class="field">
                <span class="field__label">毕业日期</span>
                <span class="field__value mono">{{ textOr(detail.graduate_date) }}</span>
              </div>
              <div class="field">
                <span class="field__label">紧急联系人</span>
                <span class="field__value">{{ textOr(detail.emergency_contact) }}</span>
              </div>
              <div class="field">
                <span class="field__label">紧急联系电话</span>
                <span class="field__value mono">{{ textOr(detail.emergency_phone) }}</span>
              </div>
              <div class="field">
                <span class="field__label">与本人关系</span>
                <span class="field__value">{{ textOr(detail.emergency_relation) }}</span>
              </div>
              <div class="field">
                <span class="field__label">籍贯</span>
                <span class="field__value">{{ textOr(detail.native_place) }}</span>
              </div>
              <div class="field field--wide">
                <span class="field__label">现住址</span>
                <span class="field__value">{{ textOr(detail.address) }}</span>
              </div>
            </div>

            <div class="section-title">工资卡与备注</div>
            <div class="field-grid">
              <div class="field">
                <span class="field__label">开户行</span>
                <span class="field__value">{{ textOr(detail.bank_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">开户支行</span>
                <span class="field__value">{{ textOr(detail.bank_branch) }}</span>
              </div>
              <div class="field">
                <span class="field__label">户名</span>
                <span class="field__value">{{ textOr(detail.bank_account_name) }}</span>
              </div>
              <div class="field">
                <span class="field__label">最后修改</span>
                <span class="field__value mono">
                  {{ detail.updated_at }}<span class="muted"> · {{ textOr(detail.update_by_name) }}</span>
                </span>
              </div>
              <div class="field field--wide">
                <span class="field__label">备注</span>
                <span class="field__value">{{ textOr(detail.remark) }}</span>
              </div>
            </div>
          </t-tab-panel>

          <!-- ---------- ② 劳动合同时间线 ---------- -->
          <t-tab-panel value="contract" :label="`劳动合同（${contractCount}）`">
            <div v-if="detail.current_contract" class="current-contract" data-testid="hr-emp-current-contract">
              <div class="current-contract__main">
                <span class="current-contract__label">现行合同</span>
                <span class="mono current-contract__no">{{ detail.current_contract.contract_no }}</span>
                <t-tag size="small" :theme="contractStatusTheme(detail.current_contract.status)" variant="light">
                  {{ detail.current_contract.status_label }}
                </t-tag>
                <span class="current-contract__range mono">
                  {{ detail.current_contract.start_date }} ~ {{ textOr(detail.current_contract.end_date, '无固定期限') }}
                </span>
              </div>
              <span
                v-if="detail.current_contract.expire_in_days !== null"
                class="current-contract__expire"
                :class="contractExpireClass(detail.current_contract.expire_in_days)"
              >
                {{ contractExpireText(detail.current_contract.expire_in_days) }}
              </span>
            </div>

            <t-timeline v-if="detail.contracts.length > 0" class="timeline" data-testid="hr-emp-contract-timeline">
              <t-timeline-item
                v-for="contract in detail.contracts"
                :key="contract.id"
                :time="`签订 ${contract.sign_date} · ${contract.contract_type_label}`"
              >
                <template #dot>
                  <span class="timeline__dot" :class="`timeline__dot--${contractStatusTheme(contract.status)}`" />
                </template>

                <div class="contract-card" :class="{ 'contract-card--selected': selectedContractId === contract.id }">
                  <div class="contract-card__head">
                    <span class="mono contract-card__no" data-testid="hr-emp-contract-no">{{ contract.contract_no }}</span>
                    <t-tag size="small" :theme="contractStatusTheme(contract.status)" variant="light">
                      {{ contract.status_label }}
                    </t-tag>
                    <span class="contract-card__range mono">
                      {{ contract.start_date }} ~ {{ textOr(contract.end_date, '无固定期限') }}
                    </span>
                    <span v-if="contract.expire_in_days !== null" class="contract-card__expire" :class="contractExpireClass(contract.expire_in_days)">
                      {{ contractExpireText(contract.expire_in_days) }}
                    </span>
                    <span class="contract-card__grow" />
                    <t-link
                      theme="primary"
                      data-testid="hr-emp-contract-scan"
                      @click="selectedContractId = contract.id"
                    >
                      {{ selectedContractId === contract.id ? '正在查看扫描件' : '扫描件' }}
                    </t-link>
                  </div>

                  <div class="contract-card__meta">
                    <span>试用期 {{ contract.probation_months }} 个月</span>
                    <span v-if="contract.renewed_from_no">续签自 {{ contract.renewed_from_no }}</span>
                    <span v-if="contract.terminated_date">解除于 {{ contract.terminated_date }}</span>
                    <span v-if="contract.terminate_reason">原因：{{ contract.terminate_reason }}</span>
                  </div>
                  <div v-if="contract.remark" class="contract-card__remark">{{ contract.remark }}</div>
                </div>
              </t-timeline-item>
            </t-timeline>
            <div v-else class="empty-line" data-testid="hr-emp-contract-empty">
              暂无劳动合同记录（合同台账在合同管理里录，续签会在这里连成时间线）
            </div>

            <!-- 选中合同的扫描件（合同扫描件按合同 id 挂附件） -->
            <template v-if="selectedContract">
              <div class="section-title">
                合同扫描件 · <span class="mono">{{ selectedContract.contract_no }}</span>
              </div>
              <DocAttachmentPanel
                :key="selectedContract.id"
                :doc-type="DOC_KIND.EMPLOYEE_CONTRACT"
                :doc-id="selectedContract.id"
                :default-role="DEFAULT_ROLE_BY_DOC.EMPLOYEE_CONTRACT"
                hint="劳动合同扫描件（签字页 / 盖章页）；不传也能保存合同"
                testid-prefix="hr-emp-contract-attach"
              />
            </template>
          </t-tab-panel>

          <!-- ---------- ③ 薪资标准 ---------- -->
          <t-tab-panel value="salary" label="薪资标准">
            <div class="section-title">
              现行标准
              <span class="section-title__hint">业务日落在生效区间内的那一批（由后端标 current）</span>
            </div>
            <t-table
              v-if="currentSalaryRows.length > 0"
              row-key="item_code"
              :data="currentSalaryRows"
              :columns="salaryColumns"
              size="small"
              data-testid="hr-emp-salary-current"
            >
              <template #amount="{ row }">
                <span class="money">{{ formatSalaryAmount(row.amount) }}</span>
              </template>
              <template #range="{ row }">
                <span class="mono">{{ row.effective_from }} ~ {{ textOr(row.effective_to, '至今') }}</span>
              </template>
            </t-table>
            <div v-else class="empty-line" data-testid="hr-emp-salary-empty">
              还没有现行薪资标准（调薪后才会出现；薪资单按锁定考勤 + 现行标准计算）
            </div>

            <div v-if="currentSalaryRows.length > 0" class="salary-total" data-testid="hr-emp-salary-total">
              现行合计（{{ currentSalaryRows.length }} 项）：
              <span class="money salary-total__value">{{ sumSalary(currentSalaryRows) }}</span>
              <span class="salary-total__note">（按现行标准直接相加，不含考勤扣款 / 社保 / 个税）</span>
            </div>

            <div class="section-title">调薪历史</div>
            <t-table
              v-if="salaryHistoryRows.length > 0"
              :row-key="salaryRowKey"
              :data="salaryHistoryRows"
              :columns="salaryColumns"
              size="small"
              :pagination="{ pageSize: 10, total: salaryHistoryRows.length }"
              data-testid="hr-emp-salary-history"
            >
              <template #amount="{ row }">
                <span class="money">{{ formatSalaryAmount(row.amount) }}</span>
              </template>
              <template #range="{ row }">
                <span class="mono">{{ row.effective_from }} ~ {{ textOr(row.effective_to, '至今') }}</span>
              </template>
            </t-table>
            <div v-else class="empty-line">没有历史标准（现行标准就是第一条）</div>
          </t-tab-panel>

          <!-- ---------- ④ 附件 ---------- -->
          <t-tab-panel value="attach" label="附件">
            <div class="section-title">
              员工附件
              <span class="section-title__hint">学历证明 / 身份证复印件 / 入职登记表等（合同扫描件在上一个标签页）</span>
            </div>
            <DocAttachmentPanel
              :doc-type="DOC_KIND.EMPLOYEE"
              :doc-id="detail.id"
              :default-role="DEFAULT_ROLE_BY_DOC.EMPLOYEE"
              hint="身份证复印件 / 学历证明 / 入职登记表；不传也能保存员工"
              testid-prefix="hr-emp-attach"
            />
          </t-tab-panel>
        </t-tabs>

        <!-- 底部操作条（sticky：不滚到最后也能点到；编辑 / 删除各挂自己的权限码） -->
        <div class="drawer-actions" data-testid="hr-emp-drawer-actions">
          <span v-if="readonlyDetail" class="drawer-actions__hint" data-testid="hr-emp-readonly">
            已离职档案只读（历史薪资单 / 考勤快照依赖建档时的值）
          </span>
          <span v-else-if="!canUpdate && !canDelete" class="drawer-actions__hint" title="所需权限码：business:hr:employee:update / delete">
            没有修改 / 删除权限，需要更正请联系人事管理员
          </span>
          <span class="drawer-actions__grow" />
          <t-button
            v-if="canDelete && !readonlyDetail"
            theme="danger"
            variant="outline"
            data-testid="hr-emp-delete"
            @click="openDelete"
          >
            <template #icon><t-icon name="delete" /></template>
            删除
          </t-button>
          <t-button
            v-if="canUpdate && !readonlyDetail"
            theme="primary"
            data-testid="hr-emp-edit"
            @click="openEdit"
          >
            <template #icon><t-icon name="edit" /></template>
            编辑
          </t-button>
          <t-button variant="outline" data-testid="hr-emp-drawer-close-bottom" @click="detailVisible = false">
            关闭
          </t-button>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ④ 新建 / 编辑表单（同一表单两个模式） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="formMode === 'create' ? '新建员工档案' : `编辑员工档案 · ${form.emp_no} ${detail?.name ?? ''}`"
      width="920px"
      placement="center"
      :confirm-btn="{ content: formMode === 'create' ? '创建档案' : '保存修改', loading: formSubmitting }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="false"
      class="emp-form-dialog"
      data-testid="hr-emp-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-alert
        v-if="formError"
        theme="error"
        :message="formError"
        class="form-alert"
        data-testid="hr-emp-form-error"
      />

      <p v-if="formMode === 'create'" class="form-lead" data-testid="hr-emp-form-lead">
        带 <b>*</b> 的是必填。工号留空由系统自动取号（<span class="mono">EMP-0001</span> 起）；
        新档统一从「试用期」开始，转正后在详情里走状态变更。
      </p>
      <p v-else class="form-lead" data-testid="hr-emp-form-lead">
        只提交改动过的字段；<b>身份证号 / 工资卡号留空 = 不修改</b>（详情里只有打码值，重填才会覆盖）。
      </p>

      <t-form label-align="top" :data="form" class="emp-form" data-testid="hr-emp-form">
        <!-- ---------- 基本信息 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-basic">
          <div class="section-title">
            基本信息
            <span class="section-title__hint">身份与任职，必填项都在这</span>
          </div>
          <div class="form-grid">
            <t-form-item label="工号" :help="formMode === 'update' ? '工号是档案身份，建好后不可改' : '留空自动取号（EMP-0001）'">
              <t-input
                v-model="form.emp_no"
                :maxlength="FIELD_MAX.emp_no"
                :disabled="formMode === 'update'"
                :placeholder="formMode === 'update' ? '' : '留空自动取号'"
                data-testid="hr-emp-form-empno"
              />
            </t-form-item>

            <t-form-item label="姓名" required-mark :status="fieldStatus('name')" :help="formErrors.name">
              <t-input
                v-model="form.name"
                :maxlength="FIELD_MAX.name"
                placeholder="如 张三"
                data-testid="hr-emp-form-name"
              />
            </t-form-item>

            <t-form-item label="性别" required-mark :status="fieldStatus('gender')" :help="formErrors.gender">
              <t-radio-group
                :value="form.gender ?? undefined"
                data-testid="hr-emp-form-gender"
                @change="handleGenderChange"
              >
                <t-radio :value="0" v-if="form.gender === 0" data-testid="hr-emp-form-gender-0">未知</t-radio>
                <t-radio :value="1" data-testid="hr-emp-form-gender-1">男</t-radio>
                <t-radio :value="2" data-testid="hr-emp-form-gender-2">女</t-radio>
              </t-radio-group>
            </t-form-item>

            <t-form-item
              label="证件类型"
              required-mark
              :status="fieldStatus('id_card_type')"
              :help="formErrors.id_card_type"
            >
              <t-select
                v-model="form.id_card_type"
                :options="idCardTypeOptions"
                placeholder="请选择"
                data-testid="hr-emp-form-idtype"
              />
            </t-form-item>

            <t-form-item label="证件号码" :status="fieldStatus('id_card')" :help="formErrors.id_card">
              <t-input
                v-model="form.id_card"
                :maxlength="FIELD_MAX.id_card"
                :placeholder="formMode === 'update' ? `留空不修改（当前 ${masked.id_card || '未登记'}）` : '选填；身份证要 18 位 + 校验位'"
                data-testid="hr-emp-form-idcard"
              />
            </t-form-item>

            <t-form-item label="出生日期" :status="fieldStatus('birth_date')" :help="formErrors.birth_date">
              <t-date-picker
                v-model="form.birth_date"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                placeholder="yyyy-MM-dd"
                clearable
                data-testid="hr-emp-form-birth"
              />
            </t-form-item>

            <t-form-item label="人事归属部门" required-mark :status="fieldStatus('dept_id')" :help="formErrors.dept_id">
              <DeptTreeSelect
                v-model="form.dept_id"
                placeholder="请选择部门"
                data-testid="hr-emp-form-dept"
                @error="(message: string) => (formError = message)"
              />
            </t-form-item>

            <t-form-item label="岗位">
              <DictSelect
                v-model="form.position_code"
                :dict-code="DICT.position"
                placeholder="不填也行"
                data-testid="hr-emp-form-position"
              />
            </t-form-item>

            <t-form-item label="职级">
              <DictSelect
                v-model="form.job_level"
                :dict-code="DICT.job_level"
                placeholder="不填也行"
                data-testid="hr-emp-form-joblevel"
              />
            </t-form-item>

            <t-form-item
              label="用工形式"
              required-mark
              :status="fieldStatus('employment_type')"
              :help="formErrors.employment_type"
            >
              <t-select
                v-model="form.employment_type"
                :options="employmentTypeOptions"
                placeholder="请选择"
                data-testid="hr-emp-form-employment"
              />
            </t-form-item>

            <t-form-item label="入职日期" required-mark :status="fieldStatus('hire_date')" :help="formErrors.hire_date">
              <t-date-picker
                v-model="form.hire_date"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                placeholder="yyyy-MM-dd"
                :disable-date="{ after: today }"
                data-testid="hr-emp-form-hire"
              />
            </t-form-item>

            <t-form-item
              label="试用期截止"
              :status="fieldStatus('probation_end_date')"
              :help="formErrors.probation_end_date || '选填；不得早于入职日期'"
            >
              <t-date-picker
                v-model="form.probation_end_date"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                placeholder="yyyy-MM-dd"
                clearable
                data-testid="hr-emp-form-probation"
              />
            </t-form-item>
          </div>
        </div>

        <!-- ---------- 联系方式 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-contact">
          <div class="section-title">
            联系方式
            <span class="section-title__hint">手机 / 邮箱为空时不影响建档</span>
          </div>
          <div class="form-grid">
            <t-form-item label="手机号" :status="fieldStatus('mobile')" :help="formErrors.mobile">
              <t-input
                v-model="form.mobile"
                :maxlength="FIELD_MAX.mobile"
                placeholder="如 13800001234"
                data-testid="hr-emp-form-mobile"
              />
            </t-form-item>
            <t-form-item label="邮箱" :status="fieldStatus('email')" :help="formErrors.email">
              <t-input
                v-model="form.email"
                :maxlength="FIELD_MAX.email"
                placeholder="如 zhangsan@example.com"
                data-testid="hr-emp-form-email"
              />
            </t-form-item>
            <t-form-item label="籍贯">
              <t-input v-model="form.native_place" :maxlength="FIELD_MAX.native_place" placeholder="如 江苏南京" data-testid="hr-emp-form-native" />
            </t-form-item>
            <t-form-item label="现住址">
              <t-input v-model="form.address" :maxlength="FIELD_MAX.address" placeholder="选填" data-testid="hr-emp-form-address" />
            </t-form-item>
          </div>
        </div>

        <!-- ---------- 教育背景 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-edu">
          <div class="section-title">教育背景</div>
          <div class="form-grid">
            <t-form-item label="学历">
              <DictSelect
                v-model="form.education"
                :dict-code="DICT.education"
                placeholder="不填也行"
                data-testid="hr-emp-form-education"
              />
            </t-form-item>
            <t-form-item label="毕业院校">
              <t-input v-model="form.school" :maxlength="FIELD_MAX.school" placeholder="选填" data-testid="hr-emp-form-school" />
            </t-form-item>
            <t-form-item label="专业">
              <t-input v-model="form.major" :maxlength="FIELD_MAX.major" placeholder="选填" data-testid="hr-emp-form-major" />
            </t-form-item>
            <t-form-item label="毕业日期" :status="fieldStatus('graduate_date')" :help="formErrors.graduate_date">
              <t-date-picker
                v-model="form.graduate_date"
                value-type="YYYY-MM-DD"
                format="YYYY-MM-DD"
                placeholder="yyyy-MM-dd"
                clearable
                :disable-date="{ after: today }"
                data-testid="hr-emp-form-graduate"
              />
            </t-form-item>
          </div>
        </div>

        <!-- ---------- 紧急联系人 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-emergency">
          <div class="section-title">紧急联系人</div>
          <div class="form-grid form-grid--three">
            <t-form-item label="联系人">
              <t-input
                v-model="form.emergency_contact"
                :maxlength="FIELD_MAX.emergency_contact"
                placeholder="选填"
                data-testid="hr-emp-form-emergency"
              />
            </t-form-item>
            <t-form-item label="联系电话">
              <t-input
                v-model="form.emergency_phone"
                :maxlength="FIELD_MAX.emergency_phone"
                placeholder="选填"
                data-testid="hr-emp-form-emergency-phone"
              />
            </t-form-item>
            <t-form-item label="与本人关系">
              <t-input
                v-model="form.emergency_relation"
                :maxlength="FIELD_MAX.emergency_relation"
                placeholder="如 配偶 / 父母"
                data-testid="hr-emp-form-emergency-relation"
              />
            </t-form-item>
          </div>
        </div>

        <!-- ---------- 银行信息 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-bank">
          <div class="section-title">
            工资卡
            <span class="section-title__hint">发薪用；卡号在详情里只显示打码值</span>
          </div>
          <div class="form-grid">
            <t-form-item label="开户行">
              <t-input v-model="form.bank_name" :maxlength="FIELD_MAX.bank_name" placeholder="如 招商银行" data-testid="hr-emp-form-bank" />
            </t-form-item>
            <t-form-item label="开户支行">
              <t-input v-model="form.bank_branch" :maxlength="FIELD_MAX.bank_branch" placeholder="如 北京中关村支行" data-testid="hr-emp-form-branch" />
            </t-form-item>
            <t-form-item label="卡号" :status="fieldStatus('bank_account')" :help="formErrors.bank_account">
              <t-input
                v-model="form.bank_account"
                :maxlength="FIELD_MAX.bank_account"
                :placeholder="formMode === 'update' ? `留空不修改（当前 ${masked.bank_account || '未登记'}）` : '选填'"
                data-testid="hr-emp-form-bank-account"
              />
            </t-form-item>
            <t-form-item label="户名">
              <t-input
                v-model="form.bank_account_name"
                :maxlength="FIELD_MAX.bank_account_name"
                placeholder="一般与姓名一致"
                data-testid="hr-emp-form-bank-name"
              />
            </t-form-item>
          </div>
        </div>

        <!-- ---------- 备注 ---------- -->
        <div class="form-section" data-testid="hr-emp-form-section-remark">
          <div class="section-title">备注</div>
          <t-form-item label="备注">
            <t-textarea
              v-model="form.remark"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="选填：入职背景 / 特殊情况说明"
              data-testid="hr-emp-form-remark"
            />
          </t-form-item>
        </div>
      </t-form>
    </t-dialog>

    <!-- ==================== ⑤ 删除二次确认（输姓名） ==================== -->
    <t-dialog
      v-model:visible="deleteVisible"
      theme="danger"
      header="删除员工档案"
      width="520px"
      :confirm-btn="{ content: '确认删除', theme: 'danger', loading: deleteSubmitting, disabled: !deleteMatched }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="false"
      data-testid="hr-emp-delete-dialog"
      @confirm="submitDelete"
      @close="closeDelete"
    >
      <p class="delete-lead">
        即将删除 <b>{{ detail?.name }}</b>（<span class="mono">{{ detail?.emp_no }}</span>）的档案。
      </p>
      <ul class="delete-notes">
        <li>后端是<b>软删</b>：档案从列表消失，但历史薪资单 / 考勤台账里的引用还在（工号与身份证的唯一键会释放）。</li>
        <li>想保留档案只停职 → 用状态变更（停职），不要删。</li>
      </ul>
      <p class="delete-confirm-label">
        请输入姓名 <b class="mono">{{ detail?.name }}</b> 确认：
      </p>
      <t-input
        v-model="deleteConfirmText"
        :placeholder="`输入 ${detail?.name ?? ''} 以确认`"
        data-testid="hr-emp-delete-confirm"
      />
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 员工档案页（HR-18）。
 *
 * 列表 + 详情抽屉两件事，全部逻辑走 `employee-kinds.ts` 的纯函数（可单测）：
 * 状态配色 / 合同到期文案 / 筛选序列化 / 薪资合计 / 明文解析。
 *
 * 权限码（`V0387__hr_menu_perms` 逐字一致）：
 * `business:hr:employee:list`（页面）/ `:detail`（详情）/ `:sensitive`（查看明文）。
 * 页面入口由 `router/index.ts` 的 `meta.permission` 拦；抽屉里的「查看明文」
 * 由 `v-if="canViewPlain"` 拦（无权限时按钮不出现，而不是点了 403）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import {
  EMPLOYEE_COLUMNS,
  EMPLOYEE_DICT_CODES,
  EMPLOYEE_FIELD_MAX,
  EMPLOYEE_PAGE_SIZE,
  EMPLOYMENT_TYPE_OPTIONS,
  ID_CARD_TYPE_OPTIONS,
  STATUS_VALUES,
  STATUS_LABELS,
  buildEmployeeQuery,
  contractExpireClass,
  contractExpireText,
  contractStatusTheme,
  createEmployee,
  currentSalaries,
  deleteEmployee,
  emptyEmployeeFilter,
  fetchEmployeeDetail,
  fetchEmployeePage,
  fetchEmployeeSensitive,
  formatSalaryAmount,
  isEmployeeFiltered,
  plainValueOf,
  salaryHistory,
  statusLabel,
  statusTheme,
  sumSalary,
  textOr,
  updateEmployee,
  type EmployeeDetail,
  type EmployeeListRow,
} from '@/api/business/hr/employee';
import {
  buildCreatePayload,
  buildUpdatePayload,
  businessToday,
  emptyEmployeeForm,
  errorCount,
  formFromDetail,
  isDeleteConfirmed,
  isEmptyUpdate,
  isReadonlyEmployee,
  maskedHint,
  validateEmployeeForm,
  type EmployeeFormErrors,
  type EmployeeFormMode,
  type EmployeeFormState,
} from './employee-form';
import { ApiError } from '@/api/request';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import { useListFilterMemory } from '@/composables/useListFilterMemory';

/** 本页权限码（与 `V0387__hr_menu_perms` 逐字一致） */
const PERM = {
  list: 'business:hr:employee:list',
  detail: 'business:hr:employee:detail',
  sensitive: 'business:hr:employee:sensitive',
  create: 'business:hr:employee:create',
  update: 'business:hr:employee:update',
  delete: 'business:hr:employee:delete',
} as const;

/** 表单字段长度上限（与后端 `MAX_*` 逐条一致，见 `employee-kinds.ts`） */
const FIELD_MAX = EMPLOYEE_FIELD_MAX;

/** 建档表单的字典编码（岗位 / 职级 / 学历） */
const DICT = EMPLOYEE_DICT_CODES;

/** 表格列（`colKey` 与后端出参同名） */
const columns = EMPLOYEE_COLUMNS;

/** 状态下拉（码值来自 DDL 的 CHECK，中文名逐字对齐后端 `STATUS_LABELS`） */
const statusOptions = STATUS_VALUES.map((value) => ({ value, label: STATUS_LABELS[value] }));

/** 用工形式下拉 */
const employmentTypeOptions = EMPLOYMENT_TYPE_OPTIONS.map((item) => ({ ...item }));

/** 薪资标准的列（现行 / 历史共用一份） */
const salaryColumns = [
  { colKey: 'item_name', title: '薪资项', width: 160, ellipsis: true },
  { colKey: 'amount', title: '月标准金额', width: 140, align: 'right' as const },
  { colKey: 'range', title: '生效区间', width: 220 },
  { colKey: 'remark', title: '备注', ellipsis: true },
];

const userStore = useUserStore();
/** 有 `:sensitive` 权限才出现「查看明文」按钮 */
const canViewPlain = computed(() => userStore.hasPermission(PERM.sensitive));
/** 三个写按钮各挂自己的权限码：**无权限时按钮不渲染**（不是 disabled） */
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canUpdate = computed(() => userStore.hasPermission(PERM.update));
const canDelete = computed(() => userStore.hasPermission(PERM.delete));

/** 证件类型下拉（码表来自后端 `ID_CARD_TYPES`，顺序一致） */
const idCardTypeOptions = ID_CARD_TYPE_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表状态 ====================

/** 筛选行（`useListFilterMemory` 会就地恢复 / 写回） */
const filter = reactive(emptyEmployeeFilter());

/** 当前页（1 起） */
const currentPage = ref(1);
/** 每页条数 */
const pageSize = ref(EMPLOYEE_PAGE_SIZE);

/** 列表记忆：进页面恢复筛选与页码，改了防抖写回 */
const memory = useListFilterMemory({
  listName: 'hr-employee',
  filter,
  page: currentPage,
  pageSize,
});

const rows = ref<EmployeeListRow[]>([]);
const total = ref(0);
const loading = ref(false);
const loadError = ref('');

/** 分页配置（`showJumper` 便于 42 人以上的客户直接跳页） */
const pagination = computed(() => ({
  current: currentPage.value,
  pageSize: pageSize.value,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误文案（`ApiError` 带后端中文报文，其它异常兜底） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : '请求失败';
}

/** 拉列表（按当前筛选与页码） */
async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchEmployeePage(buildEmployeeQuery(filter, currentPage.value, pageSize.value));
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

/** 查询：回到第 1 页再拉 */
function handleSearch(): void {
  currentPage.value = 1;
  void loadRows();
}

/** 重置：清空筛选 + 忘掉记忆 + 回第 1 页 */
function handleReset(): void {
  Object.assign(filter, emptyEmployeeFilter());
  currentPage.value = 1;
  memory.forget();
  void loadRows();
}

/** 「清空筛选」提示条上的出口：清空筛选并重查（别让用户以为数据丢了） */
function handleClearMemory(): void {
  handleReset();
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  currentPage.value = pageInfo.current;
  if (pageInfo.pageSize && pageInfo.pageSize !== pageSize.value) {
    pageSize.value = pageInfo.pageSize;
  }
  void loadRows();
}

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<EmployeeDetail | null>(null);
/** 选中标签页（默认基本信息） */
const activeTab = ref<'basic' | 'contract' | 'salary' | 'attach'>('basic');
/** 正在看扫描件的合同 id（默认取现行合同） */
const selectedContractId = ref<string>('');

/** 敏感字段明文（用户点了才有；不预取） */
const plain = reactive<{ id_card: string | null; bank_account: string | null }>({
  id_card: null,
  bank_account: null,
});
/** 正在取明文的字段（按钮 loading） */
const plainLoading = ref<'' | 'id_card' | 'bank_account'>('');

/** 现行薪资标准（`current=true`） */
const currentSalaryRows = computed(() => currentSalaries(detail.value?.salary_items));
/** 合同份数（标签页标题用；老实例可能不给数组，兜成 0） */
const contractCount = computed(() => (detail.value?.contracts ?? []).length);
/** 历史薪资标准（非现行） */
const salaryHistoryRows = computed(() => salaryHistory(detail.value?.salary_items));
/** 历史表行键：同一薪资项会因多次调薪出现多行，单靠 `item_code` 会撞键 */
function salaryRowKey(row: { item_code: string; effective_from: string }): string {
  return `${row.item_code}@${row.effective_from}`;
}
/** 正在看扫描件的合同对象 */
const selectedContract = computed(
  () => (detail.value?.contracts ?? []).find((item) => item.id === selectedContractId.value) ?? null,
);

/** 打开详情（行点击 / 工号链接） */
function openDetail(row: EmployeeListRow): void {
  detailVisible.value = true;
  detail.value = null;
  detailError.value = '';
  activeTab.value = 'basic';
  plain.id_card = null;
  plain.bank_account = null;
  selectedContractId.value = '';
  void loadDetail(row.id);
}

/** 重新加载详情（错误态「重试」） */
function reloadDetail(): void {
  const id = detail.value?.id;
  if (id) {
    void loadDetail(id);
  } else if (detailVisible.value) {
    detailVisible.value = false;
    MessagePlugin.info('请重新点开这位员工的详情');
  }
}

/** 拉详情（一次给全：字段 + 合同 + 薪资） */
async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  try {
    const data = await fetchEmployeeDetail(id);
    detail.value = data;
    // 默认展开现行合同（没有就退到第一份），让「扫描件」一进来就有落点
    selectedContractId.value = data.current_contract?.id ?? data.contracts[0]?.id ?? '';
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

/**
 * 查看敏感字段明文。
 *
 * 后端**每次调用都写 `audit_logs`**（设计 §5.2 #7），所以这里不做缓存、不预取：
 * 用户点一次 = 一次审计记录。成功后提示「已记入审计日志」，失败按后端报文提示。
 */
async function loadPlain(field: 'id_card' | 'bank_account'): Promise<void> {
  const id = detail.value?.id;
  if (!id || plainLoading.value) {
    return;
  }
  plainLoading.value = field;
  try {
    const response = await fetchEmployeeSensitive(id, field);
    const value = plainValueOf(response, field);
    if (!value) {
      MessagePlugin.warning('接口没有返回明文（字段可能未登记）');
      return;
    }
    plain[field] = value;
    MessagePlugin.success('本次查看已记入审计日志');
  } catch (error) {
    // 接口未开放（HR-13 尚未落地时是 404）不该把「请求的资源不存在」这种技术文案
    // 甩给 HR；打码值始终在，页面照常可用。
    if (error instanceof ApiError && error.status === 404) {
      MessagePlugin.warning('查看明文功能暂不可用，打码值照常显示；如需核对请联系系统管理员');
    } else {
      MessagePlugin.error(messageOf(error));
    }
  } finally {
    plainLoading.value = '';
  }
}

// ==================== 新建 / 编辑表单 ====================

const formVisible = ref(false);
/** 当前表单模式（新建 / 编辑共用一套控件与校验） */
const formMode = ref<EmployeeFormMode>('create');
/** 表单值（就地 reactive，控件直接 v-model） */
const form = reactive<EmployeeFormState>(emptyEmployeeForm());
/**
 * 编辑时的「原始表单」快照：`buildUpdatePayload` 按它做 diff，只提交改过的字段。
 * 新建时为 `null`（用 `buildCreatePayload`）。
 */
let formOriginal: EmployeeFormState | null = null;
const formErrors = ref<EmployeeFormErrors>({});
/** 提交失败的报文（对话框内 alert，比 toast 好定位：toast 3 秒就没了） */
const formError = ref('');
const formSubmitting = ref(false);

/** 业务日（`+08:00`）：入职 / 生日 / 毕业日的上限都用它，**不用本机时钟拼**（跨时区会飘一天） */
const today = businessToday();

/** 详情里两个敏感字段的打码值（编辑表单 placeholder 显示「当前 xxx」） */
const masked = computed(() => ({
  id_card: maskedHint(detail.value, 'id_card'),
  bank_account: maskedHint(detail.value, 'bank_account'),
}));

/** 已离职档案只读：后端 `update_employee` 对 `RESIGNED` 直接 409（历史快照对不上） */
const readonlyDetail = computed(() => isReadonlyEmployee(detail.value?.status));

/** 表单字段的 error 态（TDesign `t-form-item` 的 `status`） */
function fieldStatus(key: keyof EmployeeFormState): 'error' | undefined {
  return formErrors.value[key] ? 'error' : undefined;
}

/**
 * 性别单选的回写。
 *
 * `t-radio-group` 的值域是 `RadioValue`（string / number / boolean），而表单里性别是
 * `number | null`（`null` = 还没选）—— 直接 `v-model` 过不了类型检查，而且取消选择时
 * 组件给的是 `undefined`。这里显式收敛成三态：1 / 2 / 0（未知，仅老数据）/ null。
 */
function handleGenderChange(value: unknown): void {
  form.gender = typeof value === 'number' ? value : null;
}

/** 把状态写进表单（新建用空表单，编辑用详情预填） */
function applyForm(next: EmployeeFormState): void {
  Object.assign(form, next);
  formErrors.value = {};
  formError.value = '';
}

/** 打开「新建员工」 */
function openCreate(): void {
  formMode.value = 'create';
  formOriginal = null;
  applyForm(emptyEmployeeForm(today));
  formVisible.value = true;
}

/**
 * 打开「编辑」（从详情抽屉）。
 *
 * 详情必须已经加载完：编辑要预填，而且 diff 提交依赖原始快照。
 * 身份证 / 工资卡号会被置空（详情只有打码值）——「留空 = 不改」，
 * 见 `employee-form.ts` 文件头第 1 条。
 */
function openEdit(): void {
  if (!detail.value) {
    MessagePlugin.warning('档案还没加载完，稍等一下再点编辑');
    return;
  }
  formMode.value = 'update';
  const prefill = formFromDetail(detail.value);
  formOriginal = { ...prefill };
  applyForm(prefill);
  formVisible.value = true;
}

/** 关表单（取消 / ESC / 右上角关）：清掉错误态，避免下次打开还挂着红字 */
function closeForm(): void {
  formVisible.value = false;
  formErrors.value = {};
  formError.value = '';
}

/**
 * 提交表单（新建 / 编辑走同一个按钮）。
 *
 * 校验不过时**不发请求**：把错误落到对应控件上，再 toast 一条「还有 N 项要改」，
 * 用户不用满屏找红字。
 */
async function submitForm(): Promise<void> {
  if (formSubmitting.value) {
    return;
  }
  formError.value = '';
  const errors = validateEmployeeForm(form, { mode: formMode.value, today });
  formErrors.value = errors;
  if (errorCount(errors) > 0) {
    const first = Object.values(errors)[0];
    MessagePlugin.warning(`还有 ${errorCount(errors)} 项要改：${first}`);
    return;
  }

  formSubmitting.value = true;
  try {
    if (formMode.value === 'create') {
      const created = await createEmployee(buildCreatePayload(form));
      MessagePlugin.success(`已创建 ${form.name.trim()}（${created.emp_no}）`);
      closeForm();
      // 新员工按工号排序可能落在任何一页 —— 回到第 1 页再看，别让用户以为没建成
      currentPage.value = 1;
      await loadRows();
      return;
    }

    const id = detail.value?.id;
    if (!id || !formOriginal) {
      formError.value = '档案信息丢了，请关掉表单重新打开详情再编辑';
      return;
    }
    const payload = buildUpdatePayload(form, formOriginal);
    if (isEmptyUpdate(payload)) {
      MessagePlugin.info('没有改动，无需保存');
      closeForm();
      return;
    }
    await updateEmployee(id, payload);
    MessagePlugin.success('已保存');
    closeForm();
    await Promise.all([loadDetail(id), loadRows()]);
  } catch (error) {
    formError.value = messageOf(error);
  } finally {
    formSubmitting.value = false;
  }
}

// ==================== 删除（输姓名二次确认） ====================

const deleteVisible = ref(false);
const deleteConfirmText = ref('');
const deleteSubmitting = ref(false);
/** 输入的名字与档案姓名逐字一致才允许点「确认删除」 */
const deleteMatched = computed(() => isDeleteConfirmed(detail.value?.name ?? '', deleteConfirmText.value));

function openDelete(): void {
  deleteConfirmText.value = '';
  deleteVisible.value = true;
}

function closeDelete(): void {
  deleteVisible.value = false;
  deleteConfirmText.value = '';
}

/** 确认删除（后端软删：档案从列表消失，历史单据里的引用还在） */
async function submitDelete(): Promise<void> {
  const target = detail.value;
  if (!target || !deleteMatched.value || deleteSubmitting.value) {
    return;
  }
  deleteSubmitting.value = true;
  try {
    await deleteEmployee(target.id);
    MessagePlugin.success(`已删除 ${target.name}（${target.emp_no}）`);
    closeDelete();
    detailVisible.value = false;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    deleteSubmitting.value = false;
  }
}

// ==================== 首次加载 ====================

onMounted(() => {
  // `useListFilterMemory` 在 setup 阶段已恢复筛选与页码 —— 这里用 `currentPage`
  // 而不是 1，否则「记住第 2 页」永远回到第 1 页（该 composable 的头注第 1 条）。
  void loadRows();
});
</script>

<style scoped lang="less">
.hr-employee-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  flex: 0 0 auto;
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.filter-row__keyword {
  width: 240px;
}

.filter-row__dept {
  width: 200px;
}

.filter-row__select {
  width: 170px;
}

.filter-row__switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
}

.filter-row__switch-text {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.filter-row__spacer {
  flex: 1 1 auto;
}

.filter-row__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

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
  min-width: 0;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
  white-space: nowrap;
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}

.list-alert {
  margin-bottom: 8px;
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.emp-name {
  font-weight: 500;
}

.emp-name__link-icon {
  margin-left: 6px;
  color: var(--td-brand-color);
  vertical-align: middle;
}

.emp-name__unbound {
  margin-left: 6px;
  font-size: 11px;
  color: var(--td-text-color-placeholder);
  border: 1px dashed var(--td-component-stroke);
  border-radius: 3px;
  padding: 0 4px;
}

/* 合同到期：≤30 天（含已过期）橙字 —— 设计 §6.2.1 */
.is-urgent {
  color: var(--td-warning-color);
}

.expire-note {
  display: block;
  font-size: 11px;
  line-height: 16px;
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.money {
  font-family: var(--td-font-family-number, monospace);
  font-variant-numeric: tabular-nums;
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 48px 16px;
  text-align: center;
}

.panel-empty__icon {
  font-size: 40px;
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
  max-width: 460px;
  line-height: 20px;
}

/* ==================== 详情抽屉 ==================== */
.drawer {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.drawer--loading {
  align-items: center;
  justify-content: center;
  min-height: 240px;
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
  gap: 10px;
  min-width: 0;
  flex-wrap: wrap;
}

.drawer-head__title {
  font-size: 18px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.drawer-head__no {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.drawer-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.detail-tabs {
  margin-top: 4px;
}

.section-title {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin: 16px 0 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);

  &__hint {
    font-size: 12px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
  }
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 14px 20px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.field--wide {
  grid-column: 1 / -1;
}

.field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.field__value {
  font-size: 13px;
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.field__value--with-action {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.current-contract {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--td-brand-color-light);
}

.current-contract__main {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex-wrap: wrap;
}

.current-contract__label {
  font-size: 12px;
  color: var(--td-brand-color);
}

.current-contract__no {
  font-size: 13px;
  font-weight: 600;
}

.current-contract__range {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.current-contract__expire {
  font-size: 12px;
  white-space: nowrap;
}

.timeline {
  margin-top: 12px;
}

.timeline__dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--td-text-color-placeholder);

  &--success {
    background: var(--td-success-color);
  }

  &--warning {
    background: var(--td-warning-color);
  }

  &--primary {
    background: var(--td-brand-color);
  }
}

.contract-card {
  padding: 10px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  background: var(--td-bg-color-container);

  &--selected {
    border-color: var(--td-brand-color);
    box-shadow: 0 0 0 2px var(--td-brand-color-light);
  }
}

.contract-card__head {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.contract-card__no {
  font-size: 13px;
  font-weight: 600;
}

.contract-card__range {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.contract-card__expire {
  font-size: 12px;
}

.contract-card__grow {
  flex: 1 1 auto;
}

.contract-card__meta {
  display: flex;
  align-items: center;
  gap: 14px;
  flex-wrap: wrap;
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.contract-card__remark {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.empty-line {
  padding: 20px 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.salary-total {
  margin-top: 10px;
  font-size: 12px;
  color: var(--td-text-color-secondary);

  &__value {
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__note {
    color: var(--td-text-color-placeholder);
  }
}

/* ==================== 写入口（新建 / 编辑 / 删除） ==================== */

.panel-head__actions {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}

/* 无建档权限时的说明（不是 disabled 按钮，是明说为什么没有这个入口） */
.panel-head__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  max-width: 460px;
  text-align: right;
  line-height: 18px;
}

/* 详情抽屉底部操作条：sticky 贴底，滚不滚到最后都能点到 */
.drawer-actions {
  position: sticky;
  bottom: -16px;
  display: flex;
  align-items: center;
  gap: 10px;
  /* 抽屉正文左右内边距是 16px：负 margin 与它对齐，条子才会齐着抽屉边、不出横向滚动条 */
  margin: 16px -16px -16px;
  padding: 12px 16px;
  background: var(--td-bg-color-container);
  border-top: 1px solid var(--td-component-stroke);
  box-shadow: 0 -4px 12px rgb(0 0 0 / 4%);
  z-index: 2;
}

.drawer-actions__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.drawer-actions__grow {
  flex: 1 1 auto;
}

/* 表单正文自己滚（对话框主体高度交给内容，避免整页被一屏高表单撑破） */
.emp-form {
  /* 竖屏矮屏（笔记本 1000px 高）上也要给标题栏 / 提示条 / 底部按钮留位置，
     所以同时受 vh 与「视口减固定开销」两个上限约束 */
  max-height: min(62vh, calc(100vh - 260px));
  overflow-y: auto;
  padding-right: 6px;
}

.form-lead {
  margin: 0 0 12px;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
  background: var(--td-bg-color-secondarycontainer);
  border-radius: 4px;
  padding: 8px 10px;

  b {
    color: var(--td-text-color-primary);
  }
}

.form-alert {
  margin-bottom: 12px;
}

.form-section {
  padding-top: 4px;

  & + & {
    margin-top: 8px;
    border-top: 1px dashed var(--td-component-stroke);
  }
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 2px 20px;
}

.form-grid--three {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

/* 表单里的 section 标题比详情页紧凑一档 */
.form-section .section-title {
  margin: 8px 0 6px;
}

.delete-lead {
  margin: 0 0 8px;
  font-size: 14px;
  color: var(--td-text-color-primary);
}

.delete-notes {
  margin: 0 0 14px;
  padding-left: 18px;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.delete-confirm-label {
  margin: 0 0 6px;
  font-size: 13px;
  color: var(--td-text-color-primary);
}

/* 窄屏：两列 / 三列都塌成一列（人事常在 1280 笔记本上建档） */
@media (max-width: 1400px) {
  .form-grid--three {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 1100px) {
  .form-grid,
  .form-grid--three {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
