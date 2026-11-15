<!--
  薪资管理页（ARCH-9 §6.1 页面 #4 · §5.2 #36/#39/#40/#46 · 卡 t_fc256a30）。

  ```
  ⓞ 页头：标题 + 说明 + 动作（刷新 / 试算 / 建单并保存，后两个要 salary:calc）
  ① 两个 tab：薪资单（全量，要 salary:list）/ 我的薪资（自助，要 salary:my）
  ② 筛选：期间（月选择器）/ 状态（六档）/ 单号关键字；我的薪资另给「含在途」开关
  ③ 列表：一单一行（单号 / 期间 / 状态 / 人数 / 四个合计 / 凭证），点单号进详情页
  ④ 试算抽屉：期间 + 备注 → 试算（不落库）/ 建单并保存（落库）→ 逐人核对表
  ```

  # 为什么两个 tab 按权限显隐，而不是都画出来

  `V0401` 把页面码与全量接口码解耦之后，员工（`EMPLOYEE`）进得来这一页
  （`business:hr:salary:page`）但**没有** `salary:list` —— 全量 `/salary/page` 对他 403
  才是设计意图（HR-47 实测员工能拉全公司工资单的越权面就是这么关掉的）。
  所以「薪资单」tab 只在持 `salary:list` 时画；「我的薪资」tab 只在持 `salary:my` 时画。
  两个都没有就只剩一句说明 —— 不给点了必 403 的入口（`AGENTS.md` 红线：
  同样不给设计表里有、但后端没有路由的假按钮）。

  # 试算与建单是同一次计算，只差 dry_run

  `POST /salary/calc`（试算，库里零行、单号不占）与 `POST /salary`（落库）收同一形状报文，
  后端 `parse_calc_req` 会拿 `dry_run` 与接口身份对拍（传错直接 400）。
  所以页面**不手写** `dry_run`：`buildCalcPayload(form, 'preview' | 'persist')` 生成。

  # 列表为什么不再逐单打详情

  `GET /salary/page` 回的就是单头卡（人数 + 四个合计 + 两张凭证号），一页 20 单
  逐单打 `GET /salary/{id}` 是典型的 N+1（本仓反复修过的反模式）。详情页留给
  「点进去看明细 / 审批 / 凭证」。

  # 空态分两种话

  「库里还没有薪资单」要说下一步点哪里（试算 → 建单）；「筛没了」要说换个条件。
  两者的文案在 `salary-list-kinds.ts::listEmptyText` 里分开。
-->

<template>
  <div class="hr-salary-page" data-testid="hr-salary-page">
    <!-- ==================== ⓞ 页头 ==================== -->
    <t-card :bordered="false" class="head-card">
      <div class="page-head">
        <div class="page-head__main">
          <h2 class="page-head__title">薪资管理</h2>
          <p class="page-head__desc">
            一张薪资单 = 一个薪资期间的全员工资：按当期<b>已锁定</b>的考勤台账算，
            核对无误后提交审批 → 过账生成凭证。
            <b>试算不落库、不占单号</b>，可以先算一遍看数。
          </p>
        </div>
        <div class="page-head__actions">
          <t-button
            v-if="canCalc"
            variant="outline"
            data-testid="hr-salary-preview"
            @click="openCalc('preview')"
          >
            <template #icon><t-icon name="calculate" /></template>
            试算
          </t-button>
          <t-button
            v-if="canCalc"
            theme="primary"
            data-testid="hr-salary-create"
            @click="openCalc('persist')"
          >
            <template #icon><t-icon name="add" /></template>
            建单并保存
          </t-button>
        </div>
      </div>
    </t-card>

    <!-- ==================== ①② 两个 tab 各一个 ListPage（P3-10） ==================== -->
    <!--
      tabs 提到页面级、面板留空，两张表各用自己的 ListPage：
      「薪资单」后端分页（全量 + 筛选），「我的薪资」是另一个接口且行可展开 —— 两张表的口径、
      列、行键都不同，塞进一个组件只会造出一堆 `activeTab === 'all'` 的分支。
      只有一个 tab 可见时（员工 / 只有 list 权限的人）按 `showTabs` 把 tab 条藏起来，与改造前一致。
    -->
    <t-tabs
      v-if="showTabs"
      v-model="activeTab"
      class="page-tabs"
      data-testid="hr-salary-tabs"
      @change="handleTabChange"
    >
      <t-tab-panel v-if="canList" value="all" label="薪资单" data-testid="hr-salary-tab-all" />
      <t-tab-panel v-if="canMy" value="mine" label="我的薪资" data-testid="hr-salary-tab-mine" />
    </t-tabs>

    <!-- ==================== ② 薪资单列表 ==================== -->
    <ListPage
      v-if="activeTab === 'all'"
      ref="allListRef"
      title="薪资单"
      meta-unit="单"
      testid="hr-salary"
      :columns="SALARY_LIST_COLUMNS"
      :filters="listFilters"
      :filter-state="filter"
      :initial-page="currentPage"
      :fetcher="listFetcher"
      :query-builder="listQueryBuilder"
      :page-size="SALARY_LIST_PAGE_SIZE"
      show-page-size
      @loaded="onListLoaded"
      @reset="onListReset"
      @row-click="handleRowClick"
    >
      <!-- 「共 N 单 · 本页 应发 … / 实发 …」：同元素后缀 -->
      <template #meta-extra>
        <span class="meta-extra">· 本页 {{ pageTotalsText(pageTotals) }}</span>
      </template>

      <template #head-extra>
        <span v-if="isListFiltered(filter)" class="filter-row__flag" data-testid="hr-salary-filtered">
          已筛选
        </span>
      </template>

      <!-- 空态：文案沿用 `listEmptyText`（改造前是 t-table 的 `:empty`） -->
      <template #empty>
        <div class="panel-empty" data-testid="hr-salary-empty">
          <p class="panel-empty__title">{{ listEmptyText(isListFiltered(filter)) }}</p>
        </div>
      </template>

      <!-- 薪资期间（`t-date-picker mode="month"` 不是声明式控件，走 custom 插槽） -->
      <template #filter-period_code>
        <t-date-picker
          v-model="filter.period_code"
          class="filter-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="薪资期间"
          clearable
          data-testid="hr-salary-filter-period"
          @change="handleListSearch"
        />
      </template>

        <!-- 单号：点一下进详情页（最常见的动作） -->
        <template #sheet_no="{ row }">
          <a
            class="link mono"
            :data-testid="`hr-salary-no-${row.id}`"
            @click.stop="openDetail(row)"
          >{{ row.sheet_no }}</a>
        </template>

        <!-- 期间 -->
        <template #period="{ row }">
          <span class="mono muted" data-testid="hr-salary-period">{{ sheetPeriodText(row) }}</span>
        </template>

        <!-- 状态 -->
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="salaryStatusTheme(row.status)">
            {{ row.status_label || salaryStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #employee_count="{ row }">
          <span class="mono">{{ row.employee_count }} 人</span>
        </template>

        <template #gross_total="{ row }">
          <span class="mono money">{{ moneyText(row.gross_total) }}</span>
        </template>

        <template #withhold_total="{ row }">
          <span class="mono money">{{ moneyText(row.withhold_total) }}</span>
        </template>

        <template #net_total="{ row }">
          <span class="mono money money--strong">{{ moneyText(row.net_total) }}</span>
        </template>

        <template #accrual_total="{ row }">
          <span class="mono money">{{ moneyText(row.accrual_total) }}</span>
        </template>

        <!-- 凭证：只有真拿到号的两张才显示（红冲凭证号后端不下发，不猜） -->
        <template #voucher="{ row }">
          <div v-if="sheetVoucherLines(row).length" class="voucher-cell">
            <span
              v-for="line in sheetVoucherLines(row)"
              :key="line"
              class="mono voucher-cell__line"
            >{{ line }}</span>
          </div>
          <span v-else class="muted">未过账</span>
        </template>

        <template #op="{ row }">
          <div class="row-ops">
            <t-button
              size="small"
              variant="text"
              data-testid="hr-salary-open"
              @click.stop="openDetail(row)"
            >详情</t-button>
            <t-button
              v-if="canExport"
              size="small"
              variant="text"
              :loading="exportingId === row.id"
              data-testid="hr-salary-export"
              @click.stop="handleExport(row)"
            >导出</t-button>
          </div>
        </template>
    </ListPage>

    <!-- ==================== ③ 我的薪资（员工自助） ==================== -->
    <ListPage
      v-else
      ref="myListRef"
      title="我的薪资"
      meta-unit="期"
      testid="hr-salary-my"
      row-key="detail_id"
      expandable
      :table-slots="['expandedRow']"
      :columns="SALARY_MY_COLUMNS"
      :filters="myFilters"
      :filter-state="myFilter"
      :initial-page="myPage"
      :fetcher="myFetcher"
      :query-builder="myQueryBuilder"
      :page-size="SALARY_LIST_PAGE_SIZE"
      show-page-size
      @loaded="onMyLoaded"
      @reset="onMyReset"
    >
      <template #meta-extra>
        <span class="meta-extra">
          · 只显示已定稿的工资单{{ myFilter.include_pending ? '（含在途）' : '' }}
        </span>
      </template>

      <template #empty>
        <div class="panel-empty" data-testid="hr-salary-my-empty">
          <p class="panel-empty__title">{{ mySalaryEmptyText(myPeriodFiltered) }}</p>
        </div>
      </template>

      <!-- 薪资期间（自定义插槽，与「薪资单」tab 同款） -->
      <template #filter-period_code>
        <t-date-picker
          v-model="myFilter.period_code"
          class="filter-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="薪资期间"
          clearable
          data-testid="hr-salary-my-period"
          @change="handleMySearch"
        />
      </template>

      <!-- 含在途单：只有能算薪资的人（HR / 财务）才提这个开关 -->
      <template #filter-include_pending>
        <label v-if="canCalc" class="filter-row__switch">
          <t-switch v-model="myFilter.include_pending" data-testid="hr-salary-my-pending" />
          <span class="filter-row__switch-text">含在途单</span>
          <t-tooltip content="草稿 / 已计算 / 审批中的金额还会变，默认不显示">
            <t-icon name="help-circle" />
          </t-tooltip>
        </label>
      </template>

        <template #sheet_status="{ row }">
          <t-tag size="small" variant="light" :theme="salaryStatusTheme(row.sheet_status)">
            {{ row.sheet_status_label || salaryStatusLabel(row.sheet_status) }}
          </t-tag>
        </template>

        <template #paid="{ row }">
          <t-tag size="small" variant="light" :theme="row.paid ? 'success' : 'warning'">
            {{ myPaidText(row) }}
          </t-tag>
        </template>

        <template #attendance="{ row }">
          <span class="muted mono">{{ slipAttendanceText(row) }}</span>
        </template>

        <template #gross_amount="{ row }">
          <span class="mono money">{{ moneyText(row.gross_amount) }}</span>
        </template>

        <template #withhold_amount="{ row }">
          <span class="mono money">{{ moneyText(row.withhold_amount) }}</span>
        </template>

        <template #net_amount="{ row }">
          <span class="mono money money--strong">{{ moneyText(row.net_amount) }}</span>
        </template>

        <!-- 展开 = 我这期工资由哪些项组成（员工真正想看的东西） -->
        <template #expandedRow="{ row }">
          <div class="my-items" data-testid="hr-salary-my-items">
            <div v-if="!row.items?.length" class="muted">这张工资条没有明细项（HR 可用明细接口补看）</div>
            <template v-else>
              <div
                v-for="item in myItemsSorted(row)"
                :key="item.item_code"
                class="my-item"
              >
                <t-tag size="small" variant="light" :theme="myItemTheme(item.item_type)">
                  {{ item.item_type_label }}
                </t-tag>
                <span class="my-item__name">{{ item.item_name }}</span>
                <span class="mono my-item__code muted">{{ item.item_code }}</span>
                <span class="mono money my-item__amount">{{ moneyText(item.amount) }}</span>
              </div>
              <div v-if="row.remark" class="my-item-remark">
                <span class="muted">HR 备注：</span>{{ row.remark }}
              </div>
            </template>
          </div>
        </template>
    </ListPage>

    <!-- ==================== ④ 试算 / 建单抽屉 ==================== -->
    <t-drawer
      v-model:visible="calcVisible"
      :header="calcMode === 'preview' ? '试算（不落库）' : '建单并保存'"
      size="880px"
      :footer="false"
      data-testid="hr-salary-calc-drawer"
      @close="closeCalc"
    >
      <div class="calc-body">
        <t-alert theme="info" :message="CALC_MODE_HINTS[calcMode]" class="calc-hint" />

        <div class="calc-form">
          <div class="calc-form__field">
            <label class="calc-form__label">薪资期间<b class="required">*</b></label>
            <t-date-picker
              v-model="calcForm.period_code"
              mode="month"
              value-type="YYYY-MM"
              format="YYYY-MM"
              placeholder="yyyy-MM"
              data-testid="hr-salary-calc-period"
            />
            <div class="calc-form__help muted">
              期间决定算哪个月的工资，也决定单号前缀（SAL-{{ periodShort(calcForm.period_code).replace('-', '') }}）
            </div>
          </div>

          <div class="calc-form__field">
            <label class="calc-form__label">单头备注</label>
            <t-textarea
              v-model="calcForm.remark"
              :maxlength="MAX_CALC_REMARK"
              placeholder="例如「11 月工资（含 10 月补发）」"
              :autosize="{ minRows: 2, maxRows: 4 }"
              data-testid="hr-salary-calc-remark"
            />
          </div>

          <div class="calc-form__actions">
            <t-button
              theme="primary"
              :loading="calcPending === 'preview'"
              :disabled="Boolean(calcPending) && calcPending !== 'preview'"
              data-testid="hr-salary-calc-run"
              @click="handleCalc('preview')"
            >
              试算
            </t-button>
            <t-button
              v-if="calcMode === 'persist'"
              theme="success"
              :loading="calcPending === 'persist'"
              :disabled="Boolean(calcPending) && calcPending !== 'persist'"
              data-testid="hr-salary-calc-persist"
              @click="handleCalc('persist')"
            >
              建单并保存
            </t-button>
            <span class="muted calc-form__tip">
              试算与建单走同一次计算，只是建单会真正落库并占单号
            </span>
          </div>
        </div>

        <t-alert
          v-if="calcError"
          theme="error"
          class="calc-error"
          data-testid="hr-salary-calc-error"
          :message="calcError"
        />

        <!-- 试算结果：先看合计与缺勤，再看逐人 -->
        <template v-if="calcResult">
          <div class="calc-metrics" data-testid="hr-salary-calc-metrics">
            <div class="metric">
              <span class="metric__label">期间</span>
              <span class="metric__value mono">{{ calcResultText(calcResult) }}</span>
            </div>
            <div class="metric">
              <span class="metric__label">应发合计</span>
              <span class="metric__value mono money" data-testid="hr-salary-calc-gross">
                {{ moneyText(calcResult.totals.gross_total) }}
              </span>
            </div>
            <div class="metric">
              <span class="metric__label">代扣合计</span>
              <span class="metric__value mono money">{{ moneyText(calcResult.totals.withhold_total) }}</span>
            </div>
            <div class="metric">
              <span class="metric__label">实发合计</span>
              <span class="metric__value mono money money--strong" data-testid="hr-salary-calc-net">
                {{ moneyText(calcResult.totals.net_total) }}
              </span>
            </div>
            <div class="metric">
              <span class="metric__label">计提合计</span>
              <span class="metric__value mono money">{{ moneyText(calcResult.totals.accrual_total) }}</span>
            </div>
            <div class="metric">
              <span class="metric__label">单位社保 / 公积金</span>
              <span class="metric__value mono money">
                {{ moneyText(calcResult.totals.si_company_total) }} /
                {{ moneyText(calcResult.totals.hf_company_total) }}
              </span>
            </div>
          </div>

          <t-alert
            v-if="missingText"
            theme="warning"
            class="calc-missing"
            data-testid="hr-salary-calc-missing"
            :message="missingText"
          />

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">逐人核对</span>
              <span class="panel-head__meta">共 {{ calcResult.employees.length }} 人</span>
            </div>
          </div>

          <t-table
            row-key="employee_id"
            size="small"
            hover
            max-height="360"
            :data="calcResult.employees"
            :columns="SALARY_SLIP_COLUMNS"
            data-testid="hr-salary-calc-slips"
          >
            <template #employee="{ row }">
              <span class="mono">{{ row.employee_no }}</span>
              <span class="slip-name">{{ row.employee_name }}</span>
            </template>
            <template #attendance="{ row }">
              <span class="muted mono">{{ slipAttendanceText(row) }}</span>
            </template>
            <template #earning_amount="{ row }">
              <span class="mono money">{{ moneyText(row.earning_amount) }}</span>
            </template>
            <template #gross_amount="{ row }">
              <span class="mono money">{{ moneyText(row.gross_amount) }}</span>
            </template>
            <template #withhold_amount="{ row }">
              <span class="mono money">{{ moneyText(row.withhold_amount) }}</span>
            </template>
            <template #net_amount="{ row }">
              <span class="mono money money--strong">{{ moneyText(row.net_amount) }}</span>
            </template>
          </t-table>

          <!-- 建单成功后：直接进详情页核对明细（新单从列表里点也能进，但多两步） -->
          <div v-if="createdSheet" class="calc-created" data-testid="hr-salary-calc-created">
            <t-alert
              theme="success"
              :message="`已建单 ${createdSheet.sheet_no}（${createdSheet.status_label}）`"
            />
            <t-button
              theme="primary"
              variant="outline"
              data-testid="hr-salary-calc-open"
              @click="openCreatedSheet"
            >进入详情页核对</t-button>
          </div>
        </template>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 薪资管理页（t_fc256a30）。
 *
 * 全部口径走纯函数（可单测）：筛选查询串 / 期间校验 / 试算报文 / 空态文案 /
 * 当前页合计 / 我的薪资分组在 `salary-list-kinds.ts`；状态词表与金额格式在
 * `salary-kinds.ts`。页面只做「拉数据 + 摆界面 + 弹提示 + 跳路由」。
 *
 * 权限码（`V0387__hr_menu_perms` 逐字一致）：`business:hr:salary:{list,detail,calc,export,my}`。
 * 页面入口由 `router/index.ts` 的 `meta.permission` 拦；页内 tab / 按钮由
 * `hasPermission` 拦 —— 无权限时不渲染入口，而不是让用户点出 403。
 */
import { computed, reactive, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  CALC_MODE_HINTS,
  MAX_CALC_REMARK,
  SALARY_LIST_COLUMNS,
  SALARY_LIST_PAGE_SIZE,
  SALARY_LIST_PERMS,
  SALARY_LIST_STATUS_OPTIONS,
  SALARY_MY_COLUMNS,
  SALARY_SLIP_COLUMNS,
  buildCalcPayload,
  buildListQuery,
  buildMyQuery,
  calcFormError,
  calcResultText,
  createSalarySheet,
  downloadSalaryExport,
  emptyCalcForm,
  emptyListFilter,
  emptyMyFilter,
  fetchMySalary,
  fetchSalarySheetPage,
  isListFiltered,
  listEmptyText,
  missingAttendanceText,
  moneyText,
  myItemTheme,
  myItemsSorted,
  myPaidText,
  mySalaryEmptyText,
  pageTotalsText,
  periodShort,
  previewSalarySheet,
  salaryExportFileName,
  salaryStatusLabel,
  salaryStatusTheme,
  sheetPeriodText,
  sheetVoucherLines,
  slipAttendanceText,
  sumSheetRows,
  type MySalaryFilter,
  type MySalaryRow,
  type SalaryCalcMode,
  type SalaryCalcResult,
  type SalaryListFilter,
  type SalarySheetRef,
  type SalarySheetRow,
} from '@/api/business/hr/salary';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

const router = useRouter();
const route = useRoute();
const userStore = useUserStore();

// ==================== 权限（tab / 按钮的显隐判据） ====================

/** 全量列表（`GET /salary/page`） */
const canList = computed(() => userStore.hasPermission(SALARY_LIST_PERMS.list));
/** 试算 / 建单（两条接口同一个码） */
const canCalc = computed(() => userStore.hasPermission(SALARY_LIST_PERMS.calc));
/** 导出工资表（列表行操作） */
const canExport = computed(() => userStore.hasPermission(SALARY_LIST_PERMS.export));
/** 我的薪资（员工自助） */
const canMy = computed(() => userStore.hasPermission(SALARY_LIST_PERMS.my));

/** 两个 tab 都可见时才画 tab 条（只有一个 tab 时它只是噪音）。 */
const showTabs = computed(() => canList.value && canMy.value);

/** 当前 tab：没有全量权限的员工直接落在「我的薪资」。 */
const activeTab = ref<'all' | 'mine'>(canList.value ? 'all' : 'mine');

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

// ==================== 薪资单列表（ListPage） ====================

/** 当前页（1 起；`ListPage` 每拉完一页回写，建单成功要回第 1 页） */
const currentPage = ref(1);
/** 「薪资单」ListPage 实例（建单成功后重拉） */
const allListRef = ref<InstanceType<typeof ListPage> | null>(null);
const filter = reactive<SalaryListFilter>(emptyListFilter());
const exportingId = ref('');

/** 当前页行（`ListPage` 的 `loaded` 镜像：页头「本页合计」要算它） */
const rows = ref<SalarySheetRow[]>([]);

/** 本页合计（只统计当前页：全局合计要看期间汇总，那是详情页的事） */
const pageTotals = computed(() => sumSheetRows(rows.value));

/**
 * 筛选项：期间 / 状态 / 单号（顺序 = 改造前筛选行）。
 *
 * 期间是 `t-date-picker mode="month"`，不是声明式控件，走 custom 插槽；
 * 期间与状态改了就查（与改造前的 `@change="handleSearch"` 一致），关键词按回车或点「查询」。
 */
const listFilters: FilterDef[] = [
  { key: 'period_code', type: 'custom', defaultValue: '', testid: 'hr-salary-filter-period' },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: SALARY_LIST_STATUS_OPTIONS,
    searchOnChange: true,
    testid: 'hr-salary-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '单号（SAL-202611）',
    width: 300,
    testid: 'hr-salary-filter-keyword',
  },
];

/** 拉一页（出参就是后端 `PageData`） */
const listFetcher: ListFetcher<SalarySheetRow> = (query) =>
  fetchSalarySheetPage(query as Parameters<typeof fetchSalarySheetPage>[0]);

/** 入参拼装：沿用 `buildListQuery`（空值整键缺席、期间/状态原样发） */
const listQueryBuilder: ListQueryBuilder = (_state, page, size) => buildListQuery(filter, page, size);

/** 一页拉完：镜像行（本页合计）+ 同步页码（删掉最后一页最后一条时组件已回夹） */
function onListLoaded(payload: { rows: SalarySheetRow[]; page: number }): void {
  rows.value = payload.rows;
  currentPage.value = payload.page;
}

/** 期间改了：回第 1 页重查（`t-date-picker` 的 @change 没有声明式对应） */
function handleListSearch(): void {
  void allListRef.value?.search();
}

/** 「重置」：组件已按声明清筛选并回第 1 页，这里补 `emptyListFilter()` 的形态与页码 */
function onListReset(): void {
  Object.assign(filter, emptyListFilter());
  currentPage.value = 1;
}

/** 重拉当前页（建单成功后） */
async function loadRows(): Promise<void> {
  await allListRef.value?.load(currentPage.value);
}

// ==================== 我的薪资（#46） ====================

/** 我的薪资页码（`ListPage` 每拉完一页回写） */
const myPage = ref(1);
/** 「我的薪资」ListPage 实例 */
const myListRef = ref<InstanceType<typeof ListPage> | null>(null);
const myFilter = reactive<MySalaryFilter>(emptyMyFilter());

/** 「我的薪资」是否在筛期间（空态文案 `mySalaryEmptyText` 的入参） */
const myPeriodFiltered = computed(() => Boolean((myFilter.period_code ?? '').trim()));

/** 筛选项：期间 + 「含在途单」（后者只给能算薪资的人显形，见模板里的 `v-if="canCalc"`） */
const myFilters: FilterDef[] = [
  { key: 'period_code', type: 'custom', defaultValue: '', testid: 'hr-salary-my-period' },
  { key: 'include_pending', type: 'custom', defaultValue: false, testid: 'hr-salary-my-pending' },
];

/** 拉一页（员工自助接口，只出本人） */
const myFetcher: ListFetcher<MySalaryRow> = (query) =>
  fetchMySalary(query as Parameters<typeof fetchMySalary>[0]);

/** 入参拼装：沿用 `buildMyQuery`（含在途开关只在这里换算成接口参数） */
const myQueryBuilder: ListQueryBuilder = (_state, page, size) => buildMyQuery(myFilter, page, size);

/** 一页拉完：同步页码 */
function onMyLoaded(payload: { page: number }): void {
  myPage.value = payload.page;
}

/** 期间改了：回第 1 页重查 */
function handleMySearch(): void {
  void myListRef.value?.search();
}

/** 「重置」：清筛选 + 回第 1 页（在途开关也回默认关） */
function onMyReset(): void {
  Object.assign(myFilter, emptyMyFilter());
  myPage.value = 1;
}

// ==================== tab / 导航 ====================

/**
 * tab 切换：只切 `activeTab`，数据由那个 tab 自己的 ListPage 首屏拉
 * （两个 tab 的接口 / 列 / 行键都不同，共用一个实例只会造出一堆分支）。
 */
function handleTabChange(value: string | number): void {
  // TDesign 的 change 回的是 `TabValue`（string | number），先归一再比
  activeTab.value = value === 'mine' ? 'mine' : 'all';
}

/** 进详情页（`/business/hr/salary/:id`，HR-43 已注册） */
function openDetail(row: SalarySheetRow): void {
  void router.push({ path: `/business/hr/salary/${row.id}` });
}

function handleRowClick(context: { row: unknown }): void {
  const row = context?.row as SalarySheetRow | undefined;
  if (row?.id) {
    openDetail(row);
  }
}

/** 行内导出（#45，xlsx 二进制） */
async function handleExport(row: SalarySheetRow): Promise<void> {
  if (exportingId.value) {
    return;
  }
  exportingId.value = row.id;
  try {
    const blob = await downloadSalaryExport(row.id);
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = salaryExportFileName(row.period_code, row.sheet_no);
    anchor.click();
    URL.revokeObjectURL(url);
    MessagePlugin.success(`已导出 ${salaryExportFileName(row.period_code, row.sheet_no)}`);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    exportingId.value = '';
  }
}

// ==================== 试算 / 建单 ====================

const calcVisible = ref(false);
const calcMode = ref<SalaryCalcMode>('preview');
const calcForm = reactive(emptyCalcForm());
const calcPending = ref<'' | SalaryCalcMode>('');
const calcError = ref('');
const calcResult = ref<SalaryCalcResult | null>(null);
const createdSheet = ref<SalarySheetRef | null>(null);

/** 缺考勤提示（一句话，点名到人但只列前 5 个） */
const missingText = computed(() => missingAttendanceText(calcResult.value?.missing_attendance));

/** 打开抽屉（试算入口 / 建单入口共用；建单模式预填表单） */
function openCalc(mode: SalaryCalcMode): void {
  calcMode.value = mode;
  if (!calcVisible.value) {
    Object.assign(calcForm, emptyCalcForm());
    calcResult.value = null;
    createdSheet.value = null;
  }
  calcError.value = '';
  calcVisible.value = true;
}

function closeCalc(): void {
  calcVisible.value = false;
  calcPending.value = '';
  calcError.value = '';
}

/**
 * 跑一次计算（试算 / 建单）。
 *
 * 建单先弹确认：它会真的落库并占一个单号（试算不占），点错要删单才能收回。
 */
function handleCalc(mode: SalaryCalcMode): void {
  const error = calcFormError(calcForm);
  if (error) {
    calcError.value = error;
    MessagePlugin.warning(error);
    return;
  }
  if (mode === 'persist') {
    const dialog = DialogPlugin.confirm({
      header: '建单并保存',
      body:
        `会为期间 ${calcForm.period_code} 生成一张薪资单并落库（占一个单号 SAL-` +
        `${calcForm.period_code.replace('-', '')}-00000x），生成后进详情页核对、提交审批。`,
      confirmBtn: '建单',
      onConfirm: () => {
        dialog.hide();
        void runCalc('persist');
      },
      onClose: () => dialog.hide(),
    });
    return;
  }
  void runCalc('preview');
}

async function runCalc(mode: SalaryCalcMode): Promise<void> {
  if (calcPending.value) {
    return;
  }
  calcPending.value = mode;
  calcError.value = '';
  try {
    const payload = buildCalcPayload(calcForm, mode);
    const result =
      mode === 'preview'
        ? await previewSalarySheet(payload)
        : await createSalarySheet(payload);
    calcResult.value = result;
    if (mode === 'preview') {
      MessagePlugin.success(`试算完成：${result.totals.employee_count} 人，未落库`);
      return;
    }
    createdSheet.value = result.sheet;
    MessagePlugin.success(`已建单 ${result.sheet?.sheet_no ?? ''}（${result.sheet?.status_label ?? ''}）`);
    // 新单立刻出现在列表里（建单是列表页最常见的「下一动作」）
    currentPage.value = 1;
    await loadRows();
  } catch (error) {
    calcError.value = messageOf(error);
    MessagePlugin.error(calcError.value);
  } finally {
    calcPending.value = '';
  }
}

/** 建单成功后进详情页核对明细（同一张单，多两步从列表点） */
function openCreatedSheet(): void {
  const sheet = createdSheet.value;
  if (!sheet) {
    return;
  }
  calcVisible.value = false;
  void router.push({ path: `/business/hr/salary/${sheet.id}` });
}

// ==================== 首屏 / 深链 ====================

/**
 * 凭证卡的「查看薪资单」深链：`/business/hr/salary?sheet_no=SAL-202611-0001`
 * （`views/business/erp/shared/source-route.ts` 的 `PAYROLL_ACCRUAL` / `PAYROLL_PAYMENT`
 * 是 `deepLink='filter'`，落点就是本页的单号筛选）。
 *
 * 没 `salary:list` 的账号（员工）不筛：他被路由送到「我的薪资」tab，
 * 那 tab 是全量列表之外的接口，单号筛选对它没有意义 —— 直接忽略参数比给个空表好。
 *
 * @returns 有没有吃掉这个参数（调用方据此决定是否还要按默认方式拉数据）
 */
function applyRouteQuery(): boolean {
  const sheetNo = String(route.query.sheet_no ?? '').trim();
  if (!sheetNo || !canList.value) {
    return false;
  }
  activeTab.value = 'all';
  filter.keyword = sheetNo;
  currentPage.value = 1;
  return true;
}

// 深链要在 `ListPage` 首屏**之前**解析：父 setup 早于子组件 setup，
// 放在这里就等于「首屏第一枪就带上单号筛选」，不会再白跑一次全量查询。
applyRouteQuery();

// 页面已打开时又从凭证点进来（`router.push` 只改 query 不重建组件）也要跟上
watch(
  () => route.query.sheet_no,
  () => {
    if (applyRouteQuery()) {
      void allListRef.value?.search();
    }
  },
);
</script>

<style scoped lang="less">
.hr-salary-page {
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
  max-width: 780px;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 13px;
  line-height: 1.6;
}

.page-head__actions {
  display: flex;
  flex: 0 0 auto;
  gap: 8px;
}

/* 页头计数后缀（`#meta-extra` 与「共 N 单」同一元素） */
.meta-extra {
  margin-left: 6px;
}

/* 空态（改造前是 t-table 的 `:empty` 文案，现在插槽自绘） */
.panel-empty {
  padding: 40px 16px;
  text-align: center;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  line-height: 20px;
}

.page-tabs {
  margin-bottom: 8px;
}

.filter-row__period {
  width: 160px;
}

.filter-row__flag {
  color: var(--td-brand-color, #0052d9);
  font-size: 12px;
}

.filter-row__switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: var(--td-text-color-secondary, #00000099);
  font-size: 13px;
}

.filter-row__switch-text {
  user-select: none;
}

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.muted {
  color: var(--td-text-color-placeholder, #0000004d);
}

.money {
  font-variant-numeric: tabular-nums;
}

.money--strong {
  font-weight: 600;
}

.link {
  color: var(--td-brand-color, #0052d9);
  cursor: pointer;

  &:hover {
    text-decoration: underline;
  }
}

.voucher-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.voucher-cell__line {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.row-ops {
  display: flex;
  gap: 4px;
}

.my-items {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px 12px;
}

.my-item {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
}

.my-item__name {
  min-width: 120px;
}

.my-item__code {
  font-size: 12px;
}

.my-item__amount {
  margin-left: auto;
}

.my-item-remark {
  padding-top: 4px;
  border-top: 1px dashed var(--td-component-stroke, #e7e7e7);
  color: var(--td-text-color-secondary, #00000099);
  font-size: 12px;
}

.calc-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.calc-hint {
  :deep(.t-alert__message) {
    line-height: 1.6;
  }
}

.calc-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.calc-form__field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.calc-form__label {
  color: var(--td-text-color-secondary, #00000099);
  font-size: 13px;
}

.calc-form__help {
  font-size: 12px;
}

.calc-form__actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.calc-form__tip {
  font-size: 12px;
}

.required {
  color: var(--td-error-color, #d54941);
}

.calc-metrics {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
  padding: 12px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer, #f3f3f3);
}

.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.metric__label {
  color: var(--td-text-color-placeholder, #0000004d);
  font-size: 12px;
}

.metric__value {
  font-size: 14px;
}

.slip-name {
  margin-left: 8px;
}

.calc-created {
  display: flex;
  align-items: center;
  gap: 12px;
}
</style>
