<template>
  <teleport to="body">
    <div v-if="render" class="onboarding" :class="{ 'onboarding--in': shown }" data-onboarding-hint>
      <!-- 入口高亮环 + 序号箭头：只做指示，pointer-events:none，绝不拦截顶栏点击 -->
      <template v-for="mark in marks" :key="mark.key">
        <div class="onboarding__ring" :style="mark.ringStyle" />
        <div class="onboarding__tag" :style="mark.tagStyle">{{ mark.index }}</div>
      </template>

      <section class="onboarding__card" role="dialog" aria-label="新功能引导">
        <button class="onboarding__close" type="button" aria-label="关闭引导" @click="dismiss">×</button>

        <p class="onboarding__eyebrow">新功能引导</p>
        <h3 class="onboarding__title">这 3 个入口，很多人找不到</h3>

        <ul class="onboarding__list">
          <li class="onboarding__item">
            <span class="onboarding__no">①</span>
            <span class="onboarding__text">
              点右上角<b>头像</b> → 「<b>外观设置</b>」，可换 <b>22 套配色</b>（含深色模式）
            </span>
          </li>
          <li class="onboarding__item">
            <span class="onboarding__no">②</span>
            <span class="onboarding__text">
              点顶栏<b>铃铛</b>看消息，红点是未读数，底部「查看全部」进消息中心
            </span>
          </li>
          <li class="onboarding__item">
            <span class="onboarding__no">③</span>
            <span class="onboarding__text">
              任务支持<b>子任务 / 前置依赖 / 甘特图</b>，列表页右上角切换视图，详情页可挂子任务
            </span>
          </li>
        </ul>

        <div class="onboarding__actions">
          <span class="onboarding__foot">只提示一次</span>
          <t-button size="small" theme="primary" @click="dismiss">知道了</t-button>
        </div>
      </section>
    </div>
  </teleport>
</template>

<script setup lang="ts">
/**
 * 首次登录引导（E10「隐藏功能引导」）。
 *
 * 背景：`外观设置` / `消息中心` / `发送通知` 三个菜单是有意 `is_visible=false`（入口在别处），
 * 用户不知道入口在哪 → 功能等于不存在。这里不做菜单改动，只在**首次进入后台时**浮一层
 * 3 步提示，并用高亮环 + 序号箭头指向真实入口（头像 / 铃铛）。
 *
 * 约定：
 * - 只出现一次：点「知道了」或右上角 X 写 `cp-onboarding-done=1`（localStorage，纯前端）；
 * - 不引第三方引导库，纯 CSS 定位 + fixed 坐标（坐标由真实 DOM 的 getBoundingClientRect 算）；
 * - 高亮环与箭头不接收点击（pointer-events: none），引导卡片之外不改布局、不遮顶栏交互。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue';

/** 引导完成标记：存在即不再展示 */
const STORAGE_KEY = 'cp-onboarding-done';

/** 箭头（序号气泡）几何信息 */
interface AnchorMark {
  /** 锚点标识 */
  key: string;
  /** 卡片文案里的序号 */
  index: number;
  /** 高亮环的内联定位 */
  ringStyle: Record<string, string>;
  /** 序号气泡的内联定位 */
  tagStyle: Record<string, string>;
}

/** 锚点定义：选择器 + 环形外扩像素（环要比目标略大一圈才看得出是「框住它」） */
const ANCHORS: ReadonlyArray<{ key: string; index: number; selector: string; pad: number }> = [
  { key: 'avatar', index: 1, selector: '[data-onboarding="avatar"]', pad: 4 },
  // 铃铛在 NoticeBell 里，组件不可改（并行卡在动），用它的稳定类名定位
  { key: 'bell', index: 2, selector: '.notice-bell__trigger', pad: 4 },
];

/** 是否已挂载到 DOM（关闭后延迟卸载，留给淡出动画） */
const render = ref(false);
/** 入场动画开关：挂载后下一帧再置 true，才有 transition */
const shown = ref(false);
/** 当前算出的锚点标记 */
const marks = ref<AnchorMark[]>([]);

let rafId = 0;
let showTimer = 0;
let unmountTimer = 0;

/** 读 localStorage：隐私模式/被禁用时按「已看过」处理，避免每次进来都弹 */
function isDone(): boolean {
  try {
    return window.localStorage.getItem(STORAGE_KEY) === '1';
  } catch {
    return true;
  }
}

/** 按真实 DOM 位置测量锚点；找不到（无权限看不到铃铛）就跳过该步 */
function measure(): void {
  if (!render.value) {
    return;
  }
  const next: AnchorMark[] = [];
  for (const anchor of ANCHORS) {
    const target = document.querySelector<HTMLElement>(anchor.selector);
    if (!target) {
      continue;
    }
    const rect = target.getBoundingClientRect();
    // 折叠态/未渲染时宽高为 0，跳过，避免把环画在 (0,0)
    if (rect.width < 1 || rect.height < 1) {
      continue;
    }
    // 环向外扩 pad，但左右上下都夹在视口内：贴着顶边（顶栏首行）时不会被裁掉半条边
    const left = Math.max(0, Math.round(rect.left - anchor.pad));
    const top = Math.max(0, Math.round(rect.top - anchor.pad));
    const right = Math.min(window.innerWidth, Math.round(rect.right + anchor.pad));
    const bottom = Math.round(rect.bottom + anchor.pad);
    const width = Math.max(1, right - left);
    const height = Math.max(1, bottom - top);
    next.push({
      key: anchor.key,
      index: anchor.index,
      ringStyle: {
        left: `${left}px`,
        top: `${top}px`,
        width: `${width}px`,
        height: `${height}px`,
      },
      tagStyle: {
        // 气泡压在环的正下方居中：箭头尖朝上指回目标
        left: `${left + width / 2 - 11}px`,
        top: `${top + height + 6}px`,
      },
    });
  }
  marks.value = next;
}

/** 首帧可能还没渲染完（菜单/铃铛是异步来的），最多重试 frames 帧直到两个锚点都找到 */
function measureUntilReady(frames = 20): void {
  cancelAnimationFrame(rafId);
  const tick = (): void => {
    measure();
    if (frames > 0 && marks.value.length < ANCHORS.length) {
      frames -= 1;
      rafId = requestAnimationFrame(tick);
    }
  };
  rafId = requestAnimationFrame(tick);
}

/** 视口变化/页面滚动后重新定位（rAF 合帧，避免高频 reflow） */
function onViewportChange(): void {
  cancelAnimationFrame(rafId);
  rafId = requestAnimationFrame(measure);
}

/** Esc 关闭（键盘用户不必去点小 X） */
function onKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    dismiss();
  }
}

/** 关闭：写标记 → 淡出 → 卸载监听 */
function dismiss(): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, '1');
  } catch {
    // 不可写就算了：本次仍然关掉，别把用户困在浮层里
  }
  shown.value = false;
  window.clearTimeout(unmountTimer);
  unmountTimer = window.setTimeout(() => {
    render.value = false;
    teardown();
  }, 240);
}

/** 注销所有监听与计时器（组件卸载时同样调用） */
function teardown(): void {
  cancelAnimationFrame(rafId);
  window.clearTimeout(showTimer);
  window.removeEventListener('resize', onViewportChange);
  window.removeEventListener('scroll', onViewportChange, true);
  window.removeEventListener('keydown', onKeydown);
}

onMounted(() => {
  if (isDone()) {
    return;
  }
  render.value = true;
  measureUntilReady();
  window.addEventListener('resize', onViewportChange, { passive: true });
  // 捕获阶段监听：内容区自己的滚动容器也能收到
  window.addEventListener('scroll', onViewportChange, { passive: true, capture: true });
  window.addEventListener('keydown', onKeydown);
  // 延后 480ms 再淡入：等侧边栏/铃铛的异步数据落位，避免刚进来闪一下再挪位
  showTimer = window.setTimeout(() => {
    shown.value = true;
    measureUntilReady(8);
  }, 480);
});

onBeforeUnmount(teardown);
</script>

<style lang="less" scoped>
// 外层只做定位容器：不吃点击，保证引导浮层之外顶栏/页面照常可点
.onboarding {
  position: fixed;
  inset: 0;
  z-index: 3000;
  pointer-events: none;
}

// 入口高亮环：虚线环 + 呼吸动画，告出「入口就是这个」
.onboarding__ring {
  position: fixed;
  box-sizing: border-box;
  border: 1.5px dashed var(--td-brand-color);
  border-radius: 10px;
  background-color: color-mix(in srgb, var(--td-brand-color) 10%, transparent);
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.24s ease;

  .onboarding--in & {
    opacity: 1;
    animation: onboarding-breathe 1.8s ease-in-out infinite;
  }
}

// 序号气泡（顶部小三角箭头指回被框住的入口）
.onboarding__tag {
  position: fixed;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  font-size: 12px;
  font-weight: 700;
  line-height: 1;
  color: var(--app-on-brand, var(--td-text-color-anti));
  background-color: var(--td-brand-color);
  border-radius: 50%;
  box-shadow: 0 2px 6px color-mix(in srgb, var(--td-brand-color) 45%, transparent);
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.24s ease;
  transform: translateY(-2px);

  // 尖角朝上：三边组成的三角形，颜色跟气泡一致
  &::before {
    content: '';
    position: absolute;
    top: -5px;
    left: 50%;
    margin-left: -5px;
    border-right: 5px solid transparent;
    border-bottom: 6px solid var(--td-brand-color);
    border-left: 5px solid transparent;
  }

  .onboarding--in & {
    opacity: 1;
  }
}

// 引导卡片：固定右上角，落在头栏下方（不压头栏本身）
.onboarding__card {
  position: fixed;
  top: 84px;
  right: 16px;
  box-sizing: border-box;
  width: 316px;
  padding: 14px 16px 12px;
  background-color: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: 12px;
  box-shadow: 0 12px 32px color-mix(in srgb, var(--td-text-color-primary) 16%, transparent);
  opacity: 0;
  pointer-events: auto;
  transition:
    opacity 0.24s ease,
    transform 0.24s ease;
  transform: translateY(-6px);

  .onboarding--in & {
    opacity: 1;
    transform: translateY(0);
  }
}

.onboarding__close {
  position: absolute;
  top: 6px;
  right: 8px;
  width: 24px;
  height: 24px;
  padding: 0;
  font-size: 18px;
  line-height: 1;
  color: var(--td-text-color-placeholder);
  cursor: pointer;
  background: transparent;
  border: none;
  border-radius: 6px;
  transition:
    color 0.2s,
    background-color 0.2s;

  &:hover {
    color: var(--td-text-color-primary);
    background-color: var(--td-bg-color-container-hover);
  }
}

.onboarding__eyebrow {
  margin: 0 0 2px;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.4px;
  color: var(--td-brand-color);
}

.onboarding__title {
  margin: 0 0 10px;
  font-size: 15px;
  font-weight: 600;
  line-height: 1.4;
  color: var(--td-text-color-primary);
}

.onboarding__list {
  margin: 0;
  padding: 0;
  list-style: none;
}

.onboarding__item {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  padding: 5px 0;
  font-size: 13px;
  line-height: 1.65;

  & + & {
    border-top: 1px dashed var(--td-component-stroke);
  }
}

.onboarding__no {
  flex: none;
  font-size: 14px;
  font-weight: 700;
  color: var(--td-brand-color);
}

.onboarding__text {
  min-width: 0;
  color: var(--td-text-color-secondary);

  b {
    font-weight: 600;
    color: var(--td-text-color-primary);
  }
}

.onboarding__actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 10px;
}

.onboarding__foot {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

@keyframes onboarding-breathe {
  0%,
  100% {
    box-shadow: 0 0 0 0 color-mix(in srgb, var(--td-brand-color) 45%, transparent);
  }

  50% {
    box-shadow: 0 0 0 5px color-mix(in srgb, var(--td-brand-color) 0%, transparent);
  }
}

// 尊重系统「减少动态效果」：只保留定位，不呼吸、不位移
@media (prefers-reduced-motion: reduce) {
  .onboarding__ring,
  .onboarding__tag,
  .onboarding__card {
    transition: opacity 0.01s;
    transform: none;
    animation: none;
  }
}
</style>
