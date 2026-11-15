<template>
  <div class="online-page">
    <!-- 筛选 / 分页 / 勾选交给 ListPage（P3-13）；本页只留列、筛选、拉数、行/批量动作 -->
    <ListPage
      ref="listRef"
      title="在线用户"
      testid="online"
      table-testid="online-table"
      meta-testid="online-meta"
      btn-prefix="btn-online"
      meta-unit="人在线"
      :page-size="ONLINE_PAGE_SIZE"
      row-key="token_id"
      selectable
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      @loaded="onLoaded"
      @selection-change="onSelectionChange"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.force]"
          theme="danger"
          variant="outline"
          data-testid="btn-online-batch-force"
          :disabled="selectedIds.length === 0"
          :loading="isBatchLoading"
          @click="handleBatchForceLogout"
        >
          <template #icon><t-icon name="poweroff" /></template>
          批量下线
        </t-button>
      </template>

      <template #ip="{ row }">{{ orDash(row.ip) }}</template>
      <template #location="{ row }">{{ orDash(row.location) }}</template>
      <template #browser="{ row }">{{ orDash(row.browser) }}</template>
      <template #os_type="{ row }">{{ orDash(row.os_type) }}</template>
      <template #login_time="{ row }">{{ formatDateTime(row.login_time) }}</template>
      <template #expire_at="{ row }">{{ formatDateTime(row.expire_at) }}</template>
      <template #op="{ row }">
        <t-link
          v-privilege="[PERM.force]"
          theme="danger"
          data-testid="btn-online-force"
          @click.stop="confirmForceLogout(row)"
        >
          强制下线
        </t-link>
      </template>
    </ListPage>
  </div>
</template>

<script setup lang="ts">
import { reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { forceLogout, pageOnline, type OnlineRes } from '@/api/support/online-api';
import { ApiError } from '@/api/request';
import { formatDateTime } from '@/utils/format';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQuery, ListQueryBuilder } from '@/components/ListPage/types';

/**
 * 本页权限码（与 `sys_menu.auth_code` 一致）。
 *
 * 就近放在页面里（不改 `src/constants/permissions.ts`）：那个文件是全站单点，
 * 多张卡会同时往它里面加码，谁后提交谁撞冲突。
 */
const PERM = {
  /** 强制下线会话 */
  force: 'support:online:force',
} as const;

/** 每页条数（改造前是 10，保持手感一致） */
const ONLINE_PAGE_SIZE = 10;

/** 查询条件（`''` = 不过滤） */
interface OnlineFilterState {
  /** 用户名（模糊） */
  username: string;
}

/** 筛选状态（`queryBuilder` 与 `isFiltered` 读的是同一个对象） */
const filter = reactive<OnlineFilterState>({ username: '' });

const columns: PrimaryTableCol[] = [
  { colKey: 'username', title: '用户名', width: 140 },
  { colKey: 'ip', title: 'IP 地址', width: 150 },
  { colKey: 'location', title: '登录地点', width: 150 },
  { colKey: 'browser', title: '浏览器', width: 140, ellipsis: true },
  { colKey: 'os_type', title: '操作系统', width: 130 },
  { colKey: 'login_time', title: '登录时间', width: 180 },
  { colKey: 'expire_at', title: '到期时间', width: 180 },
  { colKey: 'op', title: '操作', width: 110, fixed: 'right' },
];

/** 筛选行：一个用户名关键词（回车 / 点查询生效，与原页一致） */
const filters: FilterDef[] = [
  { key: 'username', type: 'keyword', placeholder: '用户名', width: 220, testid: 'online-filter-username' },
];

/** 拉一页 */
const fetcher: ListFetcher<OnlineRes> = (query) =>
  pageOnline(query as Parameters<typeof pageOnline>[0]);

/**
 * 入参拼装。
 *
 * 键名与改造前逐字一致（`page` / `page_size` / `username`），空用户名整键缺席 ——
 * 传 `username=` 后端会当成「模糊匹配空串」，等于查不到人。
 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const username = String((state as OnlineFilterState).username ?? '').trim();
  return {
    page,
    page_size: pageSize,
    ...(username ? { username } : {}),
  };
};

/** 当前页的行（批量下线要按 token_id 找用户名） */
const currentRows = ref<OnlineRes[]>([]);
/** 已勾选的会话标识（token_id） */
const selectedIds = ref<string[]>([]);
/** 批量下线请求进行中 */
const isBatchLoading = ref(false);
/** ListPage 实例（动作完重拉当前页） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 一页拉完：镜像行（批量下线按行找用户名） */
function onLoaded(payload: { rows: OnlineRes[] }): void {
  currentRows.value = payload.rows;
}

/** 勾选变化（组件抛的是 `token_id` 列表） */
function onSelectionChange(ids: string[]): void {
  selectedIds.value = ids;
}

/** 空字符串统一展示为 `-` */
function orDash(value: string): string {
  return value && value.trim() ? value : '-';
}

/** 统一展示接口错误 */
function showError(message: string): void {
  void MessagePlugin.error(message);
}

/** 重新拉当前页 */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

/** 强制下线二次确认 */
function confirmForceLogout(row: OnlineRes): void {
  const dialog = DialogPlugin.confirm({
    header: '强制下线',
    body: `确认将用户「${row.username}」强制下线？该操作会立即使其令牌失效。`,
    theme: 'danger',
    confirmBtn: { content: '强制下线', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await forceLogout(row.token_id);
          void MessagePlugin.success(`已将「${row.username}」强制下线`);
          await reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : '强制下线失败');
        }
      })();
    },
  });
}

/**
 * 批量强制下线。
 *
 * 后端仅提供单个会话下线接口，故在前端对已勾选的会话逐个调用 `forceLogout`，
 * 并统计成功/失败数量；失败的会话保留在提示中，便于用户重试。
 */
async function handleBatchForceLogout(): Promise<void> {
  if (selectedIds.value.length === 0) {
    return;
  }
  const targetIds = [...selectedIds.value];
  isBatchLoading.value = true;
  let successCount = 0;
  const failedUsers: string[] = [];
  for (const tokenId of targetIds) {
    const target = currentRows.value.find((item) => item.token_id === tokenId);
    try {
      await forceLogout(tokenId);
      successCount += 1;
    } catch {
      failedUsers.push(target?.username ?? tokenId);
    }
  }
  isBatchLoading.value = false;
  if (failedUsers.length > 0) {
    showError(`以下用户强制下线失败：${failedUsers.join('、')}`);
  }
  if (successCount > 0) {
    void MessagePlugin.success(`已强制下线 ${successCount} 个会话`);
  }
  // 组件重拉不会自己清勾选（只有换页/查询/重置才清），这里显式清掉再刷
  listRef.value?.clearSelection();
  selectedIds.value = [];
  await reloadList();
}
</script>

<style scoped lang="less">
.online-page {
  padding: 16px;
}
</style>
