<template>
  <div class="notice-message-page">
    <t-card :bordered="false" class="inbox-card">
      <div class="inbox-header">
        <div class="inbox-header__title">
          <t-icon name="notification" class="inbox-header__icon" />
          <span>消息中心</span>
          <t-tag v-if="unreadCount > 0" theme="danger" size="small" variant="light">{{ unreadCount }} 条未读</t-tag>
        </div>
        <t-space>
          <t-radio-group v-model="readFilter" variant="default-filled" size="small" @change="handleFilterChange">
            <t-radio-button :value="'unread'">未读</t-radio-button>
            <t-radio-button :value="'read'">已读</t-radio-button>
            <t-radio-button :value="'all'">全部</t-radio-button>
          </t-radio-group>
          <t-button size="small" variant="outline" :disabled="unreadCount === 0" @click="handleReadAll">
            <template #icon><t-icon name="check" /></template>
            全部已读
          </t-button>
          <t-button size="small" variant="text" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
          </t-button>
        </t-space>
      </div>

      <div class="inbox-body">
        <!-- 左栏：列表 -->
        <div class="inbox-list">
          <t-loading :loading="loading" size="small">
            <div
              v-for="item in rows"
              :key="item.receiver_id"
              class="notice-item"
              :class="{ 'notice-item--active': active?.receiver_id === item.receiver_id, 'notice-item--unread': !item.read_flag }"
              @click="openDetail(item)"
            >
              <div class="notice-item__head">
                <span class="notice-item__dot" />
                <span class="notice-item__title">{{ item.title }}</span>
                <t-tag :theme="levelTheme(item.level)" size="small" variant="light">{{ levelLabel(item.level) }}</t-tag>
              </div>
              <p class="notice-item__content">{{ plainText(item.content) }}</p>
              <div class="notice-item__foot">
                <span>{{ typeLabel(item.notice_type) }}</span>
                <span>{{ formatDateTime(item.created_at) }}</span>
              </div>
            </div>

            <div v-if="!loading && rows.length === 0" class="empty-state">
              <t-icon class="empty-state__icon" name="notification" />
              <p class="empty-state__title">{{ emptyText }}</p>
              <p class="empty-state__desc">新的系统消息、待办与公告会出现在这里</p>
            </div>
          </t-loading>

          <t-pagination
            v-if="pagination.total > pagination.pageSize"
            class="inbox-list__pager"
            :current="pagination.current"
            :page-size="pagination.pageSize"
            :total="pagination.total"
            size="small"
            @change="onPageChange"
          />
        </div>

        <!-- 右栏：详情 -->
        <div class="inbox-detail">
          <template v-if="active">
            <div class="detail-head">
              <div>
                <h3 class="detail-head__title">{{ active.title }}</h3>
                <div class="detail-head__meta">
                  <t-tag :theme="levelTheme(active.level)" size="small" variant="light">{{ levelLabel(active.level) }}</t-tag>
                  <span>{{ typeLabel(active.notice_type) }}</span>
                  <span>{{ formatDateTime(active.created_at) }}</span>
                  <span v-if="active.read_flag" class="detail-head__read">已读</span>
                </div>
              </div>
              <t-space>
                <t-button v-if="!active.read_flag" size="small" theme="primary" @click="handleRead(active)">标记已读</t-button>
                <t-button size="small" theme="danger" variant="outline" @click="confirmRemove(active)">删除</t-button>
              </t-space>
            </div>
            <div class="detail-body" v-html="renderContent(active)" />
            <div v-if="active.link_url" class="detail-link">
              <t-link theme="primary" @click="openLink(active.link_url)">查看详情 →</t-link>
            </div>
          </template>
          <div v-else class="empty-state empty-state--detail">
            <t-icon class="empty-state__icon" name="mail" />
            <p class="empty-state__title">选择左侧消息查看详情</p>
          </div>
        </div>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  deleteMyNotice,
  fetchUnreadCount,
  markAllNoticeRead,
  markNoticeRead,
  pageMyNotices,
  type MyNoticeRes,
} from '@/api/business/notice';
import { formatDateTime } from '@/utils/format';
import { useNoticeStore } from '@/stores/notice';

/** 阅读状态筛选 */
type ReadFilter = 'unread' | 'read' | 'all';

const noticeStore = useNoticeStore();

const rows = ref<MyNoticeRes[]>([]);
const loading = ref(false);
const readFilter = ref<ReadFilter>('unread');
const active = ref<MyNoticeRes | null>(null);
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });
const unreadCount = computed(() => noticeStore.unreadCount);

const emptyText = computed(() => {
  if (readFilter.value === 'unread') {
    return '没有未读消息';
  }
  if (readFilter.value === 'read') {
    return '没有已读消息';
  }
  return '还没有消息';
});

/** 类型中文名 */
function typeLabel(type: string): string {
  const map: Record<string, string> = {
    SYSTEM: '系统消息',
    TODO: '待办',
    ALERT: '告警',
    WORKFLOW: '审批',
    CIRCULAR: '公告',
  };
  return map[(type || '').toUpperCase()] || type;
}

/** 级别中文名 */
function levelLabel(level: string): string {
  const map: Record<string, string> = { INFO: '通知', SUCCESS: '成功', WARN: '警告', ERROR: '错误' };
  return map[(level || '').toUpperCase()] || level;
}

/** 级别 → 主题色（状态色语义固定） */
function levelTheme(level: string): 'primary' | 'success' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'SUCCESS') {
    return 'success';
  }
  if (key === 'WARN') {
    return 'warning';
  }
  if (key === 'ERROR') {
    return 'danger';
  }
  return 'primary';
}

/** 列表里只显示纯文本摘要，避免 HTML 标签裸露 */
function plainText(content: string): string {
  return (content || '').replace(/<[^>]+>/g, '').slice(0, 80);
}

/** 详情正文：TEXT 直接转义后按段落渲染，HTML 原样（后端渲染时已转义变量），MARKDOWN 折行为 <br> */
function renderContent(item: MyNoticeRes): string {
  const raw = item.content || '';
  const format = (item.content_format || 'TEXT').toUpperCase();
  if (format === 'HTML') {
    return raw;
  }
  const escaped = raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
  return escaped.replace(/\n/g, '<br />');
}

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageMyNotices({
      page: pagination.current,
      page_size: pagination.pageSize,
      read_flag: readFilter.value === 'all' ? undefined : readFilter.value === 'read',
    });
    rows.value = data.list;
    pagination.total = data.total;
    // 选中项已不在当前页时清空，避免右侧残留
    if (active.value && !data.list.some((item) => item.receiver_id === active.value?.receiver_id)) {
      active.value = data.list[0] ?? null;
    }
    if (!active.value && data.list.length > 0) {
      active.value = data.list[0];
    }
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

async function refreshUnread(): Promise<void> {
  try {
    const res = await fetchUnreadCount();
    noticeStore.setUnreadCount(res.unread_count);
  } catch {
    // 未读数刷新失败不影响列表使用
  }
}

function handleFilterChange(): void {
  pagination.current = 1;
  active.value = null;
  void loadList();
  void refreshUnread();
}

function onPageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

function openDetail(item: MyNoticeRes): void {
  active.value = item;
  if (!item.read_flag) {
    void handleRead(item);
  }
}

async function handleRead(item: MyNoticeRes): Promise<void> {
  try {
    await markNoticeRead(item.receiver_id);
    item.read_flag = true;
    item.read_at = new Date().toISOString();
    noticeStore.setUnreadCount(Math.max(0, noticeStore.unreadCount - 1));
    // 未读筛选下读掉一条就该从列表消失，避免与角标不一致
    if (readFilter.value === 'unread') {
      await loadList();
    }
  } catch (error) {
    showError(error);
  }
}

async function handleReadAll(): Promise<void> {
  try {
    await markAllNoticeRead();
    noticeStore.setUnreadCount(0);
    void MessagePlugin.success('已全部标记为已读');
    await loadList();
  } catch (error) {
    showError(error);
  }
}

async function confirmRemove(item: MyNoticeRes): Promise<void> {
  try {
    await deleteMyNotice(item.receiver_id);
    void MessagePlugin.success('已删除');
    active.value = null;
    await loadList();
    await refreshUnread();
  } catch (error) {
    showError(error);
  }
}

function openLink(url: string): void {
  window.open(url, '_blank', 'noopener');
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

// SSE 推送到达时（noticeStore 会 bump unreadCount）自动刷新列表，保证「角标变了列表也变了」
watch(
  () => noticeStore.refreshToken,
  () => {
    void loadList();
  },
);

onMounted(() => {
  noticeStore.connect();
  void loadList();
  void refreshUnread();
});

onBeforeUnmount(() => {
  // 页面离开不主动断开 SSE（顶栏铃铛还要用），由布局统一管理生命周期
});
</script>

<style lang="less" scoped>
.notice-message-page {
  padding: 16px;

  .inbox-card {
    :deep(.t-card__body) {
      padding: 0;
    }
  }

  .inbox-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 20px;
    border-bottom: 1px solid var(--td-component-stroke);

    &__title {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 15px;
      font-weight: 600;
      color: var(--td-text-color-primary);
    }

    &__icon {
      color: var(--td-brand-color-7);
    }
  }

  .inbox-body {
    display: flex;
    height: calc(100vh - 190px);
  }

  .inbox-list {
    width: 400px;
    min-width: 400px;
    overflow-y: auto;
    border-right: 1px solid var(--td-component-stroke);

    &__pager {
      padding: 8px 12px;
    }
  }

  .notice-item {
    padding: 12px 20px;
    border-bottom: 1px solid var(--td-component-stroke);
    cursor: pointer;
    transition: background 0.15s;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }

    &--active {
      background: var(--td-bg-color-container-select);
    }

    &__head {
      display: flex;
      align-items: center;
      gap: 8px;
    }

    &__dot {
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: transparent;
      flex: none;
    }

    &--unread &__dot {
      background: var(--td-error-color);
    }

    &__title {
      flex: 1;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 14px;
      color: var(--td-text-color-primary);
    }

    &--unread &__title {
      font-weight: 600;
    }

    &__content {
      margin: 6px 0;
      font-size: 13px;
      line-height: 1.5;
      color: var(--td-text-color-secondary);
      display: -webkit-box;
      -webkit-line-clamp: 2;
      -webkit-box-orient: vertical;
      overflow: hidden;
    }

    &__foot {
      display: flex;
      justify-content: space-between;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
    }
  }

  .inbox-detail {
    flex: 1;
    min-width: 0;
    padding: 20px 24px;
    overflow-y: auto;
  }

  .detail-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--td-component-stroke);

    &__title {
      margin: 0 0 8px;
      font-size: 17px;
      color: var(--td-text-color-primary);
    }

    &__meta {
      display: flex;
      align-items: center;
      gap: 12px;
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }

    &__read {
      color: var(--td-success-color);
    }
  }

  .detail-body {
    padding: 16px 0;
    font-size: 14px;
    line-height: 1.8;
    color: var(--td-text-color-primary);
    word-break: break-word;
  }

  .detail-link {
    padding-top: 8px;
  }

  .empty-state {
    padding: 60px 0;
    text-align: center;

    &--detail {
      padding: 120px 0;
    }

    &__icon {
      font-size: 48px;
      color: var(--td-text-color-placeholder);
    }

    &__title {
      margin: 12px 0 4px;
      font-size: 15px;
      color: var(--td-text-color-primary);
    }

    &__desc {
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }
}
</style>
