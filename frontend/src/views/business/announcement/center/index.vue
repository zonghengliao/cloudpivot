<template>
  <div class="ann-center-page">
    <!-- 工具条：搜索 / 只看未读 / 全部已读 -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <div class="toolbar__title">
          <t-icon name="notification" class="toolbar__icon" />
          <span>公告中心</span>
          <t-tag v-if="unreadCount > 0" theme="danger" size="small" variant="light">{{ unreadCount }} 条未读</t-tag>
        </div>
        <t-space size="small">
          <t-input
            v-model="query.keyword"
            class="toolbar__search"
            placeholder="搜索标题 / 摘要"
            clearable
            @enter="handleSearch"
          >
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>
          <t-switch v-model="onlyUnread" size="small" @change="handleSearch">
            <template #label>只看未读</template>
          </t-switch>
          <t-button size="small" variant="outline" :disabled="unreadCount === 0" @click="handleReadAll">
            <template #icon><t-icon name="check" /></template>
            全部已读
          </t-button>
          <t-button size="small" variant="text" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
          </t-button>
        </t-space>
      </div>
    </t-card>

    <div class="ann-body">
      <!-- 左：公告列表 -->
      <t-card :bordered="false" class="ann-list-card">
        <t-loading :loading="loading" size="small">
          <div class="ann-list">
            <div
              v-for="item in rows"
              :key="item.id"
              class="ann-item"
              :class="{ 'ann-item--active': activeId === item.id, 'ann-item--unread': !item.read }"
              @click="openDetail(item)"
            >
              <div class="ann-item__head">
                <t-icon v-if="item.is_top" class="ann-item__top" name="pin" />
                <span class="ann-item__dot" />
                <span class="ann-item__title">{{ item.title }}</span>
                <t-tag :theme="levelTheme(item.level)" size="small" variant="light">{{ levelLabel(item.level) }}</t-tag>
              </div>
              <p class="ann-item__summary">{{ item.summary || '（无摘要）' }}</p>
              <div class="ann-item__foot">
                <span>{{ item.ann_no }}</span>
                <span>{{ item.publisher_name || '系统' }}</span>
                <span>{{ formatDateTime(item.publish_at) }}</span>
                <span v-if="item.attachment_count > 0" class="ann-item__att">
                  <t-icon name="attach" /> {{ item.attachment_count }}
                </span>
              </div>
            </div>

            <div v-if="!loading && rows.length === 0" class="empty-state">
              <t-icon class="empty-state__icon" name="notification" />
              <p class="empty-state__title">{{ onlyUnread ? '没有未读公告' : '暂无公告' }}</p>
              <p class="empty-state__desc">发布范围包含你的公告会出现在这里</p>
            </div>
          </div>
        </t-loading>

        <t-pagination
          v-if="pagination.total > pagination.pageSize"
          class="ann-list__pager"
          :current="pagination.current"
          :page-size="pagination.pageSize"
          :total="pagination.total"
          size="small"
          @change="onPageChange"
        />
      </t-card>

      <!-- 右：详情（正文走 MarkdownViewer：后端 content_html + 前端二次净化） -->
      <t-card :bordered="false" class="ann-detail-card">
        <template v-if="detail">
          <div class="detail-head">
            <div class="detail-head__main">
              <h2 class="detail-head__title">
                <t-icon v-if="detail.is_top" class="detail-head__pin" name="pin" />
                {{ detail.title }}
              </h2>
              <div class="detail-head__meta">
                <t-tag :theme="levelTheme(detail.level)" size="small" variant="light">
                  {{ levelLabel(detail.level) }}
                </t-tag>
                <span>{{ detail.ann_no }}</span>
                <span>发布人：{{ detail.publisher_name || '系统' }}</span>
                <span>{{ formatDateTime(detail.publish_at) }}</span>
                <span v-if="detail.expire_at">有效期至 {{ formatDateTime(detail.expire_at) }}</span>
                <t-tag v-if="detail.read" size="small" variant="light" theme="success">已读</t-tag>
              </div>
            </div>
            <t-space size="small">
              <t-button v-if="!detail.read" size="small" theme="primary" @click="handleRead(detail.id)">标记已读</t-button>
              <t-button size="small" variant="text" @click="loadDetail(detail.id)">
                <template #icon><t-icon name="refresh" /></template>
              </t-button>
            </t-space>
          </div>

          <MarkdownViewer :html="detail.content_html" :show-toc="false" />

          <div v-if="detail.attachments.length > 0" class="detail-attachments">
            <div class="detail-attachments__title">
              <t-icon name="attach" />
              附件（{{ detail.attachments.length }}）
            </div>
            <div v-for="file in detail.attachments" :key="file.id" class="detail-attachments__item">
              <t-icon class="detail-attachments__icon" name="file" />
              <span class="detail-attachments__name" :title="file.file_name">{{ file.file_name }}</span>
              <span class="detail-attachments__size">{{ formatFileSize(file.file_size) }}</span>
              <t-link theme="primary" @click="downloadAttachment(file)">下载</t-link>
            </div>
          </div>
        </template>

        <div v-else class="empty-state empty-state--detail">
          <t-icon class="empty-state__icon" name="mail" />
          <p class="empty-state__title">选择左侧公告查看详情</p>
        </div>
      </t-card>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 公告中心（用户侧，`business:announcement:read`，所有角色默认授予）。
 *
 * # 五件事
 *
 * 列表（分页 + 关键词 + 只看未读）/ 详情（后端 `content_html` → `MarkdownViewer` 二次净化）
 * / 已读标记（点开即读，幂等）/ 全部已读 / 附件下载（代理接口取 blob，不能 `<a href>`）。
 *
 * # 一个细节
 *
 * 「只看未读」筛选下点开一条就把它读掉了，列表必须重拉 —— 否则会出现
 * 「角标减了但列表里还挂着这条」的错位（A5 消息中心踩过的同一个坑）。
 */
import { onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import { ApiError } from '@/api/request';
import {
  downloadAnnouncementAttachment,
  fetchAnnouncementUnreadCount,
  getAnnouncementDetail,
  markAllAnnouncementsRead,
  markAnnouncementRead,
  pageVisibleAnnouncements,
  type AnnouncementAttachment,
  type AnnouncementCard,
  type AnnouncementDetail,
} from '@/api/business/announcement-api';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { saveBlob } from '@/utils/download';
import { useBadgeStore } from '@/stores/badge';

const rows = ref<AnnouncementCard[]>([]);
const loading = ref(false);
const badgeStore = useBadgeStore();
const detailLoading = ref(false);
const onlyUnread = ref(false);
const unreadCount = ref(0);
const activeId = ref('');
const detail = ref<AnnouncementDetail | null>(null);
const query = reactive({ keyword: '' });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

/** 级别 → 中文 */
function levelLabel(level: string): string {
  const map: Record<string, string> = { NORMAL: '普通', IMPORTANT: '重要', URGENT: '紧急' };
  return map[(level || '').toUpperCase()] || level || '普通';
}

/** 级别 → 语义色（状态色固定语义，不跟配色方案走） */
function levelTheme(level: string): 'primary' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'URGENT') {
    return 'danger';
  }
  if (key === 'IMPORTANT') {
    return 'warning';
  }
  return 'primary';
}

/** 列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageVisibleAnnouncements({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword || undefined,
      only_unread: onlyUnread.value || undefined,
    });
    rows.value = data.list;
    pagination.total = data.total;
    if (rows.value.length > 0 && !rows.value.some((item) => item.id === activeId.value)) {
      void openDetail(rows.value[0]);
    }
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

/** 未读数 */
async function refreshUnread(): Promise<void> {
  try {
    const res = await fetchAnnouncementUnreadCount();
    unreadCount.value = res.announcement_unread;
  } catch {
    // 未读数失败不影响阅读
  }
}

/** 详情（点开即读） */
async function openDetail(item: AnnouncementCard): Promise<void> {
  activeId.value = item.id;
  await loadDetail(item.id);
  if (!item.read) {
    await handleRead(item.id, false);
  }
}

/** 拉详情 */
async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  try {
    detail.value = await getAnnouncementDetail(id);
    activeId.value = id;
  } catch (error) {
    detail.value = null;
    showError(error);
  } finally {
    detailLoading.value = false;
  }
}

/** 标记已读（幂等；`reloadList` 只在该条从未读筛选里消失时才需要） */
async function handleRead(id: string, reloadList = true): Promise<void> {
  try {
    const res = await markAnnouncementRead(id);
    void badgeStore.refresh();
    if (detail.value && detail.value.id === id) {
      detail.value.read = true;
      detail.value.read_at = new Date().toISOString();
    }
    const target = rows.value.find((item) => item.id === id);
    if (target) {
      target.read = true;
    }
    if (res.first_read) {
      unreadCount.value = Math.max(0, unreadCount.value - 1);
    }
    if (reloadList && onlyUnread.value) {
      await loadList();
    }
  } catch (error) {
    showError(error);
  }
}

/** 全部已读 */
async function handleReadAll(): Promise<void> {
  try {
    const res = await markAllAnnouncementsRead();
    unreadCount.value = 0;
    rows.value.forEach((item) => {
      item.read = true;
    });
    void MessagePlugin.success(`已全部标记为已读（${res.affected} 条）`);
    await loadList();
  } catch (error) {
    showError(error);
  }
}

/** 搜索 / 切筛选：回第一页 */
function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function onPageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

/** 下载附件（鉴权头 + blob，不能直接 `<a href>`） */
async function downloadAttachment(file: AnnouncementAttachment): Promise<void> {
  try {
    const blob = await downloadAnnouncementAttachment(file.download_url);
    saveBlob(blob, file.file_name || 'attachment');
  } catch (error) {
    showError(error);
  }
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadList();
  void refreshUnread();
});
</script>

<style scoped lang="less">
.ann-center-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: 100%;
}

.toolbar-card {
  flex: 0 0 auto;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__icon {
    color: var(--td-brand-color);
  }

  &__search {
    width: 220px;
  }
}

.ann-body {
  display: flex;
  gap: 12px;
  flex: 1;
  min-height: 0;
}

.ann-list-card {
  width: 380px;
  flex: 0 0 380px;
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.ann-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-right: 4px;

  &__pager {
    margin-top: 10px;
    display: flex;
    justify-content: center;
  }
}

.ann-item {
  position: relative;
  padding: 10px 12px 10px 16px;
  border-radius: 8px;
  cursor: pointer;
  transition: background-color 0.15s;

  &:hover {
    background: var(--td-bg-color-container-hover);
  }

  /* 未读行：品牌浅底高亮（红点 + 加粗之外的第三重提示，极轻，不抢正文） */
  &--unread {
    background: var(--td-brand-color-1);
  }

  &--active {
    background: var(--app-accent-soft);
  }

  &__head {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  /* 已读：留占位但不显色（无红点）；未读：8px 语义错误色实心点 */
  &__dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: transparent;
    flex: 0 0 auto;
  }

  &--unread &__dot {
    background: var(--td-error-color);
  }

  &__top {
    color: var(--td-warning-color);
    font-size: 14px;
  }

  &__title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 14px;
    font-weight: 400;
    color: var(--td-text-color-primary);
  }

  &--unread &__title {
    font-weight: 600;
  }

  &__summary {
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
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__att {
    display: inline-flex;
    align-items: center;
    gap: 2px;
  }
}

.ann-detail-card {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.detail-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  margin-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 8px;
    font-size: 18px;
    color: var(--td-text-color-primary);
  }

  &__pin {
    color: var(--td-warning-color);
  }

  &__meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.detail-attachments {
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px solid var(--td-component-stroke);

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__icon {
    color: var(--td-text-color-placeholder);
  }

  &__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__size {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.empty-state {
  padding: 60px 0;
  text-align: center;

  &--detail {
    padding: 140px 0;
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
</style>
