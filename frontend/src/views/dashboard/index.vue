<template>
  <div class="dashboard">
    <!--
      首页按角色分两种形态（2026-11-09 用户拍板）：
        · 超管：两个 Tab ——「系统信息」（健康摘要 / 趋势图 / 系统概况）与「工作台」（每个人自己的活）；
        · 非超管：没有 Tab，直接就是工作台，看不到任何运维口径数字。
      工作台在两种形态下是**同一个组件**（`WorkbenchTab.vue`），不存在两份会走样的实现。
    -->
    <!--
      懒加载 + 常驻：`lazy` 让它**首次切到**才挂载（= 才发请求）；TDesign 1.19 的
      `destroyOnHide` 默认是 true（切走即销毁 → 切回重新请求），所以必须显式关掉，
      否则验收「Tab 切换不重复请求」永远不可能过。
    -->
    <t-tabs v-if="isSuperAdmin" v-model="activeTab" class="dashboard__tabs">
      <t-tab-panel value="system" lazy :destroy-on-hide="false">
        <template #label>
          <span class="dashboard__tab-label"><t-icon name="server" /> 系统信息</span>
        </template>
        <SystemInfoTab />
      </t-tab-panel>

      <t-tab-panel value="workbench" :destroy-on-hide="false">
        <template #label>
          <span class="dashboard__tab-label"><t-icon name="dashboard" /> 工作台</span>
        </template>
        <WorkbenchTab />
      </t-tab-panel>
    </t-tabs>

    <!-- 非超管：直接就是工作台（问候语 / 工作任务 / 公告与消息 / 快捷入口） -->
    <WorkbenchTab v-else />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { useUserStore } from '@/stores/user';
import WorkbenchTab from './components/WorkbenchTab.vue';
import SystemInfoTab from './components/SystemInfoTab.vue';

const userStore = useUserStore();

/** 是否超级管理员：只认后端下发的角色码（`SUPER_ADMIN`），与后端短路判定同口径 */
const isSuperAdmin = computed(() => userStore.isSuperAdmin);

/**
 * 当前 Tab。
 *
 * 默认「工作台」而不是「系统信息」：超管日常也是干活为主，系统信息是偶尔看一眼的运维面板。
 * 不做 localStorage 记忆（用户明确要求每次进来都停在工作台）。
 */
const activeTab = ref<'system' | 'workbench'>('workbench');
</script>

<style scoped lang="less">
.dashboard {
  // Tab 头部贴顶：t-tabs 默认在 nav 外层留白，首页要的是「一进来就是标签条」
  &__tabs {
    :deep(.t-tabs__nav) {
      margin: 0;
    }

    // 内容区与标签条之间 16px，与页面其它卡片间距一致
    :deep(.t-tabs__content) {
      padding-top: 16px;
    }
  }

  &__tab-label {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
}
</style>
