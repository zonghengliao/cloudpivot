<!--
  通用页面顶部卡（Page Hero）。

  ## 为什么要有它
  原来只有首页有「中午好，超级管理员」那张卡，其他页面进去直接是表格 ——
  用户不知道自己现在在哪个菜单。这张卡把「当前页面名」放在内容区最上面，
  顺带承接品牌视觉（渐变底 + 星座连线特效）。

  ## 视觉
  - 底色复用登录页那套雾蓝渐变（`--app-login-brand`），深浅主题都自动跟随
  - 星座连线：点与线跟随鼠标（`useConstellationFx`），静止时几乎不动
  - 圆角 12px + 极浅投影，和卡片体系一致

  ## 用法
  ```vue
  <PageHero title="用户管理" subtitle="共 128 人" icon="user" />
  <PageHero title="采购订单" />                       <!-- 无副标题 -->
  <PageHero title="首页" :height="132" />             <!-- 更厚 -->
  ```

  ## 无障碍
  - 特效层 `pointer-events: none`，不挡点击
  - `prefers-reduced-motion: reduce` 时 composable 不启动，只留渐变底
  - canvas 标 `aria-hidden`，标题是真实文本节点
-->
<template>
  <section
    ref="hostEl"
    class="page-hero"
    :style="{ minHeight: `${height}px` }"
    @mousemove="fx.onMouseMove"
    @mouseleave="fx.onMouseLeave"
    @touchmove="fx.onTouchMove"
  >
    <!-- 星座连线：点在渐变底上漂，鼠标带金线 -->
    <canvas ref="fxCanvas" class="page-hero__fx" aria-hidden="true"></canvas>

    <div class="page-hero__inner">
      <div class="page-hero__text">
        <h2 class="page-hero__title">{{ title }}</h2>
        <p v-if="subtitle" class="page-hero__subtitle">{{ subtitle }}</p>
      </div>
      <t-icon v-if="icon" class="page-hero__icon" :name="icon" />
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref } from 'vue';
import { useConstellationFx } from '@/composables/use-constellation-fx';

withDefaults(
  defineProps<{
    /** 主标题：通常是当前菜单名，或首页的问候语 */
    title: string;
    /** 副标题：一句补充说明，可为空 */
    subtitle?: string;
    /** 右侧装饰图标（TDesign 图标名），如 user / dashboard / cart */
    icon?: string;
    /** 最小高度（px）。首页可以给 132，普通页 104 就够 */
    height?: number;
  }>(),
  {
    subtitle: '',
    icon: '',
    height: 104,
  },
);

const hostEl = ref<HTMLElement | null>(null);
const fxCanvas = ref<HTMLCanvasElement | null>(null);
// 深色主题 + 关掉鼠标光晕：
//   · 深色：纯白点 / 亮金，压在蓝渐变上才看得清（原来的近白只有 .72 不透明度，糊了）
//   · 无光晕：用户要求「鼠标移上去不要发光」，只留连线
const fx = useConstellationFx(fxCanvas, hostEl, { theme: 'dark', glow: false });
</script>

<style scoped lang="less">
.page-hero {
  position: relative;
  display: flex;
  align-items: center;
  margin-bottom: 16px;
  padding: 22px 26px;
  overflow: hidden;
  border-radius: 12px;
  // 与首页问候卡同一套品牌渐变（深蓝 → 雾蓝 → 浅蓝），白字压得住
  background: var(--app-gradient-brand);
  box-shadow: 0 6px 20px rgb(111 138 184 / 24%);

  &__fx {
    position: absolute;
    inset: 0;
    z-index: 1;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  &__inner {
    position: relative;
    z-index: 3; // 压在 canvas（1）之上，文字永远可读
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
  }

  &__text {
    min-width: 0; // 长标题不把图标挤出去
  }

  &__title {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--app-on-gradient, #ffffff);
    // 白字在渐变深色段上，加一点深色微阴影提升边缘清晰度
    text-shadow: 0 1px 3px rgb(0 0 0 / 26%);
    // 标题换行时最多两行，超出省略（菜单名一般很短，这里只是兜底）
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  &__subtitle {
    margin: 6px 0 0;
    font-size: 13px;
    line-height: 1.6;
    color: var(--app-on-gradient, #ffffff);
    opacity: 0.94;
    text-shadow: 0 1px 2px rgb(0 0 0 / 22%);
  }

  &__icon {
    flex: none;
    font-size: 72px;
    // 图标在横幅右侧浅色段，用深色油墨才可见
    color: var(--app-on-gradient-soft, #1a2230);
    opacity: 0.16;
    pointer-events: none;
  }
}

// 小屏：降低内边距与标题字号，图标隐藏（宽度不够时纯属占地方）
@media (max-width: 768px) {
  .page-hero {
    padding: 18px 18px;

    &__title {
      font-size: 17px;
    }

    &__icon {
      display: none;
    }
  }
}
</style>
