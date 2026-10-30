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
      <span class="side-menu-item__title" @click="onTitleClick">{{ item.title }}</span>
    </template>
    <side-menu-item v-for="child in children" :key="child.id" :item="child" @navigate="emit('navigate', $event)" />
  </t-submenu>
  <t-menu-item v-else :value="menuValue">
    <template #icon><t-icon :name="icon" /></template>
    {{ item.title }}
  </t-menu-item>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import type { MenuNode } from '@/api/system/auth-api';
import { normalizeMenuPath } from '@/utils/menu';

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

/** 标题点击：只有「真页面菜单」（menu_type=2）且路径非空时才导航，纯分组目录保持原样 */
function onTitleClick(): void {
  if (props.item.menu_type === 2 && props.item.path) {
    emit('navigate', normalizeMenuPath(props.item.path));
  }
}
</script>

<style scoped lang="less">
.side-menu-item__title {
  // 只让文字区域可点：右侧展开箭头保持「只负责展开」
  display: inline-block;
}
</style>
