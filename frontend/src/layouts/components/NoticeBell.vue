<template>
  <t-popup
    v-if="canReadNotice"
    v-model:visible="visible"
    trigger="click"
    placement="bottom-right"
    :destroy-on-close="false"
    overlay-class-name="notice-bell__popup"
  >
    <t-badge :count="badgeCount" :max-count="99" :offset="[2, 2]" size="small">
      <t-button class="notice-bell__trigger" variant="text" shape="square">
        <t-icon :name="noticeStore.unreadCount > 0 ? 'notification-filled' : 'notification'" />
      </t-button>
    </t-badge>

    <template #content>
      <div class="notice-bell__panel">
        <div class="notice-bell__head">
          <span class="notice-bell__title">消息</span>
          <t-space size="small">
            <t-link size="small" theme="primary" :disabled="noticeStore.unreadCount === 0" @click="handleReadAll">
              全部已读
            </t-link>
            <t-link size="small" theme="primary" @click="goMessageCenter">查看全部</t-link>
          </t-space>
        </div>

        <t-loading :loading="loading" size="small">
          <div v-if="items.length === 0 && !loading" class="notice-bell__empty">
            <t-icon name="notification" class="notice-bell__empty-icon" />
            <p>还没有未读消息</p>
          </div>
          <div
            v-for="item in items"
            :key="item.receiver_id"
            class="notice-bell__item"
            @click="openItem(item)"
          >
            <span class="notice-bell__dot" :class="`notice-bell__dot--${levelKey(item.level)}`" />
            <div class="notice-bell__content">
              <div class="notice-bell__item-title">{{ item.title }}</div>
              <div class="notice-bell__item-time">{{ formatDateTime(item.created_at) }}</div>
            </div>
          </div>
        </t-loading>
      </div>
    </template>
  </t-popup>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import { pageMyNotices, markAllNoticeRead, type MyNoticeRes } from '@/api/business/notice';
import { useNoticeStore } from '@/stores/notice';
import { useUserStore } from '@/stores/user';
import { PERM } from '@/constants/permissions';
import { formatDateTime } from '@/utils/format';

const router = useRouter();
const noticeStore = useNoticeStore();
const userStore = useUserStore();

/**
 * 是否有站内信权限（`business:notice:mine`）。
 *
 * 无权限时**整只铃铛不渲染**：角标恒 0、面板里一次 `pageMyNotices` 也不会发出
 * （那个接口同样 403）。渲染一个点开必 403 的铃铛只是把红色报错从首屏挪到点击后。
 */
const canReadNotice = computed(() => userStore.hasPermission(PERM.noticeMine));

const visible = ref(false);
const loading = ref(false);
const items = ref<MyNoticeRes[]>([]);

/** 角标数字：未读数封顶由 t-badge 处理，这里直接给原始值 */
const badgeCount = computed(() => noticeStore.unreadCount);

function levelKey(level: string): string {
  return (level || 'INFO').toLowerCase();
}

async function loadLatest(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageMyNotices({ page: 1, page_size: 5, read_flag: false });
    items.value = data.list;
  } catch {
    // 未读数权限之外的情况（如 403）不弹错，保持顶栏安静
    items.value = [];
  } finally {
    loading.value = false;
  }
}

async function handleReadAll(): Promise<void> {
  try {
    await markAllNoticeRead();
    noticeStore.setUnreadCount(0);
    items.value = [];
    void MessagePlugin.success('已全部标记为已读');
  } catch {
    void MessagePlugin.error('操作失败，请稍后重试');
  }
}

function openItem(item: MyNoticeRes): void {
  visible.value = false;
  void router.push({ path: '/business/notice/message', query: { focus: item.receiver_id } });
}

function goMessageCenter(): void {
  visible.value = false;
  void router.push('/business/notice/message');
}

/** 面板打开时刷新一次，避免显示过期列表 */
function onVisibleChange(value: boolean): void {
  if (value) {
    void loadLatest();
  }
}

// 打开面板时刷新
watch(visible, onVisibleChange);

// SSE 推送到达后，若面板正开着就同步刷新
watch(
  () => noticeStore.refreshToken,
  () => {
    if (visible.value) {
      void loadLatest();
    }
  },
);
</script>

<style lang="less" scoped>
.notice-bell {
  &__trigger {
    color: var(--td-text-color-primary);
  }

  &__panel {
    width: 320px;
    padding: 8px 0;
  }

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 12px 8px;
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__title {
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__empty {
    padding: 32px 0;
    text-align: center;
    font-size: 13px;
    color: var(--td-text-color-placeholder);

    &-icon {
      font-size: 32px;
      color: var(--td-text-color-placeholder);
    }
  }

  &__item {
    display: flex;
    gap: 8px;
    padding: 10px 12px;
    cursor: pointer;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }
  }

  &__dot {
    width: 8px;
    height: 8px;
    margin-top: 6px;
    border-radius: 50%;
    flex: none;
    background: var(--td-brand-color-7);

    &--warn {
      background: var(--td-warning-color);
    }

    &--error {
      background: var(--td-error-color);
    }

    &--success {
      background: var(--td-success-color);
    }
  }

  &__content {
    min-width: 0;
  }

  &__item-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__item-time {
    margin-top: 2px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}
</style>
