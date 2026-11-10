<template>
  <!--
    空闲自动退出（AUTO-LOGOUT）的温和提示。

    只在「距自动退出还剩 N 秒」时出现（后端下发窗口 20–60 秒），两个出口：
    继续使用（重置计时）/ 立即退出（马上吊销令牌回登录页）。
    `t-dialog` 用 `:close-on-overlay-click="false"` + 隐藏右上角关闭，
    避免用户把唯一的提示关掉后不知道马上就要被踢。
  -->
  <t-dialog
    v-model:visible="warningVisible"
    header="即将自动退出"
    width="420px"
    placement="center"
    :close-btn="false"
    :close-on-overlay-click="false"
    :close-on-esc-keydown="false"
    :footer="false"
    destroy-on-close
  >
    <div class="idle-warning">
      <t-icon name="time" size="20px" class="idle-warning__icon" />
      <div class="idle-warning__text">
        <p class="idle-warning__mine">您已 {{ displayIdleMinutes }} 分钟无操作</p>
        <p class="idle-warning__count">
          将在 <b>{{ remainingSeconds }}</b> 秒后自动退出登录
        </p>
      </div>
    </div>
    <div class="idle-warning__actions">
      <t-button theme="primary" size="medium" @click="keepAlive">继续使用</t-button>
      <t-button theme="default" size="medium" variant="outline" @click="logoutNow">立即退出</t-button>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 空闲自动退出守卫（全站唯一实例，挂在 `App.vue`）。
 *
 * 放在根组件而不是 `BasicLayout`：首登强制改密页（`/change-password`）不在工作台布局
 * 里，挂在布局上会让「挂在改密页不动」绕过自动退出。
 *
 * 计时器与跨标签广播的逻辑都在 {@link useIdleLogout}，本组件只负责渲染提示。
 */
import { computed, onMounted } from 'vue';
import { useIdleLogout } from '@/composables/useIdleLogout';

const { warningVisible, idleMinutes, remainingSeconds, keepAlive, logoutNow, start } = useIdleLogout();

/** 文案里的「已 X 分钟无操作」：至少显示 1，避免出现「您已 0 分钟无操作」 */
const displayIdleMinutes = computed<number>(() => Math.max(1, idleMinutes.value));

onMounted(() => {
  start();
});
</script>

<style scoped lang="less">
.idle-warning {
  display: flex;
  gap: 12px;
  align-items: flex-start;

  &__icon {
    color: var(--td-warning-color, #ed7b2f);
    margin-top: 2px;
  }

  &__text {
    flex: 1;
  }

  &__mine {
    margin: 0 0 6px;
    font-size: 14px;
    color: var(--td-text-color-primary, #000000e6);
  }

  &__count {
    margin: 0;
    font-size: 13px;
    color: var(--td-text-color-secondary, #00000099);

    b {
      color: var(--td-warning-color, #ed7b2f);
      font-size: 15px;
    }
  }

  &__actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 20px;
  }
}
</style>
