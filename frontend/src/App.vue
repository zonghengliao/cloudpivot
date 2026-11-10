<template>
  <router-view />
  <!--
    空闲自动退出守卫：全站唯一实例。
    放在根组件而不是布局里 —— 首登强制改密页（/change-password）不在 BasicLayout 下，
    挂在布局上会让「挂在改密页不动」绕过自动退出。
  -->
  <IdleLogoutGuard />
</template>

<script setup lang="ts">
// 根组件承载路由出口 + 全站级守卫；布局由各路由的布局组件决定
import IdleLogoutGuard from '@/components/IdleLogoutGuard.vue';
</script>
