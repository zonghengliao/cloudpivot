<!--
  薪资项配置页（ARCH-9 §6.1 页面 #5 · §6.2.4 · HR-44）。

  ```
  列表：顺序 / 编码 / 名称 / 类型 / 算法 / 公式 / 科目 / 启用 / 操作
  抽屉：基本信息 + 科目（按类型动态必填）+ 公式编辑器（右变量面板 + 下检查按钮）
  ```

  # 数据来源（全部走 `@/api/business/hr/salary-item`）

  | 块 | 接口 | 说明 |
  |---|---|---|
  | 列表 | `GET /salary-item/page` | 含 `formula` 与后端抽好的 `depends_on` |
  | 新建 | `POST /salary-item` | 后端事务内做公式依赖检查 + 编码/顺序占用预检 |
  | 编辑 | `PUT /salary-item/{id}` | **编码不可改**（后端守卫）；系统项不可停用/改类型 |
  | 检查 | `POST /salary-item/validate` | `ok=false` 也是 200；回执带 `errors[]` + `dependency_order` |
  | 科目下拉 | `GET /business/erp/account/page?is_leaf=1&status=1` | 只列末级科目（选了非末级过账被拒） |

  # 六条与「想当然」不一样的交互（每条都有后端依据）

  1. **公式可编辑但前端不求值**（§4.4.2）：`ROUND` 的口径 / `Decimal` 精度 / 除零报错 /
     惰性 `IF` 全在后端，前端只做语法高亮与变量插入。两套求值 = 两套精度，工资条会少一分钱。
  2. **「检查」失败不改 HTTP 状态**（后端 #35 头注释）：`ok=false` 是「查询结论」，
     所以页面看 `res.ok` 而不是 `catch`。
  3. **检查与保存是同一份校验代码**（后端 `salary_item_dto` 头注释）：所以保存前先自动跑一次
     检查 —— 前端本地只挡「必填 / 列宽 / 科目分档」，公式语义必须让后端说。
  4. **报错要落到字符位置**：后端错误串里有的带「第 N 个字符」、有的只点名变量
     （`` `{BASEE}` ``）。`formulaIssues()` 把两种都换算成区间；点错误 → textarea 选区
     直接跳到那一段（`.focus()` + `setSelectionRange`），不是只弹一条 toast。
  5. **编码框编辑态 disabled**：编码是公式 `{CODE}` 与 `biz_employee_salary.item_code` 的
     弱引用键，改名会让引用它的公式与薪资标准**静默**失效（后端 PUT 直接拒）。
  6. **没有删除按钮**：配置表的生命周期是「停用」。历史薪资单的 `detail_item.item_id`
     指向它（`ON DELETE SET NULL`），删行等于把历史工资条上的项名抹掉
     （后端 `salary_item_handler` 头注释、`V0387` 种了 `salary-item:delete` 但无路由）。

  # 变量面板的两份数据（有先后的真相）

  抽屉一打开用**已加载的列表 + 本地 `ATT_VARS`** 拼一份（`fallbackVarOptions`），
  这样不打接口也能点插入；点过「检查」之后换成后端 `available`（以后端为准 ——
  后端才知道哪些项被停用、哪些编码真的存在）。
  -->

<template>
  <div class="hr-salary-item-page">
    <!-- ==================== ⓞ 页头 ==================== -->
    <t-card :bordered="false" class="head-card">
      <div class="page-head">
        <div class="page-head__main">
          <h2 class="page-head__title">薪资项配置</h2>
          <p class="page-head__desc">
            公式按 <code class="mono">{编码}</code> 引用其它薪资项与考勤变量（如
            <code class="mono">{BASE}</code>、<code class="mono">{ATT_OT_WD}</code>）；
            改公式不影响已生成的薪资单（历史按 <code class="mono">expr_snapshot</code> 留存）。
          </p>
        </div>
        <div class="page-head__actions">
          <t-button variant="outline" data-testid="hr-si-refresh" @click="loadRows">刷新</t-button>
          <t-button
            v-if="canCreate"
            theme="primary"
            data-testid="hr-si-create"
            @click="openCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新建薪资项
          </t-button>
        </div>
      </div>
    </t-card>

    <!-- ==================== ① 筛选 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="编码 / 名称"
          clearable
          data-testid="hr-si-keyword"
          @enter="handleSearch"
          @clear="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-select
          v-model="filter.item_type"
          class="filter-row__select"
          :options="itemTypeOptions()"
          placeholder="全部类型"
          clearable
          data-testid="hr-si-filter-type"
        />

        <t-select
          v-model="filter.calc_type"
          class="filter-row__select"
          :options="calcTypeOptions()"
          placeholder="全部算法"
          clearable
          data-testid="hr-si-filter-calc"
        />

        <t-select
          v-model="filter.is_active"
          class="filter-row__select"
          :options="activeFilterOptions"
          placeholder="启用 / 停用"
          clearable
          data-testid="hr-si-filter-active"
        />

        <t-button theme="primary" data-testid="hr-si-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="hr-si-reset" @click="handleReset">重置</t-button>
        <span class="filter-row__spacer" />
        <span v-if="isItemFiltered(filter)" class="filter-row__flag" data-testid="hr-si-filtered">
          已筛选
        </span>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">薪资项</span>
          <span class="panel-head__meta" data-testid="hr-si-meta">
            共 {{ total }} 项 · 启用 {{ activeCount }} 项
          </span>
        </div>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        class="list-alert"
        data-testid="hr-si-load-error"
        :message="loadError"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        hover
        :data="rows"
        :columns="SALARY_ITEM_COLUMNS"
        :loading="loading"
        :pagination="pagination"
        data-testid="hr-si-table"
        @page-change="handlePageChange"
      >
        <!-- 顺序 -->
        <template #sort_order="{ row }">
          <span class="mono sort-text">{{ row.sort_order }}</span>
        </template>

        <!-- 编码（点一下直接进编辑：最常见的动作是改公式） -->
        <template #item_code="{ row }">
          <t-link
            theme="primary"
            class="mono"
            data-testid="hr-si-open"
            @click="openEdit(row)"
          >{{ row.item_code }}</t-link>
          <t-tag v-if="row.is_system" size="small" variant="outline" theme="warning" class="system-tag">
            系统
          </t-tag>
        </template>

        <!-- 名称 + 依赖 -->
        <template #item_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__text">{{ row.item_name }}</span>
            <span v-if="dependsText(row)" class="name-cell__deps" :title="`依赖：${dependsText(row)}`">
              依赖 {{ dependsText(row) }}
            </span>
          </div>
        </template>

        <!-- 类型 -->
        <template #item_type="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="itemTypeTheme(row.item_type)"
            data-testid="hr-si-type-tag"
          >
            {{ row.item_type_label || itemTypeLabel(row.item_type) }}
          </t-tag>
        </template>

        <!-- 算法 + 来源 -->
        <template #calc_type="{ row }">
          <div class="calc-cell">
            <span>{{ row.calc_type_label || calcTypeLabel(row.calc_type) }}</span>
            <span class="calc-cell__source">{{ row.source_label || sourceLabel(row.source) }}</span>
          </div>
        </template>

        <!-- 公式（等宽截断，悬停看全文） -->
        <template #formula="{ row }">
          <span
            v-if="row.formula"
            class="mono formula-cell"
            :class="{ 'formula-cell--muted': !row.formula.trim() }"
            :title="row.formula"
          >{{ row.formula }}</span>
          <span v-else class="muted">—</span>
        </template>

        <!-- 科目 -->
        <template #account="{ row }">
          <span class="mono account-cell">{{ accountText(row) }}</span>
        </template>

        <!-- 启用（有 update 权限时可直接切：停用 = 生命周期，不是删除） -->
        <template #is_active="{ row }">
          <t-switch
            v-if="canUpdate && !isActiveLocked(row)"
            :value="row.is_active"
            size="small"
            :loading="togglingId === row.id"
            data-testid="hr-si-active-switch"
            @change="handleToggleActive(row)"
          />
          <t-tag
            v-else
            size="small"
            variant="light"
            :theme="row.is_active ? 'success' : 'default'"
          >{{ row.is_active ? '启用' : '停用' }}</t-tag>
        </template>

        <!-- 操作 -->
        <template #op="{ row }">
          <t-button
            v-if="canUpdate"
            size="small"
            variant="text"
            theme="primary"
            data-testid="hr-si-edit"
            @click="openEdit(row)"
          >配置</t-button>
          <span v-else class="muted">只读</span>
        </template>

        <template #empty>
          <div class="table-empty" data-testid="hr-si-empty">
            <span>{{ isItemFiltered(filter) ? '没有匹配的薪资项' : '还没有薪资项' }}</span>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 配置抽屉 ==================== -->
    <t-drawer
      v-model:visible="drawerVisible"
      size="820px"
      :footer="false"
      :header="false"
      destroy-on-close
      data-testid="hr-si-drawer"
    >
      <div class="drawer">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title">{{ editingId ? '编辑薪资项' : '新建薪资项' }}</span>
            <span v-if="editingRow" class="mono drawer-head__code">{{ editingRow.item_code }}</span>
            <t-tag
              v-if="editingRow"
              size="small"
              variant="light"
              :theme="itemTypeTheme(editingRow.item_type)"
            >{{ editingRow.item_type_label || itemTypeLabel(editingRow.item_type) }}</t-tag>
            <t-tag v-if="editingRow?.is_system" size="small" variant="outline" theme="warning">
              系统项
            </t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="hr-si-drawer-close" @click="closeDrawer">
              取消
            </t-button>
          </div>
        </div>

        <t-form :data="form" label-width="96px" label-align="right" class="item-form" data-testid="hr-si-form">
          <div class="form-grid">
            <t-form-item label="编码" required-mark>
              <t-input
                v-model="form.item_code"
                class="mono"
                :disabled="Boolean(editingId)"
                :maxlength="MAX_ITEM_CODE_LEN"
                placeholder="大写字母开头，如 OT_WD"
                data-testid="hr-si-code"
                @blur="handleCodeBlur"
              />
              <div v-if="formErrors.item_code" class="field-error">{{ formErrors.item_code }}</div>
              <div v-else-if="editingId" class="field-hint">编码不可改：公式与薪资标准按 `{编码}` 引用它</div>
            </t-form-item>

            <t-form-item label="名称" required-mark>
              <t-input
                v-model="form.item_name"
                :maxlength="MAX_ITEM_NAME_LEN"
                placeholder="如 平时加班费"
                data-testid="hr-si-name"
              />
              <div v-if="formErrors.item_name" class="field-error">{{ formErrors.item_name }}</div>
            </t-form-item>

            <t-form-item label="类型" required-mark>
              <t-select
                v-model="form.item_type"
                :options="itemTypeOptions()"
                :disabled="isTypeLocked(editingRow)"
                data-testid="hr-si-item-type"
                @change="handleTypeChange"
              />
              <div v-if="formErrors.item_type" class="field-error">{{ formErrors.item_type }}</div>
              <div v-else class="field-hint">{{ typeHint }}</div>
            </t-form-item>

            <t-form-item label="算法" required-mark>
              <t-select
                v-model="form.calc_type"
                :options="calcTypeOptions()"
                :disabled="isTypeLocked(editingRow)"
                data-testid="hr-si-calc-type"
              />
              <div v-if="formErrors.calc_type" class="field-error">{{ formErrors.calc_type }}</div>
            </t-form-item>

            <t-form-item label="取数来源">
              <t-select
                v-model="form.source"
                :options="sourceOptions()"
                data-testid="hr-si-source"
              />
              <div v-if="formErrors.source" class="field-error">{{ formErrors.source }}</div>
            </t-form-item>

            <t-form-item label="计算顺序">
              <t-input
                v-model="form.sort_order"
                placeholder="留空 = 当前最大 + 10"
                data-testid="hr-si-sort"
              />
              <div v-if="formErrors.sort_order" class="field-error">{{ formErrors.sort_order }}</div>
            </t-form-item>
          </div>

          <!-- 科目：按类型动态必填（EARNING/PRE_DEDUCT/EMPLOYER 借+贷；WITHHOLD 只贷；SUMMARY 隐藏） -->
          <div v-if="accountNeeds(form.item_type).debit || accountNeeds(form.item_type).credit" class="account-row">
            <t-form-item
              v-if="accountNeeds(form.item_type).debit"
              label="借方科目"
              required-mark
            >
              <t-select
                v-model="form.debit_account_code"
                class="w-full"
                filterable
                clearable
                :options="accountOptions"
                :loading="accountLoading"
                :empty="'科目未加载到，可直接手输科目码'"
                placeholder="如 5602.10（只列末级科目）"
                data-testid="hr-si-debit"
              />
              <div v-if="formErrors.debit_account_code" class="field-error">
                {{ formErrors.debit_account_code }}
              </div>
            </t-form-item>

            <t-form-item
              v-if="accountNeeds(form.item_type).credit"
              label="贷方科目"
              required-mark
            >
              <t-select
                v-model="form.credit_account_code"
                class="w-full"
                filterable
                clearable
                :options="accountOptions"
                :loading="accountLoading"
                :empty="'科目未加载到，可直接手输科目码'"
                placeholder="如 2211.01（只列末级科目）"
                data-testid="hr-si-credit"
              />
              <div v-if="formErrors.credit_account_code" class="field-error">
                {{ formErrors.credit_account_code }}
              </div>
            </t-form-item>
          </div>

          <!-- ==================== 公式编辑器（§6.2.4） ==================== -->
          <t-form-item v-if="needsFormula(form.calc_type)" label="公式" required-mark class="formula-item">
            <div class="formula-editor" data-testid="hr-si-formula-editor">
              <div class="formula-editor__main">
                <t-textarea
                  ref="formulaRef"
                  v-model="form.formula"
                  class="formula-input mono"
                  :maxlength="MAX_FORMULA_LEN"
                  :autosize="{ minRows: 3, maxRows: 8 }"
                  :status="formulaStatus"
                  placeholder="例：ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)"
                  data-testid="hr-si-formula"
                  @input="handleFormulaInput"
                  @click="rememberCaret"
                  @keyup="rememberCaret"
                  @blur="rememberCaret"
                />

                <!-- 高亮预览：变量蓝、函数紫、数字灰；出错区间标红（点错误清单可跳转） -->
                <div class="formula-preview" data-testid="hr-si-formula-preview">
                  <template v-if="form.formula">
                    <span
                      v-for="(token, index) in previewTokens"
                      :key="index"
                      class="ftok"
                      :class="[
                        `ftok--${token.kind}`,
                        { 'ftok--issue': tokenHasIssue(token), 'ftok--active': tokenHasActiveIssue(token) },
                      ]"
                    >{{ token.text }}</span>
                  </template>
                  <span v-else class="muted">公式为空</span>
                </div>

                <!-- 工具条：检查 / 字数 / 结论 -->
                <div class="formula-toolbar">
                  <t-button
                    size="small"
                    theme="primary"
                    variant="outline"
                    :loading="validating"
                    :disabled="!form.formula.trim()"
                    data-testid="hr-si-check"
                    @click="handleValidate(true)"
                  >
                    <template #icon><t-icon name="check-circle" /></template>
                    检查
                  </t-button>

                  <span class="formula-toolbar__count" :class="{ 'formula-toolbar__count--max': formulaLength >= MAX_FORMULA_LEN }">
                    {{ formulaLength }} / {{ MAX_FORMULA_LEN }}
                  </span>

                  <t-tag
                    v-if="validateStale && validateResult"
                    size="small"
                    variant="light"
                    theme="warning"
                    data-testid="hr-si-check-stale"
                  >公式已修改，请重新检查</t-tag>

                  <span
                    v-if="validateView_"
                    class="formula-toolbar__verdict"
                    :class="validateView_.ok ? 'formula-toolbar__verdict--ok' : 'formula-toolbar__verdict--bad'"
                    data-testid="hr-si-check-verdict"
                  >
                    <t-icon :name="validateView_.ok ? 'check-circle' : 'error-circle'" />
                    {{ validateSummary(validateView_) }}
                  </span>
                </div>

                <!-- 错误清单：每条带「第 N 个字符」，点一条把光标落过去 -->
                <div
                  v-if="validateView_ && !validateView_.ok"
                  class="formula-issues"
                  data-testid="hr-si-formula-issues"
                >
                  <div
                    v-for="(issue, index) in validateView_.issues"
                    :key="index"
                    class="formula-issue"
                    :class="{ 'formula-issue--active': activeIssue === index, 'formula-issue--plain': !issue.located }"
                    :data-testid="`hr-si-formula-issue-${index}`"
                    @click="focusIssue(issue, index)"
                  >
                    <span class="formula-issue__loc mono">{{ issueLocationText(issue, form.formula) }}</span>
                    <span class="formula-issue__msg">{{ issue.message }}</span>
                    <t-icon v-if="issue.located" name="jump" class="formula-issue__jump" />
                  </div>
                </div>

                <div v-else-if="validateView_ && validateView_.ok" class="formula-ok" data-testid="hr-si-formula-ok">
                  <span class="mono">{{ validateView_.dependencyText || '无薪资项依赖' }}</span>
                  <span class="muted">引用 {{ validateView_.refCount }} 个变量</span>
                </div>
              </div>

              <!-- 变量面板：点一下插到光标处 -->
              <div class="var-panel" data-testid="hr-si-var-panel">
                <div class="var-panel__head">
                  <span class="var-panel__title">变量</span>
                  <t-input
                    v-model="varKeyword"
                    size="small"
                    placeholder="筛选"
                    clearable
                    data-testid="hr-si-var-search"
                  >
                    <template #prefix-icon><t-icon name="search" /></template>
                  </t-input>
                </div>

                <div class="var-panel__body">
                  <div v-for="group in visibleVarGroups" :key="group.key" class="var-group">
                    <div class="var-group__title">
                      {{ group.title }}
                      <span class="var-group__count">{{ group.items.length }}</span>
                    </div>
                    <div class="var-group__items">
                      <button
                        v-for="option in group.items"
                        :key="option.name"
                        type="button"
                        class="var-chip"
                        :class="{ 'var-chip--disabled': !option.usable }"
                        :title="option.usable ? `插入 {${option.name}}` : `${option.name} 已停用或就是本项，仍可插入`"
                        :data-testid="`hr-si-var-${option.name}`"
                        @click="insertVar(option.name)"
                      >
                        <span class="var-chip__name mono">{{ option.name }}</span>
                        <span v-if="option.label && option.label !== option.name" class="var-chip__label">
                          {{ option.label }}
                        </span>
                        <t-tag v-if="!option.usable" size="small" variant="light" theme="default">停用</t-tag>
                      </button>
                    </div>
                  </div>

                  <div v-if="!visibleVarGroups.length" class="var-panel__empty muted">
                    没有匹配的变量
                  </div>

                  <div class="var-panel__funcs">
                    <div class="var-group__title">函数</div>
                    <div class="func-list">
                      <span
                        v-for="func in FORMULA_FUNCS"
                        :key="func"
                        class="func-chip mono"
                        :title="FORMULA_FUNC_HINTS[func]"
                      >{{ FORMULA_FUNC_HINTS[func] }}</span>
                    </div>
                  </div>
                </div>
              </div>
            </div>
            <div v-if="formErrors.formula" class="field-error">{{ formErrors.formula }}</div>
          </t-form-item>

          <t-form-item label="备注">
            <t-textarea
              v-model="form.remark"
              :maxlength="MAX_REMARK_LEN"
              :autosize="{ minRows: 2, maxRows: 4 }"
              placeholder="给 HR 看的说明（不会出现在工资条上）"
              data-testid="hr-si-remark"
            />
            <div v-if="formErrors.remark" class="field-error">{{ formErrors.remark }}</div>
          </t-form-item>

          <t-form-item label="开关">
            <div class="switch-row">
              <label class="switch-row__item">
                <t-switch v-model="form.is_active" data-testid="hr-si-form-active" />
                <span>启用</span>
                <span class="muted">停用后新薪资单不再计算该人工项（历史单不受影响）</span>
              </label>
              <label class="switch-row__item">
                <t-switch v-model="form.is_taxable" data-testid="hr-si-form-taxable" />
                <span>计入应税收入</span>
                <span class="muted">二期自动个税用；一期手工录</span>
              </label>
            </div>
          </t-form-item>
        </t-form>

        <div class="drawer-foot">
          <span class="drawer-foot__hint">
            {{ editingId ? '保存后旧薪资单不受影响（按生成时的公式快照留存）' : '保存时会先跑一次公式检查' }}
          </span>
          <div class="drawer-foot__actions">
            <t-button variant="outline" data-testid="hr-si-cancel" @click="closeDrawer">取消</t-button>
            <t-button
              theme="primary"
              :loading="saving"
              data-testid="hr-si-save"
              @click="handleSubmit"
            >
              {{ editingId ? '保存' : '创建' }}
            </t-button>
          </div>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 薪资项配置页（HR-44）。
 *
 * 全部口径走 `salary-item-kinds.ts` 的纯函数（可单测）：词表 / 列定义 / 表单校验 /
 * 公式分词与报错定位 / 变量面板分组。页面只做「拉数据 + 摆界面 + 弹提示 + 落光标」。
 *
 * 权限码（`V0387__hr_menu_perms` 逐字一致）：
 * `business:hr:salary-item:{list,create,update}`。页面入口由 `router/index.ts` 的
 * `meta.permission` 拦；页内按钮由 `hasPermission` 拦（无权限时按钮不出现，而不是点了 403）。
 */
import { computed, nextTick, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  FORMULA_FUNCS,
  FORMULA_FUNC_HINTS,
  MAX_FORMULA_LEN,
  MAX_ITEM_CODE_LEN,
  MAX_ITEM_NAME_LEN,
  MAX_REMARK_LEN,
  SALARY_ITEM_COLUMNS,
  SALARY_ITEM_PERMS,
  accountNeeds,
  accountText,
  buildPageQuery,
  calcTypeLabel,
  calcTypeOptions,
  createSalaryItem,
  dependsText,
  emptyItemFilter,
  emptyItemForm,
  fallbackVarOptions,
  fetchSalaryAccountOptions,
  fetchSalaryItemPage,
  firstFormError,
  formFromRow,
  insertVariable,
  isActiveLocked,
  isItemFiltered,
  isTypeLocked,
  itemNameMap,
  itemTypeLabel,
  itemTypeOptions,
  itemTypeTheme,
  issueLocationText,
  needsFormula,
  nextSortOrder,
  sourceLabel,
  sourceOptions,
  toCharIndex,
  toCodeUnitIndex,
  toWritePayload,
  tokenizeFormula,
  updateSalaryItem,
  validateItemForm,
  validateSalaryItemFormula,
  validateSummary,
  validateView,
  varPanelGroups,
  type FormulaIssue,
  type SalaryAccountOption,
  type SalaryItemFilter,
  type SalaryItemForm,
  type SalaryItemRow,
  type SalaryItemValidateRes,
  type SalaryItemVarOption,
} from '@/api/business/hr/salary-item';

const userStore = useUserStore();

const canCreate = computed(() => userStore.hasPermission(SALARY_ITEM_PERMS.create));
const canUpdate = computed(() => userStore.hasPermission(SALARY_ITEM_PERMS.update));

/** 统一错误文案（`ApiError` 带后端中文报文） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return '操作失败，请重试';
}

// ==================== 列表 ====================

const loading = ref(false);
const loadError = ref('');
const rows = ref<SalaryItemRow[]>([]);
const total = ref(0);
const currentPage = ref(1);
const pageSize = ref(20);
const filter = reactive<SalaryItemFilter>(emptyItemFilter());
const togglingId = ref('');

const pagination = computed(() => ({
  current: currentPage.value,
  pageSize: pageSize.value,
  total: total.value,
  showJumper: true,
  pageSizeOptions: [10, 20, 50, 100],
}));

const activeCount = computed(() => rows.value.filter((row) => row.is_active).length);

const activeFilterOptions = [
  { label: '启用', value: '1' },
  { label: '停用', value: '0' },
];

/** 拉列表（筛选 / 分页共用一条路径） */
async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchSalaryItemPage(buildPageQuery(filter, currentPage.value, pageSize.value));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    // 变量面板的兜底清单靠这份列表补中文名，改完列表要同步
    refreshFallbackVars();
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  currentPage.value = 1;
  void loadRows();
}

function handleReset(): void {
  Object.assign(filter, emptyItemFilter());
  currentPage.value = 1;
  void loadRows();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  currentPage.value = pageInfo.current;
  pageSize.value = pageInfo.pageSize;
  void loadRows();
}

/** 启用 / 停用（配置表的生命周期；后端 PUT 需要整份写入口，所以从行还原表单再改开关） */
async function handleToggleActive(row: SalaryItemRow): Promise<void> {
  if (togglingId.value) {
    return;
  }
  togglingId.value = row.id;
  try {
    const payload = toWritePayload({ ...formFromRow(row), is_active: !row.is_active });
    const saved = await updateSalaryItem(row.id, payload);
    const index = rows.value.findIndex((item) => item.id === row.id);
    if (index >= 0) {
      rows.value[index] = saved;
    }
    MessagePlugin.success(saved.is_active ? `已启用「${saved.item_name}」` : `已停用「${saved.item_name}」`);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    togglingId.value = '';
  }
}

// ==================== 抽屉 / 表单 ====================

const drawerVisible = ref(false);
const form = reactive<SalaryItemForm>(emptyItemForm());
const formErrors = ref<Record<string, string>>({});
const editingId = ref('');
const editingRow = ref<SalaryItemRow | null>(null);
const saving = ref(false);

/** 类型 → 说明（决定「进不进应发 / 要不要科目」，设计 §4.4.1） */
const typeHint = computed(() => {
  switch (form.item_type) {
    case 'EARNING':
      return '收入项：进应发，借费用 / 贷 2211.01';
    case 'PRE_DEDUCT':
      return '应发前扣减：算少应发（考勤扣款就是这类，不是代扣）';
    case 'WITHHOLD':
      return '代扣项：发的时候扣走，只填贷方（社保 / 个税）';
    case 'EMPLOYER':
      return '单位承担：进费用不进应发（单位社保 / 公积金）';
    case 'SUMMARY':
      return '汇总项：只能由公式汇总（应发合计 / 实发合计），不要科目';
    default:
      return '';
  }
});

function openCreate(): void {
  editingId.value = '';
  editingRow.value = null;
  Object.assign(form, emptyItemForm());
  form.sort_order = String(nextSortOrder(rows.value));
  formErrors.value = {};
  resetValidate();
  refreshFallbackVars();
  drawerVisible.value = true;
  void loadAccounts();
}

function openEdit(row: SalaryItemRow): void {
  editingId.value = row.id;
  editingRow.value = row;
  Object.assign(form, formFromRow(row));
  formErrors.value = {};
  resetValidate();
  refreshFallbackVars(row.item_code);
  drawerVisible.value = true;
  void loadAccounts();
  // 打开时公式末尾就是「下一个变量该落的位置」
  void nextTick(() => setCaret(form.formula.length, form.formula.length));
}

function closeDrawer(): void {
  drawerVisible.value = false;
  editingId.value = '';
  editingRow.value = null;
  resetValidate();
}

/** 类型切换：`SUMMARY` 强制 FORMULA（后端 `parse_write` 同一条）；切到非 FORMULA 清掉公式 */
function handleTypeChange(): void {
  if (form.item_type === 'SUMMARY') {
    form.calc_type = 'FORMULA';
  }
  if (!needsFormula(form.calc_type)) {
    form.formula = '';
  }
}

/** 编码失焦：顺手 trim（用户从文档里复制常带空格） */
function handleCodeBlur(): void {
  form.item_code = form.item_code.trim().toUpperCase();
}

// ==================== 科目下拉 ====================

const accountOptionsRaw = ref<SalaryAccountOption[]>([]);
const accountLoading = ref(false);

const accountOptions = computed(() =>
  accountOptionsRaw.value.map((item) => ({
    label: `${item.code} ${item.name}`,
    value: item.code,
  })),
);

/** 拉末级科目（失败回空数组：不挡住只想改公式的人，科目码也能手输） */
async function loadAccounts(): Promise<void> {
  if (accountOptionsRaw.value.length || accountLoading.value) {
    return;
  }
  accountLoading.value = true;
  try {
    accountOptionsRaw.value = await fetchSalaryAccountOptions();
  } finally {
    accountLoading.value = false;
  }
}

// ==================== 公式编辑器 ====================

/**
 * `t-textarea` 的组件实例。
 *
 * TDesign 的 Textarea 没往外暴露 textarea 元素（`type.d.ts` 里没有 `textareaRef`），
 * 但根节点是 `div.t-textarea`、里面就是原生 `textarea`。这里通过 `$el.querySelector`
 * 拿它 —— 只为了两件事：读光标位置、把出错区间设成选区。
 */
const formulaRef = ref();
const caretStart = ref(0);
const caretEnd = ref(0);

const previewTokens = computed(() => tokenizeFormula(form.formula));
const formulaLength = computed(() => Array.from(form.formula).length);

const validateResult = ref<SalaryItemValidateRes | null>(null);
const validateStale = ref(false);
const validating = ref(false);
const activeIssue = ref(-1);
const varKeyword = ref('');

const validateView_ = computed(() =>
  validateResult.value ? validateView(validateResult.value, form.formula) : null,
);

/** 公式框状态：检查过且有错 → error；通过 → success */
const formulaStatus = computed<'default' | 'success' | 'error'>(() => {
  if (!validateResult.value || validateStale.value) {
    return 'default';
  }
  return validateResult.value.ok ? 'success' : 'error';
});

const availableVars = ref<SalaryItemVarOption[]>([]);

/** 变量面板（兜底清单 → 检查回执覆盖；后端那份是唯一真相） */
function refreshFallbackVars(selfCode = ''): void {
  if (!validateResult.value) {
    availableVars.value = fallbackVarOptions(rows.value, selfCode || form.item_code.trim());
  }
}

const varGroups = computed(() =>
  varPanelGroups(availableVars.value, {
    selfCode: form.item_code.trim(),
    itemNames: itemNameMap(rows.value),
  }),
);

/** 面板搜索：按编码 / 中文名过滤（清空搜索时恢复全量） */
const visibleVarGroups = computed(() => {
  const keyword = varKeyword.value.trim().toUpperCase();
  if (!keyword) {
    return varGroups.value;
  }
  return varGroups.value
    .map((group) => ({
      ...group,
      items: group.items.filter(
        (item) => item.name.toUpperCase().includes(keyword) || item.label.includes(varKeyword.value.trim()),
      ),
    }))
    .filter((group) => group.items.length > 0);
});

/** 取原生 textarea 元素 */
function formulaEl(): HTMLTextAreaElement | null {
  const root = formulaRef.value?.$el as HTMLElement | undefined;
  if (!root) {
    return null;
  }
  return root.querySelector('textarea');
}

/** 记光标（`@click` / `@keyup` / `@blur` 都调：变量面板点击前光标会先离开 textarea） */
function rememberCaret(): void {
  const el = formulaEl();
  if (!el) {
    return;
  }
  caretStart.value = toCharIndex(form.formula, el.selectionStart ?? 0);
  caretEnd.value = toCharIndex(form.formula, el.selectionEnd ?? caretStart.value);
}

/** 把光标 / 选区落到字符区间（[start, end)） */
async function setCaret(start: number, end: number): Promise<void> {
  await nextTick();
  const el = formulaEl();
  if (!el) {
    return;
  }
  const from = toCodeUnitIndex(form.formula, start);
  const to = toCodeUnitIndex(form.formula, end);
  el.focus();
  el.setSelectionRange(from, to);
  caretStart.value = start;
  caretEnd.value = end;
}

/** 变量面板点一下：插到光标处（有选区就替换） */
function insertVar(name: string): void {
  const result = insertVariable(form.formula, name, caretStart.value, caretEnd.value);
  form.formula = result.formula;
  handleFormulaInput();
  void setCaret(result.caret, result.caret);
}

/** 公式改动：清掉「已通过」的旧结论，标成待重新检查 */
function handleFormulaInput(): void {
  if (validateResult.value) {
    validateStale.value = true;
  }
  activeIssue.value = -1;
}

/** 清空检查态 */
function resetValidate(): void {
  validateResult.value = null;
  validateStale.value = false;
  activeIssue.value = -1;
  varKeyword.value = '';
  availableVars.value = fallbackVarOptions(rows.value, form.item_code.trim());
}

/**
 * 「检查」按钮（`POST /salary-item/validate`）。
 *
 * `ok=false` 也是 200，所以看 `res.ok` 而不是 `catch`（文件头第 2 条）。
 * 回执里的 `available` 覆盖本地兜底清单 —— 后端才知道哪些项真的可引用。
 */
async function handleValidate(manual: boolean): Promise<SalaryItemValidateRes | null> {
  if (!form.formula.trim()) {
    if (manual) {
      MessagePlugin.warning('公式为空，先写公式或点变量面板插入');
    }
    return null;
  }
  validating.value = true;
  try {
    const payload: { formula: string; item_code?: string; id?: string } = { formula: form.formula };
    if (editingId.value) {
      payload.id = editingId.value;
    } else if (form.item_code.trim()) {
      payload.item_code = form.item_code.trim();
    }
    const res = await validateSalaryItemFormula(payload);
    validateResult.value = res;
    validateStale.value = false;
    activeIssue.value = -1;
    if (res.available?.length) {
      availableVars.value = res.available;
    }
    if (manual) {
      if (res.ok) {
        MessagePlugin.success(dependencyHintOf(res));
      } else {
        MessagePlugin.error(`公式检查未通过：${res.errors?.[0] ?? '未知问题'}`);
      }
    }
    return res;
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    return null;
  } finally {
    validating.value = false;
  }
}

/** 通过时的一句话（依赖顺序 / 无依赖） */
function dependencyHintOf(res: SalaryItemValidateRes): string {
  const order = (res.dependency_order ?? []).join(' → ');
  return order ? `语法通过 · 依赖顺序 ${order}` : '语法通过 · 无薪资项依赖';
}

/** 错误区间 → 高亮（任一问题命中即红） */
function tokenHasIssue(token: { start: number; end: number }): boolean {
  const view = validateView_.value;
  if (!view || view.ok) {
    return false;
  }
  return view.issues.some(
    (issue) => issue.located && token.end > issue.start && token.start < issue.end,
  );
}

/** 当前选中的那条错误 → 更深的高亮 */
function tokenHasActiveIssue(token: { start: number; end: number }): boolean {
  const view = validateView_.value;
  if (!view || view.ok || activeIssue.value < 0) {
    return false;
  }
  const issue = view.issues[activeIssue.value];
  if (!issue || !issue.located) {
    return false;
  }
  return token.end > issue.start && token.start < issue.end;
}

/** 点错误清单的一条：把选区落到出错位置（「检查按钮报错定位到位置」的落地） */
function focusIssue(issue: FormulaIssue, index: number): void {
  activeIssue.value = index;
  if (!issue.located) {
    return;
  }
  void setCaret(issue.start, issue.end);
}

// ==================== 保存 ====================

/**
 * 保存：本地校验（必填 / 列宽 / 科目分档）→ 公式检查（同一份后端校验）→ 写库。
 *
 * 为什么保存前再检查一次：后端 `POST/PUT` 与 `#35` 走**同一套**结构层 + 语义层
 * （`salary_item_dto` 头注释）。先跑一次能让用户在抽屉里看到「哪一段公式错了」，
 * 而不是拿到一条 400 报文后自己找。
 */
async function handleSubmit(): Promise<void> {
  if (saving.value) {
    return;
  }
  const errors = validateItemForm(form, { creating: !editingId.value });
  formErrors.value = errors;
  if (Object.keys(errors).length) {
    MessagePlugin.warning(firstFormError(errors));
    return;
  }

  if (needsFormula(form.calc_type)) {
    const checked = await handleValidate(false);
    if (checked && !checked.ok) {
      MessagePlugin.error(`公式未通过检查：${checked.errors?.[0] ?? '请点「检查」看详情'}`);
      return;
    }
  }

  saving.value = true;
  try {
    const payload = toWritePayload(form);
    const saved = editingId.value
      ? await updateSalaryItem(editingId.value, payload)
      : await createSalaryItem(payload);
    MessagePlugin.success(editingId.value ? `已保存「${saved.item_name}」` : `已创建「${saved.item_name}」`);
    drawerVisible.value = false;
    resetValidate();
    editingId.value = '';
    editingRow.value = null;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    saving.value = false;
  }
}

onMounted(() => {
  void loadRows();
});
</script>

<style scoped lang="less">
.hr-salary-item-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.page-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.page-head__title {
  margin: 0 0 4px;
  font-size: 18px;
  font-weight: 600;
}

.page-head__desc {
  margin: 0;
  max-width: 760px;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 13px;
  line-height: 1.6;
}

.page-head__desc code {
  padding: 0 3px;
  border-radius: 3px;
  background: var(--td-bg-color-component, #f3f3f3);
  font-size: 12px;
}

.page-head__actions {
  display: flex;
  flex: 0 0 auto;
  gap: 8px;
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

.filter-row__select {
  width: 150px;
}

.filter-row__spacer {
  flex: 1 1 auto;
}

.filter-row__flag {
  color: var(--td-brand-color, #0052d9);
  font-size: 12px;
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

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.muted {
  color: var(--td-text-color-placeholder, #0000004d);
}

.sort-text {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.system-tag {
  margin-left: 6px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.name-cell__deps {
  overflow: hidden;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.calc-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.calc-cell__source {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 11px;
}

.formula-cell {
  display: inline-block;
  overflow: hidden;
  max-width: 100%;
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
}

.formula-cell--muted {
  color: var(--td-text-color-placeholder, #0000004d);
}

.account-cell {
  font-size: 12px;
}

.table-empty {
  padding: 24px 0;
  color: var(--td-text-color-placeholder, #0000004d);
  text-align: center;
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
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke, #e7e7e7);
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
}

.drawer-head__title {
  font-size: 16px;
  font-weight: 600;
}

.drawer-head__code {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 16px;
}

.account-row {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 16px;
}

.field-error {
  margin-top: 2px;
  color: var(--td-error-color, #d54941);
  font-size: 12px;
  line-height: 1.5;
}

.field-hint {
  margin-top: 2px;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
  line-height: 1.5;
}

/* ==================== 公式编辑器 ==================== */

.formula-item {
  display: block;
}

.formula-editor {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 260px;
  gap: 12px;
  width: 100%;
}

.formula-editor__main {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-width: 0;
}

.formula-input {
  width: 100%;
}

.formula-input :deep(textarea) {
  font-size: 13px;
  line-height: 1.6;
  letter-spacing: 0.2px;
}

.formula-preview {
  overflow-x: auto;
  padding: 8px 10px;
  border: 1px dashed var(--td-component-stroke, #e7e7e7);
  border-radius: 4px;
  background: var(--td-bg-color-container-hover, #f8f8f8);
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-size: 12px;
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-all;
}

.ftok--var {
  color: var(--td-brand-color, #0052d9);
  font-weight: 600;
}

.ftok--var-invalid {
  color: var(--td-error-color, #d54941);
  background: var(--td-error-color-1, #fff0ed);
  text-decoration: underline wavy;
}

.ftok--func {
  color: var(--td-warning-color, #e37318);
  font-weight: 600;
}

.ftok--number {
  color: var(--td-text-color-secondary, #00000099);
}

.ftok--op {
  color: var(--td-text-color-primary, #000000e6);
}

.ftok--issue {
  border-radius: 2px;
  background: var(--td-error-color-1, #fff0ed);
  box-shadow: inset 0 -2px 0 var(--td-error-color, #d54941);
  text-decoration: underline wavy var(--td-error-color, #d54941);
}

.ftok--active {
  border-radius: 2px;
  background: var(--td-error-color, #d54941);
  color: #fff;
  box-shadow: none;
  text-decoration: none;
}

.formula-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}

.formula-toolbar__count {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.formula-toolbar__count--max {
  color: var(--td-warning-color, #e37318);
}

.formula-toolbar__verdict {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
}

.formula-toolbar__verdict--ok {
  color: var(--td-success-color, #2ba471);
}

.formula-toolbar__verdict--bad {
  color: var(--td-error-color, #d54941);
}

.formula-issues {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px;
  border: 1px solid var(--td-error-color-2, #ffd6d1);
  border-radius: 4px;
  background: var(--td-error-color-1, #fff0ed);
}

.formula-issue {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 3px;
  cursor: pointer;
  font-size: 12px;
  line-height: 1.6;
}

.formula-issue:hover {
  background: var(--td-bg-color-container, #fff);
}

.formula-issue--plain {
  cursor: default;
}

.formula-issue--active {
  background: var(--td-bg-color-container, #fff);
  box-shadow: inset 2px 0 0 var(--td-error-color, #d54941);
}

.formula-issue__loc {
  flex: 0 0 auto;
  color: var(--td-error-color, #d54941);
  font-weight: 600;
}

.formula-issue__msg {
  flex: 1 1 auto;
  color: var(--td-text-color-primary, #000000e6);
  word-break: break-word;
}

.formula-issue__jump {
  flex: 0 0 auto;
  color: var(--td-text-color-placeholder, #0000004d);
}

.formula-ok {
  display: flex;
  gap: 10px;
  align-items: center;
  font-size: 12px;
}

/* ==================== 变量面板 ==================== */

.var-panel {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
  max-height: 460px;
  padding: 8px;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  border-radius: 4px;
  background: var(--td-bg-color-container, #fff);
}

.var-panel__head {
  display: flex;
  align-items: center;
  gap: 8px;
}

.var-panel__title {
  flex: 0 0 auto;
  font-size: 13px;
  font-weight: 600;
}

.var-panel__body {
  display: flex;
  flex-direction: column;
  gap: 10px;
  overflow-y: auto;
}

.var-panel__empty {
  padding: 12px 0;
  font-size: 12px;
  text-align: center;
}

.var-group__title {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 4px;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 11px;
  letter-spacing: 0.4px;
}

.var-group__count {
  padding: 0 4px;
  border-radius: 8px;
  background: var(--td-bg-color-component, #f3f3f3);
}

.var-group__items {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.var-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 4px 8px;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  border-radius: 3px;
  background: transparent;
  cursor: pointer;
  font-size: 12px;
  text-align: left;
  transition: all 0.15s;
}

.var-chip:hover {
  border-color: var(--td-brand-color, #0052d9);
  background: var(--td-brand-color-1, #e3f0ff);
}

.var-chip--disabled {
  opacity: 0.6;
}

.var-chip__name {
  color: var(--td-brand-color, #0052d9);
  font-size: 11px;
}

.var-chip__label {
  overflow: hidden;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.var-panel__funcs {
  padding-top: 8px;
  border-top: 1px dashed var(--td-component-stroke, #e7e7e7);
}

.func-list {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.func-chip {
  overflow: hidden;
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ==================== 抽屉底部 ==================== */

.switch-row {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.switch-row__item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.drawer-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: auto;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke, #e7e7e7);
}

.drawer-foot__hint {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.drawer-foot__actions {
  display: flex;
  gap: 8px;
}
</style>
