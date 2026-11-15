<!--
  日志（support/log）：五个 tab（操作 / 登录 / 审计 / 安全 / 改密），一个 ListPage（P3-13）。

  # 为什么只用一个 ListPage 而不是五个
  五个 tab 的筛选条件完全一样（用户名 + 时间区间），差别只有「列」和「拉哪个接口」。
  tab 切换 = 换列 + 换 fetcher，然后 `search()` 回第 1 页重拉；换五个实例要复制五份筛选状态，
  用户切回来还得靠列表记忆找回页码。

  # 单元格槽按 colKey 命中
  `operation` 这一列在两个 tab 里含义不同（操作日志=原样文本，审计日志=字典中文），
  两个 tab 的列 colKey 同名，所以槽里按 `activeTab` 分叉 —— 这是本页唯一的分叉点。
-->
<template>
  <div class="log-page">
    <t-tabs v-model="activeTab" class="page-tabs" data-testid="log-tabs" @change="handleTabChange">
      <t-tab-panel v-for="tab in logTabs" :key="tab.value" :value="tab.value" :label="tab.label" />
    </t-tabs>

    <ListPage
      ref="listRef"
      title="日志"
      testid="log"
      table-testid="log-table"
      meta-testid="log-meta"
      btn-prefix="btn-log"
      :page-size="LOG_PAGE_SIZE"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
    >
      <template #filter-time_range="{ value, set }">
        <t-date-range-picker
          :value="rangeValueOf(value)"
          allow-input
          clearable
          :placeholder="['开始时间', '结束时间']"
          data-testid="log-filter-time-range"
          @change="(next: unknown) => set(normalizeRange(next))"
        />
      </template>

      <!-- 清理仅操作日志 tab 可用 -->
      <template #toolbar>
        <t-button
          v-if="activeTab === 'operation'"
          v-privilege="['support:log:clean']"
          theme="danger"
          variant="outline"
          data-testid="btn-log-clean"
          @click="openClean"
        >
          <template #icon><t-icon name="clear" /></template>
          清理日志
        </t-button>
      </template>

      <!-- 操作日志专用单元格 -->
      <template #request_method="{ row }">
        <t-tag :theme="methodTheme(toText(row.request_method))" variant="light">
          {{ toText(row.request_method) }}
        </t-tag>
      </template>
      <template #status_code="{ row }">
        <t-tag :theme="statusCodeTheme(Number(row.status_code))" variant="light">
          {{ toText(row.status_code) }}
        </t-tag>
      </template>

      <!-- 通用单元格：时间 / IP / 文本 -->
      <template #created_at="{ row }">{{ formatDateTime(toText(row.created_at)) }}</template>
      <template #ip="{ row }">{{ orDash(toText(row.ip)) }}</template>
      <template #username="{ row }">{{ textDash(row, 'username') }}</template>
      <template #request_path="{ row }">{{ textDash(row, 'request_path') }}</template>
      <template #duration_ms="{ row }">{{ textDash(row, 'duration_ms') }}</template>
      <template #user_agent="{ row }">{{ textDash(row, 'user_agent') }}</template>
      <template #error_message="{ row }">{{ textDash(row, 'error_message') }}</template>
      <template #target_id="{ row }">{{ textDash(row, 'target_id') }}</template>
      <template #request_id="{ row }">{{ textDash(row, 'request_id') }}</template>
      <template #detail="{ row }">{{ textDash(row, 'detail') }}</template>

      <!-- 操作列（`operation`）在两个 tab 语义不同：操作日志原样、审计日志译字典 -->
      <template #operation="{ row }">
        <span v-if="activeTab === 'audit'">{{ dictLabel(DICT_LOG_OPERATION, toText(row.operation)) }}</span>
        <span v-else>{{ textDash(row, 'operation') }}</span>
      </template>

      <!-- 字典映射单元格：编码 -> 中文 -->
      <template #auth_type="{ row }">
        {{ dictLabel(DICT_LOG_AUTH_TYPE, toText(row.auth_type)) }}
      </template>
      <template #target_type="{ row }">
        {{ dictLabel(DICT_LOG_TARGET_TYPE, toText(row.target_type)) }}
      </template>
      <template #event_type="{ row }">
        {{ dictLabel(DICT_LOG_SECURITY_EVENT, toText(row.event_type)) }}
      </template>
      <template #change_type="{ row }">
        {{ dictLabel(DICT_LOG_PASSWORD_CHANGE_TYPE, toText(row.change_type)) }}
      </template>

      <template #auth_status="{ row }">
        <t-tag v-if="isBlank(row.auth_status)" variant="light">-</t-tag>
        <t-tag v-else :theme="isSuccessStatus(row.auth_status) ? 'success' : 'danger'" variant="light">
          {{ isSuccessStatus(row.auth_status) ? '成功' : '失败' }}
        </t-tag>
      </template>
      <template #change_status="{ row }">
        <t-tag v-if="isBlank(row.change_status)" variant="light">-</t-tag>
        <t-tag v-else :theme="isSuccessStatus(row.change_status) ? 'success' : 'danger'" variant="light">
          {{ isSuccessStatus(row.change_status) ? '成功' : '失败' }}
        </t-tag>
      </template>

      <template #op="{ row }">
        <t-link theme="primary" data-testid="btn-log-detail" @click.stop="openDetail(row)">详情</t-link>
      </template>
    </ListPage>

    <!-- 详情抽屉：仅操作日志存在详情接口 -->
    <t-drawer
      v-model:visible="detailVisible"
      header="日志详情"
      size="640px"
      :footer="false"
      destroy-on-close
      data-testid="log-detail-drawer"
    >
      <t-loading :loading="detailLoading">
        <t-descriptions v-if="detail" :column="1" bordered>
          <t-descriptions-item label="操作">{{ orDash(detail.operation) }}</t-descriptions-item>
          <t-descriptions-item label="操作人">{{ orDash(detail.username) }}</t-descriptions-item>
          <t-descriptions-item label="请求">
            {{ detail.request_method }} {{ detail.request_path }}
          </t-descriptions-item>
          <t-descriptions-item label="状态码">
            <t-tag :theme="statusCodeTheme(detail.status_code)" variant="light">
              {{ detail.status_code }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="耗时">{{ detail.duration_ms }} ms</t-descriptions-item>
          <t-descriptions-item label="客户端 IP">{{ orDash(detail.ip) }}</t-descriptions-item>
          <t-descriptions-item label="目标资源">
            {{ orDash(detail.target_type) }} / {{ orDash(detail.target_id) }}
          </t-descriptions-item>
          <t-descriptions-item label="请求链路">{{ orDash(detail.request_id) }}</t-descriptions-item>
          <t-descriptions-item label="发生时间">{{ formatDateTime(detail.created_at) }}</t-descriptions-item>
          <t-descriptions-item label="详情">{{ orDash(detail.detail) }}</t-descriptions-item>
          <t-descriptions-item label="User-Agent">{{ orDash(detail.user_agent) }}</t-descriptions-item>
          <t-descriptions-item label="请求参数">
            <pre class="log-page__params">{{ requestParamsText }}</pre>
          </t-descriptions-item>
          <t-descriptions-item label="响应数据">
            <pre class="log-page__params">{{ responseDataText }}</pre>
          </t-descriptions-item>
        </t-descriptions>
      </t-loading>
    </t-drawer>

    <t-dialog
      v-model:visible="cleanVisible"
      header="清理历史日志"
      :confirm-btn="{ content: '确定清理', theme: 'danger', loading: cleaning }"
      width="440px"
      destroy-on-close
      data-testid="log-clean-dialog"
      @confirm="submitClean"
    >
      <t-form :data="cleanForm" label-width="110px">
        <t-form-item label="保留天数">
          <t-input-number v-model="cleanForm.days" :min="1" :max="3650" theme="normal" />
        </t-form-item>
        <t-form-item label-width="0">
          <span class="log-page__count">将删除 {{ cleanForm.days }} 天前的全部操作日志</span>
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  cleanLogs,
  getLog,
  pageAuditLogs,
  pageAuthLogs,
  pageLogs,
  pagePasswordLogs,
  pageSecurityLogs,
  type LogDetailRes,
  type LogPageQuery,
  type RawLogPageQuery,
  type RawLogRow,
} from '@/api/support/log-api';
import { ApiError, type PageData } from '@/api/request';
import { formatDateTime } from '@/utils/format';
import { useDictStore } from '@/stores/dict';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
} from '@/components/ListPage/types';

/** 日志字段对应的字典编码 */
const DICT_LOG_AUTH_TYPE = 'log_auth_type';
const DICT_LOG_OPERATION = 'log_operation';
const DICT_LOG_TARGET_TYPE = 'log_target_type';
const DICT_LOG_SECURITY_EVENT = 'log_security_event';
const DICT_LOG_PASSWORD_CHANGE_TYPE = 'log_password_change_type';

/** 日志页面需预加载的字典编码集合 */
const LOG_DICT_CODES = [
  DICT_LOG_AUTH_TYPE,
  DICT_LOG_OPERATION,
  DICT_LOG_TARGET_TYPE,
  DICT_LOG_SECURITY_EVENT,
  DICT_LOG_PASSWORD_CHANGE_TYPE,
];

/** 每页条数（改造前是 10，保持手感一致） */
const LOG_PAGE_SIZE = 10;

/** 日志类型 tab 枚举值 */
type LogTab = 'operation' | 'login' | 'audit' | 'security' | 'password';

/** tab 配置 */
const logTabs: { value: LogTab; label: string }[] = [
  { value: 'operation', label: '操作日志' },
  { value: 'login', label: '登录日志' },
  { value: 'audit', label: '审计日志' },
  { value: 'security', label: '安全日志' },
  { value: 'password', label: '改密日志' },
];

/** 非操作日志的分页查询器：按 tab 路由到对应后端接口 */
const rawFetchers: Record<
  Exclude<LogTab, 'operation'>,
  (query: RawLogPageQuery) => Promise<PageData<RawLogRow>>
> = {
  login: pageAuthLogs,
  audit: pageAuditLogs,
  security: pageSecurityLogs,
  password: pagePasswordLogs,
};

/** 操作日志列 */
const OPERATION_COLUMNS: ColumnDef[] = [
  { colKey: 'created_at', title: '发生时间', width: 180 },
  { colKey: 'username', title: '操作人', width: 130 },
  { colKey: 'operation', title: '操作', ellipsis: true },
  { colKey: 'request_method', title: '方法', width: 90 },
  { colKey: 'request_path', title: '请求路径', width: 220, ellipsis: true },
  { colKey: 'status_code', title: '状态码', width: 90 },
  { colKey: 'ip', title: '客户端 IP', width: 140 },
  { colKey: 'duration_ms', title: '耗时(ms)', width: 100 },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right', exportable: false },
];

/** 登录（认证）日志列 */
const LOGIN_COLUMNS: ColumnDef[] = [
  { colKey: 'created_at', title: '时间', width: 180 },
  { colKey: 'username', title: '用户名', width: 130 },
  { colKey: 'auth_type', title: '认证类型', width: 110 },
  { colKey: 'auth_status', title: '状态', width: 100 },
  { colKey: 'ip', title: 'IP 地址', width: 140 },
  { colKey: 'user_agent', title: 'User-Agent', width: 260, ellipsis: true },
  { colKey: 'error_message', title: '失败原因', width: 180, ellipsis: true },
];

/** 审计日志列 */
const AUDIT_COLUMNS: ColumnDef[] = [
  { colKey: 'created_at', title: '时间', width: 180 },
  { colKey: 'username', title: '用户名', width: 130 },
  { colKey: 'operation', title: '操作', width: 180, ellipsis: true },
  { colKey: 'target_type', title: '目标类型', width: 140 },
  { colKey: 'target_id', title: '目标 ID', width: 180, ellipsis: true },
  { colKey: 'ip', title: 'IP 地址', width: 140 },
  { colKey: 'request_id', title: '请求链路', width: 200, ellipsis: true },
];

/** 安全日志列 */
const SECURITY_COLUMNS: ColumnDef[] = [
  { colKey: 'created_at', title: '时间', width: 180 },
  { colKey: 'username', title: '用户名', width: 130 },
  { colKey: 'event_type', title: '事件类型', width: 160 },
  { colKey: 'ip', title: 'IP 地址', width: 140 },
  { colKey: 'detail', title: '详情', ellipsis: true },
];

/** 改密日志列 */
const PASSWORD_COLUMNS: ColumnDef[] = [
  { colKey: 'created_at', title: '时间', width: 180 },
  { colKey: 'username', title: '用户名', width: 130 },
  { colKey: 'change_type', title: '变更类型', width: 140 },
  { colKey: 'change_status', title: '状态', width: 100 },
  { colKey: 'ip', title: 'IP 地址', width: 140 },
  { colKey: 'error_message', title: '说明', width: 180, ellipsis: true },
];

/** tab 与列配置映射 */
const tabColumns: Record<LogTab, ColumnDef[]> = {
  operation: OPERATION_COLUMNS,
  login: LOGIN_COLUMNS,
  audit: AUDIT_COLUMNS,
  security: SECURITY_COLUMNS,
  password: PASSWORD_COLUMNS,
};

/** 筛选状态：用户名 + 时间区间（`time_range` 走 `#filter-time_range` 自绘） */
interface LogFilterState {
  /** 用户名（模糊） */
  username: string;
  /** 时间区间 `[start, end]`（空数组 = 不限） */
  time_range: string[];
}

const activeTab = ref<LogTab>('operation');
const dictStore = useDictStore();
const filter = reactive<LogFilterState>({ username: '', time_range: [] });
/** ListPage 实例（tab 切换 / 清理后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 当前 tab 对应的表格列 */
const columns = computed<ColumnDef[]>(() => tabColumns[activeTab.value]);

/** 筛选行：用户名（回车生效）+ 时间区间（值一变就查，与原页一致） */
const filters: FilterDef[] = [
  { key: 'username', type: 'keyword', placeholder: '用户名', width: 220, testid: 'log-filter-username' },
  { key: 'time_range', type: 'custom', defaultValue: [], testid: 'log-filter-time-range' },
];

/** 拉一页：tab 决定接口（入参键名与改造前逐字一致） */
const fetcher: ListFetcher<RawLogRow> = async (query) => {
  const params = query as RawLogPageQuery;
  if (activeTab.value === 'operation') {
    const data = await pageLogs(params as LogPageQuery);
    // 操作日志行字段是 `LogRes`，展示侧统一按整行 JSON 读（`RawLogRow`）
    return { ...data, list: data.list as unknown as RawLogRow[] };
  }
  return rawFetchers[activeTab.value](params);
};

/**
 * 入参拼装。
 *
 * 空用户名 / 空时间整键缺席（`?username=` 会被后端当「等于空串」过滤，查出 0 行）。
 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const { username, time_range: range } = state as LogFilterState;
  const keyword = String(username ?? '').trim();
  const [start, end] = normalizeRange(range);
  return {
    page,
    page_size: pageSize,
    ...(keyword ? { username: keyword } : {}),
    ...(start ? { start_time: start } : {}),
    ...(end ? { end_time: end } : {}),
  };
};

/** 区间值规整成 `[start, end]`（控件给什么形状都吃） */
function normalizeRange(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item) => item !== null && item !== undefined && item !== '').map(String);
}

/** 日期区间控件的受控值 */
function rangeValueOf(value: unknown): string[] {
  return normalizeRange(value);
}

/** 切 tab：回第 1 页重拉（列已经跟着 activeTab 换掉） */
function handleTabChange(): void {
  listRef.value?.search();
}

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<LogDetailRes | null>(null);

const cleanVisible = ref(false);
const cleaning = ref(false);
const cleanForm = reactive({ days: 30 });

/** 请求参数文本，反序列化为格式化 JSON */
const requestParamsText = computed(() => formatJson(detail.value?.request_params));

/** 响应数据文本，反序列化为格式化 JSON */
const responseDataText = computed(() => formatJson(detail.value?.response_data));

/** 判定值是否为空（null / undefined / 空字符串） */
function isBlank(value: unknown): boolean {
  return value === undefined || value === null || value === '';
}

/** 空字符串统一展示为 `-` */
function orDash(value: string): string {
  return value && value.trim() ? value : '-';
}

/** 字典标签：命中返回中文，未命中回退原始编码，空值展示为 `-` */
function dictLabel(dictCode: string, value: string): string {
  if (!value) {
    return '-';
  }
  return dictStore.getLabel(dictCode, value);
}

/** 预加载日志字典；单个失败不影响其余字典与列表渲染 */
async function preloadLogDicts(): Promise<void> {
  await Promise.all(
    LOG_DICT_CODES.map((code) =>
      dictStore.loadItems(code).catch(() => []),
    ),
  );
}

/** 未知字段安全转字符串，空值返回空串（由单元格占位符兜底） */
function toText(value: unknown): string {
  if (isBlank(value)) {
    return '';
  }
  return typeof value === 'string' ? value : String(value);
}

/** 通用单元格文本：支持任意 JSON 行字段（对象走 JSON.stringify） */
function cellText(row: RawLogRow, key: string): string {
  const value = row[key];
  if (isBlank(value)) {
    return '';
  }
  return typeof value === 'object' ? JSON.stringify(value) : String(value);
}

/** 通用文本单元格：空值显示 `-`（与改造前 `cell-empty-content="-"` 同观感） */
function textDash(row: RawLogRow, key: string): string {
  return cellText(row, key) || '-';
}

/** 判定状态是否为成功（兼容数字 / 字符串 / 布尔） */
function isSuccessStatus(value: unknown): boolean {
  return value === 1 || value === '1' || value === true || value === 'success' || value === 'SUCCESS';
}

/** 请求方法标签主题色 */
function methodTheme(method: string): 'primary' | 'success' | 'warning' | 'danger' {
  if (method === 'POST') {
    return 'success';
  }
  if (method === 'PUT' || method === 'PATCH') {
    return 'warning';
  }
  return method === 'DELETE' ? 'danger' : 'primary';
}

/** 状态码标签主题色 */
function statusCodeTheme(statusCode: number): 'success' | 'danger' {
  return statusCode >= 200 && statusCode < 400 ? 'success' : 'danger';
}

/** 将任意 JSON 值格式化为可读文本，空值返回占位符 */
function formatJson(value: unknown): string {
  if (value === undefined || value === null) {
    return '-';
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** 统一展示接口错误 */
function showError(message: string): void {
  void MessagePlugin.error(message);
}

async function openDetail(row: RawLogRow): Promise<void> {
  detail.value = null;
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await getLog(row.id);
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '日志详情加载失败');
  } finally {
    detailLoading.value = false;
  }
}

function openClean(): void {
  cleanForm.days = 30;
  cleanVisible.value = true;
}

async function submitClean(): Promise<void> {
  cleaning.value = true;
  try {
    const result = await cleanLogs(cleanForm.days);
    void MessagePlugin.success(`已清理 ${result.removed} 条日志`);
    cleanVisible.value = false;
    listRef.value?.search();
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '日志清理失败');
  } finally {
    cleaning.value = false;
  }
}

onMounted(() => {
  void preloadLogDicts();
});
</script>

<style scoped lang="less">
.log-page {
  padding: 16px;

  .page-tabs {
    margin-bottom: 12px;
  }

  &__count {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__params {
    margin: 0;
    font-family: 'SFMono-Regular', Consolas, 'Liberation Mono', monospace;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-all;
  }
}
</style>
