<template>
  <t-layout class="basic-layout">
    <t-aside class="basic-layout__aside" :width="asideWidth">
      <div class="basic-layout__brand" role="button" tabindex="0" @click="goHome" @keydown.enter="goHome">
        <span class="basic-layout__logo"><img src="/logo.png" alt="云枢" /></span>
        <span v-show="!collapsed" class="basic-layout__brand-name">云枢 CloudPivot</span>
      </div>

      <t-menu
        class="basic-layout__menu"
        :value="activeMenu"
        :collapsed="collapsed"
        :collapsed-width="64"
        :expand-mutex="true"
        :theme="setting.themeMode === 'dark' ? 'dark' : 'light'"
        @change="onMenuChange"
      >
        <side-menu-item
          v-for="item in userStore.navMenus"
          :key="item.id"
          :item="item"
          @navigate="goTo"
        />
      </t-menu>
    </t-aside>

    <t-layout class="basic-layout__main">
      <t-header class="basic-layout__header">
        <t-button
          class="basic-layout__collapse"
          variant="text"
          shape="square"
          @click="collapsed = !collapsed"
        >
          <t-icon :name="collapsed ? 'menu-unfold' : 'menu-fold'" />
        </t-button>
        <t-breadcrumb class="basic-layout__breadcrumb">
          <t-breadcrumb-item
            v-for="(crumb, index) in breadcrumbs"
            :key="`${crumb.title}-${index}`"
            :class="{ 'is-link': crumb.to }"
            @click="crumb.to ? goTo(crumb.to) : undefined"
          >
            {{ crumb.title }}
          </t-breadcrumb-item>
        </t-breadcrumb>

        <palette-switch />

        <!-- 顶栏消息铃铛：未读角标 + 最近 5 条未读（SSE 推送） -->
        <notice-bell />

        <t-tooltip :content="setting.themeMode === 'dark' ? '切换亮色主题' : '切换暗色主题'">
          <t-button
            class="basic-layout__theme-toggle"
            variant="text"
            shape="square"
            @click="setting.toggleTheme"
          >
            <t-icon :name="setting.themeMode === 'dark' ? 'sunny' : 'moon'" />
          </t-button>
        </t-tooltip>

        <t-dropdown :options="userActions" :popup-props="userPopupProps" @click="onUserAction">
          <!-- data-onboarding：首次登录引导（OnboardingHint）用这个属性定位头像，别删 -->
          <button class="basic-layout__user" type="button" data-onboarding="avatar">
            <t-avatar class="basic-layout__avatar" size="medium">{{ avatarText }}</t-avatar>
            <span class="basic-layout__username">{{ userStore.displayName || '未登录' }}</span>
            <t-icon class="basic-layout__caret" name="chevron-down" />
          </button>
        </t-dropdown>

        <!-- 首次登录引导：3 步提示 + 指向头像/铃铛的高亮箭头（localStorage 只弹一次） -->
        <onboarding-hint />
      </t-header>

      <t-content class="basic-layout__content">
        <router-view v-slot="{ Component }">
          <transition name="page-fade" mode="out-in">
            <component :is="Component" />
          </transition>
        </router-view>
      </t-content>
    </t-layout>

    <!-- 新消息自动浮层（右上角滑入 / 8 秒自收 / 悬停暂停）：全站只在这里挂一次 -->
    <notice-toast />

    <!-- 登录后「你有 N 条待处理」汇总弹窗：由下方 onMounted 触发，同一次登录只弹一次 -->
    <login-unread-dialog ref="loginUnreadRef" />

    <!-- FIX-1：`65018` 的「指派审批人」引导框（全站只挂一次，由 axios 拦截器唤起） -->
    <flow-assignee-dialog />

    <!-- FIX-1：首次启动向导（关键角色 0 人时登录弹一次，可跳过） -->
    <setup-wizard-dialog ref="setupWizardRef" />
  </t-layout>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, type DropdownOption } from 'tdesign-vue-next';
import SideMenuItem from '@/layouts/components/SideMenuItem.vue';
import PaletteSwitch from '@/layouts/components/PaletteSwitch.vue';
import NoticeBell from '@/layouts/components/NoticeBell.vue';
import NoticeToast from '@/layouts/components/NoticeToast.vue';
import LoginUnreadDialog from '@/layouts/components/LoginUnreadDialog.vue';
import FlowAssigneeDialog from '@/components/FlowAssigneeDialog.vue';
import SetupWizardDialog from '@/components/SetupWizardDialog.vue';
import { registerPermissionResolver } from '@/utils/flow-assignee-guard';
import OnboardingHint from '@/components/OnboardingHint.vue';
import { useUserStore } from '@/stores/user';
import { useSettingStore } from '@/stores/setting';
import { useNoticeStore } from '@/stores/notice';
import { useBadgeStore } from '@/stores/badge';
import { normalizeMenuPath, collectLeafPaths } from '@/utils/menu';

/** 用户下拉菜单项 */
const userActions: DropdownOption[] = [
  { content: '个人中心', value: 'profile' },
  // E10：外观设置入口藏在这里，用户普遍找不到 → 用 emoji + 「配色/主题」把能力写在脸上
  { content: '🎨 外观设置（配色/主题）', value: 'preference' },
  { content: '退出登录', value: 'logout' },
];

/** 用户下拉面板配置：追加类名便于放大面板与选项点击区域（弹层挂载到 body，样式见文末非 scoped 块） */
const userPopupProps = {
  overlayClassName: 'basic-layout__user-dropdown',
};

/** 面包屑条目：`to` 非空时该条可点击跳转 */
interface BreadcrumbItem {
  /** 显示文本 */
  title: string;
  /** 目标路由路径；为空表示纯文本（不可点击） */
  to?: string;
}

const route = useRoute();
const router = useRouter();
const userStore = useUserStore();
const setting = useSettingStore();

/** 应用启动时恢复上次保存的主题（明暗），避免刷新后回到默认亮色 */
setting.initTheme();

/**
 * 侧边栏折叠状态：绑到 setting store，而不是组件内 `ref`。
 *
 * 折叠状态也属于用户偏好（`ui.side_compact`），绑组件内 ref 的话「折叠一下」
 * 只在本页面生命周期内有效、刷新就丢，也不会同步到后端。
 * 写的是 `setting.setSideCompact` → 本地 localStorage 立即生效 + 异步 PUT。
 */
const collapsed = computed<boolean>({
  get: () => setting.sideCompact,
  set: (value: boolean) => setting.setSideCompact(value),
});

/** 侧边栏宽度：折叠后仅保留图标 */
const asideWidth = computed(() => (collapsed.value ? '64px' : '232px'));

/** 头像占位文字：取展示名首字符 */
const avatarText = computed(() => (userStore.displayName || 'U').slice(0, 1).toUpperCase());

/** 可跳转的叶子菜单路径，避免点击目录时跳到不存在的路由 */
const leafPaths = computed(() => collectLeafPaths(userStore.navMenus));

/** 当前高亮的菜单项 */
const activeMenu = computed(() => {
  const path = normalizeMenuPath(route.path);
  return leafPaths.value.has(path) ? path : '';
});

/** 面包屑：首页（可点击回首页）+ 当前匹配到的带标题路由（末级为纯文本） */
const breadcrumbs = computed<BreadcrumbItem[]>(() => {
  const titles = route.matched
    .map((record) => record.meta.title)
    .filter((title): title is string => typeof title === 'string');
  if (route.path === '/dashboard') {
    return titles.map((title) => ({ title }));
  }
  return [{ title: '首页', to: '/dashboard' }, ...titles.map((title) => ({ title }))];
});

/** 跳转首页 */
function goHome(): void {
  if (route.path !== '/dashboard') {
    void router.push('/dashboard');
  }
}

/** 面包屑条目跳转（仅可点击条目传入路径） */
function goTo(path: string): void {
  if (route.path !== path) {
    void router.push(path);
  }
}

/** 仅叶子菜单参与路由跳转 */
function onMenuChange(value: unknown): void {
  const path = typeof value === 'string' ? value : '';
  if (path && leafPaths.value.has(path)) {
    void router.push(path);
  }
}

function onUserAction(item: DropdownOption): void {
  if (item.value === 'profile') {
    void router.push('/user/profile');
    return;
  }
  if (item.value === 'preference') {
    void router.push('/user/preference');
    return;
  }
  if (item.value === 'logout') {
    openLogoutConfirm();
  }
}

/** 退出登录二次确认 */
function openLogoutConfirm(): void {
  const dialog = DialogPlugin.confirm({
    header: '退出登录',
    body: '确认退出当前账号？',
    confirmBtn: { content: '退出', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void doLogout();
    },
  });
}

async function doLogout(): Promise<void> {
  await userStore.logout();
  await router.push('/login');
}

/** SSE 订阅生命周期：布局挂载即连，卸载即断（全局仅一条连接） */
const noticeStore = useNoticeStore();

/**
 * 菜单徽标轮询（E16）：布局挂载即启，卸载即停。
 *
 * `startPolling` 内部有「已启动就返回」的闸，所以**路由切换不会重新请求**
 * （BasicLayout 不随路由重建）；页面隐藏时定时器会被停掉，回到前台补拉一次。
 */
const badgeStore = useBadgeStore();

/** 登录未读汇总弹窗：挂载后检查一次（「同一次登录只弹一次」由组件内部的 sessionStorage 标记保证） */
const loginUnreadRef = ref<InstanceType<typeof LoginUnreadDialog> | null>(null);

/** FIX-1：首次启动向导（关键角色 0 人时提示，可跳过） */
const setupWizardRef = ref<InstanceType<typeof SetupWizardDialog> | null>(null);

// FIX-1：把「当前登录人有哪些权限」注册给 axios 拦截器（`65018` 引导框据此决定
// 「让你自己指派」还是「请联系管理员」）。注册制避免 `api/request` ↔ `stores/user` 的循环依赖。
registerPermissionResolver((code: string) => userStore.hasPermission(code));

onMounted(() => {
  noticeStore.connect();
  badgeStore.startPolling();
  // 登录后必然经过这里：有未读才弹，全空不弹，弹过不再弹
  void loginUnreadRef.value?.open();
  // FIX-1：有 `system:setup:read` 才查（普通用户请求会 403，向导不打扰他们）；
  // 关键角色都有人时组件自己什么都不做。
  void setupWizardRef.value?.openIfNeeded(userStore.userInfo?.username ?? '');
});

onBeforeUnmount(() => {
  noticeStore.disconnect();
  badgeStore.stopPolling();
});
</script>

<style scoped lang="less">
.basic-layout {
  height: 100%;

  &__aside {
    display: flex;
    flex-direction: column;
    // 浅色侧边栏：与亮色菜单统一配色，右描边与主区分隔
    background-color: var(--td-bg-color-container);
    border-right: 1px solid var(--td-component-stroke);
    // TDesign 的 .t-layout__sider 默认 flex-shrink:1，会被内容超宽的主区挤到 0 宽，
    // 这里固定不收缩，配合 .basic-layout__main 的 min-width:0 让主区自行收缩滚动
    flex: none;
    overflow: hidden;
    transition: width 0.2s ease;
  }

  &__brand {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 56px;
    padding: 0 16px;
    white-space: nowrap;
    overflow: hidden;
    cursor: pointer;
    // 品牌区下沿用漂木棕半透明细线：全站点缀落点之一（不用大面积色块）
    border-bottom: 1px solid var(--app-accent-soft);
    transition: background-color 0.2s;

    &:hover {
      background-color: var(--td-bg-color-container-hover);
    }
  }

  &__logo {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 28px;
    height: 28px;

    img {
      width: 100%;
      height: 100%;
      object-fit: contain;
      border-radius: 8px;
    }
  }

  &__brand-name {
    font-size: 15px;
    font-weight: 600;
    color: var(--td-text-color-primary);
    letter-spacing: 0.5px;
  }

  &__menu {
    flex: 1;
    overflow-x: hidden;
    overflow-y: auto;
    // 让菜单透明，透出侧边栏底色，避免出现色块断层
    background: transparent;
    border-right: none;
  }

  &__main {
    min-width: 0;
  }

  &__header {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 56px;
    padding: 0 16px 0 8px;
    background-color: var(--td-bg-color-container);
    border-bottom: 1px solid var(--td-component-stroke);
    // 深海蓝调的轻投影，替代中性黑投影
    box-shadow: 0 1px 4px rgb(111 138 184 / 12%);
  }

  &__collapse {
    flex-shrink: 0;
    color: var(--td-text-color-secondary);
  }

  // 主题切换按钮：与折叠按钮同款观感，置于用户区左侧
  &__theme-toggle {
    flex-shrink: 0;
    color: var(--td-text-color-secondary);
  }

  &__breadcrumb {
    flex: 1;
    min-width: 0;

    // 可点击的面包屑条目：手型光标 + 悬浮高亮
    :deep(.is-link) {
      cursor: pointer;
      transition: color 0.2s;

      &:hover {
        color: var(--td-brand-color);
      }
    }
  }

  &__user {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    // 放大点击热区，避免右上角个人中心入口过小难点
    height: 44px;
    padding: 0 14px;
    cursor: pointer;
    background: transparent;
    border: none;
    border-radius: 10px;
    transition: background-color 0.2s;

    &:hover {
      background-color: var(--td-bg-color-container-hover);
    }
  }

  &__avatar {
    // 纯色底 = 压暗版主色（白字对比度恒 ≥4.5）。
    // 曾用「主色 → 强调色」渐变，但强调色多为浅色（落日流金/琥珀金），
    // 白色首字在渐变右半段只剩 1.5~2.0 对比度，看起来发糊。
    background: var(--app-brand-strong, var(--td-brand-color-8));
    color: #fff;
    font-size: 14px;
    font-weight: 600;
    line-height: 1;
    // 内高光让圆盘边界清晰；文字轻阴影进一步提升字形辨识度
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 22%);
    text-shadow: 0 1px 1px rgb(0 0 0 / 20%);
  }

  &__username {
    font-size: 15px;
    color: var(--td-text-color-primary);
  }

  &__caret {
    font-size: 16px;
    color: var(--td-text-color-placeholder);
  }

  &__content {
    padding: 16px;
    overflow: auto;
    background-color: var(--td-bg-color-page);
  }
}

// 路由切换过渡：淡入 + 轻微上移，避免页面生硬闪现
.page-fade-enter-active,
.page-fade-leave-active {
  transition:
    opacity 0.18s ease,
    transform 0.18s ease;
}

.page-fade-enter-from {
  opacity: 0;
  transform: translateY(6px);
}

.page-fade-leave-to {
  opacity: 0;
}
</style>

<!-- 用户下拉面板挂载到 body，需用非 scoped 样式才能命中：整体加宽、选项加大一号 -->
<style lang="less">
.basic-layout__user-dropdown {
  .t-dropdown__menu {
    min-width: 172px;
    padding: 8px;
  }

  .t-dropdown__item {
    // 覆盖组件内联的 max-width/min-width（默认 100px/10px），让选项撑满面板宽度
    width: 100% !important;
    max-width: none !important;
    min-width: 0 !important;
    box-sizing: border-box;
    min-height: 40px;
    padding: 0 16px;
    font-size: 15px;
    line-height: 40px;
  }
}
</style>
