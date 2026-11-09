<template>
  <teleport to="body">
    <transition name="notice-toast">
      <section
        v-if="canReadNotice && visible && items.length > 0"
        class="notice-toast"
        data-testid="notice-toast"
        role="complementary"
        aria-label="新消息提醒"
        @mouseenter="onMouseEnter"
        @mouseleave="onMouseLeave"
      >
        <header class="notice-toast__head">
          <span class="notice-toast__title" data-testid="notice-toast-title">
            <t-icon name="mail" class="notice-toast__title-icon" />
            新消息 {{ count }} 条
          </span>
          <t-button
            class="notice-toast__close"
            variant="text"
            shape="square"
            size="small"
            data-testid="notice-toast-close"
            aria-label="关闭新消息提醒"
            @click="close"
          >
            <t-icon name="close" />
          </t-button>
        </header>

        <ul class="notice-toast__list">
          <li
            v-for="item in items"
            :key="item.receiver_id"
            class="notice-toast__item"
            data-testid="notice-toast-item"
            @click="openItem(item)"
          >
            <t-icon
              :name="noticeTypeMeta(item.notice_type).icon"
              class="notice-toast__icon"
              :class="`notice-toast__icon--${levelKey(item.level)}`"
            />
            <div class="notice-toast__body">
              <p class="notice-toast__item-title">{{ item.title }}</p>
              <p class="notice-toast__item-meta">
                <span class="notice-toast__type">{{ noticeTypeMeta(item.notice_type).label }}</span>
                <span class="notice-toast__time">{{ formatRelativeTime(item.created_at) }}</span>
              </p>
            </div>
          </li>
        </ul>

        <footer class="notice-toast__foot">
          <button class="notice-toast__all" type="button" data-testid="notice-toast-all" @click="openAll">
            查看全部 →
          </button>
        </footer>
      </section>
    </transition>
  </teleport>
</template>

<script setup lang="ts">
/**
 * 右上角「新消息」自动浮层。
 *
 * # 为什么不复用 `NoticeBell`
 *
 * 铃铛是**用户主动点开**的面板（`t-popup`，锚在按钮上）；本组件是**被动弹出**的浮层
 * （`Teleport` 到 body、固定定位、8 秒自收）。两者数据源相同（`noticeStore`），但交互生命周期
 * 完全不同 —— 铃铛一行没改，只是多了一个「自动展开」的兄弟组件。
 *
 * # 状态机
 *
 * ```
 * 空闲 --(popupSeq 变化)--> 显示中 --(8s 到期 / [×] / 点条目 / 查看全部)--> 空闲
 *                              |  ↑
 *                       (悬停) ↓  |(离开，接着剩余时间跑)
 *                            暂停中
 * ```
 *
 * - 显示中再来新消息：**不叠第二个浮层**，只把条数 +1、列表补一行，并把倒计时重新计满 8 秒；
 * - 关闭时调用 `resetPopupBatch()` 清空本批（「已见过」集合不清，同一条不会再弹）。
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { useNoticeStore } from '@/stores/notice';
import { useUserStore } from '@/stores/user';
import { PERM } from '@/constants/permissions';
import {
  AutoCloseTimer,
  formatRelativeTime,
  levelKey,
  noticeTypeMeta,
  type NoticePopupItem,
} from '@/utils/notice-popup';

const router = useRouter();
const noticeStore = useNoticeStore();
const userStore = useUserStore();

/**
 * 是否有站内信权限（`business:notice:mine`）。
 *
 * 浮层本身不请求接口（数据全部来自 store），但换账号 / 退出重登时 store 里可能还留着
 * 上一个账号的消息 —— 加这道闸，无权限账号连浮层都不渲染。
 */
const canReadNotice = computed(() => userStore.hasPermission(PERM.noticeMine));

/** 浮层是否显示 */
const visible = ref(false);

/** 列表（新的在前，最多 5 条，由 store 维护） */
const items = computed(() => noticeStore.latestItems);

/** 标题条数：本批到达的条数（连续 3 条 → 「新消息 3 条」） */
const count = computed(() => noticeStore.newBatchCount);

/** 8 秒自动收起；悬停暂停、离开续跑 */
const timer = new AutoCloseTimer(() => {
  visible.value = false;
  noticeStore.resetPopupBatch();
});

/** 收起浮层（[×] / 点条目 / 查看全部 / 计时到期共用） */
function close(): void {
  timer.cancel();
  visible.value = false;
  noticeStore.resetPopupBatch();
}

/** 有新消息入队：没显示就弹，已显示就把倒计时重新计满（合并而非叠加） */
watch(
  () => noticeStore.popupSeq,
  () => {
    visible.value = true;
    timer.start();
  },
);

function onMouseEnter(): void {
  timer.pause();
}

function onMouseLeave(): void {
  timer.resume();
}

/** 点条目：跳消息中心并带上 focus（与铃铛同一落点），然后收起 */
function openItem(item: NoticePopupItem): void {
  close();
  void router.push({ path: '/business/notice/message', query: { focus: item.receiver_id } });
}

/** 查看全部：跳消息中心列表 */
function openAll(): void {
  close();
  void router.push('/business/notice/message');
}

onBeforeUnmount(() => {
  timer.cancel();
});
</script>

<style lang="less" scoped>
.notice-toast {
  position: fixed;
  top: 64px;
  right: 16px;
  z-index: 2000;
  width: 300px;
  max-height: 420px;
  overflow: hidden;
  border: 1px solid var(--td-component-stroke);
  border-radius: 10px;
  background: var(--td-bg-color-container);
  box-shadow: 0 6px 24px rgb(0 0 0 / 12%);
  // 浮层是「弹一下就走」的：不要抢走页面焦点/滚动，也不拦下面的内容
  pointer-events: auto;

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px 8px;
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__title-icon {
    color: var(--td-brand-color);
  }

  &__close {
    color: var(--td-text-color-placeholder);

    &:hover {
      color: var(--td-text-color-primary);
    }
  }

  &__list {
    margin: 0;
    padding: 4px 0;
    list-style: none;
    max-height: 320px;
    overflow-y: auto;
  }

  &__item {
    display: flex;
    gap: 8px;
    padding: 9px 12px;
    cursor: pointer;
    transition: background-color 0.2s;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }
  }

  &__icon {
    flex: none;
    margin-top: 2px;
    font-size: 16px;
    color: var(--td-brand-color);

    &--success {
      color: var(--td-success-color);
    }

    &--warn {
      color: var(--td-warning-color);
    }

    &--error {
      color: var(--td-error-color);
    }
  }

  &__body {
    min-width: 0;
  }

  &__item-title {
    margin: 0;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    font-size: 13px;
    line-height: 18px;
    color: var(--td-text-color-primary);
  }

  &__item-meta {
    display: flex;
    gap: 8px;
    margin: 2px 0 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__type {
    flex: none;
  }

  &__foot {
    padding: 6px 12px 10px;
    border-top: 1px solid var(--td-component-stroke);
    text-align: center;
  }

  &__all {
    width: 100%;
    padding: 4px 0;
    font-size: 13px;
    color: var(--td-brand-color);
    background: transparent;
    border: none;
    border-radius: 6px;
    cursor: pointer;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }
  }
}

// 从右上角滑入：位移 + 淡入，200ms（与验收要求的「滑入」一致）
.notice-toast-enter-active,
.notice-toast-leave-active {
  transition:
    transform 0.2s ease,
    opacity 0.2s ease;
}

.notice-toast-enter-from,
.notice-toast-leave-to {
  opacity: 0;
  transform: translateX(24px);
}
</style>
