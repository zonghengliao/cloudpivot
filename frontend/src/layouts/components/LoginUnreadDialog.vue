<template>
  <t-dialog
    v-model:visible="visible"
    :header="header"
    :width="480"
    :footer="false"
    placement="center"
    data-testid="login-unread-dialog"
    class="login-unread"
    @close="close"
  >
    <p class="login-unread__hint">登录后先扫一眼，别让待办烂在列表里。</p>

    <ul class="login-unread__rows">
      <li
        v-for="row in rows"
        :key="row.key"
        class="login-unread__row"
        :class="{ 'login-unread__row--empty': row.count === 0 }"
        :data-testid="`login-unread-row-${row.key}`"
      >
        <t-icon :name="row.icon" class="login-unread__icon" />
        <span class="login-unread__label">{{ row.label }}</span>
        <span class="login-unread__count" :data-testid="`login-unread-count-${row.key}`">
          {{ row.count }} 条
        </span>
        <t-button
          class="login-unread__view"
          theme="primary"
          variant="text"
          size="small"
          :disabled="row.count === 0"
          :data-testid="`login-unread-view-${row.key}`"
          @click="openRow(row)"
        >
          查看
        </t-button>
      </li>
    </ul>

    <div class="login-unread__footer">
      <t-button theme="primary" data-testid="login-unread-ok" @click="close">知道了</t-button>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 登录后「你有 N 条待处理」汇总弹窗。
 *
 * # 触发
 *
 * `BasicLayout` 挂载时（= 登录成功进入后台，或刷新后台页面）调用 `open()`：
 * 先看 sessionStorage 的 `login-unread-shown`，没弹过才**对有权限的类别**并行拉未读数；
 * 合计为 0 就不弹（全空弹一个空框只会打扰用户），但照样打标记，避免每次刷新都打接口。
 * 无权限的类别既不请求也不出现在行里（后端会 403，见 `utils/login-unread.ts` 头注释）。
 *
 * # 数据来源（实测口径见 `utils/login-unread.ts` 头注释）
 *
 * 站内信 / 公告走现成的 `unread-count`；工作任务与审批待办没有独立 count 接口，
 * 用 `page_size=1` 取分页的 `total`。卡面写的 `/flow/task/page?status=PENDING` 实测 404，
 * 待办走 `/flow/task/todo`。
 *
 * # 只弹一次
 *
 * 标记在**弹窗打开时**就打上（不是关闭时）：用户刷新页面、甚至在关闭前刷新，都不会二次打扰。
 * 退出登录时由 `userStore.logout()` 清标记，下次登录重新弹。
 */
import { computed, ref } from 'vue';
import { useRouter } from 'vue-router';
import { pageTasks } from '@/api/business/task-api';
import { listTodoTasks } from '@/api/business/flow-api';
import { fetchUnreadCount } from '@/api/business/notice';
import { fetchAnnouncementUnreadCount } from '@/api/business/announcement-api';
import { useUserStore } from '@/stores/user';
import {
  collectUnread,
  isLoginUnreadShown,
  markLoginUnreadShown,
  permittedUnreadKeys,
  pickUnreadLoaders,
  summarizeUnread,
  totalUnread,
  type UnreadCounts,
  type UnreadLoaders,
  type UnreadRow,
} from '@/utils/login-unread';

const router = useRouter();
const userStore = useUserStore();

/** 弹窗可见性 */
const visible = ref(false);
/** 四类未读汇总行 */
const rows = ref<UnreadRow[]>([]);
/** 四类合计（标题用） */
const total = ref(0);
/** 标题文案 */
const header = computed(() => `你有 ${total.value} 条待处理`);

/** 4 类取数：单类失败按 0 计（见 collectUnread），不会让整个弹窗哑火 */
const ALL_LOADERS: UnreadLoaders = {
  notice: async () => (await fetchUnreadCount()).unread_count,
  announcement: async () => (await fetchAnnouncementUnreadCount()).announcement_unread,
  task: async () => {
    const data = await pageTasks({
      page: 1,
      page_size: 1,
      status: 'TODO',
      owner_id: userStore.userInfo?.id,
    });
    return data.total;
  },
  approval: async () => {
    const data = await listTodoTasks({ page: 1, page_size: 1 });
    return data.total;
  },
};

/** 收起（[×] / 知道了 / 查看某一行后都走这里） */
function close(): void {
  visible.value = false;
}

/** 点某一行的「查看」：收起并跳到该类别页面 */
function openRow(row: UnreadRow): void {
  if (row.count === 0) {
    return;
  }
  close();
  void router.push(row.path);
}

/**
 * 按需打开：本次登录没弹过 + 至少一类有权限 + 确实有未读才弹。
 *
 * 无权限的类别**一个请求都不发**（后端那几个接口会直接 403，控制台刷红）：
 * EMPLOYEE 这类账号没有 `business:notice:mine` / `business:announcement:read`，
 * 就只查工作任务与审批待办；四类都没权限时整框不弹。
 *
 * @returns 是否真的弹了（供 `BasicLayout` / 单测断言）
 */
async function open(): Promise<boolean> {
  if (isLoginUnreadShown()) {
    return false;
  }
  const allowedKeys = permittedUnreadKeys((code) => userStore.hasPermission(code));
  if (allowedKeys.length === 0) {
    // 一类都无权看：不发请求、不弹空框；也不打「已弹过」标记（本会话本来就没查过）
    return false;
  }
  let counts: UnreadCounts;
  try {
    counts = await collectUnread(pickUnreadLoaders(ALL_LOADERS, allowedKeys));
  } catch {
    // collectUnread 内部已 allSettled，这里只是最后一道保险
    return false;
  }
  // 无论有没有未读，本次登录都算「检查过」：刷新页面不再重复打这几个接口
  markLoginUnreadShown();
  const sum = totalUnread(counts);
  if (sum === 0) {
    return false;
  }
  rows.value = summarizeUnread(counts, allowedKeys);
  total.value = sum;
  visible.value = true;
  return true;
}

defineExpose({ open, close });
</script>

<style lang="less" scoped>
.login-unread {
  &__hint {
    margin: -4px 0 10px;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }

  &__rows {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  &__row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 4px;
    border-bottom: 1px solid var(--td-component-stroke);

    &:last-child {
      border-bottom: none;
    }

    &--empty {
      color: var(--td-text-color-placeholder);
    }
  }

  &__icon {
    flex: none;
    font-size: 18px;
    color: var(--td-brand-color);

    &--empty,
    .login-unread__row--empty & {
      color: var(--td-text-color-disabled);
    }
  }

  &__label {
    flex: 1;
    min-width: 0;
    font-size: 14px;
    color: var(--td-text-color-primary);

    .login-unread__row--empty & {
      color: var(--td-text-color-placeholder);
    }
  }

  &__count {
    flex: none;
    font-size: 14px;
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-primary);

    .login-unread__row--empty & {
      color: var(--td-text-color-placeholder);
    }
  }

  &__view {
    flex: none;
  }

  &__footer {
    display: flex;
    justify-content: center;
    padding-top: 14px;
  }
}
</style>
