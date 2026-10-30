<template>
  <t-submenu v-if="hasChildren" :value="menuValue">
    <template #icon><t-icon :name="icon" /></template>
    <template #title>
      <!--
        目录既可能是「纯分组」（menu_type=1，如 /content），也可能自带页面
        （menu_type=2，如 /task 下面还挂着甘特图）。TDesign 的 submenu 标题点击
        只会展开/收起，导致这类自带页面的目录**在侧边栏里点不进自己的页面**。
        这里不动展开行为，只在标题上补一次「有页面就跳过去」的导航。
      -->
      <span class="side-menu-item__title-wrap">
        <span class="side-menu-item__title" @click="onTitleClick">{{ item.title }}</span>
        <!-- 目录徽标 = 子树里出现过的桶去重后相加（任务三视图共用同一个数字，只算一次） -->
        <t-badge
          v-if="badge > 0"
          class="side-menu-item__badge"
          :count="badge"
          :max-count="99"
          size="small"
        />
      </span>
    </template>
    <side-menu-item v-for="child in children" :key="child.id" :item="child" @navigate="emit('navigate', $event)" />
  </t-submenu>
  <t-menu-item v-else :value="menuValue">
    <template #icon><t-icon :name="icon" /></template>
    <span class="side-menu-item__title-wrap">
      <span class="side-menu-item__title">{{ item.title }}</span>
      <!-- 叶子菜单徽标：值为 0 时不渲染（不显示红点，而不是显示 0） -->
      <t-badge
        v-if="badge > 0"
        class="side-menu-item__badge"
        :count="badge"
        :max-count="99"
        size="small"
      />
    </span>
  </t-menu-item>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import type { MenuNode } from '@/api/system/auth-api';
import { normalizeMenuPath } from '@/utils/menu';
import { useBadgeStore } from '@/stores/badge';

const props = defineProps<{
  /** 菜单节点（可含多级子节点） */
  item: MenuNode;
}>();

/** 目录自带页面时，把标题点击冒泡成一次导航（由 BasicLayout 负责 router.push） */
const emit = defineEmits<{
  (event: 'navigate', path: string): void;
}>();

const children = computed(() => props.item.children ?? []);
const hasChildren = computed(() => children.value.length > 0);
const icon = computed(() => props.item.icon || (hasChildren.value ? 'folder' : 'app'));

/** 菜单项取值：有路径时用路由路径，否则用 id 占位（目录点击不跳转） */
const menuValue = computed(() => normalizeMenuPath(props.item.path) || `menu:${props.item.id}`);

/**
 * 徽标数字（E16）。
 *
 * - 叶子菜单：`badgeKeyOfPath(path)` 命中的桶（公告中心 / 消息中心 / 审批中心 / 任务三视图）；
 * - 目录：子树桶去重相加（公告与消息 5 = 3 + 2；工作任务 2 而不是 6）。
 * - 未映射的菜单（公告管理 / 消息通知 / 流程定义 / 发起审批 / 项目 / 帮助中心）恒为 0 → 不渲染。
 */
const badgeStore = useBadgeStore();
const badge = computed(() => badgeStore.badgeOfMenu(props.item));

/** 标题点击：只有「真页面菜单」（menu_type=2）且路径非空时才导航，纯分组目录保持原样 */
function onTitleClick(): void {
  if (props.item.menu_type === 2 && props.item.path) {
    emit('navigate', normalizeMenuPath(props.item.path));
  }
}
</script>

<style scoped lang="less">
// 关键：TDesign 的 .t-menu__content / .t-submenu__title 默认是 fit-content 宽度
//（Playwright 实测只有 48px），且 overflow: hidden —— 徽标超出就被裁。
// 必须让它们 flex:1 占满整行，内部的 title-wrap 才有空间做 space-between。
:deep(.t-menu__content),
:deep(.t-submenu__title) {
  flex: 1 1 auto;
  min-width: 0;
  overflow: visible;
}

// 标题 + 徽标的行容器：文字占满剩余宽度（超长省略），徽标贴右且**不被压缩**
.side-menu-item__title-wrap {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  min-width: 0; // 让子项能收缩（flex 的经典坑）
  overflow: visible;
}

.side-menu-item__title {
  // 只让文字区域可点：右侧展开箭头保持「只负责展开」
  display: inline-block;
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.side-menu-item__badge {
  // 关键：不许被 flex 压缩，否则圆点会被挤成半个
  flex: 0 0 auto;
  // 颜色/圆角全部交给 TDesign（`--td-error-color`），不写死色值
  margin-left: 0;
  vertical-align: middle;
}
</style>
