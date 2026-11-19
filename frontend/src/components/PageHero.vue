<!--
  通用页面顶部卡（Page Hero）。

  ## 为什么要有它
  原来只有首页有「中午好，超级管理员」那张卡，其他页面进去直接是表格 ——
  用户不知道自己现在在哪个菜单。这张卡把「当前页面名」放在内容区最上面，
  顺带承接品牌视觉（渐变底 + 星座连线特效）。

  ## 视觉
  - 底色用内容区品牌渐变（`--app-gradient-brand`），深浅主题自动跟随
  - 左侧方形徽章承载菜单图标（与「服务器状态」页原来的 hero 卡同一套语言）
  - 星座连线：点与线跟随鼠标（`useConstellationFx`），静止时几乎不动
  - 圆角 12px + 浅投影，和卡片体系一致

  ## 用法
  ```vue
  <PageHero title="用户管理" subtitle="共 128 人" icon="user" />
  <PageHero title="采购订单" icon="cart" />

  需要右侧状态区时用 extra 插槽（如服务器状态：运行徽章 + 采集时间）：

  <PageHero title="服务器状态" :subtitle="hostSub" icon="server">
    <template #extra>
      <span class="ss-pill">运行正常</span>
      <span>采集于 14:46:23</span>
    </template>
  </PageHero>
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
    <!-- 星座连线：点在渐变底上漂，鼠标带线 -->
    <canvas ref="fxCanvas" class="page-hero__fx" aria-hidden="true"></canvas>

    <div class="page-hero__inner">
      <!-- 左：图标徽章（菜单图标）+ 标题 / 副标题 -->
      <div class="page-hero__left">
        <span v-if="icon" class="page-hero__badge">
          <t-icon :name="icon" />
        </span>
        <div class="page-hero__text">
          <h2 class="page-hero__title">{{ title }}</h2>
          <p v-if="subtitle" class="page-hero__subtitle">{{ subtitle }}</p>
        </div>
      </div>

      <!-- 右：页面自定义状态区，不传插槽就整块不渲染 -->
      <div v-if="$slots.extra" class="page-hero__extra">
        <slot name="extra" />
      </div>
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
    /** 左侧徽章图标（TDesign 图标名），如 user / dashboard / server */
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
//   · 深色：纯白点 / 亮金，压在蓝渐变上才看得清
//   · 无光晕：用户要求「鼠标移上去不要发光」，只留连线
const fx = useConstellationFx(fxCanvas, hostEl, { theme: 'dark', glow: false });
</script>

<style scoped lang="less">
.page-hero {
  position: relative;
  display: flex;
  align-items: center;
  margin-bottom: 16px;
  padding: 20px 24px;
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

  &__left {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0; // 长标题不把右侧状态区挤出去
  }

  // 方形徽章：半透明白底 + 白图标，与「服务器状态」页原来的 hero 卡一致
  &__badge {
    display: inline-flex;
    flex: none;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border-radius: 11px;
    background: rgb(255 255 255 / 18%);
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 26%);
    color: var(--app-on-gradient, #fff);
    font-size: 22px;
  }

  &__text {
    min-width: 0;
  }

  &__title {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--app-on-gradient, #ffffff);
    // 白字在渐变深色段上，加一点深色微阴影提升边缘清晰度
    text-shadow: 0 1px 3px rgb(0 0 0 / 26%);
    // 标题最多两行，超出省略（菜单名一般很短，这里只是兜底）
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  &__subtitle {
    margin: 5px 0 0;
    font-size: 13px;
    line-height: 1.6;
    color: var(--app-on-gradient, #ffffff);
    opacity: 0.94;
    text-shadow: 0 1px 2px rgb(0 0 0 / 22%);
  }

  // 右侧状态区：竖排（状态徽章在上、时间在下），和原 ss-hero__right 一致
  &__extra {
    display: flex;
    flex: none;
    flex-direction: column;
    align-items: flex-end;
    gap: 6px;
    margin-left: auto;
    color: var(--app-on-gradient, #ffffff);
  }
}

// 小屏：降低内边距与字号，徽章略微收小；右侧状态区换到下一行
@media (max-width: 768px) {
  .page-hero {
    padding: 16px;

    &__badge {
      width: 38px;
      height: 38px;
      font-size: 19px;
    }

    &__title {
      font-size: 17px;
    }

    &__inner {
      flex-wrap: wrap;
    }

    &__extra {
      align-items: flex-start;
      margin-left: 0;
    }
  }
}
</style>
